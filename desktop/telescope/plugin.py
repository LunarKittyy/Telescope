import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Callable, Optional, Protocol

import numpy as np
from PyQt6.QtCore import QObject, pyqtSignal
from PyQt6.QtWidgets import QWidget
from telescope.i18n import N_, tr

if TYPE_CHECKING:
    from telescope.view import View
    from telescope.widgets.banner import Issue


UNCHANGED = object()
"""Sentinel for update_stream_output; UNCHANGED keeps current, None is a real value."""

SETTINGS_TABS = (
    ("general", N_("General")),
    ("streaming", N_("Streaming")),
    ("video", N_("Video")),
    ("audio", N_("Audio")),
    ("devices", N_("Virtual camera & phone")),
    ("shortcuts", N_("Keyboard shortcuts")),
    ("plugins", N_("Plugins")),
    ("about", N_("Updates & About")),
)
"""The tabs of the settings window, in order: (id, name). A SettingsSection names one by id; a tab nobody puts anything
in isn't shown. An id that isn't here makes a tab of its own, between Keyboard shortcuts and Plugins."""


@dataclass
class SettingsSection:
    """A block of controls for the settings window (usually a card from create_card)."""

    tab: str
    """Which tab it goes in: an id from SETTINGS_TABS, or a new one (then give tab_title)."""
    widget: QWidget
    """What's shown. Controls apply as they change; the window has no OK button."""
    order: int = 0
    """Where it sits in its tab: lower first, then in registration order."""
    on_show: Optional[Callable[[], None]] = None
    """Called each time its tab comes on screen, to bring the controls up to date with the plugin's state."""
    tab_title: str = ""
    """The name of the tab, for one that isn't in SETTINGS_TABS (or to replace the usual one)."""
    keywords: str = ""
    """Extra words the search finds everything in this section by (what people might call it)."""


class HostSettings(Protocol):
    """Saving and reading settings: this plugin's, another's, and each device's."""

    def schedule_save(self) -> None:
        """Persist all plugin config soon, coalescing rapid successive calls."""
        ...

    def save_now(self) -> None:
        """Persist all plugin config immediately, bypassing the debounce."""
        ...

    def plugin_config(self, name: str) -> Optional[dict]:
        """Another plugin's current get_config(), or None if it isn't registered."""
        ...

    def apply_preset(self, name: str, cfg: dict) -> None:
        """Hand a saved config to another plugin's apply_preset(); unknown names are ignored."""
        ...

    def preset_plugins(self) -> list:
        """The names of the running plugins that are in_presets, in the order they're registered (and applied)."""
        ...

    def device_config(self, source_id: Optional[str], plugin_name: str) -> dict:
        """The settings saved for source_id's stream in a device-local plugin (TelescopePlugin.device_local), for one
        the panels don't show; {} without any."""
        ...

    def has_device_settings(self, name: str) -> bool:
        """Whether settings are saved for this phone or source."""
        ...

    def switch_device(self, prev_name: Optional[str], new_name: Optional[str]) -> None:
        """Switch the active device/connection profile."""
        ...

    def forget_device_settings(self, name: str) -> None:
        """Delete the stored per-device settings for a removed device."""
        ...

    def move_device_settings(self, old: str, new: str) -> None:
        """Store a device's settings (presets included) and its virtual camera under a new id."""
        ...


