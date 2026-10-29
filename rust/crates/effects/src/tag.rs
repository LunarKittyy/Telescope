//! The name tag's drawing (telescope/name_tag.py): a lower third with a name, pronouns and an optional second line.
//!
//! `layout` works out where everything goes on a canvas of a given size, `render` paints it once into a small tile, and
//! `TagCache` keeps that tile until the text, the look or the canvas changes, so a frame only costs one blend of the tile's
//! rectangle (`apply`). The text is drawn through core::text (Inter, then the system's fallbacks for other scripts).

use telescope_core::actions::round_half_even;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::text::{app_font, visual_order, Face, FontArc, Weight};

pub const STYLES: [&str; 4] = ["bar", "pill", "light", "shadow"];
pub const POSITIONS: [&str; 5] = ["bottom_left", "bottom_centre", "bottom_right", "top_left", "top_right"];
/// The name's text height, of the canvas height.
pub const SIZES: [(&str, f64); 3] = [("small", 0.032), ("medium", 0.042), ("large", 0.056)];
pub const DEFAULT_STYLE: &str = "bar";
pub const DEFAULT_POSITION: &str = "bottom_left";
pub const DEFAULT_SIZE: &str = "medium";
pub const MAX_CHARS: usize = 80;

/// Pronouns and the second line, relative to the name.
const PRONOUNS: f64 = 0.72;
const SECOND: f64 = 0.66;
const ACCENT: u32 = 0xaa9cf5;

/// (fill as ARGB or None, name, pronouns, second line) of a style.
fn inks(style: &str) -> (Option<u32>, u32, u32, u32) {
    match style {
        "pill" => (Some(0xa60f1216), 0xffffff, 0xc9cfd8, 0xb4bcc8),
        "light" => (Some(0xebfafafc), 0x15181d, 0x4a5260, 0x58606e),
        "shadow" => (None, 0xffffff, 0xe6e9ee, 0xdde1e7),
        _ => (Some(0xd90f1216), 0xffffff, 0xc9cfd8, 0xb4bcc8),
    }
}

/// What the tag says and how it looks.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub name: String,
    pub pronouns: String,
    pub second: String,
    pub style: String,
    pub position: String,
    pub size: String,
}

impl Default for Spec {
    fn default() -> Self {
        Spec { name: String::new(), pronouns: String::new(), second: String::new(), style: DEFAULT_STYLE.into(), position: DEFAULT_POSITION.into(), size: DEFAULT_SIZE.into() }
    }
}

impl Spec {
    pub fn empty(&self) -> bool {
        self.name.trim().is_empty() && self.pronouns.trim().is_empty() && self.second.trim().is_empty()
    }

    fn size_factor(&self) -> f64 {
        SIZES.iter().find(|(k, _)| *k == self.size).map_or(SIZES[1].1, |(_, f)| *f)
    }
}

/// QRectF.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn left(&self) -> f64 {
        self.x
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn top(&self) -> f64 {
        self.y
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn centre(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// One piece of text with where its baseline starts.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub text: String,
    pub x: f64,
    pub baseline: f64,
}

/// Where the tag goes on a w x h canvas: the box, and each piece of text with its baseline, inside the box.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub box_: Rect,
    pub name: Option<Piece>,
    pub pronouns: Option<Piece>,
    pub second: Option<Piece>,
    /// The accent bar's rectangle (bar style).
    pub stripe: Option<Rect>,
    pub radius: f64,
    /// The name's pixel size, what the rest is measured in.
    pub em: f64,
}

/// The two fonts the tag draws with.
#[derive(Clone)]
pub struct Fonts {
    pub semibold: FontArc,
    pub regular: FontArc,
}

impl Fonts {
    pub fn system() -> Option<Fonts> {
        Some(Fonts { semibold: app_font(Weight::SemiBold)?, regular: app_font(Weight::Regular)? })
    }
}

/// Qt's setPixelSize(max(6, round(px))).
fn pixel_size(px: f64) -> f32 {
    round_half_even(px).max(6.0) as f32
}

/// Whether text starts out right to left.
pub use telescope_core::text::right_to_left;

/// Where everything goes on a w x h canvas, or None when there's nothing to show (or no font to measure with).
pub fn layout(spec: &Spec, w: u32, h: u32) -> Option<Layout> {
    layout_with(spec, w, h, &Fonts::system()?)
}

