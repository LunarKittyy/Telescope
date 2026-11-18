//! The Stargazer plugin: stacks frames from the stream for a phone clipped to a telescope. Lucky imaging keeps the sharpest share of a burst
//! (moon, planets), the live stack adds every frame with outliers clipped (faint things) and star trails keep the brightest of one frame
//! every few seconds. All stacking happens on a worker thread of its own; the frame step only hands pictures over.

use crate::shared::Paired;
use crate::stills::default_folder;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat, Touch};
use telescope_core::host::{Host, Poster};
use telescope_core::i18n::{n_, tr, trn, Fields};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::stack::{sharpness, Lucky, Plane, StackMode, Stacker};
use telescope_core::time::{local_stamp, monotonic_ns};
use telescope_core::ui::{CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "stargazer";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/stargazer.slint"));

pub const DEFAULT_FRAMES: u32 = 300;
pub const MIN_FRAMES: u32 = 50;
pub const MAX_FRAMES: u32 = 2000;
pub const DEFAULT_KEEP: u32 = 10;
pub const DEFAULT_TRAIL_S: u32 = 5;
const MAX_TRAIL_S: u32 = 60;
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
const TICK: &str = "tick";
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
    mode: AtomicU8,
    /// Star trails take a frame this often.
    interval_ms: AtomicU64,
    camera: AtomicBool,
    /// Pictures handed over that the worker hasn't taken yet.
    queued: AtomicUsize,
    /// The stream the panels show; the others are never looked at.
    focused: Mutex<String>,
    last_hand: Mutex<Option<Instant>>,
    /// The latest stack, for the camera.
    result: Mutex<Option<Frame>>,
}

impl Shared {
    fn mode(&self) -> Mode {
        Mode::ALL.get(usize::from(self.mode.load(Ordering::SeqCst))).copied().unwrap_or(Mode::Lucky)
    }
}

/// What a run needs to know when it starts.
struct Setup {
    run: u64,
    mode: Mode,
    frames: usize,
    keep: usize,
    folder: PathBuf,
    /// A live stack that's still there goes on instead of starting over.
    fresh: bool,
}

enum Msg {
    Frame(Frame),
    Start(Setup),
    Stop,
    Save,
    Reset,
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
    fn hand(&self, frame: Frame) -> bool {
        if self.shared.queued.fetch_add(1, Ordering::SeqCst) >= QUEUE {
            self.shared.queued.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        let sent = self.send(Msg::Frame(frame));
        if !sent {
            self.shared.queued.fetch_sub(1, Ordering::SeqCst);
        }
        sent
    }
}

// ── The worker ───────────────────────────────────────────────────────────────

struct Job {
    run: u64,
    mode: Mode,
    active: bool,
    folder: PathBuf,
    target: usize,
    keep: usize,
    capacity: usize,
    lucky: Option<Lucky>,
    kept: usize,
    seen: usize,
    stacker: Option<Stacker>,
    refused: usize,
}

impl Job {
    fn new(setup: &Setup) -> Job {
        let capacity = (setup.frames * setup.keep).div_ceil(100).max(1);
        let stacker = match setup.mode {
            Mode::Lucky => None,
            Mode::Live => Some(Stacker::new(StackMode::Clip { kappa: KAPPA })),
            Mode::Trails => {
                let mut stacker = Stacker::new(StackMode::Max);
                stacker.set_align(false);
                Some(stacker)
            }
        };
        let lucky = (setup.mode == Mode::Lucky).then(|| Lucky::new(capacity));
        Job { run: setup.run, mode: setup.mode, active: true, folder: setup.folder.clone(), target: setup.frames, keep: setup.keep, capacity, lucky, kept: 0, seen: 0, stacker, refused: 0 }
    }

