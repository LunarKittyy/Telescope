//! Decoded pictures that aren't BT.601 in the limited range (a JPEG's full range, an HD stream's BT.709) turned into
//! it. Everything after the decoder (the frame steps, the preview, the virtual cameras) reads I420 as BT.601 limited,
//! which is also what the Python app hands its virtual camera: it converts to BGR with the stream's matrix and range
//! (PyAV's swscale), and OpenCV's BGR-to-I420 writes BT.601 limited. So the frame carries no matrix or range of its own.
//! The conversion here goes straight from YUV to YUV (a matrix per pixel, clamped where the 8-bit BGR would be) and lands within
//! 3 levels of that trip through BGR (a picture with an odd height still takes the trip, as swscale stretches its chroma). The BGR itself, which the effects read, is only made when one asks.

use crate::ffmpeg::{aligned_start, padded_stride, Padded};
use ffmpeg_sys_next as ff;
use std::cell::RefCell;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::Arc;
use telescope_core::frame::{spare_bgr, Bgr, LazyBgr};

pub use crate::range::full_to_limited;

/// What a decoded picture's numbers mean: an AVColorSpace (BT.709 is 1, unspecified is 2) and whether they use 0..255.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encoding {
    pub matrix: i32,
    pub full_range: bool,
}

/// One plane of a decoded picture where the decoder left it, rows `stride` bytes apart.
#[derive(Clone, Copy)]
pub struct Plane<'a> {
    pub bytes: &'a [u8],
    pub stride: usize,
}

impl Plane<'_> {
    #[inline(always)]
    fn row(&self, y: usize, len: usize) -> &[u8] {
        &self.bytes[y * self.stride..][..len]
    }
}

/// A decoded picture's chroma: U and V planes of their own, or NV12's U, V pairs in one.
#[derive(Clone, Copy)]
pub enum Chroma<'a> {
    Split(Plane<'a>, Plane<'a>),
    Paired(Plane<'a>),
}

/// A decoded 4:2:0 picture's planes, read where they are instead of copied out first.
#[derive(Clone, Copy)]
pub struct Planes<'a> {
    pub luma: Plane<'a>,
    pub chroma: Chroma<'a>,
}

impl<'a> Planes<'a> {
    /// Packed I420: each plane straight after the one before, rows without padding.
    pub fn packed(i420: &'a [u8], width: usize, height: usize) -> Planes<'a> {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let (y, rest) = i420.split_at(width * height);
        let (u, v) = rest.split_at(cw * ch);
        Planes { luma: Plane { bytes: y, stride: width }, chroma: Chroma::Split(Plane { bytes: u, stride: cw }, Plane { bytes: v, stride: cw }) }
    }
}

/// The planes as packed I420, NV12's pairs split as they go.
pub fn packed(planes: &Planes, width: usize, height: usize) -> Vec<u8> {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let mut out = Vec::with_capacity(width * height + 2 * cw * ch);
    for y in 0..height {
        out.extend_from_slice(planes.luma.row(y, width));
    }
    match planes.chroma {
        Chroma::Split(u, v) => {
            for plane in [u, v] {
                for y in 0..ch {
                    out.extend_from_slice(plane.row(y, cw));
                }
            }
        }
        Chroma::Paired(uv) => {
            // Both planes written in one pass over the pairs, into room that is never cleared first
            let (u, v) = out.spare_capacity_mut()[..2 * cw * ch].split_at_mut(cw * ch);
            for (y, (u, v)) in u.chunks_exact_mut(cw).zip(v.chunks_exact_mut(cw)).enumerate() {
                for ((pair, u), v) in uv.row(y, cw * 2).as_chunks::<2>().0.iter().zip(u).zip(v) {
                    u.write(pair[0]);
                    v.write(pair[1]);
                }
            }
            // SAFETY: the loop above wrote all ch rows of cw samples of both planes
            unsafe { out.set_len(width * height + 2 * cw * ch) };
        }
    }
    out
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

// The path a picture used to take (through BGR and back): what the tests measure the matrix against, and what a picture with an odd height still takes
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

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
fn i420_rows_sse41(p: &Rows, luma: &mut [u8], u: &mut [u8], v: &mut [u8]) {
    i420_rows(p, luma, u, v);
}

/// BGR (rows `stride` bytes apart) to I420 the way OpenCV's COLOR_BGR2YUV_I420 does it: BT.601 limited, fixed point
/// with 20 bits, and a 2 x 2 block's chroma from its top left pixel.
fn bgr_to_i420(bgr: &[u8], stride: usize, width: usize, height: usize) -> Vec<u8> {
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
    #[cfg(target_arch = "x86_64")]
    if telescope_core::image::sse41_wanted() {
        // SAFETY: the processor was just asked whether it has SSE4.1
        unsafe { i420_rows_sse41(&rows, luma, u, v) };
        return out;
    }
    i420_rows(&rows, luma, u, v);
    out
}

/// What turns a picture in one encoding into RGB, as fixed point numbers with 16 fractional bits: swscale's coefficients
/// for the stream's matrix and range.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Matrix {
    // What a step of luma above black is worth in RGB, and the luma that is black
    luma: i32,
    black: i32,
    crv: i32,
    cbu: i32,
    cgu: i32,
    cgv: i32,
}

impl Matrix {
    fn new(enc: Encoding) -> Matrix {
        let table = coefficients(enc.matrix);
        // swscale's coefficients take limited chroma (224 steps) to the 255 of RGB; full chroma needs less of them
        let (luma, black, chroma): (f64, i32, f64) = if enc.full_range { (1.0, 0, 224.0 / 255.0) } else { (255.0 / 219.0, 16, 1.0) };
        let scaled = |c: i32| (c as f64 * chroma).round() as i32;
        Matrix { luma: (luma * 65536.0).round() as i32, black, crv: scaled(table[0]), cbu: scaled(table[1]), cgu: scaled(table[2]), cgv: scaled(table[3]) }
    }
}

/// The matrix as convert.wgsl's cpu_colour takes it, to make the same bytes as the rows here: luma gain, black, then the chroma terms.
pub(crate) fn card_terms(enc: Encoding) -> [i32; 8] {
    let m = Matrix::new(enc);
    [m.luma >> 2, m.black, m.crv, m.cbu, m.cgu, m.cgv, 0, 0]
}

/// What a row of chroma samples adds to red, green and blue (5 fractional bits).
#[derive(Default)]
struct Offsets {
    red: Vec<i16>,
    green: Vec<i16>,
    blue: Vec<i16>,
}

impl Offsets {
    fn planes(&mut self) -> [&mut Vec<i16>; 3] {
        [&mut self.red, &mut self.green, &mut self.blue]
    }

