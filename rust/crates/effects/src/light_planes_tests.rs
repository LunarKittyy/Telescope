use super::*;
use crate::cpu::Rgb;
use crate::fun_effects::{Effect, Inputs};

const W: usize = 384;
const H: usize = 288;

/// A dim, photo-like room with grain, a lamp-lit patch of wall and a bright blob at x.
fn scene(w: usize, h: usize, blob: usize) -> Rgb {
    let mut r = Rgb::new(w, h);
    let mut n = 7u32;
    for y in 0..h {
        for x in 0..w {
            n = n.wrapping_mul(1664525).wrapping_add(1013904223);
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let wall = [70.0 + 50.0 * fx, 90.0 + 40.0 * fy, 120.0 - 40.0 * fx];
            let lamp = ((fx - 0.8).powi(2) + (fy - 0.2).powi(2)).sqrt() < 0.07;
            let base = if lamp { [250.0, 245.0, 200.0] } else { wall };
            let grain = (n >> 29) as f32 - 3.0;
            r.set(x, y, base.map(|c| (c + grain).clamp(0.0, 255.0) as u8));
        }
    }
    let (cx, cy, rad) = (blob as f32, h as f32 * 0.6, w as f32 * 0.04);
    for y in 0..h {
        for x in 0..w {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            if d < rad {
                r.set(x, y, [255, (200.0 + d) as u8, (60.0 + 4.0 * d) as u8]);
            }
        }
    }
    r
}

fn frame_of(rgb: &Rgb, format: PixelFormat) -> Frame {
    let mut f = Frame::new(rgb.w as u32, rgb.h as u32, format, vec![0; format.size(rgb.w as u32, rgb.h as u32)]);
    rgb.write_to(&mut f);
    f
}

