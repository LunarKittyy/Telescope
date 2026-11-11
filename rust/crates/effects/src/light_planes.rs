//! Light painting on the planes of the frame itself. The trail stays RGB, but a frame with little lit in it never leaves its planes:
//! each pair of rows knows the columns its trail covers, and only those columns plus the ones lit now are taken to RGB, worked
//! and put back. Everything else is left exactly as the frame had it.

use crate::fun_effects::{light_row, Light, Look};
use std::sync::OnceLock;
use telescope_core::avx2_too;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::image::{byte, rgb_to_420};
use telescope_core::workers::each_part;

/// The columns a pair of rows is scanned for lit pixels in at a time.
const CHUNK: usize = 64;
/// More bands of rows than threads, so a helper held back only delays a little of the frame.
const BANDS: usize = 8;
/// Rows of the picture below which the work isn't worth spreading over threads.
const BANDED_STRIPS: usize = 128;

type Span = (usize, usize);

/// What one band of row pairs works on: its rows of the frame and of the trail, and the columns to work on in each pair.
struct Band<'a> {
    y: &'a mut [u8],
    u: &'a mut [u8],
    v: &'a mut [u8],
    trail: &'a mut [u8],
    spans: &'a mut [Span],
    active: &'a [Span],
}

/// Scratch for one pair of rows, kept for the band's other pairs.
#[derive(Default)]
struct Rows {
    frame: Vec<u8>,
    out: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    packed: Vec<u8>,
}

impl Light {
    /// The frame with its light trails painted in. None for a frame this doesn't work on (an odd size, a packed format), left as it was.
    pub(crate) fn run_planes(&mut self, frame: &mut Frame, look: &Look) -> Option<()> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let nv12 = frame.format == PixelFormat::Nv12;
        if !(nv12 || frame.format == PixelFormat::I420) || w < 2 || h < 2 || w % 2 != 0 || h % 2 != 0 {
            return None;
        }
        let (cw, ch) = (w / 2, h / 2);
        if frame.data.len() < w * h + 2 * cw * ch {
            return None;
        }
        let level = (150 + look.brightness * 10) as u8;
        let keep = (0.80 + look.fade as f64 * 0.019) as f32;
        let strips = h / 2;
        self.ready(w, h);
        let floor = luma_floor(level);
        let mut active: Vec<Span> = vec![(0, 0); strips];
        {
            let luma = &frame.data[..w * h];
            let spans = &self.spans;
            let per = per_band(strips);
            let mut parts: Vec<(usize, &mut [Span])> = active.chunks_mut(per).enumerate().map(|(i, a)| (i * per, a)).collect();
            let scan = |_: usize, part: &mut (usize, &mut [Span])| {
                let first = part.0;
                for (i, slot) in part.1.iter_mut().enumerate() {
                    let at = (first + i) * 2 * w;
                    *slot = wanted(lit_span(&luma[at..at + 2 * w], w, floor), spans[first + i], w);
                }
            };
            if strips < BANDED_STRIPS {
                parts.iter_mut().for_each(|p| scan(0, p));
            } else {
                each_part(&mut parts, scan);
            }
        }
        if active.iter().all(|(a, b)| a >= b) {
            return Some(());
        }
        let per = per_band(strips);
        let data = frame.data_mut();
        let (luma, chroma) = data.split_at_mut(w * h);
        let (us, vs): (Vec<&mut [u8]>, Vec<&mut [u8]>) = if nv12 {
            (chroma.chunks_mut(per * cw * 2).collect(), (0..strips.div_ceil(per)).map(|_| Default::default()).collect())
        } else {
            let (u, v) = chroma.split_at_mut(cw * ch);
            (u.chunks_mut(per * cw).collect(), v.chunks_mut(per * cw).take(strips.div_ceil(per)).collect())
        };
        let mut parts: Vec<Band> = luma
            .chunks_mut(per * 2 * w)
            .zip(us)
            .zip(vs)
            .zip(self.trail.as_mut().unwrap().data.chunks_mut(per * 2 * w * 3))
            .zip(self.spans.chunks_mut(per))
            .zip(active.chunks(per))
            .map(|(((((y, u), v), trail), spans), active)| Band { y, u, v, trail, spans, active })
            .collect();
        let work = |_: usize, band: &mut Band| {
            let mut rows = Rows::default();
            for i in 0..band.active.len() {
                let (x0, x1) = band.active[i];
                if x0 >= x1 {
                    continue;
                }
                let y = &mut band.y[i * 2 * w..(i + 1) * 2 * w];
                let (u, v) = if nv12 { (&mut band.u[i * cw * 2..(i + 1) * cw * 2], &mut [][..]) } else { (&mut band.u[i * cw..(i + 1) * cw], &mut band.v[i * cw..(i + 1) * cw]) };
                let trail = &mut band.trail[i * 2 * w * 3..(i + 1) * 2 * w * 3];
                band.spans[i] = strip(&mut rows, y, (u, v), trail, (w, x0, x1), nv12, level, keep);
            }
        };
        if strips < BANDED_STRIPS {
            parts.iter_mut().for_each(|p| work(0, p));
        } else {
            each_part(&mut parts, work);
        }
        Some(())
    }
}

