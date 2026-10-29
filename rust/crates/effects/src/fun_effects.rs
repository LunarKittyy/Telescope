//! The Fun pack's picture effects (fun_effects.py): plain structs over RGB pictures. An effect keeps what it needs between frames (a
//! trail, a few frames back) and is called as effect.run(frame, look) -> picture, with look the frozen settings. A new one starts
//! afresh. Game Boy is a shader instead (game_boy.wgsl) and never comes through here.

use crate::cpu::Rgb;
use crate::imgops::{compose, Gray};
use std::collections::VecDeque;
use std::sync::Arc;

/// Masks from the empty room are worked out at a quarter of the size.
const MASK_SMALL: usize = 4;
/// How far a pixel has to be from the empty room to count as someone in front of it.
const DIFFERENCE: u8 = 28;
const ASCII_RAMP: &str = " .:-=+*#%@";

/// The effects that cut the person out of the picture.
pub const EDGES: [&str; 2] = ["clones", "cloak"];
/// The effects that use the empty room.
pub const PLATE: [&str; 3] = ["clones", "cloak", "stick"];
/// Every effect that works the picture over here (the others are datamosh, which changes packets, and Game Boy, a shader).
pub const EFFECTS: [&str; 9] = ["clones", "cloak", "light", "slit", "stick", "webcam", "vhs", "crt", "ascii"];

/// What the effects need, frozen (swapped whole from the card).
#[derive(Clone)]
pub struct Look {
    pub effect: String,
    /// The empty room, as taken.
    pub plate: Option<Arc<Rgb>>,
    pub clones: Option<Arc<Clones>>,
    /// 0..10, how soft a cut-out's edge is
    pub edges: i64,
    /// 1..10, how long light trails last
    pub fade: i64,
    /// 1..10, how bright a light has to be to paint
    pub brightness: i64,
    /// Frames between the top of a slit-scan and its bottom
    pub spread: i64,
    /// Slit-scan bands run left to right instead of top to bottom
    pub across: bool,
    /// The stick figure: ((x0, y0), (x1, y1)) 0..1 of the frame, and heads as ((x, y), r)
    pub lines: Vec<((f64, f64), (f64, f64))>,
    pub heads: Vec<((f64, f64), f64)>,
    /// Bumped to start the effect afresh (Clear on light painting)
    pub reset: u32,
}

impl Default for Look {
    fn default() -> Look {
        Look { effect: "off".into(), plate: None, clones: None, edges: 3, fade: 5, brightness: 6, spread: 20, across: false, lines: Vec::new(), heads: Vec::new(), reset: 0 }
    }
}

/// Whether a picture of this shape can stand in for one at w x h: the same shape, not one stretching would only bend.
fn fits(iw: usize, ih: usize, w: usize, h: usize) -> bool {
    (iw, ih) == (w, h) || (iw * h).abs_diff(ih * w) as f64 <= (iw * h).max(ih * w) as f64 * 0.01
}

/// image at frame's size, or None when it's missing or a different shape, which stretching would only bend.
fn fit(image: Option<&Rgb>, w: usize, h: usize) -> Option<Rgb> {
    let image = image?;
    if (image.w, image.h) == (w, h) {
        return Some(image.clone());
    }
    fits(image.w, image.h, w, h).then(|| image.linear(w, h))
}

fn fit_gray(mask: &Gray, w: usize, h: usize) -> Option<Gray> {
    if (mask.w, mask.h) == (w, h) {
        return Some(mask.clone());
    }
    fits(mask.w, mask.h, w, h).then(|| mask.linear(w, h))
}

/// Where a frame differs from the empty room: someone in front of it, without the vision download.
#[derive(Default)]
pub struct PlateMask {
    /// The retained plate (kept alive here, so it can't be mistaken for another) and the frame size its small copy was made for
    held: Option<(Arc<Rgb>, usize, usize)>,
    small: Option<Rgb>,
}

