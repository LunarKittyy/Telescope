"""Plays the phone's microphone (/v1/audio: 48 kHz mono s16le) into the virtual microphone.

A reader thread fills a jitter buffer from the phone; a writer thread drains it into the output at
the output's own pace, playing slightly faster or slower since the phone's clock and the sound card's
never quite agree, so the delay stays near the target.
"""

import collections
import json
import logging
import math
import os
import select
import stat
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Callable, Optional

import numpy as np

from telescope.pinned_https import PhoneAuth
from telescope.i18n import tr

logger = logging.getLogger(__name__)

RATE = 48_000
BYTES_PER_MS = RATE * 2 // 1000
CHUNK = 10 * BYTES_PER_MS  # what the writer hands the output at a time
TARGET_MS = 60
USB_TARGET_MS = 20  # where the buffer starts on a steady connection; it grows when audio arrives unevenly
WIFI_TARGET_MS = 40
MAX_MS = 150  # jump back to the target once this far behind (more than playing faster can catch up)
MAX_TARGET_MS = 120
MAX_BACKLOG_MS = 500  # past MAX_MS, while nothing reads the output
JITTER_MARGIN_MS = 10
MAX_SKEW = 0.002  # playing 0.2% faster or slower can't be heard
SKEW_PER_SAMPLE = MAX_SKEW / (10 * RATE / 1000)  # full skew 10 ms off target
FAST_SKEW = 0.005  # far over target and never quiet (music, a fan): still hard to hear, and drains 100 ms in 20 s
FAST_AFTER_MS = 30  # over the target by this much, the skew can grow toward FAST_SKEW...
FAST_FULL_MS = 60  # ...reaching it here
QUIET_SKIP_MS = 20  # this far over target, a quiet moment is skipped with a crossfade instead
QUIET_DBFS = -50.0  # always quiet enough to skip...
QUIET_MAX_DBFS = -35.0  # ...never when louder (speech), and in between only near the room's own noise
FLOOR_RISE = 10 ** (1 / 20 / 100)  # the noise floor estimate creeps up 1 dB/s, so it follows a noisier room
AVERAGE_S = 0.5
FADE = 96  # samples (2 ms) to fade out and back in around a gap or a jump
LEAD_WINDOW_S = 10
LATE_FORGET_S = 5  # how quickly a late burst stops counting
RETRY_S = 3
_MOVED = object()  # _read_once's answer when the audio moved to another connection
FULL_SCALE = 32767
LIMIT_CEILING = 10 ** (-1 / 20)  # the limiter keeps peaks under -1 dBFS
LIMIT_RELEASE = 10 ** (0.25 / 20)  # per chunk once the peak has passed: 25 dB/s
# Automatic level: speech brought toward AUTO_TARGET_DB (RMS), within these bounds, and only while someone speaks
AUTO_TARGET_DB = -20.0
AUTO_MAX_BOOST_DB = 18.0
AUTO_MAX_CUT_DB = -12.0
AUTO_GATE_DB = 10.0  # over the room's own noise floor counts as speech; the gain holds during pauses...
AUTO_GATE_BELOW_SPEECH_DB = 18.0  # ...and so does anything this far under how loud speech has been (a fan starting up)
AUTO_GATE_FLOOR_DB = -60.0  # quieter than this is never speech
AUTO_FLOOR_S = 2.0  # the noise floor is the quietest 100 ms over this long: speech pauses at least that often
AUTO_UP_DB = 0.06  # per 10 ms chunk: 6 dB/s up, slow so it doesn't pump...
AUTO_DOWN_DB = 0.3  # ...and 30 dB/s down, so a loud bit comes down quickly (the limiter catches the first of it)
AUTO_LEVEL_RATE = 0.02  # per chunk: the speech level it goes by is an average over about half a second
LIMIT_ATTACK = 48  # samples (1 ms) to reach a new, lower gain; hard clipping catches anything in between


def _close(resp):
    try:
        getattr(resp, "abort", resp.close)()
    except Exception:
        pass


def _dbfs(db: float) -> float:
    return FULL_SCALE * 10 ** (db / 20)


