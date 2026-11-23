//! Pixel work the cameras need: fitting a frame to the size an app picked, and the formats each camera takes.

use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::image::resize_linear_into;

/// frame scaled to fit width x height, keeping its shape, with black bars as needed (the same frame if it fits).
pub fn fit_frame(frame: &Frame, width: u32, height: u32) -> Frame {
    if (frame.width, frame.height) == (width, height) || frame.format != PixelFormat::I420 {
        return frame.clone();
    }
    let scale = (width as f64 / frame.width as f64).min(height as f64 / frame.height as f64);
    let (nw, nh) = ((frame.width as f64 * scale) as u32, (frame.height as f64 * scale) as u32);
    let (x, y) = ((width - nw.min(width)) / 2, (height - nh.min(height)) / 2);
    let mut out = vec![0u8; PixelFormat::I420.size(width, height)];
    let (sy, su, sv) = frame.i420_planes().unwrap();
    let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
    let (oy, rest) = out.split_at_mut((width * height) as usize);
    let (ou, ov) = rest.split_at_mut(cw * ch);
    ou.fill(128);
    ov.fill(128);
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    resize_linear_into(sy, fw, fh, 1, oy, width as usize, (x as usize, y as usize, nw as usize, nh as usize));
    let (fcw, fch) = (fw.div_ceil(2), fh.div_ceil(2));
    let chroma = ((x / 2) as usize, (y / 2) as usize, nw.div_ceil(2) as usize, nh.div_ceil(2) as usize);
    resize_linear_into(su, fcw, fch, 1, ou, cw, chroma);
    resize_linear_into(sv, fcw, fch, 1, ov, cw, chroma);
    let mut fitted = Frame::new(width, height, PixelFormat::I420, out);
    fitted.number = frame.number;
    fitted.time_ns = frame.time_ns;
    fitted.capture_ns = frame.capture_ns;
    fitted
}

/// Writes an I420 frame as NV12 into out (Y plane, then U and V interleaved), which must hold w * h * 3 / 2 bytes.
pub fn write_nv12(frame: &Frame, out: &mut [u8]) {
    let Some((y, u, v)) = frame.i420_planes() else { return };
    out[..y.len()].copy_from_slice(y);
    for (i, (a, b)) in u.iter().zip(v).enumerate() {
        out[y.len() + 2 * i] = *a;
        out[y.len() + 2 * i + 1] = *b;
    }
}

/// BT.601 limited range, as OpenCV's YUV I420 conversions use.
#[cfg(test)]
pub(crate) fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = (y as f32 - 16.0) * 1.164;
    let (d, e) = (u as f32 - 128.0, v as f32 - 128.0);
    let clamp = |x: f32| x.round().clamp(0.0, 255.0) as u8;
    [clamp(c + 1.596 * e), clamp(c - 0.392 * d - 0.813 * e), clamp(c + 2.017 * d)]
}

/// x.round().clamp(0.0, 255.0) as u8 for a value that can't be NaN, without the library call: the add and subtract of 2^23 rounds
/// to even, and a tie that went down is moved up to round away from zero.
#[inline(always)]
fn round_byte(x: f32) -> u8 {
    let x = x.clamp(0.0, 255.0);
    let t = (x + 8_388_608.0) - 8_388_608.0;
    (if x - t == 0.5 { t + 1.0 } else { t }) as u8
}

/// Every product the conversions below multiply a byte by, made with the same f32 operations as the formulas they stand for.
struct Terms {
    /// (y - 16) * 1.164
    luma: [f32; 256],
    /// 1.596 * (v - 128), 0.813 * (v - 128)
    v_red: [f32; 256],
    v_green: [f32; 256],
    /// 0.392 * (u - 128), 2.017 * (u - 128)
    u_green: [f32; 256],
    u_blue: [f32; 256],
    /// 0.257, 0.504 and 0.098 times a channel, for luma
    y_of: [[f32; 256]; 3],
    /// 0.148, 0.291 and 0.439 times a channel, for U
    u_of: [[f32; 256]; 3],
    /// 0.439, 0.368 and 0.071 times a channel, for V
    v_of: [[f32; 256]; 3],
}

fn terms() -> &'static Terms {
    static TERMS: std::sync::OnceLock<Terms> = std::sync::OnceLock::new();
    TERMS.get_or_init(|| {
        let by = |f: fn(f32) -> f32| std::array::from_fn(|i| f(i as f32));
        let weights = |a: f32, b: f32, c: f32| [by(|x| x).map(|x| x * a), by(|x| x).map(|x| x * b), by(|x| x).map(|x| x * c)];
        Terms {
            luma: by(|x| (x - 16.0) * 1.164),
            v_red: by(|x| 1.596 * (x - 128.0)),
            v_green: by(|x| 0.813 * (x - 128.0)),
            u_green: by(|x| 0.392 * (x - 128.0)),
            u_blue: by(|x| 2.017 * (x - 128.0)),
            y_of: weights(0.257, 0.504, 0.098),
            u_of: weights(0.148, 0.291, 0.439),
            v_of: weights(0.439, 0.368, 0.071),
        }
    })
}

/// yuv_row for a whole row, the first 32 pixel blocks of it on AVX2 or SSE4.1 (within 1 of yuv_to_rgb) and the rest, or all of it on a processor with neither, the exact way.
fn convert_row<const BGRA: bool>(t: &Terms, y: &[u8], u: &[u8], v: &[u8], out: &mut [u8]) {
    let done = if BGRA { telescope_core::yuv::row::<{ telescope_core::yuv::BGRA }>(y, u, v, false, out) } else { telescope_core::yuv::row::<{ telescope_core::yuv::RGBA }>(y, u, v, false, out) };
    yuv_row::<BGRA>(t, &y[done..], &u[done / 2..], &v[done / 2..], &mut out[done * 4..]);
}

/// The RGB yuv_to_rgb makes of a run of luma, each pair of pixels sharing its chroma sample; out is RGBA, or BGRA when BGRA is set.
fn yuv_row<const BGRA: bool>(t: &Terms, y: &[u8], u: &[u8], v: &[u8], out: &mut [u8]) {
    for ((ys, os), (u, v)) in y.chunks(2).zip(out.chunks_mut(8)).zip(u.iter().zip(v)) {
        let (red, green_v, green_u, blue) = (t.v_red[*v as usize], t.v_green[*v as usize], t.u_green[*u as usize], t.u_blue[*u as usize]);
        for (y, o) in ys.iter().zip(os.as_chunks_mut::<4>().0.iter_mut()) {
            let c = t.luma[*y as usize];
            let (r, g, b) = (round_byte(c + red), round_byte(c - green_u - green_v), round_byte(c + blue));
            o.copy_from_slice(&if BGRA { [b, g, r, 255] } else { [r, g, b, 255] });
        }
    }
}

