from types import SimpleNamespace
import socket

import pytest
from PyQt6.QtWidgets import QWidget

import telescope.app as app_module
from telescope.plugin import TelescopePlugin


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
        return QWidget() if self.want_panel else None

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
    def __init__(self, selected="Phone", stream_info=("http://phone/video", True)):
        super().__init__("connection", {"mode": "wifi"})
        self.selected_device = selected
        self.stream_info = stream_info
        self.selected = []

    def get_stream_info(self):
        return self.stream_info

    def select_device(self, name):
        self.selected.append(name)


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
    yield win
    # The window is never shown. Calling QWidget.close() here would route
    # through whichever closeEvent/worker doubles a test intentionally left
    # installed and can make Qt abort while unwinding the fixture.
    win._worker = None
    win._ctrl = None


def test_register_plugin_initializes_panel_and_captures_device_defaults(window):
    plugin = _Plugin("transforms", {"zoom": 1})
    before = window._scroll_content_layout.count()

    window.register_plugin(plugin)

    assert plugin.setup_args == (window, window._bus)
    assert window._plugins == [plugin]
    assert window._plugin_defaults == {"transforms": {"zoom": 1}}
    assert window._scroll_content_layout.count() == before + 1


def test_acquire_single_instance_binds_and_listens(monkeypatch):
    calls = []

    class Socket:
        def setsockopt(self, *args): calls.append(("setsockopt", args))
        def bind(self, address): calls.append(("bind", address))
        def listen(self, count): calls.append(("listen", count))

    sock = Socket()
    monkeypatch.setattr(app_module.socket, "socket", lambda *_args: sock)
    assert app_module.acquire_single_instance() is sock
    assert ("bind", ("127.0.0.1", app_module._INSTANCE_PORT)) in calls
    assert ("listen", 1) in calls


def test_acquire_single_instance_notifies_existing_process(monkeypatch):
    events = []

    class Server:
        def setsockopt(self, *_args): pass
        def bind(self, _address): raise OSError("in use")
        def close(self): events.append("server-close")

    class Client:
        def settimeout(self, timeout): events.append(("timeout", timeout))
        def connect(self, address): events.append(("connect", address))
        def sendall(self, data): events.append(("send", data))
        def close(self): events.append("client-close")

    sockets = iter([Server(), Client()])
    monkeypatch.setattr(app_module.socket, "socket", lambda *_args: next(sockets))
    assert app_module.acquire_single_instance() is None
    assert ("send", b"raise") in events
    assert events[-1] == "server-close"


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
        def recv(self, _size): return b"raise"
        def close(self): events.append("closed")

    class Server:
        def settimeout(self, timeout): events.append(("timeout", timeout))
        def accept(self):
            if "accepted" in events:
                raise OSError("done")
            events.append("accepted")
            return Conn(), ("127.0.0.1", 1)

    app_module.listen_for_raise(Server(), lambda: events.append("raised"))
    assert events == [("timeout", 1.0), "accepted", "raised", "closed"]


def test_listen_for_raise_ignores_wrong_message_and_timeouts(monkeypatch):
    events = []

    class Conn:
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


def test_register_headless_global_plugin_does_not_add_panel(window):
    plugin = _Plugin("global", panel=False)
    before = window._scroll_content_layout.count()
    window.register_plugin(plugin)
    assert window._scroll_content_layout.count() == before
    assert "global" not in window._plugin_defaults


def test_save_config_separates_global_and_device_local_plugins(window, config_home):
    connection = _Connection(selected="PhoneA")
    global_plugin = _Plugin("setup", {"canvas": "auto"})
    local_plugin = _Plugin("transforms", {"zoom": 2})
    for plugin in (connection, global_plugin, local_plugin):
        window.register_plugin(plugin)

    window._save_config()

    cfg = config_home.load_config()
    assert cfg["selected_device"] == "PhoneA"
    assert cfg["plugin_configs"] == {
        "connection": {"mode": "wifi"},
        "setup": {"canvas": "auto"},
    }
    assert cfg["devices"]["PhoneA"]["plugin_configs"] == {
        "transforms": {"zoom": 2}
    }


def test_save_config_without_connection_skips_device_profile(window, config_home):
    window.register_plugin(_Plugin("transforms", {"zoom": 2}))
    window._save_config()
    assert config_home.load_config()["devices"] == {}


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
    window._worker = object()
    calls = []
    monkeypatch.setattr(window, "_stop", lambda: calls.append("stop") or setattr(window, "_worker", None))
    monkeypatch.setattr(window, "_start", lambda: calls.append("start"))

    window._switch_device("Old", "New")

    cfg = config_home.load_config()
    assert cfg["devices"]["Old"]["plugin_configs"]["transforms"] == {"zoom": 2}
    assert cfg["selected_device"] == "New"
    assert plugin.config["zoom"] == 4
    assert calls == ["stop", "start"]


