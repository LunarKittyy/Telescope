//! Histogram, waveform, RGB parade and vectorscope maths for the Cinema tools' scopes (scopes.py), with the counts the numbers
//! come to and the BGRA pictures made of them. Every number follows numpy and OpenCV, down to which side a half rounds to and
//! the order floats are added in, so the Python app's scope and this one agree byte for byte.
//!
//! Pictures come in as R, G, B ([`Rgb`]) and go out as B, G, R, A like the Python arrays, since the counts and colours below are
//! written in that order there. Cinema tools turns the picture into RGBA when it draws it.

use crate::cpu::Rgb;
use crate::cv_draw::Canvas;
use crate::imgops::{resize_area, Gray};

/// Columns a frame is shrunk to before any scope.
pub const SCOPE_WIDTH: usize = 256;
/// BT.709, for B, G, R.
pub const LUMA_WEIGHTS: [f32; 3] = [0.0722, 0.7152, 0.2126];
/// Alpha of a scope's dark background.
const BACKDROP: u8 = 150;
/// B, G, R, A of the scale lines.
const GRID: [u8; 4] = [90, 90, 90, 200];
/// A scope's trace colours, B, G, R.
const TRACE_LUMA: [u8; 3] = [235, 235, 235];
const TRACE_R: [u8; 3] = [90, 90, 255];
const TRACE_G: [u8; 3] = [110, 230, 110];
const TRACE_B: [u8; 3] = [255, 150, 90];
/// 75% colour bars, where a vectorscope marks them (B, G, R).
const BARS: [[u8; 3]; 6] = [[0, 0, 191], [191, 0, 191], [191, 0, 0], [191, 191, 0], [0, 191, 0], [0, 191, 191]];
/// Degrees, the skin tone line on a vectorscope (Cb right, Cr up).
pub const SKIN_ANGLE: f64 = 123.0;

/// A picture as 8-bit B, G, R, A, row after row.
#[derive(Clone, Debug, PartialEq)]
pub struct Bgra {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Bgra {
    pub fn new(w: usize, h: usize) -> Bgra {
        Bgra { w, h, data: vec![0; w * h * 4] }
    }

