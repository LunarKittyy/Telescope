//! One stream's video (StreamWorker in stream.py): it reads the source's feed, decodes it, runs the plugins' frame
//! steps and feeds the virtual camera, reconnecting when the stream drops and moving to another route without a gap.
//!
//! The reader (and its phone connection) outlives virtual camera restarts: an fps change only reopens the camera.
//! Decoding, the frame steps and frame numbering are media's (frame_reader, FramePipeline, FrameSlot); the worker owns
//! the threads, the order frames come out in, settling and what it reports.

use crate::texts::fill;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use telescope_core::frame::{Frame, FrameContext, FrameStep, PixelFormat};
use telescope_core::i18n::tr;
use telescope_core::image::resize_linear_into;
use telescope_core::media::{Codec, DecodeOptions, Decoder, FeedEnd, FeedStopper, FramePipeline, FrameReader, FrameSlot,
                            Media, Newest, PacketTap, StagedStep, VideoFeed};
use telescope_core::output::{Outputs, VideoOutput};

pub const RECONNECT_DELAY: Duration = Duration::from_secs(3);
/// Between tries after a stream drops mid-way, since a phone back on Wi-Fi answers within seconds.
pub const DROP_RETRY_DELAY: Duration = Duration::from_secs(1);
/// hand_over(): the new route has to keep frames coming this long, with no gap past HANDOVER_GAP, before the stream moves
pub const HANDOVER_HEALTHY: Duration = Duration::from_secs(2);
pub const HANDOVER_GAP: Duration = Duration::from_millis(500);
/// ...or it gives up on it
pub const HANDOVER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a ready one waits for the reader to take it (a read on the old route can block).
pub const HANDOVER_TAKE: Duration = Duration::from_secs(5);
/// After a Start, a lens switch or a new size or fps, the link is judged only once frames have flowed this long...
pub const SETTLE_AFTER_FRAMES: Duration = Duration::from_millis(2500);
/// ...a gap this long starts that over...
pub const SETTLE_GAP: Duration = Duration::from_secs(1);
/// ...or once this much has passed, frames or not, so a phone that never recovers still says so.
pub const SETTLE_MAX: Duration = Duration::from_secs(10);

/// JPEG decoders running side by side: at 4K one decode takes longer than a frame lasts.
fn mjpeg_decoders() -> usize {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    cpus.saturating_sub(1).clamp(1, 2)
}

/// What a worker reports, from its own threads.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkerEvent {
    /// (kind, text): info, ok, warn, err, waiting, reconnecting, fps, net, net_warn, idle, error.
    Status(String, String),
    /// A reconnect mid-stream worked (not the first connect).
    Reconnected,
    /// hand_over() moved the stream to this URL without a gap.
    Moved(String),
    /// hand_over() gave up on this URL; the stream carries on where it was.
    MoveFailed(String),
    /// The virtual camera opened at this width and height.
    VcamOpened(u32, u32),
    /// A picture for the preview, RGB24 at the size set_preview_size() asked for.
    Preview(Vec<u8>, u32, u32),
    /// The decoder changed mid-stream and has nothing to start from until a keyframe.
    NeedsKeyframe,
}

pub type Events = Arc<dyn Fn(WorkerEvent) + Send + Sync>;

/// Opens a fresh feed of the stream's video (a phone session's open_video, or a StreamSource's open_feed).
pub type Opener = Arc<dyn Fn() -> Result<Opened, String> + Send + Sync>;

/// A feed an opener opened: its codec, and a decoder of the source's own for packets media can't decode (a webcam's
/// raw YUYV or NV12 frames, StreamSource::decoder).
pub struct Opened {
    pub codec: Codec,
    pub feed: Box<dyn VideoFeed>,
    pub own_decoder: Option<Box<dyn Decoder>>,
}

impl From<(Codec, Box<dyn VideoFeed>)> for Opened {
    fn from((codec, feed): (Codec, Box<dyn VideoFeed>)) -> Opened {
        Opened { codec, feed, own_decoder: None }
    }
}

/// What a worker starts with.
pub struct WorkerSetup {
    pub url: String,
    pub opener: Opener,
    /// The size frames are scaled to before the plugins, None to pass them through.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: u32,
    pub canvas: (Option<u32>, Option<u32>),
    /// Which virtual camera it feeds.
    pub slot: u32,
    pub source_id: String,
    pub hardware_decode: bool,
    /// A plugin's StreamSource: it has no other route to hand over to.
    pub plugin_source: bool,
    /// Why a plugin source isn't streaming yet, shown while the stream waits to try again (WebcamReader.waiting_text).
    pub waiting_text: Option<WaitingText>,
    /// The vision service and how frames are turned upright for it (person masks and people), None without vision.
    pub vision: Option<VisionFeed>,
    /// The frame steps, in before the thread starts: no frame goes out without them (a privacy blur, say).
    pub steps: Vec<StagedStep>,
    /// Which crop each frame gets while the phone's share changes.
    pub gate: Option<Arc<crate::handover::ViewGate>>,
}

/// What a worker hands the vision service: its frames before the view, turned as the window's view says.
#[derive(Clone)]
pub struct VisionFeed {
    pub service: Arc<dyn telescope_core::vision::VisionService>,
    pub upright: Arc<Mutex<telescope_core::vision::Upright>>,
    /// Where each stream says how big its frames are before the view.
    pub frame_sizes: Arc<Mutex<std::collections::HashMap<String, (u32, u32)>>>,
    /// Motion in zones of the picture, looked for in the same frames while a plugin watches.
    pub motion: Option<Arc<telescope_core::motion::Motion>>,
}

/// StreamSource::waiting_text, asked each time opening the stream fails.
pub type WaitingText = Arc<dyn Fn() -> String + Send + Sync>;

/// A one-shot flag threads can wait on (threading.Event).
#[derive(Default)]
pub struct Flag {
    set: Mutex<bool>,
    cond: Condvar,
}

impl Flag {
    pub fn set(&self) {
        *self.set.lock().unwrap() = true;
        self.cond.notify_all();
    }
    pub fn clear(&self) {
        *self.set.lock().unwrap() = false;
    }
    pub fn is_set(&self) -> bool {
        *self.set.lock().unwrap()
    }
    /// Waits until set or timeout; whether it's set.
    pub fn wait(&self, timeout: Duration) -> bool {
        let guard = self.set.lock().unwrap();
        let (guard, _) = self.cond.wait_timeout_while(guard, timeout, |set| !*set).unwrap();
        *guard
    }
}

/// Whether the link can be judged yet: after a start or a camera restart the phone takes a while to make frames at
/// full rate again.
#[derive(Clone, Debug)]
pub struct Settle {
    /// Since when it's settling; None once over.
    pub since: Option<Instant>,
    /// Since when frames have flowed.
    pub flowing: Option<Instant>,
    pub last_frame: Option<Instant>,
}

impl Settle {
    pub fn new(now: Instant) -> Settle {
        Settle { since: Some(now), flowing: None, last_frame: None }
    }

    /// The phone is restarting its camera (a lens switch, a new size): hold off judging the link until it's back.
    pub fn restart(&mut self, now: Instant) {
        self.since = Some(now);
        self.flowing = None;
    }

    pub fn arrived(&mut self, now: Instant) {
        if self.since.is_none() {
            return;
        }
        // Once frames have kept coming long enough the phone is back, and a gap after that is the link's.
        let gap_while_starting = match (self.flowing, self.last_frame) {
            (Some(flowing), Some(last)) => last - flowing < SETTLE_AFTER_FRAMES && now - last > SETTLE_GAP,
            _ => false,
        };
        if self.flowing.is_none() || gap_while_starting {
            self.flowing = Some(now);
        }
        self.last_frame = Some(now);
    }

    /// Whether a window from start on says anything about the link: it has to start after settling ended.
    pub fn judges(&mut self, start: Instant) -> bool {
        let Some(since) = self.since else { return true };
        let mut end = since + SETTLE_MAX;
        if let Some(flowing) = self.flowing {
            end = end.min(flowing + SETTLE_AFTER_FRAMES);
        }
        if start < end {
            return false;
        }
        self.since = None;
        true
    }
}

/// frame scaled to fit width x height, keeping its shape, with black bars as needed (the same frame if it fits).
/// Black in the camera's video range is Y 16, as Python's bars come out.
pub fn fit_frame(frame: &Frame, width: u32, height: u32) -> Frame {
    if (frame.width, frame.height) == (width, height) || frame.format != PixelFormat::I420 {
        return frame.clone();
    }
    let scale = (width as f64 / frame.width as f64).min(height as f64 / frame.height as f64);
    let (nw, nh) = ((frame.width as f64 * scale) as u32, (frame.height as f64 * scale) as u32);
    let (x, y) = ((width - nw.min(width)) / 2, (height - nh.min(height)) / 2);
    let mut out = vec![0u8; PixelFormat::I420.size(width, height)];
    let (sy, su, sv) = frame.i420_planes().unwrap();
    let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
    let (oy, rest) = out.split_at_mut((width * height) as usize);
    let (ou, ov) = rest.split_at_mut(cw * ch);
    oy.fill(16);
    ou.fill(128);
    ov.fill(128);
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    resize_linear_into(sy, fw, fh, 1, oy, width as usize, (x as usize, y as usize, nw as usize, nh as usize));
    let (fcw, fch) = (fw.div_ceil(2), fh.div_ceil(2));
    let chroma = ((x / 2) as usize, (y / 2) as usize, nw.div_ceil(2) as usize, nh.div_ceil(2) as usize);
    resize_linear_into(su, fcw, fch, 1, ou, cw, chroma);
    resize_linear_into(sv, fcw, fch, 1, ov, cw, chroma);
    let mut fitted = Frame::new(width, height, PixelFormat::I420, out);
    fitted.number = frame.number;
    fitted.time_ns = frame.time_ns;
    fitted
}

/// An I420 frame as NV12, for a camera that only takes that (the Windows camera's shared memory).
pub fn to_nv12(frame: &Frame) -> Frame {
    let Some((y, u, v)) = frame.i420_planes() else { return frame.clone() };
    let mut out = Vec::with_capacity(frame.data.len());
    out.extend_from_slice(y);
    for (a, b) in u.iter().zip(v) {
        out.push(*a);
        out.push(*b);
    }
    let mut nv = Frame::new(frame.width, frame.height, PixelFormat::Nv12, out);
    nv.number = frame.number;
    nv
}

/// What the frame steps may ask about the frame they work on.
struct Ctx {
    source_id: String,
    vision: Option<Arc<dyn telescope_core::vision::VisionService>>,
}

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        &self.source_id
    }
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.vision.as_ref()?.person_mask(&self.source_id, frame)
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// Where frames come from while connected: H.264 through media's frame reader, or JPEGs each decoded on their own.
enum Cap {
    Frames(Box<dyn FrameReader>),
    /// own: the decoder is the source's, the only one there is, so it's used alone instead of media's JPEG decoders.
    Jpegs { feed: Box<dyn VideoFeed>, decoder: Box<dyn Decoder>, own: bool },
}

impl Cap {
    fn stopper(&self) -> Box<dyn FeedStopper> {
        match self {
            Cap::Frames(r) => r.stopper(),
            Cap::Jpegs { feed, .. } => feed.stopper(),
        }
    }

    /// The next frame, decoded right here (a first frame, or one checking a route works): (frame, bytes, count).
    fn read_frame(&mut self) -> Result<(Frame, u64, u32), FeedEnd> {
        match self {
            Cap::Frames(r) => r.read().map(|f| (f.frame, f.bytes, f.count)),
            Cap::Jpegs { feed, decoder, .. } => loop {
                let packet = feed.read()?;
                let bytes = packet.data.len() as u64;
                if let Some(frame) = decoder.decode(&packet).newest {
                    return Ok((frame, bytes, 1));
                }
            },
        }
    }
}

