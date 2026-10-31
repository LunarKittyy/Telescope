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

/// One {field} at the start of tail: (name, conversion, format spec, its length), as str.format reads it.
fn field_at(tail: &str) -> Option<(&str, &str, &str, usize)> {
    let end = tail.find('}')?;
    let inner = &tail[1..end];
    let (head, spec) = inner.split_once(':').unwrap_or((inner, ""));
    let (name, conversion) = head.split_once('!').unwrap_or((head, ""));
    Some((name, conversion, spec, end + 1))
}

/// The numbers format specs treat as such: a value's text is all there is to go by, so one that reads as a number counts as one.
fn as_number(text: &str) -> Option<f64> {
    text.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// `text` through a str.format spec: [[fill]align][sign][0][width][,_][.precision][dfFs]; the others (%, e, g, x) are an Err, which shows the English.
fn format_spec(text: &str, spec: &str) -> Result<String, String> {
    let bad = || format!("the format spec {spec:?} on {text:?}");
    let mut chars: Vec<char> = spec.chars().collect();
    let (mut fill_char, mut align, mut fill_given) = (' ', None, false);
    if chars.len() >= 2 && "<>=^".contains(chars[1]) {
        fill_char = chars[0];
        fill_given = true;
        align = Some(chars[1]);
        chars.drain(..2);
    } else if chars.first().is_some_and(|c| "<>=^".contains(*c)) {
        align = Some(chars.remove(0));
    }
    let sign = chars.first().copied().filter(|c| "+- ".contains(*c));
    if sign.is_some() {
        chars.remove(0);
    }
    let zero = chars.first() == Some(&'0');
    if zero {
        chars.remove(0);
    }
    let digits = |chars: &mut Vec<char>| -> Option<usize> {
        let n = chars.iter().take_while(|c| c.is_ascii_digit()).count();
        let number: String = chars.drain(..n).collect();
        number.parse().ok()
    };
    let width = digits(&mut chars).unwrap_or(0);
    let grouping = chars.first().copied().filter(|c| ",_".contains(*c));
    if grouping.is_some() {
        chars.remove(0);
    }
    let precision = if chars.first() == Some(&'.') {
        chars.remove(0);
        Some(digits(&mut chars).ok_or_else(bad)?)
    } else {
        None
    };
    let kind = chars.pop();
    if !chars.is_empty() {
        return Err(bad());
    }
    let number = as_number(text);
    let whole = !text.is_empty() && text.strip_prefix('-').unwrap_or(text).bytes().all(|b| b.is_ascii_digit());
    let is_number = number.is_some() && kind != Some('s');
    if zero {
        if !fill_given {
            fill_char = '0';
        }
        align = align.or(Some(if is_number { '=' } else { '<' }));
    }
    if !is_number && (sign.is_some() || grouping.is_some() || align == Some('=')) {
        return Err(bad());
    }
    let body = match kind {
        Some('f' | 'F') => format!("{:.*}", precision.unwrap_or(6), number.ok_or_else(bad)?),
        Some('d') if whole && precision.is_none() => text.to_string(),
        None if is_number && precision.is_none() => text.to_string(),
        None | Some('s') if !is_number => precision.map_or(text.to_string(), |p| text.chars().take(p).collect()),
        _ => return Err(bad()),
    };
    let (minus, digits_part) = match body.strip_prefix('-') {
        Some(rest) if is_number => ("-", rest.to_string()),
        _ => ("", body),
    };
    let lead = if !is_number { "" } else if !minus.is_empty() { minus } else { match sign { Some('+') => "+", Some(' ') => " ", _ => "" } };
    let digits_part = match (grouping, is_number) {
        (Some(mark), true) => {
            let (whole, part) = digits_part.split_once('.').map_or((digits_part.as_str(), ""), |(w, p)| (w, p));
            let grouped: Vec<String> = whole.as_bytes().rchunks(3).rev().map(|c| String::from_utf8_lossy(c).into_owned()).collect();
            let joined = grouped.join(&mark.to_string());
            if part.is_empty() { joined } else { format!("{joined}.{part}") }
        }
        _ => digits_part,
    };
    let length = lead.chars().count() + digits_part.chars().count();
    let room = width.saturating_sub(length);
    let pad = |n: usize| fill_char.to_string().repeat(n);
    Ok(match align.unwrap_or(if is_number { '>' } else { '<' }) {
        '<' => format!("{lead}{digits_part}{}", pad(room)),
        '^' => format!("{}{lead}{digits_part}{}", pad(room / 2), pad(room - room / 2)),
        '=' => format!("{lead}{}{digits_part}", pad(room)),
        _ => format!("{}{lead}{digits_part}", pad(room)),
    })
}

/// A value as a {field} with this conversion and spec shows it.
fn format_field(value: &dyn Display, conversion: &str, spec: &str) -> Result<String, String> {
    let text = value.to_string();
    let text = match conversion {
        "" | "s" => text,
        "r" | "a" if as_number(&text).is_some() => text,
        "r" | "a" => format!("'{text}'"),
        other => return Err(format!("the conversion !{other}")),
    };
    if spec.is_empty() { Ok(text) } else { format_spec(&text, spec) }
}

/// text with each {name} filled in like str.format, conversion and spec too ({n:02d}), {{ and }} for braces; an Err says what didn't fit.
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
        let (name, conversion, spec, len) = field_at(tail).ok_or_else(|| format!("an unclosed {{ in {text:?}"))?;
        let (_, value) = fields.iter().find(|(n, _)| *n == name).ok_or_else(|| format!("no value for {{{name}}} in {text:?}"))?;
        out += &format_field(*value, conversion, spec)?;
        rest = &tail[len..];
    }
    out += rest;
    Ok(out)
}

/// One {field} filled in at a time, by code that gathers its values as it goes; a spec the value can't take shows the value as it is.
pub trait Fields {
    /// This text with its {name} filled in, any conversion and spec too, and every other field left as it is.
    fn field(&self, name: &str, value: impl Display) -> String;
}

impl Fields for str {
    fn field(&self, name: &str, value: impl Display) -> String {
        fill_one(self, name, &value)
    }
}

fn fill_one(text: &str, wanted: &str, value: &dyn Display) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('{') {
        out += &rest[..i];
        rest = &rest[i..];
        if rest.starts_with("{{") {
            out += "{{";
            rest = &rest[2..];
            continue;
        }
        let Some((name, conversion, spec, len)) = field_at(rest) else { break };
        // A brace in the way is no field: a template of code has blocks around its fields
        if rest[1..len].contains('{') {
            out.push('{');
            rest = &rest[1..];
            continue;
        }
        if name == wanted {
            out += &format_field(value, conversion, spec).unwrap_or_else(|_| value.to_string());
        } else {
            out += &rest[..len];
        }
        rest = &rest[len..];
    }
    out + rest
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
    plural_form(singular, plural, n, "").unwrap_or_else(|| english.to_string()).field("n", n)
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
