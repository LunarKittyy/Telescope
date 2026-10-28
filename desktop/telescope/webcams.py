"""Webcams plugged into this computer, for the Webcam plugin: finding them, picking how to capture, and reading frames.

Linux finds them in sysfs and asks each V4L2 node what it is and which formats it has; Windows asks Qt Multimedia
and finds each camera's index in OpenCV's DirectShow backend by its device path (webcams_win.py). Frames come through
OpenCV, which is already a dependency and has both backends built in. Telescope's own virtual cameras and other loopback
or virtual ones are never listed, so a stream can't feed back into itself. main.py sets the OpenCV options webcams need.
"""

import logging
import os
import re
import struct
from dataclasses import dataclass, field, replace
from typing import Callable, Optional

import cv2

from telescope import vcam
from telescope.i18n import N_
from telescope.jpeg_guard import decode_jpeg

logger = logging.getLogger(__name__)

SYSFS = "/sys/class/video4linux"
BY_ID = "/dev/v4l/by-id"
BY_PATH = "/dev/v4l/by-path"
# Lists loopback cameras that aren't Telescope's too, for trying the Webcam plugin with fake cameras (docs/building.md)
ALLOW_LOOPBACK_ENV = "TELESCOPE_WEBCAM_LOOPBACK"

AUTO_LONGEST = 1920  # Auto takes the largest size up to 1080p that keeps the frame rate
COMPRESSED = ("MJPG", "JPEG")
RAW = ("YUYV", "UYVY", "NV12", "YU12", "YV12", "BGR3", "RGB3", "GREY")
FPS_STEPS = (5, 10, 15, 20, 24, 25, 30, 50, 60)  # rates offered where a camera gives a range rather than a list
STEP_SIZES = ((640, 480), (1280, 720), (1920, 1080), (2560, 1440), (3840, 2160))  # same, for sizes
_FPS_SLACK = 0.5  # 29.97 counts as 30

# Linux drivers that make virtual cameras rather than talk to hardware
VIRTUAL_DRIVERS = ("v4l2 loopback", "akvcam")
# Windows names that are virtual cameras: Telescope's (vcam.slot_label), its old UnityCapture name and anything
# calling itself virtual (OBS Virtual Camera, the Windows 11 ones)
_UC_OLD_NAME = "Unity Video Capture"
_VIRTUAL_WORDS = re.compile(r"virtual|vcam", re.IGNORECASE)

GONE_TEXT = N_("The webcam isn't plugged in. Plug it back in and it carries on.")
BUSY_TEXT = N_("Another app is using the webcam. Close it there and it starts here.")
NO_FRAMES_TEXT = N_("The webcam isn't sending anything. Try another resolution or frame rate.")


@dataclass(frozen=True)
class Mode:
    """One way a camera can send video: a pixel format at a size, with the frame rates it offers there."""
    fourcc: str
    width: int
    height: int
    fps: tuple = ()

    @property
    def compressed(self) -> bool:
        return self.fourcc in COMPRESSED

    def max_fps(self) -> float:
        return max(self.fps, default=0.0)


@dataclass(frozen=True)
class Choice:
    """What to ask a camera for."""
    fourcc: str
    width: int
    height: int
    fps: float

    @property
    def compressed(self) -> bool:
        return self.fourcc in COMPRESSED

    def describe(self) -> str:
        kind = "MJPEG" if self.compressed else ("" if self.fourcc == "RAW" else self.fourcc)
        rate = f"{self.fps:g} fps" if self.fps else ""
        return ", ".join(p for p in (f"{self.width}×{self.height}", kind, rate) if p)


@dataclass(frozen=True)
class Device:
    """A webcam as it's plugged in now. key stays the same across replugging; location is what OpenCV opens."""
    key: str
    name: str
    location: object  # Linux: its /dev/video node; Windows: its index
    modes: tuple = field(default=(), compare=False)


# ── Choosing a mode ──────────────────────────────────────────────────────────

def sizes(modes) -> list:
    """Every size the camera sends in a format we read, largest first."""
    return sorted({(m.width, m.height) for m in modes}, key=lambda s: (s[0] * s[1], s[0]), reverse=True)


def rates(modes, size: Optional[tuple]) -> list:
    """The frame rates offered at size (or at any size), rounded, highest first."""
    found = {round(f) for m in modes if size is None or (m.width, m.height) == size for f in m.fps if f > 0}
    return sorted(found, reverse=True)