impl PlateMask {
    /// None when the plate is a different shape from the frame.
    pub fn run(&mut self, frame: &Rgb, plate: &Arc<Rgb>) -> Option<Gray> {
        let (w, h) = (frame.w, frame.h);
        if !fits(plate.w, plate.h, w, h) {
            return None;
        }
        let (sw, sh) = ((w / MASK_SMALL).max(1), (h / MASK_SMALL).max(1));
        if !self.held.as_ref().is_some_and(|(p, hw, hh)| Arc::ptr_eq(p, plate) && (*hw, *hh) == (w, h)) {
            let fitted = ((plate.w, plate.h) != (w, h)).then(|| plate.linear(w, h));
            self.small = Some(fitted.as_ref().unwrap_or(plate).area(sw, sh).blur3());
            self.held = Some((plate.clone(), w, h));
        }
        let small = frame.area(sw, sh).blur3();
        let held = self.small.as_ref().unwrap();
        let mut diff = Gray::new(sw, sh);
        for (i, d) in diff.data.iter_mut().enumerate() {
            *d = (0..3).map(|c| small.data[i * 3 + c].abs_diff(held.data[i * 3 + c])).max().unwrap_or(0);
        }
        Some(diff.above(DIFFERENCE).open(3).close(7).linear(w, h))
    }
}

/// The person mask: from the vision download when it's there, else the difference from the empty room.
fn person(frame: &Rgb, look: &Look, mask: Option<Gray>, plate_mask: &mut PlateMask) -> Option<Gray> {
    if mask.is_some() {
        return mask;
    }
    plate_mask.run(frame, look.plate.as_ref()?)
}

/// The clones taken so far, flattened into one picture and one mask so a frame costs the same however many.
pub struct Clones {
    pub picture: Rgb,
    pub mask: Gray,
    pub count: usize,
}

impl Clones {
    pub const MAX: usize = 8;

    pub fn add(clones: Option<&Clones>, frame: &Rgb, mask: &Gray) -> Clones {
        match clones {
            Some(c) if (c.picture.w, c.picture.h) == (frame.w, frame.h) => Clones { picture: compose(frame, mask, &c.picture), mask: c.mask.max(mask), count: c.count + 1 },
            _ => Clones { picture: frame.clone(), mask: mask.clone(), count: 1 },
        }
    }
}

/// What an effect gets besides the picture and the look: the vision download's mask for it, if there is one.
pub struct Inputs {
    pub mask: Option<Gray>,
}

pub enum Effect {
    Clones(PlateMask),
    Cloak(PlateMask),
    Light(Option<Rgb>),
    Slit(VecDeque<Rgb>, Option<(usize, usize)>),
    Stick,
    Webcam(Webcam),
    Vhs(Vhs),
    Crt(Option<Crt>),
    Ascii,
}

impl Effect {
    pub fn named(name: &str) -> Option<Effect> {
        Some(match name {
            "clones" => Effect::Clones(PlateMask::default()),
            "cloak" => Effect::Cloak(PlateMask::default()),
            "light" => Effect::Light(None),
            "slit" => Effect::Slit(VecDeque::new(), None),
            "stick" => Effect::Stick,
            "webcam" => Effect::Webcam(Webcam::new()),
            "vhs" => Effect::Vhs(Vhs::new()),
            "crt" => Effect::Crt(None),
            "ascii" => Effect::Ascii,
            _ => return None,
        })
    }

