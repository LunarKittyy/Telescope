//! The pixel work of the window's view (view.apply_zoom, transform_frame and apply_stretch), as frame steps for the View stage.
//!
//! Python crops, flips, turns and then stretches the picture. All of it runs on the CPU, so a card that OBS is encoding on can't hold
//! up a frame for a zoom: a quarter turn is an exact copy of the planes, a flip is the planes copied in another order, and a zoom
//! or a stretch is one bilinear resampling of the picture as RGB. The zoom is a fractional crop that repeats the edge, the stretch
//! leaves black where the picture doesn't reach, both like cv2.warpAffine's linear interpolation does. Lens correction comes first, on the
//! picture as the lens made it, and is one table-driven bilinear read, also on the CPU.

use crate::frame::{CpuStep, Frame, FrameContext, FrameStep, PixelFormat};
use crate::host::View;
use crate::image::{frame_rgb, keep_bytes, over_rows, resample_linear, rgb_to_420, rint, spare_bytes, with_spares, Tap};
use crate::view::{window, Lens};
use std::sync::{Arc, Mutex};

/// Quarter turns clockwise in a rotation of degrees (0 for none).
fn quarters(rotation: Option<i32>) -> u32 {
    match rotation {
        Some(90) => 1,
        Some(180) => 2,
        Some(270) => 3,
        _ => 0,
    }
}

/// A quarter turn of the whole picture, planes copied across (no resampling).
struct Turn {
    quarters: u32,
}

/// Pixels along each side of the blocks a turn works through: a block's source rows and destination columns both stay in the cache.
const BLOCK: usize = 64;

/// `plane` (w x h, `bpp` bytes a pixel) turned clockwise by 1 or 3 quarters (h x w) into `out`. Blocks at a time, so neither the rows
/// read nor the columns written walk the whole picture for every pixel.
fn turn_plane(plane: &[u8], w: usize, h: usize, bpp: usize, quarters: u32, out: &mut [u8]) {
    match bpp {
        1 => turn_blocks::<1>(plane, w, h, quarters, out),
        2 => turn_blocks::<2>(plane, w, h, quarters, out),
        4 => turn_blocks::<4>(plane, w, h, quarters, out),
        _ => unreachable!("planes have 1, 2 or 4 bytes a pixel"),
    }
}

fn turn_blocks<const B: usize>(plane: &[u8], w: usize, h: usize, quarters: u32, out: &mut [u8]) {
    for by in (0..h).step_by(BLOCK) {
        for bx in (0..w).step_by(BLOCK) {
            for y in by..(by + BLOCK).min(h) {
                let row = &plane[y * w * B..(y + 1) * w * B];
                for x in bx..(bx + BLOCK).min(w) {
                    let to = if quarters == 1 { x * h + (h - 1 - y) } else { (w - 1 - x) * h + y } * B;
                    out[to..to + B].copy_from_slice(&row[x * B..(x + 1) * B]);
                }
            }
        }
    }
}

/// The frame turned by a quarter or three (clockwise); the same frame for other counts or formats it can't turn.
pub fn turn_frame(frame: &Frame, quarters: u32) -> Frame {
    if quarters % 4 != 1 && quarters % 4 != 3 {
        return frame.clone();
    }
    let q = quarters % 4;
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut data = vec![0u8; frame.data.len()];
    match frame.format {
        PixelFormat::Rgba | PixelFormat::Bgra => turn_plane(&frame.data, w, h, 4, q, &mut data),
        PixelFormat::I420 => {
            let (y, u, v) = frame.i420_planes().expect("an I420 frame has planes");
            let (oy, rest) = data.split_at_mut(w * h);
            let (ou, ov) = rest.split_at_mut(cw * ch);
            turn_plane(y, w, h, 1, q, oy);
            turn_plane(u, cw, ch, 1, q, ou);
            turn_plane(v, cw, ch, 1, q, ov);
        }
        PixelFormat::Nv12 => {
            let (y, uv) = frame.data.split_at(w * h);
            let (oy, ouv) = data.split_at_mut(w * h);
            turn_plane(y, w, h, 1, q, oy);
            turn_plane(uv, cw, ch, 2, q, ouv);
        }
    }
    let mut out = Frame::new(frame.height, frame.width, frame.format, data);
    out.number = frame.number;
    out.time_ns = frame.time_ns;
    out
}

impl CpuStep for Turn {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        *frame = turn_frame(frame, self.quarters);
    }

    fn geometry_key(&self) -> u64 {
        key(&("turn", self.quarters))
    }
}

/// A mirror, a flip or a half turn: the planes copied across in another order, so the picture never has to visit the graphics card.
struct Flip {
    horizontal: bool,
    vertical: bool,
}

/// `plane` (w x h, `bpp` bytes a pixel) flipped left to right and/or top to bottom, in place.
fn flip_plane(plane: &mut [u8], w: usize, h: usize, bpp: usize, horizontal: bool, vertical: bool) {
    let row = w * bpp;
    if horizontal && row > 0 {
        for line in plane.chunks_exact_mut(row) {
            if bpp == 1 {
                line.reverse();
            } else {
                for x in 0..w / 2 {
                    for b in 0..bpp {
                        line.swap(x * bpp + b, (w - 1 - x) * bpp + b);
                    }
                }
            }
        }
    }
    if vertical {
        for y in 0..h / 2 {
            let (top, bottom) = plane.split_at_mut((h - 1 - y) * row);
            top[y * row..(y + 1) * row].swap_with_slice(&mut bottom[..row]);
        }
    }
}

/// The frame flipped in place.
pub fn flip_frame(frame: &mut Frame, horizontal: bool, vertical: bool) {
    if !horizontal && !vertical {
        return;
    }
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let format = frame.format;
    let data = frame.data_mut();
    match format {
        PixelFormat::Rgba | PixelFormat::Bgra => flip_plane(data, w, h, 4, horizontal, vertical),
        PixelFormat::I420 => {
            let (y, rest) = data.split_at_mut(w * h);
            let (u, v) = rest.split_at_mut(cw * ch);
            flip_plane(y, w, h, 1, horizontal, vertical);
            flip_plane(u, cw, ch, 1, horizontal, vertical);
            flip_plane(v, cw, ch, 1, horizontal, vertical);
        }
        PixelFormat::Nv12 => {
            let (y, uv) = data.split_at_mut(w * h);
            flip_plane(y, w, h, 1, horizontal, vertical);
            flip_plane(uv, cw, ch, 2, horizontal, vertical);
        }
    }
}

impl CpuStep for Flip {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        flip_frame(frame, self.horizontal, self.vertical);
    }

    fn geometry_key(&self) -> u64 {
        key(&("flip", self.horizontal, self.vertical))
    }
}

/// A number for what a step does to the picture's geometry (CpuStep::geometry_key).
fn key(parts: &impl std::fmt::Debug) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{parts:?}").hash(&mut hasher);
    hasher.finish()
}

/// One resampling of the picture as given (turned by `done` quarters already): the crop, flips and half turn of the view, or its
/// stretch. Python does these in two resamplings, the stretch of the 8-bit result of the crop, and so does this, so thin lines blur
/// the same amount.
#[derive(Clone, Copy, Debug)]
struct Pass {
    x0: f64,
    y0: f64,
    crop_w: f64,
    crop_h: f64,
    flip_h: bool,
    flip_v: bool,
    stretch_x: f64,
    stretch_y: f64,
    turn: u32,
    done: u32,
}

