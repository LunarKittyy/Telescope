//! test_vision.py, by name, against a fake MediaPipe (the real one is in tests/parity.rs).

use super::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};
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
    let v = Vision::new(Box::new(move |k, found| p.lock().unwrap().push((k.to_string(), found))),
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

fn mask(v: &Vision, key: &str, seq: u64, frame: &Rgb) -> Option<Arc<Vec<u8>>> {
    v.person_mask(key, seq, frame.w, frame.h, &mut || Some(frame.clone()))
}

fn feed(v: &Vision, key: &str, frame: &Rgb) {
    v.feed_raw(key, &mut || Some(frame.clone()), Upright::default())
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
    sv(v, "a").mask.lock().unwrap().at -= HELD + MASK_STILL_EVERY_S + TICK;
}

fn with_arm(base: &Rgb, x: (usize, usize), y: (usize, usize), value: u8) -> Rgb {
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
    sv(&r.v, "a").mask.lock().unwrap().at -= MASK_STILL_EVERY_S + TICK;
    let calm = mask(&r.v, "a", 2, &still).unwrap();
    assert_eq!(calm[0], (200.0 * MASK_CARRY_STILL).round() as u8);
    let moved = with_arm(&still, (400, 700), (200, 500), 200);
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
    let v = Vision::new(Box::new(|_, _| {}), Box::new(move |state| {
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
    let upright = Upright { turn: Turn { rotation: Some(270), ..Turn::default() }, sent: (2.0, 0.0, 0.0) };
    r.v.feed_raw("a", &mut || Some(Rgb::filled(1280, 720, 0)), upright);
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
    }
    assert_eq!(sizes, [(640, 360)]);
}

fn sent_each(r: &Ready, frames: &[(&Rgb, f64)]) -> Vec<bool> {
    frames.iter().map(|(frame, ago)| {
        feed(&r.v, "a", frame);
        let s = sv(&r.v, "a");
        let mut p = s.people.lock().unwrap();
        let sent = p.frame.take().is_some();
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

#[test]
fn test_power_saving_runs_the_models_less_often_even_while_something_moves() {
    let r = ready();
    r.v.set_power_saving(true);
    let moving: Vec<Rgb> = (0..3).map(|i| with_arm(&Rgb::filled(1280, 720, 80), (400, 700), (100 * i, 100 * i + 300), 200)).collect();
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
    let sent = sent_each(&r, &[(&moving[0], PEOPLE_EVERY_S), (&moving[1], SAVING_PEOPLE_EVERY_S), (&moving[2], 0.0)]);
    assert_eq!(sent, [true, false, true]);
}

fn released_counter(v: &Vision) -> Arc<Mutex<u32>> {
    let released = Arc::new(Mutex::new(0));
    let r = released.clone();
    *v.inner.give_back.lock().unwrap() = Box::new(move || *r.lock().unwrap() += 1);
    released
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
    v.stream("a").newest.lock().unwrap().mask.as_mut().unwrap().at -= by;
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
