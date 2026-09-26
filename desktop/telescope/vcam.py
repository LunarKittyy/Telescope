"""The virtual camera: opening it, the wait screen it shows while nothing streams, and whether an app is reading it.

Telescope keeps the camera open whenever it runs. While idle the wait screen holds it, so apps list the camera and show
a picture instead of the driver's own "no signal" screen, and a reader starting up can be noticed and answered.
"""

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

logger = logging.getLogger(__name__)

IS_LINUX = platform.system() == "Linux"

VCAM_BACKEND     = "v4l2loopback" if IS_LINUX else "unitycapture"
V4L2_PHONE_DEV   = "/dev/video11"
V4L2_PHONE_LABEL = "Phone Camera"  # its card_label, which is how other apps list it
UC_NAME          = "Telescope"     # the name UnityCapture is registered under (platform/windows.py)

DEFAULT_SIZE   = (1280, 720)   # before any stream has said how big the camera is
IDLE_PERIOD    = 1.0           # seconds between wait-screen frames while nobody is reading
STILL_PERIOD   = 0.2           # a still image while someone reads: enough to look alive, near-free to send
MIN_GIF_PERIOD = 1 / 15        # fastest an animation plays
FRAME_BUDGET   = 64 * 1024 * 1024  # bytes of prepared animation frames kept in memory
RETRY_OPEN     = 3.0           # seconds before trying a camera that wouldn't open again
PROC_SCAN_PERIOD = 5.0         # old-driver fallback: how often to look for readers in /proc
WATCH_LINGER   = 3.0           # Windows: a reader counts as gone once it hasn't asked for a frame this long


def open_camera(width: int, height: int, fps: float, fmt=pyvirtualcam.PixelFormat.RGB):
    """Open Telescope's virtual camera (raises if it can't)."""
    def open_(device):
        return pyvirtualcam.Camera(width=width, height=height, fps=fps, fmt=fmt,
                                   backend=VCAM_BACKEND, device=device)
    if IS_LINUX:
        return open_(V4L2_PHONE_DEV)
    try:
        return open_(UC_NAME)
    except RuntimeError:
        # Registered before it was named Telescope ("Unity Video Capture"): any free one will do.
        return open_(None)


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

    def _spawn(self):
        if self._thread is not None or _is_released():
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
        return self._thread is not None

    def _suspend(self):
        with self._lock:
            self._join()

    def _resume(self):
        with self._lock:
            if self._wanted:
                self._spawn()


# ── The wait screen ───────────────────────────────────────────────────────

def load_frames(path: Optional[str], width: int, height: int, convert: Callable = None) -> list:
    """The wait screen as [(frame, seconds)] fitted to width x height: the image or animation at path, or the default
    screen when there's no path or it won't load. convert turns each RGB frame into what gets sent, as it loads; an
    animation keeps every nth frame if all of them wouldn't fit in FRAME_BUDGET."""
    from PyQt6.QtGui import QImageReader

    convert = convert or (lambda f: f)
    frames = []
    if path:
        reader = QImageReader(path)
        reader.setAutoTransform(True)
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


