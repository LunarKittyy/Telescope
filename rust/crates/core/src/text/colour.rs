//! Colour glyphs, which Qt draws straight from the font: COLR layers (Noto's COLRv1 on Linux, Segoe UI Emoji on Windows) through
//! skrifa's paint walk and tiny-skia, and embedded pictures (CBDT or sbix, Noto on Debian and Ubuntu) scaled to the size.

use skrifa::bitmap::{BitmapData, BitmapStrikes, Origin};
use skrifa::color::{Brush, ColorGlyph, ColorGlyphCollection, ColorPainter, ColorPalettes, ColorStop, CompositeMode, Extend, Transform as FontTransform};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlineGlyphCollection, OutlinePen};
use skrifa::raw::types::BoundingBox;
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use tiny_skia::{BlendMode, Color, FillRule, GradientStop, LinearGradient, Mask, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Point, RadialGradient, Rect, Shader, SpreadMode, SweepGradient, Transform};

/// A glyph painted, premultiplied R, G, B, A, with where its top left goes from the pen.
pub(super) struct Tile {
    pub(super) left: i32,
    pub(super) top: i32,
    pub(super) width: usize,
    pub(super) rgba: Vec<u8>,
}

/// Whether a font has colour glyphs at all.
pub(super) fn has_colour(font: &FontRef) -> bool {
    font.colr().is_ok() || !BitmapStrikes::new(font).is_empty()
}

/// The glyph at `px` to the em, or None when it isn't a colour one. `ink` is the text's colour, which layers may ask for.
pub(super) fn tile(font: &FontRef, glyph: u16, px: f32, ink: [u8; 3]) -> Option<Tile> {
    let id = GlyphId::new(glyph as u32);
    match ColorGlyphCollection::new(font).get(id) {
        Some(painted) => layered(font, &painted, px, ink),
        None => picture(font, id, px),
    }
}

/// Room around the pen, in ems: Noto's emoji are 1.25 em wide and reach a little past the em each way.
const LEFT: f32 = 0.5;
const UP: f32 = 1.5;
const ACROSS: f32 = 3.0;

fn layered(font: &FontRef, glyph: &ColorGlyph, px: f32, ink: [u8; 3]) -> Option<Tile> {
    let side = (px * ACROSS).ceil() as u32;
    let (left, top) = ((px * LEFT).ceil(), (px * UP).ceil());
    let upem = font.head().ok()?.units_per_em() as f32;
    let scale = px / upem;
    let palette = ColorPalettes::new(font).get(0).map_or(Vec::new(), |p| p.colors().iter().map(|c| [c.red, c.green, c.blue, c.alpha]).collect());
    let mut painter = Painter {
        outlines: font.outline_glyphs(),
        palette,
        ink,
        size: side,
        transforms: vec![Transform::from_row(scale, 0.0, 0.0, -scale, left, top)],
        clips: Vec::new(),
        layers: vec![(Pixmap::new(side, side)?, CompositeMode::SrcOver)],
    };
    glyph.paint(LocationRef::default(), &mut painter).ok()?;
    let page = painter.layers.pop()?.0;
    Some(Tile { left: -(left as i32), top: -(top as i32), width: side as usize, rgba: page.data().to_vec() })
}

fn picture(font: &FontRef, id: GlyphId, px: f32) -> Option<Tile> {
    let found = BitmapStrikes::new(font).glyph_for_size(Size::new(px), id)?;
    let BitmapData::Png(data) = found.data else { return None };
    let image = Pixmap::decode_png(data).ok()?;
    let scale = px / found.ppem_x;
    let (width, height) = (((image.width() as f32 * scale).ceil() as u32).max(1), ((image.height() as f32 * scale).ceil() as u32).max(1));
    let mut page = Pixmap::new(width, height)?;
    let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() };
    page.draw_pixmap(0, 0, image.as_ref(), &paint, Transform::from_scale(scale, scale), None);
    let bearing = found.bearing_y * px / found.ppem_y;
    let top = match found.placement_origin {
        Origin::TopLeft => -bearing,
        Origin::BottomLeft => -bearing - height as f32,
    };
    Some(Tile { left: (found.bearing_x * px / found.ppem_x).round() as i32, top: top.round() as i32, width: width as usize, rgba: page.data().to_vec() })
}

/// A glyph's outline as a path, in font units.
#[derive(Default)]
struct Outline(PathBuilder);

impl OutlinePen for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.quad_to(cx0, cy0, x, y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.cubic_to(cx0, cy0, cx1, cy1, x, y);
    }

    fn close(&mut self) {
        self.0.close();
    }
}

struct Painter<'a> {
    outlines: OutlineGlyphCollection<'a>,
    palette: Vec<[u8; 4]>,
    ink: [u8; 3],
    size: u32,
    transforms: Vec<Transform>,
    clips: Vec<Option<Mask>>,
    layers: Vec<(Pixmap, CompositeMode)>,
}

fn blend_of(mode: CompositeMode) -> BlendMode {
    use CompositeMode as C;
    match mode {
        C::Clear => BlendMode::Clear,
        C::Src => BlendMode::Source,
        C::Dest => BlendMode::Destination,
        C::DestOver => BlendMode::DestinationOver,
        C::SrcIn => BlendMode::SourceIn,
        C::DestIn => BlendMode::DestinationIn,
        C::SrcOut => BlendMode::SourceOut,
        C::DestOut => BlendMode::DestinationOut,
        C::SrcAtop => BlendMode::SourceAtop,
        C::DestAtop => BlendMode::DestinationAtop,
        C::Xor => BlendMode::Xor,
        C::Plus => BlendMode::Plus,
        C::Screen => BlendMode::Screen,
        C::Overlay => BlendMode::Overlay,
        C::Darken => BlendMode::Darken,
        C::Lighten => BlendMode::Lighten,
        C::ColorDodge => BlendMode::ColorDodge,
        C::ColorBurn => BlendMode::ColorBurn,
        C::HardLight => BlendMode::HardLight,
        C::SoftLight => BlendMode::SoftLight,
        C::Difference => BlendMode::Difference,
        C::Exclusion => BlendMode::Exclusion,
        C::Multiply => BlendMode::Multiply,
        C::HslHue => BlendMode::Hue,
        C::HslSaturation => BlendMode::Saturation,
        C::HslColor => BlendMode::Color,
        C::HslLuminosity => BlendMode::Luminosity,
        _ => BlendMode::SourceOver,
    }
}

