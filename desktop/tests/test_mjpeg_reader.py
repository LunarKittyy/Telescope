import http.server
import threading

import cv2
import numpy as np
import pytest

from telescope.mjpeg_reader import MjpegReader


def _make_frame_bytes():
    frame = np.zeros((2, 2, 3), dtype=np.uint8)
    frame[:] = (10, 20, 30)
    ok, buf = cv2.imencode(".jpg", frame)
    assert ok
    return buf.tobytes()


class _MjpegHandler(http.server.BaseHTTPRequestHandler):
    frames = []
    require_token = None

    def do_GET(self):
        auth = self.headers.get("Authorization", "")
        if self.require_token is not None and auth != f"Bearer {self.require_token}":
            self.send_response(401)
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Type", "multipart/x-mixed-replace; boundary=--mjpegframe")
        self.end_headers()
        for jpeg in self.frames:
            self.wfile.write(b"--mjpegframe\r\n")
            self.wfile.write(b"Content-Type: image/jpeg\r\n")
            self.wfile.write(f"Content-Length: {len(jpeg)}\r\n\r\n".encode())
            self.wfile.write(jpeg)
            self.wfile.write(b"\r\n")
        self.wfile.flush()

    def log_message(self, *args):
        pass


@pytest.fixture
def mjpeg_server():
    server = http.server.HTTPServer(("127.0.0.1", 0), _MjpegHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield server
    server.shutdown()
    thread.join(timeout=2)


def test_reads_authenticated_frames_in_order_then_reports_eof(mjpeg_server):
    _MjpegHandler.frames = [_make_frame_bytes(), _make_frame_bytes()]
    _MjpegHandler.require_token = "secret"
    port = mjpeg_server.server_address[1]

    reader = MjpegReader(f"http://127.0.0.1:{port}/v1/video", "secret")
    assert reader.open() is True
    assert reader.isOpened() is True

    ok1, frame1 = reader.read()
    assert ok1 is True
    assert frame1.shape == (2, 2, 3)

    ok2, frame2 = reader.read()
    assert ok2 is True

    ok3, frame3 = reader.read()
    assert ok3 is False
    assert frame3 is None

    reader.release()
    assert reader.isOpened() is False


def test_rejects_when_unauthorized(mjpeg_server):
    _MjpegHandler.frames = []
    _MjpegHandler.require_token = "secret"
    port = mjpeg_server.server_address[1]

    reader = MjpegReader(f"http://127.0.0.1:{port}/v1/video", "wrong-token")
    assert reader.open() is False
    assert reader.isOpened() is False


def test_rejects_non_multipart_response():
    class _PlainHandler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(b"not multipart")

        def log_message(self, *args):
            pass

    server = http.server.HTTPServer(("127.0.0.1", 0), _PlainHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        port = server.server_address[1]
        reader = MjpegReader(f"http://127.0.0.1:{port}/v1/video", "tok")
        assert reader.open() is False
    finally:
        server.shutdown()
        thread.join(timeout=2)


def test_open_returns_false_on_connection_failure():
    reader = MjpegReader("http://127.0.0.1:1/v1/video", "tok", timeout=0.5)
    assert reader.open() is False


def test_read_before_open_returns_false():
    reader = MjpegReader("http://127.0.0.1:1/v1/video", "tok")
    ok, frame = reader.read()
    assert ok is False
    assert frame is None


class _HoldingHandler(http.server.BaseHTTPRequestHandler):
    """Sends one frame, then keeps the connection open like a phone between frames."""
    release = threading.Event()

    def do_GET(self):
        jpeg = _make_frame_bytes()
        self.send_response(200)
        self.send_header("Content-Type", "multipart/x-mixed-replace; boundary=--mjpegframe")
        self.end_headers()
        self.wfile.write(b"--mjpegframe\r\nContent-Type: image/jpeg\r\n")
        self.wfile.write(f"Content-Length: {len(jpeg)}\r\n\r\n".encode() + jpeg + b"\r\n")
        self.wfile.flush()
        self.release.wait(5)

    def log_message(self, *args):
        pass


def test_returns_a_frame_without_waiting_for_the_next_one():
    import time
    _HoldingHandler.release.clear()
    server = http.server.HTTPServer(("127.0.0.1", 0), _HoldingHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        reader = MjpegReader(f"http://127.0.0.1:{server.server_address[1]}/v1/video", "t")
        assert reader.open()
        t0 = time.monotonic()
        ok, frame = reader.read()
        elapsed = time.monotonic() - t0
        assert ok and frame.shape == (2, 2, 3)
        # Blocking on a full-size chunk read here stalls every frame until the next one arrives.
        assert elapsed < 1.0
        reader.release()
    finally:
        _HoldingHandler.release.set()
        server.shutdown()
        thread.join(timeout=2)
