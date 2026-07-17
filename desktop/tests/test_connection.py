from types import SimpleNamespace

import pytest
from PyQt6.QtWidgets import QMessageBox

import telescope.plugins.connection as connection_module
from telescope.plugin import EventBus
from telescope.plugins.connection import (
    ConnectionPlugin,
    USB_PROFILE_KEY,
    _DeviceDialog,
    _best_ip,
    _extract_ip,
    _rank_ip,
    _valid_ipv4,
)


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


def test_best_ip_prefers_tailscale_then_lan_and_handles_empty():
    assert _best_ip([]) is None
    assert _best_ip(["8.8.8.8", "192.168.1.2", "100.64.1.2"]) == "100.64.1.2"


@pytest.mark.parametrize(
    "raw,expected",
    [
        (" 1.2.3.4 ", "1.2.3.4"),
        ("http://1.2.3.4:8080/video", "1.2.3.4"),
        ("https://10.0.0.1/path", "10.0.0.1"),
    ],
)
def test_extract_ip(raw, expected):
    assert _extract_ip(raw) == expected


@pytest.mark.parametrize(
    "ip,valid",
    [
        ("0.0.0.0", True),
        ("255.255.255.255", True),
        ("1.2.3.4", True),
        ("256.2.3.4", False),
        ("01.2.3.4", False),
        ("1.2.3", False),
        ("a.b.c.d", False),
    ],
)
def test_valid_ipv4(ip, valid):
    assert _valid_ipv4(ip) is valid


def test_get_local_ips_deduplicates_filters_loopback_and_ranks(monkeypatch):
    class DatagramSocket:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            pass

        def connect(self, _address):
            pass

        def getsockname(self):
            return "100.64.0.9", 999

    monkeypatch.setattr(connection_module.socket, "gethostname", lambda: "host")
    monkeypatch.setattr(
        connection_module.socket,
        "getaddrinfo",
        lambda *_args: [
            (None, None, None, None, ("127.0.0.1", 0)),
            (None, None, None, None, ("192.168.1.2", 0)),
            (None, None, None, None, ("192.168.1.2", 0)),
        ],
    )
    monkeypatch.setattr(connection_module.socket, "socket", lambda *_args: DatagramSocket())

    assert connection_module._get_local_ips() == ["100.64.0.9", "192.168.1.2"]


def test_get_local_ips_tolerates_both_discovery_failures(monkeypatch):
    monkeypatch.setattr(
        connection_module.socket,
        "getaddrinfo",
        lambda *_args: (_ for _ in ()).throw(OSError()),
    )
    monkeypatch.setattr(
        connection_module.socket,
        "socket",
        lambda *_args: (_ for _ in ()).throw(OSError()),
    )
    assert connection_module._get_local_ips() == []


def test_device_dialog_parses_urls_deduplicates_and_returns_device(qapp):
    dialog = _DeviceDialog(existing_names=["Other"])
    dialog._name_edit.setText(" Phone ")
    dialog._ips_edit.setPlainText("http://192.168.1.5:8080/video\n100.64.0.2")

    assert dialog._parse_ips() == ["192.168.1.5", "100.64.0.2"]
    dialog._on_accept()
    assert dialog.result_device() == {
        "name": "Phone",
        "ips": ["192.168.1.5", "100.64.0.2"],
    }


@pytest.mark.parametrize(
    "name,ips,error",
    [
        ("", "1.2.3.4", "name"),
        ("Taken", "1.2.3.4", "already exists"),
        ("New", "", "IP"),
        ("New", "bad", "Invalid"),
    ],
)
def test_device_dialog_validation(qapp, name, ips, error):
    dialog = _DeviceDialog(existing_names=["Taken"])
    dialog._name_edit.setText(name)
    dialog._ips_edit.setPlainText(ips)

    dialog._on_accept()

    assert error.lower() in dialog._err_lbl.text().lower()


class _ConnectionHost:
    def __init__(self):
        self.saves = 0
        self.switches = []
        self.reconnects = 0

    def _schedule_save(self):
        self.saves += 1

    def _save_config(self):
        self.saves += 1

    def _switch_device(self, previous, new):
        self.switches.append((previous, new))

    def reconnect_stream(self):
        self.reconnects += 1


