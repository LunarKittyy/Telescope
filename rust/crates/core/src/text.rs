//! Text drawn on pictures: finding the fonts the app uses, measuring a line and putting its glyphs on pixels. The wait screen and
//! the name tag both draw this way; where the pixels go (a pixmap, a tile) is left to the caller through `Face::draw`.
//!
//! A line is split into runs by direction (unicode-bidi) and by which font covers its characters (the primary font first, then the
//! system's font for the character's script, then the list of names below for a character it lacks), each run is shaped with its font (harfrust, so Arabic joins, Indic marks land where
//! they belong and kerning applies) and the glyphs are placed from the shaped advances and offsets, runs in visual order.

mod shape;
mod system;

pub use shape::FontArc;
use ab_glyph::{Font, GlyphId, PxScale, ScaleFont};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use unicode_bidi::{bidi_class, BidiClass, BidiInfo, Level};
use unicode_script::{Script, UnicodeScript};
use unicode_segmentation::UnicodeSegmentation;

/// The two weights the app draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    /// Qt's DemiBold (600).
    SemiBold,
}

/// Fonts to fall back on for a character neither the main font nor the system's fonts for its script have, by the start of the file
/// name (lower case), most likely first.
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

/// A font from its bytes; a variable one is set to this weight (400 regular, 600 demi-bold), else its default look.
fn load(bytes: Vec<u8>, weight: f32) -> Option<FontArc> {
    FontArc::from_bytes(bytes, weight)
}

/// The first of these file names the system has, as a font.
pub fn font_named(names: &[&str]) -> Option<FontArc> {
    load(find_font_file(names)?, 400.0)
}

/// QFontMetrics.averageCharWidth for the app's font at the body size (13px).
pub const BODY_AVERAGE_CHAR: f32 = 8.0;

/// The app's own font (Inter, as the stylesheet asks for first) in this weight, else a sans the system has.
pub fn app_font(weight: Weight) -> Option<FontArc> {
    static CACHE: [OnceLock<Option<FontArc>>; 2] = [OnceLock::new(), OnceLock::new()];
    let (slot, names): (usize, &[&str]) = match weight {
        Weight::Regular => (0, &["Inter-Regular.ttf", "Inter.ttf", "DejaVuSans.ttf", "segoeui.ttf", "arial.ttf"]),
        Weight::SemiBold => (1, &["Inter-SemiBold.ttf", "Inter-Bold.ttf", "DejaVuSans-Bold.ttf", "seguisb.ttf", "segoeuib.ttf", "arialbd.ttf"]),
    };
    let wght = if weight == Weight::SemiBold { 600.0 } else { 400.0 };
    CACHE[slot].get_or_init(|| load(find_font_file(names)?, wght)).clone()
}

/// The font that covers a character the main one doesn't, if the system has one: what it offers for the character's script (fontconfig
/// on Linux, DirectWrite on Windows), else the first of the list above that has it.
fn fallback_for(ch: char, heavy: bool) -> Option<FontArc> {
    system::font_for(ch, if heavy { 600.0 } else { 400.0 }).or_else(|| listed_fallback(ch, heavy))
}

