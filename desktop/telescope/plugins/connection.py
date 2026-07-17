import logging
from typing import Optional

import qrcode
from PyQt6.QtCore import Qt, pyqtSignal, QObject, QSize
from PyQt6.QtGui import QColor, QIntValidator, QPainter, QBrush
from PyQt6.QtWidgets import (
    QButtonGroup, QDialog, QDialogButtonBox, QFormLayout, QFrame,
    QGroupBox, QHBoxLayout, QInputDialog, QLabel, QLineEdit, QListWidget,
    QMessageBox, QPushButton, QRadioButton, QSizePolicy,
    QTextEdit, QVBoxLayout, QWidget,
)

from telescope import ip_utils
from telescope.config import load_config, save_config
from telescope.models import DeviceProfile
from telescope.pairing import PairingServer
from telescope.platform import IS_LINUX, adb_available, adb_devices, adb_forward, adb_unforward
from telescope.platform.linux import (
    V4L2_OBS_DEV, V4L2_PHONE_DEV,
    v4l2_devices_ready, v4l2_load, v4l2_module_loaded,
)
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import NoScrollComboBox, create_vector_icon

logger = logging.getLogger(__name__)

DEFAULT_PORT = 8080

# Pseudo-device key used to give USB-only sessions their own persisted
# device-local plugin profile (camera settings, transforms, etc.), same as
# named Wi-Fi devices get. Never shown in the device list/management UI.
USB_PROFILE_KEY = "__usb__"


# Re-exported under their historical private names: this module (panel/
# dialog UI + QR-pairing HTTP server) is not where these pure functions
# conceptually belong, but existing code/tests reference them here, so
# telescope/ip_utils.py is the actual implementation and this is a thin
# compatibility alias.
_get_local_ips = ip_utils.get_local_ips
_rank_ip = ip_utils.rank_ip
_best_ip = ip_utils.best_ip
_extract_ip = ip_utils.extract_ip
_valid_ipv4 = ip_utils.valid_ipv4


class _DeviceDialog(QDialog):
    """Add or edit a device. In edit mode pass the existing device dict."""

    def __init__(self, parent=None, existing_names: list = None, device: dict = None):
        super().__init__(parent)
        self._existing = existing_names or []
        self._edit_name = device["name"] if device else None
        # Kept so result_device() can preserve fields this dialog doesn't
        # edit (e.g. a pairing token) instead of dropping them on save.
        self._original_device = device
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
            f"QPushButton {{ background-color: #3a6b4f; {_base} }} QPushButton:hover {{ background-color: #4a8b65; }}"
        )
        buttons.button(QDialogButtonBox.StandardButton.Cancel).setStyleSheet(
            f"QPushButton {{ background-color: #6b3a3a; {_base} }} QPushButton:hover {{ background-color: #8b4a4a; }}"
        )

        lay = QVBoxLayout(self)
        lay.addLayout(form)
        lay.addWidget(self._err_lbl)
        lay.addWidget(buttons)

    def _parse_ips(self) -> list[str]:
        return [_extract_ip(l) for l in self._ips_edit.toPlainText().splitlines()
                if l.strip()]

    def _on_accept(self):
        name = self._name_edit.text().strip()
        ips = self._parse_ips()
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
        device = dict(self._original_device) if self._original_device else {}
        device["name"] = self._name_edit.text().strip()
        device["ips"] = self._parse_ips()
        return device


