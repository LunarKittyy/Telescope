//! What the Cinema tools draw over the preview (CinemaLayer in plugins/cinema.py): the framing guides, the false colour legend,
//! the level guide and the scope, painted into one transparent picture the size of the preview, and the layer that holds the
//! tools' look, the scope last worked out and the newest tilt for the preview to ask. The guides' lines are the sharp ones Qt
//! draws without anti-aliasing; the rest is smoothed.

use crate::cinema_tools::*;
use crate::cpu::Rgb;
use crate::scopes::{self, Bgra};
use std::sync::{Arc, Mutex};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::i18n::tr;
use telescope_core::preview::PreviewPass;
use telescope_core::text::{app_font, Face, Weight};
use telescope_core::time::monotonic_ns;

/// px, a false colour legend swatch.
const LEGEND: f64 = 6.0;
const LEVEL_GREEN: [u8; 3] = [90, 225, 120];
const LEVEL_WHITE: [u8; 3] = [255, 255, 255];
const LEVEL_WHITE_ALPHA: u8 = 225;
/// Qt's pixels per point on the screens it draws at 96 dpi.
const PX_PER_PT: f64 = 96.0 / 72.0;

/// A place in the preview, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn centre(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// A non-negative number to the nearest whole one, halves up, as a byte (f32::round, which is a libm call, gives the same).
#[inline(always)]
fn round_byte(v: f32) -> u8 {
    let whole = v as i32;
    (whole + ((v - whole as f32) >= 0.5) as i32).clamp(0, 255) as u8
}

/// colour over one RGBA pixel, coverage 0..1 of its alpha.
#[inline(always)]
fn blend_pixel(d: &mut [u8], rgb: [u8; 3], alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let da = d[3] as f32 / 255.0;
    if da == 0.0 {
        // Over nothing: its own colour, and the alpha it came with
        d[..3].copy_from_slice(&rgb);
        d[3] = round_byte(alpha * 255.0);
        return;
    }
    let out = alpha + da * (1.0 - alpha);
    for c in 0..3 {
        d[c] = round_byte((rgb[c] as f32 * alpha + d[c] as f32 * da * (1.0 - alpha)) / out);
    }
    d[3] = round_byte(out * 255.0);
}

