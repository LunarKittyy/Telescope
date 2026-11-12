use super::*;
use crate::cpu::Rgb;
use crate::fun_effects::Crt;
use telescope_core::frame::{FrameStage, PixelFormat};
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

fn photo(w: usize, h: usize) -> Rgb {
    let mut rgb = Rgb::new(w, h);
    let mut n = 99u32;
    for y in 0..h {
        for x in 0..w {
            n = n.wrapping_mul(1664525).wrapping_add(1013904223);
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let inside = ((fx - 0.5).powi(2) + (fy - 0.6).powi(2)).sqrt() < 0.22;
            let base = if inside { [215.0, 170.0, 60.0 + 80.0 * fx] } else { [90.0 + 120.0 * fy, 140.0 + 80.0 * fy, 230.0 - 60.0 * fy] };
            rgb.set(x, y, base.map(|c| (c + (n >> 29) as f32 - 3.0).clamp(0.0, 255.0) as u8));
        }
    }
    rgb
}

fn rgba(rgb: &Rgb) -> Frame {
    let mut f = Frame::new(rgb.w as u32, rgb.h as u32, PixelFormat::Rgba, vec![0; rgb.w * rgb.h * 4]);
    rgb.write_to(&mut f);
    f
}

fn on_the_card() -> impl FnMut(Frame) -> Frame {
    let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
    let step = FrameStep::Shader(ShaderStep { wgsl: CRT.into(), uniforms: json!({}), textures: Vec::new() });
    pipeline.set_steps(vec![StagedStep { owner: NAME.into(), stage: FrameStage::Effects, step }]);
    move |frame| pipeline.process(frame, &Ctx)
}

fn pattern(w: usize, h: usize, on: impl Fn(usize, usize) -> bool) -> Rgb {
    let mut rgb = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = if on(x, y) { 230 } else { 20 };
            rgb.set(x, y, [v, v, v]);
        }
    }
    rgb
}

fn difference(picture: Rgb) -> (u8, f64, f64, f64) {
    let (w, h) = (picture.w, picture.h);
    let card = Rgb::from_frame(&on_the_card()(rgba(&picture))).unwrap();
    let cpu = Crt::new(w, h).run(&picture);
    let diffs: Vec<u8> = card.data.iter().zip(&cpu.data).map(|(a, b)| a.abs_diff(*b)).collect();
    let mse = diffs.iter().map(|d| (*d as f64).powi(2)).sum::<f64>() / diffs.len() as f64;
    let over = diffs.iter().filter(|d| **d > 3).count() as f64 / diffs.len() as f64;
    let mean = diffs.iter().map(|d| *d as f64).sum::<f64>() / diffs.len() as f64;
    (*diffs.iter().max().unwrap(), 10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10(), mean, over)
}

#[test]
fn the_crt_on_the_card_looks_like_the_one_on_the_cpu() {
    if telescope_media::gpu::Gpu::shared().is_none() {
        eprintln!("no graphics card here: the shader comparison is skipped");
        return;
    }
    for (w, h) in [(640usize, 360usize), (1920, 1080), (3840, 2160)] {
        let (max, psnr, mean, over) = difference(photo(w, h));
        println!("crt {w}x{h}: max {max}, mean {mean:.3}, psnr {psnr:.1} dB, {:.3}% of values off by more than 3", over * 100.0);
        assert!(psnr > 38.0 && mean < 1.0, "{w}x{h}: psnr {psnr}, mean {mean}");
    }
}

#[test]
fn crt_card_thin_lines_and_checks_glow_as_they_do_on_the_cpu() {
    if telescope_media::gpu::Gpu::shared().is_none() {
        eprintln!("no graphics card here: the shader comparison is skipped");
        return;
    }
    for (w, h) in [(640usize, 360usize), (1920, 1080)] {
        let pictures: [(&str, Rgb); 3] = [
            ("vertical stripes", pattern(w, h, |x, _| x % 2 == 0)),
            ("horizontal stripes", pattern(w, h, |_, y| y % 2 == 0)),
            ("checkerboard", pattern(w, h, |x, y| (x + y) % 2 == 0)),
        ];
        for (name, picture) in pictures {
            let (max, psnr, mean, over) = difference(picture);
            println!("crt {w}x{h} {name}: max {max}, mean {mean:.3}, psnr {psnr:.1} dB, {:.3}% of values off by more than 3", over * 100.0);
            // full-contrast 1-px lines are the worst case for the CPU's 1/32-pixel bend positions, so the bound is looser than for a picture
            assert!(psnr > 38.0 && mean < 1.5 && over < 0.05 && max <= 12, "{w}x{h} {name}: max {max}, psnr {psnr}, mean {mean}");
        }
    }
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_crt_on_the_card() {
    if telescope_media::gpu::Gpu::shared().is_none() {
        eprintln!("no graphics card here");
        return;
    }
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let frame = rgba(&photo(w, h));
        let mut run = on_the_card();
        run(frame.clone());
        let mut times: Vec<f64> = (0..9).map(|_| {
            let input = frame.clone();
            let start = std::time::Instant::now();
            std::hint::black_box(run(input));
            start.elapsed().as_secs_f64() * 1000.0
        }).collect();
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("crt on the card {w}x{h}: {:.2} ms through the pipeline, up and down included (median of 9)", times[4]);
    }
}