class HostStreams(Protocol):
    """Starting, stopping and following streams, the one the panels show and those behind it."""

    def is_streaming(self) -> bool:
        """Whether a stream is running, the camera off (just the mic) included."""
        ...

    def is_starting(self) -> bool:
        """Whether a start is still waking the phone (is_streaming() turns true once it's through)."""
        ...

    def is_restarting(self) -> bool:
        """Whether the stream stopping now starts again right after (a reconnect or virtual camera resize)."""
        ...

    def is_streaming_from(self, source_id: Optional[str]) -> bool:
        """Whether source_id (a phone's id or a StreamSource's) streams, or a start is waking it, shown or not."""
        ...

    def stream_count(self) -> int:
        """How many streams run at once (each to its own virtual camera)."""
        ...

    def start_stream(self, interactive: bool = True) -> None:
        """Start streaming, as the Start button does; a no-op if already streaming or starting.
        interactive=False never asks anything (no password prompt): problems go to a banner."""
        ...

    def start_again(self, source_id: Optional[str] = None, before: Optional[Callable[[], None]] = None
                    ) -> Callable[[], None]:
        """For a banner's Try again: starts source_id (by default what is starting now) once more, next to any other
        stream (Start would only start one while nothing streams). before runs first, with the panels on it, so a
        setting it changes (e.g. Switch to Heavy) is that source's."""
        ...

    def stop_stream(self, source_id: Optional[str] = None) -> None:
        """Stop the stream the panels show, or source_id's. A no-op if nothing is streaming."""
        ...

    def stop_all_streams(self) -> None:
        """Stop every stream, as the Stop button does."""
        ...

    def reconnect_stream(self) -> None:
        """Restart the stream, if one is active, to pick up new settings."""
        ...

    def hand_over_stream(self, route, source_id: Optional[str] = None) -> bool:
        """Move a running phone stream (the panels', or source_id's) onto another route (a phones.Route) without a
        gap: it opens there next to the old one and moves once it's healthy. False if it can't try now."""
        ...

    def focused_source_id(self) -> Optional[str]:
        """The source the panels show (or Start would stream from), for a banner about it to act on it later."""
        ...

    def background_streams(self) -> list:
        """[(source id, name, control client)] of the streams the panels don't show."""
        ...

    def pick_source(self, source_id: Optional[str]) -> bool:
        """The phone picker moved to source_id. True if the host handled it: that source already streams and gets the
        panels, or with several streaming it replaces the one they show. False: Connection switches as before."""
        ...

    def main_stream(self) -> Optional[tuple]:
        """(id, name) of the stream plugins that don't follow the panels (the mic) are on, while one streams."""
        ...

    def make_main(self, source_id: str) -> bool:
        """Move the plugins that don't follow the panels to source_id's stream: they get its saved settings, and the
        stream they leave keeps theirs as they are now. False if it doesn't stream."""
        ...

    def update_stream_output(
        self, width=UNCHANGED, height=UNCHANGED, fps=UNCHANGED, source_id: Optional[str] = None,
    ) -> None:
        """Push new output geometry and/or fps to the running stream worker (the panels', or source_id's); a no-op if
        it isn't streaming. UNCHANGED keeps the current value, None is a real value (pass-through resolution)."""
        ...

    def canvas_size(self) -> tuple:
        """(width, height) of the virtual camera canvas Setup asks for, or (None, None) for Auto."""
        ...

    def restart_vcam_canvas(self, width: int, height: int, on_done=None) -> None:
        """Recreate the virtual camera and stream at a new canvas size; on_done(ok, message, command) after, the command
        one to run by hand when Telescope couldn't ask for the password (shown by on_done, or as a banner without it)."""
        ...

    def stream_output(self, source_id: str) -> str:
        """The virtual camera source_id streams to, as apps list it, or "" when it doesn't stream."""
        ...

    def stream_on_camera(self, slot: int = 0) -> Optional[tuple]:
        """(source id, width, height) of the stream going to virtual camera slot (0 is the one apps list as Telescope),
        or None when none does. Width and height are None until its camera has opened, and with the camera off."""
        ...

    def remove_extra_cameras(self, on_done=None) -> None:
        """Take away the virtual cameras Add camera set up, back to the one; on_done(ok, message) after."""
        ...


class HostCamera(Protocol):
    """The phone's camera going off while its mic streams on, and back."""

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


class HostSources(Protocol):
    """Places other than a paired phone to stream from (StreamSource)."""

    def add_stream_source(self, source: "StreamSource") -> None:
        """Offer somewhere other than a phone to stream from; it's listed in the phone picker. Offering one with the
        same id again updates its name."""
        ...

    def remove_stream_source(self, source_id: str) -> None:
        """Take a StreamSource back (stopping its stream); the picker moves off it."""
        ...

    def stream_source(self, source_id: str) -> None:
        """Start a StreamSource: picked and streamed when nothing streams yet, else next to the others."""
        ...


