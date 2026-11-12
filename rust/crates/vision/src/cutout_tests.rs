//! The high quality cutout's side of the service, against a fake model that counts how often its memory is cleared.

use super::tests::{ready, wait, Ready};
use super::*;
use std::sync::atomic::AtomicU32;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Counts {
    made: AtomicU32,
    resets: AtomicU32,
    runs: AtomicU32,
    closed: AtomicU32,
}

struct Memory {
    counts: Arc<Counts>,
    took: Duration,
    broken: Arc<AtomicBool>,
}

impl Segmenter for Memory {
    fn segment(&mut self, frame: &Rgb, _ts: i64) -> Result<Gray, VisionError> {
        self.counts.runs.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.took);
        if self.broken.load(Ordering::SeqCst) {
            return Err(VisionError::new("RuntimeError", "broken"));
        }
        Ok(Gray { w: frame.w, h: frame.h, data: vec![200; frame.w * frame.h] })
    }

    fn input(&self, _frame: &Rgb) -> Option<Rgb> {
        None
    }

    fn blends_last(&self) -> bool {
        false
    }

    fn reset(&mut self) {
        self.counts.resets.fetch_add(1, Ordering::SeqCst);
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        self.counts.closed.fetch_add(1, Ordering::SeqCst);
    }
}

struct Cutouts {
    counts: Arc<Counts>,
    took: Duration,
    broken: Arc<AtomicBool>,
}

impl Backend for Cutouts {
    fn version(&self) -> String {
        "memory".into()
    }
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError> {
        self.counts.made.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Memory { counts: self.counts.clone(), took: self.took, broken: self.broken.clone() }))
    }
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError> {
        Err(VisionError::new("Nope", "no pose in the cutout"))
    }
}

struct Rig {
    normal: Ready,
    counts: Arc<Counts>,
    broken: Arc<AtomicBool>,
    states: Arc<Mutex<Vec<String>>>,
}

fn rig_taking(took: Duration) -> Rig {
    let normal = ready();
    let (counts, broken): (Arc<Counts>, Arc<AtomicBool>) = Default::default();
    let backend: Arc<dyn Backend> = Arc::new(Cutouts { counts: counts.clone(), took, broken: broken.clone() });
    let states: Arc<Mutex<Vec<String>>> = Default::default();
    let heard = states.clone();
    normal.v.set_cutout(Box::new(move || Ok(backend.clone())), None, Box::new(move |s| heard.lock().unwrap().push(s.to_string())));
    Rig { normal, counts, broken, states }
}

fn frame() -> Rgb {
    Rgb::filled(64, 36, 90)
}

fn ask(v: &Vision, seq: u64) -> Option<Arc<Vec<u8>>> {
    let f = frame();
    v.newest_cutout("a", seq, f.w, f.h, Box::new(move || Some(f)))
}

/// Asks until the cutout has loaded and its thread has made a mask.
fn first_mask(v: &Vision) -> Arc<Vec<u8>> {
    let seq = std::cell::Cell::new(0);
    let got = std::cell::RefCell::new(None);
    wait(|| {
        seq.set(seq.get() + 1);
        *got.borrow_mut() = ask(v, seq.get());
        got.borrow().is_some()
    });
    got.into_inner().unwrap()
}

fn cutout_stream(v: &Vision) -> Arc<StreamVision> {
    v.stream(&cutout_key("a"))
}

#[test]
fn a_service_without_a_cutout_says_it_is_missing_and_gives_no_mask() {
    let r = ready();
    assert_eq!(r.v.cutout_state(), MISSING);
    assert!(ask(&r.v, 1).is_none());
    // the normal model neither knows nor cares
    assert_eq!(r.v.state(), READY);
    r.v.load_cutout();
    r.v.cutout_downloaded();
}

#[test]
fn the_cutout_loads_when_first_asked_and_makes_masks_of_the_frames_size() {
    let r = rig_taking(Duration::ZERO);
    assert_eq!(r.normal.v.cutout_state(), OFF);
    assert!(ask(&r.normal.v, 1).is_none(), "nothing yet while it loads");
    let mask = first_mask(&r.normal.v);
    assert_eq!(mask.len(), 64 * 36);
    assert!(mask.iter().all(|v| *v == 200));
    assert_eq!(r.normal.v.cutout_state(), READY);
    assert_eq!(*r.states.lock().unwrap(), [LOADING, READY]);
}

