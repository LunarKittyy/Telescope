import json
import urllib.error
import urllib.request

import pytest

import telescope.session_client as session_client_module
from telescope.pinned_https import PhoneAuth
from telescope.session_client import (
    HELLO_MISSING, HELLO_NONE, HELLO_OK, Hello, PhoneSessionClient, PingResult, SessionResult,
)


class _Response:
    def __init__(self, status=200, body=b""):
        self.status = status
        self._body = body

    def read(self, n=-1):
        return self._body if n < 0 else self._body[:n]

    def __enter__(self):
        return self

    def __exit__(self, *_exc):
        return False


def _stub_urlopen(monkeypatch, handler):
    """Mock urlopen with handler; record requests for assertions."""
    seen = []

    def urlopen(req, timeout=None):
        seen.append(req)
        result = handler(req)
        if isinstance(result, Exception):
            raise result
        return result

    monkeypatch.setattr(session_client_module.urllib.request, "urlopen", urlopen)
    return seen


def _http_error(code):
    return urllib.error.HTTPError("http://phone:8766/x", code, "err", {}, None)


@pytest.fixture
def client():
    return PhoneSessionClient("http://phone:8766", PhoneAuth("tok"))


# ── ping ──────────────────────────────────────────────────────────────────────

def test_ping_parses_the_phone_state_body(monkeypatch, client):
    body = json.dumps(
        {"protocol": 1, "streaming": True, "busy": False, "localOnly": True}
    ).encode()
    seen = _stub_urlopen(monkeypatch, lambda _req: _Response(200, body))

    result = client.ping()

    assert result == PingResult("paired", streaming=True, busy=False, local_only=True)
    assert result.paired is True
    assert seen[0].full_url == "http://phone:8766/v1/ping"
    assert seen[0].get_header("Authorization") == "Bearer tok"


def test_ping_treats_a_body_that_is_not_telescope_as_unreachable(monkeypatch, client):
    _stub_urlopen(monkeypatch, lambda _req: _Response(200, b"OK"))
    assert client.ping().status == "unreachable"


def test_ping_maps_401_to_not_paired_and_other_failures_to_unreachable(monkeypatch, client):
    _stub_urlopen(monkeypatch, lambda _req: _http_error(401))
    assert client.ping().status == "not_paired"

    _stub_urlopen(monkeypatch, lambda _req: _http_error(500))
    assert client.ping().status == "unreachable"

    _stub_urlopen(monkeypatch, lambda _req: OSError("no route to host"))
    assert client.ping().status == "unreachable"


def test_ping_survives_a_body_that_is_valid_json_but_not_an_object(monkeypatch, client):
    _stub_urlopen(monkeypatch, lambda _req: _Response(200, b"[1, 2, 3]"))

    assert client.ping().status == "unreachable"


# ── /v1/session ───────────────────────────────────────────────────────────────

def test_start_posts_the_action_as_json(monkeypatch, client):
    seen = _stub_urlopen(monkeypatch, lambda _req: _Response(200, b'{"ok": true}'))

    assert client.start() == SessionResult(ok=True)

    req = seen[0]
    assert req.full_url == "http://phone:8766/v1/session"
    assert req.get_method() == "POST"
    assert req.get_header("Content-type") == "application/json"
    assert json.loads(req.data.decode()) == {"action": "start"}


def test_stop_posts_the_stop_action(monkeypatch, client):
    seen = _stub_urlopen(monkeypatch, lambda _req: _Response(200, b'{"ok": true}'))

    assert client.stop().ok is True
    assert json.loads(seen[0].data.decode()) == {"action": "stop"}


def test_an_http_error_is_reported_with_its_code(monkeypatch, client):
    _stub_urlopen(monkeypatch, lambda _req: _http_error(404))

    assert client.start() == SessionResult(ok=False, error="http_404")


def test_a_refusal_body_carries_the_phone_s_reason_through(monkeypatch, client):
    _stub_urlopen(
        monkeypatch,
        lambda _req: _Response(200, b'{"ok": false, "error": "no_camera_permission"}'),
    )

    assert client.start() == SessionResult(ok=False, error="no_camera_permission")


def test_transport_and_auth_failures_are_distinguished(monkeypatch, client):
    _stub_urlopen(monkeypatch, lambda _req: _http_error(401))
    assert client.start() == SessionResult(ok=False, error="not_paired")

    _stub_urlopen(monkeypatch, lambda _req: _http_error(503))
    assert client.start() == SessionResult(ok=False, error="http_503")

    _stub_urlopen(monkeypatch, lambda _req: OSError("connection refused"))
    assert client.start() == SessionResult(ok=False, error="unreachable")


def test_an_ok_false_body_without_a_reason_still_fails_closed(monkeypatch, client):
    _stub_urlopen(monkeypatch, lambda _req: _Response(200, b'{"ok": false}'))

    result = client.start()

    assert result.ok is False
    assert result.error == "refused"


def test_a_base_url_with_a_trailing_slash_does_not_double_up(monkeypatch):
    client = PhoneSessionClient("http://phone:8766/", PhoneAuth("tok"))
    seen = _stub_urlopen(monkeypatch, lambda _req: _Response(200, b"OK"))

    client.ping()

    assert seen[0].full_url == "http://phone:8766/v1/ping"


