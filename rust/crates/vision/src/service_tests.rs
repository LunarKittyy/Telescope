//! test_vision.py, by name, against a fake MediaPipe (the real one is in tests/parity.rs).

use super::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use telescope_core::effort::{FULL, MOST};
use telescope_core::vision::Turn;

#[derive(Default)]
pub(crate) struct Log {
    pub segmented: Vec<(usize, usize)>,
    pub timestamps: Vec<i64>,
    pub closed: Vec<&'static str>,
    pub made: Vec<&'static str>,
    pub posed: Vec<(usize, usize)>,
}

type Detect = Arc<dyn Fn() -> Vec<Person> + Send + Sync>;

pub(crate) struct Fake {
    pub log: Arc<Mutex<Log>>,
    /// The values the segmenter's masks are filled with in turn (200 once they run out).
    pub values: Arc<Mutex<Vec<u8>>>,
    pub broken_segmenter: Mutex<Option<VisionError>>,
    pub detect: Mutex<Detect>,
}

impl Fake {
    pub fn new() -> Arc<Fake> {
        Arc::new(Fake {
            log: Default::default(),
            values: Default::default(),
            broken_segmenter: Mutex::new(None),
            detect: Mutex::new(Arc::new(|| vec![Person::new([("nose", 0.5, 0.25, 0.9), ("left_wrist", 0.1, 0.5, 0.2)])])),
        })
    }

    fn closed(&self) -> Vec<&'static str> {
        self.log.lock().unwrap().closed.clone()
    }
}

struct FakeSeg {
    log: Arc<Mutex<Log>>,
    values: Arc<Mutex<Vec<u8>>>,
}

impl Segmenter for FakeSeg {
    fn segment(&mut self, frame: &Rgb, ts_ms: i64) -> Result<Gray, VisionError> {
        let mut log = self.log.lock().unwrap();
        log.segmented.push((frame.w, frame.h));
        log.timestamps.push(ts_ms);
        let mut values = self.values.lock().unwrap();
        let v = if values.is_empty() { 200 } else { values.remove(0) };
        Ok(Gray { w: frame.w, h: frame.h, data: vec![v; frame.w * frame.h] })
    }
}

impl Drop for FakeSeg {
    fn drop(&mut self) {
        if let Ok(mut log) = self.log.lock() {
            log.closed.push("segmenter");
        }
    }
}

struct FakePose {
    log: Arc<Mutex<Log>>,
    detect: Detect,
}

impl PoseModel for FakePose {
    fn detect(&mut self, frame: &Rgb, _ts: i64) -> Result<Vec<Person>, VisionError> {
        self.log.lock().unwrap().posed.push((frame.w, frame.h));
        Ok((self.detect)())
    }
}

impl Drop for FakePose {
    fn drop(&mut self) {
        if let Ok(mut log) = self.log.lock() {
            log.closed.push("pose");
        }
    }
}

impl Backend for Fake {
    fn version(&self) -> String {
        "fake".into()
    }
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        self.log.lock().unwrap().made.push("segmenter");
        if let Some(e) = self.broken_segmenter.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(Box::new(FakeSeg { log: self.log.clone(), values: self.values.clone() }))
    }
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        self.log.lock().unwrap().made.push("pose");
        Ok(Box::new(FakePose { log: self.log.clone(), detect: self.detect.lock().unwrap().clone() }))
    }
}

pub(crate) type Seen = Arc<Mutex<Vec<(String, Vec<Person>)>>>;

pub(crate) struct Ready {
    pub v: Vision,
    pub backend: Arc<Fake>,
    pub states: Arc<Mutex<Vec<String>>>,
    pub people: Seen,
}

pub(crate) fn make(find: Find) -> (Vision, Arc<Mutex<Vec<String>>>, Seen) {
    let (states, people): (Arc<Mutex<Vec<String>>>, Seen) = Default::default();
    let (s, p) = (states.clone(), people.clone());
    let v = Vision::with_effort(Arc::new(Effort::free()), Box::new(move |k, found| p.lock().unwrap().push((k.to_string(), found))),
                                Box::new(move |st| s.lock().unwrap().push(st.to_string())), find, None);
    (v, states, people)
}

pub(crate) fn ready() -> Ready {
    let backend = Fake::new();
    let b = backend.clone();
    let (v, states, people) = make(Box::new(move || Ok(b.clone() as Arc<dyn Backend>)));
    v.load();
    wait(|| v.state() == READY);
    Ready { v, backend, states, people }
}