def _auto_size(modes, fps: float) -> Optional[tuple]:
    """The largest size up to AUTO_LONGEST across that reaches fps, or failing that the one that gets closest."""
    fitting = [s for s in sizes(modes) if max(s) <= AUTO_LONGEST] or sizes(modes)[-1:]
    for size in fitting:
        if any(m.max_fps() + _FPS_SLACK >= fps for m in modes if (m.width, m.height) == size):
            return size
    return max(fitting, key=lambda s: max(m.max_fps() for m in modes if (m.width, m.height) == s), default=None)


def _nearest_size(modes, want: tuple) -> Optional[tuple]:
    """want if it's offered, else the smallest bigger one, else the biggest."""
    have = sizes(modes)
    if want in have:
        return want
    bigger = [s for s in have if s[0] >= want[0] and s[1] >= want[1]]
    return bigger[-1] if bigger else (have[0] if have else None)


def choose(modes, size: Optional[tuple], fps: float) -> Optional[Choice]:
    """What to ask for: size (None for Auto) at fps. Raw when it reaches the rate there, since it costs nothing to
    decode; MJPEG when raw can't (USB 2 can't carry raw 1080p at 30 fps), and failing both the fastest there is."""
    modes = [m for m in modes if m.fourcc in COMPRESSED + RAW or m.fourcc == "RAW"]
    if not modes:
        return None
    size = _auto_size(modes, fps) if size is None else _nearest_size(modes, tuple(size))
    here = [m for m in modes if (m.width, m.height) == size]

    def reaches(m: Mode) -> bool:
        return not m.fps or m.max_fps() + _FPS_SLACK >= fps

    best = (next((m for m in here if not m.compressed and reaches(m)), None)
            or next((m for m in here if m.compressed and reaches(m)), None)
            or max(here, key=lambda m: (m.max_fps(), m.compressed)))
    offered = sorted(best.fps)
    rate = next((f for f in offered if f + _FPS_SLACK >= fps), offered[-1] if offered else fps)
    return Choice(best.fourcc, best.width, best.height, float(round(rate, 2)))


# ── Which devices are cameras of our own ─────────────────────────────────────

def own_names() -> set:
    """What Telescope's own virtual cameras are called on this system."""
    names = {vcam.slot_label(slot) for slot in range(vcam.MAX_SLOTS)}
    if not vcam.IS_LINUX:
        names.add(_UC_OLD_NAME)
    return names


def is_virtual(name: str, driver: str = "") -> bool:
    """Whether a camera is a virtual one rather than hardware: Telescope's own always, others where they say so."""
    if name in own_names() or driver in VIRTUAL_DRIVERS:
        return True
    return bool(_VIRTUAL_WORDS.search(name))


def tidy_name(name: str) -> str:
    """A V4L2 card name without the cut-off repeat some cameras add ("HP 320 FHD Webcam: HP 320 FHD W")."""
    name = name.strip()
    head, sep, tail = name.partition(": ")
    if sep and tail.strip() and head.startswith(tail.strip()):
        return head
    return name


def tell_apart(devices: list) -> list:
    """Two cameras of one model get "(2)" and on after the first, in the order they're listed."""
    seen: dict = {}
    out = []
    for dev in devices:
        seen[dev.name] = seen.get(dev.name, 0) + 1
        name = dev.name if seen[dev.name] == 1 else f"{dev.name} ({seen[dev.name]})"
        out.append(Device(dev.key, name, dev.location, dev.modes))
    return out


# ── Linux: V4L2 ──────────────────────────────────────────────────────────────

def _iowr(nr: int, size: int, read_only: bool = False) -> int:
    return ((2 if read_only else 3) << 30) | (size << 16) | (ord("V") << 8) | nr


