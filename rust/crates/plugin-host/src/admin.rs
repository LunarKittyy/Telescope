//! The Plugins tab's view of the plugins folder, the switches and the update check, as core's PluginAdmin.

use crate::package::{InstallError, PluginFolder};
use crate::updater::PluginUpdater;
use crate::Installed;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use telescope_core::plugin_admin::{AddedPlugin, InstallFailure, NewerRelease, PluginAdmin};

pub struct Admin {
    folder: PluginFolder,
    updater: Mutex<PluginUpdater>,
}

fn added(item: Installed) -> AddedPlugin {
    AddedPlugin {
        folder: item.folder, name: item.name, title: item.title, version: item.version, author: item.author, description: item.description,
        repo: item.repo, category: item.category, problem: item.problem, enabled: item.enabled, changed: item.changed, dev: item.dev,
    }
}

impl Admin {
    pub fn new(folder: PluginFolder, updater: PluginUpdater) -> Admin {
        Admin { folder, updater: Mutex::new(updater) }
    }

    /// The app's own: the plugins folder next to the settings, any plugins being written, and the update check over HTTPS.
    /// reserved are the built-in plugins' names, which an added plugin can't take.
    pub fn app(reserved: impl IntoIterator<Item = String>) -> Admin {
        Admin::new(PluginFolder::app(reserved), PluginUpdater::app())
    }

    pub fn folder(&self) -> &PluginFolder {
        &self.folder
    }

    fn updater(&self) -> std::sync::MutexGuard<'_, PluginUpdater> {
        self.updater.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn find(&self, name: &str) -> Option<Installed> {
        self.folder.find(name)
    }
}

impl PluginAdmin for Admin {
    fn builtin_off(&self) -> BTreeSet<String> {
        self.folder.switches.builtin_off()
    }
    fn set_builtin_off(&self, name: &str, off: bool) -> io::Result<()> {
        self.folder.switches.set_builtin_off(name, off)
    }
    fn all_builtin_on(&self) -> io::Result<()> {
        self.folder.switches.all_builtin_on()
    }
    fn optional_on(&self) -> BTreeSet<String> {
        self.folder.switches.optional_on()
    }
    fn set_optional_on(&self, name: &str, on: bool) -> io::Result<()> {
        self.folder.switches.set_optional_on(name, on)
    }
    fn switch(&self, key: &str, default: bool) -> bool {
        self.folder.switches.switch(key, default)
    }
    fn set_switch(&self, key: &str, on: bool) -> io::Result<()> {
        self.folder.switches.set_switch(key, on)
    }
    fn plugins_dir(&self) -> PathBuf {
        self.folder.folder.clone()
    }
    fn find_installed(&self) -> Vec<AddedPlugin> {
        self.folder.find_installed().into_iter().map(added).collect()
    }
    fn set_enabled(&self, name: &str, on: bool) -> io::Result<()> {
        let item = self.find(name).ok_or_else(|| io::Error::other("no such plugin"))?;
        self.folder.set_enabled(&item, on)
    }
    fn install_zip(&self, archive: &Path, replace: bool) -> Result<AddedPlugin, InstallFailure> {
        match self.folder.install_zip(archive, replace) {
            Ok(item) => Ok(added(item)),
            Err(InstallError::Exists(name)) => Err(InstallFailure::Exists(name)),
            Err(e) => Err(InstallFailure::Failed(e.to_string())),
        }
    }
    fn remove(&self, name: &str) -> Result<(), String> {
        let item = self.find(name).ok_or_else(|| "no such plugin".to_string())?;
        self.folder.remove(&item).map_err(|e| e.to_string())
    }
    fn newer_release(&self, name: &str) -> Option<NewerRelease> {
        let item = self.find(name)?;
        let release = self.updater().newer(&self.folder, &item)?;
        Some(NewerRelease { tag: release.tag, page: release.page })
    }
    fn maybe_check_updates(&self) {
        self.updater().maybe_check(&self.folder);
    }
    fn poll_updates(&self) -> bool {
        self.updater().poll(&self.folder)
    }
    fn auto_updated(&self, name: &str) -> Option<String> {
        self.updater().updated.get(name).cloned()
    }
}
