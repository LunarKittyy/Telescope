#!/usr/bin/env python3
"""
PhoneCam Desktop
Receives MJPEG stream from PhoneCam Android app → virtual camera.
  Linux  : /dev/video10 via v4l2loopback
  Windows: "Unity Video Capture" via UnityCapture

Control API: sends HTTP GET requests to phone's /control endpoint.
"""

import json
import math
import platform
import shutil
import subprocess
import sys
import threading
import time
import urllib.request
from typing import Optional

# ── Dependency check ─────────────────────────────────────────────────────────
_missing = []
try:    from PyQt6.QtCore import Qt  # noqa: F401
except ImportError: _missing.append("PyQt6")
try:    import cv2
except ImportError: _missing.append("opencv-python")
try:    import numpy as np  # noqa: F401
except ImportError: _missing.append("numpy")
try:    import pyvirtualcam
except ImportError: _missing.append("pyvirtualcam")

if _missing:
    print(f"Missing: pip install {' '.join(_missing)}", file=sys.stderr)
    sys.exit(1)

from PyQt6.QtCore import (
    QThread, pyqtSignal, Qt, QTimer, QSize,
)
from PyQt6.QtGui import QFont, QColor, QPalette, QIntValidator
from PyQt6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QScrollArea,
    QVBoxLayout, QHBoxLayout, QGridLayout,
    QGroupBox, QLabel, QLineEdit, QSpinBox,
    QComboBox, QCheckBox, QRadioButton, QButtonGroup,
    QPushButton, QSlider, QTextEdit, QFrame,
    QSizePolicy, QMessageBox,
)

# ── Platform ──────────────────────────────────────────────────────────────────
IS_LINUX   = platform.system() == "Linux"
IS_WINDOWS = platform.system() == "Windows"

VCAM_BACKEND   = "v4l2loopback" if IS_LINUX else "unitycapture"
V4L2_PHONE_DEV = "/dev/video10"
V4L2_OBS_DEV   = "/dev/video11"
DEFAULT_PORT   = 8080
RECONNECT_DELAY = 3

# ── Options ───────────────────────────────────────────────────────────────────
RESOLUTIONS = {
    "Pass-through (auto)": None,
    "1920 × 1080": (1920, 1080),
    "1280 × 720":  (1280,  720),
    "854 × 480":   ( 854,  480),
    "640 × 360":   ( 640,  360),
}
ROTATIONS = {
    "None":    None,
    "90° CW":  cv2.ROTATE_90_CLOCKWISE,
    "180°":    cv2.ROTATE_180,
    "90° CCW": cv2.ROTATE_90_COUNTERCLOCKWISE,
}
WB_NAMES = [
    (2000, "Candlelight"),
    (2700, "Incandescent"),
    (3200, "Warm white"),
    (4000, "Fluorescent"),
    (5500, "Daylight"),
    (6500, "Overcast"),
    (7500, "Shade"),
    (8000, "Deep shade"),
]

# ── Catppuccin Mocha (Windows theme) ─────────────────────────────────────────
_CAT = {
    "base":     "#1e1e2e",
    "mantle":   "#181825",
    "surface0": "#313244",
    "surface1": "#45475a",
    "overlay0": "#6c7086",
    "text":     "#cdd6f4",
    "subtext0": "#a6adc8",
    "mauve":    "#cba6f7",
    "mauve_l":  "#d9b8ff",
    "green":    "#a6e3a1",
    "yellow":   "#f9e2af",
    "red":      "#f38ba8",
    "blue":     "#89b4fa",
    "sel":      "#2a2444",
}

