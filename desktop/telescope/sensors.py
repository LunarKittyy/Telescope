"""The phone's motion and light sensors for plugins (HostSensors in plugin.py), from its /v1/sensors.

Plugins say which kinds they want, each under its own owner name. The shown stream's phone streams what anyone wants
of what it has, and stops as soon as nobody wants anything, so the phone's sensors are only on while they're used. It
follows the panels from phone to phone and copes with streams starting, stopping and moving to another route. No Qt
here: the window passes on_change and turns it into the bus's sensors_changed.
"""

import http.client
import json
import logging
import socket
import sys
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, replace
from typing import Callable, Iterable, Optional

logger = logging.getLogger(__name__)

KINDS = ("orientation", "accelerometer", "gyroscope", "magnetometer", "gravity", "rotation", "light")
"""What a plugin can ask for. orientation is the shown picture's roll and pitch, worked out on the phone for the camera
streaming; the rest are Android's own readings in the phone's axes (x right, y up, z out of the screen)."""

_VECTORS = ("accelerometer", "gyroscope", "magnetometer", "gravity", "rotation")
_RETRY_S = 2.0
_FAIL_AFTER_S = 6.0  # trying to reach the sensors for this long before saying it can't
_READ_TIMEOUT_S = 5.0  # the phone sends a line at least every second
_WINDOWS = sys.platform == "win32"
_ALIVE_DEFAULT_MS = 15_000  # how long the phone waits for a request before it stops, when its hello doesn't say
_MAX_LINE = 64 * 1024

REASONS = ("not_phone", "old_phone", "none", "camera", "failed")
"""Why sensors are unavailable: the stream isn't from a phone, the phone's app is from before sensors, the phone has none
of the kinds wanted, only orientation was wanted and the camera streaming is an external one (or still starting), or it
can't be reached. A plugin words them itself, where it shows them."""


@dataclass(frozen=True)
class SensorReading:
    """The newest of each kind streamed (None for one that isn't, or hasn't come in yet)."""

    at: float = 0.0
    """time.monotonic() when the last of these came in."""
    roll: Optional[float] = None
    """Degrees the horizon is turned clockwise in the picture as the phone sends it (before the view's flip and
    rotation), -180 to 180. Turning the picture anticlockwise by this levels it. None while the camera points nearly
    straight up or down, where roll means nothing."""
    pitch: Optional[float] = None
    """Degrees the camera looks above the horizon (below is negative)."""
    camera: str = ""
    """The phone's id of the camera roll and pitch are for: a reading from before a camera switch says the old one."""
    orientation_at: float = 0.0
    """time.monotonic() when roll and pitch last came in (at also moves for other kinds)."""
    accelerometer: Optional[tuple] = None
    """m/s², (x, y, z)."""
    gyroscope: Optional[tuple] = None
    """rad/s, (x, y, z)."""
    magnetometer: Optional[tuple] = None
    """µT, (x, y, z)."""
    gravity: Optional[tuple] = None
    """m/s², (x, y, z), Android's own gravity from all its motion sensors."""
    rotation: Optional[tuple] = None
    """(x, y, z, w) quaternion of the game rotation vector: the phone's turn from where it started, without the compass."""
    light: Optional[float] = None
    """Lux, at most twice a second."""


@dataclass(frozen=True)
class SensorStatus:
    """Where the shown stream's sensors stand."""

    state: str = "off"
    """"off" (no stream, or nobody wants any), "starting", "on" or "unavailable"."""
    kinds: tuple = ()
    """What the phone has, from its /v1/state (empty until that comes in)."""
    streaming: tuple = ()
    """What it streams now: the kinds wanted that it has."""
    reason: str = ""
    """With "unavailable": why, one of REASONS."""


def _socket_of(resp):
    """The socket under an http.client response, or None."""
    return getattr(getattr(getattr(resp, "fp", None), "raw", None), "_sock", None)


