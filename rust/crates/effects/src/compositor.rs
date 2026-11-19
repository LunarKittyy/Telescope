//! Drawing a layout (composite.Compositor): the background, then each stream cropped, scaled and pasted into its box back to
//! front, with its border and rounded corners. Python draws in BGR with OpenCV; this draws straight into the frame's own
//! planes (I420 as the decoder makes it, or RGBA and BGRA), so nothing is converted on the way. Scaling follows OpenCV's
//! (exact halves by INTER_AREA, then INTER_LINEAR's fixed-point taps) and the corners blend as cv2.blendLinear does, so a
//! packed picture comes out as Python's does; in I420 the colour planes are half size, and a box edge on an odd pixel shares
//! its colour sample with what's beside it.

use std::borrow::Cow;
use crate::composite::{bgr, box_px, crop_px, scaled_size, Layout, Tile};
use crate::cpu::Rgb;
use crate::imgops::resize_area;
use crate::pixels::{put_rgb, to_rgb};
use crate::shared::{no_pictures, PictureReader};
use crate::transition::{self, Spec};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::image::{halve_2x2, halve_2x2_into, keep_bytes, over_rows, resample_box, resample_box_into, spare_bytes, with_spares};
use telescope_core::time::monotonic_ns;

/// The frames to draw from: each stream's newest, with its number when the same frame can come again (None: always fresh).
pub type Frames = BTreeMap<String, (Frame, Option<u64>)>;

/// The planes a picture is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    I420,
    Rgba,
    Bgra,
}

impl Kind {
    /// What a frame of this format is drawn as: anything not packed RGB goes out as I420.
    pub fn of(format: PixelFormat) -> Kind {
        match format {
            PixelFormat::Rgba => Kind::Rgba,
            PixelFormat::Bgra => Kind::Bgra,
            _ => Kind::I420,
        }
    }

    pub fn format(self) -> PixelFormat {
        match self {
            Kind::I420 => PixelFormat::I420,
            Kind::Rgba => PixelFormat::Rgba,
            Kind::Bgra => PixelFormat::Bgra,
        }
    }
}

/// One plane of a w x h picture: its size, bytes a pixel and where it starts in the frame's data.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Plane {
    w: usize,
    h: usize,
    ch: usize,
    at: usize,
}

fn planes(kind: Kind, w: usize, h: usize) -> Vec<Plane> {
    match kind {
        Kind::I420 => {
            let (cw, chh) = (w.div_ceil(2), h.div_ceil(2));
            vec![Plane { w, h, ch: 1, at: 0 }, Plane { w: cw, h: chh, ch: 1, at: w * h }, Plane { w: cw, h: chh, ch: 1, at: w * h + cw * chh }]
        }
        _ => vec![Plane { w, h, ch: 4, at: 0 }],
    }
}

/// A box's (x0, y0, x1, y1) in a plane that's 1 / scale of the picture each way: every sample it touches.
fn plane_box(scale: usize, (x, y, w, h): (usize, usize, usize, usize)) -> (usize, usize, usize, usize) {
    (x / scale, y / scale, (x + w).div_ceil(scale), (y + h).div_ceil(scale))
}

// ── Scaling as OpenCV does ────────────────────────────────────────────────

/// A rectangle of a plane: the plane's bytes, its width in pixels, bytes a pixel, and (x0, y0, w, h).
#[derive(Clone, Copy)]
struct Region<'a> {
    data: &'a [u8],
    stride: usize,
    ch: usize,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
}

impl Region<'_> {
    #[cfg(test)]
    fn at(&self, x: usize, y: usize, c: usize) -> u8 {
        self.data[((self.y0 + y) * self.stride + self.x0 + x) * self.ch + c]
    }

    fn copy(&self) -> Vec<u8> {
        let mut out = spare_bytes(self.w * self.h * self.ch);
        for y in 0..self.h {
            let start = ((self.y0 + y) * self.stride + self.x0) * self.ch;
            let row = self.w * self.ch;
            out[y * row..(y + 1) * row].copy_from_slice(&self.data[start..start + row]);
        }
        out
    }
}

/// INTER_LINEAR's taps along an axis of n scaled to n2: (first source, second source, their weights out of 2048).
fn linear_taps(n: usize, n2: usize) -> Vec<(usize, usize, i32, i32)> {
    let scale = 1.0 / (n2 as f64 / n as f64);
    (0..n2).map(|d| {
        let mut f = ((d as f64 + 0.5) * scale - 0.5) as f32;
        let mut s = f.floor() as i64;
        f -= s as f32;
        if s < 0 {
            (f, s) = (0.0, 0);
        }
        if s >= n as i64 - 1 {
            (f, s) = (0.0, n as i64 - 1);
        }
        let s = s as usize;
        (s, (s + 1).min(n - 1), ((1.0 - f) * 2048.0).round_ties_even() as i32, (f * 2048.0).round_ties_even() as i32)
    }).collect()
}

/// cv2.resize(INTER_LINEAR) of an 8-bit region: whole-number taps across, then down with OpenCV's SSE rounding, banded and AVX2 in core.
fn linear(src: Region, w2: usize, h2: usize) -> Vec<u8> {
    let (xs, ys) = (linear_taps(src.w, w2), linear_taps(src.h, h2));
    resample_box(src.data, src.stride, src.ch, (src.x0, src.y0, src.w, src.h), &xs, &ys)
}

/// composite.scale_to on a region: in exact halves while it's at least twice the size each way, then INTER_LINEAR the rest.
fn scale_region(src: Region, w: usize, h: usize) -> Vec<u8> {
    if (src.w, src.h) == (w, h) {
        return src.copy();
    }
    halves_then_linear(src, w, h)
}

fn halves_then_linear(src: Region, w: usize, h: usize) -> Vec<u8> {
    if src.w < 2 * w || src.h < 2 * h {
        return linear(src, w, h);
    }
    let mut image = halve_2x2(src.data, src.stride, src.ch, (src.x0, src.y0, src.w, src.h));
    let (mut iw, mut ih) = (src.w / 2, src.h / 2);
    while iw >= 2 * w && ih >= 2 * h {
        let next = halve_2x2(&image, iw, src.ch, (0, 0, iw, ih));
        keep_bytes(std::mem::replace(&mut image, next));
        (iw, ih) = (iw / 2, ih / 2);
    }
    if (iw, ih) == (w, h) {
        return image;
    }
    let out = linear(Region { data: &image, stride: iw, ch: src.ch, x0: 0, y0: 0, w: iw, h: ih }, w, h);
    keep_bytes(image);
    out
}

/// scale_region into complete destination rows, using the same halves and fixed-point taps.
fn scale_region_into(src: Region, w: usize, h: usize, out: &mut [u8], stride: usize, left: usize) {
    if (src.w, src.h) == (w, h) {
        over_rows(out, stride, |y, row| {
            if y < h {
                let at = ((src.y0 + y) * src.stride + src.x0) * src.ch;
                row[left..left + w * src.ch].copy_from_slice(&src.data[at..at + w * src.ch]);
            }
        });
        return;
    }
    if src.w < 2 * w || src.h < 2 * h {
        let (xs, ys) = (linear_taps(src.w, w), linear_taps(src.h, h));
        resample_box_into(src.data, (src.stride, src.ch), (src.x0, src.y0, src.w, src.h), &xs, &ys, out, (stride, left));
        return;
    }
    if (src.w / 2, src.h / 2) == (w, h) {
        halve_2x2_into(src.data, src.stride, src.ch, (src.x0, src.y0, src.w, src.h), out, (stride, left));
        return;
    }
    let mut image = halve_2x2(src.data, src.stride, src.ch, (src.x0, src.y0, src.w, src.h));
    let (mut iw, mut ih) = (src.w / 2, src.h / 2);
    while iw >= 2 * w && ih >= 2 * h {
        if (iw / 2, ih / 2) == (w, h) {
            halve_2x2_into(&image, iw, src.ch, (0, 0, iw, ih), out, (stride, left));
            keep_bytes(image);
            return;
        }
        let next = halve_2x2(&image, iw, src.ch, (0, 0, iw, ih));
        keep_bytes(std::mem::replace(&mut image, next));
        (iw, ih) = (iw / 2, ih / 2);
    }
    let (xs, ys) = (linear_taps(iw, w), linear_taps(ih, h));
    resample_box_into(&image, (iw, src.ch), (0, 0, iw, ih), &xs, &ys, out, (stride, left));
    keep_bytes(image);
}

/// composite.scale_to on a whole w x h picture of ch bytes a pixel.
pub fn scale_to(src: &[u8], w: usize, h: usize, ch: usize, w2: usize, h2: usize) -> Cow<'_, [u8]> {
    if (w, h) == (w2, h2) {
        return Cow::Borrowed(src);
    }
    Cow::Owned(halves_then_linear(Region { data: src, stride: w, ch, x0: 0, y0: 0, w, h }, w2, h2))
}

