//! The sped-up painting routines against what they did before, byte for byte.

use super::*;

/// Bytes that don't repeat, with clear, solid and in-between alphas.
fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut s = seed.max(1);
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 11) as u8
        })
        .collect()
}

#[test]
fn a_scope_scales_to_the_byte_as_it_did_before_it_was_sped_up() {
    for (w, h, tw, th, seed) in [(8, 4, 5, 3, 1), (256, 100, 1382, 540, 2), (256, 256, 90, 120, 3), (1, 1, 7, 7, 4), (3, 9, 1, 1, 5), (64, 48, 64, 48, 6)] {
        let mut src = noise(w * h * 4, seed);
        // Runs of clear and solid pixels as scopes have, between the in-between ones
        for (i, p) in src.chunks_exact_mut(4).enumerate() {
            p[3] = match i % 5 {
                0 => 0,
                1 => 255,
                _ => p[3],
            };
        }
        assert_eq!(smooth_scale(&src, w, h, tw, th), smooth_scale_reference(&src, w, h, tw, th), "{w}x{h} to {tw}x{th}");
    }
}

/// Canvas::blend as it was written before blend_pixel took it over.
fn blend_before(d: &mut [u8], rgb: [u8; 3], alpha: f32) {
    let da = d[3] as f32 / 255.0;
    let out = alpha + da * (1.0 - alpha);
    if out <= 0.0 {
        return;
    }
    for c in 0..3 {
        d[c] = ((rgb[c] as f32 * alpha + d[c] as f32 * da * (1.0 - alpha)) / out).round().clamp(0.0, 255.0) as u8;
    }
    d[3] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
}

#[test]
fn blending_a_pixel_gives_the_bytes_it_always_did() {
    let bytes = noise(4 * 4000, 7);
    let mut checked = 0;
    for (n, d) in bytes.chunks_exact(4).enumerate() {
        for alpha in [1.0f32, 1e-9, 0.003, 0.25, 0.5, 0.999, (n % 256) as f32 / 255.0, (n * 7 % 256) as f32 * 0.7 / 255.0] {
            if alpha <= 0.0 {
                continue;
            }
            for da in [0u8, d[3]] {
                let (mut old, mut new) = ([d[0], d[1], d[2], da], [d[0], d[1], d[2], da]);
                let rgb = [d[2], d[0], d[1]];
                blend_before(&mut old, rgb, alpha);
                blend_pixel(&mut new, rgb, alpha);
                assert_eq!(new, old, "alpha {alpha} over {:?}", [d[0], d[1], d[2], da]);
                checked += 1;
            }
        }
    }
    assert!(checked > 20000);
}

#[test]
fn a_picture_laid_on_the_canvas_lands_as_it_did_pixel_by_pixel() {
    let picture = Bgra { w: 40, h: 30, data: noise(40 * 30 * 4, 9) };
    for (x, y) in [(10i64, 12i64), (-15, -9), (70, 50), (-60, 5), (55, 100), (0, 0), (20, -30)] {
        let mut now = Canvas::new(64, 48);
        now.data = noise(64 * 48 * 4, 11);
        let mut before = Canvas { w: 64, h: 48, data: now.data.clone() };
        now.image(&picture, x, y);
        for row in 0..picture.h {
            for col in 0..picture.w {
                let (cx, cy) = (x + col as i64, y + row as i64);
                let p = picture.at(col, row);
                let alpha = p[3] as f32 / 255.0;
                if cx < 0 || cy < 0 || cx >= 64 || cy >= 48 || alpha <= 0.0 {
                    continue;
                }
                let i = (cy as usize * 64 + cx as usize) * 4;
                blend_before(&mut before.data[i..i + 4], [p[2], p[1], p[0]], alpha);
            }
        }
        assert_eq!(now.data, before.data, "at {x}, {y}");
    }
}