    /// Frames in the stack (a lucky run's kept frames until they are stacked).
    fn count(&self) -> usize {
        self.stacker.as_ref().map_or(self.kept, Stacker::count)
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
    let mut path = folder.join(format!("stargazer_{stamp}.png"));
    let mut n = 2;
    while path.exists() {
        path = folder.join(format!("stargazer_{stamp}_{n}.png"));
        n += 1;
    }
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
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
            Msg::Frame(frame) => {
                self.shared.queued.fetch_sub(1, Ordering::SeqCst);
                self.frame(frame);
            }
            Msg::Start(setup) => self.start(setup),
            Msg::Stop => self.stop(),
            Msg::Save => self.save_live(),
            Msg::Reset => self.reset(),
            Msg::Publish => self.publish(true),
            Msg::Quit => {}
        }
    }

    fn start(&mut self, setup: Setup) {
        let live_goes_on = !setup.fresh && setup.mode == Mode::Live && self.job.as_ref().is_some_and(|j| j.mode == Mode::Live && j.stacker.is_some());
        if live_goes_on {
            if let Some(job) = self.job.as_mut() {
                job.run = setup.run;
                job.active = true;
            }
        } else {
            self.job = Some(Job::new(&setup));
            *self.shared.result.lock().unwrap() = None;
        }
        self.progress(true);
    }

