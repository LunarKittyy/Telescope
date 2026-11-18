//! The Cinema tints on the graphics card against cinema_tools::tint on the CPU; without a card they skip.

use super::*;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_media::gpu::Gpu;

/// Colour with noise, a luma ramp, blocks, a bright disc and a dark one with hard edges, so every tool has something to judge.
fn scene(w: usize, h: usize) -> Frame {
    noisy_scene(w, h, 1.0)
}

/// `scene` with the noise at `grain` times its size (a camera's is a good deal quieter than the test's).
fn noisy_scene(w: usize, h: usize, grain: f32) -> Frame {
    let mut n = 7u32;
    let mut data = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            n = n.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = ((n >> 28) as f32 - 8.0) * grain;
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let blocks = ((y * 6 / h) % 2) as f32 * 40.0;
            let mut c = [255.0 * u + noise, 120.0 + 90.0 * (v * 7.0).sin() + blocks + noise, 255.0 * (1.0 - u) * 0.6 + blocks + noise];
            let disc = |cx: f32, r: f32| ((u - cx) * w as f32 / h as f32).hypot(v - 0.5) < r;
            if disc(0.3, 0.2) {
                c = [250.0, 245.0, 240.0];
            } else if disc(0.8, 0.15) {
                c = [12.0, 20.0, 30.0];
            }
            data.extend(c.map(|c| c.clamp(0.0, 255.0) as u8));
            data.push(255);
        }
    }
    Frame::new(w as u32, h as u32, PixelFormat::Rgba, data)
}

/// The same tools on the CPU, as RGBA. `phase` is where the stripes end up (tint moves them on by one first).
fn cpu(look: &Look, frame: &Frame, phase: usize) -> Vec<u8> {
    let mut rgb = Rgb::from_frame(frame).unwrap();
    tint(look, &mut rgb, &mut ((phase + ZEBRA_PERIOD - 1) % ZEBRA_PERIOD));
    rgb.data.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect()
}

/// How much of the picture differs by more than `slack` levels in some colour, in %, and where the first difference is.
fn off(a: &[u8], b: &[u8], w: usize, slack: u8) -> (f64, Option<(usize, usize)>) {
    assert_eq!(a.len(), b.len());
    let mut first = None;
    let mut n = 0;
    for (i, (p, q)) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0).enumerate() {
        if p.iter().zip(q).any(|(a, b)| a.abs_diff(*b) > slack) {
            n += 1;
            first.get_or_insert((i % w, i / w));
        }
    }
    (100.0 * n as f64 / (a.len() / 4) as f64, first)
}

/// Runs a look on the card and on the CPU and checks they differ in at most `bound` % of the picture. None without a card.
fn compare(name: &str, look: &Look, frame: &Frame, phase: usize, bound: f64) -> Option<f64> {
    compare_within(name, look, frame, phase, bound, 0)
}

/// As compare, where a colour that is out by `slack` levels or fewer counts as the same.
fn compare_within(name: &str, look: &Look, frame: &Frame, phase: usize, bound: f64, slack: u8) -> Option<f64> {
    let Some(gpu) = Gpu::shared() else {
        eprintln!("no graphics card; skipped");
        return None;
    };
    let mut spec = look.tint_spec().unwrap();
    spec.stripe_phase = phase as u8;
    let card = gpu.tint_rgba(frame, &spec).unwrap();
    let (percent, first) = off(&card, &cpu(look, frame, phase), frame.width as usize, slack);
    eprintln!("{name} {}x{} phase {phase}: {percent:.4}% off by more than {slack} (bound {bound}%), first at {first:?}", frame.width, frame.height);
    assert!(percent <= bound, "{name} {}x{}: {percent:.4}% of the picture differs, first at {first:?}", frame.width, frame.height);
    Some(percent)
}

const SMALL: [(usize, usize); 2] = [(320, 180), (1280, 720)];

#[test]
fn false_colour_on_the_card_is_the_cpus() {
    let look = Look { false_colour: true, ..Look::default() };
    for (w, h) in SMALL {
        compare("false colour", &look, &scene(w, h), 0, 0.2);
    }
}

#[test]
fn zebras_on_the_card_are_the_cpus() {
    for level in [70, 85, 95, 100] {
        let look = Look { zebras: true, zebra_level: level, ..Look::default() };
        for (w, h) in SMALL {
            compare(&format!("zebras {level}"), &look, &scene(w, h), 3, 0.2);
        }
    }
}