/// pictures.cover: the picture scaled to fill w x h (INTER_AREA smaller, INTER_LINEAR bigger), cut evenly where it doesn't fit.
pub fn cover(image: &Rgb, w: usize, h: usize) -> Rgb {
    let scale = (w as f64 / image.w as f64).max(h as f64 / image.h as f64);
    let sw = w.max((image.w as f64 * scale).round_ties_even() as usize);
    let sh = h.max((image.h as f64 * scale).round_ties_even() as usize);
    let scaled = if scale < 1.0 {
        resize_area(&image.data, image.w, image.h, 3, sw, sh)
    } else {
        linear(Region { data: &image.data, stride: image.w, ch: 3, x0: 0, y0: 0, w: image.w, h: image.h }, sw, sh)
    };
    let (x0, y0) = ((sw - w) / 2, (sh - h) / 2);
    Rgb { w, h, data: Region { data: &scaled, stride: sw, ch: 3, x0, y0, w, h }.copy() }
}

// ── Rounded corners ───────────────────────────────────────────────────────

/// How much of each pixel of a radius x radius corner square is inside the curve, smoothed a pixel wide at it. The four
/// corners are mirrors of one another, so one square is kept and read flipped.
#[derive(Default)]
pub struct CornerMasks {
    radius: usize,
    inside: Vec<f32>,
}

impl CornerMasks {
    /// The inside weight at (x, y) of a corner (0 top left, 1 top right, 2 bottom left, 3 bottom right); outside is one minus it.
    pub fn inside(&self, corner: usize, x: usize, y: usize) -> f32 {
        let r = self.radius;
        let (x, y) = (if corner & 1 == 1 { r - 1 - x } else { x }, if corner & 2 == 2 { r - 1 - y } else { y });
        self.inside[y * r + x]
    }

    fn bytes(&self) -> usize {
        self.inside.len() * size_of::<f32>()
    }
}

/// What the kept corner squares may add up to: a few typical radii, or one big one, and never more.
const CORNER_CACHE_BYTES: usize = 8 * 1024 * 1024;

fn corner_cache() -> &'static Mutex<HashMap<usize, Arc<CornerMasks>>> {
    static MASKS: OnceLock<Mutex<HashMap<usize, Arc<CornerMasks>>>> = OnceLock::new();
    MASKS.get_or_init(Mutex::default)
}

/// composite.corner_masks: worked out once per radius.
pub fn corner_masks(radius: usize) -> Arc<CornerMasks> {
    let mut cache = corner_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = cache.get(&radius) {
        return m.clone();
    }
    let r = radius;
    let rf = radius as f32;
    let mut inside = vec![0f32; r * r];
    for y in 0..r {
        for x in 0..r {
            let (dx, dy) = (rf - (x as f32 + 0.5), rf - (y as f32 + 0.5));
            let dist = (dx * dx + dy * dy).sqrt();
            inside[y * r + x] = (rf - dist + 0.5).clamp(0.0, 1.0);
        }
    }
    let masks = Arc::new(CornerMasks { radius, inside });
    if cache.values().map(|m| m.bytes()).sum::<usize>() + masks.bytes() > CORNER_CACHE_BYTES {
        cache.clear();
    }
    cache.insert(radius, masks.clone());
    masks
}

/// The bytes the kept corner squares take, for the tests.
#[cfg(test)]
pub(crate) fn corner_cache_bytes() -> usize {
    corner_cache().lock().unwrap_or_else(|e| e.into_inner()).values().map(|m| m.bytes()).sum()
}

/// cv2.blendLinear of one value.
fn blend(new: u8, under: u8, inside: f32, outside: f32) -> u8 {
    ((new as f32 * inside + under as f32 * outside) / (inside + outside + 1e-5)).round_ties_even().clamp(0.0, 255.0) as u8
}

/// What goes in a box: one colour, or a picture the box's size in the canvas's planes.
enum Content<'a> {
    Colour(Vec<u8>),
    Picture(&'a [Vec<u8>]),
}

/// How much of the luma pixel (lx, ly) a w x h box at (x, y) with corners of radius r covers.
fn coverage(lx: usize, ly: usize, (x, y, w, h): (usize, usize, usize, usize), r: usize, masks: &CornerMasks) -> f32 {
    if lx < x || lx >= x + w || ly < y || ly >= y + h {
        return 0.0;
    }
    if r == 0 {
        return 1.0;
    }
    let (left, top) = (lx < x + r, ly < y + r);
    let (right, bottom) = (lx >= x + w - r, ly >= y + h - r);
    let corner = match (left, right, top, bottom) {
        (true, _, true, _) => Some((0, lx - x, ly - y)),
        (_, true, true, _) => Some((1, lx - (x + w - r), ly - y)),
        (true, _, _, true) => Some((2, lx - x, ly - (y + h - r))),
        (_, true, _, true) => Some((3, lx - (x + w - r), ly - (y + h - r))),
        _ => None,
    };
    corner.map_or(1.0, |(i, cx, cy)| masks.inside(i, cx, cy))
}

/// composite.paste: content into the box at full size, its corners rounded. Everything is copied straight over and only the
/// corner squares are blended with what was under them; I420's half-size colour planes blend by how much of each sample's
/// 2 x 2 pixels the box covers.
fn paste(canvas: &mut [u8], kind: Kind, cw: usize, chh: usize, content: Content, bx: (usize, usize, usize, usize), radius: usize) {
    let (x, y, w, h) = bx;
    let r = radius.min(w / 2).min(h / 2);
    let masks = if r > 0 { Some(corner_masks(r)) } else { None };
    for (p, plane) in planes(kind, cw, chh).into_iter().enumerate() {
        let full = plane.w == cw;
        let (px0, py0, px1, py1) = if full { (x, y, x + w, y + h) } else { plane_box(2, bx) };
        let (bw, ch) = (px1 - px0, plane.ch);
        let source = |i: usize| -> u8 {
            match &content {
                Content::Colour(c) => c[if full { i % ch } else { p }],
                Content::Picture(planes) => planes[p][i],
            }
        };
        let data = &mut canvas[plane.at..plane.at + plane.w * plane.h * ch];
        if full {
            let corners = [(x, y), (x + w - r, y), (x, y + h - r), (x + w - r, y + h - r)];
            let under: Vec<u8> = if r > 0 {
                corners.iter().flat_map(|&(cx0, cy0)| (0..r).flat_map(move |yy| {
                    let at = ((cy0 + yy) * plane.w + cx0) * ch;
                    at..at + r * ch
                })).map(|at| data[at]).collect()
            } else {
                Vec::new()
            };
            for yy in 0..py1 - py0 {
                let start = ((py0 + yy) * plane.w + px0) * ch;
                let row = &mut data[start..start + bw * ch];
                match &content {
                    Content::Picture(planes) => row.copy_from_slice(&planes[p][yy * bw * ch..(yy + 1) * bw * ch]),
                    Content::Colour(_) => row.iter_mut().enumerate().for_each(|(i, v)| *v = source(i)),
                }
            }
            if let Some(m) = &masks {
                let mut kept = under.into_iter();
                for (corner, &(cx0, cy0)) in corners.iter().enumerate() {
                    for yy in 0..r {
                        for xx in 0..r * ch {
                            let at = ((cy0 + yy) * plane.w + cx0) * ch + xx;
                            let inside = m.inside(corner, xx / ch, yy);
                            data[at] = blend(data[at], kept.next().unwrap_or(0), inside, 1.0 - inside);
                        }
                    }
                }
            }
            continue;
        }
        // A half-size colour plane: samples whose pixels are all well inside are copied, the rest blended by coverage
        let empty: CornerMasks = Default::default();
        let m = masks.as_deref().unwrap_or(&empty);
        let inside = |l: usize, lo: usize, hi: usize| l >= lo && l < hi;
        // The luma lines or columns of one sample: at most two, the last of an odd size has one
        let lines = |c: usize, limit: usize| -> ([usize; 2], usize) {
            let n = if 2 * c + 1 < limit { 2 } else if 2 * c < limit { 1 } else { 0 };
            ([2 * c, 2 * c + 1], n)
        };
        let all_in = |(l, n): ([usize; 2], usize), lo: usize, hi: usize| l[..n].iter().all(|&v| inside(v, lo, hi));
        // the samples of a row whose columns are all inside the box are one run; the cells beside it are the edge
        let run_lo = (px0..px1).find(|&cx| all_in(lines(cx, cw), x, x + w)).unwrap_or(px1);
        let run_hi = (run_lo..px1).rev().find(|&cx| all_in(lines(cx, cw), x, x + w)).map_or(run_lo, |c| c + 1);
        for cy in py0..py1 {
            let rows = lines(cy, chh);
            let in_v = all_in(rows, y, y + h);
            let plain_v = all_in(rows, y + r, y + h - r);
            let cell = |cx: usize, data: &mut [u8]| {
                let i = (cy - py0) * bw + (cx - px0);
                let at = cy * plane.w + cx;
                let cols = lines(cx, cw);
                let in_h = all_in(cols, x, x + w);
                let plain_h = all_in(cols, x + r, x + w - r);
                if in_v && in_h && (plain_v || plain_h) {
                    data[at] = source(i);
                    return;
                }
                let n = (rows.1 * cols.1) as f32;
                let mut sum = 0f32;
                for &ly in &rows.0[..rows.1] {
                    for &lx in &cols.0[..cols.1] {
                        sum += coverage(lx, ly, bx, r, m);
                    }
                }
                let cov = if n > 0.0 { sum / n } else { 0.0 };
                if cov >= 1.0 {
                    data[at] = source(i);
                } else if cov > 0.0 {
                    data[at] = blend(source(i), data[at], cov, 1.0 - cov);
                }
            };
            if in_v && plain_v && run_lo < run_hi {
                (px0..run_lo).chain(run_hi..px1).for_each(|cx| cell(cx, data));
                let (start, i) = (cy * plane.w + run_lo, (cy - py0) * bw + (run_lo - px0));
                let span = &mut data[start..start + (run_hi - run_lo)];
                match &content {
                    Content::Picture(planes) => span.copy_from_slice(&planes[p][i..i + span.len()]),
                    Content::Colour(c) => span.fill(c[p]),
                }
            } else {
                (px0..px1).for_each(|cx| cell(cx, data));
            }
        }
    }
}

