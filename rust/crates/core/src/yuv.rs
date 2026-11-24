//! I420 planes to RGB, RGBA or BGRA 32 pixels at a time on AVX2 (or SSE4.1 for a processor without it, 16 at a time with the same bytes), in 16 bit lanes with 6 fractional bits (BT.601 video range, each chroma sample shared by its two pixels). Within 1 of the float formulas for every Y, U and V. What it doesn't do (the tail of a row, a processor without SSE4.1) is left to the caller's plain loop.

/// Output layouts: bytes a pixel in memory order (alpha is opaque).
pub const RGB: u8 = 0;
pub const RGBA: u8 = 1;
pub const BGRA: u8 = 2;

#[cfg(target_arch = "x86_64")]
pub use avx2::row;

/// No vector path on this processor family: nothing is converted and the caller's plain loop does it all.
#[cfg(not(target_arch = "x86_64"))]
pub fn row<const L: u8>(_y: &[u8], _u: &[u8], _v: &[u8], _floored: bool, _out: &mut [u8]) -> usize {
    0
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use super::{BGRA, RGB};
    use crate::image::{avx2_wanted, sse41_wanted};
    use std::arch::x86_64::*;

    /// 0.164383, 0.596027, 0.017232, 0.391762 and 0.812968 at 15 fractional bits: a gain over 1 is its whole part plus one of these, as a lane can't hold 1.164 at that scale.
    const LUMA_EXTRA: i16 = 5387;
    const RED_EXTRA: i16 = 19531;
    const BLUE_EXTRA: i16 = 565;
    const GREEN_U: i16 = -12837;
    const GREEN_V: i16 = -26640;

    /// How many pixels, a whole number of 32s, one row converts here: its colours into `out` in layout `L` with the alpha opaque.
    /// With `floored` luma under 16 counts as 16. The rest of the row is left for the caller, who gets 0 when there is neither AVX2 nor SSE4.1.
    pub fn row<const L: u8>(y: &[u8], u: &[u8], v: &[u8], floored: bool, out: &mut [u8]) -> usize {
        let bytes = if L == RGB { 3 } else { 4 };
        let blocks = y.len().min(out.len() / bytes).min(u.len() * 2).min(v.len() * 2) / 32;
        if blocks == 0 {
            return 0;
        }
        if avx2_wanted() {
            // SAFETY: the processor was just asked whether it has AVX2, and `blocks` keeps every read and write below inside the slices
            unsafe { blocks_avx2::<L>(y, u, v, floored, out, blocks) }
            return blocks * 32;
        }
        if sse41_wanted() {
            // SAFETY: the processor was just asked whether it has SSE4.1, and `blocks` keeps every read and write below inside the slices
            unsafe { blocks_sse41::<L>(y, u, v, floored, out, blocks) }
            return blocks * 32;
        }
        0
    }

    /// The same arithmetic as blocks_avx2 on 128 bit lanes, 16 pixels at a time, so a processor with SSE4.1 but no AVX2 gets the same bytes.
    #[target_feature(enable = "sse4.1")]
    pub(super) unsafe fn blocks_sse41<const L: u8>(y: &[u8], u: &[u8], v: &[u8], floored: bool, out: &mut [u8], blocks: usize) {
        let (c16, c128, half, opaque) = (_mm_set1_epi16(16), _mm_set1_epi16(128), _mm_set1_epi16(HALF), _mm_set1_epi16(255));
        let (k_luma, k_red, k_blue, k_gu, k_gv) = (_mm_set1_epi16(LUMA_EXTRA), _mm_set1_epi16(RED_EXTRA), _mm_set1_epi16(BLUE_EXTRA), _mm_set1_epi16(GREEN_U), _mm_set1_epi16(GREEN_V));
        let lowest = _mm_set1_epi16(if floored { 0 } else { -16 });
        let drop_alpha = _mm_setr_epi8(0, 1, 2, 4, 5, 6, 8, 9, 10, 12, 13, 14, -1, -1, -1, -1);
        for b in 0..blocks * 2 {
            // 8 chroma samples, each as (value - 128) at 6 fractional bits
            let (us, vs) = (_mm_loadl_epi64(u.as_ptr().add(b * 8).cast()), _mm_loadl_epi64(v.as_ptr().add(b * 8).cast()));
            let d = _mm_slli_epi16::<6>(_mm_sub_epi16(_mm_cvtepu8_epi16(us), c128));
            let e = _mm_slli_epi16::<6>(_mm_sub_epi16(_mm_cvtepu8_epi16(vs), c128));
            let red = _mm_add_epi16(_mm_add_epi16(e, _mm_mulhrs_epi16(e, k_red)), half);
            let green = _mm_add_epi16(_mm_add_epi16(_mm_mulhrs_epi16(d, k_gu), _mm_mulhrs_epi16(e, k_gv)), half);
            let blue = _mm_add_epi16(_mm_add_epi16(_mm_add_epi16(d, d), _mm_mulhrs_epi16(d, k_blue)), half);
            let twice = |x: __m128i| (_mm_unpacklo_epi16(x, x), _mm_unpackhi_epi16(x, x));
            let (red, green, blue) = (twice(red), twice(green), twice(blue));
            let ys = y.as_ptr().add(b * 16);
            for (half_of, (r, g, bl)) in [(red.0, green.0, blue.0), (red.1, green.1, blue.1)].into_iter().enumerate() {
                let luma = _mm_cvtepu8_epi16(_mm_loadl_epi64(ys.add(half_of * 8).cast()));
                let a = _mm_slli_epi16::<6>(_mm_max_epi16(_mm_sub_epi16(luma, c16), lowest));
                let c = _mm_add_epi16(a, _mm_mulhrs_epi16(a, k_luma));
                let shift = |x: __m128i| _mm_srai_epi16::<6>(x);
                let (r, g, bl) = (shift(_mm_adds_epi16(c, r)), shift(_mm_adds_epi16(c, g)), shift(_mm_adds_epi16(c, bl)));
                let (first, third) = if L == BGRA { (bl, r) } else { (r, bl) };
                let (even, odd) = (_mm_packus_epi16(first, third), _mm_packus_epi16(g, opaque));
                let (pairs, tops) = (_mm_unpacklo_epi8(even, odd), _mm_unpackhi_epi8(even, odd));
                // Pixels 0 to 3 and 4 to 7 of these 8
                let (lo, hi) = (_mm_unpacklo_epi16(pairs, tops), _mm_unpackhi_epi16(pairs, tops));
                if L == RGB {
                    let at = out.as_mut_ptr().add(b * 48 + half_of * 24);
                    _mm_storeu_si128(at.cast(), _mm_shuffle_epi8(lo, drop_alpha));
                    // The last 12 bytes exactly, so nothing is written past the pixels
                    let last = _mm_shuffle_epi8(hi, drop_alpha);
                    _mm_storel_epi64(at.add(12).cast(), last);
                    at.add(20).cast::<i32>().write_unaligned(_mm_extract_epi32::<2>(last));
                } else {
                    let at = out.as_mut_ptr().add(b * 64 + half_of * 32);
                    _mm_storeu_si128(at.cast(), lo);
                    _mm_storeu_si128(at.add(16).cast(), hi);
                }
            }
        }
    }

    /// Half a level at 6 fractional bits, added to every sum so the shift that drops them rounds.
    const HALF: i16 = 32;

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn blocks_avx2<const L: u8>(y: &[u8], u: &[u8], v: &[u8], floored: bool, out: &mut [u8], blocks: usize) {
        let (c16, c128, half, opaque) = (_mm256_set1_epi16(16), _mm256_set1_epi16(128), _mm256_set1_epi16(HALF), _mm256_set1_epi16(255));
        let (k_luma, k_red, k_blue, k_gu, k_gv) = (_mm256_set1_epi16(LUMA_EXTRA), _mm256_set1_epi16(RED_EXTRA), _mm256_set1_epi16(BLUE_EXTRA), _mm256_set1_epi16(GREEN_U), _mm256_set1_epi16(GREEN_V));
        // The lowest luma step (luma - 16) that counts: -16 is every luma there is
        let lowest = _mm256_set1_epi16(if floored { 0 } else { -16 });
        // Takes the first three bytes of each of 4 pixels in a lane: 12 bytes, the rest zero
        let drop_alpha = _mm256_broadcastsi128_si256(_mm_setr_epi8(0, 1, 2, 4, 5, 6, 8, 9, 10, 12, 13, 14, -1, -1, -1, -1));
        for b in 0..blocks {
            // 16 chroma samples, each as (value - 128) at 6 fractional bits
            let (us, vs) = (_mm_loadu_si128(u.as_ptr().add(b * 16).cast()), _mm_loadu_si128(v.as_ptr().add(b * 16).cast()));
            let d = _mm256_slli_epi16::<6>(_mm256_sub_epi16(_mm256_cvtepu8_epi16(us), c128));
            let e = _mm256_slli_epi16::<6>(_mm256_sub_epi16(_mm256_cvtepu8_epi16(vs), c128));
            // What each adds to R, G and B (the rounding half included), then each given to its two pixels
            let red = _mm256_add_epi16(_mm256_add_epi16(e, _mm256_mulhrs_epi16(e, k_red)), half);
            let green = _mm256_add_epi16(_mm256_add_epi16(_mm256_mulhrs_epi16(d, k_gu), _mm256_mulhrs_epi16(e, k_gv)), half);
            let blue = _mm256_add_epi16(_mm256_add_epi16(_mm256_add_epi16(d, d), _mm256_mulhrs_epi16(d, k_blue)), half);
            // Within each 128 bit half unpack reads the low 4 samples (or the high 4), so the quarters are put in the order that gives pixels 0 to 15, then 16 to 31
            let twice = |x: __m256i| {
                let x = _mm256_permute4x64_epi64::<0xD8>(x);
                (_mm256_unpacklo_epi16(x, x), _mm256_unpackhi_epi16(x, x))
            };
            let (red, green, blue) = (twice(red), twice(green), twice(blue));
            let ys = y.as_ptr().add(b * 32);
            for (half_of, (r, g, bl)) in [(red.0, green.0, blue.0), (red.1, green.1, blue.1)].into_iter().enumerate() {
                let luma = _mm256_cvtepu8_epi16(_mm_loadu_si128(ys.add(half_of * 16).cast()));
                let a = _mm256_slli_epi16::<6>(_mm256_max_epi16(_mm256_sub_epi16(luma, c16), lowest));
                let c = _mm256_add_epi16(a, _mm256_mulhrs_epi16(a, k_luma));
                // Saturating, as the sums of the biggest values pass what a lane holds and only have to stay over 255
                let shift = |x: __m256i| _mm256_srai_epi16::<6>(x);
                let (r, g, bl) = (shift(_mm256_adds_epi16(c, r)), shift(_mm256_adds_epi16(c, g)), shift(_mm256_adds_epi16(c, bl)));
                let (first, third) = if L == BGRA { (bl, r) } else { (r, bl) };
                let (even, odd) = (_mm256_packus_epi16(first, third), _mm256_packus_epi16(g, opaque));
                let (pairs, tops) = (_mm256_unpacklo_epi8(even, odd), _mm256_unpackhi_epi8(even, odd));
                let (lo, hi) = (_mm256_unpacklo_epi16(pairs, tops), _mm256_unpackhi_epi16(pairs, tops));
                let (eight_a, eight_b) = (_mm256_permute2x128_si256::<0x20>(lo, hi), _mm256_permute2x128_si256::<0x31>(lo, hi));
                if L == RGB {
                    // 48 bytes for these 16 pixels, each store of 16 landing 12 on from the one before so the 4 spare bytes of a lane are written over
                    let at = out.as_mut_ptr().add(b * 96 + half_of * 48);
                    let (a, c) = (_mm256_shuffle_epi8(eight_a, drop_alpha), _mm256_shuffle_epi8(eight_b, drop_alpha));
                    _mm_storeu_si128(at.cast(), _mm256_castsi256_si128(a));
                    _mm_storeu_si128(at.add(12).cast(), _mm256_extracti128_si256::<1>(a));
                    _mm_storeu_si128(at.add(24).cast(), _mm256_castsi256_si128(c));
                    // The last 12 bytes exactly, so nothing is written past the pixels
                    let last = _mm256_extracti128_si256::<1>(c);
                    _mm_storel_epi64(at.add(36).cast(), last);
                    at.add(44).cast::<i32>().write_unaligned(_mm_extract_epi32::<2>(last));
                } else {
                    let at = out.as_mut_ptr().add(b * 128 + half_of * 64);
                    _mm256_storeu_si256(at.cast(), eight_a);
                    _mm256_storeu_si256(at.add(32).cast(), eight_b);
                }
            }
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::frame::{Frame, PixelFormat};
    use crate::image::{byte, read_420, scalar_switch};

    /// The float formulas the plain loops of read_420 use.
    fn reference(y: u8, u: u8, v: u8, floored: bool) -> [u8; 3] {
        let c = (y as f32 - 16.0).max(if floored { 0.0 } else { -16.0 }) * 1.164_383;
        let (d, e) = (u as f32 - 128.0, v as f32 - 128.0);
        [byte(c + 1.596_027 * e), byte(c - 0.391_762 * d - 0.812_968 * e), byte(c + 2.017_232 * d)]
    }

    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..n).map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        }).collect()
    }

    fn run<const L: u8>(y: &[u8], u: &[u8], v: &[u8], floored: bool) -> Vec<u8> {
        let mut out = vec![0u8; y.len() * if L == RGB { 3 } else { 4 }];
        assert_eq!(row::<L>(y, u, v, floored, &mut out), y.len());
        out
    }

    #[test]
    fn every_yuv_value_converts_within_a_level_in_every_layout() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        scalar_switch::vector(every_value);
    }

    fn every_value() {
        let ys: Vec<u8> = (0..=255).collect();
        let (mut differing, mut most) = (0usize, 0u8);
        for u in 0..=255u8 {
            for v in 0..=255u8 {
                let (us, vs) = ([u; 128], [v; 128]);
                for floored in [false, true] {
                    let (rgb, rgba, bgra) = (run::<RGB>(&ys, &us, &vs, floored), run::<RGBA>(&ys, &us, &vs, floored), run::<BGRA>(&ys, &us, &vs, floored));
                    for y in 0..256 {
                        let want = reference(y as u8, u, v, floored);
                        let got = [[rgb[y * 3], rgb[y * 3 + 1], rgb[y * 3 + 2]], [rgba[y * 4], rgba[y * 4 + 1], rgba[y * 4 + 2]], [bgra[y * 4 + 2], bgra[y * 4 + 1], bgra[y * 4]]];
                        assert!(rgba[y * 4 + 3] == 255 && bgra[y * 4 + 3] == 255);
                        for g in got {
                            for c in 0..3 {
                                let d = g[c].abs_diff(want[c]);
                                assert!(d <= 1, "y {y} u {u} v {v} floored {floored}: {g:?}, was {want:?}");
                                most = most.max(d);
                                differing += (d > 0) as usize;
                            }
                        }
                    }
                }
            }
        }
        eprintln!("largest difference {most}, {:.2}% of channel values differ", differing as f64 * 100.0 / (256.0 * 256.0 * 256.0 * 18.0));
    }

    #[test]
    fn the_sse41_blocks_make_the_bytes_the_avx2_blocks_do() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let (y, u, v) = (noise(32 * 40, 11), noise(16 * 40, 12), noise(16 * 40, 13));
        for floored in [false, true] {
            for (bytes, layout) in [(3usize, RGB), (4, RGBA), (4, BGRA)] {
                let (mut wide, mut narrow) = (vec![0u8; y.len() * bytes], vec![0u8; y.len() * bytes]);
                // SAFETY: both were asked above (AVX2 implies SSE4.1), and 40 blocks of 32 pixels fit every slice
                unsafe {
                    match layout {
                        RGB => (avx2::blocks_avx2::<RGB>(&y, &u, &v, floored, &mut wide, 40), avx2::blocks_sse41::<RGB>(&y, &u, &v, floored, &mut narrow, 40)),
                        RGBA => (avx2::blocks_avx2::<RGBA>(&y, &u, &v, floored, &mut wide, 40), avx2::blocks_sse41::<RGBA>(&y, &u, &v, floored, &mut narrow, 40)),
                        _ => (avx2::blocks_avx2::<BGRA>(&y, &u, &v, floored, &mut wide, 40), avx2::blocks_sse41::<BGRA>(&y, &u, &v, floored, &mut narrow, 40)),
                    };
                }
                assert!(wide == narrow, "layout {layout} floored {floored}");
            }
        }
    }

    #[test]
    fn only_its_own_blocks_are_written_whatever_the_alignment_and_layout() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        scalar_switch::vector(own_blocks);
    }

    fn own_blocks() {
        let (y, u, v) = (noise(300, 1), noise(200, 2), noise(200, 3));
        for off in 0..5 {
            for (bytes, name) in [(3usize, "rgb"), (4, "rgba")] {
                // 70 pixels: two whole blocks and 6 over, which are left alone
                let mut out = vec![0xAAu8; 70 * bytes + 7 + off];
                let slice = &mut out[off..off + 70 * bytes];
                let done = if bytes == 3 { row::<RGB>(&y[off..off + 70], &u[off..off + 35], &v[off..off + 35], false, slice) } else { row::<RGBA>(&y[off..off + 70], &u[off..off + 35], &v[off..off + 35], false, slice) };
                assert_eq!(done, 64);
                assert!(out[..off].iter().all(|b| *b == 0xAA) && out[off + 64 * bytes..].iter().all(|b| *b == 0xAA), "{name} off {off}");
                for x in 0..64 {
                    let want = reference(y[off + x], u[off + x / 2], v[off + x / 2], false);
                    for c in 0..3 {
                        assert!(out[off + x * bytes + c].abs_diff(want[c]) <= 1, "{name} off {off} x {x}");
                    }
                }
            }
        }
        // A row too short for a block, or a buffer too short for the row, does as much as fits
        assert_eq!(row::<RGB>(&y[..31], &u[..16], &v[..16], false, &mut [0u8; 93]), 0);
        assert_eq!(row::<RGB>(&y[..64], &u[..32], &v[..32], false, &mut [0u8; 64 * 3 - 3]), 32);
        assert_eq!(row::<RGBA>(&y[..64], &u[..32], &v[..32], false, &mut [0u8; 64 * 4 - 4]), 32);
    }

    #[test]
    fn a_frame_reads_within_a_level_of_the_plain_loops_at_odd_sizes() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let mut most = 0u8;
        for (w, h) in [(1usize, 1usize), (2, 2), (31, 3), (32, 2), (33, 17), (65, 5), (100, 100), (1919, 7), (4097, 3), (1920, 1080)] {
            for format in [PixelFormat::I420, PixelFormat::Nv12] {
                for floored in [false, true] {
                    let frame = Frame::new(w as u32, h as u32, format, noise(format.size(w as u32, h as u32), (w * h) as u32));
                    let (mut fast, mut plain) = (vec![0u8; w * h * 3], vec![0u8; w * h * 3]);
                    read_420(&frame, &mut fast, floored).unwrap();
                    scalar_switch::plain(|| read_420(&frame, &mut plain, floored).unwrap());
                    for (i, (a, b)) in fast.iter().zip(&plain).enumerate() {
                        let d = a.abs_diff(*b);
                        assert!(d <= 1, "{w}x{h} {format:?} floored {floored} at byte {i}: {a} against {b}");
                        most = most.max(d);
                    }
                }
            }
        }
        eprintln!("frames: largest difference {most}");
    }
}