def _wake(resp):
    """Ends a read blocked on a plain http response's socket, from another thread, without touching the response's own
    lock. Not for TLS: SSLSocket.shutdown() clears OpenSSL state under the reader."""
    sock = _socket_of(resp)
    if sock is None:
        return
    try:
        sock.shutdown(socket.SHUT_RDWR)  # enough on Linux
    except (OSError, ValueError):
        pass
    if _WINDOWS:
        # Windows only wakes a blocked recv when the handle closes; detached first, so nothing closes it again later
        try:
            socket.socket(fileno=sock.detach()).close()
        except (OSError, ValueError):
            pass


def _abort_quietly(resp):
    try:
        resp.abort()
    except Exception:
        pass


def _close_quietly(resp):
    try:
        resp.close()
    except Exception:
        pass


def _number(value) -> Optional[float]:
    return float(value) if isinstance(value, (int, float)) and not isinstance(value, bool) else None


def apply_line(reading: SensorReading, line: dict, now: float) -> SensorReading:
    """reading with what one line of /v1/sensors brought: only the kinds that changed are in it."""
    changes: dict = {}
    orientation = line.get("orientation")
    if isinstance(orientation, dict):
        changes["roll"] = _number(orientation.get("roll"))
        changes["pitch"] = _number(orientation.get("pitch"))
        changes["camera"] = str(orientation.get("camera") or "")
        changes["orientation_at"] = now
    for kind in _VECTORS:
        values = line.get(kind)
        if isinstance(values, list) and values:
            changes[kind] = tuple(_number(v) for v in values)
    if "light" in line:
        changes["light"] = _number(line.get("light"))
    return replace(reading, at=now, **changes) if changes else reading


def _only(reading: SensorReading, kinds: tuple) -> SensorReading:
    """reading without the kinds that aren't streamed any more."""
    gone: dict = {k: None for k in _VECTORS + ("light",) if k not in kinds}
    if "orientation" not in kinds:
        gone.update(roll=None, pitch=None, camera="", orientation_at=0.0)
    return replace(reading, **gone)


class _Target:
    """The stream the hub follows: the one the panels show."""

    def __init__(self, source_id: str, base: Optional[str], auth, phone: bool):
        self.source_id = source_id
        self.base = base
        self.auth = auth
        self.phone = phone
        self.offered: Optional[tuple] = None  # from its /v1/state; None until that comes
        self.old = False  # its /v1/state has no sensors: a phone from before /v1/sensors


class _Reader:
    """Reads one /v1/sensors on its own thread until stopped, opening it again when it drops."""

    def __init__(self, hub: "SensorHub", url: str, auth, kinds: tuple):
        self.url, self.auth, self.kinds = url, auth, kinds
        self._hub = hub
        self._stop = threading.Event()
        self._response = None
        self._thread = threading.Thread(target=self._run, name="phone-sensors", daemon=True)

    def start(self):
        self._thread.start()

    def stop(self):
        """Any thread, without waiting: closing the response itself would wait for the readline() holding its lock."""
        self._stop.set()
        resp = self._response
        if resp is None:
            return
        if hasattr(resp, "abort"):  # a paired phone's (pinned_https): wakes the read without touching its TLS state
            threading.Thread(target=_abort_quietly, args=(resp,), name="phone-sensors-close", daemon=True).start()
            return
        _wake(resp)
        threading.Thread(target=_close_quietly, args=(resp,), name="phone-sensors-close", daemon=True).start()

    @property
    def stopped(self) -> bool:
        return self._stop.is_set()

    def _run(self):
        failing_since: Optional[float] = None
        while not self._stop.is_set():
            problem = self._read_once()
            if self._stop.is_set():
                break
            if problem in ("old_phone", "none"):
                self._hub._reader_failed(self, problem)
                return  # asking again won't change the answer
            now = time.monotonic()
            failing_since = failing_since if failing_since is not None and problem else now
            if now - failing_since >= _FAIL_AFTER_S:
                self._hub._reader_failed(self, "failed")
            else:
                self._hub._reader_waiting(self)
            if self._stop.wait(_RETRY_S):
                break

    def _read_once(self) -> Optional[str]:
        """Streams until the connection ends; None when it got through at all, else what went wrong."""
        req = urllib.request.Request(self.url, headers=self.auth.headers())
        try:
            resp = self.auth.open(req, timeout=_READ_TIMEOUT_S)
        except urllib.error.HTTPError as e:
            return {404: "old_phone", 409: "none"}.get(e.code, "failed")
        except Exception:
            return "failed"
        self._response = resp
        got_through = False
        alive_stop = threading.Event()
        try:
            while not self._stop.is_set():
                raw = resp.readline(_MAX_LINE)
                if not raw:
                    break
                try:
                    line = json.loads(raw)
                except ValueError:
                    continue
                if not isinstance(line, dict):
                    continue
                got_through = True
                if "sensors" in line:
                    alive_ms = line.get("alive_ms")
                    alive_ms = alive_ms if isinstance(alive_ms, int) and alive_ms > 0 else _ALIVE_DEFAULT_MS
                    threading.Thread(target=self._keep_alive, args=(alive_ms / 3000, alive_stop), name="phone-sensors-alive",
                                     daemon=True).start()
                    self._hub._reader_hello(self, line.get("sensors"))
                else:
                    self._hub._reader_line(self, line)
        except (OSError, ValueError, http.client.HTTPException):
            pass  # the connection dropped, or stop() ended the read
        except Exception:
            logger.exception("Reading the phone's sensors failed")
        finally:
            alive_stop.set()
            self._response = None
            _close_quietly(resp)
        return None if got_through else "failed"

    def _keep_alive(self, every_s: float, done: threading.Event):
        """Tells the phone this computer is still here: it stops the sensors of one that went quiet."""
        url = self.url.split("?", 1)[0] + "/alive"
        while not done.wait(every_s) and not self._stop.is_set():
            try:
                with self.auth.open(urllib.request.Request(url, headers=self.auth.headers()), timeout=4) as r:
                    r.read(256)
            except Exception:
                pass  # the stream itself notices a phone that's gone


