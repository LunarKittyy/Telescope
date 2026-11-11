//! What the Cinema tools draw over the preview, and the layer the preview asks (tests/test_cinema.py).

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn layer_with(look: Look) -> CinemaLayer {
    let layer = CinemaLayer::default();
    layer.set_look(look);
    layer
}

/// A clock the test moves by hand.
fn clock() -> (Arc<AtomicU64>, Arc<dyn Fn() -> f64 + Send + Sync>) {
    let now = Arc::new(AtomicU64::new(100f64.to_bits()));
    let read = now.clone();
    (now, Arc::new(move || f64::from_bits(read.load(Ordering::SeqCst))))
}

fn set(now: &AtomicU64, seconds: f64) {
    now.store(seconds.to_bits(), Ordering::SeqCst);
}

/// What a pixel looks like laid over black, red channel.
fn red_over_black(canvas: &Canvas, x: usize, y: usize) -> f64 {
    let i = (y * canvas.w + x) * 4;
    canvas.data[i] as f64 * canvas.data[i + 3] as f64 / 255.0
}

#[test]
fn the_layer_tints_its_copy_and_only_analyses_with_a_scope() {
    let layer = CinemaLayer::default();
    assert_eq!(layer.analyse_fps(), 0.0);
    layer.set_look(Look { false_colour: true, scope: "waveform", ..Look::default() });
    assert!(layer.analyse_fps() > 0.0);
    let mut frame = Rgb::filled(64, 36, [0; 3]);
    layer.tint_rgb(&mut frame);
    assert_eq!(frame.at(0, 0), FALSE_COLOURS[0].1.unwrap());
    assert!(layer.analyse(&Frame::new(64, 36, PixelFormat::Rgba, vec![0; 64 * 36 * 4])));
    let (name, image) = layer.scope().expect("a scope");
    assert_eq!(name, "waveform");
    assert!(image.w > 0 && image.h > 0);
}

#[test]
fn the_layer_tints_a_frame_in_place_and_leaves_a_plain_one_alone() {
    let layer = layer_with(Look { false_colour: true, ..Look::default() });
    assert!(layer.tints());
    let mut frame = Frame::new(4, 2, PixelFormat::Rgba, vec![255; 4 * 2 * 4]);
    layer.tint(&mut frame);
    let clipped = FALSE_COLOURS[8].1.unwrap();
    assert_eq!(&frame.data[..4], &[clipped[0], clipped[1], clipped[2], 255]);
    let guides = layer_with(Look { guides: "thirds", ..Look::default() });
    assert!(!guides.tints());
    let mut plain = Frame::new(4, 2, PixelFormat::Rgba, vec![7; 4 * 2 * 4]);
    guides.tint(&mut plain);
    assert_eq!(plain.data.as_slice(), vec![7; 32].as_slice());
}

#[test]
fn the_guides_draw_where_they_should() {
    for (guides, shares) in [("thirds", vec![1.0 / 3.0, 2.0 / 3.0]), ("cross", vec![0.5]), ("golden", vec![0.382, 0.618])] {
        let (w, h) = (600usize, 300usize);
        let layer = layer_with(Look { guides, ..Look::default() });
        let canvas = layer.paint_canvas(w, h, 1.0).expect("the guides");
        let row = h / 7;
        let lit: Vec<usize> = (0..w).filter(|&x| red_over_black(&canvas, x, row) > 100.0).collect();
        assert_eq!(lit.len(), shares.len(), "{guides}");
        for (x, share) in lit.iter().zip(&shares) {
            assert!((*x as f64 - w as f64 * share).abs() <= 1.5, "{guides}: {x} for {share}");
        }
        assert_eq!(red_over_black(&canvas, w / 7, row), 0.0);
    }
}

#[test]
fn the_line_turns_clockwise_for_a_positive_roll() {
    let rect = Rect { x: 0.0, y: 0.0, w: 640.0, h: 360.0 };
    let (left, right) = level_line(rect, 0.0);
    assert!(left.1 == 180.0 && right.1 == 180.0 && left.0 < 320.0 && 320.0 < right.0);
    let (left, right) = level_line(rect, 10.0);
    // the right end drops
    assert!(right.1 > 180.0 && 180.0 > left.1);
    let (left, right) = level_line(rect, 90.0);
    assert!((left.0 - 320.0).abs() < 1e-9 && right.1 > left.1);
}

