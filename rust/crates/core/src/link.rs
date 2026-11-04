//! Reaching phones: pairing, finding a route and the session on it. The link crate implements these.

use crate::media::{Codec, VideoFeed};
use crate::source::{ControlClient, PairedPhone, RouteSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which routes a start may try (Connection's route setting: auto, usb or wifi).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutePrefs {
    pub usb: bool,
    pub network: bool,
}

impl Default for RoutePrefs {
    fn default() -> Self {
        RoutePrefs { usb: true, network: true }
    }
}

/// Why no route was found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "why", content = "detail", rename_all = "snake_case")]
pub enum Unreachable {
    /// The phone answered but doesn't know this computer any more.
    NotPaired,
    /// The phone only takes connections over USB right now.
    LocalOnly,
    NoAnswer,
    /// One side is too old for the other; says which to update.
    Outdated(String),
}

/// The size and rate a stream asks the phone for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRequest {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The phone opens with its camera off and the mic alone; an older phone ignores this and opens the camera.
    #[serde(default)]
    pub camera_off: bool,
}

/// One phone reached over one route: starting its camera, its video and its controls.
pub trait PhoneSession: ControlClient {
    fn route(&self) -> RouteSpec;
    /// For people: "USB (serial)", "Wi-Fi (192.168.1.20)".
    fn describe(&self) -> String;
    /// Starts the phone's camera and waits until it streams.
    fn start(&self, request: StreamRequest) -> Result<(), String>;
    fn stop(&self) -> Result<(), String>;
    fn open_video(&self) -> Result<(Codec, Box<dyn VideoFeed>), String>;
    /// open_video, which `open.stop()` ends while it's still connecting; a session that can't be stopped there just opens.
    fn open_video_stoppable(&self, _open: &crate::media::OpenStop) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        self.open_video()
    }
    /// Lets go of what the route holds (adb forwards); the session isn't used after.
    fn release(&self);
    /// The video URL it opens ("https://127.0.0.1:40001/v1/video.h264"), which says the route and format.
    fn video_url(&self) -> String {
        String::new()
    }
    /// Where its /v1/state, /v1/control and /v1/sensors are ("https://host:port/v1").
    fn stream_base(&self) -> String {
        String::new()
    }
    /// Its camera controls; a hand-over passes them on to the next route's session.
    fn controls(&self) -> Option<std::sync::Arc<dyn PhoneControl>> {
        None
    }
    /// The phone's microphone (/v1/audio: 48 kHz mono s16le) on this route. The error is the text to show next to the
    /// Microphone card (the phone's own reason when it gives one).
    fn open_audio(&self) -> Result<Box<dyn crate::audio::AudioFeed>, String> {
        Err("not in this build".into())
    }
}

/// A pairing offer while it's open: the QR code's contents, and the phone once one pairs.
pub trait PairingOffer: Send + Sync {
    /// What the QR code holds.
    fn payload(&self) -> String;
    fn port(&self) -> u16;
    /// This computer's addresses the offer lists.
    fn addresses(&self) -> Vec<String>;
    /// Sends the offer to every phone on a USB cable that has the app with this package; how many took it.
    fn offer_over_usb(&self, package: &str) -> usize;
    /// The phone that paired, once one has.
    fn poll(&self) -> Option<PairedPhone>;
    /// Sends the offer to one phone on a cable (the one picked in Add device), again each time it's called, since the
    /// broadcast only lands while the app is on screen. Blocking.
    fn offer_to(&self, _serial: &str, _package: &str) -> bool {
        false
    }
    /// Takes the offer back now, whoever still holds it: its credentials stop working and nothing more is sent to a
    /// phone. Never waits for adb; a thread stuck in offer_to doesn't hold this up.
    fn revoke(&self) {}
}

