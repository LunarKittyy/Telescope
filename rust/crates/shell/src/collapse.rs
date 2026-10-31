//! Which cards and sections are folded (widgets/collapse.py's Collapsing), remembered between runs.
//!
//! Keys are "<plugin>/<card title>" and "<plugin>/<card title>/<section>", in English so a fold survives a language
//! change. Keys of cards that aren't there right now (a plugin switched off) stay, so switching it back on finds its
//! folds as they were.

use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Default)]
pub struct Collapsing {
    folded: BTreeSet<String>,
}

impl Collapsing {
    /// The folded keys, sorted, for saving.
    pub fn keys(&self) -> Vec<String> {
        self.folded.iter().cloned().collect()
    }

    /// Take what keys() saved earlier; anything that isn't a list of strings is ignored.
    pub fn load(&mut self, keys: Option<&Value>) {
        self.folded = keys.and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(String::from).collect();
    }

    pub fn is_folded(&self, key: &str) -> bool {
        self.folded.contains(key)
    }

    /// True when it changed (the window then saves).
    pub fn set_folded(&mut self, key: &str, folded: bool) -> bool {
        if folded { self.folded.insert(key.to_string()) } else { self.folded.remove(key) }
    }

    pub fn toggle(&mut self, key: &str) -> bool {
        let folded = !self.is_folded(key);
        self.set_folded(key, folded)
    }
}

/// What a fold heading tells a screen reader and shows as its tooltip.
pub fn description(folded: bool) -> &'static str {
    if folded { "Collapsed. Press Space or Enter to expand." } else { "Expanded. Press Space or Enter to collapse." }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_say_who_owns_the_card_and_section() {
        let mut state = Collapsing::default();
        state.set_folded("camera_control/Things/Light", true);
        state.set_folded("camera_control/Things", true);
        assert_eq!(state.keys(), ["camera_control/Things", "camera_control/Things/Light"]);
    }

    #[test]
    fn state_that_isnt_a_list_of_strings_is_ignored() {
        let mut state = Collapsing::default();
        state.load(Some(&json!(["a", 3, null, "b"])));
        assert_eq!(state.keys(), ["a", "b"]);
        state.load(Some(&json!("not a list")));
        assert!(state.keys().is_empty());
        state.load(None);
        assert!(state.keys().is_empty());
    }

    #[test]
    fn changes_are_reported_once_each() {
        let mut state = Collapsing::default();
        assert!(state.set_folded("a", true));
        assert!(!state.set_folded("a", true));
        assert!(state.set_folded("a", false));
        state.load(Some(&json!(["x"])));
        assert!(state.is_folded("x"));
    }

    #[test]
    fn a_malformed_saved_list_is_reset() {
        use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
        let path = telescope_core::testing::scratch_dir("collapse-config").join(CONFIG_FILENAME);
        let file = ConfigFile::at(&path);
        let version = telescope_core::config::empty()["version"].clone();
        std::fs::write(&path, json!({"version": version, "collapsed": [1, "a"]}).to_string()).unwrap();
        assert_eq!(file.load()["collapsed"], json!([]));
        std::fs::write(&path, json!({"version": version, "collapsed": ["a"]}).to_string()).unwrap();
        assert_eq!(file.load()["collapsed"], json!(["a"]));
    }
}
