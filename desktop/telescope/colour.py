"""The colour pipeline: every colour change plugins ask for, baked into one LUT per stream and applied to its frames
with OpenCV, plus reading and writing .cube LUT files. MODULES.md has why it runs on the processor."""
import logging
import os
import re
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

import cv2
import numpy as np

from telescope.i18n import N_
from telescope.plugin import TelescopePlugin

logger = logging.getLogger(__name__)

SIZES = (33, 65)
"""The 3D LUT sizes the pipeline bakes at: points per side."""
MAX_CUBE_SIZE = 65
_KEEP_BAKED = 8  # composed LUTs kept per pipeline, for streams with different stages and quick back and forth
_SPLIT_LINE = (235, 235, 235)
# Bands of a big frame run side by side, as main.py holds OpenCV to 2 threads (1080p took 12 ms that way, 5 in four)
_BANDS = max(1, min(4, (os.cpu_count() or 2) // 2))
_BAND_ROWS = 360  # frames shorter than this aren't worth cutting up
_pool: Optional[ThreadPoolExecutor] = None
_pool_lock = threading.Lock()


def _bands() -> ThreadPoolExecutor:
    global _pool
    with _pool_lock:
        if _pool is None:
            _pool = ThreadPoolExecutor(_BANDS, thread_name_prefix="colour-lut")
        return _pool


@dataclass(frozen=True, eq=False)
class ColourStage:
    """One step of a stream's colour pipeline (host.set_colour_stage). fn takes an (n, 3) float32 array of RGB values
    0..1 and returns the changed values in the same shape, without changing its input. It runs off the Qt thread."""

    fn: Callable[[np.ndarray], np.ndarray]
    per_channel: bool = False
    """Each channel's output depends only on that channel's input (curves, gains): a pipeline of only such stages is
    applied as exact 256-entry tables instead of a 3D LUT."""
    size: int = 33
    """Points per side of the 3D LUT it needs to come through accurately: 33, or 65 for a detailed LUT file."""


# ── Applying a LUT to frames ──────────────────────────────────────────────

_tables: dict = {}


def _remap_tables(n: int) -> tuple:
    """Per input level (0..255): x of its blue slice in the tile, the blend weight to the next slice, and its position
    in red or green, all in lattice units."""
    if n not in _tables:
        v = np.arange(256, dtype=np.float32) * ((n - 1) / 255)
        b0 = np.minimum(np.floor(v), n - 2)
        w = (v - b0).astype(np.float32)
        _tables[n] = ((b0 * n).astype(np.float32), w, (1 - w).astype(np.float32), v.astype(np.float32))
    return _tables[n]


def _tile(cube: np.ndarray, dtype) -> np.ndarray:
    """A cube indexed [b][g][r] laid out as one picture for cv2.remap: y is green, x is blue * n + red."""
    n = cube.shape[0]
    return np.ascontiguousarray(cube.transpose(1, 0, 2, 3).reshape(n, n * n, cube.shape[3]).astype(dtype))


class Lut:
    """A baked colour transform for uint8 BGR frames: three 256-entry tables, or a 3D lattice applied trilinearly.
    cv2.remap does the red and green part on two blue slices and blendLinear mixes them, all in float32 so the lattice
    isn't rounded to whole levels before it's interpolated (that moved an identity LUT's colours by a level)."""

    def __init__(self, table: Optional[np.ndarray] = None, cube: Optional[np.ndarray] = None):
        """table: (256, 3) RGB values 0..1 per input level; cube: (n, n, n, 3) indexed [b][g][r], RGB values 0..1."""
        self.table = self.cube = None
        if table is not None:
            rgb = np.clip(np.asarray(table, np.float32), 0, 1)
            self.table = (rgb[:, ::-1] * 255 + 0.5).astype(np.uint8).reshape(1, 256, 3)
        if cube is not None:
            rgb = np.clip(np.asarray(cube, np.float32), 0, 1)
            self.size = rgb.shape[0]
            self._tile = _tile(rgb[..., ::-1] * 255, np.float32)

    @property
    def kind(self) -> str:
        return "1D" if self.table is not None else f"3D {self.size}"

    def apply(self, frame: np.ndarray) -> np.ndarray:
        """A new frame with the transform applied (frame itself is never changed)."""
        if self.table is not None:
            return cv2.LUT(frame, self.table)
        h = frame.shape[0]
        if _BANDS < 2 or h < _BAND_ROWS:
            return self._apply_3d(frame)
        out = np.empty_like(frame)
        edges = [h * i // _BANDS for i in range(_BANDS + 1)]

        def band(i):
            out[edges[i]:edges[i + 1]] = self._apply_3d(frame[edges[i]:edges[i + 1]])
        for done in [_bands().submit(band, i) for i in range(_BANDS)]:
            done.result()
        return out

    def _apply_3d(self, frame: np.ndarray) -> np.ndarray:
        n = self.size
        slice_x, w, w1, pos = _remap_tables(n)
        b, g, r = cv2.split(frame)
        x = cv2.add(cv2.LUT(b, slice_x), cv2.LUT(r, pos))
        y = cv2.LUT(g, pos)
        low = cv2.remap(self._tile, x, y, cv2.INTER_LINEAR)
        high = cv2.remap(self._tile, cv2.add(x, float(n)), y, cv2.INTER_LINEAR)
        return cv2.convertScaleAbs(cv2.blendLinear(low, high, cv2.LUT(b, w1), cv2.LUT(b, w)))  # rounds to uint8


def lattice(n: int) -> np.ndarray:
    """Every point of an n-point 3D LUT as (n**3, 3) RGB values 0..1, in [b][g][r] order (red changes fastest)."""
    g = np.linspace(0, 1, n, dtype=np.float32)
    b, gg, r = np.meshgrid(g, g, g, indexing="ij")
    return np.stack([r, gg, b], -1).reshape(-1, 3)


def sample_cube(cube: np.ndarray, rgb: np.ndarray) -> np.ndarray:
    """cube ([b][g][r], RGB out) at rgb ((m, 3), 0..1 per channel, clamped), interpolated trilinearly."""
    n = cube.shape[0]
    pos = np.clip(np.asarray(rgb, np.float32), 0, 1) * (n - 1)
    lo = np.minimum(pos.astype(np.int32), n - 2)
    t = pos - lo
    r0, g0, b0 = lo[:, 0], lo[:, 1], lo[:, 2]
    tr, tg, tb = t[:, 0:1], t[:, 1:2], t[:, 2:3]
    flat = cube.reshape(-1, 3)
    base = (b0 * n + g0) * n + r0
    out = np.zeros_like(pos)
    for db, wb in ((0, 1 - tb), (1, tb)):
        for dg, wg in ((0, 1 - tg), (1, tg)):
            for dr, wr in ((0, 1 - tr), (1, tr)):
                out += flat[base + (db * n + dg) * n + dr] * (wb * wg * wr)
    return out


def _run(stage: ColourStage, rgb: np.ndarray) -> np.ndarray:
    out = np.asarray(stage.fn(rgb.copy()), dtype=np.float32)
    if out.shape != rgb.shape:
        raise ValueError(f"a colour stage returned {out.shape} for {rgb.shape}")
    return np.clip(np.nan_to_num(out), 0, 1)


def bake(stages, size: Optional[int] = None) -> Optional[Lut]:
    """The stages one after the other as one Lut (None for none): exact tables when every stage is per channel, else
    a 3D LUT with the most points any stage asks for (or size). A stage that fails is left out and logged."""
    stages = list(stages)
    if not stages:
        return None
    flat = all(s.per_channel for s in stages) and size is None
    if flat:
        rgb = np.repeat(np.linspace(0, 1, 256, dtype=np.float32)[:, None], 3, 1)
    else:
        n = size or max((s.size for s in stages), default=SIZES[0])
        n = SIZES[-1] if n > SIZES[0] else SIZES[0]
        rgb = lattice(n)
    for stage in stages:
        try:
            rgb = _run(stage, rgb)
        except Exception:
            logger.exception("A colour stage failed; leaving it out")
    if flat:
        return Lut(table=rgb)
    return Lut(cube=rgb.reshape(n, n, n, 3))


# ── .cube files ───────────────────────────────────────────────────────────


class CubeError(ValueError):
    """A .cube file Telescope can't use: text (marked with N_) and the fields to fill into it once translated."""

    def __init__(self, text: str, **fields):
        super().__init__(text.format(**fields) if fields else text)
        self.text, self.fields = text, fields


@dataclass(frozen=True, eq=False)
class CubeLut:
    """A LUT read from a .cube file: a 1D table, a 3D cube, or a 1D shaper followed by a cube."""

    title: str = ""
    shaper: Optional[np.ndarray] = None
    """(n, 3) output RGB per input step, for inputs from shaper_min to shaper_max."""
    shaper_min: tuple = (0.0, 0.0, 0.0)
    shaper_max: tuple = (1.0, 1.0, 1.0)
    cube: Optional[np.ndarray] = None
    """(n, n, n, 3) indexed [b][g][r], output RGB, for inputs from cube_min to cube_max."""
    cube_min: tuple = (0.0, 0.0, 0.0)
    cube_max: tuple = (1.0, 1.0, 1.0)

    @property
    def size(self) -> int:
        """Points per side of its cube, or of its table without one."""
        return self.cube.shape[0] if self.cube is not None else len(self.shaper)

    @property
    def per_channel(self) -> bool:
        return self.cube is None

    def __call__(self, rgb: np.ndarray) -> np.ndarray:
        out = np.asarray(rgb, np.float32)
        if self.shaper is not None:
            lo, hi = np.asarray(self.shaper_min, np.float32), np.asarray(self.shaper_max, np.float32)
            steps = np.linspace(0, 1, len(self.shaper), dtype=np.float32)
            t = (out - lo) / np.maximum(hi - lo, 1e-12)
            out = np.stack([np.interp(t[:, c], steps, self.shaper[:, c]) for c in range(3)], 1).astype(np.float32)
        if self.cube is not None:
            lo, hi = np.asarray(self.cube_min, np.float32), np.asarray(self.cube_max, np.float32)
            out = sample_cube(self.cube, (out - lo) / np.maximum(hi - lo, 1e-12))
        return out


_NUMBER = r"[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?"
_TRIPLE = re.compile(rf"^\s*({_NUMBER})\s+({_NUMBER})\s+({_NUMBER})\s*$")


def _without_comment(line: str) -> str:
    """line up to a # that isn't inside quotes (a TITLE can have one)."""
    if "#" not in line:
        return line
    quoted = False
    for i, ch in enumerate(line):
        if ch == '"':
            quoted = not quoted
        elif ch == "#" and not quoted:
            return line[:i]
    return line


def parse_cube(text: str) -> CubeLut:
    """A .cube file's text (Adobe's format, plus Resolve's LUT_1D_INPUT_RANGE and shaper-then-cube files)."""
    title, size1, size3 = "", 0, 0
    dmin, dmax = (0.0, 0.0, 0.0), (1.0, 1.0, 1.0)
    range1 = range3 = None
    rows = []
    for number, raw in enumerate(text.splitlines(), 1):
        line = _without_comment(raw).strip()
        if not line:
            continue
        match = _TRIPLE.match(line)
        if match:
            rows.append(tuple(float(v) for v in match.groups()))
            continue
        word, *rest = line.split(None, 1)
        word, rest = word.upper(), (rest[0].strip() if rest else "")
        try:
            if word == "TITLE":
                title = rest.strip('"')
            elif word == "LUT_1D_SIZE":
                size1 = int(rest)
            elif word == "LUT_3D_SIZE":
                size3 = int(rest)
            elif word in ("DOMAIN_MIN", "DOMAIN_MAX"):
                values = tuple(float(v) for v in rest.split())
                if len(values) != 3:
                    raise ValueError
                dmin, dmax = (values, dmax) if word == "DOMAIN_MIN" else (dmin, values)
            elif word in ("LUT_1D_INPUT_RANGE", "LUT_3D_INPUT_RANGE"):
                lo, hi = (float(v) for v in rest.split())
                if word == "LUT_1D_INPUT_RANGE":
                    range1 = ((lo,) * 3, (hi,) * 3)
                else:
                    range3 = ((lo,) * 3, (hi,) * 3)
            elif rows:
                raise CubeError(N_("Line {line} of the file isn't a colour (three numbers)."), line=number)
            # other keywords before the data (LUT_IN_VIDEO_RANGE and the like) don't change the maths
        except CubeError:
            raise
        except ValueError:
            raise CubeError(N_("Line {line} of the file can't be read."), line=number) from None
    if not size1 and not size3:
        raise CubeError(N_("This isn't a .cube LUT: it doesn't say its size."))
    if size3 and not 2 <= size3 <= MAX_CUBE_SIZE:
        raise CubeError(N_("This LUT has {size} points per side. Telescope takes up to {most}."),
                        size=size3, most=MAX_CUBE_SIZE)
    if size1 and not 2 <= size1 <= 65536:
        raise CubeError(N_("This LUT's 1D table has {size} entries, which isn't a size Telescope can use."), size=size1)
    want = size1 + size3 ** 3
    if len(rows) != want:
        raise CubeError(N_("This LUT should have {expected} colours but has {found}."), expected=want, found=len(rows))
    data = np.asarray(rows, np.float32)
    if not np.isfinite(data).all():
        raise CubeError(N_("This LUT has colours that aren't numbers."))
    shaper = cube = None
    shaper_min, shaper_max = range1 or (dmin, dmax)
    cube_min, cube_max = range3 or (dmin, dmax)
    if size1:
        shaper = data[:size1]
        if size3:  # Resolve's shaper-then-cube files give the cube its own range, or 0..1
            cube_min, cube_max = range3 or ((0.0,) * 3, (1.0,) * 3)
    if size3:
        cube = data[size1:].reshape(size3, size3, size3, 3)
    for lo, hi in ((shaper_min, shaper_max), (cube_min, cube_max)):
        if any(h <= l for l, h in zip(lo, hi)):
            raise CubeError(N_("This LUT's DOMAIN_MAX isn't above its DOMAIN_MIN."))
    return CubeLut(title, shaper, tuple(shaper_min), tuple(shaper_max), cube, tuple(cube_min), tuple(cube_max))


def read_cube(path) -> CubeLut:
    """A .cube file; CubeError when it can't be used (OSError when it can't be read at all)."""
    data = Path(path).read_bytes()
    if len(data) > 64 * 1024 * 1024:
        raise CubeError(N_("This file is too big to be a LUT Telescope can use."))
    return parse_cube(data.decode("utf-8-sig", errors="replace"))


def cube_text(cube: np.ndarray, title: str = "") -> str:
    """A 3D LUT ([b][g][r], RGB values) as .cube text: domain 0..1, red changing fastest."""
    n = cube.shape[0]
    lines = []
    if title:
        lines.append('TITLE "{}"'.format(title.replace('"', "'")))
    lines += ["# Made with Telescope", f"LUT_3D_SIZE {n}", "DOMAIN_MIN 0.0 0.0 0.0", "DOMAIN_MAX 1.0 1.0 1.0"]
    flat = np.clip(cube.reshape(-1, 3), 0, 1)
    lines += [f"{r:.6f} {g:.6f} {b:.6f}" for r, g, b in flat.tolist()]
    return "\n".join(lines) + "\n"


def write_cube(path, cube: np.ndarray, title: str = ""):
    """Write a 3D LUT as a .cube file, through a temporary file so a failed write leaves no half file."""
    path = Path(path)
    temp = path.with_name(path.name + ".part")
    temp.write_text(cube_text(cube, title), encoding="utf-8", newline="\n")
    temp.replace(path)


# ── The pipeline the window runs ──────────────────────────────────────────


class ColourPipeline(TelescopePlugin):
    """The window's colour step (HostColour), in the "colour" frame stage: after the picture's effects, before privacy,
    overlays and anything combining streams. It isn't in the plugin list and can't be switched off."""
    frame_stage = "colour"

    name = "colour"

    def __init__(self, source_of: Callable[[], Optional[str]] = lambda: None, bake_inline: bool = False):
        """source_of: from a frame step, the id of the stream whose frame it is. bake_inline: bake on the stream thread
        that first needs a LUT instead of on a thread of its own (tests)."""
        self._source_of = source_of
        self._inline = bake_inline
        self._shown: tuple = ()  # ((order, owner, stage)) for the stream the panels show; swapped whole
        self._by_source: dict = {}  # source id: ((order, owner, stage)) for that stream alone; swapped whole
        self._baked: dict = {}  # tuple of stages: Lut or None, the newest last
        self._current: dict = {}  # stream key: the Lut its frames got last, until the one for new stages is baked
        self._wanted: dict = {}  # stream key: stages waiting for the baking thread
        self._bake_lock = threading.Lock()
        self._baking: Optional[threading.Thread] = None
        self._idle = threading.Event()
        self._idle.set()
        self._splits: dict = {}  # owner: 0..1, where the preview shows the shown stream without its colour
        self._last = None  # worker thread: (frame before, frame after) of the shown stream's newest frame
        self._timing = [0.0, 0]  # seconds spent applying, frames, for diagnostics()
        self._kind = ""

    # ── Stages (Qt thread) ────────────────────────────────────────────────

    def set_stage(self, owner: str, stage: Optional[ColourStage], order: int = 0, source_id: Optional[str] = None):
        def changed(stages: tuple) -> tuple:
            kept = [s for s in stages if s[1] != owner]
            if stage is not None:
                kept.append((int(order), owner, stage))
            return tuple(sorted(kept, key=lambda s: (s[0], s[1])))
        if source_id is None:
            self._shown = changed(self._shown)
            return
        by_source = dict(self._by_source)
        stages = changed(by_source.get(source_id, ()))
        if stages:
            by_source[source_id] = stages
        else:
            by_source.pop(source_id, None)
        self._by_source = by_source

    def stages(self, source_id: Optional[str] = None, shown: Optional[tuple] = None) -> tuple:
        """The stages source_id's stream gets in order, with shown standing in for the panels' (now by default)."""
        merged = list(self._shown if shown is None else shown) + list(self._by_source.get(source_id, ()))
        return tuple(s[2] for s in sorted(merged, key=lambda s: (s[0], s[1])))

    def set_split(self, owner: str, position: Optional[float]):
        splits = dict(self._splits)
        if position is None:
            splits.pop(owner, None)
        else:
            splits[owner] = min(1.0, max(0.0, float(position)))
        self._splits = splits
        if not splits:
            self._last = None

    # ── Frames (worker threads) ───────────────────────────────────────────

    def lut(self, stages: tuple, key=None) -> Optional[Lut]:
        """The baked Lut of these stages for the stream called key. Stages not baked yet go to the baking thread and the
        stream gets the Lut it had meanwhile, so dragging a slider never holds frames up (a 65-point bake can take
        a tenth of a second). It keeps that Lut through stages going away for a moment, as when a phone's settings
        are put back after the defaults."""
        if not stages:
            return None
        with self._bake_lock:
            if stages in self._baked:
                lut = self._current[key] = self._baked.pop(stages)
                self._baked[stages] = lut  # the newest last again
                return lut
            if not self._inline:
                self._wanted[key] = stages
                self._idle.clear()
                if self._baking is None:
                    self._baking = threading.Thread(target=self._bake_wanted, daemon=True, name="colour-bake")
                    self._baking.start()
                return self._current.get(key)
        lut = self._current[key] = self._store(stages, bake(stages))
        return lut

    def _store(self, stages: tuple, lut: Optional[Lut]) -> Optional[Lut]:
        with self._bake_lock:
            in_use = {id(v) for v in self._current.values() if v is not None}
            for old in [k for k, v in self._baked.items() if id(v) not in in_use]:
                if len(self._baked) < _KEEP_BAKED:
                    break
                del self._baked[old]  # the oldest first, never one a stream is showing
            self._baked[stages] = lut
        return lut

    def _bake_wanted(self):
        try:
            while True:
                with self._bake_lock:
                    if not self._wanted:
                        return
                    stages = self._wanted.pop(next(iter(self._wanted)))
                    if stages in self._baked or stages in self._wanted.values():
                        continue
                self._store(stages, bake(stages))
        finally:
            with self._bake_lock:
                self._baking = None
                if not self._wanted:
                    self._idle.set()

    def settle(self, timeout: float = 5.0) -> bool:
        """Wait until nothing waits to be baked (tests); False if it still does after timeout seconds."""
        return self._idle.wait(timeout)

    def _apply(self, stages: tuple, frame: np.ndarray, key=None) -> np.ndarray:
        lut = self.lut(stages, key)
        if lut is None:
            return frame
        start = time.perf_counter()
        out = lut.apply(frame)
        timing = self._timing
        timing[0] += time.perf_counter() - start
        timing[1] += 1
        self._kind = lut.kind
        return out

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        source_id = self._source_of()
        out = self._apply(self.stages(source_id), frame, source_id)
        self._last = (frame, out) if self._splits else None
        return out

    def frame_step(self):
        shown = self._shown

        def step(frame):
            source_id = self._source_of()
            return self._apply(self.stages(source_id, shown), frame, source_id)
        return step

    def preview(self, frame: np.ndarray) -> np.ndarray:
        """frame (the final frame, or a smaller copy of it) with the shown stream's picture before its colour step left
        of the split; frame itself when there's no split. Never changes frame."""
        splits, last = self._splits, self._last
        if not splits or last is None:
            return frame
        before, after = last
        if before.shape[:2] != after.shape[:2] or after.shape[2:] != frame.shape[2:]:
            return frame
        h, w = frame.shape[:2]
        if before.shape[:2] != (h, w):
            if abs(after.shape[1] / after.shape[0] - w / h) > 0.02:
                return frame  # not this stream's picture (or something cropped it on the way)
            before = cv2.resize(before, (w, h), interpolation=cv2.INTER_AREA)
        x = round(w * list(splits.values())[-1])
        out = frame.copy()
        out[:, :x] = before[:, :x]
        line = max(1, round(w / 480))
        out[:, max(0, x - line):x + line] = _SPLIT_LINE
        return out

    def diagnostics(self) -> dict:
        spent, frames = self._timing
        if not frames:
            return {}
        self._timing = [0.0, 0]
        return {"Colour pipeline": f"{self._kind}, {spent / frames * 1000:.1f} ms a frame"}
