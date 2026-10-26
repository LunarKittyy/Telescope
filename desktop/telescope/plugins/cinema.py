"""Cinema tools: focus peaking, zebras, false colour, scopes and a level guide, drawn on the preview only
(telescope.overlays)."""
import math
import time
from dataclasses import dataclass, replace
from typing import Optional

import cv2
import numpy as np
from PyQt6.QtCore import QPointF, QRectF, Qt, QTimer
from PyQt6.QtGui import QColor, QFont, QFontMetricsF, QImage, QPainter, QPen
from PyQt6.QtWidgets import QButtonGroup, QComboBox, QVBoxLayout, QWidget

from telescope import scopes
from telescope.overlays import PreviewLayer
from telescope.plugin import SettingsSection, TelescopePlugin
from telescope.view import View
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
LEVEL_SNAP = 0.5  # degrees: a tilt smaller than this shows as level
LEVEL_LEAVE = 1.0  # degrees: and it has to tilt more than this to stop showing as level, so it doesn't flicker
PITCH_SNAP, PITCH_LEAVE = 1.0, 2.0
_LEVEL_SMOOTH_S = 0.3  # how quickly the guide follows the phone; slower is calmer
_LEVEL_STALE_S = 1.5  # a reading older than this isn't drawn
_LEVEL_UNSEEN_S = 2.0  # the guide not painted for this long (preview hidden, window minimized): the sensors stop
_VIEW_TURN = {None: 0, cv2.ROTATE_90_CLOCKWISE: 90, cv2.ROTATE_180: 180, cv2.ROTATE_90_COUNTERCLOCKWISE: -90}
# Why the level guide can't show, by telescope.sensors.REASONS
LEVEL_PROBLEMS = {
    "not_phone": N_("The level needs a phone's motion sensors."),
    "old_phone": N_("Update Telescope on the phone to use the level."),
    "none": N_("This phone has no motion sensor for the level."),
    "camera": N_("The level works with the phone's own cameras, not an external one."),
    "failed": N_("Can't reach the phone's motion sensors."),
}

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


def wrap_degrees(angle: float) -> float:
    """angle as -180 up to 180."""
    return (angle + 180.0) % 360.0 - 180.0


def shown_roll(roll: float, view: View) -> float:
    """The phone's roll (degrees the horizon is turned clockwise in its picture) as the preview shows it, after the
    view's flip and then its rotation. A mirror turns a clockwise tilt anticlockwise."""
    mirrored = bool(view.flip_h) != bool(view.flip_v)
    return wrap_degrees((-roll if mirrored else roll) + _VIEW_TURN.get(view.rotation, 0))


@dataclass(frozen=True)
class Level:
    """What the level guide draws: the horizon's tilt as shown and the camera's pitch, both smoothed."""
    roll: Optional[float]  # None while the camera points nearly straight up or down
    pitch: float
    roll_level: bool
    pitch_level: bool


class LevelFilter:
    """Smooths the readings so the guide stays calm, and decides when they count as level with a margin either way,
    so it doesn't flicker at the edge. Roll comes in as shown (shown_roll)."""

    def __init__(self, smooth_s: float = _LEVEL_SMOOTH_S):
        self.smooth_s = smooth_s
        self.reset()

    def reset(self):
        self._roll: Optional[float] = None
        self._pitch: Optional[float] = None
        self._at: Optional[float] = None
        self._roll_level = self._pitch_level = False

    def update(self, roll: Optional[float], pitch: Optional[float], now: float) -> Optional[Level]:
        if pitch is None:
            self.reset()
            return None
        dt = None if self._at is None else now - self._at
        # Weighted by the time since the last reading, so a phone sending faster or slower moves it the same
        a = 1.0 if dt is None or dt > 1.0 else 1.0 - math.exp(-max(dt, 0.0) / self.smooth_s)
        if roll is None:
            self._roll = None
        elif self._roll is None or dt is None:
            self._roll = roll
        else:
            self._roll = wrap_degrees(self._roll + a * wrap_degrees(roll - self._roll))
        self._pitch = pitch if self._pitch is None or dt is None else self._pitch + a * (pitch - self._pitch)
        self._at = now
        self._roll_level = self._roll is not None and abs(self._roll) < (LEVEL_LEAVE if self._roll_level else LEVEL_SNAP)
        self._pitch_level = abs(self._pitch) < (PITCH_LEAVE if self._pitch_level else PITCH_SNAP)
        return Level(0.0 if self._roll_level else self._roll, 0.0 if self._pitch_level else self._pitch,
                     self._roll_level, self._pitch_level)


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
    level: bool = False

    def any(self) -> bool:
        return self.peaking or self.zebras or self.false_colour or self.scope != "off" or self.level


