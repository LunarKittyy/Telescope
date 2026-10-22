from pathlib import Path
from typing import Optional

import cv2
import numpy as np

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QButtonGroup, QFileDialog, QHBoxLayout, QPushButton, QWidget

from telescope.plugin import TelescopePlugin
from telescope.shortcuts import choice_action, slider_action
from telescope.widgets.vision_status import VisionStatus
from telescope.widgets.common import (
    ElidingLabel, NoScrollSlider, SegmentButton, add_card_header, card_layout, control_row as _row, control_row_widget,
    create_card, dim_until_paired, segmented_row, slider_row, value_label,
)

MODES = (("off", "Off"), ("blur", "Blur"), ("picture", "Picture"))
_PICTURES = "Pictures (*.png *.jpg *.jpeg *.webp *.bmp)"
_SMALL = 4  # edges are worked out at a quarter of the size, then scaled back up
_BLUR_SMALL = 8  # and the blurred background at an eighth, since it's that blurry anyway


def _clamp(value, default: int, lo: int, hi: int) -> int:
    try:
        return min(hi, max(lo, int(value)))
    except (TypeError, ValueError):
        return default


def read_picture(path: str) -> Optional[np.ndarray]:
    """A picture file as BGR, or None; read as bytes since cv2.imread fails on non-ASCII paths on Windows."""
    try:
        data = np.fromfile(path, np.uint8)
    except (OSError, ValueError):
        return None
    image = cv2.imdecode(data, cv2.IMREAD_COLOR) if data.size else None
    return image if image is not None and image.size else None


def cover(image: np.ndarray, w: int, h: int) -> np.ndarray:
    """image scaled to fill w x h, cut evenly at the edges that don't fit."""
    ih, iw = image.shape[:2]
    scale = max(w / iw, h / ih)
    sw, sh = max(w, round(iw * scale)), max(h, round(ih * scale))
    scaled = cv2.resize(image, (sw, sh), interpolation=cv2.INTER_AREA if scale < 1 else cv2.INTER_LINEAR)
    x0, y0 = (sw - w) // 2, (sh - h) // 2
    return np.ascontiguousarray(scaled[y0:y0 + h, x0:x0 + w])


