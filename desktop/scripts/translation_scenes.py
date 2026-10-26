"""The screens the translation workbench can render: which scenes there are and how each is built.

Only the Qt worker (translation_preview.py --render) builds them. Everything above SCENES imports no Qt and no
telescope module, so the server and the browser can list the sections without starting the app. The builders import
telescope lazily, after the worker has instrumented translation calls.
"""

import contextlib
import json
import os
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
from telescope import i18n  # noqa: E402


def key(ctx, text):
    return json.dumps([ctx, text], ensure_ascii=False)


def parts_of(text):
    """The translated texts a shown string is made of: itself, those joined into it, or none for plain text."""
    return list(getattr(text, "parts", [])) if isinstance(text, Joined) else [text] if isinstance(text, Tagged) else []


class Tagged(i18n.Shown):
    def __new__(cls, text, source, ctx, values):
        shown = super().__new__(cls, text, source)
        shown.identity = key(ctx, source)
        shown.values = {k: str(v) for k, v in values.items()}
        return shown

    def __add__(self, other):
        return Joined(str(self) + str(other), parts_of(self) + parts_of(other))

    def __radd__(self, other):
        return Joined(str(other) + str(self), parts_of(other) + parts_of(self))


class Joined(str):
    """Translated texts put together with +, which is how a notification or banner often gets its sentence."""

    def __new__(cls, text, parts):
        joined = super().__new__(cls, text)
        joined.parts = parts
        return joined

    def __add__(self, other):
        return Joined(str(self) + str(other), self.parts + parts_of(other))

    def __radd__(self, other):
        return Joined(str(other) + str(self), parts_of(other) + self.parts)


def remember(target, slot, text):
    """Note on a widget, action or item which translated text it shows in a slot (text, tooltip, ...)."""
    records = getattr(target, "_preview_texts", {})
    if parts_of(text):
        records[slot] = text
    else:
        records.pop(slot, None)
    target._preview_texts = records


@dataclass(frozen=True)
class Scene:
    id: str
    section: str
    title: str
    build: Callable
    width: int = 0  # the width text-fit measures against, 0 for none


SECTIONS = {}
SCENES = {}


def section(id, title, description):
    SECTIONS[id] = {"id": id, "title": title, "description": description, "scenes": []}


def sections():
    """The sections that have scenes, each with its scene ids in order."""
    # The main window comes first, then its Phone card up close
    up_close = ("cool", "warm", "hot")
    return [{**s, "scenes": sorted(s["scenes"], key=lambda id: id in up_close)} for s in SECTIONS.values() if s["scenes"]]


def scene(id, section_id, title, width=0):
    def register(build):
        assert id not in SCENES, f"two scenes are called {id}"
        SCENES[id] = Scene(id, section_id, title, build, width)
        SECTIONS[section_id]["scenes"].append(id)
        return build
    return register


# ── Quiet system ─────────────────────────────────────────────────────────────

class _Held:
    """Stands in for what reaches the network or the system: does nothing."""

    def __init__(self, *_args, **_kwargs):
        pass

    def __getattr__(self, _name):
        return lambda *_a, **_k: None

    def lookup(self, _pid):
        return []


def quiet_sign(plugin):
    """The On Air sign with a fixed address and no server or certificate of its own."""
    from scripts import text_fit
    text_fit.quiet_sign(plugin)


class _MicBackend:
    pick_name = "Microphone"

    def __getattr__(self, _name):
        return lambda *_a, **_k: None

    def problem(self):
        return None

    def prepare(self):
        return ""


def use_scratch_profile(stack):
    """Settings and plugin state go to a throwaway folder, never to the real profile."""
    home = stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-translation-profile-"))
    os.environ["XDG_CONFIG_HOME"] = os.path.join(home, "xdg")
    os.environ["XDG_DATA_HOME"] = os.path.join(home, "xdg-data")
    os.environ["APPDATA"] = os.path.join(home, "appdata")
    os.environ["LOCALAPPDATA"] = os.path.join(home, "localappdata")


