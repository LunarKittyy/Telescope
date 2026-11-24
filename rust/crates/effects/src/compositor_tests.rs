//! The Composite plugin's drawing tests. Frames are BGRA here, so a colour reads as the BGR Python's do.

use super::*;
use crate::composite::{big_and_small, grid, Stream, DEFAULT_BACKGROUND};
use std::time::Instant;
use telescope_core::testing::scratch_dir;

fn solid(bgr: [u8; 3], (h, w): (u32, u32)) -> Frame {
    Frame::new(w, h, PixelFormat::Bgra, [bgr[0], bgr[1], bgr[2], 255].repeat((w * h) as usize))
}

/// out[y, x] as B, G, R.
fn px(frame: &Frame, y: usize, x: usize) -> [u8; 3] {
    let at = (y * frame.width as usize + x) * 4;
    match frame.format {
        PixelFormat::Bgra => [frame.data[at], frame.data[at + 1], frame.data[at + 2]],
        _ => {
            let rgb = to_rgb(frame).unwrap();
            let at = (y * frame.width as usize + x) * 3;
            [rgb[at + 2], rgb[at + 1], rgb[at]]
        }
    }
}

fn frames(list: Vec<(&str, Frame, Option<u64>)>) -> Frames {
    list.into_iter().map(|(s, f, n)| (s.to_string(), (f, n))).collect()
}

fn render(layout: &Layout, frames: &Frames, size: (u32, u32)) -> Frame {
    let mut comp = Compositor::default();
    comp.prepare(layout, size);
    comp.render(frames, PixelFormat::Bgra).unwrap()
}

fn t(sid: &str, x: f64, y: f64, w: f64, h: f64) -> Tile {
    Tile::boxed(sid, "", x, y, w, h)
}

/// A picture file the test reader opens: width and height, then BGRA.
fn write_picture(path: &std::path::Path, bgr: [u8; 3], (h, w): (u32, u32)) {
    let mut bytes = Vec::new();
    bytes.extend(w.to_le_bytes());
    bytes.extend(h.to_le_bytes());
    bytes.extend(solid(bgr, (h, w)).data.iter());
    std::fs::write(path, bytes).unwrap();
}

fn reader() -> PictureReader {
    Arc::new(|path: &str| {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let w = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let h = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        Ok(Frame::new(w, h, PixelFormat::Bgra, bytes[8..].to_vec()))
    })
}

#[test]
fn each_stream_lands_in_its_box_over_the_background_back_to_front() {
    let layout = Layout { background: "#0000ff".into(), ..Layout::of(vec![t("a", 0.0, 0.0, 0.5, 1.0), t("b", 0.25, 0.25, 0.5, 0.5)]) };
    let out = render(&layout, &frames(vec![("a", solid([0, 255, 0], (1080, 1920)), None), ("b", solid([0, 0, 255], (1080, 1920)), None)]), (320, 180));
    assert_eq!(px(&out, 90, 20), [0, 255, 0]);
    // b, in front of a
    assert_eq!(px(&out, 90, 160), [0, 0, 255]);
    // the background (#0000ff is BGR 255, 0, 0)
    assert_eq!(px(&out, 10, 300), [255, 0, 0]);
}

#[test]
fn a_stream_that_isnt_running_is_left_out() {
    let layout = Layout { background: "#000000".into(), ..Layout::of(vec![t("a", 0.0, 0.0, 0.5, 0.5), t("gone", 0.5, 0.5, 0.5, 0.5)]) };
    let mut comp = Compositor::default();
    comp.prepare(&layout, (100, 100));
    let f = frames(vec![("a", solid([9, 9, 9], (1080, 1920)), None)]);
    assert_eq!(comp.live_tiles(&f), 1);
    assert_eq!(px(&comp.render(&f, PixelFormat::Bgra).unwrap(), 75, 75), [0, 0, 0]);
}

#[test]
fn rounded_corners_show_the_background_and_blend_at_the_curve() {
    let layout = Layout { background: "#000000".into(), ..Layout::of(vec![Tile { radius: 108, ..Tile::new("a") }]) };
    // radius 20 px at 200 lines
    let out = render(&layout, &frames(vec![("a", solid([255, 255, 255], (1080, 1920)), None)]), (400, 200));
    assert!(px(&out, 0, 0) == [0, 0, 0] && px(&out, 100, 200) == [255, 255, 255]);
    // past the curve
    assert_eq!(px(&out, 0, 20), [255, 255, 255]);
    let smoothed = (0..20).flat_map(|y| (0..20).map(move |x| (y, x))).filter(|&(y, x)| (1..255).contains(&px(&out, y, x)[0])).count();
    // smoothed, not stepped
    assert!(smoothed > 10, "{smoothed}");
}

#[test]
fn a_border_goes_inside_the_box_in_its_colour() {
    let tile = Tile { border: 54, border_colour: "#ff0000".into(), ..t("a", 0.25, 0.25, 0.5, 0.5) };
    let layout = Layout { background: "#000000".into(), ..Layout::of(vec![tile]) };
    // 10 px at 200 lines
    let out = render(&layout, &frames(vec![("a", solid([0, 255, 0], (1080, 1920)), None)]), (400, 200));
    // border, BGR
    assert_eq!(px(&out, 52, 102), [0, 0, 255]);
    assert_eq!(px(&out, 100, 200), [0, 255, 0]);
    assert_eq!(px(&out, 48, 98), [0, 0, 0]);
}

fn halves() -> Frame {
    let mut frame = solid([0, 0, 255], (90, 160));
    let data = frame.data_mut();
    for y in 0..90 {
        for x in 80..160 {
            data[(y * 160 + x) * 4..(y * 160 + x) * 4 + 3].copy_from_slice(&[255, 0, 0]);
        }
    }
    frame
}

#[test]
fn a_stretched_or_cropped_stream_is_drawn_that_way() {
    let frame = halves();
    let mut comp = Compositor::default();
    comp.prepare(&Layout::of(vec![Tile { stretch: true, ..t("a", 0.0, 0.0, 0.5, 1.0) }, Tile { crop_l: 0.5, ..t("b", 0.5, 0.0, 0.5, 1.0) }]), (320, 180));
    let out = comp.render(&frames(vec![("a", frame.clone(), Some(1)), ("b", frame, Some(1))]), PixelFormat::Bgra).unwrap();
    // both halves, squashed into it
    assert!(px(&out, 90, 10) == [0, 0, 255] && px(&out, 90, 150) == [255, 0, 0]);
    // only the right half
    assert!(px(&out, 90, 170) == [255, 0, 0] && px(&out, 90, 310) == [255, 0, 0]);
}

#[test]
fn corner_masks_are_worked_out_once_per_radius() {
    let first = corner_masks(12);
    assert!(Arc::ptr_eq(&corner_masks(12), &first));
    assert!(first.inside(0, 0, 0) == 0.0 && first.inside(0, 11, 11) == 1.0);
    // each corner is the same curve turned to face its own way
    for y in 0..12 {
        for x in 0..12 {
            assert_eq!(first.inside(1, x, y), first.inside(0, 11 - x, y));
            assert_eq!(first.inside(3, x, y), first.inside(0, 11 - x, 11 - y));
            assert_eq!(first.inside(2, x, y), first.inside(0, x, 11 - y));
        }
    }
}

#[test]
fn the_kept_corner_squares_stay_within_their_budget() {
    // radii up to a 4K picture's half height, a few thousand times over, never keep more than the budget
    for r in (1..=1080).step_by(37).chain([1080, 540, 1, 1080]) {
        corner_masks(r);
        assert!(corner_cache_bytes() <= CORNER_CACHE_BYTES, "{r}: {}", corner_cache_bytes());
    }
    // one square a radius, not eight: a 1080 radius is about 4.7 MB
    assert_eq!(corner_masks(1080).bytes(), 1080 * 1080 * 4);
}

#[test]
fn nv12_comes_into_a_tile_as_i420_without_a_trip_through_rgb() {
    let (w, h) = (66usize, 38usize);
    let mut data = noise(w, h, 1, 11);
    data.extend(noise(w / 2, h / 2, 2, 12));
    let nv12 = Frame::new(w as u32, h as u32, PixelFormat::Nv12, data.clone());
    let direct = converted(&nv12, Kind::I420).unwrap();
    assert_eq!(direct.format, PixelFormat::I420);
    assert_eq!(&direct.data[..w * h], &data[..w * h]);
    // the colour samples are the same ones, split into their planes
    let (cw, chh) = (w / 2, h / 2);
    let (u, v) = direct.data[w * h..].split_at(cw * chh);
    for i in 0..cw * chh {
        assert_eq!((u[i], v[i]), (data[w * h + 2 * i], data[w * h + 2 * i + 1]));
    }
    // against the old trip through RGB, on a smooth picture rather than noise: within a few levels in what the viewer sees
    let smooth = |f: &Frame| to_rgb(f).unwrap();
    let gradient: Vec<u8> = (0..w * h).map(|i| (40 + (i % w) * 2 + (i / w)) as u8).collect();
    let mut g = gradient;
    g.extend((0..2 * cw * chh).map(|i| if i % 2 == 0 { 110 + (i / 2 % cw) as u8 } else { 150 - (i / 2 / cw) as u8 }));
    let frame = Frame::new(w as u32, h as u32, PixelFormat::Nv12, g);
    let new = converted(&frame, Kind::I420).unwrap();
    let rgb = to_rgb(&frame).unwrap();
    let mut via = Frame::new(w as u32, h as u32, PixelFormat::I420, vec![255; PixelFormat::I420.size(w as u32, h as u32)]);
    put_rgb(&mut via, &rgb);
    let worst = smooth(&new).iter().zip(smooth(&via).iter()).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
    assert!(worst <= 3, "{worst}");
}

