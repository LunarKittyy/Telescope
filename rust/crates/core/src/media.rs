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

/// Lets a stop reach a feed that is still being opened (connecting, handshaking, waiting for the head), before there is a
/// FeedStopper to ask. The opener registers what ends its blocked step with `watch`; a stop before that ends it at once.
#[derive(Clone, Default)]
pub struct OpenStop {
    state: Arc<std::sync::Mutex<(bool, Option<Box<dyn Fn() + Send>>)>>,
}

impl OpenStop {
    pub fn stop(&self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.0 = true;
        if let Some(abort) = st.1.take() {
            abort();
        }
    }

    pub fn stopped(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0
    }

    /// What ends the open's blocked step, called at once when it's stopped already.
    pub fn watch(&self, abort: impl Fn() + Send + 'static) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.0 {
            abort();
        }
        st.1 = Some(Box::new(abort));
    }

    /// Whether both are the same open.
    pub fn is(&self, other: &OpenStop) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

/// Encoded video from a phone or another source, read on the stream's thread.
pub trait VideoFeed: Send {
    fn read(&mut self) -> Result<Packet, FeedEnd>;
    fn stopper(&self) -> Box<dyn FeedStopper>;
    /// What the last read gave decoded to a picture; a feed that falls back while none has (UDP) waits for this.
    fn decoded(&mut self) {}
    /// A flag the reader sets when a picture came out of the decoder, for a feed that is blocked in read() when that happens and so can't be told
    /// with decoded(). A feed that gives one doesn't hear decoded().
    fn decoded_flag(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        None
    }
    /// A count the feed adds to when it skips frames to catch up (UDP resyncing on a keyframe), so they still count as arrived. The reader takes it as it decodes.
    fn skipped_frames(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicU32>> {
        None
    }
    /// Whether this data (as read() gave it) lets decoding start afresh, for the reader that drops a stale backlog up to one.
    /// Annex-B H.264 by default: an IDR slice or a sequence parameter set in it (the phone sends one just ahead of an IDR, as a write of its own over TCP).
    fn is_keyframe(&self, data: &[u8]) -> bool {
        data.windows(4).any(|w| w[..3] == [0, 0, 1] && matches!(w[3] & 0x1F, 5 | 7))
    }
    /// Whether each packet read is a whole picture (or a unit the decoder can start at), so a reader may drop whole packets to catch up.
    /// A byte stream cut into chunks (TCP's H.264) isn't: dropping chunks cuts pictures in half.
    fn packetized(&self) -> bool {
        true
    }
    /// Asks the sender for a keyframe, soon; false if this feed has no way to. Called on the thread that reads.
    fn request_keyframe(&mut self) -> bool {
        false
    }
    /// When (monotonic nanoseconds) the packet read last should reach the screen, for a feed that evens out a sender's bursts. The reader decodes at once
    /// and waits for this afterwards, so the wait hides behind the decoding instead of adding to it.
    fn show_at_ns(&mut self) -> Option<u64> {
        None
    }
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
    /// Where the frame rate the stream is meant to run at goes, for a decoder that hands over to software when the card can't keep up.
    /// The stream sets it when it opens and whenever the rate changes.
    fn target_fps_cell(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicU32>> {
        None
    }
    /// Asks how decoding has gone lately: one plain line (the decoder in use, decode and copy time a picture), for the log when a stream struggles.
    fn timing_probe(&self) -> Option<TimingProbe> {
        None
    }
    /// Where this stream's phone clock offset is kept up to date, for a decoder that reads the capture time the phone stamps on each frame:
    /// it turns it into this computer's clock (Frame::capture_ns). Set before the first packet; a decoder with no capture times ignores it.
    fn set_phone_clock(&mut self, clock: std::sync::Arc<crate::clock::PhoneClock>) {
        let _ = clock;
    }
    /// Where the stream's frame steps say whether they start on the graphics card (FramePipeline::surface_wish). Only while it is set may a decoder
    /// hand over a picture that still sits on the card instead of one in memory; a decoder with nothing on the card ignores it.
    fn set_surface_wish(&mut self, wish: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let _ = wish;
    }
    /// decode, while nobody looks at the stream's pictures: the packet still goes into the decoder, so the picture after the pause needs no keyframe,
    /// but no picture comes out, and a decoder that can skips copying it off the card and converting it. Only count is set.
    fn decode_unseen(&mut self, packet: &Packet) -> Decoded {
        Decoded { newest: None, count: self.decode(packet).count }
    }
}

/// What a Decoder's timing_probe gives: callable from another thread while the decoder runs.
pub type TimingProbe = std::sync::Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Turns a frame into RGB at another size, for the preview.
pub trait Scaler: Send {
    /// RGB24, width * height * 3 bytes.
    fn rgb(&mut self, frame: &Frame, width: u32, height: u32) -> Vec<u8>;

    /// The same picture as RGBA with alpha 255, width * height * 4 bytes (empty when rgb gives the wrong size).
    fn rgba(&mut self, frame: &Frame, width: u32, height: u32) -> Vec<u8> {
        let rgb = self.rgb(frame, width, height);
        if rgb.len() != width as usize * height as usize * 3 {
            return Vec::new();
        }
        let mut rgba = vec![0; rgb.len() / 3 * 4];
        crate::image::add_alpha(&rgb, width as usize, &mut rgba);
        rgba
    }

    /// The picture as RGB into `out` (width * height * 3 bytes), so the caller's own buffer is the one that's filled. False when
    /// it didn't fit and out is left as it was.
    fn rgb_into(&mut self, frame: &Frame, width: u32, height: u32, out: &mut [u8]) -> bool {
        let rgb = self.rgb(frame, width, height);
        let fits = rgb.len() == out.len();
        if fits {
            out.copy_from_slice(&rgb);
        }
        fits
    }
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

/// What arrived off a feed while nobody looked at its pictures (FrameReader::read_unseen): the wire bytes and how many frames.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Arrived {
    pub bytes: u64,
    pub count: u32,
}

/// Frames from one feed, read on the stream's thread.
pub trait FrameReader: Send {
    fn read(&mut self) -> Result<ReadFrame, FeedEnd>;
    /// read, while nobody looks at the stream's pictures (the frame steps paused): the feed is read and decoded as usual, so the next read
    /// needs no keyframe, but what was made is only counted, and a reader that can leaves out the work of handing the picture over.
    fn read_unseen(&mut self) -> Result<Arrived, FeedEnd> {
        self.read().map(|f| Arrived { bytes: f.bytes, count: f.count })
    }
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

/// What a card picture's landing gave instead of the picture when the card failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandIn {
    /// The picture the card's steps began from, without them (uncropped, ungraded): nothing to send in the picture's place but the last good one.
    Unprocessed,
    /// A dark frame, for a picture whose steps hide something: showing the room would be worse.
    Dark,
}

/// The camera's size as FramePipeline::camera_fit holds it (width in the high half), 0 while the camera isn't open.
pub fn pack_camera_fit((width, height): (u32, u32)) -> u64 {
    (u64::from(width) << 32) | u64::from(height)
}

/// The size pack_camera_fit stored, None for 0.
pub fn unpack_camera_fit(packed: u64) -> Option<(u32, u32)> {
    (packed != 0).then_some(((packed >> 32) as u32, packed as u32))
}

/// A media-owned card picture; landing and its cache belong to the implementation.
pub trait CardPicture: Send + Sync {
    fn land(&self) -> Frame;
    fn land_until(&self, _cancel: &dyn Fn() -> bool) -> Frame {
        self.land()
    }
    /// The RAM picture if a landing already made it, without waiting for the card.
    fn landed(&self) -> Option<Frame>;
    fn as_any(&self) -> &dyn std::any::Any;
    /// The picture fitted into width x height as the virtual camera fits frames (image::fit_box, black bars), scaled on the card so only
    /// that much comes back. None when this picture can't be (the caller lands it and fits it itself).
    fn land_fitted(&self, _width: u32, _height: u32, _cancel: &dyn Fn() -> bool) -> Option<Frame> {
        None
    }
    /// The picture land_fitted made for width x height, if it was made already: for a reader that paints from the camera's size and can't wait for the card.
    fn landed_fitted(&self, _width: u32, _height: u32) -> Option<Frame> {
        None
    }
    /// Whether the card can fit this picture into width x height (land_fitted may still fail), whether or not it landed already.
    fn fits_on_card(&self, _width: u32, _height: u32) -> bool {
        false
    }
    /// Whether land_fitted keeps what it makes, so landed_fitted can give it to a reader that can't wait for the card.
    fn keeps_fitted(&self) -> bool {
        false
    }
    /// What stands in for the picture, once its landing failed; None while it is the real one (or not landed yet).
    fn stood_in(&self) -> Option<StandIn> {
        None
    }
}

/// A finished pipeline picture; with a card picture its own frame has only the size and stamps, and every landing shares the same pixels.
#[derive(Clone)]
pub struct PipelineFrame {
    frame: Frame,
    picture: Option<Arc<dyn CardPicture>>,
}

impl PipelineFrame {
    pub fn new(frame: Frame, picture: Option<Arc<dyn CardPicture>>) -> Self {
        // The pixels in hand are the picture's input, not what it came to: nothing reads them, and a decoder surface in them would stay held
        let frame = if picture.is_some() { frame.without_pixels() } else { frame };
        Self { frame, picture }
    }

    /// Size, number and stamps; the pixels are only here when there is no card picture (see landed).
    pub fn metadata(&self) -> &Frame {
        &self.frame
    }

    pub fn picture(&self) -> Option<&Arc<dyn CardPicture>> {
        self.picture.as_ref()
    }

    /// The RAM picture with the publication's stamps, read back from the card if it hasn't been: the caller waits, so never the window's thread or under a lock others need.
    pub fn land(&self) -> Frame {
        self.land_until(&|| false)
    }

    pub fn land_until(&self, cancel: &dyn Fn() -> bool) -> Frame {
        let mut frame = self.picture.as_ref().map_or_else(|| self.frame.clone(), |p| p.land_until(cancel));
        frame.number = self.frame.number;
        frame.time_ns = self.frame.time_ns;
        frame.capture_ns = self.frame.capture_ns;
        frame.phone_share = self.frame.phone_share;
        frame
    }

    /// The picture fitted into width x height on the card with the publication's stamps (CardPicture::land_fitted), None for a picture
    /// already in memory or one the card can't fit. The caller waits, like land.
    pub fn land_fitted_until(&self, width: u32, height: u32, cancel: &dyn Fn() -> bool) -> Option<Frame> {
        let mut frame = self.picture.as_ref()?.land_fitted(width, height, cancel)?;
        frame.number = self.frame.number;
        frame.time_ns = self.frame.time_ns;
        frame.capture_ns = self.frame.capture_ns;
        frame.phone_share = self.frame.phone_share;
        Some(frame)
    }

    /// What land_fitted_until already made for width x height, with the publication's stamps, without waiting for the card.
    pub fn landed_fitted(&self, width: u32, height: u32) -> Option<Frame> {
        let mut frame = self.picture.as_ref()?.landed_fitted(width, height)?;
        frame.number = self.frame.number;
        frame.time_ns = self.frame.time_ns;
        frame.capture_ns = self.frame.capture_ns;
        frame.phone_share = self.frame.phone_share;
        Some(frame)
    }

    /// Whether the card can fit the picture into width x height (CardPicture::fits_on_card); a picture in memory can't.
    pub fn fits_on_card(&self, width: u32, height: u32) -> bool {
        self.picture.as_ref().is_some_and(|p| p.fits_on_card(width, height))
    }

    /// Whether the picture keeps the fitted copy the card makes (CardPicture::keeps_fitted).
    pub fn keeps_fitted(&self) -> bool {
        self.picture.as_ref().is_some_and(|p| p.keeps_fitted())
    }

    /// What stands in for the picture when the card failed to land it (CardPicture::stood_in).
    pub fn stood_in(&self) -> Option<StandIn> {
        self.picture.as_ref()?.stood_in()
    }

    /// The RAM picture if it is there without a readback, so a thread that can't wait for the card can still read what has landed.
    pub fn landed(&self) -> Option<Frame> {
        let mut frame = match &self.picture {
            Some(p) => p.landed()?,
            None => self.frame.clone(),
        };
        frame.number = self.frame.number;
        frame.time_ns = self.frame.time_ns;
        frame.capture_ns = self.frame.capture_ns;
        frame.phone_share = self.frame.phone_share;
        Some(frame)
    }

    /// The RAM picture if it is there, else a dark one with the same size and stamps standing in for it, for a thread that can't wait for the card.
    pub fn landed_or_dark(&self) -> Frame {
        self.landed().unwrap_or_else(|| {
            let mut dark = Frame::dark(self.frame.width, self.frame.height, self.frame.format);
            (dark.number, dark.time_ns, dark.capture_ns) = (self.frame.number, self.frame.time_ns, self.frame.capture_ns);
            dark
        })
    }

    pub fn stamp(&mut self, number: u64, time_ns: u64) {
        self.frame.number = number;
        self.frame.time_ns = time_ns;
    }
}

impl From<Frame> for PipelineFrame {
    fn from(frame: Frame) -> Self {
        Self::new(frame, None)
    }
}

/// A stream's frame steps, run in stage order (registration order within a stage); a step that fails passes the frame on unchanged.
pub trait FramePipeline: Send {
    /// The steps from the next frame on (set_pipeline); give them in registration order.
    fn set_steps(&mut self, steps: Vec<StagedStep>);
    /// `owner`'s steps swapped for these, the others left as they are with what they hold.
    fn replace_steps(&mut self, _owner: &str, _stage: crate::frame::FrameStage, _steps: Vec<crate::frame::FrameStep>) {}
    fn process(&mut self, frame: Frame, ctx: &dyn FrameContext) -> Frame;
    /// Keep a card picture when supported; existing pipelines still return their RAM frame.
    fn process_picture(&mut self, frame: Frame, ctx: &dyn FrameContext) -> PipelineFrame {
        self.process(frame, ctx).into()
    }
    /// A frame that already went through process, run through the steps that hide things (stages Privacy and Cover) again: for the frame
    /// a stalled stream keeps sending when those steps just changed. A Background step that wasn't in the steps before can't be run on a finished
    /// picture, so it makes the frame dark instead. None when none of them had anything to hide, or this pipeline can't.
    fn hide_again(&mut self, _frame: Frame, _ctx: &dyn FrameContext) -> Option<Frame> {
        None
    }
    fn is_empty(&self) -> bool;
    /// The next frame follows a gap (the steps were not run for a while): every step that keeps pictures of its own starts again.
    fn forget_history(&mut self) {}
    /// Where to count what each step costs while the stats ask for it (Timers::on); a pipeline that can't just doesn't.
    fn set_stats(&mut self, _stats: Arc<crate::stats::SourceStats>) {}
    /// Set while the first step to run does it on the graphics card: the only frames a decoder may leave on the card are the ones this wants.
    /// None for a pipeline that never takes one.
    fn surface_wish(&self) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        None
    }
    /// Where the stream's camera says what size it takes, (width << 32 | height) while it is open and 0 while it isn't, so a plain stream's
    /// pictures can be scaled to it on the card. None for a pipeline that never does.
    fn camera_fit(&self) -> Option<Arc<std::sync::atomic::AtomicU64>> {
        None
    }
    /// Set while no step needs the picture and nothing converts it on the card, so the stream may let pictures wait for the card to scale them. None for a pipeline that never does.
    fn plain_steps(&self) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        None
    }
}

