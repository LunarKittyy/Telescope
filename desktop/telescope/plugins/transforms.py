from dataclasses import asdict, dataclass
from typing import Optional

import cv2
import numpy as np

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QWidget,
)

from telescope.plugin import TelescopePlugin
from telescope.widgets.common import (
    NoScrollComboBox, NoScrollSlider, PanSliderRow, SegmentButton, add_card_header,
    add_section_heading, control_row as _row, card_layout, create_card, card_action,
    segmented_row, slider_row, value_label,
)

ROTATIONS = {
    "None":   None,
    "90 CW":  cv2.ROTATE_90_CLOCKWISE,
    "180":    cv2.ROTATE_180,
    "90 CCW": cv2.ROTATE_90_COUNTERCLOCKWISE,
}


def _apply_zoom(frame, zoom: float, pan_x: float, pan_y: float):
    if zoom <= 1.0:
        return frame
    h, w = frame.shape[:2]
    x0, y0, crop_w, crop_h = _zoom_origin(w, h, zoom, pan_x, pan_y)
    return cv2.resize(frame[y0:y0 + crop_h, x0:x0 + crop_w], (w, h),
                      interpolation=cv2.INTER_LINEAR)


def _zoom_origin(w: int, h: int, zoom: float, pan_x: float, pan_y: float) -> tuple:
    """The crop _apply_zoom takes: (x0, y0, crop_w, crop_h) in source pixels."""
    crop_w = int(w / zoom)
    crop_h = int(h / zoom)
    max_dx = (w - crop_w) // 2
    max_dy = (h - crop_h) // 2
    cx = max_dx + int(pan_x * max_dx)
    cy = max_dy + int(pan_y * max_dy)
    return max(0, min(cx, w - crop_w)), max(0, min(cy, h - crop_h)), crop_w, crop_h


# ── Zoom: the phone crops what it can, this computer the rest ────────────────
# Cropping on the phone cuts from the full-resolution sensor (and lets a multi-lens camera switch to
# its telephoto); cropping here enlarges the already scaled-down stream. Framing is the same either way.

@dataclass(frozen=True)
class PhoneZoomCaps:
    """How far the current lens can zoom itself, from its /v1/state entry."""
    ratio_max: float = 1.0  # CONTROL_ZOOM_RATIO: always centred; a multi-lens camera picks its lens by it
    crop_max: float = 1.0   # SCALER_CROP_REGION, on top of the ratio
    freeform: bool = False  # the crop can sit off-centre

    @classmethod
    def from_camera(cls, cam: Optional[dict]) -> Optional["PhoneZoomCaps"]:
        """None when the lens can't zoom itself (or the phone app predates phone-side zoom)."""
        if not cam:
            return None
        caps = cls(max(1.0, cam.get("zoomRatioMax") or 1.0), max(1.0, cam.get("cropZoomMax") or 1.0),
                   cam.get("freeformCrop", False) is True)
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


def _window_centre(zoom: float, pan: float) -> float:
    """Where _zoom_origin puts the crop's centre along one axis, 0..1."""
    return 0.5 + pan * (1.0 - 1.0 / zoom) / 2.0


def _pan_within(centre: float, view_centre: float, view: float, size: float) -> float:
    """The pan that puts a window of `size` at `centre` inside a view of `view` at `view_centre`."""
    slack = (view - size) / 2.0
    return 0.0 if slack < 1e-6 else min(max((centre - view_centre) / slack, -1.0), 1.0)


def split_zoom(zoom: float, pan_x: float, pan_y: float, caps: Optional[PhoneZoomCaps]) -> ZoomSplit:
    """The framing asked for, as the phone's share plus what's left for this computer.

    The phone's ratio is the largest centred zoom the window still fits in: centred, that's all of it
    (and the telephoto comes in); panned to an edge, it falls back towards 1. A freeform crop then cuts
    the window itself; a centred one only as far as the window still fits.
    """
    if caps is None:
        return ZoomSplit(None, (zoom, pan_x, pan_y))
    zoom = max(zoom, 1.0)
    size = 1.0 / zoom
    centre = (_window_centre(zoom, pan_x), _window_centre(zoom, pan_y))
    fit = 1.0 / (2.0 * max(abs(c - 0.5) for c in centre) + size)
    ratio = max(1.0, min(fit, caps.ratio_max))
    total = max(ratio, min(zoom if caps.freeform else fit, ratio * caps.crop_max))
    view = 1.0 / total
    room = (1.0 / ratio - view) / 2.0 if caps.freeform else 0.0  # how far the crop can move off-centre
    vx, vy = (min(max(c, 0.5 - room), 0.5 + room) for c in centre)
    phone = PhoneZoom(round(ratio, 4), round(total / ratio, 4),
                      round((vx - 0.5) * ratio + 0.5, 4), round((vy - 0.5) * ratio + 0.5, 4))
    rest = zoom / total
    if rest < 1.0 + 1e-3:
        return ZoomSplit(phone, (1.0, 0.0, 0.0))
    return ZoomSplit(phone, (rest, _pan_within(centre[0], vx, view, size), _pan_within(centre[1], vy, view, size)))


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
        x0, y0, crop_w, crop_h = _zoom_origin(w, h, zoom, pan_x, pan_y)
        u = (x0 + u * crop_w) / w
        v = (y0 + v * crop_h) / h
    return min(max(u, 0.0), 1.0), min(max(v, 0.0), 1.0)