    pub fn run(&mut self, frame: &Rgb, look: &Look, inputs: Inputs) -> Rgb {
        let (w, h) = (frame.w, frame.h);
        match self {
            Effect::Clones(pm) => {
                let Some(clones) = look.clones.as_ref() else { return frame.clone() };
                let (Some(picture), Some(cmask)) = (fit(Some(&clones.picture), w, h), fit_gray(&clones.mask, w, h)) else { return frame.clone() };
                let out = compose(&picture, &cmask, frame);
                match person(frame, look, inputs.mask, pm) {
                    Some(mask) => compose(frame, &mask.soften(look.edges), &out),
                    None => out,
                }
            }
            Effect::Cloak(pm) => {
                let Some(plate) = fit(look.plate.as_deref(), w, h) else { return frame.clone() };
                let mask = match inputs.mask {
                    // the model's edge sits a little inside the person
                    Some(m) => m.dilate(9),
                    None => match pm.run(frame, look.plate.as_ref().unwrap()) {
                        Some(m) => m,
                        None => return frame.clone(),
                    },
                };
                compose(&plate, &mask.soften(look.edges), frame)
            }
            Effect::Light(trail) => {
                let level = (150 + look.brightness * 10) as u8;
                let lit = frame.gray().above(level);
                let mut lights = Rgb::new(w, h);
                for (i, m) in lit.data.iter().enumerate() {
                    if *m != 0 {
                        lights.data[i * 3..i * 3 + 3].copy_from_slice(&frame.data[i * 3..i * 3 + 3]);
                    }
                }
                let next = match trail.as_ref().filter(|t| (t.w, t.h) == (w, h)) {
                    None => lights,
                    Some(t) => {
                        // 1: gone in about half a second, 10: hangs around for seconds. At least a level off each time: rounding
                        // alone would keep a faint trail for good at long fades
                        let keep = 0.80 + look.fade as f32 * 0.019;
                        let data = t.data.iter().zip(&lights.data).map(|(&old, &new)| ((old as f32 * keep).round_ties_even().min(255.0) as u8).saturating_sub(1).max(new)).collect();
                        Rgb { w, h, data }
                    }
                };
                let out = Rgb { w, h, data: frame.data.iter().zip(&next.data).map(|(&a, &b)| a.max(b)).collect() };
                *trail = Some(next);
                out
            }
            Effect::Slit(frames, shape) => {
                let small = frame.area((w / 2).max(1), (h / 2).max(1));
                if *shape != Some((small.w, small.h)) {
                    frames.clear();
                    *shape = Some((small.w, small.h));
                }
                let spread = look.spread.max(1) as usize;
                let (sw, sh) = (small.w, small.h);
                frames.push_front(small);
                while frames.len() > spread + 1 {
                    frames.pop_back();
                }
                let size = if look.across { sw } else { sh };
                let bands = 45.min(size);
                let mut out = Rgb::new(sw, sh);
                let last = frames.len() - 1;
                for b in 0..bands {
                    let (lo, hi) = (b * size / bands, (b + 1) * size / bands);
                    let src = &frames[last.min(b * spread / (bands - 1).max(1))];
                    for y in 0..sh {
                        for x in 0..sw {
                            let inside = if look.across { (lo..hi).contains(&x) } else { (lo..hi).contains(&y) };
                            if inside {
                                out.set(x, y, src.at(x, y));
                            }
                        }
                    }
                }
                out.linear(w, h)
            }
            Effect::Stick => stick_figure(frame, look),
            Effect::Webcam(c) => c.run(frame),
            Effect::Vhs(v) => v.run(frame),
            Effect::Crt(c) => {
                if c.as_ref().is_none_or(|c| (c.w, c.h) != (w, h)) {
                    *c = Some(Crt::new(w, h));
                }
                c.as_ref().unwrap().run(frame)
            }
            Effect::Ascii => ascii(frame),
        }
    }
}

// ── Stick figure ───────────────────────────────────────────────────────

pub const PAPER: [u8; 3] = [240, 236, 232];
pub const INK: [u8; 3] = [28, 32, 40];

/// How much of a pixel at distance d from a stroke's middle line the stroke covers, for a stroke `half` wide each side: a soft edge
/// a pixel across, as an anti-aliased line has. OpenCV's thick anti-aliased strokes come out wider than their thickness says (about
/// 1.5 times for a line, 1.9 for a ring), which the callers allow for.
fn cover(d: f64, half: f64) -> f64 {
    (half + 0.5 - d).clamp(0.0, 1.0)
}

fn blend(out: &mut Rgb, x: usize, y: usize, amount: f64) {
    if amount <= 0.0 {
        return;
    }
    let p = out.at(x, y);
    let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * amount).round() as u8;
    out.set(x, y, [mix(p[0], INK[0]), mix(p[1], INK[1]), mix(p[2], INK[2])]);
}

