//! The Stargazer plugin: stacks frames from the stream for a phone clipped to a telescope. Lucky imaging keeps the sharpest share of a burst
//! (moon, planets), the live stack adds every frame with outliers clipped (faint things) and star trails keep the brightest of one frame
//! every few seconds. The frames come from the video or, for the live stack and the trails, from photos the camera takes one after the other.
//! All stacking happens on a worker thread of its own; the frame step only hands pictures over and a photo's file is decoded there too.

use crate::shared::Paired;
use crate::stargazer_photos::{self as photos, Plan};
use crate::stills::{self, default_folder, ShotChoice, StillCaps};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat, Touch};
use telescope_core::host::{Host, Poster, RequestId};
use telescope_core::i18n::{n_, tr, trn, Fields};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::settings::{off_default, reset_config};
use telescope_core::stack::{sharpness, Lucky, Plane, StackMode, Stacker};
use telescope_core::system::free_space;
use telescope_core::time::{local_stamp, monotonic_ns};
use telescope_core::ui::{CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "stargazer";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/stargazer.slint"));

pub const DEFAULT_FRAMES: u32 = 300;
pub const MIN_FRAMES: u32 = 50;
pub const MAX_FRAMES: u32 = 2000;
pub const DEFAULT_PHOTOS: u32 = 10;
pub const MAX_PHOTOS: u32 = 200;
/// The longest wait asked for between photos, in seconds.
pub const MAX_PHOTO_EVERY: u32 = 300;
pub const DEFAULT_KEEP: u32 = 10;
pub const DEFAULT_TRAIL_S: u32 = 5;
const MAX_TRAIL_S: u32 = 60;
const ISO_STOPS_PER_LABEL: usize = 3;
const ISO_LABELS: [i64; 10] = [50, 100, 200, 400, 800, 1600, 3200, 6400, 12800, 25600];
/// Live stacking leaves out values further than this many standard deviations from the mean.
const KAPPA: f32 = 2.5;
/// Pictures waiting for the worker; a newer one is dropped while this many are there.
const QUEUE: usize = 6;
/// Between pictures handed over for the focus readout when no run is going.
const FOCUS_EVERY: Duration = Duration::from_millis(250);
/// Between refreshes of the stack on the camera, and of what the card hears about a run.
const CAMERA_EVERY: Duration = Duration::from_secs(1);
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// A lucky run stopped early stacks what it has only when it kept at least this many frames.
const MIN_LUCKY: usize = 3;
/// Retained pictures of a lucky run (the kept ones and the queue) stay under this many bytes.
const LUCKY_BUDGET: usize = 1_500_000_000;
/// What stacking the kept frames takes on top of them, per pixel: a mean and a spread per channel as f32 and a weight as u32.
const STACK_BYTES_PER_PIXEL: usize = 28;
/// What the stacker holds per pixel when it only has a mean or a brightest value and a weight (star trails).
const PLAIN_STACK_BYTES_PER_PIXEL: usize = 16;
/// Between tries to take the next photo when it can't start (the camera is busy) or the picture is hidden.
const RETRY_MS: u64 = 1000;
/// How many times in a row a photo that can't start is tried before the run stops.
const BUSY_TRIES: u32 = 30;
/// A photo run stops when the volume the photos go to has less than this free.
const MIN_FREE: u64 = 1 << 30;
/// How long after a photo was cancelled a new run waits for the camera to be free, in seconds.
const SLOT_WAIT_S: f64 = 60.0;
/// Switching the plugin off waits this long for a save in progress.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);
/// Between looks at whether the picture is hidden.
const HIDDEN_EVERY: f64 = 1.0;
const TICK: &str = "tick";
const NEXT_PHOTO: &str = "photo";
const FOLDER: &str = "Stargazer";

const OPEN_TIP: &str = n_("Open the folder the stacks are saved in.");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Lucky,
    Live,
    Trails,
}

impl Mode {
    const ALL: [Mode; 3] = [Mode::Lucky, Mode::Live, Mode::Trails];

    fn key(self) -> &'static str {
        match self {
            Mode::Lucky => "lucky",
            Mode::Live => "live",
            Mode::Trails => "trails",
        }
    }

    fn index(self) -> usize {
        Mode::ALL.iter().position(|m| *m == self).unwrap_or(0)
    }

    fn from_key(key: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.key() == key)
    }
}

/// Where the frames of a run come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Video,
    Photos,
}

impl Input {
    const ALL: [Input; 2] = [Input::Video, Input::Photos];

    fn key(self) -> &'static str {
        match self {
            Input::Video => "video",
            Input::Photos => "photos",
        }
    }

    fn index(self) -> usize {
        Input::ALL.iter().position(|i| *i == self).unwrap_or(0)
    }

    fn from_key(key: &str) -> Option<Input> {
        Input::ALL.into_iter().find(|i| i.key() == key)
    }

    /// Whether this mode can take its frames from here: lucky imaging looks at hundreds of frames, which photos are too slow to give.
    fn suits(self, mode: Mode) -> bool {
        self == Input::Video || mode != Mode::Lucky
    }
}

/// The live stack. Photos are few, so one with nothing to line up on (a dark Night shot) is still taken as the base.
fn live_stacker(input: Input) -> Stacker {
    let mut stacker = Stacker::new(StackMode::Clip { kappa: KAPPA });
    stacker.set_any_base(input == Input::Photos);
    stacker
}

/// What the stacker holds for each pixel in this mode.
fn accumulator_bytes(mode: Mode) -> usize {
    if mode == Mode::Live { STACK_BYTES_PER_PIXEL } else { PLAIN_STACK_BYTES_PER_PIXEL }
}

/// Where a run is on the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Collecting,
    /// The frames are in and being lined up and saved.
    Stacking,
}

/// What the frame step on the stream's thread and the plugin on the window's thread both look at.
#[derive(Default)]
struct Shared {
    /// The run that wants frames (0: none).
    run: AtomicU64,
    /// The stream that run is of; frames of any other are left alone.
    source: Mutex<String>,
    /// The stream's picture is hidden by another step, so nothing is collected and the stack stays off the camera.
    hidden: AtomicBool,
    window_hidden: AtomicBool,
    card_folded: AtomicBool,
    /// The host's own flag for the run's stream, current at every frame (the poll above can be a second behind).
    host_hidden: Mutex<Option<Arc<AtomicBool>>>,
    mode: AtomicU8,
    /// Where the frames come from (the index of an Input).
    input: AtomicU8,
    /// The size of the video's picture on each stream, as the frame step last saw it. A stack of photos goes on the camera at that size.
    video_size: Mutex<HashMap<String, (u32, u32)>>,
    /// Star trails take a frame this often.
    interval_ms: AtomicU64,
    camera: AtomicBool,
    /// Pictures handed over that the worker hasn't taken yet.
    queued: AtomicUsize,
    /// The stream the panels show; the others are never looked at.
    focused: Mutex<String>,
    last_hand: Mutex<Option<Instant>>,
    /// The latest stack, for the camera.
    result: Mutex<Option<Stack>>,
}

/// A stack and the stream it was made from, which is the only one it goes back onto.
struct Stack {
    source: String,
    frame: Frame,
}

impl Shared {
    fn focus_visible(&self) -> bool {
        !self.window_hidden.load(Ordering::SeqCst) && !self.card_folded.load(Ordering::SeqCst)
    }

    fn mode(&self) -> Mode {
        Mode::ALL.get(usize::from(self.mode.load(Ordering::SeqCst))).copied().unwrap_or(Mode::Lucky)
    }

    fn input(&self) -> Input {
        Input::ALL.get(usize::from(self.input.load(Ordering::SeqCst))).copied().unwrap_or(Input::Video)
    }

    /// Whether the picture is hidden by another step now: the plugin's last look or the host's own flag, which is current.
    fn hidden_now(&self) -> bool {
        self.hidden.load(Ordering::SeqCst) || self.host_hidden.lock().unwrap().as_ref().is_some_and(|f| f.load(Ordering::SeqCst))
    }

    /// Notes the size of a stream's video picture; whether it was another than before.
    fn note_video_size(&self, source: &str, size: (u32, u32)) -> bool {
        let mut sizes = self.video_size.lock().unwrap();
        if sizes.get(source) == Some(&size) {
            return false;
        }
        sizes.insert(source.to_string(), size);
        true
    }
}

/// What a run needs to know when it starts.
struct Setup {
    run: u64,
    mode: Mode,
    input: Input,
    frames: usize,
    keep: usize,
    folder: PathBuf,
    /// The stream the run is of.
    source: String,
    /// Bytes a lucky run may retain.
    budget: usize,
    /// A live stack that's still there goes on instead of starting over.
    fresh: bool,
}

enum Msg {
    /// A picture of that stream, handed over for that run (0: just for the focus readout).
    Frame { run: u64, source: String, frame: Frame },
    Start(Setup),
    /// A photo's file, taken for that run of that stream, to be decoded and stacked.
    Photo { run: u64, source: String, path: PathBuf },
    Stop,
    Save,
    /// Wipes the stack, answered for that run (a new one when nothing was going, so earlier progress is left behind).
    Reset { run: u64 },
    Publish,
    Quit,
}

/// The way to the worker: frames wait in a queue of QUEUE and more are dropped, so handing one over never blocks.
#[derive(Clone)]
struct Feed {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    /// Every message sent and not yet dealt with, for whoever waits for the worker to catch up.
    pending: Arc<AtomicUsize>,
}

impl Feed {
    fn send(&self, msg: Msg) -> bool {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let sent = self.tx.send(msg).is_ok();
        if !sent {
            self.pending.fetch_sub(1, Ordering::SeqCst);
        }
        sent
    }

