//! Matching typed words against a few named fields of text, forgiving about typos and word endings (fuzzy.py).

use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;

const STOP_WORDS: &[&str] = &[
    "how", "to", "the", "a", "an", "my", "i", "of", "for", "in", "on", "can", "do", "does", "where", "what", "is", "it",
    "change", "set", "use", "and", "or", "with", "me", "turn", "enable", "disable", "switch", "toggle", "show", "find",
    "want", "need", "please", "setting", "settings", "option", "options", "feature", "not", "working", "showing",
    "missing", "doesn", "wont", "cant", "won", "t", "s",
];
const MIN_SCORE: f64 = 18.0;

/// str.casefold() as far as it shows after the rest of normalize: lowercase, plus the folds lowercase doesn't make (ß is "ss").
fn casefold(text: &str) -> String {
    text.chars().flat_map(|c| match c {
        'ß' | 'ẞ' => vec!['s', 's'],
        // a combining mark that folds to a letter, so it stays
        '\u{345}' => vec!['\u{3b9}'],
        _ => c.to_lowercase().collect(),
    }).collect()
}

pub fn normalize(text: &str) -> String {
    // unicodedata.combining: has a combining class, which not every mark does
    let folded: String = casefold(text).nfkd().filter(|c| unicode_normalization::char::canonical_combining_class(*c) == 0).collect();
    let mut out = String::new();
    let mut gap = false;
    for c in folded.chars() {
        // Any script's letters and digits, not just English's, so a Russian or Japanese label can be found
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

pub fn split(text: &str) -> Vec<String> {
    normalize(text).split(' ').filter(|w| !w.is_empty()).map(String::from).collect()
}

/// Edit distance, a swap of two neighbours counting as one edit.
fn osa(a: &[char], b: &[char]) -> usize {
    let mut rows = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, v) in rows[0].iter_mut().enumerate() {
        *v = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = (a[i - 1] != b[j - 1]) as usize;
            rows[i][j] = (rows[i - 1][j] + 1).min(rows[i][j - 1] + 1).min(rows[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                rows[i][j] = rows[i][j].min(rows[i - 2][j - 2] + 1);
            }
        }
    }
    rows[a.len()][b.len()]
}

fn subsequence(q: &str, text: &str) -> bool {
    let mut it = text.chars();
    q.chars().all(|c| it.any(|t| t == c))
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{ac00}'..='\u{d7af}')
}

/// How well the typed word q fits one word of a field, 0 for not at all.
fn word_score(q: &str, w: &str) -> f64 {
    if w == q {
        return 100.0;
    }
    let (qc, wc): (Vec<char>, Vec<char>) = (q.chars().collect(), w.chars().collect());
    if w.starts_with(q) {
        return 88.0 - (wc.len() - qc.len()).min(12) as f64;
    }
    let mut best = 0.0;
    // Chinese and Japanese run words together, so a short piece of one is found inside it
    if (qc.len() >= 5 || (qc.len() >= 2 && qc.iter().any(|&c| is_cjk(c)))) && w.contains(q) {
        best = 55.0;
    }
    if qc.len() >= 4 && qc[0] == wc[0] {
        let allowed = if qc.len() < 7 { 1 } else { 2 };
        let sizes: Vec<usize> = if qc.len() == 4 { vec![4] } else { vec![qc.len() - 1, qc.len(), qc.len() + 1] };
        let d = sizes.iter().map(|&k| osa(&qc, &wc[..k.min(wc.len())])).min().unwrap_or(usize::MAX);
        if d <= allowed {
            best = f64::max(best, 74.0 - 22.0 * d as f64);
        }
    }
    best
}

/// Named fields of text, ready to be scored against what someone typed (fuzzy.Doc).
pub struct Doc {
    fields: HashMap<&'static str, (Vec<String>, String, String)>,
}

impl Doc {
    pub fn new(fields: &[(&'static str, &str)]) -> Doc {
        Doc {
            fields: fields.iter().map(|(k, text)| {
                let words = split(text);
                let joined = words.concat();
                let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
                (*k, (words, joined, initials))
            }).collect(),
        }
    }

    fn field_score(&self, token: &str, key: &str) -> f64 {
        let Some((words, joined, initials)) = self.fields.get(key) else { return 0.0 };
        let mut best = words.iter().map(|w| word_score(token, w)).fold(0.0, f64::max);
        let n = token.chars().count();
        if best < 60.0 && (2..=6).contains(&n) && words.len() > 1 && initials.starts_with(token) {
            best = best.max(66.0);
        }
        if best < 40.0 && key == "name" && n >= 5 && token.chars().next() == joined.chars().next() && subsequence(token, joined) {
            best = best.max(34.0);
        }
        best
    }

    /// 0 when a word of the query matches nothing (a one or two letter word is let off), else higher is better.
    pub fn score(&self, query: &str, weights: &[(&str, f64)], partial: bool) -> f64 {
        let mut tokens = split(query);
        if tokens.len() > 1 {
            let kept: Vec<String> = tokens.iter().filter(|t| !STOP_WORDS.contains(&t.as_str())).cloned().collect();
            if !kept.is_empty() {
                tokens = kept;
            }
        }
        let (mut total, mut used, mut counted) = (0.0, 0, 0);
        for token in &tokens {
            let best = weights.iter().filter(|(k, _)| self.fields.contains_key(k)).map(|(k, w)| w * self.field_score(token, k)).fold(0.0, f64::max);
            if best <= 0.0 {
                if token.chars().count() <= 2 {
                    continue;
                }
                if !partial {
                    return 0.0;
                }
                counted += 1;
                continue;
            }
            total += best;
            used += 1;
            counted += 1;
        }
        if used == 0 {
            return 0.0;
        }
        let mut result = total / counted as f64;
        let phrase = normalize(query);
        let name = self.fields.get("name").map(|f| f.0.join(" ")).unwrap_or_default();
        if !phrase.is_empty() && !name.is_empty() {
            if phrase == name {
                result += 40.0;
            } else if name.starts_with(&phrase) {
                result += 16.0;
            } else if name.contains(&phrase) {
                result += 10.0;
            }
        }
        if result >= MIN_SCORE { result } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Python's matcher made of these (recorded from the Python app).
    fn recorded() -> serde_json::Value {
        serde_json::from_str(include_str!("../fixtures/fuzzy.json")).unwrap()
    }

    #[test]
    fn normalize_gives_what_python_recorded() {
        // What the recording drops is any letter outside English's: those stay now, so what's compared is the rest
        let english = |text: &str| -> String {
            let plain: String = text.chars().map(|c| if c.is_ascii() { c } else { ' ' }).collect();
            plain.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        for case in recorded()["normalize"].as_array().unwrap() {
            let (text, want) = (case[0].as_str().unwrap(), case[1].as_str().unwrap());
            let got = normalize(text);
            if got != want {
                assert_eq!(english(&got), want, "{text:?}");
            }
        }
    }

    #[test]
    fn scores_are_the_ones_python_recorded() {
        let fixture = recorded();
        let weights: Vec<(&str, f64)> = vec![("name", fixture["weights"]["name"].as_f64().unwrap()), ("keywords", fixture["weights"]["keywords"].as_f64().unwrap())];
        for case in fixture["scores"].as_array().unwrap() {
            let (name, keywords, query) = (case["name"].as_str().unwrap(), case["keywords"].as_str().unwrap(), case["query"].as_str().unwrap());
            let got = Doc::new(&[("name", name), ("keywords", keywords)]).score(query, &weights, case["partial"].as_bool().unwrap());
            assert!((got - case["score"].as_f64().unwrap()).abs() < 1e-9, "{query:?} in {name:?}: want {}, got {got}", case["score"]);
        }
    }

    #[test]
    fn typos_are_forgiven_and_word_starts_are_wanted() {
        let doc = Doc::new(&[("name", "Mute"), ("keywords", "silence"), ("card", "Microphone"), ("extra", "microphone.mute")]);
        let weights = [("name", 1.0), ("keywords", 0.9), ("card", 0.7), ("extra", 0.6)];
        for q in ["mute", "mutee", "silence", "micro"] {
            assert!(doc.score(q, &weights, false) > 0.0, "{q}");
        }
        assert_eq!(doc.score("crop", &weights, false), 0.0, "inside microphone is no match");
    }

    #[test]
    fn other_scripts_are_searchable_by_their_translated_labels() {
        assert_eq!(normalize("\u{41c}\u{438}\u{43a}\u{440}\u{43e}\u{444}\u{43e}\u{43d}, \u{412}\u{43a}\u{43b}"), "\u{43c}\u{438}\u{43a}\u{440}\u{43e}\u{444}\u{43e}\u{43d} \u{432}\u{43a}\u{43b}");
        assert_eq!(normalize("\u{65e5}\u{672c}\u{8a9e} abc"), "\u{65e5}\u{672c}\u{8a9e} abc");
        let weights = [("name", 1.0), ("keywords", 0.9)];
        // (label as the translation shows it, what someone types)
        for (label, typed) in [
            ("\u{41c}\u{438}\u{43a}\u{440}\u{43e}\u{444}\u{43e}\u{43d}", "\u{41c}\u{438}\u{43a}\u{440}\u{43e}\u{444}\u{43e}\u{43d}"),
            ("\u{41c}\u{438}\u{43a}\u{440}\u{43e}\u{444}\u{43e}\u{43d}", "\u{43c}\u{438}\u{43a}\u{440}\u{43e}"),
            ("\u{30de}\u{30a4}\u{30af}", "\u{30de}\u{30a4}"),
            ("\u{9ea6}\u{514b}\u{98ce}\u{9759}\u{97f3}", "\u{9759}\u{97f3}"),
            ("\u{b9c8}\u{c774}\u{d06c}", "\u{b9c8}\u{c774}\u{d06c}"),
        ] {
            let doc = Doc::new(&[("name", label), ("keywords", "")]);
            assert!(doc.score(typed, &weights, false) > 0.0, "{typed} in {label}");
            assert_eq!(doc.score("xyz", &weights, false), 0.0);
        }
        let doc = Doc::new(&[("name", "\u{41c}\u{438}\u{43a}\u{440}\u{43e}\u{444}\u{43e}\u{43d}"), ("keywords", "")]);
        assert_eq!(doc.score("\u{43a}\u{430}\u{43c}\u{435}\u{440}\u{430}", &weights, false), 0.0, "another word still isn't found");
    }

    #[test]
    fn accents_and_case_dont_matter() {
        assert_eq!(normalize("R\u{e9}solution  \u{c9}cran!"), "resolution ecran");
        assert_eq!(split("  Hello, World "), ["hello", "world"]);
    }
}
