//! Person masks and people's positions, once per stream, for the plugins that ask. MediaPipe loads on first
//! use; every call is safe before it's ready (None, nothing). A frame step gets the newest mask at once and leaves its frame for a
//! thread of the stream's own, so the model and the work around it never hold a frame up, and a mask is a frame or so behind the
//! picture and never over MASK_MAX_AGE_S behind (past that a step gets None). People come from another such thread, about 12 times a second
//! while something moves and 3 while nothing does. The high quality cutout (matting) is a second model behind the same kind of mask thread, on
//! a stream key of its own: it only ever stands in for the normal mask when it has one in time, and the normal one is what a frame step
//! gets while it's missing, loading, failed or late. While the effects are lighter and every owner only needs heads (want_heads), people are
//! followed by their faces (heads::Heads), with the pose model checking once a second and taking over whenever faces don't fit.

use crate::heads::Heads;
use crate::ops::{add_weighted, firm_mask, mask_resized, model_input, people_size, turned, Motion, Rgb, MODEL_SIZE};
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
use telescope_core::effort::{Effort, MOST};
use telescope_core::pose::Person;
use telescope_core::frame::MaskSpace;
use telescope_core::view::{shown_to_view, view_to_shown, Geometry};
use telescope_core::vision::{Turn, Upright, FAILED, LOADING, MISSING, OFF, READY};

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
/// The high quality cutout costs about ten times the normal model a run, so while nothing moves it is worked out this often.
pub const CUTOUT_STILL_EVERY_S: f64 = 0.25;

/// Frames arrive a few ms off their beat, so 3 frames at 30 fps (exactly 0.1 s) count as due instead of waiting for a 4th. Every gap in CADENCE past level 0 is this much shorter than it says.
pub const SAVING_MASK_JITTER_S: f64 = 0.008;

/// The least time between two runs of a model, while something moves and while nothing does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gaps {
    pub moving: f64,
    pub still: f64,
}

/// How often the models run at one effort level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cadence {
    pub mask: Gaps,
    /// A big move (ops::BIG_MOVE_SHARE) runs the mask after this long instead, so standing up or waving doesn't leave the room sharp around you; None: it waits like any other move.
    pub big_move: Option<f64>,
    /// The high quality cutout; None: it doesn't run, and the normal mask stands in.
    pub cutout: Option<Gaps>,
    pub people: Gaps,
    /// How often people are looked for while they are followed by their faces; None: never by faces, the pose model does it all.
    pub faces: Option<Gaps>,
}

const fn gaps(moving: f64, still: f64) -> Gaps {
    Gaps { moving, still }
}

/// A gap as the clock sees it, a jitter early (see SAVING_MASK_JITTER_S).
const fn early(gap: f64) -> f64 {
    gap - SAVING_MASK_JITTER_S
}

/// The cadence of each effort level (core::effort), which only says when the models run and changes nothing about what they work out.
pub const CADENCE: [Cadence; MOST as usize + 1] = [
    // 0: full, a mask for every frame that moves, people about 12 times a second
    Cadence { mask: gaps(0.0, MASK_STILL_EVERY_S), big_move: Some(0.0), cutout: Some(gaps(0.0, CUTOUT_STILL_EVERY_S)), people: gaps(PEOPLE_EVERY_S, PEOPLE_STILL_EVERY_S), faces: None },
    // 1: what power-saving effects does, the mask 10 times a second (a big move sooner, not within 25 ms of the last), the cutout 4, people 6 or faces 12 (a face costs a tenth of a pose)
    Cadence { mask: gaps(early(0.1), early(0.1)), big_move: Some(0.025), cutout: Some(gaps(CUTOUT_STILL_EVERY_S, CUTOUT_STILL_EVERY_S)), people: gaps(0.16, 0.5), faces: Some(gaps(PEOPLE_EVERY_S, PEOPLE_STILL_EVERY_S)) },
    // 2: the mask 6 times a second (a big move not within 0.1 s of the last), the cutout 2, people 4 or faces 8
    Cadence { mask: gaps(early(0.15), early(0.15)), big_move: Some(early(0.1)), cutout: Some(gaps(0.5, 0.5)), people: gaps(0.25, 0.66), faces: Some(gaps(0.125, 0.5)) },
    // 3: the mask 5 times a second whatever moves, no cutout, people 3 or faces 5
    Cadence { mask: gaps(early(0.2), early(0.2)), big_move: None, cutout: None, people: gaps(0.33, 1.0), faces: Some(gaps(0.2, 0.66)) },
];

/// The cadence for a level, which a level past the lightest has as the lightest.
pub fn cadence(level: u8) -> Cadence {
    CADENCE[(level as usize).min(CADENCE.len() - 1)]
}
/// How long a frame waits for its mask when there's none it can use, so the picture doesn't go out without one when a stream starts.
pub const FIRST_MASK_WAIT_S: f64 = 0.5;
/// A mask of a frame further back than this is of a picture that's gone: frame steps hear there's none, not an old silhouette.
pub const MASK_MAX_AGE_S: f64 = 0.5;
/// A high quality cutout mask this much older than the normal one is behind it: the normal one stands in, however recent the cutout still is.
pub const HQ_BEHIND_S: f64 = 0.15;
/// How far apart two sizes' shapes can be for a mask of one to stand in for the other, as a share.
const MASK_SHAPE_SLACK: f64 = 0.02;
/// A crop that moves no more than this share of the picture between two masks keeps the model's memory and the last mask's carry.
const CARRY_JUMP: f64 = 0.15;
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