pub trait Link: Send + Sync {
    /// A new computer id and this computer's name, for a settings file that has none yet (what phones know this computer by).
    fn new_identity(&self) -> (String, String);
    /// The way to this phone right now: USB first when it answers over a cable, else the network.
    fn resolve(&self, phone: &PairedPhone, prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable>;
    /// Opens a pairing offer as this computer.
    fn pair(&self, computer_id: &str, computer_name: &str) -> Result<std::sync::Arc<dyn PairingOffer>, String>;

    /// How this phone can be reached right now, and why not USB (RouteResolver.resolve). Blocking.
    fn check(&self, _phone: &PairedPhone, _preference: RoutePreference) -> Resolution {
        Resolution::new(PhoneStatus::Unreachable)
    }
    /// A session on a route check found, holding its adb forward until released. video_path is the format's
    /// ("/v1/video", "/v1/video.h264" or "/v1/video.udp"); controls carries a running stream's over (a hand-over).
    fn open_route(&self, _phone: &PairedPhone, _route: &RouteSpec, _video_path: &str,
                  _controls: Option<std::sync::Arc<dyn PhoneControl>>) -> Result<Box<dyn PhoneSession>, String> {
        Err("not in this build".into())
    }
    /// Starts the phone's camera unless it streams already and waits until it does; progress gets texts to show.
    /// after_stop: this computer's stop just reached the phone. The error is the text to show. Blocking.
    fn ensure_streaming(&self, _phone: &PairedPhone, _route: &RouteSpec, _opening: Option<StreamRequest>,
                        _after_stop: bool, _progress: &dyn Fn(&str)) -> Result<(), String> {
        Err("not in this build".into())
    }
    /// Tells the phone to stop its camera (best effort). Blocking.
    fn stop_streaming(&self, _phone: &PairedPhone, _route: &RouteSpec) {}
    /// Asks a reachable phone to forget this computer (best effort, after it's removed). Blocking.
    fn unpair(&self, _phone: &PairedPhone) {}
    fn adb_available(&self) -> bool {
        false
    }
    /// Everything adb sees on a cable, with model names. Blocking.
    fn usb_devices(&self) -> Vec<UsbDevice> {
        Vec::new()
    }
    /// The phone app this build ships with, to install over USB.
    fn bundled_apk(&self) -> Option<String> {
        None
    }
    /// `adb install -r` onto one phone; the error is what to tell the user. Blocking.
    fn install_app(&self, _serial: &str, _apk: &str) -> Result<(), String> {
        Err("not in this build".into())
    }
    /// Windows: fetches adb from Google, or with own (an adb.exe or platform-tools zip) keeps that one instead; the version
    /// it got, or what to tell the user. Blocking, with progress texts.
    fn get_adb(&self, _own: Option<&str>, _progress: &dyn Fn(&str)) -> Result<String, String> {
        Err("Not in this build yet".into())
    }
    fn sensors(&self) -> Option<std::sync::Arc<dyn PhoneSensors>> {
        None
    }
    /// Full-quality photos from the phones (stills.py), for the window's HostStills.
    fn stills(&self) -> Option<std::sync::Arc<dyn PhoneStills>> {
        None
    }
    /// At quit: stops LAN discovery and the adb server this app started.
    fn shutdown(&self) {}

    // The stream flow's side of the Connection plugin (get_stream_info and the route methods in connection.py): the
    // window calls these, and the Connection plugin, which shares their state inside the link crate, shows what
    // comes of them (the card, and the Start banner with its fixes). source_id None is the picked phone.

    /// Checks how the phone can be reached and holds that route for its stream (a USB forward); where to open its
    /// video. Err is a reason for the log: the Connection plugin shows the banner. Blocking.
    fn start_route(&self, _source_id: Option<&str>, _format: StreamFormat) -> Result<StreamTarget, String> {
        Err("not in this build".into())
    }
    /// The phone and the route its stream holds, for ensure_streaming and stop_streaming (session_target).
    fn session_target(&self, _source_id: Option<&str>) -> Option<(PairedPhone, RouteSpec)> {
        None
    }
    /// The stream is over: let go of its route and forward (release_stream).
    fn release_stream(&self, _source_id: &str) {}
    /// The video URL for the stream on route while it still runs on its old one (a hand-over), or None if it's there
    /// already or can't. Blocking (a USB forward).
    fn prepare_route(&self, _source_id: &str, _route: &RouteSpec) -> Option<String> {
        None
    }
    /// The hand-over worked: the prepared route is the stream's now.
    fn commit_route(&self, _source_id: &str) {}
    /// The hand-over didn't happen; failed: that route (USB) sits out a while.
    fn drop_route(&self, _source_id: &str, _failed: bool) {}
    /// How a dropped stream's phone can be reached right now, with the Connect via choice (recovery_probe). Blocking.
    fn recovery_check(&self, _source_id: Option<&str>) -> Resolution {
        Resolution::new(PhoneStatus::Unreachable)
    }
    /// Moves the running stream onto a route recovery_check found; the new video URL, or None. Blocking.
    fn adopt_route(&self, _source_id: Option<&str>, _route: &RouteSpec) -> Option<String> {
        None
    }
    /// A dropped stream won't come back: the Connection plugin says why, as a failed Start would (show_problem).
    fn show_problem(&self, _source_id: Option<&str>, _res: &Resolution) {}
    /// The paired phones, and which source the picker shows (selected_device), as the Connection plugin has them.
    fn phones(&self) -> Vec<PairedPhone> {
        Vec::new()
    }
    fn picked_source(&self) -> Option<String> {
        None
    }
}

/// What the Stream output card picked, as far as the video path goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamFormat {
    /// Light (H.264, and this computer decodes it) rather than Heavy (MJPEG).
    pub h264: bool,
    /// Dynamic bitrate (bitrate_mbps -1).
    pub dynamic: bool,
}

