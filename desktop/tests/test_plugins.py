import threading

import pytest

from telescope import theme
from telescope.plugin import UNCHANGED, EventBus
from telescope.plugins.monitoring import MonitoringPlugin
from telescope.plugins.setup import AdvancedDialog, SetupPlugin
from telescope.plugins.stream_output import StreamOutputPlugin


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

    def schedule_save(self):
        self.saves += 1

    def reconnect_stream(self):
        pass

    def is_streaming(self):
        return self._worker is not None

    def stop_stream(self):
        if self._worker is not None:
            self._worker = None

    def update_stream_output(self, width=UNCHANGED, height=UNCHANGED, fps=UNCHANGED):
        if self._worker is None:
            return
        kwargs = {}
        if width is not UNCHANGED:  kwargs["width"] = width
        if height is not UNCHANGED: kwargs["height"] = height
        if fps is not UNCHANGED:    kwargs["fps"] = fps
        if kwargs:
            self._worker.update_output(**kwargs)

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
    bus = EventBus()
    plugin.setup(host, bus)
    panel = plugin.create_panel()
    bus.phones_changed.emit(1)
    return plugin, host, panel


def test_stream_output_defaults_and_resolution_placeholder(stream_output):
    plugin, _host, _panel = stream_output
    # Resolution controlled by phone, not desktop resize - combo disabled until phone reports sizes.
    assert plugin.get_stream_params() == (None, None, 30)
    assert plugin._res_combo.currentText() == "—"
    assert not plugin._res_combo.isEnabled()


def test_stream_output_resolution_populates_from_phone_state(stream_output):
    """A fresh device (no saved preference) gets our preferred default, not the phone's live guess."""
    plugin, _host, _panel = stream_output
    plugin.on_phone_state({
        "cameras": [{"id": "0", "current": True, "supportedSizes": [
            {"width": 1920, "height": 1080},
            {"width": 1280, "height": 720},
            {"width": 854, "height": 480},
        ]}],
        "stream_width": 1280, "stream_height": 720,
    })

    assert plugin._res_combo.isEnabled()
    assert plugin._res_combo.currentText() == "1920 x 1080"
    assert [plugin._res_combo.itemText(i) for i in range(plugin._res_combo.count())] == [
        "1920 x 1080", "1280 x 720", "854 x 480",
    ]
    # Still pass-through - selecting size changes phone capture, nothing left for desktop to resize.
    assert plugin.get_stream_params() == (None, None, 30)


def test_stream_output_saved_resolution_wins_over_default_and_live(stream_output):
    """A device with a saved preference gets that preference, not our default or the phone's live guess."""
    plugin, _host, _panel = stream_output
    plugin.set_config({"resolution": "1280 x 720"})
    plugin.on_phone_state({
        "cameras": [{"id": "0", "current": True, "supportedSizes": [
            {"width": 1920, "height": 1080},
            {"width": 1280, "height": 720},
        ]}],
        "stream_width": 1920, "stream_height": 1080,
    })

    assert plugin._res_combo.currentText() == "1280 x 720"


def test_stream_output_resolution_selection_sends_control(stream_output):
    plugin, _host, _panel = stream_output
    ctrl = _Ctrl()
    plugin.on_stream_start("url", ctrl)
    plugin.on_phone_state({
        "cameras": [{"id": "0", "current": True, "supportedSizes": [
            {"width": 1920, "height": 1080},
            {"width": 1280, "height": 720},
        ]}],
        "stream_width": 1920, "stream_height": 1080,
    })

    plugin._res_combo.setCurrentText("1280 x 720")

    assert {"action": "resolution", "width": 1280, "height": 720} in ctrl.sent
    # Selection changes phone capture, nothing left for desktop to resize.
    assert plugin.get_stream_params() == (None, None, 30)


def test_stream_output_camera_switch_carries_over_the_current_resolution(stream_output):
    """Lens switch preserves selected resolution if new lens supports it."""
    plugin, _host, _panel = stream_output
    plugin.on_phone_state({
        "cameras": [{"id": "0", "current": True, "supportedSizes": [
            {"width": 4032, "height": 3024},
            {"width": 1920, "height": 1080},
        ]}],
        "stream_width": 1920, "stream_height": 1080,
    })
    plugin._res_combo.setCurrentText("1920 x 1080")

    # Lens switch reuses cached capability dict, no fresh /v1/state.
    plugin._on_camera_switched({"id": "1", "current": True, "supportedSizes": [
        {"width": 4032, "height": 3024},
        {"width": 1920, "height": 1080},
    ]})

    assert plugin._res_combo.currentText() == "1920 x 1080"