pub(crate) fn wait(condition: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < end, "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// a bit more than Windows' clock step (about 16 ms), so turning a clock back by an interval is past it there too
const TICK: f64 = 0.02;

pub(crate) fn mask(v: &Vision, key: &str, seq: u64, frame: &Rgb) -> Option<Arc<Vec<u8>>> {
    v.person_mask(key, seq, frame.w, frame.h, &mut || Some(frame.clone()))
}

pub(crate) fn feed(v: &Vision, key: &str, frame: &Rgb) {
    v.feed_raw(key, &mut || Some(crate::ops::thumb(frame)), &mut || Some(frame.clone()), Upright::default())
}

fn sv(v: &Vision, key: &str) -> Arc<StreamVision> {
    v.inner.streams.lock().unwrap()[key].clone()
}

// debug builds shrink a 720p frame slower than MASK_STILL_EVERY_S, so the mask's clock is held still between calls
const HELD: f64 = 1000.0;

fn hold_mask_clock(v: &Vision) {
    sv(v, "a").mask.lock().unwrap().at += HELD;
}

fn age_mask_clock(v: &Vision) {
    sv(v, "a").mask.lock().unwrap().at -= HELD + cadence(1).mask.still + TICK;
}

pub(crate) fn with_arm(base: &Rgb, x: (usize, usize), y: (usize, usize), value: u8) -> Rgb {
    let mut out = base.clone();
    for row in y.0..y.1 {
        for col in x.0..x.1 {
            let i = (row * out.w + col) * 3;
            out.data[i..i + 3].fill(value);
        }
    }
    out
}

#[test]
fn test_a_mask_is_worked_out_once_per_frame_and_per_stream() {
    let r = ready();
    let frame = Rgb::filled(6, 4, 0);
    let first = mask(&r.v, "a", 1, &frame).unwrap();
    let again = mask(&r.v, "a", 1, &frame).unwrap();
    let other = mask(&r.v, "b", 1, &frame).unwrap();
    mask(&r.v, "a", 2, &Rgb::filled(6, 4, 255));
    assert!(Arc::ptr_eq(&first, &again) && first.len() == 24);
    assert!(!Arc::ptr_eq(&other, &first));
    let log = r.backend.log.lock().unwrap();
    assert_eq!(log.segmented.len(), 3);
    assert!(log.timestamps[2] > log.timestamps[0]);
    assert_eq!(*r.states.lock().unwrap(), [LOADING, READY]);
}

#[test]
fn test_the_segmenter_gets_a_small_copy_and_the_mask_comes_back_at_the_frames_size() {
    for (h, w, given) in [(720, 1280, (256, 144)), (1080, 1920, (256, 144)), (1280, 720, (256, 455)), (100, 160, (160, 100))] {
        let r = ready();
        let m = mask(&r.v, "a", 1, &Rgb::filled(w, h, 0)).unwrap();
        assert_eq!(r.backend.log.lock().unwrap().segmented[0], given);
        assert_eq!(m.len(), w * h);
    }
}

#[test]
fn test_while_nothing_moves_the_mask_is_worked_out_less_often() {
    let r = ready();
    let still = Rgb::filled(1280, 720, 80);
    let mut masks = vec![mask(&r.v, "a", 1, &still).unwrap()];
    hold_mask_clock(&r.v);
    masks.extend((2..=3).map(|seq| mask(&r.v, "a", seq, &still).unwrap()));
    assert_eq!(r.backend.log.lock().unwrap().segmented.len(), 1);
    assert!(Arc::ptr_eq(&masks[1], &masks[0]) && Arc::ptr_eq(&masks[2], &masks[0]));
    age_mask_clock(&r.v);
    mask(&r.v, "a", 4, &still);
    assert_eq!(r.backend.log.lock().unwrap().segmented.len(), 2);
    let moved = with_arm(&still, (400, 700), (200, 500), 200);
    mask(&r.v, "a", 5, &moved);
    assert_eq!(r.backend.log.lock().unwrap().segmented.len(), 3);
}

#[test]
fn test_more_of_the_last_mask_carries_over_while_nothing_moves() {
    let r = ready();
    *r.backend.values.lock().unwrap() = vec![200, 0, 0];
    let still = Rgb::filled(1280, 720, 80);
    mask(&r.v, "a", 1, &still);
    sv(&r.v, "a").mask.lock().unwrap().at = monotonic() - (MASK_STILL_EVERY_S + TICK);
    let calm = mask(&r.v, "a", 2, &still).unwrap();
    assert_eq!(calm[0], (200.0 * MASK_CARRY_STILL).round() as u8);
    let moved = with_arm(&still, (400, 700), (200, 500), 200);
    sv(&r.v, "a").mask.lock().unwrap().at = monotonic();
    let quick = mask(&r.v, "a", 3, &moved).unwrap();
    assert_eq!(quick[0], (calm[0] as f64 * MASK_CARRY).round() as u8);
}

#[test]
fn test_camera_noise_alone_doesnt_count_as_moving() {
    let mut seed = 1u64;
    let mut next = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    let base = Rgb::new(1280, 720, (0..1280 * 720 * 3).map(|_| 60 + (next() % 140) as u8).collect());
    let mut motion = Motion::default();
    assert!(motion.moved(&base));
    motion.used();
    let noisy = Rgb::new(1280, 720, base.data.iter().map(|&v| (v as i32 + (next() % 31) as i32 - 15).clamp(0, 255) as u8).collect());
    assert!(!motion.moved(&noisy));
    let moved = with_arm(&base, (600, 720), (300, 420), 255);
    assert!(motion.moved(&moved));
}

#[test]
fn test_a_big_move_is_a_tenth_of_the_picture_changing_and_is_judged_against_the_frame_used() {
    let still = Rgb::filled(1280, 720, 80);
    let mut motion = Motion::default();
    // The first frame has nothing to compare with
    assert!(motion.moved(&still) && motion.big());
    motion.used();
    assert!(!motion.moved(&still) && !motion.big());
    // 5% of the picture: moving, not big
    assert!(motion.moved(&with_arm(&still, (0, 1280), (0, 36), 200)) && !motion.big());
    // 20%: big
    assert!(motion.moved(&with_arm(&still, (0, 1280), (0, 144), 200)) && motion.big());
    // Back to still, and a level change of 12 or less doesn't count however much of the picture it is
    assert!(!motion.moved(&still) && !motion.big());
    assert!(!motion.moved(&Rgb::filled(1280, 720, 80 + crate::ops::MOVED_LEVELS)) && !motion.big());
    // After the big one is used, the same picture is no longer a move
    motion.moved(&with_arm(&still, (0, 1280), (0, 144), 200));
    motion.used();
    assert!(!motion.moved(&with_arm(&still, (0, 1280), (0, 144), 200)) && !motion.big());
    // A frame of another size has nothing to compare with either
    assert!(motion.moved(&Rgb::filled(640, 360, 80)) && motion.big());
}

#[test]
fn test_model_input_looks_like_one_area_shrink() {
    let (w, h) = (1920, 1080);
    let data: Vec<u8> = (0..w * h * 3).map(|i| {
        let (p, c) = (i / 3, i % 3);
        let (x, y) = ((p % w) as f64, (p / w) as f64);
        (128.0 + 60.0 * (x / 37.0 + c as f64).sin() + 60.0 * (y / 23.0).cos()) as u8
    }).collect();
    let frame = Rgb::new(w, h, data);
    let small = crate::ops::model_input(&frame, MODEL_SIZE).unwrap();
    let area = frame.area(256, 144);
    assert_eq!((small.w, small.h), (area.w, area.h));
    let mean = small.data.iter().zip(&area.data).map(|(a, b)| a.abs_diff(*b) as f64).sum::<f64>() / small.data.len() as f64;
    assert!(mean < 1.0, "{mean}");
    assert!(crate::ops::model_input(&Rgb::filled(160, 90, 0), MODEL_SIZE).is_none());
}

#[test]
fn test_before_its_ready_or_outside_a_stream_there_is_no_mask() {
    let (tx, rx) = mpsc::channel::<()>();
    let rx = Mutex::new(rx);
    let backend = Fake::new();
    let (v, _states, _people) = make(Box::new(move || {
        let _ = rx.lock().unwrap().recv();
        Ok(backend.clone() as Arc<dyn Backend>)
    }));
    let frame = Rgb::filled(6, 4, 0);
    assert!(mask(&v, "a", 1, &frame).is_none());
    assert_eq!(v.state(), LOADING);
    tx.send(()).unwrap();
    wait(|| v.state() == READY);
    // outside a frame step there's no stream to ask for: the API needs one, so the shell only asks from a step
    assert!(mask(&v, "a", 1, &frame).is_some());
}

#[test]
fn test_without_a_pack_its_missing_and_tries_again_later() {
    let tries = Arc::new(Mutex::new(0));
    let t = tries.clone();
    let (v, _s, _p) = make(Box::new(move || {
        *t.lock().unwrap() += 1;
        Err(LoadError::Missing("no vision pack in /home/someone/.local/share/telescope/vision/3.6.0".into()))
    }));
    v.load();
    wait(|| v.state() == MISSING);
    assert_eq!(v.diagnostics(), [("Vision".to_string(), "not downloaded".to_string())]);
    v.load();
    assert_eq!(*tries.lock().unwrap(), 1);
    *v.inner.looked_at.lock().unwrap() -= LOOK_AGAIN_S;
    v.load();
    wait(|| *tries.lock().unwrap() == 2);
}

#[test]
fn test_a_loader_finishing_after_shutdown_does_not_notify_the_window() {
    for error in [None, Some(true), Some(false)] {
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (finish_tx, finish_rx) = mpsc::channel::<()>();
        let loads = Arc::new(Mutex::new(0));
        let (l, entered_tx, finish_rx) = (loads.clone(), Mutex::new(entered_tx), Mutex::new(finish_rx));
        let (v, states, _p) = make(Box::new(move || {
            *l.lock().unwrap() += 1;
            entered_tx.lock().unwrap().send(()).unwrap();
            finish_rx.lock().unwrap().recv_timeout(Duration::from_secs(2)).unwrap();
            match error {
                None => Ok(Fake::new() as Arc<dyn Backend>),
                Some(true) => Err(LoadError::Missing("test load failed".into())),
                Some(false) => Err(LoadError::Failed(VisionError::new("OSError", "test load failed"))),
            }
        }));
        v.load();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        v.shutdown();
        finish_tx.send(()).unwrap();
        // the loader's thread holds its own handle on the service until it's done
        wait(|| Arc::strong_count(&v.inner) == 1);
        assert_eq!(*states.lock().unwrap(), [LOADING]);
        v.load();
        assert_eq!(*loads.lock().unwrap(), 1);
    }
}

#[test]
fn test_shutdown_waits_for_a_state_callback_already_in_progress() {
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (finish_tx, finish_rx) = mpsc::channel::<()>();
    let (entered_tx, finish_rx) = (Mutex::new(entered_tx), Mutex::new(finish_rx));
    let v = Vision::with_effort(Arc::new(Effort::free()), Box::new(|_, _| {}), Box::new(move |state| {
        if state == READY {
            entered_tx.lock().unwrap().send(()).unwrap();
            finish_rx.lock().unwrap().recv_timeout(Duration::from_secs(2)).unwrap();
        }
    }), Box::new(|| Ok(Fake::new() as Arc<dyn Backend>)), None);
    v.load();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    let (v2, s2) = (v.clone(), stopped.clone());
    let shutdown = std::thread::spawn(move || {
        v2.shutdown();
        s2.store(true, Ordering::SeqCst);
    });
    std::thread::sleep(Duration::from_millis(50));
    assert!(!stopped.load(Ordering::SeqCst));
    finish_tx.send(()).unwrap();
    shutdown.join().unwrap();
    assert!(stopped.load(Ordering::SeqCst));
}

#[test]
fn test_people_detection_finishing_after_shutdown_does_not_notify_the_window() {
    let r = ready();
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (finish_tx, finish_rx) = mpsc::channel::<()>();
    let (entered_tx, finish_rx) = (Mutex::new(entered_tx), Mutex::new(finish_rx));
    *r.backend.detect.lock().unwrap() = Arc::new(move || {
        entered_tx.lock().unwrap().send(()).unwrap();
        finish_rx.lock().unwrap().recv_timeout(Duration::from_secs(2)).unwrap();
        vec![Person::default()]
    });
    r.v.want_people("test", true);
    feed(&r.v, "a", &Rgb::filled(8, 8, 0));
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    r.v.shutdown();
    finish_tx.send(()).unwrap();
    wait(|| r.backend.closed().contains(&"pose"));
    assert!(r.people.lock().unwrap().is_empty());
    assert_eq!(r.backend.closed(), ["pose"]);
}

#[test]
fn test_a_broken_mediapipe_fails_once_and_names_only_the_error() {
    let tries = Arc::new(Mutex::new(0));
    let t = tries.clone();
    let (v, _s, _p) = make(Box::new(move || {
        *t.lock().unwrap() += 1;
        Err(LoadError::Failed(VisionError::new("OSError", "libmediapipe.so: cannot open /home/someone/x")))
    }));
    v.load();
    wait(|| v.state() == FAILED);
    v.load();
    assert_eq!(*tries.lock().unwrap(), 1);
    assert_eq!(v.diagnostics(), [("Vision".to_string(), "didn't load (OSError)".to_string())]);
}

#[test]
fn test_auto_framing_loads_a_pack_that_arrives_while_it_is_on() {
    let backend = Fake::new();
    let (available, tries) = (Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(0)));
    let (a, t, b) = (available.clone(), tries.clone(), backend.clone());
    let (v, _s, people) = make(Box::new(move || {
        *t.lock().unwrap() += 1;
        if !a.load(Ordering::SeqCst) {
            return Err(LoadError::Missing("pack not downloaded".into()));
        }
        Ok(b.clone() as Arc<dyn Backend>)
    }));
    v.want_people("Auto framing", true);
    wait(|| v.state() == MISSING);
    available.store(true, Ordering::SeqCst);
    let frame = Rgb::filled(60, 40, 0);
    feed(&v, "a", &frame);
    assert_eq!(*tries.lock().unwrap(), 1);
    *v.inner.looked_at.lock().unwrap() -= LOOK_AGAIN_S;
    feed(&v, "a", &frame);
    wait(|| v.state() == READY);
    feed(&v, "a", &frame);
    wait(|| !people.lock().unwrap().is_empty());
    assert_eq!(people.lock().unwrap()[0].0, "a");
    v.shutdown();
}

#[test]
fn test_people_come_from_their_own_thread_only_while_wanted() {
    let r = ready();
    let frame = Rgb::filled(1280, 720, 0);
    feed(&r.v, "a", &frame);
    assert!(r.people.lock().unwrap().is_empty());
    r.v.want_people("Auto framing", true);
    feed(&r.v, "a", &frame);
    wait(|| !r.people.lock().unwrap().is_empty());
    let (key, found) = r.people.lock().unwrap()[0].clone();
    assert_eq!(key, "a");
    let nose = found[0].point("nose", 0.5).unwrap();
    assert!((nose.0 - 0.5).abs() < 1e-12 && (nose.1 - 0.25).abs() < 1e-12);
    assert!(found[0].point("left_wrist", 0.5).is_none());
    r.v.drop_stream("a");
    wait(|| r.backend.closed().contains(&"pose"));
    r.v.want_people("Auto framing", false);
    assert!(!r.v.wants_people());
}

#[test]
fn test_people_are_found_upright_and_put_where_they_are_shown() {
    let r = ready();
    r.v.want_people("Auto framing", true);
    let upright = Upright { turn: Turn { rotation: Some(270), ..Turn::default() }, sent: (2.0, 0.0, 0.0), ..Upright::default() };
    let still = Rgb::filled(1280, 720, 0);
    r.v.feed_raw("a", &mut || Some(crate::ops::thumb(&still)), &mut || Some(still.clone()), upright);
    wait(|| !r.people.lock().unwrap().is_empty());
    // the small copy is 640 x 360, turned a quarter for the model
    assert_eq!(r.backend.log.lock().unwrap().posed, [(360, 640)]);
    let nose = r.people.lock().unwrap()[0].1[0].point("nose", 0.5).unwrap();
    // what view_controller._upright's to_shown makes of the fake's nose at (0.5, 0.25), worked out in Python
    assert!((nose.0 - 0.5).abs() < 1e-12 && (nose.1 - 0.375).abs() < 1e-12, "{nose:?}");
}