def test_reconnect_stream_only_restarts_when_active(window, monkeypatch):
    calls = []
    monkeypatch.setattr(window, "_stop", lambda: calls.append("stop"))
    monkeypatch.setattr(window, "_start", lambda: calls.append("start"))
    window.reconnect_stream()
    assert calls == []

    window._worker = object()
    window.reconnect_stream()
    assert calls == ["stop", "start"]


def test_toggle_routes_to_start_or_stop(window, monkeypatch):
    calls = []
    monkeypatch.setattr(window, "_start", lambda: calls.append("start"))
    monkeypatch.setattr(window, "_stop", lambda: calls.append("stop"))
    window._toggle()
    window._worker = object()
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
    assert connection.selected == ["PhoneB"]


def test_apply_config_empty_is_noop(window):
    window._apply_config({})


def test_start_without_connection_or_invalid_stream_is_noop(window):
    window._start()
    assert window._worker is None
    window.register_plugin(_Connection(stream_info=(None, False)))
    window._start()
    assert window._worker is None


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
        def __init__(self, url):
            self.url = url
            self.closed = False
            clients.append(self)

        def close(self):
            self.closed = True

    class Worker:
        def __init__(self, **kwargs):
            self.kwargs = kwargs
            self.status = _Signal()
            self.started = False
            workers.append(self)

        def start(self):
            self.started = True

    class Thread:
        def __init__(self, target, args=(), daemon=False):
            threads.append((target, args, daemon))

        def start(self):
            pass

    monkeypatch.setattr(app_module, "PhoneControlClient", Client)
    monkeypatch.setattr(app_module, "StreamWorker", Worker)
    monkeypatch.setattr(app_module.threading, "Thread", Thread)
    bus_urls = []
    window._bus.stream_started.connect(bus_urls.append)

    window._start()

    assert clients[0].url == "http://phone/video"
    assert workers[0].kwargs["width"] == 1280
    assert workers[0].kwargs["height"] == 720
    assert workers[0].kwargs["fps"] == 25
    assert workers[0].kwargs["canvas_width"] == 1920
    assert workers[0].kwargs["canvas_height"] == 1080
    assert workers[0].kwargs["frame_pipeline"] == [p.process_frame for p in window._plugins]
    assert workers[0].started is True
    assert bus_urls == ["http://phone/video"]
    assert all(plugin.started for plugin in window._plugins)
    assert threads[0][1] == ("http://phone/video",)
    assert threads[0][2] is True
    assert window._start_btn.text() == "Stop Streaming"


def test_start_uses_defaults_without_optional_plugins(window, monkeypatch):
    window.register_plugin(_Connection())
    captured = []

    class Worker:
        def __init__(self, **kwargs):
            captured.append(kwargs)
            self.status = _Signal()

        def start(self):
            pass

    monkeypatch.setattr(app_module, "PhoneControlClient", lambda _url: object())
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
    window._worker = worker
    window._ctrl = client

    window._stop()

    assert worker.stop_requested is True
    assert worker.timeout == 5000
    assert client.closed is True
    assert window._worker is None
    assert window._ctrl is None
    assert plugin.stopped == 1
    assert bus == [True]
    assert window._start_btn.text() == "Start Streaming"


def test_stop_is_safe_when_already_stopped(window):
    window._stop()
    assert window._status_lbl.text() == "Stopped."


def test_restart_canvas_non_linux_waits_and_restarts_active_stream(window, monkeypatch):
    events = []

    class OldWorker:
        def wait(self, timeout):
            events.append(("wait", timeout))

    old = OldWorker()
    window._worker = old
    monkeypatch.setattr(app_module, "IS_LINUX", False)
    monkeypatch.setattr(
        window,
        "_stop",
        lambda: events.append("stop") or setattr(window, "_worker", None),
    )
    monkeypatch.setattr(window, "_start", lambda: events.append("start"))

    class ImmediateThread:
        def __init__(self, target, daemon): self.target = target
        def start(self): self.target()

    monkeypatch.setattr(app_module.threading, "Thread", ImmediateThread)
    done = []
    window.restart_vcam_canvas(1920, 1080, on_done=lambda *args: done.append(args))
    assert events == ["stop", ("wait", 5000), "start"]
    assert done == [(True, "canvas updated")]


