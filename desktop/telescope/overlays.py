"""What plugins draw on the preview, and the points and areas they ask for there. None of it reaches the virtual camera.

The window makes one PreviewOverlays (window._overlays) and the HostPreview methods forward to it. Whatever shows the
preview (the Preview plugin) hands it its own scaled copy of each frame through tint(), draws paint() over the picture
and passes it the mouse and keys while a pick runs. Without a preview, layers just aren't drawn and picks can't start."""
import logging
import threading
import time
from dataclasses import dataclass, field
from typing import Callable, Optional

import numpy as np
from PyQt6.QtCore import QObject, QPointF, QRectF, Qt, pyqtSignal
from PyQt6.QtGui import QColor, QFont, QFontMetricsF, QPainter, QPainterPath, QPen, QPolygonF

from telescope.i18n import tr
from telescope.view import View, to_full, to_shown

logger = logging.getLogger(__name__)

PICK_KINDS = ("point", "rect", "quad")
_HANDLE = 7  # px, the radius of a corner handle
_GRAB = 16  # px, how close to a corner a press grabs it
_MIN_RECT = 0.01  # a rectangle narrower or shorter than this (of the frame) is a stray click, not an area
_QUAD_INSET = 0.2  # where a quad's corners start without a start, in from each edge of the frame as shown


class PreviewLayer:
    """Something a plugin draws on the preview (host.add_preview_layer). Override what it needs; the rest does nothing.
    Apps never see any of it."""

    order: int = 0
    """Layers with a lower order are tinted and painted first, under the others."""
    analyse_fps: float = 0.0
    """How many times a second analyse() wants a frame; 0 for never. Read each frame, so it can change."""

    def tint(self, frame: np.ndarray) -> np.ndarray:
        """UI thread: the preview's own copy of the frame as shown, already scaled to the preview's size (never above
        the stream's). Return it changed (in place is fine) to colour the picture. Runs for each frame the preview
        shows, so keep it cheap."""
        return frame

    def analyse(self, frame: np.ndarray) -> bool:
        """Its own thread, at most analyse_fps a second: a copy of the preview's frame before any tint, to work
        something out from (a scope). True asks for paint() to run again."""
        return False

    def paint(self, painter: QPainter, rect: QRectF):
        """UI thread: draw over the frame, which sits at rect in the preview's widget pixels (painting is clipped to
        it). Ask for another paint with host.refresh_preview()."""


@dataclass(frozen=True)
class Pick:
    """What host.pick_on_preview hands its on_done: one point, a rectangle's top left and bottom right corners, or
    four corners in the order they were placed."""

    kind: str
    """"point", "rect" or "quad"."""
    shown: tuple
    """The points as (u, v), 0..1 in the frame as shown when the pick ended: where they are in process_frame's frame."""
    full: tuple
    """The same points in the main lens's whole picture, before zoom, pan, flip and rotation: they stay on the same
    spot when the view changes (telescope.view.to_shown brings them back)."""
    picture: Optional[np.ndarray] = field(default=None, compare=False, repr=False)
    """The picture the preview showed when the pick ended (BGR, the preview's size, before any layer's tint), for
    reading colours at in_picture: it's what was clicked on, after every plugin. None without one."""
    in_picture: tuple = ()
    """The points where they are in picture (0..1). The same as shown unless a plugin moves the picture around after
    the view (Desk mode's warp, see set_warp)."""


@dataclass
class _PickState:
    owner: str
    kind: str
    on_done: Callable
    hint: str
    points: list  # in the whole picture (to_full), so they stay put while the view moves
    drag: object = None  # the corner being dragged, "all" for the whole quad, "rect" while drawing one, or None
    grab_at: tuple = (0.0, 0.0)  # where a drag of the whole quad is now, in the frame as shown


def _clamp(point: tuple) -> tuple:
    return min(max(point[0], 0.0), 1.0), min(max(point[1], 0.0), 1.0)


