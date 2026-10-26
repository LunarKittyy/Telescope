"""The wait screen: what the virtual camera shows while nothing streams, and whether an app is reading it.

Holds the camera from app start to quit except while a stream has it (a stream with the camera off doesn't). The size
follows the stream (the Setup canvas, or the size the last stream opened at) so a call that's already watching keeps
working when the phone takes over. The extra cameras Add camera sets up show it too while nothing streams to them.
Another plugin can also ask for it in place of the shown stream's picture (cover_requested on the bus).
"""

import logging
import os
import shutil
import threading
import time
from pathlib import Path
from typing import Optional

import cv2
import numpy as np
from PyQt6.QtCore import QObject, QSignalBlocker, QSize, Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QImage, QImageReader, QMovie, QPixmap
from PyQt6.QtWidgets import QFileDialog, QLabel, QVBoxLayout, QWidget

from telescope import vcam
from telescope.config import config_path
from telescope.plugin import SettingsSection, TelescopePlugin
from telescope.widgets.settings_search import keywords
from telescope.widgets.common import (
    Switch,
    action_button, add_card_header, button_row, card_layout, create_card, ui_px, wrapped_note,
)
from telescope.i18n import N_, tr

logger = logging.getLogger(__name__)

_PREVIEW_W = 384
_KEPT_PREFIX = "wait_screen-"  # the copy of the chosen image, next to the config
_RETRY_S = 10.0  # a stand-in that couldn't be made is tried again this long after, not on every frame


class _Signals(QObject):
    watched = pyqtSignal(bool)