_CAP = struct.Struct("16s32s32s3I12x")  # struct v4l2_capability
_FMTDESC = struct.Struct("3I32s2I12x")  # struct v4l2_fmtdesc
_FRMSIZE = struct.Struct("3I6I8x")  # struct v4l2_frmsizeenum
_FRMIVAL = struct.Struct("5I6I8x")  # struct v4l2_frmivalenum
VIDIOC_QUERYCAP = _iowr(0, _CAP.size, read_only=True)
VIDIOC_ENUM_FMT = _iowr(2, _FMTDESC.size)
VIDIOC_ENUM_FRAMESIZES = _iowr(74, _FRMSIZE.size)
VIDIOC_ENUM_FRAMEINTERVALS = _iowr(75, _FRMIVAL.size)
CAP_VIDEO_CAPTURE = 0x1
CAP_DEVICE_CAPS = 0x80000000
BUF_TYPE_VIDEO_CAPTURE = 1
_DISCRETE, _CONTINUOUS, _STEPWISE = 1, 2, 3


def _ioctl(fd: int, request: int, packed: bytes) -> Optional[bytes]:
    import fcntl
    buf = bytearray(packed)
    try:
        fcntl.ioctl(fd, request, buf)
    except OSError:
        return None
    return bytes(buf)


def _text(raw: bytes) -> str:
    return raw.split(b"\0", 1)[0].decode("utf-8", "replace").strip()


def _fourcc(code: int) -> str:
    return code.to_bytes(4, "little").decode("ascii", "replace").strip()


def query_caps(fd: int) -> Optional[tuple]:
    """(driver, card name, whether it captures video) of an open V4L2 node."""
    raw = _ioctl(fd, VIDIOC_QUERYCAP, bytes(_CAP.size))
    if raw is None:
        return None
    driver, card, _bus, _version, caps, device_caps = _CAP.unpack(raw)
    caps = device_caps if caps & CAP_DEVICE_CAPS else caps
    return _text(driver), _text(card), bool(caps & CAP_VIDEO_CAPTURE)


def _rates(fd: int, code: int, width: int, height: int) -> tuple:
    out = []
    for index in range(64):
        raw = _ioctl(fd, VIDIOC_ENUM_FRAMEINTERVALS, _FRMIVAL.pack(index, code, width, height, 0, *[0] * 6))
        if raw is None:
            break
        kind, *u = _FRMIVAL.unpack(raw)[4:]
        if kind == _DISCRETE:
            if u[0]:
                out.append(round(u[1] / u[0], 2))
            continue
        # A range: from its shortest interval (fastest) to its longest
        fastest = u[1] / u[0] if u[0] else 0
        slowest = u[3] / u[2] if u[2] else 0
        out.extend(f for f in FPS_STEPS if slowest - _FPS_SLACK <= f <= fastest + _FPS_SLACK)
        break
    return tuple(sorted(set(out)))


def list_modes(fd: int) -> tuple:
    """Every Mode an open V4L2 node offers in a format OpenCV reads."""
    modes = []
    for index in range(32):
        raw = _ioctl(fd, VIDIOC_ENUM_FMT, _FMTDESC.pack(index, BUF_TYPE_VIDEO_CAPTURE, 0, b"", 0, 0))
        if raw is None:
            break
        code = _FMTDESC.unpack(raw)[4]
        fourcc = _fourcc(code)
        if fourcc not in COMPRESSED + RAW:
            continue
        for s in range(64):
            raw = _ioctl(fd, VIDIOC_ENUM_FRAMESIZES, _FRMSIZE.pack(s, code, 0, *[0] * 6))
            if raw is None:
                break
            kind, *u = _FRMSIZE.unpack(raw)[2:]
            if kind == _DISCRETE:
                found = [(u[0], u[1])]
            else:  # a range: the usual sizes inside it
                found = [(w, h) for w, h in STEP_SIZES if u[0] <= w <= u[1] and u[3] <= h <= u[4]]
            for w, h in found:
                modes.append(Mode(fourcc, w, h, _rates(fd, code, w, h)))
            if kind != _DISCRETE:
                break
    return tuple(modes)


def _read(path: str) -> str:
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError:
        return ""


def _stable_keys() -> dict:
    """/dev/videoN: the name udev gives it under /dev/v4l, which stays the same when it's plugged in again."""
    keys: dict = {}
    for folder in (BY_PATH, BY_ID):  # by-id last, so it wins: it follows the camera to another port
        try:
            names = os.listdir(folder)
        except OSError:
            continue
        for name in names:
            target = os.path.realpath(os.path.join(folder, name))
            keys[target] = re.sub(r"^usb-|-video-index\d+$", "", name)
    return keys


