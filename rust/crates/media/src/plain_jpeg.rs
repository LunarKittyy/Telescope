//! JPEG without FFmpeg, as the Python app reads MJPEG through OpenCV whether or not PyAV is there: a copy whose FFmpeg
//! didn't load, or a build without it, can still stream Heavy.

use crate::jpeg_guard::{jpeg_size, within_limits};
use crate::picture::{exif_orientation, oriented, MAX_PIXELS};
use crate::range::full_to_limited;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::media::{Decoded, Decoder, Packet};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// The pixels in `colour`, with their size and bytes per pixel; None for one that doesn't decode. A grey JPEG comes back grey whatever was asked.
fn read(jpeg: &[u8], colour: ColorSpace) -> Option<(usize, usize, usize, Vec<u8>)> {
    // Set before the headers are read, which is when zune-jpeg picks its YCbCr converter; JPEG itself stops at 65535 a side
    let options = DecoderOptions::default().set_max_width(65535).set_max_height(65535).jpeg_set_out_colorspace(colour);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    decoder.decode_headers().ok()?;
    let colour = if decoder.info()?.components == 1 { ColorSpace::Luma } else { colour };
    decoder.set_options(options.jpeg_set_out_colorspace(colour));
    let pixels = decoder.decode().ok()?;
    let (w, h) = decoder.dimensions()?;
    Some((w, h, colour.num_components(), pixels))
}

/// YCbCr per pixel (or grey), full range, to I420 in the limited range everything after the decoder reads. Each chroma sample is
/// the rounded average of its 2 x 2 block, or of what's left of it at an odd edge.
fn to_i420(w: usize, h: usize, step: usize, pixels: &[u8]) -> Vec<u8> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = vec![128u8; w * h + 2 * cw * ch];
    let (luma, chroma) = out.split_at_mut(w * h);
    if step == 1 {
        luma.copy_from_slice(&pixels[..w * h]);
    } else {
        for (l, px) in luma.iter_mut().zip(pixels.as_chunks::<3>().0) {
            *l = px[0];
        }
    }
    if step == 3 {
        let (u, v) = chroma.split_at_mut(cw * ch);
        for (cy, (u, v)) in u.chunks_exact_mut(cw).zip(v.chunks_exact_mut(cw)).enumerate() {
            let top = &pixels[cy * 2 * w * 3..][..w * 3];
            // The last row of an odd height has no row under it
            let under = (cy * 2 + 1 < h).then(|| &pixels[(cy * 2 + 1) * w * 3..][..w * 3]);
            match under {
                Some(under) => {
                    for (((u, v), a), b) in u.iter_mut().zip(v.iter_mut()).zip(top.chunks_exact(6)).zip(under.chunks_exact(6)) {
                        *u = ((a[1] as u32 + a[4] as u32 + b[1] as u32 + b[4] as u32 + 2) >> 2) as u8;
                        *v = ((a[2] as u32 + a[5] as u32 + b[2] as u32 + b[5] as u32 + 2) >> 2) as u8;
                    }
                    if w % 2 == 1 {
                        let (a, b) = (&top[(w - 1) * 3..], &under[(w - 1) * 3..]);
                        u[cw - 1] = ((a[1] as u32 + b[1] as u32 + 1) >> 1) as u8;
                        v[cw - 1] = ((a[2] as u32 + b[2] as u32 + 1) >> 1) as u8;
                    }
                }
                None => {
                    for ((u, v), a) in u.iter_mut().zip(v.iter_mut()).zip(top.chunks_exact(6)) {
                        *u = ((a[1] as u32 + a[4] as u32 + 1) >> 1) as u8;
                        *v = ((a[2] as u32 + a[5] as u32 + 1) >> 1) as u8;
                    }
                    if w % 2 == 1 {
                        u[cw - 1] = top[(w - 1) * 3 + 1];
                        v[cw - 1] = top[(w - 1) * 3 + 2];
                    }
                }
            }
        }
    }
    full_to_limited(&mut out, w, h);
    out
}

