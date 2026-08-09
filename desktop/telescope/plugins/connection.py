import base64
import contextlib
import logging
import threading
import time
from typing import Optional

import qrcode
from PyQt6.QtCore import Qt, pyqtSignal, QObject, QSize, QTimer
from PyQt6.QtGui import QColor, QIntValidator, QPainter, QBrush
from PyQt6.QtWidgets import (
    QButtonGroup, QDialog, QDialogButtonBox, QFormLayout, QFrame,
    QGroupBox, QHBoxLayout, QInputDialog, QLabel, QLineEdit, QListWidget,
    QMessageBox, QPushButton, QRadioButton,
    QTextEdit, QVBoxLayout, QWidget,
)

from telescope import ip_utils
from telescope.config import load_config, save_config
from telescope.ip_utils import PairingAddress
from telescope.models import DeviceProfile
from telescope.pairing import PairingServer
from telescope.platform import (
    IS_LINUX, adb_available, adb_broadcast_pair, adb_devices, adb_forward,
    adb_reverse, adb_unforward, adb_unreverse,
)
from telescope.platform.linux import (
    V4L2_OBS_DEV, V4L2_PHONE_DEV,
    v4l2_devices_ready, v4l2_load, v4l2_module_loaded,
)
from telescope.plugin import TelescopePlugin
from telescope.session_client import (
    PING_PORT, START_POLL_INTERVAL, START_TIMEOUT, PhoneSessionClient,
)
from telescope import theme
from telescope.widgets.common import (
    NoScrollComboBox, add_card_header, control_row as _row, create_card,
    create_vector_icon, segmented_row, set_ui_role,
)

logger = logging.getLogger(__name__)

DEFAULT_PORT = 8080

# PING_PORT (8766) is always-on; DEFAULT_PORT (8080) only listens during streaming.
_PAIR_STATUS_POLL_MS = 3_000

# Pseudo-device key for USB sessions to persist their own device-local profile (camera settings, etc.).
USB_PROFILE_KEY = "__usb__"

# Tolerated unreachable pings; camera startup is heavy and can starve the HTTP server briefly.
_UNREACHABLE_STREAK_LIMIT = 3


# Re-exported compatibility aliases; actual implementation is in telescope/ip_utils.py.
_get_pairing_addresses = ip_utils.get_pairing_addresses
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
        # Keep original so fields like pairing tokens aren't lost on save.
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
        set_ui_role(buttons.button(QDialogButtonBox.StandardButton.Ok), "success")
        set_ui_role(buttons.button(QDialogButtonBox.StandardButton.Cancel), "quiet")

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
    """Device list management popup — pair, edit, remove.

    A device only ever becomes usable by pairing (it needs a bearer token
    the phone issues, nothing here can fabricate one) - "Add" hands off to
    that flow instead of a bare name/IP form, which used to produce
    entries that could never actually connect."""

    def __init__(self, parent, devices: list, on_add, on_edit, on_remove):
        super().__init__(parent)
        self.setWindowTitle("Devices")
        self.setMinimumWidth(360)
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        self._devices = devices
        # on_add starts pairing flow asynchronously; result comes via _on_device_paired().
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
        self._add_btn    = QPushButton("Pair...")
        self._edit_btn   = QPushButton("Edit")
        self._remove_btn = QPushButton("Remove")
        set_ui_role(self._add_btn, "success")
        set_ui_role(self._edit_btn, "quiet")
        set_ui_role(self._remove_btn, "danger")
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
        self._on_add_cb()

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

    # qrcode's border param doesn't affect .modules matrix; explicit margin ensures quiet zone for phone cameras.
    _QUIET_ZONE_PX = 24

    def __init__(self, data: str, parent=None):
        super().__init__(parent)
        qr = qrcode.QRCode(
            error_correction=qrcode.constants.ERROR_CORRECT_L,
            box_size=1,
            border=0,
        )
        qr.add_data(data)
        qr.make(fit=True)
        self._matrix = qr.modules
        n = len(self._matrix)
        self._code_size = n * 8
        self.setFixedSize(
            self._code_size + self._QUIET_ZONE_PX * 2,
            self._code_size + self._QUIET_ZONE_PX * 2,
        )

    def paintEvent(self, event):
        n = len(self._matrix)
        cell = self._code_size // n
        margin = self._QUIET_ZONE_PX
        painter = QPainter(self)
        painter.fillRect(self.rect(), QColor("white"))
        painter.setPen(Qt.PenStyle.NoPen)
        painter.setBrush(QBrush(QColor("black")))
        for row in range(n):
            for col in range(n):
                if self._matrix[row][col]:
                    painter.drawRect(margin + col * cell, margin + row * cell, cell, cell)
        painter.end()


