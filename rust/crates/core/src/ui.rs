//! What plugins put on screen, described as data: cards and settings sections are .slint components plus properties.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where a .slint component comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "from", content = "value", rename_all = "snake_case")]
pub enum SlintSource {
    /// Source text (a built-in plugin's include_str!, or what a Lua plugin hands over).
    Text(String),
    /// A .slint file on disk (an added plugin's folder).
    File(String),
}

/// A piece of UI a plugin owns: a .slint component, the properties it starts with and the folder its imports resolve from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiSpec {
    pub source: SlintSource,
    /// The exported component to build.
    pub component: String,
    /// Property name to value, set before it's shown; later changes go through HostUi::set_ui_props.
    #[serde(default)]
    pub props: Value,
}

/// Something happened in a plugin's UI: a callback the component declared fired (with its arguments), or "shown".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiEvent {
    /// Which of the plugin's pieces of UI: "card", "header", or a settings section's id.
    pub ui: String,
    /// The callback's name in the .slint file.
    pub callback: String,
    #[serde(default)]
    pub args: Vec<Value>,
}

/// A card in the window's columns (create_panel). The window adds the header, folding and colour from PluginInfo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CardSpec {
    pub ui: UiSpec,
    /// Panel placement: "left", "right" or "center" (the video stage).
    #[serde(default = "left")]
    pub region: String,
}

fn left() -> String {
    "left".into()
}

/// A block of controls for the settings window (SettingsSection).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingsSection {
    /// Names it in UiEvent.ui and set_ui_props.
    pub id: String,
    /// An id from SETTINGS_TABS, or a new one (then give tab_title).
    pub tab: String,
    pub ui: UiSpec,
    /// Lower first, then in registration order.
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub tab_title: String,
    /// Extra words the search finds everything in this section by.
    #[serde(default)]
    pub keywords: String,
    /// A bar kept at the bottom of the tab under the scrolling page.
    #[serde(default)]
    pub footer: Option<UiSpec>,
}

/// A tray menu entry a plugin keeps up to date (create_tray_actions).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrayAction {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub checkable: bool,
    #[serde(default)]
    pub checked: bool,
    #[serde(default = "yes")]
    pub visible: bool,
}

fn yes() -> bool {
    true
}

/// Something a key can do (telescope.shortcuts.ShortcutAction); Plugin::run_action does it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionSpec {
    pub id: String,
    pub label: String,
    pub group: String,
    /// The settings a binding can give it, as the shortcut editor shows them.
    #[serde(default)]
    pub params: Vec<Value>,
    /// Holding the key down repeats it (a step does, a toggle doesn't).
    #[serde(default)]
    pub repeats: bool,
    /// It wants to hear when a held key comes back up (Plugin::release_action).
    #[serde(default)]
    pub on_release: bool,
    /// Where the control is now, which a new "Set to" binding starts at.
    #[serde(default)]
    pub current: Option<f64>,
    #[serde(default)]
    pub keywords: String,
}

/// A key event in one of Telescope's windows, as Slint gives it (KeyEvent): text is the key's text, special keys as
/// Slint's Key codes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeyInput {
    pub text: String,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    /// Held down long enough to repeat.
    pub repeat: bool,
    /// Let go, rather than pressed.
    pub released: bool,
    /// A text box has focus, so a plain key belongs to it.
    pub typing: bool,
    /// A dialog or menu is open over the window.
    pub modal: bool,
}

/// A problem banner above the body (telescope.widgets.banner.Issue).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub title: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub actions: Vec<IssueAction>,
    /// "err", "warn" or "info".
    #[serde(default = "err")]
    pub kind: String,
    /// Shown monospaced under the text, e.g. a command to run.
    #[serde(default)]
    pub details: String,
    /// Tell the plugin (Plugin::on_issue_action with "dismiss") when the person closes it.
    #[serde(default)]
    pub tell_dismiss: bool,
}

fn err() -> String {
    "err".into()
}

/// A banner button.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IssueAction {
    pub label: String,
    pub does: IssueDoes,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IssueDoes {
    /// Plugin::on_issue_action gets this id.
    Plugin { id: String },
    /// Start this source again next to any other stream (start_again); Plugin::on_issue_action hears `before` first, if given.
    StartAgain { source_id: Option<String>, before: Option<String> },
    OpenSettings { tab: String },
    OpenUrl { url: String },
}
