import shutil
import socket
import subprocess
import threading
import time
from typing import Optional

from PyQt6.QtCore import Qt, QTimer, pyqtSignal
from PyQt6.QtGui import (
    QAction, QBrush, QColor, QIcon, QPainter, QPen, QPixmap,
)
from PyQt6.QtWidgets import (
    QApplication, QFrame, QHBoxLayout, QLabel,
    QMainWindow, QMenu, QScrollArea,
    QSizePolicy, QSystemTrayIcon, QVBoxLayout, QWidget,
)

from phonecam.config import DEVICE_LOCAL_PLUGINS, load_config, save_config
from phonecam.phone_client import PhoneControlClient
from phonecam.platform import IS_LINUX, IS_WINDOWS
from phonecam.plugin import EventBus, PhoneCamPlugin
from phonecam.stream import StreamWorker
from phonecam.widgets.common import create_vector_icon

# ── Theme / QSS ───────────────────────────────────────────────────────────────

STATUS_COLORS = {
    "status_ok":   "#66bb6a",
    "status_warn": "#ffa726",
    "status_err":  "#ef5350",
    "status_dim":  "#78909c",
}

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

# ── Single-instance enforcement ───────────────────────────────────────────────
_INSTANCE_PORT = 47823


def acquire_single_instance() -> Optional[socket.socket]:
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 0)
    try:
        srv.bind(("127.0.0.1", _INSTANCE_PORT))
        srv.listen(1)
        return srv
    except OSError:
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


