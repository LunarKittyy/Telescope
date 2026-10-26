"""Desk and document mode: four corners of a sheet marked on the preview, straightened with a perspective warp."""
import math
from dataclasses import dataclass, replace
from typing import Optional

import cv2
import numpy as np
from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QComboBox, QHBoxLayout, QPushButton, QSizePolicy, QWidget

from telescope.plugin import TelescopePlugin
from telescope.shortcuts import SHORTCUT_VALUE_ROLE, button_action, choice_action
from telescope.view import View, to_shown
from telescope.widgets.common import (
    NoScrollComboBox, NoScrollSlider, Switch, add_card_header, card_action, card_layout, control_row as _row,
    create_card, dim_until_paired, set_status_kind, slider_row, value_label, wrapped_note,
)
from telescope.i18n import N_, tr

# Translators: what "make the writing stand out" does to the sheet: nothing, more contrast, or black and white
CLEANUPS = (("off", N_("Off")), ("contrast", N_("Contrast")), ("bw", N_("Black and white")))
# Translators: the sheet's shape: worked out from its corners, a paper size, square, or stretched over the picture
SHAPES = (("auto", N_("Auto")), ("a4", N_("A4")), ("letter", N_("Letter")), ("square", N_("Square")),
          ("fill", N_("Fill the picture")))
_SIZES = {"a4": 210 / 297, "letter": 8.5 / 11, "square": 1.0}  # width over height, standing up
STRETCH_MIN, STRETCH_MAX = 50, 200  # percent, around 100
_FOCAL = 0.71  # a phone's main camera: focal length over the picture's long side (about 70 degrees across)
_BACKDROP = (28, 28, 28)  # BGR around a sheet that keeps its shape
_PAPER = (255, 255, 255)  # ...and around a cleaned-up one
_SHADE_SMALL = 8  # the paper's shading is worked out at an eighth of the size
_INSET = 0.985  # the sheet is cut this much inside the corners as marked
_INK = 0.92  # contrast: the paper's brightness is divided out, then scaled by this so ink stays dark
_MIN_AREA = 0.01  # a sheet covering less than this share of the picture is corners bunched up, not a sheet
_MIN_ANGLE = 15.0  # degrees: a sharper corner, or one this close to flat, means corners in a line


def order_corners(points) -> np.ndarray:
    """Four (x, y) points as top left, top right, bottom right, bottom left (float32), however they were placed."""
    pts = np.asarray(points, np.float32).reshape(4, 2)
    centre = pts.mean(axis=0)
    angles = np.arctan2(pts[:, 1] - centre[1], pts[:, 0] - centre[0])
    pts = pts[np.argsort(angles)]  # clockwise on screen, since y points down
    start = int(np.argmin(pts.sum(axis=1)))
    return np.roll(pts, -start, axis=0)


def usable_corners(points) -> bool:
    """Whether four points (0..1 of the picture) outline a sheet that can be straightened: not bunched up, crossed
    or lined up, which would give a warp of garbage."""
    pts = order_corners(points).astype(np.float64)
    edges = np.roll(pts, -1, axis=0) - pts
    after = np.roll(edges, -1, axis=0)
    turns = edges[:, 0] * after[:, 1] - edges[:, 1] * after[:, 0]
    area = abs(float(np.sum(pts[:, 0] * np.roll(pts[:, 1], -1) - np.roll(pts[:, 0], -1) * pts[:, 1]))) / 2
    if area < _MIN_AREA or not (np.all(turns > 0) or np.all(turns < 0)):
        return False
    lengths = np.linalg.norm(edges, axis=1)
    for i in range(4):
        before, after = -edges[i - 1], edges[i]
        cos = float(before @ after) / max(lengths[i - 1] * lengths[i], 1e-12)
        if not _MIN_ANGLE <= math.degrees(math.acos(min(max(cos, -1.0), 1.0))) <= 180.0 - _MIN_ANGLE:
            return False
    return True


def sheet_size(corners: np.ndarray) -> tuple:
    """(width, height) of the sheet in pixels, from the average of its opposite edges."""
    tl, tr, br, bl = corners
    width = (np.linalg.norm(tr - tl) + np.linalg.norm(br - bl)) / 2
    height = (np.linalg.norm(bl - tl) + np.linalg.norm(br - tr)) / 2
    return max(float(width), 1.0), max(float(height), 1.0)


