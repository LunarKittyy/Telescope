"""The wait screen: what the virtual camera shows while nothing streams, and whether an app is reading it.

Holds the camera from app start to quit except while a stream has it. The size follows the stream (the Setup canvas,
or the size the last stream opened at) so a call that's already watching keeps working when the phone takes over.
"""

import logging
import shutil
from pathlib import Path
from typing import Optional

from PyQt6.QtCore import QObject, QSize, Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QAction, QImage, QImageReader, QMovie, QPixmap
from PyQt6.QtWidgets import QDialog, QFileDialog, QLabel, QPushButton

from telescope import vcam
from telescope.config import config_path
from telescope.plugin import TelescopePlugin
from telescope.plugins.setup import canvas_dims
from telescope.widgets.common import (
    action_button, button_row, dialog_buttons, dialog_header, dialog_layout, ui_px, wrapped_note,
)

logger = logging.getLogger(__name__)

_PREVIEW_W = 384


class _Signals(QObject):
    watched = pyqtSignal(bool)


class WaitScreenPlugin(TelescopePlugin):
    name = "wait_screen"

    def __init__(self, screen: vcam.WaitScreen = None, watch_cls=vcam.CameraWatch):
        self.image_path: Optional[str] = None
        self.last_size: Optional[tuple] = None
        self._screen = screen or vcam.WaitScreen()
        self._watch_cls = watch_cls
        self._watch = None
        self._dlg = None
        self._shut = False

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._signals = _Signals()
        self._signals.watched.connect(self._on_watched)
        bus.vcam_opened.connect(self._on_vcam_opened)
        self._watch = self._watch_cls(self._signals.watched.emit)
        self._watch.start()
        QTimer.singleShot(0, self._show)  # once every plugin's config is in, for the canvas size

    # ── Holding the camera ────────────────────────────────────────────────

    def size(self) -> tuple:
        w, h = canvas_dims(self._host.plugin_config("setup") or {})
        if w and h:
            return w, h
        return self.last_size or vcam.DEFAULT_SIZE

    def _show(self):
        if self._shut or self._host.is_streaming():
            return
        self._screen.show(self.size(), self.image_path)

    def on_stream_starting(self):
        self._screen.stop()

    def on_stream_stop(self):
        self._show()

    def _on_vcam_opened(self, w: int, h: int):
        if (w, h) != self.last_size:
            self.last_size = (w, h)
            self._host.schedule_save()

    def _on_watched(self, watched: bool):
        self._screen.set_watched(watched)
        self._bus.camera_watched.emit(watched)

    def set_image(self, path: Optional[str]):
        """Show path (kept as a copy next to the config, so moving the original doesn't lose it), or None for the
        default screen."""
        if path:
            try:
                dest = config_path().parent / ("wait_screen" + Path(path).suffix.lower())
                dest.parent.mkdir(parents=True, exist_ok=True)
                for old in dest.parent.glob("wait_screen.*"):
                    if old != dest:
                        old.unlink(missing_ok=True)
                if Path(path).resolve() != dest.resolve():
                    shutil.copyfile(path, dest)
                path = str(dest)
            except OSError:
                logger.exception("Couldn't keep a copy of the wait screen image; using it where it is")
        self.image_path = path or None
        self._host.schedule_save()
        self._show()

    # ── Menu and dialog ───────────────────────────────────────────────────

    def create_menu_actions(self) -> list:
        action = QAction("Wait screen…", None)
        action.triggered.connect(self._open_dialog)
        return [action]

    def _open_dialog(self):
        if self._dlg is None:
            self._dlg = WaitScreenDialog(self)
        self._dlg.refresh()
        self._dlg.show()
        self._dlg.raise_()
        self._dlg.activateWindow()

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"image": self.image_path, "last_size": list(self.last_size) if self.last_size else None}

    def set_config(self, cfg: dict):
        image = cfg.get("image")
        self.image_path = image if isinstance(image, str) and image else None
        size = cfg.get("last_size")
        valid = isinstance(size, list) and len(size) == 2 and all(
            isinstance(v, int) and not isinstance(v, bool) and 16 <= v <= 8192 for v in size)
        self.last_size = tuple(size) if valid else None

    def diagnostics(self) -> dict:
        return {
            "Wait screen": "own image" if self.image_path else "default",
            "An app is reading the camera": "yes" if self._watch is not None and self._watch.watched else "no",
        }

    def shutdown(self):
        self._shut = True
        self._screen.stop()
        if self._watch is not None:
            self._watch.stop()


class WaitScreenDialog(QDialog):
    def __init__(self, plugin: WaitScreenPlugin):
        super().__init__()
        self._plugin = plugin
        self._movie = None
        self.setWindowTitle("Wait screen")
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        lay = dialog_layout(self)
        dialog_header(lay, "Wait screen", "What video calls see while the phone isn't streaming.")

        self._preview = QLabel()
        self._preview.setAlignment(Qt.AlignmentFlag.AlignCenter)
        lay.addWidget(self._preview, alignment=Qt.AlignmentFlag.AlignHCenter)
        self._note = wrapped_note("")
        lay.addWidget(self._note)

        self._choose_btn = action_button("Choose image…", role="primary")
        self._choose_btn.clicked.connect(self._choose)
        self._default_btn = action_button("Use default")
        self._default_btn.clicked.connect(lambda: self._set(None))
        lay.addLayout(button_row(self._choose_btn, self._default_btn))

        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.accept)
        dialog_buttons(lay, close_btn)

    def _preview_size(self) -> QSize:
        w, h = self._plugin.size()
        pw = ui_px(_PREVIEW_W)
        return QSize(pw, max(1, round(pw * h / w)))

    def refresh(self):
        if self._movie is not None:
            self._movie.stop()
            self._movie = None
        box = self._preview_size()
        self._preview.setFixedSize(box)
        path = self._plugin.image_path
        if path and QImageReader(path).supportsAnimation() and QImageReader(path).imageCount() != 1:
            movie = QMovie(path)
            first = QImageReader(path).size()
            if first.isValid():
                movie.setScaledSize(first.scaled(box, Qt.AspectRatioMode.KeepAspectRatio))
            self._preview.setMovie(movie)
            movie.start()
            self._movie = movie
        else:
            frame = vcam.load_frames(path, box.width(), box.height())[0][0]
            h, w = frame.shape[:2]
            img = QImage(frame.data, w, h, w * 3, QImage.Format.Format_RGB888).copy()
            self._preview.setPixmap(QPixmap.fromImage(img))
        w, h = self._plugin.size()
        self._note.setText(f"Shown at {w} × {h}, the size the stream opens the camera at. "
                           "Still images and GIFs work; transparent parts show the dark background.")
        self._default_btn.setEnabled(bool(path))

    def _choose(self):
        formats = " ".join(f"*.{bytes(f).decode()}" for f in QImageReader.supportedImageFormats())
        path, _ = QFileDialog.getOpenFileName(self, "Choose a wait screen", str(Path.home()),
                                              f"Images ({formats})")
        if path:
            self._set(path)

    def _set(self, path: Optional[str]):
        self._plugin.set_image(path)
        self.refresh()
