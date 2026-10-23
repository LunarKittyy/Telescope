"""Keyboard shortcuts: keys bound to any control's action, inside Telescope or everywhere.

Bindings are global settings (one list for every phone). Each runs an action plugins offer (telescope.shortcuts)
with its own settings, so "Exposure +1" and "Exposure -2" are two bindings of one action. A binding marked
global is also claimed from the system (telescope.platform.hotkeys); one the system wouldn't give us still works
inside Telescope. Keys pressed inside Telescope go through an application-wide event filter rather than
QShortcut, so "while held" bindings see the key come back up.

`telescope --action ID name=value` runs an action in the running copy too (handle_remote), for desktops with no
global shortcuts portal: bind that command in the desktop's own keyboard settings.
"""

import logging
import math
import shlex
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional
from weakref import WeakSet

from PyQt6.QtCore import QEvent, QObject, QPoint, Qt, QTimer
from PyQt6.QtGui import QAction, QGuiApplication, QKeySequence
from PyQt6.QtWidgets import (
    QAbstractSpinBox, QApplication, QComboBox, QDialog, QFrame, QHBoxLayout, QKeySequenceEdit, QLabel,
    QLineEdit, QMenu, QPlainTextEdit, QPushButton, QTextEdit, QVBoxLayout, QWidget,
)

from telescope import dev_profile, fuzzy
from telescope.platform import IS_LINUX, IS_WINDOWS, autostart
from telescope.platform.hotkeys import Hotkey, create_backend
from telescope.plugin import SettingsSection, TelescopePlugin
from telescope.widgets.settings_search import keywords
from telescope.shortcuts import (
    META_NAME, Param, ShortcutAction, clashes, clean_bindings, has_command_modifier, keys_from_event, native_keys,
    new_binding_id, normalize_keys, split_keys,
)
from telescope.widgets.common import (
    Switch,
    ElidingLabel, NoScrollComboBox, NoScrollDoubleSpinBox, control_row,
    control_row_widget, dialog_buttons, dialog_header, dialog_layout, set_ui_role, ui_px, wrapped_note,
    add_card_header, card_action, card_layout, create_card, icon_button,
)
from telescope.widgets.controls import setting_row
from telescope.i18n import N_, tr

logger = logging.getLogger(__name__)

_SEARCH_WEIGHTS = {"name": 1.0, "keywords": 0.9, "card": 0.7, "extra": 0.6}

_TEXT_INPUTS = (QLineEdit, QAbstractSpinBox, QTextEdit, QPlainTextEdit)
_NEVER_TYPES = {
    Qt.Key.Key_Pause, Qt.Key.Key_Print, Qt.Key.Key_ScrollLock, Qt.Key.Key_VolumeMute, Qt.Key.Key_VolumeDown,
    Qt.Key.Key_VolumeUp, Qt.Key.Key_MediaPlay, Qt.Key.Key_MediaTogglePlayPause, Qt.Key.Key_MediaStop,
    Qt.Key.Key_MediaNext, Qt.Key.Key_MediaPrevious, Qt.Key.Key_MediaPause, Qt.Key.Key_MicMute,
}


def edits_text(keys: str) -> bool:
    """Whether a text box would use keys: a plain key that types or moves the cursor, not an F-key or media key,
    and nothing with Ctrl, Alt or Meta."""
    if has_command_modifier(keys):
        return False
    qkey = split_keys(keys)[2]
    return not (Qt.Key.Key_F1 <= qkey <= Qt.Key.Key_F35 or qkey in _NEVER_TYPES)
_ADD_TEXT = N_("Add keyboard shortcut…")
_DOUBLED_S = 0.15  # a global and an in-app press of one binding this close together are one key press


def _remote_param_problem(action: ShortcutAction, params: dict) -> str:
    """Why --action's settings won't do, or "": a typo must not quietly run the default instead."""
    for p in action.params:
        if p.name not in params:
            continue
        value = params[p.name]
        if p.kind == "number":
            try:
                number = float(value)
            except (TypeError, ValueError):
                number = math.nan
            if not math.isfinite(number):
                return tr("{name} has to be a number, not {value}.", name=p.name, value=repr(value))
        elif value not in [v for v, _ in p.options()]:
            options = " | ".join(str(v) for v, _ in p.options()) or tr("nothing right now")
            return tr("{name} can be {options}, not {value}.", name=p.name, options=options, value=repr(value))
        if value == "hold":
            # Translators: hold, toggle, on and off are values typed on the command line: keep them in English
            return tr("hold needs a key to let go of; use toggle, on or off with --action.")
    full = action.full_params(params)
    for p in action.params:
        if p.shown(full) and p.kind == "number" and full.get(p.name) is None:
            return tr("{action} needs {name}=… for that.", action=action.id, name=p.name)
    return ""


def action_command(action_id: str, params: dict) -> list:
    """The command line that runs this action in the running Telescope (for a desktop's own shortcut settings)."""
    if getattr(sys, "frozen", False):
        base = [str(Path(sys.executable).resolve())]
    else:  # the Python that runs this copy (start.sh's venv), straight at main.py: quicker, and quiet
        base = [str(Path(sys.executable)), str(autostart.app_dir() / "main.py")]
    words = [f"{k}={v:g}" if isinstance(v, float) else f"{k}={v}" for k, v in params.items() if v is not None]
    return base + ["--action", action_id] + words


def command_text(command: list) -> str:
    return subprocess.list2cmdline(command) if IS_WINDOWS else shlex.join(command)


