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
    let pair = pair_frames(&left_history[2], &right_history[3], |s| s.1, near_in(&left_history, &right_history), 0);
    assert_eq!((pair.left.0, pair.right.0, pair.by_time), ("l3", "r3", true));
    // the left phone is ahead: the right one is behind with r1 (110), and l1 (100) is nearest to it
    let ahead = [("l1", Some(100)), ("l2", Some(133)), ("l3", Some(166))];
    let behind = [("r0", Some(80)), ("r1", Some(110))];
    let pair = pair_frames(&ahead[2], &behind[1], |s| s.1, near_in(&ahead, &behind), 0);
    assert_eq!((pair.left.0, pair.right.0), ("l1", "r1"));
}

#[test]
fn a_frame_with_nothing_near_falls_back_to_the_newest() {
    let (l, r) = (("l", Some(100)), ("r", Some(300)));
    let pair = pair_frames(&l, &r, |s| s.1, |_, _| None, 0);
    assert_eq!((pair.left.0, pair.right.0, pair.by_time), ("l", "r", true), "no history held yet");
}

#[test]
fn without_capture_times_the_newest_of_each_go_together_and_it_says_so() {
    let pair = pair_frames(&("l", None), &("r", Some(5)), |s| s.1, |_, _| panic!("nothing to look up"), 0);
    assert_eq!((pair.left.0, pair.right.0, pair.by_time), ("l", "r", false));
    let pair = pair_frames(&("l", None), &("r", None), |s| s.1, |_, _| panic!("nothing to look up"), 0);
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
    let out = side_by_side(&solid(160, 90, [200, 0, 0]), &solid(160, 90, [0, 0, 200]), 160, 90, &shift(0.0, 0.0));
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
    let out = side_by_side(&left, &right, 128, 36, &shift(0.0, 0.0));
    for (x, y) in [(0, 0), (13, 7), (63, 35)] {
        assert_eq!(out.at(x, y), left.at(x, y));
        assert_eq!(out.at(64 + x, y), right.at(x, y));
    }
}

#[test]
fn side_by_side_works_on_a_canvas_of_any_size() {
    for (w, h) in [(1, 1), (3, 2), (101, 57), (1920, 1080), (7, 400)] {
        let out = side_by_side(&solid(64, 36, [9, 9, 9]), &solid(48, 48, [9, 9, 9]), w, h, &shift(5.0, -5.0));
        assert_eq!((out.w, out.h, out.data.len()), (w, h, w * h * 3));
    }
    // an odd width gives the right eye the extra column
    let out = side_by_side(&solid(10, 4, [255, 0, 0]), &solid(10, 4, [0, 255, 0]), 7, 4, &shift(0.0, 0.0));
    assert_eq!((out.at(2, 0), out.at(3, 0), out.at(6, 3)), ([255, 0, 0], [0, 255, 0], [0, 255, 0]));
}

#[test]
fn depth_moves_the_eyes_against_each_other_inside_their_halves() {
    // each eye is 80 wide, so 5 % is 4 pixels: the left one goes 2 to the right and the right one 2 to the left
    let eye = ramp(80, 4);
    let out = side_by_side(&eye, &eye, 160, 4, &shift(5.0, 0.0));
    assert_eq!(out.at(0, 0), [0, 0, 0], "uncovered, so black");
    assert_eq!(out.at(1, 0), [0, 0, 0]);
    assert_eq!(out.at(2, 0), eye.at(0, 0));
    assert_eq!(out.at(79, 0), eye.at(77, 0));
    assert_eq!(out.at(80, 0), eye.at(2, 0));
    assert_eq!(out.at(80 + 77, 0), eye.at(79, 0));
    assert_eq!(out.at(80 + 78, 0), [0, 0, 0]);
    // the other way round
    let out = side_by_side(&eye, &eye, 160, 4, &shift(-5.0, 0.0));
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
    let out = side_by_side(&eye, &eye, 40, 40, &shift(0.0, 5.0));
    assert_eq!(out.at(3, 10), eye.at(3, 10), "the left eye stays");
    assert_eq!(out.at(20 + 3, 10), eye.at(3, 12));
    assert_eq!(out.at(20 + 3, 38), [0, 0, 0]);
    assert_eq!(out.at(20 + 3, 39), [0, 0, 0]);
    let out = side_by_side(&eye, &eye, 40, 40, &shift(0.0, -5.0));
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
    let out = red_cyan(&solid(32, 18, [255, 255, 255]), &solid(32, 18, [0, 0, 0]), 32, 18, &shift(0.0, 0.0));
    assert_eq!((out.w, out.h), (32, 18));
    assert_eq!(out.at(5, 5), [255, 0, 0]);
    // a grey in both eyes stays that grey
    let same = red_cyan(&solid(32, 18, [120, 120, 120]), &solid(32, 18, [120, 120, 120]), 32, 18, &shift(0.0, 0.0));
    let got = same.at(10, 10);
    assert!(got.iter().all(|a| (*a as i32 - 120).abs() <= 1), "{got:?}");
}