/// Where a phone's stream goes: the phone, the route it holds and the video URL to open (get_stream_info).
#[derive(Clone, Debug, PartialEq)]
pub struct StreamTarget {
    pub phone: PairedPhone,
    pub route: RouteSpec,
    pub video_url: String,
}

/// Connection's "Connect via" setting, as saved ("auto", "usb", "wifi").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutePreference {
    #[default]
    Auto,
    Usb,
    Wifi,
}

impl RoutePreference {
    pub fn as_str(self) -> &'static str {
        match self {
            RoutePreference::Auto => "auto",
            RoutePreference::Usb => "usb",
            RoutePreference::Wifi => "wifi",
        }
    }

    /// The saved value, or None for anything else (which Connection reads as Auto).
    pub fn parse(value: &str) -> Option<RoutePreference> {
        match value {
            "auto" => Some(RoutePreference::Auto),
            "usb" => Some(RoutePreference::Usb),
            "wifi" => Some(RoutePreference::Wifi),
            _ => None,
        }
    }
}

/// How a phone is doing right now (phones.py's statuses, with the same names).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PhoneStatus {
    #[serde(rename = "ready")]
    Ready,
    /// The app isn't open, or the phone isn't on this network.
    #[serde(rename = "unreachable")]
    Unreachable,
    /// The phone answered but no longer accepts this computer.
    #[serde(rename = "not_paired")]
    NotPaired,
    /// Found over Wi-Fi, but the phone only allows USB.
    #[serde(rename = "local_only")]
    LocalOnly,
    /// USB forced and not usable; the usb note says why.
    #[serde(rename = "usb_attention")]
    UsbNeedsAttention,
    #[serde(rename = "phone_outdated")]
    PhoneOutdated,
    #[serde(rename = "desktop_outdated")]
    DesktopOutdated,
}

/// Why USB wasn't used, or failed when it was forced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbNote {
    /// adb isn't installed.
    NoAdb,
    /// adb sees no device at all.
    NoCable,
    /// A device is attached but hasn't accepted USB debugging.
    Unauthorized,
    /// A device answered, but it's a different phone.
    OtherPhone,
    /// A device is attached but Telescope isn't answering on it.
    AppClosed,
}

/// What a check of one phone found (phones.Resolution).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub status: PhoneStatus,
    pub route: Option<RouteSpec>,
    /// Set whenever the route isn't USB (or USB failed).
    pub usb_note: Option<UsbNote>,
    pub streaming: bool,
    pub busy: bool,
    /// The other paired computer the phone's stream belongs to, if any.
    pub streaming_for: String,
    pub phone_version: String,
    /// The phone runs Dynamic over UDP from this desktop's reports.
    pub udp_dynamic: bool,
    /// NOT_PAIRED because the pairing predates TLS, not because the phone dropped it.
    pub before_tls: bool,
}

