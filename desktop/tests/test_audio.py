import io
import json
import time
import urllib.error


from telescope import audio
from telescope.audio import BYTES_PER_MS, AudioWorker, JitterBuffer
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
        raise OSError("pacat: not found")

    w = AudioWorker("http://p/v1/audio", "t", bad_sink, lambda k, t: statuses.append((k, t)), opener=opener)
    w.start()
    assert _until(lambda: any(k == "err" and "pacat" in t for k, t in statuses))
    w.stop()


# ── Virtual mic plumbing ──────────────────────────────────────────────────────

class _Pactl:
    def __init__(self, sources="", fail_on=None):
        self.sources = sources
        self.fail_on = fail_on
        self.calls = []
        self.next_id = 40

    def __call__(self, cmd, timeout=5):
        self.calls.append(cmd)
        if cmd[1:3] == ["list", "short"]:
            return True, self.sources
        if cmd[1] == "load-module":
            if self.fail_on and self.fail_on in cmd[2]:
                return False, "Module initialization failed"
            self.next_id += 1
            return True, str(self.next_id)
        return True, ""


def test_linux_setup_creates_sink_then_named_source():
    pactl = _Pactl()
    ids, err = virtual_mic.linux_setup(pactl)
    assert err == "" and ids == [41, 42]
    null, remap = pactl.calls[1], pactl.calls[2]
    assert null[2] == "module-null-sink" and "sink_name=telescope_mic_sink" in null
    assert remap[2] == "module-remap-source"
    assert "master=telescope_mic_sink.monitor" in remap
    assert 'source_properties=device.description="Telescope Microphone"' in remap
    virtual_mic.linux_teardown(ids, pactl)
    assert pactl.calls[-2:] == [["pactl", "unload-module", "42"], ["pactl", "unload-module", "41"]]


def test_linux_setup_reuses_an_existing_source():
    pactl = _Pactl(sources="3\ttelescope_mic\tmodule-remap-source.c\ts16le 1ch 48000Hz\tIDLE")
    assert virtual_mic.linux_setup(pactl) == ([], "")
    assert len(pactl.calls) == 1


def test_linux_setup_undoes_half_a_setup():
    pactl = _Pactl(fail_on="remap")
    ids, err = virtual_mic.linux_setup(pactl)
    assert ids == [] and "Module initialization failed" in err
    assert pactl.calls[-1] == ["pactl", "unload-module", "41"]


def test_linux_tools_and_pacat_target_the_sink():
    assert virtual_mic.linux_tools_missing(lambda t: None if t == "pacat" else "/usr/bin/" + t) == ["pacat"]
    cmd = virtual_mic.pacat_command()
    assert cmd[0] == "pacat" and "--device=telescope_mic_sink" in cmd and "--rate=48000" in cmd


def test_vb_cable_is_found_by_its_playback_end():
    devices = [{"name": "Speakers", "max_output_channels": 2},
               {"name": "CABLE Output (VB-Audio Virtual Cable)", "max_output_channels": 0},
               {"name": "CABLE Input (VB-Audio Virtual Cable)", "max_output_channels": 2}]
    assert virtual_mic.find_vb_cable(devices) == 2
    assert virtual_mic.find_vb_cable(devices[:2]) is None
