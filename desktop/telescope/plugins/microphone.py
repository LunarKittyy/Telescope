"""The phone's microphone as a microphone on this computer (see telescope/audio.py).

Per phone. While it's on, it follows the stream: it starts with it and stops with it. The phone only
records while something is listening.

Mute keeps the virtual mic and the stream running and writes silence, for call apps that can't mute
(or where it's out of reach). It isn't saved: nobody should start the app muted without noticing.

Setting up and removing the virtual mic, and stopping the audio worker, all wait on other processes
or threads, so they run in order on one background thread and the window never waits on them.
"""

import logging
import sys
from concurrent.futures import ThreadPoolExecutor
from typing import Optional

from PyQt6.QtCore import QObject, Qt, QTimer, QUrl, pyqtSignal
from PyQt6.QtGui import QAction, QDesktopServices
from PyQt6.QtWidgets import QCheckBox, QHBoxLayout, QPushButton, QWidget

from telescope import audio, theme
from telescope.platform import virtual_mic
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import (
    SPIN_COL_WIDTH, NoScrollSlider, NoScrollSpinBox, add_card_header, card_action, card_layout, control_row,
    control_row_widget, create_card, create_vector_icon, dim_until_paired, set_status_kind, stretch_slider, ui_px,
    value_label, wrapped_note,
)
from telescope.widgets.level_meter import FLOOR_DB, LevelMeter

logger = logging.getLogger(__name__)

METER_MS = 33
GAIN_MIN_DB = -24
GAIN_MAX_DB = 12  # until Advanced says otherwise (max_gain_changed)


class _GainSpin(NoScrollSpinBox):
    """Type an exact gain; shows +6 dB rather than 6 dB, and takes either."""

    def textFromValue(self, value: int) -> str:
        return f"{value:+d}" if value else "0"

    def valueFromText(self, text: str) -> int:
        return int(text.replace(self.suffix(), "").strip() or 0)


def _readout_row(main: QWidget, readout: QWidget) -> QHBoxLayout:
    lay = QHBoxLayout()
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(8)
    lay.addWidget(main, 1)
    lay.addWidget(readout)
    return lay


class LinuxMic:
    pick_name = virtual_mic.SOURCE_NAME

    def __init__(self):
        self._modules: list = []

    def problem(self):
        """(text, (button label, url) or None) when this computer can't have the mic, else None."""
        missing = virtual_mic.linux_tools_missing()
        if missing:
            return (f"Needs {' and '.join(missing)} (in pulseaudio-utils; PipeWire systems use it too).", None)
        return None

    def prepare(self) -> str:
        if self._modules:
            return ""
        self._modules, err = virtual_mic.linux_setup()
        return err

    def open_sink(self):
        return audio.FifoSink(virtual_mic.fifo_path())

    def teardown(self):
        virtual_mic.linux_teardown(self._modules)
        self._modules = []


class WindowsMic:
    pick_name = virtual_mic.VB_CABLE_RECORD

    def __init__(self):
        try:
            import sounddevice
        except Exception:  # missing module, or PortAudio failing to load
            sounddevice = None
        self._sd = sounddevice
        self._device: Optional[int] = None

    def problem(self):
        if self._sd is None:
            return ("The sounddevice package isn't installed.", None)
        try:
            self._device = virtual_mic.find_vb_cable(list(self._sd.query_devices()))
        except Exception:
            self._device = None
        if self._device is None:
            return ("Needs VB-Audio Virtual Cable (free). Install it, then switch this off and on.",
                    ("Get VB-Cable", virtual_mic.VB_CABLE_URL))
        return None

    def prepare(self) -> str:
        return ""

    def open_sink(self):
        return audio.SoundDeviceSink(self._sd, self._device)

    def teardown(self):
        pass


def default_backend():
    return WindowsMic() if sys.platform == "win32" else LinuxMic()


class _Signals(QObject):
    status = pyqtSignal(str, str)    # from the worker's threads
    prepared = pyqtSignal(int, str)  # start generation, error text ("" when ready)