    fn resize(&mut self, len: usize) {
        self.planes().into_iter().for_each(|p| p.resize(len, 0));
    }

    /// What the samples of these U and V rows add.
    #[inline(always)]
    fn fill(&mut self, m: &Matrix, us: &[u8], vs: &[u8]) {
        for (((r, g), b), (&u, &v)) in self.red.iter_mut().zip(&mut self.green).zip(&mut self.blue).zip(us.iter().zip(vs)) {
            let (u, v) = (u as i32 - 128, v as i32 - 128);
            let q5 = |x: i32| (x >> 11) as i16;
            *r = q5(m.crv * v);
            *g = q5(-(m.cgu * u + m.cgv * v));
            *b = q5(m.cbu * u);
        }
    }

    /// What the samples of a row of NV12's U, V pairs add.
    #[inline(always)]
    fn fill_paired(&mut self, m: &Matrix, uv: &[u8]) {
        for (((r, g), b), pair) in self.red.iter_mut().zip(&mut self.green).zip(&mut self.blue).zip(uv.as_chunks::<2>().0) {
            let (u, v) = (pair[0] as i32 - 128, pair[1] as i32 - 128);
            let q5 = |x: i32| (x >> 11) as i16;
            *r = q5(m.crv * v);
            *g = q5(-(m.cgu * u + m.cgv * v));
            *b = q5(m.cbu * u);
        }
    }

