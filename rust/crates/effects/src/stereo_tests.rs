use super::*;
use telescope_core::frame::PixelFormat;

fn solid(w: usize, h: usize, rgb: [u8; 3]) -> Rgb {
    Rgb::filled(w, h, rgb)
}

/// A picture whose red is its column times 10, so a shift shows where each column went.
fn ramp(w: usize, h: usize) -> Rgb {
    let mut rgb = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            rgb.set(x, y, [(x * 10) as u8, (y * 10) as u8, 7]);
        }
    }
    rgb
}

// ── Pairing ───────────────────────────────────────────────────────────────

#[test]
fn the_nearest_capture_time_wins_and_the_later_one_of_a_tie() {
    assert_eq!(nearest_capture(&[Some(10), Some(20), Some(30)], 24), Some(1));
    assert_eq!(nearest_capture(&[Some(10), Some(20), Some(30)], 26), Some(2));
    assert_eq!(nearest_capture(&[Some(10), Some(20)], 15), Some(1), "as near: the later");
    assert_eq!(nearest_capture(&[None, Some(20), None], 1_000), Some(1), "frames without a time don't count");
    assert_eq!(nearest_capture(&[None, None], 5), None);
    assert_eq!(nearest_capture(&[], 5), None);
}

/// A frame as the pairing sees it: a name and a capture time.
type Shot = (&'static str, Option<u64>);

fn near_in<'a>(left: &'a [Shot], right: &'a [Shot]) -> impl Fn(Eye, u64) -> Option<Shot> + 'a {
    move |eye, at| {
        let list = if eye == Eye::Left { left } else { right };
        nearest_capture(&list.iter().map(|s| s.1).collect::<Vec<_>>(), at).map(|i| list[i])
    }
}

#[test]
fn the_eye_that_runs_behind_keeps_its_frame_and_the_other_gives_its_nearest() {
    let left_history = [("l1", Some(100)), ("l2", Some(133)), ("l3", Some(166))];
    let right_history = [("r1", Some(110)), ("r2", Some(143)), ("r3", Some(176)), ("r4", Some(209))];
    // the right phone is ahead: the left one is behind with l3 (166), and r3 (176) is the right frame nearest to it
    let pair = pair_frames(&left_history[2], &right_history[3], |s| s.1, near_in(&left_history, &right_history));
    assert_eq!((pair.left.0, pair.right.0, pair.by_time), ("l3", "r3", true));
    // the left phone is ahead: the right one is behind with r1 (110), and l1 (100) is nearest to it
    let ahead = [("l1", Some(100)), ("l2", Some(133)), ("l3", Some(166))];
    let behind = [("r0", Some(80)), ("r1", Some(110))];
    let pair = pair_frames(&ahead[2], &behind[1], |s| s.1, near_in(&ahead, &behind));
    assert_eq!((pair.left.0, pair.right.0), ("l1", "r1"));
}

#[test]
fn a_frame_with_nothing_near_falls_back_to_the_newest() {
    let (l, r) = (("l", Some(100)), ("r", Some(300)));
    let pair = pair_frames(&l, &r, |s| s.1, |_, _| None);
    assert_eq!((pair.left.0, pair.right.0, pair.by_time), ("l", "r", true), "no history held yet");
}

#[test]
fn without_capture_times_the_newest_of_each_go_together_and_it_says_so() {
    let pair = pair_frames(&("l", None), &("r", Some(5)), |s| s.1, |_, _| panic!("nothing to look up"));
    assert_eq!((pair.left.0, pair.right.0, pair.by_time), ("l", "r", false));
    let pair = pair_frames(&("l", None), &("r", None), |s| s.1, |_, _| panic!("nothing to look up"));
    assert!(!pair.by_time);
}

#[test]
fn the_sync_comes_from_the_error_bounds_and_the_capture_gap() {
    let frame = 33_000_000;
    let ms = |n: u64| n * 1_000_000;
    // worst error 4 ms plus a 6 ms gap is 10 ms: under half a frame (16.5 ms)
    assert_eq!(sync_status([Some(ms(2)), Some(ms(4))], Some(ms(6)), frame), SyncStatus::InSync);
    // the worse of the two bounds counts, not the better
    assert_eq!(sync_status([Some(ms(1)), Some(ms(12))], Some(ms(6)), frame), SyncStatus::Rough);
    // a gap of its own can be too much
    assert_eq!(sync_status([Some(ms(1)), Some(ms(1))], Some(ms(20)), frame), SyncStatus::Rough);
    assert_eq!(sync_status([Some(ms(1)), Some(ms(1))], Some(ms(300)), frame), SyncStatus::NotSynced);
    // no capture times, or no clock offset on one phone
    assert_eq!(sync_status([None, None], None, frame), SyncStatus::NotSynced);
    assert_eq!(sync_status([Some(ms(1)), None], Some(ms(1)), frame), SyncStatus::NotSynced);
    assert_eq!(sync_status([Some(ms(1)), Some(ms(1))], None, frame), SyncStatus::NotSynced);
    // half a frame is shorter at 60 fps
    assert_eq!(sync_status([Some(ms(2)), Some(ms(2))], Some(ms(7)), 16_600_000), SyncStatus::Rough);
}

#[test]
fn the_frame_time_follows_the_gaps_between_frames_and_ignores_stalls() {
    let mut ns = DEFAULT_FRAME_NS;
    for _ in 0..80 {
        ns = smooth_frame_ns(ns, 16_666_666);
    }
    assert!((16_000_000..17_500_000).contains(&ns), "{ns}");
    assert_eq!(smooth_frame_ns(ns, 2_000_000_000), ns, "a stall isn't a frame time");
    assert_eq!(smooth_frame_ns(ns, 1_000_000), ns, "nor is a burst");
}

// ── Pixels ────────────────────────────────────────────────────────────────

#[test]
fn shifts_are_percent_of_the_extent_held_to_the_slider() {
    assert_eq!(shift_px(0.0, 1000), 0);
    assert_eq!(shift_px(5.0, 1000), 50);
    assert_eq!(shift_px(-5.0, 1000), -50);
    assert_eq!(shift_px(40.0, 1000), 50, "held to 5 %");
    assert_eq!(shift_px(-40.0, 200), -10);
}

#[test]
fn side_by_side_gives_each_eye_half_the_width() {
    let out = side_by_side(&solid(160, 90, [200, 0, 0]), &solid(160, 90, [0, 0, 200]), 160, 90, 0.0, 0.0);
    assert_eq!((out.w, out.h), (160, 90));
    assert_eq!(out.at(0, 0), [200, 0, 0]);
    assert_eq!(out.at(79, 89), [200, 0, 0]);
    assert_eq!(out.at(80, 0), [0, 0, 200]);
    assert_eq!(out.at(159, 89), [0, 0, 200]);
}

#[test]
fn a_wide_canvas_gives_full_side_by_side_without_stretching() {
    // a 32:9 canvas with two 16:9 pictures: every pixel of an eye lands on its own place
    let (left, right) = (ramp(64, 36), ramp(64, 36));
    let out = side_by_side(&left, &right, 128, 36, 0.0, 0.0);
    for (x, y) in [(0, 0), (13, 7), (63, 35)] {
        assert_eq!(out.at(x, y), left.at(x, y));
        assert_eq!(out.at(64 + x, y), right.at(x, y));
    }
}

#[test]
fn side_by_side_works_on_a_canvas_of_any_size() {
    for (w, h) in [(1, 1), (3, 2), (101, 57), (1920, 1080), (7, 400)] {
        let out = side_by_side(&solid(64, 36, [9, 9, 9]), &solid(48, 48, [9, 9, 9]), w, h, 5.0, -5.0);
        assert_eq!((out.w, out.h, out.data.len()), (w, h, w * h * 3));
    }
    // an odd width gives the right eye the extra column
    let out = side_by_side(&solid(10, 4, [255, 0, 0]), &solid(10, 4, [0, 255, 0]), 7, 4, 0.0, 0.0);
    assert_eq!((out.at(2, 0), out.at(3, 0), out.at(6, 3)), ([255, 0, 0], [0, 255, 0], [0, 255, 0]));
}

#[test]
fn depth_moves_the_eyes_against_each_other_inside_their_halves() {
    // each eye is 80 wide, so 5 % is 4 pixels: the left one goes 2 to the right and the right one 2 to the left
    let eye = ramp(80, 4);
    let out = side_by_side(&eye, &eye, 160, 4, 5.0, 0.0);
    assert_eq!(out.at(0, 0), [0, 0, 0], "uncovered, so black");
    assert_eq!(out.at(1, 0), [0, 0, 0]);
    assert_eq!(out.at(2, 0), eye.at(0, 0));
    assert_eq!(out.at(79, 0), eye.at(77, 0));
    assert_eq!(out.at(80, 0), eye.at(2, 0));
    assert_eq!(out.at(80 + 77, 0), eye.at(79, 0));
    assert_eq!(out.at(80 + 78, 0), [0, 0, 0]);
    // the other way round
    let out = side_by_side(&eye, &eye, 160, 4, -5.0, 0.0);
    assert_eq!(out.at(0, 0), eye.at(2, 0));
    assert_eq!(out.at(80, 0), [0, 0, 0], "the right eye's first columns are black");
    assert_eq!(out.at(82, 0), eye.at(0, 0));
    // nothing leaks across the middle
    assert_eq!(out.at(79, 0), [0, 0, 0]);
}

#[test]
fn height_moves_only_the_right_eye() {
    // 5 % of 40 rows is 2: up for a positive value
    let eye = ramp(20, 40);
    let out = side_by_side(&eye, &eye, 40, 40, 0.0, 5.0);
    assert_eq!(out.at(3, 10), eye.at(3, 10), "the left eye stays");
    assert_eq!(out.at(20 + 3, 10), eye.at(3, 12));
    assert_eq!(out.at(20 + 3, 38), [0, 0, 0]);
    assert_eq!(out.at(20 + 3, 39), [0, 0, 0]);
    let out = side_by_side(&eye, &eye, 40, 40, 0.0, -5.0);
    assert_eq!(out.at(20 + 3, 0), [0, 0, 0]);
    assert_eq!(out.at(20 + 3, 12), eye.at(3, 10));
    assert_eq!(out.at(3, 12), eye.at(3, 12));
}

#[test]
fn dubois_keeps_grey_and_white_and_sends_red_to_the_left_eye() {
    for v in [0u8, 64, 128, 255] {
        let out = dubois([v, v, v], [v, v, v]);
        assert!(out.iter().all(|c| (*c as i32 - v as i32).abs() <= 1), "grey {v} gives {out:?}");
    }
    // the left eye only reaches the red channel, the right one the green and the blue
    let left_only = dubois([255, 255, 255], [0, 0, 0]);
    assert_eq!(left_only, [255, 0, 0]);
    let right_only = dubois([0, 0, 0], [255, 255, 255]);
    assert_eq!(right_only[0], 0);
    assert!(right_only[1] > 200 && right_only[2] > 200, "{right_only:?}");
    // a pure red in the left eye is a dimmer red than white is, since the matrix isn't a plain channel pick
    let red = dubois([255, 0, 0], [0, 0, 0]);
    assert!((100..130).contains(&red[0]) && red[1] == 0 && red[2] == 0, "{red:?}");
    // the cyan of the right eye is what a channel swap would not give: red is held back, blue is lifted
    let cyan = dubois([0, 0, 0], [0, 255, 255]);
    assert_eq!(cyan[0], 0);
    assert!(cyan[1] > 100 && cyan[2] > 200, "{cyan:?}");
}

#[test]
fn red_cyan_mixes_the_two_eyes_through_the_matrices() {
    let out = red_cyan(&solid(32, 18, [255, 255, 255]), &solid(32, 18, [0, 0, 0]), 32, 18, 0.0, 0.0);
    assert_eq!((out.w, out.h), (32, 18));
    assert_eq!(out.at(5, 5), [255, 0, 0]);
    // a grey in both eyes stays that grey
    let same = red_cyan(&solid(32, 18, [120, 120, 120]), &solid(32, 18, [120, 120, 120]), 32, 18, 0.0, 0.0);
    let got = same.at(10, 10);
    assert!(got.iter().all(|a| (*a as i32 - 120).abs() <= 1), "{got:?}");
}

#[test]
fn red_cyan_fits_an_eye_of_another_shape_with_bars() {
    // a square picture on a 2:1 canvas keeps its shape: it fills the height and leaves bars at both sides
    let out = red_cyan(&solid(20, 20, [255, 255, 255]), &solid(20, 20, [255, 255, 255]), 40, 20, 0.0, 0.0);
    assert_eq!(out.at(0, 10), [0, 0, 0]);
    assert_eq!(out.at(39, 10), [0, 0, 0]);
    assert!(out.at(20, 10).iter().all(|c| *c >= 254));
}

#[test]
fn red_cyan_shifts_the_eyes_as_side_by_side_does() {
    // the canvas is 40 wide so 5 % is 2 pixels: the left eye goes one to the right
    let out = red_cyan(&solid(40, 10, [255, 255, 255]), &solid(40, 10, [0, 0, 0]), 40, 10, 5.0, 0.0);
    assert_eq!(out.at(0, 5), [0, 0, 0]);
    assert_eq!(out.at(1, 5), [255, 0, 0]);
    // and the right eye one up when it's lifted by 5 % of the height of 20
    let eye = ramp(40, 20);
    let lifted = red_cyan(&solid(40, 20, [0, 0, 0]), &eye, 40, 20, 0.0, 5.0);
    let flat = red_cyan(&solid(40, 20, [0, 0, 0]), &eye, 40, 20, 0.0, 0.0);
    assert_eq!(lifted.at(10, 5), flat.at(10, 6));
}

fn frame(w: u32, h: u32, format: PixelFormat, rgb: [u8; 3]) -> Frame {
    let mut f = Frame::new(w, h, format, vec![0; format.size(w, h)]);
    Rgb::filled(w as usize, h as usize, rgb).write_to(&mut f);
    f
}

#[test]
fn a_picture_comes_out_in_the_format_it_was_asked_for_at_the_canvas_size() {
    for format in [PixelFormat::Rgba, PixelFormat::Bgra, PixelFormat::I420, PixelFormat::Nv12] {
        let left = frame(64, 36, format, [200, 30, 30]);
        let right = frame(64, 36, format, [30, 30, 200]);
        for look in Look::ALL {
            let out = render(&left, &right, look, (96, 54), 0.0, 0.0, &left).expect("a picture");
            assert_eq!((out.width, out.height, out.format), (96, 54, format));
            assert_eq!(out.data.len(), format.size(96, 54));
        }
    }
}

#[test]
fn a_side_by_side_picture_has_each_eyes_colour_in_its_half() {
    let left = frame(64, 36, PixelFormat::Rgba, [200, 30, 30]);
    let right = frame(64, 36, PixelFormat::Rgba, [30, 30, 200]);
    let out = render(&left, &right, Look::SideBySide, (64, 36), 0.0, 0.0, &left).unwrap();
    let px = |x: usize| {
        let at = (5 * 64 + x) * 4;
        [out.data[at], out.data[at + 1], out.data[at + 2]]
    };
    assert_eq!(px(5), [200, 30, 30]);
    assert_eq!(px(58), [30, 30, 200]);
}

#[test]
fn an_empty_canvas_gives_no_picture() {
    let f = frame(8, 8, PixelFormat::Rgba, [1, 2, 3]);
    assert!(render(&f, &f, Look::RedCyan, (0, 10), 0.0, 0.0, &f).is_none());
}

#[test]
fn the_looks_keep_their_names() {
    for look in Look::ALL {
        assert_eq!(Look::from_name(look.name()), Some(look));
        assert_eq!(Look::ALL[look.index()], look);
    }
    assert_eq!(Look::from_name("nonsense"), None);
    assert_eq!(Look::default(), Look::SideBySide);
}

#[test]
fn a_manual_phone_gives_its_values_to_both() {
    let state = json!({
        "auto": false, "iso": 400, "shutter_ns": 8_000_000, "wb_manual": true, "wb_r": 2.0, "wb_ge": 1.0, "wb_go": 1.0, "wb_b": 1.5,
        "focus_mode": "manual", "focus_distance": 2.5,
    });
    assert_eq!(link_bodies(&state), vec![
        json!({"action": "iso", "value": 400}),
        json!({"action": "shutter", "value": 8_000_000}),
        json!({"action": "wb_gains", "r": 2.0, "ge": 1.0, "go": 1.0, "b": 1.5}),
        json!({"action": "focus_mode", "value": "manual"}),
        json!({"action": "focus_distance", "value": 2.5}),
    ]);
}

#[test]
fn an_automatic_phone_puts_both_back_to_automatic() {
    let state = json!({"auto": true, "iso": null, "shutter_ns": null, "wb_manual": false, "wb_r": null, "focus_mode": "continuous"});
    assert_eq!(link_bodies(&state), vec![
        json!({"action": "auto"}),
        json!({"action": "wb_auto"}),
        json!({"action": "focus_mode", "value": "continuous"}),
    ]);
    // one of ISO and shutter alone is no manual exposure, and a phone on point focus keeps its focus
    let half = json!({"iso": 400, "shutter_ns": null, "wb_manual": false, "focus_mode": "point"});
    assert_eq!(link_bodies(&half), vec![json!({"action": "auto"}), json!({"action": "wb_auto"})]);
}
