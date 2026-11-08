//! Auto colour grading's measuring: a face and some near-grey background read off a shrunk copy of each frame, and about a second of them boiled down to numbers and a few thousand sample pixels. Nothing here knows about sliders, so a tool that only wants the picture as numbers can use it too.

use crate::auto_tuning::*;
use crate::cpu::Rgb;
use crate::grade::LUMA;
use crate::imgops::Gray;
use serde_json::{json, Value};
use std::sync::OnceLock;
use telescope_core::people::{self, Box4};
use telescope_core::pose::{Person, SEEN};
use telescope_core::view::{upright_to_shown, Geometry};

/// A colour as encoded R, G, B, 0..1.
pub type Px = [f32; 3];

/// A sample pixel and whether the camera had already clipped it (white or black), which no grade can bring back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pix {
    pub rgb: Px,
    pub clipped: bool,
}

pub fn luma(p: Px) -> f32 {
    LUMA[0] * p[0] + LUMA[1] * p[1] + LUMA[2] * p[2]
}

/// The camera clipped it: a channel at white, or everything at black.
pub fn is_clipped(p: Px) -> bool {
    let top = p[0].max(p[1]).max(p[2]);
    top >= CLIPPED || top <= BLACK_CLIPPED
}

/// ln(red over green) and ln(blue over green) of an encoded colour, 0 and 0 for grey.
pub fn log_ratios(p: Px) -> [f32; 2] {
    let g = p[1].max(0.004);
    [(p[0].max(0.004) / g).ln(), (p[2].max(0.004) / g).ln()]
}

/// The same in linear light, which is where white balance works.
pub fn linear_ratios(p: Px) -> [f32; 2] {
    let lin = p.map(crate::grade::srgb_to_linear);
    let g = lin[1].max(1e-4);
    [(lin[0].max(1e-4) / g).ln(), (lin[2].max(1e-4) / g).ln()]
}

pub fn median(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f32::total_cmp);
    let n = values.len();
    if n % 2 == 1 { values[n / 2] } else { (values[n / 2 - 1] + values[n / 2]) / 2.0 }
}

/// The median absolute deviation, at least `SPREAD_FLOOR`.
fn spread(values: &[f32], middle: f32) -> f32 {
    median(&mut values.iter().map(|v| (v - middle).abs()).collect::<Vec<_>>()).max(SPREAD_FLOOR)
}

/// The value q (0..1) of the way up a sorted list.
pub fn percentile(sorted: &[f32], q: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f32 * q).round() as usize]
}

/// Every position a deterministic scatter visits in 0..n, the same for the same n.
fn scatter(n: usize) -> impl Iterator<Item = usize> {
    (0..).map(move |k: u64| (k.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11) as usize % n.max(1))
}

/// At most `n` of the items, spread evenly through them.
pub fn thin<T: Copy>(items: &[T], n: usize) -> Vec<T> {
    if items.len() <= n {
        return items.to_vec();
    }
    (0..n).map(|i| items[i * items.len() / n]).collect()
}

fn lin_of(byte: u8) -> f32 {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| crate::grade::srgb_to_linear(i as f32 / 255.0)))[byte as usize]
}

fn px_at(rgb: &Rgb, i: usize) -> Px {
    [rgb.data[i * 3] as f32 / 255.0, rgb.data[i * 3 + 1] as f32 / 255.0, rgb.data[i * 3 + 2] as f32 / 255.0]
}

// ── Faces ────────────────────────────────────────────────────────────

/// The part of a face that's skin, as a tilted ellipse on the shrunk copy (pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ellipse {
    pub centre: (f64, f64),
    /// Unit vector from the eyes towards the mouth.
    pub down: (f64, f64),
    pub across: f64,
    pub along: f64,
}

