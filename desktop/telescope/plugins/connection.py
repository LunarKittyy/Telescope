import json
import socket
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Optional

import qrcode
from PyQt6.QtCore import Qt, pyqtSignal, QObject, QSize
from PyQt6.QtGui import QColor, QIntValidator, QPainter, QBrush
from PyQt6.QtWidgets import (
    QButtonGroup, QDialog, QDialogButtonBox, QFormLayout, QFrame,
    QGroupBox, QHBoxLayout, QLabel, QLineEdit, QListWidget,
    QMessageBox, QPushButton, QRadioButton, QSizePolicy,
    QTextEdit, QVBoxLayout, QWidget,
)

from telescope.config import load_config, save_config
from telescope.platform import IS_LINUX, adb_available, adb_forward, adb_unforward
from telescope.platform.linux import (
    V4L2_OBS_DEV, V4L2_PHONE_DEV,
    v4l2_devices_ready, v4l2_load, v4l2_module_loaded,
)
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import NoScrollComboBox, create_vector_icon

DEFAULT_PORT = 8080
PAIRING_PORT = 8765


def _get_local_ips() -> list[str]:
    ips: set[str] = set()
    try:
        for info in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET):
            ip = info[4][0]
            if not ip.startswith("127."):
                ips.add(ip)
    except Exception:
        pass
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.connect(("8.8.8.8", 80))
            ips.add(s.getsockname()[0])
    except Exception:
        pass
    return sorted(ips, key=_rank_ip)


def _rank_ip(ip: str) -> int:
    parts = ip.split(".")
    if len(parts) == 4:
        try:
            a, b = int(parts[0]), int(parts[1])
            if a == 100 and 64 <= b <= 127:
                return 0  # Tailscale
        except ValueError:
            pass
    if ip.startswith(("192.168.", "10.", "172.")):
        return 1  # LAN
    return 2


def _best_ip(ips: list[str]) -> Optional[str]:
    if not ips:
        return None
    return min(ips, key=_rank_ip)


def _valid_ipv4(ip: str) -> bool:
    parts = ip.split(".")
    if len(parts) != 4:
        return False
    try:
        return all(0 <= int(p) <= 255 and str(int(p)) == p for p in parts)
    except ValueError:
        return False


class _DeviceDialog(QDialog):
    """Add or edit a device. In edit mode pass the existing device dict."""

    def __init__(self, parent=None, existing_names: list = None, device: dict = None):
        super().__init__(parent)
        self._existing = existing_names or []
        self._edit_name = device["name"] if device else None
        self.setWindowTitle("Edit Device" if device else "Add Device")
        self.setMinimumWidth(340)
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)

        form = QFormLayout()
        self._name_edit = QLineEdit(device["name"] if device else "")
        self._name_edit.setPlaceholderText("e.g. Phone1")
        self._ips_edit = QTextEdit()
        self._ips_edit.setPlaceholderText("One IP per line\ne.g. 192.168.1.100\n100.64.0.5")
        self._ips_edit.setFixedHeight(80)
        if device:
            self._ips_edit.setPlainText("\n".join(device.get("ips", [])))
        form.addRow("Name", self._name_edit)
        form.addRow("IP addresses", self._ips_edit)

        self._err_lbl = QLabel("")
        self._err_lbl.setObjectName("status_err")
        self._err_lbl.setWordWrap(True)

        buttons = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Ok | QDialogButtonBox.StandardButton.Cancel
        )
        buttons.accepted.connect(self._on_accept)
        buttons.rejected.connect(self.reject)
        _base = "padding: 4px 12px; border-radius: 4px;"
        buttons.button(QDialogButtonBox.StandardButton.Ok).setStyleSheet(
            f"background-color: #3a6b4f; {_base}"
        )
        buttons.button(QDialogButtonBox.StandardButton.Cancel).setStyleSheet(
            f"background-color: #6b3a3a; {_base}"
        )

        lay = QVBoxLayout(self)
        lay.addLayout(form)
        lay.addWidget(self._err_lbl)
        lay.addWidget(buttons)

    def _on_accept(self):
        name = self._name_edit.text().strip()
        ips = [l.strip() for l in self._ips_edit.toPlainText().splitlines() if l.strip()]
        if not name:
            self._err_lbl.setText("Name cannot be empty."); return
        if name != self._edit_name and name in self._existing:
            self._err_lbl.setText(f'"{name}" already exists.'); return
        if not ips:
            self._err_lbl.setText("Add at least one IP address."); return
        invalid = [ip for ip in ips if not _valid_ipv4(ip)]
        if invalid:
            self._err_lbl.setText(f"Invalid IP(s): {', '.join(invalid)}"); return
        seen: set[str] = set()
        dupes = [ip for ip in ips if ip in seen or seen.add(ip)]  # type: ignore[func-returns-value]
        if dupes:
            self._err_lbl.setText(f"Duplicate IP(s): {', '.join(dupes)}"); return
        self.accept()

    def result_device(self) -> dict:
        name = self._name_edit.text().strip()
        ips = [l.strip() for l in self._ips_edit.toPlainText().splitlines() if l.strip()]
        return {"name": name, "ips": ips}


