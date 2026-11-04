use super::*;
use std::cell::RefCell;

thread_local! {
    static GOT: RefCell<Vec<Stall>> = const { RefCell::new(Vec::new()) };
}

fn collect(stall: Stall) {
    GOT.with(|g| g.borrow_mut().push(stall));
}

fn got() -> Vec<Stall> {
    GOT.with(|g| std::mem::take(&mut *g.borrow_mut()))
}

const SECOND: u64 = 1_000_000_000;
const MS: u64 = 1_000_000;

fn context() -> (DecoderKind, Foreground) {
    (DecoderKind::Hardware, Foreground::Fullscreen)
}

#[test]
fn stage_times_are_summed_up_over_the_latest_ones() {
    // Only this test records, so the stage is its own
    for i in 1..=100u64 {
        record("readback", i * MS);
    }
    let all = stage_summaries();
    let s = all.iter().find(|s| s.stage == "readback").unwrap();
    assert_eq!((s.count, s.p50_ms, s.p99_ms, s.max_ms), (100, 50.0, 99.0, 100.0));
    assert!(stage_line().unwrap().contains("readback 50/99/100"));
    // The stall tests' watches take peaks too, so a peak may be gone; the slowest stage then is whichever is left
    let peak = (0..50).find_map(|_| {
        record("camera", 7 * MS);
        take_peak().filter(|p| p.0 == "camera")
    });
    assert_eq!(peak, Some(("camera", 7.0)));
    assert!(take_peak().is_none_or(|p| p.0 != "camera"), "a peak is taken once");
    record("nonsense", MS);
    assert!(stage_summaries().iter().all(|s| s.stage != "nonsense"));
    // Past the window only the latest count
    for _ in 0..WINDOW {
        record("readback", MS);
    }
    let again = stage_summaries().into_iter().find(|s| s.stage == "readback").unwrap();
    assert_eq!((again.count, again.p99_ms, again.max_ms), (100 + WINDOW as u64, 1.0, 1.0));
}

#[test]
fn milliseconds_are_short() {
    assert_eq!((ms(0.04), ms(3.14159), ms(9.96), ms(10.4), ms(2010.0)), ("0.0".into(), "3.1".into(), "10.0".into(), "10".into(), "2010".into()));
}

#[test]
fn a_gap_between_frames_is_a_stall_with_the_windows_around_it() {
    let mut watch = StallWatch::into(collect);
    watch.frame(0, &context);
    watch.frame(33 * MS, &context);
    watch.window(2.0, 30.0, 8.0, true, &context);
    watch.frame(33 * MS + 2100 * MS, &context);
    assert!(got().is_empty(), "the window after it isn't known yet");
    watch.window(2.0, 12.0, 3.0, true, &context);
    assert!(got().is_empty());
    watch.window(2.0, 29.0, 7.5, true, &context);
    let stalls = got();
    assert_eq!(stalls.len(), 1);
    let s = &stalls[0];
    assert_eq!((s.ms, s.gap, s.before, s.after, s.decoder, s.foreground), (2100, true, Some((30.0, 8.0)), Some((29.0, 7.5)), DecoderKind::Hardware, Foreground::Fullscreen));
    assert!(s.line().contains("2100 ms gap") && s.line().contains("fps/Mbps 30/8.0 then 29/7.5") && s.line().contains("hardware decoder, front: fullscreen (other app)"), "{}", s.line());
}

#[test]
fn frames_a_little_apart_are_not_a_stall() {
    let mut watch = StallWatch::into(collect);
    for i in 0..100u64 {
        watch.frame(i * 200 * MS, &context);
    }
    watch.window(2.0, 5.0, 1.0, true, &context);
    watch.finish();
    assert!(got().is_empty());
}

