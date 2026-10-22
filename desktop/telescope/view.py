"""Zoom, pan, flip and rotation of a stream's picture, and how the zoom is shared with the phone. No Qt here."""
import math
from dataclasses import dataclass
from typing import Optional

import cv2
import numpy as np

ROTATIONS = {
    "None":   None,
    "90 CW":  cv2.ROTATE_90_CLOCKWISE,
    "180":    cv2.ROTATE_180,
    "90 CCW": cv2.ROTATE_90_COUNTERCLOCKWISE,
}


@dataclass(frozen=True)
class View:
    """What's done to a stream's picture on this computer, before the plugins see it."""
    zoom: float = 1.0
    pan_x: float = 0.0
    pan_y: float = 0.0
    flip_h: bool = False
    flip_v: bool = False
    rotation: Optional[int] = None  # one of ROTATIONS' values


def apply_zoom(frame, zoom: float, pan_x: float, pan_y: float):
    if zoom <= 1.0:
        return frame
    h, w = frame.shape[:2]
    x0, y0, _crop_w, _crop_h = zoom_origin(w, h, zoom, pan_x, pan_y)
    # A fractional crop: one cut at whole pixels made a slowly moving framing shake and breathe
    move = np.float32([[zoom, 0, zoom * (0.5 - x0) - 0.5], [0, zoom, zoom * (0.5 - y0) - 0.5]])
    return cv2.warpAffine(frame, move, (w, h), flags=cv2.INTER_LINEAR, borderMode=cv2.BORDER_REPLICATE)


def zoom_origin(w: int, h: int, zoom: float, pan_x: float, pan_y: float) -> tuple:
    """The crop apply_zoom takes: (x0, y0, crop_w, crop_h) in source pixels, fractional."""
    zoom = max(zoom, 1.0)
    (x0, size_x), (y0, size_y) = window(zoom, min(max(pan_x, -1.0), 1.0)), window(zoom, min(max(pan_y, -1.0), 1.0))
    return x0 * w, y0 * h, size_x * w, size_y * h


# ── Zoom: the phone crops what it can, this computer the rest ────────────────
# Cropping on the phone cuts from the full-resolution sensor (and lets a multi-lens camera switch to
# its telephoto); cropping here enlarges the already scaled-down stream. Framing is the same either way.
# The phone's zoom ratio only ever steps between its lenses: phones animate ratio changes, so a ratio
# that followed the pan made the picture breathe in and out. Pan moves the crop, which doesn't animate.

_LENS_MARGIN = 1.05  # a lens is taken only once the window sits this far inside its view (no flip-flopping)

@dataclass(frozen=True)
class PhoneZoomCaps:
    """How far the current lens can zoom itself, from its /v1/state entry."""
    ratio_max: float = 1.0  # CONTROL_ZOOM_RATIO: always centred; a multi-lens camera picks its lens by it
    crop_max: float = 1.0   # SCALER_CROP_REGION, on top of the ratio
    freeform: bool = False  # the crop can sit off-centre
    lens_zooms: tuple = ()  # the ratios where a multi-lens camera's longer lenses take over, ascending

    @classmethod
    def from_camera(cls, cam: Optional[dict]) -> Optional["PhoneZoomCaps"]:
        """None when the lens can't zoom itself (or the phone app predates phone-side zoom)."""
        if not cam:
            return None
        ratio_max = max(1.0, cam.get("zoomRatioMax") or 1.0)
        lenses = tuple(sorted(z for z in cam.get("lensZooms") or () if 1.0 < z <= ratio_max))
        caps = cls(ratio_max, max(1.0, cam.get("cropZoomMax") or 1.0), cam.get("freeformCrop", False) is True,
                   lenses)
        return caps if caps.ratio_max > 1.0 or caps.crop_max > 1.0 else None


@dataclass(frozen=True)
class PhoneZoom:
    """The phone's `zoom` control: a centred ratio, then a 1/crop box around (x, y) of that view (0..1)."""
    ratio: float = 1.0
    crop: float = 1.0
    x: float = 0.5
    y: float = 0.5


