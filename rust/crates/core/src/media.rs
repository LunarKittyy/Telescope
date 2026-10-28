//! Between link and media: encoded video coming in, decoded frames going out. The media crate implements Decoders.

use crate::frame::Frame;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Codec {
    /// Annex-B H.264, in chunks that needn't line up with access units.
    H264,
    /// One JPEG per packet (Heavy streams and the Browser camera).
    Mjpeg,
}

/// A piece of encoded video as it arrived.
#[derive(Clone, Debug)]
pub struct Packet {
    pub data: Vec<u8>,
    /// When it arrived, monotonic nanoseconds.
    pub time_ns: u64,
}

/// Why a feed ended.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedEnd {
    /// Stopped from this side (FeedStopper).
    Stopped,
    /// The other side closed it.
    Closed,
    Error(String),
}

/// Ends a feed's blocked read from another thread.
pub trait FeedStopper: Send + Sync {
    fn stop(&self);
}

/// Encoded video from a phone or another source, read on the stream's thread.
pub trait VideoFeed: Send {
    fn read(&mut self) -> Result<Packet, FeedEnd>;
    fn stopper(&self) -> Box<dyn FeedStopper>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecodeOptions {
    /// Decode on the graphics card when it can (VA-API, D3D11VA); software otherwise.
    pub hardware: bool,
}

/// What one packet decoded to: the newest finished frame (older ones are dropped to keep latency down) and how many finished.
#[derive(Debug, Default)]
pub struct Decoded {
    pub newest: Option<Frame>,
    pub count: u32,
}

pub trait Decoder: Send {
    fn decode(&mut self, packet: &Packet) -> Decoded;
    /// Whether it really ended up on the graphics card.
    fn hardware(&self) -> bool;
}

/// Turns a frame into RGB at another size, for the preview.
pub trait Scaler: Send {
    /// RGB24, width * height * 3 bytes.
    fn rgb(&mut self, frame: &Frame, width: u32, height: u32) -> Vec<u8>;
}

/// What the media crate offers the rest of the app.
pub trait Media: Send + Sync {
    fn decoder(&self, codec: Codec, options: &DecodeOptions) -> Result<Box<dyn Decoder>, String>;
    fn scaler(&self) -> Box<dyn Scaler>;
}