#[test]
fn test_frames_for_people_are_spaced_out_and_made_small() {
    let r = ready();
    r.v.want_people("x", true);
    r.v.inner.hold_people.store(true, Ordering::SeqCst);
    let frame = Rgb::filled(1920, 1080, 0);
    let mut sizes = Vec::new();
    for _ in 0..5 {
        feed(&r.v, "a", &frame);
        if let Some((small, _)) = sv(&r.v, "a").people.lock().unwrap().frame.take() {
            sizes.push((small.w, small.h));
        }
        // from now, so a slow machine's time between frames doesn't count
        let s = sv(&r.v, "a");
        let mut p = s.people.lock().unwrap();
        (p.at, p.probed) = (monotonic(), 0.0);
    }
    assert_eq!(sizes, [(640, 360)]);
}

// like the mask's, the people clock is pushed ahead so a slow machine's time between frames isn't a cadence
fn hold_people_clock(v: &Vision) {
    let s = sv(v, "a");
    let mut p = s.people.lock().unwrap();
    (p.at, p.probed) = (p.at + HELD, p.probed + HELD);
}

#[test]
fn test_a_still_scene_is_looked_at_by_a_grey_copy_once_a_cadence_and_made_into_colour_only_when_sent() {
    let r = ready();
    r.v.want_people("x", true);
    r.v.inner.hold_people.store(true, Ordering::SeqCst);
    let still = Rgb::filled(1280, 720, 80);
    let (looks, pictures) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
    let feed_counted = || r.v.feed_raw("a", &mut || { looks.set(looks.get() + 1); Some(crate::ops::thumb(&still)) }, &mut || { pictures.set(pictures.get() + 1); Some(still.clone()) }, Upright::default());
    // debug builds on a busy machine can take longer than a cadence over these frames, so the stream's clock is held still between them
    let hold = || hold_people_clock(&r.v);
    // the first frame has nothing to be compared with, so it goes
    feed_counted();
    hold();
    assert_eq!((looks.get(), pictures.get()), (1, 1));
    // frames of a still scene straight after it are not even looked at
    for _ in 0..20 {
        feed_counted();
    }
    assert_eq!((looks.get(), pictures.get()), (1, 1));
    // a cadence later one is looked at, and a still one isn't made into colour
    let rewind = |s: f64| {
        let sv = sv(&r.v, "a");
        let mut p = sv.people.lock().unwrap();
        (p.at, p.probed) = (monotonic() - s, monotonic() - s);
    };
    rewind(PEOPLE_EVERY_S + TICK);
    feed_counted();
    assert_eq!((looks.get(), pictures.get()), (2, 1));
    hold();
    // and then not again until the next
    feed_counted();
    assert_eq!((looks.get(), pictures.get()), (2, 1));
    // until the still scene is due a refresh
    rewind(PEOPLE_STILL_EVERY_S);
    feed_counted();
    assert_eq!((looks.get(), pictures.get()), (3, 2));
}

#[test]
fn test_people_due_follows_the_cadence_so_a_caller_can_skip_making_the_frame() {
    let r = ready();
    assert!(!r.v.people_due("a"), "nobody wants people");
    r.v.want_people("x", true);
    r.v.inner.hold_people.store(true, Ordering::SeqCst);
    assert!(r.v.people_due("a"));
    feed(&r.v, "a", &Rgb::filled(1280, 720, 0));
    hold_people_clock(&r.v);
    assert!(!r.v.people_due("a"), "just fed");
    let sv = sv(&r.v, "a");
    let mut p = sv.people.lock().unwrap();
    (p.at, p.probed) = (monotonic() - PEOPLE_EVERY_S - TICK, monotonic() - PEOPLE_EVERY_S - TICK);
    drop(p);
    assert!(r.v.people_due("a"));
}

fn sent_each(r: &Ready, frames: &[(&Rgb, f64)]) -> Vec<bool> {
    frames.iter().map(|(frame, ago)| {
        feed(&r.v, "a", frame);
        let s = sv(&r.v, "a");
        let mut p = s.people.lock().unwrap();
        let sent = p.frame.take().is_some();
        p.probed = 0.0;
        p.at = monotonic() - (ago + TICK); // from now, so a slow machine's time between frames doesn't count
        sent
    }).collect()
}

#[test]
fn test_people_are_looked_for_less_often_while_nothing_moves() {
    let r = ready();
    r.v.want_people("x", true);
    r.v.inner.hold_people.store(true, Ordering::SeqCst);
    let still = Rgb::filled(1280, 720, 80);
    let moved = with_arm(&still, (400, 700), (200, 500), 200);
    let sent = sent_each(&r, &[(&still, PEOPLE_EVERY_S), (&still, PEOPLE_STILL_EVERY_S), (&still, PEOPLE_EVERY_S), (&moved, 0.0)]);
    assert_eq!(sent, [true, false, true, true]);
}

// The mask's last run was this long ago on the service's clock (tests using it fake the clock, so real time between calls doesn't count)
fn mask_ran_ago(v: &Vision, secs: f64) {
    sv(v, "a").mask.lock().unwrap().at = v.now() - secs;
}

fn segmented(r: &Ready) -> usize {
    r.backend.log.lock().unwrap().segmented.len()
}

#[test]
fn test_power_saving_runs_the_mask_soon_on_a_big_move() {
    let r = ready();
    r.v.set_power_saving(true);
    r.v.fake_clock();
    let still = Rgb::filled(1280, 720, 80);
    mask(&r.v, "a", 0, &still);
    hold_mask_clock(&r.v);
    // An arm moving a little waits for the slower beat, standing up (most of the picture changing) doesn't
    mask(&r.v, "a", 1, &with_arm(&still, (400, 600), (200, 400), 200));
    assert_eq!(segmented(&r), 1);
    let standing = with_arm(&still, (200, 1100), (100, 720), 200);
    // not within the shortest gap of the last run, so 60 fps or a flickering picture doesn't make a mask every frame
    mask_ran_ago(&r.v, cadence(1).big_move.unwrap() / 2.0);
    mask(&r.v, "a", 2, &standing);
    assert_eq!(segmented(&r), 1);
    mask_ran_ago(&r.v, cadence(1).big_move.unwrap() * 1.5);
    mask(&r.v, "a", 3, &standing);
    assert_eq!(segmented(&r), 2);
}

#[test]
fn test_a_big_move_brings_the_mask_sooner_only_with_power_saving() {
    for saving in [false, true] {
        let r = ready();
        r.v.set_power_saving(saving);
        r.v.fake_clock();
        let still = Rgb::filled(160, 90, 80);
        mask(&r.v, "a", 0, &still);
        // A small move 0.04 s after the last run: only without Power-saving
        mask_ran_ago(&r.v, 0.04);
        mask(&r.v, "a", 1, &with_arm(&still, (0, 21), (0, 21), 200));
        assert_eq!(segmented(&r), if saving { 1 } else { 2 }, "saving {saving}");
        // A big one: both have a mask for it
        mask_ran_ago(&r.v, 0.04);
        mask(&r.v, "a", 2, &with_arm(&still, (0, 160), (0, 45), 200));
        assert_eq!(segmented(&r), if saving { 2 } else { 3 }, "saving {saving}");
    }
}

#[test]
fn test_power_saving_masks_come_ten_a_second_at_any_frame_rate_whatever_the_jitter() {
    // Arms in three places, 3% of the picture each: neither the move between two nor standing still makes a big one
    let still = Rgb::filled(160, 90, 80);
    let arms: Vec<Rgb> = (0..3).map(|i| with_arm(&still, (40 * i, 40 * i + 21), (0, 21), 200)).collect();
    for (fps, per_second) in [(15.0, 7.5), (24.0, 8.0), (30.0, 10.0), (60.0, 10.0)] {
        let r = ready();
        r.v.set_power_saving(true);
        r.v.fake_clock();
        let frames = (fps * 6.0) as usize;
        for seq in 0..frames {
            // The frame arrives a few ms off its beat, one early and one late
            let jitter = if seq % 2 == 0 { -0.003 } else { 0.003 };
            if seq > 0 {
                r.v.advance_fake_clock(1.0 / fps + jitter);
            }
            mask(&r.v, "a", seq as u64, &arms[seq % 3]);
        }
        let runs = segmented(&r) as f64;
        assert!((runs - per_second * 6.0).abs() <= 2.0, "{fps} fps: {runs} runs in 6 s");
    }
}

#[test]
fn test_power_saving_masks_on_a_run_of_big_moves_stay_at_or_under_thirty_a_second() {
    let still = Rgb::filled(160, 90, 80);
    // Each of the three is a big move from either of the others
    let busy = [with_arm(&still, (0, 160), (0, 45), 200), with_arm(&still, (0, 160), (45, 90), 200), with_arm(&still, (0, 80), (0, 90), 200)];
    for (fps, saving, expected) in [(30.0, true, 60), (60.0, true, 60), (60.0, false, 120)] {
        let r = ready();
        r.v.set_power_saving(saving);
        r.v.fake_clock();
        for seq in 0..(fps * 2.0) as usize {
            if seq > 0 {
                r.v.advance_fake_clock(1.0 / fps);
            }
            mask(&r.v, "a", seq as u64, &busy[seq % 3]);
        }
        assert_eq!(segmented(&r), expected, "{fps} fps, saving {saving}");
    }
}

#[test]
fn test_power_saving_runs_the_models_less_often_even_while_something_moves() {
    let r = ready();
    r.v.set_power_saving(true);
    // small moves: a big one runs the mask at once (see above)
    let moving: Vec<Rgb> = (0..3).map(|i| with_arm(&Rgb::filled(1280, 720, 80), (400, 700), (40 * i, 40 * i + 300), 200)).collect();
    for (seq, frame) in moving.iter().enumerate() {
        mask(&r.v, "a", seq as u64, frame);
        if seq == 0 {
            hold_mask_clock(&r.v);
        }
    }
    assert_eq!(r.backend.log.lock().unwrap().segmented.len(), 1);
    age_mask_clock(&r.v);
    mask(&r.v, "a", 9, &moving[0]);
    assert_eq!(r.backend.log.lock().unwrap().segmented.len(), 2);
    assert!(r.v.diagnostics()[0].1.contains("power saving"));
    r.v.want_people("x", true);
    r.v.inner.hold_people.store(true, Ordering::SeqCst);
    let sent = sent_each(&r, &[(&moving[0], PEOPLE_EVERY_S), (&moving[1], cadence(1).people.moving), (&moving[2], 0.0)]);
    assert_eq!(sent, [true, false, true]);
}

