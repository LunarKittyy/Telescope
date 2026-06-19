import cv2
import numpy as np

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QCheckBox, QFrame, QHBoxLayout, QLabel, QVBoxLayout, QWidget,
)

from phonecam.plugin import PhoneCamPlugin
from phonecam.widgets.common import (
    NoScrollComboBox, NoScrollSlider, PanSliderRow, create_separator, create_vector_icon,
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
    crop_w = int(w / zoom)
    crop_h = int(h / zoom)
    max_dx = (w - crop_w) // 2
    max_dy = (h - crop_h) // 2
    cx = max_dx + int(pan_x * max_dx)
    cy = max_dy + int(pan_y * max_dy)
    x0 = max(0, min(cx, w - crop_w))
    y0 = max(0, min(cy, h - crop_h))
    return cv2.resize(frame[y0:y0 + crop_h, x0:x0 + crop_w], (w, h),
                      interpolation=cv2.INTER_LINEAR)


def _transform_frame(frame, flip_h: bool, flip_v: bool, rotation):
    if flip_h and flip_v: frame = cv2.flip(frame, -1)
    elif flip_h:          frame = cv2.flip(frame,  1)
    elif flip_v:          frame = cv2.flip(frame,  0)
    if rotation is not None: frame = cv2.rotate(frame, rotation)
    return frame


class TransformsPlugin(PhoneCamPlugin):
    name = "transforms"

    def setup(self, host, bus):
        self._host = host
        # Written by Qt thread; read atomically by process_frame on worker thread (GIL).
        self.flip_h   = False
        self.flip_v   = False
        self.rotation = None
        self.zoom     = 1.0
        self.pan_x    = 0.0
        self.pan_y    = 0.0

    def create_panel(self) -> QWidget:
        card = QFrame()
        card.setFrameShape(QFrame.Shape.StyledPanel)
        card.setObjectName("card")
        lay = QVBoxLayout(card)
        lay.setContentsMargins(14, 14, 14, 14)
        lay.setSpacing(10)

        hdr = QHBoxLayout()
        hdr.setContentsMargins(0, 0, 0, 4)
        hdr.setSpacing(8)
        icon_lbl = QLabel()
        icon_lbl.setPixmap(create_vector_icon("transforms", "#518cc6").pixmap(18, 18))
        icon_lbl.setFixedSize(18, 18)
        hdr.addWidget(icon_lbl)
        title_lbl = QLabel("Transforms")
        title_lbl.setObjectName("card_title")
        hdr.addWidget(title_lbl)
        hdr.addStretch()
        lay.addLayout(hdr)

        # ── Flip ─────────────────────────────────────────────────────────────
        flip_row = QHBoxLayout()
        flip_row.setContentsMargins(0, 0, 0, 0)
        fl = QLabel("Flip")
        fl.setObjectName("dim")
        fl.setFixedWidth(110)
        fl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        flip_row.addWidget(fl)
        self._flip_h = QCheckBox("Horizontal")
        self._flip_v = QCheckBox("Vertical")
        self._flip_h.toggled.connect(self._on_flip)
        self._flip_v.toggled.connect(self._on_flip)
        flip_row.addWidget(self._flip_h)
        flip_row.addWidget(self._flip_v)
        flip_row.addStretch()
        lay.addLayout(flip_row)

        # ── Rotation ──────────────────────────────────────────────────────────
        rot_row = QHBoxLayout()
        rot_row.setContentsMargins(0, 0, 0, 0)
        rot_lbl = QLabel("Rotation")
        rot_lbl.setObjectName("dim")
        rot_lbl.setFixedWidth(110)
        rot_lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        rot_row.addWidget(rot_lbl)
        self._rot_combo = NoScrollComboBox()
        self._rot_combo.setFixedWidth(150)
        self._rot_combo.addItems(list(ROTATIONS.keys()))
        self._rot_combo.currentTextChanged.connect(self._on_rotate)
        rot_row.addWidget(self._rot_combo)
        rot_row.addStretch()
        lay.addLayout(rot_row)

        lay.addWidget(create_separator())

        # ── Zoom ──────────────────────────────────────────────────────────────
        zoom_row = QHBoxLayout()
        zoom_row.setContentsMargins(0, 0, 0, 0)
        zoom_row.setSpacing(8)
        zoom_lbl = QLabel("Zoom")
        zoom_lbl.setObjectName("dim")
        zoom_lbl.setFixedWidth(110)
        zoom_lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        zoom_row.addWidget(zoom_lbl)
        self._zoom_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._zoom_slider.setRange(100, 500)
        self._zoom_slider.setValue(100)
        self._zoom_slider.setMinimumWidth(120)
        self._zoom_val_lbl = QLabel("1.0×")
        self._zoom_val_lbl.setObjectName("val")
        self._zoom_val_lbl.setMinimumWidth(40)
        self._zoom_slider.valueChanged.connect(self._on_zoom_changed)
        zoom_row.addWidget(self._zoom_slider, 1)
        zoom_row.addWidget(self._zoom_val_lbl)
        lay.addLayout(zoom_row)

        # ── Pan ───────────────────────────────────────────────────────────────
        pan_x_row = QHBoxLayout()
        pan_x_row.setContentsMargins(0, 0, 0, 0)
        pan_x_row.setSpacing(8)
        pan_x_lbl = QLabel("Pan X")
        pan_x_lbl.setObjectName("dim")
        pan_x_lbl.setFixedWidth(110)
        pan_x_lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        pan_x_row.addWidget(pan_x_lbl)
        self._pan_x_slider = PanSliderRow("L", "R")
        self._pan_x_slider.value_changed.connect(self._on_pan_changed)
        pan_x_row.addWidget(self._pan_x_slider, 1)
        lay.addLayout(pan_x_row)

        pan_y_row = QHBoxLayout()
        pan_y_row.setContentsMargins(0, 0, 0, 0)
        pan_y_row.setSpacing(8)
        pan_y_lbl = QLabel("Pan Y")
        pan_y_lbl.setObjectName("dim")
        pan_y_lbl.setFixedWidth(110)
        pan_y_lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        pan_y_row.addWidget(pan_y_lbl)
        self._pan_y_slider = PanSliderRow("U", "D")
        self._pan_y_slider.value_changed.connect(self._on_pan_changed)
        pan_y_row.addWidget(self._pan_y_slider, 1)
        lay.addLayout(pan_y_row)

        self._pan_x_slider.set_enabled(False)
        self._pan_y_slider.set_enabled(False)

        return card

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        frame = _apply_zoom(frame, self.zoom, self.pan_x, self.pan_y)
        return _transform_frame(frame, self.flip_h, self.flip_v, self.rotation)

    # ── Handlers (Qt thread) ──────────────────────────────────────────────────

    def _on_flip(self):
        self.flip_h = self._flip_h.isChecked()
        self.flip_v = self._flip_v.isChecked()
        self._host._schedule_save()

    def _on_rotate(self):
        self.rotation = ROTATIONS.get(self._rot_combo.currentText())
        self._host._schedule_save()

    def _on_zoom_changed(self, val: int):
        self.zoom = val / 100.0
        self._zoom_val_lbl.setText(f"{self.zoom:.1f}×")
        pan_active = self.zoom > 1.0
        self._pan_x_slider.set_enabled(pan_active)
        self._pan_y_slider.set_enabled(pan_active)
        if not pan_active:
            self._pan_x_slider.reset()
            self._pan_y_slider.reset()
            self.pan_x = 0.0
            self.pan_y = 0.0
        else:
            self.pan_x = self._pan_x_slider.get_value()
            self.pan_y = self._pan_y_slider.get_value()
        self._host._schedule_save()

    def _on_pan_changed(self, _val: float):
        self.pan_x = self._pan_x_slider.get_value()
        self.pan_y = self._pan_y_slider.get_value()
        self._host._schedule_save()

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
        self._pan_x_slider.set_value(cfg.get("pan_x", 0.0))
        self._pan_y_slider.set_value(cfg.get("pan_y", 0.0))
        self._pan_x_slider.set_enabled(pan_active)
        self._pan_y_slider.set_enabled(pan_active)
