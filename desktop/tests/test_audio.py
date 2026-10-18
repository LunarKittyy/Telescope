import io
import os
import json
import time
import urllib.error

import numpy as np
import pytest


from telescope.pinned_https import PhoneAuth
from telescope import audio
from telescope.audio import BYTES_PER_MS, AudioWorker, FifoSink, JitterBuffer
from telescope.platform import virtual_mic


def _pcm(samples) -> bytes:
    return np.asarray(np.rint(samples), np.int16).tobytes()


def _sine(n, start=0, freq=440.0, amp=10000):
    return amp * np.sin(2 * np.pi * freq * (np.arange(n) + start) / audio.RATE)


class _Clock:
    def __init__(self):
        self.t = 0.0

    def __call__(self):
        return self.t


def _steady(jb, clock, seconds, phone_speed=1.0, freq=440.0):
    """Phone pushes 10 ms chunks at phone_speed while the writer pops 10 ms; returns what came out and the fill each pop."""
    out, fills, sent, owed = [], [], 0, 0.0
    chunk = audio.CHUNK // 2
    for _ in range(int(seconds * 100)):
        clock.t += 0.01
        owed += chunk * phone_speed
        n = int(owed) - sent
        jb.push(_pcm(_sine(n, sent, freq)))
        sent += n
        out.append(np.frombuffer(jb.pop(audio.CHUNK), np.int16))
        fills.append(jb.buffered_ms())
    return np.concatenate(out).astype(np.float32), fills


def _biggest_step(samples):
    return float(np.abs(np.diff(samples)).max())