#[test]
fn a_frame_seen_before_isnt_scaled_again() {
    let layout = Layout::of(vec![t("a", 0.0, 0.0, 0.5, 0.5), t("b", 0.5, 0.5, 0.5, 0.5)]);
    let mut comp = Compositor::default();
    comp.prepare(&layout, (320, 180));
    let frame = solid([1, 2, 3], (1080, 1920));
    comp.render(&frames(vec![("a", frame.clone(), Some(41)), ("b", frame.clone(), None)]), PixelFormat::Bgra);
    let scaled_a = comp.scaled_of(0).unwrap();
    comp.render(&frames(vec![("a", frame.clone(), Some(41)), ("b", frame.clone(), None)]), PixelFormat::Bgra);
    assert!(Arc::ptr_eq(&comp.scaled_of(0).unwrap(), &scaled_a));
    comp.render(&frames(vec![("a", frame.clone(), Some(42)), ("b", frame, None)]), PixelFormat::Bgra);
    assert!(!Arc::ptr_eq(&comp.scaled_of(0).unwrap(), &scaled_a));
}

fn noise(w: usize, h: usize, ch: usize, seed: u64) -> Vec<u8> {
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..w * h * ch).map(|_| {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (s >> 33) as u8
    }).collect()
}

#[test]
fn shrinking_goes_in_halves_then_the_rest() {
    let frame = noise(1920, 1080, 3, 1);
    assert_eq!(scale_to(&frame, 1920, 1080, 3, 480, 270).len(), 270 * 480 * 3);
    assert_eq!(scale_to(&frame, 1920, 1080, 3, 333, 187).len(), 187 * 333 * 3);
    assert!(std::ptr::eq(scale_to(&frame, 1920, 1080, 3, 1920, 1080).as_ptr(), frame.as_ptr()), "the same picture, not a copy");
    // exact halves are the 2 x 2 means
    let quarter = scale_to(&frame, 1920, 1080, 3, 960, 540);
    let mean = (frame[0] as u32 + frame[3] as u32 + frame[1920 * 3] as u32 + frame[1920 * 3 + 3] as u32 + 2) / 4;
    assert_eq!(quarter[0] as u32, mean);
}

#[test]
fn the_background_picture_covers_the_picture() {
    let dir = scratch_dir("composite-bg");
    let path = dir.join("bg.png");
    write_picture(&path, [10, 20, 30], (40, 30));
    let mut comp = Compositor::new(reader());
    comp.prepare(&Layout { picture: path.to_string_lossy().into(), ..Layout::default() }, (64, 36));
    let out = comp.render(&Frames::new(), PixelFormat::Bgra).unwrap();
    assert!((out.width, out.height) == (64, 36) && px(&out, 18, 32) == [10, 20, 30]);
    let missing = Layout { picture: dir.join("missing.png").to_string_lossy().into(), background: "#ffffff".into(), ..Layout::default() };
    comp.prepare(&missing, (8, 8));
    assert_eq!(px(&comp.render(&Frames::new(), PixelFormat::Bgra).unwrap(), 0, 0), [255, 255, 255]);
}

#[test]
fn a_missing_background_picture_is_read_again_once_its_there() {
    let dir = scratch_dir("composite-later");
    let path = dir.join("later.png");
    let layout = Layout { picture: path.to_string_lossy().into(), background: "#ffffff".into(), ..Layout::default() };
    let mut comp = Compositor::new(reader());
    comp.prepare(&layout, (8, 8));
    assert_eq!(px(&comp.render(&Frames::new(), PixelFormat::Bgra).unwrap(), 0, 0), [255, 255, 255]);
    write_picture(&path, [10, 20, 30], (8, 8));
    comp.prepare(&layout, (8, 8));
    assert_eq!(px(&comp.render(&Frames::new(), PixelFormat::Bgra).unwrap(), 0, 0), [10, 20, 30]);
}

#[test]
fn moving_a_tile_doesnt_scale_the_background_picture_again() {
    let dir = scratch_dir("composite-move");
    let path = dir.join("bg.png");
    write_picture(&path, [10, 20, 30], (40, 30));
    let picture: String = path.to_string_lossy().into();
    let mut comp = Compositor::new(reader());
    for x in [0.0, 0.1, 0.2] {
        comp.prepare(&Layout { picture: picture.clone(), ..Layout::of(vec![t("a", x, 0.0, 0.5, 0.5)]) }, (64, 36));
    }
    assert_eq!(comp.covered, [(64, 36)]);
    comp.prepare(&Layout { picture, ..Layout::default() }, (32, 18));
    assert_eq!(comp.covered, [(64, 36), (32, 18)]);
}

/// How many pieces of reference frame work f takes, best run against best run (conftest.Timing): halving a 1080p BGR
/// frame and copying it.
fn relative(mut f: impl FnMut()) -> f64 {
    let frame = noise(1920, 1080, 3, 0);
    let reference = || {
        let half = scale_to(&frame, 1920, 1080, 3, 960, 540);
        std::hint::black_box(half.clone());
    };
    f();
    let (mut best_f, mut best_ref) = (f64::MAX, f64::MAX);
    for _ in 0..9 {
        let start = Instant::now();
        reference();
        best_ref = best_ref.min(start.elapsed().as_secs_f64());
        let start = Instant::now();
        f();
        best_f = best_f.min(start.elapsed().as_secs_f64());
    }
    best_f / best_ref
}

#[test]
fn four_1080p_streams_draw_quickly_even_on_a_busy_runner() {
    let streams: Vec<Stream> = ["a", "b", "c", "d"].iter().map(|c| (c.to_string(), c.to_string(), 16.0 / 9.0)).collect();
    let y = |seed| {
        let mut data = noise(1920, 1080, 1, seed);
        data.extend(vec![128u8; 2 * 960 * 540]);
        Frame::new(1920, 1080, PixelFormat::I420, data)
    };
    let all: Frames = ["a", "b", "c", "d"].iter().enumerate().map(|(i, c)| (c.to_string(), (y(i as u64 + 2), None))).collect();
    for tiles in [big_and_small(&streams, 16.0 / 9.0), grid(&streams, 16.0 / 9.0)] {
        let mut comp = Compositor::default();
        comp.prepare(&Layout::of(tiles), (1920, 1080));
        // About 9 pieces of reference work in Python; four times that is a real slowdown, not a busy runner
        let took = relative(|| {
            comp.render(&all, PixelFormat::I420);
        });
        // A debug build measured 17 and 38 here on a quiet machine, so it gets 4 times the room; CI runs debug builds
        let limit = if cfg!(debug_assertions) { 144.0 } else { 36.0 };
        assert!(took < limit, "{took}");
    }
}

#[test]
fn i420_frames_are_drawn_in_their_own_planes() {
    let layout = Layout { background: "#0000ff".into(), ..Layout::of(vec![t("a", 0.0, 0.0, 0.5, 1.0), Tile { radius: 54, border: 20, border_colour: "#ffffff".into(), ..t("b", 0.5, 0.25, 0.4, 0.5) }]) };
    let green = converted(&solid([0, 255, 0], (90, 160)), Kind::I420).unwrap();
    let red = converted(&solid([0, 0, 255], (90, 160)), Kind::I420).unwrap();
    let f = frames(vec![("a", green, Some(1)), ("b", red, Some(2))]);
    let mut comp = Compositor::default();
    comp.prepare(&layout, (320, 180));
    let out = comp.render(&f, PixelFormat::I420).unwrap();
    assert_eq!(out.format, PixelFormat::I420);
    let close = |got: [u8; 3], want: [u8; 3]| got.iter().zip(want).all(|(a, b)| (*a as i32 - b as i32).abs() <= 3);
    assert!(close(px(&out, 90, 40), [0, 255, 0]), "{:?}", px(&out, 90, 40));
    assert!(close(px(&out, 90, 230), [0, 0, 255]), "{:?}", px(&out, 90, 230));
    assert!(close(px(&out, 46, 230), [255, 255, 255]), "{:?}", px(&out, 46, 230));
    assert!(close(px(&out, 2, 300), [255, 0, 0]), "{:?}", px(&out, 2, 300));
    // a stream of another kind is converted on the way in
    let packed = frames(vec![("a", solid([0, 255, 0], (90, 160)), Some(3))]);
    comp.prepare(&Layout::of(vec![t("a", 0.0, 0.0, 1.0, 1.0)]), (64, 36));
    assert!(close(px(&comp.render(&packed, PixelFormat::I420).unwrap(), 18, 32), [0, 255, 0]));
    assert_eq!(bgr(DEFAULT_BACKGROUND), [0x1a, 0x15, 0x15]);
}

// ── Cut out: a tile with an alpha plane over what is on the canvas ──

use telescope_core::frame::Alpha;

/// A w x h BGRA frame of one colour (B, G, R) with an alpha plane from alpha(x, y); a premultiplied one has its colour multiplied by it.
fn cut(bgr: [u8; 3], (h, w): (u32, u32), premultiplied: bool, alpha: impl Fn(usize, usize) -> u8) -> Frame {
    let mut f = solid(bgr, (h, w));
    let mut plane = Vec::new();
    for y in 0..h as usize {
        for x in 0..w as usize {
            let a = alpha(x, y);
            plane.push(a);
            if premultiplied {
                let at = (y * w as usize + x) * 4;
                for (c, v) in bgr.iter().enumerate() {
                    f.data_mut()[at + c] = (*v as f32 * a as f32 / 255.0).round() as u8;
                }
            }
        }
    }
    assert!(f.set_alpha(Arc::new(Alpha::new(w, h, plane, premultiplied))));
    f
}

