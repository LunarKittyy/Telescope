import cv2
import numpy as np
from PyQt6.QtCore import Qt, QEvent, QObject, QPoint, QRectF, QSize, QTimer, QVariantAnimation, pyqtSignal
from PyQt6.QtGui import QColor, QFont, QFontMetricsF, QImage, QPainter, QPen, QPixmap
from PyQt6.QtWidgets import (
    QFrame, QHBoxLayout, QLabel, QPushButton, QSizePolicy, QVBoxLayout, QWidget,
)

from telescope import theme
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import create_vector_icon, set_ui_role, ui_px


_IDLE_TEXT    = "Not streaming\n\nPress Start Streaming and the phone's camera comes up on its own."
_WAITING_TEXT = "Waiting for the first frame\u2026"


class _Sig(QObject):
    frame = pyqtSignal(object)


_MARKER = 44  # the square drawn where you clicked, in px
_DRAG_START = 4  # px the mouse has to move before a press is a drag, not a click
_WHEEL_STEP = 1.1  # zoom factor per wheel notch
_BOXES_HOLD_MS = 1000  # the lens boxes stay this long after the framing last moved, then fade
_BOXES_FADE_MS = 300


class FrameLabel(QLabel):
    """A QLabel showing a letterboxed frame. Reports clicks as a point in the frame (0..1), drags and
    wheel turns for panning and zooming, and draws the lens boxes over the frame (never into it)."""

    picked = pyqtSignal(float, float)
    dragged = pyqtSignal(float, float)            # moved by (du, dv), fractions of the frame
    scrolled = pyqtSignal(float, float, float)    # zoom factor, and the (u, v) under the mouse

    def __init__(self, parent=None):
        super().__init__(parent)
        self._pickable = False
        self._pannable = False
        self._press = None      # where the left button went down, while it's down on the frame
        self._last = None       # the last position a drag was reported from
        self._dragging = False
        self._boxes: list = []  # [(label, x0, y0, x1, y1)] in the frame, 0..1
        self._boxes_opacity = 0.0
        self._marker = QFrame(self)
        self._marker.setObjectName("focus_marker")
        self._marker.setAttribute(Qt.WidgetAttribute.WA_TransparentForMouseEvents)
        self._marker.resize(_MARKER, _MARKER)
        self._marker.hide()
        self._marker_timer = QTimer(self)
        self._marker_timer.setSingleShot(True)
        self._marker_timer.timeout.connect(self._marker.hide)

    def set_pickable(self, on: bool):
        self._pickable = on
        self._update_cursor()

    def set_pannable(self, on: bool):
        self._pannable = on
        self._update_cursor()

    def set_boxes(self, boxes: list, opacity: float):
        if boxes != self._boxes or opacity != self._boxes_opacity:
            self._boxes, self._boxes_opacity = boxes, opacity
            self.update()

    def _update_cursor(self):
        if self._dragging:
            shape = Qt.CursorShape.ClosedHandCursor
        elif self._pannable:
            shape = Qt.CursorShape.OpenHandCursor
        elif self._pickable:
            shape = Qt.CursorShape.CrossCursor
        else:
            shape = Qt.CursorShape.ArrowCursor
        self.setCursor(shape)

    def frame_rect(self):
        """Where the frame sits inside the letterbox, in widget px; None without one."""
        pm = self.pixmap()
        if pm is None or pm.isNull():
            return None
        pw, ph = pm.width() / pm.devicePixelRatio(), pm.height() / pm.devicePixelRatio()
        return QRectF((self.width() - pw) / 2, (self.height() - ph) / 2, pw, ph)

    def frame_point(self, pos: QPoint):
        """Where pos falls in the shown frame, as (u, v) in 0..1, or None in the letterbox bars."""
        rect = self.frame_rect()
        if rect is None:
            return None
        u, v = (pos.x() - rect.x()) / rect.width(), (pos.y() - rect.y()) / rect.height()
        if not (0.0 <= u <= 1.0 and 0.0 <= v <= 1.0):
            return None
        return u, v

    def mousePressEvent(self, event):
        pos = event.position().toPoint()
        if (event.button() != Qt.MouseButton.LeftButton or not (self._pickable or self._pannable)
                or self.frame_point(pos) is None):
            super().mousePressEvent(event)
            return
        self._press = self._last = pos
        self._dragging = False

    def mouseMoveEvent(self, event):
        if self._press is None or not self._pannable:
            super().mouseMoveEvent(event)
            return
        pos = event.position().toPoint()
        if not self._dragging:
            if (pos - self._press).manhattanLength() < ui_px(_DRAG_START):
                return
            self._dragging = True
            self._update_cursor()
        rect = self.frame_rect()
        if rect is not None:
            self.dragged.emit((pos.x() - self._last.x()) / rect.width(), (pos.y() - self._last.y()) / rect.height())
        self._last = pos

    def mouseReleaseEvent(self, event):
        if self._press is None or event.button() != Qt.MouseButton.LeftButton:
            super().mouseReleaseEvent(event)
            return
        press, was_drag = self._press, self._dragging
        self._press = self._last = None
        self._dragging = False
        self._update_cursor()
        point = self.frame_point(press)
        if was_drag or not self._pickable or point is None:
            return
        self._marker.move(press.x() - _MARKER // 2, press.y() - _MARKER // 2)
        self._marker.show()
        self._marker.raise_()
        self._marker_timer.start(1000)
        self.picked.emit(*point)

    def wheelEvent(self, event):
        notches = event.angleDelta().y() / 120
        point = self.frame_point(event.position().toPoint())
        if not notches or point is None:
            super().wheelEvent(event)
            return
        self.scrolled.emit(_WHEEL_STEP ** notches, *point)

    def paintEvent(self, event):
        super().paintEvent(event)
        rect = self.frame_rect()
        if rect is None or not self._boxes or self._boxes_opacity <= 0.0:
            return
        p = QPainter(self)
        p.setClipRect(rect)
        p.setOpacity(self._boxes_opacity)
        font = QFont(self.font())
        font.setPointSizeF(8.0)
        font.setBold(True)
        p.setFont(font)
        fm = QFontMetricsF(font)
        halo, line = ui_px(4), ui_px(2)
        for label, x0, y0, x1, y1 in self._boxes:
            eps = 1e-3
            if x0 <= eps and y0 <= eps and x1 >= 1 - eps and y1 >= 1 - eps:
                continue  # the whole frame is inside it (exactly on the lens, or past it): no edge to show
            # Edges on whole pixels, so an even-width line covers whole pixels and stays crisp.
            box = QRectF(round(rect.x() + x0 * rect.width()), round(rect.y() + y0 * rect.height()), 0, 0)
            box.setRight(round(rect.x() + x1 * rect.width()))
            box.setBottom(round(rect.y() + y1 * rect.height()))
            p.setBrush(Qt.BrushStyle.NoBrush)
            p.setPen(QPen(QColor(0, 0, 0, 120), halo))
            p.drawRect(box)
            p.setPen(QPen(QColor(255, 255, 255, 235), line))
            p.drawRect(box)
            # The label in the box's top-left corner, or the corner of the part of it still in view.
            seen = box.intersected(rect)
            pad = ui_px(4)
            tag = QRectF(seen.x() + line + pad, seen.y() + line + pad,
                         fm.horizontalAdvance(label) + 2 * pad, fm.height() + pad)
            if tag.right() + pad > seen.right() or tag.bottom() + pad > seen.bottom():
                continue  # only a sliver of the box is in view: a label there would stick out of it
            p.setPen(Qt.PenStyle.NoPen)
            p.setBrush(QColor(0, 0, 0, 150))
            p.drawRoundedRect(tag, ui_px(3), ui_px(3))
            p.setPen(QColor(255, 255, 255))
            p.drawText(tag, Qt.AlignmentFlag.AlignCenter, label)


class _PopoutWindow(QWidget):
    """Floating preview window that enforces the stream's aspect ratio on resize."""

    closed = pyqtSignal()

    def __init__(self, parent=None):
        super().__init__(parent, Qt.WindowType.Window)
        self.setWindowTitle("Telescope - Video Preview")
        self.setMinimumSize(320, 180)
        self._aspect: float = 16 / 9
        self._adjusting = False

        self._lbl = FrameLabel()
        self._lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._lbl.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding)
        self._lbl.setStyleSheet("background: #000;")

        lay = QVBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.addWidget(self._lbl)

    def set_frame(self, pixmap: QPixmap, aspect: float):
        self._aspect = aspect
        self._lbl.setPixmap(
            pixmap.scaled(
                self._lbl.width(), self._lbl.height(),
                Qt.AspectRatioMode.KeepAspectRatio,
                Qt.TransformationMode.SmoothTransformation,
            )
        )

    def resizeEvent(self, event):
        super().resizeEvent(event)
        if self._adjusting or self._aspect <= 0:
            return
        self._adjusting = True
        target_h = round(self.width() / self._aspect)
        if abs(target_h - self.height()) > 4:
            self.resize(self.width(), target_h)
        self._adjusting = False

    def closeEvent(self, event):
        self.closed.emit()
        super().closeEvent(event)