#[test]
fn the_guide_paints_without_trouble() {
    let rect = Rect { x: 0.0, y: 0.0, w: 320.0, h: 180.0 };
    for level in [
        Level { roll: Some(3.2), pitch: -4.0, roll_level: false, pitch_level: false },
        Level { roll: Some(0.0), pitch: 0.0, roll_level: true, pitch_level: true },
        Level { roll: None, pitch: 88.0, roll_level: false, pitch_level: false },
    ] {
        let mut canvas = Canvas::new(320, 180);
        paint_level(&mut canvas, rect, &level, 1.0);
        assert_eq!(canvas.data.len(), 320 * 180 * 4);
    }
    let mut canvas = Canvas::new(320, 180);
    paint_level(&mut canvas, rect, &Level { roll: Some(3.2), pitch: -4.0, roll_level: false, pitch_level: false }, 1.0);
    assert!(!canvas.is_empty());
}

#[test]
fn a_frozen_horizon_isnt_drawn() {
    let (now, read) = clock();
    let layer = CinemaLayer::with_clock(read);
    layer.set_look(Look { level: true, ..Look::default() });
    layer.set_level(Some((Level { roll: Some(3.0), pitch: 0.0, roll_level: false, pitch_level: false }, 10.0)));
    set(&now, 10.5);
    assert!(layer.paint_canvas(64, 36, 1.0).is_some());
    // at 12 s the newest roll is 2 s old
    set(&now, 12.0);
    assert!(layer.paint_canvas(64, 36, 1.0).is_none());
}

#[test]
fn painting_notes_when_the_preview_last_drew_it() {
    let (now, read) = clock();
    let layer = CinemaLayer::with_clock(read);
    layer.set_look(Look { guides: "cross", ..Look::default() });
    assert_eq!(layer.painted_at(), 0.0);
    set(&now, 123.5);
    layer.paint(64, 36, 1.0);
    assert_eq!(layer.painted_at(), 123.5);
}

#[test]
fn nothing_on_means_nothing_drawn() {
    assert!(CinemaLayer::default().paint(64, 36, 1.0).is_none());
}

#[test]
fn the_scope_sits_bottom_right_and_keeps_its_shape() {
    let rect = Rect { x: 0.0, y: 0.0, w: 640.0, h: 360.0 };
    let (x, y, w, h) = scope_target(rect, (256, 128), "medium", 1.0);
    assert!((w - 230.4).abs() < 1e-9 && (h - 115.2).abs() < 1e-9);
    assert!((x - (640.0 - 230.4 - 8.0)).abs() < 1e-9 && (y - (360.0 - 115.2 - 8.0)).abs() < 1e-9);
    // a tall scope is held to 60% of the preview's height
    let (_, _, w, h) = scope_target(rect, (256, 256), "large", 1.0);
    assert!((h - 216.0).abs() < 1e-9 && (w - 216.0).abs() < 1e-9);
    // an unknown size is the medium one
    assert_eq!(scope_target(rect, (256, 128), "huge", 1.0), scope_target(rect, (256, 128), "medium", 1.0));
}

#[test]
fn a_scope_is_drawn_once_its_picture_is_in() {
    let layer = layer_with(Look { scope: "histogram", ..Look::default() });
    // nothing worked out yet
    assert!(layer.paint_canvas(640, 360, 1.0).is_none());
    layer.analyse_rgb(&Rgb::filled(64, 36, [120, 60, 30]));
    let canvas = layer.paint_canvas(640, 360, 1.0).expect("the scope");
    // its backdrop shows bottom right and nowhere near the top left
    assert!(canvas.data[((350 * 640) + 630) * 4 + 3] > 0);
    assert_eq!(canvas.data[3], 0);
    // a scope picked since is not drawn from the old one's picture
    layer.set_look(Look { scope: "parade", ..Look::default() });
    assert!(layer.paint_canvas(640, 360, 1.0).is_none());
}

#[test]
fn the_false_colour_legend_names_the_colours_it_uses() {
    let layer = layer_with(Look { false_colour: true, ..Look::default() });
    // it needs a font to name them; without one the swatches are still the legend's
    if let Some(canvas) = layer.paint_canvas(640, 360, 1.0) {
        assert_eq!(canvas.data[3], 0);
        let swatch = FALSE_COLOURS[0].1.unwrap();
        let found = canvas.data.as_chunks::<4>().0.iter().any(|p| p[..3] == swatch && p[3] == 255);
        assert!(found || app_font(Weight::Regular).is_none());
    } else {
        assert!(app_font(Weight::Regular).is_none());
    }
}

