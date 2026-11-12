//! Person masks and people's positions, once per stream, for the plugins that ask (vision.py's Vision). MediaPipe loads on first
//! use; every call is safe before it's ready (None, nothing). A frame step gets the newest mask at once and leaves its frame for a
//! thread of the stream's own, so the model and the work around it never hold a frame up, and a mask is a frame or so behind the
//! picture and never over MASK_MAX_AGE_S behind (past that a step gets None). People come from another such thread, about 12 times a second
//! while something moves and 3 while nothing does. The high quality cutout (matting) is a second model behind the same kind of mask thread, on
//! a stream key of its own: it only ever stands in for the normal mask when it has one in time, and the normal one is what a frame step
//! gets while it's missing, loading, failed or late.

use crate::ops::{add_weighted, mask_resized, model_input, people_size, turned, Motion, Rgb, MODEL_SIZE};
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
use telescope_core::pose::Person;
use telescope_core::vision::{Upright, FAILED, LOADING, MISSING, OFF, READY};

/// How often people are looked for while something moves (about 12 a second); framing smooths in between.
pub const PEOPLE_EVERY_S: f64 = 0.08;
/// And while nothing does.
pub const PEOPLE_STILL_EVERY_S: f64 = 0.33;
/// The size the pose model gets the frame at.
pub const PEOPLE_WIDTH: usize = 640;
/// How much of the last frame's mask carries into the next.
pub const MASK_CARRY: f64 = 0.3;
/// And while nothing moves: steadier edges, and no movement to lag behind.
pub const MASK_CARRY_STILL: f64 = 0.5;
/// While nothing moves the mask is worked out this often, instead of for every frame.
pub const MASK_STILL_EVERY_S: f64 = 1.0 / 15.0;
/// Power-saving effects (Settings > Video): the mask at most this often even while something moves, people this often.
pub const SAVING_PEOPLE_EVERY_S: f64 = 0.16;
pub const SAVING_PEOPLE_STILL_EVERY_S: f64 = 0.5;
/// How long a frame waits for its mask when there's none it can use, so the picture doesn't go out without one when a stream starts.
pub const FIRST_MASK_WAIT_S: f64 = 0.5;
/// A mask of a frame further back than this is of a picture that's gone: frame steps hear there's none, not an old silhouette.
pub const MASK_MAX_AGE_S: f64 = 0.5;
/// How far apart two sizes' shapes can be for a mask of one to stand in for the other, as a share.
const MASK_SHAPE_SLACK: f64 = 0.02;
/// A failed cutout run this many times in a row (with no good one between) marks the cutout failed, so frames stop asking for it.
pub const CUTOUT_MAX_FAILS: u32 = 5;
/// Without a pack, how often asking looks for one again (a download may have finished).
pub const LOOK_AGAIN_S: f64 = 5.0;
/// A model nothing has asked for this long is closed and its memory given back; switched back on sooner, it's still there.
pub const IDLE_CLOSE_S: f64 = 10.0;
pub const TIDY_EVERY_S: f64 = 2.0;

/// Seconds on the monotonic clock (time.monotonic()).
pub fn monotonic() -> f64 {
    telescope_core::time::monotonic_ns() as f64 / 1e9
}

/// What went wrong, with Python's exception type for it (problem() and the diagnostics show only the type).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisionError {
    pub kind: String,
    pub message: String,
}

impl VisionError {
    pub fn new(kind: &str, message: &str) -> VisionError {
        VisionError { kind: kind.into(), message: message.into() }
    }
}

/// Why find() gave no backend: the pack isn't there (MISSING) or it didn't load (FAILED).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    Missing(String),
    Failed(VisionError),
}

/// A one-byte-a-pixel mask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

/// The person segmenter (one stream's: VIDEO mode keeps tracking state). Closed when dropped.
pub trait Segmenter: Send {
    /// How sure it is each pixel is a person, 0..255, as cv2.convertScaleAbs(mask, alpha=255) makes it.
    fn segment(&mut self, frame: &Rgb, ts_ms: i64) -> Result<Gray, VisionError>;
    /// The picture segment gets, shrunk from the frame; None when the frame is small enough as it is.
    fn input(&self, frame: &Rgb) -> Option<Rgb> {
        model_input(frame, MODEL_SIZE)
    }
    /// Whether each mask is steadied with the last one carried in (a model that keeps its own memory of the last frames doesn't need it).
    fn blends_last(&self) -> bool {
        true
    }
    /// Forget the last frames: the next one is the first of a new picture. Called after a gap and when the stream starts again.
    fn reset(&mut self) {}
}

/// The pose model (one stream's). Closed when dropped.
pub trait PoseModel: Send {
    fn detect(&mut self, frame: &Rgb, ts_ms: i64) -> Result<Vec<Person>, VisionError>;
}

/// Makes the models (MediaPipeBackend).
pub trait Backend: Send + Sync {
    fn version(&self) -> String;
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError>;
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError>;
}

pub type Find = Box<dyn Fn() -> Result<Arc<dyn Backend>, LoadError> + Send + Sync>;
pub type OnPeople = Box<dyn Fn(&str, Vec<Person>) + Send + Sync>;
pub type OnState = Box<dyn Fn(&str) + Send + Sync>;
/// Makes the RGB picture of a frame, on the thread that needs it.
pub type Picture = Box<dyn FnOnce() -> Option<Rgb> + Send>;

/// VIDEO mode wants each task's timestamps to go up, strictly.
#[derive(Debug)]
pub(crate) struct Timestamps {
    last: i64,
}

impl Timestamps {
    fn new() -> Timestamps {
        Timestamps { last: -1 }
    }

    pub(crate) fn next(&mut self) -> i64 {
        self.last = (self.last + 1).max((monotonic() * 1000.0) as i64);
        self.last
    }
}

