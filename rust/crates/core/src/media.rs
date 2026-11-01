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
    /// One JPEG per packet (Heavy streams).
    Mjpeg,
    /// One VP8 frame per packet (the Browser camera, from a browser that has no H.264).
    Vp8,
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
    /// What the last read gave decoded to a picture; a feed that falls back while none has (UDP) waits for this.
    fn decoded(&mut self) {}
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

/// Encoded audio to 48 kHz mono s16le, the PCM the phone's /v1/audio sends.
pub trait AudioDecoder: Send {
    /// One packet's samples; empty when it doesn't decode.
    fn decode(&mut self, packet: &[u8]) -> Vec<u8>;
}

pub trait Decoder: Send {
    fn decode(&mut self, packet: &Packet) -> Decoded;
    /// Whether it really ended up on the graphics card.
    fn hardware(&self) -> bool;
    /// For a decoder that may change horses mid-stream: ask gets the stream a keyframe, so a new decoder has something to start from.
    fn on_keyframe_needed(&mut self, ask: Box<dyn Fn() + Send>) {
        let _ = ask;
    }
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
    /// Opus as WebRTC carries it (the Browser camera's mic).
    fn opus_decoder(&self) -> Result<Box<dyn AudioDecoder>, String> {
        Err("Opus decoding isn't in this build".into())
    }

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
    /// A picture file's bytes (PNG, JPEG, WebP or BMP) as an RGBA frame, for the pictures plugins put behind the person.
    fn decode_picture(&self, bytes: &[u8]) -> Result<Frame, String> {
        let _ = bytes;
        Err("pictures can't be read in this build".into())
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
    /// `owner`'s steps swapped for these, the others left as they are with what they hold.
    fn replace_steps(&mut self, _owner: &str, _stage: crate::frame::FrameStage, _steps: Vec<crate::frame::FrameStep>) {}
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

/// Saved next to a false by this app, which is how its choice is told from Python's, which saved false whatever the person wanted.
pub const HW_DECODE_FROM_RUST: &str = "hw_decode_from_rust";

/// Whether Setup's saved settings ask for the graphics card decoder. On unless they say otherwise, and a false only counts with
/// HW_DECODE_FROM_RUST next to it: Python never writes that, and the fallback to software now covers a card that can't decode.
pub fn hardware_decode_on(setup: Option<&serde_json::Value>) -> bool {
    use serde_json::Value;
    match setup.and_then(|c| c.get("hw_decode")) {
        None | Some(Value::Bool(true)) => true,
        Some(Value::Bool(false)) => setup.and_then(|c| c.get(HW_DECODE_FROM_RUST)) != Some(&Value::Bool(true)),
        Some(_) => false,
    }
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

/// Hands the newest packet from a stream's reader to its decoders (stream._Newest): one nobody took in time is
/// dropped, so a slow decoder skips frames instead of letting them queue up as lag.
pub struct Newest<T> {
    item: std::sync::Mutex<(Option<T>, bool)>,
    ready: std::sync::Condvar,
}

impl<T> Default for Newest<T> {
    fn default() -> Self {
        Newest { item: std::sync::Mutex::new((None, false)), ready: std::sync::Condvar::new() }
    }
}

impl<T> Newest<T> {
    /// Replaces whatever is waiting.
    pub fn put(&self, item: T) {
        self.item.lock().unwrap().0 = Some(item);
        self.ready.notify_one();
    }

    /// No more items come; take() hands out what's left, then None.
    pub fn finish(&self) {
        self.item.lock().unwrap().1 = true;
        self.ready.notify_all();
    }

    /// The newest item, waiting for one; None once finished and nothing is left.
    pub fn take(&self) -> Option<T> {
        let mut st = self.item.lock().unwrap();
        loop {
            if let Some(item) = st.0.take() {
                return Some(item);
            }
            if st.1 {
                return None;
            }
            st = self.ready.wait(st).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_hands_over_only_the_latest_item() {
        let newest = Newest::default();
        newest.put("old");
        newest.put("new");
        newest.finish();
        assert_eq!(newest.take(), Some("new"));
        assert_eq!(newest.take(), None);
    }

    #[test]
    fn a_waiting_taker_wakes_for_an_item_or_the_end() {
        let newest = std::sync::Arc::new(Newest::default());
        let n = newest.clone();
        let taker = std::thread::spawn(move || (n.take(), n.take()));
        std::thread::sleep(Duration::from_millis(20));
        newest.put(1);
        std::thread::sleep(Duration::from_millis(20));
        newest.finish();
        assert_eq!(taker.join().unwrap(), (Some(1), None));
    }
}
