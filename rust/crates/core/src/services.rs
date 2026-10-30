//! The implementations the binary hands the shell: every Phase 1 crate's part, behind core's traits.

use crate::audio::Audio;
use crate::link::Link;
use crate::media::Media;
use crate::output::Outputs;
use crate::plugin::Plugin;
use crate::system::{Autostart, Notifier, Tray, Updater};
use std::sync::{Arc, Mutex};

/// An added plugin that is switched on but didn't start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginProblem {
    pub name: String,
    pub title: String,
    pub error: String,
}

/// The plugins that didn't start the last time the factory ran, which it fills in for the shell to say so.
pub type PluginProblems = Arc<Mutex<Vec<PluginProblem>>>;

/// Builds a fresh set of plugins (every built-in one, then those from the plugin host), in registration order.
pub type PluginFactory = Box<dyn Fn() -> Vec<Box<dyn Plugin>> + Send + Sync>;

pub struct Services {
    pub link: Arc<dyn Link>,
    pub media: Arc<dyn Media>,
    pub outputs: Arc<dyn Outputs>,
    pub audio: Option<Arc<dyn Audio>>,
    pub notifier: Option<Arc<dyn Notifier>>,
    pub autostart: Option<Arc<dyn Autostart>>,
    pub updater: Option<Arc<dyn Updater>>,
    /// The tray icon and its menu, where the desktop shows one (the window's run only: a command line run has none).
    pub tray: Option<std::sync::Mutex<Box<dyn Tray>>>,
    pub plugins: PluginFactory,
    /// What the factory couldn't start (plugin_loader.load_enabled's failed list); empty when it wasn't asked.
    pub plugin_problems: PluginProblems,
}
