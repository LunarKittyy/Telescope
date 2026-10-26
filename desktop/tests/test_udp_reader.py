import base64
import fractions
import http.server
import json
import os
import random
import socket
import struct
import threading
import time

import numpy as np
import pytest

av = pytest.importorskip("av")

from telescope import udp_reader  # noqa: E402
from telescope.pinned_https import PhoneAuth  # noqa: E402
from telescope.udp_reader import HEADER, Reassembler, Seal, UdpH264Reader  # noqa: E402

_AUD = b"\x00\x00\x00\x01\x09\xf0"
KEY = bytes(range(32))


def _frames(values, size=(64, 48), gop=5):
    """(Annex-B frame with a delimiter, is a keyframe) for flat grey frames, a keyframe every gop."""
    w, h = size
    enc = av.CodecContext.create("libx264", "w")
    enc.width, enc.height, enc.pix_fmt = w, h, "yuv420p"
    enc.time_base = fractions.Fraction(1, 30)
    enc.options = {"tune": "zerolatency", "g": str(gop), "bf": "0", "keyint_min": str(gop)}
    out = []
    for i, v in enumerate(values):
        noise = np.random.default_rng(i).integers(0, 40, (h, w, 3), dtype=np.uint8)  # so frames span several datagrams
        frame = av.VideoFrame.from_ndarray(np.clip(noise.astype(int) + v, 0, 255).astype(np.uint8), format="bgr24")
        frame.pts = i
        out += [(bytes(p) + _AUD, p.is_keyframe) for p in enc.encode(frame)]
    return out


