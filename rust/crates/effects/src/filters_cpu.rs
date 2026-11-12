//! The Filters plugin's pixel maths on the CPU, for when the balancer keeps its steps off the graphics card. Each function is the same maths
//! as the shader of the same name (filter_*.wgsl) and takes the same numbers, so a picture comes out alike either way; the card works on
//! every pixel with sparse taps where this goes the cheaper way round (a blurred copy at a fraction of the size, one bilateral ring). Colours
//! are the camera's own 0..1 values, brightness is Rec. 601 luma, and sizes are in pixels of a 1080p frame, scaled to the picture they run on.

use crate::cpu::Rgb;
use crate::imgops::{blur_quick, Gray};

/// How far a sharpened pixel may go past the brightest or darkest of its 3 x 3 neighbours, so a strong edge doesn't get a halo.
pub const HALO: f32 = 0.03;
const LUMA: [f32; 3] = [0.299, 0.587, 0.114];
/// What clarity's range weight forgives: neighbours this much brighter or darker count less, so an edge isn't blurred across.
pub const CLARITY_RANGE: f32 = 0.1;
/// The grid dehaze looks for the darkest colour on is 5 x 5 taps, this many pixels of a 1080p frame apart.
pub const DEHAZE_TAP_1080: f32 = 5.0;
/// The air light dehaze assumes: nearly white, since the haze is a lamp, a window or a dirty lens.
pub const AIR: f32 = 0.92;
/// The most haze dehaze takes out (1.0 would call a flat grey picture all haze) and the least light it lets a pixel keep.
pub const DEHAZE_MOST: f32 = 0.6;
pub const DEHAZE_FLOOR: f32 = 0.25;
/// Skin smoothing's bilateral grid is 7 x 7 taps this many pixels of a 1080p frame apart, and forgives this much colour difference.
pub const SKIN_TAP_1080: f32 = 2.0;
pub const SKIN_RANGE: f32 = 0.09;
/// Where glow starts to pick up brightness, and where it picks up all of it.
pub const GLOW_FROM: f32 = 0.55;

pub struct Sharpen {
    /// How much of the detail is added back (0..2)
    pub amount: f32,
    /// The blur the detail is what's left over from, in pixels of a 1080p frame
    pub sigma: f32,
    /// Detail smaller than this (in brightness, 0..1) is left alone, and it comes in fully at twice this
    pub gate: f32,
}

pub struct Clarity {
    /// -1.5..1.5: more local contrast, or less
    pub amount: f32,
}

pub struct Dehaze {
    pub strength: f32,
}

pub struct DenoiseSpace {
    /// How different two colours can be and still be called the same thing, 0..1
    pub range: f32,
}

pub struct DenoiseTime {
    /// How different the picture can be from the last one, as noise, before it counts as something moving
    pub tolerance: f32,
    /// The most of the last picture that is kept (0..1)
    pub keep: f32,
}

pub struct Skin {
    pub strength: f32,
}

pub struct Finish {
    /// -1..1: darken the corners, or lighten them
    pub vignette: f32,
    /// 0..1: where the dark starts, from the middle out
    pub vignette_inner: f32,
    /// 0 follows the frame's shape, 1 is a circle
    pub vignette_round: f32,
    pub glow: f32,
    /// How far the glow spreads, in pixels of a 1080p frame
    pub glow_radius: f32,
    pub grain: f32,
    /// A grain's size in pixels of a 1080p frame
    pub grain_size: f32,
}

