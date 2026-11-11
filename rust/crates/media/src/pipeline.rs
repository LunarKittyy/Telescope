//! A stream's frame steps, run in FRAME_STAGES order (stream_flow.pipeline and StreamWorker._process), which is never changed. CPU
//! steps run on the stream's thread and shader steps on the graphics card. The picture goes up at the first shader and stays there
//! for as long as the order allows: a skipped or idle CPU step doesn't bring it down, one that only reads brings a copy down, and
//! only one that changes the pixels makes the next shader upload again. Transfers are counted here; see transfers.rs. A step that can
//! run either way goes where core::balance says, which hears from here what each frame waited for the card.

use crate::gpu::{self, Gpu, Held, MaskPlane};
use crate::transfers;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use telescope_core::balance::{Balance, Place};
use telescope_core::image::SparePool;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat, Touch};
use telescope_core::media::{FramePipeline, StagedStep};
use telescope_core::stats::{SourceStats, Timer};
use telescope_core::time::{monotonic_ns, span};

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

    fn slot(&mut self) -> &mut gpu::ShaderSlot {
        match self {
            Kind::Shader(slot) | Kind::Either(slot, _) => slot,
            Kind::Cpu(_) => unreachable!("a CPU step has no shader"),
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
    /// What it costs the stream's thread, filled while the stats ask.
    timer: Option<Arc<Timer>>,
}

impl Entry {
    fn skipped(&self) -> bool {
        self.fails >= STEP_FAILS_BEFORE_SKIP
    }

    fn failed(&mut self, why: &str) {
        self.fails += 1;
        if self.fails == 1 {
            log::error!("Plugin {} failed on a frame ({why}); passing it through unchanged", self.owner);
        } else if self.fails == STEP_FAILS_BEFORE_SKIP {
            log::error!("Plugin {} failed on {} frames in a row; skipping it until the stream restarts", self.owner, self.fails);
        }
    }
}

/// How many pictures' worth of RGB the shared spare pool keeps between steps.
const SPARE_POOL_FRAMES: usize = 5;

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
    mask: Option<(u32, u32, Option<Arc<Vec<u8>>>)>,
    /// What it has waited for the card so far.
    waited_ns: u64,
    /// Whether the steps are being timed, so the readbacks are too.
    timing: bool,
    /// How long the readbacks took, which isn't the cost of the step that happened to trigger one.
    down_ns: u64,
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
    balance: Arc<Balance>,
    /// The card failed a frame: steps that can run on the CPU stay there until the next stream, whatever the balance says.
    card_lost: bool,
    stats: Option<Arc<SourceStats>>,
    /// The spare buffers every CPU step of this stream draws on.
    pool: SparePool,
    /// Whether this pipeline is counted in ON_THE_CARD.
    on_card: bool,
    /// How long the last frame spent bringing pictures down from the card (measured while the steps are timed).
    last_readback_ns: u64,
}

