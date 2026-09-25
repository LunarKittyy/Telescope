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
        self._bus = bus
        bus.focus_point_picked.connect(self._on_point_picked)

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
        frame = _apply_zoom(frame, self.zoom, self.pan_x, self.pan_y)
        return _transform_frame(frame, self.flip_h, self.flip_v, self.rotation)

    # ── Handlers (Qt thread) ──────────────────────────────────────────────────

    def _on_point_picked(self, u: float, v: float):
        w, h = self._frame_size
        self._bus.focus_point.emit(*inverse_map(u, v, w, h, self.zoom, self.pan_x, self.pan_y,
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
        self._host.schedule_save()

    def _on_pan_changed(self, _val: float):
        self.pan_x = self._pan_x_slider.get_value()
        self.pan_y = self._pan_y_slider.get_value()
        self._show_pan()
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