struct Shared {
    url: Mutex<String>,
    opener: Mutex<Opener>,
    waiting_text: Option<WaitingText>,
    media: Arc<dyn Media>,
    outputs: Arc<dyn Outputs>,
    events: Events,
    slot_number: u32,
    source_id: String,
    vision: Option<VisionFeed>,
    gate: Option<Arc<crate::handover::ViewGate>>,
    hardware: bool,
    plugin_source: bool,
    size: Mutex<(Option<u32>, Option<u32>)>,
    fps: AtomicU32,
    canvas: (Option<u32>, Option<u32>),
    stop: AtomicBool,
    restart_vcam: Flag,
    retry_now: AtomicBool,
    failed_tries: AtomicU32,
    parked: AtomicBool,
    vcam_free: Flag,
    settle: Mutex<Settle>,
    photo_until: Mutex<Option<Instant>>,
    /// The frame steps, and the newest packet number that made it out (one frame at a time, never an older one).
    publish: Mutex<(Box<dyn FramePipeline>, u64)>,
    next_steps: Mutex<Option<Vec<StagedStep>>>,
    next_colour: Mutex<Option<Vec<FrameStep>>>,
    seq: AtomicU64,
    slot: Arc<dyn FrameSlot>,
    tap: Option<Arc<dyn PacketTap>>,
    bytes_total: AtomicU64,
    // When the newest published frame's packet came in, for the delivery log; publish overwrites the frame's own time
    newest_arrival: AtomicU64,
    frames_arrived: AtomicU64,
    last_arrival_fps: Mutex<f32>,
    camera_size: Mutex<Option<(u32, u32)>>,
    stopper: Mutex<Option<Box<dyn FeedStopper>>>,
    /// The preview's size, (width << 32) | height; 0 while nobody shows this stream.
    preview_size: AtomicU64,
    handover_busy: AtomicBool,
    /// A hand_over()'s reader, healthy and waiting to take over: (cap, url, taken).
    handover_ready: Mutex<Option<(Cap, String, Arc<Flag>)>>,
    handover_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// A running stream's worker as the window uses it: StreamWorker, or a test's stand-in.
pub trait VideoWorker: Send + Sync {
    fn source_id(&self) -> Option<&str>;
    fn tap(&self) -> Option<Arc<dyn PacketTap>>;
    fn slot(&self) -> Arc<dyn FrameSlot>;
    fn set_steps(&self, steps: Vec<StagedStep>);
    fn set_colour(&self, steps: Vec<FrameStep>);
    fn update_output(&self, width: Option<Option<u32>>, height: Option<Option<u32>>, fps: Option<u32>);
    fn settle(&self);
    fn hold_for_photo(&self, seconds: f32);
    fn stop(&self);
    fn parked(&self) -> bool;
    fn set_parked(&self, parked: bool);
    fn retarget(&self, url: &str, opener: Opener);
    fn can_hand_over(&self) -> bool;
    fn set_opener(&self, opener: Opener);
    fn failed_tries(&self) -> u32;
    fn last_arrival_fps(&self) -> f32;
    fn camera_size(&self) -> Option<(u32, u32)>;
    fn hand_over(&self, url: &str, opener: Opener) -> bool;
}

/// Starts a stream's worker in place of StreamWorker::start.
pub type WorkerMaker = Rc<dyn Fn(WorkerSetup, Arc<dyn FrameSlot>, Events) -> Arc<dyn VideoWorker>>;

impl VideoWorker for StreamWorker {
    fn source_id(&self) -> Option<&str> {
        StreamWorker::source_id(self)
    }
    fn tap(&self) -> Option<Arc<dyn PacketTap>> {
        StreamWorker::tap(self)
    }
    fn slot(&self) -> Arc<dyn FrameSlot> {
        StreamWorker::slot(self)
    }
    fn set_steps(&self, steps: Vec<StagedStep>) {
        StreamWorker::set_steps(self, steps);
    }
    fn set_colour(&self, steps: Vec<FrameStep>) {
        StreamWorker::set_colour(self, steps);
    }
    fn update_output(&self, width: Option<Option<u32>>, height: Option<Option<u32>>, fps: Option<u32>) {
        StreamWorker::update_output(self, width, height, fps);
    }
    fn settle(&self) {
        StreamWorker::settle(self);
    }
    fn hold_for_photo(&self, seconds: f32) {
        StreamWorker::hold_for_photo(self, seconds);
    }
    fn stop(&self) {
        StreamWorker::stop(self);
    }
    fn parked(&self) -> bool {
        StreamWorker::parked(self)
    }
    fn set_parked(&self, parked: bool) {
        StreamWorker::set_parked(self, parked);
    }
    fn retarget(&self, url: &str, opener: Opener) {
        StreamWorker::retarget(self, url, opener);
    }
    fn can_hand_over(&self) -> bool {
        StreamWorker::can_hand_over(self)
    }
    fn set_opener(&self, opener: Opener) {
        StreamWorker::set_opener(self, opener);
    }
    fn failed_tries(&self) -> u32 {
        StreamWorker::failed_tries(self)
    }
    fn last_arrival_fps(&self) -> f32 {
        StreamWorker::last_arrival_fps(self)
    }
    fn camera_size(&self) -> Option<(u32, u32)> {
        StreamWorker::camera_size(self)
    }
    fn hand_over(&self, url: &str, opener: Opener) -> bool {
        StreamWorker::hand_over(self, url, opener)
    }
}

/// One stream's video worker; dropping it doesn't stop it, stop() does.
pub struct StreamWorker {
    shared: Arc<Shared>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl StreamWorker {
    /// Starts reading at once. slot is the stream's frame slot (media's stream_frames), which the plugins read.
    pub fn start(setup: WorkerSetup, media: Arc<dyn Media>, outputs: Arc<dyn Outputs>, slot: Arc<dyn FrameSlot>,
                 events: Events) -> Result<StreamWorker, String> {
        let mut pipeline = media.pipeline().unwrap_or_else(|e| {
            log::warn!("No frame steps for this stream: {e}");
            Box::new(NoSteps)
        });
        pipeline.set_steps(setup.steps);
        let tap = media.packet_tap();
        let shared = Arc::new(Shared {
            url: Mutex::new(setup.url), opener: Mutex::new(setup.opener), waiting_text: setup.waiting_text, media, outputs, events,
            slot_number: setup.slot, source_id: setup.source_id, vision: setup.vision, hardware: setup.hardware_decode,
            plugin_source: setup.plugin_source, size: Mutex::new((setup.width, setup.height)),
            fps: AtomicU32::new(setup.fps.max(1)), canvas: setup.canvas, stop: AtomicBool::new(false),
            restart_vcam: Flag::default(), retry_now: AtomicBool::new(false), failed_tries: AtomicU32::new(0),
            parked: AtomicBool::new(false), vcam_free: Flag::default(), settle: Mutex::new(Settle::new(Instant::now())),
            photo_until: Mutex::new(None), publish: Mutex::new((pipeline, 0)), next_steps: Mutex::new(None), next_colour: Mutex::new(None), gate: setup.gate,
            seq: AtomicU64::new(1), slot, tap, bytes_total: AtomicU64::new(0), newest_arrival: AtomicU64::new(0), frames_arrived: AtomicU64::new(0),
            last_arrival_fps: Mutex::new(0.0), camera_size: Mutex::new(None), stopper: Mutex::new(None),
            preview_size: AtomicU64::new(0), handover_busy: AtomicBool::new(false), handover_ready: Mutex::new(None),
            handover_thread: Mutex::new(None),
        });
        shared.vcam_free.set();
        let s = shared.clone();
        let thread = std::thread::Builder::new().name("stream".into()).spawn(move || {
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.run()));
            if let Err(e) = caught {
                // A bug here must end the stream visibly, not leave a thread the app thinks is still streaming
                let what = e.downcast_ref::<String>().cloned().or(e.downcast_ref::<&str>().map(|s| s.to_string()));
                log::error!("Stream worker crashed: {}", what.clone().unwrap_or_default());
                s.status("error", &fill(&tr("Stream error: {error}"), &[("error", &what.unwrap_or_default())]));
            }
            s.join_handover();
            (crate::window::app_hooks().release_memory)();
            s.status("idle", &tr("Not streaming"));
        }).map_err(|e| e.to_string())?;
        Ok(StreamWorker { shared, thread: Mutex::new(Some(thread)) })
    }

    pub fn url(&self) -> String {
        self.shared.url.lock().unwrap().clone()
    }

    /// The stream's source id, or None for the one with no id.
    pub fn source_id(&self) -> Option<&str> {
        Some(self.shared.source_id.as_str()).filter(|id| !id.is_empty())
    }

    /// The packet steps this stream's H.264 goes through (HostPackets); None for a media without them.
    pub fn tap(&self) -> Option<Arc<dyn PacketTap>> {
        self.shared.tap.clone()
    }

    pub fn slot(&self) -> Arc<dyn FrameSlot> {
        self.shared.slot.clone()
    }

    /// Swap the plugin steps from the next frame on (the panels moved to another stream, or a plugin changed its step).
    pub fn set_steps(&self, steps: Vec<StagedStep>) {
        *self.shared.next_steps.lock().unwrap() = Some(steps);
    }

    /// Swap the colour step from the next frame on, the other steps left as they are (a LUT baked for a stream behind the panels).
    pub fn set_colour(&self, steps: Vec<FrameStep>) {
        *self.shared.next_colour.lock().unwrap() = Some(steps);
    }

    /// Update stream parameters live (None keeps one; Some(None) passes the frame size through). A new fps restarts
    /// the virtual camera; the same one again doesn't, so it doesn't flicker.
    pub fn update_output(&self, width: Option<Option<u32>>, height: Option<Option<u32>>, fps: Option<u32>) {
        let mut size = self.shared.size.lock().unwrap();
        if let Some(w) = width {
            size.0 = w;
        }
        if let Some(h) = height {
            size.1 = h;
        }
        if let Some(fps) = fps.filter(|f| *f > 0 && *f != self.shared.fps.load(Ordering::SeqCst)) {
            self.shared.fps.store(fps, Ordering::SeqCst);
            self.settle();
            self.shared.restart_vcam.set();
        }
    }

    /// The phone is restarting its camera (a lens switch, a new size): hold off judging the link until it's back.
    pub fn settle(&self) {
        self.shared.settle.lock().unwrap().restart(Instant::now());
    }

    /// The phone is taking a photo, which can pause its video: for a while a stream that stops is reconnected quietly.
    pub fn hold_for_photo(&self, seconds: f32) {
        *self.shared.photo_until.lock().unwrap() = Some(Instant::now() + Duration::from_secs_f32(seconds));
        self.settle();
    }

    pub fn request_stop(&self) {
        let s = &self.shared;
        s.stop.store(true, Ordering::SeqCst);
        s.restart_vcam.set();
        if let Some(stopper) = s.stopper.lock().unwrap().as_ref() {
            stopper.stop();
        }
    }

    /// Stops and waits for its threads, up to a few seconds.
    pub fn stop(&self) {
        self.request_stop();
        if let Some(t) = self.thread.lock().unwrap().take() {
            let deadline = Instant::now() + Duration::from_secs(4);
            while !t.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if t.is_finished() {
                let _ = t.join();
            } else {
                log::warn!("A stream worker didn't stop in time; leaving it to finish by itself");
            }
        }
    }

    pub fn parked(&self) -> bool {
        self.shared.parked.load(Ordering::SeqCst)
    }

    /// Let go of the virtual camera while the stream is gone (so the wait screen can show), or take it back.
    pub fn set_parked(&self, parked: bool) {
        self.shared.parked.store(parked, Ordering::SeqCst);
        self.shared.restart_vcam.set();
        if parked {
            self.shared.vcam_free.wait(Duration::from_secs(1)); // the wait screen opens it next; it retries if this ran late
        }
    }

    /// Reconnect through opener (at url) from the next attempt on (the phone moved to another route), and try now.
    pub fn retarget(&self, url: &str, opener: Opener) {
        *self.shared.opener.lock().unwrap() = opener;
        *self.shared.url.lock().unwrap() = url.into();
        self.shared.retry_now.store(true, Ordering::SeqCst);
    }

    /// Whether hand_over() would try now: a phone's stream that isn't stopping or trying another route already.
    pub fn can_hand_over(&self) -> bool {
        let s = &self.shared;
        !s.plugin_source && !s.stop.load(Ordering::SeqCst) && !s.handover_busy.load(Ordering::SeqCst)
    }

    /// What later reconnects open with, after a hand-over moved the stream (its URL moved with it).
    pub fn set_opener(&self, opener: Opener) {
        *self.shared.opener.lock().unwrap() = opener;
    }

    pub fn failed_tries(&self) -> u32 {
        self.shared.failed_tries.load(Ordering::SeqCst)
    }

    pub fn last_arrival_fps(&self) -> f32 {
        *self.shared.last_arrival_fps.lock().unwrap()
    }

    /// (width, height) the virtual camera last opened at.
    pub fn camera_size(&self) -> Option<(u32, u32)> {
        *self.shared.camera_size.lock().unwrap()
    }

    pub fn set_preview_size(&self, width: u32, height: u32) {
        self.shared.preview_size.store(((width as u64) << 32) | height as u64, Ordering::Relaxed);
    }

    /// Open url through opener next to the running stream and move over once it has kept frames coming for
    /// HANDOVER_HEALTHY; only then does the old connection close. Moved or MoveFailed says how it went.
    pub fn hand_over(&self, url: &str, opener: Opener) -> bool {
        let s = &self.shared;
        if !self.can_hand_over() {
            return false;
        }
        if s.handover_busy.swap(true, Ordering::SeqCst) {
            return false;
        }
        let (shared, url) = (self.shared.clone(), url.to_string());
        let t = std::thread::Builder::new().name("stream-handover".into())
            .spawn(move || shared.try_handover(url, opener));
        match t {
            Ok(t) => {
                *s.handover_thread.lock().unwrap() = Some(t);
                true
            }
            Err(_) => {
                s.handover_busy.store(false, Ordering::SeqCst);
                false
            }
        }
    }
}

impl Shared {
    fn status(&self, kind: &str, msg: &str) {
        (self.events)(WorkerEvent::Status(kind.into(), msg.into()));
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn join_handover(&self) {
        let t = self.handover_thread.lock().unwrap().take();
        if let Some(t) = t {
            let deadline = Instant::now() + HANDOVER_TAKE + Duration::from_secs(1);
            while !t.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
    }

    fn open_cap_with(&self, opener: &Opener) -> Result<Cap, String> {
        let Opened { codec, feed, own_decoder } = opener()?;
        let options = DecodeOptions { hardware: self.hardware };
        // Packets only the source understands go through its decoder, one at a time like JPEGs, newest first
        if let Some(decoder) = own_decoder {
            return Ok(Cap::Jpegs { feed, decoder, own: true });
        }
        match codec {
            Codec::Mjpeg => Ok(Cap::Jpegs { feed, decoder: self.media.decoder(codec, &options)?, own: false }),
            _ => {
                let mut decoder = match &self.tap {
                    Some(tap) => self.media.decoder_with_tap(codec, &options, tap.clone())?,
                    None => self.media.decoder(codec, &options)?,
                };
                let events = self.events.clone();
                decoder.on_keyframe_needed(Box::new(move || events(WorkerEvent::NeedsKeyframe)));
                Ok(Cap::Frames(self.media.frame_reader(feed, decoder)?))
            }
        }
    }

    fn open_cap(&self) -> Result<Cap, String> {
        let opener = self.opener.lock().unwrap().clone();
        let cap = self.open_cap_with(&opener)?;
        *self.stopper.lock().unwrap() = Some(cap.stopper());
        if self.stopped() {
            cap.stopper().stop();
        }
        Ok(cap)
    }

    fn count_arrived(&self, frames: u32, bytes: u64) {
        self.bytes_total.fetch_add(bytes, Ordering::Relaxed);
        self.frames_arrived.fetch_add(frames as u64, Ordering::Relaxed);
        self.settle.lock().unwrap().arrived(Instant::now());
    }

    /// Resize and run the plugin steps on frame, then make it the frame the camera sends. One frame at a time, and
    /// never one older than what's already out. Whether it went out: a frame that breaks on the way is dropped.
    fn publish(&self, seq: u64, frame: Frame) -> bool {
        let mut publish = self.publish.lock().unwrap();
        if seq <= publish.1 {
            return false;
        }
        let pipeline = publish.0.as_mut();
        let arrived = frame.time_ns;
        let Ok(out) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.prepare(pipeline, seq, frame))) else {
            log::error!("Frame processing failed; dropping this frame");
            return false;
        };
        self.newest_arrival.store(arrived, Ordering::Relaxed);
        self.slot.publish(out);
        publish.1 = seq;
        true
    }