/// x / 255 rounded, for x up to 255 * 255.
fn div255(x: u32) -> u32 {
    (x + 127) / 255
}

/// A frame whose colour is multiplied by its alpha, as straight colour: every value divided by its alpha (a colour sample by the mean of its 2 x 2).
fn unpremultiplied(frame: &Frame, alpha: &telescope_core::frame::Alpha) -> Frame {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let divide = |v: u8, a: u32| (v as u32 * 255 + a / 2).checked_div(a).map_or(0, |q| q.min(255) as u8);
    let mut out = frame.clone();
    let data = out.data_mut();
    let block = |cx: usize, cy: usize| {
        let (x1, y1) = ((2 * cx + 2).min(w), (2 * cy + 2).min(h));
        let (mut sum, mut n) = (0u32, 0u32);
        for y in 2 * cy..y1 {
            for x in 2 * cx..x1 {
                sum += alpha.data[y * w + x] as u32;
                n += 1;
            }
        }
        (sum + n / 2) / n.max(1)
    };
    match frame.format {
        PixelFormat::Rgba | PixelFormat::Bgra => {
            for (pixel, &a) in data.as_chunks_mut::<4>().0.iter_mut().zip(&alpha.data) {
                pixel.iter_mut().take(3).for_each(|v| *v = divide(*v, a as u32));
            }
        }
        PixelFormat::I420 | PixelFormat::Nv12 => {
            let (cw, chh) = (w.div_ceil(2), h.div_ceil(2));
            for (v, &a) in data[..w * h].iter_mut().zip(&alpha.data) {
                *v = divide(*v, a as u32);
            }
            for cy in 0..chh {
                for cx in 0..cw {
                    let a = block(cx, cy);
                    if frame.format == PixelFormat::Nv12 {
                        let at = w * h + (cy * cw + cx) * 2;
                        data[at] = divide(data[at], a);
                        data[at + 1] = divide(data[at + 1], a);
                    } else {
                        let at = w * h + cy * cw + cx;
                        data[at] = divide(data[at], a);
                        data[at + cw * chh] = divide(data[at + cw * chh], a);
                    }
                }
            }
        }
        other => debug_assert!(false, "no cut-out for {other:?}"),
    }
    out
}

/// A plane's colour values times alpha (one alpha a pixel), the fourth byte of a packed pixel left alone.
fn multiply(plane: &mut [u8], alpha: &[u8], ch: usize) {
    for (pixel, &a) in plane.chunks_exact_mut(ch).zip(alpha) {
        for v in pixel.iter_mut().take(3) {
            *v = div255(*v as u32 * a as u32) as u8;
        }
    }
}

/// The alpha of each colour sample of a box in I420: the mean of its 2 x 2 luma pixels that are inside the box.
fn chroma_alpha(luma: &[u8], bx: (usize, usize, usize, usize)) -> Vec<u8> {
    let (x, y, w, h) = bx;
    let (cx0, cy0, cx1, cy1) = plane_box(2, bx);
    let mut out = spare_bytes((cx1 - cx0) * (cy1 - cy0));
    for cy in cy0..cy1 {
        for cx in cx0..cx1 {
            let (mut sum, mut n) = (0u32, 0u32);
            for ly in 2 * cy..2 * cy + 2 {
                for lx in 2 * cx..2 * cx + 2 {
                    if lx >= x && lx < x + w && ly >= y && ly < y + h {
                        sum += luma[(ly - y) * w + lx - x] as u32;
                        n += 1;
                    }
                }
            }
            out[(cy - cy0) * (cx1 - cx0) + cx - cx0] = (sum + n / 2).checked_div(n).map_or(0, |v| v as u8);
        }
    }
    out
}

/// composite.paste for a cut-out: the premultiplied picture over the canvas, out = picture * coverage + under * (1 - alpha * coverage), the corner curve's coverage mean over a colour sample's 2 x 2 pixels.
fn paste_over(canvas: &mut [u8], kind: Kind, cw: usize, chh: usize, picture: &[Vec<u8>], alpha: &[Vec<u8>], bx: (usize, usize, usize, usize), radius: usize) {
    let (x, y, w, h) = bx;
    let r = radius.min(w / 2).min(h / 2);
    let masks = if r > 0 { Some(corner_masks(r)) } else { None };
    let empty: CornerMasks = Default::default();
    let m = masks.as_deref().unwrap_or(&empty);
    for (p, plane) in planes(kind, cw, chh).into_iter().enumerate() {
        let full = plane.w == cw;
        let (px0, py0, px1, py1) = if full { (x, y, x + w, y + h) } else { plane_box(2, bx) };
        let (bw, ch) = (px1 - px0, plane.ch);
        let a_plane = &alpha[p.min(1).min(alpha.len() - 1)];
        let data = &mut canvas[plane.at..plane.at + plane.w * plane.h * ch];
        let stride = plane.w * ch;
        over_rows(&mut data[py0 * stride..py1 * stride], stride, |row, line| {
            let cy = py0 + row;
            let (solid0, solid1) = if full {
                if cy >= y + r && cy < y + h - r { (px0, px1) } else { (x + r, x + w - r) }
            } else if 2 * cy < y || 2 * cy + 1 >= y + h {
                (px0, px0)
            } else if 2 * cy >= y + r && 2 * cy + 1 < y + h - r {
                (x.div_ceil(2), (x + w) / 2)
            } else {
                ((x + r).div_ceil(2), (x + w - r) / 2)
            };
            let first = row * bw;
            let alphas = &a_plane[first..first + bw];
            let source = &picture[p][first * ch..(first + bw) * ch];
            let mut cx = px0;
            while cx < px1 {
                let a = alphas[cx - px0];
                if a == 0 {
                    let start = cx;
                    // Non-zero premultiplied colour at zero alpha must still follow the original blend.
                    while cx < px1 && alphas[cx - px0] == 0
                        && source[(cx - px0) * ch..(cx - px0) * ch + ch.min(3)].iter().all(|&v| v == 0)
                    {
                        cx += 1;
                    }
                    if cx > start {
                        continue;
                    }
                } else if a == 255 && cx >= solid0 && cx < solid1 {
                    let start = cx;
                    while cx < solid1 && alphas[cx - px0] == 255 {
                        cx += 1;
                    }
                    let from = &source[(start - px0) * ch..(cx - px0) * ch];
                    let to = &mut line[start * ch..cx * ch];
                    if ch == 1 {
                        to.copy_from_slice(from);
                    } else {
                        // Packed pixels keep the canvas's fourth byte, as the scalar blend does.
                        for (to, from) in to.chunks_exact_mut(ch).zip(from.chunks_exact(ch)) {
                            to[..3].copy_from_slice(&from[..3]);
                        }
                    }
                    continue;
                }
                // only a corner square has a curve to look up; a luma pixel or colour sample clear of both bands of one is fully covered
                let cov = if full {
                    if (cy >= y + r && cy < y + h - r) || (cx >= x + r && cx < x + w - r) { 1.0 } else { coverage(cx, cy, bx, r, m) }
                } else if (2 * cy >= y && 2 * cy + 1 < y + h) && (2 * cx >= x && 2 * cx + 1 < x + w)
                    && ((2 * cy >= y + r && 2 * cy + 1 < y + h - r) || (2 * cx >= x + r && 2 * cx + 1 < x + w - r))
                {
                    1.0
                } else {
                    let mut sum = 0f32;
                    let mut n = 0f32;
                    for ly in (2 * cy..2 * cy + 2).filter(|&v| v < chh) {
                        for lx in (2 * cx..2 * cx + 2).filter(|&v| v < cw) {
                            sum += coverage(lx, ly, bx, r, m);
                            n += 1.0;
                        }
                    }
                    if n > 0.0 { sum / n } else { 0.0 }
                };
                if cov <= 0.0 {
                    cx += 1;
                    continue;
                }
                let i = cx - px0;
                let at = cx * ch;
                for c in 0..ch.min(3) {
                    let (pm, under) = (source[i * ch + c], line[at + c]);
                    line[at + c] = if cov >= 1.0 {
                        (pm as u32 + div255(under as u32 * (255 - a as u32))).min(255) as u8
                    } else {
                        (pm as f32 * cov + under as f32 * (1.0 - a as f32 / 255.0 * cov)).round_ties_even().clamp(0.0, 255.0) as u8
                    };
                }
                cx += 1;
            }
        });
    }
}

