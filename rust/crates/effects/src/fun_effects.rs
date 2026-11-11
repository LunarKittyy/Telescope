//! The Fun pack's picture effects (fun_effects.py): plain structs over RGB pictures. An effect keeps what it needs between frames (a
//! trail, a few frames back) and is called as effect.run(frame, look) -> picture, with look the frozen settings. A new one starts
//! afresh. Game Boy is a shader instead (game_boy.wgsl) and never comes through here.

use crate::cpu::Rgb;
use crate::cv_draw::{plain_scale_from_height, plain_text_width, Canvas};
use crate::imgops::{compose, shrink_plane, times, Gray, Remap};
use std::borrow::Cow;
use std::collections::VecDeque;
use telescope_core::avx2_too;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::image::{over_bands, resize_area, resize_linear_into, rint};
use telescope_core::workers::each_part;
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

/// image at frame's size, or None when it's missing or a different shape, which stretching would only bend. One that is the right size is borrowed.
fn fit(image: Option<&Rgb>, w: usize, h: usize) -> Option<Cow<'_, Rgb>> {
    let image = image?;
    if (image.w, image.h) == (w, h) {
        return Some(Cow::Borrowed(image));
    }
    fits(image.w, image.h, w, h).then(|| Cow::Owned(image.linear_exact(w, h)))
}

fn fit_gray(mask: &Gray, w: usize, h: usize) -> Option<Cow<'_, Gray>> {
    if (mask.w, mask.h) == (w, h) {
        return Some(Cow::Borrowed(mask));
    }
    fits(mask.w, mask.h, w, h).then(|| Cow::Owned(mask.linear_exact(w, h)))
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
            let fitted = ((plate.w, plate.h) != (w, h)).then(|| plate.linear_exact(w, h));
            self.small = Some(fitted.as_ref().unwrap_or(plate).area(sw, sh).blur3());
            self.held = Some((plate.clone(), w, h));
        }
        let small = frame.area(sw, sh).blur3();
        let held = self.small.as_ref().unwrap();
        let mut diff = Gray::new(sw, sh);
        for (i, d) in diff.data.iter_mut().enumerate() {
            *d = (0..3).map(|c| small.data[i * 3 + c].abs_diff(held.data[i * 3 + c])).max().unwrap_or(0);
        }
        Some(diff.above(DIFFERENCE).open(3).close(7).linear_exact(w, h))
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
    Stick(Option<StickPaper>),
    Webcam(Webcam),
    Vhs(Vhs),
    Crt(Option<Crt>),
    Ascii(Option<AsciiGlyphs>),
}

impl Effect {
    pub fn named(name: &str) -> Option<Effect> {
        Some(match name {
            "clones" => Effect::Clones(PlateMask::default()),
            "cloak" => Effect::Cloak(PlateMask::default()),
            "light" => Effect::Light(None),
            "slit" => Effect::Slit(VecDeque::new(), None),
            "stick" => Effect::Stick(None),
            "webcam" => Effect::Webcam(Webcam::new()),
            "vhs" => Effect::Vhs(Vhs::new()),
            "crt" => Effect::Crt(None),
            "ascii" => Effect::Ascii(None),
            _ => return None,
        })
    }

    /// The effects that need no RGB picture work on the frame's own data (VHS, ASCII, stick figure); None for the rest, frame untouched.
    pub fn run_frame(&mut self, frame: &mut Frame, look: &Look) -> Option<()> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        match self {
            Effect::Vhs(v) if frame.format == PixelFormat::I420 => v.run_i420(frame.data_mut(), w, h).then_some(()),
            Effect::Ascii(glyphs) if matches!(frame.format, PixelFormat::I420 | PixelFormat::Nv12) && frame.data.len() >= w * h => {
                let out = ascii_from_luma(&frame.data[..w * h], w, h, glyphs);
                out.write_to(frame);
                Some(())
            }
            Effect::Stick(paper) => stick_figure(paper, w, h, look).write_over(frame).then_some(()),
            _ => None,
        }
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
            Effect::Light(trail) => light(trail, frame, look),
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
                slit_bands(frames, look.across, spread, sw, sh).linear_exact(w, h)
            }
            Effect::Stick(paper) => stick_figure(paper, w, h, look).picture(),
            Effect::Webcam(c) => c.run(frame),
            Effect::Vhs(v) => v.run(frame),
            Effect::Crt(c) => {
                if c.as_ref().is_none_or(|c| (c.w, c.h) != (w, h)) {
                    *c = Some(Crt::new(w, h));
                }
                c.as_ref().unwrap().run(frame)
            }
            Effect::Ascii(glyphs) => ascii(frame, glyphs),
        }
    }
}

