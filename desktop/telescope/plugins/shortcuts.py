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
import shlex
import subprocess
import sys
from pathlib import Path
from typing import Optional

from PyQt6.QtCore import QEvent, QObject, QPoint, QSize, Qt, QTimer
from PyQt6.QtGui import QAction, QGuiApplication, QKeySequence
from PyQt6.QtWidgets import (
    QAbstractSpinBox, QApplication, QCheckBox, QComboBox, QDialog, QHBoxLayout, QKeySequenceEdit, QLabel,
    QLineEdit, QListWidget, QListWidgetItem, QMenu, QPlainTextEdit, QPushButton, QTextEdit, QVBoxLayout, QWidget,
)

from telescope import dev_profile
from telescope.platform import IS_LINUX, IS_WINDOWS, autostart
from telescope.platform.hotkeys import Hotkey, create_backend
from telescope.plugin import TelescopePlugin
from telescope.shortcuts import (
    Param, ShortcutAction, clashes, clean_bindings, has_command_modifier, keys_from_event, native_keys,
    new_binding_id, normalize_keys, split_keys,
)
from telescope.widgets.common import (
    ElidingLabel, NoScrollComboBox, NoScrollDoubleSpinBox, action_button, button_row, control_row,
    control_row_widget, dialog_buttons, dialog_header, dialog_layout, set_ui_role, ui_px, wrapped_note,
)

logger = logging.getLogger(__name__)

_TEXT_INPUTS = (QLineEdit, QAbstractSpinBox, QTextEdit, QPlainTextEdit)
_ADD_TEXT = "Add keyboard shortcut…"


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