def _usb_key(folder: str) -> str:
    """A key from sysfs for a USB camera: its IDs, serial and interface, or its port when it has no serial ("" if it isn't USB)."""
    iface = os.path.realpath(os.path.join(folder, "device"))
    usb = os.path.dirname(iface)
    vendor, product = _read(os.path.join(usb, "idVendor")), _read(os.path.join(usb, "idProduct"))
    if not vendor or not product:
        return ""
    serial = _read(os.path.join(usb, "serial"))
    if serial:  # udev's by-id link can't tell two interfaces of one camera apart, so the interface goes in too
        return f"usb-{vendor}:{product}-{serial}-if{_read(os.path.join(iface, 'bInterfaceNumber')) or '00'}"
    # Identical cameras without a serial share a by-id name, so the port is what tells them apart
    return "usb-port-" + os.path.relpath(iface, "/sys/devices").replace("/", "-")


class LinuxScanner:
    """Lists webcams from sysfs, opening each node only when it first shows up (opening wakes a sleeping camera)."""

    def __init__(self, sysfs: str = SYSFS, dev: str = "/dev", allow_loopback: Optional[bool] = None,
                 open_node: Callable = None, stable_keys: Callable = _stable_keys):
        self._sysfs, self._dev = sysfs, dev
        self._allow_loopback = (os.environ.get(ALLOW_LOOPBACK_ENV) == "1") if allow_loopback is None else allow_loopback
        self._open = open_node or self._open_node
        self._stable_keys = stable_keys
        self._known: dict = {}  # node: (its sysfs identity, Device or None)

    @staticmethod
    def _open_node(path: str):
        """(caps, modes) of a node, or None if it can't be opened."""
        try:
            fd = os.open(path, os.O_RDWR | os.O_NONBLOCK)
        except OSError:
            return None
        try:
            caps = query_caps(fd)
            return (caps, list_modes(fd) if caps and caps[2] else ())
        finally:
            os.close(fd)

    @staticmethod
    def _key(folder: str, path: str, node: str, keys: dict) -> str:
        return _usb_key(folder) or keys.get(os.path.realpath(path)) or node

    def scan(self) -> list:
        try:
            nodes = sorted(n for n in os.listdir(self._sysfs) if n.startswith("video"))
        except OSError:
            nodes = []
        nodes.sort(key=lambda n: int(n[5:]) if n[5:].isdigit() else 0)
        keys = None
        known, found = {}, []
        for node in nodes:
            folder = os.path.join(self._sysfs, node)
            name = _read(os.path.join(folder, "name"))
            # v4l2loopback's own sysfs files tell its devices apart without opening them
            loopback = os.path.exists(os.path.join(folder, "max_openers"))
            if is_virtual(name) or (loopback and not self._allow_loopback):
                continue
            try:
                ident = (os.path.realpath(folder), os.stat(folder).st_ino, _read(os.path.join(folder, "dev")))
            except OSError:
                continue
            cached = self._known.get(node)
            if cached is not None and cached[0] == ident and not loopback:  # a loopback's caps follow its writer
                if cached[1] is not None and cached[1].key == node:  # udev hadn't made its link yet: look again
                    if keys is None:
                        keys = self._stable_keys()
                    cached = (ident, replace(cached[1], key=self._key(folder, cached[1].location, node, keys)))
                known[node] = cached
                if cached[1] is not None:
                    found.append(cached[1])
                continue
            path = os.path.join(self._dev, node)
            opened = self._open(path)
            device = None
            if opened is not None and opened[0] is not None:
                (driver, card, captures), modes = opened
                virtual = driver in VIRTUAL_DRIVERS and not (self._allow_loopback and driver == "v4l2 loopback")
                if captures and modes and not virtual and not is_virtual(card or name):
                    if keys is None:
                        keys = self._stable_keys()
                    device = Device(self._key(folder, path, node, keys), tidy_name(card or name), path, modes)
            elif opened is None:
                continue  # no permission or it went away: try again next time
            known[node] = (ident, device)
            if device is not None:
                found.append(device)
        self._known = known
        return tell_apart(found)


# ── Windows: Qt Multimedia ───────────────────────────────────────────────────

_QT_FPS = (5, 10, 15, 20, 24, 25, 30, 50, 60)


_QT_FOURCC = {"Format_Jpeg": "MJPG", "Format_YUYV": "YUYV", "Format_UYVY": "UYVY", "Format_NV12": "NV12"}