/// What each effort level asks of the models: how often the mask runs while something moves, and how often people are looked for.
#[test]
fn test_each_effort_level_runs_the_mask_less_often_while_something_moves() {
    for level in 0..=MOST {
        let r = ready();
        r.v.inner.effort.set_floor(level);
        let moving: Vec<Rgb> = (0..4).map(|i| with_arm(&Rgb::filled(1280, 720, 80), (400, 700), (100 * i, 100 * i + 300), 200)).collect();
        let runs = || r.backend.log.lock().unwrap().segmented.len();
        let set_age = |ago: f64| sv(&r.v, "a").mask.lock().unwrap().at = monotonic() - ago;
        mask(&r.v, "a", 0, &moving[0]);
        let gap = cadence(level).mask.moving;
        if level == FULL {
            // Every frame that moves gets a mask
            mask(&r.v, "a", 1, &moving[1]);
            mask(&r.v, "a", 2, &moving[2]);
            assert_eq!(runs(), 3);
            continue;
        }
        set_age(gap / 2.0);
        mask(&r.v, "a", 1, &moving[1]);
        assert_eq!(runs(), 1, "level {level}: too soon");
        set_age(gap + TICK);
        mask(&r.v, "a", 2, &moving[2]);
        assert_eq!(runs(), 2, "level {level}: due");
    }
}

#[test]
fn test_each_effort_level_runs_the_mask_for_a_still_picture_at_its_own_pace() {
    for level in 0..=MOST {
        let r = ready();
        r.v.inner.effort.set_floor(level);
        let still = Rgb::filled(1280, 720, 80);
        let runs = || r.backend.log.lock().unwrap().segmented.len();
        let set_age = |ago: f64| sv(&r.v, "a").mask.lock().unwrap().at = monotonic() - ago;
        mask(&r.v, "a", 0, &still);
        let gap = cadence(level).mask.still;
        set_age(gap / 2.0);
        mask(&r.v, "a", 1, &still);
        assert_eq!(runs(), 1, "level {level}: too soon");
        set_age(gap + TICK);
        mask(&r.v, "a", 2, &still);
        assert_eq!(runs(), 2, "level {level}: due");
    }
}

#[test]
fn test_each_effort_level_looks_for_people_at_its_own_pace() {
    for level in 0..=MOST {
        let r = ready();
        r.v.inner.effort.set_floor(level);
        r.v.want_people("x", true);
        r.v.inner.hold_people.store(true, Ordering::SeqCst);
        let people = cadence(level).people;
        let below = if level == FULL { 0.0 } else { cadence(level - 1).people.moving };
        let moving: Vec<Rgb> = (0..3).map(|i| with_arm(&Rgb::filled(1280, 720, 80), (400, 700), (100 * i, 100 * i + 300), 200)).collect();
        let sent = sent_each(&r, &[(&moving[0], below), (&moving[1], people.moving), (&moving[2], 0.0)]);
        assert_eq!(sent, [true, false, true], "level {level} while something moves");
        let r = ready();
        r.v.inner.effort.set_floor(level);
        r.v.want_people("x", true);
        r.v.inner.hold_people.store(true, Ordering::SeqCst);
        let (still, moved) = (Rgb::filled(1280, 720, 80), with_arm(&Rgb::filled(1280, 720, 80), (400, 700), (200, 500), 200));
        let sent = sent_each(&r, &[(&still, people.moving), (&still, people.still), (&still, people.moving), (&moved, 0.0)]);
        assert_eq!(sent, [true, false, true, true], "level {level} while nothing does");
    }
}

#[test]
fn test_the_levels_only_ask_for_less_as_they_rise() {
    for level in 1..=MOST {
        let (lighter, heavier) = (cadence(level), cadence(level - 1));
        assert!(lighter.mask.moving >= heavier.mask.moving && lighter.mask.still >= heavier.mask.still, "level {level} mask");
        assert!(lighter.big_move.unwrap_or(f64::INFINITY) >= heavier.big_move.unwrap_or(f64::INFINITY), "level {level} big move");
        assert!(lighter.people.moving > heavier.people.moving && lighter.people.still > heavier.people.still, "level {level} people");
        assert!(lighter.cutout.is_none() || heavier.cutout.is_some_and(|h| lighter.cutout.unwrap().still >= h.still), "level {level} cutout");
    }
    assert_eq!(cadence(MOST + 9), cadence(MOST));
    // Level 1 is exactly power-saving effects as it was
    let saving = cadence(1);
    assert_eq!((saving.mask.moving, saving.mask.still, saving.big_move), (0.1 - SAVING_MASK_JITTER_S, 0.1 - SAVING_MASK_JITTER_S, Some(0.025)));
    assert_eq!((saving.cutout.map(|c| (c.moving, c.still)), saving.people.moving, saving.people.still), (Some((CUTOUT_STILL_EVERY_S, CUTOUT_STILL_EVERY_S)), 0.16, 0.5));
}

#[test]
fn test_power_saving_is_the_floor_of_the_effort_level() {
    let r = ready();
    assert!(!r.v.power_saving());
    assert_eq!(r.v.inner.effort.level(), FULL);
    r.v.set_power_saving(true);
    assert!(r.v.power_saving());
    assert_eq!(r.v.inner.effort.level(), 1);
    r.v.set_power_saving(false);
    assert_eq!((r.v.power_saving(), r.v.inner.effort.level()), (false, FULL));
    // A floor is power saving, not a step down
    r.v.inner.effort.set_floor(2);
    assert!(r.v.diagnostics()[0].1.contains("power saving"));
    assert!(!r.v.diagnostics()[0].1.contains("stepped down"), "a floor isn't a step down");
}

fn released_counter(v: &Vision) -> Arc<Mutex<u32>> {
    let released = Arc::new(Mutex::new(0));
    let r = released.clone();
    *v.inner.give_back.lock().unwrap() = Box::new(move || *r.lock().unwrap() += 1);
    released
}

fn start_tidier(v: &Vision) -> (std::thread::JoinHandle<()>, mpsc::Receiver<()>) {
    let (quit, stopped) = mpsc::channel();
    *v.inner.tidier.lock().unwrap() = Some(quit);
    let weak = Arc::downgrade(&v.inner);
    let (done, finished) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        tidy_loop(weak, stopped);
        done.send(()).unwrap();
    });
    (thread, finished)
}

#[test]
fn test_shutdown_interrupts_the_tidy_wait() {
    for waiting in [false, true] {
        let (v, _, _) = make(Box::new(|| Err(LoadError::Missing(String::new()))));
        let (thread, finished) = start_tidier(&v);
        if waiting {
            assert_eq!(finished.recv_timeout(Duration::from_millis(20)), Err(mpsc::RecvTimeoutError::Timeout));
        }
        v.shutdown();
        finished.recv_timeout(Duration::from_secs(1)).expect("shutdown must not wait for the two-second deadline");
        thread.join().unwrap();
    }
}

#[test]
fn test_dropping_the_service_interrupts_the_tidy_wait() {
    let (v, _, _) = make(Box::new(|| Err(LoadError::Missing(String::new()))));
    let (thread, finished) = start_tidier(&v);
    assert_eq!(finished.recv_timeout(Duration::from_millis(20)), Err(mpsc::RecvTimeoutError::Timeout));
    drop(v);
    finished.recv_timeout(Duration::from_secs(1)).expect("the tidier must not keep the service alive");
    thread.join().unwrap();
}

#[test]
fn test_the_tidy_deadline_still_closes_unused_models() {
    let (v, _, _) = make(Box::new(|| Err(LoadError::Missing(String::new()))));
    let backend = Fake::new();
    let stream = Arc::new(StreamVision::new());
    {
        let mut mask = stream.mask.lock().unwrap();
        mask.segment = Some(backend.segmenter().unwrap());
        mask.asked = monotonic() - IDLE_CLOSE_S;
    }
    v.inner.streams.lock().unwrap().insert("a".into(), stream);
    let (released, freed) = mpsc::channel();
    *v.inner.give_back.lock().unwrap() = Box::new(move || released.send(()).unwrap());
    let (thread, finished) = start_tidier(&v);
    assert_eq!(freed.recv_timeout(Duration::from_millis(20)), Err(mpsc::RecvTimeoutError::Timeout));
    freed.recv_timeout(Duration::from_secs_f64(TIDY_EVERY_S + 1.0)).expect("unused models close at the tidy deadline");
    assert_eq!(backend.closed(), ["segmenter"]);
    v.shutdown();
    finished.recv_timeout(Duration::from_secs(1)).unwrap();
    thread.join().unwrap();
}

#[test]
fn test_a_mask_nobody_asks_for_closes_after_a_while_and_comes_back_when_asked() {
    let r = ready();
    let released = released_counter(&r.v);
    let frame = Rgb::filled(1280, 720, 0);
    mask(&r.v, "a", 1, &frame);
    let asked = sv(&r.v, "a").mask.lock().unwrap().asked;
    r.v.tidy(Some(asked + IDLE_CLOSE_S - 1.0));
    assert!(r.backend.closed().is_empty() && *released.lock().unwrap() == 0);
    r.v.tidy(Some(asked + IDLE_CLOSE_S + TICK));
    assert_eq!(r.backend.closed(), ["segmenter"]);
    assert_eq!(*released.lock().unwrap(), 1);
    assert!(mask(&r.v, "a", 2, &frame).is_some());
    assert_eq!(r.backend.log.lock().unwrap().made, ["segmenter", "segmenter"]);
    r.v.shutdown();
}

#[test]
fn test_flipping_auto_framing_on_and_off_keeps_the_pose_model_loaded() {
    let r = ready();
    let frame = Rgb::filled(1280, 720, 0);
    for seq in 0..20 {
        r.v.want_people("Auto framing", seq % 2 == 0);
        feed(&r.v, "a", &frame);
        if let Some(s) = r.v.inner.streams.lock().unwrap().get("a") {
            s.people.lock().unwrap().at -= PEOPLE_EVERY_S + TICK;
        }
        r.v.tidy(None);
    }
    wait(|| !r.people.lock().unwrap().is_empty());
    assert_eq!(r.backend.log.lock().unwrap().made, ["pose"]);
    assert!(!r.backend.closed().contains(&"pose"));
}

