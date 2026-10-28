"""A webcam's own picture settings (exposure, white balance, focus and the like), for the Webcam card.

Only what the camera reports is offered, with its own range, step and default. Linux asks V4L2 directly with the
same ioctls webcams.py lists formats with; Windows asks the camera's Media Foundation source for its DirectShow
IAMVideoProcAmp and IAMCameraControl interfaces (webcams_win.py). Each control has one name here whichever
platform it came from, so the card and the saved settings don't care.
"""

import locale
import logging
import os
import struct
from dataclasses import dataclass
from typing import Optional

from telescope import vcam
from telescope.webcams import _ioctl, _iowr

logger = logging.getLogger(__name__)

# Auto switches and the manual control each one greys out while it's on
AUTO_OF = {"exposure_time": "exposure_auto", "white_balance": "white_balance_auto", "focus": "focus_auto"}
AUTOS = tuple(AUTO_OF.values())

# Anti-flicker choices, as V4L2 numbers them
FLICKER_OFF, FLICKER_50, FLICKER_60, FLICKER_AUTO = 0, 1, 2, 3
# Where mains electricity runs at 60 Hz (most of the Americas and a few others); everywhere else is 50 Hz
_MAINS_60 = {"US", "CA", "MX", "BR", "CO", "VE", "EC", "PE", "BO", "GT", "HN", "SV", "NI", "CR", "PA", "CU", "DO",
             "PR", "HT", "BS", "BZ", "TT", "JM", "BB", "GU", "KR", "TW", "PH", "SA", "LR", "SR", "GY", "AS", "VI"}


@dataclass(frozen=True)
class Control:
    """One setting a camera offers. unit says how to show the number: "" as it is, "100us" (exposure in 0.1 ms),
    "log2s" (exposure as a power of two seconds), "kelvin", "arcsec" (pan and tilt) or "zoom"."""
    name: str
    kind: str  # "int", "bool" or "menu"
    minimum: int
    maximum: int
    step: int
    default: int
    value: int
    menu: tuple = ()  # the values a menu offers
    inactive: bool = False  # the camera says it does nothing right now (its auto partner is on)
    unit: str = ""

    def clamp(self, value: int) -> int:
        value = max(self.minimum, min(self.maximum, int(value)))
        step = max(1, self.step)
        return self.minimum + round((value - self.minimum) / step) * step if self.kind == "int" else value


def mains_hz(territory: str = "") -> int:
    """50 or 60: the mains frequency where this computer is. The time zone says where better than the language does
    (English with Swedish formats reports the US)."""
    if not territory:
        try:
            from PyQt6.QtCore import QLocale, QTimeZone
            territory = QLocale.territoryToCode(QTimeZone.systemTimeZone().territory())
            if not territory or territory == "001":
                territory = QLocale.territoryToCode(QLocale.system().territory())
        except Exception:
            territory = (locale.getlocale()[0] or "").partition("_")[2][:2]
    return 60 if territory.upper() in _MAINS_60 else 50


def local_flicker(control: Control) -> Optional[int]:
    """The anti-flicker choice for the local mains, if the camera offers it."""
    want = FLICKER_60 if mains_hz() == 60 else FLICKER_50
    return want if want in control.menu else None


def default_of(control: Control) -> int:
    """What Reset puts a control back to: the camera's default, but anti-flicker to the local mains."""
    if control.name == "anti_flicker":
        local = local_flicker(control)
        if local is not None:
            return local
    return control.default


def apply_order(names) -> list:
    """Auto switches first, so the manual values that follow aren't refused while auto is still on."""
    return sorted(names, key=lambda n: (n not in AUTOS, n))


# ── Linux: V4L2 controls ─────────────────────────────────────────────────────

_QUERYCTRL = struct.Struct("2I32s5i2I")  # struct v4l2_queryctrl
_QUERYMENU = struct.Struct("<2I32sI")  # struct v4l2_querymenu, packed
_CONTROL = struct.Struct("Ii")  # struct v4l2_control
VIDIOC_G_CTRL = _iowr(27, _CONTROL.size)
VIDIOC_S_CTRL = _iowr(28, _CONTROL.size)
VIDIOC_QUERYCTRL = _iowr(36, _QUERYCTRL.size)
VIDIOC_QUERYMENU = _iowr(37, _QUERYMENU.size)
_TYPE_INT, _TYPE_BOOL, _TYPE_MENU = 1, 2, 3
_FLAG_DISABLED, _FLAG_READ_ONLY, _FLAG_INACTIVE = 0x1, 0x4, 0x10
_EXPOSURE_MANUAL = 1  # V4L2_EXPOSURE_MANUAL; every other choice is some kind of auto