#[test]
fn a_cutout_not_downloaded_stays_missing_until_it_is() {
    let normal = ready();
    let there = Arc::new(AtomicBool::new(false));
    let backend: Arc<dyn Backend> = Arc::new(Cutouts { counts: Default::default(), took: Duration::ZERO, broken: Default::default() });
    let (t, states): (Arc<AtomicBool>, Arc<Mutex<Vec<String>>>) = (there.clone(), Default::default());
    let heard = states.clone();
    normal.v.set_cutout(
        Box::new(move || if t.load(Ordering::SeqCst) { Ok(backend.clone()) } else { Err(LoadError::Missing("no".into())) }),
        None, Box::new(move |s| heard.lock().unwrap().push(s.to_string())));
    assert!(ask(&normal.v, 1).is_none());
    wait(|| normal.v.cutout_state() == MISSING);
    // asked again at once it doesn't look again (it waits LOOK_AGAIN_S)
    assert!(ask(&normal.v, 2).is_none());
    there.store(true, Ordering::SeqCst);
    normal.v.cutout_downloaded();
    assert_eq!(normal.v.cutout_state(), OFF);
    first_mask(&normal.v);
    assert_eq!(*states.lock().unwrap(), [LOADING, MISSING, OFF, LOADING, READY]);
}

#[test]
fn a_cutout_that_wont_load_fails_alone_and_the_normal_mask_carries_on() {
    let normal = ready();
    let e = VisionError::new("OSError", "libonnxruntime.so: cannot open shared object file: No such file or directory");
    normal.v.set_cutout(Box::new(move || Err(LoadError::Failed(e.clone()))), None, Box::new(|_| {}));
    ask(&normal.v, 1);
    wait(|| normal.v.cutout_state() == FAILED);
    assert_eq!(normal.v.cutout_problem(), "OSError: libonnxruntime.so missing");
    assert_eq!(normal.v.state(), READY);
    assert!(normal.v.person_mask("a", 1, 6, 4, &mut || Some(Rgb::filled(6, 4, 0))).is_some());
    assert!(ask(&normal.v, 2).is_none());
    let d = format!("{:?}", normal.v.diagnostics());
    assert!(d.contains("High quality cutout") && d.contains("didn't load"), "{d}");
}

#[test]
fn a_slow_cutout_never_holds_a_frame_up() {
    let r = rig_taking(Duration::from_millis(300));
    let v = &r.normal.v;
    ask(v, 1);
    wait(|| v.cutout_state() == READY);
    // Its first mask is 300 ms away, and nothing waits for it: every ask is quick and has no mask to give yet
    for seq in 2..12 {
        let began = Instant::now();
        let mask = ask(v, seq);
        assert!(began.elapsed() < Duration::from_millis(50), "{:?}", began.elapsed());
        assert!(mask.is_none() || seq > 2);
        std::thread::sleep(Duration::from_millis(5));
    }
    wait(|| ask(v, 100).is_some());
    // Frames come faster than the model: it skips some instead of queueing them
    (200..230).for_each(|seq| {
        ask(v, seq);
        std::thread::sleep(Duration::from_millis(10));
    });
    assert!(r.counts.runs.load(Ordering::SeqCst) < 12, "{}", r.counts.runs.load(Ordering::SeqCst));
}

#[test]
fn a_cutout_mask_that_gets_too_old_is_not_handed_out() {
    let r = rig_taking(Duration::ZERO);
    let v = &r.normal.v;
    first_mask(v);
    v.stream(&cutout_key("a")).newest.lock().unwrap().mask.as_mut().unwrap().at -= MASK_MAX_AGE_S + 0.1;
    let sv = cutout_stream(v);
    // the thread's next mask is for the frame just handed over; until then the old one stays unusable
    sv.newest.lock().unwrap().asked = Some(1);
    assert!(ask(v, 1).is_none());
}

