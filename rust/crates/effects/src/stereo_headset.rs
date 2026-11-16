//! The 3D plugin's headset viewer: a page and a WebSocket that send the side by side picture to a headset's browser as JPEG messages.
//!
//! The 3D step hands each pair of eyes it made a picture from to the Feed (only while a headset is connected). One worker thread packs the pair at
//! each eye's own size, with the card's Fine tune and Match, into one wide picture, encodes it and publishes it as the newest message. Each headset
//! has a thread of its own that sends the newest message it hasn't sent yet, so nothing queues: a headset that can't keep up skips pictures. The
//! picture is made from the same frames the camera's picture is, so Privacy blur and every step before the pair apply, and a cover (the Wait
//! screen) on the camera's frame covers the headset's too.

use crate::cpu::Rgb;
use crate::shared::same;
use crate::stereo::{self, Tune};
use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::ExtendedColorType;
use std::io::{ErrorKind, Read};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use telescope_core::frame::Frame;
use telescope_core::host::FrameStore;
use telescope_core::web::{PageHandler, Reply, Request, ServerStats, Socket, Step, Upgrade};

/// Next to On Air's 8768.
pub const HEADSET_PORT: u16 = 8769;
/// How many headsets can watch at once.
pub const MAX_VIEWERS: usize = 2;
/// The fewest milliseconds between two pictures: 30 a second.
const MIN_GAP: Duration = Duration::from_millis(33);
/// The biggest an eye is sent at; both eyes side by side make 3840x1080.
pub const MAX_EYE: (u32, u32) = (1920, 1080);
const QUALITY: u8 = 80;
/// A picture this old isn't shown any more: the headset goes black until a new one comes.
const STALE: Duration = Duration::from_secs(3);
/// A pair the worker wasn't free for in this long is dropped for the next.
const JOB_STALE: Duration = Duration::from_millis(500);
/// How long the worker and a headset's thread wait for something to do before looking at the clock.
const IDLE_WAIT: Duration = Duration::from_millis(250);
const SEND_WAIT: Duration = Duration::from_millis(30);
const PING_EVERY: Duration = Duration::from_secs(5);

const PAGE: &[u8] = include_bytes!("../web/headset.html");
const SCRIPT: &[u8] = include_bytes!("../web/headset.js");

/// What the 3D step hands over for a picture: the two eyes it paired, and what the camera's picture of them looked like.
pub struct Job {
    pub target: String,
    pub left: Frame,
    pub right: Frame,
    pub tune: Tune,
    /// The number of the camera's frame the pair made, and its pixels: how to tell whether something later covered it.
    pub number: u64,
    pub handed: Arc<Vec<u8>>,
    /// The pipeline epoch the pair was made under.
    pub epoch: u64,
    pub at: Instant,
}

/// What a headset is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Jpeg(Arc<Vec<u8>>),
    /// Nothing to show: the page goes black.
    Blank,
}

struct Shown {
    seq: u64,
    message: Message,
    epoch: u64,
    at: Instant,
}

struct State {
    job: Option<Job>,
    shown: Option<Shown>,
    seq: u64,
    open: bool,
    token: String,
}

/// Pictures from the 3D step to the headsets watching.
pub struct Feed {
    state: Mutex<State>,
    job_ready: Condvar,
    shown_changed: Condvar,
    viewers: AtomicUsize,
    token_gen: AtomicU64,
    /// How many pictures were built, for the tests.
    built: AtomicU64,
    max_eye: (u32, u32),
}

/// Holds one of the places for a headset until it's dropped.
pub struct Slot(Arc<Feed>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.viewers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The size an eye is sent at: its own, shrunk to fit `max` keeping its shape, both sides even.
pub fn eye_size(w: usize, h: usize, (max_w, max_h): (u32, u32)) -> (usize, usize) {
    if w == 0 || h == 0 {
        return (2, 2);
    }
    let scale = (max_w as f64 / w as f64).min(max_h as f64 / h as f64).min(1.0);
    let even = |v: usize| (((v as f64 * scale).round() as usize).max(2)) & !1;
    (even(w), even(h))
}

/// The picture as a JPEG.
pub fn encode(rgb: &Rgb, quality: u8) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(rgb.w * rgb.h / 6);
    JpegEncoder::new_with_quality(&mut out, quality).encode(&rgb.data, rgb.w as u32, rgb.h as u32, ExtendedColorType::Rgb8).ok()?;
    Some(out)
}

