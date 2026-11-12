//! The pixel tools and the level maths of tests/test_cinema.py.

use super::*;

fn grey(level: u8, w: usize, h: usize) -> Rgb {
    Rgb::filled(w, h, [level; 3])
}

fn colour_for(pct: u32) -> [u8; 3] {
    FALSE_COLOURS.iter().find(|(upper, ..)| pct <= *upper).and_then(|(_, c, _)| *c).expect("a colour")
}

fn fraction(frame: &Rgb, rgb: [u8; 3]) -> f64 {
    frame.data.as_chunks::<3>().0.iter().filter(|p| **p == rgb).count() as f64 / (frame.w * frame.h) as f64
}

fn look() -> Look {
    Look::default()
}

fn tinted(look: &Look, frame: &Rgb) -> Rgb {
    let mut out = frame.clone();
    tint(look, &mut out, &mut 0);
    out
}

#[test]
fn false_colour_marks_crushed_middle_grey_skin_and_clipped() {
    let level = |pct: f64| (pct * 255.0).round_ties_even() as u8;
    let at = |l: u8| false_colour(&grey(l, 64, 36), None).at(0, 0);
    assert_eq!(at(0), colour_for(0));
    assert_eq!(at(255), colour_for(100));
    // 18% grey lands around 42%
    assert_eq!(at(level(0.42)), colour_for(42));
    assert_eq!(at(level(0.55)), colour_for(55));
    // well exposed stays grey
    let plain = at(level(0.25));
    assert!(plain[0] == plain[1] && plain[1] == plain[2]);
}

#[test]
fn zebras_stripe_only_what_is_bright_enough() {
    let mut frame = grey(200, 64, 36);
    for y in 0..36 {
        for x in 32..64 {
            frame.set(x, y, [250; 3]);
        }
    }
    let mask = zebra_mask(&frame, 95, 0, None);
    let count = |cols: std::ops::Range<usize>| (0..36).flat_map(|y| cols.clone().map(move |x| (x, y))).filter(|&(x, y)| mask[y * 64 + x]).count();
    assert_eq!(count(0..32), 0);
    let right = count(32..64) as f64 / (36.0 * 32.0);
    // stripes, not a solid block
    assert!(0.3 < right && right < 0.7);
    // they move along
    assert_ne!(mask, zebra_mask(&frame, 95, 2, None));
}

#[test]
fn peaking_finds_sharp_edges_and_nothing_on_a_flat_picture() {
    let mut frame = grey(40, 64, 36);
    for y in 0..36 {
        for x in 32..64 {
            frame.set(x, y, [220; 3]);
        }
    }
    let mask = peaking_mask(&frame, 5);
    assert!((0..36).flat_map(|y| (30..34).map(move |x| (x, y))).any(|(x, y)| mask[y * 64 + x]));
    assert!(!peaking_mask(&grey(128, 64, 36), 10).iter().any(|m| *m));
    // a gentle ramp isn't an edge
    let mut soft = Rgb::new(64, 36);
    for y in 0..36 {
        for x in 0..64 {
            let v = if x == 63 { 220 } else { (40.0 + x as f64 * (180.0 / 63.0)) as u8 };
            soft.set(x, y, [v; 3]);
        }
    }
    assert!(!peaking_mask(&soft, 1).iter().any(|m| *m));
}

#[test]
fn the_tools_judge_the_picture_before_any_of_them_paints() {
    // a plain bright wall: zebras all over, nothing sharp
    let wall = grey(250, 64, 36);
    let out = tinted(&Look { zebras: true, peaking: true, peaking_colour: "green", ..look() }, &wall);
    // the zebras' stripes aren't edges to peak on
    assert!(fraction(&out, [60, 255, 60]) < 0.01);
    assert!(fraction(&out, [0, 0, 0]) > 0.3);
    let clipped = grey(255, 64, 36);
    let out = tinted(&Look { zebras: true, false_colour: true, ..look() }, &clipped);
    // still striped under false colour's red
    assert!(fraction(&out, [0, 0, 0]) > 0.3);
    assert!(fraction(&out, colour_for(100)) > 0.3);
}

