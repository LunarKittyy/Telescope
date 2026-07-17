import json
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

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
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video", "tok")

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
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video", "tok")

    client.send(action="iso", value=100)
    client.send(action="camera", id="cam0")
    client.send(action="torch", value="1")

    time.sleep(0.5)
    client.close()

    actions = [r["action"] for r in recording_server.received]
    assert actions == ["iso", "camera", "torch"]


def test_requests_carry_bearer_token(recording_server):
    port = recording_server.server_address[1]
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video", "secret-tok")

    client.send(action="iso", value=100)
    time.sleep(0.3)
    client.close()

    assert recording_server.auth_headers == ["Bearer secret-tok"]


def test_close_stops_accepting_new_requests(recording_server):
    port = recording_server.server_address[1]
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video", "tok")
    client.close()
    client.send(action="iso", value=1)

    time.sleep(0.3)
    assert recording_server.received == []


class _Response:
    def __init__(self, body=b"{}"):
        self.body = body
        self.read_count = 0

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        pass

    def read(self):
        self.read_count += 1
        return self.body


def test_base_url_strips_only_trailing_video_component(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", "tok")
    assert client.base == "http://phone"
    nested = PhoneControlClient("http://video-host/path/video", "tok")
    assert nested.base == "http://video-host/path"


def test_get_state_decodes_json_and_sends_auth_header(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", "tok123")
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
    client = PhoneControlClient("http://phone/video", "tok")

    def open_url(*_args, **_kwargs):
        if isinstance(effect, OSError):
            raise effect
        return _Response(b"not-json")

    monkeypatch.setattr(phone_client_module.urllib.request, "urlopen", open_url)
    assert client.get_state() is None


def test_send_now_posts_json_body_with_auth_header(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", "tok123")
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
    client = PhoneControlClient("http://phone/video", "tok")
    monkeypatch.setattr(
        phone_client_module.urllib.request,
        "urlopen",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(OSError("offline")),
    )
    client._send_now({"action": "iso", "value": 100})


def test_close_is_idempotent(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", "tok")
    client.close()
    client.close()
    assert client._closed is True
    assert client._queue.get_nowait() is None


def test_worker_skips_stale_pending_key(monkeypatch):
    monkeypatch.setattr(phone_client_module.threading.Thread, "start", lambda _self: None)
    client = PhoneControlClient("http://phone/video", "tok")
    sent = []
    monkeypatch.setattr(client, "_send_now", sent.append)
    client._queue.put("iso")
    client._queue.put(None)
    client._worker()
    assert sent == []
