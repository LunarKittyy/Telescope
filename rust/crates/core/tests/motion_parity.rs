//! core::motion against what telescope/motion.py does on the same pictures (recorded from the Python app), and test_motion.py's checks.

use serde_json::Value;
use std::sync::{Arc, Mutex};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::image::frame_rgb;
use telescope_core::motion::*;
use telescope_core::vision::{Turn, Upright};

const W: usize = 640;
const H: usize = 360;
const LEFT: [f64; 4] = [0.0, 0.0, 0.4, 1.0];
const RIGHT: [f64; 4] = [0.6, 0.0, 1.0, 1.0];

/// A grey room with a little texture (a hash of the pixel's place, as the fixtures make it), and a bright square at square_x if there is one.
fn scene(brightness: f64, square_x: Option<usize>, size: usize) -> Frame {
    let mut data = Vec::with_capacity(W * H * 4);
    for y in 0..H {
        for x in 0..W {
            let mut px = [0u8; 4];
            for (c, v) in px.iter_mut().take(3).enumerate() {
                // The fixtures' pictures are BGR, so the texture's first channel is the blue one
                let i = ((y * W + x) * 3 + (2 - c)) as u64;
                let noise = ((i.wrapping_mul(2654435761) >> 7) % 7) as i64 - 3;
                *v = (brightness + noise as f64).clamp(0.0, 255.0) as u8;
            }
            if square_x.is_some_and(|sx| (150..150 + size).contains(&y) && (sx..sx + size).contains(&x)) {
                px[..3].fill(230);
            }
            px[3] = 255;
            data.extend_from_slice(&px);
        }
    }
    Frame::new(W as u32, H as u32, PixelFormat::Rgba, data)
}

fn plain() -> Frame {
    scene(100.0, None, 60)
}

fn square(x: usize) -> Frame {
    scene(100.0, Some(x), 60)
}

fn paint(frame: &Frame, rows: std::ops::Range<usize>, cols: std::ops::Range<usize>, value: u8) -> Frame {
    let mut out = frame.clone();
    let data = out.data_mut();
    for y in rows {
        for x in cols.clone() {
            data[(y * W + x) * 4..(y * W + x) * 4 + 3].fill(value);
        }
    }
    out
}

type Events = Arc<Mutex<Vec<(String, String, Vec<u32>)>>>;

/// A Motion watching the left and right of the picture for "trap", fed as stream "s".
struct Run {
    m: Motion,
    events: Events,
    trace: Vec<Vec<u32>>,
}

impl Run {
    fn new(sensitivity: i64) -> Run {
        let events: Events = Arc::default();
        let log = events.clone();
        let m = Motion::new(Box::new(move |key, owner, zones| log.lock().unwrap().push((key.to_string(), owner.to_string(), zones.to_vec()))));
        m.watch("trap", &[LEFT, RIGHT], sensitivity);
        Run { m, events, trace: Vec::new() }
    }

    fn feed(&mut self, frame: &Frame, t: f64) {
        self.m.feed(frame, "s", None, t);
        self.trace.push(self.m.moving("s", "trap"));
    }

    fn zones(&self) -> Vec<Vec<u32>> {
        self.events.lock().unwrap().iter().map(|e| e.2.clone()).collect()
    }

    fn moving(&self) -> Vec<u32> {
        self.m.moving("s", "trap")
    }
}

fn recorded() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/motion.json");
    telescope_core::json::loads(&std::fs::read_to_string(path).expect("motion.json")).expect("json")
}

/// What Python's run of this scenario did, which this one has to have done the same.
fn same_as_python(name: &str, run: &Run) {
    let want = &recorded()[name];
    let trace: Vec<Vec<u32>> = want["trace"].as_array().unwrap().iter().map(|t| t.as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect()).collect();
    assert_eq!(run.trace, trace, "{name}: which zones moved after each frame");
    let events: Vec<(String, String, Vec<u32>)> = want["events"].as_array().unwrap().iter()
        .map(|e| (e[0].as_str().unwrap().into(), e[1].as_str().unwrap().into(), e[2].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect())).collect();
    assert_eq!(*run.events.lock().unwrap(), events, "{name}: what it told the owner");
}

