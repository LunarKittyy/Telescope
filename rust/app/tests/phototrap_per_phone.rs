//! Phototrap keeps its settings per phone, and a config from before that picks its old global copy up.

use serde_json::{json, Value};
use telescope_core::config::ConfigFile;
use telescope_core::testing::scratch_dir;
use telescope_effects::phototrap::PhototrapPlugin;
use telescope_shell::shell::State;

fn state_with(name: &str, config: Value) -> State {
    let file = ConfigFile::at(scratch_dir(name).join("telescope_config.json"));
    let mut config = config;
    file.save(&mut config);
    State::new(ConfigFile::at(file.path()))
}

fn value(s: &mut State) -> Value {
    s.plugin_config("phototrap").expect("a plugin")["count"].clone()
}

#[test]
fn two_phones_keep_separate_phototrap_settings() {
    let mut s = state_with("phototrap-two", json!({"selected_device": "A", "devices": {
        "A": {"plugin_configs": {"phototrap": {"count": 5}}},
        "B": {"plugin_configs": {"phototrap": {"count": 9}}},
    }}));
    s.register(Box::new(PhototrapPlugin::new())).unwrap();
    s.apply_saved_config();
    assert_eq!(value(&mut s), 5);
    s.switch_device(Some("A"), Some("B"));
    assert_eq!(value(&mut s), 9);
    s.switch_device(Some("B"), Some("A"));
    assert_eq!(value(&mut s), 5);
    assert!(s.save_now());
    let saved = s.file.load();
    assert!(saved["plugin_configs"].get("phototrap").is_none(), "nothing left in the shared section");
    assert_eq!(saved["devices"]["A"]["plugin_configs"]["phototrap"]["count"], 5);
    assert_eq!(saved["devices"]["B"]["plugin_configs"]["phototrap"]["count"], 9);
}

#[test]
fn an_old_shared_config_reaches_the_picked_and_every_paired_phone() {
    let mut s = state_with("phototrap-old", json!({"selected_device": "A",
        "plugin_configs": {"phototrap": {"count": 5}, "connection": {"phones": [{"id": "A"}, {"id": "B"}]}},
        "devices": {"A": {"plugin_configs": {"transforms": {"zoom": 2}}}},
    }));
    s.register(Box::new(PhototrapPlugin::new())).unwrap();
    s.apply_saved_config();
    assert_eq!(value(&mut s), 5);
    s.switch_device(Some("A"), Some("B"));
    assert_eq!(value(&mut s), 5, "a paired phone with no settings of its own");
    let saved = s.file.load();
    assert!(saved["plugin_configs"].get("phototrap").is_none());
}
