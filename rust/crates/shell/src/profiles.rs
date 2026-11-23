//! Each device's own copy of the device-local plugins' settings (app.py's device profile methods), as plain changes
//! to the loaded settings so they can be tested without a window.

use serde_json::{json, Map, Value};
use telescope_core::config::CONFIG_VERSION;

/// devices.<id>.plugin_configs, made if missing (and replacing anything in the way that isn't an object).
pub fn device_section<'a>(cfg: &'a mut Value, id: &str) -> &'a mut Map<String, Value> {
    let root = obj(cfg);
    let devices = obj(root.entry("devices").or_insert_with(|| json!({})));
    let device = obj(devices.entry(id.to_string()).or_insert_with(|| json!({})));
    obj(device.entry("plugin_configs").or_insert_with(|| json!({})))
}

fn obj(v: &mut Value) -> &mut Map<String, Value> {
    if !v.is_object() {
        *v = json!({});
    }
    v.as_object_mut().unwrap()
}

/// A device's saved settings for one plugin ({} without any), as HostSettings::device_config answers.
pub fn device_config(cfg: &Value, id: Option<&str>, plugin: &str) -> Value {
    let Some(id) = id.filter(|i| !i.is_empty()) else { return json!({}) };
    match cfg.pointer(&format!("/devices/{}/plugin_configs/{}", escape(id), escape(plugin))) {
        Some(v @ Value::Object(_)) => v.clone(),
        _ => json!({}),
    }
}

/// The settings saved for a device, by plugin name ({} without any).
pub fn device_plugin_configs(cfg: &Value, id: Option<&str>) -> Map<String, Value> {
    let Some(id) = id.filter(|i| !i.is_empty()) else { return Map::new() };
    cfg.pointer(&format!("/devices/{}/plugin_configs", escape(id))).and_then(Value::as_object).cloned().unwrap_or_default()
}