class CinemaLayer(PreviewLayer):
    """The tools as a preview layer: the pixel tools in tint(), the scope worked out in analyse() and drawn in paint()."""

    order = -10  # under anything else on the preview (a pick's corners stay on top)

    def __init__(self):
        self.look = Look()
        self._phase = 0
        self._scope: Optional[tuple] = None  # (name, QImage) last worked out
        self.level: Optional[tuple] = None  # (Level, time.monotonic() its reading came) for the level guide
        self.painted_at = 0.0  # time.monotonic() the preview last drew this layer

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
        self.painted_at = time.monotonic()
        if look.false_colour:
            self._paint_legend(p, rect)
        level = self.level
        if look.level and level is not None and time.monotonic() - level[1] < _LEVEL_STALE_S:
            paint_level(p, rect, level[0])
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


_LEVEL_GREEN = QColor(90, 225, 120)
_LEVEL_WHITE = QColor(255, 255, 255, 225)


def level_line(rect: QRectF, roll: float) -> tuple:
    """The two ends of the level guide's moving line: through the middle of rect, turned clockwise by roll degrees."""
    half = min(rect.width(), rect.height() * 1.6) * 0.3
    a = math.radians(roll)
    dx, dy = half * math.cos(a), half * math.sin(a)  # widget y goes down, so a positive angle turns clockwise
    c = rect.center()
    return QPointF(c.x() - dx, c.y() - dy), QPointF(c.x() + dx, c.y() + dy)


def paint_level(p: QPainter, rect: QRectF, level: Level):
    """The level guide: fixed marks for level on both sides, a line along the horizon between them and the pitch."""
    p.save()
    p.setRenderHint(QPainter.RenderHint.Antialiasing)
    c = rect.center()
    half = min(rect.width(), rect.height() * 1.6) * 0.3
    colour = _LEVEL_GREEN if level.roll_level else _LEVEL_WHITE
    shadow = QPen(QColor(0, 0, 0, 110), 4.0, cap=Qt.PenCapStyle.RoundCap)
    pen = QPen(colour, 2.0, cap=Qt.PenCapStyle.RoundCap)
    lines = []
    if level.roll is not None:
        gap, mark = 8.0, max(16.0, half * 0.18)
        lines = [(QPointF(c.x() - half - gap - mark, c.y()), QPointF(c.x() - half - gap, c.y())),
                 (QPointF(c.x() + half + gap, c.y()), QPointF(c.x() + half + gap + mark, c.y())),
                 level_line(rect, level.roll)]
    for line_pen in (shadow, pen):
        p.setPen(line_pen)
        for a, b in lines:
            p.drawLine(a, b)
    font = QFont(p.font())
    font.setPointSizeF(9.0)
    p.setFont(font)
    fm = QFontMetricsF(font)
    labels = []
    if level.roll is not None:
        labels.append((f"{abs(level.roll):.1f}°", colour, QPointF(c.x() + half + 8.0, c.y() - 8.0)))
    pitch_text = "0.0°" if level.pitch_level else ("▲" if level.pitch > 0 else "▼") + f" {abs(level.pitch):.1f}°"
    pitch_width = fm.horizontalAdvance(pitch_text)
    labels.append((pitch_text, _LEVEL_GREEN if level.pitch_level else _LEVEL_WHITE,
                   QPointF(c.x() - pitch_width / 2, c.y() + fm.height() + 10.0)))
    for text, text_colour, at in labels:
        box = QRectF(at.x() - 4, at.y() - fm.ascent() - 2, fm.horizontalAdvance(text) + 8, fm.height() + 4)
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(QColor(0, 0, 0, 130))
        p.drawRoundedRect(box, 3, 3)
        p.setPen(text_colour)
        p.drawText(at, text)
    p.restore()


