import cv2
import numpy as np
from PyQt6.QtCore import Qt, QEvent, QObject, QSize, pyqtSignal
from PyQt6.QtGui import QImage, QPixmap
from PyQt6.QtWidgets import (
    QFrame, QHBoxLayout, QLabel, QPushButton, QSizePolicy, QVBoxLayout, QWidget,
)

from telescope import theme
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import create_vector_icon, set_ui_role


_IDLE_TEXT    = "Not streaming"
_WAITING_TEXT = "Waiting for the first frame\u2026"


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
    panel_region = "center"

    # Max width sent across thread for the in-window preview. The stage is
    # the widest thing on screen now, so this is larger than it was when the
    # preview was a ~400px card.
    _CARD_MAX_W = 960

    def setup(self, host, bus):
        self._host   = host
        # On by default: the preview is the centre of the window now, not an
        # opt-in extra. The toggle stays as an escape hatch for anyone who'd
        # rather not spend the decode.
        self._active = True
        self._popout: _PopoutWindow | None = None
        # Plain flag mirroring "popout is open", updated only from GUI-thread
        # slots. process_frame() runs on the stream reader thread and must
        # never touch self._popout (a QWidget) directly - Qt widgets are not
        # thread-safe, and the popout can also be closed between reads.
        self._popout_active = False
        self._busy   = False
        self._source_size: tuple[int, int] = (0, 0)
        self._sig    = _Sig()
        self._sig.frame.connect(self._on_frame)

        self._host_filter = _HostFilter()
        self._host_filter.hidden.connect(self._on_host_hidden)
        host.installEventFilter(self._host_filter)

    def create_panel(self) -> QWidget:
        """The video stage: a letterboxed frame area with status badges over
        it and a toolbar beneath. It's the centre column, so unlike the rail
        panels it has no card header competing with the picture."""
        stage = QFrame()
        stage.setObjectName("preview_stage")
        stage.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding)
        lay = QVBoxLayout(stage)
        lay.setContentsMargins(1, 1, 1, 1)
        lay.setSpacing(0)

        self._preview_lbl = QLabel()
        self._preview_lbl.setObjectName("preview_surface")
        self._preview_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._preview_lbl.setMinimumHeight(240)
        self._preview_lbl.setSizePolicy(
            QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding)
        self._preview_lbl.setText(_IDLE_TEXT)
        lay.addWidget(self._preview_lbl, 1)

        # Badges float over the frame rather than taking layout space, so the
        # picture is never pushed around by them appearing.
        self._live_badge = QLabel("LIVE", self._preview_lbl)
        self._live_badge.setObjectName("preview_badge")
        self._live_badge.setProperty("live", True)
        self._live_badge.move(12, 12)
        self._live_badge.setVisible(False)

        self._res_badge = QLabel("", self._preview_lbl)
        self._res_badge.setObjectName("preview_badge")
        self._res_badge.setVisible(False)

        toolbar = QWidget()
        toolbar.setObjectName("preview_toolbar")
        tb_lay = QHBoxLayout(toolbar)
        tb_lay.setContentsMargins(12, 9, 12, 10)
        tb_lay.setSpacing(8)

        self._toggle_btn = QPushButton("Hide")
        self._toggle_btn.setMinimumWidth(78)
        set_ui_role(self._toggle_btn, "quiet")
        self._toggle_btn.setToolTip(
            "Stop decoding frames for this view. The virtual camera output is "
            "unaffected either way."
        )
        self._toggle_btn.clicked.connect(self._toggle)
        tb_lay.addWidget(self._toggle_btn)

        tb_lay.addStretch()

        self._popout_btn = QPushButton("  Pop out")
        self._popout_btn.setMinimumWidth(92)
        set_ui_role(self._popout_btn, "quiet")
        self._popout_btn.setIcon(create_vector_icon("expand", theme.TEXT_DIM))
        self._popout_btn.setIconSize(QSize(14, 14))
        self._popout_btn.clicked.connect(self._open_popout)
        tb_lay.addWidget(self._popout_btn)

        lay.addWidget(toolbar)

        return stage

    def _toggle(self):
        self._active = not self._active
        self._toggle_btn.setText("Hide" if self._active else "Show")
        if not self._active:
            self._preview_lbl.setPixmap(QPixmap())
            self._preview_lbl.setText("Preview hidden")
            self._res_badge.setVisible(False)
        else:
            self._preview_lbl.setText(
                _WAITING_TEXT if self._host.is_streaming() else _IDLE_TEXT)

    def on_stream_start(self, stream_url: str, ctrl):
        self._live_badge.setVisible(True)
        if self._active and self._preview_lbl.pixmap().isNull():
            self._preview_lbl.setText(_WAITING_TEXT)

    def on_stream_stop(self):
        self._live_badge.setVisible(False)
        self._res_badge.setVisible(False)
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
        self._popout.resize(640, 360)
        self._popout.show()
        self._popout_active = True

    def _on_popout_closed(self):
        self._popout = None
        self._popout_active = False
        self._toggle_btn.setEnabled(True)

    def _on_host_hidden(self):
        if self._active:
            self._toggle()

    # ── Worker thread ─────────────────────────────────────────────────────────

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        popout_open = self._popout_active
        if not (self._active or popout_open) or self._busy:
            return frame
        self._busy = True
        h, w = frame.shape[:2]
        # Recorded here, before any downscale, so the badge reports the real
        # output size rather than whatever fits the label. A plain tuple
        # written from this thread and read from the GUI thread - same
        # pattern the transforms plugin uses for its settings.
        self._source_size = (w, h)
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
            self._update_res_badge()
        self._busy = False

    def _update_res_badge(self):
        """Pin the resolution badge to the frame's top-right. Repositioned
        per frame rather than on a resize hook - it's a move() on a label
        that's already being repainted anyway."""
        src_w, src_h = self._source_size
        if not src_w:
            return
        self._res_badge.setText(f"{src_w} × {src_h}")
        self._res_badge.adjustSize()
        self._res_badge.move(self._preview_lbl.width() - self._res_badge.width() - 12, 12)
        self._res_badge.setVisible(True)
        self._res_badge.raise_()