pub(crate) struct MaskState {
    pub(crate) segment: Option<Box<dyn Segmenter>>,
    pub(crate) clock: Timestamps,
    /// The frame the cached mask is for.
    pub(crate) seq: Option<u64>,
    /// At the frame's size.
    pub(crate) mask: Option<(usize, usize, Arc<Vec<u8>>)>,
    /// As the model gave it, with the last ones carried in.
    pub(crate) small: Option<Gray>,
    pub(crate) at: f64,
    pub(crate) motion: Motion,
    pub(crate) asked: f64,
    /// The view's turns it has seen (Inner::turns): when that moves on, everything above is of another picture.
    turns: u64,
    /// Failed runs since the last good one.
    pub(crate) fails: u32,
}

/// A frame the mask thread hasn't got to: only the newest one waits.
struct MaskJob {
    seq: u64,
    size: (usize, usize),
    picture: Picture,
    /// When the frame was handed over.
    at: f64,
    /// The view's turns its picture was made under (Inner::turns when its step asked).
    epoch: u64,
}

/// The newest mask a frame step can get at once.
struct MadeMask {
    w: usize,
    h: usize,
    mask: Arc<Vec<u8>>,
    /// When its frame was handed over.
    at: f64,
    /// The same mask at other sizes asked for since, dropped with it when the next one comes.
    sizes: Vec<(usize, usize, Arc<Vec<u8>>)>,
    /// Which inference this is, so a size made outside the lock is only kept if it still belongs.
    generation: u64,
    /// The view's turns its picture was made under: only a frame made under the same count is handed it.
    epoch: u64,
}

impl MadeMask {
    /// Whether it still says where a person is in a picture of w x h: new enough, and of a picture the same shape.
    fn usable(&self, w: usize, h: usize) -> bool {
        let (ours, theirs) = ((self.w * h) as f64, (self.h * w) as f64);
        monotonic() - self.at <= MASK_MAX_AGE_S && (ours - theirs).abs() <= MASK_SHAPE_SLACK * ours.max(theirs)
    }
}

/// What a frame step and the mask thread pass each other, so neither waits for the other's work.
#[derive(Default)]
pub(crate) struct NewestMask {
    job: Option<MaskJob>,
    /// What was last asked for, so a second step on one frame doesn't ask again.
    asked: Option<u64>,
    mask: Option<MadeMask>,
    /// How many frames the thread has finished, with a mask or without.
    done: u64,
    /// The last wait for a mask ran out: frames don't wait again until the model answers.
    gave_up: bool,
    thread: bool,
}

/// Which model a mask thread runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Which {
    /// MediaPipe's selfie segmenter, the one everything uses.
    Normal,
    /// Robust Video Matting, for a frame step that asked for the high quality cutout.
    Cutout,
}

/// The high quality cutout's side of the service: its own download (found, loaded and failed on their own), none of it touching the normal
/// model's state.
pub(crate) struct Cutout {
    find: Find,
    there: Box<dyn Fn() -> bool + Send + Sync>,
    on_state: OnState,
    backend: Mutex<Option<Arc<dyn Backend>>>,
    state: Mutex<&'static str>,
    problem: Mutex<String>,
    looked_at: Mutex<f64>,
}

/// The stream key a stream's cutout masks go under, so they have a mask thread and a model of their own beside the normal ones.
pub(crate) fn cutout_key(key: &str) -> String {
    format!("{key}\u{1}cutout")
}

pub(crate) struct PeopleState {
    /// The newest small frame waiting for the pose model, and how to turn it upright.
    pub(crate) frame: Option<(Rgb, Upright)>,
    pub(crate) at: f64,
    pub(crate) motion: Motion,
    pub(crate) thread: Option<JoinHandle<()>>,
    /// Set: that people thread closes its model and ends.
    pub(crate) quit: Option<Arc<AtomicBool>>,
    pub(crate) wake: bool,
    pub(crate) fed: f64,
}

/// One stream's models and its people thread.
pub(crate) struct StreamVision {
    /// The model's state: the mask thread holds this while the model runs, a frame step never waits for it.
    pub(crate) mask: Mutex<MaskState>,
    pub(crate) newest: Mutex<NewestMask>,
    mask_wanted: Condvar,
    mask_done: Condvar,
    pub(crate) people: Mutex<PeopleState>,
    pub(crate) woken: Condvar,
    pub(crate) stopping: AtomicBool,
}

impl StreamVision {
    fn new() -> StreamVision {
        StreamVision {
            mask: Mutex::new(MaskState { segment: None, clock: Timestamps::new(), seq: None, mask: None, small: None, at: 0.0,
                                         motion: Motion::default(), asked: 0.0, turns: 0, fails: 0 }),
            newest: Mutex::default(),
            mask_wanted: Condvar::new(),
            mask_done: Condvar::new(),
            people: Mutex::new(PeopleState { frame: None, at: 0.0, motion: Motion::default(), thread: None, quit: None, wake: false, fed: 0.0 }),
            woken: Condvar::new(),
            stopping: AtomicBool::new(false),
        }
    }

    fn wake(&self) {
        self.people.lock().unwrap().wake = true;
        self.woken.notify_all();
        let _held = self.newest.lock().unwrap();
        self.mask_wanted.notify_all();
    }
}