fn to_u8(v: f32) -> u8 {
    (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

fn smoothstep(from: f32, to: f32, x: f32) -> f32 {
    let t = ((x - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn luma_of(r: f32, g: f32, b: f32) -> f32 {
    LUMA[0] * r + LUMA[1] * g + LUMA[2] * b
}

/// Pixels of a 1080p frame to pixels of this one.
pub fn scale_of(h: usize) -> f32 {
    h as f32 / 1080.0
}

/// The brightness of every pixel, 0..1.
fn luma(img: &Rgb) -> Vec<f32> {
    let w = img.w;
    let mut out = vec![0f32; img.w * img.h];
    in_bands(&mut out, w, |first, band| {
        for (r, row) in band.chunks_exact_mut(w).enumerate() {
            for (o, p) in row.iter_mut().zip(img.data[(first + r) * w * 3..][..w * 3].chunks_exact(3)) {
                *o = luma_of(p[0] as f32, p[1] as f32, p[2] as f32) / 255.0;
            }
        }
    });
    out
}

/// Adds a change of brightness to every colour of a pixel, which moves it along the grey line and keeps its hue.
fn add_luma(img: &mut Rgb, changes: &[f32]) {
    let w = img.w;
    in_bands(&mut img.data, w * 3, |first, band| {
        for (r, row) in band.chunks_exact_mut(w * 3).enumerate() {
            for (p, d) in row.chunks_exact_mut(3).zip(&changes[(first + r) * w..][..w]) {
                if *d != 0.0 {
                    for c in p.iter_mut() {
                        *c = to_u8(*c as f32 / 255.0 + d);
                    }
                }
            }
        }
    });
}

/// A Gaussian blur of a plane, a pass along each way, the edges repeated (as the shader's clamped samples do).
fn gaussian(v: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let n = (3.0 * sigma).ceil() as usize;
    let mut k: Vec<f32> = (0..=2 * n).map(|i| (-((i as f32 - n as f32).powi(2)) / (2.0 * sigma * sigma)).exp()).collect();
    let total: f32 = k.iter().sum();
    k.iter_mut().for_each(|t| *t /= total);
    let mut across = vec![0f32; w * h];
    in_bands(&mut across, w, |first, band| {
        let mut line = vec![0f32; w + 2 * n];
        for (r, row) in band.chunks_exact_mut(w).enumerate() {
            let src = &v[(first + r) * w..][..w];
            line[..n].fill(src[0]);
            line[n..n + w].copy_from_slice(src);
            line[n + w..].fill(src[w - 1]);
            for (j, kj) in k.iter().enumerate() {
                for (o, l) in row.iter_mut().zip(&line[j..]) {
                    *o += kj * l;
                }
            }
        }
    });
    let mut out = vec![0f32; w * h];
    in_bands(&mut out, w, |first, band| {
        for (r, row) in band.chunks_exact_mut(w).enumerate() {
            for (j, kj) in k.iter().enumerate() {
                let y = (first as isize + r as isize + j as isize - n as isize).clamp(0, h as isize - 1) as usize;
                for (o, a) in row.iter_mut().zip(&across[y * w..][..w]) {
                    *o += kj * a;
                }
            }
        }
    });
    out
}

/// The smallest and largest of a pixel and its eight neighbours (a pass along each way, which is all the 3 x 3 asks for).
fn min_max_3x3(v: &[f32], w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut lo, mut hi) = (vec![0f32; w * h], vec![0f32; w * h]);
    for y in 0..h {
        let row = &v[y * w..][..w];
        for x in 0..w {
            let (a, b, c) = (row[x.saturating_sub(1)], row[x], row[(x + 1).min(w - 1)]);
            lo[y * w + x] = a.min(b).min(c);
            hi[y * w + x] = a.max(b).max(c);
        }
    }
    let (mut lo2, mut hi2) = (vec![0f32; w * h], vec![0f32; w * h]);
    for y in 0..h {
        let (y0, y1) = (y.saturating_sub(1) * w, (y + 1).min(h - 1) * w);
        for x in 0..w {
            lo2[y * w + x] = lo[y0 + x].min(lo[y * w + x]).min(lo[y1 + x]);
            hi2[y * w + x] = hi[y0 + x].max(hi[y * w + x]).max(hi[y1 + x]);
        }
    }
    (lo2, hi2)
}

pub fn sharpen(img: &mut Rgb, p: &Sharpen) {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 || p.amount <= 0.0 {
        return;
    }
    let changes = sharpen_changes(&luma(img), w, h, p);
    add_luma(img, &changes);
}

/// What sharpen changes the brightness of each pixel by, from the brightness plane (0..1).
pub fn sharpen_changes(y: &[f32], w: usize, h: usize, p: &Sharpen) -> Vec<f32> {
    let sigma = (p.sigma * scale_of(h)).max(0.4);
    let blur = gaussian(&y, w, h, sigma);
    let (lo, hi) = min_max_3x3(&y, w, h);
    let mut changes = vec![0f32; w * h];
    in_bands(&mut changes, w, |first, band| {
        for (o, i) in band.iter_mut().zip(first * w..) {
            let d = y[i] - blur[i];
            let gate = if p.gate > 0.0 { smoothstep(p.gate, 2.0 * p.gate, d.abs()) } else { 1.0 };
            *o = (y[i] + p.amount * d * gate).clamp(lo[i] - HALO, hi[i] + HALO) - y[i];
        }
    });
    changes
}

/// The spacing, in whole pixels, of clarity's taps and the size of the picture it blurs.
pub fn clarity_step(h: usize) -> usize {
    ((24.0 * scale_of(h) / 4.0 + 0.5).floor() as usize).max(1)
}

/// A picture of `v`'s brightness shrunk by `f`, each pixel the mean of the pixels it covers.
fn shrink(v: &[f32], w: usize, h: usize, f: usize) -> (Vec<f32>, usize, usize) {
    let (sw, sh) = (w.div_ceil(f), h.div_ceil(f));
    let mut out = vec![0f32; sw * sh];
    in_bands(&mut out, sw, |first, band| {
        for (r, row) in band.chunks_exact_mut(sw).enumerate() {
            let sy = first + r;
            for (sx, o) in row.iter_mut().enumerate() {
                let (mut sum, mut n) = (0.0, 0.0);
                for y in sy * f..((sy + 1) * f).min(h) {
                    for x in sx * f..((sx + 1) * f).min(w) {
                        sum += v[y * w + x];
                        n += 1.0;
                    }
                }
                *o = sum / n;
            }
        }
    });
    (out, sw, sh)
}

/// exp(-d^2 / 2 range^2) for a brightness difference of 0..1 in 256 steps.
fn range_table(range: f32) -> Vec<f32> {
    (0..256).map(|i| (-((i as f32 / 255.0).powi(2)) / (2.0 * range * range)).exp()).collect()
}

/// `small` (of a picture `f` times the size) scaled back up to w x h, linearly between the centres of its pixels, but each of the four it
/// takes from counts for less the more its own brightness (`guide`) differs from the brightness here (`full`), so an edge's blur doesn't
/// spill across it (a joint bilateral upsample). Where all four differ too much, which is a thin line the small picture lost, the pixel
/// is its own base and nothing changes.
fn grow(small: &[f32], guide: &[f32], full: &[f32], sw: usize, sh: usize, f: usize, w: usize, h: usize) -> Vec<f32> {
    let axis = |n: usize, sn: usize| -> Vec<(usize, usize, f32)> {
        (0..n).map(|i| {
            let p = ((i as f32 + 0.5) / f as f32 - 0.5).max(0.0);
            let i0 = (p.floor() as usize).min(sn - 1);
            (i0, (i0 + 1).min(sn - 1), p - i0 as f32)
        }).collect()
    };
    let (xs, ys) = (axis(w, sw), axis(h, sh));
    let table = range_table(CLARITY_RANGE);
    let mut out = vec![0f32; w * h];
    in_bands(&mut out, w, |first, band| {
        for (r, row) in band.chunks_exact_mut(w).enumerate() {
            let y = first + r;
            let (y0, y1, fy) = ys[y];
            for (x, o) in row.iter_mut().enumerate() {
                let (x0, x1, fx) = xs[x];
                let here = full[y * w + x];
                let (mut sum, mut total) = (0.0, 0.0);
                for (sy, wy) in [(y0, 1.0 - fy), (y1, fy)] {
                    for (sx, wx) in [(x0, 1.0 - fx), (x1, fx)] {
                        let i = sy * sw + sx;
                        let weight = wx * wy * table[(((guide[i] - here).abs() * 255.0 + 0.5) as usize).min(255)];
                        sum += weight * small[i];
                        total += weight;
                    }
                }
                *o = if total > 1e-4 { sum / total } else { here };
            }
        }
    });
    out
}

pub fn clarity(img: &mut Rgb, p: &Clarity) {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 || p.amount == 0.0 {
        return;
    }
    let changes = clarity_changes(&luma(img), w, h, p);
    add_luma(img, &changes);
}

/// What clarity changes the brightness of each pixel by, from the brightness plane (0..1).
pub fn clarity_changes(y: &[f32], w: usize, h: usize, p: &Clarity) -> Vec<f32> {
    let f = clarity_step(h);
    let (small, sw, sh) = shrink(y, w, h, f);
    // An edge-aware blur on the small picture: the nearer and the more alike a neighbour, the more it counts
    let spatial: Vec<f32> = (-4i32..=4).flat_map(|j| (-4i32..=4).map(move |i| (-((i * i + j * j) as f32) / 8.0).exp())).collect();
    let table = range_table(CLARITY_RANGE);
    let mut base_small = vec![0f32; sw * sh];
    in_bands(&mut base_small, sw, |first, band| {
        for (r, row) in band.chunks_exact_mut(sw).enumerate() {
            let sy = (first + r) as isize;
            for (sx, o) in row.iter_mut().enumerate() {
                let sx = sx as isize;
                let centre = small[sy as usize * sw + sx as usize];
                let (mut sum, mut total) = (0.0, 0.0);
                for (n, (j, i)) in (-4isize..=4).flat_map(|j| (-4isize..=4).map(move |i| (j, i))).enumerate() {
                    let q = small[(sy + j).clamp(0, sh as isize - 1) as usize * sw + (sx + i).clamp(0, sw as isize - 1) as usize];
                    let weight = spatial[n] * table[(((q - centre).abs() * 255.0 + 0.5) as usize).min(255)];
                    sum += weight * q;
                    total += weight;
                }
                *o = sum / total;
            }
        }
    });
    let base = grow(&base_small, &small, y, sw, sh, f, w, h);
    let mut changes = vec![0f32; w * h];
    in_bands(&mut changes, w, |first, band| {
        for (o, i) in band.iter_mut().zip(first * w..) {
            let tones = 0.3 + 0.7 * 4.0 * y[i] * (1.0 - y[i]);
            *o = (p.amount * (y[i] - base[i]) * tones).clamp(-1.0, 1.0);
        }
    });
    changes
}

/// The brightness of a video range Y plane (16..235) as 0..1, the way the RGB it decodes to has it.
pub fn plane_luma(plane: &[u8], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0f32; w * h];
    in_bands(&mut out, w, |first, band| {
        for (o, y) in band.iter_mut().zip(&plane[first * w..]) {
            *o = (*y as f32 - 16.0) / 219.0;
        }
    });
    out
}

/// Adds changes of brightness (0..1) to a video range Y plane. The colours are the planes' own and stay as they were, which is what the
/// same change to all of R, G and B leaves of a pixel.
pub fn add_plane_luma(plane: &mut [u8], w: usize, changes: &[f32]) {
    in_bands(plane, w, |first, band| {
        for (y, d) in band.iter_mut().zip(&changes[first * w..]) {
            if *d != 0.0 {
                *y = (*y as f32 + d * 219.0 + 0.5).clamp(16.0, 235.0) as u8;
            }
        }
    });
}

/// The spacing, in whole pixels, of dehaze's taps.
pub fn dehaze_step(h: usize) -> usize {
    ((DEHAZE_TAP_1080 * scale_of(h) + 0.5).floor() as usize).max(1)
}

/// Each pixel's smallest value within 2 taps either way along one axis, edges repeated.
fn min_along(v: &[u8], w: usize, h: usize, step: usize, vertical: bool) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    in_bands(&mut out, w, |first, band| {
        let pad = 2 * step;
        let mut padded = vec![0u8; w + 2 * pad];
        for (r, row) in band.chunks_exact_mut(w).enumerate() {
            let y = first + r;
            if vertical {
                let rows: [&[u8]; 5] = std::array::from_fn(|k| {
                    let yy = (y as isize + (k as isize - 2) * step as isize).clamp(0, h as isize - 1) as usize;
                    &v[yy * w..][..w]
                });
                for (x, o) in row.iter_mut().enumerate() {
                    *o = rows[0][x].min(rows[1][x]).min(rows[2][x]).min(rows[3][x]).min(rows[4][x]);
                }
            } else {
                let src = &v[y * w..][..w];
                padded[..pad].fill(src[0]);
                padded[pad..pad + w].copy_from_slice(src);
                padded[pad + w..].fill(src[w - 1]);
                let at: [&[u8]; 5] = std::array::from_fn(|k| &padded[k * step..][..w]);
                for (x, o) in row.iter_mut().enumerate() {
                    *o = at[0][x].min(at[1][x]).min(at[2][x]).min(at[3][x]).min(at[4][x]);
                }
            }
        }
    });
    out
}

/// The picture's gamma that dehaze undoes and redoes its maths through: the haze adds light, and light adds up in linear values.
pub const GAMMA: f32 = 2.2;

pub fn dehaze(img: &mut Rgb, p: &Dehaze) {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 || p.strength <= 0.0 {
        return;
    }
    // the darkest colour of every pixel, then of its patch (a minimum stays the minimum through the gamma, so this stays in 8 bits)
    let mut lowest = vec![0u8; w * h];
    in_bands(&mut lowest, w, |first, band| {
        for (r, row) in band.chunks_exact_mut(w).enumerate() {
            for (o, c) in row.iter_mut().zip(img.data[(first + r) * w * 3..][..w * 3].chunks_exact(3)) {
                *o = c[0].min(c[1]).min(c[2]);
            }
        }
    });
    let step = dehaze_step(h);
    let dark = min_along(&min_along(&lowest, w, h, step, false), w, h, step, true);
    let haze = p.strength * DEHAZE_MOST;
    let air = AIR.powf(GAMMA);
    // by the darkest value of the patch: how much of the light gets through; by a colour: its linear value; and back by sqrt of a linear value
    let through: Vec<f32> = (0..256).map(|m| (1.0 - haze * (m as f32 / 255.0 / AIR).powf(GAMMA)).max(DEHAZE_FLOOR)).collect();
    let linear: Vec<f32> = (0..256).map(|i| (i as f32 / 255.0).powf(GAMMA)).collect();
    let encode: Vec<u8> = (0..=4096).map(|i| to_u8((i as f32 / 4096.0).powf(2.0 / GAMMA))).collect();
    in_bands(&mut img.data, w * 3, |first, band| {
        for (r, row) in band.chunks_exact_mut(w * 3).enumerate() {
            for (c, d) in row.chunks_exact_mut(3).zip(&dark[(first + r) * w..][..w]) {
                let t = through[*d as usize];
                for v in c.iter_mut() {
                    let j = ((linear[*v as usize] - air) / t + air).clamp(0.0, 1.0);
                    *v = encode[(j.sqrt() * 4096.0 + 0.5) as usize];
                }
            }
        }
    });
}

/// The 12 neighbours denoise looks at around a pixel, with how much their distance counts: the ring at 1 and the four straight out at 2.
pub fn space_taps() -> [(isize, isize, f32); 12] {
    let offsets: [(isize, isize); 12] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, -1), (1, -1), (-1, 1), (2, 0), (-2, 0), (0, 2), (0, -2)];
    offsets.map(|(x, y)| (x, y, (-((x * x + y * y) as f32) / (2.0 * 1.5 * 1.5)).exp()))
}

