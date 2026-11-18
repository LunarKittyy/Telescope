use super::*;

const W: usize = 192;
const H: usize = 160;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }
}

const STARS: [(f32, f32, f32, f32); 6] = [(50.0, 40.0, 3.0, 0.9), (120.0, 60.0, 2.5, 0.7), (80.0, 110.0, 4.0, 0.8), (150.0, 120.0, 2.0, 0.6), (30.0, 130.0, 3.5, 0.5), (100.0, 30.0, 2.2, 0.75)];

/// Brightness 0..1 of the star field at x, y when the picture is moved by (dx, dy); blur widens every star.
fn field(x: f32, y: f32, dx: f32, dy: f32, blur: f32) -> f32 {
    let mut v = 0.05;
    for &(sx, sy, s, a) in &STARS {
        let s = s + blur;
        let (ex, ey) = (x - sx - dx, y - sy - dy);
        v += a * (s * s).recip() * 3.0 * (-(ex * ex + ey * ey) / (2.0 * s * s)).exp() * 3.0;
    }
    v.min(1.0)
}

fn rgba(mut f: impl FnMut(usize, usize) -> [f32; 3]) -> Frame {
    let mut data = Vec::with_capacity(W * H * 4);
    for y in 0..H {
        for x in 0..W {
            let p = f(x, y);
            data.extend(p.iter().map(|v| (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8));
            data.push(255);
        }
    }
    Frame::new(W as u32, H as u32, PixelFormat::Rgba, data)
}

fn shifted(dx: f32, dy: f32, blur: f32, noise: f32, rng: &mut Lcg) -> Frame {
    rgba(|x, y| {
        let v = field(x as f32, y as f32, dx, dy, blur);
        let mut n = || (rng.next() - 0.5) * 2.0 * noise;
        [v + n(), v * 0.9 + n(), v * 0.8 + n()]
    })
}

fn plane(frame: &Frame) -> Plane {
    Plane::from_frame(frame, 1).unwrap()
}

fn error_to(result: &Frame, clean: &Frame, margin: usize) -> f32 {
    let (mut sum, mut n) = (0.0, 0);
    for y in margin..H - margin {
        for x in margin..W - margin {
            let i = (y * W + x) * 4;
            for c in 0..3 {
                let d = result.data[i + c] as f32 - clean.data[i + c] as f32;
                sum += d * d;
                n += 1;
            }
        }
    }
    (sum / n as f32).sqrt()
}

#[test]
fn finds_integer_shifts_exactly() {
    let mut rng = Lcg(1);
    let a = plane(&shifted(0.0, 0.0, 0.0, 0.02, &mut rng));
    let b = plane(&shifted(7.0, -3.0, 0.0, 0.02, &mut rng));
    let s = shift_between(&a, &b).unwrap();
    assert!((s.dx - 7.0).abs() < 0.15 && (s.dy + 3.0).abs() < 0.15, "{s:?}");
    assert!(s.confidence > 10.0, "{s:?}");
}

#[test]
fn finds_a_sub_pixel_shift() {
    let mut rng = Lcg(2);
    let a = plane(&shifted(0.0, 0.0, 0.0, 0.01, &mut rng));
    let b = plane(&shifted(2.5, 1.25, 0.0, 0.01, &mut rng));
    let s = shift_between(&a, &b).unwrap();
    assert!((s.dx - 2.5).abs() < 0.2 && (s.dy - 1.25).abs() < 0.2, "{s:?}");
}

#[test]
fn small_planes_have_no_shift() {
    let p = Plane { width: 40, height: 40, data: vec![0.5; 1600] };
    assert!(shift_between(&p, &p).is_none());
}

#[test]
fn noise_has_low_confidence() {
    let mut rng = Lcg(3);
    let a = plane(&shifted(0.0, 0.0, 0.0, 0.01, &mut rng));
    let real = shift_between(&a, &plane(&shifted(4.0, 2.0, 0.0, 0.01, &mut rng))).unwrap();
    let noise = |rng: &mut Lcg| Plane { width: W, height: H, data: (0..W * H).map(|_| rng.next()).collect() };
    let (n1, n2) = (noise(&mut rng), noise(&mut rng));
    let fake = shift_between(&n1, &n2).unwrap();
    assert!(fake.confidence < real.confidence / 3.0, "{fake:?} vs {real:?}");
    assert!(fake.confidence < Stacker::new(StackMode::Mean).min_confidence, "{fake:?}");
}

#[test]
fn blur_lowers_sharpness() {
    let mut rng = Lcg(4);
    let sharp = sharpness(&plane(&shifted(0.0, 0.0, 0.0, 0.0, &mut rng)));
    let soft = sharpness(&plane(&shifted(0.0, 0.0, 3.0, 0.0, &mut rng)));
    assert!(soft < sharp, "{soft} vs {sharp}");
}

#[test]
fn mean_stack_is_cleaner_and_aligned() {
    let mut rng = Lcg(5);
    let clean = shifted(0.0, 0.0, 0.0, 0.0, &mut rng);
    let first = shifted(0.0, 0.0, 0.0, 0.1, &mut rng);
    let single = error_to(&first, &clean, 20);
    let mut stacker = Stacker::new(StackMode::Mean);
    stacker.add(&first).unwrap();
    for i in 1..16 {
        let (dx, dy) = ((i % 5) as f32 * 2.0 - 4.0, (i % 3) as f32 * 3.0 - 3.0);
        let s = stacker.add(&shifted(dx, dy, 0.0, 0.1, &mut rng)).unwrap_or_else(|| panic!("frame {i} refused"));
        assert!((s.dx - dx).abs() < 0.3 && (s.dy - dy).abs() < 0.3, "{s:?} for {dx}, {dy}");
    }
    assert_eq!(stacker.count(), 16);
    let result = stacker.result().unwrap();
    let stacked = error_to(&result, &clean, 20);
    assert!(stacked < single * 0.5, "{stacked} vs {single}");
    // the brightest star sits where the reference has it
    let (sx, sy) = (STARS[0].0 as usize, STARS[0].1 as usize);
    let at = |f: &Frame, x: usize, y: usize| f.data[(y * W + x) * 4] as i32;
    assert!((at(&result, sx, sy) - at(&clean, sx, sy)).abs() < 30);
}

fn dot_frame(x: usize) -> Frame {
    rgba(|px, py| if (px as i32 - x as i32).abs() <= 1 && (py as i32 - 80).abs() <= 1 { [1.0; 3] } else { [0.1; 3] })
}

#[test]
fn max_keeps_the_trail() {
    let mut stacker = Stacker::new(StackMode::Max);
    stacker.set_align(false);
    for x in (20..120).step_by(10) {
        assert!(stacker.add(&dot_frame(x)).is_some());
    }
    let result = stacker.result().unwrap();
    for x in (20..120).step_by(10) {
        assert_eq!(result.data[(80 * W + x) * 4], 255, "x {x}");
    }
    assert!(result.data[(80 * W + 150) * 4] < 60);
}

#[test]
fn clip_removes_a_streak_and_a_hot_pixel() {
    let mut rng = Lcg(6);
    let mut stacker = Stacker::new(StackMode::Clip { kappa: 2.5 });
    for i in 0..10 {
        let mut f = shifted(0.0, 0.0, 0.0, 0.03, &mut rng);
        if i == 7 {
            let d = f.data_mut();
            d[(100 * W + 100) * 4..][..3].fill(255);
            for x in 20..60 {
                d[(30 * W + x) * 4..][..3].fill(255);
            }
        }
        stacker.add(&f).unwrap();
    }
    let result = stacker.result().unwrap();
    let clean = shifted(0.0, 0.0, 0.0, 0.0, &mut Lcg(0));
    let near = |x: usize, y: usize| (result.data[(y * W + x) * 4] as i32 - clean.data[(y * W + x) * 4] as i32).abs();
    assert!(near(100, 100) < 25, "hot pixel {}", near(100, 100));
    assert!(near(40, 30) < 25, "streak {}", near(40, 30));
}

#[test]
fn lucky_keeps_only_the_sharpest() {
    let mut rng = Lcg(7);
    let mut lucky = Lucky::new(2);
    let blurs = [3.0, 0.0, 4.0, 5.0, 0.5, 6.0];
    let kept: Vec<bool> = blurs.iter().map(|&b| lucky.add(shifted(0.0, 0.0, b, 0.01, &mut rng))).collect();
    assert_eq!(kept, vec![true, true, false, false, true, false]);
    let best = lucky.best();
    assert_eq!(best.len(), 2);
    assert!(sharpness(&plane(&best[0])) > sharpness(&plane(&best[1])));
    assert_eq!(Lucky::new(0).best().len(), 0);
    let mut one = Lucky::new(0);
    one.add(shifted(0.0, 0.0, 0.0, 0.0, &mut rng));
    one.add(shifted(0.0, 0.0, 0.0, 0.0, &mut rng));
    assert_eq!(one.best().len(), 1);
}

#[test]
fn another_size_is_refused() {
    let mut rng = Lcg(8);
    let mut stacker = Stacker::new(StackMode::Mean);
    stacker.add(&shifted(0.0, 0.0, 0.0, 0.02, &mut rng)).unwrap();
    let small = Frame::new(100, 100, PixelFormat::Rgba, vec![128; 100 * 100 * 4]);
    assert!(stacker.add(&small).is_none());
    assert_eq!(stacker.count(), 1);
}

#[test]
fn a_dark_or_flat_first_frame_is_refused() {
    let mut rng = Lcg(9);
    let mut stacker = Stacker::new(StackMode::Mean);
    assert!(stacker.add(&rgba(|_, _| [0.0; 3])).is_none());
    assert!(stacker.add(&rgba(|_, _| [0.5; 3])).is_none());
    let tiny = Frame::new(32, 32, PixelFormat::Rgba, vec![100; 32 * 32 * 4]);
    assert!(stacker.add(&tiny).is_none());
    assert_eq!(stacker.count(), 0);
    stacker.add(&shifted(0.0, 0.0, 0.0, 0.02, &mut rng)).unwrap();
    assert!(stacker.add(&shifted(3.0, 2.0, 0.0, 0.02, &mut rng)).is_some());
    assert_eq!(stacker.count(), 2);
}

#[test]
fn a_big_shift_does_not_wrap_the_wrong_way() {
    let blob = |cx: f32| rgba(|x, y| {
        let (ex, ey) = (x as f32 - cx, y as f32 - 80.0);
        [0.05 + 0.9 * (-(ex * ex + ey * ey) / 18.0).exp(); 3]
    });
    let s = shift_between(&plane(&blob(40.0)), &plane(&blob(115.0))).unwrap();
    assert!((s.dx - 75.0).abs() < 4.0 && s.dy.abs() < 1.0, "{s:?}");
}

#[test]
fn changing_mode_or_align_starts_over() {
    let mut rng = Lcg(11);
    let mut stacker = Stacker::new(StackMode::Mean);
    stacker.add(&shifted(0.0, 0.0, 0.0, 0.02, &mut rng)).unwrap();
    stacker.set_mode(StackMode::Clip { kappa: 2.5 });
    assert_eq!(stacker.count(), 0);
    assert!(stacker.result().is_none());
    stacker.add(&shifted(0.0, 0.0, 0.0, 0.02, &mut rng)).unwrap();
    stacker.add(&shifted(0.0, 0.0, 0.0, 0.02, &mut rng)).unwrap();
    stacker.set_align(false);
    assert_eq!(stacker.count(), 0);
    stacker.add(&shifted(0.0, 0.0, 0.0, 0.02, &mut rng)).unwrap();
    stacker.set_align(true);
    assert_eq!(stacker.count(), 0);
    assert!(stacker.add(&shifted(0.0, 0.0, 0.0, 0.02, &mut rng)).is_some());
    assert!(stacker.align() && stacker.mode() == StackMode::Clip { kappa: 2.5 });
}

#[test]
fn malformed_frames_are_refused_without_align() {
    let mut stacker = Stacker::new(StackMode::Mean);
    stacker.set_align(false);
    let mut short = Frame::new(W as u32, H as u32, PixelFormat::Rgba, vec![9; W * H * 4]);
    short.data_mut().truncate(W * (H - 1) * 4);
    assert!(stacker.add(&short).is_none());
    assert!(stacker.add(&Frame::new(0, 0, PixelFormat::Nv12, Vec::new())).is_none());
    assert!(stacker.add(&Frame::new(0, 8, PixelFormat::Rgba, Vec::new())).is_none());
    assert_eq!(stacker.count(), 0);
}

#[test]
fn clip_restarts_after_an_exposure_change() {
    let mut stacker = Stacker::new(StackMode::Clip { kappa: 2.5 });
    stacker.set_align(false);
    let level = |v: f32| rgba(|_, _| [v; 3]);
    for _ in 0..6 {
        stacker.add(&level(0.3)).unwrap();
    }
    assert_eq!(stacker.count(), 6);
    stacker.add(&level(0.5)).unwrap();
    assert_eq!(stacker.count(), 1);
    for _ in 0..3 {
        stacker.add(&level(0.5)).unwrap();
    }
    let result = stacker.result().unwrap();
    assert!((result.data[0] as i32 - 128).abs() <= 1, "{}", result.data[0]);
}

#[test]
fn short_plane_data_has_no_shift() {
    let empty = Plane { width: 64, height: 64, data: Vec::new() };
    assert!(shift_between(&empty, &empty).is_none());
}

/// A dark 1024x576 frame with twenty clean 3x3 stars at quarter brightness, moved by (dx, dy).
fn faint_sky(dx: usize, dy: usize) -> Frame {
    let (w, h) = (1024, 576);
    let mut rng = Lcg(21);
    let stars: Vec<(usize, usize)> = (0..20).map(|_| (40 + (rng.next() * 900.0) as usize, 40 + (rng.next() * 480.0) as usize)).collect();
    let mut data = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let lit = stars.iter().any(|&(sx, sy)| (sx + dx..sx + dx + 3).contains(&x) && (sy + dy..sy + dy + 3).contains(&y));
            let v = if lit { 0.25 } else { 0.02 };
            data.extend([(v * 255.0 + 0.5) as u8, (v * 255.0 + 0.5) as u8, (v * 255.0 + 0.5) as u8, 255]);
        }
    }
    Frame::new(w as u32, h as u32, PixelFormat::Rgba, data)
}

