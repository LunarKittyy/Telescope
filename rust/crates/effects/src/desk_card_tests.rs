use super::*;
use crate::cpu::Rgb;
use crate::sheet::{invert, perspective};
use telescope_core::frame::PixelFormat;
use telescope_core::balance::{Balance, Place};
use telescope_core::media::{FramePipeline, Media, StagedStep};
use telescope_media::pipeline::Pipeline;

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

/// A card the frames really go to: Windows' software renderer (WARP) is found as a card, but the steps stay on the CPU there.
fn real_card() -> bool {
    telescope_media::gpu::Gpu::shared().is_some_and(|gpu| !gpu.adapter_name.contains("Basic Render"))
}

/// The picture through the desk step on the card (as a step that can run either way) and on the CPU: (card, cpu).
fn both(config: Value, picture: &Rgb) -> (Rgb, Rgb) {
    both_of(config, rgba(picture))
}

/// The same for a frame of any format, both read back as RGB the same way.
fn both_of(config: Value, frame: Frame) -> (Rgb, Rgb) {
    let mut p = plugin(config);
    // Pinned to the card: a slow software card would otherwise get moved to the CPU by the balance mid-test
    let mut pipeline = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
    let Some(step @ FrameStep::Either(..)) = p.frame_step() else { panic!("plain straightening runs either way") };
    pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Geometry, step }]);
    let before = telescope_media::transfers::totals();
    let card = pipeline.process(frame.clone(), &Ctx);
    assert_eq!(card.format, frame.format);
    assert!(telescope_media::transfers::totals().up > before.up, "the frame didn't go to the card");
    let Some(FrameStep::Either(_, mut cpu)) = p.frame_step() else { panic!("a step") };
    let mut f = frame;
    cpu.process(&mut f, &Ctx);
    (Rgb::from_frame(&card).unwrap(), Rgb::from_frame(&f).unwrap())
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
    if !real_card() {
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

/// The desk with saturated blocks and stripes laid over it, so chroma has sharp edges to get wrong, as a frame of the format.
fn coloured(w: usize, h: usize, format: PixelFormat) -> Frame {
    let mut rgb = desk(w, h);
    let colours = [[235, 20, 20], [20, 220, 30], [30, 40, 240], [240, 230, 20], [230, 30, 220], [20, 220, 230]];
    for y in 0..h {
        for x in 0..w {
            let (bx, by) = (x / 7, y / 9);
            if (bx + 2 * by) % 5 == 0 {
                rgb.set(x, y, colours[(bx / 5 + by) % colours.len()]);
            } else if x % 11 == 0 && y > h / 2 {
                rgb.set(x, y, colours[x % 6]);
            }
        }
    }
    let mut f = Frame::new(w as u32, h as u32, format, vec![0; format.size(w as u32, h as u32)]);
    rgb.write_to(&mut f);
    f
}

#[test]
fn the_desk_on_the_card_keeps_the_cpus_chroma_for_yuv_frames() {
    if !real_card() {
        eprintln!("no graphics card here: the shader comparison is skipped");
        return;
    }
    for format in [PixelFormat::I420, PixelFormat::Nv12] {
        for (w, h) in [(1920usize, 1080usize), (1283, 721)] {
            for (name, config) in [("auto", json!({})), ("fill", json!({"shape": "fill"}))] {
                let (card, cpu) = both_of(config, coloured(w, h, format));
                let (max, mean, psnr, over) = difference(&card, &cpu);
                println!("desk {format:?} {w}x{h} {name}: max {max}, mean {mean:.3}, psnr {psnr:.1} dB, {:.3}% of values off by more than 3", over * 100.0);
                assert!(psnr >= 52.0 && mean < 0.2 && over < 0.01, "{format:?} {w}x{h} {name}: max {max}, psnr {psnr}, mean {mean}, over {over}");
            }
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
    // the cleanups go either way too, with the sizes of their passes for this picture
    for cleanup in ["contrast", "bw"] {
        let mut clean = plugin(json!({"cleanup": cleanup}));
        let Some(FrameStep::Either(_, mut cpu)) = clean.frame_step() else { panic!("{cleanup} runs either way") };
        let u = cpu.card_uniforms(1920, 1080).unwrap();
        let rect = &u["rect"];
        assert_eq!(u[shader::PASS_SIZES][0], json!([rect[2], rect[3]]), "{cleanup}: the first pass is the sheet at its own size");
        assert_eq!(cpu.card_uniforms(1, 40).unwrap()["untouched"], json!(1));
    }
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

/// A page of writing under uneven light, as a phone sees a sheet on a desk: lines of words (short dark strokes), a heading, a smudge of colour and a shadow across a corner.
fn document(w: usize, h: usize) -> Rgb {
    let mut rgb = Rgb::new(w, h);
    let corners = SHEET.map(|[x, y]| [x * w as f64, y * h as f64]);
    let inv = invert(&perspective(&[[0.0, 0.0], [210.0, 0.0], [210.0, 297.0], [0.0, 297.0]], &corners));
    let mut n = 11u32;
    let mut noise = || {
        n = n.wrapping_mul(1664525).wrapping_add(1013904223);
        (n >> 29) as f32 - 3.5
    };
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f64, y as f64);
            let d = inv[2][0] * fx + inv[2][1] * fy + inv[2][2];
            let (sx, sy) = ((inv[0][0] * fx + inv[0][1] * fy + inv[0][2]) / d, (inv[1][0] * fx + inv[1][1] * fy + inv[1][2]) / d);
            let c = if (0.0..210.0).contains(&sx) && (0.0..297.0).contains(&sy) {
                // Light falls off towards the bottom left, with a soft shadow over the top right
                let light = 0.62 + 0.38 * (sx / 210.0) as f32 * (1.0 - 0.4 * (sy / 297.0) as f32);
                let shadow = if sx + (297.0 - sy) * 0.3 > 230.0 { 0.75 } else { 1.0 };
                let paper = 238.0 * light * shadow;
                let (line, inline) = ((sy - 30.0) / 7.0, (sy - 30.0) % 7.0);
                let word = ((sx - 18.0) / 9.0).floor() as i64;
                let in_word = (sx - 18.0) % 9.0 < 7.0 - (word.rem_euclid(3)) as f64;
                let writing = sy > 30.0 && sy < 270.0 && sx > 18.0 && sx < 192.0 && inline > 2.0 && inline < 4.2 && in_word && (line as i64 + word) % 7 != 0;
                let heading = sy > 14.0 && sy < 22.0 && sx > 40.0 && sx < 170.0 && ((sx - 40.0) % 12.0) < 9.0;
                let smudge = ((sx - 150.0).powi(2) + (sy - 240.0).powi(2)).sqrt() < 14.0;
                if writing || heading {
                    [40.0 * light, 45.0 * light, 70.0 * light]
                } else if smudge {
                    [paper, paper * 0.85, paper * 0.4]
                } else {
                    [paper, paper, paper - 8.0]
                }
            } else {
                let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
                [90.0 + 50.0 * v, 70.0 + 30.0 * u, 55.0 + 20.0 * (u * 9.0).sin().abs()]
            };
            rgb.set(x, y, c.map(|c| (c + noise()).clamp(0.0, 255.0) as u8));
        }
    }
    rgb
}

/// The step on the card (balance pinned there) and the CPU half on the same frame, both read back as RGB.
fn cleaned_both(config: Value, frame: Frame) -> (Rgb, Rgb) {
    let mut p = plugin(config);
    let mut pipeline = Pipeline::with_balance(Arc::new(Balance::pinned_to(Place::Card)));
    let step = p.frame_step().unwrap();
    assert!(matches!(step, FrameStep::Either(..)));
    pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Geometry, step }]);
    let before = telescope_media::transfers::totals();
    let card = pipeline.process(frame.clone(), &Ctx);
    assert_eq!((card.format, card.width, card.height), (frame.format, frame.width, frame.height));
    assert!(telescope_media::transfers::totals().up > before.up, "the frame didn't go to the card");
    let Some(FrameStep::Either(_, mut cpu)) = p.frame_step() else { panic!("a step") };
    let mut f = frame;
    cpu.process(&mut f, &Ctx);
    (Rgb::from_frame(&card).unwrap(), Rgb::from_frame(&f).unwrap())
}

