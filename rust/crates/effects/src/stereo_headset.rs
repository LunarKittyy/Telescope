//! The 3D plugin's headset viewer: a page and a WebSocket that send the side by side picture to a headset's browser as JPEG messages.
//!
//! The 3D step hands each pair of eyes it made a picture from to the Feed (only while a headset is connected). One worker thread packs the pair side by
//! side at up to 1920x1080, with the card's Fine tune and Match, into one picture, encodes it and publishes it as the newest message. Each headset
//! has a thread of its own that sends the newest message it hasn't sent yet, so nothing queues: a headset that can't keep up skips pictures. The
//! picture is made from the same frames the camera's picture is, so Privacy blur and every step before the pair apply, and a cover (the Wait
//! screen) on the camera's frame covers the headset's too. The pair is handed over before the camera's frame is finished, so nothing is sent until
//! that finished frame is out and shows whether a cover came on it: an unfinished or missing frame never lets the pair through.
//!
//! Every time the feed is opened it is a new run with its own places for headsets, and a headset belongs to the run and the link it connected under:
//! once either is gone it is disconnected and no longer takes up a place.

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

/// A port of its own, clear of the other Telescope ports.
pub const HEADSET_PORT: u16 = 7484;
/// How many headsets can watch at once.
pub const MAX_VIEWERS: usize = 2;
/// The fewest milliseconds between two pictures: 30 a second.
const MIN_GAP: Duration = Duration::from_millis(33);
/// The biggest the packed picture is sent at: two 1080p eyes get half their width each, as the camera's side by side does. Both eyes at their full
/// width (3840x1080) take about 49 ms a picture to pack and encode, which can't hold 30 a second.
pub const MAX_PACKED: (u32, u32) = (1920, 1080);
const QUALITY: u8 = 80;
/// A picture this old isn't shown any more: the headset goes black until a new one comes.
const STALE: Duration = Duration::from_secs(3);
/// A pair the worker wasn't free for in this long is dropped for the next.
const JOB_STALE: Duration = Duration::from_millis(500);
/// How long the worker and a headset's thread wait for something to do before looking at the clock.
const IDLE_WAIT: Duration = Duration::from_millis(250);
const SEND_WAIT: Duration = Duration::from_millis(30);
const PING_EVERY: Duration = Duration::from_secs(5);
/// How long a pair waits for the camera's finished frame to be out before it's dropped for the next.
const PROOF_WAIT: Duration = Duration::from_millis(150);
const PROOF_POLL: Duration = Duration::from_millis(2);

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
    /// Goes up every time the feed is opened, so what belonged to an earlier run is told apart from this one.
    run: u64,
    token: String,
    /// Goes up with every new token. Read together with the token, under the same lock.
    token_gen: u64,
    /// The headsets of this run; a new run counts from none, whatever the last one's are doing.
    viewers: Arc<AtomicUsize>,
}

/// Pictures from the 3D step to the headsets watching.
pub struct Feed {
    state: Mutex<State>,
    job_ready: Condvar,
    shown_changed: Condvar,
    /// How many pictures were built, for the tests.
    built: AtomicU64,
    max_size: (u32, u32),
    proof_wait: Duration,
    /// Where each headset reads the pipeline epoch just before a picture goes out, so one made before a hiding step came on is never sent.
    epochs: Mutex<Option<Arc<dyn FrameStore>>>,
}

/// Holds one of the places for a headset until it's dropped, and says which run and link it was given under.
pub struct Slot {
    viewers: Arc<AtomicUsize>,
    run: u64,
    generation: u64,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.viewers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What the camera's frame says about a pair.
enum Proof {
    /// The finished frame is the pair's own: nothing covers it.
    Clear,
    /// Something covered the frame: this is what both eyes get.
    Covered(Rgb),
    /// The finished frame isn't out yet (or can't be read): nothing may be sent.
    Missing,
}

/// The size of the packed picture for eyes of w x h: twice the width and the height of an eye, the width shrunk to fit `max` and the height too.
/// Each eye is stretched to its half, which the page puts back to its own shape. Both sides even.
pub fn packed_size(w: usize, h: usize, (max_w, max_h): (u32, u32)) -> (usize, usize) {
    if w == 0 || h == 0 {
        return (2, 2);
    }
    let even = |v: usize, max: u32| v.min(max as usize).max(2) & !1;
    (even(w * 2, max_w), even(h, max_h))
}

/// The picture as a JPEG.
pub fn encode(rgb: &Rgb, quality: u8) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(rgb.w * rgb.h / 6);
    JpegEncoder::new_with_quality(&mut out, quality).encode(&rgb.data, rgb.w as u32, rgb.h as u32, ExtendedColorType::Rgb8).ok()?;
    Some(out)
}