#[test]
fn thresholds_get_finer_with_sensitivity() {
    let (low, high) = (thresholds(1), thresholds(10));
    assert!(low.0 > high.0 && low.1 > high.1);
    assert_eq!((thresholds(0), thresholds(99)), (low, high));
}

#[test]
fn zones_are_cleaned_up() {
    assert_eq!(clean_zones(&[[0.5, 0.6, 0.1, 0.2], [0.0, 0.0, 0.0, 1.0], [f64::NAN, 0.0, 1.0, 1.0], [-1.0, 0.0, 2.0, 0.5]]),
               [(0.1, 0.2, 0.5, 0.6), (0.0, 0.0, 1.0, 0.5)]);
    assert!(clean_zones(&[]).is_empty());
    // The host's f32 zones mean the decimal numbers they were written as
    assert_eq!(widen(&[[0.6, 0.4, 1.0, 0.75]]), [[0.6, 0.4, 1.0, 0.75]]);
}

#[test]
fn a_square_moving_in_and_out_of_a_zone() {
    let mut r = Run::new(5);
    let mut t = 0.0;
    for _ in 0..20 {
        r.feed(&plain(), t);
        t += 0.1;
    }
    assert!(r.zones().is_empty(), "an empty room settles in");
    for x in (400..600).step_by(20) {
        r.feed(&square(x), t);
        t += 0.1;
    }
    let events = r.events.lock().unwrap().clone();
    assert_eq!(events[0], ("s".to_string(), "trap".to_string(), vec![1]));
    drop(events);
    assert_eq!(r.moving(), [1]);
    for _ in 0..30 {
        r.feed(&plain(), t);
        t += 0.1;
    }
    assert_eq!(r.events.lock().unwrap().last().unwrap().2, Vec::<u32>::new());
    assert!(r.moving().is_empty(), "gone again: the zone calms down once the hold is over");
    assert!(r.zones().iter().all(|z| !z.contains(&0)), "nothing ever moved on the left");
    same_as_python("square_in_and_out", &r);
}

#[test]
fn a_slow_brightness_ramp_doesnt_count() {
    let mut r = Run::new(5);
    for i in 0..300 {
        r.feed(&scene(60.0 + i as f64 * 0.4, None, 60), i as f64 * 0.1);
    }
    assert!(r.zones().is_empty(), "30 seconds from dim to bright, like the sun coming out");
    same_as_python("brightness_ramp", &r);
}

#[test]
fn the_background_takes_in_something_left_behind() {
    let mut r = Run::new(5);
    let mut t = 0.0;
    for _ in 0..10 {
        r.feed(&plain(), t);
        t += 0.1;
    }
    for _ in 0..100 {
        r.feed(&square(500), t);
        t += 0.1;
    }
    let zones = r.zones();
    assert_eq!((zones.first(), zones.last()), (Some(&vec![1]), Some(&vec![])), "a box put down on the right and left there");
    same_as_python("left_behind", &r);
}

#[test]
fn stopping_watching_stops_the_reports() {
    let mut r = Run::new(5);
    r.m.watch("trap", &[], 5);
    assert!(!r.m.watching());
    r.feed(&plain(), 0.0);
    r.feed(&square(500), 1.0);
    assert!(r.zones().is_empty());
}

fn change_after(first: &Frame, second: &Frame) -> (Grey, Grey, Vec<u8>) {
    let mut d = Detector::new();
    let (a, b) = (grey(first).unwrap(), grey(second).unwrap());
    d.change(&a, 0.0);
    let change = d.change(&b, 0.1).expect("a change");
    (a, b, change)
}

