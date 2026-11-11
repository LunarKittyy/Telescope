//! Reading frames off a feed (H264Reader.read): the newest frame decoded from the next data that has any, with the
//! bytes and frames behind it for the stream's throughput and "keeping up" readouts.
//!
//! The feed is read on a thread of its own (the feed-reader thread), so a decoder that stalls for a moment (a card waiting behind a game's graphics work) doesn't
//! stop the reading. The phone would see its connection stop taking data, take that for a bad link and cut the bitrate to the minimum.
//! What was read waits in a queue shared with the stream's thread, which decodes it in order; neither thread waits on the other except to hand over.
//!
//! Two limits keep that queue from turning a slow decoder into seconds of delay. Both only ever drop whole packets up to a keyframe, so what the
//! decoder is given next always decodes cleanly:
//! - age: once the oldest packet has waited STALE_AFTER, everything before the newest keyframe in the queue is dropped (by whichever thread notices
//!   first). With no keyframe in the queue the feed is asked for one (about once a second, then less often while it stays unanswered) and the queue
//!   carries on as it was until it comes. Nothing is dropped before the first picture is out (the stream's setup may be what's waiting), nor from a
//!   feed whose packets are slices of a byte stream (packetized).
//! - size: a queue holding MAX_QUEUED_BYTES makes the feed-reader thread stop reading until it drains, as the connection pushing back, unless the
//!   feed took a keyframe request, which gets room to wait for the keyframe.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use telescope_core::media::{Decoder, FeedEnd, FeedStopper, FrameReader, Packet, ReadFrame, VideoFeed};
use telescope_core::stats::DropReason;

/// Data with no finished frame after this much isn't a stream the phone sent; stop instead of buffering on.
pub const MAX_BYTES_WITHOUT_FRAME: u64 = 32 * 1024 * 1024;

/// How long a packet may wait for the decoder before the backlog is dropped up to a keyframe. It rides out a stall of half a second, and the phone's
/// own budgets (a second over TCP, 300 ms over UDP) only act when its link is slow, so the two don't compete.
const STALE_AFTER: Duration = Duration::from_millis(600);

/// How much read data waits for the decoder before the reading stops too and the connection pushes back: about 2 s of a fast stream, enough to ride out a stall without a slow decoder piling up seconds of delay.
const MAX_QUEUED_BYTES: u64 = 4 * 1024 * 1024;

/// The same once the feed has been asked for a keyframe, which is on its way and has to be read to be of any use.
const MAX_QUEUED_BYTES_ASKED: u64 = 16 * 1024 * 1024;

/// How often a feed is asked for a keyframe while the queue is stale and holds none, and how far that backs off while the asks go unanswered (a
/// phone on intra refresh has no IDR to give).
const KEY_ASK_EVERY: Duration = Duration::from_secs(1);
const KEY_ASK_BACKOFF: Duration = Duration::from_secs(5);

/// The longest a decoded picture waits to be shown, whatever the feed says.
const MAX_HOLD: Duration = Duration::from_millis(250);

struct Queued {
    packet: Packet,
    /// When the feed-reader thread had it.
    at: Instant,
    key: bool,
    /// When the feed would like it shown (monotonic nanoseconds).
    show_at_ns: Option<u64>,
}

#[derive(Default)]
struct Queue {
    packets: VecDeque<Queued>,
    bytes: u64,
    /// How the feed ended, once the packets before it are gone.
    end: Option<FeedEnd>,
    /// Packets and bytes dropped for age that the stream's thread hasn't counted yet.
    dropped: u32,
    dropped_bytes: u64,
    /// The queue is stale and has no keyframe to go to: the feed-reader thread asks for one.
    want_key: bool,
    /// A picture has come out of the decoder, so what's queued is no longer what its setup waits for (a TCP phone sends its SPS and PPS once).
    shown: bool,
}

impl Queue {
    fn push(&mut self, q: Queued) {
        self.bytes += q.packet.data.len() as u64;
        self.packets.push_back(q);
    }

    /// Drops what has waited too long, up to the newest keyframe behind it (a keyframe in front has nothing to drop up to).
    fn trim(&mut self, now: Instant, stale_after: Duration) {
        let Some(oldest) = self.packets.front() else { return };
        if !self.shown || now.saturating_duration_since(oldest.at) < stale_after {
            return;
        }
        let Some(key) = self.packets.iter().rposition(|q| q.key).filter(|&i| i > 0) else {
            self.want_key = true;
            return;
        };
        let bytes: usize = self.packets.drain(..key).map(|q| q.packet.data.len()).sum();
        self.bytes -= bytes as u64;
        self.dropped += key as u32;
        self.dropped_bytes += bytes as u64;
        // A packet counts as a frame, which a chunk of TCP's byte stream only roughly is
        telescope_core::stats::note_drop(DropReason::Paced, key as u64);
    }
}

