//! Translating shown text: tr() where it's shown, n_() to mark text kept in English until then (telescope.i18n).
//!
//! The English is written in the code and is the fallback for anything not translated yet. start() loads the language Settings
//! picked (or the system's) from the catalogs the program was given (use_source), and from then on tr(), tr_ctx(), trn(), the Slint
//! `@tr` texts the shell puts through tr(), and number() all follow it.

pub mod catalog;
pub mod lang;
pub mod po;

pub use catalog::{pseudo, Catalog, Source, PSEUDO_GROWTH};
pub use lang::{native_name, pick, system_languages, ENV_LANGUAGE, PSEUDO_LANGUAGE, SOURCE_LANGUAGE};

use std::fmt::Display;
use std::sync::{Arc, RwLock};

type Translator = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

static TRANSLATOR: RwLock<Option<Translator>> = RwLock::new(None);
static CATALOG: RwLock<Option<Arc<Catalog>>> = RwLock::new(None);
static SOURCE: RwLock<Option<Source>> = RwLock::new(None);

/// A lookup of text by its key, in place of a loaded catalog (for tests); until one is set, and for missing entries, text stays English.
pub fn set_translator(lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static) {
    *TRANSLATOR.write().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(lookup));
}

/// The translation of a text by its key, None for English.
fn lookup(key: &str) -> Option<String> {
    if let Some(t) = TRANSLATOR.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return t(key);
    }
    let guard = CATALOG.read().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().and_then(|c| c.get(key)).and_then(|forms| forms.first()).filter(|t| !t.is_empty()).cloned()
}

/// The text in the current language. The msgid is the English text, word for word as in the Python app.
pub fn tr(msgid: &str) -> String {
    lookup(msgid).unwrap_or_else(|| msgid.to_string())
}

/// A text that has a context in the Python app (tr(text, ctx="search")): looked up as gettext does, "ctx\u{4}text".
pub fn tr_ctx(ctx: &str, msgid: &str) -> String {
    lookup(&po::key(ctx, msgid)).unwrap_or_else(|| msgid.to_string())
}

/// Marks text for extraction without translating it yet (N_ in Python).
pub const fn n_(msgid: &str) -> &str {
    msgid
}

/// text with each {name} replaced by its value; {{ and }} stand for braces (str.format). An Err says what didn't fit, which is a
/// {name} with no value or a lone brace.
pub fn fill(text: &str, fields: &[(&str, &dyn Display)]) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(['{', '}']) {
        out += &rest[..i];
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push(tail.as_bytes()[0] as char);
            rest = &tail[2..];
            continue;
        }
        if tail.starts_with('}') {
            return Err(format!("a lone }} in {text:?}"));
        }
        let end = tail.find('}').ok_or_else(|| format!("an unclosed {{ in {text:?}"))?;
        let name = &tail[1..end];
        let (_, value) = fields.iter().find(|(n, _)| *n == name).ok_or_else(|| format!("no value for {{{name}}} in {text:?}"))?;
        out += &value.to_string();
        rest = &tail[end + 1..];
    }
    out += rest;
    Ok(out)
}

/// _fill: the text with its fields in, or the English when the translation has a field the call doesn't. No fields, no filling.
fn fill_or(text: &str, fallback: &str, fields: &[(&str, &dyn Display)]) -> String {
    if fields.is_empty() {
        return text.to_string();
    }
    fill(text, fields).unwrap_or_else(|_| {
        log::warn!("Translation of {fallback:?} has bad fields, using English");
        fill(fallback, fields).unwrap_or_else(|e| {
            log::error!("Text {fallback:?} doesn't take the fields it was given: {e}");
            fallback.to_string()
        })
    })
}

/// tr(text, ctx=ctx, **fields): the text in the current language with its {fields} filled in; English when the translation can't take them.
pub fn tr_with(msgid: &str, ctx: &str, fields: &[(&str, &dyn Display)]) -> String {
    let text = lookup(&po::key(ctx, msgid));
    fill_or(text.as_deref().unwrap_or(msgid), msgid, fields)
}

/// The translated form of a text with plural forms that fits n, None for English (no translation, or none for that form).
fn plural_form(singular: &str, plural: &str, n: i64, ctx: &str) -> Option<String> {
    if TRANSLATOR.read().unwrap_or_else(|e| e.into_inner()).is_some() {
        return lookup(&po::key(ctx, if n == 1 { singular } else { plural }));
    }
    let guard = CATALOG.read().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().and_then(|c| {
        let forms = c.get(&po::key(ctx, singular))?;
        forms.get(c.form(n)?).filter(|t| !t.is_empty()).cloned()
    })
}

/// trn(singular, plural, n, ctx=ctx, **fields): the form that fits n, with {n} and the other fields filled in.
pub fn trn_with(singular: &str, plural: &str, n: i64, ctx: &str, fields: &[(&str, &dyn Display)]) -> String {
    let mut all: Vec<(&str, &dyn Display)> = vec![("n", &n)];
    all.extend_from_slice(fields);
    let english = if n == 1 { singular } else { plural };
    fill_or(plural_form(singular, plural, n, ctx).as_deref().unwrap_or(english), english, &all)
}

/// The form that fits n with only {n} filled in, for a text with other fields the caller fills in afterwards.
pub fn trn_open(singular: &str, plural: &str, n: i64) -> String {
    let english = if n == 1 { singular } else { plural };
    plural_form(singular, plural, n, "").unwrap_or_else(|| english.to_string()).replace("{n}", &n.to_string())
}

/// trn(singular, plural, n): the form that fits n, with {n} filled in.
pub fn trn(singular: &str, plural: &str, n: i64) -> String {
    trn_with(singular, plural, n, "", &[])
}

