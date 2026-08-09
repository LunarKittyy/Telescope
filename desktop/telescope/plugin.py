from typing import Optional, Protocol

import numpy as np
from PyQt6.QtCore import QObject, pyqtSignal
from PyQt6.QtWidgets import QWidget


UNCHANGED = object()
"""Sentinel for update_stream_output; UNCHANGED keeps current, None is a real value."""


class HostServices(Protocol):
    """Public contract for plugin-to-host calls (structural typing); plugins use this instead of reaching into private methods."""

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

    panel_region: str = "left"
    """Panel placement: "left" (connection/output), "right" (camera/image), or "center" (video stage)."""

    def setup(self, host: HostServices, bus: "EventBus"): ...
    def create_panel(self) -> Optional[QWidget]: return None

    def create_header_widget(self) -> Optional[QWidget]:
        """Compact header bar widget (e.g. device picker), not panel content."""
        return None

    def create_menu_actions(self) -> list:
        """QActions for settings menu (lets dialog-only plugins skip panel)."""
        return []
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
    camera_switched        = pyqtSignal(dict)
    """Lens switch sent to phone; carries selected camera capability dict."""
    resolution_change_requested = pyqtSignal(int, int)
    """Resolution change sent to phone; host shows pending state until confirmed."""
