use super::*;
use std::sync::Arc;
use telescope_core::frame::PixelFormat;
use telescope_core::media::{Media, StagedStep};
use telescope_core::testing::FakeHost;
use telescope_core::vision::{VisionPack, MISSING};

struct Ctx(Option<Arc<Vec<u8>>>);

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        "cam"
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        self.0.clone()
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

fn rgba(w: u32, h: u32, mut f: impl FnMut(u32, u32) -> [u8; 3]) -> Frame {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let c = f(x, y);
            data.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    Frame::new(w, h, PixelFormat::Rgba, data)
}

fn value(f: &Frame, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * f.width + x) * 4) as usize;
    [f.data[i], f.data[i + 1], f.data[i + 2]]
}

fn grey(f: &Frame, x: u32, y: u32) -> f32 {
    let c = value(f, x, y);
    0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32
}

/// A deterministic noise, about N(0, 1)
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        let mut sum = 0.0;
        for _ in 0..6 {
            self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
            sum += (self.0 >> 8) as f32 / 16777216.0;
        }
        (sum - 3.0) * 1.414
    }
}

fn noisy(w: u32, h: u32, level: u8, sigma: f32, seed: u32) -> Frame {
    let mut n = Noise(seed);
    rgba(w, h, |_, _| {
        let v = (level as f32 + sigma * n.next()).round().clamp(0.0, 255.0) as u8;
        [v, v, v]
    })
}

fn with(settings: &[(usize, i64)]) -> Settings {
    let mut s = Settings::default();
    for (i, v) in settings {
        s.values[*i] = *v;
    }
    s
}

/// The shader and the CPU twin of the only step the settings make.
fn only_step(s: &Settings) -> (FrameStage, ShaderStep, Box<dyn CpuStep>) {
    let mut steps = s.steps();
    assert_eq!(steps.len(), 1, "one step");
    let (stage, step) = steps.remove(0);
    match step {
        FrameStep::Either(shader, cpu) => (stage, shader, cpu),
        other => panic!("a step that can go either way, got {other:?}"),
    }
}

fn on_cpu(cpu: &mut Box<dyn CpuStep>, frame: &Frame, ctx: &Ctx) -> Frame {
    let mut out = frame.clone();
    cpu.process(&mut out, ctx);
    out
}

fn gpu_here() -> bool {
    let here = telescope_media::gpu::Gpu::shared().is_some();
    if !here {
        eprintln!("no graphics card here: the shader checks are skipped");
    }
    here
}

/// The app's own frame pipeline with one shader step in it, which keeps what it needs between frames.
struct OnCard(Box<dyn telescope_core::media::FramePipeline>);

impl OnCard {
    fn new(stage: FrameStage, shader: ShaderStep) -> OnCard {
        let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
        pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage, step: FrameStep::Shader(shader) }]);
        OnCard(pipeline)
    }

    fn run(&mut self, frame: &Frame, ctx: &Ctx) -> Frame {
        self.0.process(frame.clone(), ctx)
    }
}

/// How far two pictures are apart: the mean and the largest difference of any colour of any pixel.
fn apart(a: &Frame, b: &Frame) -> (f32, i32) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let diffs: Vec<i32> = a.data.iter().zip(b.data.iter()).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| (*x as i32 - *y as i32).abs()).collect();
    (diffs.iter().sum::<i32>() as f32 / diffs.len() as f32, *diffs.iter().max().unwrap())
}

/// A picture with a smooth gradient, hard edges, a skin-coloured disc and a little noise, for comparing the card with the CPU.
fn scene(w: u32, h: u32) -> Frame {
    let mut n = Noise(7);
    rgba(w, h, |x, y| {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let mut c = [60.0 + 120.0 * fx, 80.0 + 90.0 * fy, 140.0 - 60.0 * fx];
        if (0.15..0.4).contains(&fx) && (0.2..0.5).contains(&fy) {
            c = [235.0, 230.0, 220.0];
        }
        if (0.55..0.9).contains(&fx) && (0.55..0.85).contains(&fy) {
            c = [30.0, 40.0, 60.0];
        }
        let (dx, dy) = (fx - 0.62, fy - 0.35);
        if dx * dx * 4.0 + dy * dy * 9.0 < 0.06 {
            c = [224.0, 172.0, 140.0];
        }
        let v = 3.0 * n.next();
        [(c[0] + v).clamp(0.0, 255.0) as u8, (c[1] + v).clamp(0.0, 255.0) as u8, (c[2] + v).clamp(0.0, 255.0) as u8]
    })
}

fn scene_mask(w: u32, h: u32) -> Arc<Vec<u8>> {
    let mut mask = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let (dx, dy) = (fx - 0.62, fy - 0.35);
            mask[(y * w + x) as usize] = if dx * dx * 4.0 + dy * dy * 9.0 < 0.1 { 255 } else { 0 };
        }
    }
    Arc::new(mask)
}

// ── Neutral settings ──────────────────────────────────────────────

#[test]
fn nothing_at_its_neutral_value_makes_a_step() {
    assert!(Settings::default().steps().is_empty());
    // a radius or a size alone is nothing to apply
    assert!(with(&[(RADIUS, 25), (THRESHOLD, 80), (VIGNETTE_SIZE, 10), (VIGNETTE_ROUND, 0), (GLOW_SIZE, 100), (GRAIN_SIZE, 100)]).steps().is_empty());
    let mut p = FiltersPlugin::new();
    assert!(p.staged_frame_steps().is_empty() && p.frame_step().is_none());
    // and off means nothing even with sliders up
    p.set_config(&json!({"on": false, "sharpen": 50, "grain": 40})).unwrap();
    assert!(p.staged_frame_steps().is_empty());
}

#[test]
fn neutral_settings_give_a_bit_identical_picture_on_both_ways() {
    let frame = scene(96, 54);
    let ctx = Ctx(None);
    // the filters' own functions at their neutral values leave every byte as it was
    let mut img = Rgb::from_frame(&frame).unwrap();
    let before = img.clone();
    cpu::sharpen(&mut img, &cpu::Sharpen { amount: 0.0, sigma: 1.0, gate: 0.0 });
    cpu::clarity(&mut img, &cpu::Clarity { amount: 0.0 });
    cpu::dehaze(&mut img, &cpu::Dehaze { strength: 0.0 });
    cpu::denoise_space(&mut img, &cpu::DenoiseSpace { range: 0.0 });
    cpu::finish(&mut img, &cpu::Finish { vignette: 0.0, vignette_inner: 0.5, vignette_round: 0.5, glow: 0.0, glow_radius: 50.0, grain: 0.0, grain_size: 1.0 }, 3);
    assert_eq!(img, before);
    // the plugin with every slider at neutral hands the pipeline nothing, so the frame goes through as it is
    let mut p = FiltersPlugin::new();
    p.set_config(&json!({})).unwrap();
    let steps: Vec<StagedStep> = p.staged_frame_steps().into_iter().map(|(stage, step)| StagedStep { owner: NAME.into(), stage, step }).collect();
    let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
    pipeline.set_steps(steps);
    assert!(pipeline.is_empty());
    assert_eq!(pipeline.process(frame.clone(), &ctx).data, frame.data);
}

