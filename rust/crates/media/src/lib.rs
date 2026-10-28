//! Decoding the phone's video and running each stream's frame steps, behind core's Media trait. FFmpeg's libavcodec
//! decodes (VA-API on Linux, D3D11VA on Windows).

#[cfg(ffmpeg)]
pub mod ffmpeg;
#[cfg(ffmpeg)]
pub mod h264;
pub mod jpeg_guard;
#[cfg(ffmpeg)]
pub mod mjpeg;
pub mod packets;
pub mod reader;
#[cfg(all(test, ffmpeg))]
mod testenc;

use std::sync::Arc;
use telescope_core::frame::Frame;
use telescope_core::media::*;

/// The decoders this build has.
#[derive(Default)]
pub struct FfmpegMedia;

/// The preview, scaled and converted on the processor.
#[cfg(ffmpeg)]
struct PreviewScaler(ffmpeg::CpuScaler);

#[cfg(ffmpeg)]
impl Scaler for PreviewScaler {
    fn rgb(&mut self, frame: &Frame, width: u32, height: u32) -> Vec<u8> {
        self.0.rgb(frame, width, height)
    }
}

/// Black RGB, for builds without FFmpeg.
#[cfg(not(ffmpeg))]
struct PreviewScaler;

#[cfg(not(ffmpeg))]
impl Scaler for PreviewScaler {
    fn rgb(&mut self, _frame: &Frame, width: u32, height: u32) -> Vec<u8> {
        vec![0; (width * height * 3) as usize]
    }
}

impl Media for FfmpegMedia {
    fn decoder(&self, codec: Codec, options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
        self.decoder_with_tap(codec, options, new_tap())
    }

    fn decoder_with_tap(&self, codec: Codec, options: &DecodeOptions, tap: Arc<dyn PacketTap>) -> Result<Box<dyn Decoder>, String> {
        #[cfg(ffmpeg)]
        {
            // Only our own taps hand out steps; someone else's PacketTap can't be fed from here
            let tap = arc_tap(&tap);
            match codec {
                Codec::H264 => return Ok(Box::new(h264::H264::new(options.hardware, tap)?)),
                Codec::Mjpeg => return Ok(Box::new(mjpeg::Mjpeg::new()?)),
                _ => {}
            }
        }
        let _ = (options, tap);
        Err(format!("{codec:?} decoding isn't in this build"))
    }

    fn scaler(&self) -> Box<dyn Scaler> {
        #[cfg(ffmpeg)]
        return Box::new(PreviewScaler(ffmpeg::CpuScaler::default()));
        #[cfg(not(ffmpeg))]
        Box::new(PreviewScaler)
    }

    fn frame_reader(&self, feed: Box<dyn VideoFeed>, decoder: Box<dyn Decoder>) -> Result<Box<dyn FrameReader>, String> {
        Ok(Box::new(reader::Reader::new(feed, decoder)))
    }

    fn packet_tap(&self) -> Option<Arc<dyn PacketTap>> {
        Some(new_tap())
    }

    fn decoding_status(&self) -> DecodingStatus {
        #[cfg(ffmpeg)]
        return h264::status();
        #[cfg(not(ffmpeg))]
        DecodingStatus::default()
    }
}

/// The taps packet_tap() handed out, so a decoder can find its own type behind the trait object.
static TAPS: std::sync::Mutex<Vec<std::sync::Weak<packets::Tap>>> = std::sync::Mutex::new(Vec::new());

fn new_tap() -> Arc<packets::Tap> {
    let tap = packets::Tap::new();
    let mut taps = TAPS.lock().unwrap();
    taps.retain(|w| w.strong_count() > 0);
    taps.push(Arc::downgrade(&tap));
    tap
}

#[cfg(ffmpeg)]
fn arc_tap(tap: &Arc<dyn PacketTap>) -> Option<Arc<packets::Tap>> {
    let at = Arc::as_ptr(tap) as *const () as usize;
    TAPS.lock().unwrap().iter().filter_map(std::sync::Weak::upgrade).find(|t| Arc::as_ptr(t) as *const () as usize == at)
}
