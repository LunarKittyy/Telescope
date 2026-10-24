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
    def do_GET(self):
        if self.headers.get("Authorization") != "Bearer secret":
            self.send_response(401)
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Type", self.server.content_type)
        self.end_headers()
        self.wfile.write(self.server.body)
        self.wfile.flush()

    def log_message(self, *args):
        pass


@pytest.fixture
def server():
    srv = http.server.HTTPServer(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=lambda: srv.serve_forever(poll_interval=0.01), daemon=True)
    srv.body = b""
    srv.content_type = "video/h264"
    thread.start()
    yield srv
    srv.shutdown()
    srv.server_close()
    thread.join(timeout=2)
    assert not thread.is_alive()


def test_reader_reads_authenticated_frames_then_reports_eof(server):
    server.body = _encode([30, 90, 150, 210])
    server.content_type = "video/h264"
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
    assert 0 < total <= len(server.body)
    assert reader.isOpened() is False


def test_a_burst_shows_its_newest_frame_but_counts_every_frame_that_arrived(server):
    # Frames that come in together (after a moment of backlog) are all decoded; only the newest is shown, to stay
    # live, but each counts as arrived, or the stream would look like it's falling behind.
    server.body = _encode([30, 60, 90, 120, 150, 180])
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
    server.body = b"hi"
    server.content_type = "text/plain"
    reader = H264Reader(f"http://127.0.0.1:{server.server_address[1]}/v1/video.h264", PhoneAuth("secret"))
    assert reader.open() is False


def test_without_pyav_the_reader_never_opens(monkeypatch):
    monkeypatch.setattr(h264_reader, "_av_failed", True)
    assert h264_reader.available() is False
    assert H264Reader("http://127.0.0.1:1/v1/video.h264", PhoneAuth("t")).open() is False


def test_pyav_is_imported_by_one_thread_at_a_time(monkeypatch):
    # The start-up preload and a stream's decoder importing PyAV together deadlocked on av.video.reformatter
    import builtins
    import time
    real_import = builtins.__import__
    inside = []
    overlapped = []

    def slow_import(name, *args, **kwargs):
        if name == "av":
            overlapped.append(bool(inside))
            inside.append(1)
            time.sleep(0.05)
            inside.pop()
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(h264_reader, "_av_module", None)
    monkeypatch.setattr(builtins, "__import__", slow_import)
    threads = [threading.Thread(target=h264_reader._av) for _ in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert overlapped == [False]
    assert h264_reader._av() is av


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


@pytest.fixture
def hardware(monkeypatch):
    """A computer whose graphics card decoder opens; the codec it gives is the test's to choose."""
    made = []
    monkeypatch.setattr(h264_reader, "_hw_wanted", True)
    monkeypatch.setattr(h264_reader, "_hw_failed", False)
    monkeypatch.setattr(h264_reader, "_hw_probed", True)
    monkeypatch.setattr(h264_reader, "_hw_found", "cuda")
    monkeypatch.setattr(h264_reader, "_open_hw", lambda device: made.pop(0) if made else None)
    return made


class _BrokenCodec:
    options = {}

    def parse(self, data):
        raise RuntimeError("the driver fell over")


def test_decoding_stays_in_software_unless_hardware_is_on(monkeypatch):
    monkeypatch.setattr(h264_reader, "_hw_wanted", False)
    assert new_decoder().hardware is None


def test_a_graphics_card_that_fails_hands_over_to_software_and_stays_out(hardware):
    hardware.append(_BrokenCodec())
    dec = new_decoder()
    assert dec.hardware == "cuda"
    packets = _packets([60, 120, 180])
    frame, count = h264_reader.decode_counted(dec, packets[0])
    assert frame is not None and count == 1  # retry the same input in software
    assert dec.hardware is None and h264_reader._hw_failed
    frame, count = h264_reader.decode_counted(dec, b"".join(packets))
    assert frame is not None and count == 3
    assert new_decoder().hardware is None  # not tried again this run
    assert not h264_reader.hardware_in_use()


def test_no_graphics_card_decoder_means_software(hardware):
    dec = new_decoder()  # _open_hw finds nothing to open
    assert dec.hardware is None
    assert decode_newest(dec, _encode([90])) is not None


def test_linux_tries_va_api_before_cuda_when_pyav_has_it(monkeypatch):
    import av.codec.hwaccel
    monkeypatch.setattr(h264_reader, "_HW_TYPES", ("vaapi", "cuda"))
    monkeypatch.setattr(av.codec.hwaccel, "hwdevices_available", lambda: ["cuda", "drm", "vaapi"])
    assert h264_reader._hw_types() == ["vaapi", "cuda"]
    monkeypatch.setattr(av.codec.hwaccel, "hwdevices_available", lambda: ["cuda", "drm"])  # PyAV's own wheel
    assert h264_reader._hw_types() == ["cuda"]


def test_failure_after_setup_recovers_at_an_idr_without_repeated_headers(hardware):
    import re

    class FailsLater:
        options = {}
        def __init__(self):
            self.codec = h264_reader._software_codec()
            self.calls = 0
        def parse(self, data):
            self.calls += 1
            if self.calls == 2:
                raise RuntimeError("driver failed after receiving setup")
            return self.codec.parse(data)
        def decode(self, packet):
            return self.codec.decode(packet)

    hardware.append(FailsLater())
    decoder = new_decoder()
    packets = _packets([60 + i for i in range(15)])
    assert decode_newest(decoder, packets[0]) is not None
    recovered = []
    for packet in packets[1:]:
        parts = re.split(b'(\x00\x00(?:\x00)?\x01)', packet)
        without_headers = b''.join(parts[i] + parts[i + 1] for i in range(1, len(parts), 2)
                                   if parts[i + 1] and parts[i + 1][0] & 31 not in (7, 8))
        recovered.append(decode_newest(decoder, without_headers))
    assert any(frame is not None for frame in recovered)
    assert decoder.hardware is None


def test_decoding_diagnostics_follow_the_live_decoder_not_the_next_preference(hardware):
    hardware.append(h264_reader._software_codec())
    decoder = new_decoder()
    h264_reader.set_hardware(False)
    assert decoder.hardware == "cuda"
    assert "cuda" in h264_reader.decoding_backends()
    assert h264_reader.hardware_in_use()


def test_a_hardware_decoder_refusing_the_stream_falls_back_too(hardware):
    class RefusesStream:
        options = {}
        def parse(self, data):
            return [None]
        def decode(self, packet):
            raise av.error.InvalidDataError(22, "unsupported hardware format")

    hardware.append(RefusesStream())
    decoder = new_decoder()
    frame, count = h264_reader.decode_counted(decoder, _encode([100]))
    assert frame is not None and count == 1
    assert decoder.hardware is None


def test_a_bad_packet_after_hardware_decoded_frames_is_skipped_not_a_fallback(hardware):
    class OneBadPacket:
        options = {}
        def __init__(self):
            self.codec = h264_reader._software_codec()
            self.bad = False
        def parse(self, data):
            return self.codec.parse(data)
        def decode(self, packet):
            if self.bad:
                self.bad = False
                raise av.error.InvalidDataError(22, "lost a reference frame")
            return self.codec.decode(packet)

    codec = OneBadPacket()
    hardware.append(codec)
    decoder = new_decoder()
    packets = _packets([60 + i for i in range(5)])
    assert decode_newest(decoder, packets[0]) is not None
    codec.bad = True
    decode_newest(decoder, packets[1])
    assert decoder.hardware == "cuda" and not h264_reader._hw_failed
    assert decode_newest(decoder, b"".join(packets[2:])) is not None
