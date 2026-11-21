//! Frame alignment and stacking for lucky imaging, live stacking and star trails: luma planes, a sharpness score, phase correlation
//! for the shift between two pictures, and a stacker that lines frames up with the first and averages, maxes or sigma-clips them.

use crate::frame::{Frame, PixelFormat};
use crate::image::frame_rgb;
use std::f32::consts::TAU;

/// The smallest and largest square the correlation looks at
const MIN_SQUARE: usize = 64;
const MAX_SQUARE: usize = 256;
/// The correlation's frequency fade, as a share of the square's size
const LOW_PASS: f32 = 0.1;
/// Frames wider than this are aligned on a smaller copy
const ALIGN_WIDTH: usize = 1024;
/// Clip mode takes everything until it has this many frames to judge by
const CLIP_AFTER: u32 = 5;
/// Pixel values are 0..1, so this is a bit under half a level
const MIN_SPREAD: f32 = 0.002;
/// A reference picture needs a tile with at least this much brightness spread (standard deviation), or there is nothing to line up on
const MIN_REFERENCE_SPREAD: f32 = 0.01;
/// The side of the tiles the spread is judged on: a few stars on a dark sky show in a tile, though hardly in the whole frame
const FEATURE_TILE: usize = 16;
/// When more than this share of a clip frame's pixels is rejected, the scene changed and the statistics restart from that frame
const CLIP_RESTART: f32 = 0.5;

/// Whether the frame has pixels and exactly the data its format and size call for.
fn frame_ok(frame: &Frame) -> bool {
    frame.width > 0 && frame.height > 0 && frame.data.len() == frame.format.size(frame.width, frame.height)
}

/// A picture's brightness, 0..1.
#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl Plane {
    /// The frame's luma (the Y plane, or Rec.709 for RGB), shrunk by a box filter by an integer factor. None when the frame is empty or its data doesn't fit its size.
    pub fn from_frame(frame: &Frame, factor: usize) -> Option<Plane> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let d = &frame.data;
        if !frame_ok(frame) {
            return None;
        }
        let luma = |i: usize| -> f32 {
            match frame.format {
                PixelFormat::I420 | PixelFormat::Nv12 => d[i] as f32,
                PixelFormat::Bgra => 0.0722 * d[i * 4] as f32 + 0.7152 * d[i * 4 + 1] as f32 + 0.2126 * d[i * 4 + 2] as f32,
                _ => 0.2126 * d[i * 4] as f32 + 0.7152 * d[i * 4 + 1] as f32 + 0.0722 * d[i * 4 + 2] as f32,
            }
        };
        let f = factor.max(1);
        let (ow, oh) = (w / f, h / f);
        if ow == 0 || oh == 0 {
            return None;
        }
        let mut data = vec![0.0; ow * oh];
        let scale = 1.0 / (255.0 * (f * f) as f32);
        for (oy, row) in data.chunks_exact_mut(ow).enumerate() {
            for (ox, out) in row.iter_mut().enumerate() {
                let mut sum = 0.0;
                for y in oy * f..oy * f + f {
                    for x in ox * f..ox * f + f {
                        sum += luma(y * w + x);
                    }
                }
                *out = sum * scale;
            }
        }
        Some(Plane { width: ow, height: oh, data })
    }

    /// Whether the plane is big enough to line up on and has a feature in it: some tile with enough brightness spread to hold a peak.
    fn alignable(&self) -> bool {
        if self.width < MIN_SQUARE || self.height < MIN_SQUARE || self.data.len() != self.width * self.height {
            return false;
        }
        let limit = MIN_REFERENCE_SPREAD * MIN_REFERENCE_SPREAD;
        (0..self.height).step_by(FEATURE_TILE).any(|y0| (0..self.width).step_by(FEATURE_TILE).any(|x0| self.tile_variance(x0, y0) >= limit))
    }

    /// The brightness variance of the tile whose top left is x0, y0 (smaller at the edges).
    fn tile_variance(&self, x0: usize, y0: usize) -> f32 {
        let (x1, y1) = ((x0 + FEATURE_TILE).min(self.width), (y0 + FEATURE_TILE).min(self.height));
        let (mut sum, mut sum2) = (0.0f64, 0.0f64);
        for y in y0..y1 {
            for &v in &self.data[y * self.width + x0..y * self.width + x1] {
                sum += f64::from(v);
                sum2 += f64::from(v) * f64::from(v);
            }
        }
        let n = ((x1 - x0) * (y1 - y0)) as f64;
        (sum2 / n - (sum / n) * (sum / n)).max(0.0) as f32
    }
}