#[test]
fn a_big_preview_is_judged_small_and_marked_at_its_own_size() {
    let mut frame = grey(40, 2560, 1440);
    for y in 0..1440 {
        for x in 1280..2560 {
            frame.set(x, y, [255; 3]);
        }
    }
    let out = tinted(&Look { peaking: true, zebras: true, peaking_colour: "green", ..look() }, &frame);
    assert_eq!((out.w, out.h), (frame.w, frame.h));
    let green = [60, 255, 60];
    // the edge, where it is in the big frame
    assert!((0..1440).any(|y| (1276..1284).any(|x| out.at(x, y) == green)));
    assert!(!(0..1440).any(|y| (0..1200).any(|x| out.at(x, y) == green)));
    let black = (0..1440).flat_map(|y| (1300..2560).map(move |x| (x, y))).filter(|&(x, y)| out.at(x, y) == [0, 0, 0]).count() as f64 / (1440.0 * 1260.0);
    // striped, not blacked out
    assert!(0.3 < black && black < 0.7);
    let out = tinted(&Look { false_colour: true, ..look() }, &frame);
    assert_eq!(out.at(2000, 10), colour_for(100));
}

// ── Level guide ──────────────────────────────────────────────────────────────

fn view(flip_h: bool, flip_v: bool, rotation: Option<i32>) -> View {
    View { flip_h, flip_v, rotation, ..View::default() }
}

#[test]
fn the_roll_as_shown_follows_the_views_flip_and_rotation() {
    assert_eq!(shown_roll(5.0, &View::default()), 5.0);
    assert_eq!(shown_roll(5.0, &view(true, false, None)), -5.0);
    assert_eq!(shown_roll(5.0, &view(false, true, None)), -5.0);
    // both flips are a half turn
    assert_eq!(shown_roll(5.0, &view(true, true, None)), 5.0);
    // portrait phone, turned upright here
    assert_eq!(shown_roll(-90.0, &view(false, false, Some(90))), 0.0);
    assert_eq!(shown_roll(90.0, &view(false, false, Some(270))), 0.0);
    assert_eq!(shown_roll(170.0, &view(false, false, Some(180))), -10.0);
    // Flipped, then turned: the flip comes first
    assert_eq!(shown_roll(80.0, &view(true, false, Some(90))), 10.0);
}

#[test]
fn the_filter_follows_a_change_gradually() {
    let mut f = LevelFilter::new(0.3);
    // the first reading counts as it is
    assert_eq!(f.update(Some(10.0), Some(0.0), 0.0).unwrap().roll, Some(10.0));
    let rolls: Vec<f64> = (1..=30).map(|i| f.update(Some(0.0), Some(0.0), i as f64 / 30.0).unwrap().roll.unwrap()).collect();
    assert!(rolls.windows(2).all(|w| w[0] >= w[1]) && rolls[0] > 8.0 && rolls[5] > 3.0);
    // a second later it's there, and shows as level
    assert_eq!(*rolls.last().unwrap(), 0.0);
}

#[test]
fn jitter_near_level_stays_level_and_green() {
    let mut f = LevelFilter::default();
    let mut seed = 1u64;
    let mut noise = |spread: f64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) * spread
    };
    let levels: Vec<Level> = (0..120).map(|i| f.update(Some(noise(0.7)), Some(noise(1.0)), i as f64 / 30.0).unwrap()).collect();
    assert!(levels[10..].iter().all(|l| l.roll_level && l.roll == Some(0.0)));
    assert!(levels[10..].iter().all(|l| l.pitch_level && l.pitch == 0.0));
}

#[test]
fn level_has_a_margin_either_way() {
    // no smoothing, to see the margins
    let mut f = LevelFilter::new(0.001);
    // not level yet: has to come within LEVEL_SNAP
    assert!(!f.update(Some(0.7), Some(0.0), 0.0).unwrap().roll_level);
    assert!(f.update(Some(0.4), Some(0.0), 0.1).unwrap().roll_level);
    // stays level until past LEVEL_LEAVE
    assert!(f.update(Some(0.8), Some(0.0), 0.2).unwrap().roll_level);
    let lv = f.update(Some(1.2), Some(0.0), 0.3).unwrap();
    assert!(!lv.roll_level);
    assert!((lv.roll.unwrap() - 1.2).abs() < 1e-9);
}

#[test]
fn the_filter_goes_the_short_way_round_past_180() {
    let mut f = LevelFilter::new(0.3);
    f.update(Some(179.0), Some(0.0), 0.0);
    let roll = f.update(Some(-179.0), Some(0.0), 1.0 / 30.0).unwrap().roll.unwrap();
    // not swung back through 0
    assert!(roll.abs() > 178.9, "{roll}");
}

