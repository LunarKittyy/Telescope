"""A grading colour wheel: a puck on a ring of hues, dragged (or moved with the arrow keys) towards the colour to add."""
import math

import numpy as np
from PyQt6.QtCore import QPointF, QRectF, QSize, Qt, pyqtSignal
from PyQt6.QtGui import QBrush, QColor, QConicalGradient, QPainter, QPen, QRadialGradient
from PyQt6.QtWidgets import QSizePolicy, QWidget

from telescope import theme
from telescope.grade import wheel_shift
from telescope.i18n import N_, tr
from telescope.widgets.common import ui_px

_STEP = 0.01  # an arrow key moves the puck this far (of the radius)
_BIG_STEP = 0.05  # with Shift
_FINE = 0.25  # dragging with Shift held moves it this much slower


def wheel_colour(x: float, y: float, strength: float = 1.0) -> QColor:
    """The colour a puck at (x, y) adds, shown on mid grey (what the ring and the puck are painted with)."""
    rgb = np.clip(0.5 + wheel_shift(x, y) * 0.55 * strength, 0, 1)
    return QColor.fromRgbF(float(rgb[0]), float(rgb[1]), float(rgb[2]))


class ColourWheel(QWidget):
    """Drag anywhere to move the puck (it doesn't jump to the pointer), Shift for finer moves; arrows move it, Home or
    a double-click puts it back in the middle. The scroll wheel is left to the page, like the sliders."""

    moved = pyqtSignal(float, float)
    """The puck moved (x, y in the unit circle; y up), by the user or set_value(emit=True)."""

    HINT = N_("Drag towards a colour to add it. Double-click to reset.")

    def __init__(self, parent=None):
        super().__init__(parent)
        self._x = self._y = 0.0
        self._drag = None  # (pointer at the start, puck at the start)
        self.setFocusPolicy(Qt.FocusPolicy.StrongFocus)
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        self.setFixedHeight(ui_px(124))
        self.setToolTip(tr(self.HINT))
        self.setCursor(Qt.CursorShape.OpenHandCursor)

    def sizeHint(self) -> QSize:
        return QSize(ui_px(124), ui_px(124))

    def minimumSizeHint(self) -> QSize:
        return QSize(ui_px(88), ui_px(88))

    def value(self) -> tuple:
        return (self._x, self._y)

    def set_value(self, x: float, y: float, emit: bool = False):
        r = math.hypot(x, y)
        if r > 1:
            x, y = x / r, y / r
        x, y = round(x, 4), round(y, 4)
        if (x, y) == (self._x, self._y):
            return
        self._x, self._y = x, y
        self.update()
        if emit:
            self.moved.emit(x, y)

    # ── Geometry ──────────────────────────────────────────────────────────

    def _disc(self) -> tuple:
        """(centre, radius of the area the puck moves in)."""
        side = min(self.width(), self.height())
        ring = ui_px(7)
        return QPointF(self.width() / 2, self.height() / 2), side / 2 - ring - ui_px(4)

    # ── Input ─────────────────────────────────────────────────────────────

    def mousePressEvent(self, event):
        if event.button() != Qt.MouseButton.LeftButton:
            return super().mousePressEvent(event)
        self._drag = (event.position(), (self._x, self._y))
        self.setCursor(Qt.CursorShape.ClosedHandCursor)

    def mouseMoveEvent(self, event):
        if self._drag is None:
            return
        start, (x0, y0) = self._drag
        _centre, radius = self._disc()
        delta = event.position() - start
        speed = _FINE if event.modifiers() & Qt.KeyboardModifier.ShiftModifier else 1.0
        self.set_value(x0 + delta.x() / radius * speed, y0 - delta.y() / radius * speed, emit=True)

    def mouseReleaseEvent(self, event):
        self._drag = None
        self.setCursor(Qt.CursorShape.OpenHandCursor)

    def mouseDoubleClickEvent(self, event):
        if event.button() == Qt.MouseButton.LeftButton:
            self._drag = None
            self.set_value(0.0, 0.0, emit=True)

    def wheelEvent(self, event):
        event.ignore()

    def keyPressEvent(self, event):
        key = event.key()
        step = _BIG_STEP if event.modifiers() & Qt.KeyboardModifier.ShiftModifier else _STEP
        moves = {Qt.Key.Key_Left: (-step, 0), Qt.Key.Key_Right: (step, 0), Qt.Key.Key_Up: (0, step),
                 Qt.Key.Key_Down: (0, -step)}
        if key in moves:
            dx, dy = moves[key]
            self.set_value(self._x + dx, self._y + dy, emit=True)
        elif key in (Qt.Key.Key_Home, Qt.Key.Key_Delete, Qt.Key.Key_Backspace):
            self.set_value(0.0, 0.0, emit=True)
        else:
            super().keyPressEvent(event)

    # ── Painting ──────────────────────────────────────────────────────────

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        centre, radius = self._disc()
        enabled = self.isEnabled()
        ring_w = ui_px(5)
        ring_r = radius + ui_px(4) + ring_w / 2

        # The ring of hues, each where a puck pointing at it would push the picture
        hues = QConicalGradient(centre, 0)
        for i in range(37):
            angle = math.radians(i * 10)
            colour = wheel_colour(math.cos(angle), math.sin(angle), 1.6 if enabled else 0.4)
            hues.setColorAt(i / 36, colour)
        p.setPen(QPen(QBrush(hues), ring_w))
        p.setBrush(Qt.BrushStyle.NoBrush)
        p.drawEllipse(centre, ring_r, ring_r)

        well = QRadialGradient(centre, radius)
        well.setColorAt(0, QColor(theme.SURFACE_HOVER))
        well.setColorAt(1, QColor(theme.SURFACE_RAISE))
        p.setPen(QPen(QColor(theme.BORDER_STRONG), 1))
        p.setBrush(QBrush(well))
        p.drawEllipse(centre, radius, radius)

        p.setPen(QPen(QColor(theme.BORDER_STRONG), 1))
        p.drawLine(QPointF(centre.x() - radius, centre.y()), QPointF(centre.x() + radius, centre.y()))
        p.drawLine(QPointF(centre.x(), centre.y() - radius), QPointF(centre.x(), centre.y() + radius))
        for r in (0.33, 0.66):
            p.setBrush(Qt.BrushStyle.NoBrush)
            p.drawEllipse(centre, radius * r, radius * r)

        puck = QPointF(centre.x() + self._x * radius, centre.y() - self._y * radius)
        if (self._x, self._y) != (0.0, 0.0):
            p.setPen(QPen(QColor(theme.TEXT_DIM), 1.2))
            p.drawLine(centre, puck)
        size = ui_px(7)
        p.setPen(QPen(QColor(theme.TEXT if enabled else theme.TEXT_DISABLED), 2))
        p.setBrush(wheel_colour(self._x, self._y, 1.6) if enabled else QColor(theme.SURFACE_RAISE))
        p.drawEllipse(puck, size, size)

        if theme.keyboard_focus(self):
            ring = ring_r + ring_w / 2 + ui_px(2)
            theme.focus_ring(p, QRectF(centre.x() - ring, centre.y() - ring, 2 * ring, 2 * ring), ring)
        p.end()