    /// Spread over the pixels: each sample's offsets for both of the pair it colours (an odd width's last sample has one pixel).
    #[inline(always)]
    fn spread(&self, to: &mut Offsets) {
        for (from, to) in [(&self.red, &mut to.red), (&self.green, &mut to.green), (&self.blue, &mut to.blue)] {
            for (pair, &a) in to.as_chunks_mut::<2>().0.iter_mut().zip(from) {
                *pair = [a, a];
            }
        }
    }
}

/// The scratch space the matrix rows work in: a row of chroma, and the same spread over its pixels.
#[derive(Default)]
struct Scratch {
    chroma: Offsets,
    spread: Offsets,
}

/// A pixel's red, green and blue (8-bit, clamped as the BGR would be) from its luma and its chroma sample's offsets. Sixteen
/// bit lanes, so a vector holds sixteen pixels.
#[inline(always)]
fn colour(gain: i16, black: i16, y: u8, [r, g, b]: [i16; 3]) -> [i16; 3] {
    // No array::map: a build without link-time optimisation leaves it a call, and the rows 30 times slower
    // 5 fractional bits
    let base = ((((y as i16 - black) << 7) as i32 * gain as i32) >> 16) as i16;
    [level(base, r), level(base, g), level(base, b)]
}

/// One of a pixel's red, green or blue from its luma's part and its chroma's.
#[inline(always)]
fn level(base: i16, offset: i16) -> i16 {
    ((base + offset) >> 5).clamp(0, 255)
}

/// Weighs colours as OpenCV's BT.601 does: a bias with 6 fractional bits, then each of R, G, B at 15 fractional bits.
#[inline(always)]
fn weigh(bias: i16, rgb: [i16; 3], weights: [i16; 3]) -> i16 {
    // An indexed loop, for the same reason as in colour: a fold over a zip stays calls too, and writing the three out ran slower
    let mut sum = bias;
    #[allow(clippy::needless_range_loop)]
    for i in 0..3 {
        sum += part(rgb[i], weights[i]);
    }
    (sum + 32) >> 6
}

/// A colour times its weight, as OpenCV's fixed point takes it.
#[inline(always)]
fn part(c: i16, w: i16) -> i16 {
    (((c << 7) as i32 * w as i32) >> 16) as i16
}

/// OpenCV's BT.601 weights of R, G, B for luma, U and V, at 15 fractional bits.
const LUMA: [i16; 3] = [8421, 16515, 3211];
const BLUE_DIFF: [i16; 3] = [-4850, -9535, 14385];
const RED_DIFF: [i16; 3] = [14385, -12059, -2327];

/// One row of luma as BT.601 limited out of the stream's.
#[inline(always)]
fn luma_row(gain: i16, black: i16, y: &[u8], offsets: &Offsets, out: &mut [MaybeUninit<u8>]) {
    let n = out.len();
    let (y, red, green, blue) = (&y[..n], &offsets.red[..n], &offsets.green[..n], &offsets.blue[..n]);
    for ((((l, &y), &r), &g), &b) in out.iter_mut().zip(y).zip(red).zip(green).zip(blue) {
        l.write(weigh(16 << 6, colour(gain, black, y, [r, g, b]), LUMA) as u8);
    }
}

/// The planes of `src` (in the encoding `m` is for) as BT.601 limited packed I420 in `out`, every byte of it written: what going through 8-bit BGR does
/// (the clamping included), without making the BGR. Each chroma sample colours its 2 x 2 pixels, and the top left one's colour
/// gives the sample back, which is what swscale does too when the height is even (a picture with an odd height gets its chroma
/// stretched and takes `standard`'s old way).
#[inline(always)]
fn matrix_rows(m: &Matrix, src: &Planes, out: &mut [MaybeUninit<u8>], scratch: &mut Scratch, width: usize, height: usize) {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let (y_out, chroma_out) = out.split_at_mut(width * height);
    let (u_out, v_out) = chroma_out.split_at_mut(cw * ch);
    let Scratch { chroma, spread } = scratch;
    chroma.resize(cw);
    spread.resize(cw * 2);
    let (gain, black) = ((m.luma >> 2) as i16, m.black as i16);
    for cy in 0..ch {
        match src.chroma {
            Chroma::Split(u, v) => chroma.fill(m, u.row(cy, cw), v.row(cy, cw)),
            Chroma::Paired(uv) => chroma.fill_paired(m, uv.row(cy, cw * 2)),
        }
        chroma.spread(spread);
        // The chroma out of the top left pixel of each block
        let top = src.luma.row(cy * 2, width);
        let (us_out, vs_out) = (&mut u_out[cy * cw..(cy + 1) * cw], &mut v_out[cy * cw..(cy + 1) * cw]);
        let whole = top.as_chunks::<2>().0.iter().zip(&chroma.red).zip(&chroma.green).zip(&chroma.blue);
        for ((((y, &r), &g), &b), (uo, vo)) in whole.zip(us_out.iter_mut().zip(vs_out.iter_mut())) {
            let rgb = colour(gain, black, y[0], [r, g, b]);
            uo.write(weigh(128 << 6, rgb, BLUE_DIFF) as u8);
            vo.write(weigh(128 << 6, rgb, RED_DIFF) as u8);
        }
        if width % 2 == 1 {
            let i = cw - 1;
            let rgb = colour(gain, black, top[width - 1], [chroma.red[i], chroma.green[i], chroma.blue[i]]);
            us_out[i].write(weigh(128 << 6, rgb, BLUE_DIFF) as u8);
            vs_out[i].write(weigh(128 << 6, rgb, RED_DIFF) as u8);
        }
        for y in (cy * 2..cy * 2 + 2).filter(|&y| y < height) {
            luma_row(gain, black, src.luma.row(y, width), spread, &mut y_out[y * width..(y + 1) * width]);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn matrix_rows_avx2(m: &Matrix, src: &Planes, out: &mut [MaybeUninit<u8>], scratch: &mut Scratch, width: usize, height: usize) {
    matrix_rows(m, src, out, scratch, width, height);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
fn matrix_rows_sse41(m: &Matrix, src: &Planes, out: &mut [MaybeUninit<u8>], scratch: &mut Scratch, width: usize, height: usize) {
    matrix_rows(m, src, out, scratch, width, height);
}

/// What the BGR for a converted picture is made with: swscale's context and buffer, kept per thread between pictures.
#[derive(Default)]
struct Scale {
    sws: Option<*mut ff::SwsContext>,
    // What the context was made for: size, matrix and range
    made_for: Option<(i32, i32, Encoding)>,
    src: Padded,
    // The luma with a column added, for the widths swscale's ARM code leaves a pixel of
    luma: Vec<u8>,
}

impl Drop for Scale {
    fn drop(&mut self) {
        if let Some(sws) = self.sws {
            // SAFETY: made by sws_getContext, freed once
            unsafe { ff::sws_freeContext(sws) }
        }
    }
}

thread_local! {
    static SCALE: RefCell<Scale> = RefCell::new(Scale::default());
}

/// Packed I420 in this encoding as 8-bit BGR, the way PyAV's swscale does it (bilinear chroma, the stream's matrix and range). None if swscale can't.
fn bgr_of(i420: &[u8], width: usize, height: usize, enc: Encoding) -> Option<Bgr> {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    if i420.len() < width * height + 2 * cw * ch || width == 0 || height == 0 {
        return None;
    }
    let (y, rest) = i420.split_at(width * height);
    let (u, v) = rest.split_at(cw * ch);
    // swscale's ARM code skips the last pixel of each row when the width is odd and the height even, so it gets one more column
    // (a copy of the last, whose chroma sample is the same one) and the extra pixel is left out of the rows read back
    let wide = cfg!(target_arch = "aarch64") && width % 2 == 1 && height.is_multiple_of(2);
    let sw = width + wide as usize;
    SCALE.with_borrow_mut(|scale| {
        let y = if wide {
            scale.luma.clear();
            for row in y.chunks_exact(width) {
                scale.luma.extend_from_slice(row);
                scale.luma.push(row[width - 1]);
            }
            &scale.luma[..]
        } else {
            y
        };
        let (src, src_stride) = scale.src.load_i420(sw, height, (y, u, v));
        let stride = padded_stride(sw * 3);
        let mut bgr = Bgr { data: spare_bgr(), start: 0, stride };
        bgr.start = aligned_start(&mut bgr.data, stride * height);
        // SAFETY: start and the tail behind it are inside data, as aligned_start says
        let out = unsafe { bgr.data.as_mut_ptr().add(bgr.start) };
        // SAFETY: src and out are padded as the strides say, and the context is made for exactly this size and encoding before it runs
        unsafe {
            if scale.made_for != Some((sw as i32, height as i32, enc)) {
                if let Some(old) = scale.sws.take() {
                    ff::sws_freeContext(old);
                }
                scale.made_for = None;
                let sws = ff::sws_getContext(sw as i32, height as i32, ff::AVPixelFormat::AV_PIX_FMT_YUV420P, sw as i32, height as i32,
                                             ff::AVPixelFormat::AV_PIX_FMT_BGR24, ff::SwsFlags::SWS_BILINEAR as i32,
                                             ptr::null_mut(), ptr::null_mut(), ptr::null());
                if sws.is_null() {
                    return None;
                }
                scale.sws = Some(sws);
                let (to_rgb, from_rgb) = (coefficients(enc.matrix), coefficients(5));
                // The stream's own matrix and range in; the BGR end has no range to speak of
                let r = ff::sws_setColorspaceDetails(sws, to_rgb.as_ptr(), enc.full_range as i32, from_rgb.as_ptr(), 0, 0, 1 << 16, 1 << 16);
                if r < 0 {
                    return None;
                }
                scale.made_for = Some((sw as i32, height as i32, enc));
            }
            let dst = [out, ptr::null_mut(), ptr::null_mut(), ptr::null_mut()];
            let dst_stride = [stride as i32, 0, 0, 0];
            ff::sws_scale(scale.sws?, src.as_ptr(), src_stride.as_ptr(), 0, height as i32, dst.as_ptr(), dst_stride.as_ptr());
        }
        Some(bgr)
    })
}

/// The conversions for pictures that aren't already BT.601 limited, with their scratch space kept between frames.
#[derive(Default)]
pub struct Converter {
    scratch: Scratch,
    // What the matrix was worked out for
    matrix: Option<(Encoding, Matrix)>,
}

impl Converter {
    /// Packed I420 in this encoding as BT.601 limited I420, with the BGR it stands for, which is only made if something asks for
    /// it (the effects do, a plain stream never does). A BT.601 picture comes with none.
    pub fn standard(&mut self, mut i420: Vec<u8>, width: usize, height: usize, enc: Encoding) -> (Vec<u8>, Option<Arc<LazyBgr>>) {
        if is_bt601(enc.matrix) {
            if enc.full_range {
                full_to_limited(&mut i420, width, height);
            }
            return (i420, None);
        }
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        if i420.len() < width * height + 2 * cw * ch || width == 0 || height == 0 {
            log::warn!("A {width}x{height} picture in matrix {} couldn't be converted; showing it as BT.601", enc.matrix);
            return (i420, None);
        }
        if height % 2 == 1 {
            // swscale scales the chroma of a picture with an odd height in a way the rows below don't follow, so it still goes through BGR
            if let Some(bgr) = bgr_of(&i420, width, height, enc) {
                let out = bgr_to_i420(&bgr.data[bgr.start..][..bgr.stride * height], bgr.stride, width, height);
                return (out, Some(lazy_bgr(move || i420, width, height, enc)));
            }
        }
        let out = self.matrix(&Planes::packed(&i420, width, height), width, height, enc);
        (out, Some(lazy_bgr(move || i420, width, height, enc)))
    }

    /// BT.601 limited I420 straight from a picture's planes; None for one the matrix doesn't take alone (BT.601, an odd height), which goes through `standard`.
    pub fn direct(&mut self, planes: &Planes, width: usize, height: usize, enc: Encoding) -> Option<Vec<u8>> {
        if is_bt601(enc.matrix) || width == 0 || height == 0 || height % 2 == 1 {
            return None;
        }
        Some(self.matrix(planes, width, height, enc))
    }

    /// The matrix rows into a new buffer, which isn't cleared first since they write all of it.
    fn matrix(&mut self, planes: &Planes, width: usize, height: usize, enc: Encoding) -> Vec<u8> {
        let matrix = match self.matrix {
            Some((for_enc, m)) if for_enc == enc => m,
            _ => {
                let m = Matrix::new(enc);
                self.matrix = Some((enc, m));
                m
            }
        };
        let len = width * height + 2 * width.div_ceil(2) * height.div_ceil(2);
        let mut out = Vec::with_capacity(len);
        let room = &mut out.spare_capacity_mut()[..len];
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: the processor was just asked whether it has AVX2
            unsafe { matrix_rows_avx2(&matrix, planes, room, &mut self.scratch, width, height) };
            // SAFETY: matrix_rows wrote every byte of the len
            unsafe { out.set_len(len) };
            return out;
        }
        #[cfg(target_arch = "x86_64")]
        if telescope_core::image::sse41_wanted() {
            // SAFETY: the processor was just asked whether it has SSE4.1
            unsafe { matrix_rows_sse41(&matrix, planes, room, &mut self.scratch, width, height) };
            // SAFETY: matrix_rows wrote every byte of the len
            unsafe { out.set_len(len) };
            return out;
        }
        matrix_rows(&matrix, planes, room, &mut self.scratch, width, height);
        // SAFETY: matrix_rows wrote every byte of the len
        unsafe { out.set_len(len) };
        out
    }
}

/// The BGR of a picture still in its own encoding (`i420` gives it packed), made the first time it is read.
pub fn lazy_bgr(i420: impl FnOnce() -> Vec<u8> + Send + 'static, width: usize, height: usize, enc: Encoding) -> Arc<LazyBgr> {
    Arc::new(LazyBgr::new(move || {
        let i420 = i420();
        bgr_of(&i420, width, height, enc).unwrap_or_else(|| {
            log::warn!("A {width}x{height} picture in matrix {} couldn't be given colours; its effects will read it as BT.601", enc.matrix);
            Bgr::default()
        })
    }))
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

    /// What the old path made of a picture: through BGR with swscale and back with OpenCV's rule.
    fn through_bgr(i420: &[u8], w: usize, h: usize, enc: Encoding) -> Vec<u8> {
        let bgr = bgr_of(i420, w, h, enc).unwrap();
        bgr_to_i420(&bgr.data[bgr.start..][..bgr.stride * h], bgr.stride, w, h)
    }

    /// The most any byte of a differs from b's.
    fn worst(a: &[u8], b: &[u8]) -> i32 {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0)
    }

    /// The per-pixel maths in its plain form (array maps and a fold), which the rows must match to the byte.
    #[test]
    fn the_rows_give_the_bytes_of_the_plain_per_pixel_maths() {
        fn colour(gain: i16, black: i16, y: u8, offsets: [i16; 3]) -> [i16; 3] {
            let base = ((((y as i16 - black) << 7) as i32 * gain as i32) >> 16) as i16;
            offsets.map(|off| ((base + off) >> 5).clamp(0, 255))
        }
        fn weigh(bias: i16, rgb: [i16; 3], weights: [i16; 3]) -> i16 {
            let sum = rgb.iter().zip(weights).fold(bias, |a, (&c, w)| a + (((c << 7) as i32 * w as i32) >> 16) as i16);
            (sum + 32) >> 6
        }
        let mut seed = 99u64;
        let mut random = |n: usize| -> Vec<u8> {
            (0..n).map(|_| { seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (seed >> 33) as u8 }).collect()
        };
        for (w, h) in [(1usize, 2usize), (2, 2), (3, 2), (15, 4), (16, 4), (17, 4), (31, 8), (33, 8), (64, 18), (65, 18), (130, 6)] {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let source = random(w * h + 2 * cw * ch);
            for matrix in [1, 4, 7, 9] {
                for full_range in [false, true] {
                    let enc = Encoding { matrix, full_range };
                    let m = Matrix::new(enc);
                    let (gain, black) = ((m.luma >> 2) as i16, m.black as i16);
                    let q5 = |x: i32| (x >> 11) as i16;
                    let offsets = |x: usize, y: usize| {
                        let (u, v) = (source[w * h + y * cw + x] as i32 - 128, source[w * h + cw * ch + y * cw + x] as i32 - 128);
                        [q5(m.crv * v), q5(-(m.cgu * u + m.cgv * v)), q5(m.cbu * u)]
                    };
                    let mut want = vec![0u8; source.len()];
                    for y in 0..h {
                        for x in 0..w {
                            want[y * w + x] = weigh(16 << 6, colour(gain, black, source[y * w + x], offsets(x / 2, y / 2)), LUMA) as u8;
                        }
                    }
                    for y in 0..ch {
                        for x in 0..cw {
                            let rgb = colour(gain, black, source[2 * y * w + 2 * x], offsets(x, y));
                            want[w * h + y * cw + x] = weigh(128 << 6, rgb, BLUE_DIFF) as u8;
                            want[w * h + cw * ch + y * cw + x] = weigh(128 << 6, rgb, RED_DIFF) as u8;
                        }
                    }
                    let name = format!("{w}x{h} matrix {matrix} full {full_range}");
                    assert!(Converter::default().standard(source.clone(), w, h, enc).0 == want, "{name}: the converter differs");
                    // Poison in the room, to see that every byte of it is written
                    for poison in [0u8, 255] {
                        let mut plain = vec![MaybeUninit::new(poison); source.len()];
                        matrix_rows(&m, &Planes::packed(&source, w, h), &mut plain, &mut Scratch::default(), w, h);
                        // SAFETY: the room was initialised with the poison to begin with
                        let plain: Vec<u8> = plain.into_iter().map(|b| unsafe { b.assume_init() }).collect();
                        assert!(plain == want, "{name}: the plain rows differ (room poisoned with {poison})");
                    }
                }
            }
        }
    }

    /// A picture the way a camera makes one: soft gradients, a little noise, and edges whose chroma is spread over a few samples.
    fn a_natural_picture(w: usize, h: usize) -> Vec<u8> {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut noise = 12345u32;
        let mut rand = move || {
            noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (noise >> 24) as i32 % 5 - 2
        };
        let blob = |x: f64, y: f64, cx: f64, cy: f64, r: f64| (1.0 - ((x - cx).hypot(y - cy) / r - 0.8).clamp(0.0, 0.4) / 0.4).clamp(0.0, 1.0);
        let mut out = vec![0u8; w * h + 2 * cw * ch];
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f64 / w as f64, y as f64 / h as f64);
                let l = 70.0 + 110.0 * fx + 30.0 * (fy * 9.0).sin() + 40.0 * blob(fx, fy, 0.3, 0.5, 0.2);
                out[y * w + x] = (l + rand() as f64).clamp(16.0, 235.0) as u8;
            }
        }
        for y in 0..ch {
            for x in 0..cw {
                let (fx, fy) = (x as f64 / cw as f64, y as f64 / ch as f64);
                let red = blob(fx, fy, 0.3, 0.5, 0.2);
                let blue = blob(fx, fy, 0.75, 0.4, 0.15);
                out[w * h + y * cw + x] = (128.0 + 60.0 * (fx - 0.5) - 80.0 * red + 110.0 * blue + rand() as f64).clamp(16.0, 240.0) as u8;
                out[w * h + cw * ch + y * cw + x] = (128.0 + 40.0 * (fy - 0.5) + 100.0 * red - 70.0 * blue + rand() as f64).clamp(16.0, 240.0) as u8;
            }
        }
        out
    }