#[test]
fn test_a_pose_model_nobody_feeds_closes_in_its_own_thread_and_comes_back() {
    let r = ready();
    let released = released_counter(&r.v);
    r.v.want_people("Auto framing", true);
    let frame = Rgb::filled(1280, 720, 0);
    feed(&r.v, "a", &frame);
    wait(|| !r.people.lock().unwrap().is_empty());
    r.v.want_people("Auto framing", false);
    let fed = sv(&r.v, "a").people.lock().unwrap().fed;
    r.v.tidy(Some(fed + IDLE_CLOSE_S + TICK));
    wait(|| r.backend.closed().contains(&"pose") && *released.lock().unwrap() > 0);
    assert!(sv(&r.v, "a").people.lock().unwrap().thread.is_none());
    r.v.want_people("Auto framing", true);
    r.people.lock().unwrap().clear();
    feed(&r.v, "a", &frame);
    wait(|| !r.people.lock().unwrap().is_empty());
    assert_eq!(r.backend.log.lock().unwrap().made, ["pose", "pose"]);
    r.v.shutdown();
}

#[test]
fn test_a_segmenter_that_cant_load_fails_once_and_names_the_missing_library() {
    let r = ready();
    *r.backend.broken_segmenter.lock().unwrap() = Some(VisionError::new("OSError", "libGLESv2.so.2: cannot open shared object file: No such file or directory"));
    let frame = Rgb::filled(6, 4, 0);
    for seq in 0..3 {
        assert!(mask(&r.v, "a", seq, &frame).is_none());
    }
    assert_eq!(r.backend.log.lock().unwrap().made, ["segmenter"]);
    assert_eq!(r.v.state(), FAILED);
    assert_eq!(r.states.lock().unwrap().last().map(String::as_str), Some(FAILED));
    assert!(r.v.diagnostics()[0].1.contains("OSError: libGLESv2.so.2 missing"));
}

#[test]
fn test_a_failure_message_with_a_path_in_it_stays_out_of_diagnostics() {
    let (v, _s, _p) = make(Box::new(|| Err(LoadError::Failed(VisionError::new("OSError", "/home/someone/vision/site: broken")))));
    v.load();
    wait(|| v.state() == FAILED);
    let d = format!("{:?}", v.diagnostics());
    assert!(!d.contains("someone") && d.contains("OSError"));
}

#[test]
fn test_dropping_a_stream_closes_its_segmenter() {
    let r = ready();
    mask(&r.v, "a", 1, &Rgb::filled(6, 4, 0));
    r.v.drop_stream("a");
    assert_eq!(r.backend.closed(), ["segmenter"]);
    r.v.drop_stream("a");
}

#[test]
fn a_downloaded_pack_makes_a_missing_vision_look_again() {
    let available = Arc::new(AtomicBool::new(false));
    let a = available.clone();
    let (v, states, _p) = make(Box::new(move || {
        if a.load(Ordering::SeqCst) { Ok(Fake::new() as Arc<dyn Backend>) } else { Err(LoadError::Missing("no".into())) }
    }));
    v.want_people("Auto framing", true);
    wait(|| v.state() == MISSING);
    available.store(true, Ordering::SeqCst);
    v.pack_downloaded();
    wait(|| v.state() == READY);
    assert_eq!(*states.lock().unwrap(), [LOADING, MISSING, OFF, LOADING, READY]);
}

/// A segmenter that takes a while and says where the bright block in the frame is, like a model looking at a person.
struct Finder {
    took: Duration,
    looked: Arc<Mutex<Vec<Instant>>>,
}

impl Segmenter for Finder {
    fn segment(&mut self, frame: &Rgb, _ts: i64) -> Result<Gray, VisionError> {
        self.looked.lock().unwrap().push(Instant::now());
        std::thread::sleep(self.took);
        Ok(Gray { w: frame.w, h: frame.h, data: frame.data.as_chunks::<3>().0.iter().map(|p| if p[0] > 128 { 255 } else { 0 }).collect() })
    }
}

struct Finders {
    took: Duration,
    looked: Arc<Mutex<Vec<Instant>>>,
}

impl Backend for Finders {
    fn version(&self) -> String {
        "finder".into()
    }
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        Ok(Box::new(Finder { took: self.took, looked: self.looked.clone() }))
    }
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        Err(VisionError::new("Nope", "no pose here"))
    }
}

fn finder(took: Duration) -> (Vision, Arc<Mutex<Vec<Instant>>>) {
    let looked: Arc<Mutex<Vec<Instant>>> = Default::default();
    let backend: Arc<dyn Backend> = Arc::new(Finders { took, looked: looked.clone() });
    let (v, _, _) = make(Box::new(move || Ok(backend.clone())));
    v.load();
    wait(|| v.state() == READY);
    (v, looked)
}

/// A dark picture with a bright block, a stand-in for a person, with its left edge at x.
fn someone_at(x: usize) -> Rgb {
    let mut frame = Rgb::filled(256, 144, 0);
    for row in 40..120 {
        frame.data[(row * 256 + x) * 3..(row * 256 + x + 60) * 3].fill(255);
    }
    frame
}

#[test]
fn a_frame_step_gets_the_newest_mask_without_waiting_for_the_model() {
    let (v, looked) = finder(Duration::from_millis(30));
    let frame = someone_at(50);
    let ask = |seq: u64| {
        let f = frame.clone();
        let began = Instant::now();
        let mask = v.newest_mask("a", seq, 256, 144, Box::new(move || Some(f)));
        (mask, began.elapsed())
    };
    assert!(ask(1).0.is_some(), "a stream's first frame waits for its mask");
    let (mask, took) = ask(2);
    assert!(mask.is_some());
    assert!(took < Duration::from_millis(10), "{took:?}");
    // Frames faster than the model: it skips some instead of falling further and further behind
    (3..30).for_each(|seq| {
        ask(seq);
        std::thread::sleep(Duration::from_millis(5));
    });
    assert!(looked.lock().unwrap().len() < 15, "{}", looked.lock().unwrap().len());
    v.drop_stream("a");
    wait(|| !v.stream("a").newest.lock().unwrap().thread);
}

#[test]
fn a_moving_person_is_never_more_than_a_frame_or_so_behind() {
    let (v, _) = finder(Duration::from_millis(4));
    const STEP: usize = 3;
    let mut worst = 0;
    for seq in 0..50usize {
        let at = 30 + seq * STEP;
        let frame = someone_at(at);
        let mask = v.newest_mask("a", seq as u64, 256, 144, Box::new(move || Some(frame)));
        if let Some(mask) = mask.filter(|_| seq > 5) {
            // Where the mask's block starts on a row through it, with the carried part of the last mask counted out
            let row = &mask[80 * 256..81 * 256];
            let edge = row.iter().position(|v| *v > 128).unwrap();
            worst = worst.max(at.abs_diff(edge));
        }
        // The model has this frame by the time the next one comes, however busy the machine is: how far behind the mask is must not depend on the scheduler
        wait(|| v.stream("a").newest.lock().unwrap().done > seq as u64);
        std::thread::sleep(Duration::from_millis(33));
    }
    assert!(worst > 0 && worst <= 2 * STEP, "{worst} pixels behind, moving {STEP} a frame");
}

#[test]
fn a_step_asking_at_another_size_gets_the_mask_it_can_at_once() {
    let (v, _) = finder(Duration::from_millis(1));
    let frame = someone_at(50);
    let ask = |seq, w, h| {
        let f = frame.clone();
        v.newest_mask("a", seq, w, h, Box::new(move || Some(f)))
    };
    ask(1, 256, 144);
    assert_eq!(ask(2, 128, 72).unwrap().len(), 128 * 72);
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture --test-threads 1"]
fn bench_second_size_of_a_mask() {
    let (v, _) = finder(Duration::from_millis(1));
    let frame = someone_at(50);
    let ask = |w, h| {
        let f = frame.clone();
        v.newest_mask("a", 7, w, h, Box::new(move || Some(f))).unwrap()
    };
    ask(256, 144);
    wait(|| v.stream("a").newest.lock().unwrap().done >= 1);
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        ask(w, h);
        let kept = (0..50).map(|_| { let t = Instant::now(); ask(w, h); t.elapsed().as_secs_f64() * 1000.0 }).fold(f64::MAX, f64::min);
        let base = vec![128u8; 256 * 144];
        let resize = (0..20).map(|_| { let t = Instant::now(); crate::ops::mask_resized(&base, 256, 144, w, h); t.elapsed().as_secs_f64() * 1000.0 }).fold(f64::MAX, f64::min);
        println!("mask at {w}x{h}: each frame before (a resize) {resize:.3} ms, now (a kept one) {kept:.4} ms");
    }
}

#[test]
fn test_two_steps_at_different_sizes_get_stable_masks_and_one_inference() {
    let (v, looked) = finder(Duration::from_millis(1));
    let frame = someone_at(50);
    let ask = |w, h| {
        let f = frame.clone();
        v.newest_mask("a", 7, w, h, Box::new(move || Some(f))).unwrap()
    };
    ask(256, 144);
    ask(64, 36);
    wait(|| v.stream("a").newest.lock().unwrap().done >= 1);
    let (full, quarter) = (ask(256, 144), ask(64, 36));
    let inferences = looked.lock().unwrap().len();
    for _ in 0..5 {
        assert!(Arc::ptr_eq(&ask(256, 144), &full));
        assert!(Arc::ptr_eq(&ask(64, 36), &quarter));
    }
    assert_eq!((full.len(), quarter.len()), (256 * 144, 64 * 36));
    assert_eq!(looked.lock().unwrap().len(), inferences);
    assert_eq!(inferences, 1);
}

/// Finder until it's told to fail.
struct Breaks {
    finder: Finder,
    broken: Arc<AtomicBool>,
}

impl Segmenter for Breaks {
    fn segment(&mut self, frame: &Rgb, ts_ms: i64) -> Result<Gray, VisionError> {
        if self.broken.load(Ordering::SeqCst) {
            return Err(VisionError::new("RuntimeError", "broken"));
        }
        self.finder.segment(frame, ts_ms)
    }
}

struct Breakers(Arc<AtomicBool>);

impl Backend for Breakers {
    fn version(&self) -> String {
        "breaks".into()
    }
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        Ok(Box::new(Breaks { finder: Finder { took: Duration::from_millis(1), looked: Default::default() }, broken: self.0.clone() }))
    }
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        Err(VisionError::new("Nope", "no pose here"))
    }
}

fn age_newest(v: &Vision, by: f64) {
    let sv = v.stream("a");
    let mut newest = sv.newest.lock().unwrap();
    newest.mask.as_mut().unwrap().at -= by;
    if let Some(mask) = newest.background.as_mut() {
        mask.at -= by;
    }
}

