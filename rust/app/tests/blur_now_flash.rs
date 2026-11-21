// Blur now on, off and on again over a shown stream and a stream behind the panels, and a Composite of the two: no output frame goes dark between the clear and the blurred picture.
use std::sync::Arc;
use serde_json::json;
use telescope_core::frame::{Frame, FrameContext, FrameStage, PixelFormat};
use telescope_core::media::{FramePipeline, StagedStep};
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::UiEvent;
use telescope_effects::composite::{Layout, Tile};
use telescope_effects::compositor::{Compositor, Frames};
use telescope_effects::privacy_blur::PrivacyBlurPlugin;
use telescope_media::pipeline::Pipeline;

struct Ctx;

impl FrameContext for Ctx {
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

/// A picture with detail, so a blur changes it but keeps roughly its brightness.
fn picture(shift: usize) -> Frame {
    let (w, h) = (128usize, 72usize);
    let mut data: Vec<u8> = (0..w * h).map(|i| 90 + (((i % w + shift) / 8 + i / w / 8) % 2) as u8 * 80).collect();
    data.extend(vec![128; 2 * (w / 2) * (h / 2)]);
    Frame::new(w as u32, h as u32, PixelFormat::I420, data)
}

fn mean_of(frame: &Frame) -> f64 {
    let luma = match frame.format {
        PixelFormat::I420 => &frame.data[..frame.width as usize * frame.height as usize],
        _ => &frame.data[..],
    };
    luma.iter().map(|v| *v as f64).sum::<f64>() / luma.len() as f64
}

fn click(plugin: &mut PrivacyBlurPlugin, host: &mut FakeHost) {
    plugin.on_ui(host, &UiEvent { ui: "card".into(), callback: "now-clicked".into(), args: vec![] });
}

fn stream(plugin: &mut PrivacyBlurPlugin) -> Pipeline {
    let mut p = Pipeline::new();
    p.set_steps(vec![StagedStep { owner: "privacy_blur".into(), stage: FrameStage::Privacy, step: plugin.frame_step().unwrap() }]);
    p
}

#[test]
fn blur_now_on_off_and_on_again_never_sends_a_dark_frame_on_any_output() {
    let mut host = FakeHost::new();
    host.focused = Some("a".into());
    host.streaming = true;
    host.vision = Some("ready".into());
    let mut plugin = PrivacyBlurPlugin::new();
    plugin.setup(&mut host);
    plugin.set_config(&json!({})).unwrap();
    let (mut shown, mut behind) = (stream(&mut plugin), stream(&mut plugin));
    let layout = Layout::of(vec![Tile::boxed("a", "", 0.0, 0.0, 0.5, 1.0), Tile::boxed("b", "", 0.5, 0.0, 0.5, 1.0)]);
    let mut compositor = Compositor::default();
    compositor.prepare(&layout, (256, 72));
    // Each output's brightness after every frame it sent: the shown stream, the one behind, and the composite of what both last sent
    let mut seen: [Vec<f64>; 3] = Default::default();
    let mut last: [Option<Frame>; 2] = [None, None];
    let mut note = |last: &[Option<Frame>; 2], seen: &mut [Vec<f64>; 3], which: usize| {
        seen[which].push(mean_of(last[which].as_ref().unwrap()));
        if let [Some(a), Some(b)] = last {
            let frames: Frames = [("a".to_string(), (a.clone(), None)), ("b".to_string(), (b.clone(), None))].into_iter().collect();
            seen[2].push(mean_of(&compositor.render(&frames, PixelFormat::Bgra).unwrap()));
        }
    };
    for i in 0..60usize {
        match i {
            10 | 40 => {
                click(&mut plugin, &mut host);
                // Blur now: the shown stream takes the new step and hides its last frame, then the one behind hides its own
                let step = StagedStep { owner: "privacy_blur".into(), stage: FrameStage::Privacy, step: plugin.frame_step().unwrap() };
                shown.set_steps(vec![step]);
                if let Some(out) = shown.hide_again(last[0].clone().unwrap(), &Ctx) {
                    last[0] = Some(out);
                    note(&last, &mut seen, 0);
                }
                if let Some(out) = behind.hide_again(last[1].clone().unwrap(), &Ctx) {
                    last[1] = Some(out);
                    note(&last, &mut seen, 1);
                }
            }
            25 => {
                click(&mut plugin, &mut host);
                let step = StagedStep { owner: "privacy_blur".into(), stage: FrameStage::Privacy, step: plugin.frame_step().unwrap() };
                shown.set_steps(vec![step]);
            }
            _ => {}
        }
        for (which, pipeline) in [&mut shown, &mut behind].into_iter().enumerate() {
            last[which] = Some(pipeline.process(picture(i), &Ctx));
            note(&last, &mut seen, which);
            // the blur really ran while Blur now was on, and not while it was off
            let blurred = last[which].as_ref().unwrap().data != picture(i).data;
            assert_eq!(blurred, (10..25).contains(&i) || i >= 40, "frame {i} of output {which}");
        }
    }
    let mut worst = 0.0f64;
    for (name, means) in ["shown", "behind", "composite"].iter().zip(&seen) {
        assert!(means.len() >= 60, "{name} sent frames");
        let jump = means.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
        let dip = means.iter().cloned().fold(f64::MAX, f64::min);
        eprintln!("{name}: largest frame-to-frame change {jump:.2}, darkest frame {dip:.2}");
        assert!(dip > 60.0, "{name} went dark: {dip}");
        worst = worst.max(jump);
    }
    eprintln!("largest jump on any output: {worst:.2}");
    assert!(worst < 10.0, "a frame stood out in brightness: {worst}");
}
