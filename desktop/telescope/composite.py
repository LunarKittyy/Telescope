"""Several streams in one picture: the layout, the maths the editor moves tiles with, and drawing it.

No Qt here, so the maths is tested on its own. Boxes are fractions of the picture (0..1) and borders and corners are
pixels of a 1080-line picture, so a layout looks the same at any canvas size.
"""

import functools
import math
import threading
from dataclasses import dataclass, replace
from typing import Optional

import cv2
import numpy as np

from telescope.pictures import cover, read_picture

MAX_TILES = 4  # as many as Telescope runs streams
MAX_LAYOUTS = 30
MAX_NAME = 40
REFERENCE_HEIGHT = 1080
MAX_BORDER = 40
MAX_RADIUS = 200
MAX_ZOOM = 4.0
MIN_SIZE = 0.05  # a box can't get smaller than this share of the picture
MIN_SHOWN = 0.05  # cropping always leaves at least this share of the stream's frame each way
STALE_S = 3.0  # a stream whose newest frame is older than this (it dropped) is left out until it's back
SNAP = 0.012  # how close an edge has to come to another to snap to it, as a share of the picture
DEFAULT_BACKGROUND = "#15151a"
_HEX = "0123456789abcdef"


@dataclass(frozen=True)
class Tile:
    """One stream's place in the picture."""

    source_id: str
    name: str = ""  # shown in the editor while that stream isn't running
    x: float = 0.0  # the box, as fractions of the picture
    y: float = 0.0
    w: float = 1.0
    h: float = 1.0
    zoom: float = 1.0  # how far in, and where the middle of what's shown is in the cropped frame (0..1)
    cx: float = 0.5
    cy: float = 0.5
    border: int = 0
    border_colour: str = "#ffffff"
    radius: int = 0
    crop_l: float = 0.0  # how much is cut off each side of the stream's frame, as fractions of it
    crop_t: float = 0.0
    crop_r: float = 0.0
    crop_b: float = 0.0
    stretch: bool = False  # fill the box whatever its shape; otherwise the middle of the crop is cut to the box's shape


@dataclass(frozen=True)
class Layout:
    name: str = ""
    tiles: tuple = ()  # back to front
    background: str = DEFAULT_BACKGROUND
    picture: str = ""  # a picture file behind the tiles instead of the colour


# ── Saving and loading ────────────────────────────────────────────────────

def _number(value, default: float, lo: float, hi: float) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        return default
    return min(hi, max(lo, float(value)))


def _colour(value, default: str) -> str:
    if isinstance(value, str) and len(value) == 7 and value[0] == "#" and all(c in _HEX for c in value[1:].lower()):
        return value.lower()
    return default


def _text(value, limit: int) -> str:
    return "".join(c for c in value if c.isprintable() and c not in "<>")[:limit].strip() if isinstance(value, str) else ""


def clean_tile(raw) -> Optional[Tile]:
    if not isinstance(raw, dict) or not isinstance(raw.get("source_id"), str):
        return None
    w = _number(raw.get("w"), 1.0, MIN_SIZE, 1.0)
    h = _number(raw.get("h"), 1.0, MIN_SIZE, 1.0)
    crop_l = _number(raw.get("crop_l"), 0.0, 0.0, 1.0 - MIN_SHOWN)
    crop_t = _number(raw.get("crop_t"), 0.0, 0.0, 1.0 - MIN_SHOWN)
    return Tile(
        source_id=raw["source_id"][:200], name=_text(raw.get("name"), MAX_NAME),
        x=_number(raw.get("x"), 0.0, 0.0, 1.0 - w), y=_number(raw.get("y"), 0.0, 0.0, 1.0 - h), w=w, h=h,
        zoom=_number(raw.get("zoom"), 1.0, 1.0, MAX_ZOOM),
        cx=_number(raw.get("cx"), 0.5, 0.0, 1.0), cy=_number(raw.get("cy"), 0.5, 0.0, 1.0),
        border=int(_number(raw.get("border"), 0, 0, MAX_BORDER)),
        border_colour=_colour(raw.get("border_colour"), "#ffffff"),
        radius=int(_number(raw.get("radius"), 0, 0, MAX_RADIUS)),
        crop_l=crop_l, crop_t=crop_t, crop_r=_number(raw.get("crop_r"), 0.0, 0.0, 1.0 - MIN_SHOWN - crop_l),
        crop_b=_number(raw.get("crop_b"), 0.0, 0.0, 1.0 - MIN_SHOWN - crop_t),
        stretch=raw.get("stretch") is True,
    )