class _HostFilter(QObject):
    """Event filter installed on the main window to detect hide/show."""
    visibility_changed = pyqtSignal(bool)

    def eventFilter(self, obj, event):
        if event.type() == QEvent.Type.Hide:
            self.visibility_changed.emit(False)
        elif event.type() == QEvent.Type.Show:
            self.visibility_changed.emit(True)
        return False


class PreviewPlugin(TelescopePlugin):
    name = "preview"
    panel_region = "center"

    # Max width for in-window preview sent across thread (stage is now the widest element).
    _CARD_MAX_W = 960

    def setup(self, host, bus):
        self._host   = host
        # On by default (toggle is escape hatch for decode savings).
        self._active = True
        self._popout: _PopoutWindow | None = None
        # Flag; process_frame() runs on stream thread and must never touch self._popout (not thread-safe).
        self._popout_active = False
        self._busy   = False
        # Skip decoding for the card while the window is in the tray, without flipping the user's Hide/Show choice.
        self._host_visible = True
        self._sig    = _Sig()
        self._sig.frame.connect(self._on_frame)

        self._host_filter = _HostFilter()
        self._host_filter.visibility_changed.connect(self._on_host_visibility)
        host.installEventFilter(self._host_filter)
        bus.setup_needed.connect(self._on_setup_needed)
        self._bus = bus
        self._pickable = False
        self._pannable = False
        bus.focus_point_available.connect(self._on_focus_point_available)
        # Lens boxes: shown for a moment whenever the framing moves, or all the time with the Lenses button.
        self._boxes: list = []
        self._boxes_opacity = 0.0
        self._boxes_hold = QTimer()
        self._boxes_hold.setSingleShot(True)
        self._boxes_hold.setInterval(_BOXES_HOLD_MS)
        self._boxes_fade = QVariantAnimation()
        self._boxes_fade.setStartValue(1.0)
        self._boxes_fade.setEndValue(0.0)
        self._boxes_fade.setDuration(_BOXES_FADE_MS)
        self._boxes_fade.valueChanged.connect(self._set_boxes_opacity)
        self._boxes_hold.timeout.connect(self._boxes_fade.start)
        bus.lens_boxes.connect(self._on_lens_boxes)
        bus.view_pannable.connect(self._on_pannable)

    def create_panel(self) -> QWidget:
        """Video stage: letterboxed frame area with toolbar beneath (centre column, no chrome)."""
        stage = QFrame()
        stage.setObjectName("preview_stage")
        stage.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding)
        lay = QVBoxLayout(stage)
        lay.setContentsMargins(1, 1, 1, 1)
        lay.setSpacing(0)

        self._preview_lbl = FrameLabel()
        self._wire(self._preview_lbl)
        self._preview_lbl.setObjectName("preview_surface")
        self._preview_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._preview_lbl.setMinimumHeight(240)
        self._preview_lbl.setSizePolicy(
            QSizePolicy.Policy.Ignored, QSizePolicy.Policy.Ignored)  # QLabel min hint ignored; frames scaled on arrival.
        self._preview_lbl.setMinimumWidth(1)
        self._preview_lbl.setText(_IDLE_TEXT)
        lay.addWidget(self._preview_lbl, 1)

        toolbar = QWidget()
        toolbar.setObjectName("preview_toolbar")
        tb_lay = QHBoxLayout(toolbar)
        tb_lay.setContentsMargins(12, 9, 12, 10)
        tb_lay.setSpacing(8)

        self._toggle_btn = QPushButton("Hide")
        self._toggle_btn.setMinimumWidth(ui_px(78))
        set_ui_role(self._toggle_btn, "quiet")
        self._toggle_btn.setToolTip(
            "Stop decoding frames for this view. The virtual camera output is "
            "unaffected either way."
        )
        self._toggle_btn.clicked.connect(self._toggle)
        tb_lay.addWidget(self._toggle_btn)

        tb_lay.addStretch()

        self._lenses_btn = QPushButton("  Lenses")
        self._lenses_btn.setMinimumWidth(ui_px(92))
        set_ui_role(self._lenses_btn, "quiet")
        self._lenses_btn.setIcon(create_vector_icon("lenses", theme.TEXT_DIM))
        self._lenses_btn.setIconSize(QSize(14, 14))
        self._lenses_btn.setCheckable(True)
        self._lenses_btn.setEnabled(False)
        self._lenses_btn.setToolTip(
            "Keep showing what each of the phone's longer lenses sees. They also show for a moment "
            "whenever you zoom or pan. Only here, never in the camera output."
        )
        self._lenses_btn.toggled.connect(self._push_boxes)
        tb_lay.addWidget(self._lenses_btn)

        self._popout_btn = QPushButton("  Pop out")
        self._popout_btn.setMinimumWidth(ui_px(92))
        set_ui_role(self._popout_btn, "quiet")
        self._popout_btn.setIcon(create_vector_icon("expand", theme.TEXT_DIM))
        self._popout_btn.setIconSize(QSize(14, 14))
        self._popout_btn.clicked.connect(self._open_popout)
        tb_lay.addWidget(self._popout_btn)

        lay.addWidget(toolbar)

        self._stage = stage
        return stage

    def _on_setup_needed(self, needed: bool):
        # The first-run checklist takes the stage; there's nothing to preview before setup anyway.
        self._stage.setVisible(not needed)

    def _toggle(self):
        self._active = not self._active
        self._toggle_btn.setText("Hide" if self._active else "Show")
        if not self._active:
            self._preview_lbl.setPixmap(QPixmap())
            self._preview_lbl.setText("Preview hidden")
        else:
            self._preview_lbl.setText(
                _WAITING_TEXT if self._host.is_streaming() else _IDLE_TEXT)

    def on_stream_start(self, stream_url: str, ctrl):
        if self._active and self._preview_lbl.pixmap().isNull():
            self._preview_lbl.setText(_WAITING_TEXT)

    def on_stream_stop(self):
        self._preview_lbl.setPixmap(QPixmap())
        self._preview_lbl.setText(_IDLE_TEXT if self._active else "Preview hidden")

    def _open_popout(self):
        if self._popout and self._popout.isVisible():
            self._popout.raise_()
            self._popout.activateWindow()
            return
        if self._active:
            self._toggle()
        self._toggle_btn.setEnabled(False)

        self._popout = _PopoutWindow(None)
        self._popout.closed.connect(self._on_popout_closed)
        self._wire(self._popout._lbl)
        self._popout._lbl.set_pickable(self._pickable)
        self._popout._lbl.set_pannable(self._pannable)
        self._push_boxes()
        self._popout.resize(640, 360)
        self._popout.show()
        self._popout_active = True

    def _on_popout_closed(self):
        self._popout = None
        self._popout_active = False
        self._toggle_btn.setEnabled(True)

    def _on_focus_point_available(self, available: bool):
        self._pickable = available
        self._preview_lbl.set_pickable(available)
        if self._popout is not None:
            self._popout._lbl.set_pickable(available)

    def _wire(self, lbl: FrameLabel):
        lbl.picked.connect(self._bus.focus_point_picked.emit)
        lbl.dragged.connect(self._bus.view_dragged.emit)
        lbl.scrolled.connect(self._bus.view_scrolled.emit)

    def _labels(self):
        return [self._preview_lbl] + ([self._popout._lbl] if self._popout is not None else [])

    def _on_pannable(self, pannable: bool):
        self._pannable = pannable
        for lbl in self._labels():
            lbl.set_pannable(pannable)

    def _on_lens_boxes(self, boxes: list, moved: bool):
        self._boxes = boxes
        self._lenses_btn.setEnabled(bool(boxes))
        if moved and boxes:
            self._boxes_fade.stop()
            self._boxes_opacity = 1.0
            self._boxes_hold.start()
        self._push_boxes()

    def _set_boxes_opacity(self, opacity):
        self._boxes_opacity = float(opacity)
        self._push_boxes()

    def _push_boxes(self, *_):
        opacity = 1.0 if self._lenses_btn.isChecked() else self._boxes_opacity
        for lbl in self._labels():
            lbl.set_boxes(self._boxes, opacity)

    def get_config(self) -> dict:
        return {"lens_boxes": self._lenses_btn.isChecked()}

    def set_config(self, cfg: dict):
        self._lenses_btn.setChecked(cfg.get("lens_boxes", False) is True)

    def _on_host_visibility(self, visible: bool):
        self._host_visible = visible

    # ── Worker thread ─────────────────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        popout_open = self._popout_active
        card_wanted = self._active and self._host_visible
        if not (card_wanted or popout_open) or self._busy:
            return frame
        self._busy = True
        h, w = frame.shape[:2]
        if popout_open:
            # Full resolution for pop-out - it can be any size
            self._sig.frame.emit(frame.copy())
        else:
            # Downscale to card label size to keep cross-thread copy cheap
            if w > self._CARD_MAX_W:
                small = cv2.resize(frame, (self._CARD_MAX_W, int(h * self._CARD_MAX_W / w)),
                                   interpolation=cv2.INTER_AREA)
            else:
                small = frame.copy()
            self._sig.frame.emit(small)
        return frame

    # ── UI thread ─────────────────────────────────────────────────────────────

    def _on_frame(self, frame: np.ndarray):
        h, w = frame.shape[:2]
        img = QImage(frame.data, w, h, w * 3, QImage.Format.Format_RGB888).copy()
        px = QPixmap.fromImage(img)

        if self._popout and self._popout.isVisible():
            self._popout.set_frame(px, w / h)
        elif self._active:
            self._preview_lbl.setPixmap(
                px.scaled(
                    self._preview_lbl.width(), self._preview_lbl.height(),
                    Qt.AspectRatioMode.KeepAspectRatio,
                    Qt.TransformationMode.SmoothTransformation,
                )
            )
        self._busy = False
