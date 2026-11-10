//! Decoded pictures that aren't BT.601 in the limited range (a JPEG's full range, an HD stream's BT.709) turned into
//! it. Everything after the decoder (the frame steps, the preview, the virtual cameras) reads I420 as BT.601 limited,
//! which is also what the Python app hands its virtual camera: it converts to BGR with the stream's matrix and range
//! (PyAV's swscale), and OpenCV's BGR-to-I420 writes BT.601 limited. So the frame carries no matrix or range of its own.

use crate::ffmpeg::{aligned_start, padded_stride, Padded};
use ffmpeg_sys_next as ff;
use std::ptr;
use std::sync::Arc;
use telescope_core::frame::Bgr;

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

const SHIFT: u32 = 20;
const ROUND: i32 = 1 << (SHIFT - 1);

/// What bgr_to_i420 reads.
struct Rows<'a> {
    bgr: &'a [u8],
    stride: usize,
    width: usize,
    height: usize,
    cw: usize,
}

#[inline(always)]
fn i420_rows(p: &Rows, luma: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    for y in 0..p.height {
        let row = p.bgr[y * p.stride..y * p.stride + p.width * 3].as_chunks::<3>().0;
        for (l, px) in luma[y * p.width..(y + 1) * p.width].iter_mut().zip(row) {
            let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
            *l = ((269_484 * r + 528_482 * g + 102_760 * b + (16 << SHIFT) + ROUND) >> SHIFT) as u8;
        }
        if y % 2 == 0 {
            let (us, vs) = (&mut u[(y / 2) * p.cw..(y / 2 + 1) * p.cw], &mut v[(y / 2) * p.cw..(y / 2 + 1) * p.cw]);
            for ((a, c), px) in us.iter_mut().zip(vs.iter_mut()).zip(row.iter().step_by(2)) {
                let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
                *a = ((-155_188 * r - 305_135 * g + 460_324 * b + (128 << SHIFT) + ROUND) >> SHIFT) as u8;
                *c = ((460_324 * r - 385_875 * g - 74_448 * b + (128 << SHIFT) + ROUND) >> SHIFT) as u8;
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn i420_rows_avx2(p: &Rows, luma: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    i420_rows(p, luma, u, v);
}

/// BGR (rows `stride` bytes apart) to I420 the way OpenCV's COLOR_BGR2YUV_I420 does it: BT.601 limited, fixed point
/// with 20 bits, and a 2 x 2 block's chroma from its top left pixel.
pub fn bgr_to_i420(bgr: &[u8], stride: usize, width: usize, height: usize) -> Vec<u8> {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let mut out = vec![0u8; width * height + 2 * cw * ch];
    let (luma, chroma) = out.split_at_mut(width * height);
    let (u, v) = chroma.split_at_mut(cw * ch);
    let rows = Rows { bgr, stride, width, height, cw };
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the processor was just asked whether it has AVX2
        unsafe { i420_rows_avx2(&rows, luma, u, v) };
        return out;
    }
    i420_rows(&rows, luma, u, v);
    out
}

/// The conversions for pictures that aren't already BT.601 limited, with swscale's contexts and buffers kept between frames.
pub struct Converter {
    sws: *mut ff::SwsContext,
    // What the context was made for: size, matrix and range
    made_for: Option<(i32, i32, Encoding)>,
    src: Padded,
    // The BGR handed out with frames, to write the next one over once those frames are gone
    handed_out: Vec<Arc<Bgr>>,
}

/// How many BGR buffers are kept: a picture being made, the newest frame and the ones still on their way out.
const KEPT_BGR: usize = 4;

impl Default for Converter {
    fn default() -> Self {
        Converter { sws: ptr::null_mut(), made_for: None, src: Padded::default(), handed_out: Vec::new() }
    }
}

impl Converter {
    /// Packed I420 in this encoding as BT.601 limited I420, with the BGR that went through if it took any. A picture that can't
    /// be converted is returned as it was.
    pub fn standard(&mut self, mut i420: Vec<u8>, width: usize, height: usize, enc: Encoding) -> (Vec<u8>, Option<Arc<Bgr>>) {
        if is_bt601(enc.matrix) {
            if enc.full_range {
                full_to_limited(&mut i420, width, height);
            }
            return (i420, None);
        }
        match self.through_bgr(&i420, width, height, enc) {
            Some((converted, bgr)) => (converted, Some(bgr)),
            None => {
                log::warn!("A {width}x{height} picture in matrix {} couldn't be converted; showing it as BT.601", enc.matrix);
                (i420, None)
            }
        }
    }

    /// A buffer nothing else holds any more, or a new one.
    fn free_bgr(&mut self) -> Arc<Bgr> {
        match self.handed_out.iter().position(|b| Arc::strong_count(b) == 1) {
            Some(i) => self.handed_out.swap_remove(i),
            None => Arc::default(),
        }
    }

    fn through_bgr(&mut self, i420: &[u8], width: usize, height: usize, enc: Encoding) -> Option<(Vec<u8>, Arc<Bgr>)> {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let (y, rest) = i420.split_at(width * height);
        let (u, v) = rest.split_at(cw * ch);
        let (src, src_stride) = self.src.load_i420(width, height, (y, u, v));
        let stride = padded_stride(width * 3);
        let mut picture = self.free_bgr();
        let bgr = Arc::get_mut(&mut picture)?;
        bgr.stride = stride;
        bgr.start = aligned_start(&mut bgr.data, stride * height);
        // SAFETY: start and the tail behind it are inside data, as aligned_start says
        let out = unsafe { bgr.data.as_mut_ptr().add(bgr.start) };
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
        let i420 = bgr_to_i420(&bgr.data[bgr.start..][..stride * height], stride, width, height);
        if self.handed_out.len() < KEPT_BGR {
            self.handed_out.push(picture.clone());
        }
        Some((i420, picture))
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
    use crate::ffmpeg::Padded;

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
    fn the_plain_and_avx2_rows_give_the_same_bytes() {
        let (width, height) = (37usize, 11usize);
        let stride = width * 3 + 5;
        let bgr: Vec<u8> = (0..stride * height).map(|i| (i * i % 251) as u8).collect();
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let rows = Rows { bgr: &bgr, stride, width, height, cw };
        let (mut luma, mut u, mut v) = (vec![0; width * height], vec![0; cw * ch], vec![0; cw * ch]);
        i420_rows(&rows, &mut luma, &mut u, &mut v);
        assert_eq!(bgr_to_i420(&bgr, stride, width, height), [luma, u, v].concat());
    }

    fn bt709() -> Encoding {
        Encoding { matrix: 1, full_range: false }
    }

    fn a_picture(w: usize, h: usize) -> Vec<u8> {
        (0..w * h + 2 * w.div_ceil(2) * h.div_ceil(2)).map(|i| 40 + (i * 7 % 150) as u8).collect()
    }

    #[test]
    fn a_picture_that_went_through_bgr_comes_with_it() {
        let (w, h) = (34, 18);
        let (i420, bgr) = Converter::default().standard(a_picture(w, h), w, h, bt709());
        let bgr = bgr.expect("BT.709 goes through BGR");
        let packed: Vec<u8> = (0..h).flat_map(|y| bgr.row(y, w).to_vec()).collect();
        assert_eq!(bgr_to_i420(&packed, w * 3, w, h), i420);
    }

    #[test]
    fn bt601_pictures_make_no_bgr() {
        let (_, bgr) = Converter::default().standard(a_picture(34, 18), 34, 18, Encoding { matrix: 5, full_range: false });
        assert!(bgr.is_none());
    }

    #[test]
    fn a_bgr_buffer_is_used_again_once_its_frame_is_gone_and_never_before() {
        let mut c = Converter::default();
        let (w, h) = (34, 18);
        let (_, first) = c.standard(a_picture(w, h), w, h, bt709());
        let first = first.unwrap();
        let (_, second) = c.standard(a_picture(w, h), w, h, bt709());
        let second = second.unwrap();
        assert!(!Arc::ptr_eq(&first, &second), "a frame still holds the first");
        let at = Arc::as_ptr(&first);
        drop(first);
        let (_, third) = c.standard(a_picture(w, h), w, h, bt709());
        assert_eq!(Arc::as_ptr(&third.unwrap()), at);
        // Held for ever by nobody's fault, the kept ones stop growing
        let held: Vec<_> = (0..KEPT_BGR * 2).map(|_| c.standard(a_picture(w, h), w, h, bt709()).1).collect();
        assert!(c.handed_out.len() <= KEPT_BGR, "{}", held.len());
    }

    #[test]
    fn only_matrices_swscale_reads_as_bt601_skip_the_conversion() {
        // Unspecified, BT.470 B/G and SMPTE 170M are what Python's swscale treats as BT.601; BT.709 and BT.2020 aren't
        assert!([2, 3, 5, 6, 99].iter().all(|&m| is_bt601(m)));
        assert!(![1, 4, 7, 9, 10].iter().any(|&m| is_bt601(m)));
    }

    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture what_the_round_trip_costs
    #[test]
    #[ignore = "a measurement"]
    fn what_the_round_trip_costs() {
        use std::time::Instant;
        let med = |mut v: Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
            let pic = a_picture(w, h);
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let (y, rest) = pic.split_at(w * h);
            let (u, v) = rest.split_at(cw * ch);
            let mut conv = Converter::default();
            let (mut load, mut scale, mut back, mut whole) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let mut padded = Padded::default();
            let stride = padded_stride(w * 3);
            let mut bgr = vec![0u8; stride * h + 4096];
            let mut hash = 0u64;
            for round in 0..20 {
                let t = Instant::now();
                let (src, src_stride) = padded.load_i420(w, h, (y, u, v));
                load.push(t.elapsed().as_secs_f64() * 1e3);
                let t = Instant::now();
                unsafe {
                    let sws = ff::sws_getContext(w as i32, h as i32, ff::AVPixelFormat::AV_PIX_FMT_YUV420P, w as i32, h as i32, ff::AVPixelFormat::AV_PIX_FMT_BGR24,
                                                 ff::SwsFlags::SWS_BILINEAR as i32, ptr::null_mut(), ptr::null_mut(), ptr::null());
                    let dst = [bgr.as_mut_ptr(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut()];
                    let dst_stride = [stride as i32, 0, 0, 0];
                    ff::sws_scale(sws, src.as_ptr(), src_stride.as_ptr(), 0, h as i32, dst.as_ptr(), dst_stride.as_ptr());
                    ff::sws_freeContext(sws);
                }
                scale.push(t.elapsed().as_secs_f64() * 1e3);
                let t = Instant::now();
                let out = bgr_to_i420(&bgr, stride, w, h);
                back.push(t.elapsed().as_secs_f64() * 1e3);
                let t = Instant::now();
                let (out2, _) = conv.standard(pic.clone(), w, h, bt709());
                whole.push(t.elapsed().as_secs_f64() * 1e3);
                if round == 0 {
                    hash = out.iter().chain(out2.iter()).fold(1469598103934665603u64, |a, &b| (a ^ b as u64).wrapping_mul(1099511628211));
                }
            }
            println!("{w}x{h}: load {:.2} ms, sws (with its context made each time) {:.2} ms, bgr to i420 {:.2} ms, standard() whole incl. a 12 MB clone {:.2} ms, hash {hash:016x}",
                     med(load), med(scale), med(back), med(whole));
        }
    }
}
