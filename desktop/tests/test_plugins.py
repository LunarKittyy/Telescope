import threading

import pytest

from telescope.plugin import EventBus
from telescope.plugins.monitoring import MonitoringPlugin
from telescope.plugins.setup import SetupDialog, SetupPlugin
from telescope.plugins.stream_output import RESOLUTIONS, StreamOutputPlugin


class _Ctrl:
    def __init__(self, state=None):
        self.sent = []
        self.state = state

    def send(self, **params):
        self.sent.append(params)

    def get_state(self):
        return self.state


class _Worker:
    def __init__(self):
        self.updates = []

    def update_output(self, **kwargs):
        self.updates.append(kwargs)


class _Host:
    def __init__(self):
        self._worker = None
        self.saves = 0
        self.notifications = []
        self.canvas_restarts = []

    def _schedule_save(self):
        self.saves += 1

    def send_notification(self, title, body):
        self.notifications.append((title, body))

    def restart_vcam_canvas(self, w, h, on_done=None):
        self.canvas_restarts.append((w, h))
        if on_done:
            on_done(True, "done")


@pytest.fixture
def stream_output(qapp):
    host = _Host()
    plugin = StreamOutputPlugin()
    plugin.setup(host, EventBus())
    panel = plugin.create_panel()
    return plugin, host, panel


def test_stream_output_defaults_and_resolution_mapping(stream_output):
    plugin, _host, _panel = stream_output
    assert plugin.get_stream_params() == (None, None, 30)

    for label, dimensions in RESOLUTIONS.items():
        plugin._res_combo.setCurrentText(label)
        w, h, fps = plugin.get_stream_params()
        assert (w, h) == (dimensions if dimensions else (None, None))
        assert fps == 30


def test_stream_output_hot_updates_worker(stream_output):
    plugin, host, _panel = stream_output
    host._worker = _Worker()

    plugin._res_combo.setCurrentText("1280 x 720")
    plugin._fps_spin.setValue(60)
    plugin._on_fps()

    assert host._worker.updates[-2:] == [
        {"width": 1280, "height": 720},
        {"fps": 60},
    ]
    assert host.saves >= 2


def test_stream_output_pass_through_hot_update_uses_none(stream_output):
    plugin, host, _panel = stream_output
    host._worker = _Worker()
    plugin._res_combo.setCurrentText("640 x 360")

    plugin._res_combo.setCurrentText("Pass-through (auto)")

    assert host._worker.updates[-1] == {"width": None, "height": None}


def test_stream_output_phone_settings_lifecycle(stream_output):
    plugin, _host, _panel = stream_output
    ctrl = _Ctrl()
    plugin._quality_slider.setValue(92)
    plugin._phone_fps_spin.setValue(25)

    plugin.on_stream_start("url", ctrl)
    plugin._push_initial_settings()
    plugin._quality_slider.setValue(91)
    plugin._on_phone_fps_changed()

    assert {"action": "jpeg_quality", "value": 92} in ctrl.sent
    assert {"action": "fps_target", "value": 25} in ctrl.sent
    assert {"action": "jpeg_quality", "value": 91} in ctrl.sent
    assert plugin._quality_val_lbl.text() == "91%  Balanced"

    plugin.on_stream_stop()
    before = list(ctrl.sent)
    plugin._push_initial_settings()
    assert ctrl.sent == before


def test_stream_output_config_round_trip_and_invalid_resolution(stream_output):
    plugin, _host, _panel = stream_output
    plugin.set_config({
        "resolution": "854 x 480",
        "fps": 48,
        "jpeg_quality": 77,
        "phone_fps": 18,
    })
    assert plugin.get_config() == {
        "resolution": "854 x 480",
        "fps": 48,
        "jpeg_quality": 77,
        "phone_fps": 18,
    }

    plugin.set_config({"resolution": "not a real size"})
    assert plugin._res_combo.currentText() == "854 x 480"


@pytest.fixture
def monitoring(qapp):
    host = _Host()
    bus = EventBus()
    plugin = MonitoringPlugin()
    plugin.setup(host, bus)
    panel = plugin.create_panel()
    return plugin, host, bus, panel


def test_monitoring_stream_lifecycle(monitoring):
    plugin, _host, _bus, _panel = monitoring
    ctrl = _Ctrl()
    plugin._battery_notified = True
    plugin._temp_notified = True

    plugin.on_stream_start("url", ctrl)
    assert plugin._timer.isActive()
    assert plugin._battery_notified is False
    assert plugin._temp_notified is False

    plugin.on_stream_stop()
    assert not plugin._timer.isActive()
    assert plugin._ctrl is None
    assert plugin._battery_lbl.text() == "—"
    assert plugin._temp_lbl.text() == "—"


def test_monitoring_ignores_state_without_battery(monitoring):
    plugin, _host, _bus, _panel = monitoring
    plugin._on_state({"battery_temp_c": 99})
    assert plugin._battery_lbl.text() == "—"


@pytest.mark.parametrize(
    "level,charging,temp,batt_colour,temp_colour",
    [
        (10, False, 50, "#ef5350", "#ef5350"),
        (25, False, 42, "#ffa726", "#ffa726"),
        (80, False, 30, "#66bb6a", "#66bb6a"),
        (10, True, 30, "#66bb6a", "#66bb6a"),
    ],
)
def test_monitoring_display_colours(
    monitoring, level, charging, temp, batt_colour, temp_colour
):
    plugin, _host, _bus, _panel = monitoring
    plugin._update_display(level, charging, temp)
    assert batt_colour in plugin._battery_lbl.styleSheet()
    assert temp_colour in plugin._temp_lbl.styleSheet()
    assert plugin._battery_lbl.text() == f"{level}%" + ("  [charging]" if charging else "")
    assert plugin._temp_lbl.text() == f"{temp:.1f} °C"