/// Runs `work(first_row, rows)` over bands of an output picture on a few threads.
fn in_bands<T: Send>(out: &mut [T], row_len: usize, work: impl Fn(usize, &mut [T]) + Sync) {
    let rows = out.len() / row_len.max(1);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(4).min(rows.max(1));
    if threads <= 1 || rows < 64 {
        return work(0, out);
    }
    let per = rows.div_ceil(threads);
    std::thread::scope(|s| {
        for (n, band) in out.chunks_mut(per * row_len).enumerate() {
            let work = &work;
            s.spawn(move || work(n * per, band));
        }
    });
}

pub fn denoise_space(img: &mut Rgb, p: &DenoiseSpace) {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 || p.range <= 0.0 {
        return;
    }
    let taps = space_taps();
    // exp(-d2 / 2 range^2) by the squared difference of two 8-bit colours in steps of 32
    let table: Vec<f32> = (0..=(3 * 255 * 255 / 32 + 1)).map(|i| (-(((i * 32 + 16) as f32) / (255.0 * 255.0)) / (2.0 * p.range * p.range)).exp()).collect();
    // The picture with its edge repeated 2 pixels out, so every tap is a plain offset
    let stride = (w + 4) * 3;
    let mut padded = vec![0u8; stride * (h + 4)];
    for y in 0..h + 4 {
        let src = &img.data[(y.saturating_sub(2).min(h - 1)) * w * 3..][..w * 3];
        let row = &mut padded[y * stride..][..stride];
        row[..6].copy_from_slice(&[src[0], src[1], src[2], src[0], src[1], src[2]]);
        row[6..6 + w * 3].copy_from_slice(src);
        let last = &src[(w - 1) * 3..];
        row[6 + w * 3..].copy_from_slice(&[last[0], last[1], last[2], last[0], last[1], last[2]]);
    }
    let offsets: Vec<(isize, f32)> = taps.iter().map(|&(dx, dy, near)| (dy * stride as isize + dx * 3, near)).collect();
    in_bands(&mut img.data, w * 3, |first, band| {
        for (r, row) in band.chunks_exact_mut(w * 3).enumerate() {
            let y = first + r;
            for x in 0..w {
                let at = ((y + 2) * stride + (x + 2) * 3) as isize;
                let c = [padded[at as usize] as i32, padded[at as usize + 1] as i32, padded[at as usize + 2] as i32];
                let mut sum = [c[0] as f32, c[1] as f32, c[2] as f32];
                let mut total = 1.0f32;
                for &(off, near) in &offsets {
                    let q = &padded[(at + off) as usize..][..3];
                    let (d0, d1, d2) = (q[0] as i32 - c[0], q[1] as i32 - c[1], q[2] as i32 - c[2]);
                    let weight = near * table[((d0 * d0 + d1 * d1 + d2 * d2) as usize) >> 5];
                    sum[0] += weight * q[0] as f32;
                    sum[1] += weight * q[1] as f32;
                    sum[2] += weight * q[2] as f32;
                    total += weight;
                }
                let o = &mut row[x * 3..][..3];
                for k in 0..3 {
                    o[k] = (sum[k] / total + 0.5) as u8;
                }
            }
        }
    });
}