/// Light painting in one pass, with the trail kept in place.
fn light(trail: &mut Option<Rgb>, frame: &Rgb, look: &Look) -> Rgb {
    let (w, h) = (frame.w, frame.h);
    let level = (150 + look.brightness * 10) as u8;
    // 1: gone in about half a second, 10: seconds; a level off each time or rounding keeps a faint trail for good
    let keep = (0.80 + look.fade as f64 * 0.019) as f32;
    let fresh = trail.as_ref().is_none_or(|t| (t.w, t.h) != (w, h));
    if fresh {
        *trail = Some(Rgb::scratch(w, h));
    }
    let mut out = Rgb::scratch(w, h);
    let rl = w * 3;
    if rl > 0 {
        let held = trail.as_mut().unwrap();
        over_two(&mut out.data, &mut held.data, rl, |first, out, held| {
            for (i, (o, t)) in out.chunks_exact_mut(rl).zip(held.chunks_exact_mut(rl)).enumerate() {
                light_row(&frame.data[(first + i) * rl..][..rl], t, o, level, keep, fresh);
            }
        });
    }
    out
}

/// over_bands for two pictures of the same shape: band(first row, its rows of a, the same rows of b).
fn over_two(a: &mut [u8], b: &mut [u8], row_len: usize, band: impl Fn(usize, &mut [u8], &mut [u8]) + Sync) {
    const BANDS: usize = 8;
    let rows = a.len() / row_len.max(1);
    if rows < 256 || a.len() < 512 * 1024 {
        band(0, a, b);
        return;
    }
    let per_band = rows.div_ceil(BANDS);
    let mut parts: Vec<(usize, &mut [u8], &mut [u8])> = a.chunks_mut(per_band * row_len).zip(b.chunks_mut(per_band * row_len)).enumerate().map(|(i, (x, y))| (i * per_band, x, y)).collect();
    each_part(&mut parts, |_, (first, x, y)| band(*first, x, y));
}

avx2_too! {
    /// One row of light painting: the trail (held, or fresh from what's lit now) and the picture over it.
    fn light_row(frame: &[u8], trail: &mut [u8], out: &mut [u8], level: u8, keep: f32, fresh: bool) {
        for ((f, t), o) in frame.as_chunks::<3>().0.iter().zip(trail.as_chunks_mut::<3>().0).zip(out.as_chunks_mut::<3>().0) {
            let grey = ((f[0] as u32 * 9798 + f[1] as u32 * 19235 + f[2] as u32 * 3735 + (1 << 14)) >> 15) as u8;
            let lit = grey > level;
            for c in 0..3 {
                let lights = if lit { f[c] } else { 0 };
                let next = if fresh { lights } else { ((t[c] as f32 * keep).round_ties_even().min(255.0) as u8).saturating_sub(1).max(lights) };
                t[c] = next;
                o[c] = f[c].max(next);
            }
        }
    }
}

/// Slit's picture at the reduced size: each of up to 45 bands (columns across, rows otherwise) taken from an older frame, the
/// further along the older, so motion smears into a ripple.
fn slit_bands(frames: &VecDeque<Rgb>, across: bool, spread: usize, sw: usize, sh: usize) -> Rgb {
    let size = if across { sw } else { sh };
    let bands = 45.min(size);
    let mut out = Rgb::scratch(sw, sh);
    let last = frames.len() - 1;
    for b in 0..bands {
        let (lo, hi) = (b * size / bands, (b + 1) * size / bands);
        let src = &frames[last.min(b * spread / (bands - 1).max(1))].data;
        if across {
            for y in 0..sh {
                let span = (y * sw + lo) * 3..(y * sw + hi) * 3;
                out.data[span.clone()].copy_from_slice(&src[span]);
            }
        } else {
            let span = lo * sw * 3..hi * sw * 3;
            out.data[span.clone()].copy_from_slice(&src[span]);
        }
    }
    out
}

