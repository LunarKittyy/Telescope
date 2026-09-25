import cv2
import numpy as np
from PyQt6.QtCore import Qt, QEvent, QObject, QPoint, QSize, QTimer, pyqtSignal
from PyQt6.QtGui import QImage, QPixmap
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


class FrameLabel(QLabel):
    """A QLabel showing a letterboxed frame that reports clicks as a point in the frame (0..1)."""

    picked = pyqtSignal(float, float)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._pickable = False
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
        self.setCursor(Qt.CursorShape.CrossCursor if on else Qt.CursorShape.ArrowCursor)

    def frame_point(self, pos: QPoint):
        """Where pos falls in the shown frame, as (u, v) in 0..1, or None in the letterbox bars."""
        pm = self.pixmap()
        if pm is None or pm.isNull():
            return None
        pw, ph = pm.width() / pm.devicePixelRatio(), pm.height() / pm.devicePixelRatio()
        x0, y0 = (self.width() - pw) / 2, (self.height() - ph) / 2
        u, v = (pos.x() - x0) / pw, (pos.y() - y0) / ph
        if not (0.0 <= u <= 1.0 and 0.0 <= v <= 1.0):
            return None
        return u, v

    def mousePressEvent(self, event):
        point = self.frame_point(event.position().toPoint()) if self._pickable else None
        if point is None or event.button() != Qt.MouseButton.LeftButton:
            super().mousePressEvent(event)
            return
        pos = event.position().toPoint()
        self._marker.move(pos.x() - _MARKER // 2, pos.y() - _MARKER // 2)
        self._marker.show()
        self._marker.raise_()
        self._marker_timer.start(1000)
        self.picked.emit(*point)


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
        bus.focus_point_available.connect(self._on_focus_point_available)

    def create_panel(self) -> QWidget:
        """Video stage: letterboxed frame area with toolbar beneath (centre column, no chrome)."""
        stage = QFrame()
        stage.setObjectName("preview_stage")
        stage.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding)
        lay = QVBoxLayout(stage)
        lay.setContentsMargins(1, 1, 1, 1)
        lay.setSpacing(0)

        self._preview_lbl = FrameLabel()
        self._preview_lbl.picked.connect(self._bus.focus_point_picked.emit)
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
        self._popout._lbl.picked.connect(self._bus.focus_point_picked.emit)
        self._popout._lbl.set_pickable(self._pickable)
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
