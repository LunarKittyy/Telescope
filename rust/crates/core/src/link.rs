//! Reaching phones: pairing, finding a route and the session on it. The link crate implements these.

use crate::media::{Codec, VideoFeed};
use crate::source::{ControlClient, PairedPhone, RouteSpec};
use serde::{Deserialize, Serialize};

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
}
