import http.client
import json
import socket
import time

import pytest

from telescope.plugins.connection import _PairingDialog


@pytest.fixture
def pairing_dialog(qapp):
    paired = []
    dlg = _PairingDialog(None, lambda name, ips, token: paired.append((name, ips, token)))
    dlg._start_server()
    time.sleep(0.2)
    yield dlg, paired
    dlg._stop_server()
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


def test_wrong_nonce_is_rejected(pairing_dialog):
    dlg, _ = pairing_dialog
    port = dlg._server.server_address[1]
    assert _post(port, "/pair/not-the-nonce", b"{}") == 404


def test_oversized_body_is_rejected(pairing_dialog):
    dlg, _ = pairing_dialog
    port = dlg._server.server_address[1]
    nonce = dlg._nonce
    body = b"x" * (17 * 1024)
    assert _post(port, f"/pair/{nonce}", body) == 413


def test_missing_content_length_is_rejected(pairing_dialog):
    dlg, _ = pairing_dialog
    port = dlg._server.server_address[1]
    nonce = dlg._nonce
    s = socket.create_connection(("127.0.0.1", port), timeout=2)
    s.sendall(f"POST /pair/{nonce} HTTP/1.1\r\nContent-Length: notanumber\r\n\r\n".encode())
    status = int(s.recv(200).split(b" ")[1])
    s.close()
    assert status == 411


def test_invalid_ip_in_payload_is_rejected(pairing_dialog):
    dlg, _ = pairing_dialog
    port = dlg._server.server_address[1]
    nonce = dlg._nonce
    token = dlg._token
    body = json.dumps({"name": "Phone", "ips": ["not-an-ip"], "token": token}).encode()
    assert _post(port, f"/pair/{nonce}", body) == 400


def test_wrong_echoed_token_is_rejected(pairing_dialog):
    dlg, paired = pairing_dialog
    port = dlg._server.server_address[1]
    nonce = dlg._nonce
    body = json.dumps({"name": "Phone", "ips": ["192.168.1.55"], "token": "wrong-token"}).encode()
    assert _post(port, f"/pair/{nonce}", body) == 400
    assert paired == []


def test_missing_token_is_rejected(pairing_dialog):
    dlg, paired = pairing_dialog
    port = dlg._server.server_address[1]
    nonce = dlg._nonce
    body = json.dumps({"name": "Phone", "ips": ["192.168.1.55"]}).encode()
    assert _post(port, f"/pair/{nonce}", body) == 400
    assert paired == []


def test_valid_payload_pairs_and_emits_callback(pairing_dialog, qapp):
    dlg, paired = pairing_dialog
    port = dlg._server.server_address[1]
    nonce = dlg._nonce
    token = dlg._token
    body = json.dumps({"name": "MyPhone", "ips": ["192.168.1.55"], "token": token}).encode()

    assert _post(port, f"/pair/{nonce}", body) == 200

    for _ in range(20):
        qapp.processEvents()
        time.sleep(0.05)
        if paired:
            break
    assert paired == [("MyPhone", ["192.168.1.55"], token)]


def test_stop_server_is_idempotent(pairing_dialog):
    dlg, _ = pairing_dialog
    dlg._stop_server()
    dlg._stop_server()  # must not raise
