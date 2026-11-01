//! Setup and Monitoring against what the Python app does. fixtures/setup_configs.json holds what each plugin saves after loading odd
//! settings (make_setup_fixtures.py), and the config fixtures what it ends up saving from real files.

mod common;

use serde_json::{json, Value};
use std::path::PathBuf;
use telescope_core::canvas;
use telescope_core::plugin::Plugin;
use telescope_setup::monitoring::MonitoringPlugin;

fn loaded(path: PathBuf) -> Value {
    telescope_core::json::loads(&std::fs::read_to_string(&path).expect("a fixture")).unwrap()
}

/// Exact, so 24 and 24.0 count as different: Python saves what it was given.
fn same(a: &Value, b: &Value) -> bool {
    serde_json::to_string(a).unwrap() == serde_json::to_string(b).unwrap()
}

#[test]
fn settings_load_and_save_as_python_does() {
    let cases = loaded(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/setup_configs.json"));
    let mut checked = std::collections::BTreeMap::new();
    for case in cases.as_array().unwrap() {
        let name = case["plugin"].as_str().unwrap();
        let mut plugin: Box<dyn Plugin> = if name == "setup" { Box::new(common::setup_plugin()) } else { Box::new(MonitoringPlugin::new()) };
        let result = plugin.set_config(&case["input"]);
        if case["output"].is_null() {
            assert!(result.is_err(), "{name} given {} should be refused", case["input"]);
            continue;
        }
        result.unwrap();
        let got = plugin.get_config();
        // The one place Rust means to differ: with nothing saved, or Python's false, hardware decoding is on
        let mut want = case["output"].clone();
        if name == "setup" && want["hw_decode"] == json!(false) {
            want["hw_decode"] = json!(true);
        }
        assert!(same(&got, &want), "{name} given {}:\n  rust   {got}\n  python {want}", case["input"]);
        if name == "setup" {
            let (w, h) = canvas::canvas_dims(&got);
            assert_eq!(json!([w, h]), case["canvas"], "the canvas of {}", case["input"]);
        }
        *checked.entry(name).or_insert(0) += 1;
    }
    assert!(checked["setup"] > 30 && checked["monitoring"] > 10, "{checked:?}");
}

/// What every real settings file saves for Setup, and for Monitoring on each phone, comes out of the plugins unchanged.
#[test]
fn the_config_fixtures_round_trip() {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../desktop/tests/fixtures/configs");
    let mut seen = 0;
    for entry in std::fs::read_dir(folder).unwrap().flatten().filter(|e| e.path().is_dir()) {
        let expected = loaded(entry.path().join("expected.json"));
        let saved = &expected["saved"];
        if let Some(cfg) = saved.pointer("/plugin_configs/setup") {
            let mut plugin = common::setup_plugin();
            plugin.set_config(cfg).unwrap();
            assert_eq!(&plugin.get_config(), cfg, "{:?}", entry.path());
            seen += 1;
        }
        for device in saved["devices"].as_object().into_iter().flatten().map(|(_, d)| d) {
            if let Some(cfg) = device.pointer("/plugin_configs/monitoring") {
                let mut plugin = MonitoringPlugin::new();
                plugin.set_config(cfg).unwrap();
                assert_eq!(&plugin.get_config(), cfg, "{:?}", entry.path());
                seen += 1;
            }
        }
    }
    assert!(seen >= 9, "{seen}");
}
