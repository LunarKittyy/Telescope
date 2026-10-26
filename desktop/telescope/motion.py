"""Motion in zones of the picture, for the plugins that ask: a small grey copy of each frame compared with a background
that slowly takes in light changes. Plain OpenCV, no Qt here."""
import math
import threading
import time
from dataclasses import dataclass
from typing import Callable, Optional

import cv2
import numpy as np

from telescope import vision

WIDTH = 160  # the grey copy's width
EVERY_S = 0.1  # how often a stream's frames are looked at
ADAPT_S = 2.0  # the background takes in changes with this time constant (light drifting, something left behind)
HOLD_S = 1.0  # a zone counts as moving for this long after it last moved, so a pause in a walk isn't two events
GAP_S = 1.0  # frames this far apart (the panels came back to a stream, it stalled) start the background over
CUT = 0.75  # this share of the whole picture changing at once is a cut (another lens, a zoom), not motion
SENSITIVITY_DEFAULT = 5
# At sensitivity 1 and 10: how many grey levels a pixel has to change by, and what share of a zone has to change
_LEVELS = (40, 12)
_SHARE = (0.05, 0.003)
_MIN_PIXELS = 3


@dataclass(frozen=True)
class _Watch:
    zones: tuple  # ((x0, y0, x1, y1), ...) 0..1 in the picture as shown at zoom 1
    level: float
    share: float


def thresholds(sensitivity: int) -> tuple:
    """(grey levels, share of the zone) something has to change by to count, for a sensitivity of 1 to 10."""
    t = (min(max(int(sensitivity), 1), 10) - 1) / 9
    level = _LEVELS[0] + (_LEVELS[1] - _LEVELS[0]) * t
    share = _SHARE[0] * (_SHARE[1] / _SHARE[0]) ** t
    return level, share


def clean_zones(zones) -> tuple:
    """zones as ((x0, y0, x1, y1), ...) inside 0..1, each corner pair put in order; anything that isn't one is left
    out, and so is a zone with no area."""
    out = []
    for zone in zones or ():
        try:
            x0, y0, x1, y1 = (min(max(float(v), 0.0), 1.0) for v in zone)
        except (TypeError, ValueError):
            continue
        if not all(math.isfinite(v) for v in (x0, y0, x1, y1)):
            continue
        x0, x1 = sorted((x0, x1))
        y0, y1 = sorted((y0, y1))
        if x1 > x0 and y1 > y0:
            out.append((x0, y0, x1, y1))
    return tuple(out)