def _qt_modes(camera) -> tuple:
    modes = set()
    for fmt in camera.videoFormats():
        res = fmt.resolution()
        lo, hi = fmt.minFrameRate(), fmt.maxFrameRate()
        if hi - lo < 1:  # one rate: as it is, 59.94 stays 59.94
            fps = (round(hi, 2),) if hi else ()
        else:
            fps = tuple(f for f in _QT_FPS if lo - _FPS_SLACK <= f <= hi + _FPS_SLACK)
        modes.add(Mode(_QT_FOURCC.get(fmt.pixelFormat().name, "RAW"), res.width(), res.height(), fps))
    # Media Foundation offers an NV12 copy of each MJPEG mode, decoded by itself: not something the camera sends
    jpeg = {(m.width, m.height, m.fps) for m in modes if m.fourcc == "MJPG"}
    modes = {m for m in modes if not (m.fourcc == "NV12" and (m.width, m.height, m.fps) in jpeg)}
    return tuple(sorted(modes, key=lambda m: (m.fourcc, m.width, m.height)))


def scan_windows(paths=None) -> list:
    """GUI thread: the webcams Qt Multimedia lists, each at the index OpenCV's DirectShow backend opens it by.
    paths gives the DirectShow device paths in that order (tests pass their own)."""
    try:
        from PyQt6.QtMultimedia import QMediaDevices
    except ImportError:
        logger.warning("Qt Multimedia isn't there, so webcams can't be listed")
        return []
    from telescope.webcams_win import device_instance, dshow_paths
    if paths is None:
        paths = dshow_paths()
    index_of = {device_instance(p): i for i, p in enumerate(paths) if p}
    found = []
    for camera in QMediaDevices.videoInputs():
        name = camera.description().removesuffix(" (Windows Virtual Camera)")
        key = bytes(camera.id()).decode("utf-8", "replace")
        index = index_of.get(device_instance(key))
        if is_virtual(name) or index is None:
            continue  # virtual, or not a DirectShow camera OpenCV can open
        found.append(Device(key, name, index, _qt_modes(camera)))
    return tell_apart(found)


# ── Who has a camera open ────────────────────────────────────────────────────

def who_uses(path: str, proc: str = "/proc") -> str:
    """Linux: the name of another program that has path open, or "" (only programs of this user can be seen)."""
    target = os.path.realpath(path)
    try:
        pids = [p for p in os.listdir(proc) if p.isdigit() and int(p) != os.getpid()]
    except OSError:
        return ""
    for pid in pids:
        fds = os.path.join(proc, pid, "fd")
        try:
            links = os.listdir(fds)
        except OSError:
            continue
        for fd in links:
            try:
                if os.readlink(os.path.join(fds, fd)) == target:
                    return _read(os.path.join(proc, pid, "comm"))
            except OSError:
                continue
    return ""


# ── Reading frames ───────────────────────────────────────────────────────────

_DSHOW_FOURCC = {"YUYV": "YUY2"}


def open_capture(device: Device, choice: Choice):
    """An OpenCV capture of device asking for choice. On Linux an MJPEG camera hands over its JPEGs undecoded, so
    the stream's decoders can work on several at once."""
    if vcam.IS_LINUX:
        cap = cv2.VideoCapture(device.location, cv2.CAP_V4L2)
        if not cap.isOpened():
            return cap
        if choice.fourcc != "RAW":
            cap.set(cv2.CAP_PROP_FOURCC, cv2.VideoWriter_fourcc(*choice.fourcc))
    else:
        cap = cv2.VideoCapture(device.location, cv2.CAP_DSHOW)
        if not cap.isOpened():
            return cap
        if choice.fourcc != "RAW":  # before the size, or DirectShow keeps the format it had
            cap.set(cv2.CAP_PROP_FOURCC, cv2.VideoWriter_fourcc(*_DSHOW_FOURCC.get(choice.fourcc, choice.fourcc)))
    cap.set(cv2.CAP_PROP_FRAME_WIDTH, choice.width)
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, choice.height)
    if choice.fps:
        cap.set(cv2.CAP_PROP_FPS, choice.fps)
    if vcam.IS_LINUX and choice.compressed:
        cap.set(cv2.CAP_PROP_CONVERT_RGB, 0)
    return cap


