import http.client
import json
import socket
import time

import pytest

from telescope.ip_utils import PairingAddress
from telescope.pairing import PAIRING_PROTOCOL_VERSION, PairingResult, PairingServer, pairing_proof

_CANDIDATES = [
    PairingAddress(ip="192.168.1.42", interface="Wi-Fi", kind="lan"),
    PairingAddress(ip="100.90.12.34", interface="tailscale0", kind="tailscale"),
]


@pytest.fixture
def pairing_server(monkeypatch):
    import telescope.pairing as pairing_module

    monkeypatch.setattr(
        pairing_module.ip_utils, "get_pairing_addresses", lambda: list(_CANDIDATES),
    )
    paired = []
    server = PairingServer(on_paired=paired.append)
    offer = server.start()
    assert offer is not None
    yield server, offer, paired
    server.stop()
    time.sleep(0.2)


def _post(port, path, body: bytes, headers=None):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
    headers = headers if headers is not None else {"Content-Length": str(len(body))}
    conn.request("POST", path, body=body, headers=headers)
    r = conn.getresponse()
    status = r.status
    r.read()
    conn.close()
    return status


def test_start_without_network_still_offers_usb_pairing(monkeypatch):
    import telescope.pairing as pairing_module

    monkeypatch.setattr(pairing_module.ip_utils, "get_pairing_addresses", lambda: [])
    server = PairingServer(on_paired=lambda r: None)
    offer = server.start()
    try:
        # No LAN address means no QR code, but a USB-only machine can still pair through adb reverse.
        assert offer.candidates == []
        assert json.loads(offer.usb_payload)["candidates"] == [
            {"ip": "127.0.0.1", "interface": "USB (adb)", "kind": "other"},
        ]
    finally:
        server.stop()


def test_start_is_idempotent(pairing_server):
    server, offer, _paired = pairing_server
    assert server.start() is offer


def test_payload_is_version_4_with_every_candidate(pairing_server):
    _server, offer, _paired = pairing_server
    payload = json.loads(offer.payload)

    assert payload["version"] == PAIRING_PROTOCOL_VERSION == 4
    assert "computer_id" in payload and "computer_name" in payload
    assert payload["port"] == offer.port
    assert payload["nonce"] == offer.nonce
    assert payload["token"] == offer.token
    # LAN before Tailscale, with interface and kind for phone routing decision.
    assert payload["candidates"] == [
        {"ip": "192.168.1.42", "interface": "Wi-Fi", "kind": "lan"},
        {"ip": "100.90.12.34", "interface": "tailscale0", "kind": "tailscale"},
    ]
    assert offer.candidates == _CANDIDATES


def test_start_with_advertised_addresses_skips_discovery(monkeypatch):
    import telescope.pairing as pairing_module

    monkeypatch.setattr(
        pairing_module.ip_utils, "get_pairing_addresses",
        lambda: (_ for _ in ()).throw(AssertionError("should not enumerate interfaces")),
    )
    server = PairingServer(on_paired=lambda r: None)
    try:
        offer = server.start(
            advertise=[PairingAddress(ip="127.0.0.1", interface="USB (adb)", kind="other")]
        )
        assert offer is not None
        assert json.loads(offer.payload)["candidates"] == [
            {"ip": "127.0.0.1", "interface": "USB (adb)", "kind": "other"},
        ]
    finally:
        server.stop()
        time.sleep(0.2)


PIN = "ab" * 32


def _pair_body(offer, name="Phone", ips=(), phone_id="ph-1", cert=PIN, proof=None, **extra) -> bytes:
    data = {"name": name, "ips": list(ips), "phone_id": phone_id, "cert_sha256": cert,
            "proof": proof if proof is not None else pairing_proof(offer.token, offer.nonce, phone_id, cert)}
    data.update(extra)
    return json.dumps(data).encode()


def _wait_for(paired):
    for _ in range(20):
        time.sleep(0.05)
        if paired:
            break


def test_empty_ips_in_payload_is_accepted(pairing_server):
    # USB-only phone with no Wi-Fi reports empty IPs; only malformed entries rejected.
    server, offer, paired = pairing_server
    assert _post(offer.port, f"/pair/{offer.nonce}", _pair_body(offer)) == 200
    _wait_for(paired)
    assert paired == [
        PairingResult(name="Phone", ips=[], token=offer.token, source_ip="127.0.0.1", phone_id="ph-1", cert_sha256=PIN),
    ]


def test_wrong_nonce_is_rejected(pairing_server):
    server, offer, _paired = pairing_server
    assert _post(offer.port, "/pair/not-the-nonce", b"{}") == 404


def test_oversized_body_is_rejected(pairing_server):
    server, offer, _paired = pairing_server
    body = b"x" * (17 * 1024)
    assert _post(offer.port, f"/pair/{offer.nonce}", body) == 413


def test_missing_content_length_is_rejected(pairing_server):
    server, offer, _paired = pairing_server
    s = socket.create_connection(("127.0.0.1", offer.port), timeout=2)
    s.sendall(f"POST /pair/{offer.nonce} HTTP/1.1\r\nContent-Length: notanumber\r\n\r\n".encode())
    status = int(s.recv(200).split(b" ")[1])
    s.close()
    assert status == 411


