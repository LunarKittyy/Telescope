//! Text drawn on pictures: finding the fonts the app uses, measuring a line and putting its glyphs on pixels. The wait screen and
//! the name tag both draw this way; where the pixels go (a pixmap, a tile) is left to the caller through `Face::draw`.
//!
//! Glyphs come from ab_glyph, one font per script as the system has them: the primary font first, then the fallbacks below for
//! a character it lacks. There is no shaping, so scripts that join or reorder letters (Arabic, Indic) come out as separate glyphs.

pub use ab_glyph::FontArc;
use ab_glyph::{Font, GlyphId, PxScale, ScaleFont};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The two weights the app draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    /// Qt's DemiBold (600).
    SemiBold,
}

/// Fonts to fall back on for a character the main one lacks, by the start of the file name (lower case), most likely first.
const FALLBACKS: &[&str] = &[
    "notosans[", "notosans-regular", "dejavusans.", "notosanscjk", "notosansjp", "notosanskr", "notosanssc", "notosansarabic",
    "notosansdevanagari", "notosanshebrew", "notosansthai", "notosansbengali", "notosanstamil", "notosansgreek", "segoeui.",
    "msyh", "yugoth", "malgun", "nirmala", "arial.",
];

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = vec!["/usr/share/fonts".into(), "/usr/local/share/fonts".into()];
    if let Some(home) = std::env::home_dir() {
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
    }
    if let Some(windir) = std::env::var_os("WINDIR") {
        dirs.push(PathBuf::from(windir).join("Fonts"));
    }
    dirs
}

fn collect(dir: &Path, depth: u32, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            if depth > 0 {
                collect(&path, depth - 1, out);
            }
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let lower = name.to_ascii_lowercase();
            if lower.contains(".tt") || lower.ends_with(".otf") {
                out.push((lower, path.clone()));
            }
        }
    }
}

/// Every font file under the usual folders, by lower-case file name, in folder order.
fn font_files() -> &'static [(String, PathBuf)] {
    static FILES: OnceLock<Vec<(String, PathBuf)>> = OnceLock::new();
    FILES.get_or_init(|| {
        let mut out = Vec::new();
        for dir in font_dirs() {
            collect(&dir, 3, &mut out);
        }
        out
    })
}

/// The bytes of the first of these file names the system has (the names in order, whichever folder it is in).
pub fn find_font_file(names: &[&str]) -> Option<Vec<u8>> {
    names.iter().find_map(|name| {
        let wanted = name.to_ascii_lowercase();
        font_files().iter().find(|(file, _)| *file == wanted).and_then(|(_, path)| std::fs::read(path).ok())
    })
}

/// The first of these file names the system has, as a font.
pub fn font_named(names: &[&str]) -> Option<FontArc> {
    FontArc::try_from_vec(find_font_file(names)?).ok()
}

/// The app's own font (Inter, as the stylesheet asks for first) in this weight, else a sans the system has.
pub fn app_font(weight: Weight) -> Option<FontArc> {
    static CACHE: [OnceLock<Option<FontArc>>; 2] = [OnceLock::new(), OnceLock::new()];
    let (slot, names): (usize, &[&str]) = match weight {
        Weight::Regular => (0, &["Inter-Regular.ttf", "Inter.ttf", "DejaVuSans.ttf", "segoeui.ttf", "arial.ttf"]),
        Weight::SemiBold => (1, &["Inter-SemiBold.ttf", "Inter-Bold.ttf", "DejaVuSans-Bold.ttf", "seguisb.ttf", "segoeuib.ttf", "arialbd.ttf"]),
    };
    CACHE[slot].get_or_init(|| font_named(names)).clone()
}

/// The font that covers a character the main one doesn't, if the system has one.
fn fallback_for(ch: char) -> Option<FontArc> {
    static LOADED: OnceLock<Vec<OnceLock<Option<FontArc>>>> = OnceLock::new();
    let loaded = LOADED.get_or_init(|| FALLBACKS.iter().map(|_| OnceLock::new()).collect());
    for (i, prefix) in FALLBACKS.iter().enumerate() {
        let font = loaded[i].get_or_init(|| {
            let (_, path) = font_files().iter().find(|(file, _)| file.starts_with(prefix))?;
            FontArc::try_from_vec(std::fs::read(path).ok()?).ok()
        });
        if let Some(font) = font {
            if font.glyph_id(ch).0 != 0 {
                return Some(font.clone());
            }
        }
    }
    None
}

/// One glyph of a measured line.
#[derive(Clone)]
struct Placed {
    font: FontArc,
    glyph: GlyphId,
    /// Pen position along the line, from its start.
    x: f32,
}

/// A line of text worked out for one face: where each glyph goes, and how wide it is.
#[derive(Clone)]
pub struct Line {
    placed: Vec<Placed>,
    pub width: f32,
}

/// A font at a size, with the fallbacks for the characters it lacks.
#[derive(Clone)]
pub struct Face {
    font: FontArc,
    scale: PxScale,
    fallbacks: bool,
}

impl Face {
    /// `px` is the size of the em, which is what Qt's setPixelSize means.
    pub fn em(font: FontArc, px: f32) -> Face {
        let per_em = font.height_unscaled() / font.units_per_em().unwrap_or(1000.0);
        Face { font, scale: PxScale::from(px * per_em), fallbacks: true }
    }

    /// `px` from the top of the tallest letter to the bottom of the lowest, ab_glyph's own idea of a size; no fallbacks.
    pub fn tall(font: FontArc, px: f32) -> Face {
        Face { font, scale: PxScale::from(px), fallbacks: false }
    }

