//! Qt's way of finding a fallback font on Linux (QFontconfigDatabase::fallbacksForFamily): fontconfig sorts every installed font for
//! sans-serif in the script's language, and the first font in that order that has the character is the one used.

use crate::text::FontArc;
use fontconfig_sys as fc;
use fontconfig_sys::constants::{FC_CHARSET, FC_FAMILY, FC_FILE, FC_INDEX, FC_SLANT, FC_WEIGHT};
use std::collections::HashMap;
use std::ffi::{c_int, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::{Mutex, OnceLock};
use unicode_script::{Script, UnicodeScript};

/// The language Qt asks fontconfig for in a script (the language of its writing system), none for a script Qt has no writing system for.
fn language(script: Script) -> Option<&'static str> {
    use Script::*;
    Some(match script {
        Latin => "en",
        Greek => "el",
        Cyrillic => "ru",
        Armenian => "hy",
        Hebrew => "he",
        Arabic => "ar",
        Syriac => "syr",
        Thaana => "dv",
        Devanagari => "hi",
        Bengali => "bn",
        Gurmukhi => "pa",
        Gujarati => "gu",
        Oriya => "or",
        Tamil => "ta",
        Telugu => "te",
        Kannada => "kn",
        Malayalam => "ml",
        Sinhala => "si",
        Thai => "th",
        Lao => "lo",
        Tibetan => "bo",
        Myanmar => "my",
        Georgian => "ka",
        Khmer => "km",
        Ethiopic => "am",
        Hiragana | Katakana => "ja",
        Hangul => "ko",
        Bopomofo => "zh-tw",
        Ogham => "sga",
        Runic => "non",
        Nko => "nqo",
        _ => return None,
    })
}

/// fontconfig's weight on the OpenType scale (FcWeightToOpenType).
fn open_type_weight(fc_weight: c_int) -> f32 {
    const MAP: [(f32, f32); 13] = [
        (0.0, 100.0), (40.0, 200.0), (50.0, 300.0), (55.0, 350.0), (75.0, 380.0), (80.0, 400.0), (100.0, 500.0), (180.0, 600.0),
        (200.0, 700.0), (205.0, 800.0), (210.0, 900.0), (215.0, 1000.0), (1000.0, 1000.0),
    ];
    let w = fc_weight as f32;
    let at = MAP.iter().position(|&(f, _)| f >= w).unwrap_or(MAP.len() - 1);
    if at == 0 || MAP[at].0 == w {
        return MAP[at].1;
    }
    let ((f0, o0), (f1, o1)) = (MAP[at - 1], MAP[at]);
    o0 + (o1 - o0) * (w - f0) / (f1 - f0)
}

/// The characters a font has, owned by the sorted set that is kept for the life of the program.
struct Charset(*const fc::FcCharSet);

// SAFETY: fontconfig's charsets are immutable once built and the set that owns them is never freed.
unsafe impl Send for Charset {}
unsafe impl Sync for Charset {}

impl Charset {
    fn has(&self, ch: char) -> bool {
        // SAFETY: the pointer is a live charset of a font in the sorted set
        unsafe { fc::FcCharSetHasChar(self.0, ch as u32) != 0 }
    }
}

/// One upright font file of a family.
struct Style {
    file: PathBuf,
    /// Which font of a collection, and the weight a variable font is set to (its named instance's).
    index: u32,
    weight: f32,
    covers: Charset,
    loaded: OnceLock<Option<FontArc>>,
}

/// Every family fontconfig lists for a language, best first, each with its upright styles.
struct Sorted(Vec<Vec<Style>>);

fn string(pattern: *mut fc::FcPattern, object: &CStr) -> Option<String> {
    let mut out: *mut fc::FcChar8 = null_mut();
    // SAFETY: the pattern is a live pattern of the sorted set; the string it gives back lives as long as the pattern does
    let ok = unsafe { fc::FcPatternGetString(pattern, object.as_ptr(), 0, &mut out) } == fc::FcResultMatch;
    // SAFETY: fontconfig strings end in a nul
    (ok && !out.is_null()).then(|| unsafe { CStr::from_ptr(out as *const _) }.to_string_lossy().into_owned())
}

fn integer(pattern: *mut fc::FcPattern, object: &CStr) -> Option<c_int> {
    let mut out: c_int = 0;
    // SAFETY: the pattern is live; a value that is a range (a variable font's own entry) is a mismatch, not an integer
    (unsafe { fc::FcPatternGetInteger(pattern, object.as_ptr(), 0, &mut out) } == fc::FcResultMatch).then_some(out)
}

