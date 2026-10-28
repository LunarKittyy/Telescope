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
    fn sensors(&self) -> Option<std::sync::Arc<dyn PhoneSensors>> {
        None
    }
    /// At quit: stops LAN discovery and the adb server this app started.
    fn shutdown(&self) {}
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
}