#[test]
fn the_steps_go_in_the_order_and_the_stages_the_look_needs() {
    let all = with(&[(OVER_TIME, 50), (DENOISE, 50), (DEHAZE_AT, 50), (SKIN_AT, 50), (SHARPEN_AT, 50), (CLARITY_AT, 50), (VIGNETTE, 50), (GLOW, 50), (GRAIN, 50)]);
    let stages: Vec<FrameStage> = all.steps().iter().map(|(s, _)| *s).collect();
    // clean-up and detail before the grade (the effects stage), the finish after the grade's own step (the colour stage)
    assert_eq!(stages, [FrameStage::Effects, FrameStage::Effects, FrameStage::Effects, FrameStage::Effects, FrameStage::Effects, FrameStage::Effects, FrameStage::Colour]);
    assert!(FrameStage::Effects < FrameStage::Colour);
    // the finish is one step however many of its three are on
    assert_eq!(with(&[(GRAIN, 10)]).steps().len(), 1);
    assert_eq!(with(&[(VIGNETTE, -30), (GLOW, 10), (GRAIN, 10)]).steps().len(), 1);
}

// ── Sharpen ───────────────────────────────────────────────────────

/// A soft edge between two greys, blurred the way a lens blurs one (a logistic curve `width` wide), the same on every row. 360 rows tall,
/// since the filters' sizes are set for 1080 rows and shrink with the picture.
fn soft_edge(from: f32, to: f32, width: f32) -> Frame {
    rgba(128, 360, |x, _| {
        let t = 1.0 / (1.0 + (-(x as f32 - 64.0) / width).exp());
        let v = (from + (to - from) * t).round() as u8;
        [v, v, v]
    })
}

fn steepest(f: &Frame) -> f32 {
    (0..f.width - 1).map(|x| (grey(f, x + 1, 12) - grey(f, x, 12)).abs()).fold(0.0, f32::max)
}

fn range_of(f: &Frame) -> (f32, f32) {
    let all: Vec<f32> = (0..f.width).map(|x| grey(f, x, 12)).collect();
    (all.iter().cloned().fold(f32::MAX, f32::min), all.iter().cloned().fold(f32::MIN, f32::max))
}

fn checks_a_sharpened_edge(before: &Frame, after: &Frame) {
    assert!(steepest(after) > steepest(before) * 1.1, "{} against {}", steepest(after), steepest(before));
    // never past the plateaus by more than the halo limit (0.03 of the range), and a level for rounding
    let limit = cpu::HALO * 255.0 + 1.5;
    let ((lo, hi), (lo2, hi2)) = (range_of(before), range_of(after));
    assert!(hi2 <= hi + limit && lo2 >= lo - limit, "{lo2}..{hi2} against {lo}..{hi}");
    // and the far side of each plateau is as it was
    assert_eq!(value(after, 2, 12), value(before, 2, 12));
    assert_eq!(value(after, 125, 12), value(before, 125, 12));
}

#[test]
fn sharpen_makes_an_edge_steeper_without_overshooting_past_the_limit() {
    let before = soft_edge(80.0, 170.0, 1.5);
    let (stage, shader, mut cpu_step) = only_step(&with(&[(SHARPEN_AT, 100), (RADIUS, 30), (THRESHOLD, 0)]));
    assert_eq!(stage, FrameStage::Effects);
    checks_a_sharpened_edge(&before, &on_cpu(&mut cpu_step, &before, &Ctx(None)));
    if gpu_here() {
        checks_a_sharpened_edge(&before, &OnCard::new(stage, shader).run(&before, &Ctx(None)));
    }
}

#[test]
fn the_threshold_leaves_a_flat_noisy_area_alone_and_still_sharpens_an_edge() {
    // noise of a level or so on a flat grey: its detail is under the threshold, so every byte stays
    let flat = noisy(128, 360, 128, 1.0, 11);
    let (_, shader, mut cpu_step) = only_step(&with(&[(SHARPEN_AT, 100), (RADIUS, 30), (THRESHOLD, 20)]));
    assert_eq!(on_cpu(&mut cpu_step, &flat, &Ctx(None)).data, flat.data);
    if gpu_here() {
        let (mean, most) = apart(&OnCard::new(FrameStage::Effects, shader.clone()).run(&flat, &Ctx(None)), &flat);
        assert!(mean < 0.1 && most <= 1, "{mean} {most}");
    }
    // with no threshold the same noise is sharpened (that's what the threshold is for)
    let (_, _, mut open) = only_step(&with(&[(SHARPEN_AT, 100), (RADIUS, 30), (THRESHOLD, 0)]));
    assert_ne!(on_cpu(&mut open, &flat, &Ctx(None)).data, flat.data);
    // and a real edge is still sharpened with the threshold up
    let edge = soft_edge(30.0, 230.0, 1.0);
    assert!(steepest(&on_cpu(&mut cpu_step, &edge, &Ctx(None))) > steepest(&edge) * 1.05);
}

// ── Denoise over time ─────────────────────────────────────────────

fn spread(f: &Frame, area: (u32, u32, u32, u32)) -> f32 {
    let mut vals = Vec::new();
    for y in area.1..area.3 {
        for x in area.0..area.2 {
            vals.push(grey(f, x, y));
        }
    }
    let mean = vals.iter().sum::<f32>() / vals.len() as f32;
    (vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / vals.len() as f32).sqrt()
}

fn timed(mut f: Frame, n: u64) -> Frame {
    f.number = n;
    f.time_ns = n * 33_000_000;
    f
}

#[test]
fn denoising_over_time_lowers_the_noise_of_a_still_picture() {
    let (_, shader, mut cpu_step) = only_step(&with(&[(OVER_TIME, 70)]));
    let mut card = gpu_here().then(|| OnCard::new(FrameStage::Effects, shader));
    let (mut last_cpu, mut last_gpu, mut raw) = (None, None, None);
    for n in 0..16 {
        // a still picture with a different noise on every frame
        let frame = timed(noisy(64, 48, 120, 9.0, 100 + n as u32), n);
        last_cpu = Some(on_cpu(&mut cpu_step, &frame, &Ctx(None)));
        if let Some(card) = &mut card {
            last_gpu = Some(card.run(&frame, &Ctx(None)));
        }
        raw = Some(frame);
    }
    let area = (8, 8, 56, 40);
    let noise = spread(raw.as_ref().unwrap(), area);
    let cleaned = spread(last_cpu.as_ref().unwrap(), area);
    assert!(cleaned < noise * 0.6, "CPU: {cleaned} against {noise}");
    if let Some(out) = last_gpu {
        let cleaned = spread(&out, area);
        assert!(cleaned < noise * 0.6, "card: {cleaned} against {noise}");
    }
}

/// A grey picture with noise and a bright square whose left edge is at `x`.
fn square_at(x: u32, seed: u32) -> Frame {
    let mut n = Noise(seed);
    rgba(96, 48, |px, py| {
        let v = if (x..x + 16).contains(&px) && (16..32).contains(&py) { 210.0 } else { 90.0 };
        let v = (v + 4.0 * n.next()).round().clamp(0.0, 255.0) as u8;
        [v, v, v]
    })
}