/// Variance of the 3x3 Laplacian: higher is sharper.
pub fn sharpness(plane: &Plane) -> f32 {
    let (w, h) = (plane.width, plane.height);
    if w < 3 || h < 3 || plane.data.len() < w * h {
        return 0.0;
    }
    let p = &plane.data;
    let (mut sum, mut sum2, mut n) = (0.0f64, 0.0f64, 0usize);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let l = (p[i - 1] + p[i + 1] + p[i - w] + p[i + w] - 4.0 * p[i]) as f64;
            sum += l;
            sum2 += l * l;
            n += 1;
        }
    }
    let mean = sum / n as f64;
    (sum2 / n as f64 - mean * mean).max(0.0) as f32
}

/// How far a picture is displaced from another, in pixels (positive dx: to the right, positive dy: down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shift {
    pub dx: f32,
    pub dy: f32,
    /// How sharp the correlation peak was against the rest of the surface: noise gives a few, a real match tens.
    pub confidence: f32,
}

/// In-place radix-2 FFT of one row; tw holds the n/2 twiddles of the forward transform.
fn fft_row(re: &mut [f32], im: &mut [f32], tw: &[(f32, f32)], inverse: bool) {
    let n = re.len();
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let step = n / len;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (c, s) = tw[k * step];
                let s = if inverse { -s } else { s };
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * c - im[b] * s, re[b] * s + im[b] * c);
                (re[b], im[b]) = (re[a] - tr, im[a] - ti);
                (re[a], im[a]) = (re[a] + tr, im[a] + ti);
            }
        }
        len *= 2;
    }
}

fn transpose(a: &mut [f32], n: usize) {
    for y in 0..n {
        for x in y + 1..n {
            a.swap(y * n + x, x * n + y);
        }
    }
}

/// 2D FFT of an n x n picture (rows, then columns); the inverse isn't scaled.
fn fft2d(re: &mut [f32], im: &mut [f32], n: usize, inverse: bool) {
    let tw: Vec<(f32, f32)> = (0..n / 2).map(|k| {
        let a = -(TAU as f64) * k as f64 / n as f64;
        (a.cos() as f32, a.sin() as f32)
    }).collect();
    for pass in 0..2 {
        for (r, i) in re.chunks_exact_mut(n).zip(im.chunks_exact_mut(n)) {
            fft_row(r, i, &tw, inverse);
        }
        if pass == 0 {
            transpose(re, n);
            transpose(im, n);
        }
    }
    transpose(re, n);
    transpose(im, n);
}

/// The central n x n square of the plane, mean removed and under a Hann window, in the corner of an m x m field of zeros (so the correlation is linear).
fn windowed(plane: &Plane, n: usize, m: usize) -> Vec<f32> {
    let (x0, y0) = ((plane.width - n) / 2, (plane.height - n) / 2);
    let hann: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (TAU * (i as f32 + 0.5) / n as f32).cos()).collect();
    let mut out = vec![0.0; m * m];
    let mut sum = 0.0;
    for (y, row) in out.chunks_exact_mut(m).take(n).enumerate() {
        row[..n].copy_from_slice(&plane.data[(y0 + y) * plane.width + x0..][..n]);
        sum += row[..n].iter().sum::<f32>();
    }
    let mean = sum / (n * n) as f32;
    for (y, row) in out.chunks_exact_mut(m).take(n).enumerate() {
        for (x, v) in row[..n].iter_mut().enumerate() {
            *v = (*v - mean) * hann[y] * hann[x];
        }
    }
    out
}

/// Where a parabola through three samples peaks, relative to the middle one (-0.5..0.5).
fn parabola(a: f32, b: f32, c: f32) -> f32 {
    let d = a - 2.0 * b + c;
    if d.abs() < 1e-12 { 0.0 } else { (0.5 * (a - c) / d).clamp(-0.5, 0.5) }
}