    #[test]
    fn a_picture_comes_with_its_bgr_made_only_when_asked() {
        let (w, h) = (34, 18);
        let source = a_picture(w, h);
        let (_, bgr) = Converter::default().standard(source.clone(), w, h, bt709());
        let bgr = bgr.expect("BT.709 comes with its BGR");
        assert!(!bgr.is_made(), "a stream with no effects never makes it");
        let want = bgr_of(&source, w, h, bt709()).unwrap();
        let got = bgr.get();
        assert_eq!((0..h).flat_map(|y| got.row(y, w).to_vec()).collect::<Vec<_>>(), (0..h).flat_map(|y| want.row(y, w).to_vec()).collect::<Vec<_>>());
        assert!(bgr.is_made());
    }

    #[test]
    fn bt601_pictures_make_no_bgr() {
        let (_, bgr) = Converter::default().standard(a_picture(34, 18), 34, 18, Encoding { matrix: 5, full_range: false });
        assert!(bgr.is_none());
    }

    #[test]
    fn the_matrix_gives_what_the_trip_through_bgr_gives_to_within_three_levels() {
        {
            let (w, h) = (385usize, 257usize);
            let source = a_natural_picture(w, h);
            let got = Converter::default().standard(source.clone(), w, h, bt709()).0;
            let old = through_bgr(&source, w, h, bt709());
            for (i, (a, b)) in got.iter().zip(&old).enumerate() {
                if (*a as i32 - *b as i32).abs() > 2 {
                    println!("DIFF at {i} luma x {} y {}: new {a} old {b}", i % w, i / w);
                }
            }
        }
        // Every matrix swscale doesn't read as BT.601 and both ranges
        for (w, h, room) in [(384, 256, 3), (385, 256, 3), (385, 257, 3)] {
            let source = a_natural_picture(w, h);
            for matrix in [1, 4, 7, 9, 10] {
                for full_range in [false, true] {
                    let enc = Encoding { matrix, full_range };
                    let got = Converter::default().standard(source.clone(), w, h, enc).0;
                    let diff = worst(&got, &through_bgr(&source, w, h, enc));
                    assert!(diff <= room, "matrix {matrix} full {full_range} {w}x{h}: {diff} levels from the old path");
                }
            }
        }
    }

