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


def test_jitter_buffer_waits_for_the_target_then_plays_in_order():
    jb = JitterBuffer(target_ms=20, max_ms=100)
    jb.push(b"\x01\x00" * (10 * BYTES_PER_MS // 2))
    assert jb.pop(4) == bytes(4)  # still filling: silence
    jb.push(b"\x02\x00" * (10 * BYTES_PER_MS // 2))
    assert jb.pop(4) == b"\x01\x00\x01\x00"
    assert jb.buffered_ms() == 19


def test_jitter_buffer_pads_a_gap_with_silence_and_refills():
    jb = JitterBuffer(target_ms=1, max_ms=100)
    jb.push(b"\x05\x00" * BYTES_PER_MS)  # 2 ms
    jb.pop(BYTES_PER_MS * 2 - 2)
    out = jb.pop(6)
    assert out == b"\x05\x00" + bytes(4)
    assert jb.pop(4) == bytes(4)  # refilling again


def test_jitter_buffer_drops_the_oldest_audio_when_the_phone_runs_ahead():
    jb = JitterBuffer(target_ms=10, max_ms=30)
    jb.push(b"\x01\x00" * (20 * BYTES_PER_MS // 2))
    jb.push(b"\x02\x00" * (20 * BYTES_PER_MS // 2))  # 40 ms > max: back down to 10
    assert jb.buffered_ms() == 10
    assert jb.pop(2) == b"\x02\x00"


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
