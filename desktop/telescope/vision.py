"""Person masks and people's positions from MediaPipe, once per stream, for the plugins that ask. No Qt here."""
import contextvars
import logging
import re
import sys
import threading
import time
import types
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

import cv2
import numpy as np

from telescope import vision_pack
from telescope.vision_pack import POSE_MODEL, SEGMENTER_MODEL

logger = logging.getLogger(__name__)

# How often people are looked for while something moves (about 12 a second); framing smooths in between
_PEOPLE_EVERY_S = 0.08
_PEOPLE_STILL_EVERY_S = 0.33  # and while nothing does
_PEOPLE_WIDTH = 640  # the size the pose model gets the frame at
_MAX_PEOPLE = 3
_MASK_CARRY = 0.3  # how much of the last frame's mask carries into the next
_MASK_CARRY_STILL = 0.5  # and while nothing moves: steadier edges, and no movement to lag behind
_MASK_STILL_EVERY_S = 1 / 15  # while nothing moves the mask is worked out this often, instead of for every frame
_MODEL_SIZE = (256, 144)  # the segmenter's input (selfie_segmenter_landscape): MediaPipe shrinks anything bigger to it
# Moving: more than this share of a tiny grey copy changed by more than this many levels since the frame last used
_MOVED_SHARE = 0.005
_MOVED_LEVELS = 12
_THUMB = (64, 36)
# Power-saving effects (Settings > Video): the mask at most this often even while something moves, people this often
_SAVING_PEOPLE_EVERY_S = 0.16
_SAVING_PEOPLE_STILL_EVERY_S = 0.5
_saving = False
_LOOK_AGAIN_S = 5.0  # without a pack, how often asking looks for one again (a download may have finished)
# A model nothing has asked for this long is closed and its memory given back (Background or Auto framing switched
# off). Switched back on sooner, it's still there, so flipping a switch back and forth doesn't load it again each time.
_IDLE_CLOSE_S = 10.0
_TIDY_EVERY_S = 2.0

# Which stream the frame being processed comes from, and its number: set by StreamWorker around the plugin steps.
current_frame: contextvars.ContextVar = contextvars.ContextVar("current_frame", default=None)

# Pose landmarks we use, by their index in MediaPipe's 33.
_POINTS = {
    "nose": 0, "left_eye": 2, "right_eye": 5, "left_ear": 7, "right_ear": 8,
    "left_shoulder": 11, "right_shoulder": 12, "left_elbow": 13, "right_elbow": 14, "left_wrist": 15, "right_wrist": 16,
    "left_pinky": 17, "right_pinky": 18, "left_index": 19, "right_index": 20, "left_thumb": 21, "right_thumb": 22,
    "left_hip": 23, "right_hip": 24, "left_knee": 25, "right_knee": 26, "left_ankle": 27, "right_ankle": 28,
}

_MISSING_LIBRARY = re.compile(r"^(lib[\w.+-]+\.so[\w.]*): cannot open shared object file")

# What state() reports
OFF, LOADING, READY, MISSING, FAILED = "off", "loading", "ready", "missing", "failed"


@dataclass(frozen=True)
class Person:
    """One person the pose model found: name -> (x, y, visibility), x and y 0..1 of the picture as shown at zoom 1
    (upright, flipped the way the view is)."""
    points: dict

    def mapped(self, to: Callable) -> "Person":
        """The same person with each (x, y) put through to(x, y)."""
        return Person({name: (*to(x, y), seen) for name, (x, y, seen) in self.points.items()})

    def point(self, name: str, min_visibility: float = 0.5) -> Optional[tuple]:
        """(x, y) of a landmark, or None when the model isn't sure it's in the picture."""
        p = self.points.get(name)
        return (p[0], p[1]) if p is not None and p[2] >= min_visibility else None


vision_dir = vision_pack.default_dir


def pack_dir() -> Path:
    """The pack this version uses (vision_pack.json): site/ holds MediaPipe, models/ the two models."""
    return vision_dir() / (vision_pack.pack_id() or "none")


def _stub_matplotlib():
    # MediaPipe's drawing helpers import matplotlib at the top though nothing we call uses it
    try:
        import matplotlib.pyplot  # noqa: F401
    except ImportError:
        mpl, pyplot = types.ModuleType("matplotlib"), types.ModuleType("matplotlib.pyplot")
        mpl.pyplot = pyplot
        sys.modules.setdefault("matplotlib", mpl)
        sys.modules.setdefault("matplotlib.pyplot", pyplot)