#[test]
fn the_stripes_are_exactly_the_cpus_at_every_phase() {
    // White everywhere is bright everywhere, so what is left is the stripes alone
    let white = Frame::new(333, 111, PixelFormat::Rgba, vec![255; 333 * 111 * 4]);
    let look = Look { zebras: true, zebra_level: 70, ..Look::default() };
    for phase in 0..8 {
        compare("stripes", &look, &white, phase, 0.0);
    }
    // And the first light level the zebras start at, on a ramp with no noise
    let ramp: Vec<u8> = (0..64 * 16).flat_map(|i| [(i % 64 * 4) as u8; 3].into_iter().chain([255])).collect();
    let ramp = Frame::new(64, 16, PixelFormat::Rgba, ramp);
    for level in [70, 100] {
        compare("ramp stripes", &Look { zebras: true, zebra_level: level, ..look.clone() }, &ramp, 5, 0.0);
    }
}

#[test]
fn peaking_on_the_card_is_the_cpus() {
    for level in [1, 5, 10] {
        let look = Look { peaking: true, peaking_level: level, peaking_colour: "green", ..Look::default() };
        for (w, h) in SMALL {
            compare(&format!("peaking {level}"), &look, &scene(w, h), 0, 1.0);
        }
        compare(&format!("peaking {level}"), &look, &scene(1920, 1080), 0, 3.0);
    }
}

#[test]
fn the_tools_together_are_painted_in_the_cpus_order() {
    let look = Look { false_colour: true, zebras: true, zebra_level: 80, peaking: true, peaking_level: 6, peaking_colour: "white", ..Look::default() };
    for (w, h) in SMALL {
        compare("together", &look, &scene(w, h), 6, 1.0);
    }
    // Zebras and peaking over the plain picture, then each alone on a picture with nothing to judge
    let look = Look { zebras: true, zebra_level: 90, peaking: true, peaking_level: 3, ..Look::default() };
    compare("together without colours", &look, &scene(640, 360), 2, 1.0);
}

#[test]
fn a_picture_wider_than_the_tools_judge_is_shrunk_first() {
    let look = Look { false_colour: true, zebras: true, zebra_level: 85, peaking: true, peaking_level: 6, ..Look::default() };
    let frame = scene(1920, 1080);
    // Where the card and the CPU shrink a little differently, a grey in false colour can come out a few levels apart
    for (what, look, slack) in [
        ("false colour", Look { false_colour: true, ..Look::default() }, 3),
        ("zebras", Look { zebras: true, zebra_level: 85, ..Look::default() }, 0),
        ("together", look, 3),
    ] {
        compare_within(what, &look, &frame, 1, 3.0, slack);
    }
    // An odd shape that doesn't shrink to whole pixels
    compare_within("odd", &Look { zebras: true, zebra_level: 85, peaking: true, ..Look::default() }, &scene(1531, 887), 4, 3.0, 0);
}

#[test]
fn the_small_readback_gives_the_scopes_the_picture_the_cpus_copy_does() {
    let Some(gpu) = Gpu::shared() else {
        eprintln!("no graphics card; skipped");
        return;
    };
    for (w, h) in [(1280usize, 720usize), (1920, 1080), (1531, 887)] {
        let frame = noisy_scene(w, h, 0.25);
        let texture = gpu.picture(w as u32, h as u32);
        gpu.paint(&frame, &texture).unwrap();
        let small = gpu.scope_picture(&texture, scopes::SCOPE_WIDTH as u32).unwrap();
        let (sw, sh) = scopes::shrunk_size(w, h, scopes::SCOPE_WIDTH).unwrap();
        assert_eq!((small.width as usize, small.height as usize), (sw, sh));
        assert_eq!(small.data.len(), sw * sh * 4);
        assert!(small.data.len() < 160_000, "about 150 KB comes back, not the picture");
        let from_card = Rgb::from_frame(&small).unwrap();
        let from_cpu = Rgb::from_frame(&frame).unwrap();
        for name in scopes::SCOPE_NAMES {
            let (a, b) = (scopes::scope_image(name, &from_card).unwrap(), scopes::scope_image(name, &from_cpu).unwrap());
            assert_eq!((a.w, a.h), (b.w, b.h));
            let mean = a.data.iter().zip(&b.data).map(|(p, q)| p.abs_diff(*q) as u64).sum::<u64>() as f64 / a.data.len() as f64;
            eprintln!("{name} {w}x{h}: mean {mean:.4} levels apart");
            // The card's average is a level off the CPU's in about 3% of values, which a waveform's sparse dots show most
            let bound = if name == "waveform" { 4.0 } else { 2.0 };
            assert!(mean <= bound, "{name} {w}x{h}: the scope from the small readback is {mean:.3} levels from the CPU's");
        }
    }
    // A picture no wider than a scope's is read back as it is
    let frame = scene(200, 100);
    let texture = gpu.picture(200, 100);
    gpu.paint(&frame, &texture).unwrap();
    assert_eq!(gpu.scope_picture(&texture, scopes::SCOPE_WIDTH as u32).unwrap().data, frame.data);
}