#[cfg(test)]
fn paste_over_reference(canvas: &mut [u8], kind: Kind, cw: usize, chh: usize, picture: &[Vec<u8>], alpha: &[Vec<u8>], bx: (usize, usize, usize, usize), radius: usize) {
    let (x, y, w, h) = bx;
    let r = radius.min(w / 2).min(h / 2);
    let masks = if r > 0 { Some(corner_masks(r)) } else { None };
    let empty: CornerMasks = Default::default();
    let m = masks.as_deref().unwrap_or(&empty);
    for (p, plane) in planes(kind, cw, chh).into_iter().enumerate() {
        let full = plane.w == cw;
        let (px0, py0, px1, py1) = if full { (x, y, x + w, y + h) } else { plane_box(2, bx) };
        let (bw, ch) = (px1 - px0, plane.ch);
        let a_plane = &alpha[p.min(1).min(alpha.len() - 1)];
        let data = &mut canvas[plane.at..plane.at + plane.w * plane.h * ch];
        for cy in py0..py1 {
            for cx in px0..px1 {
                // only a corner square has a curve to look up; a luma pixel or colour sample clear of both bands of one is fully covered
                let cov = if full {
                    if (cy >= y + r && cy < y + h - r) || (cx >= x + r && cx < x + w - r) { 1.0 } else { coverage(cx, cy, bx, r, m) }
                } else if (2 * cy >= y && 2 * cy + 1 < y + h) && (2 * cx >= x && 2 * cx + 1 < x + w)
                    && ((2 * cy >= y + r && 2 * cy + 1 < y + h - r) || (2 * cx >= x + r && 2 * cx + 1 < x + w - r))
                {
                    1.0
                } else {
                    let mut sum = 0f32;
                    let mut n = 0f32;
                    for ly in (2 * cy..2 * cy + 2).filter(|&v| v < chh) {
                        for lx in (2 * cx..2 * cx + 2).filter(|&v| v < cw) {
                            sum += coverage(lx, ly, bx, r, m);
                            n += 1.0;
                        }
                    }
                    if n > 0.0 { sum / n } else { 0.0 }
                };
                if cov <= 0.0 {
                    continue;
                }
                let i = (cy - py0) * bw + (cx - px0);
                let a = a_plane[i];
                let at = (cy * plane.w + cx) * ch;
                for c in 0..ch.min(3) {
                    let (pm, under) = (picture[p][i * ch + c], data[at + c]);
                    data[at + c] = if cov >= 1.0 {
                        (pm as u32 + div255(under as u32 * (255 - a as u32))).min(255) as u8
                    } else {
                        (pm as f32 * cov + under as f32 * (1.0 - a as f32 / 255.0 * cov)).round_ties_even().clamp(0.0, 255.0) as u8
                    };
                }
            }
        }
    }
}

// ── Fading and mixing ─────────────────────────────────────────────────────

/// The rows of every plane inside box, to put back (or mix with) after something is drawn over them.
fn save_region(canvas: &[u8], kind: Kind, cw: usize, chh: usize, bx: (usize, usize, usize, usize)) -> Vec<Vec<u8>> {
    planes(kind, cw, chh).into_iter().map(|plane| {
        let (x0, y0, x1, y1) = if plane.w == cw { (bx.0, bx.1, bx.0 + bx.2, bx.1 + bx.3) } else { plane_box(2, bx) };
        let (x1, y1) = (x1.min(plane.w), y1.min(plane.h));
        let mut rows = Vec::new();
        for y in y0..y1 {
            let start = plane.at + (y * plane.w + x0) * plane.ch;
            rows.extend_from_slice(&canvas[start..start + (x1 - x0) * plane.ch]);
        }
        rows
    }).collect()
}

/// What save_region saved and what is drawn over it now, mixed by `alpha` (the drawn share).
fn restore_mix(canvas: &mut [u8], kind: Kind, cw: usize, chh: usize, bx: (usize, usize, usize, usize), before: &[Vec<u8>], alpha: f64) {
    let a = (alpha.clamp(0.0, 1.0) * 256.0).round() as u32;
    for (plane, saved) in planes(kind, cw, chh).into_iter().zip(before) {
        let (x0, y0, x1, y1) = if plane.w == cw { (bx.0, bx.1, bx.0 + bx.2, bx.1 + bx.3) } else { plane_box(2, bx) };
        let (x1, y1) = (x1.min(plane.w), y1.min(plane.h));
        let row = (x1 - x0) * plane.ch;
        for (n, y) in (y0..y1).enumerate() {
            let start = plane.at + (y * plane.w + x0) * plane.ch;
            for (now, was) in canvas[start..start + row].iter_mut().zip(&saved[n * row..(n + 1) * row]) {
                *now = ((u32::from(*was) * (256 - a) + u32::from(*now) * a + 128) >> 8) as u8;
            }
        }
    }
}

/// Two whole pictures mixed the way `how` says, `p` (0 to 1) of the way from `a` to `b`, into `out`. Each plane is mixed at its own size.
/// A crossfade is one pass of byte mixes; what only ever reads straight across (a slide, a push, a wipe) copies runs of pixels from
/// one picture or the other and mixes only the band between; a zoom reads each pixel from where its column and row say.
fn mix_pictures(out: &mut Vec<u8>, cols: &mut Vec<(usize, usize, u32)>, kind: Kind, (cw, chh): (usize, usize), a: &[u8], b: &[u8], how: transition::Kind, p: f64) {
    out.clear();
    out.resize(a.len(), 0);
    use transition::Kind as K;
    if !matches!(how, K::Slide | K::Push | K::Wipe | K::Zoom | K::Blinds | K::Curtain | K::Pixelate | K::Iris | K::Dissolve) {
        let m = (p.clamp(0.0, 1.0) * 256.0).round() as u32;
        for ((o, x), y) in out.iter_mut().zip(a).zip(b) {
            *o = ((u32::from(*x) * (256 - m) + u32::from(*y) * m + 128) >> 8) as u8;
        }
        return;
    }
    let index = |c: f64, n: usize| ((c * n as f64).floor().max(0.0) as usize).min(n - 1);
    let straight = !matches!(how, K::Zoom | K::Pixelate);
    for plane in planes(kind, cw, chh) {
        if matches!(how, K::Iris | K::Dissolve) {
            mix_regions(out, &plane, a, b, how, p);
            continue;
        }
        let aspect = cw as f64 / chh as f64;
        cols.clear();
        cols.extend((0..plane.w).map(|x| {
            let cell = transition::cell(how, false, (x as f64 + 0.5) / plane.w as f64, p, aspect);
            (index(cell.a, plane.w), index(cell.b, plane.w), (cell.mix * 256.0).round() as u32)
        }));
        for y in 0..plane.h {
            let row = transition::cell(how, true, (y as f64 + 0.5) / plane.h as f64, p, aspect);
            let (ay, by) = (index(row.a, plane.h), index(row.b, plane.h));
            let rm = (row.mix * 256.0).round() as u32;
            let line = plane.at + y * plane.w * plane.ch;
            if straight {
                // runs of columns that read one picture at a steady offset
                let mut x = 0;
                while x < plane.w {
                    let (ax, bx, m) = cols[x];
                    let m = (m * rm + 128) >> 8;
                    let mut end = x + 1;
                    if m == 0 || m >= 256 {
                        let off = if m == 0 { ax as isize - x as isize } else { bx as isize - x as isize };
                        while end < plane.w {
                            let (nax, nbx, nm) = cols[end];
                            let nm = (nm * rm + 128) >> 8;
                            let same = (m == 0 && nm == 0 && nax as isize - end as isize == off) || (m >= 256 && nm >= 256 && nbx as isize - end as isize == off);
                            if !same {
                                break;
                            }
                            end += 1;
                        }
                        let src = if m == 0 { a } else { b };
                        let from = (line as isize + (x as isize + off) * plane.ch as isize) as usize;
                        out[line + x * plane.ch..line + end * plane.ch].copy_from_slice(&src[from..from + (end - x) * plane.ch]);
                    } else {
                        for c in 0..plane.ch {
                            let (va, vb) = (u32::from(a[line + ax * plane.ch + c]), u32::from(b[line + bx * plane.ch + c]));
                            out[line + x * plane.ch + c] = ((va * (256 - m) + vb * m + 128) >> 8) as u8;
                        }
                    }
                    x = end;
                }
                continue;
            }
            for (x, &(ax, bx, cm)) in cols.iter().enumerate() {
                let mix = (cm * rm + 128) >> 8;
                let at = line + x * plane.ch;
                let from_a = plane.at + (ay * plane.w + ax) * plane.ch;
                let from_b = plane.at + (by * plane.w + bx) * plane.ch;
                for c in 0..plane.ch {
                    let (va, vb) = (u32::from(a[from_a + c]), u32::from(b[from_b + c]));
                    out[at + c] = if mix == 0 { va as u8 } else if mix >= 256 { vb as u8 } else { ((va * (256 - mix) + vb * mix + 128) >> 8) as u8 };
                }
            }
        }
    }
}

