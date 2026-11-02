//! The headless CLI's one phone stream (telescope-rs stream), kept until it moves to worker.rs: video in, decoded, out to the virtual camera and scaled for the preview; a second thread polls /v1/state, which also keeps the phone counting this computer as its viewer.

use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::frame::Frame;
use telescope_core::link::PhoneSession;
use telescope_core::media::{DecodeOptions, FeedEnd, FeedStopper, Media};
use telescope_core::output::{Outputs, VideoOutput};

const PREVIEW_PERIOD: Duration = Duration::from_millis(33);
const STATE_PERIOD: Duration = Duration::from_secs(1);

/// Numbers every frame any stream makes (StreamFrame.number).
static FRAME_NUMBER: AtomicU64 = AtomicU64::new(0);

pub struct StreamOptions {
    pub fps: u32,
    /// Which virtual camera to write to (0 is the one apps list as Telescope).
    pub slot: u32,
    pub hardware_decode: bool,
}

#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub frames: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub mbps: f32,
    pub decode_ms: f32,
    pub hardware: bool,
    pub camera: String,
    pub error: String,
}

pub trait StreamEvents: Send + Sync + 'static {
    /// A preview frame, RGB24 at the size set_preview_size() asked for.
    fn preview(&self, rgb: Vec<u8>, width: u32, height: u32);
    fn state(&self, state: Value);
    fn stats(&self, stats: Stats);
    fn ended(&self, why: String);
}

pub struct Streaming {
    pub session: Arc<dyn PhoneSession>,
    running: Arc<AtomicBool>,
    stopper: Arc<Mutex<Option<Box<dyn FeedStopper>>>>,
    /// The preview's size in pixels, (width << 32) | height; 0 means nobody's watching.
    preview_size: Arc<AtomicU64>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

struct Pipeline {
    media: Arc<dyn Media>,
    outputs: Arc<dyn Outputs>,
    opts: StreamOptions,
}

impl Streaming {
    pub fn start(session: Arc<dyn PhoneSession>, media: Arc<dyn Media>, outputs: Arc<dyn Outputs>, opts: StreamOptions,
                 events: Arc<dyn StreamEvents>) -> Streaming {
        let running = Arc::new(AtomicBool::new(true));
        let stopper: Arc<Mutex<Option<Box<dyn FeedStopper>>>> = Arc::default();
        let preview_size = Arc::new(AtomicU64::new(0));
        let mut threads = Vec::new();
        {
            let (session, running, stopper, preview_size, events) =
                (session.clone(), running.clone(), stopper.clone(), preview_size.clone(), events.clone());
            let pipeline = Pipeline { media, outputs, opts };
            threads.push(std::thread::Builder::new().name("video".into()).spawn(move || {
                let why = pipeline.run(session.as_ref(), &running, &stopper, &preview_size, events.as_ref());
                if running.swap(false, Ordering::SeqCst) {
                    events.ended(why);
                }
            }).unwrap());
        }
        {
            let (session, running) = (session.clone(), running.clone());
            threads.push(std::thread::Builder::new().name("state".into()).spawn(move || {
                while running.load(Ordering::SeqCst) {
                    if let Ok(state) = session.state() {
                        events.state(state);
                    }
                    let until = Instant::now() + STATE_PERIOD;
                    while running.load(Ordering::SeqCst) && Instant::now() < until {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }).unwrap());
        }
        Streaming { session, running, stopper, preview_size, threads }
    }

    pub fn set_preview_size(&self, width: u32, height: u32) {
        self.preview_size.store(((width as u64) << 32) | height as u64, Ordering::Relaxed);
    }

    pub fn stop(mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(s) = self.stopper.lock().unwrap().as_ref() {
            s.stop();
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Pipeline {
    fn run(&self, session: &dyn PhoneSession, running: &AtomicBool, stopper: &Mutex<Option<Box<dyn FeedStopper>>>,
           preview_size: &AtomicU64, events: &dyn StreamEvents) -> String {
        let (codec, mut feed) = match session.open_video() {
            Ok(f) => f,
            Err(e) => return e,
        };
        *stopper.lock().unwrap() = Some(feed.stopper());
        let mut decoder = match self.media.decoder(codec, &DecodeOptions { hardware: self.opts.hardware_decode }) {
            Ok(d) => d,
            Err(e) => return e,
        };
        let mut camera: Option<Box<dyn VideoOutput>> = None;
        let mut camera_error = String::new();
        let mut scaler = self.media.scaler();
        let mut stats = Stats { hardware: decoder.hardware(), ..Stats::default() };
        let (mut last_preview, mut window_start) = (Instant::now() - PREVIEW_PERIOD, Instant::now());
        let (mut window_frames, mut window_bytes, mut decode_time) = (0u32, 0u64, Duration::ZERO);
        while running.load(Ordering::SeqCst) {
            let packet = match feed.read() {
                Ok(p) => p,
                Err(FeedEnd::Closed) => return "The phone stopped sending video".into(),
                Err(FeedEnd::Stopped) => return String::new(),
                Err(FeedEnd::Error(e)) => return format!("The video stopped: {e}"),
            };
            window_bytes += packet.data.len() as u64;
            stats.bytes += packet.data.len() as u64;
            let t = Instant::now();
            let decoded = decoder.decode(&packet);
            decode_time += t.elapsed();
            window_frames += decoded.count;
            stats.frames += decoded.count as u64;
            let Some(mut frame) = decoded.newest else { continue };
            feed.decoded();
            frame.number = FRAME_NUMBER.fetch_add(1, Ordering::Relaxed) + 1;
            if camera.as_ref().is_none_or(|c| c.size() != (frame.width, frame.height)) {
                camera = self.outputs.open(self.opts.slot, frame.width, frame.height, self.opts.fps).map_err(|e| camera_error = e).ok();
            }
            if let Some(cam) = camera.as_mut() {
                if let Err(e) = cam.send(&frame) {
                    camera_error = e;
                    camera = None;
                }
            }
            let size = preview_size.load(Ordering::Relaxed);
            if size != 0 && last_preview.elapsed() >= PREVIEW_PERIOD {
                last_preview = Instant::now();
                let (pw, ph) = fit((size >> 32) as u32, size as u32, &frame);
                events.preview(scaler.rgb(&frame, pw, ph), pw, ph);
            }
            let elapsed = window_start.elapsed();
            if elapsed >= Duration::from_secs(1) {
                let secs = elapsed.as_secs_f32();
                stats.fps = window_frames as f32 / secs;
                stats.mbps = window_bytes as f32 * 8.0 / secs / 1e6;
                stats.decode_ms = decode_time.as_secs_f32() * 1000.0 / window_frames.max(1) as f32;
                (stats.width, stats.height) = (frame.width, frame.height);
                stats.camera = camera.as_ref().map(|c| c.device()).unwrap_or_default();
                stats.error = camera_error.clone();
                events.stats(stats.clone());
                (window_start, window_frames, window_bytes, decode_time) = (Instant::now(), 0, 0, Duration::ZERO);
            }
        }
        String::new()
    }
}

/// The largest size with the frame's shape that fits in (w, h), even on both sides.
fn fit(w: u32, h: u32, frame: &Frame) -> (u32, u32) {
    let (fw, fh) = (frame.width, frame.height);
    let scale = (w as f32 / fw as f32).min(h as f32 / fh as f32).min(1.0);
    (((fw as f32 * scale) as u32 & !1).max(2), ((fh as f32 * scale) as u32 & !1).max(2))
}
