import numpy as np
import pytest

from telescope.plugin import EventBus
from telescope.plugins.camera_control import (
    CameraControlPlugin,
    _diopters_to_label,
    _kelvin_to_rggb,
    derive_camera_control_view,
)


class _Host:
    def __init__(self):
        self.saves = 0

    def schedule_save(self):
        self.saves += 1


class _Ctrl:
    def __init__(self):
        self.sent = []

    def send(self, **params):
        self.sent.append(params)


@pytest.fixture
def camera_plugin(qapp):
    host = _Host()
    bus = EventBus()
    plugin = CameraControlPlugin()
    plugin.setup(host, bus)
    panel = plugin.create_panel()
    return plugin, host, bus, panel


def test_derive_camera_control_view_returns_none_for_empty_state():
    assert derive_camera_control_view({}) is None
    assert derive_camera_control_view(None) is None


def test_derive_camera_control_view_maps_auto_and_manual_flags():
    view = derive_camera_control_view({
        "cameras": [], "auto": False, "wb_manual": True, "focus_mode": "manual",
    })
    assert view.manual_exposure is True
    assert view.manual_wb is True
    assert view.manual_focus is True


def test_derive_camera_control_view_picks_current_camera_and_its_ranges():
    state = {
        "cameras": [
            {"id": "0", "current": False, "aeCompMin": -8, "aeCompMax": 8},
            {"id": "1", "current": True, "aeCompMin": -3, "aeCompMax": 3, "aeCompStep": 0.5},
        ],
        "auto": True,
    }
    view = derive_camera_control_view(state)
    assert view.current_camera["id"] == "1"
    assert view.ae_comp_range == (-3, 3)
    assert view.ae_comp_step == 0.5


def test_derive_camera_control_view_defaults_ae_range_without_current_camera():
    view = derive_camera_control_view({"cameras": [], "auto": True})
    assert view.current_camera is None
    assert view.ae_comp_range == (-8, 8)
    assert view.ae_comp_step == 0.167


def test_derive_camera_control_view_maps_nr_and_edge_mode_indices():
    view = derive_camera_control_view({"cameras": [], "auto": True, "nr_mode": 2, "edge_mode": 0})
    assert view.nr_mode_index == 2
    assert view.edge_mode_index == 0
    # An unrecognized mode value falls back to index 1 ("Fast").
    view2 = derive_camera_control_view({"cameras": [], "auto": True, "nr_mode": 99})
    assert view2.nr_mode_index == 1


def test_kelvin_gains_are_symmetric_and_tint_is_clamped():
    neutral = _kelvin_to_rggb(5500, 0)
    cool = _kelvin_to_rggb(2000, 0)
    warm = _kelvin_to_rggb(10000, 0)
    magenta = _kelvin_to_rggb(5500, 10_000)
    green = _kelvin_to_rggb(5500, -10_000)

    assert neutral == pytest.approx((2, 1, 1, 2))
    assert cool[0] < neutral[0] and cool[3] > neutral[3]
    assert warm[0] > neutral[0] and warm[3] < neutral[3]
    assert magenta[1:3] == pytest.approx((0.5, 0.5))
    assert green[1:3] == pytest.approx((2.5, 2.5))


@pytest.mark.parametrize(
    "diopters,expected",
    [(0, "inf"), (0.01, "inf"), (0.5, "2.00 m"), (2, "0.50 m")],
)
def test_diopters_label(diopters, expected):
    assert _diopters_to_label(diopters) == expected


def test_stream_start_pushes_all_default_settings(camera_plugin):
    plugin, _host, _bus, _panel = camera_plugin
    ctrl = _Ctrl()

    plugin.on_stream_start("url", ctrl)

    actions = [params["action"] for params in ctrl.sent]
    assert actions == [
        "auto", "wb_auto", "ois", "focus_mode", "ae_comp",
        "nr_mode", "edge_mode", "black_level_lock",
    ]
    assert plugin._lens_panel.layout().itemAt(0).widget().text() == "Loading lenses..."


def test_manual_handlers_send_controls_and_schedule_saves(camera_plugin):
    plugin, host, _bus, _panel = camera_plugin
    ctrl = _Ctrl()
    plugin._ctrl = ctrl

    plugin._rb_exp_manual.setChecked(True)
    plugin._rb_exp_auto.setChecked(False)
    plugin._on_exp_mode()
    plugin._on_iso_changed(321.9)
    plugin._on_shutter_changed(2_000_000.9)

    plugin._rb_wb_manual.setChecked(True)
    plugin._rb_wb_auto.setChecked(False)
    plugin._on_wb_mode()
    plugin._on_wb_changed(6200)
    plugin._on_tint_changed(25)

    actions = [params["action"] for params in ctrl.sent]
    assert actions[:2] == ["iso", "shutter"]
    assert {"action": "iso", "value": 321} in ctrl.sent
    assert {"action": "shutter", "value": 2_000_000} in ctrl.sent
    assert actions.count("wb_gains") == 3
    assert host.saves >= 6


