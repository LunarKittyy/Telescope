//! Renders Background on a short moving sequence with the real person model, as the vision service feeds it (the fresh mask, then
//! the blend with the last one), through the real frame step in both frame formats and with both backdrops, once without the mask
//! firming and once with it, to judge by eye. Does nothing unless TELESCOPE_EDGE_FRAMES (a folder of numbered 1280x720 .rgb files)
//! and TELESCOPE_EDGE_OUT (where the .ppm files go) are set.

use serde_json::json;
use std::sync::Arc;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStep, PixelFormat};
use telescope_core::plugin::Plugin;
use telescope_effects::background::BackgroundPlugin;
use telescope_effects::cpu::Rgb;
use telescope_vision::ops::{self, Rgb as VRgb};
use telescope_vision::service::Backend;

struct Served(Vec<u8>, usize, usize);

impl FrameContext for Served {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        Some(Arc::new(ops::mask_resized(&self.0, self.1, self.2, frame.width as usize, frame.height as usize)))
    }
    fn person_mask_at(&self, _: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        Some(Arc::new(ops::mask_resized(&self.0, self.1, self.2, w, h)))
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

fn ppm(path: &std::path::Path, rgb: &Rgb) {
    let mut out = format!("P6\n{} {}\n255\n", rgb.w, rgb.h).into_bytes();
    out.extend_from_slice(&rgb.data);
    std::fs::write(path, out).unwrap();
}

fn frame_of(rgb: &Rgb, format: PixelFormat) -> Frame {
    let mut frame = Frame::new(rgb.w as u32, rgb.h as u32, format, vec![0; format.size(rgb.w as u32, rgb.h as u32)]);
    rgb.write_to(&mut frame);
    frame
}

#[test]
fn render_a_sequence_through_background() {
    let (Some(frames), Some(out)) = (std::env::var_os("TELESCOPE_EDGE_FRAMES"), std::env::var_os("TELESCOPE_EDGE_OUT")) else { return };
    let (dir, info) = (telescope_vision::pack::default_dir(), telescope_vision::pack::PackInfo::current());
    assert!(info.installed(&dir), "no vision pack");
    let backend = telescope_vision::mediapipe::MediaPipe::load(&info.folder(&dir)).unwrap_or_else(|e| panic!("{e:?}"));
    let out = std::path::PathBuf::from(out);
    let mut names: Vec<_> = std::fs::read_dir(frames).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "rgb")).collect();
    names.sort();
    let (w, h) = (1280, 720);
    let picture = Rgb { w, h, data: (0..w * h).flat_map(|i| { let x = (i % w) as u8; [200u8, 60u8.saturating_add(x / 4), 240u8.saturating_sub(x / 3)] }).collect() };
    for (label, firm) in [("before", false), ("after", true)] {
        let mut segment = backend.segmenter().unwrap();
        let mut last: Option<Vec<u8>> = None;
        let mut steps: Vec<(String, PixelFormat, Box<dyn CpuStep>)> = Vec::new();
        for mode in ["blur", "picture"] {
            for (fname, format) in [("rgb", PixelFormat::Rgba), ("i420", PixelFormat::I420)] {
                let reader_picture = picture.clone();
                let reader: telescope_effects::shared::PictureReader = Arc::new(move |_| Ok(frame_of(&reader_picture, PixelFormat::Rgba)));
                let mut plugin = BackgroundPlugin::with_reader(reader);
                plugin.set_config(&json!({"mode": mode, "blur": 6, "edges": 3, "picture": "p"})).unwrap();
                let Some(FrameStep::Cpu(step)) = plugin.frame_step() else { panic!("no cpu step") };
                steps.push((format!("{mode}-{fname}"), format, step));
            }
        }
        for (i, path) in names.iter().enumerate() {
            let data = std::fs::read(path).unwrap();
            let rgb = Rgb { w, h, data: data.clone() };
            let small = ops::model_input(&VRgb::new(w, h, data), ops::MODEL_SIZE).unwrap();
            let mut mask = segment.segment(&small, i as i64 + 1).unwrap();
            if firm {
                mask.data = ops::firm_mask(&mask.data, mask.w, mask.h);
            }
            if let Some(prev) = &last {
                mask.data = ops::add_weighted(&mask.data, 0.7, prev, 0.3);
            }
            last = Some(mask.data.clone());
            let ctx = Served(mask.data.clone(), mask.w, mask.h);
            for (name, format, step) in steps.iter_mut() {
                let mut frame = frame_of(&rgb, *format);
                step.process(&mut frame, &ctx);
                ppm(&out.join(format!("seq-{label}-{name}-{i:02}.ppm")), &Rgb::from_frame(&frame).unwrap());
            }
        }
    }
}
