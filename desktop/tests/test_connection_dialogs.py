"""Add phone dialog: QR over Wi-Fi, or a picked phone over USB."""

import http.client
import json
import time
from types import SimpleNamespace

import pytest

import telescope.pairing as pairing_module
import telescope.plugins.connection as connection_module
from telescope.ip_utils import PairingAddress
from telescope.pairing import PairingResult, pairing_proof
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
    state = {"devices": [], "available": True, "models": {}}
    monkeypatch.setattr(connection_module, "adb_available", lambda: state["available"])
    monkeypatch.setattr(connection_module, "adb_device_models", lambda: list(state["devices"]))
    monkeypatch.setattr(connection_module, "adb_model", lambda serial: state["models"].get(serial, ""))
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


def _post_pairing(offer, phone_id):
    """What a phone that scanned the code sends; the status it gets back."""
    cert = "ab" * 32
    body = json.dumps({"name": "Phone", "ips": [], "phone_id": phone_id, "cert_sha256": cert,
                       "proof": pairing_proof(offer.token, offer.nonce, phone_id, cert)}).encode()
    conn = http.client.HTTPConnection("127.0.0.1", offer.port, timeout=2)
    conn.request("POST", f"/pair/{offer.nonce}", body=body, headers={"Content-Length": str(len(body))})
    status = conn.getresponse().status
    conn.close()
    return status


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


def test_without_adb_usb_says_so(qapp, adb, lan, monkeypatch):
    monkeypatch.setattr(connection_module, "IS_WINDOWS", False)  # Windows offers to get it instead (below)
    adb["available"] = False
    dialog = _open(qapp)
    try:
        assert "isn't installed" in dialog._usb_lbl.text()
        assert not dialog._usb_timer.isActive()
    finally:
        dialog.reject()


def test_an_unauthorized_phone_asks_for_the_debugging_prompt(qapp, adb, lan):
    adb["devices"] = [("serial-1", "unauthorized", "")]
    dialog = _open(qapp)
    try:
        dialog._poll_usb()
        assert "Allow USB debugging" in dialog._usb_lbl.text()
        assert not any(c[0] == "broadcast" for c in adb["calls"])
    finally:
        dialog.reject()


def _picks(dialog):
    return [dialog._usb_picks.itemAt(i).widget() for i in range(dialog._usb_picks.count())]


def _broadcasts(adb):
    return [c[1] for c in adb["calls"] if c[0] == "broadcast"]


def test_a_plugged_in_phone_waits_to_be_picked(qapp, adb, lan):
    # Left plugged in, it would otherwise pair before the phone being scanned
    adb["devices"] = [("serial-1", "device", "Pixel 7")]
    dialog = _open(qapp)
    try:
        assert "Pick a phone below" in dialog._usb_lbl.text()
        assert [b.text() for b in _picks(dialog)] == ["Pair Pixel 7"]
        assert _broadcasts(adb) == []
    finally:
        dialog.reject()


def test_a_picked_phone_gets_the_pairing_offer_over_usb(qapp, adb, lan, monkeypatch):
    adb["devices"] = [("serial-1", "device", "Pixel 7")]
    clock = [100.0]
    monkeypatch.setattr(connection_module.time, "monotonic", lambda: clock[0])
    dialog = _open(qapp)
    try:
        port = dialog._server.offer.port
        _picks(dialog)[0].click()
        assert ("reverse", port, "serial-1") in adb["calls"]
        assert _broadcasts(adb) == ["serial-1"]
        assert "Pairing Pixel 7" in dialog._usb_lbl.text()
        assert not _picks(dialog)[0].isEnabled()

        dialog._poll_usb()  # too soon to re-send
        assert _broadcasts(adb) == ["serial-1"]
        clock[0] += 5
        dialog._poll_usb()  # re-sent, in case the app wasn't on screen the first time
        assert _broadcasts(adb) == ["serial-1", "serial-1"]
        assert adb["calls"].count(("reverse", port, "serial-1")) == 1
    finally:
        dialog.reject()
    assert ("unreverse", port, "serial-1") in adb["calls"]


def test_with_several_phones_plugged_in_only_the_picked_one_pairs(qapp, adb, lan):
    adb["devices"] = [("serial-1", "device", "Pixel 7"), ("serial-2", "device", ""),
                      ("serial-3", "unauthorized", "")]
    dialog = _open(qapp)
    try:
        assert [b.text() for b in _picks(dialog)] == ["Pair Pixel 7", "Pair serial-2"]
        _picks(dialog)[1].click()
        dialog._poll_usb()
        assert _broadcasts(adb) == ["serial-2"]
    finally:
        dialog.reject()