def blurred(frame: np.ndarray, strength: int, mask: Optional[np.ndarray] = None) -> np.ndarray:
    """The frame blurred (strength 1..10), at a quarter of the size and scaled back up. With the person mask, the
    person is left out of the blur, so they don't smear into a dark halo around themselves."""
    h, w = frame.shape[:2]
    size = (max(1, w // _BLUR_SMALL), max(1, h // _BLUR_SMALL))
    small = cv2.resize(frame, size, interpolation=cv2.INTER_AREA)
    sigma = (1.0 + strength * 1.2) * _SMALL / _BLUR_SMALL
    if mask is None:
        return cv2.resize(cv2.GaussianBlur(small, (0, 0), sigma), (w, h), interpolation=cv2.INTER_LINEAR)
    rest = 1.0 - cv2.resize(mask, size, interpolation=cv2.INTER_AREA).astype(np.float32) / 255.0
    spread = cv2.GaussianBlur(rest, (0, 0), sigma)
    total = cv2.GaussianBlur(small.astype(np.float32) * rest[..., None], (0, 0), sigma)
    plain = cv2.GaussianBlur(small, (0, 0), sigma).astype(np.float32)
    weight = np.clip(spread * 4.0, 0.0, 1.0)[..., None]  # where there's next to no background, the plain blur
    out = total / np.maximum(spread, 1e-3)[..., None] * weight + plain * (1.0 - weight)
    return cv2.resize(out.clip(0, 255).astype(np.uint8), (w, h), interpolation=cv2.INTER_LINEAR)


def soften(mask: np.ndarray, edges: int) -> np.ndarray:
    """The person mask with its edge feathered (edges 0..10; 0 leaves it as the model drew it)."""
    if edges <= 0:
        return mask
    h, w = mask.shape[:2]
    small = cv2.resize(mask, (max(1, w // _SMALL), max(1, h // _SMALL)), interpolation=cv2.INTER_AREA)
    small = cv2.GaussianBlur(small, (0, 0), edges * 0.6)
    return cv2.resize(small, (w, h), interpolation=cv2.INTER_LINEAR)


def compose(frame: np.ndarray, mask: np.ndarray, background: np.ndarray) -> np.ndarray:
    """The person (mask 255) from frame over background, all uint8: two multiplies and an add."""
    weight = cv2.merge((mask, mask, mask))
    person = cv2.multiply(frame, weight, scale=1 / 255)
    rest = cv2.multiply(background, cv2.bitwise_not(weight), scale=1 / 255)
    return cv2.add(person, rest)


class _Settings:
    """What a frame step needs, frozen (a stream behind the panels keeps the ones it was left with)."""

    def __init__(self, mode="off", blur=6, edges=3, picture=None):
        self.mode, self.blur, self.edges, self.picture = mode, blur, edges, picture
        self._covered = {}  # (w, h): the picture scaled to cover that frame size

    def background(self, frame: np.ndarray, mask: np.ndarray) -> Optional[np.ndarray]:
        if self.mode == "blur":
            return blurred(frame, self.blur, mask)
        if self.mode == "picture" and self.picture is not None:
            h, w = frame.shape[:2]
            if (w, h) not in self._covered:
                self._covered = {(w, h): cover(self.picture, w, h)}
            return self._covered[(w, h)]
        return None


class BackgroundPlugin(TelescopePlugin):
    name = "background"
    title = "Background"
    about = "Blurs the background behind you or swaps it for a picture."
    when_off = "Without it there's no Background card and the background stays as the camera sees it."
    optional = True
    needs_vision = True
    panel_region = "right"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._settings = _Settings()  # Written by Qt thread; read by worker thread (swapped whole, GIL-atomic).
        self._picture_path = ""
        self._picture: Optional[np.ndarray] = None
        self._picture_problem = ""

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, "Background", "image", hue="vision")
        dim_until_paired(card, self._bus)

        self._mode_btns = {}
        group = QButtonGroup(card)
        for key, text in MODES:
            btn = SegmentButton(text)
            btn.setProperty("shortcutValue", key)
            btn.clicked.connect(lambda _c=False, k=key: self._set_mode(k))
            group.addButton(btn)
            self._mode_btns[key] = btn
        self._mode_btns["off"].setChecked(True)
        lay.addLayout(_row("Background", segmented_row(*self._mode_btns.values()), stretch=True))

        self._blur_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._blur_slider.setRange(1, 10)
        self._blur_slider.setValue(6)
        self._blur_slider.set_default(6)
        self._blur_lbl = value_label("6")
        self._blur_slider.valueChanged.connect(self._on_blur)
        self._blur_row = control_row_widget("Blur", slider_row(self._blur_slider, self._blur_lbl), stretch=True)
        lay.addWidget(self._blur_row)

        self._pick_btn = QPushButton("Choose…")
        self._pick_btn.setToolTip("Pick the picture that goes behind you")
        self._pick_btn.clicked.connect(self._choose_picture)
        self._picture_lbl = ElidingLabel("No picture yet", mode=Qt.TextElideMode.ElideMiddle)  # a long name can't widen the card
        self._picture_lbl.setObjectName("setting_sub")
        pick = QHBoxLayout()
        pick.setSpacing(8)
        pick.addWidget(self._pick_btn)
        pick.addWidget(self._picture_lbl, 1)
        self._picture_row = control_row_widget("Picture", pick, stretch=True)
        lay.addWidget(self._picture_row)

        self._edges_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._edges_slider.setRange(0, 10)
        self._edges_slider.setValue(3)
        self._edges_slider.set_default(3)
        self._edges_slider.setToolTip("How soft the edge around you is")
        self._edges_lbl = value_label("3")
        self._edges_slider.valueChanged.connect(self._on_edges)
        self._edges_row = control_row_widget("Edges", slider_row(self._edges_slider, self._edges_lbl), stretch=True)
        lay.addWidget(self._edges_row)

        self._status = VisionStatus(self._host, self._bus)
        lay.addWidget(self._status)

        self._show_mode()
        return card

    def create_actions(self) -> list:
        g = "Background"
        return [
            choice_action("background.mode", "Background", g, buttons=list(self._mode_btns.values())),
            slider_action("background.blur", "Background blur", g, self._blur_slider, step=1.0,
                          readout=self._blur_lbl),
        ]

    # ── Handlers (Qt thread) ──────────────────────────────────────────────

    def _set_mode(self, mode: str):
        self._mode_btns[mode].setChecked(True)
        self._apply()
        self._show_mode()
        self._host.schedule_save()

    def _on_blur(self, value: int):
        self._blur_lbl.setText(str(value))
        self._apply()
        self._host.schedule_save()

    def _on_edges(self, value: int):
        self._edges_lbl.setText(str(value))
        self._apply()
        self._host.schedule_save()

    def _choose_picture(self):
        start = str(Path(self._picture_path).parent) if self._picture_path else str(Path.home())
        path, _filter = QFileDialog.getOpenFileName(self._pick_btn.window(), "Choose a picture", start, _PICTURES)
        if path:
            self._load_picture(path)
            if not self._picture_problem:
                self._set_mode("picture")
            self._host.schedule_save()

    def _load_picture(self, path: str):
        self._picture_path = path
        self._picture = read_picture(path) if path else None
        self._picture_problem = "Telescope can't open that picture." if path and self._picture is None else ""
        self._picture_lbl.setText(Path(path).name if path else "No picture yet")
        self._picture_lbl.setToolTip(path)
        self._apply()
        self._show_status()

    def _mode(self) -> str:
        return next((k for k, btn in self._mode_btns.items() if btn.isChecked()), "off")

    def _apply(self):
        self._settings = _Settings(self._mode(), self._blur_slider.value(), self._edges_slider.value(),
                                   self._picture)

    def _show_mode(self):
        mode = self._mode()
        self._blur_row.setVisible(mode == "blur")
        self._picture_row.setVisible(mode == "picture")
        self._edges_row.setVisible(mode != "off")
        self._show_status()

    def _show_status(self):
        if not hasattr(self, "_status"):
            return
        mode, problem = self._mode(), ""
        if mode == "picture" and self._picture_problem:
            problem = self._picture_problem
        elif mode == "picture" and self._picture is None:
            problem = "Choose a picture to put behind you."
        self._status.refresh(mode != "off", problem)

    # ── Frames (worker thread) ────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        return self._step(frame, self._settings)

    def frame_step(self):
        settings = self._settings
        if settings.mode == "off":
            return None
        return lambda frame: self._step(frame, settings)

    def _step(self, frame: np.ndarray, settings: _Settings) -> np.ndarray:
        if settings.mode == "off":
            return frame
        mask = self._host.person_mask(frame)
        if mask is None:
            return frame
        background = settings.background(frame, mask)
        if background is None:
            return frame
        return compose(frame, soften(mask, settings.edges), background)

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"mode": self._mode(), "blur": self._blur_slider.value(), "edges": self._edges_slider.value(),
                "picture": self._picture_path}

    def set_config(self, cfg: dict):
        mode = cfg.get("mode") if cfg.get("mode") in dict(MODES) else "off"
        self._blur_slider.setValue(_clamp(cfg.get("blur"), 6, 1, 10))
        self._edges_slider.setValue(_clamp(cfg.get("edges"), 3, 0, 10))
        picture = cfg.get("picture")
        self._load_picture(picture if isinstance(picture, str) else "")
        self._mode_btns[mode].setChecked(True)
        self._apply()
        self._show_mode()

    def diagnostics(self) -> dict:
        return {"Background": self._mode()}
