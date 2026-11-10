//! Timings by hand, through Pipeline::process with the app's allocator: cargo test --release -p telescope-media --test pipeline_cost -- --ignored --nocapture

use std::sync::Arc;
use telescope_core::colour::{bake, lattice};
use telescope_core::colour_cpu::Graded;
use telescope_core::frame::{Frame, FrameContext, FrameStage, FrameStep, PixelFormat, ShaderStep};
use telescope_core::host::{ColourStage, View};
use telescope_core::media::{FramePipeline, StagedStep};
use telescope_core::view_step;
use telescope_media::gpu::Gpu;
use telescope_media::pipeline::Pipeline;

struct NoContext;

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

fn noise(w: u32, h: u32, format: PixelFormat) -> Frame {
    let mut x = 9u32;
    Frame::new(w, h, format, (0..format.size(w, h)).map(|_| {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (x >> 24) as u8
    }).collect())
}

/// The best and the median over rounds of 8 frames, in ms a frame; `shared`: the caller still holds the frame handed in, as a preview might.
fn cost(steps: impl Fn() -> Vec<StagedStep>, source: &Frame, shared: bool) -> (f64, f64) {
    let mut p = Pipeline::new();
    p.set_steps(steps());
    let held = source.clone();
    let mut rounds = Vec::new();
    for round in 0..14 {
        let frames: Vec<Frame> = (0..8).map(|_| if shared { held.clone() } else { Frame::new(source.width, source.height, source.format, source.data.to_vec()) }).collect();
        let t = std::time::Instant::now();
        for frame in frames {
            std::hint::black_box(p.process(frame, &NoContext));
        }
        // The first rounds warm the buffers and the card
        if round >= 5 {
            rounds.push(t.elapsed().as_secs_f64() * 1000.0 / 8.0);
        }
    }
    rounds.sort_by(f64::total_cmp);
    (rounds[0], rounds[rounds.len() / 2])
}

fn view_steps(view: View) -> impl Fn() -> Vec<StagedStep> {
    move || view_step::steps(&view).into_iter().map(|step| StagedStep { owner: "view".into(), stage: FrameStage::View, step }).collect()
}

fn grade_steps() -> Vec<StagedStep> {
    let mut rgb = lattice(33);
    for px in rgb.as_chunks_mut::<3>().0 {
        *px = [px[0].sqrt(), 0.3 * px[1] + 0.7 * px[2], 1.0 - px[0] * px[2]];
    }
    let cube = bake(&[&ColourStage::new(33, Arc::new(rgb), None)], None).unwrap();
    vec![StagedStep { owner: "colour".into(), stage: FrameStage::Colour, step: FrameStep::Cpu(Box::new(Graded::new(&cube))) }]
}

#[test]
#[ignore]
fn cpu_steps_cost() {
    for (w, h) in [(1920, 1080), (3840, 2160)] {
        for format in [PixelFormat::I420, PixelFormat::Nv12] {
            let source = noise(w, h, format);
            let cases: [(&str, Box<dyn Fn() -> Vec<StagedStep>>); 3] = [
                ("zoom 2x", Box::new(view_steps(View { zoom: 2.0, pan_x: 0.3, ..View::default() }))),
                ("zoom and stretch", Box::new(view_steps(View { zoom: 1.7, stretch_y: 1.1, flip_h: true, ..View::default() }))),
                ("cpu grade", Box::new(grade_steps)),
            ];
            for (name, steps) in &cases {
                for shared in [true, false] {
                    let (best, median) = cost(steps, &source, shared);
                    println!("{w}x{h} {format:?} {name} {}: best {best:.2} ms, median {median:.2} ms a frame", if shared { "input held elsewhere" } else { "input held only here" });
                }
            }
        }
    }
}

const IDENTITY: &str = "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }";

/// A shader step round trip, one case a test so the stage times (the latest 512) are only this case's.
fn card_case(w: u32, h: u32, format: PixelFormat) {
    let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
    let source = noise(w, h, format);
    let steps = || vec![StagedStep { owner: "fx".into(), stage: FrameStage::Effects,
                                     step: FrameStep::Shader(ShaderStep { wgsl: IDENTITY.into(), uniforms: serde_json::json!({}), textures: vec![] }) }];
    // Only the pipeline holds the frame, as it does when the decoder hands it over
    let (best, median) = cost(steps, &source, false);
    let stages: Vec<String> = telescope_core::perf::stage_summaries().iter().filter(|s| ["upload", "submit", "gpu_wait", "readback"].contains(&s.stage))
        .map(|s| format!("{} {:.2}", s.stage, s.p50_ms)).collect();
    println!("{w}x{h} {format:?} shader step on {}: best {best:.2} ms, median {median:.2} ms a frame (p50 ms: {})", gpu.adapter_name, stages.join(", "));
}

#[test]
#[ignore]
fn card_round_trip_1080p_i420() {
    card_case(1920, 1080, PixelFormat::I420);
}

#[test]
#[ignore]
fn card_round_trip_1080p_nv12() {
    card_case(1920, 1080, PixelFormat::Nv12);
}

#[test]
#[ignore]
fn card_round_trip_4k_i420() {
    card_case(3840, 2160, PixelFormat::I420);
}

#[test]
#[ignore]
fn card_round_trip_4k_nv12() {
    card_case(3840, 2160, PixelFormat::Nv12);
}

/// What the copy the rollback frame forces costs a step that changes the pixels in place (Frame::data_mut on shared pixels).
#[test]
#[ignore]
fn forced_copy_cost() {
    for (w, h) in [(1920, 1080), (3840, 2160)] {
        let held = noise(w, h, PixelFormat::I420);
        let mut rounds: Vec<f64> = (0..12).map(|_| {
            let t = std::time::Instant::now();
            for _ in 0..8 {
                let mut f = held.clone();
                f.data_mut()[0] = 1;
                std::hint::black_box(&f);
            }
            t.elapsed().as_secs_f64() * 1000.0 / 8.0
        }).collect();
        rounds.sort_by(f64::total_cmp);
        println!("{w}x{h} I420 forced copy: best {:.2} ms, median {:.2} ms a frame", rounds[0], rounds[6]);
    }
}
