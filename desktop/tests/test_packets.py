import fractions
import logging

import numpy as np
import pytest

av = pytest.importorskip("av")

from telescope import h264_reader, packets  # noqa: E402
from telescope.h264_reader import decode_counted, new_decoder  # noqa: E402


_AUD = b"\x00\x00\x00\x01\x09\xf0"


def _stream(values, size=(64, 48), gop=5):
    """Annex-B H.264 of flat grey frames, no B-frames and a delimiter after each, like the phone sends."""
    w, h = size
    enc = av.CodecContext.create("libx264", "w")
    enc.width, enc.height, enc.pix_fmt = w, h, "yuv420p"
    enc.time_base = fractions.Fraction(1, 30)
    enc.options = {"tune": "zerolatency", "g": str(gop), "bf": "0"}
    out = b""
    for i, v in enumerate(values):
        frame = av.VideoFrame.from_ndarray(np.full((h, w, 3), v, dtype=np.uint8), format="bgr24")
        frame.pts = i
        out += b"".join(bytes(p) + _AUD for p in enc.encode(frame))
    return out


def _tap(make):
    tap = packets.Tap()
    tap.set({"test": make})
    return tap


def test_nothing_extra_runs_while_no_plugin_changes_packets(monkeypatch):
    made = []
    monkeypatch.setattr(h264_reader._av(), "Packet", lambda *a: made.append(a))  # what a step's units are wrapped in
    dec = new_decoder(packets.Tap())
    frame, count = decode_counted(dec, _stream([50] * 8))
    assert frame is not None and count == 8
    assert made == []


def test_a_step_sees_every_access_unit_and_what_it_hands_back_is_decoded():
    seen = []

    def make():
        def step(unit):
            seen.append(unit)
            return [unit, unit] if len(seen) == 3 else [unit]  # the third twice
        return step
    dec = new_decoder(_tap(make))
    _frame, count = decode_counted(dec, _stream([50] * 6))
    assert len(seen) == 6 and all(u.startswith(b"\x00\x00") for u in seen)
    assert count == 7


def test_a_step_can_drop_a_frame():
    dec = new_decoder(_tap(lambda: lambda unit: [] if b"\x00\x00\x01\x41" in unit[:64] and len(unit) % 2 else [unit]))
    _frame, count = decode_counted(dec, _stream([50] * 10))
    assert count < 10


def test_a_failing_step_passes_units_through_and_is_left_out_after_a_while(caplog):
    calls = []

    def step(unit):
        calls.append(1)
        raise RuntimeError("broken")
    run = packets.guarded("fun", step)
    with caplog.at_level(logging.ERROR):
        for _ in range(packets.FAILS_BEFORE_SKIP + 10):
            assert run(b"abc") == [b"abc"]
    assert len(calls) == packets.FAILS_BEFORE_SKIP
    assert "failed on a packet" in caplog.text


def test_a_step_that_hands_back_the_wrong_thing_is_ignored():
    assert packets.guarded("fun", lambda unit: unit)(b"abc") == [b"abc"]
    assert packets.guarded("fun", lambda unit: [b"x", bytearray(b"y")])(b"abc") == [b"x", b"y"]


def test_steps_run_one_after_the_other():
    tap = packets.Tap()
    tap.set({"a": lambda: lambda u: [u, u], "b": lambda: lambda u: [u + b"!"]})
    assert tap.step(b"x") == [b"x!", b"x!"]


def test_each_stream_gets_a_fresh_step_and_taking_one_off_says_so():
    made = []

    def make():
        made.append(1)
        return lambda u: [u]
    one, two = packets.Tap(), packets.Tap()
    assert not one.set({"fun": make})
    assert not one.set({"fun": make})  # already on: it keeps its step
    assert not two.set({"fun": make})
    assert len(made) == 2
    assert one.set({}) and one.step is None
    assert not one.set({})


def test_both_readers_hand_their_tap_to_the_decoder(monkeypatch):
    from telescope.udp_reader import UdpH264Reader
    tap = packets.Tap()
    taken = []
    monkeypatch.setattr(h264_reader, "new_decoder", lambda t=None: taken.append(t))
    reader = UdpH264Reader("https://phone/v1/video.udp", None, tap=tap)
    assert reader.tap is tap
    reader._tcp = None
    tcp = h264_reader.H264Reader("https://phone/v1/video.h264", None, tap=tap)
    assert tcp.tap is tap


def test_the_stream_worker_gives_its_tap_to_light_readers_only():
    from telescope.stream import StreamWorker
    worker = StreamWorker("https://phone/v1/video.h264", None, None, 30)
    assert worker._new_reader("https://phone/v1/video.h264").tap is worker.packets
    assert worker._new_reader("https://phone/v1/video.udp").tap is worker.packets
    assert not hasattr(worker._new_reader("https://phone/v1/video.mjpeg"), "tap")
