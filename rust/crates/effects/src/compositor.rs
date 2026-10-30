//! Drawing a layout (composite.Compositor): the background, then each stream cropped, scaled and pasted into its box back to
//! front, with its border and rounded corners. Python draws in BGR with OpenCV; this draws straight into the frame's own
//! planes (I420 as the decoder makes it, or RGBA and BGRA), so nothing is converted on the way. Scaling follows OpenCV's
//! (exact halves by INTER_AREA, then INTER_LINEAR's fixed-point taps) and the corners blend as cv2.blendLinear does, so a
//! packed picture comes out as Python's does; in I420 the colour planes are half size, and a box edge on an odd pixel shares
//! its colour sample with what's beside it.

use crate::composite::{bgr, box_px, crop_px, scaled_size, Layout, Tile};
use crate::cpu::Rgb;
use crate::imgops::resize_area;
use crate::pixels::{put_rgb, to_rgb};
use crate::shared::{no_pictures, PictureReader};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use telescope_core::frame::{Frame, PixelFormat};

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
    fn at(&self, x: usize, y: usize, c: usize) -> u8 {
        self.data[((self.y0 + y) * self.stride + self.x0 + x) * self.ch + c]
    }

    fn copy(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.w * self.h * self.ch);
        for y in 0..self.h {
            let start = ((self.y0 + y) * self.stride + self.x0) * self.ch;
            out.extend_from_slice(&self.data[start..start + self.w * self.ch]);
        }
        out
    }

    /// cv2.resize to exactly half (INTER_AREA): each pixel the rounded mean of a 2 x 2 block, an odd last row or column left out.
    fn halved(&self) -> Vec<u8> {
        let (w2, h2, ch) = (self.w / 2, self.h / 2, self.ch);
        let mut out = vec![0u8; w2 * h2 * ch];
        for y in 0..h2 {
            let r0 = ((self.y0 + 2 * y) * self.stride + self.x0) * ch;
            let r1 = r0 + self.stride * ch;
            let row = &mut out[y * w2 * ch..(y + 1) * w2 * ch];
            for x in 0..w2 {
                for c in 0..ch {
                    let (a, b) = (r0 + 2 * x * ch + c, r1 + 2 * x * ch + c);
                    let sum = self.data[a] as u32 + self.data[a + ch] as u32 + self.data[b] as u32 + self.data[b + ch] as u32;
                    row[x * ch + c] = ((sum + 2) >> 2) as u8;
                }
            }
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

/// cv2.resize(INTER_LINEAR) of an 8-bit region: whole-number taps across, then down with OpenCV's SSE rounding for each
/// stretch of 16 and 8 values and its plain one for what's left of a row.
fn linear(src: Region, w2: usize, h2: usize) -> Vec<u8> {
    let ch = src.ch;
    let (xs, ys) = (linear_taps(src.w, w2), linear_taps(src.h, h2));
    let n = w2 * ch;
    let row = |y: usize| -> Vec<i32> {
        let mut out = vec![0i32; n];
        for (dx, &(x0, x1, a0, a1)) in xs.iter().enumerate() {
            for c in 0..ch {
                out[dx * ch + c] = src.at(x0, y, c) as i32 * a0 + src.at(x1, y, c) as i32 * a1;
            }
        }
        out
    };
    let mut rows: HashMap<usize, Vec<i32>> = HashMap::new();
    let mut out = vec![0u8; n * h2];
    let vec16 = n / 16 * 16;
    let vec_end = if (vec16 as i64) < n as i64 - 8 { vec16 + 8 } else { vec16 };
    for (dy, &(y0, y1, b0, b1)) in ys.iter().enumerate() {
        rows.entry(y0).or_insert_with(|| row(y0));
        rows.entry(y1).or_insert_with(|| row(y1));
        rows.retain(|k, _| *k >= y0);
        let (s0, s1) = (&rows[&y0], &rows[&y1]);
        let line = &mut out[dy * n..(dy + 1) * n];
        for i in 0..n {
            line[i] = if i < vec_end {
                let hi = |b: i32, s: i32| (b * ((s >> 4).clamp(-32768, 32767))) >> 16;
                ((hi(b0, s0[i]) + hi(b1, s1[i]) + 2) >> 2).clamp(0, 255) as u8
            } else {
                ((b0 as i64 * s0[i] as i64 + b1 as i64 * s1[i] as i64 + (1 << 21)) >> 22).clamp(0, 255) as u8
            };
        }
    }
    out
}

/// composite.scale_to on a region: in exact halves while it's at least twice the size each way, then INTER_LINEAR the rest.
fn scale_region(src: Region, w: usize, h: usize) -> Vec<u8> {
    if (src.w, src.h) == (w, h) {
        return src.copy();
    }
    if src.w < 2 * w || src.h < 2 * h {
        return linear(src, w, h);
    }
    let mut image = src.halved();
    let (mut iw, mut ih) = (src.w / 2, src.h / 2);
    while iw >= 2 * w && ih >= 2 * h {
        let r = Region { data: &image, stride: iw, ch: src.ch, x0: 0, y0: 0, w: iw, h: ih };
        image = r.halved();
        (iw, ih) = (iw / 2, ih / 2);
    }
    if (iw, ih) == (w, h) {
        return image;
    }
    linear(Region { data: &image, stride: iw, ch: src.ch, x0: 0, y0: 0, w: iw, h: ih }, w, h)
}

/// composite.scale_to on a whole w x h picture of ch bytes a pixel.
pub fn scale_to(src: &[u8], w: usize, h: usize, ch: usize, w2: usize, h2: usize) -> Vec<u8> {
    scale_region(Region { data: src, stride: w, ch, x0: 0, y0: 0, w, h }, w2, h2)
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

/// For each corner (top left, top right, bottom left, bottom right): (inside, outside) weights of a radius x radius square,
/// smoothed a pixel wide at the curve.
pub type CornerMasks = [(Vec<f32>, Vec<f32>); 4];

/// composite.corner_masks: worked out once per radius.
pub fn corner_masks(radius: usize) -> Arc<CornerMasks> {
    static MASKS: OnceLock<Mutex<HashMap<usize, Arc<CornerMasks>>>> = OnceLock::new();
    let mut cache = MASKS.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = cache.get(&radius) {
        return m.clone();
    }
    if cache.len() >= 64 {
        cache.clear();
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
    let flip = |fx: bool, fy: bool| -> (Vec<f32>, Vec<f32>) {
        let a: Vec<f32> = (0..r * r).map(|i| {
            let (y, x) = (i / r, i % r);
            inside[(if fy { r - 1 - y } else { y }) * r + if fx { r - 1 - x } else { x }]
        }).collect();
        let b = a.iter().map(|v| 1.0 - v).collect();
        (a, b)
    };
    let masks = Arc::new([flip(false, false), flip(true, false), flip(false, true), flip(true, true)]);
    cache.insert(radius, masks.clone());
    masks
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
    corner.map_or(1.0, |(i, cx, cy)| masks[i].0[cy * r + cx])
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
                    let (inside, outside) = &m[corner];
                    for yy in 0..r {
                        for xx in 0..r * ch {
                            let at = ((cy0 + yy) * plane.w + cx0) * ch + xx;
                            let k = yy * r + xx / ch;
                            data[at] = blend(data[at], kept.next().unwrap_or(0), inside[k], outside[k]);
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
        for cy in py0..py1 {
            let rows: Vec<usize> = [2 * cy, 2 * cy + 1].into_iter().filter(|l| *l < chh).collect();
            let in_v = rows.iter().all(|&l| inside(l, y, y + h));
            let plain_v = rows.iter().all(|&l| inside(l, y + r, y + h - r));
            for cx in px0..px1 {
                let i = (cy - py0) * bw + (cx - px0);
                let at = cy * plane.w + cx;
                let cols: Vec<usize> = [2 * cx, 2 * cx + 1].into_iter().filter(|l| *l < cw).collect();
                let in_h = cols.iter().all(|&l| inside(l, x, x + w));
                let plain_h = cols.iter().all(|&l| inside(l, x + r, x + w - r));
                if in_v && in_h && (plain_v || plain_h) {
                    data[at] = source(i);
                    continue;
                }
                let n = (rows.len() * cols.len()) as f32;
                let sum: f32 = rows.iter().flat_map(|&ly| cols.iter().map(move |&lx| (lx, ly))).map(|(lx, ly)| coverage(lx, ly, bx, r, m)).sum();
                let cov = if n > 0.0 { sum / n } else { 0.0 };
                if cov >= 1.0 {
                    data[at] = source(i);
                } else if cov > 0.0 {
                    data[at] = blend(source(i), data[at], cov, 1.0 - cov);
                }
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

/// A layout worked out at one size; swapped in whole, so a render never sees half of a change.
struct Plan {
    layout: Layout,
    size: (usize, usize),
    background: Arc<Rgb>,
    boxes: Vec<Placed>,
}

type ScaleKey = (u64, (i64, i64, i64, i64), usize, usize, usize, usize, Kind);
type ScaledPlanes = Vec<Vec<u8>>;
type BackgroundKey = (String, String, usize, usize);

/// composite.Compositor: prepare() on the host thread (it may read the background picture), render() on a stream's thread.
pub struct Compositor {
    plan: Option<Arc<Plan>>,
    /// Tile index: the scaled stream for a frame seen before.
    scaled: HashMap<usize, (ScaleKey, Arc<ScaledPlanes>)>,
    reader: PictureReader,
    /// The picture as read, so moving a tile doesn't read it again.
    pictures: Option<(String, Arc<Rgb>)>,
    /// (picture, colour, size): the background drawn, so moving a tile reuses it.
    backgrounds: Option<(BackgroundKey, Arc<Rgb>)>,
    /// The background in the planes last drawn in.
    planes: Option<(usize, Kind, Arc<Vec<u8>>)>,
    /// The sizes a background picture was scaled to, for the tests.
    pub(crate) covered: Vec<(usize, usize)>,
}

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
    let rgb = to_rgb(frame)?;
    let format = kind.format();
    let mut out = Frame::new(frame.width, frame.height, format, vec![255; format.size(frame.width, frame.height)]);
    put_rgb(&mut out, &rgb);
    Some(out)
}

impl Compositor {
    pub fn new(reader: PictureReader) -> Compositor {
        Compositor { plan: None, scaled: HashMap::new(), reader, pictures: None, backgrounds: None, planes: None, covered: Vec::new() }
    }

    pub fn prepare(&mut self, layout: &Layout, size: (u32, u32)) {
        let (width, height) = (size.0.max(1) as usize, size.1.max(1) as usize);
        let background = self.background(layout, width, height);
        let boxes = layout.tiles.iter().map(|tile| {
            let (x0, y0, x1, y1) = box_px(tile, width as i64, height as i64);
            let border = scaled_size(tile.border, height as i64).min((x1 - x0) / 3).min((y1 - y0) / 3);
            let radius = scaled_size(tile.radius, height as i64);
            Placed { bx: (x0, y0, x1 - x0, y1 - y0), border, radius, inner: 0.max(radius - border) }
        }).collect();
        self.plan = Some(Arc::new(Plan { layout: layout.clone(), size: (width, height), background, boxes }));
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
        self.scaled.get(&index).map(|(_, s)| s.clone())
    }

    fn background_planes(&mut self, plan: &Plan, kind: Kind) -> Arc<Vec<u8>> {
        let id = Arc::as_ptr(&plan.background) as usize;
        if let Some((i, k, data)) = &self.planes {
            if *i == id && *k == kind {
                return data.clone();
            }
        }
        let (w, h) = plan.size;
        let format = kind.format();
        let mut frame = Frame::new(w as u32, h as u32, format, vec![255; format.size(w as u32, h as u32)]);
        put_rgb(&mut frame, &plan.background.data);
        let data = frame.data.clone();
        self.planes = Some((id, kind, data.clone()));
        data
    }

    /// The picture, from frames; tiles without a frame are left out. In format's planes (I420 for one that isn't RGBA or
    /// BGRA); None before prepare().
    pub fn render(&mut self, frames: &Frames, format: PixelFormat) -> Option<Frame> {
        let plan = self.plan.clone()?;
        let kind = Kind::of(format);
        let (cw, chh) = plan.size;
        let mut out = self.background_planes(&plan, kind).as_ref().clone();
        for (i, (tile, placed)) in plan.layout.tiles.iter().zip(&plan.boxes).enumerate() {
            let Some((frame, number)) = frames.get(&tile.source_id) else { continue };
            let (mut x, mut y, mut w, mut h) = placed.bx;
            if placed.border > 0 {
                let colour = colour_planes(kind, { let [b, g, r] = bgr(&tile.border_colour); [r, g, b] });
                paste(&mut out, kind, cw, chh, Content::Colour(colour), (x as usize, y as usize, w as usize, h as usize), placed.radius as usize);
                (x, y, w, h) = (x + placed.border, y + placed.border, w - 2 * placed.border, h - 2 * placed.border);
            }
            if w > 0 && h > 0 {
                let bx = (x as usize, y as usize, w as usize, h as usize);
                if let Some(scaled) = self.scale(i, tile, frame, *number, bx, kind) {
                    let radius = if placed.border > 0 { placed.inner } else { placed.radius };
                    paste(&mut out, kind, cw, chh, Content::Picture(&scaled), bx, radius as usize);
                }
            }
        }
        Some(Frame::new(cw as u32, chh as u32, kind.format(), out))
    }

    /// The tile's crop of frame scaled to its box, plane by plane; kept for a frame that can come again.
    fn scale(&mut self, index: usize, tile: &Tile, frame: &Frame, number: Option<u64>, bx: (usize, usize, usize, usize), kind: Kind) -> Option<Arc<Vec<Vec<u8>>>> {
        let (fw, fh) = (frame.width as i64, frame.height as i64);
        if fw == 0 || fh == 0 {
            return None;
        }
        let (_, _, w, h) = bx;
        let crop = crop_px(tile, fw, fh, w as i64, h as i64);
        let (cx0, cy0, cx1, cy1) = plane_box(2, bx);
        let key = (number.unwrap_or(0), crop, w, h, cx1 - cx0, cy1 - cy0, kind);
        if let (Some(_), Some((k, cached))) = (number, self.scaled.get(&index)) {
            if *k == key {
                // a slower stream's frame that hasn't changed since the last picture
                return Some(cached.clone());
            }
        }
        let source = converted(frame, kind)?;
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
        let out = Arc::new(out);
        self.scaled.insert(index, (key, out.clone()));
        Some(out)
    }
}

#[cfg(test)]
#[path = "compositor_tests.rs"]
mod tests;
