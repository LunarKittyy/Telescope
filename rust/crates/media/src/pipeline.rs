//! A stream's frame steps, run in FRAME_STAGES order (stream_flow.pipeline and StreamWorker._process), which is never changed. CPU
//! steps run on the stream's thread and shader steps on the graphics card. The picture goes up at the first shader and stays there
//! for as long as the order allows: a skipped or idle CPU step doesn't bring it down, one that only reads brings a copy down, and
//! only one that changes the pixels makes the next shader upload again. Transfers are counted here; see transfers.rs. A step that can
//! run either way goes where core::balance says, which hears from here what each frame waited for the card.

use crate::gpu::{self, Gpu, Held, MaskPlane};
use crate::transfers;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use telescope_core::balance::{Balance, Lane, Place};
use telescope_core::image::SparePool;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat, Touch};
use crate::surface_fit::{self, SurfacePicture};
use telescope_core::media::{unpack_camera_fit, FramePipeline, PipelineFrame, StagedStep};
use telescope_core::stats::{SourceStats, Timer};
use telescope_core::time::{monotonic_ns, span};

enum Finish { Ram, Card }

/// A plugin step that fails this many frames in a row is skipped until the next stream.
pub const STEP_FAILS_BEFORE_SKIP: u32 = 30;

enum Kind {
    Cpu(Box<dyn CpuStep>),
    Shader(gpu::ShaderSlot),
    Either(gpu::ShaderSlot, Box<dyn CpuStep>),
}

impl Kind {
    fn of(step: FrameStep) -> Kind {
        match step {
            FrameStep::Cpu(c) => Kind::Cpu(c),
            FrameStep::Shader(s) => Kind::Shader(gpu::ShaderSlot::new(s)),
            FrameStep::Either(s, c) => Kind::Either(gpu::ShaderSlot::new(s), c),
        }
    }

    /// Where it can run, for the log.
    fn label(&self) -> &'static str {
        match self {
            Kind::Cpu(_) => "cpu",
            Kind::Shader(_) => "card only",
            Kind::Either(..) => "either",
        }
    }

    fn cpu(&mut self) -> &mut Box<dyn CpuStep> {
        match self {
            Kind::Cpu(step) | Kind::Either(_, step) => step,
            Kind::Shader(_) => unreachable!("a shader has no CPU step"),
        }
    }

    fn slot_ref(&self) -> Option<&gpu::ShaderSlot> {
        match self {
            Kind::Shader(slot) | Kind::Either(slot, _) => Some(slot),
            Kind::Cpu(_) => None,
        }
    }

    fn slot(&mut self) -> &mut gpu::ShaderSlot {
        match self {
            Kind::Shader(slot) | Kind::Either(slot, _) => slot,
            Kind::Cpu(_) => unreachable!("a CPU step has no shader"),
        }
    }

    fn slot_mut(&mut self) -> Option<&mut gpu::ShaderSlot> {
        match self {
            Kind::Shader(slot) | Kind::Either(slot, _) => Some(slot),
            Kind::Cpu(_) => None,
        }
    }
}

/// New steps take over the shaders the steps they replace (the same owner and stage) built, so one made again every frame builds its shader once.
fn adopt_shaders(fresh: &mut [Entry], old: &mut [Entry]) {
    for entry in fresh {
        let Some(slot) = entry.kind.slot_mut() else { continue };
        for before in old.iter_mut().filter(|o| o.owner == entry.owner && o.stage == entry.stage) {
            let no_shader = before.no_shader;
            if before.kind.slot_mut().is_some_and(|o| slot.adopt(o)) {
                entry.no_shader = no_shader;
                break;
            }
        }
    }
}

struct Entry {
    owner: String,
    stage: FrameStage,
    kind: Kind,
    fails: u32,
    /// Its shader won't build here, so a step with a CPU way stays on that.
    no_shader: bool,
    /// Its CPU half has no pictures for the shader (it said so on the first ask), so it isn't asked again.
    no_textures: bool,
    /// Its CPU half has given some, so the frame comes down for it before each ask.
    has_textures: bool,
    /// What it costs the stream's thread, filled while the stats ask.
    timer: Option<Arc<Timer>>,
    /// Which shader and settings it was made with (a step with a card half), to tell it from the one a frame already went through.
    sig: Option<u64>,
}

impl Entry {
    fn new(owner: String, stage: FrameStage, step: FrameStep, timer: Option<Arc<Timer>>) -> Entry {
        let kind = Kind::of(step);
        Entry { owner, stage, sig: kind.slot_ref().map(|s| s.signature()), kind, fails: 0, no_shader: false, no_textures: false, has_textures: false, timer }
    }

    fn skipped(&self) -> bool {
        self.fails >= STEP_FAILS_BEFORE_SKIP
    }

    /// Whether the cut-out a frame carries (Frame::alpha) no longer fits after this step ran: an Effects or Cover CPU step that doesn't say it keeps every pixel in place, and a card-only one at any stage after Background unless it says so.
    fn drops_alpha(&self) -> bool {
        match &self.kind {
            Kind::Cpu(step) | Kind::Either(_, step) => matches!(self.stage, FrameStage::Effects | FrameStage::Cover) && !step.keeps_geometry(),
            Kind::Shader(shader) => !matches!(self.stage, FrameStage::View | FrameStage::Geometry | FrameStage::Reading | FrameStage::Background) && !shader.keeps_geometry(),
        }
    }

    /// A CPU step that says it's there to hide something, or a card-only step at a stage that hides: when it fails, what goes on is a dark frame, never the clear one.
    fn hides(&self) -> bool {
        match &self.kind {
            Kind::Cpu(step) | Kind::Either(_, step) => step.hides(),
            Kind::Shader(_) => self.stage.hides(),
        }
    }

    /// Whether this step, run on the card, still has the picture brought into memory first for the pictures its shader samples
    /// (card_textures), so a decoder's copy off the card is paid whichever way it runs.
    fn textures_from_memory(&self) -> bool {
        let Kind::Either(_, cpu) = &self.kind else { return false };
        !self.no_textures && (self.has_textures || catch_unwind(AssertUnwindSafe(|| cpu.card_textures_offered())).ok().flatten() == Some(true))
    }

    /// Whether this step puts a picture of its own over the whole frame, so what runs before it on this frame is thrown away.
    fn covers(&self) -> bool {
        !self.skipped() && match &self.kind {
            Kind::Cpu(step) | Kind::Either(_, step) => step.covers(),
            Kind::Shader(_) => false,
        }
    }

    /// Whether the frame can go without this step when a later one covers it: an Effects or Colour step that only changes pixels and keeps nothing between frames (no history, no mask, no count).
    fn pixels_only(&self) -> bool {
        matches!(self.stage, FrameStage::Effects | FrameStage::Colour) && match &self.kind {
            Kind::Cpu(step) => step.pixels_only(),
            Kind::Shader(slot) => !slot.keeps_history(),
            Kind::Either(slot, step) => !slot.keeps_history() && step.pixels_only(),
        }
    }

    fn failed(&mut self, why: &str) {
        self.fails += 1;
        let (hides, owner) = (self.hides(), &self.owner);
        if self.fails == 1 {
            let then = if hides { "withholding the frame" } else { "passing it through unchanged" };
            log::error!("Plugin {owner} failed on a frame ({why}); {then}");
        } else if self.fails == STEP_FAILS_BEFORE_SKIP {
            let then = if hides { "withholding every frame" } else { "skipping it" };
            log::error!("Plugin {owner} failed on {} frames in a row; {then} until the stream restarts", self.fails);
        }
    }
}

/// How many pictures' worth of RGB the shared spare pool keeps between steps.
const SPARE_POOL_FRAMES: usize = 2;

/// Frames between two looks at what a frame costs.
const TALLY_FRAMES: u32 = 300;

/// What one frame is doing on the card while the steps run.
#[derive(Default)]
struct Chain {
    held: Option<Held>,
    /// Shader steps recorded on held that nobody has read back yet, so the frame in hand is behind.
    ahead: Vec<usize>,
    ups: u32,
    downs: u32,
    /// The person mask this frame asked for and the size it was asked at: a second shader using it needn't bring the picture down.
    mask: Option<(u32, u32, Option<(u32, u32, Arc<Vec<u8>>)>)>,
    /// What it has waited for the card so far, on the landings of work that could move off it, and how many of those there were.
    waited_ns: u64,
    waits: u32,
    /// Work that could move off the card (a step with two ways, a plain stream's conversion) is on held, not landed yet: only a wait
    /// for that says where those steps should run, not one a step that only runs on the card makes on its own.
    movable: bool,
    /// How long the readbacks took, which isn't the cost of the step that happened to trigger one.
    down_ns: u64,
    /// The frame in hand was replaced by a dark one to hide the picture, so a step that could bring the picture back from its own history is left out.
    withheld: bool,
    /// held is the decoder's picture taken in and converted on the card with no step on it, still to come down.
    plain: bool,
}

impl Chain {
    /// Whether the card holds something the frame in hand doesn't have yet.
    fn pending(&self) -> bool {
        !self.ahead.is_empty() || self.plain
    }
}

/// Whether a plain stream may convert on the card: always on Linux, on Windows only with TELESCOPE_PLAIN_ON_CARD set, since it hasn't run on a real card there yet.
pub(crate) fn plain_allowed() -> bool {
    plain_gate(cfg!(windows), plain_opted_in())
}

fn plain_opted_in() -> bool {
    std::env::var_os("TELESCOPE_PLAIN_ON_CARD").is_some_and(|v| v != "0")
}

fn plain_gate(windows: bool, opted_in: bool) -> bool {
    !windows || opted_in
}

/// A step that hides, as the frame last through the steps had it: what a frame sent again is compared with.
#[derive(Clone)]
struct Applied {
    owner: String,
    stage: FrameStage,
    /// Which shader and settings, for a card-only step.
    sig: Option<u64>,
}

/// What the frame last through the steps had of a step that hides now.
#[derive(PartialEq)]
enum Seen {
    /// The same step with the same shader and settings.
    Same,
    /// The same owner's step at that stage with other settings: the picture already carries the old one's work.
    Changed,
    /// Nothing of it.
    New,
}

/// Takes the match for a step off the list: by its owner and stage, and by its shader and settings.
fn take_applied(applied: &mut Vec<Applied>, owner: &str, stage: FrameStage, sig: Option<u64>) -> Seen {
    let of = |a: &Applied| a.owner == owner && a.stage == stage;
    if let Some(i) = applied.iter().position(|a| of(a) && a.sig == sig) {
        applied.swap_remove(i);
        return Seen::Same;
    }
    match applied.iter().position(of) {
        Some(i) => {
            applied.swap_remove(i);
            Seen::Changed
        }
        None => Seen::New,
    }
}

/// Transfers over the last TALLY_FRAMES frames.
#[derive(Default)]
struct Tally {
    frames: u32,
    ups: u32,
    downs: u32,
    logged: (u32, u32),
    /// The app's mask uploads at the last look.
    masks: u64,
}

/// How many pipelines are using the card now; the last one to stop lets go of what it kept.
static ON_THE_CARD: AtomicUsize = AtomicUsize::new(0);

/// Runs one stream's steps. The graphics card is only opened once a shader step comes along.
pub struct Pipeline {
    steps: Vec<Entry>,
    gpu: Option<Option<Arc<Gpu>>>,
    started_ns: Option<u64>,
    mask: Option<MaskPlane>,
    tally: Tally,
    /// What the last frame cost: (up, down).
    last_transfers: (u32, u32),
    /// This stream's way in to the balance.
    balance: Lane,
    /// The size of the last picture through the steps.
    size: (u32, u32),
    /// The card failed a frame: steps that can run on the CPU stay there until the next stream, whatever the balance says.
    card_lost: bool,
    deferred_failed: Arc<AtomicBool>,
    stats: Option<Arc<SourceStats>>,
    /// The spare buffers every CPU step of this stream draws on.
    pool: SparePool,
    /// Whether this pipeline is counted in ON_THE_CARD.
    on_card: bool,
    /// How long the last frame spent bringing pictures down from the card.
    last_readback_ns: u64,
    /// The steps that hide as they are now, and as the frame last through the steps had them (a frame sent again is only missing the difference).
    hiding: Arc<Vec<Applied>>,
    applied: Arc<Vec<Applied>>,
    /// What the window's view steps do to the picture's geometry as of the last steps (None before any), and whether it changed since
    /// the last frame: the pictures that steps keep (denoising over time) no longer line up with the next one, so they start again.
    view_key: Option<u64>,
    forget_due: bool,
    /// Whether the first step to run does it on the card, for the stream's decoder to read (FramePipeline::surface_wish).
    surface_wish: Arc<AtomicBool>,
    /// The size the stream's camera takes while it is open, 0 while it isn't (FramePipeline::camera_fit).
    camera_fit: Arc<AtomicU64>,
    /// This stream's last submission to the card, so streams sharing the card don't wait for each other's pictures.
    pacing: gpu::Pacing,
    /// Whether the last frame ran its either-way steps off the card, so leaving the card releases cached textures once.
    was_off_card: bool,
    /// The newest trial of the card still out, and the flag that calls it off.
    trial: Option<(std::thread::JoinHandle<()>, Arc<AtomicBool>)>,
    /// Whether the stream's decoder has left pictures on the card this card can take in.
    card_decoder: bool,
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        // A trial left running would still be using the card when the process closes it (an exit while it was mid-pass crashed the driver)
        if let Some((handle, stop)) = self.trial.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
        }
        if self.on_card && ON_THE_CARD.fetch_sub(1, Ordering::SeqCst) == 1 {
            if let Some(gpu) = self.gpu.take().flatten() {
                gpu.release_idle();
            }
        }
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Pipeline::with_balance(Balance::shared())
    }
}

impl Pipeline {
    pub fn new() -> Pipeline {
        Pipeline::default()
    }

    /// One that listens to this balance instead of the app's, so tests of the card don't move each other's steps.
    pub fn with_balance(balance: Arc<Balance>) -> Pipeline {
        let pipeline = Pipeline { steps: Vec::new(), gpu: None, started_ns: None, mask: None, tally: Tally::default(), last_transfers: (0, 0), balance: balance.lane(), size: (0, 0), card_lost: false, deferred_failed: Arc::default(), stats: None, pool: SparePool::default(), on_card: false, last_readback_ns: 0, hiding: Arc::default(), applied: Arc::default(), view_key: None, forget_due: false, surface_wish: Arc::default(), camera_fit: Arc::default(), pacing: gpu::Pacing::default(), was_off_card: false, trial: None, card_decoder: false };
        // Asked before the first picture, which sizes the decoder's pool: a stream with no steps yet takes them in on the card
        pipeline.note_surface_wish();
        pipeline
    }

    /// Runs on this card instead of the app's, for tests that lose it.
    #[cfg(all(test, target_os = "linux", ffmpeg))]
    pub(crate) fn use_card(&mut self, gpu: Arc<Gpu>) {
        self.gpu = Some(Some(gpu));
    }

    /// This stream's way in to the balance, which decides where its steps that can run either way run.
    pub fn lane(&self) -> &Lane {
        &self.balance
    }

    /// How many times the newest frame went up to the card and came down, for this pipeline alone.
    pub fn last_transfers(&self) -> (u32, u32) {
        self.last_transfers
    }

    fn run(&mut self, mut frame: Frame, ctx: &dyn FrameContext, finish: Finish) -> PipelineFrame {
        if frame.mask_space.source_size == (0, 0) {
            frame.mask_space.source_size = (frame.width, frame.height);
        }
        if self.deferred_failed.load(Ordering::SeqCst) {
            self.lost_card("a deferred landing failed");
        }
        self.card_decoder |= Gpu::importable(&frame);
        // A decoder's picture still on the card is only for a first step that runs there: any other gets the picture in memory, as it always did
        // What the CPU does this frame that the card would have done, for the balance: the decoder's copy off the card (noted on the frame
        // when the card's way would have spared it) and making a surface's picture ready in memory, when the first step would have taken
        // the picture on the card
        let card_takes_it = self.steps.iter().find(|e| !e.skipped()).is_none_or(|e| matches!(e.kind, Kind::Either(..)) && !e.textures_from_memory());
        let mut cpu_ns = frame.copied_off_card().filter(|_| card_takes_it).unwrap_or(0);
        // A picture of another size costs the CPU something else
        if (frame.width, frame.height) != self.size {
            self.size = (frame.width, frame.height);
            self.balance.work_changed();
        }
        // A plain stream's picture stays on the card for a smaller camera, which takes it scaled there (surface_fit)
        let keep_fit = matches!(finish, Finish::Card) && self.fit_wanted() && self.camera_size().is_some_and(|(w, h)| frame.surface().is_some_and(|s| s.can_fit(w, h)));
        if frame.surface().is_some() && !self.starts_on_card() && !keep_fit {
            let began = monotonic_ns();
            frame = frame.materialise();
            if card_takes_it {
                cpu_ns += monotonic_ns().saturating_sub(began);
            }
        }
        if let Some(fps) = self.stats.as_ref().map(|s| s.target_fps()).filter(|fps| *fps > 0) {
            self.balance.set_interval(1_000_000_000 / fps as u64);
        }
        let started = *self.started_ns.get_or_insert_with(monotonic_ns);
        let time = monotonic_ns().saturating_sub(started) as f32 / 1e9;
        // A step that builds a new frame stays on this one: person masks are worked out once per number, and the camera's time is the same picture's
        let (number, capture_ns) = (frame.number, frame.capture_ns);
        let began = monotonic_ns();
        if std::mem::take(&mut self.forget_due) {
            self.forget_histories(ctx.source_id());
        }
        let mut chain = Chain::default();
        // Asked once, so a frame doesn't change its mind halfway
        let on_card = self.balance.place() == Place::Card;
        if let Some(stats) = &self.stats {
            stats.set_steps_on_card(on_card);
        }
        let mut off_card = false;
        // Asked once a frame: a step is only timed while the stats panel wants every plugin's time
        let timing = self.stats.as_ref().is_some_and(|s| s.steps().on());
        // A step that covers the whole picture makes what pixel-only steps before it did on this frame pointless; the cover itself always runs
        let covered_by = self.steps.iter().position(Entry::covers);
        for i in 0..self.steps.len() {
            if ctx.stopped() {
                // Stop is waiting for this worker: nothing more is done, and what's left goes out dark in case it is published after all
                frame = neutral((frame.width, frame.height), frame.format, &frame);
                self.drop_card_work(&mut chain);
                break;
            }
            if self.steps[i].skipped() {
                if self.steps[i].hides() {
                    // Skipped for failing too often, which can't mean the room shows: the card's copy goes with the picture
                    frame = neutral((frame.width, frame.height), frame.format, &frame);
                    // Dropped, not brought down: the card's copy has the picture this one stands in for, and landing it would put that back
                    self.drop_card_work(&mut chain);
                    chain.withheld = true;
                    self.tell_withheld(i, ctx);
                }
                continue;
            }
            if chain.withheld && self.restores_pixels(i) {
                continue;
            }
            if covered_by.is_some_and(|c| i < c) && self.steps[i].pixels_only() {
                continue;
            }
            let geometry = frame.geometry.clone();
            let mut mask_space = frame.mask_space;
            let step_began = timing.then(monotonic_ns);
            let down_before = chain.down_ns;
            let either = matches!(self.steps[i].kind, Kind::Either(..)) && !self.steps[i].no_shader && !self.card_lost;
            if either && !(on_card && self.gpu().is_some()) {
                self.ready_on_card(i);
                let began = monotonic_ns();
                self.run_cpu(i, &mut frame, &mut chain, ctx);
                // Bringing down what a step that only runs on the card made is the card's, whichever way this one runs
                cpu_ns += monotonic_ns().saturating_sub(began).saturating_sub(chain.down_ns - down_before);
                off_card = true;
            } else if either || matches!(self.steps[i].kind, Kind::Shader(_)) {
                chain.movable |= either;
                self.run_shader(i, &mut frame, &mut chain, ctx, time);
            } else {
                self.run_cpu(i, &mut frame, &mut chain, ctx);
            }
            if let (Some(began), Some(timer)) = (step_began, &self.steps[i].timer) {
                // Bringing the picture down from the card is the card's cost (the readback figure), not this step's
                timer.add(monotonic_ns().saturating_sub(began).saturating_sub(chain.down_ns - down_before));
            }
            frame.number = number;
            frame.capture_ns = capture_ns;
            if self.steps[i].stage != FrameStage::View {
                frame.geometry = geometry;
            }
            if self.steps[i].stage == FrameStage::Geometry {
                use std::hash::{Hash, Hasher};
                let key = match &self.steps[i].kind {
                    Kind::Cpu(step) | Kind::Either(_, step) => step.geometry_key(),
                    Kind::Shader(slot) => slot.geometry_key(),
                };
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                (mask_space.warp, frame.width, frame.height, key).hash(&mut hash);
                if let Some(g) = frame.geometry.as_deref() {
                    [g.zoom, g.pan_x, g.pan_y, g.stretch_x, g.stretch_y, g.distortion].map(f64::to_bits).hash(&mut hash);
                    (g.flip_h, g.flip_v, g.rotation).hash(&mut hash);
                }
                if key == 0 {
                    number.hash(&mut hash);
                }
                mask_space.warp = Some(hash.finish());
            }
            frame.mask_space = mask_space;
        }
        // No step took the decoder's picture: it is converted on the card into the camera's format, and only that comes down
        if chain.held.is_none() && !chain.withheld && !ctx.stopped() && Gpu::importable(&frame) && self.plain_on_card() {
            self.lift_plain(&frame, &mut chain);
            chain.movable |= chain.plain;
        }
        let mut picture = None;
        if matches!(finish, Finish::Card) && chain.pending() && !ctx.stopped() {
            let gpu = self.gpu.clone().flatten().unwrap();
            match gpu.submit_held(chain.held.as_mut().unwrap(), &mut self.pacing, &|| ctx.stopped()) {
                Ok(()) => {
                    let waited = chain.held.as_mut().unwrap().take_waited();
                    if chain.movable {
                        self.balance.note_wait(waited);
                    }
                    let hides = chain.ahead.iter().any(|&i| self.steps[i].hides());
                    let fallback = if hides { gpu::LandingFailure::Dark } else { gpu::LandingFailure::LastGood };
                    for i in chain.ahead.drain(..) {
                        self.steps[i].fails = 0;
                    }
                    chain.plain = false;
                    picture = Some(Arc::new(gpu::CardFrame::new(gpu, chain.held.take().unwrap(), frame.clone(), fallback, self.deferred_failed.clone(), chain.movable.then(|| self.balance.clone()))) as Arc<dyn telescope_core::media::CardPicture>);
                }
                Err(why) => self.card_failed(&why, &mut chain, &mut frame, ctx, &gpu),
            }
        }
        // Still the decoder's picture and nothing touched it: it is landed when the camera asks, scaled to the camera's size if it can
        if picture.is_none() && keep_fit && !chain.pending() && !chain.withheld {
            if let Some(surface) = frame.surface().cloned() {
                picture = Some(Arc::new(SurfacePicture::new(surface)) as Arc<dyn telescope_core::media::CardPicture>);
            }
        }
        if picture.is_none() {
            self.bring_down(&mut chain, &mut frame, ctx, false);
        }
        frame.number = number;
        frame.capture_ns = capture_ns;
        self.last_readback_ns = chain.down_ns;
        if chain.waits > 0 {
            self.balance.note_wait(chain.waited_ns);
        }
        // A plain stream that would convert on the card but was moved off it tries the card again like a step with both ways
        off_card |= !on_card && self.card_decoder && !self.card_lost && plain_allowed() && self.no_steps_run();
        if off_card {
            self.balance.note_cpu(cpu_ns);
            if !self.was_off_card && ON_THE_CARD.load(Ordering::SeqCst) <= 1 {
                if let Some(gpu) = self.gpu.clone().flatten() {
                    gpu.release_idle();
                }
            }
            self.try_card(&frame);
        }
        self.was_off_card = off_card;
        self.count(chain.ups, chain.downs);
        self.applied = self.hiding.clone();
        self.note_surface_wish();
        span("pipeline", began);
        PipelineFrame::new(frame, picture)
    }

    fn timer_of(&self, owner: &str) -> Option<Arc<Timer>> {
        self.stats.as_ref().map(|s| s.steps().timer(owner))
    }

    /// The stages in the order their steps run, for tests and diagnostics.
    pub fn stages(&self) -> Vec<(String, FrameStage)> {
        self.steps.iter().map(|e| (e.owner.clone(), e.stage)).collect()
    }

    fn gpu(&mut self) -> Option<Arc<Gpu>> {
        let gpu = self.gpu.get_or_insert_with(Gpu::shared).clone();
        if gpu.is_some() && !self.on_card {
            self.on_card = true;
            ON_THE_CARD.fetch_add(1, Ordering::SeqCst);
        }
        gpu
    }

