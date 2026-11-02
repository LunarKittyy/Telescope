//! Presets, Startup and the Wait screen against what the Python app does. tests/fixtures/cards3_configs.json holds what each
//! plugin saves after loading odd settings (recorded from the Python app).

use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use telescope_core::plugin::Plugin;
use telescope_core::system::Autostart;
use telescope_outputs::wait_screen_plugin::WaitScreenPlugin;
use telescope_shell::plugins::presets::PresetsPlugin;
use telescope_system::startup::StartupPlugin;

struct Never;

impl Autostart for Never {
    fn enabled(&self) -> bool {
        false
    }
    fn set_enabled(&self, _on: bool, _minimized: bool) -> Result<(), String> {
        Ok(())
    }
}

fn fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cards3_configs.json");
    telescope_core::json::loads(&std::fs::read_to_string(path).expect("cards3_configs.json")).unwrap()
}

/// A saved number is the same number, whether it was written 100 or 100.0.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-9 * x.abs().max(1.0),
            _ => false,
        },
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().zip(y.iter()).all(|((ka, va), (kb, vb))| ka == kb && same(va, vb)),
        _ => a == b,
    }
}

#[test]
fn settings_load_and_save_as_python_does() {
    let mut checked = std::collections::BTreeMap::new();
    let cases = fixture();
    for case in cases.as_array().unwrap() {
        let name = case["plugin"].as_str().unwrap();
        let mut plugin: Box<dyn Plugin> = match name {
            "presets" => Box::new(PresetsPlugin::new()),
            "startup" => Box::new(StartupPlugin::new(Arc::new(Never))),
            "wait_screen" => Box::new(WaitScreenPlugin::new(Arc::new(telescope_outputs::VirtualCameras::default()))),
            other => panic!("{other}"),
        };
        let loaded = plugin.set_config(&case["input"]);
        if case["output"].is_null() {
            assert!(loaded.is_err(), "{name} given {} should be refused", case["input"]);
            continue;
        }
        loaded.unwrap();
        let got = plugin.get_config();
        assert!(same(&got, &case["output"]), "{name} given {}:\n  rust   {got}\n  python {}", case["input"], case["output"]);
        *checked.entry(name).or_insert(0) += 1;
    }
    for name in ["presets", "startup", "wait_screen"] {
        assert!(checked.get(name).copied().unwrap_or(0) > 10, "{name}: {checked:?}");
    }
}