def test_auto_mode_handlers_send_reset_commands(camera_plugin):
    plugin, _host, _bus, _panel = camera_plugin
    ctrl = _Ctrl()
    plugin._ctrl = ctrl

    plugin._rb_exp_manual.setChecked(False)
    plugin._rb_exp_auto.setChecked(True)
    plugin._on_exp_mode()
    plugin._rb_wb_manual.setChecked(False)
    plugin._rb_wb_auto.setChecked(True)
    plugin._on_wb_mode()
    plugin._rb_focus_manual.setChecked(False)
    plugin._rb_focus_auto.setChecked(True)
    plugin._on_focus_mode()

    assert [x["action"] for x in ctrl.sent] == ["auto", "wb_auto", "focus_mode"]
    assert ctrl.sent[-1]["value"] == "continuous"


def test_discrete_control_handlers_send_selected_values(camera_plugin):
    plugin, host, _bus, _panel = camera_plugin
    ctrl = _Ctrl()
    plugin._ctrl = ctrl

    plugin._on_ois(False)
    plugin._on_ae_comp_changed(3)
    plugin._on_nr_mode_changed(2)
    plugin._on_edge_mode_changed(0)
    plugin._on_bll_changed(True)
    plugin._on_torch_toggled(True)

    assert ctrl.sent == [
        {"action": "ois", "value": "0"},
        {"action": "ae_comp", "value": 3},
        {"action": "nr_mode", "value": 2},
        {"action": "edge_mode", "value": 0},
        {"action": "black_level_lock", "value": "1"},
        {"action": "torch", "value": "1"},
    ]
    assert host.saves == 5  # torch is intentionally live-only, not persisted


def test_phone_state_populates_camera_capabilities_and_controls(camera_plugin):
    plugin, _host, _bus, _panel = camera_plugin
    state = {
        "cameras": [{
            "id": "tele",
            "label": "Tele",
            "current": True,
            "isoMin": 100,
            "isoMax": 1600,
            "shutterMinNs": 200_000,
            "shutterMaxNs": 500_000_000,
            "supportsManualSensor": True,
            "supportsManualWB": True,
            "supportsManualFocus": True,
            "minFocusDistance": 5.0,
            "supportsFlash": True,
            "hasOis": False,
            "hwLevel": "FULL",
            "aeCompMin": -3,
            "aeCompMax": 4,
            "aeCompStep": 0.5,
        }],
        "auto": False,
        "iso": 400,
        "shutter_ns": 10_000_000,
        "wb_manual": True,
        "ois": False,
        "focus_mode": "manual",
        "focus_distance": 2.5,
        "ae_comp": 2,
        "nr_mode": 2,
        "edge_mode": 0,
        "black_level_lock": True,
        "torch": True,
    }

    plugin.on_phone_state(state)

    assert plugin._lens_panel._btns[0].isChecked()
    assert plugin._manual_exp is True
    assert plugin._manual_wb is True
    assert plugin._manual_focus is True
    assert plugin._iso_slider.get_value() == pytest.approx(400, rel=0.01)
    assert plugin._sht_slider.get_value() == pytest.approx(10_000_000, rel=0.01)
    assert plugin._ae_comp_slider.minimum() == -3
    assert plugin._ae_comp_slider.maximum() == 4
    assert plugin._ae_comp_lbl.text() == "+1.0 EV"
    assert plugin._focus_slider.value() == 500
    assert not plugin._ois_cb.isEnabled()
    assert plugin._torch_btn.isEnabled()
    assert "FULL" in plugin._cam_info_lbl.text()


def test_empty_phone_state_and_stop_clear_camera_ui(camera_plugin):
    plugin, _host, _bus, _panel = camera_plugin
    plugin.on_phone_state({})
    assert plugin._lens_panel.layout().itemAt(0).widget().text() == "Unavailable"

    plugin._cam_info_lbl.setText("info")
    plugin._ctrl = _Ctrl()
    plugin.on_stream_stop()
    assert plugin._ctrl is None
    assert plugin._lens_panel._btns == []
    assert plugin._lens_panel._ph.text() == "Start streaming to load lenses"
    assert plugin._cam_info_lbl.text() == ""


