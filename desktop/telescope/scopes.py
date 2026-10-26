"""Histogram, waveform, RGB parade and vectorscope maths for scopes on the preview. numpy and OpenCV, no Qt.

Each scope comes as counts (the maths, tested) and as a BGRA picture of them (what the preview draws). Both work on a
small copy: shrink() first, so a scope costs about a millisecond whatever the stream's size."""
import cv2
import numpy as np

SCOPE_WIDTH = 256  # columns a frame is shrunk to before any scope
LUMA_WEIGHTS = (0.0722, 0.7152, 0.2126)  # BT.709, for B, G, R
_BACKDROP = 150  # alpha of a scope's dark background
_GRID = (90, 90, 90, 200)  # BGRA of the scale lines
_TRACE = {"luma": (235, 235, 235), "r": (90, 90, 255), "g": (110, 230, 110), "b": (255, 150, 90)}
# 75% colour bars, where a vectorscope marks them (BGR)
_BARS = ((0, 0, 191), (191, 0, 191), (191, 0, 0), (191, 191, 0), (0, 191, 0), (0, 191, 191))
_SKIN_ANGLE = 123.0  # degrees, the skin tone line on a vectorscope (Cb right, Cr up)


def shrink(frame: np.ndarray, width: int = SCOPE_WIDTH) -> np.ndarray:
    """frame scaled to width columns (keeping its shape), averaging pixels; never wider than it was."""
    h, w = frame.shape[:2]
    if w <= width:
        return frame
    return cv2.resize(frame, (width, max(1, round(h * width / w))), interpolation=cv2.INTER_AREA)


def luma(frame: np.ndarray) -> np.ndarray:
    """Brightness of each pixel of a BGR frame, 0..255 (BT.709 weights)."""
    return cv2.transform(frame, np.array([LUMA_WEIGHTS], dtype=np.float32))


def histogram(frame: np.ndarray) -> np.ndarray:
    """(4, 256) pixel counts at each level: blue, green, red and luma."""
    channels = (frame[..., 0], frame[..., 1], frame[..., 2], luma(frame))
    return np.stack([np.bincount(c.ravel(), minlength=256) for c in channels])


def waveform(channel: np.ndarray) -> np.ndarray:
    """(256, w) counts for one channel (h, w uint8): column x holds how many of x's pixels sit at each level, with
    level 255 in row 0, so it reads like a waveform monitor (bright at the top)."""
    h, w = channel.shape
    index = (255 - channel.astype(np.int32)) * w + np.arange(w, dtype=np.int32)
    return np.bincount(index.ravel(), minlength=256 * w).reshape(256, w)


def parade(frame: np.ndarray) -> np.ndarray:
    """(256, 3w) counts: the red, green and blue waveforms side by side."""
    return np.hstack([waveform(frame[..., c]) for c in (2, 1, 0)])


def chroma(frame: np.ndarray) -> tuple:
    """(Cb, Cr) of each pixel, uint8 with 128 for no colour."""
    ycrcb = cv2.cvtColor(frame, cv2.COLOR_BGR2YCrCb)
    return ycrcb[..., 2], ycrcb[..., 1]


