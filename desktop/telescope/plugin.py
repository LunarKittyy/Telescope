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
        """Whether a stream is running, the camera off (just the mic) included."""
        ...

    def is_camera_on(self) -> bool:
        """False while the stream runs with the camera off, or, before a start, when it will start that way."""
        ...

    def is_camera_off_auto(self) -> bool:
        """Whether Automatic streaming turned the camera off (no app was using it), rather than the user."""
        ...

    def can_turn_camera_off(self) -> tuple:
        """(whether set_camera_on(False) would work now, and if not, why), for a tooltip."""
        ...

    def set_camera_on(self, on: bool, auto: bool = False) -> None:
        """Turn the phone's camera off (the stream carries on with the mic) or on; before a start, what it starts with.
        auto: Automatic streaming did it, so an app opening the camera may turn it back on."""
        ...

    def is_starting(self) -> bool:
        """Whether a start is still waking the phone (is_streaming() turns true once it's through)."""
        ...

    def is_restarting(self) -> bool:
        """Whether the stream stopping now starts again right after (a reconnect or virtual camera resize)."""
        ...

    def stop_stream(self, source_id: Optional[str] = None) -> None:
        """Stop the stream the panels show, or source_id's. A no-op if nothing is streaming."""
        ...

    def is_streaming_from(self, source_id: Optional[str]) -> bool:
        """Whether source_id (a phone's id or a StreamSource's) streams, or a start is waking it, shown or not."""
        ...

    def stream_count(self) -> int:
        """How many streams run at once (each to its own virtual camera)."""
        ...

    def pick_source(self, source_id: Optional[str]) -> bool:
        """The phone picker moved to source_id. True if the host handled it: that source already streams and gets the
        panels, or with several streaming it replaces the one they show. False: Connection switches as before."""
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

    def add_stream_source(self, source: "StreamSource") -> None:
        """Offer somewhere other than a phone to stream from; it's listed in the phone picker."""
        ...


class StreamSource(Protocol):
    """Something other than a paired phone to stream from, like the Browser camera. A plugin offers it with
    host.add_stream_source(); while it's the one picked, Start streams from it instead of waking a phone."""

    id: str
    """Also the key its per-device settings are saved under, so it mustn't look like a phone id."""
    name: str
    url: str
    """What stream_started and on_stream_start get as the stream's URL."""

    def prepare(self, interactive: bool) -> bool:
        """GUI thread, at Start: get ready, or show a banner and return False."""
        ...

    def open_reader(self):
        """A new reader for StreamWorker, like MjpegReader: open, isOpened, read, read_packet, decode, release."""
        ...

    def control_client(self):
        """Stands in for PhoneControlClient in on_stream_start: base, auth, send(), get_state(), close()."""
        ...

    def fps(self) -> int:
        """The rate the virtual camera runs at."""
        ...


class TelescopePlugin:
    name: str = ""

    follows_focus: bool = True
    """With several streams, hear about the one the panels show (False: the first one, like the mic)."""

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
        """QActions (or a QMenu, shown as a submenu) for the settings menu; lets dialog-only plugins skip a panel."""
        return []

    def create_tray_actions(self) -> list:
        """QActions for the tray menu, asked for once at startup; the plugin keeps them up to date (visible, checked)."""
        return []
    def on_stream_starting(self):
        """A stream is about to open the virtual camera; anything holding it while idle lets go now."""
    def on_stream_start(self, stream_url: str, ctrl): ...
    def on_stream_stop(self): ...
    def on_camera_off(self):
        """The stream carries on with the camera off: no frames until on_camera_on (after on_stream_starting)."""
    def on_camera_on(self):
        """The camera is coming back on mid-stream; on_stream_starting came just before."""
    def on_phone_state(self, state: dict): ...
    def process_frame(self, frame: np.ndarray) -> np.ndarray: return frame
    def frame_step(self):
        """process_frame frozen at the current settings, for a stream that carries on while the panels show another
        one; None leaves such streams alone (the preview only draws the one shown)."""
        return None
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
    stream_start_failed    = pyqtSignal()
    """A Start ended without a stream (the phone couldn't be reached or woken); no stream_started follows."""
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
    view_dragged           = pyqtSignal(float, float)
    """The preview was dragged by (du, dv), as fractions of the frame as shown; transforms pans."""
    view_scrolled          = pyqtSignal(float, float, float)
    """Scrolled over the preview: a zoom factor and the (u, v) under the mouse; transforms zooms around it."""
    view_pannable          = pyqtSignal(bool)
    """Whether dragging the preview pans (zoomed in); the preview shows a hand cursor."""
    lens_boxes             = pyqtSignal(list, bool)
    """What each longer lens sees, [(label, x0, y0, x1, y1)] in the frame as shown, and whether the
    user just moved the framing (the preview shows them for a moment)."""
    max_zoom_changed       = pyqtSignal(int)
    """How far the Zoom slider goes (Advanced); Setup emits it on change and when its config loads."""
    max_gain_changed       = pyqtSignal(int)
    """How far the microphone's Gain slider goes, in dB (Advanced); Setup emits it on change and when its config loads."""
    limiter_changed        = pyqtSignal(bool)
    """Whether the microphone's limiter is on (Advanced); Setup emits it on change and when its config loads."""
    update_requested       = pyqtSignal()
    """Show the desktop app's update dialog (e.g. the phone app turned out to be newer)."""
    vcam_opened            = pyqtSignal(int, int)
    """The stream opened the virtual camera at this width and height."""
    camera_watched         = pyqtSignal(bool)
    """Whether an app is reading the virtual camera (Wait screen watches it)."""
    camera_on_changed      = pyqtSignal(bool)
    """The camera went off (the stream is just the mic) or back on, or what the next start uses changed."""
    mic_changed            = pyqtSignal(bool, bool)
    """The microphone card was switched on or off, or muted (enabled, muted)."""
    stream_sources_changed = pyqtSignal(list)
    """[(id, name)] of every StreamSource plugins offer; the phone picker lists them after the phones."""
    source_selected        = pyqtSignal(str)
    """The picked StreamSource's id, or "" for a phone (or nothing): panels that only make sense for a phone hide."""
    streams_changed        = pyqtSignal(int)
    """How many streams run at once now (more than one: Add camera); stream_started and stream_stopped only mark the
    first starting and the last stopping."""
    stream_behind          = pyqtSignal(bool)
    """The stream fell behind its frame rate (the throughput readout went amber), or caught up again. Only on a
    change, and never for a dropped stream: that's stream_lost."""