fn look() -> Look {
    Look { effect: "light".into(), fade: 6, brightness: 3, ..Look::default() }
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

/// What the picture becomes through RGB, the way light painting used to run.
fn through_rgb(e: &mut Effect, f: &mut Frame, look: &Look) {
    let rgb = Rgb::from_frame(f).unwrap();
    e.run(&rgb, look, Inputs { mask: None }).write_to(f);
}

#[test]
fn light_painting_on_the_planes_looks_like_light_painting_through_rgb() {
    for format in [PixelFormat::I420, PixelFormat::Nv12] {
        let (mut planes, mut rgb) = (Effect::named("light").unwrap(), Effect::named("light").unwrap());
        let mut worst = 0u8;
        for step in 0..14 {
            let picture = scene(W, H, 40 + step * 20);
            let (mut a, mut b) = (frame_of(&picture, format), frame_of(&picture, format));
            assert!(planes.run_frame(&mut a, &look()).is_some());
            through_rgb(&mut rgb, &mut b, &look());
            let worst_now = a.data.iter().zip(b.data.iter()).map(|(&x, &y)| x.abs_diff(y)).max().unwrap();
            worst = worst.max(worst_now);
            let p = psnr(&a.data, &b.data);
            assert!(p > 45.0, "{format:?} step {step}: {p:.1} dB, worst {worst_now}");
        }
        assert!(worst <= 6, "{format:?}: worst difference {worst}");
    }
}

#[test]
fn a_frame_with_nothing_lit_and_no_trail_is_left_exactly_as_it_was() {
    let dark = Rgb::filled(W, H, [30, 40, 50]);
    let mut f = frame_of(&dark, PixelFormat::I420);
    let before = f.data.clone();
    let mut e = Effect::named("light").unwrap();
    assert!(e.run_frame(&mut f, &look()).is_some());
    // not even copied: the pixels are still the one held here
    assert!(std::sync::Arc::ptr_eq(&before, &f.data));
}

#[test]
fn the_trail_leaves_the_frame_when_it_has_faded_and_the_work_goes_with_it() {
    let lit = scene(W, H, 100);
    let dark = Rgb::filled(W, H, [10, 10, 10]);
    let mut e = Effect::named("light").unwrap();
    let mut f = frame_of(&lit, PixelFormat::I420);
    e.run_frame(&mut f, &look());
    let spans = |e: &Effect| match e {
        Effect::Light(l) => l.spans.iter().filter(|(a, b)| a < b).count(),
        _ => unreachable!(),
    };
    assert!(spans(&e) > 0);
    for _ in 0..400 {
        let mut f = frame_of(&dark, PixelFormat::I420);
        e.run_frame(&mut f, &look());
    }
    assert_eq!(spans(&e), 0);
    let mut f = frame_of(&dark, PixelFormat::I420);
    let held = f.data.clone();
    e.run_frame(&mut f, &look());
    assert!(std::sync::Arc::ptr_eq(&held, &f.data));
}

#[test]
fn a_trail_made_through_rgb_carries_on_on_the_planes_and_a_new_size_starts_afresh() {
    let mut e = Effect::named("light").unwrap();
    let rgb = scene(W, H, 100);
    e.run(&rgb, &look(), Inputs { mask: None });
    let mut f = frame_of(&Rgb::filled(W, H, [10, 10, 10]), PixelFormat::I420);
    assert!(e.run_frame(&mut f, &look()).is_some());
    // the blob's trail is still there a frame later
    let at = f.data[(H * 6 / 10) * W + 100];
    assert!(at > 100, "luma {at}");
    let mut other = frame_of(&Rgb::filled(W / 2, H / 2, [10, 10, 10]), PixelFormat::I420);
    let held = other.data.clone();
    e.run_frame(&mut other, &look());
    assert!(std::sync::Arc::ptr_eq(&held, &other.data));
}

#[test]
fn frames_it_cannot_work_on_are_left_to_the_rgb_way() {
    let mut e = Effect::named("light").unwrap();
    let mut odd = frame_of(&Rgb::filled(161, 91, [10, 10, 10]), PixelFormat::I420);
    assert!(e.run_frame(&mut odd, &look()).is_none());
    let mut packed = frame_of(&Rgb::filled(64, 48, [10, 10, 10]), PixelFormat::Rgba);
    assert!(e.run_frame(&mut packed, &look()).is_none());
}

#[test]
fn a_cell_lit_only_through_saturated_chroma_is_still_painted() {
    // Y 151 with U 16 and V 61 is about RGB (50, 255, 0): grey 165 is over the level of 160 although the luma is under it
    let (w, h) = (64, 64);
    let mut data = vec![151u8; w * h];
    data.extend(vec![16u8; w * h / 4]);
    data.extend(vec![61u8; w * h / 4]);
    let look = Look { effect: "light".into(), fade: 6, brightness: 1, ..Look::default() };
    let dark = |f: &mut Frame| {
        let n = w * h;
        let d = f.data_mut();
        d[..n].fill(16);
        d[n..].fill(128);
    };
    let (mut planes, mut rgb) = (Effect::named("light").unwrap(), Effect::named("light").unwrap());
    let (mut fa, mut fb) = (Frame::new(w as u32, h as u32, PixelFormat::I420, data.clone()), Frame::new(w as u32, h as u32, PixelFormat::I420, data));
    assert!(planes.run_frame(&mut fa, &look).is_some());
    through_rgb(&mut rgb, &mut fb, &look);
    // the next frame is black, so only the trail is left in it
    dark(&mut fa);
    dark(&mut fb);
    assert!(planes.run_frame(&mut fa, &look).is_some());
    through_rgb(&mut rgb, &mut fb, &look);
    assert!(fb.data[..w * h].iter().any(|&y| y > 100), "the reference holds no trail");
    let worst = fa.data.iter().zip(fb.data.iter()).map(|(&a, &b)| a.abs_diff(b)).max().unwrap();
    assert!(worst <= 3, "differs by {worst}");
}

#[test]
#[ignore = "timing bench, run with --release -- --ignored --nocapture"]
fn bench_light_planes() {
    for (w, h) in [(1920usize, 1080usize), (3840, 2160)] {
        let pictures: Vec<Rgb> = (0..6).map(|i| scene(w, h, w / 8 + i * w / 16)).collect();
        let frames: Vec<Frame> = pictures.iter().map(|p| frame_of(p, PixelFormat::I420)).collect();
        let time = |planes: bool| {
            let mut e = Effect::named("light").unwrap();
            let mut best = f64::MAX;
            for round in 0..4 {
                for src in &frames {
                    let mut f = src.clone();
                    f.data_mut();
                    let start = std::time::Instant::now();
                    if planes {
                        e.run_frame(&mut f, &look()).unwrap();
                    } else {
                        through_rgb(&mut e, &mut f, &look());
                    }
                    if round > 0 {
                        best = best.min(start.elapsed().as_secs_f64() * 1000.0);
                    }
                }
            }
            best
        };
        let (mut a, mut b) = (Effect::named("light").unwrap(), Effect::named("light").unwrap());
        let mut worst = (0u8, f64::MAX);
        for src in &frames {
            let (mut fa, mut fb) = (src.clone(), src.clone());
            a.run_frame(&mut fa, &look()).unwrap();
            through_rgb(&mut b, &mut fb, &look());
            worst = (worst.0.max(fa.data.iter().zip(fb.data.iter()).map(|(&x, &y)| x.abs_diff(y)).max().unwrap()), worst.1.min(psnr(&fa.data, &fb.data)));
        }
        println!("light {w}x{h}: worst difference {} levels, lowest {:.1} dB", worst.0, worst.1);
        println!("light step {w}x{h}: through rgb {:.2} ms, on the planes {:.2} ms", time(false), time(true));
    }
}