fn unturn(p: (f64, f64), quarters: u32) -> (f64, f64) {
    match quarters {
        1 => (p.1, 1.0 - p.0),
        2 => (1.0 - p.0, 1.0 - p.1),
        3 => (1.0 - p.1, p.0),
        _ => p,
    }
}

fn turned(p: (f64, f64), quarters: u32) -> (f64, f64) {
    match quarters {
        1 => (1.0 - p.1, p.0),
        2 => (1.0 - p.0, 1.0 - p.1),
        3 => (p.1, 1.0 - p.0),
        _ => p,
    }
}

impl Pass {
    /// `zoom` the crop, flips and half turn of `view`, `stretch` its stretch.
    fn new(view: &View, done: u32, zoom: bool, stretch: bool) -> Pass {
        let (z, pan_x, pan_y) = if zoom { (view.zoom as f64, (view.pan_x as f64).clamp(-1.0, 1.0), (view.pan_y as f64).clamp(-1.0, 1.0)) } else { (1.0, 0.0, 0.0) };
        let ((x0, crop_w), (y0, crop_h)) = (window(z, pan_x), window(z, pan_y));
        let (stretch_x, stretch_y) = if stretch { (view.stretch_x as f64, view.stretch_y as f64) } else { (1.0, 1.0) };
        Pass { x0, y0, crop_w, crop_h, flip_h: zoom && view.flip_h, flip_v: zoom && view.flip_v, stretch_x, stretch_y,
               turn: if zoom { quarters(view.rotation) } else { 0 }, done: if zoom { done } else { 0 } }
    }

    /// The point of the stretched picture (0..1) an output point (0..1) shows.
    fn stretched(&self, u: f64, v: f64) -> (f64, f64) {
        (0.5 + (u - 0.5) / self.stretch_x, 0.5 + (v - 0.5) / self.stretch_y)
    }

    /// Where in the source (0..1) the output point (u, v) is taken from. Each axis of the output reads one axis of the source,
    /// whatever the turn, so the two can be worked out apart.
    fn source(&self, u: f64, v: f64) -> (f64, f64) {
        let mut p = unturn(self.stretched(u, v), self.turn);
        if self.flip_h {
            p.0 = 1.0 - p.0;
        }
        if self.flip_v {
            p.1 = 1.0 - p.1;
        }
        turned((self.x0 + p.0 * self.crop_w, self.y0 + p.1 * self.crop_h), self.done)
    }

    fn is_stretch(&self) -> bool {
        (self.stretch_x - 1.0).abs() >= 1e-6 || (self.stretch_y - 1.0).abs() >= 1e-6
    }
}

/// The two taps of a bilinear sample at `at` (in pixels, the first pixel's centre at 0) from n pixels, the edge repeated.
fn tap(at: f32, n: usize) -> Tap {
    let last = n as i64 - 1;
    let s = at.floor();
    let q = rint(((at - s) * 2048.0) as f64);
    let s = s as i64;
    (s.clamp(0, last) as usize, (s + 1).clamp(0, last) as usize, 2048 - q, q)
}

/// How much of a pixel a stretched picture covers along an axis: 1 inside, fading to 0 over half a pixel past its edge.
fn covered(s: f64, n: usize) -> f32 {
    let at = s * n as f64;
    ((at + 0.5).clamp(0.0, 1.0) * (n as f64 - at + 0.5).clamp(0.0, 1.0)) as f32
}

/// Zoom and stretch: the frame resampled as RGB, then back in its own format.
struct Resample {
    passes: Vec<Pass>,
    // The sampling of each pass at the picture size it saw last
    plans: Option<((usize, usize), Vec<Plan>)>,
    // The RGB pictures of the frame before, for this one to draw on
    spares: Vec<Vec<u8>>,
}

/// What one pass does at a picture size: the taps of each column and row, and for a stretch how much of each the picture covers.
struct Plan {
    xs: Vec<Tap>,
    ys: Vec<Tap>,
    covered: Option<(Vec<f32>, Vec<f32>)>,
}

impl Plan {
    fn new(w: usize, h: usize, pass: &Pass) -> Plan {
        let xs = (0..w).map(|x| tap((pass.source((x as f64 + 0.5) / w as f64, 0.5).0 * w as f64 - 0.5) as f32, w)).collect();
        let ys = (0..h).map(|y| tap((pass.source(0.5, (y as f64 + 0.5) / h as f64).1 * h as f64 - 0.5) as f32, h)).collect();
        let covered = pass.is_stretch().then(|| {
            let fx = (0..w).map(|x| covered(pass.stretched((x as f64 + 0.5) / w as f64, 0.5).0, w)).collect();
            let fy = (0..h).map(|y| covered(pass.stretched(0.5, (y as f64 + 0.5) / h as f64).1, h)).collect();
            (fx, fy)
        });
        Plan { xs, ys, covered }
    }
}

/// `pixels` (RGB, w x h) through one pass, the same size.
fn resample(pixels: Vec<u8>, w: usize, h: usize, plan: &Plan) -> Vec<u8> {
    let mut out = resample_linear(&pixels, w, h, 3, &plan.xs, &plan.ys);
    keep_bytes(pixels);
    if let Some((fx, fy)) = &plan.covered {
        black_outside(&mut out, w, fx, fy);
    }
    out
}

/// What a stretch leaves bare: black, fading in over the pixel at the picture's edge (fx and fy: how much of each column and row is covered).
fn black_outside(rgb: &mut [u8], w: usize, fx: &[f32], fy: &[f32]) {
    // The columns at either end that aren't wholly inside; the middle only ever changes with its row
    let left = fx.iter().take_while(|&&f| f < 1.0).count();
    let right = w - fx.iter().rev().take_while(|&&f| f < 1.0).count();
    let (left, right) = (left.min(w), right.max(left));
    // Columns with none of the picture are black whatever their row, which a fill does faster than the blend that gives 0
    let bare_left = fx[..left].iter().take_while(|&&f| f == 0.0).count();
    let bare_right = right + fx[right..].iter().take_while(|&&f| f != 0.0).count();
    let scale = |px: &mut [u8], f: f32| px.iter_mut().for_each(|v| *v = (*v as f32 * f + 0.5) as u8);
    for (line, &f) in rgb.chunks_exact_mut(w * 3).zip(fy) {
        if f == 0.0 {
            line.fill(0);
            continue;
        }
        line[..bare_left * 3].fill(0);
        line[bare_right.min(w) * 3..].fill(0);
        for x in (bare_left..left).chain(right..bare_right.min(w)) {
            scale(&mut line[x * 3..][..3], fx[x] * f);
        }
        if f < 1.0 {
            scale(&mut line[left * 3..right * 3], f);
        }
    }
}

impl Resample {
    fn new(passes: Vec<Pass>) -> Resample {
        Resample { passes, plans: None, spares: Vec::new() }
    }

