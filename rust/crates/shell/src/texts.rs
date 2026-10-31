//! Text helpers from telescope.i18n: plurals, lists and filling in named blanks.

use telescope_core::i18n::Fields;

/// The form that fits n in the current language's plural rule, with {n} filled in (and the rest left to the caller).
pub fn trn(singular: &str, plural: &str, n: usize) -> String {
    telescope_core::i18n::trn_open(singular, plural, n as i64)
}

/// ["A", "B", "C"] as "A, B and C" in the current language.
pub fn and_list(words: &[String]) -> String {
    telescope_core::i18n::and_list(words)
}

/// Fills in {name} blanks, as tr(text, name=value) does.
pub fn fill(text: &str, fields: &[(&str, &str)]) -> String {
    fields.iter().fold(text.to_string(), |t, (k, v)| t.field(k, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_plurals_read_like_the_python_app() {
        let w = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(and_list(&w(&["Camera", "Microphone", "Preview"])), "Camera, Microphone and Preview");
        assert_eq!(and_list(&w(&["Camera"])), "Camera");
        assert_eq!(trn("{n} plugin didn't start", "{n} plugins didn't start", 3), "3 plugins didn't start");
        assert_eq!(fill("to {output}", &[("output", "Telescope")]), "to Telescope");
    }
}
