import numpy as np
import os
from dataclasses import replace
import time
from types import SimpleNamespace
import socket
import sys

import pytest
from PyQt6.QtCore import QCoreApplication, QEvent
from PyQt6.QtGui import QAction
from PyQt6.QtWidgets import QMenu, QWidget

import telescope.app as app_module
import telescope.sources as sources_module
from telescope import theme
from telescope.plugin import TelescopePlugin
from telescope.session import StreamSession


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


class _Plugin(TelescopePlugin):
    def __init__(self, name, config=None, panel=True):
        self.name = name
        self.config = dict(config or {})
        self.want_panel = panel
        self.setup_args = None
        self.started = []
        self.stopped = 0
        self.states = []
        self.applied = []

    def setup(self, host, bus):
        self.setup_args = (host, bus)

    def create_panel(self):
        self.panel = QWidget() if self.want_panel else None
        return self.panel

    def create_header_widget(self):
        return None

    def create_menu_actions(self):
        return list(getattr(self, "menu_actions", []))

    def process_frame(self, frame):
        return frame

    def get_config(self):
        return dict(self.config)

    def set_config(self, cfg):
        self.applied.append(dict(cfg))
        self.config.update(cfg)

    def on_stream_start(self, url, ctrl):
        self.started.append((url, ctrl))

    def on_stream_stop(self):
        self.stopped += 1

    def on_phone_state(self, state):
        self.states.append(state)


class _Connection(_Plugin):
    def __init__(
        self,
        selected="Phone",
        stream_info=("http://phone/video", "tok", True),
        wake=(True, ""),
    ):
        super().__init__("connection", {"mode": "wifi"})
        self.selected_device = selected
        self.stream_info = stream_info
        self.wake = wake
        self.selected = []
        self.synced = 0
        self.wakes = 0
        self.remote_stops = 0

    def get_stream_info(self, interactive=True, pid=None):
        return self.stream_info

    def select_device(self, name):
        self.selected.append(name)

    def sync_active_profile(self):
        self.synced += 1

    def session_target(self, pid=None):
        return None

    def show_selected(self, pid):
        self.selected_device = pid

    def select(self, pid):
        self.selected_device = pid

    def phone(self, pid):
        return None

    def ensure_phone_streaming(self, on_progress=None, target=None, opening=None):
        self.opening = opening
        self.wakes += 1
        self.progress_msgs = getattr(self, "progress_msgs", [])
        if on_progress:
            on_progress("waking...")
            self.progress_msgs.append("waking...")
        return self.wake

    def stop_phone_streaming(self, target=None):
        self.remote_stops += 1


class _StreamOutput(_Plugin):
    def __init__(self):
        super().__init__("stream_output", {"fps": 30})

    def get_stream_params(self):
        return 1280, 720, 25


class _Setup(_Plugin):
    def __init__(self):
        super().__init__("setup", {"canvas_preset": "preset"})

    def get_canvas_dims(self):
        return 1920, 1080


@pytest.fixture
def window(qapp, config_home, monkeypatch):
    monkeypatch.setattr(
        app_module.QSystemTrayIcon,
        "isSystemTrayAvailable",
        lambda: False,
    )
    win = app_module.TelescopeWindow()
    # Run phone-wake synchronously to keep _start() a single testable act; a real background thread would deliver queued signals to a destroyed QObject (PyQt hard abort).
    monkeypatch.setattr(
        sources_module.PhoneSource, "_spawn_wake",
        lambda self, *a: self._wake_phone(*a),
    )
    # Recorded, not run: the fetch sleeps and then signals, possibly after the window is gone.
    win.state_fetches = []
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_state_fetch",
                        lambda self, session_id: self._win.state_fetches.append(session_id))
    yield win
    # A dropped stream's timers would fire into a window that's gone, in a later test's event loop.
    for source in (*win._phones.values(), *win._sources.values()):
        source.end_recovery()
    # Don't call close(); a test's intentional closeEvent stub would abort Qt during fixture teardown.
    win._session = None
    win._tray = None
    # Destroy it now: windows left alive get restyled by a later apply_theme(), after their tests
    # swapped attributes out from under them, and that crashes Qt.
    win.deleteLater()
    QCoreApplication.sendPostedEvents(None, QEvent.Type.DeferredDelete)


def test_register_plugin_initializes_panel_and_captures_device_defaults(window):
    plugin = _Plugin("transforms", {"zoom": 1})

    window.register_plugin(plugin)

    assert plugin.setup_args == (window, window._bus)
    assert window._plugins == [plugin]
    assert window._plugin_defaults == {"transforms": {"zoom": 1}}
    assert window._panels["left"] == [plugin.panel]


@pytest.fixture(autouse=True)
def port_notices(monkeypatch):
    shown = []
    monkeypatch.setattr(app_module, "_port_taken_notice", lambda: shown.append(True))
    return shown


def test_acquire_single_instance_binds_and_listens(monkeypatch):
    calls = []

    class Socket:
        def setsockopt(self, *args): calls.append(("setsockopt", args))
        def bind(self, address): calls.append(("bind", address))
        def listen(self, count): calls.append(("listen", count))

    sock = Socket()
    monkeypatch.setattr(app_module.socket, "socket", lambda *_args: sock)
    assert app_module.acquire_single_instance() is sock
    assert ("bind", app_module._INSTANCE_ADDRESS) in calls
    assert ("listen", 1) in calls


def test_acquire_single_instance_notifies_existing_process(monkeypatch, port_notices):
    events = []

    class Server:
        def setsockopt(self, *_args): pass
        def bind(self, _address): raise OSError("in use")
        def close(self): events.append("server-close")

    class Client:
        def settimeout(self, timeout): events.append(("timeout", timeout))
        def connect(self, address): events.append(("connect", address))
        def sendall(self, data): events.append(("send", data))
        def recv(self, _size): return b"ok"
        def close(self): events.append("client-close")

    sockets = iter([Server(), Client()])
    monkeypatch.setattr(app_module.socket, "socket", lambda *_args: next(sockets))
    assert app_module.acquire_single_instance() is None
    assert ("send", b"raise:" + app_module.user_token()) in events
    assert events[0] == "server-close"
    assert port_notices == []


def test_acquire_single_instance_says_so_when_nothing_answers_the_raise(monkeypatch, port_notices):
    class Server:
        def setsockopt(self, *_args): pass
        def bind(self, _address): raise OSError("in use")
        def close(self): pass

    class Client:
        def settimeout(self, _timeout): pass
        def connect(self, _address): pass
        def sendall(self, _data): pass
        def recv(self, _size): return b""  # some other program accepted the connection and closed it
        def close(self): pass

    sockets = iter([Server(), Client()])
    monkeypatch.setattr(app_module.socket, "socket", lambda *_args: next(sockets))
    assert app_module.acquire_single_instance() is None
    assert port_notices == [True]


def test_a_running_telescope_answers_a_raise_with_ok():
    import threading
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(4)
    raised = threading.Event()
    threading.Thread(target=app_module.listen_for_raise, args=(srv, raised.set), daemon=True).start()
    try:
        with socket.create_connection(("127.0.0.1", srv.getsockname()[1]), timeout=5) as c:
            c.sendall(b"raise:" + app_module.user_token())
            assert c.recv(2) == b"ok"
        assert raised.is_set()
    finally:
        srv.close()


def test_acquire_single_instance_waits_for_the_old_copy_after_an_update(monkeypatch):
    attempts = []

    class Socket:
        def setsockopt(self, *_args): pass
        def bind(self, _address):
            attempts.append(1)
            if len(attempts) < 3:
                raise OSError("still running")
        def listen(self, _count): pass
        def close(self): pass

    monkeypatch.setattr(app_module.socket, "socket", lambda *_args: Socket())
    monkeypatch.setattr(app_module.time, "sleep", lambda _s: None)
    assert app_module.acquire_single_instance(wait=5) is not None
    assert len(attempts) == 3


def test_acquire_single_instance_tolerates_stale_listener(monkeypatch):
    closed = []

    class Socket:
        def setsockopt(self, *_args): pass
        def bind(self, _address): raise OSError("in use")
        def settimeout(self, _timeout): pass
        def connect(self, _address): raise OSError("stale")
        def close(self): closed.append(True)

    monkeypatch.setattr(app_module.socket, "socket", lambda *_args: Socket())
    assert app_module.acquire_single_instance() is None
    assert closed


def test_acquire_single_instance_closes_failed_notification_socket(monkeypatch):
    class Socket:
        def __init__(self, server):
            self.server = server
            self.closed = False

        def setsockopt(self, *_args):
            pass

        def bind(self, _address):
            if self.server:
                raise OSError("in use")

        def settimeout(self, _timeout):
            pass

        def connect(self, _address):
            raise OSError("stale listener")

        def close(self):
            self.closed = True

    server = Socket(server=True)
    client = Socket(server=False)
    sockets = iter([server, client])
    monkeypatch.setattr(app_module.socket, "socket", lambda *_args: next(sockets))

    app_module.acquire_single_instance()

    assert client.closed is True


def test_listen_for_raise_invokes_callback_and_closes_connection():
    events = []

    class Conn:
        def settimeout(self, timeout): events.append(("conn timeout", timeout))
        def recv(self, _size): return b"raise:" + app_module.user_token()
        def sendall(self, data): events.append(("reply", data))
        def close(self): events.append("closed")

    class Server:
        def settimeout(self, timeout): events.append(("timeout", timeout))
        def accept(self):
            if "accepted" in events:
                raise OSError("done")
            events.append("accepted")
            return Conn(), ("127.0.0.1", 1)

    app_module.listen_for_raise(Server(), lambda: events.append("raised"))
    assert events == [("timeout", 1.0), "accepted", ("conn timeout", 1.0), "raised", ("reply", b"ok"), "closed"]


def test_listen_for_raise_ignores_wrong_message_and_timeouts(monkeypatch):
    events = []

    class Conn:
        def settimeout(self, _timeout): pass
        def recv(self, _size): return b"other"
        def close(self): events.append("closed")

    class Server:
        def __init__(self): self.calls = 0
        def settimeout(self, _timeout): pass
        def accept(self):
            self.calls += 1
            if self.calls == 1: raise socket.timeout()
            if self.calls == 2: return Conn(), None
            raise OSError("done")

    app_module.listen_for_raise(Server(), lambda: events.append("raised"))
    assert events == ["closed"]


def test_a_silent_connection_does_not_stop_later_raises():
    # Anything on this machine that connects and says nothing used to block the listener for good.
    import threading
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(4)
    raised = threading.Event()
    threading.Thread(target=app_module.listen_for_raise, args=(srv, raised.set), daemon=True).start()
    port = srv.getsockname()[1]
    silent = socket.create_connection(("127.0.0.1", port))
    try:
        with socket.create_connection(("127.0.0.1", port)) as c:
            c.sendall(b"raise:" + app_module.user_token())
        assert raised.wait(5)
    finally:
        silent.close()
        srv.close()


def test_a_reset_connection_does_not_stop_later_raises():
    import struct
    import threading
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(4)
    raised = threading.Event()
    listener = threading.Thread(target=app_module.listen_for_raise, args=(srv, raised.set), daemon=True)
    listener.start()
    port = srv.getsockname()[1]
    try:
        rude = socket.create_connection(("127.0.0.1", port))
        rude.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        time.sleep(0.2)  # the listener is waiting in recv
        rude.close()  # sends RST
        time.sleep(0.2)
        with socket.create_connection(("127.0.0.1", port)) as c:
            c.sendall(b"raise:" + app_module.user_token())
        assert raised.wait(5)
    finally:
        srv.close()


def test_a_canvas_reload_does_not_start_over_a_stream_started_meanwhile(window, monkeypatch):
    starts = []
    monkeypatch.setattr(window, "_start", lambda *_a: starts.append(True))
    window._session = StreamSession(source=window._phone, id=1, url="url", client=object(), worker=object())
    window._on_canvas_reload_done(True, "", True)
    assert starts == []


def _resizing(window, monkeypatch):
    """A stream stopped for a canvas resize that hasn't finished yet; returns the starts it asks for."""
    monkeypatch.setattr(app_module, "IS_LINUX", False)
    monkeypatch.setattr(app_module.threading, "Thread",
                        lambda target, daemon=False: SimpleNamespace(start=lambda: None))
    starts = []
    monkeypatch.setattr(window, "_start", lambda *_a, **_k: starts.append(True))
    window._session = StreamSession(source=window._phone, id=1, url="url", client=_Client(), worker=_RetargetWorker())
    window.restart_vcam_canvas(1280, 720)
    assert window._session is None
    return starts


def test_a_canvas_resize_starts_the_stream_it_stopped_again(window, monkeypatch):
    starts = _resizing(window, monkeypatch)
    window._on_canvas_reload_done(True, "", True)
    assert starts == [True]


def test_stop_during_a_canvas_resize_is_not_undone_when_it_finishes(window, monkeypatch):
    starts = _resizing(window, monkeypatch)
    # Start, then Stop, before the resize is through
    window._session = StreamSession(source=window._phone, id=2, url="url", client=_Client(), worker=_RetargetWorker())
    window._stop_all()
    window._on_canvas_reload_done(True, "", True)
    assert starts == [] and window._session is None


def test_a_loopback_reload_that_raises_still_reports_back(window, monkeypatch, qapp):
    import telescope.app as app_module
    import telescope.platform.linux as linux
    done = []
    monkeypatch.setattr(app_module, "IS_LINUX", True)
    monkeypatch.setattr(linux, "v4l2_reload", lambda: (_ for _ in ()).throw(RuntimeError("modprobe gone")))
    window.restart_vcam_canvas(1280, 720, on_done=lambda *a: done.append(a))
    deadline = time.monotonic() + 3
    while not done and time.monotonic() < deadline:
        qapp.processEvents()
        time.sleep(0.01)
    assert done


def test_a_canvas_change_while_idle_does_not_announce_a_stop(window, monkeypatch, qapp):
    import telescope.app as app_module
    monkeypatch.setattr(app_module, "IS_LINUX", False)
    stopped = []
    window._bus.stream_stopped.connect(lambda: stopped.append(True))
    done = []
    window.restart_vcam_canvas(1280, 720, on_done=lambda *a: done.append(a))
    deadline = time.monotonic() + 3
    while not done and time.monotonic() < deadline:
        qapp.processEvents()
        time.sleep(0.01)
    assert done and stopped == []


def test_a_canvas_change_while_the_phone_wakes_starts_it_again_without_stopping_it(window, monkeypatch, qapp):
    import telescope.app as app_module
    monkeypatch.setattr(app_module, "IS_LINUX", False)
    connection = _Connection()
    window.register_plugin(connection)
    spawned = _real_spawn_wake(monkeypatch)
    window._start()
    wake_id, _conn, url, token, _target, _opening = spawned[0]
    starts = []
    monkeypatch.setattr(window, "start_stream", lambda *a, **k: starts.append(True))
    done = []
    window.restart_vcam_canvas(1280, 720, on_done=lambda *a: done.append(a))
    window._phone._on_wake_done(wake_id, True, "", url, token)  # the first wake lands during the reload
    deadline = time.monotonic() + 3
    while not done and time.monotonic() < deadline:
        qapp.processEvents()
        time.sleep(0.01)
    window._drain_phone_stops()
    assert starts == [True] and connection.remote_stops == 0


def test_register_headless_global_plugin_does_not_add_panel(window):
    plugin = _Plugin("global", panel=False)
    window.register_plugin(plugin)
    assert all(not panels for panels in window._panels.values())


@pytest.mark.parametrize("region", ["left", "center", "right"])
def test_register_plugin_routes_panel_to_its_declared_region(window, region):
    plugin = _Plugin("regional")
    plugin.panel_region = region
    window.register_plugin(plugin)
    assert window._panels[region] == [plugin.panel]


def test_register_plugin_routes_unknown_region_to_the_left_rail(window):
    plugin = _Plugin("regional")
    plugin.panel_region = "nowhere"
    window.register_plugin(plugin)
    assert window._panels["left"] == [plugin.panel]


@pytest.mark.parametrize("width,mode", [
    (1400, "three"), (1300, "three"),
    (1299, "two"),   (900, "two"),
    (899, "one"),    (600, "one"),
])
def test_layout_mode_follows_window_width(window, width, mode):
    # Breakpoints are design pixels, scaled with the UI font like every other width.
    assert window._layout_mode_for(app_module.ui_px(width)) == mode