def quiet_system(stack, tray=False):
    """The window as on a machine with no phone, no virtual camera set up and nothing to check online."""
    from unittest import mock
    import telescope.app as app_module
    import telescope.plugins.connection as connection
    import telescope.plugins.setup as setup
    from telescope import h264_reader
    from telescope.plugins import on_air as on_air_plugin
    from telescope.plugins.onboarding import OnboardingPlugin
    from telescope.plugins.updates import UpdatesPlugin
    for target, name, value in (
        (app_module.QSystemTrayIcon, "isSystemTrayAvailable", lambda: tray),
        (app_module, "save_config", lambda _cfg: True),
        (connection, "LanDiscovery", _Held),
        (connection, "stop_adb_server", lambda: None),
        (h264_reader, "hardware_device", lambda: None),  # looked for on a thread, so the picture would depend on timing
        (setup, "uc_registered_name", lambda: None),
        (setup, "uc_in_app_folder", lambda: False),
        (connection.ConnectionPlugin, "_spawn_resolve", lambda self, *a: None),
        (connection.ConnectionPlugin, "_check_status", lambda self, *a, **k: None),
        (connection.ConnectionPlugin, "ensure_virtual_camera", lambda self, interactive=True: True),
        (OnboardingPlugin, "check_virtual_camera", lambda self: None),
        (on_air_plugin, "ensure_certificate", lambda _folder, _hosts: ("", "")),
        (UpdatesPlugin, "_maybe_auto_check", lambda self: None),
    ):
        if hasattr(target, name):
            stack.enter_context(mock.patch.object(target, name, value))
    # This computer's virtual cameras and drivers stay out of the picture unless the scene set up its own;
    # by default v4l2loopback is set up and working, as on most Linux machines that use Telescope
    if not getattr(stack, "faked_linux", False):
        from scripts.translation_scenes_setup import fake_linux
        from telescope.platform.linux import V4L2_OBS_DEV, V4L2_PHONE_DEV
        fake_linux(stack, loaded=True, nodes=(V4L2_PHONE_DEV, V4L2_OBS_DEV))
    if getattr(setup.unitycapture_dir, "__module__", "") == "telescope.platform.windows":
        stack.enter_context(mock.patch.object(setup, "unitycapture_dir",
                                              lambda: Path(tempfile.gettempdir()) / "telescope-preview" / "no-unitycapture"))


def main_window(stack, tray=False, phone=True):
    """The real window with every built-in plugin registered, like text_fit builds it."""
    from PyQt6.QtWidgets import QApplication
    from telescope.app import TelescopeWindow
    from telescope.plugins import builtin_plugins
    use_scratch_profile(stack)
    quiet_system(stack, tray)
    win = TelescopeWindow()
    for plugin in builtin_plugins():
        if plugin.name == "wait_screen":
            plugin._screen, plugin._watch_cls, plugin._extra_screen = _Held(), _Held, _Held
        elif plugin.name == "microphone":
            plugin._backend = _MicBackend()
        elif plugin.name == "shortcuts":
            plugin._backend_factory = lambda: None
        elif plugin.name == "on_air":
            quiet_sign(plugin)
        win.register_plugin(plugin)
    win.apply_saved_config()
    if phone:
        connection = next(p for p in win._plugins if p.name == "connection")
        connection.set_config({**connection.get_config(), "selected_phone": "phone-1", "phones": [
            {"id": "phone-1", "name": "Pixel 8", "token": "token", "ips": [], "active_ip": None}]})
    QApplication.processEvents()
    stack.callback(close_window, win)
    return win


