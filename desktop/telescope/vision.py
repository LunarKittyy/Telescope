"""Person masks and people's positions from MediaPipe, once per stream, for the plugins that ask. No Qt here."""
import contextvars
import logging
import os
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

from telescope import dev_profile

logger = logging.getLogger(__name__)

SEGMENTER_MODEL = "selfie_segmenter_landscape.tflite"
POSE_MODEL = "pose_landmarker_lite.task"
_PEOPLE_EVERY_S = 0.08  # how often people are looked for (about 12 a second); framing smooths in between
_PEOPLE_WIDTH = 640  # the size the pose model gets the frame at
_MAX_PEOPLE = 3
_MASK_CARRY = 0.3  # how much of the last frame's mask carries into the next
_LOOK_AGAIN_S = 5.0  # without a pack, how often asking looks for one again (a download may have finished)

# Which stream the frame being processed comes from, and its number: set by StreamWorker around the plugin steps.
current_frame: contextvars.ContextVar = contextvars.ContextVar("current_frame", default=None)

# Pose landmarks we use, by their index in MediaPipe's 33.
_POINTS = {
    "nose": 0, "left_eye": 2, "right_eye": 5, "left_ear": 7, "right_ear": 8,
    "left_shoulder": 11, "right_shoulder": 12, "left_elbow": 13, "right_elbow": 14, "left_wrist": 15, "right_wrist": 16,
    "left_pinky": 17, "right_pinky": 18, "left_index": 19, "right_index": 20, "left_thumb": 21, "right_thumb": 22,
    "left_hip": 23, "right_hip": 24,
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


def vision_dir() -> Path:
    """Where vision packs are unpacked, one folder per app version."""
    if dev_profile.active():
        return dev_profile.folder() / "vision"
    if sys.platform == "win32":
        base = os.environ.get("LOCALAPPDATA") or str(Path.home())
        return Path(base) / "Telescope" / "vision"
    base = os.environ.get("XDG_DATA_HOME") or str(Path.home() / ".local" / "share")
    return Path(base) / "telescope" / "vision"


def pack_dir() -> Path:
    """The pack this version uses (vision_pack.json): site/ holds MediaPipe, models/ the two models."""
    from telescope import vision_pack
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


def find_backend():
    """MediaPipe from this version's pack (checked when it was unpacked); FileNotFoundError when it isn't there."""
    from telescope import vision_pack
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


class _StreamVision:
    """One stream's tasks (VIDEO mode keeps tracking state, so streams don't share them) and its people thread."""

    def __init__(self):
        self.segment = self.close_segmenter = None
        self.mask_clock = _Timestamps()
        self.mask_seq = None  # the frame the cached mask is for
        self.mask = None
        self.people_frame = None  # the newest (small frame, prepare, to_shown) waiting for the pose model
        self.people_at = 0.0  # time.monotonic() a frame was last handed over
        self.people_wake = threading.Event()
        self.people_thread: Optional[threading.Thread] = None
        self.stopping = False


class Vision:
    """Loads MediaPipe on first use and runs it for each stream; every call is safe before it's ready (None, [])."""

    def __init__(self, on_people: Callable[[object, list], None], on_state: Callable[[str], None],
                 find: Callable[[], object] = find_backend):
        self._on_people = on_people  # (stream key, [Person]) from a people thread
        self._on_state = on_state  # state() changed, from whichever thread changed it
        self._find = find
        self._backend = None
        self._state = OFF
        self._problem = ""
        self._lock = threading.Lock()
        self._streams: dict = {}
        self._looked_at = -_LOOK_AGAIN_S  # time.monotonic() of the last load() that found no pack
        self._people_wanted: set = set()
        self._ms = {"mask": [], "people": []}  # recent timings, for diagnostics

    # ── Loading ───────────────────────────────────────────────────────────

    def state(self) -> str:
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
        if sv.mask_seq == seq and sv.mask is not None and sv.mask.shape == frame.shape[:2]:
            return sv.mask
        if sv.segment is None:
            try:
                sv.segment, sv.close_segmenter = self._backend.segmenter()
            except Exception as exc:
                logger.exception("Couldn't make the person segmenter")
                self._fail(exc)  # trying again on every frame would only fill the log
                return None
        t0 = time.perf_counter()
        mask = sv.segment(frame, sv.mask_clock.next())
        if sv.mask is not None and sv.mask.shape == mask.shape:
            mask = cv2.addWeighted(mask, 1.0 - _MASK_CARRY, sv.mask, _MASK_CARRY, 0)  # less flicker at the edges
        self._note("mask", t0)
        sv.mask_seq, sv.mask = seq, mask
        return mask

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
        """A stream's frame before the view; one every _PEOPLE_EVERY_S goes to its people thread, the rest are skipped.

        prepare(frame) turns the small copy upright for the model (on that thread), and to_shown(x, y) takes a point
        from it to the picture as shown at zoom 1.
        """
        here = current_frame.get()
        if not self._people_wanted or self._state != READY or here is None:
            return
        sv = self._stream(here[0])
        now = time.monotonic()
        if now - sv.people_at < _PEOPLE_EVERY_S:
            return
        sv.people_at = now
        h, w = frame.shape[:2]
        width = min(_PEOPLE_WIDTH, w)
        small = cv2.resize(frame, (width, max(1, round(h * width / w))), interpolation=cv2.INTER_AREA)
        sv.people_frame = (small, prepare, to_shown)
        if sv.people_thread is None:
            sv.people_thread = threading.Thread(target=self._people_loop, args=(here[0], sv), daemon=True,
                                                name="vision-people")
            sv.people_thread.start()
        sv.people_wake.set()

    def _people_loop(self, key, sv: _StreamVision):
        try:
            detect, close = self._backend.pose()
        except Exception as exc:
            logger.exception("Couldn't make the pose model")
            self._fail(exc)
            return
        clock = _Timestamps()
        try:
            while not sv.stopping:
                sv.people_wake.wait()
                sv.people_wake.clear()
                waiting, sv.people_frame = sv.people_frame, None
                if sv.stopping or waiting is None:
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

    # ── Streams ───────────────────────────────────────────────────────────

    def _stream(self, key) -> _StreamVision:
        sv = self._streams.get(key)
        if sv is None:
            sv = self._streams[key] = _StreamVision()
        return sv

    def drop_stream(self, key):
        """A stream ended: its tasks close and its people thread ends."""
        sv = self._streams.pop(key, None)
        if sv is None:
            return
        sv.stopping = True
        sv.people_wake.set()
        if sv.close_segmenter is not None:
            try:
                sv.close_segmenter()
            except Exception:
                logger.exception("Closing the person segmenter failed")

    def shutdown(self):
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
        return {"Vision": line}