fn cut_tile(sid: &str, x: f64, y: f64, w: f64, h: f64) -> Tile {
    Tile { cut_out: true, ..t(sid, x, y, w, h) }
}

fn render_in(layout: &Layout, f: &Frames, size: (u32, u32), format: PixelFormat) -> Frame {
    let mut comp = Compositor::default();
    comp.prepare(layout, size);
    comp.render(f, format).unwrap()
}

#[test]
fn a_cut_out_blends_over_the_canvas_like_a_float_reference_within_a_level() {
    let bg_hex = "#2060c0";
    let [bb, bg, br] = bgr(bg_hex);
    let tile_bgr = [200u8, 40, 120];
    // columns in pairs share an alpha, so the colour planes' 2 x 2 mean is exactly it
    let ramp = |x: usize, _y: usize| ((x / 2) * 255 / 79) as u8;
    for premultiplied in [false, true] {
        let layout = Layout { background: bg_hex.into(), ..Layout::of(vec![cut_tile("a", 0.0, 0.0, 1.0, 1.0)]) };
        let f = frames(vec![("a", cut(tile_bgr, (90, 160), premultiplied, ramp), Some(1))]);
        // packed canvas
        let out = render_in(&layout, &f, (160, 90), PixelFormat::Bgra);
        for (y, x) in [(0, 0), (10, 1), (45, 40), (80, 100), (89, 159)] {
            let a = ramp(x, y) as f32 / 255.0;
            let got = px(&out, y, x);
            for c in 0..3 {
                let want = tile_bgr[c] as f32 * a + [bb, bg, br][c] as f32 * (1.0 - a);
                assert!((got[c] as f32 - want).abs() <= 1.0, "premultiplied {premultiplied} at {x},{y} channel {c}: {} against {want}", got[c]);
            }
        }
        assert!(out.data.as_chunks::<4>().0.iter().all(|p| p[3] == 255), "the canvas stays opaque");
        // I420 canvas: the luma and both colour planes against the same blend of the planes it was drawn from
        let out = render_in(&layout, &f, (160, 90), PixelFormat::I420);
        let tile_planes = converted(&solid(tile_bgr, (90, 160)), Kind::I420).unwrap();
        let back = colour_planes(Kind::I420, [br, bg, bb]);
        let (w, h) = (160usize, 90usize);
        let cw = w / 2;
        for (plane, (at, pw, ph)) in [(0usize, (0usize, w, h)), (1, (w * h, cw, h / 2)), (2, (w * h + cw * (h / 2), cw, h / 2))] {
            for (y, x) in [(0, 0), (1, 3), (ph / 2, pw / 3), (ph - 1, pw - 1)] {
                let alpha_x = if plane == 0 { x } else { 2 * x };
                let a = ramp(alpha_x, 0) as f32 / 255.0;
                let want = tile_planes.data[at + y * pw + x] as f32 * a + back[plane] as f32 * (1.0 - a);
                let got = out.data[at + y * pw + x] as f32;
                assert!((got - want).abs() <= 1.0, "premultiplied {premultiplied} plane {plane} at {x},{y}: {got} against {want}");
            }
        }
    }
}

#[test]
fn cut_out_bands_and_alpha_runs_match_the_scalar_blend() {
    fn next(state: &mut u32) -> u8 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        (*state >> 24) as u8
    }

    let mut state = 0x2468_ace1u32;
    let cases = [
        (1, 1, (0, 0, 1, 1)),
        (9, 7, (1, 1, 7, 5)),
        (13, 11, (2, 3, 9, 7)),
        (19, 13, (3, 2, 11, 9)),
        (17, 15, (0, 0, 17, 15)),
        (15, 17, (1, 1, 14, 16)),
        (65, 33, (1, 1, 63, 31)),
        (1031, 2063, (1, 3, 1027, 2059)),
    ];
    for (cw, chh, bx) in cases {
        let (x, y, w, h) = bx;
        let radii: &[usize] = if cw > 1000 { &[0, 37] } else { &[0, 1, 7, usize::MAX] };
        for kind in [Kind::I420, Kind::Rgba, Kind::Bgra] {
            for &radius in radii {
                let patterns = if cw > 1000 { 2 } else { 5 };
                for pattern in 0..patterns {
                    let luma: Vec<u8> = (0..w * h).map(|i| {
                        let random = next(&mut state);
                        match pattern {
                            0 => random,
                            4 => match i % 7 { 0 | 1 => 0, 2 | 3 => 255, _ => random },
                            1 => match ((i % w) / 97 + (i / w) / 31) % 3 {
                                0 => 0,
                                1 => 255,
                                _ => random,
                            },
                            2 => 0,
                            _ => 255,
                        }
                    }).collect();
                    let mut alpha = vec![luma];
                    if kind == Kind::I420 {
                        alpha.push(chroma_alpha(&alpha[0], bx));
                    }
                    let mut picture = Vec::new();
                    let specs = planes(kind, cw, chh);
                    for (p, plane) in specs.iter().enumerate() {
                        let (x0, y0, x1, y1) = if plane.w == cw { (x, y, x + w, y + h) } else { plane_box(2, bx) };
                        let mut values: Vec<u8> = (0..(x1 - x0) * (y1 - y0) * plane.ch).map(|_| next(&mut state)).collect();
                        // Also compare arbitrary colour bytes at zero alpha, without assuming valid premultiplication.
                        if pattern != 4 {
                            multiply(&mut values, &alpha[p.min(1)], plane.ch);
                        }
                        picture.push(values);
                    }
                    let bytes = specs.iter().map(|plane| plane.w * plane.h * plane.ch).sum::<usize>();
                    let mut actual: Vec<u8> = (0..bytes).map(|_| next(&mut state)).collect();
                    let mut expected = actual.clone();
                    paste_over_reference(&mut expected, kind, cw, chh, &picture, &alpha, bx, radius);
                    paste_over(&mut actual, kind, cw, chh, &picture, &alpha, bx, radius);
                    assert_eq!(actual, expected, "{kind:?} {cw}x{chh}, box {bx:?}, radius {radius}, pattern {pattern}");
                }
            }
        }
    }
}

#[test]
fn a_cut_out_shows_what_is_under_it_where_the_alpha_is_nothing() {
    let half = |x: usize, _y: usize| if x < 80 { 0 } else { 255 };
    for format in [PixelFormat::Bgra, PixelFormat::I420] {
        let layout = Layout { background: "#000000".into(), ..Layout::of(vec![t("under", 0.0, 0.0, 1.0, 1.0), cut_tile("over", 0.0, 0.0, 1.0, 1.0)]) };
        let f = frames(vec![("under", solid([0, 255, 0], (90, 160)), Some(1)), ("over", cut([0, 0, 255], (90, 160), false, half), Some(2))]);
        let out = render_in(&layout, &f, (160, 90), format);
        let close = |got: [u8; 3], want: [u8; 3]| got.iter().zip(want).all(|(a, b)| (*a as i32 - b as i32).abs() <= 3);
        // nothing of the tile on the left, all of it on the right
        let rgb = to_rgb(&out).unwrap();
        let at = |y: usize, x: usize| [rgb[(y * 160 + x) * 3 + 2], rgb[(y * 160 + x) * 3 + 1], rgb[(y * 160 + x) * 3]];
        assert!(close(at(45, 20), [0, 255, 0]), "{format:?} {:?}", at(45, 20));
        assert!(close(at(45, 140), [0, 0, 255]), "{format:?} {:?}", at(45, 140));
    }
}

#[test]
fn a_tile_without_a_cut_out_or_with_alpha_it_ignores_draws_the_same_bytes_as_before() {
    let layout = |cut_out: bool| Layout {
        background: "#102030".into(),
        ..Layout::of(vec![
            Tile { radius: 20, border: 6, cut_out, ..t("a", 0.1, 0.1, 0.5, 0.6) },
            Tile { radius: 12, cut_out, ..t("b", 0.4, 0.3, 0.55, 0.6) },
        ])
    };
    let plain = |n: u64| frames(vec![("a", solid([10, 200, 90], (90, 160)), Some(n)), ("b", solid([220, 30, 40], (90, 160)), Some(n + 1))]);
    let with_alpha = |n: u64| frames(vec![
        ("a", cut([10, 200, 90], (90, 160), false, |x, _| (x * 255 / 159) as u8), Some(n)),
        ("b", cut([220, 30, 40], (90, 160), false, |_, y| (y * 255 / 89) as u8), Some(n + 1)),
    ]);
    for format in [PixelFormat::Bgra, PixelFormat::Rgba, PixelFormat::I420] {
        let before = render_in(&layout(false), &plain(1), (320, 180), format);
        // alpha in the frames, cut out off
        assert_eq!(render_in(&layout(false), &with_alpha(1), (320, 180), format).data, before.data, "{format:?} cut out off, frames with alpha");
    }
}

