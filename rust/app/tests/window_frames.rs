// The window's steps run on real frames.
mod rig;

use rig::Rig;
use std::sync::Arc;
use telescope_core::colour::{lattice, with_split, ColourPipeline};
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::host::ColourStage;
use telescope_core::media::FramePipeline;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_media::pipeline::Pipeline;

struct Adds100;

impl CpuStep for Adds100 {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        for pixel in frame.data_mut().chunks_mut(4) {
            pixel[0..3].iter_mut().for_each(|v| *v += 100);
        }
    }
}

struct Brighten;

impl Plugin for Brighten {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "brighten".into(), frame_stage: FrameStage::Effects, ..Default::default() }
    }
    fn frame_step(&mut self) -> Option<FrameStep> {
        Some(FrameStep::Cpu(Box::new(Adds100)))
    }
}

struct Phone;

impl FrameContext for Phone {
    fn source_id(&self) -> &str {
        "pixel"
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

#[test]
fn colour_stages_go_on_after_the_plugins() {
    if telescope_media::gpu::Gpu::shared().is_none() {
        return eprintln!("no graphics card; skipped");
    }
    let r = Rig::with("colour-last", |_, _| vec![Box::new(Brighten) as Box<dyn Plugin>]);
    let mut pipeline = Pipeline::new();
    {
        let mut state = r.w.state.borrow_mut();
        state.colour = ColourPipeline::inline();
        let half = ColourStage::new(2, Arc::new(lattice(2).iter().map(|v| v * 0.5).collect()), None);
        state.colour.set_stage("grade", Some(half), 0, None);
        state.colour.set_split("grade", Some(0.5));
        pipeline.set_steps(state.pipeline_steps(None));
    }
    let out = pipeline.process(Frame::new(4, 4, PixelFormat::Rgba, [0, 0, 0, 255].repeat(16)), &Phone);
    assert!(out.data[12].abs_diff(50) <= 1, "brightened first, then halved: {}", out.data[12]);
    let state = r.w.state.borrow();
    let before = state.colour_before.lock().unwrap()["pixel"].clone();
    assert_eq!(with_split(&before, &out, 0.5).data[0], 100, "before the colour step on the preview's left");
}