/// One stream's newest frames: what it made, and what goes out on its virtual camera (which replace_frame can swap).
pub trait FrameSlot: Send + Sync {
    /// Publish a picture that may still be on the card; the publisher already landed it if ram_wanted said so, so readers of pixels find them there.
    fn publish_picture_at(&self, frame: PipelineFrame, epoch: u64) -> u64 {
        self.publish_at(frame.land(), epoch)
    }
    /// The newest picture as kept, for a reader that paints from the card or lands it where waiting is fine; the window's preview reads this and never lands.
    fn latest_picture(&self) -> Option<(PipelineFrame, u64)> {
        self.latest().map(|(f, v)| (f.into(), v))
    }
    /// Whether anything reads this stream's pictures from memory; the publisher lands each before it goes out when so, on its own thread and outside every lock.
    fn ram_wanted(&self) -> bool {
        true
    }
    /// A reader that needs pictures in memory and can't land one itself (the window): from the next picture on they are landed before they go out, for a while after the last ask.
    fn want_ram(&self) {}
    /// Makes frame the stream's newest, numbered after every frame any stream made before; hands back that number.
    fn publish(&self, frame: Frame) -> u64;
    /// The newest frame the stream itself made, numbered, from any thread.
    fn published(&self) -> Option<Frame>;
    /// Sends frame out in place of the newest one unless a newer one came since (number isn't published's); published keeps the stream's own.
    fn replace(&self, frame: Frame, number: u64) -> bool;
    /// The same as `replace`, but only while the camera's frame still has the pixels `expected`: another plugin may have put its own out since.
    fn replace_if(&self, frame: Frame, number: u64, expected: &Arc<Vec<u8>>) -> bool {
        self.latest().is_some_and(|(f, _)| Arc::ptr_eq(&f.data, expected)) && self.replace(frame, number)
    }
    /// The frame for the virtual camera and a count that goes up whenever it changes.
    fn latest(&self) -> Option<(Frame, u64)>;
    /// Waits until latest's count passes seen, or timeout; true if it did.
    fn wait_newer(&self, seen: u64, timeout: Duration) -> bool;
    /// The stream's phone clock, which is what its frames' capture times were read against (None for a stream with no phone clock), so
    /// FrameStore::clock_sync can say how good they are. Set again whenever the stream opens a new connection.
    fn set_phone_clock(&self, _clock: Option<Arc<crate::clock::PhoneClock>>) {}
    fn phone_clock(&self) -> Option<Arc<crate::clock::PhoneClock>> {
        None
    }
    /// The frame in the recent history (see StreamFrames::set_history) whose capture time is nearest to capture_ns, None when there's no history
    /// or none of it has a capture time.
    fn near(&self, _capture_ns: u64) -> Option<Frame> {
        None
    }
    /// The pipeline epoch (FrameStore::pipeline_epoch) as this slot sees it now; 0 for a slot that has none.
    fn epoch(&self) -> u64 {
        0
    }
    /// Like `publish`, for a frame the steps started on while the pipeline epoch was `epoch`: if it has gone up since, the frame is told from
    /// the ones made after, whatever the epoch is when it comes out.
    fn publish_at(&self, frame: Frame, _epoch: u64) -> u64 {
        self.publish(frame)
    }
    /// The steps of this stream's pipeline were just handed out again: lets go of the frames `near` would hand out and starts a new epoch for
    /// every stream (FrameStore::pipeline_epoch), so one that went out before a Blur came on is never handed to a plugin that puts stream
    /// frames together afterwards, and frames published from now on are told from the ones before.
    fn forget_history(&self) {}
}