    fn scaled(&self) -> ab_glyph::PxScaleFont<&FontArc> {
        self.font.as_scaled(self.scale)
    }

    /// Above the baseline, in pixels.
    pub fn ascent(&self) -> f32 {
        self.scaled().ascent()
    }

    /// Below the baseline, in pixels (positive).
    pub fn descent(&self) -> f32 {
        -self.scaled().descent()
    }

    /// Ascent plus descent.
    pub fn height(&self) -> f32 {
        self.ascent() + self.descent()
    }

    /// The text on one line; `spacing` is added between letters (negative to tighten).
    pub fn layout(&self, text: &str, spacing: f32) -> Line {
        let mut x = 0.0;
        let mut placed = Vec::new();
        // The glyph before, when it came from the main font (kerning is only worked out inside one font)
        let mut last: Option<GlyphId> = None;
        let mut started = false;
        for ch in text.chars() {
            let mut font = self.font.clone();
            let mut id = font.glyph_id(ch);
            let mut own = true;
            if id.0 == 0 && self.fallbacks {
                if let Some(other) = fallback_for(ch) {
                    id = other.glyph_id(ch);
                    font = other;
                    own = false;
                }
            }
            let scaled = font.as_scaled(self.scale);
            if started {
                if let (Some(prev), true) = (last, own) {
                    x += scaled.kern(prev, id);
                }
                x += spacing;
            }
            placed.push(Placed { font: font.clone(), glyph: id, x });
            x += scaled.h_advance(id);
            last = own.then_some(id);
            started = true;
        }
        Line { placed, width: x }
    }

    /// How wide the text is on one line.
    pub fn advance(&self, text: &str) -> f32 {
        self.layout(text, 0.0).width
    }

    /// The text cut short with an ellipsis so it fits in `width` (QFontMetricsF.elidedText with ElideRight).
    pub fn elide(&self, text: &str, width: f32) -> String {
        if self.advance(text) <= width {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        for keep in (0..chars.len()).rev() {
            let cut: String = chars[..keep].iter().collect::<String>() + "\u{2026}";
            if self.advance(&cut) <= width {
                return cut;
            }
        }
        "\u{2026}".into()
    }

    /// Calls `cover(x, y, coverage)` for each pixel the line touches, with its start at `x` and its baseline at `baseline`.
    /// Pixels may be reported more than once where glyphs overlap.
    pub fn draw(&self, line: &Line, x: f32, baseline: f32, mut cover: impl FnMut(i32, i32, f32)) {
        for p in &line.placed {
            let glyph = p.glyph.with_scale_and_position(self.scale, ab_glyph::point(x + p.x, baseline));
            let Some(outlined) = p.font.outline_glyph(glyph) else { continue };
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, c| cover(bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32, c));
        }
    }
}

/// Whether text starts out right to left (its first strongly directional character is Arabic, Hebrew and the like).
pub fn right_to_left(text: &str) -> bool {
    for ch in text.chars() {
        match ch as u32 {
            0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF => return true,
            _ if ch.is_alphabetic() => return false,
            _ => {}
        }
    }
    false
}

/// The text in the order its glyphs are drawn left to right: reversed for a right-to-left line.
pub fn visual_order(text: &str) -> String {
    if right_to_left(text) { text.chars().rev().collect() } else { text.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(px: f32) -> Option<Face> {
        let face = app_font(Weight::Regular).map(|f| Face::em(f, px));
        if face.is_none() {
            eprintln!("no font on this machine: text measuring is skipped");
        }
        face
    }

    #[test]
    fn text_is_wider_the_more_of_it_and_the_bigger() {
        let Some(small) = face(20.0) else { return };
        let big = face(40.0).unwrap();
        assert!(small.advance("Luna Kozankiewicz") > small.advance("Luna"));
        let ratio = big.advance("Luna") / small.advance("Luna");
        assert!((ratio - 2.0).abs() < 0.02, "{ratio}");
        assert!(small.height() > 20.0 && small.ascent() > small.descent());
    }

    #[test]
    fn a_long_line_is_cut_with_an_ellipsis_to_fit() {
        let Some(f) = face(30.0) else { return };
        let text = "Wolfeschlegelsteinhausenbergerdorff";
        let cut = f.elide(text, 200.0);
        assert!(cut.ends_with('\u{2026}') && f.advance(&cut) <= 200.0 && cut.len() < text.len());
        assert_eq!(f.elide("Luna", 200.0), "Luna");
    }

    #[test]
    fn drawing_reports_pixels_inside_the_line() {
        let Some(f) = face(30.0) else { return };
        let line = f.layout("Hi", 0.0);
        let (mut hits, mut max_x) = (0, 0);
        f.draw(&line, 10.0, 40.0, |x, y, c| {
            if c > 0.5 {
                hits += 1;
                max_x = max_x.max(x);
                assert!(y > 40 - 35 && y < 40 + 2, "{y}");
            }
        });
        assert!(hits > 20 && max_x as f32 <= 10.0 + line.width + 1.0);
    }

    #[test]
    fn direction_comes_from_the_first_strong_letter() {
        assert!(right_to_left("\u{633}\u{627}\u{631}\u{629}"));
        assert!(!right_to_left("Luna") && !right_to_left("\u{5c71}\u{7530}") && !right_to_left("   "));
        assert!(right_to_left("  \u{647}\u{64a}"));
        assert_eq!(visual_order("abc"), "abc");
        assert_eq!(visual_order("\u{5d0}\u{5d1}"), "\u{5d1}\u{5d0}");
    }
}