#[test]
fn a_cut_out_tile_draws_nothing_until_its_stream_has_an_alpha() {
    let layout = |tiles: Vec<Tile>| Layout { background: "#102030".into(), ..Layout::of(tiles) };
    let tile = |cut_out: bool| Tile { radius: 20, border: 6, cut_out, ..t("a", 0.1, 0.1, 0.5, 0.6) };
    let plain = frames(vec![("a", solid([10, 200, 90], (90, 160)), Some(1))]);
    for format in [PixelFormat::Bgra, PixelFormat::Rgba, PixelFormat::I420] {
        let empty = render_in(&layout(vec![]), &plain, (320, 180), format);
        let waiting = render_in(&layout(vec![tile(true)]), &plain, (320, 180), format);
        assert_eq!(waiting.data, empty.data, "{format:?}: no box and no border box before the alpha comes");
        // and the same tile fading in draws nothing either
        let mut comp = Compositor::default();
        comp.prepare(&layout(vec![tile(true)]), (320, 180));
        let plan = comp.plan.clone().unwrap();
        let background = comp.background_planes(&plan, Kind::of(format));
        let mut out = comp.copy_of(&background);
        let items = [Item { tile: Cow::Borrowed(&plan.layout.tiles[0]), placed: plan.boxes[0], alpha: 0.5, key: 5000 }];
        comp.draw_items(&mut out, Kind::of(format), plan.size, &items, &plain);
        assert_eq!(out, *background, "{format:?}: nothing drawn mid-fade");
    }
}

#[test]
fn a_cut_out_tile_with_no_alpha_is_drawn_plain_without_a_border_box_after_a_second() {
    let layout = |cut_out: bool| Layout { background: "#102030".into(), ..Layout::of(vec![Tile { radius: 20, border: 6, border_colour: "#ff0000".into(), cut_out, ..t("a", 0.1, 0.1, 0.5, 0.6) }]) };
    let plain = frames(vec![("a", solid([10, 200, 90], (90, 160)), Some(1))]);
    let masked = frames(vec![("a", cut([10, 200, 90], (90, 160), false, |_, _| 255), Some(2))]);
    let format = PixelFormat::Bgra;
    let size = (320, 180);
    let empty = render_in(&Layout { background: "#102030".into(), ..Layout::of(vec![]) }, &plain, size, format);
    let mut comp = Compositor::default();
    comp.prepare(&layout(true), size);
    let s = 1_000_000_000u64;
    assert_eq!(comp.render_at(&plain, format, 10 * s).unwrap().data, empty.data, "blank when first drawn");
    assert_eq!(comp.render_at(&plain, format, 10 * s + s / 2).unwrap().data, empty.data, "blank within the grace");
    let late = comp.render_at(&plain, format, 11 * s).unwrap();
    assert_ne!(late.data, empty.data, "drawn after the grace");
    // plain picture in the inner box, no red border box: the border's room stays the canvas
    let border_off = render_in(&layout(false), &plain, size, format);
    assert!(!late.data.chunks(4).any(|p| p[2] > 200 && p[1] < 40 && p[0] < 40), "no red border box");
    assert!(border_off.data.chunks(4).any(|p| p[2] > 200 && p[1] < 40 && p[0] < 40), "a plain tile does have one");
    // an alpha arriving resets it: without one again, the tile is blank for the grace again
    comp.render_at(&masked, format, 12 * s).unwrap();
    assert_eq!(comp.render_at(&plain, format, 12 * s + s / 10).unwrap().data, empty.data, "blank again after an alpha came");
    // so does the tile stopping being cut out
    comp.prepare(&layout(false), size);
    comp.render_at(&plain, format, 13 * s).unwrap();
    comp.prepare(&layout(true), size);
    assert_eq!(comp.render_at(&plain, format, 13 * s + s / 10).unwrap().data, empty.data, "blank again after it was switched off and on");
}

#[test]
fn a_transition_names_the_cut_outs_it_still_draws_from_the_picture_it_leaves() {
    let spec = Spec { kind: transition::Kind::Crossfade, ms: 500, ease: transition::Ease::Even };
    let mut comp = Compositor::default();
    comp.prepare(&Layout::of(vec![cut_tile("a", 0.0, 0.0, 0.5, 1.0), t("b", 0.5, 0.0, 0.5, 1.0)]), (160, 90));
    assert!(comp.leaving_cut_outs().is_empty());
    comp.prepare_with(&Layout::of(vec![t("a", 0.0, 0.0, 1.0, 1.0), t("b", 0.7, 0.7, 0.25, 0.25)]), (160, 90), spec, 0);
    assert_eq!(comp.leaving_cut_outs(), BTreeSet::from(["a".to_string()]));
    comp.render_at(&Frames::new(), PixelFormat::Bgra, 1_000_000_000);
    assert_eq!(comp.leaving_cut_outs().len(), 1);
    comp.render_at(&Frames::new(), PixelFormat::Bgra, 2_000_000_000);
    assert!(comp.leaving_cut_outs().is_empty(), "gone when the transition has ended");
}

#[test]
fn rounded_corners_fade_the_cut_out_into_the_canvas() {
    // an opaque cut-out rounds its corners the way a plain tile does, give or take a level
    for format in [PixelFormat::Bgra, PixelFormat::I420] {
        let layout = |cut_out: bool, border: i64| Layout { background: "#202020".into(), ..Layout::of(vec![Tile { radius: 40, border, border_colour: "#ff0000".into(), cut_out, ..t("a", 0.1, 0.1, 0.8, 0.8) }]) };
        let f = frames(vec![("a", cut([10, 200, 90], (90, 160), false, |_, _| 255), Some(1))]);
        let size = (320, 180);
        let plain = render_in(&layout(false, 0), &f, size, format);
        let cut_out = render_in(&layout(true, 0), &f, size, format);
        let worst = plain.data.iter().zip(cut_out.data.iter()).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst <= 1, "{format:?}: off by {worst}");
        // a see-through one with a border has the canvas in the box and no border colour in it
        let clear = frames(vec![("a", cut([10, 200, 90], (90, 160), false, |_, _| 0), Some(1))]);
        let none = to_rgb(&render_in(&layout(true, 8), &clear, size, format)).unwrap();
        for (x, y) in [(160, 90), (34, 90), (160, 20), (40, 36)] {
            let at = (y * 320 + x) * 3;
            assert!(none[at..at + 3].iter().all(|v| (*v as i32 - 0x20).abs() <= 2), "{format:?} at {x},{y}: {:?}", &none[at..at + 3]);
        }
    }
}

#[test]
fn a_cut_out_tile_mid_fade_draws_at_the_fades_share_within_a_level() {
    let bg_hex = "#2060c0";
    let [bb, bg, br] = bgr(bg_hex);
    let tile_bgr = [200u8, 40, 120];
    let ramp = |x: usize, _y: usize| ((x / 2) * 255 / 79) as u8;
    let layout = Layout { background: bg_hex.into(), ..Layout::of(vec![cut_tile("a", 0.0, 0.0, 1.0, 1.0)]) };
    let f = frames(vec![("a", cut(tile_bgr, (90, 160), false, ramp), Some(1))]);
    let mut comp = Compositor::default();
    comp.prepare(&layout, (160, 90));
    let plan = comp.plan.clone().unwrap();
    for share in [0.25, 0.5, 0.8] {
        let background = comp.background_planes(&plan, Kind::Bgra);
        let mut out = comp.copy_of(&background);
        let items = [Item { tile: Cow::Borrowed(&plan.layout.tiles[0]), placed: plan.boxes[0], alpha: share, key: 5000 }];
        comp.draw_items(&mut out, Kind::Bgra, plan.size, &items, &f);
        for (y, x) in [(0usize, 0usize), (10, 1), (45, 40), (80, 100), (89, 159)] {
            let a = ramp(x, y) as f64 / 255.0 * share;
            let at = (y * 160 + x) * 4;
            for c in 0..3 {
                let want = f64::from(tile_bgr[c]) * a + f64::from([bb, bg, br][c]) * (1.0 - a);
                assert!((f64::from(out[at + c]) - want).abs() <= 1.0, "share {share} at {x},{y} channel {c}: {} against {want}", out[at + c]);
            }
        }
    }
}

#[test]
fn a_scaled_cut_out_is_kept_for_a_frame_that_comes_again_and_made_again_when_it_is_switched_on() {
    let alpha = |x: usize, _y: usize| (x * 255 / 159) as u8;
    let f = frames(vec![("a", cut([0, 255, 0], (90, 160), false, alpha), Some(7))]);
    let mut comp = Compositor::default();
    let off = Layout::of(vec![t("a", 0.0, 0.0, 1.0, 1.0)]);
    let on = Layout::of(vec![cut_tile("a", 0.0, 0.0, 1.0, 1.0)]);
    comp.prepare(&off, (80, 45));
    let plain = comp.render(&f, PixelFormat::I420).unwrap();
    let kept = comp.scaled_of(0).unwrap();
    comp.prepare(&on, (80, 45));
    let first = comp.render(&f, PixelFormat::I420).unwrap();
    assert_ne!(first.data, plain.data);
    // the same frame number with Cut out switched on is a different picture, so it isn't taken from what was kept
    assert!(!Arc::ptr_eq(&comp.scaled_of(0).unwrap(), &kept));
    let again = comp.scaled_of(0).unwrap();
    assert_eq!(comp.render(&f, PixelFormat::I420).unwrap().data, first.data);
    assert!(Arc::ptr_eq(&comp.scaled_of(0).unwrap(), &again));
}

// ── Item: paste without allocation, against the old version ──