def _nudge(d: float, lo: float, hi: float) -> float:
    """A move of d along one axis of shapes whose corners span lo..hi there, cut short only where it would push a
    corner further off the frame. A shape already reaching past an edge doesn't jump back."""
    if d < 0:
        return max(d, min(0.0, -lo))
    return min(d, max(0.0, 1.0 - hi))


def _inside(point: tuple, polygon: list) -> bool:
    """Whether point is inside polygon (ray casting)."""
    x, y = point
    inside = False
    for (x0, y0), (x1, y1) in zip(polygon, polygon[1:] + polygon[:1]):
        if (y0 > y) != (y1 > y) and x < x0 + (y - y0) * (x1 - x0) / (y1 - y0):
            inside = not inside
    return inside


class PreviewOverlays(QObject):
    changed = pyqtSignal()
    """Something to draw changed: whatever shows the preview paints again."""
    pick_changed = pyqtSignal(str)
    """A pick started (its owner) or ended ("")."""
    _analysed = pyqtSignal()

    def __init__(self, view: Callable[[], View]):
        super().__init__()
        self._view = view
        self._layers: dict = {}
        self._sorted: tuple = ()  # (owner, layer), by order; swapped whole, since the stream thread reads it
        self._failing: set = set()  # owners whose tint or analyse failed, so it's logged once
        self._surfaces = 0
        self._pick: Optional[_PickState] = None
        self._picture: Optional[np.ndarray] = None  # the last frame the preview showed, while a pick runs
        self._warp: Optional[tuple] = None  # (owner, matrix, inverse): a plugin moving the picture after the view
        self._analysed.connect(self.changed)
        self._cond = threading.Condition()
        self._pending: Optional[np.ndarray] = None
        self._thread: Optional[threading.Thread] = None
        self._quit = False
        self._fed_at = 0.0

    # ── Layers ────────────────────────────────────────────────────────────

    def add_layer(self, owner: str, layer: PreviewLayer):
        self._layers[owner] = layer
        self._failing.discard(owner)
        self._sort()

    def remove_layer(self, owner: str):
        if self._layers.pop(owner, None) is not None:
            self._sort()

    def layers(self) -> list:
        return [layer for _owner, layer in self._sorted]

    def _sort(self):
        self._sorted = tuple(sorted(self._layers.items(), key=lambda item: item[1].order))
        self.changed.emit()

    def refresh(self):
        """Paint again (a layer's paint() would draw something else now). Any thread."""
        self._analysed.emit()

    # ── Whatever shows the preview ────────────────────────────────────────

    def attach(self):
        """Something shows the preview now, so picks can start."""
        self._surfaces += 1

    def detach(self):
        self._surfaces = max(0, self._surfaces - 1)
        if not self._surfaces:
            self.cancel_pick()

    def tint(self, frame: np.ndarray) -> np.ndarray:
        """UI thread: the preview's copy of a frame through every layer's tint(), and handed to analyse() when one
        wants it. A layer that fails leaves the frame as it was."""
        if self._pick is not None:
            self._picture = frame.copy()
        layers = self._sorted
        if not layers:
            return frame
        fps = max((layer.analyse_fps for _owner, layer in layers), default=0.0)
        now = time.monotonic()
        if fps > 0 and now - self._fed_at >= 1.0 / fps:
            self._fed_at = now
            self._feed(frame.copy())
        for owner, layer in layers:
            try:
                out = layer.tint(frame)
                if isinstance(out, np.ndarray) and out.shape == frame.shape and out.dtype == frame.dtype:
                    frame = out
            except Exception:
                self._failed(owner, "tint")
        return frame

    def _failed(self, owner: str, what: str):
        if owner not in self._failing:
            self._failing.add(owner)
            logger.exception("Preview layer %s failed in %s", owner, what)

    def _feed(self, frame: np.ndarray):
        with self._cond:
            if self._quit:
                return
            self._pending = frame
            if self._thread is None:
                self._thread = threading.Thread(target=self._analyse_loop, name="preview-analyse", daemon=True)
                self._thread.start()
            self._cond.notify()

    def _analyse_loop(self):
        while True:
            with self._cond:
                while self._pending is None and not self._quit:
                    self._cond.wait()
                if self._quit:
                    return
                frame, self._pending = self._pending, None
            repaint = False
            for owner, layer in self._sorted:
                if layer.analyse_fps <= 0:
                    continue
                try:
                    repaint = layer.analyse(frame) or repaint
                except Exception:
                    self._failed(owner, "analyse")
            if repaint:
                self._analysed.emit()

    def shutdown(self):
        with self._cond:
            self._quit = True
            self._cond.notify()
        if self._thread is not None:
            self._thread.join(timeout=1)

    def paint(self, painter: QPainter, rect: QRectF):
        """UI thread: every layer, then the pick under way, over the frame at rect."""
        for owner, layer in self._sorted:
            painter.save()
            try:
                layer.paint(painter, rect)
            except Exception:
                self._failed(owner, "paint")
            finally:
                painter.restore()
        if self._pick is not None:
            painter.save()
            self._paint_pick(painter, rect)
            painter.restore()

    # ── A plugin moving the picture ───────────────────────────────────────

    def set_warp(self, owner: str, matrix):
        """Any thread: owner's plugin moves the whole picture after the view, by matrix (a 3 x 3 homography from the
        frame as shown with the view to what the preview shows, both 0..1), or stops with None. A matrix that can't be
        undone (it squashes the picture flat) counts as None."""
        inverse = None
        if matrix is not None:
            matrix = np.asarray(matrix, np.float64)
            try:
                inverse = np.linalg.inv(matrix)
            except np.linalg.LinAlgError:
                logger.warning("%s gave the preview a warp that can't be undone; ignoring it", owner)
        if inverse is None or not np.isfinite(inverse).all():
            if self._warp is not None and self._warp[0] == owner:
                self._warp = None
            return
        self._warp = (owner, matrix, inverse)

    def warping(self) -> bool:
        return self._warp is not None

    @staticmethod
    def _apply(matrix, u: float, v: float) -> tuple:
        x, y, z = matrix @ (u, v, 1.0)
        return (x / z, y / z) if abs(z) > 1e-9 else (u, v)

    def unwarp(self, u: float, v: float) -> tuple:
        """A point on the preview back to the frame as shown with the view (where clicks there mean)."""
        warp = self._warp
        return self._apply(warp[2], u, v) if warp is not None else (u, v)

    def warp_point(self, u: float, v: float) -> tuple:
        """The reverse of unwarp: where a point of the frame as shown lands on the preview."""
        warp = self._warp
        return self._apply(warp[1], u, v) if warp is not None else (u, v)

    # ── Picks ─────────────────────────────────────────────────────────────

    def start_pick(self, owner: str, kind: str, on_done: Callable, start=None, hint: str = "") -> bool:
        if kind not in PICK_KINDS:
            raise ValueError(f"unknown pick kind {kind!r}")
        if not self._surfaces:
            return False
        self.cancel_pick()
        view = self._view()
        points = []
        if kind == "quad":
            if start is not None and len(start) == 4:
                points = [(float(x), float(y)) for x, y in start]
            else:
                lo, hi = _QUAD_INSET, 1.0 - _QUAD_INSET
                points = [to_full(view, u, v) for u, v in ((lo, lo), (hi, lo), (hi, hi), (lo, hi))]
        self._pick = _PickState(owner, kind, on_done, hint, points)
        self._picture = None
        self.pick_changed.emit(owner)
        self.changed.emit()
        return True

    def picking(self) -> str:
        """The owner of the pick under way, or ""."""
        return self._pick.owner if self._pick is not None else ""

    def finish_pick(self, owner: Optional[str] = None) -> bool:
        """End the pick with the points it has (a quad as it is now). False if it has none yet."""
        pick = self._pick
        if pick is None or (owner is not None and pick.owner != owner):
            return False
        if pick.kind == "rect" and len(pick.points) < 2:
            return False
        if pick.kind == "point" and not pick.points:
            return False
        self._end(self._result(pick))
        return True

    def cancel_pick(self, owner: Optional[str] = None):
        """End the pick under way (only owner's, given one) without points: its on_done gets None."""
        pick = self._pick
        if pick is not None and (owner is None or pick.owner == owner):
            self._end(None)

    def _end(self, result: Optional[Pick]):
        pick, self._pick = self._pick, None
        self._picture = None
        self.pick_changed.emit("")
        self.changed.emit()
        try:
            pick.on_done(result)
        except Exception:
            logger.exception("%s couldn't take its pick", pick.owner)

    def _result(self, pick: _PickState) -> Pick:
        view = self._view()
        full = list(pick.points)
        if pick.kind == "rect":
            (x0, y0), (x1, y1) = full
            full = [(min(x0, x1), min(y0, y1)), (max(x0, x1), max(y0, y1))]
            shown = [to_shown(view, *p) for p in full]
            (u0, v0), (u1, v1) = shown
            shown = [(min(u0, u1), min(v0, v1)), (max(u0, u1), max(v0, v1))]
        else:
            shown = [to_shown(view, *p) for p in full]
        on_screen = tuple(_clamp(self.warp_point(*p)) for p in shown)
        return Pick(pick.kind, tuple(_clamp(p) for p in shown), tuple(_clamp(p) for p in full), self._picture,
                    on_screen)

    def _shown_points(self) -> list:
        view = self._view()
        return [to_shown(view, *p) for p in self._pick.points] if self._pick else []

    def _screen_points(self) -> list:
        """The pick's points where they are on the preview."""
        return [self.warp_point(*p) for p in self._shown_points()]

    def _corner_at(self, u: float, v: float, size: tuple) -> Optional[int]:
        w, h = size
        best, best_d = None, _GRAB
        for i, (pu, pv) in enumerate(self._screen_points()):
            d = ((pu - u) * w) ** 2 + ((pv - v) * h) ** 2
            if d <= best_d ** 2:
                best, best_d = i, d ** 0.5
        return best

    def press(self, u: float, v: float, size: tuple) -> bool:
        """The left button went down at (u, v) in the frame as shown (size: the frame's size on screen, px). True if
        the pick took it; False leaves it to the preview (a drag that pans)."""
        pick = self._pick
        if pick is None:
            return False
        full = to_full(self._view(), *self.unwarp(u, v))
        if pick.kind == "point":
            pick.points = [full]
            return True
        if pick.kind == "rect":
            pick.points = [full, full]
            pick.drag = "rect"
            self.changed.emit()
            return True
        corner = self._corner_at(u, v, size)
        if corner is not None:
            pick.drag = corner
            return True
        if _inside((u, v), self._screen_points()):
            pick.drag, pick.grab_at = "all", self.unwarp(u, v)
            return True
        return False

    def move(self, u: float, v: float, size: tuple) -> bool:
        """The mouse moved with the button down after press() took it."""
        pick = self._pick
        if pick is None or pick.drag is None:
            return False
        u, v = self.unwarp(min(max(u, 0.0), 1.0), min(max(v, 0.0), 1.0))
        full = to_full(self._view(), u, v)
        if pick.drag == "rect":
            pick.points[1] = full
        elif pick.drag == "all":
            # Moved as far as all four corners stay on the picture, so the shape never gets squashed at an edge
            shown = self._shown_points()
            us, vs = [p[0] for p in shown], [p[1] for p in shown]
            du = _nudge(u - pick.grab_at[0], min(us), max(us))
            dv = _nudge(v - pick.grab_at[1], min(vs), max(vs))
            view = self._view()
            pick.points = [to_full(view, pu + du, pv + dv) for pu, pv in shown]
            pick.grab_at = (pick.grab_at[0] + du, pick.grab_at[1] + dv)
        else:
            pick.points[pick.drag] = full
        self.changed.emit()
        return True

    def release(self, u: float, v: float) -> bool:
        """The button came up. A point is picked here, and a rectangle once it's big enough; a quad waits for Enter or
        finish_pick()."""
        pick = self._pick
        if pick is None:
            return False
        drag, pick.drag = pick.drag, None
        if pick.kind == "point" and pick.points:
            self._end(self._result(pick))
        elif pick.kind == "rect" and drag == "rect":
            (u0, v0), (u1, v1) = self._shown_points()
            if abs(u1 - u0) >= _MIN_RECT and abs(v1 - v0) >= _MIN_RECT:
                self._end(self._result(pick))
            else:
                pick.points = []
                self.changed.emit()
        return True

    def key(self, key: int) -> bool:
        """Enter finishes the pick, Escape cancels it. True if it was for the pick."""
        if self._pick is None:
            return False
        if key in (Qt.Key.Key_Return, Qt.Key.Key_Enter):
            self.finish_pick()
            return True
        if key == Qt.Key.Key_Escape:
            self.cancel_pick()
            return True
        return False

    def cursor(self, u: float, v: float, size: tuple) -> Optional[Qt.CursorShape]:
        """The mouse cursor over (u, v) while a pick runs, or None for the preview's usual one."""
        pick = self._pick
        if pick is None:
            return None
        if pick.kind != "quad":
            return Qt.CursorShape.CrossCursor
        if pick.drag is not None or self._corner_at(u, v, size) is not None:
            return Qt.CursorShape.SizeAllCursor
        if _inside((u, v), self._screen_points()):
            return Qt.CursorShape.OpenHandCursor
        return None

    def _hint(self, pick: _PickState) -> str:
        if pick.hint:
            return pick.hint
        if pick.kind == "point":
            return tr("Click a point on the picture. Esc cancels.")
        if pick.kind == "rect":
            return tr("Drag across the picture to mark an area. Esc cancels.")
        return tr("Drag the corners into place, then press Enter. Esc cancels.")

    def _paint_pick(self, p: QPainter, rect: QRectF):
        pick = self._pick
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        points = self._shown_points()
        if pick.kind == "rect" and len(points) == 2:
            (u0, v0), (u1, v1) = points
            points = [(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
        at = [QPointF(rect.x() + u * rect.width(), rect.y() + v * rect.height())
              for u, v in (self.warp_point(*p) for p in points)]
        light, dark = QColor(255, 255, 255, 235), QColor(0, 0, 0, 130)
        if len(at) == 4:
            shape = QPolygonF(at + at[:1])
            outside = QPainterPath()
            outside.addRect(rect)
            outside.addPolygon(shape)
            outside.setFillRule(Qt.FillRule.OddEvenFill)
            p.fillPath(outside, QColor(0, 0, 0, 90))
            for colour, width in ((dark, 4), (light, 2)):
                p.setPen(QPen(colour, width))
                p.setBrush(Qt.BrushStyle.NoBrush)
                p.drawPolygon(shape)
            if pick.kind == "quad":
                p.setPen(QPen(dark, 2))
                p.setBrush(light)
                for point in at:
                    p.drawEllipse(point, _HANDLE, _HANDLE)
        self._paint_hint(p, rect, self._hint(pick))

    @staticmethod
    def _paint_hint(p: QPainter, rect: QRectF, text: str):
        font = QFont(p.font())
        font.setPointSizeF(9.0)
        font.setBold(True)
        p.setFont(font)
        fm = QFontMetricsF(font)
        pad = 6.0
        width = min(fm.horizontalAdvance(text) + 2 * pad, rect.width() - 2 * pad)
        chip = QRectF(rect.center().x() - width / 2, rect.y() + 2 * pad, width, fm.height() + pad)
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(QColor(0, 0, 0, 170))
        p.drawRoundedRect(chip, 4, 4)
        p.setPen(QColor(255, 255, 255))
        p.drawText(chip, Qt.AlignmentFlag.AlignCenter, fm.elidedText(text, Qt.TextElideMode.ElideRight, width - pad))
