//! Reading frames off a feed (H264Reader.read): the newest frame decoded from the next data that has any, with the
//! bytes and frames behind it for the stream's throughput and "keeping up" readouts.
//!
//! The feed is read on a thread of its own, so a decoder that stalls for a moment (a card waiting behind a game's graphics work) doesn't
//! stop the reading. The phone would see its connection stop taking data, take that for a bad link and cut the bitrate to the minimum.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;
use telescope_core::media::{Decoder, FeedEnd, FeedStopper, FrameReader, Packet, ReadFrame, VideoFeed};

/// Data with no finished frame after this much isn't a stream the phone sent; stop instead of buffering on.
pub const MAX_BYTES_WITHOUT_FRAME: u64 = 32 * 1024 * 1024;

/// How much read data waits for the decoder before the reading stops too and the connection pushes back: about 2 s of a fast stream, enough to ride out a stall without a slow decoder piling up seconds of delay.
const MAX_QUEUED_BYTES: u64 = 4 * 1024 * 1024;

pub struct Reader {
    packets: Receiver<Result<Packet, FeedEnd>>,
    /// Bytes read and not decoded yet.
    queued: Arc<AtomicU64>,
    /// Pictures decoded that the feed hasn't been told of yet (it's read on the other thread).
    decoded: Arc<AtomicU32>,
    /// The feed's own flag for a picture decoded, which reaches it while it's blocked reading.
    decoded_flag: Option<Arc<AtomicBool>>,
    stop: Arc<dyn FeedStopper>,
    /// Set when the Reader is dropped, which the feed-reader thread can't see while the queue is full and it isn't sending.
    gone: Arc<AtomicBool>,
    decoder: Box<dyn Decoder>,
    max_bytes: u64,
    pending_bytes: u64,
    pending_frames: u32,
}

/// Ends the feed's blocked read, for however many copies of it are asked for.
struct Stop(Arc<dyn FeedStopper>);

impl FeedStopper for Stop {
    fn stop(&self) {
        self.0.stop();
    }
}

impl Reader {
    pub fn new(mut feed: Box<dyn VideoFeed>, decoder: Box<dyn Decoder>) -> Reader {
        let stop: Arc<dyn FeedStopper> = Arc::from(feed.stopper());
        let decoded_flag = feed.decoded_flag();
        let (send, packets) = channel();
        let (queued, decoded) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU32::new(0)));
        let (q, d) = (queued.clone(), decoded.clone());
        let gone = Arc::new(AtomicBool::new(false));
        let g = gone.clone();
        let spawned = std::thread::Builder::new().name("feed-reader".into()).spawn(move || {
            let _priority = telescope_core::system::prioritise_thread("frame receive");
            loop {
                for _ in 0..d.swap(0, Ordering::Relaxed) {
                    feed.decoded();
                }
                if g.load(Ordering::Relaxed) {
                    return;
                }
                if q.load(Ordering::Relaxed) > MAX_QUEUED_BYTES {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                let read = feed.read();
                let end = read.is_err();
                if let Ok(p) = &read {
                    q.fetch_add(p.data.len() as u64, Ordering::Relaxed);
                }
                // The reader is gone
                if send.send(read).is_err() || end {
                    return;
                }
            }
        });
        if let Err(e) = spawned {
            log::error!("Couldn't start the thread that reads the stream: {e}");
        }
        Reader { packets, queued, decoded, decoded_flag, stop, gone, decoder, max_bytes: MAX_BYTES_WITHOUT_FRAME, pending_bytes: 0, pending_frames: 0 }
    }

    /// The next packet read, waiting for one; the end of the feed once the thread has gone.
    fn next(&mut self) -> Result<Packet, FeedEnd> {
        let read = self.packets.recv().unwrap_or(Err(FeedEnd::Closed));
        if let Ok(p) = &read {
            self.queued.fetch_sub(p.data.len() as u64, Ordering::Relaxed);
        }
        read
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
            let packet = self.next()?;
            self.pending_bytes += packet.data.len() as u64;
            if self.pending_bytes > self.max_bytes {
                return Err(FeedEnd::Error("data that never makes a frame".into()));
            }
            let decoded = self.decoder.decode(&packet);
            self.pending_frames += decoded.count;
            if let Some(frame) = decoded.newest {
                match &self.decoded_flag {
                    Some(flag) => flag.store(true, Ordering::SeqCst),
                    None => {
                        self.decoded.fetch_add(1, Ordering::Relaxed);
                    }
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