def test_stream_output_camera_switch_falls_back_when_new_lens_lacks_the_size(stream_output):
    plugin, _host, _panel = stream_output
    plugin.on_phone_state({
        "cameras": [{"id": "0", "current": True, "supportedSizes": [
            {"width": 4032, "height": 3024},
            {"width": 640, "height": 480},
        ]}],
        "stream_width": 640, "stream_height": 480,
    })
    plugin._res_combo.setCurrentText("640 x 480")

    # Falls back to largest option when new lens lacks selected resolution.
    plugin._on_camera_switched({"id": "1", "current": True, "supportedSizes": [
        {"width": 4032, "height": 3024},
        {"width": 1920, "height": 1080},
    ]})

    assert plugin._res_combo.currentText() == "4032 x 3024"


def test_a_lens_without_the_live_size_sends_the_size_it_falls_back_to(stream_output):
    plugin, _host, _panel = stream_output
    ctrl = _Ctrl()
    plugin.on_stream_start("url", ctrl)
    plugin.on_phone_state({
        "cameras": [{"id": "0", "current": True, "supportedSizes": [{"width": 1920, "height": 1080}]}],
        "stream_width": 1920, "stream_height": 1080,
    })
    ctrl.sent.clear()

    plugin._on_camera_switched({"id": "1", "current": True, "supportedSizes": [
        {"width": 1280, "height": 720}, {"width": 1024, "height": 768},
    ]})

    shown = plugin._res_combo.currentData()
    assert {"action": "resolution", "width": shown[0], "height": shown[1]} in ctrl.sent


_TWO_SIZES = {
    "cameras": [{"id": "0", "current": True, "supportedSizes": [
        {"width": 4096, "height": 3072}, {"width": 1920, "height": 1080}, {"width": 1280, "height": 720}]}],
    "stream_width": 4096, "stream_height": 3072,
}


def _stopped_after(plugin, state):
    ctrl = _Ctrl()
    plugin.on_stream_start("url", ctrl)
    plugin.on_phone_state(state)
    plugin.on_stream_stop()
    return ctrl


def test_a_size_can_be_picked_while_stopped_and_the_next_start_opens_at_it(stream_output):
    # 4K was too much for the encoder and the stream stopped: the sizes stay up to pick a smaller one.
    plugin, host, _panel = stream_output
    plugin.set_config({"resolution": "4096 x 3072", "fps": 60})
    ctrl = _stopped_after(plugin, _TWO_SIZES)
    assert plugin._res_combo.isEnabled() and plugin._res_combo.currentText() == "4096 x 3072"
    assert plugin.opening() == {"width": 4096, "height": 3072, "fps": 60}

    ctrl.sent.clear()
    plugin._ar_combo.setCurrentIndex(plugin._ratios_sorted.index((16, 9)))  # the largest 16:9 comes up first
    plugin._res_combo.setCurrentIndex(plugin._res_combo.findText("1280 x 720"))
    assert ctrl.sent == []  # nothing to send it to
    assert plugin.opening() == {"width": 1280, "height": 720, "fps": 60}
    assert plugin.get_config()["resolution"] == "1280 x 720" and host.saves >= 1

    next_ctrl = _Ctrl()
    plugin.on_stream_start("url", next_ctrl)
    plugin.on_phone_state({**_TWO_SIZES, "stream_width": 1280, "stream_height": 720})  # it opened at the pick
    assert plugin._res_combo.currentText() == "1280 x 720"
    assert not [m for m in next_ctrl.sent if m["action"] == "resolution"]


def test_without_a_known_size_the_start_leaves_it_to_the_phone(stream_output):
    plugin, _host, _panel = stream_output
    plugin.set_config({"fps": 30})
    assert plugin.opening() == {"fps": 30}
    plugin.set_config({"resolution": "1920 x 1080", "fps": 30})  # saved from an earlier run: known before any state
    assert plugin.opening() == {"width": 1920, "height": 1080, "fps": 30}


def test_another_phone_does_not_keep_this_one_s_sizes(stream_output):
    plugin, _host, _panel = stream_output
    _stopped_after(plugin, _TWO_SIZES)
    plugin._on_device_changed("other phone")
    assert plugin._res_combo.currentText() == "—"
    assert not plugin._res_combo.isEnabled()


def test_a_preset_applied_while_stopped_is_what_the_next_start_opens_at(stream_output):
    plugin, _host, _panel = stream_output
    plugin.set_config({"resolution": "4096 x 3072"})
    _stopped_after(plugin, _TWO_SIZES)
    plugin.apply_preset({"resolution": "1920 x 1080", "fps": 30})
    assert plugin._res_combo.currentText() == "1920 x 1080"
    assert plugin.opening() == {"width": 1920, "height": 1080, "fps": 30}
    assert plugin.get_config()["resolution"] == "1920 x 1080"


