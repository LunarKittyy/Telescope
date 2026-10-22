"""Browser camera server, end to end: a Python client stands in for the browser over TLS and a WebSocket, and _FakeRtc
for its WebRTC side (test_browser_rtc.py runs the real one)."""

import base64
import datetime
import json
import os
import socket
import ssl
import struct
import sys
import threading
import time
import urllib.error

import numpy as np
import pytest

import telescope.stream as stream
from telescope import audio
from telescope import browser_rtc
from telescope import browser_server as bs


@pytest.fixture(scope="module")
def cert(tmp_path_factory):
    return bs.ensure_certificate(tmp_path_factory.mktemp("cert"), ["127.0.0.1"])


@pytest.fixture
def server(cert):
    feed = bs.BrowserFeed()
    changes = []
    srv = bs.BrowserServer(feed, *cert, on_change=lambda: changes.append(1), port=0, host="127.0.0.1",
                           rtc_session=_FakeRtc)
    srv.changes = changes
    srv.start()
    yield srv
    srv.stop()


class _Browser:
    """Just enough of a browser's WebSocket: masked frames out, unmasked in."""

    def __init__(self, port: int, token: str, bid: str = ""):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.minimum_version = ssl.TLSVersion.TLSv1_2
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        raw = socket.create_connection(("127.0.0.1", port), timeout=5)
        self.sock = ctx.wrap_socket(raw)
        key = base64.b64encode(os.urandom(16)).decode()
        query = f"token={token}" + (f"&id={bid}" if bid else "")
        self.sock.sendall((f"GET /ws?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\n"
                           f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = self.sock.recv(1)
            if not chunk:
                break
            head += chunk
        self.status = int(head.split(b" ", 2)[1]) if head else 0
        self.accept = bs.ws_accept_key(key)
        self.head = head.decode("latin-1")
        self._buf = b""

    def send(self, opcode: int, payload: bytes, fin: bool = True):
        mask = os.urandom(4)
        n = len(payload)
        b0 = (0x80 if fin else 0) | opcode
        if n < 126:
            head = struct.pack("!BB", b0, 0x80 | n)
        elif n < 1 << 16:
            head = struct.pack("!BBH", b0, 0x80 | 126, n)
        else:
            head = struct.pack("!BBQ", b0, 0x80 | 127, n)
        self.sock.sendall(head + mask + bs.unmask(payload, mask))

    def _take(self, n):
        while len(self._buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError
            self._buf += chunk
        out, self._buf = self._buf[:n], self._buf[n:]
        return out

    def recv(self):
        b0, b1 = self._take(2)
        n = b1 & 0x7F
        if n == 126:
            n = struct.unpack("!H", self._take(2))[0]
        elif n == 127:
            n = struct.unpack("!Q", self._take(8))[0]
        return b0 & 0x0F, self._take(n)

    def recv_json(self):
        op, payload = self.recv()
        assert op == 0x1
        return json.loads(payload)

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


class _FakeRtc:
    """Stands in for browser_rtc.RtcSession: answers any offer, and the test hands frames and audio in through it."""

    made: list = []

    def __init__(self, on_video, on_audio):
        self._on_video, self._on_audio = on_video, on_audio
        self.codec = ""
        self.max_kbps = 0
        self.closed = False
        _FakeRtc.made.append(self)

    def answer(self, offer: str, max_kbps: int) -> str:
        self.codec, self.max_kbps = "H264", max_kbps
        return f"answer to {offer}"

    def on_video(self, frame, size: int = 0):
        if not self.closed:
            self._on_video(frame, size)

    def on_audio(self, pcm: bytes):
        if not self.closed:
            self._on_audio(pcm)

    def close(self):
        self.closed = True


class _FailingRtc(_FakeRtc):
    def answer(self, offer: str, max_kbps: int) -> str:
        raise browser_rtc.RtcError("no codec in common")


def _frame(w=64, h=48, value=200):
    """A decoded frame the way aiortc hands them over."""
    av = pytest.importorskip("av")
    return av.VideoFrame.from_ndarray(np.full((h, w, 3), value, np.uint8), format="bgr24").reformat(format="yuv420p")


def _next_close(b) -> tuple:
    """Skips what the server sends until its close frame."""
    while True:
        op, payload = b.recv()
        if op == 0x8:
            return op, payload


def _wait(cond, timeout=5.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if cond():
            return True
        time.sleep(0.01)
    return False


# ── Framing ──────────────────────────────────────────────────────────────────

def test_accept_key_matches_the_rfc_example():
    assert bs.ws_accept_key("dGhlIHNhbXBsZSBub25jZQ==") == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="


def test_unmask_is_its_own_inverse_at_any_length():
    mask = b"\x01\x02\x03\x04"
    for n in (0, 1, 3, 4, 5, 200, 70000):
        data = os.urandom(n)
        assert bs.unmask(bs.unmask(data, mask), mask) == data


def test_server_frames_use_the_right_length_encoding():
    assert bs.ws_frame(0x1, b"hi")[:2] == b"\x81\x02"
    assert bs.ws_frame(0x2, bytes(300))[:4] == b"\x82\x7e\x01\x2c"
    assert bs.ws_frame(0x2, bytes(70000))[:2] == b"\x82\x7f"


# ── Certificate ──────────────────────────────────────────────────────────────

def test_certificate_is_kept_until_it_nears_its_end(tmp_path):
    cert, key = bs.ensure_certificate(tmp_path, ["192.168.1.5"])
    first = cert.read_bytes()
    if sys.platform != "win32":
        assert key.stat().st_mode & 0o777 == 0o600
    assert bs.ensure_certificate(tmp_path, [])[0].read_bytes() == first  # a browser that trusted it keeps doing so

    later = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=380)
    assert bs.ensure_certificate(tmp_path, [], now=later)[0].read_bytes() != first


def test_an_unreadable_certificate_is_replaced(tmp_path):
    cert, _ = bs.ensure_certificate(tmp_path, [])
    cert.write_text("not a certificate")
    assert b"BEGIN CERTIFICATE" in bs.ensure_certificate(tmp_path, [])[0].read_bytes()


# ── Server ───────────────────────────────────────────────────────────────────

def _get(port: int, path: str) -> tuple:
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    import http.client
    conn = http.client.HTTPSConnection("127.0.0.1", port, context=ctx, timeout=5)
    conn.request("GET", path)
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, resp.getheader("Content-Type"), body


def test_serves_the_page_and_its_script(server):
    status, ctype, body = _get(server.port, "/")
    assert status == 200 and ctype.startswith("text/html") and b"/app.js" in body
    assert _get(server.port, "/app.js")[0] == 200
    assert _get(server.port, "/mic-worklet.js")[0] == 404  # the mic goes over WebRTC now
    assert _get(server.port, "/../browser_server.py")[0] == 404


def test_url_carries_the_token_after_the_hash(server):
    assert server.url_for("10.0.0.2") == f"https://10.0.0.2:{server.port}/#{server.token}"


def test_wrong_token_is_refused(server):
    b = _Browser(server.port, "nope")
    assert b.status == 403
    b.close()
    assert not server.feed.connected
    assert server.stats["refused"] == 1


def test_stats_count_each_step_a_browser_gets_to(server, caplog):
    caplog.set_level("INFO", logger="telescope.browser_server")
    assert server.stats.nothing_arrived() and server.stats.port == server.port
    _get(server.port, "/")
    raw = socket.create_connection(("127.0.0.1", server.port), timeout=5)
    raw.sendall(b"not a TLS handshake")  # what a browser that won't trust the certificate amounts to here
    raw.close()
    b = _Browser(server.port, server.token)
    b.recv_json()
    b.close()
    stats = server.stats
    assert _wait(lambda: stats["tls_failed"] == 1)
    assert stats["connections"] == 3 and stats["pages"] == 1 and stats["browsers"] == 1
    logged = [r.getMessage() for r in caplog.records]
    assert logged.count("Browser camera: a device reached this computer") == 1  # only the first time
    assert not any("127.0.0.1" in line or server.token in line for line in logged)


def test_a_taken_port_is_logged_and_shown(cert, caplog):
    first = bs.BrowserServer(bs.BrowserFeed(), *cert, port=0, host="127.0.0.1")
    first.start()
    try:
        second = bs.BrowserServer(bs.BrowserFeed(), *cert, port=first.port, host="127.0.0.1")
        second.start()
        assert second.port != first.port
        assert second.stats.fell_back and second.stats.port == second.port
        assert not first.stats.fell_back
        assert "was taken" in caplog.text
        second.stop()
    finally:
        first.stop()


def _offer(b, sdp="offer-sdp") -> dict:
    b.send(0x1, json.dumps({"type": "offer", "sdp": sdp}).encode())
    return b.recv_json()


def test_an_offer_is_answered_and_the_browsers_video_and_mic_come_out(server):
    feed = server.feed
    b = _Browser(server.port, server.token)
    assert b.status == 101
    assert f"Sec-WebSocket-Accept: {b.accept}" in b.head
    config = b.recv_json()
    assert config == {"type": "config", "width": 1280, "height": 720, "fps": 30, "audio": False,
                      "max_kbps": browser_rtc.max_kbps_for(1280, 720, 30)}

    b.send(0x1, json.dumps({"type": "hello", "device": "Safari on <iPhone>", "mic_error": ""}).encode())
    assert _wait(lambda: feed.device == "Safari on iPhone")

    assert _offer(b) == {"type": "answer", "sdp": "answer to offer-sdp"}
    rtc = _FakeRtc.made[-1]
    assert rtc.max_kbps == config["max_kbps"] and feed.codec == "H264"

    reader = bs.BrowserReader(feed)
    assert reader.open() and reader.isOpened()
    rtc.on_video(_frame(value=120), 2345)
    ok, frame = reader.read()
    assert ok and frame.shape == (48, 64, 3) and abs(int(frame.mean()) - 120) <= 3
    assert reader.last_frame_bytes == 2345

    # The mic asks for audio: the page hears it should send some, and it comes out of the stream
    mic = feed.open_audio()
    assert b.recv_json()["audio"] is True
    pcm = struct.pack("<4h", 1, -2, 300, -32768)
    rtc.on_audio(pcm)
    assert mic.read1(4096) == pcm
    mic.close()
    assert b.recv_json()["audio"] is False

    b.close()
    assert _wait(lambda: not feed.connected)
    assert _wait(lambda: rtc.closed)  # the browser left, so its WebRTC side ends too
    assert reader.read_packet() == (False, None)


def test_a_new_offer_replaces_the_connection_before_it(server):
    b = _Browser(server.port, server.token)
    b.recv_json()
    _offer(b, "first")
    first = _FakeRtc.made[-1]
    assert _offer(b, "second") == {"type": "answer", "sdp": "answer to second"}
    assert first.closed and not _FakeRtc.made[-1].closed
    first.on_video(_frame(), 10)  # the old one's frames are dropped by the stand-in, as the real one has stopped
    b.close()


def test_an_offer_that_cant_be_answered_tells_the_page_and_the_card(cert):
    srv = bs.BrowserServer(bs.BrowserFeed(), *cert, port=0, host="127.0.0.1", rtc_session=_FailingRtc)
    srv.start()
    try:
        b = _Browser(srv.port, srv.token)
        b.recv_json()
        reply = _offer(b)
        assert reply["type"] == "rtc_error" and reply["message"]
        assert srv.feed.rtc_error and srv.feed.codec == ""
        b.close()
    finally:
        srv.stop()


def test_the_pages_stats_reach_the_card_cleaned_up(server):
    b = _Browser(server.port, server.token)
    b.recv_json()
    before = len(server.changes)
    b.send(0x1, json.dumps({"type": "stats", "width": 1280, "height": 720, "fps": 29.6, "limit": "cpu"}).encode())
    assert _wait(lambda: server.feed.sent == {"width": 1280, "height": 720, "fps": 30, "limit": "cpu"})
    assert _wait(lambda: len(server.changes) > before)
    b.send(0x1, json.dumps({"type": "stats", "width": "<b>", "fps": -5, "limit": "everything"}).encode())
    assert _wait(lambda: server.feed.sent == {"limit": ""})
    b.close()


def test_fragmented_messages_and_pings_are_handled(server):
    b = _Browser(server.port, server.token)
    b.recv_json()
    b.send(0x9, b"hey")
    assert b.recv() == (0xA, b"hey")
    data = json.dumps({"type": "hello", "device": "Firefox on Android", "mic_error": ""}).encode()
    b.send(0x1, data[:10], fin=False)
    b.send(0x0, data[10:])
    assert _wait(lambda: server.feed.device == "Firefox on Android")
    b.send(0x2, b"\x01old page media")  # a page from before WebRTC: ignored, not an error
    b.send(0x1, json.dumps({"type": "hello", "device": "still here", "mic_error": ""}).encode())
    assert _wait(lambda: server.feed.device == "still here")
    b.close()


def test_a_quiet_page_that_answers_pings_stays_and_one_that_doesnt_goes(server, monkeypatch):
    monkeypatch.setattr(bs, "_PING_S", 0.1)
    monkeypatch.setattr(bs, "_IDLE_TIMEOUT_S", 0.6)
    b = _Browser(server.port, server.token)
    b.recv_json()
    deadline = time.monotonic() + 1.5
    pings = 0
    while time.monotonic() < deadline:
        op, payload = b.recv()
        if op == 0x9:
            pings += 1
            b.send(0xA, payload)  # what a browser does by itself
    assert pings >= 3 and server.feed.connected
    assert _wait(lambda: not server.feed.connected, timeout=3)  # nothing answers now
    b.close()


def test_a_newer_browser_takes_over_and_the_older_is_told(server):
    first = _Browser(server.port, server.token)
    first.recv_json()
    second = _Browser(server.port, server.token)
    second.recv_json()
    op, payload = first.recv()
    assert op == 0x8 and struct.unpack("!H", payload[:2])[0] == 4000
    assert server.feed.connected
    second.close()
    first.close()


@pytest.fixture
def hub_server(cert):
    joined = []
    hub = bs.BrowserHub(can_join=lambda bid: joined.append(bid) or len(joined) <= 2)
    srv = bs.BrowserServer(hub, *cert, port=0, host="127.0.0.1", rtc_session=_FakeRtc)
    srv.start()
    yield srv, hub
    srv.stop()


def test_each_browser_gets_a_feed_of_its_own(hub_server):
    server, hub = hub_server
    a = _Browser(server.port, server.token, "pixel-aaaaaaaa")
    a.recv_json()
    b = _Browser(server.port, server.token, "iphone-bbbbbbbb")
    b.recv_json()
    feeds = hub.feeds()
    assert set(feeds) == {"pixel-aaaaaaaa", "iphone-bbbbbbbb"} and all(f.connected for f in feeds.values())
    _offer(a)
    _FakeRtc.made[-1].on_video(_frame(), 10)
    assert _wait(lambda: feeds["pixel-aaaaaaaa"]._frame is not None)
    assert feeds["iphone-bbbbbbbb"]._frame is None

    again = _Browser(server.port, server.token, "pixel-aaaaaaaa")  # a reload: the same browser, not a third
    again.recv_json()
    op, payload = a.recv()
    assert op == 0x8 and struct.unpack("!H", payload[:2])[0] == bs.CLOSE_REPLACED
    assert len(hub.feeds()) == 2
    for x in (a, b, again):
        x.close()


def test_a_browser_with_no_room_is_told_every_camera_is_taken(hub_server):
    server, hub = hub_server
    kept = [_Browser(server.port, server.token, f"browser-{n}0000000") for n in range(2)]
    for k in kept:
        k.recv_json()
    late = _Browser(server.port, server.token, "browser-20000000")
    op, payload = late.recv()
    assert op == 0x8 and struct.unpack("!H", payload[:2])[0] == bs.CLOSE_FULL
    assert "browser-20000000" not in hub.feeds()
    for x in (*kept, late):
        x.close()


def test_the_page_hears_which_camera_it_streams_to(hub_server):
    server, hub = hub_server
    b = _Browser(server.port, server.token, "pixel-aaaaaaaa")
    assert "camera" not in b.recv_json()
    hub.feed("pixel-aaaaaaaa").set_camera("Phone Camera 2")
    assert b.recv_json()["camera"] == "Phone Camera 2"
    b.close()
    assert _wait(lambda: not hub.feed("pixel-aaaaaaaa").connected)
    hub.drop("pixel-aaaaaaaa")
    assert hub.feeds() == {}


def test_a_new_link_drops_the_browser_and_the_old_token(server):
    b = _Browser(server.port, server.token)
    b.recv_json()
    old = server.token
    server.new_token()
    assert _wait(lambda: not server.feed.connected)
    op, payload = _next_close(b)  # ended by its own thread, with the code that tells the page to stop
    assert struct.unpack("!H", payload[:2])[0] == bs.CLOSE_NEW_LINK
    assert _Browser(server.port, old).status == 403
    b.close()


def test_capture_changes_reach_a_connected_page(server):
    b = _Browser(server.port, server.token)
    b.recv_json()
    server.feed.set_capture(1920, 1080, 24)
    assert b.recv_json() == {"type": "config", "width": 1920, "height": 1080, "fps": 24, "audio": False,
                             "max_kbps": browser_rtc.max_kbps_for(1920, 1080, 24)}
    b.close()


def test_mic_is_refused_with_the_browsers_reason(server):
    b = _Browser(server.port, server.token)
    b.recv_json()
    b.send(0x1, json.dumps({"type": "hello", "device": "x", "mic_error": "The browser didn't allow the microphone."}).encode())
    assert _wait(lambda: server.feed.mic_error)
    with pytest.raises(urllib.error.HTTPError) as err:
        server.feed.open_audio()
    assert audio._phone_reason(err.value) == "The browser didn't allow the microphone."
    b.close()


def test_stop_ends_a_connected_browser(cert):
    srv = bs.BrowserServer(bs.BrowserFeed(), *cert, port=0, host="127.0.0.1", rtc_session=_FakeRtc)
    srv.start()
    b = _Browser(srv.port, srv.token)
    b.recv_json()
    _offer(b)
    rtc = _FakeRtc.made[-1]
    srv.stop()
    assert _wait(lambda: not srv.feed.connected)
    op, payload = _next_close(b)
    assert struct.unpack("!H", payload[:2])[0] == 1001
    assert _wait(lambda: rtc.closed)
    b.close()


# ── Into the pipeline ────────────────────────────────────────────────────────

class _FakeCam:
    def __init__(self):
        self.sent = []
        self.device = "fake"

    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False

    def send(self, frame):
        self.sent.append(frame)


def test_stream_worker_sends_browser_frames_to_the_virtual_camera(server, monkeypatch):
    feed = server.feed
    b = _Browser(server.port, server.token)
    b.recv_json()
    _offer(b)
    rtc = _FakeRtc.made[-1]
    cam = _FakeCam()
    worker = stream.StreamWorker(url="browser:", width=None, height=None, fps=30,
                                 frame_pipeline=[lambda f: f[:, ::-1].copy()],
                                 open_reader=lambda: bs.BrowserReader(feed))
    monkeypatch.setattr(worker, "_open_vcam", lambda w, h: cam)
    stop = threading.Event()

    def pump():
        while not stop.is_set():
            rtc.on_video(_frame(value=90), 1000)
            time.sleep(0.02)

    pumper = threading.Thread(target=pump, daemon=True)
    pumper.start()
    runner = threading.Thread(target=worker.run, daemon=True)
    runner.start()
    try:
        assert _wait(lambda: len(cam.sent) >= 5)
    finally:
        worker.request_stop()
        runner.join(5)
        stop.set()
        pumper.join(2)
        b.close()
    assert cam.sent[-1].shape == (48, 64, 3)
    assert abs(int(cam.sent[-1].mean()) - 90) <= 3
    assert not runner.is_alive()


def test_audio_worker_plays_the_browsers_mic(server):
    feed = server.feed
    b = _Browser(server.port, server.token)
    b.recv_json()
    _offer(b)
    rtc = _FakeRtc.made[-1]
    written = []

    class Sink:
        def write(self, data):
            written.append(data)
            time.sleep(0.005)

        def close(self):
            pass

    ctrl = bs.BrowserControl(feed)
    statuses = []
    worker = audio.AudioWorker(f"{ctrl.base}/audio", ctrl.auth, Sink, lambda k, t: statuses.append((k, t)))
    worker.start()
    try:
        assert b.recv_json()["audio"] is True
        tone = (np.sin(np.arange(4800) / 10) * 10000).astype("<i2").tobytes()
        for i in range(0, len(tone), 1920):
            rtc.on_audio(tone[i:i + 1920])
        assert _wait(lambda: any(any(chunk) for chunk in written))
        assert ("ok", "") in statuses
    finally:
        worker.stop()
        b.close()


def test_reader_waits_for_a_browser_and_says_what_its_waiting_for():
    feed = bs.BrowserFeed()
    reader = bs.BrowserReader(feed)
    reader.OPEN_WAIT_S = 0.05
    assert not reader.open()
    assert "Browser camera" in reader.waiting_text


def test_the_reader_hands_over_only_the_newest_frame():
    feed = bs.BrowserFeed()
    gen = feed.connect()
    reader = bs.BrowserReader(feed)
    assert reader.open()
    for value in (10, 20, 30):
        feed.put_video(gen, _frame(value=value))
    ok, frame = reader.read()
    assert ok and abs(int(frame.mean()) - 30) <= 3
    reader.FRAME_WAIT_S = 0.05
    assert reader.read_packet() == (False, None)  # nothing newer