impl Ellipse {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let (dx, dy) = (x - self.centre.0, y - self.centre.1);
        let (u, v) = (dx * self.down.1 - dy * self.down.0, dx * self.down.0 + dy * self.down.1);
        (u / self.across).powi(2) + (v / self.along).powi(2) <= 1.0
    }

    fn bbox(&self) -> (f64, f64, f64, f64) {
        let across = (self.down.1, -self.down.0);
        let half = |i: usize| {
            let (a, d) = if i == 0 { (across.0, self.down.0) } else { (across.1, self.down.1) };
            ((self.across * a).powi(2) + (self.along * d).powi(2)).sqrt()
        };
        (self.centre.0 - half(0), self.centre.1 - half(1), self.centre.0 + half(0), self.centre.1 + half(1))
    }
}

/// A face found on the shrunk copy, and the person it belongs to (so the background can keep clear of them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Face {
    pub ellipse: Ellipse,
    /// The person's box, grown, in pixels.
    pub person: Box4,
    /// Eye distance in pixels.
    pub eyes: f64,
}

/// The face of a person on a picture of w x h pixels shown with `view`, from their eyes, nose and ears; None when the landmarks don't make a face.
pub fn find_face(person: &Person, view: &Geometry, w: usize, h: usize) -> Option<Face> {
    let at = |name: &str| {
        let (x, y) = person.point(name, SEEN)?;
        let (u, v) = upright_to_shown(view, x, y);
        Some((u * w as f64, v * h as f64))
    };
    let (left, right, nose) = (at("left_eye")?, at("right_eye")?, at("nose")?);
    let inside = |p: (f64, f64)| (-0.02 * w as f64..=1.02 * w as f64).contains(&p.0) && (-0.02 * h as f64..=1.02 * h as f64).contains(&p.1);
    if ![left, right, nose].into_iter().all(inside) {
        return None;
    }
    let eyes = (left.0 - right.0).hypot(left.1 - right.1);
    if eyes < MIN_EYE_PX {
        return None;
    }
    let mid = ((left.0 + right.0) / 2.0, (left.1 + right.1) / 2.0);
    let drop = (nose.0 - mid.0, nose.1 - mid.1);
    let length = drop.0.hypot(drop.1);
    if length < NOSE_DROP.0 * eyes || length > NOSE_DROP.1 * eyes {
        return None;
    }
    let down = (drop.0 / length, drop.1 / length);
    // the eyes lie across the face: a head tipped past sideways has them along the nose instead
    let along_eyes = ((right.0 - left.0) * down.0 + (right.1 - left.1) * down.1).abs() / eyes;
    if along_eyes > 0.5 {
        return None;
    }
    let mut across = FACE_ACROSS * eyes;
    if let (Some(a), Some(b)) = (at("left_ear"), at("right_ear")) {
        across = across.min(FACE_EAR_SHARE * (a.0 - b.0).hypot(a.1 - b.1));
    }
    let centre = (mid.0 + down.0 * FACE_CENTRE_DOWN * eyes, mid.1 + down.1 * FACE_CENTRE_DOWN * eyes);
    let ellipse = Ellipse { centre, down, across: across.max(2.0), along: FACE_ALONG * eyes };
    let points: Vec<(f64, f64)> = person.points.keys().filter_map(|name| at(name)).collect();
    let (x0, y0, x1, y1) = points.iter().fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |b, p| (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1)));
    let (gw, gh) = ((x1 - x0) * PERSON_GROW, (y1 - y0) * PERSON_GROW);
    // the landmarks stop at the eyes, so the head above them is added
    let person = (x0 - gw, y0 - gh - 1.2 * eyes, x1 + gw, y1 + gh);
    Some(Face { ellipse, person, eyes })
}

