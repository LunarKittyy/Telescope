"""A grading curve: points dragged on a square, joined by the same monotone curve telescope.grade applies."""
import numpy as np
from PyQt6.QtCore import QPointF, QRectF, QSize, Qt, pyqtSignal
from PyQt6.QtGui import QColor, QLinearGradient, QPainter, QPainterPath, QPen
from PyQt6.QtWidgets import QApplication, QSizePolicy, QWidget

from telescope import theme
from telescope.grade import monotone_curve, periodic_curve
from telescope.i18n import N_, tr
from telescope.widgets.common import ui_px

_GAP = 0.01  # the least room between two points of a curve (round the hues for a hue curve)
_HUE_TOP = 0.9999  # a hue point stays below 1, which is the hue at 0 again
_STEP = 0.005  # an arrow key moves the picked point this far
_BIG_STEP = 0.05  # with Shift
_GRAB_PX = 9  # how close a press has to be to a point to pick it up


class CurveEditor(QWidget):
    """Click to add a point, drag to move it, double-click a point (or right-click it) to remove it and double-click
    anywhere else to reset the curve. With the keyboard: arrows move the picked point, [ and ] pick another, Delete
    removes it and Home resets.

    "tone" curves go from black to white with their end points stuck to the edges; "hue" curves go round the hues
    (a point near one edge also bends the other) and 0.5 means no change."""

    edited = pyqtSignal(tuple)
    """The points changed by the user: ((x, y), ...) sorted by x."""

    HINTS = {"tone": N_("Click to add a point and drag it. Double-click a point to remove it, or the curve to reset."),
             "hue": N_("Click a hue to add a point and drag it. Double-click a point to remove it, or the curve to "
                       "reset.")}

    def __init__(self, kind: str = "tone", parent=None):
        super().__init__(parent)
        self._kind = kind
        self._points: list = []
        self._default: tuple = ()
        self._picked = -1
        self._dragging = False
        self._added = None  # (point, press time) of a point the last press added, which a double-click takes off again
        self._colour = QColor(theme.TEXT)
        self.setFocusPolicy(Qt.FocusPolicy.StrongFocus)
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        self.setFixedHeight(ui_px(200))
        self.setMouseTracking(True)

    def sizeHint(self) -> QSize:
        return QSize(ui_px(300), ui_px(200))

    def set_curve(self, kind: str, points, default, colour: str = ""):
        """Show another curve: its kind, points, what Home or a double-click goes back to, and the line's colour."""
        self._kind = kind
        self._points = sorted((float(x), float(y)) for x, y in points)
        self._default = tuple(default)
        self._colour = QColor(colour or theme.TEXT)
        self._picked = min(self._picked, len(self._points) - 1)
        self._added = None
        self.setToolTip(tr(self.HINTS[kind]))
        self.update()

    def points(self) -> tuple:
        return tuple(self._points)

    def picked(self) -> int:
        return self._picked

    # ── Geometry ──────────────────────────────────────────────────────────

    def _area(self) -> QRectF:
        pad = ui_px(8)
        bottom = ui_px(14) if self._kind == "hue" else pad
        return QRectF(pad, pad, self.width() - 2 * pad, self.height() - pad - bottom)

    def _to_px(self, x: float, y: float) -> QPointF:
        a = self._area()
        return QPointF(a.left() + x * a.width(), a.bottom() - y * a.height())

    def _from_px(self, pos: QPointF) -> tuple:
        a = self._area()
        x = (pos.x() - a.left()) / max(1.0, a.width())
        y = (a.bottom() - pos.y()) / max(1.0, a.height())
        return min(1.0, max(0.0, x)), min(1.0, max(0.0, y))

    def _near(self, pos: QPointF) -> int:
        best, best_d = -1, ui_px(_GRAB_PX)
        for i, (x, y) in enumerate(self._points):
            d = (self._to_px(x, y) - pos).manhattanLength()
            if d <= best_d:
                best, best_d = i, d
        return best

    def _fixed_x(self, i: int) -> bool:
        return self._kind == "tone" and i in (0, len(self._points) - 1)

    # ── Editing ───────────────────────────────────────────────────────────

    def _move(self, i: int, x: float, y: float):
        pts = self._points
        if self._fixed_x(i):
            x = pts[i][0]
        elif self._kind == "tone":
            x = min(pts[i + 1][0] - _GAP, max(pts[i - 1][0] + _GAP, x))
        else:  # between its neighbours round the hues, and below 1
            lo = pts[i - 1][0] + _GAP if i > 0 else max(0.0, pts[-1][0] - 1 + _GAP)
            hi = pts[i + 1][0] - _GAP if i < len(pts) - 1 else min(_HUE_TOP, pts[0][0] + 1 - _GAP)
            x = min(hi, max(lo, x))
        pts[i] = (round(x, 4), round(min(1.0, max(0.0, y)), 4))

    def _add(self, x: float, y: float) -> int:
        if self._kind == "hue":
            x = min(_HUE_TOP, x)
            if any(min(abs(px - x), 1 - abs(px - x)) < _GAP for px, _ in self._points):
                return -1
        elif any(abs(px - x) < _GAP for px, _ in self._points):
            return -1
        point = (round(x, 4), round(y, 4))
        self._points.append(point)
        self._points.sort()
        return self._points.index(point)

    def _remove(self, i: int):
        if i < 0 or self._fixed_x(i) or (self._kind == "tone" and len(self._points) <= 2):
            return
        del self._points[i]
        self._picked = min(i, len(self._points) - 1)
        self._changed()

    def _reset(self):
        self._points = sorted(self._default)
        self._picked = -1
        self._changed()

    def _changed(self):
        self.update()
        self.edited.emit(tuple(self._points))

    # ── Input ─────────────────────────────────────────────────────────────

    def mousePressEvent(self, event):
        pos = event.position()
        if event.button() == Qt.MouseButton.RightButton:
            self._remove(self._near(pos))
            return
        if event.button() != Qt.MouseButton.LeftButton:
            return
        i = self._near(pos)
        added, self._added = self._added, None
        if i < 0:
            x, _y = self._from_px(pos)
            y = float(self._curve(np.array([x], np.float32))[0])  # on the line, then dragged from there
            i = self._add(x, y)
            if i >= 0:
                self._added = (self._points[i], event.timestamp())
                self._changed()
        elif added and self._points[i] == added[0] and \
                event.timestamp() - added[1] <= QApplication.doubleClickInterval():
            self._added = added  # the second press of a double-click on the point the first one added
        self._picked = i
        self._dragging = i >= 0
        self.update()

    def mouseMoveEvent(self, event):
        if self._dragging and self._picked >= 0:
            self._move(self._picked, *self._from_px(event.position()))
            self._added = None
            self._changed()
        else:
            near = self._near(event.position()) >= 0
            self.setCursor(Qt.CursorShape.PointingHandCursor if near else Qt.CursorShape.CrossCursor)

    def mouseReleaseEvent(self, event):
        self._dragging = False

    def mouseDoubleClickEvent(self, event):
        if event.button() != Qt.MouseButton.LeftButton:
            return
        added, self._added = self._added, None
        self._dragging = False
        if added and added[0] in self._points:
            self._points.remove(added[0])  # the first click of this double-click added it
            self._reset()
            return
        i = self._near(event.position())
        if i >= 0 and not self._fixed_x(i) and not (self._kind == "tone" and len(self._points) <= 2):
            self._remove(i)
        else:
            self._reset()

    def wheelEvent(self, event):
        event.ignore()

    def keyPressEvent(self, event):
        key, n = event.key(), len(self._points)
        step = _BIG_STEP if event.modifiers() & Qt.KeyboardModifier.ShiftModifier else _STEP
        moves = {Qt.Key.Key_Left: (-step, 0), Qt.Key.Key_Right: (step, 0), Qt.Key.Key_Up: (0, step),
                 Qt.Key.Key_Down: (0, -step)}
        if key in moves and n:
            if self._picked < 0:
                self._picked = 0
            x, y = self._points[self._picked]
            dx, dy = moves[key]
            self._move(self._picked, x + dx, y + dy)
            self._changed()
        elif key in (Qt.Key.Key_BracketLeft, Qt.Key.Key_BracketRight) and n:
            forward = key == Qt.Key.Key_BracketRight
            self._picked = (self._picked + (1 if forward else -1)) % n if self._picked >= 0 else 0
            self.update()
        elif key in (Qt.Key.Key_Delete, Qt.Key.Key_Backspace):
            self._remove(self._picked)
        elif key == Qt.Key.Key_Home:
            self._reset()
        else:
            super().keyPressEvent(event)

    # ── Painting ──────────────────────────────────────────────────────────

    def _curve(self, xs: np.ndarray) -> np.ndarray:
        if self._kind == "hue":
            return periodic_curve(self._points, xs)
        return monotone_curve(self._points, xs)

    def paintEvent(self, _event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        a = self._area()
        enabled = self.isEnabled()
        p.setPen(QPen(QColor(theme.BORDER), 1))
        p.setBrush(QColor(theme.SURFACE_SUNK))
        p.drawRoundedRect(a.adjusted(-1, -1, 1, 1), ui_px(4), ui_px(4))

        if self._kind == "hue":
            strip = QRectF(a.left(), a.bottom() + ui_px(4), a.width(), ui_px(6))
            hues = QLinearGradient(strip.topLeft(), strip.topRight())
            for i in range(13):
                hues.setColorAt(i / 12, QColor.fromHsvF((i / 12) % 1.0, 0.55 if enabled else 0.15, 0.85))
            p.setPen(Qt.PenStyle.NoPen)
            p.setBrush(hues)
            p.drawRoundedRect(strip, ui_px(2), ui_px(2))

        p.setPen(QPen(QColor(theme.BORDER), 1))
        for i in range(1, 4):
            x = a.left() + a.width() * i / 4
            y = a.top() + a.height() * i / 4
            p.drawLine(QPointF(x, a.top()), QPointF(x, a.bottom()))
            p.drawLine(QPointF(a.left(), y), QPointF(a.right(), y))
        p.setPen(QPen(QColor(theme.BORDER_STRONG), 1, Qt.PenStyle.DashLine))
        if self._kind == "hue":
            p.drawLine(self._to_px(0, 0.5), self._to_px(1, 0.5))
        else:
            p.drawLine(self._to_px(0, 0), self._to_px(1, 1))

        xs = np.linspace(0, 1, max(32, int(a.width())), dtype=np.float32)
        ys = self._curve(xs)
        path = QPainterPath(self._to_px(float(xs[0]), float(ys[0])))
        for x, y in zip(xs[1:].tolist(), ys[1:].tolist()):
            path.lineTo(self._to_px(x, y))
        p.save()
        p.setClipRect(a.adjusted(-2, -2, 2, 2))
        p.setPen(QPen(self._colour if enabled else QColor(theme.TEXT_DISABLED), ui_px(2)))
        p.setBrush(Qt.BrushStyle.NoBrush)
        p.drawPath(path)
        p.restore()

        size = ui_px(5)
        for i, (x, y) in enumerate(self._points):
            picked = i == self._picked
            p.setPen(QPen(self._colour if enabled else QColor(theme.TEXT_DISABLED), 1.5))
            p.setBrush(self._colour if picked and enabled else QColor(theme.SURFACE))
            p.drawEllipse(self._to_px(x, y), size, size)

        if theme.keyboard_focus(self):
            theme.focus_ring(p, QRectF(self.rect()).adjusted(1, 1, -1, -1), ui_px(6))
        p.end()
