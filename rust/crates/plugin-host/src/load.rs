//! Making plugins out of installed packages: the plugin_loader.load and load_enabled half that runs code.

use crate::guest::{GuestPlugin, HostCalls, PluginContext};
use crate::lua::LuaGuest;
use crate::package::{capped, Installed, PluginFolder};
use crate::text::tr;
use telescope_core::services::{PluginProblem, PluginProblems};
use telescope_core::Plugin;

/// A Lua plugin that's ready to register.
pub type LuaPlugin = GuestPlugin<LuaGuest>;

/// Runs a plugin's main.lua and wraps it as a core Plugin; lang picks its locale/<lang>.po. Says what went wrong otherwise.
pub fn load(plugin: &Installed, lang: &str) -> Result<LuaPlugin, String> {
    if !plugin.problem.is_empty() || !plugin.enabled {
        return Err(if plugin.problem.is_empty() { tr("The plugin isn't turned on.", &[]) } else { plugin.problem.clone() });
    }
    load_unchecked(plugin, lang)
}

/// load() for a plugin whether or not it's switched on (the test harness, a plugin being written).
pub fn load_unchecked(plugin: &Installed, lang: &str) -> Result<LuaPlugin, String> {
    let mut context = PluginContext::new(plugin.info(), &plugin.folder, plugin.permissions.clone());
    context.set_language(lang);
    context.snapshot_modules();
    let guest = LuaGuest::load(&mut HostCalls::new(&mut context, None), &plugin.main)?;
    Ok(GuestPlugin::new(context, guest))
}

impl PluginFolder {
    /// (plugins to register, the ones that are on but couldn't load, error filled in). A plugin that fails never stops the rest.
    pub fn load_enabled(&self, lang: &str) -> (Vec<Box<dyn Plugin>>, Vec<Installed>) {
        self.load_enabled_gated(lang, &|_| None)
    }