/// The paste this replaced, kept to compare every byte against.
fn paste_reference(canvas: &mut [u8], kind: Kind, cw: usize, chh: usize, content: Content, bx: (usize, usize, usize, usize), radius: usize) {
    let (x, y, w, h) = bx;
    let r = radius.min(w / 2).min(h / 2);
    let masks = if r > 0 { Some(corner_masks(r)) } else { None };
    for (p, plane) in planes(kind, cw, chh).into_iter().enumerate() {
        let full = plane.w == cw;
        let (px0, py0, px1, py1) = if full { (x, y, x + w, y + h) } else { plane_box(2, bx) };
        let (bw, ch) = (px1 - px0, plane.ch);
        let source = |i: usize| -> u8 {
            match &content {
                Content::Colour(c) => c[if full { i % ch } else { p }],
                Content::Picture(planes) => planes[p][i],
            }
        };
        let data = &mut canvas[plane.at..plane.at + plane.w * plane.h * ch];
        if full {
            let corners = [(x, y), (x + w - r, y), (x, y + h - r), (x + w - r, y + h - r)];
            let under: Vec<u8> = if r > 0 {
                corners.iter().flat_map(|&(cx0, cy0)| (0..r).flat_map(move |yy| {
                    let at = ((cy0 + yy) * plane.w + cx0) * ch;
                    at..at + r * ch
                })).map(|at| data[at]).collect()
            } else {
                Vec::new()
            };
            for yy in 0..py1 - py0 {
                let start = ((py0 + yy) * plane.w + px0) * ch;
                let row = &mut data[start..start + bw * ch];
                match &content {
                    Content::Picture(planes) => row.copy_from_slice(&planes[p][yy * bw * ch..(yy + 1) * bw * ch]),
                    Content::Colour(_) => row.iter_mut().enumerate().for_each(|(i, v)| *v = source(i)),
                }
            }
            if let Some(m) = &masks {
                let mut kept = under.into_iter();
                for (corner, &(cx0, cy0)) in corners.iter().enumerate() {
                    for yy in 0..r {
                        for xx in 0..r * ch {
                            let at = ((cy0 + yy) * plane.w + cx0) * ch + xx;
                            let inside = m.inside(corner, xx / ch, yy);
                            data[at] = blend(data[at], kept.next().unwrap_or(0), inside, 1.0 - inside);
                        }
                    }
                }
            }
            continue;
        }
        // A half-size colour plane: samples whose pixels are all well inside are copied, the rest blended by coverage
        let empty: CornerMasks = Default::default();
        let m = masks.as_deref().unwrap_or(&empty);
        let inside = |l: usize, lo: usize, hi: usize| l >= lo && l < hi;
        for cy in py0..py1 {
            let rows: Vec<usize> = [2 * cy, 2 * cy + 1].into_iter().filter(|l| *l < chh).collect();
            let in_v = rows.iter().all(|&l| inside(l, y, y + h));
            let plain_v = rows.iter().all(|&l| inside(l, y + r, y + h - r));
            for cx in px0..px1 {
                let i = (cy - py0) * bw + (cx - px0);
                let at = cy * plane.w + cx;
                let cols: Vec<usize> = [2 * cx, 2 * cx + 1].into_iter().filter(|l| *l < cw).collect();
                let in_h = cols.iter().all(|&l| inside(l, x, x + w));
                let plain_h = cols.iter().all(|&l| inside(l, x + r, x + w - r));
                if in_v && in_h && (plain_v || plain_h) {
                    data[at] = source(i);
                    continue;
                }
                let n = (rows.len() * cols.len()) as f32;
                let sum: f32 = rows.iter().flat_map(|&ly| cols.iter().map(move |&lx| (lx, ly))).map(|(lx, ly)| coverage(lx, ly, bx, r, m)).sum();
                let cov = if n > 0.0 { sum / n } else { 0.0 };
                if cov >= 1.0 {
                    data[at] = source(i);
                } else if cov > 0.0 {
                    data[at] = blend(source(i), data[at], cov, 1.0 - cov);
                }
            }
        }
    }
}

fn paste_cases() -> Vec<((usize, usize), (usize, usize, usize, usize), usize)> {
    let mut cases = Vec::new();
    for size in [(64, 48), (65, 49), (1920, 1080)] {
        for bx in [(0, 0, size.0, size.1), (3, 5, size.0 / 2 + 1, size.1 / 2 + 1), (1, 1, size.0 - 2, size.1 - 3), (size.0 / 3, size.1 / 3, size.0 / 3 + 1, size.1 / 3 + 1)] {
            for r in [0, 1, 5, 40] {
                cases.push((size, bx, r));
            }
        }
    }
    cases
}

#[test]
fn paste_matches_the_allocating_version_byte_for_byte() {
    for kind in [Kind::I420, Kind::Rgba] {
        for ((cw, chh), bx, r) in paste_cases() {
            let len = kind.format().size(cw as u32, chh as u32);
            let canvas = noise(len, 1, 1, 9);
            let pics: Vec<Vec<u8>> = planes(kind, cw, chh).iter().enumerate().map(|(p, pl)| {
                let (px0, py0, px1, py1) = if pl.w == cw { (bx.0, bx.1, bx.0 + bx.2, bx.1 + bx.3) } else { plane_box(2, bx) };
                noise((px1 - px0) * (py1 - py0) * pl.ch, 1, 1, 20 + p as u64)
            }).collect();
            let colour = vec![200, 90, 30, 255];
            for pic in [true, false] {
                let (mut a, mut b) = (canvas.clone(), canvas.clone());
                let content = |_: ()| if pic { Content::Picture(&pics) } else { Content::Colour(colour.clone()) };
                paste(&mut a, kind, cw, chh, content(()), bx, r);
                paste_reference(&mut b, kind, cw, chh, content(()), bx, r);
                assert!(a == b, "{kind:?} {cw}x{chh} {bx:?} r{r} picture {pic}");
            }
        }
    }
}