class _DeviceManagerDialog(QDialog):
    """Device list management popup — add, edit, remove."""

    def __init__(self, parent, devices: list, on_change):
        super().__init__(parent)
        self.setWindowTitle("Devices")
        self.setMinimumWidth(360)
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self._devices = devices
        self._on_change = on_change
        self._active_dlg = None
        self._build_ui()

    def _build_ui(self):
        lay = QVBoxLayout(self)
        lay.setSpacing(12)

        gb = QGroupBox("Registered Devices")
        gb_lay = QVBoxLayout(gb)

        self._list = QListWidget()
        self._list.setAlternatingRowColors(False)
        self._list.currentRowChanged.connect(self._on_selection)
        gb_lay.addWidget(self._list)

        btn_row = QHBoxLayout()
        self._add_btn    = QPushButton("Add")
        self._edit_btn   = QPushButton("Edit")
        self._remove_btn = QPushButton("Remove")
        _base = "padding: 4px 12px; border-radius: 4px;"
        self._add_btn.setStyleSheet(f"background-color: #3a6b4f; {_base}")
        self._edit_btn.setStyleSheet(_base)
        self._remove_btn.setStyleSheet(f"background-color: #6b3a3a; {_base}")
        for btn in (self._add_btn, self._edit_btn, self._remove_btn):
            btn.setFixedWidth(90)
            btn.setFixedHeight(30)
        self._edit_btn.setEnabled(False)
        self._remove_btn.setEnabled(False)
        self._add_btn.clicked.connect(self._on_add)
        self._edit_btn.clicked.connect(self._on_edit)
        self._remove_btn.clicked.connect(self._on_remove)
        btn_row.addWidget(self._add_btn)
        btn_row.addWidget(self._edit_btn)
        btn_row.addWidget(self._remove_btn)
        btn_row.addStretch()
        gb_lay.addLayout(btn_row)

        lay.addWidget(gb)

        close_row = QHBoxLayout()
        close_row.addStretch()
        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.accept)
        close_row.addWidget(close_btn)
        lay.addLayout(close_row)

        self._refresh_list()

    def _refresh_list(self):
        self._list.clear()
        for d in self._devices:
            ips = d.get("ips", [])
            label = f"{d['name']}  -  {', '.join(ips[:2])}{'...' if len(ips) > 2 else ''}"
            self._list.addItem(label)

    def _on_selection(self, idx: int):
        ok = 0 <= idx < len(self._devices)
        self._edit_btn.setEnabled(ok)
        self._remove_btn.setEnabled(ok)

    def _open_device_dlg(self, dlg: "_DeviceDialog"):
        if self._active_dlg and self._active_dlg.isVisible():
            self._active_dlg.close()
        self._active_dlg = dlg
        dlg.setWindowModality(Qt.WindowModality.NonModal)
        dlg.show()
        dlg.raise_()
        dlg.activateWindow()

    def _on_add(self):
        existing = [d["name"] for d in self._devices]
        dlg = _DeviceDialog(self, existing_names=existing)
        dlg.accepted.connect(lambda: self._finish_add(dlg))
        self._open_device_dlg(dlg)

    def _finish_add(self, dlg: "_DeviceDialog"):
        self._devices.append(dlg.result_device())
        self._refresh_list()
        self._on_change(self._devices)

    def _on_edit(self):
        idx = self._list.currentRow()
        if idx < 0 or idx >= len(self._devices):
            return
        existing = [d["name"] for i, d in enumerate(self._devices) if i != idx]
        dlg = _DeviceDialog(self, existing_names=existing, device=self._devices[idx])
        dlg.accepted.connect(lambda: self._finish_edit(idx, dlg))
        self._open_device_dlg(dlg)

    def _finish_edit(self, idx: int, dlg: "_DeviceDialog"):
        self._devices[idx] = dlg.result_device()
        self._refresh_list()
        self._on_change(self._devices)

    def _on_remove(self):
        idx = self._list.currentRow()
        if idx < 0 or idx >= len(self._devices):
            return
        name = self._devices[idx]["name"]
        r = QMessageBox.question(
            self, "Remove device",
            f'Remove "{name}"? Its saved settings will be deleted.',
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            QMessageBox.StandardButton.No,
        )
        if r != QMessageBox.StandardButton.Yes:
            return
        self._devices.pop(idx)
        self._refresh_list()
        self._on_change(self._devices)