/// Which face to read: the one it followed before, else the person Auto framing follows (`main`), else the biggest. None when there's none.
pub fn choose_face(people: &[Person], main: Option<Box4>, view: &Geometry, size: (usize, usize), anchor: Option<(f64, f64)>) -> Option<Face> {
    let found: Vec<(&Person, Face)> = people.iter().filter_map(|p| find_face(p, view, size.0, size.1).map(|f| (p, f))).collect();
    let norm = |f: &Face| (f.ellipse.centre.0 / size.0 as f64, f.ellipse.centre.1 / size.1 as f64);
    if let Some(anchor) = anchor {
        let near = |f: &Face| (norm(f).0 - anchor.0).hypot(norm(f).1 - anchor.1);
        let best = found.iter().map(|(_, f)| (near(f), *f)).min_by(|a, b| a.0.total_cmp(&b.0))?;
        return (best.0 <= SAME_PERSON).then_some(best.1);
    }
    if let Some(main) = main {
        let mut best: Option<(f64, Face)> = None;
        for (p, f) in &found {
            let overlap = people::box_of(p).map_or(0.0, |b| people::overlap(b, main));
            if overlap > 0.2 && best.is_none_or(|b| overlap > b.0) {
                best = Some((overlap, *f));
            }
        }
        if let Some((_, f)) = best {
            return Some(f);
        }
    }
    let mut biggest: Option<Face> = None;
    for (_, f) in &found {
        if biggest.is_none_or(|b| f.eyes > b.eyes) {
            biggest = Some(*f);
        }
    }
    biggest
}

// ── One frame ────────────────────────────────────────────────────────

/// A face read off one frame.
#[derive(Clone, Debug)]
pub struct FaceFrame {
    pub colour: Px,
    pub spread: f32,
    pub clipped: f32,
    pub kept: Vec<Px>,
}

/// A cell of near-grey background.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Patch {
    pub cell: (usize, usize),
    /// Log red over green and log blue over green in linear light, 0 and 0 for grey.
    pub cast: [f32; 2],
    pub colour: Px,
}

/// The tone of one frame: percentiles of what the camera didn't clip, and how much it did.
#[derive(Clone, Copy, Debug, Default)]
pub struct ToneFrame {
    pub p01: f32,
    pub p50: f32,
    pub p99: f32,
    pub background_p99: f32,
    pub white_clipped: f32,
    pub black_clipped: f32,
    pub near_clip: f32,
    pub chroma: f32,
}

#[derive(Clone, Debug, Default)]
pub struct FrameMeasure {
    pub face: Option<FaceFrame>,
    pub patches: Vec<Patch>,
    pub tone: ToneFrame,
    pub noise: Option<f32>,
    pub motion: f32,
    background: Vec<Pix>,
    all: Vec<Pix>,
}

/// What `measure_frame` reads: the shrunk copy, the person mask shrunk to match, the face found and the frame's noise.
pub struct Picture<'a> {
    pub small: &'a Rgb,
    pub mask: Option<&'a Gray>,
    pub face: Option<Face>,
    pub noise: Option<f32>,
}

const PER_FRAME: (usize, usize, usize) = (200, 250, 400);

/// Skin out of the pixels of a face: the dark and clipped dropped, then whatever strays from the middle of the rest, twice over.
pub fn select_skin(pixels: &[Px]) -> Option<FaceFrame> {
    let total = pixels.len();
    let clipped = pixels.iter().filter(|p| is_clipped(**p)).count();
    let mut kept: Vec<Px> = pixels.iter().copied().filter(|p| !is_clipped(*p) && luma(*p) >= SKIN_LUMA_MIN).collect();
    let usable = kept.len();
    if usable < MIN_SKIN_PIXELS {
        return None;
    }
    let features = |p: &Px| {
        let r = log_ratios(*p);
        [r[0], r[1], luma(*p)]
    };
    let mut first_spread = 0.0;
    for round in 0..2 {
        let f: Vec<[f32; 3]> = kept.iter().map(features).collect();
        let mut mid = [0.0f32; 3];
        let mut mad = [0.0f32; 3];
        for c in 0..3 {
            let col: Vec<f32> = f.iter().map(|v| v[c]).collect();
            mid[c] = median(&mut col.clone());
            mad[c] = spread(&col, mid[c]);
        }
        if round == 0 {
            first_spread = mad[0].max(mad[1]);
        }
        let keep: Vec<bool> = f.iter().map(|v| {
            (0..2).all(|c| (v[c] - mid[c]).abs() <= SKIN_OUTLIER_CHROMA * mad[c]) && (v[2] - mid[2]).abs() <= SKIN_OUTLIER_LUMA * mad[2]
        }).collect();
        let mut i = 0;
        kept.retain(|_| {
            i += 1;
            keep[i - 1]
        });
        if kept.len() < MIN_SKIN_PIXELS {
            return None;
        }
    }
    if (kept.len() as f32 / usable as f32) < MIN_SKIN_KEPT || first_spread > SKIN_SPREAD_MAX {
        return None;
    }
    let colour: Px = std::array::from_fn(|c| median(&mut kept.iter().map(|p| p[c]).collect::<Vec<_>>()));
    let f: Vec<[f32; 2]> = kept.iter().map(|p| log_ratios(*p)).collect();
    let spread_now = (0..2).map(|c| spread(&f.iter().map(|v| v[c]).collect::<Vec<_>>(), log_ratios(colour)[c])).fold(0.0, f32::max);
    Some(FaceFrame { colour, spread: spread_now, clipped: clipped as f32 / total as f32, kept: thin(&kept, PER_FRAME.0) })
}

