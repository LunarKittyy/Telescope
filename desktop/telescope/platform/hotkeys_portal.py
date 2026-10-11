"""Linux global shortcuts through the GlobalShortcuts desktop portal (Plasma 5.27+, GNOME 48+, Hyprland).

Talks D-Bus with jeepney on threads of its own; nothing here blocks the GUI. The flow: register the app id
(an app outside Flatpak has to, before any other portal call, or the portal can't tell whose shortcuts they
are), create a session, bind the shortcuts to it. The desktop then asks the user to confirm them, and may bind
other keys than the ones suggested: the answer says which, and ShortcutsChanged says when that changes later. A
session can only bind once, so new bindings get a new session.
"""

import logging
import os
import queue
import secrets
import threading
from pathlib import Path
from typing import Optional

from PyQt6.QtCore import Qt

from telescope.platform.hotkeys import HotkeyBackend, HotkeyStatus
from telescope.shortcuts import split_keys

logger = logging.getLogger(__name__)

BUS_NAME = "org.freedesktop.portal.Desktop"
PORTAL_PATH = "/org/freedesktop/portal/desktop"
SHORTCUTS = "org.freedesktop.portal.GlobalShortcuts"
REGISTRY = "org.freedesktop.host.portal.Registry"
REQUEST = "org.freedesktop.portal.Request"
SESSION = "org.freedesktop.portal.Session"
_CALL_TIMEOUT = 10
_WAIT_STEP = 0.5

# The shortcuts spec writes keys as xkb keysym names
_KEYSYMS = {
    Qt.Key.Key_Space: "space", Qt.Key.Key_Return: "Return", Qt.Key.Key_Enter: "KP_Enter",
    Qt.Key.Key_Escape: "Escape", Qt.Key.Key_Tab: "Tab", Qt.Key.Key_Backspace: "BackSpace",
    Qt.Key.Key_Insert: "Insert", Qt.Key.Key_Delete: "Delete", Qt.Key.Key_Home: "Home", Qt.Key.Key_End: "End",
    Qt.Key.Key_PageUp: "Page_Up", Qt.Key.Key_PageDown: "Page_Down", Qt.Key.Key_Left: "Left",
    Qt.Key.Key_Up: "Up", Qt.Key.Key_Right: "Right", Qt.Key.Key_Down: "Down", Qt.Key.Key_Pause: "Pause",
    Qt.Key.Key_Print: "Print", Qt.Key.Key_ScrollLock: "Scroll_Lock",
    Qt.Key.Key_VolumeMute: "XF86AudioMute", Qt.Key.Key_VolumeDown: "XF86AudioLowerVolume",
    Qt.Key.Key_VolumeUp: "XF86AudioRaiseVolume", Qt.Key.Key_MediaPlay: "XF86AudioPlay",
    Qt.Key.Key_MediaTogglePlayPause: "XF86AudioPlay", Qt.Key.Key_MediaStop: "XF86AudioStop",
    Qt.Key.Key_MediaNext: "XF86AudioNext", Qt.Key.Key_MediaPrevious: "XF86AudioPrev",
    Qt.Key.Key_Plus: "plus", Qt.Key.Key_Minus: "minus", Qt.Key.Key_Comma: "comma", Qt.Key.Key_Period: "period",
    Qt.Key.Key_Slash: "slash", Qt.Key.Key_Backslash: "backslash", Qt.Key.Key_Semicolon: "semicolon",
    Qt.Key.Key_Apostrophe: "apostrophe", Qt.Key.Key_BracketLeft: "bracketleft",
    Qt.Key.Key_BracketRight: "bracketright", Qt.Key.Key_QuoteLeft: "grave", Qt.Key.Key_Equal: "equal",
    Qt.Key.Key_Asterisk: "asterisk", Qt.Key.Key_NumberSign: "numbersign",
}
_XDG_MODS = {"Ctrl": "CTRL", "Alt": "ALT", "Shift": "SHIFT", "Meta": "LOGO"}


def to_trigger(keys: str) -> str:
    """keys as the shortcuts spec writes a trigger ("CTRL+ALT+m"), or "" for a key it has no name for."""
    mods, name, qkey = split_keys(keys)
    if Qt.Key.Key_A <= qkey <= Qt.Key.Key_Z:
        key = name.lower()
    elif Qt.Key.Key_0 <= qkey <= Qt.Key.Key_9 or Qt.Key.Key_F1 <= qkey <= Qt.Key.Key_F35:
        key = name
    else:
        key = _KEYSYMS.get(qkey, "")
    if not key:
        return ""
    return "+".join([_XDG_MODS[m] for m in mods] + [key])


def portal_possible() -> bool:
    """Whether trying the portal makes sense: jeepney is there and so is a session bus."""
    try:
        import jeepney  # noqa: F401
    except ImportError:
        logger.info("jeepney isn't installed, so there are no global shortcuts")
        return False
    if os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
        return True
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    return bool(runtime) and Path(runtime, "bus").exists()


