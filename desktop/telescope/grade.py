"""The maths of the Colour grading card: one Grade turns RGB values 0..1 into graded ones, in the order a grading
panel reads (white balance, wheels, contrast, tone, saturation, curves, then a LUT file). No Qt here."""
from dataclasses import dataclass, field, replace
from typing import Optional

import numpy as np

from telescope.colour import CubeLut

LUMA = np.array([0.2126, 0.7152, 0.0722], np.float32)  # Rec. 709
PIVOT = 0.435  # where contrast turns around by default: about mid grey on a video signal
NEUTRAL_HUE = tuple((i / 6, 0.5) for i in range(6))  # a hue curve's points at red, yellow, green, cyan, blue and magenta
WHEELS = ("lift", "gamma", "gain", "offset")
CHANNELS = ("all", "r", "g", "b")
LINEAR = ((0.0, 0.0), (1.0, 1.0))
# How far a wheel's puck at the rim moves each channel (wheel_shift gives up to about 0.9) and its master's range
_WHEEL_REACH = {"lift": 0.2, "gamma": 0.5, "gain": 0.4, "offset": 0.2}
MASTER_RANGE = {"lift": (-0.5, 0.5), "gamma": (-1.0, 1.0), "gain": (0.0, 2.0), "offset": (-0.5, 0.5)}
MASTER_DEFAULT = {"lift": 0.0, "gamma": 0.0, "gain": 1.0, "offset": 0.0}
_TEMPERATURE_STOPS = 0.25  # temperature 100 moves red and blue this many stops apart each way
_TINT_STOPS = 0.2
_TONE = 1.0  # shadows or highlights at 100 add up to 4/27 of the range to the part they're about


def wheel_shift(x: float, y: float) -> np.ndarray:
    """RGB change of a wheel's puck at (x, y) in the unit circle, x towards blue and y towards red as on a
    vectorscope (Rec. 709 Cb and Cr at half scale). It adds up to no change in brightness."""
    cb, cr = 0.5 * x, 0.5 * y
    return np.array([1.5748 * cr, -0.1873 * cb - 0.4681 * cr, 1.8556 * cb], np.float32)


def srgb_to_linear(v: np.ndarray) -> np.ndarray:
    v = np.clip(v, 0, 1)
    return np.where(v <= 0.04045, v / 12.92, ((v + 0.055) / 1.055) ** 2.4)


def linear_to_srgb(v: np.ndarray) -> np.ndarray:
    v = np.clip(v, 0, None)
    return np.where(v <= 0.0031308, v * 12.92, 1.055 * np.power(v, 1 / 2.4) - 0.055)


def white_balance_gains(temperature: float, tint: float) -> np.ndarray:
    """Linear-light RGB gains for temperature and tint (-100..100; warmer and more magenta upwards), scaled so white
    keeps its brightness."""
    t, n = temperature / 100, tint / 100
    gains = np.array([2 ** (_TEMPERATURE_STOPS * t), 2 ** (-_TINT_STOPS * n), 2 ** (-_TEMPERATURE_STOPS * t)],
                     np.float32)
    return gains / float(LUMA @ gains)


def monotone_curve(points, x: np.ndarray) -> np.ndarray:
    """The curve through points ((x, y), x rising, both 0..1) at x: a monotone cubic (Fritsch and Carlson), so it
    never overshoots between points. Flat past the first and last point."""
    pts = sorted((float(px), float(py)) for px, py in points)
    xs = np.array([p[0] for p in pts], np.float64)
    ys = np.array([p[1] for p in pts], np.float64)
    if len(xs) < 2:
        return np.full_like(x, ys[0] if len(ys) else 0.0)
    return _hermite(xs, ys, _slopes(xs, ys), x)


def _slopes(xs: np.ndarray, ys: np.ndarray) -> np.ndarray:
    h = np.diff(xs)
    h = np.where(h <= 0, 1e-9, h)
    d = np.diff(ys) / h
    m = np.empty_like(ys)
    m[0], m[-1] = d[0], d[-1]
    for i in range(1, len(xs) - 1):
        if d[i - 1] * d[i] <= 0:
            m[i] = 0.0
        else:  # weighted harmonic mean, which keeps each piece monotone
            w1, w2 = 2 * h[i] + h[i - 1], h[i] + 2 * h[i - 1]
            m[i] = (w1 + w2) / (w1 / d[i - 1] + w2 / d[i])
    return m


