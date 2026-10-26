import ctypes
import logging

import numpy as np
import pytest

from telescope import audio, denoise
from telescope.audio import AudioWorker
from telescope.denoise import FRAME, Denoiser
from telescope.pinned_https import PhoneAuth

CHUNK_SAMPLES = audio.CHUNK // 2


class _EchoLib:
    """Stands in for RNNoise: hands each frame back unchanged and keeps a copy of what it was given."""

    def __init__(self):
        self.frames = []
        self.destroyed = 0

    def rnnoise_create(self, _model):
        return 1

    def rnnoise_destroy(self, _state):
        self.destroyed += 1

    def rnnoise_process_frame(self, _state, out, inp):
        frame = np.ctypeslib.as_array((ctypes.c_float * FRAME).from_address(inp)).copy()
        self.frames.append(frame)
        ctypes.memmove(out, frame.ctypes.data, frame.nbytes)
        return 0.0


class _Lagging:
    """A denoiser that scales everything by gain and, like RNNoise, hands it back 20 ms late."""

    def __init__(self, gain=1.0):
        self.gain = gain
        self.seen = []
        self.closed = False
        self._lag = np.zeros(denoise.DELAY, np.float32)

    def process(self, samples):
        self.seen.append(samples.copy())
        both = np.concatenate((self._lag, samples))
        self._lag = both[samples.size:]
        return both[:samples.size] * self.gain

    def close(self):
        self.closed = True


def _worker(make_denoiser):
    return AudioWorker("http://phone/v1/audio", PhoneAuth("tok"), lambda: None, lambda k, t: None,
                       make_denoiser=make_denoiser)


def _pcm(samples) -> bytes:
    return np.asarray(np.rint(samples), np.int16).tobytes()


def _ramp(n):
    return np.arange(1, n + 1, dtype=np.float32)


def _shape_all(w, samples):
    out = [np.frombuffer(w._shape(_pcm(samples[i:i + CHUNK_SAMPLES])), np.int16)
           for i in range(0, len(samples) - CHUNK_SAMPLES + 1, CHUNK_SAMPLES)]
    return np.concatenate(out).astype(np.float32)


def test_the_stream_is_already_what_rnnoise_takes_so_nothing_is_resampled_or_held_back():
    # Phones and the Browser camera both send 48 kHz mono, and the writer's 10 ms chunk is exactly one frame
    assert audio.RATE == denoise.RATE == 48_000
    assert CHUNK_SAMPLES == FRAME == 480


def test_whole_frames_go_straight_through_with_no_delay():
    lib = _EchoLib()
    d = Denoiser(lib)
    x = _ramp(FRAME * 6)
    out = np.concatenate([d.process(x[i:i + FRAME * 2]) for i in range(0, x.size, FRAME * 2)])
    assert np.array_equal(out, x)
    assert len(lib.frames) == 6 and all(f.size == FRAME for f in lib.frames)
    assert np.array_equal(np.concatenate(lib.frames), x)  # in order, none skipped or repeated


def test_uneven_chunks_come_out_in_order_just_under_a_frame_late():
    lib = _EchoLib()
    d = Denoiser(lib)
    x = _ramp(FRAME * 20)
    sizes = [300, 333, 7, 480, 1000, 1, 479, 960]
    out, at = [], 0
    while at < x.size:
        n = min(sizes[len(out) % len(sizes)], x.size - at)
        piece = d.process(x[at:at + n])
        assert piece.size == n  # always as much out as went in
        out.append(piece)
        at += n
    out = np.concatenate(out)
    assert np.array_equal(out, np.concatenate((np.zeros(FRAME - 1, np.float32), x))[:x.size])
    assert np.array_equal(np.concatenate(lib.frames), x[:len(lib.frames) * FRAME])


def test_once_chunks_stop_lining_up_it_shifts_once_and_never_again():
    d = Denoiser(_EchoLib())
    x = _ramp(FRAME * 10)
    first = d.process(x[:FRAME * 2])
    assert np.array_equal(first, x[:FRAME * 2])
    rest = [d.process(x[i:i + 250]) for i in range(FRAME * 2, x.size, 250)]
    out = np.concatenate([first] + rest)
    expected = np.concatenate((x[:FRAME * 2], np.zeros(FRAME - 1, np.float32), x[FRAME * 2:]))[:x.size]
    assert np.array_equal(out, expected)


def test_close_frees_the_state_once():
    lib = _EchoLib()
    d = Denoiser(lib)
    d.close()
    d.close()
    assert lib.destroyed == 1


def test_a_missing_library_says_the_package_isnt_installed(tmp_path):
    lib, problem = denoise.open_library([str(tmp_path / "librnnoise.so")])
    assert lib is None and problem == ("missing", "")