// ── Stick figure ───────────────────────────────────────────────────────

pub const PAPER: [u8; 3] = [240, 236, 232];
pub const INK: [u8; 3] = [28, 32, 40];

/// The stick figure's picture, kept while the size, the room and the lines stay the same.
pub struct StickPaper {
    size: (usize, usize),
    plate: Option<Arc<Rgb>>,
    lines: Vec<((f64, f64), (f64, f64))>,
    heads: Vec<((f64, f64), f64)>,
    drawn: Rgb,
    /// The same picture in a frame's own format, for a step that writes it straight over the frame
    packed: Option<Frame>,
}

impl StickPaper {
    /// A picture of its own, as the effect hands out a picture a later stage may draw on.
    fn picture(&self) -> Rgb {
        let mut out = Rgb::scratch(self.drawn.w, self.drawn.h);
        out.data.copy_from_slice(&self.drawn.data);
        out
    }

    /// Puts the picture over frame in its format. False when this can't say how (a format Rgb doesn't know, or another size).
    pub fn write_over(&mut self, frame: &mut Frame) -> bool {
        let format = frame.format;
        if (frame.width as usize, frame.height as usize) != self.size || !matches!(format, PixelFormat::I420 | PixelFormat::Nv12 | PixelFormat::Rgba | PixelFormat::Bgra) {
            return false;
        }
        if self.packed.as_ref().is_none_or(|p| p.format != format) {
            let mut packed = Frame::new(frame.width, frame.height, format, vec![0; format.size(frame.width, frame.height)]);
            self.drawn.write_to(&mut packed);
            self.packed = Some(packed);
        }
        let packed = self.packed.as_ref().unwrap();
        let data = frame.data_mut();
        if data.len() != packed.data.len() {
            return false;
        }
        data.copy_from_slice(&packed.data);
        true
    }
}

pub fn stick_figure<'a>(paper: &'a mut Option<StickPaper>, w: usize, h: usize, look: &Look) -> &'a mut StickPaper {
    let same_plate = |held: &Option<Arc<Rgb>>| match (held, &look.plate) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    };
    if !paper.as_ref().is_some_and(|p| p.size == (w, h) && same_plate(&p.plate) && p.lines == look.lines && p.heads == look.heads) {
        let mut out = fit(look.plate.as_deref(), w, h).map_or_else(|| Rgb::filled(w, h, PAPER), Cow::into_owned);
        let thick = 3.max(h / 70) as i32;
        let ink = [INK[0], INK[1], INK[2], 0];
        let (fw, fh) = (w as f64, h as f64);
        let mut canvas = Canvas { data: &mut out.data, w, h, channels: 3 };
        for &((x0, y0), (x1, y1)) in &look.lines {
            canvas.line(((x0 * fw) as i32, (y0 * fh) as i32), ((x1 * fw) as i32, (y1 * fh) as i32), ink, thick);
        }
        for &((x, y), r) in &look.heads {
            canvas.circle(((x * fw) as i32, (y * fh) as i32), thick.max((r * fh) as i32), ink, thick);
        }
        *paper = Some(StickPaper { size: (w, h), plate: look.plate.clone(), lines: look.lines.clone(), heads: look.heads.clone(), drawn: out, packed: None });
    }
    paper.as_mut().unwrap()
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
        for p in small.data.as_chunks_mut::<3>().0 {
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
    for (i, p) in img.data.as_chunks::<3>().0.iter().enumerate() {
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
        let (mut y, mut cr, mut cb) = (Gray::scratch(sw, sh), Gray::scratch(sw, sh), Gray::scratch(sw, sh));
        to_ycc(&small.data, &mut y.data, &mut cr.data, &mut cb.data);
        let shift = (sw / 120).max(1);
        let smear = |g: &Gray| roll(g, shift).box_blur(9, 1);
        let (cr, cb) = (smear(&cr), smear(&cb));
        let mut y = y.box_blur(3, 1);
        self.roughen(&mut y, 255);
        let mut out = Rgb::scratch(sw, sh);
        from_ycc(&y.data, &cr.data, &cb.data, &mut out.data);
        out.linear_exact(w, h)
    }

    /// The same look on the planes of an I420 picture in place, with no trip through RGB. False for a buffer too short.
    pub fn run_i420(&mut self, data: &mut [u8], w: usize, h: usize) -> bool {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        if w == 0 || h == 0 || data.len() < w * h + 2 * cw * ch {
            return false;
        }
        self.t += 1;
        let (sw, sh) = ((w / 3).max(1), (h / 2).max(1));
        let (luma, chroma) = data.split_at_mut(w * h);
        let mut y = Gray { w: sw, h: sh, data: resize_area(luma, w, h, 1, sw, sh) }.box_blur(3, 1);
        // video's luma has 219 levels between black and white
        self.roughen(&mut y, 219);
        resize_linear_into(&y.data, sw, sh, 1, luma, w, (0, 0, w, h));
        // The RGB look smears by 9 of its 3-pixel columns, 27 pixels: 12 chroma columns is 24, and the shift scales the same way
        let shift = ((sw / 120).max(1) * 3).div_ceil(2);
        for plane in chroma.chunks_exact_mut(cw * ch).take(2) {
            let mut g = Gray::scratch(cw, ch);
            g.data.copy_from_slice(plane);
            plane.copy_from_slice(&roll(&g, shift).box_blur(12, 1).data);
        }
        true
    }

    /// Each line a little off, and the tracking band. `range` is the levels between black and white in this luma.
    fn roughen(&mut self, y: &mut Gray, range: i32) {
        let (sw, sh) = (y.w, y.h);
        for line in y.data.chunks_exact_mut(sw) {
            let n = self.noise.between(-12, 12);
            let by = ((n.unsigned_abs() as i32 * range + 127) / 255) as u8;
            if n < 0 {
                line.iter_mut().for_each(|v| *v = v.saturating_sub(by));
            } else {
                line.iter_mut().for_each(|v| *v = v.saturating_add(by));
            }
        }
        let band = ((sh as i64 - ((self.t * 3) % (sh + 40)) as i64).rem_euclid(sh as i64)) as usize;
        let (lo, hi) = (band.saturating_sub(6), (band + 6).min(sh));
        let slide = self.noise.between(-8, 8);
        let bright = ((40 * range + 127) / 255) as u8;
        for line in y.data.chunks_exact_mut(sw).take(hi).skip(lo) {
            line.rotate_right(slide.rem_euclid(sw as i32) as usize);
            line.iter_mut().for_each(|v| *v = v.saturating_add(bright));
        }
    }
}