/// The Iris and the Dissolve: each pixel is the new picture's, the old one's or a mix by where it is. What is wholly one or the other is
/// copied in runs and only the soft edge is mixed pixel by pixel: the Iris finds where along a row its circle's edges are, and the Dissolve
/// has one mix for each square.
fn mix_regions(out: &mut [u8], plane: &Plane, a: &[u8], b: &[u8], how: transition::Kind, p: f64) {
    let size = (plane.w as f64, plane.h as f64);
    let ch = plane.ch;
    let copy = |out: &mut [u8], line: usize, from: usize, to: usize, src: &[u8]| {
        if to > from {
            out[line + from * ch..line + to * ch].copy_from_slice(&src[line + from * ch..line + to * ch]);
        }
    };
    let lerp = |out: &mut [u8], line: usize, from: usize, to: usize, m: u32| {
        for at in line + from * ch..line + to * ch {
            out[at] = ((u32::from(a[at]) * (256 - m) + u32::from(b[at]) * m + 128) >> 8) as u8;
        }
    };
    let (w, h) = size;
    for y in 0..plane.h {
        let line = plane.at + y * plane.w * ch;
        if how == transition::Kind::Dissolve {
            let square = (w / 40.0).max(1.0);
            let mut x = 0usize;
            while x < plane.w {
                let end = (((x as f64 / square).floor() + 1.0) * square).ceil().min(w) as usize;
                let end = end.max(x + 1);
                let m = (transition::mix_at(how, x as f64, y as f64, size, p) * 256.0).round() as u32;
                match m {
                    0 => copy(out, line, x, end, a),
                    256.. => copy(out, line, x, end, b),
                    _ => lerp(out, line, x, end, m),
                }
                x = end;
            }
            continue;
        }
        // Iris: pixels inside the circle's solid part are the new picture's, outside it the old one's, and the soft edge lies between
        let reach = (w * w + h * h).sqrt() / 2.0;
        let band = 0.18 * reach;
        let radius = p * (reach + band);
        let dy = y as f64 + 0.5 - h / 2.0;
        let half = |r: f64| if r > 0.0 && dy.abs() < r { (r * r - dy * dy).sqrt() } else { -1.0 };
        let (outer, inner) = (half(radius), half(radius - band));
        let (cx, wide) = (w / 2.0, plane.w);
        let span = |half: f64| if half < 0.0 { None } else { Some(((cx - half - 1.0).floor().max(0.0) as usize, ((cx + half + 1.0).ceil() as usize).min(wide))) };
        let Some((lo_o, hi_o)) = span(outer) else {
            copy(out, line, 0, wide, a);
            continue;
        };
        // a margin of a pixel each side of the solid part, so rounding never copies a mixed pixel
        let (lo_i, hi_i) = match span(inner) {
            Some((lo, hi)) if hi > lo + 4 => (lo + 2, hi - 2),
            _ => ((lo_o + hi_o) / 2, (lo_o + hi_o) / 2),
        };
        copy(out, line, 0, lo_o, a);
        copy(out, line, hi_o, wide, a);
        copy(out, line, lo_i, hi_i, b);
        for (from, to) in [(lo_o, lo_i), (hi_i, hi_o)] {
            for x in from..to {
                let m = (transition::mix_at(how, x as f64, y as f64, size, p) * 256.0).round() as u32;
                lerp(out, line, x, x + 1, m);
            }
        }
    }
}

// ── The plan and the drawing ──────────────────────────────────────────────

/// Where a tile goes in pixels: its box, border, corner radius and the radius inside the border.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placed {
    bx: (i64, i64, i64, i64),
    border: i64,
    radius: i64,
    inner: i64,
}

/// Where a tile goes in a width x height picture.
fn place(tile: &Tile, width: usize, height: usize) -> Placed {
    let (x0, y0, x1, y1) = box_px(tile, width as i64, height as i64);
    let border = scaled_size(tile.border, height as i64).min((x1 - x0) / 3).min((y1 - y0) / 3);
    let radius = scaled_size(tile.radius, height as i64);
    Placed { bx: (x0, y0, x1 - x0, y1 - y0), border, radius, inner: 0.max(radius - border) }
}

/// A layout worked out at one size; swapped in whole, so a render never sees half of a change.
struct Plan {
    layout: Layout,
    size: (usize, usize),
    background: Arc<Rgb>,
    boxes: Vec<Placed>,
}

/// Where a transition leaves from. Mostly the plan that was shown; when one is interrupted by another it is the picture as it was
/// in the middle of the first (so it never jumps): the tiles where they were for the ones that glide, the old transition frozen at
/// that moment for the others.
#[derive(Clone)]
enum Start {
    Plan(Arc<Plan>),
    /// The tiles where they were (some of them part faded), over this plan's background, which is part way to `over`'s when there is one
    Tiles { plan: Arc<Plan>, tiles: Vec<transition::Posed>, over: Option<(Arc<Plan>, f64)> },
    /// The transition `active` towards `to`, frozen `t` into its length
    Frozen { active: Box<Active>, to: Arc<Plan>, t: f64 },
}

impl Start {
    /// The plan whose background it leaves from.
    fn plan(&self) -> Arc<Plan> {
        match self {
            Start::Plan(p) | Start::Tiles { plan: p, .. } => p.clone(),
            Start::Frozen { active, .. } => active.from.plan(),
        }
    }

    fn tiles(&self) -> Vec<transition::Posed> {
        match self {
            Start::Tiles { tiles, .. } => tiles.clone(),
            other => other.plan().layout.tiles.iter().map(|tile| transition::Posed { tile: tile.clone(), alpha: 1.0 }).collect(),
        }
    }

    /// How many transitions are frozen inside this one.
    fn depth(&self) -> usize {
        match self {
            Start::Frozen { active, .. } => 1 + active.from.depth(),
            _ => 0,
        }
    }
}

/// A transition under way: where it leaves from, how it goes and when it began (at the first picture drawn, so a slow first frame doesn't use it up).
#[derive(Clone)]
struct Active {
    from: Start,
    spec: Spec,
    start: Option<u64>,
}

impl Active {
    /// How far into its length it is at `now`, 0 to 1.
    fn at(&self, now: u64) -> f64 {
        let start = self.start.unwrap_or(now);
        (now.saturating_sub(start) as f64 / self.spec.length_ns().max(1) as f64).clamp(0.0, 1.0)
    }
}

/// One tile to draw: where, how much of it shows and which cached scaling it uses.
struct Item<'a> {
    tile: Cow<'a, Tile>,
    placed: Placed,
    alpha: f64,
    key: usize,
}

type ScaleKey = (u64, (i64, i64, i64, i64), usize, usize, usize, usize, Kind, bool);
type ScaledPlanes = Vec<Vec<u8>>;

/// A tile's scaled picture; with a cut-out its planes are premultiplied once here and `alpha` is the scaled plane (I420 adds the 2 x 2 mean the colour planes share).
#[derive(Clone)]
struct Scaled {
    planes: Arc<ScaledPlanes>,
    alpha: Option<Arc<ScaledPlanes>>,
}