fn listed_fallback(ch: char, heavy: bool) -> Option<FontArc> {
    static LOADED: [OnceLock<Vec<OnceLock<Option<FontArc>>>>; 2] = [OnceLock::new(), OnceLock::new()];
    let loaded = LOADED[usize::from(heavy)].get_or_init(|| FALLBACKS.iter().map(|_| OnceLock::new()).collect());
    for (i, prefix) in FALLBACKS.iter().enumerate() {
        let font = loaded[i].get_or_init(|| {
            let (_, path) = font_files().iter().find(|(file, _)| file.starts_with(prefix))?;
            load(std::fs::read(path).ok()?, if heavy { 600.0 } else { 400.0 })
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
    scale: PxScale,
    glyph: GlyphId,
    /// Pen position along the line, from its start, with the glyph's own offset (a mark over its base) in.
    x: f32,
    /// How far the glyph sits below the baseline (negative: above).
    y: f32,
    /// Where its cluster starts in the text (a byte offset).
    cluster: usize,
    /// How far the pen moved for it, before any letter spacing.
    advance: f32,
}

/// A line of text worked out for one face: where each glyph goes (left to right, whatever the text's direction), and how wide it is.
#[derive(Clone)]
pub struct Line {
    placed: Vec<Placed>,
    pub width: f32,
}

impl Line {
    /// Where in the text each glyph's cluster starts, left to right as drawn.
    pub fn clusters(&self) -> Vec<usize> {
        self.placed.iter().map(|p| p.cluster).collect()
    }

    /// The glyph ids, left to right as drawn.
    pub fn glyph_ids(&self) -> Vec<u16> {
        self.placed.iter().map(|p| p.glyph.0).collect()
    }
}

/// A font at a size, with the fallbacks for the characters it lacks.
#[derive(Clone)]
pub struct Face {
    font: FontArc,
    size: f32,
    /// `size` is the em (every font gets the same), else ab_glyph's own height-based scale.
    em: bool,
    fallbacks: bool,
    /// Fallbacks for other scripts are demi-bold too.
    heavy: bool,
}

/// Characters that take the font of their neighbours when it has them: spaces, punctuation, marks and joiners.
fn takes_neighbours_font(ch: char) -> bool {
    use BidiClass::*;
    matches!(bidi_class(ch), WS | ON | CS | ES | ET | S | B | NSM | BN)
}

/// The script each character is written in: spaces, punctuation, digits and marks (Common and Inherited) take the script of the
/// letter before them, or of the first letter after them when the text starts with some.
fn script_runs(chars: impl Iterator<Item = char>) -> Vec<Script> {
    let mut scripts: Vec<Script> = chars.map(|c| c.script()).collect();
    let plain = |s: Script| s == Script::Common || s == Script::Inherited;
    let mut before = scripts.iter().copied().find(|&s| !plain(s)).unwrap_or(Script::Common);
    for s in scripts.iter_mut() {
        if plain(*s) {
            *s = before;
        } else {
            before = *s;
        }
    }
    scripts
}

/// Marks and joiners that belong to the letter before them.
fn is_mark(ch: char) -> bool {
    matches!(bidi_class(ch), BidiClass::NSM | BidiClass::BN)
}

impl Face {
    /// `px` is the size of the em, which is what Qt's setPixelSize means.
    pub fn em(font: FontArc, px: f32) -> Face {
        Face { font, size: px, em: true, fallbacks: true, heavy: false }
    }

    /// The fallbacks for other scripts in demi-bold, like the font this face was made from.
    pub fn heavy(mut self) -> Face {
        self.heavy = true;
        self
    }

    /// `px` from the top of the tallest letter to the bottom of the lowest, ab_glyph's own idea of a size; no fallbacks.
    pub fn tall(font: FontArc, px: f32) -> Face {
        Face { font, size: px, em: false, fallbacks: false, heavy: false }
    }

    /// Characters this font lacks come from the fallbacks for other scripts, at the same em.
    pub fn with_fallbacks(mut self) -> Face {
        self.fallbacks = true;
        self
    }

    /// The scale that makes this face's size in a font: the same em in every font, so a fallback matches its neighbours.
    fn scale_in(&self, font: &FontArc) -> PxScale {
        if self.em {
            PxScale::from(self.size * font.height_unscaled() / font.units_per_em().unwrap_or(1000.0))
        } else {
            // A tall size is this face's font's own; a fallback gets the same em
            let em = |f: &FontArc| f.units_per_em().unwrap_or(1000.0) / f.height_unscaled();
            PxScale::from(self.size * em(&self.font) / em(font))
        }
    }

    fn scaled(&self) -> ab_glyph::PxScaleFont<&ab_glyph::FontArc> {
        self.font.as_scaled(self.scale_in(&self.font))
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

    /// The font that has this character: the main one, else a fallback.
    fn pick(&self, ch: char) -> Option<FontArc> {
        if self.font.glyph_id(ch).0 != 0 {
            return Some(self.font.clone());
        }
        if self.fallbacks { fallback_for(ch, self.heavy) } else { None }
    }

    /// `run` split into stretches that one font covers and one script writes, in text order (Qt itemizes by script too, so Hebrew
    /// followed by Arabic is shaped as each, not both as the first). A space or a mark stays in its neighbours' font when that
    /// has it, so one Arabic phrase is not cut into a piece per word.
    fn pieces(&self, text: &str, run: Range<usize>) -> Vec<(Range<usize>, FontArc)> {
        let chars: Vec<(usize, char)> = text[run.clone()].char_indices().map(|(i, c)| (run.start + i, c)).collect();
        let weak: Vec<bool> = chars.iter().map(|&(_, c)| takes_neighbours_font(c)).collect();
        let picked: Vec<Option<FontArc>> = chars.iter().zip(&weak).map(|(&(_, c), &w)| if w { None } else { self.pick(c) }).collect();
        // The font of the next letter at or after each character; none when there is no such letter or no font has it
        let mut ahead: Vec<Option<&FontArc>> = vec![None; chars.len()];
        let mut next = None;
        for i in (0..chars.len()).rev() {
            if !weak[i] {
                next = picked[i].as_ref();
            }
            ahead[i] = next;
        }
        let scripts = script_runs(chars.iter().map(|&(_, c)| c));
        let mut out: Vec<(Range<usize>, FontArc)> = Vec::new();
        let mut last_script = None;
        let mut cur: Option<FontArc> = None;
        for (i, &(at, ch)) in chars.iter().enumerate() {
            let font = if weak[i] {
                let stay = cur.as_ref().filter(|c| c.glyph_id(ch).0 != 0 && (is_mark(ch) || ahead[i].is_none_or(|n| n.same(c))));
                stay.cloned().or_else(|| self.pick(ch))
            } else {
                picked[i].clone()
            };
            let font = font.or_else(|| cur.clone()).unwrap_or_else(|| self.font.clone());
            cur = Some(font.clone());
            let end = at + ch.len_utf8();
            match out.last_mut() {
                Some((range, last)) if last.same(&font) && last_script == Some(scripts[i]) => range.end = end,
                _ => out.push((at..end, font)),
            }
            last_script = Some(scripts[i]);
        }
        out
    }

    /// The text on one line, in the order it reads (right to left when it starts with a right-to-left letter, with the
    /// Latin and digits inside it still left to right); `spacing` is added after each letter (negative to tighten).
    pub fn layout(&self, text: &str, spacing: f32) -> Line {
        let mut placed = Vec::new();
        let mut x = 0.0;
        if !text.is_empty() {
            let base = if right_to_left(text) { Level::rtl() } else { Level::ltr() };
            let bidi = BidiInfo::new(text, Some(base));
            for para in &bidi.paragraphs {
                let (levels, runs) = bidi.visual_runs(para, para.range.clone());
                for run in runs {
                    let rtl = levels[run.start].is_rtl();
                    let mut pieces = self.pieces(text, run);
                    if rtl {
                        pieces.reverse();
                    }
                    for (range, font) in pieces {
                        let scale = self.scale_in(&font);
                        let per_unit = scale.x / font.height_unscaled();
                        let mut last = None;
                        for g in font.shape(text, range, rtl, spacing != 0.0) {
                            // Letters are spaced apart by cluster (the space after the one before); a mark rides on its base
                            if last != Some(g.cluster) && !placed.is_empty() {
                                x += spacing;
                            }
                            last = Some(g.cluster);
                            placed.push(Placed {
                                font: font.clone(),
                                scale,
                                glyph: GlyphId(g.glyph),
                                x: x + g.x_offset as f32 * per_unit,
                                y: -(g.y_offset as f32) * per_unit,
                                cluster: g.cluster,
                                advance: g.x_advance as f32 * per_unit,
                            });
                            x += g.x_advance as f32 * per_unit;
                        }
                    }
                }
            }
        }
        // Qt spaces the last letter too, which only shows in how wide the line is
        if !placed.is_empty() {
            x += spacing;
        }
        Line { placed, width: x }
    }

    /// Whether the font, or a fallback for it, has a glyph for this character.
    pub fn has_glyph(&self, ch: char) -> bool {
        self.pick(ch).is_some()
    }

    /// How wide the text is on one line.
    pub fn advance(&self, text: &str) -> f32 {
        self.layout(text, 0.0).width
    }

    /// The text word-wrapped at `width`: how many lines, and how wide the widest is.
    fn wrapped(&self, text: &str, width: f32) -> (u32, f32) {
        let space = self.advance(" ");
        let (mut lines, mut line, mut widest) = (u32::from(!text.is_empty()), 0.0f32, 0.0f32);
        for word in text.split_whitespace() {
            let w = self.advance(word);
            if line > 0.0 && line + space + w > width {
                lines += 1;
                widest = widest.max(line);
                line = w;
            } else {
                line += if line > 0.0 { space + w } else { w };
            }
        }
        (lines, widest.max(line))
    }

    /// How many lines a word-wrapped QLabel's sizeHint gives the text, which it has to guess a width for: 80 average characters,
    /// half that when it fits in under 4 lines, half again under 2 (QLabelPrivate::sizeForWidth). average_char is
    /// QFontMetrics.averageCharWidth.
    pub fn qt_hint_lines(&self, text: &str, average_char: f32) -> u32 {
        let mut width = average_char * 80.0;
        let (mut lines, mut widest) = self.wrapped(text, width);
        if lines < 4 && widest > width / 2.0 {
            width /= 2.0;
            (lines, widest) = self.wrapped(text, width);
        }
        if lines < 2 && widest > width / 2.0 {
            (lines, _) = self.wrapped(text, width / 2.0);
        }
        lines
    }

    /// How wide the ellipsis is: the main font's own glyph, else a fallback's.
    fn ellipsis_width(&self) -> f32 {
        self.advance("\u{2026}")
    }

    /// The text cut short with an ellipsis so it fits in `width` (QFontMetricsF.elidedText with ElideRight, which is
    /// QStackTextEngine::elidedText): the end of the text goes, which is the left side of a right-to-left line. It is cut between
    /// graphemes, never through a letter and its marks. Qt adds up the width each grapheme has in the whole line (kerning and
    /// joining included) until the sum reaches what is left after the ellipsis, and keeps what came before the grapheme that reached it.
    pub fn elide(&self, text: &str, width: f32) -> String {
        let line = self.layout(text, 0.0);
        // Qt leaves a text of one UTF-16 unit whole, however narrow
        if line.width <= width || text.encode_utf16().count() <= 1 {
            return text.to_string();
        }
        // and drops all of it when not even the ellipsis fits
        let available = width - self.ellipsis_width();
        if available < 0.0 {
            return String::new();
        }
        let starts: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
        // A glyph's advance goes to the grapheme its cluster starts in; a ligature's other graphemes have no width of their own
        let mut widths = vec![0.0f32; starts.len()];
        for p in &line.placed {
            let grapheme = starts.partition_point(|&s| s <= p.cluster).saturating_sub(1);
            widths[grapheme] += p.advance;
        }
        let (mut taken, mut sum) = (0, 0.0);
        let at = loop {
            let at = taken;
            sum += widths[at];
            taken += 1;
            if taken >= starts.len() || sum >= available {
                break at;
            }
        };
        // When the first letter cut off would have joined to the one before, a joiner keeps that letter in its joining form
        let joiner = if next_char_joins(text, starts[at]) { "\u{200d}" } else { "" };
        format!("{}{joiner}\u{2026}", &text[..starts[at]])
    }

    /// Calls `cover(x, y, coverage)` for each pixel the line touches, with its start at `x` and its baseline at `baseline`.
    /// Pixels may be reported more than once where glyphs overlap.
    pub fn draw(&self, line: &Line, x: f32, baseline: f32, mut cover: impl FnMut(i32, i32, f32)) {
        for p in &line.placed {
            let glyph = p.glyph.with_scale_and_position(p.scale, ab_glyph::point(x + p.x, baseline + p.y));
            let Some(outlined) = p.font.outline_glyph(glyph) else { continue };
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, c| cover(bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32, c));
        }
    }
}

/// Whether the letter at `pos`, past any marks, joins to the one before it (Qt's nextCharJoins).
fn next_char_joins(text: &str, pos: usize) -> bool {
    use unicode_joining_type::{get_joining_type, JoiningType};
    let Some(ch) = text[pos..].chars().find(|&c| bidi_class(c) != BidiClass::NSM) else { return false };
    matches!(get_joining_type(ch), JoiningType::DualJoining | JoiningType::JoinCausing)
}

/// Whether text starts out right to left (its first strongly directional character is Arabic, Hebrew and the like), by bidi class
/// like Python's unicodedata.bidirectional: an Arabic comma or digit is not strong.
pub fn right_to_left(text: &str) -> bool {
    for ch in text.chars() {
        match bidi_class(ch) {
            BidiClass::R | BidiClass::AL => return true,
            BidiClass::L => return false,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests;