def test_monitoring_alerts_once_then_rearm_after_hysteresis(monitoring):
    plugin, host, _bus, _panel = monitoring

    plugin._check_alerts(20, False, 45)
    plugin._check_alerts(10, False, 50)
    assert len(host.notifications) == 2
    assert "Low Battery" in host.notifications[0][0]
    assert "Running Hot" in host.notifications[1][0]

    plugin._check_alerts(26, False, 39.9)
    assert plugin._battery_notified is False
    assert plugin._temp_notified is False
    plugin._check_alerts(20, False, 45)
    assert len(host.notifications) == 4


def test_monitoring_charging_suppresses_battery_alert(monitoring):
    plugin, host, _bus, _panel = monitoring
    plugin._check_alerts(5, True, 20)
    assert host.notifications == []


def test_monitoring_fetch_emits_only_valid_battery_state(monitoring):
    plugin, _host, _bus, _panel = monitoring
    seen = []
    plugin._sig.state_ready.connect(seen.append)
    plugin._ctrl = _Ctrl({"battery": 80})
    plugin._fetch()
    assert seen == [{"battery": 80}]

    plugin._ctrl = _Ctrl({"cameras": []})
    plugin._fetch()
    plugin._ctrl = None
    plugin._fetch()
    assert seen == [{"battery": 80}]


def test_monitoring_poll_starts_daemon_fetch_thread(monkeypatch, monitoring):
    plugin, _host, _bus, _panel = monitoring
    plugin._ctrl = _Ctrl()
    started = []

    class FakeThread:
        def __init__(self, target, daemon):
            started.append((target, daemon))

        def start(self):
            started.append("started")

    monkeypatch.setattr(threading, "Thread", FakeThread)
    plugin._poll()
    assert started[0] == (plugin._fetch, True)
    assert started[1] == "started"

    plugin._ctrl = None
    plugin._poll()
    assert len(started) == 2


def test_monitoring_bus_subscription_and_config(monitoring):
    plugin, _host, bus, _panel = monitoring
    plugin.set_config({"battery_alert": 30, "temp_alert": 50})
    assert plugin.get_config() == {"battery_alert": 30, "temp_alert": 50}

    bus.phone_state_updated.emit({"battery": 29, "charging": False, "battery_temp_c": 49})
    assert plugin._battery_lbl.text() == "29%"
    assert plugin._temp_lbl.text() == "49.0 °C"


@pytest.fixture
def setup_plugin(qapp):
    host = _Host()
    plugin = SetupPlugin()
    plugin.setup(host, EventBus())
    panel = plugin.create_panel()
    return plugin, host, panel


@pytest.mark.parametrize(
    "config,expected",
    [
        ({}, (None, None)),
        ({"canvas_preset": "1280 x 720"}, (None, None)),
        ({"canvas_preset": "720p 16:9 - 1280 x 720"}, (1280, 720)),
        ({"canvas_preset": "Custom...", "custom_canvas_w": 1111, "custom_canvas_h": 777}, (1111, 777)),
    ],
)
def test_setup_plugin_canvas_config(setup_plugin, config, expected):
    plugin, _host, _panel = setup_plugin
    plugin.set_config(config)
    assert plugin.get_canvas_dims() == expected


def test_setup_plugin_config_round_trip(setup_plugin):
    plugin, _host, _panel = setup_plugin
    cfg = {
        "canvas_preset": "Custom...",
        "custom_canvas_w": 2048,
        "custom_canvas_h": 1536,
    }
    plugin.set_config(cfg)
    assert plugin.get_config() == cfg


def test_setup_plugin_apply_canvas_persists_and_reports_result(setup_plugin):
    plugin, host, _panel = setup_plugin

    class Dialog:
        def get_canvas_preset_label(self):
            return "Custom..."

        def set_canvas_apply_result(self, ok, msg):
            self.result = (ok, msg)

    plugin._dlg = Dialog()
    plugin._on_apply_canvas(900, 700)

    assert plugin.get_canvas_dims() == (900, 700)
    assert host.saves == 1
    assert host.canvas_restarts == [(900, 700)]
    assert plugin._dlg.result == (True, "done")


def test_setup_dialog_canvas_dimension_selection_and_result_messages(qapp):
    dialog = SetupDialog()
    dialog.set_canvas_preset("Custom...", 1234, 567)
    assert dialog._get_selected_dims() == (1234, 567)
    assert dialog.get_canvas_preset_label() == "Custom..."
    assert dialog._custom_widget.isVisible() is False  # parent dialog itself is hidden

    dialog.set_canvas_preset("Auto (from first frame)")
    assert dialog._get_selected_dims() == (None, None)

    dialog.set_canvas_apply_result(False, "device in use")
    assert "Close OBS" in dialog._canvas_status_lbl.text()
    dialog.set_canvas_apply_result(False, "permission denied")
    assert dialog._canvas_status_lbl.text() == "Failed: permission denied"
    dialog.set_canvas_apply_result(True, "ok")
    assert "successfully" in dialog._canvas_status_lbl.text()