    /// Flat luma with chroma that jumps from one colour to its opposite along a line: a row boundary (the sample rows from `at` down are the second colour) or a column one.
    fn a_sharp_chroma_edge(w: usize, h: usize, luma: u8, at: usize, columns: bool) -> Vec<u8> {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut out = vec![luma; w * h];
        for (first, second) in [(16u8, 240u8), (240, 16)] {
            for y in 0..ch {
                for x in 0..cw {
                    let after = if columns { x >= at } else { y >= at };
                    out.push(if after { second } else { first });
                }
            }
        }
        out
    }

    #[test]
    fn sharp_chroma_edges_come_out_as_the_old_path_has_them() {
        // Both of the matrices the direct conversion takes over from swscale (BT.709 and BT.2020), a range of lumas, and even and odd sizes (an odd height goes the old way, so those check the route)
        let (mut bad, mut top) = (0, 0);
        for (w, h, room) in [(32, 16, 3), (33, 16, 3), (64, 48, 3), (65, 48, 3), (33, 17, 3), (32, 17, 3)] {
            for matrix in [1, 9] {
                for luma in [30u8, 60, 128, 200] {
                    for columns in [false, true] {
                        for at in [1, 4, 8] {
                            let source = a_sharp_chroma_edge(w, h, luma, at, columns);
                            let enc = Encoding { matrix, full_range: false };
                            let got = Converter::default().standard(source.clone(), w, h, enc).0;
                            let diff = worst(&got, &through_bgr(&source, w, h, enc));
                            if diff > room {
                                println!("SHARP matrix {matrix} {w}x{h} luma {luma} columns {columns} at {at}: {diff} levels from the old path");
                                bad += 1;
                            }
                            top = top.max(diff);
                        }
                    }
                }
            }
        }
        println!("SHARP worst {top}, over the room {bad}");
        assert_eq!(bad, 0);
    }