/// What denoising over time remembers of one stream: the last picture it made, as floats so a slow change isn't rounded away.
#[derive(Default)]
pub struct Memory {
    last: Vec<f32>,
    w: usize,
    h: usize,
    source: String,
    time_ns: u64,
}

impl Memory {
    /// How long a last picture still goes with the next one (shader::HISTORY_MAX_AGE_NS), the same as on the card.
    pub const MAX_AGE_NS: u64 = telescope_core::shader::HISTORY_MAX_AGE_NS;

    /// Forgets everything.
    pub fn reset(&mut self) {
        *self = Memory::default();
    }

    /// Whether the last picture goes with one of this size, from this source, at this time.
    fn goes_with(&self, w: usize, h: usize, source: &str, time_ns: u64) -> bool {
        !self.last.is_empty() && (self.w, self.h) == (w, h) && self.source == source && time_ns >= self.time_ns && time_ns - self.time_ns < Self::MAX_AGE_NS
    }
}

pub fn denoise_time(img: &mut Rgb, memory: &mut Memory, p: &DenoiseTime, source: &str, time_ns: u64) {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 {
        return;
    }
    if !memory.goes_with(w, h, source, time_ns) {
        // Start again from this picture
        memory.last = img.data.iter().map(|v| *v as f32 / 255.0).collect();
        (memory.w, memory.h, memory.time_ns) = (w, h, time_ns);
        memory.source = source.to_string();
        return;
    }
    // How different each pixel is from the last picture, and that averaged over its 3 x 3 so a speck of noise doesn't count as movement
    let mut diff = vec![0f32; w * h];
    in_bands(&mut diff, w, |first, band| {
        for (r, row) in band.chunks_exact_mut(w).enumerate() {
            let at = (first + r) * w;
            for (x, o) in row.iter_mut().enumerate() {
                let (c, l) = (&img.data[(at + x) * 3..][..3], &memory.last[(at + x) * 3..][..3]);
                *o = ((c[0] as f32 / 255.0 - l[0]).abs() + (c[1] as f32 / 255.0 - l[1]).abs() + (c[2] as f32 / 255.0 - l[2]).abs()) / 3.0;
            }
        }
    });
    let mut across = vec![0f32; w * h];
    for y in 0..h {
        let row = &diff[y * w..][..w];
        for x in 0..w {
            across[y * w + x] = row[x.saturating_sub(1)] + row[x] + row[(x + 1).min(w - 1)];
        }
    }
    let mut weights = vec![0f32; w * h];
    in_bands(&mut weights, w, |first, band| {
        for (r, row) in band.chunks_exact_mut(w).enumerate() {
            let y = first + r;
            let (y0, y1) = (y.saturating_sub(1) * w, (y + 1).min(h - 1) * w);
            for (x, o) in row.iter_mut().enumerate() {
                let mean = (across[y0 + x] + across[y * w + x] + across[y1 + x]) / 9.0;
                // the mean lets a lone pixel that changed a lot pass for noise, so a pixel that changed far more than noise does counts alone
                let alone = 1.0 - smoothstep(3.0 * p.tolerance, 6.0 * p.tolerance, diff[y * w + x]);
                *o = p.keep * (1.0 - smoothstep(p.tolerance, 2.0 * p.tolerance, mean)) * alone;
            }
        }
    });
    let width = w * 3;
    for ((row, last), k) in img.data.chunks_exact_mut(width).zip(memory.last.chunks_exact_mut(width)).zip(weights.chunks_exact(w)) {
        for ((c, l), k) in row.chunks_exact_mut(3).zip(last.chunks_exact_mut(3)).zip(k) {
            for i in 0..3 {
                let out = c[i] as f32 / 255.0 * (1.0 - k) + l[i] * k;
                l[i] = out;
                c[i] = to_u8(out);
            }
        }
    }
    memory.time_ns = time_ns;
}

