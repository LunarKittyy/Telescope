//! How a plugin is named for people, shared by the plugins that list others' costs.

use telescope_core::host::PluginLook;
use telescope_core::i18n::tr;

/// "privacy_blur" as a name for people.
pub(crate) fn plugin_label(name: &str) -> String {
    let mut chars = name.replace('_', " ").chars().collect::<Vec<_>>();
    if let Some(first) = chars.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    chars.into_iter().collect()
}

/// A plugin's name as people read it (what the Plugins tab calls it, translated), or the id tidied up when no plugin by that name is known.
pub(crate) fn plugin_title(name: &str, looks: &[PluginLook]) -> String {
    match looks.iter().find(|l| l.name == name && !l.title.is_empty()) {
        Some(look) => tr(&look.title),
        None => plugin_label(name),
    }
}