class _DeviceManagerDialog(QDialog):
    """Device list management popup — add, edit, remove."""

    def __init__(self, parent, devices: list, on_add, on_edit, on_remove):
        super().__init__(parent)
        self.setWindowTitle("Devices")
        self.setMinimumWidth(360)
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self._devices = devices
        self._on_add_cb    = on_add
        self._on_edit_cb   = on_edit
        self._on_remove_cb = on_remove
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
        self._add_btn.setStyleSheet(f"QPushButton {{ background-color: #3a6b4f; {_base} }} QPushButton:hover {{ background-color: #4a8b65; }}")
        self._edit_btn.setStyleSheet(_base)
        self._remove_btn.setStyleSheet(f"QPushButton {{ background-color: #6b3a3a; {_base} }} QPushButton:hover {{ background-color: #8b4a4a; }}")
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
        device = dlg.result_device()
        self._devices.append(device)
        self._refresh_list()
        self._on_add_cb(device)

    def _on_edit(self):
        idx = self._list.currentRow()
        if idx < 0 or idx >= len(self._devices):
            return
        existing = [d["name"] for i, d in enumerate(self._devices) if i != idx]
        dlg = _DeviceDialog(self, existing_names=existing, device=self._devices[idx])
        dlg.accepted.connect(lambda: self._finish_edit(idx, dlg))
        self._open_device_dlg(dlg)

    def _finish_edit(self, idx: int, dlg: "_DeviceDialog"):
        old_name = self._devices[idx]["name"]
        new_device = dlg.result_device()
        self._devices[idx] = new_device
        self._refresh_list()
        self._on_edit_cb(old_name, new_device)

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
        self._on_remove_cb(name)


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
    paired = pyqtSignal(str, list, str)  # name, ips, token


class _PairingDialog(QDialog):
    """Shows a QR code and runs a pairing HTTP server while open."""

    def __init__(self, parent, on_paired):
        super().__init__(parent)
        self.setWindowTitle("Pair with Phone")
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self._on_paired = on_paired
        self._pairing_server: Optional[PairingServer] = None
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
        if self._pairing_server is not None:
            return  # already running - showEvent() can fire more than once

        signals = self._signals
        server = PairingServer(on_paired=lambda r: signals.paired.emit(r.name, r.ips, r.token))
        offer = server.start()
        if offer is None:
            self._status_lbl.setObjectName("status_err")
            self._status_lbl.setText("No network interfaces found.")
            self._status_lbl.setStyleSheet("")
            return
        self._pairing_server = server

        qr_widget = _QRCodeWidget(offer.payload)
        while self._qr_container.count():
            item = self._qr_container.takeAt(0)
            if item.widget():
                item.widget().deleteLater()
        self._qr_container.addWidget(qr_widget)

        self._status_lbl.setObjectName("status_dim")
        self._status_lbl.setText("Scan with the Telescope app on your phone.")
        self._status_lbl.setStyleSheet("")

    def _stop_server(self):
        if self._pairing_server is None:
            return
        self._pairing_server.stop()
        self._pairing_server = None

    def _on_paired_signal(self, name: str, ips: list, token: str):
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
        self._on_paired(name, ips, token)