/// A transparent picture to draw on: R, G, B, A with straight alpha.
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas { w, h, data: vec![0; w * h * 4] }
    }

    /// colour over what is there already, coverage 0..1 of its alpha.
    fn blend(&mut self, x: i64, y: i64, rgb: [u8; 3], alpha: f32) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 || alpha <= 0.0 {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 4;
        blend_pixel(&mut self.data[i..i + 4], rgb, alpha);
    }

    /// A rectangle with sharp edges: the pixels whose centres are inside it.
    pub fn fill_rect(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, rgb: [u8; 3], alpha: u8) {
        let (px0, px1) = ((x0 - 0.5).ceil() as i64, (x1 - 0.5).ceil() as i64);
        let (py0, py1) = ((y0 - 0.5).ceil() as i64, (y1 - 0.5).ceil() as i64);
        for y in py0.max(0)..py1.min(self.h as i64) {
            for x in px0.max(0)..px1.min(self.w as i64) {
                self.blend(x, y, rgb, alpha as f32 / 255.0);
            }
        }
    }

    /// A smoothed line of this width between two points, with round ends.
    pub fn line(&mut self, a: (f64, f64), b: (f64, f64), width: f64, rgb: [u8; 3], alpha: u8) {
        let half = width / 2.0;
        let pad = half + 1.0;
        let (x0, x1) = ((a.0.min(b.0) - pad).floor().max(0.0) as i64, (a.0.max(b.0) + pad).ceil().min(self.w as f64) as i64);
        let (y0, y1) = ((a.1.min(b.1) - pad).floor().max(0.0) as i64, (a.1.max(b.1) + pad).ceil().min(self.h as f64) as i64);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f64 + 0.5 - a.0, y as f64 + 0.5 - a.1);
                let t = if len2 == 0.0 { 0.0 } else { ((px * dx + py * dy) / len2).clamp(0.0, 1.0) };
                let d = ((px - t * dx).powi(2) + (py - t * dy).powi(2)).sqrt();
                let cover = (half + 0.5 - d).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.blend(x, y, rgb, cover as f32 * alpha as f32 / 255.0);
                }
            }
        }
    }

    /// A smoothed rectangle with rounded corners.
    pub fn rounded_rect(&mut self, r: Rect, radius: f64, rgb: [u8; 3], alpha: u8) {
        let (x0, x1) = (r.x.floor().max(0.0) as i64, r.right().ceil().min(self.w as f64) as i64);
        let (y0, y1) = (r.y.floor().max(0.0) as i64, r.bottom().ceil().min(self.h as f64) as i64);
        let (cx, cy) = r.centre();
        let (hx, hy) = (r.w / 2.0 - radius, r.h / 2.0 - radius);
        for y in y0..y1 {
            for x in x0..x1 {
                let (qx, qy) = (((x as f64 + 0.5) - cx).abs() - hx, ((y as f64 + 0.5) - cy).abs() - hy);
                let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius;
                let cover = (0.5 - d).clamp(0.0, 1.0);
                if cover > 0.0 {
                    self.blend(x, y, rgb, cover as f32 * alpha as f32 / 255.0);
                }
            }
        }
    }

    /// Text with its start at x and its baseline at `baseline`.
    pub fn text(&mut self, face: &Face, text: &str, x: f64, baseline: f64, rgb: [u8; 3], alpha: u8) {
        let line = face.layout(text, 0.0);
        let mut cover = Vec::new();
        face.draw(&line, x as f32, baseline as f32, |px, py, c| cover.push((px, py, c)));
        for (px, py, c) in cover {
            self.blend(px as i64, py as i64, rgb, c * alpha as f32 / 255.0);
        }
    }

    /// A picture (B, G, R, A) laid over the canvas with its top left at (x, y).
    pub fn image(&mut self, image: &Bgra, x: i64, y: i64) {
        // The columns and rows of the picture that land on the canvas
        let (cols, rows) = ((-x).max(0) as usize..image.w.min((self.w as i64 - x).max(0) as usize), (-y).max(0) as usize..image.h.min((self.h as i64 - y).max(0) as usize));
        if cols.is_empty() {
            return;
        }
        if rows.is_empty() {
            return;
        }
        // Rows of the canvas from the first one the picture lands on, spread over the workers when the picture is big
        let (top, canvas_w) = ((y + rows.start as i64) as usize, self.w);
        let band = &mut self.data[top * canvas_w * 4..(top + rows.len()) * canvas_w * 4];
        telescope_core::image::over_rows(band, canvas_w * 4, |i, line| {
            let row = rows.start + i;
            let from = &image.data[(row * image.w + cols.start) * 4..(row * image.w + cols.end) * 4];
            let to = (x + cols.start as i64) as usize * 4;
            for (d, p) in line[to..to + from.len()].chunks_exact_mut(4).zip(from.chunks_exact(4)) {
                blend_pixel(d, [p[2], p[1], p[0]], p[3] as f32 / 255.0);
            }
        });
    }

    pub fn is_empty(&self) -> bool {
        self.data.as_chunks::<4>().0.iter().all(|p| p[3] == 0)
    }
}

fn face(points: f64, scale: f64) -> Option<Face> {
    Some(Face::em(app_font(Weight::Regular)?, (points * PX_PER_PT * scale) as f32))
}

/// Lines across and down the picture at shares of it, white with a faint dark edge so they show on any picture.
pub fn paint_guides(canvas: &mut Canvas, rect: Rect, shares: &[f64], scale: f64) {
    // (across the picture, where): a line down at x, or one along at y
    let mut lines = Vec::new();
    for &share in shares {
        lines.push((true, (rect.x + rect.w * share).round_ties_even() + 0.5));
        lines.push((false, (rect.y + rect.h * share).round_ties_even() + 0.5));
    }
    for (rgb, alpha, width) in [([0, 0, 0], 70u8, 3.0), ([255, 255, 255], 150, 1.0)] {
        let half = width * scale / 2.0;
        for &(down, at) in &lines {
            if down {
                canvas.fill_rect(at - half, rect.y, at + half, rect.bottom(), rgb, alpha);
            } else {
                canvas.fill_rect(rect.x, at - half, rect.right(), at + half, rgb, alpha);
            }
        }
    }
}

