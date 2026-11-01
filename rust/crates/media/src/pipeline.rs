//! A stream's frame steps, run in FRAME_STAGES order (stream_flow.pipeline and StreamWorker._process), which is never changed. CPU
//! steps run on the stream's thread and shader steps on the graphics card. The picture goes up at the first shader and stays there
//! for as long as the order allows: a skipped or idle CPU step doesn't bring it down, one that only reads brings a copy down, and
//! only one that changes the pixels makes the next shader upload again. Transfers are counted here; see transfers.rs.

use crate::gpu::{self, Gpu, Held, MaskPlane};
use crate::transfers;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, Touch};
use telescope_core::media::{FramePipeline, StagedStep};
use telescope_core::time::{monotonic_ns, span};

/// A plugin step that fails this many frames in a row is skipped until the next stream.
pub const STEP_FAILS_BEFORE_SKIP: u32 = 30;

enum Kind {
    Cpu(Box<dyn CpuStep>),
    Shader(gpu::ShaderSlot),
}

struct Entry {
    owner: String,
    stage: FrameStage,
    kind: Kind,
    fails: u32,
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

/// Runs one stream's steps. The graphics card is only opened once a shader step comes along.
#[derive(Default)]
pub struct Pipeline {
    steps: Vec<Entry>,
    gpu: Option<Option<Arc<Gpu>>>,
    started_ns: Option<u64>,
    mask: Option<MaskPlane>,
    tally: Tally,
    /// What the last frame cost: (up, down).
    last_transfers: (u32, u32),
}

impl Pipeline {
    pub fn new() -> Pipeline {
        Pipeline::default()
    }

    /// The stages in the order their steps run, for tests and diagnostics.
    pub fn stages(&self) -> Vec<(String, FrameStage)> {
        self.steps.iter().map(|e| (e.owner.clone(), e.stage)).collect()
    }

    fn gpu(&mut self) -> Option<Arc<Gpu>> {
        self.gpu.get_or_insert_with(Gpu::shared).clone()
    }