CATPPUCCIN_QSS = f"""
QMainWindow, QWidget {{
    background: {_CAT['base']};
    color: {_CAT['text']};
    font-size: 10pt;
}}
QScrollArea, QScrollArea > QWidget > QWidget {{
    background: {_CAT['base']};
    border: none;
}}
QGroupBox {{
    background: {_CAT['surface0']};
    border: 1px solid {_CAT['surface1']};
    border-radius: 6px;
    margin-top: 10px;
    padding: 10px 8px 8px 8px;
    font-size: 8pt;
    font-weight: bold;
    color: {_CAT['overlay0']};
    text-transform: uppercase;
    letter-spacing: 1px;
}}
QGroupBox::title {{
    subcontrol-origin: margin;
    left: 10px;
    padding: 0 4px;
    background: {_CAT['surface0']};
}}
QLineEdit, QSpinBox {{
    background: {_CAT['mantle']};
    border: 1px solid {_CAT['surface1']};
    border-radius: 4px;
    color: {_CAT['text']};
    padding: 4px 6px;
    selection-background-color: {_CAT['mauve']};
}}
QSpinBox::up-button, QSpinBox::down-button {{
    background: {_CAT['surface1']};
    border: none;
    width: 16px;
}}
QComboBox {{
    background: {_CAT['mantle']};
    border: 1px solid {_CAT['surface1']};
    border-radius: 4px;
    color: {_CAT['text']};
    padding: 4px 8px;
    min-width: 100px;
}}
QComboBox::drop-down {{ border: none; width: 20px; }}
QComboBox QAbstractItemView {{
    background: {_CAT['surface0']};
    color: {_CAT['text']};
    selection-background-color: {_CAT['mauve']};
    border: 1px solid {_CAT['surface1']};
    outline: none;
}}
QSlider::groove:horizontal {{
    height: 4px;
    background: {_CAT['surface1']};
    border-radius: 2px;
}}
QSlider::sub-page:horizontal {{
    background: {_CAT['sel']};
    border-radius: 2px;
}}
QSlider::handle:horizontal {{
    background: {_CAT['mauve']};
    width: 14px;
    height: 14px;
    margin: -5px 0;
    border-radius: 7px;
}}
QSlider::handle:horizontal:hover {{ background: {_CAT['mauve_l']}; }}
QSlider:disabled::handle:horizontal {{ background: {_CAT['surface1']}; }}
QSlider:disabled::sub-page:horizontal {{ background: {_CAT['surface0']}; }}
QPushButton {{
    background: {_CAT['surface0']};
    color: {_CAT['text']};
    border: 1px solid {_CAT['surface1']};
    border-radius: 4px;
    padding: 5px 12px;
}}
QPushButton:hover {{
    background: {_CAT['sel']};
    color: {_CAT['mauve_l']};
    border-color: {_CAT['mauve']};
}}
QPushButton:pressed {{ background: {_CAT['mantle']}; }}
QPushButton:checked {{
    background: {_CAT['sel']};
    color: {_CAT['mauve_l']};
    border-color: {_CAT['mauve']};
}}
QPushButton#start_btn {{
    background: {_CAT['mauve']};
    color: {_CAT['base']};
    border: none;
    font-size: 11pt;
    font-weight: bold;
    padding: 12px;
    border-radius: 6px;
}}
QPushButton#start_btn:hover {{ background: {_CAT['mauve_l']}; }}
QPushButton#start_btn[streaming=true] {{
    background: {_CAT['surface0']};
    border: 1px solid {_CAT['surface1']};
    color: {_CAT['subtext0']};
    font-size: 11pt;
}}
QRadioButton, QCheckBox {{
    color: {_CAT['text']};
    spacing: 6px;
}}
QRadioButton::indicator {{
    width: 14px; height: 14px;
    border: 1px solid {_CAT['overlay0']};
    border-radius: 7px;
    background: {_CAT['mantle']};
}}
QCheckBox::indicator {{
    width: 14px; height: 14px;
    border: 1px solid {_CAT['overlay0']};
    border-radius: 3px;
    background: {_CAT['mantle']};
}}
QRadioButton::indicator:checked, QCheckBox::indicator:checked {{
    background: {_CAT['mauve']};
    border-color: {_CAT['mauve']};
}}
QLabel#dim   {{ color: {_CAT['subtext0']}; font-size: 9pt; }}
QLabel#val   {{ color: {_CAT['blue']}; font-family: monospace; font-size: 9pt; }}
QLabel#status_ok   {{ color: {_CAT['green']}; }}
QLabel#status_warn {{ color: {_CAT['yellow']}; }}
QLabel#status_err  {{ color: {_CAT['red']}; }}
QLabel#status_dim  {{ color: {_CAT['overlay0']}; }}
QLabel#fps_lbl {{ color: {_CAT['mauve']}; font-family: monospace; font-size: 9pt; }}
QTextEdit {{
    background: {_CAT['mantle']};
    color: {_CAT['subtext0']};
    border: 1px solid {_CAT['surface1']};
    border-radius: 4px;
    font-family: monospace;
    font-size: 9pt;
    padding: 4px;
}}
QScrollBar:vertical {{
    background: {_CAT['base']};
    width: 7px;
}}
QScrollBar::handle:vertical {{
    background: {_CAT['surface1']};
    border-radius: 3px;
    min-height: 20px;
}}
QScrollBar::handle:vertical:hover {{ background: {_CAT['overlay0']}; }}
QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical {{ height: 0; }}
QScrollBar:horizontal {{ height: 7px; background: {_CAT['base']}; }}
QScrollBar::handle:horizontal {{
    background: {_CAT['surface1']};
    border-radius: 3px;
    min-width: 20px;
}}
QScrollBar::add-line:horizontal, QScrollBar::sub-line:horizontal {{ width: 0; }}
"""

# Minimal semantic stylesheet for Linux (status colors only, rest is system theme)
LINUX_QSS = """
QLabel#status_ok   { color: #4ade80; }
QLabel#status_warn { color: #f59e0b; }
QLabel#status_err  { color: #f87171; }
QLabel#fps_lbl     { font-family: monospace; font-size: 9pt; }
QLabel#val         { font-family: monospace; font-size: 9pt; }
"""

