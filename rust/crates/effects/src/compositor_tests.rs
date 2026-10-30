//! test_composite.py's drawing tests, by the same names. Frames are BGRA here, so a colour reads as the BGR Python's do.

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
    let [(tl_in, tl_out), (tr_in, _), (bl_in, _), (br_in, _)] = &*first;
    assert!(tl_in[0] == 0.0 && tl_in[12 * 12 - 1] == 1.0 && tl_in.iter().zip(tl_out).all(|(a, b)| (a + b - 1.0).abs() < 1e-6));
    let at = |m: &Vec<f32>, y: usize, x: usize| m[y * 12 + x];
    for y in 0..12 {
        for x in 0..12 {
            assert_eq!(at(tr_in, y, x), at(tl_in, y, 11 - x));
            assert_eq!(at(br_in, y, x), at(tl_in, 11 - y, 11 - x));
            assert_eq!(at(bl_in, y, x), at(tl_in, 11 - y, x));
        }
    }
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
    // Python hands back the same array; here it's an equal copy
    assert_eq!(scale_to(&frame, 1920, 1080, 3, 1920, 1080), frame);
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
    for _ in 0..5 {
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
        assert!(took < 36.0, "{took}");
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
