//! plugin.json: what a plugin is, read without running any of it.

use serde_json::Value;
use telescope_core::plugin::PluginInfo;

pub const MANIFEST: &str = "plugin.json";
/// The script a plugin starts from unless plugin.json names another ("main").
pub const DEFAULT_MAIN: &str = "main.lua";

/// The plugin API this Telescope offers: Lua plugins are 2. 1 was the Python app's, whose plugins can't run here.
pub const PLUGIN_API: i64 = 2;
/// The oldest plugin API still loaded.
pub const OLDEST_PLUGIN_API: i64 = 2;

/// Longest title, author or description shown.
pub const TEXT_MAX: usize = 300;
const REPO_MAX: usize = 200;
pub const NEEDS_NOT_NAMES: &str = "\"needs\" in plugin.json has to be a list of built-in plugin names, like [\"photos\"].";
pub const NEEDS_TOO_MANY: &str = "\"needs\" in plugin.json lists more than 16 plugins.";

/// What a plugin may ask for in "permissions", and what each one lets it do. Everything else in the host API needs none.
pub const PERMISSIONS: &[(&str, &str)] = &[
    ("phone", "Change the phone's camera settings and take photos"),
    ("streams", "Start and stop streams and change the virtual camera"),
    ("app", "Quit or restart Telescope, show and hide its window"),
    ("settings", "Read other plugins' settings, apply presets and move a phone's settings"),
    ("notifications", "Show desktop notifications"),
];

/// Manifest text as one plain line: no control or invisible format characters, capped (_text).
pub fn text(value: Option<&Value>) -> String {
    let raw = match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return String::new(),
    };
    let clean: String = raw.chars().filter(|c| !c.is_control() && !telescope_core::names::is_format_char(*c)).collect();
    clean.trim().chars().take(TEXT_MAX).collect()
}

/// Whether name can be a plugin's name and folder: [a-z][a-z0-9_]{1,31}, and not one Windows keeps for a device (con, nul, com1).
pub fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !telescope_core::names::is_device_name(name.split('.').next().unwrap_or(name))
        && (2..=32).contains(&b.len()) && b[0].is_ascii_lowercase() && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// Why a manifest's "repo" can't be shown as a link ("" when it can, or when there's none). Only the test harness shows it, in English.
pub fn repo_problem(value: Option<&Value>) -> String {
    let Some(value) = value.filter(|v| !v.is_null()) else { return String::new() };
    let ok = value.as_str().is_some_and(|s| s.len() <= REPO_MAX && valid_repo(s));
    if ok {
        String::new()
    } else {
        format!("\"repo\" has to be an https:// address of up to {REPO_MAX} characters, like https://github.com/you/your_plugin.")
    }
}

/// https://host[:port][/path] with only the characters plugin_loader._REPO allows.
fn valid_repo(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("https://") else { return false };
    let (authority, path) = rest.find('/').map_or((rest, ""), |i| (&rest[..i], &rest[i..]));
    let (host, port) = authority.split_once(':').map_or((authority, None), |(h, p)| (h, Some(p)));
    let host_ok = !host.is_empty() && host.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-');
    let port_ok = port.is_none_or(|p| (1..=5).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_digit()));
    let path_ok = path.bytes().all(|c| c.is_ascii_alphanumeric() || b"._~%!$&'()*+,;=:@/-".contains(&c));
    host_ok && port_ok && path_ok
}

/// A plugin.json, read as far as it goes; the loader says what's missing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manifest {
    pub raw: Value,
}

impl Manifest {
    pub fn new(raw: Value) -> Manifest {
        Manifest { raw }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.raw.get(key)
    }

    pub fn api(&self) -> i64 {
        match self.get("api") {
            Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
            _ => 0,
        }
    }