/// How much a colour looks like skin, 0..1: the Cb and Cr that skin of every tone sits inside, with soft edges.
pub fn skin_tone(r: f32, g: f32, b: f32) -> f32 {
    let cb = 128.0 + 255.0 * (-0.168736 * r - 0.331264 * g + 0.5 * b);
    let cr = 128.0 + 255.0 * (0.5 * r - 0.418688 * g - 0.081312 * b);
    smoothstep(69.0, 77.0, cb) * (1.0 - smoothstep(127.0, 135.0, cb)) * smoothstep(125.0, 133.0, cr) * (1.0 - smoothstep(173.0, 181.0, cr))
}

pub fn skin_step(h: usize) -> isize {
    ((SKIN_TAP_1080 * scale_of(h) + 0.5).floor() as isize).max(1)
}

/// The edge-aware blur skin smoothing blends towards, at a pixel of `src` (RGB, w x h) with taps `step` pixels apart.
fn skin_blur(src: &[f32], w: usize, h: usize, step: isize, x: isize, y: isize, spatial: &[f32], range: &[f32]) -> [f32; 3] {
    let at = |x: isize, y: isize| {
        let i = ((y.clamp(0, h as isize - 1) as usize) * w + x.clamp(0, w as isize - 1) as usize) * 3;
        [src[i], src[i + 1], src[i + 2]]
    };
    let c = at(x, y);
    let reach = 3 * step;
    let inside = x >= reach && y >= reach && x + reach < w as isize && y + reach < h as isize;
    let (mut sum, mut total) = ([0f32; 3], 0.0);
    let mut n = 0;
    for j in -3isize..=3 {
        for i in -3isize..=3 {
            let q = if inside {
                let k = (((y + j * step) as usize) * w + (x + i * step) as usize) * 3;
                [src[k], src[k + 1], src[k + 2]]
            } else {
                at(x + i * step, y + j * step)
            };
            let d = (q[0] - c[0]).powi(2) + (q[1] - c[1]).powi(2) + (q[2] - c[2]).powi(2);
            let k = spatial[n] * range.get((d * 4096.0) as usize).copied().unwrap_or(0.0);
            n += 1;
            for t in 0..3 {
                sum[t] += k * q[t];
            }
            total += k;
        }
    }
    [sum[0] / total, sum[1] / total, sum[2] / total]
}