def test_narrow_layout_stacks_every_panel_into_one_visible_column(window):
    for name, region in (("a", "left"), ("b", "center"), ("c", "right")):
        plugin = _Plugin(name)
        plugin.panel_region = region
        window.register_plugin(plugin)

    window.resize(700, 800)
    window._refresh_layout(force=True)

    assert window._layout_mode == "one"
    assert window._columns[0].isVisibleTo(window)
    assert not window._columns[1].isVisibleTo(window)
    assert not window._columns[2].isVisibleTo(window)
    col = window._column_layouts[0]
    stacked = [col.itemAt(i).widget() for i in range(col.count())]
    stacked = [w for w in stacked if w is not None]
    # Center first (video visible without scrolling), then left, then right panels.
    assert stacked == [window._panels["center"][0],
                       window._panels["left"][0],
                       window._panels["right"][0]]


def test_settings_menu_collects_actions_from_every_plugin(window, qapp):
    plugin = _Plugin("with_actions")
    action = QAction("Do a thing", None)
    plugin.menu_actions = [action]
    window.register_plugin(plugin)

    assert [a.text() for p in window._plugins for a in p.create_menu_actions()] \
        == ["Do a thing"]


def test_settings_menu_takes_a_submenu_and_owns_it(window, qapp, monkeypatch):
    plugin = _Plugin("with_submenu")
    sub = QMenu("More")
    sub.addAction("Inside")
    plugin.menu_actions = [QAction("Before", None), sub]
    window.register_plugin(plugin)
    shown = []
    monkeypatch.setattr(QMenu, "exec", lambda self, *_a: shown.append(self))

    window._show_settings_menu()

    (menu,) = shown
    before, more = menu.actions()
    assert before.text() == "Before" and more.menu() is sub
    assert sub.parent() is menu and sub.isWindow()  # still a popup, and freed with the menu


def test_save_config_separates_global_and_device_local_plugins(window, config_home):
    connection = _Connection(selected="PhoneA")
    global_plugin = _Plugin("setup", {"canvas": "auto"})
    local_plugin = _Plugin("transforms", {"zoom": 2})
    for plugin in (connection, global_plugin, local_plugin):
        window.register_plugin(plugin)

    window.save_now()

    cfg = config_home.load_config()
    assert cfg["selected_device"] == "PhoneA"
    assert cfg["plugin_configs"] == {
        "connection": {"mode": "wifi"},
        "setup": {"canvas": "auto"},
    }
    assert cfg["devices"]["PhoneA"]["plugin_configs"] == {
        "transforms": {"zoom": 2}
    }


def test_a_settings_file_that_cant_be_read_is_not_saved_over(window, config_home, monkeypatch):
    cfg = config_home.load_config()
    cfg["devices"] = {"PhoneB": {"plugin_configs": {"transforms": {"zoom": 3}}}}
    config_home.save_config(cfg)
    window.register_plugin(_Connection(selected="PhoneA"))
    window.register_plugin(_Plugin("transforms", {"zoom": 2}))
    from pathlib import Path
    real = Path.read_bytes

    def locked(self):
        if self.name == "telescope_config.json":
            raise PermissionError("held by a sync app")
        return real(self)

    monkeypatch.setattr(Path, "read_bytes", locked)
    monkeypatch.setattr(config_home.time, "sleep", lambda _s: None)
    window.save_now()
    monkeypatch.setattr(Path, "read_bytes", real)

    assert config_home.load_config()["devices"]["PhoneB"]["plugin_configs"] == {"transforms": {"zoom": 3}}
    assert window._save_timer.isActive()  # and it tries again


def test_save_config_without_connection_skips_device_profile(window, config_home):
    window.register_plugin(_Plugin("transforms", {"zoom": 2}))
    window.save_now()
    assert config_home.load_config()["devices"] == {}


def test_save_config_failure_notifies_once_until_a_save_succeeds(window, config_home, monkeypatch):
    notifications = []
    monkeypatch.setattr(window, "send_notification", lambda *args: notifications.append(args))
    monkeypatch.setattr(app_module, "save_config", lambda _cfg: False)

    window.save_now()
    window.save_now()

    assert len(notifications) == 1
    assert notifications[0][0] == "Telescope - Save failed"

    monkeypatch.setattr(app_module, "save_config", lambda _cfg: True)
    window.save_now()
    monkeypatch.setattr(app_module, "save_config", lambda _cfg: False)
    window.save_now()

    assert len(notifications) == 2


def test_apply_device_profile_resets_defaults_before_saved_values(window, config_home):
    plugin = _Plugin("transforms", {"zoom": 1, "pan_x": 0})
    window.register_plugin(plugin)
    cfg = config_home.load_config()
    cfg["devices"] = {
        "Phone": {"plugin_configs": {"transforms": {"zoom": 3}}}
    }
    config_home.save_config(cfg)
    plugin.config = {"zoom": 5, "pan_x": 1}

    window._apply_device_profile("Phone")

    assert plugin.applied == [{"zoom": 1, "pan_x": 0}, {"zoom": 3}]
    assert plugin.config == {"zoom": 3, "pan_x": 0}


def test_apply_device_profile_none_uses_defaults_only(window):
    plugin = _Plugin("monitoring", {"battery_alert": 20})
    window.register_plugin(plugin)
    plugin.config = {"battery_alert": 90}
    window._apply_device_profile(None)
    assert plugin.config == {"battery_alert": 20}


def test_switch_device_saves_old_profile_applies_new_and_restarts(window, config_home, monkeypatch):
    plugin = _Plugin("transforms", {"zoom": 2})
    window.register_plugin(plugin)
    cfg = config_home.load_config()
    cfg["devices"] = {
        "New": {"plugin_configs": {"transforms": {"zoom": 4}}}
    }
    config_home.save_config(cfg)
    window._session = StreamSession(source=window._phone, id=1, url="url", client=object(), worker=object())
    calls = []
    monkeypatch.setattr(window, "_stop_all", lambda **_kw: calls.append("stop") or setattr(window, "_session", None))
    monkeypatch.setattr(window, "_start", lambda: calls.append("start"))
    window._bus.device_changed.connect(lambda name: calls.append(f"changed {name}"))

    window.switch_device("Old", "New")

    cfg = config_home.load_config()
    assert cfg["devices"]["Old"]["plugin_configs"]["transforms"] == {"zoom": 2}
    assert cfg["selected_device"] == "New"
    assert plugin.config["zoom"] == 4
    assert calls == ["stop", "changed New", "start"]


def test_reconnect_stream_only_restarts_when_active(window, monkeypatch):
    calls = []
    monkeypatch.setattr(window, "_stop", lambda **_kw: calls.append("stop"))
    monkeypatch.setattr(window, "_start", lambda **_kw: calls.append("start"))
    window.reconnect_stream()
    assert calls == []

    window._session = StreamSession(source=window._phone, id=1, url="url", client=object(), worker=object())
    window.reconnect_stream()
    assert calls == ["stop", "start"]


def test_window_implements_the_public_host_services_contract(window):
    # Every method plugins are allowed to call must exist on the window as a
    # public (non-underscore) callable - guards against a future rename
    # reintroducing private coupling that the HostServices Protocol hides.
    from telescope.plugin import HostServices
    contract = [
        n for n, v in vars(HostServices).items()
        if not n.startswith("_") and callable(v)
    ]
    assert "schedule_save" in contract  # sanity: the introspection found methods
    for name in contract:
        assert callable(getattr(window, name, None)), f"host missing {name}()"


def test_is_streaming_and_stop_stream_track_the_session(window, monkeypatch):
    assert window.is_streaming() is False

    stopped = []
    monkeypatch.setattr(window, "_stop", lambda **_kw: stopped.append(True))
    # stop_stream is a guarded no-op while idle - no spurious _stop().
    window.stop_stream()
    assert stopped == []

    window._session = StreamSession(source=window._phone, id=1, url="url", client=object(), worker=object())
    assert window.is_streaming() is True
    window.stop_stream()
    assert stopped == [True]


def test_update_stream_output_forwards_only_provided_values(window):
    class _Worker:
        def __init__(self):
            self.updates = []

        def update_output(self, **kwargs):
            self.updates.append(kwargs)

    # No-op when idle.
    window.update_stream_output(width=1280, height=720)

    worker = _Worker()
    window._session = StreamSession(source=window._phone, id=1, url="url", client=object(), worker=worker)
    window.update_stream_output(fps=60)
    window.update_stream_output(width=None, height=None)  # None = pass-through
    assert worker.updates == [{"fps": 60}, {"width": None, "height": None}]


def test_stream_reconnected_resends_settings_to_every_plugin(window):
    plugin_a = _Plugin("a")
    plugin_b = _Plugin("b")
    window.register_plugin(plugin_a)
    window.register_plugin(plugin_b)
    client = object()
    window._session = StreamSession(source=window._phone, id=1, url="http://phone/video", client=client, worker=object())

    window._on_stream_reconnected()

    assert plugin_a.started == [("http://phone/video", client)]
    assert plugin_b.started == [("http://phone/video", client)]


def test_stream_reconnected_is_noop_without_an_active_session(window):
    plugin = _Plugin("a")
    window.register_plugin(plugin)
    window._session = None

    window._on_stream_reconnected()

    assert plugin.started == []


def test_toggle_routes_to_start_or_stop(window, monkeypatch):
    calls = []
    monkeypatch.setattr(window, "_start", lambda: calls.append("start"))
    monkeypatch.setattr(window, "_stop_all", lambda **_kw: calls.append("stop"))
    window._toggle()
    window._session = StreamSession(source=window._phone, id=1, url="url", client=object(), worker=object())
    window._toggle()
    assert calls == ["start", "stop"]


def test_apply_config_routes_global_and_selected_device_config(window, monkeypatch):
    connection = _Connection()
    setup = _Plugin("setup", {"old": True})
    local = _Plugin("transforms", {"zoom": 1})
    for plugin in (connection, setup, local):
        window.register_plugin(plugin)
    seen = []
    monkeypatch.setattr(window, "_apply_device_profile", seen.append)

    window._apply_config({
        "selected_device": "PhoneB",
        "plugin_configs": {"connection": {"mode": "usb"}, "setup": {"canvas": "4k"}},
    })

    assert connection.config["mode"] == "usb"
    assert setup.config["canvas"] == "4k"
    assert local.applied == []
    assert seen == ["PhoneB"]
    assert connection.synced == 1


class _Picky(_Plugin):
    """Rejects a saved value it can't use, the way a real plugin's spin box does."""

    def set_config(self, cfg):
        if cfg.get("zoom") == "junk":
            raise TypeError("bad zoom")
        super().set_config(cfg)


def test_a_saved_config_that_wont_load_falls_back_to_defaults(window, config_home):
    # One bad value (a hand edit, another version's shape) used to stop the app from starting at all.
    connection = _Connection(selected="PhoneA")
    setup = _Picky("setup", {"zoom": 1})
    local = _Picky("transforms", {"zoom": 1})
    for plugin in (connection, setup, local):
        window.register_plugin(plugin)

    window._apply_config({
        "selected_device": "PhoneA",
        "plugin_configs": {"setup": "not a dict"},
    })
    config_home.save_config({
        "selected_device": "PhoneA", "plugin_configs": {},
        "devices": {"PhoneA": {"plugin_configs": {"transforms": {"zoom": "junk"}}}},
    })
    window._apply_device_profile("PhoneA")

    assert setup.config == {"zoom": 1}
    assert local.config == {"zoom": 1}


def test_apply_config_empty_is_noop(window):
    window._apply_config({})


def test_start_without_connection_or_invalid_stream_is_noop(window):
    failed = []
    window._bus.stream_start_failed.connect(lambda: failed.append(1))
    window._start()
    assert window._worker is None
    window.register_plugin(_Connection(stream_info=(None, None, False)))
    window._start()
    assert window._worker is None
    assert failed == [1]  # the Start that got as far as asking for the stream says it ended


def test_start_builds_worker_pipeline_and_notifies_plugins(window, monkeypatch):
    connection = _Connection()
    output = _StreamOutput()
    setup = _Setup()
    transform = _Plugin("transforms", {"zoom": 1})
    for plugin in (connection, output, setup, transform):
        window.register_plugin(plugin)

    clients = []
    workers = []
    threads = []

    class Client:
        def __init__(self, url, token):
            self.url = url
            self.token = token
            self.closed = False
            clients.append(self)

        def close(self):
            self.closed = True

    class Worker:
        def __init__(self, **kwargs):
            self.kwargs = kwargs
            self.status = _Signal()
            self.reconnected = _Signal()
            self.vcam_opened = _Signal()
            self.started = False
            workers.append(self)

        def start(self):
            self.started = True

    class Thread:
        def __init__(self, target, args=(), daemon=False):
            threads.append((target, args, daemon))

        def start(self):
            pass

    monkeypatch.setattr(sources_module, "PhoneControlClient", Client)
    monkeypatch.setattr(app_module, "StreamWorker", Worker)
    monkeypatch.setattr(app_module.threading, "Thread", Thread)
    bus_urls = []
    window._bus.stream_started.connect(bus_urls.append)

    window._start()

    assert clients[0].url == "http://phone/video"
    assert clients[0].token == "tok"
    assert workers[0].kwargs["width"] == 1280
    assert workers[0].kwargs["height"] == 720
    assert workers[0].kwargs["fps"] == 25
    assert workers[0].kwargs["canvas_width"] == 1920
    assert workers[0].kwargs["canvas_height"] == 1080
    assert workers[0].kwargs["auth"] == "tok"
    frame = np.zeros((2, 2, 3), np.uint8)
    steps = workers[0].kwargs["frame_pipeline"]
    assert len(steps) == len(window._plugins)  # every fake here has its own process_frame
    assert all(step(frame) is frame for step in steps)
    assert workers[0].started is True
    assert bus_urls == ["http://phone/video"]
    assert all(plugin.started for plugin in window._plugins)
    assert window._session == StreamSession(
        source=window._phone, id=1, url="http://phone/video", client=clients[0], worker=workers[0],
    )
    assert window._worker is workers[0]
    assert window._ctrl is clients[0]
    assert threads[0][1] == (window._session.id,)
    assert threads[0][2] is True
    assert window._start_btn.text() == "Stop Streaming"
    # on_progress callback must reach plugin and route through signal to fix "frozen button for 12s" complaint.
    assert connection.progress_msgs == ["waking..."]


def test_a_stream_the_panels_dont_show_gets_their_steps_frozen(window):
    class Frozen(_Plugin):
        def frame_step(self):
            return lambda frame: frame + 1

    class Broken(_Plugin):
        def frame_step(self):
            raise RuntimeError("no")

    for plugin in (Frozen("transforms"), _Plugin("preview"), Broken("other")):
        window.register_plugin(plugin)
    frame = np.zeros((1, 1, 3), np.uint8)
    assert len(window._pipeline()) == 3
    steps = window._pipeline(live=False)
    assert len(steps) == 1 and steps[0](frame)[0, 0, 0] == 1


def test_wake_progress_updates_status_while_waking(window):
    window._phone._wake_id = 5
    window._waking = True

    window._phone._on_wake_progress(5, "Phone's camera is opening... (3s)")

    assert window._status_lbl.fullText() == "Phone's camera is opening... (3s)"


def test_wake_progress_ignores_a_stale_or_cancelled_wake(window):
    window._phone._wake_id = 5
    window._waking = True
    window._on_worker_status("other", "sentinel")  # baseline status text

    # A progress tick for an old wake_id (a newer start superseded it).
    window._phone._on_wake_progress(4, "stale tick")
    assert window._status_lbl.fullText() == "sentinel"

    # A progress tick after the wake was cancelled (stop/device switch).
    window._waking = False
    window._phone._on_wake_progress(5, "too late")
    assert window._status_lbl.fullText() == "sentinel"


def test_start_uses_defaults_without_optional_plugins(window, monkeypatch):
    window.register_plugin(_Connection())
    captured = []

    class Worker:
        def __init__(self, **kwargs):
            captured.append(kwargs)
            self.status = _Signal()
            self.reconnected = _Signal()
            self.vcam_opened = _Signal()

        def start(self):
            pass

    monkeypatch.setattr(sources_module, "PhoneControlClient", lambda _url, _token: object())
    monkeypatch.setattr(app_module, "StreamWorker", Worker)
    monkeypatch.setattr(
        app_module.threading,
        "Thread",
        lambda **_kwargs: SimpleNamespace(start=lambda: None),
    )
    window._start()
    assert captured[0]["width"] is None
    assert captured[0]["height"] is None
    assert captured[0]["fps"] == 30
    assert captured[0]["canvas_width"] is None


