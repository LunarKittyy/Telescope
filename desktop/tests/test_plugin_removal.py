"""Plugins really are plugins: the app starts, streams and quits with any one of them left out, Connection aside."""

import logging
from types import SimpleNamespace

import pytest
from PyQt6.QtCore import QCoreApplication, QEvent, QObject, QTimer
from PyQt6.QtWidgets import QLabel, QWidget

import telescope.app as app_module
import telescope.plugins.connection as connection_module
from telescope.config import load_config
import telescope.plugins.setup as setup_plugin
import telescope.updates as updates_module
from telescope import packets
from telescope.plugin import TelescopePlugin
from telescope.plugins import builtin_plugins
from telescope.plugins.onboarding import OnboardingPlugin
from telescope.plugins.updates import UpdatesPlugin

REQUIRED = {"connection"}  # the window asks it which phone is picked and how to reach it
OPTIONAL = [p.name for p in builtin_plugins() if p.name not in REQUIRED]


class _Signal:
    def __init__(self):
        self.callbacks = []

    def connect(self, callback):
        self.callbacks.append(callback)

    def disconnect(self, callback):
        self.callbacks.remove(callback)

    def emit(self, *args):
        for callback in list(self.callbacks):
            callback(*args)


class _Worker:
    """Stands in for StreamWorker: no thread, no virtual camera."""
    made = []
    parked = False
    camera_size = None

    def __init__(self, **kwargs):
        self.kwargs = kwargs
        self.status, self.reconnected, self.vcam_opened = _Signal(), _Signal(), _Signal()
        self.started = self.stopped = False
        self.packets = packets.Tap()
        _Worker.made.append(self)

    def start(self):
        self.started = True

    def request_stop(self):
        self.stopped = True

    def wait(self, _ms):
        return True

    def set_pipeline(self, pipeline):
        self.kwargs["frame_pipeline"] = pipeline

    def latest_frame(self):
        return None

    def update_output(self, **_kwargs):
        pass

    def settle(self):
        pass

    def set_parked(self, parked):
        self.parked = parked


class _Ctrl:
    base, auth = "test://cam", None

    def __init__(self):
        self.sent = []

    def send(self, **params):
        self.sent.append(params)

    def get_state(self):
        return {}

    def close(self):
        pass


class _TestCamera(TelescopePlugin):
    """A plugin of the test's own offering a StreamSource, so streaming needs neither a phone nor Browser camera."""
    name = "test_camera"

    def setup(self, host, bus):
        self.ctrl = _Ctrl()
        plugin = self
        host.add_stream_source(SimpleNamespace(
            id="test:cam", name="Test camera", url="test://cam", prepare=lambda interactive: True,
            open_reader=lambda: None, control_client=lambda: plugin.ctrl, fps=lambda: 30,
        ))


class _Held:
    def __init__(self, *_args, **_kwargs):
        pass

    def show(self, *_args):
        pass

    def stop(self):
        pass

    def start(self):
        pass

    def set_watched(self, _watched):
        pass

    def lookup(self, _pid):
        return []


class _MicBackend:
    pick_name = "Test mic"

    def problem(self):
        return None

    def prepare(self):
        return ""

    def teardown(self):
        pass


