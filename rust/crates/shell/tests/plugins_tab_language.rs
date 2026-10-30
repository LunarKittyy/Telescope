// The Plugins tab's search looks through the names and descriptions in the language shown (its own process: the
// translator is global).
use std::collections::BTreeSet;
use telescope_core::config::ConfigFile;
use telescope_core::plugin::PluginInfo;
use telescope_plugin_host::package::PluginFolder;
use telescope_plugin_host::switches::Switches;
use telescope_plugin_host::updates::Release;
use telescope_plugin_host::Installed;
use telescope_shell::plugins_tab::{Filter, NoVision, Panel, UpdateCheck};

struct Never;

impl UpdateCheck for Never {
    fn checking(&self, _: &PluginFolder) -> bool { false }
    fn auto(&self, _: &PluginFolder) -> bool { false }
    fn newer(&self, _: &PluginFolder, _: &Installed) -> Option<Release> { None }
    fn maybe_check(&mut self, _: &PluginFolder) {}
    fn updated(&self, _: &str) -> Option<String> { None }
}

fn info(name: &str, title: &str, about: &str) -> PluginInfo {
    PluginInfo { name: name.into(), title: title.into(), about: about.into(), ..Default::default() }
}

#[test]
fn the_search_finds_by_name_and_description_in_the_language_shown() {
    telescope_core::i18n::set_translator(|m| match m {
        "Presets" => Some("Voreinstellungen".into()),
        "What Presets does." => Some("Speichert Einstellungen.".into()),
        _ => None,
    });
    let root = telescope_core::testing::scratch_dir("plugins-language");
    let folder = PluginFolder::new(root.join("plugins"), Switches::new(ConfigFile::at(root.join("telescope_config.json"))), []);
    let list = vec![info("presets", "Presets", "What Presets does."), info("zoom_tool", "Zoom tool", "Makes the picture bigger.")];
    let panel = Panel::new(list, BTreeSet::new(), Default::default(), folder, Box::new(NoVision), Box::new(Never));
    assert_eq!(panel.shown_tiles("vorein", Filter::All), ["Voreinstellungen"]);
    assert_eq!(panel.shown_tiles("speichert", Filter::All), ["Voreinstellungen"]);
    assert!(panel.shown_tiles("presets", Filter::All).is_empty(), "the English isn't what's shown");
    assert_eq!(panel.shown_tiles("picture bigger", Filter::All), ["Zoom tool"]);
    telescope_core::i18n::set_translator(|_| None);
}