fn frame_of(rgb: &Rgb, format: PixelFormat) -> Frame {
    let mut f = Frame::new(rgb.w as u32, rgb.h as u32, format, vec![0; format.size(rgb.w as u32, rgb.h as u32)]);
    rgb.write_to(&mut f);
    f
}

/// Saves the picture as a PNG in DESK_PNG_DIR when that is set, to look at side by side.
fn save_png(rgb: &Rgb, name: &str) {
    let Ok(dir) = std::env::var("DESK_PNG_DIR") else { return };
    let path = std::path::Path::new(&dir).join(format!("{name}.ppm"));
    let mut data = format!("P6\n{} {}\n255\n", rgb.w, rgb.h).into_bytes();
    data.extend_from_slice(&rgb.data);
    std::fs::write(path, data).unwrap();
}

#[test]
fn contrast_on_the_card_looks_like_the_cpu_one() {
    if !real_card() {
        return eprintln!("no graphics card here: the shader comparison is skipped");
    }
    for format in [PixelFormat::I420, PixelFormat::Rgba, PixelFormat::Nv12] {
        for (w, h) in [(1920usize, 1080usize), (1283, 721), (3840, 2160)] {
            if format == PixelFormat::Nv12 && w == 3840 {
                continue;
            }
            for (name, config) in [("auto", json!({"cleanup": "contrast"})), ("fill", json!({"cleanup": "contrast", "shape": "fill"})), ("a4", json!({"cleanup": "contrast", "shape": "a4"}))] {
                let picture = document(w, h);
                let (card, cpu) = cleaned_both(config.clone(), frame_of(&picture, format));
                let (plain_card, plain_cpu) = cleaned_both(json!({"shape": config["shape"].as_str().unwrap_or("auto")}), frame_of(&picture, format));
                let (max, mean, psnr, over) = difference(&card, &cpu);
                let (pmax, pmean, ..) = difference(&plain_card, &plain_cpu);
                println!("contrast {format:?} {w}x{h} {name}: max {max}, mean {mean:.3}, psnr {psnr:.1} dB, {:.4}% off by more than 3 (plain straightening: max {pmax}, mean {pmean:.3})", over * 100.0);
                if w == 1920 && name == "auto" {
                    save_png(&card, &format!("contrast-{format:?}-card"));
                    save_png(&cpu, &format!("contrast-{format:?}-cpu"));
                }
                assert!(mean < 1.0 && over < 0.005 && psnr > 40.0, "{format:?} {w}x{h} {name}: max {max}, mean {mean}, psnr {psnr}, over {over}");
            }
        }
    }
}

