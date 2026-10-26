"""Desk and document mode: four corners of a sheet marked on the preview, straightened with a perspective warp."""
import math
from dataclasses import dataclass
from typing import Optional

import cv2
import numpy as np
from PyQt6.QtWidgets import QComboBox, QHBoxLayout, QPushButton, QSizePolicy, QVBoxLayout, QWidget

from telescope.plugin import SettingsSection, TelescopePlugin
from telescope.shortcuts import SHORTCUT_VALUE_ROLE, button_action, choice_action
from telescope.view import View, to_shown
from telescope.widgets.settings_search import keywords
from telescope.widgets.common import (
    NoScrollComboBox, Switch, add_card_header, card_layout, control_row as _row, create_card, dim_until_paired,
    set_status_kind, wrapped_note,
)
from telescope.i18n import N_, tr

# Translators: what "make the writing stand out" does to the sheet: nothing, more contrast, or black and white
CLEANUPS = (("off", N_("Off")), ("contrast", N_("Contrast")), ("bw", N_("Black and white")))
_BACKDROP = (28, 28, 28)  # BGR around a sheet that keeps its shape
_PAPER = (255, 255, 255)  # ...and around a cleaned-up one
_SHADE_SMALL = 8  # the paper's shading is worked out at an eighth of the size
_INSET = 0.985  # the sheet is cut this much inside the corners as marked
_INK = 0.92  # contrast: the paper's brightness is divided out, then scaled by this so ink stays dark


def order_corners(points) -> np.ndarray:
    """Four (x, y) points as top left, top right, bottom right, bottom left (float32), however they were placed."""
    pts = np.asarray(points, np.float32).reshape(4, 2)
    centre = pts.mean(axis=0)
    angles = np.arctan2(pts[:, 1] - centre[1], pts[:, 0] - centre[0])
    pts = pts[np.argsort(angles)]  # clockwise on screen, since y points down
    start = int(np.argmin(pts.sum(axis=1)))
    return np.roll(pts, -start, axis=0)


def sheet_size(corners: np.ndarray) -> tuple:
    """(width, height) of the sheet in pixels, from the average of its opposite edges."""
    tl, tr, br, bl = corners
    width = (np.linalg.norm(tr - tl) + np.linalg.norm(br - bl)) / 2
    height = (np.linalg.norm(bl - tl) + np.linalg.norm(br - tr)) / 2
    return max(float(width), 1.0), max(float(height), 1.0)


