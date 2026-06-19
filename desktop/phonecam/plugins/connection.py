from typing import Optional

from PyQt6.QtCore import Qt
from PyQt6.QtGui import QIntValidator
from PyQt6.QtWidgets import (
    QButtonGroup, QDialog, QDialogButtonBox, QFormLayout, QFrame,
    QHBoxLayout, QLabel, QLineEdit, QMessageBox, QPushButton, QRadioButton,
    QSizePolicy, QVBoxLayout, QWidget,
)

from phonecam.config import load_config, save_config
from phonecam.platform import IS_LINUX, adb_available, adb_forward, adb_unforward
from phonecam.platform.linux import (
    V4L2_OBS_DEV, V4L2_PHONE_DEV,
    v4l2_devices_ready, v4l2_load, v4l2_module_loaded,
)
from phonecam.plugin import PhoneCamPlugin
from phonecam.widgets.common import NoScrollComboBox, create_vector_icon

DEFAULT_PORT = 8080


class _AddDeviceDialog(QDialog):
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
            self._err_lbl.setText("Name cannot be empty."); return
        if name in self._existing:
            self._err_lbl.setText(f'"{name}" already exists.'); return
        if not ip:
            self._err_lbl.setText("IP address cannot be empty."); return
        self.accept()

    def result_values(self) -> tuple:
        return self._name_edit.text().strip(), self._ip_edit.text().strip()


class ConnectionPlugin(PhoneCamPlugin):
    name = "connection"

    def setup(self, host, bus):
        self._host             = host
        self._devices: list    = []
        self._selected_device: Optional[str] = None
        self._switching_device = False
        self._forwarded_port: Optional[int] = None

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
        _btn_style = "padding: 0px;"
        self._add_device_btn = QPushButton("+")
        self._add_device_btn.setFixedSize(28, 28)
        self._add_device_btn.setStyleSheet(_btn_style)
        self._add_device_btn.clicked.connect(self._on_add_device)
        self._remove_device_btn = QPushButton("−")
        self._remove_device_btn.setFixedSize(28, 28)
        self._remove_device_btn.setStyleSheet(_btn_style)
        self._remove_device_btn.clicked.connect(self._on_remove_device)
        combo_row.addWidget(self._add_device_btn)
        combo_row.addWidget(self._remove_device_btn)
        device_v.addLayout(combo_row)

        ip_row = QHBoxLayout()
        ip_row.setContentsMargins(0, 0, 0, 0)
        ip_row.addSpacing(118)
        self._ip_display_lbl = QLabel("")
        self._ip_display_lbl.setObjectName("dim")
        self._ip_display_lbl.setWordWrap(True)
        ip_row.addWidget(self._ip_display_lbl, 1)
        device_v.addLayout(ip_row)

        lay.addWidget(self._device_row_w)
        self._device_row_w.setVisible(False)

        # ── Port ──────────────────────────────────────────────────────────────
        port_row = QHBoxLayout()
        port_row.setContentsMargins(0, 0, 0, 0)
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
        """Validate, ADB-forward if needed, return (url, ok). Shows error dialogs on failure."""
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
                    "Another virtual camera setup is using the module. PhoneCam won't touch it.\n\n"
                    "To use PhoneCam's setup instead, first run:\n"
                    "    sudo modprobe -r v4l2loopback\n\n"
                    "Then click Start again."
                )
                return None, False
            r = QMessageBox.question(
                self._host, "Virtual camera not ready",
                f"The virtual camera module (v4l2loopback) is not loaded.\n\n"
                f"PhoneCam will load it now. This needs admin access and may ask for your password.\n\n"
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
            prev = self._selected_device
            self._selected_device = name
            self._update_ip_display()
            self._host._switch_device(prev, name)

    def _on_add_device(self):
        existing = [d["name"] for d in self._devices]
        dlg = _AddDeviceDialog(self._host, existing_names=existing)
        if dlg.exec() != QDialog.DialogCode.Accepted:
            return
        name, ip = dlg.result_values()
        self._devices.append({"name": name, "ip": ip})
        self._refresh_device_combo(select_name=name)
        self._selected_device = name
        self._host._save_config()

    def _on_remove_device(self):
        name = self._current_device_name()
        if not name:
            return
        r = QMessageBox.question(
            self._host, "Remove device",
            f'Remove "{name}"? Its saved settings will be deleted.',
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            QMessageBox.StandardButton.No,
        )
        if r != QMessageBox.StandardButton.Yes:
            return
        self._devices = [d for d in self._devices if d["name"] != name]
        cfg = load_config()
        cfg.get("devices", {}).pop(name, None)
        cfg["devices"] = cfg.get("devices", {})
        if self._devices:
            new_name = self._devices[0]["name"]
            cfg["selected_device"] = new_name
            self._selected_device = new_name
        else:
            cfg.pop("selected_device", None)
            self._selected_device = None
        save_config(cfg)
        self._refresh_device_combo(select_name=self._selected_device)
        self._update_ip_display()

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
        self._devices = cfg.get("devices_list", [])
        # Populate combo without triggering device-change logic; host calls select_device() next
        self._switching_device = True
        self._device_combo.blockSignals(True)
        self._device_combo.clear()
        for d in self._devices:
            self._device_combo.addItem(d["name"])
        self._device_combo.blockSignals(False)
        self._switching_device = False
        self._remove_device_btn.setEnabled(bool(self._devices))

    def select_device(self, name: Optional[str]):
        """Called by host after set_config to set the active device in the combo."""
        if not name and self._devices:
            name = self._devices[0]["name"]
        self._selected_device = name
        self._refresh_device_combo(select_name=name)