/// Everything one frame says. `motion` is left at 0 for the caller to fill in.
pub fn measure_frame(picture: &Picture) -> FrameMeasure {
    let rgb = picture.small;
    let (w, h) = (rgb.w, rgb.h);
    let n = w * h;
    // background: where the person mask says nobody is and the person's box isn't
    let in_box = |x: usize, y: usize| picture.face.is_some_and(|f| {
        let b = f.person;
        (x as f64) >= b.0 && (x as f64) <= b.2 && (y as f64) >= b.1 && (y as f64) <= b.3
    });
    let background: Vec<bool> = (0..n).map(|i| {
        let masked = picture.mask.is_some_and(|m| m.data[i] >= MASK_BACKGROUND);
        !masked && !in_box(i % w, i / w)
    }).collect();

    // the face: skin pixels inside the ellipse, where the mask agrees there's a person
    let face = picture.face.and_then(|f| {
        let (x0, y0, x1, y1) = f.ellipse.bbox();
        let mut pixels = Vec::new();
        let (mut inside, mut covered) = (0usize, 0usize);
        for y in (y0.floor().max(0.0) as usize)..((y1.ceil() as usize + 1).min(h)) {
            for x in (x0.floor().max(0.0) as usize)..((x1.ceil() as usize + 1).min(w)) {
                if !f.ellipse.contains(x as f64 + 0.5, y as f64 + 0.5) {
                    continue;
                }
                inside += 1;
                if picture.mask.is_none_or(|m| m.data[y * w + x] >= MASK_PERSON) {
                    covered += 1;
                    pixels.push(px_at(rgb, y * w + x));
                }
            }
        }
        if inside == 0 || (covered as f32 / inside as f32) < MASK_FACE_COVER {
            return None;
        }
        select_skin(&pixels)
    });

    FrameMeasure {
        face,
        patches: patches(rgb, &background),
        tone: tone(rgb, &background),
        noise: picture.noise,
        motion: 0.0,
        background: samples(rgb, PER_FRAME.1, |i| background[i]),
        all: samples(rgb, PER_FRAME.2, |_| true),
    }
}

fn samples(rgb: &Rgb, count: usize, wanted: impl Fn(usize) -> bool) -> Vec<Pix> {
    let n = rgb.w * rgb.h;
    let mut out = Vec::with_capacity(count);
    for i in scatter(n).take(count * 4) {
        if wanted(i) {
            let p = px_at(rgb, i);
            out.push(Pix { rgb: p, clipped: is_clipped(p) });
            if out.len() == count {
                break;
            }
        }
    }
    out
}

