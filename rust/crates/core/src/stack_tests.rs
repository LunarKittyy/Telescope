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
    stacker.align = false;
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
fn lucky_picks_the_sharp_frames() {
    let mut rng = Lcg(7);
    let frames: Vec<Frame> = (0..8).map(|i| shifted(0.0, 0.0, if i % 4 == 1 { 0.0 } else { 3.0 }, 0.01, &mut rng)).collect();
    let mut picked = lucky(&frames, 0.25);
    picked.sort();
    assert_eq!(picked, vec![1, 5]);
    assert_eq!(lucky(&frames, 0.0).len(), 1);
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