def close_window(win):
    from PyQt6 import sip
    from PyQt6.QtCore import QCoreApplication, QEvent, QTimer
    from PyQt6.QtWidgets import QApplication
    if win._settings_win is not None:
        win._settings_win.close()
    win._shut_down()
    for plugin in win._plugins:
        for value in vars(plugin).values():
            if isinstance(value, QTimer):
                value.stop()
    win._session = None
    win.deleteLater()
    QCoreApplication.sendPostedEvents(None, QEvent.Type.DeferredDelete)
    QApplication.processEvents()
    if not sip.isdeleted(win):
        sip.delete(win)


def grow(widget, width, height):
    """widget at width, as tall as its scrolling parts need so every text is laid out and drawn."""
    from scripts import text_fit
    from telescope.widgets.common import ui_px
    widget.show()
    text_fit._grow_to_content(widget, ui_px(width), ui_px(height))


# ── Phone card ───────────────────────────────────────────────────────────────

section("main", "Main window", "The cards, banners and layouts of the main window, including the Phone card")


class PhoneHost:
    def __init__(self):
        self.issues = []

    def schedule_save(self):
        pass

    def is_streaming(self):
        return True

    def stop_stream(self, _sid):
        pass

    def send_notification(self, *_args):
        pass

    def show_issue(self, _key, issue):
        self.issues.append(issue)


def phone(stack, state):
    from PyQt6.QtWidgets import QApplication, QVBoxLayout, QWidget
    from telescope.plugin import EventBus
    from telescope.plugins.monitoring import MonitoringPlugin
    from telescope.widgets.banner import Banner
    host, bus = PhoneHost(), EventBus()
    plugin = MonitoringPlugin()
    plugin.setup(host, bus)
    stack.callback(plugin._timer.stop)
    top = QWidget()
    top.setObjectName("translation_preview")
    layout = QVBoxLayout(top)
    layout.setContentsMargins(16, 16, 16, 16)
    panel = plugin.create_panel()
    layout.addWidget(panel)
    panel.setGraphicsEffect(None)
    panel.setEnabled(True)
    if state == "hot":
        plugin._temp_stop.setChecked(True)
        plugin._on_state({"battery": 64, "charging": False, "battery_temp_c": 48.2})
    else:
        plugin._update_display(64, state == "cool", 29.4 if state == "cool" else 42.1)
    for issue in host.issues:
        layout.addWidget(Banner(issue))
    top.resize(620, 600)
    top.setMaximumWidth(1400)
    top.show()
    QApplication.processEvents()
    top.resize(620, top.sizeHint().height())
    QApplication.processEvents()
    return top


@scene("cool", "main", "Phone · cool and charging", width=620)
def phone_cool(stack):
    return phone(stack, "cool")


@scene("warm", "main", "Phone · warm", width=620)
def phone_warm(stack):
    return phone(stack, "warm")


@scene("hot", "main", "Phone · stopped for heat", width=620)
def phone_hot(stack):
    return phone(stack, "hot")


# ── Main window ──────────────────────────────────────────────────────────────



def window_at(stack, width):
    import telescope.app as app_module
    win = main_window(stack)
    grow(win, getattr(app_module, width), 900)
    return win


@scene("main-empty", "main", "Main window · before any phone is paired")
def main_empty(stack):
    import telescope.app as app_module
    win = main_window(stack, phone=False)
    grow(win, app_module._WIDTH_THREE_COL, 900)
    return win


@scene("main-wide", "main", "Main window · three columns")
def main_wide(stack):
    return window_at(stack, "_WIDTH_THREE_COL")


@scene("main-medium", "main", "Main window · two columns")
def main_medium(stack):
    return window_at(stack, "_WIDTH_TWO_COL")


@scene("main-narrow", "main", "Main window · narrow")
def main_narrow(stack):
    win = main_window(stack)
    grow(win, 560, 900)
    return win


# ── Settings ─────────────────────────────────────────────────────────────────

section("settings", "Settings", "Every page of the Settings window")


def settings_page(stack, tab):
    from PyQt6.QtWidgets import QApplication
    from telescope.widgets.settings_window import _SIZE
    win = main_window(stack)
    win.resize(1380, 900)
    win.open_settings()
    settings = win._settings_win
    settings.show_tab(tab)
    QApplication.processEvents()
    grow(settings, _SIZE[0], _SIZE[1])
    return settings


