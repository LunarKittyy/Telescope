"""Cinema tools: focus peaking, zebras, false colour and scopes, drawn on the preview only (telescope.overlays)."""
from dataclasses import dataclass, replace
from typing import Optional

import cv2
import numpy as np
from PyQt6.QtCore import QRectF, Qt
from PyQt6.QtGui import QColor, QFont, QFontMetricsF, QImage, QPainter
from PyQt6.QtWidgets import QButtonGroup, QComboBox, QVBoxLayout, QWidget

from telescope import scopes
from telescope.overlays import PreviewLayer
from telescope.plugin import SettingsSection, TelescopePlugin
from telescope.shortcuts import SHORTCUT_VALUE_ROLE, button_action, choice_action
from telescope.widgets.settings_search import keywords
from telescope.widgets.common import (
    NoScrollComboBox, NoScrollSlider, SegmentButton, Switch, add_card_header, card_layout, control_row as _row,
    create_card, dim_until_paired, segmented_row, slider_row, value_label, wrapped_note,
)
from telescope.i18n import N_, tr

SCOPES = (("off", N_("Off")), ("histogram", N_("Histogram")), ("waveform", N_("Waveform")),
          ("parade", N_("RGB parade")), ("vectorscope", N_("Vectorscope")))
PEAKING_COLOURS = (("red", N_("Red"), (40, 40, 255)), ("green", N_("Green"), (60, 255, 60)),
                   ("blue", N_("Blue"), (255, 120, 40)), ("white", N_("White"), (255, 255, 255)))
SIZES = (("small", N_("Small"), 0.26), ("medium", N_("Medium"), 0.36), ("large", N_("Large"), 0.5))
_SCOPE_FPS = 12  # scopes are worked out this often, off the stream thread
_ZEBRA_PERIOD = 8  # px, one black and one clear stripe
_LEGEND = 6  # px, a false colour legend swatch
_WORK_WIDTH = 1280  # px, the widest the tools judge a frame at; a bigger preview gets their marks scaled up

# False colour: (up to this luma %, BGR), the classic exposure scheme; None keeps the picture in grey there
# Translators: the false colour legend on the preview, as short as you can
FALSE_COLOURS = (
    (2, (160, 40, 120), N_("Crushed")),
    (10, (200, 80, 20), N_("Too dark")),
    (38, None, ""),
    (45, (60, 180, 60), N_("Middle grey")),
    (52, None, ""),
    (58, (170, 130, 240), N_("Skin")),
    (95, None, ""),
    (99, (40, 220, 240), N_("Nearly clipped")),
    (100, (40, 40, 230), N_("Clipped")),
)


def _clamp(value, default: int, lo: int, hi: int) -> int:
    try:
        return min(hi, max(lo, int(value)))
    except (TypeError, ValueError):
        return default


def _false_colour_lut() -> np.ndarray:
    lut = np.zeros((1, 256, 3), np.uint8)
    lo = 0
    for upper, colour, _name in FALSE_COLOURS:
        hi = min(255, round(upper / 100 * 255))
        for level in range(lo, hi + 1):
            lut[0, level] = colour if colour is not None else (level * 0.8 + 20,) * 3  # the grey a little flatter
        lo = hi + 1
    return lut


_FALSE_LUT = _false_colour_lut()


def false_colour(frame: np.ndarray, luma: Optional[np.ndarray] = None) -> np.ndarray:
    """The frame with each pixel coloured by how bright it is (FALSE_COLOURS); luma when it's worked out already."""
    y = scopes.luma(frame) if luma is None else luma
    return cv2.LUT(cv2.merge((y, y, y)), _FALSE_LUT)


def peaking_threshold(sensitivity: int) -> int:
    """How strong an edge has to be to light up, for a sensitivity of 1 to 10."""
    return round(200 - min(max(sensitivity, 1), 10) * 15)