    /// Brings frame up to date with the card, and drops the card's copy unless keep.
    fn bring_down(&mut self, chain: &mut Chain, frame: &mut Frame, keep: bool) {
        let Some(gpu) = self.gpu.clone().flatten() else { return };
        if let (Some(held), false) = (chain.held.as_mut(), chain.ahead.is_empty()) {
            chain.downs += 1;
            match gpu.land(held) {
                Ok(out) => {
                    *frame = out;
                    for i in chain.ahead.drain(..) {
                        self.steps[i].fails = 0;
                    }
                }
                Err(why) => {
                    // The frame in hand is the last good one, and what the card did after it is lost with it
                    for i in chain.ahead.drain(..) {
                        self.steps[i].failed(&why);
                    }
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

    fn run_cpu(&mut self, i: usize, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext) {
        let Kind::Cpu(step) = &self.steps[i].kind else { unreachable!() };
        // One that panics here will panic in process too, where that's dealt with
        let touch = catch_unwind(AssertUnwindSafe(|| step.touch(ctx))).unwrap_or(Touch::Changes);
        if touch == Touch::Nothing {
            return;
        }
        self.bring_down(chain, frame, touch == Touch::Reads);
        let entry = &mut self.steps[i];
        let Kind::Cpu(step) = &mut entry.kind else { unreachable!() };
        // A clone shares the pixels, so keeping the frame to fall back on only costs a copy if the step changes it
        let before = frame.clone();
        match catch_unwind(AssertUnwindSafe(|| step.process(frame, ctx))) {
            Ok(()) if sound(frame) => entry.fails = 0,
            Ok(()) => {
                entry.failed(&format!("handed back {}x{} {:?} with {} bytes", frame.width, frame.height, frame.format, frame.data.len()));
                *frame = before.clone();
            }
            Err(_) => {
                entry.failed("it panicked");
                *frame = before.clone();
            }
        }
        // A step that said it only reads and then didn't: the frame in hand wins
        if !Arc::ptr_eq(&frame.data, &before.data) {
            self.bring_down(chain, frame, false);
        }
    }

    fn run_shader(&mut self, i: usize, frame: &mut Frame, chain: &mut Chain, ctx: &dyn FrameContext, time: f32) {
        let Some(gpu) = self.gpu() else { return self.steps[i].failed("no graphics card to run shaders on") };
        let entry = &mut self.steps[i];
        let Kind::Shader(slot) = &mut entry.kind else { unreachable!() };
        if let Err(why) = slot.prepare(&gpu) {
            // A shader that doesn't build never will: say so once and leave it out
            log::error!("Plugin {}'s shader can't run ({why}); leaving it out", entry.owner);
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
        let Kind::Shader(slot) = &entry.kind else { unreachable!() };
        if chain.held.is_none() {
            match gpu.lift(frame) {
                Ok(held) => {
                    chain.held = Some(held);
                    chain.ups += 1;
                }
                Err(why) => return entry.failed(&why),
            }
        }
        gpu.shade(chain.held.as_mut().unwrap(), slot, time, self.mask.as_ref().filter(|_| masked));
        chain.ahead.push(i);
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
                log::info!("Frame steps cost {:.2} up and {:.2} down per frame over {TALLY_FRAMES} frames, with {} steps and {} person masks up",
                           per(t.ups), per(t.downs), self.steps.len(), masks - t.masks);
                t.logged = (t.ups, t.downs);
            }
            (t.frames, t.ups, t.downs, t.masks) = (0, 0, 0, masks);
        }
    }
}

/// Whether a step handed back something the rest of the stream can use.
fn sound(frame: &Frame) -> bool {
    frame.width > 0 && frame.height > 0 && frame.data.len() == frame.format.size(frame.width, frame.height)
}

impl FramePipeline for Pipeline {
    fn set_steps(&mut self, steps: Vec<StagedStep>) {
        let mut entries: Vec<Entry> = steps.into_iter().map(|s| Entry {
            owner: s.owner,
            stage: s.stage,
            kind: match s.step {
                FrameStep::Cpu(c) => Kind::Cpu(c),
                FrameStep::Shader(sh) => Kind::Shader(gpu::ShaderSlot::new(sh)),
            },
            fails: 0,
        }).collect();
        // Stable: as registered within a stage
        entries.sort_by_key(|e| e.stage);
        self.steps = entries;
    }

    fn replace_steps(&mut self, owner: &str, stage: FrameStage, steps: Vec<FrameStep>) {
        self.steps.retain(|e| e.owner != owner);
        let fresh = steps.into_iter().map(|step| Entry {
            owner: owner.to_string(),
            stage,
            kind: match step {
                FrameStep::Cpu(c) => Kind::Cpu(c),
                FrameStep::Shader(sh) => Kind::Shader(gpu::ShaderSlot::new(sh)),
            },
            fails: 0,
        });
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
        for i in 0..self.steps.len() {
            if self.steps[i].skipped() {
                continue;
            }
            match self.steps[i].kind {
                Kind::Cpu(_) => self.run_cpu(i, &mut frame, &mut chain, ctx),
                Kind::Shader(_) => self.run_shader(i, &mut frame, &mut chain, ctx, time),
            }
            frame.number = number;
        }
        self.bring_down(&mut chain, &mut frame, false);
        frame.number = number;
        self.count(chain.ups, chain.downs);
        span("pipeline", began);
        frame
    }

    fn is_empty(&self) -> bool {
        self.steps.is_empty()
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
        for _ in 0..STEP_FAILS_BEFORE_SKIP + 10 {
            let out = quietly(|| p.process(source.clone(), &NoContext));
            assert_eq!(out.data, source.data, "passed through, what the step did to it left behind");
            assert_eq!((out.width, out.height), (4, 2));
        }
        assert_eq!(*calls.lock().unwrap(), STEP_FAILS_BEFORE_SKIP, "skipped from then on");
        assert!(p.steps[0].skipped() && p.steps[1].skipped());
        // What the decoder made is never changed under it
        assert_eq!(source.data[0], 1);
        // The next stream's pipeline tries them again, and steps after a broken one still run
        p.set_steps(vec![step("broken", FrameStage::Effects, Panics), step("fine", FrameStage::Overlay, Push(5, seen.clone()))]);
        let out = quietly(|| p.process(grey(4, 2, 1), &NoContext));
        assert_eq!(out.data[0], 5);
        assert_eq!(*seen.lock().unwrap(), [5]);
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