    /// _prepare: the steps as they are now, the frame at its size, and the frame through the steps.
    fn prepare(&self, pipeline: &mut dyn FramePipeline, seq: u64, frame: Frame) -> Frame {
        let next_steps = self.next_steps.lock().unwrap().take();
        let replaced = next_steps.is_some();
        if let Some(steps) = next_steps {
            pipeline.set_steps(steps);
        }
        if let Some(steps) = self.next_colour.lock().unwrap().take() {
            pipeline.replace_steps("colour", telescope_core::frame::FrameStage::Colour, steps);
        }
        let (rw, rh) = *self.size.lock().unwrap();
        let mut frame = match (rw, rh) {
            (None, None) => frame,
            _ => stretch(&frame, rw.unwrap_or(frame.width), rh.unwrap_or(frame.height)),
        };
        // Its number before the steps see it: what a person mask is worked out once by (slot.publish numbers what goes out)
        frame.number = seq;
        if let Some(view) = self.gate.as_ref().and_then(|g| g.for_frame(&self.source_id, &frame, crate::shell::now_s(), replaced)) {
            pipeline.replace_steps("view", telescope_core::frame::FrameStage::View, telescope_core::view_step::steps(&view));
        }
        if let Some(feed) = &self.vision {
            let size = (frame.width, frame.height);
            let mut sizes = feed.frame_sizes.lock().unwrap();
            if sizes.get(&self.source_id) != Some(&size) {
                sizes.insert(self.source_id.clone(), size);
            }
        }
        // vision looks for people in the frame before the view (view_controller.process_frame)
        if let Some(feed) = self.vision.as_ref().filter(|f| f.service.wants_people()) {
            let upright = *feed.upright.lock().unwrap();
            feed.service.feed_raw(&self.source_id, &frame, upright);
        }
        if let Some(motion) = self.vision.as_ref().and_then(|f| f.motion.as_ref()).filter(|m| m.watching()) {
            let upright = *self.vision.as_ref().map(|f| f.upright.lock().unwrap()).expect("a feed");
            motion.feed(&frame, &self.source_id, Some(&upright), telescope_core::time::monotonic_ns() as f64 / 1e9);
        }
        let ctx = Ctx { source_id: self.source_id.clone(), vision: self.vision.as_ref().map(|f| f.service.clone()) };
        pipeline.process(frame, &ctx)
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst)
    }

    fn run(self: &Arc<Self>) {
        let url = self.url.lock().unwrap().clone();
        self.status("info", &if self.plugin_source { tr("Connecting...") } else { fill(&tr("Connecting to {url}..."), &[("url", &url)]) });
        let mut failed = false; // the phone may be reachable another way by now, so the host looks for it (see "waiting")
        while !self.stopped() {
            let mut cap = match self.open_cap() {
                Ok(cap) => cap,
                Err(e) => {
                    log::info!("Opening the stream failed: {e}");
                    failed = true;
                    let secs = RECONNECT_DELAY.as_secs().to_string();
                    let own = self.waiting_text.as_ref().map(|f| f()).filter(|t| !t.is_empty());
                    self.status("waiting", &own.unwrap_or_else(|| fill(&tr("Can't reach the phone's stream. Trying again in {seconds} s…"), &[("seconds", &secs)])));
                    self.restart_vcam.wait(RECONNECT_DELAY);
                    self.restart_vcam.clear();
                    continue;
                }
            };
            let first = match cap.read_frame() {
                Ok(f) => f,
                Err(FeedEnd::Stopped) => break,
                Err(_) => {
                    drop(cap);
                    failed = true;
                    self.status("waiting", &tr("Waiting for the first frame…"));
                    self.restart_vcam.wait(RECONNECT_DELAY);
                    self.restart_vcam.clear();
                    continue;
                }
            };
            self.count_arrived(first.2, first.1);
            // Run the first frame through the steps so the camera's size accounts for them (a 90° turn swaps sides).
            let seq = self.next_seq();
            if !self.publish(seq, first.0) || self.slot.latest().is_none() {
                drop(cap);
                self.restart_vcam.wait(RECONNECT_DELAY);
                self.restart_vcam.clear();
                continue;
            }
            if failed {
                failed = false;
                (self.events)(WorkerEvent::Reconnected);
            }
            let reader_done = Arc::new(Flag::default());
            let reader = {
                let (s, done) = (self.clone(), reader_done.clone());
                std::thread::Builder::new().name("stream-reader".into()).spawn(move || {
                    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.read_frames(cap)));
                    if caught.is_err() {
                        log::error!("Stream reader crashed; reconnecting");
                    }
                    // Wake the camera loop so a reader that died doesn't leave the last frame frozen on the camera
                    done.set();
                    s.restart_vcam.set();
                }).expect("a thread for the stream reader")
            };
            while !self.stopped() && !reader_done.is_set() {
                self.restart_vcam.clear();
                if self.parked.load(Ordering::SeqCst) {
                    self.restart_vcam.wait(RECONNECT_DELAY);
                    continue;
                }
                let failed_camera = !self.run_vcam(&reader_done);
                if self.stopped() {
                    break;
                }
                if failed_camera && !self.restart_vcam.is_set() {
                    // The camera failed rather than being asked to restart; back off before reopening it.
                    self.restart_vcam.wait(RECONNECT_DELAY);
                }
            }
            if let Some(stopper) = self.stopper.lock().unwrap().as_ref() {
                stopper.stop();
            }
            let _ = reader.join();
            if self.stopped() {
                break;
            }
        }
    }

    /// Pull frames off the wire as fast as they come, reconnecting when the stream drops; returns at a stop.
    fn read_frames(self: &Arc<Self>, mut cap: Cap) {
        loop {
            if self.stopped() {
                return;
            }
            if let Some(moved) = self.take_handover() {
                cap = moved;
                continue;
            }
            let ended = match &mut cap {
                Cap::Frames(reader) => self.read_h264(reader.as_mut()),
                Cap::Jpegs { feed, own: false, .. } => self.read_jpegs(feed.as_mut(), &mut jpeg_decoders(self)),
                Cap::Jpegs { feed, decoder, own: true } => {
                    // Its decoder goes to the decoding thread; a reconnect opens the feed again and asks for a new one
                    self.read_jpegs(feed.as_mut(), &mut vec![std::mem::replace(decoder, Box::new(NoDecoder))])
                }
            };
            match ended {
                Ended::Stopped => return,
                Ended::Moved(new) => {
                    cap = new;
                    continue;
                }
                Ended::Dropped => {}
            }
            let Some(new) = self.reconnect() else { return };
            cap = new;
            self.settle.lock().unwrap().restart(Instant::now());
            self.status("ok", &tr("Stream reconnected"));
            (self.events)(WorkerEvent::Reconnected);
        }
    }

    fn read_h264(&self, reader: &mut dyn FrameReader) -> Ended {
        loop {
            if self.stopped() {
                return Ended::Stopped;
            }
            if let Some(moved) = self.take_handover() {
                return Ended::Moved(moved);
            }
            match reader.read() {
                Ok(f) => {
                    self.count_arrived(f.count, f.bytes);
                    let seq = self.next_seq();
                    self.publish(seq, f.frame);
                }
                Err(FeedEnd::Stopped) if self.stopped() => return Ended::Stopped,
                Err(e) => {
                    log::info!("The stream ended: {e:?}");
                    return Ended::Dropped;
                }
            }
        }
    }

    /// JPEGs: the newest packet goes to the decoders; one nobody took in time is dropped, so a slow decoder skips
    /// frames instead of letting them queue up as lag.
    fn read_jpegs(self: &Arc<Self>, feed: &mut dyn VideoFeed, decoders: &mut Vec<Box<dyn Decoder>>) -> Ended {
        let newest: Arc<Newest<(u64, telescope_core::media::Packet)>> = Arc::default();
        let threads: Vec<_> = decoders.drain(..).map(|mut decoder| {
            let (s, newest) = (self.clone(), newest.clone());
            std::thread::spawn(move || {
                while let Some((seq, packet)) = newest.take() {
                    if let Some(frame) = decoder.decode(&packet).newest {
                        s.publish(seq, frame);
                    }
                }
            })
        }).collect();
        let ended = loop {
            if self.stopped() {
                break Ended::Stopped;
            }
            if let Some(moved) = self.take_handover() {
                break Ended::Moved(moved);
            }
            match feed.read() {
                Ok(packet) => {
                    self.count_arrived(1, packet.data.len() as u64);
                    newest.put((self.next_seq(), packet));
                }
                Err(FeedEnd::Stopped) if self.stopped() => break Ended::Stopped,
                Err(_) => break Ended::Dropped,
            }
        };
        newest.finish();
        for t in threads {
            let _ = t.join();
        }
        ended
    }

    fn reconnect(self: &Arc<Self>) -> Option<Cap> {
        let photo = self.photo_until.lock().unwrap().is_some_and(|until| Instant::now() < until);
        if photo {
            self.status("info", &tr("The video is paused while the phone takes a photo"));
        } else {
            self.status("reconnecting", &tr("Stream dropped - reconnecting"));
        }
        while !self.stopped() {
            let until = Instant::now() + DROP_RETRY_DELAY;
            while Instant::now() < until {
                if self.stopped() {
                    return None;
                }
                if self.retry_now.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            self.retry_now.store(false, Ordering::SeqCst);
            if let Some(moved) = self.take_handover() {
                return Some(moved);
            }
            let url = self.url.lock().unwrap().clone();
            if let Ok(mut cap) = self.open_cap() {
                if let Ok((frame, bytes, count)) = cap.read_frame() {
                    self.count_arrived(count, bytes);
                    let seq = self.next_seq();
                    self.publish(seq, frame);
                    return Some(cap);
                }
            }
            if url == *self.url.lock().unwrap() {
                self.failed_tries.fetch_add(1, Ordering::SeqCst); // a retarget() meanwhile makes it a try on the old url
            }
        }
        None
    }

    fn try_handover(self: &Arc<Self>, url: String, opener: Opener) {
        let taken = Arc::new(Flag::default());
        let mut handed = false;
        match self.open_cap_with(&opener) {
            Ok(mut cap) => {
                if self.healthy(&mut cap) {
                    *self.handover_ready.lock().unwrap() = Some((cap, url.clone(), taken.clone()));
                    let give_up = Instant::now() + HANDOVER_TAKE;
                    while !taken.wait(Duration::from_millis(100)) {
                        if self.stopped() || Instant::now() >= give_up {
                            break; // a stop waits for this thread, so it doesn't wait out the rest
                        }
                    }
                    let mut ready = self.handover_ready.lock().unwrap();
                    if ready.as_ref().is_some_and(|r| Arc::ptr_eq(&r.2, &taken)) {
                        *ready = None;
                    }
                    handed = taken.is_set();
                }
            }
            Err(e) => log::info!("Trying the new route failed: {e}"),
        }
        self.handover_busy.store(false, Ordering::SeqCst);
        if !handed && !self.stopped() {
            (self.events)(WorkerEvent::MoveFailed(url));
        }
    }

    /// Read (and decode, so it's ready to carry on) until frames have come without a gap for a while.
    fn healthy(&self, cap: &mut Cap) -> bool {
        let start = Instant::now();
        let (mut since, mut last) = (start, start);
        while !self.stopped() && last - start < HANDOVER_TIMEOUT {
            if cap.read_frame().is_err() {
                return false;
            }
            let now = Instant::now();
            if now - last > HANDOVER_GAP {
                since = now; // a gap: the time without one starts over
            }
            last = now;
            if now - since >= HANDOVER_HEALTHY {
                return true;
            }
        }
        false
    }

    /// The new route's reader, if one is ready to take over; the stream moves to it.
    fn take_handover(&self) -> Option<Cap> {
        let (cap, url, taken) = self.handover_ready.lock().unwrap().take()?;
        taken.set();
        *self.url.lock().unwrap() = url.clone();
        *self.stopper.lock().unwrap() = Some(cap.stopper());
        self.settle.lock().unwrap().restart(Instant::now());
        log::info!("Stream moved to {url}");
        (self.events)(WorkerEvent::Moved(url));
        Some(cap)
    }

    /// Open the virtual camera at the current size and fps and feed it until a stop or a restart. False if it failed.
    fn run_vcam(&self, reader_done: &Flag) -> bool {
        let Some((src0, _)) = self.slot.latest() else { return false };
        let cam_w = self.canvas.0.unwrap_or(src0.width);
        let cam_h = self.canvas.1.unwrap_or(src0.height);
        // An app already reading the camera (the wait screen's) keeps it at that size; frames are fitted to it.
        let (cam_w, cam_h) = self.outputs.locked_size(self.slot_number).unwrap_or((cam_w, cam_h));
        let fps = self.fps.load(Ordering::SeqCst);
        self.vcam_free.clear();
        let result = self.outputs.open(self.slot_number, cam_w, cam_h, fps);
        let mut cam: Box<dyn VideoOutput> = match result {
            Ok(cam) => cam,
            Err(e) => {
                self.vcam_free.set();
                self.status("warn", &fill(&tr("Virtual camera error: {error}"), &[("error", &e)]));
                return false;
            }
        };
        let nv12 = !self.outputs.formats().contains(&PixelFormat::I420) && self.outputs.formats().contains(&PixelFormat::Nv12);
        // Name the camera the way other apps list it.
        let shown_as = self.outputs.slots().into_iter().find(|s| s.slot == self.slot_number).map(|s| s.name)
            .unwrap_or_else(|| cam.device());
        *self.camera_size.lock().unwrap() = Some((cam_w, cam_h));
        (self.events)(WorkerEvent::VcamOpened(cam_w, cam_h));
        let size = format!("{cam_w} × {cam_h}");
        self.status("ok", &fill(&tr("Streaming {size} at {fps} fps to {camera}"),
                                &[("size", &size), ("fps", &fps.to_string()), ("camera", &shown_as)]));
        let period = Duration::from_secs_f64(1.0 / fps as f64);
        let (mut t0, mut bytes0, mut recv0) = (Instant::now(), self.bytes_total.load(Ordering::Relaxed), self.frames_arrived.load(Ordering::Relaxed));
        let mut weak_streak = 0;
        let mut last_sent: Option<Instant> = None;
        let mut last_preview = Instant::now() - Duration::from_secs(1);
        let (mut seen, mut fitted): (u64, Option<Frame>) = (0, None);
        let mut scaler = None;
        let mut ok = true;
        let mut delivery_log = telescope_core::time::DeliveryLog::from_env();
        while !self.stopped() && !self.restart_vcam.is_set() && !self.parked.load(Ordering::SeqCst) && !reader_done.is_set() {
            // A new frame goes out as soon as it's ready. The last one is resent only after two periods without one,
            // so a frame that's a little late isn't held back behind a resend.
            let wait = last_sent.map_or(Duration::ZERO, |t| (t + 2 * period).saturating_duration_since(Instant::now()));
            self.slot.wait_newer(seen, wait.min(Duration::from_millis(100)).max(Duration::from_millis(1)));
            if let Some(t) = last_sent {
                // A burst from the phone is paced to at most twice the camera's rate.
                let pace = (t + period / 2).saturating_duration_since(Instant::now());
                if !pace.is_zero() {
                    self.restart_vcam.wait(pace);
                }
            }
            if let Some((src, count)) = self.slot.latest() {
                let due = last_sent.is_none_or(|t| t.elapsed() >= 2 * period);
                let new_frame = count != seen || fitted.is_none();
                if new_frame {
                    seen = count;
                    let fit = fit_frame(&src, cam_w, cam_h);
                    fitted = Some(if nv12 { to_nv12(&fit) } else { fit });
                } else if !due {
                    continue;
                }
                if let Err(e) = cam.send(fitted.as_ref().unwrap()) {
                    self.status("warn", &fill(&tr("Virtual camera error: {error}"), &[("error", &e)]));
                    ok = false;
                    break;
                }
                if let (true, Some(log)) = (new_frame, delivery_log.as_mut()) {
                    log.delivered(src.number, self.newest_arrival.load(Ordering::Relaxed), src.time_ns);
                }
                last_sent = Some(Instant::now());
                let preview = self.preview_size.load(Ordering::Relaxed);
                if preview != 0 && last_preview.elapsed() >= Duration::from_millis(33) {
                    last_preview = Instant::now();
                    let (pw, ph) = fit_inside((preview >> 32) as u32, preview as u32, src.width, src.height);
                    let scaler = scaler.get_or_insert_with(|| self.media.scaler());
                    (self.events)(WorkerEvent::Preview(scaler.rgb(&src, pw, ph), pw, ph));
                }
            }
            let elapsed = t0.elapsed();
            if elapsed >= Duration::from_secs(2) {
                if cam.outdated() {
                    self.restart_vcam.set(); // the camera was installed or updated while streaming
                }
                let secs = elapsed.as_secs_f64();
                let (src_w, src_h) = self.slot.latest().map_or((cam_w, cam_h), |(f, _)| (f.width, f.height));
                // Frames that came from the phone, not what the camera was sent: resends of the last frame padded that out.
                let recv_now = self.frames_arrived.load(Ordering::Relaxed);
                let arrival_fps = (recv_now - recv0) as f64 / secs;
                *self.last_arrival_fps.lock().unwrap() = arrival_fps as f32;
                self.status("fps", &format!("{arrival_fps:.1} fps  {src_w}x{src_h}"));
                let bytes_now = self.bytes_total.load(Ordering::Relaxed);
                let mbps = (bytes_now - bytes0) as f64 * 8.0 / secs / 1_000_000.0;
                // Warn only if frames sustainedly arrive well under the target rate. A window that started while the
                // phone was still settling says nothing about the link, and nothing at all is a dropped link.
                let judged = self.settle.lock().unwrap().judges(t0);
                let struggling = judged && arrival_fps > 0.0 && arrival_fps < fps as f64 * 0.85;
                weak_streak = if struggling { weak_streak + 1 } else { 0 };
                let net_kind = if weak_streak >= 2 { "net_warn" } else { "net" };
                // Frames but no bytes means the reader can't tell
                let counted = bytes_now > bytes0 || recv_now == recv0;
                self.status(net_kind, &if counted { format!("{mbps:.1} Mbps") } else { "—".into() });
                (t0, bytes0, recv0) = (Instant::now(), bytes_now, recv_now);
            }
        }
        drop(cam);
        self.vcam_free.set();
        ok
    }
}

