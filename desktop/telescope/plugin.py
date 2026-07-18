from typing import Optional, Protocol

import numpy as np
from PyQt6.QtCore import QObject, pyqtSignal
from PyQt6.QtWidgets import QWidget


UNCHANGED = object()
"""Sentinel for :meth:`HostServices.update_stream_output`. A parameter left as
UNCHANGED keeps its current value; passing None is a real value (e.g. None
width/height means pass-through / no resize)."""


class HostServices(Protocol):
    """The public contract a plugin may call on its `host` handle.

    Structural typing only (Protocol, not a base class) - TelescopeWindow
    doesn't need to inherit from this, it just needs to already implement these
    methods, which it does. Exists so a plugin's `self._host` can be annotated
    with something narrower than the full window class.

    Every operation here is deliberately public: plugins go through this
    contract instead of reaching into private (`_`-prefixed) window methods or
    attributes like the stream worker. That keeps the host free to change its
    internals as long as this surface is preserved."""

    def schedule_save(self) -> None:
        """Persist all plugin config soon, coalescing rapid successive calls."""
        ...

    def save_now(self) -> None:
        """Persist all plugin config immediately, bypassing the debounce."""
        ...

    def switch_device(self, prev_name: Optional[str], new_name: Optional[str]) -> None:
        """Switch the active device/connection profile."""
        ...

    def reconnect_stream(self) -> None:
        """Restart the stream, if one is active, to pick up new settings."""
        ...

    def send_notification(self, title: str, body: str) -> None:
        """Show a desktop/tray notification."""
        ...

    def is_streaming(self) -> bool:
        """Whether a stream worker is currently active."""
        ...

    def stop_stream(self) -> None:
        """Stop the active stream. A no-op if nothing is streaming."""
        ...

    def update_stream_output(
        self, width=UNCHANGED, height=UNCHANGED, fps=UNCHANGED,
    ) -> None:
        """Push new output geometry and/or fps to the running stream worker.
        A no-op if nothing is streaming. A parameter left as UNCHANGED keeps
        its current value; None is a real value (pass-through resolution)."""
        ...

    def restart_vcam_canvas(self, width: int, height: int, on_done=None) -> None:
        """Recreate the virtual camera at a new canvas size, restarting the
        stream around it."""
        ...


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
