from typing import Optional

import numpy as np
from PyQt6.QtCore import QObject, pyqtSignal
from PyQt6.QtWidgets import QWidget


class TelescopePlugin:
    name: str = ""

    def setup(self, host, bus: "EventBus"): ...
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
    phone_state_updated    = pyqtSignal(dict)
    device_changed         = pyqtSignal(str)
