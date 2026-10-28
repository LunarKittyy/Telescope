//! One phone's stream: reads /v1/video.h264, decodes it, writes every frame to the virtual camera and hands a scaled
//! copy to the preview. A second thread polls /v1/state, which also keeps the phone counting this computer as its viewer.

use crate::decode::{Decoder, Frame, PreviewScaler};
use crate::session::Route;
use crate::v4l2::VirtualCamera;
use serde_json::Value;
use std::io::Read;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CHUNK: usize = 64 * 1024;
const PREVIEW_PERIOD: Duration = Duration::from_millis(33);
const STATE_PERIOD: Duration = Duration::from_secs(1);

pub struct StreamOptions {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub device: Option<String>,
    pub vaapi: bool,
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
    /// A preview frame, RGB24 at the size preview_size() asked for.
    fn preview(&self, rgb: Vec<u8>, width: u32, height: u32);
    fn state(&self, state: Value);
    fn stats(&self, stats: Stats);
    fn ended(&self, why: String);
}

pub struct Streaming {
    pub route: Route,
    running: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    /// The preview's size in pixels, (width << 32) | height; 0 means nobody's watching.
    pub preview_size: Arc<AtomicU64>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Streaming {
    pub fn start(route: Route, opts: StreamOptions, events: Arc<dyn StreamEvents>) -> Streaming {
        let running = Arc::new(AtomicBool::new(true));
        let socket: Arc<Mutex<Option<TcpStream>>> = Arc::default();
        let preview_size = Arc::new(AtomicU64::new(0));
        let mut threads = Vec::new();
        {
            let (route, running, socket, preview_size, events) =
                (route.clone(), running.clone(), socket.clone(), preview_size.clone(), events.clone());
            threads.push(std::thread::Builder::new().name("video".into()).spawn(move || {
                let why = run_video(&route, &opts, &running, &socket, &preview_size, events.as_ref());
                if running.swap(false, Ordering::SeqCst) {
                    events.ended(why);
                }
            }).unwrap());
        }
        {
            let (route, running) = (route.clone(), running.clone());
            threads.push(std::thread::Builder::new().name("state".into()).spawn(move || {
                while running.load(Ordering::SeqCst) {
                    if let Ok(state) = route.state() {
                        events.state(state);
                    }
                    let until = Instant::now() + STATE_PERIOD;
                    while running.load(Ordering::SeqCst) && Instant::now() < until {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }).unwrap());
        }
        Streaming { route, running, socket, preview_size, threads }
    }

    pub fn set_preview_size(&self, width: u32, height: u32) {
        self.preview_size.store(((width as u64) << 32) | height as u64, Ordering::Relaxed);
    }

    pub fn stop(mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(s) = self.socket.lock().unwrap().as_ref() {
            crate::https::abort(s);
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn run_video(route: &Route, opts: &StreamOptions, running: &AtomicBool, socket: &Mutex<Option<TcpStream>>,
             preview_size: &AtomicU64, events: &dyn StreamEvents) -> String {
    let mut resp = match route.auth.request(route.stream_port, "GET", "/v1/video.h264", None, Duration::from_secs(5)) {
        Ok(r) => r,
        Err(e) => return format!("Couldn't open the video: {e}"),
    };
    if resp.status != 200 || !resp.header("content-type").unwrap_or("").contains("video/h264") {
        return format!("The phone answered {} instead of H.264", resp.status);
    }
    *socket.lock().unwrap() = resp.aborter().ok();
    let mut decoder = match Decoder::new(opts.vaapi) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let mut camera: Option<VirtualCamera> = None;
    let mut camera_error = String::new();
    let mut scaler = PreviewScaler::default();
    let mut stats = Stats { hardware: decoder.hardware, ..Stats::default() };
    let mut buf = vec![0u8; CHUNK];
    let (mut last_preview, mut window_start) = (Instant::now() - PREVIEW_PERIOD, Instant::now());
    let (mut window_frames, mut window_bytes, mut decode_time) = (0u32, 0u64, Duration::ZERO);
    while running.load(Ordering::SeqCst) {
        let n = match resp.body.read(&mut buf) {
            Ok(0) => return "The phone stopped sending video".into(),
            Ok(n) => n,
            Err(e) => return format!("The video stopped: {e}"),
        };
        window_bytes += n as u64;
        stats.bytes += n as u64;
        let t = Instant::now();
        let (frame, count) = decoder.feed(&buf[..n]);
        decode_time += t.elapsed();
        window_frames += count;
        stats.frames += count as u64;
        let Some(frame) = frame else { continue };
        if camera.as_ref().is_none_or(|c| c.width != frame.width || c.height != frame.height) {
            camera = open_camera(opts, &frame).map_err(|e| camera_error = e).ok();
        }
        if let Some(cam) = camera.as_mut() {
            if let Err(e) = cam.send(&frame.i420) {
                camera_error = format!("{}: {e}", cam.device);
                camera = None;
            }
        }
        let size = preview_size.load(Ordering::Relaxed);
        if size != 0 && last_preview.elapsed() >= PREVIEW_PERIOD {
            last_preview = Instant::now();
            let (pw, ph) = fit((size >> 32) as u32, size as u32, frame.width, frame.height);
            events.preview(scaler.rgb(&frame, pw, ph), pw, ph);
        }
        let elapsed = window_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let secs = elapsed.as_secs_f32();
            stats.fps = window_frames as f32 / secs;
            stats.mbps = window_bytes as f32 * 8.0 / secs / 1e6;
            stats.decode_ms = decode_time.as_secs_f32() * 1000.0 / window_frames.max(1) as f32;
            (stats.width, stats.height) = (frame.width, frame.height);
            stats.camera = camera.as_ref().map(|c| c.device.clone()).unwrap_or_default();
            stats.error = camera_error.clone();
            events.stats(stats.clone());
            (window_start, window_frames, window_bytes, decode_time) = (Instant::now(), 0, 0, Duration::ZERO);
        }
    }
    String::new()
}

fn open_camera(opts: &StreamOptions, frame: &Frame) -> Result<VirtualCamera, String> {
    let device = opts.device.clone()
        .or_else(|| crate::v4l2::find_by_label("Phone Camera"))
        .ok_or("No v4l2loopback camera is set up")?;
    VirtualCamera::open(&device, frame.width, frame.height, opts.fps).map_err(|e| format!("{device}: {e}"))
}

/// The largest size with the frame's shape that fits in (w, h), even on both sides.
fn fit(w: u32, h: u32, fw: u32, fh: u32) -> (u32, u32) {
    let scale = (w as f32 / fw as f32).min(h as f32 / fh as f32).min(1.0);
    (((fw as f32 * scale) as u32 & !1).max(2), ((fh as f32 * scale) as u32 & !1).max(2))
}