def test_plugging_in_after_it_opened_grows_the_dialog_to_fit(qapp, adb, lan):
    # Buttons added to a shown dialog only count once they show; the dialog has to fit them straight away
    dialog = _open(qapp)
    try:
        lay = dialog.layout()
        adb["devices"] = [("serial-1", "device", "")]
        adb["models"] = {"serial-1": "Pixel 7"}
        dialog._poll_usb()
        height = dialog.height()
        qapp.processEvents()
        assert dialog.height() == height  # not squeezed into a size picked before the button showed
        qr = dialog._qr_container.itemAt(0).widget()
        assert dialog._wifi_row.geometry().top() - qr.geometry().bottom() - 1 == lay.spacing()
        assert dialog._usb_lbl.height() == dialog._usb_lbl.minimumHeight()
    finally:
        dialog.reject()


def test_lots_of_phones_scroll_instead_of_growing_the_dialog(qapp, adb, lan):
    adb["devices"] = [(f"serial-{i}", "device", f"Phone {i}") for i in range(20)]
    dialog = _open(qapp)
    try:
        qapp.processEvents()
        assert len(_picks(dialog)) == 20
        assert dialog._picks_area.verticalScrollBar().maximum() > 0
        twenty = dialog._picks_area.maximumHeight()
        adb["devices"] = adb["devices"][:4]
        dialog._poll_usb()
        assert dialog._picks_area.maximumHeight() == twenty  # four rows show either way
        adb["devices"] = adb["devices"][:1]
        dialog._poll_usb()
        assert dialog._picks_area.maximumHeight() < twenty
    finally:
        dialog.reject()


def test_buttons_name_the_phone_as_it_names_itself(qapp, adb, lan):
    # adb devices -l turns SM-G975F into SM_G975F
    adb["devices"] = [("serial-1", "device", "SM G975F"), ("serial-2", "device", "Pixel 7")]
    adb["models"] = {"serial-1": "SM-G975F"}
    dialog = _open(qapp)
    try:
        assert [b.text() for b in _picks(dialog)] == ["Pair SM-G975F", "Pair Pixel 7"]
    finally:
        dialog.reject()


def test_unplugging_the_picked_phone_forgets_the_pick(qapp, adb, lan):
    adb["devices"] = [("serial-1", "device", "Pixel 7")]
    dialog = _open(qapp)
    try:
        _picks(dialog)[0].click()
        adb["devices"] = []
        dialog._poll_usb()
        assert _picks(dialog) == []
        adb["devices"] = [("serial-1", "device", "Pixel 7")]
        dialog._poll_usb()
        assert _picks(dialog)[0].isEnabled()
        assert _broadcasts(adb) == ["serial-1"]
    finally:
        dialog.reject()


def test_a_scanned_phone_pairs_while_another_is_plugged_in(qapp, adb, lan):
    adb["devices"] = [("serial-1", "device", "Pixel 7")]
    paired = []
    dialog = _open(qapp, paired)
    try:
        offer = dialog._server.offer
        status = _post_pairing(offer, "phone-wifi")
        deadline = time.monotonic() + 2
        while not paired and time.monotonic() < deadline:
            qapp.processEvents()
        assert status == 200
        assert [r.phone_id for r in paired] == ["phone-wifi"]
        assert _broadcasts(adb) == []
        assert _picks(dialog) == []
    finally:
        dialog.reject()


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
        assert "plug the phone in" in dialog._usb_lbl.text()
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


def test_an_adb_download_that_ends_after_closing_doesn_t_poll_usb(qapp, adb, lan, monkeypatch):
    adb["available"] = False
    monkeypatch.setattr(connection_module, "IS_WINDOWS", True)
    dialog = _open(qapp)
    dialog._adb_download.running = True
    dialog.reject()  # closed while it downloads
    adb["available"] = True
    dialog._adb_download.finished.emit(True, "37.0.1")
    assert not dialog._usb_timer.isActive()


def test_reopened_mid_download_it_says_so_instead_of_offering_it_again(qapp, adb, lan, monkeypatch):
    adb["available"] = False
    monkeypatch.setattr(connection_module, "IS_WINDOWS", True)
    dialog = _open(qapp)
    try:
        dialog._adb_download.running = True
        dialog._start_usb()
        assert "Getting adb" in dialog._usb_lbl.text() and "href" not in dialog._usb_lbl.text()
    finally:
        dialog._adb_download.running = False
        dialog.reject()