pub(crate) struct Inner {
    on_people: OnPeople,
    on_state: OnState,
    find: Find,
    there: Box<dyn Fn() -> bool + Send + Sync>,
    pub(crate) backend: Mutex<Option<Arc<dyn Backend>>>,
    pub(crate) cutout: Mutex<Option<Arc<Cutout>>>,
    pub(crate) state: Mutex<&'static str>,
    pub(crate) problem: Mutex<String>,
    /// Held while load() decides; looked_at is when the last load() found no pack.
    pub(crate) looked_at: Mutex<f64>,
    pub(crate) streams: Mutex<HashMap<String, Arc<StreamVision>>>,
    people_wanted: Mutex<BTreeSet<String>>,
    ms: Mutex<HashMap<&'static str, Vec<f64>>>,
    tidier: Mutex<bool>,
    closed: AtomicBool,
    callback_lock: Mutex<()>,
    pub(crate) saving: AtomicBool,
    pub(crate) give_back: Mutex<Box<dyn Fn() + Send + Sync>>,
    /// Tests: frames for people wait in the slot instead of going to a thread.
    pub(crate) hold_people: AtomicBool,
    /// Tests with a real model: a frame step waits for its own mask.
    masks_wait: AtomicBool,
    /// How many times the view has turned (drop_masks), so a mask made for a picture from before one isn't taken for the new picture's.
    turns: AtomicU64,
}

/// The vision service's core, shared with its threads.
#[derive(Clone)]
pub struct Vision {
    pub(crate) inner: Arc<Inner>,
}

/// Linux: glibc keeps memory a closed model freed for later; hand it back to the system.
fn give_back_memory() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::malloc_trim(0);
    }
}

fn missing_library(message: &str) -> Option<String> {
    // ^(lib[\w.+-]+\.so[\w.]*): cannot open shared object file
    let (name, rest) = message.split_once(": cannot open shared object file")?;
    let ok = name.starts_with("lib") && name.contains(".so") && name.chars().all(|c| c.is_alphanumeric() || "_.+-".contains(c));
    let _ = rest;
    ok.then(|| name.to_string())
}

impl Vision {
    /// on_people: (stream, people) from a people thread; on_state: state() changed, from whichever thread changed it.
    /// there: whether a pack is there without loading anything (state() says MISSING before load() when it isn't).
    pub fn new(on_people: OnPeople, on_state: OnState, find: Find, there: Option<Box<dyn Fn() -> bool + Send + Sync>>) -> Vision {
        Vision { inner: Arc::new(Inner {
            on_people, on_state, find,
            there: there.unwrap_or_else(|| Box::new(|| true)),
            backend: Mutex::new(None),
            cutout: Mutex::new(None),
            state: Mutex::new(OFF),
            problem: Mutex::new(String::new()),
            looked_at: Mutex::new(-LOOK_AGAIN_S),
            streams: Mutex::new(HashMap::new()),
            people_wanted: Mutex::new(BTreeSet::new()),
            ms: Mutex::new(HashMap::new()),
            tidier: Mutex::new(false),
            closed: AtomicBool::new(false),
            callback_lock: Mutex::new(()),
            saving: AtomicBool::new(false),
            give_back: Mutex::new(Box::new(give_back_memory)),
            hold_people: AtomicBool::new(false),
            masks_wait: AtomicBool::new(false),
            turns: AtomicU64::new(0),
        }) }
    }

    // Loading

