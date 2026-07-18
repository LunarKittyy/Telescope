from typing import Optional, Protocol

import numpy as np
from PyQt6.QtCore import QObject, pyqtSignal
from PyQt6.QtWidgets import QWidget


class HostServices(Protocol):
    """The subset of TelescopeWindow that plugins actually call on `host`.
    Structural typing only (Protocol, not a base class) - TelescopeWindow
    doesn't need to inherit from this, it just needs to already have these
    methods, which it does. Exists so a plugin's `self._host` can be
    annotated with something narrower than the full window class."""

    def _schedule_save(self) -> None: ...
    def _save_config(self) -> None: ...
    def _switch_device(self, prev_name: Optional[str], new_name: Optional[str]) -> None: ...
    def reconnect_stream(self) -> None: ...
    def send_notification(self, title: str, body: str) -> None: ...


class TelescopePlugin:
    name: str = ""

    def setup(self, host: HostServices, bus: "EventBus"): ...
    def create_panel(self) -> Optional[QWidget]: return None
    def on_stream_start(self, stream_url: str, ctrl): ...
    def on_stream_stop(self): ...
    def on_phone_state(self, state: dict): ...
    def process_frame(self, frame: np.ndarray) -> np.ndarray: return frame
    def get_config(self) -> dict: return {}
    def set_config(self, cfg: dict): ...


class EventBus(QObject):
    frame_ready            = pyqtSignal(object)
    stream_start_requested = pyqtSignal(str)
    stream_stop_requested  = pyqtSignal()
    stream_started         = pyqtSignal(str)
    stream_stopped         = pyqtSignal()
    stream_connected       = pyqtSignal()
    phone_state_updated    = pyqtSignal(dict)
    device_changed         = pyqtSignal(str)
