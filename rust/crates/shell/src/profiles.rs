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

/// forget_device_settings: a removed phone's settings and virtual camera go, and it no longer needs the start's cleanup to spare it.
/// True if anything did.
pub fn forget_device(cfg: &mut Value, id: &str) -> bool {
    let a = cfg.get_mut("devices").and_then(Value::as_object_mut).and_then(|d| d.remove(id)).is_some();
    let b = cfg.get_mut("output_slots").and_then(Value::as_object_mut).and_then(|d| d.remove(id)).is_some();
    let c = set_kept(cfg, |kept| kept.retain(|k| k != id));
    a || b || c
}

/// A source id that is a phone's own: browsers and webcams carry a prefix (`browser:..`, `webcam:..`), a phone's id has none.
fn is_phone_id(id: &str) -> bool {
    id != "browser" && !id.contains(':')
}

/// The top-level list of phone ids whose settings the start's cleanup must leave alone.
const KEPT: &str = "spared_phone_settings";

fn kept(cfg: &Value) -> Vec<String> {
    cfg.get(KEPT).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

/// Changes the list of spared ids (the key goes when it is empty). True if the list is different afterwards.
fn set_kept(cfg: &mut Value, change: impl FnOnce(&mut Vec<String>)) -> bool {
    let before = kept(cfg);
    let mut after = before.clone();
    change(&mut after);
    if after == before {
        return false;
    }
    let Some(map) = cfg.as_object_mut() else { return false };
    if after.is_empty() {
        map.shift_remove(KEPT);
    } else {
        map.insert(KEPT.into(), json!(after));
    }
    true
}

/// The id of a phone entry Connection can read (it needs an id, a name and a token, and `ips`, when there, a list of text).
fn readable_phone_id(entry: &Value) -> Option<&str> {
    let text = |k: &str| entry.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let ips_ok = entry.get("ips").is_none_or(|ips| ips.as_array().is_some_and(|l| l.iter().all(Value::is_string)));
    let (id, _, _) = (text("id")?, text("name")?, text("token")?);
    ips_ok.then_some(id)
}

/// drop_unpaired_phones: settings and virtual cameras left behind by a phone that was removed on an earlier run go, but only
/// when it is sure the phone is gone: the file is of this version and Connection's list is a non-empty list of entries it can all
/// read. Anything else (a reset or unreadable section, an empty list, an entry that lost its id or token) is a list Connection
/// may shorten when it next saves, so the phone ids that have settings and no readable entry are written down as spared first
/// and never go in a later cleanup (an orphan is cheaper than a phone's lost settings). A removal already forgets the phone's
/// settings on the spot. Only entries that are phone ids count. True if the file changed.
pub fn drop_unpaired_phones(cfg: &mut Value) -> bool {
    let entries = cfg.pointer("/plugin_configs/connection/phones").and_then(Value::as_array).cloned().unwrap_or_default();
    let readable: Vec<String> = entries.iter().filter_map(readable_phone_id).map(str::to_string).collect();
    let complete = !entries.is_empty() && readable.len() == entries.len();
    let with_settings: Vec<String> = ["devices", "output_slots"].into_iter().filter_map(|key| cfg.get(key).and_then(Value::as_object))
        .flat_map(|map| map.keys()).filter(|id| is_phone_id(id)).cloned().collect();
    if !complete {
        return set_kept(cfg, |kept| {
            for id in with_settings.iter().filter(|id| !readable.contains(id)) {
                if !kept.contains(id) {
                    kept.push(id.clone());
                }
            }
        });
    }
    if cfg.get("version").and_then(Value::as_i64) != Some(CONFIG_VERSION) {
        return false;
    }
    // Phones listed again no longer need sparing: if they are removed some day, the removal says so
    let mut changed = set_kept(cfg, |kept| kept.retain(|id| !readable.contains(id)));
    let spared = kept(cfg);
    for key in ["devices", "output_slots"] {
        if let Some(map) = cfg.get_mut(key).and_then(Value::as_object_mut) {
            let before = map.len();
            map.retain(|id, _| !is_phone_id(id) || readable.contains(id) || spared.contains(id));
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

    fn phone(id: &str) -> Value {
        json!({"id": id, "name": "Phone", "token": "tok"})
    }

    #[test]
    fn settings_of_phones_that_are_no_longer_paired_go_but_browsers_and_webcams_stay() {
        let mut cfg = json!({
            "version": 3,
            "plugin_configs": {"connection": {"phones": [phone("kept")]}},
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
        drop_unpaired_phones(&mut cfg);
        cfg["plugin_configs"]["connection"] = json!({"phones": "damaged"});
        drop_unpaired_phones(&mut cfg);
        assert_eq!(cfg["devices"], before["devices"]);
        assert_eq!(cfg["output_slots"], before["output_slots"]);
    }

    #[test]
    fn an_empty_list_drops_nothing_because_it_is_what_a_reset_section_looks_like() {
        let mut cfg = json!({"version": 3, "plugin_configs": {"connection": {"phones": []}}, "devices": {"a": {}}, "output_slots": {"a": 1}});
        let before = cfg.clone();
        drop_unpaired_phones(&mut cfg);
        assert_eq!(cfg["devices"], before["devices"]);
        assert_eq!(cfg["output_slots"], before["output_slots"]);
    }

    #[test]
    fn a_list_with_an_entry_connection_cannot_read_drops_nothing() {
        let bad = [json!({"name": "no id", "token": "t"}), json!("junk"), json!({"id": "", "name": "n", "token": "t"}), json!({"id": 5}),
                   json!({"id": "b"}), json!({"id": "b", "name": "n"}), json!({"id": "b", "name": "n", "token": "t", "ips": "10.0.0.5"})];
        for entry in bad {
            let mut cfg = json!({"version": 3, "plugin_configs": {"connection": {"phones": [phone("a"), entry]}},
                                 "devices": {"a": {}, "b": {}}, "output_slots": {"b": 1}});
            let before = cfg.clone();
            drop_unpaired_phones(&mut cfg);
            assert_eq!(cfg["devices"], before["devices"], "{entry}");
            assert_eq!(cfg["output_slots"], before["output_slots"], "{entry}");
        }
    }

    #[test]
    fn a_phone_whose_entry_was_damaged_keeps_its_settings_after_connection_drops_the_entry() {
        let mut cfg = json!({"version": 3, "plugin_configs": {"connection": {"phones": [phone("a"), {"name": "lost its id", "token": "t"}]}},
                             "devices": {"a": {}, "b": {}}, "output_slots": {"a": 1, "b": 2}});
        // First start: Connection can't read the second entry
        assert!(drop_unpaired_phones(&mut cfg));
        assert_eq!(cfg["devices"], json!({"a": {}, "b": {}}));
        // Connection saves the list without it, and a later start sees a list it can read in full
        cfg["plugin_configs"]["connection"]["phones"] = json!([phone("a")]);
        assert!(!drop_unpaired_phones(&mut cfg));
        assert_eq!(cfg["devices"], json!({"a": {}, "b": {}}));
        assert_eq!(cfg["output_slots"], json!({"a": 1, "b": 2}));
        // Even after a phone is paired meanwhile
        cfg["plugin_configs"]["connection"]["phones"] = json!([phone("a"), phone("c")]);
        assert!(!drop_unpaired_phones(&mut cfg));
        assert!(cfg["devices"].get("b").is_some());
    }

    #[test]
    fn a_phone_that_is_removed_after_being_spared_loses_its_settings_and_its_spared_mark() {
        let mut cfg = json!({"version": 3, "plugin_configs": {"connection": {"phones": [{"id": "b"}]}}, "devices": {"b": {}}});
        drop_unpaired_phones(&mut cfg);
        assert_eq!(cfg["spared_phone_settings"], json!(["b"]));
        assert!(forget_device(&mut cfg, "b"));
        assert!(cfg.get("spared_phone_settings").is_none());
        assert_eq!(cfg["devices"], json!({}));
    }

    #[test]
    fn a_spared_phone_that_is_listed_again_is_cleaned_up_like_any_other_when_it_goes() {
        let mut cfg = json!({"version": 3, "plugin_configs": {"connection": {"phones": []}}, "devices": {"b": {}}});
        drop_unpaired_phones(&mut cfg);
        cfg["plugin_configs"]["connection"]["phones"] = json!([phone("b")]);
        drop_unpaired_phones(&mut cfg);
        assert!(cfg.get("spared_phone_settings").is_none());
        cfg["plugin_configs"]["connection"]["phones"] = json!([phone("c")]);
        assert!(drop_unpaired_phones(&mut cfg));
        assert_eq!(cfg["devices"], json!({}));
    }

    #[test]
    fn a_file_from_another_version_drops_nothing() {
        for version in [json!(99), json!(2), json!(null), json!("3")] {
            let mut cfg = json!({"version": version, "plugin_configs": {"connection": {"phones": [phone("a")]}}, "devices": {"a": {}, "b": {}}});
            let before = cfg.clone();
            assert!(!drop_unpaired_phones(&mut cfg));
            assert_eq!(cfg, before);
        }
        let mut cfg = json!({"plugin_configs": {"connection": {"phones": [phone("a")]}}, "devices": {"a": {}, "b": {}}});
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