def _hermite(xs, ys, m, x):
    x = np.asarray(x, np.float64)
    i = np.clip(np.searchsorted(xs, x, side="right") - 1, 0, len(xs) - 2)
    h = np.maximum(xs[i + 1] - xs[i], 1e-9)
    t = np.clip((x - xs[i]) / h, 0, 1)
    t2, t3 = t * t, t * t * t
    out = ((2 * t3 - 3 * t2 + 1) * ys[i] + (t3 - 2 * t2 + t) * h * m[i] + (-2 * t3 + 3 * t2) * ys[i + 1]
           + (t3 - t2) * h * m[i + 1])
    out = np.where(x <= xs[0], ys[0], np.where(x >= xs[-1], ys[-1], out))
    return out.astype(np.float32)


def periodic_curve(points, x: np.ndarray) -> np.ndarray:
    """A hue curve: like monotone_curve, but going round (x is a hue 0..1 and 1 is 0 again)."""
    pts = []
    for px, py in sorted((float(px) % 1.0, float(py)) for px, py in points):
        if pts and px - pts[-1][0] < 1e-6:
            pts[-1] = (pts[-1][0], (pts[-1][1] + py) / 2)  # one hue given twice: the middle of the two
        else:
            pts.append((px, py))
    if len(pts) > 1 and pts[0][0] + 1 - pts[-1][0] < 1e-6:  # x near 1 is the hue at 0 again
        pts[0] = (pts[0][0], (pts[0][1] + pts.pop()[1]) / 2)
    if not pts:
        return np.full_like(x, 0.5)
    if len(pts) == 1:
        return np.full_like(x, pts[0][1])
    wrapped = [(px - 1, py) for px, py in pts[-2:]] + pts + [(px + 1, py) for px, py in pts[:2]]
    xs = np.array([p[0] for p in wrapped], np.float64)
    ys = np.array([p[1] for p in wrapped], np.float64)
    return _hermite(xs, ys, _slopes(xs, ys), np.mod(x, 1.0))


def hue_of(rgb: np.ndarray) -> np.ndarray:
    """HSV hue 0..1 (red 0, green 1/3, blue 2/3) of (n, 3) RGB values; 0 for greys."""
    r, g, b = rgb[:, 0], rgb[:, 1], rgb[:, 2]
    top, low = rgb.max(1), rgb.min(1)
    span = np.where(top - low > 1e-6, top - low, 1.0)
    h = np.where(top == r, (g - b) / span, np.where(top == g, 2 + (b - r) / span, 4 + (r - g) / span))
    return np.where(top - low > 1e-6, np.mod(h / 6, 1.0), 0.0).astype(np.float32)


def rotate_hue(rgb: np.ndarray, turns: np.ndarray) -> np.ndarray:
    """rgb turned around the grey axis by turns (1 = all the way round; positive goes red to green to blue)."""
    angle = 2 * np.pi * turns[:, None]
    k = np.float32(1 / np.sqrt(3))
    cross = k * np.stack([rgb[:, 2] - rgb[:, 1], rgb[:, 0] - rgb[:, 2], rgb[:, 1] - rgb[:, 0]], 1)
    along = k * k * rgb.sum(1, keepdims=True)
    return (rgb * np.cos(angle) + cross * np.sin(angle) + along * (1 - np.cos(angle))).astype(np.float32)