fn patches(rgb: &Rgb, background: &[bool]) -> Vec<Patch> {
    let (w, h) = (rgb.w, rgb.h);
    let mut out = Vec::new();
    for row in 0..GRID.1 {
        for col in 0..GRID.0 {
            let (x0, x1, y0, y1) = (col * w / GRID.0, (col + 1) * w / GRID.0, row * h / GRID.1, (row + 1) * h / GRID.1);
            let area = ((x1 - x0) * (y1 - y0)).max(1);
            let (mut count, mut clipped) = (0usize, 0usize);
            let (mut lin, mut enc, mut l1, mut l2) = ([0.0f32; 3], [0.0f32; 3], 0.0f32, 0.0f32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = y * w + x;
                    if !background[i] {
                        continue;
                    }
                    count += 1;
                    let p = px_at(rgb, i);
                    clipped += is_clipped(p) as usize;
                    for c in 0..3 {
                        lin[c] += lin_of(rgb.data[i * 3 + c]);
                        enc[c] += p[c];
                    }
                    let l = luma(p);
                    (l1, l2) = (l1 + l, l2 + l * l);
                }
            }
            if (count as f32) < PATCH_BACKGROUND * area as f32 {
                continue;
            }
            let k = count as f32;
            let (mean_l, var) = (l1 / k, (l2 / k - (l1 / k).powi(2)).max(0.0));
            if !(PATCH_LUMA.0..=PATCH_LUMA.1).contains(&mean_l) || clipped as f32 / k > PATCH_CLIPPED || var.sqrt() > PATCH_SMOOTH {
                continue;
            }
            let g = (lin[1] / k).max(1e-4);
            let cast = [((lin[0] / k).max(1e-4) / g).ln(), ((lin[2] / k).max(1e-4) / g).ln()];
            if cast.iter().any(|c| c.abs() > GREY_NEAR) {
                continue;
            }
            out.push(Patch { cell: (col, row), cast, colour: enc.map(|v| v / k) });
        }
    }
    out
}

fn tone(rgb: &Rgb, background: &[bool]) -> ToneFrame {
    let n = rgb.w * rgb.h;
    let (mut all, mut bg) = ([0u32; 256], [0u32; 256]);
    let (mut white, mut black, mut near, mut chroma_sum, mut chroma_n) = (0usize, 0usize, 0usize, 0.0f32, 0usize);
    for i in 0..n {
        let p = px_at(rgb, i);
        let top = p[0].max(p[1]).max(p[2]);
        if top >= CLIPPED {
            white += 1;
            continue;
        }
        if top <= BLACK_CLIPPED {
            black += 1;
            continue;
        }
        let l = luma(p);
        let bin = (l.clamp(0.0, 1.0) * 255.0).round() as usize;
        all[bin] += 1;
        if background[i] {
            bg[bin] += 1;
        }
        if l >= NEAR_CLIP_LUMA {
            near += 1;
        }
        if (CHROMA_LUMA.0..=CHROMA_LUMA.1).contains(&l) {
            chroma_sum += top - p[0].min(p[1]).min(p[2]);
            chroma_n += 1;
        }
    }
    let from = |hist: &[u32; 256], q: f32| {
        let total: u32 = hist.iter().sum();
        if total == 0 {
            return 0.0;
        }
        let want = (total as f32 * q).ceil().max(1.0) as u32;
        let mut seen = 0;
        for (i, c) in hist.iter().enumerate() {
            seen += c;
            if seen >= want {
                return i as f32 / 255.0;
            }
        }
        1.0
    };
    let nf = n.max(1) as f32;
    let all_p99 = from(&all, 0.99);
    ToneFrame {
        p01: from(&all, 0.01),
        p50: from(&all, 0.5),
        p99: all_p99,
        background_p99: if bg.iter().sum::<u32>() > 0 { from(&bg, 0.99) } else { all_p99 },
        white_clipped: white as f32 / nf,
        black_clipped: black as f32 / nf,
        near_clip: near as f32 / nf,
        chroma: if chroma_n > 0 { chroma_sum / chroma_n as f32 } else { 0.0 },
    }
}

/// How much noise a frame has: the typical size of its pixels' difference from their neighbours' average, on a scatter of its darker pixels. 0..1, like a pixel.
pub fn noise_level(rgb: &Rgb) -> Option<f32> {
    let (w, h) = (rgb.w, rgb.h);
    if w < 8 || h < 8 {
        return None;
    }
    let lum = |x: usize, y: usize| luma(px_at(rgb, y * w + x));
    let mut found = Vec::new();
    for i in scatter((w - 2) * (h - 2)).take(4000) {
        let (x, y) = (1 + i % (w - 2), 1 + i / (w - 2));
        let c = lum(x, y);
        if c > 0.5 {
            continue;
        }
        // Immerkaer's kernel: flat ground and edges cancel, noise doesn't
        let l = lum(x - 1, y - 1) + lum(x + 1, y - 1) + lum(x - 1, y + 1) + lum(x + 1, y + 1) - 2.0 * (lum(x, y - 1) + lum(x, y + 1) + lum(x - 1, y) + lum(x + 1, y)) + 4.0 * c;
        found.push(l.abs());
    }
    (found.len() >= 200).then(|| median(&mut found) * 0.247)
}