fn checks_no_ghost(out: &Frame, now_at: u32, was_at: u32) {
    // where the square was a few frames ago and isn't now is the background again, not a trail of it
    let mut trail = Vec::new();
    for y in 18..30 {
        for x in was_at..was_at + 8 {
            if x + 2 < now_at || x >= now_at + 16 {
                trail.push(grey(out, x, y));
            }
        }
    }
    let mean = trail.iter().sum::<f32>() / trail.len() as f32;
    assert!((mean - 90.0).abs() < 8.0, "a ghost of the square: {mean} where it should be 90");
    // and the square itself is as bright as it was
    let inside: f32 = (20..28).flat_map(|y| (now_at + 3..now_at + 13).map(move |x| (x, y))).map(|(x, y)| grey(out, x, y)).sum::<f32>() / 80.0;
    assert!((inside - 210.0).abs() < 12.0, "the square: {inside}");
}

#[test]
fn denoising_over_time_leaves_no_ghost_behind_a_moving_square() {
    let (_, shader, mut cpu_step) = only_step(&with(&[(OVER_TIME, 90)]));
    let mut card = gpu_here().then(|| OnCard::new(FrameStage::Effects, shader));
    let (mut cpu_out, mut gpu_out) = (None, None);
    // the square steps 8 pixels a frame, after a few frames of the average settling on the empty picture
    for n in 0..20u64 {
        let x = if n < 6 { 4 } else { 4 + (n as u32 - 6) * 5 };
        let frame = timed(square_at(x, 500 + n as u32), n);
        cpu_out = Some(on_cpu(&mut cpu_step, &frame, &Ctx(None)));
        if let Some(card) = &mut card {
            gpu_out = Some(card.run(&frame, &Ctx(None)));
        }
    }
    let now_at = 4 + 13 * 5;
    checks_no_ghost(&cpu_out.unwrap(), now_at, 4 + 8 * 5);
    if let Some(out) = gpu_out {
        checks_no_ghost(&out, now_at, 4 + 8 * 5);
    }
}

#[test]
fn denoising_over_time_starts_again_when_the_source_the_size_or_the_time_changes() {
    let p = cpu::DenoiseTime { tolerance: 0.06, keep: 0.9 };
    let mut memory = cpu::Memory::default();
    let step = |memory: &mut cpu::Memory, level: u8, size: (usize, usize), source: &str, time: u64| {
        let mut img = Rgb::filled(size.0, size.1, [level; 3]);
        cpu::denoise_time(&mut img, memory, &p, source, time);
        img.at(0, 0)[0]
    };
    // the same source holds on to what it saw: a small change is mostly kept out
    assert_eq!(step(&mut memory, 100, (8, 8), "a", 0), 100);
    assert_eq!(step(&mut memory, 108, (8, 8), "a", 33_000_000), 108 - 7);
    // another source, another size, or too long since the last picture: the picture comes out as it came in
    assert_eq!(step(&mut memory, 100, (8, 8), "b", 66_000_000), 100);
    assert_eq!(step(&mut memory, 108, (8, 8), "b", 99_000_000), 108 - 7);
    assert_eq!(step(&mut memory, 108, (16, 8), "b", 132_000_000), 108);
    assert_eq!(step(&mut memory, 100, (16, 8), "b", 165_000_000), 100 + 7);
    assert_eq!(step(&mut memory, 108, (16, 8), "b", 165_000_000 + cpu::Memory::MAX_AGE_NS + 1), 108);
    // and a frame from the past (a restarted clock) never joins the picture that came after
    assert_eq!(step(&mut memory, 100, (16, 8), "b", 1), 100);
}

// ── The card and the CPU make the same picture ────────────────────

fn alike(name: &str, s: &Settings, mean_most: f32, most_most: i32) {
    alike_at((640, 360), name, s, mean_most, most_most);
}

fn alike_at((w, h): (u32, u32), name: &str, s: &Settings, mean_most: f32, most_most: i32) {
    let frame = scene(w, h);
    let mask = scene_mask(w, h);
    let ctx = Ctx(Some(mask));
    let (stage, shader, mut cpu_step) = only_step(s);
    let by_cpu = on_cpu(&mut cpu_step, &timed(frame.clone(), 5), &ctx);
    let by_card = OnCard::new(stage, shader).run(&timed(frame.clone(), 5), &ctx);
    let (mean, most) = apart(&by_cpu, &by_card);
    let (changed, _) = apart(&by_cpu, &frame);
    eprintln!("{name}: card and CPU differ by {mean:.2} on average and {most} at most; the filter itself changes it by {changed:.2}");
    assert!(changed > mean * 2.0 || mean < 0.05, "{name} hardly changes the picture ({changed}) against how far apart they are ({mean})");
    assert!(mean <= mean_most && most <= most_most, "{name}: {mean} {most}");
}

#[test]
fn the_card_and_the_cpu_agree_on_each_filter() {
    if !gpu_here() {
        return;
    }
    alike("sharpen", &with(&[(SHARPEN_AT, 70), (RADIUS, 30), (THRESHOLD, 0)]), 1.5, 40);
    alike("clarity", &with(&[(CLARITY_AT, 80)]), 1.0, 60);
    alike("dehaze", &with(&[(DEHAZE_AT, 80)]), 0.6, 4);
    alike("denoise", &with(&[(DENOISE, 60)]), 0.6, 4);
    alike("skin", &with(&[(SKIN_AT, 100)]), 0.6, 6);
    alike("vignette", &with(&[(VIGNETTE, 70), (VIGNETTE_SIZE, 30), (VIGNETTE_ROUND, 40)]), 0.4, 3);
    alike("lighter corners", &with(&[(VIGNETTE, -70)]), 0.4, 3);
    alike("grain", &with(&[(GRAIN, 80), (GRAIN_SIZE, 50)]), 0.4, 3);
    alike("glow", &with(&[(GLOW, 80), (GLOW_SIZE, 30)]), 3.0, 40);
    // at 1080 rows the CPU works the blur out on a shrunk copy
    alike_at((384, 1080), "skin at 1080 rows", &with(&[(SKIN_AT, 100)]), 0.3, 40);
}

// ── The card, the settings and the plugin ─────────────────────────

fn rig() -> (FiltersPlugin, FakeHost) {
    let (mut p, mut host) = (FiltersPlugin::new(), FakeHost::new());
    p.setup(&mut host);
    p.on_event(&mut host, &Event::PhonesChanged { count: 1 });
    p.set_config(&json!({})).unwrap();
    (p, host)
}

fn shown(host: &FakeHost, key: &str) -> Value {
    host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok()).find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
}

#[test]
fn it_says_what_it_is() {
    let info = FiltersPlugin::new().info();
    assert_eq!((info.name.as_str(), info.category.as_str(), info.icon.as_str()), ("filters", "look", "sliders"));
    assert!(info.optional && info.in_presets && info.device_local && !info.required && !info.needs_vision);
    assert_eq!(info.frame_stage, FrameStage::Effects);
    assert_eq!(FiltersPlugin::new().card().unwrap().region, "right");
    assert_eq!(FiltersPlugin::new().diagnostics(), vec![("Filters".to_string(), "on, nothing set".to_string())]);
}