class ShortcutsPlugin(TelescopePlugin):
    name = "shortcuts"

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
        self._filter: Optional[_KeyFilter] = None
        self._dlg: Optional[ShortcutsDialog] = None
        self._with_menu: list = []  # controls given "Add keyboard shortcut…", so it can come off at shutdown
        QTimer.singleShot(0, self.start)  # once every plugin is registered and its controls exist

    # ── Actions of its own: the ones that belong to the app, not a card ──────

    def create_actions(self) -> list:
        host = self._host
        streaming_modes = (("toggle", "Start or stop"), ("start", "Start"), ("stop", "Stop"))

        def stream(p):
            running = host.is_streaming() or host.is_starting()
            mode = p.get("mode", "toggle")
            if mode == "start" or (mode == "toggle" and not running):
                if running:
                    return "Already streaming"
                host.start_stream(interactive=QApplication.activeWindow() is not None)
                return "Streaming: starting"
            if not running:
                return "Not streaming"
            host.stop_all_streams()
            return "Streaming: stopped"

        def camera(p):
            on = host.is_camera_on()
            want = {"on": True, "off": False}.get(p.get("mode", "toggle"), not on)
            if want != on:
                if not want and not host.can_turn_camera_off()[0]:
                    return None
                host.set_camera_on(want)
            return f"Phone camera: {'on' if host.is_camera_on() else 'off'}"

        def window(_p):
            host.toggle_window()
            return None if QApplication.activeWindow() is None else "Telescope"

        camera_modes = (("toggle", "Switch on or off"), ("on", "Turn on"), ("off", "Turn off"))
        return [
            ShortcutAction("streaming.start_stop", "Streaming", "Telescope", stream,
                           params=(Param("mode", "Does", "choice", "toggle", streaming_modes),),
                           describe=lambda p: f"Streaming: {dict(streaming_modes)[p['mode']].lower()}"),
            ShortcutAction("streaming.camera", "Phone camera", "Telescope", camera,
                           params=(Param("mode", "Does", "choice", "toggle", camera_modes),),
                           describe=lambda p: f"Phone camera: {dict(camera_modes)[p['mode']].lower()}"),
            ShortcutAction("window.show_hide", "Show or hide Telescope", "Telescope", window),
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
            for w in action.controls():
                self._give_menu(w, action.id)
        app = QApplication.instance()
        self._filter = _KeyFilter(self)
        app.installEventFilter(self._filter)
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
            self._backend.changed.connect(self._refresh_dialog)
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
            self._backend.apply([
                Hotkey(b["id"], b["keys"], self._describe(b)) for b in self._bindings
                if b["global"] and b["id"] not in skip and b["action"] in self._actions])
        self._refresh_dialog()

    def _describe(self, binding: dict) -> str:
        action = self._actions.get(binding["action"])
        return action.summary(binding["params"]) if action else binding["action"]

    def describe(self, binding: dict) -> str:
        return self._describe(binding)

    def where(self, binding: dict) -> tuple:
        """(text, ok) for the Works column: where this binding works, or why it doesn't."""
        if binding["id"] in clashes(self._bindings):
            return "Key used by a shortcut above", False
        if binding["action"] not in self._actions:
            return "Not in this version of Telescope", False
        if not binding["global"]:
            return "In Telescope", True
        if self._backend is None:
            return "In Telescope (no global shortcuts here)", False
        status = self._backend.status(binding["id"])
        if status is None:
            return "Everywhere (setting up)", True
        return (status.text, True) if status.working else (f"In Telescope: {status.text}", False)

    def _globally_bound(self, binding: dict) -> bool:
        if not binding["global"] or self._backend is None:
            return False
        status = self._backend.status(binding["id"])
        return status is not None and status.working

    # ── Running ───────────────────────────────────────────────────────────────

    def binding_for_keys(self, keys: str) -> Optional[dict]:
        """The binding keys pressed inside Telescope run: the first with those keys, unless the system already
        delivers it (then the key never reaches us anyway, and acting on it twice would be wrong)."""
        for b in self._bindings:
            if b["keys"] == keys:
                return None if self._globally_bound(b) else b
        return None

    def press(self, bid: str, source: str, repeat: bool = False) -> Optional[str]:
        binding = next((b for b in self._bindings if b["id"] == bid), None)
        if binding is None:
            return None
        action = self._actions.get(binding["action"])
        if action is None:
            return None
        params = action.full_params(binding["params"])
        if repeat and not action.repeats(params):
            return None
        if not repeat:
            self._held[bid] = source
        try:
            said = action.run(params)
        except Exception:
            logger.exception("Shortcut %s failed", binding["action"])
            return None
        if said and source != "app" and self._notify and QApplication.activeWindow() is None:
            self._host.send_notification("Telescope", said, urgent=False)
        return said

    def release(self, bid: str):
        if self._held.pop(bid, None) is None:
            return
        binding = next((b for b in self._bindings if b["id"] == bid), None)
        action = self._actions.get(binding["action"]) if binding else None
        if action is not None and action.release is not None:
            try:
                action.release(action.full_params(binding["params"]))
            except Exception:
                logger.exception("Letting go of shortcut %s failed", binding["action"])

    def release_app_keys(self, key: Optional[int] = None):
        """Keys held inside Telescope come up: the one with this Qt key, or all of them (focus moved away)."""
        for bid, source in list(self._held.items()):
            if source != "app":
                continue
            binding = next((b for b in self._bindings if b["id"] == bid), None)
            if binding is None or key is None or split_keys(binding["keys"])[2] == key:
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
            return {"ok": False, "text": f"No action called {request.get('action')!r}. "
                                         "telescope --list-actions lists them."}
        params = request.get("params") if isinstance(request.get("params"), dict) else {}
        unknown = sorted(set(params) - {p.name for p in action.params})
        if unknown:
            return {"ok": False, "text": f"{action.id} has no setting {unknown[0]!r}."}
        try:
            said = action.run(action.full_params(params))
        except Exception:
            logger.exception("--action %s failed", action.id)
            return {"ok": False, "text": "That failed; Copy diagnostics has the details."}
        if said is None:
            return {"ok": False, "text": f"{action.label} can't be used right now (no phone, or the control is off)."}
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
            lambda pos, w=widget, a=action_id: self._control_menu(w, a, pos))
        self._with_menu.append(widget)

    def _control_menu(self, widget: QWidget, action_id: str, pos: QPoint):
        menu = QMenu(widget)
        add = QAction(_ADD_TEXT, menu)
        add.triggered.connect(lambda: self.add_binding(action_id, parent=widget.window()))
        menu.addAction(add)
        existing = [b for b in self._bindings if b["action"] == action_id]
        if existing:
            menu.addSeparator()
            for b in existing:
                edit = QAction(f"{native_keys(b['keys'])}: {self._describe(b)}".replace("&", "&&"), menu)
                edit.triggered.connect(lambda _=False, bid=b["id"]: self.edit_binding(bid, parent=widget.window()))
                menu.addAction(edit)
        menu.exec(widget.mapToGlobal(pos))
        menu.deleteLater()

    # ── Editing ───────────────────────────────────────────────────────────────

    def add_binding(self, action_id: Optional[str] = None, parent=None) -> Optional[dict]:
        editor = BindingEditor(self, {"id": new_binding_id(), "keys": "", "action": action_id or "",
                                      "params": {}, "global": True}, parent)
        if editor.exec() != QDialog.DialogCode.Accepted:
            return None
        binding = editor.binding()
        self.set_bindings(self._bindings + [binding])
        return binding

    def edit_binding(self, bid: str, parent=None) -> Optional[dict]:
        old = next((b for b in self._bindings if b["id"] == bid), None)
        if old is None:
            return None
        editor = BindingEditor(self, dict(old), parent)
        if editor.exec() != QDialog.DialogCode.Accepted:
            return None
        binding = editor.binding()
        self.set_bindings([binding if b["id"] == bid else b for b in self._bindings])
        return binding

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

    def create_menu_actions(self) -> list:
        action = QAction("Keyboard shortcuts…", None)
        action.triggered.connect(self.open_dialog)
        return [action]

    def open_dialog(self):
        if self._dlg is None:
            self._dlg = ShortcutsDialog(self)
        self._dlg.refresh()
        self._dlg.show()
        self._dlg.raise_()
        self._dlg.activateWindow()

    def _refresh_dialog(self):
        if self._dlg is not None and self._dlg.isVisible():
            self._dlg.refresh()

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
            app.removeEventFilter(self._filter)
            self._filter = None