def clean_layout(raw) -> Optional[Layout]:
    """A layout from saved settings, with anything broken or out of range fixed; None for something that isn't one."""
    if not isinstance(raw, dict):
        return None
    tiles, seen = [], set()
    for entry in raw.get("tiles") if isinstance(raw.get("tiles"), list) else []:
        tile = clean_tile(entry)
        if tile is not None and tile.source_id not in seen and len(tiles) < MAX_TILES:
            seen.add(tile.source_id)
            tiles.append(tile)
    picture = raw.get("picture")
    return Layout(name=_text(raw.get("name"), MAX_NAME), tiles=tuple(tiles),
                  background=_colour(raw.get("background"), DEFAULT_BACKGROUND),
                  picture=picture if isinstance(picture, str) and len(picture) < 4096 else "")


def clean_layouts(raw) -> list:
    """Saved layouts with a name each, the first of each name kept, at most MAX_LAYOUTS."""
    out, names = [], set()
    for entry in raw if isinstance(raw, list) else []:
        layout = clean_layout(entry)
        if layout is not None and layout.name and layout.name not in names and len(out) < MAX_LAYOUTS:
            names.add(layout.name)
            out.append(layout)
    return out


def layout_dict(layout: Layout) -> dict:
    return {"name": layout.name, "background": layout.background, "picture": layout.picture,
            "tiles": [{k: getattr(t, k) for k in Tile.__dataclass_fields__} for t in layout.tiles]}


def same_arrangement(a: Layout, b: Layout) -> bool:
    """Whether two layouts look the same, names aside."""
    return replace(a, name="") == replace(b, name="")


# ── Where things go ───────────────────────────────────────────────────────

def box_px(tile: Tile, width: int, height: int) -> tuple:
    """(x0, y0, x1, y1) of the tile's box in a width x height picture, at least a pixel each way."""
    x0, y0 = round(tile.x * width), round(tile.y * height)
    x1, y1 = round((tile.x + tile.w) * width), round((tile.y + tile.h) * height)
    x0, y0 = min(max(0, x0), width - 1), min(max(0, y0), height - 1)
    return x0, y0, min(width, max(x0 + 1, x1)), min(height, max(y0 + 1, y1))


def shown(tile: Tile, src_w: float, src_h: float, box_w: float, box_h: float) -> tuple:
    """(x0, y0, x1, y1) of the part of a src_w x src_h frame that fills a box_w x box_h box, unrounded. The crop is
    cut off first; then, unless the tile stretches, the biggest piece of what's left with the box's shape; the zoom
    makes that smaller around the tile's centre without leaving the crop."""
    rx, ry = tile.crop_l * src_w, tile.crop_t * src_h
    rw = max(1e-6, (1.0 - tile.crop_l - tile.crop_r) * src_w)
    rh = max(1e-6, (1.0 - tile.crop_t - tile.crop_b) * src_h)
    if tile.stretch:
        w, h = rw, rh
    else:
        aspect = box_w / max(1e-6, box_h)
        w, h = (rh * aspect, rh) if rw / rh > aspect else (rw, rw / aspect)
    w, h = w / tile.zoom, h / tile.zoom
    x0 = rx + min(max(0.0, tile.cx * rw - w / 2), rw - w)
    y0 = ry + min(max(0.0, tile.cy * rh - h / 2), rh - h)
    return x0, y0, x0 + w, y0 + h