pub fn has_device_settings(cfg: &Value, id: &str) -> bool {
    !device_plugin_configs(cfg, Some(id)).is_empty()
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// _spread_shared_settings: a plugin that kept one copy for the whole app and now keeps one per device starts every
/// device with settings saved (and the picked and paired ones) off with that copy, then the shared copy goes. True if it changed.
pub fn spread_shared_settings(cfg: &mut Value, device_local: &[String]) -> bool {
    let moved: Vec<String> = device_local.iter()
        .filter(|n| cfg.pointer(&format!("/plugin_configs/{}", escape(n))).is_some_and(Value::is_object)).cloned().collect();
    if moved.is_empty() {
        return false;
    }
    let mut ids: Vec<String> = cfg.get("devices").and_then(Value::as_object).into_iter().flatten()
        .filter(|(_, v)| v.is_object()).map(|(k, _)| k.clone()).collect();
    if let Some(sel) = cfg.get("selected_device").and_then(Value::as_str) {
        if !ids.iter().any(|i| i == sel) {
            ids.push(sel.to_string());
        }
    }
    let paired = cfg.pointer("/plugin_configs/connection/phones").and_then(Value::as_array).into_iter().flatten()
        .filter_map(|p| p.get("id").and_then(Value::as_str)).map(str::to_string).collect::<Vec<_>>();
    for id in paired {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    for name in moved {
        let section = cfg["plugin_configs"].as_object_mut().and_then(|p| p.remove(&name)).unwrap_or(json!({}));
        for id in &ids {
            let device = obj(cfg).entry("devices").or_insert_with(|| json!({}));
            let entry = obj(device).entry(id.clone()).or_insert_with(|| json!({}));
            if !entry.is_object() {
                continue;
            }
            let pcfg = obj(entry).entry("plugin_configs").or_insert_with(|| json!({}));
            if let Value::Object(pcfg) = pcfg {
                pcfg.entry(name.clone()).or_insert_with(|| section.clone());
            }
        }
    }
    true
}

/// _drop_unchanged: a source that's only remembered once changed (a browser) keeps no settings still at the defaults.
pub fn drop_unchanged(cfg: &mut Value, changes_only: &[String], defaults: &Map<String, Value>) {
    let Some(devices) = cfg.get_mut("devices").and_then(Value::as_object_mut) else { return };
    for name in changes_only {
        let Some(device) = devices.get_mut(name) else { continue };
        if let Some(pcfg) = device.get_mut("plugin_configs").and_then(Value::as_object_mut) {
            pcfg.retain(|k, v| defaults.get(k) != Some(v));
            if !pcfg.is_empty() {
                continue;
            }
        }
        devices.remove(name);
    }
}

/// forget_device_settings: a removed phone's settings and virtual camera go. True if anything did.
pub fn forget_device(cfg: &mut Value, id: &str) -> bool {
    let a = cfg.get_mut("devices").and_then(Value::as_object_mut).and_then(|d| d.remove(id)).is_some();
    let b = cfg.get_mut("output_slots").and_then(Value::as_object_mut).and_then(|d| d.remove(id)).is_some();
    a || b
}

/// A source id that is a phone's own: browsers and webcams carry a prefix (`browser:..`, `webcam:..`), a phone's id has none.
fn is_phone_id(id: &str) -> bool {
    id != "browser" && !id.contains(':')
}

/// drop_unpaired_phones: settings and virtual cameras left behind by a phone that was removed on an earlier run go. Only entries
/// that are phone ids go, and only when the file is of this version and Connection's list is a non-empty list in which every entry
/// has an id. An empty list is what a reset or unreadable Connection section saves, so it never counts as "nobody is paired" (a
/// removal already forgets the phone's settings on the spot). True if anything did.
pub fn drop_unpaired_phones(cfg: &mut Value) -> bool {
    if cfg.get("version").and_then(Value::as_i64) != Some(CONFIG_VERSION) {
        return false;
    }
    let Some(phones) = cfg.pointer("/plugin_configs/connection/phones").and_then(Value::as_array).filter(|p| !p.is_empty()) else { return false };
    let ids: Vec<Option<&str>> = phones.iter().map(|p| p.get("id").and_then(Value::as_str).filter(|id| !id.is_empty())).collect();
    if ids.contains(&None) {
        return false;
    }
    let paired: Vec<String> = ids.into_iter().flatten().map(str::to_string).collect();
    let mut changed = false;
    for key in ["devices", "output_slots"] {
        if let Some(map) = cfg.get_mut(key).and_then(Value::as_object_mut) {
            let before = map.len();
            map.retain(|id, _| !is_phone_id(id) || paired.contains(id));
            changed |= map.len() != before;
        }
    }
    changed
}

/// move_device_settings: a phone back with a new id (Telescope reinstalled on it) gets its settings and camera under it too. The old
/// entries stay until forget_device_settings, so a file written at any point in between still holds them under a phone it lists.
pub fn copy_device(cfg: &mut Value, old: &str, new: &str) {
    for key in ["devices", "output_slots"] {
        if let Some(map) = cfg.get_mut(key).and_then(Value::as_object_mut) {
            if let Some(v) = map.get(old).cloned() {
                map.insert(new.to_string(), v);
            }
        }
    }
    if cfg.get("selected_device").and_then(Value::as_str) == Some(old) {
        cfg["selected_device"] = json!(new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_once_kept_for_the_whole_app_go_to_every_phone() {
        let mut cfg = json!({
            "selected_device": "Phone",
            "plugin_configs": {"fun_pack": {"effect": "vhs"}},
            "devices": {"Phone": {"plugin_configs": {"transforms": {"zoom": 2}}}, "B": {"plugin_configs": {"fun_pack": {"effect": "crt"}}}},
        });
        assert!(spread_shared_settings(&mut cfg, &["fun_pack".into()]));
        assert!(cfg["plugin_configs"].get("fun_pack").is_none());
        assert_eq!(cfg["devices"]["Phone"]["plugin_configs"], json!({"transforms": {"zoom": 2}, "fun_pack": {"effect": "vhs"}}));
        assert_eq!(cfg["devices"]["B"]["plugin_configs"], json!({"fun_pack": {"effect": "crt"}}));
        assert!(!spread_shared_settings(&mut cfg, &["fun_pack".into()]));
    }

    #[test]
    fn the_picked_phone_gets_the_shared_copy_even_without_settings_of_its_own() {
        let mut cfg = json!({"selected_device": "New", "plugin_configs": {"fun_pack": {"effect": "vhs"}}, "devices": {}});
        spread_shared_settings(&mut cfg, &["fun_pack".into()]);
        assert_eq!(cfg["devices"]["New"]["plugin_configs"]["fun_pack"], json!({"effect": "vhs"}));
    }

    #[test]
    fn a_paired_phone_without_settings_gets_the_shared_copy_too() {
        let mut cfg = json!({"selected_device": "A", "plugin_configs": {"fun_pack": {"effect": "vhs"}, "connection": {"phones": [{"id": "A"}, {"id": "P2", "name": "x"}]}}, "devices": {}});
        spread_shared_settings(&mut cfg, &["fun_pack".into()]);
        assert_eq!(cfg["devices"]["P2"]["plugin_configs"]["fun_pack"], json!({"effect": "vhs"}));
        assert_eq!(cfg["devices"]["A"]["plugin_configs"]["fun_pack"], json!({"effect": "vhs"}));
    }

    #[test]
    fn a_phone_paired_after_the_move_starts_without_the_old_copy() {
        let mut cfg = json!({"selected_device": "A", "plugin_configs": {"fun_pack": {"effect": "vhs"}, "connection": {"phones": [{"id": "A"}]}}});
        spread_shared_settings(&mut cfg, &["fun_pack".into()]);
        cfg["plugin_configs"]["connection"]["phones"].as_array_mut().unwrap().push(json!({"id": "Later"}));
        assert!(!spread_shared_settings(&mut cfg, &["fun_pack".into()]));
        assert!(cfg["devices"].get("Later").is_none());
    }

    #[test]
    fn old_shared_settings_are_dropped_when_there_is_no_phone() {
        let mut cfg = json!({"selected_device": null, "plugin_configs": {"fun_pack": {"effect": "vhs"}}});
        assert!(spread_shared_settings(&mut cfg, &["fun_pack".into()]));
        assert!(cfg["plugin_configs"].get("fun_pack").is_none());
        assert!(cfg.get("devices").is_none_or(|d| d.as_object().is_some_and(|d| d.is_empty())));
    }

    #[test]
    fn a_phone_back_with_a_new_id_takes_its_settings_and_virtual_camera() {
        let presets = json!({"presets": {"items": [{"name": "Desk"}]}});
        let mut cfg = json!({"devices": {"old": {"plugin_configs": presets}, "other": {"plugin_configs": {}}},
                             "output_slots": {"old": 2}, "selected_device": "old"});
        copy_device(&mut cfg, "old", "new");
        assert_eq!(cfg["devices"]["new"]["plugin_configs"], presets);
        assert_eq!(cfg["devices"]["old"]["plugin_configs"], presets, "the old entry waits for the new pairing to be saved");
        assert_eq!(cfg["output_slots"], json!({"old": 2, "new": 2}));
        assert_eq!(cfg["selected_device"], "new");
        forget_device(&mut cfg, "old");
        assert_eq!(cfg["output_slots"], json!({"new": 2}));
    }

    #[test]
    fn forgetting_a_phone_drops_its_settings_and_camera() {
        let mut cfg = json!({"devices": {"a": {}}, "output_slots": {"a": 1}});
        assert!(forget_device(&mut cfg, "a"));
        assert!(!forget_device(&mut cfg, "a"));
        assert_eq!(cfg, json!({"devices": {}, "output_slots": {}}));
    }

    #[test]
    fn settings_of_phones_that_are_no_longer_paired_go_but_browsers_and_webcams_stay() {
        let mut cfg = json!({
            "version": 3,
            "plugin_configs": {"connection": {"phones": [{"id": "kept"}]}},
            "devices": {"kept": {}, "gone": {}, "browser": {}, "browser:abc": {}, "webcam:cam1": {}},
            "output_slots": {"kept": 1, "gone": 2, "browser:abc": 3, "webcam:cam1": 4},
        });
        assert!(drop_unpaired_phones(&mut cfg));
        assert_eq!(cfg["devices"], json!({"kept": {}, "browser": {}, "browser:abc": {}, "webcam:cam1": {}}));
        assert_eq!(cfg["output_slots"], json!({"kept": 1, "browser:abc": 3, "webcam:cam1": 4}));
        assert!(!drop_unpaired_phones(&mut cfg));
    }

    #[test]
    fn nothing_is_dropped_without_a_list_of_paired_phones_to_go_by() {
        let mut cfg = json!({"version": 3, "plugin_configs": {}, "devices": {"a": {}}, "output_slots": {"a": 1}});
        let before = cfg.clone();
        assert!(!drop_unpaired_phones(&mut cfg));
        cfg["plugin_configs"]["connection"] = json!({"phones": "damaged"});
        assert!(!drop_unpaired_phones(&mut cfg));
        assert_eq!(cfg["devices"], before["devices"]);
    }

    #[test]
    fn an_empty_list_drops_nothing_because_it_is_what_a_reset_section_looks_like() {
        let mut cfg = json!({"version": 3, "plugin_configs": {"connection": {"phones": []}}, "devices": {"a": {}}, "output_slots": {"a": 1}});
        let before = cfg.clone();
        assert!(!drop_unpaired_phones(&mut cfg));
        assert_eq!(cfg, before);
    }

    #[test]
    fn a_list_with_an_entry_that_has_no_id_drops_nothing() {
        for phones in [json!([{"id": "a"}, {"name": "no id"}]), json!([{"id": "a"}, "junk"]), json!([{"id": ""}]), json!([{"id": 5}])] {
            let mut cfg = json!({"version": 3, "plugin_configs": {"connection": {"phones": phones}}, "devices": {"a": {}, "b": {}}, "output_slots": {"b": 1}});
            let before = cfg.clone();
            assert!(!drop_unpaired_phones(&mut cfg), "{phones}");
            assert_eq!(cfg, before, "{phones}");
        }
    }

    #[test]
    fn a_file_from_another_version_drops_nothing() {
        for version in [json!(99), json!(2), json!(null), json!("3")] {
            let mut cfg = json!({"version": version, "plugin_configs": {"connection": {"phones": [{"id": "a"}]}}, "devices": {"a": {}, "b": {}}});
            let before = cfg.clone();
            assert!(!drop_unpaired_phones(&mut cfg));
            assert_eq!(cfg, before);
        }
        let mut cfg = json!({"plugin_configs": {"connection": {"phones": [{"id": "a"}]}}, "devices": {"a": {}, "b": {}}});
        assert!(!drop_unpaired_phones(&mut cfg), "no version at all");
    }

    #[test]
    fn a_browser_keeps_only_the_settings_that_changed() {
        let defaults: Map<String, Value> = json!({"transforms": {"zoom": 1}, "fun_pack": {"effect": "off"}}).as_object().unwrap().clone();
        let mut cfg = json!({"devices": {
            "browser:a": {"plugin_configs": {"transforms": {"zoom": 1}, "fun_pack": {"effect": "vhs"}}},
            "browser:b": {"plugin_configs": {"transforms": {"zoom": 1}}},
            "phone": {"plugin_configs": {"transforms": {"zoom": 1}}},
        }});
        drop_unchanged(&mut cfg, &["browser:a".into(), "browser:b".into()], &defaults);
        assert_eq!(cfg["devices"]["browser:a"]["plugin_configs"], json!({"fun_pack": {"effect": "vhs"}}));
        assert!(cfg["devices"].get("browser:b").is_none());
        assert_eq!(cfg["devices"]["phone"]["plugin_configs"], json!({"transforms": {"zoom": 1}}));
    }

    #[test]
    fn device_config_is_an_empty_object_without_one() {
        let cfg = json!({"devices": {"P/1": {"plugin_configs": {"transforms": {"zoom": 3}, "bad": 4}}}});
        assert_eq!(device_config(&cfg, Some("P/1"), "transforms"), json!({"zoom": 3}));
        assert_eq!(device_config(&cfg, Some("P/1"), "bad"), json!({}));
        assert_eq!(device_config(&cfg, None, "transforms"), json!({}));
        assert!(has_device_settings(&cfg, "P/1"));
        assert!(!has_device_settings(&cfg, "Q"));
    }
}