class WebcamReader:
    """StreamWorker's reader for a webcam: the same open/read_packet/decode/release as MjpegReader.

    locate() gives the camera as it's plugged in now (or None), want() the Choice to ask it for, and report(state,
    detail) hears how it went from the stream's threads: "ok" with the Choice's text, "busy" with the program
    holding it if known, "gone" or "silent"."""

    parallel_decode = False
    waiting_text = BUSY_TEXT

    def __init__(self, locate: Callable, want: Callable, report: Callable = lambda state, detail="": None,
                 opener: Callable = open_capture, users: Callable = who_uses):
        self._locate, self._want, self._report = locate, want, report
        self._opener, self._users = opener, users
        self._cap = None
        self._first = None  # the frame open() read to check the camera sends, handed out first
        self._jpeg = False
        self._decoded_jpeg = False  # MJPEG that came decoded (DirectShow), whose size on the cable isn't known
        self.last_frame_bytes = 0
        self.last_frame_count = 1

    def open(self) -> bool:
        self.release()
        device = self._locate()
        if device is None:
            self.waiting_text = GONE_TEXT
            self._report("gone")
            return False
        choice = self._want(device)
        if choice is None:
            self.waiting_text = NO_FRAMES_TEXT
            self._report("silent")
            return False
        cap = self._opener(device, choice)
        ok, frame = (cap.read() if cap.isOpened() else (False, None))
        if not ok or frame is None:
            opened = cap.isOpened()  # before release, which always closes it
            cap.release()
            self._failed(device, opened=opened)
            return False
        self._cap, self._first = cap, frame
        self._jpeg = vcam.IS_LINUX and choice.compressed and frame.ndim == 2 and frame.shape[0] == 1
        self._decoded_jpeg = choice.compressed and not self._jpeg
        self.parallel_decode = self._jpeg
        self._report("ok", choice.describe())
        return True

    def _failed(self, device: Device, opened: bool):
        """Why the camera gave nothing: gone, held by another program, or opened but silent."""
        if self._locate() is None or (vcam.IS_LINUX and not os.path.exists(device.location)):
            self.waiting_text = GONE_TEXT
            self._report("gone")
            return
        user = self._users(device.location) if vcam.IS_LINUX else ""
        if user or not opened or not vcam.IS_LINUX:
            self.waiting_text = BUSY_TEXT
            self._report("busy", user)
        else:
            self.waiting_text = NO_FRAMES_TEXT
            self._report("silent")

    def isOpened(self) -> bool:
        return self._cap is not None

    def read_packet(self):
        if self._cap is None:
            return False, None
        if self._first is not None:
            frame, self._first = self._first, None
        else:
            ok, frame = self._cap.read()
            if not ok or frame is None:
                device = self._locate()
                if device is None or (vcam.IS_LINUX and not os.path.exists(device.location)):
                    self.waiting_text = GONE_TEXT
                    self._report("gone")
                else:
                    self.waiting_text = NO_FRAMES_TEXT
                    self._report("silent")
                return False, None
        # What crossed the cable: the JPEG, or about two bytes a pixel raw (YUYV), not the decoded picture
        if self._jpeg:
            self.last_frame_bytes = int(frame.nbytes)
        else:
            self.last_frame_bytes = 0 if self._decoded_jpeg else frame.shape[0] * frame.shape[1] * 2
        return True, frame

    def decode(self, frame):
        if self._jpeg:
            return decode_jpeg(frame.tobytes())
        return frame

    def read(self):
        ok, packet = self.read_packet()
        if not ok:
            return False, None
        frame = self.decode(packet)
        return (True, frame) if frame is not None else (False, None)

    def release(self):
        cap, self._cap, self._first = self._cap, None, None
        if cap is not None:
            cap.release()


class WebcamAuth:
    """What the microphone plugin reaches the audio with; a webcam's mic is picked in the call app instead."""

    def headers(self) -> dict:
        return {}


class WebcamControl:
    """Stands in for PhoneControlClient while a webcam streams: there's no phone to control or ask for state."""

    base = "webcam:"
    audio = False  # nothing for the microphone plugin to bring over

    def __init__(self):
        self.auth = WebcamAuth()

    def get_state(self):
        return None

    def send(self, **params):
        pass

    def close(self):
        pass