pub fn layout_with(spec: &Spec, w: u32, h: u32, fonts: &Fonts) -> Option<Layout> {
    if spec.empty() || w < 16 || h < 16 {
        return None;
    }
    let (wf, hf) = (w as f64, h as f64);
    let em = (hf * spec.size_factor()).max(8.0);
    let (pad_x, pad_y, gap) = (em * 0.7, em * 0.42, em * 0.16);
    let stripe_w = if spec.style == "bar" { em * 0.16 } else { 0.0 };
    let bottom = spec.position.starts_with("bottom");
    let margin_x = (wf * 0.035).max(4.0);
    let margin_y = (hf * if bottom { 0.075 } else { 0.05 }).max(4.0);
    let room = (wf - 2.0 * margin_x - 2.0 * pad_x - stripe_w).max(em);
    let fm_name = Face::em(fonts.semibold.clone(), pixel_size(em)).heavy();
    let fm_pron = Face::em(fonts.regular.clone(), pixel_size(em * PRONOUNS));
    let fm_second = Face::em(fonts.regular.clone(), pixel_size(em * SECOND));

    let mut name = spec.name.trim().to_string();
    let mut pronouns = spec.pronouns.trim().to_string();
    let mut second = spec.second.trim().to_string();
    let pron_gap = if !name.is_empty() && !pronouns.is_empty() { em * 0.45 } else { 0.0 };
    let mut pron_w = if pronouns.is_empty() { 0.0 } else { fm_pron.advance(&pronouns) as f64 };
    if pron_w > room * 0.5 && !name.is_empty() {
        pronouns = fm_pron.elide(&pronouns, (room * 0.5) as f32);
        pron_w = fm_pron.advance(&pronouns) as f64;
    }
    if !name.is_empty() {
        name = fm_name.elide(&name, (room - pron_w - pron_gap) as f32);
    }
    if name.is_empty() && !pronouns.is_empty() {
        pronouns = fm_pron.elide(&pronouns, room as f32);
        pron_w = fm_pron.advance(&pronouns) as f64;
    }
    if !second.is_empty() {
        second = fm_second.elide(&second, room as f32);
    }
    let name_w = if name.is_empty() { 0.0 } else { fm_name.advance(&name) as f64 };
    let first_w = name_w + pron_gap + pron_w;
    let second_w = if second.is_empty() { 0.0 } else { fm_second.advance(&second) as f64 };
    let first_h = f64::max(if name.is_empty() { 0.0 } else { fm_name.height() as f64 }, if pronouns.is_empty() { 0.0 } else { fm_pron.height() as f64 });
    let inner_w = first_w.max(second_w);
    let inner_h = first_h + if second.is_empty() { 0.0 } else { gap + fm_second.height() as f64 };
    let (box_w, box_h) = (inner_w + 2.0 * pad_x + stripe_w, inner_h + 2.0 * pad_y);

    let x = if spec.position.ends_with("left") {
        margin_x
    } else if spec.position.ends_with("right") {
        wf - margin_x - box_w
    } else {
        (wf - box_w) / 2.0
    };
    let y = if bottom { hf - margin_y - box_h } else { margin_y };
    let box_ = Rect { x: round_half_even(x), y: round_half_even(y), w: round_half_even(box_w), h: round_half_even(box_h) };

    let direction = [&name, &pronouns, &second].into_iter().find(|t| !t.is_empty()).map_or("", |t| t.as_str());
    let rtl = right_to_left(direction);
    let (mut left, mut right) = (box_.left() + pad_x, box_.right() - pad_x);
    if rtl {
        right -= stripe_w; // the accent goes where the text starts
    } else {
        left += stripe_w;
    }
    let centred = spec.position == "bottom_centre";
    let start = |width: f64| if centred { left + (inner_w - width) / 2.0 } else if rtl { right - width } else { left };

    let base = box_.top() + pad_y + f64::max(if name.is_empty() { 0.0 } else { fm_name.ascent() as f64 }, if pronouns.is_empty() { 0.0 } else { fm_pron.ascent() as f64 });
    let line = start(first_w);
    let (name_x, pron_x) = if rtl { (line + pron_w + pron_gap, line) } else { (line, line + name_w + pron_gap) };
    let stripe = (stripe_w > 0.0).then(|| Rect { x: if rtl { box_.right() - stripe_w } else { box_.left() }, y: box_.top(), w: stripe_w, h: box_.h });
    let radius = match spec.style.as_str() {
        "pill" => if second.is_empty() { box_h / 2.0 } else { em * 0.5 },
        "light" => em * 0.28,
        _ => 0.0,
    };
    let piece = |text: String, x: f64, baseline: f64| (!text.is_empty()).then_some(Piece { text, x, baseline });
    let second_base = base + f64::max(fm_name.descent() as f64, fm_pron.descent() as f64) + gap + fm_second.ascent() as f64;
    Some(Layout {
        box_,
        name: piece(name, name_x, base),
        pronouns: piece(pronouns, pron_x, base),
        second: piece(second.clone(), start(second_w), second_base),
        stripe,
        radius,
        em,
    })
}