impl Drop for Scaled {
    fn drop(&mut self) {
        if let Some(planes) = Arc::get_mut(&mut self.planes) {
            std::mem::take(planes).into_iter().for_each(keep_bytes);
        }
        if let Some(alpha) = self.alpha.as_mut().and_then(Arc::get_mut) {
            std::mem::take(alpha).into_iter().for_each(keep_bytes);
        }
    }
}
/// Where the last render's time went: scaling the streams, drawing them onto the canvas and getting the canvas ready.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderTimes {
    pub scale: Duration,
    pub paste: Duration,
    pub setup: Duration,
}

type BackgroundKey = (String, String, usize, usize);

/// composite.Compositor: prepare() on the host thread (it may read the background picture), render() on a stream's thread.
pub struct Compositor {
    plan: Option<Arc<Plan>>,
    /// Tile index: the scaled stream for a frame seen before.
    scaled: HashMap<usize, (ScaleKey, Scaled)>,
    reader: PictureReader,
    /// The picture as read, so moving a tile doesn't read it again.
    pictures: Option<(String, Arc<Rgb>)>,
    /// (picture, colour, size): the background drawn, so moving a tile reuses it.
    backgrounds: Option<(BackgroundKey, Arc<Rgb>)>,
    /// The background in the planes last drawn in.
    planes: Vec<(usize, Kind, Arc<Vec<u8>>)>,
    /// The sizes a background picture was scaled to, for the tests.
    pub covered: Vec<(usize, usize)>,
    transition: Option<Active>,
    /// Buffers of whole pictures to draw into again, so a transition's frames don't ask for new ones.
    spare: Vec<Vec<u8>>,
    plane_spares: Vec<Vec<u8>>,
    /// The columns of a mix, worked out again each plane of each picture into the same room.
    cols: Vec<(usize, usize, u32)>,
    /// The clock of the picture being drawn (ns).
    now: u64,
    /// Cut-out tiles (by source id) drawn without an alpha in this picture, and when each one first was.
    waiting: HashMap<String, u64>,
    /// The same as of the picture before, so a tile that has an alpha now or isn't cut out any more starts again next time.
    waiting_before: HashMap<String, u64>,
    times: RenderTimes,
}

/// How long a cut-out tile whose stream has no alpha stays blank before it is drawn plain (ns).
const MASK_GRACE_NS: u64 = 1_000_000_000;

impl Default for Compositor {
    fn default() -> Self {
        Compositor::new(no_pictures())
    }
}

fn colour_planes(kind: Kind, rgb: [u8; 3]) -> Vec<u8> {
    let (r, g, b) = (rgb[0] as f32, rgb[1] as f32, rgb[2] as f32);
    let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    match kind {
        Kind::I420 => vec![q(16.0 + 0.257 * r + 0.504 * g + 0.098 * b), q(128.0 - 0.148 * r - 0.291 * g + 0.439 * b), q(128.0 + 0.439 * r - 0.368 * g - 0.071 * b)],
        Kind::Rgba => vec![rgb[0], rgb[1], rgb[2], 255],
        Kind::Bgra => vec![rgb[2], rgb[1], rgb[0], 255],
    }
}

/// A frame in another kind's planes, through RGB; None for a format that isn't a picture.
fn converted(frame: &Frame, kind: Kind) -> Option<Frame> {
    if Kind::of(frame.format) == kind && frame.format == kind.format() {
        return Some(frame.clone());
    }
    if kind == Kind::I420 && frame.format == PixelFormat::Nv12 {
        return nv12_as_i420(frame);
    }
    let rgb = to_rgb(frame)?;
    let format = kind.format();
    // Packed pictures keep the alpha they had, so they start opaque; I420 is written whole and needs no fill
    let start = if kind == Kind::I420 { 0 } else { 255 };
    let mut out = Frame::new(frame.width, frame.height, format, vec![start; format.size(frame.width, frame.height)]);
    put_rgb(&mut out, &rgb);
    Some(out)
}

/// An unrounded plain tile into the canvas, when every touched chroma sample belongs wholly to the tile.
fn scale_plain_into(out: &mut [u8], kind: Kind, (cw, chh): (usize, usize), tile: &Tile, frame: &Frame, bx: (usize, usize, usize, usize)) {
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    if fw == 0 || fh == 0 {
        return;
    }
    let Some(source) = converted(frame, kind) else { return };
    let crop = crop_px(tile, fw as i64, fh as i64, bx.2 as i64, bx.3 as i64);
    let (x0, y0, x1, y1) = (crop.0 as usize, crop.1 as usize, crop.2 as usize, crop.3 as usize);
    for (plane, target) in planes(kind, fw, fh).into_iter().zip(planes(kind, cw, chh)) {
        let full = plane.w == fw;
        let (rx0, ry0, rx1, ry1) = if full {
            (x0, y0, x1, y1)
        } else {
            (x0 / 2, y0 / 2, x1.div_ceil(2).min(plane.w).max(x0 / 2 + 1), y1.div_ceil(2).min(plane.h).max(y0 / 2 + 1))
        };
        let (tx0, ty0, tx1, ty1) = if target.w == cw { (bx.0, bx.1, bx.0 + bx.2, bx.1 + bx.3) } else { plane_box(2, bx) };
        let data = &source.data[plane.at..plane.at + plane.w * plane.h * plane.ch];
        let region = Region { data, stride: plane.w, ch: plane.ch, x0: rx0, y0: ry0, w: rx1 - rx0, h: ry1 - ry0 };
        let stride = target.w * target.ch;
        let rows = &mut out[target.at + ty0 * stride..target.at + ty1 * stride];
        scale_region_into(region, tx1 - tx0, ty1 - ty0, rows, stride, tx0 * target.ch);
    }
}

/// NV12 as I420: the luma as it is and the interleaved colour pairs split into their two planes, with no trip through RGB.
fn nv12_as_i420(frame: &Frame) -> Option<Frame> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (luma, chroma) = (w * h, w.div_ceil(2) * h.div_ceil(2));
    if frame.data.len() < luma + 2 * chroma {
        return None;
    }
    let mut data = vec![0u8; luma + 2 * chroma];
    data[..luma].copy_from_slice(&frame.data[..luma]);
    let (u, v) = data[luma..].split_at_mut(chroma);
    for ((u, v), pair) in u.iter_mut().zip(v).zip(frame.data[luma..luma + 2 * chroma].as_chunks::<2>().0) {
        (*u, *v) = (pair[0], pair[1]);
    }
    Some(Frame::new(frame.width, frame.height, PixelFormat::I420, data))
}

impl Compositor {
    pub fn new(reader: PictureReader) -> Compositor {
        Compositor { plan: None, scaled: HashMap::new(), reader, pictures: None, backgrounds: None, planes: Vec::new(), covered: Vec::new(), transition: None, spare: Vec::new(), plane_spares: Vec::new(), cols: Vec::new(), now: 0, waiting: HashMap::new(), waiting_before: HashMap::new(), times: RenderTimes::default() }
    }

    pub fn prepare(&mut self, layout: &Layout, size: (u32, u32)) {
        let next = self.plan_of(layout, size);
        // a transition under way goes on to the new target (an edit, a stream that joined); a new size ends it
        if self.plan.as_ref().is_some_and(|p| p.size != next.size) {
            self.end_transition();
        }
        self.plan = Some(next);
    }

    /// prepare(), but the picture goes from what it shows now to `layout` the way `spec` says. The clock starts with the first picture
    /// drawn. It cuts when nothing is drawn yet, the picture's size changed, the arrangement is the same or the spec is a cut. One
    /// that interrupts another leaves from where that one is.
    pub fn prepare_with(&mut self, layout: &Layout, size: (u32, u32), spec: Spec, now: u64) {
        let aspect = size.0.max(1) as f64 / size.1.max(1) as f64;
        let next = self.plan_of(layout, size);
        let previous = self.plan.clone();
        let from = match (self.transition.take(), previous) {
            (Some(active), Some(prev)) if prev.size == next.size => {
                let t = active.at(now);
                if active.spec.kind.moves_tiles() && spec.kind.moves_tiles() {
                    let posed = transition::poses_from(active.spec.kind, &active.from.tiles(), &prev.layout.tiles, t, active.spec.ease, aspect);
                    // the background goes on from where it is too: part way from the old one to the one it was going to
                    let base = active.from.plan();
                    let over = (base.layout.background != prev.layout.background || base.layout.picture != prev.layout.picture)
                        .then(|| (prev.clone(), transition::effect(active.spec.ease, t)));
                    Some(Start::Tiles { plan: base, tiles: posed, over })
                } else if !spec.kind.moves_tiles() && active.from.depth() < 2 {
                    // a mix leaves from the picture the last one had reached, glide or mix, still live
                    Some(Start::Frozen { active: Box::new(active), to: prev, t })
                } else {
                    // a glide cannot leave from a picture mixed of two (and chains of mixes stop at two): it leaves from whichever end is nearer.
                    // This happens only when the kind was changed while one was playing
                    Some(Start::Plan(if t < 0.5 { active.from.plan() } else { prev }))
                }
            }
            (None, Some(prev)) if prev.size == next.size && !crate::composite::same_arrangement(&prev.layout, layout) => Some(Start::Plan(prev)),
            _ => None,
        };
        self.end_transition();
        self.transition = from.filter(|_| spec.kind != transition::Kind::Cut).map(|from| Active { from, spec, start: None });
        self.plan = Some(next);
    }