@dataclass(frozen=True)
class ZoomSplit:
    phone: Optional[PhoneZoom]  # None: this lens can't zoom itself, so the phone is left alone
    desktop: tuple              # (zoom, pan_x, pan_y) still to crop from the frame that arrives


def window_centre(zoom: float, pan: float) -> float:
    """Where zoom_origin puts the crop's centre along one axis, 0..1."""
    return 0.5 + pan * (1.0 - 1.0 / zoom) / 2.0


def _pan_within(centre: float, view_centre: float, view: float, size: float) -> float:
    """The pan that puts a window of `size` at `centre` inside a view of `view` at `view_centre`."""
    slack = (view - size) / 2.0
    return 0.0 if slack < 1e-6 else min(max((centre - view_centre) / slack, -1.0), 1.0)


def _lens_ratio(fit: float, caps: PhoneZoomCaps, current_ratio: float, zoom: float) -> float:
    """The longest lens whose (centred) view still holds the window; 1 = the main lens.

    `fit` is the largest centred zoom the window fits in. Moving onto a lens needs a margin, staying on
    the current one doesn't, so the lens doesn't flip back and forth at the edge of its view. A centred
    window (fit == zoom) always gets its lens, or zooms just past a lens could never get back onto it.
    """
    ratio = 1.0
    for lens in caps.lens_zooms:
        on_it = abs(lens - current_ratio) < 1e-3
        # 1/(1/z) can land a hair under z: without the slack, a zoom right on a lens mark wouldn't get the lens
        if fit * (1 + 1e-9) >= (lens if on_it else max(lens, min(lens * _LENS_MARGIN, zoom))):
            ratio = lens
    return ratio


def split_zoom(zoom: float, pan_x: float, pan_y: float, caps: Optional[PhoneZoomCaps],
               current_ratio: float = 1.0) -> ZoomSplit:
    """The framing asked for, as the phone's share plus what's left for this computer.

    The phone's ratio picks the lens (see _lens_ratio; `current_ratio` is the one it's on now). A
    freeform crop then cuts the window itself out of that lens's view; with a centre-only crop the
    phone stops at the lens, so its share doesn't change with the pan, and this computer does the rest.
    """
    if caps is None:
        return ZoomSplit(None, (zoom, pan_x, pan_y))
    zoom = max(zoom, 1.0)
    size = 1.0 / zoom
    centre = (window_centre(zoom, pan_x), window_centre(zoom, pan_y))
    fit = 1.0 / (2.0 * max(abs(c - 0.5) for c in centre) + size)
    ratio = _lens_ratio(fit, caps, current_ratio, zoom)
    total = max(ratio, min(zoom, ratio * caps.crop_max)) if caps.freeform else ratio
    view = 1.0 / total
    room = (1.0 / ratio - view) / 2.0 if caps.freeform else 0.0  # how far the crop can move off-centre
    vx, vy = (min(max(c, 0.5 - room), 0.5 + room) for c in centre)
    phone = PhoneZoom(round(ratio, 4), round(total / ratio, 4),
                      round((vx - 0.5) * ratio + 0.5, 4), round((vy - 0.5) * ratio + 0.5, 4))
    rest = zoom / total
    if rest < 1.0 + 1e-3:
        return ZoomSplit(phone, (1.0, 0.0, 0.0))
    return ZoomSplit(phone, (rest, _pan_within(centre[0], vx, view, size), _pan_within(centre[1], vy, view, size)))


def phone_window(phone: Optional[PhoneZoom]) -> tuple:
    """The (zoom, pan_x, pan_y) of the full view that a phone share sends."""
    if phone is None:
        return (1.0, 0.0, 0.0)
    total = phone.ratio * phone.crop
    return (total, pan_for(0.5 + (phone.x - 0.5) / phone.ratio, total), pan_for(0.5 + (phone.y - 0.5) / phone.ratio, total))