    pub fn at(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.w + x) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    fn set(&mut self, x: usize, y: usize, v: [u8; 4]) {
        let i = (y * self.w + x) * 4;
        self.data[i..i + 4].copy_from_slice(&v);
    }
}

/// The width a frame of w x h is shrunk to for a scope: None when it is no wider than `width` already.
pub fn shrunk_size(w: usize, h: usize, width: usize) -> Option<(usize, usize)> {
    if w <= width {
        return None;
    }
    Some((width, (((h * width) as f64 / w as f64).round_ties_even() as usize).max(1)))
}

/// frame scaled to `width` columns (keeping its shape), averaging pixels; never wider than it was.
pub fn shrink(frame: &Rgb, width: usize) -> Rgb {
    match shrunk_size(frame.w, frame.h, width) {
        None => frame.clone(),
        Some((w, h)) => Rgb { w, h, data: resize_area(&frame.data, frame.w, frame.h, 3, w, h) },
    }
}

/// One pixel's brightness, 0..255 (BT.709 weights): cv2.transform's float arithmetic, which adds green first and fuses each
/// multiply into the add.
pub fn luma_of(r: u8, g: u8, b: u8) -> u8 {
    let acc = LUMA_WEIGHTS[1] * g as f32;
    let acc = LUMA_WEIGHTS[0].mul_add(b as f32, acc);
    let acc = LUMA_WEIGHTS[2].mul_add(r as f32, acc);
    acc.round_ties_even().clamp(0.0, 255.0) as u8
}

/// Brightness of each pixel of a frame, 0..255 (BT.709 weights).
pub fn luma(frame: &Rgb) -> Gray {
    Gray { w: frame.w, h: frame.h, data: frame.data.as_chunks::<3>().0.iter().map(|p| luma_of(p[0], p[1], p[2])).collect() }
}

/// [blue, green, red, luma] pixel counts at each level.
pub fn histogram(frame: &Rgb) -> [[u32; 256]; 4] {
    let mut counts = [[0u32; 256]; 4];
    for p in frame.data.as_chunks::<3>().0 {
        counts[0][p[2] as usize] += 1;
        counts[1][p[1] as usize] += 1;
        counts[2][p[0] as usize] += 1;
        counts[3][luma_of(p[0], p[1], p[2]) as usize] += 1;
    }
    counts
}

/// (256 x w) counts for one channel (h rows of w): column x holds how many of x's pixels sit at each level, with level 255 in row
/// 0, so it reads like a waveform monitor (bright at the top).
pub fn waveform(channel: &[u8], w: usize, h: usize) -> Vec<u32> {
    let mut counts = vec![0u32; 256 * w];
    for y in 0..h {
        for x in 0..w {
            counts[(255 - channel[y * w + x] as usize) * w + x] += 1;
        }
    }
    counts
}

fn channel(frame: &Rgb, c: usize) -> Vec<u8> {
    frame.data.as_chunks::<3>().0.iter().map(|p| p[c]).collect()
}

/// (256 x 3w) counts: the red, green and blue waveforms side by side.
pub fn parade(frame: &Rgb) -> Vec<u32> {
    let w = frame.w;
    let planes: Vec<Vec<u32>> = (0..3).map(|c| waveform(&channel(frame, c), w, frame.h)).collect();
    let mut out = Vec::with_capacity(256 * 3 * w);
    for row in 0..256 {
        for plane in &planes {
            out.extend_from_slice(&plane[row * w..(row + 1) * w]);
        }
    }
    out
}

/// cv2.cvtColor(BGR2YCrCb) of one pixel: (Y, Cr, Cb).
pub fn ycrcb_of(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    let y = (r * 4899 + g * 9617 + b * 1868 + (1 << 13)) >> 14;
    let delta = (128 << 14) + (1 << 13);
    let cr = ((r - y) * 11682 + delta) >> 14;
    let cb = ((b - y) * 9241 + delta) >> 14;
    (y.clamp(0, 255) as u8, cr.clamp(0, 255) as u8, cb.clamp(0, 255) as u8)
}

/// cv2.cvtColor(YCrCb2BGR) of one pixel: (B, G, R).
pub fn bgr_of_ycrcb(y: u8, cr: u8, cb: u8) -> [u8; 3] {
    let (y, cr, cb) = (y as i32, cr as i32 - 128, cb as i32 - 128);
    let r = y + ((cr * 22987 + (1 << 13)) >> 14);
    let g = y + ((cr * -11698 + cb * -5636 + (1 << 13)) >> 14);
    let b = y + ((cb * 29049 + (1 << 13)) >> 14);
    [b.clamp(0, 255) as u8, g.clamp(0, 255) as u8, r.clamp(0, 255) as u8]
}

/// (Cb, Cr) of each pixel, 128 for no colour.
pub fn chroma(frame: &Rgb) -> (Vec<u8>, Vec<u8>) {
    frame.data.as_chunks::<3>().0.iter().map(|p| {
        let (_, cr, cb) = ycrcb_of(p[0], p[1], p[2]);
        (cb, cr)
    }).unzip()
}

/// Where a chroma pair lands on a vectorscope of this size, in (x, y) cells.
fn cell(cb: u8, cr: u8, size: usize) -> (usize, usize) {
    (cb as usize * size / 256, ((256 - cr as usize) * size / 256).min(size - 1))
}

/// (size x size) counts of each pixel's colour: Cb to the right, Cr up, no colour in the middle.
pub fn vectorscope(frame: &Rgb, size: usize) -> Vec<u32> {
    let mut counts = vec![0u32; size * size];
    for p in frame.data.as_chunks::<3>().0 {
        let (_, cr, cb) = ycrcb_of(p[0], p[1], p[2]);
        let (x, y) = cell(cb, cr, size);
        counts[y * size + x] += 1;
    }
    counts
}

/// Where a colour (R, G, B) lands on vectorscope(), in (x, y) cells.
pub fn scope_point(rgb: [u8; 3], size: usize) -> (usize, usize) {
    let (_, cr, cb) = ycrcb_of(rgb[0], rgb[1], rgb[2]);
    cell(cb, cr, size)
}

// ── Pictures of them ─────────────────────────────────────────────────────────

/// counts as brightness 0..1, on a log scale so a few pixels still show; per_cell is what an even spread gives.
pub fn glow(counts: &[u32], per_cell: f64) -> Vec<f32> {
    let k = 8.0 / per_cell.max(1e-6);
    let scale = 48f64.ln();
    counts.iter().map(|&c| ((c as f64 * k).ln_1p() / scale).clamp(0.0, 1.0) as f32).collect()
}

fn backdrop(w: usize, h: usize) -> Bgra {
    let mut out = Bgra::new(w, h);
    for p in out.data.as_chunks_mut::<4>().0 {
        p[3] = BACKDROP;
    }
    out
}

/// Lines across a waveform picture at every 25% (0 and 100% a little brighter), over columns x0 up to x1.
fn levels_grid(image: &mut Bgra, x0: usize, x1: usize) {
    let h = image.h;
    for pct in [100, 75, 50, 25, 0] {
        let row = (h - 1).min((((100 - pct) as f64 / 100.0) * (h - 1) as f64).round_ties_even() as usize);
        let alpha = if pct == 0 || pct == 100 { 255 } else { GRID[3] };
        for x in x0..x1 {
            image.set(x, row, [GRID[0], GRID[1], GRID[2], alpha]);
        }
    }
}

/// strength (0..1, h x w) of a colour over the backdrop, in place, from column x0.
fn paint_trace(image: &mut Bgra, strength: &[f32], w: usize, colour: [u8; 3], x0: usize) {
    let h = strength.len() / w;
    for y in 0..h {
        for x in 0..w {
            let s = strength[y * w + x];
            let i = (y * image.w + x0 + x) * 4;
            for (c, &col) in colour.iter().enumerate() {
                let trace = (s * col as f32) as u8;
                image.data[i + c] = image.data[i + c].max(trace);
            }
            let alpha = (BACKDROP as f32 + s * 105.0) as u8;
            image.data[i + 3] = image.data[i + 3].max(alpha);
        }
    }
}

/// A BGRA luma waveform of a shrunk frame, (256, w).
pub fn waveform_image(frame: &Rgb) -> Bgra {
    let counts = waveform(&luma(frame).data, frame.w, frame.h);
    let mut image = backdrop(frame.w, 256);
    levels_grid(&mut image, 0, frame.w);
    paint_trace(&mut image, &glow(&counts, frame.h as f64 / 256.0), frame.w, TRACE_LUMA, 0);
    image
}

/// A BGRA RGB parade of a shrunk frame, (256, 3w + 2 gaps).
pub fn parade_image(frame: &Rgb) -> Bgra {
    let (w, h) = (frame.w, frame.h);
    let gap = 4;
    let mut image = backdrop(3 * w + 2 * gap, 256);
    for (i, (c, colour)) in [(0, TRACE_R), (1, TRACE_G), (2, TRACE_B)].into_iter().enumerate() {
        let x0 = i * (w + gap);
        levels_grid(&mut image, x0, x0 + w);
        let counts = waveform(&channel(frame, c), w, h);
        paint_trace(&mut image, &glow(&counts, h as f64 / 256.0), w, colour, x0);
    }
    image
}

/// A BGRA histogram: red, green and blue filled in on top of each other, luma as a white outline.
pub fn histogram_image(frame: &Rgb, height: usize) -> Bgra {
    let counts = histogram(frame);
    // A pile at the very ends (clipping) can run off the top
    let peak = counts.iter().flat_map(|c| c[2..254].iter()).copied().max().unwrap_or(0) as f64;
    let top = (peak.max(1.0) * 1.1) as f32;
    let heights: Vec<Vec<f32>> = counts.iter().map(|c| c.iter().map(|&n| (n as f32 / top).clamp(0.0, 1.0) * height as f32).collect()).collect();
    let mut image = backdrop(256, height);
    for (c, colour) in [(2, TRACE_R), (1, TRACE_G), (0, TRACE_B)] {
        for row in 0..height {
            let level = (height - row) as f32;
            for (x, &bar) in heights[c].iter().enumerate() {
                let filled = (if level <= bar { 1.0f32 } else { 0.0 }) * 0.6;
                let i = (row * 256 + x) * 4;
                for (k, &col) in colour.iter().enumerate() {
                    let v = image.data[i + k] as f32 + filled * col as f32;
                    image.data[i + k] = v.clamp(0.0, 255.0) as u8;
                }
                let alpha = if filled > 0.0 { 220 } else { 0 };
                image.data[i + 3] = image.data[i + 3].max(alpha);
            }
        }
    }
    for row in 0..height {
        let level = (height - row) as f32;
        for (x, &bar) in heights[3].iter().enumerate() {
            if (level - bar).abs() < 1.0 {
                image.set(x, row, [TRACE_LUMA[0], TRACE_LUMA[1], TRACE_LUMA[2], 255]);
            }
        }
    }
    for quarter in [64, 128, 192] {
        for row in 0..height {
            let i = (row * 256 + quarter) * 4;
            for (k, &grid) in GRID.iter().take(3).enumerate() {
                image.data[i + k] = image.data[i + k].max(grid);
            }
        }
    }
    image
}

/// The circle, the colour bar targets and the skin tone line of a vectorscope this size, B, G, R, A and 0 alpha where nothing is drawn.
pub fn vectorscope_grid(size: usize) -> Bgra {
    let mut grid = Bgra::new(size, size);
    let mid = (size / 2) as i32;
    let colour = [GRID[0], GRID[1], GRID[2], 255];
    let reach = size as f64 * 0.45;
    let skin = SKIN_ANGLE.to_radians();
    let end = ((mid as f64 + skin.cos() * reach).round_ties_even() as i32, (mid as f64 - skin.sin() * reach).round_ties_even() as i32);
    let boxed = (size / 40).max(3) as i32;
    let mut canvas = Canvas { data: &mut grid.data, w: size, h: size, channels: 4 };
    canvas.circle((mid, mid), reach.round_ties_even() as i32, colour, 1);
    canvas.straight_line((mid - 4, mid), (mid + 4, mid), colour);
    canvas.straight_line((mid, mid - 4), (mid, mid + 4), colour);
    canvas.line((mid, mid), end, [GRID[0], GRID[1], GRID[2], 160], 1);
    for bar in BARS {
        let (x, y) = scope_point([bar[2], bar[1], bar[0]], size);
        let (x, y) = (x as i32, y as i32);
        canvas.rectangle((x - boxed, y - boxed), (x + boxed, y + boxed), [bar[0], bar[1], bar[2], 255]);
    }
    grid
}

/// A BGRA vectorscope, (size, size): each spot drawn in the colour it stands for.
pub fn vectorscope_image(frame: &Rgb, size: usize) -> Bgra {
    let counts = vectorscope(frame, size);
    let strength = glow(&counts, (frame.h * frame.w) as f64 / (size * size) as f64 * 6.0);
    let cells: Vec<u8> = (0..size).map(|i| (i * 256 / size + 128 / size) as u8).collect();
    let mut image = backdrop(size, size);
    let grid = vectorscope_grid(size);
    for row in 0..size {
        let cr = cells[size - 1 - row];
        for col in 0..size {
            let cb = cells[col];
            let bgr = bgr_of_ycrcb(170, cr, cb);
            let s = strength[row * size + col];
            let i = (row * size + col) * 4;
            for (k, &value) in bgr.iter().enumerate() {
                // paler, so a dim spot still shows on the dark backdrop
                let tint = value as f32 * 0.6 + 100.0;
                image.data[i + k] = (tint * s).clamp(0.0, 255.0) as u8;
            }
            let alpha = (BACKDROP as f32 + s * 105.0) as u8;
            image.data[i + 3] = image.data[i + 3].max(alpha);
            let g = grid.at(col, row);
            if g[3] > 0 {
                for (k, &value) in g.iter().enumerate() {
                    image.data[i + k] = image.data[i + k].max(value);
                }
            }
        }
    }
    image
}

/// The scopes the card can show, by name.
pub const SCOPE_NAMES: [&str; 4] = ["histogram", "waveform", "parade", "vectorscope"];

/// The named scope (one of SCOPE_NAMES) of a frame of any size, as a BGRA picture; None for another name.
pub fn scope_image(name: &str, frame: &Rgb) -> Option<Bgra> {
    let small = shrink(frame, SCOPE_WIDTH);
    Some(match name {
        "histogram" => histogram_image(&small, 128),
        "waveform" => waveform_image(&small),
        "parade" => parade_image(&small),
        "vectorscope" => vectorscope_image(&small, 256),
        _ => return None,
    })
}

#[cfg(test)]
#[path = "scopes_tests.rs"]
mod tests;