# ── hello ─────────────────────────────────────────────────────────────────────

def test_hello_reads_identity_and_version(monkeypatch, client):
    body = json.dumps({"protocol": 2, "phoneId": "p1", "phoneName": "Pixel",
                       "appVersion": "0.5.0", "build": 60}).encode()
    _stub_urlopen(monkeypatch, lambda _req: _Response(200, body))
    assert client.hello() == Hello(HELLO_OK, "p1", "Pixel", 2, "0.5.0", 60)


def test_hello_from_an_app_without_version_fields_still_identifies_it(monkeypatch, client):
    body = json.dumps({"protocol": 2, "phoneId": "p1", "phoneName": "Pixel"}).encode()
    _stub_urlopen(monkeypatch, lambda _req: _Response(200, body))
    hello = client.hello()
    assert hello.ok and hello.phone_id == "p1" and hello.app_version == "" and hello.build == 0


def test_hello_tells_an_old_app_from_nothing_at_all(monkeypatch, client):
    _stub_urlopen(monkeypatch, lambda _req: _http_error(404))
    assert client.hello().status == HELLO_MISSING
    _stub_urlopen(monkeypatch, lambda _req: urllib.error.URLError("refused"))
    assert client.hello().status == HELLO_NONE
    _stub_urlopen(monkeypatch, lambda _req: _Response(200, b"[1]"))
    assert client.hello().status == HELLO_NONE


def test_an_oversized_reply_is_treated_as_no_phone(monkeypatch, client):
    huge = b'{"phoneId": "p1", "pad": "' + b"x" * (session_client_module.MAX_REPLY_BYTES + 10) + b'"}'
    _stub_urlopen(monkeypatch, lambda _req: _Response(200, huge))
    assert client.hello().status == HELLO_NONE
    assert client.ping().status == "unreachable"
    assert client.start() == SessionResult(ok=False, error="unreachable")


def test_phone_names_are_cleaned_into_plain_display_text():
    from telescope.session_client import MAX_NAME_CHARS, clean_name
    assert clean_name("Sam's Pixel") == "Sam's Pixel"
    assert clean_name("<b>Your bank</b>\n\x07phone") == "bYour bank/b phone"
    assert len(clean_name("x" * 500)) == MAX_NAME_CHARS
    assert clean_name("  \t ") == ""


def test_a_name_from_the_phone_arrives_cleaned(monkeypatch, client):
    body = json.dumps({"protocol": 3, "phoneId": "p1", "phoneName": "<img src=x>Pixel"}).encode()
    _stub_urlopen(monkeypatch, lambda _req: _Response(200, body))
    assert client.hello().phone_name == "img src=xPixel"


def test_an_address_nobody_answers_on_costs_one_timeout_not_two(monkeypatch):
    # The plain-HTTP look for an old app only follows a failed TLS handshake, never a dead address.
    from telescope.pinned_https import HandshakeFailed, PhoneAuth
    probes = []
    client = PhoneSessionClient("https://10.0.0.9:8766", PhoneAuth("tok", "ab" * 32))
    monkeypatch.setattr(client, "_answers_plain_http", lambda timeout: probes.append(timeout) or True)
    for failure, expect_probe in ((TimeoutError("timed out"), False), (ConnectionRefusedError(), False),
                                  (HandshakeFailed("not TLS"), True)):
        def fail(*_args, _failure=failure, **_kwargs):
            raise urllib.error.URLError(_failure)
        monkeypatch.setattr(client.auth, "open", fail)
        probes.clear()
        status = client.hello(timeout=0.1).status
        assert bool(probes) == expect_probe
        assert status == (HELLO_MISSING if expect_probe else HELLO_NONE)


def test_the_look_for_an_old_app_skips_a_configured_proxy(monkeypatch):
    # A proxy answering 502 for a closed phone app would read as "an old app is here".
    import socket
    import threading
    from http.server import BaseHTTPRequestHandler, HTTPServer

    class Proxy(BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(502)
            self.end_headers()

        def log_message(self, *_a):
            pass

    proxy = HTTPServer(("127.0.0.1", 0), Proxy)
    threading.Thread(target=proxy.serve_forever, daemon=True).start()
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        closed_port = s.getsockname()[1]
    monkeypatch.setenv("http_proxy", f"http://127.0.0.1:{proxy.server_address[1]}")
    monkeypatch.delenv("no_proxy", raising=False)
    monkeypatch.delenv("NO_PROXY", raising=False)
    try:
        client = PhoneSessionClient(f"https://127.0.0.1:{closed_port}", PhoneAuth("tok", "ab" * 32))
        assert client._answers_plain_http(1.0) is False
    finally:
        proxy.shutdown()
        proxy.server_close()


def test_a_listener_that_answers_with_something_other_than_http_is_not_a_phone():
    import socket
    import threading
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)

    def answer():
        conn, _ = srv.accept()
        conn.recv(4096)
        conn.sendall(b"\x15\x03\x03\x00\x02\x02\x28")  # a TLS alert, not a status line
        conn.close()

    threading.Thread(target=answer, daemon=True).start()
    try:
        client = PhoneSessionClient(f"https://127.0.0.1:{srv.getsockname()[1]}", PhoneAuth("tok", "ab" * 32))
        assert client._answers_plain_http(2.0) is False
    finally:
        srv.close()