#[test]
fn the_settings_are_kept_clamped_and_come_back() {
    let (mut p, mut host) = rig();
    p.set_config(&json!({"sharpen": 40, "sharpen_radius": 99, "clarity": -250, "glow_size": "70", "grain": "lots", "on": true})).unwrap();
    let cfg = p.get_config();
    assert_eq!((cfg["sharpen"].clone(), cfg["sharpen_radius"].clone(), cfg["clarity"].clone(), cfg["glow_size"].clone(), cfg["grain"].clone()), (json!(40), json!(30), json!(-100), json!(70), json!(0)));
    assert_eq!(cfg["vignette_size"], json!(50), "what isn't there is its default");
    let mut q = FiltersPlugin::new();
    q.set_config(&cfg).unwrap();
    assert_eq!(q.get_config(), cfg);
    // a preset is the same settings, and the stream gets its steps again
    p.apply_preset(&mut host, &json!({"dehaze": 30})).unwrap();
    assert_eq!(p.get_config()["dehaze"], json!(30));
    assert_eq!(p.get_config()["sharpen"], json!(0));
    assert!(host.calls.iter().any(|c| c == "refresh_frame_step"));
    // an older or broken file is the defaults
    q.set_config(&json!("nonsense")).unwrap();
    assert_eq!(q.get_config()["clarity"], json!(0));
}

#[test]
fn a_slider_changes_the_steps_and_asks_for_a_save() {
    let (mut p, mut host) = rig();
    assert!(p.staged_frame_steps().is_empty());
    p.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "slider-moved".into(), args: vec![json!(SHARPEN_AT), json!(60.4)] });
    assert_eq!(p.get_config()["sharpen"], json!(60));
    assert_eq!(p.staged_frame_steps().len(), 1);
    assert!(host.calls.iter().any(|c| c == "refresh_frame_step") && host.calls.iter().any(|c| c == "schedule_save"));
    assert_eq!(shown(&host, "slider-text")[SHARPEN_AT], json!("60"));
    // the radius shows once there's an amount to take it
    assert_eq!(shown(&host, "slider-shown")[RADIUS], json!(true));
    p.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "slider-moved".into(), args: vec![json!(SHARPEN_AT), json!(0)] });
    assert_eq!(shown(&host, "slider-shown")[RADIUS], json!(false));
    assert!(p.staged_frame_steps().is_empty());
    // the switch in the header sets them aside without losing them
    p.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "slider-moved".into(), args: vec![json!(GRAIN), json!(30)] });
    p.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "switch-toggled-by-hand".into(), args: vec![json!(false)] });
    assert!(p.staged_frame_steps().is_empty() && p.get_config()["grain"] == json!(30));
    p.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "switch-toggled-by-hand".into(), args: vec![json!(true)] });
    assert_eq!(p.staged_frame_steps().len(), 1);
}

#[test]
fn the_shortcut_switches_it_and_a_card_without_a_phone_ignores_it() {
    let (mut p, mut host) = rig();
    assert_eq!(p.run_action(&mut host, "filters.on", &json!({"mode": "off"})).as_deref(), Some("Filters: off"));
    assert!(!p.is_on());
    assert_eq!(p.run_action(&mut host, "filters.on", &json!({})).as_deref(), Some("Filters: on"));
    assert_eq!(p.actions().into_iter().map(|a| a.id).collect::<Vec<_>>(), ["filters.on"]);
    let mut idle = FiltersPlugin::new();
    assert_eq!(idle.run_action(&mut host, "filters.on", &json!({})), None);
}

#[test]
fn skin_smoothing_says_why_it_cant_work_without_the_vision_pack() {
    let (mut p, mut host) = rig();
    host.vision = Some(MISSING.into());
    host.pack = VisionPack { available: true, size_mb: 47, ..VisionPack::default() };
    p.on_event(&mut host, &Event::VisionStateChanged { state: MISSING.into() });
    assert_eq!(shown(&host, "vision-text"), json!("Needs a one-time download (about 47 MB)."));
    assert_eq!(shown(&host, "vision-download"), json!(true));
    assert_eq!(shown(&host, "skin-available"), json!(false));
    p.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "download-clicked".into(), args: vec![] });
    assert!(host.calls.iter().any(|c| c == "download_vision"));
    // with the pack ready there is nothing to explain
    host.vision = Some("ready".into());
    host.pack = VisionPack { ready: true, available: true, ..VisionPack::default() };
    p.on_event(&mut host, &Event::VisionStateChanged { state: "ready".into() });
    assert_eq!((shown(&host, "vision-text"), shown(&host, "skin-available")), (json!(""), json!(true)));
}

#[test]
fn skin_smoothing_leaves_the_picture_alone_without_a_person_mask() {
    let frame = scene(96, 54);
    let (_, shader, mut cpu_step) = only_step(&with(&[(SKIN_AT, 100)]));
    assert_eq!(on_cpu(&mut cpu_step, &frame, &Ctx(None)).data, frame.data);
    if gpu_here() {
        assert_eq!(OnCard::new(FrameStage::Effects, shader).run(&frame, &Ctx(None)).data, frame.data);
    }
}

#[test]
fn skin_smoothing_softens_skin_inside_the_mask_and_keeps_an_eye_sharp() {
    let (w, h) = (96, 96);
    let mut n = Noise(3);
    // skin with pores (noise), and a dark "eye" in it
    let frame = rgba(w, h, |x, y| {
        let skin = [224.0, 172.0, 140.0];
        let eye = (44..52).contains(&x) && (44..52).contains(&y);
        let c = if eye { [50.0, 40.0, 40.0] } else { skin };
        let v = 6.0 * n.next();
        [(c[0] + v).clamp(0.0, 255.0) as u8, (c[1] + v).clamp(0.0, 255.0) as u8, (c[2] + v).clamp(0.0, 255.0) as u8]
    });
    let ctx = Ctx(Some(Arc::new(vec![255u8; (w * h) as usize])));
    let (_, _, mut cpu_step) = only_step(&with(&[(SKIN_AT, 100)]));
    let out = on_cpu(&mut cpu_step, &frame, &ctx);
    let (before, after) = (spread(&frame, (4, 4, 36, 36)), spread(&out, (4, 4, 36, 36)));
    assert!(after < before * 0.6, "skin pores: {after} against {before}");
    // the eye keeps its darkness
    assert!(grey(&out, 47, 47) < 70.0, "the eye: {}", grey(&out, 47, 47));
    // and outside the mask nothing changes
    let masked = Ctx(Some(Arc::new((0..w * h).map(|i| if i % w < 48 { 255 } else { 0 }).collect())));
    let half = on_cpu(&mut cpu_step, &frame, &masked);
    assert_eq!(value(&half, 80, 10), value(&frame, 80, 10));
    assert_ne!(value(&half, 10, 10), value(&frame, 10, 10));
}

// ── Dehaze, clarity, vignette, glow, grain ────────────────────────

#[test]
fn dehaze_adds_contrast_to_a_washed_out_picture() {
    // a dark and a light patch, both lifted by haze
    let frame = rgba(64, 32, |x, _| if x < 32 { [150, 150, 155] } else { [200, 200, 205] });
    let (_, _, mut cpu_step) = only_step(&with(&[(DEHAZE_AT, 80)]));
    let out = on_cpu(&mut cpu_step, &frame, &Ctx(None));
    let gap = |f: &Frame| grey(f, 48, 10) - grey(f, 16, 10);
    assert!(gap(&out) > gap(&frame) * 1.1, "{} against {}", gap(&out), gap(&frame));
    assert!(grey(&out, 16, 10) < grey(&frame, 16, 10));
}