fn fnv(data: &[u8]) -> String {
    let mut h = 0xcbf29ce484222325u64;
    for &b in data {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[test]
fn small_zones_and_sensitivity() {
    let (a, b, change) = change_after(&plain(), &scene(100.0, Some(500), 12));
    let small = (0.75, 0.38, 0.85, 0.5);
    let zones = |zones: &[Zone], sensitivity: i64| {
        let (level, share) = thresholds(sensitivity);
        moving_zones(&change, b.w, b.h, zones, level, share).into_iter().collect::<Vec<_>>()
    };
    let (left, right) = ((LEFT[0], LEFT[1], LEFT[2], LEFT[3]), (RIGHT[0], RIGHT[1], RIGHT[2], RIGHT[3]));
    assert_eq!(zones(&[small, left], 5), [0]);
    assert!(zones(&[right], 1).is_empty(), "a tiny thing in a big zone, at the lowest");
    // and the pictures Python made of them: the grey copy made from the whole RGB picture is still byte for byte what it made
    let want = &recorded()["detector"];
    let (first, second) = (plain(), scene(100.0, Some(500), 12));
    let whole = |f: &Frame| grey_copy(&frame_rgb(f).unwrap(), W, H, WIDTH);
    let (a0, b0) = (whole(&first), whole(&second));
    let mut d = Detector::new();
    d.change(&a0, 0.0);
    let change0 = d.change(&b0, 0.1).expect("a change");
    assert_eq!((fnv(&a0.data), fnv(&b0.data), fnv(&change0)), (want["grey"].as_str().unwrap().to_string(), want["grey_square"].as_str().unwrap().to_string(), want["change"].as_str().unwrap().to_string()));
    assert_eq!([b.h as u64, b.w as u64], [want["shape"][0].as_u64().unwrap(), want["shape"][1].as_u64().unwrap()]);
    // the one made from the shrunk planes is close to it: the same where nothing is, a hard edge a few levels off
    for (new, old) in [(&a, &a0), (&b, &b0)] {
        assert_eq!((new.w, new.h), (old.w, old.h));
        let off = new.data.iter().zip(&old.data).filter(|(x, y)| x.abs_diff(**y) > 8).count();
        assert!(off * 100 < new.data.len(), "{off} of {} grey values are far from the whole-picture ones", new.data.len());
    }
    assert!(change.iter().zip(&change0).filter(|(x, y)| x.abs_diff(**y) > 20).count() * 50 < change.len());
    assert_eq!(want["small_zones"], serde_json::json!([0]));
}

#[test]
fn zones_follow_the_phones_share_of_the_zoom() {
    // The phone sends the middle half of the picture: the right zone of the whole picture is the right edge of it
    let to_shown = |u: f64, v: f64| (0.25 + u * 0.5, 0.25 + v * 0.5);
    let close = |a: &[Zone], b: &[Zone]| a.len() == b.len() && a.iter().zip(b).all(|(p, q)| (p.0 - q.0).abs() < 1e-9 && (p.1 - q.1).abs() < 1e-9 && (p.2 - q.2).abs() < 1e-9 && (p.3 - q.3).abs() < 1e-9);
    assert!(close(&to_frame(&[(0.6, 0.0, 1.0, 1.0)], Some(&to_shown)), &[(0.7, 0.0, 1.0, 1.0)]));
    assert_eq!(to_frame(&[(0.0, 0.0, 0.2, 1.0)], Some(&to_shown)), [(0.0, 0.0, 0.0, 0.0)], "off the frame: never moves");
    assert_eq!(to_frame(&[(0.1, 0.2, 0.3, 0.4)], None), [(0.1, 0.2, 0.3, 0.4)]);
}

#[test]
fn a_dark_coat_filling_part_of_the_picture_doesnt_trip_a_still_wall() {
    let mut r = Run::new(8);
    let wall = scene(200.0, None, 60);
    let mut t = 0.0;
    for _ in 0..10 {
        r.feed(&wall, t);
        t += 0.1;
    }
    let coat = paint(&wall, 0..H, 0..210, 100);
    for _ in 0..10 {
        r.feed(&coat, t);
        t += 0.1;
    }
    assert!(r.zones().iter().all(|z| !z.contains(&1)));
    same_as_python("dark_coat", &r);
}

#[test]
fn coming_back_after_a_gap_starts_the_background_over() {
    let mut r = Run::new(5);
    r.feed(&plain(), 0.0);
    r.feed(&plain(), 0.1);
    r.feed(&square(500), 30.0); // the panels were on another stream for half a minute
    r.feed(&square(500), 30.1);
    assert!(r.zones().is_empty() && r.moving().is_empty());
    same_as_python("after_a_gap", &r);
}

#[test]
fn a_lens_switch_picked_here_is_not_motion_even_in_a_room_of_similar_colours() {
    let mut r = Run::new(5);
    for i in 0..5 {
        r.feed(&square(100), i as f64 * 0.1);
    }
    r.m.reset(LENS_SETTLE_S, 0.45);
    r.feed(&square(100), 0.5); // the old lens's last frames
    for i in 6..30 {
        r.feed(&scene(100.0, Some(400), 120), i as f64 * 0.1); // the tele lens: same colours, everything bigger and moved
    }
    assert!(r.zones().is_empty() && r.moving().is_empty());
    same_as_python("lens_switch", &r);
}

#[test]
fn someone_filling_most_of_the_picture_still_counts() {
    let mut r = Run::new(5);
    for i in 0..5 {
        r.feed(&plain(), i as f64 * 0.1);
    }
    // Right up to the lens: about 80% of the picture, the top corners still the room
    r.feed(&paint(&plain(), 60..H, 40..600, 230), 0.5);
    assert_eq!(r.moving(), [0, 1]);
    same_as_python("filling_the_picture", &r);
}

#[test]
fn everything_changing_at_once_is_a_cut_not_motion() {
    let mut r = Run::new(5);
    for i in 0..5 {
        r.feed(&scene(60.0, None, 60), i as f64 * 0.1);
    }
    let mut flipped = scene(60.0, None, 60);
    for p in flipped.data_mut().chunks_mut(4) {
        for v in &mut p[..3] {
            *v = 255 - *v;
        }
    }
    for i in 5..10 {
        r.feed(&flipped, i as f64 * 0.1);
    }
    assert!(r.zones().is_empty(), "everything changed at once");
    same_as_python("a_cut", &r);
}

#[test]
fn watching_again_or_turning_the_picture_starts_over() {
    let mut r = Run::new(5);
    for i in 0..5 {
        r.feed(&plain(), i as f64 * 0.1);
    }
    r.feed(&square(500), 0.5);
    assert_eq!(r.moving(), [1]);
    r.m.watch("trap", &[LEFT, RIGHT], 5);
    assert!(r.moving().is_empty());
    r.trace.push(r.moving());
    r.m.reset(0.0, 0.55);
    r.feed(&plain(), 0.6); // a turned picture: no difference with the background from before it
    assert!(r.moving().is_empty());
    same_as_python("watching_again", &r);
}

#[test]
fn the_picture_is_turned_the_way_its_shown_before_it_is_looked_at() {
    // A square on the right of the frame, seen through a flip, moves in the zone on the left of the picture as shown
    let upright = Upright { turn: Turn { flip_h: true, flip_v: false, rotation: None }, sent: (1.0, 0.0, 0.0) };
    let events: Events = Arc::default();
    let log = events.clone();
    let m = Motion::new(Box::new(move |key, owner, zones| log.lock().unwrap().push((key.to_string(), owner.to_string(), zones.to_vec()))));
    m.watch("trap", &[LEFT, RIGHT], 5);
    let mut t = 0.0;
    for _ in 0..10 {
        m.feed(&plain(), "s", Some(&upright), t);
        t += 0.1;
    }
    for x in (400..600).step_by(20) {
        m.feed(&square(x), "s", Some(&upright), t);
        t += 0.1;
    }
    assert_eq!(m.moving("s", "trap"), [0]);
}


/// (wall ms, cpu ms) a call takes, the best wall of a few runs and the cpu over all of them (every thread of the process, so work spread over the workers counts in full).
fn measure(f: &mut dyn FnMut()) -> (f64, f64) {
    let cpu = || {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let fields: Vec<&str> = stat.rsplit_once(')').map_or("", |(_, rest)| rest).split_whitespace().collect();
        let ticks = |i: usize| fields.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        (ticks(11) + ticks(12)) / 100.0 * 1000.0
    };
    f();
    let runs = 60;
    let before = cpu();
    let wall = (0..runs).map(|_| {
        let start = std::time::Instant::now();
        f();
        start.elapsed().as_secs_f64() * 1000.0
    }).fold(f64::MAX, f64::min);
    (wall, (cpu() - before) / runs as f64)
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture --test-threads 1"]
fn bench_grey_copy() {
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let size = PixelFormat::I420.size(w as u32, h as u32);
        let frame = Frame::new(w as u32, h as u32, PixelFormat::I420, (0..size).map(|i| (60 + i % 90) as u8).collect());
        let old = measure(&mut || { grey_copy(&frame_rgb(&frame).unwrap(), w, h, WIDTH); });
        let new = measure(&mut || { grey(&frame).unwrap(); });
        println!("motion grey copy {w}x{h} (wall ms, cpu ms): old {old:.2?} new {new:.2?}");
    }
}