def test_stream_output_fps_hot_updates_worker_and_phone_together(stream_output):
    """FPS change updates StreamWorker and pushes fps_target to phone if live."""
    plugin, host, _panel = stream_output
    host._worker = _Worker()
    ctrl = _Ctrl()
    plugin.on_stream_start("url", ctrl)

    plugin._fps_combo.setCurrentIndex(plugin._fps_combo.findData(48))
    plugin._on_fps_picked(plugin._fps_combo.currentIndex())

    assert host._worker.updates[-1] == {"fps": 48}
    assert {"action": "fps_target", "value": 48} in ctrl.sent
    assert host.saves >= 1


def _pick_fps(plugin, fps):
    plugin._fps_combo.setCurrentIndex(plugin._fps_combo.findData(fps))
    plugin._on_fps_picked(plugin._fps_combo.currentIndex())


def _fps_state(max_fps):
    return {"cameras": [{"id": "0", "current": True, "maxFps": max_fps,
                         "supportedSizes": [{"width": 1920, "height": 1080}]}]}


def test_rates_past_the_lens_are_grayed_and_the_pick_comes_back_on_a_lens_that_can(stream_output):
    plugin, host, _panel = stream_output
    host._worker = _Worker()
    ctrl = _Ctrl()
    plugin.on_stream_start("url", ctrl)
    _pick_fps(plugin, 60)
    plugin.on_phone_state(_fps_state(30))
    model = plugin._fps_combo.model()
    enabled = {plugin._fps_combo.itemData(i): model.item(i).isEnabled() for i in range(plugin._fps_combo.count())}
    assert enabled == {15: True, 24: True, 25: True, 30: True, 48: False, 60: False}
    assert plugin._fps() == 30
    assert {"action": "fps_target", "value": 30} in ctrl.sent and host._worker.updates[-1] == {"fps": 30}
    assert plugin.get_config()["fps"] == 60  # still what was picked
    plugin.on_phone_state(_fps_state(60))
    assert plugin._fps() == 60


def test_a_phone_that_doesnt_say_leaves_every_rate_open(stream_output):
    plugin, _host, _panel = stream_output
    plugin.on_phone_state(_fps_state(None))
    model = plugin._fps_combo.model()
    assert all(model.item(i).isEnabled() for i in range(plugin._fps_combo.count()))


@pytest.mark.parametrize("saved,shown", [(45, 48), (5, 15), (27, 25), (60, 60), ("junk", 30)])
def test_a_saved_rate_from_before_the_list_lands_on_the_nearest_choice(stream_output, saved, shown):
    plugin, _host, _panel = stream_output
    plugin.set_config({"fps": saved})
    assert plugin._fps() == shown


def test_stream_output_phone_settings_lifecycle(stream_output):
    plugin, _host, _panel = stream_output
    ctrl = _Ctrl()
    plugin._quality_slider.setValue(92)
    _pick_fps(plugin, 25)

    plugin.on_stream_start("url", ctrl)
    plugin._push_initial_settings()
    plugin._quality_slider.setValue(91)

    assert {"action": "jpeg_quality", "value": 92} in ctrl.sent
    assert {"action": "fps_target", "value": 25} in ctrl.sent
    assert {"action": "jpeg_quality", "value": 91} in ctrl.sent
    assert plugin._quality_val_lbl.text() == "91%"
    assert "High" in plugin._quality_val_lbl.toolTip()

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
    })
    # Saved resolution survives a save made before the phone reports sizes.
    assert plugin.get_config() == {
        "fps": 48,
        "jpeg_quality": 77,
        "format": "h264",  # Light is the default
        "bitrate_mbps": 0,
        "resolution": "854 x 480",
    }

    plugin.on_phone_state({
        "cameras": [{"id": "0", "current": True, "supportedSizes": [
            {"width": 1920, "height": 1080},
            {"width": 854, "height": 480},
        ]}],
        "stream_width": 1920, "stream_height": 1080,
    })
    assert plugin._res_combo.currentText() == "854 x 480"
    assert plugin.get_config()["resolution"] == "854 x 480"


def test_stream_output_invalid_persisted_resolution_falls_back_to_first(stream_output):
    plugin, _host, _panel = stream_output
    plugin.set_config({"resolution": "not a real size"})

    # Falls back to first (largest) option when persisted and live sizes are invalid.
    plugin.on_phone_state({
        "cameras": [{"id": "0", "current": True, "supportedSizes": [
            {"width": 1920, "height": 1080},
            {"width": 854, "height": 480},
        ]}],
        "stream_width": 999, "stream_height": 999,
    })

    assert plugin._res_combo.currentText() == "1920 x 1080"