def test_capability_gating_forces_unsupported_modes_off(camera_plugin):
    plugin, _host, _bus, _panel = camera_plugin
    plugin._rb_exp_manual.setChecked(True)
    plugin._rb_wb_manual.setChecked(True)
    plugin._rb_focus_manual.setChecked(True)

    plugin._update_camera_caps(False, False, False, 0, False, False)

    assert not plugin._rb_exp_manual.isEnabled()
    assert not plugin._rb_exp_manual.isChecked()
    assert not plugin._rb_wb_manual.isEnabled()
    assert not plugin._rb_focus_manual.isEnabled()
    assert not plugin._torch_btn.isEnabled()
    assert not plugin._ois_cb.isEnabled()
    assert plugin._focus_max_diopters == 10


def test_lens_selection_updates_ranges_caps_and_sends_camera(camera_plugin):
    plugin, _host, _bus, _panel = camera_plugin
    ctrl = _Ctrl()
    plugin._ctrl = ctrl
    cam = {
        "id": "wide", "label": "Wide", "isoMin": 80, "isoMax": 800,
        "shutterMinNs": 300_000, "shutterMaxNs": 300_000_000,
        "aeCompMin": -2, "aeCompMax": 2, "aeCompStep": 1.0,
    }

    plugin._on_lens_selected(cam)

    assert ctrl.sent[0] == {"action": "camera", "id": "wide"}
    assert plugin._iso_slider.v_min == 80
    assert plugin._iso_slider.v_max == 800
    assert plugin._ae_comp_slider.minimum() == -2
    assert plugin._ae_comp_slider.maximum() == 2


def test_focus_slider_conversion_and_manual_send(camera_plugin):
    plugin, _host, _bus, _panel = camera_plugin
    ctrl = _Ctrl()
    plugin._ctrl = ctrl
    plugin._focus_max_diopters = 8
    plugin._manual_focus = True

    plugin._on_focus_slider(250)

    assert plugin._slider_to_diopters(250) == 2
    assert plugin._focus_val_lbl.text() == "0.50 m"
    assert ctrl.sent[-1] == {"action": "focus_distance", "value": 2}

    plugin._set_focus_slider_value(100)
    assert plugin._focus_slider.value() == 1000


def test_camera_config_round_trip_for_supported_values(camera_plugin):
    plugin, _host, _bus, _panel = camera_plugin
    plugin._focus_max_diopters = 10
    cfg = {
        "exp_manual": True,
        "iso": 500,
        "shutter_ns": 12_000_000,
        "ois": False,
        "focus_manual": True,
        "focus_diopters": 3,
        "wb_manual": True,
        "wb_kelvin": 6500,
        "wb_tint": -20,
        "ae_comp": 2,
        "nr_mode": 2,
        "edge_mode": 0,
        "bll": True,
    }

    plugin.set_config(cfg)
    got = plugin.get_config()

    for key in ("exp_manual", "ois", "focus_manual", "wb_manual", "wb_kelvin",
                "wb_tint", "ae_comp", "nr_mode", "edge_mode", "bll"):
        assert got[key] == cfg[key]
    assert got["iso"] == pytest.approx(cfg["iso"], rel=0.01)
    assert got["shutter_ns"] == pytest.approx(cfg["shutter_ns"], rel=0.01)
    assert got["focus_diopters"] == pytest.approx(cfg["focus_diopters"], rel=0.01)


def test_saved_manual_modes_are_pushed_to_phone_on_stream_start(camera_plugin):
    plugin, _host, _bus, _panel = camera_plugin
    plugin.set_config({
        "exp_manual": True,
        "iso": 400,
        "shutter_ns": 5_000_000,
        "focus_manual": True,
        "focus_diopters": 2,
    })
    ctrl = _Ctrl()

    plugin.on_stream_start("url", ctrl)

    actions = [item["action"] for item in ctrl.sent]
    assert "iso" in actions
    assert "shutter" in actions
    assert {"action": "focus_mode", "value": "manual"} in ctrl.sent


def test_default_config_resets_boolean_camera_settings(camera_plugin):
    plugin, _host, _bus, _panel = camera_plugin
    plugin.set_config({
        "exp_manual": True,
        "focus_manual": True,
        "wb_manual": True,
        "bll": True,
    })

    plugin.set_config({
        "exp_manual": False,
        "focus_manual": False,
        "wb_manual": False,
        "bll": False,
    })

    cfg = plugin.get_config()
    assert cfg["exp_manual"] is False
    assert cfg["focus_manual"] is False
    assert cfg["wb_manual"] is False
    assert cfg["bll"] is False
