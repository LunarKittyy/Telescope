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
    settings = Mosh()
    moshers = [Mosher(lambda: settings)]

    headers = b"".join(b"\x00\x00\x00\x01" + units[0][s:e] for s, e in h264_mosh._nals(units[0]) if units[0][s] & 31 in (7, 8))

    def step(unit):
        if unit is units[15]:
            moshers.append(Mosher(lambda: settings, clean_up=True))  # Clean up puts a new one on
            unit = headers + unit  # which the decoder hands the stream's SPS and PPS first
        return moshers[-1](unit)
    moshed = _decode(units, step)
    assert np.abs(moshed[14].astype(int) - clean[16].astype(int)).mean() > 10  # keyframes 6 and 12 dropped
    assert any(np.abs(m.astype(int) - clean[20].astype(int)).mean() < 3 for m in moshed[14:22])  # 18 went through
    assert np.abs(moshed[-1].astype(int) - clean[-1].astype(int)).mean() > 10  # and 24 is dropped again


def test_put_on_mid_stream_it_moshes_from_the_next_keyframe():
    units = _units(14)
    headers = b"".join(b"\x00\x00\x00\x01" + units[0][s:e] for s, e in h264_mosh._nals(units[0]) if units[0][s] & 31 in (7, 8))
    settings = Mosh()
    mosher = Mosher(lambda: settings)

    def step(unit):
        if unit is units[3]:
            return mosher(headers + unit)  # the decoder hands a new step the SPS and PPS first
        return mosher(unit) if units.index(unit) > 3 else [unit]
    assert len(_decode(units, step)) == len(_decode(units)) - 2  # keyframes 6 and 12


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


def _split(unit):
    """(SPS and PPS, the rest) of an access unit: the phone sends the first only as config, not with each keyframe."""
    nals = [(unit[s] & 31, b"\x00\x00\x00\x01" + unit[s:e]) for s, e in h264_mosh._nals(unit)]
    return b"".join(n for t, n in nals if t in (7, 8)), b"".join(n for t, n in nals if t not in (7, 8))


def test_a_new_decoder_on_the_same_stream_gets_its_keyframes_after_a_reconnect():
    from telescope import h264_reader, packets
    units = _units(60, gop=10, profile="baseline")
    config = _split(units[0])[0]
    body = [_split(u)[1] for u in units]
    settings = Mosh()
    tap = packets.Tap()
    tap.set({"fun_pack": lambda: Mosher(lambda: settings)})

    def run(us):
        dec = h264_reader.Decoder(tap)
        return sum(dec.feed((config if i == 0 else b"") + u + _AUD)[1] for i, u in enumerate(us))
    assert run(body[:25]) >= 20
    assert run(body[30:60]) >= 25  # the step from before had seen a frame and would drop this keyframe