#[test]
fn clarity_adds_local_contrast_and_a_negative_one_softens() {
    // mid-sized texture: stripes 20 levels apart and 24 pixels wide, on a grey
    let frame = rgba(192, 540, |x, _| {
        let v = if (x / 24) % 2 == 0 { 118 } else { 138 };
        [v, v, v]
    });
    let (_, _, mut more) = only_step(&with(&[(CLARITY_AT, 100)]));
    let (_, _, mut less) = only_step(&with(&[(CLARITY_AT, -100)]));
    let (sharp, soft) = (on_cpu(&mut more, &frame, &Ctx(None)), on_cpu(&mut less, &frame, &Ctx(None)));
    // the step across a stripe's edge
    let step = |f: &Frame| (grey(f, 24, 200) - grey(f, 23, 200)).abs();
    assert!(step(&sharp) > step(&frame) * 1.2, "{} against {}", step(&sharp), step(&frame));
    assert!(step(&soft) < step(&frame) * 0.9, "{} against {}", step(&soft), step(&frame));
}

#[test]
fn clarity_leaves_no_glowing_outline_at_a_strong_edge() {
    let edge = rgba(192, 540, |x, _| if x < 96 { [40, 40, 40] } else { [210, 210, 210] });
    let (_, _, mut cpu_step) = only_step(&with(&[(CLARITY_AT, 100)]));
    let out = on_cpu(&mut cpu_step, &edge, &Ctx(None));
    // an edge-aware blur keeps the two sides apart, so neither side is pushed far past itself
    for x in 0..192 {
        let v = grey(&out, x, 200);
        assert!((25.0..=225.0).contains(&v), "{x}: {v}");
    }
}

#[test]
fn the_vignette_darkens_the_corners_and_lightens_them_below_zero() {
    let frame = rgba(96, 54, |_, _| [128, 128, 128]);
    let (_, _, mut dark) = only_step(&with(&[(VIGNETTE, 80)]));
    let (_, _, mut light) = only_step(&with(&[(VIGNETTE, -80)]));
    let (d, l) = (on_cpu(&mut dark, &frame, &Ctx(None)), on_cpu(&mut light, &frame, &Ctx(None)));
    assert_eq!(value(&d, 48, 27), [128, 128, 128], "the middle stays as it was");
    assert!(grey(&d, 0, 0) < 70.0 && grey(&l, 0, 0) > 170.0);
    // a rounder one reaches further along the short side
    let (_, _, mut flat) = only_step(&with(&[(VIGNETTE, 80), (VIGNETTE_ROUND, 0)]));
    let (_, _, mut round) = only_step(&with(&[(VIGNETTE, 80), (VIGNETTE_ROUND, 100)]));
    assert!(grey(&on_cpu(&mut round, &frame, &Ctx(None)), 48, 0) > grey(&on_cpu(&mut flat, &frame, &Ctx(None)), 48, 0) - 1.0);
}

#[test]
fn glow_blooms_around_a_bright_part_and_leaves_the_dark_far_away_alone() {
    let frame = rgba(160, 90, |x, y| if (70..90).contains(&x) && (35..55).contains(&y) { [250, 250, 250] } else { [30, 30, 30] });
    let (_, _, mut cpu_step) = only_step(&with(&[(GLOW, 100), (GLOW_SIZE, 100)]));
    let out = on_cpu(&mut cpu_step, &frame, &Ctx(None));
    assert!(grey(&out, 66, 45) > grey(&frame, 66, 45) + 10.0, "next to the light: {}", grey(&out, 66, 45));
    assert_eq!(value(&out, 2, 2), value(&frame, 2, 2));
}

#[test]
fn grain_moves_every_frame_stays_the_same_in_a_frame_and_is_strongest_in_the_mid_tones() {
    let mid = rgba(64, 64, |_, _| [128, 128, 128]);
    let dark = rgba(64, 64, |_, _| [4, 4, 4]);
    let (_, _, mut cpu_step) = only_step(&with(&[(GRAIN, 80), (GRAIN_SIZE, 0)]));
    let at = |step: &mut Box<dyn CpuStep>, f: &Frame, n: u64| on_cpu(step, &timed(f.clone(), n), &Ctx(None));
    let (a, b, a_again) = (at(&mut cpu_step, &mid, 1), at(&mut cpu_step, &mid, 2), at(&mut cpu_step, &mid, 1));
    assert_ne!(a.data, b.data, "a new frame, new grain");
    assert_eq!(a.data, a_again.data, "the same frame, the same grain");
    // brightness only: the three colours of a pixel move together
    assert!(a.data.chunks_exact(4).all(|p| p[0] == p[1] && p[1] == p[2]));
    let (ms, ds) = (spread(&a, (0, 0, 64, 64)), spread(&at(&mut cpu_step, &dark, 1), (0, 0, 64, 64)));
    assert!(ms > 4.0 && ms > ds * 1.5, "mid-tones {ms} against shadows {ds}");
}

#[test]
fn the_cpu_filters_work_on_every_frame_format() {
    let (w, h) = (32usize, 18usize);
    let pixels = |format: PixelFormat| Frame::new(w as u32, h as u32, format, vec![128; format.size(w as u32, h as u32)]);
    let (_, _, mut cpu_step) = only_step(&with(&[(VIGNETTE, 90), (GRAIN, 50)]));
    for format in [PixelFormat::I420, PixelFormat::Nv12, PixelFormat::Rgba, PixelFormat::Bgra] {
        let out = on_cpu(&mut cpu_step, &pixels(format), &Ctx(None));
        assert_eq!((out.format, out.data.len()), (format, format.size(w as u32, h as u32)));
        assert_ne!(out.data, pixels(format).data, "{format:?}");
    }
}

// ── The codex review's cases ──────────────────────────────────────

#[test]
fn the_cpu_average_goes_on_when_a_setting_changes_and_the_steps_are_new() {
    let mut s = with(&[(OVER_TIME, 70)]);
    let memories = Memories::default();
    let step = |s: &Settings| match s.steps_with(&memories, true).remove(0).1 {
        FrameStep::Either(_, cpu) => cpu,
        _ => unreachable!(),
    };
    let mut first = step(&s);
    let ctx = Ctx(None);
    let mut last = None;
    for n in 0..10 {
        last = Some(on_cpu(&mut first, &timed(noisy(64, 48, 120, 9.0, 300 + n as u32), n), &ctx));
    }
    let settled = spread(last.as_ref().unwrap(), (8, 8, 56, 40));
    // a slider moves: a new step, which has to carry on from the average, not start from one noisy frame
    s.values[OVER_TIME] = 75;
    let mut second = step(&s);
    let next = on_cpu(&mut second, &timed(noisy(64, 48, 120, 9.0, 999), 10), &ctx);
    assert!(spread(&next, (8, 8, 56, 40)) < 9.0 * 0.7, "started again: {}", spread(&next, (8, 8, 56, 40)));
    assert!(settled < 9.0 * 0.6);
    // another stream has an average of its own
    struct Other;
    impl FrameContext for Other {
        fn source_id(&self) -> &str {
            "other"
        }
        fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
            None
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
    }
    let mut out = timed(noisy(64, 48, 120, 9.0, 5), 11);
    second.process(&mut out, &Other);
    assert_eq!(out.data, noisy(64, 48, 120, 9.0, 5).data, "its first frame comes out as it came in");
}