def test_stop_requests_worker_closes_client_and_notifies_plugins(window):
    plugin = _Plugin("other")
    window.register_plugin(plugin)
    bus = []
    window._bus.stream_stopped.connect(lambda: bus.append(True))

    class Worker:
        def __init__(self):
            self.status = _Signal()
            self.status.connect(window._on_worker_status)
            self.reconnected = _Signal()
            self.vcam_opened = _Signal()
            self.reconnected.connect(window._on_stream_reconnected)
            self.vcam_opened.connect(window._bus.vcam_opened)
            self.stop_requested = False

        def request_stop(self):
            self.stop_requested = True

        def wait(self, timeout):
            self.timeout = timeout
            return True

    class Client:
        def close(self):
            self.closed = True

    worker = Worker()
    client = Client()
    window._session = StreamSession(source=window._phone, id=1, url="url", client=client, worker=worker)

    window._stop()

    assert worker.stop_requested is True
    assert worker.timeout == 5000
    assert client.closed is True
    assert window._worker is None
    assert window._ctrl is None
    assert window._session is None
    assert plugin.stopped == 1
    assert bus == [True]
    assert window._start_btn.text() == "Start Streaming"


def test_stop_is_safe_when_already_stopped(window):
    window._stop()
    assert window._status_lbl.fullText() == "Not streaming"


def test_restart_canvas_non_linux_waits_and_restarts_active_stream(window, monkeypatch):
    events = []

    class OldWorker:
        def wait(self, timeout):
            events.append(("wait", timeout))

    old = OldWorker()
    window._session = StreamSession(source=window._phone, id=1, url="url", client=object(), worker=old)
    monkeypatch.setattr(app_module, "IS_LINUX", False)
    monkeypatch.setattr(
        window,
        "_stop",
        lambda **_kw: events.append("stop") or setattr(window, "_session", None),
    )
    monkeypatch.setattr(window, "_start", lambda *_a: events.append("start"))

    class ImmediateThread:
        def __init__(self, target, daemon): self.target = target
        def start(self): self.target()

    monkeypatch.setattr(app_module.threading, "Thread", ImmediateThread)
    done = []
    window.restart_vcam_canvas(1920, 1080, on_done=lambda *args: done.append(args))
    assert events == ["stop", ("wait", 5000), "start"]
    assert done == [(True, "")]


def test_canvas_reload_failure_reports_error_and_clears_callback(window, monkeypatch):
    done = []
    window._vcam_reload_callback = lambda *args: done.append(args)
    monkeypatch.setattr(window, "_start", lambda: (_ for _ in ()).throw(AssertionError()))
    window._on_canvas_reload_done(False, "busy", True)
    assert window._status_lbl.fullText() == "Reload failed: busy"
    assert window._status_lbl.objectName() == "status_err"
    assert done == [(False, "busy")]
    assert window._vcam_reload_callback is None


_VALID_STATE = {
    "cameras": [], "auto": True, "wb_manual": False, "ois": True,
    "focus_mode": "continuous", "focus_distance": 0.0, "ae_comp": 0,
    "nr_mode": 1, "edge_mode": 1, "black_level_lock": False, "torch": False,
    "battery": 80, "charging": False, "battery_temp_c": 25.0,
}


def test_fetch_state_retries_then_emits_success(window, monkeypatch):
    states = iter([None, _VALID_STATE])
    ctrl = SimpleNamespace(get_state=lambda: next(states))
    window._session = StreamSession(source=window._phone, id=1, url="url", client=ctrl, worker=object())
    emitted = []
    window._phone._sig_state.connect(lambda sid, state: emitted.append((sid, state)))
    sleeps = []
    monkeypatch.setattr(app_module.time, "sleep", sleeps.append)

    window._phone._fetch_state_async(1)

    assert emitted == [(1, _VALID_STATE)]
    assert sleeps == [1.5, 2]


def test_fetch_state_emits_empty_after_three_failures(window, monkeypatch):
    ctrl = SimpleNamespace(get_state=lambda: None)
    window._session = StreamSession(source=window._phone, id=1, url="url", client=ctrl, worker=object())
    emitted = []
    window._phone._sig_state.connect(lambda sid, state: emitted.append((sid, state)))
    monkeypatch.setattr(app_module.time, "sleep", lambda _seconds: None)
    window._phone._fetch_state_async(1)
    assert emitted == [(1, {})]


def test_fetch_state_exits_if_session_is_removed(window, monkeypatch):
    monkeypatch.setattr(app_module.time, "sleep", lambda _seconds: None)
    window._session = None
    emitted = []
    window._phone._sig_state.connect(lambda sid, state: emitted.append((sid, state)))
    window._phone._fetch_state_async(1)
    assert emitted == []


def test_fetch_state_discards_result_from_a_superseded_session(window, monkeypatch):
    ctrl = SimpleNamespace(get_state=lambda: {**_VALID_STATE, "battery": 10})
    window._session = StreamSession(source=window._phone, id=1, url="phoneA", client=ctrl, worker=object())
    emitted = []
    window._phone._sig_state.connect(lambda sid, state: emitted.append((sid, state)))

    def sleep_and_switch(_seconds):
        # Switch device while fetch sleeps, so old result is stale when it completes.
        window._session = StreamSession(source=window._phone, id=2, url="phoneB", client=object(), worker=object())

    monkeypatch.setattr(app_module.time, "sleep", sleep_and_switch)

    window._phone._fetch_state_async(1)

    assert emitted == []


def test_apply_state_emits_bus_and_calls_plugins(window):
    plugin = _Plugin("other")
    window.register_plugin(plugin)
    window._session = StreamSession(source=window._phone, id=1, url="url", client=object(), worker=object())
    bus = []
    window._bus.phone_state_updated.connect(bus.append)
    window._apply_state(1, _VALID_STATE)
    assert bus == [_VALID_STATE]
    assert plugin.states == [_VALID_STATE]


def test_apply_state_discards_result_for_inactive_session(window):
    plugin = _Plugin("other")
    window.register_plugin(plugin)
    window._session = StreamSession(source=window._phone, id=2, url="url", client=object(), worker=object())
    bus = []
    window._bus.phone_state_updated.connect(bus.append)
    window._apply_state(1, _VALID_STATE)
    assert bus == []
    assert plugin.states == []


def test_apply_state_rejects_malformed_non_empty_state(window):
    plugin = _Plugin("other")
    window.register_plugin(plugin)
    window._session = StreamSession(source=window._phone, id=1, url="url", client=object(), worker=object())
    bus = []
    window._bus.phone_state_updated.connect(bus.append)

    window._apply_state(1, {"battery": 50})  # missing every other required field

    assert bus == []
    assert plugin.states == []
    assert "can't read" in window._status_lbl.fullText()


def test_apply_state_accepts_empty_state(window):
    plugin = _Plugin("other")
    window.register_plugin(plugin)
    window._session = StreamSession(source=window._phone, id=1, url="url", client=object(), worker=object())
    bus = []
    window._bus.phone_state_updated.connect(bus.append)

    window._apply_state(1, {})

    assert bus == [{}]
    assert plugin.states == [{}]


@pytest.mark.parametrize(
    "kind,object_name",
    [("ok", "status_ok"), ("warn", "status_warn"), ("other", "status_dim")],
)
def test_worker_status_updates_status_label(window, kind, object_name):
    window._on_worker_status(kind, "message")
    assert window._status_lbl.fullText() == "message"
    assert window._status_lbl.objectName() == object_name


def test_worker_fps_and_idle_status(window):
    window._on_worker_status("fps", "29.9 fps")
    assert window._fps_lbl.text() == "29.9 fps"
    stopped = []
    window._bus.stream_stopped.connect(lambda: stopped.append(True))
    worker = _RetargetWorker()
    window._session = StreamSession(source=window._phone, id=1, url="url", client=_Client(), worker=worker)
    worker.status.connect(window._on_worker_status)
    window._on_worker_status("idle", "Not streaming")  # the worker ended by itself: tidied up like a Stop
    assert window._fps_lbl.text() == "—"
    assert window._worker is None
    assert window._session is None
    assert window._start_btn.text() == "Start Streaming"
    assert stopped == [True]
    assert window._banners.issue(f"stopped:{window._phone.id}").title == "The stream stopped unexpectedly"


def test_a_start_is_not_started_again_while_it_finds_the_phone(window):
    starts = []

    class Conn(_Connection):
        def get_stream_info(self, interactive=True, pid=None):
            window.start_stream()  # an auto-start timer firing inside the nested event loop
            starts.append(True)
            return None, None, False

    window.register_plugin(Conn())
    window._start()
    assert starts == [True]


def test_a_start_that_raises_says_so(window):
    class Conn(_Connection):
        def get_stream_info(self, interactive=True):
            raise RuntimeError("boom")

    window.register_plugin(Conn())
    window._start()
    assert window._banners.issue("start").title == "Couldn't start streaming"
    assert not window.is_starting()


def test_a_setting_changed_just_before_quit_is_saved(window, config_home):
    plugin = _Plugin("global", {"a": 1})
    window.register_plugin(plugin)
    plugin.config["a"] = 2
    window.schedule_save()
    window._shut_down()
    assert config_home.load_config()["plugin_configs"]["global"] == {"a": 2}


def test_reconnecting_status_is_warn_coloured_and_animates_dots(window):
    window._on_worker_status("reconnecting", "Stream dropped - reconnecting")

    assert window._status_lbl.fullText() == "Stream dropped - reconnecting."
    assert window._status_lbl.objectName() == "status_warn"

    window._tick_reconnecting_animation()
    assert window._status_lbl.fullText() == "Stream dropped - reconnecting.."
    window._tick_reconnecting_animation()
    assert window._status_lbl.fullText() == "Stream dropped - reconnecting..."
    # Loops back to one dot rather than growing forever.
    window._tick_reconnecting_animation()
    assert window._status_lbl.fullText() == "Stream dropped - reconnecting."


def test_reconnecting_animation_stops_when_another_status_arrives(window):
    window._on_worker_status("reconnecting", "Stream dropped - reconnecting")
    assert window._reconnecting_timer.isActive()

    window._on_worker_status("ok", "Stream reconnected")

    # Timer must be stopped, not just replaced, so it can't fire and overwrite new status.
    assert not window._reconnecting_timer.isActive()
    assert window._status_lbl.fullText() == "Stream reconnected"
    assert window._status_lbl.objectName() == "status_ok"


def test_status_colour_actually_changes_with_kind(qapp):
    # Swapping objectName alone leaves the QSS colour stale; set_status_kind must re-polish.
    from PyQt6.QtWidgets import QLabel
    from telescope.theme import apply_theme
    from telescope.widgets.common import set_status_kind
    apply_theme(qapp)
    lbl = QLabel("x")
    set_status_kind(lbl, "status_dim")
    lbl.ensurePolished()
    set_status_kind(lbl, "status_err")
    assert lbl.palette().color(lbl.foregroundRole()).name() == theme.ERR.lower()


def test_resolution_pending_shows_warn_color_until_confirmed(window):
    """Pending resolution shows as warn (amber) until confirmed."""
    window._on_resolution_pending(1280, 720)
    assert f"color: {theme.WARN}" in window._fps_lbl.styleSheet()
    assert window._phone.pending_resolution == (1280, 720)

    # A "fps" update reporting a size that doesn't match yet (still mid
    # switch) must not clear the pending state early.
    window._on_worker_status("fps", "29.9 fps  1920x1080")
    assert f"color: {theme.WARN}" in window._fps_lbl.styleSheet()
    assert window._phone.pending_resolution == (1280, 720)

    # The matching size arrives - back to the default (green/OK) style.
    window._on_worker_status("fps", "29.9 fps  1280x720")
    assert window._fps_lbl.styleSheet() == ""
    assert window._phone.pending_resolution is None


def test_resolution_pending_times_out_to_error_then_self_clears(window, monkeypatch):
    fired = {}

    def fake_single_shot(ms, fn):
        fired["ms"] = ms
        fn()

    monkeypatch.setattr(app_module.QTimer, "singleShot", staticmethod(fake_single_shot))

    window._on_resolution_pending(1280, 720)
    window._phone.pending_resolution_timer.timeout.emit()

    assert fired["ms"] == 4000
    # Monkeypatched singleShot ran callback immediately, so auto-clear already executed.
    assert window._fps_lbl.styleSheet() == ""
    assert window._phone.pending_resolution is None


def test_resolution_pending_cleared_on_idle_status(window):
    window._on_resolution_pending(1280, 720)

    window._on_worker_status("idle", "Not streaming")

    assert window._phone.pending_resolution is None
    assert window._fps_lbl.styleSheet() == ""


def test_resolution_pending_replaced_by_a_second_request(window):
    """Second resolution request cancels first pending timer."""
    window._on_resolution_pending(1280, 720)
    first_timer = window._phone.pending_resolution_timer

    window._on_resolution_pending(854, 480)

    assert not first_timer.isActive()
    assert window._phone.pending_resolution == (854, 480)

    window._on_worker_status("fps", "29.9 fps  854x480")
    assert window._phone.pending_resolution is None


def test_send_notification_uses_notify_send_on_linux(window, monkeypatch):
    popen_calls = []
    monkeypatch.setattr(app_module, "IS_LINUX", True)
    monkeypatch.setattr(app_module.shutil, "which", lambda _name: "/usr/bin/notify-send")
    monkeypatch.setattr(
        app_module.subprocess,
        "Popen",
        lambda *args, **kwargs: popen_calls.append((args, kwargs)),
    )
    window.send_notification("-Title", "--Body")
    # "--" ends notify-send's options, so text starting with a dash can't be read as one.
    assert popen_calls[0][0][0][-3:] == ["--", "-Title", "--Body"]


def test_send_notification_falls_back_to_tray(window, monkeypatch):
    messages = []
    monkeypatch.setattr(app_module, "IS_LINUX", False)
    window._tray = SimpleNamespace(showMessage=lambda *args: messages.append(args))
    window.send_notification("Title", "Body")
    assert messages[0][:2] == ("Title", "Body")


def test_tray_show_quit_and_activation(window, monkeypatch):
    calls = []
    monkeypatch.setattr(window, "showNormal", lambda: calls.append("show"))
    monkeypatch.setattr(window, "raise_", lambda: calls.append("raise"))
    monkeypatch.setattr(window, "activateWindow", lambda: calls.append("activate"))
    window._tray_show()
    assert calls == ["show", "raise", "activate"]

    monkeypatch.setattr(window, "isVisible", lambda: True)
    monkeypatch.setattr(window, "hide", lambda: calls.append("hide"))
    window._on_tray_activated(app_module.QSystemTrayIcon.ActivationReason.Trigger)
    assert calls[-1] == "hide"
    window._on_tray_activated(app_module.QSystemTrayIcon.ActivationReason.Context)
    assert calls[-1] == "hide"

    monkeypatch.setattr(window, "isVisible", lambda: False)
    window._on_tray_activated(app_module.QSystemTrayIcon.ActivationReason.Trigger)
    assert calls[-3:] == ["show", "raise", "activate"]

    monkeypatch.setattr(window, "_stop_all", lambda **_kw: calls.append("stop"))
    monkeypatch.setattr(app_module.QApplication, "quit", lambda: calls.append("quit"))
    window._tray_quit()
    assert window._tray_close_notified is True
    assert calls[-2:] == ["stop", "quit"]


def test_close_event_minimizes_active_stream_to_tray(window, monkeypatch):
    window._tray = object()
    window._session = StreamSession(source=window._phone, id=1, url="url", client=object(), worker=object())
    notifications = []
    monkeypatch.setattr(window, "hide", lambda: None)
    monkeypatch.setattr(window, "send_notification", lambda *args, **kw: notifications.append((args, kw)))

    event = SimpleNamespace(ignore=lambda: setattr(event, "ignored", True))
    window.closeEvent(event)
    assert event.ignored is True
    assert len(notifications) == 1
    assert notifications[0][1] == {"urgent": False}  # informational, shouldn't pin itself on screen
    window.closeEvent(event)
    assert len(notifications) == 1