def rest_within(zoom: float, pan_x: float, pan_y: float, phone: PhoneZoom, clamp: bool = False) -> Optional[tuple]:
    """What this computer crops for the framing asked for when the phone stays on phone's share, or None when the
    framing doesn't fit inside what the phone sends (it would have to be told something else). clamp: the nearest
    framing that does fit instead of None."""
    total = phone.ratio * phone.crop
    zoom = max(zoom, total) if clamp else max(zoom, 1.0)
    size, view = 1.0 / zoom, 1.0 / total
    centre = (window_centre(zoom, pan_x), window_centre(zoom, pan_y))
    view_centre = (0.5 + (phone.x - 0.5) / phone.ratio, 0.5 + (phone.y - 0.5) / phone.ratio)
    fits = zoom >= total - 1e-6 and all(abs(c - v) + size / 2 <= view / 2 + 1e-6 for c, v in zip(centre, view_centre))
    if not fits and not clamp:
        return None
    rest = zoom / total
    if rest < 1.0 + 1e-3:
        return (1.0, 0.0, 0.0)
    return (rest, _pan_within(centre[0], view_centre[0], view, size), _pan_within(centre[1], view_centre[1], view, size))


def share_shift(old: PhoneZoom, new: PhoneZoom) -> tuple:
    """(scale, dx, dy): where a point at u (0..1) of a frame cropped to old lands in one cropped to new,
    u * scale + d (the same scale on both axes, since a share keeps the picture's shape)."""
    (oz, opx, opy), (nz, npx, npy) = phone_window(old), phone_window(new)
    (ox, osize), (oy, _) = window(oz, opx), window(oz, opy)
    (nx, nsize), (ny, _) = window(nz, npx), window(nz, npy)
    return osize / nsize, (ox - nx) / nsize, (oy - ny) / nsize


def lens_step(lens: float) -> int:
    """A lens's ratio as a Zoom slider value (x100), rounded up: a hair under the lens wouldn't get it."""
    return math.ceil(lens * 100 - 1e-6)


def inverse_map(u: float, v: float, w: int, h: int, zoom: float = 1.0, pan_x: float = 0.0,
                pan_y: float = 0.0, flip_h: bool = False, flip_v: bool = False, rotation=None) -> tuple:
    """A point in the transformed frame (u, v in 0..1) back to the phone's frame (w x h before transforms).

    Undoes process_frame in reverse: rotation, then flip, then zoom/pan.
    """
    if rotation == cv2.ROTATE_90_CLOCKWISE:
        u, v = v, 1.0 - u
    elif rotation == cv2.ROTATE_180:
        u, v = 1.0 - u, 1.0 - v
    elif rotation == cv2.ROTATE_90_COUNTERCLOCKWISE:
        u, v = 1.0 - v, u
    if flip_h:
        u = 1.0 - u
    if flip_v:
        v = 1.0 - v
    if zoom > 1.0 and w > 0 and h > 0:
        x0, y0, crop_w, crop_h = zoom_origin(w, h, zoom, pan_x, pan_y)
        u = (x0 + u * crop_w) / w
        v = (y0 + v * crop_h) / h
    return min(max(u, 0.0), 1.0), min(max(v, 0.0), 1.0)


def window(zoom: float, pan: float) -> tuple:
    """The output's window along one axis of the full view: (left edge, size), both 0..1."""
    zoom = max(zoom, 1.0)
    return window_centre(zoom, pan) - 0.5 / zoom, 1.0 / zoom


