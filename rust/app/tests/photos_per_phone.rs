//! Photos keeps its settings per phone, and a config from before that picks its old global copy up.

use serde_json::{json, Value};
use telescope_core::config::ConfigFile;
use telescope_core::testing::scratch_dir;
use telescope_effects::photos::PhotosPlugin;
use telescope_shell::shell::State;

fn state_with(name: &str, config: Value) -> State {
    let file = ConfigFile::at(scratch_dir(name).join("telescope_config.json"));
    let mut config = config;
    file.save(&mut config);
    State::new(ConfigFile::at(file.path()))
}

fn value(s: &mut State) -> Value {
    s.plugin_config("photos").expect("a plugin")["timer"].clone()
}

#[test]
fn two_phones_keep_separate_photos_settings() {
    let mut s = state_with("photos-two", json!({"selected_device": "A", "devices": {
        "A": {"plugin_configs": {"photos": {"timer": 10}}},
        "B": {"plugin_configs": {"photos": {"timer": 3}}},
    }}));
    s.register(Box::new(PhotosPlugin::new())).unwrap();
    s.apply_saved_config();
    assert_eq!(value(&mut s), 10);
    s.switch_device(Some("A"), Some("B"));
    assert_eq!(value(&mut s), 3);
    s.switch_device(Some("B"), Some("A"));
    assert_eq!(value(&mut s), 10);
    assert!(s.save_now());
    let saved = s.file.load();
    assert!(saved["plugin_configs"].get("photos").is_none(), "nothing left in the shared section");
    assert_eq!(saved["devices"]["A"]["plugin_configs"]["photos"]["timer"], 10);
    assert_eq!(saved["devices"]["B"]["plugin_configs"]["photos"]["timer"], 3);
}

#[test]
fn an_old_shared_config_reaches_the_picked_and_every_paired_phone() {
    let mut s = state_with("photos-old", json!({"selected_device": "A",
        "plugin_configs": {"photos": {"timer": 10}, "connection": {"phones": [{"id": "A"}, {"id": "B"}]}},
        "devices": {"A": {"plugin_configs": {"transforms": {"zoom": 2}}}},
    }));
    s.register(Box::new(PhotosPlugin::new())).unwrap();
    s.apply_saved_config();
    assert_eq!(value(&mut s), 10);
    s.switch_device(Some("A"), Some("B"));
    assert_eq!(value(&mut s), 10, "a paired phone with no settings of its own");
    let saved = s.file.load();
    assert!(saved["plugin_configs"].get("photos").is_none());
}