    /// Whether the frame was handed over (not when the queue is full).
    fn hand(&self, run: u64, source: &str, frame: Frame) -> bool {
        if self.shared.queued.fetch_add(1, Ordering::SeqCst) >= QUEUE {
            self.shared.queued.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        let sent = self.send(Msg::Frame { run, source: source.to_string(), frame });
        if !sent {
            self.shared.queued.fetch_sub(1, Ordering::SeqCst);
        }
        sent
    }
}

// ── The worker ───────────────────────────────────────────────────────────────

/// Size and format of a picture: a run only ever takes pictures of one.
type Shape = (u32, u32, PixelFormat);

/// What a picture means for the run it was handed over for.
enum Verdict {
    Go,
    /// It is of another stream than the run.
    Left,
    /// It is of another size or format than the run's pictures so far.
    Resized,
}

struct Job {
    run: u64,
    mode: Mode,
    input: Input,
    active: bool,
    folder: PathBuf,
    source: String,
    budget: usize,
    target: usize,
    keep: usize,
    /// How many frames a lucky run holds: its share of the target, or fewer when memory says so (known with the first picture).
    capacity: usize,
    shape: Option<Shape>,
    /// The next picture is the first of this stretch of the run, which may start a live stack over.
    first: bool,
    /// The cap on a lucky run when it is lower than the share asked for, until it has been told.
    limited: Option<usize>,
    lucky: Option<Lucky>,
    kept: usize,
    seen: usize,
    stacker: Option<Stacker>,
    refused: usize,
    /// How a photo run shrinks its photos, settled with the first one and kept for the run.
    plan: Option<Plan>,
    /// The run's working size hasn't been told to the card yet.
    working: bool,
}

/// How many frames a lucky run can hold: the share it wants, or fewer when that many pictures like this one would pass the budget (the
/// ones waiting in the queue count too). Never under one.
fn lucky_capacity(wanted: usize, frame: &Frame, budget: usize) -> usize {
    let bytes = frame.format.size(frame.width, frame.height).max(1);
    let stacking = frame.width as usize * frame.height as usize * STACK_BYTES_PER_PIXEL;
    (budget.saturating_sub(stacking) / bytes).saturating_sub(QUEUE).clamp(1, wanted.max(1))
}

impl Job {
    fn new(setup: &Setup) -> Job {
        let capacity = (setup.frames * setup.keep).div_ceil(100).max(1);
        let stacker = match setup.mode {
            Mode::Lucky => None,
            Mode::Live => Some(live_stacker(setup.input)),
            Mode::Trails => {
                let mut stacker = Stacker::new(StackMode::Max);
                stacker.set_align(false);
                Some(stacker)
            }
        };
        Job {
            run: setup.run, mode: setup.mode, input: setup.input, active: true, folder: setup.folder.clone(), source: setup.source.clone(), budget: setup.budget,
            target: setup.frames, keep: setup.keep, capacity, shape: None, first: true, limited: None, lucky: None, kept: 0, seen: 0, stacker, refused: 0, plan: None, working: true,
        }
    }

    /// Frames in the stack (a lucky run's kept frames until they are stacked).
    fn count(&self) -> usize {
        self.stacker.as_ref().map_or(self.kept, Stacker::count)
    }

    /// Looks at a picture before it is collected: it has to be of the run's stream and of one size and format. The first of a stretch settles
    /// those (and a lucky run's capacity, from the size); a live stack that goes on at another size starts over from it.
    fn check(&mut self, source: &str, frame: &Frame) -> Verdict {
        let first = self.first;
        let verdict = self.check_shape(source, (frame.width, frame.height, frame.format));
        if first && matches!(verdict, Verdict::Go) && self.mode == Mode::Lucky && self.lucky.is_none() {
            let wanted = self.capacity;
            self.capacity = lucky_capacity(wanted, frame, self.budget);
            self.lucky = Some(Lucky::new(self.capacity));
            self.limited = (self.capacity < wanted).then_some(self.capacity);
        }
        verdict
    }

    /// The part of check that only needs the size: a picture of this shape, from this stream, for the run.
    fn check_shape(&mut self, source: &str, shape: Shape) -> Verdict {
        if self.source != source {
            return Verdict::Left;
        }
        if !std::mem::take(&mut self.first) {
            return if self.shape == Some(shape) { Verdict::Go } else { Verdict::Resized };
        }
        if self.shape != Some(shape) {
            self.shape = Some(shape);
            if self.mode == Mode::Live {
                self.stacker = Some(live_stacker(self.input));
                (self.refused, self.seen) = (0, 0);
            }
        }
        Verdict::Go
    }
}

/// How a save went for the card: whether it ends the run, whether the run was cut short, and what was in the stack.
struct Outcome {
    last: bool,
    partial: bool,
    count: usize,
    refused: usize,
}

struct Worker {
    shared: Arc<Shared>,
    poster: Arc<dyn Poster>,
    job: Option<Job>,
    last_focus: Option<Instant>,
    last_progress: Option<Instant>,
    last_publish: Option<Instant>,
}

/// The stack as a PNG in the folder, named for the moment; the path it went to.
fn save_png(frame: &Frame, folder: &Path) -> Result<PathBuf, String> {
    let size = tiny_skia::IntSize::from_wh(frame.width, frame.height).ok_or("empty picture")?;
    let pixmap = tiny_skia::Pixmap::from_vec(frame.data.to_vec(), size).ok_or("picture's data doesn't fit its size")?;
    let bytes = pixmap.encode_png().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let stamp = local_stamp();
    // The name is taken by making the file, so another save at the same moment can't pick it too and replace the stack
    let path = telescope_core::names::reserve_file(folder, |n| if n == 1 { format!("stargazer_{stamp}.png") } else { format!("stargazer_{stamp}_{n}.png") })
        .map_err(|e| e.to_string())?;
    if let Err(e) = write_atomic(&path, &bytes) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

/// Writes the file under a temporary name beside it and renames it into place, so the final name never holds half a picture.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    let temp = path.with_file_name(name);
    let done = std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, path));
    if done.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    done.map_err(|e| e.to_string())
}

impl Worker {
    fn post(&self, token: &str, data: Value) {
        self.poster.post(token, data);
    }

    fn run(mut self, rx: Receiver<Msg>, pending: Arc<AtomicUsize>) {
        while let Ok(msg) = rx.recv() {
            let quit = matches!(msg, Msg::Quit);
            self.handle(msg);
            pending.fetch_sub(1, Ordering::SeqCst);
            if quit {
                break;
            }
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Frame { run, source, frame } => {
                self.shared.queued.fetch_sub(1, Ordering::SeqCst);
                self.frame(run, source, frame);
            }
            Msg::Start(setup) => self.start(setup),
            Msg::Photo { run, source, path } => self.photo(run, source, path),
            Msg::Stop => self.stop(),
            Msg::Save => self.save_live(),
            Msg::Reset { run } => self.reset(run),
            Msg::Publish => self.publish(true),
            Msg::Quit => {}
        }
    }

    fn start(&mut self, setup: Setup) {
        let live_goes_on = !setup.fresh && setup.mode == Mode::Live
            && self.job.as_ref().is_some_and(|j| j.mode == Mode::Live && j.input == setup.input && j.stacker.is_some() && j.source == setup.source);
        if live_goes_on {
            if let Some(job) = self.job.as_mut() {
                job.run = setup.run;
                job.active = true;
                job.first = true;
                job.target = setup.frames;
                job.plan = None;
                job.working = true;
            }
        } else {
            self.job = Some(Job::new(&setup));
            *self.shared.result.lock().unwrap() = None;
        }
        self.progress(true);
    }

    fn frame(&mut self, run: u64, source: String, frame: Frame) {
        if self.shared.focus_visible() && self.last_focus.is_none_or(|at| at.elapsed() >= FOCUS_EVERY) {
            self.last_focus = Some(Instant::now());
            let plane = Plane::from_frame(&frame, (frame.width as usize).div_ceil(1024));
            self.post("focus", json!({"now": plane.map_or(0.0, |p| sharpness(&p))}));
        }
        // The run the picture was handed over for, not the one wanting frames now: what was admitted before a Stop is still collected
        let verdict = match self.job.as_mut().filter(|j| run != 0 && j.active && j.run == run) {
            Some(job) => job.check(&source, &frame),
            None => return,
        };
        match verdict {
            Verdict::Go => {}
            // Ended the way the stream ending ends it
            Verdict::Left => return self.stop(),
            Verdict::Resized => {
                self.stop();
                self.post("resized", json!({"run": run}));
                return;
            }
        }
        if let Some(kept) = self.job.as_mut().and_then(|j| j.limited.take()) {
            self.post("capped", json!({"run": run, "kept": kept}));
        }
        let Some(job) = self.job.as_mut() else { return };
        match job.mode {
            Mode::Lucky => {
                job.seen += 1;
                if job.lucky.as_mut().is_some_and(|l| l.add(frame)) {
                    job.kept = (job.kept + 1).min(job.capacity);
                }
                if job.seen >= job.target {
                    self.finish_lucky(false);
                    return;
                }
            }
            Mode::Live | Mode::Trails => {
                if job.stacker.as_mut().is_some_and(|s| s.add(&frame).is_none()) {
                    job.refused += 1;
                }
                self.publish(false);
            }
        }
        self.progress(false);
    }

    /// A photo's file for the run: decoded, shrunk to the run's size and stacked, then answered so the next one can be asked for. A photo for a
    /// run that isn't going (Stop came first) is left alone and not answered.
    fn photo(&mut self, run: u64, source: String, path: PathBuf) {
        let Some(job) = self.job.as_mut().filter(|j| j.active && j.run == run && j.input == Input::Photos) else { return };
        if job.source != source {
            return self.stop();
        }
        // The picture can have been covered while the photo was taken, which the request's own check at its start couldn't know
        if self.shared.hidden_now() {
            return self.discard(run, &path);
        }
        let header = match photos::probe(&path) {
            Ok(header) => header,
            Err(error) => return self.photo_failed(run, "unreadable", &error),
        };
        let Some(job) = self.job.as_mut() else { return };
        if job.first {
            let preview = self.shared.video_size.lock().unwrap().get(&job.source).map_or(1920 * 1080, |&(w, h)| (w as usize * h as usize).min(photos::PREVIEW_MAX_PIXELS));
            match photos::plan(header.len, header.size, accumulator_bytes(job.mode), preview, job.budget) {
                Some(plan) => job.plan = Some(plan),
                None => return self.photo_failed(run, "too_large", "no way of shrinking the photo makes it fit in memory"),
            }
        }
        let Some(plan) = job.plan else { return };
        let shape = {
            let (w, h) = photos::scaled(header.size, plan.factor);
            (w as u32, h as u32, PixelFormat::Rgba)
        };
        match job.check_shape(&source, shape) {
            Verdict::Go => {}
            Verdict::Left => return self.stop(),
            Verdict::Resized => {
                self.stop();
                self.post("resized", json!({"run": run}));
                return;
            }
        }
        if std::mem::take(&mut job.working) {
            let (width, height) = job.plan.map_or((0, 0), |p| p.size);
            let (from_width, from_height) = plan.source;
            self.post("working", json!({"run": run, "width": width, "height": height, "from_width": from_width, "from_height": from_height}));
        }
        let photo = match photos::read(&path, header) {
            Ok(photo) => photo,
            Err(error) => return self.photo_failed(run, "unreadable", &error),
        };
        let frame = match photo.decode(plan.factor) {
            Ok(frame) => frame,
            Err(error) => return self.photo_failed(run, "unreadable", &error),
        };
        // Decoding takes a while, and a cover can land in it
        if self.shared.hidden_now() {
            return self.discard(run, &path);
        }
        let Some(job) = self.job.as_mut() else { return };
        let accepted = job.stacker.as_mut().is_some_and(|s| s.add(&frame).is_some());
        job.seen += 1;
        if !accepted {
            job.refused += 1;
        }
        drop(frame);
        self.publish(true);
        self.progress(true);
        self.post("photo", json!({"run": run, "accepted": accepted}));
    }