#[test]
fn a_scope_shrinks_as_qt_draws_it_without_see_through_pixels_darkening_it() {
    // Columns of opaque white and clear, 8 x 4 to 5 x 3; the alphas are what QPainter.drawImage gave
    let src: Vec<u8> = (0..8 * 4).flat_map(|i| if i % 2 == 0 { [255u8; 4] } else { [0u8; 4] }).collect();
    let out = smooth_scale(&src, 8, 4, 5, 3);
    for (x, want) in [179i32, 229, 127, 25, 77].into_iter().enumerate() {
        let p = &out[(5 + x) * 4..][..4];
        assert_eq!(p[..3], [255, 255, 255], "clear neighbours leave the colour alone");
        assert!((p[3] as i32 - want).abs() <= 1, "column {x}: {} against Qt's {want}", p[3]);
    }
}

/// paint() on the layer against paint_canvas() on it, which paints from nothing: the same pixels, or both nothing.
fn same_paint(layer: &CinemaLayer, w: usize, h: usize, scale: f64, what: &str) {
    let fast = layer.paint(w as u32, h as u32, scale as f32);
    match (fast, layer.paint_canvas(w, h, scale)) {
        (None, None) => {}
        (Some(frame), Some(canvas)) => assert!(*frame.data == canvas.data, "{what}"),
        (fast, slow) => panic!("{what}: {} against {}", fast.is_some(), slow.is_some()),
    }
}

/// A picture with something different in it for each seed, for the scope to draw.
fn scene(seed: u32) -> Rgb {
    let mut rgb = Rgb::filled(96, 54, [0; 3]);
    for (i, v) in rgb.data.iter_mut().enumerate() {
        *v = ((i as u32 * (7 + seed * 3) / 5) % 256) as u8;
    }
    rgb
}

#[test]
fn the_kept_paint_matches_painting_from_nothing_as_things_change() {
    let (now, read) = clock();
    let layer = CinemaLayer::with_clock(read);
    let level = |roll: f64| Level { roll: Some(roll), pitch: 2.5, roll_level: false, pitch_level: false };
    layer.set_look(Look { guides: "thirds", false_colour: true, scope: "waveform", ..Look::default() });
    same_paint(&layer, 640, 360, 1.0, "guides and legend, no scope picture yet");
    layer.analyse_rgb(&scene(1));
    same_paint(&layer, 640, 360, 1.0, "a scope");
    // a scope that moves, the preview letting go of each picture before the next
    for seed in 2..6 {
        layer.analyse_rgb(&scene(seed));
        same_paint(&layer, 640, 360, 1.0, "the scope moved");
    }
    // the level guide, tilting
    layer.set_look(Look { guides: "thirds", false_colour: true, scope: "waveform", level: true, ..Look::default() });
    for roll in [0.0, 3.0, -7.5, 12.0] {
        layer.set_level(Some((level(roll), 100.0)));
        same_paint(&layer, 640, 360, 1.0, "the level tilted");
    }
    // a picture handed out stays as it was while a newer one is painted
    let held = layer.paint(640, 360, 1.0).expect("a picture");
    let before = (*held.data).clone();
    layer.analyse_rgb(&scene(7));
    layer.set_level(Some((level(-3.0), 100.0)));
    same_paint(&layer, 640, 360, 1.0, "painted while the last was still held");
    assert!(*held.data == before, "a picture handed out is never drawn on again");
    drop(held);
    // a guide taken away and another put, the legend gone
    layer.set_look(Look { guides: "golden", scope: "waveform", level: true, ..Look::default() });
    same_paint(&layer, 640, 360, 1.0, "other guides");
    // another size, then another scale
    same_paint(&layer, 500, 300, 1.0, "resized");
    same_paint(&layer, 500, 300, 1.5, "rescaled");
    // a scope picked that has no picture yet, the level going stale, then everything off
    layer.set_look(Look { scope: "parade", level: true, ..Look::default() });
    same_paint(&layer, 500, 300, 1.5, "a scope not worked out yet");
    set(&now, 100.0 + LEVEL_STALE_S + 1.0);
    same_paint(&layer, 500, 300, 1.5, "a stale level");
    assert!(layer.paint(500, 300, 1.5).is_none());
    layer.set_look(Look::default());
    assert!(layer.paint(500, 300, 1.5).is_none());
}
