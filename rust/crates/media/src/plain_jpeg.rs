//! JPEG without FFmpeg, as the Python app reads MJPEG through OpenCV whether or not PyAV is there: a copy whose FFmpeg
//! didn't load, or a build without it, can still stream Heavy.

use crate::jpeg_guard::within_limits;
use crate::range::full_to_limited;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::media::{Decoded, Decoder, Packet};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// The pixels in `colour`, with their size and bytes per pixel; None for one that doesn't decode. A grey JPEG comes back grey whatever was asked.
fn read(jpeg: &[u8], colour: ColorSpace) -> Option<(usize, usize, usize, Vec<u8>)> {
    if !within_limits(jpeg) {
        return None;
    }
    let mut decoder = JpegDecoder::new(ZCursor::new(jpeg));
    decoder.decode_headers().ok()?;
    let colour = if decoder.info()?.components == 1 { ColorSpace::Luma } else { colour };
    decoder.set_options(DecoderOptions::default().jpeg_set_out_colorspace(colour));
    let pixels = decoder.decode().ok()?;
    let (w, h) = decoder.dimensions()?;
    Some((w, h, colour.num_components(), pixels))
}

/// YCbCr per pixel (or grey), full range, to I420 in the limited range everything after the decoder reads.
fn to_i420(w: usize, h: usize, step: usize, pixels: &[u8]) -> Vec<u8> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = vec![128u8; w * h + 2 * cw * ch];
    let (luma, chroma) = out.split_at_mut(w * h);
    for (y, row) in luma.chunks_exact_mut(w).enumerate() {
        for (x, p) in row.iter_mut().enumerate() {
            *p = pixels[(y * w + x) * step];
        }
    }
    if step == 3 {
        let (u, v) = chroma.split_at_mut(cw * ch);
        for cy in 0..ch {
            for cx in 0..cw {
                let (mut sum, mut count) = ([0u32; 2], 0);
                for y in (cy * 2)..(cy * 2 + 2).min(h) {
                    for x in (cx * 2)..(cx * 2 + 2).min(w) {
                        let at = (y * w + x) * 3;
                        sum[0] += pixels[at + 1] as u32;
                        sum[1] += pixels[at + 2] as u32;
                        count += 1;
                    }
                }
                u[cy * cw + cx] = ((sum[0] + count / 2) / count) as u8;
                v[cy * cw + cx] = ((sum[1] + count / 2) / count) as u8;
            }
        }
    }
    full_to_limited(&mut out, w, h);
    out
}

/// The JPEG in a packet as I420, or None if it doesn't decode or declares more pixels than we'll allocate.
pub fn frame(jpeg: &[u8]) -> Option<Frame> {
    let (w, h, step, pixels) = read(jpeg, ColorSpace::YCbCr)?;
    Some(Frame::new(w as u32, h as u32, PixelFormat::I420, to_i420(w, h, step, &pixels)))
}

/// A JPEG picture as RGBA. Without FFmpeg it's the one kind of picture file that reads, and it isn't turned by its EXIF.
pub fn picture(jpeg: &[u8]) -> Option<Frame> {
    let (w, h, step, pixels) = read(jpeg, ColorSpace::RGBA)?;
    let rgba = if step == 1 { pixels.iter().flat_map(|&g| [g, g, g, 255]).collect() } else { pixels };
    Some(Frame::new(w as u32, h as u32, PixelFormat::Rgba, rgba))
}

/// One JPEG per packet, like mjpeg::Mjpeg.
pub struct PlainJpeg;

impl Decoder for PlainJpeg {
    fn decode(&mut self, packet: &Packet) -> Decoded {
        let mut newest = frame(&packet.data);
        if let Some(f) = newest.as_mut() {
            f.time_ns = packet.time_ns;
        }
        Decoded { count: newest.is_some() as u32, newest }
    }

    fn hardware(&self) -> bool {
        false
    }
}

#[cfg(all(test, ffmpeg))]
mod tests {
    use super::*;
    use crate::mjpeg::Mjpeg;
    use crate::testenc;

    #[test]
    fn it_reads_a_jpeg_within_a_level_or_two_of_ffmpeg() {
        for (w, h) in [(64, 48), (33, 17)] {
            let jpeg = testenc::jpeg_yuv(90, 100, 180, w, h);
            let (ours, theirs) = (frame(&jpeg).unwrap(), Mjpeg::new().unwrap().decode_jpeg(&jpeg).unwrap());
            assert_eq!((ours.width, ours.height, ours.format), (theirs.width, theirs.height, theirs.format));
            assert!(ours.data.iter().zip(theirs.data.iter()).all(|(a, b)| a.abs_diff(*b) <= 2));
        }
    }

    #[test]
    fn a_picture_comes_back_as_rgba() {
        let f = picture(&testenc::jpeg(128, 16, 16)).unwrap();
        assert_eq!((f.width, f.height, f.format), (16, 16, PixelFormat::Rgba));
    }

    #[test]
    fn garbage_and_oversized_claims_are_refused() {
        assert!(frame(b"hello there, not a jpeg").is_none());
        let mut big = testenc::jpeg(128, 16, 16);
        let at = big.windows(2).position(|p| p == [0xFF, 0xC0]).unwrap() + 5;
        big[at..at + 4].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(frame(&big).is_none());
    }
}