def _candidates_text(candidates: list) -> str:
    """The "waiting for the phone on: ..." block under the QR code."""
    lines = "\n".join(f"• {ip_utils.describe_address(c)}" for c in candidates)
    return f"Waiting for the phone on:\n{lines}"


class _PairingSignals(QObject):
    paired = pyqtSignal(str, list, str, str)  # name, ips, token, source_ip


class _PairStatusSignals(QObject):
    result = pyqtSignal(str)  # "paired" | "not_paired" | "unreachable" | "unknown"


class _PairingDialog(QDialog):
    """Runs a pairing HTTP server while open, and shows either a QR code
    (Wi-Fi) or a "Pair via ADB" button (USB) to complete it."""

    def __init__(self, parent, on_paired, usb_serial: Optional[str] = None):
        super().__init__(parent)
        self.setWindowTitle("Pair with Phone")
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        # QR payload controls matrix size; size dialog for typical pairing code plus margins.
        self.setMinimumWidth(420)
        self._on_paired = on_paired
        # If set, pairing uses adb reverse tunnel to localhost (works without Wi-Fi or with VPN).
        self._usb_serial = usb_serial
        self._pairing_server: Optional[PairingServer] = None
        self._reversed_port: Optional[int] = None
        self._pair_btn: Optional[QPushButton] = None
        self._pair_timeout: Optional[QTimer] = None
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
        self._status_lbl.setWordWrap(True)
        lay.addWidget(self._status_lbl)

        self._qr_container = QVBoxLayout()
        self._qr_container.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        self._qr_container.setContentsMargins(0, 0, 0, 12)
        lay.addLayout(self._qr_container, 1)

        # Show addresses QR code advertises; visible list helps debug unreachable phones (guest Wi-Fi, VPN).
        self._candidates_lbl = QLabel("")
        self._candidates_lbl.setObjectName("dim")
        self._candidates_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._candidates_lbl.setTextInteractionFlags(
            Qt.TextInteractionFlag.TextSelectableByMouse
        )
        self._candidates_lbl.setVisible(False)
        lay.addWidget(self._candidates_lbl)

        hint_text = (
            "Keep the Telescope app open on your phone, then click Pair via ADB below."
            if self._usb_serial is not None else
            "Open Telescope on your phone and tap the scan button in the top-right corner."
        )
        self._hint_lbl = QLabel(hint_text)
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
        server = PairingServer(
            on_paired=lambda r: signals.paired.emit(r.name, r.ips, r.token, r.source_ip)
        )

        if self._usb_serial is not None:
            # Bind first to learn actual port, then tunnel it over adb; QR at 127.0.0.1 needs the tunnel up.
            offer = server.start(
                advertise=[PairingAddress(ip="127.0.0.1", interface="USB (adb)", kind="other")]
            )
            if offer is not None:
                ok, err = adb_reverse(offer.port, serial=self._usb_serial)
                if not ok:
                    server.stop()
                    self._status_lbl.setObjectName("status_err")
                    self._status_lbl.setText(f"adb reverse failed: {err}")
                    self._status_lbl.setStyleSheet("")
                    return
                self._reversed_port = offer.port
        else:
            offer = server.start()

        if offer is None:
            self._status_lbl.setObjectName("status_err")
            self._status_lbl.setText(
                "No usable network address found. Connect this computer to the "
                "same Wi-Fi as your phone, or pair over USB instead."
            )
            self._status_lbl.setStyleSheet("")
            return
        self._pairing_server = server

        while self._qr_container.count():
            item = self._qr_container.takeAt(0)
            if item.widget():
                item.widget().deleteLater()

        self._status_lbl.setObjectName("status_dim")
        self._status_lbl.setStyleSheet("")
        if self._usb_serial is not None:
            # USB uses explicit button (not automatic scan) since MainActivity foreground is not verifiable from here.
            self._pair_btn = QPushButton("Pair via ADB")
            self._pair_btn.clicked.connect(self._send_pair_broadcast)
            self._qr_container.addWidget(self._pair_btn)
            self._status_lbl.setText("Ready to pair.")
        else:
            qr_widget = _QRCodeWidget(offer.payload)
            self._qr_container.addWidget(qr_widget)
            # Size dialog from rendered code, not hard-coded width (device name/IP list affect QR size).
            required_width = qr_widget.width() + 48
            if self.width() < required_width:
                self.resize(required_width, self.height())
            self._status_lbl.setText("Scan with the Telescope app on your phone.")
            self._candidates_lbl.setText(_candidates_text(offer.candidates))
            self._candidates_lbl.setVisible(True)

    def _send_pair_broadcast(self):
        if self._pairing_server is None or self._pairing_server.offer is None:
            return
        self._pair_btn.setEnabled(False)
        self._status_lbl.setObjectName("status_dim")
        self._status_lbl.setStyleSheet("")
        self._status_lbl.setText("Sending pairing request to phone...")
        payload_b64 = base64.b64encode(self._pairing_server.offer.payload.encode()).decode()
        ok, err = adb_broadcast_pair(payload_b64, serial=self._usb_serial)
        if not ok:
            self._status_lbl.setObjectName("status_err")
            self._status_lbl.setText(f"Broadcast failed: {err}")
            self._pair_btn.setEnabled(True)
            return
        self._status_lbl.setText("Broadcast sent - waiting for the phone to respond...")
        self._pair_timeout = QTimer(self)
        self._pair_timeout.setSingleShot(True)
        self._pair_timeout.timeout.connect(self._on_pair_timeout)
        self._pair_timeout.start(8000)

    def _on_pair_timeout(self):
        self._pair_timeout = None
        self._status_lbl.setObjectName("status_err")
        self._status_lbl.setStyleSheet("")
        self._status_lbl.setText(
            "No response after 8s. Make sure Telescope is open and in the "
            "foreground on your phone, then click Pair via ADB again."
        )
        if self._pair_btn is not None:
            self._pair_btn.setEnabled(True)

    def _stop_server(self):
        if self._pairing_server is None:
            return
        if self._pair_timeout is not None:
            self._pair_timeout.stop()
            self._pair_timeout = None
        self._pairing_server.stop()
        self._pairing_server = None
        if self._reversed_port is not None:
            adb_unreverse(self._reversed_port, serial=self._usb_serial)
            self._reversed_port = None

    def _on_paired_signal(self, name: str, ips: list, token: str, source_ip: str = ""):
        if self._pair_timeout is not None:
            self._pair_timeout.stop()
            self._pair_timeout = None
        # Replace the QR code/pair button with a big success message
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
        self._candidates_lbl.setVisible(False)
        self._on_paired(name, ips, token, source_ip)