/// The FrameStore plugins read, plus what the window tells it (Frames.set_streams).
pub trait StreamFrames: FrameStore {
    /// A slot for a new stream to publish into.
    fn new_slot(&self) -> Arc<dyn FrameSlot>;
    /// The streams running now as (source id, name, slot), in the order they started; a stream with the camera off is left out.
    fn set_streams(&self, streams: Vec<(String, String, Arc<dyn FrameSlot>)>);
    /// How many of each stream's newest frames are kept for FrameStore::stream_frame_near; 0, the default, keeps none (the stream's newest
    /// frame is the only one held). Takes effect from the next frame each stream publishes.
    fn set_history(&self, _frames: usize) {}
}

/// What Setup's saved settings say about decoding on the graphics card: Some for a choice made there, None for one never made. Older
/// versions always saved `hw_decode`, so a false only counts as a choice when `hw_decode_chosen` came with it; a value that isn't a
/// boolean is no choice either.
pub fn hardware_decode_choice(setup: Option<&serde_json::Value>) -> Option<bool> {
    match setup.and_then(|c| c.get("hw_decode")) {
        Some(serde_json::Value::Bool(true)) => Some(true),
        Some(serde_json::Value::Bool(false)) => setup.and_then(|c| c.get("hw_decode_chosen")).is_some_and(|v| v == &serde_json::Value::Bool(true)).then_some(false),
        Some(_) | None => None,
    }
}