#[test]
fn red_cyan_fits_an_eye_of_another_shape_with_bars() {
    // a square picture on a 2:1 canvas keeps its shape: it fills the height and leaves bars at both sides
    let out = red_cyan(&solid(20, 20, [255, 255, 255]), &solid(20, 20, [255, 255, 255]), 40, 20, &shift(0.0, 0.0));
    assert_eq!(out.at(0, 10), [0, 0, 0]);
    assert_eq!(out.at(39, 10), [0, 0, 0]);
    assert!(out.at(20, 10).iter().all(|c| *c >= 254));
}

#[test]
fn red_cyan_shifts_the_eyes_as_side_by_side_does() {
    // the canvas is 40 wide so 5 % is 2 pixels: the left eye goes one to the right
    let out = red_cyan(&solid(40, 10, [255, 255, 255]), &solid(40, 10, [0, 0, 0]), 40, 10, &shift(5.0, 0.0));
    assert_eq!(out.at(0, 5), [0, 0, 0]);
    assert_eq!(out.at(1, 5), [255, 0, 0]);
    // and the right eye one up when it's lifted by 5 % of the height of 20
    let eye = ramp(40, 20);
    let lifted = red_cyan(&solid(40, 20, [0, 0, 0]), &eye, 40, 20, &shift(0.0, 5.0));
    let flat = red_cyan(&solid(40, 20, [0, 0, 0]), &eye, 40, 20, &shift(0.0, 0.0));
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
            let out = render(&left, &right, look, (96, 54), &Tune::default(), &left).expect("a picture");
            assert_eq!((out.width, out.height, out.format), (96, 54, format));
            assert_eq!(out.data.len(), format.size(96, 54));
        }
    }
}