def test_close_event_stops_and_accepts_without_background_stream(window, monkeypatch):
    calls = []
    monkeypatch.setattr(window, "_stop_all", lambda **_kw: calls.append("stop"))
    monkeypatch.setattr(app_module.QApplication, "quit", lambda: calls.append("quit"))
    event = SimpleNamespace(accept=lambda: calls.append("accept"))
    window.closeEvent(event)
    assert calls == ["stop", "accept", "quit"]


# ── Remote phone wake / symmetric stop ────────────────────────────────────────

def _real_spawn_wake(monkeypatch):
    # Capture spawn call to let test control when wake completes.
    spawned = []
    monkeypatch.setattr(
        sources_module.PhoneSource, "_spawn_wake",
        lambda self, *args: spawned.append(args),
    )
    return spawned


def test_start_wakes_the_phone_before_building_a_worker(window, monkeypatch):
    connection = _Connection()
    window.register_plugin(connection)
    built = []
    monkeypatch.setattr(sources_module, "PhoneControlClient", lambda _u, _t: object())
    monkeypatch.setattr(
        app_module, "StreamWorker",
        lambda **kwargs: built.append(kwargs) or SimpleNamespace(
            status=_Signal(), reconnected=_Signal(), vcam_opened=_Signal(), start=lambda: None,
        ),
    )
    monkeypatch.setattr(
        app_module.threading, "Thread",
        lambda **_kw: SimpleNamespace(start=lambda: None),
    )

    window._start()

    assert connection.wakes == 1
    assert built, "the stream was never built after a successful wake"


def test_start_asks_the_phone_to_open_at_the_size_picked(window, monkeypatch):
    # After a size too much for the encoder, the phone would otherwise open at it again.
    connection = _Connection()
    window.register_plugin(connection)
    output = _Plugin("stream_output")
    output.opening = lambda: {"width": 1920, "height": 1080, "fps": 60}
    window.register_plugin(output)
    spawned = _real_spawn_wake(monkeypatch)
    window._start()
    *_rest, opening = spawned[0]
    assert opening == {"width": 1920, "height": 1080, "fps": 60}
    window._phone._sig_wake_done.disconnect()  # only the wake itself: no stream to build here
    window._phone._wake_phone(*spawned[0])
    assert connection.opening == opening


def test_start_shows_the_reason_and_builds_nothing_when_the_wake_fails(window, monkeypatch):
    window.register_plugin(_Connection(wake=(False, "Open the app on your phone.")))
    monkeypatch.setattr(
        app_module, "StreamWorker",
        lambda **_kw: pytest.fail("no worker may be built when the phone never came up"),
    )
    failed = []
    window._bus.stream_start_failed.connect(lambda: failed.append(1))
    window._start()

    assert window._worker is None
    assert failed == [1]
    issue = window._banners.issue("start")
    assert issue.title == "Couldn't start the phone's camera"
    assert issue.text == "Open the app on your phone."
    assert [a.label for a in issue.actions] == ["Try again"]
    # Button must come back enabled so user can retry.
    assert window._start_btn.isEnabled()
    assert window._start_btn.text() == "Start Streaming"


def test_a_wake_that_lands_after_the_user_gave_up_is_discarded(window, monkeypatch):
    connection = _Connection()
    window.register_plugin(connection)
    spawned = _real_spawn_wake(monkeypatch)
    monkeypatch.setattr(
        app_module, "StreamWorker",
        lambda **_kw: pytest.fail("a cancelled start must not build a worker"),
    )

    window._start()
    assert spawned, "the wake was never spawned"
    wake_id, _conn, url, token, _target, _opening = spawned[0]

    window._stop()          # user hits Stop while the phone is still starting
    window._phone._on_wake_done(wake_id, True, "", url, token)

    assert window._worker is None


def test_a_wake_that_finishes_after_stop_stops_the_phone_again(window, monkeypatch):
    connection = _Connection()
    window.register_plugin(connection)
    spawned = _real_spawn_wake(monkeypatch)
    window._start()
    wake_id, _conn, url, token, _target, _opening = spawned[0]

    window._stop()  # its stop can reach the phone before the wake's start does
    window._phone._on_wake_done(wake_id, True, "", url, token)
    window._drain_phone_stops()

    assert connection.remote_stops == 2


def test_stop_takes_the_phone_s_camera_down_with_it(window, monkeypatch):
    connection = _Connection()
    window.register_plugin(connection)
    monkeypatch.setattr(
        app_module.threading, "Thread",
        lambda target, daemon=False: SimpleNamespace(start=target, is_alive=lambda: False),
    )
    window._session = StreamSession(source=window._phone, id=1, url="url", client=None, worker=None)

    window._stop()

    assert connection.remote_stops == 1


def test_stop_leaves_an_idle_phone_alone(window, monkeypatch):
    connection = _Connection()
    window.register_plugin(connection)
    monkeypatch.setattr(
        app_module.threading, "Thread",
        lambda target, daemon=False: SimpleNamespace(start=target, is_alive=lambda: False),
    )

    window._stop()  # nothing was ever started

    assert connection.remote_stops == 0


def test_a_cancelled_wake_still_stops_a_phone_that_may_be_mid_start(window, monkeypatch):
    connection = _Connection()
    window.register_plugin(connection)
    _real_spawn_wake(monkeypatch)
    monkeypatch.setattr(
        app_module.threading, "Thread",
        lambda target, daemon=False: SimpleNamespace(start=target, is_alive=lambda: False),
    )

    window._start()   # wake in flight, no session yet
    window._stop()

    assert connection.remote_stops == 1


def test_reconnect_and_canvas_reload_leave_the_phone_streaming(window, monkeypatch):
    # Desktop-side reconnect only; phone camera already live, so skip the re-open cost.
    connection = _Connection()
    window.register_plugin(connection)
    monkeypatch.setattr(
        app_module.threading, "Thread",
        lambda target, daemon=False: SimpleNamespace(start=target, is_alive=lambda: False),
    )
    monkeypatch.setattr(window, "_start", lambda **_kw: None)
    window._session = StreamSession(
        source=window._phone, id=1, url="url", client=None,
        worker=SimpleNamespace(
            status=_Signal(), reconnected=_Signal(), vcam_opened=_Signal(),
            request_stop=lambda: None, wait=lambda _ms: True,
        ),
    )
    window._session.worker.status.connect(window._on_worker_status)
    window._session.worker.reconnected.connect(window._on_stream_reconnected)
    window._session.worker.vcam_opened.connect(window._bus.vcam_opened)

    window.reconnect_stream()

    assert connection.remote_stops == 0


def test_drain_phone_stops_is_bounded(window, monkeypatch):
    # Gone-away phone must not hold app open during shutdown.
    joins = []

    class SlowThread:
        def __init__(self, target=None, daemon=False):
            pass

        def start(self):
            pass

        def is_alive(self):
            return True

        def join(self, timeout=None):
            joins.append(timeout)

    monkeypatch.setattr(app_module.threading, "Thread", SlowThread)
    window.register_plugin(_Connection())
    window._session = StreamSession(source=window._phone, id=1, url="url", client=None, worker=None)
    window._stop()

    window._drain_phone_stops(timeout=1.5)

    assert joins and joins[0] <= 1.5
    assert window._phone._stop_threads == []


def test_stop_stream_cancels_a_wake_that_is_still_in_flight(window, monkeypatch):
    # Re-pairing calls this, and it rotates the token the in-flight wake is
    # about to hand to a new StreamWorker.
    connection = _Connection()
    window.register_plugin(connection)
    spawned = _real_spawn_wake(monkeypatch)
    monkeypatch.setattr(
        app_module.threading, "Thread",
        lambda target, daemon=False: SimpleNamespace(start=target, is_alive=lambda: False),
    )
    monkeypatch.setattr(
        app_module, "StreamWorker",
        lambda **_kw: pytest.fail("a cancelled start must not build a worker"),
    )

    window._start()
    window.stop_stream()
    wake_id, _conn, url, token, _target, _opening = spawned[0]
    window._phone._on_wake_done(wake_id, True, "", url, token)

    assert window._worker is None


def test_a_start_problem_banner_clears_on_the_next_start_and_on_a_working_stream(window):
    from telescope.widgets.banner import Issue
    window.show_issue("start", Issue("Can't connect"))
    window.show_issue("vcam", Issue("Resize"))
    window.register_plugin(_Connection(wake=(False, "still closed")))
    window._start()
    assert window._banners.issue("start").text == "still closed"  # replaced, not stacked
    window._on_worker_status("ok", "Streaming")
    assert window._banners.keys() == []


def test_a_working_stream_keeps_the_note_on_why_it_fell_back_to_mjpeg(window):
    from telescope.widgets.banner import Issue
    window.show_issue("h264", Issue("Switched to Heavy", "The phone's H.264 encoder stopped."))
    window._on_worker_status("ok", "Streaming")
    assert window._banners.issue("h264") is not None


def _behind_events(window):
    events = []
    window._bus.stream_behind.connect(events.append)
    return events


def test_an_amber_throughput_readout_says_the_stream_is_behind_once_until_it_catches_up(window):
    events = _behind_events(window)
    window._on_worker_status("net", "12.0 Mbps")
    assert events == []  # keeping up from the start: nothing to say
    window._on_worker_status("net_warn", "40.0 Mbps")
    window._on_worker_status("net_warn", "41.0 Mbps")  # still behind: said once, so a dismissed note stays gone
    assert events == [True]
    assert window._net_lbl.styleSheet() == f"color: {theme.WARN};"
    window._on_worker_status("net", "12.0 Mbps")
    assert events == [True, False]
    assert window._net_lbl.styleSheet() == ""


def test_a_stream_that_stops_or_reconnects_is_no_longer_behind(window):
    events = _behind_events(window)
    window._on_worker_status("net_warn", "40.0 Mbps")
    window._stop()
    assert events == [True, False]
    window._on_worker_status("net_warn", "40.0 Mbps")
    window._on_worker_status("ok", "Streaming")  # the virtual camera reopened, e.g. for a new FPS
    assert events == [True, False, True, False]


def test_a_dropped_stream_is_not_called_slow(window, monkeypatch):
    events = _behind_events(window)
    conn = _RecoveringConnection([None])
    window.register_plugin(conn)
    window._on_worker_status("net_warn", "40.0 Mbps")
    _conn, worker, _client, _lost = _dropped_stream(window, monkeypatch, [None], conn=conn)
    assert events == [True, False]
    worker.status.emit("net_warn", "0.0 Mbps")  # no frames while it's down
    assert events == [True, False]
    worker.reconnected.emit()
    worker.status.emit("net_warn", "3.0 Mbps")  # this report still counts the time it was down
    assert events == [True, False]
    worker.status.emit("net_warn", "40.0 Mbps")  # really behind now
    assert events == [True, False, True]


def test_a_stream_back_from_a_drop_that_keeps_up_says_nothing(window, monkeypatch):
    events = _behind_events(window)
    _conn, worker, _client, _lost = _dropped_stream(window, monkeypatch, [None])
    worker.reconnected.emit()
    worker.status.emit("net_warn", "3.0 Mbps")
    worker.status.emit("net", "12.0 Mbps")
    assert events == []


def test_falling_behind_on_heavy_offers_light_and_switching_reconnects(window, monkeypatch):
    import telescope.plugins.stream_output as so
    monkeypatch.setattr(so.h264_reader, "available", lambda: True)
    plugin = so.StreamOutputPlugin()
    window.register_plugin(plugin)
    plugin.set_config({"format": "mjpeg"})
    reconnects = []
    monkeypatch.setattr(window, "reconnect_stream", lambda: reconnects.append(True))

    window._on_worker_status("net_warn", "40.0 Mbps")
    banner = window._banners.banner("behind")
    assert banner.issue.title == "Can't keep up" and banner.issue.text == "Try Light or lower quality."
    assert [b.text() for b in banner.buttons] == ["Switch to Light"]
    assert banner.issue.kind == "warn"

    banner.buttons[0].click()
    assert plugin.stream_format() == "h264" and plugin._fmt_h264.isChecked()
    assert reconnects == [True]
    assert window._banners.issue("behind") is None


def test_the_can_t_keep_up_note_goes_once_the_stream_keeps_up(window, monkeypatch):
    import telescope.plugins.stream_output as so
    monkeypatch.setattr(so.h264_reader, "available", lambda: True)
    plugin = so.StreamOutputPlugin()
    window.register_plugin(plugin)  # Light, the default
    window._on_worker_status("net_warn", "8.0 Mbps")
    banner = window._banners.banner("behind")
    assert banner.issue.text == "Try Dynamic or lower quality."  # already on Light
    assert [b.text() for b in banner.buttons] == ["Switch to Dynamic"]
    window._on_worker_status("net", "8.0 Mbps")
    assert window._banners.issue("behind") is None

    window._on_worker_status("net_warn", "8.0 Mbps")
    window._banners.banner("behind").buttons[0].click()
    assert plugin.get_config()["bitrate_mbps"] == -1 and window._banners.issue("behind") is None


def test_waiting_to_start_by_itself_keeps_running_in_the_tray(window, monkeypatch):
    window._tray = object()
    window.set_keep_in_tray(True)
    notes = []
    monkeypatch.setattr(window, "hide", lambda: None)
    monkeypatch.setattr(window, "send_notification", lambda title, body, **kw: notes.append(body))
    event = SimpleNamespace(ignore=lambda: setattr(event, "ignored", True))
    window.closeEvent(event)
    assert event.ignored is True
    assert "starts streaming by itself" in notes[0]


def test_start_hidden_minimizes_without_a_tray(window, monkeypatch):
    calls = []
    monkeypatch.setattr(window, "showMinimized", lambda: calls.append("min"))
    window._tray = object()
    window.start_hidden()
    assert calls == []
    window._tray = None
    window.start_hidden()
    assert calls == ["min"]


def test_start_stream_passes_on_that_nobody_asked(window):
    conn = _Connection(wake=(False, "x"))
    seen = []
    conn.get_stream_info = lambda interactive=True, pid=None: seen.append(interactive) or (None, None, False)
    window.register_plugin(conn)
    window.start_stream(interactive=False)
    assert seen == [False]


def test_diagnostics_report_gathers_plugins_and_recent_status(window, monkeypatch):
    from telescope import diagnostics
    monkeypatch.setattr(diagnostics, "events", diagnostics.EventLog())

    class Reports(_Plugin):
        selected_device = None  # registered as the connection, which says which phone is picked

        def diagnostics(self):
            return {"Connection": "usb"}

    class Broken(_Plugin):
        def diagnostics(self):
            raise KeyError("gone")

    window.register_plugin(Reports("connection", {}))
    window.register_plugin(Broken("broken", {}))
    window._set_status("Can't reach the phone", "err")

    lines = window.diagnostics_report().splitlines()
    assert "Streaming: no" in lines
    assert "Connection: usb" in lines
    assert "broken: couldn't read (KeyError)" in lines
    assert any(line.endswith("NOTE Status: Can't reach the phone") for line in lines)


# ── A dropped stream finding its way back ────────────────────────────────────

class _RecoveringConnection(_Connection):
    """Answers recovery probes from a queue; hands out one URL per route."""

    def __init__(self, answers):
        super().__init__()
        self.answers = list(answers)
        self.adopted = []
        self.problems = []

    def show_problem(self, res, pid=None):
        self.problems.append(res.status)

    def recovery_probe(self, pid=None):
        answer = self.answers.pop(0) if self.answers else None
        return lambda: answer

    def adopt_stream_route(self, route, pid=None):
        self.adopted.append(route)
        return f"http://{route.host}:8080/v1/video"


class _RetargetWorker:
    def __init__(self):
        self.status, self.reconnected, self.vcam_opened = _Signal(), _Signal(), _Signal()
        self.auth = "tok"
        self.urls = []

    def retarget(self, url):
        self.urls.append(url)

    def request_stop(self):
        pass

    def wait(self, _ms):
        return True


class _Client:
    def __init__(self):
        self.closed = False
        self.urls = []

    def close(self):
        self.closed = True

    def retarget(self, url):
        self.urls.append(url)