class WaitScreenPlugin(TelescopePlugin):
    name = "wait_screen"
    title = N_("Wait screen")
    about = N_("The picture apps see on the virtual camera while nothing is streaming.")
    category = "on_screen"
    icon = "moon"
    frame_stage = "cover"
    when_off = N_("Without it Wait screen isn't in Settings and between streams apps might see a black picture "
                  "or nothing at all.")

    def __init__(self, screen: vcam.WaitScreen = None, watch_cls=vcam.CameraWatch, extra_screen=None,
                 loader=vcam.load_frames):
        self.image_path: Optional[str] = None
        self.mirror = False
        self.last_size: Optional[tuple] = None
        self._screen = screen or vcam.WaitScreen()
        self._extra_screen = extra_screen or (lambda slot: vcam.WaitScreen(slot=slot))
        self._extras: dict = {}  # slot: its WaitScreen, kept once made (each registers for driver reloads)
        self._idle: list = []  # the extra cameras nothing streams to
        self._main_idle = False  # nothing streams to the main camera while others stream to extra ones
        self._watch_cls = watch_cls
        self._watch = None
        self._section: Optional[WaitScreenSection] = None
        self._shut = False
        self._loader = loader
        self._covers: set = set()  # who asked for it in place of the stream's picture
        self._covering = False  # read by the worker thread
        self._cover_frames: dict = {}  # (w, h, image, mirror): ([(BGR frame, seconds)], seconds in all), swapped whole
        self._cover_loading: set = set()  # the keys a thread is making frames for
        self._cover_failed: dict = {}  # key: time.monotonic() its frames last couldn't be made
        self._cover_lock = threading.Lock()

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        bus.cover_requested.connect(self._on_cover_requested)
        self._signals = _Signals()
        self._signals.watched.connect(self._on_watched)
        bus.vcam_opened.connect(self._on_vcam_opened)
        bus.idle_outputs.connect(self._on_idle_outputs)
        self._watch = self._watch_cls(self._signals.watched.emit)
        self._watch.start()
        QTimer.singleShot(0, self._show)  # once every plugin's config is in, for the canvas size

    # ── Holding the camera ────────────────────────────────────────────────

    def size(self) -> tuple:
        w, h = self._host.canvas_size()
        if w and h:
            return w, h
        return self.last_size or vcam.DEFAULT_SIZE

    def _show(self):
        if self._shut:
            return
        self._show_extras()
        if self._host.is_streaming() and self._host.is_camera_on() and not self._main_idle:
            return
        self._screen.show(self.size(), self.image_path, self.mirror)

    def _on_idle_outputs(self, slots: list):
        self._idle = [slot for slot in slots if slot]
        for slot, screen in self._extras.items():
            if slot not in self._idle:
                screen.stop()
        main_idle, self._main_idle = self._main_idle, 0 in slots
        if self._main_idle and not main_idle:
            self._show()
        else:
            self._show_extras()

    def _show_extras(self):
        if self._shut:
            return
        # A stream to an extra camera keeps one size (the canvas, or the default), so its wait screen does too
        w, h = self._host.canvas_size()
        size = (w, h) if w and h else vcam.DEFAULT_SIZE
        for slot in self._idle:
            if slot not in self._extras:
                self._extras[slot] = self._extra_screen(slot)
            self._extras[slot].show(size, self.image_path, self.mirror)

    def on_stream_starting(self):
        self._screen.stop()

    def on_stream_stop(self):
        self._show()

    def on_camera_off(self):
        self._show()  # at the size the stream had, so an app reading the camera keeps its picture

    def _on_vcam_opened(self, w: int, h: int):
        if (w, h) != self.last_size:
            self.last_size = (w, h)
            self._host.schedule_save()

    def _on_watched(self, watched: bool):
        self._screen.set_watched(watched)
        self._bus.camera_watched.emit(watched)

    # ── In place of the stream's picture ──────────────────────────────────

    def _on_cover_requested(self, owner: str, on: bool):
        (self._covers.add if on else self._covers.discard)(owner)
        self._covering = bool(self._covers)
        if self._covering:
            w, h = self.size()
            self._make_cover((w, h, self.image_path, self.mirror))  # likely the stream's size: ready by the first frame
        self._bus.stream_covered.emit(self._covering)  # every time, so whoever asked hears back straight away

    def process_frame(self, frame: np.ndarray) -> np.ndarray:
        return self._cover(frame) if self._covering else frame

    def frame_step(self):
        return self._cover if self._covering else None

    def _make_cover(self, key: tuple):
        """Make the frames for key (w, h, image, mirror) on a thread: a long animation takes a while to load."""
        with self._cover_lock:
            if key in self._cover_frames or key in self._cover_loading:
                return
            if time.monotonic() - self._cover_failed.get(key, float("-inf")) < _RETRY_S:
                return
            self._cover_loading.add(key)
        threading.Thread(target=self._load_cover, args=(key,), daemon=True, name="wait-screen-cover").start()

    def _load_cover(self, key: tuple):
        w, h, path, mirror = key
        frames = []
        try:
            flip = (lambda f: f[:, ::-1]) if mirror else (lambda f: f)  # noqa: E731
            frames = [(np.ascontiguousarray(cv2.cvtColor(flip(f), cv2.COLOR_RGB2BGR)), max(s, 0.01))
                      for f, s in self._loader(path, w, h)]
            if not frames:
                logger.warning("The wait screen for the stream came out empty")
        except Exception:
            logger.exception("Couldn't make the wait screen for the stream")
        with self._cover_lock:
            if frames:
                kept = {k: v for k, v in self._cover_frames.items() if k[2:] == key[2:]}  # this picture's sizes
                kept[key] = (frames, sum(s for _f, s in frames))
                self._cover_frames = dict(list(kept.items())[-2:])
                self._cover_failed.pop(key, None)
            else:
                self._cover_failed[key] = time.monotonic()
            self._cover_loading.discard(key)

    def _cover(self, frame: np.ndarray) -> np.ndarray:
        """The wait screen at frame's size, as the virtual camera shows it (animations play); black while it's being
        made."""
        h, w = frame.shape[:2]
        key = (w, h, self.image_path, self.mirror)
        cached = self._cover_frames.get(key)
        if cached is None:
            self._make_cover(key)
            return np.zeros_like(frame)
        frames, total = cached
        at = time.monotonic() % total
        for picture, seconds in frames:
            if at < seconds:
                return picture.copy()
            at -= seconds
        return frames[-1][0].copy()

    def set_image(self, path: Optional[str]) -> Optional[str]:
        """Show path (kept as a copy next to the config, so moving the original doesn't lose it), or None for the
        default screen. A pick that can't be kept leaves the current image alone and returns why."""
        if path:
            try:
                path = str(self._keep_copy(path))
            except OSError as exc:
                logger.exception("Couldn't keep a copy of the wait screen image")
                return exc.strerror or str(exc)
        self.image_path = path or None
        self._host.schedule_save()
        self._show()
        return None

    @staticmethod
    def _keep_copy(path: str) -> Path:
        """path copied to a fresh wait_screen-<id><ext> next to the config; the previous copy goes only once the new
        one is in place, and a new name makes the camera reload even for the same file type."""
        folder = config_path().parent
        folder.mkdir(parents=True, exist_ok=True)
        if Path(path).resolve().parent == folder.resolve() and Path(path).name.startswith(_KEPT_PREFIX):
            return Path(path)  # already one of ours
        dest = folder / f"{_KEPT_PREFIX}{os.urandom(8).hex()}{Path(path).suffix.lower()}"  # time_ns() repeats on Windows
        part = dest.with_name(dest.name + ".part")
        try:
            shutil.copyfile(path, part)
            os.replace(part, dest)
        except OSError:
            part.unlink(missing_ok=True)
            raise
        for old in [*folder.glob(_KEPT_PREFIX + "*"), *folder.glob("wait_screen.*")]:  # wait_screen.<ext>: older versions
            if old != dest:
                old.unlink(missing_ok=True)
        return dest

    def set_mirror(self, on: bool):
        self.mirror = on
        self._host.schedule_save()
        self._show()

    # ── Menu and dialog ───────────────────────────────────────────────────

    def create_settings_sections(self) -> list:
        self._section = WaitScreenSection(self)
        return [SettingsSection("streaming", self._section, order=10, on_show=self._section.refresh)]

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"image": self.image_path, "mirror": self.mirror,
                "last_size": list(self.last_size) if self.last_size else None}

    def set_config(self, cfg: dict):
        image = cfg.get("image")
        self.image_path = image if isinstance(image, str) and image else None
        self.mirror = cfg.get("mirror") is True
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
        for screen in self._extras.values():
            screen.stop()
        if self._watch is not None:
            self._watch.stop()