class HostShell(Protocol):
    """The window around the plugins: banners, notifications, the tray and quitting."""

    def show_issue(self, key: str, issue: "Issue") -> None:
        """Show a problem banner above the body (telescope.widgets.banner.Issue); the same key replaces it."""
        ...

    def clear_issue(self, key: Optional[str] = None, issue: Optional["Issue"] = None) -> None:
        """Remove one banner, or all of them; with issue, only while key still shows that one."""
        ...

    def send_notification(self, title: str, body: str, urgent: bool = True) -> None:
        """Show a desktop/tray notification; urgent ones stay on screen until dismissed (Linux)."""
        ...

    def set_keep_in_tray(self, keep: bool) -> None:
        """Closing the window hides it to the tray even when idle (something is waiting to start)."""
        ...

    def toggle_window(self) -> bool:
        """Hide the window to the tray if it's showing (or minimize it without a tray), else bring it up. Returns
        whether it's showing now."""
        ...

    def quit_app(self) -> None:
        """Stop streaming, shut every plugin down and quit (the tray's Quit)."""
        ...

    def restart_app(self) -> bool:
        """Start a new copy of Telescope and quit this one (Restart now); False if the new one couldn't start."""
        ...

    def diagnostics_report(self) -> str:
        """The Copy diagnostics text: version, system, each plugin's diagnostics() and recent events."""
        ...

    def open_settings(self, tab: str = "") -> None:
        """Bring up the settings window, on the tab with this id (see SETTINGS_TABS) or the one it was left on."""
        ...

    def shortcut_actions(self) -> list:
        """Every plugin's create_actions(), in registration order."""
        ...


class HostView(Protocol):
    """The shown stream's view: zoom, pan, flip and rotation, done before the plugins see the frame."""

    def view(self) -> "View":
        """The shown stream's view as it is now (telescope.view.View)."""
        ...

    def view_aspect(self) -> float:
        """Width over height of the shown stream's picture as shown (after rotation), 0 before its first frame."""
        ...

    def set_view(self, **changes) -> None:
        """Change the manual view (zoom, pan_x, pan_y, flip_h, flip_v, rotation); the phone does its share of the zoom.
        While something has taken the view, zoom and pan wait until it lets go."""
        ...

    def take_view(self, owner: str) -> bool:
        """Move zoom and pan yourself (owner: what the card says is moving them); False if something else has it."""
        ...

    def steer_view(self, owner: str, zoom: float, pan_x: float, pan_y: float) -> None:
        """Set zoom and pan while owner has the view taken; ignored otherwise."""
        ...

    def release_view(self, owner: str) -> None:
        """Let go of the view: zoom and pan go back to the manual ones."""
        ...


class HostVision(Protocol):
    """People in the picture, from MediaPipe: loaded the first time something asks, None or nothing until then."""

    def vision_state(self) -> str:
        """"off" (nothing asked yet), "loading", "ready", "missing" (not downloaded) or "failed"; vision_state_changed."""
        ...

    def person_mask(self, frame: np.ndarray) -> Optional[np.ndarray]:
        """From process_frame or a frame_step only: how sure it is each pixel of frame is a person (uint8 0..255, the
        frame's size), or None while it can't say. The same frame asked twice is worked out once."""
        ...

    def want_people(self, owner: str, on: bool) -> None:
        """While on, people_seen reports where the people in the shown stream are, about 12 times a second while
        anything moves and 3 while nothing does."""
        ...


@dataclass(frozen=True)
class StreamFrame:
    """The newest picture of one running stream, as it goes out to its virtual camera (after that stream's plugins)."""

    source_id: str
    """The stream's source: a phone's id or a StreamSource's ("" for the stream with no phone picked)."""
    name: str
    """What the phone picker and the stream's tile call it."""
    frame: np.ndarray
    """BGR, read-only and shared with the stream: never copied unless copy=True was asked for."""
    number: int
    """Goes up with every frame any stream sends out, so a different number is a different frame."""
    time: float
    """When the stream made it, in time.monotonic() seconds."""

    def age(self, now: Optional[float] = None) -> float:
        """Seconds since it was made. A stream that dropped keeps its last frame, which only gets older."""
        return (time.monotonic() if now is None else now) - self.time