/// The JPEG in a packet as I420, or None if it doesn't decode or declares more pixels than we'll allocate.
pub fn frame(jpeg: &[u8]) -> Option<Frame> {
    if !within_limits(jpeg) {
        return None;
    }
    let (w, h, step, pixels) = read(jpeg, ColorSpace::YCbCr)?;
    Some(Frame::new(w as u32, h as u32, PixelFormat::I420, to_i420(w, h, step, &pixels)))
}

/// A JPEG picture as RGBA, turned by its EXIF. A file someone picked isn't held to a peer's limits: phones take bigger photos than that.
pub fn picture(jpeg: &[u8]) -> Option<Frame> {
    jpeg_size(jpeg).filter(|&(w, h)| w as u64 * h as u64 <= MAX_PIXELS)?;
    // RGB and not RGBA: zune-jpeg's CMYK conversions step through RGBA rows three bytes at a time
    let (w, h, step, pixels) = read(jpeg, ColorSpace::RGB)?;
    let rgba = if step == 1 {
        pixels.iter().flat_map(|&g| [g, g, g, 255]).collect()
    } else {
        pixels.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect()
    };
    Some(oriented(rgba, w, h, exif_orientation(jpeg)))
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
        // Y 81, U 90, V 240 in a JPEG's full range is a strong red, which an RGB conversion written into RGBA rows smears into other colours
        let f = picture(&testenc::jpeg_yuv(81, 90, 240, 16, 16)).unwrap();
        assert_eq!((f.width, f.height, f.format), (16, 16, PixelFormat::Rgba));
        for px in f.data.as_chunks::<4>().0 {
            assert!(px[..3].iter().zip([238, 14, 14]).all(|(&a, b)| a.abs_diff(b) <= 3) && px[3] == 255, "came out {px:?}");
        }
    }

    #[test]
    fn a_picture_is_not_held_to_a_streams_limits() {
        let jpeg = testenc::jpeg(128, 20000, 2);
        assert!(frame(&jpeg).is_none());
        assert_eq!(picture(&jpeg).map(|f| (f.width, f.height)), Some((20000, 2)));
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

#[cfg(test)]
mod conversion_tests {
    use super::*;
    use std::time::Instant;

    fn pixels(w: usize, h: usize, step: usize) -> Vec<u8> {
        let mut x = 12345u32;
        (0..w * h * step).map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 9) as u8
        }).collect()
    }

    /// The conversion as it was first written: luma gathered, then each chroma sample summed over its block with edge checks.
    fn to_i420_reference(w: usize, h: usize, step: usize, pixels: &[u8]) -> Vec<u8> {
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

    #[test]
    fn the_loops_give_the_bytes_of_the_first_version() {
        for (w, h) in [(1, 1), (1, 2), (2, 1), (1, 7), (7, 1), (2, 2), (3, 3), (4, 5), (5, 4), (16, 9), (17, 9), (33, 18), (64, 63), (65, 65)] {
            for step in [1, 3] {
                let px = pixels(w, h, step);
                assert!(to_i420(w, h, step, &px) == to_i420_reference(w, h, step, &px), "{w}x{h} at {step} bytes a pixel");
            }
        }
    }

    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture what_the_jpeg_chroma_costs
    #[test]
    #[ignore = "a measurement"]
    fn what_the_jpeg_chroma_costs() {
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            for step in [3, 1] {
                let px = pixels(w, h, step);
                let mut times = Vec::new();
                let mut hash = 0u64;
                for round in 0..15 {
                    let began = Instant::now();
                    let out = to_i420(w, h, step, &px);
                    times.push(began.elapsed().as_secs_f64() * 1e3);
                    if round == 0 {
                        hash = out.iter().fold(1469598103934665603u64, |a, &b| (a ^ b as u64).wrapping_mul(1099511628211));
                    }
                }
                times.sort_by(|a, b| a.partial_cmp(b).unwrap());
                println!("{w}x{h} {} bytes a pixel: median {:.2} ms, best {:.2} ms, hash {hash:016x}", step, times[7], times[0]);
            }
        }
    }
}
