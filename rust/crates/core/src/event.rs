//! Everything on the event bus: one variant per EventBus signal of the Python app's plugin.py, same name, same meaning.

use crate::host::{Pick, RequestId, View};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A longer lens's view in the frame as shown: (label, x0, y0, x1, y1), 0..1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LensBox {
    pub label: String,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// A StreamSource as the phone picker lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceName {
    pub id: String,
    pub name: String,
}

/// A source a plugin remembers settings for (browsers), as the phones list shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RememberedSource {
    pub id: String,
    pub name: String,
    pub detail: String,
}

macro_rules! events {
    ($( $(#[doc = $doc:literal])* $name:literal => $variant:ident $({ $($field:ident : $ty:ty),* $(,)? })? ),* $(,)?) => {
        /// One event. Serialised as {"event": "<signal name>", ...fields}, which is what a Lua or WASM plugin sees.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "event")]
        #[non_exhaustive]
        pub enum Event {
            $( $(#[doc = $doc])* #[serde(rename = $name)] $variant $({ $($field: $ty),* })? ),*
        }

        impl Event {
            /// The signal's name in plugin.py, which is also its "event" field.
            pub fn name(&self) -> &'static str {
                match self { $( Event::$variant { .. } => $name ),* }
            }

            /// Every event with its fields, as (name, type) in the order they're declared. The plugin API freezes these.
            pub const FIELDS: &'static [(&'static str, &'static [(&'static str, &'static str)])] =
                &[$(($name, &[$($((stringify!($field), stringify!($ty))),*)?])),*];

            /// Every event name, in plugin.py's order.
            pub const NAMES: &'static [&'static str] = &[$($name),*];
        }
    };
}

events! {
    /// A stream started (its URL); only the first of several.
    "stream_started" => StreamStarted { url: String },
    /// The last stream stopped.
    "stream_stopped" => StreamStopped,
    /// A Start ended without a stream (the phone couldn't be reached or woken); no stream_started follows.
    "stream_start_failed" => StreamStartFailed,
    /// The shown stream's settings changed which video transport it can use; Connection re-evaluates its route.
    "stream_transport_changed" => StreamTransportChanged,
    /// A stream's frames are coming in (its source id; "" for no phone picked), the first time or back after a drop.
    "stream_connected" => StreamConnected { source_id: String },
    /// A stream's frames stopped mid-stream (its source id); the host is looking for a route back.
    "stream_lost" => StreamLost { source_id: String },
    /// The phone's /v1/state, as it last answered.
    "phone_state_updated" => PhoneStateUpdated { state: Value },
    /// The picked phone or source changed (its id, "" for none).
    "device_changed" => DeviceChanged { source_id: String },
    /// Number of paired phones, emitted whenever the list changes.
    "phones_changed" => PhonesChanged { count: u32 },
    "add_phone_requested" => AddPhoneRequested,
    /// Open the first-run checklist again, phone steps included, or close it if that's how it opened.
    "setup_guide_requested" => SetupGuideRequested,
    /// First-run checklist is showing (true) or done/hidden (false); the video stage makes room for it.
    "setup_needed" => SetupNeeded { showing: bool },
    /// Lens switch sent to phone; carries the selected camera's capabilities from /v1/state.
    "camera_switched" => CameraSwitched { camera: Value },
    /// Resolution change sent to phone; host shows pending state until confirmed.
    "resolution_change_requested" => ResolutionChangeRequested { width: u32, height: u32 },
    /// A point picked on the preview, 0..1 in the frame as shown (after transforms).
    "focus_point_picked" => FocusPointPicked { x: f32, y: f32 },
    /// The same point, 0..1 in the phone's own frame (transforms undone); camera control sends it.
    "focus_point" => FocusPoint { x: f32, y: f32 },
    /// Whether the current lens can focus on a point (and a stream is running to send it to).
    "focus_point_available" => FocusPointAvailable { available: bool },
    /// A point to meter exposure on and leave focus alone, 0..1 in the frame as shown.
    "meter_point_picked" => MeterPointPicked { x: f32, y: f32 },
    /// The same point, 0..1 in the phone's own frame (transforms undone); camera control sends it.
    "meter_point" => MeterPoint { x: f32, y: f32 },
    /// Whether the current lens can meter on a point; said right after focus_point_available.
    "meter_point_available" => MeterPointAvailable { available: bool },
    /// A focus mode was picked by hand on the Camera card: "auto", "point" or "manual".
    "focus_mode_picked" => FocusModePicked { mode: String },
    /// The Camera card's focus mode is now "auto", "point" or "manual", however it changed.
    "focus_mode_changed" => FocusModeChanged { mode: String },
    /// The selected phone's id and whether Start would work right now (idle status checks only).
    "phone_ready" => PhoneReady { phone_id: String, ready: bool },
    /// The preview was dragged by (du, dv), as fractions of the frame as shown; transforms pans.
    "view_dragged" => ViewDragged { du: f32, dv: f32 },
    /// Scrolled over the preview: a zoom factor and the (u, v) under the mouse; transforms zooms around it.
    "view_scrolled" => ViewScrolled { factor: f32, u: f32, v: f32 },
    /// Whether dragging the preview pans (zoomed in); the preview shows a hand cursor.
    "view_pannable" => ViewPannable { pannable: bool },
    /// What each longer lens sees in the frame as shown, and whether the user just moved the framing.
    "lens_boxes" => LensBoxes { boxes: Vec<LensBox>, moved: bool },
    /// The shown stream's view changed, manually or by whatever took it.
    "view_changed" => ViewChanged { view: View },
    /// Something took the view (what's moving it), or "" once it let go.
    "view_taken" => ViewTaken { owner: String },
    /// Where the people are in a stream as the vision service's Person records, in its frame before the view.
    "people_seen" => PeopleSeen { source_id: String, people: Vec<Value> },
    /// people_seen made steady, right after it: the stream's source id, its Track records and how many people are there.
    "people_tracked" => PeopleTracked { source_id: String, tracks: Vec<Value>, count: u32 },
    /// Motion in a watch_motion owner's zones started or stopped: the zones something moves in now.
    "motion_seen" => MotionSeen { source_id: String, owner: String, zones: Vec<u32> },
    /// host.vision_state() changed.
    "vision_state_changed" => VisionStateChanged { state: String },
    /// A stream's phone sensors sent new readings, or host.sensor_status() changed.
    "sensors_changed" => SensorsChanged { source_id: String },
    /// How the shown stream's zoom is shared with the phone now, for the lens dot.
    "view_status" => ViewStatus { status: Value },
    /// How far the Zoom slider goes (Advanced); Setup emits it on change and when its config loads.
    "max_zoom_changed" => MaxZoomChanged { max_zoom: i32 },
    /// How far the microphone's Gain slider goes, in dB (Advanced).
    "max_gain_changed" => MaxGainChanged { max_gain: i32 },
    /// Whether the microphone's limiter is on (Advanced).
    "limiter_changed" => LimiterChanged { on: bool },
    /// Show the desktop app's update dialog (e.g. the phone app turned out to be newer).
    "update_requested" => UpdateRequested,
    /// The stream opened the virtual camera at this width and height.
    "vcam_opened" => VcamOpened { width: u32, height: u32 },
    /// Any stream's virtual camera opened: its source id, slot (0 is Telescope's own), width and height.
    "camera_opened" => CameraOpened { source_id: String, slot: u32, width: u32, height: u32 },
    /// Whether an app is reading the virtual camera (Wait screen watches it).
    "camera_watched" => CameraWatched { watched: bool },
    /// Something asks for the wait screen in place of the shown stream's picture, or lets go.
    "cover_requested" => CoverRequested { owner: String, on: bool },
    /// Wait screen shows in place of the shown stream's picture for a cover_requested, or stopped.
    "stream_covered" => StreamCovered { covered: bool },
    /// The main window came on screen, or went to the tray or was minimized.
    "window_shown" => WindowShown { shown: bool },
    /// Something waits for the phone to be ready (Automatic streaming), so it's checked even with the window hidden.
    "phone_wanted" => PhoneWanted { wanted: bool, held: bool },
    /// The camera went off (the stream is just the mic) or back on, or what the next start uses changed.
    "camera_on_changed" => CameraOnChanged { on: bool },
    /// The microphone card was switched on or off, or muted.
    "mic_changed" => MicChanged { enabled: bool, muted: bool },
    /// Every StreamSource plugins offer; the phone picker lists them after the phones.
    "stream_sources_changed" => StreamSourcesChanged { sources: Vec<SourceName> },
    /// The picked StreamSource's id, or "" for a phone (or nothing).
    "source_selected" => SourceSelected { source_id: String },
    // The window wants this source picked, as if by hand: Connection picks it and loads its settings
    "select_source" => SelectSource { source_id: String },
    // The window moved the panels to this source and swapped the settings itself: Connection shows it as picked
    "source_shown" => SourceShown { source_id: String },
    /// The extra virtual cameras (slot numbers) that are set up but nothing streams to; comes before a stream opens one.
    "idle_outputs" => IdleOutputs { slots: Vec<u32> },
    /// How many streams run at once now.
    "streams_changed" => StreamsChanged { count: u32 },
    /// StreamSources a plugin remembers settings for (browsers); the phones list shows them.
    "remembered_sources" => RememberedSources { sources: Vec<RememberedSource> },
    /// Remove was clicked on one of remembered_sources in the phones list: its plugin forgets it.
    "forget_source_requested" => ForgetSourceRequested { source_id: String },
    /// A pick on the preview started (who asked) or ended ("").
    "preview_pick_changed" => PreviewPickChanged { owner: String },
    /// A photo started or finished on a stream's phone, whichever plugin asked for it.
    "still_busy_changed" => StillBusyChanged { source_id: String, busy: bool },
    /// Photos were saved from a stream's phone, whichever plugin asked for them.
    "still_saved" => StillSaved { source_id: String, paths: Vec<String> },
    /// A stream fell behind its frame rate, or caught up again; only the shown one is measured.
    "stream_behind" => StreamBehind { source_id: String, behind: bool },

    /// Not in plugin.py: a pick_on_preview ended, with the pick or None when cancelled (Python's on_done callback).
    "preview_picked" => PreviewPicked { owner: String, pick: Option<Pick> },
    /// Not in plugin.py: a host request that finishes later did (Python's on_done callbacks), with what it returned.
    "request_done" => RequestDone { request: RequestId, ok: bool, message: String, data: Value },
    /// Not in plugin.py: progress on a host request that's still going (take_still's on_progress).
    "request_progress" => RequestProgress { request: RequestId, data: Value },
    /// Not in plugin.py: a stream's /v1/state as it arrives, the streams behind the panels too (Monitoring reads their
    /// batteries the way Python's ctrl.get_state() did).
    "stream_state" => StreamState { source_id: String, state: Value },
    /// The answer to host.request_phone_state, to whoever polls and nobody else: the stream's session, and whether the panels show it.
    "phone_polled" => PhonePolled { source_id: String, session: u64, shown: bool, state: Value },
    /// Not in plugin.py: a timer from Host::schedule went off; only the plugin that scheduled it (owner) hears it.
    "timer_fired" => TimerFired { owner: String, token: String },
    /// Not in plugin.py: a Poster's post arrived on the host thread; only the plugin it belongs to (owner) hears it.
    "posted" => Posted { owner: String, token: String, data: Value },
    /// Not in plugin.py: the vision pack's download moved on (VisionDownload.progress): bytes so far, bytes in all (0: not known).
    "vision_download_progress" => VisionDownloadProgress { done: u64, size: u64 },
    /// Not in plugin.py: the vision pack's download ended (VisionDownload.finished): "" when it's ready, else what went wrong,
    /// and the optional plugin that waited for it (to switch on when it worked).
    "vision_download_finished" => VisionDownloadFinished { error: String, switch_on: String },
    /// Not in plugin.py: an event a plugin someone added defines for itself, named "<plugin>.<what>".
    "custom" => Custom { name: String, data: Value },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_are_plain_data() {
        let e = Event::CameraOpened { source_id: "p".into(), slot: 1, width: 1280, height: 720 };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v, json!({"event": "camera_opened", "source_id": "p", "slot": 1, "width": 1280, "height": 720}));
        assert_eq!(serde_json::from_value::<Event>(v).unwrap(), e);
        assert_eq!(serde_json::to_value(Event::StreamStopped).unwrap(), json!({"event": "stream_stopped"}));
        assert_eq!(Event::StreamStopped.name(), "stream_stopped");
    }

    // The EventBus signals of the Python app's plugin.py; each has an Event of the same name
    const PYTHON_SIGNALS: [&str; 58] = [
        "stream_started", "stream_stopped", "stream_start_failed", "stream_transport_changed", "stream_connected", "stream_lost",
        "phone_state_updated", "device_changed", "phones_changed", "add_phone_requested", "setup_guide_requested", "setup_needed",
        "camera_switched", "resolution_change_requested", "focus_point_picked", "focus_point", "focus_point_available", "meter_point_picked",
        "meter_point", "meter_point_available", "focus_mode_picked", "focus_mode_changed", "phone_ready", "view_dragged", "view_scrolled",
        "view_pannable", "lens_boxes", "view_changed", "view_taken", "people_seen", "people_tracked", "motion_seen", "vision_state_changed",
        "sensors_changed", "view_status", "max_zoom_changed", "max_gain_changed", "limiter_changed", "update_requested", "vcam_opened",
        "camera_opened", "camera_watched", "cover_requested", "stream_covered", "window_shown", "phone_wanted", "camera_on_changed",
        "mic_changed", "stream_sources_changed", "source_selected", "idle_outputs", "streams_changed", "remembered_sources",
        "forget_source_requested", "preview_pick_changed", "still_busy_changed", "still_saved", "stream_behind",
    ];

    #[test]
    fn every_python_signal_has_an_event() {
        assert!(PYTHON_SIGNALS.len() > 50);
        for signal in PYTHON_SIGNALS {
            assert!(Event::NAMES.contains(&signal), "the Python app's {signal} has no Event");
        }
    }

    #[test]
    fn timers_and_posts_are_plain_data() {
        let e = Event::Posted { owner: "connection".into(), token: "resolved".into(), data: json!({"check_id": 3}) };
        assert_eq!(serde_json::to_value(&e).unwrap(),
                   json!({"event": "posted", "owner": "connection", "token": "resolved", "data": {"check_id": 3}}));
        let t = Event::TimerFired { owner: "connection".into(), token: "poll".into() };
        assert_eq!(serde_json::from_value::<Event>(serde_json::to_value(&t).unwrap()).unwrap(), t);
    }

    #[test]
    fn names_are_unique() {
        let mut names = Event::NAMES.to_vec();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), Event::NAMES.len());
    }
}