class _KeyFilter(QObject):
    """Sees every key inside Telescope before the control with focus does, and runs the binding it belongs to."""

    def __init__(self, plugin: ShortcutsPlugin):
        super().__init__()
        self._plugin = plugin

    def _binding(self, event) -> Optional[dict]:
        keys = keys_from_event(event.key(), event.modifiers())
        if not keys:
            return None
        binding = self._plugin.binding_for_keys(keys)
        if binding is None:
            return None
        if QApplication.activeModalWidget() is not None:
            return None  # a dialog is asking something (the shortcut editor among them)
        focus = QApplication.focusWidget()
        if focus is not None and _inside_key_editor(focus):
            return None
        typing = isinstance(focus, _TEXT_INPUTS) or (isinstance(focus, QComboBox) and focus.isEditable())
        if typing and not has_command_modifier(binding["keys"]):
            return None  # a plain key belongs to the text box
        return binding

    def eventFilter(self, obj, event):
        kind = event.type()
        if kind not in (QEvent.Type.KeyPress, QEvent.Type.ShortcutOverride, QEvent.Type.KeyRelease):
            return False
        if not obj.isWidgetType():
            return False  # the same event comes through the window first; act on it once, at a widget
        if kind == QEvent.Type.KeyRelease:
            if not event.isAutoRepeat():
                self._plugin.release_app_keys(event.key())
            return False
        binding = self._binding(event)
        if binding is None:
            return False
        if kind == QEvent.Type.ShortcutOverride:
            event.accept()  # the key comes as a KeyPress then, instead of going to a menu shortcut
            return True
        self._plugin.press(binding["id"], "app", repeat=event.isAutoRepeat())
        return True


def _inside_key_editor(widget: QWidget) -> bool:
    while widget is not None:
        if isinstance(widget, QKeySequenceEdit):
            return True
        widget = widget.parentWidget()
    return False


# ── Dialogs ───────────────────────────────────────────────────────────────────

class _BindingRow(QWidget):
    """One line of the list: the key, what it does, and where it works (or why not, in amber)."""

    def __init__(self, keys: str, does: str, where: str, ok: bool):
        super().__init__()
        self.setAttribute(Qt.WidgetAttribute.WA_TransparentForMouseEvents)  # clicks select the item under it
        lay = QHBoxLayout(self)
        lay.setContentsMargins(2, 0, 2, 0)
        lay.setSpacing(12)
        key = QLabel(native_keys(keys))
        key.setObjectName("key_chip")
        key.setFixedWidth(ui_px(120))
        lay.addWidget(key)
        text = ElidingLabel(does)
        text.setMinimumWidth(ui_px(120))
        lay.addWidget(text, 1)
        status = QLabel(where)
        status.setObjectName("dim" if ok else "status_warn")
        status.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        lay.addWidget(status)
        self.setMinimumHeight(ui_px(26))