/// The false colour legend: a swatch and a name for each colour, bottom left.
pub fn paint_legend(canvas: &mut Canvas, rect: Rect, scale: f64) {
    let Some(face) = face(8.0, scale) else { return };
    let rows: Vec<([u8; 3], String)> = FALSE_COLOURS.iter().rev().filter_map(|(_, c, name)| c.map(|c| (c, tr(name)))).collect();
    let line = face.height() as f64;
    let pad = 6.0 * scale;
    let swatch = LEGEND * scale;
    let width = rows.iter().map(|(_, t)| face.advance(t) as f64).fold(0.0, f64::max) + swatch + 3.0 * pad;
    let height = rows.len() as f64 * line + 2.0 * pad;
    let bx = Rect { x: rect.x + 8.0 * scale, y: rect.bottom() - 8.0 * scale - height, w: width, h: height };
    canvas.rounded_rect(bx, 4.0 * scale, [0, 0, 0], 150);
    for (i, (colour, text)) in rows.iter().enumerate() {
        let top = bx.y + pad + i as f64 * line;
        let sy = top + (line - swatch) / 2.0;
        canvas.fill_rect(bx.x + pad, sy, bx.x + pad + swatch, sy + swatch, *colour, 255);
        canvas.text(&face, text, bx.x + 2.0 * pad + swatch, top + face.ascent() as f64, [255, 255, 255], 255);
    }
}

/// The two ends of the level guide's moving line: through the middle of rect, turned clockwise by roll degrees.
pub fn level_line(rect: Rect, roll: f64) -> ((f64, f64), (f64, f64)) {
    let half = rect.w.min(rect.h * 1.6) * 0.3;
    let a = roll.to_radians();
    // y goes down, so a positive angle turns clockwise
    let (dx, dy) = (half * a.cos(), half * a.sin());
    let c = rect.centre();
    ((c.0 - dx, c.1 - dy), (c.0 + dx, c.1 + dy))
}

/// The level guide: fixed marks for level on both sides, a line along the horizon between them and the pitch.
pub fn paint_level(canvas: &mut Canvas, rect: Rect, level: &Level, scale: f64) {
    let c = rect.centre();
    let half = rect.w.min(rect.h * 1.6) * 0.3;
    let (colour, colour_alpha) = if level.roll_level { (LEVEL_GREEN, 255) } else { (LEVEL_WHITE, LEVEL_WHITE_ALPHA) };
    let mut lines = Vec::new();
    if level.roll.is_some() {
        let (gap, mark) = (8.0 * scale, (16.0 * scale).max(half * 0.18));
        lines.push(((c.0 - half - gap - mark, c.1), (c.0 - half - gap, c.1)));
        lines.push(((c.0 + half + gap, c.1), (c.0 + half + gap + mark, c.1)));
        lines.push(level_line(rect, level.roll.unwrap_or(0.0)));
    }
    for (rgb, alpha, width) in [([0, 0, 0], 110u8, 4.0), (colour, colour_alpha, 2.0)] {
        for &(a, b) in &lines {
            canvas.line(a, b, width * scale, rgb, alpha);
        }
    }
    let Some(face) = face(9.0, scale) else { return };
    #[allow(clippy::type_complexity)] // a throwaway row: text, colour, alpha, position
    let mut labels: Vec<(String, [u8; 3], u8, (f64, f64))> = Vec::new();
    if let Some(roll) = level.roll {
        labels.push((format!("{:.1}°", roll.abs()), colour, colour_alpha, (c.0 + half + 8.0 * scale, c.1 - 8.0 * scale)));
    }
    let pitch_text = if level.pitch_level { "0.0°".to_string() } else { format!("{} {:.1}°", if level.pitch > 0.0 { "▲" } else { "▼" }, level.pitch.abs()) };
    let pitch_width = face.advance(&pitch_text) as f64;
    let (pitch_rgb, pitch_alpha) = if level.pitch_level { (LEVEL_GREEN, 255) } else { (LEVEL_WHITE, LEVEL_WHITE_ALPHA) };
    labels.push((pitch_text, pitch_rgb, pitch_alpha, (c.0 - pitch_width / 2.0, c.1 + face.height() as f64 + 10.0 * scale)));
    for (text, rgb, alpha, at) in labels {
        let width = face.advance(&text) as f64;
        let boxed = Rect { x: at.0 - 4.0 * scale, y: at.1 - face.ascent() as f64 - 2.0 * scale, w: width + 8.0 * scale, h: face.height() as f64 + 4.0 * scale };
        canvas.rounded_rect(boxed, 3.0 * scale, [0, 0, 0], 130);
        canvas.text(&face, &text, at.0, at.1, rgb, alpha);
    }
}

/// Where the scope picture goes on the preview and how big it is: (x, y, w, h).
pub fn scope_target(rect: Rect, image: (usize, usize), size: &str, scale: f64) -> (f64, f64, f64, f64) {
    let share = SIZES.iter().find(|(k, ..)| *k == size).map_or(SIZES[1].2, |s| s.2);
    let margin = 8.0 * scale;
    let (mut w, mut h) = (rect.w * share, rect.w * share * image.1 as f64 / image.0 as f64);
    if h > rect.h * 0.6 {
        h = rect.h * 0.6;
        w = h * image.0 as f64 / image.1 as f64;
    }
    (rect.right() - w - margin, rect.bottom() - h - margin, w, h)
}

