//! What one browser sends (BrowserFeed in browser_server.py), a feed per browser (BrowserHub), and the two ends the
//! stream reads them from: the video as a VideoFeed and the mic as an AudioFeed.
//!
//! Python kept only the newest decoded frame, since aiortc had decoded it already. Here the frames are still encoded
//! and an H.264 frame can't be skipped without breaking the ones after it, so they queue in order; a reader that falls
//! too far behind drops the queue and waits for the next keyframe, which the connection is asked for. They leave here
//! by the sender's own timestamps, since Firefox sends in bursts.

use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use telescope_core::audio::{AudioFeed, AudioPacket};
use telescope_core::media::{Codec, FeedEnd, FeedStopper, Packet, VideoFeed};
use telescope_core::pace::EvenTiming;

/// A page that sends no id of its own (an older one).
pub const DEFAULT_ID: &str = "default";
/// Frames waiting for the stream past this many: it fell behind.
const MAX_QUEUED: usize = 90;
/// A second of mic at most, so a stalled reader can't build up delay.
const MAX_AUDIO_BYTES: usize = 48_000 * 2;
/// How long the video waits for a browser to be there, and for its next frame before the stream reconnects.
pub const OPEN_WAIT: Duration = Duration::from_secs(1);
pub const FRAME_WAIT: Duration = Duration::from_secs(3);
/// The longest a held frame sleeps before looking for a stop.
const HOLD_NAP: Duration = Duration::from_millis(20);

/// What the page says it sends: its size and rate, and what holds it back ("cpu", "bandwidth" or "").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sent {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub limit: String,
}

/// One encoded frame, in the order it came.
#[derive(Clone, Debug)]
pub struct Frame {
    pub seq: u64,
    pub codec: Codec,
    pub data: Vec<u8>,
    /// When the sender made it, on the sender's clock.
    pub made_ns: u64,
    /// When it got here.
    pub arrived_ns: u64,
}

/// Whether the stream can start decoding at this frame.
pub fn is_keyframe(codec: Codec, data: &[u8]) -> bool {
    match codec {
        // An IDR slice or a sequence parameter set in it
        Codec::H264 => data.windows(4).any(|w| w[..3] == [0, 0, 1] && matches!(w[3] & 0x1F, 5 | 7)),
        // The frame tag's first bit is 0 on a keyframe
        Codec::Vp8 => data.first().is_some_and(|b| b & 1 == 0),
        _ => true,
    }
}

#[derive(Default)]
struct State {
    gen: u64,
    connected: bool,
    frames: VecDeque<Frame>,
    frame_seq: u64,
    /// Dropped frames for falling behind: the next one taken is a keyframe.
    resync: bool,
    frame_bytes: u64,
    device: String,
    mic_error: String,
    codec: String,
    rtc_error: String,
    sent: Sent,
    audio: Vec<Arc<AudioQueue>>,
    settings: (u32, u32, u32),
    settings_seq: u64,
    camera: String,
}

/// Frames and audio from whichever browser is connected; thread-safe. A connection is a generation: a newer one takes
/// over, and the frames of an older one are never handed out after it.
pub struct BrowserFeed {
    state: Mutex<State>,
    changed: Condvar,
    /// The stream lost its place: the connection asks the browser for a keyframe.
    keyframe: AtomicBool,
}

impl Default for BrowserFeed {
    fn default() -> Self {
        BrowserFeed::new()
    }
}

