#!/usr/bin/env python3
"""
PhoneCam Desktop
Receives MJPEG stream from PhoneCam Android app and feeds it into a virtual camera.
  Linux  : /dev/video10 via v4l2loopback
  Windows: "Unity Video Capture" via UnityCapture

Control API: sends HTTP GET requests to the phone's /control endpoint.
"""

import json
import math
import platform
import shutil
import socket
import subprocess
import sys
import threading
import time
import urllib.request
from typing import Optional

# ── Dependency check ──────────────────────────────────────────────────────────
_missing = []
try:    from PyQt6.QtCore import Qt
except ImportError: _missing.append("PyQt6")
try:    import cv2
except ImportError: _missing.append("opencv-python")
try:    import numpy as np
except ImportError: _missing.append("numpy")
try:    import pyvirtualcam
except ImportError: _missing.append("pyvirtualcam")
try:    import qt_material
except ImportError: _missing.append("qt-material")

if _missing:
    print(f"Missing: pip install {' '.join(_missing)}", file=sys.stderr)
    sys.exit(1)

from PyQt6.QtCore import (
    QThread, pyqtSignal, Qt, QTimer, QUrl,
)
from PyQt6.QtGui import (
    QFont, QColor, QPalette, QIntValidator, QDesktopServices,
    QIcon, QPixmap, QPainter, QPen, QBrush, QAction,
)
from PyQt6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QScrollArea,
    QVBoxLayout, QHBoxLayout, QGridLayout, QLayout,
    QGroupBox, QLabel, QLineEdit, QSpinBox, QDoubleSpinBox,
    QComboBox, QCheckBox, QRadioButton, QButtonGroup,
    QPushButton, QSlider, QTextEdit, QFrame,
    QSizePolicy, QMessageBox, QSystemTrayIcon, QMenu,
    QDialog, QDialogButtonBox, QFormLayout,
)
from pathlib import Path
from qt_material import apply_stylesheet

# ── Platform ──────────────────────────────────────────────────────────────────
IS_LINUX   = platform.system() == "Linux"
IS_WINDOWS = platform.system() == "Windows"

VCAM_BACKEND   = "v4l2loopback" if IS_LINUX else "unitycapture"
V4L2_PHONE_DEV = "/dev/video11"
V4L2_OBS_DEV   = "/dev/video10"
DEFAULT_PORT    = 8080
RECONNECT_DELAY = 3

# ── Paths ─────────────────────────────────────────────────────────────────────
CONFIG_FILE = Path(__file__).parent / "phonecam_config.json"

def unitycapture_dir() -> Path:
    """Always next to the EXE (frozen) or next to this script (dev)."""
    if getattr(sys, "frozen", False):
        return Path(sys.executable).parent / "unitycapture"
    return Path(__file__).parent / "unitycapture"

# ── Defaults ──────────────────────────────────────────────────────────────────
DEFAULT_QUALITY   = 85
DEFAULT_PHONE_FPS = 30

ADB_DOWNLOAD_URL          = "https://developer.android.com/tools/releases/platform-tools"
UNITYCAPTURE_URL_BASE     = "https://github.com/schellingb/UnityCapture/raw/master/Install"

# ── Options ───────────────────────────────────────────────────────────────────
RESOLUTIONS = {
    "Pass-through (auto)": None,
    "1920 x 1080": (1920, 1080),
    "1280 x 720":  (1280,  720),
    "854 x 480":   ( 854,  480),
    "640 x 360":   ( 640,  360),
}
ROTATIONS = {
    "None":    None,
    "90 CW":   cv2.ROTATE_90_CLOCKWISE,
    "180":     cv2.ROTATE_180,
    "90 CCW":  cv2.ROTATE_90_COUNTERCLOCKWISE,
}
WB_NAMES = [
    (2000, "Candlelight"),
    (2700, "Incandescent"),   # → INCANDESCENT preset
    (3200, "Warm white"),     # → WARM_FLUORESCENT preset
    (4000, "Fluorescent"),    # → FLUORESCENT preset
    (5500, "Daylight"),       # → DAYLIGHT preset (D65)
    (6500, "Overcast"),       # → CLOUDY_DAYLIGHT preset
    (7500, "Shade"),          # → SHADE preset
    (8000, "Deep shade"),     # → SHADE preset
]

# Kelvin → preset name (mirrors the Android kelvinToAwbMode mapping)
def wb_preset_name(k: int) -> str:
    if k < 2500: return "Incandescent"
    if k < 3500: return "Warm fluorescent"
    if k < 4500: return "Fluorescent"
    if k < 6000: return "Daylight"
    if k < 7000: return "Cloudy daylight"
    return "Shade"

# ── Custom styling extensions for qt-material ─────────────────────────────────
EXTRA_QSS = """
* {
    font-family: 'Segoe UI', -apple-system, BlinkMacSystemFont, 'Ubuntu', 'Cantarell', 'Helvetica Neue', 'Arial', sans-serif;
}
QMainWindow, QScrollArea, QScrollArea > QWidget, QScrollArea > QWidget > QWidget, QWidget#content_widget, QWidget#footer_panel {
    background-color: #1e222b;
}
QScrollArea, QScrollArea > QWidget, QScrollArea > QWidget > QWidget {
    border: none;
}
QWidget#ip_row_container, QWidget#battery_row {
    background-color: transparent;
}
QWidget#footer_panel {
    border-top: 1px solid #282c34;
    background-color: #1e222b;
}
QFrame#card {
    background-color: #121419;
    border: 1px solid #282c34;
    border-radius: 8px;
}
QFrame#separator {
    background-color: #282c34;
    max-height: 1px;
    border: none;
}
QLabel#card_title {
    font-size: 10pt;
    font-weight: bold;
    text-transform: uppercase;
    color: #518cc6;
    letter-spacing: 1px;
}
QLabel#dim {
    color: #78909c;
    font-size: 9pt;
    font-weight: 500;
}
QLabel#val {
    color: #518cc6;
    font-family: monospace;
    font-size: 9pt;
}
QLabel#status_ok {
    color: #66bb6a;
}
QLabel#status_warn {
    color: #ffa726;
}
QLabel#status_err {
    color: #ef5350;
}
QLabel#status_dim {
    color: #78909c;
}
QLabel#fps_lbl {
    color: #518cc6;
    font-family: monospace;
    font-size: 9pt;
}
QComboBox {
    padding-left: 6px;
    padding-right: 20px;
}
QLineEdit:focus, QComboBox:focus, QSpinBox:focus, QDoubleSpinBox:focus {
    color: #ffffff;
    border-bottom: 2px solid #518cc6;
}
QSlider {
    background: transparent;
    height: 20px;
    padding-left: 3px;
    padding-right: 3px;
}
QSlider::groove:horizontal {
    border: none;
    height: 4px;
    background: #2c3e50;
    border-radius: 2px;
    margin-left: 7px;
    margin-right: 7px;
}
QSlider::sub-page:horizontal {
    background: #518cc6;
    border-radius: 2px;
}
QSlider::handle:horizontal {
    background: #518cc6;
    width: 14px;
    height: 14px;
    margin-top: -5px;
    margin-bottom: -5px;
    border-radius: 7px;
}
QSlider::handle:horizontal:hover {
    background: #487aa8;
}
QSlider::handle:horizontal:disabled {
    background: #546e7a;
}
QSlider::groove:horizontal:disabled {
    background: #1c2730;
}
QSlider::sub-page:horizontal:disabled {
    background: #37474f;
}
QPushButton {
    background-color: #3b5e7f;
    border: none;
    border-radius: 4px;
    padding: 6px 12px;
    color: #ffffff;
    font-weight: bold;
}
QPushButton:hover {
    background-color: #466e95;
}
QPushButton:pressed {
    background-color: #2f4b66;
}
QPushButton:checked {
    background-color: #518cc6;
    color: #ffffff;
}
QPushButton:checked:hover {
    background-color: #619cd6;
}
QPushButton#start_btn {
    font-size: 11pt;
    font-weight: bold;
    padding: 12px;
    border-radius: 6px;
    background-color: #3b5e7f;
    color: #ffffff;
}
QPushButton#start_btn:hover {
    background-color: #466e95;
}
QPushButton#start_btn[streaming=true] {
    background-color: #2c3e50;
    border: 1px solid #34495e;
    color: #cfd8dc;
}
QPushButton#start_btn[streaming=true]:hover {
    background-color: #34495e;
}
"""

# Direct color values matching EXTRA_QSS -- used where objectName selectors
# lose to qt-material's specificity (e.g. battery/temp labels).
STATUS_COLORS = {
    "status_ok":   "#66bb6a",
    "status_warn": "#ffa726",
    "status_err":  "#ef5350",
    "status_dim":  "#78909c",
}

# ── No-scroll subclasses to prevent mouse wheel events from changing values ──
class NoScrollComboBox(QComboBox):
    def wheelEvent(self, event):
        event.ignore()

class NoScrollSlider(QSlider):
    def wheelEvent(self, event):
        event.ignore()

class NoScrollSpinBox(QSpinBox):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setButtonSymbols(QSpinBox.ButtonSymbols.NoButtons)

    def wheelEvent(self, event):
        event.ignore()

class NoScrollDoubleSpinBox(QDoubleSpinBox):
    def __init__(self, parent=None):
        super().__init__(parent)
        self.setButtonSymbols(QDoubleSpinBox.ButtonSymbols.NoButtons)

    def wheelEvent(self, event):
        event.ignore()


def create_vector_icon(icon_name: str, color_hex: str) -> QIcon:
    pixmap = QPixmap(32, 32)
    pixmap.fill(Qt.GlobalColor.transparent)

    painter = QPainter(pixmap)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)

    color = QColor(color_hex)
    pen = QPen(color)
    pen.setWidth(2)
    pen.setCapStyle(Qt.PenCapStyle.RoundCap)
    pen.setJoinStyle(Qt.PenJoinStyle.RoundJoin)
    painter.setPen(pen)

    brush = QBrush(Qt.BrushStyle.NoBrush)
    painter.setBrush(brush)

    if icon_name == "connection":
        painter.drawRoundedRect(11, 10, 10, 12, 2, 2)
        painter.drawLine(5, 13, 11, 13)
        painter.drawLine(5, 19, 11, 19)
        painter.drawLine(21, 16, 27, 16)

    elif icon_name == "camera":
        painter.drawRoundedRect(6, 11, 20, 13, 2, 2)
        painter.drawEllipse(12, 13, 8, 8)
        painter.drawRect(10, 8, 5, 3)

    elif icon_name == "stream":
        painter.drawRoundedRect(5, 8, 22, 14, 2, 2)
        painter.drawLine(16, 22, 16, 26)
        painter.drawLine(11, 26, 21, 26)

    elif icon_name == "gear":
        painter.drawEllipse(11, 11, 10, 10)
        painter.drawEllipse(14, 14, 4, 4)
        for i in range(8):
            angle = i * 45
            painter.save()
            painter.translate(16, 16)
            painter.rotate(angle)
            painter.drawLine(0, -5, 0, -8)
            painter.restore()

    elif icon_name == "status":
        painter.drawEllipse(7, 7, 18, 18)
        pen_dot = QPen(color)
        pen_dot.setWidth(3)
        painter.setPen(pen_dot)
        painter.drawPoint(16, 12)
        painter.setPen(pen)
        painter.drawLine(16, 15, 16, 20)

    painter.end()
    return QIcon(pixmap)


def create_separator() -> QFrame:
    sep = QFrame()
    sep.setFrameShape(QFrame.Shape.HLine)
    sep.setObjectName("separator")
    return sep