def portal_app_id() -> str:
    """The id Telescope registers with the desktop portal: its menu entry's name (a dev profile's own)."""
    return autostart.APP_ID + ("-dev" if dev_profile.active() else "")


WINDOW_SHOWN = N_("Telescope: shown")


class ShortcutsPlugin(TelescopePlugin):
    name = "shortcuts"
    title = N_("Keyboard shortcuts")
    about = N_("Keys for Telescope's controls, including ones that work in other apps.")
    when_off = N_("Without it no shortcut does anything and Keyboard shortcuts isn't in Settings (your "
                  "shortcuts are kept for when it's back).")

    def __init__(self, backend_factory=None):
        self._backend_factory = backend_factory

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._bindings: list = []
        self._notify = False
        self._actions: dict = {}
        self._backend = None
        self._started = False
        self._held: dict = {}  # binding id: where it was pressed ("app" or "global"), while held
        self._held_scan: dict = {}  # binding id: the scan code of the key that pressed it inside Telescope
        self._editing = False  # the shortcut editor is open: global keys are being pressed to pick one
        self._filter: Optional[_KeyFilter] = None
        self._section: Optional[ShortcutsSection] = None
        self._with_menu: list = []  # controls given "Add keyboard shortcut…", so it can come off at shutdown
        self._dynamic: list = []  # actions whose controls come and go (the lens buttons)
        self._last_global: Optional[list] = None  # what the system was last handed, so a same list isn't rebound
        self._recent: dict = {}  # binding id: (source, monotonic time) of its last press, to drop a doubled one
        QTimer.singleShot(0, self.start)  # once every plugin is registered and its controls exist

    # ── Actions of its own: the ones that belong to the app, not a card ──────

    def create_actions(self) -> list:
        host = self._host
        streaming_modes = (("toggle", tr("Start or stop")), ("start", tr("Start")), ("stop", tr("Stop")))

        def stream(p):
            running = host.is_streaming() or host.is_starting()
            mode = p.get("mode", "toggle")
            if mode == "start" or (mode == "toggle" and not running):
                if running:
                    return tr("Already streaming")
                host.start_stream(interactive=QApplication.activeWindow() is not None)
                return tr("Streaming: starting")
            if not running:
                return tr("Not streaming")
            host.stop_all_streams()
            return tr("Streaming: stopped")

        def camera(p):
            on = host.is_camera_on()
            want = {"on": True, "off": False}.get(p.get("mode", "toggle"), not on)
            if want != on:
                if not want and not host.can_turn_camera_off()[0]:
                    return None
                host.set_camera_on(want)
            return tr("Phone camera: on") if host.is_camera_on() else tr("Phone camera: off")

        def window(_p):
            return tr(WINDOW_SHOWN) if host.toggle_window() else tr("Telescope: hidden")

        camera_modes = (("toggle", tr("Switch on or off")), ("on", tr("Turn on")), ("off", tr("Turn off")))
        streaming, camera_label = tr("Streaming"), tr("Phone camera")
        return [
            ShortcutAction("streaming.start_stop", streaming, "Telescope", stream,
                           params=(Param("mode", tr("Does"), "choice", "toggle", streaming_modes),),
                           describe=lambda p: tr("{label}: {value}", label=streaming,
                                                 value=dict(streaming_modes)[p['mode']].lower())),
            ShortcutAction("streaming.camera", camera_label, "Telescope", camera,
                           params=(Param("mode", tr("Does"), "choice", "toggle", camera_modes),),
                           describe=lambda p: tr("{label}: {value}", label=camera_label,
                                                 value=dict(camera_modes)[p['mode']].lower())),
            ShortcutAction("window.show_hide", tr("Show or hide Telescope"), "Telescope", window),
        ]

    # ── Start, apply ──────────────────────────────────────────────────────────

    def start(self):
        if self._started:
            return
        self._started = True
        self._actions = {}
        found = self._host.shortcut_actions()
        # By card, in the order the cards come; the app's own (Telescope) first
        groups = list(dict.fromkeys(["Telescope"] + [a.group for a in found]))
        for action in sorted(found, key=lambda a: groups.index(a.group)):
            self._actions.setdefault(action.id, action)
            if callable(action.widgets):  # controls that come and go: the key filter catches their right-clicks
                self._dynamic.append(action)
            else:
                for w in action.controls():
                    self._give_menu(w, action.id)
        app = QApplication.instance()
        self._filter = _KeyFilter(self)
        # On the focused control and the active window, not the app: PyQt can crash wrapping objects Qt is tearing down
        app.focusChanged.connect(self._filter.follow_focus)
        app.focusWindowChanged.connect(self._filter.follow_window)
        self._filter.follow_focus(None, QApplication.focusWidget())
        app.applicationStateChanged.connect(self._on_app_state)
        factory = self._backend_factory or (lambda: create_backend(portal_app_id()))
        try:
            self._backend = factory()
        except Exception:
            logger.exception("Global shortcuts couldn't start")
            self._backend = None
        if self._backend is not None:
            self._backend.pressed.connect(lambda bid: self.press(bid, "global"))
            self._backend.released.connect(lambda bid: self.release(bid))
            self._backend.changed.connect(self._refresh_section)
        self.apply()

    def actions(self) -> list:
        return list(self._actions.values())

    def action(self, action_id: str) -> Optional[ShortcutAction]:
        return self._actions.get(action_id)

    @property
    def backend(self):
        return self._backend

    @property
    def bindings(self) -> list:
        return [dict(b) for b in self._bindings]

    def set_bindings(self, bindings: list):
        self._bindings = clean_bindings(bindings)
        self._host.schedule_save()
        self.apply()

    def apply(self):
        """Hand the global bindings to the system; the rest are matched as keys come in."""
        for bid in list(self._held):
            self.release(bid)
        if self._backend is not None and self._started:
            skip = clashes(self._bindings)
            hotkeys = [Hotkey(b["id"], b["keys"], self._describe(b)) for b in self._bindings
                       if b["global"] and b["id"] not in skip and b["action"] in self._actions]
            if hotkeys != self._last_global:  # rebinding can ask the user to confirm them all again (Plasma)
                self._last_global = hotkeys
                self._backend.apply(hotkeys)
        self._refresh_section()

    def _describe(self, binding: dict) -> str:
        action = self._actions.get(binding["action"])
        return action.summary(binding["params"]) if action else binding["action"]

    def describe(self, binding: dict) -> str:
        return self._describe(binding)

    def where(self, binding: dict) -> tuple:
        """(text, ok) for the Works column: where this binding works, or why it doesn't."""
        if binding["id"] in clashes(self._bindings):
            return tr("Key used by a shortcut above"), False
        if binding["action"] not in self._actions:
            return tr("Not in this version of Telescope"), False
        if not binding["global"]:
            return tr("In Telescope"), True
        if self._backend is None:
            return tr("In Telescope (no global shortcuts here)"), False
        status = self._backend.status(binding["id"])
        if status is None:
            return tr("Everywhere (setting up)"), True
        if status.working:
            return status.text, True
        return (tr("In Telescope: {status}", status=status.text), False) if status.reaches_app else (status.text, False)

    # ── Running ───────────────────────────────────────────────────────────────

    def binding_for_keys(self, keys: str) -> Optional[dict]:
        """The binding keys pressed inside Telescope run: the first with those keys, if this version has its action.
        A global one too: a system that took the key never lets it reach us, and the one it bound may be another
        key (the portal lets the desktop pick), which then still works here."""
        for b in self._bindings:
            if b["keys"] == keys:
                return b if b["action"] in self._actions else None
        return None

    def press(self, bid: str, source: str, repeat: bool = False, scan: int = 0) -> Optional[str]:
        binding = next((b for b in self._bindings if b["id"] == bid), None)
        if binding is None:
            return None
        if source == "global" and self._editing:
            return None  # the user is pressing keys to pick one, not to run them
        action = self._actions.get(binding["action"])
        if action is None:
            return None
        params = action.full_params(binding["params"])
        if repeat and not action.repeats(params):
            return None
        now = time.monotonic()
        last = self._recent.get(bid)
        if not repeat and last is not None and last[0] != source and now - last[1] < _DOUBLED_S:
            return None  # the same key press, delivered both by the system and inside Telescope
        self._recent[bid] = (source, now)
        if not repeat:
            self._held[bid] = source
            self._held_scan[bid] = scan
        try:
            said = action.run(params)
        except Exception:
            logger.exception("Shortcut %s failed", binding["action"])
            said = None
        if said is None:
            if not repeat:
                self._held.pop(bid, None)  # nothing happened, so there's nothing to undo on release
            return None
        # Showing the window says it all, and Windows only activates it a moment later
        shown = said == tr(WINDOW_SHOWN)
        if source != "app" and self._notify and QApplication.activeWindow() is None and not shown:
            self._host.send_notification("Telescope", said, urgent=False)
        return said

    def is_held(self, bid: str) -> bool:
        return bid in self._held

    def release(self, bid: str):
        self._held_scan.pop(bid, None)
        if self._held.pop(bid, None) is None:
            return
        binding = next((b for b in self._bindings if b["id"] == bid), None)
        action = self._actions.get(binding["action"]) if binding else None
        if action is not None and action.release is not None:
            try:
                action.release(action.full_params(binding["params"]))
            except Exception:
                logger.exception("Letting go of shortcut %s failed", binding["action"])

    def release_app_keys(self, key: Optional[int] = None, scan: int = 0):
        """Keys held inside Telescope come up: the one with this Qt key or scan code (a shifted key can come up as
        another key once Shift is let go first), or all of them (focus moved away)."""
        for bid, source in list(self._held.items()):
            if source != "app":
                continue
            binding = next((b for b in self._bindings if b["id"] == bid), None)
            if binding is None or key is None or split_keys(binding["keys"])[2] == key \
                    or (scan and self._held_scan.get(bid) == scan):
                self.release(bid)

    def _on_app_state(self, state):
        if state != Qt.ApplicationState.ApplicationActive:
            self.release_app_keys()

    def handle_remote(self, request: dict) -> dict:
        """A `telescope --action` request: {"list": True}, or {"action": id, "params": {name: value}}."""
        if request.get("list"):
            return {"ok": True, "text": self.list_text()}
        action = self._actions.get(request.get("action"))
        if action is None:
            return {"ok": False, "text": tr("No action called {action}. telescope --list-actions lists them.",
                                            action=repr(request.get('action')))}
        params = request.get("params") if isinstance(request.get("params"), dict) else {}
        unknown = sorted(set(params) - {p.name for p in action.params})
        if unknown:
            return {"ok": False, "text": tr("{action} has no setting {name}.", action=action.id, name=repr(unknown[0]))}
        problem = _remote_param_problem(action, params)
        if problem:
            return {"ok": False, "text": problem}
        try:
            said = action.run(action.full_params(params))
        except Exception:
            logger.exception("--action %s failed", action.id)
            return {"ok": False, "text": tr("That failed; Copy diagnostics has the details.")}
        if said is None:
            return {"ok": False, "text": tr("{action} can't be used right now (no phone, or the control is off).",
                                            action=action.label)}
        if self._notify and QApplication.activeWindow() is None:
            self._host.send_notification("Telescope", said, urgent=False)
        return {"ok": True, "text": said}

    def list_text(self) -> str:
        lines = []
        for action in self._actions.values():
            lines.append(f"{action.id}  ({action.group}: {action.label})")
            for p in action.params:
                if p.kind == "choice":
                    values = " | ".join(str(v) for v, _ in p.options()) or "…"
                    lines.append(f"    {p.name}={values}  (default {p.default})" if p.default != ""
                                 else f"    {p.name}={values}")
                else:
                    lo, hi = p.bounds()
                    lines.append(f"    {p.name}=<number {lo:g} to {hi:g}{p.suffix}>")
        return "\n".join(lines)

    # ── Right-click on a control ──────────────────────────────────────────────

    def _give_menu(self, widget: QWidget, action_id: str):
        if widget is None or widget.contextMenuPolicy() == Qt.ContextMenuPolicy.CustomContextMenu:
            return
        widget.setContextMenuPolicy(Qt.ContextMenuPolicy.CustomContextMenu)
        widget.customContextMenuRequested.connect(
            lambda pos, w=widget, a=action_id: self.control_menu(w, a, pos))
        self._with_menu.append(widget)

    def dynamic_action_for(self, widget: QWidget) -> Optional[str]:
        """The action whose current controls include widget, for one that came after start (a lens button)."""
        for action in self._dynamic:
            try:
                if widget in action.controls():
                    return action.id
            except Exception:
                continue
        return None

    def control_menu(self, widget: QWidget, action_id: str, pos: QPoint):
        window = widget.window()  # the menu belongs to the window: a lens button can go while it's open
        at = widget.mapToGlobal(pos)
        menu = QMenu(window)
        actions = []
        for action in self._actions.values():
            try:
                if widget in action.controls():
                    actions.append(action)
            except Exception:
                continue  # another plugin's dynamic controls may have disappeared
        ids = [a.id for a in actions] if len(actions) > 1 else [action_id]
        for aid in ids:
            text = tr("Add shortcut for {action}…", action=self._actions[aid].label) if len(ids) > 1 else tr(_ADD_TEXT)
            add = QAction(text, menu)
            add.triggered.connect(lambda _=False, a=aid: self.add_binding(a, parent=window))
            menu.addAction(add)
        existing = [b for b in self._bindings if b["action"] in ids]
        if existing:
            menu.addSeparator()
            for b in existing:
                edit = QAction(f"{native_keys(b['keys'])}: {self._describe(b)}".replace("&", "&&"), menu)
                edit.triggered.connect(lambda _=False, bid=b["id"]: self.edit_binding(bid, parent=window))
                menu.addAction(edit)
        menu.exec(at)
        menu.deleteLater()

    # ── Editing ───────────────────────────────────────────────────────────────

    def add_binding(self, action_id: Optional[str] = None, parent=None) -> Optional[dict]:
        editor = BindingEditor(self, {"id": new_binding_id(), "keys": "", "action": action_id or "",
                                      "params": {}, "global": True}, parent)
        accepted, binding = self._run_editor(editor)
        if not accepted:
            return None
        self.set_bindings(self._bindings + [binding])
        return binding

    def edit_binding(self, bid: str, parent=None) -> Optional[dict]:
        old = next((b for b in self._bindings if b["id"] == bid), None)
        if old is None:
            return None
        editor = BindingEditor(self, dict(old), parent)
        accepted, binding = self._run_editor(editor)
        if not accepted:
            return None
        self.set_bindings([binding if b["id"] == bid else b for b in self._bindings])
        return binding

    def _run_editor(self, editor: "BindingEditor") -> tuple:
        """Show the editor with the global keys let go of (where that's cheap), so pressing one records it
        instead of running it."""
        self._editing = True
        if self._backend is not None:
            self._backend.suspend(True)
        try:
            accepted = editor.exec() == QDialog.DialogCode.Accepted
            return accepted, editor.binding()
        finally:
            self._editing = False
            if self._backend is not None:
                self._backend.suspend(False)
            editor.deleteLater()

    def rebind_global(self):
        """Hand the global keys to the system again, for after the desktop's confirm dialog was cancelled."""
        self._last_global = None
        self.apply()

    def remove_binding(self, bid: str):
        self.set_bindings([b for b in self._bindings if b["id"] != bid])

    def set_notify(self, on: bool):
        self._notify = on
        self._host.schedule_save()

    @property
    def notify(self) -> bool:
        return self._notify

    def open_system_settings(self) -> bool:
        """The desktop's own shortcut settings: the portal's dialog for these, or Plasma's Shortcuts page."""
        if self._backend is not None and self._backend.configure():
            return True
        if self._backend is not None and self._backend.can_rebind():
            self.rebind_global()  # some weren't confirmed: the desktop asks again
            return True
        if IS_LINUX:
            for cmd in (["systemsettings", "kcm_keys"], ["kcmshell6", "kcm_keys"], ["kcmshell5", "kcm_keys"]):
                try:
                    subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                     start_new_session=True)
                    return True
                except OSError:
                    continue
        return False

    # ── Menu and dialog ───────────────────────────────────────────────────────

    def create_settings_sections(self) -> list:
        self._section = ShortcutsSection(self)
        return [SettingsSection("shortcuts", self._section, on_show=self._section.refresh)]

    def _refresh_section(self):
        if self._section is not None and self._section.isVisible():
            self._section.refresh()

    # ── Config ────────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"bindings": [dict(b) for b in self._bindings], "notify": self._notify}

    def set_config(self, cfg: dict):
        self._bindings = clean_bindings(cfg.get("bindings"))
        self._notify = cfg.get("notify") is True
        if self._started:
            self.apply()

    def diagnostics(self) -> dict:
        if not self._bindings:
            return {"Keyboard shortcuts": "none"}
        everywhere = sum(1 for b in self._bindings if b["global"])
        backend = type(self._backend).__name__ if self._backend is not None else "none"
        return {"Keyboard shortcuts": f"{len(self._bindings)} ({everywhere} everywhere, via {backend})"}

    def shutdown(self):
        if self._backend is not None:
            try:
                self._backend.stop()
            except Exception:
                logger.exception("Couldn't let go of the global shortcuts")
        app = QApplication.instance()
        if self._filter is not None and app is not None:
            try:
                app.focusChanged.disconnect(self._filter.follow_focus)
                app.focusWindowChanged.disconnect(self._filter.follow_window)
            except (TypeError, RuntimeError):
                pass
            self._filter.let_go()
            self._filter = None


