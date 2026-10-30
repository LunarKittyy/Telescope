//! One VP8 frame per packet, as the Browser camera's WebRTC hands them over from a browser that has no H.264.

use crate::ffmpeg::Codec;
use telescope_core::media::{Decoded, Decoder, Packet};

pub struct Vp8 {
    codec: Codec,
}

impl Vp8 {
    pub fn new() -> Result<Vp8, String> {
        Ok(Vp8 { codec: Codec::vp8()? })
    }
}

impl Decoder for Vp8 {
    fn decode(&mut self, packet: &Packet) -> Decoded {
        let mut newest = None;
        let count = self.codec.decode(&packet.data, |f| newest = Some(f)).unwrap_or(0);
        if let Some(f) = newest.as_mut() {
            f.time_ns = packet.time_ns;
        }
        Decoded { count, newest }
    }

    fn hardware(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_ffmpeg_decodes_vp8_and_skips_what_isnt() {
        let mut vp8 = Vp8::new().unwrap();
        let d = vp8.decode(&Packet { data: vec![1, 2, 3], time_ns: 0 });
        assert!(d.newest.is_none() && d.count == 0);
    }
}