def crop_px(tile: Tile, src_w: int, src_h: int, box_w: int, box_h: int) -> tuple:
    """(x0, y0, x1, y1) of shown() in whole pixels, at least one each way and inside the frame."""
    x0, y0, x1, y1 = shown(tile, src_w, src_h, box_w, box_h)
    w, h = max(1, round(x1 - x0)), max(1, round(y1 - y0))
    x0, y0 = min(max(0, round(x0)), src_w - 1), min(max(0, round(y0)), src_h - 1)
    return x0, y0, min(src_w, x0 + w), min(src_h, y0 + h)


def scaled_size(reference: int, height: int) -> int:
    """A border or corner size given for a 1080-line picture, in pixels of one height lines tall."""
    return round(reference * height / REFERENCE_HEIGHT) if reference else 0


def fit_box(aspect: float, cell: tuple, canvas_aspect: float) -> tuple:
    """(x, y, w, h) of the biggest box with a stream's shape (aspect, width over height in pixels) centred in cell,
    where the picture is canvas_aspect wide for each unit tall."""
    cx, cy, cw, ch = cell
    w = cw
    h = w * canvas_aspect / aspect
    if h > ch:
        h = ch
        w = h * aspect / canvas_aspect
    return cx + (cw - w) / 2, cy + (ch - h) / 2, w, h


def side_by_side(streams: list, canvas_aspect: float, gap: float = 0.02) -> tuple:
    """Tiles for streams [(source id, name, aspect)] next to each other, each as big as its shape allows."""
    n = len(streams)
    if not n:
        return ()
    cell_w = (1.0 - gap * (n + 1)) / n
    tiles = []
    for i, (sid, name, aspect) in enumerate(streams[:MAX_TILES]):
        x, y, w, h = fit_box(aspect, (gap + i * (cell_w + gap), gap, cell_w, 1.0 - 2 * gap), canvas_aspect)
        tiles.append(Tile(sid, name, x, y, w, h, radius=16))
    return tuple(tiles)


def big_and_small(streams: list, canvas_aspect: float) -> tuple:
    """The first stream filling the picture and the others small in the bottom corner, the newest on top. Upright
    phones in a short picture are made smaller so the stack still fits."""
    if not streams:
        return ()
    sid, name, _aspect = streams[0]
    tiles = [Tile(sid, name, 0.0, 0.0, 1.0, 1.0)]
    margin, width = 0.03, 0.26
    gap = margin * canvas_aspect  # the same gap up and down as across, in fractions of the height
    sizes = []
    for _sid, _name, aspect in streams[1:MAX_TILES]:
        h = width * canvas_aspect / aspect
        if h > 0.45:
            h = 0.45
        sizes.append(h)
    room = 1.0 - gap * (len(sizes) + 1)
    shrink = min(1.0, room / sum(sizes)) if sizes else 1.0
    bottom = 1.0 - gap
    for (sid, name, aspect), h in zip(streams[1:MAX_TILES], sizes):
        h *= shrink
        w = h * aspect / canvas_aspect
        y = bottom - h
        tiles.append(Tile(sid, name, 1.0 - margin - w, max(0.0, y), w, h, border=4, radius=24))
        bottom = y - gap
    return tuple(tiles)