@pytest.fixture
def build(qapp, config_home, monkeypatch):
    """Build a window with the built-in plugins but left_out, with the bits that reach the system stood in for."""
    monkeypatch.setattr(app_module.QSystemTrayIcon, "isSystemTrayAvailable", lambda: False)
    monkeypatch.setattr(app_module, "StreamWorker", _Worker)
    monkeypatch.setattr(connection_module, "LanDiscovery", lambda *a, **k: _Held())
    monkeypatch.setattr(connection_module, "stop_adb_server", lambda: None)
    monkeypatch.setattr(setup_plugin, "uc_registered_name", lambda: None, raising=False)  # Setup's tabs scan the registry on a thread when shown
    monkeypatch.setattr(setup_plugin, "uc_in_app_folder", lambda: False, raising=False)
    monkeypatch.setattr(connection_module.ConnectionPlugin, "_spawn_resolve", lambda self, *a: None)
    monkeypatch.setattr(connection_module.ConnectionPlugin, "ensure_virtual_camera", lambda self, interactive=True: True)
    monkeypatch.setattr(OnboardingPlugin, "check_virtual_camera", lambda self: None)
    monkeypatch.setattr(UpdatesPlugin, "_maybe_auto_check", lambda self: None)
    _Worker.made = []
    windows = []

    def make(left_out):
        window = app_module.TelescopeWindow()
        windows.append(window)
        plugins = [p for p in builtin_plugins() if p.name != left_out] + [_TestCamera()]
        for plugin in plugins:
            if plugin.name == "wait_screen":
                plugin._screen, plugin._watch_cls, plugin._extra_screen = _Held(), _Held, _Held
            elif plugin.name == "microphone":
                plugin._backend = _MicBackend()
            elif plugin.name == "shortcuts":
                plugin._backend_factory = lambda: None
            window.register_plugin(plugin)
        window.apply_saved_config()
        qapp.processEvents()
        return window

    yield make
    for window in windows:
        window._shut_down()
        for plugin in window._plugins:
            for value in vars(plugin).values():
                if isinstance(value, QTimer):
                    value.stop()  # a plugin's timer mustn't fire into a later test
        window._session = None
        window.deleteLater()
    QCoreApplication.sendPostedEvents(None, QEvent.Type.DeferredDelete)


@pytest.mark.parametrize("left_out", [None, *OPTIONAL])
def test_the_app_starts_streams_and_quits_without_any_one_optional_plugin(build, qapp, caplog, left_out):
    caplog.set_level(logging.ERROR)
    window = build(left_out)
    names = {p.name for p in window._plugins}
    assert left_out not in names and REQUIRED <= names

    window.open_settings()  # every tab builds and comes up, whatever's missing
    win = window._settings_win
    for tab in win.tabs():
        win.show_tab(tab)
        qapp.processEvents()
    assert ("shortcuts" in win.tabs()) == (left_out != "shortcuts")
    assert ({"video", "audio", "devices"} <= set(win.tabs())) == (left_out != "setup")
    assert "plugins" in win.tabs()
    win.close()

    window.stream_source("test:cam")
    assert window.is_streaming() and _Worker.made[-1].started
    _Worker.made[-1].status.emit("ok", "Streaming")
    window._on_worker_status("ok", "Streaming", window._sessions.focus)
    window.save_now()
    assert window.diagnostics_report()
    window.stop_all_streams()
    assert not window.is_streaming() and _Worker.made[-1].stopped

    # The host logs a plugin hook that raises instead of letting it through, so the log is where breakage shows
    assert [r.getMessage() for r in caplog.records if r.levelno >= logging.ERROR] == []


def test_every_card_and_section_in_the_window_folds(build):
    window = build(None)
    cards = {t.text() for t in window.findChildren(QLabel, "card_title")}
    folds = set(window._collapse._bodies)
    assert cards and cards <= {key.split("/")[1] for key in folds}
    assert {"camera_control/Camera/Light", "camera_control/Camera/Image", "monitoring/Phone/Warnings"} <= folds


def test_a_plugin_switched_off_and_on_again_has_its_folds_as_it_left_them(build):
    first = build(None)
    first._collapse.set_folded("transforms/Transforms", True)
    first._collapse.set_folded("camera_control/Camera/Image", True)
    first.save_now()

    without = build("transforms")  # Transforms is off for this run
    assert "transforms/Transforms" not in without._collapse._bodies
    assert without._collapse.is_folded("camera_control/Camera/Image")
    without.save_now()
    assert "transforms/Transforms" in load_config()["collapsed"]

    again = build(None)
    assert again._collapse.is_folded("transforms/Transforms")
    assert again._collapse._bodies["transforms/Transforms"].isHidden()
    assert again._collapse._bodies["camera_control/Camera/Image"].isHidden()
    assert not again._collapse._bodies["camera_control/Camera/Light"].isHidden()


