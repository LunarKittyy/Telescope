//! `telescope-rs config-snapshot`: what this app makes of a fixture's settings file, in the shape of desktop/scripts/config_fixtures.py's expected.json.

use crate::config::{self, ConfigFile, PluginSection, WindowState, CONFIG_FILENAME};
use crate::json::{self, Style};
use serde_json::{json, Value};
use std::path::Path;

/// What the app made up on the spot shows as this (a new computer id, this computer's name, a new shortcut id).
pub const NEW: &str = "<new>";

/// Loads folder's telescope_config.json (none for a fresh install) in scratch, starts as the window does and saves straight away.
pub fn snapshot(folder: &Path, scratch: &Path, plugins: &[String], sections: &[PluginSection]) -> std::io::Result<Value> {
    let path = scratch.join(CONFIG_FILENAME);
    let source = folder.join(CONFIG_FILENAME);
    if source.exists() {
        std::fs::create_dir_all(scratch)?;
        std::fs::copy(&source, &path)?;
    }
    let file = ConfigFile::at(&path);
    // apply_saved_config, then save_now, which reads the file again before writing it
    let window = WindowState::from_config(&file.load());
    let mut cfg = file.load_strict()?;
    config::merge_for_save(&mut cfg, &window, sections);
    file.save(&mut cfg);
    result(folder, scratch, plugins)
}

/// What came of folder's settings file once it was saved in scratch, by this code or the real window.
pub fn result(folder: &Path, scratch: &Path, plugins: &[String]) -> std::io::Result<Value> {
    let set_aside = std::fs::read_dir(scratch)?.flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with(&format!("{CONFIG_FILENAME}.invalid-")));
    let mut saved = read(&scratch.join(CONFIG_FILENAME)).unwrap_or(Value::Null);
    mark_new(&mut saved, &read(&folder.join(CONFIG_FILENAME)).unwrap_or(Value::Null));
    let mut plugins = plugins.to_vec();
    plugins.sort();
    Ok(json!({"set_aside": set_aside, "plugins": plugins, "saved": saved}))
}

/// The snapshot as config_fixtures.py writes it.
pub fn dump(snapshot: &Value) -> String {
    json::dumps(snapshot, Style::FIXTURE) + "\n"
}

fn read(path: &Path) -> Option<Value> {
    let data = std::fs::read(path).ok()?;
    let text = std::str::from_utf8(data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&data)).ok()?;
    json::loads(text).ok()
}

// _mark_new: values the app makes up differ every run, so they show as NEW
fn mark_new(cfg: &mut Value, original: &Value) {
    let old_pcfg = original.get("plugin_configs").filter(|p| p.is_object());
    let old_conn = old_pcfg.and_then(|p| p.get("connection")).cloned().unwrap_or(json!({}));
    if let Some(conn) = cfg.pointer_mut("/plugin_configs/connection").and_then(Value::as_object_mut) {
        for key in ["computer_id", "computer_name"] {
            if conn.get(key).is_some_and(|v| Some(v) != old_conn.get(key)) {
                conn.insert(key.into(), NEW.into());
            }
        }
    }
    let old_ids: Vec<Value> = old_pcfg.and_then(|p| p.pointer("/shortcuts/bindings")).and_then(Value::as_array)
        .map(|b| b.iter().filter_map(|b| b.get("id").cloned()).collect()).unwrap_or_default();
    if let Some(bindings) = cfg.pointer_mut("/plugin_configs/shortcuts/bindings").and_then(Value::as_array_mut) {
        for binding in bindings.iter_mut().filter_map(Value::as_object_mut) {
            if binding.get("id").is_some_and(|id| !old_ids.contains(id)) {
                binding.insert("id".into(), NEW.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch_dir;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/configs")
    }

    fn sections(v: &Value) -> Vec<String> {
        let mut keys: Vec<String> = v.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        keys.sort();
        keys
    }

    // The parts config.py and the window decide; plugin sections become exact as plugins get ported
    #[test]
    fn config_level_parts_match_every_fixture() {
        let mut folders: Vec<PathBuf> = std::fs::read_dir(fixtures()).expect("desktop fixtures").flatten()
            .map(|e| e.path()).filter(|p| p.is_dir()).collect();
        folders.sort();
        assert!(folders.len() >= 9);
        for folder in folders {
            let name = folder.file_name().unwrap().to_string_lossy().into_owned();
            let expected: Value = json::loads(&std::fs::read_to_string(folder.join("expected.json")).unwrap()).unwrap();
            let ours = snapshot(&folder, &scratch_dir(&name), &[], &[]).unwrap();
            let (want, got) = (&expected["saved"], &ours["saved"]);
            assert_eq!(ours["set_aside"], expected["set_aside"], "{name}: set_aside");
            assert_eq!(sections(got), sections(want), "{name}: top-level keys");
            for (key, value) in want.as_object().unwrap() {
                match key.as_str() {
                    "plugin_configs" | "devices" => {}
                    // Connection picks a phone when the saved choice was unusable
                    "selected_device" if got[key].is_null() && !value.is_null() => assert_eq!(name, "damaged-sections", "{name}: selected_device"),
                    _ => assert_eq!(&got[key], value, "{name}: {key}"),
                }
            }
            // Every section kept is one Python kept too (the v2 migration drops connection); plugins add their defaults
            for section in sections(&got["plugin_configs"]) {
                // A plugin that now keeps settings per phone moves its old shared copy onto each phone (_spread_shared_settings)
                let spread = want["devices"].as_object().unwrap().values().any(|d| d["plugin_configs"].get(&section).is_some());
                assert!(want["plugin_configs"].get(&section).is_some() || spread, "{name}: plugin_configs.{section}");
            }
            for (device, entry) in got["devices"].as_object().unwrap() {
                match want["devices"].get(device) {
                    Some(w) => {
                        let fields = |e: &Value| e.as_object().unwrap().iter().filter(|(k, _)| *k != "plugin_configs")
                            .map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>();
                        assert_eq!(fields(entry), fields(w), "{name}: devices.{device}");
                    }
                    // Browser camera forgets browsers it hasn't seen for 60 days
                    None => assert!(device.starts_with("browser:"), "{name}: devices.{device} should be gone"),
                }
            }
            for device in sections(&want["devices"]) {
                // Connection's picked phone gets its device-local plugins' settings
                let picked = want["selected_device"].as_str() == Some(device.as_str());
                assert!(got["devices"].get(&device).is_some() || picked, "{name}: devices.{device} missing");
            }
        }
    }

    #[test]
    fn there_is_a_fixture_for_each_kind_of_file() {
        let names: Vec<String> = std::fs::read_dir(fixtures()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        for kind in ["fresh-install", "cut-off", "version-1", "version-2", "future-version", "damaged-sections"] {
            assert!(names.iter().any(|n| n == kind), "no fixture called {kind}");
        }
    }

    #[test]
    fn dump_matches_the_fixture_format() {
        let folder = fixtures().join("fresh-install");
        let ours = snapshot(&folder, &scratch_dir("fresh"), &[], &[]).unwrap();
        assert_eq!(dump(&ours), "{\n  \"plugins\": [],\n  \"saved\": {\n    \"collapsed\": [],\n    \"devices\": {},\n    \"plugin_configs\": {},\n    \"selected_device\": null,\n    \"version\": 3\n  },\n  \"set_aside\": false\n}\n");
    }
}
