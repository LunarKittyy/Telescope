//! Stand-ins for what Setup is built with, when a test only looks at its settings or its UI.

use std::sync::Arc;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::link::{Link, PairingOffer, PhoneSession, RoutePrefs, Unreachable};
use telescope_core::media::{Codec, DecodeOptions, Decoder, Media, Scaler};
use telescope_core::output::{Outputs, Slot, VideoOutput};
use telescope_core::source::PairedPhone;
use telescope_setup::setup::SetupPlugin;

struct NoOutputs;

impl Outputs for NoOutputs {
    fn slots(&self) -> Vec<Slot> {
        Vec::new()
    }
    fn formats(&self) -> Vec<PixelFormat> {
        Vec::new()
    }
    fn open(&self, _slot: u32, _w: u32, _h: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no".into())
    }
}

struct NoLink;

impl Link for NoLink {
    fn new_identity(&self) -> (String, String) {
        (String::new(), String::new())
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        Err(Unreachable::NoAnswer)
    }
    fn pair(&self, _id: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        Err("no".into())
    }
}

struct NoScaler;

impl Scaler for NoScaler {
    fn rgb(&mut self, _frame: &Frame, _w: u32, _h: u32) -> Vec<u8> {
        Vec::new()
    }
}

struct NoMedia;

impl Media for NoMedia {
    fn decoder(&self, _codec: Codec, _options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
        Err("no".into())
    }
    fn scaler(&self) -> Box<dyn Scaler> {
        Box::new(NoScaler)
    }
}

pub fn setup_plugin() -> SetupPlugin {
    SetupPlugin::new(Arc::new(NoOutputs), Arc::new(NoLink), Arc::new(NoMedia), "3.0.0", Vec::new())
}