class MediaPipeBackend:
    """Makes MediaPipe's tasks; the models are read into memory (a path with non-ASCII characters fails on Windows)."""

    def __init__(self, folder: Path):
        site = folder / "site"
        if site.is_dir() and str(site) not in sys.path:
            sys.path.insert(0, str(site))
        _stub_matplotlib()
        import mediapipe as mp
        from mediapipe.tasks.python import vision
        from mediapipe.tasks.python.core.base_options import BaseOptions
        self._mp, self._vision, self._options = mp, vision, BaseOptions
        self._segmenter_model = (folder / "models" / SEGMENTER_MODEL).read_bytes()
        self._pose_model = (folder / "models" / POSE_MODEL).read_bytes()
        self.version = getattr(mp, "__version__", "?")
        _segment, close = self.segmenter()  # loads the native library now, not on the first frame
        close()

    def segmenter(self):
        v = self._vision
        task = v.ImageSegmenter.create_from_options(v.ImageSegmenterOptions(
            base_options=self._options(model_asset_buffer=self._segmenter_model), running_mode=v.RunningMode.VIDEO,
            output_confidence_masks=True, output_category_mask=False))
        image = self._image

        def segment(frame, ts_ms):
            mask = task.segment_for_video(image(frame), ts_ms).confidence_masks[0].numpy_view()
            return cv2.convertScaleAbs(mask, alpha=255.0)
        return segment, task.close

    def pose(self):
        v = self._vision
        task = v.PoseLandmarker.create_from_options(v.PoseLandmarkerOptions(
            base_options=self._options(model_asset_buffer=self._pose_model), running_mode=v.RunningMode.VIDEO,
            num_poses=_MAX_PEOPLE))
        image = self._image

        def detect(frame, ts_ms):
            found = task.detect_for_video(image(frame), ts_ms).pose_landmarks
            return [Person({name: (lm[i].x, lm[i].y, lm[i].visibility) for name, i in _POINTS.items()})
                    for lm in found]
        return detect, task.close

    def _image(self, frame):
        return self._mp.Image(image_format=self._mp.ImageFormat.SRGB, data=cv2.cvtColor(frame, cv2.COLOR_BGR2RGB))


def pack_there() -> bool:
    """This version's pack is unpacked and checked (cheap: nothing is imported)."""
    return vision_pack.installed(vision_dir())


def find_backend():
    """MediaPipe from this version's pack (checked when it was unpacked); FileNotFoundError when it isn't there."""
    if not vision_pack.installed(vision_dir()):
        raise FileNotFoundError(f"no vision pack in {pack_dir()}")
    return MediaPipeBackend(pack_dir())


class _Timestamps:
    """VIDEO mode wants each task's timestamps to go up, strictly."""

    def __init__(self):
        self._last = -1

    def next(self) -> int:
        self._last = max(self._last + 1, int(time.monotonic() * 1000))
        return self._last


def _give_back_memory():
    """Linux: glibc keeps memory a closed model freed for later; hand it back to the system."""
    if sys.platform.startswith("linux"):
        from telescope.platform.linux import release_freed_memory
        release_freed_memory()


def set_power_saving(on: bool):
    """Power-saving effects (Settings > Video): the models run less often, so blur and framing follow quick moves a
    little later but use less of the processor. Takes effect from the next frame."""
    global _saving
    _saving = bool(on)


def power_saving() -> bool:
    return _saving


def model_input(frame: np.ndarray, size: tuple = _MODEL_SIZE) -> np.ndarray:
    """frame shrunk to just cover size, keeping its shape. A linear step to twice that and an area step from there
    looks like one area step at a fraction of the cost (that one is slow at uneven ratios such as 1080p's 7.5)."""
    h, w = frame.shape[:2]
    scale = max(size[0] / w, size[1] / h)
    if scale >= 1:
        return frame
    out = (max(1, round(w * scale)), max(1, round(h * scale)))
    if out[0] * 2 < w and out[1] * 2 < h:
        frame = cv2.resize(frame, (out[0] * 2, out[1] * 2), interpolation=cv2.INTER_LINEAR)
    return cv2.resize(frame, out, interpolation=cv2.INTER_AREA)