class HostFrames(Protocol):
    """Frames from other streams: the newest picture of every running stream, from any thread and without copying."""

    def stream_frames(self, copy: bool = False) -> list:
        """[StreamFrame] of every running stream that has sent a picture, in the order they started. A stream that
        stops is gone from the next call; copy=True hands out frames that are yours to change."""
        ...

    def stream_frame(self, source_id: Optional[str], copy: bool = False) -> Optional[StreamFrame]:
        """The newest StreamFrame of source_id's stream, or None while it doesn't run or hasn't sent a picture yet."""
        ...

    def frame_source(self) -> Optional[str]:
        """From process_frame or a frame_step only: the source id of the stream the frame being worked on is from."""
        ...

    def replace_frame(self, source_id: Optional[str], frame: np.ndarray, number: int) -> bool:
        """From any thread: frame (BGR, never changed after) goes out on source_id's virtual camera in place of that
        stream's newest frame, the one numbered number. False, and nothing sent, once the stream has a newer frame or
        doesn't run. For a picture that should move on while the stream it goes out on stalls; stream_frame() keeps
        handing out the stream's own frame."""
        ...


class HostServices(HostSettings, HostStreams, HostCamera, HostSources, HostShell, HostView, HostVision, HostFrames,
                   Protocol):
    """Everything a plugin may call on the host (structural typing), instead of reaching into private methods.
    A plugin that only needs part of it can say so with one of the narrower protocols above."""


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

    # Optional:
    # redirect() -> Optional[str]: at Start with nothing streaming, another source's id to stream instead.
    # remember_changes_only = True: its settings are only saved where they differ from the defaults.


class StreamsBehind:
    """What a plugin following the panels keeps about a stream they moved away from while it streams on, so coming
    back to it picks up where it was instead of starting over (and resending what the phone already has)."""

    def __init__(self):
        self._kept: dict = {}

    def keep(self, host: HostStreams, ctrl, value):
        """From on_focus_lost: value is kept for the stream the panels are leaving."""
        sid = host.focused_source_id()
        if ctrl is not None:
            self._kept[sid] = (ctrl, value)
        else:
            self._kept.pop(sid, None)

    def take(self, host: HostStreams, ctrl):
        """From on_focus_gained: what keep() held for this stream, or None if it has started again since."""
        kept = self._kept.pop(host.focused_source_id(), None)
        return kept[1] if kept is not None and kept[0] is ctrl else None

    def drop(self, host: HostStreams):
        """From on_stream_stop: the shown stream ended, so nothing about it is kept (and nothing at all once idle)."""
        self._kept.pop(host.focused_source_id(), None)
        if not host.is_streaming():
            self._kept.clear()


