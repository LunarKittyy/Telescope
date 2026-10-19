import math
import threading
import weakref

from PyQt6.QtCore import QByteArray, QCoreApplication, QEventLoop, QMetaObject, QThread, QPoint, QPointF, QRect, QRectF, QSize, Qt, pyqtSignal
from PyQt6 import sip
from PyQt6.QtSvg import QSvgRenderer
from PyQt6.QtGui import (
    QBrush, QColor, QFont, QFontMetrics, QFontMetricsF, QIcon, QPainter, QPainterPath, QPen, QPixmap,
)
from PyQt6.QtWidgets import (
    QAbstractButton, QAbstractSlider, QAbstractSpinBox, QApplication, QCheckBox, QComboBox, QDoubleSpinBox, QFrame, QGraphicsOpacityEffect, QHBoxLayout, QLabel, QLayout, QPushButton,
    QSlider, QSpinBox, QSizePolicy, QStyle, QStyleOptionButton, QStyleOptionSlider, QVBoxLayout, QWidget,
)

from telescope import theme
from telescope.widgets.line_icons import LINE_ICONS, line_icon


# ── Shared desktop UI primitives ─────────────────────────────────────────────
# Layout rules every card follows (keep new controls inside them):
#   - Type: one family. 14pt dialog titles, 11pt card titles, 9.5pt everything else, 7.5pt caps
#     section headings.
#   - Row: [label column, top-anchored][control column]; rows are at least ROW_HEIGHT tall.
#   - Text, number and select inputs fill the control column (control_row(..., stretch=True)).
#   - Segmented toggles (SegmentButton) fill the control column in equal shares, like the inputs
#     around them; outside a card they use SEGMENT_WIDTH per segment. Text fits the segment, not
#     the other way round.
#   - Slider rows are slider_row(): [slider][value column]; in a card where any slider row has a
#     direct-entry spinbox, every slider row reserves that column (gutter=True) so tracks end together.
#   - Card-level actions (Pair, Reset) go in the card header, not in a row.
#   - On/off settings are a Switch at the end of the row (control_row(..., stretch=True)), so the row reads as one line.
#   - Each card has a hue (theme.PLUGIN_HUES) for its header icon, sliders and readouts; selections stay theme.FILL.
#   - Buttons: one primary per view (the thing you came to do), "danger" only for destructive
#     actions, everything else default. A lone action button is BUTTON_WIDTH wide; buttons sharing
#     a row split it equally (button_row). Dialogs: dialog_header() on top, sections are cards,
#     dialog_buttons() bottom-right.

#   - Every width and height constant below is in design pixels, measured against the Inter UI
#     font. Pass them through ui_px(), which scales them up when the font the user actually gets
#     is wider (another fallback font, larger system text, a different DPI), so text keeps fitting.

FORM_LABEL_WIDTH = 96

SEGMENT_WIDTH = 78
"""Width of one segment where a toggle has no control column to fill (the header)."""

BUTTON_WIDTH = 160
"""Width of a lone action button; its text has to fit, not the other way round."""

DIALOG_BUTTON_WIDTH = 96
"""Width of the Close / OK / Cancel buttons in a dialog's bottom bar."""

ROW_HEIGHT = 36
"""Minimum height of a settings row; matches the input controls."""

VALUE_COL_WIDTH = 56
"""Width of numeric readout (fixed, right-aligned so readouts line up)."""

SPIN_COL_WIDTH = 78
"""Width of the direct-entry spinbox beside a slider."""

SPIN_COL_GUTTER = SPIN_COL_WIDTH + 8
"""Space slider rows reserve for alignment with spinbox rows."""

SLIDER_TRACK_WIDTH = 84
"""Minimum width for slider track (floor for draggability; apply via stretch_slider())."""

_SCALE_PROBE = "Apply and reload"
_SCALE_PROBE_PX = 125  # its advance in the button font at the design size (Inter 9.5pt, 96 dpi)
_scale_cache: dict = {}


def _probe_advance() -> float:
    probe = QPushButton(_SCALE_PROBE)
    probe.ensurePolished()
    return probe.fontMetrics().horizontalAdvance(_SCALE_PROBE)


def ui_scale() -> float:
    """How much wider the real UI font is than the design font; never below 1."""
    app = QApplication.instance()
    if app is None:
        return 1.0
    screen = app.primaryScreen()
    key = (len(app.styleSheet()), app.font().key(), screen.logicalDotsPerInch() if screen else 96)
    if key not in _scale_cache:
        _scale_cache[key] = max(1.0, _probe_advance() / _SCALE_PROBE_PX)
    return _scale_cache[key]


def ui_px(design_px: int) -> int:
    """A design-pixel size scaled to the font actually in use (see ui_scale())."""
    return round(design_px * ui_scale())


def stretch_slider(slider: QWidget, minimum: int = SLIDER_TRACK_WIDTH) -> QWidget:
    """Let a slider grow with its column while keeping a draggable minimum."""
    slider.setMinimumWidth(ui_px(minimum))
    slider.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
    return slider


class ElidingLabel(QLabel):
    """Label that elides text instead of forcing row wider (full text in tooltip)."""

    def __init__(self, text: str = "", parent=None,
                 mode: Qt.TextElideMode = Qt.TextElideMode.ElideRight):
        super().__init__(parent)
        self._full = ""
        self._mode = mode
        self.setMinimumWidth(1)
        self.setSizePolicy(QSizePolicy.Policy.Ignored, QSizePolicy.Policy.Preferred)
        self.setText(text)

    def setText(self, text: str):
        self._full = text
        self.setToolTip(text if text else "")
        self._apply_elide()

    def fullText(self) -> str:
        return self._full

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._apply_elide()

    def _apply_elide(self):
        metrics = QFontMetrics(self.font())
        width = max(self.width(), 1)
        super().setText(metrics.elidedText(self._full, self._mode, width))


