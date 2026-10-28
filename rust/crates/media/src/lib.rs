//! Decoding the phone's video and running each stream's frame steps, behind core's Media trait. FFmpeg's libavcodec
//! decodes (VA-API on Linux, D3D11VA on Windows); wgpu runs shader steps and draws the preview on the graphics card.

#[cfg(ffmpeg)]
pub mod ffmpeg;
pub mod frames;
pub mod gpu;
#[cfg(ffmpeg)]
pub mod h264;
pub mod jpeg_guard;
#[cfg(ffmpeg)]
pub mod mjpeg;
pub mod packets;
pub mod pipeline;
pub mod reader;
#[cfg(all(test, ffmpeg))]
mod testenc;

use std::sync::{Arc, OnceLock};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::media::*;

/// The decoders and frame steps this build has.
#[derive(Default)]
pub struct FfmpegMedia;

/// The preview on the graphics card, scaled and converted there; on the processor when there's no card.
struct PreviewScaler {
    gpu: Option<Arc<gpu::Gpu>>,
    #[cfg(ffmpeg)]
    cpu: ffmpeg::CpuScaler,
}

impl Scaler for PreviewScaler {
    fn rgb(&mut self, frame: &Frame, width: u32, height: u32) -> Vec<u8> {
        if let Some(gpu) = &self.gpu {
            match gpu.rgba(frame, width, height) {
                Ok(rgba) => return rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect(),
                Err(why) => {
                    log::warn!("The preview can't be drawn on the graphics card ({why}); drawing it on the processor");
                    self.gpu = None;
                }
            }
        }
        #[cfg(ffmpeg)]
        if frame.format == PixelFormat::I420 {
            return self.cpu.rgb(frame, width, height);
        }
        vec![0; (width * height * 3) as usize]
    }
}

/// The one frame store for every stream.
fn frames() -> Arc<frames::Frames> {
    static FRAMES: OnceLock<Arc<frames::Frames>> = OnceLock::new();
    FRAMES.get_or_init(frames::Frames::new).clone()
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
        Box::new(PreviewScaler {
            gpu: gpu::Gpu::shared(),
            #[cfg(ffmpeg)]
            cpu: ffmpeg::CpuScaler::default(),
        })
    }

    fn frame_reader(&self, feed: Box<dyn VideoFeed>, decoder: Box<dyn Decoder>) -> Result<Box<dyn FrameReader>, String> {
        Ok(Box::new(reader::Reader::new(feed, decoder)))
    }

    fn packet_tap(&self) -> Option<Arc<dyn PacketTap>> {
        Some(new_tap())
    }

    fn pipeline(&self) -> Result<Box<dyn FramePipeline>, String> {
        Ok(Box::new(pipeline::Pipeline::new()))
    }

    fn stream_frames(&self) -> Option<Arc<dyn StreamFrames>> {
        Some(frames())
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