/// A face the face finder saw, in 0..1 of the picture it was given: its box and the head points the pose model names the same way.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    /// (x0, y0, x1, y1)
    pub area: (f64, f64, f64, f64),
    pub right_eye: (f64, f64),
    pub left_eye: (f64, f64),
    pub nose: (f64, f64),
    pub right_ear: (f64, f64),
    pub left_ear: (f64, f64),
    pub score: f64,
}

/// The face finder (one stream's). Closed when dropped.
pub trait FaceModel: Send {
    fn detect(&mut self, frame: &Rgb, ts_ms: i64) -> Result<Vec<Face>, VisionError>;
}

/// Makes the models (MediaPipeBackend).
pub trait Backend: Send + Sync {
    fn version(&self) -> String;
    fn segmenter(&self) -> Result<Box<dyn Segmenter>, VisionError>;
    fn pose(&self) -> Result<Box<dyn PoseModel>, VisionError>;
    /// The face finder; an error where the pack has none (one from before it), and the pose model does it all.
    fn faces(&self) -> Result<Box<dyn FaceModel>, VisionError> {
        Err(VisionError::new("NotImplementedError", "no face finder"))
    }
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
    /// When the frame the model last ran on was taken up, which is before `at` when the model had to open first.
    pub(crate) frame_at: f64,
    /// When a frame last looked the same as the one `small` is of, so the mask held over a still scene is no older than that.
    pub(crate) seen: f64,
    /// `seen` as the camera frame's own time, so a worker that starts late on an old frame doesn't make it look newer.
    pub(crate) seen_frame: f64,
    /// The frame the model last ran on, which `small` is of: a frame that got `small` again without a run is not it.
    pub(crate) ran: Option<u64>,
    pub(crate) motion: Motion,
    pub(crate) asked: f64,
    /// The view's turns it has seen (Inner::turns): when that moves on, everything above is of another picture.
    turns: u64,
    /// Failed runs since the last good one.
    pub(crate) fails: u32,
    geometry: Option<Geometry>,
    space: MaskSpace,
    /// The crop `geometry` was a guess: what the model remembers isn't moved along with a guessed crop.
    unsure: bool,
}

impl MaskState {
    fn geometry(&mut self, geometry: Option<Geometry>, space: MaskSpace, unsure: bool) {
        let was_unsure = std::mem::replace(&mut self.unsure, unsure);
        if self.geometry == geometry && self.space == space {
            return;
        }
        // A small move of the crop keeps what the model remembers, moved with it: dropping it on every step of a moving crop leaves every mask unsteadied
        let carried = match (self.geometry, geometry) {
            (Some(old), Some(new)) if !unsure && !was_unsure && self.space == space && compatible_geometry(Some(old), Some(new)) && crop_jump(&old, &new) <= CARRY_JUMP => Some((old, new)),
            _ => None,
        };
        self.geometry = geometry;
        self.space = space;
        (self.seq, self.mask) = (None, None);
        match (carried, self.small.take()) {
            (Some((old, new)), Some(small)) => {
                let data = remap_mask(&small.data, small.w, small.h, small.w, small.h, &old, &new);
                self.small = Some(Gray { w: small.w, h: small.h, data });
            }
            _ => {
                (self.small, self.motion) = (None, Motion::default());
                if let Some(segment) = self.segment.as_mut() {
                    segment.reset();
                }
            }
        }
    }
}

/// How far, as a share of the picture, the corners and middle of a frame shown with `new` are from where `old` had them.
fn crop_jump(old: &Geometry, new: &Geometry) -> f64 {
    let mut worst: f64 = 0.0;
    for (u, v) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.5)] {
        let (x, y) = shown_to_view(u, v, new);
        let (u2, v2) = view_to_shown(x, y, old);
        worst = worst.max((u2 - u).abs()).max((v2 - v).abs());
    }
    worst
}

#[derive(Clone, Copy, Default)]
pub(crate) struct MaskView {
    pub(crate) geometry: Option<Geometry>,
    pub(crate) keep_late: bool,
    pub(crate) space: MaskSpace,
    /// The crop in `geometry` is a guess (MaskSpace::crop_unsure): a mask that isn't recent is as wrong as the guess is.
    pub(crate) unsure: bool,
}

pub(crate) struct MaskSnapshot {
    pub(crate) mask: Arc<Vec<u8>>,
    pub(crate) at: f64,
    /// When its frame was taken up, for telling which of two models' masks is of the newer frame: `at` can be later by a slow model open.
    pub(crate) frame_at: f64,
    /// The frame it was made for: two masks of one frame are equally recent whichever was asked for first.
    pub(crate) seq: u64,
}

impl MaskSnapshot {
    pub(crate) fn fresh(&self) -> bool {
        monotonic() - self.at <= MASK_MAX_AGE_S
    }

    /// How far behind the picture it is, seconds.
    pub(crate) fn age(&self) -> f64 {
        monotonic() - self.at
    }
}