    /// A photo taken while the picture was hidden is not stacked and its file is deleted, since what was hidden must not stay on disk. The run
    /// takes it again.
    fn discard(&self, run: u64, path: &Path) {
        let _ = std::fs::remove_file(path);
        self.post("photo", json!({"run": run, "hidden": true}));
    }

    /// The run's photo couldn't be taken in: the plugin stops the run, which keeps the stack it has.
    fn photo_failed(&self, run: u64, kind: &str, error: &str) {
        self.post("photo", json!({"run": run, "failed": kind, "error": error}));
    }

    /// Takes the run's turn for frames away, so the step stops handing them over.
    fn release(&mut self) {
        if let Some(job) = self.job.as_mut() {
            let _ = self.shared.run.compare_exchange(job.run, 0, Ordering::SeqCst, Ordering::SeqCst);
            job.active = false;
        }
    }

    fn finish_lucky(&mut self, partial: bool) {
        self.release();
        let Some(job) = self.job.as_mut() else { return };
        let mut frames = job.lucky.take().map(Lucky::into_best).unwrap_or_default();
        if partial {
            frames.truncate((job.seen * job.keep).div_ceil(100).max(MIN_LUCKY));
        }
        let (run, seen, target, folder) = (job.run, job.seen, job.target, job.folder.clone());
        self.post("progress", json!({"run": run, "stacking": true, "collected": seen, "target": target, "count": frames.len(), "refused": 0}));
        let mut stacker = Stacker::new(StackMode::Mean);
        let refused = frames.iter().filter(|f| stacker.add(f).is_none()).count();
        let (count, result) = (stacker.count(), stacker.result());
        if let Some(job) = self.job.as_mut() {
            job.stacker = Some(stacker);
            job.refused = refused;
        }
        match result {
            Some(result) => {
                self.publish_frame(&result);
                self.saved(run, &result, &folder, Outcome { last: true, partial, count, refused });
            }
            None => self.post("failed", json!({"run": run, "final": true, "error": "no frame could be lined up"})),
        }
    }

    fn saved(&self, run: u64, result: &Frame, folder: &Path, outcome: Outcome) {
        let Outcome { last, partial, count, refused } = outcome;
        match save_png(result, folder) {
            Ok(path) => self.post("saved", json!({"run": run, "final": last, "partial": partial, "path": path.display().to_string(), "frames": count, "refused": refused})),
            // The stack stays in the job, so Save can try again
            Err(error) => self.post("failed", json!({"run": run, "final": last, "error": error, "retry": true})),
        }
    }

    fn stop(&mut self) {
        let Some(job) = self.job.as_mut().filter(|j| j.active) else { return };
        let (run, mode) = (job.run, job.mode);
        match mode {
            Mode::Lucky if job.kept >= MIN_LUCKY => self.finish_lucky(true),
            Mode::Trails if job.stacker.as_ref().is_some_and(|s| s.count() >= if job.input == Input::Photos { 1 } else { 2 }) => {
                self.release();
                let Some(job) = self.job.as_ref() else { return };
                let (folder, count, refused) = (job.folder.clone(), job.count(), job.refused);
                if let Some(result) = job.stacker.as_ref().and_then(Stacker::result) {
                    self.publish_frame(&result);
                    self.saved(run, &result, &folder, Outcome { last: true, partial: false, count, refused });
                }
            }
            _ => {
                let few = mode != Mode::Live;
                self.release();
                self.post("ended", json!({"run": run, "few": few}));
            }
        }
        if mode == Mode::Live {
            self.progress(true);
        }
    }

    fn save_live(&mut self) {
        let Some(job) = self.job.as_ref() else { return };
        let run = job.run;
        match job.stacker.as_ref().and_then(Stacker::result) {
            Some(result) => self.saved(run, &result, &job.folder, Outcome { last: false, partial: false, count: job.count(), refused: job.refused }),
            None => self.post("failed", json!({"run": run, "final": false, "error": "nothing to save"})),
        }
    }

    fn reset(&mut self, run: u64) {
        if let Some(job) = self.job.as_mut() {
            if job.active && job.mode == Mode::Live {
                job.stacker = Some(live_stacker(job.input));
                job.refused = 0;
            } else {
                self.job = None;
            }
        }
        *self.shared.result.lock().unwrap() = None;
        self.post("progress", json!({"run": run, "stacking": false, "collected": 0, "target": 0, "count": 0, "refused": 0, "reset": true}));
    }

    /// What goes on the camera for a stack: a video stack as it is, a stack of photos as a copy the size of the video's picture (none until the
    /// frame step has seen that size, and none while the picture is hidden).
    fn shown(&self, job: &Job, result: Frame) -> Option<Frame> {
        if job.input == Input::Video {
            return Some(result);
        }
        if self.shared.hidden_now() {
            return None;
        }
        let (width, height) = *self.shared.video_size.lock().unwrap().get(&job.source)?;
        if width as usize * height as usize > photos::PREVIEW_MAX_PIXELS {
            return None;
        }
        photos::fit_into(&result, width, height)
    }

    fn publish_frame(&self, result: &Frame) {
        if let Some(job) = self.job.as_ref().filter(|_| self.shared.camera.load(Ordering::SeqCst)) {
            if let Some(frame) = self.shown(job, result.clone()) {
                *self.shared.result.lock().unwrap() = Some(Stack { source: job.source.clone(), frame });
            }
        }
    }

    /// Puts the stack where the camera step finds it, about once a second.
    fn publish(&mut self, force: bool) {
        if !self.shared.camera.load(Ordering::SeqCst) || (!force && self.last_publish.is_some_and(|at| at.elapsed() < CAMERA_EVERY)) {
            return;
        }
        let Some(job) = self.job.as_ref() else { return };
        let Some(frame) = job.stacker.as_ref().and_then(Stacker::result).and_then(|result| self.shown(job, result)) else { return };
        self.last_publish = Some(Instant::now());
        *self.shared.result.lock().unwrap() = Some(Stack { source: job.source.clone(), frame });
    }

    fn progress(&mut self, force: bool) {
        if !force && self.last_progress.is_some_and(|at| at.elapsed() < PROGRESS_EVERY) {
            return;
        }
        self.last_progress = Some(Instant::now());
        let Some(job) = self.job.as_ref() else { return };
        self.post("progress", json!({"run": job.run, "stacking": false, "collected": job.seen, "target": job.target, "count": job.count(), "refused": job.refused}));
    }
}

// ── The frame step ───────────────────────────────────────────────────────────

/// Hands pictures to the worker and, with the camera switch on, swaps the picture for the latest stack. Nothing else happens on the stream's thread.
struct Handoff {
    shared: Arc<Shared>,
    feed: Feed,
}

impl Handoff {
    fn hidden(&self) -> bool {
        self.shared.hidden_now()
    }

    fn focused_here(&self, source_id: &str) -> bool {
        *self.shared.focused.lock().unwrap() == source_id
    }

    /// The run a picture of this stream is wanted for (0: only for the focus readout), None when it isn't wanted at all: another stream than
    /// the run's, or the one the panels show when nothing runs, or a picture something else hides.
    fn wanted(&self, source_id: &str, now: Instant) -> Option<u64> {
        if self.hidden() || self.shared.queued.load(Ordering::SeqCst) >= QUEUE {
            return None;
        }
        let run = self.shared.run.load(Ordering::SeqCst);
        if run == 0 && !self.shared.focus_visible() {
            return None;
        }
        let mine = if run == 0 { self.focused_here(source_id) } else { *self.shared.source.lock().unwrap() == source_id };
        let wait = match (run, self.shared.mode()) {
            (0, _) => FOCUS_EVERY,
            (_, Mode::Trails) => Duration::from_millis(self.shared.interval_ms.load(Ordering::SeqCst)),
            _ => Duration::ZERO,
        };
        let due = !self.shared.last_hand.lock().unwrap().is_some_and(|at| now.duration_since(at) < wait);
        (mine && due).then_some(run)
    }

    /// The stack, when it goes on this stream's picture: the camera switch is on, the stream is the one shown and the stack's own, and nothing hides it.
    fn stack(&self, source_id: &str) -> Option<Frame> {
        if !self.shared.camera.load(Ordering::SeqCst) || self.hidden() || !self.focused_here(source_id) {
            return None;
        }
        self.shared.result.lock().unwrap().as_ref().filter(|s| s.source == source_id).map(|s| s.frame.clone())
    }
}

impl Handoff {
    /// Whether the size of this stream's picture is still to be learned: a stack of photos goes on the camera at that size and no one else
    /// tells the worker. Only until it is known, so a card with the camera switch on doesn't make the pipeline read every frame.
    fn needs_size(&self, source_id: &str) -> bool {
        self.shared.camera.load(Ordering::SeqCst) && self.shared.input() == Input::Photos && self.focused_here(source_id) && !self.shared.video_size.lock().unwrap().contains_key(source_id)
    }
}

impl CpuStep for Handoff {
    fn touch(&self, ctx: &dyn FrameContext) -> Touch {
        if self.stack(ctx.source_id()).is_some() {
            Touch::Changes
        } else if self.wanted(ctx.source_id(), Instant::now()).is_some() || self.needs_size(ctx.source_id()) {
            Touch::Reads
        } else {
            Touch::Nothing
        }
    }

    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        let source = ctx.source_id();
        let now = Instant::now();
        // A stack of photos is made for the video's size, so the worker is told when that changes
        if self.shared.camera.load(Ordering::SeqCst) && self.shared.input() == Input::Photos && self.shared.note_video_size(source, (frame.width, frame.height)) {
            self.feed.send(Msg::Publish);
        }
        if let Some(run) = self.wanted(source, now) {
            if self.feed.hand(run, source, frame.clone()) {
                *self.shared.last_hand.lock().unwrap() = Some(now);
            }
        }
        if let Some(mut stack) = self.stack(source).filter(|s| (s.width, s.height) == (frame.width, frame.height) && s.format == PixelFormat::Rgba) {
            (stack.number, stack.time_ns, stack.capture_ns) = (frame.number, frame.time_ns, frame.capture_ns);
            *frame = stack;
        }
    }
}