impl Drop for Pipeline {
    fn drop(&mut self) {
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

    /// One that listens to this balance, for tests.
    fn with_balance(balance: Arc<Balance>) -> Pipeline {
        Pipeline { steps: Vec::new(), gpu: None, started_ns: None, mask: None, tally: Tally::default(), last_transfers: (0, 0), balance, card_lost: false, stats: None, pool: SparePool::default(), on_card: false, last_readback_ns: 0 }
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
    fn bring_down(&mut self, chain: &mut Chain, frame: &mut Frame, keep: bool) {
        let Some(gpu) = self.gpu.clone().flatten() else { return };
        if let (Some(held), false) = (chain.held.as_mut(), chain.ahead.is_empty()) {
            chain.downs += 1;
            let cap = SPARE_POOL_FRAMES * held.size().0 as usize * held.size().1 as usize * 3;
            let down_began = chain.timing.then(monotonic_ns);
            let landed = self.pool.scope(cap, || gpu.land(held).map(|out| std::mem::replace(frame, out).recycle()));
            if let Some(began) = down_began {
                chain.down_ns += monotonic_ns().saturating_sub(began);
            }
            match landed {
                Ok(()) => {
                    chain.waited_ns += held.take_waited();
                    for i in chain.ahead.drain(..) {
                        self.steps[i].fails = 0;
                    }
                }
                Err(why) => {
                    // The frame in hand is the last good one, and what the card did after it is lost with it
                    for i in chain.ahead.drain(..) {
                        if matches!(self.steps[i].kind, Kind::Shader(_)) {
                            self.steps[i].failed(&why);
                        }
                    }
                    self.lost_card(&why);
                    gpu.put_down(chain.held.take().unwrap());
                }
            }
        }
        if !keep {
            if let Some(held) = chain.held.take() {
                gpu.put_down(held);
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
        self.bring_down(chain, frame, touch == Touch::Reads);
        let entry = &mut self.steps[i];
        let step = entry.kind.cpu();
        // A step that changes pixels in place would copy them all if a clone kept the old ones, so only one that failed on its last frame
        // gets that fallback; a built-in step that panics the first time may hand on a half-changed frame. Whole-frame writers build a new
        // buffer and swap it in, so they leave this one intact either way.
        let before = (entry.fails > 0).then(|| frame.clone());
        let (size, format) = ((frame.width, frame.height), frame.format);
        let was = Arc::downgrade(&frame.data);
        let cap = SPARE_POOL_FRAMES * frame.width as usize * frame.height as usize * 3;
        let pool = &mut self.pool;
        match catch_unwind(AssertUnwindSafe(|| pool.scope(cap, || step.process(frame, ctx)))) {
            Ok(()) if sound(frame) => entry.fails = 0,
            Ok(()) => {
                entry.failed(&format!("handed back {}x{} {:?} with {} bytes", frame.width, frame.height, frame.format, frame.data.len()));
                *frame = before.clone().unwrap_or_else(|| neutral(size, format, frame.number, frame.time_ns));
            }
            Err(_) => {
                entry.failed("it panicked");
                match &before {
                    Some(before) => *frame = before.clone(),
                    None if !sound(frame) => *frame = neutral(size, format, frame.number, frame.time_ns),
                    None => {}
                }
            }
        }
        // A step that said it only reads and then didn't: the frame in hand wins
        if !std::ptr::eq(Weak::as_ptr(&was), Arc::as_ptr(&frame.data)) {
            self.bring_down(chain, frame, false);
        }
    }

    fn run_shader(&mut self, i: usize, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext, time: f32) {
        let Some(gpu) = self.gpu() else { return self.steps[i].failed("no graphics card to run shaders on") };
        let entry = &mut self.steps[i];
        // A step with a CPU half may need uniforms that depend on this frame's size
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
            return;
        }
        let mut masked = false;
        if slot.wants_mask() {
            // The vision model looks at the picture as it is here, and answers once per frame
            if !chain.mask.as_ref().is_some_and(|(w, h, _)| (*w, *h) == (frame.width, frame.height)) {
                self.bring_down(chain, frame, true);
                let asked = catch_unwind(AssertUnwindSafe(|| ctx.person_mask(frame))).unwrap_or(None);
                chain.mask = Some((frame.width, frame.height, asked));
            }
            if let Some((w, h, Some(mask))) = &chain.mask {
                masked = gpu.keep_mask(&mut self.mask, mask, *w, *h);
            }
        }
        let entry = &mut self.steps[i];
        if chain.held.is_none() {
            match gpu.lift(frame) {
                Ok(held) => {
                    chain.held = Some(held);
                    chain.ups += 1;
                }
                Err(why) if matches!(entry.kind, Kind::Either(..)) => {
                    self.lost_card(&why);
                    return self.run_cpu(i, frame, chain, ctx);
                }
                Err(why) => return entry.failed(&why),
            }
        }
        gpu.shade(chain.held.as_mut().unwrap(), entry.kind.slot(), time, self.mask.as_ref().filter(|_| masked));
        chain.ahead.push(i);
    }

    /// Keeps a step that's off the card ready for coming back, so that doesn't cost a frame later.
    fn ready_on_card(&mut self, i: usize) {
        if let Some(gpu) = self.gpu() {
            if let Err(why) = self.steps[i].kind.slot().prepare(&gpu) {
                log::error!("Plugin {}'s shader can't run ({why}); it stays on the CPU", self.steps[i].owner);
                self.steps[i].no_shader = true;
            }
        }
    }

    /// While frames stay off the card, sends one of them there now and then to see what it would have waited.
    fn try_card(&mut self, frame: &Frame) {
        let Some(gpu) = self.gpu() else { return };
        if !self.balance.trial_due() {
            return;
        }
        let frame = frame.clone();
        let done = Trial(self.balance.clone(), None);
        let trial = move || {
            // Taken whole here: a closure would only capture the field it writes, and the rest would drop now
            let mut done = done;
            match gpu.trial(&frame) {
                Ok(waited) => done.1 = Some(waited),
                Err(why) => log::warn!("Couldn't try the graphics card: {why}"),
            }
        };
        if let Err(why) = std::thread::Builder::new().name("card-trial".into()).spawn(trial) {
            log::warn!("Couldn't try the graphics card: {why}");
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

/// A trial out on the card, however it ends: a panic or a thread that wouldn't start still lets the next one go.
struct Trial(Arc<Balance>, Option<u64>);

impl Drop for Trial {
    fn drop(&mut self) {
        self.0.trial_done(self.1);
    }
}

/// A flat dark picture of this size, for a step that broke the frame it was given when there is no copy to go back to.
fn neutral((w, h): (u32, u32), format: PixelFormat, number: u64, time_ns: u64) -> Frame {
    let (luma, rest) = (w as usize * h as usize, format.size(w, h));
    let data = match format {
        PixelFormat::I420 | PixelFormat::Nv12 => [vec![16; luma], vec![128; rest - luma]].concat(),
        _ => [0, 0, 0, 255].repeat(luma),
    };
    let mut out = Frame::new(w, h, format, data);
    (out.number, out.time_ns) = (number, time_ns);
    out
}

/// Whether a step handed back something the rest of the stream can use.
fn sound(frame: &Frame) -> bool {
    frame.width > 0 && frame.height > 0 && frame.data.len() == frame.format.size(frame.width, frame.height)
}

impl FramePipeline for Pipeline {
    fn set_steps(&mut self, steps: Vec<StagedStep>) {
        let mut entries: Vec<Entry> = steps.into_iter().map(|s| Entry { timer: self.timer_of(&s.owner), owner: s.owner, stage: s.stage, kind: Kind::of(s.step), fails: 0, no_shader: false }).collect();
        // Stable: as registered within a stage
        entries.sort_by_key(|e| e.stage);
        self.steps = entries;
    }

    fn replace_steps(&mut self, owner: &str, stage: FrameStage, steps: Vec<FrameStep>) {
        self.steps.retain(|e| e.owner != owner);
        let timer = self.timer_of(owner);
        let fresh: Vec<Entry> = steps.into_iter().map(|step| Entry { owner: owner.to_string(), stage, kind: Kind::of(step), fails: 0, no_shader: false, timer: timer.clone() }).collect();
        // First within the stage, where the window's own steps go
        let at = self.steps.iter().position(|e| e.stage >= stage).unwrap_or(self.steps.len());
        self.steps.splice(at..at, fresh);
    }

    fn process(&mut self, mut frame: Frame, ctx: &dyn FrameContext) -> Frame {
        let started = *self.started_ns.get_or_insert_with(monotonic_ns);
        let time = monotonic_ns().saturating_sub(started) as f32 / 1e9;
        // A step that builds a new frame stays on this one: person masks are worked out once per number
        let number = frame.number;
        let began = monotonic_ns();
        let mut chain = Chain::default();
        // Asked once, so a frame doesn't change its mind halfway
        let on_card = self.balance.place() == Place::Card;
        let mut off_card = false;
        // Asked once a frame: a step is only timed while the stats panel wants every plugin's time
        let timing = self.stats.as_ref().is_some_and(|s| s.steps().on());
        chain.timing = timing;
        for i in 0..self.steps.len() {
            if self.steps[i].skipped() {
                continue;
            }
            let step_began = timing.then(monotonic_ns);
            let down_before = chain.down_ns;
            let either = matches!(self.steps[i].kind, Kind::Either(..)) && !self.steps[i].no_shader && !self.card_lost;
            if either && !(on_card && self.gpu().is_some()) {
                self.ready_on_card(i);
                self.run_cpu(i, &mut frame, &mut chain, ctx);
                off_card = true;
            } else if either || matches!(self.steps[i].kind, Kind::Shader(_)) {
                self.run_shader(i, &mut frame, &mut chain, ctx, time);
            } else {
                self.run_cpu(i, &mut frame, &mut chain, ctx);
            }
            if let (Some(began), Some(timer)) = (step_began, &self.steps[i].timer) {
                // Bringing the picture down from the card is the card's cost (the readback figure), not this step's
                timer.add(monotonic_ns().saturating_sub(began).saturating_sub(chain.down_ns - down_before));
            }
            frame.number = number;
        }
        self.bring_down(&mut chain, &mut frame, false);
        frame.number = number;
        self.last_readback_ns = chain.down_ns;
        if chain.downs > 0 {
            self.balance.note_wait(chain.waited_ns);
        }
        if off_card {
            self.try_card(&frame);
        }
        self.count(chain.ups, chain.downs);
        span("pipeline", began);
        frame
    }

    fn is_empty(&self) -> bool {
        self.steps.is_empty()
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
    use telescope_core::frame::{PixelFormat, ShaderStep};

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
        let step = ShaderStep { wgsl: wgsl.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
        StagedStep { owner: "shader".into(), stage: FrameStage::Effects, step: FrameStep::Shader(step) }
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
    fn a_zoomed_or_stretched_view_never_visits_the_card() {
        use telescope_core::host::View;
        for view in [View { zoom: 2.0, ..View::default() }, View { stretch_x: 0.5, ..View::default() }, View { rotation: Some(90), zoom: 1.5, stretch_y: 1.2, ..View::default() }] {
            let mut p = Pipeline::new();
            let steps = telescope_core::view_step::steps(&view);
            p.set_steps(steps.into_iter().map(|step| StagedStep { owner: "view".into(), stage: FrameStage::View, step }).collect());
            let out = p.process(grey(8, 8, 10), &NoContext);
            assert_eq!(p.last_transfers, (0, 0), "{view:?}: a busy card can't hold the frame up");
            assert_eq!(out.data.len(), out.format.size(out.width, out.height), "{view:?}: a whole picture comes out");
        }
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
        let step = ShaderStep { wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
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
        p.balance = balance.clone();
        p.process(grey(8, 8, 100), &NoContext);
        assert_eq!(p.last_transfers, (1, 1));
        (0..8).for_each(|_| balance.note_wait(50_000_000));
        assert_eq!(balance.place(), Place::Cpu);
        p.process(grey(8, 8, 100), &NoContext);
        assert_eq!(p.last_transfers, (0, 0));
    }

    #[test]
    fn a_shader_that_wont_build_leaves_a_step_with_two_ways_on_the_cpu() {
        if Gpu::shared().is_none() {
            return eprintln!("no graphics card; skipped");
        }
        let (mut p, _) = either(Place::Card);
        let broken = ShaderStep { wgsl: "fn effect(uv: vec2<f32>) -> vec4<f32> { return nope; }".into(), uniforms: serde_json::json!({}), textures: Vec::new() };
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
        gpu.lose();
        // The frame on the card when it went is the last that isn't changed
        p.process(grey(8, 8, 100), &NoContext);
        for _ in 0..STEP_FAILS_BEFORE_SKIP + 5 {
            assert_eq!(p.process(grey(8, 8, 100), &NoContext).data[0], 167);
            assert_eq!(p.last_transfers, (0, 0));
        }
        assert!(!p.steps[0].skipped());
        // Changing the grade hands over a new step, which doesn't go back
        let step = ShaderStep { wgsl: INVERT.into(), uniforms: serde_json::json!({}), textures: Vec::new() };
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
}