# ── Helpers ───────────────────────────────────────────────────────────────────

def _run(cmd, timeout=10):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout, r.stderr
    except FileNotFoundError:         return -1, "", f"Not found: {cmd[0]}"
    except subprocess.TimeoutExpired: return -2, "", "Timed out"

def v4l2_module_loaded() -> bool:
    """True if v4l2loopback is present in lsmod, regardless of which devices it created."""
    rc, out, _ = _run(["lsmod"])
    return rc == 0 and "v4l2loopback" in out

def v4l2_devices_ready() -> bool:
    """True if our specific device node exists and is ready to use."""
    import os
    return os.path.exists(V4L2_PHONE_DEV)

def v4l2_is_loaded() -> bool:
    """True only when both the module is loaded AND our devices are present."""
    return v4l2_module_loaded() and v4l2_devices_ready()

def v4l2_load() -> tuple:
    """Load v4l2loopback with PhoneCam's parameters.
    Never unloads an already-running module -- that could break other setups.
    """
    import os
    if v4l2_module_loaded():
        return False, (
            f"v4l2loopback is loaded with a different config "
            f"and {V4L2_PHONE_DEV} is unavailable. "
            f"Run: sudo modprobe -r v4l2loopback"
        )
    # Check our target device numbers aren't already claimed by something else
    for dev in (V4L2_PHONE_DEV, V4L2_OBS_DEV):
        if os.path.exists(dev):
            return False, f"{dev} already exists and is not a v4l2loopback device."
    priv = ["pkexec"] if shutil.which("pkexec") else ["sudo"]
    rc, _, err = _run(priv + ["modprobe", "v4l2loopback",
        "devices=2", "video_nr=10,11",
        "card_label=Phone Camera,OBS Virtual Camera",
        "exclusive_caps=1"], timeout=60)
    return (True, f"Loaded: {V4L2_PHONE_DEV} + {V4L2_OBS_DEV}") \
        if rc == 0 else (False, err or "modprobe failed")

def platform_tools_dir() -> Path:
    if getattr(sys, "frozen", False):
        return Path(sys.executable).parent / "platform-tools"
    return Path(__file__).parent / "platform-tools"

# ── Single-instance enforcement ───────────────────────────────────────────────
_INSTANCE_PORT = 47823  # local IPC port; arbitrary, just needs to be consistent

def acquire_single_instance() -> Optional[socket.socket]:
    """Try to become the single running instance.

    Returns a bound server socket if we are the first instance.
    Returns None if another instance is already running (and signals it to raise).
    """
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 0)
    try:
        srv.bind(("127.0.0.1", _INSTANCE_PORT))
        srv.listen(1)
        return srv
    except OSError:
        # Another instance is running -- ask it to raise its window then exit
        try:
            c = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            c.settimeout(1)
            c.connect(("127.0.0.1", _INSTANCE_PORT))
            c.sendall(b"raise")
            c.close()
        except Exception:
            pass
        srv.close()
        return None

def listen_for_raise(srv: socket.socket, raise_cb):
    """Background thread: wait for 'raise' messages from future instances."""
    srv.settimeout(1.0)
    while True:
        try:
            conn, _ = srv.accept()
            try:
                if conn.recv(16) == b"raise":
                    raise_cb()
            finally:
                conn.close()
        except socket.timeout:
            continue
        except Exception:
            break

def adb_exe() -> Optional[str]:
    """Local bundled adb first, then PATH fallback."""
    local = platform_tools_dir() / ("adb.exe" if IS_WINDOWS else "adb")
    if local.exists():
        return str(local)
    return shutil.which("adb")

def adb_available() -> bool:
    return adb_exe() is not None

def adb_forward(port):
    rc, _, err = _run([adb_exe(), "forward", f"tcp:{port}", f"tcp:{port}"])
    return (True, f"Port {port} forwarded") if rc == 0 else (False, err)
def adb_unforward(port): _run([adb_exe(), "forward", "--remove", f"tcp:{port}"])

def ns_to_display(ns: int) -> str:
    if ns <= 0: return "?"
    s = ns / 1_000_000_000.0
    if s >= 1.0:
        return f"{s:.1f} s"
    denom = round(1.0 / s)
    return f"1/{denom:,}"

def wb_name(k: int) -> str:
    return min(WB_NAMES, key=lambda x: abs(x[0] - k))[1]

def quality_label(q: int) -> str:
    if q >= 95: return f"{q}%  High"
    if q >= 80: return f"{q}%  Balanced"
    if q >= 60: return f"{q}%  Low"
    return f"{q}%  Very low"

def log_pos_to_val(pos: int, steps: int, v_min: float, v_max: float) -> float:
    if v_min <= 0: v_min = 1
    t = pos / max(steps, 1)
    val = math.exp(math.log(v_min) + t * (math.log(v_max) - math.log(v_min)))
    return max(v_min, min(v_max, val))

def val_to_log_pos(val: float, steps: int, v_min: float, v_max: float) -> int:
    if val <= 0 or v_min <= 0: return 0
    val = max(v_min, min(v_max, val))
    t   = (math.log(val) - math.log(v_min)) / (math.log(v_max) - math.log(v_min))
    return round(t * steps)

def transform_frame(frame, flip_h: bool, flip_v: bool, rotation):
    if flip_h and flip_v: frame = cv2.flip(frame, -1)
    elif flip_h:          frame = cv2.flip(frame,  1)
    elif flip_v:          frame = cv2.flip(frame,  0)
    if rotation is not None: frame = cv2.rotate(frame, rotation)
    return frame

def apply_zoom(frame, zoom: float, pan_x: float, pan_y: float):
    """Center-crop with optional pan offset. zoom=1.0 is a no-op."""
    if zoom <= 1.0:
        return frame
    h, w = frame.shape[:2]
    crop_w = int(w / zoom)
    crop_h = int(h / zoom)
    max_dx = (w - crop_w) // 2
    max_dy = (h - crop_h) // 2
    cx = max_dx + int(pan_x * max_dx)
    cy = max_dy + int(pan_y * max_dy)
    x0 = max(0, min(cx, w - crop_w))
    y0 = max(0, min(cy, h - crop_h))
    cropped = frame[y0:y0 + crop_h, x0:x0 + crop_w]
    return cv2.resize(cropped, (w, h), interpolation=cv2.INTER_LINEAR)

# ── UnityCapture helpers (Windows) ────────────────────────────────────────────

def download_unitycapture(progress_cb=None) -> tuple:
    """Download UnityCaptureFilter32/64.dll from GitHub. Returns (ok, msg)."""
    d = unitycapture_dir()
    d.mkdir(parents=True, exist_ok=True)
    for bits in ("32", "64"):
        url  = f"{UNITYCAPTURE_URL_BASE}/UnityCaptureFilter{bits}.dll"
        dest = d / f"UnityCaptureFilter{bits}.dll"
        try:
            if progress_cb:
                progress_cb(f"Downloading {dest.name}...")
            urllib.request.urlretrieve(url, dest)
        except Exception as e:
            return False, f"Download failed: {e}"
    return True, "Downloaded"

def register_unitycapture() -> tuple:
    """Register both DLLs via elevated regsvr32. Returns (ok, msg)."""
    d = unitycapture_dir()
    dll32 = str(d / "UnityCaptureFilter32.dll")
    dll64 = str(d / "UnityCaptureFilter64.dll")
    # Single UAC prompt via PowerShell -> elevated cmd
    ps = (
        'Start-Process cmd.exe '
        f'-ArgumentList \'/c regsvr32 /s "{dll32}" && regsvr32 /s "{dll64}"\' '
        '-Verb RunAs -Wait -WindowStyle Hidden'
    )
    try:
        r = subprocess.run(
            ["powershell", "-NoProfile", "-Command", ps],
            capture_output=True, timeout=60,
        )
        if r.returncode == 0:
            return True, "Installed"
        return False, "Registration failed (cancelled or denied?)"
    except subprocess.TimeoutExpired:
        return False, "Timed out"
    except Exception as e:
        return False, str(e)

def uc_is_registered() -> bool:
    """Check the Windows registry to see if UnityCapture is actually registered."""
    if not IS_WINDOWS:
        return True
    try:
        import winreg
        # regsvr32 registers the DLL under HKCR\CLSID\{...}\InprocServer32
        # We search for our DLL path rather than hardcoding the CLSID
        dll = str(unitycapture_dir() / "UnityCaptureFilter64.dll").lower()
        with winreg.OpenKey(winreg.HKEY_CLASSES_ROOT, "CLSID") as clsid_root:
            i = 0
            while True:
                try:
                    clsid = winreg.EnumKey(clsid_root, i)
                    try:
                        with winreg.OpenKey(clsid_root, f"{clsid}\\InprocServer32") as k:
                            val, _ = winreg.QueryValueEx(k, "")
                            if val.lower() == dll:
                                return True
                    except OSError:
                        pass
                    i += 1
                except OSError:
                    break
    except Exception:
        pass
    return False

# ── Phone control client ──────────────────────────────────────────────────────
class PhoneControlClient:
    def __init__(self, stream_url: str):
        self.base = stream_url.rsplit("/video", 1)[0]

    def get_state(self) -> Optional[dict]:
        try:
            r = urllib.request.urlopen(f"{self.base}/cameras", timeout=4)
            return json.loads(r.read().decode())
        except Exception:
            return None

    def send(self, **params):
        qs  = "&".join(f"{k}={v}" for k, v in params.items())
        url = f"{self.base}/control?{qs}"
        threading.Thread(target=self._req, args=(url,), daemon=True).start()

    def _req(self, url):
        try: urllib.request.urlopen(url, timeout=3)
        except Exception: pass