class _QRCodeWidget(QWidget):
    """Renders a QR code matrix using QPainter — no Pillow needed."""

    def __init__(self, data: str, parent=None):
        super().__init__(parent)
        qr = qrcode.QRCode(
            error_correction=qrcode.constants.ERROR_CORRECT_L,
            box_size=1,
            border=2,
        )
        qr.add_data(data)
        qr.make(fit=True)
        self._matrix = qr.modules
        n = len(self._matrix)
        size = n * 8
        self.setFixedSize(size, size)

    def paintEvent(self, event):
        n = len(self._matrix)
        cell = self.width() // n
        painter = QPainter(self)
        painter.fillRect(self.rect(), QColor("white"))
        painter.setPen(Qt.PenStyle.NoPen)
        painter.setBrush(QBrush(QColor("black")))
        for row in range(n):
            for col in range(n):
                if self._matrix[row][col]:
                    painter.drawRect(col * cell, row * cell, cell, cell)
        painter.end()


class _PairingSignals(QObject):
    paired = pyqtSignal(str, list)  # name, ips


class _PairingDialog(QDialog):
    """Shows a QR code and runs a pairing HTTP server while open."""

    def __init__(self, parent, on_paired):
        super().__init__(parent)
        self.setWindowTitle("Pair with Phone")
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self._on_paired = on_paired
        self._server: Optional[HTTPServer] = None
        self._signals = _PairingSignals()
        self._signals.paired.connect(self._on_paired_signal)
        self._build_ui()

    def _build_ui(self):
        lay = QVBoxLayout(self)
        lay.setSpacing(12)
        lay.setAlignment(Qt.AlignmentFlag.AlignHCenter)

        self._status_lbl = QLabel("Starting pairing server...")
        self._status_lbl.setObjectName("status_dim")
        self._status_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        lay.addWidget(self._status_lbl)

        self._qr_container = QVBoxLayout()
        self._qr_container.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        self._qr_container.setContentsMargins(0, 0, 0, 12)
        lay.addLayout(self._qr_container, 1)

        self._hint_lbl = QLabel("Open Telescope on your phone and tap the scan button in the top-right corner.")
        self._hint_lbl.setObjectName("dim")
        self._hint_lbl.setWordWrap(True)
        self._hint_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        lay.addWidget(self._hint_lbl)

        close_row = QHBoxLayout()
        close_row.addStretch()
        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.reject)
        close_row.addWidget(close_btn)
        lay.addLayout(close_row)

    def showEvent(self, event):
        super().showEvent(event)
        self._start_server()

    def closeEvent(self, event):
        self._stop_server()
        super().closeEvent(event)

    def _start_server(self):
        local_ips = _get_local_ips()
        if not local_ips:
            self._status_lbl.setObjectName("status_err")
            self._status_lbl.setText("No network interfaces found.")
            self._status_lbl.setStyleSheet("")
            return

        # Try to bind the fixed pairing port; fall back to random if in use
        port = PAIRING_PORT
        try:
            test = socket.socket()
            test.bind(("", port))
            test.close()
        except OSError:
            with socket.socket() as s:
                s.bind(("", 0))
                port = s.getsockname()[1]

        signals = self._signals

        class _Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"Telescope pairing server")

            def do_POST(self):
                if self.path != "/pair":
                    self.send_response(404); self.end_headers(); return
                length = int(self.headers.get("Content-Length", 0))
                body = self.rfile.read(length)
                try:
                    data = json.loads(body)
                    name = str(data.get("name", "Phone"))
                    ips = [str(x) for x in data.get("ips", [])]
                    signals.paired.emit(name, ips)
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(b"OK")
                except Exception:
                    self.send_response(400); self.end_headers()

            def log_message(self, *args):
                pass

        self._server = HTTPServer(("", port), _Handler)
        threading.Thread(target=self._server.serve_forever, daemon=True).start()

        payload = json.dumps({"port": port, "ips": local_ips})
        qr_widget = _QRCodeWidget(payload)
        while self._qr_container.count():
            item = self._qr_container.takeAt(0)
            if item.widget():
                item.widget().deleteLater()
        self._qr_container.addWidget(qr_widget)

        self._status_lbl.setObjectName("status_dim")
        self._status_lbl.setText("Scan with the Telescope app on your phone.")
        self._status_lbl.setStyleSheet("")

    def _stop_server(self):
        if self._server:
            threading.Thread(target=self._server.shutdown, daemon=True).start()
            self._server = None

    def _on_paired_signal(self, name: str, ips: list):
        # Replace QR with a big success message
        while self._qr_container.count():
            item = self._qr_container.takeAt(0)
            if item.widget():
                item.widget().deleteLater()
        success_lbl = QLabel(f'Paired!\n"{name}" added.')
        success_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        success_lbl.setStyleSheet("color: #4db87a; font-size: 16px; font-weight: bold;")
        self._qr_container.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._qr_container.addStretch()
        self._qr_container.addWidget(success_lbl)
        self._qr_container.addStretch()
        self._status_lbl.setText("")
        self._hint_lbl.setVisible(False)
        self._on_paired(name, ips)