#[test]
fn denoising_over_time_does_not_hold_on_to_a_point_that_vanished_or_a_thin_line_that_moved() {
    let p = cpu::DenoiseTime { tolerance: 0.08, keep: 0.9 };
    let mut memory = cpu::Memory::default();
    let mut run = |img: Rgb, n: u64| {
        let mut img = img;
        cpu::denoise_time(&mut img, &mut memory, &p, "cam", n * 33_000_000);
        img
    };
    let mut base = Rgb::filled(32, 32, [90; 3]);
    base.set(16, 16, [210; 3]);
    for n in 0..5 {
        run(base.clone(), n);
    }
    // the bright point is gone: nothing of it is kept
    let out = run(Rgb::filled(32, 32, [90; 3]), 5);
    assert!(out.at(16, 16)[0] <= 92, "a ghost of the point: {}", out.at(16, 16)[0]);
    // a one pixel line that moves a pixel over leaves nothing on the column it left
    let line = |x: usize| {
        let mut img = Rgb::filled(32, 32, [90; 3]);
        for y in 0..32 {
            img.set(x, y, [210; 3]);
        }
        img
    };
    let mut memory2 = cpu::Memory::default();
    for n in 0..5 {
        let mut img = line(10);
        cpu::denoise_time(&mut img, &mut memory2, &p, "cam", n * 33_000_000);
    }
    let mut img = line(11);
    cpu::denoise_time(&mut img, &mut memory2, &p, "cam", 5 * 33_000_000);
    assert!(img.at(10, 8)[0] <= 92, "a ghost of the line: {}", img.at(10, 8)[0]);
    assert!(img.at(11, 8)[0] >= 205, "the line itself: {}", img.at(11, 8)[0]);
    // the same on the card
    if gpu_here() {
        let (_, shader, _) = only_step(&with(&[(OVER_TIME, 100)]));
        let mut card = OnCard::new(FrameStage::Effects, shader);
        let frame = |img: &Rgb, n: u64| {
            let mut f = Frame::new(32, 32, PixelFormat::Rgba, vec![255; 32 * 32 * 4]);
            img.write_to(&mut f);
            timed(f, n)
        };
        for n in 0..5 {
            card.run(&frame(&base, n), &Ctx(None));
        }
        let out = card.run(&frame(&Rgb::filled(32, 32, [90; 3]), 5), &Ctx(None));
        assert!(value(&out, 16, 16)[0] <= 95, "a ghost of the point on the card: {}", value(&out, 16, 16)[0]);
    }
}

#[test]
fn clarity_leaves_a_one_pixel_line_alone() {
    let frame = rgba(192, 540, |x, _| if x == 101 { [210, 210, 210] } else { [40, 40, 40] });
    let (_, shader, mut cpu_step) = only_step(&with(&[(CLARITY_AT, 100)]));
    let out = on_cpu(&mut cpu_step, &frame, &Ctx(None));
    for x in 90..112 {
        assert!(grey(&out, x, 200) <= 215.0, "{x}: {}", grey(&out, x, 200));
    }
    if gpu_here() {
        let out = OnCard::new(FrameStage::Effects, shader).run(&frame, &Ctx(None));
        for x in 90..112 {
            assert!(grey(&out, x, 200) <= 215.0, "card {x}: {}", grey(&out, x, 200));
        }
    }
}

#[test]
fn glow_is_a_soft_bloom_and_not_a_copy_of_the_highlight_at_every_tap() {
    // a small bright spot, with the glow spread as far as it goes
    let frame = rgba(640, 360, |x, y| if (314..326).contains(&x) && (174..186).contains(&y) { [255, 255, 255] } else { [10, 10, 10] });
    let (_, shader, mut cpu_step) = only_step(&with(&[(GLOW, 100), (GLOW_SIZE, 100)]));
    let check = |name: &str, out: &Frame| {
        // all the way round a ring 16 px out the bloom is there, and about as strong everywhere (copies at each tap would leave gaps)
        let lit: Vec<f32> = (0..64).map(|k| {
            let a = k as f32 / 64.0 * std::f32::consts::TAU;
            grey(out, (320.0 + 16.0 * a.cos()) as u32, (180.0 + 16.0 * a.sin()) as u32) - 10.0
        }).collect();
        let (least, most) = (lit.iter().cloned().fold(f32::MAX, f32::min), lit.iter().cloned().fold(f32::MIN, f32::max));
        assert!(least > 4.0 && least > most * 0.25, "{name}: {least} to {most}");
        // and it fades with the distance
        assert!(grey(out, 320 + 12, 180) > grey(out, 320 + 40, 180), "{name}");
    };
    check("CPU", &on_cpu(&mut cpu_step, &frame, &Ctx(None)));
    if gpu_here() {
        check("card", &OnCard::new(FrameStage::Colour, shader).run(&frame, &Ctx(None)));
    }
}

#[test]
fn dehaze_takes_synthetic_haze_out_in_linear_light() {
    let (a, t) = (0.92f32.powf(2.2), 0.7f32);
    let encode = |lin: f32| (lin.max(0.0).powf(1.0 / 2.2) * 255.0).round().clamp(0.0, 255.0) as u8;
    let linear = |v: u8| (v as f32 / 255.0).powf(2.2);
    // a clear picture with dark cells in every patch, then haze laid over it as light adds: lin = J * t + A * (1 - t)
    let clear = rgba(96, 96, |x, y| {
        let cell = (x / 3 + y / 3) % 2 == 0;
        if cell { [20, 25, 30] } else { [130, 120, 100] }
    });
    let hazy = rgba(96, 96, |x, y| {
        let c = value(&clear, x, y);
        [encode(linear(c[0]) * t + a * (1.0 - t)), encode(linear(c[1]) * t + a * (1.0 - t)), encode(linear(c[2]) * t + a * (1.0 - t))]
    });
    let (_, shader, mut cpu_step) = only_step(&with(&[(DEHAZE_AT, 100)]));
    let error = |f: &Frame| apart(f, &clear).0;
    let out = on_cpu(&mut cpu_step, &hazy, &Ctx(None));
    assert!(error(&out) < error(&hazy) * 0.75, "{} against {}", error(&out), error(&hazy));
    if gpu_here() {
        let by_card = OnCard::new(FrameStage::Effects, shader).run(&hazy, &Ctx(None));
        assert!(error(&by_card) < error(&hazy) * 0.75, "{} against {}", error(&by_card), error(&hazy));
        assert!(apart(&by_card, &out).0 < 0.6);
    }
}

#[test]
fn skin_smoothing_stays_out_of_the_steps_while_there_can_be_no_mask() {
    let (mut p, mut host) = rig();
    p.set_config(&json!({"skin": 50})).unwrap();
    host.vision = Some("ready".into());
    host.pack = VisionPack { ready: true, available: true, ..VisionPack::default() };
    p.on_event(&mut host, &Event::VisionStateChanged { state: "ready".into() });
    assert_eq!(p.staged_frame_steps().len(), 1);
    host.vision = Some(MISSING.into());
    host.pack = VisionPack { available: true, size_mb: 47, ..VisionPack::default() };
    let before = host.calls.iter().filter(|c| *c == "refresh_frame_step").count();
    p.on_event(&mut host, &Event::VisionStateChanged { state: MISSING.into() });
    assert!(p.staged_frame_steps().is_empty(), "no pack, no step, so nothing is brought down from the card for a mask that can't come");
    assert_eq!(host.calls.iter().filter(|c| *c == "refresh_frame_step").count(), before + 1, "the stream is told");
    // the setting is still there for when the pack is
    assert_eq!(p.get_config()["skin"], json!(50));
}

