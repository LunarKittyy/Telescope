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
#[cfg(ffmpeg)]
pub mod opus;
#[cfg(ffmpeg)]
pub mod vp8;
pub mod packets;
pub mod picture;
pub mod pipeline;
pub mod plain_jpeg;
mod plain_scale;
mod pool;
mod range;
pub mod reader;
#[cfg(ffmpeg)]
mod slow;
pub mod transfers;
#[cfg(ffmpeg)]
mod yuv;
#[cfg(test)]
mod guard_alloc;
#[cfg(ffmpeg)]
#[allow(dead_code)] // the tests use the rest of what it makes
mod testenc;

#[cfg(test)]
#[global_allocator]
static ALLOC: guard_alloc::Guard = guard_alloc::Guard;

use std::sync::{Arc, OnceLock};
use telescope_core::frame::Frame;
#[cfg(ffmpeg)]
use telescope_core::frame::PixelFormat;
use telescope_core::media::*;

/// H.264 made with this build's encoder for tests in other crates (None where it has none): a grey square moving over a picture
/// that changes every 6 frames, as raw packets with no delimiters.
#[doc(hidden)]
pub fn test_h264_moving(count: usize, w: u32, h: u32, gop: u32, profile: &str) -> Option<Vec<Vec<u8>>> {
    #[cfg(ffmpeg)]
    return testenc::h264_moving(count, w, h, gop, profile);
    #[cfg(not(ffmpeg))]
    {
        let _ = (count, w, h, gop, profile);
        None
    }
}

/// H.264 of flat greys for tests in other crates, one packet a frame with a delimiter after each, as the phone sends them. noisy: with
/// noise on the greys, so frames span several datagrams.
#[doc(hidden)]
pub fn test_h264(values: &[u8], w: u32, h: u32, gop: u32, noisy: bool) -> Option<Vec<Vec<u8>>> {
    #[cfg(ffmpeg)]
    return if noisy { testenc::h264_noisy(values, w, h, gop) } else { testenc::h264(values, w, h, gop) };
    #[cfg(not(ffmpeg))]
    {
        let _ = (values, w, h, gop, noisy);
        None
    }
}

/// Opus packets of a 20 ms mono tone, made with this build's encoder for tests in other crates (None where it has none).
#[doc(hidden)]
pub fn test_opus_tone(packets: usize, peak: f64) -> Option<Vec<Vec<u8>>> {
    #[cfg(ffmpeg)]
    return testenc::opus_tone(packets, peak);
    #[cfg(not(ffmpeg))]
    {
        let _ = (packets, peak);
        None
    }
}

/// Whether FFmpeg can be called here (h264_reader.available()). Windows loads its DLLs on first use, so a copy that lost one still
/// starts: H.264, VP8 and Opus are then missing, MJPEG goes through plain_jpeg and pictures through picture. Without this, nothing
/// may touch FFmpeg, its destructors included.
pub fn ffmpeg_available() -> bool {
    #[cfg(all(ffmpeg, windows))]
    {
        static LOADS: OnceLock<bool> = OnceLock::new();
        *LOADS.get_or_init(|| {
            use ffmpeg_sys_next as ff;
            // These pull in whatever else they need, so a missing swresample fails the avcodec line too
            let libraries = [format!("avutil-{}.dll", ff::LIBAVUTIL_VERSION_MAJOR), format!("swscale-{}.dll", ff::LIBSWSCALE_VERSION_MAJOR),
                             format!("avcodec-{}.dll", ff::LIBAVCODEC_VERSION_MAJOR)];
            // SAFETY: FFmpeg's libraries run no code of ours on load, and stay loaded for the calls to come
            libraries.iter().all(|name| match unsafe { libloading::Library::new(name) } {
                Ok(library) => {
                    std::mem::forget(library);
                    true
                }
                Err(why) => {
                    log::warn!("FFmpeg isn't available, so only MJPEG can be decoded: {why}");
                    false
                }
            })
        })
    }
    #[cfg(all(ffmpeg, not(windows)))]
    return true;
    #[cfg(not(ffmpeg))]
    false
}

