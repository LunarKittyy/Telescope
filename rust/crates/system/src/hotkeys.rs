//! Global shortcuts: keys that work while another app has focus (platform/hotkeys.py).
//!
//! Windows lets an app claim a key outright (RegisterHotKey, hotkeys_windows.rs); on Linux the GlobalShortcuts portal
//! does it (hotkeys_portal.rs), where the desktop asks the user to confirm and may bind another key. Elsewhere there's
//! no backend, and `telescope --action` bound in the desktop's own settings stands in. Backends tell the plugin about
//! presses through its poster: Event::Posted with token HOTKEY and {"kind": "pressed" | "released" | "changed", "id"}.

use serde_json::json;
use std::sync::Arc;
use telescope_core::host::Poster;

pub const HOTKEY: &str = "hotkey";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub id: String,
    /// Qt portable text, "Ctrl+Alt+M".
    pub keys: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyStatus {
    pub working: bool,
    /// What to show next to the binding: the key the system bound, or why it doesn't work.
    pub text: String,
    /// False when the key doesn't even reach Telescope's own window (another app took it on Windows).
    pub reaches_app: bool,
}

impl HotkeyStatus {
    pub fn new(working: bool, text: String) -> HotkeyStatus {
        HotkeyStatus { working, text, reaches_app: true }
    }
}

/// How a backend reports presses, releases and changed statuses, from whatever thread it's on.
#[derive(Clone)]
pub struct Signals(pub Arc<dyn Poster>);

impl Signals {
    pub fn pressed(&self, id: &str) {
        self.0.post(HOTKEY, json!({"kind": "pressed", "id": id}));
    }
    pub fn released(&self, id: &str) {
        self.0.post(HOTKEY, json!({"kind": "released", "id": id}));
    }
    pub fn changed(&self) {
        self.0.post(HOTKEY, json!({"kind": "changed"}));
    }
}

/// Claims keys from the system and says when they're pressed and let go.
pub trait HotkeyBackend: Send {
    /// Make exactly these the global ones.
    fn apply(&mut self, hotkeys: &[Hotkey]);
    /// How the hotkey with this id is doing, or None before the system answered.
    fn status(&self, id: &str) -> Option<HotkeyStatus>;
    /// Let the keys go for a while (the shortcut editor is recording one), where that's cheap; false takes them back.
    fn suspend(&mut self, _on: bool) {}
    /// A line for the Shortcuts page about how global shortcuts work here.
    fn note(&self) -> String {
        String::new()
    }
    /// The system decides the final key (the portal): the page then offers its settings.
    fn system_picks_keys(&self) -> bool {
        false
    }
    /// Whether binding the same keys again would ask the user once more (some weren't confirmed).
    fn can_rebind(&self) -> bool {
        false
    }
    /// Open the system's own settings for these shortcuts; false if that didn't work.
    fn configure(&mut self) -> bool {
        false
    }
    /// For diagnostics.
    fn name(&self) -> &'static str;
    /// Let every key go (quitting).
    fn stop(&mut self) {}
}

/// This system's backend, or None where there's none (the page then points at telescope --action).
pub fn create_backend(_app_id: &str, _signals: Signals) -> Option<Box<dyn HotkeyBackend>> {
    None
}