impl BrowserFeed {
    pub fn new() -> BrowserFeed {
        let state = State { settings: (1280, 720, 30), ..State::default() };
        BrowserFeed { state: Mutex::new(state), changed: Condvar::new(), keyframe: AtomicBool::new(false) }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // The server's side

    pub fn connect(&self) -> u64 {
        let mut s = self.lock();
        s.gen += 1;
        s.connected = true;
        s.frames.clear();
        s.resync = false;
        s.device.clear();
        s.mic_error.clear();
        s.codec.clear();
        s.rtc_error.clear();
        s.sent = Sent::default();
        self.changed.notify_all();
        s.gen
    }

    pub fn disconnect(&self, gen: u64) {
        let mut s = self.lock();
        if gen != s.gen {
            return;
        }
        s.connected = false;
        s.frames.clear();
        self.changed.notify_all();
    }

    pub fn current(&self, gen: u64) -> bool {
        self.lock().gen == gen
    }

    pub fn put_video(&self, gen: u64, codec: Codec, data: Vec<u8>, frame_bytes: u64, made_ns: u64) {
        let mut s = self.lock();
        if gen != s.gen {
            return;
        }
        if s.resync && !is_keyframe(codec, &data) {
            self.keyframe.store(true, Ordering::SeqCst);
            return;
        }
        s.resync = false;
        if s.frames.len() >= MAX_QUEUED {
            // Fell behind: start again at the next keyframe instead of building up delay
            s.frames.clear();
            s.resync = !is_keyframe(codec, &data);
            self.keyframe.store(true, Ordering::SeqCst);
            if s.resync {
                return;
            }
        }
        s.frame_seq += 1;
        let seq = s.frame_seq;
        s.frames.push_back(Frame { seq, codec, data, made_ns, arrived_ns: telescope_core::time::monotonic_ns() });
        if frame_bytes > 0 {
            s.frame_bytes = frame_bytes;
        }
        self.changed.notify_all();
    }

    pub fn put_audio(&self, gen: u64, pcm: &[u8]) {
        let streams = {
            let s = self.lock();
            if gen != s.gen {
                return;
            }
            s.audio.clone()
        };
        for stream in streams {
            stream.push(pcm);
        }
    }

    /// A keyframe, soon: the stream lost its place.
    pub fn ask_keyframe(&self) {
        self.keyframe.store(true, Ordering::SeqCst);
    }

    /// Whether the stream asked for a keyframe since this was last asked.
    pub fn take_keyframe_wish(&self) -> bool {
        self.keyframe.swap(false, Ordering::SeqCst)
    }

    pub fn note_hello(&self, gen: u64, device: &str, mic_error: &str) {
        let mut s = self.lock();
        if gen == s.gen {
            (s.device, s.mic_error) = (device.into(), mic_error.into());
        }
    }

    pub fn note_rtc(&self, gen: u64, codec: &str, error: &str) {
        let mut s = self.lock();
        if gen == s.gen {
            (s.codec, s.rtc_error) = (codec.into(), error.into());
            self.changed.notify_all();
        }
    }

    pub fn note_sent(&self, gen: u64, sent: Sent) {
        let mut s = self.lock();
        if gen == s.gen {
            s.sent = sent;
        }
    }

    /// (change counter, what the page should capture), sent to the page when the counter moves.
    pub fn page_config(&self) -> (u64, Value) {
        let s = self.lock();
        let (width, height, fps) = s.settings;
        let mut cfg = json!({"width": width, "height": height, "fps": fps, "audio": !s.audio.is_empty(),
                             "max_kbps": super::rtc::max_kbps_for(width, height, fps)});
        if !s.camera.is_empty() {
            cfg["camera"] = json!(s.camera);
        }
        (s.settings_seq, cfg)
    }

    /// The most the page may send at the size and rate it's asked for.
    pub fn max_kbps(&self) -> u32 {
        let (w, h, fps) = self.lock().settings;
        super::rtc::max_kbps_for(w, h, fps)
    }

    // The desktop's side

    pub fn connected(&self) -> bool {
        self.lock().connected
    }

    /// Moves on each time a browser connects to it.
    pub fn generation(&self) -> u64 {
        self.lock().gen
    }

    pub fn device(&self) -> String {
        self.lock().device.clone()
    }

    pub fn mic_error(&self) -> String {
        self.lock().mic_error.clone()
    }

    pub fn codec(&self) -> String {
        self.lock().codec.clone()
    }

    pub fn rtc_error(&self) -> String {
        self.lock().rtc_error.clone()
    }

    pub fn sent(&self) -> Sent {
        self.lock().sent.clone()
    }

    pub fn frame_bytes(&self) -> u64 {
        self.lock().frame_bytes
    }

    pub fn set_capture(&self, width: u32, height: u32, fps: u32) {
        let mut s = self.lock();
        s.settings = (width, height, fps);
        s.settings_seq += 1;
    }

    /// The virtual camera this browser streams to ("" for none); its page shows it.
    pub fn set_camera(&self, name: &str) {
        let mut s = self.lock();
        if s.camera != name {
            s.camera = name.into();
            s.settings_seq += 1;
        }
    }

    /// The connected browser's generation, waiting up to timeout for one; None if nothing connected.
    pub fn wait_connected(&self, timeout: Duration) -> Option<u64> {
        let deadline = Instant::now() + timeout;
        let mut s = self.lock();
        while !s.connected {
            let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
            s = self.changed.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        Some(s.gen)
    }

    /// The codec connection gen's video comes in, once its offer is answered; None if it goes or isn't answered in time.
    pub fn wait_codec(&self, gen: u64, timeout: Duration) -> Option<Codec> {
        let deadline = Instant::now() + timeout;
        let mut s = self.lock();
        loop {
            if s.gen != gen || !s.connected || !s.rtc_error.is_empty() {
                return None;
            }
            match s.codec.as_str() {
                "H264" => return Some(Codec::H264),
                "VP8" => return Some(Codec::Vp8),
                _ => {}
            }
            let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
            s = self.changed.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// The next frame from connection gen, waiting up to timeout; None once it's gone or quiet.
    pub fn next_frame(&self, gen: u64, timeout: Duration) -> Option<Frame> {
        let deadline = Instant::now() + timeout;
        let mut s = self.lock();
        loop {
            if s.gen != gen || !s.connected {
                return None;
            }
            if let Some(frame) = s.frames.pop_front() {
                return Some(frame);
            }
            let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
            s = self.changed.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// The browser's mic for the virtual microphone. It outlives the browser reconnecting (silence meanwhile); only
    /// dropping it ends it. Refused, with the browser's reason, while the connected browser has no mic.
    pub fn open_audio(self: &Arc<Self>) -> Result<BrowserAudio, String> {
        let mut s = self.lock();
        if s.connected && !s.mic_error.is_empty() {
            return Err(s.mic_error.clone());
        }
        let queue = Arc::new(AudioQueue::default());
        s.audio.push(queue.clone());
        s.settings_seq += 1; // the page starts sending audio
        Ok(BrowserAudio { queue, feed: self.clone() })
    }

    fn drop_audio(&self, queue: &Arc<AudioQueue>) {
        let mut s = self.lock();
        if let Some(i) = s.audio.iter().position(|q| Arc::ptr_eq(q, queue)) {
            s.audio.remove(i);
            s.settings_seq += 1;
        }
    }
}

/// A feed per browser, by the id its page keeps; thread-safe. can_join(id) decides whether a browser that isn't known
/// yet gets one (false: every camera is taken). Given a feed, every browser shares it.
pub struct BrowserHub {
    can_join: Box<dyn Fn(&str) -> bool + Send + Sync>,
    single: Option<Arc<BrowserFeed>>,
    /// In the order they joined.
    feeds: Mutex<Vec<(String, Arc<BrowserFeed>)>>,
    capture: Mutex<(u32, u32, u32)>,
}

impl BrowserHub {
    pub fn new(can_join: impl Fn(&str) -> bool + Send + Sync + 'static) -> BrowserHub {
        BrowserHub { can_join: Box::new(can_join), single: None, feeds: Mutex::default(), capture: Mutex::new((1280, 720, 30)) }
    }

    /// Every browser into this one feed.
    pub fn single(feed: Arc<BrowserFeed>) -> BrowserHub {
        BrowserHub { single: Some(feed), ..BrowserHub::new(|_| true) }
    }

    fn feeds_lock(&self) -> MutexGuard<'_, Vec<(String, Arc<BrowserFeed>)>> {
        self.feeds.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn join(&self, bid: &str) -> Option<Arc<BrowserFeed>> {
        if let Some(feed) = &self.single {
            return Some(feed.clone());
        }
        if let Some(feed) = self.feed(bid) {
            return Some(feed);
        }
        if !(self.can_join)(bid) {
            return None;
        }
        let mut feeds = self.feeds_lock();
        if let Some((_, feed)) = feeds.iter().find(|(id, _)| id == bid) {
            return Some(feed.clone());
        }
        let feed = Arc::new(BrowserFeed::new());
        let (w, h, fps) = *self.capture.lock().unwrap_or_else(|e| e.into_inner());
        feed.set_capture(w, h, fps);
        feeds.push((bid.into(), feed.clone()));
        Some(feed)
    }

    pub fn feed(&self, bid: &str) -> Option<Arc<BrowserFeed>> {
        if let Some(feed) = &self.single {
            return Some(feed.clone());
        }
        self.feeds_lock().iter().find(|(id, _)| id == bid).map(|(_, f)| f.clone())
    }

    /// Every browser that connected since the server started (or was dropped), in the order they came.
    pub fn feeds(&self) -> Vec<(String, Arc<BrowserFeed>)> {
        if let Some(feed) = &self.single {
            return vec![(DEFAULT_ID.into(), feed.clone())];
        }
        self.feeds_lock().clone()
    }

    /// Forget a browser that left, so it needs room to join again.
    pub fn drop_feed(&self, bid: &str) {
        self.feeds_lock().retain(|(id, feed)| id != bid || feed.connected());
    }

    pub fn set_capture(&self, width: u32, height: u32, fps: u32) {
        *self.capture.lock().unwrap_or_else(|e| e.into_inner()) = (width, height, fps);
        for (_, feed) in self.feeds() {
            feed.set_capture(width, height, fps);
        }
    }
}

/// The mic's samples between the connection and the reader.
#[derive(Default)]
struct AudioQueue {
    buf: Mutex<(Vec<u8>, bool)>,
    more: Condvar,
}

impl AudioQueue {
    fn push(&self, data: &[u8]) {
        let mut b = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        b.0.extend_from_slice(data);
        if b.0.len() > MAX_AUDIO_BYTES {
            let drop = b.0.len() - MAX_AUDIO_BYTES;
            b.0.drain(..drop - drop % 2);
        }
        self.more.notify_all();
    }

    fn close(&self) {
        let mut b = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        b.1 = true;
        self.more.notify_all();
    }
}

struct AudioStopper(Arc<AudioQueue>);

impl FeedStopper for AudioStopper {
    fn stop(&self) {
        self.0.close();
    }
}

/// The browser's mic as the audio worker reads it (BrowserAudioStream).
pub struct BrowserAudio {
    queue: Arc<AudioQueue>,
    feed: Arc<BrowserFeed>,
}

impl BrowserAudio {
    /// Up to n bytes as soon as there are any (read1); empty once it's closed.
    pub fn read_some(&self, n: usize) -> Vec<u8> {
        let mut b = self.queue.buf.lock().unwrap_or_else(|e| e.into_inner());
        while b.0.is_empty() && !b.1 {
            b = self.queue.more.wait_timeout(b, Duration::from_millis(500)).unwrap_or_else(|e| e.into_inner()).0;
        }
        if b.1 {
            return Vec::new();
        }
        let n = n.min(b.0.len());
        b.0.drain(..n).collect()
    }

    pub fn close(&self) {
        self.queue.close();
    }
}

impl AudioFeed for BrowserAudio {
    fn read(&mut self) -> Result<AudioPacket, FeedEnd> {
        let data = self.read_some(4096);
        if data.is_empty() {
            return Err(FeedEnd::Stopped);
        }
        Ok(AudioPacket { data, time_ns: telescope_core::time::monotonic_ns() })
    }

    fn stopper(&self) -> Option<Box<dyn FeedStopper>> {
        Some(Box::new(AudioStopper(self.queue.clone())))
    }
}

impl Drop for BrowserAudio {
    fn drop(&mut self) {
        self.close();
        self.feed.drop_audio(&self.queue);
    }
}

/// The video as the stream reads it (BrowserReader): open waits for a browser and its answered offer, then frames
/// come off the feed in order until the browser goes or goes quiet.
pub struct BrowserVideo {
    feed: Arc<BrowserFeed>,
    gen: u64,
    stopped: Arc<AtomicBool>,
    pace: EvenTiming,
    told: bool,
}

impl BrowserVideo {
    /// None when no browser is there within OPEN_WAIT, or its offer isn't answered within the answer's own wait.
    pub fn open(feed: Arc<BrowserFeed>) -> Option<(Codec, BrowserVideo)> {
        let gen = feed.wait_connected(OPEN_WAIT)?;
        let codec = feed.wait_codec(gen, super::rtc::ANSWER_TIMEOUT)?;
        Some((codec, BrowserVideo { feed, gen, stopped: Arc::default(), pace: EvenTiming::new(), told: false }))
    }

    /// Waits until a frame is due, in short sleeps so a stop is noticed soon. False if the stream stopped.
    fn hold_until(&self, due_ns: u64) -> bool {
        loop {
            let left = due_ns.saturating_sub(telescope_core::time::monotonic_ns());
            if left == 0 {
                return true;
            }
            if self.stopped.load(Ordering::SeqCst) {
                return false;
            }
            std::thread::sleep(Duration::from_nanos(left).min(HOLD_NAP));
        }
    }
}

struct Stopper(Arc<AtomicBool>);

impl FeedStopper for Stopper {
    fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl VideoFeed for BrowserVideo {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        let deadline = Instant::now() + FRAME_WAIT;
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return Err(FeedEnd::Stopped);
            }
            // In short waits, so a stop is noticed soon
            if let Some(frame) = self.feed.next_frame(self.gen, Duration::from_millis(200)) {
                let due = self.pace.due(frame.made_ns, frame.arrived_ns);
                if self.pace.hold_ns() > 0 && !std::mem::replace(&mut self.told, true) {
                    log::info!("Browser camera: frames come in bursts, holding them back a little to even them out");
                }
                if !self.hold_until(due) {
                    return Err(FeedEnd::Stopped);
                }
                return Ok(Packet { data: frame.data, time_ns: frame.arrived_ns });
            }
            if !self.feed.current(self.gen) || !self.feed.connected() || Instant::now() >= deadline {
                return Err(FeedEnd::Closed);
            }
        }
    }

    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(Stopper(self.stopped.clone()))
    }

    fn decoded(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_a_silent_mic_wakes_its_reader_and_unregisters_it() {
        let feed = Arc::new(BrowserFeed::new());
        let mut mic = feed.open_audio().unwrap();
        assert_eq!(feed.page_config().1["audio"], true);
        let stopper = mic.stopper().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let read = mic.read();
            drop(mic);
            let _ = tx.send(matches!(read, Err(FeedEnd::Stopped)));
        });
        std::thread::sleep(Duration::from_millis(50));
        stopper.stop();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(true));
        assert_eq!(feed.page_config().1["audio"], false);
    }
}