/// Fontconfig's sorted list for sans-serif in this language, the way Qt asks for it.
fn sort(lang: Option<&str>) -> Sorted {
    let mut families: Vec<(String, Vec<Style>)> = Vec::new();
    // SAFETY: plain fontconfig calls on a pattern made here; the sorted set it returns is deliberately never freed, since the
    // charsets the styles point into belong to it
    unsafe {
        let pattern = fc::FcPatternCreate();
        if pattern.is_null() {
            return Sorted(Vec::new());
        }
        fc::FcPatternAddString(pattern, FC_FAMILY.as_ptr(), c"sans-serif".as_ptr() as *const fc::FcChar8);
        if let Some(lang) = lang.and_then(|l| CString::new(l).ok()) {
            let set = fc::FcLangSetCreate();
            if !set.is_null() {
                fc::FcLangSetAdd(set, lang.as_ptr() as *const fc::FcChar8);
                fc::FcPatternAddLangSet(pattern, fc::constants::FC_LANG.as_ptr(), set);
                fc::FcLangSetDestroy(set);
            }
        }
        fc::FcConfigSubstitute(null_mut(), pattern, fc::FcMatchPattern);
        fc::FcDefaultSubstitute(pattern);
        let (mut coverage, mut result) = (null_mut(), 0);
        let found = fc::FcFontSort(null_mut(), pattern, 0, &mut coverage, &mut result);
        fc::FcPatternDestroy(pattern);
        if found.is_null() {
            return Sorted(Vec::new());
        }
        for i in 0..(*found).nfont.max(0) as usize {
            let font = *(*found).fonts.add(i);
            // Italics are left out, and so is a variable font's own entry (it has no one weight): its named instances are listed too
            let (Some(name), Some(file), Some(weight)) = (string(font, FC_FAMILY), string(font, FC_FILE), integer(font, FC_WEIGHT)) else { continue };
            if integer(font, FC_SLANT).unwrap_or(0) > 0 {
                continue;
            }
            let mut charset = null_mut();
            if fc::FcPatternGetCharSet(font, FC_CHARSET.as_ptr(), 0, &mut charset) != fc::FcResultMatch || charset.is_null() {
                continue;
            }
            // The low half of the index is the font in a collection, the high half the named instance of a variable font
            let index = integer(font, FC_INDEX).unwrap_or(0) as u32 & 0xffff;
            let style = Style { file: PathBuf::from(std::ffi::OsStr::from_bytes(file.as_bytes())), index, weight: open_type_weight(weight), covers: Charset(charset), loaded: OnceLock::new() };
            match families.iter_mut().find(|(n, _)| *n == name) {
                Some((_, styles)) => styles.push(style),
                None => families.push((name, vec![style])),
            }
        }
    }
    Sorted(families.into_iter().map(|(_, styles)| styles).collect())
}

/// The style of a family nearest this weight (Qt's QFontDatabase::bestStyle compares the distance), the first listed on a tie.
fn nearest(styles: &[Style], weight: f32) -> &Style {
    styles.iter().reduce(|best, s| if (s.weight - weight).abs() < (best.weight - weight).abs() { s } else { best }).expect("a family has a style")
}

/// The style of the first family fontconfig lists for this character's script that has the character.
fn pick(ch: char, _weight: f32) -> Option<&'static Style> {
    static SORTED: OnceLock<Mutex<HashMap<Option<&'static str>, &'static Sorted>>> = OnceLock::new();
    let lang = language(ch.script());
    let sorted: &'static Sorted = {
        let mut cache = SORTED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        *cache.entry(lang).or_insert_with(|| Box::leak(Box::new(sort(lang))))
    };
    sorted.0.iter().filter(|styles| !styles.is_empty()).map(|styles| nearest(styles, 400.0)).find(|style| style.covers.has(ch))
}

/// The system's own font for this character in this weight, as Qt would pick it when the app's fonts lack it.
pub(in crate::text) fn font_for(ch: char, weight: f32) -> Option<FontArc> {
    let style = pick(ch, weight)?;
    style.loaded.get_or_init(|| FontArc::from_bytes_at(std::fs::read(&style.file).ok()?, style.index, style.weight)).clone()
}

/// Which file, and which font in it, `font_for` takes for this character.
#[cfg(test)]
pub(in crate::text) fn file_for(ch: char, weight: f32) -> Option<(PathBuf, u32)> {
    pick(ch, weight).map(|s| (s.file.clone(), s.index))
}