def test_a_library_that_wont_load_says_why(tmp_path):
    broken = tmp_path / "librnnoise.so"
    broken.write_bytes(b"not a library")
    lib, problem = denoise.open_library([str(broken)])
    assert lib is None and problem[0] == "failed" and problem[1]


def test_problem_explains_each_case(monkeypatch):
    monkeypatch.setattr(denoise, "_loaded", True)
    monkeypatch.setattr(denoise, "_problem", ("missing", ""))
    assert "pyrnnoise" in denoise.problem()
    monkeypatch.setattr(denoise, "_problem", ("failed", "bad ELF"))
    assert "bad ELF" in denoise.problem()
    monkeypatch.setattr(denoise, "_problem", None)
    assert denoise.problem() == ""


def test_without_the_library_switching_it_on_leaves_the_audio_as_it_was():
    w = _worker(lambda: None)
    w.noise_reduction = True
    chunk = _pcm(np.full(CHUNK_SAMPLES, 1000))
    assert w._shape(chunk) == chunk


def test_a_denoiser_that_fails_to_start_is_logged_once_and_the_audio_carries_on(caplog):
    calls = []

    def broken():
        calls.append(1)
        raise OSError("no")
    w = _worker(broken)
    w.noise_reduction = True
    chunk = _pcm(np.full(CHUNK_SAMPLES, 1000))
    with caplog.at_level(logging.ERROR):
        assert w._shape(chunk) == chunk
        assert w._shape(chunk) == chunk
    assert len(calls) == 1 and "noise reduction" in caplog.text


def _made(gain=1.0):
    made = []

    def make():
        made.append(_Lagging(gain))
        return made[-1]
    return made, make


def test_switching_in_a_pause_fades_over_one_chunk_each_way_and_lets_go_of_rnnoise():
    made, make = _made(0.5)
    w = _worker(make)
    w.limit = False
    hush = np.full(CHUNK_SAMPLES * 3, 200.0)  # the room between words
    assert np.array_equal(_shape_all(w, hush), hush)  # off: untouched
    w.noise_reduction = True
    fading = _shape_all(w, hush[:CHUNK_SAMPLES])
    assert fading[0] == 200 and fading[-1] == pytest.approx(100, abs=1)
    assert np.all(np.diff(fading) <= 0)  # smoothly, no jump
    assert np.array_equal(_shape_all(w, hush[:CHUNK_SAMPLES]), np.full(CHUNK_SAMPLES, 100.0))
    w.noise_reduction = False
    back = _shape_all(w, hush[:CHUNK_SAMPLES])
    assert back[0] == 100 and back[-1] == pytest.approx(200, abs=1)
    assert np.array_equal(_shape_all(w, hush[:CHUNK_SAMPLES]), hush[:CHUNK_SAMPLES])
    assert len(made) == 1 and made[0].closed


def _talk(segments, speech_chunks=30, pause_chunks=5):
    """Loud speech in which no two neighbouring samples match, with silent pauses between."""
    parts, at = [], 0
    for _ in range(segments):
        n = CHUNK_SAMPLES * speech_chunks
        parts += [1000 + (np.arange(at, at + n) % 20000), np.zeros(CHUNK_SAMPLES * pause_chunks)]
        at += n
    return np.concatenate(parts).astype(np.float32)