#[test]
fn black_and_white_on_the_card_looks_like_the_cpu_one() {
    if !real_card() {
        return eprintln!("no graphics card here: the shader comparison is skipped");
    }
    for format in [PixelFormat::I420, PixelFormat::Rgba, PixelFormat::Nv12] {
        for (w, h) in [(1920usize, 1080usize), (1283, 721), (3840, 2160)] {
            if format == PixelFormat::Nv12 && w == 3840 {
                continue;
            }
            for (name, config) in [("auto", json!({"cleanup": "bw"})), ("fill", json!({"cleanup": "bw", "shape": "fill"}))] {
                let picture = document(w, h);
                let (card, cpu) = cleaned_both(config, frame_of(&picture, format));
                let (max, mean, psnr, _) = difference(&card, &cpu);
                // Each pixel is ink or paper: what differs is pixels that sat on the threshold and went the other way
                let flipped = card.data.as_chunks::<3>().0.iter().zip(cpu.data.as_chunks::<3>().0).filter(|(a, b)| a[1].abs_diff(b[1]) > 64).count() as f64 / (w * h) as f64;
                println!("black and white {format:?} {w}x{h} {name}: max {max}, mean {mean:.3}, psnr {psnr:.1} dB, {:.4}% of pixels flipped", flipped * 100.0);
                if w == 1920 && name == "auto" {
                    save_png(&card, &format!("bw-{format:?}-card"));
                    save_png(&cpu, &format!("bw-{format:?}-cpu"));
                }
                assert!(mean < 1.0 && flipped < 0.002, "{format:?} {w}x{h} {name}: max {max}, mean {mean}, flipped {flipped}");
            }
        }
    }
}