    fn frame(&mut self, frame: Frame) {
        if self.last_focus.is_none_or(|at| at.elapsed() >= FOCUS_EVERY) {
            self.last_focus = Some(Instant::now());
            let plane = Plane::from_frame(&frame, (frame.width as usize).div_ceil(1024));
            self.post("focus", json!({"now": plane.map_or(0.0, |p| sharpness(&p))}));
        }
        let run = self.shared.run.load(Ordering::SeqCst);
        let Some(job) = self.job.as_mut().filter(|j| j.active && j.run == run) else { return };
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
        let mut frames = job.lucky.take().map(|l| l.best()).unwrap_or_default();
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
            Err(error) => self.post("failed", json!({"run": run, "final": last, "error": error})),
        }
    }

    fn stop(&mut self) {
        let Some(job) = self.job.as_mut().filter(|j| j.active) else { return };
        let (run, mode) = (job.run, job.mode);
        match mode {
            Mode::Lucky if job.kept >= MIN_LUCKY => self.finish_lucky(true),
            Mode::Trails if job.stacker.as_ref().is_some_and(|s| s.count() >= 2) => {
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

    fn reset(&mut self) {
        if let Some(job) = self.job.as_mut() {
            if job.active && job.mode == Mode::Live {
                job.stacker = Some(Stacker::new(StackMode::Clip { kappa: KAPPA }));
                job.refused = 0;
            } else {
                self.job = None;
            }
        }
        *self.shared.result.lock().unwrap() = None;
        let run = self.job.as_ref().map_or(0, |j| j.run);
        self.post("progress", json!({"run": run, "stacking": false, "collected": 0, "target": 0, "count": 0, "refused": 0, "reset": true}));
    }

    fn publish_frame(&self, result: &Frame) {
        if self.shared.camera.load(Ordering::SeqCst) {
            *self.shared.result.lock().unwrap() = Some(result.clone());
        }
    }

    /// Puts the stack where the camera step finds it, about once a second.
    fn publish(&mut self, force: bool) {
        if !self.shared.camera.load(Ordering::SeqCst) || (!force && self.last_publish.is_some_and(|at| at.elapsed() < CAMERA_EVERY)) {
            return;
        }
        if let Some(result) = self.job.as_ref().and_then(|j| j.stacker.as_ref()).and_then(Stacker::result) {
            self.last_publish = Some(Instant::now());
            *self.shared.result.lock().unwrap() = Some(result);
        }
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
    fn shown(&self, source_id: &str) -> bool {
        *self.shared.focused.lock().unwrap() == source_id
    }

    /// Whether the worker wants another picture of this stream now.
    fn due(&self, now: Instant) -> bool {
        if self.shared.queued.load(Ordering::SeqCst) >= QUEUE {
            return false;
        }
        let wait = match (self.shared.run.load(Ordering::SeqCst), self.shared.mode()) {
            (0, _) => FOCUS_EVERY,
            (_, Mode::Trails) => Duration::from_millis(self.shared.interval_ms.load(Ordering::SeqCst)),
            _ => Duration::ZERO,
        };
        !self.shared.last_hand.lock().unwrap().is_some_and(|at| now.duration_since(at) < wait)
    }

    fn stack(&self) -> Option<Frame> {
        if !self.shared.camera.load(Ordering::SeqCst) {
            return None;
        }
        self.shared.result.lock().unwrap().clone()
    }
}

impl CpuStep for Handoff {
    fn touch(&self, ctx: &dyn FrameContext) -> Touch {
        if !self.shown(ctx.source_id()) {
            Touch::Nothing
        } else if self.stack().is_some() {
            Touch::Changes
        } else if self.due(Instant::now()) {
            Touch::Reads
        } else {
            Touch::Nothing
        }
    }

    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        if !self.shown(ctx.source_id()) {
            return;
        }
        let now = Instant::now();
        if self.due(now) && self.feed.hand(frame.clone()) {
            *self.shared.last_hand.lock().unwrap() = Some(now);
        }
        if let Some(mut stack) = self.stack().filter(|s| (s.width, s.height) == (frame.width, frame.height) && s.format == PixelFormat::Rgba) {
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

pub struct StargazerPlugin {
    mode: Mode,
    frames: u32,
    keep: u32,
    trail_s: u32,
    camera: bool,
    paired: Paired,
    shared: Arc<Shared>,
    poster: Option<Arc<dyn Poster>>,
    feed: Option<Feed>,
    folder: String,
    phase: Phase,
    run: u64,
    collected: usize,
    target: usize,
    count: usize,
    refused: usize,
    /// Seconds of earlier stints of a live stack, and when this one began on the clock.
    banked: f64,
    began: f64,
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
            frames: DEFAULT_FRAMES,
            keep: DEFAULT_KEEP,
            trail_s: DEFAULT_TRAIL_S,
            camera: false,
            paired: Paired::default(),
            shared,
            poster: None,
            feed: None,
            folder: Path::new(&default_folder()).join(FOLDER).display().to_string(),
            phase: Phase::Idle,
            run: 0,
            collected: 0,
            target: 0,
            count: 0,
            refused: 0,
            banked: 0.0,
            began: 0.0,
            focus: None,
            best: 0.0,
            last: String::new(),
            note: String::new(),
            problem: None,
            down: false,
            pending: false,
            clock: Box::new(|| monotonic_ns() as f64 / 1e9),
        }
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
            std::thread::Builder::new().name("stargazer".into()).spawn(move || worker.run(rx, waiting)).ok()?;
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
        !self.down && host.is_streaming()
    }

    fn elapsed(&self) -> f64 {
        self.banked + if self.phase == Phase::Idle { 0.0 } else { self.now() - self.began }
    }

    // ── What the card shows ───────────────────────────────────────────

    fn status(&self, host: &dyn Host) -> (String, &'static str) {
        match self.phase {
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
                if let Some(problem) = &self.problem {
                    (problem.clone(), "status_err")
                } else if !self.note.is_empty() {
                    (self.note.clone(), "status_ok")
                } else if !self.can_start(host) {
                    (tr("Waiting for the picture"), "status_dim")
                } else {
                    (tr("Ready"), "status_dim")
                }
            }
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

    fn fixed_props() -> Value {
        json!({"mode-items": [tr("Lucky"), tr("Live"), tr("Trails")], "save-text": tr("Save"), "reset-text": tr("Reset"),
               "camera-name": tr("Show the stack on the camera"), "open-text": tr("Open folder"), "open-tip": tr(OPEN_TIP)})
    }

    fn state_props(&self, host: &dyn Host) -> Value {
        let (status, kind) = self.status(host);
        let idle = self.phase == Phase::Idle;
        let name = Path::new(&self.last).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let saved = if name.is_empty() { String::new() } else { tr("Saved {name}").field("name", name) };
        json!({
            "active": self.paired.on(), "mode-index": self.mode.index(), "running": self.phase != Phase::Idle,
            "frames": self.frames, "frames-text": tr("{n} frames").field("n", self.frames),
            "keep": self.keep, "keep-text": format!("{} %", self.keep),
            "trail": self.trail_s, "trail-text": tr("{seconds} s").field("seconds", self.trail_s),
            "run-text": match self.phase { Phase::Idle => tr("Start"), Phase::Collecting => tr("Stop"), Phase::Stacking => tr("Stacking") },
            "run-enabled": self.phase == Phase::Collecting || (idle && self.can_start(host)),
            "save-enabled": self.mode == Mode::Live && self.count > 0,
            "reset-enabled": self.mode == Mode::Live && (self.count > 0 || self.refused > 0 || self.phase == Phase::Collecting),
            "status": status, "status-kind": kind, "tally": self.tally(), "focus-text": self.focus_text(host),
            "camera": self.camera, "saved": saved, "open-enabled": !self.last.is_empty(),
        })
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

    fn catch_up(&mut self, host: &mut dyn Host) {
        *self.shared.focused.lock().unwrap() = host.focused_source_id().unwrap_or_default();
        if self.pending {
            self.pending = false;
            let props = self.all_props(host);
            host.set_ui_props("card", &props);
        }
    }

    // ── Runs ──────────────────────────────────────────────────────────

    fn start(&mut self, host: &mut dyn Host) {
        if self.phase != Phase::Idle || !self.can_start(host) {
            return;
        }
        let Some(feed) = self.feed() else { return };
        let fresh = !(self.mode == Mode::Live && self.count > 0);
        self.run += 1;
        if fresh {
            (self.collected, self.target, self.count, self.refused, self.banked) = (0, 0, 0, 0, 0.0);
        }
        (self.note, self.problem) = (String::new(), None);
        self.phase = Phase::Collecting;
        self.began = self.now();
        self.shared.mode.store(self.mode.index() as u8, Ordering::SeqCst);
        *self.shared.last_hand.lock().unwrap() = None;
        self.shared.run.store(self.run, Ordering::SeqCst);
        let setup = Setup { run: self.run, mode: self.mode, frames: self.frames as usize, keep: self.keep as usize, folder: PathBuf::from(&self.folder), fresh };
        feed.send(Msg::Start(setup));
        host.want_frames(NAME, true);
        host.schedule(1000, TICK);
        self.show(host);
    }

    /// Ends the run: a lucky one with enough frames stacks what it has, star trails are saved, a live stack stays for saving.
    fn stop(&mut self, host: &mut dyn Host) {
        if self.phase != Phase::Collecting {
            return;
        }
        self.shared.run.store(0, Ordering::SeqCst);
        self.send(Msg::Stop);
        self.banked = self.elapsed();
        host.want_frames(NAME, false);
        if self.mode == Mode::Live {
            self.phase = Phase::Idle;
            self.note = tr("Stopped. Reset starts a new stack");
        }
        self.show(host);
    }

    fn ended(&mut self, host: &mut dyn Host) {
        if self.phase != Phase::Idle {
            self.banked = self.elapsed();
        }
        self.phase = Phase::Idle;
        host.want_frames(NAME, false);
        host.cancel_schedule(TICK);
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
                self.problem = None;
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
            }
            _ => {}
        }
        self.show(host);
    }

    fn set_number(&mut self, host: &mut dyn Host, which: &str, value: f64) {
        let v = value.round() as u32;
        match which {
            "frames" => self.frames = v.clamp(MIN_FRAMES, MAX_FRAMES),
            "keep" => self.keep = v.clamp(1, 100),
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
        self.mode = mode;
        self.send(Msg::Reset);
        (self.collected, self.target, self.count, self.refused, self.banked) = (0, 0, 0, 0, 0.0);
        (self.note, self.problem) = (String::new(), None);
        self.show(host);
        host.schedule_save();
    }

    fn reset(&mut self, host: &mut dyn Host) {
        if self.mode != Mode::Live {
            return;
        }
        self.send(Msg::Reset);
        (self.collected, self.target, self.count, self.refused, self.banked) = (0, 0, 0, 0, 0.0);
        (self.note, self.problem) = (String::new(), None);
        self.began = self.now();
        self.show(host);
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
        // The worker finishes what it was asked, a lucky run with enough frames included, and ends
        if let Some(feed) = self.feed.take() {
            if self.phase == Phase::Collecting {
                feed.send(Msg::Stop);
            }
            feed.send(Msg::Quit);
        }
        self.shared.run.store(0, Ordering::SeqCst);
        self.shared.camera.store(false, Ordering::SeqCst);
        self.phase = Phase::Idle;
        host.want_frames(NAME, false);
        host.cancel_schedule(TICK);
        self.poster = None;
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        match event {
            Event::Posted { owner, token, data } if owner == NAME => self.heard(host, token, data),
            Event::TimerFired { owner, token } if owner == NAME && token == TICK => {
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
    }

    fn card(&mut self) -> Option<CardSpec> {
        let mut props = Self::fixed_props();
        if let (Value::Object(a), Value::Object(s)) = (&mut props, json!({
            "active": self.paired.on(), "frames": self.frames, "frames-text": tr("{n} frames").field("n", self.frames), "keep": self.keep,
            "keep-text": format!("{} %", self.keep), "trail": self.trail_s, "trail-text": tr("{seconds} s").field("seconds", self.trail_s),
            "run-text": tr("Start"), "status": tr("Waiting for the picture"), "status-kind": "status_dim", "focus-text": tr("Waiting for the picture"),
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
            "frames-moved" => self.set_number(host, "frames", number),
            "keep-moved" => self.set_number(host, "keep", number),
            "trail-moved" => self.set_number(host, "trail", number),
            "run-clicked" => {
                if self.phase == Phase::Collecting { self.stop(host) } else { self.start(host) }
            }
            "save-clicked" => {
                if self.mode == Mode::Live && self.count > 0 {
                    self.send(Msg::Save);
                }
            }
            "reset-clicked" => self.reset(host),
            "camera-toggled" => self.set_camera(host, flag),
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
        json!({"mode": self.mode.key(), "frames": self.frames, "keep": self.keep, "interval": self.trail_s, "camera": self.camera})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let number = |key: &str, default: u32, low: u32, high: u32| {
            cfg.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(default, |v| (v.round() as i64).clamp(i64::from(low), i64::from(high)) as u32)
        };
        self.mode = cfg.get("mode").and_then(Value::as_str).and_then(Mode::from_key).unwrap_or(Mode::Lucky);
        self.frames = number("frames", DEFAULT_FRAMES, MIN_FRAMES, MAX_FRAMES);
        self.keep = number("keep", DEFAULT_KEEP, 1, 100);
        self.trail_s = number("interval", DEFAULT_TRAIL_S, 1, MAX_TRAIL_S);
        self.camera = cfg.get("camera") == Some(&Value::Bool(true));
        self.shared.mode.store(self.mode.index() as u8, Ordering::SeqCst);
        self.shared.interval_ms.store(u64::from(self.trail_s) * 1000, Ordering::SeqCst);
        self.shared.camera.store(self.camera, Ordering::SeqCst);
        self.pending = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("Stargazer".into(), format!("{}, {}", self.mode.key(), if self.phase == Phase::Idle { "idle" } else { "running" }))]
    }
}

#[cfg(test)]
#[path = "stargazer_tests.rs"]
mod tests;