class ShortcutsDialog(QDialog):
    def __init__(self, plugin: ShortcutsPlugin):
        super().__init__()
        self._plugin = plugin
        self.setWindowTitle("Keyboard shortcuts")
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setMinimumSize(ui_px(620), ui_px(440))
        lay = dialog_layout(self)
        dialog_header(lay, "Keyboard shortcuts",
                      "Keys for Telescope's controls, with how much each one changes. "
                      "Right-click most controls to add one for it.")
        self._list = QListWidget()
        self._list.currentRowChanged.connect(lambda _r: self._on_selection())
        self._list.itemDoubleClicked.connect(lambda _i: self._edit())
        self._list.setContextMenuPolicy(Qt.ContextMenuPolicy.CustomContextMenu)
        self._list.customContextMenuRequested.connect(self._row_menu)
        lay.addWidget(self._list, 1)
        self._empty = wrapped_note("No shortcuts yet. Add one, or right-click a control in the main window.")
        lay.addWidget(self._empty)

        self._add_btn = QPushButton("Add…")
        set_ui_role(self._add_btn, "primary")
        self._add_btn.clicked.connect(lambda: self._plugin.add_binding(parent=self))
        self._edit_btn = QPushButton("Edit…")
        self._edit_btn.clicked.connect(self._edit)
        self._remove_btn = QPushButton("Remove")
        set_ui_role(self._remove_btn, "danger")
        self._remove_btn.clicked.connect(self._remove)
        lay.addLayout(button_row(self._add_btn, self._edit_btn, self._remove_btn))

        self._note = wrapped_note("")
        self._system_btn = action_button("Desktop shortcut settings…",
                                         tooltip="Where your desktop lets you change the keys of global shortcuts")
        self._system_btn.setFixedWidth(ui_px(220))
        self._system_btn.clicked.connect(self._open_system)
        note_row = QHBoxLayout()
        note_row.setContentsMargins(0, 0, 0, 0)
        note_row.setSpacing(12)
        note_row.addWidget(self._note, 1)
        note_row.addWidget(self._system_btn, 0, Qt.AlignmentFlag.AlignTop)
        lay.addLayout(note_row)

        self._notify_box = QCheckBox("Show a notification when a shortcut runs while Telescope is in the background")
        self._notify_box.toggled.connect(self._plugin.set_notify)
        lay.addWidget(self._notify_box)

        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.accept)
        dialog_buttons(lay, close_btn)

    def refresh(self):
        current = self._current_id()
        self._list.clear()
        for b in self._plugin.bindings:
            where, ok = self._plugin.where(b)
            item = QListWidgetItem()
            item.setData(Qt.ItemDataRole.UserRole, b["id"])
            row = _BindingRow(b["keys"], self._plugin.describe(b), where, ok)
            item.setSizeHint(QSize(0, row.minimumHeight() + ui_px(8)))  # the list's item padding comes on top
            self._list.addItem(item)
            self._list.setItemWidget(item, row)
            if b["id"] == current:
                self._list.setCurrentItem(item)
        has_any = self._list.count() > 0
        self._list.setVisible(has_any)
        self._empty.setVisible(not has_any)
        backend = self._plugin.backend
        if backend is not None:
            self._note.setText(backend.note())
        else:
            self._note.setText("Global shortcuts aren't available on this desktop, so these work inside Telescope. "
                               "For keys that work everywhere, bind a shortcut's Copy command line in your "
                               "desktop's keyboard settings.")
        self._system_btn.setVisible(IS_LINUX and (backend is None or backend.system_picks_keys))
        self._notify_box.blockSignals(True)
        self._notify_box.setChecked(self._plugin.notify)
        self._notify_box.blockSignals(False)
        self._on_selection()

    def _current_id(self) -> Optional[str]:
        item = self._list.currentItem()
        return item.data(Qt.ItemDataRole.UserRole) if item is not None else None

    def _on_selection(self):
        has = self._current_id() is not None
        self._edit_btn.setEnabled(has)
        self._remove_btn.setEnabled(has)

    def _edit(self):
        bid = self._current_id()
        if bid is not None:
            self._plugin.edit_binding(bid, parent=self)

    def _remove(self):
        bid = self._current_id()
        if bid is not None:
            self._plugin.remove_binding(bid)

    def _row_menu(self, pos: QPoint):
        item = self._list.itemAt(pos)
        if item is None:
            return
        bid = item.data(Qt.ItemDataRole.UserRole)
        binding = next((b for b in self._plugin.bindings if b["id"] == bid), None)
        if binding is None:
            return
        menu = QMenu(self)
        copy = QAction("Copy command line", menu)
        copy.triggered.connect(lambda: QGuiApplication.clipboard().setText(
            command_text(action_command(binding["action"], binding["params"]))))
        menu.addAction(copy)
        menu.exec(self._list.viewport().mapToGlobal(pos))
        menu.deleteLater()

    def _open_system(self):
        if not self._plugin.open_system_settings():
            self._note.setText("Couldn't open your desktop's shortcut settings. Look for Shortcuts in its "
                               "System Settings.")