class FlowLayout(QLayout):
    """Wrapping flow layout that sizes items to content and adapts column count to available space."""

    def __init__(self, parent=None, spacing: int = 6, uniform: bool = False, columns: int = 0):
        super().__init__(parent)
        self._items: list = []
        self._spacing = spacing
        # uniform: equal-width items, rows end flush (grid-like, not pill pile).
        self._uniform = uniform
        # columns > 0 fixes the grid instead of deriving it from the widest item's text.
        self._columns = columns
        self.setContentsMargins(0, 0, 0, 0)

    def addItem(self, item):        self._items.append(item)
    def count(self):                return len(self._items)
    def itemAt(self, i):            return self._items[i] if 0 <= i < len(self._items) else None
    def takeAt(self, i):            return self._items.pop(i) if 0 <= i < len(self._items) else None
    def expandingDirections(self):  return Qt.Orientation(0)
    def hasHeightForWidth(self):    return True
    def heightForWidth(self, width): return self._layout(QRect(0, 0, width, 0), apply=False)

    def setGeometry(self, rect):
        super().setGeometry(rect)
        self._layout(rect, apply=True)

    def sizeHint(self):
        return self.minimumSize()

    def minimumSize(self):
        size = QSize()
        for item in self._items:
            size = size.expandedTo(item.minimumSize())
        m = self.contentsMargins()
        return size + QSize(m.left() + m.right(), m.top() + m.bottom())

    def _layout(self, rect: QRect, apply: bool) -> int:
        """Place items and return total height (measurement pass when apply=False)."""
        if not self._items:
            return 0

        m = self.contentsMargins()
        left = rect.x() + m.left()
        y = rect.y() + m.top()
        avail = max(rect.width() - m.left() - m.right(), 1)

        if self._uniform:
            widest = max(i.sizeHint().width() for i in self._items)
            height = max(i.sizeHint().height() for i in self._items)
            if self._columns:
                per_row = self._columns
            else:
                per_row = max(1, min(len(self._items),
                                     (avail + self._spacing) // (widest + self._spacing)))
            width = (avail - (per_row - 1) * self._spacing) // per_row
            for index, item in enumerate(self._items):
                row, col = divmod(index, per_row)
                if apply:
                    item.setGeometry(QRect(
                        left + col * (width + self._spacing),
                        y + row * (height + self._spacing),
                        width, height))
            rows = -(-len(self._items) // per_row)
            return rows * height + (rows - 1) * self._spacing + m.top() + m.bottom()

        x = left
        line_height = 0
        for item in self._items:
            hint = item.sizeHint()
            if x > left and x + hint.width() > left + avail:
                x = left
                y += line_height + self._spacing
                line_height = 0
            if apply:
                item.setGeometry(QRect(QPoint(x, y), hint))
            x += hint.width() + self._spacing
            line_height = max(line_height, hint.height())

        return y + line_height - rect.y() + m.bottom()


def set_ui_role(widget: QWidget, role: str):
    """Apply a semantic visual role defined by the application QSS."""
    widget.setProperty("uiRole", role)
    style = widget.style()
    style.unpolish(widget)
    style.polish(widget)


def set_status_kind(label: QWidget, kind: str):
    """Switch a label between the status_* QSS roles; a bare setObjectName() doesn't re-apply the stylesheet."""
    label.setObjectName(kind)
    style = label.style()
    style.unpolish(label)
    style.polish(label)


def make_segmented(*buttons: QWidget):
    """Style button run as segmented pill strip (purely presentational; signals untouched)."""
    last = len(buttons) - 1
    for i, btn in enumerate(buttons):
        if len(buttons) == 1:  pos = "only"
        elif i == 0:           pos = "first"
        elif i == last:        pos = "last"
        else:                  pos = "mid"
        btn.setProperty("segmented", True)
        btn.setProperty("segPos", pos)
        style = btn.style()
        style.unpolish(btn)
        style.polish(btn)
    return buttons


class SegmentButton(QPushButton):
    """One checkable segment of a segmented toggle; exclusivity comes from the QButtonGroup it's added to."""

    def __init__(self, text: str = "", parent=None):
        super().__init__(text, parent)
        self.setCheckable(True)
        self.setAutoExclusive(False)

    def minimumSizeHint(self) -> QSize:
        """In a segment row: room for the text in bold, as it turns when picked, plus the theme's 8 px padding."""
        hint = super().minimumSizeHint()
        if self.property("segmented") is not True:
            return hint
        self.ensurePolished()
        bold = QFont(self.font())
        bold.setWeight(QFont.Weight.DemiBold)
        return QSize(math.ceil(QFontMetricsF(bold).horizontalAdvance(self.text())) + 18, hint.height())


OPTION_HEIGHT = 52
"""Height of an OptionButton."""


class OptionButton(SegmentButton):
    """A segment that says what picking it means: its name, and a short note under it. text() stays the name."""

    def __init__(self, text: str, note: str, parent=None):
        super().__init__(text, parent)
        self._note = note
        self.setMinimumHeight(ui_px(OPTION_HEIGHT))

    def note(self) -> str:
        return self._note

    def sizeHint(self) -> QSize:
        return QSize(super().sizeHint().width(), ui_px(OPTION_HEIGHT))

    def paintEvent(self, _event):
        p = QPainter(self)
        opt = QStyleOptionButton()
        self.initStyleOption(opt)
        opt.text = ""
        self.style().drawControl(QStyle.ControlElement.CE_PushButton, opt, p, self)
        on = self.isChecked()
        ink = QColor("#ffffff" if on else theme.TEXT) if self.isEnabled() else QColor(theme.TEXT_DISABLED)
        title, small = QFont(self.font()), QFont(self.font())
        title.setWeight(QFont.Weight.DemiBold)
        small.setPointSizeF(self.font().pointSizeF() * 0.9)
        box = self.rect().adjusted(12, 0, -10, 0)
        half = self.height() // 2
        p.setPen(ink)
        p.setFont(title)
        p.drawText(QRect(box.left(), 0, box.width(), half + 1), int(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignBottom),
                   p.fontMetrics().elidedText(self.text(), Qt.TextElideMode.ElideRight, box.width()))
        faint = QColor(ink)
        faint.setAlphaF(0.85 if on else 0.6)
        p.setPen(faint)
        p.setFont(small)
        p.drawText(QRect(box.left(), half + 2, box.width(), half - 2), int(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignTop),
                   p.fontMetrics().elidedText(self._note, Qt.TextElideMode.ElideRight, box.width()))
        p.end()


SWITCH_W, SWITCH_H = 36, 20
"""Size of a Switch's track."""


class Switch(QCheckBox):
    """An on/off switch: optional text on the left, the track at the right end, so it reads as one line in a row."""

    def __init__(self, text: str = "", parent=None):
        super().__init__(text, parent)
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)

    def sizeHint(self) -> QSize:
        text_w = self.fontMetrics().horizontalAdvance(self.text()) + 12 if self.text() else 0
        return QSize(text_w + ui_px(SWITCH_W), max(ui_px(ROW_HEIGHT) - 4, ui_px(SWITCH_H)))

    def minimumSizeHint(self) -> QSize:
        return QSize(ui_px(SWITCH_W), ui_px(SWITCH_H))

    def hitButton(self, pos: QPoint) -> bool:
        return self.rect().contains(pos)

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        w, h = ui_px(SWITCH_W), ui_px(SWITCH_H)
        track = QRectF(self.width() - w, (self.height() - h) / 2, w, h)
        on, enabled = self.isChecked(), self.isEnabled()
        if not enabled:
            fill, knob = QColor(theme.BORDER_STRONG if on else theme.SURFACE_RAISE), QColor(theme.TEXT_DISABLED)
        elif on:
            fill, knob = QColor(theme.FILL_HOVER if self.underMouse() else theme.FILL), QColor("#ffffff")
        else:
            fill, knob = QColor(theme.BORDER_HOVER if self.underMouse() else theme.BORDER_STRONG), QColor(theme.TEXT_DIM)
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(fill)
        p.drawRoundedRect(track, h / 2, h / 2)
        if self.hasFocus() and self.focusPolicy() != Qt.FocusPolicy.NoFocus and self.property("focusVisible"):
            p.setPen(QPen(QColor(theme.ACCENT), 1.5))
            p.setBrush(Qt.BrushStyle.NoBrush)
            p.drawRoundedRect(track.adjusted(-2, -2, 2, 2), h / 2 + 2, h / 2 + 2)
            p.setPen(Qt.PenStyle.NoPen)
        d = h - 6
        x = track.right() - 3 - d if on else track.left() + 3
        p.setBrush(knob)
        p.drawEllipse(QRectF(x, track.top() + 3, d, d))
        if self.text():
            p.setPen(QColor((theme.TEXT_DIM if self.property("dim_text") else theme.TEXT) if enabled else theme.TEXT_DISABLED))
            p.setFont(self.font())
            text_rect = QRect(0, 0, int(track.left()) - 12, self.height())
            fm = self.fontMetrics()
            if fm.horizontalAdvance(self.text()) > text_rect.width() and self.height() >= 2 * fm.height():
                # A long name in a tight spot (a tile) wraps onto a second line rather than losing its end
                p.drawText(text_rect, int(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter
                                          | Qt.TextFlag.TextWordWrap), self.text())
            else:
                p.drawText(text_rect, int(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter),
                           fm.elidedText(self.text(), Qt.TextElideMode.ElideRight, text_rect.width()))
        p.end()

    def keyPressEvent(self, event):
        self.setProperty("focusVisible", True)
        super().keyPressEvent(event)

    def focusInEvent(self, event):
        self.setProperty("focusVisible", event.reason() in (Qt.FocusReason.TabFocusReason, Qt.FocusReason.BacktabFocusReason))
        super().focusInEvent(event)

    def enterEvent(self, event):
        self.update()
        super().enterEvent(event)

    def leaveEvent(self, event):
        self.update()
        super().leaveEvent(event)


PILL_HEIGHT = 40
"""Height of a StatusPill."""


def cap_baseline(fm: QFontMetrics, top: float, height: float) -> int:
    """Baseline that centres a line's capitals in a band: what reads as centred, unlike Qt's line box with its
    descender room."""
    return round(top + (height + fm.capHeight()) / 2)


def paint_focus(painter: QPainter, widget: QWidget):
    """Keep keyboard focus visible on controls that paint over Qt's standard focus indicator; a click shows none."""
    if widget.hasFocus() and widget.window().testAttribute(Qt.WidgetAttribute.WA_KeyboardFocusChange):
        painter.setPen(QPen(QColor(theme.ACCENT), 2))
        painter.setBrush(Qt.BrushStyle.NoBrush)
        painter.drawRoundedRect(QRectF(widget.rect()).adjusted(2, 2, -2, -2), 7, 7)


def stacked_baselines(top_fm: QFontMetrics, low_fm: QFontMetrics, top: float, height: float,
                      gap: float = 7) -> tuple:
    """Baselines for two lines read as one block (a value over its caption), the block centred in the band; gap is
    from the first line's baseline to the second's capitals."""
    span = top_fm.capHeight() + gap + low_fm.capHeight()
    first = round(top + (height - span) / 2 + top_fm.capHeight())
    return first, round(first + gap + low_fm.capHeight())


class StatusPill(QFrame):
    """A live reading at a glance: icon, value and a short note, tinted by how it's doing (theme.STATUS_COLORS).
    Painted, so the value and note share one baseline and the icon centres on it."""

    def __init__(self, icon_name: str, parent=None):
        super().__init__(parent)
        self.setObjectName("status_pill")
        self.setFixedHeight(ui_px(PILL_HEIGHT))
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        self.setMinimumWidth(1)
        self.kind = ""
        self._value = self._note = self._icon_name = ""
        self._ink = QColor(theme.TEXT_DIM)
        self.set("—", "", "status_dim", icon_name)

    def set(self, value: str, note: str, kind: str, icon_name: str):
        """kind is one of theme.STATUS_COLORS ("status_ok", "status_warn", "status_err", "status_dim")."""
        self.kind = kind
        color = QColor(theme.STATUS_COLORS.get(kind, theme.DIM))
        self._ink = QColor(color.lighter(112).name() if kind != "status_dim" else theme.TEXT_DIM)
        self._fill, self._edge = QColor(color), QColor(color)
        self._fill.setAlphaF(0.12 if kind != "status_dim" else 0.06)
        self._edge.setAlphaF(0.25 if kind != "status_dim" else 0.15)
        self._value, self._note, self._icon_name = value, note, icon_name
        self.setAccessibleName(" ".join(t for t in (value, note) if t))
        self.update()

    def value(self) -> str:
        return self._value

    def note(self) -> str:
        return self._note

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        r = QRectF(self.rect()).adjusted(0.5, 0.5, -0.5, -0.5)
        p.setPen(QPen(self._edge, 1))
        p.setBrush(self._fill)
        p.drawRoundedRect(r, r.height() / 2, r.height() / 2)
        big = QFont(self.font())
        big.setPointSizeF(self.font().pointSizeF() * 1.13)
        big.setWeight(QFont.Weight.DemiBold)
        fm_big = QFontMetrics(big)
        base = cap_baseline(fm_big, 0, self.height())
        x = 14
        icon_px = 18
        mid = base - fm_big.capHeight() / 2
        made = line_icon if self._icon_name in LINE_ICONS else create_vector_icon
        made(self._icon_name, self._ink.name()).paint(p, QRect(x, round(mid - icon_px / 2), icon_px, icon_px))
        x += icon_px + 9
        p.setPen(self._ink)
        p.setFont(big)
        room = self.width() - x - 14
        p.drawText(QPointF(x, base), fm_big.elidedText(self._value, Qt.TextElideMode.ElideRight, room))
        x += fm_big.horizontalAdvance(self._value) + 9
        if self._note and x < self.width() - 14:
            p.setFont(self.font())
            p.drawText(QPointF(x, base), p.fontMetrics().elidedText(self._note, Qt.TextElideMode.ElideRight,
                                                                     self.width() - 14 - x))
        p.end()


def segmented_row(*buttons: QWidget, fill: bool = True) -> QHBoxLayout:
    """Segments in one track, all the same width: equal shares of the row when fill, else SEGMENT_WIDTH each. The
    picked one is a filled pill inside the track."""
    make_segmented(*buttons)
    outer = QHBoxLayout()
    outer.setContentsMargins(0, 0, 0, 0)
    track = QFrame()
    track.setObjectName("seg_track")
    track.setSizePolicy(QSizePolicy.Policy.Expanding if fill else QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
    outer.addWidget(track, 1 if fill else 0)
    lay = QHBoxLayout(track)
    lay.setContentsMargins(2, 2, 2, 2)
    lay.setSpacing(2)
    for btn in buttons:
        if fill:
            if type(btn) is not SegmentButton:
                btn.setMinimumWidth(1)  # these size by the row; a plain segment keeps room for its bold text
            btn.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
            lay.addWidget(btn, 1)
        else:
            btn.setFixedWidth(ui_px(SEGMENT_WIDTH))
            lay.addWidget(btn)
    return outer


def create_card(parent=None) -> QFrame:
    card = QFrame(parent)
    card.setObjectName("card")
    card.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Preferred)
    return card


DIMMED_OPACITY = 0.4
"""How visible a card is while it waits for a phone; disabled styling alone leaves titles and readouts bright."""


def dim_until_paired(card: QWidget, bus, phone_only: bool = False):
    """Fade a card out and disable it until a phone is paired, since its controls do nothing without one. Picking a
    stream source other than a phone (Browser camera) counts as paired, or hides the card if it's phone_only."""
    state = {"phones": 0, "source": ""}

    def apply():
        on = state["phones"] > 0 or bool(state["source"])
        card.setEnabled(on)
        if on:
            card.setGraphicsEffect(None)  # an effect re-renders the whole card offscreen; only keep it while static
        elif card.graphicsEffect() is None:
            effect = QGraphicsOpacityEffect(card)
            effect.setOpacity(DIMMED_OPACITY)
            card.setGraphicsEffect(effect)
        if phone_only and card.parentWidget() is not None and card.isHidden() != bool(state["source"]):
            card.setVisible(not state["source"])

    def on_phones(count: int):
        state["phones"] = count
        apply()

    def on_source(sid: str):
        state["source"] = sid
        apply()
    apply()
    bus.phones_changed.connect(on_phones)
    bus.source_selected.connect(on_source)


def card_layout(card: QFrame) -> QVBoxLayout:
    """The standard padded column every main-window card uses."""
    lay = QVBoxLayout(card)
    lay.setContentsMargins(16, 14, 16, 16)
    lay.setSpacing(8)
    return lay


ICON_CHIP = 30
"""Size of the tinted square behind a card's header icon."""

_hued: "weakref.WeakKeyDictionary[QWidget, tuple]" = weakref.WeakKeyDictionary()  # card -> (hue name, chip, icon name)


def _paint_hue(card: QWidget, hue_name: str, chip: QLabel = None, icon_name: str = ""):
    """Colour a card's header icon, sliders and readouts in its hue."""
    color = theme.hue(hue_name)
    if chip is not None:
        tint = QColor(color)
        tint.setAlphaF(0.14)
        chip.setStyleSheet(f"background: {tint.name(QColor.NameFormat.HexArgb)}; border-radius: 9px;")
        chip.setPixmap(_header_icon(icon_name, color).pixmap(16, 16))
    hover = QColor(color).lighter(115).name()
    picked = QColor(color)
    picked.setAlphaF(0.12)
    picked_hex = picked.name(QColor.NameFormat.HexArgb)
    card.setStyleSheet(
        f"QSlider::handle:horizontal {{ background: {color}; }}"
        f"QSlider::handle:horizontal:hover {{ background: {hover}; }}"
        f"QLabel#val {{ color: {color}; }}"
        f"QPushButton#lens_button:checked, QPushButton#option_card:checked {{"
        f" background-color: {picked_hex}; border-color: {color}; }}")


def _header_icon(name: str, color: str) -> QIcon:
    return line_icon(name, color) if name in LINE_ICONS else create_vector_icon(name, color)


def card_hue(widget: QWidget) -> str:
    """The hue of the card a widget sits in, ACCENT outside one."""
    while widget is not None:
        entry = _hued.get(widget)
        if entry is not None:
            return theme.hue(entry[0])
        widget = widget.parentWidget()
    return theme.ACCENT


_hued_icons: "weakref.WeakKeyDictionary[QLabel, tuple]" = weakref.WeakKeyDictionary()  # label -> (icon name, hue name, px)


def hued_icon(icon_name: str, hue_name: str, px: int = 16) -> QLabel:
    """A small icon in a card's hue, repainted by refresh_hues()."""
    label = QLabel()
    label.setFixedSize(px, px)
    label.setPixmap(create_vector_icon(icon_name, theme.hue(hue_name)).pixmap(px, px))
    _hued_icons[label] = (icon_name, hue_name, px)
    return label


def refresh_hues():
    """Repaint every card after theme.set_plugin_hues() changed."""
    for label, (icon_name, hue_name, px) in list(_hued_icons.items()):
        if sip.isdeleted(label):
            _hued_icons.pop(label, None)
            continue
        label.setPixmap(create_vector_icon(icon_name, theme.hue(hue_name)).pixmap(px, px))
    for card, (hue_name, chip, icon_name) in list(_hued.items()):
        if sip.isdeleted(card) or (chip is not None and sip.isdeleted(chip)):
            _hued.pop(card, None)  # Qt deleted it, though Python still holds the wrapper
            continue
        _paint_hue(card, hue_name, chip, icon_name)


def add_card_header(layout: QVBoxLayout, title: str, icon_name: str,
                    subtitle: str = "", action: QWidget = None, hue: str = "") -> QHBoxLayout:
    """Add the standard icon/title header used by every main-window card; action is the card's one button, top right.
    hue names the card's colour in theme.PLUGIN_HUES (none: ACCENT)."""
    header = QHBoxLayout()
    header.setContentsMargins(0, 0, 0, 2)
    header.setSpacing(10)

    chip = None
    if icon_name:
        chip = QLabel()
        chip.setFixedSize(ICON_CHIP, ICON_CHIP)
        chip.setAlignment(Qt.AlignmentFlag.AlignCenter)
        header.addWidget(chip)
    card = layout.parentWidget()
    if card is not None:
        _hued[card] = (hue, chip, icon_name)
        _paint_hue(card, hue, chip, icon_name)
    elif chip is not None:
        chip.setPixmap(_header_icon(icon_name, theme.hue(hue)).pixmap(16, 16))

    title_label = QLabel(title)
    title_label.setObjectName("card_title")
    # Fixed height whether or not the card has a header action, so titles line up across cards.
    title_label.setFixedHeight(30)
    header.addWidget(title_label)

    if subtitle:
        subtitle_label = QLabel(subtitle)
        subtitle_label.setObjectName("card_subtitle")
        header.addWidget(subtitle_label)

    header.addStretch()
    if action is not None:
        header.addWidget(action)
    layout.addLayout(header)
    return header


def action_button(text: str, role: str = "", tooltip: str = "") -> QPushButton:
    """A lone action button at the standard width; role is "", "primary" or "danger"."""
    btn = QPushButton(text)
    btn.setFixedWidth(ui_px(BUTTON_WIDTH))
    if role:
        set_ui_role(btn, role)
    if tooltip:
        btn.setToolTip(tooltip)
    return btn


def button_row(*buttons: QPushButton) -> QHBoxLayout:
    """Buttons that share a row split it into equal widths."""
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(8)
    for btn in buttons:
        btn.setMinimumWidth(1)
        btn.setMaximumWidth(16777215)
        btn.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        lay.addWidget(btn, 1)
    return lay


def host_window(host) -> "QWidget | None":
    """The main window, as the parent of a plugin's own window so it stays above it; None for a test's stand-in."""
    return host if isinstance(host, QWidget) else None


def icon_button(icon_name: str, name: str, tooltip: str = "") -> QPushButton:
    """A small square button with only a line icon; name is what a screen reader says."""
    btn = QPushButton()
    btn.setProperty("strip", True)
    btn.setIcon(line_icon(icon_name, theme.TEXT_DIM))
    btn.setIconSize(QSize(ui_px(15), ui_px(15)))
    btn.setFixedSize(ui_px(30), ui_px(30))
    btn.setAccessibleName(name)
    btn.setToolTip(tooltip or name)
    return btn


def dialog_layout(dialog: QWidget) -> QVBoxLayout:
    lay = QVBoxLayout(dialog)
    lay.setContentsMargins(20, 18, 20, 18)
    lay.setSpacing(14)
    return lay


def dialog_header(layout: QVBoxLayout, title: str, subtitle: str = "") -> QLabel:
    """Title plus one line of context; returns the subtitle label so callers can update it."""
    box = QVBoxLayout()
    box.setContentsMargins(0, 0, 0, 0)
    box.setSpacing(2)
    title_lbl = QLabel(title)
    title_lbl.setObjectName("dialog_title")
    box.addWidget(title_lbl)
    sub = WrapLabel(subtitle)
    sub.setObjectName("dialog_subtitle")
    sub.setVisible(bool(subtitle))
    box.addWidget(sub)
    layout.addLayout(box)
    return sub


def dialog_buttons(layout: QVBoxLayout, *buttons: QPushButton) -> QHBoxLayout:
    """Bottom-right button bar; pass the primary last so it sits at the corner."""
    bar = QHBoxLayout()
    bar.setContentsMargins(0, 4, 0, 0)
    bar.setSpacing(8)
    bar.addStretch(1)
    for btn in buttons:
        btn.setFixedWidth(ui_px(DIALOG_BUTTON_WIDTH))
        bar.addWidget(btn)
    layout.addLayout(bar)
    return bar


class WrapLabel(QLabel):
    """Word-wrapped label that reserves the height its current width needs.

    Plain wrapped QLabels report a one-line minimum to box layouts, so dialogs sized before the text
    wrapped clipped it. Re-pinning the minimum on every resize makes the layout (and the window) grow.
    """

    def __init__(self, text: str = "", parent=None):
        super().__init__(text, parent)
        self.setWordWrap(True)

    def _fit(self):
        # Measured from the text itself: heightForWidth() never reports less than the pinned minimum,
        # so a label pinned while narrow would keep that height after widening.
        width = self.contentsRect().width()
        if width <= 1:
            return
        m = self.contentsMargins()
        need = self.fontMetrics().boundingRect(
            0, 0, width, 100_000, int(Qt.TextFlag.TextWordWrap | self.alignment().value), self.text(),
        ).height() + m.top() + m.bottom()
        if need != self.minimumHeight():
            self.setMinimumHeight(need)

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._fit()

    def setText(self, text: str):
        super().setText(text)
        self._fit()


def wrapped_note(text: str, kind: str = "dim") -> QLabel:
    """Explanatory text that sits in a card's control column."""
    lbl = WrapLabel(text)
    lbl.setObjectName(kind)
    return lbl


def set_button_icon(btn: QPushButton, icon: QIcon, px: int = 14):
    """Put an icon on a button. Qt keeps it 4px from the words, which reads cramped, so the pixmap carries 2px more
    of empty room on its right."""
    gap = 2 if btn.text() else 0
    padded = QIcon()
    for mode in (QIcon.Mode.Normal, QIcon.Mode.Disabled):
        pm = QPixmap((px + gap) * 2, px * 2)
        pm.fill(Qt.GlobalColor.transparent)
        pm.setDevicePixelRatio(2)
        p = QPainter(pm)
        p.drawPixmap(QRect(0, 0, px, px), icon.pixmap(QSize(px, px), 2.0, mode))
        p.end()
        padded.addPixmap(pm, mode)
    btn.setIcon(padded)
    btn.setIconSize(QSize(px + gap, px))


def card_action(text: str, icon_name: str, tooltip: str = "") -> QPushButton:
    """The small quiet button a card header carries."""
    btn = QPushButton(text)
    btn.setObjectName("card_action")
    made = line_icon if icon_name in LINE_ICONS else create_vector_icon
    set_button_icon(btn, made(icon_name, theme.TEXT_DIM), 14)
    if tooltip:
        btn.setToolTip(tooltip)
    return btn


def value_label(text: str = "") -> QLabel:
    """Fixed-width, right-aligned readout that sits after a slider."""
    lbl = QLabel(text)
    lbl.setObjectName("val")
    lbl.setFixedWidth(ui_px(VALUE_COL_WIDTH))
    lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
    return lbl


def slider_row(slider: QWidget, readout: QLabel, gutter: bool = False) -> QHBoxLayout:
    """[slider][value column], plus the spinbox column's width when the card has direct-entry rows."""
    stretch_slider(slider)
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(8)
    lay.addWidget(slider, 1)
    lay.addWidget(readout)
    if gutter:
        lay.addSpacing(ui_px(SPIN_COL_GUTTER))
    return lay


def add_section_heading(layout: QVBoxLayout, text: str, icon_name: str = ""):
    """A quiet heading between groups of rows: a small icon, the name and a hairline running to the card's edge."""
    row = QWidget()
    row.setObjectName("section_head")
    lay = QHBoxLayout(row)
    lay.setContentsMargins(0, 4, 0, 0)
    lay.setSpacing(8)
    if icon_name:
        icon = QLabel()
        icon.setFixedSize(13, 13)
        made = line_icon if icon_name in LINE_ICONS else create_vector_icon
        icon.setPixmap(made(icon_name, theme.TEXT_FAINT).pixmap(13, 13))
        lay.addWidget(icon)
    heading = QLabel(text)
    heading.setIndent(0)  # QSS margins switch on QLabel's auto-indent, which pushed headings ~4px right of the row labels
    heading.setObjectName("section_title")
    lay.addWidget(heading)
    rule = QFrame()
    rule.setObjectName("section_rule")
    rule.setFixedHeight(1)
    lay.addWidget(rule, 1)
    layout.addWidget(row)
    return heading


def form_label(text: str, width: int = FORM_LABEL_WIDTH) -> QLabel:
    label = QLabel(text)
    label.setObjectName("form_label")
    label.setFixedWidth(ui_px(width))
    label.setMinimumHeight(ui_px(ROW_HEIGHT))  # every row, text-only or not, keeps the same rhythm
    label.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter)
    return label


def name_controls(label: str, item):
    """Give a row's unnamed controls its label as their screen-reader name; segments get "Label: Their text"."""
    if isinstance(item, QLayout):
        for i in range(item.count()):
            child = item.itemAt(i)
            name_controls(label, child.widget() or child.layout())
    elif isinstance(item, SegmentButton):
        if not item.accessibleName():
            item.setAccessibleName(f"{label}: {item.text()}")
    elif isinstance(item, (QAbstractSlider, QAbstractSpinBox, QComboBox, QAbstractButton)):
        if not item.accessibleName() and not (isinstance(item, QAbstractButton) and item.text().strip()):
            item.setAccessibleName(label)
    elif isinstance(item, QWidget) and item.layout() is not None:
        name_controls(label, item.layout())


def control_row(label: str, widget, label_width: int = FORM_LABEL_WIDTH,
                stretch: bool = False) -> QHBoxLayout:
    """Settings row: dim label, then the control starting at a shared column so label and value sit side by side."""
    stretch = stretch or isinstance(widget, Switch)  # a switch sits at the row's end
    if label:
        name_controls(label, widget)
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(10)
    # Top-anchored: when the control wraps (lens pills) the label lines up with its first line, not the block's middle.
    lay.addWidget(form_label(label, label_width), 0, Qt.AlignmentFlag.AlignTop)
    if isinstance(widget, QLayout): lay.addLayout(widget, 1 if stretch else 0)
    else:                           lay.addWidget(widget, 1 if stretch else 0)
    if not stretch:
        lay.addStretch(1)
    return lay


def control_row_widget(label: str, widget, label_width: int = FORM_LABEL_WIDTH,
                       stretch: bool = False) -> QWidget:
    """A hideable `control_row` - for rows revealed by a mode toggle."""
    container = QWidget()
    container.setObjectName("form_row")
    container.setLayout(control_row(label, widget, label_width, stretch))
    return container


def run_off_ui_thread(fn, *args, **kwargs):
    """Run a blocking call (adb, mostly) on a worker thread and return its result, repainting meanwhile.

    User input is held back until it returns, so nothing can re-enter the caller mid-call. Off the
    GUI thread, or with no QApplication, it just calls fn.
    """
    app = QCoreApplication.instance()
    if app is None or QThread.currentThread() is not app.thread():
        return fn(*args, **kwargs)
    loop = QEventLoop()
    done = threading.Event()
    outcome = {}

    def work():
        try:
            outcome["value"] = fn(*args, **kwargs)
        except BaseException as exc:
            outcome["error"] = exc
        finally:
            done.set()
            # Queued: if it lands before exec() starts it's still delivered by exec(), so no lost wakeup.
            QMetaObject.invokeMethod(loop, "quit", Qt.ConnectionType.QueuedConnection)

    threading.Thread(target=work, daemon=True).start()
    if not done.is_set():
        loop.exec(QEventLoop.ProcessEventsFlag.ExcludeUserInputEvents)
    if "error" in outcome:
        raise outcome["error"]
    return outcome.get("value")


# ── Pure display helpers ──────────────────────────────────────────────────────


def ns_to_display(ns: int) -> str:
    if ns <= 0: return "?"
    s = ns / 1_000_000_000.0
    if s >= 1.0:
        return f"{s:.1f} s"
    denom = round(1.0 / s)
    return f"1/{denom:,}"


def quality_label(q: int) -> str:
    if q >= 91: return f"{q}%: High"
    if q >= 80: return f"{q}%: Balanced"
    if q >= 60: return f"{q}%: Low"
    return f"{q}%: Very low"


# ── Log-scale math ────────────────────────────────────────────────────────────

def log_pos_to_val(pos: int, steps: int, v_min: float, v_max: float) -> float:
    if v_min <= 0: v_min = 1
    if v_max <= v_min: return v_min  # a range of one value (or none), as some cameras report
    t = pos / max(steps, 1)
    val = math.exp(math.log(v_min) + t * (math.log(v_max) - math.log(v_min)))
    return max(v_min, min(v_max, val))


def val_to_log_pos(val: float, steps: int, v_min: float, v_max: float) -> int:
    if val <= 0 or v_min <= 0 or v_max <= v_min: return 0
    val = max(v_min, min(v_max, val))
    t   = (math.log(val) - math.log(v_min)) / (math.log(v_max) - math.log(v_min))
    return round(t * steps)


# ── No-scroll subclasses ──────────────────────────────────────────────────────

def fit_on_screen(rect: QRect, avail: QRect, margin: int) -> QRect:
    """`rect` moved (not shrunk) to sit inside `avail` less `margin`; shrunk only if it can't fit at all."""
    top, bottom = avail.y() + margin, avail.y() + avail.height() - margin
    h = min(rect.height(), bottom - top)
    y = min(max(rect.y(), top), bottom - h)
    return QRect(rect.x(), y, rect.width(), h)


class NoScrollComboBox(QComboBox):
    def wheelEvent(self, event):
        event.ignore()

    def paintEvent(self, event):
        super().paintEvent(event)
        p = QPainter(self)  # the chevron, drawn here so every dropdown has the same one
        color = theme.TEXT_DIM if self.isEnabled() else theme.TEXT_DISABLED
        line_icon("chevron", color).paint(p, QRect(self.width() - 22, (self.height() - 12) // 2, 12, 12))
        p.end()

    def showPopup(self):
        # Qt sizes/positions the popup before it can know it overshoots the screen - it lines the current
        # item up with the combo, so picking a lower item near the top edge pushes the list above it.
        # Slide it back on screen, and only shrink it when it's taller than the screen itself.
        super().showPopup()
        popup = self.view().window()
        screen = self.screen()
        if popup is None or screen is None:
            return
        g = fit_on_screen(popup.geometry(), screen.availableGeometry(), 8)
        if g != popup.geometry():
            popup.setGeometry(g)
            self.view().scrollTo(self.view().currentIndex())


class NoScrollSlider(QSlider):
    """A slider the scroll wheel leaves alone, with optional snap points and a default.

    Snap points (set_snaps) are drawn as dots on the groove, and dragging the handle sticks to one while
    the mouse is within SNAP_PX of it; moving further away lets go. Only a drag snaps: the arrow keys and
    clicks on the groove step freely. A double-click puts the slider back on its default (set_default).
    """

    _MARK = 5  # dot diameter, px; a bit wider than the 4px groove so it shows on the filled part too
    SNAP_PX = 5  # how close (design px) a dragged handle has to come to a snap point to stick to it
    RESET_HINT = "Double-click to reset"

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._marks: list = []
        self._default = None
        self._tip = ""
        self.actionTriggered.connect(self._snap)

    def wheelEvent(self, event):
        event.ignore()

    def set_snaps(self, values):
        self._marks = sorted(values)
        self.update()

    def snaps(self) -> list:
        return list(self._marks)

    def set_default(self, value):
        self._default = value
        self.setToolTip(self._tip)

    def default(self):
        """Where a double-click puts it back, or None."""
        return self._default

    def setToolTip(self, text: str):
        self._tip = text
        hint = self.RESET_HINT if self._default is not None else ""
        super().setToolTip("\n".join(t for t in (text, hint) if t))

    def mouseDoubleClickEvent(self, event):
        if self._default is None or event.button() != Qt.MouseButton.LeftButton:
            super().mouseDoubleClickEvent(event)
            return
        self.setValue(self._default)

    def _snap(self, action: int):
        # Runs after a drag moved sliderPosition but before the value follows, so listeners only ever see
        # the snapped value.
        if action != QSlider.SliderAction.SliderMove.value or not self._marks:
            return
        pos = self.sliderPosition()
        x = self.mark_x(pos)
        near = min(self._marks, key=lambda m: abs(self.mark_x(m) - x))
        # Close neighbours each get at most a third of the gap, so there's always room between them.
        gaps = [abs(self.mark_x(m) - self.mark_x(near)) for m in self._marks if m != near]
        reach = min([ui_px(self.SNAP_PX)] + [g / 3 for g in gaps])
        if near != pos and abs(self.mark_x(near) - x) <= reach:
            self.setSliderPosition(near)

    def mark_x(self, value: int) -> float:
        """Where the handle's centre sits at `value`, in widget px (asked of the style, margins and all)."""
        def centre(pos):
            opt = QStyleOptionSlider()
            self.initStyleOption(opt)
            opt.sliderPosition = opt.sliderValue = pos
            return QRectF(self.style().subControlRect(QStyle.ComplexControl.CC_Slider, opt,
                                                      QStyle.SubControl.SC_SliderHandle, self)).center().x()
        lo, hi = self.minimum(), self.maximum()
        if hi <= lo:
            return centre(lo)
        a, b = centre(lo), centre(hi)
        return a + (b - a) * (value - lo) / (hi - lo)

    def paintEvent(self, event):
        super().paintEvent(event)
        marks = [m for m in self._marks if self.minimum() < m < self.maximum()]
        if not marks:
            return
        opt = QStyleOptionSlider()
        self.initStyleOption(opt)
        handle = QRectF(self.style().subControlRect(QStyle.ComplexControl.CC_Slider, opt,
                                                    QStyle.SubControl.SC_SliderHandle, self))
        groove = QRectF(self.style().subControlRect(QStyle.ComplexControl.CC_Slider, opt,
                                                    QStyle.SubControl.SC_SliderGroove, self))
        # On top of the fill, but cut around the (round) handle so a mark never sits over it.
        clip = QPainterPath()
        clip.addRect(QRectF(self.rect()))
        knob = QPainterPath()
        knob.addEllipse(handle)
        p = QPainter(self)
        p.setClipPath(clip.subtracted(knob))
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        p.setPen(Qt.PenStyle.NoPen)
        d = ui_px(self._MARK)
        y = groove.center().y()
        on_groove = QColor(theme.TEXT_DIM if self.isEnabled() else theme.TEXT_DISABLED)
        on_fill = QColor(theme.TEXT if self.isEnabled() else theme.TEXT_DISABLED)  # lighter on the lavender fill
        for m in marks:
            p.setBrush(on_fill if m < self.value() else on_groove)
            p.drawEllipse(QRectF(self.mark_x(m) - d / 2, y - d / 2, d, d))


class NoScrollSpinBox(QSpinBox):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setButtonSymbols(QSpinBox.ButtonSymbols.NoButtons)

    def wheelEvent(self, event):
        event.ignore()


class NoScrollDoubleSpinBox(QDoubleSpinBox):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setButtonSymbols(QDoubleSpinBox.ButtonSymbols.NoButtons)

    def wheelEvent(self, event):
        event.ignore()


# ── Widget factory helpers ────────────────────────────────────────────────────

def create_separator() -> QFrame:
    sep = QFrame()
    sep.setFrameShape(QFrame.Shape.HLine)
    sep.setObjectName("separator")
    return sep


def create_app_icon(size: int = 32) -> QIcon:
    """Telescope mark: accent-blue disc with dark centre punched out; only app icon asset, used everywhere for consistency."""
    pixmap = QPixmap(size, size)
    pixmap.fill(Qt.GlobalColor.transparent)

    painter = QPainter(pixmap)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    painter.setPen(Qt.PenStyle.NoPen)

    margin = size / 22
    outer_d = size - 2 * margin
    painter.setBrush(QBrush(QColor(theme.ACCENT)))
    painter.drawEllipse(QRectF(margin, margin, outer_d, outer_d))

    inner_margin = size * 7 / 22
    inner_d = size * 8 / 22
    painter.setBrush(QBrush(QColor(theme.BG)))
    painter.drawEllipse(QRectF(inner_margin, inner_margin, inner_d, inner_d))

    painter.end()
    return QIcon(pixmap)


TRAY_SIZES = (16, 20, 22, 24, 32, 48, 64)  # tray icons are drawn per size, not scaled, so the badges stay crisp


def _tray_pixmap(size: int, camera: bool, mic) -> QPixmap:
    s = size
    pixmap = create_app_icon(s).pixmap(s, s)
    painter = QPainter(pixmap)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    painter.setPen(Qt.PenStyle.NoPen)
    gap = max(1.0, s / 16)
    if camera:
        # The red dot of a recording light, top right, cut clear of the ring so it reads on any panel
        d = s * 0.36
        centre = QPointF(s - d / 2 - gap / 2, d / 2 + gap / 2)
        painter.setCompositionMode(QPainter.CompositionMode.CompositionMode_Clear)
        painter.setBrush(Qt.GlobalColor.black)
        painter.drawEllipse(centre, d / 2 + gap, d / 2 + gap)
        painter.setCompositionMode(QPainter.CompositionMode.CompositionMode_SourceOver)
        painter.setBrush(QColor(theme.ERR))
        painter.drawEllipse(centre, d / 2, d / 2)
    if mic:
        # A dark mic sitting wholly on the ring, bottom right, so the panel never shows through it
        u = s * 0.44 / 12
        cx, cy = s * 0.73, s * 0.72
        dark = QColor(theme.BG)
        painter.setBrush(dark)
        painter.drawRoundedRect(QRectF(cx - 1.5 * u, cy - 4 * u, 3 * u, 5 * u), 1.5 * u, 1.5 * u)
        pen = QPen(dark, max(1.0, 1.1 * u))
        pen.setCapStyle(Qt.PenCapStyle.RoundCap)
        painter.setPen(pen)
        painter.setBrush(Qt.BrushStyle.NoBrush)
        painter.drawArc(QRectF(cx - 3 * u, cy - 3.5 * u, 6 * u, 6 * u), 180 * 16, 180 * 16)
        painter.drawLine(QPointF(cx, cy + 2.5 * u), QPointF(cx, cy + 4 * u))
        if mic == "muted":
            pen.setColor(QColor(theme.ERR))
            painter.setPen(pen)
            painter.drawLine(QPointF(cx - 3.5 * u, cy - 3.5 * u), QPointF(cx + 3.5 * u, cy + 3.5 * u))
    painter.end()
    return pixmap


def create_tray_icon(camera: bool = False, mic=None) -> QIcon:
    """The app mark with what's streaming on it: a red dot for the camera, a mic for the mic (mic="on" or "muted")."""
    icon = QIcon()
    for size in TRAY_SIZES:
        icon.addPixmap(_tray_pixmap(size, camera, mic))
    return icon


# Icon artwork: 24-unit grid, rounded 2.2 strokes, and a soft 25% fill on each
# icon's main shape so the set has some body at 16-20 px. "{c}" is the colour.
_SOFT = 'fill="{c}" fill-opacity="0.25"'
_ICON_SVG = {
    "connection": '''<path d="M10 14a4.2 4.2 0 0 0 6 0l3-3a4.2 4.2 0 0 0-6-6l-1.2 1.2"/>
        <path d="M14 10a4.2 4.2 0 0 0-6 0l-3 3a4.2 4.2 0 0 0 6 6l1.2-1.2"/>''',
    "usb": f'''<rect x="8" y="2.5" width="8" height="6" rx="1.5" {_SOFT}/>
        <path d="M6.5 8.5h11v4.5a5.5 5.5 0 0 1-11 0z"/><path d="M12 18.5v3"/>''',
    "camera": f'''<path d="M4.5 7.5h2.8l1.8-2.6h5.8l1.8 2.6h2.8a1.5 1.5 0 0 1 1.5 1.5v9a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18V9a1.5 1.5 0 0 1 1.5-1.5z" {_SOFT}/>
        <circle cx="12" cy="13.2" r="3.4"/>''',
    "camera_off": f'''<path d="M4.5 7.5h2.8l1.8-2.6h5.8l1.8 2.6h2.8a1.5 1.5 0 0 1 1.5 1.5v9a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18V9a1.5 1.5 0 0 1 1.5-1.5z" {_SOFT}/>
        <circle cx="12" cy="13.2" r="3.4"/><path d="M3.5 3.5l17 17"/>''',
    "stream": f'''<rect x="3" y="4" width="18" height="12.5" rx="2.2" {_SOFT}/>
        <path d="M8.5 20.5h7M12 16.5v4"/>''',
    "gear": f'''<path d="M4 7h9M19 7h1M4 17h3M13 17h7"/>
        <circle cx="16" cy="7" r="2.6" {_SOFT}/><circle cx="10" cy="17" r="2.6" {_SOFT}/>''',
    "status": f'''<rect x="3" y="3" width="18" height="18" rx="4.5" {_SOFT} stroke="none"/>
        <path d="M5.5 12.5h3l2-4.5 3 8.5 2-4h3"/>''',
    "qr": f'''<rect x="3.5" y="3.5" width="7" height="7" rx="1.6" {_SOFT}/>
        <rect x="13.5" y="3.5" width="7" height="7" rx="1.6" {_SOFT}/>
        <rect x="3.5" y="13.5" width="7" height="7" rx="1.6" {_SOFT}/>
        <path d="M14 14h2.5v2.5M20.5 14v.01M14 20.5h6.5v-3.5"/>''',
    "play": '''<path d="M8 5.8v12.4a1.2 1.2 0 0 0 1.8 1l9.6-6.2a1.2 1.2 0 0 0 0-2l-9.6-6.2A1.2 1.2 0 0 0 8 5.8z" fill="{c}"/>''',
    "stop": '''<rect x="6" y="6" width="12" height="12" rx="2.6" fill="{c}"/>''',
    "expand": f'''<rect x="3" y="7" width="14" height="14" rx="2.2" {_SOFT}/>
        <path d="M13.5 3h7.5v7.5M21 3l-8.5 8.5"/>''',
    "lenses": f'''<rect x="3" y="5" width="18" height="14" rx="2.2" {_SOFT}/>
        <rect x="8.5" y="9" width="7" height="6" rx="1.2"/>''',
    "reset": '''<path d="M3.5 12a8.5 8.5 0 1 0 2.6-6.1L3.5 8.5"/><path d="M3.5 3.5v5h5"/>''',
    "transforms": f'''<path d="M12 3v2.5M12 9.5v5M12 18.5V21"/>
        <path d="M9 6.5L3.5 17.5H9z" {_SOFT}/><path d="M15 6.5l5.5 11H15z"/>''',
    "check": f'''<circle cx="12" cy="12" r="8.8" {_SOFT}/><path d="M8 12.3l2.8 2.8 5.2-5.6"/>''',
    "help": f'''<circle cx="12" cy="12" r="8.8" {_SOFT}/><path d="M9.7 9.5a2.4 2.4 0 1 1 3.3 2.2c-.6.3-1 .8-1 1.4v.4"/>
        <path d="M12 16.6v.2"/>''',
    "update": f'''<circle cx="12" cy="12" r="8.8" {_SOFT}/><path d="M12 7.5v8.5M8.3 12.6l3.7 3.7 3.7-3.7"/>''',
    "devices": f'''<rect x="3.5" y="4" width="10" height="17" rx="2.2" {_SOFT}/>
        <path d="M7.5 17.5h2"/><path d="M16.5 7.5h2.5a1.5 1.5 0 0 1 1.5 1.5v9.5a1.5 1.5 0 0 1-1.5 1.5h-2.5"/>''',
    "alert": f'''<path d="M10.3 4.2a2 2 0 0 1 3.4 0l7.4 12.8a2 2 0 0 1-1.7 3H4.6a2 2 0 0 1-1.7-3z" {_SOFT}/>
        <path d="M12 9.5v4"/><path d="M12 16.8v.2"/>''',
    "preset": f'''<path d="M6.5 3.5h11a1 1 0 0 1 1 1v16l-6.5-4.2-6.5 4.2v-16a1 1 0 0 1 1-1z" {_SOFT}/>''',
    "mic": f'''<rect x="8.5" y="3" width="7" height="11.5" rx="3.5" {_SOFT}/>
        <path d="M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v3"/>''',
    "mic_off": f'''<rect x="8.5" y="3" width="7" height="11.5" rx="3.5" {_SOFT}/>
        <path d="M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v3M4 3.5l16 17"/>''',
    "close": '''<path d="M6.5 6.5l11 11M17.5 6.5l-11 11"/>''',
    "wifi": '''<path d="M3 9.5a13 13 0 0 1 18 0M6 13a8.5 8.5 0 0 1 12 0M9 16.5a4 4 0 0 1 6 0"/><path d="M12 20v.2"/>''',
    "battery": f'''<rect x="2.5" y="7" width="17" height="10" rx="2.5" {_SOFT}/><path d="M22 10.5v3"/>''',
    "battery_charging": f'''<rect x="2.5" y="7" width="17" height="10" rx="2.5" {_SOFT}/><path d="M22 10.5v3"/>
        <path d="M11.5 8.5l-2.5 3.5h4l-2.5 3.5"/>''',
    "thermometer": f'''<path d="M9.5 4.5a2.5 2.5 0 0 1 5 0v9a4.5 4.5 0 1 1-5 0z" {_SOFT}/><path d="M12 9v7"/>''',
    "bell": f'''<path d="M6 16.5v-5.5a6 6 0 0 1 12 0v5.5l1.5 1.5h-15z" {_SOFT}/><path d="M10 20.5h4"/>''',
    "plus": '''<path d="M12 5v14M5 12h14"/>''',
}

_ICON_RENDER_PX = 64  # rendered once, large; QIcon scales down smoothly for every use


def create_vector_icon(icon_name: str, color_hex: str) -> QIcon:
    body = _ICON_SVG.get(icon_name, "").replace("{c}", color_hex)  # unknown name: blank icon, not a crash
    svg = (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" '
        f'stroke="{color_hex}" stroke-width="2.2" stroke-linecap="round" '
        f'stroke-linejoin="round">{body}</svg>'
    )
    pixmap = QPixmap(_ICON_RENDER_PX, _ICON_RENDER_PX)
    pixmap.fill(Qt.GlobalColor.transparent)
    painter = QPainter(pixmap)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    QSvgRenderer(QByteArray(svg.encode())).render(painter)
    painter.end()
    return QIcon(pixmap)


# ── Log-scale slider row ──────────────────────────────────────────────────────

class LogSliderRow(QWidget):
    """Horizontal log-scale slider with spinbox; spinbox_scale multiplies display (e.g., 1e-6 shows nanoseconds as milliseconds)."""
    value_changed = pyqtSignal(float)
    STEPS = 2000

    def __init__(self, v_min: float, v_max: float,
                 display_fn=None, spinbox_suffix: str = "",
                 spinbox_scale: float = 1.0,
                 spinbox_decimals: int = 0, parent=None):
        super().__init__(parent)
        self.setObjectName("inline_control")
        self.v_min = v_min
        self.v_max = v_max
        self.display_fn = display_fn or str
        self._spin_scale = spinbox_scale


        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        self._slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(0, self.STEPS)
        self._slider.setValue(0)
        stretch_slider(self._slider)
        lay.addWidget(self._slider, 1)

        self._val_lbl = QLabel(display_fn(v_min) if display_fn else str(v_min))
        self._val_lbl.setObjectName("val")
        self._val_lbl.setFixedWidth(ui_px(VALUE_COL_WIDTH))
        self._val_lbl.setAlignment(
            Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        lay.addWidget(self._val_lbl)

        self._is_double_spin = spinbox_decimals > 0
        if self._is_double_spin:
            spin = NoScrollDoubleSpinBox()
            spin.setDecimals(spinbox_decimals)
            spin.setRange(v_min * spinbox_scale, v_max * spinbox_scale)
            spin.setSingleStep(10 ** -spinbox_decimals)
        else:
            spin = NoScrollSpinBox()
            spin.setRange(int(v_min * spinbox_scale), int(v_max * spinbox_scale))
        spin.setSuffix(spinbox_suffix)
        spin.setFixedWidth(ui_px(SPIN_COL_WIDTH))
        self._spin = spin
        lay.addWidget(self._spin)

        self._slider.valueChanged.connect(self._on_slider)
        self._spin.editingFinished.connect(self._on_spin)

    def _to_spin(self, val: float):
        sv = val * self._spin_scale
        return sv if self._is_double_spin else int(round(sv))

    def _on_slider(self, pos: int):
        val = log_pos_to_val(pos, self.STEPS, self.v_min, self.v_max)
        display_val = val if self._is_double_spin else round(val)
        self._val_lbl.setText(self.display_fn(display_val))
        self._spin.blockSignals(True)
        self._spin.setValue(self._to_spin(val))
        self._spin.blockSignals(False)
        self.value_changed.emit(val)

    def _on_spin(self):
        val = float(self._spin.value()) / self._spin_scale
        pos = val_to_log_pos(val, self.STEPS, self.v_min, self.v_max)
        self._slider.blockSignals(True)
        self._slider.setValue(pos)
        self._slider.blockSignals(False)
        display_val = val if self._is_double_spin else round(val)
        self._val_lbl.setText(self.display_fn(display_val))
        self.value_changed.emit(val)

    def set_range(self, v_min: float, v_max: float):
        cur = self.get_value()  # keeps the value, not the slider position, clamped to the new range
        self.v_min = v_min
        self.v_max = v_max
        lo, hi = self._to_spin(v_min), self._to_spin(v_max)
        self._spin.setRange(lo, hi)
        self.set_value(min(max(cur, v_min), max(v_min, v_max)))

    def get_value(self) -> float:
        return log_pos_to_val(self._slider.value(), self.STEPS, self.v_min, self.v_max)

    def set_value(self, val: float):
        pos = val_to_log_pos(val, self.STEPS, self.v_min, self.v_max)
        self._slider.blockSignals(True)
        self._slider.setValue(pos)
        self._slider.blockSignals(False)
        self._spin.blockSignals(True)
        self._spin.setValue(self._to_spin(val))
        self._spin.blockSignals(False)
        display_val = val if self._is_double_spin else round(val)
        self._val_lbl.setText(self.display_fn(display_val))

    def set_enabled(self, enabled: bool):
        self._slider.setEnabled(enabled)
        self._spin.setEnabled(enabled)

    def is_enabled(self) -> bool:
        return self._slider.isEnabled()

    def display_text(self) -> str:
        """What the row shows next to the slider ("ISO 400")."""
        return self._val_lbl.text()


# ── Pan slider row ────────────────────────────────────────────────────────────

class PanSliderRow(QWidget):
    """Linear slider -1.0 to 1.0 with a centre mark it snaps to (and resets to on a double-click)."""
    value_changed = pyqtSignal(float)
    STEPS = 200

    def __init__(self, label_neg: str = "L", label_pos: str = "R",
                 show_end_labels: bool = True, parent=None):
        super().__init__(parent)
        self.setObjectName("inline_control")
        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        if show_end_labels:
            neg_lbl = QLabel(label_neg)
            neg_lbl.setObjectName("dim")
            lay.addWidget(neg_lbl)

        self._slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(-self.STEPS, self.STEPS)
        self._slider.setValue(0)
        self._slider.set_snaps([0])
        self._slider.set_default(0)
        stretch_slider(self._slider)
        lay.addWidget(self._slider, 1)

        if show_end_labels:
            pos_lbl = QLabel(label_pos)
            pos_lbl.setObjectName("dim")
            lay.addWidget(pos_lbl)
        else:
            self.setMinimumWidth(ui_px(SLIDER_TRACK_WIDTH))

        self._slider.valueChanged.connect(self._on_slider)

    def _on_slider(self, pos: int):
        self.value_changed.emit(pos / self.STEPS)

    def get_value(self) -> float:
        return self._slider.value() / self.STEPS

    def set_value(self, val: float):
        self._slider.blockSignals(True)
        self._slider.setValue(int(val * self.STEPS))
        self._slider.blockSignals(False)

    def reset(self):
        self.set_value(0.0)

    def slider(self) -> NoScrollSlider:
        return self._slider

    def set_enabled(self, enabled: bool):
        self._slider.setEnabled(enabled)