/// What the two threads hand over through.
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    stale_after: Duration,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Ends the queue when the feed-reader thread does, however it does (or never starts).
struct Ender(Arc<Shared>);

impl Drop for Ender {
    fn drop(&mut self) {
        self.0.lock().end.get_or_insert(FeedEnd::Closed);
        self.0.ready.notify_all();
    }
}

pub struct Reader {
    shared: Arc<Shared>,
    /// Pictures decoded that the feed hasn't been told of yet (it's read on the other thread).
    decoded: Arc<AtomicU32>,
    /// The feed's own flag for a picture decoded, which reaches it while it's blocked reading.
    decoded_flag: Option<Arc<AtomicBool>>,
    /// The feed's count of frames it skipped, which still arrived.
    skipped: Option<Arc<AtomicU32>>,
    stop: Arc<dyn FeedStopper>,
    /// Set when the Reader is dropped, which the feed-reader thread can't see while the queue is full and it isn't sending.
    gone: Arc<AtomicBool>,
    decoder: Box<dyn Decoder>,
    max_bytes: u64,
    pending_bytes: u64,
    pending_frames: u32,
    /// A picture has been returned (Queue::shown).
    shown: bool,
}

/// Ends the feed's blocked read, for however many copies of it are asked for.
struct Stop(Arc<dyn FeedStopper>);

impl FeedStopper for Stop {
    fn stop(&self) {
        self.0.stop();
    }
}

impl Reader {
    pub fn new(feed: Box<dyn VideoFeed>, decoder: Box<dyn Decoder>) -> Reader {
        Reader::with_budget(feed, decoder, STALE_AFTER)
    }