#[test]
fn a_faint_sky_of_a_few_small_stars_can_start_a_stack() {
    let first = faint_sky(0, 0);
    // Whole-frame contrast is under the old threshold, so only a local test finds the stars
    let whole = plane(&first);
    let mean = whole.data.iter().sum::<f32>() / whole.data.len() as f32;
    let spread = (whole.data.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / whole.data.len() as f32).sqrt();
    assert!(spread < MIN_REFERENCE_SPREAD, "{spread}");
    let mut stacker = Stacker::new(StackMode::Mean);
    assert!(stacker.add(&first).is_some());
    assert!(stacker.add(&faint_sky(4, 3)).is_some(), "the next frame lines up with it");
    assert_eq!(stacker.count(), 2);
    // A black frame, a flat one and a smooth ramp have nothing to line up on
    let (w, h) = (1024usize, 576usize);
    let solid = |f: &dyn Fn(usize) -> u8| Frame::new(w as u32, h as u32, PixelFormat::Rgba, (0..w * h).flat_map(|i| { let v = f(i % w); [v, v, v, 255] }).collect());
    for frame in [solid(&|_| 0), solid(&|_| 120), solid(&|x| (x / 8) as u8)] {
        assert!(Stacker::new(StackMode::Mean).add(&frame).is_none());
    }
}