    /// OFF until something asks for it, then LOADING and READY; MISSING (without loading) when there's no pack.
    pub fn state(&self) -> &'static str {
        let state = *self.inner.state.lock().unwrap();
        if state == OFF && !(self.inner.there)() { MISSING } else { state }
    }

    /// What went wrong when it failed (the error's type; the log has the rest).
    pub fn problem(&self) -> String {
        self.inner.problem.lock().unwrap().clone()
    }

    /// Start loading in the background, once; after no pack was found, again every LOOK_AGAIN_S.
    pub fn load(&self) {
        {
            let mut looked_at = self.inner.looked_at.lock().unwrap();
            if self.inner.closed.load(Ordering::SeqCst) {
                return;
            }
            let state = *self.inner.state.lock().unwrap();
            if [LOADING, READY, FAILED].contains(&state) {
                return;
            }
            if state == MISSING && monotonic() - *looked_at < LOOK_AGAIN_S {
                return;
            }
            *looked_at = monotonic();
            self.inner.set_state(LOADING);
        }
        let inner = self.inner.clone();
        let _ = std::thread::Builder::new().name("vision-load".into()).spawn(move || inner.load_now());
    }

    /// The pack was just downloaded: after MISSING it reads OFF again, so the next ask loads it straight away.
    pub fn pack_downloaded(&self) {
        {
            let mut looked_at = self.inner.looked_at.lock().unwrap();
            if self.inner.closed.load(Ordering::SeqCst) || *self.inner.state.lock().unwrap() != MISSING {
                return;
            }
            *looked_at = -LOOK_AGAIN_S;
        }
        self.inner.set_state(OFF);
        if self.wants_people() {
            self.load();
        }
    }

    pub fn set_power_saving(&self, on: bool) {
        self.inner.saving.store(on, Ordering::SeqCst);
    }

    pub fn power_saving(&self) -> bool {
        self.inner.saving.load(Ordering::SeqCst)
    }

    fn ready(&self) -> Option<Arc<dyn Backend>> {
        if *self.inner.state.lock().unwrap() != READY {
            return None;
        }
        self.inner.backend.lock().unwrap().clone()
    }

    // The high quality cutout's model (matting)

    /// Gives the service a cutout: find loads it (Missing when the download isn't there), there says whether it is without loading anything and
    /// on_state hears each change of cutout_state().
    pub fn set_cutout(&self, find: Find, there: Option<Box<dyn Fn() -> bool + Send + Sync>>, on_state: OnState) {
        *self.inner.cutout.lock().unwrap() = Some(Arc::new(Cutout {
            find, on_state,
            there: there.unwrap_or_else(|| Box::new(|| true)),
            backend: Mutex::new(None),
            state: Mutex::new(OFF),
            problem: Mutex::new(String::new()),
            looked_at: Mutex::new(-LOOK_AGAIN_S),
        }));
    }

    pub(crate) fn cutout_slot(&self) -> Option<Arc<Cutout>> {
        self.inner.cutout.lock().unwrap().clone()
    }

    /// OFF until a frame step asks for the cutout, then LOADING and READY; MISSING (without loading) when it isn't downloaded; FAILED. A service
    /// with no cutout at all says MISSING.
    pub fn cutout_state(&self) -> &'static str {
        let Some(cutout) = self.cutout_slot() else { return MISSING };
        let state = *cutout.state.lock().unwrap();
        if state == OFF && !(cutout.there)() { MISSING } else { state }
    }

    /// What went wrong when the cutout failed (the error's type).
    pub fn cutout_problem(&self) -> String {
        self.cutout_slot().map(|c| c.problem.lock().unwrap().clone()).unwrap_or_default()
    }

    fn cutout_ready(&self) -> Option<Arc<dyn Backend>> {
        let cutout = self.cutout_slot()?;
        if *cutout.state.lock().unwrap() != READY {
            return None;
        }
        let backend = cutout.backend.lock().unwrap().clone();
        backend
    }

    /// Start loading the cutout in the background, once; after no download was found, again every LOOK_AGAIN_S.
    pub fn load_cutout(&self) {
        let Some(cutout) = self.cutout_slot() else { return };
        {
            let mut looked_at = cutout.looked_at.lock().unwrap();
            if self.inner.closed.load(Ordering::SeqCst) {
                return;
            }
            let state = *cutout.state.lock().unwrap();
            if [LOADING, READY, FAILED].contains(&state) {
                return;
            }
            if state == MISSING && monotonic() - *looked_at < LOOK_AGAIN_S {
                return;
            }
            *looked_at = monotonic();
            self.inner.set_cutout_state(&cutout, LOADING);
        }
        let inner = self.inner.clone();
        let _ = std::thread::Builder::new().name("matting-load".into()).spawn(move || inner.load_cutout_now(&cutout));
    }

    /// The cutout's download just finished: after MISSING it reads OFF again, so the next ask loads it straight away.
    pub fn cutout_downloaded(&self) {
        let Some(cutout) = self.cutout_slot() else { return };
        {
            let mut looked_at = cutout.looked_at.lock().unwrap();
            if self.inner.closed.load(Ordering::SeqCst) || *cutout.state.lock().unwrap() != MISSING {
                return;
            }
            *looked_at = -LOOK_AGAIN_S;
        }
        self.inner.set_cutout_state(&cutout, OFF);
    }

    // Person masks

    /// A frame step's way in: how sure the model was about each pixel of the newest frame it has done, at w x h, or None when there's
    /// nothing yet. Frame seq goes to the mask thread, and picture() runs there.
    pub fn newest_mask(&self, key: &str, seq: u64, w: usize, h: usize, picture: Picture) -> Option<Arc<Vec<u8>>> {
        self.newest_mask_in(key, seq, w, h, picture, self.view_epoch())
    }

    /// How many times the view has turned. A frame's picture is of the view it went through, so what asks for its mask says which count that was (newest_mask_in).
    pub fn view_epoch(&self) -> u64 {
        self.inner.turns.load(Ordering::SeqCst)
    }

    /// newest_mask for a picture made when the view had turned `epoch` times: once it has turned again, that picture's mask is of the wrong way round, so none is made or handed out for it.
    pub fn newest_mask_in(&self, key: &str, seq: u64, w: usize, h: usize, picture: Picture, epoch: u64) -> Option<Arc<Vec<u8>>> {
        self.newest_normal(key, true, seq, w, h, picture, epoch)
    }

    /// newest_mask_in that never waits for the first mask: what the high quality cutout falls back on, so a frame step is never held up.
    pub fn newest_mask_nowait_in(&self, key: &str, seq: u64, w: usize, h: usize, picture: Picture, epoch: u64) -> Option<Arc<Vec<u8>>> {
        self.newest_normal(key, false, seq, w, h, picture, epoch)
    }

    #[allow(clippy::too_many_arguments)]
    fn newest_normal(&self, key: &str, wait: bool, seq: u64, w: usize, h: usize, picture: Picture, epoch: u64) -> Option<Arc<Vec<u8>>> {
        if self.inner.masks_wait.load(Ordering::SeqCst) {
            let mut picture = Some(picture);
            return self.person_mask_in(key, seq, w, h, &mut || picture.take().and_then(|p| p()), epoch);
        }
        let Some(backend) = self.ready() else {
            self.load();
            return None;
        };
        self.newest_from(key, backend, Which::Normal, wait, seq, w, h, picture, epoch)
    }

    /// The high quality cutout's way in, the same as newest_mask from its own model and thread, and never waiting: None while it isn't
    /// downloaded, loading, failed, hasn't a mask yet or its newest is too old, and the caller then has the normal mask to use.
    pub fn newest_cutout(&self, key: &str, seq: u64, w: usize, h: usize, picture: Picture) -> Option<Arc<Vec<u8>>> {
        self.newest_cutout_in(key, seq, w, h, picture, self.view_epoch())
    }

    /// newest_cutout for a picture made when the view had turned `epoch` times (see newest_mask_in).
    pub fn newest_cutout_in(&self, key: &str, seq: u64, w: usize, h: usize, picture: Picture, epoch: u64) -> Option<Arc<Vec<u8>>> {
        let Some(backend) = self.cutout_ready() else {
            self.load_cutout();
            return None;
        };
        let key = cutout_key(key);
        if self.inner.masks_wait.load(Ordering::SeqCst) {
            let mut picture = Some(picture);
            let sv = self.stream(&key);
            let mut m = sv.mask.lock().unwrap();
            m.asked = monotonic();
            return self.mask(&mut m, &*backend, Which::Cutout, seq, w, h, &mut || picture.take().and_then(|p| p()), epoch);
        }
        self.newest_from(&key, backend, Which::Cutout, false, seq, w, h, picture, epoch)
    }

    #[allow(clippy::too_many_arguments)]
    fn newest_from(&self, key: &str, backend: Arc<dyn Backend>, which: Which, wait: bool, seq: u64, w: usize, h: usize, picture: Picture, epoch: u64)
                   -> Option<Arc<Vec<u8>>> {
        let sv = self.stream(key);
        let mut n = sv.newest.lock().unwrap();
        // Read under the lock drop_masks turns the count with
        if self.view_epoch() != epoch {
            return None;
        }
        if n.asked != Some(seq) {
            n.asked = Some(seq);
            n.job = Some(MaskJob { seq, size: (w, h), picture, at: monotonic(), epoch });
            if !n.thread {
                let (inner, sv2) = (self.inner.clone(), sv.clone());
                n.thread = std::thread::Builder::new().name("vision-mask".into()).spawn(move || mask_loop(inner, backend, sv2, which)).is_ok();
            }
            sv.mask_wanted.notify_all();
        }
        let usable = |n: &NewestMask| n.mask.as_ref().is_some_and(|m| m.epoch == epoch && m.usable(w, h));
        if wait && !usable(&n) && !n.gave_up {
            let (done, until) = (n.done, std::time::Instant::now() + std::time::Duration::from_secs_f64(FIRST_MASK_WAIT_S));
            while !usable(&n) && n.done == done {
                let Some(left) = until.checked_duration_since(std::time::Instant::now()) else {
                    n.gave_up = true;
                    break;
                };
                n = sv.mask_done.wait_timeout(n, left).unwrap().0;
            }
        }
        let Some(made) = n.mask.as_ref().filter(|m| m.epoch == epoch && m.usable(w, h)) else { return None };
        if (made.w, made.h) == (w, h) {
            return Some(made.mask.clone());
        }
        // One of another size until the thread has this size's: the picture changed size, or two steps look at different ones
        if let Some((_, _, m)) = made.sizes.iter().find(|(sw, sh, _)| (*sw, *sh) == (w, h)) {
            return Some(m.clone());
        }
        let (base, (bw, bh), generation) = (made.mask.clone(), (made.w, made.h), made.generation);
        drop(n);
        let resized = Arc::new(mask_resized(&base, bw, bh, w, h));
        let mut n = sv.newest.lock().unwrap();
        if let Some(made) = n.mask.as_mut().filter(|m| m.generation == generation) {
            match made.sizes.iter().find(|(sw, sh, _)| (*sw, *sh) == (w, h)) {
                Some((_, _, m)) => return Some(m.clone()),
                None => made.sizes.push((w, h, resized.clone())),
            }
        }
        Some(resized)
    }

    /// From now on a frame step waits for its own mask, as a test with a real model wants.
    pub fn wait_for_masks(&self) {
        self.inner.masks_wait.store(true, Ordering::SeqCst);
    }

    /// How sure the model is each pixel of frame `seq` (w x h) is a person, or None when it can't say yet. picture() gives the
    /// frame as RGB and is only called when the model needs it.
    pub fn person_mask(&self, key: &str, seq: u64, w: usize, h: usize, picture: &mut dyn FnMut() -> Option<Rgb>) -> Option<Arc<Vec<u8>>> {
        self.person_mask_in(key, seq, w, h, picture, self.view_epoch())
    }

    fn person_mask_in(&self, key: &str, seq: u64, w: usize, h: usize, picture: &mut dyn FnMut() -> Option<Rgb>, epoch: u64) -> Option<Arc<Vec<u8>>> {
        let Some(backend) = self.ready() else {
            self.load();
            return None;
        };
        let sv = self.stream(key);
        let mut m = sv.mask.lock().unwrap();
        m.asked = monotonic();
        self.mask(&mut m, &*backend, Which::Normal, seq, w, h, picture, epoch)
    }

    #[allow(clippy::too_many_arguments)]
    fn mask(&self, m: &mut MaskState, backend: &dyn Backend, which: Which, seq: u64, w: usize, h: usize, picture: &mut dyn FnMut() -> Option<Rgb>, epoch: u64)
            -> Option<Arc<Vec<u8>>> {
        // A picture from before a turn of the view leaves what's kept alone: it's of the old way round, and what it would blend into the next mask is too
        if self.view_epoch() != epoch {
            return None;
        }
        if m.turns != epoch {
            // The view turned since what was kept was made: it, and what the next mask would blend in, is of the old picture
            (m.turns, m.seq, m.mask, m.small, m.motion) = (epoch, None, None, None, Motion::default());
            // A model that keeps its own memory of the last frames (the cutout's) forgets them too
            if let Some(segment) = m.segment.as_mut() {
                segment.reset();
            }
        }
        if let (Some(at), Some((mw, mh, mask))) = (m.seq, &m.mask) {
            if at == seq && (*mw, *mh) == (w, h) {
                return Some(mask.clone());
            }
        }
        if m.segment.is_none() {
            match backend.segmenter() {
                Ok(s) => m.segment = Some(s),
                Err(e) => {
                    log::error!("Couldn't make the person segmenter: {}: {}", e.kind, e.message);
                    self.inner.fail_model(which, &e);
                    return None;
                }
            }
        }
        let frame = picture()?;
        let small = m.segment.as_ref().and_then(|s| s.input(&frame)).unwrap_or(frame);
        let now = monotonic();
        // Nothing moving: the last mask still fits, so the model runs less often (anything moving brings it back)
        let moved = m.motion.moved(&small);
        let saving = self.power_saving();
        if (moved && !saving) || m.small.is_none() || now - m.at >= MASK_STILL_EVERY_S {
            let t0 = std::time::Instant::now();
            let ts = m.clock.next();
            let gap = m.small.is_some() && now - m.at > MASK_MAX_AGE_S;
            let resized = m.small.as_ref().is_some_and(|s| (s.w, s.h) != (small.w, small.h));
            let segment = m.segment.as_mut().unwrap();
            // After a long gap, or at another size, the model's memory is of another picture (the camera may have been switched meanwhile)
            if gap || resized {
                segment.reset();
            }
            let mut mask = match segment.segment(&small, ts) {
                Ok(mask) => mask,
                Err(e) => {
                    log::warn!("The person segmenter failed: {}: {}", e.kind, e.message);
                    m.fails += 1;
                    if which == Which::Cutout && m.fails >= CUTOUT_MAX_FAILS {
                        self.inner.fail_model(which, &e);
                    }
                    return None;
                }
            };
            m.fails = 0;
            // After a long gap the last mask is of another moment
            if let Some(last) = m.small.as_ref().filter(|s| (s.w, s.h) == (mask.w, mask.h) && now - m.at <= MASK_MAX_AGE_S) {
                if m.segment.as_ref().is_some_and(|s| s.blends_last()) {
                    let carry = if moved { MASK_CARRY } else { MASK_CARRY_STILL };
                    mask.data = add_weighted(&mask.data, 1.0 - carry, &last.data, carry);
                }
            }
            self.note(if which == Which::Cutout { "cutout" } else { "mask" }, t0);
            m.motion.used();
            m.small = Some(mask);
            m.at = now;
            m.mask = None;
        }
        let small = m.small.as_ref().unwrap();
        if !matches!(&m.mask, Some((mw, mh, _)) if (*mw, *mh) == (w, h)) {
            let full = if (small.w, small.h) == (w, h) { small.data.clone() } else { mask_resized(&small.data, small.w, small.h, w, h) };
            m.mask = Some((w, h, Arc::new(full)));
        }
        m.seq = Some(seq);
        m.mask.as_ref().map(|(_, _, mask)| mask.clone())
    }

    // People (a thread per stream)

    pub fn want_people(&self, owner: &str, on: bool) {
        if on {
            self.inner.people_wanted.lock().unwrap().insert(owner.to_string());
            self.load();
        } else {
            self.inner.people_wanted.lock().unwrap().remove(owner);
        }
    }

    pub fn wants_people(&self) -> bool {
        !self.inner.people_wanted.lock().unwrap().is_empty()
    }

    /// A stream's frame before the view; one every PEOPLE_EVERY_S goes to its people thread while something moves, one every
    /// PEOPLE_STILL_EVERY_S while nothing does, and the rest are skipped. upright turns the small copy for the model (on that
    /// thread) and takes its points to the picture as shown at zoom 1.
    pub fn feed_raw(&self, key: &str, picture: &mut dyn FnMut() -> Option<Rgb>, upright: Upright) {
        if !self.wants_people() {
            return;
        }
        if self.ready().is_none() {
            self.load();
            return;
        }
        let sv = self.stream(key);
        let now = monotonic();
        let mut p = sv.people.lock().unwrap();
        p.fed = now;
        let (every, still_every) = if self.power_saving() { (SAVING_PEOPLE_EVERY_S, SAVING_PEOPLE_STILL_EVERY_S) } else { (PEOPLE_EVERY_S, PEOPLE_STILL_EVERY_S) };
        if now - p.at < every {
            return;
        }
        let Some(frame) = picture() else { return };
        if !p.motion.moved(&frame) && now - p.at < still_every {
            return;
        }
        p.motion.used();
        p.at = now;
        let (w, h) = people_size(frame.w, frame.h, PEOPLE_WIDTH);
        p.frame = Some((if (frame.w, frame.h) == (w, h) { frame } else { frame.area(w, h) }, upright));
        if p.thread.is_none() && !self.inner.hold_people.load(Ordering::SeqCst) {
            let quit = Arc::new(AtomicBool::new(false));
            p.quit = Some(quit.clone());
            let (inner, key, sv2) = (self.inner.clone(), key.to_string(), sv.clone());
            p.thread = std::thread::Builder::new().name("vision-people".into()).spawn(move || people_loop(inner, key, sv2, quit)).ok();
        }
        p.wake = true;
        drop(p);
        sv.woken.notify_all();
    }

    // Streams

    pub(crate) fn stream(&self, key: &str) -> Arc<StreamVision> {
        let mut streams = self.inner.streams.lock().unwrap();
        if let Some(sv) = streams.get(key) {
            return sv.clone();
        }
        let sv = Arc::new(StreamVision::new());
        streams.insert(key.to_string(), sv.clone());
        drop(streams);
        let mut tidier = self.inner.tidier.lock().unwrap();
        if !*tidier {
            *tidier = true;
            let weak = Arc::downgrade(&self.inner);
            let _ = std::thread::Builder::new().name("vision-tidy".into()).spawn(move || tidy_loop(weak));
        }
        sv
    }

    /// Close the models no plugin has asked for in IDLE_CLOSE_S, and give their memory back. The next ask makes them again.
    pub fn tidy(&self, now: Option<f64>) {
        let now = now.unwrap_or_else(monotonic);
        let mut freed = false;
        let streams: Vec<Arc<StreamVision>> = self.inner.streams.lock().unwrap().values().cloned().collect();
        for sv in streams {
            {
                let mut m = sv.mask.lock().unwrap();
                if m.segment.is_some() && now - m.asked >= IDLE_CLOSE_S {
                    m.segment = None;
                    m.mask = None;
                    m.small = None;
                    m.seq = None;
                    m.motion = Motion::default();
                    let mut n = sv.newest.lock().unwrap();
                    n.mask = None;
                    n.gave_up = false;
                    freed = true;
                }
            }
            let mut p = sv.people.lock().unwrap();
            if p.thread.is_some() && now - p.fed >= IDLE_CLOSE_S {
                // the thread closes the pose model and gives its memory back on the way out
                if let Some(quit) = &p.quit {
                    quit.store(true, Ordering::SeqCst);
                }
                p.wake = true;
                p.thread = None;
                p.frame = None;
                p.motion = Motion::default();
                p.at = 0.0;
                drop(p);
                sv.woken.notify_all();
            }
        }
        if freed {
            (self.inner.give_back.lock().unwrap())();
        }
    }

    /// The view turned (flipped or rotated): a mask kept for a stream, or one being made, says where people were in the picture as it was. None of them is handed out again and none is blended into the next, so a frame step gets nothing until the model has looked at the turned picture, and a step that hides things stays safe on that.
    pub fn drop_masks(&self) {
        // Once, before any stream's: a stream with nothing yet still has its first picture asked about under the new count
        self.inner.turns.fetch_add(1, Ordering::SeqCst);
        let streams: Vec<Arc<StreamVision>> = self.inner.streams.lock().unwrap().values().cloned().collect();
        for sv in streams {
            let mut n = sv.newest.lock().unwrap();
            (n.job, n.asked, n.mask, n.gave_up) = (None, None, None, false);
            sv.mask_done.notify_all();
        }
    }

    /// A stream ended: its tasks close and its people thread ends.
    pub fn drop_stream(&self, key: &str) {
        // The cutout's models of the stream go with it, so a stream that starts again begins with fresh memory
        for key in [key.to_string(), cutout_key(key)] {
            let Some(sv) = self.inner.streams.lock().unwrap().remove(&key) else { continue };
            sv.stopping.store(true, Ordering::SeqCst);
            sv.wake();
            // A mask being made holds the lock for as long as the model takes: its thread closes the model when it's done (see mask_loop)
            if let Ok(mut m) = sv.mask.try_lock() {
                m.segment = None;
            };
        }
    }

    /// No callback reaches on_people or on_state once this returns.
    pub fn shutdown(&self) {
        {
            let _held = self.inner.callback_lock.lock().unwrap();
            self.inner.closed.store(true, Ordering::SeqCst);
        }
        let keys: Vec<String> = self.inner.streams.lock().unwrap().keys().cloned().collect();
        for key in keys {
            self.drop_stream(&key);
        }
    }

    // Diagnostics

    fn note(&self, kind: &'static str, t0: std::time::Instant) {
        let mut ms = self.inner.ms.lock().unwrap();
        let recent = ms.entry(kind).or_default();
        recent.push(t0.elapsed().as_secs_f64() * 1000.0);
        let extra = recent.len().saturating_sub(60);
        recent.drain(..extra);
    }

    /// {"Vision": "..."} for Copy diagnostics; empty while OFF.
    pub fn diagnostics(&self) -> Vec<(String, String)> {
        let state = *self.inner.state.lock().unwrap();
        if state == OFF {
            return Vec::new();
        }
        let version = self.inner.backend.lock().unwrap().as_ref().map_or("?".into(), |b| b.version());
        let mut line = match state {
            READY => format!("ready (MediaPipe {version})"),
            LOADING => "loading".into(),
            MISSING => "not downloaded".into(),
            _ => format!("didn't load ({})", self.problem()),
        };
        let ms = self.inner.ms.lock().unwrap();
        for kind in ["mask", "people"] {
            if let Some(recent) = ms.get(kind).filter(|r| !r.is_empty()) {
                let mut sorted = recent.clone();
                sorted.sort_by(f64::total_cmp);
                line += &format!(", {kind} {:.1} ms", sorted[sorted.len() / 2]);
            }
        }
        if self.power_saving() {
            line += ", power saving";
        }
        let mut lines = vec![("Vision".into(), line)];
        if let Some(cutout) = self.cutout_slot().filter(|_| self.cutout_state() != OFF) {
            let version = cutout.backend.lock().unwrap().as_ref().map_or("?".into(), |b| b.version());
            let mut line = match self.cutout_state() {
                READY => format!("ready ({version})"),
                LOADING => "loading".into(),
                MISSING => "not downloaded".into(),
                _ => format!("didn't load ({})", cutout.problem.lock().unwrap()),
            };
            if let Some(recent) = ms.get("cutout").filter(|r| !r.is_empty()) {
                let mut sorted = recent.clone();
                sorted.sort_by(f64::total_cmp);
                line += &format!(", mask {:.1} ms", sorted[sorted.len() / 2]);
            }
            lines.push(("High quality cutout".into(), line));
        }
        lines
    }
}

