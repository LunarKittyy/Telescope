use super::*;

const MS: u64 = 1_000_000;

/// A picture whose three channels each run through all 256 values (rows of 256 columns, the value being the column), so a curve shows everywhere.
fn ramp() -> Rgb {
    let mut rgb = Rgb::new(256, 32);
    for y in 0..32 {
        for x in 0..256 {
            rgb.set(x, y, [x as u8, x as u8, x as u8]);
        }
    }
    rgb
}

/// The picture with a curve per channel applied.
fn curved(src: &Rgb, curves: [&dyn Fn(f64) -> f64; 3]) -> Rgb {
    let mut out = src.clone();
    for px in out.data.as_chunks_mut::<3>().0 {
        for c in 0..3 {
            px[c] = (curves[c](px[c] as f64 / 255.0) * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn stats_of(rgb: &Rgb) -> Stats {
    // the centre of the ramp is only its middle half of the values, so the whole picture goes in through a framed copy
    let mut framed = Rgb::new(rgb.w * 2, rgb.h * 2);
    for y in 0..rgb.h {
        for x in 0..rgb.w {
            framed.set(x + rgb.w / 2, y + rgb.h / 2, rgb.at(x, y));
        }
    }
    centre_stats(&framed).expect("a middle")
}

// ── The pictures ──────────────────────────────────────────────────────────

#[test]
fn the_middle_of_a_picture_gives_its_brightness_and_histograms() {
    // black all round and a white middle: only the middle half counts
    let mut rgb = Rgb::new(40, 40);
    for y in 10..30 {
        for x in 10..30 {
            rgb.set(x, y, [255, 255, 255]);
        }
    }
    let stats = centre_stats(&rgb).unwrap();
    assert!((stats.luma - 255.0).abs() < 0.01, "{}", stats.luma);
    assert_eq!(stats.hist[0][255], 400);
    assert_eq!(stats.hist[1].iter().sum::<u32>(), 400);
    // the weights are Rec. 709, green counting for the most
    let mut green = Rgb::filled(20, 20, [0, 200, 0]);
    green.set(0, 0, [0, 200, 0]);
    assert!((centre_stats(&green).unwrap().luma - 0.7152 * 200.0).abs() < 0.01);
    // nothing to read
    assert!(centre_stats(&Rgb::new(0, 0)).is_none());
}

#[test]
fn a_large_picture_is_sampled_on_a_grid() {
    let big = Rgb::filled(1920, 1080, [100, 100, 100]);
    let stats = centre_stats(&big).unwrap();
    assert!((stats.luma - 100.0).abs() < 0.01);
    let n: u32 = stats.hist[0].iter().sum();
    assert!(n < 960 * 540 / 4, "not every pixel: {n}");
}

#[test]
fn the_identity_curve_changes_nothing_and_a_curve_is_applied_per_channel() {
    let mut rgb = Rgb::filled(4, 4, [10, 20, 30]);
    Lut::identity().apply(&mut rgb);
    assert_eq!(rgb, Rgb::filled(4, 4, [10, 20, 30]));
    assert!(Lut::identity().is_identity());
    let mut lut = Lut::identity();
    lut.0[0] = std::array::from_fn(|i| 255 - i as u8);
    lut.0[2] = [7; 256];
    assert!(!lut.is_identity());
    lut.apply(&mut rgb);
    assert_eq!(rgb.at(2, 2), [245, 20, 7]);
}

#[test]
fn histogram_matching_recovers_a_known_curve_per_channel() {
    // the left eye is the ramp; the right eye saw it darker in red, with a lifted black in green and untouched in blue
    let left = ramp();
    let red = |v: f64| v.powf(1.5);
    let green = |v: f64| 0.8 * v + 0.15;
    let blue = |v: f64| v;
    let right = curved(&left, [&red, &green, &blue]);
    let lut = fit_lut(&stats_of(&right).hist, &stats_of(&left).hist);
    // the curves put the right eye back on the left one wherever the right eye has a value
    let mut fixed = right.clone();
    lut.apply(&mut fixed);
    let (mut worst, mut total, mut n) = (0i32, 0i64, 0i64);
    for x in 12..244 {
        let (a, b) = (fixed.at(x, 5), left.at(x, 5));
        for c in 0..3 {
            let d = (a[c] as i32 - b[c] as i32).abs();
            (worst, total, n) = (worst.max(d), total + d as i64, n + 1);
        }
    }
    assert!(worst <= 6, "worst error {worst}");
    assert!(total as f64 / n as f64 <= 2.0, "mean error {}", total as f64 / n as f64);
    // blue was untouched, so its curve is close to a line
    assert!((0..=255).step_by(5).all(|v| (lut.0[2][v] as i32 - v as i32).abs() <= 2));
}

#[test]
fn a_fitted_curve_is_smooth_and_never_goes_down() {
    // noisy, gappy histograms: two spikes against a flat spread
    let mut from: Hist = [[0; 256]; 3];
    let mut to: Hist = [[0; 256]; 3];
    for c in 0..3 {
        for v in 0..256 {
            from[c][v] = if v % 7 == 0 { 400 } else { 1 };
            to[c][v] = if v % 11 == 0 { 300 } else { 2 };
        }
    }
    let lut = fit_lut(&from, &to);
    for line in &lut.0 {
        assert!(line.windows(2).all(|w| w[0] <= w[1]), "never goes down");
        assert!(line.windows(2).all(|w| w[1] - w[0] <= 40), "no cliffs");
    }
}

#[test]
fn a_narrow_middle_doesnt_crush_the_blacks_or_clip_the_whites() {
    // the middle of both pictures only has values from 100 to 150 (left) and 80 to 120 (right), but the rest of the right picture has more
    let (mut from, mut to): (Hist, Hist) = ([[0; 256]; 3], [[0; 256]; 3]);
    for c in 0..3 {
        for v in 80..=120 {
            from[c][v] = 10;
        }
        for v in 100..=150 {
            to[c][v] = 10;
        }
    }
    let lut = fit_lut(&from, &to);
    for line in &lut.0 {
        assert_eq!(line[0], 0, "black stays black");
        assert_eq!(line[255], 255, "white stays white");
        assert!(line.windows(2).all(|w| w[0] <= w[1]));
        assert!((95..=110).contains(&line[80]) && (140..=155).contains(&line[120]), "{} {}", line[80], line[120]);
        assert!(line[40] > 0 && line[40] < line[80], "a value below the middle's range lands between black and the curve: {}", line[40]);
        assert!(line[200] > line[120] && line[200] < 255);
    }
}

#[test]
fn a_flat_colour_matches_to_a_flat_colour() {
    let (mut from, mut to): (Hist, Hist) = ([[0; 256]; 3], [[0; 256]; 3]);
    for c in 0..3 {
        (from[c][60], to[c][120]) = (500, 500);
    }
    let lut = fit_lut(&from, &to);
    assert!((lut.0[0][60] as i32 - 120).abs() <= 3, "{}", lut.0[0][60]);
}

#[test]
fn matching_a_picture_to_itself_or_to_nothing_changes_nothing() {
    let stats = stats_of(&ramp());
    let lut = fit_lut(&stats.hist, &stats.hist);
    assert!((0..=255).step_by(3).all(|v| (lut.0[0][v] as i32 - v as i32).abs() <= 2));
    let empty: Hist = [[0; 256]; 3];
    assert!(fit_lut(&empty, &stats.hist).is_identity());
    assert!(fit_lut(&stats.hist, &empty).is_identity());
}

// ── Nudging the ISO ───────────────────────────────────────────────────────

#[test]
fn a_picture_within_the_tolerance_is_left_alone() {
    assert_eq!(iso_step(400, 100.0, 100.0, (50, 3200)), IsoStep::Hold);
    assert_eq!(iso_step(400, 102.9, 100.0, (50, 3200)), IsoStep::Hold, "just under 3 %");
    assert_eq!(iso_step(400, 97.1, 100.0, (50, 3200)), IsoStep::Hold);
    assert!(matches!(iso_step(400, 103.5, 100.0, (50, 3200)), IsoStep::Set(_)));
}

#[test]
fn a_dark_picture_gets_a_higher_iso_and_a_bright_one_a_lower() {
    let IsoStep::Set(up) = iso_step(400, 80.0, 100.0, (50, 3200)) else { panic!("a step up") };
    assert!(up > 400, "{up}");
    let IsoStep::Set(down) = iso_step(400, 125.0, 100.0, (50, 3200)) else { panic!("a step down") };
    assert!(down < 400, "{down}");
    // the picture is stored with a gamma, so 80 of 100 asks for about 1.6 times the light
    assert!((up as f64 / 400.0 - 1.63).abs() < 0.05, "{up}");
}

#[test]
fn a_step_is_never_more_than_double_or_half() {
    assert_eq!(iso_step(400, 10.0, 200.0, (50, 12800)), IsoStep::Set(800));
    assert_eq!(iso_step(400, 250.0, 20.0, (50, 12800)), IsoStep::Set(200));
    // a black picture gives no ratio, so it doubles
    assert_eq!(iso_step(400, 0.0, 100.0, (50, 12800)), IsoStep::Set(800));
}

#[test]
fn an_iso_at_the_end_of_the_lens_says_so_and_never_leaves_it() {
    assert_eq!(iso_step(3200, 50.0, 100.0, (50, 3200)), IsoStep::Limit);
    assert_eq!(iso_step(50, 200.0, 100.0, (50, 3200)), IsoStep::Limit);
    assert_eq!(iso_step(2500, 50.0, 100.0, (50, 3200)), IsoStep::Set(3200), "held to the top");
}

/// A camera whose picture brightness (0..255, gamma 2.2) follows its ISO: the right phone sees `scale` times the light of the left one.
fn brightness(iso: i64, scale: f64) -> f64 {
    255.0 * (iso as f64 * scale / 6400.0).powf(1.0 / 2.2).min(1.0)
}

/// Runs the nudging against that camera model, a look every 400 ms; returns what it ended with and the number of steps.
fn run_against(mut run: Run, left_scale: f64, right_scale: f64) -> (Outcome, Run, u64) {
    let mut now = 0u64;
    let mut lag: Option<(u64, [i64; 2])> = None;
    let mut iso = run.iso();
    for _ in 0..200 {
        now += 400 * MS;
        // a change shows in the pictures 300 ms later
        if let Some((at, next)) = lag {
            if now >= at {
                iso = next;
                lag = None;
            }
        }
        let look = Some((now, [brightness(iso[0], left_scale), brightness(iso[1], right_scale)]));
        match run.tick(now, look) {
            Act::Wait => {}
            Act::SetIso { eye, iso: new } => {
                let mut next = lag.map_or(iso, |l| l.1);
                next[if eye == Eye::Left { 0 } else { 1 }] = new;
                lag = Some((now + 300 * MS, next));
            }
            Act::Fit(outcome) => return (outcome, run, now),
        }
    }
    panic!("it never finished");
}

#[test]
fn the_right_iso_is_nudged_until_the_pictures_are_as_bright() {
    // the right phone sees only 40 % of the light the left one does at the same ISO
    let run = Run::new([800, 800], [(50, 12800), (50, 12800)], false, true, 0);
    let (outcome, run, ended) = run_against(run, 1.0, 0.4);
    assert_eq!(outcome, Outcome::Matched);
    assert!(run.steps() <= 3, "a few steps: {}", run.steps());
    assert_eq!(run.iso()[0], 800, "the left phone is left alone");
    let [l, r] = [brightness(run.iso()[0], 1.0), brightness(run.iso()[1], 0.4)];
    assert!(((r - l) / l).abs() <= LUMA_TOLERANCE, "{l} {r}");
    assert!(ended < 8_000 * MS, "a few seconds");
    // the other way round
    let (outcome, run, _) = run_against(Run::new([800, 800], [(50, 12800), (50, 12800)], false, true, 0), 0.5, 1.8);
    assert_eq!(outcome, Outcome::Matched);
    assert!(run.iso()[1] < 800);
}

#[test]
fn a_left_phone_on_automatic_is_brought_to_a_decent_brightness_first() {
    // both start at 400 on a scene that is dark for them: 6400 is a full picture, so the middle is 118 of 255 at about 1400
    let run = Run::new([400, 400], [(50, 12800), (50, 12800)], true, true, 0);
    let (outcome, run, _) = run_against(run, 1.0, 0.7);
    assert_eq!(outcome, Outcome::Matched);
    let [l, r] = [brightness(run.iso()[0], 1.0), brightness(run.iso()[1], 0.7)];
    assert!((l - TARGET_LUMA).abs() <= LUMA_TOLERANCE * TARGET_LUMA + 0.5, "the left went to the target: {l}");
    assert!(((r - l) / l).abs() <= LUMA_TOLERANCE + 0.005, "{l} {r}");
    assert!(run.iso()[0] > 400);
}

#[test]
fn a_phone_that_runs_out_of_iso_ends_the_run_as_limited() {
    // the right lens takes at most 800 and would need far more
    let run = Run::new([800, 800], [(50, 12800), (50, 800)], false, true, 0);
    let (outcome, run, _) = run_against(run, 1.0, 0.1);
    assert_eq!(outcome, Outcome::Limited);
    assert_eq!(run.iso()[1], 800);
}

#[test]
fn a_run_stops_nudging_after_a_few_steps() {
    // a picture that never changes however the ISO moves can't be matched
    let mut run = Run::new([400, 400], [(50, 1_000_000), (50, 1_000_000)], false, true, 0);
    let mut now = 0;
    let mut sets = 0;
    for _ in 0..40 {
        now += 2_000 * MS;
        match run.tick(now, Some((now, [100.0, 40.0]))) {
            Act::SetIso { .. } => sets += 1,
            Act::Fit(outcome) => {
                assert_eq!(outcome, Outcome::Limited);
                assert_eq!(sets, MAX_ISO_STEPS);
                return;
            }
            Act::Wait => {}
        }
    }
    panic!("it never gave up");
}

#[test]
fn a_run_waits_for_a_change_to_show_in_the_pictures() {
    let mut run = Run::new([400, 400], [(50, 12800), (50, 12800)], false, true, 0);
    let now = 5_000 * MS;
    assert_eq!(run.tick(now, None), Act::Wait, "no picture yet");
    assert_eq!(run.tick(now, Some((100 * MS, [100.0, 50.0]))), Act::Wait, "a picture from before the run started settling");
    let Act::SetIso { eye: Eye::Right, iso } = run.tick(now, Some((now, [100.0, 50.0]))) else { panic!("a step") };
    assert!(iso > 400);
    // the next look has to come after the change had time to show
    assert_eq!(run.tick(now + 100 * MS, Some((now + 100 * MS, [100.0, 50.0]))), Act::Wait);
    assert_eq!(run.tick(now + SETTLE_NS + MS, Some((now + SETTLE_NS + MS, [100.0, 100.0]))), Act::Fit(Outcome::Matched));
}

#[test]
fn phones_without_a_manual_exposure_go_straight_to_the_colour() {
    let mut run = Run::new([0, 0], [(0, 0); 2], false, false, 0);
    assert_eq!(run.tick(10 * MS, Some((10 * MS, [100.0, 20.0]))), Act::Wait, "not before the first settled look");
    assert_eq!(run.tick(SETTLE_NS + MS, Some((SETTLE_NS + MS, [100.0, 20.0]))), Act::Fit(Outcome::Matched));
}

// ── Reading the phones ────────────────────────────────────────────────────

fn phone(auto: bool, iso: Option<i64>, shutter: Option<i64>, focal: i64, fps: i64) -> Value {
    json!({
        "cameras": [
            {"id": "0", "current": false, "focalMm": 13, "isoMin": 50, "isoMax": 3200, "shutterMinNs": 100_000, "shutterMaxNs": 500_000_000, "supportsManualSensor": true},
            {"id": "2", "current": true, "focalMm": focal, "isoMin": 100, "isoMax": 6400, "shutterMinNs": 50_000, "shutterMaxNs": 1_000_000_000, "supportsManualSensor": true},
        ],
        "active_lens": "2", "auto": auto, "iso": iso, "shutter_ns": shutter, "phone_fps": fps,
    })
}

#[test]
fn the_lens_in_use_is_the_active_one_then_the_current_one() {
    let state = phone(true, None, None, 24, 30);
    assert_eq!(active_lens(&state).unwrap()["id"], json!("2"));
    assert_eq!(iso_range(&state), Some((100, 6400)));
    assert_eq!(shutter_range(&state), Some((50_000, 1_000_000_000)));
    assert_eq!(focal_mm(&state), Some(24.0));
    let mut no_active = state.clone();
    no_active["active_lens"] = Value::Null;
    assert_eq!(active_lens(&no_active).unwrap()["id"], json!("2"), "the one marked current");
    no_active["cameras"][1]["current"] = json!(false);
    assert_eq!(active_lens(&no_active).unwrap()["id"], json!("0"), "else the first");
    assert!(active_lens(&json!({})).is_none());
    assert_eq!(focal_mm(&json!({"cameras": [{"id": "1"}]})), None, "a phone that doesn't say");
}

#[test]
fn a_manual_phone_gives_its_values_and_an_automatic_one_gives_none() {
    assert_eq!(manual_exposure(&phone(false, Some(400), Some(8_000_000), 24, 30)), Some((400, 8_000_000)));
    assert_eq!(manual_exposure(&phone(true, None, None, 24, 30)), None);
    assert_eq!(manual_exposure(&phone(false, Some(400), None, 24, 30)), None, "one of the two is no manual exposure");
    assert_eq!(manual_exposure(&phone(true, Some(400), Some(8_000_000), 24, 30)), None);
}

#[test]
fn both_phones_get_the_same_shutter_the_frame_rate_allows() {
    let manual = phone(false, Some(400), Some(8_000_000), 24, 30);
    let auto = phone(true, None, None, 24, 30);
    assert_eq!(pick_shutter(&manual, &auto, 33_333_333), 8_000_000, "the left phone's");
    assert_eq!(pick_shutter(&auto, &manual, 33_333_333), 8_000_000, "else the right phone's");
    assert_eq!(pick_shutter(&auto, &auto, 33_333_333), DEFAULT_SHUTTER_NS, "else 1/60 s");
    // a long shutter is cut to a frame at 30 fps, and to less at 60
    let slow = phone(false, Some(100), Some(200_000_000), 24, 30);
    assert_eq!(pick_shutter(&slow, &auto, 33_333_333), 30_000_000);
    let fast = phone(false, Some(100), Some(200_000_000), 24, 60);
    assert_eq!(pick_shutter(&fast, &auto, 33_333_333), 15_000_000);
    // with no rate from the phone the stream's own frame time is used
    let mut no_fps = slow.clone();
    no_fps["phone_fps"] = Value::Null;
    assert_eq!(pick_shutter(&no_fps, &auto, 16_666_667), 15_000_000);
    // and what both lenses take: this one stops at 100 us
    let mut short = phone(false, Some(100), Some(60_000), 24, 30);
    short["cameras"][1]["shutterMinNs"] = json!(100_000);
    assert_eq!(pick_shutter(&short, &auto, 33_333_333), 100_000);
}

#[test]
fn the_start_holds_each_phones_iso_to_its_lens_and_says_if_the_left_one_was_a_guess() {
    let left = phone(false, Some(5000), Some(8_000_000), 24, 30);
    let mut right = phone(true, None, None, 26, 30);
    right["cameras"][1]["isoMax"] = json!(3200);
    let start = plan(&left, &right, 33_333_333).unwrap();
    assert_eq!(start.shutter, 8_000_000);
    assert_eq!(start.iso, [5000, 3200], "the right lens stops at 3200");
    assert!(!start.tune_left);
    // a left phone on automatic starts from the default and has to be brought to a decent brightness
    let start = plan(&phone(true, None, None, 24, 30), &right, 33_333_333).unwrap();
    assert_eq!((start.iso, start.tune_left), ([START_ISO, START_ISO], true));
    // a phone that can't take a manual exposure gives no plan
    let mut fixed = left.clone();
    fixed["cameras"][1]["supportsManualSensor"] = json!(false);
    assert!(plan(&fixed, &right, 33_333_333).is_none());
    assert!(plan(&left, &json!({"cameras": []}), 33_333_333).is_none());
}

#[test]
fn the_bodies_are_the_phones_own_control_actions() {
    let [iso, shutter] = exposure_bodies(640, 8_333_333);
    assert_eq!(iso, json!({"action": "iso", "value": 640}));
    assert_eq!(shutter, json!({"action": "shutter", "value": 8_333_333}));
    assert_eq!(iso_body(100), json!({"action": "iso", "value": 100}));
}

// ── Size from the lenses ──────────────────────────────────────────────────

#[test]
fn size_comes_from_the_ratio_of_the_fields_of_view() {
    assert_eq!(size_from_focal(24.0, 24.0), Some(100.0));
    // the right lens sees wider (shorter focal length): its picture is enlarged to match
    assert_eq!(size_from_focal(26.0, 24.0), Some(108.3));
    // the right lens sees less: it is shrunk
    assert_eq!(size_from_focal(24.0, 26.0), Some(92.3));
    // the tangent of half the field of view goes with the inverse of the focal length
    let (a, b) = (size_from_focal(30.0, 24.0).unwrap(), 30.0 / 24.0 * 100.0);
    assert!((a - b).abs() < 0.1, "{a} {b}");
    // a wide lens against a telephoto is off the slider, and nothing is set
    assert_eq!(size_from_focal(24.0, 48.0), None);
    assert_eq!(size_from_focal(48.0, 24.0), None);
    assert_eq!(size_from_focal(0.0, 24.0), None);
    assert_eq!(size_from_focal(24.0, -1.0), None);
}