def test_jitter_buffer_waits_for_the_target_then_fades_in_and_plays_in_order():
    jb = JitterBuffer(target_ms=20, adaptive=False)
    jb.push(_pcm(np.full(10 * audio.RATE // 1000, 1000)))
    assert jb.pop(4) == bytes(4)  # still filling: silence
    jb.push(_pcm(np.full(10 * audio.RATE // 1000, 1000)))
    out = np.frombuffer(jb.pop(audio.CHUNK), np.int16)
    assert out[0] == 0 and out[audio.FADE // 2] < 1000  # fading in, not starting at full level
    assert (out[audio.FADE:] == 1000).all()
    assert 9 <= jb.buffered_ms() <= 10


def test_jitter_buffer_fades_out_when_it_runs_dry_and_refills():
    jb = JitterBuffer(target_ms=10, adaptive=False)
    jb.push(_pcm(np.full(15 * audio.RATE // 1000, 1000)))
    jb.pop(audio.CHUNK)
    out = np.frombuffer(jb.pop(audio.CHUNK), np.int16)  # only 5 ms left
    played = out[out != 0]
    assert played.size and played[-1] < 100  # faded down, not cut off at 1000
    assert (out[played.size:] == 0).all()
    assert jb.pop(4) == bytes(4)  # refilling again


def test_jitter_buffer_holds_the_target_when_the_phones_clock_runs_fast_or_slow():
    for speed in (1.001, 0.999):
        clock = _Clock()
        jb = JitterBuffer(target_ms=40, clock=clock, adaptive=False)
        out, fills = _steady(jb, clock, 120, phone_speed=speed)
        settled = fills[1000:]
        assert 20 <= min(settled) and max(settled) <= 40, speed  # never cut back, never ran dry (read just after a pop)
        assert _biggest_step(out[5000:]) < 650, speed  # a 440 Hz sine at 10000 moves at most ~575 a sample


def test_jitter_buffer_drift_correction_keeps_high_notes_sharp():
    clock = _Clock()
    jb = JitterBuffer(target_ms=40, clock=clock, adaptive=False)
    out, _ = _steady(jb, clock, 10, phone_speed=1.002, freq=8000)
    assert np.abs(out[audio.RATE * 5:]).max() > 9500  # straight-line interpolation would lose a lot of this


def test_jitter_buffer_fades_back_to_the_target_when_far_behind():
    clock = _Clock()
    jb = JitterBuffer(target_ms=20, max_ms=60, clock=clock, adaptive=False)
    out, fills = _steady(jb, clock, 1)
    jb.push(_pcm(_sine(200 * audio.RATE // 1000, 100 * audio.CHUNK // 2)))  # 200 ms arrive at once
    after = np.frombuffer(jb.pop(audio.CHUNK), np.int16).astype(np.float32)
    assert jb.buffered_ms() <= 25
    assert _biggest_step(np.concatenate((out[-10:], after))) < 1500  # crossfaded, not a jump


def test_jitter_buffer_grows_the_target_when_audio_arrives_unevenly_then_shrinks_again():
    clock = _Clock()
    jb = JitterBuffer(target_ms=20, clock=clock)
    _steady(jb, clock, 2)
    assert jb.target_ms() == 20
    clock.t += 0.08  # 80 ms with nothing, then it all arrives
    jb.push(_pcm(_sine(80 * audio.RATE // 1000)))
    assert jb.target_ms() >= 70  # 70 ms later than the rest, plus a margin
    _steady(jb, clock, 30)
    assert jb.target_ms() < 40


def test_audio_lost_on_the_way_doesnt_keep_the_target_up():
    clock = _Clock()
    jb = JitterBuffer(target_ms=20, clock=clock)
    _steady(jb, clock, 2)
    clock.t += 1.3  # the phone's mic reopened: 1.3 s of audio that never comes
    jb.push(_pcm(_sine(480)))
    assert jb.target_ms() == audio.MAX_TARGET_MS
    _steady(jb, clock, 30)
    assert jb.target_ms() < 30


def test_jitter_buffer_caps_what_piles_up_while_nothing_reads():
    jb = JitterBuffer(target_ms=20, adaptive=False)
    for _ in range(300):
        jb.push(_pcm(np.zeros(audio.CHUNK // 2)))
    assert jb.buffered_ms() <= 20 + 130 + audio.MAX_BACKLOG_MS


def test_usb_starts_with_a_shorter_buffer_than_wifi():
    assert audio._target_for("https://127.0.0.1:5000/v1/audio") == audio.USB_TARGET_MS
    assert audio._target_for("https://192.168.1.20:8443/v1/audio") == audio.WIFI_TARGET_MS
    assert audio._target_for("browser:/audio") == audio.WIFI_TARGET_MS


class _Resp:
    def __init__(self, data: bytes):
        self._data = io.BytesIO(data)
        self.closed = False

    def read1(self, n):
        return self._data.read(min(n, 480))

    def close(self):
        self.closed = True


class _Sink:
    def __init__(self):
        self.data = bytearray()
        self.closed = False

    def write(self, b):
        self.data += b
        time.sleep(0.002)

    def close(self):
        self.closed = True


def _worker(opener, sink):
    statuses = []
    w = AudioWorker("http://phone/v1/audio", PhoneAuth("tok"), lambda: sink, lambda k, t: statuses.append((k, t)),
                    opener=opener)
    return w, statuses


def _until(cond, timeout=3.0):
    end = time.time() + timeout
    while not cond() and time.time() < end:
        time.sleep(0.01)
    return cond()


def test_worker_plays_the_phone_audio_into_the_sink_with_the_token():
    pcm = b"\x10\x00" * (200 * BYTES_PER_MS // 2)
    seen = {}

    def opener(req, timeout):
        seen["auth"] = req.get_header("Authorization")
        return _Resp(pcm)

    sink = _Sink()
    w, statuses = _worker(opener, sink)
    w.start()
    assert _until(lambda: b"\x10\x00" * 8 in bytes(sink.data))
    w.stop()
    assert seen["auth"] == "Bearer tok"
    assert ("ok", "") in statuses
    assert sink.closed


def test_worker_reports_the_phones_reason_and_keeps_trying(monkeypatch):
    monkeypatch.setattr(audio, "RETRY_S", 0.05)
    calls = []

    def opener(req, timeout):
        calls.append(1)
        body = io.BytesIO(json.dumps({"error": "Allow the microphone in Telescope on the phone"}).encode())
        raise urllib.error.HTTPError(req.full_url, 403, "Forbidden", {}, body)

    w, statuses = _worker(opener, _Sink())
    w.start()
    assert _until(lambda: len(calls) >= 2)
    w.stop()
    assert statuses[0] == ("err", "Allow the microphone in Telescope on the phone")


def test_worker_reports_an_output_that_wont_open():
    def opener(req, timeout):
        return _Resp(b"")

    statuses = []

    def bad_sink():
        raise OSError("no reader")

    w = AudioWorker("http://p/v1/audio", PhoneAuth("t"), bad_sink, lambda k, t: statuses.append((k, t)), opener=opener)
    w.start()
    assert _until(lambda: any(k == "err" and "no reader" in t for k, t in statuses))
    w.stop()


def test_worker_reopens_an_output_that_stopped_taking_audio(monkeypatch):
    monkeypatch.setattr(audio, "RETRY_S", 0.05)

    class Dying(_Sink):
        def write(self, b):
            raise OSError("reader gone")

    sinks = [Dying(), _Sink()]
    opened = []

    def open_sink():
        opened.append(sinks[min(len(opened), 1)])
        return opened[-1]

    statuses = []
    w = AudioWorker("http://p/v1/audio", PhoneAuth("t"), open_sink, lambda k, t: statuses.append((k, t)),
                    opener=lambda req, timeout: _Resp(b"\x10\x00" * 4000))
    w.start()
    assert _until(lambda: len(sinks[1].data) > 0)
    w.stop()
    assert sinks[0].closed and ("err", "The virtual microphone stopped taking audio.") in statuses


# ── Virtual mic plumbing ──────────────────────────────────────────────────────

class _Pactl:
    def __init__(self, modules="", fail=False):
        self.modules = modules
        self.fail = fail
        self.calls = []
        self.next_id = 40

    def __call__(self, cmd, timeout=5):
        self.calls.append(cmd)
        if cmd[1:3] == ["list", "short"]:
            return True, self.modules
        if cmd[1] == "load-module":
            if self.fail:
                return False, "Module initialization failed"
            self.next_id += 1
            return True, str(self.next_id)
        return True, ""


def test_linux_setup_creates_an_input_only_source(tmp_path):
    pactl = _Pactl()
    fifo = str(tmp_path / "mic.fifo")
    ids, err = virtual_mic.linux_setup(pactl, fifo)
    assert err == "" and ids == [41]
    load = pactl.calls[1]
    assert load[2] == "module-pipe-source"
    assert "source_name=telescope_mic" in load and f"file={fifo}" in load
    assert {"format=s16le", "rate=48000", "channels=1"} <= set(load)
    assert "source_properties=\"device.description='Telescope Microphone'\"" in load
    assert not any("sink" in arg for arg in load)  # nothing that shows up as a speaker
    virtual_mic.linux_teardown(ids, pactl)
    assert pactl.calls[-1] == ["pactl", "unload-module", "41"]


def test_linux_setup_clears_what_an_earlier_run_left_loaded(tmp_path):
    modules = "\n".join([
        "7\tmodule-null-sink\tsink_name=telescope_mic_sink sink_properties=...",
        "8\tmodule-remap-source\tmaster=telescope_mic_sink.monitor source_name=telescope_mic",
        "9\tmodule-alsa-card\tdevice_id=0",
    ])
    pactl = _Pactl(modules)
    stale = tmp_path / "mic.fifo"
    stale.write_text("left over")
    ids, err = virtual_mic.linux_setup(pactl, str(stale))
    assert err == "" and ids == [41]
    assert ["pactl", "unload-module", "8"] in pactl.calls and ["pactl", "unload-module", "7"] in pactl.calls
    assert ["pactl", "unload-module", "9"] not in pactl.calls
    assert not stale.exists()  # the module makes a fresh FIFO


def test_linux_setup_reports_a_failed_load(tmp_path):
    pactl = _Pactl(fail=True)
    ids, err = virtual_mic.linux_setup(pactl, str(tmp_path / "f"))
    assert ids == [] and "Module initialization failed" in err
    loads = [c for c in pactl.calls if c[1] == "load-module"]
    assert len(loads) == 2 and not any("source_properties" in a for a in loads[1])  # retried plain


def test_linux_needs_only_pactl():
    assert virtual_mic.linux_tools_missing(lambda t: None) == ["pactl"]
    assert virtual_mic.linux_tools_missing(lambda t: "/usr/bin/" + t) == []


@pytest.mark.skipif(not hasattr(os, "mkfifo"), reason="Unix only")
def test_fifo_sink_writes_everything_at_real_time_pace():
    read_end, write_end = os.pipe()
    clock = [100.0]
    sleeps = []

    def sleep(s):
        sleeps.append(round(s, 4))
        clock[0] += s

    sink = FifoSink("unused", clock=lambda: clock[0], sleep=sleep, open_fd=lambda _path: write_end)
    ten_ms = bytes(960)
    for _ in range(3):
        sink.write(ten_ms)
    assert os.read(read_end, 10_000) == ten_ms * 3
    assert sleeps == [0.01, 0.01]  # the first write goes at once, then one every 10 ms
    clock[0] += 1.0  # a long stall: start over rather than burst to catch up
    sink.write(ten_ms)
    assert sleeps == [0.01, 0.01]
    sink.close()
    os.close(read_end)


def test_fifo_sink_fails_fast_when_the_source_is_gone(tmp_path):
    with pytest.raises(OSError):
        FifoSink(str(tmp_path / "missing"))
    if hasattr(os, "mkfifo"):  # a FIFO nobody reads: the open must fail rather than hang
        path = str(tmp_path / "f")
        os.mkfifo(path)
        with pytest.raises(OSError):
            FifoSink(path)


@pytest.mark.skipif(not hasattr(os, "mkfifo"), reason="Unix only")
def test_fifo_sink_refuses_a_file_or_a_link_in_place_of_the_pipe(tmp_path):
    target = tmp_path / "precious.txt"
    target.write_text("keep me")
    with pytest.raises(OSError):
        FifoSink(str(target))
    link = tmp_path / "mic.fifo"
    link.symlink_to(target)
    with pytest.raises(OSError):
        FifoSink(str(link))
    assert target.read_text() == "keep me"


@pytest.mark.skipif(not hasattr(os, "getuid"), reason="Unix only")
def test_without_a_runtime_dir_the_fifo_goes_in_a_private_folder(tmp_path, monkeypatch):
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    monkeypatch.setattr(virtual_mic.tempfile, "gettempdir", lambda: str(tmp_path))
    path = virtual_mic.fifo_path()
    folder = os.path.dirname(path)
    assert folder == str(tmp_path / f"telescope-{os.getuid()}")
    assert os.stat(folder).st_mode & 0o777 == 0o700


@pytest.mark.skipif(not hasattr(os, "getuid"), reason="Unix only")
def test_a_shared_or_linked_fallback_folder_is_refused(tmp_path, monkeypatch):
    monkeypatch.delenv("XDG_RUNTIME_DIR", raising=False)
    monkeypatch.setattr(virtual_mic.tempfile, "gettempdir", lambda: str(tmp_path))
    folder = tmp_path / f"telescope-{os.getuid()}"
    folder.mkdir(mode=0o777)
    os.chmod(folder, 0o777)
    with pytest.raises(OSError):
        virtual_mic.fifo_path()
    folder.rmdir()
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir(mode=0o700)
    folder.symlink_to(elsewhere)
    with pytest.raises(OSError):
        virtual_mic.fifo_path()
    ids, err = virtual_mic.linux_setup(_Pactl())
    assert ids == [] and "private folder" in err


def test_vb_cable_is_found_by_its_playback_end():
    devices = [{"name": "Speakers", "max_output_channels": 2},
               {"name": "CABLE Output (VB-Audio Virtual Cable)", "max_output_channels": 0},
               {"name": "CABLE Input (VB-Audio Virtual Cable)", "max_output_channels": 2}]
    assert virtual_mic.find_vb_cable(devices) == 2
    assert virtual_mic.find_vb_cable(devices[:2]) is None


def _chunk(value: int) -> bytes:
    return np.full(audio.CHUNK // 2, value, np.int16).tobytes()


def test_muting_fades_out_over_one_chunk_then_writes_silence():
    w, _ = _worker(None, _Sink())
    assert w._mute(_chunk(1000)) == _chunk(1000)
    w.muted = True
    faded = np.frombuffer(w._mute(_chunk(1000)), np.int16)
    assert faded[0] == 1000 and faded[-1] == 0 and np.all(np.diff(faded) <= 0)
    assert w._mute(_chunk(1000)) == bytes(audio.CHUNK)
    w.muted = False
    back = np.frombuffer(w._mute(_chunk(1000)), np.int16)
    assert back[0] == 0 and back[-1] == 1000
    assert w._mute(_chunk(1000)) == _chunk(1000)


def test_level_is_the_loudest_since_the_last_look_and_ignores_mute():
    w, _ = _worker(None, _Sink())
    w.muted = True
    w._measure(_chunk(16384))
    w._measure(_chunk(-32768))
    w._measure(_chunk(100))
    peak, rms, limited = w.take_level()
    assert peak == pytest.approx(audio.LIMIT_CEILING) and limited  # the limiter's ceiling, not a clip
    w.limit = False
    w._measure(_chunk(-32768))
    assert w.take_level()[:2] == (1.0, pytest.approx(1.0, abs=0.001))
    assert w.take_level() == (0.0, 0.0, False)


def test_gain_fades_to_its_new_level_and_clips_at_full_scale_without_the_limiter():
    w, _ = _worker(None, _Sink())
    w.limit = False
    w.gain = 4.0
    ramped = np.frombuffer(w._shape(_chunk(1000)), np.int16)
    assert ramped[0] == 1000 and ramped[-1] == 4000
    assert w._shape(_chunk(1000)) == _chunk(4000)
    assert w._shape(_chunk(20000)) == _chunk(32767)
    assert w._shape(_chunk(-20000)) == _chunk(-32768)
    w.gain = 1.0
    w._shape(_chunk(1000))
    assert w._shape(_chunk(1000)) == _chunk(1000)


def test_level_counts_the_gain_and_keeps_samples_whole_across_odd_reads():
    w, _ = _worker(None, _Sink())
    w.gain = 2.0
    data = _chunk(8000)
    w._measure(data[:3])
    w._measure(data[3:])
    peak, rms, _ = w.take_level()
    assert peak == pytest.approx(16000 / 32767) and rms == pytest.approx(peak)
    w.gain = 8.0
    w.limit = False
    w._measure(_chunk(8000))
    assert w.take_level()[0] == 1.0


def test_meter_keeps_moving_while_nothing_reads_the_mic():
    pcm = np.full(48_000, 16384, np.int16).tobytes()

    class _StuckSink(_Sink):
        def write(self, b):
            time.sleep(10)  # PipeWire suspended the mic and stopped reading the pipe

    w, _ = _worker(lambda req, timeout: _Resp(pcm), _StuckSink())
    seen = []
    w.start()
    assert _until(lambda: seen.append(w.take_level()[0]) or max(seen) > 0.4)  # each look resets the level
    w._stop.set()


def _wave(amp: float, n: int = audio.CHUNK // 2, phase: int = 0) -> bytes:
    t = np.arange(phase, phase + n)
    return (np.sin(2 * np.pi * 440 * t / audio.RATE) * amp).astype(np.int16).tobytes()


def test_limiter_holds_loud_peaks_under_the_ceiling_then_lets_go_slowly():
    w, _ = _worker(None, _Sink())
    w.gain = 4.0  # +12 dB on a wave at about -6 dBFS would clip hard
    ceiling = audio.LIMIT_CEILING * 32767
    w._shape(_wave(16000))
    outs = [np.frombuffer(w._shape(_wave(16000, phase=i * 480)), np.int16) for i in range(1, 20)]
    assert all(np.abs(o).max() <= ceiling * 10 ** (0.1 / 20) for o in outs)  # the 1 ms attack may overshoot a hair
    assert not any(np.abs(o).max() >= 32767 for o in outs)
    w._shape(_wave(1000))  # quiet again: the gain comes back over time, not at once
    assert w._limit_gain < 0.5
    for _ in range(200):
        w._shape(_wave(1000))
    assert w._limit_gain == 1.0


def test_quiet_audio_goes_through_untouched_with_the_limiter_on():
    w, _ = _worker(None, _Sink())
    quiet = _wave(3000)
    assert w._shape(quiet) == quiet


def test_remove_stale_source_unloads_only_ours():
    pactl = _Pactl("\n".join([
        "8\tmodule-pipe-source\tsource_name=telescope_mic file=/run/user/1000/telescope-mic.fifo",
        "10\tmodule-pipe-source\tsource_name=telescope_dev_mic file=/run/user/1000/telescope-dev-mic.fifo",
        "9\tmodule-alsa-card\tdevice_id=0",
    ]))
    virtual_mic.remove_stale_source(pactl)
    assert ["pactl", "unload-module", "8"] in pactl.calls
    assert not any(c[1] == "load-module" for c in pactl.calls)
    assert ["pactl", "unload-module", "9"] not in pactl.calls
    assert ["pactl", "unload-module", "10"] not in pactl.calls  # a running dev profile's source stays


def test_remove_stale_source_is_quiet_without_a_sound_server():
    calls = []

    def pactl(cmd):
        calls.append(cmd)
        return False, "No such file or directory"

    virtual_mic.remove_stale_source(pactl)
    assert calls == [["pactl", "list", "short", "modules"]]


# ── Automatic level ───────────────────────────────────────────────────────────

def _talk(seconds, db, pause_db=-70.0, seed=1):
    """Speech-ish: 300 ms of tone at db (RMS dBFS), then 100 ms of room noise at pause_db."""
    rng = np.random.default_rng(seed)
    n = int(seconds * audio.RATE)
    t = np.arange(n)
    tone = np.sin(2 * np.pi * 220 * t / audio.RATE) * np.sqrt(2) * audio.FULL_SCALE * 10 ** (db / 20)
    noise = rng.standard_normal(n) * audio.FULL_SCALE * 10 ** (pause_db / 20)
    talking = (t % (audio.RATE * 4 // 10)) < audio.RATE * 3 // 10
    return np.where(talking, tone, noise)


def _shape_all(w, samples):
    chunk = audio.CHUNK // 2
    out = [np.frombuffer(w._shape(_pcm(samples[i:i + chunk])), np.int16) for i in range(0, len(samples) - chunk + 1, chunk)]
    return np.concatenate(out).astype(np.float32)


def _rms_db(samples):
    return 20 * np.log10(np.sqrt(np.mean(samples * samples)) / audio.FULL_SCALE)


def _speech_db(samples):
    t = np.arange(samples.size)
    talking = (t % (audio.RATE * 4 // 10)) < audio.RATE * 3 // 10
    return _rms_db(samples[talking])


def _auto_worker():
    w, _ = _worker(lambda req, timeout: _Resp(b""), _Sink())
    w.auto_level = True
    return w


def test_automatic_level_brings_quiet_speech_up():
    w = _auto_worker()
    out = _shape_all(w, _talk(8, -34))
    assert abs(_speech_db(out[-audio.RATE * 2:]) - audio.AUTO_TARGET_DB) < 2


def test_automatic_level_brings_loud_speech_down():
    w = _auto_worker()
    out = _shape_all(w, _talk(4, -10))
    assert abs(_speech_db(out[-audio.RATE * 2:]) - audio.AUTO_TARGET_DB) < 2


def test_automatic_level_doesnt_turn_up_the_room_while_nobody_speaks():
    w = _auto_worker()
    _shape_all(w, _talk(6, -26))
    held = w._auto_db
    _shape_all(w, _talk(5, -48, pause_db=-45))  # only the room now, with a fan going
    assert w._auto_db <= held + 0.5


def test_automatic_level_changes_gain_without_clicks():
    w = _auto_worker()
    out = _shape_all(w, _talk(3, -10))
    tone_step = 2 * np.pi * 220 / audio.RATE * np.sqrt(2) * audio.FULL_SCALE * 10 ** (-10 / 20)
    assert _biggest_step(out) < tone_step * 1.2  # never a jump bigger than the loudest tone itself makes


def test_automatic_level_is_off_unless_asked_for():
    w, _ = _worker(lambda req, timeout: _Resp(b""), _Sink())
    quiet = _talk(1, -34)
    assert np.array_equal(_shape_all(w, quiet), np.rint(quiet[:len(quiet) // 480 * 480]).astype(np.int16).astype(np.float32))


def test_switching_automatic_level_off_fades_its_gain_out():
    w = _auto_worker()
    talk = _talk(6, -34)
    before = _shape_all(w, talk)
    assert w._auto_db > 10
    w.auto_level = False
    after = _shape_all(w, talk[:audio.RATE])
    assert w._auto_db == 0.0
    joined = np.concatenate((before[-audio.RATE:], after))
    assert _biggest_step(joined) <= _biggest_step(before[-audio.RATE:]) * 1.05  # no jump down to the plain level
    assert np.allclose(after[-audio.RATE // 2:], np.rint(talk[audio.RATE // 2:audio.RATE]), atol=1)


def _feed(jb, clock, seconds, make):
    """Pushes and pops 10 ms at a time; make(n, start) gives the phone's samples. Returns the output and the fills."""
    out, fills, sent = [], [], 0
    chunk = audio.CHUNK // 2
    for _ in range(int(seconds * 100)):
        clock.t += 0.01
        jb.push(_pcm(make(chunk, sent)))
        sent += chunk
        out.append(np.frombuffer(jb.pop(audio.CHUNK), np.int16))
        fills.append(jb.buffered_ms())
    return np.concatenate(out).astype(np.float32), fills


def _room(n, start):
    return np.random.default_rng(start).normal(0, 30, n)  # a quiet room, about -60 dBFS


def test_extra_delay_goes_in_the_next_pause():
    clock = _Clock()
    jb = JitterBuffer(target_ms=40, max_ms=200, clock=clock, adaptive=False)
    _feed(jb, clock, 2, _room)
    jb.push(_pcm(_room(100 * audio.RATE // 1000, 10**6)))  # a Wi-Fi burst: 100 ms more than it needs
    _, fills = _feed(jb, clock, 1, _room)
    assert fills[-1] <= 45  # skipped within about the half second its average takes, not drained over a minute


def test_extra_delay_under_constant_sound_drains_by_playing_faster_without_jumps():
    clock = _Clock()
    jb = JitterBuffer(target_ms=40, max_ms=200, clock=clock, adaptive=False)
    _feed(jb, clock, 2, _sine)
    jb.push(_pcm(_sine(100 * audio.RATE // 1000, 200 * audio.CHUNK // 2)))
    out, fills = _feed(jb, clock, 40, lambda n, s: _sine(n, s + 100 * audio.RATE // 1000))
    assert fills[200] > 100  # never skipped: it's never quiet
    assert max(fills[-200:]) <= 50
    assert _biggest_step(out[audio.RATE:]) < 650  # sped up smoothly, no jumps


def test_speech_is_never_skipped():
    clock = _Clock()
    jb = JitterBuffer(target_ms=40, max_ms=200, clock=clock, adaptive=False)
    speech = lambda n, s: _sine(n, s, freq=300, amp=1500)  # about -30 dBFS
    _feed(jb, clock, 2, speech)
    jb.push(_pcm(speech(60 * audio.RATE // 1000, 200 * audio.CHUNK // 2)))
    _, fills = _feed(jb, clock, 2, speech)
    assert min(fills) > 70


def test_a_splice_fades_out_the_old_connection_and_in_the_new_one():
    clock = _Clock()
    jb = JitterBuffer(target_ms=20, clock=clock, adaptive=False)
    jb.push(_pcm(np.full(30 * audio.RATE // 1000, 8000)))
    jb.splice()
    jb.push(_pcm(np.full(30 * audio.RATE // 1000, -8000)))  # where the other route's audio happens to start
    out = np.concatenate([np.frombuffer(jb.pop(audio.CHUNK), np.int16) for _ in range(5)]).astype(np.float32)
    assert _biggest_step(out[audio.FADE:]) < 400  # through zero in 2 ms steps, not a 16000 jump


@pytest.mark.skipif(not hasattr(os, "pipe2"), reason="Linux pipes")
def test_fifo_sink_drops_audio_nobody_reads_instead_of_blocking(monkeypatch):
    import fcntl
    monkeypatch.setattr(FifoSink, "STALL_S", 0.02)
    read_end, write_end = os.pipe()
    fcntl.fcntl(write_end, FifoSink._F_SETPIPE_SZ, FifoSink.PIPE_BYTES)
    sink = FifoSink("unused", open_fd=lambda _path: write_end)
    started = time.monotonic()
    for _ in range(20):  # five times what the pipe holds
        sink.write(bytes(960))
    assert time.monotonic() - started < 2
    assert len(os.read(read_end, 1 << 20)) % 960 == 0  # what went in went in whole
    sink.close()
    os.close(read_end)


class _Endless:
    """A phone connection that keeps sending one value until closed."""

    def __init__(self, value):
        self.value, self.closed = value, False

    def read1(self, n):
        if self.closed:
            raise OSError("closed")
        time.sleep(0.005)
        return _pcm(np.full(240, self.value))

    def close(self):
        self.closed = True


def test_worker_hands_over_to_another_route_without_restarting():
    conns = {}

    def opener(req, timeout):
        conns[req.full_url] = _Endless(100 if "wifi" in req.full_url else 200)
        return conns[req.full_url]

    sink = _Sink()
    w = AudioWorker("http://wifi/v1/audio", PhoneAuth("tok"), lambda: sink, lambda k, t: None, opener=opener)
    w.start()
    assert _until(lambda: b"\x64\x00" * 8 in bytes(sink.data))
    w.hand_over("http://usb/v1/audio")
    assert _until(lambda: conns["http://wifi/v1/audio"].closed)
    assert _until(lambda: b"\xc8\x00" * 8 in bytes(sink.data))
    w.stop()
    assert conns["http://usb/v1/audio"].closed


def test_a_hand_over_that_cant_connect_keeps_the_old_route():
    def opener(req, timeout):
        if "usb" in req.full_url:
            raise OSError("refused")
        return _Endless(100)

    sink = _Sink()
    statuses = []
    w = AudioWorker("http://wifi/v1/audio", PhoneAuth("tok"), lambda: sink, lambda k, t: statuses.append(k), opener=opener)
    w.start()
    assert _until(lambda: b"\x64\x00" * 8 in bytes(sink.data))
    w.hand_over("http://usb/v1/audio")
    time.sleep(0.2)
    before = len(sink.data)
    assert _until(lambda: len(sink.data) > before + 4000)  # still playing
    assert "err" not in statuses
    w.stop()