/// The tools as a preview pass: the pixel tools in tint(), the scope worked out in analyse() and drawn in paint().
pub struct CinemaLayer {
    state: Mutex<State>,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
}

struct State {
    look: Look,
    phase: usize,
    /// The scope last worked out, by name.
    scope: Option<(String, Bgra)>,
    /// The level guide's tilt and the monotonic seconds its reading came.
    level: Option<(Level, f64)>,
    /// Monotonic seconds the preview last drew this layer.
    painted_at: f64,
}

/// Monotonic seconds, the clock the sensors' readings are stamped with.
pub fn now() -> f64 {
    monotonic_ns() as f64 / 1e9
}

impl Default for CinemaLayer {
    fn default() -> CinemaLayer {
        CinemaLayer::with_clock(Arc::new(now))
    }
}

impl CinemaLayer {
    pub fn with_clock(clock: Arc<dyn Fn() -> f64 + Send + Sync>) -> CinemaLayer {
        CinemaLayer { state: Mutex::new(State { look: Look::default(), phase: 0, scope: None, level: None, painted_at: 0.0 }), clock }
    }

    pub fn look(&self) -> Look {
        self.state.lock().unwrap().look.clone()
    }

    pub fn set_look(&self, look: Look) {
        self.state.lock().unwrap().look = look;
    }

    pub fn level(&self) -> Option<(Level, f64)> {
        self.state.lock().unwrap().level
    }

    pub fn set_level(&self, level: Option<(Level, f64)>) {
        self.state.lock().unwrap().level = level;
    }

    pub fn painted_at(&self) -> f64 {
        self.state.lock().unwrap().painted_at
    }

    pub fn set_painted_at(&self, at: f64) {
        self.state.lock().unwrap().painted_at = at;
    }

    /// The scope worked out last: its name and picture.
    pub fn scope(&self) -> Option<(String, Bgra)> {
        self.state.lock().unwrap().scope.clone()
    }

    /// The pixel tools on a picture, as RGB.
    pub fn tint_rgb(&self, frame: &mut Rgb) {
        let mut state = self.state.lock().unwrap();
        let look = state.look.clone();
        tint(&look, frame, &mut state.phase);
    }

    /// The scope of a picture for the current look, as analyse() does it.
    pub fn analyse_rgb(&self, frame: &Rgb) {
        let name = self.look().scope;
        let scope = if name == "off" { None } else { scopes::scope_image(name, frame).map(|image| (name.to_string(), image)) };
        self.state.lock().unwrap().scope = scope;
    }

    /// Everything the layer draws, at this size, or None when nothing shows.
    pub fn paint_canvas(&self, width: usize, height: usize, scale: f64) -> Option<Canvas> {
        let now = (self.clock)();
        let (look, level, scope) = {
            let mut state = self.state.lock().unwrap();
            state.painted_at = now;
            (state.look.clone(), state.level, state.scope.clone())
        };
        let rect = Rect { x: 0.0, y: 0.0, w: width as f64, h: height as f64 };
        let mut canvas = Canvas::new(width, height);
        if let Some(shares) = guide_lines(look.guides) {
            paint_guides(&mut canvas, rect, &shares, scale);
        }
        if look.false_colour {
            paint_legend(&mut canvas, rect, scale);
        }
        if let (true, Some((level, at))) = (look.level, level) {
            if now - at < LEVEL_STALE_S {
                paint_level(&mut canvas, rect, &level, scale);
            }
        }
        if let (Some((name, image)), true) = (scope, look.scope != "off") {
            if name == look.scope {
                let (x, y, w, h) = scope_target(rect, (image.w, image.h), look.scope_size, scale);
                let (tw, th) = ((w.round() as usize).max(1), (h.round() as usize).max(1));
                let scaled = Bgra { w: tw, h: th, data: smooth_scale(&image.data, image.w, image.h, tw, th) };
                canvas.image(&scaled, x.round() as i64, y.round() as i64);
            }
        }
        (!canvas.is_empty()).then_some(canvas)
    }
}

