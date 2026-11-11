use super::*;
use crate::cpu::Rgb;
use crate::sheet::{invert, perspective};
use telescope_core::frame::PixelFormat;
use telescope_core::media::{Media, StagedStep};

struct Ctx;

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        "phone"
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

const SHEET: [[f64; 2]; 4] = [[0.30, 0.18], [0.72, 0.22], [0.78, 0.85], [0.24, 0.80]];

/// A desk with noise, a bright sheet of ruled lines and a blob, so the warp has edges and detail to get wrong.
fn desk(w: usize, h: usize) -> Rgb {
    let mut rgb = Rgb::new(w, h);
    let mut n = 7u32;
    let corners = SHEET.map(|[x, y]| [x * w as f64, y * h as f64]);
    let inv = invert(&perspective(&[[0.0, 0.0], [210.0, 0.0], [210.0, 297.0], [0.0, 297.0]], &corners));
    for y in 0..h {
        for x in 0..w {
            n = n.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = (n >> 29) as f32 - 3.0;
            let (fx, fy) = (x as f64, y as f64);
            let d = inv[2][0] * fx + inv[2][1] * fy + inv[2][2];
            let (sx, sy) = ((inv[0][0] * fx + inv[0][1] * fy + inv[0][2]) / d, (inv[1][0] * fx + inv[1][1] * fy + inv[1][2]) / d);
            let base = if (0.0..210.0).contains(&sx) && (0.0..297.0).contains(&sy) {
                let ruled = (sy % 9.0) < 0.9 && sx > 12.0;
                let blob = ((sx - 105.0).powi(2) + (sy - 150.0).powi(2)).sqrt() < 40.0;
                let shade = 225.0 + 25.0 * (sx / 210.0) as f32;
                if ruled { [60.0, 80.0, 190.0] } else if blob { [200.0, 40.0, 40.0] } else { [shade, shade, shade - 12.0] }
            } else {
                let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
                [70.0 + 60.0 * v, 55.0 + 40.0 * u, 40.0 + 30.0 * (u * 7.0).sin().abs()]
            };
            rgb.set(x, y, base.map(|c| (c + noise).clamp(0.0, 255.0) as u8));
        }
    }
    rgb
}

fn rgba(rgb: &Rgb) -> Frame {
    let mut f = Frame::new(rgb.w as u32, rgb.h as u32, PixelFormat::Rgba, vec![0; rgb.w * rgb.h * 4]);
    rgb.write_to(&mut f);
    f
}

fn plugin(config: Value) -> DeskPlugin {
    let mut p = DeskPlugin::new();
    let mut cfg = json!({"on": true, "corners": SHEET.to_vec()});
    cfg.as_object_mut().unwrap().extend(config.as_object().unwrap().clone());
    p.set_config(&cfg).unwrap();
    p
}

/// The picture through the desk step on the card (as a step that can run either way) and on the CPU: (card, cpu).
fn both(config: Value, picture: &Rgb) -> (Rgb, Rgb) {
    let mut p = plugin(config);
    let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
    let Some(step @ FrameStep::Either(..)) = p.frame_step() else { panic!("plain straightening runs either way") };
    pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Geometry, step }]);
    let before = telescope_media::transfers::totals();
    let card = Rgb::from_frame(&pipeline.process(rgba(picture), &Ctx)).unwrap();
    assert!(telescope_media::transfers::totals().up > before.up, "the frame didn't go to the card");
    let Some(FrameStep::Either(_, mut cpu)) = p.frame_step() else { panic!("a step") };
    let mut f = rgba(picture);
    cpu.process(&mut f, &Ctx);
    (card, Rgb::from_frame(&f).unwrap())
}