# ── Stream worker ─────────────────────────────────────────────────────────────
class StreamWorker(QThread):
    status = pyqtSignal(str, str)   # (kind, msg): info/ok/warn/fps/idle

    def __init__(self, url: str, width: Optional[int], height: Optional[int],
                 fps: int, flip_h: bool, flip_v: bool, rotation,
                 zoom: float = 1.0, pan_x: float = 0.0, pan_y: float = 0.0):
        super().__init__()
        self.url       = url
        self._width    = width
        self._height   = height
        self._fps      = fps
        self.flip_h    = flip_h
        self.flip_v    = flip_v
        self.rotation  = rotation
        self.zoom      = zoom
        self.pan_x     = pan_x
        self.pan_y     = pan_y
        self._stop_flag    = False
        self._restart_vcam = threading.Event()
        self._latest_rgb   = None

    def update_output(self, width=None, height=None, fps=None):
        if width  is not None: self._width  = width
        if height is not None: self._height = height
        if fps    is not None: self._fps    = fps
        self._restart_vcam.set()

    def request_stop(self):
        self._stop_flag = True
        self._restart_vcam.set()

    def _open_cap(self):
        cap = cv2.VideoCapture(self.url)
        cap.set(cv2.CAP_PROP_OPEN_TIMEOUT_MSEC, 3000)
        cap.set(cv2.CAP_PROP_READ_TIMEOUT_MSEC, 1000)  # short so stop_flag is checked frequently
        return cap

    def _reconnect_cap(self, stop_event: threading.Event) -> Optional[object]:
        self.status.emit("warn", "Stream dropped - reconnecting...")
        while not stop_event.is_set() and not self._stop_flag:
            time.sleep(RECONNECT_DELAY)
            cap = self._open_cap()
            if cap.isOpened():
                ret, _ = cap.read()
                if ret:
                    return cap
            cap.release()
        return None

    def _stream_reader(self, cap, resize_w: Optional[int], resize_h: Optional[int],
                       stop_event: threading.Event):
        while not stop_event.is_set() and not self._stop_flag:
            ret, raw = cap.read()
            if not ret or raw is None:
                cap.release()
                cap = self._reconnect_cap(stop_event)
                if cap is None:
                    return
                self.status.emit("ok", "Stream reconnected")
                continue
            # Resize BEFORE rotation so aspect ratio is preserved through 90° turns
            if resize_w or resize_h:
                rw = resize_w or raw.shape[1]
                rh = resize_h or raw.shape[0]
                raw = cv2.resize(raw, (rw, rh))
            raw = apply_zoom(raw, self.zoom, self.pan_x, self.pan_y)
            raw = transform_frame(raw, self.flip_h, self.flip_v, self.rotation)
            self._latest_rgb = cv2.cvtColor(raw, cv2.COLOR_BGR2RGB)
        if cap is not None:
            cap.release()

    def run(self):
        self.status.emit("info", f"Connecting to {self.url}...")
        while not self._stop_flag:
            cap = self._open_cap()
            if not cap.isOpened():
                cap.release()
                self.status.emit("warn", f"Cannot open stream - retry in {RECONNECT_DELAY}s...")
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue

            ret, frame = cap.read()
            if not ret or frame is None:
                cap.release()
                self.status.emit("warn", "Empty first frame - retrying...")
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue

            # Resize BEFORE rotation so a 90° turn doesn't distort the image.
            # The vcam then opens with whatever dimensions result after rotation.
            if self._width or self._height:
                rw = self._width  or frame.shape[1]
                rh = self._height or frame.shape[0]
                frame = cv2.resize(frame, (rw, rh))
            frame = apply_zoom(frame, self.zoom, self.pan_x, self.pan_y)
            frame = transform_frame(frame, self.flip_h, self.flip_v, self.rotation)
            w = frame.shape[1]
            h = frame.shape[0]
            self._latest_rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
            self._restart_vcam.clear()
            self.status.emit("ok", f"Stream {w}x{h} @ {self._fps} fps -> {VCAM_BACKEND}")

            reader_stop = threading.Event()
            reader = threading.Thread(
                target=self._stream_reader,
                args=(cap, self._width, self._height, reader_stop),
                daemon=True,
            )
            reader.start()

            try:
                with pyvirtualcam.Camera(width=w, height=h, fps=self._fps,
                                         backend=VCAM_BACKEND,
                                         device=V4L2_PHONE_DEV if IS_LINUX else None) as cam:
                    self.status.emit("ok", f"Virtual camera: {cam.device}")
                    fc, t0 = 0, time.time()
                    while not self._stop_flag and not self._restart_vcam.is_set():
                        rgb = self._latest_rgb
                        if rgb is not None:
                            cam.send(rgb)
                        cam.sleep_until_next_frame()
                        fc += 1
                        if (elapsed := time.time() - t0) >= 2.0:
                            self.status.emit("fps", f"{fc/elapsed:.1f} fps  {w}x{h}")
                            fc, t0 = 0, time.time()
            except Exception as exc:
                self.status.emit("warn", f"Virtual cam error: {exc}")

            reader_stop.set()
            reader.join(timeout=3)
            if reader.is_alive():
                # Reader is stuck in cap.read() -- it'll exit on next timeout (1s max)
                # and is a daemon thread so won't block process exit
                pass

            if self._stop_flag:
                break
            if not self._restart_vcam.is_set():
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
            self._restart_vcam.clear()

        self.status.emit("idle", "Stopped.")

# ── Lens panel ────────────────────────────────────────────────────────────────
class LensPanel(QWidget):
    lens_selected = pyqtSignal(dict)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._layout  = QGridLayout(self)
        self._layout.setContentsMargins(0, 0, 0, 0)
        self._layout.setSpacing(4)
        self._cameras: list = []
        self._btns:    list = []
        self._ph = QLabel("Start streaming to load lenses")
        self._ph.setObjectName("dim")
        self._layout.addWidget(self._ph, 0, 0)

    def load(self, cameras: list):
        self._ph.hide()
        for b in self._btns: b.deleteLater()
        self._btns.clear()
        self._cameras = cameras
        cols = 3
        for i, cam in enumerate(cameras):
            lbl = cam["label"].replace(" [phys]", "").replace("Back ", "").replace("Front ", "F/")
            btn = QPushButton(lbl)
            btn.setCheckable(True)
            btn.setChecked(cam.get("current", False))
            btn.clicked.connect(lambda _, c=cam, b=btn: self._select(c, b))
            self._layout.addWidget(btn, i // cols, i % cols)
            self._btns.append(btn)

    def _select(self, cam: dict, clicked_btn: QPushButton):
        for b in self._btns: b.setChecked(False)
        clicked_btn.setChecked(True)
        self.lens_selected.emit(cam)

    def set_placeholder(self, text: str):
        self._ph.setText(text)
        if not self._btns:
            self._ph.show()

    def clear(self):
        for b in self._btns: b.deleteLater()
        self._btns.clear()
        self._cameras.clear()
        self._ph.setText("Start streaming to load lenses")
        self._ph.show()

# ── Log-scale slider row ──────────────────────────────────────────────────────
class LogSliderRow(QWidget):
    """Horizontal slider on log scale with spinbox for direct entry.

    spinbox_scale: multiply internal value by this for spinbox display.
    e.g. spinbox_scale=1e-6 shows nanoseconds as milliseconds.
    """
    value_changed = pyqtSignal(float)
    STEPS = 2000

    def __init__(self, v_min: float, v_max: float,
                 display_fn=None, spinbox_suffix: str = "",
                 spinbox_scale: float = 1.0,
                 spinbox_decimals: int = 0, parent=None):
        super().__init__(parent)
        self.v_min = v_min
        self.v_max = v_max
        self.display_fn = display_fn or str
        self._spin_scale = spinbox_scale
        self._debounce: Optional[QTimer] = None

        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        self._slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(0, self.STEPS)
        self._slider.setValue(0)
        self._slider.setMinimumWidth(140)
        lay.addWidget(self._slider, 1)

        self._val_lbl = QLabel(display_fn(v_min) if display_fn else str(v_min))
        self._val_lbl.setObjectName("val")
        self._val_lbl.setMinimumWidth(70)
        lay.addWidget(self._val_lbl)

        self._is_double_spin = spinbox_decimals > 0
        if self._is_double_spin:
            spin = NoScrollDoubleSpinBox()
            spin.setDecimals(spinbox_decimals)
            spin.setRange(v_min * spinbox_scale, v_max * spinbox_scale)
            spin.setSingleStep(10 ** -spinbox_decimals)
        else:
            spin = NoScrollSpinBox()
            spin.setRange(int(v_min * spinbox_scale), int(v_max * spinbox_scale))
        spin.setSuffix(spinbox_suffix)
        spin.setFixedWidth(100)
        self._spin = spin
        lay.addWidget(self._spin)

        self._slider.valueChanged.connect(self._on_slider)
        self._spin.editingFinished.connect(self._on_spin)

    def _to_spin(self, val: float):
        sv = val * self._spin_scale
        return sv if self._is_double_spin else int(round(sv))

    def _on_slider(self, pos: int):
        val = log_pos_to_val(pos, self.STEPS, self.v_min, self.v_max)
        display_val = val
        if not self._is_double_spin:
            display_val = round(val)
        self._val_lbl.setText(self.display_fn(display_val))
        self._spin.blockSignals(True)
        self._spin.setValue(self._to_spin(val))
        self._spin.blockSignals(False)
        self._schedule_emit(val)

    def _on_spin(self):
        val = float(self._spin.value()) / self._spin_scale
        pos = val_to_log_pos(val, self.STEPS, self.v_min, self.v_max)
        self._slider.blockSignals(True)
        self._slider.setValue(pos)
        self._slider.blockSignals(False)
        display_val = val
        if not self._is_double_spin:
            display_val = round(val)
        self._val_lbl.setText(self.display_fn(display_val))
        self._schedule_emit(val)

    def _schedule_emit(self, val: float):
        if self._debounce:
            self._debounce.stop()
        self._debounce = QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.timeout.connect(lambda: self.value_changed.emit(val))
        self._debounce.start(80)

    def set_range(self, v_min: float, v_max: float):
        self.v_min = v_min
        self.v_max = v_max
        lo, hi = self._to_spin(v_min), self._to_spin(v_max)
        self._spin.setRange(lo, hi)
        cur_pos = self._slider.value()
        val = log_pos_to_val(cur_pos, self.STEPS, v_min, v_max)
        display_val = val
        if not self._is_double_spin:
            display_val = round(val)
        self._val_lbl.setText(self.display_fn(display_val))

    def get_value(self) -> float:
        return log_pos_to_val(self._slider.value(), self.STEPS, self.v_min, self.v_max)

    def set_value(self, val: float):
        pos = val_to_log_pos(val, self.STEPS, self.v_min, self.v_max)
        self._slider.blockSignals(True)
        self._slider.setValue(pos)
        self._slider.blockSignals(False)
        self._spin.blockSignals(True)
        self._spin.setValue(self._to_spin(val))
        self._spin.blockSignals(False)
        display_val = val
        if not self._is_double_spin:
            display_val = round(val)
        self._val_lbl.setText(self.display_fn(display_val))

    def set_enabled(self, enabled: bool):
        self._slider.setEnabled(enabled)
        self._spin.setEnabled(enabled)

# ── WB slider row ─────────────────────────────────────────────────────────────
class WbSliderRow(QWidget):
    """Linear Kelvin slider 2000-8000 with spinbox for direct entry."""
    value_changed = pyqtSignal(int)

    def __init__(self, parent=None):
        super().__init__(parent)
        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        self._slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(2000, 8000)
        self._slider.setValue(5500)
        self._slider.setMinimumWidth(140)
        self._slider.setSingleStep(50)
        self._slider.setPageStep(500)
        lay.addWidget(self._slider, 1)

        self._lbl = QLabel("Daylight")
        self._lbl.setObjectName("val")
        self._lbl.setMinimumWidth(70)
        lay.addWidget(self._lbl)

        self._spin = NoScrollSpinBox()
        self._spin.setRange(2000, 8000)
        self._spin.setValue(5500)
        self._spin.setSingleStep(50)
        self._spin.setSuffix(" K")
        self._spin.setFixedWidth(100)
        lay.addWidget(self._spin)

        self._debounce: Optional[QTimer] = None
        self._slider.valueChanged.connect(self._on_slider)
        self._spin.editingFinished.connect(self._on_spin)

    def _on_slider(self, k: int):
        self._lbl.setText(wb_name(k))
        self._spin.blockSignals(True)
        self._spin.setValue(k)
        self._spin.blockSignals(False)
        self._schedule_emit(k)

    def _on_spin(self):
        k = self._spin.value()
        self._slider.blockSignals(True)
        self._slider.setValue(k)
        self._slider.blockSignals(False)
        self._lbl.setText(wb_name(k))
        self._schedule_emit(k)

    def _schedule_emit(self, k: int):
        if self._debounce: self._debounce.stop()
        self._debounce = QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.timeout.connect(lambda: self.value_changed.emit(k))
        self._debounce.start(80)

    def get_value(self) -> int: return self._slider.value()

    def set_value(self, k: int):
        self._slider.blockSignals(True)
        self._slider.setValue(k)
        self._slider.blockSignals(False)
        self._spin.blockSignals(True)
        self._spin.setValue(k)
        self._spin.blockSignals(False)
        self._lbl.setText(wb_name(k))

    def set_enabled(self, enabled: bool):
        self._slider.setEnabled(enabled)
        self._spin.setEnabled(enabled)

# ── Linear pan slider ─────────────────────────────────────────────────────────
class PanSliderRow(QWidget):
    """Linear slider -1.0 to 1.0 with a centered zero tick."""
    value_changed = pyqtSignal(float)
    STEPS = 200

    def __init__(self, label_neg: str = "L", label_pos: str = "R", parent=None):
        super().__init__(parent)
        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        neg_lbl = QLabel(label_neg)
        neg_lbl.setObjectName("dim")
        lay.addWidget(neg_lbl)

        self._slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(-self.STEPS, self.STEPS)
        self._slider.setValue(0)
        self._slider.setMinimumWidth(120)
        lay.addWidget(self._slider, 1)

        pos_lbl = QLabel(label_pos)
        pos_lbl.setObjectName("dim")
        lay.addWidget(pos_lbl)

        self._debounce: Optional[QTimer] = None
        self._slider.valueChanged.connect(self._on_slider)

    def _on_slider(self, pos: int):
        val = pos / self.STEPS
        if self._debounce:
            self._debounce.stop()
        self._debounce = QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.timeout.connect(lambda: self.value_changed.emit(val))
        self._debounce.start(30)

    def get_value(self) -> float:
        return self._slider.value() / self.STEPS

    def set_value(self, val: float):
        self._slider.blockSignals(True)
        self._slider.setValue(int(val * self.STEPS))
        self._slider.blockSignals(False)

    def reset(self):
        self.set_value(0.0)

    def set_enabled(self, enabled: bool):
        self._slider.setEnabled(enabled)

# ── Add device dialog ─────────────────────────────────────────────────────────
class AddDeviceDialog(QDialog):
    def __init__(self, parent=None, existing_names: list = None):
        super().__init__(parent)
        self.setWindowTitle("Add Device")
        self.setMinimumWidth(320)
        self._existing = existing_names or []

        form = QFormLayout()
        self._name_edit = QLineEdit()
        self._name_edit.setPlaceholderText("e.g. Phone1")
        self._ip_edit   = QLineEdit()
        self._ip_edit.setPlaceholderText("e.g. 192.168.1.100")
        form.addRow("Name", self._name_edit)
        form.addRow("IP address", self._ip_edit)

        self._err_lbl = QLabel("")
        self._err_lbl.setObjectName("status_err")
        self._err_lbl.setWordWrap(True)

        buttons = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Ok | QDialogButtonBox.StandardButton.Cancel
        )
        buttons.accepted.connect(self._on_accept)
        buttons.rejected.connect(self.reject)

        lay = QVBoxLayout(self)
        lay.addLayout(form)
        lay.addWidget(self._err_lbl)
        lay.addWidget(buttons)

    def _on_accept(self):
        name = self._name_edit.text().strip()
        ip   = self._ip_edit.text().strip()
        if not name:
            self._err_lbl.setText("Name cannot be empty.")
            return
        if name in self._existing:
            self._err_lbl.setText(f'"{name}" already exists.')
            return
        if not ip:
            self._err_lbl.setText("IP address cannot be empty.")
            return
        self.accept()

    def result_values(self) -> tuple:
        return self._name_edit.text().strip(), self._ip_edit.text().strip()