/// QPainter.drawImage with SmoothPixmapTransform: bilinear on premultiplied colour, so see-through pixels don't darken what's
/// next to them.
fn smooth_scale(src: &[u8], w: usize, h: usize, tw: usize, th: usize) -> Vec<u8> {
    // The source's colour already multiplied by its alpha, once for each pixel instead of for every time it's read
    let premultiplied: Vec<[f32; 4]> = src
        .chunks_exact(4)
        .take(w * h)
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, p[3] as f32]
        })
        .collect();
    // Which two source columns each output column mixes, and how much of the second
    let columns: Vec<(usize, usize, f32)> = (0..tw)
        .map(|tx| {
            let sx = (((tx as f32 + 0.5) * w as f32 / tw as f32) - 0.5).clamp(0.0, (w - 1) as f32);
            let x0 = sx.floor() as usize;
            (x0, (x0 + 1).min(w - 1), sx.fract())
        })
        .collect();
    let mut out = vec![0u8; tw * th * 4];
    telescope_core::image::over_rows(&mut out, tw * 4, |ty, line| {
        let sy = (((ty as f32 + 0.5) * h as f32 / th as f32) - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, fy) = (sy.floor() as usize, sy.fract());
        let y1 = (y0 + 1).min(h - 1);
        let (upper, lower) = (&premultiplied[y0 * w..][..w], &premultiplied[y1 * w..][..w]);
        for (o, &(x0, x1, fx)) in line.chunks_exact_mut(4).zip(&columns) {
            let (a, b, c, d) = (&upper[x0], &upper[x1], &lower[x0], &lower[x1]);
            let mix: [f32; 4] = std::array::from_fn(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy);
            let alpha = mix[3] / 255.0;
            for (o, colour) in o[..3].iter_mut().zip(&mix[..3]) {
                *o = if alpha > 0.0 { round_byte(colour / alpha) } else { 0 };
            }
            o[3] = round_byte(mix[3]);
        }
    });
    out
}

/// What smooth_scale did before it was sped up, to check the new one against byte for byte.
#[cfg(test)]
fn smooth_scale_reference(src: &[u8], w: usize, h: usize, tw: usize, th: usize) -> Vec<u8> {
    let at = |x: usize, y: usize| -> [f32; 4] {
        let p = &src[(y * w + x) * 4..][..4];
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, p[3] as f32]
    };
    let mut out = Vec::with_capacity(tw * th * 4);
    for ty in 0..th {
        let sy = (((ty as f32 + 0.5) * h as f32 / th as f32) - 0.5).clamp(0.0, (h - 1) as f32);
        let (y0, fy) = (sy.floor() as usize, sy.fract());
        let y1 = (y0 + 1).min(h - 1);
        for tx in 0..tw {
            let sx = (((tx as f32 + 0.5) * w as f32 / tw as f32) - 0.5).clamp(0.0, (w - 1) as f32);
            let (x0, fx) = (sx.floor() as usize, sx.fract());
            let x1 = (x0 + 1).min(w - 1);
            let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
            let mix: [f32; 4] = std::array::from_fn(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy);
            let alpha = mix[3] / 255.0;
            for colour in &mix[..3] {
                out.push(if alpha > 0.0 { (colour / alpha).round().min(255.0) as u8 } else { 0 });
            }
            out.push(mix[3].round() as u8);
        }
    }
    out
}

impl PreviewPass for CinemaLayer {
    /// Under anything else on the preview (a pick's corners stay on top).
    fn order(&self) -> i32 {
        -10
    }

    fn tints(&self) -> bool {
        self.state.lock().unwrap().look.pixels()
    }

    fn tint(&self, frame: &mut Frame) {
        if !self.tints() {
            return;
        }
        let Some(mut rgb) = Rgb::from_frame(frame) else { return };
        self.tint_rgb(&mut rgb);
        if (rgb.w, rgb.h) == (frame.width as usize, frame.height as usize) {
            rgb.write_to(frame);
        }
    }

    fn analyse_fps(&self) -> f32 {
        if self.state.lock().unwrap().look.scope != "off" { SCOPE_FPS } else { 0.0 }
    }

    fn analyse(&self, frame: &Frame) -> bool {
        if let Some(rgb) = Rgb::from_frame(frame) {
            self.analyse_rgb(&rgb);
        }
        true
    }

    fn paint(&self, width: u32, height: u32, scale: f32) -> Option<Frame> {
        let canvas = self.paint_canvas(width as usize, height as usize, scale as f64)?;
        Some(Frame::new(width, height, PixelFormat::Rgba, canvas.data))
    }
}

#[cfg(test)]
#[path = "cinema_paint_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "cinema_paint_speed_tests.rs"]
mod speed_tests;