SETTINGS_PAGES = (("general", "General"), ("streaming", "Streaming"), ("video", "Video"), ("audio", "Audio"),
                  ("devices", "Virtual camera & phone"), ("shortcuts", "Keyboard shortcuts"), ("on_air", "On Air"),
                  ("plugins", "Plugins"),
                  ("about", "Updates & About"))


def _settings_scene(tab, title):
    scene(f"settings-{tab}", "settings", f"Settings · {title}")(lambda stack: settings_page(stack, tab))


for _tab, _title in SETTINGS_PAGES:
    _settings_scene(_tab, _title)


# ── Dialogs and menus ────────────────────────────────────────────────────────

section("dialogs", "Dialogs and menus", "Pairing, shortcuts, confirmations, the tray and preset menus")


def hold_dialogs(stack):
    """Modal dialogs and message boxes return at once and are kept for the picture; returns the list they're added to."""
    from unittest import mock
    from PyQt6.QtWidgets import QDialog, QInputDialog, QMessageBox
    held = []

    def keep(dialog, *_args):
        held.append(dialog)
        return 0

    def asking(icon):
        def ask(parent, title, text, *_args, **_kwargs):
            box = QMessageBox(icon, title, text, parent=parent)
            remember(box, "window title", title)
            remember(box, "text", text)
            held.append(box)
            return QMessageBox.StandardButton.No
        return staticmethod(ask)

    def get_text(parent, title, label, *_args, **_kwargs):
        dialog = QInputDialog(parent)
        dialog.setWindowTitle(title)
        dialog.setLabelText(label)
        held.append(dialog)
        return "", False

    stack.enter_context(mock.patch.object(QDialog, "exec", keep))
    stack.enter_context(mock.patch.object(QMessageBox, "exec", keep))
    for name, icon in (("question", QMessageBox.Icon.Question), ("warning", QMessageBox.Icon.Warning),
                       ("information", QMessageBox.Icon.Information), ("critical", QMessageBox.Icon.Critical)):
        stack.enter_context(mock.patch.object(QMessageBox, name, asking(icon)))
    stack.enter_context(mock.patch.object(QInputDialog, "getText", staticmethod(get_text)))
    return held


def shown(stack, widget):
    """widget, shown at the size its content needs, and closed with the scene."""
    from PyQt6.QtWidgets import QApplication
    if widget.minimumWidth() < 360 and hasattr(widget, "setMinimumWidth") and widget.isWindow():
        widget.setMinimumWidth(360)
    widget.show()
    widget.adjustSize()
    QApplication.processEvents()
    stack.callback(widget.close)
    return widget


class FakePhones:
    computer_name = "Studio PC"
    remembered = [("browser:x", "Chrome on Android", "Browser")]

    def __init__(self):
        from telescope.phones import Phone
        self.phones = [Phone("id-1", "Pixel 8", "token")]

    def phone(self, pid):
        return self.phones[0] if pid == "id-1" else None

    def open_add_phone(self):
        pass


def add_phone(stack, adb):
    from types import SimpleNamespace
    from unittest import mock
    import telescope.plugins.connection as connection

    class Server:
        offer = None

        def __init__(self, **_kwargs):
            pass

        def start(self):
            return SimpleNamespace(candidates=["192.168.1.20"], payload="telescope://pair/example")

        def stop(self):
            pass

    use_scratch_profile(stack)
    for name, value in (("adb_available", lambda: adb), ("adb_device_states", lambda: []), ("PairingServer", Server)):
        stack.enter_context(mock.patch.object(connection, name, value))
    return shown(stack, connection.AddPhoneDialog(None, "computer-id", "Studio PC", lambda *_args: None))