/// (max, mean, psnr, share of values off by more than 3)
fn difference(card: &Rgb, cpu: &Rgb) -> (u8, f64, f64, f64) {
    let diffs: Vec<u8> = card.data.iter().zip(&cpu.data).map(|(a, b)| a.abs_diff(*b)).collect();
    let mse = diffs.iter().map(|d| (*d as f64).powi(2)).sum::<f64>() / diffs.len() as f64;
    let mean = diffs.iter().map(|d| *d as f64).sum::<f64>() / diffs.len() as f64;
    let over = diffs.iter().filter(|d| **d > 3).count() as f64 / diffs.len() as f64;
    (*diffs.iter().max().unwrap(), mean, 10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10(), over)
}

#[test]
fn the_desk_on_the_card_looks_like_the_one_on_the_cpu() {
    if telescope_media::gpu::Gpu::shared().is_none() {
        eprintln!("no graphics card here: the shader comparison is skipped");
        return;
    }
    let cases = [
        ("auto", json!({})),
        ("a4", json!({"shape": "a4"})),
        ("fill", json!({"shape": "fill"})),
        ("square stretched", json!({"shape": "square", "stretch": [140, 70]})),
        ("letter squeezed", json!({"shape": "letter", "stretch": [60, 150]})),
    ];
    for (w, h) in [(1920usize, 1080usize), (3840, 2160), (1283, 721)] {
        let picture = desk(w, h);
        for (name, config) in &cases {
            let (card, cpu) = both(config.clone(), &picture);
            let (max, mean, psnr, over) = difference(&card, &cpu);
            println!("desk {w}x{h} {name}: max {max}, mean {mean:.3}, psnr {psnr:.1} dB, {:.3}% of values off by more than 3", over * 100.0);
            assert!(psnr > 40.0 && mean < 0.5 && over < 0.01, "{w}x{h} {name}: max {max}, psnr {psnr}, mean {mean}, over {over}");
        }
    }
}

#[test]
fn the_desk_card_borders_and_edges_land_where_the_cpu_puts_them() {
    if telescope_media::gpu::Gpu::shared().is_none() {
        eprintln!("no graphics card here: the shader comparison is skipped");
        return;
    }
    // Sharp white and black blocks over the whole picture: the worst case for a position a 1/32 pixel off
    let (w, h) = (1283usize, 721usize);
    let mut blocks = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = if (x / 3 + y / 5) % 2 == 0 { 235 } else { 25 };
            blocks.set(x, y, [v, v, v]);
        }
    }
    for config in [json!({}), json!({"shape": "fill"})] {
        let (card, cpu) = both(config.clone(), &blocks);
        let (max, mean, psnr, over) = difference(&card, &cpu);
        println!("desk blocks {config}: max {max}, mean {mean:.3}, psnr {psnr:.1} dB, {:.3}% of values off by more than 3", over * 100.0);
        assert!(psnr > 30.0 && mean < 3.0 && over < 0.05, "{config}: max {max}, psnr {psnr}, mean {mean}, over {over}");
        // the grey around the sheet is the CPU's
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            let (a, b) = (card.data[(y * w + x) * 3], cpu.data[(y * w + x) * 3]);
            assert!(a.abs_diff(b) <= 1, "{config} corner {x},{y}: {a} against {b}");
        }
    }
}