/// How far `moved` is displaced from `reference`, by phase correlation on their central square. None when the planes are under 64 px, differ in size, hold less data than their size or have no clear peak.
pub fn shift_between(reference: &Plane, moved: &Plane) -> Option<Shift> {
    if (reference.width, reference.height) != (moved.width, moved.height) {
        return None;
    }
    let small = reference.width.min(reference.height);
    if small < MIN_SQUARE || [reference, moved].iter().any(|p| p.data.len() < p.width * p.height) {
        return None;
    }
    let n = (1usize << small.ilog2()).min(MAX_SQUARE);
    // zero-padded to twice the square, so shifts up to n don't wrap round into the wrong direction
    let m = n * 2;
    let (mut ar, mut ai) = (windowed(reference, n, m), vec![0.0; m * m]);
    let (mut br, mut bi) = (windowed(moved, n, m), vec![0.0; m * m]);
    fft2d(&mut ar, &mut ai, m, false);
    fft2d(&mut br, &mut bi, m, false);
    // normalised cross-power spectrum moved * conj(reference), written over a, with the fine detail (mostly noise) faded out
    let sigma = m as f32 * LOW_PASS;
    let bin = |k: usize| (if k > m / 2 { m - k } else { k }) as f32;
    for i in 0..m * m {
        let (r, q) = (br[i] * ar[i] + bi[i] * ai[i], bi[i] * ar[i] - br[i] * ai[i]);
        let mag = (r * r + q * q).sqrt();
        let (fx, fy) = (bin(i % m), bin(i / m));
        let fade = (-(fx * fx + fy * fy) / (2.0 * sigma * sigma)).exp();
        (ar[i], ai[i]) = if mag > 1e-9 { (r / mag * fade, q / mag * fade) } else { (0.0, 0.0) };
    }
    fft2d(&mut ar, &mut ai, m, true);
    let surface: Vec<f32> = ar.iter().map(|v| v / (m * m) as f32).collect();
    let (peak_at, peak) = surface.iter().enumerate().fold((0, f32::MIN), |best, (i, &v)| if v > best.1 { (i, v) } else { best });
    // the noise floor is judged where the two squares overlap by at least half, not over the faint far lags
    let lags: Vec<usize> = (0..n).map(|k| (k + m - n / 2) % m).collect();
    let mean_abs = lags.iter().flat_map(|&y| lags.iter().map(move |&x| (y, x))).map(|(y, x)| surface[y * m + x].abs()).sum::<f32>() / (n * n) as f32;
    if peak <= 0.0 || mean_abs < 1e-12 {
        return None;
    }
    let (px, py) = (peak_at % m, peak_at / m);
    let at = |x: usize, y: usize| surface[(y % m) * m + x % m];
    let fx = parabola(at(px + m - 1, py), peak, at(px + 1, py));
    let fy = parabola(at(px, py + m - 1), peak, at(px, py + 1));
    let wrap = |p: usize| if p > m / 2 { p as f32 - m as f32 } else { p as f32 };
    Some(Shift { dx: wrap(px) + fx, dy: wrap(py) + fy, confidence: peak / mean_abs })
}

/// How a stacker combines the frames it has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StackMode {
    /// The average
    Mean,
    /// The brightest value of each channel (star trails)
    Max,
    /// A running mean that, from the fifth frame on, leaves out values further than kappa standard deviations from it
    Clip { kappa: f32 },
}

/// Accumulates frames lined up with the first one.
pub struct Stacker {
    mode: StackMode,
    align: bool,
    /// The first frame is taken as the base even when it has no feature to line up on (a stack of one photo has nothing to line up with).
    any_base: bool,
    /// Frames whose correlation peak is weaker than this are refused
    pub min_confidence: f32,
    size: (usize, usize),
    reference: Option<Plane>,
    // R, G, B per pixel: the running mean (the brightest, in Max mode)
    mean: Vec<f32>,
    // the sum of squared differences from the mean (Clip mode only)
    spread: Vec<f32>,
    // how many frames each pixel has had
    weight: Vec<u32>,
    count: usize,
}

impl Stacker {
    pub fn new(mode: StackMode) -> Stacker {
        Stacker { mode, align: true, any_base: false, min_confidence: 8.0, size: (0, 0), reference: None, mean: Vec::new(), spread: Vec::new(), weight: Vec::new(), count: 0 }
    }