/// Stands in for a source's decoder once it's gone to the decoding thread.
struct NoDecoder;

impl Decoder for NoDecoder {
    fn decode(&mut self, _packet: &telescope_core::media::Packet) -> telescope_core::media::Decoded {
        telescope_core::media::Decoded { newest: None, count: 0 }
    }
    fn hardware(&self) -> bool {
        false
    }
}

/// Frames through unchanged, for a media without frame steps.
struct NoSteps;

impl FramePipeline for NoSteps {
    fn set_steps(&mut self, _steps: Vec<StagedStep>) {}
    fn process(&mut self, frame: Frame, _ctx: &dyn FrameContext) -> Frame {
        frame
    }
    fn is_empty(&self) -> bool {
        true
    }
}

enum Ended {
    Stopped,
    Dropped,
    Moved(Cap),
}

/// The decoders for a JPEG feed: one, or two side by side on a computer with cores to spare.
fn jpeg_decoders(s: &Shared) -> Vec<Box<dyn Decoder>> {
    let options = DecodeOptions { hardware: s.hardware };
    (0..mjpeg_decoders()).filter_map(|_| s.media.decoder(Codec::Mjpeg, &options).ok()).collect()
}

/// frame scaled to exactly width x height (cv2.resize), for a stream asked to pass on a set size.
fn stretch(frame: &Frame, width: u32, height: u32) -> Frame {
    if (frame.width, frame.height) == (width, height) || frame.format != PixelFormat::I420 {
        return frame.clone();
    }
    let mut out = vec![0u8; PixelFormat::I420.size(width, height)];
    let (sy, su, sv) = frame.i420_planes().unwrap();
    let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
    let (oy, rest) = out.split_at_mut((width * height) as usize);
    let (ou, ov) = rest.split_at_mut(cw * ch);
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    resize_linear_into(sy, fw, fh, 1, oy, width as usize, (0, 0, width as usize, height as usize));
    resize_linear_into(su, fw.div_ceil(2), fh.div_ceil(2), 1, ou, cw, (0, 0, cw, ch));
    resize_linear_into(sv, fw.div_ceil(2), fh.div_ceil(2), 1, ov, cw, (0, 0, cw, ch));
    let mut f = Frame::new(width, height, PixelFormat::I420, out);
    f.number = frame.number;
    f.time_ns = frame.time_ns;
    f
}