/// The luma at and below which no chroma can bring a pixel's brightness above `level`. Chroma that clips a channel can raise the
/// brightness over what the luma says (Y 151 with saturated green is over 160), so this is worked out for every luma against every chroma.
fn luma_floor(level: u8) -> u8 {
    static MOST: OnceLock<[u8; 256]> = OnceLock::new();
    let most = MOST.get_or_init(|| {
        let mut most = [0u8; 256];
        for (y, m) in most.iter_mut().enumerate() {
            let c = (y as f32 - 16.0) * 1.164_383;
            for u in 0..=255u32 {
                let d = u as f32 - 128.0;
                let (b, g0) = (byte(c + 2.017_232 * d), c - 0.391_762 * d);
                for v in 0..=255u32 {
                    let e = v as f32 - 128.0;
                    let (r, g) = (byte(c + 1.596_027 * e), byte(g0 - 0.812_968 * e));
                    let grey = ((r as u32 * 9798 + g as u32 * 19235 + b as u32 * 3735 + (1 << 14)) >> 15) as u8;
                    *m = (*m).max(grey);
                }
            }
        }
        most
    });
    // One level of room, so a rounding step in the real path never leaves a lit pixel out
    (0..255usize).rev().find(|&y| most[y] as u32 + 1 <= level as u32).unwrap_or(0) as u8
}

fn per_band(strips: usize) -> usize {
    if strips < BANDED_STRIPS { strips.max(1) } else { strips.div_ceil(BANDS) }
}

/// The columns, in whole chunks, where either row of a pair of rows has luma above `floor`; None when neither does.
fn lit_span(rows: &[u8], w: usize, floor: u8) -> Option<Span> {
    let lit = |c: &[u8]| c.iter().copied().max().is_some_and(|m| m > floor);
    let chunks = |row: &[u8]| {
        let first = row.chunks(CHUNK).position(&lit)?;
        let last = row.chunks(CHUNK).rposition(&lit)?;
        Some((first * CHUNK, ((last + 1) * CHUNK).min(w)))
    };
    let (a, b) = (chunks(&rows[..w]), chunks(&rows[w..]));
    match (a, b) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
        (a, b) => a.or(b),
    }
}

/// What to work on for a pair of rows: the lit columns and the ones the trail is in, whole pairs of pixels.
fn wanted(lit: Option<Span>, trail: Span, w: usize) -> Span {
    let held = (trail.0 < trail.1).then_some(trail);
    match (lit, held) {
        (None, None) => (0, 0),
        (a, b) => {
            let (lo, hi) = a.into_iter().chain(b).fold((w, 0), |(lo, hi), (x, y)| (lo.min(x), hi.max(y)));
            (lo & !1, ((hi + 1) & !1).min(w))
        }
    }
}

