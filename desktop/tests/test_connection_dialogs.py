"""Add phone dialog: QR over Wi-Fi, automatic pairing over USB."""

from types import SimpleNamespace

import pytest

import telescope.pairing as pairing_module
import telescope.plugins.connection as connection_module
from telescope.ip_utils import PairingAddress
from telescope.pairing import PairingResult
from telescope.plugins.connection import AddPhoneDialog
from telescope.widgets.qr import QRCodeWidget


class _SyncThread:
    """Runs a would-be background thread inline so the test sees its effects."""

    def __init__(self, target, args=(), kwargs=None, daemon=None):
        self._run = lambda: target(*args, **(kwargs or {}))

    def start(self):
        self._run()


@pytest.fixture
def adb(monkeypatch):
    calls = []
    state = {"devices": [], "available": True}
    monkeypatch.setattr(connection_module, "adb_available", lambda: state["available"])
    monkeypatch.setattr(connection_module, "adb_device_states", lambda: list(state["devices"]))
    monkeypatch.setattr(connection_module, "adb_reverse",
                        lambda port, serial=None: calls.append(("reverse", port, serial)) or (True, ""))
    monkeypatch.setattr(connection_module, "adb_unreverse",
                        lambda port, serial=None: calls.append(("unreverse", port, serial)))
    monkeypatch.setattr(connection_module, "adb_broadcast_pair",
                        lambda payload, serial=None: calls.append(("broadcast", serial)) or (True, ""))
    # Only the plugin module's view of threading: the pairing server needs its real thread.
    monkeypatch.setattr(connection_module, "threading", SimpleNamespace(Thread=_SyncThread))
    state["calls"] = calls
    return state


@pytest.fixture
def lan(monkeypatch):
    addresses = [PairingAddress("192.168.1.2", "wlan0", "lan")]
    monkeypatch.setattr(pairing_module.ip_utils, "get_pairing_addresses", lambda: list(addresses))
    return addresses


def _open(qapp, paired=None):
    dialog = AddPhoneDialog(None, "computer-id", "Desk", (paired if paired is not None else []).append)
    dialog.show()
    qapp.processEvents()
    return dialog


def _qr_shown(dialog):
    return any(isinstance(dialog._qr_container.itemAt(i).widget(), QRCodeWidget)
               for i in range(dialog._qr_container.count()))


def test_qr_widget_builds_matrix_and_renders(qapp):
    widget = QRCodeWidget("hello")
    assert widget.width() == widget.height() > 0
    widget.grab()


def test_shows_a_qr_code_when_the_computer_is_on_a_network(qapp, adb, lan):
    dialog = _open(qapp)
    try:
        assert _qr_shown(dialog)
        assert "Scan pairing code" in dialog._wifi_lbl.text()
        assert "computer-id" in dialog._server.offer.payload
    finally:
        dialog.reject()


def test_without_a_network_it_steers_to_usb(qapp, adb, lan):
    lan.clear()
    dialog = _open(qapp)
    try:
        assert not _qr_shown(dialog)
        assert "pair over USB" in dialog._wifi_lbl.text()
    finally:
        dialog.reject()


def test_without_adb_usb_says_so(qapp, adb, lan):
    adb["available"] = False
    dialog = _open(qapp)
    try:
        assert "isn't installed" in dialog._usb_lbl.text()
        assert not dialog._usb_timer.isActive()
    finally:
        dialog.reject()


def test_an_unauthorized_phone_asks_for_the_debugging_prompt(qapp, adb, lan):
    adb["devices"] = [("serial-1", "unauthorized")]
    dialog = _open(qapp)
    try:
        dialog._poll_usb()
        assert "Allow USB debugging" in dialog._usb_lbl.text()
        assert not any(c[0] == "broadcast" for c in adb["calls"])
    finally:
        dialog.reject()


def test_a_plugged_in_phone_gets_the_pairing_offer_over_usb(qapp, adb, lan, monkeypatch):
    adb["devices"] = [("serial-1", "device")]
    clock = [100.0]
    monkeypatch.setattr(connection_module.time, "monotonic", lambda: clock[0])
    dialog = _open(qapp)
    try:
        port = dialog._server.offer.port
        assert ("reverse", port, "serial-1") in adb["calls"]
        assert adb["calls"].count(("broadcast", "serial-1")) == 1

        dialog._poll_usb()  # too soon to re-send
        assert adb["calls"].count(("broadcast", "serial-1")) == 1
        clock[0] += 5
        dialog._poll_usb()  # re-sent, in case the app wasn't on screen the first time
        assert adb["calls"].count(("broadcast", "serial-1")) == 2
        assert adb["calls"].count(("reverse", port, "serial-1")) == 1
    finally:
        dialog.reject()
    assert ("unreverse", port, "serial-1") in adb["calls"]


def test_success_shows_the_phone_and_hands_the_result_over(qapp, adb, lan):
    paired = []
    dialog = _open(qapp, paired)
    result = PairingResult(name="Pixel", ips=["192.168.1.9"], token="tok", source_ip="192.168.1.9",
                           phone_id="phone-1")
    dialog._on_paired_signal(result)
    assert paired == [result]
    assert dialog._result_lbl.text() == "Paired with Pixel"
    assert dialog._close_btn.text() == "Done"
    assert dialog._server is None
    assert not _qr_shown(dialog)

    dialog._on_paired_signal(result)  # a late duplicate is ignored
    assert paired == [result]
    dialog.accept()


def test_closing_stops_the_pairing_server(qapp, adb, lan):
    dialog = _open(qapp)
    server = dialog._server
    dialog.reject()
    assert dialog._server is None
    assert server.offer is None


def test_on_windows_without_adb_it_offers_to_get_it_and_then_pairs_over_usb(qapp, adb, lan, monkeypatch):
    import telescope.widgets.adb as adb_widget
    adb["available"] = False
    asked = []
    monkeypatch.setattr(connection_module, "IS_WINDOWS", True)
    monkeypatch.setattr(adb_widget, "confirm", lambda parent: asked.append(parent) or True)
    monkeypatch.setattr(adb_widget, "threading", SimpleNamespace(Thread=_SyncThread))

    def download(progress):
        progress("Downloading adb 37.0.1 from Google...")
        adb["available"] = True
        return True, "37.0.1"
    monkeypatch.setattr(adb_widget.adb_download, "download_adb", download)
    dialog = _open(qapp)
    try:
        assert 'href="get-adb"' in dialog._usb_lbl.text()
        dialog._usb_lbl.linkActivated.emit("get-adb")
        assert asked == [dialog]
        assert "pairs by itself" in dialog._usb_lbl.text()
        assert dialog._usb_timer.isActive()
    finally:
        dialog.reject()


def test_a_failed_adb_download_says_why_and_offers_another_go(qapp, adb, lan, monkeypatch):
    import telescope.widgets.adb as adb_widget
    adb["available"] = False
    monkeypatch.setattr(connection_module, "IS_WINDOWS", True)
    monkeypatch.setattr(adb_widget, "confirm", lambda parent: True)
    monkeypatch.setattr(adb_widget, "threading", SimpleNamespace(Thread=_SyncThread))
    monkeypatch.setattr(adb_widget.adb_download, "download_adb",
                        lambda progress: (False, "Couldn't download adb: <urlopen error timed out>"))
    dialog = _open(qapp)
    try:
        dialog._usb_lbl.linkActivated.emit("get-adb")
        assert "&lt;urlopen error timed out&gt;" in dialog._usb_lbl.text()
        assert "Try again" in dialog._usb_lbl.text()
        assert not dialog._usb_timer.isActive()
    finally:
        dialog.reject()