    /// How the frames are combined.
    pub fn mode(&self) -> StackMode {
        self.mode
    }

    /// Changes how frames are combined; a change starts the stack over.
    pub fn set_mode(&mut self, mode: StackMode) {
        if mode != self.mode {
            self.mode = mode;
            self.reset();
        }
    }

    /// Whether frames are lined up with the first one.
    pub fn align(&self) -> bool {
        self.align
    }

    /// Lines frames up with the first one (off for star trails, where the sky should move across a fixed frame); a change starts the stack over.
    pub fn set_align(&mut self, align: bool) {
        if align != self.align {
            self.align = align;
            self.reset();
        }
    }

    /// Whether a first frame with no feature to line up on is still taken as the base of the stack; later frames then can't line up with it.
    pub fn set_any_base(&mut self, any_base: bool) {
        self.any_base = any_base;
    }

    /// Empties the stack, keeping the settings.
    pub fn reset(&mut self) {
        self.size = (0, 0);
        self.reference = None;
        self.mean = Vec::new();
        self.spread = Vec::new();
        self.weight = Vec::new();
        self.count = 0;
    }

    /// How many frames are in the stack (after a clip restart, the ones since).
    pub fn count(&self) -> usize {
        self.count
    }

    /// Lines the frame up with the first one and adds it, returning the shift used. None (and not counted) when it is another size than
    /// the first, empty, short of data, can't be lined up with enough confidence, lands without overlapping the stack, or (with align on, unless any_base is set) has no feature to line up on as the first.
    /// In clip mode a frame that most pixels reject is a scene change (an exposure jump, say): the statistics restart from it and count() starts again at one.
    pub fn add(&mut self, frame: &Frame) -> Option<Shift> {
        if !frame_ok(frame) {
            return None;
        }
        let (w, h) = (frame.width as usize, frame.height as usize);
        if self.count > 0 && (w, h) != self.size {
            return None;
        }
        let factor = w.div_ceil(ALIGN_WIDTH);
        let plane = if self.align { Some(Plane::from_frame(frame, factor)?) } else { None };
        let shift = match (&self.reference, &plane) {
            (Some(reference), Some(plane)) => {
                let s = shift_between(reference, plane)?;
                if s.confidence < self.min_confidence {
                    return None;
                }
                Shift { dx: s.dx * factor as f32, dy: s.dy * factor as f32, confidence: s.confidence }
            }
            _ => Shift { dx: 0.0, dy: 0.0, confidence: f32::INFINITY },
        };
        if self.count == 0 && !self.any_base && plane.as_ref().is_some_and(|p| !p.alignable()) {
            return None;
        }
        let rgb = frame_rgb(frame).filter(|r| r.len() == w * h * 3)?;
        if self.count == 0 {
            self.size = (w, h);
            self.mean = vec![0.0; w * h * 3];
            self.spread = if matches!(self.mode, StackMode::Clip { .. }) { vec![0.0; w * h * 3] } else { Vec::new() };
            self.weight = vec![0; w * h];
            self.reference = plane;
        }
        self.take(&rgb, shift)
    }

    /// Puts a frame's pixels into the stack at the shift. None (and nothing changed) when the shift leaves no pixel of it over the stack.
    fn take(&mut self, rgb: &[u8], shift: Shift) -> Option<Shift> {
        let (covered, rejected) = self.accumulate(rgb, shift);
        if covered == 0 {
            return None;
        }
        if rejected as f32 > covered as f32 * CLIP_RESTART {
            self.mean.fill(0.0);
            self.spread.fill(0.0);
            self.weight.fill(0);
            self.accumulate(rgb, shift);
            self.count = 1;
        } else {
            self.count += 1;
        }
        Some(shift)
    }