    fn with_budget(mut feed: Box<dyn VideoFeed>, decoder: Box<dyn Decoder>, stale_after: Duration) -> Reader {
        let stop: Arc<dyn FeedStopper> = Arc::from(feed.stopper());
        let decoded_flag = feed.decoded_flag();
        let skipped = feed.skipped_frames();
        let shared = Arc::new(Shared { queue: Mutex::default(), ready: Condvar::new(), stale_after });
        let decoded = Arc::new(AtomicU32::new(0));
        let (sh, d) = (shared.clone(), decoded.clone());
        let gone = Arc::new(AtomicBool::new(false));
        let g = gone.clone();
        // What the feed drops on its own thread (UDP giving up on a frame) counts for the stream that opened it
        let counting = telescope_core::stats::current();
        // Made here, so a thread that can't start still ends the queue when its closure is dropped
        let ends = Ender(shared.clone());
        let spawned = std::thread::Builder::new().name("feed-reader".into()).spawn(move || {
            let _ends = ends;
            let _counting = counting.as_ref().map(telescope_core::stats::bind);
            let _priority = telescope_core::system::prioritise_thread("frame receive");
            let (mut last_ask, mut can_ask, mut ask_every) = (None::<Instant>, false, KEY_ASK_EVERY);
            loop {
                for _ in 0..d.swap(0, Ordering::Relaxed) {
                    feed.decoded();
                }
                if g.load(Ordering::Relaxed) {
                    return;
                }
                let (full, ask) = {
                    let mut q = sh.lock();
                    q.trim(Instant::now(), sh.stale_after);
                    let limit = if can_ask { MAX_QUEUED_BYTES_ASKED } else { MAX_QUEUED_BYTES };
                    (q.bytes > limit, std::mem::take(&mut q.want_key))
                };
                if ask && last_ask.is_none_or(|t| t.elapsed() >= ask_every) {
                    last_ask = Some(Instant::now());
                    can_ask = feed.request_keyframe();
                    // An ask that goes unanswered is made less often
                    ask_every = (ask_every * 2).min(KEY_ASK_BACKOFF);
                }
                if full {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                let read = feed.read();
                match read {
                    Ok(packet) => {
                        // Slices of a byte stream have no keyframes to land on: cut anywhere, they cut a picture in half
                        let key = feed.packetized() && feed.is_keyframe(&packet.data);
                        let show_at_ns = feed.show_at_ns();
                        if key {
                            // The one it asked for (or the phone's own) is here: no more room to wait for it, and asks start over
                            (can_ask, ask_every) = (false, KEY_ASK_EVERY);
                        }
                        let mut q = sh.lock();
                        q.push(Queued { packet, at: Instant::now(), key, show_at_ns });
                        q.trim(Instant::now(), sh.stale_after);
                        drop(q);
                        sh.ready.notify_one();
                    }
                    Err(end) => {
                        sh.lock().end = Some(end);
                        sh.ready.notify_all();
                        return;
                    }
                }
            }
        });
        if let Err(e) = spawned {
            log::error!("Couldn't start the thread that reads the stream: {e}");
        }
        Reader { shared, decoded, decoded_flag, skipped, stop, gone, decoder, max_bytes: MAX_BYTES_WITHOUT_FRAME, pending_bytes: 0, pending_frames: 0, shown: false }
    }

    /// The next packet read, waiting for one; the end of the feed once the thread has gone.
    fn next(&mut self) -> Result<Queued, FeedEnd> {
        let mut q = self.shared.lock();
        loop {
            q.trim(Instant::now(), self.shared.stale_after);
            self.pending_frames += std::mem::take(&mut q.dropped);
            self.pending_bytes += std::mem::take(&mut q.dropped_bytes);
            if let Some(next) = q.packets.pop_front() {
                q.bytes -= next.packet.data.len() as u64;
                return Ok(next);
            }
            if let Some(end) = &q.end {
                return Err(end.clone());
            }
            q = self.shared.ready.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.gone.store(true, Ordering::Relaxed);
    }
}

impl FrameReader for Reader {
    fn read(&mut self) -> Result<ReadFrame, FeedEnd> {
        loop {
            let Queued { packet, show_at_ns, .. } = self.next()?;
            self.pending_bytes += packet.data.len() as u64;
            if self.pending_bytes > self.max_bytes {
                return Err(FeedEnd::Error("data that never makes a frame".into()));
            }
            let decoded = self.decoder.decode(&packet);
            self.pending_frames += decoded.count;
            let finished = decoded.count.max(decoded.newest.is_some() as u32);
            telescope_core::stats::note_decoded(finished as u64);
            // A burst shows only its newest frame, to stay live
            if decoded.newest.is_some() && finished > 1 {
                telescope_core::stats::note_drop(DropReason::Paced, finished as u64 - 1);
            }
            if let Some(skipped) = &self.skipped {
                let skipped = skipped.swap(0, Ordering::Relaxed);
                self.pending_frames += skipped;
                telescope_core::stats::note_drop(DropReason::WaitingForKeyframe, skipped as u64);
            }
            if let Some(frame) = decoded.newest {
                if !self.shown {
                    self.shown = true;
                    self.shared.lock().shown = true;
                }
                match &self.decoded_flag {
                    Some(flag) => flag.store(true, Ordering::SeqCst),
                    None => {
                        self.decoded.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // Decoded already, so the wait for the time the feed wants it shown (a browser's bursts evened out) hides behind the decoding
                if let Some(at) = show_at_ns {
                    let left = at.saturating_sub(telescope_core::time::monotonic_ns());
                    std::thread::sleep(Duration::from_nanos(left).min(MAX_HOLD));
                }
                let bytes = std::mem::take(&mut self.pending_bytes);
                // Frames that arrived for it, the ones skipped to keep up included
                let count = std::mem::take(&mut self.pending_frames);
                return Ok(ReadFrame { frame, bytes, count });
            }
        }
    }

    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(Stop(self.stop.clone()))
    }

    fn hardware(&self) -> bool {
        self.decoder.hardware()
    }
}

#[cfg(all(test, ffmpeg))]
mod tests {
    use super::*;
    use crate::h264::H264;
    use crate::testenc;
    use telescope_core::media::Packet;

    struct Chunks(Vec<Vec<u8>>);

    struct NoStop;

    impl FeedStopper for NoStop {
        fn stop(&self) {}
    }

    impl VideoFeed for Chunks {
        fn read(&mut self) -> Result<Packet, FeedEnd> {
            if self.0.is_empty() {
                return Err(FeedEnd::Closed);
            }
            Ok(Packet { data: self.0.remove(0), time_ns: 0 })
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            Box::new(NoStop)
        }
    }

    /// A peer that keeps sending bytes that never make a frame.
    struct Endless;

    impl VideoFeed for Endless {
        fn read(&mut self) -> Result<Packet, FeedEnd> {
            Ok(Packet { data: vec![0; 64 * 1024], time_ns: 0 })
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            Box::new(NoStop)
        }
    }

    #[test]
    fn reads_frames_then_reports_the_end() {
        let Some(packets) = testenc::h264(&[30, 90, 150, 210], 64, 48, 5) else { return };
        let total: usize = packets.iter().map(Vec::len).sum();
        let mut r = Reader::new(Box::new(Chunks(packets)), Box::new(H264::new(false, None).unwrap()));
        let (mut frames, mut bytes) = (0, 0);
        loop {
            match r.read() {
                Ok(f) => {
                    assert_eq!((f.frame.width, f.frame.height), (64, 48));
                    frames += 1;
                    bytes += f.bytes;
                }
                Err(end) => {
                    assert_eq!(end, FeedEnd::Closed);
                    break;
                }
            }
        }
        assert!(frames > 0 && bytes > 0 && bytes as usize <= total);
    }

    #[test]
    fn a_burst_shows_its_newest_frame_but_counts_every_frame_that_arrived() {
        // Frames that come in together are all decoded; only the newest is shown, to stay live, but each counts as
        // arrived, or the stream would look like it's falling behind
        let Some(packets) = testenc::h264(&[30, 60, 90, 120, 150, 180], 64, 48, 5) else { return };
        let mut r = Reader::new(Box::new(Chunks(vec![packets[..3].concat(), packets[3..].concat()])), Box::new(H264::new(false, None).unwrap()));
        let (mut shown, mut arrived) = (0, 0);
        while let Ok(f) = r.read() {
            shown += 1;
            arrived += f.count;
        }
        assert!(arrived == 6 && shown < arrived);
    }

    #[test]
    fn data_that_never_makes_a_frame_ends_the_stream() {
        let mut r = Reader::new(Box::new(Endless), Box::new(H264::new(false, None).unwrap()));
        r.max_bytes = 1024 * 1024;
        assert!(matches!(r.read(), Err(FeedEnd::Error(_))));
    }
}

#[cfg(test)]
mod stall_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use telescope_core::media::Decoded;

    struct NoStop;

    impl FeedStopper for NoStop {
        fn stop(&self) {}
    }

    /// Hands out small packets, counting the reads, until it has given `limit`.
    struct Counting {
        reads: Arc<AtomicUsize>,
        limit: usize,
    }

    impl VideoFeed for Counting {
        fn read(&mut self) -> Result<Packet, FeedEnd> {
            if self.reads.fetch_add(1, Ordering::SeqCst) >= self.limit {
                return Err(FeedEnd::Closed);
            }
            Ok(Packet { data: vec![1; 100], time_ns: 0 })
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            Box::new(NoStop)
        }
    }

    /// A decoder stalled on the graphics card for its first packet, noting how much of the feed was read when it came out of it.
    struct Stalled {
        reads: Arc<AtomicUsize>,
        read_by_then: Arc<AtomicUsize>,
        stalled: bool,
    }

    impl Decoder for Stalled {
        fn decode(&mut self, _packet: &Packet) -> Decoded {
            if !self.stalled {
                self.stalled = true;
                std::thread::sleep(Duration::from_millis(300));
                self.read_by_then.store(self.reads.load(Ordering::SeqCst), Ordering::SeqCst);
            }
            Decoded { newest: None, count: 0 }
        }
        fn hardware(&self) -> bool {
            true
        }
    }

    #[test]
    fn the_feed_keeps_being_read_while_the_decoder_stalls() {
        let reads = Arc::new(AtomicUsize::new(0));
        let read_by_then = Arc::new(AtomicUsize::new(0));
        let decoder = Stalled { reads: reads.clone(), read_by_then: read_by_then.clone(), stalled: false };
        let mut r = Reader::new(Box::new(Counting { reads, limit: 20 }), Box::new(decoder));
        assert!(matches!(r.read(), Err(FeedEnd::Closed)));
        // Without the thread, only the one packet would have been read by the time the decoder was out of its stall
        assert!(read_by_then.load(Ordering::SeqCst) > 20, "{}", read_by_then.load(Ordering::SeqCst));
    }

    /// A feed with more data than the queue holds that notes when it is dropped, which is when the connection closes.
    struct Flood(Arc<AtomicBool>);

    impl Drop for Flood {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl VideoFeed for Flood {
        fn read(&mut self) -> Result<Packet, FeedEnd> {
            Ok(Packet { data: vec![0; 1024 * 1024], time_ns: 0 })
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            Box::new(NoStop)
        }
    }

    #[test]
    fn dropping_the_reader_with_a_full_queue_lets_go_of_the_feed() {
        let closed = Arc::new(AtomicBool::new(false));
        let r = Reader::new(Box::new(Flood(closed.clone())), Box::new(Stalled { reads: Arc::default(), read_by_then: Arc::default(), stalled: true }));
        // Long enough for the queue to fill and the thread to start waiting on it
        std::thread::sleep(Duration::from_millis(200));
        drop(r);
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while !closed.load(Ordering::SeqCst) && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(closed.load(Ordering::SeqCst), "the feed-reader thread kept the feed open");
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize};
    use std::sync::Mutex;
    use std::time::Instant;
    use telescope_core::frame::{Frame, PixelFormat};
    use telescope_core::media::Decoded;

    const KEY_EVERY: usize = 30;

    #[derive(Default)]
    struct Seen {
        /// Bytes the feed has handed over, and bytes the decoder has been given.
        read: AtomicU64,
        decoded: AtomicU64,
        max_backlog: AtomicU64,
        max_age_ns: AtomicU64,
        ages_ns: Mutex<Vec<u64>>,
        read_times: Mutex<Vec<Instant>>,
        key_asks: AtomicUsize,
    }

    impl Seen {
        fn note_backlog(&self) {
            let backlog = self.read.load(Ordering::SeqCst).saturating_sub(self.decoded.load(Ordering::SeqCst));
            self.max_backlog.fetch_max(backlog, Ordering::SeqCst);
        }
    }

    struct NoStop;

    impl FeedStopper for NoStop {
        fn stop(&self) {}
    }

    /// A phone sending `size` byte pictures at a steady rate, a keyframe every second.
    struct Sender {
        seen: Arc<Seen>,
        size: usize,
        period: Duration,
        left: usize,
        index: usize,
        next: Instant,
        key_wanted: bool,
    }

    impl VideoFeed for Sender {
        fn read(&mut self) -> Result<Packet, FeedEnd> {
            if self.left == 0 {
                return Err(FeedEnd::Closed);
            }
            let now = Instant::now();
            if self.next > now {
                std::thread::sleep(self.next - now);
            }
            self.next += self.period;
            self.left -= 1;
            let key = self.index % KEY_EVERY == 0 || std::mem::take(&mut self.key_wanted);
            self.index += 1;
            let mut data = vec![0u8; self.size];
            data[..5].copy_from_slice(&[0, 0, 0, 1, if key { 0x65 } else { 0x41 }]);
            self.seen.read.fetch_add(data.len() as u64, Ordering::SeqCst);
            self.seen.note_backlog();
            self.seen.read_times.lock().unwrap().push(Instant::now());
            Ok(Packet { data, time_ns: telescope_core::time::monotonic_ns() })
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            Box::new(NoStop)
        }
        fn request_keyframe(&mut self) -> bool {
            self.key_wanted = true;
            self.seen.key_asks.fetch_add(1, Ordering::SeqCst);
            true
        }
    }

    /// A decoder that takes `per_packet` a picture, with one stall of `stall` at its `stall_at`th packet.
    struct Slow {
        seen: Arc<Seen>,
        per_packet: Duration,
        stall: Duration,
        stall_at: usize,
        n: usize,
    }

    impl Decoder for Slow {
        fn decode(&mut self, packet: &Packet) -> Decoded {
            let age = telescope_core::time::monotonic_ns().saturating_sub(packet.time_ns);
            self.seen.max_age_ns.fetch_max(age, Ordering::SeqCst);
            self.seen.ages_ns.lock().unwrap().push(age);
            self.seen.decoded.fetch_add(packet.data.len() as u64, Ordering::SeqCst);
            self.seen.note_backlog();
            self.n += 1;
            std::thread::sleep(if self.n == self.stall_at { self.stall } else { self.per_packet });
            Decoded { newest: Some(Frame::new(2, 2, PixelFormat::I420, vec![0; 6])), count: 1 }
        }
        fn hardware(&self) -> bool {
            false
        }
    }

    struct Run {
        label: String,
        max_age_ms: f64,
        p99_age_ms: f64,
        max_backlog_kib: f64,
        max_read_gap_ms: f64,
        decoded: usize,
        sent: usize,
        key_asks: usize,
    }

    fn run(label: &str, size: usize, send_fps: u32, decode: Duration, stall: Duration, secs: usize) -> Run {
        let seen = Arc::new(Seen::default());
        let sent = send_fps as usize * secs;
        let sender = Sender { seen: seen.clone(), size, period: Duration::from_secs_f64(1.0 / send_fps as f64), left: sent, index: 0, next: Instant::now(), key_wanted: false };
        let decoder = Slow { seen: seen.clone(), per_packet: decode, stall, stall_at: send_fps as usize, n: 0 };
        let mut r = Reader::new(Box::new(sender), Box::new(decoder));
        let mut frames = 0;
        while r.read().is_ok() {
            frames += 1;
        }
        let mut ages = seen.ages_ns.lock().unwrap().clone();
        ages.sort_unstable();
        let times = seen.read_times.lock().unwrap();
        let gap = times.windows(2).map(|w| w[1] - w[0]).max().unwrap_or_default();
        Run {
            label: label.into(),
            max_age_ms: seen.max_age_ns.load(Ordering::SeqCst) as f64 / 1e6,
            p99_age_ms: ages.get(ages.len() * 99 / 100).copied().unwrap_or(0) as f64 / 1e6,
            max_backlog_kib: seen.max_backlog.load(Ordering::SeqCst) as f64 / 1024.0,
            max_read_gap_ms: gap.as_secs_f64() * 1e3,
            decoded: frames,
            sent,
            key_asks: seen.key_asks.load(Ordering::SeqCst),
        }
    }

    #[test]
    #[ignore = "timed: cargo test -p telescope-media --release reader::bench -- --ignored --nocapture"]
    fn queue_behaviour_under_a_stall_and_a_slow_decoder() {
        let ms = Duration::from_millis;
        let mut runs = Vec::new();
        for (name, size) in [("1080p 12 Mbps", 50_000), ("4K 32 Mbps", 133_000)] {
            runs.push(run(&format!("{name}: 500 ms stall, decode 2 ms"), size, 30, ms(2), ms(500), 5));
            runs.push(run(&format!("{name}: 2 s stall, decode 2 ms"), size, 30, ms(2), ms(2000), 6));
            runs.push(run(&format!("{name}: decode 40 ms (25 fps) vs 30 fps"), size, 30, ms(40), ms(0), 10));
        }
        for r in runs {
            println!("{:<48} age max {:>6.0} ms p99 {:>6.0} ms | backlog max {:>7.0} KiB | read gap max {:>5.0} ms | decoded {}/{} | key asks {}",
                r.label, r.max_age_ms, r.p99_age_ms, r.max_backlog_kib, r.max_read_gap_ms, r.decoded, r.sent, r.key_asks);
        }
    }
}
#[cfg(test)]
mod age_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;
    use telescope_core::frame::{Frame, PixelFormat};
    use telescope_core::media::Decoded;

    struct NoStop;

    impl FeedStopper for NoStop {
        fn stop(&self) {}
    }

    /// A packet whose sixth byte says which it is; an IDR slice when it's a keyframe.
    fn packet(id: u8, key: bool) -> Packet {
        Packet { data: vec![0, 0, 0, 1, if key { 0x65 } else { 0x41 }, id, 0, 0], time_ns: 0 }
    }

    /// Hands out its packets at once, then (while `then` has any) one every `gap`; a keyframe asked for comes next, with the end after it.
    struct Scripted {
        now: Vec<Packet>,
        then: Vec<Packet>,
        gap: Duration,
        asked: Arc<AtomicUsize>,
        key_wanted: bool,
        ended: bool,
        show_first_at: Option<u64>,
        show: Option<u64>,
    }

    impl Scripted {
        fn new(now: Vec<Packet>, then: Vec<Packet>, gap: Duration) -> (Scripted, Arc<AtomicUsize>) {
            let asked = Arc::new(AtomicUsize::new(0));
            (Scripted { now, then, gap, asked: asked.clone(), key_wanted: false, ended: false, show_first_at: None, show: None }, asked)
        }
    }

    impl VideoFeed for Scripted {
        fn read(&mut self) -> Result<Packet, FeedEnd> {
            if self.ended {
                return Err(FeedEnd::Closed);
            }
            if !self.now.is_empty() {
                self.show = self.show_first_at.take();
                return Ok(self.now.remove(0));
            }
            if std::mem::take(&mut self.key_wanted) {
                self.ended = true;
                return Ok(packet(99, true));
            }
            if self.then.is_empty() {
                return Err(FeedEnd::Closed);
            }
            std::thread::sleep(self.gap);
            Ok(self.then.remove(0))
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            Box::new(NoStop)
        }
        fn request_keyframe(&mut self) -> bool {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.key_wanted = true;
            true
        }
        fn show_at_ns(&mut self) -> Option<u64> {
            self.show.take()
        }
    }

    /// Notes which packets it was given and when, and sits on the `stall_at`th for `stall`. Packets numbered below `silent_until` give no picture.
    struct Logging {
        got: Arc<Mutex<Vec<(u8, Instant)>>>,
        stall: Duration,
        stall_at: usize,
        silent_until: u8,
    }

    impl Decoder for Logging {
        fn decode(&mut self, packet: &Packet) -> Decoded {
            let stalls = {
                let mut got = self.got.lock().unwrap();
                got.push((packet.data[5], Instant::now()));
                got.len() == self.stall_at
            };
            if stalls {
                std::thread::sleep(self.stall);
            }
            if packet.data[5] < self.silent_until {
                return Decoded { newest: None, count: 0 };
            }
            Decoded { newest: Some(Frame::new(2, 2, PixelFormat::I420, vec![0; 6])), count: 1 }
        }
        fn hardware(&self) -> bool {
            false
        }
    }

    /// Stalls on its second packet, after the first has given a picture (what's queued is only dropped once the stream is showing).
    fn logging(stall: Duration) -> (Logging, Arc<Mutex<Vec<(u8, Instant)>>>) {
        let got = Arc::new(Mutex::new(Vec::new()));
        (Logging { got: got.clone(), stall, stall_at: 2, silent_until: 0 }, got)
    }

    fn ids(got: &Mutex<Vec<(u8, Instant)>>) -> Vec<u8> {
        got.lock().unwrap().iter().map(|g| g.0).collect()
    }

    #[test]
    fn a_stale_backlog_is_dropped_up_to_the_newest_keyframe_and_still_counted() {
        let packets: Vec<Packet> = (0..7).map(|i| packet(i, i == 0 || i == 4)).collect();
        let total: u64 = packets.iter().map(|p| p.data.len() as u64).sum();
        let (feed, _) = Scripted::new(packets, vec![], Duration::ZERO);
        let (decoder, got) = logging(Duration::from_millis(400));
        let mut r = Reader::with_budget(Box::new(feed), Box::new(decoder), Duration::from_millis(150));
        let (mut bytes, mut count) = (0, 0);
        while let Ok(f) = r.read() {
            bytes += f.bytes;
            count += f.count;
        }
        // Packets 2 and 3 waited out the stall and lead to nothing a decoder could start from but the keyframe after them
        assert_eq!(ids(&got), [0, 1, 4, 5, 6]);
        assert_eq!((bytes, count), (total, 7));
    }

    #[test]
    fn a_stale_queue_with_no_keyframe_is_kept_and_the_feed_is_asked_for_one() {
        let first: Vec<Packet> = (0..3).map(|i| packet(i, i == 0)).collect();
        let later: Vec<Packet> = (3..30).map(|i| packet(i, false)).collect();
        let (feed, asked) = Scripted::new(first, later, Duration::from_millis(100));
        let (decoder, got) = logging(Duration::from_millis(500));
        let mut r = Reader::with_budget(Box::new(feed), Box::new(decoder), Duration::from_millis(150));
        while r.read().is_ok() {}
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        // Nothing was dropped without a keyframe to land on, and the one that came ended the backlog
        assert_eq!(ids(&got), [0, 1, 99]);
    }

    #[test]
    fn nothing_is_dropped_before_the_first_picture_is_out() {
        // A TCP phone sends its SPS and PPS once: a decoder that was slow to start (or got only those so far) must still be given them
        let packets: Vec<Packet> = (0..7).map(|i| packet(i, i == 0 || i == 4)).collect();
        let (feed, _) = Scripted::new(packets, vec![], Duration::ZERO);
        let (mut decoder, got) = logging(Duration::from_millis(400));
        (decoder.stall_at, decoder.silent_until) = (1, 4);
        let mut r = Reader::with_budget(Box::new(feed), Box::new(decoder), Duration::from_millis(150));
        while r.read().is_ok() {}
        assert_eq!(ids(&got), [0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn a_feed_of_byte_stream_slices_is_never_cut_up_to_a_keyframe() {
        struct Slices(Scripted);
        impl VideoFeed for Slices {
            fn read(&mut self) -> Result<Packet, FeedEnd> {
                self.0.read()
            }
            fn stopper(&self) -> Box<dyn FeedStopper> {
                Box::new(NoStop)
            }
            fn packetized(&self) -> bool {
                false
            }
        }
        let packets: Vec<Packet> = (0..7).map(|i| packet(i, i == 0 || i == 4)).collect();
        let (feed, _) = Scripted::new(packets, vec![], Duration::ZERO);
        let (decoder, got) = logging(Duration::from_millis(400));
        let mut r = Reader::with_budget(Box::new(Slices(feed)), Box::new(decoder), Duration::from_millis(150));
        while r.read().is_ok() {}
        assert_eq!(ids(&got), [0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn a_sequence_parameter_set_counts_as_a_place_to_start() {
        let (feed, _) = Scripted::new(vec![], vec![], Duration::ZERO);
        assert!(feed.is_keyframe(&[0, 0, 0, 1, 0x67, 1, 2]) && feed.is_keyframe(&[9, 0, 0, 1, 0x65, 1]));
        assert!(!feed.is_keyframe(&[0, 0, 0, 1, 0x41, 1, 2]));
    }

    #[test]
    fn a_keyframe_in_front_has_nothing_to_drop_up_to() {
        let packets: Vec<Packet> = (0..5).map(|i| packet(i, i == 2)).collect();
        let (feed, _) = Scripted::new(packets, vec![], Duration::ZERO);
        let (decoder, got) = logging(Duration::from_millis(400));
        let mut r = Reader::with_budget(Box::new(feed), Box::new(decoder), Duration::from_millis(150));
        while r.read().is_ok() {}
        // Packet 1 went in front of the stall; the keyframe is the front of what's left, with delta frames only behind it
        assert_eq!(ids(&got), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_picture_is_decoded_at_once_and_shown_when_the_feed_wants_it() {
        let (mut feed, _) = Scripted::new(vec![packet(0, true)], vec![], Duration::ZERO);
        let began = Instant::now();
        feed.show_first_at = Some(telescope_core::time::monotonic_ns() + 240_000_000);
        let (decoder, got) = logging(Duration::ZERO);
        let mut r = Reader::new(Box::new(feed), Box::new(decoder));
        assert!(r.read().is_ok());
        let shown = began.elapsed();
        let decoded_at = got.lock().unwrap()[0].1.duration_since(began);
        // Judged against each other, so a busy machine moves both: the wait comes after the decoding, not before it
        assert!(shown >= Duration::from_millis(230), "{shown:?}");
        assert!(decoded_at + Duration::from_millis(100) <= shown, "{decoded_at:?} then {shown:?}");
    }
}

/// What the feed-reader thread costs while the decoder is behind and the queue is full.
#[cfg(all(test, target_os = "linux"))]
mod wakeups {
    use super::*;
    use telescope_core::media::Decoded;

    struct Endless;

    struct NoStop;

    impl FeedStopper for NoStop {
        fn stop(&self) {}
    }

    impl VideoFeed for Endless {
        fn read(&mut self) -> Result<Packet, FeedEnd> {
            Ok(Packet { data: vec![0; 1 << 20], time_ns: 0 })
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            Box::new(NoStop)
        }
    }

    struct Slow;

    impl Decoder for Slow {
        fn decode(&mut self, _: &Packet) -> Decoded {
            std::thread::sleep(Duration::from_millis(100));
            Decoded { newest: None, count: 0 }
        }
        fn hardware(&self) -> bool {
            false
        }
    }

    fn feed_thread_switches() -> u64 {
        for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
            let dir = task.path();
            if std::fs::read_to_string(dir.join("comm")).is_ok_and(|c| c.trim() == "feed-reader") {
                let status = std::fs::read_to_string(dir.join("status")).unwrap();
                return status.lines().filter(|l| l.contains("ctxt_switches")).filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok()).sum();
            }
        }
        0
    }

    /// cargo test -p telescope-media --release --lib -- --ignored --nocapture full_queue_wakeups
    #[test]
    #[ignore = "a measurement"]
    fn full_queue_wakeups() {
        let mut reader = Reader::new(Box::new(Endless), Box::new(Slow));
        reader.max_bytes = u64::MAX;
        std::thread::spawn(move || {
            let _ = reader.read();
        });
        std::thread::sleep(Duration::from_millis(500));
        let before = feed_thread_switches();
        std::thread::sleep(Duration::from_secs(3));
        println!("feed thread, queue full, decoder at 10 packets/s: {:.0} context switches/s", (feed_thread_switches() - before) as f64 / 3.0);
    }
}

