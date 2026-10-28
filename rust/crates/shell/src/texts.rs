//! Text helpers from telescope.i18n that core::i18n doesn't have yet: plurals, lists and filling in named blanks.

use telescope_core::i18n::tr;

/// The form that fits n, with {n} filled in. Only the English plural rule until core's translator knows plurals.
pub fn trn(singular: &str, plural: &str, n: usize) -> String {
    tr(if n == 1 { singular } else { plural }).replace("{n}", &n.to_string())
}

/// ["A", "B", "C"] as "A, B and C" in the current language.
pub fn and_list(words: &[String]) -> String {
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => fill(&tr("{items} and {last}"), &[("items", &rest.join(&tr(", "))), ("last", last)]),
    }
}

/// Fills in {name} blanks, as tr(text, name=value) does.
pub fn fill(text: &str, fields: &[(&str, &str)]) -> String {
    fields.iter().fold(text.to_string(), |t, (k, v)| t.replace(&format!("{{{k}}}"), v))
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