// ── The plugin ───────────────────────────────────────────────────────────────

/// 4:07 or 1:04:07.
fn clock_text(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// A run that takes photos: what it pinned when it started, so nothing changed on the card or recalled from a preset reaches it, and where it is.
struct PhotoRun {
    /// The options every photo of the run is asked for with.
    options: Value,
    folder: String,
    source: String,
    total: usize,
    /// The shortest time from the start of one photo to the start of the next.
    every_ms: u64,
    /// Photos the worker has dealt with (stacked, or refused by the stacker).
    finished: usize,
    /// A photo's file is with the worker, which hasn't answered yet.
    handed: bool,
    /// When the photo in flight, or the last one, was asked for, on the plugin's clock.
    started_at: Option<f64>,
    /// How many times in a row the next photo couldn't start.
    tries: u32,
}

/// Settings as an older version saved them: Night, the fourth mode, took one long photo. It is the live stack taking one photo now, with the
/// shot's own values kept.
fn migrated(cfg: &Value) -> Value {
    let mut cfg = cfg.clone();
    if cfg.get("mode").and_then(Value::as_str) == Some("night") {
        cfg["mode"] = json!(Mode::Live.key());
        cfg["input"] = json!(Input::Photos.key());
        cfg["photo_count"] = json!(1);
    }
    cfg
}

pub struct StargazerPlugin {
    mode: Mode,
    input: Input,
    /// Where the stack that is there got its frames.
    stack_input: Input,
    /// The stream cleaner video was asked of, while a run of video frames collects.
    clean_for: Option<String>,
    frames: u32,
    keep: u32,
    trail_s: u32,
    photo_count: u32,
    photo_every: u32,
    camera: bool,
    use_zoom: bool,
    shot: ShotChoice,
    shot_initialized: bool,
    /// The shot is still the first-pick default, which gives way to Camera on a dial the camera can't set.
    shot_default: bool,
    customize_open: bool,
    taking: Option<RequestId>,
    exposing: bool,
    /// The run that is taking photos, if one is.
    photo: Option<PhotoRun>,
    /// The problem on the card is one that stopped a photo run, which a save of what it had doesn't clear.
    photo_problem: bool,
    paired: Paired,
    shared: Arc<Shared>,
    poster: Option<Arc<dyn Poster>>,
    feed: Option<Feed>,
    worker: Option<JoinHandle<()>>,
    folder: String,
    /// Bytes a lucky run may retain.
    budget: usize,
    phase: Phase,
    run: u64,
    /// The stream the last run was of, which its stack belongs to.
    source: String,
    collected: usize,
    target: usize,
    count: usize,
    refused: usize,
    /// Seconds of earlier stints of a live stack, and when this one began on the clock (which only runs while `ticking`).
    banked: f64,
    began: f64,
    ticking: bool,
    /// The picture is hidden by another step, as of the last look.
    hidden: bool,
    hidden_at: Option<f64>,
    /// A finished stack that couldn't be saved, which Save tries again until it works or a new run starts.
    unsaved: bool,
    /// What a cap on a lucky run's memory made of it.
    limit: String,
    focus: Option<f32>,
    best: f32,
    last: String,
    note: String,
    problem: Option<String>,
    /// The stream ended and no start has been heard since.
    down: bool,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
    clock: Box<dyn Fn() -> f64 + Send>,
    /// A DSLR's photo came back but its settings couldn't be put back, which the card says without stopping the run.
    warning: Option<String>,
    /// When a photo on its way was last cancelled: its request can hold the camera for a while, so a start right after is waited out.
    cancelled_at: Option<f64>,
    /// How many bytes are free on the volume a folder is on.
    free: Box<dyn Fn(&Path) -> Option<u64> + Send>,
    /// A photo run stops when less than this is free where its photos go.
    min_free: u64,
}

impl Default for StargazerPlugin {
    fn default() -> Self {
        StargazerPlugin::new()
    }
}

impl StargazerPlugin {
    pub fn new() -> StargazerPlugin {
        let shared = Arc::new(Shared::default());
        shared.interval_ms.store(u64::from(DEFAULT_TRAIL_S) * 1000, Ordering::SeqCst);
        StargazerPlugin {
            mode: Mode::Lucky,
            input: Input::Video,
            stack_input: Input::Video,
            clean_for: None,
            frames: DEFAULT_FRAMES,
            keep: DEFAULT_KEEP,
            trail_s: DEFAULT_TRAIL_S,
            photo_count: DEFAULT_PHOTOS,
            photo_every: 0,
            camera: false,
            use_zoom: false,
            shot: ShotChoice::default(),
            shot_initialized: false,
            shot_default: false,
            customize_open: false,
            taking: None,
            exposing: false,
            photo: None,
            photo_problem: false,
            paired: Paired::default(),
            shared,
            poster: None,
            feed: None,
            worker: None,
            folder: Path::new(&default_folder()).join(FOLDER).display().to_string(),
            budget: LUCKY_BUDGET,
            phase: Phase::Idle,
            run: 0,
            source: String::new(),
            collected: 0,
            target: 0,
            count: 0,
            refused: 0,
            banked: 0.0,
            began: 0.0,
            ticking: false,
            hidden: false,
            hidden_at: None,
            unsaved: false,
            limit: String::new(),
            focus: None,
            best: 0.0,
            last: String::new(),
            note: String::new(),
            problem: None,
            down: false,
            pending: false,
            clock: Box::new(|| monotonic_ns() as f64 / 1e9),
            warning: None,
            cancelled_at: None,
            free: Box::new(free_space),
            min_free: MIN_FREE,
        }
    }

    fn default_shot() -> ShotChoice {
        ShotChoice { shutter_s: Some(10.0), iso: Some(800), focus_diopters: Some(0.0) }
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    /// The way to the worker, which is started with the first need of it.
    fn feed(&mut self) -> Option<Feed> {
        if self.feed.is_none() {
            let poster = self.poster.clone()?;
            let (tx, rx) = channel();
            let pending = Arc::new(AtomicUsize::new(0));
            let worker = Worker { shared: self.shared.clone(), poster, job: None, last_focus: None, last_progress: None, last_publish: None };
            let waiting = pending.clone();
            self.worker = Some(std::thread::Builder::new().name("stargazer".into()).spawn(move || worker.run(rx, waiting)).ok()?);
            self.feed = Some(Feed { tx, shared: self.shared.clone(), pending });
        }
        self.feed.clone()
    }

    fn send(&mut self, msg: Msg) {
        if let Some(feed) = self.feed() {
            feed.send(msg);
        }
    }

    fn can_start(&self, host: &dyn Host) -> bool {
        !self.down && host.is_streaming() && (self.input != Input::Photos || self.still_caps(host).is_some())
    }

    fn elapsed(&self) -> f64 {
        self.banked + if self.ticking { self.now() - self.began } else { 0.0 }
    }

    /// Stops the clock, once: what has run so far is banked.
    fn freeze(&mut self) {
        if self.ticking {
            self.banked = self.elapsed();
            self.ticking = false;
        }
    }

    fn can_save(&self) -> bool {
        (self.mode == Mode::Live && self.count > 0) || self.unsaved
    }

    /// Looks again at whether another step hides the picture, at most about once a second unless forced.
    fn refresh_hidden(&mut self, host: &mut dyn Host, force: bool) {
        let now = self.now();
        if !force && self.hidden_at.is_some_and(|at| now - at < HIDDEN_EVERY) {
            return;
        }
        self.hidden_at = Some(now);
        let ours = self.run > 0 && (self.phase != Phase::Idle || self.camera);
        let source = if ours { self.source.clone() } else { host.focused_source_id().unwrap_or_default() };
        *self.shared.host_hidden.lock().unwrap() = host.hidden_flag(&source);
        let hidden = host.picture_hidden(Some(&source));
        if hidden != self.hidden {
            self.hidden = hidden;
            self.shared.hidden.store(hidden, Ordering::SeqCst);
            self.show(host);
        }
    }

    // ── What the card shows ───────────────────────────────────────────

    fn still_caps(&self, host: &dyn Host) -> Option<StillCaps> {
        let source = host.focused_source_id()?;
        serde_json::from_value(host.still_caps(Some(&source))?).ok()
    }

    fn shutter_props(&self, caps: Option<&StillCaps>) -> Value {
        let stops = caps.and_then(|c| c.shot.as_ref()).map(stills::shot_shutter_stops).unwrap_or_default();
        let (labels, index) = stills::shot_shutter_labels(&stops, self.shot.shutter_s);
        json!({
            "shutter-stops": labels, "shutter-index": index,
            "shutter-enabled": self.phase == Phase::Idle && (!stops.is_empty() || self.shot.shutter_s.is_some()),
            "shutter-tip": if stops.is_empty() && self.shot.shutter_s.is_some() { tr("Use the camera's own shutter speed.") } else if caps.is_none() { tr("Waiting for the picture") } else if stops.is_empty() { tr("This camera can't set the shutter speed.") } else { tr("Set the shutter speed for the photo.") },
        })
    }

    fn iso_props(&self, caps: Option<&StillCaps>) -> Value {
        let stops = caps.and_then(|c| c.shot.as_ref()).map(stills::shot_iso_stops).unwrap_or_default();
        let (labels, index) = stills::shot_iso_labels(&stops, self.shot.iso);
        let from = stops.iter().position(|v| ISO_LABELS.contains(v)).map_or(1, |i| i + 1);
        json!({
            "iso-stops": labels, "iso-index": index, "iso-per": ISO_STOPS_PER_LABEL, "iso-from": from,
            "iso-enabled": self.phase == Phase::Idle && (!stops.is_empty() || self.shot.iso.is_some()),
            "iso-tip": if stops.is_empty() && self.shot.iso.is_some() { tr("Use the camera's own ISO.") } else if caps.is_none() { tr("Waiting for the picture") } else if stops.is_empty() { tr("This camera can't set the ISO.") } else { tr("Set the ISO for the photo.") },
        })
    }

    /// The nearest focus a photo can be set to, in diopters; 0 when this camera takes no focus for a photo (an older phone, a DSLR).
    fn focus_near(caps: Option<&StillCaps>) -> f64 {
        caps.and_then(|c| c.shot.as_ref()).and_then(|c| c.focus.as_ref()).map_or(0.0, |f| f.near_diopters).max(0.0)
    }

    /// The focus the photos are taken at: none on a camera that can't set it, which then uses its own.
    fn photo_focus(&self, caps: Option<&StillCaps>) -> Option<f64> {
        self.shot.focus_diopters.filter(|_| Self::focus_near(caps) > 0.0)
    }

    fn focus_props(&self, caps: Option<&StillCaps>) -> Value {
        let near = Self::focus_near(caps);
        let focus = self.photo_focus(caps);
        json!({
            "focus-enabled": self.phase == Phase::Idle && near > 0.0,
            "focus-tip": if caps.is_none() { tr("Waiting for the picture") } else if near > 0.0 { tr("Set the focus for the photos.") } else { tr("This camera can't set the focus for a photo.") },
            "focus-value": stills::focus_position(near, focus.unwrap_or(0.0)), "focus-unset": focus.is_none(),
            "focus-readout": focus.map_or_else(|| tr("Camera"), stills::shot_focus_text),
        })
    }

    /// The focus slider moved (position on the focus scale) or was reset to the camera's own (None).
    fn set_focus(&mut self, host: &mut dyn Host, position: Option<f64>) {
        if self.input != Input::Photos || self.phase != Phase::Idle {
            return;
        }
        let near = Self::focus_near(self.still_caps(host).as_ref());
        if near <= 0.0 {
            return;
        }
        let focus = position.map(|p| (p.clamp(0.0, stills::FOCUS_SCALE) / stills::FOCUS_SCALE * near * 100.0).round() / 100.0);
        if focus != self.shot.focus_diopters {
            self.shot.focus_diopters = focus;
            self.shot_initialized = true;
            host.schedule_save();
        }
        self.show(host);
    }

    fn shot_props(&self, host: &dyn Host) -> Value {
        let caps = self.still_caps(host);
        let shown = ShotChoice { focus_diopters: self.photo_focus(caps.as_ref()), ..self.shot.clone() };
        let mut props = json!({"customize-open": self.customize_open, "shot-summary": shown.summary(stills::shot_focus_text)});
        if let Some(all) = props.as_object_mut() {
            for control in [self.shutter_props(caps.as_ref()), self.iso_props(caps.as_ref()), self.focus_props(caps.as_ref())] {
                if let Value::Object(fields) = control {
                    all.extend(fields);
                }
            }
        }
        props
    }

    fn pick_shot(&mut self, host: &mut dyn Host, which: &str, index: usize) {
        if self.input != Input::Photos || self.phase != Phase::Idle {
            return;
        }
        let caps = self.still_caps(host).unwrap_or_default();
        self.settle_default_shot(host);
        if stills::pick_shot(&mut self.shot, &caps, which == "shutter-picked", index, false) {
            self.shot_default = false;
            host.schedule_save();
        }
        self.show(host);
    }

    fn status(&self, host: &dyn Host) -> (String, &'static str) {
        if let Some(problem) = &self.problem { return (problem.clone(), "status_err"); }
        if let Some(warning) = &self.warning { return (warning.clone(), "status_warn"); }
        match self.phase {
            Phase::Collecting if self.taking.is_some() => (self.taking_text(), "status_ok"),
            Phase::Collecting if self.hidden => (tr("Paused while part of the picture is hidden"), "status_warn"),
            Phase::Collecting if self.photo.as_ref().is_some_and(|p| p.handed) => (tr("Stacking the photo"), "status_ok"),
            Phase::Collecting if self.photo.is_some() => (tr("Waiting for the next photo"), "status_ok"),
            Phase::Collecting => {
                let text = match self.mode {
                    Mode::Lucky => tr("Collecting frame {done} of {total}").field("done", self.collected).field("total", self.target),
                    Mode::Live => tr("Stacking every frame"),
                    Mode::Trails => tr("Recording the trails"),
                };
                (text, "status_ok")
            }
            Phase::Stacking => (trn("Stacking the sharpest {n} frame", "Stacking the sharpest {n} frames", self.count as i64), "status_warn"),
            Phase::Idle => {
                if !self.note.is_empty() {
                    (self.note.clone(), "status_ok")
                } else if !self.can_start(host) {
                    (tr("Waiting for the picture"), "status_dim")
                } else {
                    (tr("Ready"), "status_dim")
                }
            }
        }
    }

    /// What the card says while a photo is on its way.
    fn taking_text(&self) -> String {
        let total = self.photo.as_ref().map_or(1, |p| p.total);
        if self.exposing {
            tr("Video paused while exposing")
        } else if total > 1 {
            let n = self.photo.as_ref().map_or(1, |p| p.finished + 1);
            tr("Taking photo {n} of {total}").field("n", n).field("total", total)
        } else {
            tr("Taking the photo")
        }
    }

    fn tally(&self) -> String {
        if self.count == 0 && self.refused == 0 && self.phase == Phase::Idle {
            return String::new();
        }
        tr("In the stack: {frames}, refused: {refused}, time: {time}")
            .field("frames", self.count).field("refused", self.refused).field("time", clock_text(self.elapsed()))
    }

    fn focus_text(&self, host: &dyn Host) -> String {
        match self.focus {
            Some(now) if host.is_streaming() && !self.down => tr("Now {now}, best {best}").field("now", format!("{:.1}", now * 10_000.0)).field("best", format!("{:.1}", self.best * 10_000.0)),
            _ => tr("Waiting for the picture"),
        }
    }

    fn photo_count_text(&self) -> String {
        trn("{n} photo", "{n} photos", i64::from(self.photo_count))
    }

    fn photo_every_text(&self) -> String {
        if self.photo_every == 0 { tr("No wait") } else { tr("{seconds} s").field("seconds", self.photo_every) }
    }

    fn fixed_props() -> Value {
        json!({"mode-items": [tr("Lucky"), tr("Live"), tr("Trails")], "input-items": [tr("Video"), tr("Photos")], "save-text": tr("Save"), "reset-text": tr("Reset"),
               "camera-name": tr("Show the stack on the camera"), "open-text": tr("Open folder"), "open-tip": tr(OPEN_TIP)})
    }

    fn state_props(&self, host: &dyn Host) -> Value {
        let (status, kind) = self.status(host);
        let idle = self.phase == Phase::Idle;
        let name = Path::new(&self.last).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let saved = if name.is_empty() { String::new() } else { tr("Saved {name}").field("name", name) };
        let mut props = json!({
            "active": self.paired.on(), "mode-index": self.mode.index(), "input-index": self.input.index(), "running": self.phase != Phase::Idle,
            "frames": self.frames, "frames-text": tr("{n} frames").field("n", self.frames),
            "keep": self.keep, "keep-text": format!("{} %", self.keep),
            "trail": self.trail_s, "trail-text": tr("{seconds} s").field("seconds", self.trail_s),
            "photo-count": self.photo_count, "photo-count-text": self.photo_count_text(),
            "photo-every": self.photo_every, "photo-every-text": self.photo_every_text(),
            "run-text": match self.phase { Phase::Idle => tr("Start"), Phase::Collecting => tr("Stop"), Phase::Stacking => tr("Stacking") },
            "run-enabled": self.phase == Phase::Collecting || (idle && self.can_start(host)),
            "save-enabled": self.can_save(),
            "settings-reset-shown": self.settings_reset_shown(),
            "reset-enabled": self.mode == Mode::Live && (self.count > 0 || self.refused > 0 || self.phase == Phase::Collecting),
            "status": status, "status-kind": kind, "tally": self.tally(), "limit": self.limit, "focus-text": self.focus_text(host),
            "camera": self.camera, "use-zoom": self.use_zoom, "saved": saved, "open-enabled": !self.last.is_empty(),
        });
        if let (Some(all), Value::Object(shot)) = (props.as_object_mut(), self.shot_props(host)) {
            all.extend(shot);
        }
        props
    }

    fn all_props(&self, host: &dyn Host) -> Value {
        let mut all = Self::fixed_props();
        if let (Value::Object(a), Value::Object(s)) = (&mut all, self.state_props(host)) {
            a.extend(s);
        }
        all
    }

    fn show(&self, host: &mut dyn Host) {
        let props = self.state_props(host);
        host.set_ui_props("card", &props);
    }

    /// Once the camera is known, a first-pick default it can't set becomes Camera, so the first start isn't refused. A pick by hand is left alone.
    fn settle_default_shot(&mut self, host: &dyn Host) {
        if !self.shot_default || self.input != Input::Photos || self.phase != Phase::Idle {
            return;
        }
        let Some(caps) = self.still_caps(host) else { return };
        let shot = caps.shot.as_ref();
        if shot.map(stills::shot_shutter_stops).unwrap_or_default().is_empty() { self.shot.shutter_s = None; }
        if shot.map(stills::shot_iso_stops).unwrap_or_default().is_empty() { self.shot.iso = None; }
        self.shot_default = false;
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        self.settle_default_shot(host);
        *self.shared.focused.lock().unwrap() = host.focused_source_id().unwrap_or_default();
        if self.pending {
            self.pending = false;
            let props = self.all_props(host);
            host.set_ui_props("card", &props);
            host.refresh_frame_step();
        }
    }

    // ── Runs ──────────────────────────────────────────────────────────

    /// The options a photo of a run is asked for with. The shot's own values are left for the host to fit to what the camera can do.
    fn photo_options(&self, caps: &StillCaps) -> Value {
        let mut options = json!({"format": "jpeg", "rotation": 0, "use_zoom": self.use_zoom, "shot": {}, "skip_if_hidden": true});
        if !caps.camera { options["quality"] = json!(95); }
        stills::add_shot(&mut options, &self.shot, true, false);
        // Only a camera that takes a focus for one photo gets one: an older phone would refuse the photo
        if let Some(focus) = self.photo_focus(Some(caps)) {
            options["shot"]["focus_diopters"] = json!(focus);
        }
        options
    }

    /// Begins a run of photos. The first one is asked for before anything changes, so a camera that can't take it leaves the card as it was.
    fn start_photos(&mut self, host: &mut dyn Host) {
        let Some(source) = host.focused_source_id() else { return };
        let Some(caps) = self.still_caps(host) else { return };
        self.settle_default_shot(host);
        let Some(feed) = self.feed() else { return };
        let photo = PhotoRun {
            options: self.photo_options(&caps), folder: self.folder.clone(), source: source.clone(), total: self.photo_count as usize,
            every_ms: u64::from(self.photo_every) * 1000, finished: 0, handed: false, started_at: None, tries: 0,
        };
        if let Some(why) = self.low_disk(&photo.folder) {
            (self.note, self.problem, self.photo_problem) = (String::new(), Some(why), false);
            self.show(host);
            return;
        }
        let hidden = host.picture_hidden(Some(&source));
        let request = if hidden { None } else { host.take_still(&photo.options, &photo.folder, Some(&source)) };
        // A photo cancelled a moment ago can still hold the camera, so a start right after Stop waits for it like a photo between two does
        let slot_wait = request.is_none() && !hidden && self.cancelled_at.is_some_and(|at| self.now() - at < SLOT_WAIT_S);
        if request.is_none() && !hidden && !slot_wait {
            (self.note, self.problem, self.photo_problem) = (String::new(), Some(tr("Couldn't take the photo")), false);
            self.show(host);
            return;
        }
        let fresh = !(self.mode == Mode::Live && self.count > 0 && source == self.source && self.stack_input == Input::Photos);
        self.run += 1;
        self.source = source.clone();
        self.stack_input = Input::Photos;
        if fresh {
            (self.collected, self.target, self.count, self.refused, self.banked) = (0, 0, 0, 0, 0.0);
        }
        (self.note, self.problem, self.photo_problem, self.limit, self.unsaved) = (String::new(), None, false, String::new(), false);
        self.warning = None;
        self.phase = Phase::Collecting;
        (self.began, self.ticking) = (self.now(), true);
        self.shared.mode.store(self.mode.index() as u8, Ordering::SeqCst);
        self.shared.input.store(Input::Photos.index() as u8, Ordering::SeqCst);
        *self.shared.source.lock().unwrap() = source.clone();
        self.refresh_hidden(host, true);
        let setup = Setup {
            run: self.run, mode: self.mode, input: Input::Photos, frames: photo.total, keep: self.keep as usize, folder: PathBuf::from(&self.folder), source,
            budget: self.budget, fresh,
        };
        // The worker hears of the run before any photo is handed over for it
        feed.send(Msg::Start(setup));
        self.photo = Some(photo);
        self.taking = request;
        self.exposing = false;
        match request {
            Some(_) => self.mark_started(),
            None => host.schedule(RETRY_MS, NEXT_PHOTO),
        }
        host.want_frames(NAME, false);
        host.schedule(1000, TICK);
        self.show(host);
    }

    /// The photo in flight began now, which is where the next one's wait counts from.
    fn mark_started(&mut self) {
        let now = self.now();
        if let Some(photo) = self.photo.as_mut() {
            (photo.started_at, photo.tries) = (Some(now), 0);
        }
    }

    /// Asks for the next photo of the run when none is on its way and the worker has dealt with the last: never two at once, and one that is late
    /// is followed by one, not by the ones that would have been taken meanwhile.
    fn next_photo(&mut self, host: &mut dyn Host) {
        if self.phase != Phase::Collecting || self.taking.is_some() {
            return;
        }
        let Some(photo) = self.photo.as_ref().filter(|p| !p.handed && p.finished < p.total) else { return };
        let (options, folder, source) = (photo.options.clone(), photo.folder.clone(), photo.source.clone());
        if let Some(why) = self.low_disk(&folder) {
            self.photo_stopped(host, why);
            return;
        }
        self.refresh_hidden(host, true);
        if self.hidden {
            host.schedule(self.retry_wait(), NEXT_PHOTO);
            self.show(host);
            return;
        }
        match host.take_still(&options, &folder, Some(&source)) {
            Some(request) => {
                self.taking = Some(request);
                self.exposing = false;
                self.mark_started();
            }
            None => {
                let tries = self.photo.as_mut().map_or(BUSY_TRIES, |p| { p.tries += 1; p.tries });
                if tries >= BUSY_TRIES {
                    self.photo_stopped(host, tr("Couldn't take the photo"));
                    return;
                }
                host.schedule(self.retry_wait(), NEXT_PHOTO);
            }
        }
        self.show(host);
    }

    /// Why a photo can't be taken for lack of room: the volume the photos go to has less than the least a run keeps free.
    fn low_disk(&self, folder: &str) -> Option<String> {
        let free = (self.free)(Path::new(folder))?;
        (free < self.min_free).then(|| tr("Stopped because there's less than {size} GB free on the drive").field("size", self.min_free >> 30))
    }

    /// How long to wait before trying again after a photo was hidden or couldn't start: a second, or longer when Every says the next one
    /// can't start yet.
    fn retry_wait(&self) -> u64 {
        let Some(photo) = self.photo.as_ref() else { return RETRY_MS };
        let elapsed = photo.started_at.map_or(f64::MAX, |at| (self.now() - at).max(0.0) * 1000.0);
        RETRY_MS.max(photo.every_ms.saturating_sub(elapsed.min(1e12) as u64))
    }

    /// The request for a photo ended: its file goes to the worker, a picture that is hidden is tried again and anything else stops the run.
    fn photo_done(&mut self, host: &mut dyn Host, ok: bool, data: &Value) {
        self.taking = None;
        self.exposing = false;
        if self.photo.is_none() {
            return;
        }
        let file = data.get("files").and_then(Value::as_array).and_then(|files| files.first()).and_then(Value::as_str).filter(|s| !s.is_empty());
        let code = data.get("code").and_then(Value::as_str).unwrap_or("");
        // A DSLR that couldn't put its settings back still took the photo: it is stacked and the card says what to check
        let restore_failed = code == "restore";
        if let Some(file) = file.filter(|_| (ok && data.get("ok").and_then(Value::as_bool) != Some(false)) || restore_failed) {
            if restore_failed {
                self.warning = Some(tr("The camera's photo settings could not be restored. Check its ISO and shutter speed."));
            }
            // A cover can land while the photo exposes: what was in it isn't stacked and doesn't stay on disk
            self.refresh_hidden(host, true);
            if self.hidden {
                let _ = std::fs::remove_file(file);
                host.schedule(self.retry_wait(), NEXT_PHOTO);
            } else {
                self.last = file.to_string();
                let Some(photo) = self.photo.as_mut() else { return };
                photo.handed = true;
                let msg = Msg::Photo { run: self.run, source: photo.source.clone(), path: PathBuf::from(file) };
                self.send(msg);
            }
        } else if code == "hidden" {
            host.schedule(self.retry_wait(), NEXT_PHOTO);
        } else {
            let error = data.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()).map_or_else(|| tr("Couldn't take the photo"), String::from);
            self.photo_stopped(host, error);
            return;
        }
        self.show(host);
    }

    /// The worker has dealt with a photo (or couldn't): the run goes on after the wait, ends with the last photo or stops.
    fn photo_answered(&mut self, host: &mut dyn Host, data: &Value) {
        let now = self.now();
        let Some(photo) = self.photo.as_mut() else { return };
        photo.handed = false;
        if let Some(kind) = data.get("failed").and_then(Value::as_str) {
            log::warn!("Stargazer: {}", data.get("error").and_then(Value::as_str).unwrap_or(""));
            let why = if kind == "too_large" { tr("The photo is too big to stack in the memory there is") } else { tr("Couldn't read the photo") };
            self.photo_stopped(host, why);
            return;
        }
        if data.get("hidden") != Some(&Value::Bool(true)) {
            photo.finished += 1;
        }
        if photo.finished >= photo.total {
            self.photos_done(host);
            return;
        }
        let elapsed = photo.started_at.map_or(f64::MAX, |at| (now - at).max(0.0) * 1000.0);
        let remaining = photo.every_ms.saturating_sub(elapsed.min(1e12) as u64);
        let wait = if data.get("hidden") == Some(&Value::Bool(true)) { RETRY_MS.max(remaining) } else { remaining };
        if wait == 0 {
            self.next_photo(host);
        } else {
            host.schedule(wait, NEXT_PHOTO);
        }
        self.show(host);
    }

    /// The last photo is in: the run ends the way Stop ends it.
    fn photos_done(&mut self, host: &mut dyn Host) {
        self.stop(host);
        if self.mode == Mode::Live {
            self.note = trn("Stacked {n} photo. Save keeps it, Reset starts a new stack", "Stacked {n} photos. Save keeps it, Reset starts a new stack", self.count as i64);
        }
        self.show(host);
    }

    /// A photo couldn't be taken or read: the run stops with the stack it has, which is there to save.
    fn photo_stopped(&mut self, host: &mut dyn Host, why: String) {
        self.problem = Some(why);
        self.photo_problem = true;
        self.stop(host);
        self.show(host);
    }

    fn start(&mut self, host: &mut dyn Host) {
        if self.phase != Phase::Idle || !self.can_start(host) {
            return;
        }
        if self.input == Input::Photos {
            self.start_photos(host);
            return;
        }
        let Some(feed) = self.feed() else { return };
        // A live stack only goes on with frames of the stream it was made from
        let source = host.focused_source_id().unwrap_or_default();
        let fresh = !(self.mode == Mode::Live && self.count > 0 && source == self.source && self.stack_input == Input::Video);
        self.run += 1;
        self.source = source.clone();
        self.stack_input = Input::Video;
        if fresh {
            (self.collected, self.target, self.count, self.refused, self.banked) = (0, 0, 0, 0, 0.0);
        }
        (self.note, self.problem, self.photo_problem, self.limit, self.unsaved) = (String::new(), None, false, String::new(), false);
        self.warning = None;
        self.phase = Phase::Collecting;
        (self.began, self.ticking) = (self.now(), true);
        self.shared.mode.store(self.mode.index() as u8, Ordering::SeqCst);
        self.shared.input.store(Input::Video.index() as u8, Ordering::SeqCst);
        *self.shared.last_hand.lock().unwrap() = None;
        *self.shared.source.lock().unwrap() = source.clone();
        self.refresh_hidden(host, true);
        let setup = Setup { run: self.run, mode: self.mode, input: Input::Video, frames: self.frames as usize, keep: self.keep as usize, folder: PathBuf::from(&self.folder), source, budget: self.budget, fresh };
        // The worker hears of the run before any picture is handed over for it
        feed.send(Msg::Start(setup));
        self.shared.run.store(self.run, Ordering::SeqCst);
        host.want_frames(NAME, true);
        host.schedule(1000, TICK);
        self.sync_clean_video(host);
        self.show(host);
    }

    /// Ends the run: a lucky one with enough frames stacks what it has, star trails are saved, a live stack stays for saving. A photo on its way is
    /// cancelled and its late answer ignored. What the stack has is kept.
    fn stop(&mut self, host: &mut dyn Host) {
        if self.phase != Phase::Collecting {
            return;
        }
        if let Some(request) = self.taking.take() {
            host.cancel_still(request);
            self.exposing = false;
            self.cancelled_at = Some(self.now());
        }
        if self.photo.take().is_some() {
            host.cancel_schedule(NEXT_PHOTO);
        }
        self.shared.run.store(0, Ordering::SeqCst);
        self.send(Msg::Stop);
        self.freeze();
        host.want_frames(NAME, false);
        if self.mode == Mode::Live {
            self.phase = Phase::Idle;
            self.note = tr("Stopped. Reset starts a new stack");
        }
        self.sync_clean_video(host);
        self.show(host);
    }

    /// Asks the phone for cleaner video (a higher bitrate, as far as the link takes it) while a run collects video frames, and gives it back
    /// after. Only a phone that says it takes it is asked; the bitrate changes on the running stream, so there's no reconnect.
    fn sync_clean_video(&mut self, host: &mut dyn Host) {
        let wanted = (self.phase == Phase::Collecting && self.stack_input == Input::Video && !self.down && !self.source.is_empty()).then(|| self.source.clone());
        if wanted == self.clean_for {
            return;
        }
        if let Some(old) = self.clean_for.take() {
            host.phone_control(Some(&old), &json!({"action": "clean_video", "value": 0}));
        }
        let Some(source) = wanted else { return };
        let takes = host.phone_state(Some(&source)).is_some_and(|s| s.get("clean_video_toggle") == Some(&Value::Bool(true)));
        if takes {
            host.phone_control(Some(&source), &json!({"action": "clean_video", "value": 1}));
            self.clean_for = Some(source);
        }
    }

    fn ended(&mut self, host: &mut dyn Host) {
        self.freeze();
        self.phase = Phase::Idle;
        self.photo = None;
        host.want_frames(NAME, false);
        host.cancel_schedule(TICK);
        host.cancel_schedule(NEXT_PHOTO);
        self.sync_clean_video(host);
    }

    fn heard(&mut self, host: &mut dyn Host, token: &str, data: &Value) {
        let number = |key: &str| data.get(key).and_then(Value::as_u64).unwrap_or(0) as usize;
        if token == "focus" {
            let now = data.get("now").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            self.focus = Some(now);
            self.best = self.best.max(now);
            host.set_ui_props("card", &json!({"focus-text": self.focus_text(host)}));
            return;
        }
        if data.get("run").and_then(Value::as_u64) != Some(self.run) {
            return;
        }
        let last = data.get("final").and_then(Value::as_bool).unwrap_or(false);
        match token {
            "progress" => {
                if data.get("reset") == Some(&Value::Bool(true)) {
                    (self.note, self.problem) = (String::new(), None);
                }
                (self.collected, self.target, self.count, self.refused) = (number("collected"), number("target"), number("count"), number("refused"));
                if data.get("stacking") == Some(&Value::Bool(true)) && self.phase == Phase::Collecting {
                    self.phase = Phase::Stacking;
                }
            }
            "saved" => {
                self.last = data.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                self.unsaved = false;
                if !self.photo_problem {
                    self.problem = None;
                }
                let frames = number("frames");
                if last {
                    (self.count, self.refused) = (frames, number("refused"));
                    self.ended(host);
                }
                let partial = data.get("partial") == Some(&Value::Bool(true));
                self.note = match (partial, self.mode) {
                    (true, _) => trn("Stopped early and stacked the sharpest {n} frame it had", "Stopped early and stacked the sharpest {n} frames it had", frames as i64),
                    (_, Mode::Trails) => tr("Saved the trails"),
                    _ => trn("Saved a stack of {n} frame", "Saved a stack of {n} frames", frames as i64),
                };
            }
            "ended" => {
                self.ended(host);
                if data.get("few") == Some(&Value::Bool(true)) {
                    self.note = tr("Stopped before enough frames came in. Nothing was saved");
                }
            }
            "failed" => {
                if last {
                    self.ended(host);
                }
                let error = data.get("error").and_then(Value::as_str).unwrap_or("");
                log::warn!("Stargazer: {error}");
                self.problem = Some(tr("Couldn't save the picture"));
                if data.get("retry") == Some(&Value::Bool(true)) {
                    self.unsaved = true;
                }
            }
            "photo" => return self.photo_answered(host, data),
            "working" => {
                let number = |key: &str| number(key).to_string();
                let (width, height) = (number("width"), number("height"));
                self.limit = if data.get("width") == data.get("from_width") && data.get("height") == data.get("from_height") {
                    tr("Stacking at {width} x {height}").field("width", width).field("height", height)
                } else {
                    tr("Stacking at {width} x {height} to save memory").field("width", width).field("height", height)
                };
            }
            "capped" => self.limit = trn("Memory limits this run to the sharpest {n} frame", "Memory limits this run to the sharpest {n} frames", number("kept") as i64),
            "resized" => self.note = tr("Stopped because the picture size changed"),
            _ => {}
        }
        self.show(host);
    }

    fn set_number(&mut self, host: &mut dyn Host, which: &str, value: f64) {
        let v = value.round() as u32;
        match which {
            "frames" => self.frames = v.clamp(MIN_FRAMES, MAX_FRAMES),
            "keep" => self.keep = v.clamp(1, 100),
            "photo-count" => self.photo_count = v.clamp(1, MAX_PHOTOS),
            "photo-every" => self.photo_every = v.min(MAX_PHOTO_EVERY),
            _ => {
                self.trail_s = v.clamp(1, MAX_TRAIL_S);
                self.shared.interval_ms.store(u64::from(self.trail_s) * 1000, Ordering::SeqCst);
            }
        }
        self.show(host);
        host.schedule_save();
    }

    fn pick_mode(&mut self, host: &mut dyn Host, index: usize) {
        let Some(&mode) = Mode::ALL.get(index) else { return };
        if self.phase != Phase::Idle || mode == self.mode {
            self.show(host);
            return;
        }
        // Lucky imaging needs video, so choosing it brings the frames back to the video
        let input = if self.input.suits(mode) { self.input } else { Input::Video };
        self.change_mode(mode, input);
        self.settle_default_shot(host);
        self.show(host);
        host.schedule_save();
    }

    fn pick_input(&mut self, host: &mut dyn Host, index: usize) {
        let Some(&input) = Input::ALL.get(index) else { return };
        if self.phase != Phase::Idle || input == self.input || !input.suits(self.mode) {
            self.show(host);
            return;
        }
        self.change_mode(self.mode, input);
        self.settle_default_shot(host);
        self.show(host);
        host.schedule_save();
    }

    /// Another mode or another source of frames starts from nothing: the worker and the counters forget the old stack. Only while idle. The first
    /// time photos are picked they start at a long exposure, which a saved choice is never replaced by.
    fn change_mode(&mut self, mode: Mode, input: Input) {
        if input == Input::Photos && !self.shot_initialized {
            self.shot = Self::default_shot();
            self.shot_initialized = true;
            self.shot_default = true;
        }
        (self.mode, self.input) = (mode, input);
        self.shared.mode.store(mode.index() as u8, Ordering::SeqCst);
        self.shared.input.store(input.index() as u8, Ordering::SeqCst);
        self.run += 1;
        if let Some(feed) = &self.feed {
            feed.send(Msg::Reset { run: self.run });
        }
        (self.collected, self.target, self.count, self.refused, self.banked) = (0, 0, 0, 0, 0.0);
        (self.note, self.problem, self.photo_problem, self.limit, self.unsaved) = (String::new(), None, false, String::new(), false);
        self.warning = None;
    }

    fn reset(&mut self, host: &mut dyn Host) {
        if self.mode != Mode::Live {
            return;
        }
        if self.phase == Phase::Idle {
            self.run += 1;
        }
        self.send(Msg::Reset { run: self.run });
        (self.collected, self.target, self.count, self.refused, self.banked) = (0, 0, 0, 0, 0.0);
        (self.note, self.problem, self.unsaved) = (String::new(), None, false);
        self.began = self.now();
        self.show(host);
    }

    /// The mode and where the frames come from are the person's picks (switching one ends a stack) and the camera sends the stack out, so the card's Reset leaves all three.
    const SETTINGS_KEPT: &'static [&'static str] = &["mode", "input", "camera"];

    /// Locked during a run, like the sliders it puts back.
    fn settings_reset_shown(&self) -> bool {
        let mut defaults = StargazerPlugin::new().get_config();
        if self.input == Input::Photos { defaults["shot"] = Self::default_shot().to_options_value(); }
        self.phase == Phase::Idle && off_default(&self.get_config(), &defaults, Self::SETTINGS_KEPT)
    }

    fn reset_settings(&mut self, host: &mut dyn Host) {
        if self.phase != Phase::Idle {
            return;
        }
        let cfg = reset_config(&self.get_config(), &StargazerPlugin::new().get_config(), Self::SETTINGS_KEPT);
        if self.set_config(&cfg).is_ok() {
            self.pending = false;
            self.show(host);
            host.schedule_save();
        }
    }

    fn set_camera(&mut self, host: &mut dyn Host, on: bool) {
        self.camera = on;
        self.shared.camera.store(on, Ordering::SeqCst);
        if on {
            self.send(Msg::Publish);
        } else {
            *self.shared.result.lock().unwrap() = None;
        }
        self.show(host);
        host.schedule_save();
    }

    fn open_folder(&self, host: &mut dyn Host) {
        let folder = Path::new(&self.last).parent().map_or_else(|| PathBuf::from(&self.folder), Path::to_path_buf);
        host.open_url(&telescope_core::system::file_url(&folder));
    }
}