    #[test]
    fn tiny_and_odd_pictures_convert_without_trouble() {
        for (w, h) in [(1, 1), (1, 5), (5, 1), (2, 2), (3, 3), (7, 2), (2, 7)] {
            let source = a_picture(w, h);
            for full_range in [false, true] {
                let (out, bgr) = Converter::default().standard(source.clone(), w, h, Encoding { matrix: 1, full_range });
                assert_eq!(out.len(), source.len(), "{w}x{h}");
                assert!(bgr.is_some());
            }
        }
    }

    #[test]
    fn flat_colours_at_the_ends_of_the_range_land_where_the_old_path_puts_them() {
        let (w, h) = (32, 16);
        let (cw, ch) = (16, 8);
        for (y, u, v) in [(16u8, 128u8, 128u8), (235, 128, 128), (128, 128, 128), (81, 90, 240), (180, 170, 20)] {
            let mut source = vec![y; w * h];
            source.extend(vec![u; cw * ch]);
            source.extend(vec![v; cw * ch]);
            let got = Converter::default().standard(source.clone(), w, h, bt709()).0;
            let diff = worst(&got, &through_bgr(&source, w, h, bt709()));
            assert!(diff <= 3, "{y} {u} {v}: {diff} levels from the old path");
        }
    }

    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture how_far_real_frames_are_from_the_old_path
    /// Raw yuv420p frames named in R6_FRAMES (as WxH:path, comma separated), all BT.709 limited.
    #[test]
    #[ignore = "reads frames from the files R6_FRAMES names"]
    fn how_far_real_frames_are_from_the_old_path() {
        let list = std::env::var("R6_FRAMES").unwrap();
        for item in list.split(',') {
            let (size, path) = item.split_once(':').unwrap();
            let (w, h) = size.split_once('x').unwrap();
            let (w, h): (usize, usize) = (w.parse().unwrap(), h.parse().unwrap());
            let source = std::fs::read(path).unwrap();
            let got = Converter::default().standard(source.clone(), w, h, bt709()).0;
            let old = through_bgr(&source, w, h, bt709());
            if let Ok(dir) = std::env::var("R6_DUMP") {
                let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
                std::fs::write(format!("{dir}/{name}.old"), &old).unwrap();
                std::fs::write(format!("{dir}/{name}.new"), &got).unwrap();
            }
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let planes = [(0, w * h, "Y"), (w * h, cw * ch, "U"), (w * h + cw * ch, cw * ch, "V")];
            for (from, len, name) in planes {
                let (a, b) = (&got[from..from + len], &old[from..from + len]);
                let mut hist = [0usize; 8];
                for (a, b) in a.iter().zip(b) {
                    hist[((*a as i32 - *b as i32).unsigned_abs() as usize).min(7)] += 1;
                }
                println!("{item} {name}: worst {}, differences 0..7+ {hist:?}", worst(a, b));
            }
        }
    }