/// A rendered tag: where its tile goes, and the tile as premultiplied colour and how much of the frame under it shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    /// RGB times alpha, 3 bytes a pixel.
    pub ink: Vec<u8>,
    /// 255 minus alpha, a byte a pixel.
    pub keep: Vec<u8>,
}

/// A premultiplied float tile the pieces are painted on.
struct Canvas {
    w: usize,
    h: usize,
    /// r, g, b, a premultiplied, 0..1.
    px: Vec<[f32; 4]>,
}

impl Canvas {
    fn over(&mut self, x: i64, y: i64, rgb: u32, alpha: f32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h || alpha <= 0.0 {
            return;
        }
        let c = [(rgb >> 16 & 0xff) as f32 / 255.0, (rgb >> 8 & 0xff) as f32 / 255.0, (rgb & 0xff) as f32 / 255.0];
        let d = &mut self.px[y as usize * self.w + x as usize];
        let keep = 1.0 - alpha;
        for i in 0..3 {
            d[i] = c[i] * alpha + d[i] * keep;
        }
        d[3] = alpha + d[3] * keep;
    }

    /// How much of the pixel [i, i + 1) the span lo..hi covers.
    fn span(i: i64, lo: f64, hi: f64) -> f32 {
        (hi.min(i as f64 + 1.0) - lo.max(i as f64)).clamp(0.0, 1.0) as f32
    }

    fn fill_rect(&mut self, r: Rect, rgb: u32, alpha: f32) {
        let (x0, x1) = (r.left().floor() as i64, r.right().ceil() as i64);
        let (y0, y1) = (r.top().floor() as i64, r.bottom().ceil() as i64);
        for y in y0..y1 {
            let cy = Self::span(y, r.top(), r.bottom());
            for x in x0..x1 {
                self.over(x, y, rgb, alpha * cy * Self::span(x, r.left(), r.right()));
            }
        }
    }

    /// A rounded rectangle, edges anti-aliased by how far each pixel's middle is from the outline.
    fn fill_round_rect(&mut self, r: Rect, radius: f64, rgb: u32, alpha: f32) {
        let radius = radius.min(r.w / 2.0).min(r.h / 2.0);
        let (cx, cy) = r.centre();
        let (hx, hy) = (r.w / 2.0 - radius, r.h / 2.0 - radius);
        for y in (r.top().floor() as i64)..(r.bottom().ceil() as i64) {
            for x in (r.left().floor() as i64)..(r.right().ceil() as i64) {
                let (dx, dy) = ((x as f64 + 0.5 - cx).abs() - hx, (y as f64 + 0.5 - cy).abs() - hy);
                let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - radius;
                self.over(x, y, rgb, alpha * (0.5 - outside).clamp(0.0, 1.0) as f32);
            }
        }
    }

    fn text(&mut self, face: &Face, piece: &Piece, rgb: u32) {
        let line = face.layout(&visual_order(&piece.text), 0.0);
        face.draw(&line, piece.x as f32, piece.baseline as f32, |x, y, cov| self.over(x as i64, y as i64, rgb, cov.clamp(0.0, 1.0)));
    }
}

/// cv2.GaussianBlur(src, (0, 0), sigma) on one 8-bit channel: the kernel OpenCV picks for it, edges reflected.
pub fn gaussian_blur(src: &[u8], w: usize, h: usize, sigma: f64) -> Vec<u8> {
    let size = ((sigma * 3.0 * 2.0 + 1.0).round() as usize) | 1;
    let half = (size / 2) as i64;
    let mut kernel: Vec<f32> = (-half..=half).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp() as f32).collect();
    let total: f32 = kernel.iter().sum();
    kernel.iter_mut().for_each(|k| *k /= total);
    let reflect = |i: i64, n: i64| {
        let mut i = i;
        if n == 1 {
            return 0;
        }
        loop {
            if i < 0 {
                i = -i;
            } else if i >= n {
                i = 2 * (n - 1) - i;
            } else {
                return i as usize;
            }
        }
    };
    let mut mid = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            mid[y * w + x] = kernel.iter().enumerate().map(|(k, c)| c * src[y * w + reflect(x as i64 + k as i64 - half, w as i64)] as f32).sum();
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let v: f32 = kernel.iter().enumerate().map(|(k, c)| c * mid[reflect(y as i64 + k as i64 - half, h as i64) * w + x]).sum();
            out[y * w + x] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// The tag for a w x h canvas, painted once; None when there's nothing to show.