fn best_of(n: usize, mut f: impl FnMut()) -> f64 {
    (0..n).map(|_| {
        let start = Instant::now();
        f();
        start.elapsed().as_secs_f64() * 1000.0
    }).fold(f64::MAX, f64::min)
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_paste() {
    for (cw, chh) in [(1920usize, 1080usize), (3840, 2160)] {
        for r in [0usize, 24] {
            let bx = (0, 0, cw, chh);
            let canvas = vec![7u8; Kind::I420.format().size(cw as u32, chh as u32)];
            let pics: Vec<Vec<u8>> = planes(Kind::I420, cw, chh).iter().map(|pl| vec![99u8; pl.w * pl.h]).collect();
            let (mut a, mut b) = (canvas.clone(), canvas.clone());
            let new = best_of(5, || paste(&mut a, Kind::I420, cw, chh, Content::Picture(&pics), bx, r));
            let old = best_of(5, || paste_reference(&mut b, Kind::I420, cw, chh, Content::Picture(&pics), bx, r));
            println!("paste {cw}x{chh} r{r}: old {old:.2} ms, new {new:.2} ms");
        }
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_two_tile_render() {
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        for radius in [0i64, 24] {
            let mut tiles = vec![t("a", 0.0, 0.0, 0.5, 1.0), t("b", 0.5, 0.0, 0.5, 1.0)];
            tiles.iter_mut().for_each(|tile| tile.radius = radius);
            let mut data = noise(w as usize, h as usize, 1, 3);
            data.extend(vec![128u8; 2 * (w as usize / 2) * (h as usize / 2)]);
            let frame = Frame::new(w, h, PixelFormat::I420, data);
            let all: Frames = [("a", 1u64), ("b", 2)].iter().map(|(s, n)| (s.to_string(), (frame.clone(), Some(*n)))).collect();
            let mut comp = Compositor::default();
            comp.prepare(&Layout::of(tiles), (w, h));
            comp.render(&all, PixelFormat::I420);
            let took = best_of(5, || { std::hint::black_box(comp.render(&all, PixelFormat::I420)); });
            println!("render two tiles {w}x{h} radius {radius}: {took:.2} ms");
        }
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_two_tile_render_fresh() {
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        for format in [PixelFormat::I420, PixelFormat::Nv12] {
            let tiles = vec![t("a", 0.0, 0.0, 0.5, 1.0), t("b", 0.5, 0.0, 0.5, 1.0)];
            let mut data = noise(w as usize, h as usize, 1, 3);
            data.extend(noise(2 * (w as usize / 2) * (h as usize / 2), 1, 1, 4));
            let frame = Frame::new(w, h, format, data);
            let all: Frames = [("a", None::<u64>), ("b", None)].iter().map(|(s, n)| (s.to_string(), (frame.clone(), *n))).collect();
            let mut comp = Compositor::default();
            comp.prepare(&Layout::of(tiles), (w, h));
            comp.render(&all, PixelFormat::I420);
            let took = best_of(5, || { std::hint::black_box(comp.render(&all, PixelFormat::I420)); });
            println!("render two fresh {format:?} tiles {w}x{h}: {took:.2} ms");
        }
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_linear_scale() {
    for ((w, h), (w2, h2)) in [((1920usize, 1080usize), (1280usize, 720usize)), ((3840, 2160), (2560, 1440)), ((3840, 2160), (1333, 750))] {
        for ch in [1usize, 4] {
            let plane = noise(w, h, ch, 5);
            let took = best_of(5, || { std::hint::black_box(scale_to(&plane, w, h, ch, w2, h2)); });
            println!("linear scale {w}x{h} x{ch} -> {w2}x{h2}: {took:.2} ms");
        }
    }
}

/// The scaling this replaced, kept to compare every byte against.
fn linear_reference(src: Region, w2: usize, h2: usize) -> Vec<u8> {
    let ch = src.ch;
    let (xs, ys) = (linear_taps(src.w, w2), linear_taps(src.h, h2));
    let n = w2 * ch;
    let row = |y: usize| -> Vec<i32> {
        let mut out = vec![0i32; n];
        for (dx, &(x0, x1, a0, a1)) in xs.iter().enumerate() {
            for c in 0..ch {
                out[dx * ch + c] = src.at(x0, y, c) as i32 * a0 + src.at(x1, y, c) as i32 * a1;
            }
        }
        out
    };
    let mut rows: HashMap<usize, Vec<i32>> = HashMap::new();
    let mut out = vec![0u8; n * h2];
    let vec16 = n / 16 * 16;
    let vec_end = if (vec16 as i64) < n as i64 - 8 { vec16 + 8 } else { vec16 };
    for (dy, &(y0, y1, b0, b1)) in ys.iter().enumerate() {
        rows.entry(y0).or_insert_with(|| row(y0));
        rows.entry(y1).or_insert_with(|| row(y1));
        rows.retain(|k, _| *k >= y0);
        let (s0, s1) = (&rows[&y0], &rows[&y1]);
        let line = &mut out[dy * n..(dy + 1) * n];
        for i in 0..n {
            line[i] = if i < vec_end {
                let hi = |b: i32, s: i32| (b * ((s >> 4).clamp(-32768, 32767))) >> 16;
                ((hi(b0, s0[i]) + hi(b1, s1[i]) + 2) >> 2).clamp(0, 255) as u8
            } else {
                ((b0 as i64 * s0[i] as i64 + b1 as i64 * s1[i] as i64 + (1 << 21)) >> 22).clamp(0, 255) as u8
            };
        }
    }
    out
}

#[test]
fn linear_scaling_is_the_same_as_the_version_it_replaced() {
    for ch in [1usize, 3, 4] {
        for ((w, h), (w2, h2)) in [((64usize, 48usize), (40usize, 30usize)), ((65, 49), (130, 98)), ((200, 120), (133, 77)), ((1920, 1080), (1280, 720)), ((31, 17), (31, 40)), ((9, 9), (1, 1)), ((300, 200), (299, 3))] {
            let data = noise(w + 3, h + 2, ch, (w * h) as u64);
            let region = Region { data: &data, stride: w + 3, ch, x0: 2, y0: 1, w, h };
            assert!(linear(region, w2, h2) == linear_reference(region, w2, h2), "{ch} channels {w}x{h} to {w2}x{h2}");
        }
    }
}

/// The scalar halving this replaced, kept to compare every byte against.
fn halved_reference(src: Region) -> Vec<u8> {
    let (w2, h2, ch) = (src.w / 2, src.h / 2, src.ch);
    let mut out = vec![0u8; w2 * h2 * ch];
    for y in 0..h2 {
        for x in 0..w2 {
            for c in 0..ch {
                let sum = src.at(2 * x, 2 * y, c) as u32 + src.at(2 * x + 1, 2 * y, c) as u32 + src.at(2 * x, 2 * y + 1, c) as u32 + src.at(2 * x + 1, 2 * y + 1, c) as u32;
                out[(y * w2 + x) * ch + c] = ((sum + 2) >> 2) as u8;
            }
        }
    }
    out
}

/// scale_region as it was: the scalar halves repeated, then the scalar linear step.
fn scale_reference(src: Region, w: usize, h: usize) -> Vec<u8> {
    if (src.w, src.h) == (w, h) {
        return src.copy();
    }
    if src.w < 2 * w || src.h < 2 * h {
        return linear_reference(src, w, h);
    }
    let mut image = halved_reference(src);
    let (mut iw, mut ih) = (src.w / 2, src.h / 2);
    while iw >= 2 * w && ih >= 2 * h {
        image = halved_reference(Region { data: &image, stride: iw, ch: src.ch, x0: 0, y0: 0, w: iw, h: ih });
        (iw, ih) = (iw / 2, ih / 2);
    }
    if (iw, ih) == (w, h) {
        return image;
    }
    linear_reference(Region { data: &image, stride: iw, ch: src.ch, x0: 0, y0: 0, w: iw, h: ih }, w, h)
}

#[test]
fn scaling_a_region_is_the_same_as_the_scalar_halves_and_linear_step() {
    // crops of bigger planes, odd sizes, every channel count the compositor has, an alpha plane's one byte, and banded ones
    let cases = [((64usize, 48usize), (16usize, 12usize)), ((65, 49), (8, 6)), ((200, 121), (33, 20)), ((131, 77), (130, 76)), ((97, 31), (49, 15)), ((401, 301), (50, 37)), ((3000, 1200), (700, 301)), ((2000, 1500), (500, 375)), ((1000, 300), (257, 299)), ((15, 9), (7, 4))];
    for ch in [1usize, 3, 4] {
        for (i, &((w, h), (w2, h2))) in cases.iter().enumerate() {
            let data = noise(w + 5, h + 3, ch, (i * 31 + ch) as u64);
            let region = Region { data: &data, stride: w + 5, ch, x0: 3, y0: 2, w, h };
            assert!(scale_region(region, w2, h2) == scale_reference(region, w2, h2), "{ch} channels {w}x{h} to {w2}x{h2}");
        }
    }
}

#[test]
fn the_plain_loops_scale_a_region_to_the_same_bytes_as_avx2_and_the_reference() {
    let mut seed = 11u64;
    let mut next = |m: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as usize % m
    };
    // one-pixel wide, tall and both, then random crops of bigger planes, a few banded
    let mut cases = vec![((1usize, 1usize), (1usize, 1usize)), ((1, 1), (3, 2)), ((1, 40), (1, 7)), ((40, 1), (7, 1)), ((1, 9), (4, 1)), ((2, 2), (1, 1)), ((1, 700), (1, 90)), ((700, 1), (90, 1)), ((2400, 900), (300, 113))];
    cases.extend((0..40).map(|_| ((1 + next(150), 1 + next(150)), (1 + next(170), 1 + next(170)))));
    for ch in [1usize, 3, 4] {
        for (i, &((w, h), (w2, h2))) in cases.iter().enumerate() {
            let data = noise(w + 3, h + 2, ch, (i * 17 + ch) as u64);
            let region = Region { data: &data, stride: w + 3, ch, x0: 2, y0: 1, w, h };
            let case = format!("{ch} channels {w}x{h} to {w2}x{h2}");
            let (wide, plain) = (scale_region(region, w2, h2), telescope_core::image::scalar_switch::plain(|| scale_region(region, w2, h2)));
            assert!(plain == wide, "plain and avx2 differ, {case}");
            assert!(plain == scale_reference(region, w2, h2), "plain and reference differ, {case}");
        }
    }
}

// ── Transitions ───────────────────────────────────────────────────────────

mod transitions {
    use super::*;
    use crate::transition::{Ease, Kind as How, Spec};

    const SIZE: (u32, u32) = (160, 90);

    fn spec(kind: How) -> Spec {
        Spec { kind, ms: 1000, ease: Ease::Even }
    }

    fn two() -> Frames {
        frames(vec![("a", solid([200, 0, 0], (90, 160)), None), ("b", solid([0, 200, 0], (90, 160)), None)])
    }

    fn old_layout() -> Layout {
        Layout { tiles: vec![t("a", 0.0, 0.0, 1.0, 1.0), t("b", 0.7, 0.7, 0.25, 0.25)], ..Layout::default() }
    }

    fn new_layout() -> Layout {
        Layout { tiles: vec![t("b", 0.0, 0.0, 1.0, 1.0), t("a", 0.7, 0.7, 0.25, 0.25)], ..Layout::default() }
    }

    fn started(kind: How, from: &Layout, to: &Layout) -> Compositor {
        let mut comp = Compositor::default();
        comp.prepare(from, SIZE);
        comp.prepare_with(to, SIZE, spec(kind), 1_000);
        // the clock starts with the first picture
        comp.render_at(&two(), PixelFormat::Bgra, 1_000);
        comp
    }

    fn at(comp: &mut Compositor, t: f64) -> Frame {
        comp.render_at(&two(), PixelFormat::Bgra, 1_000 + (t * 1e9) as u64).unwrap()
    }

    #[test]
    fn every_kind_starts_on_the_old_picture_and_ends_on_the_new_one() {
        let (from, to) = (old_layout(), new_layout());
        let (old, new) = (render(&from, &two(), SIZE), render(&to, &two(), SIZE));
        for kind in How::ALL {
            let mut comp = started(kind, &from, &to);
            let first = at(&mut comp, 0.0);
            if kind == How::Cut {
                assert_eq!(first.data, new.data, "a cut is the new picture at once");
                continue;
            }
            let diff = |a: &Frame, b: &Frame| a.data.iter().zip(b.data.iter()).filter(|(x, y)| x != y).count();
            // a first frame is the old picture, give or take the pixels a glide's tiles round differently
            assert!(diff(&first, &old) < first.data.len() / 50, "{kind:?} begins on the old picture: {}", diff(&first, &old));
            let last = at(&mut comp, 1.0);
            assert_eq!(last.data, new.data, "{kind:?} ends on the new picture");
            assert!(!comp.transitioning(1_000 + 2_000_000_000));
        }
    }

    #[test]
    fn a_cut_a_same_arrangement_and_a_new_size_start_nothing() {
        let (from, to) = (old_layout(), new_layout());
        assert!(!started(How::Cut, &from, &to).transitioning(1_000));
        assert!(!started(How::Glide, &from, &from).transitioning(1_000));
        let mut comp = Compositor::default();
        comp.prepare(&from, (160, 90));
        comp.prepare_with(&to, (320, 180), spec(How::Glide), 1_000);
        assert!(!comp.transitioning(1_000));
        // nothing drawn before: it begins on the new layout
        let mut fresh = Compositor::default();
        fresh.prepare_with(&to, SIZE, spec(How::Glide), 1_000);
        assert!(!fresh.transitioning(1_000));
    }

    #[test]
    fn a_crossfade_halfway_is_the_two_pictures_mixed_evenly() {
        let (from, to) = (old_layout(), new_layout());
        let mut comp = started(How::Crossfade, &from, &to);
        let mid = at(&mut comp, 0.5);
        let (old, new) = (render(&from, &two(), SIZE), render(&to, &two(), SIZE));
        for (y, x) in [(10, 10), (40, 80), (85, 150)] {
            for c in 0..3 {
                let want = (i32::from(px(&old, y, x)[c]) + i32::from(px(&new, y, x)[c])) / 2;
                assert!((i32::from(px(&mid, y, x)[c]) - want).abs() <= 2, "({y}, {x}) channel {c}");
            }
        }
    }

    /// How many bytes of a and b differ.
    fn differ(a: &Frame, b: &Frame) -> usize {
        a.data.iter().zip(b.data.iter()).filter(|(x, y)| x != y).count()
    }

    #[test]
    fn a_transition_interrupted_goes_on_from_where_it_is() {
        let (from, to) = (old_layout(), new_layout());
        for kind in [How::Glide, How::Shuffle, How::Crossfade, How::Push, How::Zoom] {
            let mut comp = started(kind, &from, &to);
            at(&mut comp, 0.0);
            let mid = at(&mut comp, 0.3);
            // back to where it came from, a moment later: the first picture is the one it was showing
            comp.prepare_with(&from, SIZE, spec(kind), 1_000 + 300_000_000);
            let again = comp.render_at(&two(), PixelFormat::Bgra, 1_000 + 300_000_000).unwrap();
            assert!(differ(&mid, &again) < mid.data.len() / 40, "{kind:?} jumped: {} of {}", differ(&mid, &again), mid.data.len());
            let end = comp.render_at(&two(), PixelFormat::Bgra, 1_000 + 300_000_000 + 2_000_000_000).unwrap();
            assert_eq!(end.data, render(&from, &two(), SIZE).data, "{kind:?} ends where it was sent");
        }
    }

    #[test]
    fn a_tile_that_was_fading_out_does_not_jump_to_full_when_interrupted() {
        // b leaves and c arrives: both part faded at 0.3
        let from = Layout { tiles: vec![t("a", 0.0, 0.0, 0.5, 1.0), t("b", 0.5, 0.0, 0.5, 1.0)], ..Layout::default() };
        let to = Layout { tiles: vec![t("a", 0.0, 0.0, 0.5, 1.0), t("c", 0.5, 0.0, 0.5, 1.0)], ..Layout::default() };
        let live = frames(vec![("a", solid([200, 0, 0], (90, 160)), None), ("b", solid([0, 200, 0], (90, 160)), None), ("c", solid([0, 0, 200], (90, 160)), None)]);
        let mut comp = Compositor::default();
        comp.prepare(&from, SIZE);
        comp.prepare_with(&to, SIZE, spec(How::Glide), 1_000);
        comp.render_at(&live, PixelFormat::Bgra, 1_000);
        let mid = comp.render_at(&live, PixelFormat::Bgra, 1_000 + 300_000_000).unwrap();
        comp.prepare_with(&from, SIZE, spec(How::Glide), 1_000 + 300_000_000);
        let again = comp.render_at(&live, PixelFormat::Bgra, 1_000 + 300_000_000).unwrap();
        assert!(differ(&mid, &again) < mid.data.len() / 40, "{} of {}", differ(&mid, &again), mid.data.len());
    }

    #[test]
    fn a_mix_interrupting_a_glide_leaves_from_the_glide_picture() {
        let (from, to) = (old_layout(), new_layout());
        let mut comp = started(How::Glide, &from, &to);
        let mid = at(&mut comp, 0.3);
        comp.prepare_with(&from, SIZE, spec(How::Crossfade), 1_000 + 300_000_000);
        let again = comp.render_at(&two(), PixelFormat::Bgra, 1_000 + 300_000_000).unwrap();
        assert!(differ(&mid, &again) < mid.data.len() / 40, "{} of {}", differ(&mid, &again), mid.data.len());
    }

    #[test]
    fn preparing_again_in_the_middle_goes_on_to_the_new_target() {
        let (from, to) = (old_layout(), new_layout());
        let mut comp = started(How::Glide, &from, &to);
        at(&mut comp, 0.0);
        comp.prepare(&to, SIZE);
        assert!(comp.has_transition() && comp.transitioning(1_000 + 100_000_000), "a stream joining doesn't end it");
        let other = Layout { tiles: vec![t("a", 0.0, 0.0, 0.5, 1.0), t("b", 0.5, 0.0, 0.5, 1.0)], ..Layout::default() };
        comp.prepare(&other, SIZE);
        let end = comp.render_at(&two(), PixelFormat::Bgra, 1_000 + 2_000_000_000).unwrap();
        assert_eq!(end.data, render(&other, &two(), SIZE).data, "an edit is what it ends on");
        comp.prepare(&to, (320, 180));
        assert!(!comp.has_transition(), "a new size ends it");
    }

    #[test]
    fn the_clock_starts_with_the_first_picture_drawn() {
        let (from, to) = (old_layout(), new_layout());
        let mut comp = Compositor::default();
        comp.prepare(&from, SIZE);
        comp.prepare_with(&to, SIZE, spec(How::Crossfade), 1_000);
        // asked for long after, as when the first picture was slow to come
        comp.render_at(&two(), PixelFormat::Bgra, 5_000_000_000).unwrap();
        assert!(comp.transitioning(5_000_000_000 + 500_000_000));
        assert!(!comp.transitioning(5_000_000_000 + 1_500_000_000));
    }

    #[test]
    fn tiles_that_change_places_in_the_stack_swap_where_they_overlap_least() {
        // a and b cross over each other: a goes under b and b over a
        let from = vec![t("a", 0.0, 0.2, 0.4, 0.4), t("b", 0.6, 0.2, 0.4, 0.4)];
        let to = vec![t("b", 0.0, 0.2, 0.4, 0.4), t("a", 0.6, 0.2, 0.4, 0.4)];
        let order = |p: f64| crate::transition::poses(How::Glide, &from, &to, p, Ease::Even, 16.0 / 9.0).iter().map(|x| x.tile.source_id.clone()).collect::<Vec<_>>();
        assert_eq!(order(0.0), ["a", "b"], "the old stack at first");
        assert_eq!(order(1.0), ["b", "a"], "the new one at the end");
        // where the order changes the two are not on top of each other
        let swap = (0..=100).map(|i| i as f64 / 100.0).find(|p| order(*p) != order(0.0)).unwrap();
        let boxes: Vec<(f64, f64, f64, f64)> = crate::transition::poses(How::Glide, &from, &to, swap, Ease::Even, 16.0 / 9.0).iter().map(|x| (x.tile.x, x.tile.y, x.tile.w, x.tile.h)).collect();
        let (a, b) = (boxes[0], boxes[1]);
        let apart = a.0 + a.2 <= b.0 + 1e-9 || b.0 + b.2 <= a.0 + 1e-9;
        assert!(apart, "swapped at {swap} while overlapping: {boxes:?}");
    }

    #[test]
    fn a_new_tile_fades_in_over_what_is_there() {
        let from = Layout { tiles: vec![t("a", 0.0, 0.0, 1.0, 1.0)], ..Layout::default() };
        let to = Layout { tiles: vec![t("a", 0.0, 0.0, 1.0, 1.0), t("b", 0.0, 0.0, 1.0, 1.0)], ..Layout::default() };
        let mut comp = started(How::Glide, &from, &to);
        let mid = at(&mut comp, 0.4);
        let [b, g, _] = px(&mid, 45, 80);
        // between the blue stream and the green one: some of each
        assert!(b > 20 && b < 190 && g > 20 && g < 190, "{b} {g}");
    }

    #[test]
    fn a_picture_that_changes_size_is_never_half_drawn() {
        let (from, to) = (old_layout(), new_layout());
        let mut comp = started(How::Push, &from, &to);
        for n in 0..=10 {
            let f = at(&mut comp, n as f64 / 10.0);
            assert_eq!((f.width, f.height, f.data.len()), (160, 90, 160 * 90 * 4));
        }
    }

    /// With TRANSITION_SHOTS naming a folder, a contact sheet of every kind's frames to look at.
    #[test]
    fn the_film_of_every_transition() {
        let Ok(dir) = std::env::var("TRANSITION_SHOTS") else { return };
        std::fs::create_dir_all(&dir).unwrap();
        let (w, h) = (320usize, 180usize);
        let pattern = |bgr: [u8; 3], bar: usize| {
            let mut data = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let stripe = ((x + y) / 24) % 2 == 0;
                    let f = if stripe { 1.0 } else { 0.72 };
                    let marker = x > bar * 40 && x < bar * 40 + 28 && y > 60 && y < 120;
                    let c = |v: u8| if marker { 255 } else { (f64::from(v) * f) as u8 };
                    data.extend_from_slice(&[c(bgr[0]), c(bgr[1]), c(bgr[2]), 255]);
                }
            }
            Frame::new(w as u32, h as u32, PixelFormat::Bgra, data)
        };
        let live = frames(vec![
            ("a", pattern([210, 120, 60], 1), None), ("b", pattern([70, 170, 90], 2), None),
            ("c", pattern([60, 90, 200], 3), None), ("d", pattern([180, 80, 170], 4), None),
        ]);
        let from = Layout { tiles: vec![t("a", 0.0, 0.0, 0.62, 0.62), t("b", 0.66, 0.0, 0.34, 0.34), t("c", 0.66, 0.38, 0.34, 0.34), t("d", 0.0, 0.66, 0.3, 0.3)], ..Layout::default() };
        let to = Layout { tiles: vec![t("b", 0.0, 0.0, 0.62, 0.62), t("a", 0.66, 0.0, 0.34, 0.34), t("d", 0.66, 0.38, 0.34, 0.34), t("c", 0.34, 0.66, 0.3, 0.3)], ..Layout::default() };
        let steps = [0.0, 0.12, 0.25, 0.38, 0.5, 0.65, 0.8, 1.0];
        for ease in [Ease::Springy] {
            for kind in How::ALL {
                let mut comp = Compositor::default();
                comp.prepare(&from, (w as u32, h as u32));
                comp.prepare_with(&to, (w as u32, h as u32), Spec { kind, ms: 1000, ease }, 0);
                let (cols, rows) = (4usize, 2usize);
                let mut sheet = vec![30u8; (cols * w + (cols + 1) * 6) * (rows * h + (rows + 1) * 6) * 4];
                let sw = cols * w + (cols + 1) * 6;
                for (n, step) in steps.iter().enumerate() {
                    let frame = comp.render_at(&live, PixelFormat::Bgra, (step * 1e9) as u64).unwrap();
                    let (ox, oy) = (6 + (n % cols) * (w + 6), 6 + (n / cols) * (h + 6));
                    for y in 0..h {
                        for x in 0..w {
                            let (s, d) = ((y * w + x) * 4, ((oy + y) * sw + ox + x) * 4);
                            // BGRA to RGBA for the PNG
                            sheet[d..d + 4].copy_from_slice(&[frame.data[s + 2], frame.data[s + 1], frame.data[s], 255]);
                        }
                    }
                }
                let size = tiny_skia::IntSize::from_wh(sw as u32, (rows * h + (rows + 1) * 6) as u32).unwrap();
                tiny_skia::Pixmap::from_vec(sheet, size).unwrap().save_png(std::path::Path::new(&dir).join(format!("{}.png", kind.id()))).unwrap();
            }
        }
    }

    #[test]
    fn the_iris_and_the_dissolve_by_runs_are_the_same_as_pixel_by_pixel() {
        let (w, h) = (160usize, 90usize);
        let pic = |c: u8| (0..w * h).flat_map(|i| [(i % 251) as u8 ^ c, (i / w) as u8, c, 255]).collect::<Vec<u8>>();
        let (a, b) = (pic(0), pic(120));
        for how in [How::Iris, How::Dissolve] {
            for p in [0.0, 0.1, 0.37, 0.5, 0.83, 1.0] {
                let mut out = Vec::new();
                mix_pictures(&mut out, &mut Vec::new(), Kind::Bgra, (w, h), &a, &b, how, p);
                for y in 0..h {
                    for x in 0..w {
                        let m = (crate::transition::mix_at(how, x as f64, y as f64, (w as f64, h as f64), p) * 256.0).round() as u32;
                        let at = (y * w + x) * 4;
                        for c in 0..4 {
                            let want = ((u32::from(a[at + c]) * (256 - m) + u32::from(b[at + c]) * m + 128) >> 8) as u8;
                            assert_eq!(out[at + c], want, "{how:?} at p = {p}, ({x}, {y}) channel {c}");
                        }
                    }
                }
            }
        }
    }

    /// With TRANSITION_COST set: what a frame costs at 4K, for each kind (run it with an optimised test profile).
    #[test]
    fn the_cost_of_a_4k_transition() {
        if std::env::var("TRANSITION_COST").is_err() {
            return;
        }
        let size = (3840u32, 2160u32);
        let src = |c: u8| {
            let mut data = Vec::with_capacity(1920 * 1080 * 4);
            for y in 0..1080u32 {
                for x in 0..1920u32 {
                    data.extend_from_slice(&[(x / 8) as u8 ^ c, (y / 8) as u8, c.wrapping_add((x + y) as u8), 255]);
                }
            }
            Frame::new(1920, 1080, PixelFormat::Bgra, data)
        };
        let (fa, fb, fc) = (src(10), src(90), src(170));
        // numbered: the same frame again hits what was scaled, a new one asks for it to be scaled again
        let live = |n: u64| frames(vec![("a", fa.clone(), Some(n)), ("b", fb.clone(), Some(n)), ("c", fc.clone(), Some(n))]);
        let from = Layout { tiles: vec![t("a", 0.0, 0.0, 0.66, 0.66), t("b", 0.7, 0.0, 0.3, 0.3), t("c", 0.7, 0.35, 0.3, 0.3)], ..Layout::default() };
        let to = Layout { tiles: vec![t("b", 0.0, 0.0, 0.66, 0.66), t("a", 0.7, 0.0, 0.3, 0.3), t("c", 0.35, 0.7, 0.3, 0.3)], ..Layout::default() };
        for kind in [How::Cut, How::Glide, How::Shuffle, How::Crossfade, How::Push, How::Zoom, How::Bounce, How::Iris, How::Curtain, How::Blinds, How::Pixelate, How::Dissolve] {
            let mut comp = Compositor::default();
            comp.prepare(&from, size);
            let mut n = 0;
            let mut ms = |comp: &mut Compositor, fresh: bool, at: &[f64]| {
                let start = Instant::now();
                for s in at {
                    n += u64::from(fresh);
                    comp.render_at(&live(n), PixelFormat::Bgra, (s * 1e9) as u64).unwrap();
                }
                start.elapsed().as_secs_f64() * 1000.0 / at.len() as f64
            };
            ms(&mut comp, true, &[0.0]);
            let (plain_new, plain_same) = (ms(&mut comp, true, &[0.0; 5]), ms(&mut comp, false, &[0.0; 5]));
            comp.prepare_with(&to, size, Spec { kind, ms: 1000, ease: Ease::Springy }, 0);
            ms(&mut comp, true, &[0.0]);
            let steps = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];
            let (new, same) = (ms(&mut comp, true, &steps), ms(&mut comp, false, &steps));
            eprintln!("4K {:?}: {new:.1} ms a frame in the transition with a new stream frame each time, {same:.1} ms with the same one (plain picture: {plain_new:.1} and {plain_same:.1})", kind);
        }
    }
}

#[test]
fn direct_scaling_preserves_pixels_outside_the_destination_and_matches_the_plane_scaler() {
    let cases = [((17usize, 13usize), (17usize, 13usize)), ((64, 48), (32, 24)), ((65, 49), (8, 6)), ((200, 121), (33, 20)), ((15, 9), (37, 23)), ((1100, 800), (257, 301))];
    for ch in [1, 3, 4] {
        for (i, &((w, h), (tw, th))) in cases.iter().enumerate() {
            let data = noise(w + 7, h + 4, ch, (i * 31 + ch) as u64);
            let region = Region { data: &data, stride: w + 7, ch, x0: 3, y0: 2, w, h };
            let (stride, left) = ((tw + 9) * ch, 3 * ch);
            let mut out = vec![213; stride * (th + 2)];
            scale_region_into(region, tw, th, &mut out[stride..stride * (th + 1)], stride, left);
            let scaled = scale_reference(region, tw, th);
            let mut expected = vec![213; out.len()];
            for y in 0..th {
                expected[(y + 1) * stride + left..(y + 1) * stride + left + tw * ch].copy_from_slice(&scaled[y * tw * ch..(y + 1) * tw * ch]);
            }
            assert_eq!(out, expected, "{ch} channels {w}x{h} to {tw}x{th}");
        }
    }
}

#[test]
fn plain_unnumbered_tiles_draw_the_same_bytes_directly_as_the_cached_plane_path() {
    for format in [PixelFormat::Bgra, PixelFormat::Rgba, PixelFormat::I420, PixelFormat::Nv12] {
        for size in [(100, 80), (101, 81)] {
            for tile in [Tile::new("a"), t("a", 0.2, 0.2, 0.4, 0.4), t("a", 0.01, 0.0125, 0.5, 0.5), Tile { border: 54, ..Tile::new("a") }] {
                let layout = Layout::of(vec![Tile { crop_l: 0.13, crop_b: 0.17, zoom: 1.3, ..tile }]);
                let frame = Frame::new(193, 129, format, noise(format.size(193, 129), 1, 1, 23));
                let fresh = frames(vec![("a", frame.clone(), None)]);
                let cached = frames(vec![("a", frame, Some(41))]);
                let mut direct = Compositor::default();
                direct.prepare(&layout, size);
                let mut reference = Compositor::default();
                reference.prepare(&layout, size);
                assert_eq!(direct.render(&fresh, format).unwrap().data, reference.render(&cached, format).unwrap().data, "{format:?} {size:?}");
                assert!(reference.scaled_of(0).is_some());
                if Kind::of(format) != Kind::I420 || (layout.tiles[0].x == 0.0 && layout.tiles[0].border == 0) {
                    assert!(direct.scaled_of(0).is_none(), "plain tile should draw directly");
                }
            }
        }
    }
}

#[test]
fn scaled_plane_buffers_return_to_the_pool_only_after_the_last_owner_releases_them() {
    let mut held = Vec::new();
    with_spares(&mut held, || {
        let plane = spare_bytes(65_536);
        let alpha = spare_bytes(65_536);
        let (plane_at, alpha_at) = (plane.as_ptr(), alpha.as_ptr());
        let scaled = Scaled { planes: Arc::new(vec![plane]), alpha: Some(Arc::new(vec![alpha])) };
        let shared = scaled.clone();
        drop(scaled);
        assert_eq!(shared.planes[0].as_ptr(), plane_at);
        assert_eq!(shared.alpha.as_ref().unwrap()[0].as_ptr(), alpha_at);
        drop(shared);
        let first = spare_bytes(65_536);
        let second = spare_bytes(65_536);
        assert!(first.as_ptr() == plane_at || first.as_ptr() == alpha_at);
        assert!(second.as_ptr() == plane_at || second.as_ptr() == alpha_at);
        assert_ne!(first.as_ptr(), second.as_ptr());
    });
}