class _KeyFilter(QObject):
    """Sees every key inside Telescope before the control with focus does, and runs the binding it belongs to."""

    def __init__(self, plugin: ShortcutsPlugin):
        super().__init__()
        self._plugin = plugin
        # Owning a watched window can destroy it inside a focus callback while Qt still uses its pointer.
        self._watched: WeakSet = WeakSet()
        self._watching = True

    def follow_focus(self, _old, new):
        window = QApplication.activeWindow()
        if new is not None and window is None:
            window = new.window()
        self._watch([w for w in (new, window) if w is not None])

    def follow_window(self, _window):
        # A window with nothing that takes focus gets its keys too; later, as Qt sets the active window after this
        QTimer.singleShot(0, lambda: self._watching and self.follow_focus(None, QApplication.focusWidget()))

    def let_go(self):
        self._watching = False
        self._watch([])

    def _watch(self, widgets: list):
        for w in self._watched:
            if not any(w is n for n in widgets):
                try:
                    w.removeEventFilter(self)
                except RuntimeError:
                    pass  # already deleted
        for w in widgets:
            if not any(w is o for o in self._watched):
                w.installEventFilter(self)
        self._watched = WeakSet(widgets)

    def _binding(self, event) -> Optional[dict]:
        keys = keys_from_event(event.key(), event.modifiers())
        if not keys:
            return None
        binding = self._plugin.binding_for_keys(keys)
        if binding is None:
            return None
        if QApplication.activeModalWidget() is not None or QApplication.activePopupWidget() is not None:
            return None  # a dialog is asking something (the shortcut editor among them), or a menu is open
        focus = QApplication.focusWidget()
        if focus is not None and _inside_key_editor(focus):
            return None
        typing = isinstance(focus, _TEXT_INPUTS) or (isinstance(focus, QComboBox) and focus.isEditable())
        if typing and edits_text(binding["keys"]):
            return None  # a plain key belongs to the text box
        return binding

    def eventFilter(self, obj, event):
        kind = event.type()
        if kind == QEvent.Type.ContextMenu and obj.isWidgetType():
            # A right-click the control ignored comes up to the window: find the control that was under it
            widget = obj.childAt(event.pos()) or obj
            while widget is not None:
                action_id = self._plugin.dynamic_action_for(widget)
                if action_id is not None:
                    self._plugin.control_menu(widget, action_id, widget.mapFrom(obj, event.pos()))
                    return True
                if widget is obj:
                    break
                widget = widget.parentWidget()
            return False
        if kind not in (QEvent.Type.KeyPress, QEvent.Type.ShortcutOverride, QEvent.Type.KeyRelease):
            return False
        if not obj.isWidgetType():
            return False  # the same event comes through the window first; act on it once, at a widget
        if obj is not (QApplication.focusWidget() or obj.window()):
            return False  # Qt hands a key to the focused widget first; one it ignores comes back at each parent
        if kind == QEvent.Type.KeyRelease:
            if not event.isAutoRepeat():
                self._plugin.release_app_keys(event.key(), event.nativeScanCode())
            return False
        binding = self._binding(event)
        if binding is None:
            return False
        if kind == QEvent.Type.ShortcutOverride:
            event.accept()  # the key comes as a KeyPress then, instead of going to a menu shortcut
            return True
        said = self._plugin.press(binding["id"], "app", repeat=event.isAutoRepeat(), scan=event.nativeScanCode())
        # A binding that couldn't run (its control greyed out) leaves the key to the control with focus
        return said is not None or self._plugin.is_held(binding["id"])