#[test]
fn the_cleanup_passes_on_the_card_give_what_the_cpu_makes_of_the_same_sheet() {
    if !real_card() {
        return eprintln!("no graphics card here: the shader comparison is skipped");
    }
    // The straightening alone already differs a little between the two; this takes the card's own straightened sheet and cleans it up on the CPU
    for (w, h) in [(1920usize, 1080usize), (1283, 721), (3840, 2160)] {
        let frame = frame_of(&document(w, h), PixelFormat::Rgba);
        for shape in ["auto", "fill"] {
            let (plain, _) = cleaned_both(json!({"shape": shape}), frame.clone());
            let mut p = plugin(json!({"shape": shape}));
            let Some(FrameStep::Either(_, mut cpu)) = p.frame_step() else { panic!("a step") };
            let rect = cpu.card_uniforms(w as u32, h as u32).unwrap()["rect"].clone();
            let [x, y, rw, rh] = [0, 1, 2, 3].map(|i| rect[i].as_u64().unwrap() as usize);
            let crop = |rgb: &Rgb| -> Vec<u8> { (y..y + rh).flat_map(|r| rgb.data[(r * w + x) * 3..(r * w + x + rw) * 3].to_vec()).collect() };
            let sheet = crop(&plain);
            for (cleanup, cpu_way) in [("contrast", sheet::whiten as fn(&[u8], usize, usize) -> Vec<u8>), ("bw", sheet::black_and_white)] {
                let (card, _) = cleaned_both(json!({"shape": shape, "cleanup": cleanup}), frame.clone());
                let (card, want) = (crop(&card), cpu_way(&sheet, rw, rh));
                let diffs: Vec<u8> = card.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).collect();
                let max = *diffs.iter().max().unwrap();
                let mean = diffs.iter().map(|d| *d as f64).sum::<f64>() / diffs.len() as f64;
                let off = diffs.iter().filter(|d| **d > 0).count() as f64 / diffs.len() as f64;
                println!("{cleanup} passes {w}x{h} {shape}: max {max}, mean {mean:.4}, {:.4}% of values not exact", off * 100.0);
                if cleanup == "contrast" {
                    assert!(max <= 3 && mean < 0.01, "{cleanup} {w}x{h} {shape}: max {max}, mean {mean}");
                } else {
                    assert!(off < 0.0005, "{cleanup} {w}x{h} {shape}: {off} of values differ");
                }
            }
        }
    }
}

/// The mean of the first plane (luma for YUV), which a dark frame would pull down.
fn mean_luma(f: &Frame) -> f64 {
    let n = (f.width * f.height) as usize;
    f.data[..n].iter().step_by(5).map(|&v| v as f64).sum::<f64>() / n.div_ceil(5) as f64
}