impl Inner {
    fn set_state(&self, state: &'static str) {
        let _held = self.callback_lock.lock().unwrap();
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        *self.state.lock().unwrap() = state;
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.on_state)(state))).is_err() {
            log::error!("Vision state callback failed");
        }
    }

    fn fail(&self, e: &VisionError) {
        // Only the type and a missing library's name: the message can hold a path with the user's name in it
        let missing = if e.kind == "OSError" { missing_library(&e.message) } else { None };
        *self.problem.lock().unwrap() = match missing {
            Some(lib) => format!("{}: {lib} missing", e.kind),
            None => e.kind.clone(),
        };
        self.set_state(FAILED);
    }

    /// A model that can't run: the normal one says so for the whole service, the cutout only for itself.
    fn fail_model(&self, which: Which, e: &VisionError) {
        match which {
            Which::Normal => self.fail(e),
            Which::Cutout => {
                let Some(cutout) = self.cutout.lock().unwrap().clone() else { return };
                // The same as fail(): the type and a missing library's name, never a message with a path in it
                let missing = if e.kind == "OSError" { missing_library(&e.message) } else { None };
                *cutout.problem.lock().unwrap() = match missing {
                    Some(lib) => format!("{}: {lib} missing", e.kind),
                    None => e.kind.clone(),
                };
                self.set_cutout_state(&cutout, FAILED);
            }
        }
    }

    fn set_cutout_state(&self, cutout: &Cutout, state: &'static str) {
        let _held = self.callback_lock.lock().unwrap();
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        *cutout.state.lock().unwrap() = state;
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (cutout.on_state)(state))).is_err() {
            log::error!("Cutout state callback failed");
        }
    }

    fn load_cutout_now(&self, cutout: &Cutout) {
        match (cutout.find)() {
            Ok(backend) => {
                log::info!("High quality cutout ({}) loaded", backend.version());
                *cutout.backend.lock().unwrap() = Some(backend);
                self.set_cutout_state(cutout, READY);
            }
            Err(LoadError::Missing(why)) => {
                log::info!("The high quality cutout isn't there: {why}");
                cutout.problem.lock().unwrap().clear();
                self.set_cutout_state(cutout, MISSING);
            }
            Err(LoadError::Failed(e)) => {
                log::error!("The high quality cutout didn't load: {}: {}", e.kind, e.message);
                self.fail_model(Which::Cutout, &e);
            }
        }
    }

    fn load_now(&self) {
        match (self.find)() {
            Ok(backend) => {
                log::info!("MediaPipe {} loaded", backend.version());
                *self.backend.lock().unwrap() = Some(backend);
                self.set_state(READY);
            }
            Err(LoadError::Missing(why)) => {
                log::info!("Vision isn't there: {why}");
                self.problem.lock().unwrap().clear();
                self.set_state(MISSING);
            }
            Err(LoadError::Failed(e)) => {
                log::error!("MediaPipe didn't load: {}: {}", e.kind, e.message);
                self.fail(&e);
            }
        }
    }
}