def _fragments(seq, data, key, size):
    count = max(1, -(-len(data) // size))
    return [HEADER.pack(seq, i, count, 1 if key else 0) + data[i * size:(i + 1) * size] for i in range(count)]


def test_seals_exactly_like_the_phone():
    # The same vector is in the phone's UdpVideoTest.kt
    seal = Seal(KEY, udp_reader.TO_DESKTOP)
    for _ in range(5):
        seal.seal(b"")
    assert seal.seal(b"telescope").hex() == "00000000000000055e76b1e3a4fb9044e263c4c95177c93d06d25e7b83374ba48c"


def test_opens_once_and_nothing_forged_or_from_the_other_direction():
    phone, desktop = Seal(KEY, udp_reader.TO_DESKTOP), Seal(KEY, udp_reader.TO_DESKTOP)
    a, b = phone.seal(b"a"), phone.seal(b"b")
    assert desktop.open(b) == b"b" and desktop.open(a) == b"a"
    assert desktop.open(a) is None  # replayed
    assert desktop.open(b[:-1] + bytes([b[-1] ^ 1])) is None
    assert Seal(KEY, udp_reader.TO_PHONE).open(phone.seal(b"c")) is None
    assert Seal(bytes(32), udp_reader.TO_DESKTOP).open(phone.seal(b"d")) is None


def test_frames_come_out_whole_and_in_order_whatever_order_the_pieces_came_in():
    r = Reassembler()
    pieces = _fragments(0, b"K" * 25, True, 10) + _fragments(1, b"P" * 15, False, 10)
    for p in reversed(pieces):
        r.add(p)
    assert r.take() == (b"K" * 25, 1)
    assert r.take() == (b"P" * 15, 1)
    assert r.take() is None


def test_a_lost_piece_is_asked_for_and_the_frame_waits_for_it():
    r = Reassembler()
    for p in _fragments(0, b"K" * 10, True, 10):
        r.add(p)
    assert r.take() is not None
    one = _fragments(1, b"P" * 30, False, 10)
    for p in one[:1] + one[2:] + _fragments(2, b"Q" * 10, False, 10):
        r.add(p)
    assert r.take() is None  # 2 is here but 1 isn't whole
    assert r.chase(100.0) == ([], False)  # just noticed
    assert r.chase(100.0 + udp_reader.NACK_AFTER_S) == ([(1, [1])], False)
    assert r.chase(100.0 + udp_reader.NACK_AFTER_S + 0.001) == ([], False)  # not straight away again
    r.add(one[1])
    assert r.take() == (b"P" * 30, 1) and r.take() == (b"Q" * 10, 1)


def test_a_whole_frame_lost_is_asked_for_in_full():
    r = Reassembler()
    for p in _fragments(0, b"K", True, 10) + _fragments(2, b"Q", False, 10):
        r.add(p)
    r.take()
    r.chase(0.0)
    assert r.chase(udp_reader.NACK_AFTER_S) == ([(1, [])], False)


def test_a_frame_that_never_comes_freezes_until_a_keyframe():
    r = Reassembler()
    for p in _fragments(0, b"K", True, 10) + _fragments(2, b"Q", False, 10):
        r.add(p)
    r.take()
    r.chase(0.0)
    assert r.chase(udp_reader.GIVE_UP_S) == ([], True)
    assert r.dropped == 1
    for p in _fragments(3, b"R", False, 10):
        r.add(p)
    assert r.take() is None  # a P-frame after the gap would decode wrong
    for p in _fragments(4, b"K2", True, 10):
        r.add(p)
    assert r.take() == (b"K2", 3)  # 2 and 3 skipped
    assert r.chase(1000.0) == ([], False)


def test_a_partial_first_keyframe_is_repaired_without_asking_for_another():
    r = Reassembler()
    pieces = _fragments(0, b"keyframe" * 100, True, 200)
    for piece in pieces[:-1]:
        r.add(piece)
    assert r.chase(1.0) == ([], False)
    assert r.chase(1.1) == ([(0, [len(pieces) - 1])], False)
    r.add(pieces[-1])
    assert r.take() == (b"keyframe" * 100, 1)


def test_a_keyframe_can_replace_a_full_backlog():
    r = Reassembler()
    for seq in range(udp_reader.MAX_FRAMES):
        r.add(_fragments(seq, b"p", False, 10)[0])
    r.add(_fragments(udp_reader.MAX_FRAMES, b"key", True, 10)[0])
    assert r.take() is not None
    assert len(r.frames) == 0


@pytest.mark.parametrize("established", [False, True])
def test_a_large_keyframe_is_not_discarded_while_it_is_still_arriving(established):
    r = Reassembler()
    if established:
        r.add(_fragments(0, b"first", True, 10)[0])
        assert r.take() == (b"first", 1)
    seq = int(established)
    pieces = _fragments(seq, b"K" * 80, True, 10)
    for i, piece in enumerate(pieces):
        r.add(piece)
        _nacks, want_key = r.chase(i * 0.1)
        assert not want_key
    assert r.take() == (b"K" * 80, 1)


@pytest.mark.parametrize("established", [False, True])
def test_a_keyframe_that_stops_progressing_still_times_out(established):
    r = Reassembler()
    if established:
        r.add(_fragments(0, b"first", True, 10)[0])
        r.take()
    piece = _fragments(int(established), b"K" * 80, True, 10)[0]
    r.add(piece)
    r.chase(0.0)
    r.add(piece)  # a duplicate isn't progress
    assert r.chase(udp_reader.GIVE_UP_S + 0.01) == ([], True)


class _Phone(http.server.BaseHTTPRequestHandler):
    """/v1/video.udp: hands out a key and sends frames over UDP, losing some on the way; /v1/video.h264 over TCP."""
    frames: list = []
    loss = 0.0
    udp_refused = False
    udp_silent = False
    slow_start = 0.0  # seconds of datagrams that never make a whole frame before the real ones
    nacks = 0
    reports: list = []

    def do_GET(self):
        if self.headers.get("Authorization") != "Bearer secret":
            self.send_response(401)
            self.end_headers()
            return
        if self.path.startswith("/v1/video.h264"):
            self.send_response(200)
            self.send_header("Content-Type", "video/h264")
            self.end_headers()
            self.wfile.write(b"".join(f for f, _k in self.frames))
            return
        if self.udp_refused:
            self.send_response(409)
            self.end_headers()
            return
        port = int(self.path.split("port=")[1])
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.bind(("127.0.0.1", 0))
        out, back = Seal(KEY, udp_reader.TO_DESKTOP), Seal(KEY, udp_reader.TO_PHONE)
        self.send_response(200)
        self.send_header("Content-Type", "application/x-telescope-udp")
        self.end_headers()
        hello = {"key": base64.b64encode(KEY).decode(), "port": sock.getsockname()[1]}
        self.wfile.write(json.dumps(hello).encode() + b"\n")
        self.wfile.flush()
        if self.udp_silent:
            time.sleep(2)
            return
        sent, rnd, peer = {}, random.Random(5), ("127.0.0.1", port)

        def send(p, first):
            sealed = out.seal(p)  # lost on the way, like on a real link: its counter is gone too
            if not (first and rnd.random() < self.loss):
                sock.sendto(sealed, peer)

        sock.settimeout(0.002)
        end = time.monotonic() + self.slow_start
        while time.monotonic() < end:  # a keyframe too big for the link: pieces arrive, it never completes
            sock.sendto(out.seal(HEADER.pack(0, 0, 2, 1) + b"x"), peer)
            time.sleep(0.05)
        for seq, (frame, key) in enumerate(self.frames, start=1 if self.slow_start else 0):
            sent[seq] = _fragments(seq, frame, key, 200)
            for p in sent[seq]:
                send(p, True)
            end = time.monotonic() + 1 / 30
            while time.monotonic() < end:  # answer NACKs between frames, like the phone's feedback thread
                try:
                    plain = back.open(sock.recvfrom(2048)[0])
                except OSError:
                    continue
                if plain and plain[0] == udp_reader.REPORT:
                    _Phone.reports.append(udp_reader.REPORT_BODY.unpack(plain))
                if plain and plain[0] == udp_reader.NACK:
                    _Phone.nacks += 1
                    nseq = struct.unpack(">I", plain[1:5])[0]
                    idx = [struct.unpack(">H", plain[i:i + 2])[0] for i in range(5, len(plain), 2)]
                    for i in idx or range(len(sent[nseq])):
                        send(sent[nseq][i], False)
        time.sleep(0.5)

    def log_message(self, *args):
        pass


@pytest.fixture
def phone():
    _Phone.loss, _Phone.udp_refused, _Phone.udp_silent, _Phone.nacks, _Phone.reports = 0.0, False, False, 0, []
    _Phone.slow_start = 0.0
    udp_reader._UDP_FAILED.clear()
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Phone)
    thread = threading.Thread(target=srv.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{srv.server_address[1]}/v1/video.udp"
    srv.shutdown()
    thread.join(timeout=2)
    udp_reader._UDP_FAILED.clear()


def _read_all(reader, limit=200):
    means = []
    while len(means) < limit:
        ok, frame = reader.read()
        if not ok:
            break
        means.append(float(frame.mean()))
    reader.release()
    return means


def test_frames_arrive_over_udp_and_lost_datagrams_are_sent_again(phone, monkeypatch):
    monkeypatch.setattr(UdpH264Reader, "__init__", _short_timeout(UdpH264Reader.__init__))
    values = list(range(40, 200, 4))
    _Phone.frames, _Phone.loss = _frames(values), 0.1
    reader = UdpH264Reader(phone, PhoneAuth("secret"))
    assert reader.open() and not reader.fell_back
    means = _read_all(reader)
    assert len(means) >= len(values) - 5  # nearly every frame, lost pieces and all
    assert _Phone.nacks > 0
    assert all(b >= a - 3 for a, b in zip(means, means[1:]))  # in order, nothing decoded from a broken frame


def test_reports_say_how_much_got_through(phone, monkeypatch):
    monkeypatch.setattr(UdpH264Reader, "__init__", _short_timeout(UdpH264Reader.__init__))
    _Phone.frames, _Phone.loss = _frames(list(range(40, 200, 4))), 0.1
    reader = UdpH264Reader(phone, PhoneAuth("secret"))
    assert reader.open()
    _read_all(reader)
    assert len(_Phone.reports) >= 5  # about every 100 ms over the 1.3 s of video
    _kind, received, highest, taken, _clock, whole, since, gave_up = _Phone.reports[-1]
    assert 0.03 < 1 - received / (highest + 1) < 0.2  # the first sends lost, their resends arrived
    assert taken > 0 and whole > 30 and since < 1000 and gave_up == 0
    counts = [r[1] for r in _Phone.reports]
    assert counts == sorted(counts)  # they only grow, so a lost one costs nothing


def test_a_report_packs_what_the_reassembler_saw():
    r = Reassembler()
    assert udp_reader.report(0, -1, r, 1.0) == udp_reader.REPORT_BODY.pack(4, 0, 0, 0, 1000, 0xFFFFFFFF, 0xFFFF, 0)
    for p in _fragments(0, b"x" * 30, True, 10):
        r.add(p, 2.0)
    r.add(_fragments(0, b"x" * 30, True, 10)[1], 2.1)  # a duplicate isn't counted again
    r.add(_fragments(1, b"y" * 30, False, 10)[0], 2.2)  # not whole yet
    body = udp_reader.report(5, 6, r, 2.25)
    assert udp_reader.REPORT_BODY.unpack(body) == (4, 5, 6, 40, 2250, 0, 250, 0)
    big = udp_reader.report(2 ** 32 + 3, 2 ** 32 + 4, r, 2.0 ** 32 / 1000 + 100)
    assert udp_reader.REPORT_BODY.unpack(big)[1:3] == (3, 4)  # counts wrap at 32 bits


def test_a_phone_that_refuses_udp_streams_over_tcp(phone):
    _Phone.frames, _Phone.udp_refused = _frames([60, 120, 180]), True
    reader = UdpH264Reader(phone, PhoneAuth("secret"))
    assert reader.open() and reader.fell_back
    assert len(_read_all(reader)) >= 1


def test_datagrams_that_never_arrive_fall_back_to_tcp_and_stay_there(phone, monkeypatch):
    monkeypatch.setattr(udp_reader, "FIRST_FRAME_S", 0.3)
    _Phone.frames, _Phone.udp_silent = _frames([60, 120, 180]), True
    reader = UdpH264Reader(phone, PhoneAuth("secret"))
    assert reader.open() and not reader.fell_back
    assert len(_read_all(reader)) >= 1 and reader.fell_back
    assert "127.0.0.1" in udp_reader._UDP_FAILED
    again = UdpH264Reader(phone, PhoneAuth("secret"))
    assert again.open() and again.fell_back  # straight to TCP: no waiting on the firewall a second time
    again.release()


def test_datagrams_that_arrive_but_take_a_while_to_make_a_frame_stay_on_udp(phone, monkeypatch):
    monkeypatch.setattr(udp_reader, "FIRST_FRAME_S", 0.3)
    monkeypatch.setattr(UdpH264Reader, "__init__", _short_timeout(UdpH264Reader.__init__))
    _Phone.frames, _Phone.slow_start = _frames([60, 120, 180]), 1.5  # longer than FIRST_FRAME_S and the read timeout
    reader = UdpH264Reader(phone, PhoneAuth("secret"))
    assert reader.open()
    assert len(_read_all(reader)) >= 1 and not reader.fell_back


def _short_timeout(init):
    def wrapped(self, url, auth, timeout=1.0):
        init(self, url, auth, timeout)
    return wrapped