def grey(frame: np.ndarray, width: int = WIDTH) -> np.ndarray:
    """frame as a small blurred grey copy, width wide."""
    h, w = frame.shape[:2]
    step = max(1, w // (width * 2))
    small = frame[::step, ::step]
    size = (width, max(1, round(h * width / w)))
    small = cv2.resize(small, size, interpolation=cv2.INTER_AREA)
    if small.ndim == 3:
        small = cv2.cvtColor(small, cv2.COLOR_BGR2GRAY)
    return cv2.GaussianBlur(small, (5, 5), 0)


class Detector:
    """One stream's background, and how far each pixel of a frame is from it."""

    def __init__(self):
        self._background: Optional[np.ndarray] = None
        self._at = 0.0

    def change(self, small: np.ndarray, now: float) -> Optional[np.ndarray]:
        """How many grey levels each pixel of small differs from the background (uint8), or None for the first
        frame or a new size. The whole picture getting brighter or darker (the camera's exposure, a cloud) is taken
        out first, and the background then takes in a little of this frame."""
        frame = small.astype(np.float32)
        if self._background is None or self._background.shape != frame.shape:
            self._background, self._at = frame, now
            return None
        background = self._background
        # The median ratio, so a dark coat filling part of the picture doesn't count as the room getting darker
        lit = float(np.median(frame[::2, ::2] / np.maximum(background[::2, ::2], 1.0)))
        diff = cv2.absdiff(frame, background * lit)
        if np.count_nonzero(diff > _LEVELS[0]) > CUT * diff.size:
            self._background, self._at = frame, now
            return None
        a = 1.0 - math.exp(-max(now - self._at, 0.0) / ADAPT_S)
        self._background = background * lit * (1.0 - a) + frame * a
        self._at = now
        return np.minimum(diff, 255).astype(np.uint8)

    def reset(self):
        self._background = None


def moving_zones(change: np.ndarray, zones: tuple, level: float, share: float) -> set:
    """The indexes of the zones (0..1 of change) where enough of change is over level."""
    h, w = change.shape[:2]
    out = set()
    for i, (x0, y0, x1, y1) in enumerate(zones):
        c0, c1 = int(x0 * w), max(int(math.ceil(x1 * w)), int(x0 * w) + 1)
        r0, r1 = int(y0 * h), max(int(math.ceil(y1 * h)), int(y0 * h) + 1)
        part = change[r0:r1, c0:c1]
        if part.size and np.count_nonzero(part > level) >= max(_MIN_PIXELS, share * part.size):
            out.add(i)
    return out


def _to_frame(zones: tuple, to_shown: Optional[Callable]) -> tuple:
    """zones from the picture as shown at zoom 1 onto the upright frame to_shown maps from (the phone's share of the
    zoom moves and scales it, without turning it)."""
    if to_shown is None:
        return zones
    (ax, ay), (bx, by) = to_shown(0.0, 0.0), to_shown(1.0, 1.0)
    sx, sy = bx - ax, by - ay
    if abs(sx) < 1e-9 or abs(sy) < 1e-9:
        return zones
    mapped = [((x0 - ax) / sx, (y0 - ay) / sy, (x1 - ax) / sx, (y1 - ay) / sy) for x0, y0, x1, y1 in zones]
    return tuple(z if z else (0.0, 0.0, 0.0, 0.0) for z in (_clip(m) for m in mapped))


def _clip(zone: tuple) -> Optional[tuple]:
    cleaned = clean_zones([zone])
    return cleaned[0] if cleaned else None


class _Stream:
    def __init__(self):
        self.detector = Detector()
        self.at = 0.0
        self.last_moved: dict = {}  # owner: {zone index: time.monotonic() it last moved}
        self.moving: dict = {}  # owner: [zone indexes], as last reported


class Motion:
    """Watches the shown stream's frames for the owners that ask; every call is safe from its own thread."""

    def __init__(self, on_change: Callable[[object, str, list], None]):
        self._on_change = on_change  # (stream key, owner, [zone indexes]) from the stream's worker thread
        self._watches: dict = {}  # owner: _Watch, swapped whole (read on worker threads)
        self._streams: dict = {}
        self._lock = threading.Lock()
        self._ms: list = []

    def watch(self, owner: str, zones, sensitivity: int = SENSITIVITY_DEFAULT):
        """Look for motion in zones for owner, or stop with no zones. Each call starts owner over (nothing moving)."""
        zones = clean_zones(zones)
        watches = dict(self._watches)
        if zones:
            watches[owner] = _Watch(zones, *thresholds(sensitivity))
        else:
            watches.pop(owner, None)
        with self._lock:
            for sv in self._streams.values():
                sv.moving.pop(owner, None)
                sv.last_moved.pop(owner, None)
                if not self._watches:
                    sv.detector.reset()  # nobody watched: the background is from long ago
        self._watches = watches

    def reset(self):
        """Every stream's background starts over (the picture turned, so everything is somewhere else)."""
        with self._lock:
            for sv in self._streams.values():
                sv.detector.reset()

    def watching(self) -> bool:
        return bool(self._watches)

    def moving(self, key, owner: str) -> list:
        """The zones something moved in lately on key's stream, for owner."""
        with self._lock:
            sv = self._streams.get(key)
            return list(sv.moving.get(owner, [])) if sv is not None else []

    def feed(self, frame: np.ndarray, prepare: Optional[Callable] = None, to_shown: Optional[Callable] = None,
             now: Optional[float] = None):
        """A stream's frame before the view (worker thread): one every EVERY_S is looked at while anyone watches.
        prepare and to_shown as for Vision.feed_raw."""
        watches = self._watches
        here = vision.current_frame.get()
        if not watches or here is None:
            return
        key = here[0]
        now = time.monotonic() if now is None else now
        with self._lock:
            sv = self._streams.setdefault(key, _Stream())
        if now - sv.at < EVERY_S:
            return
        if now - sv.at > GAP_S:
            sv.detector.reset()
        sv.at = now
        t0 = time.perf_counter()
        small = grey(frame)
        if prepare is not None:
            small = prepare(small)
        change = sv.detector.change(small, now)
        if change is None:
            return
        reports = []
        with self._lock:
            for owner, watch in watches.items():
                last = sv.last_moved.setdefault(owner, {})
                for i in moving_zones(change, _to_frame(watch.zones, to_shown), watch.level, watch.share):
                    last[i] = now
                moving = sorted(i for i, at in last.items() if now - at < HOLD_S and i < len(watch.zones))
                if moving != sv.moving.get(owner, []):
                    sv.moving[owner] = moving
                    reports.append((owner, moving))
            for owner in [o for o in sv.moving if o not in watches]:
                del sv.moving[owner]
                sv.last_moved.pop(owner, None)
        self._ms.append((time.perf_counter() - t0) * 1000)
        del self._ms[:-60]
        for owner, moving in reports:
            self._on_change(key, owner, moving)

    def drop_stream(self, key):
        with self._lock:
            self._streams.pop(key, None)

    def diagnostics(self) -> dict:
        if not self._watches or not self._ms:
            return {}
        return {"Motion": f"{len(self._watches)} watching, {sorted(self._ms)[len(self._ms) // 2]:.1f} ms"}