#[test]
fn pointing_straight_up_has_no_roll_but_keeps_pitch() {
    let mut f = LevelFilter::default();
    f.update(Some(3.0), Some(10.0), 0.0);
    let lv = f.update(None, Some(85.0), 0.1).unwrap();
    assert!(lv.roll.is_none() && lv.pitch > 10.0);
    assert!(f.update(None, None, 0.2).is_none());
}

#[test]
fn every_guide_in_the_list_has_lines() {
    let keys: Vec<&str> = GUIDES.iter().map(|g| g.0).collect();
    assert_eq!(keys, ["off", "thirds", "cross", "4x4", "golden"]);
    for key in &keys[1..] {
        assert!(guide_lines(key).is_some(), "{key}");
    }
    assert!(guide_lines("off").is_none());
}

// ── Speed ────────────────────────────────────────────────────────────────────

/// A picture with edges, gradients and noise, so every tool has something to mark.
fn busy(w: usize, h: usize) -> Rgb {
    let mut s = 99u32;
    let mut out = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let block = if (x / 64 + y / 64) % 2 == 0 { 40 } else { 215 };
            let v = (block as u32 + (s >> 27)).min(255) as u8;
            out.set(x, y, [v, (x * 255 / w) as u8, (y * 255 / h) as u8]);
        }
    }
    out
}

/// sobel_abs as it was written before the interior got its own loop.
fn sobel_abs_before(grey: &Gray, dx: bool) -> Vec<u8> {
    let (w, h) = (grey.w as isize, grey.h as isize);
    let at = |x: isize, y: isize| -> i32 {
        let r = |i: isize, n: isize| if n == 1 { 0 } else if i < 0 { -i } else if i >= n { 2 * (n - 1) - i } else { i };
        grey.data[(r(y, h) * w + r(x, w)) as usize] as i32
    };
    let mut out = vec![0u8; grey.data.len()];
    for y in 0..h {
        for x in 0..w {
            let g = if dx {
                (at(x + 1, y - 1) - at(x - 1, y - 1)) + 2 * (at(x + 1, y) - at(x - 1, y)) + (at(x + 1, y + 1) - at(x - 1, y + 1))
            } else {
                (at(x - 1, y + 1) - at(x - 1, y - 1)) + 2 * (at(x, y + 1) - at(x, y - 1)) + (at(x + 1, y + 1) - at(x + 1, y - 1))
            };
            out[(y * w + x) as usize] = g.unsigned_abs().min(255) as u8;
        }
    }
    out
}

/// peaking_mask as it was: two Sobel passes, then the mask.
fn peaking_mask_before(frame: &Rgb, sensitivity: i64) -> Vec<bool> {
    let grey = frame.gray();
    let (gx, gy) = (sobel_abs_before(&grey, true), sobel_abs_before(&grey, false));
    let threshold = peaking_threshold(sensitivity) / 2;
    gx.iter().zip(&gy).map(|(&x, &y)| ((x as f32 * 0.5 + y as f32 * 0.5).round_ties_even() as i64) > threshold).collect()
}

#[test]
fn peaking_gives_the_mask_it_always_did_at_any_size() {
    for (w, h) in [(1, 1), (1, 7), (7, 1), (2, 2), (3, 3), (5, 4), (64, 36), (131, 77)] {
        let frame = busy(w, h);
        for sensitivity in [1, 5, 10] {
            assert_eq!(peaking_mask(&frame, sensitivity), peaking_mask_before(&frame, sensitivity), "{w}x{h} at {sensitivity}");
        }
    }
}

#[test]
#[ignore = "timing"]
fn time_the_pixel_tools() {
    for (w, h) in [(1920, 1080), (3840, 2160)] {
        let frame = busy(w, h);
        for (name, look) in [
            ("peaking", Look { peaking: true, ..look() }),
            ("zebras", Look { zebras: true, ..look() }),
            ("false colour", Look { false_colour: true, ..look() }),
            ("all three", Look { peaking: true, zebras: true, false_colour: true, ..look() }),
        ] {
            let mut phase = 0;
            let mut work = frame.clone();
            tint(&look, &mut work, &mut phase);
            let runs = 8;
            let start = std::time::Instant::now();
            for _ in 0..runs {
                let mut work = frame.clone();
                tint(&look, &mut work, &mut phase);
            }
            eprintln!("{w}x{h} {name}: {:.2} ms", start.elapsed().as_secs_f64() * 1000.0 / runs as f64);
        }
    }
}