class WaitScreenSection(QWidget):
    def __init__(self, plugin: WaitScreenPlugin):
        super().__init__()
        self._plugin = plugin
        self._movie = None
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Wait screen"), "image", hue="stream")
        keywords(card, tr("idle standby placeholder picture image gif black screen frozen not streaming mirror flip "
                          "custom background logo", ctx="search"))
        lay.addWidget(wrapped_note(tr("What video calls see while the phone isn't streaming.")))
        outer.addWidget(card)

        self._preview = QLabel()
        self._preview.setAlignment(Qt.AlignmentFlag.AlignCenter)
        lay.addWidget(self._preview, alignment=Qt.AlignmentFlag.AlignHCenter)
        self._note = wrapped_note("")
        lay.addWidget(self._note)

        self._choose_btn = action_button(tr("Choose image…"), role="primary")
        self._choose_btn.clicked.connect(self._choose)
        self._default_btn = action_button(tr("Use default"))
        self._default_btn.clicked.connect(lambda: self._set(None))
        lay.addLayout(button_row(self._choose_btn, self._default_btn))
        self._mirror_box = Switch(tr("Mirror it, for apps that flip the camera"))
        self._mirror_box.toggled.connect(self._plugin.set_mirror)
        lay.addWidget(self._mirror_box)

    def _preview_size(self) -> QSize:
        w, h = self._plugin.size()
        pw = ui_px(_PREVIEW_W)
        return QSize(pw, max(1, round(pw * h / w)))

    def refresh(self, pick_error: str = ""):
        """Redraw; pick_error is why the image just chosen couldn't be kept, if it couldn't."""
        if self._movie is not None:
            self._movie.stop()
            self._movie = None
        box = self._preview_size()
        self._preview.setFixedSize(box)
        path = self._plugin.image_path
        unreadable = False
        if path and QImageReader(path).supportsAnimation() and QImageReader(path).imageCount() != 1:
            movie = QMovie(path)
            first = QImageReader(path).size()
            if first.isValid():
                movie.setScaledSize(first.scaled(box, Qt.AspectRatioMode.KeepAspectRatio))
            self._preview.setMovie(movie)
            movie.start()
            self._movie = movie
            unreadable = not movie.isValid()
        else:
            frames = vcam.read_image_frames(path, box.width(), box.height()) if path else []
            unreadable = bool(path) and not frames
            frame = frames[0][0] if frames else vcam.load_frames(None, box.width(), box.height())[0][0]
            h, w = frame.shape[:2]
            img = QImage(frame.data, w, h, w * 3, QImage.Format.Format_RGB888).copy()
            self._preview.setPixmap(QPixmap.fromImage(img))
        w, h = self._plugin.size()
        self._note.setText(tr("Shown at {size}, the size the stream opens the camera at. Still images and GIFs "
                              "work; transparent parts show the dark background.", size=f"{w} × {h}"))
        if pick_error:
            self._note.setText(tr("Couldn't use that image: {problem}. Keeping the current wait screen.",
                                  problem=pick_error))
        elif unreadable:
            self._note.setText(tr("Couldn't read that image, so the default screen is showing. Choose another one."))
        self._default_btn.setEnabled(bool(path))
        with QSignalBlocker(self._mirror_box):
            self._mirror_box.setChecked(self._plugin.mirror)

    def _choose(self):
        formats = " ".join(f"*.{bytes(f).decode()}" for f in QImageReader.supportedImageFormats())
        path, _ = QFileDialog.getOpenFileName(self, tr("Choose a wait screen"), str(Path.home()),
                                              tr("Images ({formats})", formats=formats))
        if path:
            self._set(path)

    def _set(self, path: Optional[str]):
        self.refresh(self._plugin.set_image(path) or "")