class CinemaPlugin(TelescopePlugin):
    name = "cinema"
    title = N_("Cinema tools")
    about = N_("Focus peaking, zebras, false colour, scopes and a level on the preview, for judging focus, exposure "
               "and a straight horizon.")
    category = "preview"
    icon = "focus"
    when_off = N_("Without it there's no Cinema tools card and the preview shows just the picture.")
    optional = True
    panel_region = "right"

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self._layer = CinemaLayer()
        self._shown = False  # the layer is on the preview
        self._level_filter = LevelFilter()
        self._wants_sensors = False
        # While the level is on: the sensors only run while something actually draws the guide
        self._seen_timer = QTimer()
        self._seen_timer.setInterval(1000)
        self._seen_timer.timeout.connect(self._check_seen)
        bus.sensors_changed.connect(self._on_sensors)
        bus.view_changed.connect(lambda _view: self._level_filter.reset())  # the old tilt as shown means nothing now
        bus.device_changed.connect(lambda _id: self._level_filter.reset())

    # ── Card ──────────────────────────────────────────────────────────────

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Cinema tools"), self.icon, hue=self.category)
        dim_until_paired(card, self._bus)
        self._peaking_switch = Switch(tr("Focus peaking"))
        self._peaking_switch.setToolTip(tr("Outline what's sharp"))
        self._zebras_switch = Switch(tr("Zebras"))
        self._zebras_switch.setToolTip(tr("Stripe what's about to clip to white"))
        self._false_switch = Switch(tr("False colour"))
        self._false_switch.setToolTip(tr("Colour the picture by how bright it is"))
        self._level_switch = Switch(tr("Level"))
        self._level_switch.setToolTip(tr("Show the horizon from the phone's motion sensors"))
        for switch in (self._peaking_switch, self._zebras_switch, self._false_switch, self._level_switch):
            switch.toggled.connect(self._changed)
            lay.addWidget(switch)
        self._level_note = wrapped_note("")
        self._level_note.hide()
        lay.addWidget(self._level_note)
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
        add_card_header(lay, tr("Cinema tools"), self.icon, hue=self.category)
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
            button_action("cinema.level", tr("Level"), g, self._level_switch),
            choice_action("cinema.scope", tr("Scope"), g, combo=self._scope_combo),
        ]

    # ── Handlers (Qt thread) ──────────────────────────────────────────────

    def _settings_built(self) -> bool:
        return hasattr(self, "_peaking_slider")

    def _changed(self, *_):
        look = replace(self._layer.look, peaking=self._peaking_switch.isChecked(),
                       zebras=self._zebras_switch.isChecked(), false_colour=self._false_switch.isChecked(),
                       scope=self._scope_combo.currentData() or "off", level=self._level_switch.isChecked())
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
        if look.level and not self._seen_timer.isActive():
            self._layer.painted_at = time.monotonic()  # just added: the preview gets its chance to draw it
            self._seen_timer.start()
        elif not look.level:
            self._seen_timer.stop()
        self._want_sensors(look.level and self._seen())
        self._host.refresh_preview()

    # ── Level guide ───────────────────────────────────────────────────────

    def _want_sensors(self, on: bool):
        if on == self._wants_sensors:
            return
        self._wants_sensors = on
        self._host.want_sensors(self.name, ["orientation"] if on else [])
        if not on:
            self._level_filter.reset()
            self._layer.level = None
        self._show_level_status()

    def _seen(self) -> bool:
        """Whether the preview drew the guide lately."""
        return time.monotonic() - self._layer.painted_at < _LEVEL_UNSEEN_S

    def _check_seen(self):
        self._want_sensors(self._layer.look.level and self._seen())

    def _on_sensors(self, source_id: str):
        if not self._wants_sensors or source_id != self._host.focused_source_id():
            return
        self._show_level_status()
        reading = self._host.sensor_reading(source_id)
        if reading is None or reading.pitch is None:
            if self._layer.level is not None:
                self._level_filter.reset()
                self._layer.level = None
                self._host.refresh_preview()
            return
        old = self._layer.level
        if old is not None and reading.orientation_at <= old[1]:
            return  # a heartbeat or another kind: nothing new about the horizon, so it can still go stale
        roll = shown_roll(reading.roll, self._host.view()) if reading.roll is not None else None
        level = self._level_filter.update(roll, reading.pitch, reading.orientation_at)
        self._layer.level = (level, reading.orientation_at) if level is not None else None
        # Painted again only when what's drawn changes, at the tenth of a degree it shows
        if old is None or level is None or _drawn(old[0]) != _drawn(level):
            self._host.refresh_preview()

    def _show_level_status(self):
        if not hasattr(self, "_level_note"):
            return
        status = self._host.sensor_status() if self._wants_sensors else None
        problem = LEVEL_PROBLEMS.get(status.reason) if status is not None and status.state == "unavailable" else None
        self._level_note.setText(tr(problem) if problem else "")
        self._level_note.setVisible(problem is not None)

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
                "level": look.level, "scope": look.scope, "peaking_colour": look.peaking_colour, "peaking_level": look.peaking_level,
                "zebra_level": look.zebra_level, "scope_size": look.scope_size}

    def set_config(self, cfg: dict):
        look = Look(
            peaking=cfg.get("peaking") is True, zebras=cfg.get("zebras") is True,
            false_colour=cfg.get("false_colour") is True, level=cfg.get("level") is True,
            scope=cfg.get("scope") if cfg.get("scope") in dict(SCOPES) else "off",
            peaking_colour=cfg.get("peaking_colour") if cfg.get("peaking_colour") in
            {key for key, *_ in PEAKING_COLOURS} else "red",
            peaking_level=_clamp(cfg.get("peaking_level"), 5, 1, 10),
            zebra_level=_clamp(cfg.get("zebra_level"), 95, 70, 100),
            scope_size=cfg.get("scope_size") if cfg.get("scope_size") in {key for key, *_ in SIZES} else "medium",
        )
        for switch, on in ((self._peaking_switch, look.peaking), (self._zebras_switch, look.zebras),
                           (self._false_switch, look.false_colour), (self._level_switch, look.level)):
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
                                      ("false colour", look.false_colour), (look.scope, look.scope != "off"),
                                      ("level", look.level)) if used]
        return {"Cinema tools": ", ".join(on) or "nothing on"}

    def shutdown(self):
        self._seen_timer.stop()
        self._want_sensors(False)
        if self._shown:
            self._host.remove_preview_layer(self.name)
            self._shown = False


def _drawn(level: Level) -> tuple:
    """What the guide shows of level, to the tenth of a degree."""
    return (None if level.roll is None else round(level.roll, 1), round(level.pitch, 1), level.roll_level,
            level.pitch_level)