#[test]
fn the_models_memory_starts_again_after_a_gap_a_new_size_and_a_new_stream() {
    let r = rig_taking(Duration::ZERO);
    let v = &r.normal.v;
    v.wait_for_masks();
    let (frame, bigger) = (frame(), Rgb::filled(128, 72, 90));
    let go = |seq, f: &Rgb| {
        let f2 = f.clone();
        v.newest_cutout("a", seq, f.w, f.h, Box::new(move || Some(f2)))
    };
    // a cutout that isn't loaded yet gives none; synchronously loaded here
    assert!(go(1, &frame).is_none());
    wait(|| v.cutout_state() == READY);
    assert!(go(2, &frame).is_some());
    let resets = || r.counts.resets.load(Ordering::SeqCst);
    for seq in 3..8 {
        assert!(go(seq, &frame).is_some());
    }
    assert_eq!(resets(), 0, "frames one after another keep its memory");
    // A camera switch or a phone that went away: nothing for longer than a mask lives
    cutout_stream(v).mask.lock().unwrap().at -= MASK_MAX_AGE_S + 0.5;
    assert!(go(8, &frame).is_some());
    assert_eq!(resets(), 1);
    assert!(go(9, &frame).is_some());
    assert_eq!(resets(), 1, "and only once");
    // The picture changes size (a quiet picture is only looked at every MASK_STILL_EVERY_S, so it's that long since the last run)
    cutout_stream(v).mask.lock().unwrap().at -= MASK_STILL_EVERY_S + 0.02;
    assert!(go(10, &bigger).is_some());
    assert_eq!(resets(), 2);
    assert!(go(11, &bigger).is_some());
    assert_eq!(resets(), 2);
    // The stream ends and starts again: a new model with no memory at all
    assert_eq!(r.counts.made.load(Ordering::SeqCst), 1);
    v.drop_stream("a");
    assert_eq!(r.counts.closed.load(Ordering::SeqCst), 1, "the stream's model closes with it");
    assert!(go(12, &frame).is_some());
    assert_eq!(r.counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn ending_a_stream_ends_its_cutout_and_its_thread() {
    let r = rig_taking(Duration::ZERO);
    let v = &r.normal.v;
    first_mask(v);
    assert!(v.inner.streams.lock().unwrap().contains_key(&cutout_key("a")));
    v.drop_stream("a");
    assert!(!v.inner.streams.lock().unwrap().contains_key(&cutout_key("a")));
    wait(|| r.counts.closed.load(Ordering::SeqCst) == 1);
}

#[test]
fn a_cutout_that_keeps_failing_is_given_up_without_touching_the_normal_model() {
    let r = rig_taking(Duration::ZERO);
    let v = &r.normal.v;
    first_mask(v);
    r.broken.store(true, Ordering::SeqCst);
    let seq = std::cell::Cell::new(1000);
    wait(|| {
        seq.set(seq.get() + 1);
        ask(v, seq.get());
        v.cutout_state() == FAILED
    });
    assert_eq!(v.state(), READY);
    assert_eq!(v.cutout_problem(), "RuntimeError");
    assert!(ask(v, seq.get() + 1).is_none());
}

#[test]
fn the_cutout_closes_when_nobody_asks_and_comes_back_when_asked() {
    let r = rig_taking(Duration::ZERO);
    let v = &r.normal.v;
    first_mask(v);
    assert_eq!(r.counts.closed.load(Ordering::SeqCst), 0);
    v.tidy(Some(monotonic() + IDLE_CLOSE_S + 1.0));
    assert_eq!(r.counts.closed.load(Ordering::SeqCst), 1);
    first_mask(v);
    assert_eq!(r.counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn a_turn_of_the_view_leaves_no_cutout_mask_of_the_old_picture_and_clears_the_models_memory() {
    let r = rig_taking(Duration::ZERO);
    let v = &r.normal.v;
    v.wait_for_masks();
    let f = frame();
    let go = |seq, epoch| {
        let f2 = f.clone();
        v.newest_cutout_in("a", seq, f.w, f.h, Box::new(move || Some(f2)), epoch)
    };
    assert!(go(1, v.view_epoch()).is_none());
    wait(|| v.cutout_state() == READY);
    assert!(go(2, v.view_epoch()).is_some());
    let before = v.view_epoch();
    v.drop_masks();
    assert_ne!(v.view_epoch(), before);
    let resets = r.counts.resets.load(Ordering::SeqCst);
    assert!(go(3, before).is_none(), "a picture from before the turn gets nothing");
    assert!(go(3, v.view_epoch()).is_some(), "the turned picture gets a new mask");
    assert_eq!(r.counts.resets.load(Ordering::SeqCst), resets + 1, "and the model starts again");
}

#[test]
fn the_fallback_to_the_normal_mask_never_holds_a_frame_up() {
    let (counts, broken): (Arc<Counts>, Arc<AtomicBool>) = Default::default();
    let slow = Cutouts { counts, took: Duration::from_millis(400), broken };
    let backend: Arc<dyn Backend> = Arc::new(slow);
    let (v, _, _) = super::tests::make(Box::new(move || Ok(backend.clone())));
    v.load();
    wait(|| v.state() == READY);
    // the normal model's first mask is 400 ms away and no cutout is ready: the old waiting ask would sit here for up to FIRST_MASK_WAIT_S
    for seq in 1..6 {
        let f = frame();
        let began = Instant::now();
        v.newest_mask_nowait_in("a", seq, f.w, f.h, Box::new(move || Some(f)), v.view_epoch());
        assert!(began.elapsed() < Duration::from_millis(100), "{:?}", began.elapsed());
        std::thread::sleep(Duration::from_millis(5));
    }
    wait(|| {
        let f = frame();
        v.newest_mask_nowait_in("a", 100, f.w, f.h, Box::new(move || Some(f)), v.view_epoch()).is_some()
    });
}

#[test]
fn ending_a_stream_does_not_wait_for_a_mask_still_being_made() {
    let r = rig_taking(Duration::from_millis(1500));
    // the first asks load the cutout and then hand the frame to its thread, whose run takes a while
    wait(|| {
        ask(&r.normal.v, 1);
        r.counts.runs.load(Ordering::SeqCst) > 0
    });
    let began = Instant::now();
    r.normal.v.drop_stream("a");
    assert!(began.elapsed() < Duration::from_millis(500), "ending the stream waited {:?} for the model", began.elapsed());
    // the model closes once its run ends
    let end = Instant::now() + Duration::from_secs(5);
    while r.counts.closed.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < end, "the model was never closed");
        std::thread::sleep(Duration::from_millis(10));
    }
}
