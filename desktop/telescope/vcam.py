"""The virtual camera: opening it, the wait screen it shows while nothing streams, and whether an app is reading it.

Telescope keeps the camera open whenever it runs. While idle the wait screen holds it, so apps list the camera and show
a picture instead of the driver's own "no signal" screen, and a reader starting up can be noticed and answered.
"""

import functools
import glob
import logging
import math
import os
import platform
import struct
import threading
import time
from contextlib import contextmanager
from typing import Callable, Optional

import numpy as np
import pyvirtualcam
from telescope.i18n import tr

logger = logging.getLogger(__name__)

IS_LINUX = platform.system() == "Linux"

VCAM_BACKEND     = "v4l2loopback" if IS_LINUX else "unitycapture"
V4L2_PHONE_DEV   = "/dev/video11"
V4L2_PHONE_LABEL = "Phone Camera"  # its card_label, which is how other apps list it
UC_NAME          = "Telescope"     # the name UnityCapture is registered under (platform/windows.py)

DEFAULT_SIZE   = (1920, 1080)  # before any stream has said how big the camera is (most phones stream 1080p)
IDLE_PERIOD    = 1.0           # seconds between wait-screen frames while nobody is reading
STILL_PERIOD   = 0.2           # a still image while someone reads: enough to look alive, near-free to send
MIN_GIF_PERIOD = 1 / 15        # fastest an animation plays
FRAME_BUDGET   = 64 * 1024 * 1024  # bytes of prepared animation frames kept in memory
FILTER_POLL    = 0.05          # Windows: how often to look for an app's filter while nothing receives the frames
RETRY_OPEN     = 3.0           # seconds before trying a camera that wouldn't open again
PROC_SCAN_PERIOD = 5.0         # old-driver fallback: how often to look for readers in /proc
WATCH_LINGER   = 3.0           # Windows: a reader counts as gone once it hasn't asked for a frame this long
MF_WATCH_PERIOD = 0.5          # Windows 11 camera: how often the watch looks at it


MAX_SLOTS = 4  # virtual cameras streams can go to at once: the usual one and up to three more


def slot_label(slot: int) -> str:
    """What apps list slot's camera as. UnityCapture numbers the extra devices it registers "Telescope #2" and on."""
    if IS_LINUX:
        return V4L2_PHONE_LABEL if slot == 0 else f"{V4L2_PHONE_LABEL} {slot + 1}"
    return UC_NAME if slot == 0 else f"{UC_NAME} #{slot + 1}"


def slot_device(slot: int) -> Optional[str]:
    """Linux: slot's /dev/video node, or None if it isn't set up. The extra ones take whatever number was free, so
    they're found by their label."""
    if slot == 0:
        return V4L2_PHONE_DEV
    label = slot_label(slot)
    for name_file in sorted(glob.glob("/sys/class/video4linux/video*/name")):
        try:
            with open(name_file) as f:
                if f.read().strip() == label:
                    return "/dev/" + os.path.basename(os.path.dirname(name_file))
        except OSError:
            continue
    return None


def slot_ready(slot: int) -> bool:
    """Whether slot's camera is set up to stream to (the first one is whenever Telescope is)."""
    if slot == 0:
        return True
    if IS_LINUX:
        return slot_device(slot) is not None
    return _use_mf(slot) or slot_label(slot) in _uc_names().values()


def open_camera(width: int, height: int, fps: float, fmt=pyvirtualcam.PixelFormat.RGB, slot: int = 0):
    """Open Telescope's virtual camera, or another of its slots (raises if it can't)."""
    def open_(device):
        return pyvirtualcam.Camera(width=width, height=height, fps=fps, fmt=fmt,
                                   backend=VCAM_BACKEND, device=device)
    if IS_LINUX:
        device = slot_device(slot)
        if device is None:
            raise RuntimeError(f"{slot_label(slot)} isn't set up")
        return open_(device)
    if _use_mf(slot):
        return MFCamera(width, height, fps, fmt, slot=slot)
    if slot:
        return open_(slot_label(slot))
    try:
        return open_(UC_NAME)
    except RuntimeError:
        # Registered before it was named Telescope ("Unity Video Capture"): any free one will do.
        return open_(None)


def locked_size(slot: int = 0) -> Optional[tuple]:
    """The size slot's camera is stuck at while an app still reads it, else None. On the Windows 11 camera that's the
    size the app picked; UnityCapture has none.

    v4l2loopback keeps its format while any reader holds buffers, and silently ignores a new writer asking for another
    size, so whoever opens the camera next has to use this size or the reader sees garbage.
    """
    if not IS_LINUX:
        return _mf_locked_size(slot)
    device = slot_device(slot)
    if device is None:
        return None
    try:
        with open(f"/sys/class/video4linux/{os.path.basename(device)}/format") as f:
            fmt = f.read().strip()  # "YU12:1280x720@30", or empty once nothing holds the device
        w, h = fmt.split(":", 1)[1].split("@", 1)[0].split("x")
        return int(w), int(h)
    except (OSError, IndexError, ValueError):
        return None