@scene("dialog-add-phone", "dialogs", "Add phone · adb missing")
def add_phone_without_adb(stack):
    return add_phone(stack, False)


@scene("dialog-add-phone-usb", "dialogs", "Add phone · USB ready")
def add_phone_with_adb(stack):
    return add_phone(stack, True)


@scene("dialog-add-phone-done", "dialogs", "Add phone · paired")
def add_phone_done(stack):
    from types import SimpleNamespace
    dialog = add_phone(stack, True)
    dialog._on_paired_signal(SimpleNamespace(name="Pixel 8"))
    return shown(stack, dialog)


@scene("dialog-phones", "dialogs", "Your phones")
def phones_dialog(stack):
    import telescope.plugins.connection as connection
    return shown(stack, connection.PhonesDialog(FakePhones()))


@scene("dialog-phone-remove", "dialogs", "Your phones · remove a phone")
def phone_remove(stack):
    import telescope.plugins.connection as connection
    held = hold_dialogs(stack)
    owner = connection.PhonesDialog(FakePhones())  # the question is its child, so it has to outlive the call
    stack.callback(owner.deleteLater)
    owner._remove("id-1")
    return shown(stack, held[-1])


@scene("dialog-phone-rename", "dialogs", "Your phones · rename a phone")
def phone_rename(stack):
    import telescope.plugins.connection as connection
    held = hold_dialogs(stack)
    owner = connection.PhonesDialog(FakePhones())
    stack.callback(owner.deleteLater)
    owner._rename("id-1")
    return shown(stack, held[-1])


@scene("dialog-shortcut", "dialogs", "Keyboard shortcut editor")
def shortcut_editor(stack):
    from telescope.plugins.shortcuts import BindingEditor, new_binding_id
    win = main_window(stack)
    plugin = next(p for p in win._plugins if p.name == "shortcuts")
    return shown(stack, BindingEditor(plugin, {"id": new_binding_id(), "keys": "", "action": "", "params": {}, "global": True}))


@scene("dialog-vaapi", "dialogs", "Set up VA-API")
def vaapi(stack):
    from unittest import mock
    from telescope.widgets import vaapi_setup
    use_scratch_profile(stack)
    stack.enter_context(mock.patch.object(vaapi_setup.VaapiSetupDialog, "check_again", lambda self: None))
    return shown(stack, vaapi_setup.VaapiSetupDialog(None, lambda: True))


@scene("dialog-adb-licence", "dialogs", "Get adb · licence question")
def adb_licence(stack):
    from telescope.widgets import adb
    held = hold_dialogs(stack)
    adb.confirm(None)
    return shown(stack, held[-1])


@scene("dialog-vb-cable", "dialogs", "Install VB-Cable question")
def vb_cable(stack):
    from types import SimpleNamespace
    from PyQt6.QtWidgets import QPushButton
    from telescope.plugins.microphone import MicrophonePlugin
    held = hold_dialogs(stack)
    button = QPushButton()
    stack.callback(button.deleteLater)
    MicrophonePlugin._confirm_install(SimpleNamespace(_action_btn=button))
    return shown(stack, held[-1])


@scene("dialog-already-running", "dialogs", "Telescope is already running")
def already_running(stack):
    from telescope import single_instance
    held = hold_dialogs(stack)
    single_instance._other_user_notice()
    return shown(stack, held[-1])


@scene("dialog-lock-held", "dialogs", "Telescope can't start")
def lock_held(stack):
    from telescope import single_instance
    held = hold_dialogs(stack)
    single_instance._port_taken_notice()
    return shown(stack, held[-1])


def preset_plugin(stack):
    from telescope.plugins.presets import PresetsPlugin
    from PyQt6.QtWidgets import QWidget
    win = main_window(stack)
    plugin = next(p for p in win._plugins if p.name == "presets")
    plugin._presets = [{"name": "Desk"}, {"name": "Stream"}] if hasattr(plugin, "_presets") else []
    return win, plugin