def _inside_key_editor(widget: QWidget) -> bool:
    while widget is not None:
        if isinstance(widget, QKeySequenceEdit):
            return True
        widget = widget.parentWidget()
    return False


# ── Dialogs ───────────────────────────────────────────────────────────────────

class _BindingRow(QFrame):
    """One shortcut as a strip: the key, what it does with where it works (or why not, in amber) under it, and its
    edit and remove buttons. Clicking the strip edits it too; right-clicking offers its command line."""

    def __init__(self, keys: str, does: str, where: str, ok: bool, on_edit, on_remove, on_menu):
        super().__init__()
        self.setObjectName("setting_row")
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self._on_edit = on_edit
        lay = QHBoxLayout(self)
        lay.setContentsMargins(12, 6, 6, 6)
        lay.setSpacing(12)
        key = QLabel(native_keys(keys))
        key.setObjectName("key_chip")
        key.setFixedWidth(ui_px(150))
        lay.addWidget(key)
        texts = QVBoxLayout()
        texts.setSpacing(1)
        self.does = ElidingLabel(does)
        self.does.setMinimumWidth(ui_px(120))
        texts.addWidget(self.does)
        self.where = ElidingLabel(where)
        self.where.setObjectName("setting_sub" if ok else "status_warn")
        self.where.setProperty("sub", True)
        texts.addWidget(self.where)
        lay.addLayout(texts, 1)
        self.edit_btn = icon_button("pencil", tr("Edit {shortcut}", shortcut=does), tr("Edit"))
        self.edit_btn.clicked.connect(on_edit)
        lay.addWidget(self.edit_btn)
        self.remove_btn = icon_button("trash", tr("Remove {shortcut}", shortcut=does), tr("Remove"))
        self.remove_btn.clicked.connect(on_remove)
        lay.addWidget(self.remove_btn)
        self.setContextMenuPolicy(Qt.ContextMenuPolicy.CustomContextMenu)
        self.customContextMenuRequested.connect(lambda pos: on_menu(self.mapToGlobal(pos)))

    def mouseReleaseEvent(self, event):
        if event.button() == Qt.MouseButton.LeftButton and self.rect().contains(event.position().toPoint()):
            self._on_edit()
        super().mouseReleaseEvent(event)