fn tidy_loop(inner: Weak<Inner>) {
    let step = std::time::Duration::from_millis(100);
    let mut waited = 0.0;
    loop {
        std::thread::sleep(step);
        waited += step.as_secs_f64();
        let Some(strong) = inner.upgrade() else { return };
        if strong.closed.load(Ordering::SeqCst) {
            return;
        }
        if waited >= TIDY_EVERY_S {
            waited = 0.0;
            Vision { inner: strong }.tidy(None);
        }
    }
}

/// A stream's masks: only ever the newest frame waiting, so when the model falls behind it skips frames instead of queueing them.
fn mask_loop(inner: Arc<Inner>, backend: Arc<dyn Backend>, sv: Arc<StreamVision>, which: Which) {
    let vision = Vision { inner };
    loop {
        let job = {
            let mut n = sv.newest.lock().unwrap();
            loop {
                if sv.stopping.load(Ordering::SeqCst) {
                    n.thread = false;
                    return;
                }
                match n.job.take() {
                    Some(job) => break job,
                    None => n = sv.mask_wanted.wait(n).unwrap(),
                }
            }
        };
        let MaskJob { seq, size: (w, h), picture, at, epoch } = job;
        let mut picture = Some(picture);
        let mut m = sv.mask.lock().unwrap();
        m.asked = monotonic();
        let mask = vision.mask(&mut m, &*backend, which, seq, w, h, &mut || picture.take().and_then(|p| p()), epoch);
        if sv.stopping.load(Ordering::SeqCst) {
            m.segment = None;
        }
        drop(m);
        let mut n = sv.newest.lock().unwrap();
        n.done += 1;
        // Without one the last stays until it's too old: a failed frame now and then doesn't show. One of a picture from before a turn of the view is dropped, whenever the turn came.
        if let Some(mask) = mask.filter(|_| vision.inner.turns.load(Ordering::SeqCst) == epoch) {
            n.mask = Some(MadeMask { w, h, mask, at, sizes: Vec::new(), generation: n.done, epoch });
            n.gave_up = false;
        }
        sv.mask_done.notify_all();
    }
}