impl Painter<'_> {
    fn transform(&self) -> Transform {
        *self.transforms.last().unwrap_or(&Transform::identity())
    }

    fn colour(&self, index: u16, alpha: f32) -> Color {
        // 0xffff is the text's own colour
        let [r, g, b, a] = self.palette.get(index as usize).copied().unwrap_or([self.ink[0], self.ink[1], self.ink[2], 255]);
        Color::from_rgba8(r, g, b, (a as f32 * alpha.clamp(0.0, 1.0)).round() as u8)
    }

    fn stops(&self, stops: &[ColorStop]) -> Vec<GradientStop> {
        stops.iter().map(|s| GradientStop::new(s.offset.clamp(0.0, 1.0), self.colour(s.palette_index, s.alpha))).collect()
    }

    fn clip_with(&mut self, path: Option<Path>) {
        let transform = self.transform();
        let last = self.clips.last().cloned().flatten();
        let next = path.and_then(|path| {
            match last {
                Some(mut mask) => {
                    mask.intersect_path(&path, FillRule::Winding, true, transform);
                    Some(mask)
                }
                None => {
                    let mut mask = Mask::new(self.size, self.size)?;
                    mask.fill_path(&path, FillRule::Winding, true, transform);
                    Some(mask)
                }
            }
        });
        self.clips.push(next);
    }

    fn shader(&self, brush: Brush<'_>) -> Option<Shader<'static>> {
        let transform = self.transform();
        let spread = |e: Extend| match e {
            Extend::Repeat => SpreadMode::Repeat,
            Extend::Reflect => SpreadMode::Reflect,
            _ => SpreadMode::Pad,
        };
        let point = |p: skrifa::raw::types::Point<f32>| Point::from_xy(p.x, p.y);
        let first = |stops: &[ColorStop]| stops.first().map(|s| self.colour(s.palette_index, s.alpha));
        match brush {
            Brush::Solid { palette_index, alpha } => Some(Shader::SolidColor(self.colour(palette_index, alpha))),
            Brush::LinearGradient { p0, p1, color_stops, extend } => {
                LinearGradient::new(point(p0), point(p1), self.stops(color_stops), spread(extend), transform).or_else(|| first(color_stops).map(Shader::SolidColor))
            }
            Brush::RadialGradient { c0, r0, c1, r1, color_stops, extend } => {
                RadialGradient::new(point(c0), r0.max(0.0), point(c1), r1.max(0.0), self.stops(color_stops), spread(extend), transform).or_else(|| first(color_stops).map(Shader::SolidColor))
            }
            Brush::SweepGradient { c0, start_angle, end_angle, color_stops, extend } => {
                SweepGradient::new(point(c0), start_angle, end_angle, self.stops(color_stops), spread(extend), transform).or_else(|| first(color_stops).map(Shader::SolidColor))
            }
        }
    }
}

impl ColorPainter for Painter<'_> {
    fn push_transform(&mut self, t: FontTransform) {
        let next = self.transform().pre_concat(Transform::from_row(t.xx, t.yx, t.xy, t.yy, t.dx, t.dy));
        self.transforms.push(next);
    }

    fn pop_transform(&mut self) {
        self.transforms.pop();
    }

    fn push_clip_glyph(&mut self, glyph: GlyphId) {
        let mut pen = Outline::default();
        let drawn = self.outlines.get(glyph).and_then(|outline| {
            let units = DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
            outline.draw(units, &mut pen).ok()
        });
        self.clip_with(drawn.and_then(|_| pen.0.finish()));
    }

    fn push_clip_box(&mut self, b: BoundingBox<f32>) {
        let path = Rect::from_ltrb(b.x_min, b.y_min, b.x_max, b.y_max).map(PathBuilder::from_rect);
        self.clip_with(path);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn fill(&mut self, brush: Brush<'_>) {
        let Some(shader) = self.shader(brush) else { return };
        let Some(whole) = Rect::from_xywh(0.0, 0.0, self.size as f32, self.size as f32) else { return };
        let paint = Paint { shader, anti_alias: false, ..Paint::default() };
        let clip = self.clips.last().cloned().flatten();
        if let Some((page, _)) = self.layers.last_mut() {
            page.fill_rect(whole, &paint, Transform::identity(), clip.as_ref());
        }
    }

    fn push_layer(&mut self, mode: CompositeMode) {
        if let Some(page) = Pixmap::new(self.size, self.size) {
            self.layers.push((page, mode));
        }
    }

    fn pop_layer(&mut self) {
        if self.layers.len() < 2 {
            return;
        }
        let Some((top, mode)) = self.layers.pop() else { return };
        let paint = PixmapPaint { blend_mode: blend_of(mode), ..PixmapPaint::default() };
        let clip = self.clips.last().cloned().flatten();
        if let Some((page, _)) = self.layers.last_mut() {
            page.draw_pixmap(0, 0, top.as_ref(), &paint, Transform::identity(), clip.as_ref());
        }
    }
}
