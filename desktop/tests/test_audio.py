import io
import os
import json
import time
import urllib.error

import pytest


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
    w = AudioWorker("http://phone/v1/audio", "tok", lambda: sink, lambda k, t: statuses.append((k, t)),
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

    w = AudioWorker("http://p/v1/audio", "t", bad_sink, lambda k, t: statuses.append((k, t)), opener=opener)
    w.start()
    assert _until(lambda: any(k == "err" and "no reader" in t for k, t in statuses))
    w.stop()


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


def test_fifo_sink_writes_everything_at_real_time_pace(tmp_path):
    path = str(tmp_path / "f")
    os.mkfifo(path)
    reader = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
    clock = [100.0]
    sleeps = []

    def sleep(s):
        sleeps.append(round(s, 4))
        clock[0] += s

    sink = FifoSink(path, clock=lambda: clock[0], sleep=sleep)
    ten_ms = bytes(960)
    for _ in range(3):
        sink.write(ten_ms)
    assert os.read(reader, 10_000) == ten_ms * 3
    assert sleeps == [0.01, 0.01]  # the first write goes at once, then one every 10 ms
    clock[0] += 1.0  # a long stall: start over rather than burst to catch up
    sink.write(ten_ms)
    assert sleeps == [0.01, 0.01]
    sink.close()
    os.close(reader)


def test_fifo_sink_fails_fast_without_a_reader(tmp_path):
    path = str(tmp_path / "f")
    os.mkfifo(path)
    with pytest.raises(OSError):
        FifoSink(path)


def test_vb_cable_is_found_by_its_playback_end():
    devices = [{"name": "Speakers", "max_output_channels": 2},
               {"name": "CABLE Output (VB-Audio Virtual Cable)", "max_output_channels": 0},
               {"name": "CABLE Input (VB-Audio Virtual Cable)", "max_output_channels": 2}]
    assert virtual_mic.find_vb_cable(devices) == 2
    assert virtual_mic.find_vb_cable(devices[:2]) is None
