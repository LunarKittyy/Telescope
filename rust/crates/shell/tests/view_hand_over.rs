// The picture's effective zoom (phone share x desktop crop) while a phone that applies a zoom late is steered.
use serde_json::{json, Value};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::View;
use telescope_core::view::{phone_window, window, window_centre, PhoneZoom};
use telescope_shell::view_control::{Said, ViewController};

const FPS: f64 = 30.0;

thread_local! {
    static NOISE: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
    /// A tagging phone loses the tag of every this many frames (0: none).
    static MISSING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn caps(lens: Option<f64>, crop_max: f64, tags: bool) -> Value {
    let mut cam = json!({"id": "0", "current": true, "zoomRatioMax": 10.0, "cropZoomMax": crop_max, "freeformCrop": true, "cropTags": tags});
    if let Some(l) = lens {
        cam["lensZooms"] = json!([l]);
    }
    json!({"cameras": [cam]})
}

/// What the phone sends for a share: that part of a scene with `amp` levels of texture, tagged with the share by a phone that does.
fn from_phone(share: &PhoneZoom, amp: f64, k: usize, tags: bool) -> Frame {
    let cell = |x: i64, y: i64| ((x.wrapping_mul(7_919) ^ y.wrapping_mul(104_729)).wrapping_mul(2_654_435_761) >> 13 & 255) as f64;
    let scene = |u: f64, v: f64| {
        let (x, y) = (u * 32.0, v * 18.0);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let top = cell(x0, y0) * (1.0 - fx) + cell(x0 + 1, y0) * fx;
        let bottom = cell(x0, y0 + 1) * (1.0 - fx) + cell(x0 + 1, y0 + 1) * fx;
        128.0 + (top * (1.0 - fy) + bottom * fy - 128.0) * amp
    };
    let (zoom, px, py) = phone_window(Some(share));
    let ((x0, w), (y0, h)) = (window(zoom, px), window(zoom, py));
    let (fw, fh) = (320u32, 180u32);
    // A head that sways about, and the sensor's noise
    let t = k as f64 / FPS;
    let head = (0.5 + 0.08 * (t * 1.3).sin(), 0.42 + 0.03 * (t * 0.9).sin());
    let data = (0..fw * fh).flat_map(|i| {
        let (u, v) = ((i % fw) as f64 / fw as f64, (i / fw) as f64 / fh as f64);
        let (x, y) = (x0 + u * w, y0 + v * h);
        let mut g = scene(x, y);
        let d = ((x - head.0).powi(2) + ((y - head.1) * 1.4).powi(2)).sqrt();
        if d < 0.12 {
            g = 70.0 + 40.0 * amp.min(1.0) * ((x * 90.0).sin() * (y * 70.0).cos());
        }
        let noise = ((i as u64 ^ (k as u64).wrapping_mul(0x9E37_79B9)).wrapping_mul(2_654_435_761) >> 11 & 7) as f64 - 3.5;
        let g = (g + noise * NOISE.with(|n| n.get())).clamp(0.0, 255.0) as u8;
        [g, g, g, 255]
    }).collect();
    let mut frame = Frame::new(fw, fh, PixelFormat::Rgba, data);
    let every = MISSING.with(|m| m.get());
    let lost = every > 0 && k.is_multiple_of(every);
    frame.phone_share = (tags && !lost).then_some(*share);
    frame
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    t: f64,
    /// The zoom the output really has over the zoom asked for.
    zoom_off: f64,
    /// How far its centre is from the asked one, in windows.
    centre_off: f64,
    /// The zoom the frame's crop label says the phone sent over the zoom it really sent.
    label_off: f64,
    /// How far the label's centre is from the real one, in windows of the real one.
    label_centre_off: f64,
    /// The frame says its label is a guess.
    unsure: bool,
}

struct Run {
    delay: f64,
    amp: f64,
    lens: Option<f64>,
    crop_max: f64,
    /// Seconds the phone takes to glide to a new zoom (0: it lands on one frame).
    glide: f64,
    /// Levels of sensor noise.
    noise: f64,
    /// The phone tags each frame with the crop its camera reported (a newer phone app).
    tags: bool,
}

/// Steers `framing(t)` for `seconds` against a phone that applies each zoom `delay` late, and measures every frame.
fn run(r: &Run, seconds: f64, framing: &impl Fn(f64) -> (f32, f32, f32)) -> Vec<Sample> {
    NOISE.with(|n| n.set(r.noise));
    let (mut vc, mut view) = (ViewController::new(), View::default());
    let start = 1000.0;
    vc.stream_start(&view, "p1", 1, start);
    vc.phone_state(&view, &caps(r.lens, r.crop_max, r.tags), start);
    vc.take("Auto framing");
    let gate = vc.gate();
    let mut actual = PhoneZoom { ratio: 1.0, crop: 1.0, x: 0.5, y: 0.5 };
    let mut pending: Vec<(f64, PhoneZoom)> = Vec::new();
    let mut gliding: Option<(f64, PhoneZoom, PhoneZoom)> = None;
    let mut target = actual;
    let mut in_place = vc.frame_view(&view);
    let mut label = vc.frame_window();
    let mut samples = Vec::new();
    let mut warmed = false;
    for k in 0..(seconds * FPS) as usize {
        let t = k as f64 / FPS;
        let now = start + t;
        let (z, px, py) = framing(t);
        // Nobody holds quite still
        let (z, px, py) = (z * (1.0 + 0.004 * (t * 5.1).sin() as f32), px + 0.004 * (t * 3.7).sin() as f32, py + 0.004 * (t * 4.3).cos() as f32);
        let said = vc.steer(&mut view, "Auto framing", z, px, py, now);
        let mut restep = false;
        for s in said {
            match s {
                Said::Zoom(p) => pending.push((if warmed { t + r.delay } else { t }, p)),
                Said::Restep => restep = true,
                _ => {}
            }
        }
        if t > 1.0 {
            warmed = true;
        }
        while let Some(i) = pending.iter().position(|(at, _)| *at <= t) {
            gliding = Some((t, actual, pending.remove(i).1));
            target = gliding.unwrap().2;
        }
        if let Some((from_t, from, to)) = gliding {
            let f = if r.glide <= 0.0 { 1.0 } else { ((t - from_t) / r.glide).min(1.0) };
            let mix = |a: f64, b: f64| a + (b - a) * f;
            actual = PhoneZoom { ratio: mix(from.ratio, to.ratio), crop: (from.crop.ln() + (to.crop.ln() - from.crop.ln()) * f).exp(), x: mix(from.x, to.x), y: mix(from.y, to.y) };
            if f >= 1.0 {
                actual = target;
                gliding = None;
            }
        }
        let frame = from_phone(&actual, r.amp, k, r.tags);
        if restep {
            in_place = vc.frame_view(&view);
            label = vc.frame_window();
        }
        // A frame held back isn't shown: the one before it stays up
        let Some(telescope_shell::handover::Checked { changed, unsure, .. }) = gate.for_frame_checked("p1", &frame, now, restep) else { continue };
        if let Some((v, crop)) = changed {
            (in_place, label) = (v, crop);
        }
        // The window the output shows, in the full view of the main lens
        let (pz, ppx, ppy) = phone_window(Some(&actual));
        let ((pw_x, pw_w), (pw_y, pw_h)) = (window(pz, ppx), window(pz, ppy));
        let (iz, ix, iy) = (in_place.zoom as f64, in_place.pan_x as f64, in_place.pan_y as f64);
        let eff = (pw_x + window_centre(iz.max(1.0), ix) * pw_w, pw_y + window_centre(iz.max(1.0), iy) * pw_h);
        let eff_zoom = pz * iz.max(1.0);
        let (wz, wx, wy) = (view.zoom as f64, view.pan_x as f64, view.pan_y as f64);
        let want = (window_centre(wz, wx), window_centre(wz, wy));
        let centre_off = ((eff.0 - want.0).abs().max((eff.1 - want.1).abs())) * wz;
        let ((lx, _), (ly, _)) = (window(label.0, label.1), window(label.0, label.2));
        let label_centre_off = ((lx + 0.5 / label.0 - (pw_x + 0.5 * pw_w)).abs().max((ly + 0.5 / label.0 - (pw_y + 0.5 * pw_h)).abs())) / pw_w;
        samples.push(Sample { t, zoom_off: eff_zoom / wz, centre_off, label_off: label.0 / pz, label_centre_off, unsure });
    }
    samples
}

fn report(name: &str, s: &[Sample]) {
    let s: Vec<&Sample> = s.iter().filter(|s| s.t > 3.0).collect();
    let worst = s.iter().map(|s| s.zoom_off.ln().abs()).fold(0.0, f64::max).exp();
    let max_off = s.iter().map(|s| s.zoom_off).fold(0.0, f64::max);
    let min_off = s.iter().map(|s| s.zoom_off).fold(f64::MAX, f64::min);
    let step = s.windows(2).map(|w| (w[1].zoom_off / w[0].zoom_off).ln().abs()).fold(0.0, f64::max).exp();
    let bad: Vec<f64> = s.iter().filter(|s| s.zoom_off.ln().abs() > 0.08).map(|s| s.t).collect();
    let centre = s.iter().map(|s| s.centre_off).fold(0.0, f64::max);
    let span = bad.last().zip(bad.first()).map_or(0.0, |(l, f)| l - f);
    let label = s.iter().map(|s| s.label_off.ln().abs()).fold(0.0, f64::max).exp();
    let label_centre = s.iter().map(|s| s.label_centre_off).fold(0.0, f64::max);
    eprintln!("{name}: label zoom worst x{label:.3}, centre {label_centre:.3}");
    eprintln!("{name}: zoom off x{min_off:.2}..x{max_off:.2} (worst x{worst:.2}), biggest frame step x{step:.2}, centre off {centre:.2} windows, {} frames off, first {:?} span {span:.1}s", bad.len(), bad.first());
}

/// A head that sways and drifts, framed at about `zoom`: the framing's own spring follows it smoothly.
fn swaying(zoom: f64) -> impl Fn(f64) -> (f32, f32, f32) {
    move |t| {
        let z = zoom * (0.06 * (t * 0.9).sin() + 0.04 * (t * 2.3).sin()).exp();
        let (cx, cy) = (0.5 + 0.12 * (t * 0.37).sin(), 0.42 + 0.05 * (t * 0.71).sin());
        let pan = |c: f64| telescope_core::view::pan_for(c, z);
        (z as f32, pan(cx) as f32, pan(cy) as f32)
    }
}

/// The head leans away (framing zooms in `from` to `to` over `ramp` seconds at `at`) and later back.
fn lean(from: f64, to: f64, at: f64, ramp: f64, hold: f64) -> impl Fn(f64) -> (f32, f32, f32) {
    move |t| {
        let k = |t: f64| ((t - at) / ramp).clamp(0.0, 1.0);
        let f = k(t) - k(t - ramp - hold);
        let z = (from.ln() + (to.ln() - from.ln()) * f).exp();
        let pan = telescope_core::view::pan_for(0.5, z);
        (z as f32, pan as f32, telescope_core::view::pan_for(0.42, z) as f32)
    }
}

/// The head moves sideways by `by` (of the picture) over `ramp` seconds at `at`, and back `hold` seconds later.
fn aside(zoom: f64, by: f64, at: f64, ramp: f64, hold: f64) -> impl Fn(f64) -> (f32, f32, f32) {
    move |t| {
        let k = |t: f64| ((t - at) / ramp).clamp(0.0, 1.0);
        let f = k(t) - k(t - ramp - hold);
        let c = 0.5 - by * f;
        (zoom as f32, telescope_core::view::pan_for(c, zoom) as f32, telescope_core::view::pan_for(0.42, zoom) as f32)
    }
}

/// The head moves up to `zoom` over a second at `at` and stays.
fn closer(from: f64, to: f64, at: f64) -> impl Fn(f64) -> (f32, f32, f32) {
    lean(from, to, at, 1.0, 1000.0)
}

fn step_of(s: &[Sample]) -> f64 {
    s.iter().filter(|s| s.t > 3.0).collect::<Vec<_>>().windows(2).map(|w| (w[1].zoom_off / w[0].zoom_off).ln().abs()).fold(0.0, f64::max).exp()
}

/// How far the picture's zoom jumps from one frame to the next, at worst, in each kind of scene and phone: for a phone that
/// lands a zoom on one frame, and for one that glides there (which can't be followed as closely).
fn steps_in(name: &str, f: impl Fn(f64) -> (f32, f32, f32), seconds: f64, tags: bool) -> (Vec<f64>, Vec<f64>) {
    let (mut sudden, mut gliding) = (Vec::new(), Vec::new());
    for (scene, amp, noise) in [("textured", 1.0, 0.0), ("busy", 0.3, 1.0), ("dull", 0.1, 1.0)] {
        for (delay, glide) in [(0.2, 0.0), (0.4, 0.5), (0.5, 1.0)] {
            let r = Run { delay, amp, lens: Some(2.1), crop_max: 4.0, glide, noise, tags };
            let samples = run(&r, seconds, &f);
            report(&format!("{name}, {scene}, phone {delay}s late gliding {glide}s, tags {tags}"), &samples);
            // A scene too dull to find the picture in, and a phone that glides there, can only be waited out, unless the frames say where they are
            if glide == 0.0 || tags {
                sudden.push(step_of(&samples));
            } else if scene != "dull" {
                gliding.push(step_of(&samples));
            }
        }
    }
    (sudden, gliding)
}

fn assert_steady((sudden, gliding): (Vec<f64>, Vec<f64>)) {
    assert!(sudden.iter().all(|s| *s < 1.05), "{sudden:?}");
    assert!(gliding.iter().all(|s| *s < 1.2), "{gliding:?}");
}

#[test]
fn a_lens_that_lets_go_doesnt_make_the_picture_jump() {
    // A phone that tags its frames gets no crop on the longer lens, and one that doesn't keeps getting one
    for tags in [true, false] {
        assert_steady(steps_in("person steps aside", aside(10.0, 0.2, 5.0, 1.0, 5.0), 14.0, tags));
    }
}

#[test]
fn zooming_in_on_a_person_doesnt_make_the_picture_jump() {
    for tags in [true, false] {
        assert_steady(steps_in("person leans away", closer(3.5, 10.0, 4.0), 20.0, tags));
    }
}

#[test]
fn a_swaying_head_doesnt_make_the_picture_jump() {
    for tags in [true, false] {
        assert_steady(steps_in("head sways", swaying(5.0), 30.0, tags));
    }
}

/// With tags, the picture follows even a phone that glides, in any scene: the frames say where they are.
#[test]
fn a_phone_that_tags_its_frames_keeps_the_picture_still_through_every_hand_over() {
    let framings: [(&str, Box<dyn Fn(f64) -> (f32, f32, f32)>, f64); 3] = [("person leans away", Box::new(closer(3.5, 10.0, 4.0)), 20.0),
        ("head sways", Box::new(swaying(5.0)), 30.0), ("person steps aside", Box::new(aside(10.0, 0.2, 5.0, 1.0, 5.0)), 14.0)];
    for (name, framing, seconds) in framings {
        let (sudden, _) = steps_in(name, framing, seconds, true);
        assert!(sudden.iter().all(|s| *s < 1.02), "{name}: {sudden:?}");
    }
}

/// A tagging phone that loses a tag now and then still never makes the picture jump: a frame without one while a zoom is on its
/// way is held back when it is the first of a run, and found by its picture after that.
#[test]
fn a_phone_that_loses_some_tags_still_keeps_the_picture_still() {
    let framings: [(&str, Box<dyn Fn(f64) -> (f32, f32, f32)>, f64); 2] = [("person leans away", Box::new(closer(3.5, 10.0, 4.0)), 20.0),
        ("person steps aside", Box::new(aside(10.0, 0.2, 5.0, 1.0, 5.0)), 14.0)];
    for every in [2, 5] {
        MISSING.with(|m| m.set(every));
        for (name, framing, seconds) in &framings {
            let (sudden, _) = steps_in(name, framing, *seconds, true);
            assert!(sudden.iter().all(|s| *s < 1.02), "{name}, every {every}th tag lost: {sudden:?}");
        }
    }
    MISSING.with(|m| m.set(0));
}

/// What the crop labels of a run's frames say about the phone's real crop: the worst error among the frames that don't call their label a guess (the zoom they
/// name over the zoom the phone really sent, always 1 or more), and the worst among all.
fn label_errors(name: &str, f: impl Fn(f64) -> (f32, f32, f32), seconds: f64, tags: bool) -> Vec<(String, f64, f64)> {
    let mut worst = Vec::new();
    for (scene, amp, noise) in [("textured", 1.0, 0.0), ("busy", 0.3, 1.0), ("dull", 0.1, 1.0)] {
        for (delay, glide) in [(0.2, 0.0), (0.4, 0.5), (0.5, 1.0)] {
            let r = Run { delay, amp, lens: Some(2.1), crop_max: 4.0, glide, noise, tags };
            let samples = run(&r, seconds, &f);
            let label = format!("{name}, {scene}, phone {delay}s late gliding {glide}s, tags {tags}");
            report(&label, &samples);
            let off = |s: &Sample| s.label_off.ln().abs();
            let settled: Vec<&Sample> = samples.iter().filter(|s| s.t > 3.0).collect();
            let sure = settled.iter().filter(|s| !s.unsure).map(|s| off(s)).fold(0.0, f64::max).exp();
            let all = settled.iter().map(|s| off(s)).fold(0.0, f64::max).exp();
            eprintln!("{label}: label zoom worst x{sure:.3} when sure, x{all:.3} in all, {} of {} frames a guess", settled.iter().filter(|s| s.unsure).count(), settled.len());
            worst.push((label, sure, all));
        }
    }
    worst
}

#[test]
fn a_phone_that_glides_its_zoom_is_labelled_with_the_crop_it_sent() {
    type Framing = Box<dyn Fn(f64) -> (f32, f32, f32)>;
    for tags in [false, true] {
        let scenes: [(&str, Framing, f64); 3] = [("person leans away", Box::new(closer(3.5, 10.0, 4.0)), 20.0), ("head sways", Box::new(swaying(5.0)), 30.0),
                                                 ("person steps aside", Box::new(aside(10.0, 0.2, 5.0, 1.0, 5.0)), 14.0)];
        for (name, framing, seconds) in scenes {
            // A tagged frame's label is the crop it came with, never a guess
            let (limit, all) = if tags { (1.01, 1.01) } else { (1.10, f64::MAX) };
            let wrong: Vec<_> = label_errors(name, framing, seconds, tags).into_iter().filter(|(_, sure, every)| *sure > limit || *every > all).collect();
            assert!(wrong.is_empty(), "a label that isn't called a guess is off: {wrong:?}");
        }
    }
}