class JitterBuffer:
    """Holds the phone's audio for the writer, playing up to MAX_SKEW faster or slower to stay near the target."""

    def __init__(self, target_ms: int = TARGET_MS, max_ms: int = MAX_MS, clock: Callable = time.monotonic,
                 adaptive: bool = True):
        self._base = target_ms * RATE // 1000  # the target with a steady connection, in samples
        self._target = self._base
        self._max_extra = (max_ms - target_ms) * RATE // 1000  # how far over target before jumping back
        self._adaptive = adaptive
        self._clock = clock
        self._buf = np.zeros(1, np.float32)
        self._pos = 1.0  # read position in _buf; the sample before it stays for the interpolation
        self._odd = b""
        self._playing = False  # False until the target has built up, and again after running dry
        self._avg = 0.0  # how full it is on average, in samples
        self._lock = threading.Lock()
        self._leads: collections.deque = collections.deque()  # [second, earliest arrival relative to the audio's own clock]
        self._late = 0.0  # how late audio has been arriving recently, in samples
        self._received = 0
        self._last_arrival = 0.0
        self.ran_dry = 0  # how many times it ran out and faded to silence
        self._floor = float(FULL_SCALE)  # the quietest it's been lately (RMS per chunk)
        self._fade_next = False  # the next audio pushed starts another connection's: fade it in
        self.has_audio = False  # the last pop was a whole chunk of recorded audio, rather than refill silence

    def set_target_ms(self, target_ms: int):
        """The connection changed: learn its jitter afresh, with the appropriate USB or Wi-Fi minimum."""
        with self._lock:
            base = target_ms * RATE // 1000
            self._max_extra = max(0, self._max_extra + self._base - base)
            self._base = self._target = base
            self._late = 0.0
            self._leads.clear()
            self._received = 0
            self._last_arrival = 0.0

    def push(self, data: bytes):
        now = self._clock()
        with self._lock:
            data = self._odd + data
            whole = len(data) - len(data) % 2
            self._odd = data[whole:]
            samples = np.frombuffer(data[:whole], np.int16).astype(np.float32)
            if not samples.size:
                return
            if self._fade_next:
                self._fade_next = False
                k = min(FADE, samples.size)
                samples[:k] *= np.linspace(0.0, 1.0, k, dtype=np.float32)
            self._buf = np.concatenate((self._buf, samples))
            self._track_jitter(now)  # by when this chunk's first sample arrived
            self._received += samples.size
            hard = int(self._pos) + self._target + self._max_extra + MAX_BACKLOG_MS * RATE // 1000
            if self._buf.size > hard:
                # The writer stalled (an idle output stops reading): never pile up more than this
                drop = self._buf.size - hard
                self._buf = self._buf[drop:]
                self._pos = max(1.0, self._pos - drop)

    def restart_clock(self):
        """A new connection: its audio's timing starts over, so the old arrivals say nothing about it."""
        with self._lock:
            self._leads.clear()
            self._received = 0
            self._odd = b""

    def splice(self):
        """The audio moves to another connection: fade out what's here and fade in what comes next, since the
        two don't line up to the sample."""
        with self._lock:
            k = min(FADE, int(self._buf.size - self._pos))
            if k > 0:
                self._buf[-k:] *= np.linspace(1.0, 0.0, k, dtype=np.float32)
            self._fade_next = True
            self._leads.clear()
            self._received = 0
            self._odd = b""

    def _track_jitter(self, now: float):
        lead = now * RATE - self._received  # the later audio arrives, the bigger this gets
        # The earliest arrival over the last LEAD_WINDOW_S, kept per second: audio lost on the way (a mic that
        # reopened, a backlog the phone dropped) shifts this for good, and a window lets that go
        second = int(now)
        if self._leads and self._leads[-1][0] == second:
            self._leads[-1][1] = min(self._leads[-1][1], lead)
        else:
            self._leads.append([second, lead])
        while self._leads[0][0] <= second - LEAD_WINDOW_S:
            self._leads.popleft()
        earliest = min(v for _s, v in self._leads)
        dt = max(0.0, now - self._last_arrival) if self._last_arrival else 0.0
        self._last_arrival = now
        if not self._adaptive:
            return
        self._late = max(lead - earliest, self._late * math.exp(-dt / LATE_FORGET_S))
        self._target = int(min(max(self._base, self._late + JITTER_MARGIN_MS * RATE / 1000),
                               MAX_TARGET_MS * RATE / 1000))

    def pop(self, n: int) -> bytes:
        """n bytes of audio, or silence while (re)filling."""
        m = n // 2
        with self._lock:
            self.has_audio = False
            fill = self._buf.size - self._pos
            if not self._playing:
                if fill < self._target:
                    return bytes(n)
                self._playing = True
                self._avg = fill
                fade_in = True
            else:
                fade_in = False
            self._avg += (fill - self._avg) * min(1.0, m / (AVERAGE_S * RATE))
            over_ms = (self._avg - self._target) * 1000 / RATE
            fast = min(1.0, max(0.0, (over_ms - FAST_AFTER_MS) / (FAST_FULL_MS - FAST_AFTER_MS)))
            ratio = 1.0 + float(np.clip((self._avg - self._target) * SKEW_PER_SAMPLE,
                                        -MAX_SKEW, MAX_SKEW + (FAST_SKEW - MAX_SKEW) * fast))
            skip = 0
            if fill - m * ratio > self._target + self._max_extra:
                skip = int(fill - m * ratio - self._target)  # far too much: jump back to the target
                self._avg = self._target
            elif (fill - m * ratio > self._target and over_ms > QUIET_SKIP_MS
                  and self._quiet(int(self._pos), int(fill - self._target))):
                skip = int(fill - m * ratio - self._target)  # a pause: jumping back to the target can't be heard
                self._avg = self._target
            at = self._pos + skip + ratio * np.arange(m, dtype=np.float64)
            usable = int(np.searchsorted(at, self._buf.size - 2.0, side="left"))  # cubic needs two samples ahead
            self.has_audio = usable == m
            out = self._cubic(at[:usable])
            if skip:
                k = min(FADE, usable)
                before = self._cubic(self._pos + ratio * np.arange(k, dtype=np.float64))
                w = np.linspace(0.0, 1.0, k, dtype=np.float32)
                out[:k] = before * (1 - w) + out[:k] * w
            if fade_in:
                k = min(FADE, out.size)
                out[:k] *= np.linspace(0.0, 1.0, k, dtype=np.float32)
            if out.size:
                self._floor = min(float(np.sqrt(np.mean(out * out))), self._floor * FLOOR_RISE)
            if usable < m:
                # Ran dry: fade out what's left instead of stopping dead, then fill up to the target again
                k = min(FADE, out.size)
                if k:
                    out[-k:] *= np.linspace(1.0, 0.0, k, dtype=np.float32)
                self._buf = self._buf[-1:]
                self._pos = 1.0
                self._playing = False
                self.ran_dry += 1
                out = np.concatenate((out, np.zeros(m - usable, np.float32)))
            else:
                end = self._pos + skip + ratio * m
                keep_from = int(end) - 1
                self._buf = self._buf[keep_from:]
                self._pos = end - keep_from
            return np.clip(np.rint(out), -32768, FULL_SCALE).astype(np.int16).tobytes() + bytes(n - 2 * m)

    def _quiet(self, start: int, n: int) -> bool:
        """Whether the n samples from start (what a skip would leave out, and a bit after) are a quiet moment."""
        part = self._buf[start:start + n]
        if not part.size:
            return False
        rms = float(np.sqrt(np.mean(part * part)))
        limit = min(_dbfs(QUIET_MAX_DBFS), max(_dbfs(QUIET_DBFS), self._floor * 2))
        return rms < limit

    def _cubic(self, at: np.ndarray) -> np.ndarray:
        """Catmull-Rom between samples: straight lines would dull "s" and "t" sounds."""
        i = np.floor(at).astype(np.int64)
        t = (at - i).astype(np.float32)
        x = self._buf
        p0, p1, p2, p3 = x[np.maximum(i - 1, 0)], x[i], x[i + 1], x[i + 2]
        return p1 + 0.5 * t * (p2 - p0 + t * (2 * p0 - 5 * p1 + 4 * p2 - p3 + t * (3 * (p1 - p2) + p3 - p0)))

    def buffered_ms(self) -> int:
        with self._lock:
            return int(self._buf.size - self._pos) * 1000 // RATE

    def target_ms(self) -> int:
        with self._lock:
            return self._target * 1000 // RATE