pub fn render(spec: &Spec, w: u32, h: u32) -> Option<Tag> {
    render_with(spec, w, h, &Fonts::system()?)
}

pub fn render_with(spec: &Spec, w: u32, h: u32, fonts: &Fonts) -> Option<Tag> {
    let lay = layout_with(spec, w, h, fonts)?;
    let blur = if spec.style == "shadow" { lay.em * 0.14 } else { 0.0 };
    let spread = if blur > 0.0 { (blur * 3.0 + 2.0) as i64 } else { 0 };
    let (x0, y0) = ((lay.box_.left() as i64 - spread).max(0), (lay.box_.top() as i64 - spread).max(0));
    let (x1, y1) = ((lay.box_.right() as i64 + spread + 1).min(w as i64), (lay.box_.bottom() as i64 + spread + 1).min(h as i64));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (tw, th) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut canvas = Canvas { w: tw, h: th, px: vec![[0.0; 4]; tw * th] };
    let shift = |r: Rect| Rect { x: r.x - x0 as f64, y: r.y - y0 as f64, ..r };
    let (fill, ink_name, ink_pron, ink_second) = inks(&spec.style);
    if let Some(argb) = fill {
        canvas.fill_round_rect(shift(lay.box_), lay.radius, argb & 0xffffff, (argb >> 24) as f32 / 255.0);
    }
    if let Some(stripe) = lay.stripe {
        canvas.fill_rect(shift(stripe), ACCENT, 1.0);
    }
    let em = |scale: f64| Face::em(fonts.regular.clone(), pixel_size(lay.em * scale));
    let faces = [Face::em(fonts.semibold.clone(), pixel_size(lay.em)).heavy(), em(PRONOUNS), em(SECOND)];
    for ((piece, face), ink) in [(&lay.name, &faces[0]), (&lay.pronouns, &faces[1]), (&lay.second, &faces[2])].into_iter().zip([ink_name, ink_pron, ink_second]) {
        if let Some(piece) = piece {
            let moved = Piece { text: piece.text.clone(), x: piece.x - x0 as f64, baseline: piece.baseline - y0 as f64 };
            canvas.text(face, &moved, ink);
        }
    }
    let to_byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let mut ink = Vec::with_capacity(tw * th * 3);
    let mut alpha: Vec<u8> = Vec::with_capacity(tw * th);
    for p in &canvas.px {
        ink.extend([to_byte(p[0]), to_byte(p[1]), to_byte(p[2])]);
        alpha.push(to_byte(p[3]));
    }
    if blur > 0.0 {
        let mut shadow = gaussian_blur(&alpha, tw, th, blur);
        let shift_rows = (round_half_even(lay.em * 0.05) as usize).max(1);
        // Down by a few rows, the top ones empty: a soft dark halo under the text
        let rows = (shift_rows * tw).min(shadow.len());
        shadow.rotate_right(rows);
        shadow[..rows].fill(0);
        for (a, s) in alpha.iter_mut().zip(shadow) {
            let s = (s as u32 * 200 / 255) as f32;
            let behind = (s * (255 - *a) as f32 / 255.0).round();
            *a = (*a as f32 + behind).min(255.0) as u8;
        }
    }
    let keep = alpha.iter().map(|a| 255 - a).collect();
    Some(Tag { x: x0 as usize, y: y0 as usize, width: tw, height: th, ink, keep })
}

/// BT.601 limited range, as the decoder's frames and the preview use it: (Y, U, V) of premultiplied ink, with the offsets taken
/// out (they are added in proportion to how much of the tag covers the pixel).
fn yuv_of(ink: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (ink[0] as f32, ink[1] as f32, ink[2] as f32);
    (0.257 * r + 0.504 * g + 0.098 * b, -0.148 * r - 0.291 * g + 0.439 * b, 0.439 * r - 0.368 * g - 0.071 * b)
}