# Our names for the V4L2 controls the card offers, with how to show them
V4L2_CONTROLS = {
    0x00980900: ("brightness", ""), 0x00980901: ("contrast", ""), 0x00980902: ("saturation", ""),
    0x00980903: ("hue", ""), 0x0098090C: ("white_balance_auto", ""), 0x00980910: ("gamma", ""),
    0x00980913: ("gain", ""), 0x00980918: ("anti_flicker", ""), 0x0098091A: ("white_balance", "kelvin"),
    0x0098091B: ("sharpness", ""), 0x0098091C: ("backlight", ""),
    0x009A0901: ("exposure_auto", ""), 0x009A0902: ("exposure_time", "100us"),
    0x009A0903: ("frame_rate_drop", ""), 0x009A0908: ("pan", "arcsec"), 0x009A0909: ("tilt", "arcsec"),
    0x009A090A: ("focus", ""), 0x009A090C: ("focus_auto", ""), 0x009A090D: ("zoom", "zoom"),
}
_CID_OF = {name: cid for cid, (name, _unit) in V4L2_CONTROLS.items()}


class LinuxControls:
    """The V4L2 controls of the node at path. Opens it for each call, which works while a stream holds it too."""

    def __init__(self, path: str, ioctl=None):
        self._path = path
        self._ioctl = ioctl or _ioctl
        self._exposure_auto = 3  # what auto exposure is on this camera: aperture priority on most webcams

    def _open(self) -> Optional[int]:
        try:
            return os.open(self._path, os.O_RDWR | os.O_NONBLOCK)
        except OSError:
            return None

    def list(self) -> list:
        fd = self._open()
        if fd is None:
            return []
        try:
            return [c for c in (self._query(fd, cid, name, unit) for cid, (name, unit) in V4L2_CONTROLS.items()) if c]
        finally:
            os.close(fd)

    def _query(self, fd: int, cid: int, name: str, unit: str) -> Optional[Control]:
        raw = self._ioctl(fd, VIDIOC_QUERYCTRL, _QUERYCTRL.pack(cid, 0, b"", 0, 0, 0, 0, 0, 0, 0))
        if raw is None:
            return None
        _id, kind, _label, lo, hi, step, default, flags, _r0, _r1 = _QUERYCTRL.unpack(raw)
        if flags & (_FLAG_DISABLED | _FLAG_READ_ONLY) or kind not in (_TYPE_INT, _TYPE_BOOL, _TYPE_MENU):
            return None
        got = self._ioctl(fd, VIDIOC_G_CTRL, _CONTROL.pack(cid, 0))
        value = _CONTROL.unpack(got)[1] if got is not None else default
        inactive = bool(flags & _FLAG_INACTIVE)
        if name == "exposure_auto":  # a menu in V4L2; here it's a switch, as on Windows
            menu = self._menu(fd, cid, lo, hi)
            autos = [v for v in menu if v != _EXPOSURE_MANUAL]
            if not autos or _EXPOSURE_MANUAL not in menu:
                return None
            self._exposure_auto = default if default in autos else autos[-1]
            return Control(name, "bool", 0, 1, 1, int(default != _EXPOSURE_MANUAL), int(value != _EXPOSURE_MANUAL))
        if kind == _TYPE_MENU:
            menu = self._menu(fd, cid, lo, hi)
            return Control(name, "menu", lo, hi, 1, default, value, tuple(menu), inactive, unit) if menu else None
        if kind == _TYPE_BOOL:
            return Control(name, "bool", 0, 1, 1, default, value, (), inactive, unit)
        if hi <= lo:
            return None
        return Control(name, "int", lo, hi, max(1, step), default, value, (), inactive, unit)

    def _menu(self, fd: int, cid: int, lo: int, hi: int) -> list:
        return [i for i in range(lo, min(hi, lo + 32) + 1)
                if self._ioctl(fd, VIDIOC_QUERYMENU, _QUERYMENU.pack(cid, i, b"", 0)) is not None]

    def set(self, name: str, value: int) -> bool:
        cid = _CID_OF.get(name)
        if cid is None:
            return False
        if name == "exposure_auto":
            value = self._exposure_auto if value else _EXPOSURE_MANUAL
        fd = self._open()
        if fd is None:
            return False
        try:
            ok = self._ioctl(fd, VIDIOC_S_CTRL, _CONTROL.pack(cid, int(value))) is not None
        finally:
            os.close(fd)
        if not ok:
            logger.debug("%s refused %s = %s", self._path, name, value)
        return ok


def controls_for(device) -> Optional[object]:
    """What reads and sets device's controls on this system, or None where that can't be done."""
    if device is None:
        return None
    if vcam.IS_LINUX:
        return LinuxControls(device.location) if isinstance(device.location, str) else None
    from telescope.webcams_win import WindowsControls
    return WindowsControls(device.key)