class FifoSink:
    """Linux: the virtual mic's FIFO. The pipe source reads whatever arrives, so this paces itself
    to real time, and the pipe is shrunk to one page so a stalled reader can't queue up delay."""

    PIPE_BYTES = 4096  # about 40 ms; Linux won't go below a page
    STALL_S = 0.2  # a pipe that stays full this long has no reader (nothing uses the mic): drop the chunk
    _F_SETPIPE_SZ = 1031

    def __init__(self, path: str, clock: Callable = time.monotonic, sleep: Callable = time.sleep,
                 open_fd: Optional[Callable] = None):
        self._fd = (open_fd or self._open_fifo)(path)
        os.set_blocking(self._fd, False)  # so a pipe nobody reads can't hold up stopping
        self._clock, self._sleep = clock, sleep
        self._due: Optional[float] = None

    @classmethod
    def _open_fifo(cls, path: str) -> int:
        # Non-blocking open fails at once if nothing holds the read end, instead of hanging.
        fd = os.open(path, os.O_WRONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0))
        # Only the sound server's pipe, made by this user: never a file someone left there to have audio written into
        st = os.fstat(fd)
        if not stat.S_ISFIFO(st.st_mode) or (hasattr(os, "getuid") and st.st_uid != os.getuid()):
            os.close(fd)
            raise OSError(f"{path} isn't the microphone's pipe")
        try:
            import fcntl
            fcntl.fcntl(fd, cls._F_SETPIPE_SZ, cls.PIPE_BYTES)
        except (ImportError, OSError):
            pass
        return fd

    def write(self, data: bytes):
        now = self._clock()
        if self._due is None or now - self._due > 0.05:
            self._due = now  # first write, or so far behind that catching up would only burst
        elif self._due > now:
            self._sleep(self._due - now)
        # Chunks are under PIPE_BUF, so each write goes in whole or not at all and samples never split
        give_up = None
        while True:
            try:
                os.write(self._fd, data)
                break
            except BlockingIOError:
                give_up = give_up or time.monotonic() + self.STALL_S
                left = give_up - time.monotonic()
                if left <= 0 or not select.select([], [self._fd], [], left)[1]:
                    break
        self._due += len(data) / (RATE * 2)

    def close(self):
        try:
            os.close(self._fd)
        except OSError:
            pass


