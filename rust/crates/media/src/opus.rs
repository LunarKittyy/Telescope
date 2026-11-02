//! Opus as WebRTC carries it (the Browser camera's mic) to 48 kHz mono s16le, the PCM the phone's /v1/audio sends.

use ffmpeg_sys_next as ff;
use std::ptr;
use telescope_core::media::AudioDecoder;

pub struct Opus {
    ctx: *mut ff::AVCodecContext,
    packet: *mut ff::AVPacket,
    frame: *mut ff::AVFrame,
}

// SAFETY: used from one thread at a time; FFmpeg contexts aren't tied to the thread that made them
unsafe impl Send for Opus {}

impl Opus {
    pub fn new() -> Result<Opus, String> {
        // SAFETY: plain libavcodec setup; every pointer is checked before use and freed in Drop
        unsafe {
            let codec = ff::avcodec_find_decoder(ff::AVCodecID::AV_CODEC_ID_OPUS);
            if codec.is_null() {
                return Err("this FFmpeg has no Opus decoder".into());
            }
            let o = Opus { ctx: ff::avcodec_alloc_context3(codec), packet: ff::av_packet_alloc(), frame: ff::av_frame_alloc() };
            if o.ctx.is_null() || o.packet.is_null() || o.frame.is_null() {
                return Err("out of memory opening a decoder".into());
            }
            // WebRTC always says opus/48000/2; a mono packet comes out on both channels
            (*o.ctx).sample_rate = 48000;
            ff::av_channel_layout_default(&mut (*o.ctx).ch_layout, 2);
            if ff::avcodec_open2(o.ctx, codec, ptr::null_mut()) < 0 {
                return Err("couldn't open the Opus decoder".into());
            }
            Ok(o)
        }
    }

    /// The channels of the frame just decoded, averaged into one, as s16le.
    unsafe fn mono(&self, out: &mut Vec<u8>) {
        let f = &*self.frame;
        let channels = f.ch_layout.nb_channels.max(1) as usize;
        let n = f.nb_samples.max(0) as usize;
        let planar = ff::av_sample_fmt_is_planar(std::mem::transmute::<i32, ff::AVSampleFormat>(f.format)) != 0;
        let sample = |c: usize, i: usize| -> f32 {
            let fmt = f.format;
            if fmt == ff::AVSampleFormat::AV_SAMPLE_FMT_FLTP as i32 || fmt == ff::AVSampleFormat::AV_SAMPLE_FMT_FLT as i32 {
                let base = if planar { *f.extended_data.add(c) as *const f32 } else { *f.extended_data as *const f32 };
                *base.add(if planar { i } else { i * channels + c })
            } else if fmt == ff::AVSampleFormat::AV_SAMPLE_FMT_S16P as i32 || fmt == ff::AVSampleFormat::AV_SAMPLE_FMT_S16 as i32 {
                let base = if planar { *f.extended_data.add(c) as *const i16 } else { *f.extended_data as *const i16 };
                f32::from(*base.add(if planar { i } else { i * channels + c })) / 32768.0
            } else {
                0.0
            }
        };
        out.reserve(n * 2);
        for i in 0..n {
            let v = (0..channels).map(|c| sample(c, i)).sum::<f32>() / channels as f32;
            out.extend_from_slice(&((v * 32767.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes());
        }
    }
}

impl AudioDecoder for Opus {
    fn decode(&mut self, packet: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut data = packet.to_vec();
        data.resize(packet.len() + ff::AV_INPUT_BUFFER_PADDING_SIZE as usize, 0);
        // SAFETY: the packet points at a padded copy for the length of send_packet, which copies it
        unsafe {
            (*self.packet).data = data.as_mut_ptr();
            (*self.packet).size = packet.len() as i32;
            let r = ff::avcodec_send_packet(self.ctx, self.packet);
            (*self.packet).data = ptr::null_mut();
            (*self.packet).size = 0;
            if r < 0 {
                return out;
            }
            while ff::avcodec_receive_frame(self.ctx, self.frame) >= 0 {
                self.mono(&mut out);
                ff::av_frame_unref(self.frame);
            }
        }
        out
    }
}

impl Drop for Opus {
    fn drop(&mut self) {
        // SAFETY: each was allocated in new() and is freed once
        unsafe {
            ff::av_frame_free(&mut self.frame);
            ff::av_packet_free(&mut self.packet);
            ff::avcodec_free_context(&mut self.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packet_comes_out_as_20_ms_of_mono_pcm() {
        let mut opus = Opus::new().unwrap();
        // The silence browsers send while the mic is quiet: one 20 ms CELT frame
        let pcm = opus.decode(&[0xF8, 0xFF, 0xFE]);
        assert_eq!(pcm.len(), 960 * 2);
        assert!(opus.decode(b"").is_empty());
    }
}