class ConnectionPlugin(TelescopePlugin):
    name = "connection"

    def setup(self, host, bus):
        self._host             = host
        self._devices: list    = []
        self._selected_device: Optional[str] = None
        self._switching_device = False
        self._forwarded_port: Optional[int] = None
        self._device_dlg: Optional[QDialog] = None
        self._pairing_dlg: Optional[QDialog] = None

    def create_panel(self) -> QWidget:
        card = QFrame()
        card.setFrameShape(QFrame.Shape.StyledPanel)
        card.setObjectName("card")
        lay = QVBoxLayout(card)
        lay.setContentsMargins(14, 14, 14, 14)
        lay.setSpacing(10)

        hdr = QHBoxLayout()
        hdr.setContentsMargins(0, 0, 0, 4)
        hdr.setSpacing(8)
        icon_lbl = QLabel()
        icon_lbl.setPixmap(create_vector_icon("connection", "#518cc6").pixmap(18, 18))
        icon_lbl.setFixedSize(18, 18)
        hdr.addWidget(icon_lbl)
        title_lbl = QLabel("Connection")
        title_lbl.setObjectName("card_title")
        hdr.addWidget(title_lbl)
        hdr.addStretch()
        lay.addLayout(hdr)

        # ── Mode ──────────────────────────────────────────────────────────────
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
        self._conn_grp = QButtonGroup(card)
        self._conn_grp.addButton(self._rb_usb)
        self._conn_grp.addButton(self._rb_wifi)
        self._rb_usb.setChecked(True)
        self._conn_grp.buttonClicked.connect(lambda _: self._on_mode())
        mode_row.addWidget(self._rb_usb)
        mode_row.addWidget(self._rb_wifi)
        mode_row.addStretch()
        lay.addLayout(mode_row)

        # ── Device list (Wi-Fi only) ───────────────────────────────────────────
        self._device_row_w = QWidget()
        self._device_row_w.setObjectName("ip_row_container")
        device_v = QVBoxLayout(self._device_row_w)
        device_v.setContentsMargins(0, 0, 0, 0)
        device_v.setSpacing(4)

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

        _icon_color = "#c8d0da"
        _icon_size  = QSize(18, 18)
        self._gear_btn = QPushButton()
        self._gear_btn.setFixedSize(28, 28)
        self._gear_btn.setIcon(create_vector_icon("gear", _icon_color))
        self._gear_btn.setIconSize(_icon_size)
        self._gear_btn.setToolTip("Manage devices")
        self._gear_btn.clicked.connect(self._on_manage_devices)

        self._qr_btn = QPushButton()
        self._qr_btn.setFixedSize(28, 28)
        self._qr_btn.setIcon(create_vector_icon("qr", _icon_color))
        self._qr_btn.setIconSize(_icon_size)
        self._qr_btn.setToolTip("Pair via QR code")
        self._qr_btn.clicked.connect(self._on_pair_qr)

        combo_row.addWidget(self._gear_btn)
        combo_row.addWidget(self._qr_btn)
        device_v.addLayout(combo_row)

        ip_row = QHBoxLayout()
        ip_row.setContentsMargins(0, 0, 0, 0)
        ip_row.setSpacing(0)
        ip_row.addSpacing(116)  # matches: label(110) + spacing(6) in combo_row
        self._ip_combo = NoScrollComboBox()
        self._ip_combo.setFixedWidth(155)
        self._ip_combo.currentTextChanged.connect(self._on_ip_changed)
        ip_row.addWidget(self._ip_combo)
        ip_row.addStretch()
        device_v.addLayout(ip_row)

        lay.addWidget(self._device_row_w)
        self._device_row_w.setVisible(False)

        # ── Port ──────────────────────────────────────────────────────────────
        port_row = QHBoxLayout()
        port_row.setContentsMargins(0, 0, 0, 0)
        port_row.setSpacing(6)
        port_lbl = QLabel("Port")
        port_lbl.setObjectName("dim")
        port_lbl.setFixedWidth(110)
        port_lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        port_row.addWidget(port_lbl)
        self._port_field = QLineEdit(str(DEFAULT_PORT))
        self._port_field.setValidator(QIntValidator(1, 65535))
        self._port_field.setMaximumWidth(90)
        self._port_field.editingFinished.connect(self._host._schedule_save)
        port_row.addWidget(self._port_field)
        port_row.addStretch()
        lay.addLayout(port_row)

        return card

    # ── Stream lifecycle ──────────────────────────────────────────────────────

    def get_stream_info(self) -> tuple:
        try:
            port = int(self._port_field.text())
        except ValueError:
            QMessageBox.critical(self._host, "Bad port", "Port must be a number.")
            return None, False

        if IS_LINUX and not v4l2_devices_ready():
            if v4l2_module_loaded():
                QMessageBox.warning(
                    self._host, "v4l2loopback conflict",
                    f"v4l2loopback is already loaded but {V4L2_PHONE_DEV} is not available.\n\n"
                    "Another virtual camera setup is using the module. Telescope won't touch it.\n\n"
                    "To use Telescope's setup instead, first run:\n"
                    "    sudo modprobe -r v4l2loopback\n\n"
                    "Then click Start again."
                )
                return None, False
            r = QMessageBox.question(
                self._host, "Virtual camera not ready",
                f"The virtual camera module (v4l2loopback) is not loaded.\n\n"
                f"Telescope will load it now. This needs admin access and may ask for your password.\n\n"
                f"Devices: {V4L2_PHONE_DEV} (phone), {V4L2_OBS_DEV} (OBS)",
                QMessageBox.StandardButton.Ok | QMessageBox.StandardButton.Cancel,
                QMessageBox.StandardButton.Ok,
            )
            if r != QMessageBox.StandardButton.Ok:
                return None, False
            ok, msg = v4l2_load()
            if not ok:
                QMessageBox.critical(self._host, "Load failed", msg)
                return None, False

        if self._rb_usb.isChecked():
            if not adb_available():
                QMessageBox.critical(
                    self._host, "ADB not found",
                    "ADB is needed for USB mode but wasn't found.\n\n"
                    "Click the Download ADB button in the Windows Setup section "
                    "and try again, or switch to Wi-Fi mode."
                )
                return None, False
            ok, msg = adb_forward(port)
            if not ok:
                QMessageBox.critical(self._host, "ADB forward failed", msg)
                return None, False
            self._forwarded_port = port
            return f"http://localhost:{port}/video", True
        else:
            ip = self._current_device_ip()
            if not ip:
                QMessageBox.critical(self._host, "No device", "Add a device in Wi-Fi mode first.")
                return None, False
            self._forwarded_port = None
            return f"http://{ip}:{port}/video", True

    def on_stream_stop(self):
        if self._forwarded_port is not None:
            adb_unforward(self._forwarded_port)
            self._forwarded_port = None

    # ── Mode / device handlers ────────────────────────────────────────────────

    def _on_mode(self):
        self._device_row_w.setVisible(self._rb_wifi.isChecked())
        self._host._schedule_save()
        if self._host._worker is not None:
            self._host._stop()
            self._host._start()

    def _current_device_name(self) -> Optional[str]:
        idx = self._device_combo.currentIndex()
        if idx < 0 or idx >= len(self._devices):
            return None
        return self._devices[idx]["name"]

    def _current_device_ip(self) -> Optional[str]:
        ip = self._ip_combo.currentText().strip()
        return ip if ip else None

    def _refresh_device_combo(self, select_name: Optional[str] = None):
        self._switching_device = True
        self._device_combo.blockSignals(True)
        self._device_combo.clear()
        for d in self._devices:
            self._device_combo.addItem(d["name"])
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
        self._update_ip_combo()

    def _update_ip_combo(self):
        idx = self._device_combo.currentIndex()
        self._ip_combo.blockSignals(True)
        self._ip_combo.clear()
        if 0 <= idx < len(self._devices):
            ips = list(dict.fromkeys(self._devices[idx].get("ips", [])))  # deduplicate, preserve order
            for ip in sorted(ips, key=_rank_ip):
                self._ip_combo.addItem(ip)
        self._ip_combo.blockSignals(False)

    def _on_device_changed(self, idx: int):
        if self._switching_device:
            return
        name = self._devices[idx]["name"] if 0 <= idx < len(self._devices) else None
        if name and name != self._selected_device:
            prev = self._selected_device
            self._selected_device = name
            self._update_ip_combo()
            self._host._switch_device(prev, name)

    def _on_ip_changed(self, ip: str):
        pass  # used directly via _current_device_ip()

    def _on_manage_devices(self):
        if self._device_dlg is None or not self._device_dlg.isVisible():
            self._device_dlg = _DeviceManagerDialog(self._host, self._devices, self._on_devices_changed)
            self._device_dlg.setAttribute(Qt.WidgetAttribute.WA_DeleteOnClose, False)
            self._device_dlg.setWindowModality(Qt.WindowModality.NonModal)
        self._device_dlg.show()
        self._device_dlg.raise_()
        self._device_dlg.activateWindow()

    def _on_pair_qr(self):
        if self._pairing_dlg is None or not self._pairing_dlg.isVisible():
            self._pairing_dlg = _PairingDialog(self._host, self._on_device_paired)
            self._pairing_dlg.setAttribute(Qt.WidgetAttribute.WA_DeleteOnClose, False)
            self._pairing_dlg.setWindowModality(Qt.WindowModality.NonModal)
        self._pairing_dlg.show()
        self._pairing_dlg.raise_()
        self._pairing_dlg.activateWindow()

    def _on_devices_changed(self, devices: list):
        self._devices = devices
        prev = self._selected_device
        self._refresh_device_combo(select_name=prev)
        if not any(d["name"] == prev for d in self._devices):
            self._selected_device = self._devices[0]["name"] if self._devices else None
        self._host._save_config()

    def _on_device_paired(self, name: str, ips: list):
        existing_names = [d["name"] for d in self._devices]
        if name in existing_names:
            for d in self._devices:
                if d["name"] == name:
                    d["ips"] = ips
                    break
        else:
            self._devices.append({"name": name, "ips": ips})
        self._refresh_device_combo(select_name=name)
        self._selected_device = name
        self._host._save_config()

    @property
    def selected_device(self) -> Optional[str]:
        return self._selected_device

    # ── Config ────────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {
            "mode":         "wifi" if self._rb_wifi.isChecked() else "usb",
            "port":         self._port_field.text(),
            "devices_list": self._devices,
        }

    def set_config(self, cfg: dict):
        if cfg.get("mode") == "wifi":
            self._rb_wifi.setChecked(True)
            self._rb_usb.setChecked(False)
            self._on_mode()
        else:
            self._rb_usb.setChecked(True)
            self._rb_wifi.setChecked(False)
            self._device_row_w.setVisible(False)
        if port := cfg.get("port"):
            self._port_field.setText(str(port))
        raw = cfg.get("devices_list", [])
        # Migrate old format {"name": str, "ip": str} → {"name": str, "ips": [str]}
        self._devices = []
        for d in raw:
            if "ip" in d and "ips" not in d:
                self._devices.append({"name": d["name"], "ips": [d["ip"]]})
            else:
                self._devices.append(d)
        self._switching_device = True
        self._device_combo.blockSignals(True)
        self._device_combo.clear()
        for d in self._devices:
            self._device_combo.addItem(d["name"])
        self._device_combo.blockSignals(False)
        self._switching_device = False

    def select_device(self, name: Optional[str]):
        if not name and self._devices:
            name = self._devices[0]["name"]
        self._selected_device = name
        self._refresh_device_combo(select_name=name)