impl Resolution {
    pub fn new(status: PhoneStatus) -> Resolution {
        Resolution { status, route: None, usb_note: None, streaming: false, busy: false, streaming_for: String::new(),
                     phone_version: String::new(), udp_dynamic: false, before_tls: false }
    }
}

/// Something adb sees on a cable: state is "device" once USB debugging is allowed, else "unauthorized", "offline", ...
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbDevice {
    pub serial: String,
    pub state: String,
    /// The phone's model name, or "" when adb can't say.
    pub model: String,
}

/// A stream's camera controls (PhoneControlClient): queued, coalesced by action and sent again when lost. Kept across
/// a hand-over, so what's queued and the settings it resends follow the stream onto its new route.
pub trait PhoneControl: Send + Sync {
    /// One /v1/control body, e.g. {"action": "torch", "value": 1}.
    fn send(&self, body: &Value);
    /// The phone's /v1/state, or None when it didn't answer with one; sends again any setting it doesn't show.
    fn get_state(&self) -> Option<Value>;
    /// The same stream on a new route: its stream base ("https://host:port/v1").
    fn retarget(&self, stream_base: &str);
    /// Sends the phone every setting again, for a stream that came back.
    fn resend_settings(&self);
    fn close(&self);
    fn closed(&self) -> bool;
}

/// The shown stream's phone sensors (telescope.sensors.SensorHub): who wants which kinds, and the stream it follows.
pub trait PhoneSensors: Send + Sync {
    fn want(&self, owner: &str, kinds: &[String]);
    fn wanted(&self) -> Vec<String>;
    /// {"state", "kinds", "streaming", "reason"} for source_id (None: the stream followed).
    fn status(&self, source_id: Option<&str>) -> Value;
    /// The newest reading as SensorReading's fields, or None.
    fn reading(&self, source_id: Option<&str>) -> Option<Value>;
    /// The panels show source_id's stream (None: none), reached at stream_base with phone's pairing; phone is None
    /// for a source that isn't a phone. state: its /v1/state when one just came in ({} when it didn't answer).
    fn follow(&self, source_id: Option<&str>, stream_base: Option<&str>, phone: Option<&PairedPhone>, state: Option<&Value>);
    fn diagnostics(&self) -> Value;
    fn shutdown(&self);
    /// Called with a source id, from any thread, when its readings or status change (sensors_changed on the bus).
    fn set_on_change(&self, _on_change: Box<dyn Fn(&str) + Send + Sync>) {}
}

/// Full-quality photos on a stream's phone (stills.Stills), one at a time per phone. Progress and results are plain
/// data: progress {"state", "shots", "taken", "countdown_ms", "paused", "saved"}, result {"ok", "files", "code",
/// "error"} with error in this app's language.
pub trait PhoneStills: Send + Sync {
    /// A phone's /v1/state; only a whole one (with its cameras) says what its lenses can take.
    fn note_state(&self, source_id: Option<&str>, state: &Value);
    /// The stream moved to another lens (its /v1/state camera entry).
    fn note_lens(&self, source_id: Option<&str>, camera: &Value);
    /// What the current lens can take ({"formats", "size", "max_res", ...}), or None when it can't take photos.
    fn caps(&self, source_id: Option<&str>) -> Option<Value>;
    fn busy(&self, source_id: Option<&str>) -> bool;
    /// Starts a photo with the /v1/still options into folder; false while one is being taken there. alive() turning
    /// false (the stream stopped or restarted) ends it as interrupted. The callbacks come from its own thread.
    #[allow(clippy::too_many_arguments)]
    fn take(&self, source_id: Option<&str>, stream_base: &str, phone: &PairedPhone, options: &Value, folder: &str,
            on_progress: Box<dyn Fn(Value) + Send>, on_done: Box<dyn FnOnce(Value) + Send>,
            alive: Box<dyn Fn() -> bool + Send>) -> bool;
    /// At quit: every photo ends, and its phone is told to forget it.
    fn shutdown(&self);
}