class ConnectionPlugin(TelescopePlugin):
    name = "connection"

    def setup(self, host, bus):
        self._host             = host
        self._bus               = bus
        self._devices: list    = []
        self._selected_device: Optional[str] = None
        # Active profile key; kept separate from _selected_device to avoid spurious save/reconnect cycles.
        self._active_key: Optional[str] = None
        self._switching_device = False
        self._forwarded_port: Optional[int] = None
        self._adb_serial: Optional[str] = None
        self._device_dlg: Optional[QDialog] = None
        self._pairing_dlg: Optional[QDialog] = None
        self._last_port: str = str(DEFAULT_PORT)
        self._pair_status_signals = _PairStatusSignals()
        self._pair_status_signals.result.connect(self._set_pair_status)
        self._pair_status_check_id = 0
        # True only once stream produces a frame (not just having a saved token or worker object).
        self._stream_connected = False
        self._bus.stream_connected.connect(self._on_stream_connected)

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = QVBoxLayout(card)
        lay.setContentsMargins(16, 15, 16, 15)
        lay.setSpacing(10)
        add_card_header(lay, "Connection", "connection")

        # ── Mode ──────────────────────────────────────────────────────────────
        self._rb_wifi = QRadioButton("Wi-Fi")
        self._rb_usb  = QRadioButton("USB (ADB)")
        for rb in (self._rb_wifi, self._rb_usb):
            rb.setAutoExclusive(False)
        self._conn_grp = QButtonGroup(card)
        self._conn_grp.addButton(self._rb_usb)
        self._conn_grp.addButton(self._rb_wifi)
        self._rb_usb.setChecked(True)
        self._conn_grp.buttonClicked.connect(lambda _: self._on_mode())
        lay.addLayout(_row("Mode", segmented_row(self._rb_wifi, self._rb_usb)))

        # ── Pairing (always available - a USB-only phone still needs to be
        #     paired, it just gets there via adb reverse instead of the LAN) ──
        self._pair_status_lbl = QLabel("")
        lay.addLayout(_row("Status", self._pair_status_lbl, stretch=True))

        self._qr_btn = QPushButton("Pair Device")
        self._qr_btn.setIconSize(QSize(16, 16))
        set_ui_role(self._qr_btn, "quiet")
        self._qr_btn.clicked.connect(self._on_pair_qr)
        self._update_pair_button()
        lay.addLayout(_row("", self._qr_btn, stretch=True))

        # ── Device address (Wi-Fi only; hidden wholesale in USB mode) ────────
        self._device_row_w = QWidget()
        self._device_row_w.setObjectName("ip_row_container")
        device_v = QVBoxLayout(self._device_row_w)
        device_v.setContentsMargins(0, 0, 0, 0)
        device_v.setSpacing(4)

        self._ip_combo = NoScrollComboBox()
        self._ip_combo.currentTextChanged.connect(self._on_ip_changed)
        device_v.addLayout(_row("IP address", self._ip_combo, stretch=True))

        lay.addWidget(self._device_row_w)
        self._device_row_w.setVisible(False)

        # Build here so picker exists even if header widget is never used.
        self._build_device_picker()

        # ── Port ──────────────────────────────────────────────────────────────
        self._port_field = QLineEdit(str(DEFAULT_PORT))
        self._port_field.setValidator(QIntValidator(1, 65535))
        self._port_field.setMaximumWidth(96)
        self._port_field.editingFinished.connect(self._on_port_changed)
        lay.addLayout(_row("Port", self._port_field))

        # Periodic backstop; catches phones coming online between triggers. Stopped during streaming.
        self._pair_status_timer = QTimer(card)
        self._pair_status_timer.timeout.connect(self._check_pair_status)
        self._pair_status_timer.start(_PAIR_STATUS_POLL_MS)

        return card

    def create_header_widget(self) -> QWidget:
        """Return device picker (moved to header, not duplicated)."""
        return self._header_device_w

    def _build_device_picker(self):
        self._header_device_w = QWidget()
        self._header_device_w.setObjectName("card_body")
        lay = QHBoxLayout(self._header_device_w)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        col = QVBoxLayout()
        col.setContentsMargins(0, 0, 0, 0)
        col.setSpacing(1)
        cap = QLabel("DEVICE")
        cap.setObjectName("header_label")
        col.addWidget(cap)

        self._device_combo = NoScrollComboBox()
        self._device_combo.setMinimumWidth(196)
        self._device_combo.currentIndexChanged.connect(self._on_device_changed)
        col.addWidget(self._device_combo)
        lay.addLayout(col)

        self._gear_btn = QPushButton()
        self._gear_btn.setObjectName("icon_btn")
        self._gear_btn.setFixedSize(30, 30)
        self._gear_btn.setIcon(create_vector_icon("gear", theme.TEXT_DIM))
        self._gear_btn.setIconSize(QSize(16, 16))
        self._gear_btn.setToolTip("Manage devices")
        self._gear_btn.clicked.connect(self._on_manage_devices)
        lay.addWidget(self._gear_btn, 0, Qt.AlignmentFlag.AlignBottom)

        self._header_device_w.setVisible(self._rb_wifi.isChecked())

    def _set_wifi_rows_visible(self, visible: bool):
        """Show/hide Wi-Fi-only rows (header picker and panel address row)."""
        self._device_row_w.setVisible(visible)
        if hasattr(self, "_header_device_w"):
            self._header_device_w.setVisible(visible)

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
                    "Something else set it up first - another app's virtual camera (e.g. OBS's own), "
                    "or a previous session - with different settings than Telescope needs. "
                    "Telescope leaves it alone rather than risk breaking that.\n\n"
                    "To free it up for Telescope, run:\n"
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
                "Click Pair Device next to the device selector and follow the "
                "Wi-Fi or USB pairing steps."
            )
            return None, None, False

        if self._rb_usb.isChecked():
            if not adb_available():
                QMessageBox.critical(
                    self._host, "ADB not found",
                    "ADB is needed for USB mode but wasn't found.\n\n"
                    "Install Android platform-tools so adb is available, or use "
                    "the bundled Windows release, then try again. You can also "
                    "switch to Wi-Fi mode."
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
                QMessageBox.critical(self._host, "No device", "Pair a device in Wi-Fi mode first.")
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

    def on_stream_start(self, stream_url: str, ctrl):
        # Keep probing until _on_stream_connected confirms actual frame delivery.
        self._stream_connected = False
        self._check_pair_status()

    def on_stream_stop(self):
        if self._forwarded_port is not None:
            adb_unforward(self._forwarded_port, serial=self._adb_serial)
            self._forwarded_port = None
            self._adb_serial = None
        self._stream_connected = False
        self._pair_status_timer.start(_PAIR_STATUS_POLL_MS)
        self._check_pair_status()

    def _on_stream_connected(self):
        # Decoding frames proves working pairing; retire probe (no need to keep asking).
        self._stream_connected = True
        self._pair_status_timer.stop()
        self._set_pair_status("paired")

    # ── Pair status ──────────────────────────────────────────────────────────

    def _check_pair_status(self):
        """Background probe of whether stored token is still accepted (tokens can be stale)."""
        if self._stream_connected:
            # Already proven good by decoded frames; don't second-guess.
            self._set_pair_status("paired")
            return
        token = self._current_device_token()
        if token is None:
            self._set_pair_status("not_paired")
            return
        self._set_pair_status("checking")
        self._pair_status_check_id += 1
        check_id = self._pair_status_check_id
        usb = self._rb_usb.isChecked()
        self._spawn_pair_probe(check_id, token, usb)

    def _spawn_pair_probe(self, check_id: int, token: str, usb: bool):
        """Spawns background thread (split out so tests can make synchronous to avoid signal-on-destroyed-receiver crash)."""
        threading.Thread(
            target=self._probe_pair_status, args=(check_id, token, usb), daemon=True,
        ).start()

    def _probe_pair_status(self, check_id: int, token: str, usb: bool):
        with self.session_channel(token, usb=usb) as (client, unavailable):
            result = client.ping().status if client else unavailable
        # Don't let stale result clobber a fresher one (later check may have already finished).
        if check_id != self._pair_status_check_id:
            return
        try:
            self._pair_status_signals.result.emit(result)
        except RuntimeError:
            # App quit or plugin destroyed; QObject already gone.
            pass

    # ── Session channel (phone port 8766) ────────────────────────────────────

    @contextlib.contextmanager
    def session_channel(self, token: Optional[str] = None, usb: Optional[bool] = None):
        """Yields (client, unavailable_status) for phone's session port (Wi-Fi IP or USB adb forward)."""
        if token is None:
            token = self._current_device_token()
        if usb is None:
            usb = self._rb_usb.isChecked()
        if not token:
            yield None, "not_paired"
            return

        if not usb:
            ip = self._current_device_ip()
            if not ip:
                yield None, "not_paired"
                return
            yield PhoneSessionClient(f"http://{ip}:{PING_PORT}", token), "unreachable"
            return

        serials = adb_devices()
        if len(serials) != 1:
            yield None, "unknown"
            return
        serial = serials[0]
        ok, _err = adb_forward(PING_PORT, serial=serial)
        if not ok:
            yield None, "unreachable"
            return
        try:
            yield PhoneSessionClient(f"http://localhost:{PING_PORT}", token), "unreachable"
        finally:
            adb_unforward(PING_PORT, serial=serial)

    def ensure_phone_streaming(self, on_progress=None) -> tuple[bool, str]:
        """Start phone's camera if not already streaming. Returns (ok, reason). Calls on_progress with status updates."""
        with self.session_channel() as (client, unavailable):
            if client is None:
                return False, self._unreachable_reason(unavailable)

            ping = client.ping()
            if ping.status == "not_paired":
                return False, (
                    "The phone no longer accepts this desktop's pairing token.\n\n"
                    "Pair the device again."
                )
            if ping.status != "paired":
                return False, self._unreachable_reason("unreachable")
            if ping.streaming:
                return True, ""
            if not ping.knows_session:
                # Older app; can't start from here but may already be streaming.
                return True, ""
            if ping.local_only and not self._rb_usb.isChecked():
                return False, (
                    "The phone has \"Local only\" enabled, so its stream is reachable "
                    "over USB but not over Wi-Fi.\n\n"
                    "Switch this desktop to USB mode, or uncheck \"Local only\" on the phone."
                )

            if not ping.busy:
                if on_progress:
                    on_progress("Sending start request to phone...")
                result = client.start()
                if result.unsupported:
                    return True, ""
                if not result.ok:
                    return False, self._start_refused_reason(result.error)

            return self._await_streaming(client, on_progress)

    @staticmethod
    def _await_streaming(client: PhoneSessionClient, on_progress=None) -> tuple[bool, str]:
        """Poll until phone reports live stream (service answers immediately but camera startup is async)."""
        deadline = time.monotonic() + START_TIMEOUT
        wait_start = time.monotonic()
        started = False
        unreachable_streak = 0
        while time.monotonic() < deadline:
            time.sleep(START_POLL_INTERVAL)
            ping = client.ping()
            elapsed = time.monotonic() - wait_start
            if ping.streaming:
                return True, ""
            if ping.status == "not_paired":
                # Real state change (not network blip); don't tolerate.
                return False, "Lost contact with the phone while its camera was starting."
            if ping.status != "paired":
                # Tolerate brief unreachable (camera startup is heavy); only bail on sustained failure.
                unreachable_streak += 1
                if unreachable_streak >= _UNREACHABLE_STREAK_LIMIT:
                    return False, "Lost contact with the phone while its camera was starting."
                if on_progress:
                    on_progress(f"Phone went quiet for a moment, still waiting... ({elapsed:.0f}s)")
                continue
            unreachable_streak = 0
            if ping.busy:
                started = True
            elif started:
                return False, (
                    "The phone's camera stopped before it finished starting.\n\n"
                    "Check the phone for a permission prompt or an error."
                )
            if on_progress:
                phase = "Phone's camera is opening" if started else "Waiting for the phone's camera"
                on_progress(f"{phase}... ({elapsed:.0f}s)")
        return False, (
            "The phone's camera did not finish starting in time.\n\n"
            "Try again, or start the stream on the phone directly."
        )

    def stop_phone_streaming(self):
        """Tell phone to shut camera down (best effort; desktop side already torn down)."""
        with self.session_channel() as (client, _unavailable):
            if client is not None:
                client.stop()

    def _unreachable_reason(self, status: str) -> str:
        if status == "not_paired":
            return (
                "This device isn't paired yet.\n\nUse Pair Device to connect your phone."
            )
        if status == "unknown":
            return (
                "Couldn't tell which phone to talk to.\n\n"
                "Connect exactly one device over USB, or switch to Wi-Fi mode."
            )
        return (
            "Couldn't reach the phone.\n\n"
            "Open the Telescope app on your phone and leave it on screen, then try again."
        )

    @staticmethod
    def _start_refused_reason(error: Optional[str]) -> str:
        return {
            "no_camera_permission": (
                "The phone hasn't granted Telescope camera access.\n\n"
                "Open the app on your phone and allow the camera permission."
            ),
            "busy": (
                "The phone is already busy starting or stopping a stream.\n\nTry again in a moment."
            ),
            "start_refused": (
                "Android refused to start the camera in the background.\n\n"
                "Bring the Telescope app to the foreground on your phone and try again."
            ),
            "not_paired": (
                "The phone no longer accepts this desktop's pairing token.\n\nPair the device again."
            ),
        }.get(error or "", f"The phone refused to start streaming ({error or 'unknown error'}).")

    def _set_pair_status(self, state: str):
        color, text = {
            "paired":      ("#4db87a", "● Paired"),
            "not_paired":  ("#e57373", "○ Not paired"),
            "unreachable": ("#e0a030", "○ Unreachable"),
            "checking":    ("#78909c", "Checking..."),
            "unknown":     ("", ""),
        }.get(state, ("", ""))
        self._pair_status_lbl.setText(text)
        self._pair_status_lbl.setStyleSheet(f"color: {color};" if color else "")

    def _resolve_adb_serial(self) -> Optional[str]:
        """Return adb serial to target (prompt if multiple devices attached)."""
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
        """Profile key for current mode (Wi-Fi device name or USB_PROFILE_KEY)."""
        if self._rb_usb.isChecked():
            return USB_PROFILE_KEY
        return self._selected_device

    def _activate_profile(self, new_key: Optional[str]):
        """Switch profile via host if actually changed (avoids spurious save/reconnect from combo repopulation)."""
        if new_key == self._active_key:
            return
        prev_key = self._active_key
        self._active_key = new_key
        self._host.switch_device(prev_key, new_key)

    def _on_mode(self):
        self._set_wifi_rows_visible(self._rb_wifi.isChecked())
        self._update_pair_button()
        self._check_pair_status()
        self._host.schedule_save()
        self._activate_profile(self._profile_key)

    def _update_pair_button(self):
        """Update Pair button icon/tooltip to match mode (QR vs ADB)."""
        if self._rb_usb.isChecked():
            self._qr_btn.setIcon(create_vector_icon("usb", "#c8d0da"))
            self._qr_btn.setToolTip("Pair via ADB")
        else:
            self._qr_btn.setIcon(create_vector_icon("qr", "#c8d0da"))
            self._qr_btn.setToolTip("Pair via QR code")

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
        self._host.schedule_save()
        new_port = self._port_field.text()
        if new_port != self._last_port:
            self._last_port = new_port
            self._host.reconnect_stream()

    def _on_manage_devices(self):
        if self._device_dlg is None or not self._device_dlg.isVisible():
            self._device_dlg = _DeviceManagerDialog(
                self._host, self._devices,
                on_add=self._on_pair_qr,
                on_edit=self._on_device_edited,
                on_remove=self._on_device_removed,
            )
            self._device_dlg.setAttribute(Qt.WidgetAttribute.WA_DeleteOnClose, False)
            self._device_dlg.setWindowModality(Qt.WindowModality.NonModal)
        self._device_dlg.show()
        self._device_dlg.raise_()
        self._device_dlg.activateWindow()

    def _on_pair_qr(self):
        usb_serial = None
        if self._rb_usb.isChecked():
            if not adb_available():
                QMessageBox.critical(
                    self._host, "ADB not found",
                    "ADB is needed to pair over USB but wasn't found.\n\n"
                    "Install Android platform-tools so adb is available, or use "
                    "the bundled Windows release, then try again. You can also "
                    "switch to Wi-Fi mode to pair."
                )
                return
            usb_serial = self._resolve_adb_serial()
            if usb_serial is None:
                return

        if self._pairing_dlg is None or not self._pairing_dlg.isVisible():
            self._pairing_dlg = _PairingDialog(self._host, self._on_device_paired, usb_serial=usb_serial)
            self._pairing_dlg.setAttribute(Qt.WidgetAttribute.WA_DeleteOnClose, False)
            self._pairing_dlg.setWindowModality(Qt.WindowModality.NonModal)
        self._pairing_dlg.show()
        self._pairing_dlg.raise_()
        self._pairing_dlg.activateWindow()

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
        self._host.save_now()

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
        self._host.save_now()
        if was_selected:
            self._activate_profile(self._profile_key)

    def _on_device_paired(self, name: str, ips: list, token: str, source_ip: str = ""):
        # A fresh pairing rotates the phone's bearer token (and, on the
        # phone side, kills its own stream, since the running MjpegServer
        # only checks the token it started with) - anything the desktop was
        # mid-stream to is about to be rejected either way, so stop cleanly
        # now instead of leaving it to error out on the next request.
        # stop_stream() is a safe no-op when nothing is streaming.
        self._host.stop_stream()
        existing_names = [d["name"] for d in self._devices]
        if name in existing_names:
            for d in self._devices:
                if d["name"] == name:
                    d["ips"] = ips
                    d["token"] = token
                    break
        else:
            self._devices.append({"name": name, "ips": ips, "token": token})
        # The phone reached us from [source_ip], so that address is - right
        # now, over whatever path it found - a working way back to it. Pin it
        # as this device's active address: the rank-based default prefers a
        # Tailscale address over a LAN one, which is wrong whenever the phone
        # is on a tailnet this desktop isn't, and a saved choice from an
        # earlier pairing can be staler still. The rest stay in the dropdown
        # to switch to by hand.
        if source_ip and source_ip in ips:
            cfg = load_config()
            cfg.setdefault("devices", {}).setdefault(name, {})["active_ip"] = source_ip
            save_config(cfg)
        self._refresh_device_combo(select_name=name)
        self._selected_device = name
        self._host.save_now()
        self._activate_profile(self._profile_key)
        self._check_pair_status()
        # Pairing can now be triggered from inside the device manager
        # ("Pair..." opens this same flow) - if it's open, its list is
        # showing a now-stale snapshot of self._devices until told to redraw.
        if self._device_dlg is not None and self._device_dlg.isVisible():
            self._device_dlg._refresh_list()

    @property
    def selected_device(self) -> Optional[str]:
        """The profile key currently persisted/restored by the host - the
        selected Wi-Fi device's name, or the USB pseudo-key in USB mode."""
        return self._profile_key

    # ── Config ────────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {
            "mode":                 "wifi" if self._rb_wifi.isChecked() else "usb",
            "port":                 self._port_field.text(),
            "devices_list":         self._devices,
            # Persisted separately from the app-level "selected device" (which
            # in USB mode is the USB_PROFILE_KEY pseudo-key, not a roster
            # name) so the actually-paired device stays selected across a
            # restart regardless of which mode was active when it saved.
            "selected_device_name": self._selected_device,
        }

    def set_config(self, cfg: dict):
        if cfg.get("mode") == "wifi":
            self._rb_wifi.setChecked(True)
            self._rb_usb.setChecked(False)
            self._set_wifi_rows_visible(True)
        else:
            self._rb_usb.setChecked(True)
            self._rb_wifi.setChecked(False)
            self._set_wifi_rows_visible(False)
        self._update_pair_button()
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

        name = cfg.get("selected_device_name")
        if not isinstance(name, str) or not any(d["name"] == name for d in self._devices):
            name = self._devices[0]["name"] if self._devices else None
        self._selected_device = name
        self._refresh_device_combo(select_name=name)
        self._check_pair_status()

    def select_device(self, name: Optional[str]):
        if not name and self._devices:
            name = self._devices[0]["name"]
        self._selected_device = name
        self._refresh_device_combo(select_name=name)
        self._active_key = self._profile_key

    def sync_active_profile(self):
        """Record _active_key after the host's _apply_device_profile() has
        already applied the right device-local plugin settings at startup,
        so a later _activate_profile() doesn't spuriously re-trigger a
        switch. Deliberately doesn't touch _selected_device/the combo box -
        set_config()'s own selected_device_name already restored those; the
        app-level profile key this syncs against can be the USB pseudo-key,
        which isn't a roster device name select_device() could use."""
        self._active_key = self._profile_key