/// An I420 frame as RGBA bytes, bottom row first (UnityCapture takes Unity's upside-down textures).
pub fn i420_to_rgba_flipped(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    i420_to_rgba_flipped_into(frame, &mut out);
    out
}

/// i420_to_rgba_flipped into out, which keeps its allocation from one call to the next (black if the frame isn't I420).
pub fn i420_to_rgba_flipped_into(frame: &Frame, out: &mut Vec<u8>) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let Some((py, pu, pv)) = frame.i420_planes() else {
        out.clear();
        out.resize(w * h * 4, 0);
        return;
    };
    out.resize(w * h * 4, 0);
    if w == 0 {
        return;
    }
    let (t, cw) = (terms(), w.div_ceil(2));
    telescope_core::image::over_rows(out, w * 4, |at, line| {
        let row = h - 1 - at;
        convert_row::<false>(t, &py[row * w..][..w], &pu[(row / 2) * cw..][..cw], &pv[(row / 2) * cw..][..cw], line);
    });
}

/// An I420 frame as BGRA bytes with the alpha left opaque, top row first, into out which keeps its allocation from one call to the next (black if the frame isn't I420).
pub fn i420_to_bgra_into(frame: &Frame, out: &mut Vec<u8>) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let Some((py, pu, pv)) = frame.i420_planes() else {
        out.clear();
        out.resize(w * h * 4, 0);
        return;
    };
    // Every byte is written below, so a buffer of the right size isn't cleared first
    out.resize(w * h * 4, 0);
    if w == 0 {
        return;
    }
    let (t, cw) = (terms(), w.div_ceil(2));
    telescope_core::image::over_rows(out, w * 4, |row, line| {
        convert_row::<true>(t, &py[row * w..][..w], &pu[(row / 2) * cw..][..cw], &pv[(row / 2) * cw..][..cw], line);
    });
}

/// An I420 frame as BGR bytes, top row first (an odd-sized v4l2 camera, which I420 can't be).
pub fn i420_to_bgr(frame: &Frame) -> Vec<u8> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut out = vec![0u8; w * h * 3];
    let Some((py, pu, pv)) = frame.i420_planes() else { return out };
    if w == 0 {
        return out;
    }
    let (t, cw) = (terms(), w.div_ceil(2));
    telescope_core::image::over_bands(&mut out, w * 3, |first, part| {
        let mut bgra = vec![0u8; w * 4];
        for (i, line) in part.chunks_exact_mut(w * 3).enumerate() {
            let row = first + i;
            convert_row::<true>(t, &py[row * w..][..w], &pu[(row / 2) * cw..][..cw], &pv[(row / 2) * cw..][..cw], &mut bgra);
            line.as_chunks_mut::<3>().0.iter_mut().zip(bgra.as_chunks::<4>().0).for_each(|(o, p)| o.copy_from_slice(&p[..3]));
        }
    });
    out
}

/// RGBA (or RGB with stride 3) pixels as an I420 frame; odd sizes round the chroma up. mirror flips left to right.
pub fn rgb_to_i420(pixels: &[u8], width: u32, height: u32, channels: usize, mirror: bool) -> Frame {
    let mut out = Vec::new();
    rgb_to_i420_into(pixels, width, height, channels, mirror, &mut out);
    Frame::new(width, height, PixelFormat::I420, out)
}

/// BGRA (or BGRx) pixels as an I420 frame, read in that order without swapping them first.
pub fn bgra_to_i420(pixels: &[u8], width: u32, height: u32) -> Frame {
    let mut out = Vec::new();
    to_i420_into(pixels, width, height, 4, false, [2, 1, 0], &mut out);
    Frame::new(width, height, PixelFormat::I420, out)
}

/// The I420 planes of rgb_to_i420 in out, which keeps its allocation from one call to the next.
pub fn rgb_to_i420_into(pixels: &[u8], width: u32, height: u32, channels: usize, mirror: bool, out: &mut Vec<u8>) {
    to_i420_into(pixels, width, height, channels, mirror, [0, 1, 2], out);
}