def sheet_aspect(corners: np.ndarray, centre: tuple, focal: float) -> float:
    """The real sheet's width over height from its ordered corners (px), seen by a camera with its middle at centre
    and about this focal length (px). Undoes the perspective (Zhang and He's whiteboard method), where the average
    of opposite edges takes a sheet tilted away for a much wider one."""
    tl, tr, br, bl = (np.array([x, y, 1.0]) for x, y in corners)
    u0, v0 = centre
    with np.errstate(divide="ignore", invalid="ignore"):
        k2 = np.float64(np.cross(tl, br) @ bl) / np.float64(np.cross(tr, br) @ bl)
        k3 = np.float64(np.cross(tl, br) @ tr) / np.float64(np.cross(bl, br) @ tr)
    if not (np.isfinite(k2) and np.isfinite(k3) and k2 > 0 and k3 > 0):
        sw, sh = sheet_size(corners)
        return sw / sh
    n2, n3 = k2 * tr - tl, k3 * bl - tl  # the sheet's top and left edges in 3D, up to the camera's matrix
    # Converging edges give the focal length too (a zoomed lens), trusted only near the guess as it jumps around
    if abs(n2[2] * n3[2]) > 1e-12:
        f2 = -((n2[0] * n3[0] - (n2[0] * n3[2] + n2[2] * n3[0]) * u0 + n2[2] * n3[2] * u0 * u0)
               + (n2[1] * n3[1] - (n2[1] * n3[2] + n2[2] * n3[1]) * v0 + n2[2] * n3[2] * v0 * v0)) / (n2[2] * n3[2])
        if 0.25 * focal ** 2 < f2 < 16 * focal ** 2:
            focal = math.sqrt(f2)

    def length2(n):
        return ((n[0] - u0 * n[2]) / focal) ** 2 + ((n[1] - v0 * n[2]) / focal) ** 2 + n[2] ** 2

    return min(max(math.sqrt(length2(n2) / max(length2(n3), 1e-12)), 0.1), 10.0)


def output_aspect(estimate: float, shape: str, stretch: tuple = (100, 100)) -> Optional[float]:
    """Width over height the sheet comes out at: the estimate, or a paper size turned the way the sheet lies, times
    the card's stretch (percent). None fills the whole picture."""
    if shape == "fill":
        return None
    size = _SIZES.get(shape)
    aspect = estimate if size is None else (size if estimate < 1 else 1 / size)
    return aspect * stretch[0] / stretch[1]


def target_rect(aspect: Optional[float], w: int, h: int) -> tuple:
    """(x, y, width, height) the sheet takes in a w x h frame: as big as fits at aspect and centred, or all of it."""
    if aspect is None:
        return 0, 0, w, h
    rw, rh = (w, w / aspect) if w / aspect <= h else (h * aspect, h)
    rw, rh = max(2, min(w, round(rw))), max(2, min(h, round(rh)))
    return (w - rw) // 2, (h - rh) // 2, rw, rh