/// Makes a tiny grey copy of a frame for cheap motion checks, called only when the mask may skip decoding colour pixels.
pub type Look = Box<dyn FnOnce() -> Option<Vec<u8>> + Send>;

/// A frame the mask thread hasn't got to: only the newest one waits.
struct MaskJob {
    geometry: Option<Geometry>,
    space: MaskSpace,
    seq: u64,
    size: (usize, usize),
    look: Option<Look>,
    picture: Picture,
    /// The view's turns its picture was made under (Inner::turns when its step asked).
    epoch: u64,
    unsure: bool,
    /// When the frame step asked, the frame's own time whichever worker gets to it when.
    taken: f64,
}

/// The newest mask a frame step can get at once.
#[derive(Clone)]
struct MadeMask {
    geometry: Option<Geometry>,
    space: MaskSpace,
    w: usize,
    h: usize,
    mask: Arc<Vec<u8>>,
    /// When the model ran, which for a mask got again without a run is the run it came from.
    at: f64,
    frame_at: f64,
    seq: u64,
    /// The same mask at other sizes asked for since, dropped with it when the next one comes.
    sizes: Vec<MaskSize>,
    /// Which inference this is, so a size made outside the lock is only kept if it still belongs.
    generation: u64,
    /// The view's turns its picture was made under: only a frame made under the same count is handed it.
    epoch: u64,
}

#[derive(Clone)]
struct MaskSize {
    size: (usize, usize),
    geometry: Option<Geometry>,
    mask: Arc<Vec<u8>>,
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
    background: Option<MadeMask>,
    /// How many frames the thread has finished, with a mask or without.
    done: u64,
    /// The last wait for a mask ran out: frames don't wait again until the model answers.
    gave_up: bool,
    thread: bool,
}

impl NewestMask {
    fn for_view(&self, view: MaskView) -> Option<&MadeMask> {
        if view.keep_late { self.background.as_ref() } else { self.mask.as_ref() }
    }
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
    /// When a frame was last looked at to see whether it moved, so a still scene is looked at once per cadence, not once per frame.
    pub(crate) probed: f64,
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
    /// While people are followed by their faces: the pose model is doing the work (a group, no face, the face finder failing), so frames come at its pace and not the face finder's.
    pub(crate) posing: AtomicBool,
}