class BindingEditor(QDialog):
    """Add or change one binding: the control, how much, the key, and whether it works everywhere."""

    def __init__(self, plugin: ShortcutsPlugin, binding: dict, parent=None):
        super().__init__(parent)
        self._plugin = plugin
        self._binding = dict(binding)
        self._param_widgets: dict = {}
        self.setWindowTitle("Keyboard shortcut")
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setModal(True)
        self.setMinimumWidth(ui_px(520))
        lay = dialog_layout(self)
        dialog_header(lay, "Keyboard shortcut", "Pick a control, what the key does to it, then press the key.")

        self._action_combo = NoScrollComboBox()
        self._fill_actions()
        self._action_combo.currentIndexChanged.connect(lambda _i: self._build_params())
        lay.addLayout(control_row("Control", self._action_combo, stretch=True))

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
        lay.addLayout(control_row("Key", self._keys_edit, stretch=True))

        self._global_box = QCheckBox("Also works while another app is in front")
        self._global_box.setChecked(binding.get("global", True))
        self._global_box.toggled.connect(lambda _on: self._check())
        lay.addLayout(control_row("", self._global_box, stretch=True))
        self._problem = wrapped_note("", kind="status_warn")
        lay.addWidget(self._problem)

        self._copy_btn = QPushButton("Copy command")
        self._copy_btn.setToolTip("A command line that does the same, for your desktop's own keyboard settings")
        self._copy_btn.clicked.connect(self._copy_command)
        cancel = QPushButton("Cancel")
        cancel.clicked.connect(self.reject)
        self._save_btn = QPushButton("Save")
        set_ui_role(self._save_btn, "primary")
        self._save_btn.setDefault(False)
        self._save_btn.setAutoDefault(False)
        self._save_btn.clicked.connect(self.accept)
        bar = dialog_buttons(lay, cancel, self._save_btn)
        bar.insertWidget(0, self._copy_btn)

        self._build_params(self._binding.get("params"))
        self._keys_edit.setFocus()

    def _fill_actions(self):
        combo = self._action_combo
        model = combo.model()
        group = None
        wanted = self._binding.get("action")
        for action in self._plugin.actions():
            if action.group != group:
                group = action.group
                combo.addItem(group.upper())
                header = model.item(combo.count() - 1)
                header.setFlags(Qt.ItemFlag.NoItemFlags)
            combo.addItem(action.label, action.id)
            if action.id == wanted:
                combo.setCurrentIndex(combo.count() - 1)
        if wanted and combo.findData(wanted) < 0:  # an action this version doesn't have: kept as it was
            combo.addItem(f"Unknown ({wanted})", wanted)
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
                    w.addItem(f"{value} (not there now)", value)
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

    def _keys(self) -> str:
        return normalize_keys(self._keys_edit.keySequence().toString(QKeySequence.SequenceFormat.PortableText))

    def _check(self):
        keys = self._keys()
        problem, blocking = "", True
        if self._action_combo.currentData() is None:
            problem = "Pick a control."
        elif not keys:
            problem = ("Press a key, with Ctrl, Alt, Shift or Meta if you like." if self._keys_edit.keySequence().isEmpty()
                       else "A modifier can't be a shortcut on its own. Add a key to it.")
        elif (other := next((b for b in self._plugin.bindings
                             if b["keys"] == keys and b["id"] != self._binding["id"]), None)) is not None:
            problem = f"{native_keys(keys)} already does \"{self._plugin.describe(other)}\". Pick another key."
        elif self._global_box.isChecked() and not has_command_modifier(keys):
            problem, blocking = (f"Everywhere, {native_keys(keys)} would stop typing it in other apps. "
                                 "Adding Ctrl, Alt or Meta is safer."), False
        self._problem.setText(problem)
        self._problem.setVisible(bool(problem))
        self._save_btn.setEnabled(not (problem and blocking))

    def _copy_command(self):
        action_id = self._action_combo.currentData()
        if action_id:
            QGuiApplication.clipboard().setText(command_text(action_command(action_id, self._params())))
            self._copy_btn.setText("Copied")
            QTimer.singleShot(1500, lambda: self._copy_btn.setText("Copy command"))

    def binding(self) -> dict:
        return {"id": self._binding["id"], "keys": self._keys(), "action": self._action_combo.currentData(),
                "params": self._params(), "global": self._global_box.isChecked()}
