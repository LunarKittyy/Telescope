//! How long one 1080p picture takes to pack, for `cargo test --release -p telescope-effects pack_speed -- --ignored --nocapture`.

use super::*;
use std::time::Instant;
use telescope_core::frame::PixelFormat;

fn eye(seed: u8, format: PixelFormat) -> Frame {
    let (w, h) = (1920u32, 1080u32);
    let data: Vec<u8> = (0..format.size(w, h)).map(|i| (i as u32).wrapping_mul(2654435761).wrapping_add(seed as u32).to_le_bytes()[2]).collect();
    Frame::new(w, h, format, data)
}

fn lut() -> Arc<Lut> {
    Arc::new(Lut(std::array::from_fn(|c| std::array::from_fn(|i| ((i as f32 / 255.0).powf(0.9 + c as f32 * 0.05) * 255.0) as u8))))
}

fn time(name: &str, look: Look, tune: &Tune, format: PixelFormat) {
    let (l, r) = (eye(1, format), eye(2, format));
    let like = l.clone();
    for _ in 0..5 {
        render(&l, &r, look, (1920, 1080), tune, &like).unwrap();
    }
    let mut times: Vec<f64> = (0..60)
        .map(|_| {
            let t = Instant::now();
            let out = render(&l, &r, look, (1920, 1080), tune, &like).unwrap();
            std::hint::black_box(&out);
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(|a, b| a.total_cmp(b));
    println!("{name:<34} {format:?}: p50 {:.2} ms, p90 {:.2} ms, max {:.2} ms", times[30], times[54], times[59]);
}

#[test]
#[ignore]
fn pack_speed() {
    let plain = Tune::default();
    let everything = Tune { depth: 1.0, height: 0.5, size: 92.3, tilt: 1.2, lut: Some(lut()) };
    let sized = Tune { size: 92.3, lut: Some(lut()), ..Tune::default() };
    for format in [PixelFormat::I420, PixelFormat::Rgba] {
        time("side by side, plain", Look::SideBySide, &plain, format);
        time("side by side, size + lut", Look::SideBySide, &sized, format);
        time("side by side, everything", Look::SideBySide, &everything, format);
        time("red/cyan, everything", Look::RedCyan, &everything, format);
    }
}