fn stick_figure(frame: &Rgb, look: &Look) -> Rgb {
    let (w, h) = (frame.w, frame.h);
    let mut out = fit(look.plate.as_deref(), w, h).unwrap_or_else(|| Rgb::filled(w, h, PAPER));
    let thick = 3.max(h / 70) as f64;
    for &((x0, y0), (x1, y1)) in &look.lines {
        let (ax, ay, bx, by) = ((x0 * w as f64).trunc(), (y0 * h as f64).trunc(), (x1 * w as f64).trunc(), (y1 * h as f64).trunc());
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = (dx * dx + dy * dy).max(1e-9);
        let pad = thick / 2.0 + 1.0;
        for y in (ay.min(by) - pad).max(0.0) as usize..((ay.max(by) + pad) as usize + 1).min(h) {
            for x in (ax.min(bx) - pad).max(0.0) as usize..((ax.max(bx) + pad) as usize + 1).min(w) {
                let t = (((x as f64 - ax) * dx + (y as f64 - ay) * dy) / len2).clamp(0.0, 1.0);
                let d = ((x as f64 - ax - t * dx).powi(2) + (y as f64 - ay - t * dy).powi(2)).sqrt();
                blend(&mut out, x, y, cover(d, thick * 0.77));
            }
        }
    }
    for &((hx, hy), r) in &look.heads {
        let (cx, cy) = ((hx * w as f64).trunc(), (hy * h as f64).trunc());
        let radius = thick.max((r * h as f64).trunc());
        let pad = radius + thick;
        for y in (cy - pad).max(0.0) as usize..((cy + pad) as usize + 1).min(h) {
            for x in (cx - pad).max(0.0) as usize..((cx + pad) as usize + 1).min(w) {
                let d = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt();
                blend(&mut out, x, y, cover((d - radius + 0.5).abs(), thick * 0.95));
            }
        }
    }
    out
}

// ── Randomness ─────────────────────────────────────────────────────────

/// A small xorshift: these effects only need noise that looks like noise.
pub struct Noise(u64);

impl Noise {
    pub fn new() -> Noise {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
        Noise(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A whole number in lo..=hi.
    pub fn between(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1) as u64) as i32
    }
}

impl Default for Noise {
    fn default() -> Self {
        Noise::new()
    }
}

// ── 1998 webcam ────────────────────────────────────────────────────────

/// 320 wide, washed out, blocky JPEG and 15 frames a second.
pub struct Webcam {
    held: Option<(Rgb, (usize, usize))>,
    odd: bool,
    noise: Noise,
}

impl Webcam {
    const WIDTH: usize = 320;
    const QUALITY: u32 = 12;

    pub fn new() -> Webcam {
        Webcam { held: None, odd: false, noise: Noise::new() }
    }

    pub fn run(&mut self, frame: &Rgb) -> Rgb {
        self.odd = !self.odd;
        let (w, h) = (frame.w, frame.h);
        if let (true, Some((small, shape))) = (self.odd, &self.held) {
            if *shape == (w, h) {
                // Scaled up again, not handed out twice: later stages may draw on what they get
                return small.nearest(w, h);
            }
        }
        let (sw, sh) = (Self::WIDTH, ((h * Self::WIDTH) as f64 / w as f64).round_ties_even().max(1.0) as usize);
        let mut small = frame.area(sw, sh);
        for p in small.data.chunks_exact_mut(3) {
            // washed out, with a cold cast
            let cast = [0u32, 6, 18];
            for c in 0..3 {
                p[c] = ((p[c] as f32 * 0.8 + 28.0).round_ties_even().min(255.0) as u32 + cast[c]).min(255) as u8;
            }
            let n = self.noise.between(-10, 10);
            for c in p.iter_mut() {
                *c = (*c as i32 + n).clamp(0, 255) as u8;
            }
        }
        let small = jpeg(&small, Self::QUALITY);
        let out = small.nearest(w, h);
        self.held = Some((small, (w, h)));
        out
    }
}

impl Default for Webcam {
    fn default() -> Self {
        Webcam::new()
    }
}

