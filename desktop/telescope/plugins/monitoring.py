import logging
import threading
from typing import Optional

from PyQt6.QtCore import QObject, QTimer, pyqtSignal
from PyQt6.QtWidgets import QFrame, QHBoxLayout, QLabel, QVBoxLayout, QWidget

from telescope.plugin import TelescopePlugin
from telescope.widgets.banner import BannerAction, Issue
from telescope.widgets.controls import hairline
from telescope.widgets.common import (
    StatusPill, Switch,
    NoScrollSpinBox, add_card_header, add_section_heading, card_layout, create_card,
    dim_until_paired, hued_icon, ui_px,
)
from telescope.i18n import N_, number, percent, tr

logger = logging.getLogger(__name__)


class _Signals(QObject):
    state_ready = pyqtSignal(object, dict)  # the stream's control client it was read through, and the state
    other_ready = pyqtSignal(object, str, object, dict)  # the same for a stream the panels don't show: id, name first


_DEFAULTS = {"battery_alert": 20, "temp_alert": 45, "battery_notify": True, "battery_stop": False,
             "temp_notify": True, "temp_stop": False}


def _thresholds(saved) -> dict:
    """A stream's saved alert settings, the defaults for what's missing or doesn't make sense."""
    cfg = dict(_DEFAULTS)
    if isinstance(saved, dict):
        for key, default in _DEFAULTS.items():
            value = saved.get(key)
            if isinstance(default, bool) and isinstance(value, bool):
                cfg[key] = value
            elif not isinstance(default, bool) and isinstance(value, int) and not isinstance(value, bool):
                cfg[key] = value
    return cfg


def _reading(state: dict) -> Optional[tuple]:
    """(battery level, charging, temperature) from a phone state, or None without one that makes sense."""
    if "battery" not in state:
        return None
    try:
        return int(state["battery"]), bool(state.get("charging", True)), float(state.get("battery_temp_c", 0.0))
    except (TypeError, ValueError):
        return None