    fn run(&mut self, frame: &mut Frame) {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let Some(mut rgb) = frame_rgb(frame) else { return };
        if !matches!(&self.plans, Some((size, _)) if *size == (w, h)) {
            self.plans = Some(((w, h), self.passes.iter().map(|pass| Plan::new(w, h, pass)).collect()));
        }
        for plan in self.plans.iter().flat_map(|(_, plans)| plans) {
            rgb = resample(rgb, w, h, plan);
        }
        store_rgb(frame, rgb);
    }
}

/// `rgb` (the frame's size) written back into the frame in its own format.
fn store_rgb(frame: &mut Frame, rgb: Vec<u8>) {
    let (w, h, format) = (frame.width as usize, frame.height as usize, frame.format);
    // Written whole into a spare buffer that then replaces the pixels, so a panic leaves the frame as it was
    let mut data = spare_bytes(format.size(frame.width, frame.height));
    match format {
        PixelFormat::Rgba => data.as_chunks_mut::<4>().0.iter_mut().zip(rgb.as_chunks::<3>().0).for_each(|(o, p)| *o = [p[0], p[1], p[2], 255]),
        PixelFormat::Bgra => data.as_chunks_mut::<4>().0.iter_mut().zip(rgb.as_chunks::<3>().0).for_each(|(o, p)| *o = [p[2], p[1], p[0], 255]),
        PixelFormat::I420 => rgb_to_420(&rgb, w, h, &mut data, false),
        PixelFormat::Nv12 => rgb_to_420(&rgb, w, h, &mut data, true),
    }
    frame.replace_data(data);
    keep_bytes(rgb);
}

/// One output pixel of the lens correction: the top left of the 2 x 2 source pixels it blends (as a pixel index) and the weights
/// of the right and lower ones out of 2048.
#[derive(Clone, Copy)]
struct Remap {
    at: u32,
    right: u16,
    below: u16,
}

/// Where each pixel of a w x h picture reads its colour from for a Lens correction of `amount`, as a table.
fn lens_table(amount: f64, w: usize, h: usize) -> Vec<Remap> {
    let lens = Lens::new(amount, w as f64 / h as f64);
    let mut table = vec![Remap { at: 0, right: 0, below: 0 }; w * h];
    let place = |at: f64, n: usize| -> (usize, u16) {
        // The last pixel has nothing to its right: it's the right one of the pair before it, in full
        let at = at.clamp(0.0, n as f64 - 1.0);
        let i = (at.floor() as usize).min(n - 2);
        (i, rint((at - i as f64) * 2048.0).clamp(0, 2048) as u16)
    };
    over_rows(&mut table, w, |y, row| {
        for (x, out) in row.iter_mut().enumerate() {
            // The shared mapping works on 0..1 of the frame, a pixel's centre at (index + 0.5) / size
            let (sx, sy) = lens.forward((x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64);
            let ((ix, fx), (iy, fy)) = (place(sx * w as f64 - 0.5, w), place(sy * h as f64 - 0.5, h));
            *out = Remap { at: (iy * w + ix) as u32, right: fx, below: fy };
        }
    });
    table
}

type Table = Arc<Vec<Remap>>;

/// The last tables made, as (amount, w, h, table): steps are made again on every refresh of the view (a zoom, Auto framing, another
/// plugin's change), and the table is the same for as long as the amount and the size are.
static TABLES: Mutex<Vec<(u64, usize, usize, Table)>> = Mutex::new(Vec::new());

/// How many tables are kept (a second stream of another size).
const TABLES_KEPT: usize = 2;

fn table_for(amount: f64, w: usize, h: usize) -> Table {
    let key = amount.to_bits();
    {
        let mut kept = TABLES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = kept.iter().position(|(a, tw, th, _)| (*a, *tw, *th) == (key, w, h)) {
            let hit = kept.remove(i);
            let table = hit.3.clone();
            kept.insert(0, hit);
            return table;
        }
    }
    // Made without the lock: it takes long, and other streams and the view's steps need the cache meanwhile
    let table = Arc::new(lens_table(amount, w, h));
    let mut kept = TABLES.lock().unwrap_or_else(|e| e.into_inner());
    kept.retain(|(a, tw, th, _)| (*a, *tw, *th) != (key, w, h));
    kept.insert(0, (key, w, h, table.clone()));
    kept.truncate(TABLES_KEPT);
    table
}

/// Lens correction: the picture bent so that the straight lines a wide (or long) lens curves come out straight, cut or pulled in
/// to keep the edges filled. One bilinear read per pixel from a table that is made again only when the amount or the size changes.
struct Undistort {
    amount: f64,
    table: Option<(usize, usize, Table)>,
    spares: Vec<Vec<u8>>,
}

impl Undistort {
    fn run(&mut self, frame: &mut Frame) {
        let (w, h) = (frame.width as usize, frame.height as usize);
        if w < 2 || h < 2 {
            return;
        }
        if self.table.as_ref().is_none_or(|(tw, th, _)| (*tw, *th) != (w, h)) {
            self.table = Some((w, h, table_for(self.amount, w, h)));
        }
        let Some(mut rgb) = frame_rgb(frame) else { return };
        let Some((_, _, table)) = &self.table else { return };
        let mut out = spare_bytes(w * h * 3);
        over_rows(&mut out, w * 3, |y, line| {
            for (px, m) in line.as_chunks_mut::<3>().0.iter_mut().zip(&table[y * w..(y + 1) * w]) {
                let (a, wx, wy) = (m.at as usize * 3, m.right as u32, m.below as u32);
                for (ch, o) in px.iter_mut().enumerate() {
                    let top = rgb[a + ch] as u32 * (2048 - wx) + rgb[a + 3 + ch] as u32 * wx;
                    let bottom = rgb[a + w * 3 + ch] as u32 * (2048 - wx) + rgb[a + w * 3 + 3 + ch] as u32 * wx;
                    *o = ((top as u64 * (2048 - wy) as u64 + bottom as u64 * wy as u64 + (1 << 21)) >> 22) as u8;
                }
            }
        });
        keep_bytes(std::mem::take(&mut rgb));
        store_rgb(frame, out);
    }
}

impl CpuStep for Undistort {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        let mut spares = std::mem::take(&mut self.spares);
        with_spares(&mut spares, || self.run(frame));
        self.spares = spares;
    }

    fn geometry_key(&self) -> u64 {
        key(&("lens", self.amount.to_bits()))
    }
}

impl CpuStep for Resample {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        let mut spares = std::mem::take(&mut self.spares);
        with_spares(&mut spares, || self.run(frame));
        self.spares = spares;
    }

    fn geometry_key(&self) -> u64 {
        key(&("resample", &self.passes))
    }
}

/// Whether the crop, flips or a half turn of the view change the picture.
fn crop_needed(view: &View) -> bool {
    view.zoom > 1.0 || view.flip_h || view.flip_v || quarters(view.rotation) == 2
}

/// Whether Lens correction does anything (an amount of 0 adds no step, so the frame goes through exactly as it came).
fn lens_needed(view: &View) -> bool {
    view.distortion.abs() >= 1e-6
}

fn stretch_needed(view: &View) -> bool {
    (view.stretch_x - 1.0).abs() >= 1e-6 || (view.stretch_y - 1.0).abs() >= 1e-6
}

