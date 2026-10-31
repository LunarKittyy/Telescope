//! Decoded pictures that aren't BT.601 in the limited range (a JPEG's full range, an HD stream's BT.709) turned into
//! it. Everything after the decoder (the frame steps, the preview, the virtual cameras) reads I420 as BT.601 limited,
//! which is also what the Python app hands its virtual camera: it converts to BGR with the stream's matrix and range
//! (PyAV's swscale), and OpenCV's BGR-to-I420 writes BT.601 limited. So the frame carries no matrix or range of its own.

use crate::ffmpeg::{padded_stride, Padded};
use ffmpeg_sys_next as ff;
use std::ptr;

pub use crate::range::full_to_limited;

/// What a decoded picture's numbers mean: an AVColorSpace (BT.709 is 1, unspecified is 2) and whether they use 0..255.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encoding {
    pub matrix: i32,
    pub full_range: bool,
}

/// swscale's YUV to RGB coefficients for a matrix; the ones it falls back to for a number it doesn't know.
fn coefficients(matrix: i32) -> &'static [i32; 4] {
    // SAFETY: sws_getCoefficients returns a pointer to one of its static four-entry tables
    unsafe { &*(ff::sws_getCoefficients(matrix) as *const [i32; 4]) }
}

/// Whether swscale reads this matrix as BT.601, as it does the unspecified one (what a stream that doesn't say gets).
fn is_bt601(matrix: i32) -> bool {
    // AVCOL_SPC_BT470BG
    coefficients(matrix) == coefficients(5)
}

/// BGR (rows `stride` bytes apart) to I420 the way OpenCV's COLOR_BGR2YUV_I420 does it: BT.601 limited, fixed point
/// with 20 bits, and a 2 x 2 block's chroma from its top left pixel.
pub fn bgr_to_i420(bgr: &[u8], stride: usize, width: usize, height: usize) -> Vec<u8> {
    const SHIFT: u32 = 20;
    const ROUND: i32 = 1 << (SHIFT - 1);
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let mut out = vec![0u8; width * height + 2 * cw * ch];
    let (luma, chroma) = out.split_at_mut(width * height);
    let (u, v) = chroma.split_at_mut(cw * ch);
    for y in 0..height {
        let row = &bgr[y * stride..y * stride + width * 3];
        for (x, px) in row.as_chunks::<3>().0.iter().enumerate() {
            let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
            luma[y * width + x] = ((269_484 * r + 528_482 * g + 102_760 * b + (16 << SHIFT) + ROUND) >> SHIFT) as u8;
            if x % 2 == 0 && y % 2 == 0 {
                let at = (y / 2) * cw + x / 2;
                u[at] = ((-155_188 * r - 305_135 * g + 460_324 * b + (128 << SHIFT) + ROUND) >> SHIFT) as u8;
                v[at] = ((460_324 * r - 385_875 * g - 74_448 * b + (128 << SHIFT) + ROUND) >> SHIFT) as u8;
            }
        }
    }
    out
}

/// The conversions for pictures that aren't already BT.601 limited, with swscale's contexts and buffers kept between frames.
pub struct Converter {
    sws: *mut ff::SwsContext,
    // What the context was made for: size, matrix and range
    made_for: Option<(i32, i32, Encoding)>,
    src: Padded,
    dst: Padded,
}

impl Default for Converter {
    fn default() -> Self {
        Converter { sws: ptr::null_mut(), made_for: None, src: Padded::default(), dst: Padded::default() }
    }
}

impl Converter {
    /// Packed I420 in this encoding as BT.601 limited I420. A picture that can't be converted is returned as it was.
    pub fn standard(&mut self, mut i420: Vec<u8>, width: usize, height: usize, enc: Encoding) -> Vec<u8> {
        if is_bt601(enc.matrix) {
            if enc.full_range {
                full_to_limited(&mut i420, width, height);
            }
            return i420;
        }
        match self.through_bgr(&i420, width, height, enc) {
            Some(converted) => converted,
            None => {
                log::warn!("A {width}x{height} picture in matrix {} couldn't be converted; showing it as BT.601", enc.matrix);
                i420
            }
        }
    }

