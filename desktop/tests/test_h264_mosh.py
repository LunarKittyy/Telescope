import fractions

import numpy as np
import pytest

av = pytest.importorskip("av")

from telescope import h264_mosh  # noqa: E402
from telescope.h264_mosh import Mosh, Mosher  # noqa: E402

_AUD = b"\x00\x00\x00\x01\x09\xf0"
_W, _H = 96, 64


def _picture(i):
    """A grey square that moves right, over a picture that changes completely every 6 frames."""
    img = np.full((_H, _W, 3), 40 + 60 * (i // 6 % 3), np.uint8)
    x = 4 + 3 * (i % 20)
    img[20:44, x:x + 24] = 230
    return img


def _units(count, gop=6, profile="high"):
    enc = av.CodecContext.create("libx264", "w")
    enc.width, enc.height, enc.pix_fmt = _W, _H, "yuv420p"
    enc.time_base = fractions.Fraction(1, 30)
    enc.options = {"tune": "zerolatency", "g": str(gop), "bf": "0", "profile": profile,
                   "x264-params": "scenecut=0"}
    out = []
    for i in range(count):
        frame = av.VideoFrame.from_ndarray(_picture(i), format="bgr24")
        frame.pts = i
        out += [bytes(p) for p in enc.encode(frame)]
    return out


def _decode(units, step=None):
    dec = av.CodecContext.create("h264", "r")
    pictures = []
    for unit in units:
        for u in (step(unit) if step else [unit]):
            for packet in dec.parse(u + _AUD):
                pictures += [f.to_ndarray(format="gray") for f in dec.decode(packet)]
    return pictures


def _is_keyframe(unit):
    return any(unit[s] & 31 == 5 for s, _e in h264_mosh._nals(unit))


def test_dropped_keyframes_smear_the_old_picture_instead_of_freezing():
    units = _units(30)
    clean = _decode(units)
    settings = Mosh()
    moshed = _decode(units, Mosher(lambda: settings))
    keyframes = sum(_is_keyframe(u) for u in units)
    assert len(moshed) == len(clean) - (keyframes - 1)  # every frame but the dropped keyframes still decodes
    after = moshed[6:]
    assert any(np.abs(a.astype(int) - c.astype(int)).mean() > 10 for a, c in zip(after, clean[7:]))
    assert len({a.tobytes() for a in after}) > len(after) // 2  # it moves, not a frozen picture


def test_clean_up_lets_the_next_keyframe_through_and_the_picture_comes_back():
    units = _units(30)
    clean = _decode(units)
    settings = [Mosh()]
    mosher = Mosher(lambda: settings[0])

    def step(unit):
        if unit is units[15]:
            settings[0] = Mosh(clean=1)
        return mosher(unit)
    moshed = _decode(units, step)
    assert np.abs(moshed[14].astype(int) - clean[16].astype(int)).mean() > 10  # keyframes 6 and 12 dropped
    assert np.abs(moshed[18].astype(int) - clean[20].astype(int)).mean() < 3  # 18 went through
    assert len(moshed) == len(clean) - 3  # and 24 is dropped again


def test_repeats_hand_back_more_frames_and_all_of_them_decode():
    units = _units(12, gop=100)
    settings = Mosh(drop_keyframes=False, repeats=2)
    moshed = _decode(units, Mosher(lambda: settings))
    assert len(moshed) == 1 + 3 * 11


def test_the_first_keyframe_always_goes_through():
    units = _units(8)
    settings = Mosh()
    out = Mosher(lambda: settings)(units[0])
    assert out == [units[0]]


def test_a_baseline_stream_moshes_too():
    units = _units(18, profile="baseline")
    settings = Mosh()
    moshed = _decode(units, Mosher(lambda: settings))
    assert len(moshed) == 18 - 2


def test_escaping_round_trips():
    raw = bytes([0, 0, 1, 0, 0, 0, 0, 0, 3, 5, 0, 0, 2])
    assert h264_mosh._unescape(h264_mosh._escape(raw)) == raw
    assert b"\x00\x00\x01" not in h264_mosh._escape(raw)
