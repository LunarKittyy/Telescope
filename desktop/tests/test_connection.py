import pytest

from telescope.plugins.connection import _rank_ip


@pytest.mark.parametrize("ip,expected_rank", [
    ("100.64.0.5", 0),   # Tailscale CGNAT
    ("100.127.255.255", 0),
    ("10.0.0.1", 1),
    ("192.168.1.1", 1),
    ("172.16.0.1", 1),   # RFC 1918 lower bound of 172.16.0.0/12
    ("172.31.255.255", 1),  # RFC 1918 upper bound
    ("172.15.0.1", 2),   # just below the RFC 1918 172.x range - not private
    ("172.32.0.1", 2),   # just above the RFC 1918 172.x range - not private
    ("8.8.8.8", 2),
])
def test_rank_ip(ip, expected_rank):
    assert _rank_ip(ip) == expected_rank


@pytest.fixture
def window_with_plugins(qapp, config_home):
    from telescope.app import TelescopeWindow
    from telescope.plugins.camera_control import CameraControlPlugin
    from telescope.plugins.connection import ConnectionPlugin
    from telescope.plugins.monitoring import MonitoringPlugin
    from telescope.plugins.stream_output import StreamOutputPlugin
    from telescope.plugins.transforms import TransformsPlugin

    win = TelescopeWindow()
    conn = ConnectionPlugin()
    cam = CameraControlPlugin()
    for p in (conn, cam, StreamOutputPlugin(), TransformsPlugin(), MonitoringPlugin()):
        win.register_plugin(p)
    win.apply_saved_config()
    return win, conn, cam


def test_usb_only_session_gets_its_own_persisted_profile(window_with_plugins):
    win, conn, cam = window_with_plugins
    assert conn.selected_device == "__usb__"
    assert conn._active_key == "__usb__"


def test_switching_wifi_device_resets_to_defaults_then_applies_profile(window_with_plugins):
    win, conn, cam = window_with_plugins

    conn._rb_wifi.setChecked(True)
    conn._rb_usb.setChecked(False)
    conn._on_mode()
    conn._devices = [
        {"name": "PhoneA", "ips": ["192.168.1.10"]},
        {"name": "PhoneB", "ips": ["192.168.1.20"]},
    ]
    conn._refresh_device_combo(select_name="PhoneA")
    conn._selected_device = "PhoneA"
    conn._activate_profile(conn._profile_key)

    default_iso = cam.get_config()["iso"]

    cam._rb_exp_manual.setChecked(True)
    cam._iso_slider.set_value(400)
    assert cam.get_config()["iso"] == pytest.approx(400, abs=1)

    # Switching to PhoneB (which has no saved profile yet) must reset to
    # defaults, not inherit PhoneA's iso=400.
    conn._on_device_changed(1)
    assert conn._active_key == "PhoneB"
    assert cam.get_config()["iso"] == pytest.approx(default_iso, abs=1)

    # Switching back to PhoneA must restore its saved iso=400.
    conn._on_device_changed(0)
    assert conn._active_key == "PhoneA"
    assert cam.get_config()["iso"] == pytest.approx(400, abs=1)


def test_renaming_selected_device_preserves_settings_and_moves_config_key(window_with_plugins):
    win, conn, cam = window_with_plugins

    conn._rb_wifi.setChecked(True)
    conn._rb_usb.setChecked(False)
    conn._on_mode()
    conn._devices = [{"name": "PhoneA", "ips": ["192.168.1.10"]}]
    conn._refresh_device_combo(select_name="PhoneA")
    conn._selected_device = "PhoneA"
    conn._activate_profile(conn._profile_key)

    cam._rb_exp_manual.setChecked(True)
    cam._iso_slider.set_value(555)

    conn._on_device_edited("PhoneA", {"name": "PhoneAlpha", "ips": ["192.168.1.10"]})
    conn._devices[0] = {"name": "PhoneAlpha", "ips": ["192.168.1.10"]}

    from telescope.config import load_config
    cfg = load_config()
    assert "PhoneA" not in cfg.get("devices", {})
    assert "PhoneAlpha" in cfg.get("devices", {})
    assert cfg["selected_device"] == "PhoneAlpha"
    assert cam.get_config()["iso"] == pytest.approx(555, abs=1)


def test_ip_change_persists_active_ip_per_device(window_with_plugins):
    win, conn, cam = window_with_plugins

    conn._rb_wifi.setChecked(True)
    conn._rb_usb.setChecked(False)
    conn._on_mode()
    conn._devices = [{"name": "PhoneA", "ips": ["192.168.1.10", "100.64.0.5"]}]
    conn._refresh_device_combo(select_name="PhoneA")
    conn._selected_device = "PhoneA"
    conn._activate_profile(conn._profile_key)

    # Tailscale IP should be auto-selected first (best-ranked).
    assert conn._current_device_ip() == "100.64.0.5"

    conn._ip_combo.setCurrentIndex(1)
    assert conn._current_device_ip() == "192.168.1.10"

    from telescope.config import load_config
    cfg = load_config()
    assert cfg["devices"]["PhoneA"]["active_ip"] == "192.168.1.10"
