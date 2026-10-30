//! Finding settings (fuzzy.py and widgets/settings_search.py): typed words matched against a setting's name, keywords,
//! card, tab and small print, forgiving about typos and word endings.

use std::collections::HashMap;
use telescope_core::i18n::tr;
use unicode_normalization::UnicodeNormalization;

const STOP_WORDS: &[&str] = &[
    "how", "to", "the", "a", "an", "my", "i", "of", "for", "in", "on", "can", "do", "does", "where", "what", "is", "it",
    "change", "set", "use", "and", "or", "with", "me", "turn", "enable", "disable", "switch", "toggle", "show", "find",
    "want", "need", "please", "setting", "settings", "option", "options", "feature", "not", "working", "showing",
    "missing", "doesn", "wont", "cant", "won", "t", "s",
];
const MIN_SCORE: f64 = 18.0;

pub fn normalize(text: &str) -> String {
    let folded: String = text.to_lowercase().nfkd().filter(|c| !unicode_normalization::char::is_combining_mark(*c)).collect();
    let mut out = String::new();
    let mut gap = false;
    for c in folded.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
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

fn split(text: &str) -> Vec<String> {
    normalize(text).split(' ').filter(|w| !w.is_empty()).map(String::from).collect()
}

/// Edit distance, a swap of two neighbours counting as one edit.
fn osa(a: &[char], b: &[char]) -> usize {
    let mut rows = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
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
    if qc.len() >= 5 && w.contains(q) {
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

const WEIGHTS: &[(&str, f64)] = &[("name", 1.0), ("keywords", 0.9), ("card", 0.62), ("tab", 0.5), ("detail", 0.55), ("extra", 0.42)];

/// One setting the search can offer (settings_search.Entry). target says where it is on its tab, for scrolling to it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub tab: String,
    pub tab_name: String,
    pub card: String,
    pub name: String,
    pub detail: String,
    pub keywords: String,
    pub extra: String,
    pub card_keywords: String,
    pub tab_keywords: String,
    pub card_entry: bool,
    pub tab_entry: bool,
    /// (x, y, width, height) on its page, in logical pixels.
    pub target: (f32, f32, f32, f32),
}

impl Entry {
    /// Where it lives, under its name in the results.
    pub fn path(&self) -> String {
        if self.tab_entry {
            return tr("Tab");
        }
        if !self.card.is_empty() && self.card != self.name && self.card != self.tab_name {
            format!("{}  ›  {}", self.tab_name, self.card)
        } else {
            self.tab_name.clone()
        }
    }

    fn doc(&self) -> Doc {
        Doc::new(&[
            ("name", &self.name),
            ("keywords", &self.keywords),
            ("card", &format!("{} {}", self.card, self.card_keywords)),
            ("tab", &format!("{} {}", self.tab_name, self.tab_keywords)),
            ("detail", &self.detail),
            ("extra", &self.extra),
        ])
    }

    pub fn score(&self, query: &str, partial: bool) -> f64 {
        let s = self.doc().score(query, WEIGHTS, partial);
        if self.card_entry { s * 0.92 } else { s }
    }
}

fn rank(entries: &[Entry], query: &str, limit: usize, partial: bool) -> Vec<Entry> {
    let mut ranked: Vec<(f64, usize, usize, &Entry)> = entries.iter().enumerate()
        .map(|(i, e)| (e.score(query, partial), e.name.chars().count(), i, e)).filter(|r| r.0 > 0.0).collect();
    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    ranked.into_iter().take(limit).map(|r| r.3.clone()).collect()
}

/// (entries that fit every word, best first; false) or, if none do, (ones that fit some; true).
pub fn search_ex(entries: &[Entry], query: &str, limit: usize) -> (Vec<Entry>, bool) {
    let hits = rank(entries, query, limit, false);
    if !hits.is_empty() {
        return (hits, false);
    }
    (rank(entries, query, limit, true), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, tab: &str, card: &str) -> Entry {
        Entry { tab: tab.to_lowercase(), tab_name: tab.into(), card: card.into(), name: name.into(), detail: String::new(), keywords: String::new(),
                extra: String::new(), card_keywords: String::new(), tab_keywords: String::new(), card_entry: false, tab_entry: false, target: (0.0, 0.0, 0.0, 0.0) }
    }

    #[test]
    fn one_letter_slips_and_swapped_letters_still_find_it() {
        let e = vec![entry("Resolution", "Streaming", "Output"), entry("Microphone", "Audio", "")];
        assert_eq!(search_ex(&e, "resolutoin", 12).0[0].name, "Resolution");
        assert_eq!(search_ex(&e, "microphne", 12).0[0].name, "Microphone");
    }

    #[test]
    fn a_word_has_to_start_a_word_unless_it_is_long() {
        let e = vec![entry("Microphone", "Audio", ""), entry("Crop", "Video", "")];
        assert_eq!(search_ex(&e, "crop", 12).0.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["Crop"]);
        assert!(search_ex(&e, "phone", 12).0.iter().any(|e| e.name == "Microphone"));
    }

    #[test]
    fn accents_and_case_dont_matter() {
        assert_eq!(normalize("Résolution  Écran!"), "resolution ecran");
    }

    #[test]
    fn every_word_has_to_match_but_filler_doesnt_count() {
        let e = vec![entry("Open Telescope when I sign in", "General", "Startup")];
        assert!(!search_ex(&e, "how do i change startup", 12).1);
        let (hits, partial) = search_ex(&e, "startup zebra", 12);
        assert!(partial && !hits.is_empty());
    }

    #[test]
    fn nonsense_finds_nothing() {
        let e = vec![entry("Resolution", "Streaming", "")];
        assert!(search_ex(&e, "qqqqzz", 12).0.is_empty());
    }

    #[test]
    fn results_say_where_the_setting_is() {
        let mut e = entry("Resolution", "Streaming", "Output");
        assert_eq!(e.path(), "Streaming  ›  Output");
        e.card = "Resolution".into();
        assert_eq!(e.path(), "Streaming");
        e.tab_entry = true;
        assert_eq!(e.path(), "Tab");
    }
}
