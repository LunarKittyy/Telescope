//! In-window problem banners (widgets/banner.py's BannerArea): keyed issues stacked above the body. Showing a key
//! again replaces its banner.

use telescope_core::ui::{Issue, IssueDoes};

#[derive(Default)]
pub struct Banners {
    shown: Vec<(String, Issue)>,
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
    use telescope_core::ui::IssueAction;

    fn issue(title: &str) -> Issue {
        Issue { title: title.into(), text: String::new(), actions: vec![], kind: "err".into(), details: String::new(), tell_dismiss: false }
    }

    #[test]
    fn keys_replace_and_clear() {
        let mut b = Banners::default();
        b.show("a", issue("one"));
        b.show("b", issue("two"));
        b.show("a", issue("three"));
        assert_eq!(b.keys(), ["b", "a"]);
        assert_eq!(b.issue("a").unwrap().title, "three");
        b.clear(Some("b"), None);
        assert_eq!(b.keys(), ["a"]);
        b.clear(None, None);
        assert!(b.keys().is_empty());
    }

    #[test]
    fn an_action_runs_and_takes_the_banner_with_it() {
        let mut b = Banners::default();
        let mut i = issue("x");
        i.actions.push(IssueAction { label: "Retry".into(), does: IssueDoes::Plugin { id: "retry".into() } });
        b.show("k", i);
        assert_eq!(b.press("k", 0), Pressed::Action(IssueDoes::Plugin { id: "retry".into() }));
        assert!(b.issue("k").is_none());
    }

    #[test]
    fn copy_keeps_the_banner() {
        let mut b = Banners::default();
        let mut i = issue("x");
        i.actions.push(IssueAction { label: "Copy command".into(), does: IssueDoes::OpenUrl { url: "copy:sudo modprobe".into() } });
        b.show("k", i);
        assert!(matches!(b.press("k", 0), Pressed::Action(_)));
        assert!(b.issue("k").is_some());
    }

    #[test]
    fn closing_tells_whoever_showed_it() {
        let mut b = Banners::default();
        b.show("quiet", issue("x"));
        let mut told = issue("y");
        told.tell_dismiss = true;
        b.show("told", told);
        assert_eq!(b.dismiss("quiet"), Pressed::Nothing);
        assert_eq!(b.dismiss("told"), Pressed::Dismissed);
        assert!(b.keys().is_empty());
    }

    #[test]
    fn clearing_one_issue_leaves_a_newer_one_under_the_same_key() {
        let mut b = Banners::default();
        let old = issue("old");
        b.show("k", old.clone());
        b.show("k", issue("new"));
        b.clear(Some("k"), Some(&old));
        assert_eq!(b.issue("k").unwrap().title, "new");
    }
}