/// The largest size with the frame's shape that fits in (w, h), even on both sides.
fn fit_inside(w: u32, h: u32, fw: u32, fh: u32) -> (u32, u32) {
    let scale = (w as f32 / fw as f32).min(h as f32 / fh as f32).min(1.0);
    (((fw as f32 * scale) as u32 & !1).max(2), ((fh as f32 * scale) as u32 & !1).max(2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use telescope_core::media::{Decoded, Packet, ReadFrame, Scaler};
    use telescope_core::output::Slot as OutSlot;

    type Log = Arc<Mutex<Vec<WorkerEvent>>>;

    fn grey(w: u32, h: u32, y: u8) -> Frame {
        let mut data = vec![y; (w * h) as usize];
        data.extend(vec![128; 2 * (w.div_ceil(2) * h.div_ceil(2)) as usize]);
        Frame::new(w, h, PixelFormat::I420, data)
    }

    #[test]
    fn fit_frame_returns_matching_frame_without_copy() {
        let frame = grey(6, 4, 1);
        let fitted = fit_frame(&frame, 6, 4);
        assert!(Arc::ptr_eq(&fitted.data, &frame.data));
    }

    /// A plain colour: distinct planes, so a check can't pass on luma alone.
    fn tinted(w: u32, h: u32, y: u8, u: u8, v: u8) -> Frame {
        let chroma = (w.div_ceil(2) * h.div_ceil(2)) as usize;
        let mut data = vec![y; (w * h) as usize];
        data.extend(vec![u; chroma]);
        data.extend(vec![v; chroma]);
        Frame::new(w, h, PixelFormat::I420, data)
    }

    #[test]
    fn fit_frame_resizes_same_aspect_ratio() {
        let fitted = fit_frame(&tinted(4, 2, 17, 40, 200), 8, 4);
        assert_eq!((fitted.width, fitted.height, fitted.format), (8, 4, PixelFormat::I420));
        let (y, u, v) = fitted.i420_planes().unwrap();
        assert!(y.iter().all(|&p| p == 17) && u.iter().all(|&p| p == 40) && v.iter().all(|&p| p == 200));
    }

    // What cv2.resize(.., INTER_LINEAR) makes of each plane of a 6 x 4 picture
    #[test]
    fn a_stretched_frame_is_each_plane_through_cv2s_linear_resize() {
        let y = [139, 74, 229, 241, 169, 65, 6, 160, 149, 106, 38, 175, 188, 205, 175, 229, 98, 249, 10, 148, 95, 86, 147, 198];
        let u = [10, 60, 200, 90, 30, 250];
        let frame = Frame::new(6, 4, PixelFormat::I420, [&y[..], &u, &u].concat());
        let out = stretch(&frame, 4, 3);
        let (oy, ou, ov) = out.i420_planes().unwrap();
        assert_eq!(oy, [110, 184, 201, 99, 118, 167, 143, 176, 69, 120, 117, 189]);
        assert_eq!((ou, ov), (&[23, 165, 75, 195][..], &[23, 165, 75, 195][..]));
        let wide = fit_frame(&frame, 9, 6);
        assert_eq!(wide.data, stretch(&frame, 9, 6).data, "a fitted one is the same resize");
    }

    #[test]
    fn fit_frame_letterboxes() {
        let fitted = fit_frame(&tinted(8, 4, 255, 40, 200), 8, 8);
        assert_eq!((fitted.width, fitted.height, fitted.format), (8, 8, PixelFormat::I420));
        let (y, u, v) = fitted.i420_planes().unwrap();
        assert!(y[..16].iter().all(|&p| p == 16));
        assert!(y[16..48].iter().all(|&p| p == 255));
        assert!(y[48..].iter().all(|&p| p == 16));
        // Black bars, so no colour in them, and the picture's colour between
        for plane in [(u, 40), (v, 200)] {
            assert_eq!(plane.0.chunks(4).map(|r| r[0]).collect::<Vec<_>>(), [128, plane.1, plane.1, 128]);
            assert!(plane.0.chunks(4).all(|r| r.iter().all(|&p| p == r[0])));
        }
    }

    #[test]
    fn fit_frame_pillarboxes_narrow_input() {
        let fitted = fit_frame(&tinted(4, 8, 9, 40, 200), 8, 8);
        assert_eq!((fitted.width, fitted.height, fitted.format), (8, 8, PixelFormat::I420));
        let (y, u, v) = fitted.i420_planes().unwrap();
        for row in y.chunks(8) {
            assert_eq!(row, [16, 16, 9, 9, 9, 9, 16, 16]);
        }
        for (plane, value) in [(u, 40), (v, 200)] {
            for row in plane.chunks(4) {
                assert_eq!(row, [128, value, value, 128]);
            }
        }
    }

    #[test]
    fn nv12_interleaves_the_chroma() {
        let mut f = grey(2, 2, 5);
        let d = f.data_mut();
        d[4] = 1;
        d[5] = 2;
        assert_eq!(*to_nv12(&f).data, vec![5, 5, 5, 5, 1, 2]);
    }

    struct Clock(Instant);

    impl Clock {
        fn flow(&mut self, settle: &mut Settle, seconds: f32) {
            for _ in 0..(seconds * 10.0).round() as u32 {
                self.0 += Duration::from_millis(100);
                settle.arrived(self.0);
            }
        }
    }

    #[test]
    fn the_link_is_judged_only_once_frames_have_flowed_for_a_moment_after_start() {
        let mut clock = Clock(Instant::now());
        let mut settle = Settle::new(clock.0);
        clock.0 += Duration::from_secs(4); // a slow phone: nothing for a while
        settle.arrived(clock.0);
        assert!(!settle.judges(clock.0)); // frames only just started
        clock.flow(&mut settle, SETTLE_AFTER_FRAMES.as_secs_f32() - 0.1);
        assert!(!settle.judges(clock.0));
        clock.flow(&mut settle, 0.1);
        assert!(!settle.judges(clock.0 - Duration::from_secs(1))); // a window that started while settling doesn't count
        assert!(settle.judges(clock.0));
        assert!(settle.since.is_none());
    }

    #[test]
    fn old_lens_frames_before_the_gap_do_not_count_as_back() {
        let mut clock = Clock(Instant::now());
        let mut settle = Settle::new(clock.0);
        settle.since = None; // streaming along
        settle.restart(clock.0); // lens switch
        settle.arrived(clock.0); // the old lens's last frame
        clock.0 += Duration::from_secs(3); // an older phone switching
        settle.arrived(clock.0);
        clock.flow(&mut settle, SETTLE_AFTER_FRAMES.as_secs_f32() - 0.5);
        assert!(!settle.judges(clock.0));
        clock.flow(&mut settle, 0.5);
        assert!(settle.judges(clock.0));
    }

    #[test]
    fn a_phone_that_never_gets_going_is_judged_after_the_cap() {
        let start = Instant::now();
        let mut settle = Settle::new(start);
        assert!(!settle.judges(start + SETTLE_MAX - Duration::from_millis(100)));
        assert!(settle.judges(start + SETTLE_MAX));
    }

    #[test]
    fn a_gap_once_settled_is_not_excused() {
        let mut clock = Clock(Instant::now());
        let mut settle = Settle::new(clock.0);
        settle.arrived(clock.0);
        clock.flow(&mut settle, SETTLE_AFTER_FRAMES.as_secs_f32());
        clock.0 += Duration::from_secs(3); // the link stalls after the phone got going
        settle.arrived(clock.0);
        assert!(settle.judges(clock.0 - Duration::from_secs(2)));
    }

    // A media and outputs that need no FFmpeg or v4l2loopback: JPEG "packets" whose first byte is the picture's grey.

    struct Feed {
        packets: Arc<Mutex<VecDeque<Option<u8>>>>,
        stop: Arc<AtomicBool>,
        /// Once the packets run out, this grey every 10 ms.
        endless: Option<u8>,
    }

    struct Stop(Arc<AtomicBool>);

    impl FeedStopper for Stop {
        fn stop(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl VideoFeed for Feed {
        fn read(&mut self) -> Result<Packet, FeedEnd> {
            loop {
                if self.stop.load(Ordering::SeqCst) {
                    return Err(FeedEnd::Stopped);
                }
                match self.packets.lock().unwrap().pop_front() {
                    Some(Some(y)) => return Ok(Packet { data: vec![y], time_ns: 0 }),
                    Some(None) => return Err(FeedEnd::Closed),
                    None => {}
                }
                std::thread::sleep(Duration::from_millis(if self.endless.is_some() { 10 } else { 5 }));
                if let Some(y) = self.endless {
                    return Ok(Packet { data: vec![y], time_ns: 0 });
                }
            }
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            Box::new(Stop(self.stop.clone()))
        }
    }

    struct Jpeg;

    impl Decoder for Jpeg {
        fn decode(&mut self, packet: &Packet) -> Decoded {
            Decoded { newest: Some(grey(4, 2, packet.data[0])), count: 1 }
        }
        fn hardware(&self) -> bool {
            false
        }
    }

    struct Pass;

    impl FramePipeline for Pass {
        fn set_steps(&mut self, _steps: Vec<StagedStep>) {}
        fn process(&mut self, frame: Frame, _ctx: &dyn FrameContext) -> Frame {
            frame
        }
        fn is_empty(&self) -> bool {
            true
        }
    }

    struct Rgb;

    impl Scaler for Rgb {
        fn rgb(&mut self, _frame: &Frame, width: u32, height: u32) -> Vec<u8> {
            vec![0; (width * height * 3) as usize]
        }
    }

    struct FakeMedia;

    impl Media for FakeMedia {
        fn decoder(&self, _codec: Codec, _options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
            Ok(Box::new(Jpeg))
        }
        fn scaler(&self) -> Box<dyn Scaler> {
            Box::new(Rgb)
        }
        fn pipeline(&self) -> Result<Box<dyn FramePipeline>, String> {
            Ok(Box::new(Pass))
        }
    }

    #[derive(Default)]
    struct Cams {
        opened: Mutex<Vec<(u32, u32, u32, u32)>>,
        sent: Mutex<Vec<u8>>,
        sent_at: Mutex<Vec<(u8, Instant)>>,
        /// The frames themselves, whole.
        frames: Mutex<Vec<Frame>>,
        /// "open" and "close" as they happened.
        life: Mutex<Vec<&'static str>>,
        formats: Mutex<Vec<PixelFormat>>,
        /// It takes NV12, like the Windows camera's shared memory.
        nv12: AtomicBool,
        /// What locked_size answers.
        locked: Mutex<Option<(u32, u32)>>,
        /// The open camera reports itself outdated once.
        outdated: AtomicBool,
    }

    struct Cam(Arc<Cams>, (u32, u32));

    impl VideoOutput for Cam {
        fn send(&mut self, frame: &Frame) -> Result<(), String> {
            self.0.sent.lock().unwrap().push(frame.data[0]);
            self.0.sent_at.lock().unwrap().push((frame.data[0], Instant::now()));
            self.0.formats.lock().unwrap().push(frame.format);
            self.0.frames.lock().unwrap().push(frame.clone());
            Ok(())
        }
        fn size(&self) -> (u32, u32) {
            self.1
        }
        fn device(&self) -> String {
            "/dev/video9".into()
        }
        fn outdated(&self) -> bool {
            self.0.outdated.swap(false, Ordering::SeqCst)
        }
    }

    impl Drop for Cam {
        fn drop(&mut self) {
            self.0.life.lock().unwrap().push("close");
        }
    }

    struct FakeOutputs(Arc<Cams>);

    impl Outputs for FakeOutputs {
        fn slots(&self) -> Vec<OutSlot> {
            vec![OutSlot { slot: 0, name: "Telescope".into(), device: "/dev/video9".into() }]
        }
        fn formats(&self) -> Vec<PixelFormat> {
            vec![if self.0.nv12.load(Ordering::SeqCst) { PixelFormat::Nv12 } else { PixelFormat::I420 }]
        }
        fn open(&self, slot: u32, width: u32, height: u32, fps: u32) -> Result<Box<dyn VideoOutput>, String> {
            self.0.opened.lock().unwrap().push((slot, width, height, fps));
            self.0.life.lock().unwrap().push("open");
            Ok(Box::new(Cam(self.0.clone(), (width, height))))
        }
        fn locked_size(&self, _slot: u32) -> Option<(u32, u32)> {
            *self.0.locked.lock().unwrap()
        }
    }

    fn worker(packets: Arc<Mutex<VecDeque<Option<u8>>>>, cams: Arc<Cams>, log: Arc<Mutex<Vec<WorkerEvent>>>) -> StreamWorker {
        worker_from(packets, cams, log, false, None)
    }

    fn worker_from(packets: Arc<Mutex<VecDeque<Option<u8>>>>, cams: Arc<Cams>, log: Arc<Mutex<Vec<WorkerEvent>>>,
                   plugin_source: bool, endless: Option<u8>) -> StreamWorker {
        let opener: Opener = Arc::new(move || {
            Ok((Codec::Mjpeg, Box::new(Feed { packets: packets.clone(), stop: Arc::default(), endless }) as Box<dyn VideoFeed>).into())
        });
        worker_with(opener, None, cams, log, plugin_source)
    }

    fn worker_with(opener: Opener, waiting_text: Option<WaitingText>, cams: Arc<Cams>, log: Arc<Mutex<Vec<WorkerEvent>>>,
                   plugin_source: bool) -> StreamWorker {
        start(WorkerSetup { waiting_text, plugin_source, ..setup(opener) }, Arc::new(FakeMedia), &cams, &log)
    }

    fn setup(opener: Opener) -> WorkerSetup {
        WorkerSetup { url: "http://phone/v1/video".into(), opener, width: None, height: None, fps: 30,
                      canvas: (None, None), slot: 0, source_id: "p".into(), hardware_decode: false, waiting_text: None, vision: None,
                      plugin_source: false, steps: Vec::new(), gate: None }
    }

    fn start(setup: WorkerSetup, media: Arc<dyn Media>, cams: &Arc<Cams>, log: &Log) -> StreamWorker {
        let log = log.clone();
        let events: Events = Arc::new(move |e| {
            if !matches!(e, WorkerEvent::Preview(..)) {
                log.lock().unwrap().push(e)
            }
        });
        let slot: Arc<dyn FrameSlot> = telescope_media_free_slot();
        StreamWorker::start(setup, media, Arc::new(FakeOutputs(cams.clone())), slot, events).unwrap()
    }

    /// A FrameSlot without media's: the newest frame and a count.
    fn telescope_media_free_slot() -> Arc<dyn FrameSlot> {
        #[derive(Default)]
        struct S(Mutex<(Option<Frame>, u64)>, Condvar);
        impl FrameSlot for S {
            fn publish(&self, frame: Frame) -> u64 {
                let mut st = self.0.lock().unwrap();
                st.1 += 1;
                st.0 = Some(frame);
                self.1.notify_all();
                st.1
            }
            fn published(&self) -> Option<Frame> {
                self.0.lock().unwrap().0.clone()
            }
            fn replace(&self, _frame: Frame, _number: u64) -> bool {
                false
            }
            fn latest(&self) -> Option<(Frame, u64)> {
                let st = self.0.lock().unwrap();
                st.0.clone().map(|f| (f, st.1))
            }
            fn wait_newer(&self, seen: u64, timeout: Duration) -> bool {
                let st = self.0.lock().unwrap();
                self.1.wait_timeout_while(st, timeout, |st| st.1 <= seen).unwrap().0.1 > seen
            }
        }
        Arc::new(S::default())
    }

    fn wait_for(what: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !what() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(what(), "timed out");
    }

    #[test]
    fn the_camera_opens_at_the_size_an_app_already_holds_it_at() {
        let (cams, log) = (Arc::new(Cams::default()), Arc::new(Mutex::new(Vec::new())));
        *cams.locked.lock().unwrap() = Some((6, 6));
        let (opener, _opens) = scripted(vec![Some(vec![Step::Frame(10)])]);
        let w = started(WorkerSetup { canvas: (Some(4), Some(4)), ..setup(opener) }, &cams, &log);
        wait_for(|| !cams.frames.lock().unwrap().is_empty());
        w.stop();
        assert_eq!(cams.opened.lock().unwrap()[0], (0, 6, 6, 30));
        let sent = cams.frames.lock().unwrap()[0].clone();
        assert_eq!((sent.width, sent.height, sent.data.len()), (6, 6, 54), "the 4x2 frames are fitted into it");
        assert_eq!(log.lock().unwrap().iter().filter(|e| matches!(e, WorkerEvent::VcamOpened(..))).collect::<Vec<_>>(), [&WorkerEvent::VcamOpened(6, 6)]);
    }

    #[test]
    fn a_camera_replaced_while_streaming_is_opened_again() {
        let (cams, log) = (Arc::new(Cams::default()), Arc::new(Mutex::new(Vec::new())));
        let w = worker_from(Arc::default(), cams.clone(), log, false, Some(1));
        wait_for(|| cams.opened.lock().unwrap().len() == 1);
        cams.outdated.store(true, Ordering::SeqCst);
        let until = Instant::now() + Duration::from_secs(5);
        while cams.opened.lock().unwrap().len() < 2 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        w.stop();
        assert_eq!(cams.opened.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_sources_own_decoder_decodes_what_media_cant() {
        struct Raw;
        impl Decoder for Raw {
            fn decode(&mut self, packet: &Packet) -> Decoded {
                Decoded { newest: Some(grey(6, 2, packet.data[0])), count: 1 }
            }
            fn hardware(&self) -> bool {
                false
            }
        }
        let (cams, log) = (Arc::new(Cams::default()), Arc::new(Mutex::new(Vec::new())));
        let opener: Opener = Arc::new(|| Ok(Opened {
            codec: Codec::Mjpeg,
            feed: Box::new(Feed { packets: Arc::new(Mutex::new(VecDeque::from([Some(40)]))), stop: Arc::default(), endless: None }),
            own_decoder: Some(Box::new(Raw)),
        }));
        let w = worker_with(opener, None, cams.clone(), log, true);
        wait_for(|| cams.sent.lock().unwrap().contains(&40));
        w.stop();
        assert_eq!(cams.opened.lock().unwrap()[0], (0, 6, 2, 30), "the source's decoder made the frame, not media's 4x2 one");
    }

    #[test]
    fn a_source_says_why_it_isnt_streaming_while_it_waits() {
        let (cams, log) = (Arc::new(Cams::default()), Arc::new(Mutex::new(Vec::new())));
        let opener: Opener = Arc::new(|| Err("busy".into()));
        let waiting: WaitingText = Arc::new(|| "Another app is using this camera.".into());
        let w = worker_with(opener, Some(waiting), cams, log.clone(), true);
        wait_for(|| log.lock().unwrap().iter().any(|e| matches!(e, WorkerEvent::Status(k, _) if k == "waiting")));
        w.stop();
        assert!(log.lock().unwrap().contains(&WorkerEvent::Status("waiting".into(), "Another app is using this camera.".into())));
    }

    #[test]
    fn run_streams_frames_to_the_camera_and_stops_cleanly() {
        let (opener, opens) = scripted(vec![Some(vec![Step::Frame(10), Step::Frame(20), Step::Frame(30)])]);
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = started(WorkerSetup { fps: 24, canvas: (Some(4), Some(4)), ..setup(opener) }, &cams, &log);
        let row = |f: &Frame| f.i420_planes().unwrap().0[4]; // the picture starts under the bar
        wait_for(|| cams.frames.lock().unwrap().iter().any(|f| row(f) == 30));
        w.stop();
        assert_eq!(cams.opened.lock().unwrap()[0], (0, 4, 4, 24));
        let sent = cams.frames.lock().unwrap()[0].clone();
        assert_eq!((sent.width, sent.height, sent.format, sent.data.len()), (4, 4, PixelFormat::I420, 24), "the 4x2 frame fitted to the square canvas");
        assert_eq!(sent.i420_planes().unwrap().0[..4], [16; 4], "with its bar");
        assert_eq!(opens.gone(), 1);
        assert_eq!(*cams.life.lock().unwrap().last().unwrap(), "close");
        let log = log.lock().unwrap();
        assert_eq!(log[0], WorkerEvent::Status("info".into(), "Connecting to http://phone/v1/video...".into()));
        assert!(log.contains(&WorkerEvent::VcamOpened(4, 4)));
        assert!(log.contains(&WorkerEvent::Status("ok".into(), "Streaming 4 × 4 at 24 fps to Telescope".into())));
        assert_eq!(log.last().unwrap(), &WorkerEvent::Status("idle".into(), "Not streaming".into()));
    }

    #[test]
    fn a_dropped_stream_reconnects_and_says_so() {
        let (opener, opens) = scripted(vec![Some(vec![Step::Frame(10), Step::End]), Some(vec![Step::Frame(40)])]);
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = started(setup(opener), &cams, &log);
        wait_for(|| log.lock().unwrap().contains(&WorkerEvent::Reconnected));
        wait_for(|| cams.sent.lock().unwrap().contains(&40));
        assert_eq!(opens.gone(), 1, "the first reader was let go of");
        w.stop();
        assert_eq!(opens.gone(), 2, "and so was the second, at the stop");
        let log = log.lock().unwrap();
        assert_eq!(log.iter().filter(|e| **e == WorkerEvent::Reconnected).count(), 1);
        assert!(log.contains(&WorkerEvent::Status("reconnecting".into(), "Stream dropped - reconnecting".into())));
        assert!(log.contains(&WorkerEvent::Status("ok".into(), "Stream reconnected".into())));
        assert_eq!(w.failed_tries(), 0);
    }

    #[test]
    fn the_same_fps_again_does_not_reopen_the_virtual_camera() {
        let packets = Arc::new(Mutex::new(VecDeque::from([Some(1)])));
        let (cams, log) = (Arc::new(Cams::default()), Arc::new(Mutex::new(Vec::new())));
        let w = worker(packets.clone(), cams.clone(), log);
        wait_for(|| cams.opened.lock().unwrap().len() == 1);
        w.update_output(None, None, Some(30));
        assert!(!w.shared.restart_vcam.is_set());
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(cams.opened.lock().unwrap().len(), 1);
        w.stop();
    }

    #[test]
    fn fps_change_rebuilds_the_vcam_without_reopening_the_phone_stream() {
        let (opener, opens) = scripted(vec![Some(flowing(1, 500))]);
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = started(WorkerSetup { fps: 24, canvas: (Some(4), Some(4)), ..setup(opener) }, &cams, &log);
        wait_for(|| cams.sent.lock().unwrap().len() >= 2);
        w.update_output(None, None, Some(15));
        wait_for(|| cams.opened.lock().unwrap().len() == 2);
        let sent = cams.sent.lock().unwrap().len();
        wait_for(|| cams.sent.lock().unwrap().len() > sent);
        std::thread::sleep(DROP_RETRY_DELAY + Duration::from_millis(300)); // long enough for a reconnect to have shown
        assert_eq!(cams.opened.lock().unwrap().iter().map(|o| o.3).collect::<Vec<_>>(), [24, 15]);
        assert_eq!((opens.calls(), opens.gone()), (1, 0), "the same phone connection throughout");
        w.stop();
    }

    #[test]
    fn a_new_fps_settles_the_stream() {
        let (opener, opens) = scripted(vec![Some(vec![])]);
        let (w, _cams, _log) = sitting(opener, &opens);
        w.shared.settle.lock().unwrap().since = None; // streaming along
        w.update_output(None, None, Some(30));
        assert!(w.shared.settle.lock().unwrap().since.is_none(), "the same fps is no change");
        let before = Instant::now();
        w.update_output(None, None, Some(60));
        assert!(w.shared.settle.lock().unwrap().since.is_some_and(|since| since >= before));
        w.stop();
    }

    #[test]
    fn a_frame_that_finishes_after_a_newer_one_is_dropped() {
        let packets = Arc::new(Mutex::new(VecDeque::from([Some(1)])));
        let (cams, log) = (Arc::new(Cams::default()), Arc::new(Mutex::new(Vec::new())));
        let w = worker(packets, cams.clone(), log);
        wait_for(|| cams.opened.lock().unwrap().len() == 1);
        let received = w.shared.slot.latest().unwrap().1;
        assert!(w.shared.publish(1000, grey(4, 2, 7)));
        assert!(!w.shared.publish(999, grey(4, 2, 8)));
        let (frame, count) = w.shared.slot.latest().unwrap();
        assert_eq!((frame.data[0], count), (7, received + 1), "the older one isn't received at all");
        w.stop();
    }

    #[test]
    fn a_parked_worker_lets_go_of_the_camera_and_takes_it_back_when_unparked() {
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = worker_from(Arc::default(), cams.clone(), log.clone(), false, Some(1));
        let life = || cams.life.lock().unwrap().clone();
        wait_for(|| life() == ["open"]);
        w.set_parked(true);
        assert_eq!(life(), ["open", "close"], "let go before set_parked returns, so the wait screen can open it");
        assert!(w.parked());
        let arrived = w.shared.frames_arrived.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(life(), ["open", "close"], "and stays let go");
        assert!(w.shared.frames_arrived.load(Ordering::SeqCst) > arrived, "while the reader carries on");
        w.set_parked(false);
        wait_for(|| life() == ["open", "close", "open"]);
        w.stop();
        assert_eq!(log.lock().unwrap().last(), Some(&WorkerEvent::Status("idle".into(), "Not streaming".into())), "the runner ended");
    }

    #[test]
    fn sources_from_plugins_dont_hand_over() {
        let packets = Arc::new(Mutex::new(VecDeque::new()));
        let (cams, log) = (Arc::new(Cams::default()), Arc::new(Mutex::new(Vec::new())));
        let w = worker_from(packets, cams, log, true, None);
        assert!(!w.hand_over("x", Arc::new(|| Err("no".into()))));
        w.stop();
    }

    fn moves(log: &Log) -> Vec<WorkerEvent> {
        log.lock().unwrap().iter().filter(|e| matches!(e, WorkerEvent::Moved(_) | WorkerEvent::MoveFailed(_))).cloned().collect()
    }

    #[test]
    fn hand_over_fails_on_a_route_that_wont_open() {
        let packets = Arc::new(Mutex::new(VecDeque::from([Some(1)])));
        let (cams, log) = (Arc::new(Cams::default()), Arc::new(Mutex::new(Vec::new())));
        let w = worker(packets, cams, log.clone());
        assert!(w.hand_over("https://usb/v1/video", Arc::new(|| Err("refused".into()))));
        wait_for(|| log.lock().unwrap().contains(&WorkerEvent::MoveFailed("https://usb/v1/video".into())));
        assert_eq!(moves(&log), [WorkerEvent::MoveFailed("https://usb/v1/video".into())], "and it didn't move");
        assert_eq!(w.url(), "http://phone/v1/video");
        w.stop();
    }

    #[test]
    fn hand_over_moves_to_the_new_route_once_it_keeps_frames_coming() {
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let (old, old_opens) = scripted(vec![Some(flowing(1, 1000))]); // the old route keeps going
        let w = started(setup(old), &cams, &log);
        wait_for(|| cams.opened.lock().unwrap().len() == 1);
        let (new, new_opens) = scripted(vec![Some(flowing(77, 1000))]);
        let url = "https://usb/v1/video";
        assert!(w.hand_over(url, new.clone()));
        assert!(!w.hand_over(url, new)); // one at a time
        let until = Instant::now() + HANDOVER_HEALTHY + Duration::from_secs(3);
        while moves(&log).is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(moves(&log), [WorkerEvent::Moved(url.into())]);
        assert_eq!(w.url(), url);
        wait_for(|| cams.sent.lock().unwrap().contains(&77)); // its frames reach the camera
        wait_for(|| old_opens.gone() == 1);
        assert_eq!((new_opens.calls(), new_opens.gone()), (1, 0), "the new reader carries on");
        w.stop();
    }

    // test_stream.py's reader and reconnect checks: a feed told what to do, which says when the worker lets go of it.

    #[derive(Clone)]
    enum Step {
        Frame(u8),
        Sized(u8, usize),
        Pause(Duration),
        End,
        Crash,
    }

    struct Script {
        steps: VecDeque<Step>,
        stop: Arc<AtomicBool>,
        gone: Arc<AtomicU32>,
    }

    impl VideoFeed for Script {
        fn read(&mut self) -> Result<Packet, FeedEnd> {
            loop {
                if self.stop.load(Ordering::SeqCst) {
                    return Err(FeedEnd::Stopped);
                }
                match self.steps.pop_front() {
                    Some(Step::Frame(y)) => return Ok(Packet { data: vec![y], time_ns: 0 }),
                    Some(Step::Sized(y, size)) => return Ok(Packet { data: vec![y; size], time_ns: 0 }),
                    Some(Step::Pause(time)) => std::thread::sleep(time),
                    Some(Step::End) => return Err(FeedEnd::Closed),
                    Some(Step::Crash) => panic!("bad packet"),
                    None => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            Box::new(Stop(self.stop.clone()))
        }
    }

    impl Drop for Script {
        fn drop(&mut self) {
            self.gone.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// How often a scripted opener was asked, and how many of its feeds were let go of.
    #[derive(Clone, Default)]
    struct Opens {
        calls: Arc<AtomicU32>,
        gone: Arc<AtomicU32>,
    }

    impl Opens {
        fn calls(&self) -> u32 {
            self.calls.load(Ordering::SeqCst)
        }
        fn gone(&self) -> u32 {
            self.gone.load(Ordering::SeqCst)
        }
    }

    /// Each try takes the next line: its steps, then it waits to be stopped, or None for a route that won't open. Past the
    /// end they don't open.
    fn scripted(tries: Vec<Option<Vec<Step>>>) -> (Opener, Opens) {
        let (opens, tries) = (Opens::default(), Mutex::new(VecDeque::from(tries)));
        let seen = opens.clone();
        let opener: Opener = Arc::new(move || {
            seen.calls.fetch_add(1, Ordering::SeqCst);
            let steps = tries.lock().unwrap().pop_front().flatten().ok_or("refused")?;
            let feed = Script { steps: steps.into(), stop: Arc::default(), gone: seen.gone.clone() };
            Ok((Codec::Mjpeg, Box::new(feed) as Box<dyn VideoFeed>).into())
        });
        (opener, opens)
    }

    fn flowing(y: u8, frames: usize) -> Vec<Step> {
        (0..frames).flat_map(|_| [Step::Frame(y), Step::Pause(Duration::from_millis(10))]).collect()
    }

    fn started(setup: WorkerSetup, cams: &Arc<Cams>, log: &Log) -> StreamWorker {
        start(setup, Arc::new(FakeMedia), cams, log)
    }

    /// A worker sitting in its first read, for a test to call its reconnect() directly.
    fn sitting(opener: Opener, opens: &Opens) -> (StreamWorker, Arc<Cams>, Log) {
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = started(setup(opener), &cams, &log);
        wait_for(|| opens.calls() == 1);
        (w, cams, log)
    }

    /// DROP_RETRY_DELAY of nothing while it lives: reconnect() tries again straight away.
    struct NoDelay(Arc<AtomicBool>);

    impl Drop for NoDelay {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn no_delay(w: &StreamWorker) -> NoDelay {
        let (done, shared) = (Arc::new(AtomicBool::new(false)), w.shared.clone());
        let over = done.clone();
        std::thread::spawn(move || {
            while !over.load(Ordering::SeqCst) {
                shared.retry_now.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        NoDelay(done)
    }

    fn kinds(log: &Log) -> Vec<String> {
        log.lock().unwrap().iter().filter_map(|e| if let WorkerEvent::Status(kind, _) = e { Some(kind.clone()) } else { None }).collect()
    }

    #[test]
    fn update_output_distinguishes_omitted_from_pass_through() {
        let (opener, opens) = scripted(vec![Some(vec![])]);
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = started(WorkerSetup { width: Some(1280), height: Some(720), ..setup(opener) }, &cams, &log);
        wait_for(|| opens.calls() == 1);
        w.update_output(Some(None), None, None);
        assert_eq!(*w.shared.size.lock().unwrap(), (None, Some(720)));
        assert_eq!(w.shared.fps.load(Ordering::SeqCst), 30);
        assert!(!w.shared.restart_vcam.is_set());
        w.update_output(None, None, Some(60));
        assert_eq!(w.shared.fps.load(Ordering::SeqCst), 60);
        assert!(w.shared.restart_vcam.is_set());
        w.stop();
    }

    #[test]
    fn request_stop_sets_both_stop_signals() {
        let (opener, opens) = scripted(vec![Some(vec![])]);
        let (w, _cams, _log) = sitting(opener, &opens);
        w.request_stop();
        assert!(w.shared.stopped() && w.shared.restart_vcam.is_set());
        wait_for(|| opens.gone() == 1); // the read it was blocked in stops too
        w.stop();
    }

    #[test]
    fn reconnect_returns_first_capture_with_a_readable_frame() {
        let (opener, opens) = scripted(vec![Some(vec![]), None, Some(vec![Step::End]), Some(vec![Step::Frame(9)])]);
        let (w, _cams, _log) = sitting(opener, &opens);
        let fast = no_delay(&w);
        let good = w.shared.reconnect();
        drop(fast);
        assert!(good.is_some());
        assert_eq!(opens.calls(), 4);
        assert_eq!(opens.gone(), 1, "the one without a frame was let go of, the good one wasn't");
        assert_eq!(w.failed_tries(), 2);
        assert_eq!(w.shared.slot.latest().unwrap().0.data[0], 9, "its frame goes out");
        drop(good);
        assert_eq!(opens.gone(), 2);
        w.stop();
    }

    #[test]
    fn retarget_reconnects_to_the_new_url_without_waiting_out_the_delay() {
        let (old, old_opens) = scripted(vec![Some(vec![])]);
        let (new, new_opens) = scripted(vec![Some(vec![Step::Frame(5)])]);
        let (w, _cams, _log) = sitting(old, &old_opens);
        w.retarget("http://10.0.0.5:8080/v1/video", new);
        let started = Instant::now();
        assert!(w.shared.reconnect().is_some());
        assert!(started.elapsed() < DROP_RETRY_DELAY / 2, "{:?}", started.elapsed());
        assert_eq!(w.url(), "http://10.0.0.5:8080/v1/video");
        assert_eq!((old_opens.calls(), new_opens.calls()), (1, 1));
        w.stop();
    }

    #[test]
    fn failed_tries_count_only_the_url_it_was_pointed_at() {
        // A recovery reads it to tell a dead USB forward from one it's still trying
        let worker: Arc<std::sync::OnceLock<(Arc<StreamWorker>, Opener)>> = Arc::default();
        let (tries, inner) = (Arc::new(AtomicU32::new(0)), worker.clone());
        let counted = tries.clone();
        let opener: Opener = Arc::new(move || {
            let n = counted.fetch_add(1, Ordering::SeqCst);
            if n == 2 {
                // Moved while the second try was out
                let (w, me) = inner.get().unwrap();
                w.retarget("http://127.0.0.1:41001/v1/video", me.clone());
            }
            match n {
                0 | 4 => {
                    let feed = Script { steps: VecDeque::from([Step::Frame(1)]), stop: Arc::default(), gone: Arc::default() };
                    Ok((Codec::Mjpeg, Box::new(feed) as Box<dyn VideoFeed>).into())
                }
                _ => Err("refused".into()),
            }
        });
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = Arc::new(started(setup(opener.clone()), &cams, &log));
        assert!(worker.set((w.clone(), opener)).is_ok());
        wait_for(|| tries.load(Ordering::SeqCst) == 1);
        let fast = no_delay(&w);
        assert!(w.shared.reconnect().is_some());
        drop(fast);
        assert_eq!(tries.load(Ordering::SeqCst), 5);
        assert_eq!(w.failed_tries(), 2, "the try out while it moved counts for neither");
        w.stop();
    }

    #[test]
    fn reconnect_stops_without_opening_when_cancelled() {
        let (opener, opens) = scripted(vec![Some(vec![])]);
        let (w, _cams, _log) = sitting(opener, &opens);
        w.request_stop();
        assert!(w.shared.reconnect().is_none());
        assert_eq!(opens.calls(), 1);
        w.stop();
    }

    #[test]
    fn frames_stopping_for_a_photo_reconnect_quietly() {
        let (opener, opens) = scripted(vec![Some(vec![]), Some(vec![Step::Frame(1)]), Some(vec![Step::Frame(2)])]);
        let (w, _cams, log) = sitting(opener, &opens);
        let fast = no_delay(&w);
        log.lock().unwrap().clear();
        w.hold_for_photo(5.0);
        assert!(w.shared.reconnect().is_some());
        *w.shared.photo_until.lock().unwrap() = None; // the hold ran out: a stop is a dropped stream again
        assert!(w.shared.reconnect().is_some());
        drop(fast);
        assert_eq!(kinds(&log), ["info", "reconnecting"]);
        w.stop();
    }

    /// Adds one to the picture, and notes the size it was given.
    struct PlusOne(Arc<Mutex<Option<(u32, u32)>>>);

    impl FramePipeline for PlusOne {
        fn set_steps(&mut self, _steps: Vec<StagedStep>) {}
        fn process(&mut self, mut frame: Frame, _ctx: &dyn FrameContext) -> Frame {
            *self.0.lock().unwrap() = Some((frame.width, frame.height));
            frame.data_mut().iter_mut().for_each(|b| *b += 1);
            frame
        }
        fn is_empty(&self) -> bool {
            false
        }
    }

    /// Frames in a colour, with a pipeline of its own: packets say the luma, and chroma sits above it.
    struct Colours(Arc<Mutex<Option<(u32, u32)>>>);

    impl Decoder for Colours {
        fn decode(&mut self, packet: &Packet) -> Decoded {
            let y = packet.data[0];
            Decoded { newest: Some(tinted(4, 2, y, y + 10, y + 20)), count: 1 }
        }
        fn hardware(&self) -> bool {
            false
        }
    }

    impl Media for Colours {
        fn decoder(&self, _codec: Codec, _options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
            Ok(Box::new(Colours(self.0.clone())))
        }
        fn scaler(&self) -> Box<dyn Scaler> {
            FakeMedia.scaler()
        }
        fn pipeline(&self) -> Result<Box<dyn FramePipeline>, String> {
            Ok(Box::new(PlusOne(self.0.clone())))
        }
    }

    /// FakeMedia with a pipeline of its own.
    struct WithPipeline(Box<dyn Fn() -> Box<dyn FramePipeline> + Send + Sync>);

    impl Media for WithPipeline {
        fn decoder(&self, codec: Codec, options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
            FakeMedia.decoder(codec, options)
        }
        fn scaler(&self) -> Box<dyn Scaler> {
            FakeMedia.scaler()
        }
        fn pipeline(&self) -> Result<Box<dyn FramePipeline>, String> {
            Ok((self.0)())
        }
    }

    #[test]
    fn stream_reader_resizes_and_runs_pipeline_keeping_bgr() {
        let (opener, opens) = scripted(vec![Some(vec![Step::Frame(10)])]);
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let seen: Arc<Mutex<Option<(u32, u32)>>> = Arc::default();
        let w = start(WorkerSetup { width: Some(4), height: Some(3), ..setup(opener) }, Arc::new(Colours(seen.clone())), &cams, &log);
        wait_for(|| cams.sent.lock().unwrap().contains(&11));
        assert_eq!(*seen.lock().unwrap(), Some((4, 3)), "resized before the steps");
        let (frame, _) = w.shared.slot.latest().unwrap();
        assert_eq!((frame.width, frame.height), (4, 3));
        let (y, u, v) = frame.i420_planes().unwrap();
        assert!(y.iter().all(|&p| p == 11) && u.iter().all(|&p| p == 21) && v.iter().all(|&p| p == 31), "every plane through the steps, none swapped: {y:?} {u:?} {v:?}");
        w.stop();
        assert_eq!(opens.gone(), 1);
    }

    #[test]
    fn stream_reader_accumulates_bytes_from_successful_reads() {
        let (opener, opens) = scripted(vec![Some(vec![Step::Sized(1, 12_345), Step::Sized(2, 12_345)])]);
        let (w, _cams, _log) = sitting(opener, &opens);
        wait_for(|| w.shared.bytes_total.load(Ordering::SeqCst) == 2 * 12_345);
        w.stop();
    }

    /// Fails on one picture, past what the real pipeline's guard on each step catches, and notes that it got there.
    struct BreaksOn(u8, Arc<AtomicBool>);

    impl FramePipeline for BreaksOn {
        fn set_steps(&mut self, _steps: Vec<StagedStep>) {}
        fn process(&mut self, frame: Frame, _ctx: &dyn FrameContext) -> Frame {
            if frame.data[0] == self.0 {
                self.1.store(true, Ordering::SeqCst);
                panic!("bad transform")
            }
            frame
        }
        fn is_empty(&self) -> bool {
            false
        }
    }

    fn breaking_on(picture: u8) -> (Arc<WithPipeline>, Arc<AtomicBool>) {
        let tried: Arc<AtomicBool> = Arc::default();
        let seen = tried.clone();
        (Arc::new(WithPipeline(Box::new(move || Box::new(BreaksOn(picture, seen.clone()))))), tried)
    }

    fn pause() -> Step {
        Step::Pause(Duration::from_millis(40))
    }

    #[test]
    fn stream_reader_drops_pipeline_errors_and_releases_capture() {
        let (opener, opens) = scripted(vec![Some(vec![Step::Frame(1), pause(), Step::Frame(2), pause(), Step::Frame(3)])]);
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let (media, tried) = breaking_on(2);
        let w = start(setup(opener), media, &cams, &log);
        wait_for(|| cams.sent.lock().unwrap().contains(&3));
        assert!(tried.load(Ordering::SeqCst), "the frame did reach the pipeline");
        assert_eq!(*cams.sent.lock().unwrap().iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>(), [&1, &3], "dropped, and the next one went out");
        assert_eq!(w.shared.slot.latest().unwrap().0.data[0], 3);
        assert_eq!((opens.calls(), opens.gone()), (1, 0), "the same connection carries on");
        assert!(!kinds(&log).iter().any(|k| k == "error" || k == "idle"), "{:?}", kinds(&log));
        w.stop();
        assert_eq!(opens.gone(), 1);
    }

    #[test]
    fn a_first_frame_that_breaks_lets_go_of_the_capture_and_the_stream_tries_again() {
        let (opener, opens) = scripted(vec![Some(vec![Step::Frame(2)]), Some(vec![Step::Frame(3)])]);
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let (media, tried) = breaking_on(2);
        let w = start(setup(opener), media, &cams, &log);
        wait_for(|| tried.load(Ordering::SeqCst));
        wait_for(|| opens.gone() == 1);
        assert!(w.shared.slot.latest().is_none(), "nothing went out");
        assert!(cams.sent.lock().unwrap().is_empty());
        wait_for(|| cams.sent.lock().unwrap().contains(&3));
        assert!(!kinds(&log).iter().any(|k| k == "error" || k == "idle"), "{:?}", kinds(&log));
        w.stop();
    }

    #[test]
    fn a_crashing_reader_wakes_the_vcam_instead_of_freezing_it() {
        let (opener, opens) = scripted(vec![Some(vec![Step::Frame(1), Step::Crash]), Some(vec![])]);
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = started(setup(opener), &cams, &log);
        wait_for(|| !cams.opened.lock().unwrap().is_empty());
        wait_for(|| opens.calls() == 2 && opens.gone() == 1);
        wait_for(|| w.shared.vcam_free.is_set());
        w.stop();
    }

    #[test]
    fn a_crash_in_the_worker_ends_with_an_error_and_idle() {
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let opener: Opener = Arc::new(|| panic!("boom"));
        let w = started(setup(opener), &cams, &log);
        wait_for(|| log.lock().unwrap().last() == Some(&WorkerEvent::Status("idle".into(), "Not streaming".into())));
        assert!(log.lock().unwrap().contains(&WorkerEvent::Status("error".into(), "Stream error: boom".into())));
        w.stop();
    }

    #[test]
    fn a_new_frame_goes_to_the_camera_without_waiting_for_the_next_tick() {
        let (opener, _opens) = scripted(vec![Some(vec![Step::Frame(1)])]);
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = started(WorkerSetup { fps: 2, ..setup(opener) }, &cams, &log); // a tick every 0.5 s would be far too late
        wait_for(|| !cams.sent.lock().unwrap().is_empty());
        std::thread::sleep(Duration::from_millis(300)); // past the burst pacing (half a period) after the first send
        let published_at = Instant::now();
        assert!(w.shared.publish(1000, grey(4, 2, 7)));
        wait_for(|| cams.sent_at.lock().unwrap().iter().any(|(y, _)| *y == 7));
        let sent = cams.sent_at.lock().unwrap().iter().find(|(y, _)| *y == 7).unwrap().1;
        w.stop();
        assert!(sent - published_at < Duration::from_millis(100), "{:?}", sent - published_at);
    }

    #[test]
    fn the_fps_readout_counts_frames_from_the_phone_not_resends() {
        let (opener, _opens) = scripted(vec![Some(vec![Step::Frame(1)])]); // one frame, then the phone sends nothing new
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = started(WorkerSetup { fps: 60, ..setup(opener) }, &cams, &log);
        wait_for(|| kinds(&log).iter().any(|k| k == "fps"));
        let fps = log.lock().unwrap().iter().find_map(|e| match e { WorkerEvent::Status(k, m) if k == "fps" => Some(m.clone()), _ => None }).unwrap();
        w.stop();
        assert!(fps.starts_with("0.0 fps"), "{fps}");
        assert!(cams.sent.lock().unwrap().len() > 10, "the last frame was resent meanwhile");
    }

    struct Slow;

    impl Decoder for Slow {
        fn decode(&mut self, packet: &Packet) -> Decoded {
            std::thread::sleep(Duration::from_millis(100));
            Jpeg.decode(packet)
        }
        fn hardware(&self) -> bool {
            false
        }
    }

    #[test]
    fn a_slow_decoder_skips_to_the_newest_packet() {
        let steps = VecDeque::from([1, 2, 3, 4, 5].map(Step::Frame));
        let steps = Mutex::new(Some(steps));
        let opener: Opener = Arc::new(move || {
            let steps = steps.lock().unwrap().take().unwrap_or_default();
            Ok(Opened { codec: Codec::Mjpeg, feed: Box::new(Script { steps, stop: Arc::default(), gone: Arc::default() }), own_decoder: Some(Box::new(Slow)) })
        });
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = started(setup(opener), &cams, &log);
        wait_for(|| w.shared.slot.latest().is_some_and(|(f, _)| f.data[0] == 5));
        assert!(w.shared.slot.latest().unwrap().1 < 5, "the ones in between were skipped");
        w.stop();
    }

    /// A reader whose every read took in three frames and kept the newest.
    struct Burst(u8, Arc<AtomicBool>);

    impl FrameReader for Burst {
        fn read(&mut self) -> Result<ReadFrame, FeedEnd> {
            if self.0 == 0 {
                while !self.1.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                return Err(FeedEnd::Stopped);
            }
            self.0 -= 1;
            Ok(ReadFrame { frame: grey(4, 2, self.0), bytes: 1, count: 3 })
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            Box::new(Stop(self.1.clone()))
        }
        fn hardware(&self) -> bool {
            false
        }
    }

    struct BurstMedia;

    impl Media for BurstMedia {
        fn decoder(&self, codec: Codec, options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
            FakeMedia.decoder(codec, options)
        }
        fn scaler(&self) -> Box<dyn Scaler> {
            FakeMedia.scaler()
        }
        fn frame_reader(&self, _feed: Box<dyn VideoFeed>, _decoder: Box<dyn Decoder>) -> Result<Box<dyn FrameReader>, String> {
            Ok(Box::new(Burst(3, Arc::default())))
        }
        fn pipeline(&self) -> Result<Box<dyn FramePipeline>, String> {
            Ok(Box::new(Pass))
        }
    }

    #[test]
    fn frames_that_arrived_together_all_count_as_arrived() {
        let (opener, _opens) = (Arc::new(|| {
            let feed = Script { steps: VecDeque::new(), stop: Arc::default(), gone: Arc::default() };
            Ok((Codec::H264, Box::new(feed) as Box<dyn VideoFeed>).into())
        }) as Opener, ());
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = start(setup(opener), Arc::new(BurstMedia), &cams, &log);
        wait_for(|| w.shared.frames_arrived.load(Ordering::SeqCst) == 9);
        assert!(w.shared.slot.latest().unwrap().1 <= 3);
        w.stop();
    }

    #[test]
    fn another_slot_opens_its_own_camera() {
        let (opener, _opens) = scripted(vec![Some(vec![Step::Frame(1)])]);
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let w = started(WorkerSetup { slot: 2, ..setup(opener) }, &cams, &log);
        wait_for(|| !cams.opened.lock().unwrap().is_empty());
        w.stop();
        assert_eq!(cams.opened.lock().unwrap()[0].0, 2);
    }

    #[test]
    fn the_virtual_camera_opens_in_its_own_format() {
        for (nv12, format) in [(false, PixelFormat::I420), (true, PixelFormat::Nv12)] {
            let (opener, _opens) = scripted(vec![Some(vec![Step::Frame(1)])]);
            let (cams, log) = (Arc::new(Cams::default()), Log::default());
            cams.nv12.store(nv12, Ordering::SeqCst);
            let w = started(setup(opener), &cams, &log);
            wait_for(|| !cams.formats.lock().unwrap().is_empty());
            w.stop();
            assert_eq!(cams.formats.lock().unwrap()[0], format);
        }
    }

    #[test]
    fn hand_over_gives_up_on_a_route_with_gaps_and_keeps_the_old_one() {
        let (cams, log) = (Arc::new(Cams::default()), Log::default());
        let (old, old_opens) = scripted(vec![Some(flowing(1, 3000))]); // the old route keeps going
        let w = started(setup(old), &cams, &log);
        wait_for(|| cams.opened.lock().unwrap().len() == 1);
        // Never HANDOVER_HEALTHY without a gap past HANDOVER_GAP
        let steps: Vec<Step> = (0..8).flat_map(|_| flowing(2, 150).into_iter().chain([Step::Pause(HANDOVER_GAP + Duration::from_millis(200))])).collect();
        let (new, opens) = scripted(vec![Some(steps)]);
        let url = "https://127.0.0.1:41000/v1/video";
        assert!(w.hand_over(url, new));
        let until = Instant::now() + HANDOVER_TIMEOUT + Duration::from_secs(3);
        while moves(&log).is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(moves(&log), [WorkerEvent::MoveFailed(url.into())]);
        assert_eq!((opens.gone(), old_opens.gone()), (1, 0), "the new one let go of, the old one still open");
        assert_eq!(w.url(), "http://phone/v1/video");
        assert_eq!(w.shared.slot.latest().unwrap().0.data[0], 1);
        let received = w.shared.slot.latest().unwrap().1;
        wait_for(|| w.shared.slot.latest().unwrap().1 > received);
        assert!(w.hand_over(url, Arc::new(|| Err("no".into()))), "it can try again later");
        w.stop();
    }

    #[test]
    fn a_stream_that_drops_mid_hand_over_carries_on_on_the_new_route() {
        let (opener, opens) = scripted(vec![Some(vec![])]);
        let (w, _cams, log) = sitting(opener, &opens);
        let (new, new_opens) = scripted(vec![Some(flowing(2, 400))]);
        let url = "https://127.0.0.1:41000/v1/video";
        assert!(w.hand_over(url, new));
        wait_for(|| w.shared.handover_ready.lock().unwrap().is_some());
        let fast = no_delay(&w);
        let mut cap = w.shared.reconnect().expect("the prepared route");
        drop(fast);
        assert_eq!(cap.read_frame().unwrap().0.data[0], 2, "the reader the hand-over prepared, still on its stream");
        assert_eq!(new_opens.calls(), 1, "not a new one");
        assert_eq!(moves(&log), [WorkerEvent::Moved(url.into())]);
        assert_eq!(w.url(), url);
        assert_eq!(opens.calls(), 1, "the old route isn't tried again");
        w.stop();
    }

    #[test]
    fn a_stop_doesnt_wait_for_a_ready_hand_over_to_be_taken() {
        let (opener, opens) = scripted(vec![Some(vec![])]);
        let (w, _cams, log) = sitting(opener, &opens);
        let (new, new_opens) = scripted(vec![Some(flowing(2, 400))]);
        assert!(w.hand_over("https://127.0.0.1:41000/v1/video", new));
        wait_for(|| w.shared.handover_ready.lock().unwrap().is_some());
        w.request_stop();
        let started = Instant::now();
        w.shared.join_handover();
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
        assert_eq!(new_opens.gone(), 1);
        assert!(!log.lock().unwrap().iter().any(|e| matches!(e, WorkerEvent::Moved(_) | WorkerEvent::MoveFailed(_))));
        w.stop();
    }

    // What a stream hands media: the tap for H.264 only, and which stream each frame is from.

    #[derive(Default)]
    struct Seen {
        replaced: Mutex<Vec<String>>,
        tapped: Mutex<Vec<Codec>>,
        plain: Mutex<Vec<Codec>>,
        sources: Mutex<Vec<String>>,
        /// The number each frame had while the steps ran.
        numbers: Mutex<Vec<u64>>,
        /// How many steps the pipeline had at each frame.
        steps_at_first: Mutex<Vec<usize>>,
        /// The first byte of each frame's person mask.
        masks: Mutex<Vec<u8>>,
        /// Each tap this media handed out, and whether a decoder got the first, which is the one the worker keeps.
        tap: Mutex<Vec<Arc<dyn PacketTap>>>,
        tap_was_its_own: Mutex<Vec<bool>>,
    }

    struct Tap;

    impl PacketTap for Tap {
        fn set(&self, _makers: Vec<(String, Arc<dyn telescope_core::host::PacketStepFactory>)>) -> bool {
            false
        }
        fn on(&self) -> bool {
            false
        }
    }

    struct Reader(Feed);

    impl FrameReader for Reader {
        fn read(&mut self) -> Result<telescope_core::media::ReadFrame, FeedEnd> {
            let p = self.0.read()?;
            Ok(telescope_core::media::ReadFrame { frame: grey(4, 2, p.data[0]), bytes: 1, count: 1 })
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            self.0.stopper()
        }
        fn hardware(&self) -> bool {
            false
        }
    }

    struct Says(Arc<Seen>, usize);

    impl FramePipeline for Says {
        fn set_steps(&mut self, steps: Vec<StagedStep>) {
            self.1 = steps.len();
        }
        fn replace_steps(&mut self, owner: &str, _stage: telescope_core::frame::FrameStage, _steps: Vec<telescope_core::frame::FrameStep>) {
            self.0.replaced.lock().unwrap().push(owner.to_string());
        }
        fn process(&mut self, frame: Frame, ctx: &dyn FrameContext) -> Frame {
            self.0.steps_at_first.lock().unwrap().push(self.1);
            self.0.masks.lock().unwrap().extend(ctx.person_mask(&frame).map(|m| m[0]));
            self.0.sources.lock().unwrap().push(ctx.source_id().to_string());
            self.0.numbers.lock().unwrap().push(frame.number);
            frame
        }
        fn is_empty(&self) -> bool {
            false
        }
    }

    struct Watching(Arc<Seen>);

    impl Media for Watching {
        fn decoder(&self, codec: Codec, _options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
            self.0.plain.lock().unwrap().push(codec);
            Ok(Box::new(Jpeg))
        }
        fn decoder_with_tap(&self, codec: Codec, _options: &DecodeOptions, tap: Arc<dyn PacketTap>) -> Result<Box<dyn Decoder>, String> {
            self.0.tapped.lock().unwrap().push(codec);
            self.0.tap_was_its_own.lock().unwrap().push(Arc::ptr_eq(&tap, &self.0.tap.lock().unwrap()[0]));
            Ok(Box::new(Jpeg))
        }
        fn frame_reader(&self, feed: Box<dyn VideoFeed>, _decoder: Box<dyn Decoder>) -> Result<Box<dyn FrameReader>, String> {
            let _ = feed; // the fake reads its own packets
            Ok(Box::new(Reader(Feed { packets: Arc::new(Mutex::new(VecDeque::from([Some(5)]))), stop: Arc::default(), endless: None })))
        }
        fn packet_tap(&self) -> Option<Arc<dyn PacketTap>> {
            let tap: Arc<dyn PacketTap> = Arc::new(Tap);
            self.0.tap.lock().unwrap().push(tap.clone());
            Some(tap)
        }
        fn scaler(&self) -> Box<dyn Scaler> {
            Box::new(Rgb)
        }
        fn pipeline(&self) -> Result<Box<dyn FramePipeline>, String> {
            Ok(Box::new(Says(self.0.clone(), 0)))
        }
    }

    fn watched(url: &str, codec: Codec) -> Arc<Seen> {
        watched_as("phone-1", url, codec)
    }

    fn watched_as(source: &str, url: &str, codec: Codec) -> Arc<Seen> {
        let seen = Arc::new(Seen::default());
        let opener: Opener = Arc::new(move || {
            Ok((codec, Box::new(Feed { packets: Arc::new(Mutex::new(VecDeque::from([Some(5)]))), stop: Arc::default(), endless: None }) as Box<dyn VideoFeed>).into())
        });
        let setup = WorkerSetup { url: url.into(), opener, width: None, height: None, fps: 30, canvas: (None, None), slot: 0, waiting_text: None,
                                  source_id: source.into(), hardware_decode: false, plugin_source: false, vision: None, steps: Vec::new(), gate: None };
        let cams = Arc::new(Cams::default());
        let w = StreamWorker::start(setup, Arc::new(Watching(seen.clone())), Arc::new(FakeOutputs(cams.clone())),
                                    telescope_media_free_slot(), Arc::new(|_| {})).unwrap();
        wait_for(|| !cams.sent.lock().unwrap().is_empty());
        w.stop();
        seen
    }

    /// A person mask that says which frame it was worked out for, once per number as the service does; with a list, it
    /// wants people and notes what it's fed.
    #[derive(Default)]
    struct Masks(Option<Fed>);

    /// What a vision service was fed.
    type Fed = Arc<Mutex<Vec<(Frame, telescope_core::vision::Upright)>>>;

    impl telescope_core::vision::VisionService for Masks {
        fn attach(&self, _bus: telescope_core::BusSender) {}
        fn state(&self) -> String {
            "ready".into()
        }
        fn load(&self) {}
        fn person_mask(&self, _source_id: &str, frame: &Frame) -> Option<Arc<Vec<u8>>> {
            Some(Arc::new(vec![frame.number as u8]))
        }
        fn want_people(&self, _owner: &str, _on: bool) {}
        fn wants_people(&self) -> bool {
            self.0.is_some()
        }
        fn feed_raw(&self, _source_id: &str, frame: &Frame, upright: telescope_core::vision::Upright) {
            if let Some(fed) = &self.0 {
                fed.lock().unwrap().push((frame.clone(), upright));
            }
        }
        fn tracked_people(&self, _source_id: &str) -> (Vec<serde_json::Value>, u32) {
            (Vec::new(), 0)
        }
        fn view_turned(&self, _old: telescope_core::vision::Turn, _new: telescope_core::vision::Turn) {}
        fn drop_stream(&self, _source_id: &str) {}
        fn set_power_saving(&self, _on: bool) {}
        fn diagnostics(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn shutdown(&self) {}
        fn pack(&self) -> telescope_core::vision::VisionPack {
            unimplemented!()
        }
        fn download(&self, _switch_on: &str) {}
        fn cancel_download(&self) {}
        fn remove_pack(&self) -> Result<(), String> {
            Ok(())
        }
    }

    struct NoStep;

    impl telescope_core::frame::CpuStep for NoStep {
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
    }

    fn with_steps(steps: Vec<StagedStep>, vision: Option<VisionFeed>, frames: usize) -> Arc<Seen> {
        with_gate(steps, vision, frames, None)
    }

    fn with_gate(steps: Vec<StagedStep>, vision: Option<VisionFeed>, frames: usize, gate: Option<Arc<crate::handover::ViewGate>>) -> Arc<Seen> {
        let seen = Arc::new(Seen::default());
        let w = running(&seen, steps, vision, gate);
        wait_for(|| seen.sources.lock().unwrap().len() >= frames);
        w.stop();
        seen
    }

    fn running(seen: &Arc<Seen>, steps: Vec<StagedStep>, vision: Option<VisionFeed>, gate: Option<Arc<crate::handover::ViewGate>>) -> StreamWorker {
        let opener: Opener = Arc::new(|| {
            Ok((Codec::Mjpeg, Box::new(Feed { packets: Arc::default(), stop: Arc::default(), endless: Some(5) }) as Box<dyn VideoFeed>).into())
        });
        let setup = WorkerSetup { url: "https://phone/v1/video".into(), opener, width: None, height: None, fps: 30, canvas: (None, None), slot: 0,
                                  waiting_text: None, source_id: "phone-1".into(), hardware_decode: false, plugin_source: false, vision, steps, gate };
        StreamWorker::start(setup, Arc::new(Watching(seen.clone())), Arc::new(FakeOutputs(Arc::new(Cams::default()))),
                            telescope_media_free_slot(), Arc::new(|_| {})).unwrap()
    }

    #[test]
    fn a_new_colour_step_takes_the_place_of_the_old_on_the_next_frame() {
        let seen = Arc::new(Seen::default());
        let w = running(&seen, Vec::new(), None, None);
        w.set_colour(Vec::new());
        wait_for(|| seen.replaced.lock().unwrap().iter().any(|o| o == "colour"));
        w.stop();
    }

    #[test]
    fn the_view_changes_crop_from_the_worker_while_a_phone_share_is_on_its_way() {
        use telescope_core::host::View;
        use telescope_core::view::PhoneZoom;
        let gate = Arc::new(crate::handover::ViewGate::default());
        gate.follow(Some("phone-1"), true);
        let (old, new) = (PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 }, PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.6, y: 0.5 });
        gate.begin(old, new, (View { zoom: 2.0, ..View::default() }, View { zoom: 3.0, ..View::default() }), crate::shell::now_s());
        let seen = with_gate(Vec::new(), None, 1, Some(gate));
        assert_eq!(seen.replaced.lock().unwrap().first().map(String::as_str), Some("view"));
    }

    #[test]
    fn the_steps_are_in_before_the_first_frame_goes_out() {
        let steps = vec![StagedStep { owner: "blur".into(), stage: telescope_core::frame::FrameStage::Effects,
                                      step: telescope_core::frame::FrameStep::Cpu(Box::new(NoStep)) }];
        let seen = with_steps(steps, None, 1);
        assert_eq!(seen.steps_at_first.lock().unwrap()[0], 1, "a first frame on an empty pipeline would miss the blur");
    }

    #[test]
    fn each_frame_of_a_stream_gets_a_person_mask_of_its_own() {
        let vision = VisionFeed { service: Arc::<Masks>::default(), upright: Arc::default(), frame_sizes: Arc::default(), motion: None };
        let seen = with_steps(Vec::new(), Some(vision), 2);
        let masks = seen.masks.lock().unwrap();
        assert!(masks.len() >= 2 && masks[0] != masks[1], "the mask of the first frame stuck: {masks:?}");
    }

    // test_view_controller.py::test_people_are_found_in_the_picture_as_shown_through_the_phone_share
    #[test]
    fn people_are_found_in_the_picture_as_shown_through_the_phone_share() {
        use telescope_core::host::ViewChange;
        let path = telescope_core::testing::scratch_dir("people-share").join("telescope_config.json");
        let mut s = crate::shell::State::new(telescope_core::config::ConfigFile::at(path));
        let fed = Fed::default();
        s.vision = Some(Arc::new(Masks(Some(fed.clone()))));
        s.streams.source_id = Some("phone-1".into());
        let zooms = serde_json::json!({"cameras": [{"id": "0", "current": true, "zoomRatioMax": 10.0, "cropZoomMax": 4.0, "freeformCrop": true}]});
        let said = s.vc.stream_start(&s.view, "phone-1", 1, 0.0);
        s.apply_said(said);
        let said = s.vc.phone_state(&s.view, &zooms, 0.0);
        s.apply_said(said);
        let said = s.vc.take("Auto framing").1;
        s.apply_said(said);
        let st = &mut s;
        let said = st.vc.steer(&mut st.view, "Auto framing", 2.0, 0.0, 0.0, 0.0); // the phone sends the middle 1/1.4286
        st.apply_said(said);
        let said = st.vc.set_view(&mut st.view, &ViewChange { rotation: Some(Some(90)), ..Default::default() }, 0.0);
        st.apply_said(said);
        with_steps(Vec::new(), s.vision_feed(), 1);
        let fed = fed.lock().unwrap();
        let (frame, upright) = &fed[0];
        assert_eq!(upright.turn.rotation, Some(90), "the model gets the picture as shown (view_pixels checks the turned size)");
        assert_eq!(s.view_aspect(), frame.height as f32 / frame.width as f32);
        let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
        let (x, y) = upright.to_shown(0.5, 0.5);
        assert!(near(x, 0.5) && near(y, 0.5));
        let (x, y) = upright.to_shown(1.0, 0.0); // the top right of the upright frame, inside the share
        assert!(near(x, 0.85) && near(y, 0.15), "{x} {y}");
    }

    // Which kind of feed a URL gets is link's (feeds::a_feed_goes_by_what_the_url_asks_for); here the worker gets what link hands it
    #[test]
    fn the_stream_worker_gives_its_tap_to_light_readers_only() {
        for url in ["https://phone/v1/video.h264", "https://phone/v1/video.udp"] {
            let light = watched(url, Codec::H264);
            assert_eq!(*light.tapped.lock().unwrap(), [Codec::H264]);
            assert_eq!(*light.tap_was_its_own.lock().unwrap(), [true]);
            assert!(light.plain.lock().unwrap().is_empty());
        }
        let heavy = watched("https://phone/v1/video", Codec::Mjpeg);
        assert!(heavy.tapped.lock().unwrap().is_empty());
        assert!(heavy.plain.lock().unwrap().iter().all(|c| *c == Codec::Mjpeg));
    }

    #[test]
    fn both_readers_hand_their_tap_to_the_decoder() {
        // TCP and UDP H.264 both come from link as H.264 feeds
        for url in ["https://phone/v1/video.h264", "https://phone/v1/video.udp"] {
            let seen = watched(url, Codec::H264);
            assert_eq!(*seen.tapped.lock().unwrap(), [Codec::H264]);
            assert_eq!(*seen.tap_was_its_own.lock().unwrap(), [true]);
        }
    }

    #[test]
    fn a_frame_step_can_tell_which_stream_its_frame_is_from() {
        for source in ["a", "b"] {
            let seen = watched_as(source, "https://phone/v1/video.h264", Codec::H264);
            let sources = seen.sources.lock().unwrap();
            assert!(!sources.is_empty() && sources.iter().all(|s| s == source), "{sources:?}");
        }
        // and which frame it is: what a person mask is worked out once by
        let seen = watched("https://phone/v1/video.h264", Codec::H264);
        let w = running(&seen, Vec::new(), None, None);
        assert!(w.shared.publish(1_000, Frame::new(6, 4, PixelFormat::Rgba, vec![0; 6 * 4 * 4])));
        assert_eq!(seen.numbers.lock().unwrap().last(), Some(&1_000));
        assert_eq!(seen.sources.lock().unwrap().last().map(String::as_str), Some("phone-1"));
        w.stop();
    }
}
