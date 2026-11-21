//! I420 to RGBA or BGRA 32 pixels at a time on AVX2, in 16 bit lanes with 6 fractional bits. Within 1 of what yuv_to_rgb gives for every Y, U and V.

use std::arch::x86_64::*;

/// 0.164, 0.596, 0.017, 0.392 and 0.813 at 15 fractional bits: a gain over 1 is its whole part plus one of these, as a lane can't hold 1.164 at that scale.
const LUMA_EXTRA: i16 = 5374;
const RED_EXTRA: i16 = 19529;
const BLUE_EXTRA: i16 = 557;
const GREEN_U: i16 = -12845;
const GREEN_V: i16 = -26640;

/// How many pixels, a whole number of 32s, one row converts here: its colours as 4 bytes a pixel into `out` (BGRA when BGRA is set, else RGBA) with the alpha opaque.
/// The rest of the row is left for the caller.
pub fn row<const BGRA: bool>(y: &[u8], u: &[u8], v: &[u8], out: &mut [u8]) -> usize {
    let blocks = y.len().min(out.len() / 4).min(u.len() * 2).min(v.len() * 2) / 32;
    if blocks == 0 || !std::arch::is_x86_feature_detected!("avx2") {
        return 0;
    }
    // SAFETY: the processor was just asked whether it has AVX2, and `blocks` keeps every read and write below inside the slices
    unsafe { blocks_avx2::<BGRA>(y, u, v, out, blocks) }
    blocks * 32
}

/// Half a level at 6 fractional bits, added to every sum so the shift that drops them rounds.
const HALF: i16 = 32;

#[target_feature(enable = "avx2")]
unsafe fn blocks_avx2<const BGRA: bool>(y: &[u8], u: &[u8], v: &[u8], out: &mut [u8], blocks: usize) {
    let (c16, c128, half, opaque) = (_mm256_set1_epi16(16), _mm256_set1_epi16(128), _mm256_set1_epi16(HALF), _mm256_set1_epi16(255));
    let (k_luma, k_red, k_blue, k_gu, k_gv) = (_mm256_set1_epi16(LUMA_EXTRA), _mm256_set1_epi16(RED_EXTRA), _mm256_set1_epi16(BLUE_EXTRA), _mm256_set1_epi16(GREEN_U), _mm256_set1_epi16(GREEN_V));
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
            let a = _mm256_slli_epi16::<6>(_mm256_sub_epi16(luma, c16));
            let c = _mm256_add_epi16(a, _mm256_mulhrs_epi16(a, k_luma));
            // Saturating, as the sums of the biggest values pass what a lane holds and only have to stay over 255
            let shift = |x: __m256i| _mm256_srai_epi16::<6>(x);
            let (r, g, bl) = (shift(_mm256_adds_epi16(c, r)), shift(_mm256_adds_epi16(c, g)), shift(_mm256_adds_epi16(c, bl)));
            let (first, third) = if BGRA { (bl, r) } else { (r, bl) };
            let (even, odd) = (_mm256_packus_epi16(first, third), _mm256_packus_epi16(g, opaque));
            let (pairs, tops) = (_mm256_unpacklo_epi8(even, odd), _mm256_unpackhi_epi8(even, odd));
            let (lo, hi) = (_mm256_unpacklo_epi16(pairs, tops), _mm256_unpackhi_epi16(pairs, tops));
            let at = out.as_mut_ptr().add(b * 128 + half_of * 64);
            _mm256_storeu_si256(at.cast(), _mm256_permute2x128_si256::<0x20>(lo, hi));
            _mm256_storeu_si256(at.add(32).cast(), _mm256_permute2x128_si256::<0x31>(lo, hi));
        }
    }
}