fn people_loop(inner: Arc<Inner>, key: String, sv: Arc<StreamVision>, quit: Arc<AtomicBool>) {
    let Some(backend) = inner.backend.lock().unwrap().clone() else { return };
    let mut model = match backend.pose() {
        Ok(model) => model,
        Err(e) => {
            log::error!("Couldn't make the pose model: {}: {}", e.kind, e.message);
            inner.fail(&e);
            return;
        }
    };
    let mut clock = Timestamps::new();
    let done = || sv.stopping.load(Ordering::SeqCst) || quit.load(Ordering::SeqCst);
    while !done() {
        let waiting = {
            let mut p = sv.people.lock().unwrap();
            while !p.wake {
                p = sv.woken.wait(p).unwrap();
            }
            p.wake = false;
            if done() {
                break;
            }
            p.frame.take()
        };
        let Some((frame, upright)) = waiting else { continue };
        let t0 = std::time::Instant::now();
        let prepared = if upright.turn == Default::default() { frame } else { turned(&frame, upright.turn) };
        let people = match model.detect(&prepared, clock.next()) {
            Ok(people) => people.iter().map(|p| p.mapped(&|x, y| upright.to_shown(x, y))).collect::<Vec<Person>>(),
            Err(e) => {
                log::warn!("Looking for people failed; trying the next frame: {}: {}", e.kind, e.message);
                continue;
            }
        };
        Vision { inner: inner.clone() }.note("people", t0);
        let _held = inner.callback_lock.lock().unwrap();
        if !inner.closed.load(Ordering::SeqCst) {
            (inner.on_people)(&key, people);
        }
    }
    drop(model);
    if quit.load(Ordering::SeqCst) {
        (inner.give_back.lock().unwrap())();
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "cutout_tests.rs"]
mod cutout_tests;