/// Whether decoding on the graphics card is on when nobody chose: on Linux when the card found is VA-API, which decodes H.264 well
/// there. Anything else stays in software until it's turned on.
pub fn hardware_decode_default(linux: bool, device: Option<&str>) -> bool {
    linux && device == Some("vaapi")
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
    /// The graphics card was too slow lately, so the processor decodes for a while and the card gets another go after.
    #[serde(default)]
    pub hardware_slow: bool,
}

/// Hands the newest packet from a stream's reader to its decoders (stream._Newest): one nobody took in time is
/// dropped, so a slow decoder skips frames instead of letting them queue up as lag.
pub struct Newest<T> {
    item: std::sync::Mutex<(std::collections::VecDeque<(std::time::Instant, T)>, bool, Option<std::time::Duration>)>,
    ready: std::sync::Condvar,
}

impl<T> Default for Newest<T> {
    fn default() -> Self {
        Newest { item: std::sync::Mutex::new((std::collections::VecDeque::new(), false, None)), ready: std::sync::Condvar::new() }
    }
}

impl<T> Newest<T> {
    /// Replaces whatever is waiting.
    /// True when it replaced an item nobody had taken, which is dropped.
    pub fn put(&self, item: T) -> bool {
        self.put_keeping(item, 1, None) > 0
    }