avx2_too! {
    /// cv2's BGR2YCrCb, fixed point, written out as planes.
    fn to_ycc(rgb: &[u8], y: &mut [u8], cr: &mut [u8], cb: &mut [u8]) {
        for (((p, y), cr), cb) in rgb.as_chunks::<3>().0.iter().zip(y).zip(cr).zip(cb) {
            let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
            let l = (b * 1868 + g * 9617 + r * 4899 + 8192) >> 14;
            *y = l as u8;
            *cr = (((r - l) * 11682 + 2105344) >> 14).clamp(0, 255) as u8;
            *cb = (((b - l) * 9241 + 2105344) >> 14).clamp(0, 255) as u8;
        }
    }
}

avx2_too! {
    /// cv2's YCrCb2BGR, fixed point.
    fn from_ycc(y: &[u8], cr: &[u8], cb: &[u8], rgb: &mut [u8]) {
        for (o, ((y, cr), cb)) in rgb.as_chunks_mut::<3>().0.iter_mut().zip(y.iter().zip(cr).zip(cb)) {
            let (l, r, b) = (*y as i32, *cr as i32 - 128, *cb as i32 - 128);
            let d = |v: i32| (v + (1 << 13)) >> 14;
            *o = [(l + d(r * 22987)).clamp(0, 255) as u8, (l + d(b * -5636 + r * -11698)).clamp(0, 255) as u8, (l + d(b * 29049)).clamp(0, 255) as u8];
        }
    }
}

impl Default for Vhs {
    fn default() -> Self {
        Vhs::new()
    }
}

/// np.roll along the rows: each one slides right by n, the end wrapping round to the start.
fn roll(g: &Gray, n: usize) -> Gray {
    let mut out = Gray::scratch(g.w, g.h);
    let n = n % g.w.max(1);
    for (o, line) in out.data.chunks_exact_mut(g.w).zip(g.data.chunks_exact(g.w)) {
        let (start, end) = line.split_at(g.w - n);
        o[..n].copy_from_slice(end);
        o[n..].copy_from_slice(start);
    }
    out
}