class SoundDeviceSink:
    """Windows: sounddevice into VB-Cable's playback end."""

    def __init__(self, sd, device: int):
        self._stream = sd.RawOutputStream(samplerate=RATE, channels=1, dtype="int16",
                                          device=device, latency="low")
        self._stream.start()

    def write(self, data: bytes):
        self._stream.write(data)

    def close(self):
        try:
            self._stream.stop()
            self._stream.close()
        except Exception:
            pass


class AudioWorker:
    """on_status(kind, text) is called from the worker's threads: "ok" once audio flows, "err" with
    the phone's reason (it retries every few seconds, so allowing the mic on the phone just works)."""

    def __init__(self, url: str, auth: PhoneAuth, open_sink: Callable, on_status: Callable,
                 opener: Optional[Callable] = None):
        self.url, self.auth = url, auth
        self._open_sink = open_sink
        self._on_status = on_status
        self._opener = opener or auth.open
        self._stop = threading.Event()
        self._buffer = JitterBuffer(_target_for(url))
        self._response = None
        self._threads: list = []
        self.gain = 1.0  # linear; changes fade in over a chunk like mute does
        self.muted = False  # the output gets silence, so apps keep the mic but hear nothing
        self.limit = True  # squash peaks smoothly instead of letting gain clip them
        self.auto_level = False  # bring speech to a steady level, after gain and before the limiter
        self._auto_db = 0.0  # automatic level's gain now
        self._speech_db: Optional[float] = None  # how loud speech has been lately (RMS, dBFS); None before any
        self._quietest: collections.deque = collections.deque(maxlen=int(AUTO_FLOOR_S * 10))  # per 100 ms, the quietest chunk
        self._chunks_in_block = 0
        self._limit_gain = 1.0  # the limiter's current gain reduction
        self._gain = 1.0  # where the last chunk's gain fade ended
        self._mute_gain = 1.0  # where the last chunk's mute fade ended
        self._peak = self._rms = 0.0
        self._limited = False
        self._level_lock = threading.Lock()
        self._handover_lock = threading.Lock()
        self._handover = None  # (response, its first audio) for a connection that is ready to take over
        self._odd = b""  # a reply can end mid-sample; its first byte waits for the next one

    def take_level(self) -> tuple:
        """(peak, rms, limited) since the last call: fractions of full scale after gain and limiter, before muting."""
        # Measured as audio arrives, not as it's written: PipeWire stops reading an idle mic, which stalls the writer
        with self._level_lock:
            level = (self._peak, self._rms, self._limited)
            self._peak = self._rms = 0.0
            self._limited = False
        return level

    def start(self):
        for target, name in ((self._read_loop, "mic-read"), (self._write_loop, "mic-write")):
            t = threading.Thread(target=target, name=name, daemon=True)
            t.start()
            self._threads.append(t)

    def stop(self):
        self._stop.set()
        with self._handover_lock:
            pending, self._handover = self._handover, None
        if pending is not None:
            _close(pending[0])
        resp = self._response
        if resp is not None:
            _close(resp)
        for t in self._threads:
            t.join(timeout=3)
        self._threads.clear()

    def hand_over(self, url: str):
        """Moves to url without a gap: opens it next to the current connection and switches once audio comes
        through. If it doesn't, the current one keeps going and the next reconnect goes to url."""
        self.url = url

        def work():
            try:
                resp = self._opener(urllib.request.Request(url, headers=self.auth.headers()), timeout=5)
            except Exception:
                logger.info("Couldn't open the microphone's new connection; staying on the old one")
                return
            try:
                first = resp.read1(4096)
            except Exception:
                first = b""
            with self._handover_lock:
                if not first or self._stop.is_set() or self.url != url:
                    _close(resp)
                    return
                old, self._handover = self._handover, (resp, first)
                current = self._response
            if old is not None:
                _close(old[0])
            if current is not None:
                _close(current)  # wakes a read waiting on the old route; it sees the hand-over and moves on
        threading.Thread(target=work, name="mic-handover", daemon=True).start()

    def _read_loop(self):
        while not self._stop.is_set():
            problem = self._read_once()
            if self._stop.is_set():
                break
            if problem is _MOVED:
                continue
            if problem:
                self._on_status("err", problem)
            until = time.monotonic() + RETRY_S
            while time.monotonic() < until and self._handover is None:  # a hand-over meanwhile goes ahead at once
                if self._stop.wait(min(0.1, RETRY_S)):
                    break

    def _read_once(self):
        with self._handover_lock:
            taken, self._handover = self._handover, None
            if taken is not None:
                resp, pending = taken
                self._response = resp
        if taken is not None:
            self._buffer.splice()
            self._buffer.set_target_ms(_target_for(self.url))
        else:
            req = urllib.request.Request(self.url, headers=self.auth.headers())
            try:
                resp = self._opener(req, timeout=5)
            except urllib.error.HTTPError as e:
                return _phone_reason(e)
            except Exception:
                return tr("Can't reach the phone's microphone.")
            pending = b""
            self._response = resp
            self._buffer.restart_clock()
            self._buffer.set_target_ms(_target_for(self.url))
        self._odd = b""
        told = False
        try:
            while not self._stop.is_set():
                if self._handover is not None:
                    return _MOVED
                chunk, pending = pending or resp.read1(4096), b""
                if not chunk:
                    return _MOVED if self._handover is not None else tr("The phone's microphone stopped.")
                self._buffer.push(chunk)
                self._measure(chunk)
                if not told:
                    self._on_status("ok", "")
                    told = True
        except Exception:
            if self._stop.is_set():
                return None
            return _MOVED if self._handover is not None else tr("The phone's microphone stopped.")
        finally:
            self._response = None
            try:
                resp.close()
            except Exception:
                pass
        return None

    def _write_loop(self):
        # The output is reopened after a failure (a restarted sound server, a reader that went away)
        failed = False
        while not self._stop.is_set():
            problem = self._write_once(failed)
            if problem is None or self._stop.is_set():
                break
            failed = True
            self._on_status("err", problem)
            self._stop.wait(RETRY_S)

    def _write_once(self, recovering: bool) -> Optional[str]:
        try:
            sink = self._open_sink()
        except Exception as e:
            if not recovering:
                logger.exception("Couldn't open the microphone output")
            return tr("Couldn't open the virtual microphone: {error}", error=e)
        try:
            if recovering:
                self._on_status("ok", "")
            while not self._stop.is_set():
                raw = self._buffer.pop(CHUNK)
                data = self._shape(raw, observe_level=self._buffer.has_audio)
                sink.write(self._mute(data))  # blocks at the output's pace
        except Exception:
            if not self._stop.is_set():
                logger.exception("Microphone output failed")
                return tr("The virtual microphone stopped taking audio.")
        finally:
            sink.close()
        return None

    def _measure(self, data: bytes):
        data = self._odd + data
        whole = len(data) - len(data) % 2
        self._odd = data[whole:]
        samples = np.frombuffer(data[:whole], np.int16).astype(np.float32)
        if not samples.size:
            return
        gain = self.gain * (10 ** (self._auto_db / 20) if self.auto_level else 1.0) / FULL_SCALE
        peak = float(np.abs(samples).max()) * gain
        rms = float(np.sqrt(np.mean(samples * samples))) * gain
        limited = self.limit and peak > LIMIT_CEILING
        top = LIMIT_CEILING if self.limit else 1.0  # what the output can reach
        with self._level_lock:
            self._peak = max(self._peak, min(peak, top))
            self._rms = max(self._rms, min(rms, top))
            self._limited = self._limited or limited

    def _shape(self, data: bytes, observe_level: bool = True) -> bytes:
        """Gain, then the limiter, then clipping to 16 bits."""
        target = self.gain
        raw = np.frombuffer(data, np.int16)
        if not self.auto_level and self._auto_db == 0.0 and target == self._gain == 1.0 and (not self.limit or (
                self._limit_gain == 1.0 and int(np.abs(raw.astype(np.int32)).max(initial=0)) <= LIMIT_CEILING * FULL_SCALE)):
            self._limit_gain = 1.0
            return data  # nothing to do, the usual case
        samples = raw.astype(np.float32)
        if target != self._gain:
            samples *= np.linspace(self._gain, target, samples.size, dtype=np.float32)  # a jump in gain clicks too
            self._gain = target
        elif target != 1.0:
            samples *= target
        if self.auto_level:
            samples = self._auto(samples) if observe_level else samples * 10 ** (self._auto_db / 20)
        elif self._auto_db != 0.0:
            # Switched off: its gain fades out over this chunk, and it starts over if switched on again
            samples = samples * np.linspace(10 ** (self._auto_db / 20), 1.0, samples.size, dtype=np.float32)
            self._auto_db = 0.0
            self._speech_db = None
            self._quietest.clear()
        if self.limit:
            samples = self._limit(samples)
        else:
            self._limit_gain = 1.0
        return np.clip(samples, -32768, FULL_SCALE).astype(np.int16).tobytes()

    def _auto(self, samples: np.ndarray) -> np.ndarray:
        """Automatic level: follows how loud speech is and moves its gain toward the target, holding it in pauses."""
        if not samples.size:
            return samples
        rms = float(np.sqrt(np.mean(samples * samples))) / FULL_SCALE
        db = 20 * math.log10(max(rms, 1e-9))
        if db < -90:
            # Digital silence (startup, a muted source, a recording gap) says nothing about the room's noise floor.
            return samples * 10 ** (self._auto_db / 20)
        if self._chunks_in_block == 0 or not self._quietest:
            self._quietest.append(db)
        else:
            self._quietest[-1] = min(self._quietest[-1], db)
        self._chunks_in_block = (self._chunks_in_block + 1) % 10
        gate = max(min(self._quietest) + AUTO_GATE_DB, AUTO_GATE_FLOOR_DB)
        if self._speech_db is not None:
            gate = max(gate, self._speech_db - AUTO_GATE_BELOW_SPEECH_DB)
        before = self._auto_db
        if db > gate:
            self._speech_db = db if self._speech_db is None else self._speech_db + (db - self._speech_db) * AUTO_LEVEL_RATE
            want = min(max(AUTO_TARGET_DB - self._speech_db, AUTO_MAX_CUT_DB), AUTO_MAX_BOOST_DB)
            self._auto_db += min(max(want - before, -AUTO_DOWN_DB), AUTO_UP_DB)
        if before == self._auto_db == 0.0:
            return samples
        ramp = np.linspace(10 ** (before / 20), 10 ** (self._auto_db / 20), samples.size, dtype=np.float32)
        return samples * ramp  # a jump in gain clicks

    def _limit(self, samples: np.ndarray) -> np.ndarray:
        peak = float(np.abs(samples).max()) / FULL_SCALE if samples.size else 0.0
        need = LIMIT_CEILING / peak if peak > LIMIT_CEILING else 1.0
        target = min(need, self._limit_gain * LIMIT_RELEASE, 1.0)
        if target == self._limit_gain == 1.0:
            return samples
        if target < self._limit_gain:
            ramp = np.full(samples.size, target, np.float32)  # down fast, so the peak is already under
            n = min(LIMIT_ATTACK, samples.size)
            ramp[:n] = np.linspace(self._limit_gain, target, n, dtype=np.float32)
        else:
            ramp = np.linspace(self._limit_gain, target, samples.size, dtype=np.float32)  # back up slowly
        self._limit_gain = target
        return samples * ramp

    def _mute(self, data: bytes) -> bytes:
        target = 0.0 if self.muted else 1.0
        if target == self._mute_gain:
            return data if target else bytes(len(data))
        # Fade over one chunk (10 ms): cutting mid-wave clicks
        samples = np.frombuffer(data, np.int16)
        ramp = np.linspace(self._mute_gain, target, samples.size, dtype=np.float32)
        self._mute_gain = target
        return (samples * ramp).astype(np.int16).tobytes()