// ── About a second of them ───────────────────────────────────────────

/// What `Measurer::look` gets about a frame.
pub struct Look<'a> {
    pub small: &'a Rgb,
    pub mask: Option<&'a Gray>,
    /// Who the vision service saw, when it saw them close enough to this frame (an empty list otherwise).
    pub people: &'a [Person],
    /// The person Auto framing follows, in the same pictures as `people`.
    pub main: Option<Box4>,
    pub view: &'a Geometry,
    pub noise: Option<f32>,
    pub time_ns: u64,
}

/// Collects frames until there's about a second of steady ones.
#[derive(Default)]
pub struct Measurer {
    frames: Vec<FrameMeasure>,
    moving: usize,
    previous: Option<Vec<u8>>,
    anchor: Option<(f64, f64)>,
    span: Option<(u64, u64)>,
}

impl Measurer {
    pub fn new() -> Measurer {
        Measurer::default()
    }

    /// Looks at one frame. True once there are enough steady frames over enough time.
    pub fn look(&mut self, look: &Look) -> bool {
        let small = look.small;
        let face = choose_face(look.people, look.main, look.view, (small.w, small.h), self.anchor);
        let bytes: Vec<u8> = (0..small.w * small.h).map(|i| (luma(px_at(small, i)) * 255.0).round() as u8).collect();
        let diff = |from: &[u8], to: &[u8], keep: &dyn Fn(usize) -> bool| {
            let (mut sum, mut n) = (0u64, 0u64);
            for (i, (a, b)) in from.iter().zip(to).enumerate() {
                if keep(i) {
                    sum += a.abs_diff(*b) as u64;
                    n += 1;
                }
            }
            if n == 0 { 0.0 } else { sum as f32 / n as f32 / 255.0 }
        };
        let mut motion = 0.0;
        if let Some(before) = self.previous.as_ref().filter(|b| b.len() == bytes.len()) {
            motion = diff(before, &bytes, &|_| true);
            if let Some(f) = face {
                let w = small.w;
                let (x0, y0, x1, y1) = f.ellipse.bbox();
                let inside = move |i: usize| {
                    let (x, y) = ((i % w) as f64, (i / w) as f64);
                    x >= x0 && x <= x1 && y >= y0 && y <= y1
                };
                motion = motion.max(diff(before, &bytes, &inside) * 0.5);
            }
        }
        self.previous = Some(bytes);
        if motion > MOTION_SKIP {
            self.moving += 1;
            return false;
        }
        let mut fm = measure_frame(&Picture { small, mask: look.mask, face, noise: look.noise });
        fm.motion = motion;
        if fm.face.is_some() && self.anchor.is_none() {
            let f = face.expect("a face was read");
            self.anchor = Some((f.ellipse.centre.0 / small.w as f64, f.ellipse.centre.1 / small.h as f64));
        }
        let span = self.span.get_or_insert((look.time_ns, look.time_ns));
        span.1 = look.time_ns.max(span.1);
        self.frames.push(fm);
        self.ready()
    }

    pub fn ready(&self) -> bool {
        let seconds = self.span.map_or(0.0, |(a, b)| (b - a) as f64 / 1e9);
        self.frames.len() >= MIN_FRAMES && seconds >= MEASURE_SECONDS
    }

    pub fn frames(&self) -> usize {
        self.frames.len()
    }

    pub fn finish(self) -> Measurement {
        combine(self.frames, self.moving)
    }
}

/// What the face says once the frames are boiled down: the middle of each number over the frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceMeasure {
    pub colour: Px,
    pub luma: f32,
    pub clipped: f32,
    pub spread: f32,
    pub frames: usize,
}

