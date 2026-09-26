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

    def is_starting(self) -> bool:
        """Whether a start is still waking the phone (is_streaming() turns true once it's through)."""
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

    def start_stream(self, interactive: bool = True) -> None:
        """Start streaming, as the Start button does; a no-op if already streaming or starting.
        interactive=False never asks anything (no password prompt): problems go to a banner."""
        ...

    def set_keep_in_tray(self, keep: bool) -> None:
        """Closing the window hides it to the tray even when idle (something is waiting to start)."""
        ...

    def show_issue(self, key: str, issue: "Issue") -> None:
        """Show a problem banner above the body (telescope.widgets.banner.Issue); the same key replaces it."""
        ...

    def clear_issue(self, key: Optional[str] = None) -> None:
        """Remove one banner, or all of them."""
        ...

    def diagnostics_report(self) -> str:
        """The Copy diagnostics text: version, system, each plugin's diagnostics() and recent events."""
        ...

    def plugin_config(self, name: str) -> Optional[dict]:
        """Another plugin's current get_config(), or None if it isn't registered."""
        ...

    def apply_preset(self, name: str, cfg: dict) -> None:
        """Hand a saved config to another plugin's apply_preset(); unknown names are ignored."""
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
    def on_stream_starting(self):
        """A stream is about to open the virtual camera; anything holding it while idle lets go now."""
    def on_stream_start(self, stream_url: str, ctrl): ...
    def on_stream_stop(self): ...
    def on_phone_state(self, state: dict): ...
    def process_frame(self, frame: np.ndarray) -> np.ndarray: return frame
    def get_config(self) -> dict: return {}
    def diagnostics(self) -> dict:
        """A few "Label": "value" lines for Copy diagnostics. No tokens, addresses or names."""
        return {}
    def set_config(self, cfg: dict): ...
    def apply_preset(self, cfg: dict):
        """Load settings from a preset; plugins that drive the phone also send them while streaming."""
        self.set_config(cfg)
    def shutdown(self):
        """App is quitting: stop background services the plugin started."""


class EventBus(QObject):
    stream_started         = pyqtSignal(str)
    stream_stopped         = pyqtSignal()
    stream_connected       = pyqtSignal()
    stream_lost            = pyqtSignal()
    """Frames stopped mid-stream; the host is looking for a route back (stream_connected ends it)."""
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
    focus_point_picked     = pyqtSignal(float, float)
    """A point picked on the preview, 0..1 in the frame as shown (after transforms)."""
    focus_point            = pyqtSignal(float, float)
    """The same point, 0..1 in the phone's own frame (transforms undone); camera control sends it."""
    focus_point_available  = pyqtSignal(bool)
    """Whether the current lens can focus on a point (and a stream is running to send it to)."""
    phone_ready            = pyqtSignal(str, bool)
    """The selected phone's id and whether Start would work right now (idle status checks only)."""
    max_zoom_changed       = pyqtSignal(int)
    """How far the Zoom slider goes (Advanced); Setup emits it on change and when its config loads."""
    update_requested       = pyqtSignal()
    """Show the desktop app's update dialog (e.g. the phone app turned out to be newer)."""
    vcam_opened            = pyqtSignal(int, int)
    """The stream opened the virtual camera at this width and height."""
    camera_watched         = pyqtSignal(bool)
    """Whether an app is reading the virtual camera (Wait screen watches it)."""