class MonitoringPlugin(TelescopePlugin):
    name = "monitoring"
    title = N_("Monitoring")
    about = N_("The phone's battery and temperature, with warnings when they get low or high.")
    when_off = N_("Without it there's no Phone card and nothing warns you about a low battery or a hot phone.")
    device_local = True
    panel_region = "right"  # the phone's own health, beside its camera

    def setup(self, host, bus):
        self._bus  = bus
        self._host = host
        self._ctrl = None
        self._battery_notified = False
        self._temp_notified    = False
        self._last_level: Optional[int] = None
        self._sig = _Signals()
        self._sig.state_ready.connect(self._on_polled)
        self._sig.other_ready.connect(self._on_other_polled)
        self._sid = None  # the shown stream's source
        self._others: dict = {}  # source id: what's been warned about for a stream the panels don't show
        self._allowed: dict = {}  # source id: {"batt", "temp"} started anyway, not stopped again until that's fine

        self._timer = QTimer()
        self._timer.setInterval(15_000)
        self._timer.timeout.connect(self._poll)

        bus.phone_state_updated.connect(self._on_state)

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Phone"), "phone", hue="phone")
        dim_until_paired(card, self._bus, phone_only=True)

        # ── Live readings ─────────────────────────────────────────────────────
        self._battery_lbl = StatusPill("battery")
        self._temp_lbl = StatusPill("thermometer")
        pills = QHBoxLayout()
        pills.setContentsMargins(0, 0, 0, 0)
        pills.setSpacing(8)
        pills.addWidget(self._battery_lbl, 1)
        pills.addWidget(self._temp_lbl, 1)
        lay.addLayout(pills)

        # ── Warnings ──────────────────────────────────────────────────────────
        add_section_heading(lay, tr("Warnings"), "bell")
        block = QFrame()
        block.setObjectName("subsection")
        warnings = QVBoxLayout(block)
        warnings.setContentsMargins(12, 10, 12, 10)
        warnings.setSpacing(10)
        lay.addWidget(block)
        self._batt_alert_spin = NoScrollSpinBox()
        self._batt_alert_spin.setRange(5, 95)
        self._batt_alert_spin.setValue(20)
        self._batt_alert_spin.setSuffix(tr("%", ctx="after a number"))
        self._batt_alert_spin.setToolTip(
            tr("Counts as low at this level, even while charging if it keeps dropping")
        )
        self._batt_alert_spin.valueChanged.connect(self._host.schedule_save)
        self._batt_notify, self._batt_stop = self._alert_block(
            warnings, "battery", tr("Battery under"), self._batt_alert_spin,
            tr("Show a notification when the battery runs low"),
            tr("Stop the stream when the battery runs low, and at every restart until it's fine again"))
        warnings.addWidget(hairline())

        self._temp_alert_spin = NoScrollSpinBox()
        self._temp_alert_spin.setRange(35, 65)
        self._temp_alert_spin.setValue(45)
        self._temp_alert_spin.setSuffix(tr(" °C", ctx="after a number"))
        self._temp_alert_spin.setToolTip(tr("The temperature that counts as too hot"))
        self._temp_alert_spin.valueChanged.connect(self._host.schedule_save)
        self._temp_notify, self._temp_stop = self._alert_block(
            warnings, "thermometer", tr("Hotter than"), self._temp_alert_spin,
            tr("Show a notification when the phone gets too hot"),
            tr("Stop the stream when the phone gets too hot, and at every restart until it's fine again"))

        return card

    def _alert_block(self, lay, icon_name: str, label: str, spin, notify_tip: str, stop_tip: str) -> tuple:
        """One warning: what counts, then whether to notify and stop the stream."""
        inner = lay
        top = QHBoxLayout()
        top.setSpacing(10)
        top.addWidget(hued_icon(icon_name, "phone"))
        top.addWidget(QLabel(label), 1)
        spin.setObjectName("chip_spin")
        spin.setAccessibleName(label)
        spin.setFixedSize(ui_px(80), ui_px(30))
        top.addWidget(spin)
        inner.addLayout(top)
        return self._alert_choices(inner, notify_tip, stop_tip)

    def _alert_choices(self, lay, notify_tip: str, stop_tip: str) -> tuple:
        """The Notify and Stop streaming switches of one warning."""
        notify = Switch(tr("Notify me"))
        notify.setChecked(True)
        notify.setToolTip(notify_tip)
        stop = Switch(tr("Stop the stream"))
        stop.setToolTip(stop_tip)
        for box in (notify, stop):
            box.setProperty("dim_text", True)  # the choices under a warning read quieter than the warning itself
            box.toggled.connect(self._host.schedule_save)
        boxes = QHBoxLayout()
        boxes.setContentsMargins(26, 0, 0, 0)  # under the label, past the icon
        boxes.setSpacing(24)
        boxes.addWidget(notify)
        boxes.addWidget(stop)
        lay.addLayout(boxes)
        return notify, stop

    def on_stream_start(self, stream_url: str, ctrl):
        self._timer.start()
        if ctrl is not None and ctrl is self._ctrl:
            return  # a reconnect of the same stream: keep the reading shown and don't warn about it again
        self._show_stream(ctrl, None)

    def on_focus_gained(self, stream_url: str, ctrl):
        self._timer.start()
        kept = self._others.pop(self._host.focused_source_id(), None)
        self._show_stream(ctrl, kept if kept is not None and kept["ctrl"] is ctrl else None)

    def _show_stream(self, ctrl, kept):
        """The panels show ctrl's stream now; kept is what was already warned about for it, behind another one."""
        self._ctrl, self._sid = ctrl, self._host.focused_source_id()
        self._others.pop(self._sid, None)
        self._clear_readings()
        if kept is not None:  # back on a tile: what it already warned about stays said
            self._battery_notified, self._temp_notified, self._last_level = kept["batt"], kept["temp"], kept["last"]
        else:
            self._battery_notified = self._temp_notified = False
            self._last_level = None

    def on_stream_stop(self):
        self._others.pop(self._sid, None)
        if not self._host.is_streaming():
            self._timer.stop()
            self._others.clear()
        self._forget_shown()

    def on_focus_lost(self):
        if self._ctrl is not None:  # the panels moved on: it's watched from behind
            self._others[self._sid] = {"ctrl": self._ctrl, "batt": self._battery_notified,
                                       "temp": self._temp_notified, "last": self._last_level}
        self._forget_shown()

    def _forget_shown(self):
        self._ctrl = None
        self._clear_readings()

    def _clear_readings(self):
        self._battery_lbl.set("—", "", "status_dim", "battery")
        self._temp_lbl.set("—", "", "status_dim", "thermometer")

    def _poll(self):
        if self._ctrl:
            threading.Thread(target=self._fetch, args=(self._ctrl,), daemon=True).start()
        for sid, name, ctrl in self._host.background_streams():
            if ctrl is not None:
                threading.Thread(target=self._fetch_other, args=(sid, name, ctrl), daemon=True).start()

    def _fetch(self, ctrl):
        state = ctrl.get_state()
        if state and "battery" in state:
            self._sig.state_ready.emit(ctrl, state)

    def _fetch_other(self, sid, name, ctrl):
        state = ctrl.get_state()
        if state and "battery" in state:
            self._sig.other_ready.emit(sid, name, ctrl, state)

    def _on_polled(self, ctrl, state: dict):
        if ctrl is self._ctrl:  # a reading from a stream that has since stopped (or been replaced) is dropped
            self._on_state(state)

    def _on_other_polled(self, sid, name: str, ctrl, state: dict):
        """A stream the panels don't show: its own alerts, by the thresholds saved for it."""
        if not any(s == sid and c is ctrl for s, _n, c in self._host.background_streams()):
            return  # stopped, or the panels show it now
        reading = _reading(state)
        if reading is None:
            return
        entry = self._others.get(sid)
        if entry is None or entry["ctrl"] is not ctrl:
            entry = self._others[sid] = {"ctrl": ctrl, "batt": False, "temp": False, "last": None}
        cfg = _thresholds(self._host.device_config(sid, self.name))
        self._alert(entry, *reading, cfg, sid, name)

    def _on_state(self, state: dict):
        reading = _reading(state)
        if reading is None:
            return  # a reading we can't make sense of is skipped; the next poll tries again
        self._update_display(*reading)
        self._check_alerts(*reading)

    def _update_display(self, level: int, charging: bool, temp_c: float):
        batt_thresh = self._batt_alert_spin.value()
        temp_thresh = self._temp_alert_spin.value()

        if not charging and level <= batt_thresh:
            batt_kind = "status_err"
        elif not charging and level <= batt_thresh + 10:
            batt_kind = "status_warn"
        else:
            batt_kind = "status_ok"
        self._battery_lbl.set(percent(level), tr("Charging") if charging else "", batt_kind,
                              "battery_charging" if charging else "battery")

        if temp_c >= temp_thresh:
            temp_kind = "status_err"
        elif temp_c >= temp_thresh - 5:
            temp_kind = "status_warn"
        else:
            temp_kind = "status_ok"
        feel = {"status_ok": tr("Cool"), "status_warn": tr("Warm"), "status_err": tr("Hot")}[temp_kind]
        self._temp_lbl.set(tr("{temp} °C", temp=number(temp_c, 1)), feel, temp_kind, "thermometer")

    def _check_alerts(self, level: int, charging: bool, temp_c: float):
        """The shown stream's alerts, by the thresholds on the card."""
        entry = {"batt": self._battery_notified, "temp": self._temp_notified, "last": self._last_level}
        self._alert(entry, level, charging, temp_c, self.get_config(), None, "")
        self._battery_notified, self._temp_notified, self._last_level = entry["batt"], entry["temp"], entry["last"]

    def _alert(self, entry: dict, level: int, charging: bool, temp_c: float, cfg: dict, sid, name: str):
        """Notify once per crossing of a threshold; stop on every reading past one whose Stop streaming is on.
        sid and name: a stream the panels don't show (None: the one they do)."""
        batt_thresh = int(cfg["battery_alert"])
        temp_thresh = int(cfg["temp_alert"])

        falling = (not charging) or (entry["last"] is not None and level < entry["last"])  # Wonky charger may not keep up.
        entry["last"] = level
        low = falling and level <= batt_thresh
        hot = temp_c >= temp_thresh
        streaming = self._host.is_streaming()
        key = sid if sid is not None else self._sid
        allowed = self._allowed.setdefault(key, set())
        if level > batt_thresh + 5:
            allowed.discard("batt")  # fine again: the next time it runs low stops it as usual
        if temp_c < temp_thresh - 5:
            allowed.discard("temp")
        stop_low = low and streaming and cfg["battery_stop"] is True and "batt" not in allowed
        stop_hot = hot and streaming and cfg["temp_stop"] is True and "temp" not in allowed
        phone = name or tr("Phone")

        notes = []
        if low and (stop_low or not entry["batt"]):
            entry["batt"] = True
            if cfg["battery_notify"] is not False:
                text = (tr("{phone} battery is at {level}% and still dropping despite being plugged in - the charger "
                           "may not be keeping up.", phone=phone, level=level) if charging else
                        tr("{phone} battery is at {level}%.", phone=phone, level=level))
                notes.append((tr("Telescope - Low Battery"),
                              text + (" " + tr("Stopped streaming.") if stop_low else "")))
        elif level > batt_thresh + 5:
            entry["batt"] = False

        if hot and (stop_hot or not entry["temp"]):
            entry["temp"] = True
            if cfg["temp_notify"] is not False:
                temp = number(temp_c, 1)
                text = (tr("Temperature is {temp} °C.", temp=temp) if not name
                        else tr("{phone}: temperature is {temp} °C.", phone=name, temp=temp))
                text += " " + (tr("Stopped streaming to let it cool down.") if stop_hot
                               else tr("Consider stopping charging or closing other apps."))
                notes.append((tr("Telescope - Phone Running Hot"), text))
        elif temp_c < temp_thresh - 5:
            entry["temp"] = False

        for title, text in notes:
            self._host.send_notification(title, text)
        if stop_low or stop_hot:
            logger.info("Stopping the stream: phone %s", "battery low" if stop_low else "too hot")
            self._host.stop_stream(sid)  # for the shown one it runs on_stream_stop, which clears its state here
            if stop_low:
                title = (tr("{phone}'s battery is at {level}%", phone=name, level=level) if name
                         else tr("The phone's battery is at {level}%", level=level))
                text = tr("Streaming stopped, as Monitoring stops it at {level}% or lower.", level=batt_thresh)
            else:
                temp = number(temp_c, 1)
                title = (tr("{phone} is at {temp} °C", phone=name, temp=temp) if name
                         else tr("The phone is at {temp} °C", temp=temp))
                text = tr("Streaming stopped to let it cool down, as Monitoring stops it at {temp} °C.",
                          temp=temp_thresh)
            self._host.show_issue(f"stopped:{key}", Issue(
                title, text, [BannerAction(tr("Start anyway"), lambda: self._start_anyway(key, stop_low, stop_hot))],
                kind="warn"))

    def _start_anyway(self, sid, low: bool, hot: bool):
        """Start sid again, and don't stop it for the same reason until the battery or temperature is fine again."""
        self._allowed.setdefault(sid, set()).update({"batt"} if low else set(), {"temp"} if hot else set())
        self._host.start_again(sid)()

    def get_config(self) -> dict:
        return {
            "battery_alert":  self._batt_alert_spin.value(),
            "temp_alert":     self._temp_alert_spin.value(),
            "battery_notify": self._batt_notify.isChecked(),
            "battery_stop":   self._batt_stop.isChecked(),
            "temp_notify":    self._temp_notify.isChecked(),
            "temp_stop":      self._temp_stop.isChecked(),
        }

    def set_config(self, cfg: dict):
        if ba := cfg.get("battery_alert"):
            self._batt_alert_spin.setValue(int(ba))
        if ta := cfg.get("temp_alert"):
            self._temp_alert_spin.setValue(int(ta))
        # Notifying is on unless switched off; stopping is off unless switched on.
        self._batt_notify.setChecked(cfg.get("battery_notify") is not False)
        self._temp_notify.setChecked(cfg.get("temp_notify") is not False)
        self._batt_stop.setChecked(cfg.get("battery_stop") is True)
        self._temp_stop.setChecked(cfg.get("temp_stop") is True)