// ── CRT ────────────────────────────────────────────────────────────────

/// A curved screen: bulged, with scanlines, a phosphor mask and dark corners.
pub struct Crt {
    w: usize,
    h: usize,
    /// For each output pixel, where in the picture it looks, as cv2.convertMaps to CV_16SC2 keeps it
    bend: Remap,
    /// What each colour of each pixel is multiplied by
    shade: Vec<u8>,
    glow: Glow,
}

/// The glow, worked out at a third of the size (shrunk, blurred and bent there) and scaled back up as it's added.
struct Glow {
    w: usize,
    h: usize,
    bend: Remap,
    /// For each column and row of the full picture, the two small ones it sits between and how far towards the second, in 256ths
    columns: Vec<(u32, u32, u16)>,
    rows: Vec<(u32, u32, u16)>,
}

/// The axis of a picture n pixels across, from -1 to 1.
fn axis(n: usize) -> Vec<f32> {
    (0..n).map(|i| if i + 1 == n && n > 1 { 1.0 } else if n == 1 { -1.0 } else { (-1.0 + i as f64 * (2.0 / (n - 1) as f64)) as f32 }).collect()
}

/// Where the screen's pixel at (px, py) looks in a picture w x h, in pixels, and whether it's off the bulge.
fn bend_at(px: f32, py: f32, w: usize, h: usize) -> (f32, f32, bool) {
    let bulge = 1.0 + 0.06 * (px * px + py * py);
    let (mx, my) = ((px * bulge + 1.0) * (w - 1) as f32 / 2.0, (py * bulge + 1.0) * (h - 1) as f32 / 2.0);
    (mx, my, (px * bulge).abs() > 1.0 || (py * bulge).abs() > 1.0)
}

/// How each of n2 pixels samples a line of n: the two it lies between and the weight of the second in 256ths.
fn taps(n: usize, n2: usize) -> Vec<(u32, u32, u16)> {
    (0..n2).map(|i| {
        let p = ((i as f64 + 0.5) * n as f64 / n2 as f64 - 0.5).max(0.0);
        let i0 = (p.floor() as usize).min(n - 1);
        (i0 as u32, (i0 + 1).min(n - 1) as u32, ((p - i0 as f64) * 256.0).round().min(256.0) as u16)
    }).collect()
}

impl Glow {
    fn new(w: usize, h: usize) -> Glow {
        let (sw, sh) = ((w / 3).max(1), (h / 3).max(1));
        let (xs, ys) = (axis(sw), axis(sh));
        let mut places = Vec::with_capacity(sw * sh);
        for &py in &ys {
            for &px in &xs {
                // off the bulge it looks at the nearest edge, so no dark rim is blurred in; the shade is black there anyway
                let (mx, my, _) = bend_at(px, py, sw, sh);
                let (mx, my) = (mx.clamp(0.0, (sw - 1) as f32), my.clamp(0.0, (sh - 1) as f32));
                places.push((rint((mx * 32.0) as f64), rint((my * 32.0) as f64)));
            }
        }
        Glow { w: sw, h: sh, bend: Remap::new(sw, sh, sw, sh, &places), columns: taps(sw, w), rows: taps(sh, h) }
    }

    /// The glow of a picture w x h: small and bent, then widened to the picture's width so that each full row is just a blend of two.
    fn of(&self, picture: &[u8], w: usize, h: usize) -> Rgb {
        let soft = Rgb { w: self.w, h: self.h, data: resize_area(picture, w, h, 3, self.w, self.h) }.box_blur(3, 3);
        let mut bent = Rgb::scratch(self.w, self.h);
        self.bend.run(&soft.data, &mut bent.data);
        let mut wide = Rgb::scratch(w, self.h);
        let sl = self.w * 3;
        over_bands(&mut wide.data, w * 3, |first, part| {
            for (i, line) in part.chunks_exact_mut(w * 3).enumerate() {
                let row = &bent.data[(first + i) * sl..][..sl];
                for (o, &(x0, x1, wx)) in line.as_chunks_mut::<3>().0.iter_mut().zip(&self.columns) {
                    let (p, q, wx) = (&row[x0 as usize * 3..][..3], &row[x1 as usize * 3..][..3], wx as u32);
                    for c in 0..3 {
                        o[c] = ((p[c] as u32 * (256 - wx) + q[c] as u32 * wx + 128) >> 8) as u8;
                    }
                }
            }
        });
        wide
    }

