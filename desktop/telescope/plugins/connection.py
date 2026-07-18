import base64
import logging
import threading
import urllib.error
import urllib.request
from typing import Optional

import qrcode
from PyQt6.QtCore import Qt, pyqtSignal, QObject, QSize, QTimer
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
from telescope.platform import (
    IS_LINUX, adb_available, adb_broadcast_pair, adb_devices, adb_forward,
    adb_reverse, adb_unforward, adb_unreverse,
)
from telescope.platform.linux import (
    V4L2_OBS_DEV, V4L2_PHONE_DEV,
    v4l2_devices_ready, v4l2_load, v4l2_module_loaded,
)
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import (
    NoScrollComboBox, add_card_header, add_section_heading, create_card,
    create_vector_icon, set_ui_role,
)

logger = logging.getLogger(__name__)

DEFAULT_PORT = 8080

# The phone's always-on pairing-status responder (PingServer.kt) - separate
# from DEFAULT_PORT, which only has anything listening while actively
# streaming, so pairing status can't be checked through it beforehand.
PING_PORT = 8766
_PAIR_STATUS_POLL_MS = 3_000

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
        # on_add takes no arguments - it just starts the pairing flow, which
        # reports its own result asynchronously via _on_device_paired().
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

    # qrcode's own `border` param only affects make_image(), not the raw
    # .modules matrix this paints from directly - without an explicit margin
    # here the code has no quiet zone at all beyond the widget's own edge,
    # which some phone cameras struggle to autofocus/read against a dialog
    # background that isn't already white.
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


class _PairingSignals(QObject):
    paired = pyqtSignal(str, list, str)  # name, ips, token


class _PairStatusSignals(QObject):
    result = pyqtSignal(str)  # "paired" | "not_paired" | "unreachable" | "unknown"


class _PairingDialog(QDialog):
    """Runs a pairing HTTP server while open, and shows either a QR code
    (Wi-Fi) or a "Pair via ADB" button (USB) to complete it."""

    def __init__(self, parent, on_paired, usb_serial: Optional[str] = None):
        super().__init__(parent)
        self.setWindowTitle("Pair with Phone")
        self.setWindowFlag(Qt.WindowType.WindowContextHelpButtonHint, False)
        # The QR payload length controls the matrix size.  Keep the dialog
        # resizable and give the default layout enough room for the normal
        # pairing code plus its quiet zone and dialog margins.
        self.setMinimumWidth(420)
        self._on_paired = on_paired
        # If set, pairing tunnels through this adb-attached phone instead of
        # the LAN - the phone reaches the pairing server via an adb reverse
        # tunnel to its own localhost, so it works even with no Wi-Fi at all
        # (or a VPN shadowing the desktop's real LAN address).
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
        server = PairingServer(on_paired=lambda r: signals.paired.emit(r.name, r.ips, r.token))

        if self._usb_serial is not None:
            # Bind first so we know the actual port (it may have fallen back
            # off PAIRING_PORT), then tunnel that exact port over adb before
            # advertising it - a QR pointing at 127.0.0.1 only works once the
            # reverse tunnel is actually up.
            offer = server.start(advertise_ips=["127.0.0.1"])
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
            self._status_lbl.setText("No network interfaces found.")
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
            # No camera-scan step over USB: a button pushes the same payload
            # a QR code would encode straight to the phone's pairing
            # broadcast receiver over adb. Explicit and re-triggerable rather
            # than firing automatically the moment this dialog opens - the
            # phone's receiver only exists while its MainActivity is actually
            # foregrounded, and there's no way to confirm that from here
            # before sending, so a silent auto-fire had no way to tell the
            # user it needs a retry.
            self._pair_btn = QPushButton("Pair via ADB")
            self._pair_btn.clicked.connect(self._send_pair_broadcast)
            self._qr_container.addWidget(self._pair_btn)
            self._status_lbl.setText("Ready to pair.")
        else:
            qr_widget = _QRCodeWidget(offer.payload)
            self._qr_container.addWidget(qr_widget)
            # A device name or a larger IP list can add QR modules.  Size the
            # dialog from the actual rendered code instead of clipping it to a
            # hard-coded window width.
            required_width = qr_widget.width() + 48
            if self.width() < required_width:
                self.resize(required_width, self.height())
            self._status_lbl.setText("Scan with the Telescope app on your phone.")

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

    def _on_paired_signal(self, name: str, ips: list, token: str):
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
        self._on_paired(name, ips, token)