def test_stream_output_keeps_the_saved_resolution_through_an_idle_save(stream_output):
    # Stopping clears the combos; a save while idle (any setting change, a device switch) used to drop it.
    plugin, _host, _panel = stream_output
    plugin.on_stream_start("url", _Ctrl())
    plugin.on_phone_state({
        "cameras": [{"id": "0", "current": True, "supportedSizes": [
            {"width": 1920, "height": 1080}, {"width": 1280, "height": 720},
        ]}],
        "stream_width": 1920, "stream_height": 1080,
    })
    plugin._res_combo.setCurrentIndex(plugin._res_combo.findText("1280 x 720"))
    plugin.on_stream_stop()

    assert plugin.get_config()["resolution"] == "1280 x 720"


def test_stream_output_saved_resolution_does_not_leak_into_the_next_device(stream_output):
    plugin, _host, _panel = stream_output
    plugin.set_config({"resolution": "1280 x 720"})
    plugin.set_config({"fps": 30, "jpeg_quality": 85})  # host resets to defaults on device switch

    assert "resolution" not in plugin.get_config()
    assert plugin._pending_resolution_text is None


@pytest.mark.parametrize("plugin_cls", [StreamOutputPlugin, MonitoringPlugin])
def test_the_card_is_greyed_out_until_a_phone_is_paired(qapp, plugin_cls):
    bus = EventBus()
    plugin = plugin_cls()
    plugin.setup(_Host(), bus)
    panel = plugin.create_panel()
    assert not panel.isEnabled()
    bus.phones_changed.emit(1)
    assert panel.isEnabled()


def test_monitoring_threshold_changes_are_saved(monitoring):
    plugin, host, _bus, _panel = monitoring
    before = host.saves
    plugin._batt_alert_spin.setValue(30)
    plugin._temp_alert_spin.setValue(50)
    assert host.saves == before + 2


@pytest.fixture
def monitoring(qapp):
    host = _Host()
    bus = EventBus()
    plugin = MonitoringPlugin()
    plugin.setup(host, bus)
    panel = plugin.create_panel()
    bus.phones_changed.emit(1)
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


def test_monitoring_does_not_warn_again_when_the_same_stream_reconnects(monitoring):
    plugin, host, _bus, _panel = monitoring
    ctrl = _Ctrl()
    plugin.on_stream_start("url", ctrl)
    plugin._check_alerts(10, False, 30)
    plugin.on_stream_start("url", ctrl)
    plugin._check_alerts(10, False, 30)
    assert len(host.notifications) == 1


def test_monitoring_skips_a_reading_it_cant_read(monitoring):
    plugin, _host, _bus, _panel = monitoring
    plugin._on_state({"battery": "full", "battery_temp_c": None})
    assert plugin._battery_lbl.text() == "—"


def test_monitoring_ignores_state_without_battery(monitoring):
    plugin, _host, _bus, _panel = monitoring
    plugin._on_state({"battery_temp_c": 99})
    assert plugin._battery_lbl.text() == "—"


@pytest.mark.parametrize(
    "level,charging,temp,batt_colour,temp_colour",
    [
        (10, False, 50, theme.ERR, theme.ERR),
        (25, False, 42, theme.WARN, theme.WARN),
        (80, False, 30, theme.OK, theme.OK),
        (10, True, 30, theme.OK, theme.OK),
    ],
)
def test_monitoring_display_colours(
    monitoring, level, charging, temp, batt_colour, temp_colour
):
    plugin, _host, _bus, _panel = monitoring
    plugin._update_display(level, charging, temp)
    assert batt_colour in plugin._battery_lbl.styleSheet()
    assert temp_colour in plugin._temp_lbl.styleSheet()
    assert plugin._battery_lbl.text() == f"{level}%" + ("  ·  charging" if charging else "")
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


def test_monitoring_charging_but_falling_still_alerts(monitoring):
    # wonky charger: `charging` stays true but the level keeps dropping
    plugin, host, _bus, _panel = monitoring
    plugin._check_alerts(25, True, 20)
    assert host.notifications == []

    plugin._check_alerts(19, True, 20)
    assert len(host.notifications) == 1
    assert "Low Battery" in host.notifications[0][0]
    assert "plugged in" in host.notifications[0][1]


def test_monitoring_charging_rising_does_not_alert(monitoring):
    plugin, host, _bus, _panel = monitoring
    plugin._check_alerts(15, True, 20)
    plugin._check_alerts(18, True, 20)
    plugin._check_alerts(22, True, 20)
    assert host.notifications == []


def test_monitoring_charging_plateau_does_not_alert(monitoring):
    # flat readings while charging shouldn't be treated as falling
    plugin, host, _bus, _panel = monitoring
    plugin._check_alerts(15, True, 20)
    plugin._check_alerts(15, True, 20)
    plugin._check_alerts(15, True, 20)
    assert host.notifications == []