    /// Adds an item behind up to `kept - 1` waiting ones, dropping the oldest beyond that and any that have waited more than `max_age` (so a slow patch leaves no standing delay): for a stage that now and then takes more than one frame time.
    /// How many items nobody had taken were dropped.
    pub fn put_keeping(&self, item: T, kept: usize, max_age: Option<std::time::Duration>) -> usize {
        let now = std::time::Instant::now();
        let mut st = self.item.lock().unwrap();
        st.2 = max_age;
        let mut dropped = 0;
        while max_age.is_some_and(|age| st.0.front().is_some_and(|(at, _)| now.duration_since(*at) > age)) {
            st.0.pop_front();
            dropped += 1;
        }
        st.0.push_back((now, item));
        while st.0.len() > kept.max(1) {
            st.0.pop_front();
            dropped += 1;
        }
        drop(st);
        self.ready.notify_one();
        dropped
    }

    /// No more items come; take() hands out what's left, then None.
    pub fn finish(&self) {
        self.item.lock().unwrap().1 = true;
        self.ready.notify_all();
    }

    /// The oldest item waiting (the only one unless put_keeping kept more), waiting for one; None once finished and nothing is left.
    pub fn take(&self) -> Option<T> {
        self.take_counting().0
    }

    /// `take`, and how many items it dropped because they had waited longer than the `max_age` of the last put_keeping: nothing newer came to push them out (the feed stalled or ended), and they are never handed out late.
    pub fn take_counting(&self) -> (Option<T>, usize) {
        let mut st = self.item.lock().unwrap();
        let mut dropped = 0;
        loop {
            let now = std::time::Instant::now();
            while st.2.is_some_and(|age| st.0.front().is_some_and(|(at, _)| now.duration_since(*at) > age)) {
                st.0.pop_front();
                dropped += 1;
            }
            if let Some((_, item)) = st.0.pop_front() {
                return (Some(item), dropped);
            }
            if st.1 {
                return (None, dropped);
            }
            st = self.ready.wait(st).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_keeps_the_asked_number_and_hands_out_the_oldest_first() {
        let q = Newest::default();
        assert_eq!(q.put_keeping(1, 2, None), 0);
        assert_eq!(q.put_keeping(2, 2, None), 0);
        assert_eq!(q.put_keeping(3, 2, None), 1, "the oldest is dropped beyond two");
        assert_eq!((q.take(), q.take()), (Some(2), Some(3)));
        assert!(!q.put(4));
        assert!(q.put(5), "the plain put still keeps only the newest");
        assert_eq!(q.take(), Some(5));
    }

    #[test]
    fn newest_drops_what_waited_too_long_and_counts_every_drop() {
        let q = Newest::default();
        q.put_keeping(1, 3, None);
        q.put_keeping(2, 3, None);
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(q.put_keeping(3, 3, Some(std::time::Duration::from_millis(10))), 2, "both stale pictures go at once");
        assert_eq!(q.take(), Some(3));
    }

    #[test]
    fn newest_never_hands_out_what_waited_too_long_for_a_taker_even_when_nothing_newer_came() {
        let q = Newest::default();
        q.put_keeping(1, 3, Some(std::time::Duration::from_millis(10)));
        q.put_keeping(2, 3, Some(std::time::Duration::from_millis(10)));
        std::thread::sleep(std::time::Duration::from_millis(40));
        q.finish();
        assert_eq!(q.take_counting(), (None, 2), "stale pictures were drained after the feed ended");
        let q = Newest::default();
        q.put_keeping(1, 3, Some(std::time::Duration::from_secs(60)));
        q.finish();
        assert_eq!(q.take_counting(), (Some(1), 0), "a picture within its time is handed out as before");
    }

    /// A scaler that only knows rgb, as most do.
    struct Greys(usize);

    impl Scaler for Greys {
        fn rgb(&mut self, _frame: &Frame, width: u32, height: u32) -> Vec<u8> {
            (0..width as usize * height as usize * 3).map(|i| (i % 251) as u8).take(self.0).collect()
        }
    }

    #[test]
    fn a_scaler_that_only_gives_rgb_gives_rgba_and_fills_a_buffer_too() {
        let frame = Frame::new(1, 1, crate::frame::PixelFormat::Rgba, vec![0; 4]);
        let mut scaler = Greys(usize::MAX);
        let rgb = scaler.rgb(&frame, 5, 3);
        let rgba = scaler.rgba(&frame, 5, 3);
        assert_eq!(rgba.len(), 5 * 3 * 4);
        assert!(rgba.as_chunks::<4>().0.iter().zip(rgb.as_chunks::<3>().0).all(|(a, c)| a[..3] == c[..] && a[3] == 255));
        let mut out = vec![0; 5 * 3 * 3];
        assert!(scaler.rgb_into(&frame, 5, 3, &mut out));
        assert_eq!(out, rgb);
        assert!(!scaler.rgb_into(&frame, 5, 3, &mut [0; 10]), "a buffer of the wrong size isn't filled");
        // A picture of the wrong size is none
        assert!(Greys(10).rgba(&frame, 5, 3).is_empty());
        let mut out = vec![9; 5 * 3 * 3];
        assert!(!Greys(10).rgb_into(&frame, 5, 3, &mut out) && out.iter().all(|v| *v == 9));
    }

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

    /// A card picture that only knows its pixels once landed.
    struct Away(std::sync::OnceLock<Frame>);

    impl CardPicture for Away {
        fn land(&self) -> Frame {
            self.0.get_or_init(|| Frame::new(2, 2, crate::frame::PixelFormat::Rgba, vec![9; 16])).clone()
        }
        fn landed(&self) -> Option<Frame> {
            self.0.get().cloned()
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn a_pipeline_picture_on_the_card_keeps_only_its_stamps_until_it_is_landed() {
        let mut input = Frame::new(2, 2, crate::frame::PixelFormat::Rgba, vec![1; 16]);
        (input.number, input.capture_ns) = (7, Some(3));
        let mut picture = PipelineFrame::new(input.clone(), Some(Arc::new(Away(Default::default()))));
        assert!(picture.metadata().data.is_empty(), "the input's pixels aren't kept beside a picture made from them");
        assert_eq!((picture.metadata().number, picture.metadata().capture_ns), (7, Some(3)));
        assert!(picture.landed().is_none());
        let dark = picture.landed_or_dark();
        assert_eq!((dark.width, dark.number, dark.data[3]), (2, 7, 255), "a dark stand-in of the same size and number");
        picture.stamp(8, 100);
        let landed = picture.land();
        assert_eq!((landed.data[0], landed.number, landed.time_ns), (9, 8, 100), "landed with the publication's stamps");
        assert_eq!(picture.landed().unwrap().data[0], 9);
        assert_eq!(PipelineFrame::from(input).landed().unwrap().data[0], 1, "a picture with none on the card is its own pixels");
    }
}