# ── Main window ───────────────────────────────────────────────────────────────
class PhoneCamWindow(QMainWindow):
    _sig_state = pyqtSignal(dict)
    _sig_raise = pyqtSignal()
    _sig_canvas_reload_done = pyqtSignal(bool, str, bool)  # ok, msg, restart_stream

    def __init__(self):
        super().__init__()
        self.setWindowTitle("PhoneCam")
        self.setMinimumSize(520, 480)
        self.resize(540, 900)

        self._bus     = EventBus()
        self._plugins: list[PhoneCamPlugin] = []

        self._worker: Optional[StreamWorker] = None
        self._ctrl:   Optional[PhoneControlClient] = None

        self._save_timer = QTimer(self)
        self._save_timer.setSingleShot(True)
        self._save_timer.timeout.connect(self._save_config)

        self._tray: Optional[QSystemTrayIcon] = None

        self._build_ui()
        self._setup_tray()

        self._sig_state.connect(self._apply_state)
        self._sig_raise.connect(self._tray_show)
        self._sig_canvas_reload_done.connect(self._on_canvas_reload_done)

    def register_plugin(self, plugin: PhoneCamPlugin):
        plugin.setup(self, self._bus)
        panel = plugin.create_panel()
        if panel:
            # Insert before the trailing stretch (always last item)
            stretch_idx = self._scroll_content_layout.count() - 1
            self._scroll_content_layout.insertWidget(stretch_idx, panel)
        self._plugins.append(plugin)

    def apply_saved_config(self):
        """Restore persisted config into all registered plugins. Call after all plugins registered."""
        self._apply_config(load_config())

    # ── UI construction ───────────────────────────────────────────────────────

    def _build_ui(self):
        from PyQt6.QtWidgets import QPushButton
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
        self._scroll_content_layout = QVBoxLayout(content)
        self._scroll_content_layout.setContentsMargins(16, 16, 16, 16)
        self._scroll_content_layout.setSpacing(14)
        self._scroll_content_layout.addStretch()
        scroll.setWidget(content)
        root_lay.addWidget(scroll, 1)

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

        btn_lay.addWidget(self._status_lbl)
        btn_lay.addWidget(self._fps_lbl)
        btn_lay.addWidget(self._start_btn)
        root_lay.addWidget(btn_frame)

    # ── Config persistence ────────────────────────────────────────────────────

    def _schedule_save(self):
        self._save_timer.start(500)

    def _save_config(self):
        cfg = load_config()
        # Global plugin configs (connection, setup, etc.)
        global_pcfg = cfg.setdefault("plugin_configs", {})
        conn = next((p for p in self._plugins if p.name == "connection"), None)
        selected = conn.selected_device if conn else None
        cfg["selected_device"] = selected
        for p in self._plugins:
            if p.name and p.name not in DEVICE_LOCAL_PLUGINS:
                global_pcfg[p.name] = p.get_config()
        # Per-device plugin configs
        if selected:
            dev = cfg.setdefault("devices", {}).setdefault(selected, {})
            dev_pcfg = dev.setdefault("plugin_configs", {})
            for p in self._plugins:
                if p.name and p.name in DEVICE_LOCAL_PLUGINS:
                    dev_pcfg[p.name] = p.get_config()
        save_config(cfg)

    def _switch_device(self, prev_name, new_name: str):
        """Save current device's per-device configs then restore the new device's."""
        cfg = load_config()
        if prev_name:
            prev_pcfg = cfg.setdefault("devices", {}).setdefault(prev_name, {}).setdefault("plugin_configs", {})
            for p in self._plugins:
                if p.name and p.name in DEVICE_LOCAL_PLUGINS:
                    prev_pcfg[p.name] = p.get_config()
        cfg["selected_device"] = new_name
        save_config(cfg)
        new_pcfg = cfg.get("devices", {}).get(new_name, {}).get("plugin_configs", {})
        for p in self._plugins:
            if p.name and p.name in DEVICE_LOCAL_PLUGINS and p.name in new_pcfg:
                p.set_config(new_pcfg[p.name])

    def _apply_config(self, cfg: dict):
        if not cfg:
            return
        # config.py's load_config() already ran migration; cfg is always v2 here
        selected    = cfg.get("selected_device")
        global_pcfg = cfg.get("plugin_configs", {})
        device_pcfg = cfg.get("devices", {}).get(selected, {}).get("plugin_configs", {}) if selected else {}

        conn = next((p for p in self._plugins if p.name == "connection"), None)
        for p in self._plugins:
            if not p.name:
                continue
            if p.name in DEVICE_LOCAL_PLUGINS:
                if p.name in device_pcfg:
                    p.set_config(device_pcfg[p.name])
            else:
                if p.name in global_pcfg:
                    p.set_config(global_pcfg[p.name])
        # Restore device selection after connection plugin has its devices_list
        if conn:
            conn.select_device(selected)

    # ── Start / Stop ──────────────────────────────────────────────────────────

    def _toggle(self):
        if self._worker: self._stop()
        else:            self._start()

    def _start(self):
        conn = next((p for p in self._plugins if p.name == "connection"), None)
        if not conn:
            return
        url, ok = conn.get_stream_info()
        if not ok:
            return

        so = next((p for p in self._plugins if p.name == "stream_output"), None)
        w, h, fps = so.get_stream_params() if so else (None, None, 30)

        setup = next((p for p in self._plugins if p.name == "setup"), None)
        canvas_w, canvas_h = setup.get_canvas_dims() if setup else (None, None)

        pipeline = [p.process_frame for p in self._plugins]

        self._ctrl = PhoneControlClient(url)
        self._worker = StreamWorker(
            url=url, width=w, height=h, fps=fps,
            frame_pipeline=pipeline,
            canvas_width=canvas_w, canvas_height=canvas_h,
        )
        self._worker.status.connect(self._on_worker_status)
        self._worker.start()

        self._bus.stream_started.emit(url)
        for p in self._plugins:
            p.on_stream_start(url, self._ctrl)

        threading.Thread(target=self._fetch_state_async, args=(url,), daemon=True).start()

        self._start_btn.setText("Stop Streaming")
        self._start_btn.setProperty("streaming", True)
        self._start_btn.setStyle(self._start_btn.style())
        self._set_status("Connecting...", "dim")

    def _stop(self):
        if self._worker:
            self._worker.status.disconnect(self._on_worker_status)
            self._worker.request_stop()
            self._worker.wait()
            self._worker = None
        self._ctrl = None
        self._start_btn.setText("Start Streaming")
        self._start_btn.setProperty("streaming", False)
        self._start_btn.setStyle(self._start_btn.style())
        self._fps_lbl.setText("")
        self._set_status("Stopped.", "dim")

        self._bus.stream_stopped.emit()
        for p in self._plugins:
            p.on_stream_stop()

    def restart_vcam_canvas(self, w, h, on_done=None):
        """Stop stream, optionally reload the vcam driver, restart stream."""
        self._vcam_reload_callback = on_done
        was_streaming = self._worker is not None
        old_worker = self._worker  # capture before _stop() clears it
        self._stop()

        if IS_LINUX:
            self._set_status("Reloading v4l2loopback…", "dim")

            def worker():
                if old_worker:
                    old_worker.wait(5000)
                from phonecam.platform.linux import v4l2_reload
                ok, msg = v4l2_reload()
                self._sig_canvas_reload_done.emit(ok, msg, was_streaming)

            threading.Thread(target=worker, daemon=True).start()
        else:
            self._set_status("Restarting stream…", "dim")

            def worker():
                if old_worker:
                    old_worker.wait(5000)
                self._sig_canvas_reload_done.emit(True, "canvas updated", was_streaming)

            threading.Thread(target=worker, daemon=True).start()

    def _on_canvas_reload_done(self, ok: bool, msg: str, restart_stream: bool):
        if ok:
            self._set_status(f"Loopback reloaded: {msg}", "ok")
            if restart_stream:
                self._start()
        else:
            self._set_status(f"Reload failed: {msg}", "err")
        cb = getattr(self, "_vcam_reload_callback", None)
        if cb:
            cb(ok, msg)
            self._vcam_reload_callback = None

    def _fetch_state_async(self, url: str):
        time.sleep(1.5)
        for _ in range(3):
            if not self._ctrl: return
            state = self._ctrl.get_state()
            if state:
                self._sig_state.emit(state)
                return
            time.sleep(2)
        if self._ctrl:
            self._sig_state.emit({})

    def _apply_state(self, state: dict):
        self._bus.phone_state_updated.emit(state)
        for p in self._plugins:
            p.on_phone_state(state)

    # ── Tray ──────────────────────────────────────────────────────────────────

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

    def send_notification(self, title: str, body: str):
        if IS_LINUX and shutil.which("notify-send"):
            subprocess.Popen(
                ["notify-send", "-a", "PhoneCam", "-u", "critical", title, body],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        elif self._tray:
            self._tray.showMessage(title, body, QSystemTrayIcon.MessageIcon.Warning, 0)

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
                self.send_notification(
                    "PhoneCam is still running",
                    "Streaming continues in the background. Right-click the tray icon to quit.",
                )
        else:
            self._stop()
            event.accept()