class SensorHub:
    """Who wants which sensors, and the shown stream's phone streaming them (HostSensors)."""

    def __init__(self, on_change: Callable[[str], None]):
        self._on_change = on_change
        self._lock = threading.Lock()
        self._wants: dict = {}  # owner: frozenset of kinds
        self._target: Optional[_Target] = None
        self._reader: Optional[_Reader] = None
        self._reading: Optional[SensorReading] = None
        self._state = "off"
        self._reason = ""  # one of REASONS
        self._streaming: tuple = ()

    # ── What plugins call (any thread) ────────────────────────────────────

    def want(self, owner: str, kinds: Iterable[str]):
        wanted = frozenset(k for k in kinds if k in KINDS)
        with self._lock:
            if wanted:
                self._wants[owner] = wanted
            else:
                self._wants.pop(owner, None)
        self._sync()

    def wanted(self) -> frozenset:
        with self._lock:
            return frozenset().union(*self._wants.values())

    def status(self, source_id: Optional[str] = None) -> SensorStatus:
        """source_id's sensors; None: the stream followed."""
        with self._lock:
            target = self._target
            if target is None or (source_id is not None and target.source_id != source_id):
                return SensorStatus()
            return SensorStatus(self._state, target.offered or (), self._streaming,
                                self._reason if self._state == "unavailable" else "")

    def reading(self, source_id: Optional[str] = None) -> Optional[SensorReading]:
        with self._lock:
            target = self._target
            if target is None or (source_id is not None and target.source_id != source_id):
                return None
            return self._reading

    # ── What the window calls (GUI thread) ────────────────────────────────

    def follow(self, source_id: Optional[str], base: Optional[str] = None, auth=None, phone: bool = True,
               state: Optional[dict] = None):
        """The panels show source_id's stream (None: no stream), reached at base (the phone's https://.../v1). state:
        its /v1/state when one just came in ({} when it didn't answer)."""
        changed_source = None
        with self._lock:
            target = self._target
            if source_id is None:
                if target is not None:
                    changed_source = target.source_id
                self._target = None
            elif target is None or target.source_id != source_id:
                if target is not None:
                    changed_source = target.source_id
                self._target = _Target(source_id, base, auth, phone)
                self._reading = None
            else:
                target.base, target.auth, target.phone = base, auth, phone
            if self._target is not None and state:  # {} is a /v1/state that didn't answer, not an old phone
                offered = state.get("sensors")
                self._target.old = not isinstance(offered, list)
                self._target.offered = tuple(k for k in offered if k in KINDS) if isinstance(offered, list) else ()
        if changed_source is not None:
            self._notify(changed_source)  # its readings are gone
        self._sync()

    def shutdown(self):
        with self._lock:
            reader, self._reader = self._reader, None
        if reader is not None:
            reader.stop()

    def diagnostics(self) -> dict:
        with self._lock:
            wanted = sorted(frozenset().union(*self._wants.values()))
            if not wanted:
                return {}
            text = self._state + (f" ({self._reason})" if self._state == "unavailable" else "")
            return {"Phone sensors": f"{text}, wanted: {', '.join(wanted)}"}

    # ── Keeping the stream in line with what's wanted ─────────────────────

    def _sync(self):
        """Start, restart or stop the reader for what's wanted now, and work out the status."""
        stop = None
        with self._lock:
            before = (self._state, self._reason, self._streaming)
            target = self._target
            wanted = frozenset().union(*self._wants.values())
            kinds: tuple = ()
            if target is None:
                state, reason = "off", ""
            elif not target.phone:
                state, reason = "unavailable", "not_phone"
            elif target.old:
                state, reason = "unavailable", "old_phone"
            elif target.offered is None:
                state, reason = ("starting" if wanted else "off"), ""  # its /v1/state isn't in yet
            else:
                kinds = tuple(k for k in KINDS if k in wanted and k in target.offered)
                if wanted and not kinds:
                    # Orientation needs one of the phone's own cameras streaming, not just its motion sensors
                    camera = "orientation" in wanted and any(k in target.offered for k in ("gravity", "accelerometer"))
                    state, reason = "unavailable", ("camera" if camera else "none")
                else:
                    state, reason = ("starting" if kinds else "off"), ""
            reader = self._reader
            url = f"{target.base}/sensors?kinds={','.join(kinds)}" if kinds and target.base else None
            if reader is not None and (url is None or reader.url != url or reader.auth is not target.auth
                                       or reader.stopped):
                stop, self._reader, reader = reader, None, None
            if url is not None and reader is None:
                reader = self._reader = _Reader(self, url, target.auth, kinds)
                reader.start()
            elif reader is not None:
                state, reason = self._state, self._reason  # the reader says how it's going
            self._streaming = kinds if state == "on" else ()
            self._state, self._reason = state, reason
            if not kinds:
                self._reading = None
            elif self._reading is not None:
                self._reading = _only(self._reading, kinds)
            after = (self._state, self._reason, self._streaming)
            source_id = target.source_id if target is not None else None
        if stop is not None:
            stop.stop()
        if before != after and source_id is not None:
            self._notify(source_id)

    def _notify(self, source_id: str):
        try:
            self._on_change(source_id)
        except Exception:
            logger.exception("Telling plugins about the phone's sensors failed")

    # ── What the reader says (its own thread) ─────────────────────────────

    def _current(self, reader: _Reader) -> Optional[str]:
        """The followed source's id while reader is the one that counts (caller holds the lock)."""
        if reader is not self._reader or self._target is None:
            return None
        return self._target.source_id

    def _reader_hello(self, reader: _Reader, kinds):
        with self._lock:
            source_id = self._current(reader)
            if source_id is None:
                return
            self._state, self._reason = "on", ""
            self._streaming = tuple(k for k in KINDS if isinstance(kinds, list) and k in kinds)
        self._notify(source_id)

    def _reader_line(self, reader: _Reader, line: dict):
        with self._lock:
            source_id = self._current(reader)
            if source_id is None:
                return
            self._reading = apply_line(self._reading or SensorReading(), line, time.monotonic())
        self._notify(source_id)

    def _reader_waiting(self, reader: _Reader):
        self._reader_state(reader, "starting", "")

    def _reader_failed(self, reader: _Reader, reason: str):
        self._reader_state(reader, "unavailable", reason)

    def _reader_state(self, reader: _Reader, state: str, reason: str):
        with self._lock:
            source_id = self._current(reader)
            if source_id is None or (self._state, self._reason) == (state, reason):
                return
            self._state, self._reason = state, reason
            self._streaming = ()
            self._reading = None
        self._notify(source_id)