class ConnectionPlugin(TelescopePlugin):
    name = "connection"

    def setup(self, host, bus):
        self._host             = host
        self._devices: list    = []
        self._selected_device: Optional[str] = None
        # The profile key (device name, or USB_PROFILE_KEY) that's actually
        # been applied/reconnected via the host - kept separate from
        # _selected_device so mode toggles and device switches only trigger a
        # save+reset+reconnect when the *effective* profile actually changes.
        self._active_key: Optional[str] = None
        self._switching_device = False
        self._forwarded_port: Optional[int] = None
        self._adb_serial: Optional[str] = None
        self._device_dlg: Optional[QDialog] = None
        self._pairing_dlg: Optional[QDialog] = None
        self._last_port: str = str(DEFAULT_PORT)

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
        self._port_field.editingFinished.connect(self._on_port_changed)
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
            return None, None, False

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
                return None, None, False
            r = QMessageBox.question(
                self._host, "Virtual camera not ready",
                f"The virtual camera module (v4l2loopback) is not loaded.\n\n"
                f"Telescope will load it now. This needs admin access and may ask for your password.\n\n"
                f"Devices: {V4L2_PHONE_DEV} (phone), {V4L2_OBS_DEV} (OBS)",
                QMessageBox.StandardButton.Ok | QMessageBox.StandardButton.Cancel,
                QMessageBox.StandardButton.Ok,
            )
            if r != QMessageBox.StandardButton.Ok:
                return None, None, False
            ok, msg = v4l2_load()
            if not ok:
                QMessageBox.critical(self._host, "Load failed", msg)
                return None, None, False

        token = self._current_device_token()
        if token is None:
            QMessageBox.critical(
                self._host, "Not paired",
                "This device hasn't been paired yet.\n\n"
                "Click the QR button next to the device selector and scan the "
                "code with the Telescope app on your phone."
            )
            return None, None, False

        if self._rb_usb.isChecked():
            if not adb_available():
                QMessageBox.critical(
                    self._host, "ADB not found",
                    "ADB is needed for USB mode but wasn't found.\n\n"
                    "Click the Download ADB button in the Windows Setup section "
                    "and try again, or switch to Wi-Fi mode."
                )
                return None, None, False
            serial = self._resolve_adb_serial()
            if serial is None:
                return None, None, False
            ok, msg = adb_forward(port, serial=serial)
            if not ok:
                QMessageBox.critical(self._host, "ADB forward failed", msg)
                return None, None, False
            self._forwarded_port = port
            self._adb_serial = serial
            return f"http://localhost:{port}/v1/video", token, True
        else:
            ip = self._current_device_ip()
            if not ip:
                QMessageBox.critical(self._host, "No device", "Add a device in Wi-Fi mode first.")
                return None, None, False
            self._forwarded_port = None
            return f"http://{ip}:{port}/v1/video", token, True

    def _current_device_token(self) -> Optional[str]:
        """The stored pairing token for the profile currently in play (the
        selected Wi-Fi device, or whichever device was last selected before
        switching to USB mode - USB streaming still authenticates with a
        paired device's token, it just reaches it via adb forward)."""
        name = self._selected_device
        if not name:
            return None
        for d in self._devices:
            if d["name"] == name:
                return d.get("token")
        return None

    def on_stream_stop(self):
        if self._forwarded_port is not None:
            adb_unforward(self._forwarded_port, serial=self._adb_serial)
            self._forwarded_port = None
            self._adb_serial = None

    def _resolve_adb_serial(self) -> Optional[str]:
        """Return the adb serial to target, prompting if more than one device is attached."""
        serials = adb_devices()
        if not serials:
            QMessageBox.critical(
                self._host, "No ADB device",
                "No authorized ADB device or emulator was found.\n\n"
                "Make sure your phone is plugged in, USB debugging is enabled, "
                "and you've accepted the debugging prompt on the phone."
            )
            return None
        if len(serials) == 1:
            return serials[0]
        serial, ok = QInputDialog.getItem(
            self._host, "Select device",
            "Multiple ADB devices/emulators are connected.\nChoose which one to use:",
            serials, 0, False,
        )
        return serial if ok else None

    # ── Mode / device handlers ────────────────────────────────────────────────

    @property
    def _profile_key(self) -> Optional[str]:
        """The device-local-plugin profile key for the current mode: the
        selected Wi-Fi device's name, or a fixed pseudo-key for USB so USB
        sessions get their own persisted camera/transform/monitoring
        settings instead of silently not saving them."""
        if self._rb_usb.isChecked():
            return USB_PROFILE_KEY
        return self._selected_device

    def _activate_profile(self, new_key: Optional[str]):
        """Switch the effective device-local profile via the host, but only
        if it actually changed - avoids a spurious save/reset/reconnect
        cycle from signals fired while combo boxes are being repopulated."""
        if new_key == self._active_key:
            return
        prev_key = self._active_key
        self._active_key = new_key
        self._host._switch_device(prev_key, new_key)

    def _on_mode(self):
        self._device_row_w.setVisible(self._rb_wifi.isChecked())
        self._host._schedule_save()
        self._activate_profile(self._profile_key)

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
        active_ip = None
        if 0 <= idx < len(self._devices):
            device = self._devices[idx]
            ips = list(dict.fromkeys(device.get("ips", [])))  # deduplicate, preserve order
            for ip in sorted(ips, key=_rank_ip):
                self._ip_combo.addItem(ip)
            cfg = load_config()
            saved_ip = cfg.get("devices", {}).get(device["name"], {}).get("active_ip")
            active_ip = saved_ip if saved_ip in ips else _best_ip(ips)
        if active_ip:
            found = self._ip_combo.findText(active_ip)
            if found >= 0:
                self._ip_combo.setCurrentIndex(found)
        self._ip_combo.blockSignals(False)

    def _on_device_changed(self, idx: int):
        if self._switching_device:
            return
        name = self._devices[idx]["name"] if 0 <= idx < len(self._devices) else None
        if name and name != self._selected_device:
            self._selected_device = name
            self._update_ip_combo()
            self._activate_profile(self._profile_key)

    def _on_ip_changed(self, ip: str):
        if self._switching_device or not ip:
            return
        name = self._current_device_name()
        if not name:
            return
        cfg = load_config()
        dev = cfg.setdefault("devices", {}).setdefault(name, {})
        if dev.get("active_ip") == ip:
            return
        dev["active_ip"] = ip
        save_config(cfg)
        self._host.reconnect_stream()

    def _on_port_changed(self):
        self._host._schedule_save()
        new_port = self._port_field.text()
        if new_port != self._last_port:
            self._last_port = new_port
            self._host.reconnect_stream()

    def _on_manage_devices(self):
        if self._device_dlg is None or not self._device_dlg.isVisible():
            self._device_dlg = _DeviceManagerDialog(
                self._host, self._devices,
                on_add=self._on_device_added,
                on_edit=self._on_device_edited,
                on_remove=self._on_device_removed,
            )
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

    def _on_device_added(self, device: dict):
        # _DeviceManagerDialog mutates the shared self._devices list directly.
        self._refresh_device_combo(select_name=self._selected_device)
        # _refresh_device_combo() blocks signals, so if this is the first device
        # ever added, the combo now defaults to it but _selected_device (still
        # None from before) never hears about it - sync from the combo itself.
        self._selected_device = self._current_device_name()
        self._host._save_config()
        self._activate_profile(self._profile_key)

    def _on_device_edited(self, old_name: str, new_device: dict):
        new_name = new_device["name"]
        if new_name != old_name:
            cfg = load_config()
            devices_cfg = cfg.setdefault("devices", {})
            if old_name in devices_cfg:
                devices_cfg[new_name] = devices_cfg.pop(old_name)
            if cfg.get("selected_device") == old_name:
                cfg["selected_device"] = new_name
            save_config(cfg)
            if self._selected_device == old_name:
                self._selected_device = new_name
                # Same device, only the label changed - update tracking
                # without a reset/reconnect (settings weren't moved elsewhere).
                self._active_key = new_name
        self._refresh_device_combo(select_name=self._selected_device)
        self._host._save_config()

    def _on_device_removed(self, name: str):
        cfg = load_config()
        cfg.get("devices", {}).pop(name, None)
        save_config(cfg)
        was_selected = self._selected_device == name
        if was_selected:
            self._selected_device = self._devices[0]["name"] if self._devices else None
        self._refresh_device_combo(select_name=self._selected_device)
        # Persist the mutated devices_list too, or the removed device reappears
        # on next launch (set_config() repopulates from the stale saved list).
        self._host._save_config()
        if was_selected:
            self._activate_profile(self._profile_key)

    def _on_device_paired(self, name: str, ips: list, token: str):
        existing_names = [d["name"] for d in self._devices]
        if name in existing_names:
            for d in self._devices:
                if d["name"] == name:
                    d["ips"] = ips
                    d["token"] = token
                    break
        else:
            self._devices.append({"name": name, "ips": ips, "token": token})
        self._refresh_device_combo(select_name=name)
        self._selected_device = name
        self._host._save_config()
        self._activate_profile(self._profile_key)

    @property
    def selected_device(self) -> Optional[str]:
        """The profile key currently persisted/restored by the host - the
        selected Wi-Fi device's name, or the USB pseudo-key in USB mode."""
        return self._profile_key

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
            self._device_row_w.setVisible(True)
        else:
            self._rb_usb.setChecked(True)
            self._rb_wifi.setChecked(False)
            self._device_row_w.setVisible(False)
        if port := cfg.get("port"):
            self._port_field.setText(str(port))
            self._last_port = str(port)
        raw = cfg.get("devices_list", [])
        if not isinstance(raw, list):
            raw = []
        self._devices = []
        for d in raw:
            # Migrate old format {"name": str, "ip": str} -> {"name": str, "ips": [str]}
            if isinstance(d, dict) and "ip" in d and "ips" not in d:
                d = {"name": d.get("name"), "ips": [d["ip"]]}
            try:
                profile = DeviceProfile.from_dict(d)
            except ValueError:
                logger.warning("Discarding malformed device entry in config: %r", d)
                continue
            self._devices.append(profile.to_dict())
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
        # This restores what was already applied by _apply_config()'s call to
        # _apply_device_profile() - just record it, don't re-trigger a switch.
        self._active_key = self._profile_key
