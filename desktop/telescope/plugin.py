import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Callable, Optional, Protocol

import numpy as np
from PyQt6.QtCore import QObject, pyqtSignal
from PyQt6.QtWidgets import QWidget
from telescope.i18n import N_, tr

if TYPE_CHECKING:
    from telescope.colour import ColourStage
    from telescope.overlays import PreviewLayer, PreviewOverlays
    from telescope.sensors import SensorReading, SensorStatus
    from telescope.stills import StillCaps
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

PLUGIN_CATEGORIES = (
    ("camera", N_("Camera and picture")),
    ("preview", N_("Preview tools")),
    ("vision", N_("People and vision")),
    ("on_screen", N_("On screen")),
    ("sound", N_("Sound")),
    ("streaming", N_("Streaming and devices")),
    ("extras", N_("Extras")),
)
"""The groups of the Plugins tab, in order: (id, name). TelescopePlugin.category names one by id; an id that isn't
here goes under Extras."""


FRAME_STAGES = (
    ("view", "the window's zoom, pan, flip, rotation and stretch"),
    ("geometry", "moving the whole picture after the view (Desk mode)"),
    ("reading", "looking at the clean picture without changing it (QR codes)"),
    ("background", "what's behind you (Background, Chroma key)"),
    ("effects", "changing the picture (Fun pack), and where a plugin goes that doesn't say"),
    ("colour", "the colour pipeline: every grade baked into one LUT per stream"),
    ("privacy", "hiding things, after anything that could bring them back (Privacy blur)"),
    ("overlay", "drawn on top, never graded or blurred (Name tag)"),
    ("combine", "several streams put into one picture"),
    ("cover", "the wait screen in place of the picture"),
    ("final", "seeing what apps get and handing it back unchanged (Preview)"),
)
"""The stages a frame goes through, in order: (id, what belongs there). TelescopePlugin.frame_stage names one; plugins in
the same stage run in the order they're registered."""


def frame_stage_index(plugin) -> int:
    """Where plugin's process_frame runs among the others: its frame_stage's place in FRAME_STAGES."""
    ids = [sid for sid, _what in FRAME_STAGES]
    stage = "final" if getattr(plugin, "sees_final_frame", False) else getattr(plugin, "frame_stage", "effects")
    return ids.index(stage) if stage in ids else ids.index("effects")


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
    footer: Optional[QWidget] = None
    """A bar kept at the bottom of the tab under the scrolling page, always in reach. It shows and hides itself."""


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
    """The shown stream's view: zoom, pan, flip, rotation and stretch, done before the plugins see the frame."""

    def view(self) -> "View":
        """The shown stream's view as it is now (telescope.view.View)."""
        ...

    def view_aspect(self) -> float:
        """Width over height of the shown stream's picture as shown (after rotation), 0 before its first frame."""
        ...

    def set_view(self, **changes) -> None:
        """Change the manual view (zoom, pan_x, pan_y, flip_h, flip_v, rotation, stretch_x, stretch_y); the phone does
        its share of the zoom.
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

    def tracked_people(self, source_id: Optional[str] = None) -> tuple:
        """([telescope.people.Track], how many people are there) on source_id's stream, by default the shown one, as
        the last people_tracked said; ([], 0) before that. Needs want_people like people_seen."""
        ...

class HostPackets(Protocol):
    """The shown stream's H.264 before it's decoded, for effects that change the video itself (datamosh). Only a
    phone's Light stream has it: Heavy is a picture at a time, with no keyframes to work with."""

    def packets_problem(self) -> str:
        """Why the shown stream's packets can't be changed (Heavy, a browser), or "" when they can, or will once it
        streams; it can change with stream_transport_changed, source_selected and the stream starting."""
        ...

    def change_packets(self, owner: str, make_step: Optional[Callable[[], Callable[[bytes], list]]]) -> None:
        """While set, the shown stream's access units (Annex-B bytes, one frame each) go through a step from
        make_step(), on the stream's thread, before they're decoded: step(unit) returns the ones to decode in its place
        ([] drops it). Each stream it's put on gets a fresh step, starting with the stream's SPS and PPS and at a keyframe, and it moves with the panels.
        None takes it off, and the phone is asked for a keyframe so the picture recovers. Nothing extra runs while nobody has one."""
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