class TelescopePlugin:
    name: str = ""
    """Its id: where its settings are saved, and what other plugins and the Plugins tab know it by."""

    title: str = ""
    """What the Plugins tab calls it ("Camera"); a plugin someone added takes it from its plugin.json.
    Built-in plugins mark it, about and when_off with N_() and the window shows them through tr()."""
    about: str = ""
    """One plain sentence for the Plugins tab: what it does."""
    when_off: str = ""
    """What goes away while it's turned off, so nobody wonders where a card went ("The Camera card goes away...")."""
    required: bool = False
    """Telescope can't work without it, so the Plugins tab doesn't offer to turn it off."""
    optional: bool = False
    """A built-in one that's off until someone switches it on in Plugins, since it asks more of the computer."""
    needs_vision: bool = False
    """It uses person_mask or want_people: switching it on in Plugins downloads the vision pack first."""

    in_presets: bool = False
    """Presets save its get_config() and bring it back through apply_preset(), in the order plugins are registered."""

    device_local: bool = False
    """Each phone (or other source) gets its own copy of this plugin's settings, swapped in when it's picked or the
    panels move to its stream; False: one copy for the whole app."""

    sees_final_frame: bool = False
    """Its process_frame runs after every other plugin's, so it sees the frame the virtual camera gets (the preview).
    It has to hand the frame back unchanged."""
    follows_focus: bool = True
    """With several streams, hear about the one the panels show (False: the first one, like the mic)."""

    panel_region: str = "left"
    """Panel placement: "left" (connection/output), "right" (camera/image), or "center" (video stage)."""

    def setup(self, host: HostServices, bus: "EventBus"):
        """Plugins are set up one at a time, so one set up later misses what this one emits here: state other plugins
        follow is announced again from on_plugins_ready."""
    def create_panel(self) -> Optional[QWidget]: return None

    header_side: str = "left"
    """Where the header widget goes: "left" (with the phone picker) or "right" (beside the settings button)."""

    def create_header_widget(self) -> Optional[QWidget]:
        """Compact header bar widget (e.g. device picker), not panel content."""
        return None

    def create_settings_sections(self) -> list:
        """SettingsSections for the settings window, asked for once when it first opens; lets a plugin without a panel
        still have settings. Its controls apply as they change."""
        entries = self.create_menu_actions()
        if not entries:
            return []
        # Keep API 1 plugins' old actions reachable for the transition to SettingsSections.
        import logging
        from PyQt6 import sip
        from PyQt6.QtCore import Qt
        from PyQt6.QtWidgets import QMenu, QToolButton
        from telescope.widgets.common import add_card_header, card_layout, create_card

        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr(self.title or self.name), "plug")
        button = QToolButton(card)
        button.setText(tr("Open settings"))
        button.setPopupMode(QToolButton.ToolButtonPopupMode.InstantPopup)
        menu = QMenu(button)

        def fill(items):
            for entry in items:
                if isinstance(entry, QMenu):
                    entry.setParent(menu, entry.windowFlags())
                    menu.addMenu(entry)
                else:
                    entry.setParent(menu)
                    menu.addAction(entry)

        def refresh():
            nonlocal entries
            menu.clear()
            for entry in entries:
                if not sip.isdeleted(entry):  # QMenu.clear() already deletes actions it owns
                    entry.deleteLater()
            entries = []
            try:
                entries = self.create_menu_actions()
                fill(entries)
            except Exception:
                logging.exception("Plugin %s couldn't list its legacy settings", self.name)

        fill(entries)
        menu.aboutToShow.connect(refresh)  # the old hook was asked afresh on every menu open
        button.setMenu(menu)
        lay.addWidget(button, 0, Qt.AlignmentFlag.AlignLeft)
        return [SettingsSection("general", card)]

    def create_menu_actions(self) -> list:
        """Deprecated API 1 hook: QActions and QMenus, shown under this plugin in Settings > General.
        New plugins should use create_settings_sections()."""
        return []

    def create_tray_actions(self) -> list:
        """QActions for the tray menu, asked for once at startup; the plugin keeps them up to date (visible, checked)."""
        return []

    def create_actions(self) -> list:
        """telescope.shortcuts.ShortcutActions keys can be bound to, asked for once, after create_panel()."""
        return []
    def on_stream_starting(self):
        """A stream is about to open the virtual camera; anything holding it while idle lets go now."""
    def on_stream_start(self, stream_url: str, ctrl):
        """The stream this plugin hears about started, or came back after a drop (the same ctrl then)."""
    def on_stream_stop(self):
        """The stream this plugin hears about ended (or stops to start again: host.is_restarting())."""
    def on_focus_gained(self, stream_url: str, ctrl):
        """The plugin moved onto a stream that was already running (a tile was clicked, the shown stream stopped
        with others carrying on, or the mic was moved). By default it's treated like on_stream_start."""
        self.on_stream_start(stream_url, ctrl)
    def on_focus_lost(self):
        """The plugin moved off a stream that carries on without it. By default it's treated like on_stream_stop."""
        self.on_stream_stop()
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
    def on_plugins_ready(self):
        """Every plugin is set up and the saved settings are in: emit again any state other plugins follow."""
    def shutdown(self):
        """App is quitting: stop background services the plugin started."""