@scene("menu-presets", "dialogs", "Presets menu")
def presets_menu(stack):
    from PyQt6.QtCore import QPoint
    win, plugin = preset_plugin(stack)
    menu = plugin.build_menu(win)
    menu.popup(QPoint(20, 20))
    stack.callback(menu.close)
    return shown(stack, menu)


@scene("dialog-preset-save", "dialogs", "Presets · save as")
def preset_save(stack):
    held = hold_dialogs(stack)
    win, plugin = preset_plugin(stack)
    plugin._ask_save()
    return shown(stack, held[-1])


@scene("dialog-preset-replace", "dialogs", "Presets · replace question")
def preset_replace(stack):
    held = hold_dialogs(stack)
    win, plugin = preset_plugin(stack)
    plugin._may_replace(plugin.names()[0] if plugin.names() else "Desk")
    return shown(stack, held[-1])


@scene("menu-tray", "dialogs", "Tray menu")
def tray_menu(stack):
    from PyQt6.QtCore import QPoint
    win = main_window(stack, tray=True)
    menu = win._tray.contextMenu()
    menu.popup(QPoint(20, 20))
    stack.callback(menu.close)
    return shown(stack, menu)


# ── Notifications ────────────────────────────────────────────────────────────

section("states", "Status lines and problems", "The cards and banners when something is wrong, or in the middle of doing something")
section("notifications", "Notifications", "What pops up on the desktop when Telescope has something to say")


def capture_notifications(stack):
    """Desktop notifications are kept instead of sent; returns the list of (title, body, urgent) they're added to."""
    from unittest import mock
    import telescope.app as app_module
    sent = []
    stack.enter_context(mock.patch.object(app_module.TelescopeWindow, "send_notification",
                                          lambda self, title, body, urgent=True: sent.append((title, body, urgent))))
    return sent


def bubble(stack, notices):
    """notices as a desktop shows them: a rounded card with the app's name, a title and the text."""
    from PyQt6.QtCore import Qt
    from PyQt6.QtWidgets import QApplication, QFrame, QHBoxLayout, QLabel, QVBoxLayout, QWidget
    from telescope.widgets.common import create_app_icon
    top = QWidget()
    outer = QVBoxLayout(top)
    outer.setContentsMargins(24, 24, 24, 24)
    outer.setSpacing(10)
    for title, body, _urgent in notices:
        card = QFrame()
        card.setObjectName("notification")
        card.setStyleSheet("QFrame#notification { background: #2b2b30; border: 1px solid #45454d; border-radius: 12px; }"
                           "QFrame#notification QLabel { background: transparent; color: #f2f2f4; }")
        row = QHBoxLayout(card)
        row.setContentsMargins(14, 12, 14, 12)
        row.setSpacing(12)
        icon = QLabel()
        icon.setPixmap(create_app_icon(36).pixmap(36, 36))
        icon.setAlignment(Qt.AlignmentFlag.AlignTop)
        row.addWidget(icon)
        column = QVBoxLayout()
        column.setSpacing(2)
        heading = QLabel(title)
        heading.setStyleSheet("font-weight: 600;")
        text = QLabel(body)
        text.setWordWrap(True)
        column.addWidget(heading)
        column.addWidget(text)
        row.addLayout(column, 1)
        outer.addWidget(card)
    top.setFixedWidth(400)
    top.show()
    top.adjustSize()
    QApplication.processEvents()
    stack.callback(top.close)
    return top


# ── Settings search ──────────────────────────────────────────────────────────