def peaking_mask(frame: np.ndarray, sensitivity: int) -> np.ndarray:
    """Where the frame has sharp edges (what's in focus), as a bool array."""
    grey = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)
    gx = cv2.convertScaleAbs(cv2.Sobel(grey, cv2.CV_16S, 1, 0, ksize=3))
    gy = cv2.convertScaleAbs(cv2.Sobel(grey, cv2.CV_16S, 0, 1, ksize=3))
    return cv2.addWeighted(gx, 0.5, gy, 0.5, 0) > peaking_threshold(sensitivity) // 2


_stripes: dict = {}
_solids: dict = {}


def _stripes_at(h: int, w: int, phase: int) -> np.ndarray:
    """The zebras' diagonal stripes for an h x w frame as a bool array, cached per size; phase moves them along."""
    if (h, w) not in _stripes:
        ys, xs = np.mgrid[0:h, 0:w + _ZEBRA_PERIOD]
        _stripes.clear()
        _stripes[(h, w)] = ((xs + ys) // (_ZEBRA_PERIOD // 2)) % 2 == 0
    phase %= _ZEBRA_PERIOD
    return _stripes[(h, w)][:, phase:phase + w]


def zebra_mask(frame: np.ndarray, level: int, phase: int = 0, luma: Optional[np.ndarray] = None) -> np.ndarray:
    """Diagonal stripes where luma is at level % or more, as a bool array; phase moves them along."""
    h, w = frame.shape[:2]
    y = scopes.luma(frame) if luma is None else luma
    return (y >= round(level / 100 * 255)) & _stripes_at(h, w, phase)


def _paint(frame: np.ndarray, mask: np.ndarray, colour: tuple):
    """colour on frame wherever mask (uint8, the frame's size) is set, in place; far quicker than frame[mask] = colour."""
    key = (frame.shape, colour)
    if key not in _solids:
        for old in [k for k in _solids if k[0] != frame.shape]:
            del _solids[old]
        _solids[key] = np.full(frame.shape, colour, np.uint8)
    cv2.copyTo(_solids[key], mask, frame)


@dataclass(frozen=True)
class Look:
    """What the tools draw, swapped whole when anything changes (the stream and analysis threads read it)."""
    peaking: bool = False
    zebras: bool = False
    false_colour: bool = False
    scope: str = "off"
    peaking_colour: str = "red"
    peaking_level: int = 5
    zebra_level: int = 95
    scope_size: str = "medium"

    def any(self) -> bool:
        return self.peaking or self.zebras or self.false_colour or self.scope != "off"


class CinemaLayer(PreviewLayer):
    """The tools as a preview layer: the pixel tools in tint(), the scope worked out in analyse() and drawn in paint()."""

    order = -10  # under anything else on the preview (a pick's corners stay on top)

    def __init__(self):
        self.look = Look()
        self._phase = 0
        self._scope: Optional[tuple] = None  # (name, QImage) last worked out

    @property
    def analyse_fps(self) -> float:
        return _SCOPE_FPS if self.look.scope != "off" else 0.0

    def tint(self, frame: np.ndarray) -> np.ndarray:
        look = self.look
        h, w = frame.shape[:2]
        # Judged on a copy at most _WORK_WIDTH wide, so a full screen pop-out costs about what the card does
        small = scopes.shrink(frame, _WORK_WIDTH)
        scaled = small is not frame

        def full(mask: np.ndarray) -> np.ndarray:
            if not scaled:
                return mask.view(np.uint8)
            # Scaled smoothly and cut at half way, so edges don't come out as steps
            grown = cv2.resize(mask.view(np.uint8) * 255, (w, h), interpolation=cv2.INTER_LINEAR)
            return cv2.threshold(grown, 127, 1, cv2.THRESH_BINARY)[1]

        # Every tool judges the picture as it came in, then they all paint: one tool's paint isn't another's edge
        luma = scopes.luma(small) if look.false_colour or look.zebras else None
        zebras = peaks = None
        if look.zebras:
            self._phase = (self._phase + 1) % _ZEBRA_PERIOD
            bright = full(luma >= round(look.zebra_level / 100 * 255))
            # The stripes are drawn at the frame's own size, so they stay crisp however it was judged
            zebras = cv2.bitwise_and(bright, _stripes_at(h, w, self._phase).view(np.uint8))
        if look.peaking:
            peaks = full(peaking_mask(small, look.peaking_level))
        if look.false_colour:
            coloured = false_colour(small, luma)
            frame = cv2.resize(coloured, (w, h), interpolation=cv2.INTER_LINEAR) if scaled else coloured
        if zebras is not None:
            _paint(frame, zebras, (0, 0, 0))
        if peaks is not None:
            _paint(frame, peaks, next((c for key, _n, c in PEAKING_COLOURS if key == look.peaking_colour),
                                      PEAKING_COLOURS[0][2]))
        return frame

    def analyse(self, frame: np.ndarray) -> bool:
        name = self.look.scope
        if name == "off":
            self._scope = None
            return True
        image = np.ascontiguousarray(scopes.scope_image(name, frame))
        h, w = image.shape[:2]
        self._scope = (name, QImage(image.data, w, h, w * 4, QImage.Format.Format_ARGB32).copy())
        return True

    def paint(self, p: QPainter, rect: QRectF):
        look = self.look
        if look.false_colour:
            self._paint_legend(p, rect)
        scope = self._scope
        if look.scope == "off" or scope is None or scope[0] != look.scope:
            return
        image = scope[1]
        share = next((s for key, _n, s in SIZES if key == look.scope_size), SIZES[1][2])
        margin = 8.0
        w = rect.width() * share
        h = w * image.height() / image.width()
        if h > rect.height() * 0.6:
            h = rect.height() * 0.6
            w = h * image.width() / image.height()
        target = QRectF(rect.right() - w - margin, rect.bottom() - h - margin, w, h)
        p.setRenderHint(QPainter.RenderHint.SmoothPixmapTransform)
        p.drawImage(target, image)

    @staticmethod
    def _paint_legend(p: QPainter, rect: QRectF):
        font = QFont(p.font())
        font.setPointSizeF(8.0)
        p.setFont(font)
        fm = QFontMetricsF(font)
        rows = [(colour, tr(name)) for _upper, colour, name in reversed(FALSE_COLOURS) if colour is not None]
        line = fm.height()
        pad = 6.0
        width = max(fm.horizontalAdvance(text) for _c, text in rows) + _LEGEND + 3 * pad
        box = QRectF(rect.x() + 8, rect.bottom() - 8 - len(rows) * line - 2 * pad, width, len(rows) * line + 2 * pad)
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(QColor(0, 0, 0, 150))
        p.drawRoundedRect(box, 4, 4)
        for i, (colour, text) in enumerate(rows):
            top = box.y() + pad + i * line
            p.setBrush(QColor(colour[2], colour[1], colour[0]))
            p.setPen(Qt.PenStyle.NoPen)
            p.drawRect(QRectF(box.x() + pad, top + (line - _LEGEND) / 2, _LEGEND, _LEGEND))
            p.setPen(QColor(255, 255, 255))
            p.drawText(QRectF(box.x() + 2 * pad + _LEGEND, top, width, line), Qt.AlignmentFlag.AlignVCenter, text)


class CinemaPlugin(TelescopePlugin):
    name = "cinema"
    title = N_("Cinema tools")
    about = N_("Focus peaking, zebras, false colour and scopes on the preview, for judging focus and exposure.")
    when_off = N_("Without it there's no Cinema tools card and the preview shows just the picture.")
    optional = True
    panel_region = "right"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._layer = CinemaLayer()
        self._shown = False  # the layer is on the preview

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Cinema tools"), "focus", hue="camera")
        dim_until_paired(card, self._bus)
        self._peaking_switch = Switch(tr("Focus peaking"))
        self._peaking_switch.setToolTip(tr("Outline what's sharp"))
        self._zebras_switch = Switch(tr("Zebras"))
        self._zebras_switch.setToolTip(tr("Stripe what's about to clip to white"))
        self._false_switch = Switch(tr("False colour"))
        self._false_switch.setToolTip(tr("Colour the picture by how bright it is"))
        for switch in (self._peaking_switch, self._zebras_switch, self._false_switch):
            switch.toggled.connect(self._changed)
            lay.addWidget(switch)
        self._scope_combo = NoScrollComboBox()
        self._scope_combo.setSizeAdjustPolicy(QComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        self._scope_combo.setMinimumContentsLength(8)  # a long name can't widen the card; the list opens wide enough
        for key, text in SCOPES:
            self._scope_combo.addItem(tr(text), key)
            self._scope_combo.setItemData(self._scope_combo.count() - 1, text, SHORTCUT_VALUE_ROLE)
        self._scope_combo.currentIndexChanged.connect(self._changed)
        lay.addLayout(_row(tr("Scope"), self._scope_combo, stretch=True))
        lay.addWidget(wrapped_note(tr("Only on the preview. Apps never see these.")))
        return card

    def create_settings_sections(self) -> list:
        section = QWidget()
        outer = QVBoxLayout(section)
        outer.setContentsMargins(0, 0, 0, 0)
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Cinema tools"), "focus", hue="camera")
        keywords(card, tr("focus peaking zebras false colour color scopes waveform parade vectorscope histogram "
                          "exposure monitor", ctx="search"))
        outer.addWidget(card)

        self._colour_btns = {}
        group = QButtonGroup(card)
        for key, text, _bgr in PEAKING_COLOURS:
            btn = SegmentButton(tr(text), value=key)
            btn.clicked.connect(self._changed)
            group.addButton(btn)
            self._colour_btns[key] = btn
        lay.addLayout(_row(tr("Peaking colour"), segmented_row(*self._colour_btns.values()), stretch=True))

        self._peaking_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._peaking_slider.setRange(1, 10)
        self._peaking_slider.set_default(5)
        self._peaking_slider.setToolTip(tr("Higher outlines softer edges too"))
        self._peaking_lbl = value_label()
        self._peaking_slider.valueChanged.connect(self._changed)
        lay.addLayout(_row(tr("Peaking sensitivity"), slider_row(self._peaking_slider, self._peaking_lbl), stretch=True))

        self._zebra_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._zebra_slider.setRange(70, 100)
        self._zebra_slider.set_default(95)
        self._zebra_slider.setToolTip(tr("How bright something has to be to get stripes"))
        self._zebra_lbl = value_label()
        self._zebra_slider.valueChanged.connect(self._changed)
        lay.addLayout(_row(tr("Zebras from"), slider_row(self._zebra_slider, self._zebra_lbl), stretch=True))

        self._size_btns = {}
        group = QButtonGroup(card)
        for key, text, _share in SIZES:
            btn = SegmentButton(tr(text), value=key)
            btn.clicked.connect(self._changed)
            group.addButton(btn)
            self._size_btns[key] = btn
        lay.addLayout(_row(tr("Scope size"), segmented_row(*self._size_btns.values()), stretch=True))
        self._show_settings()
        return [SettingsSection("video", section, order=40, on_show=self._show_settings)]

    def create_actions(self) -> list:
        g = tr("Cinema tools")
        return [
            button_action("cinema.peaking", tr("Focus peaking"), g, self._peaking_switch),
            button_action("cinema.zebras", tr("Zebras"), g, self._zebras_switch),
            button_action("cinema.false_colour", tr("False colour"), g, self._false_switch),
            choice_action("cinema.scope", tr("Scope"), g, combo=self._scope_combo),
        ]

    # ── Handlers (Qt thread) ──────────────────────────────────────────────

    def _settings_built(self) -> bool:
        return hasattr(self, "_peaking_slider")

    def _changed(self, *_):
        look = replace(self._layer.look, peaking=self._peaking_switch.isChecked(),
                       zebras=self._zebras_switch.isChecked(), false_colour=self._false_switch.isChecked(),
                       scope=self._scope_combo.currentData() or "off")
        if self._settings_built():
            look = replace(look, peaking_colour=self._checked(self._colour_btns, look.peaking_colour),
                           peaking_level=self._peaking_slider.value(), zebra_level=self._zebra_slider.value(),
                           scope_size=self._checked(self._size_btns, look.scope_size))
            self._show_readouts(look)
        self._set_look(look)
        self._host.schedule_save()

    @staticmethod
    def _checked(buttons: dict, current: str) -> str:
        return next((key for key, btn in buttons.items() if btn.isChecked()), current)

    def _set_look(self, look: Look):
        self._layer.look = look
        if look.any() and not self._shown:
            self._host.add_preview_layer(self.name, self._layer)
        elif not look.any() and self._shown:
            self._host.remove_preview_layer(self.name)
        self._shown = look.any()
        self._host.refresh_preview()

    def _show_readouts(self, look: Look):
        self._peaking_lbl.setText(str(look.peaking_level))
        self._zebra_lbl.setText(f"{look.zebra_level}%")

    def _show_settings(self):
        if not self._settings_built():
            return
        look = self._layer.look
        widgets = (*self._colour_btns.values(), *self._size_btns.values(), self._peaking_slider, self._zebra_slider)
        for w in widgets:
            w.blockSignals(True)
        self._colour_btns.get(look.peaking_colour, self._colour_btns["red"]).setChecked(True)
        self._size_btns.get(look.scope_size, self._size_btns["medium"]).setChecked(True)
        self._peaking_slider.setValue(look.peaking_level)
        self._zebra_slider.setValue(look.zebra_level)
        for w in widgets:
            w.blockSignals(False)
        self._show_readouts(look)

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        look = self._layer.look
        return {"peaking": look.peaking, "zebras": look.zebras, "false_colour": look.false_colour,
                "scope": look.scope, "peaking_colour": look.peaking_colour, "peaking_level": look.peaking_level,
                "zebra_level": look.zebra_level, "scope_size": look.scope_size}

    def set_config(self, cfg: dict):
        look = Look(
            peaking=cfg.get("peaking") is True, zebras=cfg.get("zebras") is True,
            false_colour=cfg.get("false_colour") is True,
            scope=cfg.get("scope") if cfg.get("scope") in dict(SCOPES) else "off",
            peaking_colour=cfg.get("peaking_colour") if cfg.get("peaking_colour") in
            {key for key, *_ in PEAKING_COLOURS} else "red",
            peaking_level=_clamp(cfg.get("peaking_level"), 5, 1, 10),
            zebra_level=_clamp(cfg.get("zebra_level"), 95, 70, 100),
            scope_size=cfg.get("scope_size") if cfg.get("scope_size") in {key for key, *_ in SIZES} else "medium",
        )
        for switch, on in ((self._peaking_switch, look.peaking), (self._zebras_switch, look.zebras),
                           (self._false_switch, look.false_colour)):
            switch.blockSignals(True)
            switch.setChecked(on)
            switch.blockSignals(False)
        self._scope_combo.blockSignals(True)
        self._scope_combo.setCurrentIndex(max(0, self._scope_combo.findData(look.scope)))
        self._scope_combo.blockSignals(False)
        self._set_look(look)
        self._show_settings()

    def diagnostics(self) -> dict:
        look = self._layer.look
        on = [name for name, used in (("peaking", look.peaking), ("zebras", look.zebras),
                                      ("false colour", look.false_colour), (look.scope, look.scope != "off")) if used]
        return {"Cinema tools": ", ".join(on) or "nothing on"}

    def shutdown(self):
        if self._shown:
            self._host.remove_preview_layer(self.name)
            self._shown = False