    /// Brings frame up to date with the card, and drops the card's copy unless keep.
    fn bring_down(&mut self, chain: &mut Chain, frame: &mut Frame, ctx: &dyn FrameContext, keep: bool) {
        if !chain.pending() && frame.surface().is_some() {
            *frame = frame.materialise();
        }
        let Some(gpu) = self.gpu.clone().flatten() else { return };
        let pending = chain.pending();
        if let (Some(held), true) = (chain.held.as_mut(), pending) {
            chain.downs += 1;
            let cap = SPARE_POOL_FRAMES * held.size().0 as usize * held.size().1 as usize * 3;
            let down_began = monotonic_ns();
            // The card gives back a new frame, so the cut-out the one in hand carries goes onto it
            let alpha = frame.alpha().cloned();
            let geometry = frame.geometry.clone();
            let mask_space = frame.mask_space;
            let landed = self.pool.scope(cap, || gpu.land_until(held, &|| ctx.stopped()).map(|out| std::mem::replace(frame, out).recycle()));
            chain.down_ns += monotonic_ns().saturating_sub(down_began);
            match landed {
                Ok(()) => {
                    frame.geometry = geometry;
                    frame.mask_space = mask_space;
                    if let Some(alpha) = alpha {
                        frame.set_alpha(alpha);
                    }
                    let waited = held.take_waited();
                    if std::mem::take(&mut chain.movable) {
                        chain.waited_ns += waited;
                        chain.waits += 1;
                    }
                    for i in chain.ahead.drain(..) {
                        self.steps[i].fails = 0;
                    }
                    chain.plain = false;
                }
                Err(why) => self.read_failed(&why, chain, frame, ctx, &gpu),
            }
        }
        if !keep {
            if let Some(held) = chain.held.take() {
                gpu.put_down(held);
            }
        }
        if frame.surface().is_some() {
            *frame = frame.materialise();
        }
    }

    /// A readback of the card's picture failed: the stop that was waiting on it is let through dark, anything else is the card failing this frame.
    fn read_failed(&mut self, why: &str, chain: &mut Chain, frame: &mut Frame, ctx: &dyn FrameContext, gpu: &Gpu) {
        if ctx.stopped() {
            // Stop is waiting on this frame: it isn't worth the CPU's work, the card isn't to blame, and what it was missing mustn't go out as it is
            chain.ahead.clear();
            chain.plain = false;
            if let Some(held) = chain.held.take() {
                gpu.put_down(held);
            }
            *frame = neutral((frame.width, frame.height), frame.format, frame);
            chain.withheld = true;
        } else {
            self.card_failed(why, chain, frame, ctx, gpu);
        }
    }

    /// The card failed what it was doing for this frame: the frame in hand is the last good one, and what the card did after it is lost with it.
    fn card_failed(&mut self, why: &str, chain: &mut Chain, frame: &mut Frame, ctx: &dyn FrameContext, gpu: &Gpu) {
        let lost: Vec<usize> = std::mem::take(&mut chain.ahead);
        // A plain picture still has the decoder's in hand, which comes down the CPU's way
        chain.plain = false;
        for &i in &lost {
            if matches!(self.steps[i].kind, Kind::Shader(_)) {
                self.steps[i].failed(why);
            }
        }
        self.lost_card(why);
        if let Some(held) = chain.held.take() {
            gpu.put_down(held);
        }
        // Steps with a CPU half do their work on the frame in hand now, in order, so this frame isn't missing them; card-only steps stay lost
        for &i in &lost {
            if matches!(self.steps[i].kind, Kind::Either(..)) {
                self.run_cpu(i, frame, chain, ctx);
            }
        }
        // A card-only step that hides can't be done on the CPU, and the frame in hand is without it
        let hiding: Vec<usize> = lost.into_iter().filter(|&i| matches!(self.steps[i].kind, Kind::Shader(_)) && self.steps[i].hides()).collect();
        if !hiding.is_empty() {
            *frame = neutral((frame.width, frame.height), frame.format, frame);
            chain.withheld = true;
            for i in hiding {
                self.tell_withheld(i, ctx);
            }
        }
    }

    /// A card-only step that failed this frame: one that hides sends a dark frame in place of the picture it couldn't hide in.
    fn shader_failed(&mut self, i: usize, why: &str, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext) {
        self.steps[i].failed(why);
        self.withhold_if_hiding(i, frame, chain, ctx);
    }

    fn withhold_if_hiding(&mut self, i: usize, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext) {
        if self.steps[i].hides() {
            *frame = neutral((frame.width, frame.height), frame.format, frame);
            // Dropped, not brought down: the card's copy has the picture this one stands in for
            self.drop_card_work(chain);
            chain.withheld = true;
            self.tell_withheld(i, ctx);
        }
    }

    /// Lets go of what the card holds for this frame without bringing it down, for a frame whose picture is being withheld.
    fn drop_card_work(&mut self, chain: &mut Chain) {
        chain.ahead.clear();
        chain.plain = false;
        if let (Some(held), Some(gpu)) = (chain.held.take(), self.gpu.clone().flatten()) {
            gpu.put_down(held);
        }
    }

    /// The frame in hand is replaced by a dark one, the card's copy dropped, and what comes after can't bring the picture back.
    fn black_out(&mut self, frame: &mut Frame, chain: &mut Chain) {
        *frame = neutral((frame.width, frame.height), frame.format, frame);
        self.drop_card_work(chain);
        chain.withheld = true;
    }

    /// The steps that hide, as they are now.
    fn hiding_now(&self) -> Vec<Applied> {
        self.steps.iter().filter(|e| e.stage.hides() && e.hides()).map(|e| Applied { owner: e.owner.clone(), stage: e.stage, sig: e.sig }).collect()
    }

    /// The steps were changed: what they hide is looked at again, and a step that is gone isn't remembered as on the last frame.
    /// Whether the first step that will run does it on the card, so a decoder picture still there is taken in by it and not copied to memory first.
    fn starts_on_card(&self) -> bool {
        // No card to open, or one that was lost, leaves nothing for a surface to be used by
        let card = match &self.gpu {
            Some(Some(gpu)) => !gpu.is_lost(),
            Some(None) => false,
            None => true,
        };
        let on_card = card && self.balance.place() == Place::Card && !self.card_lost;
        match self.steps.iter().find(|e| !e.skipped()) {
            None => self.plain_on_card(),
            Some(e) => match &e.kind {
                Kind::Shader(_) => card,
                Kind::Either(..) => on_card && !e.no_shader,
                Kind::Cpu(_) => false,
            },
        }
    }

    /// Whether a stream with no step to run converts the decoder's picture on the card: not on one drawing in software, nor while the balance has moved work off it.
    fn plain_on_card(&self) -> bool {
        self.plain_on_card_if(plain_allowed())
    }

    fn plain_on_card_if(&self, allowed: bool) -> bool {
        let card = match &self.gpu {
            Some(Some(gpu)) => !gpu.is_lost() && !gpu.is_software(),
            Some(None) => false,
            None => true,
        };
        card && !self.card_lost && allowed && self.balance.place() == Place::Card && self.no_steps_run()
    }

    /// Whether every step is left out (none, or all skipped for failing).
    fn no_steps_run(&self) -> bool {
        self.steps.iter().all(Entry::skipped)
    }

    /// Takes the decoder's picture in on the card, converted there into I420; anything that stops that leaves it to come down the CPU's way.
    fn lift_plain(&mut self, frame: &Frame, chain: &mut Chain) {
        let Some(gpu) = self.gpu().filter(|gpu| !gpu.is_lost() && !gpu.is_software()) else { return };
        match gpu.lift(frame) {
            Ok(held) if held.was_imported() => {
                chain.held = Some(held);
                chain.plain = true;
            }
            // The stream's import was refused and the picture went up through memory: it comes down the CPU's way, as the next ones will
            Ok(held) => gpu.put_down(held),
            Err(why) => self.lost_card(&why),
        }
    }

    fn note_surface_wish(&self) {
        self.surface_wish.store(self.starts_on_card() || self.fit_wanted(), Ordering::Relaxed);
    }

    /// The size the camera takes while it is open.
    fn camera_size(&self) -> Option<(u32, u32)> {
        unpack_camera_fit(self.camera_fit.load(Ordering::Relaxed))
    }

    /// Whether the decoder's pictures stay on the card for the camera to take scaled there: no step needs them, nothing converts them on the card, and
    /// the camera is open at a size the last picture's can be scaled to.
    fn fit_wanted(&self) -> bool {
        // Tests stand in for Windows, where a plain stream converts on the card only when opted in
        let windows = cfg!(any(windows, test));
        let plain = self.no_steps_run() && !self.plain_on_card_if(plain_gate(windows, plain_opted_in()));
        surface_fit::wanted(windows, plain, self.camera_size(), self.size)
    }

    fn steps_changed(&mut self) {
        self.balance.work_changed();
        self.note_surface_wish();
        let mut now = self.hiding_now();
        let mut applied = (*self.applied).clone();
        applied.retain(|a| match now.iter().position(|n| n.owner == a.owner && n.stage == a.stage) {
            Some(i) => {
                now.swap_remove(i);
                true
            }
            None => false,
        });
        self.applied = Arc::new(applied);
        self.hiding = Arc::new(self.hiding_now());
    }

    /// Whether a step could put back what a dark frame took out, from a picture it keeps of its own (denoising over time) or by working on the picture as it was.
    fn restores_pixels(&self, i: usize) -> bool {
        let e = &self.steps[i];
        !e.hides() && (matches!(e.stage, FrameStage::Effects | FrameStage::Colour) || e.kind.slot_ref().is_some_and(|s| s.keeps_history()))
    }

    /// A skipped step that hides is sent a dark frame in place of its work: a step watching what it hides hears of it.
    fn tell_withheld(&self, i: usize, ctx: &dyn FrameContext) {
        if let Kind::Cpu(step) | Kind::Either(_, step) = &self.steps[i].kind {
            let _ = catch_unwind(AssertUnwindSafe(|| step.withheld(ctx)));
        }
    }

    /// The picture a replay (hide_again) sends out in place of the one that went out is also what the steps at the end of the chain (Spout send) get to see, or they keep showing the clear picture until the next frame comes.
    fn tap_final(&mut self, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext) {
        // A replay that was replaced or a stream that stopped makes nothing that goes out
        if ctx.stopped() {
            return;
        }
        for i in 0..self.steps.len() {
            if self.steps[i].stage == FrameStage::Final && !self.steps[i].skipped() && matches!(self.steps[i].kind, Kind::Cpu(_) | Kind::Either(..)) {
                self.run_cpu(i, frame, chain, ctx);
            }
        }
    }

    fn lost_card(&mut self, why: &str) {
        if !self.card_lost {
            log::error!("The graphics card failed a frame ({why}); steps that can run on the CPU stay there until the stream restarts");
            self.card_lost = true;
        }
    }

    fn run_cpu(&mut self, i: usize, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext) {
        let step = self.steps[i].kind.cpu();
        // One that panics here will panic in process too, where that's dealt with
        let touch = catch_unwind(AssertUnwindSafe(|| step.touch(ctx))).unwrap_or(Touch::Changes);
        if touch == Touch::Nothing {
            return;
        }
        self.bring_down(chain, frame, ctx, touch == Touch::Reads);
        // The readback just failed and the frame in hand went dark for it: this step's own picture would put the clear one back
        if chain.withheld && self.restores_pixels(i) {
            return;
        }
        let entry = &mut self.steps[i];
        let hides = entry.hides();
        // A step that only reads leaves the picture, and so the cut-out, as it is
        let drops_alpha = touch != Touch::Reads && entry.drops_alpha();
        let step = entry.kind.cpu();
        // A step that changes pixels in place would copy them all if a clone kept the old ones, so only one that failed on its last frame
        // gets that fallback; a built-in step that panics the first time may hand on a half-changed frame. Whole-frame writers build a new
        // buffer and swap it in, so they leave this one intact either way.
        // (a step that hides never falls back to the picture it was given)
        let before = (entry.fails > 0 && !hides).then(|| frame.clone());
        let (size, format) = ((frame.width, frame.height), frame.format);
        let was = Arc::downgrade(&frame.data);
        let cap = SPARE_POOL_FRAMES * frame.width as usize * frame.height as usize * 3;
        let pool = &mut self.pool;
        match catch_unwind(AssertUnwindSafe(|| pool.scope(cap, || step.process(frame, ctx)))) {
            Ok(()) if sound(frame) => {
                entry.fails = 0;
                if drops_alpha {
                    frame.clear_alpha();
                }
            }
            Ok(()) => {
                entry.failed(&format!("handed back {}x{} {:?} with {} bytes", frame.width, frame.height, frame.format, frame.data.len()));
                *frame = before.clone().unwrap_or_else(|| neutral(size, format, frame));
                chain.withheld |= hides;
            }
            Err(_) => {
                entry.failed("it panicked");
                chain.withheld |= hides;
                match &before {
                    Some(before) => *frame = before.clone(),
                    None if hides || !sound(frame) => *frame = neutral(size, format, frame),
                    None => {
                        if drops_alpha {
                            frame.clear_alpha();
                        }
                    }
                }
            }
        }
        // A step that said it only reads and then didn't: the frame in hand wins
        if !std::ptr::eq(Weak::as_ptr(&was), Arc::as_ptr(&frame.data)) {
            self.bring_down(chain, frame, ctx, false);
        }
    }

    fn run_shader(&mut self, i: usize, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext, time: f32) {
        let Some(gpu) = self.gpu() else { return self.shader_failed(i, "no graphics card to run shaders on", frame, chain, ctx) };
        // Before any work for the card, however small: a lost device is not touched again
        if gpu.is_lost() || self.card_lost {
            return self.off_the_card(i, "the graphics card was lost", frame, chain, ctx);
        }
        // A step with a CPU half may have pictures for its shader that it works out on the frame, so that comes first
        let textures = self.card_textures(i, frame, chain, ctx);
        // Bringing the picture down for that failed: this frame goes on the CPU from here, with no second wait on the card
        if self.card_lost {
            return self.off_the_card(i, "the graphics card failed a frame", frame, chain, ctx);
        }
        let entry = &mut self.steps[i];
        // It may need uniforms that depend on this frame's size, and on the pictures it just made
        if let Kind::Either(slot, cpu) = &mut entry.kind {
            let (w, h) = (frame.width, frame.height);
            if let Ok(Some(uniforms)) = catch_unwind(AssertUnwindSafe(|| cpu.card_uniforms(w, h))) {
                slot.set_uniforms(&gpu, uniforms);
            }
        }
        let slot = entry.kind.slot();
        if let Err(why) = slot.prepare(&gpu) {
            // A shader that doesn't build never will: say so once and leave it out, or run the CPU way
            log::error!("Plugin {}'s shader can't run ({why}); leaving it out", entry.owner);
            if matches!(entry.kind, Kind::Either(..)) {
                entry.no_shader = true;
                return self.run_cpu(i, frame, chain, ctx);
            }
            entry.fails = STEP_FAILS_BEFORE_SKIP;
            return self.withhold_if_hiding(i, frame, chain, ctx);
        }
        if let Some(textures) = textures {
            if let Err(why) = slot.set_textures(&gpu, &textures) {
                // The shader never draws with a picture of the last frame's in place of this one's
                return self.refused(i, &why, frame, chain, ctx);
            }
        }
        let mut masked = false;
        if slot.wants_mask() {
            // The vision model looks at the picture as it is here, and answers once per frame
            if !chain.mask.as_ref().is_some_and(|(w, h, _)| (*w, *h) == (frame.width, frame.height)) {
                self.bring_down(chain, frame, ctx, true);
                if chain.withheld && self.restores_pixels(i) {
                    return;
                }
                // A shader reads the mask by uv through a linear sampler, so it goes up at a size near the model's own, not the frame's
                let (mw, mh) = mask_up_size(frame.width, frame.height);
                let asked = catch_unwind(AssertUnwindSafe(|| if (mw, mh) == (frame.width, frame.height) { ctx.person_mask(frame) } else { ctx.person_mask_at(frame, mw as usize, mh as usize) })).unwrap_or(None);
                chain.mask = Some((frame.width, frame.height, asked.map(|mask| (mw, mh, mask))));
            }
            let kept = match &chain.mask {
                Some((_, _, Some((w, h, mask)))) => Some(gpu.keep_mask(&mut self.mask, mask, *w, *h)),
                _ => None,
            };
            match kept {
                Some(Ok(on_card)) => masked = on_card,
                // The old mask is gone with it: the shader never draws with a mask that isn't this picture's
                Some(Err(why)) => return self.refused(i, &why, frame, chain, ctx),
                None => {}
            }
        }
        // Read the way the CPU reads it if any step here has a CPU way that takes chroma by its block, so both ways see the same colours
        let block_chroma = self.steps.iter().any(|e| matches!(&e.kind, Kind::Either(_, cpu) if cpu.card_block_chroma()));
        let entry = &mut self.steps[i];
        if chain.held.is_none() {
            match gpu.lift_with(frame, block_chroma) {
                Ok(held) => {
                    if !held.was_imported() { chain.ups += 1; }
                    chain.held = Some(held);
                }
                Err(why) if matches!(entry.kind, Kind::Either(..)) => {
                    self.lost_card(&why);
                    return self.run_cpu(i, frame, chain, ctx);
                }
                Err(why) => return self.shader_failed(i, &why, frame, chain, ctx),
            }
        }
        // A frame that went dark has nothing of the room in it, and the step's last frame has: it starts again from this one
        let shaded = gpu.shade_with(chain.held.as_mut().unwrap(), entry.kind.slot(), time, self.mask.as_ref().filter(|_| masked), !chain.withheld);
        chain.ahead.push(i);
        // What the CPU way would have changed besides the pixels (the view's geometry) goes onto the frame in hand, which lands with the card's picture
        if let Kind::Either(_, cpu) = &mut self.steps[i].kind {
            let _ = catch_unwind(AssertUnwindSafe(|| cpu.card_frame(frame)));
        }
        if self.steps[i].drops_alpha() {
            // The frame in hand is behind the card, and the cut-out it carries is the one that lands with the picture
            frame.clear_alpha();
        }
        if let Err(why) = shaded {
            // What was recorded for this frame can't be trusted, so it is lost as a failed readback loses it
            self.card_failed(&why, chain, frame, ctx, &gpu);
        }
    }

    /// The card refused a picture or mask for this step: one with a CPU half runs there on the frame in hand, one that is the card's alone fails for this frame.
    fn refused(&mut self, i: usize, why: &str, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext) {
        if matches!(self.steps[i].kind, Kind::Either(..)) {
            self.lost_card(why);
            self.run_cpu(i, frame, chain, ctx);
        } else {
            self.shader_failed(i, why, frame, chain, ctx);
        }
    }

    /// A step the card can't take now: one with a CPU half runs there on the frame in hand, one that is the card's alone fails for this frame.
    fn off_the_card(&mut self, i: usize, why: &str, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext) {
        self.lost_card(why);
        if matches!(self.steps[i].kind, Kind::Either(..)) {
            self.run_cpu(i, frame, chain, ctx);
        } else {
            self.shader_failed(i, why, frame, chain, ctx);
        }
    }

    /// The pictures an Either's CPU half has for its shader, asked with the frame on the CPU (so it comes down first, the card keeping its copy). A step that has none says so once and isn't asked again, and until it has given some it is first asked on the frame in hand, so one that has none never brings the picture down. One that panics gives none, which leaves what it hides hidden.
    fn card_textures(&mut self, i: usize, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext) -> Option<Vec<(String, Frame)>> {
        if !matches!(self.steps[i].kind, Kind::Either(..)) || self.steps[i].no_textures {
            return None;
        }
        let ask = |entry: &mut Entry, frame: &Frame, ctx: &dyn FrameContext| -> Result<Option<Vec<(String, Frame)>>, ()> {
            let Kind::Either(_, cpu) = &mut entry.kind else { return Ok(None) };
            catch_unwind(AssertUnwindSafe(|| cpu.card_textures(frame, ctx))).map_err(|_| ())
        };
        // What it says about itself comes first: asking for pictures on the frame in hand while the card is ahead of it would send a mask request for the picture from before the card's work, and the answer for a frame is kept
        let offered = match &self.steps[i].kind {
            Kind::Either(_, cpu) => catch_unwind(AssertUnwindSafe(|| cpu.card_textures_offered())).unwrap_or(None),
            _ => None,
        };
        if offered == Some(false) {
            self.steps[i].no_textures = true;
            return None;
        }
        if offered.is_none() && !self.steps[i].has_textures && !chain.ahead.is_empty() {
            if let Ok(None) = ask(&mut self.steps[i], frame, ctx) {
                self.steps[i].no_textures = true;
                return None;
            }
        }
        // A step that only needs the person mask gets it from a small copy the card makes, and the picture stays up there
        let mask_only = matches!(&self.steps[i].kind, Kind::Either(_, cpu) if catch_unwind(AssertUnwindSafe(|| cpu.card_textures_from_mask())).unwrap_or(false));
        let small = if mask_only { self.small_for_vision(frame, chain, ctx) } else { Small::Whole };
        // The card failed the small picture: this frame goes on the CPU from here, as for a failed readback, with no second wait on the card
        let small = match small {
            Small::Failed => return None,
            Small::Made(small) => Some(*small),
            Small::Whole => None,
        };
        let vision = small.as_ref().map(|small| SmallVision { inner: ctx, small });
        let ctx: &dyn FrameContext = match &vision {
            Some(vision) => vision,
            None => {
                self.bring_down(chain, frame, ctx, true);
                ctx
            }
        };
        let asked = ask(&mut self.steps[i], frame, ctx);
        // The shader makes no cut-out, so the CPU half gives it now and it comes back with the picture
        if asked.is_ok() {
            if let Kind::Either(_, cpu) = &mut self.steps[i].kind {
                if let Ok(Some(alpha)) = catch_unwind(AssertUnwindSafe(|| cpu.card_cut_out(frame, ctx))) {
                    frame.set_alpha(alpha);
                }
            }
        }
        match asked {
            Ok(None) => {
                self.steps[i].no_textures = true;
                None
            }
            Ok(some) => {
                self.steps[i].has_textures = true;
                some
            }
            Err(()) => Some(Vec::new()),
        }
    }

    /// The picture as it is now, shrunk on the card to the size the vision models take and read back, so the whole one needn't come down. Whole (the caller brings it down as before) when the card doesn't hold it and can't take it in where it is. When the card fails the readback, what it held for this frame is gone with it (the recorded steps went with the refused or stalled submission), so the frame goes on the CPU as for any failed readback.
    fn small_for_vision(&mut self, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext) -> Small {
        if !vision_on_card() || self.card_lost {
            return Small::Whole;
        }
        let Some(gpu) = self.gpu().filter(|gpu| !gpu.is_lost() && (!gpu.is_software() || small_on_software())) else { return Small::Whole };
        let Some(size) = vision_size(frame.width, frame.height) else { return Small::Whole };
        if chain.held.is_none() {
            // Only a decoder's picture the card takes in where it is: one in memory has its pixels at hand already
            if !Gpu::importable(frame) {
                return Small::Whole;
            }
            let block_chroma = self.steps.iter().any(|e| matches!(&e.kind, Kind::Either(_, cpu) if cpu.card_block_chroma()));
            match gpu.lift_with(frame, block_chroma) {
                Ok(held) if held.was_imported() => chain.held = Some(held),
                Ok(held) => {
                    gpu.put_down(held);
                    return Small::Whole;
                }
                Err(_) => return Small::Whole,
            }
        }
        let Some(held) = chain.held.as_mut() else { return Small::Whole };
        let began = monotonic_ns();
        let small = gpu.land_small(held, size, &|| ctx.stopped());
        span("vision_small", began);
        match small {
            Ok(None) => Small::Whole,
            Ok(Some(mut small)) => {
                small.geometry = frame.geometry.clone();
                small.mask_space = frame.mask_space;
                small.number = frame.number;
                Small::Made(Box::new(small))
            }
            Err(why) => {
                self.read_failed(&why, chain, frame, ctx, &gpu);
                Small::Failed
            }
        }
    }

    /// Keeps a step that's off the card ready for coming back, so that doesn't cost a frame later.
    fn ready_on_card(&mut self, i: usize) {
        if let Some(gpu) = self.gpu().filter(|gpu| !gpu.is_lost()) {
            if let Err(why) = self.steps[i].kind.slot().prepare(&gpu) {
                log::error!("Plugin {}'s shader can't run ({why}); it stays on the CPU", self.steps[i].owner);
                self.steps[i].no_shader = true;
            }
        }
    }