def _dropped_stream(window, monkeypatch, answers, url="http://127.0.0.1:40001/v1/video", conn=None):
    if conn is None:
        conn = _RecoveringConnection(answers)
        window.register_plugin(conn)
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_recovery_probe",
                        lambda self, sid, gen, job: self._on_recovery_probed(sid, gen, job()))
    worker, client = _RetargetWorker(), _Client()
    window._session = StreamSession(source=window._phone, id=1, url=url, client=client, worker=worker)
    worker.status.connect(window._on_worker_status)
    worker.reconnected.connect(window._on_stream_reconnected)
    worker.vcam_opened.connect(window._bus.vcam_opened)
    lost = []
    window._bus.stream_lost.connect(lambda: lost.append(True))
    worker.status.emit("reconnecting", "Stream dropped - reconnecting")
    return conn, worker, client, lost


def test_a_dropped_stream_asks_the_phone_why_quietly(window, monkeypatch):
    _conn, _worker, _client, lost = _dropped_stream(window, monkeypatch, [None])
    assert lost == [True] and window.state_fetches == [1]  # H.264 it can't do: its state says so
    emitted = []
    window._phone._sig_state.connect(lambda sid, state: emitted.append(state))
    answers = [None, {**_VALID_STATE, "codec": "h264"}, {**_VALID_STATE, "codec_error": "Too big"}]
    window._session = replace(window._session, client=SimpleNamespace(get_state=lambda: answers.pop(0)))
    window._phone._poll_codec_state(1)  # got nothing: mid-stream that's no news, not an empty state
    window._phone._poll_codec_state(1)  # nothing about why it stopped: plugins keep what they have
    assert emitted == []
    window._phone._poll_codec_state(1)
    assert len(emitted) == 1 and emitted[0]["codec_error"] == "Too big"
    assert window._phone._state_poll_busy is False


def test_a_camera_another_app_took_is_named_until_the_stream_is_back(window, monkeypatch):
    _conn, _worker, _client, _lost = _dropped_stream(window, monkeypatch, [None])
    taken = {**_VALID_STATE, "camera_taken": True}
    window._session = replace(window._session, client=SimpleNamespace(get_state=lambda: taken))
    window._phone._poll_codec_state(window._session.id)
    key = f"camera_taken:{window._phone.id}"
    assert window._banners.issue(key).title == "Another app on the phone is using the camera"
    window._on_stream_reconnected()
    assert window._banners.issue(key) is None


def test_a_dropped_stream_keeps_asking_the_phone_why(window, monkeypatch):
    # The camera can take seconds to give up on a size it can't do, after the stream already stopped.
    _dropped_stream(window, monkeypatch, [None, None])
    window._phone._recovery_timer.stop()
    window._phone._probe_recovery()
    assert window.state_fetches == [1, 1]
    window._on_stream_reconnected()
    window._phone._probe_recovery()
    assert window.state_fetches == [1, 1]  # back: nothing more to ask


def _slow_stream(window, monkeypatch, camera_fps, arrival=46.8):
    """A stream whose frames arrive under the target, and a phone that answers with its camera's rate."""
    asked = []

    def get_state():
        asked.append(True)
        return None if camera_fps is None else {**_VALID_STATE, "camera_fps": camera_fps}

    worker = _RetargetWorker()
    worker.last_arrival_fps = arrival
    window._session = StreamSession(source=window._phone, id=1, url="http://127.0.0.1:40001/v1/video",
                                    client=SimpleNamespace(get_state=get_state, close=lambda: None), worker=worker)
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_camera_check",
                        lambda self, sid, fps: self._check_camera_rate(sid, fps))
    return asked


def test_a_camera_making_fewer_frames_is_not_called_a_slow_link(window, monkeypatch):
    events = _behind_events(window)
    asked = _slow_stream(window, monkeypatch, camera_fps=47.1)  # 1080p60 asked, dim desk: the camera does 47
    window._on_worker_status("net_warn", "0.9 Mbps")
    window._on_worker_status("net_warn", "0.9 Mbps")
    assert events == [] and window._net_lbl.styleSheet() == ""
    assert "Dim light" in window._fps_lbl.toolTip()
    assert asked == [True]  # the answer holds for a while
    window._on_worker_status("net", "0.9 Mbps")
    assert window._fps_lbl.toolTip() == ""
    window._session.worker.last_arrival_fps = 30.0  # the link got worse too: the camera's rate still holds, but
    window._on_worker_status("net_warn", "0.9 Mbps")  # frames go missing on the way now
    assert events == [True] and asked == [True]


def test_frames_lost_between_camera_and_computer_are_a_slow_link(window, monkeypatch):
    events = _behind_events(window)
    _slow_stream(window, monkeypatch, camera_fps=60.0)
    window._on_worker_status("net_warn", "40.0 Mbps")
    assert events == [True] and window._net_lbl.styleSheet() == f"color: {theme.WARN};"
    assert window._fps_lbl.toolTip() == ""


@pytest.mark.parametrize("camera_fps", [None, 0.0])  # no answer, or a phone too old to say
def test_without_the_camera_rate_a_slow_stream_is_behind_as_before(window, monkeypatch, camera_fps):
    events = _behind_events(window)
    _slow_stream(window, monkeypatch, camera_fps=camera_fps)
    window._on_worker_status("net_warn", "40.0 Mbps")
    assert events == [True]


def test_the_camera_rate_answer_expires(window, monkeypatch):
    asked = _slow_stream(window, monkeypatch, camera_fps=47.1)
    window._on_worker_status("net_warn", "0.9 Mbps")
    window._phone._camera_fps_until = 0.0
    window._on_worker_status("net_warn", "0.9 Mbps")
    assert asked == [True, True]


def test_a_slow_stream_waits_for_the_phone_and_a_late_answer_after_it_caught_up_says_nothing(window, monkeypatch):
    events = _behind_events(window)
    _slow_stream(window, monkeypatch, camera_fps=60.0)
    checks = []
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_camera_check",
                        lambda self, sid, fps: checks.append((sid, fps)))
    window._on_worker_status("net_warn", "40.0 Mbps")
    window._on_worker_status("net_warn", "40.0 Mbps")
    assert checks == [(1, 46.8)] and events == []  # asked once, nothing said yet
    window._on_worker_status("net", "40.0 Mbps")
    window._phone._on_camera_rate(1, 46.8, 60.0)
    assert events == []
    window._on_worker_status("net_warn", "40.0 Mbps")  # the answer is in: no need to ask again
    assert checks == [(1, 46.8)] and events == [True]


def test_h264_too_much_for_the_phone_stops_the_stream_with_a_note_that_stays(window, monkeypatch):
    import telescope.plugins.stream_output as so
    from PyQt6.QtCore import QCoreApplication
    monkeypatch.setattr(so.h264_reader, "available", lambda: True)
    plugin = so.StreamOutputPlugin()
    window.register_plugin(plugin)
    _conn, _worker, _client, _lost = _dropped_stream(window, monkeypatch, [None])
    window._apply_state(1, {**_VALID_STATE, "codecs": ["mjpeg", "h264"], "codec": "mjpeg", "codec_unsupported": True,
                            "codec_error": "H.264 isn't available at 4096x3072 on this phone"})
    QCoreApplication.processEvents()
    assert window._session is None  # stopped
    assert plugin.stream_format() == "h264"
    assert window._banners.issue("encoder").title == "Too much for the phone's H.264 encoder"


def test_a_dropped_usb_stream_moves_to_wifi_when_the_cable_is_pulled(window, monkeypatch):
    from telescope.phones import READY, Resolution, Route
    wifi = Route("wifi", "192.168.1.20")
    conn, worker, client, lost = _dropped_stream(
        window, monkeypatch, [Resolution(READY, wifi, streaming=True)])

    assert lost == [True]
    assert conn.adopted == [wifi]
    assert worker.urls == ["http://192.168.1.20:8080/v1/video"]
    assert window._session.url == "http://192.168.1.20:8080/v1/video"
    assert window._session.client is client and client.urls == ["http://192.168.1.20:8080/v1/video"]
    assert not client.closed  # the same one, so it keeps what it sent
    assert window._phone._recovery_timer.isActive()  # until frames actually come back


def test_recovery_keeps_looking_while_the_phone_is_unreachable(window, monkeypatch):
    from telescope.phones import READY, UNREACHABLE, Resolution, Route
    usb = Route("usb", "127.0.0.1", "SER")
    conn, worker, _client, _lost = _dropped_stream(
        window, monkeypatch, [Resolution(UNREACHABLE), None, Resolution(READY, usb, streaming=True)])

    assert conn.adopted == [] and window._phone._recovery_timer.isActive()
    window._phone._probe_recovery()  # the probe itself failed
    assert conn.adopted == []
    window._phone._probe_recovery()  # plugged back in: a fresh forward
    assert conn.adopted == [usb]
    assert worker.urls == ["http://127.0.0.1:8080/v1/video"]


def test_recovery_moves_the_stream_once_per_route(window, monkeypatch):
    from telescope.phones import READY, Resolution, Route
    wifi = Route("wifi", "192.168.1.20")
    conn, worker, _client, _lost = _dropped_stream(
        window, monkeypatch, [Resolution(READY, wifi, streaming=True)] * 2)

    window._phone._probe_recovery()

    assert conn.adopted == [wifi]  # the worker keeps retrying it on its own


def test_recovery_ends_when_frames_come_back(window, monkeypatch):
    conn, worker, _client, _lost = _dropped_stream(window, monkeypatch, [None])

    window._on_stream_reconnected()

    assert not window._phone.recovering and not window._phone._recovery_timer.isActive()
    window._phone._probe_recovery()
    assert conn.adopted == []


def test_a_probe_from_before_the_stream_came_back_is_dropped(window, monkeypatch):
    from telescope.phones import READY, Resolution, Route
    conn, worker, _client, _lost = _dropped_stream(window, monkeypatch, [None])
    gen = window._phone._recovery_gen
    window._on_stream_reconnected()

    window._phone._on_recovery_probed(1, gen, Resolution(READY, Route("wifi", "10.0.0.2"), streaming=True))

    assert conn.adopted == [] and worker.urls == []


def test_recovery_stops_the_stream_when_the_phone_stopped_streaming(window, monkeypatch):
    from telescope.phones import READY, Resolution, Route
    conn, _worker, _client, _lost = _dropped_stream(
        window, monkeypatch, [Resolution(READY, Route("wifi", "192.168.1.20"), streaming=False)])

    assert window._session is None
    assert conn.remote_stops == 0
    assert not window._phone.recovering
    issue = window._banners.issue(f"stopped:{window._phone.id}")
    assert issue.title == "The phone stopped streaming"
    starts = []
    monkeypatch.setattr(window, "_start_source", lambda sid, before=None: starts.append(sid))
    issue.actions[0].callback()
    assert starts == [window._phone.id]  # this phone, even with others still streaming


def test_recovery_leaves_a_stream_another_computer_started_alone(window, monkeypatch):
    from telescope.phones import READY, Resolution, Route
    conn, worker, _client, _lost = _dropped_stream(window, monkeypatch, [
        Resolution(READY, Route("wifi", "192.168.1.20"), streaming=True, streaming_for="Office PC")])

    assert window._session is None
    assert conn.adopted == [] and worker.urls == [] and conn.remote_stops == 0
    assert not window._phone.recovering
    assert window._banners.issue(f"stopped:{window._phone.id}").title == "The phone is streaming to Office PC"


@pytest.mark.parametrize("status", ["NOT_PAIRED", "LOCAL_ONLY", "PHONE_OUTDATED", "DESKTOP_OUTDATED"])
def test_recovery_stops_and_says_why_when_the_phone_wont_take_the_stream_back(window, monkeypatch, status):
    import telescope.phones as phones
    conn, _worker, _client, _lost = _dropped_stream(window, monkeypatch, [phones.Resolution(getattr(phones, status))])

    assert conn.problems == [getattr(phones, status)]
    assert window._session is None and not window._phone.recovering
    assert not window._phone._recovery_timer.isActive()


def test_a_stream_that_never_got_its_first_frame_looks_for_the_phone_too(window, monkeypatch):
    from telescope.phones import READY, Resolution, Route
    wifi = Route("wifi", "192.168.1.20")
    conn = _RecoveringConnection([Resolution(READY, wifi, streaming=True)])
    window.register_plugin(conn)
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_recovery_probe",
                        lambda self, sid, gen, job: self._on_recovery_probed(sid, gen, job()))
    worker = _RetargetWorker()
    window._session = StreamSession(source=window._phone, id=1, url="http://127.0.0.1:40001/v1/video", client=_Client(), worker=worker)
    worker.status.connect(window._on_worker_status)

    worker.status.emit("waiting", "Can't reach the phone's stream. Trying again in 3 s…")

    assert conn.adopted == [wifi]
    assert worker.urls == ["http://192.168.1.20:8080/v1/video"]


def test_recovery_waits_while_the_phone_is_mid_start(window, monkeypatch):
    from telescope.phones import READY, Resolution, Route
    conn, _worker, _client, _lost = _dropped_stream(
        window, monkeypatch, [Resolution(READY, Route("wifi", "192.168.1.20"), busy=True)])

    assert window._session is not None and conn.adopted == []
    assert window._phone._recovery_timer.isActive()


class _ParkingWorker(_RetargetWorker):
    parked = False

    def set_parked(self, parked):
        self.parked = parked


class _WaitScreen(_Plugin):
    def __init__(self):
        super().__init__("wait_screen", panel=False)
        self.starting = 0

    def on_stream_starting(self):
        self.starting += 1


def _gone_stream(window, monkeypatch):
    """A phone stream that dropped and whose phone doesn't answer."""
    from telescope.phones import UNREACHABLE, Resolution
    wait = _WaitScreen()
    window.register_plugin(wait)
    window.register_plugin(_RecoveringConnection([Resolution(UNREACHABLE)] * 2))
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_recovery_probe",
                        lambda self, sid, gen, job: self._on_recovery_probed(sid, gen, job()))
    worker = _ParkingWorker()
    window._session = StreamSession(source=window._phone, id=1, url="http://127.0.0.1:40001/v1/video",
                                    client=_Client(), worker=worker)
    worker.status.connect(window._on_worker_status)
    worker.reconnected.connect(window._on_stream_reconnected)
    idle = []
    window._bus.idle_outputs.connect(idle.append)
    worker.status.emit("reconnecting", "Stream dropped - reconnecting")
    return worker, idle, wait


def test_a_phone_gone_a_while_gets_the_wait_screen_then_a_banner_until_it_is_back(window, monkeypatch):
    worker, idle, wait = _gone_stream(window, monkeypatch)
    phone = window._phone
    assert phone._park_timer.isActive() and phone._park_timer.interval() == 10_000
    assert phone._gone_timer.isActive() and phone._gone_timer.interval() == 30_000
    assert not worker.parked

    window._park_stream(phone)  # 10 s on
    assert worker.parked and 0 in idle[-1]  # the wait screen takes the camera, not the last frame
    phone._show_unreachable()  # 30 s on
    issue = window._banners.issue(f"stopped:{phone.id}")
    assert issue.title == "Can't reach the phone" and issue.text == "Open Telescope on it."
    assert phone.recovering and phone._recovery_timer.isActive()  # still looking for it

    worker.reconnected.emit()  # back
    assert not worker.parked and wait.starting == 1 and 0 not in idle[-1]
    assert window._banners.issue(f"stopped:{phone.id}") is None
    assert not phone._park_timer.isActive() and not phone._gone_timer.isActive()


def test_a_phone_back_before_the_wait_screen_keeps_its_camera(window, monkeypatch):
    worker, idle, wait = _gone_stream(window, monkeypatch)
    worker.reconnected.emit()
    window._park_stream(window._phone)  # a timeout that was already on its way
    assert not worker.parked and wait.starting == 0


def test_stopping_a_phone_that_cant_be_reached_takes_its_banner_away(window, monkeypatch):
    worker, _idle, _wait = _gone_stream(window, monkeypatch)
    phone = window._phone
    window._park_stream(phone)
    phone._show_unreachable()
    window._stop()
    assert window._banners.issue(f"stopped:{phone.id}") is None
    assert not phone._park_timer.isActive() and not phone._gone_timer.isActive()
    phone._show_unreachable()  # late: says nothing about a stream that's over
    assert window._banners.issue(f"stopped:{phone.id}") is None


