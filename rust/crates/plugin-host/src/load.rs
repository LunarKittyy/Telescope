//! Making plugins out of installed packages: the plugin_loader.load and load_enabled half that runs code.

use crate::guest::{GuestPlugin, HostCalls, PluginContext};
use crate::lua::LuaGuest;
use crate::package::{capped, Installed, PluginFolder};
use crate::text::tr;
use telescope_core::Plugin;

/// A Lua plugin that's ready to register.
pub type LuaPlugin = GuestPlugin<LuaGuest>;

/// Runs a plugin's main.lua and wraps it as a core Plugin; lang picks its locale/<lang>.po. Says what went wrong otherwise.
pub fn load(plugin: &Installed, lang: &str) -> Result<LuaPlugin, String> {
    if !plugin.problem.is_empty() || !plugin.enabled {
        return Err(if plugin.problem.is_empty() { tr("It isn't turned on.", &[]) } else { plugin.problem.clone() });
    }
    load_unchecked(plugin, lang)
}

/// load() for a plugin whether or not it's switched on (the test harness, a plugin being written).
pub fn load_unchecked(plugin: &Installed, lang: &str) -> Result<LuaPlugin, String> {
    let mut context = PluginContext::new(plugin.info(), &plugin.folder, plugin.permissions.clone());
    context.set_language(lang);
    let guest = LuaGuest::load(&mut HostCalls::new(&mut context, None), &plugin.main)?;
    Ok(GuestPlugin::new(context, guest))
}

impl PluginFolder {
    /// (plugins to register, the ones that are on but couldn't load, error filled in). A plugin that fails never stops the rest.
    pub fn load_enabled(&self, lang: &str) -> (Vec<Box<dyn Plugin>>, Vec<Installed>) {
        let (ready, mut failed) = self.enabled();
        let mut loaded: Vec<Box<dyn Plugin>> = Vec::new();
        for mut plugin in ready {
            match load(&plugin, lang) {
                Ok(p) => {
                    log::info!("Loaded plugin {} {}", plugin.name, plugin.version);
                    loaded.push(Box::new(p));
                }
                Err(e) => {
                    log::error!("Plugin {} couldn't load: {e}", plugin.name);
                    plugin.error = capped(&e);
                    failed.push(plugin);
                }
            }
        }
        (loaded, failed)
    }
}
