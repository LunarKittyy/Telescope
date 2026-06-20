import cv2
import numpy as np
from PyQt6.QtCore import Qt, QEvent, QObject, pyqtSignal
from PyQt6.QtGui import QImage, QPixmap
from PyQt6.QtWidgets import (
    QFrame, QHBoxLayout, QLabel, QPushButton, QSizePolicy, QVBoxLayout, QWidget,
)

from telescope.plugin import TelescopePlugin
from telescope.widgets.common import create_vector_icon


class _Sig(QObject):
    frame = pyqtSignal(object)


class _PopoutWindow(QWidget):
    """Floating preview window that enforces the stream's aspect ratio on resize."""

    closed = pyqtSignal()

    def __init__(self, parent=None):
        super().__init__(parent, Qt.WindowType.Window)
        self.setWindowTitle("Telescope - Video Preview")
        self.setMinimumSize(320, 180)
        self._aspect: float = 16 / 9
        self._adjusting = False

        self._lbl = QLabel()
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
    hidden = pyqtSignal()

    def eventFilter(self, obj, event):
        if event.type() == QEvent.Type.Hide:
            self.hidden.emit()
        return False


class PreviewPlugin(TelescopePlugin):
    name = "preview"

    # Max width sent across thread for in-card preview (label is ~400px wide at most)
    _CARD_MAX_W = 480

    def setup(self, host, bus):
        self._host   = host
        self._active = False
        self._popout: _PopoutWindow | None = None
        self._busy   = False
        self._sig    = _Sig()
        self._sig.frame.connect(self._on_frame)

        self._host_filter = _HostFilter()
        self._host_filter.hidden.connect(self._on_host_hidden)
        host.installEventFilter(self._host_filter)

    def create_panel(self) -> QWidget:
        card = QFrame()
        card.setFrameShape(QFrame.Shape.StyledPanel)
        card.setObjectName("card")
        lay = QVBoxLayout(card)
        lay.setContentsMargins(14, 14, 14, 14)
        lay.setSpacing(10)

        hdr = QHBoxLayout()
        hdr.setContentsMargins(0, 0, 0, 0)
        hdr.setSpacing(8)
        icon_lbl = QLabel()
        icon_lbl.setPixmap(create_vector_icon("stream", "#518cc6").pixmap(18, 18))
        icon_lbl.setFixedSize(18, 18)
        hdr.addWidget(icon_lbl)
        title_lbl = QLabel("Video Preview")
        title_lbl.setObjectName("card_title")
        hdr.addWidget(title_lbl)
        hdr.addStretch()

        self._toggle_btn = QPushButton("Show")
        self._toggle_btn.setMinimumWidth(62)
        self._toggle_btn.clicked.connect(self._toggle)
        hdr.addWidget(self._toggle_btn)

        self._popout_btn = QPushButton("Pop out")
        self._popout_btn.setMinimumWidth(66)
        self._popout_btn.clicked.connect(self._open_popout)
        hdr.addWidget(self._popout_btn)

        lay.addLayout(hdr)

        self._preview_lbl = QLabel()
        self._preview_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._preview_lbl.setMinimumHeight(180)
        self._preview_lbl.setSizePolicy(
            QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        self._preview_lbl.setObjectName("dim")
        self._preview_lbl.setText("Preview hidden")
        self._preview_lbl.setVisible(False)
        lay.addWidget(self._preview_lbl)

        return card

    def _toggle(self):
        self._active = not self._active
        self._toggle_btn.setText("Hide" if self._active else "Show")
        self._preview_lbl.setVisible(self._active)
        if not self._active:
            self._preview_lbl.setPixmap(QPixmap())
            self._preview_lbl.setText("Preview hidden")

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
        self._popout.resize(640, 360)
        self._popout.show()

    def _on_popout_closed(self):
        self._popout = None
        self._toggle_btn.setEnabled(True)

    def _on_host_hidden(self):
        if self._active:
            self._toggle()

    # ── Worker thread ─────────────────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        popout_open = self._popout is not None and self._popout.isVisible()
        if not (self._active or popout_open) or self._busy:
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