@pytest.fixture
def connection_plugin(qapp, config_home):
    host = _ConnectionHost()
    plugin = ConnectionPlugin()
    plugin.setup(host, EventBus())
    panel = plugin.create_panel()
    return plugin, host, panel


def test_connection_config_migrates_old_ip_list_and_selects_profile(connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin.set_config({
        "mode": "wifi",
        "port": 9000,
        "devices_list": [{"name": "Old", "ip": "1.2.3.4"}],
    })
    plugin.select_device("Old")

    assert plugin.get_config() == {
        "mode": "wifi",
        "port": "9000",
        "devices_list": [{"name": "Old", "ips": ["1.2.3.4"]}],
    }
    assert plugin.selected_device == "Old"
    assert plugin._current_device_ip() == "1.2.3.4"


def test_connection_select_device_defaults_first_wifi_device(connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin.set_config({
        "mode": "wifi",
        "devices_list": [
            {"name": "A", "ips": ["10.0.0.1"]},
            {"name": "B", "ips": ["10.0.0.2"]},
        ],
    })
    plugin.select_device(None)
    assert plugin.selected_device == "A"


def test_connection_usb_profile_and_profile_switch_deduplication(connection_plugin):
    plugin, host, _panel = connection_plugin
    assert plugin.selected_device == USB_PROFILE_KEY
    plugin._active_key = USB_PROFILE_KEY
    plugin._activate_profile(USB_PROFILE_KEY)
    assert host.switches == []
    plugin._activate_profile("Phone")
    assert host.switches == [(USB_PROFILE_KEY, "Phone")]


def test_wifi_stream_info_and_missing_device(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "IS_LINUX", False)
    critical = []
    monkeypatch.setattr(
        connection_module.QMessageBox,
        "critical",
        lambda *_args: critical.append(_args),
    )
    plugin.set_config({"mode": "wifi", "port": "8123", "devices_list": []})
    plugin.select_device(None)
    assert plugin.get_stream_info() == (None, False)
    assert critical[-1][1] == "No device"

    plugin.set_config({
        "mode": "wifi", "port": "8123",
        "devices_list": [{"name": "Phone", "ips": ["10.0.0.5"]}],
    })
    plugin.select_device("Phone")
    assert plugin.get_stream_info() == ("http://10.0.0.5:8123/video", True)


def test_bad_port_is_rejected(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "IS_LINUX", False)
    seen = []
    monkeypatch.setattr(
        connection_module.QMessageBox,
        "critical",
        lambda *_args: seen.append(_args),
    )
    plugin._port_field.setText("not-a-number")
    assert plugin.get_stream_info() == (None, False)
    assert seen[0][1] == "Bad port"


def test_usb_stream_info_forwards_specific_device_and_unforwards(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "IS_LINUX", False)
    monkeypatch.setattr(connection_module, "adb_available", lambda: True)
    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["serial-1"])
    forwards = []
    unforwards = []
    monkeypatch.setattr(
        connection_module,
        "adb_forward",
        lambda port, serial: forwards.append((port, serial)) or (True, "ok"),
    )
    monkeypatch.setattr(
        connection_module,
        "adb_unforward",
        lambda port, serial: unforwards.append((port, serial)),
    )
    plugin._port_field.setText("8081")

    assert plugin.get_stream_info() == ("http://localhost:8081/video", True)
    assert forwards == [(8081, "serial-1")]
    plugin.on_stream_stop()
    plugin.on_stream_stop()
    assert unforwards == [(8081, "serial-1")]


def test_usb_stream_info_rejects_missing_adb_and_forward_failure(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "IS_LINUX", False)
    errors = []
    monkeypatch.setattr(
        connection_module.QMessageBox,
        "critical",
        lambda *_args: errors.append(_args),
    )
    monkeypatch.setattr(connection_module, "adb_available", lambda: False)
    assert plugin.get_stream_info() == (None, False)
    assert errors[-1][1] == "ADB not found"

    monkeypatch.setattr(connection_module, "adb_available", lambda: True)
    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["serial"])
    monkeypatch.setattr(connection_module, "adb_forward", lambda *_args, **_kwargs: (False, "denied"))
    assert plugin.get_stream_info() == (None, False)
    assert errors[-1][1] == "ADB forward failed"