def _target_for(url: str) -> int:
    """USB arrives evenly enough for a shorter buffer than Wi-Fi."""
    host = urllib.parse.urlsplit(url).hostname or ""
    return USB_TARGET_MS if host in ("127.0.0.1", "localhost") else WIFI_TARGET_MS


def phone_mic_reason(code, english: str) -> str:
    """Why the phone won't give its microphone, in the current language. `code` is what the phone sent next to its
    English text; a phone that sends none, or one this app doesn't know, gets the English as it is."""
    # Translators: why the phone's microphone can't be used; shown next to the Microphone card
    reasons = {"mic_none": tr("No microphone"),
               "mic_not_streaming": tr("Not streaming"),
               "mic_permission": tr("Allow the microphone in Telescope on the phone"),
               "mic_open_app": tr("Open Telescope on the phone once, then try again"),
               "mic_in_use": tr("The phone's microphone is in use")}
    return (reasons.get(code) if isinstance(code, str) else None) or english


def _phone_reason(err: urllib.error.HTTPError) -> str:
    try:
        body = json.loads(err.read(4096).decode("utf-8"))
        return phone_mic_reason(body.get("code"), body.get("error") or f"HTTP {err.code}")
    except Exception:
        return tr("The phone refused the microphone (HTTP {code}).", code=err.code)