class ConnectionPlugin(TelescopePlugin):
    name = "connection"

    def setup(self, host, bus):
        self._host             = host
        self._bus               = bus
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
        self._pair_status_signals = _PairStatusSignals()
        self._pair_status_signals.result.connect(self._set_pair_status)
        self._pair_status_check_id = 0
        # True only once the current stream has actually produced a frame
        # (StreamWorker's first "ok" status) - a saved token or a worker
        # object existing is not proof the phone accepted it; a stale token
        # would otherwise pin "Paired" while StreamWorker silently retries
        # forever.
        self._stream_connected = False
        self._bus.stream_connected.connect(self._on_stream_connected)

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = QVBoxLayout(card)
        lay.setContentsMargins(16, 15, 16, 15)
        lay.setSpacing(10)
        add_card_header(lay, "Connection", "connection")

        # ── Mode ──────────────────────────────────────────────────────────────
        add_section_heading(lay, "Connection mode")
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

        # ── Pair (always visible - a USB-only phone still needs to be paired,
        #     it just gets there via adb reverse instead of the LAN) ──────────
        add_section_heading(lay, "Phone")
        pair_row = QHBoxLayout()
        pair_row.setContentsMargins(0, 0, 0, 0)
        pair_row.setSpacing(6)
        pair_lbl = QLabel("Pair")
        pair_lbl.setObjectName("dim")
        pair_lbl.setFixedWidth(110)
        pair_lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        pair_row.addWidget(pair_lbl)

        _icon_color = "#c8d0da"
        _icon_size  = QSize(18, 18)
        self._qr_btn = QPushButton()
        self._qr_btn.setFixedSize(28, 28)
        self._qr_btn.setIconSize(_icon_size)
        set_ui_role(self._qr_btn, "quiet")
        self._qr_btn.clicked.connect(self._on_pair_qr)
        self._update_pair_button()
        pair_row.addWidget(self._qr_btn)

        self._pair_status_lbl = QLabel("")
        pair_row.addWidget(self._pair_status_lbl)
        pair_row.addStretch()
        lay.addLayout(pair_row)

        # ── Device list (Wi-Fi only) ────────────────────────────────────────
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

        self._gear_btn = QPushButton()
        self._gear_btn.setFixedSize(28, 28)
        set_ui_role(self._gear_btn, "quiet")
        self._gear_btn.setIcon(create_vector_icon("gear", _icon_color))
        self._gear_btn.setIconSize(_icon_size)
        self._gear_btn.setToolTip("Manage devices")
        self._gear_btn.clicked.connect(self._on_manage_devices)
        combo_row.addWidget(self._gear_btn)
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
        add_section_heading(lay, "Network")
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

        # Backstop for the trigger-based checks above: catches a phone that
        # comes online (app opened, adb plugged in) between triggers,
        # without needing the user to touch anything. Cheap enough to run
        # often - one tiny HTTP round-trip (plus an adb forward/unforward in
        # USB mode) every few seconds, dwarfed by the video stream itself.
        # Stopped while actually streaming - see on_stream_start/_stop.
        self._pair_status_timer = QTimer(card)
        self._pair_status_timer.timeout.connect(self._check_pair_status)
        self._pair_status_timer.start(_PAIR_STATUS_POLL_MS)

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

    def on_stream_start(self, stream_url: str, ctrl):
        # A worker existing isn't proof the phone accepted it yet - keep
        # probing (an unconfirmed connection can't rely on "is streaming" as
        # a pinned-good signal) until _on_stream_connected fires.
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
        # Actively streaming is its own proof of a working pairing - no need
        # to keep polling PingServer, which (unlike the streaming server) is
        # tied to MainActivity's foreground lifetime and would wrongly read
        # "unreachable" the moment the phone's screen is minimized mid-stream.
        self._stream_connected = True
        self._pair_status_timer.stop()
        self._set_pair_status("paired")

    # ── Pair status ──────────────────────────────────────────────────────────

    def _check_pair_status(self):
        """Kicks off a background probe of whether the current profile's
        stored token is actually still accepted by the phone right now, not
        just whether one happens to be saved locally - a saved token can be
        stale (the phone was reset, or paired to a different desktop since).
        Runs off the UI thread since it may make a network call (and, in USB
        mode, shell out to adb); the result comes back via a Qt signal."""
        if self._stream_connected:
            # Belt-and-suspenders for the same reason as _on_stream_connected:
            # any trigger firing while already streaming (the periodic timer
            # is stopped, but a mode switch mid-stream, say, still isn't
            # impossible) shouldn't second-guess a connection already known
            # to be good via a check that can't see it while minimized.
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
        """Split out from _check_pair_status() so tests can make this
        synchronous - a real background thread that outlives its QObject
        (e.g. the widgets/plugin it was fired from getting torn down at the
        end of a test, while its 3s network timeout is still pending) is a
        real crash: PyQt aborts hard when a queued cross-thread signal is
        finally delivered to an already-destroyed receiver."""
        threading.Thread(
            target=self._probe_pair_status, args=(check_id, token, usb), daemon=True,
        ).start()

    def _probe_pair_status(self, check_id: int, token: str, usb: bool):
        result = self._probe_usb(token) if usb else self._probe_wifi(token)
        # A later check (mode switched again, re-paired) may have already
        # started and finished while this one was still in flight - don't
        # let a stale result clobber a fresher one.
        if check_id != self._pair_status_check_id:
            return
        try:
            self._pair_status_signals.result.emit(result)
        except RuntimeError:
            # The app quit (or, in tests, the plugin/qapp was torn down)
            # while this network call was still in flight - the receiving
            # QObject is already gone, and there's nothing left to update.
            pass

    def _probe_wifi(self, token: str) -> str:
        ip = self._current_device_ip()
        if not ip:
            return "not_paired"
        return self._probe_url(f"http://{ip}:{PING_PORT}/v1/ping", token)

    def _probe_usb(self, token: str) -> str:
        serials = adb_devices()
        if len(serials) != 1:
            return "unknown"
        serial = serials[0]
        # A short-lived forward dedicated to the ping port - separate from
        # whatever port streaming forwards, since the phone's PingServer
        # binds its own fixed port independent of streaming (see
        # PingServer.kt) and is normally not already forwarded.
        ok, _err = adb_forward(PING_PORT, serial=serial)
        if not ok:
            return "unreachable"
        try:
            return self._probe_url(f"http://localhost:{PING_PORT}/v1/ping", token)
        finally:
            adb_unforward(PING_PORT, serial=serial)

    @staticmethod
    def _probe_url(url: str, token: str) -> str:
        req = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"})
        try:
            with urllib.request.urlopen(req, timeout=3) as r:
                return "paired" if r.status == 200 else "unreachable"
        except urllib.error.HTTPError as exc:
            return "not_paired" if exc.code == 401 else "unreachable"
        except Exception:
            return "unreachable"

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
        self._host.switch_device(prev_key, new_key)

    def _on_mode(self):
        self._device_row_w.setVisible(self._rb_wifi.isChecked())
        self._update_pair_button()
        self._check_pair_status()
        self._host.schedule_save()
        self._activate_profile(self._profile_key)

    def _update_pair_button(self):
        """The Pair button opens different flows depending on mode (a QR
        scan over Wi-Fi, an adb-pushed pairing broadcast over USB) - its
        icon/tooltip should say which."""
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
                    "Click the Download ADB button in the Windows Setup section "
                    "and try again, or switch to Wi-Fi mode to pair."
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

    def _on_device_paired(self, name: str, ips: list, token: str):
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
            self._device_row_w.setVisible(True)
        else:
            self._rb_usb.setChecked(True)
            self._rb_wifi.setChecked(False)
            self._device_row_w.setVisible(False)
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
