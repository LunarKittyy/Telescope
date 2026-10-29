//! The phone's microphone into a virtual microphone (audio.py, denoise.py, platform/virtual_mic.py and the Microphone
//! plugin).

pub mod denoise;
pub mod jitter;
pub mod shape;
pub mod sink;
pub mod virtual_mic;
pub mod worker;

use telescope_core::audio::{Audio, AudioFeed, MicSettings, MicStream};

/// The virtual microphone on this computer.
#[derive(Default)]
pub struct VirtualMic;

impl Audio for VirtualMic {
    fn virtual_mic_ready(&self) -> Result<(), String> {
        Err("The virtual microphone isn't in this build yet".into())
    }

    fn start(&self, _feed: Box<dyn AudioFeed>, _settings: &MicSettings) -> Result<Box<dyn MicStream>, String> {
        Err("The virtual microphone isn't in this build yet".into())
    }
}
