"""The Fun pack's picture effects: plain functions and small classes over BGR frames, no Qt.

An effect is a class whose instance keeps what it needs between frames (a trail, a few frames back) and is called as
effect(frame, look) -> frame, with look the frozen settings (Look). A new instance starts afresh, which is what a
stream behind the panels gets. The heavy parts work on a smaller copy where it looks the same.
"""

from collections import deque
from dataclasses import dataclass, field
from typing import Callable, Optional

import cv2
import numpy as np

_MASK_SMALL = 4  # masks from the empty room are worked out at a quarter of the size
_DIFFERENCE = 28  # how far a pixel has to be from the empty room to count as someone in front of it
_ASCII_RAMP = " .:-=+*#%@"
_GAME_BOY = np.array([[15, 56, 15], [48, 98, 48], [15, 172, 139], [15, 188, 155]], np.uint8)  # BGR, dark to light
_BAYER = np.array([[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]], np.float32) / 16 - 0.5


@dataclass(frozen=True)
class Look:
    """What the effects need, frozen (swapped whole from the GUI thread)."""

    effect: str = "off"
    plate: Optional[np.ndarray] = field(default=None, compare=False)  # the empty room, as taken
    clones: Optional["Clones"] = field(default=None, compare=False)
    edges: int = 3  # 0..10, how soft a cut-out's edge is
    fade: int = 5  # 1..10, how long light trails last
    brightness: int = 6  # 1..10, how bright a light has to be to paint
    spread: int = 20  # frames between the top of a slit-scan and its bottom
    across: bool = False  # slit-scan bands run left to right instead of top to bottom
    lines: tuple = ()  # the stick figure: ((x0, y0), (x1, y1)) 0..1 of the frame, and heads as ((x, y), r)
    heads: tuple = ()
    mask: Optional[Callable] = field(default=None, compare=False)  # frame -> person mask or None (host.person_mask)
    reset: int = 0  # bumped to start the effect afresh (Clear on light painting)


def _fit(image: np.ndarray, frame: np.ndarray) -> np.ndarray:
    h, w = frame.shape[:2]
    if image.shape[:2] == (h, w):
        return image
    return cv2.resize(image, (w, h), interpolation=cv2.INTER_LINEAR)


def compose(top: np.ndarray, mask: np.ndarray, under: np.ndarray) -> np.ndarray:
    """top where mask is 255 over under, all uint8: two multiplies and an add."""
    weight = cv2.merge((mask, mask, mask))
    return cv2.add(cv2.multiply(top, weight, scale=1 / 255), cv2.multiply(under, cv2.bitwise_not(weight), scale=1 / 255))


