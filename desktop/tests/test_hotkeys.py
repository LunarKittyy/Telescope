import os
import shutil
import subprocess
import threading
import time

import pytest
from PyQt6.QtCore import QCoreApplication

from telescope.platform.hotkeys import Hotkey
from telescope.platform.hotkeys_portal import PortalHotkeys, to_trigger
from telescope.platform.hotkeys_windows import MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, \
    WindowsHotkeys, to_windows


def _wait_for(cond, timeout=5.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        QCoreApplication.processEvents()
        if cond():
            return True
        time.sleep(0.01)
    return cond()


# ── Windows ──────────────────────────────────────────────────────────────────

@pytest.mark.parametrize("keys, want", [
    ("Ctrl+Alt+M", (MOD_CONTROL | MOD_ALT, ord("M"))),
    ("Meta+Shift+5", (MOD_WIN | MOD_SHIFT, ord("5"))),
    ("F13", (0, 0x7C)),
    ("Ctrl+Up", (MOD_CONTROL, 0x26)),
    ("Media Play", (0, 0xB3)),
])
def test_to_windows(qapp, keys, want):
    assert to_windows(keys) == want


def test_to_windows_asks_the_layout_for_punctuation(qapp):
    # VkKeyScanW: low byte the key, 0x100 when the layout needs Shift for it
    assert to_windows("Ctrl+!", lambda ch: 0x131) == (MOD_CONTROL | MOD_SHIFT, 0x31)
    assert to_windows("Ctrl+/", lambda ch: -1) is None
    assert to_windows("Ctrl+/") is None


class _User32:
    def __init__(self, taken=()):
        self.taken = set(taken)
        self.registered = {}
        self.down = set()

    def RegisterHotKey(self, hwnd, number, flags, vk):
        if (flags & ~MOD_NOREPEAT, vk) in self.taken:
            return 0
        assert flags & MOD_NOREPEAT
        self.registered[number] = (flags, vk)
        return 1

    def UnregisterHotKey(self, hwnd, number):
        self.registered.pop(number, None)
        return 1

    def GetAsyncKeyState(self, vk):
        return -32768 if vk in self.down else 0

    def VkKeyScanW(self, ch):
        return -1


def test_windows_backend_registers_and_reports(qapp):
    user32 = _User32(taken={(MOD_CONTROL, ord("T"))})
    backend = WindowsHotkeys(user32=user32, install_filter=False)
    changed = []
    backend.changed.connect(lambda: changed.append(True))
    backend.apply([Hotkey("a", "Ctrl+Alt+M", "Mute"), Hotkey("b", "Ctrl+T", "Torch"), Hotkey("c", "Ctrl+/", "x")])
    assert user32.registered == {1: (MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, ord("M"))}
    assert backend.status("a").working
    assert backend.status("b").text == "Another app already uses this key"
    assert not backend.status("c").working
    assert changed
    backend.apply([])
    assert user32.registered == {}
    backend.stop()


def test_windows_backend_presses_and_watches_for_the_release(qapp):
    user32 = _User32()
    backend = WindowsHotkeys(user32=user32, install_filter=False)
    backend.apply([Hotkey("a", "Ctrl+Space", "Talk")])
    events = []
    backend.pressed.connect(lambda h: events.append(("down", h)))
    backend.released.connect(lambda h: events.append(("up", h)))
    user32.down.add(0x20)
    backend._on_hotkey(1)
    backend._on_hotkey(1)  # still down: not pressed twice
    backend._on_hotkey(99)  # not ours
    assert events == [("down", "a")]
    QCoreApplication.processEvents()
    assert events == [("down", "a")]
    user32.down.clear()
    assert _wait_for(lambda: ("up", "a") in events)
    assert events == [("down", "a"), ("up", "a")]
    backend.stop()


# ── Portal ───────────────────────────────────────────────────────────────────

@pytest.mark.parametrize("keys, want", [
    ("Ctrl+Alt+M", "CTRL+ALT+m"),
    ("Meta+F5", "LOGO+F5"),
    ("Shift+Page Up", "SHIFT+Page_Up"),
    ("Ctrl+1", "CTRL+1"),
    ("Ctrl+=", "CTRL+equal"),
    ("Volume Mute", "XF86AudioMute"),
    ("Ctrl+§", ""),
])
def test_to_trigger(qapp, keys, want):
    assert to_trigger(keys) == want


class _FakePortal:
    """Just enough of xdg-desktop-portal's GlobalShortcuts, on a private bus, to drive PortalHotkeys for real."""

    def __init__(self, address, version=2, accept=True, triggers=None):
        from jeepney.io.blocking import open_dbus_connection
        self.conn = open_dbus_connection(bus=address)
        self.conn.send_and_get_reply(self._bus_call("RequestName", "su", ("org.freedesktop.portal.Desktop", 0)))
        self.version, self.accept, self.triggers = version, accept, triggers or {}
        self.calls = []
        self.session = None
        self._stop = False
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    @staticmethod
    def _bus_call(method, sig, body):
        from jeepney import DBusAddress, new_method_call
        bus = DBusAddress("/org/freedesktop/DBus", bus_name="org.freedesktop.DBus", interface="org.freedesktop.DBus")
        return new_method_call(bus, method, sig, body)

    def _signal(self, path, interface, member, sig, body):
        from jeepney import DBusAddress, new_signal
        self.conn.send(new_signal(DBusAddress(path, interface=interface), member, sig, body))

    def _serve(self):
        from jeepney import HeaderFields, MessageType, new_error, new_method_return
        while not self._stop:
            try:
                msg = self.conn.receive(timeout=0.2)
            except TimeoutError:
                continue
            except Exception:
                return
            if msg.header.message_type != MessageType.method_call:
                continue
            f = msg.header.fields
            iface, member, path = f.get(HeaderFields.interface), f.get(HeaderFields.member), f.get(HeaderFields.path)
            sender = f.get(HeaderFields.sender).lstrip(":").replace(".", "_")
            self.calls.append((member, msg.body))
            if member == "Register":
                self.conn.send(new_method_return(msg))
            elif member == "Get":
                if self.version is None:
                    self.conn.send(new_error(msg, "org.freedesktop.DBus.Error.UnknownInterface"))
                else:
                    self.conn.send(new_method_return(msg, "v", (("u", self.version),)))
            elif member == "CreateSession":
                opts = msg.body[0]
                handle = f"/org/freedesktop/portal/desktop/request/{sender}/{opts['handle_token'][1]}"
                self.session = f"/org/freedesktop/portal/desktop/session/{sender}/{opts['session_handle_token'][1]}"
                self.conn.send(new_method_return(msg, "o", (handle,)))
                self._signal(handle, "org.freedesktop.portal.Request", "Response", "ua{sv}",
                             (0, {"session_handle": ("s", self.session)}))
            elif member == "BindShortcuts":
                session, shortcuts, _parent, opts = msg.body
                handle = f"/org/freedesktop/portal/desktop/request/{sender}/{opts['handle_token'][1]}"
                self.conn.send(new_method_return(msg, "o", (handle,)))
                if not self.accept:
                    self._signal(handle, "org.freedesktop.portal.Request", "Response", "ua{sv}", (1, {}))
                    continue
                bound = [(sid, {"description": props["description"],
                                "trigger_description": ("s", self.triggers.get(sid, props.get(
                                    "preferred_trigger", ("s", ""))[1]))}) for sid, props in shortcuts]
                self._signal(handle, "org.freedesktop.portal.Request", "Response", "ua{sv}",
                             (0, {"shortcuts": ("a(sa{sv})", bound)}))
            elif member in ("ConfigureShortcuts", "Close"):
                self.conn.send(new_method_return(msg))
            else:
                self.conn.send(new_error(msg, "org.freedesktop.DBus.Error.UnknownMethod"))

    def emit(self, member, shortcut_id):
        self._signal("/org/freedesktop/portal/desktop", "org.freedesktop.portal.GlobalShortcuts", member,
                     "osta{sv}", (self.session, shortcut_id, 0, {}))

    def change(self, shortcut_id, trigger):
        self._signal("/org/freedesktop/portal/desktop", "org.freedesktop.portal.GlobalShortcuts",
                     "ShortcutsChanged", "oa(sa{sv})",
                     (self.session, [(shortcut_id, {"description": ("s", "x"),
                                                    "trigger_description": ("s", trigger)})]))

    def close(self):
        self._stop = True
        self.thread.join(timeout=2)
        self.conn.close()


@pytest.fixture
def bus():
    pytest.importorskip("jeepney")
    if shutil.which("dbus-daemon") is None:
        pytest.skip("needs dbus-daemon")
    proc = subprocess.Popen(["dbus-daemon", "--session", "--nofork", "--print-address"], stdout=subprocess.PIPE,
                            text=True, env={**os.environ})
    address = proc.stdout.readline().strip()
    yield address
    proc.terminate()
    proc.wait(timeout=5)


@pytest.fixture
def portal_backend(bus, qapp):
    made = []

    def build(**kw):
        from jeepney.io.threading import open_dbus_router
        portal = _FakePortal(bus, **kw)
        prepared = []
        backend = PortalHotkeys("telescope", open_router=lambda: open_dbus_router(bus=bus),
                                prepare=lambda: prepared.append(True))
        made.append((portal, backend))
        return portal, backend, prepared

    yield build
    for portal, backend in made:
        backend.stop()
        portal.close()


def test_portal_binds_and_relays_presses(portal_backend):
    portal, backend, prepared = portal_backend(triggers={"b2": "Meta+T"})
    assert not prepared  # nothing happens before there's something to bind
    events = []
    backend.pressed.connect(lambda h: events.append(("down", h)))
    backend.released.connect(lambda h: events.append(("up", h)))
    backend.apply([Hotkey("b1", "Ctrl+Alt+M", "Mute: switch on or off"), Hotkey("b2", "Ctrl+T", "Torch")])
    assert _wait_for(lambda: backend.status("b1") is not None and backend.status("b1").working)
    assert prepared == [True]
    assert [c[0] for c in portal.calls][:4] == ["Register", "Get", "CreateSession", "BindShortcuts"]
    assert portal.calls[0] == ("Register", ("telescope", {}))  # first of all, as the portal requires
    bind = next(body for name, body in portal.calls if name == "BindShortcuts")
    assert bind[1][0] == ("b1", {"description": ("s", "Mute: switch on or off"),
                                 "preferred_trigger": ("s", "CTRL+ALT+m")})
    assert backend.status("b1").text == "Everywhere: CTRL+ALT+m"
    assert backend.status("b2").text == "Everywhere: Meta+T"  # the desktop picked another key
    assert backend.can_configure()

    portal.emit("Activated", "b1")
    portal.emit("Deactivated", "b1")
    assert _wait_for(lambda: events == [("down", "b1"), ("up", "b1")])

    portal.change("b2", "")
    assert _wait_for(lambda: not backend.status("b2").working)
    assert "No key set" in backend.status("b2").text

    assert backend.configure()
    assert _wait_for(lambda: any(c[0] == "ConfigureShortcuts" for c in portal.calls))

    # New bindings mean a new session: the old one is closed first
    old = portal.session
    backend.apply([Hotkey("b3", "Ctrl+F1", "x")])
    assert _wait_for(lambda: backend.status("b3") is not None and backend.status("b3").working)
    assert portal.session != old
    assert any(c[0] == "Close" for c in portal.calls)
    portal.emit("Activated", "b3")
    assert _wait_for(lambda: ("down", "b3") in events)


def test_portal_without_global_shortcuts_says_so(portal_backend):
    portal, backend, _ = portal_backend(version=None)
    backend.apply([Hotkey("b1", "Ctrl+M", "Mute")])
    assert _wait_for(lambda: backend.status("b1") is not None)
    assert not backend.status("b1").working
    assert "doesn't offer global shortcuts" in backend.note()
    assert not backend.can_configure()


def test_portal_shortcuts_the_user_turned_down(portal_backend):
    portal, backend, _ = portal_backend(accept=False)
    backend.apply([Hotkey("b1", "Ctrl+M", "Mute")])
    assert _wait_for(lambda: backend.status("b1") is not None and "Not confirmed" in backend.status("b1").text)
