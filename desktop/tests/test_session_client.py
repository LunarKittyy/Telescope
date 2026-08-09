import json
import urllib.error
import urllib.request

import pytest

import telescope.session_client as session_client_module
from telescope.session_client import PhoneSessionClient, PingResult, SessionResult


class _Response:
    def __init__(self, status=200, body=b""):
        self.status = status
        self._body = body

    def read(self):
        return self._body

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
    return PhoneSessionClient("http://phone:8766", "tok")


# ── ping ──────────────────────────────────────────────────────────────────────

def test_ping_parses_the_phone_state_body(monkeypatch, client):
    body = json.dumps(
        {"protocol": 1, "streaming": True, "busy": False, "localOnly": True}
    ).encode()
    seen = _stub_urlopen(monkeypatch, lambda _req: _Response(200, body))

    result = client.ping()

    assert result == PingResult("paired", streaming=True, busy=False, local_only=True)
    assert result.paired is True
    assert result.knows_session is True
    assert seen[0].full_url == "http://phone:8766/v1/ping"
    assert seen[0].get_header("Authorization") == "Bearer tok"


def test_ping_treats_a_bodyless_200_as_paired_but_unaware(monkeypatch, client):
    # What an app predating /v1/session answers: a bare "OK". Still proof of
    # a live pairing, just with nothing to say about the stream - which is
    # what knows_session exists to signal.
    _stub_urlopen(monkeypatch, lambda _req: _Response(200, b"OK"))

    result = client.ping()

    assert result.status == "paired"
    assert result.knows_session is False
    assert result.streaming is None


def test_ping_maps_401_to_not_paired_and_other_failures_to_unreachable(monkeypatch, client):
    _stub_urlopen(monkeypatch, lambda _req: _http_error(401))
    assert client.ping().status == "not_paired"

    _stub_urlopen(monkeypatch, lambda _req: _http_error(500))
    assert client.ping().status == "unreachable"

    _stub_urlopen(monkeypatch, lambda _req: OSError("no route to host"))
    assert client.ping().status == "unreachable"


def test_ping_survives_a_body_that_is_valid_json_but_not_an_object(monkeypatch, client):
    _stub_urlopen(monkeypatch, lambda _req: _Response(200, b"[1, 2, 3]"))

    result = client.ping()

    assert result.status == "paired"
    assert result.knows_session is False


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


def test_a_404_reports_unsupported_rather_than_an_error(monkeypatch, client):
    # An APK predating this endpoint. The desktop's answer is to fall back to
    # connecting to a hand-started stream, not to block the user, so this
    # must be distinguishable from a real failure.
    _stub_urlopen(monkeypatch, lambda _req: _http_error(404))

    result = client.start()

    assert result.unsupported is True
    assert result.ok is False
    assert result.error is None


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
    client = PhoneSessionClient("http://phone:8766/", "tok")
    seen = _stub_urlopen(monkeypatch, lambda _req: _Response(200, b"OK"))

    client.ping()

    assert seen[0].full_url == "http://phone:8766/v1/ping"
