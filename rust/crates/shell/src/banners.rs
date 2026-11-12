//! In-window problem banners (widgets/banner.py's BannerArea): keyed issues stacked above the body. Showing a key
//! again replaces its banner.

use telescope_core::ui::{Issue, IssueDoes};

#[derive(Default)]
pub struct Banners {
    shown: Vec<(String, Issue)>,
}

/// One banner as the window draws it. A banner whose issue is gone stays as a closed row until its strip has shut.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: String,
    pub issue: Issue,
    pub open: bool,
    /// Goes up whenever the issue under this key says something different, so the strip cross-fades.
    pub rev: i32,
}

/// What a row's change comes to in the window's list of rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edit {
    Insert(usize),
    Set(usize),
}

/// The rows the window draws. They are changed in place (a banner that is already up keeps its place and its motion), a new key is
/// added at the end, a key that is gone is closed and only removed once the window says its strip has shut.
#[derive(Default)]
pub struct View {
    rows: Vec<Row>,
}

impl View {
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Brings the rows to what is shown now, and says which rows changed.
    pub fn sync(&mut self, shown: &[(String, Issue)]) -> Vec<Edit> {
        let mut edits = Vec::new();
        for (key, issue) in shown {
            match self.rows.iter().position(|r| &r.key == key) {
                Some(i) => {
                    let row = &mut self.rows[i];
                    let said = row.issue != *issue;
                    if said || !row.open {
                        row.open = true;
                        row.rev += i32::from(said);
                        row.issue = issue.clone();
                        edits.push(Edit::Set(i));
                    }
                }
                None => {
                    self.rows.push(Row { key: key.clone(), issue: issue.clone(), open: true, rev: 0 });
                    edits.push(Edit::Insert(self.rows.len() - 1));
                }
            }
        }
        for (i, row) in self.rows.iter_mut().enumerate() {
            if row.open && !shown.iter().any(|(k, _)| *k == row.key) {
                row.open = false;
                edits.push(Edit::Set(i));
            }
        }
        edits
    }

    /// The strip of a closed row has shut: its row goes, and this is where it was. Nothing for a row that has opened again meanwhile.
    pub fn remove_shut(&mut self, key: &str) -> Option<usize> {
        let at = self.rows.iter().position(|r| r.key == key && !r.open)?;
        self.rows.remove(at);
        Some(at)
    }
}

/// What pressing a banner's button or its X comes to.
#[derive(Clone, Debug, PartialEq)]
pub enum Pressed {
    Action(IssueDoes),
    /// The X on an issue that asked to hear about it.
    Dismissed,
    Nothing,
}

impl Banners {
    pub fn show(&mut self, key: &str, issue: Issue) {
        let line = if issue.text.is_empty() { format!("Banner: {}", issue.title) } else { format!("Banner: {}. {}", issue.title, issue.text) };
        telescope_core::diagnostics::events().note(&line);
        self.shown.retain(|(k, _)| k != key);
        self.shown.push((key.to_string(), issue));
    }

    /// Remove key's banner (all of them without a key); with issue, only while key still shows that one.
    pub fn clear(&mut self, key: Option<&str>, issue: Option<&Issue>) {
        if let Some(issue) = issue {
            if self.issue(key.unwrap_or("")) != Some(issue) {
                return;
            }
        }
        match key {
            Some(key) => self.shown.retain(|(k, _)| k != key),
            None => self.shown.clear(),
        }
    }

    pub fn issue(&self, key: &str) -> Option<&Issue> {
        self.shown.iter().find(|(k, _)| k == key).map(|(_, i)| i)
    }

    pub fn keys(&self) -> Vec<String> {
        self.shown.iter().map(|(k, _)| k.clone()).collect()
    }

    pub fn all(&self) -> &[(String, Issue)] {
        &self.shown
    }

    /// The nth button of key's banner: most actions retry or move on, so the banner goes; Copy stays.
    pub fn press(&mut self, key: &str, action: usize) -> Pressed {
        let Some(does) = self.issue(key).and_then(|i| i.actions.get(action)).map(|a| a.does.clone()) else {
            return Pressed::Nothing;
        };
        if !keeps_banner(&does) {
            self.clear(Some(key), None);
        }
        Pressed::Action(does)
    }

    /// The X: the banner goes, and its plugin hears about it when it asked to.
    pub fn dismiss(&mut self, key: &str) -> Pressed {
        let tell = self.issue(key).is_some_and(|i| i.tell_dismiss);
        self.clear(Some(key), None);
        if tell { Pressed::Dismissed } else { Pressed::Nothing }
    }
}

/// Copying a command (an OpenUrl with a copy: scheme, the Rust form of copy_action) leaves the banner up.
pub fn keeps_banner(does: &IssueDoes) -> bool {
    matches!(does, IssueDoes::OpenUrl { url } if url.starts_with(COPY_SCHEME))
}

/// copy_action's Rust form: the text after this goes to the clipboard.
pub const COPY_SCHEME: &str = "copy:";

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(title: &str) -> Issue {
        Issue { title: title.into(), text: String::new(), actions: vec![], kind: "err".into(), details: String::new(), tell_dismiss: false }
    }

    fn shown(rows: &[(&str, &str)]) -> Vec<(String, Issue)> {
        rows.iter().map(|(k, t)| (k.to_string(), issue(t))).collect()
    }

    #[test]
    fn a_row_that_says_the_same_is_left_alone() {
        let mut view = View::default();
        assert_eq!(view.sync(&shown(&[("a", "One")])), [Edit::Insert(0)]);
        assert_eq!(view.sync(&shown(&[("a", "One")])), []);
    }

    #[test]
    fn a_new_message_under_the_same_key_is_a_revision_in_the_same_place() {
        let mut view = View::default();
        view.sync(&shown(&[("a", "One"), ("b", "Two")]));
        assert_eq!(view.sync(&shown(&[("b", "Two"), ("a", "Other")])), [Edit::Set(0)]);
        assert_eq!((view.rows()[0].key.as_str(), view.rows()[0].rev), ("a", 1));
        assert_eq!(view.rows()[1].rev, 0);
    }

    #[test]
    fn a_gone_key_closes_and_stays_until_it_has_shut_and_a_new_one_goes_after_it() {
        let mut view = View::default();
        view.sync(&shown(&[("a", "One"), ("b", "Two")]));
        assert_eq!(view.sync(&shown(&[("b", "Two")])), [Edit::Set(0)]);
        assert!(!view.rows()[0].open && view.rows()[1].open);
        assert_eq!(view.sync(&shown(&[("b", "Two"), ("c", "Three")])), [Edit::Insert(2)]);
        assert_eq!(view.remove_shut("b"), None, "an open row is never removed");
        assert_eq!(view.remove_shut("a"), Some(0));
        assert_eq!(view.rows().iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["b", "c"]);
    }

    #[test]
    fn a_key_that_comes_back_while_closing_opens_again_with_no_new_revision() {
        let mut view = View::default();
        view.sync(&shown(&[("a", "One")]));
        view.sync(&[]);
        assert_eq!(view.sync(&shown(&[("a", "One")])), [Edit::Set(0)]);
        assert!(view.rows()[0].open);
        assert_eq!(view.rows()[0].rev, 0);
        assert_eq!(view.remove_shut("a"), None);
    }
}