@dataclass(frozen=True)
class Grade:
    """Every setting of the Colour grading card. Frozen: the card makes a new one on each change and the stream threads
    use whichever they were handed."""

    temperature: float = 0.0
    tint: float = 0.0
    wheels: dict = field(default_factory=lambda: {w: (0.0, 0.0) for w in WHEELS})
    """Each wheel's puck (x, y) in the unit circle."""
    masters: dict = field(default_factory=lambda: dict(MASTER_DEFAULT))
    contrast: float = 1.0
    pivot: float = PIVOT
    shadows: float = 0.0
    """-100..100"""
    highlights: float = 0.0
    saturation: float = 1.0
    curves: dict = field(default_factory=lambda: {c: LINEAR for c in CHANNELS})
    hue_sat: tuple = NEUTRAL_HUE
    hue_hue: tuple = NEUTRAL_HUE
    lut: Optional[CubeLut] = None
    lut_strength: float = 1.0

    def __hash__(self):
        return id(self)

    def changed(self, **changes) -> "Grade":
        return replace(self, **changes)

    def _wheel(self, name: str) -> np.ndarray:
        x, y = self.wheels.get(name, (0.0, 0.0))
        return wheel_shift(x, y) * _WHEEL_REACH[name]

    def _primaries_neutral(self) -> bool:
        return all(self.wheels.get(w, (0, 0)) == (0.0, 0.0) and self.masters.get(w) == MASTER_DEFAULT[w]
                   for w in WHEELS)

    def is_identity(self) -> bool:
        return (self.temperature == 0 and self.tint == 0 and self._primaries_neutral() and self.contrast == 1
                and self.shadows == 0 and self.highlights == 0 and self.saturation == 1
                and all(tuple(map(tuple, self.curves.get(c, LINEAR))) == LINEAR for c in CHANNELS)
                and _neutral_hue(self.hue_sat) and _neutral_hue(self.hue_hue)
                and (self.lut is None or self.lut_strength == 0))

    def per_channel(self) -> bool:
        """Each channel only depends on itself: no saturation, tone, hue curves or 3D LUT in play."""
        return (self.shadows == 0 and self.highlights == 0 and self.saturation == 1 and _neutral_hue(self.hue_sat)
                and _neutral_hue(self.hue_hue)
                and (self.lut is None or self.lut_strength == 0 or self.lut.per_channel))

    def lut_size(self) -> int:
        """Points the colour pipeline needs to carry this grade: 65 with a detailed 3D LUT file, else 33."""
        return 65 if self.lut is not None and self.lut.cube is not None and self.lut.size > 33 else 33

    def __call__(self, rgb: np.ndarray) -> np.ndarray:
        """(n, 3) RGB values 0..1, graded."""
        x = np.asarray(rgb, np.float32)
        if self.temperature or self.tint:
            gains = white_balance_gains(self.temperature, self.tint)
            x = linear_to_srgb(srgb_to_linear(x) * gains)
        if not self._primaries_neutral():
            m = self.masters
            x = x + (m["offset"] + self._wheel("offset"))
            lift = m["lift"] + self._wheel("lift")
            gain = m["gain"] * (1 + self._wheel("gain"))
            x = gain * x + lift * (1 - x)
            power = 2.0 ** -(m["gamma"] + self._wheel("gamma"))
            x = np.power(np.clip(x, 0, None), power)
        if self.contrast != 1 or self.pivot != PIVOT:
            x = (x - self.pivot) * self.contrast + self.pivot
        x = np.clip(x, 0, 1).astype(np.float32)
        if self.shadows or self.highlights:
            y = np.clip(x @ LUMA, 0, 1)
            shift = _TONE * (self.shadows / 100 * y * (1 - y) ** 2 + self.highlights / 100 * y * y * (1 - y))
            x = x + shift[:, None]
        if self.saturation != 1:
            y = (x @ LUMA)[:, None]
            x = y + self.saturation * (x - y)
        curves = self.curves
        if any(tuple(map(tuple, curves.get(c, LINEAR))) != LINEAR for c in CHANNELS):
            x = np.clip(x, 0, 1)
            x = monotone_curve(curves.get("all", LINEAR), x)
            x = np.stack([monotone_curve(curves.get(c, LINEAR), x[:, i]) for i, c in enumerate("rgb")], 1)
        if not (_neutral_hue(self.hue_sat) and _neutral_hue(self.hue_hue)):
            x = np.clip(x, 0, 1).astype(np.float32)
            hue = hue_of(x)
            if not _neutral_hue(self.hue_hue):
                x = rotate_hue(x, periodic_curve(self.hue_hue, hue) - 0.5)
            if not _neutral_hue(self.hue_sat):
                y = (x @ LUMA)[:, None]
                x = y + 2 * periodic_curve(self.hue_sat, hue)[:, None] * (x - y)
        if self.lut is not None and self.lut_strength > 0:
            x = np.clip(x, 0, 1).astype(np.float32)
            looked = self.lut(x)
            x = x + self.lut_strength * (looked - x)
        return np.clip(x, 0, 1).astype(np.float32)


def _neutral_hue(points) -> bool:
    return all(abs(py - 0.5) < 1e-6 for _px, py in points)