def search_scene(stack, text_key):
    """The Settings search box, or the shortcut editor's control search, with a word of this keyword text typed in."""
    from PyQt6.QtWidgets import QApplication, QWidget
    win = main_window(stack)
    win.open_settings()
    settings = win._settings_win
    owners = [w for w in settings.findChildren(QWidget) if getattr(w, "_preview_texts", {}).get("search keywords")
              and w._preview_texts["search keywords"].identity == text_key]
    owners += [s for _tab, _name, mine in settings._tabs for s in mine
               if getattr(getattr(s, "keywords", None), "identity", None) == text_key]
    for owner in owners:
        text = getattr(owner, "_preview_texts", {}).get("search keywords") or owner.keywords
        grow(settings, 940, 680)
        for word in dict.fromkeys(str(text).split()):
            settings._search.setText(word)
            QApplication.processEvents()
            row = next((i for i, hit in enumerate(settings._hits)
                        if str(text) in (hit.keywords, hit.card_keywords, hit.tab_keywords)), None)
            if row is not None:
                settings._results.setCurrentRow(row)
                remember(settings._results.item(row), "search keywords", text)
                return settings
    plugin = next(p for p in win._plugins if p.name == "shortcuts")
    for action in plugin._actions.values():
        if getattr(action.keywords, "identity", None) == text_key:
            from telescope.plugins.shortcuts import BindingEditor, new_binding_id
            settings.close()
            editor = BindingEditor(plugin, {"id": new_binding_id(), "keys": "", "action": "", "params": {}, "global": True})
            editor._search.setText(str(action.keywords).split()[0])
            remember(editor._action_combo, "editor search keywords", action.keywords)
            return shown(stack, editor)
    grow(settings, 940, 680)
    return settings


def register_search(entries):
    """One scene per search keyword text: the box with one of its words typed in."""
    for entry in entries:
        if entry.ctx == "search":
            identity = key(entry.ctx, entry.msgid)
            title = "Search: " + " ".join(entry.msgid.split()[:6]) + ("…" if len(entry.msgid.split()) > 6 else "")
            scene(f"search-{len(SECTIONS['search']['scenes']) + 1}", "search", title)(
                lambda stack, identity=identity: search_scene(stack, identity))


# ── Texts that need a situation ──────────────────────────────────────────────

section("search", "Settings search", "The words people can type into the Settings search box")
section("messages", "Other texts",
        "Texts no screen shows yet, with where the app uses them and sample values filled in")

AREAS = (
    ("connection", "Connection and phones", ("plugins/connection.py", "phones.py", "sources.py", "pairing.py", "stream_flow.py",
                                             "stream.py", "udp_reader.py", "session_client.py")),
    ("setup", "Setup and virtual camera", ("plugins/setup.py", "plugins/onboarding.py", "platform/", "vcam.py", "pyav_vaapi.py",
                                           "widgets/vaapi_setup.py", "widgets/adb.py", "widgets/vision", "vision_pack.py",
                                           "single_instance.py")),
    ("window", "Main window and plugins", ("app.py", "plugin.py", "plugin_loader.py", "widgets/plugins_panel.py", "widgets/")),
    ("camera", "Camera, video and framing", ("plugins/camera_control.py", "plugins/framing.py", "plugins/transforms.py",
                                             "plugins/preview.py", "plugins/stream_output.py", "plugins/background.py",
                                             "plugins/colour_grading.py", "colour.py", "plugins/photos.py", "stills.py",
                                             "plugins/presets.py", "plugins/wait_screen.py", "plugins/browser_camera.py",
                                             "browser_server.py")),
    ("audio", "Microphone and shortcuts", ("plugins/microphone.py", "audio.py", "plugins/shortcuts.py", "shortcuts.py")),
    ("updates", "Updates, startup and monitoring", ("plugins/updates.py", "updates.py", "plugins/startup.py",
                                                    "plugins/monitoring.py", "splash.py", "i18n.py")),
)
PAGE = 30
SAMPLES = {"name": "Pixel 8", "phone": "Pixel 8", "computer": "Studio PC", "version": "3.0.0", "error": "Connection refused",
           "message": "Something went wrong", "file": "camera.apk", "path": "/home/me/Pictures", "folder": "/home/me/Pictures",
           "url": "https://example.com", "plugin": "Example plugin", "plugins": "Example plugin", "ip": "192.168.1.20",
           "device": "/dev/video10", "label": "Camera", "action": "Start", "source": "Pixel 8", "camera": "Back camera",
           "shortcut": "Ctrl+Shift+M", "keys": "Ctrl+Shift+M"}