def test_a_plugin_hook_that_raises_does_not_stop_the_others(window):
    class Broken(_Plugin):
        def on_stream_stop(self):
            raise RuntimeError("boom")

        broken_config = False

        def get_config(self):
            if self.broken_config:
                raise RuntimeError("boom")
            return {}

    heard = []
    first, last = Broken("first", {}), _Plugin("last", {})
    last.on_stream_stop = lambda: heard.append("last")
    window.register_plugin(first)
    window.register_plugin(last)
    first.broken_config = True

    window._each_plugin("on_stream_stop")
    window.save_now()  # the broken plugin's get_config doesn't stop the save

    assert heard == ["last"]


def test_a_frame_step_that_keeps_failing_is_passed_through_then_skipped():
    from telescope.stream import STEP_FAILS_BEFORE_SKIP, guarded_step
    calls = []

    def broken(frame):
        calls.append(1)
        raise RuntimeError("boom")

    step = guarded_step("broken", broken)
    frame = np.zeros((4, 4, 3), np.uint8)
    for _ in range(STEP_FAILS_BEFORE_SKIP + 10):
        assert step(frame) is frame
    assert len(calls) == STEP_FAILS_BEFORE_SKIP
    assert guarded_step("gray", lambda f: f[..., 0])(frame) is frame  # a mangled frame doesn't go on


def test_a_lens_switch_or_new_size_lets_the_stream_settle_before_it_counts_as_behind(window, monkeypatch):
    settled = []
    worker = SimpleNamespace(settle=lambda: settled.append(True))
    window._session = StreamSession(source=window._phone, id=1, url="http://127.0.0.1:40001/v1/video", client=_Client(), worker=worker)
    window._bus.camera_switched.emit({"id": "2"})
    window._bus.resolution_change_requested.emit(1920, 1080)
    assert settled == [True, True]
    window._session = None
    window._bus.camera_switched.emit({"id": "0"})  # not streaming: nothing to settle
    assert settled == [True, True]


def test_plugin_tray_actions_go_between_show_and_quit(window):
    menu = QMenu()
    menu.addAction("Show")
    window._tray_quit_sep = menu.addSeparator()
    menu.addAction("Quit")
    window._tray = SimpleNamespace(contextMenu=lambda: menu)

    class _Muting(TelescopePlugin):
        def create_tray_actions(self):
            self.action = QAction("Mute microphone")
            return [self.action]

    plugin = _Muting()
    window.register_plugin(plugin)
    assert [a.text() for a in menu.actions()] == ["Show", "Mute microphone", "", "Quit"]
    assert plugin.action.parent() is menu


# ── Camera off: the stream carries on with just the mic ───────────────────────

class _Watcher(_Plugin):
    """Records the camera hooks, and owns the microphone card's on/off."""

    def __init__(self, mic=True):
        super().__init__("microphone", {"enabled": mic})
        self.calls = []

    def on_stream_starting(self):
        self.calls.append("starting")

    def on_camera_off(self):
        self.calls.append("off")

    def on_camera_on(self):
        self.calls.append("on")


@pytest.fixture
def camera_env(window, monkeypatch):
    connection, mic = _Connection(), _Watcher()
    for plugin in (connection, _StreamOutput(), _Setup(), mic):
        window.register_plugin(plugin)
    clients, workers = [], []

    class Client:
        def __init__(self, url, auth):
            self.url, self.auth, self.sent, self.closed = url, auth, [], False
            self.state = {"camera_toggle": True}
            clients.append(self)

        def send(self, **params):
            self.sent.append(params)

        def retarget(self, url):
            self.url = url

        def get_state(self):
            return self.state

        def close(self):
            self.closed = True

    class Worker:
        def __init__(self, **kwargs):
            self.kwargs, self.auth = kwargs, kwargs["auth"]
            self.status, self.reconnected, self.vcam_opened = _Signal(), _Signal(), _Signal()
            self.started = self.stopped = False
            workers.append(self)

        def start(self):
            self.started = True

        def request_stop(self):
            self.stopped = True

        def wait(self, _ms):
            return True

        def set_pipeline(self, steps):
            self.pipelines = getattr(self, "pipelines", []) + [steps]

        def latest_frame(self):
            return None

        parked = False

        def set_parked(self, parked):
            self.parked = parked

    class Thread:
        def __init__(self, target, args=(), daemon=False):
            pass

        def start(self):
            pass

        def is_alive(self):
            return False

    monkeypatch.setattr(sources_module, "PhoneControlClient", Client)
    monkeypatch.setattr(app_module, "StreamWorker", Worker)
    monkeypatch.setattr(app_module.threading, "Thread", Thread)
    changes = []
    window._bus.camera_on_changed.connect(changes.append)
    return window, connection, mic, clients, workers, changes


def test_the_camera_goes_off_and_on_while_the_stream_and_mic_carry_on(camera_env):
    window, _conn, mic, clients, workers, changes = camera_env
    window._start()
    assert workers[0].started and mic.calls == ["starting"]

    window.set_camera_on(False)
    assert clients[0].sent == [{"action": "camera_on", "value": 0}]
    assert workers[0].stopped and window._worker is None
    assert window.is_streaming() and not window.is_camera_on()
    assert mic.calls[-1] == "off" and changes == [False]
    assert window._status_lbl.text() == "Streaming the mic, camera off"
    assert window._phone._alive_timer.isActive()  # no video to say the phone went away

    window.set_camera_on(True)
    assert clients[0].sent[-1] == {"action": "camera_on", "value": 1}
    assert len(workers) == 2 and workers[1].started and window._worker is workers[1]
    assert mic.calls[-2:] == ["starting", "on"]  # the wait screen lets go before the new worker opens the camera
    assert changes == [False, True] and not window._phone._alive_timer.isActive()


def test_the_camera_stays_on_without_the_mic(camera_env):
    window, _conn, mic, clients, workers, changes = camera_env
    mic.config["enabled"] = False
    window._start()
    assert window.can_turn_camera_off()[0] is False
    window.set_camera_on(False)
    assert window.is_camera_on() and clients[0].sent == [] and changes == []


def test_start_mic_only_opens_the_phone_with_its_camera_off(camera_env):
    window, conn, mic, clients, workers, changes = camera_env
    window.set_camera_on(False)
    assert changes == [False] and window._start_btn.text() == "Start mic only"
    window._start()
    assert conn.opening["camera"] == "off"
    assert workers == [] and window.is_streaming() and mic.calls == ["off"]
    assert window._start_btn.text() == "Stop Streaming"
    window._stop()
    assert window.is_camera_on() and changes == [False, True]  # the next stream starts with the camera on
    assert window._start_btn.text() == "Start Streaming"


def test_a_restart_keeps_the_camera_off(camera_env):
    window, _conn, _mic, clients, workers, _changes = camera_env
    window._start()
    window.set_camera_on(False)
    window.reconnect_stream()
    assert not window.is_camera_on() and len(workers) == 1  # no video worker for the restarted stream either


def test_a_phone_left_with_its_camera_on_or_off_is_matched(camera_env):
    window, _conn, _mic, clients, _workers, _changes = camera_env
    window._start()
    window._apply_state(window._session.id, {**_VALID_STATE, "camera_toggle": True, "camera_off": True})
    assert clients[0].sent == [{"action": "camera_on", "value": 1}]


def test_an_older_phone_that_cant_turn_its_camera_off_gets_the_camera_back(camera_env):
    window, _conn, _mic, clients, workers, changes = camera_env
    window.set_camera_on(False)
    window._start()
    window._apply_state(window._session.id, _VALID_STATE)  # no camera_toggle: it opened its camera anyway
    assert window.is_camera_on() and len(workers) == 1 and changes == [False, True]
    assert window._banners.issue("camera") is not None
    assert window.can_turn_camera_off()[0] is False


def test_a_camera_that_wont_come_back_on_stays_off_and_says_why(camera_env):
    window, _conn, _mic, clients, workers, changes = camera_env
    window._start()
    window.set_camera_on(False)
    window.set_camera_on(True)
    window._phone._on_camera_check(window._session.id, {"camera_toggle": True, "camera_off": True,
                                                 "camera_error": "Another app may be using it."})
    assert not window.is_camera_on() and window._worker is None and workers[1].stopped
    assert changes == [False, True, False] and window._banners.issue("camera") is not None


def test_switching_the_mic_off_brings_the_camera_back(camera_env):
    window, _conn, mic, _clients, workers, changes = camera_env
    window._start()
    window.set_camera_on(False)
    mic.config["enabled"] = False
    window._bus.mic_changed.emit(False, False)
    assert window.is_camera_on() and len(workers) == 2


def test_with_the_camera_off_a_phone_that_stops_answering_is_looked_for(camera_env, monkeypatch):
    window, _conn, _mic, _clients, _workers, _changes = camera_env
    window._start()
    window.set_camera_on(False)
    probes = []
    monkeypatch.setattr(window, "_begin_recovery", lambda _source=None: probes.append(True))
    sid = window._session.id
    window._phone._on_alive(sid, False)
    assert probes == []  # one miss can be a blip
    window._phone._on_alive(sid, False)
    assert probes == [True]
    window._phone.recovering = True
    reconnects = []
    monkeypatch.setattr(window, "_on_stream_reconnected", lambda _source=None: reconnects.append(True))
    window._phone._on_alive(sid, True)
    assert reconnects == [True] and window._phone._alive_misses == 0


def test_the_tray_icon_says_what_streams(camera_env):
    window, _conn, _mic, _clients, _workers, _changes = camera_env

    class Tray:
        tips, icons = [], 0

        def setIcon(self, _icon):
            Tray.icons += 1

        def setToolTip(self, tip):
            Tray.tips.append(tip)

    window._tray = Tray()
    window._bus.mic_changed.emit(True, False)
    window._start()
    window.set_camera_on(False)
    window._bus.mic_changed.emit(True, True)
    window._stop()
    assert Tray.tips == ["Telescope", "Telescope: streaming the camera and mic",
                         "Telescope: streaming the mic, camera off", "Telescope: streaming the mic, camera off (muted)",
                         "Telescope"]
    window._tray = None


def test_a_browser_camera_streams_with_its_camera_on(camera_env):
    window, conn, mic, clients, workers, changes = camera_env

    class Source:
        id, name, url = "browser", "Browser camera", "browser://camera"

        def prepare(self, interactive):
            return True

        def open_reader(self):
            return None

        def control_client(self):
            return clients[0] if clients else None

        def fps(self):
            return 30

    conn.selected_device = "browser"
    conn.ensure_virtual_camera = lambda interactive=True: True
    window.add_stream_source(Source())
    assert window.can_turn_camera_off()[0] is False  # only a phone can keep its mic going alone
    window.set_camera_on(False)
    assert window.is_camera_on() and changes == []
    window._start()
    assert len(workers) == 1 and workers[0].kwargs["open_reader"] is not None
    assert window.can_turn_camera_off()[0] is False


class _Browser:
    id, name, url = "browser", "Browser camera", "browser://camera"

    def prepare(self, interactive):
        return True

    def open_reader(self):
        return None

    def control_client(self):
        return SimpleNamespace(send=lambda **_kw: None, get_state=lambda: None, close=lambda: None)

    def fps(self):
        return 30


class _OneBrowser(_Browser):
    remember_changes_only = True

    def __init__(self, bid):
        self.id, self.name = f"browser:{bid}", bid


def test_a_browser_starts_by_itself_and_goes_when_it_leaves(camera_env):
    window, conn, _mic, _clients, workers, _changes = camera_env
    conn.ensure_virtual_camera = lambda interactive=True: True
    launcher = _Browser()
    launcher.redirect = lambda: "browser:pixel"
    window.add_stream_source(launcher)
    window.add_stream_source(_OneBrowser("pixel"))
    conn.selected_device = "browser"
    window._start()  # Start with Browser camera picked streams the browser that's there
    assert conn.selected_device == "browser:pixel" and window.is_streaming_from("browser:pixel")
    window.remove_stream_source("browser:pixel")
    assert not window.is_streaming() and conn.selected_device == "browser"
    assert "browser:pixel" not in window._sources

    window.add_stream_source(_OneBrowser("ipad"))
    window.stream_source("browser:ipad")  # nothing streams: it's picked and streamed
    assert conn.selected_device == "browser:ipad" and window.stream_output("browser:ipad") == app_module.vcam.slot_label(0)


def test_a_phone_back_with_a_new_id_takes_its_settings_and_virtual_camera(window, config_home):
    cfg = config_home.load_config()
    presets = {"presets": {"items": [{"name": "Desk"}]}}
    cfg["devices"] = {"old": {"plugin_configs": presets}, "other": {"plugin_configs": {}}}
    cfg["output_slots"] = {"old": 2}
    cfg["selected_device"] = "old"
    config_home.save_config(cfg)
    window._slot_memory = {"old": 2}
    window.move_device_settings("old", "new")
    cfg = config_home.load_config()
    assert cfg["devices"]["new"]["plugin_configs"] == presets and "old" not in cfg["devices"]
    assert cfg["output_slots"] == {"new": 2} and cfg["selected_device"] == "new"
    assert window._slot_memory == {"new": 2}


def test_a_browser_keeps_only_the_settings_that_changed(camera_env, config_home):
    window, conn, _mic, _clients, _workers, _changes = camera_env
    view = _Plugin("transforms", {"zoom": 1})
    window.register_plugin(view)
    window.add_stream_source(_OneBrowser("guest"))
    window.add_stream_source(_OneBrowser("pixel"))
    window._save_profile("browser:guest", [view])
    assert not window.has_device_settings("browser:guest")
    view.config["zoom"] = 2
    window._save_profile("browser:pixel", [view])
    assert config_home.load_config()["devices"]["browser:pixel"]["plugin_configs"]["transforms"] == {"zoom": 2}
    window.forget_device_settings("browser:pixel")
    assert not window.has_device_settings("browser:pixel")


def test_each_source_keeps_its_own_camera_choice(camera_env):
    window, conn, _mic, _clients, _workers, changes = camera_env
    window.add_stream_source(_Browser())
    window.set_camera_on(False)
    conn.selected_device = "browser"
    window._bus.source_selected.emit("browser")
    assert window.is_camera_on() and changes == [False, True]
    assert window._start_btn.text() == "Start Streaming"
    conn.selected_device = "Phone"
    window._bus.source_selected.emit("")
    assert not window.is_camera_on() and changes == [False, True, False]
    assert window._start_btn.text() == "Start mic only"


def test_a_dropped_browser_stream_waits_for_it_without_looking_for_a_phone(camera_env):
    window, conn, _mic, _clients, workers, _changes = camera_env
    conn.selected_device = "browser"
    conn.ensure_virtual_camera = lambda interactive=True: True
    window.add_stream_source(_Browser())
    window._start()
    lost = []
    window._bus.stream_lost.connect(lambda: lost.append(True))
    workers[0].status.emit("reconnecting", "Stream dropped - reconnecting")  # _Connection can't probe: it would raise
    assert lost == [True] and window.state_fetches == [] and window._session is not None
    window._stop()
    assert conn.remote_stops == 0  # nothing on a phone to stop


def test_a_browser_gone_a_while_gets_the_wait_screen_and_a_banner(camera_env):
    window, conn, mic, _clients, workers, _changes = camera_env
    conn.selected_device = "browser"
    conn.ensure_virtual_camera = lambda interactive=True: True
    window.add_stream_source(_Browser())
    window._start()
    workers[0].status.emit("reconnecting", "Stream dropped - reconnecting")
    source = window._sources["browser"]
    assert source._park_timer.isActive()
    window._park_stream(source)
    assert workers[0].parked
    source._show_unreachable()
    issue = window._banners.issue("stopped:browser")
    assert issue.title == "Can't reach Browser camera" and issue.text == "Open the Telescope page in it again."
    starting = mic.calls.count("starting")
    workers[0].reconnected.emit()
    assert not workers[0].parked and mic.calls.count("starting") == starting + 1
    assert window._banners.issue("stopped:browser") is None


def test_picking_another_source_while_the_phone_wakes_still_stops_the_phone(window, monkeypatch):
    connection = _Connection()
    window.register_plugin(connection)
    window.add_stream_source(_Browser())
    spawned = _real_spawn_wake(monkeypatch)
    monkeypatch.setattr(app_module.threading, "Thread",
                        lambda target, daemon=False: SimpleNamespace(start=target, is_alive=lambda: False))
    window._start()
    phone = window._phone
    connection.selected_device = "browser"  # Connection moves the pick before asking for the switch
    window._stop()
    wake_id, _conn, url, token, _target, _opening = spawned[0]
    phone._on_wake_done(wake_id, True, "", url, token)
    assert connection.remote_stops == 2 and window._worker is None