def _default_screen(width: int, height: int) -> np.ndarray:
    # Placeholder look; a designed default screen comes later.
    from PyQt6.QtCore import QRect, Qt
    from PyQt6.QtGui import QColor, QFont, QImage, QPainter
    from telescope.theme import SURFACE_SUNK, TEXT, TEXT_DIM

    canvas = QImage(width, height, QImage.Format.Format_RGB888)
    canvas.fill(QColor(SURFACE_SUNK))
    p = QPainter(canvas)
    p.setRenderHint(QPainter.RenderHint.TextAntialiasing)
    unit = min(width, height)
    title, note = QFont(), QFont()
    title.setPixelSize(max(12, unit // 10))
    title.setWeight(QFont.Weight.DemiBold)
    note.setPixelSize(max(8, unit // 28))
    mid = height // 2
    p.setFont(title)
    p.setPen(QColor(TEXT))
    p.drawText(QRect(0, 0, width, mid), Qt.AlignmentFlag.AlignHCenter | Qt.AlignmentFlag.AlignBottom, "Telescope")
    p.setFont(note)
    p.setPen(QColor(TEXT_DIM))
    p.drawText(QRect(0, mid + unit // 40, width, mid), Qt.AlignmentFlag.AlignHCenter | Qt.AlignmentFlag.AlignTop,
               "The camera will be here in a moment")
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

    def __init__(self, open_camera: Callable = open_camera, loader: Callable = load_frames):
        super().__init__()
        self._open_camera = open_camera
        self._loader = loader
        self._size = DEFAULT_SIZE
        self._path: Optional[str] = None
        self._watched = False
        self._nudge = threading.Event()

    def show(self, size: tuple, path: Optional[str]):
        """Hold the camera at size (width, height) showing path (None: the default screen); restarts if either
        changed."""
        with self._lock:
            if self._wanted and self._thread is not None and (size, path) == (self._size, self._path):
                return
            self._join()
            self._size, self._path = size, path
            self._wanted = True
            self._spawn()

    def set_watched(self, watched: bool):
        """Someone is reading the camera: send often enough to look alive (or play the animation)."""
        self._watched = watched
        self._nudge.set()

    def _wake(self):
        self._nudge.set()

    def _prepare(self, width: int, height: int):
        if IS_LINUX and width % 2 == 0 and height % 2 == 0:
            import cv2
            to_i420 = lambda f: cv2.cvtColor(f, cv2.COLOR_RGB2YUV_I420)  # noqa: E731
            return pyvirtualcam.PixelFormat.I420, self._loader(self._path, width, height, to_i420)
        return pyvirtualcam.PixelFormat.RGB, self._loader(self._path, width, height)

    def _target(self, stop: threading.Event):
        width, height = self._size
        try:
            fmt, frames = self._prepare(width, height)
        except Exception:
            logger.exception("Preparing the wait screen failed")
            return
        complained = False
        while not stop.is_set():
            try:
                cam = self._open_camera(width, height, 30, fmt)
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
        i = 0
        while not stop.is_set():
            frame, seconds = frames[i]
            self._nudge.clear()
            cam.send(frame)
            if self._watched:
                i = (i + 1) % len(frames)
                wait = seconds
            else:
                wait = IDLE_PERIOD
            self._nudge.wait(wait)


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


def _uc_want_event_name(name: str = UC_NAME) -> str:
    """The event the UnityCapture filter in an app sets each time it wants a frame, for the camera called name
    (or the first registered one, as open_camera falls back to). Mirrors pyvirtualcam's numbering."""
    import sys
    import winreg
    offset = 0x10 if sys.maxsize > 2**32 else 0x20
    first = None
    for num in range(ord("z") - ord("0")):
        key = rf"CLSID\{{5C2CD55C-92AD-4999-8666-912BD3E700{offset + num + (1 if num else 0):02X}}}"
        try:
            with winreg.OpenKey(winreg.HKEY_CLASSES_ROOT, key) as k:
                registered, _ = winreg.QueryValueEx(k, "")
        except OSError:
            continue
        if first is None:
            first = num
        if registered.strip() == name:
            first = num
            break
    num = first or 0
    return "UnityCapture_Want" + (chr(ord("0") + num) if num else "")


class CameraWatch(_Held):
    """Reports whether an app is reading the virtual camera, from a thread of its own.

    Linux: v4l2loopback (0.13+) sends an event when a reader starts or stops streaming; older drivers fall back to
    looking for other processes holding the device. Windows: the UnityCapture filter inside an app sets a named event
    whenever it wants a frame. on_change(watched) runs on the watch thread, only when the answer changes.
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
        import ctypes
        from ctypes import wintypes
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.OpenEventW.restype = wintypes.HANDLE
        k32.OpenEventW.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.LPCWSTR)
        k32.WaitForSingleObject.argtypes = (wintypes.HANDLE, wintypes.DWORD)
        k32.CloseHandle.argtypes = (wintypes.HANDLE,)
        synchronize, wait_object_0 = 0x00100000, 0
        name = _uc_want_event_name()
        handle, last_seen = None, 0.0
        try:
            while not stop.is_set():
                if handle is None:
                    # It exists while something sends to the camera (the wait screen, or a stream).
                    handle = k32.OpenEventW(synchronize, False, name) or None
                    if handle is None:
                        stop.wait(RETRY_OPEN)
                        continue
                if k32.WaitForSingleObject(handle, 1000) == wait_object_0:
                    last_seen = time.monotonic()
                    self._report(True)
                    stop.wait(0.5)  # one sighting is enough; don't wake for every frame the app asks for
                elif time.monotonic() - last_seen > WATCH_LINGER:
                    self._report(False)
        finally:
            if handle is not None:
                k32.CloseHandle(handle)