impl Feed {
    pub fn new(token: &str) -> Feed {
        Feed::with_max_eye(token, MAX_EYE)
    }

    pub fn with_max_eye(token: &str, max_eye: (u32, u32)) -> Feed {
        Feed {
            state: Mutex::new(State { job: None, shown: None, seq: 0, open: false, token: token.into() }),
            job_ready: Condvar::new(),
            shown_changed: Condvar::new(),
            viewers: AtomicUsize::new(0),
            token_gen: AtomicU64::new(0),
            built: AtomicU64::new(0),
            max_eye,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ready for a worker and headsets.
    pub fn open(&self) {
        let mut state = self.lock();
        (state.open, state.job, state.shown) = (true, None, None);
    }

    /// Ends the worker and every headset's thread.
    pub fn close(&self) {
        let mut state = self.lock();
        (state.open, state.job) = (false, None);
        drop(state);
        self.job_ready.notify_all();
        self.shown_changed.notify_all();
    }

    pub fn is_open(&self) -> bool {
        self.lock().open
    }

    pub fn token(&self) -> String {
        self.lock().token.clone()
    }

    pub fn token_ok(&self, token: &str) -> bool {
        !token.is_empty() && same(token, &self.lock().token)
    }

    /// A new link: headsets on the old one are disconnected.
    pub fn set_token(&self, token: &str) {
        self.lock().token = token.into();
        self.token_gen.fetch_add(1, Ordering::SeqCst);
        self.shown_changed.notify_all();
    }

    pub fn viewers(&self) -> usize {
        self.viewers.load(Ordering::SeqCst)
    }

    /// Whether anything needs pictures: a headset is connected. Without one the step builds nothing.
    pub fn wanted(&self) -> bool {
        self.viewers() > 0
    }

    /// Whether a pair waits for the worker.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> bool {
        self.lock().job.is_some()
    }

    pub fn built(&self) -> u64 {
        self.built.load(Ordering::SeqCst)
    }

    /// A place for one more headset, None when MAX_VIEWERS are watching.
    pub fn claim(self: &Arc<Self>) -> Option<Slot> {
        let mut n = self.viewers.load(Ordering::SeqCst);
        loop {
            if n >= MAX_VIEWERS {
                return None;
            }
            match self.viewers.compare_exchange(n, n + 1, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return Some(Slot(self.clone())),
                Err(now) => n = now,
            }
        }
    }

    /// The pair for the next picture. Only the newest waits: an older one that wasn't taken yet is replaced. False without a headset.
    pub fn submit(&self, job: Job) -> bool {
        if !self.wanted() {
            return false;
        }
        let mut state = self.lock();
        if !state.open {
            return false;
        }
        state.job = Some(job);
        drop(state);
        self.job_ready.notify_all();
        true
    }

    pub(crate) fn take_job(&self, timeout: Duration) -> Option<Job> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if !state.open {
                return None;
            }
            if let Some(job) = state.job.take() {
                return Some(job);
            }
            let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
            state = self.job_ready.wait_timeout(state, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// The newest message headsets get; every one replaces the last.
    pub fn publish(&self, message: Message, epoch: u64) {
        let mut state = self.lock();
        state.seq += 1;
        let seq = state.seq;
        state.shown = Some(Shown { seq, message, epoch, at: Instant::now() });
        drop(state);
        self.shown_changed.notify_all();
    }

    /// The newest message if it's newer than `seen`, waiting up to `timeout` for one.
    pub fn next(&self, seen: u64, timeout: Duration) -> Option<(u64, Message)> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if !state.open {
                return None;
            }
            if let Some(shown) = state.shown.as_ref().filter(|s| s.seq > seen) {
                return Some((shown.seq, shown.message.clone()));
            }
            let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
            state = self.shown_changed.wait_timeout(state, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// A picture that is too old, or was made before something that hides things came on, gives way to black.
    fn settle(&self, epoch: u64) {
        let stale = {
            let state = self.lock();
            state.shown.as_ref().is_some_and(|s| matches!(s.message, Message::Jpeg(_)) && (s.epoch < epoch || s.at.elapsed() > STALE))
        };
        if stale {
            self.publish(Message::Blank, epoch);
        }
    }

    /// The picture for a pair as a JPEG: the eyes side by side, each at its own size. Where something after the 3D step replaced the camera's
    /// picture (the Wait screen) that is what both eyes get.
    pub fn picture(&self, job: &Job, store: &dyn FrameStore, was_ours: &dyn Fn(&Frame) -> bool) -> Option<Vec<u8>> {
        let covered = store
            .outgoing_frame(Some(job.target.as_str()))
            .filter(|o| o.frame.number >= job.number && !Arc::ptr_eq(&o.frame.data, &job.handed) && !was_ours(&o.frame))
            .and_then(|o| Rgb::from_frame(&o.frame));
        let (left, right, tune);
        let (one, two);
        match covered {
            Some(rgb) => {
                one = rgb;
                (left, right, tune) = (&one, &one, Tune::default());
            }
            None => {
                one = Rgb::from_frame(&job.left)?;
                two = Rgb::from_frame(&job.right)?;
                (left, right, tune) = (&one, &two, job.tune.clone());
            }
        }
        let (w, h) = eye_size(left.w, left.h, self.max_eye);
        let packed = stereo::side_by_side(left, right, w * 2, h, &tune);
        let jpeg = encode(&packed, QUALITY);
        self.built.fetch_add(1, Ordering::SeqCst);
        jpeg
    }

    /// The worker: until close(), builds a picture from each pair handed over, at most 30 a second.
    pub fn run(&self, store: &dyn FrameStore, was_ours: &dyn Fn(&Frame) -> bool) {
        let mut last: Option<Instant> = None;
        while self.is_open() {
            if let Some(rest) = last.and_then(|at| MIN_GAP.checked_sub(at.elapsed())) {
                std::thread::sleep(rest);
            }
            let job = self.take_job(IDLE_WAIT);
            let epoch = store.pipeline_epoch();
            self.settle(epoch);
            let Some(job) = job.filter(|j| j.epoch >= epoch && j.at.elapsed() < JOB_STALE) else { continue };
            last = Some(Instant::now());
            let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.picture(&job, store, was_ours)));
            match made {
                Ok(Some(jpeg)) => {
                    // a hiding step came on while it was made
                    if store.pipeline_epoch() <= job.epoch {
                        self.publish(Message::Jpeg(Arc::new(jpeg)), job.epoch);
                    }
                }
                Ok(None) => {}
                Err(_) => log::error!("Making the headset's picture failed"),
            }
        }
    }
}

// ── WebSocket ──────────────────────────────────────────────────────────────────────────────────────────────────────────────

const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const BINARY: u8 = 0x2;
const TEXT: u8 = 0x1;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xA;

pub fn accept_key(key: &str) -> String {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    ctx.update(key.as_bytes());
    ctx.update(WS_GUID);
    base64::engine::general_purpose::STANDARD.encode(ctx.finish().as_ref())
}

/// The start of a server-to-client frame: final, unmasked.
pub fn frame_header(opcode: u8, len: usize) -> Vec<u8> {
    let mut out = vec![0x80 | opcode];
    if len < 126 {
        out.push(len as u8);
    } else if len < 1 << 16 {
        out.push(126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out
}

fn send(sock: &mut dyn Socket, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    sock.write_all(&frame_header(opcode, payload.len()))?;
    sock.write_all(payload)?;
    sock.flush()
}

/// What the browser sent: false once it closed or sent something that isn't a browser's. Pings are answered.
fn client_frames(buf: &mut Vec<u8>, sock: &mut dyn Socket) -> bool {
    loop {
        if buf.len() < 2 {
            return true;
        }
        let (opcode, masked, mut len, mut at) = (buf[0] & 0x0F, buf[1] & 0x80 != 0, (buf[1] & 0x7F) as usize, 2);
        if len == 126 {
            if buf.len() < 4 {
                return true;
            }
            (len, at) = (u16::from_be_bytes([buf[2], buf[3]]) as usize, 4);
        } else if len == 127 {
            return false;
        }
        // a browser sends this page nothing but control frames
        if !masked || len > 1024 {
            return false;
        }
        if buf.len() < at + 4 + len {
            return true;
        }
        let mask = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
        let payload: Vec<u8> = buf[at + 4..at + 4 + len].iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect();
        buf.drain(..at + 4 + len);
        match opcode {
            CLOSE => {
                let _ = send(sock, CLOSE, &payload[..payload.len().min(2)]);
                return false;
            }
            PING => {
                if send(sock, PONG, &payload[..payload.len().min(125)]).is_err() {
                    return false;
                }
            }
            _ => {}
        }
    }
}

/// One headset: sends the newest picture it hasn't had, never more than one waiting, until it leaves, the link changes or the feed closes.
fn serve_viewer(feed: Arc<Feed>, mut sock: Box<dyn Socket>, generation: u64, _slot: Slot) {
    let _ = sock.set_read_timeout(Some(Duration::from_millis(2)));
    let (mut seen, mut pinged, mut incoming) = (0u64, Instant::now(), Vec::new());
    let mut chunk = [0u8; 1024];
    loop {
        if !feed.is_open() || feed.token_gen.load(Ordering::SeqCst) != generation {
            let _ = send(&mut *sock, CLOSE, &1001u16.to_be_bytes());
            return;
        }
        if let Some((seq, message)) = feed.next(seen, SEND_WAIT) {
            seen = seq;
            let sent = match &message {
                Message::Jpeg(jpeg) => send(&mut *sock, BINARY, jpeg),
                Message::Blank => send(&mut *sock, TEXT, b"blank"),
            };
            if sent.is_err() {
                return;
            }
        }
        if pinged.elapsed() >= PING_EVERY {
            pinged = Instant::now();
            if send(&mut *sock, PING, b"").is_err() {
                return;
            }
        }
        match sock.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                incoming.extend_from_slice(&chunk[..n]);
                if !client_frames(&mut incoming, &mut *sock) {
                    return;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            Err(_) => return,
        }
    }
}

// ── The page ──────────────────────────────────────────────────────────────────────────────────────────────────────────────

fn websocket_request(request: &Request) -> bool {
    request.header("Upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
        && request.header("Sec-WebSocket-Key").is_some_and(|k| !k.is_empty())
        && request.header("Sec-WebSocket-Version") == Some("13")
}

/// Serves the viewer page, and its WebSocket to a link with the token.
pub struct Headset(pub Arc<Feed>);

impl PageHandler for Headset {
    fn handle(&self, request: &Request, stats: &ServerStats) -> Reply {
        match request.path() {
            "/" => {
                stats.note(Step::Pages);
                Reply::new(200, PAGE, "text/html; charset=utf-8")
            }
            "/headset.js" => Reply::new(200, SCRIPT, "text/javascript; charset=utf-8"),
            // a request for the picture that didn't become a WebSocket: a wrong link, or all the places taken
            "/ws" if !self.0.token_ok(request.param("token").unwrap_or("")) => {
                stats.note(Step::Refused);
                Reply::text(403, "This link doesn't work any more.")
            }
            "/ws" => Reply::text(503, "Too many headsets are watching."),
            _ => Reply::text(404, "Not found"),
        }
    }

    fn upgrade(&self, request: &Request, stats: &ServerStats) -> Option<Upgrade> {
        if request.path() != "/ws" || !websocket_request(request) || !self.0.token_ok(request.param("token").unwrap_or("")) {
            return None;
        }
        let key = request.header("Sec-WebSocket-Key")?;
        let slot = self.0.claim()?;
        stats.note(Step::Browsers);
        let (feed, generation) = (self.0.clone(), self.0.token_gen.load(Ordering::SeqCst));
        Some(Upgrade {
            headers: vec![("Upgrade".into(), "websocket".into()), ("Connection".into(), "Upgrade".into()), ("Sec-WebSocket-Accept".into(), accept_key(key))],
            run: Box::new(move |sock| serve_viewer(feed, sock, generation, slot)),
        })
    }
}

#[cfg(test)]
#[path = "stereo_headset_tests.rs"]
mod tests;