# ── Several streams at once ───────────────────────────────────────────────────

@pytest.fixture
def two_streams(camera_env, config_home, monkeypatch):
    window, conn, mic, clients, workers, _changes = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    mic.follows_focus = False
    view = _Plugin("transforms", {"zoom": 1})
    window.register_plugin(view)
    cfg = config_home.load_config()
    cfg["devices"] = {"browser": {"plugin_configs": {"transforms": {"zoom": 3}}}}
    config_home.save_config(cfg)
    conn.ensure_virtual_camera = lambda interactive=True: True
    window.add_stream_source(_Browser())
    window._start()
    view.config["zoom"] = 2  # changed while the phone streams
    window.add_stream("browser")
    return window, conn, mic, view, workers, config_home


def test_a_second_camera_streams_next_to_the_first_to_a_camera_of_its_own(two_streams):
    window, conn, mic, view, workers, _config = two_streams
    assert [w.kwargs["slot"] for w in workers] == [0, 1]
    assert (workers[1].kwargs["canvas_width"], workers[1].kwargs["canvas_height"]) == (1920, 1080)
    assert window.stream_count() == 2 and window._focus.id == "browser" and conn.selected_device == "browser"
    assert len(workers[0].pipelines) == 1  # frozen at the phone's settings when the panels moved on
    assert not window._tiles.isHidden() and window._tiles.ids() == ["Phone", "browser"]
    assert view.config["zoom"] == 3 and view.started[-1][0] == "browser://camera" and view.stopped == 1
    assert [url for url, _ctrl in mic.started] == ["http://phone/video"]  # the mic stays with the first phone


def test_a_tile_follows_its_stream_back_to_live(two_streams):
    window, _conn, _mic, _view, _workers, _config = two_streams
    browser = window._focus
    state = lambda: window._tiles._tiles["browser"].state.text()  # noqa: E731
    window._on_worker_status("waiting", "Waiting for the first frame…", browser)
    assert state() == "● Reconnecting"
    window._on_worker_status("ok", "Streaming", browser)
    window._on_stream_reconnected(browser)
    assert state() == "● Live"


def test_the_wait_screen_hears_which_extra_cameras_are_free(two_streams):
    window, _conn, _mic, _view, _workers, _config = two_streams
    idle = []
    window._bus.idle_outputs.connect(idle.append)
    window._streams_changed()
    assert idle[-1] == [2, 3]
    window.stop_stream("browser")
    assert idle[-1] == [1, 2, 3]


def test_extra_cameras_stay_while_a_stream_goes_to_one(two_streams):
    window, *_ = two_streams
    answers = []
    window.remove_extra_cameras(lambda ok, msg: answers.append((ok, msg)))
    assert answers == [(False, "Stop the streams going to them first")]


def test_clicking_a_tile_brings_its_settings_back(two_streams):
    window, conn, _mic, view, workers, config = two_streams
    window.focus_stream("Phone")
    assert conn.selected_device == "Phone" and view.config["zoom"] == 2
    assert view.started[-1][0] == "http://phone/video"
    assert config.load_config()["devices"]["browser"]["plugin_configs"]["transforms"] == {"zoom": 3}
    assert len(workers[1].pipelines) == 1 and len(workers[0].pipelines) == 2


def test_stopping_the_first_camera_hands_the_mic_to_the_next(two_streams):
    window, conn, mic, _view, _workers, _config = two_streams
    stopped = []
    window._bus.stream_stopped.connect(lambda: stopped.append(True))
    window.stop_stream("Phone")
    assert window.stream_count() == 1 and window._focus.id == "browser" and window._tiles.isHidden()
    assert mic.stopped == 1 and mic.started[-1][0] == "browser://camera"
    assert stopped == []
    window._toggle()
    assert not window.is_streaming() and stopped == [True] and window._focus is None


def test_stop_stops_every_camera(two_streams):
    window, conn, mic, _view, workers, _config = two_streams
    window._toggle()
    assert not window.is_streaming() and all(w.stopped for w in workers)
    assert mic.stopped == 1


def test_with_two_cameras_neither_turns_its_camera_off(two_streams):
    window, _conn, _mic, _view, _workers, _config = two_streams
    window.focus_stream("Phone")
    ok, why = window.can_turn_camera_off()
    assert not ok and "one camera" in why


def test_picking_another_source_with_two_streaming_replaces_the_one_shown(two_streams, monkeypatch):
    window, conn, _mic, _view, workers, _config = two_streams
    other = _Browser()
    other.id, other.name = "browser2", "Other browser"
    window.add_stream_source(other)
    assert window.pick_source("browser2") is True
    assert sorted(s.id for s in window._streams) == ["Phone", "browser2"]
    assert workers[1].stopped and workers[2].kwargs["slot"] == 1 and window._focus.id == "browser2"
    assert window.pick_source("Phone") is True and window._focus.id == "Phone"


def test_an_extra_camera_that_isnt_set_up_asks_first(camera_env, monkeypatch):
    window, conn, _mic, _clients, workers, _changes = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda slot: slot == 0)
    asked = []
    monkeypatch.setattr(window, "_ask_slots", lambda: asked.append(True) or False)
    window.add_stream_source(_Browser())
    window._start()
    window.add_stream("browser")
    assert asked == [True] and len(workers) == 1 and window._focus.id == "Phone"


def test_extra_cameras_ready_carries_on_and_a_failure_says_why(window):
    calls = []
    window._slots_then = [lambda: calls.append(True)]
    window._on_slots_ready(True, "", "")
    assert calls == [True]
    window._slots_then = [lambda: calls.append(True)]
    window._on_slots_ready(False, "too old", "sudo modprobe")
    assert calls == [True] and window._banners.issue("vcam").title == "Couldn't add more virtual cameras"


def test_a_browser_camera_on_auto_canvas_streams_at_its_own_default_size(camera_env):
    window, conn, _mic, _clients, workers, _changes = camera_env
    conn.selected_device = "browser"
    conn.ensure_virtual_camera = lambda interactive=True: True
    window.add_stream_source(_Browser())
    window._plugin("setup").get_canvas_dims = lambda: (None, None)  # Auto
    window._start()
    expected = (1920, 1080) if sources_module.vcam.IS_LINUX else (None, None)
    assert (workers[0].kwargs["canvas_width"], workers[0].kwargs["canvas_height"]) == expected



def test_a_stopped_cameras_start_brings_it_back_next_to_the_others(two_streams):
    window, conn, _mic, view, _workers, _config = two_streams
    window.stop_stream("browser")
    assert window.stream_count() == 1
    picked = []
    window.start_again("browser", lambda: picked.append((window._focus.id, view.config["zoom"])))()
    assert window.stream_count() == 2 and window.is_streaming_from("browser")
    assert picked == [("browser", 3)]  # before ran on its own settings, not the phone's


def test_another_cameras_start_working_keeps_a_stopped_ones_banner(two_streams):
    from telescope.widgets.banner import Issue
    window, _conn, _mic, _view, _workers, _config = two_streams
    window.show_issue("stopped:Phone", Issue("Phone stopped streaming"))
    window._on_worker_status("ok", "Streaming", window._focus)  # the browser's stream
    assert window._banners.issue("stopped:Phone") is not None


def test_stop_keeps_the_mics_settings_with_its_own_phone(two_streams):
    window, conn, mic, _view, _workers, config = two_streams
    mic.config["gain"] = 7  # set while the phone has the mic and the panels show the browser
    window._toggle()
    devices = config.load_config()["devices"]
    assert devices["Phone"]["plugin_configs"]["microphone"]["gain"] == 7
    assert devices.get("browser", {}).get("plugin_configs", {}).get("microphone", {}).get("gain") != 7


def test_a_canvas_change_waits_for_one_camera(two_streams):
    window, *_ = two_streams
    answers = []
    window.restart_vcam_canvas(1280, 720, on_done=lambda ok, msg: answers.append(ok))
    assert answers == [False] and window.stream_count() == 2


def test_reconnecting_the_mics_stream_keeps_the_mic_on_it(two_streams):
    window, conn, mic, _view, _workers, _config = two_streams
    window.focus_stream("Phone")
    starts = len(mic.started)
    window.reconnect_stream()
    assert window._main is window._phone and window.stream_count() == 2
    assert len(mic.started) == starts + 1 and mic.started[-1][0] == "http://phone/video"


def test_back_on_a_tile_its_state_is_asked_for_again(two_streams, monkeypatch):
    window, *_ = two_streams
    asked = []
    phone = window._source_by_id("Phone")
    monkeypatch.setattr(phone, "refresh_state", asked.append)
    window.focus_stream("Phone")
    assert asked == [phone.session.id]  # as it is now, not as it was when the stream started


def test_the_main_camera_counts_as_free_while_only_an_extra_one_streams(two_streams):
    window, *_ = two_streams
    idle = []
    window._bus.idle_outputs.connect(idle.append)
    window.stop_stream("Phone")
    assert idle[-1] == [0, 2, 3]
    window._toggle()
    assert idle[-1] == [1, 2, 3]  # nothing streams: the main camera's wait screen follows the stop itself


class _WaitingBrowser(_Browser):
    """Browser camera with no browser yet: added next to a stream, it waits in a tile of its own."""
    id, name = "waiting", "Browser camera"

    def __init__(self):
        self.waits, self.cancels = True, 0

    def prepare(self, interactive):
        return False

    def waiting(self):
        return self.waits

    def cancel_wait(self):
        self.waits = False
        self.cancels += 1


def test_a_source_that_waits_gets_a_tile_with_the_panels_on_it(two_streams):
    window, conn, _mic, _view, _workers, _config = two_streams
    launcher = _WaitingBrowser()
    window.add_stream_source(launcher)
    window.add_stream("waiting")
    assert window._focus.id == "waiting" and conn.selected_device == "waiting"
    assert window._tiles.ids() == ["Phone", "browser", "waiting"]
    assert window._tiles._tiles["waiting"].state.text() == "● Waiting"
    assert "waiting" not in [sid for sid, _name in window._add_candidates()]

    window.focus_stream("Phone")
    assert window._focus.id == "Phone"
    window.focus_stream("waiting")
    assert window._focus.id == "waiting"

    window.stop_stream("waiting")  # its tile's X
    assert launcher.cancels == 1 and window._tiles.ids() == ["Phone", "browser"]
    assert window._focus.id == "Phone"


def test_stop_drops_a_waiting_tile_too(two_streams):
    window, *_ = two_streams
    launcher = _WaitingBrowser()
    window.add_stream_source(launcher)
    window.add_stream("waiting")
    window._toggle()
    assert launcher.cancels == 1 and not window.is_streaming() and window._pending is None


def test_a_resolution_timeout_only_clears_its_own_stream(two_streams):
    window, *_ = two_streams
    window.focus_stream("Phone")
    phone = window._focus
    window._on_resolution_pending(1280, 720)
    phone_timer = phone.pending_resolution_timer
    window.focus_stream("browser")
    browser = window._focus
    window._on_resolution_pending(1920, 1080)
    phone_timer.timeout.emit()
    assert phone.pending_resolution is None and phone.pending_resolution_timer is None
    assert browser.pending_resolution == (1920, 1080)
    assert window._fps_lbl.styleSheet() == f"color: {theme.WARN};"  # still waiting on the browser's own change
    window._clear_pending_resolution(browser)


def test_a_starting_tiles_stop_cancels_its_start(two_streams, monkeypatch):
    window, *_ = two_streams
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_wake", lambda self, *a: None)
    window.add_stream("third-phone")
    assert window._waking and "third-phone" in window._tiles.ids()
    window._tiles.stop_requested.emit("third-phone")
    assert not window._waking and window._wake_source is None
    assert window.stream_count() == 2 and window._focus.id != "third-phone"


def test_a_cancelled_phone_that_starts_late_is_stopped_while_another_wakes(camera_env, monkeypatch):
    window, conn, *_ = camera_env
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_wake", lambda self, *a: None)
    old = window._phone_source("Phone")
    remote = []
    monkeypatch.setattr(old, "_stop_phone_async", lambda target=None: remote.append(target))
    window._start()
    wake = old._wake_id
    conn.selected_device = "new-phone"
    window.switch_device("Phone", "new-phone")
    assert window._waking and window._wake_source.id == "new-phone" and len(remote) == 1
    old._on_wake_done(wake, True, "", "old-url", None)  # its start went through after the stop got there
    assert len(remote) == 2


def test_a_background_stream_the_encoder_cant_do_gets_the_panels_and_stops(two_streams, monkeypatch):
    from telescope.plugins.stream_output import StreamOutputPlugin
    import telescope.plugins.stream_output as stream_output_module
    window, *_ = two_streams
    monkeypatch.setattr(stream_output_module.h264_reader, "available", lambda: True)
    window.register_plugin(StreamOutputPlugin())
    phone = window._phone_source("Phone")
    assert window._focus is not phone
    state = {**_VALID_STATE, "codecs": ["mjpeg", "h264"], "codec": "mjpeg", "codec_unsupported": True,
             "codec_error": "H.264 isn't available at 4096x3072 on this phone", "camera_toggle": True}
    window._apply_state(phone.session.id, state, phone)
    QCoreApplication.processEvents()
    assert phone.session is None and window._banners.issue("encoder") is not None


def test_restarting_the_only_stream_left_keeps_its_virtual_camera(camera_env, monkeypatch):
    window, *_ = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    window._start()
    window.add_stream("phone-b")
    window.stop_stream("Phone")
    source = window._focus
    assert source.slot == 1
    window.reconnect_stream()
    assert source.session is not None and source.slot == 1
    assert source.session.worker.kwargs["slot"] == 1
    window.stop_stream()
    window._start()
    assert source.slot == 0  # a real stop and start alone goes back to the usual camera


def test_a_background_stream_that_reconnects_gets_its_settings_again(two_streams):
    window, *_ = two_streams
    phone = window._phone_source("Phone")
    resent = []
    phone.session.client.resend_settings = lambda: resent.append(True)
    window._on_stream_reconnected(phone)
    assert resent == [True]
    window.focus_stream("Phone")
    window._on_stream_reconnected(phone)
    assert resent == [True]  # the panels' plugins send them for the stream they show


def test_re_pairing_a_background_phone_restarts_it_with_the_new_token(camera_env, monkeypatch):
    import telescope.plugins.connection as connection_module
    from telescope.phones import READY, Resolution
    from telescope.plugins.connection import ConnectionPlugin
    from test_connection import WIFI, _add, _FakeDiscovery, _FakeResolver, _FakeTunnels
    window, *_ = camera_env
    monkeypatch.setattr(connection_module, "LanDiscovery", _FakeDiscovery)
    monkeypatch.setattr(connection_module, "run_off_ui_thread", lambda fn, *a, **k: fn(*a, **k))
    monkeypatch.setattr(connection_module, "IS_LINUX", False)
    monkeypatch.setattr(ConnectionPlugin, "_spawn_resolve", lambda self, *a: None)
    monkeypatch.setattr(ConnectionPlugin, "ensure_virtual_camera", lambda self, *a, **k: True)
    monkeypatch.setattr(ConnectionPlugin, "ensure_phone_streaming", lambda self, **k: (True, ""))
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    conn = ConnectionPlugin()
    window.register_plugin(conn)
    conn._resolver = _FakeResolver(Resolution(READY, route=WIFI))
    conn._tunnels = _FakeTunnels()
    conn._status_timer.stop()
    _add(conn, "id-a")
    _add(conn, "id-b", name="B")
    conn.select("id-a")
    window._start()
    window.add_stream("id-b")
    assert window._focus.id == "id-b"
    _add(conn, "id-a", token="new-token")
    assert window._focus.id == "id-a"
    assert window._focus.session.worker.auth.token == "new-token"
    assert window._focus.session.client.auth.token == "new-token"
    conn.shutdown()