/// What the near-grey background says about the light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grey {
    /// No near-grey areas, or too few apart from each other.
    None,
    /// Areas that nearly agree and nearly don't: not enough to go by.
    Unsure,
    /// Several separate areas agree on this cast (log red over green, log blue over green, linear light).
    Agree { cast: [f32; 2], patches: usize },
    /// The areas disagree: mixed light, coloured lamps or paint.
    Conflict,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ToneMeasure {
    pub p01: f32,
    pub p50: f32,
    pub p99: f32,
    pub background_p99: f32,
    pub white_clipped: f32,
    pub black_clipped: f32,
    pub near_clip: f32,
    pub chroma: f32,
}

/// The picture as numbers, plus a few thousand sample pixels to try grades on.
#[derive(Clone, Debug)]
pub struct Measurement {
    pub frames: usize,
    pub moving: usize,
    pub face: Option<FaceMeasure>,
    pub grey: Grey,
    pub tone: ToneMeasure,
    pub noise: f32,
    pub skin: Vec<Px>,
    pub patches: Vec<Px>,
    pub background: Vec<Pix>,
    pub all: Vec<Pix>,
}

impl Measurement {
    /// Not enough steady frames to say anything.
    pub fn is_empty(&self) -> bool {
        self.frames < MIN_FRAMES
    }

    /// The numbers as JSON, for a tool that reads the picture.
    pub fn numbers(&self) -> Value {
        let t = &self.tone;
        let face = self.face.map(|f| json!({"colour": f.colour, "brightness": f.luma, "clipped": f.clipped, "spread": f.spread, "frames": f.frames,
                                            "skin_angle": crate::auto_fit::skin_angle(f.colour)}));
        let grey = match self.grey {
            Grey::None => json!({"state": "none"}),
            Grey::Unsure => json!({"state": "unsure"}),
            Grey::Agree { cast, patches } => json!({"state": "agree", "cast": cast, "patches": patches}),
            Grey::Conflict => json!({"state": "conflict"}),
        };
        json!({
            "frames": self.frames, "skipped_moving": self.moving, "face": face, "grey": grey, "noise": self.noise,
            "tone": {"darkest": t.p01, "middle": t.p50, "brightest": t.p99, "background_brightest": t.background_p99,
                     "white_clipped": t.white_clipped, "black_clipped": t.black_clipped, "near_clip": t.near_clip, "colour_strength": t.chroma},
        })
    }
}

fn middle(frames: &[&FrameMeasure], pick: impl Fn(&FrameMeasure) -> f32) -> f32 {
    median(&mut frames.iter().map(|f| pick(f)).collect::<Vec<_>>())
}

fn combine(frames: Vec<FrameMeasure>, moving: usize) -> Measurement {
    let all: Vec<&FrameMeasure> = frames.iter().collect();
    let tone = ToneMeasure {
        p01: middle(&all, |f| f.tone.p01),
        p50: middle(&all, |f| f.tone.p50),
        p99: middle(&all, |f| f.tone.p99),
        background_p99: middle(&all, |f| f.tone.background_p99),
        white_clipped: middle(&all, |f| f.tone.white_clipped),
        black_clipped: middle(&all, |f| f.tone.black_clipped),
        near_clip: middle(&all, |f| f.tone.near_clip),
        chroma: middle(&all, |f| f.tone.chroma),
    };
    let noises: Vec<&FrameMeasure> = all.iter().copied().filter(|f| f.noise.is_some()).collect();
    let noise = middle(&noises, |f| f.noise.unwrap_or(0.0));

    let (face, skin) = read_face(&all);
    let (grey, patches) = read_grey(&all);
    let background = thin(&frames.iter().flat_map(|f| f.background.iter().copied()).collect::<Vec<_>>(), SAMPLES.1);
    let everything = thin(&frames.iter().flat_map(|f| f.all.iter().copied()).collect::<Vec<_>>(), SAMPLES.2);
    Measurement { frames: frames.len(), moving, face, grey, tone, noise, skin, patches, background, all: everything }
}