#[test]
fn a_mask_stops_being_handed_out_once_its_too_old() {
    let broken: Arc<AtomicBool> = Default::default();
    let backend: Arc<dyn Backend> = Arc::new(Breakers(broken.clone()));
    let (v, _, _) = make(Box::new(move || Ok(backend.clone())));
    v.load();
    wait(|| v.state() == READY);
    // A moving picture: with nothing moving the model isn't asked every time
    let ask = |seq: u64| {
        let f = someone_at(10 + seq as usize * 30);
        v.newest_mask("a", seq, 256, 144, Box::new(move || Some(f)))
    };
    assert!(ask(1).is_some());
    broken.store(true, Ordering::SeqCst);
    assert!(ask(2).is_some(), "one failed frame doesn't show");
    age_newest(&v, MASK_MAX_AGE_S * 0.9);
    assert!(ask(3).is_some(), "not yet");
    age_newest(&v, MASK_MAX_AGE_S * 0.2);
    assert!(ask(4).is_none(), "too old");
    broken.store(false, Ordering::SeqCst);
    assert!(ask(5).is_some(), "the model's back");
}

#[test]
fn a_mask_isnt_stretched_onto_a_picture_of_another_shape() {
    let (v, _) = finder(Duration::from_millis(1));
    let frame = someone_at(50);
    let f = frame.clone();
    assert!(v.newest_mask("a", 1, 256, 144, Box::new(move || Some(f))).is_some());
    assert!(v.newest_mask("a", 2, 144, 256, Box::new(|| None)).is_none());
}

#[test]
fn a_turn_of_the_view_drops_the_masks_and_what_would_be_blended_into_the_next() {
    let (v, _) = finder(Duration::from_millis(1));
    let ask = |seq: u64, x: usize| {
        let f = someone_at(x);
        v.newest_mask("a", seq, 256, 144, Box::new(move || Some(f)))
    };
    let first = ask(1, 20).unwrap();
    assert!(first[80 * 256 + 30] > 128, "the person is where the picture says");
    v.drop_masks();
    assert!(v.stream("a").newest.lock().unwrap().mask.is_none(), "nothing kept to hand out");
    // The same frame again, flipped: no trace of the old silhouette at the old place
    let flipped = ask(2, 256 - 20 - 60).unwrap();
    assert_eq!(flipped[80 * 256 + 30], 0, "the old person isn't blended into the new mask");
    assert!(flipped[80 * 256 + 200] > 128);
}

#[test]
fn a_frame_made_before_a_turn_of_the_view_gets_no_mask_after_it_and_leaves_no_trace() {
    let (v, _) = finder(Duration::from_millis(1));
    let ask = |seq: u64, x: usize, epoch: u64| {
        let f = someone_at(x);
        v.newest_mask_in("a", seq, 256, 144, Box::new(move || Some(f)), epoch)
    };
    let before = v.view_epoch();
    assert!(ask(1, 20, before).is_some());
    v.drop_masks();
    assert_ne!(v.view_epoch(), before);
    // The frame in the pipeline when the view turned only now asks: its picture is of the old way round, so nothing for it
    assert!(ask(2, 20, before).is_none(), "an old picture is never given a mask of the new view");
    assert!(v.stream("a").newest.lock().unwrap().mask.is_none(), "and none was kept for what comes next");
    // What it would blend into the next mask isn't there either
    let flipped = ask(3, 256 - 20 - 60, v.view_epoch()).unwrap();
    assert_eq!(flipped[80 * 256 + 30], 0, "no trace of the old silhouette");
    assert!(flipped[80 * 256 + 200] > 128);
}

#[test]
fn a_mask_of_the_old_view_is_not_handed_to_a_frame_of_the_new_one_even_before_it_is_cleared() {
    let (v, _) = finder(Duration::from_millis(1));
    let f = someone_at(20);
    let first = v.newest_mask("a", 1, 256, 144, Box::new(move || Some(f)));
    assert!(first.is_some());
    // The turn counted but the kept mask not yet dropped, as when the turn lands between the two
    v.inner.turns.fetch_add(1, Ordering::SeqCst);
    let epoch = v.view_epoch();
    assert!(v.stream("a").newest.lock().unwrap().mask.is_some());
    let f = someone_at(180);
    let second = v.newest_mask_in("a", 2, 256, 144, Box::new(move || Some(f)), epoch).unwrap();
    assert_eq!(second[80 * 256 + 30], 0, "not the old person's");
    assert!(second[80 * 256 + 200] > 128, "the new picture's own");
}

#[test]
fn a_stuck_model_holds_up_one_frame_and_not_every_frame() {
    let (v, _) = finder(Duration::from_secs(1));
    assert!(v.newest_mask("a", 1, 256, 144, Box::new(|| Some(someone_at(50)))).is_none());
    let began = Instant::now();
    assert!(v.newest_mask("a", 2, 256, 144, Box::new(|| Some(someone_at(50)))).is_none());
    assert!(began.elapsed() < Duration::from_millis(100), "{:?}", began.elapsed());
}

#[test]
fn waiting_for_masks_gives_each_frame_its_own() {
    let (v, _) = finder(Duration::from_millis(1));
    v.wait_for_masks();
    let frame = someone_at(50);
    assert!(v.newest_mask("a", 1, 256, 144, Box::new(move || Some(frame))).is_some());
}

#[test]
fn a_first_mask_that_doesnt_come_doesnt_hold_the_stream_up_for_long() {
    let (v, _) = finder(Duration::from_millis(1));
    let began = Instant::now();
    assert!(v.newest_mask("a", 1, 256, 144, Box::new(|| None)).is_none());
    assert!(began.elapsed() < Duration::from_millis(100), "{:?}", began.elapsed());
}

#[test]
fn background_keeps_a_late_mask_while_privacy_still_rejects_it() {
    let (v, _) = finder(Duration::from_millis(1));
    let geometry = Some(Geometry::default());
    let comfort = MaskView { geometry, keep_late: true, ..Default::default() };
    let first = v.newest_normal_for("a", true, 1, 256, 144, Box::new(|| Some(someone_at(50))), v.view_epoch(), comfort).unwrap();
    age_newest(&v, MASK_MAX_AGE_S * 2.0);
    let held = v.newest_normal_for("a", false, 2, 256, 144, Box::new(|| None), v.view_epoch(), comfort).unwrap();
    assert!(Arc::ptr_eq(&first, &held), "Background keeps the last good cutout without holding the picture");
    let privacy = MaskView { keep_late: false, ..comfort };
    assert!(v.newest_normal_for("a", false, 2, 256, 144, Box::new(|| None), v.view_epoch(), privacy).is_none());
}

#[test]
fn background_does_not_hold_a_late_mask_while_the_crop_is_a_guess() {
    let (v, _) = finder(Duration::from_millis(1));
    let sure = MaskView { geometry: Some(Geometry::default()), keep_late: true, ..Default::default() };
    v.newest_normal_for("a", true, 1, 256, 144, Box::new(|| Some(someone_at(50))), v.view_epoch(), sure).unwrap();
    age_newest(&v, MASK_MAX_AGE_S * 2.0);
    let guess = MaskView { unsure: true, ..sure };
    assert!(v.newest_normal_for("a", false, 2, 256, 144, Box::new(|| None), v.view_epoch(), guess).is_none());
    assert!(v.newest_normal_for("a", false, 2, 256, 144, Box::new(|| None), v.view_epoch(), sure).is_some());
}

#[test]
fn a_guessed_crop_does_not_carry_what_the_model_remembers() {
    let (v, _) = finder(Duration::from_millis(1));
    let stream = v.stream("a");
    let mut state = stream.mask.lock().unwrap();
    let (old, new) = (Geometry::default(), Geometry { zoom: 1.05, ..Default::default() });
    state.geometry(Some(old), MaskSpace::default(), false);
    state.small = Some(Gray { w: 4, h: 4, data: vec![255; 16] });
    state.geometry(Some(new), MaskSpace::default(), false);
    assert!(state.small.is_some(), "a small move of a known crop is carried");
    state.small = Some(Gray { w: 4, h: 4, data: vec![255; 16] });
    state.geometry(Some(old), MaskSpace::default(), true);
    assert!(state.small.is_none(), "a move to a guessed crop starts over");
}

#[test]
fn a_moving_crop_remaps_the_held_mask_and_queues_the_cropped_picture() {
    let (v, _) = finder(Duration::from_millis(1));
    let old = MaskView { geometry: Some(Geometry::default()), keep_late: true, ..Default::default() };
    v.newest_normal_for("a", true, 1, 256, 144, Box::new(|| Some(someone_at(50))), v.view_epoch(), old).unwrap();
    let new = MaskView { geometry: Some(Geometry { zoom: 2.0, ..Default::default() }), ..old };
    let (started, seen) = mpsc::channel();
    let (release, finish) = mpsc::channel();
    let held = v.newest_normal_for("a", false, 2, 256, 144, Box::new(move || {
        started.send(()).unwrap();
        finish.recv_timeout(Duration::from_secs(5)).unwrap();
        Some(someone_at(150))
    }), v.view_epoch(), new).unwrap();
    seen.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(held[80 * 256 + 40] > 128, "the old silhouette moved with the current crop");
    assert_eq!(held[80 * 256 + 140], 0, "the silhouette was not stretched over its old coordinates");
    let cached = v.newest_normal_for("a", false, 2, 256, 144, Box::new(|| None), v.view_epoch(), new).unwrap();
    assert!(Arc::ptr_eq(&held, &cached), "an unchanged crop reuses its remapping");
    release.send(()).unwrap();
    wait(|| v.stream("a").newest.lock().unwrap().done >= 2);
    let fresh = v.newest_normal_for("a", false, 2, 256, 144, Box::new(|| None), v.view_epoch(), new).unwrap();
    assert_eq!(fresh[80 * 256 + 40], 0, "a new crop does not blend in a silhouette from the old coordinates");
    assert!(fresh[80 * 256 + 180] > 128, "the newly cropped picture replaces the held mask");
}