#[test]
fn the_cleanup_never_flashes_dark_through_settings_and_the_balance_moving_it() {
    if !real_card() {
        return eprintln!("no graphics card here: the flash check is skipped");
    }
    let (w, h) = (640, 360);
    let frame = frame_of(&document(w, h), PixelFormat::I420);
    let balance = Arc::new(Balance::free());
    let mut pipeline = Pipeline::with_balance(balance.clone());
    let set = |pipeline: &mut Pipeline, config: Value| {
        let step = plugin(config).frame_step();
        pipeline.set_steps(step.into_iter().map(|step| StagedStep { owner: NAME.into(), stage: FrameStage::Geometry, step }).collect());
    };
    let mut means = Vec::new();
    let mut run = |pipeline: &mut Pipeline, n: usize| (0..n).for_each(|_| means.push(mean_luma(&pipeline.process(frame.clone(), &Ctx))));
    // plain, Contrast, Black and white and back, a new shape, then the card gets busy and the step moves to the CPU, then it's free again
    set(&mut pipeline, json!({}));
    run(&mut pipeline, 3);
    set(&mut pipeline, json!({"cleanup": "contrast"}));
    run(&mut pipeline, 3);
    set(&mut pipeline, json!({"cleanup": "bw"}));
    run(&mut pipeline, 3);
    set(&mut pipeline, json!({"cleanup": "contrast", "shape": "a4"}));
    run(&mut pipeline, 3);
    // four settings of three frames each ran on the card
    let on_card = 12;
    (0..8).for_each(|_| pipeline.lane().note_wait(50_000_000));
    assert_eq!(pipeline.lane().place(), Place::Cpu);
    run(&mut pipeline, 3);
    set(&mut pipeline, json!({"cleanup": "bw", "shape": "a4"}));
    run(&mut pipeline, 3);
    let (lo, hi) = means.iter().fold((f64::MAX, 0.0f64), |(l, h), &m| (l.min(m), h.max(m)));
    let steps = means.windows(2).map(|p| (p[0] - p[1]).abs()).collect::<Vec<_>>();
    eprintln!("desk cleanup luma across the run: {lo:.1} to {hi:.1}; card to CPU step {:.2}; {means:.1?}", steps[on_card - 1]);
    assert!(lo > 60.0, "a frame went dark: {means:?}");
    // Moving between the card and the CPU with the same setting changes nothing anyone sees
    assert!(steps[on_card - 1] < 1.0, "the picture jumped when the step moved to the CPU: {means:?}");
    // Within one setting, frames stay the same
    for run in means.chunks(3) {
        assert!(run.windows(2).all(|p| (p[0] - p[1]).abs() < 0.5), "frames within one setting moved: {means:?}");
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_desk_cleanup_on_the_card_and_the_cpu() {
    if !real_card() {
        return eprintln!("no graphics card here");
    }
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let frame = frame_of(&document(w, h), PixelFormat::I420);
        for cleanup in ["contrast", "bw"] {
            for place in [Place::Cpu, Place::Card] {
                let mut pipeline = Pipeline::with_balance(Arc::new(Balance::pinned_to(place)));
                pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Geometry, step: plugin(json!({"cleanup": cleanup})).frame_step().unwrap() }]);
                for _ in 0..6 {
                    pipeline.process(frame.clone(), &Ctx);
                }
                let mut rounds: Vec<f64> = (0..12).map(|_| {
                    let start = std::time::Instant::now();
                    for _ in 0..8 {
                        std::hint::black_box(pipeline.process(frame.clone(), &Ctx));
                    }
                    start.elapsed().as_secs_f64() * 1000.0 / 8.0
                }).collect();
                rounds.sort_by(|a, b| a.partial_cmp(b).unwrap());
                println!("desk {cleanup} {w}x{h} on the {place:?}: {:.2} ms a frame (median of 12 rounds of 8)", rounds[6]);
            }
        }
    }
}

#[test]
fn the_desk_on_the_card_never_darkens_black_or_white_yuv_frames() {
    if !real_card() {
        eprintln!("no graphics card here: the shader comparison is skipped");
        return;
    }
    let (w, h) = (640usize, 360usize);
    for (name, luma) in [("black", 16u8), ("below black", 0), ("white", 235), ("above white", 255)] {
        let mut frame = Frame::new(w as u32, h as u32, PixelFormat::I420, vec![128; PixelFormat::I420.size(w as u32, h as u32)]);
        frame.data_mut()[..w * h].fill(luma);
        let (card, cpu) = both_of(json!({"shape": "fill"}), frame);
        let (max, mean, ..) = difference(&card, &cpu);
        assert!(max <= 1 && mean < 0.1, "{name}: max {max}, mean {mean}");
    }
}
