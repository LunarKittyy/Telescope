from pathlib import Path
from types import SimpleNamespace

import pytest
from PyQt6.QtWidgets import QMessageBox

import telescope.ip_utils as ip_utils_module
import telescope.plugins.connection as connection_module
from telescope.plugin import EventBus
from telescope.session_client import PingResult, SessionResult
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


def _fake_adapters(monkeypatch, adapters):
    """Stand in for ifaddr.get_adapters() with (nice_name, [(ip, is_IPv4)])
    pairs, in the order a real enumeration would report them."""
    fake = [
        SimpleNamespace(
            name=name,
            nice_name=name,
            ips=[SimpleNamespace(ip=ip, is_IPv4=is_v4) for ip, is_v4 in ips],
        )
        for name, ips in adapters
    ]
    monkeypatch.setattr(ip_utils_module.ifaddr, "get_adapters", lambda: fake)


def test_pairing_addresses_order_lan_then_tailscale_then_other(monkeypatch):
    _fake_adapters(monkeypatch, [
        ("tailscale0", [("100.90.12.34", True)]),
        ("eth9", [("203.0.113.7", True)]),
        ("wlan0", [("192.168.1.42", True)]),
        ("eth0", [("10.1.2.3", True)]),
    ])

    addresses = connection_module._get_pairing_addresses()

    assert [(a.ip, a.interface, a.kind) for a in addresses] == [
        # Physical LAN first, in enumeration order within the kind.
        ("192.168.1.42", "wlan0", "lan"),
        ("10.1.2.3", "eth0", "lan"),
        ("100.90.12.34", "tailscale0", "tailscale"),
        ("203.0.113.7", "eth9", "other"),
    ]


def test_pairing_addresses_put_a_vpn_tunnel_behind_the_real_lan(monkeypatch):
    # A desktop VPN handing out an RFC 1918 address classifies as LAN like
    # any other, but the phone can only reach the physical one - so it must
    # not end up ahead of it in the QR code.
    # Only tunnels whose adapter name gives them away get demoted; anything
    # unrecognised keeps its enumeration position, which costs the phone a
    # timeout at worst since it works through every candidate anyway.
    _fake_adapters(monkeypatch, [
        ("tun0", [("10.8.0.6", True)]),
        ("wg0", [("10.2.0.2", True)]),
        ("wlan0", [("192.168.1.42", True)]),
    ])

    addresses = ip_utils_module.get_pairing_addresses()

    assert [a.ip for a in addresses] == ["192.168.1.42", "10.8.0.6", "10.2.0.2"]
    # Still advertised, though: a tunnel is occasionally the only shared path.
    assert all(a.kind == "lan" for a in addresses)


@pytest.mark.parametrize("name,is_vpn", [
    ("tun0", True),
    ("wg0", True),
    ("utun3", True),
    ("NordLynx", True),
    ("ProtonVPN", True),
    ("Tailscale", True),
    ("wlan0", False),
    ("eth0", False),
    ("Wi-Fi", False),
    ("Ethernet 2", False),
])
def test_looks_like_vpn_interface(name, is_vpn):
    assert ip_utils_module.looks_like_vpn_interface(name) is is_vpn


def test_pairing_addresses_cover_every_private_range(monkeypatch):
    _fake_adapters(monkeypatch, [
        ("a", [("192.168.0.1", True)]),
        ("b", [("10.255.255.254", True)]),
        ("c", [("172.16.0.1", True)]),
        ("d", [("172.31.255.254", True)]),
        ("e", [("100.64.0.1", True)]),
        ("f", [("100.127.255.254", True)]),
    ])

    kinds = {a.ip: a.kind for a in ip_utils_module.get_pairing_addresses()}

    assert kinds == {
        "192.168.0.1": "lan",
        "10.255.255.254": "lan",
        "172.16.0.1": "lan",
        "172.31.255.254": "lan",
        "100.64.0.1": "tailscale",
        "100.127.255.254": "tailscale",
    }


def test_pairing_addresses_skip_loopback_link_local_ipv6_and_duplicates(monkeypatch):
    _fake_adapters(monkeypatch, [
        ("lo", [("127.0.0.1", True)]),
        ("wlan0", [
            ("169.254.10.11", True),          # link-local: no DHCP lease
            (("fe80::1", 0, 0), False),       # ifaddr reports IPv6 as a tuple
            ("192.168.1.42", True),
        ]),
        ("wlan0:1", [("192.168.1.42", True)]),  # same address, aliased adapter
    ])

    addresses = ip_utils_module.get_pairing_addresses()

    assert [(a.ip, a.interface) for a in addresses] == [("192.168.1.42", "wlan0")]


