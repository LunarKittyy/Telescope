//! A stream's frame steps, run in FRAME_STAGES order (stream_flow.pipeline and StreamWorker._process). CPU steps run on
//! the stream's thread; shader steps run on the graphics card, and a run of them one after the other keeps the picture there.

use crate::gpu::{self, Gpu, ShaderRun};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep};
use telescope_core::media::{FramePipeline, StagedStep};
use telescope_core::time::monotonic_ns;

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

/// Runs one stream's steps. The graphics card is only opened once a shader step comes along.
#[derive(Default)]
pub struct Pipeline {
    steps: Vec<Entry>,
    gpu: Option<Option<Arc<Gpu>>>,
    started_ns: Option<u64>,
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
        // A step that builds a new frame stays on this one: person masks are worked out once per number
        let number = frame.number;
        let mut i = 0;
        while i < self.steps.len() {
            if self.steps[i].skipped() {
                i += 1;
                continue;
            }
            if matches!(self.steps[i].kind, Kind::Cpu(_)) {
                let entry = &mut self.steps[i];
                let Kind::Cpu(step) = &mut entry.kind else { unreachable!() };
                // A clone shares the pixels, so keeping the frame to fall back on only costs a copy if the step changes it
                let before = frame.clone();
                match catch_unwind(AssertUnwindSafe(|| step.process(&mut frame, ctx))) {
                    Ok(()) if sound(&frame) => entry.fails = 0,
                    Ok(()) => {
                        entry.failed(&format!("handed back {}x{} {:?} with {} bytes", frame.width, frame.height, frame.format, frame.data.len()));
                        frame = before;
                    }
                    Err(_) => {
                        entry.failed("it panicked");
                        frame = before;
                    }
                }
                frame.number = number;
                i += 1;
                continue;
            }
            // Every shader step from here up to the next CPU step goes to the graphics card in one go
            let end = (i..self.steps.len()).find(|&j| matches!(self.steps[j].kind, Kind::Cpu(_))).unwrap_or(self.steps.len());
            let Some(gpu) = self.gpu() else {
                for e in &mut self.steps[i..end] {
                    e.failed("no graphics card to run shaders on");
                }
                i = end;
                continue;
            };
            let time = (monotonic_ns().saturating_sub(started)) as f32 / 1e9;
            let mut run: Vec<ShaderRun> = Vec::new();
            for e in &mut self.steps[i..end] {
                if e.skipped() {
                    continue;
                }
                let Kind::Shader(slot) = &mut e.kind else { unreachable!() };
                match slot.prepare(&gpu) {
                    Ok(()) => run.push(ShaderRun { slot, fails: &mut e.fails }),
                    Err(why) => {
                        let (owner, fails) = (e.owner.clone(), &mut e.fails);
                        // A shader that doesn't build never will: say so once and leave it out
                        if *fails < STEP_FAILS_BEFORE_SKIP {
                            log::error!("Plugin {owner}'s shader can't run ({why}); leaving it out");
                            *fails = STEP_FAILS_BEFORE_SKIP;
                        }
                    }
                }
            }
            if !run.is_empty() {
                match gpu.run_shaders(&frame, &mut run, time) {
                    Ok(out) => frame = out,
                    Err(why) => {
                        drop(run);
                        for e in &mut self.steps[i..end] {
                            if !e.skipped() {
                                e.failed(&why);
                            }
                        }
                    }
                }
            }
            i = end;
        }
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
    use telescope_core::frame::PixelFormat;

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