pub fn skin(img: &mut Rgb, mask: &Gray, p: &Skin) {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 || p.strength <= 0.0 || (mask.w, mask.h) != (w, h) {
        return;
    }
    let step = skin_step(h);
    let src: Vec<f32> = img.data.iter().map(|v| *v as f32 / 255.0).collect();
    let spatial: Vec<f32> = (-3i32..=3).flat_map(|j| (-3i32..=3).map(move |i| (-((i * i + j * j) as f32) / (2.0 * 2.0 * 2.0)).exp())).collect();
    // exp(-d2 / 2 range^2) by the squared colour difference (0..0.25 in 1024 steps; past that it's nothing)
    let range: Vec<f32> = (0..1024).map(|i| (-(((i as f32 + 0.5) / 4096.0) / (2.0 * SKIN_RANGE * SKIN_RANGE))).exp()).collect();
    // Taps further apart than a pixel are taps on a picture shrunk by that much: the blur is worked out there, where there are far fewer
    // pixels, and scaled back up
    let f = step as usize;
    let small = (f >= 2).then(|| {
        let (sw, sh) = (w.div_ceil(f), h.div_ceil(f));
        let mut shrunk = vec![0f32; sw * sh * 3];
        let mut wanted = vec![false; sw * sh];
        for sy in 0..sh {
            for sx in 0..sw {
                let mut sum = [0f32; 3];
                let mut n = 0.0;
                for y in sy * f..((sy + 1) * f).min(h) {
                    for x in sx * f..((sx + 1) * f).min(w) {
                        let i = (y * w + x) * 3;
                        sum.iter_mut().zip(&src[i..i + 3]).for_each(|(s, v)| *s += v);
                        n += 1.0;
                        wanted[sy * sw + sx] |= mask.data[y * w + x] >= 5;
                    }
                }
                shrunk[(sy * sw + sx) * 3..][..3].iter_mut().zip(sum).for_each(|(o, s)| *o = s / n);
            }
        }
        // a block next to one that is wanted is wanted too, for the scaling back up
        let near = |sx: usize, sy: usize| (sy.saturating_sub(1)..=(sy + 1).min(sh - 1)).any(|y| (sx.saturating_sub(1)..=(sx + 1).min(sw - 1)).any(|x| wanted[y * sw + x]));
        let mut blurred = vec![0f32; sw * sh * 3];
        in_bands(&mut blurred, sw * 3, |first, band| {
            for (r, row) in band.chunks_exact_mut(sw * 3).enumerate() {
                let sy = first + r;
                for (sx, o) in row.chunks_exact_mut(3).enumerate() {
                    if near(sx, sy) {
                        o.copy_from_slice(&skin_blur(&shrunk, sw, sh, 1, sx as isize, sy as isize, &spatial, &range));
                    }
                }
            }
        });
        (blurred, sw, sh)
    });
    in_bands(&mut img.data, w * 3, |first, band| {
        for (r, row) in band.chunks_exact_mut(w * 3).enumerate() {
            let y = (first + r) as isize;
            for x in 0..w as isize {
                let m = mask.data[y as usize * w + x as usize] as f32 / 255.0;
                if m < 0.02 {
                    continue;
                }
                let i = (y as usize * w + x as usize) * 3;
                let c = [src[i], src[i + 1], src[i + 2]];
                let weight = p.strength * m * skin_tone(c[0], c[1], c[2]);
                if weight < 0.004 {
                    continue;
                }
                let smooth = match &small {
                    Some((blurred, sw, sh)) => {
                        let axis = |i: isize, sn: usize| {
                            let p = ((i as f32 + 0.5) / f as f32 - 0.5).max(0.0);
                            let i0 = (p.floor() as usize).min(sn - 1);
                            (i0, (i0 + 1).min(sn - 1), p - i0 as f32)
                        };
                        let ((x0, x1, fx), (y0, y1, fy)) = (axis(x, *sw), axis(y, *sh));
                        let mut out = [0f32; 3];
                        for t in 0..3 {
                            let top = blurred[(y0 * sw + x0) * 3 + t] * (1.0 - fx) + blurred[(y0 * sw + x1) * 3 + t] * fx;
                            let bottom = blurred[(y1 * sw + x0) * 3 + t] * (1.0 - fx) + blurred[(y1 * sw + x1) * 3 + t] * fx;
                            out[t] = top * (1.0 - fy) + bottom * fy;
                        }
                        out
                    }
                    None => skin_blur(&src, w, h, step, x, y, &spatial, &range),
                };
                // Eyes, brows, lips and hair edges differ from their blur by more than a pore does, and keep what they were
                let keep = smoothstep(0.05, 0.14, (luma_of(c[0], c[1], c[2]) - luma_of(smooth[0], smooth[1], smooth[2])).abs());
                let by = weight * (1.0 - keep);
                let o = &mut row[x as usize * 3..][..3];
                for t in 0..3 {
                    o[t] = to_u8(c[t] + (smooth[t] - c[t]) * by);
                }
            }
        }
    });
}