def test_monitoring_fetch_emits_only_valid_battery_state(monitoring):
    plugin, _host, _bus, _panel = monitoring
    seen = []
    plugin._sig.state_ready.connect(lambda ctrl, state: seen.append(state))
    plugin._fetch(_Ctrl({"battery": 80}))
    assert seen == [{"battery": 80}]

    plugin._fetch(_Ctrl({"cameras": []}))
    plugin._fetch(_Ctrl(None))
    assert seen == [{"battery": 80}]


def test_monitoring_drops_a_reading_from_a_stream_that_ended(monitoring):
    plugin, host, _bus, _panel = monitoring
    old, new = _Ctrl(), _Ctrl()
    plugin.on_stream_start("url", new)
    plugin._on_polled(old, {"battery": 3, "charging": False})  # read before the restart, landed after
    assert plugin._battery_lbl.text() == "—" and host.notifications == []
    plugin._on_polled(new, {"battery": 60, "charging": False})
    assert plugin._battery_lbl.text() == "60%"


def _streaming(host):
    host._worker = object()


def test_monitoring_stops_for_low_battery_only_when_asked(monitoring):
    plugin, host, _bus, _panel = monitoring
    _streaming(host)
    plugin._check_alerts(10, False, 30)
    assert host.is_streaming() and len(host.notifications) == 1  # the alert, no stop
    plugin._batt_stop.setChecked(True)
    plugin._check_alerts(10, False, 30)
    assert not host.is_streaming()
    assert host.notifications[-1] == ("Telescope - Low Battery", "Phone battery is at 10%. Stopped streaming.")


def test_monitoring_stops_for_heat_only_when_asked(monitoring):
    plugin, host, _bus, _panel = monitoring
    _streaming(host)
    plugin._batt_stop.setChecked(True)   # battery is fine, so this stays out of it
    plugin._check_alerts(80, False, 50)
    assert host.is_streaming()
    plugin._temp_stop.setChecked(True)
    plugin._check_alerts(80, False, 50)
    assert not host.is_streaming()
    assert "Stopped streaming" in host.notifications[-1][1]


def test_monitoring_stops_again_after_a_restart_past_the_limit(monitoring):
    plugin, host, _bus, _panel = monitoring
    plugin._temp_stop.setChecked(True)
    for _ in range(2):
        _streaming(host)
        plugin.on_stream_start("url", _Ctrl())
        plugin._check_alerts(80, False, 50)
        assert not host.is_streaming()
        plugin.on_stream_stop()


def test_monitoring_stops_quietly_with_notify_off(monitoring):
    plugin, host, _bus, _panel = monitoring
    _streaming(host)
    plugin._batt_notify.setChecked(False)
    plugin._temp_notify.setChecked(False)
    plugin._batt_stop.setChecked(True)
    plugin._check_alerts(10, False, 50)
    assert not host.is_streaming() and host.notifications == []


def test_monitoring_charging_and_rising_never_stops(monitoring):
    plugin, host, _bus, _panel = monitoring
    _streaming(host)
    plugin._batt_stop.setChecked(True)
    plugin._check_alerts(10, True, 30)
    plugin._check_alerts(12, True, 30)
    assert host.is_streaming()
    plugin._check_alerts(11, True, 30)  # still dropping on the charger
    assert not host.is_streaming()


def test_monitoring_alert_settings_round_trip_and_defaults(monitoring):
    plugin, host, _bus, _panel = monitoring
    assert plugin.get_config() == {"battery_alert": 20, "temp_alert": 45, "battery_notify": True,
                                   "battery_stop": False, "temp_notify": True, "temp_stop": False}
    cfg = {"battery_alert": 30, "temp_alert": 50, "battery_notify": False,
           "battery_stop": True, "temp_notify": False, "temp_stop": True}
    before = host.saves
    plugin.set_config(cfg)
    assert plugin.get_config() == cfg and host.saves > before
    plugin.set_config({"battery_alert": 30, "temp_alert": 50, "battery_stop": "yes", "temp_notify": 0})
    assert plugin.get_config()["battery_stop"] is False and plugin.get_config()["temp_notify"] is True


def test_monitoring_poll_starts_daemon_fetch_thread(monkeypatch, monitoring):
    plugin, _host, _bus, _panel = monitoring
    plugin._ctrl = _Ctrl()
    started = []

    class FakeThread:
        def __init__(self, target, args, daemon):
            started.append((target, args, daemon))

        def start(self):
            started.append("started")

    monkeypatch.setattr(threading, "Thread", FakeThread)
    plugin._poll()
    assert started[0] == (plugin._fetch, (plugin._ctrl,), True)
    assert started[1] == "started"

    plugin._ctrl = None
    plugin._poll()
    assert len(started) == 2