/// The English a shown text was translated from (itself if it isn't a translation). Where a text is kept under a key that mustn't
/// change with the language, the code holds the English itself; this looks up the English of any other, which picks the first
/// where two texts are translated alike.
pub fn english(shown: &str) -> String {
    let guard = CATALOG.read().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().and_then(|c| c.english(shown)).unwrap_or(shown).to_string()
}

/// ["A", "B", "C"] as "A, B and C" in the current language.
pub fn and_list(words: &[String]) -> String {
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => {
            // Translators: what goes between the items of a list but the last two, as in "Camera, Microphone and Preview"
            let between = tr(", ");
            // Translators: the end of a list, as in "Camera, Microphone and Preview"
            tr_with("{items} and {last}", "", &[("items", &rest.join(&between)), ("last", last)])
        }
    }
}

/// The text with a capital first letter, for one that can start with a filled-in field like "the phone".
pub fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

static LANGUAGE: RwLock<String> = RwLock::new(String::new());

/// Says which language is loaded ("en", "de", "pt_BR"...), which number() formats by. load() does it for the loaded language.
pub fn set_language(code: &str) {
    *LANGUAGE.write().unwrap_or_else(|e| e.into_inner()) = code.to_string();
}

/// The language loaded, "en" when nothing is translated.
pub fn language() -> String {
    let guard = LANGUAGE.read().unwrap_or_else(|e| e.into_inner());
    if guard.is_empty() { "en".into() } else { guard.clone() }
}

/// A number as shown in the current language: "1.0" is "1,0" in Swedish (telescope.i18n.number). Only for what's shown;
/// saved settings and what's sent to the phone stay in plain formatting.
pub fn number(value: f64, decimals: usize, signed: bool) -> String {
    let text = if signed { format!("{value:+.decimals$}") } else { format!("{value:.decimals$}") };
    let language = language();
    if language == SOURCE_LANGUAGE {
        return text;
    }
    let (point, minus) = lang::marks(&language);
    text.chars().map(|c| match c { '.' => point.to_string(), '-' => minus.to_string(), c => c.to_string() }).collect()
}

/// A percentage as shown in the current language: "80%", or "80 %" where that's the way to write it.
pub fn percent(value: f64, decimals: usize, signed: bool) -> String {
    tr_with("{n}%", "percentage", &[("n", &number(value, decimals, signed))])
}

/// Where the catalogs come from. A shipped build has them in the program; a folder of .po files does for a translator trying one.
pub fn use_source(source: Source) {
    *SOURCE.write().unwrap_or_else(|e| e.into_inner()) = Some(source);
}

/// use_source with a folder of <lang>.po files and telescope.pot.
pub fn use_folder(folder: impl Into<std::path::PathBuf>) {
    use_source(Source::Folder(folder.into()));
}

fn source() -> Option<Source> {
    SOURCE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Languages with a catalog, besides English, sorted.
pub fn available() -> Vec<String> {
    source().map(|s| s.available()).unwrap_or_default()
}

/// A first draft nobody has checked yet: Settings marks it as one.
pub fn is_draft(lang: &str) -> bool {
    source().is_some_and(|s| s.is_draft(lang))
}

/// The language to load for the one picked in Settings ("" is the system's). TELESCOPE_LANG wins over both.
pub fn resolve(chosen: &str) -> String {
    lang::resolve(chosen, &available())
}

/// Loads a language (the system's when None or "") and returns the one loaded, "en" when it can't be read. Call before building any window.
pub fn load(wanted: Option<&str>) -> String {
    let have = available();
    let lang = match wanted {
        Some(l) if !l.is_empty() => pick(&[l.to_string()], &have),
        _ => lang::resolve("", &have),
    };
    *TRANSLATOR.write().unwrap_or_else(|e| e.into_inner()) = None;
    *CATALOG.write().unwrap_or_else(|e| e.into_inner()) = None;
    set_language(SOURCE_LANGUAGE);
    if lang == SOURCE_LANGUAGE {
        return lang;
    }
    let loaded = source().ok_or_else(|| "there are no catalogs".to_string()).and_then(|s| s.catalog(&lang));
    match loaded {
        Ok(catalog) => {
            log::info!("Language: {lang} ({} texts translated)", catalog.len());
            *CATALOG.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(catalog));
            set_language(&lang);
            lang
        }
        Err(e) => {
            log::error!("Couldn't read the {lang} translation, staying in English: {e}");
            SOURCE_LANGUAGE.into()
        }
    }
}

/// What the app does at start: loads the language Settings picked ("" for the system's), before any window or dialog is built. A folder
/// named by TELESCOPE_LOCALE_DIR stands in for the catalogs in the program.
pub fn start(chosen: &str) -> String {
    if let Some(folder) = std::env::var_os("TELESCOPE_LOCALE_DIR").filter(|v| !v.is_empty()) {
        use_folder(folder);
    }
    load(Some(&resolve(chosen)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_the_way_the_language_does() {
        // One test, since the language is shared by every test in this binary
        set_language("en");
        assert_eq!((number(1.0, 1, false), number(0.5, 2, true), number(-3.0, 0, false)), ("1.0".into(), "+0.50".into(), "-3".into()));
        assert_eq!(percent(80.0, 0, false), "80%");
        set_language("sv");
        assert_eq!(number(1.5, 1, false), "1,5");
        assert_eq!(number(-1.5, 1, false), "\u{2212}1,5");
        set_language("pt_BR");
        assert_eq!(number(2.25, 2, true), "+2,25");
        set_language("ru");
        assert_eq!(number(-1.5, 1, false), "-1,5");
        set_language("ja");
        assert_eq!(number(2.5, 1, false), "2.5");
        set_language("en");
    }
}