#[test]
fn remapping_a_mask_lines_up_with_pixels_cropped_from_the_same_source() {
    use telescope_core::frame::{Frame, FrameContext, FrameStep, PixelFormat};
    use telescope_core::host::View;
    struct Context;
    impl FrameContext for Context {
        fn source_id(&self) -> &str { "a" }
        fn person_mask(&self, _: &Frame) -> Option<Arc<Vec<u8>>> { None }
        fn colour_preview(&self, frame: &Frame) -> Frame { frame.clone() }
    }
    let (w, h) = (32, 24);
    let mask: Vec<u8> = (0..h).flat_map(|y| (0..w).map(move |x| ((x + y) * 2) as u8)).collect();
    let source = Frame::new(w as u32, h as u32, PixelFormat::Rgba, mask.iter().flat_map(|&v| [v, v, v, 255]).collect());
    for view in [View { zoom: 2.0, pan_x: -1.0, ..Default::default() }, View { zoom: 2.0, pan_x: 1.0, ..Default::default() },
                 View { zoom: 2.0, pan_y: 0.5, rotation: Some(90), flip_h: true, ..Default::default() }] {
        let mut picture = source.clone();
        for step in telescope_core::view_step::steps(&view) {
            let (FrameStep::Cpu(mut step) | FrameStep::Either(_, mut step)) = step else { panic!("every view step has a CPU way") };
            step.process(&mut picture, &Context);
        }
        let remapped = remap_mask(&mask, w, h, picture.width as usize, picture.height as usize, &Geometry::default(), &picture.geometry.unwrap());
        for (&mask, pixel) in remapped.iter().zip(picture.data.as_chunks::<4>().0) {
            assert!(mask.abs_diff(pixel[0]) <= 1, "{view:?}: mask {mask}, picture {}", pixel[0]);
        }
    }
}

#[test]
fn background_remaps_across_view_epochs_without_clearing_other_streams() {
    let (v, _) = finder(Duration::from_millis(1));
    let old = MaskView { geometry: Some(Geometry::default()), keep_late: true, ..Default::default() };
    for key in ["a", "b"] {
        v.newest_normal_for(key, true, 1, 256, 144, Box::new(|| Some(someone_at(20))), v.view_epoch(), old).unwrap();
    }
    v.drop_masks();
    let epoch = v.view_epoch();
    let new = MaskView { geometry: Some(Geometry { flip_h: true, rotation: Some(90), ..Default::default() }), ..old };
    let turned = v.newest_normal_for("a", false, 2, 144, 256, Box::new(|| None), epoch, new).unwrap();
    let expected = remap_mask(&v.stream("a").newest.lock().unwrap().background.as_ref().unwrap().mask,
        256, 144, 144, 256, &old.geometry.unwrap(), &new.geometry.unwrap());
    assert_eq!(*turned, expected);
    assert!(turned.iter().any(|&p| p > 128));
    assert!(v.newest_normal_for("a", false, 2, 144, 256, Box::new(|| None), epoch, MaskView { keep_late: false, ..new }).is_none());
    let unchanged = v.newest_normal_for("b", false, 2, 256, 144, Box::new(|| None), epoch, old).unwrap();
    assert!(unchanged[80 * 256 + 30] > 128);
}

#[test]
fn background_keeps_an_inflight_mask_that_finishes_after_a_view_turn() {
    let (v, _) = finder(Duration::from_millis(1));
    let old = MaskView { geometry: Some(Geometry::default()), keep_late: true, ..Default::default() };
    let (started, seen) = mpsc::channel();
    let (release, finish) = mpsc::channel();
    assert!(v.newest_normal_for("a", false, 1, 256, 144, Box::new(move || {
        started.send(()).unwrap();
        finish.recv_timeout(Duration::from_secs(5)).unwrap();
        Some(someone_at(20))
    }), v.view_epoch(), old).is_none());
    seen.recv_timeout(Duration::from_secs(5)).unwrap();
    v.drop_masks();
    release.send(()).unwrap();
    wait(|| v.stream("a").newest.lock().unwrap().done >= 1);
    let new = MaskView { geometry: Some(Geometry { flip_h: true, ..Default::default() }), ..old };
    let held = v.newest_normal_for("a", false, 2, 256, 144, Box::new(|| None), v.view_epoch(), new).unwrap();
    assert!(held[80 * 256 + 200] > 128);
    assert!(v.stream("a").newest.lock().unwrap().mask.is_none());
}

#[test]
fn background_rejects_masks_from_another_lens_aspect_or_warp() {
    let (v, _) = finder(Duration::from_millis(1));
    let old = MaskView { geometry: Some(Geometry::default()), keep_late: true,
        space: MaskSpace { source_size: (256, 144), source_epoch: 1, warp: None, crop_unsure: false }, unsure: false };
    v.newest_normal_for("a", true, 1, 256, 144, Box::new(|| Some(someone_at(20))), v.view_epoch(), old).unwrap();
    for space in [MaskSpace { source_epoch: 2, ..old.space }, MaskSpace { source_size: (256, 192), ..old.space },
                  MaskSpace { warp: Some(42), ..old.space }] {
        assert!(v.newest_normal_for("a", false, 2, 256, 144, Box::new(|| None), v.view_epoch(), MaskView { space, ..old }).is_none());
    }
    assert!(v.newest_normal_for("a", false, 2, 256, 144, Box::new(|| None), v.view_epoch(), old).is_some());
}

#[test]
fn shrinking_a_stretched_axis_keeps_the_new_padding_empty() {
    for (sx, sy) in [(0.5, 1.0), (1.0, 0.5), (0.5, 0.5)] {
        let old = Geometry::default();
        let new = Geometry { stretch_x: sx, stretch_y: sy, ..Default::default() };
        let mapped = remap_mask(&[255; 16 * 16], 16, 16, 16, 16, &old, &new);
        for y in 0..16 {
            for x in 0..16 {
                let (u, v) = telescope_core::view::unstretch((x as f64 + 0.5) / 16.0, (y as f64 + 0.5) / 16.0, sx, sy);
                let expected = if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) { 255 } else { 0 };
                assert_eq!(mapped[y * 16 + x], expected, "stretch ({sx}, {sy}) at ({x}, {y})");
            }
        }
    }
}

/// A segmenter that says where the bright block in the frame is, and only finishes a picture when the test lets it.
struct Held {
    started: Mutex<mpsc::Sender<()>>,
    permit: Mutex<Option<mpsc::Receiver<()>>>,
}

struct HeldSeg {
    started: mpsc::Sender<()>,
    permit: mpsc::Receiver<()>,
}

impl Segmenter for HeldSeg {
    fn segment(&mut self, frame: &Rgb, _ts: i64) -> Result<Gray, VisionError> {
        self.started.send(()).unwrap();
        self.permit.recv_timeout(Duration::from_secs(5)).unwrap();
        Ok(Gray { w: frame.w, h: frame.h, data: frame.data.as_chunks::<3>().0.iter().map(|p| if p[0] > 128 { 255 } else { 0 }).collect() })
    }
}

impl Backend for Held {
    fn version(&self) -> String {
        "held".into()
    }
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        let permit = self.permit.lock().unwrap().take().unwrap();
        Ok(Box::new(HeldSeg { started: self.started.lock().unwrap().clone(), permit }))
    }
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        Err(VisionError::new("Nope", "no pose here"))
    }
}

/// The bright block of a frame's red channel as (left, top, right, bottom), or None when there is none.
fn block_of(data: impl Iterator<Item = u8>, w: usize) -> Option<(usize, usize, usize, usize)> {
    let mut found: Option<(usize, usize, usize, usize)> = None;
    for (i, v) in data.enumerate() {
        if v > 128 {
            let (x, y) = (i % w, i / w);
            found = Some(found.map_or((x, y, x, y), |(l, t, r, b)| (l.min(x), t.min(y), r.max(x), b.max(y))));
        }
    }
    found
}

/// A person (a bright block) in a 256 x 144 source, and what the frame steps of a view make of it, with the geometry they record.
fn framed(source: &telescope_core::frame::Frame, view: &telescope_core::host::View, crop: (f64, f64, f64)) -> telescope_core::frame::Frame {
    use telescope_core::frame::{Frame, FrameContext, FrameStep};
    struct Context;
    impl FrameContext for Context {
        fn source_id(&self) -> &str { "a" }
        fn person_mask(&self, _: &Frame) -> Option<Arc<Vec<u8>>> { None }
        fn colour_preview(&self, frame: &Frame) -> Frame { frame.clone() }
    }
    let run = |steps: Vec<FrameStep>, mut frame: Frame| {
        for step in steps {
            let (FrameStep::Cpu(mut step) | FrameStep::Either(_, mut step)) = step else { panic!("every view step has a CPU way") };
            step.process(&mut frame, &Context);
        }
        frame
    };
    // What the phone sends: the source cropped by its share, with no record of where it came from
    let phone = if crop == (1.0, 0.0, 0.0) {
        source.clone()
    } else {
        run(telescope_core::view_step::steps(&telescope_core::host::View { zoom: crop.0 as f32, pan_x: crop.1 as f32, pan_y: crop.2 as f32, ..Default::default() }), source.clone())
    };
    let mut phone = phone;
    phone.geometry = None;
    run(telescope_core::view_step::steps_in(view, crop), phone)
}

fn person_in(x: usize, y: usize, w: usize, h: usize) -> telescope_core::frame::Frame {
    use telescope_core::frame::{Frame, PixelFormat};
    let mut data = vec![0u8; 256 * 144 * 4];
    for px in data.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    for row in y..y + h {
        for col in x..x + w {
            data[(row * 256 + col) * 4..][..3].fill(255);
        }
    }
    Frame::new(256, 144, PixelFormat::Rgba, data)
}

