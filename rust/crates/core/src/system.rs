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