    fn end_transition(&mut self) {
        self.transition = None;
        // what the transition drew with its own keys is not wanted again
        self.scaled.retain(|k, _| *k < 1000);
    }

    /// Whether a transition is still going at `now` (ns).
    pub fn transitioning(&self, now: u64) -> bool {
        self.transition.as_ref().is_some_and(|a| a.start.is_none_or(|s| now.saturating_sub(s) < a.spec.length_ns()))
    }

    /// Whether there is a transition that has not been drawn to its end yet: the clock draws until it has.
    pub fn has_transition(&self) -> bool {
        self.transition.is_some()
    }

    /// The streams of the cut-out tiles a transition under way still draws from the picture it leaves, which the layout no longer has.
    pub fn leaving_cut_outs(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut from = self.transition.as_ref().map(|a| &a.from);
        while let Some(start) = from {
            let (tiles, next): (Vec<&Tile>, _) = match start {
                Start::Plan(plan) => (plan.layout.tiles.iter().collect(), None),
                Start::Tiles { tiles, .. } => (tiles.iter().map(|p| &p.tile).collect(), None),
                Start::Frozen { active, to, .. } => (to.layout.tiles.iter().collect(), Some(&active.from)),
            };
            out.extend(tiles.into_iter().filter(|t| t.cut_out).map(|t| t.source_id.clone()));
            from = next;
        }
        out
    }

    fn plan_of(&mut self, layout: &Layout, size: (u32, u32)) -> Arc<Plan> {
        let (width, height) = (size.0.max(1) as usize, size.1.max(1) as usize);
        let background = self.background(layout, width, height);
        let boxes = layout.tiles.iter().map(|tile| place(tile, width, height)).collect();
        Arc::new(Plan { layout: layout.clone(), size: (width, height), background, boxes })
    }

    /// The picture behind the tiles, worked out once per picture, colour and size.
    fn background(&mut self, layout: &Layout, width: usize, height: usize) -> Arc<Rgb> {
        let key = (layout.picture.clone(), layout.background.clone(), width, height);
        if let Some((k, bg)) = &self.backgrounds {
            if *k == key {
                return bg.clone();
            }
        }
        let mut picture = None;
        if !layout.picture.is_empty() {
            picture = self.pictures.as_ref().filter(|(p, _)| *p == layout.picture).map(|(_, rgb)| rgb.clone());
            if picture.is_none() {
                // one that's missing now may be there next time
                let read = (self.reader)(&layout.picture).ok().filter(|f| f.width > 0 && f.height > 0)
                    .and_then(|f| to_rgb(&f).map(|data| Rgb { w: f.width as usize, h: f.height as usize, data }));
                if let Some(rgb) = read {
                    let rgb = Arc::new(rgb);
                    self.pictures = Some((layout.picture.clone(), rgb.clone()));
                    picture = Some(rgb);
                }
            }
        }
        let background = Arc::new(match &picture {
            Some(p) => {
                self.covered.push((width, height));
                cover(p, width, height)
            }
            None => {
                let [b, g, r] = bgr(&layout.background);
                Rgb::filled(width, height, [r, g, b])
            }
        });
        // one at a time: a new size or colour replaces it
        if picture.is_some() || layout.picture.is_empty() {
            self.backgrounds = Some((key, background.clone()));
        }
        background
    }

    pub fn layout(&self) -> Option<&Layout> {
        self.plan.as_ref().map(|p| &p.layout)
    }

    /// How many of the layout's tiles frames has a picture for.
    pub fn live_tiles<T>(&self, frames: &BTreeMap<String, T>) -> usize {
        self.plan.as_ref().map_or(0, |p| p.layout.tiles.iter().filter(|t| frames.contains_key(&t.source_id)).count())
    }

    /// The scaled picture kept for the tile at index, for the tests.
    #[cfg(test)]
    pub(crate) fn scaled_of(&self, index: usize) -> Option<Arc<Vec<Vec<u8>>>> {
        self.scaled.get(&index).map(|(_, s)| s.planes.clone())
    }

    fn background_planes(&mut self, plan: &Plan, kind: Kind) -> Arc<Vec<u8>> {
        let id = Arc::as_ptr(&plan.background) as usize;
        if let Some((_, _, data)) = self.planes.iter().find(|(i, k, _)| *i == id && *k == kind) {
            return data.clone();
        }
        let (w, h) = plan.size;
        let format = kind.format();
        let mut frame = Frame::new(w as u32, h as u32, format, vec![255; format.size(w as u32, h as u32)]);
        put_rgb(&mut frame, &plan.background.data);
        let data = frame.data.clone();
        // the picture's and the one it leaves, no more
        self.planes.truncate(1);
        self.planes.insert(0, (id, kind, data.clone()));
        data
    }

    /// The picture, from frames; tiles without a frame are left out. In format's planes (I420 for one that isn't RGBA or
    /// BGRA); None before prepare().
    pub fn render(&mut self, frames: &Frames, format: PixelFormat) -> Option<Frame> {
        self.render_at(frames, format, monotonic_ns())
    }

    /// render() as it is at `now` (ns): partway through a transition, if one is going.
    pub fn render_at(&mut self, frames: &Frames, format: PixelFormat, now: u64) -> Option<Frame> {
        self.now = now;
        self.waiting.clear();
        let mut spares = std::mem::take(&mut self.plane_spares);
        let out = with_spares(&mut spares, || self.render_picture(frames, format, now));
        self.plane_spares = spares;
        std::mem::swap(&mut self.waiting, &mut self.waiting_before);
        out
    }

    fn render_picture(&mut self, frames: &Frames, format: PixelFormat, now: u64) -> Option<Frame> {
        self.times = RenderTimes::default();
        let plan = self.plan.clone()?;
        let kind = Kind::of(format);
        if let Some(active) = self.transition.as_mut() {
            // the clock starts with the first picture, so a slow first frame doesn't use up a short transition
            active.start.get_or_insert(now);
        }
        if let Some(active) = self.transition.clone() {
            let t = active.at(now);
            if t >= 1.0 {
                self.end_transition();
            } else {
                let out = self.in_transition(&plan, &active, t, frames, kind, 0);
                return Some(Frame::new(plan.size.0 as u32, plan.size.1 as u32, kind.format(), out));
            }
        }
        let out = self.draw_plan(&plan, frames, kind, 0);
        Some(Frame::new(plan.size.0 as u32, plan.size.1 as u32, kind.format(), out))
    }

    /// How the last render's time split up.
    pub fn last_times(&self) -> RenderTimes {
        self.times
    }

    /// A buffer holding a copy of `from`, one given back before if there is one.
    fn copy_of(&mut self, from: &[u8]) -> Vec<u8> {
        let mut buf = self.spare.pop().unwrap_or_default();
        buf.clear();
        buf.extend_from_slice(from);
        buf
    }

    fn give_back(&mut self, buf: Vec<u8>) {
        if self.spare.len() < 4 {
            self.spare.push(buf);
        }
    }

    /// The whole of a plan drawn as it is: the background, then every tile with a frame.
    fn draw_plan(&mut self, plan: &Plan, frames: &Frames, kind: Kind, key_base: usize) -> Vec<u8> {
        let started = Instant::now();
        let background = self.background_planes(plan, kind);
        let mut out = self.copy_of(&background);
        self.times.setup += started.elapsed();
        let items: Vec<Item> = plan.layout.tiles.iter().zip(&plan.boxes).enumerate()
            .map(|(i, (tile, placed))| Item { tile: Cow::Borrowed(tile), placed: *placed, alpha: 1.0, key: key_base + i }).collect();
        self.draw_items(&mut out, kind, plan.size, &items, frames);
        out
    }

