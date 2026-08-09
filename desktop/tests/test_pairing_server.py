import http.client
import json
import socket
import time

import pytest

from telescope.ip_utils import PairingAddress
from telescope.pairing import PAIRING_PROTOCOL_VERSION, PairingResult, PairingServer

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


def test_start_returns_none_without_network_interfaces(monkeypatch):
    import telescope.pairing as pairing_module

    monkeypatch.setattr(pairing_module.ip_utils, "get_pairing_addresses", lambda: [])
    server = PairingServer(on_paired=lambda r: None)
    assert server.start() is None


def test_start_is_idempotent(pairing_server):
    server, offer, _paired = pairing_server
    assert server.start() is offer


def test_payload_is_version_2_with_every_candidate(pairing_server):
    _server, offer, _paired = pairing_server
    payload = json.loads(offer.payload)

    assert payload["version"] == PAIRING_PROTOCOL_VERSION == 2
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


def test_empty_ips_in_payload_is_accepted(pairing_server):
    # USB-only phone with no Wi-Fi reports empty IPs; only malformed entries rejected.
    server, offer, paired = pairing_server
    body = json.dumps({"name": "Phone", "ips": [], "token": offer.token}).encode()
    assert _post(offer.port, f"/pair/{offer.nonce}", body) == 200
    for _ in range(20):
        time.sleep(0.05)
        if paired:
            break
    assert paired == [
        PairingResult(name="Phone", ips=[], token=offer.token, source_ip="127.0.0.1"),
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
    body = json.dumps({"name": "Phone", "ips": ["not-an-ip"], "token": offer.token}).encode()
    assert _post(offer.port, f"/pair/{offer.nonce}", body) == 400


def test_wrong_echoed_token_is_rejected(pairing_server):
    server, offer, paired = pairing_server
    body = json.dumps({"name": "Phone", "ips": ["192.168.1.55"], "token": "wrong-token"}).encode()
    assert _post(offer.port, f"/pair/{offer.nonce}", body) == 400
    assert paired == []


def test_missing_token_is_rejected(pairing_server):
    server, offer, paired = pairing_server
    body = json.dumps({"name": "Phone", "ips": ["192.168.1.55"]}).encode()
    assert _post(offer.port, f"/pair/{offer.nonce}", body) == 400
    assert paired == []


def test_valid_payload_pairs_and_invokes_callback(pairing_server):
    server, offer, paired = pairing_server
    body = json.dumps({"name": "MyPhone", "ips": ["192.168.1.55"], "token": offer.token}).encode()

    assert _post(offer.port, f"/pair/{offer.nonce}", body) == 200

    for _ in range(20):
        time.sleep(0.05)
        if paired:
            break
    assert paired == [
        PairingResult(
            name="MyPhone", ips=["192.168.1.55"], token=offer.token,
            # Source IP of actual POST; desktop streams back to this instead of guessing.
            source_ip="127.0.0.1",
        ),
    ]


def test_stop_is_idempotent(pairing_server):
    server, _offer, _paired = pairing_server
    server.stop()
    server.stop()  # must not raise