/// rgb_to_i420_into with the red, green and blue bytes at `order` within each pixel.
fn to_i420_into(pixels: &[u8], width: u32, height: u32, channels: usize, mirror: bool, order: [usize; 3], out: &mut Vec<u8>) {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    // Every byte is written below when w and h are non-zero, so skip zeroing a reused buffer.
    out.resize(w * h + 2 * cw * ch, 0);
    if w == 0 || h == 0 {
        return;
    }
    let (luma, chroma) = out.split_at_mut(w * h);
    let (us, vs) = chroma.split_at_mut(cw * ch);
    // Bands of row pairs, each with its own part of the three planes
    let per_band = if w * h < 512 * 1024 { ch } else { ch.div_ceil(8) };
    let mut bands: Vec<_> = luma.chunks_mut(2 * per_band * w).zip(us.chunks_mut(per_band * cw)).zip(vs.chunks_mut(per_band * cw)).map(|((l, u), v)| (l, u, v)).collect();
    telescope_core::workers::each_part(&mut bands, |band, (luma, us, vs)| {
        let first = band * per_band;
        rgb_band_to_i420(pixels, (w, h), channels, order, mirror, first, luma, us, vs);
    });
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rgb4_pair_avx2<const BGRA: bool>(top_px: &[u8], bot_px: &[u8], top_y: &mut [u8], bot_y: &mut [u8], urow: &mut [u8], vrow: &mut [u8], blocks: usize) {
    use std::arch::x86_64::*;
    let mask_ff = _mm256_set1_epi32(0xFF);
    let zero = _mm256_setzero_ps();
    let half = _mm256_set1_ps(0.5);
    let one = _mm256_set1_ps(1.0);
    let quarter = _mm256_set1_ps(0.25);
    let c16 = _mm256_set1_ps(16.0);
    let c128 = _mm256_set1_ps(128.0);
    let c255 = _mm256_set1_ps(255.0);
    let magic = _mm256_set1_ps(8_388_608.0);
    let (ky_r, ky_g, ky_b) = (_mm256_set1_ps(0.257), _mm256_set1_ps(0.504), _mm256_set1_ps(0.098));
    let (ku_r, ku_g, ku_b) = (_mm256_set1_ps(0.148), _mm256_set1_ps(0.291), _mm256_set1_ps(0.439));
    let (kv_r, kv_g, kv_b) = (_mm256_set1_ps(0.439), _mm256_set1_ps(0.368), _mm256_set1_ps(0.071));

    let round_i32 = |x: __m256| -> __m256i {
        let x = _mm256_min_ps(_mm256_max_ps(x, zero), c255);
        let t = _mm256_sub_ps(_mm256_add_ps(x, magic), magic);
        let tie = _mm256_cmp_ps::<_CMP_EQ_OQ>(_mm256_sub_ps(x, t), half);
        _mm256_cvttps_epi32(_mm256_add_ps(t, _mm256_and_ps(tie, one)))
    };
    let pack16 = |lo: __m256i, hi: __m256i| -> __m128i {
        let p16 = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi32(lo, hi));
        let p8 = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi16(p16, p16));
        _mm256_castsi256_si128(p8)
    };
    let pack8 = |v: __m256i| -> __m128i {
        let ordered = _mm256_permute4x64_epi64::<0xD8>(v);
        let p16 = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi32(ordered, ordered));
        let lo = _mm256_castsi256_si128(p16);
        _mm_packus_epi16(lo, lo)
    };
    let load_yuv = |ptr: *const u8| -> (__m256i, __m256, __m256) {
        let px = _mm256_loadu_si256(ptr.cast());
        let c0 = _mm256_cvtepi32_ps(_mm256_and_si256(px, mask_ff));
        let g = _mm256_cvtepi32_ps(_mm256_and_si256(_mm256_srli_epi32::<8>(px), mask_ff));
        let c2 = _mm256_cvtepi32_ps(_mm256_and_si256(_mm256_srli_epi32::<16>(px), mask_ff));
        let (r, b) = if BGRA { (c2, c0) } else { (c0, c2) };
        let y = _mm256_add_ps(_mm256_add_ps(_mm256_add_ps(c16, _mm256_mul_ps(r, ky_r)), _mm256_mul_ps(g, ky_g)), _mm256_mul_ps(b, ky_b));
        let u = _mm256_add_ps(_mm256_sub_ps(_mm256_sub_ps(c128, _mm256_mul_ps(r, ku_r)), _mm256_mul_ps(g, ku_g)), _mm256_mul_ps(b, ku_b));
        let v = _mm256_sub_ps(_mm256_sub_ps(_mm256_add_ps(c128, _mm256_mul_ps(r, kv_r)), _mm256_mul_ps(g, kv_g)), _mm256_mul_ps(b, kv_b));
        (round_i32(y), u, v)
    };

    for b in 0..blocks {
        let x = b * 16;
        let (yt0, ut0, vt0) = load_yuv(top_px.as_ptr().add(x * 4));
        let (yt1, ut1, vt1) = load_yuv(top_px.as_ptr().add((x + 8) * 4));
        let (yb0, ub0, vb0) = load_yuv(bot_px.as_ptr().add(x * 4));
        let (yb1, ub1, vb1) = load_yuv(bot_px.as_ptr().add((x + 8) * 4));
        _mm_storeu_si128(top_y.as_mut_ptr().add(x).cast(), pack16(yt0, yt1));
        _mm_storeu_si128(bot_y.as_mut_ptr().add(x).cast(), pack16(yb0, yb1));
        let u0 = _mm256_shuffle_ps::<0x88>(ut0, ut1);
        let u1 = _mm256_shuffle_ps::<0xDD>(ut0, ut1);
        let u2 = _mm256_shuffle_ps::<0x88>(ub0, ub1);
        let u3 = _mm256_shuffle_ps::<0xDD>(ub0, ub1);
        let u_avg = _mm256_mul_ps(_mm256_add_ps(_mm256_add_ps(_mm256_add_ps(u0, u1), u2), u3), quarter);
        _mm_storel_epi64(urow.as_mut_ptr().add(b * 8).cast(), pack8(round_i32(u_avg)));
        let v0 = _mm256_shuffle_ps::<0x88>(vt0, vt1);
        let v1 = _mm256_shuffle_ps::<0xDD>(vt0, vt1);
        let v2 = _mm256_shuffle_ps::<0x88>(vb0, vb1);
        let v3 = _mm256_shuffle_ps::<0xDD>(vb0, vb1);
        let v_avg = _mm256_mul_ps(_mm256_add_ps(_mm256_add_ps(_mm256_add_ps(v0, v1), v2), v3), quarter);
        _mm_storel_epi64(vrow.as_mut_ptr().add(b * 8).cast(), pack8(round_i32(v_avg)));
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn rgb4_pair_sse41<const BGRA: bool>(top_px: &[u8], bot_px: &[u8], top_y: &mut [u8], bot_y: &mut [u8], urow: &mut [u8], vrow: &mut [u8], blocks: usize) {
    use std::arch::x86_64::*;
    let mask_ff = _mm_set1_epi32(0xFF);
    let zero = _mm_setzero_ps();
    let half = _mm_set1_ps(0.5);
    let one = _mm_set1_ps(1.0);
    let quarter = _mm_set1_ps(0.25);
    let c16 = _mm_set1_ps(16.0);
    let c128 = _mm_set1_ps(128.0);
    let c255 = _mm_set1_ps(255.0);
    let magic = _mm_set1_ps(8_388_608.0);
    let (ky_r, ky_g, ky_b) = (_mm_set1_ps(0.257), _mm_set1_ps(0.504), _mm_set1_ps(0.098));
    let (ku_r, ku_g, ku_b) = (_mm_set1_ps(0.148), _mm_set1_ps(0.291), _mm_set1_ps(0.439));
    let (kv_r, kv_g, kv_b) = (_mm_set1_ps(0.439), _mm_set1_ps(0.368), _mm_set1_ps(0.071));

    let round_i32 = |x: __m128| -> __m128i {
        let x = _mm_min_ps(_mm_max_ps(x, zero), c255);
        let t = _mm_sub_ps(_mm_add_ps(x, magic), magic);
        let tie = _mm_cmpeq_ps(_mm_sub_ps(x, t), half);
        _mm_cvttps_epi32(_mm_add_ps(t, _mm_and_ps(tie, one)))
    };
    // Four pixels' luma as bytes in the low 4 bytes, and their U and V still as floats
    let load_yuv = |ptr: *const u8| -> (__m128i, __m128, __m128) {
        let px = _mm_loadu_si128(ptr.cast());
        let c0 = _mm_cvtepi32_ps(_mm_and_si128(px, mask_ff));
        let g = _mm_cvtepi32_ps(_mm_and_si128(_mm_srli_epi32::<8>(px), mask_ff));
        let c2 = _mm_cvtepi32_ps(_mm_and_si128(_mm_srli_epi32::<16>(px), mask_ff));
        let (r, b) = if BGRA { (c2, c0) } else { (c0, c2) };
        let y = _mm_add_ps(_mm_add_ps(_mm_add_ps(c16, _mm_mul_ps(r, ky_r)), _mm_mul_ps(g, ky_g)), _mm_mul_ps(b, ky_b));
        let u = _mm_add_ps(_mm_sub_ps(_mm_sub_ps(c128, _mm_mul_ps(r, ku_r)), _mm_mul_ps(g, ku_g)), _mm_mul_ps(b, ku_b));
        let v = _mm_sub_ps(_mm_sub_ps(_mm_add_ps(c128, _mm_mul_ps(r, kv_r)), _mm_mul_ps(g, kv_g)), _mm_mul_ps(b, kv_b));
        (round_i32(y), u, v)
    };
    // Two vectors of four luma values (as i32) to 8 bytes
    let pack8 = |lo: __m128i, hi: __m128i| -> __m128i { _mm_packus_epi16(_mm_packus_epi32(lo, hi), _mm_setzero_si128()) };
    // Four chroma values from the four pixels around each: the even and odd pixels of the top two vectors and of the bottom two
    let average = |t0: __m128, t1: __m128, b0: __m128, b1: __m128| -> i32 {
        let a = _mm_shuffle_ps::<0x88>(t0, t1);
        let c = _mm_shuffle_ps::<0xDD>(t0, t1);
        let d = _mm_shuffle_ps::<0x88>(b0, b1);
        let e = _mm_shuffle_ps::<0xDD>(b0, b1);
        let avg = _mm_mul_ps(_mm_add_ps(_mm_add_ps(_mm_add_ps(a, c), d), e), quarter);
        let p = _mm_packus_epi16(_mm_packus_epi32(round_i32(avg), _mm_setzero_si128()), _mm_setzero_si128());
        _mm_cvtsi128_si32(p)
    };

    for b in 0..blocks * 2 {
        let x = b * 8;
        let (yt0, ut0, vt0) = load_yuv(top_px.as_ptr().add(x * 4));
        let (yt1, ut1, vt1) = load_yuv(top_px.as_ptr().add((x + 4) * 4));
        let (yb0, ub0, vb0) = load_yuv(bot_px.as_ptr().add(x * 4));
        let (yb1, ub1, vb1) = load_yuv(bot_px.as_ptr().add((x + 4) * 4));
        _mm_storel_epi64(top_y.as_mut_ptr().add(x).cast(), pack8(yt0, yt1));
        _mm_storel_epi64(bot_y.as_mut_ptr().add(x).cast(), pack8(yb0, yb1));
        urow.as_mut_ptr().add(b * 4).cast::<i32>().write_unaligned(average(ut0, ut1, ub0, ub1));
        vrow.as_mut_ptr().add(b * 4).cast::<i32>().write_unaligned(average(vt0, vt1, vb0, vb1));
    }
}

/// The row pairs of rgb_to_i420 from `first` on that fit in the planes' parts (each starting at that row pair).
#[allow(clippy::too_many_arguments)]
fn rgb_band_to_i420(pixels: &[u8], (w, h): (usize, usize), channels: usize, [ro, go, bo]: [usize; 3], mirror: bool, first: usize, luma: &mut [u8], us: &mut [u8], vs: &mut [u8]) {
    let (t, cw) = (terms(), w.div_ceil(2));
    // Decided once, so scalar mode switching on mid-call can't land on the SSE4.1 loop
    #[cfg(target_arch = "x86_64")]
    let wide = telescope_core::image::avx2_wanted();
    #[cfg(target_arch = "x86_64")]
    let simd_blocks = if channels == 4 && !mirror && ([ro, go, bo] == [0, 1, 2] || [ro, go, bo] == [2, 1, 0]) && (wide || telescope_core::image::sse41_wanted()) { w / 16 } else { 0 };
    #[cfg(not(target_arch = "x86_64"))]
    let simd_blocks = 0usize;
    // Each pixel's U and V, and its luma written, from the pixel at output column x of row y (luma starting at row `first` * 2)
    let yuv = |x: usize, y: usize, luma: &mut [u8]| {
        let i = (y * w + if mirror { w - 1 - x } else { x }) * channels;
        let (r, g, b) = (pixels[i + ro] as usize, pixels[i + go] as usize, pixels[i + bo] as usize);
        luma[x] = round_byte(((16.0 + t.y_of[0][r]) + t.y_of[1][g]) + t.y_of[2][b]);
        let u = ((128.0 - t.u_of[0][r]) - t.u_of[1][g]) + t.u_of[2][b];
        let v = ((128.0 + t.v_of[0][r]) - t.v_of[1][g]) - t.v_of[2][b];
        (u, v)
    };
    for k in 0..us.len() / cw {
        let cy = first + k;
        let (top, bottom) = luma[2 * k * w..].split_at_mut(w);
        let bottom = if 2 * cy + 1 < h { Some(&mut bottom[..w]) } else { None };
        let (urow, vrow) = (&mut us[k * cw..][..cw], &mut vs[k * cw..][..cw]);
        match bottom {
            Some(bottom) => {
                let mut cx0 = 0;
                #[cfg(target_arch = "x86_64")]
                if simd_blocks > 0 {
                    let top_px = &pixels[(2 * cy) * w * 4..][..w * 4];
                    let bot_px = &pixels[(2 * cy + 1) * w * 4..][..w * 4];
                    // SAFETY: the processor was asked for AVX2 or SSE4.1 and simd_blocks * 16 <= w keeps every load and store in bounds.
                    unsafe {
                        if wide {
                            if ro == 2 {
                                rgb4_pair_avx2::<true>(top_px, bot_px, top, bottom, urow, vrow, simd_blocks);
                            } else {
                                rgb4_pair_avx2::<false>(top_px, bot_px, top, bottom, urow, vrow, simd_blocks);
                            }
                        } else if ro == 2 {
                            rgb4_pair_sse41::<true>(top_px, bot_px, top, bottom, urow, vrow, simd_blocks);
                        } else {
                            rgb4_pair_sse41::<false>(top_px, bot_px, top, bottom, urow, vrow, simd_blocks);
                        }
                    }
                    cx0 = simd_blocks * 8;
                }
                for cx in cx0..w / 2 {
                    let (x, y) = (2 * cx, 2 * cy);
                    let (u0, v0) = yuv(x, y, top);
                    let (u1, v1) = yuv(x + 1, y, top);
                    let (u2, v2) = yuv(x, y + 1, bottom);
                    let (u3, v3) = yuv(x + 1, y + 1, bottom);
                    urow[cx] = round_byte((((0.0 + u0) + u1) + u2 + u3) / 4.0);
                    vrow[cx] = round_byte((((0.0 + v0) + v1) + v2 + v3) / 4.0);
                }
                if w % 2 == 1 {
                    let (x, y) = (w - 1, 2 * cy);
                    let ((u0, v0), (u1, v1)) = (yuv(x, y, top), yuv(x, y + 1, bottom));
                    urow[cw - 1] = round_byte(((0.0 + u0) + u1) / 2.0);
                    vrow[cw - 1] = round_byte(((0.0 + v0) + v1) / 2.0);
                }
            }
            None => {
                for cx in 0..w / 2 {
                    let (u0, v0) = yuv(2 * cx, 2 * cy, top);
                    let (u1, v1) = yuv(2 * cx + 1, 2 * cy, top);
                    urow[cx] = round_byte(((0.0 + u0) + u1) / 2.0);
                    vrow[cx] = round_byte(((0.0 + v0) + v1) / 2.0);
                }
                if w % 2 == 1 {
                    let (u0, v0) = yuv(w - 1, 2 * cy, top);
                    urow[cw - 1] = round_byte((0.0 + u0) / 1.0);
                    vrow[cw - 1] = round_byte((0.0 + v0) / 1.0);
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn grey(w: u32, h: u32, y: u8) -> Frame {
    let mut data = vec![y; (w * h) as usize];
    data.extend(vec![128; 2 * (w.div_ceil(2) * h.div_ceil(2)) as usize]);
    Frame::new(w, h, PixelFormat::I420, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_converts_like_the_same_pixels_in_rgba() {
        let (w, h) = (37u32, 21u32);
        let bgra: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7) as u8, (i * 13) as u8, (i * 29) as u8, 255]).collect();
        let rgba: Vec<u8> = bgra.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], 255]).collect();
        assert_eq!(bgra_to_i420(&bgra, w, h).data, rgb_to_i420(&rgba, w, h, 4, false).data);
    }

    #[test]
    fn bgra_is_the_flipped_rgba_with_red_and_blue_swapped_even_into_a_dirty_buffer() {
        for (w, h) in [(37usize, 21usize), (4, 2), (1, 1)] {
            let cw = w.div_ceil(2);
            let data: Vec<u8> = (0..w * h + 2 * cw * h.div_ceil(2)).map(|i| (i * 37 + i / 7) as u8).collect();
            let f = Frame::new(w as u32, h as u32, PixelFormat::I420, data);
            let rgba = i420_to_rgba_flipped(&f);
            let want: Vec<u8> = (0..h).flat_map(|row| rgba[(h - 1 - row) * w * 4..][..w * 4].as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect::<Vec<_>>()).collect();
            for mut out in [Vec::new(), vec![9u8; w * h * 4], vec![9u8; 3]] {
                i420_to_bgra_into(&f, &mut out);
                assert_eq!(out, want, "{w}x{h}");
            }
        }
    }

    #[test]
    fn a_frame_that_fits_is_the_same_frame() {
        let f = grey(8, 4, 50);
        assert!(std::sync::Arc::ptr_eq(&fit_frame(&f, 8, 4).data, &f.data));
    }

    #[test]
    fn a_wider_frame_gets_bars_above_and_below() {
        let fitted = fit_frame(&grey(8, 2, 200), 8, 8);
        let (y, _, _) = fitted.i420_planes().unwrap();
        assert_eq!(y[0], 0);
        assert_eq!(y[4 * 8], 200);
    }

    #[test]
    fn frames_reach_an_i420_camera_converted() {
        let out = rgb_to_i420(&[255; 4 * 2 * 4], 4, 2, 4, false);
        assert_eq!(out.data.len(), 4 * 2 * 3 / 2);
        assert!(out.data[..8].iter().all(|y| *y == 235), "white's luma in the video range: {:?}", &out.data[..8]);
    }

    #[test]
    fn nv12_interleaves_u_and_v() {
        let mut f = grey(2, 2, 10);
        let d = f.data_mut();
        d[4] = 1;
        d[5] = 2;
        let mut out = vec![0; 6];
        write_nv12(&f, &mut out);
        assert_eq!(out, [10, 10, 10, 10, 1, 2]);
    }

    #[test]
    fn rgb_and_back_keeps_a_colour_close() {
        let px = [200u8, 40, 90, 255].repeat(4);
        let f = rgb_to_i420(&px, 2, 2, 4, false);
        let rgba = i420_to_rgba_flipped(&f);
        for (a, b) in rgba[..3].iter().zip([200u8, 40, 90]) {
            assert!((*a as i32 - b as i32).abs() <= 3, "{rgba:?}");
        }
    }

    #[test]
    fn mirror_flips_left_to_right() {
        let mut px = vec![0u8; 2 * 3];
        px[3..6].copy_from_slice(&[255, 255, 255]);
        let f = rgb_to_i420(&px, 2, 1, 3, true);
        let (y, _, _) = f.i420_planes().unwrap();
        assert!(y[0] > y[1]);
    }

    /// The conversions as they were before the table versions, to hold the new ones to.
    mod reference {
        use super::*;

        fn rgb_to_yuv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
            (16.0 + 0.257 * r + 0.504 * g + 0.098 * b, 128.0 - 0.148 * r - 0.291 * g + 0.439 * b, 128.0 + 0.439 * r - 0.368 * g - 0.071 * b)
        }

        pub fn i420_to_rgba_flipped(frame: &Frame) -> Vec<u8> {
            let (w, h) = (frame.width as usize, frame.height as usize);
            let mut out = vec![0u8; w * h * 4];
            let Some((py, pu, pv)) = frame.i420_planes() else { return out };
            let cw = w.div_ceil(2);
            for row in 0..h {
                let dst = &mut out[(h - 1 - row) * w * 4..][..w * 4];
                for col in 0..w {
                    let ci = (row / 2) * cw + col / 2;
                    let [r, g, b] = yuv_to_rgb(py[row * w + col], pu[ci], pv[ci]);
                    dst[col * 4..col * 4 + 4].copy_from_slice(&[r, g, b, 255]);
                }
            }
            out
        }

        /// i420_to_bgra_into as it was before the vector version: the table row loop, a band of rows to each worker.
        pub fn i420_to_bgra_into_tables(frame: &Frame, out: &mut Vec<u8>) {
            let (w, h) = (frame.width as usize, frame.height as usize);
            let Some((py, pu, pv)) = frame.i420_planes() else { return };
            out.resize(w * h * 4, 0);
            let (t, cw) = (terms(), w.div_ceil(2));
            telescope_core::image::over_rows(out, w * 4, |row, line| {
                yuv_row::<true>(t, &py[row * w..][..w], &pu[(row / 2) * cw..][..cw], &pv[(row / 2) * cw..][..cw], line);
            });
        }

        /// i420_to_bgra_into as the float formula says it, pixel by pixel.
        pub fn i420_to_bgra(frame: &Frame) -> Vec<u8> {
            let (w, h) = (frame.width as usize, frame.height as usize);
            let rgba = i420_to_rgba_flipped(frame);
            (0..h).flat_map(|row| rgba[(h - 1 - row) * w * 4..][..w * 4].as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect::<Vec<_>>()).collect()
        }

        /// i420_to_bgr as the float formula says it, pixel by pixel.
        pub fn i420_to_bgr(frame: &Frame) -> Vec<u8> {
            let (w, h) = (frame.width as usize, frame.height as usize);
            let mut out = vec![0u8; w * h * 3];
            let Some((py, pu, pv)) = frame.i420_planes() else { return out };
            let cw = w.div_ceil(2);
            for row in 0..h {
                for col in 0..w {
                    let ci = (row / 2) * cw + col / 2;
                    let [r, g, b] = yuv_to_rgb(py[row * w + col], pu[ci], pv[ci]);
                    out[(row * w + col) * 3..][..3].copy_from_slice(&[b, g, r]);
                }
            }
            out
        }

        pub fn rgb_to_i420(pixels: &[u8], width: u32, height: u32, channels: usize, mirror: bool) -> Frame {
            let (w, h) = (width as usize, height as usize);
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let mut out = vec![0u8; w * h + 2 * cw * ch];
            let px = |x: usize, y: usize| {
                let x = if mirror { w - 1 - x } else { x };
                let i = (y * w + x) * channels;
                (pixels[i] as f32, pixels[i + 1] as f32, pixels[i + 2] as f32)
            };
            for y in 0..h {
                for x in 0..w {
                    let (r, g, b) = px(x, y);
                    out[y * w + x] = rgb_to_yuv(r, g, b).0.round().clamp(0.0, 255.0) as u8;
                }
            }
            let (us, vs) = out[w * h..].split_at_mut(cw * ch);
            for cy in 0..ch {
                for cx in 0..cw {
                    let (mut su, mut sv, mut n) = (0.0, 0.0, 0.0);
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (x, y) = (cx * 2 + dx, cy * 2 + dy);
                        if x < w && y < h {
                            let (r, g, b) = px(x, y);
                            let (_, u, v) = rgb_to_yuv(r, g, b);
                            su += u;
                            sv += v;
                            n += 1.0;
                        }
                    }
                    us[cy * cw + cx] = (su / n).round().clamp(0.0, 255.0) as u8;
                    vs[cy * cw + cx] = (sv / n).round().clamp(0.0, 255.0) as u8;
                }
            }
            Frame::new(width, height, PixelFormat::I420, out)
        }
    }

    /// A deterministic spread of bytes
    fn noise(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
        (0..len).map(|_| { x ^= x << 13; x ^= x >> 17; x ^= x << 5; (x >> 11) as u8 }).collect()
    }

    fn noisy_i420(w: u32, h: u32, seed: u32) -> Frame {
        Frame::new(w, h, PixelFormat::I420, noise(PixelFormat::I420.size(w, h), seed))
    }

    #[test]
    fn the_rounding_matches_round_and_clamp_for_every_f32_near_the_range() {
        let check = |x: f32| assert_eq!(round_byte(x), x.round().clamp(0.0, 255.0) as u8, "{x}");
        // Every f32 from just under zero to just over 255 would be 1.4 billion; every tenth of a step in the ties' neighbourhoods and a stride over the rest
        let (lo, hi) = ((-1.0f32).to_bits(), 256.0f32.to_bits());
        for bits in (0..=hi).step_by(7) {
            check(f32::from_bits(bits));
        }
        for bits in (0x8000_0000u32..=lo).step_by(1 << 12) {
            check(f32::from_bits(bits));
        }
        for n in 0..=256 {
            let half = n as f32 + 0.5;
            for x in [half, f32::from_bits(half.to_bits() - 1), f32::from_bits(half.to_bits() + 1), n as f32, -(half)] {
                check(x);
            }
        }
    }

    #[test]
    fn every_yuv_value_converts_as_before() {
        let t = terms();
        for y in 0..=255u8 {
            for u in 0..=255u8 {
                for v in 0..=255u8 {
                    let c = t.luma[y as usize];
                    let got = [round_byte(c + t.v_red[v as usize]), round_byte(c - t.u_green[u as usize] - t.v_green[v as usize]), round_byte(c + t.u_blue[u as usize])];
                    assert_eq!(got, yuv_to_rgb(y, u, v), "y {y} u {u} v {v}");
                }
            }
        }
    }

    /// The largest difference between two pictures' bytes, which has to be 1 or less (the vector conversion rounds a little differently, and the alpha is the same)
    fn largest_difference(a: &[u8], b: &[u8], channels: usize, what: &str) -> (u8, usize) {
        assert_eq!(a.len(), b.len(), "{what}");
        let (mut most, mut differing) = (0u8, 0usize);
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            let d = x.abs_diff(*y);
            assert!(d <= 1, "{what}: byte {i} is {x}, was {y}");
            if channels == 4 && i % 4 == 3 {
                assert_eq!(x, y, "{what}: alpha of byte {i}");
            }
            most = most.max(d);
            differing += (d > 0) as usize;
        }
        (most, differing)
    }

    /// Sizes around the 32 pixel blocks, odd ones, and tall and thin ones
    const SIZES: [(u32, u32); 21] = [(1, 1), (2, 2), (3, 3), (5, 2), (2, 7), (17, 9), (31, 3), (32, 2), (33, 3), (63, 4), (64, 36), (65, 5), (95, 7), (96, 1), (97, 6), (1, 40), (300, 301), (1366, 33), (1921, 33), (1920, 6), (3841, 5)];

    #[test]
    fn frames_convert_to_rgba_within_a_level_of_before_at_every_awkward_size() {
        for (w, h) in SIZES {
            let f = noisy_i420(w, h, w * 31 + h);
            largest_difference(&i420_to_rgba_flipped(&f), &reference::i420_to_rgba_flipped(&f), 4, &format!("{w}x{h}"));
        }
        let mut reused = vec![9u8; 3];
        i420_to_rgba_flipped_into(&noisy_i420(6, 4, 1), &mut reused);
        let f = noisy_i420(70, 6, 2);
        i420_to_rgba_flipped_into(&f, &mut reused);
        largest_difference(&reused, &reference::i420_to_rgba_flipped(&f), 4, "reused");
    }

    #[test]
    fn frames_convert_to_bgra_and_bgr_within_a_level_of_before_at_every_awkward_size() {
        for (w, h) in SIZES {
            let f = noisy_i420(w, h, w * 17 + h);
            let mut out = vec![7u8; 5];
            i420_to_bgra_into(&f, &mut out);
            largest_difference(&out, &reference::i420_to_bgra(&f), 4, &format!("bgra {w}x{h}"));
            largest_difference(&i420_to_bgr(&f), &reference::i420_to_bgr(&f), 3, &format!("bgr {w}x{h}"));
        }
    }

    /// Smooth pictures like a camera makes (a gradient of luma with slowly changing chroma and some noise), at both the sizes that matter
    fn camera_like(w: u32, h: u32) -> Frame {
        let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
        let grain = noise(w as usize * h as usize, 3);
        let mut data: Vec<u8> = (0..w as usize * h as usize).map(|i| (16.0 + 219.0 * ((i % w as usize) as f32 / w as f32 * 0.7 + (i / w as usize) as f32 / h as f32 * 0.3)) as u8 + grain[i] % 3).collect();
        data.extend((0..cw * ch).map(|i| (60 + (i % cw) * 130 / cw) as u8));
        data.extend((0..cw * ch).map(|i| (200 - (i / cw) * 130 / ch) as u8));
        Frame::new(w, h, PixelFormat::I420, data)
    }

    #[test]
    fn camera_like_frames_stay_within_a_level_at_hd_and_4k() {
        for (w, h) in [(1920, 1080), (3840, 2160), (1280, 720)] {
            let f = camera_like(w, h);
            let mut out = Vec::new();
            i420_to_bgra_into(&f, &mut out);
            let (most, differing) = largest_difference(&out, &reference::i420_to_bgra(&f), 4, &format!("{w}x{h}"));
            eprintln!("{w}x{h}: largest difference {most}, {:.2}% of bytes differ", differing as f64 * 100.0 / out.len() as f64);
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn every_yuv_value_converts_within_a_level_on_the_vector_path() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let ys: Vec<u8> = (0..=255).collect();
        let (mut differing, mut most) = (0usize, 0u8);
        for u in 0..=255u8 {
            for v in 0..=255u8 {
                let (us, vs) = ([u; 128], [v; 128]);
                for (name, out) in [("bgra", { let mut o = [0u8; 1024]; assert_eq!(telescope_core::yuv::row::<{ telescope_core::yuv::BGRA }>(&ys, &us, &vs, false, &mut o), 256); o }), ("rgba", { let mut o = [0u8; 1024]; assert_eq!(telescope_core::yuv::row::<{ telescope_core::yuv::RGBA }>(&ys, &us, &vs, false, &mut o), 256); o })] {
                    for (y, px) in out.as_chunks::<4>().0.iter().enumerate() {
                        let want = yuv_to_rgb(y as u8, u, v);
                        let got = if name == "bgra" { [px[2], px[1], px[0]] } else { [px[0], px[1], px[2]] };
                        assert_eq!(px[3], 255);
                        for c in 0..3 {
                            let d = got[c].abs_diff(want[c]);
                            assert!(d <= 1, "{name} y {y} u {u} v {v}: {got:?}, was {want:?}");
                            most = most.max(d);
                            differing += (d > 0) as usize;
                        }
                    }
                }
            }
        }
        eprintln!("vector conversion: largest difference {most}, {:.2}% of channel values differ", differing as f64 * 100.0 / (256.0 * 256.0 * 256.0 * 6.0));
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn the_vector_path_reads_and_writes_only_its_own_blocks_whatever_the_alignment() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let (y, u, v) = (noise(300, 1), noise(200, 2), noise(200, 3));
        for off in 0..5 {
            // 70 pixels: two whole blocks and 6 over, which are left alone
            let mut out = vec![0xAAu8; 70 * 4 + 7 + off];
            let done = telescope_core::yuv::row::<{ telescope_core::yuv::BGRA }>(&y[off..off + 70], &u[off..off + 35], &v[off..off + 35], false, &mut out[off..off + 70 * 4]);
            assert_eq!(done, 64);
            assert!(out[..off].iter().all(|b| *b == 0xAA) && out[off + 64 * 4..].iter().all(|b| *b == 0xAA), "off {off}");
            let mut whole = vec![0u8; 64 * 4];
            for x in 0..64 {
                let c = yuv_to_rgb(y[off + x], u[off + x / 2], v[off + x / 2]);
                whole[x * 4..][..4].copy_from_slice(&[c[2], c[1], c[0], 255]);
            }
            largest_difference(&out[off..off + 64 * 4], &whole, 4, &format!("off {off}"));
        }
        // Rows too short for a block, or a buffer too short for the row, do nothing
        assert_eq!(telescope_core::yuv::row::<{ telescope_core::yuv::BGRA }>(&y[..31], &u[..16], &v[..16], false, &mut [0u8; 124]), 0);
        assert_eq!(telescope_core::yuv::row::<{ telescope_core::yuv::BGRA }>(&y[..64], &u[..32], &v[..32], false, &mut [0u8; 64 * 4 - 4]), 32);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_sse41_rows_make_the_bytes_the_avx2_rows_do() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let (top, bot) = (noise(64 * 4, 21), noise(64 * 4, 22));
        for bgra in [false, true] {
            let (mut ty, mut by, mut uw, mut vw) = (vec![0u8; 64], vec![0u8; 64], vec![0u8; 32], vec![0u8; 32]);
            let (mut ty2, mut by2, mut un, mut vn) = (vec![0u8; 64], vec![0u8; 64], vec![0u8; 32], vec![0u8; 32]);
            // SAFETY: AVX2 was asked above (and implies SSE4.1), and 4 blocks of 16 pixels fit the 64 pixel rows
            unsafe {
                if bgra {
                    rgb4_pair_avx2::<true>(&top, &bot, &mut ty, &mut by, &mut uw, &mut vw, 4);
                    rgb4_pair_sse41::<true>(&top, &bot, &mut ty2, &mut by2, &mut un, &mut vn, 4);
                } else {
                    rgb4_pair_avx2::<false>(&top, &bot, &mut ty, &mut by, &mut uw, &mut vw, 4);
                    rgb4_pair_sse41::<false>(&top, &bot, &mut ty2, &mut by2, &mut un, &mut vn, 4);
                }
            }
            assert!(ty == ty2 && by == by2 && uw == un && vw == vn, "bgra {bgra}");
        }
    }

    #[test]
    fn black_white_and_the_privacy_cover_colours_come_out_exact() {
        for (w, h) in [(64usize, 4usize), (70, 3)] {
            for (y, u, v, want) in [(16u8, 128u8, 128u8, [0u8, 0, 0]), (235, 128, 128, [255, 255, 255]), (126, 128, 128, [128, 128, 128])] {
                let mut data = vec![y; w * h];
                data.extend(vec![u; w.div_ceil(2) * h.div_ceil(2)]);
                data.extend(vec![v; w.div_ceil(2) * h.div_ceil(2)]);
                let f = Frame::new(w as u32, h as u32, PixelFormat::I420, data);
                let mut out = Vec::new();
                i420_to_bgra_into(&f, &mut out);
                assert!(out.as_chunks::<4>().0.iter().all(|p| p[..3] == want && p[3] == 255), "{w}x{h} y {y}: {:?}", &out[..4]);
            }
        }
    }

    #[test]
    fn rgb_converts_to_i420_as_before_at_every_awkward_size_and_stride() {
        for (w, h) in [(1, 1), (2, 2), (3, 3), (5, 2), (2, 7), (1, 6), (6, 1), (17, 9), (64, 36), (255, 129), (301, 300), (1201, 701), (1200, 700)] {
            for channels in [3, 4] {
                for mirror in [false, true] {
                    let px = noise(w * h * channels, (w * 7 + h) as u32);
                    let (a, b) = (rgb_to_i420(&px, w as u32, h as u32, channels, mirror), reference::rgb_to_i420(&px, w as u32, h as u32, channels, mirror));
                    assert_eq!(a.data, b.data, "{w}x{h} x{channels} mirror {mirror}");
                }
            }
        }
    }

    #[test]
    fn bgra_converts_to_i420_like_rgba_at_vector_and_awkward_widths() {
        for (w, h) in [(16, 2), (32, 4), (48, 5), (64, 36), (70, 3), (1200, 700), (1216, 701)] {
            let rgba = noise(w * h * 4, (w * 11 + h) as u32);
            let mut bgra = rgba.clone();
            bgra.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p.swap(0, 2));
            let want = reference::rgb_to_i420(&rgba, w as u32, h as u32, 4, false);
            assert_eq!(bgra_to_i420(&bgra, w as u32, h as u32).data, want.data, "{w}x{h} bgra");
            assert_eq!(rgb_to_i420(&rgba, w as u32, h as u32, 4, false).data, want.data, "{w}x{h} rgba");
        }
    }

    #[test]
    fn converting_into_a_dirty_buffer_leaves_nothing_of_it_behind() {
        for (w, h) in [(1, 1), (16, 2), (33, 17), (64, 36), (70, 3)] {
            let rgba = noise(w * h * 4, (w + h * 5) as u32);
            let (mut dirty, mut small) = (vec![0xABu8; w * h * 4 + 99], vec![0xCDu8; 3]);
            let want = rgb_to_i420(&rgba, w as u32, h as u32, 4, false).data.to_vec();
            rgb_to_i420_into(&rgba, w as u32, h as u32, 4, false, &mut dirty);
            rgb_to_i420_into(&rgba, w as u32, h as u32, 4, false, &mut small);
            assert_eq!(dirty, want, "{w}x{h} larger buffer");
            assert_eq!(small, want, "{w}x{h} smaller buffer");
            let i420 = Frame::new(w as u32, h as u32, PixelFormat::I420, want);
            let (mut flipped, fresh) = (vec![0xABu8; w * h * 4 + 7], i420_to_rgba_flipped(&i420));
            i420_to_rgba_flipped_into(&i420, &mut flipped);
            assert_eq!(flipped, fresh, "{w}x{h} flipped");
        }
    }

    #[test]
    fn rgb_converts_to_i420_as_before_for_extreme_colours() {
        // Every corner of the colour cube and a grey ramp, which is where the sums sit nearest a rounding tie
        let mut px = Vec::new();
        for r in [0u8, 255] { for g in [0u8, 255] { for b in [0u8, 255] { px.extend([r, g, b, 255]); } } }
        for v in 0..=255u8 { px.extend([v, v, v, 255]); px.extend([v, 255 - v, v / 2, 255]); }
        let n = px.len() / 4;
        px.truncate(n * 4);
        let (w, h) = (n as u32, 1);
        assert_eq!(rgb_to_i420(&px, w, h, 4, false).data, reference::rgb_to_i420(&px, w, h, 4, false).data);
        let (w, h) = (n as u32 / 2, 2);
        assert_eq!(rgb_to_i420(&px[..(w * h * 4) as usize], w, h, 4, false).data, reference::rgb_to_i420(&px[..(w * h * 4) as usize], w, h, 4, false).data);
    }

    fn time(what: &str, runs: u32, mut f: impl FnMut()) -> f64 {
        f();
        let start = std::time::Instant::now();
        for _ in 0..runs {
            f();
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        eprintln!("{what}: {ms:.2} ms");
        ms
    }

    #[test]
    #[ignore = "timing: cargo test --release -p telescope-outputs convert_timing -- --ignored --nocapture"]
    fn convert_timing() {
        for (name, w, h) in [("1080p", 1920u32, 1080u32), ("4K", 3840, 2160)] {
            let i420 = noisy_i420(w, h, 5);
            let rgba = noise((w * h * 4) as usize, 6);
            let mut out = Vec::new();
            time(&format!("{name} i420->bgra, tables"), 20, || { reference::i420_to_bgra_into_tables(&i420, &mut out); std::hint::black_box(&out); });
            time(&format!("{name} i420->bgra, new"), 20, || { i420_to_bgra_into(&i420, &mut out); std::hint::black_box(&out); });
            time(&format!("{name} i420->bgra, tables"), 20, || { reference::i420_to_bgra_into_tables(&i420, &mut out); std::hint::black_box(&out); });
            time(&format!("{name} i420->bgra, new"), 20, || { i420_to_bgra_into(&i420, &mut out); std::hint::black_box(&out); });
            time(&format!("{name} i420->bgra, tables"), 20, || { reference::i420_to_bgra_into_tables(&i420, &mut out); std::hint::black_box(&out); });
            time(&format!("{name} i420->bgra, new"), 20, || { i420_to_bgra_into(&i420, &mut out); std::hint::black_box(&out); });
            // One thread, as the cost in CPU time
            let (py, pu, pv) = i420.i420_planes().unwrap();
            let (t, cw, w, h) = (terms(), (w as usize).div_ceil(2), w as usize, h as usize);
            let mut line = vec![0u8; w * 4];
            time(&format!("{name} i420->bgra one thread, tables"), 5, || for row in 0..h { yuv_row::<true>(t, &py[row * w..][..w], &pu[(row / 2) * cw..][..cw], &pv[(row / 2) * cw..][..cw], &mut line); std::hint::black_box(&line); });
            time(&format!("{name} i420->bgra one thread, new"), 5, || for row in 0..h { convert_row::<true>(t, &py[row * w..][..w], &pu[(row / 2) * cw..][..cw], &pv[(row / 2) * cw..][..cw], &mut line); std::hint::black_box(&line); });
            let (w, h) = (w as u32, h as u32);
            time(&format!("{name} i420->rgba flipped, old"), 5, || { std::hint::black_box(reference::i420_to_rgba_flipped(&i420)); });
            let mut out = Vec::new();
            time(&format!("{name} i420->rgba flipped, new"), 20, || { i420_to_rgba_flipped_into(&i420, &mut out); std::hint::black_box(&out); });
            time(&format!("{name} rgba->i420, old"), 5, || { std::hint::black_box(reference::rgb_to_i420(&rgba, w, h, 4, false)); });
            for (to, tw, th) in [("1080p", 1920u32, 1080u32), ("720p", 1280, 720), ("4:3", 1600, 1200)] {
                if (tw, th) != (w, h) {
                    time(&format!("{name} fit to {to}"), 20, || { std::hint::black_box(fit_frame(&i420, tw, th)); });
                }
            }
            let mut out = Vec::new();
            time(&format!("{name} rgba->i420, new"), 20, || { rgb_to_i420_into(&rgba, w, h, 4, false, &mut out); std::hint::black_box(&out); });
        }
    }
}
