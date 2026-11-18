//! How long One picture's compositor takes to draw a frame, for
//! `cargo test --release -p telescope-effects compositor_speed -- --ignored --nocapture --test-threads=1`.
//! Set COMPOSITOR_SPEED_CASE to part of a case name to run only the cases whose names contain it.

use super::*;
use telescope_core::frame::Alpha;

const WARM_UPS: usize = 5;
const RUNS: usize = 40;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// The streams other than the target repeat their frame (cached scaling), the target is new every time.
    Steady,
    /// Every stream has a new frame every time.
    Fresh,
    /// Every picture the layout moves and zooms a little, so the cache key changes as well.
    Moving,
}

#[derive(Clone, Copy)]
enum Shape {
    Corner,
    Split,
    Three,
    Four,
}

fn pseudo_frame(seed: u8, (w, h): (u32, u32), format: PixelFormat) -> Frame {
    let data: Vec<u8> = (0..format.size(w, h)).map(|i| (i as u32).wrapping_mul(2654435761).wrapping_add(seed as u32).to_le_bytes()[2]).collect();
    Frame::new(w, h, format, data)
}

/// A soft oval of "person" in the middle of the frame, fading out over the edge: what the Background mask looks like.
fn soft_mask(w: u32, h: u32) -> Arc<Alpha> {
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let data = (0..h).flat_map(|y| (0..w).map(move |x| {
        let d = (((x as f32 - cx) / (cx * 0.7)).powi(2) + ((y as f32 - cy) / (cy * 0.9)).powi(2)).sqrt();
        let t = ((1.2 - d) / 0.4).clamp(0.0, 1.0);
        (t * t * (3.0 - 2.0 * t) * 255.0) as u8
    })).collect();
    Arc::new(Alpha::new(w, h, data, false))
}

fn boxes(shape: Shape) -> Vec<(f64, f64, f64, f64)> {
    match shape {
        Shape::Corner => vec![(0.0, 0.0, 1.0, 1.0), (0.70, 0.70, 0.27, 0.27)],
        Shape::Split => vec![(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 1.0)],
        Shape::Three => vec![(0.0, 0.0, 0.66, 1.0), (0.68, 0.0, 0.32, 0.49), (0.68, 0.51, 0.32, 0.49)],
        Shape::Four => vec![(0.0, 0.0, 0.5, 0.5), (0.5, 0.0, 0.5, 0.5), (0.0, 0.5, 0.5, 0.5), (0.5, 0.5, 0.5, 0.5)],
    }
}

/// The layout at step `step`: when moving, the first tile zooms a little and the others lose a sliver off their left side.
fn layout(shape: Shape, moving: Option<usize>, cut_out: bool) -> Layout {
    let tiles = boxes(shape).into_iter().enumerate().map(|(i, (x, y, w, h))| {
        let mut tile = Tile::boxed(&format!("s{i}"), "", x, y, w, h);
        if matches!(shape, Shape::Corner) && i == 1 {
            tile.border = 4;
            tile.border_colour = "#336699".into();
            tile.radius = 24;
        }
        if let Some(step) = moving {
            if i == 0 {
                tile.zoom = 1.0 + (step % 30) as f64 * 0.01;
            } else {
                let dx = (step % 60) as f64 / 60.0 * 0.02;
                tile.x += dx;
                tile.w -= dx;
            }
        }
        tile.cut_out = cut_out && i == boxes_len(shape) - 1;
        tile
    });
    Layout::of(tiles.collect())
}

fn boxes_len(shape: Shape) -> usize {
    boxes(shape).len()
}

struct Case {
    name: String,
    shape: Shape,
    canvas: (u32, u32),
    /// Whether every stream is the canvas's size or only the first.
    mixed: bool,
    format: PixelFormat,
    mode: Mode,
    cut_out: bool,
}

fn run(case: &Case) {
    let n = boxes_len(case.shape);
    // the target is the stream that draws the picture: the first, or the cut-out one
    let target = if case.cut_out { n - 1 } else { 0 };
    let mut frames: Frames = (0..n).map(|i| {
        let size = if case.mixed && i > 0 { (1920, 1080) } else { case.canvas };
        let mut frame = pseudo_frame(i as u8 + 1, size, case.format);
        if case.cut_out && i == target {
            assert!(frame.set_alpha(soft_mask(size.0, size.1)));
        }
        (format!("s{i}"), (frame, Some(1)))
    }).collect();
    let mut comp = Compositor::default();
    comp.prepare(&layout(case.shape, None, case.cut_out), case.canvas);
    let mut totals = Vec::new();
    let mut times = Vec::new();
    for i in 0..WARM_UPS + RUNS {
        let number = i as u64 + 2;
        for (s, (_, n)) in frames.iter_mut() {
            *n = match case.mode {
                Mode::Steady if *s == format!("s{target}") => None,
                Mode::Steady => Some(1),
                _ => Some(number),
            };
        }
        if case.mode == Mode::Moving {
            comp.prepare(&layout(case.shape, Some(i), case.cut_out), case.canvas);
        }
        let started = Instant::now();
        let out = comp.render(&frames, case.format);
        let total = started.elapsed();
        std::hint::black_box(&out);
        if i >= WARM_UPS {
            totals.push(total.as_secs_f64() * 1000.0);
            times.push(comp.last_times());
        }
    }
    totals.sort_by(|a, b| a.total_cmp(b));
    let p50 = |f: fn(&RenderTimes) -> Duration| {
        let mut v: Vec<f64> = times.iter().map(|t| f(t).as_secs_f64() * 1000.0).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    };
    println!(
        "{:<44} total p50 {:>7.2} p90 {:>7.2} max {:>7.2} ms | p50 scale {:>7.2} paste {:>6.2} setup {:>6.2}",
        case.name, totals[RUNS / 2], totals[RUNS * 9 / 10], totals[RUNS - 1], p50(|t| t.scale), p50(|t| t.paste), p50(|t| t.setup),
    );
}

#[test]
#[ignore]
fn compositor_speed() {
    let filter = std::env::var("COMPOSITOR_SPEED_CASE").unwrap_or_default();
    for canvas in [(1920, 1080), (3840, 2160)] {
        let mut cases = Vec::new();
        let mut add = |label: &str, shape, mixed, format, mode: Mode, cut_out| {
            let mode_name = match mode {
                Mode::Steady => "steady",
                Mode::Fresh => "fresh",
                Mode::Moving => "moving",
            };
            let name = format!("{}p {label}{} {mode_name}", canvas.1, if mixed { " mixed" } else { "" });
            cases.push(Case { name, shape, canvas, mixed, format, mode, cut_out });
        };
        let i420 = PixelFormat::I420;
        for mode in [Mode::Steady, Mode::Fresh, Mode::Moving] {
            add("corner2", Shape::Corner, false, i420, mode, false);
        }
        add("corner2 rgba", Shape::Corner, false, PixelFormat::Rgba, Mode::Steady, false);
        add("corner2", Shape::Corner, true, i420, Mode::Steady, false);
        add("split2", Shape::Split, false, i420, Mode::Steady, false);
        for (label, shape) in [("tiles3", Shape::Three), ("tiles4", Shape::Four)] {
            for mixed in [false, true] {
                add(label, shape, mixed, i420, Mode::Steady, false);
                add(label, shape, mixed, i420, Mode::Moving, false);
            }
        }
        add("split2 cutout", Shape::Split, false, i420, Mode::Steady, true);
        add("split2 cutout", Shape::Split, false, i420, Mode::Moving, true);
        for case in cases.iter().filter(|c| c.name.contains(&filter)) {
            run(case);
        }
    }
}
