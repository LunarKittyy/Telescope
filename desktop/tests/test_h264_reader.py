import fractions
import http.server
import threading

import numpy as np
import pytest

av = pytest.importorskip("av")

from telescope.pinned_https import PhoneAuth
from telescope import h264_reader  # noqa: E402
from telescope.h264_reader import H264Reader, decode_newest, new_decoder  # noqa: E402


_AUD = b"\x00\x00\x00\x01\x09\xf0"


def _packets(values, size=(64, 48)):
    """Annex-B H.264 of flat grey frames, one packet per value, framed like the phone sends them:
    no B-frames, and a delimiter after each packet."""
    w, h = size
    enc = av.CodecContext.create("libx264", "w")
    enc.width, enc.height, enc.pix_fmt = w, h, "yuv420p"
    enc.time_base = fractions.Fraction(1, 30)
    enc.options = {"tune": "zerolatency", "g": "5", "bf": "0"}
    out = []
    for i, v in enumerate(values):
        frame = av.VideoFrame.from_ndarray(np.full((h, w, 3), v, dtype=np.uint8), format="bgr24")
        frame.pts = i
        out += [bytes(p) + _AUD for p in enc.encode(frame)]
    return out


def _encode(values):
    return b"".join(_packets(values))


def test_each_packet_shows_its_frame_straight_away():
    values = [20, 60, 100, 140, 180, 220]
    dec = new_decoder()
    shown = [decode_newest(dec, p) for p in _packets(values)]
    assert all(f is not None for f in shown)
    assert all(abs(f.mean() - v) <= 8 for f, v in zip(shown, values))  # frame i, not frame i-1


def test_pieces_of_any_size_decode_in_order():
    data = _encode([20, 60, 100, 140, 180, 220])
    dec = new_decoder()
    seen = [decode_newest(dec, data[i:i + 97]) for i in range(0, len(data), 97)]
    means = [int(f.mean()) for f in seen if f is not None]
    assert means == sorted(means) and means[-1] > 200


class _Handler(http.server.BaseHTTPRequestHandler):
    body = b""
    content_type = "video/h264"

    def do_GET(self):
        if self.headers.get("Authorization") != "Bearer secret":
            self.send_response(401)
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Type", self.content_type)
        self.end_headers()
        self.wfile.write(self.body)
        self.wfile.flush()

    def log_message(self, *args):
        pass


@pytest.fixture
def server():
    srv = http.server.HTTPServer(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=srv.serve_forever, daemon=True)
    thread.start()
    yield srv
    srv.shutdown()
    thread.join(timeout=2)


def test_reader_reads_authenticated_frames_then_reports_eof(server):
    _Handler.body = _encode([30, 90, 150, 210])
    _Handler.content_type = "video/h264"
    url = f"http://127.0.0.1:{server.server_address[1]}/v1/video.h264"

    assert H264Reader(url, PhoneAuth("wrong")).open() is False
    reader = H264Reader(url, PhoneAuth("secret"))
    assert reader.open() is True and reader.isOpened()
    frames, total = [], 0
    while True:
        ok, frame = reader.read()
        if not ok:
            break
        frames.append(frame)
        total += reader.last_frame_bytes
    reader.release()
    assert frames and frames[0].shape == (48, 64, 3)
    assert 0 < total <= len(_Handler.body)
    assert reader.isOpened() is False


def test_a_burst_shows_its_newest_frame_but_counts_every_frame_that_arrived(server):
    # Frames that come in together (after a moment of backlog) are all decoded; only the newest is shown, to stay
    # live, but each counts as arrived, or the stream would look like it's falling behind.
    _Handler.body = _encode([30, 60, 90, 120, 150, 180])
    reader = H264Reader(f"http://127.0.0.1:{server.server_address[1]}/v1/video.h264", PhoneAuth("secret"))
    assert reader.open()
    shown, arrived = 0, 0
    while True:
        ok, _frame = reader.read()
        if not ok:
            break
        shown += 1
        arrived += reader.last_frame_count
    reader.release()
    assert arrived == 6 and shown < arrived


def test_reader_refuses_a_non_h264_response(server):
    _Handler.body = b"hi"
    _Handler.content_type = "text/plain"
    reader = H264Reader(f"http://127.0.0.1:{server.server_address[1]}/v1/video.h264", PhoneAuth("secret"))
    assert reader.open() is False


def test_without_pyav_the_reader_never_opens(monkeypatch):
    monkeypatch.setattr(h264_reader, "av", None)
    assert h264_reader.available() is False
    assert H264Reader("http://127.0.0.1:1/v1/video.h264", PhoneAuth("t")).open() is False


class _Endless:
    """A peer that keeps sending bytes that never make a frame."""

    def read1(self, n):
        return bytes(n)

    def close(self):
        pass


def test_data_that_never_makes_a_frame_ends_the_stream(monkeypatch):
    monkeypatch.setattr(h264_reader, "_MAX_BYTES_WITHOUT_FRAME", 1024 * 1024)
    reader = H264Reader("http://127.0.0.1:1/v1/video.h264", PhoneAuth("t"))
    reader._codec = h264_reader.new_decoder()
    reader._response = _Endless()
    assert reader.read() == (False, None)
