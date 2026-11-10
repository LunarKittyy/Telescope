//! Full range YUV squeezed into the limited range everything after the decoder reads.

/// x / 255 for any x up to 65534, with 16-bit arithmetic only.
#[inline(always)]
fn div255(x: u16) -> u16 {
    (x + (x >> 8) + 1) >> 8
}

/// Full range 0..255 luma squeezed into 16..235, rounded as swscale does.
#[inline(always)]
fn luma(p: u8) -> u8 {
    16 + div255(p as u16 * 219 + 127) as u8
}

/// Full range 0..255 chroma squeezed into 16..240 about 128, rounded as swscale does (away from the middle on the halves, as its integer division is).
#[inline(always)]
fn chroma(p: u8) -> u8 {
    let d = p as i16 - 128;
    let q = div255(d.unsigned_abs() * 224 + 127) as i16;
    (128 + if d >= 0 { q } else { -q }) as u8
}

#[inline(always)]
fn squeeze(plane: &mut [u8], to: impl Fn(u8) -> u8) {
    for p in plane {
        *p = to(*p);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn squeeze_avx2(plane: &mut [u8], to: impl Fn(u8) -> u8) {
    squeeze(plane, to);
}

fn run(plane: &mut [u8], to: impl Fn(u8) -> u8) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the processor was just asked whether it has AVX2
        return unsafe { squeeze_avx2(plane, to) };
    }
    squeeze(plane, to);
}

/// Packed I420 of this size from the full range into the limited range, in place.
pub fn full_to_limited(i420: &mut [u8], width: usize, height: usize) {
    let (luma_plane, chroma_plane) = i420.split_at_mut(width * height);
    run(luma_plane, luma);
    run(chroma_plane, chroma);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tables this replaced, which are the definition.
    fn luma_table(i: u32) -> u8 {
        (16 + (i * 219 + 127) / 255) as u8
    }

    fn chroma_table(i: u32) -> u8 {
        let d = i as i32 - 128;
        (128 + (d * 224 + if d >= 0 { 127 } else { -127 }) / 255) as u8
    }

    #[test]
    fn every_byte_lands_where_the_tables_put_it() {
        let mut p: Vec<u8> = (0..=255).collect();
        let mut c = p.clone();
        run(&mut p, luma);
        run(&mut c, chroma);
        for i in 0..=255u32 {
            assert_eq!((p[i as usize], c[i as usize]), (luma_table(i), chroma_table(i)), "byte {i}");
        }
    }

    #[test]
    fn a_picture_is_squeezed_plane_by_plane() {
        // 3 x 2 of luma then two 2 x 1 chroma planes (the odd width rounds up)
        let mut pic = vec![0, 255, 128, 1, 254, 100, 0, 255, 128, 64];
        full_to_limited(&mut pic, 3, 2);
        let want: Vec<u8> = [0u32, 255, 128, 1, 254, 100].iter().map(|&i| luma_table(i)).chain([0u32, 255, 128, 64].iter().map(|&i| chroma_table(i))).collect();
        assert_eq!(pic, want);
    }
}
