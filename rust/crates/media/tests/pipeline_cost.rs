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
                                     step: FrameStep::Shader(ShaderStep { keeps_geometry: false, wgsl: IDENTITY.into(), uniforms: serde_json::json!({}), textures: vec![] }) }];
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

/// Draws a few thousand pixels in place, as the name tag does.
struct Tag;

impl telescope_core::frame::CpuStep for Tag {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        let w = frame.width as usize;
        for row in 0..40 {
            frame.data_mut()[row * w..row * w + 200].fill(235);
        }
    }
}

/// A stream's CPU steps in a row (view, stretch, a grade, a tag), the work that runs one after another on the stream's thread, with the input held only here.
#[test]
#[ignore]
fn stream_steps_cost() {
    for (w, h) in [(1920, 1080), (3840, 2160)] {
        let source = noise(w, h, PixelFormat::I420);
        let steps = || {
            let mut all = view_steps(View { zoom: 1.3, flip_h: true, ..View::default() })();
            all.extend(view_steps(View { stretch_y: 1.1, ..View::default() })());
            all.extend(grade_steps());
            all.push(StagedStep { owner: "tag".into(), stage: FrameStage::Overlay, step: FrameStep::Cpu(Box::new(Tag)) });
            all
        };
        let (best, median) = cost(steps, &source, false);
        println!("{w}x{h} I420 stream steps (view, stretch, grade, tag): best {best:.2} ms, median {median:.2} ms a frame");
    }
}

/// Stands in for the vision service's masks: the model's 256x144 mask changes every other frame (about 15 a second at 30), and a size asked for is made from it once per change.
struct ServiceMasks {
    frame: std::sync::atomic::AtomicU64,
    made: std::sync::Mutex<Vec<((usize, usize), u64, Arc<Vec<u8>>)>>,
}

impl ServiceMasks {
    fn at(&self, w: usize, h: usize) -> Arc<Vec<u8>> {
        let change = self.frame.load(std::sync::atomic::Ordering::SeqCst) / 2;
        let mut made = self.made.lock().unwrap();
        if let Some((_, _, mask)) = made.iter().find(|(size, at, _)| *size == (w, h) && *at == change) {
            return mask.clone();
        }
        let base: Vec<u8> = (0..256 * 144).map(|i| if (i % 256) as u64 + change % 7 < 128 { 255 } else { 0 }).collect();
        let mask = Arc::new(telescope_core::image::resize_linear_fixed(&base, 256, 144, 1, w, h));
        made.retain(|(_, at, _)| *at == change);
        made.push(((w, h), change, mask.clone()));
        mask
    }
}

impl FrameContext for ServiceMasks {
    fn source_id(&self) -> &str {
        ""
    }
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        Some(self.at(frame.width as usize, frame.height as usize))
    }
    fn person_mask_at(&self, _frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        Some(self.at(w, h))
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// A shader that reads the person mask (skin smoothing's way) on a 4K frame, the mask changing every other frame: ms a frame and the share of the process's CPU, the best round of 8.
#[test]
#[ignore]
fn mask_shader_cost_4k() {
    let Some(gpu) = Gpu::shared() else { return eprintln!("no graphics card; skipped") };
    let source = noise(3840, 2160, PixelFormat::I420);
    let wgsl = "fn effect(uv: vec2<f32>) -> vec4<f32> { let c = sample(uv); return vec4<f32>(mix(c.rgb, vec3<f32>(person_mask(uv)), 0.2), 1.0); }";
    let step = StagedStep { owner: "fx".into(), stage: FrameStage::Effects, step: FrameStep::Shader(ShaderStep { keeps_geometry: false, wgsl: wgsl.into(), uniforms: serde_json::json!({}), textures: vec![] }) };
    let ctx = ServiceMasks { frame: Default::default(), made: Default::default() };
    let mut p = Pipeline::new();
    p.set_steps(vec![step]);
    let cpu = || {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let fields: Vec<&str> = stat.rsplit_once(')').map_or("", |(_, rest)| rest).split_whitespace().collect();
        let ticks = |i: usize| fields.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        (ticks(11) + ticks(12)) / 100.0 * 1000.0
    };
    let mut rounds = Vec::new();
    let mut uploads = 0;
    for round in 0..16 {
        let frames: Vec<Frame> = (0..8).map(|_| Frame::new(source.width, source.height, source.format, source.data.to_vec())).collect();
        let (t, c, up) = (std::time::Instant::now(), cpu(), telescope_media::transfers::totals().mask_up);
        for frame in frames {
            ctx.frame.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::hint::black_box(p.process(frame, &ctx));
        }
        if round >= 6 {
            rounds.push((t.elapsed().as_secs_f64() * 1000.0 / 8.0, (cpu() - c) / 8.0));
            uploads += telescope_media::transfers::totals().mask_up - up;
        }
    }
    rounds.sort_by(|a, b| a.0.total_cmp(&b.0));
    println!("4K I420 mask shader on {}: best {:.2} ms, median {:.2} ms a frame, process cpu {:.2} ms a frame (median round), mask uploads {uploads} in {} frames", gpu.adapter_name, rounds[0].0, rounds[rounds.len() / 2].0, rounds[rounds.len() / 2].1, rounds.len() * 8);
}

/// A soft-edged person-shaped mask at the model's size (144x256 for a portrait frame), made to any size the way the service does.
struct Blob;

impl Blob {
    fn at(w: usize, h: usize) -> Arc<Vec<u8>> {
        let base: Vec<u8> = (0..144 * 256).map(|i| {
            let (x, y) = ((i % 144) as f64 / 144.0 - 0.5, (i / 144) as f64 / 256.0 - 0.55);
            let d = (x * x / 0.06 + y * y / 0.2).sqrt();
            ((1.3 - d * 1.6).clamp(0.0, 1.0) * 255.0) as u8
        }).collect();
        Arc::new(telescope_core::image::resize_linear_fixed(&base, 144, 256, 1, w, h))
    }
}

impl FrameContext for Blob {
    fn source_id(&self) -> &str {
        ""
    }
    fn person_mask(&self, frame: &Frame) -> Option<Arc<Vec<u8>>> {
        Some(Blob::at(frame.width as usize, frame.height as usize))
    }
    fn person_mask_at(&self, _frame: &Frame, w: usize, h: usize) -> Option<Arc<Vec<u8>>> {
        Some(Blob::at(w, h))
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// The picture a mask shader makes of a real frame, written out for comparing builds: IN is a raw I420 file of 2160x3840, OUT where the result goes.
#[test]
#[ignore]
fn mask_shader_picture() {
    let (Ok(input), Ok(out)) = (std::env::var("IN"), std::env::var("OUT")) else { return eprintln!("IN and OUT not set") };
    if Gpu::shared().is_none() {
        return eprintln!("no graphics card; skipped");
    }
    let frame = Frame::new(2160, 3840, PixelFormat::I420, std::fs::read(input).unwrap());
    let wgsl = "fn effect(uv: vec2<f32>) -> vec4<f32> { let c = sample(uv); return vec4<f32>(mix(c.rgb, vec3<f32>(person_mask(uv)), 0.5), 1.0); }";
    let step = StagedStep { owner: "fx".into(), stage: FrameStage::Effects, step: FrameStep::Shader(ShaderStep { keeps_geometry: false, wgsl: wgsl.into(), uniforms: serde_json::json!({}), textures: vec![] }) };
    let mut p = Pipeline::new();
    p.set_steps(vec![step]);
    let done = p.process(frame, &Blob);
    std::fs::write(out, &done.data[..]).unwrap();
}