const LUMA_Q: [u32; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104,
    113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const CHROMA_Q: [u32; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// What a JPEG at this quality does to a picture: 4:2:0 chroma, 8 x 8 blocks taken through the discrete cosine and the standard
/// quantisation tables, and back.
fn jpeg(img: &Rgb, quality: u32) -> Rgb {
    let (w, h) = (img.w, img.h);
    let scale = if quality < 50 { 5000 / quality } else { 200 - quality * 2 };
    let table = |base: &[u32; 64]| -> Vec<f32> { base.iter().map(|b| ((b * scale + 50) / 100).clamp(1, 255) as f32).collect() };
    let (lq, cq) = (table(&LUMA_Q), table(&CHROMA_Q));
    let (mut y, mut cb, mut cr) = (vec![0f32; w * h], vec![0f32; w * h], vec![0f32; w * h]);
    for (i, p) in img.data.chunks_exact(3).enumerate() {
        let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
        y[i] = 0.299 * r + 0.587 * g + 0.114 * b;
        cb[i] = 128.0 - 0.168_736 * r - 0.331_264 * g + 0.5 * b;
        cr[i] = 128.0 + 0.5 * r - 0.418_688 * g - 0.081_312 * b;
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let down = |plane: &[f32]| -> Vec<f32> {
        let at = |x: usize, yy: usize| plane[yy.min(h - 1) * w + x.min(w - 1)];
        (0..cw * ch).map(|i| {
            let (x, yy) = ((i % cw) * 2, (i / cw) * 2);
            (at(x, yy) + at(x + 1, yy) + at(x, yy + 1) + at(x + 1, yy + 1)) / 4.0
        }).collect()
    };
    let (y, cb, cr) = (squeeze(&y, w, h, &lq), squeeze(&down(&cb), cw, ch, &cq), squeeze(&down(&cr), cw, ch, &cq));
    let up = |plane: &[f32], x: usize, yy: usize| -> f32 {
        // the chroma sample's centre is between four pixels: a linear scale back up
        let fx = ((x as f32 + 0.5) / 2.0 - 0.5).max(0.0);
        let fy = ((yy as f32 + 0.5) / 2.0 - 0.5).max(0.0);
        let (x0, y0) = ((fx as usize).min(cw - 1), (fy as usize).min(ch - 1));
        let (x1, y1) = ((x0 + 1).min(cw - 1), (y0 + 1).min(ch - 1));
        let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
        (plane[y0 * cw + x0] * (1.0 - ax) + plane[y0 * cw + x1] * ax) * (1.0 - ay) + (plane[y1 * cw + x0] * (1.0 - ax) + plane[y1 * cw + x1] * ax) * ay
    };
    let mut out = Rgb::new(w, h);
    for yy in 0..h {
        for x in 0..w {
            let (l, b, r) = (y[yy * w + x], up(&cb, x, yy) - 128.0, up(&cr, x, yy) - 128.0);
            let px = [l + 1.402 * r, l - 0.344_136 * b - 0.714_136 * r, l + 1.772 * b];
            out.set(x, yy, [px[0].round().clamp(0.0, 255.0) as u8, px[1].round().clamp(0.0, 255.0) as u8, px[2].round().clamp(0.0, 255.0) as u8]);
        }
    }
    out
}

/// A plane through 8 x 8 blocks of DCT and quantisation, the edges repeated out to a whole block.
fn squeeze(plane: &[f32], w: usize, h: usize, q: &[f32]) -> Vec<f32> {
    use std::f32::consts::PI;
    let cos: Vec<f32> = (0..64).map(|i| (((2 * (i / 8) + 1) * (i % 8)) as f32 * PI / 16.0).cos()).collect();
    let c = |k: usize| if k == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
    let mut out = vec![0f32; w * h];
    for by in (0..h).step_by(8) {
        for bx in (0..w).step_by(8) {
            let mut block = [0f32; 64];
            for j in 0..8 {
                for i in 0..8 {
                    block[j * 8 + i] = plane[(by + j).min(h - 1) * w + (bx + i).min(w - 1)] - 128.0;
                }
            }
            let mut coef = [0f32; 64];
            for v in 0..8 {
                for u in 0..8 {
                    let mut sum = 0.0;
                    for j in 0..8 {
                        for i in 0..8 {
                            sum += block[j * 8 + i] * cos[i * 8 + u] * cos[j * 8 + v];
                        }
                    }
                    coef[v * 8 + u] = ((0.25 * c(u) * c(v) * sum) / q[v * 8 + u]).round() * q[v * 8 + u];
                }
            }
            for j in 0..8 {
                for i in 0..8 {
                    let mut sum = 0.0;
                    for v in 0..8 {
                        for u in 0..8 {
                            sum += c(u) * c(v) * coef[v * 8 + u] * cos[i * 8 + u] * cos[j * 8 + v];
                        }
                    }
                    if by + j < h && bx + i < w {
                        out[(by + j) * w + bx + i] = (0.25 * sum + 128.0).round().clamp(0.0, 255.0);
                    }
                }
            }
        }
    }
    out
}

// ── VHS ────────────────────────────────────────────────────────────────

/// Smeared colour shifted to the side, soft picture, noise and a tracking band that rolls up the screen.
pub struct Vhs {
    t: usize,
    noise: Noise,
}

impl Vhs {
    pub fn new() -> Vhs {
        Vhs { t: 0, noise: Noise::new() }
    }

    pub fn run(&mut self, frame: &Rgb) -> Rgb {
        self.t += 1;
        let (w, h) = (frame.w, frame.h);
        let (sw, sh) = ((w / 3).max(1), (h / 2).max(1));
        let small = frame.area(sw, sh);
        let (mut y, mut cr, mut cb) = (Gray::new(sw, sh), Gray::new(sw, sh), Gray::new(sw, sh));
        for (i, p) in small.data.chunks_exact(3).enumerate() {
            let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
            let l = (b * 1868 + g * 9617 + r * 4899 + 8192) >> 14;
            y.data[i] = l as u8;
            cr.data[i] = (((r - l) * 11682 + 2105344) >> 14).clamp(0, 255) as u8;
            cb.data[i] = (((b - l) * 9241 + 2105344) >> 14).clamp(0, 255) as u8;
        }
        let shift = (sw / 120).max(1);
        let smear = |g: &Gray| roll(g, shift).box_blur(9, 1);
        let (cr, cb) = (smear(&cr), smear(&cb));
        let mut y = y.box_blur(3, 1);
        // each line a little off
        for row in 0..sh {
            let n = self.noise.between(-12, 12);
            for x in 0..sw {
                let i = row * sw + x;
                y.data[i] = (y.data[i] as i32 + n).clamp(0, 255) as u8;
            }
        }
        let band = ((sh as i64 - ((self.t * 3) % (sh + 40)) as i64).rem_euclid(sh as i64)) as usize;
        let (lo, hi) = (band.saturating_sub(6), (band + 6).min(sh));
        let slide = self.noise.between(-8, 8);
        for row in lo..hi {
            let line = Gray { w: sw, h: 1, data: y.data[row * sw..(row + 1) * sw].to_vec() };
            let rolled = roll(&line, slide.rem_euclid(sw as i32) as usize);
            for x in 0..sw {
                y.data[row * sw + x] = rolled.data[x].saturating_add(40);
            }
        }
        let mut out = Rgb::new(sw, sh);
        for i in 0..sw * sh {
            let (l, r, b) = (y.data[i] as i32, cr.data[i] as i32 - 128, cb.data[i] as i32 - 128);
            let d = |v: i32| (v + (1 << 13)) >> 14;
            out.data[i * 3] = (l + d(r * 22987)).clamp(0, 255) as u8;
            out.data[i * 3 + 1] = (l + d(b * -5636 + r * -11698)).clamp(0, 255) as u8;
            out.data[i * 3 + 2] = (l + d(b * 29049)).clamp(0, 255) as u8;
        }
        out.linear(w, h)
    }
}

impl Default for Vhs {
    fn default() -> Self {
        Vhs::new()
    }
}

/// np.roll along the rows: each one slides right by n, the end wrapping round to the start.
fn roll(g: &Gray, n: usize) -> Gray {
    let mut out = Gray::new(g.w, g.h);
    for y in 0..g.h {
        for x in 0..g.w {
            out.data[y * g.w + (x + n) % g.w] = g.data[y * g.w + x];
        }
    }
    out
}

// ── CRT ────────────────────────────────────────────────────────────────

/// A curved screen: bulged, with scanlines, a phosphor mask and dark corners.
pub struct Crt {
    w: usize,
    h: usize,
    /// For each output pixel, where in the picture it looks (x, y), or None outside the bulge
    maps: Vec<(f32, f32)>,
    shade: Vec<[u8; 3]>,
}

impl Crt {
    pub fn new(w: usize, h: usize) -> Crt {
        let axis = |n: usize| -> Vec<f32> { (0..n).map(|i| if n == 1 { -1.0 } else { -1.0 + 2.0 * i as f32 / (n - 1) as f32 }).collect() };
        let (xs, ys) = (axis(w), axis(h));
        let pitch = 2.max(h / 270);
        let triad = [[1.0f32, 0.82, 0.82], [0.82, 1.0, 0.82], [0.82, 0.82, 1.0]];
        let (mut maps, mut shade) = (Vec::with_capacity(w * h), Vec::with_capacity(w * h));
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (xs[x], ys[y]);
                let r2 = px * px + py * py;
                let bulge = 1.0 + 0.06 * r2;
                maps.push(((px * bulge + 1.0) * (w - 1) as f32 / 2.0, (py * bulge + 1.0) * (h - 1) as f32 / 2.0));
                let mut s = (1.15 - 0.45 * r2).clamp(0.0, 1.0);
                if y % pitch == pitch - 1 {
                    s *= 0.55;
                }
                // B, G, R columns in Python, so red leads here
                let t = triad[x % 3];
                let edge = (px * bulge).abs() > 1.0 || (py * bulge).abs() > 1.0;
                let v = |c: usize| if edge { 0 } else { ((s * t[[2, 1, 0][c]]) * 255.0) as u8 };
                shade.push([v(0), v(1), v(2)]);
            }
        }
        Crt { w, h, maps, shade }
    }

    pub fn run(&self, frame: &Rgb) -> Rgb {
        let (w, h) = (self.w, self.h);
        let at = |x: i64, y: i64, c: usize| -> f32 { if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 { 0.0 } else { frame.data[(y as usize * w + x as usize) * 3 + c] as f32 } };
        let mut bent = Rgb::new(w, h);
        for (i, &(mx, my)) in self.maps.iter().enumerate() {
            let (x0, y0) = (mx.floor(), my.floor());
            let (fx, fy) = (mx - x0, my - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            for c in 0..3 {
                let top = at(x0, y0, c) * (1.0 - fx) + at(x0 + 1, y0, c) * fx;
                let bottom = at(x0, y0 + 1, c) * (1.0 - fx) + at(x0 + 1, y0 + 1, c) * fx;
                bent.data[i * 3 + c] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
        // a little glow
        let glow = bent.box_blur(9, 9);
        let mut out = Rgb::new(w, h);
        for i in 0..w * h {
            for c in 0..3 {
                let lit = (bent.data[i * 3 + c] as u32 + ((glow.data[i * 3 + c] as f32 * 0.25).round_ties_even() as u32)).min(255);
                out.data[i * 3 + c] = (lit as f32 * self.shade[i][c] as f32 / 255.0).round_ties_even() as u8;
            }
        }
        out
    }
}

// ── ASCII ──────────────────────────────────────────────────────────────

/// 5 x 7 pictures of the ramp's characters: Python draws them with OpenCV's Hershey font, which isn't here.
const GLYPHS: [[&str; 7]; 10] = [
    [".....", ".....", ".....", ".....", ".....", ".....", "....."],
    [".....", ".....", ".....", ".....", ".....", "..#..", "..#.."],
    [".....", "..#..", "..#..", ".....", "..#..", "..#..", "....."],
    [".....", ".....", ".....", "#####", ".....", ".....", "....."],
    [".....", ".....", "#####", ".....", "#####", ".....", "....."],
    [".....", "..#..", "..#..", "#####", "..#..", "..#..", "....."],
    [".....", "#.#.#", ".###.", "#####", ".###.", "#.#.#", "....."],
    [".#.#.", "#####", ".#.#.", ".#.#.", "#####", ".#.#.", "....."],
    ["##..#", "##.#.", "..#..", ".#.##", "#..##", ".....", "....."],
    [".###.", "#...#", "#.###", "#.#.#", "#.###", "#....", ".####"],
];

/// The picture in characters, green on black, about 150 across.
fn ascii(frame: &Rgb) -> Rgb {
    const COLUMNS: usize = 150;
    let (w, h) = (frame.w, frame.h);
    let cw = 4.max(w / COLUMNS);
    let ch = cw * 2;
    let (cols, rows) = (w / cw, h / ch);
    let mut out = Rgb::new(w, h);
    if cols == 0 || rows == 0 {
        return out;
    }
    let gray = frame.shrink(cols, rows).gray();
    debug_assert_eq!(ASCII_RAMP.len(), GLYPHS.len());
    for row in 0..rows {
        for col in 0..cols {
            let glyph = &GLYPHS[gray.data[row * cols + col] as usize * GLYPHS.len() / 256];
            for y in 0..ch {
                // a line of margin above and below, as a font has
                let gy = y * 9 / ch;
                for x in 0..cw {
                    let (gx, on) = (x * 6 / cw, gy >= 1 && gy <= 7 && glyph[gy - 1].as_bytes().get(x * 6 / cw).is_some_and(|c| *c == b'#'));
                    let _ = gx;
                    if on {
                        out.set(col * cw + x, row * ch + y, [0, 255, 85]);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "fun_effects_tests.rs"]
mod tests;
