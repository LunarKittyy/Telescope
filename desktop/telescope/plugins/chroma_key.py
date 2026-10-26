"""Chroma key: a green or blue screen (or a colour picked on the preview) swapped for a colour or a picture."""
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

import cv2
import numpy as np
from PyQt6.QtCore import Qt
from PyQt6.QtGui import QColor, QIcon, QPixmap
from PyQt6.QtWidgets import QButtonGroup, QColorDialog, QFileDialog, QHBoxLayout, QPushButton, QSizePolicy, QWidget

from telescope.pictures import cover, read_picture
from telescope.plugin import TelescopePlugin
from telescope.shortcuts import button_action, choice_action, slider_action
from telescope.widgets.common import (
    ElidingLabel, NoScrollSlider, SegmentButton, Switch, add_card_header, card_layout, control_row as _row,
    control_row_widget, create_card, dim_until_paired, segmented_row, set_status_kind, slider_row, value_label,
    wrapped_note,
)
from telescope.i18n import N_, tr

SCREENS = (("green", N_("Green"), (60, 180, 50)), ("blue", N_("Blue"), (190, 80, 20)),
           ("picked", N_("Picked"), None))
BACKGROUNDS = (("colour", N_("Colour")), ("picture", N_("Picture")))
_PICTURES = N_("Pictures (*.png *.jpg *.jpeg *.webp *.bmp)")
_DEFAULT_COLOUR = (40, 34, 30)  # BGR, a dark neutral behind you
_SPILL_SPREAD = 0.5  # spill comes off colours up to about 60 degrees of hue from the screen's, fading out towards there
_SAMPLE = 4  # px: a picked colour is the median of a square this far around the click, at the stream's size
_MIN_CHROMA = 12.0  # a picked colour with less colour than this (out of about 128) can't be keyed: grey, white, black


def _clamp(value, default: int, lo: int, hi: int) -> int:
    try:
        return min(hi, max(lo, int(value)))
    except (TypeError, ValueError):
        return default


def chroma_of(bgr) -> tuple:
    """(Cb, Cr) of a BGR colour, centred on 0."""
    ycrcb = cv2.cvtColor(np.array([[bgr]], np.uint8), cv2.COLOR_BGR2YCrCb)[0, 0].astype(np.float32)
    return float(ycrcb[2]) - 128.0, float(ycrcb[1]) - 128.0


def keyable(bgr) -> bool:
    """Whether a colour has enough colour in it to key on."""
    return float(np.hypot(*chroma_of(bgr))) >= _MIN_CHROMA


def key_tables(key_bgr, tolerance: int, softness: int, spill: int) -> tuple:
    """(alpha, Cr, Cb) lookup tables, each 65536 long and indexed by Cr << 8 | Cb: how much of a pixel with that colour
    is kept (255 for all of it), and its colour once the screen's spill is taken out. tolerance, softness and spill
    are 0..100."""
    kcb, kcr = chroma_of(key_bgr)
    reach = max(float(np.hypot(kcb, kcr)), _MIN_CHROMA)
    axis = np.arange(256, dtype=np.float32) - 128.0
    cr, cb = np.meshgrid(axis, axis, indexing="ij")
    distance = np.hypot(cb - kcb, cr - kcr)
    inner = reach * (0.1 + tolerance / 100 * 0.8)  # everything this close to the screen's colour goes
    width = reach * (0.02 + softness / 100 * 0.6)  # and from there on it fades back in
    alpha = np.clip((distance - inner) / width, 0.0, 1.0)
    ux, uy = kcb / reach, kcr / reach
    along = cb * ux + cr * uy
    length = np.hypot(cb, cr)
    cos = np.where(length > 1e-3, along / np.maximum(length, 1e-3), 0.0)
    near = np.clip((cos - _SPILL_SPREAD) / (1.0 - _SPILL_SPREAD), 0.0, 1.0)  # only hues close to the screen's
    take = np.maximum(along, 0.0) * near * (spill / 100)
    out_cb, out_cr = cb - take * ux, cr - take * uy
    return _table(alpha * 255), _table(out_cr + 128), _table(out_cb + 128)


def _table(values: np.ndarray) -> np.ndarray:
    return np.clip(np.round(values), 0, 255).astype(np.uint8).ravel()


