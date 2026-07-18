from types import SimpleNamespace

import pytest
from PyQt6.QtWidgets import QMessageBox

import telescope.ip_utils as ip_utils_module
import telescope.plugins.connection as connection_module
from telescope.plugin import EventBus
from telescope.plugins.connection import (
    ConnectionPlugin,
    USB_PROFILE_KEY,
    _DeviceDialog,
    _DeviceManagerDialog,
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

    monkeypatch.setattr(ip_utils_module.socket, "gethostname", lambda: "host")
    monkeypatch.setattr(
        ip_utils_module.socket,
        "getaddrinfo",
        lambda *_args: [
            (None, None, None, None, ("127.0.0.1", 0)),
            (None, None, None, None, ("192.168.1.2", 0)),
            (None, None, None, None, ("192.168.1.2", 0)),
        ],
    )
    monkeypatch.setattr(ip_utils_module.socket, "socket", lambda *_args: DatagramSocket())

    assert connection_module._get_local_ips() == ["100.64.0.9", "192.168.1.2"]


def test_get_local_ips_tolerates_both_discovery_failures(monkeypatch):
    monkeypatch.setattr(
        ip_utils_module.socket,
        "getaddrinfo",
        lambda *_args: (_ for _ in ()).throw(OSError()),
    )
    monkeypatch.setattr(
        ip_utils_module.socket,
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
        self._worker = None
        self.stops = 0

    def _schedule_save(self):
        self.saves += 1

    def _save_config(self):
        self.saves += 1

    def _switch_device(self, previous, new):
        self.switches.append((previous, new))

    def reconnect_stream(self):
        self.reconnects += 1

    def _stop(self):
        self.stops += 1
        self._worker = None


@pytest.fixture
def connection_plugin(qapp, config_home, monkeypatch):
    # Real pair-status probes shell out to adb and/or make a network call
    # with a multi-second timeout, from a background thread - fine in the
    # running app, but a real thread that outlives this test's plugin/qapp
    # teardown is a guaranteed PyQt abort (a queued cross-thread signal
    # delivered to an already-destroyed receiver). set_config()/_on_mode()/
    # _on_device_paired() all trigger a check incidentally, so silence it by
    # default here; tests of the probe itself re-arm it explicitly below.
    monkeypatch.setattr(ConnectionPlugin, "_spawn_pair_probe", lambda self, *a: None)
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
        "selected_device_name": "Old",
    }
    assert plugin.selected_device == "Old"
    assert plugin._current_device_ip() == "1.2.3.4"


def test_connection_set_config_discards_malformed_device_entries(connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin.set_config({
        "mode": "wifi",
        "devices_list": [
            {"name": "Good", "ips": ["10.0.0.1"], "token": "tok"},
            {"name": ""},  # empty name
            {"ips": ["10.0.0.2"]},  # missing name
            {"name": "BadIps", "ips": "not-a-list"},
            "not-a-dict",
            {"name": "BadToken", "ips": ["10.0.0.3"], "token": 42},
        ],
    })
    assert plugin._devices == [{"name": "Good", "ips": ["10.0.0.1"], "token": "tok"}]


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
    assert plugin.get_stream_info() == (None, None, False)
    assert critical[-1][1] == "Not paired"

    plugin.set_config({
        "mode": "wifi", "port": "8123",
        "devices_list": [{"name": "Phone", "ips": ["10.0.0.5"], "token": "tok-123"}],
    })
    plugin.select_device("Phone")
    assert plugin.get_stream_info() == ("http://10.0.0.5:8123/v1/video", "tok-123", True)


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
    assert plugin.get_stream_info() == (None, None, False)
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
    plugin._on_device_paired("Phone", ["10.0.0.5"], "tok-usb")
    plugin._port_field.setText("8081")

    assert plugin.get_stream_info() == ("http://localhost:8081/v1/video", "tok-usb", True)
    assert forwards == [(8081, "serial-1")]
    plugin.on_stream_stop()
    plugin.on_stream_stop()
    assert unforwards == [(8081, "serial-1")]


def test_usb_stream_info_requires_pairing_first(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "IS_LINUX", False)
    errors = []
    monkeypatch.setattr(
        connection_module.QMessageBox,
        "critical",
        lambda *_args: errors.append(_args),
    )
    assert plugin.get_stream_info() == (None, None, False)
    assert errors[-1][1] == "Not paired"


def test_usb_stream_info_rejects_missing_adb_and_forward_failure(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "IS_LINUX", False)
    plugin._on_device_paired("Phone", ["10.0.0.5"], "tok-usb")
    errors = []
    monkeypatch.setattr(
        connection_module.QMessageBox,
        "critical",
        lambda *_args: errors.append(_args),
    )
    monkeypatch.setattr(connection_module, "adb_available", lambda: False)
    assert plugin.get_stream_info() == (None, None, False)
    assert errors[-1][1] == "ADB not found"

    monkeypatch.setattr(connection_module, "adb_available", lambda: True)
    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["serial"])
    monkeypatch.setattr(connection_module, "adb_forward", lambda *_args, **_kwargs: (False, "denied"))
    assert plugin.get_stream_info() == (None, None, False)
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


def test_device_row_visibility_toggles_with_mode(connection_plugin):
    # The panel's never shown as a real top-level window in this fixture, so
    # isVisible() would reflect the (never-shown) ancestor chain rather than
    # what setVisible() was actually called with - isHidden() reads the
    # widget's own explicit flag instead.
    plugin, _host, _panel = connection_plugin
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    plugin._on_mode()
    assert not plugin._device_row_w.isHidden()
    assert not plugin._qr_btn.isHidden()  # pairing is available in both modes

    plugin._rb_usb.setChecked(True)
    plugin._rb_wifi.setChecked(False)
    plugin._on_mode()
    assert plugin._device_row_w.isHidden()
    assert not plugin._qr_btn.isHidden()


def test_on_pair_qr_usb_mode_requires_adb_and_resolves_serial(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin._rb_usb.setChecked(True)
    plugin._rb_wifi.setChecked(False)

    errors = []
    monkeypatch.setattr(connection_module.QMessageBox, "critical", lambda *_args: errors.append(_args))
    monkeypatch.setattr(connection_module, "adb_available", lambda: False)
    plugin._on_pair_qr()
    assert errors[-1][1] == "ADB not found"
    assert plugin._pairing_dlg is None

    # _PairingDialog's own adb-reverse behavior is covered directly in
    # test_connection_dialogs.py (constructed with a real QWidget-less
    # parent there); this test only cares that _on_pair_qr resolves a serial
    # and passes it through, so the dialog itself is stubbed out - the
    # fixture's fake host isn't a QWidget and can't be a real QDialog parent.
    monkeypatch.setattr(connection_module, "adb_available", lambda: True)
    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["serial-1"])
    captured = {}

    class _FakeDialog:
        def __init__(self, parent, on_paired, usb_serial=None):
            captured["usb_serial"] = usb_serial

        def setAttribute(self, *_a): pass
        def setWindowModality(self, *_a): pass
        def show(self): pass
        def raise_(self): pass
        def activateWindow(self): pass
        def isVisible(self): return False

    monkeypatch.setattr(connection_module, "_PairingDialog", _FakeDialog)
    plugin._on_pair_qr()
    assert captured["usb_serial"] == "serial-1"


def test_on_pair_qr_usb_mode_cancelled_serial_picker_skips_dialog(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin._rb_usb.setChecked(True)
    plugin._rb_wifi.setChecked(False)
    monkeypatch.setattr(connection_module, "adb_available", lambda: True)
    monkeypatch.setattr(connection_module, "adb_devices", lambda: [])
    monkeypatch.setattr(connection_module.QMessageBox, "critical", lambda *_args: None)

    plugin._on_pair_qr()
    assert plugin._pairing_dlg is None


def test_manage_devices_add_button_wired_to_pairing_flow(monkeypatch, connection_plugin):
    # _DeviceManagerDialog's parent must be a real QWidget; the fixture's
    # fake host isn't one, so the dialog itself is stubbed out here too - see
    # the identical note on test_on_pair_qr_usb_mode_requires_adb_and_resolves_serial.
    plugin, _host, _panel = connection_plugin
    captured = {}

    class _FakeManagerDialog:
        def __init__(self, parent, devices, on_add, on_edit, on_remove):
            captured["on_add"] = on_add

        def setAttribute(self, *_a): pass
        def setWindowModality(self, *_a): pass
        def show(self): pass
        def raise_(self): pass
        def activateWindow(self): pass
        def isVisible(self): return False

    monkeypatch.setattr(connection_module, "_DeviceManagerDialog", _FakeManagerDialog)
    plugin._on_manage_devices()

    assert captured["on_add"] == plugin._on_pair_qr


def test_linux_virtual_camera_conflict_and_cancel(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "IS_LINUX", True)
    monkeypatch.setattr(connection_module, "v4l2_devices_ready", lambda: False)
    monkeypatch.setattr(connection_module, "v4l2_module_loaded", lambda: True)
    warnings = []
    monkeypatch.setattr(connection_module.QMessageBox, "warning", lambda *_args: warnings.append(_args))
    assert plugin.get_stream_info() == (None, None, False)
    assert warnings[-1][1] == "v4l2loopback conflict"

    monkeypatch.setattr(connection_module, "v4l2_module_loaded", lambda: False)
    monkeypatch.setattr(
        connection_module.QMessageBox,
        "question",
        lambda *_args: QMessageBox.StandardButton.Cancel,
    )
    assert plugin.get_stream_info() == (None, None, False)


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
    assert plugin.get_stream_info() == (None, None, False)
    assert errors[-1][1] == "Load failed"

    plugin._on_device_paired("Phone", ["10.0.0.5"], "tok-linux")
    monkeypatch.setattr(connection_module, "v4l2_load", lambda: (True, "ok"))
    monkeypatch.setattr(connection_module, "adb_available", lambda: True)
    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["serial"])
    monkeypatch.setattr(connection_module, "adb_forward", lambda *_args, **_kwargs: (True, "ok"))
    assert plugin.get_stream_info()[2] is True


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
    plugin._on_device_paired("Phone", ["10.0.0.1"], "tok-a")
    assert plugin._devices == [{"name": "Phone", "ips": ["10.0.0.1"], "token": "tok-a"}]
    assert plugin.selected_device == "Phone"
    # Re-pairing rotates the token, revoking the old one.
    plugin._on_device_paired("Phone", ["100.64.0.1"], "tok-b")
    assert plugin._devices == [{"name": "Phone", "ips": ["100.64.0.1"], "token": "tok-b"}]
    assert host.saves == 2


def test_pairing_stops_an_active_stream(connection_plugin):
    plugin, host, _panel = connection_plugin
    host._worker = object()
    plugin._on_device_paired("Phone", ["10.0.0.1"], "tok-a")
    assert host.stops == 1


def test_pairing_does_not_stop_when_nothing_is_streaming(connection_plugin):
    plugin, host, _panel = connection_plugin
    plugin._on_device_paired("Phone", ["10.0.0.1"], "tok-a")
    assert host.stops == 0


def test_pair_status_shows_not_paired_without_a_token(connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin._check_pair_status()
    assert plugin._pair_status_lbl.text() == "○ Not paired"


def test_pair_status_pins_to_paired_and_stops_polling_while_streaming(connection_plugin):
    # PingServer only lives while MainActivity is foregrounded, unlike the
    # streaming server - probing it mid-stream would wrongly flag a
    # perfectly fine minimized session as unreachable.
    plugin, _host, _panel = connection_plugin
    assert plugin._pair_status_timer.isActive()
    plugin.on_stream_start("http://localhost:8080/v1/video", object())
    assert plugin._pair_status_lbl.text() == "● Paired"
    assert not plugin._pair_status_timer.isActive()


def test_pair_status_check_short_circuits_while_streaming(monkeypatch, connection_plugin):
    plugin, host, _panel = connection_plugin
    _arm_synchronous_pair_probe(monkeypatch)
    probed = []
    monkeypatch.setattr(ConnectionPlugin, "_probe_url", staticmethod(lambda url, token: probed.append(url) or "not_paired"))
    host._worker = object()
    plugin._devices = [{"name": "Phone", "ips": ["10.0.0.1"], "token": "tok-a"}]
    plugin._selected_device = "Phone"
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    plugin._check_pair_status()
    assert plugin._pair_status_lbl.text() == "● Paired"
    assert probed == []


def test_pair_status_resumes_polling_on_stream_stop(connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin.on_stream_start("http://localhost:8080/v1/video", object())
    assert not plugin._pair_status_timer.isActive()
    plugin.on_stream_stop()
    assert plugin._pair_status_timer.isActive()


def _arm_synchronous_pair_probe(monkeypatch):
    # The fixture silences _spawn_pair_probe (see connection_plugin) so
    # incidental checks from other tests can't leave a real background
    # thread in flight. Tests of the probe itself put it back, but
    # synchronous - same call, just on the calling thread instead of a
    # spawned one - so results land immediately and deterministically.
    monkeypatch.setattr(
        ConnectionPlugin, "_spawn_pair_probe",
        lambda self, *a: self._probe_pair_status(*a),
    )


def test_pair_status_wifi_paired(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _arm_synchronous_pair_probe(monkeypatch)
    monkeypatch.setattr(ConnectionPlugin, "_probe_url", staticmethod(lambda url, token: "paired"))
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    plugin._on_device_paired("Phone", ["10.0.0.1"], "tok-a")
    assert plugin._pair_status_lbl.text() == "● Paired"


def test_pair_status_wifi_stale_token(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _arm_synchronous_pair_probe(monkeypatch)
    monkeypatch.setattr(ConnectionPlugin, "_probe_url", staticmethod(lambda url, token: "not_paired"))
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    plugin._on_device_paired("Phone", ["10.0.0.1"], "tok-a")
    assert plugin._pair_status_lbl.text() == "○ Not paired"


def test_pair_status_wifi_without_an_ip_skips_probe(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _arm_synchronous_pair_probe(monkeypatch)
    probed = []
    monkeypatch.setattr(ConnectionPlugin, "_probe_url", staticmethod(lambda url, token: probed.append(url) or "paired"))
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    plugin._devices = [{"name": "Phone", "ips": [], "token": "tok-a"}]
    plugin._selected_device = "Phone"
    plugin._refresh_device_combo(select_name="Phone")
    plugin._check_pair_status()
    assert probed == []
    assert plugin._pair_status_lbl.text() == "○ Not paired"


def test_pair_status_usb_ambiguous_serial_shows_unknown(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _arm_synchronous_pair_probe(monkeypatch)
    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["a", "b"])
    forwards = []
    monkeypatch.setattr(connection_module, "adb_forward", lambda *a, **k: forwards.append((a, k)) or (True, "ok"))
    monkeypatch.setattr(ConnectionPlugin, "_probe_url", staticmethod(lambda url, token: "paired"))
    plugin._devices = [{"name": "Phone", "ips": ["10.0.0.1"], "token": "tok-a"}]
    plugin._selected_device = "Phone"
    plugin._rb_usb.setChecked(True)
    plugin._rb_wifi.setChecked(False)
    plugin._check_pair_status()
    assert plugin._pair_status_lbl.text() == ""
    assert forwards == []


def test_pair_status_usb_sets_up_and_tears_down_a_temporary_forward(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _arm_synchronous_pair_probe(monkeypatch)
    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["serial-1"])
    calls = []
    monkeypatch.setattr(connection_module, "adb_forward", lambda port, serial: calls.append(("forward", port, serial)) or (True, "ok"))
    monkeypatch.setattr(connection_module, "adb_unforward", lambda port, serial: calls.append(("unforward", port, serial)))
    monkeypatch.setattr(ConnectionPlugin, "_probe_url", staticmethod(lambda url, token: "paired"))
    plugin._rb_usb.setChecked(True)
    plugin._rb_wifi.setChecked(False)
    plugin._on_device_paired("Phone", ["10.0.0.1"], "tok-a")
    assert plugin._pair_status_lbl.text() == "● Paired"
    assert calls == [
        ("forward", connection_module.PING_PORT, "serial-1"),
        ("unforward", connection_module.PING_PORT, "serial-1"),
    ]


def test_pair_status_stale_result_is_discarded(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin._devices = [{"name": "Phone", "ips": ["10.0.0.1"], "token": "tok-a"}]
    plugin._selected_device = "Phone"
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    plugin._pair_status_check_id = 5
    # Simulate a check started earlier (check_id=1) finally completing after
    # a newer one has already started (_pair_status_check_id is now 5) -
    # its result must not clobber whatever the newer check already showed.
    plugin._probe_pair_status(1, "tok-a", usb=False)
    assert plugin._pair_status_lbl.text() == ""


def test_pairing_refreshes_an_open_device_manager_list(connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    dlg = _DeviceManagerDialog(
        None, plugin._devices,
        on_add=lambda: None, on_edit=lambda *_a: None, on_remove=lambda _n: None,
    )
    plugin._device_dlg = dlg
    dlg.show()

    plugin._on_device_paired("Phone", ["10.0.0.1"], "tok-a")

    assert dlg._list.count() == 1
    assert dlg._list.item(0).text().startswith("Phone  -")
    dlg.close()


def test_editing_a_paired_device_preserves_its_token(qapp):
    device = {"name": "Phone", "ips": ["10.0.0.1"], "token": "tok-keep"}
    dialog = _DeviceDialog(existing_names=[], device=device)
    dialog._name_edit.setText("PhoneRenamed")
    dialog._ips_edit.setPlainText("10.0.0.2")

    assert dialog.result_device() == {
        "name": "PhoneRenamed",
        "ips": ["10.0.0.2"],
        "token": "tok-keep",
    }


@pytest.fixture
def window_with_plugins(qapp, config_home, monkeypatch):
    from telescope.app import TelescopeWindow
    from telescope.plugins.camera_control import CameraControlPlugin
    from telescope.plugins.connection import ConnectionPlugin
    from telescope.plugins.monitoring import MonitoringPlugin
    from telescope.plugins.stream_output import StreamOutputPlugin
    from telescope.plugins.transforms import TransformsPlugin

    # See connection_plugin's fixture comment: a real pair-status probe
    # thread that outlives this test's plugin/qapp teardown is a guaranteed
    # PyQt abort, not just a lint warning.
    monkeypatch.setattr(ConnectionPlugin, "_spawn_pair_probe", lambda self, *a: None)
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


def test_paired_device_survives_restart_while_in_usb_mode(window_with_plugins):
    """A device paired over Wi-Fi, then left selected while the app is
    switched to USB mode, must still resolve to the same roster device (and
    its token) after a restart - the app-level persisted "selected device"
    is the USB pseudo-key in that mode, not a roster name, so restoring the
    roster selection from it directly used to silently fall back to
    whichever device sorts first and had no token."""
    from telescope.app import TelescopeWindow
    from telescope.plugins.camera_control import CameraControlPlugin
    from telescope.plugins.connection import ConnectionPlugin
    from telescope.plugins.monitoring import MonitoringPlugin
    from telescope.plugins.stream_output import StreamOutputPlugin
    from telescope.plugins.transforms import TransformsPlugin

    win, conn, cam = window_with_plugins
    conn._on_device_paired("Alpha", ["10.0.0.1"], "tok-alpha")
    conn._on_device_paired("V2413", ["10.0.0.2"], "tok-v2413")
    conn._rb_usb.setChecked(True)
    conn._rb_wifi.setChecked(False)
    conn._on_mode()
    win._save_config()

    win2 = TelescopeWindow()
    conn2 = ConnectionPlugin()
    for p in (conn2, CameraControlPlugin(), StreamOutputPlugin(), TransformsPlugin(), MonitoringPlugin()):
        win2.register_plugin(p)
    win2.apply_saved_config()

    assert conn2._selected_device == "V2413"
    assert conn2._current_device_token() == "tok-v2413"


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