#[test]
fn a_masked_cpu_step_rests_without_a_mask_and_a_slow_one_rests_longer_each_time() {
    use telescope_core::frame::Touch;
    let ctx = Ctx(None);
    let mut frame = noisy(16, 16, 100, 1.0, 1);
    let mut needs_mask = Pixels::masked(|_, _, _| {});
    assert_eq!(needs_mask.touch(&ctx), Touch::Changes);
    needs_mask.process(&mut frame, &ctx);
    assert_eq!(needs_mask.touch(&ctx), Touch::Nothing, "no mask: the card's picture stays where it is for a while");
    let mut slow = Pixels::new(|_, _, _| std::thread::sleep(std::time::Duration::from_millis(CPU_BUDGET_MS + 15)));
    let mut rests = Vec::new();
    for _ in 0..3 {
        slow.process(&mut frame, &ctx);
        let mut n = 0;
        while slow.touch(&ctx) == Touch::Nothing {
            n += 1;
        }
        rests.push(n);
    }
    assert_eq!(rests, [CPU_REST_FRAMES, CPU_REST_FRAMES * 2, CPU_REST_FRAMES * 4]);
}

#[test]
fn a_slow_cpu_step_in_the_real_pipeline_is_tried_once_and_then_skipped() {
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counted = calls.clone();
    let slow = Pixels::new(move |_, _, _| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(CPU_BUDGET_MS + 15));
    });
    let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
    pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Effects, step: FrameStep::Cpu(Box::new(slow)) }]);
    let frame = noisy(16, 16, 100, 1.0, 1);
    for n in 0..20 {
        let out = pipeline.process(timed(frame.clone(), n), &Ctx(None));
        if n > 0 {
            assert_eq!(out.data, frame.data, "frame {n} comes out as the camera made it");
        }
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_cpu_step_that_takes_too_long_rests_and_lets_the_picture_through() {
    use telescope_core::frame::Touch;
    let mut slow = Pixels::new(|_, _, _| std::thread::sleep(std::time::Duration::from_millis(CPU_BUDGET_MS + 15)));
    let mut quick = Pixels::new(|_, _, _| {});
    let ctx = Ctx(None);
    let mut frame = noisy(16, 16, 100, 1.0, 1);
    assert_eq!(slow.touch(&ctx), Touch::Changes);
    slow.process(&mut frame, &ctx);
    quick.process(&mut frame, &ctx);
    // it steps aside for a while and then tries again; one that was quick never does
    assert!((0..CPU_REST_FRAMES).all(|_| slow.touch(&ctx) == Touch::Nothing));
    assert_eq!(slow.touch(&ctx), Touch::Changes);
    assert_eq!(quick.touch(&ctx), Touch::Changes);
}

#[test]
fn sharpen_and_clarity_work_on_the_y_plane_of_a_video_frame_and_leave_its_colours_alone() {
    let as_video = |f: &Frame, format: PixelFormat| {
        let mut v = Frame::new(f.width, f.height, format, vec![0; format.size(f.width, f.height)]);
        Rgb::from_frame(f).unwrap().write_to(&mut v);
        v
    };
    let planes = |f: &Frame| f.data.len() - (f.width * f.height) as usize;
    for (name, s) in [("sharpen", with(&[(SHARPEN_AT, 100), (RADIUS, 30), (THRESHOLD, 0)])), ("clarity", with(&[(CLARITY_AT, 100)]))] {
        let flat = soft_edge(30.0, 230.0, 1.5);
        for format in [PixelFormat::I420, PixelFormat::Nv12] {
            let video = as_video(&flat, format);
            let (_, _, mut cpu_step) = only_step(&s);
            let by_planes = on_cpu(&mut cpu_step, &video, &Ctx(None));
            let (w, h) = (video.width as usize, video.height as usize);
            assert_ne!(by_planes.data[..w * h], video.data[..w * h], "{name} {format:?} changed the picture");
            assert_eq!(by_planes.data[w * h..], video.data[w * h..], "{name} {format:?}: the colour planes are as they were ({} bytes)", planes(&video));
            // and it is what the RGB way makes of the same picture, give or take a level or two
            let mut rgb = Rgb::from_frame(&video).unwrap();
            match name {
                "sharpen" => cpu::sharpen(&mut rgb, &s.sharpen().unwrap()),
                _ => cpu::clarity(&mut rgb, &s.clarity().unwrap()),
            }
            let mut by_rgb = video.clone();
            rgb.write_to(&mut by_rgb);
            let worst = (0..w * h).map(|i| (by_planes.data[i] as i32 - by_rgb.data[i] as i32).abs()).max().unwrap();
            let mean = (0..w * h).map(|i| (by_planes.data[i] as i32 - by_rgb.data[i] as i32).abs() as f32).sum::<f32>() / (w * h) as f32;
            assert!(mean < 0.5 && worst <= 12, "{name} {format:?}: {mean} {worst}");
        }
    }
}

// ── What it costs ─────────────────────────────────────────────────

/// Prints what each filter costs for one 1080p frame on the CPU and on the card. `cargo test --release -p telescope-effects filters_cost -- --ignored --nocapture`
#[test]
#[ignore]
fn filters_cost_per_frame_at_1080p() {
    use std::time::Instant;
    let (w, h) = (1920, 1080);
    // as the decoder hands it over
    let frame = {
        let mut i420 = Frame::new(w, h, PixelFormat::I420, vec![0; PixelFormat::I420.size(w, h)]);
        Rgb::from_frame(&scene(w, h)).unwrap().write_to(&mut i420);
        timed(i420, 3)
    };
    let ctx = Ctx(Some(scene_mask(w, h)));
    let cases: [(&str, Settings); 9] = [
        ("denoise", with(&[(DENOISE, 60)])),
        ("over time", with(&[(OVER_TIME, 60)])),
        ("dehaze", with(&[(DEHAZE_AT, 60)])),
        ("clarity", with(&[(CLARITY_AT, 60)])),
        ("sharpen", with(&[(SHARPEN_AT, 60)])),
        ("skin", with(&[(SKIN_AT, 60)])),
        ("vignette", with(&[(VIGNETTE, 60)])),
        ("glow", with(&[(GLOW, 60)])),
        ("grain", with(&[(GRAIN, 60)])),
    ];
    let gpu = gpu_here();
    // what a CPU step costs before it does anything: the frame as RGB and back, and the copy a changed frame gets
    let mut nothing: Box<dyn CpuStep> = Box::new(Pixels::new(|_, _, _| {}));
    on_cpu(&mut nothing, &frame, &ctx);
    // wall time (the fastest of 7, which a busy machine disturbs least) and the processor time of all threads, which it disturbs less
    let busy = || -> f64 {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let after: Vec<&str> = stat.rsplit(')').next().unwrap_or("").split_whitespace().collect();
        (after.get(11).and_then(|t| t.parse::<f64>().ok()).unwrap_or(0.0) + after.get(12).and_then(|t| t.parse::<f64>().ok()).unwrap_or(0.0)) * 10.0
    };
    let fastest = |mut f: Box<dyn FnMut(u64) + '_>| {
        let (cpu_before, began) = (busy(), Instant::now());
        let wall = (0..7).map(|n| { let t = Instant::now(); f(4 + n); t.elapsed().as_secs_f64() * 1000.0 }).fold(f64::MAX, f64::min);
        let _ = began;
        (wall, (busy() - cpu_before) / 7.0)
    };
    let (wall, cpu_ms) = fastest(Box::new(|n| { on_cpu(&mut nothing, &timed(frame.clone(), n), &ctx); }));
    eprintln!("COST nothing at all: CPU wall {wall:.1} ms, processor time {cpu_ms:.1} ms");
    for (name, s) in cases {
        let (stage, shader, mut cpu_step) = only_step(&s);
        on_cpu(&mut cpu_step, &frame, &ctx);
        let (by_cpu, cpu_ms) = fastest(Box::new(|n| { on_cpu(&mut cpu_step, &timed(frame.clone(), n), &ctx); }));
        let mut by_card = f64::NAN;
        if gpu {
            let mut card = OnCard::new(stage, shader);
            for n in 0..3 {
                card.run(&timed(frame.clone(), n), &ctx);
            }
            let began = Instant::now();
            for n in 0..20 {
                card.run(&timed(frame.clone(), 10 + n), &ctx);
            }
            by_card = began.elapsed().as_secs_f64() * 50.0;
        }
        eprintln!("COST {name}: CPU wall {by_cpu:.1} ms, processor time {cpu_ms:.1} ms; card {by_card:.1} ms (with the upload and read back of a lone step)");
    }
    // the whole thing at once
    let all = with(&[(DENOISE, 50), (OVER_TIME, 50), (DEHAZE_AT, 50), (CLARITY_AT, 50), (SHARPEN_AT, 50), (SKIN_AT, 50), (VIGNETTE, 50), (GLOW, 50), (GRAIN, 50)]);
    let mut steps = all.steps();
    let shaders: Vec<(FrameStage, ShaderStep)> = steps.iter().map(|(stage, s)| match s {
        FrameStep::Either(shader, _) => (*stage, shader.clone()),
        _ => unreachable!(),
    }).collect();
    if gpu {
        let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
        pipeline.set_steps(shaders.into_iter().map(|(stage, s)| StagedStep { owner: NAME.into(), stage, step: FrameStep::Shader(s) }).collect());
        for n in 0..3 {
            pipeline.process(timed(frame.clone(), n), &ctx);
        }
        let began = Instant::now();
        for n in 0..20 {
            pipeline.process(timed(frame.clone(), 10 + n), &ctx);
        }
        eprintln!("COST all nine on the card, one upload and one read back: {:.1} ms", began.elapsed().as_secs_f64() * 50.0);
    }
    let mut cpus: Vec<Box<dyn CpuStep>> = steps.drain(..).map(|(_, s)| match s {
        FrameStep::Either(_, c) => c,
        _ => unreachable!(),
    }).collect();
    let run_all = |cpus: &mut Vec<Box<dyn CpuStep>>, n: u64| {
        let mut f = timed(frame.clone(), n);
        for c in cpus.iter_mut() {
            c.process(&mut f, &ctx);
        }
    };
    run_all(&mut cpus, 0);
    let (wall, cpu_ms) = fastest(Box::new(|n| run_all(&mut cpus, n)));
    eprintln!("COST all nine on the CPU: wall {wall:.1} ms, processor time {cpu_ms:.1} ms");
}

