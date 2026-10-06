import json
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

from telescope.pinned_https import PhoneAuth
from telescope.phone_client import PhoneControlClient
import telescope.phone_client as phone_client_module


class _RecordingHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        with self.server.lock:
            self.server.received.append(json.loads(body))
            self.server.auth_headers.append(self.headers.get("Authorization"))
        time.sleep(0.05)
        self.send_response(200)
        self.end_headers()

    def log_message(self, *args):
        pass


@pytest.fixture
def recording_server():
    srv = HTTPServer(("127.0.0.1", 0), _RecordingHandler)
    srv.lock = threading.Lock()
    srv.received = []
    srv.auth_headers = []
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield srv
    srv.shutdown()


def test_rapid_updates_coalesce_to_latest_value(recording_server):
    port = recording_server.server_address[1]
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video", PhoneAuth("tok"))

    client.send(action="iso", value=100)
    client.send(action="iso", value=200)
    client.send(action="iso", value=300)

    time.sleep(0.5)
    client.close()

    iso_requests = [r for r in recording_server.received if r["action"] == "iso"]
    assert len(iso_requests) == 1
    assert iso_requests[0]["value"] == 300


def test_non_coalescing_actions_preserve_order(recording_server):
    port = recording_server.server_address[1]
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video", PhoneAuth("tok"))

    client.send(action="iso", value=100)
    client.send(action="camera", id="cam0")
    client.send(action="torch", value="1")

    time.sleep(0.5)
    client.close()

    actions = [r["action"] for r in recording_server.received]
    assert actions == ["iso", "camera", "torch"]


def test_requests_carry_bearer_token(recording_server):
    port = recording_server.server_address[1]
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video", PhoneAuth("secret-tok"))

    client.send(action="iso", value=100)
    time.sleep(0.3)
    client.close()

    assert recording_server.auth_headers == ["Bearer secret-tok"]


def test_close_stops_accepting_new_requests(recording_server):
    port = recording_server.server_address[1]
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video", PhoneAuth("tok"))
    client.close()
    client.send(action="iso", value=1)

    time.sleep(0.3)
    assert recording_server.received == []