    /// Pixels first..end of row y of the full-size glow, blended from the two rows of the widened one it lies between.
    fn row(&self, wide: &Rgb, y: usize, (first, end): (usize, usize), out: &mut [u8]) {
        let (y0, y1, wy) = self.rows[y];
        let rl = wide.w * 3;
        let (a, b) = (&wide.data[y0 as usize * rl + first * 3..][..(end - first) * 3], &wide.data[y1 as usize * rl + first * 3..][..(end - first) * 3]);
        blend(a, b, wy as u32, out);
    }
}

avx2_too! {
    /// a and b mixed, b by wy in 256ths.
    fn blend(a: &[u8], b: &[u8], wy: u32, out: &mut [u8]) {
        for ((o, &p), &q) in out.iter_mut().zip(a).zip(b) {
            *o = ((p as u32 * (256 - wy) + q as u32 * wy + 128) >> 8) as u8;
        }
    }
}

avx2_too! {
    /// The picture, a quarter of the glow rounded to even, and the shade over it all.
    fn crt_row(bent: &[u8], glow: &[u8], shade: &[u8], out: &mut [u8]) {
        for (o, ((b, g), s)) in out.iter_mut().zip(bent.iter().zip(glow).zip(shade)) {
            let g = *g as u16;
            let quarter = (g + ((g >> 2) & 1) + 1) >> 2;
            *o = times((*b as u16 + quarter).min(255) as u8, *s) as u8;
        }
    }
}

impl Crt {
    pub fn new(w: usize, h: usize) -> Crt {
        let (xs, ys) = (axis(w), axis(h));
        let pitch = 2.max(h / 270);
        let triad = [[1.0f32, 0.82, 0.82], [0.82, 1.0, 0.82], [0.82, 0.82, 1.0]];
        let (mut places, mut shade) = (Vec::with_capacity(w * h), Vec::with_capacity(w * h * 3));
        for (y, &py) in ys.iter().enumerate() {
            for x in 0..w {
                let px = xs[x];
                let r2 = px * px + py * py;
                let (mx, my, edge) = bend_at(px, py, w, h);
                places.push(if edge { Remap::OUTSIDE } else { (rint((mx * 32.0) as f64), rint((my * 32.0) as f64)) });
                let mut s = (1.15 - 0.45 * r2).clamp(0.0, 1.0);
                if y % pitch == pitch - 1 {
                    s *= 0.55;
                }
                // B, G, R columns in Python, so red leads here
                let t = triad[x % 3];
                shade.extend([2, 1, 0].map(|c| if edge { 0 } else { ((s * t[c]) * 255.0) as u8 }));
            }
        }
        Crt { w, h, bend: Remap::new(w, h, w, h, &places), shade, glow: Glow::new(w, h) }
    }

    /// The picture bent row by row, each row glowed and shaded before the next, so the whole bent picture is never held.
    pub fn run(&self, frame: &Rgb) -> Rgb {
        let (w, h) = (self.w, self.h);
        let src = &frame.data[..w * h * 3];
        let glow = self.glow.of(src, w, h);
        let mut out = Rgb::scratch(w, h);
        let rl = w * 3;
        over_bands(&mut out.data, rl, |first, part| {
            let (mut bent, mut soft) = (vec![0u8; rl], vec![0u8; rl]);
            for (i, line) in part.chunks_exact_mut(rl).enumerate() {
                let y = first + i;
                // beyond the bulge the screen is black
                let (from, to) = self.bend.span(y);
                let (before, rest) = line.split_at_mut(from * 3);
                let (middle, after) = rest.split_at_mut((to - from) * 3);
                before.fill(0);
                after.fill(0);
                self.bend.run_row(src, y, &mut bent);
                self.glow.row(&glow, y, (from, to), &mut soft[from * 3..to * 3]);
                crt_row(&bent[from * 3..to * 3], &soft[from * 3..to * 3], &self.shade[y * rl + from * 3..y * rl + to * 3], middle);
            }
        });
        out
    }
}

// ── ASCII ──────────────────────────────────────────────────────────────