// ── The second review's cases ─────────────────────────────────────

fn halves(left: u8, right: u8) -> Frame {
    rgba(32, 16, |x, _| if x < 16 { [left; 3] } else { [right; 3] })
}

#[test]
fn flipping_the_picture_makes_the_cpu_average_start_again() {
    let s = with(&[(OVER_TIME, 100)]);
    let memories = Memories::default();
    let FrameStep::Either(_, mut step) = s.steps_with(&memories, true).remove(0).1 else { unreachable!() };
    let ctx = Ctx(None);
    for n in 0..4 {
        on_cpu(&mut step, &timed(halves(100, 120), n), &ctx);
    }
    // Without the pipeline's word the old halves are blended into the flipped ones
    let blended = on_cpu(&mut step, &timed(halves(120, 100), 4), &ctx);
    assert_ne!(value(&blended, 4, 4), [120; 3], "the review's case");
    step.forget_history("cam");
    let again = on_cpu(&mut step, &timed(halves(120, 100), 5), &ctx);
    assert_eq!((value(&again, 4, 4), value(&again, 28, 4)), ([120; 3], [100; 3]), "a flip is a new picture");
    // Another stream's average isn't touched by one stream's view
    step.forget_history("somewhere else");
}

#[test]
fn two_streams_denoise_without_waiting_for_each_other() {
    let s = with(&[(OVER_TIME, 70)]);
    let memories = Memories::default();
    let FrameStep::Either(_, mut step) = s.steps_with(&memories, true).remove(0).1 else { unreachable!() };
    // Another stream's frame is being worked on: its memory is locked
    let other = Arc::new(Mutex::new(cpu::Memory::default()));
    memories.lock().unwrap().insert("other".into(), other.clone());
    let busy = other.lock().unwrap();
    let out = on_cpu(&mut step, &timed(noisy(64, 48, 120, 9.0, 3), 0), &Ctx(None));
    assert_eq!(out.width, 64, "the frame of the stream 'cam' went through while 'other' was busy");
    drop(busy);
    assert_eq!(memories.lock().unwrap().len(), 2);
}

#[test]
fn a_stopped_stream_and_a_denoise_that_went_off_release_their_pictures() {
    let mut plugin = FiltersPlugin::new();
    plugin.settings.values[OVER_TIME] = 70;
    let mut host = FakeHost::default();
    let FrameStep::Either(_, mut step) = plugin.staged_frame_steps().remove(0).1 else { unreachable!() };
    on_cpu(&mut step, &timed(noisy(64, 48, 120, 9.0, 3), 0), &Ctx(None));
    assert_eq!(plugin.memories.lock().unwrap().len(), 1, "a stream that denoises keeps its last picture");
    plugin.on_stream_stop(&mut host);
    assert!(plugin.memories.lock().unwrap().is_empty(), "the stream stopped");
    on_cpu(&mut step, &timed(noisy(64, 48, 120, 9.0, 4), 1), &Ctx(None));
    assert_eq!(plugin.memories.lock().unwrap().len(), 1);
    plugin.settings.values[OVER_TIME] = 0;
    assert!(plugin.staged_frame_steps().is_empty());
    assert!(plugin.memories.lock().unwrap().is_empty(), "no temporal denoise any more");
}

#[test]
fn a_cpu_step_that_stays_too_slow_does_not_flash_on_a_frame_now_and_then() {
    use telescope_core::frame::Touch;
    let slow = || std::thread::sleep(std::time::Duration::from_millis(CPU_BUDGET_MS + 15));
    let mut step = Pixels::new(move |rgb, _, _| {
        slow();
        rgb.data.iter_mut().for_each(|b| *b = !*b);
    });
    let ctx = Ctx(None);
    let original = noisy(16, 16, 100, 1.0, 1);
    let mut filtered = 0;
    // enough frames for the first try and two more after rests
    for _ in 0..(CPU_REST_FRAMES * 4 + 10) {
        let mut frame = original.clone();
        if step.touch(&ctx) == Touch::Changes {
            step.process(&mut frame, &ctx);
        }
        if frame.data != original.data {
            filtered += 1;
        }
    }
    assert_eq!(filtered, 1, "only the first slow frame shows the filter; the tries after a rest are dropped while it is still too slow");
}