def soften(mask: np.ndarray, edges: int) -> np.ndarray:
    """mask with its edge feathered (edges 0..10), blurred at a quarter of the size."""
    if edges <= 0:
        return mask
    h, w = mask.shape[:2]
    small = cv2.resize(mask, (max(1, w // _MASK_SMALL), max(1, h // _MASK_SMALL)), interpolation=cv2.INTER_AREA)
    return cv2.resize(cv2.GaussianBlur(small, (0, 0), edges * 0.6), (w, h), interpolation=cv2.INTER_LINEAR)


class PlateMask:
    """Where a frame differs from the empty room: someone in front of it, without the vision download."""

    def __init__(self):
        self._plate_id, self._small = None, None

    def __call__(self, frame: np.ndarray, plate: np.ndarray) -> np.ndarray:
        h, w = frame.shape[:2]
        size = (max(1, w // _MASK_SMALL), max(1, h // _MASK_SMALL))
        key = (id(plate), size)
        if key != self._plate_id:
            self._plate_id = key
            self._small = cv2.GaussianBlur(cv2.resize(plate, size, interpolation=cv2.INTER_AREA), (3, 3), 0)
        small = cv2.GaussianBlur(cv2.resize(frame, size, interpolation=cv2.INTER_AREA), (3, 3), 0)
        diff = cv2.absdiff(small, self._small)
        diff = cv2.max(cv2.max(diff[..., 0], diff[..., 1]), diff[..., 2])
        _t, mask = cv2.threshold(diff, _DIFFERENCE, 255, cv2.THRESH_BINARY)
        mask = cv2.morphologyEx(mask, cv2.MORPH_OPEN, np.ones((3, 3), np.uint8))
        mask = cv2.morphologyEx(mask, cv2.MORPH_CLOSE, np.ones((7, 7), np.uint8))
        return cv2.resize(mask, (w, h), interpolation=cv2.INTER_LINEAR)


def person(frame: np.ndarray, look: Look, plate_mask: PlateMask) -> Optional[np.ndarray]:
    """The person mask: from the vision download when it's there, else the difference from the empty room."""
    mask = look.mask(frame) if look.mask is not None else None
    if mask is None and look.plate is not None:
        mask = plate_mask(frame, _fit(look.plate, frame))
    return mask


class Clones:
    """The clones taken so far, flattened into one picture and one mask so a frame costs the same however many."""

    MAX = 8

    def __init__(self, picture: np.ndarray, mask: np.ndarray, count: int):
        self.picture, self.mask, self.count = picture, mask, count

    @staticmethod
    def add(clones: Optional["Clones"], frame: np.ndarray, mask: np.ndarray) -> "Clones":
        if clones is None or clones.picture.shape != frame.shape:
            return Clones(frame.copy(), mask.copy(), 1)
        return Clones(compose(frame, mask, clones.picture), cv2.max(clones.mask, mask), clones.count + 1)


class CloneParty:
    def __init__(self):
        self._plate_mask = PlateMask()

    def __call__(self, frame: np.ndarray, look: Look) -> np.ndarray:
        clones = look.clones
        if clones is None:
            return frame
        out = compose(_fit(clones.picture, frame), _fit(clones.mask, frame), frame)
        mask = person(frame, look, self._plate_mask)
        return out if mask is None else compose(frame, soften(mask, look.edges), out)


class Cloak:
    def __init__(self):
        self._plate_mask = PlateMask()

    def __call__(self, frame: np.ndarray, look: Look) -> np.ndarray:
        if look.plate is None:
            return frame
        plate = _fit(look.plate, frame)
        mask = look.mask(frame) if look.mask is not None else None
        if mask is None:
            mask = self._plate_mask(frame, plate)
        else:
            mask = cv2.dilate(mask, np.ones((9, 9), np.uint8))  # the model's edge sits a little inside the person
        return compose(plate, soften(mask, look.edges), frame)


class LightPainting:
    """Bright lights leave trails that fade: the brightest of the trail, faded, and the lights in this frame."""

    def __init__(self):
        self._trail: Optional[np.ndarray] = None

    def __call__(self, frame: np.ndarray, look: Look) -> np.ndarray:
        threshold = 150 + look.brightness * 10
        gray = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)
        _t, lit = cv2.threshold(gray, threshold, 255, cv2.THRESH_BINARY)
        lights = cv2.bitwise_and(frame, frame, mask=lit)
        if self._trail is None or self._trail.shape != frame.shape:
            self._trail = lights
        else:
            keep = 0.80 + look.fade * 0.019  # 1: gone in about half a second, 10: hangs around for seconds
            self._trail = cv2.max(cv2.convertScaleAbs(self._trail, alpha=keep), lights)
        return cv2.max(frame, self._trail)


class SlitScan:
    """Each band of the picture from a little further back in time, at half the size."""

    BANDS = 45

    def __init__(self):
        self._frames: deque = deque()
        self._shape = None

    def __call__(self, frame: np.ndarray, look: Look) -> np.ndarray:
        h, w = frame.shape[:2]
        small = cv2.resize(frame, (max(1, w // 2), max(1, h // 2)), interpolation=cv2.INTER_AREA)
        if small.shape != self._shape:
            self._frames.clear()
            self._shape = small.shape
        spread = max(1, look.spread)
        self._frames.appendleft(small)
        while len(self._frames) > spread + 1:
            self._frames.pop()
        sh, sw = small.shape[:2]
        size = sw if look.across else sh
        bands = min(self.BANDS, size)
        out = np.empty_like(small)
        last = len(self._frames) - 1
        for b in range(bands):
            lo, hi = b * size // bands, (b + 1) * size // bands
            src = self._frames[min(last, b * spread // max(1, bands - 1))]
            if look.across:
                out[:, lo:hi] = src[:, lo:hi]
            else:
                out[lo:hi] = src[lo:hi]
        return cv2.resize(out, (w, h), interpolation=cv2.INTER_LINEAR)


class StickFigure:
    """A stick figure where the person is, drawn over the empty room or plain paper."""

    PAPER = (232, 236, 240)
    INK = (40, 32, 28)

    def __init__(self):
        self._paper: Optional[np.ndarray] = None

    def __call__(self, frame: np.ndarray, look: Look) -> np.ndarray:
        h, w = frame.shape[:2]
        if look.plate is not None:
            out = _fit(look.plate, frame).copy()
        else:
            if self._paper is None or self._paper.shape != frame.shape:
                self._paper = np.full_like(frame, self.PAPER)
            out = self._paper.copy()
        thick = max(3, h // 70)
        for (x0, y0), (x1, y1) in look.lines:
            cv2.line(out, (int(x0 * w), int(y0 * h)), (int(x1 * w), int(y1 * h)), self.INK, thick, cv2.LINE_AA)
        for (x, y), r in look.heads:
            cv2.circle(out, (int(x * w), int(y * h)), max(thick, int(r * h)), self.INK, thick, cv2.LINE_AA)
        return out


class GameBoy:
    """Four shades of green at 144 lines, dithered."""

    LINES = 144

    def __init__(self):
        self._bayer = {}

    def __call__(self, frame: np.ndarray, look: Look) -> np.ndarray:
        h, w = frame.shape[:2]
        sh, sw = self.LINES, max(1, round(w * self.LINES / h))
        gray = cv2.cvtColor(cv2.resize(frame, (sw, sh), interpolation=cv2.INTER_AREA), cv2.COLOR_BGR2GRAY)
        if (sh, sw) not in self._bayer:
            self._bayer = {(sh, sw): np.tile(_BAYER, (sh // 4 + 1, sw // 4 + 1))[:sh, :sw]}
        levels = np.clip(gray.astype(np.float32) / 255 * 3 + self._bayer[(sh, sw)] + 0.5, 0, 3).astype(np.uint8)
        return cv2.resize(_GAME_BOY[levels], (w, h), interpolation=cv2.INTER_NEAREST)


class Webcam1998:
    """320 wide, washed out, blocky JPEG and 15 frames a second."""

    WIDTH = 320

    def __init__(self):
        self._held: Optional[np.ndarray] = None
        self._odd = False
        self._rng = np.random.default_rng()

    def __call__(self, frame: np.ndarray, look: Look) -> np.ndarray:
        self._odd = not self._odd
        if self._odd and self._held is not None and self._held.shape == frame.shape:
            return self._held
        h, w = frame.shape[:2]
        sw, sh = self.WIDTH, max(1, round(h * self.WIDTH / w))
        small = cv2.resize(frame, (sw, sh), interpolation=cv2.INTER_AREA)
        small = cv2.convertScaleAbs(small, alpha=0.8, beta=28)  # washed out
        small = cv2.add(small, np.full_like(small, (18, 6, 0)))  # a cold cast
        noise = self._rng.integers(-10, 11, small.shape[:2], dtype=np.int16)
        small = np.clip(small.astype(np.int16) + noise[..., None], 0, 255).astype(np.uint8)
        _ok, jpeg = cv2.imencode(".jpg", small, (cv2.IMWRITE_JPEG_QUALITY, 12))
        small = cv2.imdecode(jpeg, cv2.IMREAD_COLOR)
        self._held = cv2.resize(small, (w, h), interpolation=cv2.INTER_NEAREST)
        return self._held


class Vhs:
    """Smeared colour shifted to the side, soft picture, noise and a tracking band that rolls up the screen."""

    def __init__(self):
        self._t = 0
        self._rng = np.random.default_rng()

    def __call__(self, frame: np.ndarray, look: Look) -> np.ndarray:
        self._t += 1
        h, w = frame.shape[:2]
        sw, sh = max(1, w // 3), max(1, h // 2)
        small = cv2.resize(frame, (sw, sh), interpolation=cv2.INTER_AREA)
        ycc = cv2.cvtColor(small, cv2.COLOR_BGR2YCrCb)
        y, cr, cb = cv2.split(ycc)
        shift = max(1, sw // 120)
        cr = cv2.blur(np.roll(cr, shift, axis=1), (9, 1))
        cb = cv2.blur(np.roll(cb, shift, axis=1), (9, 1))
        y = cv2.blur(y, (3, 1))
        noise = self._rng.integers(-12, 13, (sh, 1), dtype=np.int16)  # each line a little off
        y = np.clip(y.astype(np.int16) + noise, 0, 255).astype(np.uint8)
        band = (sh - (self._t * 3) % (sh + 40)) % sh
        lo, hi = max(0, band - 6), min(sh, band + 6)
        y[lo:hi] = np.roll(y[lo:hi], self._rng.integers(-8, 9), axis=1)
        y[lo:hi] = cv2.add(y[lo:hi], 40)
        out = cv2.cvtColor(cv2.merge((y, cr, cb)), cv2.COLOR_YCrCb2BGR)
        return cv2.resize(out, (w, h), interpolation=cv2.INTER_LINEAR)


class Crt:
    """A curved screen: bulged, with scanlines, a phosphor mask and dark corners."""

    def __init__(self):
        self._size = None
        self._maps = None
        self._shade = None

    def _build(self, w: int, h: int):
        self._size = (w, h)
        xs, ys = np.meshgrid(np.linspace(-1, 1, w, dtype=np.float32), np.linspace(-1, 1, h, dtype=np.float32))
        r2 = xs * xs + ys * ys
        bulge = 1 + 0.06 * r2
        self._maps = cv2.convertMaps((xs * bulge + 1) * (w - 1) / 2, (ys * bulge + 1) * (h - 1) / 2, cv2.CV_16SC2)
        shade = np.clip(1.15 - 0.45 * r2, 0, 1)[..., None] * np.ones(3, np.float32)
        pitch = max(2, h // 270)  # about 270 scanlines whatever the size
        shade[np.arange(h) % pitch == pitch - 1] *= 0.55
        triad = np.array([[1.0, 0.82, 0.82], [0.82, 1.0, 0.82], [0.82, 0.82, 1.0]], np.float32)  # B, G, R columns
        shade *= triad[np.arange(w) % 3][None]
        edge = (np.abs(xs * bulge) > 1) | (np.abs(ys * bulge) > 1)
        shade[edge] = 0
        self._shade = (shade * 255).astype(np.uint8)

    def __call__(self, frame: np.ndarray, look: Look) -> np.ndarray:
        h, w = frame.shape[:2]
        if self._size != (w, h):
            self._build(w, h)
        bent = cv2.remap(frame, self._maps[0], self._maps[1], cv2.INTER_LINEAR, borderMode=cv2.BORDER_CONSTANT)
        lit = cv2.add(bent, cv2.convertScaleAbs(cv2.blur(bent, (9, 9)), alpha=0.25))  # a little glow
        return cv2.multiply(lit, self._shade, scale=1 / 255)


class Ascii:
    """The picture in characters, green on black, about 150 across."""

    COLUMNS = 150

    def __init__(self):
        self._cell = None
        self._glyphs = None

    def _build(self, cw: int, ch: int):
        self._cell = (cw, ch)
        glyphs = np.zeros((len(_ASCII_RAMP), ch, cw), np.uint8)
        scale = cv2.getFontScaleFromHeight(cv2.FONT_HERSHEY_PLAIN, max(4, ch - 4), 1)
        for i, c in enumerate(_ASCII_RAMP):
            (tw, _th), _base = cv2.getTextSize(c, cv2.FONT_HERSHEY_PLAIN, scale, 1)
            cv2.putText(glyphs[i], c, ((cw - tw) // 2, ch - 3), cv2.FONT_HERSHEY_PLAIN, scale, 255, 1, cv2.LINE_AA)
        self._glyphs = glyphs

    def __call__(self, frame: np.ndarray, look: Look) -> np.ndarray:
        h, w = frame.shape[:2]
        cw = max(4, w // self.COLUMNS)
        ch = cw * 2
        cols, rows = w // cw, h // ch
        if self._cell != (cw, ch):
            self._build(cw, ch)
        gray = cv2.cvtColor(cv2.resize(frame, (cols, rows), interpolation=cv2.INTER_AREA), cv2.COLOR_BGR2GRAY)
        index = (gray.astype(np.uint16) * len(_ASCII_RAMP) // 256).astype(np.intp)
        text = self._glyphs[index].transpose(0, 2, 1, 3).reshape(rows * ch, cols * cw)
        out = np.zeros_like(frame)
        out[:rows * ch, :cols * cw, 1] = text
        out[:rows * ch, :cols * cw, 0] = text // 3
        return out


EFFECTS = {
    "clones": CloneParty, "cloak": Cloak, "light": LightPainting, "slit": SlitScan, "stick": StickFigure,
    "game_boy": GameBoy, "webcam": Webcam1998, "vhs": Vhs, "crt": Crt, "ascii": Ascii,
}


class Runner:
    """Runs whichever effect look names, keeping its state while it stays the same one."""

    def __init__(self):
        self.name, self.effect = None, None

    def __call__(self, frame: np.ndarray, look: Look) -> np.ndarray:
        if (look.effect, look.reset) != self.name:
            self.name = (look.effect, look.reset)
            self.effect = EFFECTS[look.effect]() if look.effect in EFFECTS else None
        return frame if self.effect is None else self.effect(frame, look)