class _Stop(Exception):
    pass


class PortalHotkeys(HotkeyBackend):
    system_picks_keys = True

    def __init__(self, app_id: str, open_router=None, prepare=None):
        super().__init__()
        self._app_id = app_id
        self._open_router = open_router
        self._prepare = prepare  # run on the thread before registering: makes sure the menu entry is there
        self._cmds: queue.Queue = queue.Queue()
        self._lock = threading.Lock()
        self._statuses: dict = {}
        self._state = "starting"  # "ready", "asking" (the desktop's dialog is up), "unavailable"
        self._why = ""
        self._version = 0
        self._session: Optional[str] = None
        self._stopping = threading.Event()
        self._thread = threading.Thread(target=self._run, name="portal-shortcuts", daemon=True)
        self._thread.start()

    # ── GUI side ─────────────────────────────────────────────────────────────

    def apply(self, hotkeys: list) -> None:
        self._cmds.put(("bind", list(hotkeys)))

    def status(self, hotkey_id: str) -> Optional[HotkeyStatus]:
        with self._lock:
            if self._state == "unavailable":
                return HotkeyStatus(False, "Not available on this desktop")
            if self._state == "asking":
                return HotkeyStatus(False, "Waiting for you to confirm it")
            return self._statuses.get(hotkey_id)

    def note(self) -> str:
        with self._lock:
            state, why = self._state, self._why
        if state == "unavailable":
            return ("This desktop doesn't offer global shortcuts to apps" + (f" ({why})" if why else "") + ". "
                    "Bind a Copy command line in its own keyboard settings instead.")
        return ("Your desktop asks you to confirm global shortcuts, and you can change their keys in its own "
                "settings. The key it settled on shows next to each one.")

    def can_configure(self) -> bool:
        with self._lock:
            return self._version >= 2 and self._session is not None and self._state == "ready"

    def configure(self) -> bool:
        if not self.can_configure():
            return False
        self._cmds.put(("configure",))
        return True

    def stop(self) -> None:
        self._stopping.set()
        self._cmds.put(("stop",))
        self._thread.join(timeout=2)

    # ── Portal thread ────────────────────────────────────────────────────────

    def _set_state(self, state: str, why: str = ""):
        with self._lock:
            self._state, self._why = state, why
        self._changed_from_thread.emit()

    def _run(self):
        # Nothing goes to the portal until there's a global shortcut to bind: no session, no dialog, no menu entry
        while True:
            cmd = self._cmds.get()
            if cmd[0] == "stop":
                return
            if cmd[0] == "bind" and cmd[1]:
                self._cmds.put(cmd)  # handled again, with any newer one, once connected
                break
        try:
            if self._prepare is not None:
                self._prepare()
            if self._open_router is None:
                from jeepney.io.threading import open_dbus_router
                self._open_router = open_dbus_router
            with self._open_router() as router:
                self._serve(router)
        except _Stop:
            pass
        except Exception as exc:
            logger.exception("Global shortcuts portal failed")
            self._set_state("unavailable", type(exc).__name__)

    def _serve(self, router):
        from jeepney import DBusAddress, HeaderFields, MatchRule, message_bus, new_method_call
        from jeepney.io.threading import Proxy
        from jeepney.wrappers import DBusErrorResponse, unwrap_msg

        self._router, self._bus = router, Proxy(message_bus, router, timeout=_CALL_TIMEOUT)
        self._unwrap, self._call = unwrap_msg, new_method_call
        self._MatchRule, self._Error, self._member = MatchRule, DBusErrorResponse, HeaderFields.member
        self._portal = DBusAddress(PORTAL_PATH, bus_name=BUS_NAME, interface=SHORTCUTS)

        try:  # first of all, or the portal can't tell which app's shortcuts these are
            self._send(DBusAddress(PORTAL_PATH, bus_name=BUS_NAME, interface=REGISTRY), "Register", "sa{sv}",
                       (self._app_id, {}))
        except DBusErrorResponse as exc:  # an older portal, or no menu entry under this id
            logger.info("Couldn't register with the desktop portal as %s: %s", self._app_id, exc)
        props = DBusAddress(PORTAL_PATH, bus_name=BUS_NAME, interface="org.freedesktop.DBus.Properties")
        try:
            (version,) = self._send(props, "Get", "ss", (SHORTCUTS, "version"))
            self._version = int(version[1])
        except DBusErrorResponse as exc:
            logger.info("The desktop portal has no global shortcuts: %s", exc)
            self._set_state("unavailable")
            return

        # No sender: signals carry the portal's unique name, which a rule matched here by jeepney wouldn't equal
        signals = MatchRule(type="signal", interface=SHORTCUTS, path=PORTAL_PATH)
        self._bus.AddMatch(signals)
        with router.filter(signals, queue=queue.Queue()) as incoming:
            listener = threading.Thread(target=self._listen, args=(incoming,), name="portal-signals", daemon=True)
            listener.start()
            self._set_state("ready")
            try:
                self._commands()
            finally:
                self._stopping.set()
                self._close_session()

    def _send(self, address, method, signature, body):
        reply = self._router.send_and_get_reply(self._call(address, method, signature, body), timeout=_CALL_TIMEOUT)
        return self._unwrap(reply)

    def _commands(self):
        while True:
            cmds = [self._cmds.get()]
            while not self._cmds.empty():
                cmds.append(self._cmds.get_nowait())
            if any(c[0] == "stop" for c in cmds):
                return
            binds = [c[1] for c in cmds if c[0] == "bind"]
            if binds:
                self._bind(binds[-1])  # only the latest bindings matter
            if any(c[0] == "configure" for c in cmds) and self._session:
                try:
                    self._send(self._portal, "ConfigureShortcuts", "osa{sv}", (self._session, "", {}))
                except self._Error as exc:
                    logger.info("Couldn't open the shortcut settings: %s", exc)

    def _bind(self, hotkeys: list):
        self._close_session()
        if not hotkeys:
            with self._lock:
                self._statuses = {}
            self._set_state("ready")
            return
        code, results = self._request("CreateSession", "a{sv}", lambda token: (
            {"handle_token": ("s", token), "session_handle_token": ("s", "telescope" + secrets.token_hex(4))},))
        if code != 0 or "session_handle" not in results:
            logger.info("The desktop portal didn't make a shortcuts session (%s)", code)
            self._set_state("unavailable", "no session")
            return
        session = str(results["session_handle"][1])
        with self._lock:
            self._session = session
            self._state = "asking"
        self._changed_from_thread.emit()
        shortcuts = []
        for hk in hotkeys:
            props = {"description": ("s", hk.description)}
            trigger = to_trigger(hk.keys)
            if trigger:
                props["preferred_trigger"] = ("s", trigger)
            shortcuts.append((hk.id, props))
        code, results = self._request("BindShortcuts", "oa(sa{sv})sa{sv}", lambda token: (
            session, shortcuts, "", {"handle_token": ("s", token)}))
        bound = dict(results.get("shortcuts", ("", []))[1]) if code == 0 else {}
        with self._lock:
            self._statuses = {hk.id: self._status_of(bound.get(hk.id)) for hk in hotkeys}
            self._state = "ready"
        self._changed_from_thread.emit()

    @staticmethod
    def _status_of(props: Optional[dict]) -> HotkeyStatus:
        if props is None:
            return HotkeyStatus(False, "Not confirmed in the desktop's dialog")
        trigger = str(props.get("trigger_description", ("s", ""))[1])
        if not trigger:
            return HotkeyStatus(False, "No key set in the desktop's shortcut settings")
        return HotkeyStatus(True, f"Everywhere: {trigger}")

    def _request(self, method: str, signature: str, body) -> tuple:
        """Call a portal method that answers through a Request object's Response signal; (code, results)."""
        token = "telescope" + secrets.token_hex(6)
        sender = self._router.unique_name.lstrip(":").replace(".", "_")
        handle = f"{PORTAL_PATH}/request/{sender}/{token}"
        rule = self._MatchRule(type="signal", interface=REQUEST, member="Response", path=handle)
        self._bus.AddMatch(rule)
        try:
            with self._router.filter(rule, queue=queue.Queue()) as responses:
                try:
                    self._send(self._portal, method, signature, body(token))
                except self._Error as exc:
                    logger.info("Desktop portal %s failed: %s", method, exc)
                    return 2, {}
                while True:  # a dialog waits on the user, for as long as they take
                    try:
                        code, results = responses.get(timeout=_WAIT_STEP).body
                        return int(code), dict(results)
                    except queue.Empty:
                        if self._stopping.is_set():
                            raise _Stop()
        finally:
            try:
                self._bus.RemoveMatch(rule)
            except Exception:
                pass

    def _close_session(self):
        with self._lock:
            session, self._session = self._session, None
        if session:
            from jeepney import DBusAddress
            try:
                self._send(DBusAddress(session, bus_name=BUS_NAME, interface=SESSION), "Close", None, ())
            except Exception:
                pass  # already gone

    def _listen(self, incoming: queue.Queue):
        while not self._stopping.is_set():
            try:
                msg = incoming.get(timeout=_WAIT_STEP)
            except queue.Empty:
                continue
            member = msg.header.fields.get(self._member)
            body = msg.body
            with self._lock:
                session = self._session
            if not body or body[0] != session:
                continue
            if member == "Activated":
                self._pressed_from_thread.emit(str(body[1]))
            elif member == "Deactivated":
                self._released_from_thread.emit(str(body[1]))
            elif member == "ShortcutsChanged":
                changed = dict(body[1])
                with self._lock:
                    for hid, props in changed.items():
                        self._statuses[hid] = self._status_of(props)
                self._changed_from_thread.emit()