NUMBERS = {"n", "level", "temp", "fps", "size", "mb", "percent", "seconds", "code", "port", "amount", "value", "v", "x", "y",
           "q", "time", "stops", "needs", "have", "wanted", "want", "quality"}


def sample_values(msgid):
    return {name: "42" if name in NUMBERS else SAMPLES.get(name, "example") for name in i18n.fields(msgid)}


def area_of(entry):
    where = entry.locations[0].split(":")[0].removeprefix("telescope/")
    if entry.ctx == "search":
        return "search"
    for id, _title, prefixes in AREAS:
        if any(where.startswith(p) if p.endswith("/") else where == p for p in prefixes):
            return id
    return "window"


def gallery(stack, rows):
    """A column of texts, each in a card saying where the app shows it, filled with sample values."""
    from PyQt6.QtCore import Qt
    from PyQt6.QtWidgets import QApplication, QFrame, QLabel, QVBoxLayout, QWidget
    top = QWidget()
    outer = QVBoxLayout(top)
    outer.setContentsMargins(16, 16, 16, 16)
    outer.setSpacing(8)
    for ctx, msgid, where in rows:
        card = QFrame()
        card.setObjectName("card")
        column = QVBoxLayout(card)
        column.setContentsMargins(14, 10, 14, 10)
        column.setSpacing(2)
        note = QLabel(where)
        note.setObjectName("muted")
        note.setWordWrap(True)
        column.addWidget(note)
        label = QLabel(i18n.tr(msgid, ctx=ctx, **sample_values(msgid)))
        label.setWordWrap(True)
        label.setTextInteractionFlags(Qt.TextInteractionFlag.NoTextInteraction)
        column.addWidget(label)
        outer.addWidget(card)
    top.resize(620, 100)
    top.show()
    QApplication.processEvents()
    top.resize(620, top.sizeHint().height())
    QApplication.processEvents()
    return top


def where_shown(entry):
    """What to tell a translator about a text no screen shows: the developer's note, else which file uses it."""
    place = entry.locations[0].split(":")[0].removeprefix("telescope/").removesuffix(".py").replace("plugins/", "").replace("_", " ")
    note = " ".join(entry.comments)
    return f"Shown by {place}" + (f": {note}" if note else "")


def register_galleries(entries, screens=()):
    """One scene per page of up to PAGE of each area's texts that no real screen shows, in source order. The
    messages scenes are rebuilt each time, so this can be called again with a new set of screens."""
    for id in [i for i in SCENES if i.startswith("messages-")]:
        del SCENES[id]
    SECTIONS["messages"]["scenes"].clear()
    titles = {id: title for id, title, _ in AREAS} | {"search": "Settings search keywords"}
    by_area = {}
    for entry in sorted(entries, key=lambda e: (e.locations[0].split(":")[0], int(e.locations[0].split(":")[1]))):
        if not entry.plural and key(entry.ctx, entry.msgid) not in screens:
            by_area.setdefault(area_of(entry), []).append((entry.ctx, entry.msgid, where_shown(entry)))
    for id in titles:
        rows = by_area.get(id, [])
        pages = [rows[i:i + PAGE] for i in range(0, len(rows), PAGE)]
        for number, page in enumerate(pages, 1):
            title = titles[id] + (f" · part {number} of {len(pages)}" if len(pages) > 1 else "")
            scene(f"messages-{id}-{number}", "messages", title)(lambda stack, page=page: gallery(stack, page))


# The scenes of each area live beside this file, so each can grow without touching the others.
from scripts import (translation_scenes_audio, translation_scenes_camera, translation_scenes_connection,  # noqa: E402,F401
                     translation_scenes_setup, translation_scenes_updates, translation_scenes_window)