def test_close_cancels_queued_and_pending_requests(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok"))
    sent = []
    monkeypatch.setattr(client, "_send_now", sent.append)

    # Both coalescing and non-coalescing actions must be cancelled by close().
    client.send(action="iso", value=100)
    client.send(action="camera", id="cam0")
    client.close()

    client._worker()
    assert sent == []


def test_resend_settings_sends_the_last_of_each_in_the_order_last_sent(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok"))
    client.send(action="iso", value=100)
    client.send(action="wb_auto")
    client.send(action="torch", value="1")  # a one-off, not a setting the phone keeps
    client.send(action="auto")  # back to auto exposure after the manual ISO
    client.send(action="zoom", value=2.0)
    resent = []
    monkeypatch.setattr(client, "send", lambda **params: resent.append(params))

    client.resend_settings()

    assert resent == [{"action": "iso", "value": 100}, {"action": "wb_auto"}, {"action": "auto"},
                      {"action": "zoom", "value": 2.0}]


class _Response:
    def __init__(self, body=b"{}"):
        self.body = body
        self.read_count = 0

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        pass

    def read(self, n=-1):
        self.read_count += 1
        return self.body if n < 0 else self.body[:n]


def test_base_url_strips_only_trailing_video_component(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok"))
    assert client.base == "http://phone"
    nested = PhoneControlClient("http://video-host/path/video", PhoneAuth("tok"))
    assert nested.base == "http://video-host/path"


def test_get_state_decodes_json_and_sends_auth_header(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok123"))
    calls = []
    response = _Response(b'{"battery": 81}')
    monkeypatch.setattr(
        phone_client_module.urllib.request,
        "urlopen",
        lambda req, timeout: calls.append((req.full_url, req.get_header("Authorization"), timeout)) or response,
    )

    assert client.get_state() == {"battery": 81}
    assert calls == [("http://phone/state", "Bearer tok123", 4)]
    assert response.read_count == 1


@pytest.mark.parametrize("effect", [OSError("offline"), ValueError("bad json")])
def test_get_state_returns_none_on_transport_or_json_error(monkeypatch, effect):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok"))

    def open_url(*_args, **_kwargs):
        if isinstance(effect, OSError):
            raise effect
        return _Response(b"not-json")

    monkeypatch.setattr(phone_client_module.urllib.request, "urlopen", open_url)
    assert client.get_state() is None


def test_get_state_returns_none_for_json_that_is_not_an_object(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok"))
    monkeypatch.setattr(phone_client_module.urllib.request, "urlopen", lambda *_a, **_k: _Response(b"[1, 2]"))
    assert client.get_state() is None


def test_send_now_posts_json_body_with_auth_header(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok123"))
    calls = []
    response = _Response(b"ok")
    monkeypatch.setattr(
        phone_client_module.urllib.request,
        "urlopen",
        lambda req, timeout: calls.append((req, timeout)) or response,
    )

    client._send_now({"action": "camera", "id": "wide angle"})

    assert len(calls) == 1
    req, timeout = calls[0]
    assert timeout == 3
    assert req.full_url == "http://phone/control"
    assert req.get_method() == "POST"
    assert req.get_header("Authorization") == "Bearer tok123"
    assert req.get_header("Content-type") == "application/json"
    assert json.loads(req.data) == {"action": "camera", "id": "wide angle"}
    assert response.read_count == 1


def test_send_now_swallows_transport_errors(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok"))
    monkeypatch.setattr(
        phone_client_module.urllib.request,
        "urlopen",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(OSError("offline")),
    )
    assert client._send_now({"action": "iso", "value": 100}) is False


def test_close_is_idempotent(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok"))
    client.close()
    client.close()
    assert client._closed is True
    assert client._queue.get_nowait() is None


def test_worker_skips_stale_pending_key(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok"))
    sent = []
    monkeypatch.setattr(client, "_send_now", sent.append)
    client._queue.put(("iso", 1))
    client._queue.put(None)
    client._worker()
    assert sent == []


def _flaky_client(monkeypatch, outcomes):
    """A client whose sends go by a script: True got there, False was lost on the way."""
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    monkeypatch.setattr(phone_client_module, "_RETRY_FIRST_WAIT_S", 0.01)
    monkeypatch.setattr(phone_client_module, "_RETRY_MAX_WAIT_S", 0.02)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok"))
    tries = []

    def send_now(params):
        tries.append(params)
        return outcomes.pop(0) if outcomes else False
    monkeypatch.setattr(client, "_send_now", send_now)
    return client, tries


def test_a_request_lost_on_a_stalling_link_is_sent_again_until_it_gets_through(monkeypatch):
    client, tries = _flaky_client(monkeypatch, [False, False, True])
    client._deliver({"action": "iso", "value": 100})
    assert tries == [{"action": "iso", "value": 100}] * 3


def test_a_lost_request_gives_way_to_a_newer_value(monkeypatch):
    client, tries = _flaky_client(monkeypatch, [False])
    client.send(action="iso", value=200)  # the newer value is already waiting its turn
    client._deliver({"action": "iso", "value": 100})
    assert tries == [{"action": "iso", "value": 100}]


def test_a_lost_request_gives_up_in_the_end(monkeypatch):
    client, tries = _flaky_client(monkeypatch, [])
    monkeypatch.setattr(phone_client_module, "_RETRY_FOR_S", 0.1)
    client._deliver({"action": "iso", "value": 100})
    assert 2 <= len(tries) < 20


def test_close_ends_the_retries(monkeypatch):
    client, tries = _flaky_client(monkeypatch, [])
    monkeypatch.setattr(phone_client_module, "_RETRY_FIRST_WAIT_S", 5.0)
    lost = client._send_now

    def send_then_close(params):
        client.close()  # e.g. Stop, while the first try hangs on a stalled link
        return lost(params)
    client._send_now = send_then_close
    started = time.monotonic()
    client._deliver({"action": "camera", "id": "0"})
    assert len(tries) == 1 and time.monotonic() - started < 1.0


def test_a_request_the_phone_refused_is_not_sent_again(monkeypatch):
    import urllib.error
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok"))
    calls = []

    def refuse(req, timeout):
        calls.append(req)
        raise urllib.error.HTTPError(req.full_url, 400, "bad value", {}, None)
    monkeypatch.setattr(phone_client_module.urllib.request, "urlopen", refuse)
    client._deliver({"action": "iso", "value": -5})
    assert len(calls) == 1


def test_a_stalled_link_still_delivers_the_setting(recording_server):
    # The first try's connection is refused, like a link mid-stall; the retry reaches the phone.
    port = recording_server.server_address[1]
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video", PhoneAuth("tok"))
    real = client._send_now
    first = []

    def send_now(params):
        if not first:
            first.append(True)
            return False
        return real(params)
    client._send_now = send_now
    client.send(action="iso", value=100)
    deadline = time.monotonic() + 3
    while not recording_server.received and time.monotonic() < deadline:
        time.sleep(0.02)
    client.close()
    assert recording_server.received == [{"action": "iso", "value": 100}]


def test_a_value_sent_again_goes_after_what_was_sent_between(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", PhoneAuth("tok"))
    sent = []
    monkeypatch.setattr(client, "_deliver", sent.append)
    client.send(action="iso", value=100)
    client.send(action="shutter", value=10_000_000)
    client.send(action="auto")  # Manual, Auto, then Manual again
    client.send(action="iso", value=200)
    client.send(action="shutter", value=20_000_000)
    client._queue.put(None)
    client._worker()
    assert sent == [{"action": "auto"}, {"action": "iso", "value": 200}, {"action": "shutter", "value": 20_000_000}]


def test_a_new_route_keeps_the_settings_sent_on_the_old_one(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    old = PhoneControlClient("http://usb/video", PhoneAuth("tok"))
    old.send(action="iso", value=500)
    old.send(action="zoom", value=2.0)
    new = PhoneControlClient("http://wifi/video", PhoneAuth("tok"))
    new.keep_settings_of(old)
    resent = []
    monkeypatch.setattr(new, "send", lambda **params: resent.append(params))
    new.resend_settings()
    assert resent == [{"action": "iso", "value": 500}, {"action": "zoom", "value": 2.0}]
