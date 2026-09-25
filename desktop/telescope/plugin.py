from typing import TYPE_CHECKING, Optional, Protocol

import numpy as np
from PyQt6.QtCore import QObject, pyqtSignal
from PyQt6.QtWidgets import QWidget

if TYPE_CHECKING:
    from telescope.widgets.banner import Issue


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

    def forget_device_settings(self, name: str) -> None:
        """Delete the stored per-device settings for a removed device."""
        ...

    def reconnect_stream(self) -> None:
        """Restart the stream, if one is active, to pick up new settings."""
        ...

    def send_notification(self, title: str, body: str, urgent: bool = True) -> None:
        """Show a desktop/tray notification; urgent ones stay on screen until dismissed (Linux)."""
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
        """Push new output geometry and/or fps to the running stream worker; a no-op if nothing is streaming. UNCHANGED keeps the current value, None is a real value (pass-through resolution)."""
        ...

    def restart_vcam_canvas(self, width: int, height: int, on_done=None) -> None:
        """Recreate the virtual camera and stream at a new canvas size."""
        ...

    def quit_app(self) -> None:
        """Stop streaming, shut every plugin down and quit (the tray's Quit)."""
        ...

    def start_stream(self) -> None:
        """Start streaming, as the Start button does; a no-op if already streaming or starting."""
        ...

    def show_issue(self, key: str, issue: "Issue") -> None:
        """Show a problem banner above the body (telescope.widgets.banner.Issue); the same key replaces it."""
        ...

    def clear_issue(self, key: Optional[str] = None) -> None:
        """Remove one banner, or all of them."""
        ...


class TelescopePlugin:
    name: str = ""

    panel_region: str = "left"
    """Panel placement: "left" (connection/output), "right" (camera/image), or "center" (video stage)."""

    def setup(self, host: HostServices, bus: "EventBus"): ...
    def create_panel(self) -> Optional[QWidget]: return None

    header_side: str = "left"
    """Where the header widget goes: "left" (with the phone picker) or "right" (beside the settings button)."""

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
    def shutdown(self):
        """App is quitting: stop background services the plugin started."""


class EventBus(QObject):
    stream_started         = pyqtSignal(str)
    stream_stopped         = pyqtSignal()
    stream_connected       = pyqtSignal()
    phone_state_updated    = pyqtSignal(dict)
    device_changed         = pyqtSignal(str)
    phones_changed         = pyqtSignal(int)
    """Number of paired phones, emitted whenever the list changes."""
    add_phone_requested    = pyqtSignal()
    setup_needed           = pyqtSignal(bool)
    """First-run checklist is showing (True) or done/hidden (False); the video stage makes room for it."""
    camera_switched        = pyqtSignal(dict)
    """Lens switch sent to phone; carries selected camera capability dict."""
    resolution_change_requested = pyqtSignal(int, int)
    """Resolution change sent to phone; host shows pending state until confirmed."""
    update_requested       = pyqtSignal()
    """Show the desktop app's update dialog (e.g. the phone app turned out to be newer)."""