def whiten(sheet: np.ndarray) -> np.ndarray:
    """The paper's uneven light divided out, so it goes white and the ink stays dark."""
    h, w = sheet.shape[:2]
    small = cv2.resize(sheet, (max(1, w // _SHADE_SMALL), max(1, h // _SHADE_SMALL)), interpolation=cv2.INTER_AREA)
    paper = cv2.dilate(small, cv2.getStructuringElement(cv2.MORPH_ELLIPSE, (5, 5)))  # ink is thin: the max is paper
    paper = cv2.GaussianBlur(paper, (0, 0), 2)
    paper = cv2.resize(paper, (w, h), interpolation=cv2.INTER_LINEAR)
    return cv2.divide(sheet, paper, scale=255 * _INK)


def black_and_white(sheet: np.ndarray) -> np.ndarray:
    """Ink black, paper white: an adaptive threshold that follows the light across the sheet."""
    grey = cv2.cvtColor(sheet, cv2.COLOR_BGR2GRAY)
    block = max(15, (min(grey.shape) // 24) | 1)
    bw = cv2.adaptiveThreshold(grey, 255, cv2.ADAPTIVE_THRESH_MEAN_C, cv2.THRESH_BINARY, block, 12)
    return cv2.cvtColor(bw, cv2.COLOR_GRAY2BGR)


CLEAN = {"off": None, "contrast": whiten, "bw": black_and_white}


@dataclass(frozen=True)
class Settings:
    """What a frame step needs, swapped whole (the stream thread reads it)."""
    on: bool = False
    corners: Optional[tuple] = None  # four (x, y) in the whole picture (telescope.overlays.Pick.full)
    cleanup: str = "off"
    shape: str = "auto"  # one of SHAPES
    stretch: tuple = (100, 100)  # width and height, percent
    view: View = View()


class Warp:
    """The straightening for one frame size, worked out once and reused while nothing changes."""

    def __init__(self):
        self._key = None
        self._made = None
        self.on_preview = None  # the same warp from the frame as shown to what comes out, both 0..1, for the preview

    def get(self, settings: Settings, w: int, h: int) -> Optional[tuple]:
        """(matrix, (x, y, width, height)) for a w x h frame, or None while there's nothing to straighten."""
        if not settings.on or settings.corners is None:
            self.on_preview = None
            return None
        key = (settings.corners, settings.shape, settings.stretch, settings.view, w, h)
        if key != self._key:
            shown = [to_shown(settings.view, x, y) for x, y in settings.corners]
            corners = order_corners([(u * w, v * h) for u, v in shown])
            middle = to_shown(settings.view, 0.5, 0.5)  # the lens's middle, wherever zoom and pan moved it
            focal = _FOCAL * max(w, h) * max(settings.view.zoom, 1.0)
            estimate = sheet_aspect(corners, (middle[0] * w, middle[1] * h), focal)
            x, y, rw, rh = target_rect(output_aspect(estimate, settings.shape, settings.stretch), w, h)
            centre = corners.mean(axis=0)
            corners = centre + (corners - centre) * _INSET  # a corner a pixel off would bring the desk's edge along
            dest = np.float32([[0, 0], [rw, 0], [rw, rh], [0, rh]])
            matrix = cv2.getPerspectiveTransform(corners, dest)
            self._key, self._made = key, (matrix, (x, y, rw, rh))
            moved = np.array([[1, 0, x], [0, 1, y], [0, 0, 1]], np.float64) @ matrix
            self.on_preview = np.diag([1 / w, 1 / h, 1.0]) @ moved @ np.diag([w, h, 1.0])
        return self._made


def straighten(frame: np.ndarray, settings: Settings, warp: Warp) -> np.ndarray:
    h, w = frame.shape[:2]
    made = warp.get(settings, w, h)
    if made is None:
        return frame
    matrix, (x, y, rw, rh) = made
    sheet = cv2.warpPerspective(frame, matrix, (rw, rh), flags=cv2.INTER_LINEAR, borderMode=cv2.BORDER_REPLICATE)
    clean = CLEAN.get(settings.cleanup)
    if clean is not None:
        sheet = clean(sheet)
    if (rw, rh) == (w, h):
        return sheet
    fill = _PAPER if clean is not None else _BACKDROP
    return cv2.copyMakeBorder(sheet, y, h - rh - y, x, w - rw - x, cv2.BORDER_CONSTANT, value=fill)


class DeskPlugin(TelescopePlugin):
    name = "desk"
    title = N_("Desk and document mode")
    about = N_("Straightens a sheet of paper you mark on the preview and can make the writing on it stand out.")
    category = "camera"
    icon = "pencil"
    when_off = N_("Without it there's no Desk mode card and the picture stays the way the camera sees it.")
    optional = True
    in_presets = True
    device_local = True  # where the sheet sits depends on where that phone is
    panel_region = "left"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._settings = Settings(view=host.view())
        self._warp = Warp()
        self._picking = False  # the warp waits while the corners are being placed, so they go on the real picture
        self._told = None  # the warp the preview was last told about
        self._refused = False  # the last corners marked didn't make a sheet, so the card says so
        bus.view_changed.connect(self._on_view)
        bus.preview_pick_changed.connect(self._on_pick_changed)

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._switch = Switch("")
        self._switch.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        self._switch.setToolTip(tr("Straighten the sheet you marked"))
        self._switch.toggled.connect(self._on_switch)
        add_card_header(lay, tr("Desk mode"), "crop", action=self._switch)
        dim_until_paired(card, self._bus)

        self._mark_btn = QPushButton(tr("Mark the sheet"))
        self._mark_btn.setToolTip(tr("Drag the four corners of the sheet into place on the preview"))
        self._mark_btn.clicked.connect(self._mark)
        self._done_btn = QPushButton(tr("Done"))
        self._done_btn.clicked.connect(lambda: self._host.finish_pick(self.name))
        self._cancel_btn = QPushButton(tr("Cancel"))
        self._cancel_btn.clicked.connect(lambda: self._host.cancel_pick(self.name))
        buttons = QHBoxLayout()
        buttons.setSpacing(8)
        for btn in (self._mark_btn, self._done_btn, self._cancel_btn):
            buttons.addWidget(btn)
        buttons.addStretch(1)
        self._reset_btn = card_action(tr("Reset"), "reset", tr("Put the width and height back to 100%"))
        self._reset_btn.clicked.connect(self._reset_stretch)
        buttons.addWidget(self._reset_btn)
        lay.addLayout(buttons)

        self._clean_combo = self._combo(CLEANUPS, tr("Make the writing on the sheet stand out"))
        self._clean_combo.currentIndexChanged.connect(lambda _i: self._set_cleanup(self._clean_combo.currentData()))
        lay.addLayout(_row(tr("Writing"), self._clean_combo, stretch=True))
        self._shape_combo = self._combo(SHAPES, tr("Auto works the shape out from the corners"))
        self._shape_combo.currentIndexChanged.connect(lambda _i: self._set_shape(self._shape_combo.currentData()))
        lay.addLayout(_row(tr("Shape"), self._shape_combo, stretch=True))

        self._stretch_sliders, self._stretch_lbls = [], []
        for label, tip in ((tr("Width"), tr("Stretch the sheet sideways if it still looks squashed")),
                           (tr("Height"), tr("Stretch the sheet up and down if it still looks squashed"))):
            slider = NoScrollSlider(Qt.Orientation.Horizontal)
            slider.setRange(STRETCH_MIN, STRETCH_MAX)
            slider.setPageStep(10)
            slider.setValue(100)
            slider.set_default(100)
            slider.set_snaps([100])
            slider.setToolTip(tip)
            readout = value_label("100%")
            slider.valueChanged.connect(self._on_stretch)
            lay.addLayout(_row(label, slider_row(slider, readout), stretch=True))
            self._stretch_sliders.append(slider)
            self._stretch_lbls.append(readout)

        self._status = wrapped_note("")
        lay.addWidget(self._status)
        self._show()
        return card

    @staticmethod
    def _combo(choices, tip: str) -> NoScrollComboBox:
        combo = NoScrollComboBox()
        combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        combo.setMinimumContentsLength(8)  # a long name can't widen the card; the list opens wide enough
        combo.setToolTip(tip)
        for key, text in choices:
            combo.addItem(tr(text), key)
            combo.setItemData(combo.count() - 1, text, SHORTCUT_VALUE_ROLE)
        return combo

    def create_actions(self) -> list:
        g = tr("Desk and document mode")
        return [
            button_action("desk.on", tr("Desk mode"), g, self._switch),
            button_action("desk.mark", tr("Mark the sheet"), g, self._mark_btn),
            choice_action("desk.writing", tr("Make the writing stand out"), g, combo=self._clean_combo),
            choice_action("desk.shape", tr("Sheet shape"), g, combo=self._shape_combo),
        ]

    # ── Handlers (Qt thread) ──────────────────────────────────────────────

    def _apply(self, **changes):
        self._settings = replace(self._settings, **changes)
        self._show()

    def _on_view(self, view: View):
        self._apply(view=view)

    def _on_switch(self, on: bool):
        if on and self._settings.corners is None:
            self._apply(on=True)
            self._mark()
        else:
            self._apply(on=on)
        self._host.schedule_save()

    def _set_cleanup(self, key: str):
        self._apply(cleanup=key)
        self._host.schedule_save()

    def _set_shape(self, key: str):
        self._apply(shape=key)
        self._host.schedule_save()

    def _on_stretch(self, _value: int):
        self._apply(stretch=tuple(s.value() for s in self._stretch_sliders))
        self._host.schedule_save()

    def _reset_stretch(self):
        for slider in self._stretch_sliders:
            slider.setValue(100)

    def _mark(self):
        start = self._settings.corners
        if not self._host.pick_on_preview(self.name, "quad", self._picked, start=start,
                                          hint=tr("Drag the corners onto the sheet's corners, then press Enter.")):
            self._status.setText(tr("Start streaming to mark the sheet on the preview."))
            self._status.setVisible(True)
            set_status_kind(self._status, "warn")

    def _picked(self, pick):
        if pick is None:
            self._show()
            return
        if not usable_corners(pick.full):
            self._refused = True
            self._show()
            return
        self._refused = False
        self._apply(corners=tuple(pick.full), on=True)
        self._switch.blockSignals(True)
        self._switch.setChecked(True)
        self._switch.blockSignals(False)
        self._host.schedule_save()

    def _on_pick_changed(self, owner: str):
        self._picking = owner == self.name
        if self._picking:
            self._refused = False
        self._show()

    def _show(self):
        if not hasattr(self, "_mark_btn"):
            return
        self._mark_btn.setVisible(not self._picking)
        self._done_btn.setVisible(self._picking)
        self._cancel_btn.setVisible(self._picking)
        self._mark_btn.setText(tr("Mark the sheet again") if self._settings.corners else tr("Mark the sheet"))
        fill = self._settings.shape == "fill"
        for slider, readout, value in zip(self._stretch_sliders, self._stretch_lbls, self._settings.stretch):
            slider.setEnabled(not fill)  # filling the picture leaves nothing to stretch
            readout.setText(f"{value}%")
        self._reset_btn.setVisible(not fill and self._settings.stretch != (100, 100))
        if self._picking:
            text, kind = tr("Drag the corners on the preview onto the sheet's corners."), ""
        elif self._refused:
            text, kind = tr("Those corners were bunched up or in a line and weren't used. Mark the sheet again with "
                            "one on each of its corners."), "warn"
        elif self._settings.on and self._settings.corners is None:
            text, kind = tr("Mark the sheet's corners to straighten it."), "warn"
        elif self._settings.on and self._off_picture():
            text, kind = tr("Part of the sheet is outside the picture. Zoom out or mark it again."), "warn"
        else:
            text, kind = "", ""
        self._status.setText(text)
        self._status.setVisible(bool(text))
        set_status_kind(self._status, kind)

    def _off_picture(self) -> bool:
        """Whether a corner sits outside the frame as shown (zoomed in past it, or Auto framing moved the view)."""
        slack = 0.01
        return any(not (-slack <= c <= 1 + slack) for x, y in self._settings.corners
                   for c in to_shown(self._settings.view, x, y))

    # ── Frames (worker thread) ────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        if self._picking:
            self._tell_preview(None)
            return frame
        out = straighten(frame, self._settings, self._warp)
        self._tell_preview(self._warp.on_preview)
        return out

    def _tell_preview(self, matrix):
        """Clicks and picks on the preview get mapped back through the warp (focus lands on the word you tapped)."""
        if matrix is not self._told:
            self._told = matrix
            self._host.set_preview_warp(self.name, matrix)

    def frame_step(self):
        settings = self._settings
        if not settings.on or settings.corners is None:
            return None
        warp = Warp()
        return lambda frame: straighten(frame, settings, warp)

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        s = self._settings
        return {"on": s.on, "corners": [list(p) for p in s.corners] if s.corners else None, "cleanup": s.cleanup,
                "shape": s.shape, "stretch": list(s.stretch)}

    def set_config(self, cfg: dict):
        corners = cfg.get("corners")
        valid = (isinstance(corners, list) and len(corners) == 4 and all(
            isinstance(p, list) and len(p) == 2 and all(isinstance(c, (int, float)) and not isinstance(c, bool)
                                                        and math.isfinite(c) for c in p) for p in corners))
        corners = tuple((float(x), float(y)) for x, y in corners) if valid and usable_corners(corners) else None
        cleanup = cfg.get("cleanup") if cfg.get("cleanup") in dict(CLEANUPS) else "off"
        shape = cfg.get("shape", "fill" if cfg.get("keep_shape") is False else "auto")  # keep_shape: early builds
        shape = shape if shape in dict(SHAPES) else "auto"
        stretch = cfg.get("stretch")
        stretch = tuple(min(max(round(v), STRETCH_MIN), STRETCH_MAX) for v in stretch) if (
            isinstance(stretch, list) and len(stretch) == 2 and all(
                isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v) for v in stretch)
        ) else (100, 100)
        on = cfg.get("on") is True and corners is not None
        self._apply(on=on, corners=corners, cleanup=cleanup, shape=shape, stretch=stretch)
        widgets = [self._switch, self._clean_combo, self._shape_combo, *self._stretch_sliders]
        for widget in widgets:
            widget.blockSignals(True)
        self._switch.setChecked(on)
        self._clean_combo.setCurrentIndex(max(0, self._clean_combo.findData(cleanup)))
        self._shape_combo.setCurrentIndex(max(0, self._shape_combo.findData(shape)))
        for slider, value in zip(self._stretch_sliders, stretch):
            slider.setValue(value)
        for widget in widgets:
            widget.blockSignals(False)

    def diagnostics(self) -> dict:
        s = self._settings
        state = "on" if s.on else "off"
        return {"Desk mode": f"{state}, corners {'marked' if s.corners else 'not marked'}, writing {s.cleanup}, "
                             f"shape {s.shape}, stretch {s.stretch[0]}% x {s.stretch[1]}%"}

    def shutdown(self):
        self._host.cancel_pick(self.name)
        self._host.set_preview_warp(self.name, None)