def matte(frame: np.ndarray, tables: tuple) -> tuple:
    """(alpha, frame with the spill out) for a BGR frame. The colour is looked up at half the size (the phone sends it
    at half the size anyway) and scaled back up; brightness stays at full size."""
    h, w = frame.shape[:2]
    ycrcb = cv2.cvtColor(frame, cv2.COLOR_BGR2YCrCb)
    small = cv2.resize(ycrcb, (max(1, w // 2), max(1, h // 2)), interpolation=cv2.INTER_AREA)
    index = (small[..., 1].astype(np.uint16) << 8) | small[..., 2]
    alpha_t, cr_t, cb_t = tables
    def up(small_channel):
        return cv2.resize(small_channel, (w, h), interpolation=cv2.INTER_LINEAR)

    alpha = up(alpha_t[index])
    clean = cv2.cvtColor(cv2.merge((ycrcb[..., 0], up(cr_t[index]), up(cb_t[index]))), cv2.COLOR_YCrCb2BGR)
    return alpha, clean


def compose(frame: np.ndarray, alpha: np.ndarray, background: np.ndarray) -> np.ndarray:
    """frame where alpha is 255 over background, all uint8."""
    weight = cv2.merge((alpha, alpha, alpha))
    return cv2.add(cv2.multiply(frame, weight, scale=1 / 255), cv2.multiply(background, cv2.bitwise_not(weight),
                                                                              scale=1 / 255))


def swatch(bgr) -> QIcon:
    """A small square of a BGR colour, for a button."""
    pixmap = QPixmap(12, 12)
    pixmap.fill(QColor(bgr[2], bgr[1], bgr[0]))
    return QIcon(pixmap)


def sample(frame: np.ndarray, u: float, v: float) -> tuple:
    """The colour around (u, v) of a frame (0..1), as a median so a stray pixel doesn't decide it."""
    h, w = frame.shape[:2]
    x, y = min(max(round(u * (w - 1)), 0), w - 1), min(max(round(v * (h - 1)), 0), h - 1)
    patch = frame[max(0, y - _SAMPLE):y + _SAMPLE + 1, max(0, x - _SAMPLE):x + _SAMPLE + 1].reshape(-1, 3)
    return tuple(int(c) for c in np.median(patch, axis=0))


@dataclass
class Settings:
    """What a frame step needs, swapped whole (the stream thread reads it)."""
    on: bool = False
    tables: Optional[tuple] = None
    background: str = "colour"
    colour: tuple = _DEFAULT_COLOUR
    picture: Optional[np.ndarray] = None

    def __post_init__(self):
        self._backdrops = {}

    def backdrop(self, w: int, h: int) -> np.ndarray:
        if (w, h) not in self._backdrops:
            if self.background == "picture" and self.picture is not None:
                made = cover(self.picture, w, h)
            else:
                made = np.empty((h, w, 3), np.uint8)
                made[:] = self.colour
            self._backdrops = {(w, h): made}
        return self._backdrops[(w, h)]


def key(frame: np.ndarray, settings: Settings) -> np.ndarray:
    if not settings.on or settings.tables is None:
        return frame
    alpha, clean = matte(frame, settings.tables)
    return compose(clean, alpha, settings.backdrop(frame.shape[1], frame.shape[0]))


class ChromaKeyPlugin(TelescopePlugin):
    name = "chroma_key"
    title = N_("Chroma key")
    about = N_("Swaps a green or blue screen behind you for a colour or a picture, with no download.")
    category = "look"
    icon = "eyedropper"
    when_off = N_("Without it there's no Chroma key card and a green screen stays green.")
    optional = True
    in_presets = True
    device_local = True  # the screen behind you is only in that camera's picture
    panel_region = "right"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._screen = "green"
        self._picked: Optional[tuple] = None  # BGR picked on the preview
        self._colour = _DEFAULT_COLOUR
        self._picture_path = ""
        self._picture: Optional[np.ndarray] = None
        self._picture_problem = ""
        self._last: Optional[np.ndarray] = None  # the last frame before keying: what a pick samples
        self._picking = False  # keying waits while the screen's colour is picked, so the screen shows to click on
        self._settings = Settings()
        bus.preview_pick_changed.connect(lambda owner: setattr(self, "_picking", owner == self.name))

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._switch = Switch("")
        self._switch.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        self._switch.setToolTip(tr("Swap the screen behind you"))
        self._switch.toggled.connect(self._changed)
        add_card_header(lay, tr("Chroma key"), self.icon, action=self._switch, hue=self.category)
        dim_until_paired(card, self._bus)

        self._screen_btns = {}
        group = QButtonGroup(card)
        for key_name, text, _bgr in SCREENS:
            btn = SegmentButton(tr(text), value=key_name)
            btn.clicked.connect(lambda _c=False, k=key_name: self._set_screen(k))
            group.addButton(btn)
            self._screen_btns[key_name] = btn
        self._screen_btns["green"].setChecked(True)
        self._screen_btns["picked"].setToolTip(tr("Pick the screen's colour from the preview"))
        lay.addLayout(_row(tr("Screen"), segmented_row(*self._screen_btns.values()), stretch=True))

        self._sliders = {}
        for name, label, default, tip in (
                ("tolerance", tr("Tolerance"), 40, tr("Higher takes out more shades of the screen")),
                ("softness", tr("Edges"), 30, tr("How soft the edge around you is")),
                ("spill", tr("Spill"), 60, tr("Takes the screen's colour out of your edges and skin"))):
            slider = NoScrollSlider(Qt.Orientation.Horizontal)
            slider.setRange(0, 100)
            slider.setValue(default)
            slider.set_default(default)
            slider.setToolTip(tip)
            readout = value_label(str(default))
            slider.valueChanged.connect(lambda v, r=readout: (r.setText(str(v)), self._changed()))
            lay.addLayout(_row(label, slider_row(slider, readout), stretch=True))
            self._sliders[name] = (slider, readout)

        self._bg_btns = {}
        group = QButtonGroup(card)
        for key_name, text in BACKGROUNDS:
            btn = SegmentButton(tr(text), value=key_name)
            btn.clicked.connect(lambda _c=False, k=key_name: self._set_background(k))
            group.addButton(btn)
            self._bg_btns[key_name] = btn
        self._bg_btns["colour"].setChecked(True)
        lay.addLayout(_row(tr("Behind you"), segmented_row(*self._bg_btns.values()), stretch=True))

        self._colour_btn = QPushButton(tr("Choose…"))
        self._colour_btn.setToolTip(tr("Pick the colour that goes behind you"))
        self._colour_btn.clicked.connect(self._choose_colour)
        self._colour_row = control_row_widget(tr("Colour"), self._colour_btn, stretch=True)
        lay.addWidget(self._colour_row)

        self._picture_btn = QPushButton(tr("Choose…"))
        self._picture_btn.setToolTip(tr("Pick the picture that goes behind you"))
        self._picture_btn.clicked.connect(self._choose_picture)
        self._picture_lbl = ElidingLabel(tr("No picture yet"), mode=Qt.TextElideMode.ElideMiddle)
        self._picture_lbl.setObjectName("setting_sub")
        pick = QHBoxLayout()
        pick.setSpacing(8)
        pick.addWidget(self._picture_btn)
        pick.addWidget(self._picture_lbl, 1)
        self._picture_row = control_row_widget(tr("Picture"), pick, stretch=True)
        lay.addWidget(self._picture_row)

        self._status = wrapped_note("")
        lay.addWidget(self._status)
        self._show()
        return card

    def create_actions(self) -> list:
        g = tr("Chroma key")
        return [
            button_action("chroma_key.on", tr("Chroma key"), g, self._switch),
            choice_action("chroma_key.screen", tr("Chroma key screen"), g, buttons=list(self._screen_btns.values())),
            slider_action("chroma_key.tolerance", tr("Chroma key tolerance"), g, self._sliders["tolerance"][0],
                          step=5.0, readout=self._sliders["tolerance"][1]),
        ]

    # ── Handlers (Qt thread) ──────────────────────────────────────────────

    def _key_colour(self) -> Optional[tuple]:
        if self._screen == "picked":
            return self._picked
        return next(bgr for k, _t, bgr in SCREENS if k == self._screen)

    def _changed(self, *_):
        colour = self._key_colour()
        tables = None
        if colour is not None:
            tables = key_tables(colour, *(self._sliders[n][0].value() for n in ("tolerance", "softness", "spill")))
        self._settings = Settings(self._switch.isChecked(), tables, self._background(), self._colour, self._picture)
        self._show()
        self._host.schedule_save()

    def _set_screen(self, screen: str):
        if screen == "picked":
            self._pick()
            if self._picked is None:  # nothing picked yet: stay on what it was until a colour comes in
                self._screen_btns[self._screen].setChecked(True)
                return
        self._screen = screen
        self._screen_btns[screen].setChecked(True)
        self._changed()

    def _pick(self):
        if not self._host.pick_on_preview(self.name, "point", self._picked_point,
                                          hint=tr("Click the screen behind you on the preview. Esc cancels.")):
            self._status.setText(tr("Start streaming to pick the screen's colour on the preview."))
            self._status.setVisible(True)
            set_status_kind(self._status, "status_warn")

    def _picked_point(self, pick):
        if pick is None:
            return
        # The key's own input at the spot clicked: shown is already mapped back past later warps (Desk mode), and this
        # frame comes before anything later changes colours (Desk's Writing clean-up), unlike the preview's picture
        if self._last is not None:
            frame, (u, v) = self._last, pick.shown[0]
        elif pick.picture is not None and pick.in_picture:
            frame, (u, v) = pick.picture, pick.in_picture[0]
        else:
            return
        colour = sample(frame, u, v)
        if not keyable(colour):
            self._status.setText(tr("That spot has hardly any colour. Click the screen itself."))
            self._status.setVisible(True)
            set_status_kind(self._status, "status_warn")
            return
        self._picked = colour
        self._screen = "picked"
        self._screen_btns["picked"].setChecked(True)
        self._changed()

    def _background(self) -> str:
        return next((k for k, btn in self._bg_btns.items() if btn.isChecked()), "colour")

    def _set_background(self, background: str):
        self._bg_btns[background].setChecked(True)
        self._changed()

    def _choose_colour(self):
        b, g, r = self._colour
        chosen = QColorDialog.getColor(QColor(r, g, b), self._colour_btn.window(), tr("Colour behind you"))
        if chosen.isValid():
            self._colour = (chosen.blue(), chosen.green(), chosen.red())
            self._changed()

    def _choose_picture(self):
        start = str(Path(self._picture_path).parent) if self._picture_path else str(Path.home())
        path, _filter = QFileDialog.getOpenFileName(self._picture_btn.window(), tr("Choose a picture"), start,
                                                    tr(_PICTURES))
        if path:
            self._load_picture(path)
            if not self._picture_problem:
                self._bg_btns["picture"].setChecked(True)
            self._changed()

    def _load_picture(self, path: str):
        self._picture_path = path
        self._picture = read_picture(path) if path else None
        self._picture_problem = tr("Telescope can't open that picture.") if path and self._picture is None else ""
        self._picture_lbl.setText(Path(path).name if path else tr("No picture yet"))
        self._picture_lbl.setToolTip(path)

    def _show(self):
        if not hasattr(self, "_status"):
            return
        background = self._background()
        self._colour_row.setVisible(background == "colour")
        self._picture_row.setVisible(background == "picture")
        self._colour_btn.setIcon(swatch(self._colour))
        if self._picked is not None:
            self._screen_btns["picked"].setIcon(swatch(self._picked))
            self._screen_btns["picked"].setToolTip(tr("The colour picked from the preview. Click to pick again."))
        text = ""
        if self._switch.isChecked() and background == "picture" and self._picture_problem:
            text = self._picture_problem
        elif self._switch.isChecked() and background == "picture" and self._picture is None:
            text = tr("Choose a picture to put behind you.")
        self._status.setText(text)
        self._status.setVisible(bool(text))
        set_status_kind(self._status, "status_warn" if text else "dim")

    # ── Frames (worker thread) ────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        if self._picking:
            self._last = frame.copy()  # a later plugin may draw on frame in place
            return frame
        self._last = frame
        return key(frame, self._settings)

    def frame_step(self):
        settings = self._settings
        if not settings.on or settings.tables is None:
            return None
        return lambda frame: key(frame, settings)

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"on": self._switch.isChecked(), "screen": self._screen,
                "picked": list(self._picked) if self._picked else None,
                "tolerance": self._sliders["tolerance"][0].value(), "softness": self._sliders["softness"][0].value(),
                "spill": self._sliders["spill"][0].value(), "background": self._background(),
                "colour": list(self._colour), "picture": self._picture_path}

    def set_config(self, cfg: dict):
        def bgr(value):
            ok = (isinstance(value, list) and len(value) == 3
                  and all(isinstance(c, int) and not isinstance(c, bool) and 0 <= c <= 255 for c in value))
            return tuple(value) if ok else None

        self._picked = bgr(cfg.get("picked"))
        if self._picked is not None and not keyable(self._picked):
            self._picked = None
        screen = cfg.get("screen") if cfg.get("screen") in {k for k, *_ in SCREENS} else "green"
        self._screen = "green" if screen == "picked" and self._picked is None else screen
        self._colour = bgr(cfg.get("colour")) or _DEFAULT_COLOUR
        picture = cfg.get("picture")
        self._load_picture(picture if isinstance(picture, str) else "")
        background = cfg.get("background") if cfg.get("background") in dict(BACKGROUNDS) else "colour"
        widgets = [self._switch, *self._screen_btns.values(), *self._bg_btns.values(),
                   *(s for s, _r in self._sliders.values())]
        for w in widgets:
            w.blockSignals(True)
        self._switch.setChecked(cfg.get("on") is True)
        self._screen_btns[self._screen].setChecked(True)
        self._bg_btns[background].setChecked(True)
        for name, default in (("tolerance", 40), ("softness", 30), ("spill", 60)):
            slider, readout = self._sliders[name]
            slider.setValue(_clamp(cfg.get(name), default, 0, 100))
            readout.setText(str(slider.value()))
        for w in widgets:
            w.blockSignals(False)
        self._changed()

    def diagnostics(self) -> dict:
        return {"Chroma key": f"{'on' if self._switch.isChecked() else 'off'}, {self._screen} screen, "
                              f"{self._background()} behind"}