def test_only_connection_is_required():
    assert set(OPTIONAL) | REQUIRED == {p.name for p in builtin_plugins()}
    assert len(OPTIONAL) == len(builtin_plugins()) - 1


def test_every_built_in_plugin_says_what_it_is_for_the_plugins_tab():
    for plugin in builtin_plugins():
        assert plugin.title and plugin.about, plugin.name
        assert plugin.required or plugin.when_off, f"{plugin.name}: say what goes away while it's off"
    assert {p.name for p in builtin_plugins() if p.required} == {"connection", "setup", "updates"}


class _WindowsShown(QObject):
    """Notes every widget shown as a window of its own."""

    def __init__(self):
        super().__init__()
        self.shown = []

    def eventFilter(self, obj, event):
        if event.type() == QEvent.Type.Show and isinstance(obj, QWidget) and obj.isWindow():
            self.shown.append(obj)
        return False


def test_building_the_window_and_settings_flashes_up_no_other_windows(build, qapp, monkeypatch):
    # A widget shown before it has a parent opens as a window of its own for a moment (seen on Windows)
    monkeypatch.setattr(updates_module, "is_unzipped_copy", lambda: True)  # as on Windows, where it brings a row along
    watch = _WindowsShown()
    qapp.installEventFilter(watch)
    try:
        window = build("")
        window.show()
        for page in ("general", "streaming", "video", "audio", "camera", "shortcuts", "plugins", "updates"):
            window.open_settings(page)
            qapp.processEvents()
    finally:
        qapp.removeEventFilter(watch)
    stray = [w for w in watch.shown if w is not window and w is not window._settings_win and not w.inherits("QMenu")]
    assert stray == [], [(type(w).__name__, w.objectName()) for w in stray]
    # A dropdown's list is a window of its own too, and building it early flashed it up on Windows without a Show event
    popups = [w for w in window.findChildren(QWidget) if type(w).__name__ == "QComboBoxPrivateContainer"]
    assert popups == [], [w.parent().objectName() for w in popups]


class _MovedAfterShow(QObject):
    """Notes a window moved or resized while it's already on screen."""

    def __init__(self):
        super().__init__()
        self.jumps = []

    def eventFilter(self, obj, event):
        if not (isinstance(obj, QWidget) and obj.isWindow() and obj.isVisible() and not obj.inherits("QMenu")):
            return False
        if event.type() == QEvent.Type.Move:
            self.jumps.append((type(obj).__name__, "Move"))
        elif event.type() == QEvent.Type.Resize:  # Your phones grows a pixel or two once its rows know their width
            old, new = event.oldSize(), event.size()
            if abs(old.width() - new.width()) > 2 or abs(old.height() - new.height()) > 2:
                self.jumps.append((type(obj).__name__, "Resize"))
        return False


def test_dialogs_are_placed_before_they_show(build, qapp, monkeypatch):
    # Centred or resized once showing, a dialog appeared at the top left of the screen for a moment on Windows
    monkeypatch.setattr(connection_module.PairingServer, "start", lambda self, *a, **k: SimpleNamespace(
        candidates=[], payload="", port=None, usb_payload=""))
    window = build("")
    window.show()
    qapp.processEvents()
    watch = _MovedAfterShow()
    qapp.installEventFilter(watch)
    try:
        conn = next(p for p in window._plugins if p.name == "connection")
        window.open_settings()
        conn.open_add_phone()
        conn.open_phones()
        for _ in range(5):
            qapp.processEvents()
    finally:
        qapp.removeEventFilter(watch)
        if conn._add_dlg is not None:
            conn._add_dlg.done(0)
    assert watch.jumps == []
