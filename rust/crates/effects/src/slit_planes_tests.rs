use super::*;
use crate::cpu::Rgb;
use crate::fun_effects::{Effect, Inputs, Look};

const W: usize = 384;
const H: usize = 216;

/// A photo-like picture whose contents move with n, so older frames differ from newer ones.
fn scene(w: usize, h: usize, n: usize) -> Rgb {
    let mut r = Rgb::new(w, h);
    let mut g = 11u32;
    for y in 0..h {
        for x in 0..w {
            g = g.wrapping_mul(1664525).wrapping_add(1013904223);
            let (fx, fy) = ((x + n * 9) as f32 / w as f32, y as f32 / h as f32);
            let hue = [110.0 + 90.0 * (fx * 3.0).sin(), 120.0 + 80.0 * (fy * 4.0 + fx).cos(), 100.0 + 70.0 * (fx * 2.0 - fy).sin()];
            let grain = (g >> 29) as f32 - 3.0;
            r.set(x, y, hue.map(|c| (c + grain).clamp(0.0, 255.0) as u8));
        }
    }
    r
}

fn frame_of(rgb: &Rgb, format: PixelFormat) -> Frame {
    let mut f = Frame::new(rgb.w as u32, rgb.h as u32, format, vec![0; format.size(rgb.w as u32, rgb.h as u32)]);
    rgb.write_to(&mut f);
    f
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

fn look(across: bool, spread: i64) -> Look {
    Look { effect: "slit".into(), across, spread, ..Look::default() }
}

#[test]
fn slit_scan_on_the_planes_looks_like_slit_scan_through_rgb() {
    for format in [PixelFormat::I420, PixelFormat::Nv12] {
        for across in [false, true] {
            let l = look(across, 9);
            let (mut planes, mut rgb) = (Effect::named("slit").unwrap(), Effect::named("slit").unwrap());
            for step in 0..14 {
                let picture = scene(W, H, step);
                let (mut a, mut b) = (frame_of(&picture, format), frame_of(&picture, format));
                assert!(planes.run_frame(&mut a, &l).is_some());
                let input = Rgb::from_frame(&b).unwrap();
                rgb.run(&input, &l, Inputs { mask: None }).write_to(&mut b);
                let worst = a.data.iter().zip(b.data.iter()).map(|(&x, &y)| x.abs_diff(y)).max().unwrap();
                let p = psnr(&a.data, &b.data);
                assert!(p > 45.0 && worst <= 8, "{format:?} across {across} step {step}: {p:.1} dB, worst {worst}");
            }
        }
    }
}

#[test]
fn frames_it_cannot_work_on_are_left_to_the_rgb_way() {
    let mut e = Effect::named("slit").unwrap();
    let mut odd = frame_of(&scene(161, 91, 0), PixelFormat::I420);
    let before = odd.data.clone();
    assert!(e.run_frame(&mut odd, &look(false, 5)).is_none());
    assert!(odd.data == before);
    let mut packed = frame_of(&scene(64, 48, 0), PixelFormat::Rgba);
    assert!(e.run_frame(&mut packed, &look(false, 5)).is_none());
}

#[test]
fn a_new_size_starts_the_history_over() {
    let mut e = Effect::named("slit").unwrap();
    let l = look(false, 5);
    for n in 0..4 {
        e.run_frame(&mut frame_of(&scene(W, H, n), PixelFormat::I420), &l);
    }
    let mut f = frame_of(&scene(W / 2, H / 2, 9), PixelFormat::I420);
    let before = f.data.clone();
    e.run_frame(&mut f, &l);
    // with no older frames of this size, every band is the frame itself
    let worst = f.data.iter().zip(before.iter()).map(|(&x, &y)| x.abs_diff(y)).max().unwrap();
    assert!(worst <= 8, "worst {worst}");
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_slit_planes() {
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let frames: Vec<Frame> = (0..6).map(|n| frame_of(&scene(w, h, n), PixelFormat::I420)).collect();
        let l = look(false, 20);
        let time = |planes: bool| {
            let mut e = Effect::named("slit").unwrap();
            let mut best = f64::MAX;
            for round in 0..6 {
                for src in &frames {
                    let mut f = src.clone();
                    f.data_mut();
                    let start = std::time::Instant::now();
                    if planes {
                        e.run_frame(&mut f, &l).unwrap();
                    } else {
                        let rgb = Rgb::from_frame(&f).unwrap();
                        e.run(&rgb, &l, Inputs { mask: None }).write_to(&mut f);
                    }
                    if round > 0 {
                        best = best.min(start.elapsed().as_secs_f64() * 1000.0);
                    }
                }
            }
            best
        };
        let (mut a, mut b) = (Effect::named("slit").unwrap(), Effect::named("slit").unwrap());
        let mut worst = (0u8, f64::MAX);
        for src in frames.iter().cycle().take(12) {
            let (mut fa, mut fb) = (src.clone(), src.clone());
            a.run_frame(&mut fa, &l).unwrap();
            let rgb = Rgb::from_frame(&fb).unwrap();
            b.run(&rgb, &l, Inputs { mask: None }).write_to(&mut fb);
            worst = (worst.0.max(fa.data.iter().zip(fb.data.iter()).map(|(&x, &y)| x.abs_diff(y)).max().unwrap()), worst.1.min(psnr(&fa.data, &fb.data)));
        }
        println!("slit {w}x{h}: worst difference {} levels, lowest {:.1} dB", worst.0, worst.1);
        println!("slit step {w}x{h}: through rgb {:.2} ms, on the planes {:.2} ms", time(false), time(true));
    }
}