fn read_face(frames: &[&FrameMeasure]) -> (Option<FaceMeasure>, Vec<Px>) {
    let with: Vec<&FrameMeasure> = frames.iter().copied().filter(|f| f.face.is_some()).collect();
    if with.len() < MIN_FACE_FRAMES || (with.len() as f32) < MIN_FACE_SHARE * frames.len() as f32 {
        return (None, Vec::new());
    }
    let faces: Vec<&FaceFrame> = with.iter().filter_map(|f| f.face.as_ref()).collect();
    let colour: Px = std::array::from_fn(|c| median(&mut faces.iter().map(|f| f.colour[c]).collect::<Vec<_>>()));
    let mid = log_ratios(colour);
    // frames whose face strays from the middle one (a hand, a turn) are left out, and a face that never settles is unsure
    let off = |f: &FaceFrame| {
        let r = log_ratios(f.colour);
        (r[0] - mid[0]).abs().max((r[1] - mid[1]).abs())
    };
    let deviations: Vec<f32> = faces.iter().map(|f| off(f)).collect();
    if median(&mut deviations.clone()) > FACE_STEADY {
        return (None, Vec::new());
    }
    let limit = (3.0 * median(&mut deviations.clone())).max(FACE_STEADY);
    let steady: Vec<&&FaceFrame> = faces.iter().filter(|f| off(f) <= limit).collect();
    let measure = FaceMeasure {
        colour,
        luma: luma(colour),
        clipped: median(&mut steady.iter().map(|f| f.clipped).collect::<Vec<_>>()),
        spread: median(&mut steady.iter().map(|f| f.spread).collect::<Vec<_>>()),
        frames: steady.len(),
    };
    let pooled: Vec<Px> = steady.iter().flat_map(|f| f.kept.iter().copied()).collect();
    (Some(measure), thin(&pooled, SAMPLES.0))
}

fn read_grey(frames: &[&FrameMeasure]) -> (Grey, Vec<Px>) {
    let mut cells: Vec<((usize, usize), Vec<&Patch>)> = Vec::new();
    for p in frames.iter().flat_map(|f| f.patches.iter()) {
        match cells.iter_mut().find(|c| c.0 == p.cell) {
            Some(c) => c.1.push(p),
            None => cells.push((p.cell, vec![p])),
        }
    }
    cells.sort_by_key(|c| (c.0 .1, c.0 .0));
    let steady: Vec<Patch> = cells.iter().filter(|(_, ps)| ps.len() as f32 >= PATCH_STEADY * frames.len() as f32).map(|(cell, ps)| Patch {
        cell: *cell,
        cast: [median(&mut ps.iter().map(|p| p.cast[0]).collect::<Vec<_>>()), median(&mut ps.iter().map(|p| p.cast[1]).collect::<Vec<_>>())],
        colour: std::array::from_fn(|c| median(&mut ps.iter().map(|p| p.colour[c]).collect::<Vec<_>>())),
    }).collect();
    // separate areas: cells that touch count as one
    let mut chosen: Vec<Patch> = Vec::new();
    for p in steady {
        if chosen.iter().all(|c| c.cell.0.abs_diff(p.cell.0).max(c.cell.1.abs_diff(p.cell.1)) >= PATCH_APART) {
            chosen.push(p);
        }
    }
    if chosen.len() < MIN_PATCHES {
        return (Grey::None, Vec::new());
    }
    let cast = [median(&mut chosen.iter().map(|p| p.cast[0]).collect::<Vec<_>>()), median(&mut chosen.iter().map(|p| p.cast[1]).collect::<Vec<_>>())];
    let apart = chosen.iter().map(|p| (p.cast[0] - cast[0]).hypot(p.cast[1] - cast[1])).fold(0.0, f32::max);
    let colours: Vec<Px> = chosen.iter().map(|p| p.colour).collect();
    if apart <= GREY_AGREE {
        (Grey::Agree { cast, patches: chosen.len() }, colours)
    } else if apart > GREY_CONFLICT {
        (Grey::Conflict, colours)
    } else {
        (Grey::Unsure, colours)
    }
}
