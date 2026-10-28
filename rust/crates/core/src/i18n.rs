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

/// Marks text for extraction without translating it yet (N_ in Python).
pub const fn n_(msgid: &str) -> &str {
    msgid
}