class EventBus(QObject):
    stream_started         = pyqtSignal(str)
    stream_stopped         = pyqtSignal()
    stream_start_failed    = pyqtSignal()
    """A Start ended without a stream (the phone couldn't be reached or woken); no stream_started follows."""
    stream_transport_changed = pyqtSignal()
    """The shown stream's settings changed which video transport it can use; Connection re-evaluates its route."""
    stream_connected       = pyqtSignal(str)
    """A stream's frames are coming in (its source id; "" for no phone picked), the first time or back after a drop.
    Every stream, shown or not, and again for the one the panels move onto: check focused_source_id() for the shown one."""
    stream_lost            = pyqtSignal(str)
    """A stream's frames stopped mid-stream (its source id); the host is looking for a route back (stream_connected
    ends it). Every stream, and again for the one the panels move onto, as stream_connected."""
    phone_state_updated    = pyqtSignal(dict)
    device_changed         = pyqtSignal(str)
    phones_changed         = pyqtSignal(int)
    """Number of paired phones, emitted whenever the list changes."""
    add_phone_requested    = pyqtSignal()
    setup_guide_requested  = pyqtSignal()
    """Open the first-run checklist again, phone steps included, or close it if that's how it opened (Connection's ?)."""
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
    view_changed           = pyqtSignal(object)
    """The shown stream's view (telescope.view.View) changed, manually or by whatever took it."""
    view_taken             = pyqtSignal(str)
    """Something took the view (what's moving it), or "" once it let go; the Transforms card greys out Zoom and Pan."""
    people_seen            = pyqtSignal(str, list)
    """Where the people are in a stream (its source id) as [telescope.vision.Person], in its frame before the view;
    only while something wants_people. Check focused_source_id() for the shown stream."""
    vision_state_changed   = pyqtSignal(str)
    """host.vision_state() changed (MediaPipe loading, ready, missing or failed)."""
    view_status            = pyqtSignal(object)
    """How the shown stream's zoom is shared with the phone now (telescope.view.ViewStatus), for the lens dot."""
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
    camera_opened          = pyqtSignal(str, int, int, int)
    """Any stream's virtual camera opened: its source id, slot (0 is Telescope's own), width and height. vcam_opened
    says the same for slot 0 alone."""
    camera_watched         = pyqtSignal(bool)
    """Whether an app is reading the virtual camera (Wait screen watches it)."""
    window_shown           = pyqtSignal(bool)
    """The main window came on screen, or went to the tray or was minimized."""
    phone_wanted           = pyqtSignal(bool, bool)
    """Something waits for the phone to be ready (Automatic streaming), so it's checked even with the window hidden
    (wanted, held). Held: it already started for this arrival, so only the phone going away matters and slow checks do."""
    camera_on_changed      = pyqtSignal(bool)
    """The camera went off (the stream is just the mic) or back on, or what the next start uses changed."""
    mic_changed            = pyqtSignal(bool, bool)
    """The microphone card was switched on or off, or muted (enabled, muted)."""
    stream_sources_changed = pyqtSignal(list)
    """[(id, name)] of every StreamSource plugins offer; the phone picker lists them after the phones."""
    source_selected        = pyqtSignal(str)
    """The picked StreamSource's id, or "" for a phone (or nothing): panels that only make sense for a phone hide."""
    idle_outputs           = pyqtSignal(list)
    """The extra virtual cameras (slot numbers) that are set up but nothing streams to; the wait screen shows on them.
    Comes before a stream opens one, so whatever holds it lets go first."""
    streams_changed        = pyqtSignal(int)
    """How many streams run at once now (more than one: Add camera); stream_started and stream_stopped only mark the
    first starting and the last stopping."""
    remembered_sources     = pyqtSignal(list)
    """[(id, name, detail)] of StreamSources a plugin remembers settings for (browsers); the phones list shows them."""
    forget_source_requested = pyqtSignal(str)
    """Remove was clicked on one of remembered_sources in the phones list: its plugin forgets it."""
    stream_behind          = pyqtSignal(str, bool)
    """A stream (its source id) fell behind its frame rate (the throughput readout went amber), or caught up again.
    Only on a change, and never for a dropped stream: that's stream_lost. A stream the panels leave counts as caught
    up; only the shown one is measured, so check focused_source_id() as for stream_connected."""
