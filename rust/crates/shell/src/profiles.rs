//! Each device's own copy of the device-local plugins' settings (app.py's device profile methods), as plain changes
//! to the loaded settings so they can be tested without a window.

use serde_json::{json, Map, Value};

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
/// device with settings saved (and the picked one) off with that copy, then the shared copy goes. True if it changed.
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

/// move_device_settings: a phone back with a new id (Telescope reinstalled on it) takes its settings and camera along.
pub fn move_device(cfg: &mut Value, old: &str, new: &str) {
    for key in ["devices", "output_slots"] {
        if let Some(map) = cfg.get_mut(key).and_then(Value::as_object_mut) {
            if let Some(v) = map.remove(old) {
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
    fn a_phone_back_with_a_new_id_takes_its_settings_and_virtual_camera() {
        let presets = json!({"presets": {"items": [{"name": "Desk"}]}});
        let mut cfg = json!({"devices": {"old": {"plugin_configs": presets}, "other": {"plugin_configs": {}}},
                             "output_slots": {"old": 2}, "selected_device": "old"});
        move_device(&mut cfg, "old", "new");
        assert_eq!(cfg["devices"]["new"]["plugin_configs"], presets);
        assert!(cfg["devices"].get("old").is_none());
        assert_eq!(cfg["output_slots"], json!({"new": 2}));
        assert_eq!(cfg["selected_device"], "new");
    }

    #[test]
    fn forgetting_a_phone_drops_its_settings_and_camera() {
        let mut cfg = json!({"devices": {"a": {}}, "output_slots": {"a": 1}});
        assert!(forget_device(&mut cfg, "a"));
        assert!(!forget_device(&mut cfg, "a"));
        assert_eq!(cfg, json!({"devices": {}, "output_slots": {}}));
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