def test_resolve_adb_serial_none_single_multiple_and_cancel(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    errors = []
    monkeypatch.setattr(connection_module.QMessageBox, "critical", lambda *_args: errors.append(_args))
    monkeypatch.setattr(connection_module, "adb_devices", lambda: [])
    assert plugin._resolve_adb_serial() is None
    assert errors[-1][1] == "No ADB device"

    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["only"])
    assert plugin._resolve_adb_serial() == "only"

    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["a", "b"])
    monkeypatch.setattr(connection_module.QInputDialog, "getItem", lambda *_args: ("b", True))
    assert plugin._resolve_adb_serial() == "b"
    monkeypatch.setattr(connection_module.QInputDialog, "getItem", lambda *_args: ("a", False))
    assert plugin._resolve_adb_serial() is None


def test_linux_virtual_camera_conflict_and_cancel(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "IS_LINUX", True)
    monkeypatch.setattr(connection_module, "v4l2_devices_ready", lambda: False)
    monkeypatch.setattr(connection_module, "v4l2_module_loaded", lambda: True)
    warnings = []
    monkeypatch.setattr(connection_module.QMessageBox, "warning", lambda *_args: warnings.append(_args))
    assert plugin.get_stream_info() == (None, False)
    assert warnings[-1][1] == "v4l2loopback conflict"

    monkeypatch.setattr(connection_module, "v4l2_module_loaded", lambda: False)
    monkeypatch.setattr(
        connection_module.QMessageBox,
        "question",
        lambda *_args: QMessageBox.StandardButton.Cancel,
    )
    assert plugin.get_stream_info() == (None, False)


def test_linux_virtual_camera_load_failure_and_success(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "IS_LINUX", True)
    monkeypatch.setattr(connection_module, "v4l2_devices_ready", lambda: False)
    monkeypatch.setattr(connection_module, "v4l2_module_loaded", lambda: False)
    monkeypatch.setattr(
        connection_module.QMessageBox,
        "question",
        lambda *_args: QMessageBox.StandardButton.Ok,
    )
    errors = []
    monkeypatch.setattr(connection_module.QMessageBox, "critical", lambda *_args: errors.append(_args))
    monkeypatch.setattr(connection_module, "v4l2_load", lambda: (False, "denied"))
    assert plugin.get_stream_info() == (None, False)
    assert errors[-1][1] == "Load failed"

    monkeypatch.setattr(connection_module, "v4l2_load", lambda: (True, "ok"))
    monkeypatch.setattr(connection_module, "adb_available", lambda: True)
    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["serial"])
    monkeypatch.setattr(connection_module, "adb_forward", lambda *_args, **_kwargs: (True, "ok"))
    assert plugin.get_stream_info()[1] is True


def test_ip_and_port_changes_persist_and_reconnect(connection_plugin, config_home):
    plugin, host, _panel = connection_plugin
    plugin.set_config({
        "mode": "wifi",
        "devices_list": [{"name": "Phone", "ips": ["10.0.0.1", "10.0.0.2"]}],
    })
    plugin.select_device("Phone")
    plugin._on_ip_changed("10.0.0.2")
    assert config_home.load_config()["devices"]["Phone"]["active_ip"] == "10.0.0.2"
    assert host.reconnects == 1

    plugin._on_ip_changed("10.0.0.2")
    assert host.reconnects == 1

    plugin._port_field.setText("9000")
    plugin._on_port_changed()
    assert host.reconnects == 2
    plugin._on_port_changed()
    assert host.reconnects == 2


def test_pairing_adds_or_updates_device(connection_plugin):
    plugin, host, _panel = connection_plugin
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    plugin._on_device_paired("Phone", ["10.0.0.1"])
    assert plugin._devices == [{"name": "Phone", "ips": ["10.0.0.1"]}]
    assert plugin.selected_device == "Phone"
    plugin._on_device_paired("Phone", ["100.64.0.1"])
    assert plugin._devices == [{"name": "Phone", "ips": ["100.64.0.1"]}]
    assert host.saves == 2


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
