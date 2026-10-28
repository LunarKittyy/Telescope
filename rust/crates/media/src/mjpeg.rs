//! One JPEG per packet (Heavy streams and the Browser camera) to I420, checked against jpeg_guard's limits before
//! anything is allocated. Each JPEG stands alone, so a stream can run several of these side by side.

use crate::ffmpeg::Codec;
use crate::jpeg_guard::within_limits;
use telescope_core::frame::Frame;
use telescope_core::media::{Decoded, Decoder, Packet};

pub struct Mjpeg {
    codec: Codec,
}

impl Mjpeg {
    pub fn new() -> Result<Mjpeg, String> {
        Ok(Mjpeg { codec: Codec::mjpeg()? })
    }

    /// The frame in jpeg, or None if it doesn't decode or declares more pixels than we'll allocate (decode_jpeg).
    pub fn decode_jpeg(&mut self, jpeg: &[u8]) -> Option<Frame> {
        if !within_limits(jpeg) {
            return None;
        }
        let mut out = None;
        self.codec.decode(jpeg, |f| out = Some(f)).ok()?;
        out
    }
}

impl Decoder for Mjpeg {
    fn decode(&mut self, packet: &Packet) -> Decoded {
        let mut newest = self.decode_jpeg(&packet.data);
        if let Some(f) = newest.as_mut() {
            f.time_ns = packet.time_ns;
        }
        Decoded { count: newest.is_some() as u32, newest }
    }

    fn hardware(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenc;

    fn patch_size(jpg: &[u8], w: u16, h: u16) -> Vec<u8> {
        let mut out = jpg.to_vec();
        let at = out.windows(2).position(|p| p == [0xFF, 0xC0]).unwrap() + 5;
        out[at..at + 2].copy_from_slice(&h.to_be_bytes());
        out[at + 2..at + 4].copy_from_slice(&w.to_be_bytes());
        out
    }

    #[test]
    fn a_normal_frame_decodes_and_a_patched_huge_one_is_dropped_before_allocating() {
        let mut dec = Mjpeg::new().unwrap();
        let good = testenc::jpeg(120, 64, 48);
        let f = dec.decode_jpeg(&good).unwrap();
        assert_eq!((f.width, f.height), (64, 48));
        assert!((f.data[0] as i32 - 120).abs() <= 3, "full-range JPEG grey keeps its value: {}", f.data[0]);
        assert!(dec.decode_jpeg(&patch_size(&good, 32767, 32767)).is_none());
    }

    #[test]
    fn the_reader_drops_an_oversized_frame_and_decodes_a_normal_one() {
        let mut dec = Mjpeg::new().unwrap();
        let (good, huge) = (testenc::jpeg(20, 32, 24), patch_size(&testenc::jpeg(20, 32, 24), 32767, 32767));
        let d = dec.decode(&Packet { data: good, time_ns: 5 });
        assert_eq!((d.count, d.newest.as_ref().map(|f| (f.width, f.height, f.time_ns))), (1, Some((32, 24, 5))));
        let d = dec.decode(&Packet { data: huge, time_ns: 6 });
        assert!(d.newest.is_none() && d.count == 0);
    }

    #[test]
    fn garbage_gives_nothing_without_failing() {
        let mut dec = Mjpeg::new().unwrap();
        for data in [&b""[..], b"\xff\xd8", b"not a jpeg at all"] {
            assert!(dec.decode_jpeg(data).is_none());
        }
        // A whole header with a broken scan after it
        let good = testenc::jpeg(20, 32, 24);
        let sos = good.windows(2).position(|p| p == [0xFF, 0xDA]).unwrap();
        let _ = dec.decode_jpeg(&good[..sos + 20]);
        assert!(dec.decode_jpeg(&good).is_some());
    }
}