class MicrophonePlugin(TelescopePlugin):
    name = "microphone"
    panel_region = "left"

    def __init__(self, backend=None, worker_cls=audio.AudioWorker, run_job=None):
        self._backend = backend
        self._worker_cls = worker_cls
        self._run_job = run_job  # runs a callable off the UI thread; tests pass one that runs it inline

    def setup(self, host, bus):
        self._host = host
        self._bus = bus
        self._max_gain_db = GAIN_MAX_DB
        self._limit = True
        bus.max_gain_changed.connect(self._on_max_gain)
        bus.limiter_changed.connect(self._on_limiter)
        self._enabled = False
        self._ctrl = None
        self._worker = None
        self._backend = self._backend or default_backend()
        self._sig = _Signals()
        self._sig.status.connect(self._on_worker_status)
        self._sig.prepared.connect(self._on_prepared)
        self._gen = 0  # bumped by every start and stop, so a late setup result can't revive a stopped mic
        self._preparing = False
        self._muted = False
        self._gain_db = 0
        self._jobs = None
        if self._run_job is None:
            self._jobs = ThreadPoolExecutor(max_workers=1, thread_name_prefix="mic-setup")
            self._run_job = self._jobs.submit

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        self._mute_btn = card_action("Mute", "mic")
        self._mute_btn.setCheckable(True)
        self._mute_btn.toggled.connect(self._on_mute)
        self._tray_mute = QAction("Mute microphone")
        self._tray_mute.setCheckable(True)
        self._tray_mute.triggered.connect(self._mute_btn.setChecked)  # triggered: only a click, not our own syncing
        add_card_header(lay, "Microphone", "mic", action=self._mute_btn)
        dim_until_paired(card, self._bus)
        self._toggle = QCheckBox("On")
        self._toggle.setToolTip("Use the phone's microphone on this computer while streaming. "
                                f"Apps list it as “{self._backend.pick_name}”.")
        self._toggle.toggled.connect(self._on_toggled)
        lay.addLayout(control_row("Phone mic", self._toggle))
        self._gain_slider = NoScrollSlider(Qt.Orientation.Horizontal)
        self._gain_slider.setRange(GAIN_MIN_DB, self._max_gain_db)
        self._gain_slider.setValue(0)
        self._gain_slider.set_snaps([0])
        self._gain_slider.set_default(0)
        self._gain_slider.setToolTip("Makes the phone mic louder or quieter for other apps. The range is in Advanced.")
        stretch_slider(self._gain_slider)
        self._gain_spin = _GainSpin()
        self._gain_spin.setRange(GAIN_MIN_DB, self._max_gain_db)
        self._gain_spin.setSuffix(" dB")
        self._gain_spin.setKeyboardTracking(False)  # applies on Enter or leaving the box, not every keystroke
        self._gain_spin.setFixedWidth(ui_px(SPIN_COL_WIDTH))
        self._gain_spin.setAlignment(Qt.AlignmentFlag.AlignRight)
        self._gain_slider.valueChanged.connect(self._on_gain)
        self._gain_spin.valueChanged.connect(self._gain_slider.setValue)
        self._gain_row = control_row_widget("Gain", _readout_row(self._gain_slider, self._gain_spin), stretch=True)
        lay.addWidget(self._gain_row)
        self._meter = LevelMeter()
        self._readout = value_label()
        self._readout.setFixedWidth(ui_px(SPIN_COL_WIDTH))  # the gain box's column, so both line up
        self._level_row = control_row_widget("Level", _readout_row(self._meter, self._readout), stretch=True)
        lay.addWidget(self._level_row)
        self._meter_timer = QTimer(card)
        self._meter_timer.setInterval(METER_MS)
        self._meter_timer.timeout.connect(self._tick_meter)
        self._status = wrapped_note("")
        self._status_row = control_row_widget("", self._status, stretch=True)
        lay.addWidget(self._status_row)
        self._action_btn = QPushButton()
        self._action_btn.clicked.connect(self._open_action)
        self._action_row = control_row_widget("", self._action_btn)
        lay.addWidget(self._action_row)
        self._action_url = ""
        self._show("", "dim")
        self._show_gain()
        self._show_mute()
        return card

    # ── State ─────────────────────────────────────────────────────────────────

    def _show(self, text: str, kind: str = "dim", action=None):
        self._status.setText(text)
        self._status_row.setVisible(bool(text))
        set_status_kind(self._status, f"status_{kind}")
        self._action_url = action[1] if action else ""
        self._action_btn.setText(action[0] if action else "")
        self._action_row.setVisible(action is not None)

    def _show_gain(self):
        self._gain_spin.blockSignals(True)
        self._gain_spin.setValue(self._gain_db)
        self._gain_spin.blockSignals(False)

    def _on_limiter(self, on: bool):
        self._limit = on
        if self._worker is not None:
            self._worker.limit = on

    def _on_max_gain(self, db: int):
        self._max_gain_db = db
        if not hasattr(self, "_gain_slider"):
            return  # the card isn't built yet; it starts with this range
        self._gain_slider.setMaximum(db)  # a gain past the new end comes down to it, through _on_gain
        self._gain_spin.setMaximum(db)

    def _on_gain(self, db: int):
        if db == self._gain_db:
            return
        self._gain_db = db
        if self._worker is not None:
            self._worker.gain = 10 ** (db / 20)
        self._show_gain()
        self._host.schedule_save()

    def _show_mute(self):
        self._mute_btn.setVisible(self._enabled)
        self._gain_row.setVisible(self._enabled)
        self._level_row.setVisible(self._enabled)
        self._mute_btn.setText("Unmute" if self._muted else "Mute")
        self._mute_btn.setIcon(create_vector_icon("mic_off" if self._muted else "mic",
                                                  theme.ERR if self._muted else theme.TEXT_DIM))
        self._mute_btn.setToolTip("Apps hear the phone mic again." if self._muted else
                                  "Apps hear silence, but the microphone stays connected.")
        self._tray_mute.setVisible(self._enabled)
        self._tray_mute.setChecked(self._muted)
        self._mute_btn.setProperty("muted", self._muted)
        self._mute_btn.style().unpolish(self._mute_btn)
        self._mute_btn.style().polish(self._mute_btn)
        self._meter.set_muted(self._muted)
        self._show_readout()

    def _show_readout(self):
        red = self._muted or self._meter.clipping()
        if self._muted:
            text = "Muted"
        elif self._worker is None or self._meter.held_db() <= FLOOR_DB:
            text = ""
        else:
            text = f"{round(self._meter.held_db())} dB"
        if self._readout.text() != text:
            self._readout.setText(text)
        # Inset like the gain box's text above it (its 10px padding plus the border)
        self._readout.setStyleSheet(f"padding-right: 11px;{f' color: {theme.ERR};' if red else ''}")

    def create_tray_actions(self) -> list:
        return [self._tray_mute]

    def _on_mute(self, on: bool):
        self._muted = on
        if self._worker is not None:
            self._worker.muted = on
        self._show_mute()

    def _tick_meter(self):
        if self._worker is None:
            return
        peak, rms, limited = self._worker.take_level()
        self._meter.set_level(peak, rms, limited)
        self._show_readout()

    def _refresh(self):
        self._show_mute()
        if not self._enabled:
            self._show("")
            return
        problem = self._backend.problem()
        if problem:
            self._show(problem[0], "warn", problem[1])
        elif self._worker is None and not self._preparing:
            self._show("Starts with the stream.")

    def _on_toggled(self, on: bool):
        if on == self._enabled:
            return
        self._enabled = on
        self._host.schedule_save()
        if on:
            self._refresh()
            self._start_worker()
        else:
            self._stop_worker()
            self._run_job(self._backend.teardown)
            self._refresh()

    def _start_worker(self):
        if self._worker is not None or self._preparing or self._ctrl is None or not self._enabled:
            return
        if self._backend.problem():
            return
        self._gen += 1
        self._preparing = True
        self._show("Setting up…")
        gen, signals, backend = self._gen, self._sig, self._backend

        def prepare():
            try:
                err = backend.prepare()
            except Exception as e:
                logger.exception("Microphone setup failed")
                err = f"Couldn't create the virtual microphone: {e}"
            try:
                signals.prepared.emit(gen, err)
            except RuntimeError:
                pass  # the window is gone
        self._run_job(prepare)

    def _on_prepared(self, gen: int, err: str):
        if gen != self._gen:
            return  # stopped or restarted meanwhile
        self._preparing = False
        if err:
            self._show(err, "err")
            return
        if self._ctrl is None or not self._enabled:
            self._refresh()
            return
        self._show("Connecting…")
        self._worker = self._worker_cls(f"{self._ctrl.base}/audio", self._ctrl.auth,
                                        self._backend.open_sink, self._sig.status.emit)
        self._worker.gain = 10 ** (self._gain_db / 20)
        self._worker.muted = self._muted
        self._worker.limit = self._limit
        self._worker.start()
        self._meter_timer.start()

    def _stop_worker(self):
        self._gen += 1
        self._preparing = False
        worker, self._worker = self._worker, None
        self._meter_timer.stop()
        self._meter.clear()
        self._show_readout()
        if worker is not None:
            self._run_job(worker.stop)

    def _on_worker_status(self, kind: str, text: str):
        if self._worker is None:
            return  # a late report from a worker that was stopped
        if kind == "ok":
            self._show("")  # the meter shows it's working
        else:
            self._show(text if text.endswith(".") else text + ".", "err")

    def _open_action(self):
        if self._action_url:
            QDesktopServices.openUrl(QUrl(self._action_url))

    # ── Plugin hooks ──────────────────────────────────────────────────────────

    def on_stream_start(self, stream_url: str, ctrl):
        moved = self._ctrl is not None and ctrl is not None and self._ctrl.base != ctrl.base
        self._ctrl = ctrl
        if moved:
            self._stop_worker()  # the stream came back over another route; its audio lives there too
        self._start_worker()

    def on_stream_stop(self):
        self._ctrl = None
        self._stop_worker()
        self._refresh()

    def shutdown(self):
        # Quitting may wait: the virtual mic has to be gone before the app is.
        self._gen += 1
        worker, self._worker = self._worker, None
        if self._jobs is not None:
            self._jobs.shutdown(wait=True)
        if worker is not None:
            worker.stop()
        self._backend.teardown()

    def diagnostics(self) -> dict:
        if not self._enabled:
            return {"Microphone": "off"}
        return {"Microphone": "on, " + ("running" if self._worker is not None else "not running")
                + (f", gain {self._gain_db:+d} dB" if self._gain_db else "") + (", muted" if self._muted else "")}

    def get_config(self) -> dict:
        return {"enabled": self._enabled, "gain_db": self._gain_db}

    def set_config(self, cfg: dict):
        try:
            gain = int(cfg.get("gain_db", 0))
        except (TypeError, ValueError):
            gain = 0
        self._gain_slider.setValue(gain)  # the slider holds it inside the range
        on = bool(cfg.get("enabled", False))
        self._toggle.setChecked(on)  # runs _on_toggled when it changes
        self._refresh()
