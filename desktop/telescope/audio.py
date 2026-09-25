"""Plays the phone's microphone (/v1/audio: 48 kHz mono s16le) into the virtual microphone.

A reader thread fills a jitter buffer from the phone; a writer thread drains it into the output at
the output's own pace, padding with silence on a gap and dropping audio when the phone runs ahead
(its clock and the sound card's never quite agree), so the delay stays near TARGET_MS.
"""

import json
import logging
import os
import threading
import time
import urllib.error
import urllib.request
from typing import Callable, Optional

logger = logging.getLogger(__name__)

RATE = 48_000
BYTES_PER_MS = RATE * 2 // 1000
CHUNK = 10 * BYTES_PER_MS  # what the writer hands the output at a time
TARGET_MS = 60
MAX_MS = 150
RETRY_S = 3


class JitterBuffer:
    def __init__(self, target_ms: int = TARGET_MS, max_ms: int = MAX_MS):
        self._target = target_ms * BYTES_PER_MS
        self._max = max_ms * BYTES_PER_MS
        self._buf = bytearray()
        self._playing = False  # False until TARGET_MS has built up, and again after running dry
        self._lock = threading.Lock()

    def push(self, data: bytes):
        with self._lock:
            self._buf += data
            if len(self._buf) > self._max:
                drop = len(self._buf) - self._target
                drop -= drop % 2  # whole samples
                del self._buf[:drop]

    def pop(self, n: int) -> bytes:
        """n bytes of audio, or silence while (re)filling."""
        with self._lock:
            if not self._playing:
                if len(self._buf) < self._target:
                    return bytes(n)
                self._playing = True
            if len(self._buf) < n:
                out = bytes(self._buf) + bytes(n - len(self._buf))
                self._buf.clear()
                self._playing = False
                return out
            out = bytes(self._buf[:n])
            del self._buf[:n]
            return out

    def buffered_ms(self) -> int:
        with self._lock:
            return len(self._buf) // BYTES_PER_MS


class FifoSink:
    """Linux: the virtual mic's FIFO. The pipe source reads whatever arrives, so this paces itself
    to real time, and the pipe is shrunk to one page so a stalled reader can't queue up delay."""

    PIPE_BYTES = 4096  # about 40 ms; Linux won't go below a page
    _F_SETPIPE_SZ = 1031

    def __init__(self, path: str, clock: Callable = time.monotonic, sleep: Callable = time.sleep,
                 open_fd: Optional[Callable] = None):
        self._fd = (open_fd or self._open_fifo)(path)
        self._clock, self._sleep = clock, sleep
        self._due: Optional[float] = None

    @classmethod
    def _open_fifo(cls, path: str) -> int:
        # Non-blocking open fails at once if nothing holds the read end, instead of hanging.
        fd = os.open(path, os.O_WRONLY | getattr(os, "O_NONBLOCK", 0))
        os.set_blocking(fd, True)
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
        view = memoryview(data)
        while view:
            view = view[os.write(self._fd, view):]
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

    def __init__(self, url: str, token: str, open_sink: Callable, on_status: Callable,
                 opener: Callable = urllib.request.urlopen):
        self.url, self.token = url, token
        self._open_sink = open_sink
        self._on_status = on_status
        self._opener = opener
        self._stop = threading.Event()
        self._buffer = JitterBuffer()
        self._response = None
        self._threads: list = []

    def start(self):
        for target, name in ((self._read_loop, "mic-read"), (self._write_loop, "mic-write")):
            t = threading.Thread(target=target, name=name, daemon=True)
            t.start()
            self._threads.append(t)

    def stop(self):
        self._stop.set()
        resp = self._response
        if resp is not None:
            try:
                resp.close()
            except Exception:
                pass
        for t in self._threads:
            t.join(timeout=3)
        self._threads.clear()

    def _read_loop(self):
        while not self._stop.is_set():
            problem = self._read_once()
            if self._stop.is_set():
                break
            if problem:
                self._on_status("err", problem)
            self._stop.wait(RETRY_S)

    def _read_once(self) -> Optional[str]:
        req = urllib.request.Request(self.url, headers={"Authorization": f"Bearer {self.token}"})
        try:
            resp = self._opener(req, timeout=5)
        except urllib.error.HTTPError as e:
            return _phone_reason(e)
        except Exception:
            return "Can't reach the phone's microphone."
        self._response = resp
        told = False
        try:
            while not self._stop.is_set():
                chunk = resp.read1(4096)
                if not chunk:
                    return "The phone's microphone stopped."
                self._buffer.push(chunk)
                if not told:
                    self._on_status("ok", "")
                    told = True
        except Exception:
            return None if self._stop.is_set() else "The phone's microphone stopped."
        finally:
            self._response = None
            try:
                resp.close()
            except Exception:
                pass
        return None

    def _write_loop(self):
        try:
            sink = self._open_sink()
        except Exception as e:
            logger.exception("Couldn't open the microphone output")
            self._on_status("err", f"Couldn't open the virtual microphone: {e}")
            return
        try:
            while not self._stop.is_set():
                sink.write(self._buffer.pop(CHUNK))  # blocks at the output's pace
        except Exception:
            if not self._stop.is_set():
                logger.exception("Microphone output failed")
                self._on_status("err", "The virtual microphone stopped taking audio.")
        finally:
            sink.close()


def _phone_reason(err: urllib.error.HTTPError) -> str:
    try:
        return json.loads(err.read().decode("utf-8")).get("error") or f"HTTP {err.code}"
    except Exception:
        return f"The phone refused the microphone (HTTP {err.code})."