    /// load_enabled() where needs says why a plugin with those built-in plugin names in its "needs" can't start (None: it can), asked before
    /// any of the plugin's code runs. A second folder with a name that's already loaded is left out, before it can touch the first one's values.
    pub fn load_enabled_gated(&self, lang: &str, needs: &NeedsGate) -> (Vec<Box<dyn Plugin>>, Vec<Installed>) {
        let (ready, mut failed) = self.enabled();
        let mut loaded: Vec<Box<dyn Plugin>> = Vec::new();
        for mut plugin in ready {
            if loaded.iter().any(|l| l.info().name == plugin.name) {
                log::error!("Plugin folder {} has the name of one that's already loaded and was left out", plugin.folder.display());
                continue;
            }
            if let Some(why) = needs(&plugin.manifest.needs()) {
                log::warn!("Plugin {} didn't start: {why}", plugin.name);
                plugin.error = capped(&why);
                plugin.blocked = true;
                failed.push(plugin);
                continue;
            }
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

/// Says why an added plugin with these names in its "needs" can't start, None when the built-in plugins are there.
pub type NeedsGate = dyn Fn(&[String]) -> Option<String>;

/// What the binary asks for the added plugins, given the gate that checks their "needs" before their code runs.
pub type AddedFactory = Box<dyn Fn(&NeedsGate) -> Vec<Box<dyn Plugin>> + Send + Sync>;

/// The added plugins for the binary: every one that's switched on, and any being written (TELESCOPE_DEV_PLUGINS), loaded afresh
/// each time the shell builds its plugins. reserved are the built-in plugins' names; lang picks each plugin's locale/<lang>.po. The ones
/// that are on but can't load go in problems, for the window to say so.
pub fn factory(reserved: Vec<String>, lang: String, problems: PluginProblems) -> AddedFactory {
    factory_in(move || PluginFolder::app(reserved.clone()), lang, problems)
}

/// factory() for any plugins folder.
pub(crate) fn factory_in(folder: impl Fn() -> PluginFolder + Send + Sync + 'static, lang: String, problems: PluginProblems) -> AddedFactory {
    Box::new(move |needs| {
        let (loaded, failed) = folder().load_enabled_gated(&lang, needs);
        for plugin in &failed {
            log::warn!("Plugin {} didn't start: {}", plugin.name, plugin.error);
        }
        *problems.lock().unwrap_or_else(|e| e.into_inner()) = failed.into_iter()
            .map(|p| PluginProblem { title: if p.title.is_empty() { p.name.clone() } else { p.title }, name: p.name, error: p.error, blocked: p.blocked }).collect();
        loaded
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::tests::{plugin, plugins};
    use crate::switches::Switches;
    use serde_json::json;
    use telescope_core::config::ConfigFile;

    #[test]
    fn a_plugin_whose_needs_arent_met_runs_none_of_its_code_and_is_listed_as_blocked() {
        let p = plugins();
        // Its code would fail to compile, which would mask the missing plugin if it ran first
        plugin(&p.folder, "needy", "this is not lua(", json!({"needs": ["photos"]}));
        plugin(&p.folder, "fine", "return {}\n", json!({"needs": ["photos"]}));
        for item in p.find_installed() {
            p.set_enabled(&item, true).unwrap();
        }
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = asked.clone();
        let gate = move |needs: &[String]| {
            seen.lock().unwrap().push(needs.to_vec());
            (!needs.is_empty()).then(|| "It needs Photos, which is switched off.".to_string())
        };
        let (loaded, failed) = p.load_enabled_gated("en", &gate);
        assert!(loaded.is_empty());
        assert_eq!(failed.iter().map(|f| (f.name.as_str(), f.error.as_str(), f.blocked)).collect::<Vec<_>>(),
                   [("fine", "It needs Photos, which is switched off.", true), ("needy", "It needs Photos, which is switched off.", true)]);
        assert_eq!(asked.lock().unwrap().len(), 2);
        let (loaded, failed) = p.load_enabled_gated("en", &|_| None);
        assert_eq!(loaded.len(), 1);
        assert!(failed.len() == 1 && !failed[0].blocked, "only the one with broken code fails then: {failed:?}");
    }

    #[test]
    fn a_second_plugin_folder_with_a_loaded_plugins_name_is_left_out() {
        let (a, b) = (plugins(), plugins());
        let first = plugin(&a.folder, "meter", "return {}\n", json!({"title": "First"}));
        let second = plugin(&b.folder, "meter", "return {}\n", json!({"title": "Second"}));
        let both = PluginFolder::new(a.folder.clone(), a.switches.clone(), []).with_dev_folders(vec![first, second]);
        let (loaded, failed) = both.load_enabled("en");
        assert_eq!(loaded.iter().map(|l| l.info().title).collect::<Vec<_>>(), ["First"]);
        assert!(failed.is_empty());
    }

    #[test]
    fn plugins_that_are_on_but_cant_load_are_listed_for_the_window() {
        let p = plugins();
        plugin(&p.folder, "fine", "return {}\n", json!({}));
        plugin(&p.folder, "broken", "this is not lua(", json!({"title": "Broken thing"}));
        for item in p.find_installed() {
            p.set_enabled(&item, true).unwrap();
        }
        let (folder, config) = (p.folder.clone(), p.folder.parent().unwrap().join("telescope_config.json"));
        let problems = PluginProblems::default();
        let make = factory_in(move || PluginFolder::new(folder.clone(), Switches::new(ConfigFile::at(config.clone())), []), "en".into(), problems.clone());
        let loaded = make(&|_| None);
        assert_eq!(loaded.iter().map(|l| l.info().name).collect::<Vec<_>>(), ["fine"]);
        let listed = problems.lock().unwrap().clone();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!((listed[0].name.as_str(), listed[0].title.as_str()), ("broken", "Broken thing"));
        assert!(!listed[0].error.is_empty());
        // Each run starts the list over
        let broken = p.find_installed().into_iter().find(|i| i.name == "broken").unwrap();
        p.set_enabled(&broken, false).unwrap();
        make(&|_| None);
        assert!(problems.lock().unwrap().is_empty());
    }
}