# ── Main window ───────────────────────────────────────────────────────────────
class PhoneCamWindow(QMainWindow):
    _sig_state       = pyqtSignal(dict)
    _sig_lens_fail   = pyqtSignal()
    _sig_v4l_result  = pyqtSignal(bool, str)
    _sig_raise       = pyqtSignal()  # emitted by background thread when another instance asks to raise
    _sig_win_checks  = pyqtSignal(bool, bool)   # (uc_ok, adb_ok)
    _sig_uc_done     = pyqtSignal(bool, str)    # (success, msg)
    _sig_uc_msg      = pyqtSignal(str)          # progress during install
    _sig_adb_done    = pyqtSignal(bool, str)    # (success, msg)
    _sig_adb_msg     = pyqtSignal(str)          # progress during download
    _sig_battery     = pyqtSignal(int, bool, float)  # (level %, charging, temp °C)

    def __init__(self):
        super().__init__()
        self.setWindowTitle("PhoneCam")
        self.setMinimumSize(520, 480)
        self.resize(540, 900)

        self._worker: Optional[StreamWorker] = None
        self._ctrl:   Optional[PhoneControlClient] = None
        self._adb_port: Optional[int] = None
        self._manual_exp = False
        self._manual_wb  = False

        # Device list: [{"name": str, "ip": str}, ...]
        self._devices: list = []
        self._selected_device: Optional[str] = None
        self._switching_device = False  # suppress save during programmatic combo change

        # Windows-specific widget refs (populated in _build_platform_setup)
        self._uc_status_lbl:  Optional[QLabel]      = None
        self._uc_btn:         Optional[QPushButton]  = None
        self._adb_status_lbl: Optional[QLabel]      = None
        self._adb_btn:        Optional[QPushButton]  = None

        self._save_timer = QTimer(self)
        self._save_timer.setSingleShot(True)
        self._save_timer.timeout.connect(self._save_config)

        self._battery_timer = QTimer(self)
        self._battery_timer.setInterval(15_000)
        self._battery_timer.timeout.connect(self._poll_battery)
        self._battery_notified = False
        self._temp_notified    = False
        self._tray: Optional[QSystemTrayIcon] = None

        self._build_ui()
        self._setup_tray()

        self._sig_state.connect(self._apply_state)
        self._sig_raise.connect(self._tray_show)
        self._sig_battery.connect(self._on_battery)
        self._sig_lens_fail.connect(lambda: self._lens_panel.set_placeholder("Unavailable"))
        self._sig_v4l_result.connect(self._on_v4l_result)
        self._sig_win_checks.connect(self._on_win_checks)
        self._sig_uc_done.connect(self._on_uc_done)

        self._apply_config(self._load_config())

        if IS_LINUX:
            self._v4l_check()

        if IS_WINDOWS:
            self._sig_uc_msg.connect(
                lambda msg: self._uc_status_lbl.setText(msg)
                if self._uc_status_lbl else None
            )
            self._sig_adb_done.connect(self._on_adb_done)
            self._sig_adb_msg.connect(
                lambda msg: self._adb_status_lbl.setText(msg)
                if self._adb_status_lbl else None
            )
            threading.Thread(target=self._check_win_setup, daemon=True).start()

    # ── UI construction ───────────────────────────────────────────────────────

    def _build_ui(self):
        root = QWidget()
        self.setCentralWidget(root)
        root_lay = QVBoxLayout(root)
        root_lay.setContentsMargins(0, 0, 0, 0)
        root_lay.setSpacing(0)

        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        content = QWidget()
        content.setObjectName("content_widget")
        c_lay = QVBoxLayout(content)
        c_lay.setContentsMargins(16, 16, 16, 16)
        c_lay.setSpacing(14)
        scroll.setWidget(content)
        root_lay.addWidget(scroll, 1)

        c_lay.addWidget(self._build_connection())
        c_lay.addWidget(self._build_camera_control())
        c_lay.addWidget(self._build_stream_output())
        c_lay.addStretch()

        btn_frame = QWidget()
        btn_frame.setObjectName("footer_panel")
        btn_lay = QVBoxLayout(btn_frame)
        btn_lay.setContentsMargins(16, 10, 16, 16)
        btn_lay.setSpacing(8)

        self._status_lbl = QLabel("Idle - configure above and press Start")
        self._status_lbl.setObjectName("status_dim")
        self._status_lbl.setWordWrap(True)
        self._status_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)

        self._fps_lbl = QLabel("")
        self._fps_lbl.setObjectName("fps_lbl")
        self._fps_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)

        self._start_btn = QPushButton("Start Streaming")
        self._start_btn.setObjectName("start_btn")
        self._start_btn.setProperty("streaming", False)
        self._start_btn.clicked.connect(self._toggle)

        # Battery + temp row (visible only while streaming)
        self._battery_row = QWidget()
        self._battery_row.setObjectName("battery_row")
        batt_lay = QHBoxLayout(self._battery_row)
        batt_lay.setContentsMargins(0, 0, 0, 0)
        batt_lay.setSpacing(20)
        self._battery_lbl = QLabel("")
        self._battery_lbl.setObjectName("status_dim")
        self._battery_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._temp_lbl = QLabel("")
        self._temp_lbl.setObjectName("status_dim")
        self._temp_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        batt_lay.addStretch()
        batt_lay.addWidget(self._battery_lbl)
        batt_lay.addWidget(self._temp_lbl)
        batt_lay.addStretch()
        self._battery_row.setVisible(False)

        btn_lay.addWidget(self._status_lbl)
        btn_lay.addWidget(self._fps_lbl)
        btn_lay.addWidget(self._battery_row)
        btn_lay.addWidget(self._start_btn)
        root_lay.addWidget(btn_frame)

    def _group(self, title: str, icon_name: str) -> tuple:
        card = QFrame()
        card.setFrameShape(QFrame.Shape.StyledPanel)
        card.setObjectName("card")

        lay = QVBoxLayout(card)
        lay.setContentsMargins(14, 14, 14, 14)
        lay.setSpacing(10)

        # Header layout
        hdr = QHBoxLayout()
        hdr.setContentsMargins(0, 0, 0, 4)
        hdr.setSpacing(8)

        # Icon
        icon_lbl = QLabel()
        icon_lbl.setPixmap(create_vector_icon(icon_name, "#518cc6").pixmap(18, 18))
        icon_lbl.setFixedSize(18, 18)
        hdr.addWidget(icon_lbl)

        # Title
        title_lbl = QLabel(title)
        title_lbl.setObjectName("card_title")
        hdr.addWidget(title_lbl)
        hdr.addStretch()

        lay.addLayout(hdr)
        return card, lay

    def _row(self, label: str, widget, label_width=110, stretch=False) -> QHBoxLayout:
        lay = QHBoxLayout()
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)
        lbl = QLabel(label)
        lbl.setObjectName("dim")
        lbl.setFixedWidth(label_width)
        lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        lay.addWidget(lbl)
        if stretch:
            if isinstance(widget, QLayout):
                lay.addLayout(widget, 1)
            else:
                lay.addWidget(widget, 1)
        else:
            if isinstance(widget, QLayout):
                lay.addLayout(widget)
            else:
                lay.addWidget(widget)
            lay.addStretch(1)
        return lay

    # ── Connection ────────────────────────────────────────────────────────────
    def _build_connection(self) -> QFrame:
        gb, lay = self._group("Connection & System", "connection")

        mode_row = QHBoxLayout()
        mode_row.setContentsMargins(0, 0, 0, 0)
        mode_lbl = QLabel("Mode")
        mode_lbl.setObjectName("dim")
        mode_lbl.setFixedWidth(110)
        mode_lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        mode_row.addWidget(mode_lbl)
        self._rb_usb  = QRadioButton("USB (ADB)")
        self._rb_wifi = QRadioButton("Wi-Fi")
        for rb in (self._rb_usb, self._rb_wifi):
            rb.setAutoExclusive(False)
        self._conn_grp = QButtonGroup(gb)
        self._conn_grp.addButton(self._rb_usb)
        self._conn_grp.addButton(self._rb_wifi)
        self._rb_usb.setChecked(True)
        self._conn_grp.buttonClicked.connect(lambda _: self._on_mode())
        mode_row.addWidget(self._rb_usb)
        mode_row.addWidget(self._rb_wifi)
        mode_row.addStretch()
        lay.addLayout(mode_row)

        # Device selector (shown in Wi-Fi mode)
        self._device_row_w = QWidget()
        self._device_row_w.setObjectName("ip_row_container")
        device_v = QVBoxLayout(self._device_row_w)
        device_v.setContentsMargins(0, 0, 0, 0)
        device_v.setSpacing(4)

        # Combo + buttons row
        combo_row = QHBoxLayout()
        combo_row.setContentsMargins(0, 0, 0, 0)
        combo_row.setSpacing(6)
        dev_lbl = QLabel("Device")
        dev_lbl.setObjectName("dim")
        dev_lbl.setFixedWidth(110)
        dev_lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        combo_row.addWidget(dev_lbl)
        self._device_combo = NoScrollComboBox()
        self._device_combo.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        self._device_combo.currentIndexChanged.connect(self._on_device_changed)
        combo_row.addWidget(self._device_combo, 1)
        _icon_btn_style = "padding: 0px;"
        self._add_device_btn = QPushButton("+")
        self._add_device_btn.setFixedSize(28, 28)
        self._add_device_btn.setStyleSheet(_icon_btn_style)
        self._add_device_btn.clicked.connect(self._on_add_device)
        self._remove_device_btn = QPushButton("−")
        self._remove_device_btn.setFixedSize(28, 28)
        self._remove_device_btn.setStyleSheet(_icon_btn_style)
        self._remove_device_btn.clicked.connect(self._on_remove_device)
        combo_row.addWidget(self._add_device_btn)
        combo_row.addWidget(self._remove_device_btn)
        device_v.addLayout(combo_row)

        # IP display label (dim, read-only)
        ip_display_row = QHBoxLayout()
        ip_display_row.setContentsMargins(0, 0, 0, 0)
        ip_display_row.addSpacing(118)  # align with combo
        self._ip_display_lbl = QLabel("")
        self._ip_display_lbl.setObjectName("dim")
        self._ip_display_lbl.setWordWrap(True)
        ip_display_row.addWidget(self._ip_display_lbl, 1)
        device_v.addLayout(ip_display_row)

        lay.addWidget(self._device_row_w)
        self._device_row_w.setVisible(False)

        self._port_field = QLineEdit(str(DEFAULT_PORT))
        self._port_field.setValidator(QIntValidator(1, 65535))
        self._port_field.setMaximumWidth(90)
        self._port_field.editingFinished.connect(self._schedule_save)
        lay.addLayout(self._row("Port", self._port_field))

        lay.addWidget(create_separator())

        # Platform / Driver Setup
        if IS_LINUX:
            self._v4l_lbl = QLabel("Status unknown")
            self._v4l_lbl.setObjectName("status_dim")
            self._v4l_lbl.setWordWrap(True)
            self._v4l_lbl.setToolTip(
                "Virtual camera mapping:\n"
                f"  • Phone Feed: {V4L2_PHONE_DEV}\n"
                f"  • OBS Loopback: {V4L2_OBS_DEV}"
            )
            lay.addLayout(self._row("Virtual Cam", self._v4l_lbl, stretch=True))

            btn_row = QHBoxLayout()
            btn_row.setContentsMargins(0, 0, 0, 0)
            chk_btn  = QPushButton("Check Status")
            load_btn = QPushButton("Load Module")
            chk_btn.clicked.connect(self._v4l_check)
            load_btn.clicked.connect(self._v4l_load)
            btn_row.addWidget(chk_btn)
            btn_row.addWidget(load_btn)
            btn_row.addStretch()
            lay.addLayout(self._row("Driver Actions", btn_row))
        else:
            # Windows setup
            vc_row = QHBoxLayout()
            vc_row.setContentsMargins(0, 0, 0, 0)
            self._uc_status_lbl = QLabel("Checking...")
            self._uc_status_lbl.setObjectName("status_dim")
            self._uc_btn = QPushButton("Install Driver")
            self._uc_btn.setFixedWidth(150)
            self._uc_btn.clicked.connect(self._install_uc)
            vc_row.addWidget(self._uc_status_lbl, 1)
            vc_row.addWidget(self._uc_btn)
            lay.addLayout(self._row("Virtual Camera", vc_row))

            adb_row = QHBoxLayout()
            adb_row.setContentsMargins(0, 0, 0, 0)
            self._adb_status_lbl = QLabel("Checking...")
            self._adb_status_lbl.setObjectName("status_dim")
            self._adb_btn = None
            adb_row.addWidget(self._adb_status_lbl, 1)
            lay.addLayout(self._row("ADB (USB Mode)", adb_row))

        return gb

    def _on_mode(self):
        self._device_row_w.setVisible(self._rb_wifi.isChecked())
        self._schedule_save()

    # ── Device management ─────────────────────────────────────────────────────

    def _current_device_name(self) -> Optional[str]:
        idx = self._device_combo.currentIndex()
        if idx < 0 or idx >= len(self._devices):
            return None
        return self._devices[idx]["name"]

    def _current_device_ip(self) -> Optional[str]:
        idx = self._device_combo.currentIndex()
        if idx < 0 or idx >= len(self._devices):
            return None
        return self._devices[idx]["ip"]

    def _refresh_device_combo(self, select_name: Optional[str] = None):
        self._switching_device = True
        self._device_combo.blockSignals(True)
        self._device_combo.clear()
        for d in self._devices:
            self._device_combo.addItem(d["name"])
        # Restore selection
        idx = 0
        if select_name:
            for i, d in enumerate(self._devices):
                if d["name"] == select_name:
                    idx = i
                    break
        if self._devices:
            self._device_combo.setCurrentIndex(idx)
        self._device_combo.blockSignals(False)
        self._switching_device = False
        self._update_ip_display()
        self._remove_device_btn.setEnabled(bool(self._devices))

    def _update_ip_display(self):
        ip = self._current_device_ip()
        self._ip_display_lbl.setText(ip or "")

    def _on_device_changed(self, idx: int):
        if self._switching_device:
            return
        name = self._devices[idx]["name"] if 0 <= idx < len(self._devices) else None
        if name and name != self._selected_device:
            # Save settings for the outgoing device before switching
            if self._selected_device:
                self._save_config()
            self._selected_device = name
            self._update_ip_display()
            cfg = self._load_config()
            device_cfg = cfg.get("devices", {}).get(name, {})
            self._apply_device_settings(device_cfg)

    def _on_add_device(self):
        existing = [d["name"] for d in self._devices]
        dlg = AddDeviceDialog(self, existing_names=existing)
        if dlg.exec() != QDialog.DialogCode.Accepted:
            return
        name, ip = dlg.result_values()
        self._devices.append({"name": name, "ip": ip})
        self._refresh_device_combo(select_name=name)
        self._selected_device = name
        self._save_config()

    def _on_remove_device(self):
        name = self._current_device_name()
        if not name:
            return
        r = QMessageBox.question(
            self, "Remove device",
            f'Remove "{name}"? Its saved settings will be deleted.',
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            QMessageBox.StandardButton.No,
        )
        if r != QMessageBox.StandardButton.Yes:
            return
        self._devices = [d for d in self._devices if d["name"] != name]
        # Remove device settings from config file immediately
        cfg = self._load_config()
        cfg.get("devices", {}).pop(name, None)
        cfg["devices"] = cfg.get("devices", {})
        if self._devices:
            new_name = self._devices[0]["name"]
            cfg["selected_device"] = new_name
            self._selected_device = new_name
        else:
            cfg.pop("selected_device", None)
            self._selected_device = None
        try:
            CONFIG_FILE.write_text(json.dumps(cfg, indent=2))
        except Exception:
            pass
        self._refresh_device_combo(select_name=self._selected_device)
        self._update_ip_display()

    # ── Camera control ────────────────────────────────────────────────────────
    def _build_camera_control(self) -> QFrame:
        gb, lay = self._group("Camera", "camera")

        lens_row = QHBoxLayout()
        lens_row.setContentsMargins(0, 0, 0, 0)
        ll = QLabel("Lens")
        ll.setObjectName("dim")
        ll.setFixedWidth(110)
        ll.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        lens_row.addWidget(ll)
        self._lens_panel = LensPanel()
        lens_row.addWidget(self._lens_panel, 1)
        lay.addLayout(lens_row)
        self._lens_panel.lens_selected.connect(self._on_lens_selected)

        self._cam_info_lbl = QLabel("")
        self._cam_info_lbl.setObjectName("dim")
        self._cam_info_lbl.setWordWrap(True)
        lay.addLayout(self._row("", self._cam_info_lbl, stretch=True))

        lay.addWidget(create_separator())

        # Exposure
        exp_row = QHBoxLayout()
        exp_row.setContentsMargins(0, 0, 0, 0)
        el = QLabel("Exposure")
        el.setObjectName("dim")
        el.setFixedWidth(110)
        el.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        exp_row.addWidget(el)
        self._rb_exp_auto   = QRadioButton("Auto")
        self._rb_exp_manual = QRadioButton("Manual")
        for rb in (self._rb_exp_auto, self._rb_exp_manual):
            rb.setAutoExclusive(False)
        self._exp_grp = QButtonGroup(gb)
        self._exp_grp.addButton(self._rb_exp_auto)
        self._exp_grp.addButton(self._rb_exp_manual)
        self._rb_exp_auto.setChecked(True)
        self._exp_grp.buttonClicked.connect(lambda _: self._on_exp_mode())
        exp_row.addWidget(self._rb_exp_auto)
        exp_row.addWidget(self._rb_exp_manual)
        exp_row.addStretch()
        lay.addLayout(exp_row)

        self._iso_slider = LogSliderRow(
            v_min=50, v_max=6400,
            display_fn=lambda v: f"ISO {int(round(v))}",
        )
        self._iso_slider.value_changed.connect(self._on_iso_changed)
        lay.addLayout(self._row("ISO", self._iso_slider, stretch=True))
        self._iso_slider.set_enabled(False)

        self._sht_slider = LogSliderRow(
            v_min=100_000, v_max=1_000_000_000,
            display_fn=lambda v: ns_to_display(int(round(v))),
            spinbox_suffix=" ms",
            spinbox_scale=1e-6,
            spinbox_decimals=2,
        )
        self._sht_slider.value_changed.connect(self._on_shutter_changed)
        lay.addLayout(self._row("Shutter", self._sht_slider, stretch=True))
        self._sht_slider.set_enabled(False)

        lay.addWidget(create_separator())

        # White balance
        wb_row = QHBoxLayout()
        wb_row.setContentsMargins(0, 0, 0, 0)
        wl = QLabel("White bal.")
        wl.setObjectName("dim")
        wl.setFixedWidth(110)
        wl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        wb_row.addWidget(wl)
        self._rb_wb_auto   = QRadioButton("Auto")
        self._rb_wb_manual = QRadioButton("Manual")
        for rb in (self._rb_wb_auto, self._rb_wb_manual):
            rb.setAutoExclusive(False)
        self._wb_grp = QButtonGroup(gb)
        self._wb_grp.addButton(self._rb_wb_auto)
        self._wb_grp.addButton(self._rb_wb_manual)
        self._rb_wb_auto.setChecked(True)
        self._wb_grp.buttonClicked.connect(lambda _: self._on_wb_mode())
        wb_row.addWidget(self._rb_wb_auto)
        wb_row.addWidget(self._rb_wb_manual)
        wb_row.addStretch()
        lay.addLayout(wb_row)

        self._wb_slider = WbSliderRow()
        self._wb_slider.value_changed.connect(self._on_wb_changed)
        lay.addLayout(self._row("Temperature", self._wb_slider, stretch=True))
        self._wb_slider.set_enabled(False)

        lay.addWidget(create_separator())

        # OIS
        ois_row = QHBoxLayout()
        ois_row.setContentsMargins(0, 0, 0, 0)
        ol = QLabel("OIS")
        ol.setObjectName("dim")
        ol.setFixedWidth(110)
        ol.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        ois_row.addWidget(ol)
        self._ois_cb = QCheckBox("Optical Image Stabilization")
        self._ois_cb.setChecked(True)
        self._ois_cb.toggled.connect(self._on_ois)
        ois_row.addWidget(self._ois_cb)
        ois_row.addStretch()
        lay.addLayout(ois_row)

        lay.addWidget(create_separator())

        # Flip & Rotation (Transform)
        flip_row = QHBoxLayout()
        flip_row.setContentsMargins(0, 0, 0, 0)
        fl = QLabel("Flip")
        fl.setObjectName("dim")
        fl.setFixedWidth(110)
        fl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        flip_row.addWidget(fl)
        self._flip_h = QCheckBox("Horizontal")
        self._flip_v = QCheckBox("Vertical")
        self._flip_h.toggled.connect(self._on_flip)
        self._flip_v.toggled.connect(self._on_flip)
        flip_row.addWidget(self._flip_h)
        flip_row.addWidget(self._flip_v)
        flip_row.addStretch()
        lay.addLayout(flip_row)

        self._rot_combo = NoScrollComboBox()
        self._rot_combo.setFixedWidth(150)
        self._rot_combo.addItems(list(ROTATIONS.keys()))
        self._rot_combo.currentTextChanged.connect(self._on_rotate)
        lay.addLayout(self._row("Rotation", self._rot_combo))

        lay.addWidget(create_separator())

        # Zoom
        zoom_row = QHBoxLayout()
        zoom_row.setContentsMargins(0, 0, 0, 0)
        zoom_row.setSpacing(8)
        zoom_lbl = QLabel("Zoom")
        zoom_lbl.setObjectName("dim")
        zoom_lbl.setFixedWidth(110)
        zoom_lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        zoom_row.addWidget(zoom_lbl)
        self._zoom_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._zoom_slider.setRange(100, 500)  # 1.00x – 5.00x stored as integer *100
        self._zoom_slider.setValue(100)
        self._zoom_slider.setMinimumWidth(120)
        self._zoom_val_lbl = QLabel("1.0×")
        self._zoom_val_lbl.setObjectName("val")
        self._zoom_val_lbl.setMinimumWidth(40)
        self._zoom_slider.valueChanged.connect(self._on_zoom_changed)
        zoom_row.addWidget(self._zoom_slider, 1)
        zoom_row.addWidget(self._zoom_val_lbl)
        lay.addLayout(zoom_row)

        # Pan X
        self._pan_x_slider = PanSliderRow("L", "R")
        self._pan_x_slider.value_changed.connect(self._on_pan_changed)
        self._pan_x_row = self._row("Pan X", self._pan_x_slider, stretch=True)
        lay.addLayout(self._pan_x_row)

        # Pan Y
        self._pan_y_slider = PanSliderRow("U", "D")
        self._pan_y_slider.value_changed.connect(self._on_pan_changed)
        self._pan_y_row = self._row("Pan Y", self._pan_y_slider, stretch=True)
        lay.addLayout(self._pan_y_row)

        self._pan_x_slider.set_enabled(False)
        self._pan_y_slider.set_enabled(False)

        return gb

    def _build_stream_output(self) -> QFrame:
        gb, lay = self._group("Stream & Output", "stream")

        self._res_combo = NoScrollComboBox()
        self._res_combo.setFixedWidth(180)
        self._res_combo.addItems(list(RESOLUTIONS.keys()))
        self._res_combo.currentTextChanged.connect(self._on_resolution)
        lay.addLayout(self._row("Resolution", self._res_combo))

        self._fps_spin = NoScrollSpinBox()
        self._fps_spin.setRange(1, 120)
        self._fps_spin.setValue(30)
        self._fps_spin.setSuffix(" fps")
        self._fps_spin.setFixedWidth(90)
        self._fps_spin.editingFinished.connect(self._on_fps)
        lay.addLayout(self._row("Playback FPS", self._fps_spin))

        lay.addWidget(create_separator())

        # JPEG quality row
        q_row = QHBoxLayout()
        q_row.setContentsMargins(0, 0, 0, 0)
        q_row.setSpacing(8)
        q_lbl = QLabel("JPEG Quality")
        q_lbl.setObjectName("dim")
        q_lbl.setFixedWidth(110)
        q_lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        self._quality_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._quality_slider.setRange(50, 100)
        self._quality_slider.setValue(DEFAULT_QUALITY)
        self._quality_slider.setMinimumWidth(120)
        self._quality_slider.setToolTip("Lower quality and FPS reduce bandwidth. Useful on slow Wi-Fi or USB 2.")
        self._quality_val_lbl = QLabel(quality_label(DEFAULT_QUALITY))
        self._quality_val_lbl.setObjectName("val")
        self._quality_val_lbl.setMinimumWidth(110)
        self._quality_slider.valueChanged.connect(self._on_quality_changed)
        q_row.addWidget(q_lbl)
        q_row.addWidget(self._quality_slider, 1)
        q_row.addWidget(self._quality_val_lbl)
        lay.addLayout(q_row)

        # Phone FPS row
        self._phone_fps_spin = NoScrollSpinBox()
        self._phone_fps_spin.setRange(5, 60)
        self._phone_fps_spin.setValue(DEFAULT_PHONE_FPS)
        self._phone_fps_spin.setSuffix(" fps")
        self._phone_fps_spin.setFixedWidth(90)
        self._phone_fps_spin.setToolTip("Lower quality and FPS reduce bandwidth. Useful on slow Wi-Fi or USB 2.")
        self._phone_fps_spin.editingFinished.connect(self._on_phone_fps_changed)
        lay.addLayout(self._row("Phone FPS", self._phone_fps_spin))

        lay.addWidget(create_separator())

        self._batt_alert_spin = NoScrollSpinBox()
        self._batt_alert_spin.setRange(5, 95)
        self._batt_alert_spin.setValue(20)
        self._batt_alert_spin.setSuffix("%")
        self._batt_alert_spin.setFixedWidth(90)
        self._batt_alert_spin.setToolTip("Alert when battery drops below this level while discharging")
        self._batt_alert_spin.editingFinished.connect(self._schedule_save)
        lay.addLayout(self._row("Battery alert", self._batt_alert_spin))

        self._temp_alert_spin = NoScrollSpinBox()
        self._temp_alert_spin.setRange(35, 65)
        self._temp_alert_spin.setValue(45)
        self._temp_alert_spin.setSuffix(" °C")
        self._temp_alert_spin.setFixedWidth(90)
        self._temp_alert_spin.setToolTip("Alert when phone temperature exceeds this")
        self._temp_alert_spin.editingFinished.connect(self._schedule_save)
        lay.addLayout(self._row("Temp alert", self._temp_alert_spin))

        return gb

    # ── v4l2 helpers (Linux) ──────────────────────────────────────────────────
    def _v4l_check(self):
        if v4l2_devices_ready():
            self._v4l_lbl.setObjectName("status_ok")
            self._v4l_lbl.setText(f"Ready: {V4L2_PHONE_DEV} + {V4L2_OBS_DEV}")
        elif v4l2_module_loaded():
            self._v4l_lbl.setObjectName("status_warn")
            self._v4l_lbl.setText(
                f"Module loaded but {V4L2_PHONE_DEV} not found — another config active"
            )
        else:
            self._v4l_lbl.setObjectName("status_err")
            self._v4l_lbl.setText("Not loaded — click Load Module")
        self._v4l_lbl.setStyleSheet("")

    def _v4l_load(self):
        self._v4l_lbl.setText("Loading...")
        self._v4l_lbl.setObjectName("status_dim")
        threading.Thread(
            target=lambda: self._sig_v4l_result.emit(*v4l2_load()),
            daemon=True,
        ).start()

    def _on_v4l_result(self, ok: bool, msg: str):
        self._v4l_lbl.setText(("Loaded — " if ok else "Failed — ") + msg)
        self._v4l_lbl.setObjectName("status_ok" if ok else "status_err")
        self._v4l_lbl.setStyleSheet("")

    # ── Windows setup helpers ─────────────────────────────────────────────────
    def _check_win_setup(self):
        uc_ok  = uc_is_registered()
        adb_ok = adb_available()
        self._sig_win_checks.emit(uc_ok, adb_ok)

    def _on_win_checks(self, uc_ok: bool, adb_ok: bool):
        if self._uc_status_lbl is None:
            return
        if uc_ok:
            self._uc_status_lbl.setObjectName("status_ok")
            self._uc_status_lbl.setText("Ready")
            self._uc_btn.setText("Reinstall")
        else:
            self._uc_status_lbl.setObjectName("status_err")
            self._uc_status_lbl.setText("Not installed")
            dlls = (unitycapture_dir() / "UnityCaptureFilter64.dll").exists()
            self._uc_btn.setText("Install" if dlls else "Download and Install")
        self._uc_status_lbl.setStyleSheet("")

        if adb_ok:
            self._adb_status_lbl.setObjectName("status_ok")
            self._adb_status_lbl.setText("Ready")
        else:
            self._adb_status_lbl.setObjectName("status_err")
            self._adb_status_lbl.setText("Not found (USB mode unavailable)")
        self._adb_status_lbl.setStyleSheet("")

    def _download_adb(self): pass  # no longer used - adb is bundled
    def _on_adb_done(self, ok: bool, msg: str): pass

    def _install_uc(self):
        self._uc_btn.setEnabled(False)
        self._uc_status_lbl.setObjectName("status_dim")
        self._uc_status_lbl.setStyleSheet("")

        def worker():
            if not (unitycapture_dir() / "UnityCaptureFilter64.dll").exists():
                self._sig_uc_msg.emit("Downloading driver files...")
                ok, msg = download_unitycapture()
                if not ok:
                    self._sig_uc_done.emit(False, msg)
                    return
            self._sig_uc_msg.emit("Registering (admin access required)...")
            ok, msg = register_unitycapture()
            self._sig_uc_done.emit(ok, msg)

        threading.Thread(target=worker, daemon=True).start()

    def _on_uc_done(self, ok: bool, msg: str):
        if self._uc_btn is None:
            return
        self._uc_btn.setEnabled(True)
        if ok:
            self._uc_status_lbl.setObjectName("status_ok")
            self._uc_status_lbl.setText("Ready")
            self._uc_btn.setText("Reinstall")
        else:
            self._uc_status_lbl.setObjectName("status_err")
            self._uc_status_lbl.setText(f"Failed: {msg}")
            self._uc_btn.setText("Retry")
        self._uc_status_lbl.setStyleSheet("")

    # ── Camera capability gating ──────────────────────────────────────────────
    def _update_cam_info_lbl(self, cam: dict):
        hw    = cam.get("hwLevel", "")
        parts = []
        if hw:
            parts.append(hw)
        parts.append("manual sensor " + ("✓" if cam.get("supportsManualSensor") else "✗"))
        parts.append("manual WB "     + ("✓" if cam.get("supportsManualWB")     else "✗"))
        parts.append("OIS "           + ("✓" if cam.get("hasOis")               else "✗"))
        self._cam_info_lbl.setText("  ·  ".join(parts))

    def _update_camera_caps(self, supports_manual_sensor: bool, supports_manual_wb: bool):
        """Gray out controls the current camera physically can't support."""
        self._rb_exp_manual.setEnabled(supports_manual_sensor)
        if not supports_manual_sensor:
            self._rb_exp_auto.setChecked(True)
            self._rb_exp_manual.setChecked(False)
            self._manual_exp = False
            self._iso_slider.set_enabled(False)
            self._sht_slider.set_enabled(False)
            self._rb_exp_manual.setToolTip("This camera does not support MANUAL_SENSOR")
        else:
            self._rb_exp_manual.setToolTip("")

        self._rb_wb_manual.setEnabled(supports_manual_wb)
        if not supports_manual_wb:
            self._rb_wb_auto.setChecked(True)
            self._rb_wb_manual.setChecked(False)
            self._manual_wb = False
            self._wb_slider.set_enabled(False)
            self._rb_wb_manual.setToolTip("This camera does not support MANUAL_POST_PROCESSING")
        else:
            self._rb_wb_manual.setToolTip("")

    # ── Camera control handlers ───────────────────────────────────────────────
    def _on_lens_selected(self, cam: dict):
        if self._ctrl:
            self._ctrl.send(action="camera", id=cam["id"])
            self._iso_slider.set_range(cam.get("isoMin", 50), cam.get("isoMax", 6400))
            self._sht_slider.set_range(
                cam.get("shutterMinNs", 100_000),
                cam.get("shutterMaxNs", 1_000_000_000),
            )
            self._update_cam_info_lbl(cam)
            self._update_camera_caps(
                cam.get("supportsManualSensor", True),
                cam.get("supportsManualWB", True),
            )

    def _on_exp_mode(self):
        manual = self._rb_exp_manual.isChecked()
        self._manual_exp = manual
        self._iso_slider.set_enabled(manual)
        self._sht_slider.set_enabled(manual)
        if self._ctrl:
            if not manual:
                self._ctrl.send(action="auto")
            else:
                self._ctrl.send(action="iso",     value=int(self._iso_slider.get_value()))
                self._ctrl.send(action="shutter", value=int(self._sht_slider.get_value()))
        self._schedule_save()

    def _on_iso_changed(self, val: float):
        if self._ctrl and self._manual_exp:
            self._ctrl.send(action="iso", value=int(val))
        self._schedule_save()

    def _on_shutter_changed(self, val: float):
        if self._ctrl and self._manual_exp:
            self._ctrl.send(action="shutter", value=int(val))
        self._schedule_save()

    def _on_wb_mode(self):
        manual = self._rb_wb_manual.isChecked()
        self._manual_wb = manual
        self._wb_slider.set_enabled(manual)
        if self._ctrl:
            if not manual:
                self._ctrl.send(action="wb_auto")
            else:
                self._ctrl.send(action="wb_kelvin", value=self._wb_slider.get_value())

    def _on_wb_changed(self, k: int):
        if self._ctrl and self._manual_wb:
            self._ctrl.send(action="wb_kelvin", value=k)

    def _on_ois(self, checked: bool):
        if self._ctrl:
            self._ctrl.send(action="ois", value="1" if checked else "0")
        self._schedule_save()

    def _on_flip(self):
        if self._worker:
            self._worker.flip_h = self._flip_h.isChecked()
            self._worker.flip_v = self._flip_v.isChecked()
        self._schedule_save()

    def _on_rotate(self):
        if self._worker:
            self._worker.rotation = ROTATIONS.get(self._rot_combo.currentText())
        self._schedule_save()

    def _on_zoom_changed(self, val: int):
        zoom = val / 100.0
        self._zoom_val_lbl.setText(f"{zoom:.1f}×")
        pan_active = zoom > 1.0
        self._pan_x_slider.set_enabled(pan_active)
        self._pan_y_slider.set_enabled(pan_active)
        if not pan_active:
            self._pan_x_slider.reset()
            self._pan_y_slider.reset()
        if self._worker:
            self._worker.zoom  = zoom
            self._worker.pan_x = self._pan_x_slider.get_value() if pan_active else 0.0
            self._worker.pan_y = self._pan_y_slider.get_value() if pan_active else 0.0
        self._schedule_save()

    def _on_pan_changed(self, _val: float):
        if self._worker:
            self._worker.pan_x = self._pan_x_slider.get_value()
            self._worker.pan_y = self._pan_y_slider.get_value()
        self._schedule_save()

    def _on_resolution(self):
        if self._worker:
            res = RESOLUTIONS.get(self._res_combo.currentText())
            w, h = res if res else (None, None)
            self._worker.update_output(width=w, height=h)
        self._schedule_save()

    def _on_fps(self):
        if self._worker:
            self._worker.update_output(fps=self._fps_spin.value())
        self._schedule_save()

    def _on_quality_changed(self, q: int):
        self._quality_val_lbl.setText(quality_label(q))
        if self._ctrl:
            self._ctrl.send(action="jpeg_quality", value=q)
        self._schedule_save()

    def _on_phone_fps_changed(self):
        fps = self._phone_fps_spin.value()
        if self._ctrl:
            self._ctrl.send(action="fps_target", value=fps)
        self._schedule_save()

    # ── Config persistence ────────────────────────────────────────────────────
    def _schedule_save(self):
        self._save_timer.start(500)

    def _load_config(self) -> dict:
        try:
            return json.loads(CONFIG_FILE.read_text())
        except Exception:
            return {}

    def _device_settings_snapshot(self) -> dict:
        """Capture all per-device settings from the current UI state."""
        return {
            "resolution":   self._res_combo.currentText(),
            "fps":          self._fps_spin.value(),
            "flip_h":       self._flip_h.isChecked(),
            "flip_v":       self._flip_v.isChecked(),
            "rotation":     self._rot_combo.currentText(),
            "exp_manual":   self._rb_exp_manual.isChecked(),
            "iso":          self._iso_slider.get_value(),
            "shutter_ns":   self._sht_slider.get_value(),
            "ois":          self._ois_cb.isChecked(),
            "jpeg_quality": self._quality_slider.value(),
            "phone_fps":    self._phone_fps_spin.value(),
            "batt_alert":   self._batt_alert_spin.value(),
            "temp_alert":   self._temp_alert_spin.value(),
            "zoom":         self._zoom_slider.value() / 100.0,
            "pan_x":        self._pan_x_slider.get_value(),
            "pan_y":        self._pan_y_slider.get_value(),
        }

    def _save_config(self):
        cfg = self._load_config()

        # Global (non-device) settings
        cfg["mode"] = "wifi" if self._rb_wifi.isChecked() else "usb"
        cfg["port"] = self._port_field.text()
        cfg["devices_list"] = self._devices  # ordered list with name+ip

        if self._selected_device:
            cfg["selected_device"] = self._selected_device
            devices = cfg.get("devices", {})
            devices[self._selected_device] = self._device_settings_snapshot()
            cfg["devices"] = devices

        # Preserve unitycapture_installed flag
        try:
            existing = json.loads(CONFIG_FILE.read_text())
            if "unitycapture_installed" in existing:
                cfg["unitycapture_installed"] = existing["unitycapture_installed"]
        except Exception:
            pass
        try:
            CONFIG_FILE.write_text(json.dumps(cfg, indent=2))
        except Exception:
            pass

    def _apply_device_settings(self, cfg: dict):
        """Load per-device settings into the UI without touching global fields."""
        if not cfg:
            return
        if res := cfg.get("resolution"):
            idx = self._res_combo.findText(res)
            if idx >= 0:
                self._res_combo.setCurrentIndex(idx)
        if fps := cfg.get("fps"):
            self._fps_spin.setValue(int(fps))
        self._flip_h.setChecked(cfg.get("flip_h", False))
        self._flip_v.setChecked(cfg.get("flip_v", False))
        if rot := cfg.get("rotation"):
            idx = self._rot_combo.findText(rot)
            if idx >= 0:
                self._rot_combo.setCurrentIndex(idx)
        if cfg.get("exp_manual"):
            self._rb_exp_manual.setChecked(True)
            self._rb_exp_auto.setChecked(False)
            self._iso_slider.set_enabled(True)
            self._sht_slider.set_enabled(True)
        if iso := cfg.get("iso"):
            self._iso_slider.set_value(float(iso))
        if sht := cfg.get("shutter_ns"):
            self._sht_slider.set_value(float(sht))
        self._ois_cb.setChecked(cfg.get("ois", True))
        if q := cfg.get("jpeg_quality"):
            self._quality_slider.setValue(int(q))
        if pfps := cfg.get("phone_fps"):
            self._phone_fps_spin.setValue(int(pfps))
        if ba := cfg.get("batt_alert"):
            self._batt_alert_spin.setValue(int(ba))
        if ta := cfg.get("temp_alert"):
            self._temp_alert_spin.setValue(int(ta))
        zoom = cfg.get("zoom", 1.0)
        self._zoom_slider.setValue(int(zoom * 100))
        pan_active = zoom > 1.0
        self._pan_x_slider.set_value(cfg.get("pan_x", 0.0))
        self._pan_y_slider.set_value(cfg.get("pan_y", 0.0))
        self._pan_x_slider.set_enabled(pan_active)
        self._pan_y_slider.set_enabled(pan_active)

    def _apply_config(self, cfg: dict):
        if not cfg:
            return

        # ── Migrate old flat format ───────────────────────────────────────────
        if "ip" in cfg and "devices_list" not in cfg:
            old_ip   = cfg.pop("ip", "")
            old_name = "Phone"
            cfg["devices_list"]    = [{"name": old_name, "ip": old_ip}]
            cfg["selected_device"] = old_name
            cfg["devices"]         = {old_name: {
                k: cfg.pop(k) for k in list(cfg.keys())
                if k in ("resolution", "fps", "flip_h", "flip_v", "rotation",
                         "exp_manual", "iso", "shutter_ns", "ois", "jpeg_quality",
                         "phone_fps", "batt_alert", "temp_alert")
            }}

        # Global settings
        if cfg.get("mode") == "wifi":
            self._rb_wifi.setChecked(True)
            self._rb_usb.setChecked(False)
            self._on_mode()
        if port := cfg.get("port"):
            self._port_field.setText(str(port))

        # Device list
        self._devices = cfg.get("devices_list", [])
        selected = cfg.get("selected_device")
        if self._devices and not selected:
            selected = self._devices[0]["name"]
        self._selected_device = selected
        self._refresh_device_combo(select_name=selected)

        # Per-device settings for the selected device
        device_cfg = cfg.get("devices", {}).get(selected, {}) if selected else {}
        self._apply_device_settings(device_cfg)

    # ── Start / Stop ──────────────────────────────────────────────────────────
    def _toggle(self):
        if self._worker: self._stop()
        else:            self._start()

    def _start(self):
        try:    port = int(self._port_field.text())
        except ValueError:
            QMessageBox.critical(self, "Bad port", "Port must be a number."); return

        if IS_LINUX and not v4l2_devices_ready():
            if v4l2_module_loaded():
                QMessageBox.warning(
                    self, "v4l2loopback conflict",
                    f"v4l2loopback is already loaded but {V4L2_PHONE_DEV} is not available.\n\n"
                    "Another virtual camera setup is using the module. PhoneCam won't touch it.\n\n"
                    "To use PhoneCam's setup instead, first run:\n"
                    "    sudo modprobe -r v4l2loopback\n\n"
                    "Then click Start again."
                )
                return
            r = QMessageBox.question(
                self, "Virtual camera not ready",
                f"The virtual camera module (v4l2loopback) is not loaded.\n\n"
                f"PhoneCam will load it now. This needs admin access and may ask for your password.\n\n"
                f"Devices: {V4L2_PHONE_DEV} (phone), {V4L2_OBS_DEV} (OBS)",
                QMessageBox.StandardButton.Ok | QMessageBox.StandardButton.Cancel,
                QMessageBox.StandardButton.Ok,
            )
            if r != QMessageBox.StandardButton.Ok: return
            ok, msg = v4l2_load()
            if not ok: QMessageBox.critical(self, "Load failed", msg); return

        if self._rb_usb.isChecked():
            if not adb_available():
                QMessageBox.critical(
                    self, "ADB not found",
                    "ADB is needed for USB mode but wasn't found.\n\n"
                    "Click the Download ADB button in the Windows Setup section "
                    "and try again, or switch to Wi-Fi mode."
                )
                return
            ok, msg = adb_forward(port)
            if not ok: QMessageBox.critical(self, "ADB forward failed", msg); return
            self._adb_port = port
            url = f"http://localhost:{port}/video"
        else:
            ip = self._current_device_ip()
            if not ip:
                QMessageBox.critical(self, "No device", "Add a device in Wi-Fi mode first."); return
            url = f"http://{ip}:{port}/video"
            self._adb_port = None

        res = RESOLUTIONS.get(self._res_combo.currentText())
        w, h = res if res else (None, None)
        rotation = ROTATIONS.get(self._rot_combo.currentText())
        zoom  = self._zoom_slider.value() / 100.0
        pan_x = self._pan_x_slider.get_value() if zoom > 1.0 else 0.0
        pan_y = self._pan_y_slider.get_value() if zoom > 1.0 else 0.0

        self._lens_panel.set_placeholder("Loading lenses...")
        self._ctrl   = PhoneControlClient(url)
        self._worker = StreamWorker(
            url=url, width=w, height=h, fps=self._fps_spin.value(),
            flip_h=self._flip_h.isChecked(), flip_v=self._flip_v.isChecked(),
            rotation=rotation, zoom=zoom, pan_x=pan_x, pan_y=pan_y,
        )
        self._worker.status.connect(self._on_worker_status)
        self._worker.start()

        self._battery_notified = False
        self._temp_notified    = False
        self._battery_row.setVisible(True)
        self._battery_timer.start()

        threading.Thread(target=self._fetch_state_async, args=(url,), daemon=True).start()

        self._start_btn.setText("Stop Streaming")
        self._start_btn.setProperty("streaming", True)
        self._start_btn.setStyle(self._start_btn.style())
        self._set_status("Connecting...", "dim")

    def _stop(self):
        if self._worker:
            self._worker.request_stop()
            self._worker = None
        if self._adb_port:
            adb_unforward(self._adb_port); self._adb_port = None
        self._ctrl = None
        self._battery_timer.stop()
        self._battery_row.setVisible(False)
        self._battery_lbl.setText("")
        self._temp_lbl.setText("")
        self._lens_panel.clear()
        self._cam_info_lbl.setText("")
        self._start_btn.setText("Start Streaming")
        self._start_btn.setProperty("streaming", False)
        self._start_btn.setStyle(self._start_btn.style())
        self._fps_lbl.setText("")
        self._set_status("Stopped.", "dim")

    def _fetch_state_async(self, url: str):
        time.sleep(1.5)
        # Push current quality/fps settings to the phone
        if self._ctrl:
            self._ctrl.send(action="jpeg_quality", value=self._quality_slider.value())
            self._ctrl.send(action="fps_target",   value=self._phone_fps_spin.value())
        for _ in range(3):
            if not self._ctrl: return
            state = self._ctrl.get_state()
            if state:
                self._sig_state.emit(state)
                self._sig_battery.emit(
                    int(state.get("battery", 100)),
                    bool(state.get("charging", True)),
                    float(state.get("battery_temp_c", 0.0)),
                )
                return
            time.sleep(2)
        if self._ctrl:
            self._sig_lens_fail.emit()

    def _apply_state(self, state: dict):
        cameras   = state.get("cameras", [])
        is_auto   = state.get("auto", True)
        wb_kelvin = state.get("wb_kelvin")
        ois       = state.get("ois", True)
        iso_val   = state.get("iso")
        sht_val   = state.get("shutter_ns")

        self._lens_panel.load(cameras)

        cur = next((c for c in cameras if c.get("current")), None)
        if cur:
            self._iso_slider.set_range(cur.get("isoMin", 50), cur.get("isoMax", 6400))
            self._sht_slider.set_range(
                cur.get("shutterMinNs", 100_000),
                cur.get("shutterMaxNs", 1_000_000_000),
            )
            self._update_cam_info_lbl(cur)
            self._update_camera_caps(
                cur.get("supportsManualSensor", True),
                cur.get("supportsManualWB", True),
            )

        self._rb_exp_auto.setChecked(is_auto)
        self._rb_exp_manual.setChecked(not is_auto)
        self._manual_exp = not is_auto
        self._iso_slider.set_enabled(not is_auto)
        self._sht_slider.set_enabled(not is_auto)
        if iso_val: self._iso_slider.set_value(float(iso_val))
        if sht_val: self._sht_slider.set_value(float(sht_val))

        manual_wb = wb_kelvin is not None
        self._rb_wb_auto.setChecked(not manual_wb)
        self._rb_wb_manual.setChecked(manual_wb)
        self._manual_wb = manual_wb
        self._wb_slider.set_enabled(manual_wb)
        if wb_kelvin: self._wb_slider.set_value(int(wb_kelvin))

        self._ois_cb.setChecked(bool(ois))

    # ── Battery ───────────────────────────────────────────────────────────────

    def _poll_battery(self):
        if not self._ctrl: return
        threading.Thread(target=self._fetch_battery_async, daemon=True).start()

    def _fetch_battery_async(self):
        if not self._ctrl: return
        state = self._ctrl.get_state()
        if state and "battery" in state:
            self._sig_battery.emit(
                int(state["battery"]),
                bool(state.get("charging", True)),
                float(state.get("battery_temp_c", 0.0)),
            )

    def _setup_tray(self):
        if not QSystemTrayIcon.isSystemTrayAvailable():
            self._tray = None
            return

        px = QPixmap(22, 22)
        px.fill(Qt.GlobalColor.transparent)
        p = QPainter(px)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        p.setBrush(QBrush(QColor("#518cc6")))
        p.setPen(Qt.PenStyle.NoPen)
        p.drawEllipse(1, 1, 20, 20)
        p.setBrush(QBrush(QColor("#1e222b")))
        p.drawEllipse(7, 7, 8, 8)
        p.end()

        self._tray = QSystemTrayIcon(QIcon(px), self)
        self._tray.setToolTip("PhoneCam")

        menu = QMenu()
        show_action = QAction("Show", self)
        quit_action = QAction("Quit", self)
        show_action.triggered.connect(self._tray_show)
        quit_action.triggered.connect(self._tray_quit)
        menu.addAction(show_action)
        menu.addSeparator()
        menu.addAction(quit_action)
        self._tray.setContextMenu(menu)

        self._tray.activated.connect(self._on_tray_activated)
        self._tray.show()
        self._tray_close_notified = False

    def _tray_show(self):
        self.showNormal()
        self.raise_()
        self.activateWindow()

    def _tray_quit(self):
        self._tray_close_notified = True
        self._stop()
        QApplication.quit()

    def _on_tray_activated(self, reason):
        if reason == QSystemTrayIcon.ActivationReason.Trigger:
            if self.isVisible():
                self.hide()
            else:
                self._tray_show()

    def _send_notification(self, title: str, body: str):
        if IS_LINUX and shutil.which("notify-send"):
            subprocess.Popen(
                ["notify-send", "-a", "PhoneCam", "-u", "critical", title, body],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        elif self._tray:
            self._tray.showMessage(title, body, QSystemTrayIcon.MessageIcon.Warning, 0)

    def _on_battery(self, level: int, charging: bool, temp_c: float):
        batt_thresh = self._batt_alert_spin.value()
        temp_thresh = self._temp_alert_spin.value()

        # Battery label
        charge_icon = "  [charging]" if charging else ""
        if not charging and level <= batt_thresh:
            batt_obj = "status_err"
        elif not charging and level <= batt_thresh + 10:
            batt_obj = "status_warn"
        else:
            batt_obj = "status_ok"
        self._battery_lbl.setText(f"{level}%{charge_icon}")
        self._battery_lbl.setStyleSheet(f"color: {STATUS_COLORS[batt_obj]};")

        # Temp label
        if temp_c >= temp_thresh:
            temp_obj = "status_err"
        elif temp_c >= temp_thresh - 5:
            temp_obj = "status_warn"
        else:
            temp_obj = "status_ok"
        self._temp_lbl.setText(f"{temp_c:.1f} °C")
        self._temp_lbl.setStyleSheet(f"color: {STATUS_COLORS[temp_obj]};")

        # Low battery notification (once per crossing, resets with 5% hysteresis)
        if not charging and level <= batt_thresh and not self._battery_notified:
            self._battery_notified = True
            self._send_notification("PhoneCam - Low Battery",
                                    f"Phone battery is at {level}%.")
        elif level > batt_thresh + 5:
            self._battery_notified = False

        # High temp notification (5 deg hysteresis on reset)
        if temp_c >= temp_thresh and not self._temp_notified:
            self._temp_notified = True
            self._send_notification("PhoneCam - Phone Running Hot",
                                    f"Temperature is {temp_c:.1f} C. Consider stopping charging or closing other apps.")
        elif temp_c < temp_thresh - 5:
            self._temp_notified = False

    # ── Worker status ─────────────────────────────────────────────────────────

    def _on_worker_status(self, kind: str, msg: str):
        if kind == "fps":
            self._fps_lbl.setText(msg)
        elif kind == "ok":
            self._set_status(msg, "ok")
        elif kind == "warn":
            self._set_status(msg, "warn")
        elif kind == "idle":
            self._fps_lbl.setText("")
            self._set_status(msg, "dim")
            if self._worker:
                self._worker = None
                self._start_btn.setText("Start Streaming")
                self._start_btn.setProperty("streaming", False)
                self._start_btn.setStyle(self._start_btn.style())
        else:
            self._set_status(msg, "dim")

    def _set_status(self, msg: str, kind: str):
        obj = {"ok": "status_ok", "warn": "status_warn",
               "err": "status_err", "dim": "status_dim"}.get(kind, "status_dim")
        self._status_lbl.setObjectName(obj)
        self._status_lbl.setText(msg)
        self._status_lbl.setStyleSheet("")

    def closeEvent(self, event):
        if self._tray:
            event.ignore()
            self.hide()
            if not self._tray_close_notified:
                self._tray_close_notified = True
                self._send_notification(
                    "PhoneCam is still running",
                    "Streaming continues in the background. Right-click the tray icon to quit.",
                )
        else:
            self._stop()
            event.accept()


# ── Entry point ───────────────────────────────────────────────────────────────
def main():
    app = QApplication(sys.argv)

    # Single-instance check -- must happen before showing any UI
    srv = acquire_single_instance()
    if srv is None:
        # Another instance is running and has been asked to raise its window
        sys.exit(0)

    # Apply qt-material theme
    apply_stylesheet(app, theme='dark_blue.xml')

    # Append custom status styling and button properties
    app.setStyleSheet(app.styleSheet() + EXTRA_QSS)

    win = PhoneCamWindow()
    win.show()

    # Listen for raise signals from future instances
    threading.Thread(
        target=listen_for_raise,
        args=(srv, win._sig_raise.emit),
        daemon=True,
    ).start()

    ret = app.exec()
    srv.close()
    sys.exit(ret)


if __name__ == "__main__":
    main()