    /// While frames stay off the card, sends one of them there now and then to see what it would have waited.
    fn try_card(&mut self, frame: &Frame) {
        let Some(gpu) = self.gpu() else { return };
        let Some(ticket) = self.balance.trial_due() else { return };
        let frame = frame.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let done = Trial(self.balance.clone(), ticket, None, stop.clone());
        let called_off = stop.clone();
        let trial = move || {
            // Taken whole here: a closure would only capture the field it writes, and the rest would drop now
            let mut done = done;
            match gpu.trial(&frame, &|| called_off.load(Ordering::SeqCst)) {
                Ok(waited) => done.2 = Some(waited),
                Err(why) if called_off.load(Ordering::SeqCst) => log::debug!("The graphics card trial was called off: {why}"),
                Err(why) => log::warn!("Couldn't try the graphics card: {why}"),
            }
        };
        match std::thread::Builder::new().name("card-trial".into()).spawn(trial) {
            // The last one has ended (a new one only starts after it did), so this only collects it
            Ok(handle) => if let Some((old, _)) = self.trial.replace((handle, stop)) { let _ = old.join(); },
            Err(why) => log::warn!("Couldn't try the graphics card: {why}"),
        }
    }

    /// Looks at what the View and Geometry stages' steps do to the picture's geometry (the window's own turn, Desk mode's sheet): when that is not what it was, the next frame makes every step
    /// that keeps pictures of its own forget them. A rebuilt step that does the same (a filter's slider, another plugin's refresh) doesn't.
    fn note_geometry(&mut self) {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for entry in self.steps.iter().filter(|e| matches!(e.stage, FrameStage::View | FrameStage::Geometry)) {
            match &entry.kind {
                Kind::Cpu(step) | Kind::Either(_, step) => step.geometry_key().hash(&mut hasher),
                Kind::Shader(slot) => slot.geometry_key().hash(&mut hasher),
            }
        }
        let key = hasher.finish();
        if self.view_key.is_some_and(|before| before != key) {
            self.forget_due = true;
        }
        self.view_key = Some(key);
    }

    fn forget_histories(&mut self, source: &str) {
        for entry in &mut self.steps {
            match &mut entry.kind {
                Kind::Cpu(step) => step.forget_history(source),
                Kind::Shader(slot) => slot.forget_history(),
                Kind::Either(slot, step) => {
                    slot.forget_history();
                    step.forget_history(source);
                }
            }
        }
    }

    fn count(&mut self, ups: u32, downs: u32) {
        transfers::frame(ups, downs);
        self.last_transfers = (ups, downs);
        let t = &mut self.tally;
        (t.frames, t.ups, t.downs) = (t.frames + 1, t.ups + ups, t.downs + downs);
        if t.frames == TALLY_FRAMES {
            let masks = transfers::totals().mask_up;
            if (t.ups, t.downs) != t.logged || masks != t.masks {
                let per = |n: u32| n as f32 / TALLY_FRAMES as f32;
                let names: Vec<String> = self.steps.iter().map(|e| format!("{} {}", e.owner, e.kind.label())).collect();
                log::info!("Frame steps cost {:.2} up and {:.2} down per frame over {TALLY_FRAMES} frames, with {} steps and {} person masks up ({})",
                           per(t.ups), per(t.downs), self.steps.len(), masks - t.masks, names.join(", "));
                t.logged = (t.ups, t.downs);
            }
            (t.frames, t.ups, t.downs, t.masks) = (0, 0, 0, masks);
        }
    }
}


/// A trial out on the card, however it ends: a panic or a thread that wouldn't start still lets the next one go. The flag is set when the pipeline is going away.
struct Trial(Lane, u64, Option<u64>, Arc<AtomicBool>);

impl Drop for Trial {
    fn drop(&mut self) {
        match self.2 {
            None if self.3.load(Ordering::SeqCst) => self.0.trial_called_off(self.1),
            wait => self.0.trial_done(self.1, wait),
        }
    }
}

/// A flat dark picture of this size, for a step that broke the frame it was given when there is no copy to go back to.
/// The width a person mask goes up to the card at.
/// Whether a step that only needs the person mask has the card make the small picture the models take (TELESCOPE_VISION_ON_CARD=0 turns it off).
fn vision_on_card() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TELESCOPE_VISION_ON_CARD").is_none_or(|v| v != "0"))
}

/// Whether the small picture is made on a card that is the processor drawing in software, which only tests have a reason for.
fn small_on_software() -> bool {
    #[cfg(test)]
    return tests::SMALL_ON_SOFTWARE.with(std::cell::Cell::get);
    #[cfg(not(test))]
    false
}

/// The pixels the biggest vision model takes (the high quality cutout's 640 x 360).
const VISION_PIXELS: u64 = 640 * 360;

/// The size of the small picture: exactly what the high quality cutout gets of the whole frame, so a stream that moves between the card and the processor doesn't change the model's input size (which starts its memory over). None for a frame no bigger than twice the model's pixels, which comes down whole as before.
fn vision_size(w: u32, h: u32) -> Option<(u32, u32)> {
    if (w as u64) * (h as u64) <= 2 * VISION_PIXELS {
        return None;
    }
    telescope_core::vision::cutout_input_size(w, h)
}

/// What the card gave for a step that only needs the person mask.
enum Small {
    /// The small picture.
    Made(Box<Frame>),
    /// None was made and nothing was lost: the picture comes down whole.
    Whole,
    /// The card failed and the frame has been dealt with as for a failed readback.
    Failed,
}

/// The frame step's context with the person mask asked for on the small picture the card made, which has the frame's number, crop and view.
struct SmallVision<'a> {
    inner: &'a dyn FrameContext,
    small: &'a Frame,
}

impl FrameContext for SmallVision<'_> {
    fn source_id(&self) -> &str {
        self.inner.source_id()
    }
    fn entry_epoch(&self) -> Option<u64> {
        self.inner.entry_epoch()
    }
    fn stopped(&self) -> bool {
        self.inner.stopped()
    }
    fn hurried(&self) -> bool {
        self.inner.hurried()
    }
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.inner.person_mask_at(self.small, frame.width as usize, frame.height as usize)
    }
    fn person_mask_at(&self, _frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        self.inner.person_mask_at(self.small, w, h)
    }
    fn person_mask_hq(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.inner.person_mask_hq_at(self.small, frame.width as usize, frame.height as usize)
    }
    fn person_mask_hq_at(&self, _frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        self.inner.person_mask_hq_at(self.small, w, h)
    }
    fn background_mask_at(&self, _frame: &Frame, w: usize, h: usize, high_quality: bool) -> Option<Arc<Vec<u8>>> {
        self.inner.background_mask_at(self.small, w, h, high_quality)
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        self.inner.colour_preview(frame)
    }
    fn effort(&self) -> u8 {
        self.inner.effort()
    }
}

const MASK_UP_WIDTH: u32 = 640;

/// The size a shader's person mask is asked at for a frame of w x h: the frame's own when it is no wider than MASK_UP_WIDTH, else that wide in the frame's shape.
fn mask_up_size(w: u32, h: u32) -> (u32, u32) {
    if w <= MASK_UP_WIDTH {
        return (w, h);
    }
    (MASK_UP_WIDTH, ((h as u64 * MASK_UP_WIDTH as u64 + w as u64 / 2) / w as u64).max(1) as u32)
}

fn neutral((w, h): (u32, u32), format: PixelFormat, stands_for: &Frame) -> Frame {
    let mut out = Frame::dark(w, h, format);
    (out.number, out.time_ns, out.capture_ns) = (stands_for.number, stands_for.time_ns, stands_for.capture_ns);
    out.geometry = stands_for.geometry.clone();
    out.mask_space = stands_for.mask_space;
    out
}

/// Whether a step handed back something the rest of the stream can use.
fn sound(frame: &Frame) -> bool {
    frame.width > 0 && frame.height > 0 && frame.data.len() == frame.format.size(frame.width, frame.height)
}

impl FramePipeline for Pipeline {
    fn set_steps(&mut self, steps: Vec<StagedStep>) {
        let mut entries: Vec<Entry> = steps.into_iter().map(|s| Entry::new(s.owner.clone(), s.stage, s.step, self.timer_of(&s.owner))).collect();
        // A step that keeps its last frame (denoising over time) and comes again with other settings goes on from it
        let mut old: Vec<Option<&Entry>> = self.steps.iter().map(Some).collect();
        for entry in &entries {
            let Some(new) = entry.kind.slot_ref().filter(|s| s.keeps_history()) else { continue };
            let found = old.iter_mut().find(|o| o.is_some_and(|o| o.owner == entry.owner && o.stage == entry.stage && o.kind.slot_ref().is_some_and(|s| s.keeps_history() && s.same_shader(new))));
            if let Some(slot) = found.and_then(Option::take).and_then(|o| o.kind.slot_ref()) {
                new.take_history_from(slot);
            }
        }
        // Stable: as registered within a stage
        entries.sort_by_key(|e| e.stage);
        adopt_shaders(&mut entries, &mut self.steps);
        self.steps = entries;
        self.steps_changed();
        self.note_geometry();
    }

    fn replace_steps(&mut self, owner: &str, stage: FrameStage, steps: Vec<FrameStep>) {
        let (mut old, kept): (Vec<Entry>, Vec<Entry>) = std::mem::take(&mut self.steps).into_iter().partition(|e| e.owner == owner);
        self.steps = kept;
        let timer = self.timer_of(owner);
        let mut fresh: Vec<Entry> = steps.into_iter().map(|step| Entry::new(owner.to_string(), stage, step, timer.clone())).collect();
        adopt_shaders(&mut fresh, &mut old);
        // First within the stage, where the window's own steps go
        let at = self.steps.iter().position(|e| e.stage >= stage).unwrap_or(self.steps.len());
        self.steps.splice(at..at, fresh);
        self.steps_changed();
        self.note_geometry();
    }

    fn surface_wish(&self) -> Option<Arc<AtomicBool>> {
        Some(self.surface_wish.clone())
    }

    fn camera_fit(&self) -> Option<Arc<AtomicU64>> {
        Some(self.camera_fit.clone())
    }

    fn process(&mut self, frame: Frame, ctx: &dyn FrameContext) -> Frame {
        self.run(frame, ctx, Finish::Ram).land()
    }

    fn process_picture(&mut self, frame: Frame, ctx: &dyn FrameContext) -> PipelineFrame {
        self.run(frame, ctx, Finish::Card)
    }

    fn hide_again(&mut self, mut frame: Frame, ctx: &dyn FrameContext) -> Option<Frame> {
        let mut chain = Chain::default();
        let mut hid = false;
        // What the frame already went through: only a step that isn't on it, or is another than it was, has to be done now
        let mut applied = (*self.applied).clone();
        let (number, capture_ns) = (frame.number, frame.capture_ns);
        let started = *self.started_ns.get_or_insert_with(monotonic_ns);
        let time = monotonic_ns().saturating_sub(started) as f32 / 1e9;
        for i in 0..self.steps.len() {
            let stage = self.steps[i].stage;
            if !matches!(stage, FrameStage::Background | FrameStage::Privacy | FrameStage::Cover) {
                continue;
            }
            if self.steps[i].skipped() {
                if self.steps[i].hides() {
                    frame = neutral((frame.width, frame.height), frame.format, &frame);
                    // Dropped, not landed: the card's copy has the picture this one stands in for
                    self.drop_card_work(&mut chain);
                    chain.withheld = true;
                    self.tell_withheld(i, ctx);
                    hid = true;
                }
                continue;
            }
            if chain.withheld && self.restores_pixels(i) {
                continue;
            }
            let (owner, sig) = (self.steps[i].owner.clone(), self.steps[i].sig);
            if stage == FrameStage::Background {
                // A Background step the picture in hand didn't go through, or went through with other settings (a moved redaction), can't be done again on a finished picture: it makes the whole frame dark until a frame comes to do it properly
                if self.steps[i].hides() && take_applied(&mut applied, &owner, stage, sig) != Seen::Same {
                    self.black_out(&mut frame, &mut chain);
                    hid = true;
                }
                continue;
            }
            // A card-only step can't say whether it has something to hide, so it runs on the card unless the picture went through it as it is, and a failure there goes dark
            if let Kind::Shader(_) = &self.steps[i].kind {
                match take_applied(&mut applied, &owner, stage, sig) {
                    Seen::Same => continue,
                    // Run on the picture that already has the old one's work, it would pile on or undo it: dark until a frame comes
                    Seen::Changed => {
                        self.black_out(&mut frame, &mut chain);
                        hid = true;
                    }
                    Seen::New => {
                        hid = true;
                        if ctx.hurried() {
                            // Nobody can wait for the card here: dark now, and the real run follows on a thread that can
                            self.black_out(&mut frame, &mut chain);
                        } else {
                            self.run_shader(i, &mut frame, &mut chain, ctx, time);
                        }
                    }
                }
                continue;
            }
            // Only a CPU step can say whether it has something to hide right now
            let (Kind::Cpu(step) | Kind::Either(_, step)) = &self.steps[i].kind else { continue };
            if catch_unwind(AssertUnwindSafe(|| step.touch(ctx))).unwrap_or(Touch::Changes) != Touch::Nothing {
                hid = true;
                // A step with a card half and other settings than the picture went through is worked on a picture that has the old ones' work: dark until a frame comes
                if sig.is_some() && take_applied(&mut applied, &owner, stage, sig) == Seen::Changed {
                    self.black_out(&mut frame, &mut chain);
                } else {
                    self.run_cpu(i, &mut frame, &mut chain, ctx);
                }
            }
        }
        if hid {
            self.bring_down(&mut chain, &mut frame, ctx, false);
            frame.number = number;
            frame.capture_ns = capture_ns;
            self.tap_final(&mut frame, &mut chain, ctx);
            // A hurried pass left the shaders undone, and a dark frame says nothing of what was done: a step that failed is still to be tried
            if !ctx.hurried() && !self.steps.iter().any(|e| e.fails > 0 && e.hides()) {
                self.applied = self.hiding.clone();
            }
        }
        hid.then_some(frame)
    }

    fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    fn forget_history(&mut self) {
        self.forget_due = true;
        // A gap in the frames is one in what the balance hears too
        self.balance.resumed();
        // What the last frame carried can't be vouched for after a gap or a discarded replay, so every hiding step counts as new until a frame goes through them
        self.applied = Arc::default();
    }

    fn set_stats(&mut self, stats: Arc<SourceStats>) {
        self.stats = Some(stats);
        for i in 0..self.steps.len() {
            self.steps[i].timer = self.timer_of(&self.steps[i].owner);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;
    use telescope_core::frame::{Alpha, PixelFormat, ShaderStep};

    pub(crate) struct NoContext;

    impl FrameContext for NoContext {
        fn source_id(&self) -> &str {
            ""
        }
        fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
            None
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
    }

    struct Push(u8, Arc<std::sync::Mutex<Vec<u8>>>);

    impl CpuStep for Push {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            self.1.lock().unwrap().push(self.0);
            frame.data_mut()[0] = self.0;
        }
    }

    struct Panics;

    impl CpuStep for Panics {
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {
            panic!("broken")
        }
    }

    struct Counts(Arc<std::sync::Mutex<u32>>);

    impl CpuStep for Counts {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            *self.0.lock().unwrap() += 1;
            frame.data_mut()[1] = 77;
            panic!("broken")
        }
    }

    struct Mangles;