def vectorscope(frame: np.ndarray, size: int = 256) -> np.ndarray:
    """(size, size) counts of each pixel's colour: Cb to the right, Cr up, no colour in the middle."""
    cb, cr = chroma(frame)
    x = cb.astype(np.int32) * size // 256
    y = np.minimum((256 - cr.astype(np.int32)) * size // 256, size - 1)  # 128 lands on the middle row, as for x
    return np.bincount((y * size + x).ravel(), minlength=size * size).reshape(size, size)


def scope_point(bgr: tuple, size: int = 256) -> tuple:
    """Where a colour lands on vectorscope(), in (x, y) cells."""
    cb, cr = chroma(np.array([[bgr]], dtype=np.uint8))
    return int(cb[0, 0]) * size // 256, min((256 - int(cr[0, 0])) * size // 256, size - 1)


# ── Pictures of them ─────────────────────────────────────────────────────────

def glow(counts: np.ndarray, per_cell: float) -> np.ndarray:
    """counts as brightness 0..1, on a log scale so a few pixels still show; per_cell is what an even spread gives."""
    return np.clip(np.log1p(counts * (8.0 / max(per_cell, 1e-6))) / np.log(48.0), 0.0, 1.0).astype(np.float32)


def _backdrop(h: int, w: int) -> np.ndarray:
    out = np.zeros((h, w, 4), np.uint8)
    out[..., 3] = _BACKDROP
    return out


def _levels_grid(image: np.ndarray, x0: int = 0, x1: int = None):
    """Lines across a waveform picture at every 25% (0 and 100% a little brighter)."""
    h = image.shape[0]
    for pct in (100, 75, 50, 25, 0):
        row = min(h - 1, round((100 - pct) / 100 * (h - 1)))
        alpha = 255 if pct in (0, 100) else _GRID[3]
        image[row, x0:x1] = (*_GRID[:3], alpha)


def _paint_trace(image: np.ndarray, strength: np.ndarray, colour, x0: int = 0):
    """strength (0..1) of a colour over the backdrop, in place."""
    h, w = strength.shape
    region = image[:, x0:x0 + w]
    trace = strength[..., None] * np.array(colour, np.float32)
    region[..., :3] = np.maximum(region[..., :3], trace.astype(np.uint8))
    region[..., 3] = np.maximum(region[..., 3], (_BACKDROP + strength * (255 - _BACKDROP)).astype(np.uint8))


def waveform_image(frame: np.ndarray) -> np.ndarray:
    """A BGRA luma waveform of a shrunk frame, (256, w)."""
    counts = waveform(luma(frame))
    image = _backdrop(*counts.shape)
    _levels_grid(image)
    _paint_trace(image, glow(counts, frame.shape[0] / 256), _TRACE["luma"])
    return image


def parade_image(frame: np.ndarray) -> np.ndarray:
    """A BGRA RGB parade of a shrunk frame, (256, 3w + 2 gaps)."""
    h, w = frame.shape[:2]
    gap = 4
    image = _backdrop(256, 3 * w + 2 * gap)
    for i, (c, name) in enumerate(((2, "r"), (1, "g"), (0, "b"))):
        x0 = i * (w + gap)
        _levels_grid(image, x0, x0 + w)
        _paint_trace(image, glow(waveform(frame[..., c]), h / 256), _TRACE[name], x0)
    return image


def histogram_image(frame: np.ndarray, height: int = 128) -> np.ndarray:
    """A BGRA histogram: red, green and blue filled in on top of each other, luma as a white outline."""
    counts = histogram(frame).astype(np.float32)
    top = max(float(counts[:, 2:-2].max()), 1.0) * 1.1  # a pile at the very ends (clipping) can run off the top
    heights = np.clip(counts / top, 0.0, 1.0) * height
    rows = np.arange(height, 0, -1, dtype=np.float32)[:, None]  # row 0 is the top
    image = _backdrop(height, 256)
    for c, name in ((2, "r"), (1, "g"), (0, "b")):
        filled = (rows <= heights[c][None, :]).astype(np.float32) * 0.6
        layer = filled[..., None] * np.array(_TRACE[name], np.float32)
        image[..., :3] = np.clip(image[..., :3].astype(np.float32) + layer, 0, 255).astype(np.uint8)
        image[..., 3] = np.maximum(image[..., 3], (filled > 0) * 220)
    edge = np.abs(rows - heights[3][None, :]) < 1.0
    image[edge] = (*_TRACE["luma"], 255)
    for quarter in (64, 128, 192):
        image[:, quarter, :3] = np.maximum(image[:, quarter, :3], _GRID[:3])
    return image


_scope_grid: dict = {}


def _vectorscope_grid(size: int) -> np.ndarray:
    """The circle, the colour bar targets and the skin tone line, drawn once per size."""
    if size not in _scope_grid:
        grid = np.zeros((size, size, 4), np.uint8)
        mid = size // 2
        colour = (*_GRID[:3], 255)
        cv2.circle(grid, (mid, mid), round(size * 0.45), colour, 1, cv2.LINE_AA)
        cv2.line(grid, (mid - 4, mid), (mid + 4, mid), colour, 1)
        cv2.line(grid, (mid, mid - 4), (mid, mid + 4), colour, 1)
        angle = np.radians(_SKIN_ANGLE)
        end = (round(mid + np.cos(angle) * size * 0.45), round(mid - np.sin(angle) * size * 0.45))
        cv2.line(grid, (mid, mid), end, (*_GRID[:3], 160), 1, cv2.LINE_AA)
        box = max(3, size // 40)
        for bar in _BARS:
            x, y = scope_point(bar, size)
            cv2.rectangle(grid, (x - box, y - box), (x + box, y + box), (*bar, 255), 1)
        _scope_grid[size] = grid
    return _scope_grid[size]


def vectorscope_image(frame: np.ndarray, size: int = 256) -> np.ndarray:
    """A BGRA vectorscope, (size, size): each spot drawn in the colour it stands for."""
    counts = vectorscope(frame, size)
    strength = glow(counts, frame.shape[0] * frame.shape[1] / (size * size) * 6)
    cells = (np.arange(size) * 256 // size + 128 // size).astype(np.uint8)
    cb, cr = np.meshgrid(cells, cells[::-1])
    ycrcb = np.dstack([np.full_like(cb, 170), cr, cb])
    tint = cv2.cvtColor(ycrcb, cv2.COLOR_YCrCb2BGR).astype(np.float32)
    tint = tint * 0.6 + 100  # paler, so a dim spot still shows on the dark backdrop
    image = _backdrop(size, size)
    image[..., :3] = (tint * strength[..., None]).clip(0, 255).astype(np.uint8)
    image[..., 3] = np.maximum(image[..., 3], (_BACKDROP + strength * (255 - _BACKDROP)).astype(np.uint8))
    grid = _vectorscope_grid(size)
    lines = grid[..., 3] > 0
    image[lines] = np.maximum(image[lines], grid[lines])
    return image


SCOPES = {
    "histogram": histogram_image,
    "waveform": waveform_image,
    "parade": parade_image,
    "vectorscope": vectorscope_image,
}
"""Each scope's picture from a shrunk BGR frame, by name."""


def scope_image(name: str, frame: np.ndarray) -> np.ndarray:
    """The named scope (a SCOPES key) of a frame of any size, as a BGRA picture."""
    return SCOPES[name](shrink(frame))
