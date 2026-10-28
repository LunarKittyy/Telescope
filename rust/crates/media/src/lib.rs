//! Decoding the phone's video into frames, behind core's Media trait. FFmpeg's libavcodec on Linux; Windows isn't written yet.

#[cfg(target_os = "linux")]
pub mod decode;

use telescope_core::frame::Frame;
use telescope_core::media::{Codec, DecodeOptions, Decoder, Media, Scaler};
#[cfg(target_os = "linux")]
use telescope_core::media::{Decoded, Packet};

/// The decoders this build has.
#[derive(Default)]
pub struct FfmpegMedia;

#[cfg(target_os = "linux")]
struct H264(decode::Decoder);

#[cfg(target_os = "linux")]
impl Decoder for H264 {
    fn decode(&mut self, packet: &Packet) -> Decoded {
        let (mut newest, count) = self.0.feed(&packet.data);
        if let Some(f) = newest.as_mut() {
            f.time_ns = packet.time_ns;
        }
        Decoded { newest, count }
    }

    fn hardware(&self) -> bool {
        self.0.hardware
    }
}

#[cfg(target_os = "linux")]
struct Swscale(decode::PreviewScaler);

#[cfg(target_os = "linux")]
impl Scaler for Swscale {
    fn rgb(&mut self, frame: &Frame, width: u32, height: u32) -> Vec<u8> {
        self.0.rgb(frame, width, height)
    }
}

/// Black RGB, for builds without a scaler yet.
#[cfg(not(target_os = "linux"))]
struct Blank;

#[cfg(not(target_os = "linux"))]
impl Scaler for Blank {
    fn rgb(&mut self, _frame: &Frame, width: u32, height: u32) -> Vec<u8> {
        vec![0; (width * height * 3) as usize]
    }
}

impl Media for FfmpegMedia {
    fn decoder(&self, codec: Codec, options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
        match codec {
            #[cfg(target_os = "linux")]
            Codec::H264 => Ok(Box::new(H264(decode::Decoder::new(options.hardware)?))),
            other => {
                let _ = options;
                Err(format!("{other:?} decoding isn't in this build yet"))
            }
        }
    }

    fn scaler(&self) -> Box<dyn Scaler> {
        #[cfg(target_os = "linux")]
        return Box::new(Swscale(decode::PreviewScaler::default()));
        #[cfg(not(target_os = "linux"))]
        Box::new(Blank)
    }
}
