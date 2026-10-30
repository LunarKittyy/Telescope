//! Reading frames off a feed (H264Reader.read): the newest frame decoded from the next data that has any, with the
//! bytes and frames behind it for the stream's throughput and "keeping up" readouts.

use telescope_core::media::{Decoder, FeedEnd, FeedStopper, FrameReader, ReadFrame, VideoFeed};

/// Data with no finished frame after this much isn't a stream the phone sent; stop instead of buffering on.
pub const MAX_BYTES_WITHOUT_FRAME: u64 = 32 * 1024 * 1024;

pub struct Reader {
    feed: Box<dyn VideoFeed>,
    decoder: Box<dyn Decoder>,
    max_bytes: u64,
    pending_bytes: u64,
    pending_frames: u32,
}

impl Reader {
    pub fn new(feed: Box<dyn VideoFeed>, decoder: Box<dyn Decoder>) -> Reader {
        Reader { feed, decoder, max_bytes: MAX_BYTES_WITHOUT_FRAME, pending_bytes: 0, pending_frames: 0 }
    }
}

impl FrameReader for Reader {
    fn read(&mut self) -> Result<ReadFrame, FeedEnd> {
        loop {
            let packet = self.feed.read()?;
            self.pending_bytes += packet.data.len() as u64;
            if self.pending_bytes > self.max_bytes {
                return Err(FeedEnd::Error("data that never makes a frame".into()));
            }
            let decoded = self.decoder.decode(&packet);
            self.pending_frames += decoded.count;
            if let Some(frame) = decoded.newest {
                self.feed.decoded();
                let bytes = std::mem::take(&mut self.pending_bytes);
                // Frames that arrived for it, the ones skipped to keep up included
                let count = std::mem::take(&mut self.pending_frames);
                return Ok(ReadFrame { frame, bytes, count });
            }
        }
    }

    fn stopper(&self) -> Box<dyn FeedStopper> {
        self.feed.stopper()
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