/// The ramp's characters as OpenCV's Hershey Plain draws them in a cell this size.
pub struct AsciiGlyphs {
    cell: (usize, usize),
    glyphs: Vec<u8>,
    /// The same as rows of R, G, B pixels as they're drawn (green, a third as much blue), so a row of a glyph is copied in one go
    coloured: Vec<u8>,
}

impl AsciiGlyphs {
    fn new(cw: usize, ch: usize) -> AsciiGlyphs {
        let mut glyphs = vec![0u8; ASCII_RAMP.len() * cw * ch];
        let scale = plain_scale_from_height(4.max(ch as i32 - 4));
        for (i, c) in ASCII_RAMP.chars().enumerate() {
            let text = c.to_string();
            let tw = plain_text_width(&text, scale);
            let mut canvas = Canvas { data: &mut glyphs[i * cw * ch..(i + 1) * cw * ch], w: cw, h: ch, channels: 1 };
            canvas.plain_text(&text, ((cw as i32 - tw).div_euclid(2), ch as i32 - 3), scale, [255, 0, 0, 0]);
        }
        let coloured = glyphs.iter().flat_map(|&t| [0, t, t / 3]).collect();
        AsciiGlyphs { cell: (cw, ch), glyphs, coloured }
    }
}

const ASCII_COLUMNS: usize = 150;

/// The cell's size and how many fit: (cell width, cell height, columns, rows).
fn ascii_cells(w: usize, h: usize) -> (usize, usize, usize, usize) {
    let cw = 4.max(w / ASCII_COLUMNS);
    let ch = cw * 2;
    (cw, ch, w / cw, h / ch)
}

/// The picture in characters, green on black, about 150 across.
fn ascii(frame: &Rgb, held: &mut Option<AsciiGlyphs>) -> Rgb {
    let (w, h) = (frame.w, frame.h);
    let (_, _, cols, rows) = ascii_cells(w, h);
    if cols == 0 || rows == 0 {
        return Rgb::new(w, h);
    }
    let gray = frame.shrink(cols, rows).gray();
    ascii_drawn(&gray.data, w, h, held)
}

/// The same from a video-range luma plane (the first w x h bytes of an I420 or NV12 frame); a character can differ at a step.
pub fn ascii_from_luma(luma: &[u8], w: usize, h: usize, held: &mut Option<AsciiGlyphs>) -> Rgb {
    let (_, _, cols, rows) = ascii_cells(w, h);
    if cols == 0 || rows == 0 || luma.len() < w * h {
        return Rgb::new(w, h);
    }
    let small = shrink_plane(&luma[..w * h], w, h, cols, rows);
    let levels: Vec<u8> = small.iter().map(|&y| ((y.saturating_sub(16) as u32 * 255 + 109) / 219).min(255) as u8).collect();
    ascii_drawn(&levels, w, h, held)
}

/// The characters for cols x rows brightness levels, drawn a row of each cell at a time.
fn ascii_drawn(levels: &[u8], w: usize, h: usize, held: &mut Option<AsciiGlyphs>) -> Rgb {
    let (cw, ch, cols, rows) = ascii_cells(w, h);
    if held.as_ref().is_none_or(|g| g.cell != (cw, ch)) {
        *held = Some(AsciiGlyphs::new(cw, ch));
    }
    let glyphs = &held.as_ref().unwrap().coloured;
    let mut out = Rgb::scratch(w, h);
    let (rl, cell_bytes) = (w * 3, cw * 3);
    over_bands(&mut out.data, rl, |first, part| {
        for (i, line) in part.chunks_exact_mut(rl).enumerate() {
            let (row, y) = ((first + i) / ch, (first + i) % ch);
            if row >= rows {
                line.fill(0);
                continue;
            }
            let (text, margin) = line.split_at_mut(cols * cell_bytes);
            margin.fill(0);
            for (cell, &level) in text.chunks_exact_mut(cell_bytes).zip(&levels[row * cols..][..cols]) {
                let index = level as usize * ASCII_RAMP.len() / 256;
                cell.copy_from_slice(&glyphs[(index * ch + y) * cell_bytes..][..cell_bytes]);
            }
        }
    });
    out
}

#[cfg(test)]
#[path = "fun_effects_tests.rs"]
mod tests;