def _transform_frame(frame, flip_h: bool, flip_v: bool, rotation):
    if flip_h and flip_v: frame = cv2.flip(frame, -1)
    elif flip_h:          frame = cv2.flip(frame,  1)
    elif flip_v:          frame = cv2.flip(frame,  0)
    if rotation is not None: frame = cv2.rotate(frame, rotation)
    return frame


class TransformsPlugin(TelescopePlugin):
    name = "transforms"
    panel_region = "left"   # desktop-side processing, with the output settings

    def setup(self, host, bus):
        self._host = host
        self.flip_h   = False  # Written by Qt thread; read by worker thread (GIL ensures atomicity).
        self.flip_v   = False
        self.rotation = None
        self.zoom     = 1.0
        self.pan_x    = 0.0
        self.pan_y    = 0.0
        self._frame_size = (0, 0)  # the phone's frame, for mapping preview clicks back onto it
        self._desktop_crop = (1.0, 0.0, 0.0)  # the zoom/pan share left for this computer (split_zoom)
        self._ctrl = None
        self._zoom_caps: Optional[PhoneZoomCaps] = None
        self._sent_zoom: Optional[PhoneZoom] = None
        self._bus = bus
        bus.focus_point_picked.connect(self._on_point_picked)
        bus.camera_switched.connect(self._on_camera_caps)

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        reset_btn = card_action("Reset", "reset", "Clear flip, rotation, zoom and pan back to defaults")
        reset_btn.clicked.connect(self._reset_all)
        add_card_header(lay, "Transforms", "transforms", action=reset_btn)

        # ── Flip ─────────────────────────────────────────────────────────────
        add_section_heading(lay, "Orientation")
        self._flip_h = SegmentButton("Horizontal")
        self._flip_v = SegmentButton("Vertical")
        self._flip_h.toggled.connect(self._on_flip)
        self._flip_v.toggled.connect(self._on_flip)
        lay.addLayout(_row("Flip", segmented_row(self._flip_h, self._flip_v), stretch=True))

        # ── Rotation ──────────────────────────────────────────────────────────
        self._rot_combo = NoScrollComboBox()
        self._rot_combo.addItems(list(ROTATIONS.keys()))
        self._rot_combo.currentTextChanged.connect(self._on_rotate)
        lay.addLayout(_row("Rotation", self._rot_combo, stretch=True))

        # ── Zoom ──────────────────────────────────────────────────────────────
        add_section_heading(lay, "Framing")
        self._zoom_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._zoom_slider.setRange(100, 500)
        self._zoom_slider.setValue(100)
        self._zoom_val_lbl = value_label("1.0×")
        self._zoom_slider.valueChanged.connect(self._on_zoom_changed)
        lay.addLayout(_row("Zoom", slider_row(self._zoom_slider, self._zoom_val_lbl), stretch=True))

        # ── Pan ───────────────────────────────────────────────────────────────
        self._pan_x_slider = PanSliderRow(show_end_labels=False)
        self._pan_x_slider.value_changed.connect(self._on_pan_changed)
        self._pan_x_lbl = value_label()
        lay.addLayout(_row("Pan left/right", slider_row(self._pan_x_slider, self._pan_x_lbl), stretch=True))

        self._pan_y_slider = PanSliderRow(show_end_labels=False)
        self._pan_y_slider.value_changed.connect(self._on_pan_changed)
        self._pan_y_lbl = value_label()
        lay.addLayout(_row("Pan up/down", slider_row(self._pan_y_slider, self._pan_y_lbl), stretch=True))

        self._pan_x_slider.set_enabled(False)
        self._pan_y_slider.set_enabled(False)
        self._show_pan()

        return card

    def _reset_all(self):
        """Reset to defaults; handlers fire from widget changes."""
        self._flip_h.setChecked(False)
        self._flip_v.setChecked(False)
        self._rot_combo.setCurrentIndex(0)
        self._zoom_slider.setValue(100)
        self._pan_x_slider.reset()
        self._pan_y_slider.reset()
        self._on_pan_changed(0.0)

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        self._frame_size = (frame.shape[1], frame.shape[0])
        frame = _apply_zoom(frame, *self._desktop_crop)
        return _transform_frame(frame, self.flip_h, self.flip_v, self.rotation)

    # ── Phone-side zoom ───────────────────────────────────────────────────────

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        self._sent_zoom = None  # a new connection (or a reconnect): tell the phone again
        self._sync_zoom()

    def on_stream_stop(self):
        self._ctrl = None
        self._zoom_caps = None
        self._sync_zoom()

    def on_phone_state(self, state: dict):
        self._on_camera_caps(next((c for c in state.get("cameras", []) if c.get("current")), None))

    def _on_camera_caps(self, cam: Optional[dict]):
        caps = PhoneZoomCaps.from_camera(cam)
        if caps != self._zoom_caps:
            self._zoom_caps = caps
            self._sync_zoom()

    def _sync_zoom(self):
        """Split the framing between phone and desktop; the phone only hears about changes."""
        split = split_zoom(self.zoom, self.pan_x, self.pan_y, self._zoom_caps if self._ctrl else None)
        self._desktop_crop = split.desktop
        if split.phone is not None and split.phone != self._sent_zoom:
            self._sent_zoom = split.phone
            self._ctrl.send(action="zoom", **asdict(split.phone))
        if split.phone is None:
            where = "Zoomed on this computer" + (": this lens can't zoom itself" if self._ctrl else "")
        elif split.desktop[0] > 1.0:
            where = "Zoomed on the phone's sensor as far as this lens allows, the rest on this computer"
        else:
            where = "Zoomed on the phone's sensor"
        self._zoom_val_lbl.setToolTip(where)

    # ── Handlers (Qt thread) ──────────────────────────────────────────────────

    def _on_point_picked(self, u: float, v: float):
        w, h = self._frame_size
        self._bus.focus_point.emit(*inverse_map(u, v, w, h, *self._desktop_crop,
                                                self.flip_h, self.flip_v, self.rotation))

    def _on_flip(self):
        self.flip_h = self._flip_h.isChecked()
        self.flip_v = self._flip_v.isChecked()
        self._host.schedule_save()

    def _on_rotate(self):
        self.rotation = ROTATIONS.get(self._rot_combo.currentText())
        self._host.schedule_save()

    def _on_zoom_changed(self, val: int):
        self.zoom = val / 100.0
        self._zoom_val_lbl.setText(f"{self.zoom:.1f}×")
        pan_active = self.zoom > 1.0
        self._pan_x_slider.set_enabled(pan_active)
        self._pan_y_slider.set_enabled(pan_active)
        self._show_pan()
        if not pan_active:
            self._pan_x_slider.reset()
            self._pan_y_slider.reset()
            self.pan_x = 0.0
            self.pan_y = 0.0
        else:
            self.pan_x = self._pan_x_slider.get_value()
            self.pan_y = self._pan_y_slider.get_value()
        self._show_pan()
        self._sync_zoom()
        self._host.schedule_save()

    def _on_pan_changed(self, _val: float):
        self.pan_x = self._pan_x_slider.get_value()
        self.pan_y = self._pan_y_slider.get_value()
        self._show_pan()
        self._sync_zoom()
        self._host.schedule_save()

    def _show_pan(self):
        for lbl, slider in ((self._pan_x_lbl, self._pan_x_slider), (self._pan_y_lbl, self._pan_y_slider)):
            pct = round(slider.get_value() * 100)
            lbl.setText(f"{pct:+d}%" if pct else "0%")
            lbl.setEnabled(slider._slider.isEnabled())

    # ── Config ────────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {
            "flip_h":    self._flip_h.isChecked(),
            "flip_v":    self._flip_v.isChecked(),
            "rotation":  self._rot_combo.currentText(),
            "zoom":      self._zoom_slider.value() / 100.0,
            "pan_x":     self._pan_x_slider.get_value(),
            "pan_y":     self._pan_y_slider.get_value(),
        }

    def set_config(self, cfg: dict):
        self._flip_h.setChecked(cfg.get("flip_h", False))
        self._flip_v.setChecked(cfg.get("flip_v", False))
        if rot := cfg.get("rotation"):
            idx = self._rot_combo.findText(rot)
            if idx >= 0:
                self._rot_combo.setCurrentIndex(idx)
        zoom = cfg.get("zoom", 1.0)
        self._zoom_slider.setValue(int(zoom * 100))
        pan_active = zoom > 1.0
        self.pan_x = cfg.get("pan_x", 0.0) if pan_active else 0.0
        self.pan_y = cfg.get("pan_y", 0.0) if pan_active else 0.0
        self._pan_x_slider.set_value(self.pan_x)
        self._pan_y_slider.set_value(self.pan_y)
        self._pan_x_slider.set_enabled(pan_active)
        self._pan_y_slider.set_enabled(pan_active)
        self._show_pan()
        self._sync_zoom()