def test_invalid_ip_in_payload_is_rejected(pairing_server):
    server, offer, _paired = pairing_server
    assert _post(offer.port, f"/pair/{offer.nonce}", _pair_body(offer, ips=["not-an-ip"])) == 400


def test_a_proof_made_with_another_token_is_rejected(pairing_server):
    server, offer, paired = pairing_server
    forged = pairing_proof("wrong-token", offer.nonce, "ph-1", PIN)
    assert _post(offer.port, f"/pair/{offer.nonce}", _pair_body(offer, proof=forged)) == 400
    assert paired == []


def test_the_proof_covers_the_fingerprint(pairing_server):
    # Someone who saw a real phone's POST can't swap in their own certificate.
    server, offer, paired = pairing_server
    real = pairing_proof(offer.token, offer.nonce, "ph-1", PIN)
    assert _post(offer.port, f"/pair/{offer.nonce}", _pair_body(offer, cert="cd" * 32, proof=real)) == 400
    assert paired == []


def test_a_phone_from_before_tls_is_rejected(pairing_server):
    # A v3 phone echoes the token and sends no fingerprint.
    server, offer, paired = pairing_server
    body = json.dumps({"name": "Phone", "ips": [], "phone_id": "ph-1", "token": offer.token}).encode()
    assert _post(offer.port, f"/pair/{offer.nonce}", body) == 400
    assert paired == []


def test_a_malformed_fingerprint_is_rejected(pairing_server):
    server, offer, paired = pairing_server
    assert _post(offer.port, f"/pair/{offer.nonce}", _pair_body(offer, cert="not-hex")) == 400
    assert paired == []


def test_valid_payload_pairs_and_invokes_callback(pairing_server):
    server, offer, paired = pairing_server
    assert _post(offer.port, f"/pair/{offer.nonce}", _pair_body(offer, name="MyPhone", ips=["192.168.1.55"])) == 200
    _wait_for(paired)
    assert paired == [
        PairingResult(
            name="MyPhone", ips=["192.168.1.55"], token=offer.token,
            # Source IP of actual POST; desktop streams back to this instead of guessing.
            source_ip="127.0.0.1", phone_id="ph-1", cert_sha256=PIN,
        ),
    ]


def test_a_stalled_connection_does_not_block_the_phone_pairing(pairing_server):
    # A phone that dropped off Wi-Fi mid-request (or a port scanner) used to hold the only handler thread.
    _server, offer, paired = pairing_server
    stalled = socket.create_connection(("127.0.0.1", offer.port), timeout=2)
    try:
        assert _post(offer.port, f"/pair/{offer.nonce}", _pair_body(offer)) == 200
        _wait_for(paired)
        assert len(paired) == 1
    finally:
        stalled.close()


def test_a_second_phone_is_refused_once_one_paired(pairing_server):
    # Both got the offer (two phones plugged in); only the first is kept, so the second mustn't think it paired.
    _server, offer, paired = pairing_server
    assert _post(offer.port, f"/pair/{offer.nonce}", _pair_body(offer, phone_id="ph-1")) == 200
    assert _post(offer.port, f"/pair/{offer.nonce}", _pair_body(offer, phone_id="ph-2")) == 409
    # The first phone retrying (its answer got lost) still gets through.
    assert _post(offer.port, f"/pair/{offer.nonce}", _pair_body(offer, phone_id="ph-1")) == 200
    _wait_for(paired)
    assert {r.phone_id for r in paired} == {"ph-1"}


def test_pairing_without_a_phone_id_is_rejected(pairing_server):
    # Without it the desktop couldn't tell phones apart.
    _server, offer, paired = pairing_server
    assert _post(offer.port, f"/pair/{offer.nonce}", _pair_body(offer, phone_id="")) == 400
    assert paired == []


def test_the_proof_matches_the_phone_app():
    # Same vector as the phone's PairingTest.kt.
    assert pairing_proof("tok-123", "nonce-abc", "phone-1", "ab" * 32) == \
        "fabf87423641e2c832e78a0385ce388ccadc57e896fe2b028586b22c2873d915"


def test_offer_names_this_computer():
    server = PairingServer(on_paired=lambda r: None, computer_id="pc-9", computer_name="Desk")
    offer = server.start(advertise=_CANDIDATES)
    try:
        for raw in (offer.payload, offer.usb_payload):
            data = json.loads(raw)
            assert (data["computer_id"], data["computer_name"]) == ("pc-9", "Desk")
            assert data["nonce"] == offer.nonce and data["token"] == offer.token
    finally:
        server.stop()


def test_stop_is_idempotent(pairing_server):
    server, _offer, _paired = pairing_server
    server.stop()
    server.stop()  # must not raise


def test_pairing_again_straight_away_keeps_the_fixed_port(monkeypatch):
    import telescope.pairing as pairing_module
    monkeypatch.setattr(pairing_module, "PAIRING_PORT", 38765)
    first = PairingServer(on_paired=lambda _r: None)
    port = first.start(advertise=[]).port
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
    conn.request("GET", "/")
    conn.getresponse().read()
    conn.close()
    first.stop()  # the server closed its side first, so the port sits in TIME_WAIT
    time.sleep(1)  # stop() lets go of the port in the background

    second = PairingServer(on_paired=lambda _r: None)
    try:
        assert second.start(advertise=[]).port == 38765
    finally:
        second.stop()