def test_pairing_addresses_skip_container_and_vm_only_adapters(monkeypatch):
    _fake_adapters(monkeypatch, [
        ("docker0", [("172.17.0.1", True)]),
        ("br-1a2b3c", [("172.18.0.1", True)]),
        ("veth3f9a", [("172.19.0.1", True)]),
        ("virbr0", [("192.168.122.1", True)]),
        ("vboxnet0", [("192.168.56.1", True)]),
        ("VMware Network Adapter VMnet8", [("192.168.75.1", True)]),
        ("VirtualBox Host-Only Network", [("192.168.99.1", True)]),
        ("Wi-Fi", [("192.168.1.42", True)]),
    ])

    assert [a.ip for a in ip_utils_module.get_pairing_addresses()] == ["192.168.1.42"]


def test_pairing_addresses_keeps_hyper_v_bridged_lan_adapter(monkeypatch):
    # Windows bridges a Hyper-V host's real LAN connection through an adapter
    # named "vEthernet (...)" - dropping those would strip the only address
    # the phone can reach on such a machine.
    _fake_adapters(monkeypatch, [("vEthernet (External)", [("192.168.1.42", True)])])

    assert [a.ip for a in ip_utils_module.get_pairing_addresses()] == ["192.168.1.42"]


def test_pairing_addresses_are_capped_and_keep_the_best_ones(monkeypatch):
    # Everything here goes into a QR code the phone has to read off a screen.
    _fake_adapters(monkeypatch, [
        *[(f"tailscale{i}", [(f"100.64.0.{i}", True)]) for i in range(8)],
        ("wlan0", [("192.168.1.42", True)]),
    ])

    addresses = ip_utils_module.get_pairing_addresses()

    assert len(addresses) == ip_utils_module.MAX_PAIRING_CANDIDATES == 8
    # The LAN address survives the cap even though it was enumerated last.
    assert addresses[0].ip == "192.168.1.42"


def test_pairing_addresses_trim_very_long_interface_names(monkeypatch):
    _fake_adapters(monkeypatch, [("Intel(R) Wi-Fi 6E AX211 160MHz Adapter #2", [("192.168.1.42", True)])])

    assert ip_utils_module.get_pairing_addresses()[0].interface == "Intel(R) Wi-Fi 6E AX211 160MHz A"


def test_pairing_addresses_tolerate_enumeration_failure(monkeypatch):
    monkeypatch.setattr(
        ip_utils_module.ifaddr,
        "get_adapters",
        lambda: (_ for _ in ()).throw(OSError()),
    )
    assert connection_module._get_pairing_addresses() == []


@pytest.mark.parametrize("ip,kind", [
    ("192.168.1.1", "lan"),
    ("10.0.0.1", "lan"),
    ("172.16.0.1", "lan"),
    ("172.15.0.1", "other"),   # just below the RFC 1918 172.x range
    ("172.32.0.1", "other"),   # just above it
    ("100.64.0.1", "tailscale"),
    ("100.63.255.255", "other"),  # just below the CGNAT block
    ("100.128.0.0", "other"),     # just above it
    ("8.8.8.8", "other"),
    ("127.0.0.1", None),
    ("169.254.1.1", None),
    ("224.0.0.1", None),
    ("0.0.0.0", None),
    ("::1", None),
    ("not-an-ip", None),
])
def test_classify_ip(ip, kind):
    assert ip_utils_module.classify_ip(ip) == kind


def test_describe_address_names_the_kind_and_interface():
    describe = ip_utils_module.describe_address
    make = ip_utils_module.PairingAddress
    assert describe(make("192.168.1.42", "Wi-Fi", "lan")) == "192.168.1.42 · Wi-Fi/LAN"
    assert describe(make("100.90.12.34", "tailscale0", "tailscale")) == "100.90.12.34 · Tailscale"
    assert describe(make("203.0.113.7", "eth9", "other")) == "203.0.113.7 · eth9"


