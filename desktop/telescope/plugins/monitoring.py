import shutil
import subprocess
import threading
from typing import Optional

from PyQt6.QtCore import QObject, QTimer, pyqtSignal
from PyQt6.QtWidgets import (
    QFrame, QHBoxLayout, QLabel, QVBoxLayout, QWidget,
)

from telescope.platform import IS_LINUX
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import (
    NoScrollSpinBox, add_card_header, add_section_heading, create_card,
    create_separator,
)

_STATUS_COLORS = {
    "ok":   "#66bb6a",
    "warn": "#ffa726",
    "err":  "#ef5350",
    "dim":  "#78909c",
}


class _Signals(QObject):
    state_ready = pyqtSignal(dict)


class MonitoringPlugin(TelescopePlugin):
    name = "monitoring"

    def setup(self, host, bus):
        self._bus  = bus
        self._host = host
        self._ctrl = None
        self._battery_notified = False
        self._temp_notified    = False
        self._sig = _Signals()
        self._sig.state_ready.connect(self._on_state)

        self._timer = QTimer()
        self._timer.setInterval(15_000)
        self._timer.timeout.connect(self._poll)

        bus.phone_state_updated.connect(self._on_state)

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = QVBoxLayout(card)
        lay.setContentsMargins(16, 15, 16, 15)
        lay.setSpacing(10)
        add_card_header(lay, "Monitoring", "status")

        # Live battery / temp display
        add_section_heading(lay, "Live status")
        live_row = QHBoxLayout()
        live_row.setContentsMargins(0, 0, 0, 0)
        live_row.setSpacing(20)
        lbl = QLabel("Battery / Temp")
        lbl.setObjectName("dim")
        lbl.setFixedWidth(110)
        live_row.addWidget(lbl)
        self._battery_lbl = QLabel("—")
        self._battery_lbl.setObjectName("status_dim")
        self._temp_lbl = QLabel("—")
        self._temp_lbl.setObjectName("status_dim")
        live_row.addWidget(self._battery_lbl)
        live_row.addWidget(self._temp_lbl)
        live_row.addStretch()
        lay.addLayout(live_row)

        lay.addWidget(create_separator())

        # Alert thresholds
        add_section_heading(lay, "Alerts")
        batt_row = QHBoxLayout()
        batt_row.setContentsMargins(0, 0, 0, 0)
        batt_lbl = QLabel("Battery alert")
        batt_lbl.setObjectName("dim")
        batt_lbl.setFixedWidth(110)
        batt_row.addWidget(batt_lbl)
        self._batt_alert_spin = NoScrollSpinBox()
        self._batt_alert_spin.setRange(5, 95)
        self._batt_alert_spin.setValue(20)
        self._batt_alert_spin.setSuffix("%")
        self._batt_alert_spin.setFixedWidth(90)
        self._batt_alert_spin.setToolTip("Alert when battery drops below this level while discharging")
        batt_row.addWidget(self._batt_alert_spin)
        batt_row.addStretch()
        lay.addLayout(batt_row)

        temp_row = QHBoxLayout()
        temp_row.setContentsMargins(0, 0, 0, 0)
        temp_lbl = QLabel("Temp alert")
        temp_lbl.setObjectName("dim")
        temp_lbl.setFixedWidth(110)
        temp_row.addWidget(temp_lbl)
        self._temp_alert_spin = NoScrollSpinBox()
        self._temp_alert_spin.setRange(35, 65)
        self._temp_alert_spin.setValue(45)
        self._temp_alert_spin.setSuffix(" °C")
        self._temp_alert_spin.setFixedWidth(90)
        self._temp_alert_spin.setToolTip("Alert when phone temperature exceeds this")
        temp_row.addWidget(self._temp_alert_spin)
        temp_row.addStretch()
        lay.addLayout(temp_row)

        return card

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        self._battery_notified = False
        self._temp_notified    = False
        self._battery_lbl.setText("—")
        self._temp_lbl.setText("—")
        self._timer.start()

    def on_stream_stop(self):
        self._timer.stop()
        self._ctrl = None
        self._battery_lbl.setText("—")
        self._temp_lbl.setText("—")

    def _poll(self):
        if not self._ctrl:
            return
        threading.Thread(target=self._fetch, daemon=True).start()

    def _fetch(self):
        if not self._ctrl:
            return
        state = self._ctrl.get_state()
        if state and "battery" in state:
            self._sig.state_ready.emit(state)

    def _on_state(self, state: dict):
        if "battery" not in state:
            return
        level    = int(state["battery"])
        charging = bool(state.get("charging", True))
        temp_c   = float(state.get("battery_temp_c", 0.0))
        self._update_display(level, charging, temp_c)
        self._check_alerts(level, charging, temp_c)

    def _update_display(self, level: int, charging: bool, temp_c: float):
        batt_thresh = self._batt_alert_spin.value()
        temp_thresh = self._temp_alert_spin.value()

        charge_icon = "  [charging]" if charging else ""
        if not charging and level <= batt_thresh:
            batt_color = _STATUS_COLORS["err"]
        elif not charging and level <= batt_thresh + 10:
            batt_color = _STATUS_COLORS["warn"]
        else:
            batt_color = _STATUS_COLORS["ok"]
        self._battery_lbl.setText(f"{level}%{charge_icon}")
        self._battery_lbl.setStyleSheet(f"color: {batt_color};")

        if temp_c >= temp_thresh:
            temp_color = _STATUS_COLORS["err"]
        elif temp_c >= temp_thresh - 5:
            temp_color = _STATUS_COLORS["warn"]
        else:
            temp_color = _STATUS_COLORS["ok"]
        self._temp_lbl.setText(f"{temp_c:.1f} °C")
        self._temp_lbl.setStyleSheet(f"color: {temp_color};")

    def _check_alerts(self, level: int, charging: bool, temp_c: float):
        batt_thresh = self._batt_alert_spin.value()
        temp_thresh = self._temp_alert_spin.value()

        if not charging and level <= batt_thresh and not self._battery_notified:
            self._battery_notified = True
            self._host.send_notification("Telescope - Low Battery",
                                         f"Phone battery is at {level}%.")
        elif level > batt_thresh + 5:
            self._battery_notified = False

        if temp_c >= temp_thresh and not self._temp_notified:
            self._temp_notified = True
            self._host.send_notification(
                "Telescope - Phone Running Hot",
                f"Temperature is {temp_c:.1f} C. Consider stopping charging or closing other apps.",
            )
        elif temp_c < temp_thresh - 5:
            self._temp_notified = False

    def get_config(self) -> dict:
        return {
            "battery_alert": self._batt_alert_spin.value(),
            "temp_alert":    self._temp_alert_spin.value(),
        }

    def set_config(self, cfg: dict):
        if ba := cfg.get("battery_alert"):
            self._batt_alert_spin.setValue(int(ba))
        if ta := cfg.get("temp_alert"):
            self._temp_alert_spin.setValue(int(ta))
