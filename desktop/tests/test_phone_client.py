import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

from telescope.phone_client import PhoneControlClient


class _RecordingHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        qs = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        with self.server.lock:
            self.server.received.append({k: v[0] for k, v in qs.items()})
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
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield srv
    srv.shutdown()


def test_rapid_updates_coalesce_to_latest_value(recording_server):
    port = recording_server.server_address[1]
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video")

    client.send(action="iso", value=100)
    client.send(action="iso", value=200)
    client.send(action="iso", value=300)

    time.sleep(0.5)
    client.close()

    iso_requests = [r for r in recording_server.received if r["action"] == "iso"]
    assert len(iso_requests) == 1
    assert iso_requests[0]["value"] == "300"


def test_non_coalescing_actions_preserve_order(recording_server):
    port = recording_server.server_address[1]
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video")

    client.send(action="iso", value=100)
    client.send(action="camera", id="cam0")
    client.send(action="torch", value="1")

    time.sleep(0.5)
    client.close()

    actions = [r["action"] for r in recording_server.received]
    assert actions == ["iso", "camera", "torch"]


def test_close_stops_accepting_new_requests(recording_server):
    port = recording_server.server_address[1]
    client = PhoneControlClient(f"http://127.0.0.1:{port}/video")
    client.close()
    client.send(action="iso", value=1)

    time.sleep(0.3)
    assert recording_server.received == []