/// Frames of a still person cut from a crop that moves every frame, and masks that finish `late` frames after their picture: whatever mask
/// Background has for a frame has to cover the person in that frame.
fn held_masks_follow_the_person(person: (usize, usize, usize, usize), exact: bool, crops: &[((f64, f64, f64), telescope_core::host::View)], late: usize) -> Vec<String> {
    let source = person_in(person.0, person.1, person.2, person.3);
    let (started, seen) = mpsc::channel();
    let (permit, receive) = mpsc::channel();
    let backend: Arc<dyn Backend> = Arc::new(Held { started: Mutex::new(started), permit: Mutex::new(Some(receive)) });
    let (v, _, _) = make(Box::new(move || Ok(backend.clone())));
    v.load();
    wait(|| v.state() == READY);
    let (mut inflight, mut pending) = (None::<usize>, false);
    let mut faults = Vec::new();
    for (i, (crop, view)) in crops.iter().enumerate() {
        if inflight.is_some_and(|s| i >= s + late) {
            let before = v.stream("a").newest.lock().unwrap().done;
            permit.send(()).unwrap();
            wait(|| v.stream("a").newest.lock().unwrap().done > before);
            inflight = None;
            if std::mem::take(&mut pending) {
                seen.recv_timeout(Duration::from_secs(5)).unwrap();
                inflight = Some(i);
            }
        }
        let frame = framed(&source, view, *crop);
        let rgb = Rgb::new(256, 144, frame.data.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect());
        let geometry = frame.geometry.as_deref().copied().unwrap_or_default();
        let mask_view = MaskView { geometry: Some(geometry), keep_late: true, ..Default::default() };
        let held = v.newest_normal_for("a", false, i as u64 + 1, 256, 144, Box::new(move || Some(rgb)), v.view_epoch(), mask_view);
        if inflight.is_none() && !pending {
            seen.recv_timeout(Duration::from_secs(5)).unwrap();
            inflight = Some(i);
        } else if inflight != Some(i) {
            pending = true;
        }
        let Some(held) = held else { continue };
        let ideal = block_of(frame.data.as_chunks::<4>().0.iter().map(|p| p[0]), 256).expect("the person is in every frame");
        // No backdrop anywhere over the person, bar the soft rim of their edge
        let covered = (ideal.1 + 2..=ideal.3.saturating_sub(2)).all(|y| (ideal.0 + 2..=ideal.2.saturating_sub(2)).all(|x| held[y * 256 + x] > 128));
        let got = block_of(held.iter().copied(), 256);
        let aligned = got.is_some_and(|got| [got.0.abs_diff(ideal.0), got.1.abs_diff(ideal.1), got.2.abs_diff(ideal.2), got.3.abs_diff(ideal.3)].iter().all(|&d| d <= 2));
        if !covered || (exact && !aligned) {
            faults.push(format!("frame {i}: person at {ideal:?}, held mask at {got:?}, covered {covered}"));
        }
    }
    permit.send(()).ok();
    v.drop_stream("a");
    faults
}

#[test]
fn a_held_mask_covers_the_person_while_the_desktop_crop_moves() {
    use telescope_core::host::View;
    let crops: Vec<_> = (0..14).map(|i| ((1.0, 0.0, 0.0), View { zoom: 2.0, pan_x: -0.5 + i as f32 * 0.06, pan_y: 0.1 - i as f32 * 0.02, ..Default::default() })).collect();
    let faults = held_masks_follow_the_person((110, 60, 30, 24), true, &crops, 3);
    assert!(faults.is_empty(), "{faults:#?}");
}

#[test]
fn a_held_mask_covers_the_person_while_the_phone_crop_moves() {
    use telescope_core::host::View;
    let crops: Vec<_> = (0..14).map(|i| ((2.0, -0.2 + i as f64 * 0.02, 0.0), View { zoom: 2.0, pan_x: 0.1 - i as f32 * 0.01, ..Default::default() })).collect();
    let faults = held_masks_follow_the_person((110, 60, 30, 24), true, &crops, 3);
    assert!(faults.is_empty(), "{faults:#?}");
}

#[test]
fn a_held_mask_keeps_covering_a_person_the_old_crop_cut_off() {
    use telescope_core::host::View;
    // The crop swings across to follow a person who is wider than the part of them the first masks were made on
    let crops: Vec<_> = (0..12).map(|i| ((1.0, 0.0, 0.0), View { zoom: 2.0, pan_x: -0.7 + i as f32 * 0.07, ..Default::default() })).collect();
    let faults = held_masks_follow_the_person((100, 60, 60, 24), false, &crops, 3);
    assert!(faults.is_empty(), "{faults:#?}");
}

#[test]
fn a_crop_that_moves_a_little_keeps_the_masks_steadying_carry() {
    let (v, _, _) = make({
        let backend = Fake::new();
        backend.values.lock().unwrap().extend([255, 0]);
        Box::new(move || Ok(backend.clone() as Arc<dyn Backend>))
    });
    v.load();
    wait(|| v.state() == READY);
    v.wait_for_masks();
    let ask = |seq: u64, zoom: f64| {
        let view = MaskView { geometry: Some(Geometry { zoom, ..Default::default() }), keep_late: true, ..Default::default() };
        v.newest_normal_for("a", false, seq, 256, 144, Box::new(move || Some(Rgb::filled(256, 144, seq as u8 * 60))), v.view_epoch(), view).unwrap()
    };
    let first = ask(1, 1.0);
    assert_eq!(first[72 * 256 + 128], 255);
    let steadied = ask(2, 1.05);
    assert!(steadied[72 * 256 + 128] > 0 && steadied[72 * 256 + 128] < 200, "the second mask is blended with the first, moved with the crop");
    let jumped = ask(3, 3.0);
    assert!(jumped[72 * 256 + 128] >= 200, "a crop that jumps starts the mask afresh, with nothing of the old one in it");
}

#[test]
fn a_mask_got_again_without_a_run_keeps_the_frame_it_was_made_for() {
    for waiting in [true, false] {
        let (v, _) = finder(Duration::from_millis(1));
        if waiting { v.wait_for_masks(); }
        let ask = |seq: u64| v.newest_normal_snapshot("a", true, seq, 256, 144, Box::new(|| Some(someone_at(50))), v.view_epoch(), MaskView::default()).unwrap();
        assert_eq!(ask(1).seq, 1);
        // Nothing moved and the last run is recent, so the next frames get that run's mask again
        v.stream("a").mask.lock().unwrap().at = monotonic() + 100.0;
        if waiting {
            assert_eq!(ask(2).seq, 1);
        } else {
            ask(2);
            while v.stream("a").newest.lock().unwrap().done < 2 { std::thread::sleep(Duration::from_millis(5)); }
            assert_eq!(ask(3).seq, 1);
        }
    }
}

struct SlowOpen(Arc<Fake>, Duration);

impl Backend for SlowOpen {
    fn version(&self) -> String {
        "slow".into()
    }
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        std::thread::sleep(self.1);
        self.0.segmenter()
    }
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        self.0.pose()
    }
}

#[test]
fn the_first_mask_after_a_cold_open_over_the_age_limit_is_still_fresh() {
    let backend: Arc<dyn Backend> = Arc::new(SlowOpen(Fake::new(), Duration::from_millis(600)));
    let (v, _, _) = make(Box::new(move || Ok(backend.clone())));
    v.load();
    wait(|| v.state() == READY);
    v.wait_for_masks();
    let asked = monotonic();
    let snap = v.newest_normal_snapshot("a", true, 1, 256, 144, Box::new(|| Some(Rgb::filled(256, 144, 200))), v.view_epoch(), MaskView::default()).unwrap();
    assert!(snap.fresh(), "the first mask was {:.2}s old as soon as it was made", snap.age());
    assert!(snap.at - asked >= 0.5, "the mask's own time went by the start, not the run");
    assert!(snap.frame_at - asked < 0.2, "its frame's time was taken after the open: {:.2}s", snap.frame_at - asked);
}

/// A backend whose 256 wide frames wait at the model until the gate opens, to hold one worker back.
struct GatedOnce(Arc<Fake>, Arc<(Mutex<bool>, Condvar)>);

struct GatedSeg(Box<dyn Segmenter>, Arc<(Mutex<bool>, Condvar)>);

impl Segmenter for GatedSeg {
    fn segment(&mut self, frame: &Rgb, ts_ms: i64) -> Result<Gray, VisionError> {
        if frame.w == 256 {
            let (open, changed) = &*self.1;
            let _held = changed.wait_while(open.lock().unwrap(), |open| !*open).unwrap();
        }
        self.0.segment(frame, ts_ms)
    }
}

impl Backend for GatedOnce {
    fn version(&self) -> String {
        "gated".into()
    }
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        Ok(Box::new(GatedSeg(self.0.segmenter()?, self.1.clone())))
    }
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        self.0.pose()
    }
}

#[test]
fn a_normal_worker_that_starts_late_on_an_older_frame_does_not_lead_a_newer_cutout() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let backend: Arc<dyn Backend> = Arc::new(GatedOnce(Fake::new(), gate.clone()));
    let b = backend.clone();
    let (v, _, _) = make(Box::new(move || Ok(b.clone())));
    v.load();
    wait(|| v.state() == READY);
    let (epoch, view) = (v.view_epoch(), MaskView::default());
    let ask = |key: &str, which, seq, w: usize, h: usize| {
        v.newest_from(key, backend.clone(), which, false, seq, w, h, None, Box::new(move || Some(Rgb::filled(w, h, 200))), epoch, view)
    };
    // The normal worker takes frame 1 and stays in the model; frame 2 waits behind it
    ask("a", Which::Normal, 1, 256, 144);
    wait(|| v.stream("a").newest.lock().unwrap().job.is_none());
    ask("a", Which::Normal, 2, 256, 144);
    // The cutout's worker takes frames 3 and 4, newer than both, at once
    ask(&cutout_key("a"), Which::Cutout, 3, 160, 90);
    ask(&cutout_key("a"), Which::Cutout, 4, 160, 90);
    wait(|| ask(&cutout_key("a"), Which::Cutout, 4, 160, 90).is_some());
    // The normal worker gets to frame 2 well over the cutout's lead later
    std::thread::sleep(Duration::from_millis(250));
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    wait(|| v.stream("a").newest.lock().unwrap().done >= 2);
    let normal = ask("a", Which::Normal, 2, 256, 144).unwrap();
    let hq = ask(&cutout_key("a"), Which::Cutout, 4, 160, 90).unwrap();
    assert!(normal.seq < hq.seq, "the normal mask is of the older frame");
    assert!(normal.fresh() && hq.fresh());
    let chosen = crate::preferred_mask(Some(hq), Some(normal), false).unwrap();
    assert_eq!(chosen.mask.len(), 160 * 90, "the cutout's newer outline stays, not the normal one of an older frame");
}

#[test]
fn a_mask_after_the_view_turned_is_stamped_with_the_frame_it_was_run_for() {
    for waiting in [true, false] {
        let (v, _) = finder(Duration::from_millis(1));
        if waiting { v.wait_for_masks(); }
        let ask = |seq: u64| v.newest_normal_snapshot("a", true, seq, 256, 144, Box::new(|| Some(someone_at(50))), v.view_epoch(), MaskView::default()).unwrap();
        assert_eq!(ask(1).seq, 1);
        v.stream("a").mask.lock().unwrap().at = monotonic() + 100.0;
        v.drop_masks();
        // What was kept is gone, so this frame is run afresh and is the one the mask is of
        assert_eq!(ask(7).seq, 7, "waiting {waiting}");
        assert_eq!(ask(8).seq, 7, "waiting {waiting}");
    }
}
