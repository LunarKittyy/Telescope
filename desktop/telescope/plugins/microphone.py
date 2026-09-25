"""The phone's microphone as a microphone on this computer (see telescope/audio.py).

Per phone. While it's on, it follows the stream: it starts with it and stops with it. The phone only
records while something is listening.

Setting up and removing the virtual mic, and stopping the audio worker, all wait on other processes
or threads, so they run in order on one background thread and the window never waits on them.
"""

import logging
import sys
from concurrent.futures import ThreadPoolExecutor
from typing import Optional

from PyQt6.QtCore import QObject, QUrl, pyqtSignal
from PyQt6.QtGui import QDesktopServices
from PyQt6.QtWidgets import QCheckBox, QPushButton, QWidget

from telescope import audio
from telescope.platform import virtual_mic
from telescope.plugin import TelescopePlugin
from telescope.widgets.common import (
    add_card_header, card_layout, control_row, control_row_widget, create_card, set_status_kind, wrapped_note,
)

logger = logging.getLogger(__name__)


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
        self._enabled = False
        self._ctrl = None
        self._worker = None
        self._backend = self._backend or default_backend()
        self._sig = _Signals()
        self._sig.status.connect(self._on_worker_status)
        self._sig.prepared.connect(self._on_prepared)
        self._gen = 0  # bumped by every start and stop, so a late setup result can't revive a stopped mic
        self._preparing = False
        self._jobs = None
        if self._run_job is None:
            self._jobs = ThreadPoolExecutor(max_workers=1, thread_name_prefix="mic-setup")
            self._run_job = self._jobs.submit

    def create_panel(self) -> QWidget:
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, "Microphone", "mic")
        self._toggle = QCheckBox("On")
        self._toggle.setToolTip("Use the phone's microphone on this computer while streaming.")
        self._toggle.toggled.connect(self._on_toggled)
        lay.addLayout(control_row("Phone mic", self._toggle))
        self._status = wrapped_note("")
        self._status_row = control_row_widget("", self._status, stretch=True)
        lay.addWidget(self._status_row)
        self._action_btn = QPushButton()
        self._action_btn.clicked.connect(self._open_action)
        self._action_row = control_row_widget("", self._action_btn)
        lay.addWidget(self._action_row)
        self._action_url = ""
        self._show("", "dim")
        return card

    # ── State ─────────────────────────────────────────────────────────────────

    def _show(self, text: str, kind: str = "dim", action=None):
        self._status.setText(text)
        self._status_row.setVisible(bool(text))
        set_status_kind(self._status, f"status_{kind}")
        self._action_url = action[1] if action else ""
        self._action_btn.setText(action[0] if action else "")
        self._action_row.setVisible(action is not None)

    def _refresh(self):
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
        self._worker = self._worker_cls(f"{self._ctrl.base}/audio", self._ctrl.token,
                                        self._backend.open_sink, self._sig.status.emit)
        self._worker.start()

    def _stop_worker(self):
        self._gen += 1
        self._preparing = False
        worker, self._worker = self._worker, None
        if worker is not None:
            self._run_job(worker.stop)

    def _on_worker_status(self, kind: str, text: str):
        if self._worker is None:
            return  # a late report from a worker that was stopped
        if kind == "ok":
            self._show(f"Apps list it as “{self._backend.pick_name}”.", "ok")
        else:
            self._show(text if text.endswith(".") else text + ".", "err")

    def _open_action(self):
        if self._action_url:
            QDesktopServices.openUrl(QUrl(self._action_url))

    # ── Plugin hooks ──────────────────────────────────────────────────────────

    def on_stream_start(self, stream_url: str, ctrl):
        self._ctrl = ctrl
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
        return {"Microphone": "on, " + ("running" if self._worker is not None else "not running")}

    def get_config(self) -> dict:
        return {"enabled": self._enabled}

    def set_config(self, cfg: dict):
        on = bool(cfg.get("enabled", False))
        self._toggle.setChecked(on)  # runs _on_toggled when it changes
        self._refresh()
