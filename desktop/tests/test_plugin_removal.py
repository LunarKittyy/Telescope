"""Plugins really are plugins: the app starts, streams and quits with any one of them left out, Connection aside."""

import logging
from types import SimpleNamespace

import pytest
from PyQt6.QtCore import QCoreApplication, QEvent, QTimer

import telescope.app as app_module
import telescope.plugins.connection as connection_module
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

    def __init__(self, **kwargs):
        self.kwargs = kwargs
        self.status, self.reconnected, self.vcam_opened = _Signal(), _Signal(), _Signal()
        self.started = self.stopped = False
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
def test_the_app_starts_streams_and_quits_without_any_one_optional_plugin(build, caplog, left_out):
    caplog.set_level(logging.ERROR)
    window = build(left_out)
    names = {p.name for p in window._plugins}
    assert left_out not in names and REQUIRED <= names

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


def test_only_connection_is_required():
    assert set(OPTIONAL) | REQUIRED == {p.name for p in builtin_plugins()}
    assert len(OPTIONAL) == len(builtin_plugins()) - 1


def test_every_built_in_plugin_says_what_it_is_for_the_plugins_dialog():
    for plugin in builtin_plugins():
        assert plugin.title and plugin.about, plugin.name
        assert plugin.required or plugin.when_off, f"{plugin.name}: say what goes away while it's off"
    assert {p.name for p in builtin_plugins() if p.required} == {"connection", "setup", "updates"}
