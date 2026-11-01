//! The desktop around the window: tray, notifications, autostart, one copy at a time, global shortcuts and updates. The system crate implements these.

use serde::{Deserialize, Serialize};

/// A tray menu entry; plugins describe theirs with ui::TrayAction and the shell builds the menu.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrayItem {
    pub id: String,
    pub title: String,
    pub checkable: bool,
    pub checked: bool,
    pub visible: bool,
    pub enabled: bool,
}

pub trait Tray: Send {
    fn set_items(&mut self, items: &[TrayItem]);
    fn set_tooltip(&mut self, text: &str);
    /// What the icon shows: the camera streaming, and the mic streaming as Some(muted) (create_tray_icon).
    fn set_state(&mut self, _camera: bool, _mic: Option<bool>) {}
    /// The tray item clicked since last asked, or the icon itself as "".
    fn poll_click(&mut self) -> Option<String>;
}

pub trait Notifier: Send + Sync {
    /// Urgent ones stay on screen until dismissed (Linux).
    fn notify(&self, title: &str, body: &str, urgent: bool);
}

pub trait Autostart: Send + Sync {
    fn enabled(&self) -> bool;
    /// minimized: start hidden in the tray.
    fn set_enabled(&self, on: bool, minimized: bool) -> Result<(), String>;
}

/// What a second copy asked the first for (main.py's --action and plain starts).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstanceMessage {
    /// Someone started Telescope again: bring the window up.
    Show,
    /// --action NAME: run that shortcut action.
    Action { name: String },
}

/// Being the one running copy, and hearing from later ones.
pub trait SingleInstance: Send {
    /// Messages later copies sent since last asked.
    fn poll(&mut self) -> Vec<InstanceMessage>;
}

/// Registering global shortcuts with the desktop.
pub trait GlobalShortcuts: Send {
    /// (action id, key sequence like "Ctrl+Alt+S") pairs; returns the ones the desktop refused.
    fn register(&mut self, bindings: &[(String, String)]) -> Vec<String>;
    /// Action ids pressed since last asked.
    fn poll(&mut self) -> Vec<String>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub channel: String,
    pub notes: String,
}

pub trait Updater: Send + Sync {
    fn check(&self, channel: &str) -> Result<Option<Release>, String>;
    /// Downloads and swaps it in; the app restarts after.
    fn install(&self, release: &Release) -> Result<(), String>;
    /// The update guard's start-up step (update_guard.py): finish a cut-short swap, roll back one that never started.
    fn on_start(&self) -> Result<(), String>;
    /// The window is up, so a trial update counts as good.
    fn confirm_started(&self);
}

/// A local file or folder as a link (QUrl.fromLocalFile), for the system to open.
pub fn file_url(path: &std::path::Path) -> String {
    file_url_for(&path.display().to_string(), cfg!(windows))
}

/// file_url with the path as text and the system given.
pub fn file_url_for(path: &str, windows: bool) -> String {
    let text = if windows { path.replace('\\', "/") } else { path.to_string() };
    let mut out = String::from(if text.starts_with("//") { "file:" } else if windows { "file:///" } else { "file://" });
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::file_url_for;

    #[test]
    fn a_path_becomes_a_link_on_either_system() {
        assert_eq!(file_url_for("/tmp/telescope-me/telescope.log", false), "file:///tmp/telescope-me/telescope.log");
        assert_eq!(file_url_for("/home/me/My Logs/#1.log", false), "file:///home/me/My%20Logs/%231.log");
        assert_eq!(file_url_for(r"C:\Users\Åsa\AppData\telescope.log", true), "file:///C:/Users/%C3%85sa/AppData/telescope.log");
        assert_eq!(file_url_for(r"\\server\share\a b.log", true), "file://server/share/a%20b.log");
    }
}

/// An io error in the system's words, like Python's strerror: no "(os error N)", and none of the full stop Windows ends them with.
pub fn os_words(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].trim_end_matches('.').to_string(),
        None => text,
    }
}

#[cfg(test)]
mod os_words_tests {
    use super::os_words;
    use std::io::Error;

    #[test]
    fn only_the_systems_own_words_are_left() {
        let words = os_words(&Error::from_raw_os_error(2));
        assert!(!words.is_empty() && !words.contains("os error") && !words.ends_with('.'), "{words}");
        assert_eq!(os_words(&Error::other("our own words.")), "our own words.");
    }
}