#[test]
fn a_side_by_side_picture_has_each_eyes_colour_in_its_half() {
    let left = frame(64, 36, PixelFormat::Rgba, [200, 30, 30]);
    let right = frame(64, 36, PixelFormat::Rgba, [30, 30, 200]);
    let out = render(&left, &right, Look::SideBySide, (64, 36), &Tune::default(), &left).unwrap();
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
    assert!(render(&f, &f, Look::RedCyan, (0, 10), &Tune::default(), &f).is_none());
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

// ── Timing, size, tilt and the colour curves ──────────────────────────────

/// The shifts alone, as the first version of the plugin had them.
fn shift(depth: f64, height: f64) -> Tune {
    Tune { depth, height, ..Tune::default() }
}

#[test]
fn timing_moves_the_pairing_target_by_the_offset() {
    let left_history = [("l1", Some(100)), ("l2", Some(133)), ("l3", Some(166))];
    let right_history = [("r1", Some(110)), ("r2", Some(143)), ("r3", Some(176)), ("r4", Some(209))];
    let near = near_in(&left_history, &right_history);
    let ms = |n: i64| n * 1_000_000;
    let times = |s: &Shot| s.1.map(|t| t * 1_000_000);
    let near_ns = |eye, at: u64| near(eye, at / 1_000_000);
    // the right eye 33 ms later than the left: l3 (166) goes with the right frame nearest to 199, which is r4 (209)
    let pair = pair_frames(&left_history[2], &right_history[3], times, &near_ns, ms(33));
    assert_eq!((pair.left.0, pair.right.0), ("l3", "r4"));
    // the right eye 33 ms earlier: nearest to 133 is r2 (143)
    let pair = pair_frames(&left_history[2], &right_history[3], times, &near_ns, ms(-33));
    assert_eq!((pair.left.0, pair.right.0), ("l3", "r2"));
    // with the right eye later, the left one can be the one that is ahead: r4 less 66 ms is 143, which l2 (133) is nearest to
    let pair = pair_frames(&left_history[2], &right_history[3], times, &near_ns, ms(66));
    assert_eq!((pair.left.0, pair.right.0), ("l2", "r4"));
    // no offset is the plain pairing
    let pair = pair_frames(&left_history[2], &right_history[3], times, &near_ns, 0);
    assert_eq!((pair.left.0, pair.right.0), ("l3", "r3"));
}

#[test]
fn the_history_follows_the_timing() {
    let frame = 33_333_333;
    assert_eq!(history_frames(0.0, frame), HISTORY_FRAMES, "never fewer than the default");
    // 200 ms is just over 6 frames at 30 fps (a frame is a hair under a third of a second), so 7 and 3 to spare
    assert_eq!(history_frames(200.0, frame), 10);
    assert_eq!(history_frames(-200.0, frame), 10, "either way");
    // at 120 fps the same offset is just over 24 frames
    assert_eq!(history_frames(200.0, 8_333_333), 28);
    assert!(history_frames(50.0, 0) > 3, "no frame time never divides by zero");
}

#[test]
fn an_eye_held_back_by_arrival_time_gives_the_frame_that_came_in_that_long_ago() {
    let ms = |n: u64| n * 1_000_000;
    let arrivals = [ms(100), ms(133), ms(166), ms(200), ms(233)];
    assert_eq!(delayed_index(&arrivals, 0), Some(4), "no delay is the newest");
    assert_eq!(delayed_index(&arrivals, ms(66)), Some(2), "233 less 66 is 167, nearest to 166");
    assert_eq!(delayed_index(&arrivals, ms(5_000)), Some(0), "held to what there is");
    assert_eq!(delayed_index(&[], 5), None);
}

#[test]
fn a_warp_of_nothing_leaves_the_eye_as_it_was() {
    let eye = ramp(24, 16);
    assert_eq!(warp(&eye, 1.0, 0.0, 1.0), eye);
}

#[test]
fn size_enlarges_about_the_middle_and_shrinking_leaves_a_border() {
    let eye = solid(40, 40, [200, 100, 50]);
    let bigger = warp(&eye, 1.25, 0.0, 1.0);
    assert_eq!((bigger.at(0, 0), bigger.at(39, 39)), ([200, 100, 50], [200, 100, 50]), "it still fills the picture");
    let smaller = warp(&eye, 0.8, 0.0, 1.0);
    assert_eq!(smaller.at(20, 20), [200, 100, 50]);
    // 0.8 of 40 is 32: a border of 4 pixels all round
    assert_eq!([smaller.at(0, 20), smaller.at(39, 20), smaller.at(20, 0), smaller.at(20, 39)], [[0, 0, 0]; 4]);
    assert_eq!(smaller.at(5, 20), [200, 100, 50]);
    // a mark 10 pixels right of the middle goes to 5 when the eye is halved
    let mut marked = solid(40, 40, [0, 0, 0]);
    for (x, y) in (29..31).flat_map(|x| (0..40).map(move |y| (x, y))) {
        marked.set(x, y, [255, 255, 255]);
    }
    let shrunk = warp(&marked, 0.5, 0.0, 1.0);
    let lit: Vec<usize> = (0..40).filter(|x| shrunk.at(*x, 20)[0] > 100).collect();
    assert!(!lit.is_empty() && lit.iter().all(|x| (24..=26).contains(x)), "{lit:?}");
    // doubled, it goes out to about 20 pixels from the middle, the very edge of the picture
    let grown = warp(&marked, 2.0, 0.0, 1.0);
    let lit: Vec<usize> = (0..40).filter(|x| grown.at(*x, 20)[0] > 100).collect();
    assert!(!lit.is_empty() && lit.iter().all(|x| *x >= 37), "{lit:?}");
}

/// Where the lit pixels of a picture are on average, from its middle.
fn lit_offset(rgb: &Rgb) -> (f64, f64) {
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
    for y in 0..rgb.h {
        for x in 0..rgb.w {
            if rgb.at(x, y)[0] > 128 {
                (sx, sy, n) = (sx + x as f64, sy + y as f64, n + 1.0);
            }
        }
    }
    (sx / n - (rgb.w as f64 - 1.0) / 2.0, sy / n - (rgb.h as f64 - 1.0) / 2.0)
}

#[test]
fn tilt_turns_the_eye_clockwise_about_its_middle() {
    let mut eye = solid(41, 41, [0, 0, 0]);
    // a mark above the middle
    for y in 8..12 {
        for x in 19..22 {
            eye.set(x, y, [255, 255, 255]);
        }
    }
    // a quarter turn clockwise takes the top to the right
    let (x, y) = lit_offset(&warp(&eye, 1.0, 90.0, 1.0));
    assert!((x - 10.0).abs() < 1.0 && y.abs() < 1.0, "({x}, {y})");
    // five degrees moves it right by 10 sin 5 degrees, about 0.9 of a pixel
    let (x, y) = lit_offset(&warp(&eye, 1.0, 5.0, 1.0));
    assert!(x > 0.5 && x < 1.5 && (y + 10.0).abs() < 0.6, "({x}, {y})");
    let (x, _) = lit_offset(&warp(&eye, 1.0, -5.0, 1.0));
    assert!(x < -0.5, "the other way: {x}");
}

#[test]
fn a_squeezed_eye_is_turned_in_its_own_shape() {
    // an eye drawn at half its width: a mark 10 above the middle, turned a quarter, lands 5 pixels right of the middle of the squeezed picture
    let mut eye = solid(21, 41, [0, 0, 0]);
    for y in 9..11 {
        for x in 9..12 {
            eye.set(x, y, [255, 255, 255]);
        }
    }
    let (x, _) = lit_offset(&warp(&eye, 1.0, 90.0, 0.5));
    assert!((x - 5.0).abs() < 1.0, "{x}");
}

#[test]
fn the_right_eye_goes_through_the_colour_curves_and_the_left_does_not() {
    let mut lut = Lut::identity();
    lut.0[0] = std::array::from_fn(|i| (i / 2) as u8);
    let tune = Tune { lut: Some(Arc::new(lut)), ..Tune::default() };
    let out = side_by_side(&solid(40, 20, [200, 100, 50]), &solid(40, 20, [200, 100, 50]), 40, 20, &tune);
    assert_eq!(out.at(5, 5), [200, 100, 50], "the left eye is as it was");
    assert_eq!(out.at(25, 5), [100, 100, 50], "the right eye's red is halved");
    let out = red_cyan(&solid(40, 20, [200, 200, 200]), &solid(40, 20, [200, 200, 200]), 40, 20, &tune);
    let plain = red_cyan(&solid(40, 20, [200, 200, 200]), &solid(40, 20, [200, 200, 200]), 40, 20, &Tune::default());
    assert_ne!(out.at(20, 10), plain.at(20, 10), "red/cyan takes the curves too");
}

#[test]
fn size_reaches_the_right_eye_only_and_is_held_to_the_slider() {
    let white = solid(40, 20, [255, 255, 255]);
    let out = side_by_side(&white, &white, 40, 20, &Tune { size: 90.0, ..Tune::default() });
    assert_eq!(out.at(0, 0), [255, 255, 255], "the left eye fills its half");
    assert_eq!(out.at(20, 0), [0, 0, 0], "the right eye shrank away from its corner");
    assert_eq!(out.at(30, 10), [255, 255, 255]);
    // a size and a tilt of nothing is the plain picture
    let plain = side_by_side(&ramp(40, 20), &ramp(40, 20), 80, 20, &Tune::default());
    assert_eq!(plain, side_by_side(&ramp(40, 20), &ramp(40, 20), 80, 20, &Tune { size: 100.0, tilt: 0.0, ..Tune::default() }));
    // out-of-range values are held to the sliders
    let wild = side_by_side(&white, &white, 40, 20, &Tune { size: 10.0, ..Tune::default() });
    assert_eq!(wild, side_by_side(&white, &white, 40, 20, &Tune { size: 80.0, ..Tune::default() }));
}

#[test]
fn a_picture_can_be_measured_while_it_is_made() {
    let left = frame(64, 36, PixelFormat::Rgba, [200, 100, 50]);
    let right = frame(64, 36, PixelFormat::Rgba, [100, 100, 100]);
    let measured = render_measured(&left, &right, Look::SideBySide, (64, 36), &Tune::default(), &left, true).unwrap();
    let [l, r] = measured.stats.expect("asked for");
    assert!(l.luma > 100.0 && l.luma < 140.0 && (r.luma - 100.0).abs() < 3.0, "{} {}", l.luma, r.luma);
    assert!(render_measured(&left, &right, Look::SideBySide, (64, 36), &Tune::default(), &left, false).unwrap().stats.is_none());
    // what is measured is the eyes themselves, not what the curves made of the right one
    let tune = Tune { lut: Some(Arc::new(Lut([[0; 256]; 3]))), ..Tune::default() };
    let curved = render_measured(&left, &right, Look::SideBySide, (64, 36), &tune, &left, true).unwrap();
    assert!((curved.stats.unwrap()[1].luma - 100.0).abs() < 3.0);
}