    /// cargo test --release -p telescope-media --lib -- --ignored --nocapture what_the_conversion_costs (CARGO_PROFILE_RELEASE_LTO=off for a build without link-time optimisation)
    #[test]
    #[ignore = "a measurement"]
    #[cfg(target_arch = "x86_64")]
    fn what_the_conversion_costs() {
        use std::time::Instant;
        let med = |mut v: Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
            let pic = a_natural_picture(w, h);
            let mut conv = Converter::default();
            let (mut old, mut new, mut with_bgr) = (Vec::new(), Vec::new(), Vec::new());
            for _ in 0..20 {
                let t = Instant::now();
                let out = through_bgr(&pic, w, h, bt709());
                old.push(t.elapsed().as_secs_f64() * 1e3);
                drop(out);
                let copy = pic.clone();
                let t = Instant::now();
                let (out, lazy) = conv.standard(copy, w, h, bt709());
                new.push(t.elapsed().as_secs_f64() * 1e3);
                drop(out);
                let t = Instant::now();
                let _ = lazy.unwrap().get();
                with_bgr.push(t.elapsed().as_secs_f64() * 1e3);
            }
            let m = Matrix::new(bt709());
            let mut out = vec![MaybeUninit::new(0u8); pic.len()];
            let mut offsets = Scratch::default();
            let mut rows_only = Vec::new();
            for _ in 0..20 {
                let t = Instant::now();
                // SAFETY: the bench runs where AVX2 is
                unsafe { matrix_rows_avx2(&m, &Planes::packed(&pic, w, h), &mut out, &mut offsets, w, h) };
                rows_only.push(t.elapsed().as_secs_f64() * 1e3);
            }
            println!("{w}x{h}: the rows alone on a buffer already touched {:.2} ms", med(rows_only));
            println!("{w}x{h}: old path (BGR made and back) {:.2} ms, matrix standard() {:.2} ms, the BGR made later when asked {:.2} ms", med(old), med(new), med(with_bgr));
        }
    }
}