def shown_to_view(u: float, v: float, zoom: float, pan_x: float, pan_y: float, flip_h: bool = False,
                  flip_v: bool = False, rotation=None) -> tuple:
    """A point in the frame as shown (after crop, flip and rotation) to the full view of the main lens.

    Continuous and unclamped (a drag can leave the frame), and by the zoom asked for: how the phone and
    this computer share it doesn't change where things are. inverse_map is the pixel-exact one for focus.
    """
    if rotation == cv2.ROTATE_90_CLOCKWISE:
        u, v = v, 1.0 - u
    elif rotation == cv2.ROTATE_180:
        u, v = 1.0 - u, 1.0 - v
    elif rotation == cv2.ROTATE_90_COUNTERCLOCKWISE:
        u, v = 1.0 - v, u
    if flip_h:
        u = 1.0 - u
    if flip_v:
        v = 1.0 - v
    (x0, w), (y0, h) = window(zoom, pan_x), window(zoom, pan_y)
    return x0 + u * w, y0 + v * h


def view_to_shown(x: float, y: float, zoom: float, pan_x: float, pan_y: float, flip_h: bool = False,
                  flip_v: bool = False, rotation=None) -> tuple:
    """The reverse of shown_to_view."""
    (x0, w), (y0, h) = window(zoom, pan_x), window(zoom, pan_y)
    u, v = (x - x0) / w, (y - y0) / h
    if flip_h:
        u = 1.0 - u
    if flip_v:
        v = 1.0 - v
    if rotation == cv2.ROTATE_90_CLOCKWISE:
        u, v = 1.0 - v, u
    elif rotation == cv2.ROTATE_180:
        u, v = 1.0 - u, 1.0 - v
    elif rotation == cv2.ROTATE_90_COUNTERCLOCKWISE:
        u, v = v, 1.0 - u
    return u, v


def pan_for(centre: float, zoom: float) -> float:
    """The pan that puts the window's centre at `centre` (0..1 of the full view), clamped to -1..1."""
    slack = 1.0 - 1.0 / max(zoom, 1.0)
    if slack < 1e-9:
        return 0.0
    return min(max((centre - 0.5) * 2.0 / slack, -1.0), 1.0)


def zoom_detent(old: int, new: int, marks) -> int:
    """A zoom step (slider values) that stops on the first lens mark it crosses; the next one goes past."""
    if new > old:
        return min((m for m in marks if old < m <= new), default=new)
    if new < old:
        return max((m for m in marks if new <= m < old), default=new)
    return new


def lens_boxes(caps: Optional[PhoneZoomCaps], zoom: float, pan_x: float, pan_y: float, flip_h: bool = False,
               flip_v: bool = False, rotation=None) -> list:
    """What each longer lens sees, as (label, x0, y0, x1, y1) in the frame as shown (may run off it).

    A lens at ratio r sees the centred 1/r of the main lens's view. Nominal: the lenses sit a few mm
    apart, so the real telephoto view is shifted a little, more so up close.
    """
    boxes = []
    for lens in caps.lens_zooms if caps else ():
        half = 0.5 / lens
        corners = [view_to_shown(x, y, zoom, pan_x, pan_y, flip_h, flip_v, rotation)
                   for x in (0.5 - half, 0.5 + half) for y in (0.5 - half, 0.5 + half)]
        xs, ys = [c[0] for c in corners], [c[1] for c in corners]
        boxes.append((f"{round(lens, 1):g}×", min(xs), min(ys), max(xs), max(ys)))
    return boxes


def transform_frame(frame, flip_h: bool, flip_v: bool, rotation):
    if flip_h and flip_v: frame = cv2.flip(frame, -1)
    elif flip_h:          frame = cv2.flip(frame,  1)
    elif flip_v:          frame = cv2.flip(frame,  0)
    if rotation is not None: frame = cv2.rotate(frame, rotation)
    return frame


@dataclass(frozen=True)
class ViewStatus:
    """How the shown stream's zoom is shared with the phone right now, for the Transforms card's lens dot."""
    streaming: bool = False                 # a phone is there to tell (False: everything is cropped here)
    caps: Optional[PhoneZoomCaps] = None    # what the current lens can zoom itself
    sent: Optional[PhoneZoom] = None        # what the phone was told last
    sent_ratio_at: float = 0.0              # time.monotonic() when it was last sent a new ratio
    desktop_zoom: float = 1.0               # the share this computer crops
