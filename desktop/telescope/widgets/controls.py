"""The card pieces of the redesign: line icons, value and dropdown tiles, switch tiles, option cards, tags, pills and
the pan pad. Sizes are design pixels (pass through ui_px), colours come from theme and the card's hue."""

import math

from PyQt6.QtCore import QEvent, QPointF, QRect, QRectF, QSize, Qt, pyqtSignal
from PyQt6.QtGui import QColor, QFont, QFontMetrics, QFontMetricsF, QGuiApplication, QPainter, QPen
from PyQt6.QtWidgets import QFrame, QHBoxLayout, QLabel, QSizePolicy, QVBoxLayout, QWidget

from telescope import theme
from telescope.widgets.common import (
    NoScrollComboBox, SegmentButton, Switch, cap_baseline, card_hue, paint_focus, stacked_baselines, ui_px,
)
from telescope.widgets.line_icons import line_icon
from telescope.i18n import tr

def icon_label(name: str, color: str, px: int = 16) -> QLabel:
    label = QLabel()
    label.setFixedSize(px, px)
    label.setPixmap(line_icon(name, color).pixmap(px, px))
    return label


def _font(base: QFont, scale: float = 1.0, weight: QFont.Weight = None) -> QFont:
    font = QFont(base)
    font.setPointSizeF(base.pointSizeF() * scale)
    if weight is not None:
        font.setWeight(weight)
    return font


def _alpha(color: str, a: float) -> QColor:
    c = QColor(color)
    c.setAlphaF(a)
    return c


def _rounded(p: QPainter, rect: QRectF, radius: float, fill: QColor, edge: QColor):
    p.setPen(QPen(edge, 1) if edge.alpha() else Qt.PenStyle.NoPen)
    p.setBrush(fill)
    p.drawRoundedRect(rect.adjusted(0.5, 0.5, -0.5, -0.5), radius, radius)


def _two_lines(p: QPainter, box: QRect, top_font: QFont, top_ink: QColor, top_text: str,
               low_font: QFont, low_ink: QColor, low_text: str, gap: float = 9) -> tuple:
    """Two lines of text read as one block, centred top to bottom in box by their letters; returns both baselines."""
    first, second = stacked_baselines(QFontMetrics(top_font), QFontMetrics(low_font), box.top(), box.height(), gap)
    for base, font, ink, text in ((first, top_font, top_ink, top_text), (second, low_font, low_ink, low_text)):
        if text:
            p.setFont(font)
            p.setPen(ink)
            p.drawText(QPointF(box.left(), base),
                       p.fontMetrics().elidedText(text, Qt.TextElideMode.ElideRight, box.width()))
    return first, second


# ── Tiles that are dropdowns ─────────────────────────────────────────────────