class ShortcutsSection(QWidget):
    def __init__(self, plugin: ShortcutsPlugin):
        super().__init__()
        self._plugin = plugin
        body = QVBoxLayout(self)
        body.setContentsMargins(0, 0, 0, 0)
        body.setSpacing(14)
        body.addWidget(wrapped_note(tr("Keys for Telescope's controls, with how much each one changes. Right-click "
                                       "most controls to add one for it.")))

        keys_card = create_card()
        keys_lay = card_layout(keys_card)
        self._add_btn = card_action(tr("Add"), "plus", tr("Pick a control and the key for it"))
        self._add_btn.clicked.connect(lambda: self._plugin.add_binding(parent=self))
        add_card_header(keys_lay, tr("Shortcuts"), "keyboard", action=self._add_btn)
        keywords(keys_card, tr("hotkey hotkeys keybind keybinding key bindings keys push to talk mute add remove", ctx="search"))
        self._rows_lay = QVBoxLayout()
        self._rows_lay.setSpacing(8)
        keys_lay.addLayout(self._rows_lay)
        self._rows: list = []
        self._empty = wrapped_note(tr("No shortcuts yet. Add one, or right-click a control in the main window."))
        keys_lay.addWidget(self._empty)
        body.addWidget(keys_card)

        global_card = create_card()
        global_lay = card_layout(global_card)
        add_card_header(global_lay, tr("Everywhere"), "monitor")
        keywords(global_card, tr("global system wide hotkeys background other apps", ctx="search"))
        self._system_btn = QPushButton(tr("Open"))
        self._system_btn.setToolTip(tr("Where your desktop lets you change the keys of global shortcuts"))
        self._system_btn.clicked.connect(self._open_system)
        strip, self._note = setting_row(tr("Desktop shortcut settings"), "", self._system_btn,
                                        keywords=tr("global hotkeys portal kde plasma gnome desktop keys rebind",
                                                    ctx="search"))
        self._note.setObjectName("setting_sub")
        global_lay.addWidget(strip)
        self._notify_box = Switch()
        self._notify_box.toggled.connect(self._plugin.set_notify)
        strip, _ = setting_row(tr("Notify when one runs"), tr("While Telescope is in the background"),
                               self._notify_box, keywords=tr("notification notify toast confirmation popup", ctx="search"))
        global_lay.addWidget(strip)
        body.addWidget(global_card)

    def refresh(self):
        for row in self._rows:
            row.hide()  # deleteLater waits for the event loop, and until then it'd still take up room
            row.deleteLater()
        self._rows = []
        for b in self._plugin.bindings:
            where, ok = self._plugin.where(b)
            bid = b["id"]
            row = _BindingRow(b["keys"], self._plugin.describe(b), where, ok,
                              lambda _c=False, i=bid: self._edit(i), lambda _c=False, i=bid: self._remove(i),
                              lambda at, i=bid: self._row_menu(i, at))
            self._rows_lay.addWidget(row)
            self._rows.append(row)
        self._empty.setVisible(not self._rows)
        backend = self._plugin.backend
        if backend is not None:
            self._note.setText(backend.note())
        else:
            self._note.setText(tr("Global shortcuts aren't available on this desktop, so these work inside "
                                  "Telescope. For keys that work everywhere, bind a shortcut's Copy command line in "
                                  "your desktop's keyboard settings."))
        self._note.setVisible(bool(self._note.text()))
        self._system_btn.setVisible(IS_LINUX and (backend is None or backend.system_picks_keys))
        self._notify_box.blockSignals(True)
        self._notify_box.setChecked(self._plugin.notify)
        self._notify_box.blockSignals(False)

    def _edit(self, bid: str):
        self._plugin.edit_binding(bid, parent=self)

    def _remove(self, bid: str):
        self._plugin.remove_binding(bid)

    def _row_menu(self, bid: str, at: QPoint):
        binding = next((b for b in self._plugin.bindings if b["id"] == bid), None)
        if binding is None:
            return
        menu = QMenu(self)
        copy = QAction(tr("Copy command line"), menu)
        copy.triggered.connect(lambda: QGuiApplication.clipboard().setText(
            command_text(action_command(binding["action"], binding["params"]))))
        copy.setEnabled(binding["params"].get("mode") != "hold")  # nothing would let go of it
        menu.addAction(copy)
        menu.exec(at)
        menu.deleteLater()

    def _open_system(self):
        if not self._plugin.open_system_settings():
            self._note.setText(tr("Couldn't open your desktop's shortcut settings. Look for Shortcuts in its "
                                  "System Settings."))