fn blend(under: u8, keep: u8, over: u8) -> u8 {
    ((under as u32 * keep as u32 + 127) / 255 + over as u32).min(255) as u8
}

/// The tag blended onto the frame: one multiply and an add over the tile. A frame in a format the tag can't be put on, or one
/// too small to hold the tile, is left as it is.
pub fn apply(frame: &mut Frame, tag: &Tag) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if tag.x + tag.width > w || tag.y + tag.height > h {
        return;
    }
    match frame.format {
        PixelFormat::Rgba | PixelFormat::Bgra => {
            let swap = frame.format == PixelFormat::Bgra;
            let data = frame.data_mut();
            for ty in 0..tag.height {
                for tx in 0..tag.width {
                    let t = ty * tag.width + tx;
                    let i = ((tag.y + ty) * w + tag.x + tx) * 4;
                    for c in 0..3 {
                        let ink = tag.ink[t * 3 + if swap { 2 - c } else { c }];
                        data[i + c] = blend(data[i + c], tag.keep[t], ink);
                    }
                }
            }
        }
        PixelFormat::I420 => {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let data = frame.data_mut();
            let (luma, chroma) = data.split_at_mut(w * h);
            let (us, vs) = chroma.split_at_mut(cw * ch);
            for ty in 0..tag.height {
                for tx in 0..tag.width {
                    let t = ty * tag.width + tx;
                    let alpha = 255.0 - tag.keep[t] as f32;
                    let (y, _, _) = yuv_of([tag.ink[t * 3], tag.ink[t * 3 + 1], tag.ink[t * 3 + 2]]);
                    let i = (tag.y + ty) * w + tag.x + tx;
                    let under = luma[i] as f32;
                    luma[i] = (under * tag.keep[t] as f32 / 255.0 + y + 16.0 * alpha / 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
            // Chroma is shared by 2 x 2 pixels: each takes the average of the tag over the pixels it covers
            let (cx0, cy0) = (tag.x / 2, tag.y / 2);
            let (cx1, cy1) = ((tag.x + tag.width).div_ceil(2), (tag.y + tag.height).div_ceil(2));
            for cy in cy0..cy1.min(ch) {
                for cx in cx0..cx1.min(cw) {
                    let (mut su, mut sv, mut keep, mut alpha, mut n) = (0.0, 0.0, 0.0, 0.0, 0.0);
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (x, y) = (cx * 2 + dx, cy * 2 + dy);
                        // Pixels of the 2 x 2 that the tile doesn't cover count as no tag
                        let inside = x >= tag.x && x < tag.x + tag.width && y >= tag.y && y < tag.y + tag.height;
                        let (k, a, u, v) = if inside {
                            let t = (y - tag.y) * tag.width + x - tag.x;
                            let (_, u, v) = yuv_of([tag.ink[t * 3], tag.ink[t * 3 + 1], tag.ink[t * 3 + 2]]);
                            (tag.keep[t] as f32, 255.0 - tag.keep[t] as f32, u, v)
                        } else {
                            (255.0, 0.0, 0.0, 0.0)
                        };
                        if x < w && y < h {
                            su += u;
                            sv += v;
                            keep += k;
                            alpha += a;
                            n += 1.0;
                        }
                    }
                    let i = cy * cw + cx;
                    us[i] = (us[i] as f32 * keep / n / 255.0 + su / n + 128.0 * alpha / n / 255.0).round().clamp(0.0, 255.0) as u8;
                    vs[i] = (vs[i] as f32 * keep / n / 255.0 + sv / n + 128.0 * alpha / n / 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        _ => {}
    }
}

/// The last rendered tag, kept until the spec or the canvas size changes.
#[derive(Default)]
pub struct TagCache {
    entry: Option<((Spec, u32, u32), Option<std::sync::Arc<Tag>>)>,
    /// How many times it painted, for tests.
    pub renders: usize,
}

impl TagCache {
    pub fn get(&mut self, spec: &Spec, w: u32, h: u32) -> Option<std::sync::Arc<Tag>> {
        let key = (spec.clone(), w, h);
        if self.entry.as_ref().is_none_or(|(k, _)| *k != key) {
            self.entry = Some((key, render(spec, w, h).map(std::sync::Arc::new)));
            self.renders += 1;
        }
        self.entry.as_ref().and_then(|(_, t)| t.clone())
    }
}

#[cfg(test)]
#[path = "tag_tests.rs"]
mod tests;