    /// The script to start from, when "main" is a .lua file inside the plugin; None for anything else.
    pub fn main(&self) -> Option<String> {
        match self.get("main") {
            None | Some(Value::Null) => Some(DEFAULT_MAIN.to_string()),
            Some(Value::String(s)) if s.ends_with(".lua") && crate::package::safe_relative(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// The permissions it asks for that this Telescope knows; unknown ones are left out.
    pub fn permissions(&self) -> Vec<String> {
        let list = self.get("permissions").and_then(Value::as_array);
        let mut out: Vec<String> = list.into_iter().flatten().filter_map(Value::as_str)
            .filter(|p| PERMISSIONS.iter().any(|(k, _)| k == p)).map(str::to_string).collect();
        out.sort();
        out.dedup();
        out
    }

    /// The class attributes of a TelescopePlugin, from the keys of the same names. An added plugin is never required or optional.
    pub fn info(&self, name: &str) -> PluginInfo {
        let mut info: PluginInfo = serde_json::from_value(self.raw.clone()).unwrap_or_else(|_| {
            // One key of the wrong type mustn't cost the others
            let mut info = PluginInfo::default();
            if let Value::Object(map) = &self.raw {
                for (k, v) in map {
                    let mut probe = serde_json::to_value(&info).unwrap_or_default();
                    probe[k] = v.clone();
                    if let Ok(ok) = serde_json::from_value(probe) {
                        info = ok;
                    }
                }
            }
            info
        });
        info.name = name.to_string();
        info.title = text(self.get("title"));
        if info.title.is_empty() {
            info.title = name.to_string();
        }
        info.about = text(self.get("description"));
        info.when_off = text(self.get("when_off"));
        info.required = false;
        info.optional = false;
        info.pinned = false;
        info.needs = self.needs();
        info.added = true;
        info
    }

    /// Whether "needs" is a list of up to 16 plugin names (or absent), as a load error in the plugin's own words; None when it is.
    pub fn needs_problem(&self) -> Option<&'static str> {
        let value = self.get("needs").filter(|v| !v.is_null())?;
        let Some(list) = value.as_array() else { return Some(NEEDS_NOT_NAMES) };
        if list.len() > 16 {
            return Some(NEEDS_TOO_MANY);
        }
        (!list.iter().all(|n| n.as_str().is_some_and(valid_name))).then_some(NEEDS_NOT_NAMES)
    }

    /// "needs": the built-in plugins it can't work without, as names that could be a plugin's, each once and at most 16. Anything else is left out.
    pub fn needs(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for name in self.get("needs").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            if valid_name(name) && !out.iter().any(|n| n == name) && out.len() < 16 {
                out.push(name.to_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_needs_that_isnt_a_list_of_plugin_names_is_a_problem() {
        let problem = |v: Value| Manifest::new(json!({"needs": v})).needs_problem();
        assert_eq!(problem(json!("photos")), Some(NEEDS_NOT_NAMES));
        assert_eq!(problem(json!(["photos", 3])), Some(NEEDS_NOT_NAMES));
        assert_eq!(problem(json!(["Bad Name"])), Some(NEEDS_NOT_NAMES));
        assert_eq!(problem(json!((0..17).map(|i| format!("p{i}")).collect::<Vec<_>>())), Some(NEEDS_TOO_MANY));
        assert_eq!(problem(json!(["photos", "preview"])), None);
        assert_eq!(problem(json!([])), None);
        assert_eq!(Manifest::new(json!({})).needs_problem(), None);
    }

    #[test]
    fn needs_keeps_plugin_names_only() {
        let m = Manifest::new(json!({"needs": ["photos", "photos", "Bad Name", 3, "motion_x"]}));
        assert_eq!(m.info("x").needs, ["photos", "motion_x"]);
        assert!(Manifest::new(json!({"needs": "photos"})).info("x").needs.is_empty());
    }

    #[test]
    fn info_comes_from_the_manifest_and_one_bad_key_costs_only_itself() {
        let m = Manifest::new(json!({"name": "x", "title": "Shiny\nthing", "description": "Does it", "frame_stage": "overlay",
                                     "device_local": "yes", "required": true, "icon": "sparkle"}));
        let info = m.info("x");
        assert_eq!((info.title.as_str(), info.about.as_str(), info.icon.as_str()), ("Shinything", "Does it", "sparkle"));
        assert_eq!(info.frame_stage, telescope_core::FrameStage::Overlay);
        assert!(!info.device_local && !info.required);
    }

    #[test]
    fn a_stage_that_doesnt_exist_counts_as_effects() {
        let info = Manifest::new(json!({"name": "x", "frame_stage": "nowhere"})).info("x");
        assert_eq!(info.frame_stage, telescope_core::FrameStage::Effects);
    }

    #[test]
    fn names_and_main_scripts() {
        for device in ["con", "aux", "nul", "prn", "com1", "com9", "lpt1", "lpt9"] {
            assert!(!valid_name(device), "{device}");
        }
        assert!(valid_name("com10") && valid_name("console") && valid_name("com0"));
        assert!(valid_name("black_and_white") && !valid_name("Bad-Name") && !valid_name("a") && !valid_name("_x"));
        assert_eq!(Manifest::new(json!({})).main().as_deref(), Some("main.lua"));
        assert_eq!(Manifest::new(json!({"main": "src/plugin.lua"})).main().as_deref(), Some("src/plugin.lua"));
        assert_eq!(Manifest::new(json!({"main": "../x.lua"})).main(), None);
        assert_eq!(Manifest::new(json!({"main": "x.py"})).main(), None);
        assert_eq!(Manifest::new(json!({"permissions": ["phone", "root", "phone"]})).permissions(), ["phone"]);
    }

    #[test]
    fn manifest_text_drops_bidi_and_other_invisible_characters() {
        let shown = text(Some(&json!("Safe\u{202E}gpj.exe \u{2066}x\u{2069}\u{200B}\u{FEFF}y")));
        assert_eq!(shown, "Safegpj.exe xy");
    }
}