def test_monitoring_bus_subscription_and_config(monitoring):
    plugin, _host, bus, _panel = monitoring
    plugin.set_config({"battery_alert": 30, "temp_alert": 50})
    assert plugin.get_config()["battery_alert"] == 30 and plugin.get_config()["temp_alert"] == 50

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
        ({"canvas_preset": "Custom...", "custom_canvas_w": "1920", "custom_canvas_h": 1080}, (1920, 1080)),
        ({"canvas_preset": "Custom...", "custom_canvas_w": 0, "custom_canvas_h": 720}, (1920, 1080)),
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
        "max_zoom": 15,
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
    dialog = AdvancedDialog()
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
    assert "Done" in dialog._canvas_status_lbl.text()


def _stream_output_with(stream_output, monkeypatch, decodable=True):
    import telescope.plugins.stream_output as so
    monkeypatch.setattr(so.h264_reader, "available", lambda: decodable)
    plugin, host, panel = stream_output
    host.reconnects = 0
    host.issues = {}
    host.reconnect_stream = lambda: setattr(host, "reconnects", host.reconnects + 1)
    host.show_issue = lambda key, issue: host.issues.__setitem__(key, issue)
    host.clear_issue = lambda key=None: host.issues.pop(key, None)
    return plugin, host


def test_light_is_the_default_and_switching_reconnects(stream_output, monkeypatch):
    plugin, host = _stream_output_with(stream_output, monkeypatch)
    plugin._show_format()
    assert plugin.stream_format() == "h264"
    assert plugin._fmt_h264.isChecked() and plugin._fmt_h264.isEnabled()  # offered before the phone has reported
    assert plugin._fmt_h264.text() == "Light" and "H.264" in plugin._fmt_h264.toolTip()
    assert plugin._fmt_mjpeg.text() == "Heavy" and "MJPEG" in plugin._fmt_mjpeg.toolTip()
    assert plugin._fmt_note.text() == "Good for most calls."
    assert plugin._bitrate_row.isHidden() is False and plugin._quality_row.isHidden()

    plugin.on_phone_state({"cameras": [], "codecs": ["mjpeg", "h264"]})
    assert plugin.stream_format() == "h264" and host.reconnects == 0

    plugin._fmt_mjpeg.click()
    assert plugin.stream_format() == "mjpeg" and host.reconnects == 1
    assert plugin.get_config()["format"] == "mjpeg"
    assert "USB" in plugin._fmt_note.text()
    assert plugin._quality_row.isHidden() is False and plugin._bitrate_row.isHidden()

    plugin._fmt_h264.click()
    assert plugin.stream_format() == "h264" and host.reconnects == 2


def test_a_phone_without_an_encoder_quietly_gets_heavy(stream_output, monkeypatch):
    plugin, host = _stream_output_with(stream_output, monkeypatch)
    plugin.on_phone_state({})  # the state fetch failed: says nothing about the phone
    assert plugin.stream_format() == "h264" and host.reconnects == 0
    plugin.on_phone_state({"cameras": []})  # a phone leaves codecs out when it only has MJPEG
    assert plugin.stream_format() == "mjpeg" and host.reconnects == 1
    assert plugin.get_config()["format"] == "mjpeg"
    assert not plugin._fmt_h264.isEnabled() and plugin._fmt_mjpeg.isChecked()
    assert "no H.264 encoder" in plugin._fmt_h264.toolTip()
    assert host.issues == {}  # no banner: nothing went wrong


def test_switching_away_from_a_phone_without_an_encoder_keeps_the_next_ones_light(stream_output, monkeypatch):
    plugin, _host = _stream_output_with(stream_output, monkeypatch)
    plugin.on_phone_state({"cameras": []})  # this phone only has MJPEG
    assert plugin.stream_format() == "mjpeg"
    plugin.set_config({"format": "h264"})  # the next phone's config loads before device_changed
    plugin._on_device_changed("second")
    assert plugin.stream_format() == "h264" and plugin.get_config()["format"] == "h264"


def test_h264_needs_a_decoder_here(stream_output, monkeypatch):
    plugin, _host = _stream_output_with(stream_output, monkeypatch, decodable=False)
    plugin.on_phone_state({"cameras": [], "codecs": ["mjpeg", "h264"]})
    assert not plugin._fmt_h264.isEnabled()
    assert "PyAV" in plugin._fmt_h264.toolTip()
    assert plugin.stream_format() == "mjpeg" and plugin._fmt_mjpeg.isChecked()


def test_an_encoder_failure_goes_back_to_mjpeg_with_a_banner(stream_output, monkeypatch):
    plugin, host = _stream_output_with(stream_output, monkeypatch)
    plugin.set_config({"format": "h264"})
    plugin.on_phone_state({"codecs": ["mjpeg", "h264"], "codec": "mjpeg",
                           "codec_error": "The phone's H.264 encoder stopped"})
    assert plugin.stream_format() == "mjpeg"
    assert host.reconnects == 1
    assert "encoder stopped" in host.issues["h264"].text