class HostPreview(Protocol):
    """Drawing on the preview without it reaching the virtual camera, and asking for points and areas there
    (telescope.overlays). Nothing shows and no pick starts while the Preview plugin is off."""

    def add_preview_layer(self, owner: str, layer: "PreviewLayer") -> None:
        """Draw layer (a telescope.overlays.PreviewLayer) on the preview until it's removed; the same owner replaces it."""
        ...

    def remove_preview_layer(self, owner: str) -> None:
        """Stop drawing owner's layer."""
        ...

    def refresh_preview(self) -> None:
        """Paint the layers again, for one that would draw something else now. Any thread."""
        ...

    def pick_on_preview(self, owner: str, kind: str, on_done: Callable, start=None, hint: str = "") -> bool:
        """Ask for a "point", a "rect" or a "quad" (four corners) on the preview: on_done gets a telescope.overlays.Pick,
        or None when it's cancelled (Esc, another pick, the stream stopping). A quad starts from start (four points in
        the whole picture, like Pick.full) and ends with Enter or finish_pick. hint replaces the line shown over the
        picture. False if there's no preview to pick on; a pick already under way is cancelled."""
        ...

    def finish_pick(self, owner: str) -> bool:
        """End owner's pick with what it has (a quad as it is now); False if it has nothing yet."""
        ...

    def cancel_pick(self, owner: str) -> None:
        """End owner's pick without points (its on_done gets None)."""
        ...

    def set_preview_warp(self, owner: str, matrix) -> None:
        """For a plugin that moves the whole picture after the view (Desk mode): matrix is a 3 x 3 homography from the
        frame as shown with the view to what comes out, both 0..1, or None once it stops. Clicks, scrolls and picks on
        the preview are mapped back through it, and the lens outlines and skeleton aren't drawn meanwhile. It lasts
        until the plugin sends None, streams stopping and starting included. Any thread."""
        ...

    def preview_overlays(self) -> "PreviewOverlays":
        """For whatever shows the preview: the layers and the pick, to draw and pass the mouse to."""
        ...


class HostColour(Protocol):
    """The colour pipeline: every plugin's colour changes baked into one LUT per stream (telescope.colour), applied
    after the plugins' frame steps and before the plugins that see the final frame."""

    def set_colour_stage(self, owner: str, stage: Optional["ColourStage"], order: int = 0,
                         source_id: Optional[str] = None) -> None:
        """Put owner's telescope.colour.ColourStage in the pipeline, in place of its last one; None takes it out. Lower
        order runs first (a grade is 0, so evening phones out would go below it). source_id: only that stream, shown
        or not. Without it the stage is the shown stream's, and a stream the panels leave keeps the one it had."""
        ...

    def colour_split(self, owner: str, position: Optional[float]) -> None:
        """Show the shown stream without its colour stages left of position (0..1 of the width), on the preview only;
        None ends it."""
        ...

    def colour_preview(self, frame: np.ndarray) -> np.ndarray:
        """From the process_frame of a plugin that sees the final frame: frame (or a smaller copy of it) as the preview
        shows it, with a colour_split drawn in. A new array when it draws anything; frame itself is never changed."""
        ...


class HostMotion(Protocol):
    """Something moving in parts of the shown stream's picture (telescope.motion): no download, plain OpenCV."""

    def watch_motion(self, owner: str, zones: list, sensitivity: int = 5) -> None:
        """Look for motion in zones, [(x0, y0, x1, y1)] 0..1 in the picture as shown at zoom 1 (where Person's points
        are), at a sensitivity of 1 to 10; motion_seen says when it starts and stops. Empty zones stop it."""
        ...

    def motion(self, owner: str) -> list:
        """The indexes of owner's zones something moved in lately on the shown stream."""
        ...


class HostStills(Protocol):
    """Full-quality photos taken on the phone (telescope.stills), not frames of the video: for the Photos card, or any
    plugin wanting a burst, without depending on the Photos plugin."""

    def still_caps(self, source_id: Optional[str] = None) -> Optional["StillCaps"]:
        """What the lens the shown stream (or source_id's) is on can take, or None when it can't take photos (an
        older phone, a Browser camera, not streaming)."""
        ...

    def take_still(self, options: dict, folder: str, on_done: Callable, on_progress: Optional[Callable] = None,
                   source_id: Optional[str] = None) -> bool:
        """Take a photo on the shown stream's phone (or source_id's) with the /v1/still options in docs/protocol.md
        and save it in folder. on_done(StillResult) and on_progress(StillProgress) come on the GUI thread. False when
        it can't start: no photos there, or one is already being taken."""
        ...

    def still_busy(self, source_id: Optional[str] = None) -> bool:
        """Whether a photo is being taken on the shown stream's phone (or source_id's)."""
        ...


