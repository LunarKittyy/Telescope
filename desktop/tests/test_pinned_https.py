import http.server
import ssl
import threading
import urllib.error
import urllib.request
from pathlib import Path

import pytest

from telescope.phones import UNREACHABLE, Phone, RouteResolver, UsbTunnels
from telescope.pinned_https import PhoneAuth, fingerprint, is_fingerprint, pin_rejected
from telescope.session_client import HELLO_MISSING, HELLO_OK, PhoneSessionClient

FIXTURES = Path(__file__).parent / "fixtures"


def _pin_of(name: str) -> str:
    pem = (FIXTURES / f"test_{name}.crt").read_text()
    return fingerprint(ssl.PEM_cert_to_DER_cert(pem))


class _Handler(http.server.BaseHTTPRequestHandler):
    seen_auth: list = []

    def do_GET(self):
        _Handler.seen_auth.append(self.headers.get("Authorization"))
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("Location", "http://127.0.0.1:1/stolen")
            self.end_headers()
            return
        body = b'{"ok": true}'
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


@pytest.fixture
def tls_server(request):
    """A local HTTPS server presenting the named test certificate."""
    name = getattr(request, "param", "phone")
    _Handler.seen_auth = []
    server = http.server.HTTPServer(("127.0.0.1", 0), _Handler)
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.load_cert_chain(FIXTURES / f"test_{name}.crt", FIXTURES / f"test_{name}.key")
    server.socket = ctx.wrap_socket(server.socket, server_side=True)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield f"https://127.0.0.1:{server.server_address[1]}"
    server.shutdown()
    server.server_close()


def _get(auth: PhoneAuth, url: str):
    return auth.open(urllib.request.Request(url, headers=auth.headers()), timeout=3)


def test_the_paired_phone_is_reached_and_gets_the_token(tls_server):
    auth = PhoneAuth("secret", _pin_of("phone"))
    with _get(auth, f"{tls_server}/v1/ping") as r:
        assert r.read() == b'{"ok": true}'
    assert _Handler.seen_auth == ["Bearer secret"]


@pytest.mark.parametrize("tls_server", ["impostor"], indirect=True)
def test_an_impostor_never_sees_the_token(tls_server):
    auth = PhoneAuth("secret", _pin_of("phone"))
    with pytest.raises(urllib.error.URLError) as info:
        _get(auth, f"{tls_server}/v1/ping")
    assert pin_rejected(info.value)
    assert _Handler.seen_auth == []


def test_a_redirect_is_not_followed(tls_server):
    auth = PhoneAuth("secret", _pin_of("phone"))
    with pytest.raises(urllib.error.HTTPError) as info:
        _get(auth, f"{tls_server}/redirect")
    assert info.value.code == 302
    assert _Handler.seen_auth == ["Bearer secret"]


def test_https_without_a_pin_fails_closed(tls_server):
    with pytest.raises(urllib.error.URLError) as info:
        _get(PhoneAuth("secret"), f"{tls_server}/v1/ping")
    assert pin_rejected(info.value)
    assert _Handler.seen_auth == []


def test_a_pinned_phone_is_never_sent_plain_http():
    with pytest.raises(urllib.error.URLError):
        _get(PhoneAuth("secret", _pin_of("phone")), "http://127.0.0.1:1/v1/ping")


def test_fingerprint_shape():
    assert is_fingerprint(_pin_of("phone"))
    assert not is_fingerprint("AB" * 32)
    assert not is_fingerprint("ab" * 31)
    assert not is_fingerprint(None)


class _PhoneHandler(_Handler):
    """Answers like a phone's session port: hello names ph-1, ping needs a token."""

    def do_GET(self):
        _Handler.seen_auth.append(self.headers.get("Authorization"))
        body = b'{"protocol": 3, "phoneId": "ph-1", "phoneName": "Pixel"}'
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class _QuietServer(http.server.HTTPServer):
    def handle_error(self, request, client_address):
        pass  # failed handshakes are the point of these tests


def _serve(handler, cert=None):
    _Handler.seen_auth = []
    server = _QuietServer(("127.0.0.1", 0), handler)
    if cert:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.minimum_version = ssl.TLSVersion.TLSv1_2
        ctx.load_cert_chain(FIXTURES / f"test_{cert}.crt", FIXTURES / f"test_{cert}.key")
        server.socket = ctx.wrap_socket(server.socket, server_side=True)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def test_a_fake_phone_repeating_the_right_id_is_ignored_and_never_sent_the_token():
    server = _serve(_PhoneHandler, cert="impostor")
    try:
        port = server.server_address[1]
        phone = Phone("ph-1", "Pixel", "secret", [], cert_sha256=_pin_of("phone"))
        resolver = RouteResolver(
            adb_available=lambda: False, adb_device_states=list, discover=lambda pid: ["127.0.0.1"],
            tunnels=UsbTunnels(lambda s, p: None, lambda s, p: None), wifi_timeout=1.0,
            client_factory=lambda base, auth: PhoneSessionClient(base.replace(":8766", f":{port}"), auth))
        assert resolver.resolve(phone).status == UNREACHABLE
        assert all(a is None for a in _Handler.seen_auth)
    finally:
        server.shutdown()
        server.server_close()


def test_the_real_phone_is_found_over_tls():
    server = _serve(_PhoneHandler, cert="phone")
    try:
        client = PhoneSessionClient(f"https://127.0.0.1:{server.server_address[1]}", PhoneAuth("secret", _pin_of("phone")))
        hello = client.hello()
        assert hello.status == HELLO_OK and hello.phone_id == "ph-1"
        assert client.ping().status == "paired"
        assert _Handler.seen_auth[-1] == "Bearer secret"
    finally:
        server.shutdown()
        server.server_close()


def test_an_app_from_before_tls_reads_as_outdated_without_being_sent_the_token():
    server = _serve(_PhoneHandler)
    try:
        client = PhoneSessionClient(f"https://127.0.0.1:{server.server_address[1]}", PhoneAuth("secret", _pin_of("phone")))
        assert client.hello(timeout=1.0).status == HELLO_MISSING
        assert all(a is None for a in _Handler.seen_auth)
    finally:
        server.shutdown()
        server.server_close()