# ── Helpers ───────────────────────────────────────────────────────────────────

def _run(cmd, timeout=10):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout, r.stderr
    except FileNotFoundError:        return -1, "", f"Not found: {cmd[0]}"
    except subprocess.TimeoutExpired: return -2, "", "Timed out"

def v4l2_is_loaded():
    import os
    rc, out, _ = _run(["lsmod"])
    return rc == 0 and "v4l2loopback" in out and os.path.exists(V4L2_PHONE_DEV)

def v4l2_load():
    priv = ["pkexec"] if shutil.which("pkexec") else ["sudo"]
    subprocess.run(priv + ["modprobe", "-r", "v4l2loopback"], capture_output=True)
    rc, _, err = _run(priv + ["modprobe", "v4l2loopback",
        "devices=2", "video_nr=10,11",
        "card_label=Phone Camera,OBS Virtual Camera",
        "exclusive_caps=1"], timeout=60)
    return (True, f"Loaded – {V4L2_PHONE_DEV} + {V4L2_OBS_DEV}") \
        if rc == 0 else (False, err or "modprobe failed")

def adb_available():  return shutil.which("adb") is not None
def adb_forward(port):
    rc, _, err = _run(["adb", "forward", f"tcp:{port}", f"tcp:{port}"])
    return (True, f"Port {port} forwarded") if rc == 0 else (False, err)
def adb_unforward(port): _run(["adb", "forward", "--remove", f"tcp:{port}"])

def ns_to_display(ns: int) -> str:
    if ns <= 0: return "?"
    s = ns / 1_000_000_000.0
    if s >= 1.0:
        return f"{s:.1f} s"
    denom = round(1.0 / s)
    return f"1/{denom:,}"

def wb_name(k: int) -> str:
    return min(WB_NAMES, key=lambda x: abs(x[0] - k))[1]

def log_pos_to_val(pos: int, steps: int, v_min: float, v_max: float) -> float:
    if v_min <= 0: v_min = 1
    t = pos / max(steps, 1)
    return math.exp(math.log(v_min) + t * (math.log(v_max) - math.log(v_min)))

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
                 fps: int, flip_h: bool, flip_v: bool, rotation):
        super().__init__()
        self.url = url
        self._width    = width
        self._height   = height
        self._fps      = fps
        self.flip_h    = flip_h
        self.flip_v    = flip_v
        self.rotation  = rotation
        self._stop_flag    = False
        self._restart_vcam = threading.Event()

    def update_output(self, width=None, height=None, fps=None):
        if width  is not None: self._width  = width
        if height is not None: self._height = height
        if fps    is not None: self._fps    = fps
        self._restart_vcam.set()

    def request_stop(self):
        self._stop_flag = True
        self._restart_vcam.set()

    def run(self):
        self.status.emit("info", f"Connecting to {self.url} …")
        while not self._stop_flag:
            cap = cv2.VideoCapture(self.url)
            cap.set(cv2.CAP_PROP_OPEN_TIMEOUT_MSEC, 3000)
            cap.set(cv2.CAP_PROP_READ_TIMEOUT_MSEC, 3000)
            if not cap.isOpened():
                self.status.emit("warn", f"Cannot open stream – retry in {RECONNECT_DELAY}s …")
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue

            ret, frame = cap.read()
            if not ret or frame is None:
                cap.release()
                self.status.emit("warn", "Empty first frame – retrying …")
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue

            frame = transform_frame(frame, self.flip_h, self.flip_v, self.rotation)
            w = self._width  or frame.shape[1]
            h = self._height or frame.shape[0]
            if self._width or self._height:
                frame = cv2.resize(frame, (w, h))

            self.status.emit("ok", f"Stream {w}×{h} @ {self._fps} fps → {VCAM_BACKEND}")
            self._restart_vcam.clear()

            try:
                with pyvirtualcam.Camera(width=w, height=h, fps=self._fps,
                                         backend=VCAM_BACKEND) as cam:
                    self.status.emit("ok", f"Virtual camera: {cam.device}")
                    fc, t0 = 0, time.time()

                    while not self._stop_flag:
                        if self._restart_vcam.is_set():
                            self._restart_vcam.clear()
                            break

                        ret, raw = cap.read()
                        if not ret or raw is None:
                            self.status.emit("warn", "Stream dropped – reconnecting …")
                            break

                        raw = transform_frame(raw, self.flip_h, self.flip_v, self.rotation)
                        if self._width or self._height:
                            raw = cv2.resize(raw, (w, h))
                        cam.send(cv2.cvtColor(raw, cv2.COLOR_BGR2RGB))
                        cam.sleep_until_next_frame()

                        fc += 1
                        if (elapsed := time.time() - t0) >= 2.0:
                            self.status.emit("fps", f"{fc/elapsed:.1f} fps  {w}×{h}")
                            fc, t0 = 0, time.time()

            except Exception as exc:
                self.status.emit("warn", f"Virtual cam error: {exc}")

            cap.release()
            if not self._stop_flag and not self._restart_vcam.is_set():
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
        self._cameras: list[dict] = []
        self._btns:    list[QPushButton] = []
        self._ph = QLabel("Start streaming to load lenses")
        self._ph.setObjectName("dim")
        self._layout.addWidget(self._ph, 0, 0)

    def load(self, cameras: list[dict]):
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

    def clear(self):
        for b in self._btns: b.deleteLater()
        self._btns.clear()
        self._cameras.clear()
        self._ph.show()