def test_no_route_probe_towards_a_public_address_remains():
    # A UDP "route probe" reports whichever interface owns the default route,
    # which under a VPN is the VPN's - the exact failure this design replaced.
    # encoding= is not optional here: the sources are UTF-8 and carry box
    # drawing/middle-dot characters, but read_text() defaults to the locale
    # encoding, which is cp1252 on a Windows CI runner.
    sources = Path(ip_utils_module.__file__).resolve().parent.rglob("*.py")
    offenders = [p.name for p in sources if "8.8.8.8" in p.read_text(encoding="utf-8")]
    assert offenders == []


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

    def schedule_save(self):
        self.saves += 1

    def save_now(self):
        self.saves += 1

    def switch_device(self, previous, new):
        self.switches.append((previous, new))

    def reconnect_stream(self):
        self.reconnects += 1

    def is_streaming(self):
        return self._worker is not None

    def stop_stream(self):
        # Mirrors the real host: a no-op when nothing is streaming.
        if self._worker is not None:
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


def test_pairing_activates_the_address_the_phone_reached_us_from(connection_plugin, config_home):
    # A phone on a tailnet this desktop isn't on reports both its Wi-Fi and
    # its Tailscale address. Rank order alone would pick the Tailscale one -
    # unreachable from here - so the address the pairing POST actually
    # arrived from wins instead, since that path is proven to work.
    plugin, _host, _panel = connection_plugin
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)

    plugin._on_device_paired(
        "Phone", ["192.168.1.50", "100.90.12.34"], "tok-a", source_ip="192.168.1.50",
    )

    assert config_home.load_config()["devices"]["Phone"]["active_ip"] == "192.168.1.50"
    assert plugin._current_device_ip() == "192.168.1.50"
    # The other address stays selectable by hand.
    items = [plugin._ip_combo.itemText(i) for i in range(plugin._ip_combo.count())]
    assert sorted(items) == ["100.90.12.34", "192.168.1.50"]


def test_pairing_activates_the_tailscale_address_when_that_is_the_working_path(
    connection_plugin, config_home,
):
    # Mirror image: both devices on the tailnet with no shared LAN. The
    # phone's Wi-Fi address is listed first and is useless from here.
    plugin, _host, _panel = connection_plugin

    plugin._on_device_paired(
        "Phone", ["192.168.5.20", "100.90.12.34"], "tok-a", source_ip="100.90.12.34",
    )

    assert config_home.load_config()["devices"]["Phone"]["active_ip"] == "100.90.12.34"
    assert plugin._current_device_ip() == "100.90.12.34"


def test_pairing_overrides_a_stale_active_ip_from_an_earlier_pairing(connection_plugin, config_home):
    plugin, _host, _panel = connection_plugin
    plugin._on_device_paired(
        "Phone", ["192.168.1.50", "100.90.12.34"], "tok-a", source_ip="100.90.12.34",
    )
    assert plugin._current_device_ip() == "100.90.12.34"

    # Same phone, paired again from a network where the LAN path works.
    plugin._on_device_paired(
        "Phone", ["192.168.1.50", "100.90.12.34"], "tok-b", source_ip="192.168.1.50",
    )

    assert config_home.load_config()["devices"]["Phone"]["active_ip"] == "192.168.1.50"
    assert plugin._current_device_ip() == "192.168.1.50"


def test_pairing_falls_back_to_ranking_when_the_source_is_not_a_phone_address(
    connection_plugin, config_home,
):
    # USB pairing arrives through the adb reverse tunnel, so the source is
    # the desktop's own loopback - nothing to learn from, keep the old
    # rank-based default rather than pinning a bogus address.
    plugin, _host, _panel = connection_plugin

    plugin._on_device_paired(
        "Phone", ["192.168.1.50", "100.90.12.34"], "tok-a", source_ip="127.0.0.1",
    )

    assert "active_ip" not in config_home.load_config().get("devices", {}).get("Phone", {})
    assert plugin._current_device_ip() == _best_ip(["192.168.1.50", "100.90.12.34"])


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


def test_pair_status_keeps_probing_until_stream_actually_connects(connection_plugin):
    # A worker existing is not proof the phone accepted the token - a stale
    # token must keep showing real status while StreamWorker silently
    # retries, not get pinned "Paired" the instant _start() fires.
    plugin, _host, _panel = connection_plugin
    assert plugin._pair_status_timer.isActive()
    plugin.on_stream_start("http://localhost:8080/v1/video", object())
    assert plugin._pair_status_lbl.text() != "● Paired"
    assert plugin._pair_status_timer.isActive()


