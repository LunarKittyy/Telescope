//! The plugin switches kept in the settings file, for a window that has no plugin host to ask (scene renders and the shell's own
//! tests). The built-in plugins can be switched; nothing can be added.

use crate::switches;
use serde_json::Value;
use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use telescope_core::config::ConfigFile;
use telescope_core::plugin_admin::PluginAdmin;

pub struct ShellAdmin {
    file: ConfigFile,
}

impl ShellAdmin {
    pub fn new(settings: &Path) -> ShellAdmin {
        ShellAdmin { file: ConfigFile::at(settings) }
    }

    fn change(&self, change: impl FnOnce(&mut Value)) -> io::Result<()> {
        if self.file.update(change) { Ok(()) } else { Err(io::Error::other("Couldn't save the settings.")) }
    }
}

impl PluginAdmin for ShellAdmin {
    fn builtin_off(&self) -> BTreeSet<String> {
        switches::builtin_off(&self.file.load())
    }
    fn set_builtin_off(&self, name: &str, off: bool) -> io::Result<()> {
        self.change(|cfg| switches::set_builtin_off(cfg, name, off))
    }
    fn reset_to_defaults(&self) -> io::Result<()> {
        self.change(switches::reset_to_defaults)
    }
    fn optional_on(&self) -> BTreeSet<String> {
        switches::optional_on(&self.file.load())
    }
    fn set_optional_on(&self, name: &str, on: bool) -> io::Result<()> {
        self.change(|cfg| switches::set_optional_on(cfg, name, on))
    }
    fn switch(&self, key: &str, default: bool) -> bool {
        self.file.load().pointer(&format!("/plugins/{key}")).and_then(Value::as_bool).unwrap_or(default)
    }
    fn set_switch(&self, key: &str, on: bool) -> io::Result<()> {
        self.change(|cfg| cfg["plugins"][key] = Value::Bool(on))
    }
}
