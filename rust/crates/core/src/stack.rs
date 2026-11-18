//! Frame alignment and stacking for lucky imaging, live stacking and star trails: luma planes, a sharpness score, phase correlation
//! for the shift between two pictures, and a stacker that lines frames up with the first and averages, maxes or sigma-clips them.

use crate::frame::{Frame, PixelFormat};
use crate::image::frame_rgb;
use std::f32::consts::TAU;

/// The smallest and largest square the correlation looks at
const MIN_SQUARE: usize = 64;
const MAX_SQUARE: usize = 512;
/// The correlation's frequency fade, as a share of the square's size
const LOW_PASS: f32 = 0.1;
/// Frames wider than this are aligned on a smaller copy
const ALIGN_WIDTH: usize = 1024;
/// Clip mode takes everything until it has this many frames to judge by
const CLIP_AFTER: u32 = 5;
/// Pixel values are 0..1, so this is a bit under half a level
const MIN_SPREAD: f32 = 0.002;

/// A picture's brightness, 0..1.
#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl Plane {
    /// The frame's luma (the Y plane, or Rec.709 for RGB), shrunk by a box filter by an integer factor. None when the frame's data is too short.
    pub fn from_frame(frame: &Frame, factor: usize) -> Option<Plane> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let d = &frame.data;
        if w == 0 || h == 0 || d.len() < frame.format.size(frame.width, frame.height) {
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

/// The central n x n square of the plane, mean removed and under a Hann window, as the real part of a complex picture.
fn windowed(plane: &Plane, n: usize) -> Vec<f32> {
    let (x0, y0) = ((plane.width - n) / 2, (plane.height - n) / 2);
    let hann: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (TAU * (i as f32 + 0.5) / n as f32).cos()).collect();
    let mut out = vec![0.0; n * n];
    for (y, row) in out.chunks_exact_mut(n).enumerate() {
        row.copy_from_slice(&plane.data[(y0 + y) * plane.width + x0..][..n]);
    }
    let mean = out.iter().sum::<f32>() / (n * n) as f32;
    for (y, row) in out.chunks_exact_mut(n).enumerate() {
        for (x, v) in row.iter_mut().enumerate() {
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

/// How far `moved` is displaced from `reference`, by phase correlation on their central square. None when the planes are under 64 px, differ in size or have no clear peak.
pub fn shift_between(reference: &Plane, moved: &Plane) -> Option<Shift> {
    if (reference.width, reference.height) != (moved.width, moved.height) {
        return None;
    }
    let small = reference.width.min(reference.height);
    if small < MIN_SQUARE {
        return None;
    }
    let n = (1usize << small.ilog2()).min(MAX_SQUARE);
    let (mut ar, mut ai) = (windowed(reference, n), vec![0.0; n * n]);
    let (mut br, mut bi) = (windowed(moved, n), vec![0.0; n * n]);
    fft2d(&mut ar, &mut ai, n, false);
    fft2d(&mut br, &mut bi, n, false);
    // normalised cross-power spectrum moved * conj(reference), written over a, with the fine detail (mostly noise) faded out
    let sigma = n as f32 * LOW_PASS;
    let bin = |k: usize| (if k > n / 2 { n - k } else { k }) as f32;
    for i in 0..n * n {
        let (r, m) = (br[i] * ar[i] + bi[i] * ai[i], bi[i] * ar[i] - br[i] * ai[i]);
        let mag = (r * r + m * m).sqrt();
        let (fx, fy) = (bin(i % n), bin(i / n));
        let fade = (-(fx * fx + fy * fy) / (2.0 * sigma * sigma)).exp();
        (ar[i], ai[i]) = if mag > 1e-9 { (r / mag * fade, m / mag * fade) } else { (0.0, 0.0) };
    }
    fft2d(&mut ar, &mut ai, n, true);
    let surface: Vec<f32> = ar.iter().map(|v| v / (n * n) as f32).collect();
    let (peak_at, peak) = surface.iter().enumerate().fold((0, f32::MIN), |best, (i, &v)| if v > best.1 { (i, v) } else { best });
    let mean_abs = surface.iter().map(|v| v.abs()).sum::<f32>() / (n * n) as f32;
    if peak <= 0.0 || mean_abs < 1e-12 {
        return None;
    }
    let (px, py) = (peak_at % n, peak_at / n);
    let at = |x: usize, y: usize| surface[(y % n) * n + x % n];
    let fx = parabola(at(px + n - 1, py), peak, at(px + 1, py));
    let fy = parabola(at(px, py + n - 1), peak, at(px, py + 1));
    let wrap = |p: usize| if p > n / 2 { p as f32 - n as f32 } else { p as f32 };
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
    pub mode: StackMode,
    /// Line frames up with the first one (off for star trails, where the sky should move across a fixed frame)
    pub align: bool,
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
        Stacker { mode, align: true, min_confidence: 6.0, size: (0, 0), reference: None, mean: Vec::new(), spread: Vec::new(), weight: Vec::new(), count: 0 }
    }

    /// How many frames are in the stack.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Lines the frame up with the first one and adds it, returning the shift used. None (and not counted) when it is another size than
    /// the first, can't be read, or can't be lined up with enough confidence.
    pub fn add(&mut self, frame: &Frame) -> Option<Shift> {
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
        let rgb = frame_rgb(frame).filter(|r| r.len() == w * h * 3)?;
        if self.count == 0 {
            self.size = (w, h);
            self.mean = vec![0.0; w * h * 3];
            self.spread = if matches!(self.mode, StackMode::Clip { .. }) { vec![0.0; w * h * 3] } else { Vec::new() };
            self.weight = vec![0; w * h];
            self.reference = plane;
        }
        self.accumulate(&rgb, shift);
        self.count += 1;
        Some(shift)
    }

    fn accumulate(&mut self, rgb: &[u8], shift: Shift) {
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
                self.put(y * w + x, v);
            }
        }
    }

    /// Takes one pixel's value from a frame into the stack.
    fn put(&mut self, i: usize, v: [f32; 3]) {
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
                    return;
                }
                for c in 0..3 {
                    let d = v[c] - mean[c];
                    mean[c] += d / (n + 1) as f32;
                    spread[c] += d * (v[c] - mean[c]);
                }
            }
        }
        self.weight[i] = n + 1;
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

/// The indexes of the sharpest `keep` share of the frames (at least one), sharpest first.
pub fn lucky(frames: &[Frame], keep: f32) -> Vec<usize> {
    let mut scored: Vec<(usize, f32)> = frames.iter().enumerate().map(|(i, f)| {
        let score = Plane::from_frame(f, (f.width as usize).div_ceil(ALIGN_WIDTH)).map_or(0.0, |p| sharpness(&p));
        (i, score)
    }).collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let take = ((frames.len() as f32 * keep.clamp(0.0, 1.0)).ceil() as usize).clamp(1, frames.len().max(1));
    scored.into_iter().take(take).map(|(i, _)| i).collect()
}

#[cfg(test)]
#[path = "stack_tests.rs"]
mod tests;