class _Motion:
    """Whether frames still look like the one last used, from a tiny grey copy (cheap enough for every frame)."""

    def __init__(self):
        self._used = None
        self._now = None

    def moved(self, frame: np.ndarray) -> bool:
        step = max(1, frame.shape[1] // (_THUMB[0] * 4))
        small = cv2.resize(frame[::step, ::step], _THUMB, interpolation=cv2.INTER_AREA)
        self._now = cv2.cvtColor(small, cv2.COLOR_BGR2GRAY) if small.ndim == 3 else small
        if self._used is None:
            return True
        changed = cv2.absdiff(self._now, self._used) > _MOVED_LEVELS
        return np.count_nonzero(changed) > _MOVED_SHARE * changed.size

    def used(self):
        """The frame moved() last looked at is the one used: later frames are compared with it."""
        self._used = self._now


class _StreamVision:
    """One stream's tasks (VIDEO mode keeps tracking state, so streams don't share them) and its people thread."""

    def __init__(self):
        self.segment = self.close_segmenter = None
        self.mask_clock = _Timestamps()
        self.mask_seq = None  # the frame the cached mask is for
        self.mask = None  # at the frame's size
        self.small_mask = None  # as the model gave it, with the last ones carried in
        self.mask_at = 0.0  # time.monotonic() the model last ran
        self.mask_motion = _Motion()
        self.people_frame = None  # the newest (small frame, prepare, to_shown) waiting for the pose model
        self.people_at = 0.0  # time.monotonic() a frame was last handed over
        self.people_motion = _Motion()
        self.people_wake = threading.Event()
        self.people_thread: Optional[threading.Thread] = None
        self.people_quit: Optional[threading.Event] = None  # set: that people thread closes its model and ends
        self.stopping = False
        self.lock = threading.Lock()  # the segmenter isn't closed while a frame is using it
        self.mask_asked = 0.0  # time.monotonic() a plugin last asked for a mask
        self.people_fed = 0.0  # and last fed a frame for people


class Vision:
    """Loads MediaPipe on first use and runs it for each stream; every call is safe before it's ready (None, [])."""

    def __init__(self, on_people: Callable[[object, list], None], on_state: Callable[[str], None],
                 find: Callable[[], object] = find_backend, there: Optional[Callable[[], bool]] = None):
        self._on_people = on_people  # (stream key, [Person]) from a people thread
        self._on_state = on_state  # state() changed, from whichever thread changed it
        self._find = find
        self._there = there or (pack_there if find is find_backend else (lambda: True))
        self._backend = None
        self._state = OFF
        self._problem = ""
        self._lock = threading.Lock()
        self._streams: dict = {}
        self._looked_at = -_LOOK_AGAIN_S  # time.monotonic() of the last load() that found no pack
        self._people_wanted: set = set()
        self._ms = {"mask": [], "people": []}  # recent timings, for diagnostics
        self._tidier: Optional[threading.Thread] = None
        self._closed = threading.Event()

    # ── Loading ───────────────────────────────────────────────────────────

    def state(self) -> str:
        """OFF until something asks for it, then LOADING and READY; MISSING (without loading) when there's no pack."""
        if self._state == OFF and not self._there():
            return MISSING
        return self._state

    def problem(self) -> str:
        """What went wrong when it failed (the error's type; the log has the rest)."""
        return self._problem

    def load(self):
        """Start loading MediaPipe in the background, once; after no pack was found, again every _LOOK_AGAIN_S."""
        with self._lock:
            if self._state in (LOADING, READY, FAILED):
                return
            if self._state == MISSING and time.monotonic() - self._looked_at < _LOOK_AGAIN_S:
                return
            self._looked_at = time.monotonic()
            self._set_state(LOADING)
        threading.Thread(target=self._load, daemon=True, name="vision-load").start()

    def _fail(self, exc: Exception):
        missing = _MISSING_LIBRARY.match(str(exc)) if isinstance(exc, OSError) else None
        # Only the type and a missing library's name: the message can hold a path with the user's name in it
        self._problem = f"{type(exc).__name__}: {missing.group(1)} missing" if missing else type(exc).__name__
        self._set_state(FAILED)

    def _load(self):
        try:
            backend = self._find()
        except FileNotFoundError as exc:
            logger.info("Vision isn't there: %s", exc)
            self._problem = ""
            self._set_state(MISSING)
            return
        except Exception as exc:
            logger.exception("MediaPipe didn't load")
            self._fail(exc)
            return
        self._backend = backend
        logger.info("MediaPipe %s loaded", getattr(backend, "version", "?"))
        self._set_state(READY)

    def _set_state(self, state: str):
        self._state = state
        try:
            self._on_state(state)
        except Exception:
            logger.exception("Vision state callback failed")

    # ── Person masks (worker thread, inside the plugin steps) ─────────────

    def person_mask(self, frame: np.ndarray) -> Optional[np.ndarray]:
        """How sure the model is each pixel is a person (uint8, frame's size), or None when it can't say yet."""
        here = current_frame.get()
        if self._state != READY:
            self.load()
            return None
        if here is None:
            return None
        key, seq = here
        sv = self._stream(key)
        with sv.lock:
            sv.mask_asked = time.monotonic()
            return self._mask(sv, frame, seq)

    def _mask(self, sv: _StreamVision, frame: np.ndarray, seq) -> Optional[np.ndarray]:
        if sv.mask_seq == seq and sv.mask is not None and sv.mask.shape == frame.shape[:2]:
            return sv.mask
        if sv.segment is None:
            try:
                sv.segment, sv.close_segmenter = self._backend.segmenter()
            except Exception as exc:
                logger.exception("Couldn't make the person segmenter")
                self._fail(exc)  # trying again on every frame would only fill the log
                return None
        small = model_input(frame)
        now = time.monotonic()
        # Nothing moving: the last mask still fits, so the model runs less often (anything moving brings it back)
        moved = sv.mask_motion.moved(small)
        if (moved and not _saving) or sv.small_mask is None or now - sv.mask_at >= _MASK_STILL_EVERY_S:
            t0 = time.perf_counter()
            mask = sv.segment(small, sv.mask_clock.next())
            if sv.small_mask is not None and sv.small_mask.shape == mask.shape:
                carry = _MASK_CARRY if moved else _MASK_CARRY_STILL  # less flicker at the edges
                mask = cv2.addWeighted(mask, 1.0 - carry, sv.small_mask, carry, 0)
            self._note("mask", t0)
            sv.mask_motion.used()
            sv.small_mask, sv.mask_at, sv.mask = mask, now, None
        h, w = frame.shape[:2]
        if sv.mask is None or sv.mask.shape != (h, w):
            sv.mask = sv.small_mask if sv.small_mask.shape == (h, w) else cv2.resize(sv.small_mask, (w, h),
                                                                                    interpolation=cv2.INTER_LINEAR)
        sv.mask_seq = seq
        return sv.mask

    # ── People (a thread per stream) ──────────────────────────────────────

    def want_people(self, owner: str, on: bool):
        if on:
            self._people_wanted.add(owner)
            self.load()
        else:
            self._people_wanted.discard(owner)

    def wants_people(self) -> bool:
        return bool(self._people_wanted)

    def feed_raw(self, frame: np.ndarray, prepare: Optional[Callable] = None, to_shown: Optional[Callable] = None):
        """A stream's frame before the view; one every _PEOPLE_EVERY_S goes to its people thread while something moves,
        one every _PEOPLE_STILL_EVERY_S while nothing does, and the rest are skipped.

        prepare(frame) turns the small copy upright for the model (on that thread), and to_shown(x, y) takes a point
        from it to the picture as shown at zoom 1.
        """
        here = current_frame.get()
        if not self._people_wanted or here is None:
            return
        if self._state != READY:
            self.load()
            return
        sv = self._stream(here[0])
        now = time.monotonic()
        sv.people_fed = now
        every, still_every = ((_SAVING_PEOPLE_EVERY_S, _SAVING_PEOPLE_STILL_EVERY_S) if _saving
                              else (_PEOPLE_EVERY_S, _PEOPLE_STILL_EVERY_S))
        if now - sv.people_at < every:
            return
        if not sv.people_motion.moved(frame) and now - sv.people_at < still_every:
            return
        sv.people_motion.used()
        sv.people_at = now
        h, w = frame.shape[:2]
        width = min(_PEOPLE_WIDTH, w)
        small = cv2.resize(frame, (width, max(1, round(h * width / w))), interpolation=cv2.INTER_AREA)
        sv.people_frame = (small, prepare, to_shown)
        if sv.people_thread is None:
            sv.people_quit = threading.Event()
            sv.people_thread = threading.Thread(target=self._people_loop, args=(here[0], sv, sv.people_quit),
                                                daemon=True, name="vision-people")
            sv.people_thread.start()
        sv.people_wake.set()

    def _people_loop(self, key, sv: _StreamVision, done: threading.Event):
        try:
            detect, close = self._backend.pose()
        except Exception as exc:
            logger.exception("Couldn't make the pose model")
            self._fail(exc)
            return
        clock = _Timestamps()
        try:
            while not (sv.stopping or done.is_set()):
                sv.people_wake.wait()
                sv.people_wake.clear()
                if sv.stopping or done.is_set():
                    break
                waiting, sv.people_frame = sv.people_frame, None
                if waiting is None:
                    continue
                frame, prepare, to_shown = waiting
                t0 = time.perf_counter()
                try:
                    people = detect(prepare(frame) if prepare else frame, clock.next())
                    if to_shown:
                        people = [person.mapped(to_shown) for person in people]
                except Exception:
                    logger.exception("Looking for people failed; trying the next frame")
                    continue
                self._note("people", t0)
                self._on_people(key, people)
        finally:
            close()
            if done.is_set():
                _give_back_memory()

    # ── Streams ───────────────────────────────────────────────────────────

    def _stream(self, key) -> _StreamVision:
        sv = self._streams.get(key)
        if sv is None:
            sv = self._streams[key] = _StreamVision()
            if self._tidier is None:
                self._tidier = threading.Thread(target=self._tidy_loop, daemon=True, name="vision-tidy")
                self._tidier.start()
        return sv

    def _tidy_loop(self):
        while not self._closed.wait(_TIDY_EVERY_S):
            try:
                self.tidy()
            except Exception:
                logger.exception("Closing unused vision models failed")

    def tidy(self, now: Optional[float] = None):
        """Close the models no plugin has asked for in _IDLE_CLOSE_S, and give their memory back. The next ask makes
        them again, which takes a moment."""
        now = time.monotonic() if now is None else now
        freed = False
        for sv in list(self._streams.values()):
            with sv.lock:
                if sv.segment is not None and now - sv.mask_asked >= _IDLE_CLOSE_S:
                    close = sv.close_segmenter
                    sv.segment = sv.close_segmenter = None
                    sv.mask = sv.small_mask = sv.mask_seq = None
                    sv.mask_motion = _Motion()
                    try:
                        close()
                    except Exception:
                        logger.exception("Closing the person segmenter failed")
                    freed = True
            if sv.people_thread is not None and now - sv.people_fed >= _IDLE_CLOSE_S:
                sv.people_quit.set()  # the thread closes the pose model and gives its memory back on the way out
                sv.people_wake.set()
                sv.people_thread = sv.people_frame = None
                sv.people_motion, sv.people_at = _Motion(), 0.0
        if freed:
            _give_back_memory()

    def drop_stream(self, key):
        """A stream ended: its tasks close and its people thread ends."""
        sv = self._streams.pop(key, None)
        if sv is None:
            return
        sv.stopping = True
        sv.people_wake.set()
        with sv.lock:
            if sv.close_segmenter is not None:
                try:
                    sv.close_segmenter()
                except Exception:
                    logger.exception("Closing the person segmenter failed")
                sv.segment = sv.close_segmenter = None

    def shutdown(self):
        self._closed.set()
        for key in list(self._streams):
            self.drop_stream(key)

    # ── Diagnostics ───────────────────────────────────────────────────────

    def _note(self, kind: str, t0: float):
        recent = self._ms[kind]
        recent.append((time.perf_counter() - t0) * 1000)
        del recent[:-60]

    def diagnostics(self) -> dict:
        if self._state == OFF:
            return {}
        line = {READY: f"ready (MediaPipe {getattr(self._backend, 'version', '?')})", LOADING: "loading",
                MISSING: "not downloaded", FAILED: f"didn't load ({self._problem})"}[self._state]
        for kind, recent in self._ms.items():
            if recent:
                line += f", {kind} {sorted(recent)[len(recent) // 2]:.1f} ms"
        if _saving:
            line += ", power saving"
        return {"Vision": line}