def test_a_restart_next_to_another_stream_keeps_its_camera_over_a_remembered_one(camera_env, monkeypatch):
    window, *_ = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    window._slot_memory["Phone"] = 2  # camera 3 in an earlier run; alone it starts on camera 1
    window._start()
    window.add_stream("phone-b")
    window.focus_stream("Phone")
    source = window._focus
    assert source.slot == 0
    window.reconnect_stream()  # the Starting tile works out its camera on the way
    assert source.slot == 0 and source.session.worker.kwargs["slot"] == 0


def test_a_background_phone_without_h264_falls_back_to_mjpeg(two_streams, monkeypatch):
    from telescope.plugins.stream_output import StreamOutputPlugin
    import telescope.plugins.stream_output as stream_output_module
    window, *_ = two_streams
    monkeypatch.setattr(stream_output_module.h264_reader, "available", lambda: True)
    plugin = StreamOutputPlugin()
    window.register_plugin(plugin)
    phone = window._phone_source("Phone")
    session = phone.session
    window._apply_state(session.id, {**_VALID_STATE, "codecs": ["mjpeg"], "camera_toggle": True}, phone)
    assert window._focus is phone and plugin._format == "mjpeg"
    assert phone.session is not None and phone.session is not session  # restarted on the MJPEG route
    window._stop_all()


def test_a_codec_error_while_another_start_prepares_waits_for_it(camera_env, config_home, monkeypatch):
    window, conn, *_ = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    window._start()
    phone = window._phone_source("Phone")
    focus_during_prepare = []
    original = conn.get_stream_info

    def preparing(*args, **kwargs):
        phone._sig_state.emit(phone.session.id, {**_VALID_STATE, "codec_error": "Encoder stopped",
                                                 "camera_toggle": True})
        focus_during_prepare.append(window._focus.id)
        return original(*args, **kwargs)
    conn.get_stream_info = preparing
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_wake", lambda self, *a: None)
    window.add_stream("third")
    assert focus_during_prepare == ["third"]  # the panels stay on the start, so it opens with its own settings
    window._on_wake_done(window._wake_source, True, "")
    QCoreApplication.processEvents()
    assert window._focus is phone  # then the stream with the error gets them


def test_re_pairing_a_phone_while_another_starts_restarts_it_after(camera_env, monkeypatch):
    import telescope.plugins.connection as connection_module
    from telescope.phones import READY, Resolution
    from telescope.plugins.connection import ConnectionPlugin
    from test_connection import WIFI, _add, _FakeDiscovery, _FakeResolver, _FakeTunnels
    window, *_ = camera_env
    monkeypatch.setattr(connection_module, "LanDiscovery", _FakeDiscovery)
    monkeypatch.setattr(connection_module, "run_off_ui_thread", lambda fn, *a, **k: fn(*a, **k))
    monkeypatch.setattr(connection_module, "IS_LINUX", False)
    monkeypatch.setattr(ConnectionPlugin, "_spawn_resolve", lambda self, *a: None)
    monkeypatch.setattr(ConnectionPlugin, "ensure_virtual_camera", lambda self, *a, **k: True)
    monkeypatch.setattr(ConnectionPlugin, "ensure_phone_streaming", lambda self, **k: (True, ""))
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    conn = ConnectionPlugin()
    window.register_plugin(conn)
    conn._resolver = _FakeResolver(Resolution(READY, route=WIFI))
    conn._tunnels = _FakeTunnels()
    conn._status_timer.stop()
    _add(conn, "id-a")
    _add(conn, "id-b", name="B")
    conn.select("id-a")
    window._start()
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_wake", lambda self, *a: None)
    window.add_stream("id-b")
    assert window._waking
    _add(conn, "id-a", token="new-token")
    a = window._phone_source("id-a")
    window._on_wake_done(window._wake_source, True, "")
    QCoreApplication.processEvents()
    window._on_wake_done(window._wake_source, True, "")  # A's own restart
    assert a.session.worker.auth.token == "new-token" and a.session.client.auth.token == "new-token"
    assert window.stream_count() == 2
    conn.shutdown()


def test_a_preview_frame_from_the_last_stream_isnt_shown_on_the_next(two_streams):
    from telescope.plugins.preview import PreviewPlugin
    window, *_ = two_streams
    preview = PreviewPlugin()
    window.register_plugin(preview)
    window.focus_stream("Phone")
    old = preview._stream_gen
    window.focus_stream("browser")
    preview._on_frame(np.zeros((8, 8, 3), dtype=np.uint8), old)  # queued before the switch
    assert preview._preview_lbl.pixmap().isNull() and not preview._busy


def test_stop_cancels_a_stream_restarting_next_to_the_shown_one(camera_env, monkeypatch):
    window, *_ = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    window._start()
    window.add_stream("B")
    phone = window._phone_source("Phone")
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_wake", lambda self, *a: None)
    window.reconnect_stream("Phone")
    wake = phone._wake_id
    window._toggle()  # the Stop button
    assert not window._streams and not window._waking
    phone._on_wake_done(wake, True, "", phone.url, phone.auth)
    assert not window.is_streaming()


def test_restarting_a_background_stream_starts_it_with_its_own_settings(camera_env, config_home, monkeypatch):
    from telescope.plugins.stream_output import StreamOutputPlugin
    window, conn, *_ = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    window.register_plugin(StreamOutputPlugin())
    cfg = config_home.load_config()
    cfg["devices"] = {"Phone": {"plugin_configs": {"stream_output": {"fps": 15}}},
                      "B": {"plugin_configs": {"stream_output": {"fps": 60}}}}
    config_home.save_config(cfg)
    window._apply_device_profile("Phone")
    window._start()
    phone = window._phone_source("Phone")
    window.add_stream("B")
    window.reconnect_stream("Phone")
    assert window._focus is phone
    assert phone.session.worker.kwargs["fps"] == 15 and conn.opening["fps"] == 15
    window._stop_all()


def _camera_state(**camera):
    from test_models import _VALID_CAMERA
    return {**_VALID_STATE, "cameras": [{**_VALID_CAMERA, **camera}], "codecs": ["mjpeg", "h264"],
            "camera_toggle": True}


def test_coming_back_to_a_stream_keeps_its_exposure_and_sends_nothing(camera_env, monkeypatch):
    from telescope.plugins.camera_control import CameraControlPlugin
    window, *_ = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    camera = CameraControlPlugin()
    window.register_plugin(camera)
    window._bus.phones_changed.emit(2)
    window._start()
    phone = window._focus
    window._apply_state(phone.session.id, {**_camera_state(isoMax=6400), "auto": False, "iso": 6400,
                                           "shutter_ns": 10_000_000}, phone)
    window.add_stream("B")
    other = window._focus
    window._apply_state(other.session.id, _camera_state(isoMax=800), other)  # a lens that stops at ISO 800
    phone.session.client.sent.clear()
    window.focus_stream("Phone")
    assert camera.get_config()["iso"] > 6390 and camera._iso_slider.v_max == 6400
    assert phone.session.client.sent == []  # the phone still has them
    window._stop_all()


def test_coming_back_to_a_stream_keeps_its_focus_point(camera_env, monkeypatch):
    from telescope.plugins.camera_control import CameraControlPlugin
    window, *_ = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    camera = CameraControlPlugin()
    window.register_plugin(camera)
    window._bus.phones_changed.emit(2)
    window._start()
    phone = window._focus
    window._apply_state(phone.session.id, _camera_state(supportsFocusPoint=True), phone)
    window._bus.focus_point.emit(0.25, 0.75)
    window.add_stream("B")
    assert not camera._point_focus  # B's panel
    phone.session.client.sent.clear()
    window.focus_stream("Phone")
    assert camera._point_focus and camera._rb_focus_point.isChecked()
    assert phone.session.client.sent == []
    window._stop_all()


def test_coming_back_to_a_zoomed_phone_doesnt_crop_its_crop_again(camera_env, monkeypatch):
    from telescope.plugins.transforms import TransformsPlugin
    window, *_ = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    view = TransformsPlugin()
    window.register_plugin(view)
    window._start()
    phone = window._focus
    window._apply_state(phone.session.id, _camera_state(zoomRatioMax=10.0, cropZoomMax=4.0, freeformCrop=True),
                        phone)
    view._zoom_slider.setValue(200)
    assert view._desktop_crop == (1.0, 0.0, 0.0)  # the phone crops it all
    window.add_stream("B")
    phone.session.client.sent.clear()
    window.focus_stream("Phone")
    assert view._desktop_crop == (1.0, 0.0, 0.0)
    assert phone.session.client.sent == []
    window._stop_all()


def test_coming_back_to_a_stream_that_changed_route_meanwhile_keeps_its_exposure(camera_env, monkeypatch):
    from telescope.plugins.camera_control import CameraControlPlugin
    window, conn, *_ = camera_env
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda _slot: True)
    camera = CameraControlPlugin()
    window.register_plugin(camera)
    window._bus.phones_changed.emit(2)
    window._start()
    phone = window._focus
    window._apply_state(phone.session.id, {**_camera_state(isoMax=6400), "auto": False, "iso": 6400,
                                           "shutter_ns": 10_000_000}, phone)
    window.add_stream("B")
    other = window._focus
    window._apply_state(other.session.id, _camera_state(isoMax=800), other)
    conn.adopt_stream_route = lambda _route, _pid: "http://192.168.1.20:8080/v1/video"
    phone.session.worker.retarget = lambda _url: None
    phone._move_stream(phone.session, object())  # cable pulled while the panels show B: it goes on over Wi-Fi
    phone.session.client.sent.clear()
    window.focus_stream("Phone")
    assert camera.get_config()["iso"] > 6390 and phone.session.client.sent == []
    window._stop_all()


def test_two_phones_waiting_for_the_extra_cameras_both_start(camera_env, monkeypatch):
    window, *_ = camera_env
    ready = {0}
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda slot: slot in ready)
    monkeypatch.setattr(window, "_ask_slots", lambda: True)
    window._start()
    window.add_stream("B")  # sets up the extra cameras
    window.add_stream("C")  # while that's under way
    monkeypatch.setattr(sources_module.PhoneSource, "_spawn_wake", lambda self, *a: None)
    ready.update(range(1, app_module.vcam.MAX_SLOTS))
    window._on_slots_ready(True, "", "")
    assert window._wake_source.id == "B"
    window._on_wake_done(window._wake_source, True, "")
    QCoreApplication.processEvents()
    assert window._wake_source.id == "C"  # once B is through
    window._on_wake_done(window._wake_source, True, "")
    assert window.stream_count() == 3
    window._stop_all()


def test_stop_drops_starts_waiting_for_the_extra_cameras(camera_env, monkeypatch):
    window, *_ = camera_env
    ready = {0}
    monkeypatch.setattr(app_module.vcam, "slot_ready", lambda slot: slot in ready)
    monkeypatch.setattr(window, "_ask_slots", lambda: True)
    window._start()
    window.add_stream("B")
    window.add_stream("C")
    window._toggle()
    ready.update(range(1, app_module.vcam.MAX_SLOTS))
    window._on_slots_ready(True, "", "")
    QCoreApplication.processEvents()
    assert not window.is_streaming() and not window._waking


def test_one_bad_saved_field_keeps_the_plugins_other_settings(window):
    class Picky(_Plugin):
        def set_config(self, cfg):
            if not isinstance(cfg.get("flip_h", False), bool):
                raise TypeError("flip_h")
            super().set_config(cfg)

    plugin = Picky("transforms", {"flip_h": False, "zoom": 1.0})
    window.register_plugin(plugin)

    window._set_plugin_config(plugin, {"flip_h": "yes", "zoom": 2.0})

    assert plugin.config == {"flip_h": False, "zoom": 2.0}


def test_a_config_that_was_set_aside_shows_a_banner(window, config_home):
    path = config_home.config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("not valid json", encoding="utf-8")

    window.apply_saved_config()

    banner = window._banners._banners["config"]
    assert "couldn't be read" in banner.issue.title
    assert ".invalid-" in banner.issue.text


def test_a_readable_config_shows_no_banner(window):
    window.apply_saved_config()
    assert "config" not in window._banners._banners


def test_another_users_telescope_on_a_shared_port_answers_other(monkeypatch, port_notices):
    class Server:
        def setsockopt(self, *_args): pass
        def bind(self, _address): raise OSError("in use")
        def close(self): pass

    class Client:
        def settimeout(self, _timeout): pass
        def connect(self, _address): pass
        def sendall(self, _data): pass
        def recv(self, _size): return b"other"
        def close(self): pass

    seen = []
    sockets = iter([Server(), Client()])
    monkeypatch.setattr(app_module.socket, "socket", lambda *_args: next(sockets))
    monkeypatch.setattr(app_module, "_other_user_notice", lambda: seen.append(True))
    assert app_module.acquire_single_instance() is None
    assert seen == [True] and port_notices == []


def test_the_listener_only_raises_for_the_same_user():
    import threading
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(4)
    raised = threading.Event()
    threading.Thread(target=app_module.listen_for_raise, args=(srv, raised.set), daemon=True).start()
    try:
        with socket.create_connection(("127.0.0.1", srv.getsockname()[1]), timeout=5) as c:
            c.sendall(b"raise:" + b"0" * 16)
            assert c.recv(5) == b"other"
        assert not raised.is_set()
    finally:
        srv.close()


@pytest.mark.skipif(not hasattr(socket, "AF_UNIX"), reason="Unix sockets")
def test_a_linux_lock_is_per_user_and_a_second_copy_of_the_same_user_gets_ok(monkeypatch):
    import threading
    import update_guard
    with monkeypatch.context() as m:
        m.setattr(update_guard.sys, "platform", "linux")
        _, mine = update_guard.instance_address()
        m.setattr(update_guard.os, "getuid", lambda: 4242424)
        _, other_users = update_guard.instance_address()
    assert mine != other_users and other_users.startswith(b"\0telescope-4242424")

    monkeypatch.setattr(app_module, "_INSTANCE_ADDRESS", b"\0telescope-test-%d" % os.getpid())
    monkeypatch.setattr(app_module, "_INSTANCE_FAMILY", socket.AF_UNIX)
    srv = app_module.acquire_single_instance()
    assert srv is not None
    raised = threading.Event()
    threading.Thread(target=app_module.listen_for_raise, args=(srv, raised.set), daemon=True).start()
    try:
        assert app_module.acquire_single_instance() is None  # the second copy: raised the first
        assert raised.wait(5)
        monkeypatch.setattr(app_module, "_INSTANCE_ADDRESS", b"\0telescope-test-other-%d" % os.getpid())
        other = app_module.acquire_single_instance()  # a different user's name binds alongside
        assert other is not None
        other.close()
    finally:
        srv.close()


# ── telescope --action ────────────────────────────────────────────────────────

@pytest.fixture
def action_listener(monkeypatch, config_home):
    import threading
    monkeypatch.setattr(app_module, "config_path", config_home.config_path)
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(4)
    monkeypatch.setattr(app_module, "_INSTANCE_FAMILY", socket.AF_INET)
    monkeypatch.setattr(app_module, "_INSTANCE_ADDRESS", srv.getsockname())
    got = []

    def answer(request):
        got.append(request)
        return {"ok": True, "text": "Torch: on"}

    key = app_module.write_control_key()
    threading.Thread(target=app_module.listen_for_raise, args=(srv, lambda: None, answer, key),
                     daemon=True).start()
    yield got
    srv.close()


def test_an_action_reaches_the_running_copy_with_its_key(action_listener):
    assert app_module.send_action({"action": "camera.torch", "params": {"mode": "on"}}) == (True, "Torch: on")
    assert action_listener == [{"action": "camera.torch", "params": {"mode": "on"}}]  # the key isn't passed on
    path = app_module.control_key_path()
    if sys.platform != "win32":
        assert path.stat().st_mode & 0o077 == 0  # nobody else can read it


def test_an_action_without_the_right_key_is_turned_down(action_listener):
    app_module.control_key_path().write_bytes(b"not-the-key")
    ok, text = app_module.send_action({"action": "camera.torch"})
    assert not ok and "turned that down" in text
    assert action_listener == []


def test_an_action_with_nothing_running(monkeypatch, config_home):
    monkeypatch.setattr(app_module, "config_path", config_home.config_path)
    assert app_module.send_action({"list": True}) == (False, "Telescope isn't running.")


def test_a_raise_still_works_next_to_actions(action_listener):
    with socket.create_connection(app_module._INSTANCE_ADDRESS, timeout=5) as c:
        c.sendall(b"raise:" + app_module.user_token())
        assert c.recv(2) == b"ok"