#[test]
fn a_shift_that_misses_the_stack_is_refused() {
    let mut rng = Lcg(31);
    let mut stacker = Stacker::new(StackMode::Mean);
    let frame = shifted(0.0, 0.0, 0.0, 0.01, &mut rng);
    stacker.add(&frame).unwrap();
    let rgb = frame_rgb(&frame).unwrap();
    assert!(stacker.take(&rgb, Shift { dx: 5000.0, dy: 0.0, confidence: 100.0 }).is_none());
    assert!(stacker.take(&rgb, Shift { dx: 0.0, dy: -(H as f32), confidence: 100.0 }).is_none());
    assert_eq!(stacker.count(), 1);
    assert!(stacker.take(&rgb, Shift { dx: 2.0, dy: 1.0, confidence: 100.0 }).is_some());
    assert_eq!(stacker.count(), 2);
}

#[test]
fn lucky_does_not_keep_a_malformed_frame() {
    let mut lucky = Lucky::new(3);
    let mut short = Frame::new(W as u32, H as u32, PixelFormat::Rgba, vec![200; W * H * 4]);
    short.data_mut().truncate(100);
    assert!(!lucky.add(short));
    assert!(!lucky.add(Frame::new(0, 0, PixelFormat::Rgba, Vec::new())));
    assert!(lucky.best().is_empty());
    let mut rng = Lcg(5);
    assert!(lucky.add(shifted(0.0, 0.0, 2.0, 0.01, &mut rng)));
    assert_eq!(lucky.best().len(), 1);
}

#[test]
fn lucky_hands_its_frames_over_sharpest_first() {
    let mut rng = Lcg(3);
    let mut lucky = Lucky::new(2);
    assert!(lucky.add(shifted(0.0, 0.0, 3.0, 0.01, &mut rng)) && lucky.add(shifted(0.0, 0.0, 0.0, 0.01, &mut rng)));
    let best = lucky.best();
    let moved = lucky.into_best();
    assert_eq!(moved.len(), 2);
    assert_eq!(moved.iter().map(|f| f.data.clone()).collect::<Vec<_>>(), best.iter().map(|f| f.data.clone()).collect::<Vec<_>>());
}