impl Feed {
    pub fn new(token: &str) -> Feed {
        Feed::with_max_size(token, MAX_PACKED)
    }

    pub fn with_max_size(token: &str, max_size: (u32, u32)) -> Feed {
        Feed {
            state: Mutex::new(State {
                job: None, shown: None, seq: 0, open: false, run: 0, token: token.into(), token_gen: 0, viewers: Arc::new(AtomicUsize::new(0)),
            }),
            job_ready: Condvar::new(),
            shown_changed: Condvar::new(),
            built: AtomicU64::new(0),
            epochs: Mutex::new(None),
            max_size,
            proof_wait: PROOF_WAIT,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A new run, ready for a worker and headsets. Headsets of an earlier run are over, and don't hold places in this one.
    pub fn open(&self) {
        let mut state = self.lock();
        (state.open, state.job, state.shown) = (true, None, None);
        state.run += 1;
        state.viewers = Arc::new(AtomicUsize::new(0));
        drop(state);
        self.shown_changed.notify_all();
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

    /// A new link: headsets on the old one are disconnected. The token and its generation change in one step.
    pub fn set_token(&self, token: &str) {
        let mut state = self.lock();
        state.token = token.into();
        state.token_gen += 1;
        drop(state);
        self.shown_changed.notify_all();
    }

    /// How many headsets this run has.
    pub fn viewers(&self) -> usize {
        self.lock().viewers.load(Ordering::SeqCst)
    }

    /// Whether anything needs pictures: a headset is connected. Without one the step builds nothing.
    pub fn wanted(&self) -> bool {
        let state = self.lock();
        state.open && state.viewers.load(Ordering::SeqCst) > 0
    }

    /// Whether a headset holding this place is still wanted: its run is on and its link is the current one.
    fn current(&self, slot: &Slot) -> bool {
        let state = self.lock();
        state.open && state.run == slot.run && state.token_gen == slot.generation
    }

    /// Whether a pair waits for the worker.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> bool {
        self.lock().job.is_some()
    }

    pub fn built(&self) -> u64 {
        self.built.load(Ordering::SeqCst)
    }

    /// A place for one more headset in this run, None when MAX_VIEWERS are watching or the feed is closed.
    pub fn claim(&self) -> Option<Slot> {
        let state = self.lock();
        Self::claim_in(&state)
    }

    /// A place for a headset with this link: the check of the link and the generation it is on are one step, so a new link can't slip between them.
    pub fn admit(&self, token: &str) -> Option<Slot> {
        let state = self.lock();
        if token.is_empty() || !same(token, &state.token) {
            return None;
        }
        Self::claim_in(&state)
    }

    fn claim_in(state: &State) -> Option<Slot> {
        // places are only given out under the lock, so counting and taking one can't be raced
        if !state.open || state.viewers.load(Ordering::SeqCst) >= MAX_VIEWERS {
            return None;
        }
        state.viewers.fetch_add(1, Ordering::SeqCst);
        Some(Slot { viewers: state.viewers.clone(), run: state.run, generation: state.token_gen })
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

    /// Sends the pipeline epochs of this store to the headsets' final check.
    pub fn watch_epochs(&self, store: Arc<dyn FrameStore>) {
        *self.epochs.lock().unwrap_or_else(|e| e.into_inner()) = Some(store);
    }

    /// The message as a headset may have it right now: a picture made before the pipeline's latest change gives way to black.
    fn vet(&self, seq: u64, message: Message) -> Message {
        let Some(store) = self.epochs.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return message };
        let made = self.lock().shown.as_ref().filter(|s| s.seq == seq).map(|s| s.epoch);
        match (&message, made) {
            (Message::Jpeg(_), Some(epoch)) if epoch < store.pipeline_epoch() => Message::Blank,
            (Message::Jpeg(_), None) => Message::Blank,
            _ => message,
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
        self.next_where(seen, timeout, &|_| true, &|_| true)
    }

    /// next(), for a headset that stops getting anything the moment `valid` says its run or link is over, and that only takes a message `wanted` says yes to.
    fn next_where(&self, seen: u64, timeout: Duration, valid: &dyn Fn(&State) -> bool, wanted: &dyn Fn(&Message) -> bool) -> Option<(u64, Message)> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if !state.open || !valid(&state) {
                return None;
            }
            if let Some(shown) = state.shown.as_ref().filter(|s| s.seq > seen && wanted(&s.message)) {
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

    /// What the camera's frame says about the pair. Only a finished frame at least as new as the pair counts: it is either the pair's own or
    /// something that came after it, and anything less (no frame out yet, the one before the pair, one that can't be read) proves nothing.
    fn proof(&self, job: &Job, store: &dyn FrameStore, was_ours: &dyn Fn(&Frame) -> bool) -> Proof {
        let Some(out) = store.outgoing_frame(Some(job.target.as_str())).filter(|o| o.frame.number >= job.number) else { return Proof::Missing };
        if Arc::ptr_eq(&out.frame.data, &job.handed) || was_ours(&out.frame) {
            return Proof::Clear;
        }
        Rgb::from_frame(&out.frame).map_or(Proof::Missing, Proof::Covered)
    }

    /// Waits for the proof, up to `proof_wait`, and for as long as nothing newer is waiting and the feed is open.
    fn proven(&self, job: &Job, store: &dyn FrameStore, was_ours: &dyn Fn(&Frame) -> bool) -> Option<Proof> {
        let deadline = Instant::now() + self.proof_wait;
        loop {
            match self.proof(job, store, was_ours) {
                Proof::Missing => {}
                found => return Some(found),
            }
            if Instant::now() >= deadline {
                return None;
            }
            {
                let state = self.lock();
                if !state.open || state.job.is_some() {
                    return None;
                }
            }
            std::thread::sleep(PROOF_POLL);
        }
    }

    /// The picture for a pair as a JPEG: the eyes side by side, at up to MAX_PACKED. Where something after the 3D step replaced the camera's
    /// picture (the Wait screen) that is what both eyes get. None when the camera's finished frame isn't out to say which it is.
    pub fn picture(&self, job: &Job, store: &dyn FrameStore, was_ours: &dyn Fn(&Frame) -> bool) -> Option<Vec<u8>> {
        let proof = self.proven(job, store, was_ours)?;
        let (left, right, tune);
        let (one, two);
        match proof {
            Proof::Covered(rgb) => {
                one = rgb;
                (left, right, tune) = (&one, &one, Tune::default());
            }
            _ => {
                one = Rgb::from_frame(&job.left)?;
                two = Rgb::from_frame(&job.right)?;
                (left, right, tune) = (&one, &two, job.tune.clone());
            }
        }
        let (w, h) = packed_size(left.w, left.h, self.max_size);
        let packed = stereo::side_by_side(left, right, w, h, &tune);
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
/// What the page sends after it has decoded a picture: the next one isn't sent before it.
const ACK: &[u8] = b"ack";

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

/// What the browser sent: false once it closed or sent something that isn't a browser's. Pings are answered, and `acked` is set when the page says it
/// has decoded a picture.
fn client_frames(buf: &mut Vec<u8>, sock: &mut dyn Socket, acked: &mut bool) -> bool {
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
        // a browser sends this page nothing but control frames and a short acknowledgement
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
            PING if send(sock, PONG, &payload[..payload.len().min(125)]).is_err() => return false,
            TEXT if payload == ACK => *acked = true,
            _ => {}
        }
    }
}

/// One headset: sends the newest picture it hasn't had, and the next only once the page has said it decoded the last, until it leaves, the link changes or the feed closes.
fn serve_viewer(feed: Arc<Feed>, mut sock: Box<dyn Socket>, slot: Slot) {
    let _ = sock.set_read_timeout(Some(Duration::from_millis(2)));
    let (mut seen, mut pinged, mut incoming, mut waiting_for_ack) = (0u64, Instant::now(), Vec::new(), false);
    let mut chunk = [0u8; 1024];
    loop {
        if !feed.current(&slot) {
            let _ = send(&mut *sock, CLOSE, &1001u16.to_be_bytes());
            return;
        }
        // black is always sent, a picture only once the last one is decoded: a slow link never fills up with old pictures
        let valid = |state: &State| state.run == slot.run && state.token_gen == slot.generation;
        if let Some((seq, message)) = feed.next_where(seen, SEND_WAIT, &valid, &|m| !waiting_for_ack || *m == Message::Blank) {
            seen = seq;
            let message = feed.vet(seq, message);
            let sent = match &message {
                Message::Jpeg(jpeg) => {
                    waiting_for_ack = true;
                    send(&mut *sock, BINARY, jpeg)
                }
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
                let mut acked = false;
                if !client_frames(&mut incoming, &mut *sock, &mut acked) {
                    return;
                }
                waiting_for_ack &= !acked;
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
        if request.path() != "/ws" || !websocket_request(request) {
            return None;
        }
        let key = request.header("Sec-WebSocket-Key")?;
        let slot = self.0.admit(request.param("token").unwrap_or(""))?;
        stats.note(Step::Browsers);
        let feed = self.0.clone();
        Some(Upgrade {
            headers: vec![("Upgrade".into(), "websocket".into()), ("Connection".into(), "Upgrade".into()), ("Sec-WebSocket-Accept".into(), accept_key(key))],
            run: Box::new(move |sock| serve_viewer(feed, sock, slot)),
        })
    }
}

#[cfg(test)]
#[path = "stereo_headset_tests.rs"]
mod tests;