class HostSensors(Protocol):
    """The shown stream's phone's motion and light sensors (telescope.sensors): only streamed while something wants
    them, and only from the phone the panels show."""

    def want_sensors(self, owner: str, kinds) -> None:
        """Ask for these kinds (telescope.sensors.KINDS) under owner's name, replacing what owner asked for before; an
        empty list lets go. The phone streams what anyone wants, and stops once nobody does."""
        ...

    def sensor_status(self, source_id: Optional[str] = None) -> "SensorStatus":
        """Where the sensors of source_id's stream (default: the shown one) stand: off, starting, on or unavailable
        with the reason. Another stream's are always off. sensors_changed says when it changes."""
        ...

    def sensor_reading(self, source_id: Optional[str] = None) -> Optional["SensorReading"]:
        """The newest readings from source_id's phone (default: the shown stream's), None while there are none. Safe
        from any thread."""
        ...


class HostServices(HostSettings, HostStreams, HostCamera, HostSources, HostShell, HostView, HostVision, HostFrames,
                   HostMotion, HostPreview, HostStills, HostColour, HostSensors, HostPackets,
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
    category: str = "extras"
    """Which group of the Plugins tab it's listed in: an id from PLUGIN_CATEGORIES."""
    icon: str = "plug"
    """Its icon on the Plugins tab: a name from widgets/line_icons.py or the icons in widgets/common.py."""

    in_presets: bool = False
    """Presets save its get_config() and bring it back through apply_preset(), in the order plugins are registered."""

    device_local: bool = False
    """Each phone (or other source) gets its own copy of this plugin's settings, swapped in when it's picked or the
    panels move to its stream; False: one copy for the whole app."""

    frame_stage: str = "effects"
    """Where its process_frame runs: an id from FRAME_STAGES. The stages keep the frame order from depending on the
    order plugins are registered in or shown."""
    sees_final_frame: bool = False
    """The same as frame_stage = "final": its process_frame runs after every other plugin's, so it sees the frame the
    virtual camera gets (the preview). It has to hand the frame back unchanged."""
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
    meter_point_picked     = pyqtSignal(float, float)
    """A point to meter exposure on and leave focus alone, 0..1 in the frame as shown, for a lens that can meter on a
    point but not focus on one."""
    meter_point            = pyqtSignal(float, float)
    """The same point, 0..1 in the phone's own frame (transforms undone); camera control sends it."""
    meter_point_available  = pyqtSignal(bool)
    """Whether the current lens can meter on a point (and a stream is running to send it to); said right after
    focus_point_available."""
    focus_mode_picked      = pyqtSignal(str)
    """A focus mode was picked by hand on the Camera card: "auto", "point" or "manual"."""
    focus_mode_changed     = pyqtSignal(str)
    """The Camera card's focus mode is now "auto", "point" or "manual", however it changed (a pick, a preset, the
    phone's state); also once every plugin is ready."""
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
    people_tracked         = pyqtSignal(str, list, int)
    """people_seen made steady (telescope.people), right after it: the stream's source id, [Track] and how many people
    are there. Someone new counts after two updates in a row and someone gone only after a few seconds."""
    motion_seen            = pyqtSignal(str, str, list)
    """Motion in a watch_motion owner's zones started or stopped: the stream's source id, the owner and the indexes of
    the zones something moves in now."""
    vision_state_changed   = pyqtSignal(str)
    """host.vision_state() changed (MediaPipe loading, ready, missing or failed; off again after the download)."""
    sensors_changed        = pyqtSignal(str)
    """A stream's phone sensors (its source id) sent new readings, up to 30 times a second, or host.sensor_status()
    changed. Only while something wants them."""
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
    cover_requested        = pyqtSignal(str, bool)
    """Something asks for the wait screen in place of the shown stream's picture, or lets go (who's asking, on).
    Wait screen answers with stream_covered; without it nothing answers."""
    stream_covered         = pyqtSignal(bool)
    """Wait screen shows in place of the shown stream's picture for a cover_requested, or stopped."""
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
    preview_pick_changed   = pyqtSignal(str)
    """A pick on the preview started (who asked: host.pick_on_preview's owner) or ended ("")."""
    still_busy_changed     = pyqtSignal(str, bool)
    """A photo started or finished on a stream's phone (its source id), whichever plugin asked for it."""
    still_saved            = pyqtSignal(str, list)
    """Photos were saved from a stream's phone (its source id, [file paths]), whichever plugin asked for them."""
    stream_behind          = pyqtSignal(str, bool)
    """A stream (its source id) fell behind its frame rate (the throughput readout went amber), or caught up again.
    Only on a change, and never for a dropped stream: that's stream_lost. A stream the panels leave counts as caught
    up; only the shown one is measured, so check focused_source_id() as for stream_connected."""