def grid(streams: list, canvas_aspect: float, gap: float = 0.015) -> tuple:
    """Two by two (or two side by side for two streams), each as big as its shape allows in its cell."""
    if len(streams) <= 2:
        return side_by_side(streams, canvas_aspect)
    cell_w, cell_h = (1.0 - 3 * gap) / 2, (1.0 - 3 * gap) / 2
    tiles = []
    for i, (sid, name, aspect) in enumerate(streams[:MAX_TILES]):
        cell = (gap + (i % 2) * (cell_w + gap), gap + (i // 2) * (cell_h + gap), cell_w, cell_h)
        tiles.append(Tile(sid, name, *fit_box(aspect, cell, canvas_aspect), radius=12))
    return tuple(tiles)


def moved(tile: Tile, x: float, y: float) -> Tile:
    """tile with its box's top left at (x, y), kept inside the picture."""
    return replace(tile, x=min(max(0.0, x), 1.0 - tile.w), y=min(max(0.0, y), 1.0 - tile.h))


def resized(box: tuple, handle: str, dx: float, dy: float, aspect: Optional[float] = None) -> tuple:
    """box (x, y, w, h) with the edges handle names ("l", "r", "t", "b" or two of them, like "tl") dragged by dx, dy,
    kept inside the picture and no smaller than MIN_SIZE. aspect: keep w / h at that (the picture's own units)."""
    x, y, w, h = box
    x1, y1 = x + w, y + h
    if "l" in handle:
        x = min(max(0.0, x + dx), x1 - MIN_SIZE)
    if "r" in handle:
        x1 = max(min(1.0, x1 + dx), x + MIN_SIZE)
    if "t" in handle:
        y = min(max(0.0, y + dy), y1 - MIN_SIZE)
    if "b" in handle:
        y1 = max(min(1.0, y1 + dy), y + MIN_SIZE)
    w, h = x1 - x, y1 - y
    if aspect:
        if handle in ("t", "b") or (handle not in ("l", "r") and h * aspect > w):
            w = h * aspect  # a corner follows whichever way it was dragged further
        else:
            h = w / aspect
        # what doesn't fit shrinks both ways, from the corner that stays put
        right, bottom = "l" not in handle, "t" not in handle
        room_w = (1.0 - box[0]) if right else (box[0] + box[2])
        room_h = (1.0 - box[1]) if bottom else (box[1] + box[3])
        shrink = min(1.0, room_w / w, room_h / h)
        w, h = w * shrink, h * shrink
        x = box[0] if right else box[0] + box[2] - w
        y = box[1] if bottom else box[1] + box[3] - h
    return x, y, w, h


def scaled(box: tuple, handle: str, dx: float, dy: float) -> tuple:
    """box (x, y, w, h) made bigger or smaller by dragging handle, keeping its shape. A corner keeps the opposite corner
    where it is; a side keeps the opposite side and stays centred the other way."""
    x, y, w, h = box
    if len(handle) == 2:
        return resized(box, handle, dx, dy, aspect=w / h)
    _nx, _ny, nw, nh = resized(box, handle, dx, dy)
    s = nw / w if handle in ("l", "r") else nh / h
    other = h if handle in ("l", "r") else w  # the side that follows along
    s = min(max(s, MIN_SIZE / min(w, h)), 1.0 / other)
    nw, nh = w * s, h * s
    if handle in ("l", "r"):
        nx = x if handle == "r" else x + w - nw
        nx = min(max(0.0, nx), 1.0 - nw)
        return nx, min(max(0.0, y + (h - nh) / 2), 1.0 - nh), nw, nh
    ny = y if handle == "b" else y + h - nh
    ny = min(max(0.0, ny), 1.0 - nh)
    return min(max(0.0, x + (w - nw) / 2), 1.0 - nw), ny, nw, nh


def pinned(tile: Tile, src_aspect: float, canvas_aspect: float) -> Tile:
    """tile with what it shows right now written into its crop, at zoom 1: it looks the same, and its crop can then be
    moved side by side or stretched. src_aspect is the stream's width over height, canvas_aspect the picture's."""
    x0, y0, x1, y1 = shown(tile, src_aspect, 1.0, tile.w * canvas_aspect, tile.h)
    return replace(tile, crop_l=x0 / src_aspect, crop_t=y0, crop_r=1.0 - x1 / src_aspect, crop_b=1.0 - y1, zoom=1.0,
                   cx=0.5, cy=0.5)


def cropped(tile: Tile, handle: str, dx: float, dy: float) -> Tile:
    """tile with the sides handle names dragged by dx, dy, cutting off (or bringing back) that side of the stream while
    the rest of it stays where it is on the picture. The tile has to be pinned() first."""
    x, y, w, h = tile.x, tile.y, tile.w, tile.h
    x1, y1 = x + w, y + h
    nx, ny, nw, nh = resized((x, y, w, h), handle, dx, dy)
    nx1, ny1 = nx + nw, ny + nh
    l, t, r, b = tile.crop_l, tile.crop_t, tile.crop_r, tile.crop_b
    per_u = w / max(1e-6, 1.0 - l - r)  # picture per share of the stream's width
    per_v = h / max(1e-6, 1.0 - t - b)
    if "l" in handle:
        l2 = min(max(0.0, l + (nx - x) / per_u), 1.0 - r - MIN_SHOWN)
        nx, l = x + (l2 - l) * per_u, l2
    if "r" in handle:
        r2 = min(max(0.0, r - (nx1 - x1) / per_u), 1.0 - l - MIN_SHOWN)
        nx1, r = x1 - (r2 - r) * per_u, r2
    if "t" in handle:
        t2 = min(max(0.0, t + (ny - y) / per_v), 1.0 - b - MIN_SHOWN)
        ny, t = y + (t2 - t) * per_v, t2
    if "b" in handle:
        b2 = min(max(0.0, b - (ny1 - y1) / per_v), 1.0 - t - MIN_SHOWN)
        ny1, b = y1 - (b2 - b) * per_v, b2
    return replace(tile, x=nx, y=ny, w=nx1 - nx, h=ny1 - ny, crop_l=l, crop_t=t, crop_r=r, crop_b=b)


def dragged(tile: Tile, handle: str, dx: float, dy: float, how: str, canvas_aspect: float,
            src_aspect: Optional[float] = None) -> Tile:
    """tile after dragging handle by dx, dy, the way OBS does it. how is "scale" (keeps the shape), "stretch" (the box
    takes any shape and the stream is squashed to fill it) or "crop" (cuts off the side being dragged). Stretching and
    cropping need the stream's shape (src_aspect); without one they only change the box, as for a stream that's off."""
    box = (tile.x, tile.y, tile.w, tile.h)
    if how == "scale":
        x, y, w, h = scaled(box, handle, dx, dy)
        return replace(tile, x=x, y=y, w=w, h=h)
    if src_aspect is None:
        x, y, w, h = resized(box, handle, dx, dy)
        return replace(tile, x=x, y=y, w=w, h=h)
    tile = pinned(tile, src_aspect, canvas_aspect)
    if how == "crop":
        return cropped(tile, handle, dx, dy)
    x, y, w, h = resized(box, handle, dx, dy)
    return replace(tile, x=x, y=y, w=w, h=h, stretch=True)


def fitted(tile: Tile, src_aspect: float, canvas_aspect: float) -> Tile:
    """tile with its box the stream's own shape inside the old one, and no crop, stretch or zoom."""
    x, y, w, h = fit_box(src_aspect, (tile.x, tile.y, tile.w, tile.h), canvas_aspect)
    return replace(tile, x=x, y=y, w=w, h=h, zoom=1.0, cx=0.5, cy=0.5, crop_l=0.0, crop_t=0.0, crop_r=0.0, crop_b=0.0,
                   stretch=False)


def snap_lines(tiles: tuple, skip: int) -> tuple:
    """([x], [y]) an edge of the tile at index skip snaps to: the picture's edges and centre and the other tiles'."""
    xs, ys = [0.0, 0.5, 1.0], [0.0, 0.5, 1.0]
    for i, t in enumerate(tiles):
        if i != skip:
            xs += [t.x, t.x + t.w / 2, t.x + t.w]
            ys += [t.y, t.y + t.h / 2, t.y + t.h]
    return xs, ys


def snap(values: tuple, lines: list, reach: float = SNAP) -> tuple:
    """(how far to move, the line it snapped to or None) for the closest of values to come within reach of a line."""
    best = None
    for v in values:
        for line in lines:
            d = line - v
            if abs(d) <= reach and (best is None or abs(d) < abs(best[0])):
                best = (d, line)
    return best if best is not None else (0.0, None)


def raised(tiles: tuple, index: int, step: int) -> tuple:
    """tiles with the one at index moved step places towards the front (negative: towards the back)."""
    if not 0 <= index < len(tiles):
        return tiles
    to = min(max(0, index + step), len(tiles) - 1)
    items = list(tiles)
    items.insert(to, items.pop(index))
    return tuple(items)


# ── Drawing ───────────────────────────────────────────────────────────────

def bgr(colour: str) -> tuple:
    colour = _colour(colour, "#000000")
    return int(colour[5:7], 16), int(colour[3:5], 16), int(colour[1:3], 16)


@functools.lru_cache(maxsize=64)
def corner_masks(radius: int) -> tuple:
    """For each corner (top left, top right, bottom left, bottom right): (inside, outside) weights of a radius x radius
    square, smoothed a pixel wide at the curve. Worked out once per radius."""
    centres = np.arange(radius, dtype=np.float32) + 0.5
    dist = np.sqrt((radius - centres[None, :]) ** 2 + (radius - centres[:, None]) ** 2)
    inside = np.clip(radius - dist + 0.5, 0.0, 1.0).astype(np.float32)
    out = []
    for flip in (inside, inside[:, ::-1], inside[::-1, :], inside[::-1, ::-1]):
        a = np.ascontiguousarray(flip)
        out.append((a, np.ascontiguousarray(1.0 - a)))
    return tuple(out)


def _corners(x: int, y: int, w: int, h: int, r: int) -> tuple:
    return ((slice(y, y + r), slice(x, x + r)), (slice(y, y + r), slice(x + w - r, x + w)),
            (slice(y + h - r, y + h), slice(x, x + r)), (slice(y + h - r, y + h), slice(x + w - r, x + w)))


def paste(dst: np.ndarray, image, x: int, y: int, w: int, h: int, radius: int):
    """image (w x h, or a BGR colour) onto dst at (x, y), its corners rounded: everything is copied straight over and
    only the four corner squares are blended with what was under them."""
    r = min(radius, w // 2, h // 2)
    corners = _corners(x, y, w, h, r) if r > 0 else ()
    under = [dst[sy, sx].copy() for sy, sx in corners]
    dst[y:y + h, x:x + w] = image
    for (sy, sx), keep, (inside, outside) in zip(corners, under, corner_masks(r) if r > 0 else ()):
        dst[sy, sx] = cv2.blendLinear(np.ascontiguousarray(dst[sy, sx]), keep, inside, outside)


def scale_to(image: np.ndarray, w: int, h: int) -> np.ndarray:
    """image resized to w x h. Shrinking by half or more goes in exact halves with INTER_AREA first, which averages
    every pixel (no shimmer on fine detail) and is nearly free at exactly half; INTER_LINEAR does the last bit."""
    while image.shape[1] >= 2 * w and image.shape[0] >= 2 * h:
        ih, iw = image.shape[:2]
        image = cv2.resize(image[:ih - ih % 2, :iw - iw % 2], (iw // 2, ih // 2), interpolation=cv2.INTER_AREA)
    if image.shape[:2] == (h, w):
        return image
    return cv2.resize(image, (w, h), interpolation=cv2.INTER_LINEAR)


@dataclass(frozen=True)
class _Plan:
    layout: Layout
    size: tuple
    background: np.ndarray
    boxes: tuple  # per tile: (x, y, w, h) of the stream inside its border, border px, radius px, inner radius px


class Compositor:
    """Draws a layout. prepare() runs on the GUI thread (it may read the background picture) and render() on a
    stream's thread; the plan is swapped in whole, so a render never sees half of a change."""

    def __init__(self):
        self._plan: Optional[_Plan] = None
        self._scaled: dict = {}  # tile index: ((frame number, crop, size), scaled stream), for a frame seen before
        self._lock = threading.Lock()  # the preview and the stream may render at once
        self._pictures: dict = {}  # path: the picture as read, so moving a tile doesn't read it again
        self._backgrounds: dict = {}  # (picture, colour, size): the background drawn, so moving a tile reuses it

    def prepare(self, layout: Layout, size: tuple):
        width, height = size
        background = self._background(layout, width, height)
        boxes = []
        for tile in layout.tiles:
            x0, y0, x1, y1 = box_px(tile, width, height)
            border = min(scaled_size(tile.border, height), (x1 - x0) // 3, (y1 - y0) // 3)
            radius = scaled_size(tile.radius, height)
            boxes.append(((x0, y0, x1 - x0, y1 - y0), border, radius, max(0, radius - border)))
        self._plan = _Plan(layout, (width, height), background, tuple(boxes))

    def _background(self, layout: Layout, width: int, height: int) -> np.ndarray:
        """The picture behind the tiles, worked out once per picture, colour and size (render() only copies it)."""
        key = (layout.picture, layout.background, width, height)
        cached = self._backgrounds.get(key)
        if cached is not None:
            return cached
        picture = None
        if layout.picture:
            picture = self._pictures.get(layout.picture)
            if picture is None:
                picture = read_picture(layout.picture)
                if picture is not None:  # one that's missing now may be there next time
                    self._pictures = {layout.picture: picture}
        if picture is not None:
            background = cover(picture, width, height)
        else:
            background = np.empty((height, width, 3), np.uint8)
            background[:] = bgr(layout.background)
        if picture is not None or not layout.picture:
            self._backgrounds = {key: background}  # one at a time: a new size or colour replaces it
        return background

    @property
    def layout(self) -> Optional[Layout]:
        plan = self._plan
        return plan.layout if plan is not None else None

    def live_tiles(self, frames: dict) -> int:
        """How many of the layout's tiles frames has a picture for."""
        plan = self._plan
        return sum(1 for t in plan.layout.tiles if t.source_id in frames) if plan is not None else 0

    def render(self, frames: dict) -> Optional[np.ndarray]:
        """The picture, from frames {source id: (BGR frame, its number or None)}; tiles without a frame are left out.
        None before prepare()."""
        plan = self._plan
        if plan is None:
            return None
        with self._lock:
            out = plan.background.copy()
            for i, (tile, ((x, y, w, h), border, radius, inner_radius)) in enumerate(zip(plan.layout.tiles, plan.boxes)):
                src = frames.get(tile.source_id)
                if src is None:
                    continue
                if border:
                    paste(out, bgr(tile.border_colour), x, y, w, h, radius)
                    x, y, w, h = x + border, y + border, w - 2 * border, h - 2 * border
                if w > 0 and h > 0:
                    paste(out, self._scale(i, tile, src, w, h), x, y, w, h, inner_radius if border else radius)
            return out

    def _scale(self, index: int, tile: Tile, src: tuple, w: int, h: int) -> np.ndarray:
        frame, number = src
        fh, fw = frame.shape[:2]
        crop = crop_px(tile, fw, fh, w, h)
        key = (number, crop, w, h)
        cached = self._scaled.get(index)
        if number is not None and cached is not None and cached[0] == key:
            return cached[1]  # a slower stream's frame that hasn't changed since the last picture
        x0, y0, x1, y1 = crop
        piece = frame[y0:y1, x0:x1]
        if (x1 - x0, y1 - y0) == (w, h):
            return piece  # already the right size: paste() copies it straight in
        scaled = scale_to(piece, w, h)
        self._scaled[index] = (key, scaled)
        return scaled