#[test]
fn a_window_under_half_the_rate_before_it_is_a_stall_when_no_gap_showed() {
    let mut watch = StallWatch::into(collect);
    watch.window(2.0, 30.0, 8.0, true, &context);
    watch.window(2.0, 14.0, 4.0, true, &context);
    watch.window(2.0, 28.0, 8.0, true, &context);
    let stalls = got();
    assert_eq!(stalls.len(), 1);
    assert_eq!((stalls[0].ms, stalls[0].gap, stalls[0].before, stalls[0].after), (2000, false, Some((30.0, 8.0)), Some((28.0, 8.0))));
    // Not while settling, not from a slow start, not from half
    let mut quiet = StallWatch::into(collect);
    quiet.window(2.0, 30.0, 8.0, true, &context);
    quiet.window(2.0, 3.0, 1.0, false, &context);
    quiet.window(2.0, 3.0, 1.0, true, &context);
    quiet.window(2.0, 1.0, 1.0, true, &context);
    quiet.finish();
    assert!(got().is_empty());
    let mut slow_start = StallWatch::into(collect);
    slow_start.window(2.0, 8.0, 1.0, true, &context);
    slow_start.window(2.0, 1.0, 0.1, true, &context);
    slow_start.finish();
    assert!(got().is_empty());
    let mut gap_and_drop = StallWatch::into(collect);
    gap_and_drop.window(2.0, 30.0, 8.0, true, &context);
    gap_and_drop.frame(SECOND, &context);
    gap_and_drop.frame(3 * SECOND, &context);
    gap_and_drop.window(2.0, 10.0, 8.0, true, &context);
    gap_and_drop.finish();
    assert_eq!(got().len(), 1, "one moment is one stall");
}

#[test]
fn the_session_keeps_the_ten_worst_stalls() {
    let stall = |ms: u64| Stall { at: "12:00:00".into(), ms, gap: true, stage: "decode", stage_ms: ms as f64, before: None, after: None,
                                  decoder: DecoderKind::Unknown, foreground: Foreground::Unknown };
    for ms in [300, 900, 400, 5000, 350, 260, 270, 280, 290, 310, 2000, 255] {
        keep_stall(stall(ms));
    }
    let worst: Vec<u64> = worst_stalls().iter().map(|s| s.ms).collect();
    assert_eq!(worst, [5000, 2000, 900, 400, 350, 310, 300, 290, 280, 270]);
}

#[test]
fn the_decoder_in_use_is_told_from_the_status() {
    let of = |b: &[&str]| DecoderKind::of(&b.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!((of(&[]), of(&["processor"]), of(&["d3d11va"]), of(&["processor", "vaapi"])), (DecoderKind::Unknown, DecoderKind::Software, DecoderKind::Hardware, DecoderKind::Hardware));
}

fn sample(seconds_ago: u64, now: u64, slot: u32, fps: f32, mbps: f32) -> Sample {
    Sample { ns: now - seconds_ago * SECOND, slot, fps, mbps }
}

#[test]
fn the_timeline_is_a_character_for_two_seconds() {
    let now = 1000 * SECOND;
    // 20 s of a stream: 30 fps, a dip to 15 and a stop, then back
    let mut samples: Vec<Sample> = (0..10).map(|i| sample(20 - 2 * i, now, 1, 30.0, 8.0)).collect();
    samples[4].fps = 15.0;
    samples[5].fps = 0.0;
    samples[5].mbps = 0.0;
    // Another stream's windows aren't drawn
    samples.insert(3, sample(14, now, 2, 99.0, 99.0));
    let lines = timeline_from(&samples, now);
    assert_eq!(lines.len(), 2);
    let fps = lines[0].split('|').nth(1).unwrap();
    assert_eq!(fps.len(), COLUMNS);
    assert_eq!(&fps[..49], "_".repeat(49), "no stream before it");
    assert_eq!(&fps[49..], "####= ####_");
    assert!(lines[0].ends_with("min 0.0 max 30.0") && lines[1].ends_with("min 0.0 max 8.0"), "{lines:?}");
    assert!(lines[0].starts_with("fps  |") && lines[1].starts_with("Mbps |"));
    assert!(timeline_from(&[], now).is_empty());
    assert!(timeline_from(&[sample(500, now, 1, 30.0, 8.0)], now).is_empty(), "older than two minutes");
}