/// The resamplings of a view whose picture was turned by `done` quarters already: the crop, then the stretch.
fn passes(view: &View, done: u32) -> Vec<Pass> {
    let mut passes = Vec::new();
    if crop_needed(view) {
        passes.push(Pass::new(view, done, true, false));
    }
    if stretch_needed(view) {
        passes.push(Pass::new(view, 0, false, true));
    }
    passes
}

/// The frame steps of a view, in order, all on the CPU (a busy card, OBS encoding on it, would hold up every frame for a shader, and
/// a shader step can't move to the CPU): a quarter turn when the picture changes shape, then either a plain copy for a view that only
/// mirrors, flips or half turns, or one resampling for the crop and another for the stretch. No steps for a view that changes nothing.
pub fn steps(view: &View) -> Vec<FrameStep> {
    let q = quarters(view.rotation);
    let mut out: Vec<FrameStep> = Vec::new();
    // Before any turn, flip or crop: the lens bent the picture as the phone sent it
    if !lens_needed(view) {
        // Nothing keeps a table for a lens that's off
        TABLES.lock().unwrap_or_else(|e| e.into_inner()).clear();
    } else {
        out.push(FrameStep::Cpu(Box::new(Undistort { amount: view.distortion as f64, table: None, spares: Vec::new() })));
    }
    // Turned first, the rest of the work is the same as for any other view, on the turned picture
    let done = if q == 1 || q == 3 { q } else { 0 };
    if done != 0 {
        out.push(FrameStep::Cpu(Box::new(Turn { quarters: done })));
    }
    if crop_needed(view) && done == 0 && view.zoom <= 1.0 && !stretch_needed(view) {
        // A half turn is both flips, and a second flip undoes the first
        let half = q == 2;
        out.push(FrameStep::Cpu(Box::new(Flip { horizontal: view.flip_h != half, vertical: view.flip_v != half })));
        return out;
    }
    let passes = passes(view, done);
    if !passes.is_empty() {
        out.push(FrameStep::Cpu(Box::new(Resample::new(passes))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::LENS_STRENGTH;

    fn rgba(w: u32, h: u32) -> Frame {
        Frame::new(w, h, PixelFormat::Rgba, (0..w * h).flat_map(|i| [i as u8, 0, 0, 255]).collect())
    }

    #[test]
    fn a_view_that_changes_nothing_has_no_steps() {
        assert!(steps(&View::default()).is_empty());
        assert!(steps(&View { zoom: 0.5, ..View::default() }).is_empty(), "at or below 1x the frame goes through untouched");
        assert!(steps(&View { zoom: 1.0, pan_x: 0.7, ..View::default() }).is_empty(), "pan counts only once zoomed in");
    }

    #[test]
    fn a_zoom_or_a_stretch_never_needs_the_graphics_card_either() {
        for view in [View { zoom: 2.0, ..View::default() }, View { stretch_x: 0.8, ..View::default() }, View { flip_h: true, zoom: 2.0, ..View::default() },
                     View { zoom: 1.5, stretch_y: 1.2, ..View::default() }, View { rotation: Some(90), zoom: 2.0, ..View::default() }] {
            let s = steps(&view);
            assert!(!s.is_empty() && s.iter().all(|step| matches!(step, FrameStep::Cpu(_))), "{view:?}");
        }
        assert_eq!(steps(&View { zoom: 1.5, stretch_y: 1.2, ..View::default() }).len(), 1, "a crop and a stretch share one trip through RGB");
    }

    #[test]
    fn a_mirror_a_flip_or_a_half_turn_alone_never_needs_the_graphics_card() {
        for view in [View { flip_h: true, ..View::default() }, View { flip_v: true, ..View::default() }, View { rotation: Some(180), ..View::default() },
                     View { rotation: Some(180), flip_h: true, flip_v: true, ..View::default() }] {
            let s = steps(&view);
            assert!(s.len() == 1 && matches!(s[0], FrameStep::Cpu(_)), "{view:?}");
        }
    }

    struct NoCtx;

    impl FrameContext for NoCtx {
        fn source_id(&self) -> &str {
            ""
        }
        fn person_mask(&self, _frame: &Frame) -> Option<std::sync::Arc<Vec<u8>>> {
            None
        }
        fn colour_preview(&self, frame: &Frame) -> Frame {
            frame.clone()
        }
    }

    fn through(view: &View, mut frame: Frame) -> Frame {
        for step in steps(view) {
            let FrameStep::Cpu(mut cpu) = step else { panic!("a CPU step") };
            cpu.process(&mut frame, &NoCtx);
        }
        frame
    }

    #[test]
    fn the_cpu_flips_move_pixels_like_opencv_flip() {
        // 3 wide, 2 tall: [[0, 1, 2], [3, 4, 5]]
        let f = rgba(3, 2);
        let reds = |f: &Frame| f.data.chunks(4).map(|p| p[0]).collect::<Vec<_>>();
        assert_eq!(reds(&through(&View { flip_h: true, ..View::default() }, f.clone())), [2, 1, 0, 5, 4, 3]);
        assert_eq!(reds(&through(&View { flip_v: true, ..View::default() }, f.clone())), [3, 4, 5, 0, 1, 2]);
        assert_eq!(reds(&through(&View { rotation: Some(180), ..View::default() }, f.clone())), [5, 4, 3, 2, 1, 0]);
        assert_eq!(reds(&through(&View { rotation: Some(180), flip_h: true, ..View::default() }, f.clone())), [3, 4, 5, 0, 1, 2], "a half turn and a mirror is a flip");
    }

    #[test]
    fn a_flipped_yuv_picture_flips_each_plane() {
        let mut data: Vec<u8> = (0..4 * 2).collect();
        data.extend([100, 101]);
        data.extend([200, 201]);
        let f = through(&View { flip_h: true, ..View::default() }, Frame::new(4, 2, PixelFormat::I420, data));
        let (y, u, v) = f.i420_planes().unwrap();
        assert_eq!(y, [3, 2, 1, 0, 7, 6, 5, 4]);
        assert_eq!((u, v), (&[101, 100][..], &[201, 200][..]));
        let nv = through(&View { flip_h: true, flip_v: true, ..View::default() }, Frame::new(4, 2, PixelFormat::Nv12, vec![0, 1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 13]));
        assert_eq!(*nv.data, vec![7, 6, 5, 4, 3, 2, 1, 0, 12, 13, 10, 11], "a pair of chroma bytes stays a pair");
    }

    #[test]
    fn a_quarter_turn_is_a_copy_first_and_a_resampling_for_the_rest() {
        let s = steps(&View { rotation: Some(90), ..View::default() });
        assert!(s.len() == 1 && matches!(s[0], FrameStep::Cpu(_)));
        let s = steps(&View { rotation: Some(270), flip_h: true, ..View::default() });
        assert!(s.len() == 2 && s.iter().all(|step| matches!(step, FrameStep::Cpu(_))));
    }

    /// `w` x `h` where the red is `40 * x` and the green `40 * y`.
    fn ramp(w: u32, h: u32) -> Frame {
        Frame::new(w, h, PixelFormat::Rgba, (0..w * h).flat_map(|i| [(i % w * 40) as u8, (i / w * 40) as u8, 7, 255]).collect())
    }

    fn channel(f: &Frame, c: usize) -> Vec<u8> {
        f.data.chunks(4).map(|p| p[c]).collect()
    }

    fn near(got: &[u8], want: &[u8]) {
        assert!(got.len() == want.len() && got.iter().zip(want).all(|(&a, &b)| a.abs_diff(b) <= 1), "{got:?} is not {want:?}");
    }

    #[test]
    fn a_zoom_takes_the_middle_half_at_2x_and_blends_between_pixels() {
        // The crop is x .25 to .75, so the output's first pixel centre is at 0.75 of the source's pixels: 40 * 0.75 = 30
        let out = through(&View { zoom: 2.0, ..View::default() }, ramp(4, 4));
        near(&channel(&out, 0)[..4], &[30, 50, 70, 90]);
        near(&channel(&out, 1).iter().step_by(4).copied().collect::<Vec<_>>(), &[30, 50, 70, 90]);
        assert_eq!((out.width, out.height), (4, 4), "the picture keeps its size");
        assert!(channel(&out, 3).iter().all(|&a| a == 255));
    }

    #[test]
    fn a_panned_zoom_repeats_the_edge_and_a_flip_turns_the_crop_over() {
        // Panned fully right, the window is x .5 to 1: first centre at 1.75 of the source pixels, the last one past the edge
        let out = through(&View { zoom: 2.0, pan_x: 1.0, ..View::default() }, ramp(4, 4));
        near(&channel(&out, 0)[..4], &[70, 90, 110, 120]);
        let flipped = through(&View { zoom: 2.0, flip_h: true, ..View::default() }, ramp(4, 4));
        near(&channel(&flipped, 0)[..4], &[90, 70, 50, 30]);
    }

    #[test]
    fn a_stretch_leaves_black_where_the_picture_does_not_reach() {
        // Half as wide: the outer pixels are bare, the inner two sit at 0.5 and 2.5 of the source's pixels
        let out = through(&View { stretch_x: 0.5, ..View::default() }, ramp(4, 2));
        near(&channel(&out, 0)[..4], &[0, 20, 100, 0]);
        near(&channel(&out, 2)[..4], &[0, 7, 7, 0]);
        assert!(channel(&out, 3).iter().all(|&a| a == 255), "black is opaque");
        // Wider than the picture: it fills the whole frame
        let wide = through(&View { stretch_x: 2.0, ..View::default() }, ramp(4, 2));
        assert!(channel(&wide, 2)[..4].iter().all(|&b| b == 7));
    }

    #[test]
    fn a_zoomed_yuv_picture_keeps_its_colour_and_format() {
        // Y 180, U 100, V 160 all over stays that colour through a zoom and a stretch
        for format in [PixelFormat::I420, PixelFormat::Nv12] {
            let (w, h) = (64u32, 48u32);
            let c = (w * h / 4) as usize;
            let chroma = if format == PixelFormat::I420 { [vec![100; c], vec![160; c]].concat() } else { [100u8, 160].repeat(c) };
            let f = Frame::new(w, h, format, [vec![180; (w * h) as usize], chroma].concat());
            let out = through(&View { zoom: 1.5, pan_x: 0.3, stretch_y: 1.1, ..View::default() }, f.clone());
            assert_eq!((out.width, out.height, out.format, out.data.len()), (w, h, format, f.data.len()));
            let diff = out.data.iter().zip(f.data.iter()).map(|(&a, &b)| a.abs_diff(b)).max().unwrap();
            assert!(diff <= 3, "{format:?} drifted by {diff}");
        }
    }

    #[test]
    fn a_quarter_turn_with_a_zoom_crops_the_picture_as_it_was_before_the_turn() {
        // 4 wide, 2 tall turned clockwise is 2 wide, 4 tall: the red climbs down the rows, and the green is 40 then 0 across them
        let plain = through(&View { rotation: Some(90), ..View::default() }, ramp(4, 2));
        assert_eq!(channel(&plain, 0), [0, 0, 40, 40, 80, 80, 120, 120]);
        let zoomed = through(&View { rotation: Some(90), zoom: 2.0, ..View::default() }, ramp(4, 2));
        assert_eq!((zoomed.width, zoomed.height), (2, 4));
        near(&channel(&zoomed, 0), &[30, 30, 50, 50, 70, 70, 90, 90]);
        near(&channel(&zoomed, 1), &[30, 10, 30, 10, 30, 10, 30, 10]);
    }

    /// Resample as it was before it kept its buffers and plans: fresh ones for every frame, and every bare column blended to black.
    mod before {
        use super::super::*;

        fn resample(pixels: Vec<u8>, w: usize, h: usize, pass: &Pass) -> Vec<u8> {
            let xs: Vec<Tap> = (0..w).map(|x| tap((pass.source((x as f64 + 0.5) / w as f64, 0.5).0 * w as f64 - 0.5) as f32, w)).collect();
            let ys: Vec<Tap> = (0..h).map(|y| tap((pass.source(0.5, (y as f64 + 0.5) / h as f64).1 * h as f64 - 0.5) as f32, h)).collect();
            let mut out = resample_linear(&pixels, w, h, 3, &xs, &ys);
            keep_bytes(pixels);
            if pass.is_stretch() {
                black_outside(&mut out, w, h, pass);
            }
            out
        }

        fn black_outside(rgb: &mut [u8], w: usize, h: usize, pass: &Pass) {
            let fx: Vec<f32> = (0..w).map(|x| covered(pass.stretched((x as f64 + 0.5) / w as f64, 0.5).0, w)).collect();
            let fy: Vec<f32> = (0..h).map(|y| covered(pass.stretched(0.5, (y as f64 + 0.5) / h as f64).1, h)).collect();
            let left = fx.iter().take_while(|&&f| f < 1.0).count();
            let right = w - fx.iter().rev().take_while(|&&f| f < 1.0).count();
            let (left, right) = (left.min(w), right.max(left));
            let scale = |px: &mut [u8], f: f32| px.iter_mut().for_each(|v| *v = (*v as f32 * f + 0.5) as u8);
            for (line, &f) in rgb.chunks_exact_mut(w * 3).zip(&fy) {
                if f == 0.0 {
                    line.fill(0);
                    continue;
                }
                for x in (0..left).chain(right..w) {
                    scale(&mut line[x * 3..][..3], fx[x] * f);
                }
                if f < 1.0 {
                    scale(&mut line[left * 3..right * 3], f);
                }
            }
        }

        pub fn process(passes: &[Pass], frame: &mut Frame) {
            let (w, h) = (frame.width as usize, frame.height as usize);
            let Some(mut rgb) = frame_rgb(frame) else { return };
            for pass in passes {
                rgb = resample(rgb, w, h, pass);
            }
            let format = frame.format;
            let data = frame.data_mut();
            match format {
                PixelFormat::Rgba => data.as_chunks_mut::<4>().0.iter_mut().zip(rgb.as_chunks::<3>().0).for_each(|(o, p)| *o = [p[0], p[1], p[2], 255]),
                PixelFormat::Bgra => data.as_chunks_mut::<4>().0.iter_mut().zip(rgb.as_chunks::<3>().0).for_each(|(o, p)| *o = [p[2], p[1], p[0], 255]),
                PixelFormat::I420 => rgb_to_420(&rgb, w, h, data, false),
                PixelFormat::Nv12 => rgb_to_420(&rgb, w, h, data, true),
            }
            keep_bytes(rgb);
        }
    }

    fn noise(w: u32, h: u32, format: PixelFormat, seed: u32) -> Frame {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        let data = (0..format.size(w, h)).map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        }).collect();
        Frame::new(w, h, format, data)
    }

    /// Views for every kind of pass: crops, flips, a half turn, stretches narrower, wider and far too narrow, and both at once.
    fn views() -> Vec<View> {
        vec![
            View { zoom: 2.0, pan_x: 0.3, ..View::default() },
            View { zoom: 1.37, pan_x: -1.0, pan_y: 0.6, flip_v: true, ..View::default() },
            View { rotation: Some(180), zoom: 1.5, ..View::default() },
            View { stretch_x: 0.8, ..View::default() },
            View { stretch_x: 0.5, stretch_y: 0.6, ..View::default() },
            View { stretch_x: 1.4, stretch_y: 0.9, ..View::default() },
            View { stretch_x: 0.02, ..View::default() },
            View { stretch_y: 0.03, stretch_x: 0.97, ..View::default() },
            View { zoom: 1.7, stretch_y: 1.1, flip_h: true, ..View::default() },
            View { zoom: 3.0, pan_y: 0.2, stretch_x: 0.75, stretch_y: 0.85, ..View::default() },
        ]
    }

    #[test]
    fn the_resampling_gives_the_bytes_it_always_gave() {
        for view in views() {
            for (w, h) in [(64, 48), (101, 67), (7, 5), (1, 1), (2, 9), (300, 260)] {
                for format in [PixelFormat::I420, PixelFormat::Nv12, PixelFormat::Rgba, PixelFormat::Bgra] {
                    let passes = passes(&view, 0);
                    let mut now = Resample::new(passes.clone());
                    // The next frames draw on the buffers the one before left
                    for seed in 0..3 {
                        let frame = noise(w, h, format, seed);
                        let (mut want, mut got) = (frame.clone(), frame);
                        before::process(&passes, &mut want);
                        now.process(&mut got, &NoCtx);
                        assert!(got.data == want.data, "{view:?} {w}x{h} {format:?} frame {seed}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_sampling_follows_the_size_of_the_frames() {
        let view = View { zoom: 1.6, stretch_x: 0.7, ..View::default() };
        let passes = passes(&view, 0);
        let mut now = Resample::new(passes.clone());
        for (w, h) in [(64, 48), (32, 24), (64, 48), (65, 48), (64, 48)] {
            let frame = noise(w, h, PixelFormat::I420, w);
            let (mut want, mut got) = (frame.clone(), frame);
            before::process(&passes, &mut want);
            now.process(&mut got, &NoCtx);
            assert!(got.data == want.data, "{w}x{h}");
        }
    }

    #[test]
    fn a_frame_that_isnt_a_picture_is_left_as_it_was() {
        let mut step = Resample::new(passes(&View { zoom: 2.0, ..View::default() }, 0));
        let mut short = Frame::new(4, 4, PixelFormat::I420, vec![0; PixelFormat::I420.size(4, 4)]);
        std::sync::Arc::make_mut(&mut short.data).truncate(3);
        let same = short.clone();
        step.process(&mut short, &NoCtx);
        assert_eq!(short, same);
    }

    /// Run by hand for the cost of a frame: cargo test --release -p telescope-core view_cost -- --ignored --nocapture
    /// Before: the step as it was; after: as it runs now.
    #[test]
    #[ignore]
    fn view_cost() {
        // The best and the median of a few rounds, since the machine is rarely quiet
        fn timed(mut run: impl FnMut()) -> (f64, f64) {
            (0..5).for_each(|_| run());
            let mut rounds: Vec<f64> = (0..9).map(|_| {
                let t = std::time::Instant::now();
                (0..8).for_each(|_| run());
                t.elapsed().as_secs_f64() * 1000.0 / 8.0
            }).collect();
            rounds.sort_by(f64::total_cmp);
            (rounds[0], rounds[4])
        }
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            for (name, view) in [("zoom 2x", View { zoom: 2.0, pan_x: 0.3, ..View::default() }), ("stretch", View { stretch_x: 0.8, ..View::default() }),
                                 ("zoom and stretch", View { zoom: 1.7, stretch_y: 1.1, flip_h: true, ..View::default() })] {
                for format in [PixelFormat::I420, PixelFormat::Rgba] {
                    let passes = passes(&view, 0);
                    let mut f = noise(w, h, format, 1);
                    let (best, median) = timed(|| before::process(&passes, &mut f));
                    println!("{w}x{h} {name} {format:?} before: best {best:.2} ms, median {median:.2} ms a frame");
                    let mut f = noise(w, h, format, 1);
                    let mut step = Resample::new(passes.clone());
                    let (best, median) = timed(|| step.process(&mut f, &NoCtx));
                    println!("{w}x{h} {name} {format:?} after: best {best:.2} ms, median {median:.2} ms a frame");
                }
            }
        }
    }

    /// Run by hand for the cost of a quarter turn: cargo test --release -p telescope-core turn_cost -- --ignored --nocapture
    #[test]
    #[ignore]
    fn turn_cost() {
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            for format in [PixelFormat::I420, PixelFormat::Nv12, PixelFormat::Rgba] {
                let f = Frame::new(w, h, format, (0..format.size(w, h)).map(|i| (i * 31 % 251) as u8).collect());
                for quarters in [1, 3] {
                    for _ in 0..3 {
                        std::hint::black_box(turn_frame(&f, quarters));
                    }
                    let t = std::time::Instant::now();
                    for _ in 0..30 {
                        std::hint::black_box(turn_frame(&f, quarters));
                    }
                    println!("turn {w}x{h} {format:?} {quarters}/4: {:.2} ms a frame", t.elapsed().as_secs_f64() * 1000.0 / 30.0);
                }
            }
        }
    }

    #[test]
    fn lens_correction_at_zero_adds_no_step_and_leaves_the_frame_alone() {
        assert!(steps(&View { distortion: 0.0, ..View::default() }).is_empty());
        let f = ramp(8, 6);
        assert_eq!(through(&View { distortion: 0.0, ..View::default() }, f.clone()).data, f.data);
    }

    #[test]
    fn lens_correction_is_a_cpu_step_that_runs_before_the_flip_and_the_zoom() {
        for view in [View { distortion: 0.5, ..View::default() }, View { distortion: -0.5, flip_h: true, ..View::default() }, View { distortion: 0.3, zoom: 2.0, ..View::default() }] {
            let s = steps(&view);
            assert!(s.iter().all(|step| matches!(step, FrameStep::Cpu(_))), "{view:?}");
        }
        assert_eq!(steps(&View { distortion: 0.5, ..View::default() }).len(), 1);
        assert_eq!(steps(&View { distortion: 0.5, flip_h: true, ..View::default() }).len(), 2, "the correction, then the plain flip");
        // Corrected first and mirrored after: the same as doing the two in turn
        let (w, h) = (64u32, 48u32);
        let f = Frame::new(w, h, PixelFormat::Rgba, (0..w * h).flat_map(|i| [(i * 7 % 251) as u8, (i * 13 % 241) as u8, (i % 61 * 4) as u8, 255]).collect());
        let both = through(&View { distortion: 0.6, flip_h: true, zoom: 1.5, ..View::default() }, f.clone());
        let apart = through(&View { flip_h: true, zoom: 1.5, ..View::default() }, through(&View { distortion: 0.6, ..View::default() }, f));
        assert_eq!(both.data, apart.data);
    }

    #[test]
    fn the_geometry_keys_say_when_the_pictures_pixels_moved() {
        let keys = |view: &View| -> Vec<u64> {
            steps(view).into_iter().map(|s| match s { FrameStep::Cpu(c) => c.geometry_key(), _ => panic!("a CPU step") }).collect()
        };
        let still = View { flip_h: true, zoom: 1.5, distortion: 0.3, ..View::default() };
        assert_eq!(keys(&still), keys(&still.clone()), "made again, the same");
        for moved in [View { flip_h: false, ..still.clone() }, View { zoom: 1.6, ..still.clone() }, View { pan_x: 0.2, ..still.clone() },
                      View { distortion: 0.31, ..still.clone() }, View { rotation: Some(90), ..still.clone() }, View { stretch_x: 1.1, ..still.clone() }] {
            assert_ne!(keys(&still), keys(&moved), "{moved:?}");
        }
    }

    #[test]
    fn a_refresh_of_the_view_keeps_the_lens_table_while_the_amount_and_size_stay() {
        // Other tests turn lens correction off, which empties the shared slots: a few tries
        let kept = (0..50).any(|_| {
            let (a, b) = (table_for(0.123, 96, 54), table_for(0.123, 96, 54));
            Arc::ptr_eq(&a, &b)
        });
        assert!(kept, "the same amount and size should reuse the table");
        let other = (0..50).any(|_| !Arc::ptr_eq(&table_for(0.123, 96, 54), &table_for(0.124, 96, 54)));
        assert!(other, "another amount is another table");
        // Two refreshes make two steps, and the second finds the first's table
        let mut frame = rgba(96, 54);
        let (mut one, mut two) = (Undistort { amount: 0.5, table: None, spares: Vec::new() }, Undistort { amount: 0.5, table: None, spares: Vec::new() });
        one.process(&mut frame, &NoCtx);
        two.process(&mut frame, &NoCtx);
        let shared = (0..50).any(|_| {
            one.table = None;
            two.table = None;
            one.process(&mut frame, &NoCtx);
            two.process(&mut frame, &NoCtx);
            matches!((&one.table, &two.table), (Some((_, _, a)), Some((_, _, b))) if Arc::ptr_eq(a, b))
        });
        assert!(shared);
    }

    #[test]
    fn lens_correction_keeps_the_edges_filled_and_the_crop_tight() {
        for (w, h) in [(1280usize, 720usize), (640, 480), (360, 640)] {
            for amount in [1.0, 0.4, 0.05, -0.05, -0.4, -1.0] {
                let lens = Lens::new(amount, w as f64 / h as f64);
                let s = lens.scale();
                let (mut reach_x, mut reach_y) = (0.0f64, 0.0f64);
                for y in (0..h).step_by(7).chain([h - 1]) {
                    for x in (0..w).step_by(7).chain([w - 1]) {
                        let (nx, ny) = lens.forward((x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64);
                        let (sx, sy) = (nx * w as f64 - 0.5, ny * h as f64 - 0.5);
                        assert!(sx >= -0.5 - 1e-6 && sx <= w as f64 - 0.5 + 1e-6 && sy >= -0.5 - 1e-6 && sy <= h as f64 - 0.5 + 1e-6, "{w}x{h} {amount}: ({x}, {y}) reads ({sx}, {sy})");
                        reach_x = reach_x.max((sx - (w as f64 - 1.0) / 2.0).abs());
                        reach_y = reach_y.max((sy - (h as f64 - 1.0) / 2.0).abs());
                    }
                }
                // Tight: some edge of the output reads right up to the edge of the source
                // (a full barrel setting stops at the scale where the mapping would fold back, short of the edge)
                assert!(amount > 0.5 || reach_x >= w as f64 / 2.0 - 2.0 || reach_y >= h as f64 / 2.0 - 2.0, "{w}x{h} {amount}: crop is wasteful ({reach_x}, {reach_y}) at scale {s}");
            }
            let scale = |amount: f64| Lens::new(amount, w as f64 / h as f64).scale();
            assert!(scale(-0.5) < 1.0 && scale(0.5) > 1.0 && (scale(1e-9) - 1.0).abs() < 1e-3);
        }
    }

    /// A grid of straight lines 100 pixels apart in the scene, photographed through a barrel lens of `amount`: each picture pixel is
    /// the scene point the lens model bends it to.
    fn barrel_grid(w: usize, h: usize, amount: f64) -> Frame {
        let c = amount * LENS_STRENGTH;
        let (hw, hh) = (w as f64 / 2.0, h as f64 / 2.0);
        let r = hw.hypot(hh);
        let mut data = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f64 + 0.5 - hw, y as f64 + 0.5 - hh);
                let rd = dx.hypot(dy) / r;
                // the scene radius the picture's rd shows: r_u * (1 - c r_u^2) = rd, on the rising side of the curve
                let (mut lo, mut hi) = (0.0, if c > 0.0 { 1.0 / (3.0 * c).sqrt() } else { 2.0 });
                for _ in 0..40 {
                    let mid = (lo + hi) / 2.0;
                    if mid * (1.0 - c * mid * mid) < rd { lo = mid } else { hi = mid }
                }
                let k = if rd > 0.0 { lo / rd } else { 1.0 };
                let (sx, sy) = (dx * k, dy * k);
                let line = |v: f64| (v - 100.0 * (v / 100.0).round()).abs();
                let d = line(sx).min(line(sy));
                let v = (255.0 * (1.0 - (1.6 - d).clamp(0.0, 1.0))) as u8;
                data.extend([v, v, v, 255]);
            }
        }
        Frame::new(w as u32, h as u32, PixelFormat::Rgba, data)
    }

    /// Where the dark line near `at` along a row (or a column) sits, by its weighted centre.
    fn line_at(f: &Frame, row: bool, index: usize, at: f64) -> f64 {
        let (w, h) = (f.width as usize, f.height as usize);
        let (mut sum, mut weighted) = (0.0, 0.0);
        for i in (at as i64 - 14).max(0) as usize..((at as i64 + 15) as usize).min(if row { w } else { h }) {
            let g = f.data[(if row { index * w + i } else { i * w + index }) * 4];
            let dark = 255.0 - g as f64;
            sum += dark;
            weighted += dark * (i as f64 + 0.5);
        }
        weighted / sum.max(1e-9)
    }

    /// How far the same grid line wanders between the rows (or columns) given, in pixels.
    fn wander(f: &Frame, amount: f64, vertical_line: bool) -> f64 {
        let (w, h) = (f.width as usize, f.height as usize);
        let s = Lens::new(amount, w as f64 / h as f64).scale();
        // A line near the edge, where the bend is greatest: x = -500 (or y = -200) of the scene
        let (scene, along): (f64, Vec<usize>) = if vertical_line { (-500.0, vec![80, 200, 360, 520, 640]) } else { (-200.0, vec![120, 400, 640, 900, 1160]) };
        let expected = if vertical_line { w as f64 / 2.0 } else { h as f64 / 2.0 } + scene / s;
        let found: Vec<f64> = along.iter().map(|&i| line_at(f, vertical_line, i, expected)).collect();
        found.iter().cloned().fold(f64::MIN, f64::max) - found.iter().cloned().fold(f64::MAX, f64::min)
    }

    #[test]
    fn a_barrel_distorted_grid_comes_out_straight_with_the_matching_setting() {
        let (w, h) = (1280usize, 720usize);
        for amount in [0.4, 0.15] {
            let photo = barrel_grid(w, h, amount);
            let fixed = through(&View { distortion: amount as f32, ..View::default() }, photo.clone());
            let half = through(&View { distortion: amount as f32 / 2.0, ..View::default() }, photo);
            for vertical in [true, false] {
                let (straight, bent) = (wander(&fixed, amount, vertical), wander(&half, amount, vertical));
                assert!(straight < 0.7, "{amount} vertical {vertical}: the line still wanders {straight} pixels");
                if amount > 0.3 {
                    assert!(bent > 2.0 * straight.max(0.3), "{amount} vertical {vertical}: half the setting should leave it bent, {bent} against {straight}");
                }
            }
        }
    }

    #[test]
    fn the_wrong_way_round_makes_the_bend_worse() {
        let photo = barrel_grid(1280, 720, 0.4);
        let wrong = through(&View { distortion: -0.4, ..View::default() }, photo);
        assert!(wander(&wrong, 0.4, true) > 3.0 || wander(&wrong, 0.4, false) > 3.0);
    }

    #[test]
    fn lens_correction_works_on_yuv_pictures_and_keeps_their_format() {
        for format in [PixelFormat::I420, PixelFormat::Nv12] {
            let (w, h) = (64u32, 48u32);
            let c = (w * h / 4) as usize;
            let chroma = if format == PixelFormat::I420 { [vec![100; c], vec![160; c]].concat() } else { [100u8, 160].repeat(c) };
            let f = Frame::new(w, h, format, [vec![180; (w * h) as usize], chroma].concat());
            for distortion in [0.7, -0.7] {
                let out = through(&View { distortion, ..View::default() }, f.clone());
                assert_eq!((out.width, out.height, out.format, out.data.len()), (w, h, format, f.data.len()));
                let diff = out.data.iter().zip(f.data.iter()).map(|(&a, &b)| a.abs_diff(b)).max().unwrap();
                assert!(diff <= 3, "{format:?} {distortion} drifted by {diff}");
            }
        }
    }

    /// Run by hand for the cost of a 1080p frame: cargo test --release -p telescope-core lens_cost -- --ignored --nocapture
    #[test]
    #[ignore]
    fn lens_cost() {
        for format in [PixelFormat::I420, PixelFormat::Rgba] {
            let data = (0..format.size(1920, 1080)).map(|i| (i * 31 % 251) as u8).collect();
            let mut f = Frame::new(1920, 1080, format, data);
            let t = std::time::Instant::now();
            let mut chain = steps(&View { distortion: 0.5, ..View::default() });
            let FrameStep::Cpu(step) = &mut chain[0] else { panic!("cpu") };
            step.process(&mut f, &NoCtx);
            println!("lens correction {format:?}: {:.2} ms for the first frame (table built)", t.elapsed().as_secs_f64() * 1000.0);
            for _ in 0..5 {
                step.process(&mut f, &NoCtx);
            }
            let t = std::time::Instant::now();
            for _ in 0..60 {
                step.process(&mut f, &NoCtx);
            }
            println!("lens correction {format:?}: {:.2} ms a frame", t.elapsed().as_secs_f64() * 1000.0 / 60.0);
        }
    }

    #[test]
    fn quarter_turns_match_opencv() {
        // 3 wide, 2 tall: [[0, 1, 2], [3, 4, 5]]; cv2.ROTATE_90_CLOCKWISE gives [[3, 0], [4, 1], [5, 2]]
        let f = rgba(3, 2);
        let cw = turn_frame(&f, 1);
        assert_eq!((cw.width, cw.height), (2, 3));
        assert_eq!(cw.data.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [3, 0, 4, 1, 5, 2]);
        // cv2.ROTATE_90_COUNTERCLOCKWISE gives [[2, 5], [1, 4], [0, 3]]
        let ccw = turn_frame(&f, 3);
        assert_eq!(ccw.data.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), [2, 5, 1, 4, 0, 3]);
        assert_eq!(turn_frame(&turn_frame(&f, 1), 3).data, f.data);
    }

    #[test]
    fn a_turned_yuv_picture_turns_each_plane() {
        let mut data: Vec<u8> = (0..4 * 2).collect();
        data.extend([100, 101]);
        data.extend([200, 201]);
        let f = Frame::new(4, 2, PixelFormat::I420, data);
        let t = turn_frame(&f, 1);
        assert_eq!((t.width, t.height), (2, 4));
        let (y, u, v) = t.i420_planes().unwrap();
        assert_eq!(y, [4, 0, 5, 1, 6, 2, 7, 3]);
        assert_eq!((u, v), (&[100, 101][..], &[200, 201][..]));
        let nv = Frame::new(2, 2, PixelFormat::Nv12, vec![1, 2, 3, 4, 9, 8]);
        assert_eq!(*turn_frame(&nv, 1).data, vec![3, 1, 4, 2, 9, 8]);
    }

    #[test]
    fn the_resampling_is_told_the_window_the_zoom_crops() {
        let pass = Pass::new(&View { zoom: 2.0, pan_x: 1.0, pan_y: -1.0, ..View::default() }, 0, true, false);
        assert_eq!((pass.x0, pass.y0, pass.crop_w), (0.5, 0.0, 0.5));
        let pass = Pass::new(&View { rotation: Some(90), zoom: 2.0, ..View::default() }, 1, true, false);
        assert_eq!((pass.turn, pass.done), (1, 1));
    }

    #[test]
    fn a_step_keeps_the_view_it_was_made_with() {
        let mut view = View { flip_h: true, zoom: 2.0, ..View::default() };
        let step = steps(&view);
        view.flip_h = false;
        view.rotation = Some(90);
        view.zoom = 1.0;
        assert!(!view.flip_h && view.rotation.is_some() && view.zoom == 1.0);
        let FrameStep::Cpu(mut cpu) = step.into_iter().next().unwrap() else { panic!() };
        let mut f = ramp(4, 4);
        cpu.process(&mut f, &NoCtx);
        near(&channel(&f, 0)[..4], &[90, 70, 50, 30]);
    }

    #[test]
    fn building_one_lens_table_does_not_hold_up_everything_else_that_wants_the_cache() {
        let builder = std::thread::spawn(|| {
            let began = std::time::Instant::now();
            table_for(0.37, 4096, 3072);
            began.elapsed()
        });
        std::thread::sleep(std::time::Duration::from_millis(30));
        let began = std::time::Instant::now();
        // what the shell does on every refresh of the view with the lens off
        steps(&View::default());
        let waited = began.elapsed();
        let build = builder.join().unwrap();
        // A machine that builds it this fast can't show the wait either way
        if build < std::time::Duration::from_millis(100) {
            return;
        }
        assert!(waited < build / 3, "steps() waited {waited:?} of a {build:?} build");
    }
}