def test_canvas_reload_failure_reports_error_and_clears_callback(window, monkeypatch):
    done = []
    window._vcam_reload_callback = lambda *args: done.append(args)
    monkeypatch.setattr(window, "_start", lambda: (_ for _ in ()).throw(AssertionError()))
    window._on_canvas_reload_done(False, "busy", True)
    assert window._status_lbl.text() == "Reload failed: busy"
    assert window._status_lbl.objectName() == "status_err"
    assert done == [(False, "busy")]
    assert window._vcam_reload_callback is None


def test_fetch_state_retries_then_emits_success(window, monkeypatch):
    states = iter([None, {"battery": 80}])
    window._ctrl = SimpleNamespace(get_state=lambda: next(states))
    emitted = []
    window._sig_state.connect(emitted.append)
    sleeps = []
    monkeypatch.setattr(app_module.time, "sleep", sleeps.append)

    window._fetch_state_async("url")

    assert emitted == [{"battery": 80}]
    assert sleeps == [1.5, 2]


def test_fetch_state_emits_empty_after_three_failures(window, monkeypatch):
    window._ctrl = SimpleNamespace(get_state=lambda: None)
    emitted = []
    window._sig_state.connect(emitted.append)
    monkeypatch.setattr(app_module.time, "sleep", lambda _seconds: None)
    window._fetch_state_async("url")
    assert emitted == [{}]


def test_fetch_state_exits_if_client_is_removed(window, monkeypatch):
    monkeypatch.setattr(app_module.time, "sleep", lambda _seconds: None)
    window._fetch_state_async("url")


def test_apply_state_emits_bus_and_calls_plugins(window):
    plugin = _Plugin("other")
    window.register_plugin(plugin)
    bus = []
    window._bus.phone_state_updated.connect(bus.append)
    state = {"battery": 50}
    window._apply_state(state)
    assert bus == [state]
    assert plugin.states == [state]


@pytest.mark.parametrize(
    "kind,object_name",
    [("ok", "status_ok"), ("warn", "status_warn"), ("other", "status_dim")],
)
def test_worker_status_updates_status_label(window, kind, object_name):
    window._on_worker_status(kind, "message")
    assert window._status_lbl.text() == "message"
    assert window._status_lbl.objectName() == object_name


def test_worker_fps_and_idle_status(window):
    window._on_worker_status("fps", "29.9 fps")
    assert window._fps_lbl.text() == "29.9 fps"
    window._worker = object()
    window._on_worker_status("idle", "Stopped.")
    assert window._fps_lbl.text() == ""
    assert window._worker is None
    assert window._start_btn.text() == "Start Streaming"


def test_send_notification_uses_notify_send_on_linux(window, monkeypatch):
    popen_calls = []
    monkeypatch.setattr(app_module, "IS_LINUX", True)
    monkeypatch.setattr(app_module.shutil, "which", lambda _name: "/usr/bin/notify-send")
    monkeypatch.setattr(
        app_module.subprocess,
        "Popen",
        lambda *args, **kwargs: popen_calls.append((args, kwargs)),
    )
    window.send_notification("Title", "Body")
    assert popen_calls[0][0][0][-2:] == ["Title", "Body"]


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

    monkeypatch.setattr(window, "_stop", lambda: calls.append("stop"))
    monkeypatch.setattr(app_module.QApplication, "quit", lambda: calls.append("quit"))
    window._tray_quit()
    assert window._tray_close_notified is True
    assert calls[-2:] == ["stop", "quit"]


def test_close_event_minimizes_active_stream_to_tray(window, monkeypatch):
    window._tray = object()
    window._worker = object()
    notifications = []
    monkeypatch.setattr(window, "hide", lambda: None)
    monkeypatch.setattr(window, "send_notification", lambda *args: notifications.append(args))

    event = SimpleNamespace(ignore=lambda: setattr(event, "ignored", True))
    window.closeEvent(event)
    assert event.ignored is True
    assert len(notifications) == 1
    window.closeEvent(event)
    assert len(notifications) == 1


def test_close_event_stops_and_accepts_without_background_stream(window, monkeypatch):
    calls = []
    monkeypatch.setattr(window, "_stop", lambda: calls.append("stop"))
    monkeypatch.setattr(app_module.QApplication, "quit", lambda: calls.append("quit"))
    event = SimpleNamespace(accept=lambda: calls.append("accept"))
    window.closeEvent(event)
    assert calls == ["stop", "accept", "quit"]
