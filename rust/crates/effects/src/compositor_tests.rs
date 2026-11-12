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
