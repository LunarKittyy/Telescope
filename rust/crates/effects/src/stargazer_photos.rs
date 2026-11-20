//! Stargazer's photo input: reading the JPEG a photo request saved, working out how far to shrink it so a stack fits in memory, and the
//! preview-sized copy of a stack that goes on the camera. Plain functions with no plugin in them, so the worker can run them on its thread.

use std::path::Path;
use telescope_core::frame::{Frame, PixelFormat};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// A photo file bigger than this is never read.
pub const MAX_FILE_BYTES: u64 = 256_000_000;
/// The most a photo is shrunk by, per side.
pub const MAX_FACTOR: usize = 16;
/// The camera's copy of a stack is only made up to this many pixels, a 4K picture.
pub const PREVIEW_MAX_PIXELS: usize = 3840 * 2160;
/// What turning a picture into the stacker's input takes per pixel while it is added: the three colour bytes it is read as.
const ADD_BYTES_PER_PIXEL: usize = 3;
/// A decoded picture as RGBA, and the stack's result as RGBA.
const RGBA: usize = 4;

/// How a run takes its photos: the whole number each side is divided by and the size that leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub factor: usize,
    /// The photo's own size.
    pub source: (usize, usize),
    /// What is stacked.
    pub size: (usize, usize),
}

/// The size a photo of this size becomes when its sides are divided by factor (never under one pixel).
pub fn scaled(source: (usize, usize), factor: usize) -> (usize, usize) {
    ((source.0 / factor.max(1)).max(1), (source.1 / factor.max(1)).max(1))
}

/// What a run holding photos of this size at this factor has in memory at once, counted before any of it is allocated: the file's bytes, the
/// decoder's RGB picture, the picture as it is stacked (RGBA and the stacker's copy of it), the stacker's accumulator, the stack's result and
/// the copy that goes to the camera.
pub fn bytes_needed(file: usize, source: (usize, usize), factor: usize, accumulator_per_pixel: usize, preview_pixels: usize) -> usize {
    let (w, h) = scaled(source, factor);
    let pixels = w.saturating_mul(h);
    let per_pixel = RGBA + ADD_BYTES_PER_PIXEL + accumulator_per_pixel + RGBA;
    file.saturating_add(source.0.saturating_mul(source.1).saturating_mul(3)).saturating_add(pixels.saturating_mul(per_pixel)).saturating_add(preview_pixels.saturating_mul(RGBA))
}

/// The smallest shrinking that keeps the run under the budget. None when even the most doesn't.
pub fn plan(file: usize, source: (usize, usize), accumulator_per_pixel: usize, preview_pixels: usize, budget: usize) -> Option<Plan> {
    if source.0 == 0 || source.1 == 0 {
        return None;
    }
    (1..=MAX_FACTOR)
        .find(|&factor| bytes_needed(file, source, factor, accumulator_per_pixel, preview_pixels) <= budget)
        .map(|factor| Plan { factor, source, size: scaled(source, factor) })
}

/// A photo's file, read, with its size from the header.
pub struct Photo {
    bytes: Vec<u8>,
    pub size: (usize, usize),
}

impl Photo {
    /// The file's size in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Decodes the photo and divides its sides by factor, averaging each block, into an RGBA picture.
    pub fn decode(self, factor: usize) -> Result<Frame, String> {
        let factor = factor.clamp(1, MAX_FACTOR);
        let Photo { bytes, size: (w, h) } = self;
        let rgb = {
            let mut decoder = JpegDecoder::new_with_options(ZCursor::new(&bytes), DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB));
            decoder.decode().map_err(|e| format!("{e:?}"))?
        };
        drop(bytes);
        if rgb.len() != w * h * 3 {
            return Err("the photo's pixels don't fit its size".into());
        }
        let (ow, oh) = scaled((w, h), factor);
        let mut out = vec![255u8; ow * oh * 4];
        let n = (factor * factor) as u32;
        for oy in 0..oh {
            for ox in 0..ow {
                let mut sum = [0u32; 3];
                for dy in 0..factor {
                    let row = ((oy * factor + dy) * w + ox * factor) * 3;
                    for dx in 0..factor {
                        let at = row + dx * 3;
                        sum[0] += u32::from(rgb[at]);
                        sum[1] += u32::from(rgb[at + 1]);
                        sum[2] += u32::from(rgb[at + 2]);
                    }
                }
                let to = (oy * ow + ox) * 4;
                for c in 0..3 {
                    out[to + c] = ((sum[c] + n / 2) / n) as u8;
                }
            }
        }
        Ok(Frame::new(ow as u32, oh as u32, PixelFormat::Rgba, out))
    }
}

/// Reads a photo's file and its size, refusing a file that is too big before any of it is read.
pub fn read(path: &Path) -> Result<Photo, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_FILE_BYTES {
        return Err("the photo's file is too big".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut decoder = JpegDecoder::new(ZCursor::new(&bytes));
    decoder.decode_headers().map_err(|e| format!("{e:?}"))?;
    let size = decoder.dimensions().ok_or("the photo has no size")?;
    Ok(Photo { bytes, size })
}

/// The picture fitted inside width by height, as large as it goes with its shape kept, on black. The camera's copy of a stack, which has to be
/// the size of the video's picture to go on it.
pub fn fit_into(frame: &Frame, width: u32, height: u32) -> Option<Frame> {
    let (sw, sh) = (frame.width as usize, frame.height as usize);
    let (tw, th) = (width as usize, height as usize);
    if sw == 0 || sh == 0 || tw == 0 || th == 0 || frame.format != PixelFormat::Rgba || frame.data.len() != sw * sh * 4 {
        return None;
    }
    // The scale that makes the picture fit, in thousandths so the size comes out in whole pixels
    let scale = (tw * 1000 / sw).min(th * 1000 / sh).max(1);
    let (iw, ih) = ((sw * scale / 1000).clamp(1, tw), (sh * scale / 1000).clamp(1, th));
    let (left, top) = ((tw - iw) / 2, (th - ih) / 2);
    let mut out = vec![0u8; tw * th * 4];
    for px in out.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    for y in 0..ih {
        let fy = ((y as f32 + 0.5) * sh as f32 / ih as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, ty) = ((fy as usize).min(sh - 1), fy.fract());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..iw {
            let fx = ((x as f32 + 0.5) * sw as f32 / iw as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, tx) = ((fx as usize).min(sw - 1), fx.fract());
            let x1 = (x0 + 1).min(sw - 1);
            let to = ((top + y) * tw + left + x) * 4;
            for c in 0..3 {
                let at = |xx: usize, yy: usize| f32::from(frame.data[(yy * sw + xx) * 4 + c]);
                let upper = at(x0, y0) * (1.0 - tx) + at(x1, y0) * tx;
                let lower = at(x0, y1) * (1.0 - tx) + at(x1, y1) * tx;
                out[to + c] = (upper * (1.0 - ty) + lower * ty + 0.5) as u8;
            }
        }
    }
    Some(Frame::new(width, height, PixelFormat::Rgba, out))
}

#[cfg(test)]
#[path = "stargazer_photos_tests.rs"]
mod tests;