    /// The picture `t` (0 to 1 of the length) into a transition from `active.from` to `plan`; `base` keeps the scaled pictures of a
    /// transition inside another apart from this one's.
    fn in_transition(&mut self, plan: &Plan, active: &Active, t: f64, frames: &Frames, kind: Kind, base: usize) -> Vec<u8> {
        let (cw, chh) = plan.size;
        let spec = active.spec;
        let from_plan = active.from.plan();
        if spec.kind.moves_tiles() {
            let posed = transition::poses_from(spec.kind, &active.from.tiles(), &plan.layout.tiles, t, spec.ease, cw as f64 / chh as f64);
            let started = Instant::now();
            let background = self.background_planes(plan, kind);
            let mut out = self.copy_of(&background);
            self.times.setup += started.elapsed();
            // a different background fades from one to the other (the one an interrupted transition left from is part way already)
            let over = match &active.from {
                Start::Tiles { over: Some((to, w)), .. } => Some((to.clone(), *w)),
                _ => None,
            };
            if over.is_some() || from_plan.layout.background != plan.layout.background || from_plan.layout.picture != plan.layout.picture {
                let mut before = self.background_planes(&from_plan, kind);
                if let Some((other, w)) = over {
                    let to = self.background_planes(&other, kind);
                    let w = (w * 256.0).round() as u32;
                    before = Arc::new(before.iter().zip(to.iter()).map(|(b, o)| ((u32::from(*b) * (256 - w) + u32::from(*o) * w + 128) >> 8) as u8).collect());
                }
                let fade = (transition::effect(spec.ease, t) * 256.0).round() as u32;
                out.iter_mut().zip(before.iter()).for_each(|(o, b)| *o = ((u32::from(*b) * (256 - fade) + u32::from(*o) * fade + 128) >> 8) as u8);
            }
            let items: Vec<Item> = posed.iter().enumerate().map(|(i, p)| Item { tile: Cow::Borrowed(&p.tile), placed: place(&p.tile, cw, chh), alpha: p.alpha, key: base + 2000 + i }).collect();
            self.draw_items(&mut out, kind, plan.size, &items, frames);
            return out;
        }
        let a = match &active.from {
            Start::Frozen { active: inner, to, t } => self.in_transition(to, inner, *t, frames, kind, base + 4000),
            _ => self.draw_plan(&from_plan, frames, kind, base + 1000),
        };
        let b = self.draw_plan(plan, frames, kind, base);
        let mut out = self.spare.pop().unwrap_or_default();
        let mut cols = std::mem::take(&mut self.cols);
        mix_pictures(&mut out, &mut cols, kind, plan.size, &a, &b, spec.kind, transition::effect(spec.ease, t));
        self.cols = cols;
        self.give_back(a);
        self.give_back(b);
        out
    }

    /// Draws items back to front on out: each one's border, then its stream, fading in over what is under it by `alpha`.
    fn draw_items(&mut self, out: &mut [u8], kind: Kind, (cw, chh): (usize, usize), items: &[Item], frames: &Frames) {
        for item in items {
            let (tile, placed) = (item.tile.as_ref(), item.placed);
            let Some((frame, number)) = frames.get(&tile.source_id) else { continue };
            // a cut-out has nothing to show until its stream's alpha arrives, as the whole frame would flash the room; if none comes it is drawn plain
            if tile.cut_out && frame.alpha().is_none() {
                let since = *self.waiting.get(&tile.source_id).or(self.waiting_before.get(&tile.source_id)).unwrap_or(&self.now);
                self.waiting.insert(tile.source_id.clone(), since);
                if self.now.saturating_sub(since) < MASK_GRACE_NS {
                    continue;
                }
            }
            let (mut x, mut y, mut w, mut h) = placed.bx;
            let under = (item.alpha < 1.0).then(|| save_region(out, kind, cw, chh, (x as usize, y as usize, w as usize, h as usize)));
            if placed.border > 0 {
                // a cut-out has no box for a border to fill, so it keeps the room the border takes and leaves it empty
                if !tile.cut_out {
                    let colour = colour_planes(kind, { let [b, g, r] = bgr(&tile.border_colour); [r, g, b] });
                    let started = Instant::now();
                    paste(out, kind, cw, chh, Content::Colour(colour), (x as usize, y as usize, w as usize, h as usize), placed.radius as usize);
                    self.times.paste += started.elapsed();
                }
                (x, y, w, h) = (x + placed.border, y + placed.border, w - 2 * placed.border, h - 2 * placed.border);
            }
            if w > 0 && h > 0 {
                let bx = (x as usize, y as usize, w as usize, h as usize);
                let radius = if placed.border > 0 { placed.inner } else { placed.radius };
                let whole_samples = kind != Kind::I420 || (x % 2 == 0 && y % 2 == 0
                    && ((x + w) % 2 == 0 || x + w == cw as i64) && ((y + h) % 2 == 0 || y + h == chh as i64));
                let started = Instant::now();
                // Numbered streams keep their scaled tiles so a slower stream does not get scaled on every picture.
                let direct = number.is_none() && !tile.cut_out && radius == 0 && whole_samples;
                let scaled = if direct {
                    self.scaled.remove(&item.key);
                    scale_plain_into(out, kind, (cw, chh), tile, frame, bx);
                    None
                } else {
                    self.scale(item.key, tile, frame, *number, bx, kind)
                };
                self.times.scale += started.elapsed();
                if let Some(scaled) = scaled {
                    let started = Instant::now();
                    match &scaled.alpha {
                        Some(alpha) => paste_over(out, kind, cw, chh, &scaled.planes, alpha, bx, radius as usize),
                        None => paste(out, kind, cw, chh, Content::Picture(&scaled.planes), bx, radius as usize),
                    }
                    self.times.paste += started.elapsed();
                }
            }
            if let Some(under) = under {
                restore_mix(out, kind, cw, chh, (placed.bx.0 as usize, placed.bx.1 as usize, placed.bx.2 as usize, placed.bx.3 as usize), &under, item.alpha);
            }
        }
    }

    /// The tile's crop of frame scaled to its box, plane by plane; kept for a frame that can come again, with its alpha plane and premultiplied colour for a cut-out.
    fn scale(&mut self, index: usize, tile: &Tile, frame: &Frame, number: Option<u64>, bx: (usize, usize, usize, usize), kind: Kind) -> Option<Scaled> {
        let (fw, fh) = (frame.width as i64, frame.height as i64);
        if fw == 0 || fh == 0 {
            return None;
        }
        let (_, _, w, h) = bx;
        let crop = crop_px(tile, fw, fh, w as i64, h as i64);
        let (cx0, cy0, cx1, cy1) = plane_box(2, bx);
        let cut = if tile.cut_out { frame.alpha().cloned() } else { None };
        let key = (number.unwrap_or(0), crop, w, h, cx1 - cx0, cy1 - cy0, kind, cut.is_some());
        if let (Some(_), Some((k, cached))) = (number, self.scaled.get(&index)) {
            if *k == key {
                // a slower stream's frame that hasn't changed since the last picture
                return Some(cached.clone());
            }
        }
        self.scaled.remove(&index);
        // premultiplied planes only stay so between frames of the same colour model: through RGB they are made straight first
        let mut premultiplied = cut.as_ref().is_some_and(|a| a.premultiplied);
        let straight = match &cut {
            Some(a) if premultiplied && matches!(frame.format, PixelFormat::I420 | PixelFormat::Nv12) != (kind == Kind::I420) => {
                premultiplied = false;
                Some(unpremultiplied(frame, a))
            }
            _ => None,
        };
        let source = converted(straight.as_ref().unwrap_or(frame), kind)?;
        let (x0, y0, x1, y1) = (crop.0 as usize, crop.1 as usize, crop.2 as usize, crop.3 as usize);
        let mut out = Vec::new();
        for plane in planes(kind, fw as usize, fh as usize) {
            let data = &source.data[plane.at..plane.at + plane.w * plane.h * plane.ch];
            let full = plane.w == fw as usize;
            let (rx0, ry0, rx1, ry1) = if full {
                (x0, y0, x1, y1)
            } else {
                (x0 / 2, y0 / 2, x1.div_ceil(2).min(plane.w).max(x0 / 2 + 1), y1.div_ceil(2).min(plane.h).max(y0 / 2 + 1))
            };
            let (tw, th) = if full { (w, h) } else { (cx1 - cx0, cy1 - cy0) };
            let region = Region { data, stride: plane.w, ch: plane.ch, x0: rx0, y0: ry0, w: rx1 - rx0, h: ry1 - ry0 };
            out.push(scale_region(region, tw, th));
        }
        let alpha = cut.map(|a| {
            let region = Region { data: &a.data, stride: a.w as usize, ch: 1, x0, y0, w: x1 - x0, h: y1 - y0 };
            let luma = scale_region(region, w, h);
            let mut alpha = vec![luma];
            if kind == Kind::I420 {
                alpha.push(chroma_alpha(&alpha[0], bx));
            }
            if !premultiplied {
                for (p, plane) in out.iter_mut().enumerate() {
                    multiply(plane, &alpha[p.min(1)], if kind == Kind::I420 { 1 } else { 4 });
                }
            }
            Arc::new(alpha)
        });
        let scaled = Scaled { planes: Arc::new(out), alpha };
        self.scaled.insert(index, (key, scaled.clone()));
        Some(scaled)
    }
}

#[cfg(test)]
#[path = "compositor_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "compositor_speed_tests.rs"]
mod speed_tests;