    fn through_bgr(&mut self, i420: &[u8], width: usize, height: usize, enc: Encoding) -> Option<Vec<u8>> {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let (y, rest) = i420.split_at(width * height);
        let (u, v) = rest.split_at(cw * ch);
        let (src, src_stride) = self.src.load_i420(width, height, (y, u, v));
        let stride = padded_stride(width * 3);
        let out = self.dst.reserve(stride * height);
        // SAFETY: src and out are padded as the strides say, and the context is made for exactly this size and encoding before it runs
        unsafe {
            if self.made_for != Some((width as i32, height as i32, enc)) {
                ff::sws_freeContext(self.sws);
                self.made_for = None;
                self.sws = ff::sws_getContext(width as i32, height as i32, ff::AVPixelFormat::AV_PIX_FMT_YUV420P, width as i32, height as i32,
                                              ff::AVPixelFormat::AV_PIX_FMT_BGR24, ff::SwsFlags::SWS_BILINEAR as i32,
                                              ptr::null_mut(), ptr::null_mut(), ptr::null());
                if self.sws.is_null() {
                    return None;
                }
                let (to_rgb, from_rgb) = (coefficients(enc.matrix), coefficients(5));
                // The stream's own matrix and range in; the BGR end has no range to speak of
                let r = ff::sws_setColorspaceDetails(self.sws, to_rgb.as_ptr(), enc.full_range as i32, from_rgb.as_ptr(), 0, 0, 1 << 16, 1 << 16);
                if r < 0 {
                    return None;
                }
                self.made_for = Some((width as i32, height as i32, enc));
            }
            let dst = [out, ptr::null_mut(), ptr::null_mut(), ptr::null_mut()];
            let dst_stride = [stride as i32, 0, 0, 0];
            ff::sws_scale(self.sws, src.as_ptr(), src_stride.as_ptr(), 0, height as i32, dst.as_ptr(), dst_stride.as_ptr());
        }
        Some(bgr_to_i420(self.dst.bytes(0, stride * height), stride, width, height))
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: made by sws_getContext, freed once (the free takes null)
        unsafe { ff::sws_freeContext(self.sws) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ends_of_each_range_land_on_the_ends_of_the_other() {
        let mut p = vec![0, 255, 16, 235, 0, 255, 128, 16];
        full_to_limited(&mut p, 4, 1);
        assert_eq!(p, [16, 235, 30, 218, 16, 240, 128, 30]);
    }

    #[test]
    fn bgr_becomes_i420_as_opencv_makes_it() {
        // A 6 x 4 picture of random bytes and what cv2.cvtColor(.., COLOR_BGR2YUV_I420) made of it
        let bgr: [u8; 72] = [
            139, 74, 229, 241, 169, 65, 6, 160, 149, 106, 38, 175, 188, 205, 175, 229, 98, 249, 10, 148, 95, 86, 147, 198, 66, 39, 106, 213, 171, 45,
            167, 57, 69, 81, 55, 14, 153, 178, 215, 76, 52, 39, 250, 72, 215, 50, 161, 223, 112, 172, 161, 233, 38, 17, 89, 1, 221, 6, 242, 127, 143,
            6, 60, 210, 94, 25, 166, 33, 249, 189, 12, 204,
        ];
        let want: [u8; 36] = [
            126, 141, 136, 91, 183, 152, 116, 149, 69, 135, 79, 55, 176, 60, 132, 159, 155, 62, 82, 171, 48, 90, 113, 93, 134, 62, 125, 112, 185, 103,
            191, 134, 116, 146, 178, 127,
        ];
        assert_eq!(bgr_to_i420(&bgr, 18, 6, 4), want);
        // Rows padded past their pixels change nothing
        let mut padded = Vec::new();
        for row in bgr.chunks(18) {
            padded.extend_from_slice(row);
            padded.extend_from_slice(&[9; 6]);
        }
        assert_eq!(bgr_to_i420(&padded, 24, 6, 4), want);
    }

    #[test]
    fn only_matrices_swscale_reads_as_bt601_skip_the_conversion() {
        // Unspecified, BT.470 B/G and SMPTE 170M are what Python's swscale treats as BT.601; BT.709 and BT.2020 aren't
        assert!([2, 3, 5, 6, 99].iter().all(|&m| is_bt601(m)));
        assert!(![1, 4, 7, 9, 10].iter().any(|&m| is_bt601(m)));
    }
}
