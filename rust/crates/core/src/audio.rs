//! The phone's microphone into a virtual microphone. The audio crate implements these.

use serde::{Deserialize, Serialize};

/// Audio as it arrives from the phone, before decoding.
#[derive(Clone, Debug)]
pub struct AudioPacket {
    pub data: Vec<u8>,
    pub time_ns: u64,
}

/// What the microphone card asks for (the Microphone plugin's settings that change the sound itself).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MicSettings {
    pub muted: bool,
    pub gain_db: f32,
    pub limiter: bool,
    pub noise_reduction: bool,
    pub auto_level: bool,
}

/// Levels for the card's meter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MicLevel {
    pub peak: f32,
    pub rms: f32,
}

/// A running microphone stream from one phone to the virtual microphone.
pub trait MicStream: Send {
    fn set_settings(&mut self, settings: &MicSettings);
    fn level(&self) -> MicLevel;
    fn stop(self: Box<Self>);
}

/// Encoded audio from a phone, read on the audio thread.
pub trait AudioFeed: Send {
    fn read(&mut self) -> Result<AudioPacket, crate::media::FeedEnd>;
}

pub trait Audio: Send + Sync {
    /// Whether the virtual microphone is set up on this computer, and if not, why.
    fn virtual_mic_ready(&self) -> Result<(), String>;
    fn start(&self, feed: Box<dyn AudioFeed>, settings: &MicSettings) -> Result<Box<dyn MicStream>, String>;
}