def target_rect(corners: np.ndarray, w: int, h: int, keep_shape: bool) -> tuple:
    """(x, y, width, height) the sheet fills in a w x h frame: as big as fits in its own shape, or the whole frame."""
    if not keep_shape:
        return 0, 0, w, h
    sw, sh = sheet_size(corners)
    scale = min(w / sw, h / sh)
    rw, rh = max(2, round(sw * scale)), max(2, round(sh * scale))
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
    keep_shape: bool = True
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
        key = (settings.corners, settings.keep_shape, settings.view, w, h)
        if key != self._key:
            shown = [to_shown(settings.view, x, y) for x, y in settings.corners]
            corners = order_corners([(u * w, v * h) for u, v in shown])
            centre = corners.mean(axis=0)
            corners = centre + (corners - centre) * _INSET  # a corner a pixel off would bring the desk's edge along
            x, y, rw, rh = target_rect(corners, w, h, settings.keep_shape)
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
        lay.addLayout(buttons)

        self._clean_combo = NoScrollComboBox()
        self._clean_combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        self._clean_combo.setMinimumContentsLength(8)  # a long name can't widen the card; the list opens wide enough
        self._clean_combo.setToolTip(tr("Make the writing on the sheet stand out"))
        for key, text in CLEANUPS:
            self._clean_combo.addItem(tr(text), key)
            self._clean_combo.setItemData(self._clean_combo.count() - 1, text, SHORTCUT_VALUE_ROLE)
        self._clean_combo.currentIndexChanged.connect(lambda _i: self._set_cleanup(self._clean_combo.currentData()))
        lay.addLayout(_row(tr("Writing"), self._clean_combo, stretch=True))

        self._status = wrapped_note("")
        lay.addWidget(self._status)
        self._show()
        return card

    def create_settings_sections(self) -> list:
        section = QWidget()
        outer = QVBoxLayout(section)
        outer.setContentsMargins(0, 0, 0, 0)
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Desk and document mode"), "crop")
        keywords(card, tr("document paper sheet whiteboard homework perspective straighten keystone scan",
                          ctx="search"))
        self._shape_switch = Switch(tr("Keep the sheet's shape"))
        self._shape_switch.setToolTip(tr("Off stretches the sheet to fill the whole picture"))
        self._shape_switch.setChecked(self._settings.keep_shape)
        self._shape_switch.toggled.connect(self._on_shape)
        lay.addWidget(self._shape_switch)
        outer.addWidget(card)
        return [SettingsSection("video", section, order=45,
                                on_show=lambda: self._shape_switch.setChecked(self._settings.keep_shape))]

    def create_actions(self) -> list:
        g = tr("Desk and document mode")
        return [
            button_action("desk.on", tr("Desk mode"), g, self._switch),
            button_action("desk.mark", tr("Mark the sheet"), g, self._mark_btn),
            choice_action("desk.writing", tr("Make the writing stand out"), g, combo=self._clean_combo),
        ]

    # ── Handlers (Qt thread) ──────────────────────────────────────────────

    def _apply(self, **changes):
        fields = {f: getattr(self._settings, f) for f in ("on", "corners", "cleanup", "keep_shape", "view")}
        fields.update(changes)
        self._settings = Settings(**fields)
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

    def _on_shape(self, keep: bool):
        self._apply(keep_shape=keep)
        self._host.schedule_save()

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
        self._apply(corners=tuple(pick.full), on=True)
        self._switch.blockSignals(True)
        self._switch.setChecked(True)
        self._switch.blockSignals(False)
        self._host.schedule_save()

    def _on_pick_changed(self, owner: str):
        self._picking = owner == self.name
        self._show()

    def _show(self):
        if not hasattr(self, "_mark_btn"):
            return
        self._mark_btn.setVisible(not self._picking)
        self._done_btn.setVisible(self._picking)
        self._cancel_btn.setVisible(self._picking)
        self._mark_btn.setText(tr("Mark the sheet again") if self._settings.corners else tr("Mark the sheet"))
        if self._picking:
            text, kind = tr("Drag the corners on the preview onto the sheet's corners."), ""
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
                "keep_shape": s.keep_shape}

    def set_config(self, cfg: dict):
        corners = cfg.get("corners")
        valid = (isinstance(corners, list) and len(corners) == 4 and all(
            isinstance(p, list) and len(p) == 2 and all(isinstance(c, (int, float)) and not isinstance(c, bool)
                                                        and math.isfinite(c) for c in p) for p in corners))
        corners = tuple((float(x), float(y)) for x, y in corners) if valid else None
        cleanup = cfg.get("cleanup") if cfg.get("cleanup") in dict(CLEANUPS) else "off"
        keep = cfg.get("keep_shape") is not False
        on = cfg.get("on") is True and corners is not None
        self._apply(on=on, corners=corners, cleanup=cleanup, keep_shape=keep)
        self._switch.blockSignals(True)
        self._switch.setChecked(on)
        self._switch.blockSignals(False)
        self._clean_combo.blockSignals(True)
        self._clean_combo.setCurrentIndex(max(0, self._clean_combo.findData(cleanup)))
        self._clean_combo.blockSignals(False)
        if hasattr(self, "_shape_switch"):
            self._shape_switch.blockSignals(True)
            self._shape_switch.setChecked(keep)
            self._shape_switch.blockSignals(False)

    def diagnostics(self) -> dict:
        s = self._settings
        state = "on" if s.on else "off"
        return {"Desk mode": f"{state}, corners {'marked' if s.corners else 'not marked'}, writing {s.cleanup}"}

    def shutdown(self):
        self._host.cancel_pick(self.name)
        self._host.set_preview_warp(self.name, None)