#[test]
fn the_desk_card_answers_for_the_size_it_is_asked_about() {
    let mut p = plugin(json!({"shape": "a4"}));
    let Some(FrameStep::Either(_, mut cpu)) = p.frame_step() else { panic!("a step") };
    let a = cpu.card_uniforms(1920, 1080).unwrap();
    let b = cpu.card_uniforms(1280, 720).unwrap();
    assert_ne!(a, b, "another size of picture is another warp");
    assert_eq!(cpu.card_uniforms(1920, 1080).unwrap(), a);
    // too small to hold a sheet: the identity, which leaves the frame as it is
    assert_eq!(cpu.card_uniforms(1, 40).unwrap()["rect"], json!([0, 0, 1, 40]));
    // cleaning the sheet up needs its pixels, so that stays on the CPU
    let mut clean = plugin(json!({"cleanup": "contrast"}));
    assert!(matches!(clean.frame_step(), Some(FrameStep::Cpu(_))));
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_desk_on_the_card_and_the_cpu() {
    let has_card = telescope_media::gpu::Gpu::shared().is_some();
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let frame = rgba(&desk(w, h));
        let median = |mut run: Box<dyn FnMut(Frame) -> Frame>| {
            run(frame.clone());
            let mut times: Vec<f64> = (0..9).map(|_| {
                let input = frame.clone();
                let start = std::time::Instant::now();
                std::hint::black_box(run(input));
                start.elapsed().as_secs_f64() * 1000.0
            }).collect();
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            times[4]
        };
        let mut p = plugin(json!({"shape": "a4"}));
        let Some(FrameStep::Either(_, mut cpu)) = p.frame_step() else { panic!("a step") };
        println!("desk on the CPU {w}x{h}: {:.2} ms", median(Box::new(move |mut f| { cpu.process(&mut f, &Ctx); f })));
        if has_card {
            let mut p = plugin(json!({"shape": "a4"}));
            let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
            pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Geometry, step: p.frame_step().unwrap() }]);
            println!("desk on the card {w}x{h}: {:.2} ms through the pipeline, up and down included", median(Box::new(move |f| pipeline.process(f, &Ctx))));
        }
    }
}

/// Stands in for a step that has to see the pixels on the CPU (a blur, a name tag): one pass over the frame.
struct Touches;

impl CpuStep for Touches {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        frame.data_mut().iter_mut().for_each(|v| *v = v.wrapping_add(1));
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_desk_between_other_steps() {
    if telescope_media::gpu::Gpu::shared().is_none() {
        return eprintln!("no graphics card here");
    }
    use telescope_core::media::StagedStep as S;
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let frame = rgba(&desk(w, h));
        for (chain, steps) in [("desk alone", 0), ("desk, a CPU step, the CRT, a CPU step", 1)] {
            for way in ["CPU", "card"] {
                let mut p = plugin(json!({"shape": "a4"}));
                let desk_step = match (p.frame_step().unwrap(), way) {
                    (FrameStep::Either(_, cpu), "CPU") => FrameStep::Cpu(cpu),
                    (step, _) => step,
                };
                let mut staged = vec![S { owner: NAME.into(), stage: FrameStage::Geometry, step: desk_step }];
                if steps == 1 {
                    let mut fun = crate::fun_pack::FunPackPlugin::new();
                    fun.set_config(&json!({"effect": "crt"})).unwrap();
                    staged.push(S { owner: "blur".into(), stage: FrameStage::Background, step: FrameStep::Cpu(Box::new(Touches)) });
                    staged.push(S { owner: "fun_pack".into(), stage: FrameStage::Effects, step: fun.frame_step().unwrap() });
                    staged.push(S { owner: "tag".into(), stage: FrameStage::Overlay, step: FrameStep::Cpu(Box::new(Touches)) });
                }
                let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
                pipeline.set_steps(staged);
                for _ in 0..5 {
                    pipeline.process(frame.clone(), &Ctx);
                }
                let before = telescope_media::transfers::totals();
                let mut times: Vec<f64> = (0..15).map(|_| {
                    let input = frame.clone();
                    let start = std::time::Instant::now();
                    std::hint::black_box(pipeline.process(input, &Ctx));
                    start.elapsed().as_secs_f64() * 1000.0
                }).collect();
                let after = telescope_media::transfers::totals();
                times.sort_by(|a, b| a.partial_cmp(b).unwrap());
                println!("{w}x{h} {chain}, desk on the {way}: {:.2} ms a frame, {:.2} up and {:.2} down per frame, balance says {:?}",
                    times[7], (after.up - before.up) as f64 / 15.0, (after.down - before.down) as f64 / 15.0, telescope_core::balance::Balance::shared().place());
            }
        }
    }
}