# ── Letting go for a driver reload (modprobe -r fails while anything holds the device) ──

_gate_lock = threading.Lock()
_released = 0
_holders: list = []


def _register(holder):
    with _gate_lock:
        _holders.append(holder)


def _is_released() -> bool:
    return _released > 0


@contextmanager
def device_released():
    """Everything holding the virtual camera while idle lets go for the block, and takes it back after."""
    global _released
    with _gate_lock:
        _released += 1
        holders = list(_holders)
    for h in holders:
        h._suspend()
    try:
        yield
    finally:
        with _gate_lock:
            _released -= 1
            resume = _released == 0
        if resume:
            for h in holders:
                h._resume()


class _Held:
    """A background job that holds the camera: start()/stop() from any thread, paused by device_released()."""

    def __init__(self):
        self._lock = threading.Lock()
        self._thread: Optional[threading.Thread] = None
        self._stop = threading.Event()
        self._wanted = False
        _register(self)

    def _target(self, stop: threading.Event): ...

    def _alive(self) -> bool:
        # A thread that ended by itself (a failed prepare, a crash) counts as not running, so asking again restarts it
        return self._thread is not None and self._thread.is_alive()

    def _spawn(self):
        if self._alive() or _is_released():
            return
        stop = self._stop = threading.Event()
        self._thread = threading.Thread(target=self._target, args=(stop,), daemon=True)
        self._thread.start()

    def _join(self):
        thread, self._thread = self._thread, None
        if thread is not None:
            self._stop.set()
            self._wake()
            thread.join(timeout=3)
            if thread.is_alive():
                logger.warning("%s did not stop within 3s", type(self).__name__)

    def _wake(self): ...

    def start(self):
        with self._lock:
            self._wanted = True
            self._spawn()

    def stop(self):
        with self._lock:
            self._wanted = False
            self._join()

    @property
    def running(self) -> bool:
        return self._alive()

    def _suspend(self):
        with self._lock:
            self._join()

    def _resume(self):
        with self._lock:
            if self._wanted:
                self._spawn()


# ── The wait screen ───────────────────────────────────────────────────────

def read_image_frames(path: str, width: int, height: int, convert: Callable = None) -> list:
    """The image or animation at path as [(frame, seconds)] fitted to width x height; empty if it won't load. convert
    turns each RGB frame into what gets sent, as it loads; an animation keeps every nth frame if all of them wouldn't
    fit in FRAME_BUDGET."""
    from PyQt6.QtGui import QImageReader

    convert = convert or (lambda f: f)
    frames = []
    reader = QImageReader(path)
    reader.setAutoTransform(True)
    _limit_decode_size(reader, max(width, height))
    animated = reader.supportsAnimation() and reader.imageCount() != 1
    count = reader.imageCount()
    keep_every = 1
    i = 0
    while True:
        img = reader.read()
        if img.isNull():
            break
        seconds = max(reader.nextImageDelay() / 1000, MIN_GIF_PERIOD) if animated else STILL_PERIOD
        if i % keep_every == 0 and (not frames or len(frames) * frames[0][0].nbytes < FRAME_BUDGET):
            frames.append([convert(_fit_image(img, width, height)), seconds])
            if i == 0 and count > 1:
                keep_every = max(1, math.ceil(count * frames[0][0].nbytes / FRAME_BUDGET))
        else:
            frames[-1][1] += seconds
        i += 1
        if not animated:
            break
    if not frames:
        logger.warning("Wait screen image %r didn't load: %s", path, reader.errorString())
    return [(f, s) for f, s in frames]


