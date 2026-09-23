import threading
from typing import Optional

from PyQt6.QtCore import QObject, QTimer, pyqtSignal
from PyQt6.QtWidgets import QLabel, QWidget

from telescope import theme
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import (
    NoScrollSpinBox, add_card_header, add_section_heading, control_row as _row,
    card_layout, create_card, set_status_kind,
)

# Inline colors (change with live values) sourced from theme for one semantic color definition.
_STATUS_COLORS = {
    "ok":   theme.OK,
    "warn": theme.WARN,
    "err":  theme.ERR,
    "dim":  theme.DIM,
}


class _Signals(QObject):
    state_ready = pyqtSignal(dict)


class MonitoringPlugin(TelescopePlugin):
    name = "monitoring"
    panel_region = "right"  # the phone's own health, beside its camera

    def setup(self, host, bus):
        self._bus  = bus
        self._host = host
        self._ctrl = None
        self._battery_notified = False
        self._temp_notified    = False
        self._last_level: Optional[int] = None
        self._sig = _Signals()
        self._sig.state_ready.connect(self._on_state)

        self._timer = QTimer()
        self._timer.setInterval(15_000)
        self._timer.timeout.connect(self._poll)

        bus.phone_state_updated.connect(self._on_state)

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, "Monitoring", "status")

        # ── Live readouts ─────────────────────────────────────────────────────
        add_section_heading(lay, "Live status")
        self._battery_lbl = QLabel("—")
        set_status_kind(self._battery_lbl, "status_dim")
        lay.addLayout(_row("Battery", self._battery_lbl, stretch=True))

        self._temp_lbl = QLabel("—")
        set_status_kind(self._temp_lbl, "status_dim")
        lay.addLayout(_row("Temperature", self._temp_lbl, stretch=True))

        # ── Alert thresholds ──────────────────────────────────────────────────
        add_section_heading(lay, "Alert thresholds")
        self._batt_alert_spin = NoScrollSpinBox()
        self._batt_alert_spin.setRange(5, 95)
        self._batt_alert_spin.setValue(20)
        self._batt_alert_spin.setSuffix("%")
        self._batt_alert_spin.setToolTip(
            "Alert when battery drops below this level - including while charging, "
            "if the level keeps falling anyway"
        )
        self._batt_alert_spin.valueChanged.connect(self._host.schedule_save)
        lay.addLayout(_row("Battery", self._batt_alert_spin, stretch=True))

        self._temp_alert_spin = NoScrollSpinBox()
        self._temp_alert_spin.setRange(35, 65)
        self._temp_alert_spin.setValue(45)
        self._temp_alert_spin.setSuffix(" °C")
        self._temp_alert_spin.setToolTip("Alert when phone temperature exceeds this")
        self._temp_alert_spin.valueChanged.connect(self._host.schedule_save)
        lay.addLayout(_row("Temperature", self._temp_alert_spin, stretch=True))

        return card

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
        self._battery_notified = False
        self._temp_notified    = False
        self._last_level = None
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
        threading.Thread(target=self._fetch, args=(self._ctrl,), daemon=True).start()

    def _fetch(self, ctrl):
        state = ctrl.get_state()
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

        charge_icon = "  ·  charging" if charging else ""
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

        falling = (not charging) or (self._last_level is not None and level < self._last_level)  # Wonky charger may not keep up.
        self._last_level = level

        if falling and level <= batt_thresh and not self._battery_notified:
            self._battery_notified = True
            if charging:
                self._host.send_notification(
                    "Telescope - Low Battery",
                    f"Phone battery is at {level}% and still dropping despite being "
                    "plugged in - the charger may not be keeping up.",
                )
            else:
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