def test_falling_behind_on_heavy_suggests_light_with_a_button(stream_output, monkeypatch):
    plugin, host = _stream_output_with(stream_output, monkeypatch)
    plugin.set_config({"format": "mjpeg"})
    host.issues["h264"] = object()  # an earlier "Switched to Heavy"
    plugin._bus.stream_behind.emit(True)
    issue = host.issues["behind"]
    assert (issue.title, issue.text, issue.kind) == ("Can't keep up", "Try Light or lower quality.", "warn")
    assert [a.label for a in issue.actions] == ["Switch to Light"]

    issue.actions[0].callback()
    assert plugin.stream_format() == "h264" and plugin.get_config()["format"] == "h264"
    assert plugin._fmt_h264.isChecked() and host.reconnects == 1
    assert "h264" not in host.issues  # Light again, so that note is out of date

    plugin._bus.stream_behind.emit(False)
    assert "behind" not in host.issues


@pytest.mark.parametrize("fmt,decodable,codecs", [
    ("h264", True, ["mjpeg", "h264"]),   # already on Light, and this phone can't do Dynamic
    ("mjpeg", False, ["mjpeg", "h264"]),  # no PyAV here
    ("mjpeg", True, ["mjpeg"]),           # the phone has no encoder
])
def test_falling_behind_without_light_to_offer_has_no_button(stream_output, monkeypatch, fmt, decodable, codecs):
    plugin, host = _stream_output_with(stream_output, monkeypatch, decodable=decodable)
    plugin.set_config({"format": fmt})
    plugin.on_phone_state({"cameras": [], "codecs": codecs})  # an older phone: no dynamic_bitrate
    reconnects = host.reconnects
    plugin._bus.stream_behind.emit(True)
    issue = host.issues["behind"]
    assert issue.text == "Try lower quality." and issue.actions == []
    assert host.reconnects == reconnects


def test_falling_behind_on_light_offers_dynamic(stream_output, monkeypatch):
    plugin, host = _stream_output_with(stream_output, monkeypatch)
    plugin._ctrl = _Ctrl()
    plugin.on_phone_state({"cameras": [], "codecs": ["mjpeg", "h264"], "dynamic_bitrate": True})
    plugin._bus.stream_behind.emit(True)
    issue = host.issues["behind"]
    assert issue.text == "Try Dynamic or lower quality." and [a.label for a in issue.actions] == ["Switch to Dynamic"]

    issue.actions[0].callback()
    assert plugin._ctrl.sent[-1] == {"action": "bitrate", "value": -1}
    assert plugin._bitrate_val_lbl.text() == "Dynamic" and plugin.get_config()["bitrate_mbps"] == -1
    assert host.reconnects == 0  # Dynamic changes nothing about the route

    plugin._bus.stream_behind.emit(False)
    plugin._bus.stream_behind.emit(True)  # behind even on Dynamic: what's left is size and frame rate
    issue = host.issues["behind"]
    assert issue.text == "Try a lower resolution or FPS." and issue.actions == []


def test_dynamic_is_the_bitrate_slider_all_the_way_right(stream_output, monkeypatch):
    plugin, host = _stream_output_with(stream_output, monkeypatch)
    plugin._ctrl = _Ctrl()
    plugin._bitrate_slider.setValue(plugin._bitrate_slider.maximum())
    assert plugin._ctrl.sent[-1] == {"action": "bitrate", "value": -1}
    assert plugin._bitrate_val_lbl.text() == "Dynamic"
    plugin._bitrate_slider.setValue(30)
    assert plugin._ctrl.sent[-1] == {"action": "bitrate", "value": 30_000_000}
    assert plugin._bitrate_val_lbl.text() == "30 Mbps"
    # Saved as -1, which an older desktop reads as Auto; loads back as Dynamic.
    plugin._bitrate_slider.setValue(plugin._bitrate_slider.maximum())
    assert plugin.get_config()["bitrate_mbps"] == -1
    plugin.set_config({"bitrate_mbps": 0})
    assert plugin._bitrate_val_lbl.text() == "Auto"
    plugin.set_config({"bitrate_mbps": -1})
    assert plugin._bitrate_val_lbl.text() == "Dynamic" and plugin._bitrate_bps() == -1
    plugin.set_config({"bitrate_mbps": 999})  # out of range: the top fixed rate, not Dynamic
    assert plugin._bitrate_val_lbl.text() == "100 Mbps"
    plugin._push_initial_settings()
    assert {"action": "bitrate", "value": 100_000_000} in plugin._ctrl.sent