    /// Puts the frame into the stack, returning how many pixels it covered and how many of those clipping rejected.
    fn accumulate(&mut self, rgb: &[u8], shift: Shift) -> (usize, usize) {
        let (mut covered, mut rejected) = (0, 0);
        let (w, h) = self.size;
        let at = |x: usize, y: usize, c: usize| rgb[(y * w + x) * 3 + c] as f32 / 255.0;
        for y in 0..h {
            let sy = y as f32 + shift.dy;
            if sy < 0.0 || sy > (h - 1) as f32 {
                continue;
            }
            let (y0, ty) = ((sy as usize).min(h.saturating_sub(2)), sy - (sy as usize).min(h.saturating_sub(2)) as f32);
            let y1 = (y0 + 1).min(h - 1);
            for x in 0..w {
                let sx = x as f32 + shift.dx;
                if sx < 0.0 || sx > (w - 1) as f32 {
                    continue;
                }
                let x0 = (sx as usize).min(w.saturating_sub(2));
                let tx = sx - x0 as f32;
                let x1 = (x0 + 1).min(w - 1);
                let mut v = [0.0; 3];
                for (c, out) in v.iter_mut().enumerate() {
                    let top = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
                    let bottom = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
                    *out = top * (1.0 - ty) + bottom * ty;
                }
                covered += 1;
                if !self.put(y * w + x, v) {
                    rejected += 1;
                }
            }
        }
        (covered, rejected)
    }

    /// Takes one pixel's value from a frame into the stack; false when clipping left it out.
    fn put(&mut self, i: usize, v: [f32; 3]) -> bool {
        let n = self.weight[i];
        let mean = &mut self.mean[i * 3..i * 3 + 3];
        match self.mode {
            StackMode::Max => {
                for c in 0..3 {
                    mean[c] = if n == 0 { v[c] } else { mean[c].max(v[c]) };
                }
            }
            StackMode::Mean => {
                for c in 0..3 {
                    mean[c] += (v[c] - mean[c]) / (n + 1) as f32;
                }
            }
            StackMode::Clip { kappa } => {
                let spread = &mut self.spread[i * 3..i * 3 + 3];
                if n >= CLIP_AFTER && (0..3).any(|c| (v[c] - mean[c]).abs() > kappa * (spread[c] / n as f32).sqrt().max(MIN_SPREAD)) {
                    return false;
                }
                for c in 0..3 {
                    let d = v[c] - mean[c];
                    mean[c] += d / (n + 1) as f32;
                    spread[c] += d * (v[c] - mean[c]);
                }
            }
        }
        self.weight[i] = n + 1;
        true
    }

    /// The stack as an RGBA frame; None while it is empty. Pixels no frame covered are black.
    pub fn result(&self) -> Option<Frame> {
        if self.count == 0 {
            return None;
        }
        let mut data = Vec::with_capacity(self.weight.len() * 4);
        for (px, &n) in self.mean.as_chunks::<3>().0.iter().zip(&self.weight) {
            for &v in px {
                data.push(if n == 0 { 0 } else { (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8 });
            }
            data.push(255);
        }
        Some(Frame::new(self.size.0 as u32, self.size.1 as u32, PixelFormat::Rgba, data))
    }
}

/// Keeps the sharpest frames of a burst, scoring each on arrival so only `capacity` are ever held. A frame with no pixels or the wrong amount of data is never kept.
pub struct Lucky {
    capacity: usize,
    // sharpest first
    kept: Vec<(f32, Frame)>,
}

impl Lucky {
    /// Holds up to `capacity` frames (at least one).
    pub fn new(capacity: usize) -> Lucky {
        Lucky { capacity: capacity.max(1), kept: Vec::new() }
    }

    /// Scores the frame and keeps it if it is among the sharpest so far; whether it was kept.
    pub fn add(&mut self, frame: Frame) -> bool {
        if !frame_ok(&frame) {
            return false;
        }
        let score = Plane::from_frame(&frame, (frame.width as usize).div_ceil(ALIGN_WIDTH)).map_or(0.0, |p| sharpness(&p));
        if self.kept.len() >= self.capacity && self.kept.last().is_some_and(|(worst, _)| score <= *worst) {
            return false;
        }
        let at = self.kept.partition_point(|(s, _)| *s >= score);
        self.kept.insert(at, (score, frame));
        self.kept.truncate(self.capacity);
        true
    }

    /// The kept frames, sharpest first.
    pub fn best(&self) -> Vec<Frame> {
        self.kept.iter().map(|(_, f)| f.clone()).collect()
    }

    /// The kept frames, sharpest first, handed over without copying them.
    pub fn into_best(self) -> Vec<Frame> {
        self.kept.into_iter().map(|(_, f)| f).collect()
    }
}

#[cfg(test)]
#[path = "stack_tests.rs"]
mod tests;
