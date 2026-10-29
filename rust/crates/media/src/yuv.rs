//! Decoded pictures that aren't BT.601 in the limited range (a JPEG's full range) turned into it. Everything after the
//! decoder (the frame steps, the preview, the virtual cameras) reads I420 as BT.601 limited, which is also what the
//! Python app hands its virtual camera: OpenCV's BGR-to-I420 writes exactly that.

const fn luma_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = (16 + (i as u32 * 219 + 127) / 255) as u8;
        i += 1;
    }
    t
}

const fn chroma_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let d = i as i32 - 128;
        t[i] = (128 + (d * 224 + if d >= 0 { 127 } else { -127 }) / 255) as u8;
        i += 1;
    }
    t
}

/// Full range 0..255 luma and chroma squeezed into the limited 16..235 and 16..240, rounded as swscale does.
const LUMA: [u8; 256] = luma_table();
const CHROMA: [u8; 256] = chroma_table();

/// Packed I420 of this size from the full range into the limited range, in place.
pub fn full_to_limited(i420: &mut [u8], width: usize, height: usize) {
    let (luma, chroma) = i420.split_at_mut(width * height);
    for p in luma {
        *p = LUMA[*p as usize];
    }
    for p in chroma {
        *p = CHROMA[*p as usize];
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
}