/// Every colour whose brightness (2126 r + 7152 g + 722 b, over 10000) lands exactly half way between two levels, where one rounding
/// rule and another part ways.
fn halves() -> Vec<[u8; 3]> {
    let inverse = (1..5000u32).find(|k| 361 * k % 5000 == 1).unwrap();
    let mut found = Vec::new();
    for r in 0..256u32 {
        for g in 0..256u32 {
            let need = (5000 + 10000 - (2126 * r + 7152 * g) % 10000) % 10000;
            let b = (need / 2 % 5000) * inverse % 5000;
            if b < 256 {
                found.push([r as u8, g as u8, b as u8]);
            }
        }
    }
    found
}

fn halves_frame() -> Frame {
    let colours = halves();
    assert!(colours.len() > 3000, "{} colours", colours.len());
    let (w, h) = (64usize, colours.len().div_ceil(64));
    let mut data = Vec::with_capacity(w * h * 4);
    for i in 0..w * h {
        data.extend(colours.get(i).copied().unwrap_or([0; 3]));
        data.push(255);
    }
    Frame::new(w as u32, h as u32, PixelFormat::Rgba, data)
}

#[test]
fn a_brightness_half_way_between_two_levels_is_rounded_the_same_way_whatever_the_card_does_with_a_multiply_and_add() {
    let Some(gpu) = Gpu::shared() else {
        eprintln!("no graphics card; skipped");
        return;
    };
    // A table that tells every level apart, so a level that is one off shows
    let table: &'static [[u8; 3]; 256] = Box::leak(Box::new(std::array::from_fn(|i| [i as u8, 255 - i as u8, 0])));
    let spec = telescope_core::preview::TintSpec { false_colour: Some(table), ..Default::default() };
    let frame = halves_frame();
    let out = gpu.tint_rgba(&frame, &spec).unwrap();
    let colours = halves();
    let wrong: Vec<([u8; 3], u8, u8)> = colours.iter().enumerate().filter(|(i, c)| out[i * 4] != scopes::luma_of(c[0], c[1], c[2])).map(|(i, c)| (*c, out[i * 4], scopes::luma_of(c[0], c[1], c[2]))).take(5).collect();
    assert!(wrong.is_empty(), "colour, level on the card, level on the CPU: {wrong:?}");
}

#[test]
fn a_brightness_half_way_between_two_levels_is_the_same_level_in_the_cpus_false_colour() {
    let look = Look { false_colour: true, ..Look::default() };
    compare("half way", &look, &halves_frame(), 0, 0.0);
    // The example from the review: (230, 80, 130) comes to 115.5
    let patch = Frame::new(8, 8, PixelFormat::Rgba, [230, 80, 130, 255].repeat(64));
    compare("half way patch", &look, &patch, 0, 0.0);
}

/// Blocks of `size` pixels, white and black like a chess board, so where two corners meet the mask the shrunk copy makes has bright and dark diagonally.
fn chess(w: usize, h: usize, size: usize) -> Frame {
    let mut data = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let v = if (x / size + y / size).is_multiple_of(2) { 255 } else { 0 };
            data.extend([v, v, v, 255]);
        }
    }
    Frame::new(w as u32, h as u32, PixelFormat::Rgba, data)
}

#[test]
fn a_mask_scaled_up_from_a_shrunk_copy_is_the_cpus_to_the_pixel() {
    // 1531 x 887 shrinks to 1280 wide by a part of a pixel, so the masks are scaled up by weights that are never half and half. Blocks wide enough
    // that the shrunk copy is judged the same on both leave only the scaling to differ, and it has to match to the pixel; on narrow ones a level
    // of rounding in the shrink at the threshold itself can still move a few pixels
    let zebras = Look { zebras: true, zebra_level: 70, ..Look::default() };
    let peaking = Look { peaking: true, peaking_level: 3, ..Look::default() };
    for (size, bound) in [(3, 0.02), (5, 0.02), (9, 0.0), (48, 0.0)] {
        compare(&format!("chess {size} zebras"), &zebras, &chess(1531, 887, size), 2, bound);
        compare(&format!("chess {size} peaking"), &peaking, &chess(1531, 887, size), 0, bound);
    }
}
