//! Which built-in plugins are switched off or on (plugin_loader.py's "plugins" section of the settings). The added
//! plugins' part of the section is the plugin host's and is kept as it is.

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

/// What plugin_loader._save_state leaves alone.
const KEPT: [&str; 2] = ["check_updates", "auto_update"];

fn names(cfg: &Value, key: &str) -> BTreeSet<String> {
    cfg.pointer(&format!("/plugins/{key}")).and_then(Value::as_array).into_iter().flatten()
        .filter_map(Value::as_str).map(String::from).collect()
}

/// The built-in plugins the person turned off.
pub fn builtin_off(cfg: &Value) -> BTreeSet<String> {
    names(cfg, "off")
}

/// The optional built-in plugins the person switched on (they start out off).
pub fn optional_on(cfg: &Value) -> BTreeSet<String> {
    names(cfg, "on")
}

/// Turned off but not yet mentioned in a banner.
pub fn unnoticed_off(cfg: &Value) -> BTreeSet<String> {
    let noticed = names(cfg, "noticed");
    builtin_off(cfg).difference(&noticed).cloned().collect()
}

/// The added plugins that are on, by name with their fingerprint.
fn added(cfg: &Value) -> Map<String, Value> {
    cfg.pointer("/plugins/added").and_then(Value::as_object).into_iter().flatten()
        .filter(|(_, v)| v.is_string()).map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// _save_state: writes the switches back the way Python orders them.
fn save_state(cfg: &mut Value, off: &BTreeSet<String>, noticed: &BTreeSet<String>, on: &BTreeSet<String>) {
    let mut added: Vec<(String, Value)> = added(cfg).into_iter().collect();
    added.sort_by(|a, b| a.0.cmp(&b.0));
    let mut section = Map::new();
    section.insert("off".into(), json!(off));
    section.insert("noticed".into(), json!(noticed.intersection(off).collect::<Vec<_>>()));
    section.insert("added".into(), Value::Object(added.into_iter().collect()));
    if let Some(old) = cfg.get("plugins").and_then(Value::as_object) {
        for (k, v) in old.iter().filter(|(k, _)| KEPT.contains(&k.as_str())) {
            section.insert(k.clone(), v.clone());
        }
    }
    if !on.is_empty() {
        section.insert("on".into(), json!(on));
    }
    if let Value::Object(map) = cfg {
        map.insert("plugins".into(), Value::Object(section));
    }
}

pub fn set_builtin_off(cfg: &mut Value, name: &str, off: bool) {
    let mut names = builtin_off(cfg);
    if off { names.insert(name.into()); } else { names.remove(name); }
    let (noticed, on) = (self::names(cfg, "noticed"), optional_on(cfg));
    save_state(cfg, &names, &noticed, &on);
}

pub fn set_optional_on(cfg: &mut Value, name: &str, on: bool) {
    let mut names = optional_on(cfg);
    if on { names.insert(name.into()); } else { names.remove(name); }
    let (off, noticed) = (builtin_off(cfg), self::names(cfg, "noticed"));
    save_state(cfg, &off, &noticed, &names);
}

/// The person has seen which built-in plugins are off: no reminder until another one is.
pub fn notice_off(cfg: &mut Value) {
    let (off, on) = (builtin_off(cfg), optional_on(cfg));
    save_state(cfg, &off, &off.clone(), &on);
}

/// The standard selection: every built-in plugin on, the optional ones off.
pub fn reset_to_defaults(cfg: &mut Value) {
    save_state(cfg, &BTreeSet::new(), &BTreeSet::new(), &BTreeSet::new());
}

/// load_plugins: whether a built-in plugin starts this run.
pub fn starts(cfg: &Value, name: &str, required: bool, optional: bool) -> bool {
    required || if optional { optional_on(cfg).contains(name) } else { !builtin_off(cfg).contains(name) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_keeps_added_plugins_and_update_switches() {
        let mut cfg = json!({"plugins": {"off": ["notes"], "added": {"z": "2", "a": "1"}, "auto_update": false, "junk": 1}});
        set_builtin_off(&mut cfg, "desk", true);
        assert_eq!(cfg["plugins"], json!({"off": ["desk", "notes"], "noticed": [], "added": {"a": "1", "z": "2"}, "auto_update": false}));
        assert_eq!(unnoticed_off(&cfg).len(), 2);
        notice_off(&mut cfg);
        assert!(unnoticed_off(&cfg).is_empty());
        set_builtin_off(&mut cfg, "notes", false);
        assert_eq!(cfg["plugins"]["noticed"], json!(["desk"]));
    }

    #[test]
    fn an_optional_plugin_starts_off_until_switched_on() {
        let mut cfg = json!({});
        assert!(!starts(&cfg, "browser_camera", false, true));
        set_optional_on(&mut cfg, "browser_camera", true);
        assert!(starts(&cfg, "browser_camera", false, true));
        assert_eq!(cfg["plugins"]["on"], json!(["browser_camera"]));
        reset_to_defaults(&mut cfg);
        assert!(!starts(&cfg, "browser_camera", false, true));
    }

    #[test]
    fn required_plugins_always_start() {
        let mut cfg = json!({});
        set_builtin_off(&mut cfg, "connection", true);
        assert!(starts(&cfg, "connection", true, false));
        assert!(!starts(&cfg, "notes", false, false) || builtin_off(&cfg).len() == 1);
    }

    #[test]
    fn junk_in_the_section_is_ignored() {
        let cfg = json!({"plugins": {"off": [1, "a", null], "on": "x"}});
        assert_eq!(builtin_off(&cfg).into_iter().collect::<Vec<_>>(), ["a"]);
        assert!(optional_on(&cfg).is_empty());
    }
}