# ── Log-scale slider row ──────────────────────────────────────────────────────
class LogSliderRow(QWidget):
    """Horizontal slider on log scale + spinbox for direct entry."""
    value_changed = pyqtSignal(float)
    STEPS = 2000

    def __init__(self, v_min: float, v_max: float,
                 display_fn=None, spinbox_suffix: str = "",
                 spinbox_decimals: int = 0, parent=None):
        super().__init__(parent)
        self.v_min = v_min
        self.v_max = v_max
        self.display_fn = display_fn or str
        self._debounce: Optional[QTimer] = None

        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        self._slider = QSlider(Qt.Orientation.Horizontal)
        self._slider.setRange(0, self.STEPS)
        self._slider.setValue(0)
        self._slider.setMinimumWidth(140)
        lay.addWidget(self._slider, 1)

        self._val_lbl = QLabel(display_fn(v_min) if display_fn else str(v_min))
        self._val_lbl.setObjectName("val")
        self._val_lbl.setMinimumWidth(70)
        lay.addWidget(self._val_lbl)

        self._spin = QSpinBox()
        self._spin.setRange(int(v_min), int(v_max))
        self._spin.setSuffix(spinbox_suffix)
        self._spin.setFixedWidth(100)
        lay.addWidget(self._spin)

        self._slider.valueChanged.connect(self._on_slider)
        self._spin.editingFinished.connect(self._on_spin)

    def _on_slider(self, pos: int):
        val = log_pos_to_val(pos, self.STEPS, self.v_min, self.v_max)
        self._val_lbl.setText(self.display_fn(val))
        self._spin.blockSignals(True)
        self._spin.setValue(int(val))
        self._spin.blockSignals(False)
        self._schedule_emit(val)

    def _on_spin(self):
        val = float(self._spin.value())
        pos = val_to_log_pos(val, self.STEPS, self.v_min, self.v_max)
        self._slider.blockSignals(True)
        self._slider.setValue(pos)
        self._slider.blockSignals(False)
        self._val_lbl.setText(self.display_fn(val))
        self._schedule_emit(val)

    def _schedule_emit(self, val: float):
        if self._debounce:
            self._debounce.stop()
        self._debounce = QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.timeout.connect(lambda: self.value_changed.emit(val))
        self._debounce.start(300)

    def set_range(self, v_min: float, v_max: float):
        self.v_min = v_min
        self.v_max = v_max
        self._spin.setRange(int(v_min), int(v_max))
        cur_pos = self._slider.value()
        val = log_pos_to_val(cur_pos, self.STEPS, v_min, v_max)
        self._val_lbl.setText(self.display_fn(val))

    def get_value(self) -> float:
        return log_pos_to_val(self._slider.value(), self.STEPS, self.v_min, self.v_max)

    def set_value(self, val: float):
        pos = val_to_log_pos(val, self.STEPS, self.v_min, self.v_max)
        self._slider.blockSignals(True)
        self._slider.setValue(pos)
        self._slider.blockSignals(False)
        self._spin.blockSignals(True)
        self._spin.setValue(int(val))
        self._spin.blockSignals(False)
        self._val_lbl.setText(self.display_fn(val))

    def set_enabled(self, enabled: bool):
        self._slider.setEnabled(enabled)
        self._spin.setEnabled(enabled)

# ── WB slider row ─────────────────────────────────────────────────────────────
class WbSliderRow(QWidget):
    """Linear Kelvin slider 2000–8000 + spinbox for direct entry."""
    value_changed = pyqtSignal(int)

    def __init__(self, parent=None):
        super().__init__(parent)
        lay = QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        self._slider = QSlider(Qt.Orientation.Horizontal)
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

        self._spin = QSpinBox()
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
        self._debounce.start(300)

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