impl StreamVision {
    fn new() -> StreamVision {
        StreamVision {
            mask: Mutex::new(MaskState { segment: None, clock: Timestamps::new(), seq: None, mask: None, small: None, at: 0.0, frame_at: 0.0, seen: 0.0, seen_frame: 0.0, ran: None,
                                         motion: Motion::default(), asked: 0.0, turns: 0, fails: 0, geometry: None, space: MaskSpace::default(), unsure: false }),
            newest: Mutex::default(),
            mask_wanted: Condvar::new(),
            mask_done: Condvar::new(),
            people: Mutex::new(PeopleState { frame: None, at: 0.0, probed: 0.0, motion: Motion::default(), thread: None, quit: None, wake: false, fed: 0.0 }),
            woken: Condvar::new(),
            stopping: AtomicBool::new(false),
            posing: AtomicBool::new(false),
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
    /// The owners in people_wanted that only need heads (want_heads).
    heads_only: Mutex<BTreeSet<String>>,
    /// The pack loaded has no face finder, so people are always followed whole; looked at again when a pack is downloaded.
    pub(crate) faceless: AtomicBool,
    ms: Mutex<HashMap<&'static str, Vec<f64>>>,
    tidier: Mutex<Option<Sender<()>>>,
    closed: AtomicBool,
    callback_lock: Mutex<()>,
    /// How hard the models work; power-saving effects is its floor.
    pub(crate) effort: Arc<Effort>,
    pub(crate) give_back: Mutex<Box<dyn Fn() + Send + Sync>>,
    /// Tests: frames for people wait in the slot instead of going to a thread.
    pub(crate) hold_people: AtomicBool,
    /// Tests with a real model: a frame step waits for its own mask.
    masks_wait: AtomicBool,
    /// Tests: the clock's bits while it is faked (see Vision::fake_clock), 0 for the real one.
    fake_now: AtomicU64,
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
        Vision::with_effort(Effort::shared(), on_people, on_state, find, there)
    }

    /// The same, reading how hard to work from effort instead of the app's own.
    pub fn with_effort(effort: Arc<Effort>, on_people: OnPeople, on_state: OnState, find: Find, there: Option<Box<dyn Fn() -> bool + Send + Sync>>) -> Vision {
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
            heads_only: Mutex::new(BTreeSet::new()),
            faceless: AtomicBool::new(false),
            ms: Mutex::new(HashMap::new()),
            tidier: Mutex::new(None),
            closed: AtomicBool::new(false),
            callback_lock: Mutex::new(()),
            effort,
            give_back: Mutex::new(Box::new(give_back_memory)),
            hold_people: AtomicBool::new(false),
            masks_wait: AtomicBool::new(false),
            fake_now: AtomicU64::new(0),
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

    /// Power-saving effects: level 1 at least, whatever the automatic part says.
    pub fn set_power_saving(&self, on: bool) {
        self.inner.effort.set_floor(u8::from(on));
    }

    /// Seconds on the clock the service goes by: the monotonic one, or the faked one tests move by hand.
    fn now(&self) -> f64 {
        match self.inner.fake_now.load(Ordering::SeqCst) {
            0 => monotonic(),
            bits => f64::from_bits(bits),
        }
    }

    /// Tests: from now on time only moves when `advance_fake_clock` says so.
    #[cfg(test)]
    pub(crate) fn fake_clock(&self) {
        self.inner.fake_now.store(monotonic().to_bits(), Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn advance_fake_clock(&self, secs: f64) {
        let now = f64::from_bits(self.inner.fake_now.load(Ordering::SeqCst));
        self.inner.fake_now.store((now + secs).to_bits(), Ordering::SeqCst);
    }

    pub fn power_saving(&self) -> bool {
        self.inner.effort.floor() >= 1
    }

    /// How the models are scheduled at the level the effects run at now.
    fn cadence(&self) -> Cadence {
        cadence(self.inner.effort.level())
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
        self.newest_normal_for(key, wait, seq, w, h, picture, epoch, MaskView::default())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn newest_normal_for(&self, key: &str, wait: bool, seq: u64, w: usize, h: usize, picture: Picture, epoch: u64, view: MaskView) -> Option<Arc<Vec<u8>>> {
        self.newest_normal_snapshot(key, wait, seq, w, h, picture, epoch, view).map(|m| m.mask)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn newest_normal_snapshot(&self, key: &str, wait: bool, seq: u64, w: usize, h: usize, picture: Picture, epoch: u64, view: MaskView) -> Option<MaskSnapshot> {
        self.newest_normal_snapshot_with_look(key, wait, seq, w, h, None, picture, epoch, view)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn newest_normal_snapshot_with_look(&self, key: &str, wait: bool, seq: u64, w: usize, h: usize, look: Option<Look>, picture: Picture, epoch: u64, view: MaskView) -> Option<MaskSnapshot> {
        if self.inner.masks_wait.load(Ordering::SeqCst) {
            let (mut look, mut picture) = (look, Some(picture));
            let Some(backend) = self.ready() else {
                self.load();
                return None;
            };
            let sv = self.stream(key);
            let mut m = sv.mask.lock().unwrap();
            m.geometry(view.geometry, view.space, view.unsure);
            m.asked = self.now();
            let has_look = look.is_some();
            let mut look_fn = || look.take().and_then(|f| f());
            let look_arg = has_look.then_some(&mut look_fn as &mut dyn FnMut() -> Option<Vec<u8>>);
            return self.mask(&mut m, &*backend, Which::Normal, seq, w, h, look_arg, &mut || picture.take().and_then(|p| p()), epoch, self.now()).map(|mask| MaskSnapshot { mask, at: m.at.max(m.seen), frame_at: m.frame_at.max(m.seen_frame), seq: m.ran.unwrap_or(seq) });
        }
        let Some(backend) = self.ready() else {
            self.load();
            return None;
        };
        self.newest_from(key, backend, Which::Normal, wait, seq, w, h, look, picture, epoch, view)
    }

    /// The high quality cutout's way in, the same as newest_mask from its own model and thread, and never waiting: None while it isn't
    /// downloaded, loading, failed, hasn't a mask yet or its newest is too old, and the caller then has the normal mask to use.
    pub fn newest_cutout(&self, key: &str, seq: u64, w: usize, h: usize, picture: Picture) -> Option<Arc<Vec<u8>>> {
        self.newest_cutout_in(key, seq, w, h, picture, self.view_epoch())
    }

    /// newest_cutout for a picture made when the view had turned `epoch` times (see newest_mask_in).
    pub fn newest_cutout_in(&self, key: &str, seq: u64, w: usize, h: usize, picture: Picture, epoch: u64) -> Option<Arc<Vec<u8>>> {
        self.newest_cutout_for(key, seq, w, h, picture, epoch, MaskView::default())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn newest_cutout_for(&self, key: &str, seq: u64, w: usize, h: usize, picture: Picture, epoch: u64, view: MaskView) -> Option<Arc<Vec<u8>>> {
        self.newest_cutout_snapshot(key, seq, w, h, picture, epoch, view).map(|m| m.mask)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn newest_cutout_snapshot(&self, key: &str, seq: u64, w: usize, h: usize, picture: Picture, epoch: u64, view: MaskView) -> Option<MaskSnapshot> {
        self.newest_cutout_snapshot_with_look(key, seq, w, h, None, picture, epoch, view)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn newest_cutout_snapshot_with_look(&self, key: &str, seq: u64, w: usize, h: usize, look: Option<Look>, picture: Picture, epoch: u64, view: MaskView) -> Option<MaskSnapshot> {
        // At the lightest level the cutout rests, so the normal mask stands in for it
        self.cadence().cutout?;
        let Some(backend) = self.cutout_ready() else {
            self.load_cutout();
            return None;
        };
        let key = cutout_key(key);
        if self.inner.masks_wait.load(Ordering::SeqCst) {
            let (mut look, mut picture) = (look, Some(picture));
            let sv = self.stream(&key);
            let mut m = sv.mask.lock().unwrap();
            m.asked = self.now();
            m.geometry(view.geometry, view.space, view.unsure);
            let has_look = look.is_some();
            let mut look_fn = || look.take().and_then(|f| f());
            let look_arg = has_look.then_some(&mut look_fn as &mut dyn FnMut() -> Option<Vec<u8>>);
            return self.mask(&mut m, &*backend, Which::Cutout, seq, w, h, look_arg, &mut || picture.take().and_then(|p| p()), epoch, self.now()).map(|mask| MaskSnapshot { mask, at: m.at.max(m.seen), frame_at: m.frame_at.max(m.seen_frame), seq: m.ran.unwrap_or(seq) });
        }
        self.newest_from(&key, backend, Which::Cutout, false, seq, w, h, look, picture, epoch, view)
    }

    #[allow(clippy::too_many_arguments)]
    fn newest_from(&self, key: &str, backend: Arc<dyn Backend>, which: Which, wait: bool, seq: u64, w: usize, h: usize, look: Option<Look>, picture: Picture, epoch: u64, view: MaskView)
                   -> Option<MaskSnapshot> {
        let sv = self.stream(key);
        let mut n = sv.newest.lock().unwrap();
        // Read under the lock drop_masks turns the count with
        if self.view_epoch() != epoch {
            return None;
        }
        if n.asked != Some(seq) {
            n.asked = Some(seq);
            n.job = Some(MaskJob { taken: self.now(), seq, size: (w, h), look, picture, epoch, geometry: view.geometry, space: view.space, unsure: view.unsure });
            if !n.thread {
                let (inner, sv2) = (self.inner.clone(), sv.clone());
                n.thread = std::thread::Builder::new().name("vision-mask".into()).spawn(move || mask_loop(inner, backend, sv2, which)).is_ok();
            }
            sv.mask_wanted.notify_all();
        }
        let usable = |n: &NewestMask| n.for_view(view).is_some_and(|m| (view.keep_late || m.epoch == epoch)
            && (view.keep_late && !view.unsure || m.usable(w, h)) && m.space == view.space && compatible_geometry(m.geometry, view.geometry));
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
        if !usable(&n) {
            return None;
        }
        let made = n.for_view(view)?;
        let (at, frame_at, made_seq) = (made.at, made.frame_at, made.seq);
        if (made.w, made.h) == (w, h) && made.geometry == view.geometry {
            return Some(MaskSnapshot { mask: made.mask.clone(), at, frame_at, seq: made_seq });
        }
        if let Some(m) = made.sizes.iter().find(|m| m.size == (w, h) && m.geometry == view.geometry) {
            return Some(MaskSnapshot { mask: m.mask.clone(), at, frame_at, seq: made_seq });
        }
        let (base, (bw, bh), geometry, generation) = (made.mask.clone(), (made.w, made.h), made.geometry, made.generation);
        drop(n);
        let resized = Arc::new(match (geometry, view.geometry) {
            (Some(old), Some(new)) if old != new => remap_mask(&base, bw, bh, w, h, &old, &new),
            _ => mask_resized(&base, bw, bh, w, h),
        });
        let mut n = sv.newest.lock().unwrap();
        let made = if view.keep_late { &mut n.background } else { &mut n.mask };
        if let Some(made) = made.as_mut().filter(|m| m.generation == generation) {
            if let Some(m) = made.sizes.iter().find(|m| m.size == (w, h) && m.geometry == view.geometry) {
                return Some(MaskSnapshot { mask: m.mask.clone(), at, frame_at, seq: made_seq });
            }
            // Moving crops must not keep a resized mask for every position they pass.
            made.sizes.truncate(3);
            made.sizes.insert(0, MaskSize { size: (w, h), geometry: view.geometry, mask: resized.clone() });
        }
        Some(MaskSnapshot { mask: resized, at, frame_at, seq: made_seq })
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
        m.asked = self.now();
        self.mask(&mut m, &*backend, Which::Normal, seq, w, h, None, picture, epoch, self.now())
    }

    #[allow(clippy::too_many_arguments)]
    fn mask(&self, m: &mut MaskState, backend: &dyn Backend, which: Which, seq: u64, w: usize, h: usize, look: Option<&mut dyn FnMut() -> Option<Vec<u8>>>, picture: &mut dyn FnMut() -> Option<Rgb>, epoch: u64, taken: f64)
            -> Option<Arc<Vec<u8>>> {
        // A picture from before a turn of the view leaves what's kept alone: it's of the old way round, and what it would blend into the next mask is too
        if self.view_epoch() != epoch {
            return None;
        }
        if m.turns != epoch {
            // The view turned since what was kept was made: it, and what the next mask would blend in, is of the old picture
            (m.turns, m.seq, m.mask, m.small, m.motion, m.ran, m.seen, m.seen_frame) = (epoch, None, None, None, Motion::default(), None, 0.0, 0.0);
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
        let now = self.now();
        let cadence = self.cadence();
        let gaps = if which == Which::Cutout { cadence.cutout.unwrap_or(cadence.mask) } else { cadence.mask };
        let overdue = m.small.is_none() || now - m.at >= gaps.still;
        let may_run = now - m.at >= gaps.moving;
        // A big move runs the normal model sooner at the levels that allow it; the cutout costs too much for that
        let may_jump = which != Which::Cutout && cadence.big_move.is_some_and(|gap| now - m.at >= gap);
        let jumped = |motion: &Motion| may_jump && motion.big();
        // With a grey look at hand the colour picture is only made when the model is about to run
        let run = if let Some(look) = look {
            let grey = look()?;
            let moved = m.motion.moved_grey(grey);
            if (moved && may_run) || jumped(&m.motion) || overdue {
                let frame = picture()?;
                let small = m.segment.as_ref().and_then(|s| s.input(&frame)).unwrap_or(frame);
                Some((small, moved))
            } else {
                if !moved {
                    m.seen = now;
                    m.seen_frame = taken;
                }
                None
            }
        } else {
            let frame = picture()?;
            let small = m.segment.as_ref().and_then(|s| s.input(&frame)).unwrap_or(frame);
            // Nothing moving: the last mask still fits, so the model runs less often (anything moving brings it back)
            let moved = m.motion.moved(&small);
            if (moved && may_run) || jumped(&m.motion) || overdue {
                Some((small, moved))
            } else {
                if !moved {
                    m.seen = now;
                    m.seen_frame = taken;
                }
                None
            }
        };
        if let Some((small, moved)) = run {
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
            // The doubtful middle is made solid first, before blending: a mask blended and then firmed would make solid what is left of an old shape
            if m.segment.as_ref().is_some_and(|s| s.blends_last()) {
                mask.data = firm_mask(&mask.data, mask.w, mask.h);
            }
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
            m.frame_at = taken;
            m.ran = Some(seq);
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
            self.inner.heads_only.lock().unwrap().remove(owner);
        }
    }

    /// Whether the owner's people are enough as faces with shoulders; an owner that wants people and never says so needs them whole.
    pub fn want_heads(&self, owner: &str, on: bool) {
        let mut heads = self.inner.heads_only.lock().unwrap();
        if on { heads.insert(owner.to_string()) } else { heads.remove(owner) };
    }

    /// People are followed by their faces now: every owner only needs heads, the effects are lighter, that is allowed and the pack has a face finder.
    pub fn following_heads(&self) -> bool {
        self.inner.following_heads()
    }

    /// How often people are looked for now on a stream: by faces at the face finder's pace, unless the pose model is doing the work (then at its own).
    fn people_gaps(&self, sv: &StreamVision) -> Gaps {
        let cadence = self.cadence();
        cadence.faces.filter(|_| self.following_heads() && !sv.posing.load(Ordering::SeqCst)).unwrap_or(cadence.people)
    }

    pub fn wants_people(&self) -> bool {
        !self.inner.people_wanted.lock().unwrap().is_empty()
    }

    /// Whether feed_raw would look at a frame of this stream now, so the caller only makes the frame's pixels when it would. Asking also starts the model when it isn't up.
    pub fn people_due(&self, key: &str) -> bool {
        if !self.wants_people() {
            return false;
        }
        if self.ready().is_none() {
            self.load();
            return false;
        }
        let sv = self.stream(key);
        let every = self.people_gaps(&sv).moving;
        let p = sv.people.lock().unwrap();
        let now = self.now();
        now - p.at >= every && now - p.probed >= every
    }

    /// A stream's frame before the view; one every PEOPLE_EVERY_S goes to its people thread while something moves, one every
    /// PEOPLE_STILL_EVERY_S while nothing does, and the rest are skipped. A frame is only looked at (look: a tiny grey copy, cheap) once
    /// every PEOPLE_EVERY_S and the small colour picture for the model (picture) is only made when it goes on. upright turns the small copy
    /// for the model (on that thread) and takes its points to the picture as shown at zoom 1.
    pub fn feed_raw(&self, key: &str, look: &mut dyn FnMut() -> Option<Vec<u8>>, picture: &mut dyn FnMut() -> Option<Rgb>, upright: Upright) {
        if !self.wants_people() {
            return;
        }
        if self.ready().is_none() {
            self.load();
            return;
        }
        let sv = self.stream(key);
        let now = self.now();
        let Gaps { moving: every, still: still_every } = self.people_gaps(&sv);
        let mut p = sv.people.lock().unwrap();
        p.fed = now;
        if now - p.at < every || now - p.probed < every {
            return;
        }
        p.probed = now;
        let Some(grey) = look() else { return };
        if !p.motion.moved_grey(grey) && now - p.at < still_every {
            return;
        }
        let Some(frame) = picture() else { return };
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
        if tidier.is_none() && !self.inner.closed.load(Ordering::SeqCst) {
            let (quit, stopped) = mpsc::channel();
            let weak = Arc::downgrade(&self.inner);
            if std::thread::Builder::new().name("vision-tidy".into()).spawn(move || tidy_loop(weak, stopped)).is_ok() {
                *tidier = Some(quit);
            }
        }
        sv
    }

    /// Close the models no plugin has asked for in IDLE_CLOSE_S, and give their memory back. The next ask makes them again.
    pub fn tidy(&self, now: Option<f64>) {
        let now = now.unwrap_or_else(|| self.now());
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
                    n.background = None;
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
                (p.at, p.probed) = (0.0, 0.0);
                drop(p);
                sv.woken.notify_all();
            }
        }
        if freed {
            (self.inner.give_back.lock().unwrap())();
        }
    }

    /// Invalidate strict masks after a view turn; Background retains its source-mapped snapshot.
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
        drop(self.inner.tidier.lock().unwrap().take());
        let keys: Vec<String> = self.inner.streams.lock().unwrap().keys().cloned().collect();
        for key in keys {
            self.drop_stream(&key);
        }
    }

    // Diagnostics

    fn note(&self, kind: &'static str, t0: std::time::Instant) {
        telescope_core::stats::note_model(kind, t0.elapsed().as_nanos() as u64);
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
        if self.inner.effort.heads() {
            line += ", following faces";
        }
        let level = self.inner.effort.level();
        if level > self.inner.effort.floor() {
            line += &format!(", effects stepped down to level {level}");
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
    /// Whether everyone that wants people only needs their heads.
    fn heads_enough(&self) -> bool {
        let wanted = self.people_wanted.lock().unwrap();
        let heads = self.heads_only.lock().unwrap();
        !wanted.is_empty() && wanted.iter().all(|o| heads.contains(o))
    }

    fn following_heads(&self) -> bool {
        let level = self.effort.level();
        cadence(level).faces.is_some() && self.effort.heads_allowed() && !self.faceless.load(Ordering::SeqCst) && self.heads_enough()
    }

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
                self.faceless.store(false, Ordering::SeqCst);
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

fn tidy_loop(inner: Weak<Inner>, stopped: Receiver<()>) {
    let every = std::time::Duration::from_secs_f64(TIDY_EVERY_S);
    while stopped.recv_timeout(every) == Err(RecvTimeoutError::Timeout) {
        let Some(strong) = inner.upgrade() else { return };
        if strong.closed.load(Ordering::SeqCst) {
            return;
        }
        Vision { inner: strong }.tidy(None);
    }
}

/// A stream's masks: only ever the newest frame waiting, so when the model falls behind it skips frames instead of queueing them.
fn compatible_geometry(old: Option<Geometry>, new: Option<Geometry>) -> bool {
    match (old, new) {
        (Some(old), Some(new)) => old.distortion == new.distortion,
        (None, None) => true,
        _ => false,
    }
}

// Sample the held mask in its original crop, so repeated moves never resample an already resampled mask.
fn remap_mask(src: &[u8], w: usize, h: usize, out_w: usize, out_h: usize, old: &Geometry, new: &Geometry) -> Vec<u8> {
    if w == 0 || h == 0 || out_w == 0 || out_h == 0 || src.len() != w * h {
        return vec![0; out_w * out_h];
    }
    if quarter_turned(old) || quarter_turned(new) {
        return remap_each(src, w, h, out_w, out_h, old, new);
    }
    // Without a quarter turn each axis maps on its own, so remap_each's sums are worked out once per column and once per row
    let mut out = vec![0; out_w * out_h];
    let cols: Vec<_> = (0..out_w).map(|x| axis_tap((x as f64 + 0.5) / out_w as f64, w, old, new, |p| p.0)).collect();
    for (y, row) in out.chunks_exact_mut(out_w).enumerate() {
        let Some((y0, y1, fy)) = axis_tap((y as f64 + 0.5) / out_h as f64, h, old, new, |p| p.1) else { continue };
        let (above, below) = (&src[y0 * w..][..w], &src[y1 * w..][..w]);
        for (pixel, col) in row.iter_mut().zip(&cols) {
            let Some((x0, x1, fx)) = *col else { continue };
            let top = above[x0] as f64 * (1.0 - fx) + above[x1] as f64 * fx;
            let bottom = below[x0] as f64 * (1.0 - fx) + below[x1] as f64 * fx;
            *pixel = (top * (1.0 - fy) + bottom * fy).round() as u8;
        }
    }
    out
}

/// Whether the view is turned a quarter, which swaps the picture's axes.
fn quarter_turned(g: &Geometry) -> bool {
    matches!(g.rotation, Some(90) | Some(270))
}

/// One axis of remap_each's sampling for views with no quarter turn: the two neighbours and the weight between them, None outside the stretched picture.
fn axis_tap(t: f64, len: usize, old: &Geometry, new: &Geometry, pick: fn((f64, f64)) -> f64) -> Option<(usize, usize, f64)> {
    if !(0.0..=1.0).contains(&pick(telescope_core::view::unstretch(t, t, new.stretch_x, new.stretch_y))) {
        return None;
    }
    let (x, y) = shown_to_view(t, t, new);
    let s = (pick(view_to_shown(x, y, old)) * len as f64 - 0.5).clamp(0.0, (len - 1) as f64);
    let i0 = s.floor() as usize;
    Some((i0, (i0 + 1).min(len - 1), s - i0 as f64))
}

/// remap_mask worked out for every pixel on its own, which a quarter turn needs.
fn remap_each(src: &[u8], w: usize, h: usize, out_w: usize, out_h: usize, old: &Geometry, new: &Geometry) -> Vec<u8> {
    let mut out = vec![0; out_w * out_h];
    for (y, row) in out.chunks_exact_mut(out_w).enumerate() {
        for (x, pixel) in row.iter_mut().enumerate() {
            let (u, v) = ((x as f64 + 0.5) / out_w as f64, (y as f64 + 0.5) / out_h as f64);
            let (covered_u, covered_v) = telescope_core::view::unstretch(u, v, new.stretch_x, new.stretch_y);
            if !(0.0..=1.0).contains(&covered_u) || !(0.0..=1.0).contains(&covered_v) {
                continue;
            }
            let (u, v) = shown_to_view(u, v, new);
            // What the held mask never saw takes the value of its nearest edge, so a person its crop cut off goes on past the cut instead of being blurred
            let (u, v) = view_to_shown(u, v, old);
            let (sx, sy) = ((u * w as f64 - 0.5).clamp(0.0, (w - 1) as f64), (v * h as f64 - 0.5).clamp(0.0, (h - 1) as f64));
            let (x0, y0) = (sx.floor() as usize, sy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (fx, fy) = (sx - x0 as f64, sy - y0 as f64);
            let top = src[y0 * w + x0] as f64 * (1.0 - fx) + src[y0 * w + x1] as f64 * fx;
            let bottom = src[y1 * w + x0] as f64 * (1.0 - fx) + src[y1 * w + x1] as f64 * fx;
            *pixel = (top * (1.0 - fy) + bottom * fy).round() as u8;
        }
    }
    out
}

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
        let MaskJob { seq, size: (w, h), look, picture, epoch, geometry, space, unsure, taken } = job;
        let (mut look, mut picture) = (look, Some(picture));
        let mut m = sv.mask.lock().unwrap();
        m.asked = monotonic();
        m.geometry(geometry, space, unsure);
        let has_look = look.is_some();
        let mut look_fn = || look.take().and_then(|f| f());
        let look_arg = has_look.then_some(&mut look_fn as &mut dyn FnMut() -> Option<Vec<u8>>);
        let mask = vision.mask(&mut m, &*backend, which, seq, w, h, look_arg, &mut || picture.take().and_then(|p| p()), epoch, taken);
        if sv.stopping.load(Ordering::SeqCst) {
            m.segment = None;
        }
        // What the model last ran on, and when: a mask got again without a run is no newer than that
        let (at, frame_at, seq) = (m.at.max(m.seen), m.frame_at.max(m.seen_frame), m.ran.unwrap_or(seq));
        drop(m);
        let mut n = sv.newest.lock().unwrap();
        n.done += 1;
        // Without one the last stays until it's too old: a failed frame now and then doesn't show. One of a picture from before a turn of the view is dropped, whenever the turn came.
        if let Some(mask) = mask {
            let made = MadeMask { w, h, mask, at, frame_at, seq, sizes: Vec::new(), generation: n.done, epoch, geometry, space };
            if geometry.is_some() {
                n.background = Some(made.clone());
            }
            if vision.inner.turns.load(Ordering::SeqCst) == epoch {
                n.mask = Some(made);
            }
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
    // The face finder, made the first time faces are followed (None in a pack without one, and the pose model does it all)
    let (mut faces, mut face_clock, mut heads, mut by_faces) = (None::<Box<dyn FaceModel>>, Timestamps::new(), Heads::new(), false);
    let mut turn = None::<Turn>;
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
        let follow_faces = inner.following_heads() && (faces.is_some() || match backend.faces() {
            Ok(made) => {
                faces = Some(made);
                true
            }
            Err(e) => {
                log::info!("Following whole bodies only, the vision pack has no face finder: {}: {}", e.kind, e.message);
                inner.faceless.store(true, Ordering::SeqCst);
                false
            }
        });
        if follow_faces != by_faces {
            by_faces = follow_faces;
            log::info!("People on {key} are followed by {}", if follow_faces { "their faces" } else { "their whole bodies" });
            sv.posing.store(false, Ordering::SeqCst);
            if follow_faces {
                heads.restart();
            }
        }
        // Landmarks from before a flip or turn of the picture aren't where they were
        if let Some(old) = turn.filter(|old| *old != upright.turn) {
            heads.turned(old.flip_h != upright.turn.flip_h, old.rotation != upright.turn.rotation);
        }
        turn = Some(upright.turn);
        let found = match faces.as_mut().filter(|_| follow_faces) {
            Some(finder) => match finder.detect(&prepared, face_clock.next()) {
                Ok(found) => Some(found),
                Err(e) => {
                    // The pose model looks in its place, at its own pace, until the face finder works again
                    log::warn!("Looking for faces failed; the pose model looks instead: {}: {}", e.kind, e.message);
                    heads.restart();
                    sv.posing.store(true, Ordering::SeqCst);
                    None
                }
            },
            None => None,
        };
        let faced = found.is_some();
        let seen = match found {
            Some(found) => {
                let aspect = prepared.w as f64 / prepared.h.max(1) as f64;
                let gap = cadence(inner.effort.level()).people.moving;
                let seen = heads.people(&found, aspect, Vision { inner: inner.clone() }.now(), gap, &mut || model.detect(&prepared, clock.next()));
                sv.posing.store(heads.alone(), Ordering::SeqCst);
                seen
            }
            None => model.detect(&prepared, clock.next()).map(Some),
        };
        let people = match seen {
            Ok(None) => {
                Vision { inner: inner.clone() }.note("people", t0);
                continue;
            }
            Ok(Some(people)) => people.iter().map(|p| p.mapped(&|x, y| upright.to_shown(x, y))).collect::<Vec<Person>>(),
            Err(e) => {
                log::warn!("Looking for people failed; trying the next frame: {}: {}", e.kind, e.message);
                continue;
            }
        };
        if faced && heads.by_faces() {
            // Faces are only what is reported while it is still allowed: a switch to whole bodies during the run doesn't get one more head-only report
            if !inner.following_heads() {
                continue;
            }
            inner.effort.note_heads();
        }
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