def test_a_phone_too_old_for_dynamic_says_so(stream_output, monkeypatch):
    plugin, _host = _stream_output_with(stream_output, monkeypatch)
    plugin.set_config({"bitrate_mbps": -1})
    assert "too old" not in plugin._bitrate_slider.toolTip()  # not known yet
    plugin.on_phone_state({"cameras": [], "codecs": ["mjpeg", "h264"]})
    assert "too old for Dynamic" in plugin._bitrate_slider.toolTip()
    assert plugin.diagnostics()["Bitrate"] == "Dynamic (phone uses Auto)"
    plugin.on_phone_state({"cameras": [], "codecs": ["mjpeg", "h264"], "dynamic_bitrate": True})
    assert "too old" not in plugin._bitrate_slider.toolTip()
    assert plugin.diagnostics()["Bitrate"] == "Dynamic"
    plugin._on_device_changed("another")
    assert "too old" not in plugin._bitrate_slider.toolTip()


def test_dynamic_is_no_wider_than_the_readouts_already_there(stream_output):
    plugin, _host, _panel = stream_output
    metrics = plugin._bitrate_val_lbl.fontMetrics()
    assert metrics.horizontalAdvance("Dynamic") <= metrics.horizontalAdvance("100 Mbps")


def test_h264_too_much_for_the_encoder_stops_and_offers_heavy(stream_output, monkeypatch):
    from PyQt6.QtCore import QCoreApplication
    plugin, host = _stream_output_with(stream_output, monkeypatch)
    host.stops = host.starts = 0
    host.stop_stream = lambda: setattr(host, "stops", host.stops + 1)
    host.start_stream = lambda: setattr(host, "starts", host.starts + 1)
    plugin.on_phone_state({"cameras": [], "codecs": ["mjpeg", "h264"], "codec": "mjpeg",
                           "codec_error": "H.264 isn't available at 4096x3072 on this phone", "codec_unsupported": True})
    QCoreApplication.processEvents()
    assert host.stops == 1 and host.reconnects == 0
    assert plugin.stream_format() == "h264"  # still Light: switching is the user's call
    issue = host.issues["encoder"]
    assert issue.title == "Too much for the phone's H.264 encoder"
    assert issue.text.startswith("H.264 isn't available at 4096x3072") and "lower resolution or FPS" in issue.text
    assert [a.label for a in issue.actions] == ["Switch to Heavy"]

    issue.actions[0].callback()
    assert plugin.stream_format() == "mjpeg" and plugin.get_config()["format"] == "mjpeg"
    assert host.starts == 1


def test_bitrate_goes_up_to_100_mbps(stream_output, monkeypatch):
    plugin, _host = _stream_output_with(stream_output, monkeypatch)
    plugin._ctrl = _Ctrl()
    plugin._bitrate_slider.setValue(100)
    assert plugin._ctrl.sent[-1] == {"action": "bitrate", "value": 100_000_000}
    assert plugin._bitrate_val_lbl.text() == "100 Mbps"
    assert plugin._bitrate_slider.maximum() == 101  # then Dynamic
    plugin.set_config({"bitrate_mbps": 80})
    assert plugin._bitrate_val_lbl.text() == "80 Mbps"


def test_bitrate_is_sent_in_bits_per_second(stream_output, monkeypatch):
    plugin, _host = _stream_output_with(stream_output, monkeypatch)

    class _Ctrl:
        sent = []

        def send(self, **kw):
            self.sent.append(kw)

    plugin._ctrl = _Ctrl()
    plugin._bitrate_slider.setValue(12)
    assert plugin._ctrl.sent[-1] == {"action": "bitrate", "value": 12_000_000}
    assert plugin._bitrate_val_lbl.text() == "12 Mbps"
    plugin._bitrate_slider.setValue(0)
    assert plugin._bitrate_val_lbl.text() == "Auto"


def test_stream_quality_and_bitrate_reset_on_a_double_click(stream_output):
    from PyQt6.QtCore import Qt
    from PyQt6.QtTest import QTest
    plugin, _host, panel = stream_output
    panel.show()
    plugin._quality_slider.setValue(40)
    plugin._bitrate_slider.setValue(12)
    QTest.mouseDClick(plugin._quality_slider, Qt.MouseButton.LeftButton)
    QTest.mouseDClick(plugin._bitrate_slider, Qt.MouseButton.LeftButton)
    assert (plugin._quality_slider.value(), plugin._bitrate_slider.value()) == (85, 0)  # 0 is Auto
    assert plugin._bitrate_slider.toolTip().startswith("Auto: about 8 Mbps")  # its own tip stays


def test_jpeg_quality_stops_at_95_and_marks_the_recommended_spot(stream_output):
    plugin, _host, _panel = stream_output
    assert plugin._quality_slider.maximum() == 95
    assert plugin._quality_slider.snaps() == [85]
    assert "recommended" in plugin._quality_slider.toolTip()
    plugin.set_config({"jpeg_quality": 100})  # saved before the cap
    assert plugin.get_config()["jpeg_quality"] == 95