# ── Main window ───────────────────────────────────────────────────────────────
class PhoneCamWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("PhoneCam")
        self.setMinimumSize(520, 480)
        self.resize(540, 780)

        self._worker: Optional[StreamWorker] = None
        self._ctrl:   Optional[PhoneControlClient] = None
        self._adb_port: Optional[int] = None
        self._manual_exp = False
        self._manual_wb  = False

        self._build_ui()

    def _build_ui(self):
        root = QWidget()
        self.setCentralWidget(root)
        root_lay = QVBoxLayout(root)
        root_lay.setContentsMargins(0, 0, 0, 0)
        root_lay.setSpacing(0)

        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        content = QWidget()
        c_lay = QVBoxLayout(content)
        c_lay.setContentsMargins(14, 14, 14, 8)
        c_lay.setSpacing(8)
        scroll.setWidget(content)
        root_lay.addWidget(scroll, 1)

        # Header
        hdr = QHBoxLayout()
        title = QLabel("PhoneCam")
        f = title.font(); f.setPointSize(14); f.setBold(True); title.setFont(f)
        hdr.addWidget(title)
        hdr.addStretch()
        self._dot = QLabel("●")
        f2 = self._dot.font(); f2.setPointSize(14); self._dot.setFont(f2)
        self._dot.setObjectName("status_dim")
        hdr.addWidget(self._dot)
        c_lay.addLayout(hdr)

        c_lay.addWidget(self._build_connection())
        c_lay.addWidget(self._build_camera_control())
        c_lay.addWidget(self._build_transform())
        c_lay.addWidget(self._build_output())
        c_lay.addWidget(self._build_platform_setup())
        c_lay.addWidget(self._build_status())
        c_lay.addStretch()

        # Start/Stop – pinned at bottom, always visible
        btn_frame = QWidget()
        btn_lay = QVBoxLayout(btn_frame)
        btn_lay.setContentsMargins(14, 6, 14, 14)
        self._start_btn = QPushButton("Start Streaming")
        self._start_btn.setObjectName("start_btn")
        self._start_btn.setProperty("streaming", False)
        self._start_btn.clicked.connect(self._toggle)
        btn_lay.addWidget(self._start_btn)
        root_lay.addWidget(btn_frame)

    def _group(self, title: str) -> tuple[QGroupBox, QVBoxLayout]:
        gb = QGroupBox(title)
        lay = QVBoxLayout(gb)
        lay.setContentsMargins(8, 8, 8, 8)
        lay.setSpacing(6)
        return gb, lay

    def _row(self, label: str, widget: QWidget, label_width=90) -> QHBoxLayout:
        lay = QHBoxLayout()
        lay.setSpacing(8)
        lbl = QLabel(label)
        lbl.setObjectName("dim")
        lbl.setFixedWidth(label_width)
        lay.addWidget(lbl)
        lay.addWidget(widget, 1)
        return lay

    # ── Connection ────────────────────────────────────────────────────────────
    def _build_connection(self) -> QGroupBox:
        gb, lay = self._group("Connection")

        mode_row = QHBoxLayout()
        mode_lbl = QLabel("Mode"); mode_lbl.setObjectName("dim"); mode_lbl.setFixedWidth(90)
        mode_row.addWidget(mode_lbl)
        self._rb_usb  = QRadioButton("USB (ADB)")
        self._rb_wifi = QRadioButton("Wi-Fi")
        self._rb_usb.setChecked(True)
        self._rb_usb.toggled.connect(self._on_mode)
        mode_row.addWidget(self._rb_usb)
        mode_row.addWidget(self._rb_wifi)
        mode_row.addStretch()
        lay.addLayout(mode_row)

        self._ip_field = QLineEdit("192.168.1.x")
        self._ip_field.setMaximumWidth(200)
        self._ip_row_w = QWidget()
        self._ip_row_w.setLayout(self._row("Phone IP", self._ip_field))
        lay.addWidget(self._ip_row_w)
        self._ip_row_w.setVisible(False)

        self._port_field = QLineEdit(str(DEFAULT_PORT))
        self._port_field.setValidator(QIntValidator(1, 65535))
        self._port_field.setMaximumWidth(80)
        lay.addLayout(self._row("Port", self._port_field))

        return gb

    def _on_mode(self):
        self._ip_row_w.setVisible(self._rb_wifi.isChecked())

    # ── Camera control ────────────────────────────────────────────────────────
    def _build_camera_control(self) -> QGroupBox:
        gb, lay = self._group("Camera")

        # Lens selector
        lens_row = QHBoxLayout()
        ll = QLabel("Lens"); ll.setObjectName("dim"); ll.setFixedWidth(90)
        lens_row.addWidget(ll)
        self._lens_panel = LensPanel()
        lens_row.addWidget(self._lens_panel, 1)
        lay.addLayout(lens_row)
        self._lens_panel.lens_selected.connect(self._on_lens_selected)

        sep = QFrame(); sep.setFrameShape(QFrame.Shape.HLine); lay.addWidget(sep)

        # Exposure
        exp_row = QHBoxLayout()
        el = QLabel("Exposure"); el.setObjectName("dim"); el.setFixedWidth(90)
        exp_row.addWidget(el)
        self._rb_exp_auto   = QRadioButton("Auto")
        self._rb_exp_manual = QRadioButton("Manual")
        self._rb_exp_auto.setChecked(True)
        self._rb_exp_auto.toggled.connect(self._on_exp_mode)
        exp_row.addWidget(self._rb_exp_auto)
        exp_row.addWidget(self._rb_exp_manual)
        exp_row.addStretch()
        lay.addLayout(exp_row)

        self._iso_slider = LogSliderRow(
            v_min=50, v_max=6400,
            display_fn=lambda v: f"ISO {int(v)}",
        )
        self._iso_slider.value_changed.connect(self._on_iso_changed)
        lay.addLayout(self._row("ISO", self._iso_slider))
        self._iso_slider.set_enabled(False)

        self._sht_slider = LogSliderRow(
            v_min=100_000, v_max=1_000_000_000,
            display_fn=lambda v: ns_to_display(int(v)),
            spinbox_suffix=" ns",
        )
        self._sht_slider.value_changed.connect(self._on_shutter_changed)
        lay.addLayout(self._row("Shutter", self._sht_slider))
        self._sht_slider.set_enabled(False)
        self._sht_slider._spin.setRange(100_000, 1_000_000_000)

        sep2 = QFrame(); sep2.setFrameShape(QFrame.Shape.HLine); lay.addWidget(sep2)

        # White balance
        wb_row = QHBoxLayout()
        wl = QLabel("White bal."); wl.setObjectName("dim"); wl.setFixedWidth(90)
        wb_row.addWidget(wl)
        self._rb_wb_auto   = QRadioButton("Auto")
        self._rb_wb_manual = QRadioButton("Manual")
        self._rb_wb_auto.setChecked(True)
        self._rb_wb_auto.toggled.connect(self._on_wb_mode)
        wb_row.addWidget(self._rb_wb_auto)
        wb_row.addWidget(self._rb_wb_manual)
        wb_row.addStretch()
        lay.addLayout(wb_row)

        self._wb_slider = WbSliderRow()
        self._wb_slider.value_changed.connect(self._on_wb_changed)
        lay.addLayout(self._row("Temperature", self._wb_slider))
        self._wb_slider.set_enabled(False)

        sep3 = QFrame(); sep3.setFrameShape(QFrame.Shape.HLine); lay.addWidget(sep3)

        # OIS
        ois_row = QHBoxLayout()
        ol = QLabel("OIS"); ol.setObjectName("dim"); ol.setFixedWidth(90)
        ois_row.addWidget(ol)
        self._ois_cb = QCheckBox("Optical Image Stabilization")
        self._ois_cb.setChecked(True)
        self._ois_cb.toggled.connect(self._on_ois)
        ois_row.addWidget(self._ois_cb)
        ois_row.addStretch()
        lay.addLayout(ois_row)

        return gb

    # ── Transform ─────────────────────────────────────────────────────────────
    def _build_transform(self) -> QGroupBox:
        gb, lay = self._group("Transform")

        flip_row = QHBoxLayout()
        fl = QLabel("Flip"); fl.setObjectName("dim"); fl.setFixedWidth(90)
        flip_row.addWidget(fl)
        self._flip_h = QCheckBox("Horizontal")
        self._flip_v = QCheckBox("Vertical")
        self._flip_h.toggled.connect(self._on_flip)
        self._flip_v.toggled.connect(self._on_flip)
        flip_row.addWidget(self._flip_h)
        flip_row.addWidget(self._flip_v)
        flip_row.addStretch()
        lay.addLayout(flip_row)

        self._rot_combo = QComboBox()
        self._rot_combo.addItems(list(ROTATIONS.keys()))
        self._rot_combo.currentTextChanged.connect(self._on_rotate)
        lay.addLayout(self._row("Rotation", self._rot_combo))

        return gb

    # ── Output ────────────────────────────────────────────────────────────────
    def _build_output(self) -> QGroupBox:
        gb, lay = self._group("Output")

        self._res_combo = QComboBox()
        self._res_combo.addItems(list(RESOLUTIONS.keys()))
        self._res_combo.currentTextChanged.connect(self._on_resolution)
        lay.addLayout(self._row("Resolution", self._res_combo))

        self._fps_spin = QSpinBox()
        self._fps_spin.setRange(1, 120)
        self._fps_spin.setValue(30)
        self._fps_spin.setSuffix(" fps")
        self._fps_spin.setFixedWidth(90)
        self._fps_spin.editingFinished.connect(self._on_fps)
        lay.addLayout(self._row("Target FPS", self._fps_spin))

        return gb

    # ── Platform setup ────────────────────────────────────────────────────────
    def _build_platform_setup(self) -> QGroupBox:
        if IS_LINUX:
            gb, lay = self._group("v4l2loopback")
            self._v4l_lbl = QLabel("Status unknown")
            self._v4l_lbl.setObjectName("status_dim")
            lay.addWidget(self._v4l_lbl)
            btn_row = QHBoxLayout()
            chk_btn  = QPushButton("Check")
            load_btn = QPushButton("Load module")
            chk_btn.clicked.connect(self._v4l_check)
            load_btn.clicked.connect(self._v4l_load)
            btn_row.addWidget(chk_btn)
            btn_row.addWidget(load_btn)
            btn_row.addStretch()
            lay.addLayout(btn_row)
            note = QLabel(
                f"Needs: devices=2  video_nr=10,11  exclusive_caps=1\n"
                f"{V4L2_PHONE_DEV} = phone  ·  {V4L2_OBS_DEV} = OBS"
            )
            note.setObjectName("dim")
            lay.addWidget(note)
        else:
            gb, lay = self._group("Windows Setup")
            lbl = QLabel(
                "Virtual camera: install UnityCapture (github.com/schellingb/UnityCapture),\n"
                "run Install.bat as Administrator.\n\n"
                "USB mode: add adb.exe (Android SDK Platform Tools) to PATH."
            )
            lbl.setObjectName("dim")
            lbl.setWordWrap(True)
            lay.addWidget(lbl)
        return gb

    # ── Status ────────────────────────────────────────────────────────────────
    def _build_status(self) -> QGroupBox:
        gb, lay = self._group("Status")
        self._status_lbl = QLabel("Idle – configure above and press Start")
        self._status_lbl.setObjectName("status_dim")
        self._status_lbl.setWordWrap(True)
        lay.addWidget(self._status_lbl)
        self._fps_lbl = QLabel("")
        self._fps_lbl.setObjectName("fps_lbl")
        lay.addWidget(self._fps_lbl)
        return gb

    # ── v4l2 helpers ──────────────────────────────────────────────────────────
    def _v4l_check(self):
        if v4l2_is_loaded():
            self._v4l_lbl.setObjectName("status_ok")
            self._v4l_lbl.setText(f"Loaded – {V4L2_PHONE_DEV} + {V4L2_OBS_DEV} ready")
        else:
            self._v4l_lbl.setObjectName("status_err")
            self._v4l_lbl.setText("Not loaded – click Load module")
        self._v4l_lbl.setStyleSheet("")

    def _v4l_load(self):
        self._v4l_lbl.setText("Loading…")
        self._v4l_lbl.setObjectName("status_dim")
        def _do():
            ok, msg = v4l2_load()
            def _apply():
                self._v4l_lbl.setText(("Loaded – " if ok else "Failed – ") + msg)
                self._v4l_lbl.setObjectName("status_ok" if ok else "status_err")
                self._v4l_lbl.setStyleSheet("")
            QTimer.singleShot(0, _apply)
        threading.Thread(target=_do, daemon=True).start()

    # ── Camera control handlers ────────────────────────────────────────────────
    def _on_lens_selected(self, cam: dict):
        if self._ctrl:
            self._ctrl.send(action="camera", id=cam["id"])
            self._iso_slider.set_range(cam.get("isoMin", 50), cam.get("isoMax", 6400))
            self._sht_slider.set_range(cam.get("shutterMinNs", 100_000),
                                       cam.get("shutterMaxNs", 1_000_000_000))
            self._sht_slider._spin.setRange(
                int(cam.get("shutterMinNs", 100_000)),
                int(cam.get("shutterMaxNs", 1_000_000_000)),
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

    def _on_iso_changed(self, val: float):
        if self._ctrl and self._manual_exp:
            self._ctrl.send(action="iso", value=int(val))

    def _on_shutter_changed(self, val: float):
        if self._ctrl and self._manual_exp:
            self._ctrl.send(action="shutter", value=int(val))

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

    def _on_flip(self):
        if self._worker:
            self._worker.flip_h = self._flip_h.isChecked()
            self._worker.flip_v = self._flip_v.isChecked()

    def _on_rotate(self):
        if self._worker:
            self._worker.rotation = ROTATIONS.get(self._rot_combo.currentText())

    def _on_resolution(self):
        if self._worker:
            res = RESOLUTIONS.get(self._res_combo.currentText())
            w, h = res if res else (None, None)
            self._worker.update_output(width=w, height=h)

    def _on_fps(self):
        if self._worker:
            self._worker.update_output(fps=self._fps_spin.value())

    # ── Start / Stop ──────────────────────────────────────────────────────────
    def _toggle(self):
        if self._worker: self._stop()
        else:            self._start()

    def _start(self):
        try:    port = int(self._port_field.text())
        except ValueError:
            QMessageBox.critical(self, "Bad port", "Port must be an integer."); return

        if IS_LINUX and not v4l2_is_loaded():
            r = QMessageBox.question(self, "v4l2loopback not ready",
                f"{V4L2_PHONE_DEV} not found. Load module now? (requires sudo)")
            if r != QMessageBox.StandardButton.Yes: return
            ok, msg = v4l2_load()
            if not ok: QMessageBox.critical(self, "Load failed", msg); return

        if self._rb_usb.isChecked():
            if not adb_available():
                QMessageBox.critical(self, "ADB missing",
                    "adb not found in PATH.\nInstall Android SDK Platform Tools."); return
            ok, msg = adb_forward(port)
            if not ok: QMessageBox.critical(self, "ADB forward failed", msg); return
            self._adb_port = port
            url = f"http://localhost:{port}/video"
        else:
            ip = self._ip_field.text().strip()
            if not ip or ip == "192.168.1.x":
                QMessageBox.critical(self, "No IP", "Enter the phone's IP address."); return
            url = f"http://{ip}:{port}/video"
            self._adb_port = None

        res = RESOLUTIONS.get(self._res_combo.currentText())
        w, h = res if res else (None, None)
        rotation = ROTATIONS.get(self._rot_combo.currentText())

        self._ctrl   = PhoneControlClient(url)
        self._worker = StreamWorker(
            url=url, width=w, height=h, fps=self._fps_spin.value(),
            flip_h=self._flip_h.isChecked(), flip_v=self._flip_v.isChecked(),
            rotation=rotation,
        )
        self._worker.status.connect(self._on_worker_status)
        self._worker.start()

        threading.Thread(target=self._fetch_state_async, args=(url,), daemon=True).start()

        self._start_btn.setText("Stop Streaming")
        self._start_btn.setProperty("streaming", True)
        self._start_btn.setStyle(self._start_btn.style())
        self._dot.setObjectName("status_ok")
        self._dot.setStyleSheet("")
        self._set_status("Connecting…", "dim")

    def _stop(self):
        if self._worker:
            self._worker.request_stop()
            self._worker = None
        if self._adb_port:
            adb_unforward(self._adb_port); self._adb_port = None
        self._ctrl = None
        self._lens_panel.clear()
        self._start_btn.setText("Start Streaming")
        self._start_btn.setProperty("streaming", False)
        self._start_btn.setStyle(self._start_btn.style())
        self._dot.setObjectName("status_dim")
        self._dot.setStyleSheet("")
        self._fps_lbl.setText("")
        self._set_status("Stopped.", "dim")

    def _fetch_state_async(self, url: str):
        time.sleep(1.5)
        for _ in range(6):
            if not self._ctrl: return
            state = self._ctrl.get_state()
            if state:
                QTimer.singleShot(0, lambda s=state: self._apply_state(s))
                return
            time.sleep(2)

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
                self._dot.setObjectName("status_dim")
                self._dot.setStyleSheet("")
        else:
            self._set_status(msg, "dim")

    def _set_status(self, msg: str, kind: str):
        obj = {"ok": "status_ok", "warn": "status_warn",
               "err": "status_err", "dim": "status_dim"}.get(kind, "status_dim")
        self._status_lbl.setObjectName(obj)
        self._status_lbl.setText(msg)
        self._status_lbl.setStyleSheet("")

    def closeEvent(self, event):
        self._stop()
        super().closeEvent(event)


# ── Entry point ───────────────────────────────────────────────────────────────
def main():
    app = QApplication(sys.argv)

    if IS_WINDOWS:
        app.setStyle("Fusion")
        pal = QPalette()
        for role, color in [
            (QPalette.ColorRole.Window,          QColor(_CAT["base"])),
            (QPalette.ColorRole.WindowText,      QColor(_CAT["text"])),
            (QPalette.ColorRole.Base,            QColor(_CAT["mantle"])),
            (QPalette.ColorRole.AlternateBase,   QColor(_CAT["surface0"])),
            (QPalette.ColorRole.Text,            QColor(_CAT["text"])),
            (QPalette.ColorRole.Button,          QColor(_CAT["surface0"])),
            (QPalette.ColorRole.ButtonText,      QColor(_CAT["text"])),
            (QPalette.ColorRole.Highlight,       QColor(_CAT["mauve"])),
            (QPalette.ColorRole.HighlightedText, QColor(_CAT["base"])),
            (QPalette.ColorRole.ToolTipBase,     QColor(_CAT["surface0"])),
            (QPalette.ColorRole.ToolTipText,     QColor(_CAT["text"])),
            (QPalette.ColorRole.PlaceholderText, QColor(_CAT["overlay0"])),
        ]:
            pal.setColor(role, color)
        app.setPalette(pal)
        app.setStyleSheet(CATPPUCCIN_QSS)
    else:
        # Use the system theme (KDE/GNOME provides it); only add semantic colors.
        app.setStyleSheet(LINUX_QSS)

    win = PhoneCamWindow()
    win.show()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
