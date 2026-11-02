//! What the Plugins tab needs from the plugins people add and from the switches that turn plugins on and off (plugin_loader.py and
//! plugin_updater.py). The plugin host implements it and the binary hands it to the shell, which never names the plugin host.
//!
//! The methods for added plugins and updates have do-nothing defaults, so a stand-in only has to say how the switches are kept.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

/// The switch under "plugins" in the settings that lets the update check run (default on).
pub const CHECK_UPDATES: &str = "check_updates";
/// The switch under "plugins" that installs updates without asking (default off).
pub const AUTO_UPDATE: &str = "auto_update";

/// A plugin in the plugins folder, or one being written, as the Plugins tab lists it (plugin_loader.Installed).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AddedPlugin {
    pub folder: PathBuf,
    pub name: String,
    pub title: String,
    pub version: String,
    pub author: String,
    pub description: String,
    /// Its source's https address, "" when it gives none.
    pub repo: String,
    /// The group it asks for, an id from PLUGIN_CATEGORIES ("" for Extras).
    pub category: String,
    /// Why it can't be loaded, "" if it can.
    pub problem: String,
    pub enabled: bool,
    /// Switched on, but its files differ from when it was.
    pub changed: bool,
    /// Loaded from where it's written, always on.
    pub dev: bool,
}

/// A newer release of an added plugin that the last update check found.
#[derive(Clone, Debug, PartialEq)]
pub struct NewerRelease {
    pub tag: String,
    /// Its page on GitHub.
    pub page: String,
}

/// Why a .zip didn't go in.
#[derive(Clone, Debug, PartialEq)]
pub enum InstallFailure {
    /// A plugin of that name is already there and replacing wasn't asked for.
    Exists(String),
    /// What's wrong with it, worded for the person installing it.
    Failed(String),
}

pub trait PluginAdmin: Send + Sync {
    /// The built-in plugins the person turned off.
    fn builtin_off(&self) -> BTreeSet<String>;
    fn set_builtin_off(&self, name: &str, off: bool) -> io::Result<()>;
    /// The standard selection: every built-in plugin on, the optional ones off (added plugins stay as they are).
    fn reset_to_defaults(&self) -> io::Result<()>;
    /// The optional built-in plugins the person switched on.
    fn optional_on(&self) -> BTreeSet<String>;
    fn set_optional_on(&self, name: &str, on: bool) -> io::Result<()>;
    /// One of the update switches (CHECK_UPDATES, AUTO_UPDATE), the default when it isn't saved.
    fn switch(&self, key: &str, default: bool) -> bool;
    fn set_switch(&self, key: &str, on: bool) -> io::Result<()>;

    /// Where added plugins are kept.
    fn plugins_dir(&self) -> PathBuf {
        PathBuf::new()
    }
    /// Every added plugin, and any being written.
    fn find_installed(&self) -> Vec<AddedPlugin> {
        Vec::new()
    }
    fn set_enabled(&self, _name: &str, _on: bool) -> io::Result<()> {
        Err(io::Error::other("no such plugin"))
    }
    /// Unpacks a plugin .zip, switched off; replace puts it in place of the one that's there.
    fn install_zip(&self, _archive: &Path, _replace: bool) -> Result<AddedPlugin, InstallFailure> {
        Err(InstallFailure::Failed(String::new()))
    }
    /// Deletes an added plugin's folder and forgets it was on.
    fn remove(&self, _name: &str) -> Result<(), String> {
        Err(String::new())
    }
    /// The release newer than what's installed that the last check found, with the check switched on.
    fn newer_release(&self, _name: &str) -> Option<NewerRelease> {
        None
    }
    /// Starts checking the repos that are due, in the background.
    fn maybe_check_updates(&self) {}
    /// Takes in a check that finished (and installs what auto-update downloaded); true when one had.
    fn poll_updates(&self) -> bool {
        false
    }
    /// The version auto-update installed this run.
    fn auto_updated(&self, _name: &str) -> Option<String> {
        None
    }
}
