//! Finding settings (fuzzy.py and widgets/settings_search.py): typed words matched against a setting's name, keywords,
//! card, tab and small print, forgiving about typos and word endings.

use telescope_core::i18n::tr;
pub use telescope_core::fuzzy::{normalize, Doc};

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