/// pcg's hash of a whole number, which the shader does the same with.
pub fn pcg(v: u32) -> u32 {
    let s = v.wrapping_mul(747796405).wrapping_add(2891336453);
    let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277803737);
    (w >> 22) ^ w
}

/// A grain's brightness change at a cell on one frame, -1..1 and bunched towards 0 (two uniform numbers added).
pub fn grain_at(cx: u32, cy: u32, number: u32) -> f32 {
    let h = pcg(cx.wrapping_add(pcg(cy.wrapping_add(pcg(number)))));
    let a = h as f32 / 4294967296.0;
    let b = pcg(h ^ 0x9E3779B9) as f32 / 4294967296.0;
    a + b - 1.0
}

/// How dark or light a pixel at (px, py) of a w x h picture makes the corners.
pub fn vignette_at(px: f32, py: f32, w: f32, h: f32, inner: f32, round: f32) -> f32 {
    let aspect = w / h;
    let q = [(px / w * 2.0 - 1.0) * (1.0 + (aspect - 1.0) * round), py / h * 2.0 - 1.0];
    let corner = (2.0 + ((aspect * aspect + 1.0) - 2.0) * round).sqrt();
    let dist = (q[0] * q[0] + q[1] * q[1]).sqrt() / corner;
    let t = ((dist - inner) / (1.0 - inner + 1e-4)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The spacing, in whole pixels, of glow's taps.
pub fn glow_step(radius: f32, h: usize) -> usize {
    ((radius * scale_of(h) / 4.0 + 0.5).floor() as usize).max(1)
}

pub fn finish(img: &mut Rgb, p: &Finish, number: u32) {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 || (p.vignette == 0.0 && p.glow <= 0.0 && p.grain <= 0.0) {
        return;
    }
    // The glow: what's bright, blurred wide, at a fraction of the size
    let f = glow_step(p.glow_radius, h);
    let glow = (p.glow > 0.0).then(|| {
        let (sw, sh) = (w.div_ceil(f), h.div_ceil(f));
        let mut small = vec![0f32; sw * sh * 3];
        in_bands(&mut small, sw * 3, |first, band| {
            for (r, row) in band.chunks_exact_mut(sw * 3).enumerate() {
                let sy = first + r;
                for (sx, o) in row.chunks_exact_mut(3).enumerate() {
                    let mut sum = [0f32; 3];
                    let mut n = 0.0;
                    for y in sy * f..((sy + 1) * f).min(h) {
                        for x in sx * f..((sx + 1) * f).min(w) {
                            let c = &img.data[(y * w + x) * 3..][..3];
                            let c = [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0];
                            let bright = smoothstep(GLOW_FROM, 1.0, luma_of(c[0], c[1], c[2]));
                            for t in 0..3 {
                                sum[t] += c[t] * bright;
                            }
                            n += 1.0;
                        }
                    }
                    for t in 0..3 {
                        o[t] = sum[t] / n;
                    }
                }
            }
        });
        let blurred = blur_quick(&small, sw, sh, 3, 2.0);
        (blurred, sw, sh)
    });
    let cell = (p.grain_size * scale_of(h) + 0.5).floor().max(1.0);
    let (wf, hf) = (w as f32, h as f32);
    let fs = f as f32;
    let at = |small: &[f32], sw: usize, sh: usize, x: usize, y: usize| -> [f32; 3] {
        let axis = |i: usize, sn: usize| {
            let p = ((i as f32 + 0.5) / fs - 0.5).max(0.0);
            let i0 = (p.floor() as usize).min(sn - 1);
            (i0, (i0 + 1).min(sn - 1), p - i0 as f32)
        };
        let ((x0, x1, fx), (y0, y1, fy)) = (axis(x, sw), axis(y, sh));
        let mut out = [0f32; 3];
        for t in 0..3 {
            let top = small[(y0 * sw + x0) * 3 + t] * (1.0 - fx) + small[(y0 * sw + x1) * 3 + t] * fx;
            let bottom = small[(y1 * sw + x0) * 3 + t] * (1.0 - fx) + small[(y1 * sw + x1) * 3 + t] * fx;
            out[t] = top * (1.0 - fy) + bottom * fy;
        }
        out
    };
    in_bands(&mut img.data, w * 3, |first, band| {
        for (r, row) in band.chunks_exact_mut(w * 3).enumerate() {
            let y = first + r;
            for x in 0..w {
                let o = &mut row[x * 3..][..3];
                let mut c = [o[0] as f32 / 255.0, o[1] as f32 / 255.0, o[2] as f32 / 255.0];
                if let Some((small, sw, sh)) = &glow {
                    let g = at(small, *sw, *sh, x, y);
                    for t in 0..3 {
                        c[t] = 1.0 - (1.0 - c[t]) * (1.0 - (g[t] * p.glow).min(1.0));
                    }
                }
                if p.vignette != 0.0 {
                    let v = vignette_at(x as f32 + 0.5, y as f32 + 0.5, wf, hf, p.vignette_inner, p.vignette_round);
                    for t in 0..3 {
                        c[t] = if p.vignette > 0.0 { c[t] * (1.0 - p.vignette * v) } else { c[t] + (1.0 - c[t]) * (-p.vignette * v * 0.6) };
                    }
                }
                if p.grain > 0.0 {
                    let (cx, cy) = (((x as f32 + 0.5) / cell) as u32, ((y as f32 + 0.5) / cell) as u32);
                    let tones = 0.25 + 0.75 * 4.0 * luma_of(c[0], c[1], c[2]).clamp(0.0, 1.0) * (1.0 - luma_of(c[0], c[1], c[2]).clamp(0.0, 1.0));
                    let n = grain_at(cx, cy, number) * p.grain * 0.12 * tones;
                    for t in 0..3 {
                        c[t] += n;
                    }
                }
                for t in 0..3 {
                    o[t] = to_u8(c[t]);
                }
            }
        }
    });
}