impl Plugin for StargazerPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Stargazer").into(),
            about: n_("Stack frames from a phone clipped to a telescope into a sharper moon or planet, a clearer faint sky or a star trail.").into(),
            when_off: n_("Without the Stargazer plugin there's no Stargazer card and no frames are stacked.").into(),
            category: "camera".into(),
            icon: "star".into(),
            frame_stage: FrameStage::Effects,
            optional: true,
            lab: true,
            in_presets: true,
            device_local: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.poster = host.poster();
        self.catch_up(host);
        let props = self.all_props(host);
        host.set_ui_props("card", &props);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        if let Some(request) = self.taking.take() {
            host.cancel_still(request);
        }
        self.shared.run.store(0, Ordering::SeqCst);
        self.shared.camera.store(false, Ordering::SeqCst);
        // Switching the plugin off restarts the app, so the worker gets to finish a save in progress (a picture not done in time never has its real name)
        if let Some(feed) = self.feed.take() {
            if self.phase == Phase::Collecting {
                feed.send(Msg::Stop);
            }
            feed.send(Msg::Quit);
        }
        if let Some(worker) = self.worker.take() {
            let deadline = Instant::now() + SHUTDOWN_WAIT;
            while !worker.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
        self.ticking = false;
        self.phase = Phase::Idle;
        self.sync_clean_video(host);
        self.taking = None;
        self.photo = None;
        host.want_frames(NAME, false);
        host.cancel_schedule(TICK);
        host.cancel_schedule(NEXT_PHOTO);
        self.poster = None;
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.down = false;
        self.best = 0.0;
        // Another phone's numbers don't belong on this one; its live stack can still be saved
        if self.phase == Phase::Idle && !self.source.is_empty() && stream.source_id != self.source {
            self.unsaved |= self.mode == Mode::Live && self.count > 0;
            (self.collected, self.target, self.count, self.refused, self.banked) = (0, 0, 0, 0, 0.0);
        }
        self.show(host);
    }

    /// Also what moving off the stream onto another does: a run is of one stream and ends the way the stream ending ends it.
    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.focus = None;
        self.stop(host);
        self.show(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        self.refresh_hidden(host, false);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        match event {
            Event::RequestProgress { data, .. } if data.get("state").and_then(Value::as_str) == Some("late") && data.get("code").and_then(Value::as_str) == Some("restore") => {
                self.problem = Some(data.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()).map_or_else(|| tr("Couldn't restore the camera settings."), String::from));
                self.show(host);
            }
            Event::RequestProgress { request, data } if Some(*request) == self.taking => {
                self.exposing = data.get("state").and_then(Value::as_str) == Some("capturing");
                self.show(host);
            }
            Event::RequestDone { request, ok, data, .. } if Some(*request) == self.taking => self.photo_done(host, *ok, data),
            Event::PhoneStateUpdated { .. } | Event::DeviceChanged { .. } | Event::CameraSwitched { .. }
            | Event::StillCapsChanged { .. } | Event::StillBusyChanged { .. } | Event::SourceSelected { .. } => self.show(host),
            Event::WindowShown { shown } => self.shared.window_hidden.store(!shown, Ordering::SeqCst),
            Event::CardFolded { key, folded } if key == "stargazer/Stargazer" => self.shared.card_folded.store(*folded, Ordering::SeqCst),
            Event::Posted { owner, token, data } if owner == NAME => self.heard(host, token, data),
            Event::TimerFired { owner, token } if owner == NAME && token == NEXT_PHOTO => self.next_photo(host),
            Event::TimerFired { owner, token } if owner == NAME && token == TICK => {
                self.refresh_hidden(host, true);
                if self.phase != Phase::Idle {
                    host.schedule(1000, TICK);
                    host.set_ui_props("card", &json!({"tally": self.tally()}));
                }
            }
            Event::StreamStopped => {
                self.down = true;
                self.focus = None;
                self.stop(host);
                self.show(host);
            }
            Event::StreamStarted { .. } | Event::StreamConnected { .. } => {
                self.down = false;
                self.best = 0.0;
                self.show(host);
            }
            _ => {}
        }
        self.sync_clean_video(host);
    }

    fn card(&mut self) -> Option<CardSpec> {
        let mut props = Self::fixed_props();
        if let (Value::Object(a), Value::Object(s)) = (&mut props, json!({
            "active": self.paired.on(), "frames": self.frames, "frames-text": tr("{n} frames").field("n", self.frames), "keep": self.keep,
            "keep-text": format!("{} %", self.keep), "trail": self.trail_s, "trail-text": tr("{seconds} s").field("seconds", self.trail_s),
            "run-text": tr("Start"), "status": tr("Waiting for the picture"), "status-kind": "status_dim", "focus-text": tr("Waiting for the picture"),
            "mode-index": self.mode.index(), "input-index": self.input.index(), "photo-count": self.photo_count, "photo-count-text": self.photo_count_text(),
            "photo-every": self.photo_every, "photo-every-text": self.photo_every_text(), "shot-summary": self.shot.summary(stills::shot_focus_text), "customize-open": self.customize_open,
            "shutter-stops": [tr("Camera")], "iso-stops": [tr("Camera")],
            "focus-unset": self.shot.focus_diopters.is_none(), "focus-readout": self.shot.focus_diopters.map_or_else(|| tr("Camera"), stills::shot_focus_text),
            "shutter-tip": tr("Waiting for the picture"), "iso-tip": tr("Waiting for the picture"), "focus-tip": tr("Waiting for the picture"),
        })) {
            a.extend(s);
        }
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "StargazerCard".into(), props }, region: "right".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let flag = event.args.first().and_then(Value::as_bool).unwrap_or(false);
        let number = event.args.first().and_then(Value::as_f64).unwrap_or(0.0);
        match event.callback.as_str() {
            "mode-picked" => self.pick_mode(host, number.max(0.0) as usize),
            "input-picked" => self.pick_input(host, number.max(0.0) as usize),
            "frames-moved" => self.set_number(host, "frames", number),
            "keep-moved" => self.set_number(host, "keep", number),
            "trail-moved" => self.set_number(host, "trail", number),
            "photo-count-moved" if self.phase == Phase::Idle => self.set_number(host, "photo-count", number),
            "photo-every-moved" if self.phase == Phase::Idle => self.set_number(host, "photo-every", number),
            "run-clicked" => {
                if self.phase == Phase::Collecting { self.stop(host) } else { self.start(host) }
            }
            "save-clicked" => {
                if self.can_save() {
                    self.send(Msg::Save);
                }
            }
            "reset-clicked" => self.reset(host),
            "settings-reset-clicked" => self.reset_settings(host),
            "camera-toggled" => self.set_camera(host, flag),
            "use-zoom-toggled" if self.phase == Phase::Idle => {
                self.use_zoom = flag;
                self.show(host);
                host.schedule_save();
            }
            "customize-toggled" => {
                self.customize_open = flag;
                self.show(host);
            }
            "shutter-picked" | "iso-picked" => self.pick_shot(host, &event.callback, number.max(0.0) as usize),
            "focus-moved" => self.set_focus(host, Some(number)),
            "focus-reset" => self.set_focus(host, None),
            "open-clicked" => self.open_folder(host),
            _ => {}
        }
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        let feed = self.feed()?;
        Some(FrameStep::Cpu(Box::new(Handoff { shared: self.shared.clone(), feed })))
    }

    /// A run isn't saved, so a restart never begins stacking.
    fn get_config(&self) -> Value {
        let mut cfg = json!({
            "mode": self.mode.key(), "input": self.input.key(), "frames": self.frames, "keep": self.keep, "interval": self.trail_s,
            "photo_count": self.photo_count, "photo_every": self.photo_every, "camera": self.camera, "use_zoom": self.use_zoom,
        });
        if self.shot_initialized { cfg["shot"] = self.shot.to_options_value(); }
        cfg
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let cfg = &migrated(cfg);
        let number = |key: &str, default: u32, low: u32, high: u32| {
            cfg.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(default, |v| (v.round() as i64).clamp(i64::from(low), i64::from(high)) as u32)
        };
        self.mode = cfg.get("mode").and_then(Value::as_str).and_then(Mode::from_key).unwrap_or(Mode::Lucky);
        // Lucky imaging is video only, whatever a settings file says
        self.input = cfg.get("input").and_then(Value::as_str).and_then(Input::from_key).filter(|i| i.suits(self.mode)).unwrap_or(Input::Video);
        self.frames = number("frames", DEFAULT_FRAMES, MIN_FRAMES, MAX_FRAMES);
        self.keep = number("keep", DEFAULT_KEEP, 1, 100);
        self.trail_s = number("interval", DEFAULT_TRAIL_S, 1, MAX_TRAIL_S);
        self.photo_count = number("photo_count", DEFAULT_PHOTOS, 1, MAX_PHOTOS);
        self.photo_every = number("photo_every", 0, 0, MAX_PHOTO_EVERY);
        self.camera = cfg.get("camera") == Some(&Value::Bool(true));
        self.use_zoom = cfg.get("use_zoom") == Some(&Value::Bool(true));
        self.shot_initialized = cfg.get("shot").is_some() || self.input == Input::Photos;
        self.shot_default = cfg.get("shot").is_none() && self.input == Input::Photos;
        self.shot = cfg.get("shot").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_else(|| {
            if self.input == Input::Photos { Self::default_shot() } else { ShotChoice::default() }
        });
        self.shared.mode.store(self.mode.index() as u8, Ordering::SeqCst);
        self.shared.input.store(self.input.index() as u8, Ordering::SeqCst);
        self.shared.interval_ms.store(u64::from(self.trail_s) * 1000, Ordering::SeqCst);
        self.shared.camera.store(self.camera, Ordering::SeqCst);
        self.pending = true;
        Ok(())
    }

    /// A key the preset lacks keeps its value. A recall never begins a run and leaves the mode alone during one.
    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        let Value::Object(cfg) = migrated(cfg) else { return Err(ConfigRejected("settings".into())) };
        let (mode, input, camera) = (self.mode, self.input, self.camera);
        let mut merged = self.get_config();
        for (key, value) in &cfg {
            merged[key] = value.clone();
        }
        let wanted_mode = merged["mode"].as_str().and_then(Mode::from_key).unwrap_or(Mode::Lucky);
        let wanted_input = merged["input"].as_str().and_then(Input::from_key).filter(|i| i.suits(wanted_mode)).unwrap_or(Input::Video);
        let wanted_camera = merged["camera"] == Value::Bool(true);
        // The mode, where the frames come from and the camera go through their own paths below, as when they're picked on the card
        merged["mode"] = json!(mode.key());
        merged["input"] = json!(input.key());
        merged["camera"] = json!(camera);
        self.set_config(&merged)?;
        if (wanted_mode, wanted_input) != (mode, input) && self.phase == Phase::Idle {
            self.change_mode(wanted_mode, wanted_input);
        }
        if wanted_camera != camera {
            self.set_camera(host, wanted_camera);
        }
        self.pending = true;
        self.catch_up(host);
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("Stargazer".into(), format!("{}, {}, {}", self.mode.key(), self.input.key(), if self.phase == Phase::Idle { "idle" } else { "running" }))]
    }
}

#[cfg(test)]
#[path = "stargazer_tests.rs"]
mod tests;