avx2_too! {
    /// A run of luma with its chroma, one value a pixel, as R, G, B (BT.601 video range).
    fn to_rgb(y: &[u8], u: &[u8], v: &[u8], out: &mut [u8]) {
        for (((o, &y), &u), &v) in out.as_chunks_mut::<3>().0.iter_mut().zip(y).zip(u).zip(v) {
            let c = (y as f32 - 16.0) * 1.164_383;
            let (d, e) = (u as f32 - 128.0, v as f32 - 128.0);
            *o = [byte(c + 1.596_027 * e), byte(c - 0.391_762 * d - 0.812_968 * e), byte(c + 2.017_232 * d)];
        }
    }
}

/// One pair of rows (y: both rows of the frame's luma, u and v: the row of chroma they share, trail: both rows of the trail) over the
/// columns x0..x1: the trail fades and takes what is lit, the picture goes over it. Returns where the trail is not black now.
#[allow(clippy::too_many_arguments)]
fn strip(rows: &mut Rows, y: &mut [u8], (u, v): (&mut [u8], &mut [u8]), trail: &mut [u8], (w, x0, x1): (usize, usize, usize), nv12: bool, level: u8, keep: f32) -> Span {
    let n = x1 - x0;
    let (c0, cells) = (x0 / 2, n / 2);
    rows.frame.resize(2 * n * 3, 0);
    rows.out.resize(2 * n * 3, 0);
    rows.u.resize(n, 0);
    rows.v.resize(n, 0);
    // Each chroma cell is given to its two pixels
    for i in 0..cells {
        let (a, b) = if nv12 { (u[(c0 + i) * 2], u[(c0 + i) * 2 + 1]) } else { (u[c0 + i], v[c0 + i]) };
        rows.u[i * 2..i * 2 + 2].fill(a);
        rows.v[i * 2..i * 2 + 2].fill(b);
    }
    for r in 0..2 {
        to_rgb(&y[r * w + x0..r * w + x1], &rows.u, &rows.v, &mut rows.frame[r * n * 3..(r + 1) * n * 3]);
    }
    let mut now: Option<Span> = None;
    for r in 0..2 {
        let line = r * n * 3..(r + 1) * n * 3;
        let held = &mut trail[(r * w + x0) * 3..(r * w + x1) * 3];
        light_row(&rows.frame[line.clone()], held, &mut rows.out[line], level, keep, false);
        let first = held.as_chunks::<3>().0.iter().position(|p| p != &[0, 0, 0]);
        let last = held.as_chunks::<3>().0.iter().rposition(|p| p != &[0, 0, 0]);
        if let (Some(a), Some(b)) = (first, last) {
            let (a, b) = (x0 + a, x0 + b + 1);
            now = Some(now.map_or((a, b), |(lo, hi)| (lo.min(a), hi.max(b))));
        }
    }
    // Nothing the trail or the lights changed: the frame keeps its own luma and chroma
    if rows.out != rows.frame {
        rows.packed.resize(2 * n + 2 * cells, 0);
        rgb_to_420(&rows.out, n, 2, &mut rows.packed, nv12);
        let (luma, colour) = rows.packed.split_at(2 * n);
        for r in 0..2 {
            y[r * w + x0..r * w + x1].copy_from_slice(&luma[r * n..(r + 1) * n]);
        }
        if nv12 {
            u[c0 * 2..(c0 + cells) * 2].copy_from_slice(&colour[..cells * 2]);
        } else {
            u[c0..c0 + cells].copy_from_slice(&colour[..cells]);
            v[c0..c0 + cells].copy_from_slice(&colour[cells..cells * 2]);
        }
    }
    now.unwrap_or((0, 0))
}

#[cfg(test)]
#[path = "light_planes_tests.rs"]
mod tests;