/// A decoder for codec, from FFmpeg when `ffmpeg` says it can be called.
fn make_decoder(codec: Codec, options: &DecodeOptions, tap: Arc<dyn PacketTap>, ffmpeg: bool) -> Result<Box<dyn Decoder>, String> {
    #[cfg(ffmpeg)]
    if ffmpeg {
        // Only our own taps hand out steps; someone else's PacketTap can't be fed from here
        let tap = arc_tap(&tap);
        match codec {
            Codec::H264 => return Ok(Box::new(h264::H264::new(options.hardware, tap)?)),
            Codec::Mjpeg => return Ok(Box::new(mjpeg::Mjpeg::new()?)),
            Codec::Vp8 => return Ok(Box::new(vp8::Vp8::new()?)),
            _ => {}
        }
    }
    if !ffmpeg && matches!(codec, Codec::Mjpeg) {
        return Ok(Box::new(plain_jpeg::PlainJpeg));
    }
    let _ = (options, tap);
    Err(format!("{codec:?} decoding isn't in this build"))
}

/// A picture file as RGBA, through FFmpeg when `ffmpeg` says it can be called.
fn read_picture(bytes: &[u8], ffmpeg: bool) -> Result<Frame, String> {
    #[cfg(ffmpeg)]
    if ffmpeg {
        return ffmpeg::decode_picture(bytes);
    }
    let _ = ffmpeg;
    picture::decode(bytes)
}

/// The decoders and frame steps this build has.
#[derive(Default)]
pub struct FfmpegMedia;

/// The preview on the graphics card, scaled and converted there; on the processor when there's no card.
struct PreviewScaler {
    // Opened on the first preview frame: a stream with no window never loads the graphics driver
    gpu: Option<Option<Arc<gpu::Gpu>>>,
    // Only made where FFmpeg can be called, since dropping one calls it
    #[cfg(ffmpeg)]
    cpu: Option<ffmpeg::CpuScaler>,
}

impl PreviewScaler {
    fn new(ffmpeg: bool) -> PreviewScaler {
        #[cfg(not(ffmpeg))]
        let _ = ffmpeg;
        PreviewScaler {
            gpu: None,
            #[cfg(ffmpeg)]
            cpu: ffmpeg.then(ffmpeg::CpuScaler::default),
        }
    }
}

impl Scaler for PreviewScaler {
    fn rgb(&mut self, frame: &Frame, width: u32, height: u32) -> Vec<u8> {
        if let Some(gpu) = self.gpu.get_or_insert_with(gpu::Gpu::shared) {
            match gpu.rgba(frame, width, height) {
                Ok(rgba) => return rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect(),
                Err(why) => {
                    log::warn!("The preview can't be drawn on the graphics card ({why}); drawing it on the processor");
                    self.gpu = Some(None);
                }
            }
        }
        #[cfg(ffmpeg)]
        if let (Some(cpu), PixelFormat::I420) = (&mut self.cpu, frame.format) {
            return cpu.rgb(frame, width, height);
        }
        plain_scale::rgb(frame, width, height)
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
        make_decoder(codec, options, tap, ffmpeg_available())
    }

    fn opus_decoder(&self) -> Result<Box<dyn AudioDecoder>, String> {
        #[cfg(ffmpeg)]
        if ffmpeg_available() {
            return Ok(Box::new(opus::Opus::new()?));
        }
        Err("Opus decoding isn't in this build".into())
    }

    fn scaler(&self) -> Box<dyn Scaler> {
        Box::new(PreviewScaler::new(ffmpeg_available()))
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
        if ffmpeg_available() {
            return h264::status();
        }
        DecodingStatus::default()
    }