class ChoiceTile(NoScrollComboBox):
    """A dropdown drawn as a tile. "value" style puts the value big with the caption under it (Stream output's
    resolution); "field" style puts a small caption over the value, with a chevron (Camera's noise reduction)."""

    def __init__(self, caption: str, style: str = "value", parent=None):
        super().__init__(parent)
        self._caption = caption
        self.setAccessibleName(caption)
        self._style = style
        self.setObjectName("choice_tile" if style == "value" else "choice_field")
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.setFixedHeight(ui_px(54 if style == "value" else 48))
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        self.setMinimumWidth(1)

    def caption(self) -> str:
        return self._caption

    def wheelEvent(self, event):
        event.ignore()  # a tile in a scrolling column shouldn't change when the wheel passes over it

    def sizeHint(self) -> QSize:
        return QSize(ui_px(90), self.height())

    def minimumSizeHint(self) -> QSize:
        return QSize(1, self.height())

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        hover = self.underMouse() and self.isEnabled()
        _rounded(p, QRectF(self.rect()), 10, QColor(theme.SURFACE_HOVER if hover else theme.SURFACE_RAISE),
                 QColor(theme.BORDER_HOVER if hover else theme.BORDER))
        enabled = self.isEnabled()
        ink = QColor(theme.TEXT if enabled else theme.TEXT_DISABLED)
        faint = QColor(theme.TEXT_FAINT if enabled else theme.TEXT_DISABLED)
        pad = 12
        chevron_w = 18 if self._style == "field" else 0
        width = self.width() - 2 * pad - chevron_w
        value = self.currentText() or "—"
        if self._style == "value":
            top_font, top_ink, top_text = _font(self.font(), 1.15, QFont.Weight.DemiBold), ink, value
            low_font, low_ink, low_text = _font(self.font(), 0.88), faint, self._caption
        else:
            top_font, top_ink, top_text = _font(self.font(), 0.88), faint, self._caption
            low_font, low_ink, low_text = _font(self.font(), 1.0, QFont.Weight.DemiBold), ink, value
        _two_lines(p, QRect(pad, 0, width, self.height()), top_font, top_ink, top_text, low_font, low_ink, low_text)
        if self._style == "field":
            line_icon("chevron", theme.TEXT_DIM).paint(p, QRect(self.width() - pad - 14, (self.height() - 14) // 2, 14, 14))
        paint_focus(p, self, 10)
        p.end()


class ChipCombo(NoScrollComboBox):
    """A small dropdown for a value inside a sentence-like row ("Battery under [20% v]"): bold value, chevron."""

    def __init__(self, parent=None, width: int = 80):
        super().__init__(parent)
        self.setObjectName("chip_combo")
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.setFixedSize(ui_px(width), ui_px(30))

    def wheelEvent(self, event):
        event.ignore()

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        hover = self.underMouse()
        _rounded(p, QRectF(self.rect()), 8, QColor(theme.BG), QColor(theme.BORDER_HOVER if hover else theme.BORDER))
        p.setFont(_font(self.font(), 1.0, QFont.Weight.DemiBold))
        p.setPen(QColor(theme.TEXT))
        p.drawText(QPointF(10, cap_baseline(p.fontMetrics(), 0, self.height())), self.currentText())
        line_icon("chevron", theme.TEXT_DIM).paint(p, QRect(self.width() - 22, (self.height() - 12) // 2, 12, 12))
        paint_focus(p, self, 8)
        p.end()


# ── Option cards ──────────────────────────────────────────────────────────────

class OptionCard(SegmentButton):
    """One of a few choices shown side by side as cards: an icon and a name, with a short note under it. The picked one
    is outlined and tinted in its card's hue. text() stays the name, for the shortcuts menu."""

    def __init__(self, text: str, note: str, icon_name: str = "", parent=None):
        super().__init__(text, parent)
        self._note = note
        self.setAccessibleDescription(note)
        self._icon = icon_name
        self.setObjectName("option_card")
        self.setFixedHeight(ui_px(56))
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        self.setMinimumWidth(1)

    def note(self) -> str:
        return self._note

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        hue = card_hue(self)
        on, enabled = self.isChecked(), self.isEnabled()
        if on:
            _rounded(p, QRectF(self.rect()), 10, _alpha(hue, 0.12), QColor(hue))
        else:
            hover = self.underMouse() and enabled
            _rounded(p, QRectF(self.rect()), 10, QColor(theme.SURFACE_HOVER if hover else theme.SURFACE_RAISE),
                     QColor(theme.BORDER_HOVER if hover else theme.BORDER))
        ink = QColor(hue if on else theme.TEXT) if enabled else QColor(theme.TEXT_DISABLED)
        note = QColor(ink if on else QColor(theme.TEXT_FAINT))
        if on:
            note.setAlphaF(0.8)
        if not enabled:
            note = QColor(theme.TEXT_DISABLED)
        pad = 12
        name_font = _font(self.font(), 1.0, QFont.Weight.DemiBold)
        name_base, _ = _two_lines(p, QRect(pad, 0, self.width() - 2 * pad, self.height()), name_font, ink, "",
                                  _font(self.font(), 0.92), note, self._note)
        x = pad
        p.setFont(name_font)
        fm = p.fontMetrics()
        if self._icon:
            mid = name_base - fm.capHeight() / 2  # the icon centres on the name's capitals
            line_icon(self._icon, ink.name()).paint(p, QRect(x, round(mid - 7), 14, 14))
            x += 21
        p.setPen(ink)
        p.drawText(QPointF(x, name_base), fm.elidedText(self.text(), Qt.TextElideMode.ElideRight, self.width() - x - pad))
        paint_focus(p, self, 10)
        p.end()


def option_row(*cards: OptionCard) -> QHBoxLayout:
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(8)
    for card in cards:
        lay.addWidget(card, 1)
    return lay


# ── Switch tiles ──────────────────────────────────────────────────────────────

class SwitchTile(QFrame):
    """An on/off setting as a small tile: icon, name, switch. Two of them side by side fill a row."""

    def __init__(self, icon_name: str, text: str, parent=None):
        super().__init__(parent)
        self.setObjectName("switch_tile")
        self.setFixedHeight(ui_px(40))
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        self.setMinimumWidth(1)
        self._icon_name = icon_name
        lay = QHBoxLayout(self)
        lay.setContentsMargins(10, 0, 10, 0)
        lay.setSpacing(9)
        self._icon = QLabel()
        self._icon.setFixedSize(15, 15)
        lay.addWidget(self._icon)
        self.switch = Switch(text)
        self.switch.toggled.connect(lambda _on: self._paint_icon())
        lay.addWidget(self.switch, 1)
        self._paint_icon()

    def _paint_icon(self):
        color = card_hue(self) if self.switch.isChecked() and self.switch.isEnabled() else theme.TEXT_DIM
        self._icon.setPixmap(line_icon(self._icon_name, color).pixmap(15, 15))

    def showEvent(self, event):
        self._paint_icon()  # its card's hue is only known once it's in the card
        super().showEvent(event)

    def changeEvent(self, event):
        super().changeEvent(event)
        self._paint_icon()


# ── Tags and pills ────────────────────────────────────────────────────────────

class Tag(QWidget):
    """A small rounded label that marks something ("Experimental"): kind is warn or ok."""

    def __init__(self, text: str, kind: str = "warn", parent=None):
        super().__init__(parent)
        self._text = text
        self.setAccessibleName(text)
        self._color = QColor(theme.WARN if kind == "warn" else theme.OK)
        self.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)

    def text(self) -> str:
        return self._text

    def _tag_font(self) -> QFont:
        self.ensurePolished()  # the theme's font only arrives with the stylesheet, after __init__
        return _font(self.font(), 0.88, QFont.Weight.DemiBold)

    def sizeHint(self) -> QSize:
        return QSize(math.ceil(QFontMetricsF(self._tag_font()).horizontalAdvance(self._text)) + 16, ui_px(20))

    def minimumSizeHint(self) -> QSize:
        return self.sizeHint()

    def changeEvent(self, event):
        if event.type() == QEvent.Type.FontChange:
            self.updateGeometry()
        super().changeEvent(event)

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(_alpha(self._color.name(), 0.14))
        p.drawRoundedRect(QRectF(self.rect()), self.height() / 2, self.height() / 2)
        p.setFont(self._tag_font())
        p.setPen(self._color.lighter(108))
        left = (self.width() - QFontMetricsF(p.font()).horizontalAdvance(self._text)) / 2
        p.drawText(QPointF(left, cap_baseline(p.fontMetrics(), 0, self.height())), self._text)
        p.end()


# ── Pan pad ───────────────────────────────────────────────────────────────────

class PanPad(QWidget):
    """Where the view sits inside the zoomed-in frame: drag the dot, double-click to centre. Values run -1 to 1,
    right and down positive."""

    moved = pyqtSignal(float, float)

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setFixedHeight(ui_px(72))
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        self.setMinimumWidth(ui_px(96))
        self.setCursor(Qt.CursorShape.OpenHandCursor)
        self.setToolTip(tr("Drag to move the view. Double-click to centre it."))
        self.setFocusPolicy(Qt.FocusPolicy.StrongFocus)
        self.setAccessibleName(tr("Pan"))
        self._x = self._y = 0.0

    def set_position(self, x: float, y: float):
        self._x, self._y = max(-1.0, min(1.0, x)), max(-1.0, min(1.0, y))
        self.setAccessibleDescription(
            f"Horizontal {self._x:.0%}, vertical {self._y:.0%}. Arrow keys move the view; Home centres it.")
        self.update()

    def keyPressEvent(self, event):
        steps = {Qt.Key.Key_Left: (-1, 0), Qt.Key.Key_Right: (1, 0),
                 Qt.Key.Key_Up: (0, -1), Qt.Key.Key_Down: (0, 1)}
        if event.key() == Qt.Key.Key_Home:
            self.set_position(0, 0)
        elif event.key() in steps:
            dx, dy = steps[event.key()]
            step = 0.1 if event.modifiers() & Qt.KeyboardModifier.ShiftModifier else 0.01
            self.set_position(self._x + dx * step, self._y + dy * step)
        else:
            super().keyPressEvent(event)
            return
        self.moved.emit(self._x, self._y)
        event.accept()

    def position(self) -> tuple:
        return self._x, self._y

    def _inner(self) -> QRectF:
        return QRectF(self.rect()).adjusted(12, 12, -12, -12)

    def _from_point(self, pos: QPointF):
        inner = self._inner()
        x = (pos.x() - inner.center().x()) / (inner.width() / 2)
        y = (pos.y() - inner.center().y()) / (inner.height() / 2)
        self.set_position(x, y)
        self.moved.emit(self._x, self._y)

    def mousePressEvent(self, event):
        if self.isEnabled() and event.button() == Qt.MouseButton.LeftButton:
            self.setFocus(Qt.FocusReason.MouseFocusReason)
            self.setCursor(Qt.CursorShape.ClosedHandCursor)
            self._from_point(event.position())

    def mouseMoveEvent(self, event):
        if self.isEnabled() and event.buttons() & Qt.MouseButton.LeftButton:
            self._from_point(event.position())

    def mouseReleaseEvent(self, _event):
        self.setCursor(Qt.CursorShape.OpenHandCursor)

    def mouseDoubleClickEvent(self, _event):
        if self.isEnabled():
            self.set_position(0.0, 0.0)
            self.moved.emit(0.0, 0.0)

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        _rounded(p, QRectF(self.rect()), 9, QColor(theme.SURFACE_RAISE), QColor(theme.BORDER))
        inner = QRectF(self.rect()).adjusted(8, 8, -8, -8)
        p.setPen(QPen(QColor(theme.BORDER_STRONG), 1))
        c = QRectF(self.rect()).center()
        p.drawLine(QPointF(c.x(), inner.top()), QPointF(c.x(), inner.bottom()))
        p.drawLine(QPointF(inner.left(), c.y()), QPointF(inner.right(), c.y()))
        dot = self._inner()
        x = dot.center().x() + self._x * dot.width() / 2
        y = dot.center().y() + self._y * dot.height() / 2
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(QColor(card_hue(self) if self.isEnabled() else theme.TEXT_DISABLED))
        p.drawEllipse(QPointF(x, y), 7, 7)
        paint_focus(p, self, 9)
        p.end()


# ── Status line ───────────────────────────────────────────────────────────────

class RoundIcon(QLabel):
    """An icon in a round chip tinted with its card's hue (Connection's route)."""

    def __init__(self, size: int = 34, parent=None):
        super().__init__(parent)
        self._size = ui_px(size) | 1  # odd, so it has a centre pixel column for the icon's stroke
        self._name = ""
        self.setFixedSize(self._size, self._size)
        self.setAlignment(Qt.AlignmentFlag.AlignCenter)

    def set_icon(self, name: str):
        self._name = name
        self.update()

    def showEvent(self, event):
        self.update()
        super().showEvent(event)

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        hue = card_hue(self)
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(_alpha(hue, 0.14))
        p.drawEllipse(QRectF(self.rect()))
        if self._name:
            px = self._size // 2 | 1  # odd like the chip, so the icon's middle stroke lands on the chip's centre
            line_icon(self._name, hue).paint(p, QRect((self.width() - px) // 2, (self.height() - px) // 2, px, px))
        p.end()


def raised_block() -> tuple:
    """A raised block inside a card for things that belong together, and the column inside it."""
    block = QFrame()
    block.setObjectName("subsection")
    lay = QVBoxLayout(block)
    lay.setContentsMargins(12, 6, 12, 8)
    lay.setSpacing(4)
    return block, lay


def hairline() -> QFrame:
    line = QFrame()
    line.setObjectName("section_rule")
    line.setFixedHeight(1)
    return line



# ── Settings strips (Advanced) ────────────────────────────────────────────────

def setting_row(title: str, sub: str = "", control=None, below=None, keywords: str = "") -> tuple:
    """One setting as a raised strip: its name with a short line under it on the left and its control on the right,
    or a full-width control below (segments). keywords are extra words the settings search finds it by. Returns the
    strip and the line under the name."""
    from telescope.widgets.common import WrapLabel
    strip = QFrame()
    strip.setObjectName("setting_row")
    if keywords:
        strip.setProperty("keywords", keywords)
    outer = QVBoxLayout(strip)
    outer.setContentsMargins(12, 8, 12, 8)
    outer.setSpacing(8)
    top = QHBoxLayout()
    top.setSpacing(12)
    texts = QVBoxLayout()
    texts.setContentsMargins(0, 0, 0, 0)
    texts.setSpacing(1)
    texts.addStretch(1)
    name = QLabel(title)
    name.setObjectName("setting_name")
    texts.addWidget(name)
    line = WrapLabel(sub)
    line.setObjectName("setting_sub")
    line.setProperty("sub", True)
    if not sub:
        line.hide()  # never setVisible(True) before it has a parent: on Windows that flashes it up as a window
    texts.addWidget(line)
    texts.addStretch(1)
    top.addLayout(texts, 1)
    if isinstance(control, QWidget):
        if not control.accessibleName():
            control.setAccessibleName(title)
        top.addWidget(control, 0, Qt.AlignmentFlag.AlignVCenter)
    elif control is not None:
        top.addLayout(control)
    outer.addLayout(top)
    if isinstance(below, QWidget):
        outer.addWidget(below)
    elif below is not None:
        outer.addLayout(below)
    strip.setMinimumHeight(ui_px(46))
    from PyQt6.QtWidgets import QPushButton
    for btn in strip.findChildren(QPushButton):
        if btn.isCheckable() or not btn.text():
            continue  # segments keep their equal shares, and an icon button its square
        btn.setProperty("strip", True)
        btn.setMinimumWidth(0)
        btn.setMaximumWidth(16777215)
        btn.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)  # as wide as its words
    return strip, line


SCREEN_MARGIN = 48  # px a window keeps clear of the screen edges


class CommandBox(QWidget):
    """A command to run by hand, monospaced and selectable, with a button that copies it. Hidden while there's none."""

    def __init__(self):
        super().__init__()
        from telescope.widgets.common import WrapLabel, card_action
        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)
        self.label = WrapLabel("")
        self.label.setObjectName("command")
        theme.use_mono(self.label)
        self.label.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
        lay.addWidget(self.label, 1)
        self.copy_btn = card_action(tr("Copy"), "copy", tr("Copy the command, to paste in a terminal"))
        self.copy_btn.clicked.connect(lambda: QGuiApplication.clipboard().setText(self.label.text()))
        lay.addWidget(self.copy_btn, 0, Qt.AlignmentFlag.AlignTop)
        self.setVisible(False)

    def set_command(self, command: str):
        self.label.setText(command)
        self.setVisible(bool(command))
