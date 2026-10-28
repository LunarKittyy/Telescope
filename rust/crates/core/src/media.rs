//! Between link and media: encoded video coming in, decoded frames going out. The media crate implements Decoders.

use crate::frame::{Frame, FrameContext, FrameStage, FrameStep};
use crate::host::{FrameStore, PacketStepFactory};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

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

    /// A decoder whose H.264 goes through tap's packet steps first (StreamWorker.packets); other codecs ignore the tap.
    fn decoder_with_tap(&self, codec: Codec, options: &DecodeOptions, tap: Arc<dyn PacketTap>) -> Result<Box<dyn Decoder>, String> {
        let _ = tap;
        self.decoder(codec, options)
    }
    /// Reads frames off a feed through a decoder (H264Reader.read): ends data that never makes a frame and counts what's behind each one.
    fn frame_reader(&self, feed: Box<dyn VideoFeed>, decoder: Box<dyn Decoder>) -> Result<Box<dyn FrameReader>, String> {
        let _ = (feed, decoder);
        Err("frame readers aren't in this build".into())
    }
    /// A stream's packet steps, empty until set.
    fn packet_tap(&self) -> Option<Arc<dyn PacketTap>> {
        None
    }
    /// Runs a stream's frame steps; one per stream.
    fn pipeline(&self) -> Result<Box<dyn FramePipeline>, String> {
        Err("frame steps aren't in this build".into())
    }
    /// Every running stream's newest frame (frames.Frames); one for the whole app.
    fn stream_frames(&self) -> Option<Arc<dyn StreamFrames>> {
        None
    }
    /// What decodes now and what the next decoder may use, for Advanced and Copy diagnostics.
    fn decoding_status(&self) -> DecodingStatus {
        DecodingStatus::default()
    }
}

/// A frame read off a feed, with the wire bytes behind it and how many frames arrived for it (the ones skipped to keep up too).
#[derive(Debug)]
pub struct ReadFrame {
    pub frame: Frame,
    pub bytes: u64,
    pub count: u32,
}

/// Frames from one feed, read on the stream's thread.
pub trait FrameReader: Send {
    fn read(&mut self) -> Result<ReadFrame, FeedEnd>;
    fn stopper(&self) -> Box<dyn FeedStopper>;
    /// Whether its decoder runs on the graphics card.
    fn hardware(&self) -> bool;
}

/// The plugins' packet steps for one stream (packets.Tap). Each decoder opened with it gets fresh steps of its own.
pub trait PacketTap: Send + Sync {
    /// The steps by owner, in order: an owner new here gets a fresh step in each decoder, one already on keeps its own, [] takes them all off.
    /// True if any came on or off, so the stream asks the phone for a keyframe.
    fn set(&self, makers: Vec<(String, Arc<dyn PacketStepFactory>)>) -> bool;
    /// Whether any plugin changes this stream's packets.
    fn on(&self) -> bool;
}

/// One plugin's frame step, where it runs.
#[derive(Debug)]
pub struct StagedStep {
    /// The plugin's name, for the log when its step fails.
    pub owner: String,
    pub stage: FrameStage,
    pub step: FrameStep,
}

/// A stream's frame steps, run in stage order (registration order within a stage); a step that fails passes the frame on unchanged.
pub trait FramePipeline: Send {
    /// The steps from the next frame on (set_pipeline); give them in registration order.
    fn set_steps(&mut self, steps: Vec<StagedStep>);
    fn process(&mut self, frame: Frame, ctx: &dyn FrameContext) -> Frame;
    fn is_empty(&self) -> bool;
}

/// One stream's newest frames: what it made, and what goes out on its virtual camera (which replace_frame can swap).
pub trait FrameSlot: Send + Sync {
    /// Makes frame the stream's newest, numbered after every frame any stream made before; hands back that number.
    fn publish(&self, frame: Frame) -> u64;
    /// The newest frame the stream itself made, numbered, from any thread.
    fn published(&self) -> Option<Frame>;
    /// Sends frame out in place of the newest one unless a newer one came since (number isn't published's); published keeps the stream's own.
    fn replace(&self, frame: Frame, number: u64) -> bool;
    /// The frame for the virtual camera and a count that goes up whenever it changes.
    fn latest(&self) -> Option<(Frame, u64)>;
    /// Waits until latest's count passes seen, or timeout; true if it did.
    fn wait_newer(&self, seen: u64, timeout: Duration) -> bool;
}

/// The FrameStore plugins read, plus what the window tells it (Frames.set_streams).
pub trait StreamFrames: FrameStore {
    /// A slot for a new stream to publish into.
    fn new_slot(&self) -> Arc<dyn FrameSlot>;
    /// The streams running now as (source id, name, slot), in the order they started; a stream with the camera off is left out.
    fn set_streams(&self, streams: Vec<(String, String, Arc<dyn FrameSlot>)>);
}

/// Decoding as it stands (h264_reader's hardware_device, decoding_backends, hardware_failed).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodingStatus {
    /// The graphics card decoder this computer has ("vaapi", "d3d11va", "cuda"), or None.
    pub hardware_device: Option<String>,
    /// What the live decoders use, sorted: a device name or "processor".
    pub backends: Vec<String>,
    /// A graphics card decoder failed this run, so decoding stays in software until restart.
    pub hardware_failed: bool,
}