    fn decode_picture(&self, bytes: &[u8]) -> Result<Frame, String> {
        read_picture(bytes, ffmpeg_available())
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

#[cfg(test)]
mod tests {
    use super::*;

    // test_without_pyav_the_reader_never_opens
    #[test]
    fn without_ffmpeg_h264_never_opens_and_mjpeg_still_does() {
        let options = DecodeOptions::default();
        assert!(make_decoder(Codec::H264, &options, new_tap(), false).is_err());
        assert!(make_decoder(Codec::Vp8, &options, new_tap(), false).is_err());
        assert!(!make_decoder(Codec::Mjpeg, &options, new_tap(), false).unwrap().hardware());
    }

    /// Flat salmon pink: Y 180, U 100, V 160.
    fn pink(w: u32, h: u32) -> Frame {
        let c = (w.div_ceil(2) * h.div_ceil(2)) as usize;
        Frame::new(w, h, telescope_core::frame::PixelFormat::I420, [vec![180; (w * h) as usize], vec![100; c], vec![160; c]].concat())
    }

    fn is_pink(rgb: &[u8]) -> bool {
        rgb.as_chunks::<3>().0.iter().all(|p| p.iter().zip([242, 175, 134]).all(|(&a, b)| (a as i32 - b).abs() <= 3))
    }

    #[test]
    fn with_no_card_and_no_ffmpeg_the_preview_still_shows_the_picture() {
        let mut scaler = PreviewScaler { gpu: Some(None), ..PreviewScaler::new(false) };
        let rgb = scaler.rgb(&pink(64, 48), 32, 24);
        assert_eq!(rgb.len(), 32 * 24 * 3);
        assert!(is_pink(&rgb));
    }

    #[test]
    fn with_no_card_a_picture_that_isnt_i420_isnt_black_either() {
        let rgba = Frame::new(4, 2, telescope_core::frame::PixelFormat::Rgba, [10, 20, 30, 255].repeat(8));
        for ffmpeg in [false, true] {
            let mut scaler = PreviewScaler { gpu: Some(None), ..PreviewScaler::new(ffmpeg && ffmpeg_available()) };
            assert_eq!(scaler.rgb(&rgba, 2, 1), [10, 20, 30].repeat(2));
        }
    }

    #[test]
    fn without_ffmpeg_a_picture_still_reads() {
        // A 1 x 1 BMP with one red pixel
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&58u32.to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&1i32.to_le_bytes());
        bmp.extend_from_slice(&1i32.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        bmp.extend_from_slice(&[0; 24]);
        bmp.extend_from_slice(&[0, 0, 255, 0]);
        assert_eq!(&read_picture(&bmp, false).unwrap().data[..], &[255, 0, 0, 255]);
    }

    #[cfg(ffmpeg)]
    #[test]
    fn a_heavy_stream_without_ffmpeg_decodes_shows_and_ends_with_nothing_of_ffmpegs_to_drop() {
        let options = DecodeOptions::default();
        let mut decoder = make_decoder(Codec::Mjpeg, &options, new_tap(), false).unwrap();
        let mut scaler = PreviewScaler { gpu: Some(None), ..PreviewScaler::new(false) };
        assert!(scaler.cpu.is_none());
        let packet = Packet { data: testenc::jpeg_yuv(180, 100, 160, 64, 48), time_ns: 1 };
        let frame = decoder.decode(&packet).newest.unwrap();
        let rgb = scaler.rgb(&frame, 32, 24);
        assert!(rgb.as_chunks::<3>().0.iter().any(|p| p != &[0, 0, 0]));
        assert!(PreviewScaler::new(true).cpu.is_some());
    }

    #[cfg(ffmpeg)]
    #[test]
    fn with_ffmpeg_every_codec_opens() {
        let options = DecodeOptions::default();
        assert!(make_decoder(Codec::H264, &options, new_tap(), true).is_ok());
        assert!(make_decoder(Codec::Mjpeg, &options, new_tap(), true).is_ok());
    }
}