def _limit_decode_size(reader, longest: int):
    """Have reader decode an image too big for Qt's allocation limit at a size that fits longest px a side, so a huge
    photo loads instead of failing. The longest side (not width x height) so a rotated photo still has pixels to spare;
    images that fit the limit are left alone, as Qt's own scaling is the coarse kind."""
    from PyQt6.QtCore import QSize, Qt

    size = reader.size()
    limit = reader.allocationLimit() * 1024 * 1024
    if size.isValid() and max(size.width(), size.height()) > longest and (
            not limit or size.width() * size.height() * 4 > limit // 2):
        reader.setScaledSize(size.scaled(QSize(longest, longest), Qt.AspectRatioMode.KeepAspectRatio))


def load_frames(path: Optional[str], width: int, height: int, convert: Callable = None) -> list:
    """The wait screen as [(frame, seconds)] fitted to width x height: the image or animation at path, or the default
    screen when there's no path or it won't load."""
    convert = convert or (lambda f: f)
    frames = [[f, s] for f, s in read_image_frames(path, width, height, convert)] if path else []
    if not frames:
        frames = [[convert(_default_screen(width, height)), STILL_PERIOD]]
    if len(frames) == 1:
        frames[0][1] = STILL_PERIOD
    return [(f, s) for f, s in frames]


def _fit_image(img, width: int, height: int) -> np.ndarray:
    """img scaled to fit width x height, centred on the app background (transparency shows it too)."""
    from PyQt6.QtCore import Qt
    from PyQt6.QtGui import QColor, QImage, QPainter
    from telescope.theme import SURFACE_SUNK

    canvas = QImage(width, height, QImage.Format.Format_RGB888)
    canvas.fill(QColor(SURFACE_SUNK))
    scaled = img.scaled(width, height, Qt.AspectRatioMode.KeepAspectRatio,
                        Qt.TransformationMode.SmoothTransformation)
    p = QPainter(canvas)
    p.drawImage((width - scaled.width()) // 2, (height - scaled.height()) // 2, scaled)
    p.end()
    return _to_rgb(canvas)


# The default screen in the project banner's units, which the Windows 11 camera's DLL lays out by too (wait_screen.cpp)
SCREEN_WIDTH = 500       # the ticks' span, at most SCREEN_WIDTH_SHARE of the width
SCREEN_WIDTH_SHARE = 0.8
SCREEN_LENS = 500        # the lens alone, and with the name under it, at most SCREEN_HEIGHT_SHARE of the height
SCREEN_HEIGHT_SHARE = 0.82
SCREEN_WITH_NAME = 654
SCREEN_CENTRE = 250      # the lens's centre below the top of the group
SCREEN_NAME_TOP = 290    # the name's top below the lens's centre
SCREEN_NAME_HEIGHT = 70
SCREEN_NAME_PX = 56      # the name's font size, left out below SCREEN_NAME_MIN_PX
SCREEN_NAME_MIN_PX = 14
SCREEN_REACH = 460       # nothing drawn further than this from the lens's centre: the outer ring and the glow


def screen_layout(width: int, height: int) -> tuple:
    """(scale in pixels per banner unit, the lens's centre, whether the name fits under it) for a width x height screen."""
    fit = lambda group_h: min(width * SCREEN_WIDTH_SHARE / SCREEN_WIDTH, height * SCREEN_HEIGHT_SHARE / group_h)  # noqa: E731
    k = fit(SCREEN_WITH_NAME)
    with_name = SCREEN_NAME_PX * k >= SCREEN_NAME_MIN_PX
    if not with_name:
        k = fit(SCREEN_LENS)
    group_h = (SCREEN_WITH_NAME if with_name else SCREEN_LENS) * k
    return k, (width / 2, (height - group_h) / 2 + SCREEN_CENTRE * k), with_name


def paint_lens(p, centre: tuple, k: float, width: int, height: int):
    """The lens from the project banner, with its glow over the whole width x height."""
    from PyQt6.QtCore import QPointF, Qt
    from PyQt6.QtGui import QBrush, QColor, QPen, QRadialGradient
    from telescope.theme import ACCENT, ACCENT_SOFT, SURFACE, SURFACE_SUNK

    def colour(hex_: str, alpha: float = 1.0) -> QColor:
        c = QColor(hex_)
        c.setAlphaF(alpha)
        return c

    def radial(centre: QPointF, radius: float, stops) -> QBrush:
        g = QRadialGradient(centre, radius)
        for at, c in stops:
            g.setColorAt(at, c)
        return QBrush(g)

    c = QPointF(*centre)
    p.setPen(Qt.PenStyle.NoPen)
    p.setBrush(radial(c, 420 * k, [(0, colour("#6a58cf", 0.34)), (0.55, colour("#6a58cf", 0.08)),
                                   (1, colour("#6a58cf", 0))]))
    p.drawRect(0, 0, width, height)

    p.setBrush(Qt.BrushStyle.NoBrush)
    for r, a in ((300, 0.10), (372, 0.06), (452, 0.035)):
        p.setPen(QPen(colour(ACCENT, a), max(1.0, 1.5 * k)))
        p.drawEllipse(c, r * k, r * k)

    for i in range(120):
        major = i % 10 == 0
        angle = math.radians(i * 3)
        inner, outer = 232 * k, (250 if major else 241) * k
        p.setPen(QPen(colour(ACCENT, 0.55 if major else 0.22), max(1.0, (2 if major else 1.4) * k),
                      cap=Qt.PenCapStyle.RoundCap))
        dx, dy = math.cos(angle), math.sin(angle)
        p.drawLine(QPointF(c.x() + dx * inner, c.y() + dy * inner), QPointF(c.x() + dx * outer, c.y() + dy * outer))

    p.setPen(Qt.PenStyle.NoPen)
    p.setBrush(radial(c + QPointF(-60 * k, -70 * k), 260 * k, [(0, QColor(ACCENT_SOFT)), (1, QColor(ACCENT))]))
    p.drawEllipse(c, 190 * k, 190 * k)
    p.setBrush(radial(c + QPointF(18 * k, 22 * k), 110 * k, [(0, QColor(SURFACE)), (1, QColor(SURFACE_SUNK))]))
    p.drawEllipse(c, 86 * k, 86 * k)
    p.setBrush(colour("#ffffff", 0.16))
    p.save()
    p.translate(c + QPointF(-28 * k, -30 * k))
    p.rotate(-35)
    p.drawEllipse(QPointF(0, 0), 20 * k, 13 * k)
    p.restore()
    p.setBrush(colour("#ffffff", 0.10))
    p.drawEllipse(c + QPointF(26 * k, 30 * k), 5 * k, 5 * k)


def paint_name(p, top: float, k: float, width: int, colour: str):
    """"Telescope", centred across width with its top at top."""
    from PyQt6.QtCore import QRectF, Qt
    from PyQt6.QtGui import QColor, QFont
    name = QFont()
    name.setFamilies(["Inter Display", "Inter", "Segoe UI", "sans-serif"])
    name.setPixelSize(round(SCREEN_NAME_PX * k))
    name.setWeight(QFont.Weight.Bold)
    name.setLetterSpacing(QFont.SpacingType.AbsoluteSpacing, -1.5 * k)
    p.setFont(name)
    p.setPen(QColor(colour))
    p.drawText(QRectF(0, top, width, SCREEN_NAME_HEIGHT * k), Qt.AlignmentFlag.AlignHCenter | Qt.AlignmentFlag.AlignTop,
               "Telescope")


def _default_screen(width: int, height: int) -> np.ndarray:
    """The lens from the project banner, centred (calls crop tiles to squares and circles), with the name under it."""
    from PyQt6.QtCore import QRectF, Qt
    from PyQt6.QtGui import QColor, QFont, QImage, QPainter
    from telescope.theme import BG, TEXT, TEXT_DIM

    canvas = QImage(width, height, QImage.Format.Format_RGB888)
    canvas.fill(QColor(BG))
    p = QPainter(canvas)
    p.setRenderHint(QPainter.RenderHint.Antialiasing)
    p.setRenderHint(QPainter.RenderHint.TextAntialiasing)
    k, centre, with_name = screen_layout(width, height)
    paint_lens(p, centre, k, width, height)
    if not with_name:
        p.end()
        return _to_rgb(canvas)
    top = centre[1] + SCREEN_NAME_TOP * k
    paint_name(p, top, k, width, TEXT)
    note = QFont()
    note.setFamilies(["Inter", "Segoe UI", "sans-serif"])
    note.setPixelSize(max(9, round(22 * k)))
    p.setFont(note)
    p.setPen(QColor(TEXT_DIM))
    p.drawText(QRectF(0, top + 74 * k, width, 40 * k), Qt.AlignmentFlag.AlignHCenter | Qt.AlignmentFlag.AlignTop,
               tr("Waiting for camera"))
    p.end()
    return _to_rgb(canvas)


def _to_rgb(img) -> np.ndarray:
    w, h = img.width(), img.height()
    ptr = img.constBits()
    ptr.setsize(img.sizeInBytes())
    rows = np.frombuffer(ptr, np.uint8).reshape(h, img.bytesPerLine())
    return rows[:, :w * 3].reshape(h, w, 3).copy()


class WaitScreen(_Held):
    """Holds the virtual camera while nothing streams and shows the wait screen on it.

    Cheap on purpose: frames are prepared once, and sent once a second until someone reads the camera. On Linux
    they're prepared in the camera's own format (I420), so a send is a plain write with no conversion.
    """

    def __init__(self, open_camera: Callable = open_camera, loader: Callable = load_frames,
                 locked: Callable = locked_size, filter_probe: Optional[Callable] = None, slot: int = 0):
        super().__init__()
        if slot:  # an extra camera: UnityCapture has no filter to watch for there, so it sends at the idle pace
            open_camera = functools.partial(open_camera, slot=slot)
            locked = functools.partial(locked, slot)
        self._open_camera = open_camera
        self._loader = loader
        self._locked = locked
        self._filter_probe = filter_probe
        self._slot = slot
        self._size = DEFAULT_SIZE
        self._path: Optional[str] = None
        self._mirror = False
        self._watched = False
        self._nudge = threading.Event()

    def show(self, size: tuple, path: Optional[str], mirror: bool = False):
        """Hold the camera at size (width, height) showing path (None: the default screen), flipped left to right if
        mirror (for apps that mirror the camera); restarts if any of them changed."""
        with self._lock:
            same = (size, path, mirror) == (self._size, self._path, self._mirror)
            if self._wanted and self._alive() and same:
                return
            self._join()
            self._size, self._path, self._mirror = size, path, mirror
            self._wanted = True
            self._spawn()

    def set_watched(self, watched: bool):
        """Someone is reading the camera: send often enough to look alive (or play the animation)."""
        self._watched = watched
        self._nudge.set()

    def _wake(self):
        self._nudge.set()

    def _prepare(self, width: int, height: int):
        flip = (lambda f: np.ascontiguousarray(f[:, ::-1])) if self._mirror else (lambda f: f)  # noqa: E731
        if IS_LINUX and width % 2 == 0 and height % 2 == 0:
            import cv2
            to_i420 = lambda f: cv2.cvtColor(flip(f), cv2.COLOR_RGB2YUV_I420)  # noqa: E731
            return pyvirtualcam.PixelFormat.I420, self._loader(self._path, width, height, to_i420)
        return pyvirtualcam.PixelFormat.RGB, self._loader(self._path, width, height, flip)

    def _target(self, stop: threading.Event):
        prepared = None
        complained = False
        while not stop.is_set():
            # An app still reading what the stream sent keeps the camera at that size, so show the screen at it.
            size = self._locked() or self._size
            if prepared is None or prepared[0] != size:
                try:
                    prepared = (size, *self._prepare(*size))
                except Exception:
                    logger.exception("Preparing the wait screen failed")
                    return
            _, fmt, frames = prepared
            try:
                cam = self._open_camera(*size, 30, fmt)
            except Exception as exc:
                if not complained:
                    logger.info("Wait screen can't open the virtual camera yet: %s", exc)
                    complained = True
                stop.wait(RETRY_OPEN)
                continue
            complained = False
            try:
                with cam:
                    self._feed(cam, frames, stop)
            except Exception:
                logger.exception("Wait screen lost the virtual camera")
                stop.wait(RETRY_OPEN)

    def _feed(self, cam, frames: list, stop: threading.Event):
        filter_open = None
        probe = self._filter_probe or _filter_probe_for(self._slot)
        if probe is not None:
            try:
                filter_open = probe()
            except Exception:
                logger.exception("Can't look for the camera's filter; sending at the idle pace")
        i = 0
        checked = time.monotonic()
        while not stop.is_set():
            if time.monotonic() - checked >= IDLE_PERIOD:
                checked = time.monotonic()
                if camera_changed(cam, self._slot):
                    return
            frame, seconds = frames[i]
            self._nudge.clear()
            cam.send(frame)
            if self._watched:
                i = (i + 1) % len(frames)
                self._nudge.wait(seconds)
            elif filter_open is not None and not filter_open():
                # Until an app opens the camera there's nowhere to send, and it shows its own screen until our first
                # frame lands: send the moment it's there (and at the idle pace regardless, should the probe be wrong).
                until = time.monotonic() + IDLE_PERIOD
                while not self._nudge.wait(FILTER_POLL) and not filter_open() and time.monotonic() < until:
                    pass
            else:
                self._nudge.wait(IDLE_PERIOD)


# ── The Windows 11 camera (desktop/virtualcamera; how it works is in docs/architecture.md) ──

MF_SHARED_NAME = "Global\\TelescopeCamera"
MF_MAGIC = 0x4D435354  # "TSCM"
MF_PROTOCOL = 1
MF_NV12 = 1
# The camera's fields (magic to heartbeat), then Telescope's (frame counter to reserved), as in shared_frame.h
_MF_HEADER = struct.Struct("<8IQQ4I")
_MF_COUNTER_AT, _MF_FRAME_AT = 40, 48


def _use_mf(slot: int) -> bool:
    """Whether slot goes to the Windows 11 camera rather than UnityCapture."""
    if IS_LINUX:
        return False
    from telescope.platform import windows
    return windows.use_mf(slot)


def camera_changed(cam, slot: int) -> bool:
    """Windows: cam is the other kind of camera from the one slot now goes to (installed or updated while it was
    open), so whoever holds it should open it again."""
    return not IS_LINUX and isinstance(cam, MFCamera) != _use_mf(slot)


def fit_frame(frame: np.ndarray, width: int, height: int) -> np.ndarray:
    """frame scaled to fit width x height, keeping its shape, with black bars as needed (the same frame if it fits)."""
    import cv2
    fh, fw = frame.shape[:2]
    if (fw, fh) == (width, height):
        return frame
    scale = min(width / fw, height / fh)
    new_w, new_h = int(fw * scale), int(fh * scale)
    if (new_w, new_h) == (width, height):
        return cv2.resize(frame, (width, height), interpolation=cv2.INTER_LINEAR)
    resized = cv2.resize(frame, (new_w, new_h), interpolation=cv2.INTER_LINEAR)
    canvas = np.zeros((height, width) + frame.shape[2:], dtype=frame.dtype)
    x, y = (width - new_w) // 2, (height - new_h) // 2
    canvas[y:y + new_h, x:x + new_w] = resized
    return canvas


class _SharedView:
    """A camera's shared memory, mapped: buf is the writable bytes, close() lets go of them."""

    def __init__(self, buf, close: Callable = lambda: None):
        self.buf = buf
        self._close = close

    def header(self) -> tuple:
        return _MF_HEADER.unpack_from(self.buf, 0)

    def valid(self) -> bool:
        h = self.header()
        return h[0] == MF_MAGIC and h[1] == MF_PROTOCOL and h[2] >= _MF_HEADER.size and h[2] + h[3] <= len(self.buf)

    def reader_size(self) -> Optional[tuple]:
        """The size the app reading the camera picked, if one is reading and it's a size frames fit in."""
        h = self.header()
        w, ht = h[5], h[6]
        if h[4] != 1 or w <= 0 or ht <= 0 or w % 2 or ht % 2 or w * ht * 3 // 2 > h[3]:
            return None
        return w, ht

    def close(self):
        close, self._close = self._close, None
        if close is not None:
            self.buf = None
            close()


def _mf_open(slot: int) -> Optional[_SharedView]:
    """Windows: slot's shared memory while an app reads the camera, else None."""
    import ctypes
    from ctypes import wintypes

    class MemoryInfo(ctypes.Structure):
        _fields_ = [("BaseAddress", ctypes.c_void_p), ("AllocationBase", ctypes.c_void_p),
                    ("AllocationProtect", wintypes.DWORD), ("PartitionId", wintypes.WORD),
                    ("RegionSize", ctypes.c_size_t), ("State", wintypes.DWORD), ("Protect", wintypes.DWORD),
                    ("Type", wintypes.DWORD)]
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.OpenFileMappingW.restype = wintypes.HANDLE
    k32.OpenFileMappingW.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.LPCWSTR)
    k32.MapViewOfFile.restype = ctypes.c_void_p
    k32.MapViewOfFile.argtypes = (wintypes.HANDLE, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, ctypes.c_size_t)
    k32.UnmapViewOfFile.argtypes = (ctypes.c_void_p,)
    k32.VirtualQuery.restype = ctypes.c_size_t
    k32.VirtualQuery.argtypes = (ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t)
    k32.CloseHandle.argtypes = (wintypes.HANDLE,)
    file_map_read_write = 0x0004 | 0x0002
    handle = k32.OpenFileMappingW(file_map_read_write, False, f"{MF_SHARED_NAME}{slot}")
    if not handle:
        return None
    address = k32.MapViewOfFile(handle, file_map_read_write, 0, 0, 0)
    info = MemoryInfo()
    if not address or not k32.VirtualQuery(address, ctypes.byref(info), ctypes.sizeof(info)):
        if address:
            k32.UnmapViewOfFile(address)
        k32.CloseHandle(handle)
        return None
    buf = memoryview((ctypes.c_char * info.RegionSize).from_address(address)).cast("B")

    def close():
        buf.release()
        k32.UnmapViewOfFile(address)
        k32.CloseHandle(handle)
    view = _SharedView(buf, close)
    if not view.valid():
        view.close()
        return None
    return view


def _mf_probe(slot: int) -> Callable[[], bool]:
    """A check for whether an app has the camera open, for the wait screen."""
    def open_now() -> bool:
        view = _mf_open(slot)
        if view is None:
            return False
        view.close()
        return True
    return open_now


def _mf_locked_size(slot: int) -> Optional[tuple]:
    if not _use_mf(slot):
        return None
    view = _mf_open(slot)
    if view is None:
        return None
    try:
        return view.reader_size()
    finally:
        view.close()


class MFCamera:
    """The Windows 11 camera, used like pyvirtualcam.Camera: each frame goes to the shared memory the camera reads, at
    the size the app reading it picked. While no app reads it there's nowhere to send, so frames are dropped."""

    def __init__(self, width: int, height: int, fps: float, fmt=pyvirtualcam.PixelFormat.RGB, slot: int = 0,
                 opener: Callable = None):
        import cv2
        codes = {pyvirtualcam.PixelFormat.RGB: cv2.COLOR_RGB2YUV_I420, pyvirtualcam.PixelFormat.BGR: cv2.COLOR_BGR2YUV_I420}
        if fmt not in codes:
            raise ValueError(f"The Windows 11 camera takes RGB or BGR frames, not {fmt}")
        self.width, self.height, self.fps = width, height, fps
        self.device = slot_label(slot)
        self._to_i420 = codes[fmt]
        self._open = opener or functools.partial(_mf_open, slot)
        self._view: Optional[_SharedView] = None
        self._beat = None
        self._beat_at = 0.0

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()
        return False

    def close(self):
        if self._view is not None:
            self._view.close()
            self._view = None

    def _reader(self) -> Optional[_SharedView]:
        """The shared memory while an app reads the camera. Let go of once it stops (or stops asking for frames), so
        the camera can take it away and the camera watch sees it gone."""
        now = time.monotonic()
        if self._view is not None:
            beat = self._view.header()[8]
            if beat != self._beat:
                self._beat, self._beat_at = beat, now
            if self._view.reader_size() is None or now - self._beat_at > WATCH_LINGER:
                self.close()
        if self._view is None:
            self._view = self._open()
            if self._view is not None:
                self._beat, self._beat_at = self._view.header()[8], now
        return self._view

    def send(self, frame: np.ndarray):
        import cv2
        view = self._reader()
        size = view.reader_size() if view is not None else None
        if size is None:
            return
        w, h = size
        yuv = cv2.cvtColor(np.ascontiguousarray(fit_frame(frame, w, h)), self._to_i420)
        planar = yuv.reshape(-1)
        luma, quarter = w * h, w * h // 4
        header = view.header()
        counter = header[9] + (header[9] & 1)  # odd: a write was cut off half way
        struct.pack_into("<Q", view.buf, _MF_COUNTER_AT, counter + 1)
        data = np.frombuffer(view.buf, np.uint8, count=luma * 3 // 2, offset=header[2])
        data[:luma] = planar[:luma]
        uv = data[luma:].reshape(h // 2, w // 2, 2)
        uv[..., 0] = planar[luma:luma + quarter].reshape(h // 2, w // 2)
        uv[..., 1] = planar[luma + quarter:].reshape(h // 2, w // 2)
        del data, uv  # nothing may point into the mapping once it's closed
        struct.pack_into("<3I", view.buf, _MF_FRAME_AT, w, h, MF_NV12)
        struct.pack_into("<Q", view.buf, _MF_COUNTER_AT, counter + 2)


# ── Whether an app is reading the camera ──────────────────────────────────

_V4L2_EVENT_CLIENT_USAGE = 0x08000000 + 0x08E00000 + 1   # v4l2loopback 0.13+: V4L2_EVENT_PRI_CLIENT_USAGE
_VIDIOC_SUBSCRIBE_EVENT  = (1 << 30) | (32 << 16) | (ord("V") << 8) | 90
_VIDIOC_DQEVENT          = (2 << 30) | (136 << 16) | (ord("V") << 8) | 89
_V4L2_EVENT_SUB_FL_SEND_INITIAL = 1


def camera_holders(device: str, own_pid: int = None) -> list:
    """Other processes with device open (the fallback for drivers without the usage event)."""
    own_pid = os.getpid() if own_pid is None else own_pid
    try:
        rdev = os.stat(device).st_rdev
    except OSError:
        return []
    found = []
    for pid in os.listdir("/proc"):
        if not pid.isdigit() or int(pid) == own_pid:
            continue
        try:
            for fd in os.scandir(f"/proc/{pid}/fd"):
                try:
                    st = os.stat(fd.path)
                except OSError:
                    continue
                if st.st_rdev == rdev and (st.st_mode & 0o170000) == 0o020000:
                    found.append(int(pid))
                    break
        except OSError:
            continue
    return found


def _uc_names() -> dict:
    """Windows: the UnityCapture cameras registered, {number: name}, numbered the way pyvirtualcam numbers them."""
    import sys
    import winreg
    offset = 0x10 if sys.maxsize > 2**32 else 0x20
    names = {}
    for num in range(ord("z") - ord("0")):
        key = rf"CLSID\{{5C2CD55C-92AD-4999-8666-912BD3E700{offset + num + (1 if num else 0):02X}}}"
        try:
            with winreg.OpenKey(winreg.HKEY_CLASSES_ROOT, key) as k:
                registered, _ = winreg.QueryValueEx(k, "")
        except OSError:
            continue
        names[num] = registered.strip()
    return names


def _uc_object_name(kind: str, name: str = UC_NAME) -> str:
    """A UnityCapture shared object's name ("Want": the event an app's filter sets each time it wants a frame; "Data":
    the shared image), for the camera called name (or the first registered one, as open_camera falls back to). Mirrors
    pyvirtualcam's numbering."""
    names = _uc_names()
    num = next((n for n, registered in names.items() if registered == name), next(iter(names), 0))
    return f"UnityCapture_{kind}" + (chr(ord("0") + num) if num else "")


def _filter_probe_for(slot: int) -> Optional[Callable[[], Callable[[], bool]]]:
    """Windows: what tells the wait screen an app has opened slot's camera, so it sends right away, or None."""
    if IS_LINUX:
        return None
    if _use_mf(slot):
        return functools.partial(_mf_probe, slot)
    return None if slot else _uc_filter_probe


def _uc_filter_probe() -> Callable[[], bool]:
    """Windows: a check for whether the camera's shared image exists yet. An app's filter creates it when it opens the
    camera, and until then every send goes nowhere; a sender that has sent once keeps it alive."""
    import ctypes
    from ctypes import wintypes
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.OpenFileMappingW.restype = wintypes.HANDLE
    k32.OpenFileMappingW.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.LPCWSTR)
    k32.CloseHandle.argtypes = (wintypes.HANDLE,)
    file_map_read, name = 0x0004, _uc_object_name("Data")

    def exists() -> bool:
        handle = k32.OpenFileMappingW(file_map_read, False, name)
        if handle:
            k32.CloseHandle(handle)
        return bool(handle)
    return exists


class CameraWatch(_Held):
    """Reports whether an app is reading the virtual camera, from a thread of its own.

    Linux: v4l2loopback (0.13+) sends an event when a reader starts or stops streaming; older drivers fall back to
    looking for other processes holding the device. Windows 11 camera: its shared memory exists while an app reads it,
    and the camera counts each frame it hands over. UnityCapture: the filter inside an app sets a named event whenever
    it wants a frame. on_change(watched) runs on the watch thread, only when the answer changes.
    """

    def __init__(self, on_change: Callable[[bool], None]):
        super().__init__()
        self._on_change = on_change
        self._watched: Optional[bool] = None

    @property
    def watched(self) -> bool:
        return bool(self._watched)

    def _report(self, watched: bool):
        if watched != self._watched:
            self._watched = watched
            try:
                self._on_change(watched)
            except Exception:
                logger.exception("Camera watch callback failed")

    def _target(self, stop: threading.Event):
        try:
            (self._run_linux if IS_LINUX else self._run_windows)(stop)
        except Exception:
            logger.exception("Camera watch stopped")

    def _suspend(self):
        super()._suspend()
        self._report(False)

    def _run_linux(self, stop: threading.Event):
        import fcntl
        import select
        while not stop.is_set():
            try:
                fd = os.open(V4L2_PHONE_DEV, os.O_RDWR | os.O_NONBLOCK)
            except OSError:
                stop.wait(RETRY_OPEN)
                continue
            try:
                sub = struct.pack("8I", _V4L2_EVENT_CLIENT_USAGE, 0, _V4L2_EVENT_SUB_FL_SEND_INITIAL, 0, 0, 0, 0, 0)
                try:
                    fcntl.ioctl(fd, _VIDIOC_SUBSCRIBE_EVENT, sub)
                except OSError:
                    os.close(fd)
                    fd = None
                    logger.info("v4l2loopback has no usage events (older than 0.13); scanning /proc instead")
                    self._scan_proc(stop)
                    return
                poller = select.poll()
                poller.register(fd, select.POLLPRI)
                while not stop.is_set():
                    events = poller.poll(500)
                    if any(ev & (select.POLLERR | select.POLLHUP | select.POLLNVAL) for _, ev in events):
                        break
                    if events:
                        buf = bytearray(136)
                        try:
                            fcntl.ioctl(fd, _VIDIOC_DQEVENT, buf)
                        except OSError:
                            continue
                        if struct.unpack_from("I", buf, 0)[0] == _V4L2_EVENT_CLIENT_USAGE:
                            self._report(struct.unpack_from("I", buf, 8)[0] > 0)
            finally:
                if fd is not None:
                    os.close(fd)
            stop.wait(RETRY_OPEN)

    def _scan_proc(self, stop: threading.Event):
        while not stop.is_set():
            self._report(bool(camera_holders(V4L2_PHONE_DEV)))
            stop.wait(PROC_SCAN_PERIOD)

    def _run_windows(self, stop: threading.Event):
        # Which camera it is can change while Telescope runs (installed from the first-run checklist)
        while not stop.is_set():
            if _use_mf(0):
                self._watch_mf(stop)
            else:
                self._watch_uc(stop)

    def _watch_mf(self, stop: threading.Event, opener: Callable = None):
        opener = opener or functools.partial(_mf_open, 0)
        beat, beat_at = None, 0.0
        while not stop.is_set():
            view = opener()
            if view is None:
                self._report(False)
                beat = None
            else:
                try:
                    reading, new_beat = view.reader_size() is not None, view.header()[8]
                finally:
                    view.close()
                now = time.monotonic()
                if new_beat != beat:
                    beat, beat_at = new_beat, now
                # An app that stopped asking for frames without closing (one hung or paused) counts as gone
                self._report(reading and now - beat_at <= WATCH_LINGER)
            if stop.wait(MF_WATCH_PERIOD) or not _use_mf(0):
                return

    def _watch_uc(self, stop: threading.Event):
        import ctypes
        from ctypes import wintypes
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.OpenEventW.restype = wintypes.HANDLE
        k32.OpenEventW.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.LPCWSTR)
        k32.WaitForSingleObject.argtypes = (wintypes.HANDLE, wintypes.DWORD)
        k32.CloseHandle.argtypes = (wintypes.HANDLE,)
        synchronize, wait_object_0 = 0x00100000, 0
        name = _uc_object_name("Want")
        handle, last_seen = None, 0.0
        try:
            while not stop.is_set():
                if handle is None:
                    # An app's filter creates it when it opens the camera, and a sender holding it keeps it alive.
                    # Until then no app has opened the camera, which is an answer too: without it an idle stop
                    # would never start for a stream started before any app came along.
                    handle = k32.OpenEventW(synchronize, False, name) or None
                    if handle is None:
                        self._report(False)
                        if stop.wait(RETRY_OPEN) or _use_mf(0):
                            return
                        continue
                if k32.WaitForSingleObject(handle, 1000) == wait_object_0:
                    last_seen = time.monotonic()
                    self._report(True)
                    stop.wait(0.5)  # one sighting is enough; don't wake for every frame the app asks for
                elif time.monotonic() - last_seen > WATCH_LINGER:
                    self._report(False)
                if _use_mf(0):  # the Windows 11 camera went in while an app had this one open
                    return
        finally:
            if handle is not None:
                k32.CloseHandle(handle)