def test_pair_status_pins_to_paired_and_stops_polling_once_stream_connects(connection_plugin):
    # Decoded frames settle the pairing question on their own, so the 3s
    # probe has nothing left to establish and retires for the duration.
    plugin, _host, _panel = connection_plugin
    plugin.on_stream_start("http://localhost:8080/v1/video", object())
    plugin._bus.stream_connected.emit()
    assert plugin._pair_status_lbl.text() == "● Paired"
    assert not plugin._pair_status_timer.isActive()


def test_pair_status_check_short_circuits_once_stream_connects(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _arm_synchronous_pair_probe(monkeypatch)
    probed = []
    _stub_ping(monkeypatch, "not_paired", bases=probed)
    plugin._devices = [{"name": "Phone", "ips": ["10.0.0.1"], "token": "tok-a"}]
    plugin._selected_device = "Phone"
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    plugin._on_stream_connected()
    plugin._check_pair_status()
    assert plugin._pair_status_lbl.text() == "● Paired"
    assert probed == []


def test_pair_status_resumes_polling_on_stream_stop(connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin.on_stream_start("http://localhost:8080/v1/video", object())
    plugin._bus.stream_connected.emit()
    assert not plugin._pair_status_timer.isActive()
    plugin.on_stream_stop()
    assert plugin._pair_status_timer.isActive()


def _stub_ping(monkeypatch, status, bases=None, **fields):
    """Stand in for the phone's GET /v1/ping.

    Replaces the old _probe_url stub: the probe now goes through
    PhoneSessionClient, which the remote start/stop share, so stubbing it
    here exercises the same resolution path (device IP vs adb-forwarded
    localhost) that a real check takes.
    """
    def ping(self):
        if bases is not None:
            bases.append(self.base)
        return PingResult(status=status, **fields)

    monkeypatch.setattr(connection_module.PhoneSessionClient, "ping", ping)


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
    _stub_ping(monkeypatch, "paired")
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    plugin._on_device_paired("Phone", ["10.0.0.1"], "tok-a")
    assert plugin._pair_status_lbl.text() == "● Paired"


def test_pair_status_wifi_stale_token(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _arm_synchronous_pair_probe(monkeypatch)
    _stub_ping(monkeypatch, "not_paired")
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    plugin._on_device_paired("Phone", ["10.0.0.1"], "tok-a")
    assert plugin._pair_status_lbl.text() == "○ Not paired"


def test_pair_status_wifi_without_an_ip_skips_probe(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _arm_synchronous_pair_probe(monkeypatch)
    probed = []
    _stub_ping(monkeypatch, "paired", bases=probed)
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
    _stub_ping(monkeypatch, "paired")
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
    _stub_ping(monkeypatch, "paired")
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
    win.save_now()

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


def test_header_widget_hosts_the_device_picker_and_follows_the_mode(connection_plugin):
    plugin, _host, _panel = connection_plugin

    header = plugin.create_header_widget()

    # The picker is moved into the header, not duplicated - the panel's own
    # device rows must not hold a second combo.
    assert header.isAncestorOf(plugin._device_combo)
    assert not plugin._device_row_w.isAncestorOf(plugin._device_combo)

    plugin.set_config({"mode": "wifi", "port": 8080, "devices_list": []})
    assert not header.isHidden()
    assert not plugin._device_row_w.isHidden()

    # USB has no roster or address to pick, so both fold away together.
    plugin.set_config({"mode": "usb", "port": 8080, "devices_list": []})
    assert header.isHidden()
    assert plugin._device_row_w.isHidden()


def test_device_picker_exists_even_if_the_host_never_asks_for_a_header(connection_plugin):
    """A host that only calls create_panel() still gets a working plugin."""
    plugin, _host, _panel = connection_plugin

    plugin.set_config({
        "mode": "wifi", "port": 8080,
        "devices_list": [{"name": "Phone", "ips": ["10.0.0.1"], "token": "t"}],
    })
    plugin.select_device("Phone")

    assert plugin._device_combo.currentText() == "Phone"


# ── Remote start/stop over the session port ───────────────────────────────────

def _wifi_device(plugin, token="tok-a", ip="10.0.0.1"):
    plugin._devices = [{"name": "Phone", "ips": [ip], "token": token}]
    plugin._selected_device = "Phone"
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)
    # The address comes off the combo, not the device dict, so the roster has
    # to actually reach the widgets before anything can resolve a URL.
    plugin._refresh_device_combo(select_name="Phone")


def _stub_session(monkeypatch, pings, start=None, stop=None):
    """Drive PhoneSessionClient with a scripted sequence of ping results.

    ensure_phone_streaming() polls until the phone reports a live stream, so
    a test has to be able to say "not yet, not yet, now" rather than pin a
    single answer.
    """
    calls = []
    remaining = list(pings)

    def ping(self):
        calls.append("ping")
        return remaining.pop(0) if len(remaining) > 1 else remaining[0]

    monkeypatch.setattr(connection_module.PhoneSessionClient, "ping", ping)
    monkeypatch.setattr(
        connection_module.PhoneSessionClient, "start",
        lambda self: calls.append("start") or (start or SessionResult(ok=True)),
    )
    monkeypatch.setattr(
        connection_module.PhoneSessionClient, "stop",
        lambda self: calls.append("stop") or (stop or SessionResult(ok=True)),
    )
    # The real poll sleeps between pings; tests shouldn't.
    monkeypatch.setattr(connection_module.time, "sleep", lambda _s: None)
    return calls


def test_ensure_phone_streaming_is_a_no_op_when_already_streaming(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _wifi_device(plugin)
    calls = _stub_session(monkeypatch, [PingResult("paired", streaming=True, busy=False, local_only=False)])

    assert plugin.ensure_phone_streaming() == (True, "")
    # One ping, and crucially no start: a stream the user set up by hand must
    # not get bounced by the desktop connecting to it.
    assert calls == ["ping"]


def test_ensure_phone_streaming_starts_then_waits_for_the_camera(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _wifi_device(plugin)
    calls = _stub_session(monkeypatch, [
        PingResult("paired", streaming=False, busy=False, local_only=False),   # idle
        PingResult("paired", streaming=False, busy=True, local_only=False),    # opening
        PingResult("paired", streaming=True, busy=False, local_only=False),    # up
    ])

    assert plugin.ensure_phone_streaming() == (True, "")
    assert calls == ["ping", "start", "ping", "ping"]


def test_ensure_phone_streaming_reports_a_refused_start(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _wifi_device(plugin)
    _stub_session(
        monkeypatch,
        [PingResult("paired", streaming=False, busy=False, local_only=False)],
        start=SessionResult(ok=False, error="no_camera_permission"),
    )

    ok, reason = plugin.ensure_phone_streaming()

    assert ok is False
    assert "camera permission" in reason


def test_ensure_phone_streaming_gives_up_when_the_start_falls_back_to_idle(monkeypatch, connection_plugin):
    # A failed start shows up as the service stopping itself. Idle only counts
    # as failure once the start has actually got going - startForegroundService
    # is async, so the first poll can legitimately still read idle.
    plugin, _host, _panel = connection_plugin
    _wifi_device(plugin)
    _stub_session(monkeypatch, [
        PingResult("paired", streaming=False, busy=False, local_only=False),
        PingResult("paired", streaming=False, busy=True, local_only=False),
        PingResult("paired", streaming=False, busy=False, local_only=False),
    ])

    ok, reason = plugin.ensure_phone_streaming()

    assert ok is False
    assert "stopped before it finished starting" in reason


def test_ensure_phone_streaming_times_out_rather_than_hanging(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _wifi_device(plugin)
    _stub_session(monkeypatch, [PingResult("paired", streaming=False, busy=True, local_only=False)])
    clock = iter([0, 0, 1, 999])
    monkeypatch.setattr(connection_module.time, "monotonic", lambda: next(clock, 999))

    ok, reason = plugin.ensure_phone_streaming()

    assert ok is False
    assert "did not finish starting in time" in reason


def test_ensure_phone_streaming_names_a_local_only_mismatch(monkeypatch, connection_plugin):
    # The phone's stream server is bound to 127.0.0.1, so over Wi-Fi there is
    # nothing to connect to - say so instead of timing out on the connect.
    plugin, _host, _panel = connection_plugin
    _wifi_device(plugin)
    calls = _stub_session(
        monkeypatch,
        [PingResult("paired", streaming=False, busy=False, local_only=True)],
    )

    ok, reason = plugin.ensure_phone_streaming()

    assert ok is False
    assert "Local only" in reason
    assert "start" not in calls


def test_ensure_phone_streaming_passes_through_for_an_app_that_predates_the_endpoint(monkeypatch, connection_plugin):
    # An old APK answers ping without a body. It can't be started from here,
    # but it may well already be streaming - fall back to today's behaviour
    # rather than blocking the stream on a version mismatch.
    plugin, _host, _panel = connection_plugin
    _wifi_device(plugin)
    calls = _stub_session(monkeypatch, [PingResult("paired")])

    assert plugin.ensure_phone_streaming() == (True, "")
    assert calls == ["ping"]


def test_ensure_phone_streaming_treats_a_404_start_as_pass_through(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _wifi_device(plugin)
    _stub_session(
        monkeypatch,
        [PingResult("paired", streaming=False, busy=False, local_only=False)],
        start=SessionResult(ok=False, unsupported=True),
    )

    assert plugin.ensure_phone_streaming() == (True, "")


def test_ensure_phone_streaming_explains_an_unreachable_phone(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _wifi_device(plugin)
    _stub_session(monkeypatch, [PingResult("unreachable")])

    ok, reason = plugin.ensure_phone_streaming()

    assert ok is False
    assert "Open the Telescope app on your phone" in reason


def test_ensure_phone_streaming_explains_a_stale_token(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _wifi_device(plugin)
    _stub_session(monkeypatch, [PingResult("not_paired")])

    ok, reason = plugin.ensure_phone_streaming()

    assert ok is False
    assert "Pair the device again" in reason


def test_ensure_phone_streaming_without_a_paired_device_says_so(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin._devices = []
    plugin._selected_device = None
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)

    ok, reason = plugin.ensure_phone_streaming()

    assert ok is False
    assert "isn't paired yet" in reason


def test_stop_phone_streaming_posts_stop(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    _wifi_device(plugin)
    calls = _stub_session(monkeypatch, [PingResult("paired", streaming=True, busy=False, local_only=False)])

    plugin.stop_phone_streaming()

    assert calls == ["stop"]


def test_stop_phone_streaming_is_silent_without_a_reachable_device(connection_plugin):
    plugin, _host, _panel = connection_plugin
    plugin._devices = []
    plugin._selected_device = None
    plugin._rb_wifi.setChecked(True)
    plugin._rb_usb.setChecked(False)

    plugin.stop_phone_streaming()  # must not raise


def test_session_channel_over_usb_forwards_and_tears_down(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["serial-1"])
    calls = []
    monkeypatch.setattr(
        connection_module, "adb_forward",
        lambda port, serial: calls.append(("forward", port, serial)) or (True, "ok"),
    )
    monkeypatch.setattr(
        connection_module, "adb_unforward",
        lambda port, serial: calls.append(("unforward", port, serial)),
    )
    plugin._devices = [{"name": USB_PROFILE_KEY, "ips": [], "token": "tok-a"}]
    plugin._selected_device = USB_PROFILE_KEY
    plugin._rb_usb.setChecked(True)
    plugin._rb_wifi.setChecked(False)

    with plugin.session_channel(token="tok-a", usb=True) as (client, _unavailable):
        assert client.base == f"http://localhost:{connection_module.PING_PORT}"
        assert calls == [("forward", connection_module.PING_PORT, "serial-1")]

    # The forward is dedicated to this call and must not outlive it.
    assert calls[-1] == ("unforward", connection_module.PING_PORT, "serial-1")


def test_session_channel_over_usb_unforwards_even_when_the_body_raises(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["serial-1"])
    calls = []
    monkeypatch.setattr(connection_module, "adb_forward", lambda port, serial: (True, "ok"))
    monkeypatch.setattr(
        connection_module, "adb_unforward",
        lambda port, serial: calls.append(("unforward", port, serial)),
    )

    with pytest.raises(RuntimeError):
        with plugin.session_channel(token="tok-a", usb=True):
            raise RuntimeError("boom")

    assert calls == [("unforward", connection_module.PING_PORT, "serial-1")]


def test_session_channel_reports_an_ambiguous_usb_device(monkeypatch, connection_plugin):
    plugin, _host, _panel = connection_plugin
    monkeypatch.setattr(connection_module, "adb_devices", lambda: ["a", "b"])
    forwards = []
    monkeypatch.setattr(
        connection_module, "adb_forward",
        lambda *a, **k: forwards.append(a) or (True, "ok"),
    )

    with plugin.session_channel(token="tok-a", usb=True) as (client, unavailable):
        assert client is None
        assert unavailable == "unknown"
    assert forwards == []