def test_switching_mid_sentence_waits_for_the_pause_so_no_speech_is_repeated_or_dropped():
    made, make = _made()
    w = _worker(make)
    w.limit = False
    x = _talk(6)
    switch = {10: True, 45: False, 80: True, 120: False}  # chunk: switched to, each in the middle of a sentence
    out = []
    for k in range(x.size // CHUNK_SAMPLES):
        if k in switch:
            w.noise_reduction = switch[k]
        if k in (31, 66, 101, 136):  # still in that sentence: nothing has changed yet
            assert w._wet == (0.0 if k in (31, 101) else 1.0)
        if k in (35, 70, 105, 140):  # the next sentence: it switched in the pause
            assert w._wet == (1.0 if k in (35, 105) else 0.0)
        out.append(np.frombuffer(w._shape(_pcm(x[k * CHUNK_SAMPLES:(k + 1) * CHUNK_SAMPLES])), np.int16))
    out = np.concatenate(out).astype(np.float32)
    assert np.array_equal(out[np.abs(out) > 500], x[x > 500])  # every sample of speech once, in order


def test_with_no_pause_it_switches_anyway_after_two_seconds():
    made, make = _made()
    w = _worker(make)
    w.limit = False
    x = _talk(1, speech_chunks=audio.NR_SWITCH_WAIT + 10, pause_chunks=0)
    w.noise_reduction = True
    for k in range(audio.NR_SWITCH_WAIT + 5):
        w._shape(_pcm(x[k * CHUNK_SAMPLES:(k + 1) * CHUNK_SAMPLES]))
        assert w._wet == (1.0 if k >= audio.NR_SWITCH_WAIT - 1 else 0.0)


def test_rnnoise_starts_on_the_audio_just_before_so_it_doesnt_open_with_silence():
    made, make = _made()
    w = _worker(make)
    w.limit = False
    for level in (100, 200, 300):
        w._shape(_pcm(np.full(CHUNK_SAMPLES, level)))
    w.noise_reduction = True
    out = np.frombuffer(w._shape(_pcm(np.full(CHUNK_SAMPLES, 400))), np.int16)
    assert [float(s[0]) for s in made[0].seen] == [200.0, 300.0, 400.0]  # its 20 ms lookahead, then the chunk
    assert out[-1] == 200  # faded into what it gave back for 20 ms ago, not into silence


def test_the_writer_lets_go_of_rnnoise_when_it_stops():
    made, make = _made()
    w = _worker(make)
    w.noise_reduction = True
    w._shape(_pcm(np.full(CHUNK_SAMPLES, 1000)))
    w._drop_denoiser()
    assert made[0].closed and w._denoiser is None


def test_noise_reduction_comes_before_gain():
    made, make = _made(0.5)
    w = _worker(make)
    w.limit = False
    w.noise_reduction = True
    w._wet = 1.0  # already switched in
    w.gain = w._gain = 2.0  # already there, no fade
    for _ in range(3):
        out = np.frombuffer(w._shape(_pcm(np.full(CHUNK_SAMPLES, 3000))), np.int16)
    assert made[0].seen[-1].max() == 3000  # it hears the mic as it comes
    assert np.all(out == 3000)  # halved, then doubled


# The rest needs the real library (pip install --no-deps -r requirements-nodeps.txt)
real = pytest.mark.skipif(denoise.load() is None, reason="RNNoise isn't installed")


def _speech(seconds, rng):
    """A voice-like sound: a gliding pitch with its harmonics, in syllables, with a pause every half second."""
    n = int(seconds * audio.RATE)
    t = np.arange(n) / audio.RATE
    f0 = 140 + 40 * np.sin(2 * np.pi * 0.7 * t)
    phase = 2 * np.pi * np.cumsum(f0) / audio.RATE
    voice = sum(np.sin(k * phase) / k for k in range(1, 20))
    syllables = np.clip(np.sin(2 * np.pi * 4 * t), 0, None) ** 0.5
    talking = (t % 0.5) < 0.35
    return voice * syllables * talking * 7000, talking


def _room(n, rng):
    """A fan's broad hum and hiss with some key clicks on top."""
    hiss = np.convolve(rng.standard_normal(n), np.ones(6) / 6, "same") * 500
    hum = np.sin(2 * np.pi * 100 * np.arange(n) / audio.RATE) * 150
    clicks = np.zeros(n)
    for at in rng.integers(0, n - 200, n // 9600):
        clicks[at:at + 120] += rng.standard_normal(120) * 2500 * np.exp(-np.arange(120) / 25)
    return hiss + hum + clicks


@real
def test_real_rnnoise_takes_the_room_out_and_keeps_the_voice():
    rng = np.random.default_rng(7)
    voice, talking = _speech(6, rng)
    room = _room(voice.size, rng)
    w = _worker(denoise.make)
    w.limit = False
    w.noise_reduction = True
    out = _shape_all(w, voice + room)
    late = np.arange(out.size) >= 2 * audio.RATE  # give it a couple of seconds to learn the room
    lag = denoise.DELAY
    pauses = np.roll(~talking, lag)[:out.size] & np.roll(~talking, lag + 1200)[:out.size] & late
    speech = np.roll(talking, lag)[:out.size] & late

    def db(x):
        return 10 * np.log10(np.mean(x * x))
    noisy = (voice + room)[:out.size]
    assert db(out[pauses]) < db(noisy[pauses]) - 15  # the room drops by well over 15 dB between words
    assert db(out[speech]) > db(noisy[speech]) - 6  # and the voice stays


@real
def test_real_rnnoise_keeps_up_with_10_ms_chunks_easily(timing):
    w = _worker(denoise.make)
    w.noise_reduction = w.auto_level = True
    rng = np.random.default_rng(3)
    chunks = iter([_pcm(rng.standard_normal(CHUNK_SAMPLES) * 3000) for _ in range(300)])
    # A 10 ms chunk is about 0.6 pieces of reference work (0.2 ms on a desktop); five times that is a real slowdown
    assert timing.relative(lambda: w._shape(next(chunks)), runs=100) < 3
