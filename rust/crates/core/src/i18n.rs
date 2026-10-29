//! Translating shown text: tr() where it's shown, n_() to mark text kept in English until then (telescope.i18n).

use std::sync::RwLock;

type Translator = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

static TRANSLATOR: RwLock<Option<Translator>> = RwLock::new(None);

/// The shell installs the loaded language's lookup; until then, and for missing entries, text stays English.
pub fn set_translator(lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static) {
    *TRANSLATOR.write().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(lookup));
}

/// The text in the current language. The msgid is the English text, word for word as in the Python app.
pub fn tr(msgid: &str) -> String {
    let guard = TRANSLATOR.read().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().and_then(|t| t(msgid)).unwrap_or_else(|| msgid.to_string())
}

/// A text that has a context in the Python app (tr(text, ctx="search")): looked up as gettext does, "ctx\u{4}text".
pub fn tr_ctx(ctx: &str, msgid: &str) -> String {
    let guard = TRANSLATOR.read().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().and_then(|t| t(&format!("{ctx}\u{4}{msgid}"))).unwrap_or_else(|| msgid.to_string())
}

/// Marks text for extraction without translating it yet (N_ in Python).
pub const fn n_(msgid: &str) -> &str {
    msgid
}

static LANGUAGE: RwLock<String> = RwLock::new(String::new());

/// The shell says which language is loaded ("en", "de", "pt_BR"...), which number() formats by.
pub fn set_language(code: &str) {
    *LANGUAGE.write().unwrap_or_else(|e| e.into_inner()) = code.to_string();
}

/// The language loaded, "en" when nothing is translated.
pub fn language() -> String {
    let guard = LANGUAGE.read().unwrap_or_else(|e| e.into_inner());
    if guard.is_empty() { "en".into() } else { guard.clone() }
}

/// (decimal point, minus sign) of a language, as Qt's locale gives them; English and anything unknown write 1.5 and -1.
fn marks(language: &str) -> (&'static str, &'static str) {
    match language.split(['_', '-']).next().unwrap_or("") {
        "de" | "es" | "fr" | "id" | "it" | "pl" | "pt" | "tr" | "uk" | "vi" => (",", "-"),
        "ru" | "sv" => (",", "\u{2212}"),
        _ => (".", "-"),
    }
}

/// A number as shown in the current language: "1.0" is "1,0" in Swedish (telescope.i18n.number). Only for what's shown;
/// saved settings and what's sent to the phone stay in plain formatting.
pub fn number(value: f64, decimals: usize, signed: bool) -> String {
    let text = if signed { format!("{value:+.decimals$}") } else { format!("{value:.decimals$}") };
    let language = language();
    if language == "en" {
        return text;
    }
    let (point, minus) = marks(&language);
    text.chars().map(|c| match c { '.' => point.to_string(), '-' => minus.to_string(), c => c.to_string() }).collect()
}

/// A percentage as shown in the current language: "80%", or "80 %" where that's the way to write it.
pub fn percent(value: f64, decimals: usize, signed: bool) -> String {
    tr_ctx("percentage", "{n}%").replace("{n}", &number(value, decimals, signed))
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
        set_language("ja");
        assert_eq!(number(2.5, 1, false), "2.5");
        set_language("en");
    }
}