    impl CpuStep for Mangles {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            frame.data_mut()[0] = 99;
            frame.data_mut().truncate(3);
        }
    }

    pub(crate) fn grey(w: u32, h: u32, v: u8) -> Frame {
        Frame::new(w, h, PixelFormat::I420, vec![v; PixelFormat::I420.size(w, h)])
    }

    fn step(owner: &str, stage: FrameStage, s: impl CpuStep + 'static) -> StagedStep {
        StagedStep { owner: owner.into(), stage, step: FrameStep::Cpu(Box::new(s)) }
    }

    fn quietly<T>(f: impl FnOnce() -> T) -> T {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let out = f();
        std::panic::set_hook(hook);
        out
    }

    /// Builds a frame of its own, as a plugin's step may.
    struct Rebuilds;

    impl CpuStep for Rebuilds {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            *frame = grey(frame.width, frame.height, 1);
        }
    }

    #[test]
    fn steps_of_one_owner_can_change_and_the_others_stay() {
        let mut p = Pipeline::new();
        p.set_steps(vec![step("view", FrameStage::View, Rebuilds), step("blur", FrameStage::Effects, Rebuilds)]);
        p.replace_steps("view", FrameStage::View, Vec::new());
        assert_eq!(p.stages(), [("blur".to_string(), FrameStage::Effects)]);
        p.replace_steps("view", FrameStage::View, vec![FrameStep::Cpu(Box::new(Rebuilds))]);
        assert_eq!(p.stages(), [("view".to_string(), FrameStage::View), ("blur".to_string(), FrameStage::Effects)]);
    }

    /// A view step that moves the picture some way (its number), and one that keeps pictures of its own and hears when to drop them.
    struct Moves(u64);

    impl CpuStep for Moves {
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
        fn geometry_key(&self) -> u64 {
            self.0
        }
    }

    struct Keeps(Arc<std::sync::Mutex<Vec<String>>>);

    impl CpuStep for Keeps {
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
        fn forget_history(&mut self, source: &str) {
            self.0.lock().unwrap().push(source.to_string());
        }
    }

    #[test]
    fn steps_that_keep_pictures_start_again_on_the_frame_after_a_pause() {
        let forgot = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("filters", FrameStage::Effects, Keeps(forgot.clone()))]);
        p.process(grey(4, 2, 0), &NoContext);
        assert!(forgot.lock().unwrap().is_empty());
        FramePipeline::forget_history(&mut p);
        assert!(forgot.lock().unwrap().is_empty(), "not until a frame comes");
        p.process(grey(4, 2, 0), &NoContext);
        p.process(grey(4, 2, 0), &NoContext);
        assert_eq!(forgot.lock().unwrap().len(), 1, "once");
    }

    #[test]
    fn steps_that_keep_pictures_start_again_when_the_view_moves_the_picture_but_not_when_only_they_change() {
        let forgot = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("view", FrameStage::View, Moves(1)), step("filters", FrameStage::Effects, Keeps(forgot.clone()))]);
        p.process(grey(4, 2, 0), &NoContext);
        // The filters' own settings change, the view doesn't
        p.set_steps(vec![step("view", FrameStage::View, Moves(1)), step("filters", FrameStage::Effects, Keeps(forgot.clone()))]);
        p.process(grey(4, 2, 0), &NoContext);
        p.replace_steps("view", FrameStage::View, vec![FrameStep::Cpu(Box::new(Moves(1)))]);
        p.process(grey(4, 2, 0), &NoContext);
        assert!(forgot.lock().unwrap().is_empty(), "a refresh that moves nothing keeps what they remember");
        // A flip, a pan, a zoom or a lens change
        p.replace_steps("view", FrameStage::View, vec![FrameStep::Cpu(Box::new(Moves(2)))]);
        p.process(grey(4, 2, 0), &NoContext);
        p.process(grey(4, 2, 0), &NoContext);
        assert_eq!(*forgot.lock().unwrap(), [""], "once, on the first frame after");
        // The view going away is a change too
        p.replace_steps("view", FrameStage::View, Vec::new());
        p.process(grey(4, 2, 0), &NoContext);
        assert_eq!(forgot.lock().unwrap().len(), 2);
    }

    #[test]
    fn steps_that_keep_pictures_start_again_when_a_geometry_step_moves_the_picture_to_the_same_size() {
        let forgot = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("desk", FrameStage::Geometry, Moves(1)), step("filters", FrameStage::Effects, Keeps(forgot.clone()))]);
        p.process(grey(4, 2, 0), &NoContext);
        p.set_steps(vec![step("desk", FrameStage::Geometry, Moves(1)), step("filters", FrameStage::Effects, Keeps(forgot.clone()))]);
        p.process(grey(4, 2, 0), &NoContext);
        assert!(forgot.lock().unwrap().is_empty(), "the same sheet again moves nothing");
        // The corners of the sheet moved: same size out, other pixels in
        p.replace_steps("desk", FrameStage::Geometry, vec![FrameStep::Cpu(Box::new(Moves(2)))]);
        p.process(grey(4, 2, 0), &NoContext);
        p.process(grey(4, 2, 0), &NoContext);
        assert_eq!(*forgot.lock().unwrap(), [""], "once, on the first frame after");
    }

    /// What number the frame has when it gets here.
    struct Numbers(Arc<std::sync::Mutex<Vec<u64>>>);

    impl CpuStep for Numbers {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            self.0.lock().unwrap().push(frame.number);
        }
    }

    #[test]
    fn a_step_that_builds_a_new_frame_keeps_its_number() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("view", FrameStage::View, Rebuilds), step("blur", FrameStage::Effects, Numbers(seen.clone()))]);
        let mut frame = grey(4, 2, 0);
        frame.number = 7;
        assert_eq!(p.process(frame, &NoContext).number, 7);
        assert_eq!(*seen.lock().unwrap(), [7]);
    }

    #[test]
    fn frames_go_through_the_stages_in_order_whatever_order_plugins_are_registered_in() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::new();
        p.set_steps(vec![
            step("preview", FrameStage::Final, Push(9, seen.clone())),
            step("tag", FrameStage::Overlay, Push(7, seen.clone())),
            step("grade", FrameStage::Colour, Push(6, seen.clone())),
            // A plugin naming a stage that doesn't exist gets the default (core's test_a_stage_that_doesnt_exist_counts_as_effects)
            step("odd", FrameStage::default(), Push(4, seen.clone())),
            step("blur", FrameStage::Privacy, Push(10, seen.clone())),
            step("key", FrameStage::Background, Push(3, seen.clone())),
            step("view", FrameStage::View, Push(1, seen.clone())),
            step("more", FrameStage::Effects, Push(5, seen.clone())),
            step("desk", FrameStage::Geometry, Push(2, seen.clone())),
        ]);
        let out = p.process(grey(4, 2, 0), &NoContext);
        assert_eq!(*seen.lock().unwrap(), [1, 2, 3, 4, 5, 6, 10, 7, 9]);
        assert_eq!(out.data[0], 9);
    }

    /// Adds to the picture or multiplies it, so what comes out says which ran first.
    struct Maths(Box<dyn Fn(u8) -> u8 + Send>);

    impl CpuStep for Maths {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            let value = (self.0)(frame.data[0]);
            frame.data_mut()[0] = value;
        }
    }

    #[test]
    fn worker_processes_pipeline_in_order() {
        let mut p = Pipeline::new();
        p.set_steps(vec![step("first", FrameStage::Effects, Maths(Box::new(|v| v + 2))), step("second", FrameStage::Effects, Maths(Box::new(|v| v * 3)))]);
        assert_eq!(p.process(grey(4, 2, 1), &NoContext).data[0], 9, "(1 + 2) * 3");
    }

    #[test]
    fn a_new_pipeline_takes_over_from_the_next_frame() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::new();
        assert!(p.is_empty());
        p.set_steps(vec![step("a", FrameStage::Effects, Push(1, seen.clone()))]);
        p.process(grey(4, 2, 0), &NoContext);
        p.set_steps(vec![step("b", FrameStage::Effects, Push(2, seen.clone()))]);
        p.process(grey(4, 2, 0), &NoContext);
        assert_eq!(*seen.lock().unwrap(), [1, 2]);
    }

    #[test]
    fn a_failing_or_mangling_step_passes_the_frame_through_and_is_skipped_after_a_while() {
        let (calls, seen) = (Arc::new(std::sync::Mutex::new(0)), Arc::new(std::sync::Mutex::new(Vec::new())));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("broken", FrameStage::Effects, Counts(calls.clone())), step("mangler", FrameStage::Effects, Mangles)]);
        let source = grey(4, 2, 1);
        for n in 0..STEP_FAILS_BEFORE_SKIP + 10 {
            let out = quietly(|| p.process(source.clone(), &NoContext));
            assert_eq!((out.width, out.height), (4, 2));
            assert_eq!(out.data.len(), out.format.size(4, 2), "a whole picture comes out");
            // The first failure has no copy to go back to (a built-in step may hand on a half-changed frame); after that there is
            if n > 0 {
                assert_eq!(out.data, source.data, "passed through, what the step did to it left behind");
            }
        }
        assert_eq!(*calls.lock().unwrap(), STEP_FAILS_BEFORE_SKIP, "skipped from then on");
        assert!(p.steps[0].skipped() && p.steps[1].skipped());
        // What the decoder made is never changed under it while somebody else holds it
        assert_eq!(source.data[0], 1);
        // The next stream's pipeline tries them again, and steps after a broken one still run
        p.set_steps(vec![step("broken", FrameStage::Effects, Panics), step("fine", FrameStage::Overlay, Push(5, seen.clone()))]);
        let out = quietly(|| p.process(grey(4, 2, 1), &NoContext));
        assert_eq!(out.data[0], 5);
        assert_eq!(*seen.lock().unwrap(), [5]);
    }

    /// Panics, and says it hides something.
    struct HidingPanics;

    impl CpuStep for HidingPanics {
        fn hides(&self) -> bool {
            true
        }
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {
            panic!("broken")
        }
    }

    #[test]
    fn a_step_that_hides_things_never_lets_the_clear_picture_through_when_it_fails_or_is_skipped() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("blur", FrameStage::Privacy, HidingPanics), step("tag", FrameStage::Overlay, Push(5, seen.clone()))]);
        let source = grey(4, 2, 200);
        let dark = |out: &Frame| out.data[1..8].iter().all(|v| *v == 16) && out.data[8..].iter().all(|v| *v == 128);
        for n in 0..STEP_FAILS_BEFORE_SKIP + 5 {
            let out = quietly(|| p.process(source.clone(), &NoContext));
            assert!(dark(&out), "frame {n}: {:?}", out.data);
            assert_eq!(out.number, source.number);
        }
        assert!(p.steps[0].skipped(), "skipped by now, and still nothing clear goes out");
        assert_eq!(seen.lock().unwrap().len(), STEP_FAILS_BEFORE_SKIP as usize + 5, "the steps after it still run");
        // One that doesn't say it hides keeps the old way
        let mut p = Pipeline::new();
        p.set_steps(vec![step("fun", FrameStage::Effects, Panics)]);
        quietly(|| p.process(source.clone(), &NoContext));
        assert_eq!(quietly(|| p.process(source.clone(), &NoContext)).data, source.data);
    }

    fn is_dark(out: &Frame) -> bool {
        let luma = out.width as usize * out.height as usize;
        out.data[..luma].iter().all(|v| *v == 16) && out.data[luma..].iter().all(|v| *v == 128)
    }

    #[test]
    fn a_skipped_hiding_step_after_a_shader_never_lets_the_cards_picture_through() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let mut p = Pipeline::new();
        p.set_steps(vec![shader(INVERT), step("blur", FrameStage::Privacy, HidingPanics)]);
        let source = grey(8, 8, 200);
        for n in 0..STEP_FAILS_BEFORE_SKIP + 5 {
            let out = quietly(|| p.process(source.clone(), &NoContext));
            assert!(is_dark(&out), "frame {n}: the inverted picture from the card is {:?}", out.data[0]);
        }
        assert!(p.steps[1].skipped(), "past the skip, with the shader's work still waiting on the card each frame");
    }

    /// Hides, fails, and counts how often the pipeline says it withheld a frame for it.
    struct HidingCounts(Arc<std::sync::Mutex<u32>>);

    impl CpuStep for HidingCounts {
        fn hides(&self) -> bool {
            true
        }
        fn withheld(&self, _ctx: &dyn FrameContext) {
            *self.0.lock().unwrap() += 1;
        }
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {
            panic!("broken")
        }
    }

    #[test]
    fn a_skipped_hiding_step_hears_that_its_frame_is_withheld() {
        let heard = Arc::new(std::sync::Mutex::new(0));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("blur", FrameStage::Privacy, HidingCounts(heard.clone()))]);
        for _ in 0..STEP_FAILS_BEFORE_SKIP {
            quietly(|| p.process(grey(4, 2, 200), &NoContext));
        }
        assert_eq!(*heard.lock().unwrap(), 0, "while it runs, it knows from process");
        assert!(is_dark(&p.process(grey(4, 2, 200), &NoContext)));
        assert_eq!(*heard.lock().unwrap(), 1, "skipped: told, though neither touch nor process is asked");
        assert!(is_dark(&p.hide_again(grey(4, 2, 200), &NoContext).expect("dark")));
        assert_eq!(*heard.lock().unwrap(), 2);
    }

    /// Hides, and does nothing to the picture.
    struct Hides;

    impl CpuStep for Hides {
        fn hides(&self) -> bool {
            true
        }
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
    }

    #[test]
    fn a_background_step_that_is_new_hides_the_frame_a_stalled_feed_keeps_sending() {
        let mut p = Pipeline::new();
        let went_out = grey(4, 2, 200);
        p.set_steps(vec![step("tag", FrameStage::Overlay, Idle)]);
        assert!(p.hide_again(went_out.clone(), &NoContext).is_none(), "no hiding step, nothing to hide");
        p.set_steps(vec![step("tag", FrameStage::Overlay, Idle), step("background", FrameStage::Background, Hides)]);
        let hidden = p.hide_again(went_out.clone(), &NoContext).expect("the room was in the picture that went out");
        assert!(is_dark(&hidden));
        assert_eq!(hidden.number, went_out.number);
        // Handed over again with the step already in: its picture went out with it, so there's nothing new to hide
        p.set_steps(vec![step("tag", FrameStage::Overlay, Idle), step("background", FrameStage::Background, Hides)]);
        assert!(p.hide_again(went_out.clone(), &NoContext).is_none());
        // Swapped for another of its kind (a new picture or blur): the same
        p.set_steps(vec![step("background", FrameStage::Background, Hides)]);
        assert!(p.hide_again(went_out.clone(), &NoContext).is_none());
        // Asked for once: a frame that went through the new steps settles it
        p.set_steps(Vec::new());
        p.set_steps(vec![step("background", FrameStage::Background, Hides)]);
        p.process(grey(4, 2, 1), &NoContext);
        assert!(p.hide_again(went_out, &NoContext).is_none());
    }

    #[test]
    fn a_frame_a_hiding_step_has_not_seen_goes_dark_once_the_history_is_forgotten() {
        let mut p = Pipeline::new();
        p.set_steps(vec![step("background", FrameStage::Background, Hides)]);
        let went_out = p.process(grey(4, 2, 200), &NoContext);
        assert!(p.hide_again(went_out.clone(), &NoContext).is_none(), "it went through the step, so nothing is new");
        p.forget_history();
        let hidden = p.hide_again(went_out.clone(), &NoContext).expect("after a gap nothing can vouch for the frame");
        assert!(is_dark(&hidden));
        assert_eq!(hidden.number, went_out.number);
        // A frame that goes through the step settles it again
        p.process(grey(4, 2, 1), &NoContext);
        assert!(p.hide_again(went_out, &NoContext).is_none());
    }

    /// Hides when told to: it paints the picture 9, and says it has nothing to do otherwise.
    struct HidesOnCue(Arc<std::sync::atomic::AtomicBool>);

    impl CpuStep for HidesOnCue {
        fn hides(&self) -> bool {
            true
        }
        fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
            if self.0.load(Ordering::SeqCst) { Touch::Changes } else { Touch::Nothing }
        }
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            frame.data_mut().fill(9);
        }
    }

    #[test]
    fn the_steps_that_hide_things_can_run_again_on_a_frame_that_already_went_out() {
        let on = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("blur", FrameStage::Privacy, HidesOnCue(on.clone())), step("grade", FrameStage::Colour, Push(3, seen.clone())), step("tag", FrameStage::Overlay, Push(4, seen.clone()))]);
        let went_out = grey(4, 2, 200);
        assert!(p.hide_again(went_out.clone(), &NoContext).is_none(), "nothing to hide, nothing to send again");
        on.store(true, Ordering::SeqCst);
        let hidden = p.hide_again(went_out.clone(), &NoContext).expect("hidden now");
        assert!(hidden.data.iter().all(|v| *v == 9));
        assert!(seen.lock().unwrap().is_empty(), "only the steps that hide ran, not the rest of the pipeline");
        assert_eq!(went_out.data[0], 200);
    }

    #[test]
    fn a_frame_sent_again_hidden_is_also_what_the_last_steps_see() {
        let on = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let read = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("blur", FrameStage::Privacy, HidesOnCue(on.clone())), step("spout", FrameStage::Final, Looks(read.clone()))]);
        let went_out = grey(4, 2, 200);
        assert!(p.hide_again(went_out.clone(), &NoContext).is_none());
        assert!(read.lock().unwrap().is_empty(), "nothing was hidden, so the last steps keep what they have");
        on.store(true, Ordering::SeqCst);
        let hidden = p.hide_again(went_out, &NoContext).expect("hidden now");
        assert_eq!(*read.lock().unwrap(), [hidden.data[0]], "the picture that goes out in place is the one they get");
        assert_eq!(hidden.data[0], 9);
    }

    #[test]
    fn a_replay_that_was_stopped_gives_the_last_steps_nothing() {
        let on = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let read = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("blur", FrameStage::Privacy, HidesOnCue(on)), step("spout", FrameStage::Final, Looks(read.clone()))]);
        struct Stopped;
        impl FrameContext for Stopped {
            fn source_id(&self) -> &str {
                ""
            }
            fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
                None
            }
            fn colour_preview(&self, frame: &Frame) -> Frame {
                frame.clone()
            }
            fn stopped(&self) -> bool {
                true
            }
        }
        p.hide_again(grey(4, 2, 200), &Stopped);
        assert!(read.lock().unwrap().is_empty());
    }

    /// Notes where the pixels were when the step got them and when it was done.
    struct Where(Arc<std::sync::Mutex<Vec<(usize, usize)>>>);

    impl CpuStep for Where {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            let was = frame.data.as_ptr() as usize;
            frame.data_mut()[0] = 3;
            self.0.lock().unwrap().push((was, frame.data.as_ptr() as usize));
        }
    }

    #[test]
    fn an_in_place_step_that_works_does_not_copy_the_picture() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("tag", FrameStage::Overlay, Where(seen.clone()))]);
        p.process(grey(64, 64, 1), &NoContext);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, seen[0].1, "changed where it was");
    }

    /// Breaks the picture halfway, then panics twice.
    struct BreaksOnce(Arc<std::sync::Mutex<u32>>);

    impl CpuStep for BreaksOnce {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            let n = {
                let mut n = self.0.lock().unwrap();
                *n += 1;
                *n
            };
            frame.data_mut()[0] = 200;
            if n <= 2 {
                panic!("broken")
            }
        }
    }

    #[test]
    fn a_panicking_in_place_step_hands_on_one_half_changed_frame_then_rolls_back_until_it_works_again() {
        let calls = Arc::new(std::sync::Mutex::new(0));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("tag", FrameStage::Overlay, BreaksOnce(calls.clone()))]);
        let out = quietly(|| p.process(grey(8, 8, 1), &NoContext));
        assert_eq!(out.data[0], 200, "accepted: the first failure hands on what the step had done");
        assert_eq!(p.steps[0].fails, 1);
        let out = quietly(|| p.process(grey(8, 8, 1), &NoContext));
        assert_eq!(out.data[0], 1, "a step that failed last frame is rolled back");
        let out = p.process(grey(8, 8, 1), &NoContext);
        assert_eq!(out.data[0], 200, "its good frame stands");
        assert_eq!(p.steps[0].fails, 0, "and it is back to running without a copy");
    }

    /// Writes a whole new picture into a spare buffer, then panics before swapping it in.
    struct RewritesThenPanics;

    impl CpuStep for RewritesThenPanics {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            let mut out = telescope_core::image::spare_bytes(frame.data.len());
            out.fill(9);
            if out.len() > 1 {
                panic!("before the swap")
            }
            frame.replace_data(out);
        }
    }

    #[test]
    fn a_whole_frame_writer_that_panics_leaves_the_frame_intact_with_no_copy() {
        let mut p = Pipeline::new();
        p.set_steps(vec![step("fx", FrameStage::Effects, RewritesThenPanics)]);
        let source = grey(8, 8, 1);
        let out = quietly(|| p.process(source.clone(), &NoContext));
        assert_eq!(out.data, source.data);
        assert!(Arc::ptr_eq(&out.data, &source.data), "not even copied");
        assert_eq!(p.steps[0].fails, 1);
    }

    #[test]
    fn steps_share_one_spare_pool_capped_by_bytes() {
        use telescope_core::image::{keep_bytes, spare_bytes};
        struct Hoards;
        impl CpuStep for Hoards {
            fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {
                let held: Vec<Vec<u8>> = (0..10).map(|_| spare_bytes(300_000)).collect();
                held.into_iter().for_each(keep_bytes);
            }
        }
        let mut p = Pipeline::new();
        p.set_steps(vec![step("a", FrameStage::Effects, Hoards), step("b", FrameStage::Overlay, Hoards)]);
        p.process(grey(256, 256, 1), &NoContext);
        let cap = SPARE_POOL_FRAMES * 256 * 256 * 3;
        assert!(p.pool.bytes() > 0 && p.pool.bytes() <= cap, "{} of {cap}", p.pool.bytes());
    }

    const INVERT: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> { let c = sample(uv); return vec4<f32>(1.0 - c.rgb, 1.0); }";
    const DIM: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv) * 0.5; }";

    fn shader(wgsl: &str) -> StagedStep {
        let step = ShaderStep { keeps_geometry: false, wgsl: wgsl.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        StagedStep { owner: "shader".into(), stage: FrameStage::Effects, step: FrameStep::Shader(step) }
    }

    fn card_pipeline(gpu: Arc<Gpu>) -> Pipeline {
        let mut pipeline = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        pipeline.gpu = Some(Some(gpu));
        pipeline.set_steps(vec![shader(INVERT)]);
        pipeline
    }

    #[test]
    fn a_card_picture_lands_once_across_readers_and_keeps_the_same_pixels_and_stamps() {
        use crate::transfers::measurement::measure;
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let gpu = Arc::new(gpu);
        let mut pipeline = card_pipeline(gpu.clone());
        let mut source = grey(17, 9, 100);
        let mut keep = shader(INVERT);
        if let FrameStep::Shader(ref mut shader) = keep.step {
            shader.keeps_geometry = true;
        }
        pipeline.set_steps(vec![keep]);
        source.set_alpha(Arc::new(Alpha::new(17, 9, vec![128; 17 * 9], false)));
        source.number = 42;
        source.capture_ns = Some(1234);
        source.mask_space = telescope_core::frame::MaskSpace { source_size: (1920, 1080), source_epoch: 2, warp: Some(4), crop_unsure: false };
        let mut eager = card_pipeline(gpu);
        let mut keep = shader(INVERT);
        if let FrameStep::Shader(ref mut shader) = keep.step {
            shader.keeps_geometry = true;
        }
        eager.set_steps(vec![keep]);
        let expected = eager.process(source.clone(), &NoContext);
        let (picture, costs) = measure(|| pipeline.process_picture(source.clone(), &NoContext));
        assert!(picture.picture().is_some());
        assert_eq!((costs.up, costs.down), (1, 0));
        let twin = picture.clone();
        let ((first, second), costs) = measure(|| (picture.land(), twin.land()));
        assert_eq!(costs.down, 1);
        assert_eq!(first.data, expected.data);
        assert_eq!(first.alpha(), expected.alpha());
        assert_eq!(first.mask_space, source.mask_space);
        assert_eq!(expected.mask_space, source.mask_space);
        assert!(Arc::ptr_eq(&first.data, &second.data));
        assert_eq!((first.number, first.time_ns, first.capture_ns), (42, source.time_ns, Some(1234)));
    }

    #[test]
    fn a_card_picture_fitted_to_the_camera_on_the_card_is_its_whole_landing_fitted_and_only_that_comes_down() {
        use crate::transfers::measurement::measure;
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let mut pipeline = card_pipeline(Arc::new(gpu));
        pipeline.set_steps(vec![shader(INVERT)]);
        let mut source = grey(64, 36, 100);
        Arc::make_mut(&mut source.data).iter_mut().enumerate().for_each(|(i, v)| *v = (i * 7 % 200 + 20) as u8);
        source.number = 9;
        let picture = pipeline.process_picture(source.clone(), &NoContext);
        let (fitted, costs) = measure(|| picture.land_fitted_until(40, 40, &|| false).unwrap());
        assert_eq!(costs.down, 1);
        assert!(picture.landed().is_none(), "the whole picture stayed on the card");
        assert_eq!((fitted.width, fitted.height, fitted.number), (40, 40, 9));
        assert_eq!(fitted.data.as_slice(), gpu::cpu_fit(&picture.land(), 40, 40).as_slice());
        // At the camera's own size there's nothing to fit, and a cut-out goes over the camera's background first, which the card doesn't do
        assert!(picture.land_fitted_until(64, 36, &|| false).is_none());
        let mut keep = shader(INVERT);
        if let FrameStep::Shader(ref mut shader) = keep.step {
            shader.keeps_geometry = true;
        }
        pipeline.set_steps(vec![keep]);
        source.set_alpha(Arc::new(Alpha::new(64, 36, vec![128; 64 * 36], false)));
        let cut_out = pipeline.process_picture(source, &NoContext);
        assert!(cut_out.picture().is_some());
        assert!(cut_out.land_fitted_until(40, 40, &|| false).is_none());
    }

    #[test]
    fn a_hiding_picture_the_card_cant_fit_is_left_to_land_whole_and_comes_out_dark() {
        use telescope_core::media::StandIn;
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let gpu = Arc::new(gpu);
        let mut pipeline = card_pipeline(gpu.clone());
        let mut hiding = shader(INVERT);
        hiding.stage = FrameStage::Privacy;
        pipeline.set_steps(vec![hiding]);
        let picture = pipeline.process_picture(grey(32, 18, 200), &NoContext);
        assert!(picture.fits_on_card(16, 16) && picture.stood_in().is_none());
        gpu.inject(gpu::Fault::Lost);
        assert!(picture.land_fitted_until(16, 16, &|| false).is_none());
        // Showing the room would be worse than a dark frame, so that is what a picture that hides things stands in with
        assert!(picture.land().data[..32 * 18].iter().all(|&p| p == 16));
        assert_eq!(picture.stood_in(), Some(StandIn::Dark));
        assert!(!picture.fits_on_card(16, 16));
    }

    #[test]
    fn a_picture_the_card_fails_to_land_says_it_stands_in_for_one_and_the_camera_is_told_not_to_send_it() {
        use telescope_core::media::StandIn;
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let gpu = Arc::new(gpu);
        let mut pipeline = card_pipeline(gpu.clone());
        let source = grey(32, 18, 200);
        // Landing fails after the fit did (a device lost between the two), and the other way round
        let picture = pipeline.process_picture(source.clone(), &NoContext);
        assert!(picture.stood_in().is_none());
        gpu.inject(gpu::Fault::Lost);
        let landed = picture.land();
        assert_eq!(picture.stood_in(), Some(StandIn::Unprocessed));
        assert_eq!(landed.data.as_slice(), source.data.as_slice(), "what the steps began from, uncropped and unshaded");
        // The same again: every landing of it is the one stand-in, and it is no more a picture to fit on the card
        assert_eq!(picture.land().data.as_slice(), landed.data.as_slice());
        assert!(picture.land_fitted_until(16, 16, &|| false).is_none());
        // A picture that landed fine says nothing of the kind
        let mut again = card_pipeline(Arc::new(Gpu::open().unwrap()));
        let fine = again.process_picture(source, &NoContext);
        fine.land();
        assert!(fine.stood_in().is_none());
    }

    #[test]
    fn a_crop_the_card_fails_to_land_leaves_the_uncropped_picture_marked_as_a_stand_in() {
        use telescope_core::host::View;
        use telescope_core::media::StandIn;
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let gpu = Arc::new(gpu);
        let mut pipeline = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        pipeline.gpu = Some(Some(gpu.clone()));
        pipeline.set_steps(view_staged(&View { zoom: 2.0, pan_x: 0.3, ..View::default() }));
        let picture = pipeline.process_picture(pattern(64, 36, PixelFormat::I420), &NoContext);
        assert!(picture.picture().is_some(), "the crop ran on the card");
        gpu.inject(gpu::Fault::Lost);
        picture.land();
        assert_eq!(picture.stood_in(), Some(StandIn::Unprocessed), "the camera is told this isn't the cropped picture");
    }

    #[test]
    fn a_picture_the_preview_is_landing_is_never_fitted_on_the_card_as_a_cut_out_would_be_without_its_alpha() {
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let mut pipeline = card_pipeline(Arc::new(gpu));
        let mut keep = shader(INVERT);
        if let FrameStep::Shader(ref mut shader) = keep.step {
            shader.keeps_geometry = true;
        }
        pipeline.set_steps(vec![keep]);
        let mut source = grey(64, 36, 100);
        source.set_alpha(Arc::new(Alpha::new(64, 36, vec![128; 64 * 36], false)));
        let cut_out = pipeline.process_picture(source, &NoContext);
        assert!(!cut_out.fits_on_card(40, 40));
        // Another thread's landing has begun: what it began from is its own now, and the picture hasn't landed yet
        let card = cut_out.picture().unwrap().as_any().downcast_ref::<gpu::CardFrame>().unwrap();
        card.begin_landing_for_test();
        assert!(cut_out.landed().is_none());
        assert!(!cut_out.fits_on_card(40, 40), "the camera could fit it without its alpha in that gap");
        assert!(cut_out.land_fitted_until(40, 40, &|| false).is_none());
    }

    #[test]
    fn a_picture_already_in_memory_is_not_read_back_again_to_be_fitted() {
        use crate::transfers::measurement::measure;
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let mut pipeline = card_pipeline(Arc::new(gpu));
        let picture = pipeline.process_picture(grey(64, 36, 100), &NoContext);
        let (fitted, costs) = measure(|| {
            picture.land();
            picture.land_fitted_until(40, 40, &|| false)
        });
        assert!(fitted.is_none(), "the camera fits it from memory");
        assert_eq!(costs.down, 1, "a second readback was made for the fit");
        assert!(picture.fits_on_card(40, 40), "it is still one the card could fit");
    }

    #[test]
    fn the_preview_draws_a_card_picture_repeatedly_without_landing() {
        use crate::transfers::measurement::measure;
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let gpu = Arc::new(gpu);
        let mut pipeline = card_pipeline(gpu.clone());
        let mut preview = gpu::CardPreview::new(gpu, 16);
        let (_, costs) = measure(|| {
            let picture = pipeline.process_picture(grey(32, 18, 100), &NoContext);
            assert!(preview.paint_picture(&picture, 16, 9, None).is_some());
            assert!(preview.paint_picture(&picture, 16, 9, None).is_some());
        });
        assert_eq!((costs.up, costs.down), (1, 0));
    }

    #[test]
    fn a_cpu_reader_inside_the_pipeline_lands_before_it_reads() {
        use crate::transfers::measurement::measure;
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let mut pipeline = card_pipeline(Arc::new(gpu));
        let read = Arc::new(std::sync::Mutex::new(Vec::new()));
        pipeline.set_steps(vec![shader(INVERT), step("reader", FrameStage::Final, Looks(read.clone()))]);
        let (picture, costs) = measure(|| pipeline.process_picture(grey(32, 18, 100), &NoContext));
        assert_eq!((costs.up, costs.down), (1, 1));
        assert!(picture.picture().is_none());
        assert_eq!(read.lock().unwrap().as_slice(), &[picture.land().data[0]]);
    }

    #[test]
    fn a_preview_on_another_device_paints_the_picture_only_once_something_else_landed_it() {
        use crate::transfers::measurement::measure;
        let (Ok(source), Ok(window)) = (Gpu::open(), Gpu::open()) else { return eprintln!("no graphics cards; skipped") };
        let picture = card_pipeline(Arc::new(source)).process_picture(grey(32, 18, 100), &NoContext);
        let mut preview = gpu::CardPreview::new(Arc::new(window), 16);
        let (_, costs) = measure(|| {
            assert!(preview.paint_picture(&picture, 16, 9, None).is_none(), "the window's painter doesn't read a picture back itself");
            picture.land();
            assert!(preview.paint_picture(&picture, 16, 9, None).is_some());
            assert!(preview.paint_picture(&picture, 16, 9, None).is_some());
        });
        assert_eq!(costs.down, 1);
    }

    #[test]
    fn a_lost_card_uses_a_safe_picture_and_the_next_frame_runs_on_the_cpu() {
        use crate::transfers::measurement::measure;
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let gpu = Arc::new(gpu);
        let (mut pipeline, _) = either(Place::Card);
        pipeline.gpu = Some(Some(gpu.clone()));
        let source = grey(8, 8, 100);
        let picture = pipeline.process_picture(source.clone(), &NoContext);
        gpu.inject(gpu::Fault::Lost);
        let (out, costs) = measure(|| picture.land());
        assert_eq!(costs.down, 1);
        assert_eq!(out.data, source.data);
        let (next, costs) = measure(|| pipeline.process_picture(source, &NoContext).land());
        assert_eq!((costs.up, costs.down), (0, 0));
        assert_eq!(next.data[0], 167);
        assert!(pipeline.card_lost);
    }

    #[test]
    fn losing_a_deferred_hiding_picture_never_returns_the_clear_pixels() {
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let gpu = Arc::new(gpu);
        let mut pipeline = card_pipeline(gpu.clone());
        let mut hiding = shader(INVERT);
        hiding.stage = FrameStage::Privacy;
        pipeline.set_steps(vec![hiding]);
        let picture = pipeline.process_picture(grey(32, 18, 200), &NoContext);
        gpu.inject(gpu::Fault::Lost);
        let out = picture.land();
        assert!(out.data[..32 * 18].iter().all(|&p| p == 16));
        assert!(out.alpha().is_none());
        assert_eq!(picture.land().data, out.data);
    }

    /// Only looks, and notes the luma it was shown.
    struct Looks(Arc<std::sync::Mutex<Vec<u8>>>);

    impl CpuStep for Looks {
        fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
            Touch::Reads
        }
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            self.0.lock().unwrap().push(frame.data[0]);
        }
    }

    /// Sits this frame out.
    struct Idle;

    impl CpuStep for Idle {
        fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
            Touch::Nothing
        }
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {
            panic!("an idle step isn't run");
        }
    }

    /// Says it only reads and then changes the picture.
    struct Lies;

    impl CpuStep for Lies {
        fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
            Touch::Reads
        }
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            let (w, h) = ((frame.width * frame.height) as usize, frame.width as usize * frame.height as usize / 2);
            *frame.data_mut() = [vec![235; w], vec![128; h]].concat();
        }
    }

    fn cpu(s: impl CpuStep + 'static) -> StagedStep {
        step("cpu", FrameStage::Effects, s)
    }

    #[test]
    fn a_frame_crosses_as_few_times_as_the_order_allows() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let noted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let chains: Vec<(&str, Vec<StagedStep>, (u32, u32))> = vec![
            ("nothing on the card", vec![cpu(Rebuilds)], (0, 0)),
            ("a run of shaders", vec![shader(INVERT), shader(DIM), shader(INVERT)], (1, 1)),
            ("an idle step doesn't split a run", vec![shader(INVERT), cpu(Idle), shader(DIM)], (1, 1)),
            ("a reader takes a copy", vec![shader(INVERT), cpu(Looks(noted.clone())), shader(DIM)], (1, 2)),
            ("a reader at the end costs nothing more", vec![shader(INVERT), shader(DIM), cpu(Looks(noted.clone()))], (1, 1)),
            ("a step that changes the picture splits it", vec![shader(INVERT), cpu(Rebuilds), shader(DIM)], (2, 2)),
            ("a CPU step first costs nothing", vec![cpu(Rebuilds), shader(INVERT), shader(DIM)], (1, 1)),
        ];
        for (what, steps, want) in chains {
            let mut p = Pipeline::new();
            p.set_steps(steps);
            p.process(grey(8, 8, 126), &NoContext);
            assert_eq!(p.last_transfers, want, "{what}");
        }
    }

    /// Reads the picture and takes its time over it, noting how long it really took.
    struct Slow(Arc<std::sync::atomic::AtomicU64>);

    impl CpuStep for Slow {
        fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
            Touch::Reads
        }
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {
            let began = monotonic_ns();
            std::thread::sleep(std::time::Duration::from_millis(2));
            self.0.fetch_add(monotonic_ns() - began, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_step_isnt_charged_for_bringing_the_picture_down_from_the_card() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let hub = telescope_core::stats::Hub::new();
        let stats = hub.register("timing", 1);
        hub.want("test", true);
        let own = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut p = Pipeline::new();
        p.set_stats(stats.clone());
        p.set_steps(vec![shader(INVERT), step("reader", FrameStage::Effects, Slow(own.clone()))]);
        let mut readback_ns = 0;
        for _ in 0..6 {
            p.process(grey(1920, 1080, 126), &NoContext);
            readback_ns += p.last_readback_ns;
        }
        assert!(readback_ns > 0, "the readback was measured");
        let timing = stats.steps().timer("reader").take();
        assert_eq!(timing.count, 6);
        let (own_ms, readback_ms) = (own.load(Ordering::SeqCst) as f64 / 6e6, readback_ns as f64 / 6e6);
        // Its own time (the sleep and a few microseconds of bookkeeping), without the readback that came before it
        assert!(timing.mean_ms as f64 <= own_ms + 0.3, "charged {:.2} ms for a step that took {own_ms:.2} ms, with a readback of {readback_ms:.2} ms in front of it", timing.mean_ms);
    }

    #[test]
    fn the_card_lets_go_of_what_it_kept_when_the_last_stream_stops() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let mut p = Pipeline::new();
        p.set_steps(vec![shader(INVERT)]);
        p.process(grey(64, 64, 126), &NoContext);
        assert!(gpu.kept_bytes() > 0, "a frame leaves its textures for the next");
        drop(p);
        // another test's stream may be running; only when none is, nothing stays kept
        assert!(gpu.kept_bytes() == 0 || ON_THE_CARD.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn a_mirrored_view_never_visits_the_card() {
        use telescope_core::host::View;
        let mut p = Pipeline::new();
        let view = telescope_core::view_step::steps(&View { flip_h: true, ..View::default() });
        p.set_steps(view.into_iter().map(|step| StagedStep { owner: "view".into(), stage: FrameStage::View, step }).collect());
        let mut frame = grey(8, 8, 10);
        frame.data_mut()[0] = 99;
        let out = p.process(frame, &NoContext);
        assert_eq!(p.last_transfers, (0, 0), "a busy card can't hold the frame up");
        assert_eq!(out.data[7], 99, "and the picture is still mirrored");
    }

    #[test]
    fn a_stretched_view_never_visits_the_card_and_a_zoom_only_while_the_card_has_room() {
        use telescope_core::host::View;
        for view in [View { stretch_x: 0.5, ..View::default() }, View { rotation: Some(90), zoom: 1.5, stretch_y: 1.2, ..View::default() }, View { zoom: 2.0, ..View::default() }] {
            let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Cpu)));
            p.set_steps(view_staged(&view));
            let out = p.process(grey(8, 8, 10), &NoContext);
            assert_eq!(p.last_transfers, (0, 0), "{view:?}: a busy card can't hold the frame up");
            assert_eq!(out.data.len(), out.format.size(out.width, out.height), "{view:?}: a whole picture comes out");
            if view.zoom > 1.0 {
                continue;
            }
            let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
            p.set_steps(view_staged(&view));
            p.process(grey(8, 8, 10), &NoContext);
            assert_eq!(p.last_transfers, (0, 0), "{view:?}: a stretch stays on the CPU");
        }
    }

    fn view_staged(view: &telescope_core::host::View) -> Vec<StagedStep> {
        telescope_core::view_step::steps(view).into_iter().map(|step| StagedStep { owner: "view".into(), stage: FrameStage::View, step }).collect()
    }

    /// Every byte different from its neighbours, in a smooth pattern a crop can be compared on.
    fn pattern(w: u32, h: u32, format: PixelFormat) -> Frame {
        let data = (0..format.size(w, h)).map(|i| {
            let (x, y) = ((i % w as usize) as f64, (i / w as usize) as f64);
            (128.0 + 60.0 * (x * 0.21).sin() + 50.0 * (y * 0.13 + x * 0.05).cos()) as u8
        }).collect();
        Frame::new(w, h, format, data)
    }

    /// The biggest and the mean difference over every byte.
    fn gap(a: &Frame, b: &Frame) -> (u8, f64) {
        assert_eq!((a.width, a.height, a.format, a.data.len()), (b.width, b.height, b.format, b.data.len()));
        let worst = a.data.iter().zip(b.data.iter()).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        let sum: u64 = a.data.iter().zip(b.data.iter()).map(|(x, y)| x.abs_diff(*y) as u64).sum();
        (worst, sum as f64 / a.data.len() as f64)
    }

    #[test]
    fn the_crop_on_the_card_gives_the_cpus_picture_and_geometry() {
        use telescope_core::host::View;
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let views = [View { zoom: 2.0, pan_x: 0.3, pan_y: -0.6, ..View::default() }, View { zoom: 1.37, flip_h: true, ..View::default() },
                     View { zoom: 3.1, pan_x: 1.0, pan_y: 1.0, rotation: Some(180), ..View::default() }, View { zoom: 1.8, rotation: Some(90), flip_v: true, ..View::default() }];
        for format in [PixelFormat::I420, PixelFormat::Rgba] {
            for (w, h) in [(320, 180), (318, 178)] {
                for view in &views {
                    let frame = pattern(w, h, format);
                    let mut card = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
                    card.set_steps(view_staged(view));
                    let on_card = card.process(frame.clone(), &NoContext);
                    assert!(card.last_transfers.0 == 1, "{view:?}: the crop ran on the card");
                    let mut cpu = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Cpu)));
                    cpu.set_steps(view_staged(view));
                    let on_cpu = cpu.process(frame.clone(), &NoContext);
                    let (worst, mean) = gap(&on_card, &on_cpu);
                    eprintln!("{format:?} {w}x{h} {view:?}: max {worst} mean {mean:.3}");
                    assert!(worst <= 2 && mean <= 0.25, "{format:?} {w}x{h} {view:?}: max {worst} mean {mean:.3}");
                    assert_eq!(on_card.geometry, on_cpu.geometry, "the card records where the picture came from as the CPU does");
                    assert!(on_card.geometry.is_some());
                }
            }
        }
    }

    #[test]
    fn a_crop_made_again_keeps_its_shader_across_sizes_and_formats() {
        use telescope_core::host::View;
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let mut card = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        let mut cpu = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Cpu)));
        let first = View { zoom: 2.0, pan_x: 0.3, ..View::default() };
        card.set_steps(view_staged(&first));
        cpu.set_steps(view_staged(&first));
        card.process(pattern(320, 180, PixelFormat::I420), &NoContext);
        let built = |p: &mut Pipeline| p.steps[0].kind.slot_mut().is_some_and(|slot| slot.is_built());
        assert!(built(&mut card), "the first frame built it");
        // Each new view takes the built shader over, and the frames after it (another size, another format) are still the CPU's
        for (n, (w, h, format)) in [(320, 180, PixelFormat::I420), (318, 178, PixelFormat::I420), (318, 178, PixelFormat::Rgba), (640, 360, PixelFormat::I420)].into_iter().enumerate() {
            let view = View { zoom: 1.5 + n as f32 * 0.4, pan_x: 0.3 - n as f32 * 0.2, flip_h: n == 2, ..View::default() };
            card.replace_steps("view", FrameStage::View, telescope_core::view_step::steps(&view));
            cpu.replace_steps("view", FrameStage::View, telescope_core::view_step::steps(&view));
            assert!(built(&mut card), "{view:?}: made again, it was built already");
            let frame = pattern(w, h, format);
            let (on_card, on_cpu) = (card.process(frame.clone(), &NoContext), cpu.process(frame, &NoContext));
            assert_eq!(card.last_transfers.0, 1, "{view:?}: still on the card");
            let (worst, mean) = gap(&on_card, &on_cpu);
            assert!(worst <= 2 && mean <= 0.25, "{w}x{h} {format:?} {view:?}: max {worst} mean {mean:.3}");
            assert_eq!(on_card.geometry, on_cpu.geometry);
        }
    }

    #[test]
    fn a_step_with_no_geometry_to_record_leaves_the_frames_own_on_the_card() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let (mut p, _) = either(Place::Card);
        let mut frame = grey(8, 8, 100);
        frame.geometry = Some(Box::new(telescope_core::view::Geometry { zoom: 2.0, ..Default::default() }));
        let out = p.process(frame.clone(), &NoContext);
        assert_eq!(p.last_transfers, (1, 1));
        assert_eq!(out.geometry, frame.geometry);
    }

    #[test]
    fn a_crop_that_moves_a_little_every_frame_moves_as_smoothly_and_never_flashes_whichever_way_it_runs() {
        use telescope_core::host::View;
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        // A ramp across: where the picture's left edge comes from shows in the first column's value
        let (w, h) = (256u32, 64u32);
        let mut frame = Frame::new(w, h, PixelFormat::I420, vec![128; PixelFormat::I420.size(w, h)]);
        for (i, v) in frame.data_mut()[..(w * h) as usize].iter_mut().enumerate() {
            *v = (16 + (i % w as usize) * 220 / w as usize) as u8;
        }
        let balance = Arc::new(Balance::free());
        // Held on the card until the move, so a slow software card's real waits can't move it early
        balance.keep_on_card(true);
        let mut p = Pipeline::with_balance(balance.clone());
        // The same frames on the CPU alone, as the stream gave them before
        let mut cpu = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Cpu)));
        let (mut lefts, mut means, mut places, mut gaps) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for n in 0..120 {
            // The card gets busy for a while in the middle and the steps move to the CPU, as a game starting would do
            if n == 40 {
                balance.keep_on_card(false);
                (0..8).for_each(|_| p.lane().note_wait(50_000_000));
            }
            // Framing on at the start, a slow pan as Auto framing makes, a zoom change, and off again at the end
            let view = if !(5..115).contains(&n) { View::default() } else { View { zoom: if n < 80 { 2.0 } else { 2.0 + (n - 80) as f32 * 0.01 }, pan_x: -0.8 + n as f32 * 0.012, ..View::default() } };
            p.replace_steps("view", FrameStage::View, telescope_core::view_step::steps(&view));
            cpu.replace_steps("view", FrameStage::View, telescope_core::view_step::steps(&view));
            let out = p.process(frame.clone(), &NoContext);
            gaps.push(gap(&out, &cpu.process(frame.clone(), &NoContext)));
            places.push(p.lane().place());
            let row = (h / 2 * w) as usize;
            lefts.push(out.data[row] as i32);
            means.push(out.data[..(w * h) as usize].iter().map(|&v| v as f64).sum::<f64>() / (w * h) as f64);
        }
        assert!(places[..40].iter().all(|&p| p == Place::Card) && places[40..].iter().all(|&p| p == Place::Cpu), "it moved once: {places:?}");
        // The left edge only ever moves right while the pan does, by about the same each frame
        let moving = &lefts[5..80];
        assert!(moving.windows(2).all(|p| (-1..=3).contains(&(p[1] - p[0]))), "the pan jumped: {moving:?}");
        assert!(moving[moving.len() - 1] - moving[0] > 30, "the pan moved the picture: {moving:?}");
        assert!(means.iter().all(|&m| m > 40.0), "a frame went dark: {means:?}");
        // Every frame is the CPU's picture to a level, before, during and after the move, so neither switch shows
        assert!(gaps.iter().all(|&(worst, mean)| worst <= 2 && mean <= 0.5), "a frame strayed from the CPU's: {gaps:?}");
    }

    #[test]
    fn a_pan_that_goes_back_and_forth_between_the_card_and_the_cpu_never_jumps() {
        use telescope_core::host::View;
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let (w, h) = (256u32, 64u32);
        let mut frame = Frame::new(w, h, PixelFormat::I420, vec![128; PixelFormat::I420.size(w, h)]);
        for (i, v) in frame.data_mut()[..(w * h) as usize].iter_mut().enumerate() {
            *v = (16 + (i % w as usize) * 220 / w as usize) as u8;
        }
        let (on_card, on_cpu) = (Arc::new(Balance::pinned_to(Place::Card)), Arc::new(Balance::pinned_to(Place::Cpu)));
        let mut p = Pipeline::with_balance(on_card.clone());
        let mut cpu = Pipeline::with_balance(on_cpu.clone());
        let (mut lefts, mut ups) = (Vec::new(), Vec::new());
        for n in 0..90 {
            // Every seventh frame the card gets busy or has room again, the first time with the shader not built yet
            p.balance = if (n / 7) % 2 == 0 { on_card.lane() } else { on_cpu.lane() };
            let view = View { zoom: 2.0, pan_x: -0.8 + n as f32 * 0.015, ..View::default() };
            p.replace_steps("view", FrameStage::View, telescope_core::view_step::steps(&view));
            cpu.replace_steps("view", FrameStage::View, telescope_core::view_step::steps(&view));
            let out = p.process(frame.clone(), &NoContext);
            ups.push(p.last_transfers.0);
            let (worst, mean) = gap(&out, &cpu.process(frame.clone(), &NoContext));
            assert!(worst <= 2 && mean <= 0.25, "frame {n}: max {worst} mean {mean:.3}");
            lefts.push(out.data[(h / 2 * w) as usize] as i32);
        }
        assert!(ups.iter().enumerate().all(|(n, &up)| up == u32::from((n / 7) % 2 == 0)), "the card did every other seven frames and the CPU the rest: {ups:?}");
        assert!(lefts.windows(2).all(|pair| (-1..=3).contains(&(pair[1] - pair[0]))), "the pan jumped: {lefts:?}");
    }

    #[test]
    fn a_skipped_step_doesnt_split_a_run_either() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let mut p = Pipeline::new();
        p.set_steps(vec![shader(INVERT), cpu(Panics), shader(DIM)]);
        p.steps[1].fails = STEP_FAILS_BEFORE_SKIP;
        p.process(grey(8, 8, 126), &NoContext);
        assert_eq!(p.last_transfers, (1, 1));
    }

    #[test]
    fn keeping_the_picture_up_doesnt_change_what_comes_out_or_what_a_reader_sees() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let run = |steps: Vec<StagedStep>| {
            let mut p = Pipeline::new();
            p.set_steps(steps);
            p.process(grey(8, 8, 200), &NoContext).data[0]
        };
        // Taken apart by a step that changes nothing but says it might, as every CPU step used to
        struct Nothing;
        impl CpuStep for Nothing {
            fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
        }
        let apart = run(vec![shader(INVERT), cpu(Nothing), shader(DIM), cpu(Nothing), shader(INVERT)]);
        let noted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let together = run(vec![shader(INVERT), cpu(Looks(noted.clone())), shader(DIM), cpu(Idle), shader(INVERT)]);
        assert!((apart as i32 - together as i32).abs() <= 1, "{apart} {together}");
        let inverted = run(vec![shader(INVERT)]);
        assert_eq!(*noted.lock().unwrap(), [inverted], "the reader sees the picture as it is at its place in the order");
        // Order counts: dimming before inverting isn't inverting before dimming
        assert!(run(vec![shader(DIM), shader(INVERT)]) > run(vec![shader(INVERT), shader(DIM)]) + 50);
    }

    #[test]
    fn a_step_that_only_said_it_reads_still_wins() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let mut p = Pipeline::new();
        p.set_steps(vec![shader(INVERT), cpu(Lies), shader(DIM)]);
        let out = p.process(grey(8, 8, 235), &NoContext);
        // Its white dimmed to about half: the card's copy from before it was the inverted picture's
        assert_eq!(p.last_transfers, (2, 2));
        assert!((out.data[0] as i32 - 126).abs() <= 3, "{}", out.data[0]);
    }

    /// Answers person_mask with what it's given, and counts how often it's asked.
    struct Masked(std::sync::Mutex<Option<Arc<Vec<u8>>>>, std::sync::atomic::AtomicU32);

    impl FrameContext for Masked {
        fn source_id(&self) -> &str {
            ""
        }
        fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.0.lock().unwrap().clone()
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
    }

    const SHOW_MASK: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> {
    if !has_person_mask() { return vec4<f32>(0.5, 0.5, 0.5, 1.0); }
    return vec4<f32>(vec3<f32>(person_mask(uv)), 1.0);
}";

    fn half_mask(left: u8, right: u8) -> Arc<Vec<u8>> {
        Arc::new((0..16 * 8).map(|i| if i % 16 < 8 { left } else { right }).collect())
    }

    #[test]
    fn a_shader_reads_the_person_mask_and_it_only_goes_up_when_it_changes() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let luma = |f: &Frame, x: usize| f.data[x] as i32;
        let ctx = Masked(std::sync::Mutex::new(None), Default::default());
        let mut p = Pipeline::new();
        p.set_steps(vec![shader(SHOW_MASK)]);
        // No mask yet: the shader can tell, and nothing goes up
        let before = transfers::totals().mask_up;
        let out = p.process(grey(16, 8, 16), &ctx);
        assert!((luma(&out, 0) - 126).abs() <= 3, "{}", luma(&out, 0));

        *ctx.0.lock().unwrap() = Some(half_mask(255, 0));
        for _ in 0..5 {
            let out = p.process(grey(16, 8, 16), &ctx);
            assert!(luma(&out, 0) >= 232 && luma(&out, 15) <= 19, "{} {}", luma(&out, 0), luma(&out, 15));
        }
        assert_eq!(transfers::totals().mask_up - before, 1, "the same mask stays on the card");
        // A new Arc with the same pixels isn't a change either
        *ctx.0.lock().unwrap() = Some(half_mask(255, 0));
        p.process(grey(16, 8, 16), &ctx);
        assert_eq!(transfers::totals().mask_up - before, 1);
        *ctx.0.lock().unwrap() = Some(half_mask(0, 255));
        let out = p.process(grey(16, 8, 16), &ctx);
        assert_eq!(transfers::totals().mask_up - before, 2);
        assert!(luma(&out, 0) <= 19 && luma(&out, 15) >= 232, "{} {}", luma(&out, 0), luma(&out, 15));
        // A mask of the wrong size isn't bound, and the shader says there's none
        *ctx.0.lock().unwrap() = Some(Arc::new(vec![255; 4]));
        let out = p.process(grey(16, 8, 16), &ctx);
        assert!((luma(&out, 0) - 126).abs() <= 3, "{}", luma(&out, 0));
    }

    /// Answers person_mask_at with a left half that is a person, and notes the sizes it was asked at; the full size it has none of.
    struct MaskedSmall(std::sync::Mutex<Vec<(usize, usize)>>);

    impl FrameContext for MaskedSmall {
        fn source_id(&self) -> &str {
            ""
        }
        fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
            self.0.lock().unwrap().push((0, 0));
            None
        }
        fn person_mask_at(&self, _frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
            self.0.lock().unwrap().push((w, h));
            Some(Arc::new((0..w * h).map(|i| if i % w < w / 2 { 255 } else { 0 }).collect()))
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
    }

    #[test]
    fn a_big_frames_mask_goes_up_small_and_a_small_frames_at_its_own_size() {
        assert_eq!([mask_up_size(3840, 2160), mask_up_size(1920, 1080), mask_up_size(1280, 720), mask_up_size(640, 480), mask_up_size(16, 8), mask_up_size(1080, 1920)], [(640, 360), (640, 360), (640, 360), (640, 480), (16, 8), (640, 1138)]);
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let ctx = MaskedSmall(Default::default());
        let mut p = Pipeline::new();
        p.set_steps(vec![shader(SHOW_MASK)]);
        let before = transfers::totals().mask_up;
        let out = p.process(grey(1280, 720, 16), &ctx);
        assert_eq!(*ctx.0.lock().unwrap(), [(640, 360)]);
        assert_eq!(transfers::totals().mask_up - before, 1);
        let (left, right) = (out.data[100 * 1280 + 100] as i32, out.data[100 * 1280 + 1180] as i32);
        assert!(left >= 232 && right <= 19, "{left} {right}");
    }

    #[test]
    fn a_mask_is_asked_for_once_a_frame_of_the_picture_as_it_reaches_the_step() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let ctx = Masked(std::sync::Mutex::new(None), Default::default());
        let mut p = Pipeline::new();
        p.set_steps(vec![shader(SHOW_MASK), shader(SHOW_MASK)]);
        p.process(grey(16, 8, 16), &ctx);
        assert_eq!(ctx.1.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(p.last_transfers, (1, 1), "the first thing on the card asks with the frame in hand");
        p.set_steps(vec![shader(INVERT), shader(SHOW_MASK), shader(SHOW_MASK)]);
        p.process(grey(16, 8, 16), &ctx);
        assert_eq!(ctx.1.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(p.last_transfers, (1, 2), "after a shader the picture has to come down for the model, once");
        // A shader that doesn't ask costs no mask at all
        p.set_steps(vec![shader(INVERT)]);
        p.process(grey(16, 8, 16), &ctx);
        assert_eq!(ctx.1.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn a_step_that_keeps_its_last_frame_goes_on_from_it_when_its_settings_change() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        // 0.2 on the first frame, then the last frame's red and 0.2 more
        let counts = |plus: f64| ShaderStep {
            keeps_geometry: false,
            wgsl: "struct Params { plus: f32 }\nfn effect(uv: vec2<f32>) -> vec4<f32> { if has_previous() { return vec4<f32>(previous(uv).r + params.plus, 0.0, 0.0, 1.0); } return vec4<f32>(0.2, 0.0, 0.0, 1.0); }".into(),
            uniforms: serde_json::json!({"plus": plus}),
            textures: Vec::new(),
        };
        let staged = |owner: &str, plus: f64| StagedStep { owner: owner.into(), stage: FrameStage::Effects, step: FrameStep::Shader(counts(plus)) };
        let frame = |n: u64| {
            let mut f = Frame::new(8, 4, PixelFormat::Rgba, vec![0; 8 * 4 * 4]);
            f.time_ns = 1_000_000_000 + n * 33_000_000;
            f
        };
        let mut p = Pipeline::new();
        p.set_steps(vec![staged("filters", 0.2)]);
        assert_eq!(p.process(frame(0), &NoContext).data[0], 51);
        assert_eq!(p.process(frame(1), &NoContext).data[0], 102);
        // new settings (and any other plugin's slider does this too): the same step again, and it counts on
        // 0.4 + 0.15 is 140.25 levels, not on a halfway point that cards round differently
        p.set_steps(vec![staged("filters", 0.15)]);
        assert_eq!(p.process(frame(2), &NoContext).data[0], 140);
        // another owner's step, or another shader, starts from nothing
        p.set_steps(vec![staged("other", 0.1)]);
        assert_eq!(p.process(frame(3), &NoContext).data[0], 51);
    }

    /// A shader and a CPU step that do the same, as a step that can run either way, with the balance in the test's hands.
    fn either(balance: Place) -> (Pipeline, Arc<Balance>) {
        let balance = Arc::new(Balance::pinned_to(balance));
        let mut p = Pipeline::with_balance(balance.clone());
        struct Invert;
        impl CpuStep for Invert {
            fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
                frame.data_mut().iter_mut().for_each(|v| *v = 251 - *v + 16);
            }
        }
        let step = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![StagedStep { owner: "either".into(), stage: FrameStage::Effects, step: FrameStep::Either(step, Box::new(Invert)) }]);
        (p, balance)
    }

    #[test]
    fn a_step_with_two_ways_goes_where_the_balance_says() {
        let (mut p, _) = either(Place::Cpu);
        let out = p.process(grey(8, 8, 100), &NoContext);
        assert_eq!(p.last_transfers, (0, 0), "nothing on the card");
        assert_eq!(out.data[0], 167);
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let (mut p, balance) = either(Place::Card);
        let out = p.process(grey(8, 8, 100), &NoContext);
        assert_eq!(p.last_transfers, (1, 1));
        assert!((out.data[0] as i32 - 151).abs() <= 3, "{}", out.data[0]);
        assert!(balance.last_wait() > 0, "the balance hears what the frame waited");
    }

    #[test]
    fn a_busy_card_moves_the_step_from_the_next_frame_on() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let balance = Arc::new(Balance::free());
        let (mut p, _) = either(Place::Card);
        p.balance = balance.lane();
        p.process(grey(8, 8, 100), &NoContext);
        assert_eq!(p.last_transfers, (1, 1));
        (0..8).for_each(|_| p.balance.note_wait(50_000_000));
        assert_eq!(p.balance.place(), Place::Cpu);
        p.process(grey(8, 8, 100), &NoContext);
        assert_eq!(p.last_transfers, (0, 0));
    }

    #[test]
    fn a_step_off_the_card_tells_the_balance_what_it_cost_the_cpu_and_the_stream_s_rate_sets_the_late_line() {
        let balance = Arc::new(Balance::free());
        let (mut p, _) = either(Place::Card);
        p.balance = balance.lane();
        let hub = telescope_core::stats::Hub::new();
        let stats = hub.register("cost", 1);
        stats.set_target_fps(60);
        p.set_stats(stats);
        (0..8).for_each(|_| p.balance.note_wait(50_000_000));
        assert_eq!((p.balance.place(), p.balance.cpu_cost()), (Place::Cpu, None));
        for _ in 0..10 {
            p.process(grey(64, 64, 100), &NoContext);
        }
        assert!(p.balance.cpu_cost().is_some(), "ten frames on the CPU were heard");
        assert_eq!(p.balance.late_ns() / 1000, 12_499, "three quarters of a 60 fps frame");
        let other = Balance::lane(&balance);
        assert_eq!((other.cpu_cost(), other.late_ns() / 1000), (None, 24_999), "another stream's rate and cost are its own");
    }

    #[test]
    fn the_decoder_s_copy_counts_for_the_cpu_only_when_the_card_s_way_would_have_spared_it() {
        let (mut p, _) = either(Place::Card);
        p.balance = Arc::new(Balance::free()).lane();
        (0..8).for_each(|_| p.balance.note_wait(50_000_000));
        // A decoder that couldn't have left the picture on the card notes nothing: every way pays for its copy
        for _ in 0..8 {
            p.process(grey(8, 8, 100), &NoContext);
        }
        assert!(p.balance.cpu_cost().unwrap() < 40_000_000, "{:?}", p.balance.cpu_cost());
        // One that could have notes the copy on the very frame it made
        p.balance.work_changed();
        for _ in 0..8 {
            p.process(grey(8, 8, 100).with_copied_off_card(40_000_000), &NoContext);
        }
        assert!(p.balance.cpu_cost().unwrap() >= 40_000_000, "{:?}", p.balance.cpu_cost());
        // A step whose shader samples pictures made in memory has the decoder's picture brought there on the card too
        struct Masked;
        impl CpuStep for Masked {
            fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
            fn card_textures_offered(&self) -> Option<bool> {
                Some(true)
            }
        }
        let step = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![StagedStep { owner: "masked".into(), stage: FrameStage::Effects, step: FrameStep::Either(step, Box::new(Masked)) }]);
        for _ in 0..8 {
            p.process(grey(8, 8, 100).with_copied_off_card(40_000_000), &NoContext);
        }
        assert!(p.balance.cpu_cost().unwrap() < 40_000_000, "paid either way: {:?}", p.balance.cpu_cost());
    }

    #[test]
    fn a_picture_of_another_size_is_measured_again_and_waits_of_steps_that_cant_move_are_not_heard() {
        let (mut p, _) = either(Place::Card);
        p.balance = Arc::new(Balance::free()).lane();
        (0..8).for_each(|_| p.balance.note_wait(50_000_000));
        for _ in 0..8 {
            p.process(grey(64, 64, 100), &NoContext);
        }
        assert!(p.balance.cpu_cost().is_some());
        p.process(grey(32, 32, 100), &NoContext);
        assert_eq!(p.balance.cpu_cost(), None, "another size costs something else");
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        // A shader that only runs on the card waits for it on every frame, but moving nothing can't help that
        let balance = Arc::new(Balance::free());
        let mut p = Pipeline::with_balance(balance.clone());
        p.set_steps(vec![shader(INVERT)]);
        p.process(grey(64, 64, 100), &NoContext);
        assert_eq!(balance.last_wait(), 0, "no wait heard");
    }

    #[test]
    fn a_step_off_the_card_isn_t_charged_for_bringing_a_card_step_s_picture_down() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let balance = Arc::new(Balance::free());
        let mut p = Pipeline::with_balance(balance.clone());
        let own = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let either = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![shader(INVERT), StagedStep { owner: "either".into(), stage: FrameStage::Effects, step: FrameStep::Either(either, Box::new(Slow(own.clone()))) }]);
        (0..8).for_each(|_| p.balance.note_wait(50_000_000));
        let mut readback_ns = 0;
        for _ in 0..8 {
            p.process(grey(1920, 1080, 126), &NoContext);
            readback_ns += p.last_readback_ns;
        }
        assert!(readback_ns > 0, "the shader's picture came down");
        let (own_ms, readback_ms) = (own.load(Ordering::SeqCst) as f64 / 8e6, readback_ns as f64 / 8e6);
        let cost_ms = p.balance.cpu_cost().unwrap() as f64 / 1e6;
        assert!(cost_ms <= own_ms + 0.3, "the CPU's cost was {cost_ms:.2} ms for a step that took {own_ms:.2} ms, with a readback of {readback_ms:.2} ms in front of it");
    }

    #[test]
    fn a_shader_that_wont_build_leaves_a_step_with_two_ways_on_the_cpu() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let (mut p, _) = either(Place::Card);
        let broken = ShaderStep { keeps_geometry: false, wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return nope; }".into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        let Kind::Either(slot, _) = &mut p.steps[0].kind else { panic!("two ways") };
        *slot = gpu::ShaderSlot::new(broken);
        for _ in 0..3 {
            assert_eq!(p.process(grey(8, 8, 100), &NoContext).data[0], 167);
            assert_eq!(p.last_transfers, (0, 0));
        }
        assert!(!p.steps[0].skipped() && p.steps[0].no_shader);
    }

    // Opening a second connection to the card rescans the Vulkan drivers, and that crashes the loader under other tests' threads
    #[test]
    fn a_lost_card_leaves_a_step_with_two_ways_on_the_cpu_for_the_stream() {
        let name = "pipeline::tests::lost_card_in_a_process_of_its_own";
        let run = std::process::Command::new(std::env::current_exe().unwrap()).args(["--ignored", "--exact", name, "--nocapture"]).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stdout));
    }

    #[test]
    #[ignore = "a_lost_card_leaves_a_step_with_two_ways_on_the_cpu_for_the_stream runs this"]
    fn lost_card_in_a_process_of_its_own() {
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let gpu = Arc::new(gpu);
        let (mut p, _) = either(Place::Card);
        p.gpu = Some(Some(gpu.clone()));
        p.process(grey(8, 8, 100), &NoContext);
        assert_eq!(p.last_transfers, (1, 1));
        // Two steps with a CPU half and a card-only one after them, none read back yet when the card goes
        struct Plus(u8);
        impl CpuStep for Plus {
            fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
                frame.data_mut().iter_mut().for_each(|v| *v = v.saturating_add(self.0));
            }
        }
        let (mut chain, _) = either(Place::Card);
        chain.gpu = Some(Some(gpu.clone()));
        let Kind::Either(_, invert) = chain.steps.remove(0).kind else { panic!("two ways") };
        let shader = || ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        let staged = |owner: &str, step| StagedStep { owner: owner.into(), stage: FrameStage::Effects, step };
        chain.set_steps(vec![
            staged("first", FrameStep::Either(shader(), invert)),
            staged("second", FrameStep::Either(shader(), Box::new(Plus(10)))),
            staged("card only", FrameStep::Shader(shader())),
        ]);
        gpu.lose();
        // The frame on the card when it went is the last that isn't changed, but the steps that could run on the CPU still do, in order
        assert_eq!(p.process(grey(8, 8, 100), &NoContext).data[0], 167);
        let out = chain.process(grey(8, 8, 100), &NoContext);
        assert_eq!(chain.last_transfers, (0, 0), "the destroyed card refused the picture going up");
        assert_eq!(out.data[0], 177, "invert, then plus 10, as the CPU makes it; the card-only step is lost with the card");
        for _ in 0..STEP_FAILS_BEFORE_SKIP + 5 {
            assert_eq!(p.process(grey(8, 8, 100), &NoContext).data[0], 167);
            assert_eq!(p.last_transfers, (0, 0));
        }
        assert!(!p.steps[0].skipped());
        // Changing the grade hands over a new step, which doesn't go back
        let step = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        let cpu = p.steps.remove(0);
        let Kind::Either(_, cpu) = cpu.kind else { panic!("two ways") };
        p.replace_steps("either", FrameStage::Effects, vec![FrameStep::Either(step, cpu)]);
        assert_eq!(p.process(grey(8, 8, 100), &NoContext).data[0], 167);
    }

    #[test]
    fn a_step_one_short_of_the_limit_that_recovers_starts_counting_again() {
        struct FailsUntil(u32, Arc<std::sync::Mutex<u32>>);
        impl CpuStep for FailsUntil {
            fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {
                let mut calls = self.1.lock().unwrap();
                *calls += 1;
                if *calls < self.0 {
                    drop(calls);
                    panic!("broken");
                }
            }
        }
        let calls = Arc::new(std::sync::Mutex::new(0));
        let mut p = Pipeline::new();
        p.set_steps(vec![step("flaky", FrameStage::Effects, FailsUntil(STEP_FAILS_BEFORE_SKIP, calls.clone()))]);
        quietly(|| (0..STEP_FAILS_BEFORE_SKIP).for_each(|_| { p.process(grey(4, 2, 1), &NoContext); }));
        assert!(!p.steps[0].skipped());
        assert_eq!(p.steps[0].fails, 0);
    }

    /// Paints what its CPU half's picture says (red 200 reads as luma 188), and the CPU way makes luma 99; counts what it was asked for.
    struct Pictures {
        picture: Arc<std::sync::Mutex<Frame>>,
        asked: Arc<AtomicUsize>,
        has_any: bool,
    }

    impl Pictures {
        fn new(has_any: bool) -> (Pictures, Arc<AtomicUsize>) {
            let asked = Arc::new(AtomicUsize::new(0));
            (Pictures { picture: Arc::new(std::sync::Mutex::new(Frame::new(1, 1, PixelFormat::Rgba, vec![200, 0, 0, 255]))), asked: asked.clone(), has_any }, asked)
        }
    }

    impl CpuStep for Pictures {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            frame.data_mut().iter_mut().take(64).for_each(|v| *v = 99);
        }
        fn card_textures(&mut self, _frame: &Frame, _ctx: &dyn FrameContext) -> Option<Vec<(String, Frame)>> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.has_any.then(|| vec![("soft".to_string(), self.picture.lock().unwrap().clone())])
        }
    }

    const FROM_PICTURE: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(vec3<f32>(sample_texture(soft, uv).r), 1.0); }";

    fn with_pictures(cpu: Pictures, place: Place, before: Vec<StagedStep>) -> Pipeline {
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(place)));
        let placeholder = Frame::new(1, 1, PixelFormat::Rgba, vec![0, 0, 0, 255]);
        let step = ShaderStep { keeps_geometry: false, wgsl: FROM_PICTURE.into(), uniforms: serde_json::json!({}), textures: vec![("soft".into(), placeholder)] };
        let mut steps = before;
        steps.push(StagedStep { owner: "either".into(), stage: FrameStage::Effects, step: FrameStep::Either(step, Box::new(cpu)) });
        p.set_steps(steps);
        p
    }

    #[test]
    fn the_cpu_half_hands_the_shader_pictures_that_go_up_only_when_they_change() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let (cpu, asked) = Pictures::new(true);
        let handle = cpu.picture.clone();
        let mut p = with_pictures(cpu, Place::Card, Vec::new());
        let before = transfers::totals();
        for _ in 0..3 {
            let out = p.process(grey(8, 8, 100), &NoContext);
            assert!((out.data[0] as i32 - 188).abs() <= 2, "{}", out.data[0]);
            assert_eq!(p.last_transfers, (1, 1), "asking for pictures on a frame that's on the CPU costs no crossing");
        }
        assert_eq!(asked.load(Ordering::SeqCst), 3);
        assert_eq!(transfers::totals().mask_up - before.mask_up, 1, "one picture, sent once");
        // a new picture goes up
        *handle.lock().unwrap() = Frame::new(1, 1, PixelFormat::Rgba, vec![100, 0, 0, 255]);
        let out = p.process(grey(8, 8, 100), &NoContext);
        assert!((out.data[0] as i32 - 102).abs() <= 2, "{}", out.data[0]);
        assert_eq!(transfers::totals().mask_up - before.mask_up, 2);
    }

    #[test]
    fn a_step_with_no_pictures_is_asked_once() {
        let (cpu, asked) = Pictures::new(false);
        let mut p = with_pictures(cpu, Place::Cpu, Vec::new());
        let out = p.process(grey(8, 8, 100), &NoContext);
        assert_eq!((out.data[0], asked.load(Ordering::SeqCst)), (99, 0), "on the CPU the shader's pictures aren't asked for");
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let (cpu, asked) = Pictures::new(false);
        let mut p = with_pictures(cpu, Place::Card, Vec::new());
        for _ in 0..3 {
            p.process(grey(8, 8, 100), &NoContext);
        }
        assert_eq!(asked.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_step_that_wants_pictures_after_a_shader_brings_the_frame_down_once_for_them() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let (cpu, _) = Pictures::new(true);
        let mut p = with_pictures(cpu, Place::Card, vec![shader(INVERT)]);
        p.process(grey(8, 8, 100), &NoContext);
        assert_eq!(p.last_transfers, (1, 2), "up once, down for the pictures and down at the end");
    }

    #[test]
    fn a_step_with_no_pictures_after_a_shader_doesnt_bring_the_frame_down_to_say_so() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let (cpu, asked) = Pictures::new(false);
        let mut p = with_pictures(cpu, Place::Card, vec![shader(INVERT)]);
        p.process(grey(8, 8, 100), &NoContext);
        assert_eq!((p.last_transfers, asked.load(Ordering::SeqCst)), ((1, 1), 1));
        p.process(grey(8, 8, 100), &NoContext);
        assert_eq!((p.last_transfers, asked.load(Ordering::SeqCst)), ((1, 1), 1), "not asked again");
    }

    #[test]
    fn the_cpu_half_that_asks_for_pictures_is_still_the_picture_when_the_balance_moves_it() {
        let (cpu, asked) = Pictures::new(true);
        let mut p = with_pictures(cpu, Place::Cpu, Vec::new());
        let out = p.process(grey(8, 8, 100), &NoContext);
        assert_eq!((out.data[0], p.last_transfers, asked.load(Ordering::SeqCst)), (99, (0, 0), 0));
    }

    /// Asks for a mask the way Background does for its picture, and notes the luma of the frame it was shown.
    struct AsksMask(Arc<std::sync::Mutex<Vec<u8>>>);

    impl CpuStep for AsksMask {
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
        fn card_textures_offered(&self) -> Option<bool> {
            Some(true)
        }
        fn card_textures(&mut self, frame: &Frame, ctx: &dyn FrameContext) -> Option<Vec<(String, Frame)>> {
            self.0.lock().unwrap().push(frame.data[0]);
            let _ = ctx.person_mask(frame);
            Some(Vec::new())
        }
    }

    #[test]
    fn a_step_that_always_has_pictures_asks_for_its_mask_once_on_the_picture_the_card_made() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        let step = ShaderStep { keeps_geometry: false, wgsl: DIM.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![shader(INVERT), StagedStep { owner: "either".into(), stage: FrameStage::Effects, step: FrameStep::Either(step, Box::new(AsksMask(seen.clone()))) }]);
        let ctx = Masked(std::sync::Mutex::new(None), std::sync::atomic::AtomicU32::new(0));
        p.process(grey(8, 8, 100), &ctx);
        assert_eq!(ctx.1.load(Ordering::SeqCst), 1, "one request for the mask");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!((seen[0] as i32 - 151).abs() <= 2, "the mask was asked for on the picture from before the card's work ({})", seen[0]);
    }

    /// Says it needs nothing of the frame but its person mask, which it asks for at a quarter of the frame's size.
    struct MaskOnly;

    impl CpuStep for MaskOnly {
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
        fn card_textures_offered(&self) -> Option<bool> {
            Some(true)
        }
        fn card_textures_from_mask(&self) -> bool {
            true
        }
        fn card_textures(&mut self, frame: &Frame, ctx: &dyn FrameContext) -> Option<Vec<(String, Frame)>> {
            let _ = ctx.background_mask_at(frame, frame.width as usize / 4, frame.height as usize / 4, false);
            Some(Vec::new())
        }
    }

    /// Notes the size, number and luma of each picture a mask is asked for on, and the size it is asked at.
    struct SeesPicture(std::sync::Mutex<Vec<((u32, u32), u64, u8, (usize, usize))>>);

    impl FrameContext for SeesPicture {
        fn source_id(&self) -> &str {
            ""
        }
        fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
            self.person_mask_at(frame, frame.width as usize, frame.height as usize)
        }
        fn person_mask_at(&self, frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
            self.0.lock().unwrap().push(((frame.width, frame.height), frame.number, frame.data[frame.data.len() / 3], (w, h)));
            None
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
    }

    #[test]
    fn a_step_that_only_needs_the_mask_gets_a_small_picture_of_the_cards_work_and_the_picture_comes_down_once() {
        assert_eq!([vision_size(3840, 2160), vision_size(1920, 1080), vision_size(1080, 1920), vision_size(1280, 720), vision_size(640, 360)], [Some((640, 360)), Some((640, 360)), Some((360, 640)), Some((640, 360)), None]);
        // A card the processor draws in software gains nothing from it, so there the picture comes down as before
        if Gpu::shared().is_none_or(|gpu| gpu.is_software()) {
            return eprintln!("no graphics card in hardware; skipped");
        }
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        let step = ShaderStep { keeps_geometry: false, wgsl: DIM.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![shader(INVERT), StagedStep { owner: "either".into(), stage: FrameStage::Effects, step: FrameStep::Either(step, Box::new(MaskOnly)) }]);
        let ctx = SeesPicture(Default::default());
        let mut frame = grey(1920, 1080, 100);
        frame.number = 7;
        p.process(frame, &ctx);
        let seen = ctx.0.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        let (size, number, luma, asked) = seen[0];
        assert_eq!((size, number, asked), ((640, 360), 7, (480, 270)), "the small picture, numbered as the frame, and the mask at the size the step asked");
        assert!((luma as i32 - 151).abs() <= 2, "the picture after the card's work ({luma})");
        assert_eq!(p.last_transfers, (1, 1), "up once, and down only at the end");
    }

    #[test]
    fn the_small_picture_has_the_size_the_cutout_model_takes_of_the_whole_frame() {
        // Another size (554 x 416 for the 555 x 416 the whole frame gets) would start the model's memory over each time a stream moved between the card and the processor
        assert_eq!([vision_size(1440, 1080), vision_size(1920, 1200), vision_size(2560, 1080), vision_size(1440, 1081)], [Some((555, 416)), Some((607, 379)), Some((740, 312)), Some((554, 416))]);
    }

    thread_local! {
        /// Lets the small picture be made on a software card for this test's thread, so the card's failures can be tried where there is no real one.
        pub(super) static SMALL_ON_SOFTWARE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// A step before Background that works on the card and, like the crop, has the same on the CPU, then one that only needs the person mask.
    fn crop_then_mask_only(gpu: Arc<Gpu>) -> Pipeline {
        struct Invert;
        impl CpuStep for Invert {
            fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
                frame.data_mut().iter_mut().for_each(|v| *v = 251 - *v + 16);
            }
        }
        SMALL_ON_SOFTWARE.with(|on| on.set(true));
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        p.gpu = Some(Some(gpu));
        let invert = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        let dim = ShaderStep { keeps_geometry: false, wgsl: DIM.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![
            StagedStep { owner: "crop".into(), stage: FrameStage::Effects, step: FrameStep::Either(invert, Box::new(Invert)) },
            StagedStep { owner: "either".into(), stage: FrameStage::Effects, step: FrameStep::Either(dim, Box::new(MaskOnly)) },
        ]);
        p
    }

    /// The pictures a mask was asked on after the crop worked, and the frame that went out, when the card fails the small readback this way.
    fn a_small_readback_that_fails(fault: crate::gpu::Fault) -> (Vec<u8>, Frame, u32, bool) {
        let gpu = Arc::new(Gpu::open().unwrap());
        let mut p = crop_then_mask_only(gpu.clone());
        gpu.inject(fault);
        let ctx = SeesPicture(Default::default());
        let out = p.process(grey(1920, 1080, 100), &ctx);
        let seen = ctx.0.lock().unwrap().iter().map(|seen| seen.2).collect();
        (seen, out, gpu.stalls(), p.card_lost)
    }

    #[test]
    fn a_refused_small_picture_never_lets_the_mask_or_the_camera_see_what_the_crop_never_made() {
        let Ok(_) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let (seen, out, _, lost) = a_small_readback_that_fails(crate::gpu::Fault::RefusedSubmit);
        // The CPU's picture of this frame (the crop's own steps, done there)
        assert_eq!(out.data[0], 167, "the camera got the CPU's cropped picture of this frame");
        assert!(out.data.iter().step_by(4099).all(|&v| v == 167), "the whole picture is the CPU's");
        assert!(seen.iter().all(|&luma| (luma as i32 - 167).abs() <= 2), "the mask was asked on a picture the crop didn't make: {seen:?}");
        assert!(lost, "the stream goes on the CPU after the card failed");
    }

    #[test]
    fn a_stalled_small_picture_costs_one_wait_on_the_card_and_the_frame_is_the_cpus() {
        let Ok(_) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        let (_, out, stalls, lost) = a_small_readback_that_fails(crate::gpu::Fault::Stalled);
        assert_eq!(stalls, 1, "the whole picture was waited for on a card that had just not answered");
        assert_eq!(out.data[0], 167);
        assert!(lost);
    }

    /// The card goes wrong on a readback (see Fault): the frame the CPU makes goes out, never what was left in the kept buffer, and the stream stays on the CPU unless the card is fine.
    fn a_readback_that_goes_wrong(fault: crate::gpu::Fault) {
        let gpu = Arc::new(Gpu::open().unwrap());
        let (mut p, _) = either(Place::Card);
        p.gpu = Some(Some(gpu.clone()));
        // a picture first, so a download buffer of this size is kept with it in
        p.process(grey(8, 8, 100), &NoContext);
        assert_eq!(p.last_transfers, (1, 1));
        gpu.inject(fault);
        let out = p.process(grey(8, 8, 60), &NoContext);
        assert_eq!(out.data[0], 207, "{fault:?}: the CPU's picture of this frame, not the last one's (167)");
        assert!(p.card_lost, "{fault:?}");
        for luma in [30, 90] {
            assert_eq!(p.process(grey(8, 8, luma), &NoContext).data[0], (267 - luma as u32) as u8, "{fault:?}");
            assert_eq!(p.last_transfers, (0, 0), "{fault:?}");
        }
        let (mut other, _) = either(Place::Card);
        other.gpu = Some(Some(gpu.clone()));
        other.process(grey(8, 8, 80), &NoContext);
        let usable = matches!(fault, crate::gpu::Fault::RefusedSubmit);
        assert_eq!(other.last_transfers == (1, 1), usable, "{fault:?}: a refusal leaves the card usable, a panic or a loss doesn't");
    }

    #[test]
    fn a_refused_readback_a_poll_panic_and_a_lost_card_all_end_on_the_cpu() {
        let name = "pipeline::tests::card_faults_in_a_process_of_its_own";
        let run = std::process::Command::new(std::env::current_exe().unwrap()).args(["--ignored", "--exact", name, "--nocapture"]).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stdout));
    }

    #[test]
    #[ignore = "a_refused_readback_a_poll_panic_and_a_lost_card_all_end_on_the_cpu runs this"]
    fn card_faults_in_a_process_of_its_own() {
        let Ok(gpu) = Gpu::open() else { return eprintln!("no graphics card; skipped") };
        // the same card, asked for a picture twice with a refusal between: the second isn't the first
        let frame = |luma| grey(8, 8, luma);
        let first = gpu.rgba(&frame(100), 8, 8).unwrap();
        gpu.inject(crate::gpu::Fault::RefusedSubmit);
        let why = gpu.rgba(&frame(40), 8, 8).unwrap_err();
        assert!(why.contains("refused"), "{why}");
        let second = gpu.rgba(&frame(40), 8, 8).unwrap();
        assert_ne!(first, second);
        drop(gpu);
        for fault in [crate::gpu::Fault::RefusedSubmit, crate::gpu::Fault::PollPanic, crate::gpu::Fault::Lost] {
            a_readback_that_goes_wrong(fault);
        }
        a_failed_readback_for_a_steps_pictures_sends_that_step_to_the_cpu_too();
        a_stop_ends_the_wait_and_the_frame_goes_out_dark();
        a_hiding_step_on_the_card_never_lets_the_clear_picture_out_when_the_card_fails();
        a_card_only_step_at_a_hiding_stage_never_lets_the_clear_picture_out_when_the_card_fails();
        a_card_only_step_at_privacy_never_lets_a_sent_again_frame_out_clear_when_the_card_fails();
        a_failed_upload_never_lets_a_card_only_hiding_step_draw_on_what_the_card_had();
        a_picture_the_card_refuses_for_a_step_runs_that_step_on_the_cpu_instead();
        a_failed_readback_never_lets_a_step_after_a_blackout_blend_the_clear_picture_back();
    }

    /// A step's pictures that won't go up: it runs on the CPU, it isn't drawn on the pictures the card still had.
    fn a_picture_the_card_refuses_for_a_step_runs_that_step_on_the_cpu_instead() {
        use crate::gpu::Fault;
        let gpu = Arc::new(Gpu::open().unwrap());
        let (cpu, _asked) = Pictures::new(true);
        let handle = cpu.picture.clone();
        let mut p = with_pictures(cpu, Place::Card, Vec::new());
        p.gpu = Some(Some(gpu.clone()));
        let out = p.process(grey(8, 8, 100), &NoContext);
        assert!((out.data[0] as i32 - 188).abs() <= 2, "the shader drew from the picture: {}", out.data[0]);
        *handle.lock().unwrap() = Frame::new(1, 1, PixelFormat::Rgba, vec![50, 0, 0, 255]);
        gpu.inject(Fault::BadUpload);
        let out = quietly(|| p.process(grey(8, 8, 100), &NoContext));
        assert_eq!(out.data[0], 99, "the step ran on the CPU and the shader drew nothing from the old picture");
    }

    /// A card-only Background step is lost with a failed readback that a CPU step after it asked for: the denoiser's old clear picture must not be blended in.
    fn a_failed_readback_never_lets_a_step_after_a_blackout_blend_the_clear_picture_back() {
        use crate::gpu::Fault;
        for fault in [Fault::RefusedSubmit, Fault::PollPanic, Fault::Lost] {
            let gpu = Arc::new(Gpu::open().unwrap());
            let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
            p.gpu = Some(Some(gpu.clone()));
            let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
            p.set_steps(vec![StagedStep { owner: "key".into(), stage: FrameStage::Background, step: FrameStep::Shader(key) }, step("denoise", FrameStage::Effects, Restores(grey(8, 8, 200)))]);
            p.process(grey(8, 8, 100), &NoContext);
            gpu.inject(fault);
            let out = quietly(|| p.process(grey(8, 8, 60), &NoContext));
            assert!(is_dark(&out), "{fault:?}: the step after the failed readback put the clear picture back: {:?}", &out.data[..4]);
        }
    }

    /// A picture or a mask that won't go up: the frame goes out dark, not drawn from the card's last picture or the last mask.
    fn a_failed_upload_never_lets_a_card_only_hiding_step_draw_on_what_the_card_had() {
        use crate::gpu::Fault;
        let gpu = Arc::new(Gpu::open().unwrap());
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        p.gpu = Some(Some(gpu.clone()));
        let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![StagedStep { owner: "key".into(), stage: FrameStage::Privacy, step: FrameStep::Shader(key) }]);
        p.process(grey(8, 8, 100), &NoContext);
        gpu.inject(Fault::BadUpload);
        let out = quietly(|| p.process(grey(8, 8, 60), &NoContext));
        assert!(is_dark(&out), "the picture didn't go up and the frame came out {:?}", &out.data[..4]);

        let gpu = Arc::new(Gpu::open().unwrap());
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        p.gpu = Some(Some(gpu.clone()));
        let key = ShaderStep { keeps_geometry: false, wgsl: SHOW_MASK.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![StagedStep { owner: "mask".into(), stage: FrameStage::Privacy, step: FrameStep::Shader(key) }]);
        let ctx = Masked(std::sync::Mutex::new(Some(half_mask(255, 0))), Default::default());
        let out = p.process(grey(16, 8, 16), &ctx);
        assert!(!is_dark(&out), "the mask drew");
        *ctx.0.lock().unwrap() = Some(half_mask(0, 255));
        gpu.inject(Fault::BadUpload);
        let out = quietly(|| p.process(grey(16, 8, 16), &ctx));
        assert!(is_dark(&out), "the new mask didn't go up and the old one was drawn: {} {}", out.data[0], out.data[15]);
    }

    /// HalvesAndCounts its runs and does what DIM does.
    struct HalvesAndCounts(Arc<std::sync::atomic::AtomicU32>);

    impl CpuStep for HalvesAndCounts {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            self.0.fetch_add(1, Ordering::SeqCst);
            frame.data_mut().iter_mut().for_each(|v| *v /= 2);
        }
        fn card_textures_offered(&self) -> Option<bool> {
            Some(true)
        }
        fn card_textures(&mut self, _frame: &Frame, _ctx: &dyn FrameContext) -> Option<Vec<(String, Frame)>> {
            Some(Vec::new())
        }
    }

    /// The readback that brings the picture down for a step's pictures fails: that step runs on the CPU in the same frame, so the card is waited on once.
    fn a_failed_readback_for_a_steps_pictures_sends_that_step_to_the_cpu_too() {
        let gpu = Arc::new(Gpu::open().unwrap());
        let runs = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        p.gpu = Some(Some(gpu.clone()));
        let dim = ShaderStep { keeps_geometry: false, wgsl: DIM.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![shader(INVERT), StagedStep { owner: "either".into(), stage: FrameStage::Effects, step: FrameStep::Either(dim, Box::new(HalvesAndCounts(runs.clone()))) }]);
        // a picture first, so the shaders are built and a download buffer of this size is kept
        p.process(grey(8, 8, 100), &NoContext);
        runs.store(0, Ordering::SeqCst);
        gpu.inject(crate::gpu::Fault::RefusedSubmit);
        let out = p.process(grey(8, 8, 100), &NoContext);
        assert!(p.card_lost);
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the step with a CPU half ran on the CPU this frame");
        assert_eq!(p.last_transfers, (1, 1), "one trip to the card and one wait on it, not a second");
        assert_eq!(out.data[0], 50, "the card's invert went with the failed readback, and the CPU halved the frame in hand");
    }

    /// Hides by painting the frame black, so a clear picture shows in the luma.
    struct HidesDark;

    impl CpuStep for HidesDark {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            frame.data_mut().iter_mut().for_each(|v| *v = 0);
        }
        fn hides(&self) -> bool {
            true
        }
    }

    /// A hiding step run on the card, with a card-only shader before it, while the card fails: nothing clear goes out.
    fn a_hiding_step_on_the_card_never_lets_the_clear_picture_out_when_the_card_fails() {
        for fault in [crate::gpu::Fault::RefusedSubmit, crate::gpu::Fault::PollPanic, crate::gpu::Fault::Lost] {
            let gpu = Arc::new(Gpu::open().unwrap());
            let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
            p.gpu = Some(Some(gpu.clone()));
            let hide = ShaderStep { keeps_geometry: false, wgsl: DIM.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
            p.set_steps(vec![shader(INVERT), StagedStep { owner: "hide".into(), stage: FrameStage::Privacy, step: FrameStep::Either(hide, Box::new(HidesDark)) }]);
            p.process(grey(8, 8, 100), &NoContext);
            gpu.inject(fault);
            for luma in [60u8, 90, 120] {
                let out = p.process(grey(8, 8, luma), &NoContext);
                assert!(out.data[..64].iter().all(|&v| v == 0), "{fault:?}: the clear picture (luma {luma}) went out: {:?}", &out.data[..4]);
            }
        }
    }

    /// A card-only shader at a stage that hides, while the card fails in each way there is: every frame goes out dark, never the clear one.
    fn a_card_only_step_at_a_hiding_stage_never_lets_the_clear_picture_out_when_the_card_fails() {
        use crate::gpu::Fault;
        for fault in [Fault::RefusedSubmit, Fault::PollPanic, Fault::Lost, Fault::LiftPanic, Fault::ShadePanic, Fault::PreparePanic] {
            let gpu = Arc::new(Gpu::open().unwrap());
            let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
            p.gpu = Some(Some(gpu.clone()));
            let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
            p.set_steps(vec![StagedStep { owner: "key".into(), stage: FrameStage::Background, step: FrameStep::Shader(key) }]);
            p.process(grey(8, 8, 100), &NoContext);
            gpu.inject(fault);
            for luma in [60u8, 90, 120] {
                let out = quietly(|| p.process(grey(8, 8, luma), &NoContext));
                assert!(is_dark(&out), "{fault:?}: the clear picture (luma {luma}) went out: {:?}", &out.data[..4]);
            }
            assert!(gpu.is_lost() || matches!(fault, Fault::RefusedSubmit), "{fault:?}: a panic in the card's calls marks it lost");
        }
    }

    /// A card-only shader at Privacy or Cover, a feed that stalls so the last frame is sent again, while the card fails in each way: the frame is dark, never the clear one.
    fn a_card_only_step_at_privacy_never_lets_a_sent_again_frame_out_clear_when_the_card_fails() {
        use crate::gpu::Fault;
        for stage in [FrameStage::Privacy, FrameStage::Cover] {
            for fault in [Fault::RefusedSubmit, Fault::PollPanic, Fault::Lost, Fault::LiftPanic, Fault::ShadePanic] {
                let gpu = Arc::new(Gpu::open().unwrap());
                let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
                p.gpu = Some(Some(gpu.clone()));
                let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
                p.set_steps(vec![StagedStep { owner: "key".into(), stage, step: FrameStep::Shader(key) }]);
                p.process(grey(8, 8, 100), &NoContext);
                // Another shader than the one the last frame went through, so it is still to be done
                let dim = ShaderStep { keeps_geometry: false, wgsl: DIM.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
                p.set_steps(vec![StagedStep { owner: "other".into(), stage, step: FrameStep::Shader(dim) }]);
                gpu.inject(fault);
                let out = quietly(|| p.hide_again(grey(8, 8, 90), &NoContext)).expect("a card-only hiding step always has something to hide");
                assert!(is_dark(&out), "{stage:?} {fault:?}: the clear picture went out again: {:?}", &out.data[..4]);
            }
        }
    }

    #[test]
    fn a_card_only_step_at_a_hiding_stage_hides_a_frame_sent_again_when_there_is_no_card() {
        for stage in [FrameStage::Privacy, FrameStage::Cover] {
            let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
            p.gpu = Some(None);
            let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
            p.set_steps(vec![StagedStep { owner: "card".into(), stage, step: FrameStep::Shader(key) }]);
            for _ in 0..=STEP_FAILS_BEFORE_SKIP {
                let out = quietly(|| p.hide_again(grey(8, 8, 100), &NoContext)).expect("something to hide");
                assert!(is_dark(&out), "{stage:?}: the clear picture went out again");
            }
        }
    }

    #[test]
    fn a_card_only_step_at_privacy_runs_on_a_frame_sent_again() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![StagedStep { owner: "key".into(), stage: FrameStage::Privacy, step: FrameStep::Shader(key) }]);
        let went_out = grey(8, 8, 100);
        let again = p.hide_again(went_out.clone(), &NoContext).expect("a card-only hiding step always has something to hide");
        assert_ne!(again.data[0], 100, "the step ran on the frame sent again");
        assert_eq!(went_out.data[0], 100);
    }

    #[test]
    fn a_card_only_step_at_any_hiding_stage_goes_dark_when_there_is_no_card() {
        let hiding: Vec<FrameStage> = FrameStage::ALL.into_iter().filter(|s| s.hides()).collect();
        // A new hiding stage has to be looked at here, and in hide_again, before it is added
        assert_eq!(hiding, [FrameStage::Background, FrameStage::Privacy, FrameStage::Cover]);
        for stage in FrameStage::ALL {
            let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
            p.gpu = Some(None);
            let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
            p.set_steps(vec![StagedStep { owner: "card".into(), stage, step: FrameStep::Shader(key) }]);
            // Past the point where it is skipped for failing too often, which has to stay dark too
            for _ in 0..=STEP_FAILS_BEFORE_SKIP {
                let out = p.process(grey(8, 8, 100), &NoContext);
                assert_eq!(is_dark(&out), stage.hides(), "{stage:?}: a card-only step that fails hides the picture only where it hides things");
            }
        }
    }

    /// Writes the clear picture it remembers over whatever it is given, as a step that blends from its history does.
    struct Restores(Frame);

    impl CpuStep for Restores {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            *frame = self.0.clone();
        }
    }

    #[test]
    fn no_step_after_a_blackout_brings_the_camera_back() {
        let clear = grey(4, 2, 200);
        for stage in [FrameStage::Effects, FrameStage::Colour] {
            let mut p = Pipeline::new();
            p.set_steps(vec![step("blur", FrameStage::Background, HidingPanics), step("denoise", stage, Restores(clear.clone()))]);
            let out = quietly(|| p.process(grey(4, 2, 60), &NoContext));
            assert!(is_dark(&out), "{stage:?}: the step after the blackout put the clear picture back: {:?}", out.data[0]);
        }
        // A card-only Background step with no card is the same, and what comes after that only draws on top still runs
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        p.gpu = Some(None);
        let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![StagedStep { owner: "key".into(), stage: FrameStage::Background, step: FrameStep::Shader(key) }, step("denoise", FrameStage::Effects, Restores(clear)), step("tag", FrameStage::Overlay, Push(4, seen.clone()))]);
        let out = quietly(|| p.process(grey(4, 2, 60), &NoContext));
        assert_eq!(out.data[0], 4, "the tag is drawn on the dark frame, not on the restored one");
        assert!(out.data[1..8].iter().all(|v| *v == 16), "{:?}", &out.data[..8]);
        assert_eq!(*seen.lock().unwrap(), vec![4]);
    }

    #[test]
    fn a_hiding_step_added_beside_one_already_there_hides_the_frame_sent_again() {
        let mut p = Pipeline::new();
        p.set_steps(vec![step("blur", FrameStage::Background, Hides)]);
        p.process(grey(4, 2, 200), &NoContext);
        p.set_steps(vec![step("blur", FrameStage::Background, Hides)]);
        assert!(p.hide_again(grey(4, 2, 200), &NoContext).is_none(), "the blur was on that picture");
        p.set_steps(vec![step("blur", FrameStage::Background, Hides), step("redact", FrameStage::Background, Hides)]);
        let out = p.hide_again(grey(4, 2, 200), &NoContext).expect("the redaction wasn't on the picture that went out");
        assert!(is_dark(&out));
        // The dark frame is what's out now: the same steps again have nothing new to hide
        p.set_steps(vec![step("blur", FrameStage::Background, Hides), step("redact", FrameStage::Background, Hides)]);
        assert!(p.hide_again(grey(4, 2, 200), &NoContext).is_none());
        // A card-only redaction with no card to run on is the same
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        p.gpu = Some(None);
        p.set_steps(vec![step("blur", FrameStage::Background, Hides)]);
        p.process(grey(4, 2, 200), &NoContext);
        let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![step("blur", FrameStage::Background, Hides), StagedStep { owner: "redact".into(), stage: FrameStage::Background, step: FrameStep::Shader(key) }]);
        assert!(is_dark(&quietly(|| p.hide_again(grey(4, 2, 200), &NoContext)).expect("hidden")));
    }

    #[test]
    fn a_settings_refresh_does_not_run_a_card_only_hiding_step_again_on_its_own_output() {
        for stage in [FrameStage::Privacy, FrameStage::Cover] {
            let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
            p.gpu = Some(None);
            let staged = |wgsl: &str| {
                let step = ShaderStep { keeps_geometry: false, wgsl: wgsl.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
                StagedStep { owner: "cover".into(), stage, step: FrameStep::Shader(step) }
            };
            p.set_steps(vec![staged(INVERT)]);
            quietly(|| p.process(grey(8, 8, 100), &NoContext));
            // Every plugin's steps are handed over again when one of them changes
            p.set_steps(vec![staged(INVERT)]);
            assert!(quietly(|| p.hide_again(grey(8, 8, 100), &NoContext)).is_none(), "{stage:?}: it was on the picture that went out");
            // Another shader is a change, and the card can't run it
            p.set_steps(vec![staged(DIM)]);
            assert!(is_dark(&quietly(|| p.hide_again(grey(8, 8, 100), &NoContext)).expect("changed")), "{stage:?}");
        }
    }

    #[test]
    fn an_unchanged_shader_on_a_healthy_card_leaves_the_frame_sent_again_as_it_was() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        let staged = || StagedStep { owner: "cover".into(), stage: FrameStage::Cover, step: FrameStep::Shader(ShaderStep { keeps_geometry: false, wgsl: DIM.into(), uniforms: serde_json::json!({}), textures: Vec::new() }) };
        p.set_steps(vec![staged()]);
        let out = p.process(grey(8, 8, 200), &NoContext);
        assert!(out.data[0] < 150, "dimmed: {}", out.data[0]);
        p.set_steps(vec![staged()]);
        assert!(p.hide_again(out, &NoContext).is_none(), "dimmed once already, not again");
    }

    /// Says it can't wait for the card.
    struct Hurried;

    impl FrameContext for Hurried {
        fn source_id(&self) -> &str {
            ""
        }
        fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
            None
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
        fn hurried(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_hurried_frame_sent_again_goes_dark_for_a_new_card_step_without_the_card() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![StagedStep { owner: "key".into(), stage: FrameStage::Privacy, step: FrameStep::Shader(key) }]);
        let out = p.hide_again(grey(8, 8, 100), &Hurried).expect("a new card step has something to hide");
        assert!(is_dark(&out));
        let later = p.hide_again(grey(8, 8, 100), &NoContext).expect("the real run is still due");
        assert!(!is_dark(&later), "the same step run for real draws its picture");
    }

    /// The window's hand-over on a stalled feed, as the worker makes it: the new steps in, then the hurried hide on the window's thread.
    fn handed_over_on_a_stall(p: &mut Pipeline, steps: impl Fn() -> Vec<StagedStep>, out: &Frame) -> Option<Frame> {
        p.set_steps(steps());
        p.hide_again(out.clone(), &Hurried)
    }

    #[test]
    fn a_replay_that_was_discarded_leaves_no_hiding_step_marked_as_done_on_the_clear_frame() {
        let mut cases: Vec<(&str, Box<dyn Fn() -> Vec<StagedStep>>, Pipeline)> = vec![("background", Box::new(|| vec![step("key", FrameStage::Background, Hides)]), Pipeline::new())];
        if let Some(gpu) = Gpu::shared() {
            let card = Box::new(|| {
                let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
                vec![StagedStep { owner: "key".into(), stage: FrameStage::Privacy, step: FrameStep::Shader(key) }]
            });
            let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
            p.gpu = Some(Some(gpu));
            cases.push(("card shader", card, p));
        }
        for (name, steps, mut p) in cases {
            // The picture the stalled feed keeps sending, made before any hiding step came on
            let clear = grey(8, 8, 200);
            p.process(clear.clone(), &NoContext);
            // Hand-over 1: dark at once, and the replay's real run notes the steps as done, but a second hand-over discards what it made
            assert!(is_dark(&handed_over_on_a_stall(&mut p, &steps, &clear).expect("something to hide")), "{name}");
            p.hide_again(clear.clone(), &NoContext).expect("the replay has something to hide");
            // What the worker does with a replay it discards
            FramePipeline::forget_history(&mut p);
            // Hand-over 2 carries the same steps again: no picture out has them, so None here would put the clear frame out
            let again = handed_over_on_a_stall(&mut p, &steps, &clear);
            assert!(again.as_ref().is_some_and(is_dark), "{name}: the same steps again took the discarded replay for done and left the clear frame to go out");
        }
    }

    #[test]
    fn a_replay_that_was_published_leaves_the_same_steps_again_nothing_to_hide() {
        let mut p = Pipeline::new();
        let steps = || vec![step("key", FrameStage::Background, Hides)];
        let clear = grey(8, 8, 200);
        p.process(clear.clone(), &NoContext);
        assert!(handed_over_on_a_stall(&mut p, steps, &clear).is_some());
        let hidden = p.hide_again(clear.clone(), &NoContext).expect("the replay has something to hide");
        assert!(is_dark(&hidden));
        assert!(handed_over_on_a_stall(&mut p, steps, &hidden).is_none(), "the picture out already has them");
    }

    #[test]
    fn a_skipped_cover_step_leaves_no_privacy_work_on_the_card_to_overwrite_its_dark_frame() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        let privacy = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![StagedStep { owner: "p".into(), stage: FrameStage::Privacy, step: FrameStep::Shader(privacy) }, step("cover", FrameStage::Cover, HidingPanics)]);
        for _ in 0..=STEP_FAILS_BEFORE_SKIP {
            quietly(|| p.process(grey(8, 8, 100), &NoContext));
        }
        assert!(p.steps[1].skipped());
        let out = p.hide_again(grey(8, 8, 90), &NoContext).expect("dark");
        assert!(is_dark(&out), "the privacy step's picture came down over the dark frame: {:?}", out.data[0]);
    }

    /// A context that says stop from its second question on.
    struct Stops(std::sync::atomic::AtomicU32);

    impl FrameContext for Stops {
        fn source_id(&self) -> &str {
            ""
        }
        fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
            None
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
        fn stopped(&self) -> bool {
            self.0.fetch_add(1, Ordering::SeqCst) > 0
        }
    }

    /// A stop while the frame waits on the card ends the wait at once: the card isn't blamed and nothing but a dark frame is left.
    fn a_stop_ends_the_wait_and_the_frame_goes_out_dark() {
        let gpu = Arc::new(Gpu::open().unwrap());
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        p.gpu = Some(Some(gpu));
        p.set_steps(vec![shader(INVERT)]);
        let began = std::time::Instant::now();
        let out = p.process(grey(8, 8, 100), &Stops(Default::default()));
        assert!(began.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(out.data[0], 16, "dark, not the frame without what the card was doing for it");
        assert!(!p.card_lost, "a stop isn't the card failing");
    }

    #[test]
    fn a_comfort_background_refresh_keeps_the_current_picture_showing() {
        struct Comfort;
        impl CpuStep for Comfort {
            fn process(&mut self, _: &mut Frame, _: &dyn FrameContext) {}
        }
        let mut pipeline = Pipeline::default();
        pipeline.gpu = Some(None);
        let step = || StagedStep { owner: "background".into(), stage: FrameStage::Background, step: FrameStep::Cpu(Box::new(Comfort)) };
        pipeline.set_steps(vec![step()]);
        let current = pipeline.process(grey(8, 8, 100), &NoContext);
        pipeline.set_steps(vec![step()]);
        assert!(pipeline.hide_again(current.clone(), &NoContext).is_none(), "comfort settings do not blank a stalled picture");
        assert_eq!(pipeline.process(grey(8, 8, 150), &NoContext).data[0], 150, "the current frame keeps moving");
    }

    #[test]
    fn source_geometry_survives_a_step_that_rebuilds_the_frame() {
        struct Rebuild;
        impl CpuStep for Rebuild {
            fn process(&mut self, frame: &mut Frame, _: &dyn FrameContext) {
                *frame = Frame::new(frame.width, frame.height, frame.format, frame.data.to_vec());
            }
        }
        let mut pipeline = Pipeline::default();
        pipeline.gpu = Some(None);
        pipeline.set_steps(vec![StagedStep { owner: "filter".into(), stage: FrameStage::Effects, step: FrameStep::Cpu(Box::new(Rebuild)) }]);
        let mut frame = grey(8, 8, 100);
        frame.geometry = Some(Box::new(telescope_core::view::Geometry { zoom: 2.0, pan_x: 0.5, ..Default::default() }));
        frame.mask_space = telescope_core::frame::MaskSpace { source_size: (1920, 1080), source_epoch: 2, warp: Some(4), crop_unsure: false };
        let out = pipeline.process(frame.clone(), &NoContext);
        assert_eq!(out.geometry, frame.geometry);
        assert_eq!(out.mask_space, frame.mask_space);
    }

    #[test]
    fn a_geometry_warp_invalidates_camera_remapping_when_the_view_or_warp_changes() {
        struct Warp(u64);
        impl CpuStep for Warp {
            fn process(&mut self, _: &mut Frame, _: &dyn FrameContext) {}
            fn geometry_key(&self) -> u64 { self.0 }
        }
        let mut pipeline = Pipeline::default();
        pipeline.gpu = Some(None);
        let step = |key| StagedStep { owner: "desk".into(), stage: FrameStage::Geometry, step: FrameStep::Cpu(Box::new(Warp(key))) };
        pipeline.set_steps(vec![step(1)]);
        let frame = grey(16, 12, 100);
        let first = pipeline.process(frame.clone(), &NoContext).mask_space;
        assert_eq!(first.source_size, (16, 12));
        assert!(first.warp.is_some());
        assert_eq!(pipeline.process(frame.clone(), &NoContext).mask_space, first);
        let mut panned = frame.clone();
        panned.geometry = Some(Box::new(telescope_core::view::Geometry { zoom: 2.0, pan_x: 0.5, ..Default::default() }));
        assert_ne!(pipeline.process(panned, &NoContext).mask_space, first);
        pipeline.set_steps(vec![step(2)]);
        assert_ne!(pipeline.process(frame.clone(), &NoContext).mask_space, first);
        pipeline.set_steps(Vec::new());
        assert_ne!(pipeline.process(frame, &NoContext).mask_space, first);
    }

    #[test]
    fn a_background_step_moved_while_the_feed_stalls_hides_the_frame_sent_again() {
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        p.gpu = Some(None);
        let staged = |uniforms: serde_json::Value| {
            let step = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms, textures: Vec::new() };
            StagedStep { owner: "redact".into(), stage: FrameStage::Background, step: FrameStep::Shader(step) }
        };
        p.set_steps(vec![staged(serde_json::json!({"x": 1}))]);
        quietly(|| p.process(grey(8, 8, 100), &NoContext));
        // A refresh with the same settings has nothing new to hide
        p.set_steps(vec![staged(serde_json::json!({"x": 1}))]);
        assert!(quietly(|| p.hide_again(grey(8, 8, 100), &NoContext)).is_none());
        // The rectangle moved: what went out still shows the new place
        p.set_steps(vec![staged(serde_json::json!({"x": 2}))]);
        assert!(is_dark(&quietly(|| p.hide_again(grey(8, 8, 100), &NoContext)).expect("the picture that went out has the old settings")));
    }

    #[test]
    fn a_card_only_hiding_step_with_new_settings_is_not_run_on_the_picture_that_has_the_old_ones() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        let staged = |uniforms: serde_json::Value| {
            let step = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms, textures: Vec::new() };
            StagedStep { owner: "cover".into(), stage: FrameStage::Privacy, step: FrameStep::Shader(step) }
        };
        p.set_steps(vec![staged(serde_json::json!({"x": 1}))]);
        let out = p.process(grey(8, 8, 100), &NoContext);
        assert_ne!(out.data[0], 100, "inverted");
        // Run again on its own output it would undo itself and give back the clear picture
        p.set_steps(vec![staged(serde_json::json!({"x": 2}))]);
        let again = p.hide_again(out, &NoContext).expect("changed settings are to be done");
        assert!(is_dark(&again), "{:?}", &again.data[..4]);
    }

    /// Hides, and panics while the cue is set.
    struct HidesUntilCued(Arc<std::sync::atomic::AtomicBool>);

    impl CpuStep for HidesUntilCued {
        fn hides(&self) -> bool {
            true
        }
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {
            if self.0.load(Ordering::SeqCst) {
                panic!("cued")
            }
        }
    }

    #[test]
    fn a_hiding_shader_that_keeps_its_last_frame_starts_again_on_a_frame_that_went_dark() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let cue = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        let cover = ShaderStep { keeps_geometry: false, wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { if has_previous() { return vec4<f32>(previous(uv).r + 0.2, 0.0, 0.0, 1.0); } return vec4<f32>(0.2, 0.0, 0.0, 1.0); }".into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![step("blur", FrameStage::Background, HidesUntilCued(cue.clone())), StagedStep { owner: "cover".into(), stage: FrameStage::Cover, step: FrameStep::Shader(cover) }]);
        // Frames a frame apart on their own clock: a history is dropped when its frame is too old, and a busy machine must not decide that
        let frame = |n: u64| {
            let mut f = grey(8, 8, 100);
            f.time_ns = 1_000_000_000 + n * 33_000_000;
            f
        };
        let first = p.process(frame(0), &NoContext).data[0];
        let second = p.process(frame(1), &NoContext).data[0];
        assert_ne!(first, second, "the step goes on from its last frame");
        cue.store(true, Ordering::SeqCst);
        let dark = quietly(|| p.process(frame(2), &NoContext)).data[0];
        assert_eq!(dark, first, "a frame that went dark isn't drawn from what the step drew for the clear one");
    }

    #[test]
    fn a_card_only_background_step_turned_on_while_the_feed_stalls_hides_the_frame_sent_again() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        // The chroma key is one: a shader of its own at the Background stage
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        p.set_steps(Vec::new());
        let went_out = p.process(grey(8, 8, 200), &NoContext);
        let key = ShaderStep { keeps_geometry: false, wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        p.set_steps(vec![StagedStep { owner: "chroma_key".into(), stage: FrameStage::Background, step: FrameStep::Shader(key) }]);
        for context in [&Hurried as &dyn FrameContext, &NoContext] {
            let out = p.hide_again(went_out.clone(), context).expect("the room is in the picture that went out");
            assert!(is_dark(&out));
        }
    }

    /// Cuts the picture out: gives the frame an alpha plane, as the steps that cut a person out do.
    struct Cuts;

    impl CpuStep for Cuts {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            let plane = Alpha::new(frame.width, frame.height, vec![128; frame.width as usize * frame.height as usize], false);
            assert!(frame.set_alpha(Arc::new(plane)));
        }
    }

    /// Brightens the pixels where they are, by writing a whole new buffer.
    struct Brightens;

    impl CpuStep for Brightens {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            let data = frame.data.iter().map(|v| v.saturating_add(1)).collect();
            frame.replace_data(data);
        }
        fn keeps_geometry(&self) -> bool {
            true
        }
    }

    /// Looks at the pixels and says so, leaving them as they are (like Stargazer's Handoff), whatever its stage.
    struct Reads;

    impl CpuStep for Reads {
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
        fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
            Touch::Reads
        }
    }

    /// Halves the picture's size, which a cut-out of the old size no longer fits.
    struct Halves;

    impl CpuStep for Halves {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            let (w, h) = (frame.width / 2, frame.height / 2);
            (frame.width, frame.height) = (w, h);
            frame.replace_data(vec![90; PixelFormat::I420.size(w, h)]);
        }
        fn keeps_geometry(&self) -> bool {
            true
        }
    }

    /// Counts its runs and brightens; says whether the frame can go without it.
    struct Tints(Arc<std::sync::Mutex<u32>>, bool);

    impl CpuStep for Tints {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            *self.0.lock().unwrap() += 1;
            frame.data_mut()[1] = 77;
        }
        fn pixels_only(&self) -> bool {
            self.1
        }
    }

    /// Puts its own picture over the whole frame, like the wait screen.
    struct Covers;

    impl CpuStep for Covers {
        fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
            frame.data_mut().fill(5);
        }
        fn hides(&self) -> bool {
            true
        }
        fn covers(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_covered_frame_goes_without_the_pixel_only_steps_before_the_cover_and_keeps_the_rest() {
        let (pure, other, after_cover) = (Arc::new(std::sync::Mutex::new(0)), Arc::new(std::sync::Mutex::new(0)), Arc::new(std::sync::Mutex::new(0)));
        let steps = |cover: bool| {
            let mut v = vec![step("filters", FrameStage::Effects, Tints(pure.clone(), true)), step("reader", FrameStage::Effects, Tints(other.clone(), false)), step("grade", FrameStage::Colour, Tints(pure.clone(), true))];
            if cover {
                v.push(step("wait", FrameStage::Cover, Covers));
            }
            v.push(step("tap", FrameStage::Final, Tints(after_cover.clone(), true)));
            v
        };
        let out = after(steps(true));
        assert!(out.data.iter().all(|&b| b == 5 || b == 77) && out.data[0] == 5, "the cover is what goes out");
        assert_eq!((*pure.lock().unwrap(), *other.lock().unwrap(), *after_cover.lock().unwrap()), (0, 1, 1), "a pixel-only step before the cover is skipped, one that isn't and one after it run");
        after(steps(false));
        assert_eq!(*pure.lock().unwrap(), 2, "without a cover they run as ever");
    }

    fn after(steps: Vec<StagedStep>) -> Frame {
        let mut p = Pipeline::new();
        p.set_steps(steps);
        p.process(grey(8, 8, 100), &NoContext)
    }

    #[test]
    fn a_cut_out_goes_through_colour_and_privacy_steps_but_not_a_step_that_moves_pixels() {
        let cut = || step("background", FrameStage::Background, Cuts);
        let out = after(vec![cut(), step("grade", FrameStage::Colour, Brightens), step("blur", FrameStage::Privacy, Brightens), step("tag", FrameStage::Overlay, Push(7, Arc::default()))]);
        assert_eq!(out.data[1], 102, "the colour and privacy steps ran");
        assert_eq!(out.alpha().map(|a| a.data[0]), Some(128));
        let out = after(vec![cut(), step("filters", FrameStage::Effects, Brightens)]);
        assert!(out.alpha().is_some(), "a step that says it keeps every pixel in place");
        let out = after(vec![cut(), step("fun", FrameStage::Effects, Push(7, Arc::default()))]);
        assert_eq!(out.data[0], 7);
        assert!(out.alpha().is_none(), "an Effects step that doesn't say so may have moved pixels");
        let out = after(vec![cut(), step("wait", FrameStage::Cover, Rebuilds)]);
        assert!(out.alpha().is_none(), "the wait screen replaces the picture");
        let out = after(vec![cut(), step("view", FrameStage::Colour, Halves)]);
        assert_eq!((out.width, out.alpha().is_none()), (4, true), "a plane of the old size doesn't fit the new picture");
    }

    #[test]
    fn a_step_that_only_reads_keeps_the_cut_out() {
        let cut = || step("background", FrameStage::Background, Cuts);
        let out = after(vec![cut(), step("handoff", FrameStage::Effects, Reads)]);
        assert!(out.alpha().is_some(), "reading isn't moving");
        let out = after(vec![cut(), step("handoff", FrameStage::Cover, Reads)]);
        assert!(out.alpha().is_some());
    }

    #[test]
    fn a_plugin_shader_drops_the_cut_out_unless_it_is_known_to_keep_pixels_in_place() {
        let owned = |stage, keeps| Entry::new("p".into(), stage, FrameStep::Shader(ShaderStep { keeps_geometry: keeps, wgsl: String::new(), uniforms: serde_json::json!({}), textures: Vec::new() }), None);
        for stage in [FrameStage::Effects, FrameStage::Colour, FrameStage::Privacy, FrameStage::Overlay, FrameStage::Combine, FrameStage::Cover, FrameStage::Final] {
            assert!(owned(stage, false).drops_alpha(), "{stage:?}");
            assert!(!owned(stage, true).drops_alpha(), "{stage:?}");
        }
        assert!(!owned(FrameStage::Background, false).drops_alpha(), "the stage that makes the cut-out");
        let (table, cube) = (telescope_core::colour::Baked::Table(vec![0.0; 3 * 256]), telescope_core::colour::Baked::Cube { size: 2, rgb: vec![0.0; 24] });
        assert!(table.shader().keeps_geometry && cube.shader().keeps_geometry, "the built-in colour shaders work on each pixel alone");
    }

    /// An Either's CPU half whose shader on the card makes no cut-out: it gives the plane itself.
    struct CutsOnTheCard;

    impl CpuStep for CutsOnTheCard {
        fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
        fn card_textures_offered(&self) -> Option<bool> {
            Some(true)
        }
        fn card_textures(&mut self, _frame: &Frame, _ctx: &dyn FrameContext) -> Option<Vec<(String, Frame)>> {
            Some(Vec::new())
        }
        fn card_cut_out(&mut self, frame: &Frame, _ctx: &dyn FrameContext) -> Option<Arc<Alpha>> {
            Some(Arc::new(Alpha::new(frame.width, frame.height, vec![77; frame.width as usize * frame.height as usize], false)))
        }
    }

    #[test]
    fn a_cut_out_an_either_gives_for_its_card_shader_is_on_the_frame_that_comes_down() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let blur = ShaderStep { keeps_geometry: false, wgsl: DIM.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        let background = StagedStep { owner: "background".into(), stage: FrameStage::Background, step: FrameStep::Either(blur, Box::new(CutsOnTheCard)) };
        let keeping = match shader(INVERT).step {
            FrameStep::Shader(mut s) => {
                s.keeps_geometry = true;
                StagedStep { stage: FrameStage::Colour, step: FrameStep::Shader(s), ..shader(INVERT) }
            }
            _ => unreachable!(),
        };
        p.set_steps(vec![background, keeping, step("blur", FrameStage::Privacy, Brightens)]);
        let before = transfers::totals();
        let out = p.process(grey(8, 8, 100), &NoContext);
        assert!(transfers::totals().up > before.up, "the frame went to the card");
        assert_ne!(out.data[0], 100, "the card's work landed");
        assert_eq!(out.alpha().map(|a| a.data[0]), Some(77), "the plane the CPU half made went down with the picture");
    }

    #[test]
    fn a_frame_that_goes_dark_has_no_cut_out() {
        let mut p = Pipeline::new();
        p.set_steps(vec![step("background", FrameStage::Background, Cuts), step("blur", FrameStage::Privacy, HidingPanics)]);
        let out = quietly(|| p.process(grey(8, 8, 100), &NoContext));
        assert!(is_dark(&out) && out.alpha().is_none());
    }

    #[test]
    fn a_cut_out_is_still_there_after_the_card_has_worked_on_the_picture() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let on_card = |stage: FrameStage| StagedStep { stage, ..shader(INVERT) };
        let keeping = |stage: FrameStage| match shader(INVERT).step {
            FrameStep::Shader(mut s) => {
                s.keeps_geometry = true;
                StagedStep { stage, step: FrameStep::Shader(s), ..shader(INVERT) }
            }
            _ => unreachable!(),
        };
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        p.set_steps(vec![step("background", FrameStage::Background, Cuts), keeping(FrameStage::Colour), step("blur", FrameStage::Privacy, Brightens)]);
        let out = p.process(grey(8, 8, 100), &NoContext);
        assert_ne!(out.data[0], 100, "the card's work landed");
        assert_eq!(out.alpha().map(|a| a.data[0]), Some(128), "re-attached to the frame the card gave back");
        p.set_steps(vec![step("background", FrameStage::Background, Cuts), on_card(FrameStage::Effects)]);
        assert!(p.process(grey(8, 8, 100), &NoContext).alpha().is_none(), "a card-only step in Effects may move anything");
        p.set_steps(vec![step("background", FrameStage::Background, Cuts), on_card(FrameStage::Colour)]);
        assert!(p.process(grey(8, 8, 100), &NoContext).alpha().is_none(), "so may a plugin's shader at Colour that doesn't say otherwise");
    }

    /// A decoder picture still on the card, with this grey as the picture it would land as.
    #[derive(Debug)]
    struct OnTheCard(Frame);

    impl telescope_core::frame::DecodedSurface for OnTheCard {
        fn ram(&self) -> &Frame {
            &self.0
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn surface(w: u32, h: u32, v: u8) -> Frame {
        Frame::from_surface(w, h, PixelFormat::I420, Arc::new(OnTheCard(grey(w, h, v))))
    }

    fn wish(p: &Pipeline) -> bool {
        p.surface_wish().unwrap().load(Ordering::Relaxed)
    }

    #[test]
    fn the_decoder_is_only_asked_for_pictures_on_the_card_while_the_first_step_runs_there() {
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        assert_eq!(wish(&p), plain_allowed(), "no steps: the picture is converted on the card for the camera, where that's on");
        p.set_steps(vec![shader(INVERT)]);
        assert!(wish(&p), "a card step first");
        p.set_steps(vec![step("first", FrameStage::Background, Brightens), shader(INVERT)]);
        assert!(!wish(&p), "a CPU step first, whatever follows it");
        p.set_steps(vec![shader(INVERT), step("last", FrameStage::Privacy, Brightens)]);
        assert!(wish(&p), "a card step first, a CPU step after it");
        p.set_steps(Vec::new());
        assert_eq!(wish(&p), plain_allowed(), "steps taken off again");
        p.set_steps(vec![step("first", FrameStage::Background, Brightens)]);
        assert!(!wish(&p), "a CPU step alone");
        let (p, _) = either(Place::Card);
        assert!(wish(&p), "a step with both ways, with the card ahead");
        let (p, _) = either(Place::Cpu);
        assert!(!wish(&p), "a step with both ways, with the CPU ahead");
    }

    /// A decoder picture the card can scale to the camera's size: the picture that comes down scaled is a flat 200, the whole one a flat 90.
    #[derive(Debug, Default)]
    struct Scalable {
        whole: Option<Frame>,
        landed: std::sync::OnceLock<Frame>,
        fits: AtomicUsize,
    }

    impl telescope_core::frame::DecodedSurface for Scalable {
        fn ram(&self) -> &Frame {
            self.landed.get_or_init(|| self.whole.clone().unwrap())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn landed(&self) -> Option<&Frame> {
            self.landed.get()
        }
        fn can_fit(&self, width: u32, height: u32) -> bool {
            let whole = self.whole.as_ref().unwrap();
            crate::surface_fit::scales_to((whole.width, whole.height), (width, height))
        }
        fn fitted(&self, width: u32, height: u32) -> Option<Frame> {
            self.fits.fetch_add(1, Ordering::SeqCst);
            Some(grey(width, height, 200))
        }
    }

    fn scalable(w: u32, h: u32) -> (Frame, Arc<Scalable>) {
        let surface = Arc::new(Scalable { whole: Some(grey(w, h, 90)), ..Scalable::default() });
        (Frame::from_surface(w, h, PixelFormat::I420, surface.clone()), surface)
    }

    fn camera(p: &Pipeline, size: Option<(u32, u32)>) {
        p.camera_fit().unwrap().store(size.map_or(0, telescope_core::media::pack_camera_fit), Ordering::Relaxed);
    }

    #[test]
    fn a_plain_stream_into_a_smaller_camera_keeps_its_pictures_on_the_card_and_goes_back_whole_when_a_step_comes_on() {
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        assert!(!wish(&p) || plain_allowed(), "no camera open, nothing to scale for");
        camera(&p, Some((64, 36)));
        let (frame, surface) = scalable(128, 72);
        let out = p.process_picture(frame, &NoContext);
        assert!(out.picture().is_some(), "left on the card");
        assert_eq!((out.metadata().width, out.metadata().height), (128, 72), "anything reading the source size still sees it");
        assert!(wish(&p), "the next pictures are kept too");
        assert!(out.fits_on_card(64, 36));
        let fitted = out.land_fitted_until(64, 36, &|| false).unwrap();
        assert_eq!((fitted.width, fitted.height, fitted.data[0]), (64, 36, 200));
        assert_eq!(surface.fits.load(Ordering::SeqCst), 1);
        let whole = out.land();
        assert_eq!((whole.width, whole.height, whole.data[0]), (128, 72, 90), "landing it whole still gives the real picture");
        assert!(out.land_fitted_until(64, 36, &|| false).is_none(), "one in memory is fitted from there, not read back again");
        let (frame, surface) = scalable(128, 72);
        let again = p.process_picture(frame, &NoContext);
        assert!(again.landed_fitted(64, 36).is_none(), "nothing scaled yet");
        let first = again.land_fitted_until(64, 36, &|| false).unwrap();
        let second = again.land_fitted_until(64, 36, &|| false).unwrap();
        assert_eq!((first.width, second.width, surface.fits.load(Ordering::SeqCst)), (64, 64, 1), "the camera and the preview share one scaling");
        assert_eq!(again.landed_fitted(64, 36).map(|f| f.height), Some(36));
        assert!(again.landed_fitted(32, 18).is_none(), "another size isn't the kept one");
        // A step that needs the whole picture comes on: the next picture is whole, not dark, and the decoder is no longer asked to keep them
        p.set_steps(vec![step("first", FrameStage::Background, Brightens)]);
        assert!(!wish(&p));
        let (frame, surface) = scalable(128, 72);
        let out = p.process_picture(frame, &NoContext);
        assert!(out.picture().is_none());
        assert_eq!((out.metadata().width, out.metadata().height), (128, 72));
        assert!(out.land().data[0] >= 90, "the picture the step began from, not a dark one");
        assert_eq!(surface.fits.load(Ordering::SeqCst), 0);
        // A picture kept before the step came on still comes down whole when the steps run
        p.set_steps(Vec::new());
        let (kept, _) = scalable(128, 72);
        p.set_steps(vec![step("first", FrameStage::Background, Brightens)]);
        let out = p.process_picture(kept, &NoContext);
        assert!(out.picture().is_none() && out.land().data[0] >= 90);
        // The camera closes: pictures come down whole as before
        p.set_steps(Vec::new());
        camera(&p, None);
        let (frame, _) = scalable(128, 72);
        assert!(p.process_picture(frame, &NoContext).picture().is_none() || plain_allowed());
        assert!(!wish(&p) || plain_allowed());
    }

    #[test]
    fn a_camera_as_big_as_the_stream_or_of_another_shape_leaves_its_pictures_alone() {
        let mut p = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        for size in [(128, 72), (64, 48), (256, 144)] {
            camera(&p, Some(size));
            let (frame, _) = scalable(128, 72);
            let out = p.process_picture(frame, &NoContext);
            assert!(out.picture().is_none() || plain_allowed(), "{size:?}");
        }
    }

    #[test]
    fn a_plain_stream_converts_on_the_card_on_windows_only_when_asked_to() {
        assert!(plain_gate(false, false), "Linux: on by default");
        assert!(!plain_gate(true, false), "Windows: off by default");
        assert!(plain_gate(true, true), "Windows: opted in");
    }

    #[test]
    fn a_stream_with_no_steps_only_converts_on_the_card_while_one_is_there_to_use() {
        assert!(!wish(&Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Cpu)))), "the balance moved work off a busy card");
        let mut missing = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        missing.gpu = Some(None);
        missing.set_steps(Vec::new());
        assert!(!wish(&missing), "no card");
        let mut lost = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        lost.lost_card("a test");
        lost.set_steps(Vec::new());
        assert!(!wish(&lost), "a card that failed a frame");
    }

    #[test]
    fn a_stream_with_no_card_step_gets_full_pixels_even_from_a_picture_left_on_the_card() {
        let mut none = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        let out = none.process_picture(surface(8, 8, 90), &NoContext);
        assert!(out.metadata().surface().is_none(), "the picture came into memory before the steps");
        assert_eq!(out.land().data, grey(8, 8, 90).data);
        assert_eq!(none.process(surface(8, 8, 90), &NoContext).data, grey(8, 8, 90).data);
        let mut cpu = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
        cpu.set_steps(vec![step("first", FrameStage::Background, Push(5, Arc::default()))]);
        let out = cpu.process_picture(surface(8, 8, 90), &NoContext);
        assert!(out.metadata().surface().is_none());
        let landed = out.land();
        assert_eq!(landed.data.len(), PixelFormat::I420.size(8, 8), "the CPU step had a whole picture to work on");
        assert_eq!((landed.data[0], landed.data[1]), (5, 90));
    }

    /// A decoder picture that says whether anything still holds it.
    #[derive(Debug)]
    struct Counted(Frame, #[allow(dead_code)] Arc<()>);

    impl telescope_core::frame::DecodedSurface for Counted {
        fn ram(&self) -> &Frame {
            &self.0
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn a_card_picture_lets_go_of_the_decoder_picture_it_was_made_from_once_it_has_landed() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let alive = Arc::new(());
        let mut p = card_pipeline(gpu);
        let out = p.process_picture(Frame::from_surface(8, 8, PixelFormat::I420, Arc::new(Counted(grey(8, 8, 100), alive.clone()))), &NoContext);
        assert!(out.metadata().data.is_empty() && out.metadata().surface().is_none(), "what is left of the input in the slot's record has no pixels and no hold on the decoder");
        out.land();
        assert_eq!(Arc::strong_count(&alive), 1, "the decoder's surface is free again, not held until the picture goes");
    }

    #[test]
    fn a_picture_left_on_the_card_is_landed_whole_for_a_card_step_and_a_reader_in_memory() {
        let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
        let mut p = card_pipeline(gpu);
        let out = p.process_picture(surface(8, 8, 100), &NoContext);
        let landed = out.land();
        assert_eq!(landed.data.len(), PixelFormat::I420.size(8, 8));
        assert!((landed.data[0] as i32 - 151).abs() <= 2, "the card worked on the picture ({})", landed.data[0]);
        assert!(!p.process(surface(8, 8, 100), &NoContext).data.is_empty());
    }
}