class BindingEditor(QDialog):
    """Add or change one binding: the control, how much, the key, and whether it works everywhere."""

    def __init__(self, plugin: ShortcutsPlugin, binding: dict, parent=None):
        super().__init__(parent)
        self._plugin = plugin
        self._binding = dict(binding)
        self._param_widgets: dict = {}
        self.setWindowTitle(tr("Keyboard shortcut"))
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setModal(True)
        self.setMinimumWidth(ui_px(520))
        lay = dialog_layout(self)
        dialog_header(lay, tr("Keyboard shortcut"), tr("Pick a control, what the key does to it, then press the key."))

        self._search = QLineEdit()
        self._search.setPlaceholderText(tr("Search: mute, zoom, exposure, preset…"))
        self._search.setClearButtonEnabled(True)
        self._search.textChanged.connect(self._filter_actions)
        lay.addLayout(control_row(tr("Find"), self._search, stretch=True))
        self._action_combo = NoScrollComboBox()
        self._shown_action = None  # the action the settings below were built for
        self._fill_actions()
        self._action_combo.currentIndexChanged.connect(lambda _i: self._on_action_picked())
        lay.addLayout(control_row(tr("Control"), self._action_combo, stretch=True))

        self._params_box = QWidget()
        self._params_lay = QVBoxLayout(self._params_box)
        self._params_lay.setContentsMargins(0, 0, 0, 0)
        self._params_lay.setSpacing(8)
        lay.addWidget(self._params_box)

        self._keys_edit = QKeySequenceEdit()
        self._keys_edit.setMaximumSequenceLength(1)
        self._keys_edit.setClearButtonEnabled(True)
        if binding.get("keys"):
            self._keys_edit.setKeySequence(QKeySequence.fromString(binding["keys"],
                                                                   QKeySequence.SequenceFormat.PortableText))
        self._keys_edit.keySequenceChanged.connect(lambda _s: self._check())
        if META_NAME != "Meta":  # Qt writes the Windows key as Meta
            self._keys_edit.keySequenceChanged.connect(lambda _s: self._show_native_keys())
            self._show_native_keys()
        lay.addLayout(control_row(tr("Key"), self._keys_edit, stretch=True))

        self._global_box = Switch(tr("Also works while another app is in front"))
        self._global_box.setChecked(binding.get("global", True))
        self._global_box.toggled.connect(lambda _on: self._check())
        lay.addLayout(control_row("", self._global_box, stretch=True))
        self._problem = wrapped_note("", kind="status_warn")
        lay.addWidget(self._problem)
        lay.addStretch(1)  # spare height goes below, so rows don't move when the note or a setting hides

        self._copy_btn = QPushButton(tr("Copy command"))
        self._copy_btn.setToolTip(tr("A command line that does the same, for your desktop's own keyboard settings"))
        self._copy_btn.clicked.connect(self._copy_command)
        cancel = QPushButton(tr("Cancel"))
        cancel.clicked.connect(self.reject)
        self._save_btn = QPushButton(tr("Save"))
        set_ui_role(self._save_btn, "primary")
        self._save_btn.setDefault(False)
        self._save_btn.setAutoDefault(False)
        self._save_btn.clicked.connect(self.accept)
        bar = dialog_buttons(lay, cancel, self._save_btn)
        bar.insertWidget(0, self._copy_btn)

        self._build_params(self._binding.get("params"))
        # A new shortcut from the dialog starts by finding the control; one for a known control, at the key
        (self._keys_edit if self._binding.get("action") else self._search).setFocus()

    @staticmethod
    def _matches(action: ShortcutAction, query: str) -> bool:
        """Whether the query fits the action's card, name, id, keywords or the names of what it can do."""
        if not fuzzy.split(query):
            return True
        choices = [label for p in action.params if p.kind == "choice" and not callable(p.choices)
                   for _v, label in p.options()]
        doc = fuzzy.Doc(name=action.label, keywords=action.keywords, card=action.group,
                        extra=" ".join([action.id, *choices]))
        return doc.score(query, _SEARCH_WEIGHTS) > 0

    def _filter_actions(self, query: str):
        combo = self._action_combo
        keep = combo.currentData() or self._shown_action  # a search that found nothing forgets no pick
        combo.blockSignals(True)
        combo.clear()
        self._fill_actions(query, keep)
        combo.blockSignals(False)
        self._on_action_picked()

    def _on_action_picked(self):
        picked = self._action_combo.currentData()
        self._params_box.setVisible(picked is not None)  # kept, not dropped, while a search shows nothing
        if picked is not None and picked != self._shown_action:
            self._build_params()
        else:
            self._check()

    def _fill_actions(self, query: str = "", wanted=None):
        combo = self._action_combo
        model = combo.model()
        group = None
        wanted = wanted or self._binding.get("action")
        for action in self._plugin.actions():
            if not self._matches(action, query):
                continue
            if action.group != group:
                group = action.group
                combo.addItem(group.upper())
                header = model.item(combo.count() - 1)
                header.setFlags(Qt.ItemFlag.NoItemFlags)
                header.setAccessibleDescription("heading")  # the combo's delegate paints it as a title, not an entry
            combo.addItem(action.label, action.id)
            if action.id == wanted:
                combo.setCurrentIndex(combo.count() - 1)
        if wanted and not query and combo.findData(wanted) < 0:  # an action this version doesn't have: kept
            combo.addItem(tr("Unknown ({action})", action=wanted), wanted)
            combo.setCurrentIndex(combo.count() - 1)
        if combo.currentData() is None:
            first = next((i for i in range(combo.count()) if combo.itemData(i) is not None), -1)
            combo.setCurrentIndex(first)

    def _action(self) -> Optional[ShortcutAction]:
        return self._plugin.action(self._action_combo.currentData())

    def _build_params(self, given: Optional[dict] = None):
        while self._params_lay.count():
            item = self._params_lay.takeAt(0)
            if item.widget() is not None:
                item.widget().deleteLater()
        self._param_widgets = {}
        self._shown_action = self._action_combo.currentData() or self._shown_action
        action = self._action()
        if action is None:
            self._check()
            return
        values = action.full_params(given if given is not None else {})
        for p in action.params:
            value = values.get(p.name)
            if p.kind == "choice":
                w = NoScrollComboBox()
                for v, label in p.options():
                    w.addItem(label, v)
                at = w.findData(value)
                if at < 0 and value not in (None, ""):  # a preset that's gone, say: keep it, so Save keeps it
                    w.addItem(tr("{value} (not there now)", value=value), value)
                    at = w.count() - 1
                w.setCurrentIndex(max(at, 0))
                w.currentIndexChanged.connect(lambda _i: self._show_params())
            else:
                w = NoScrollDoubleSpinBox()
                lo, hi = p.bounds()
                w.setDecimals(p.decimals)
                w.setRange(lo, hi)
                w.setSuffix(p.suffix)
                if value is None:
                    value = action.current() if action.current is not None else lo
                w.setValue(float(value))
            row = control_row_widget(p.label, w, stretch=True)
            self._params_lay.addWidget(row)
            self._param_widgets[p.name] = (p, w, row)
        self._show_params()

    def _params(self) -> dict:
        out = {}
        for name, (p, w, row) in self._param_widgets.items():
            out[name] = w.currentData() if p.kind == "choice" else round(w.value(), p.decimals)
        return out

    def _show_params(self):
        params = self._params()
        for p, w, row in self._param_widgets.values():
            row.setVisible(p.shown(params))
        self._check()
        self.adjustSize()

    def _show_native_keys(self):
        line = self._keys_edit.findChild(QLineEdit)
        if line is not None and self._keys():
            line.setText(native_keys(self._keys()))

    def _keys(self) -> str:
        return normalize_keys(self._keys_edit.keySequence().toString(QKeySequence.SequenceFormat.PortableText))

    def _check(self):
        keys = self._keys()
        problem, blocking = "", True
        if self._action_combo.currentData() is None:
            problem = (tr("No control matches that search.") if self._action_combo.count() == 0
                       else tr("Pick a control."))
        elif not keys:
            problem = (tr("Press a key, with Ctrl, Alt, Shift or {meta} if you like.", meta=META_NAME)
                       if self._keys_edit.keySequence().isEmpty()
                       else tr("A modifier can't be a shortcut on its own. Add a key to it."))
        elif (other := next((b for b in self._plugin.bindings
                             if b["keys"] == keys and b["id"] != self._binding["id"]), None)) is not None:
            problem = tr('{keys} already does "{action}". Pick another key.', keys=native_keys(keys),
                         action=self._plugin.describe(other))
        elif self._global_box.isChecked() and edits_text(keys):
            problem, blocking = tr("Everywhere, {keys} would stop typing it in other apps. Adding Ctrl, Alt or {meta} "
                                   "is safer.", keys=native_keys(keys), meta=META_NAME), False
        hold = self._params().get("mode") == "hold"
        self._copy_btn.setEnabled(not hold)
        self._copy_btn.setToolTip(tr("A command can't hold a key down; pick another choice to copy one") if hold else
                                  tr("A command line that does the same, for your desktop's own keyboard settings"))
        self._problem.setText(problem)
        self._problem.setVisible(bool(problem))
        self._save_btn.setEnabled(not (problem and blocking))

    def _copy_command(self):
        action_id = self._action_combo.currentData()
        if action_id and self._params().get("mode") != "hold":
            QGuiApplication.clipboard().setText(command_text(action_command(action_id, self._params())))
            self._copy_btn.setText(tr("Copied"))
            QTimer.singleShot(1500, lambda: self._copy_btn.setText(tr("Copy command")))

    def binding(self) -> dict:
        return {"id": self._binding["id"], "keys": self._keys(), "action": self._action_combo.currentData(),
                "params": self._params(), "global": self._global_box.isChecked()}
