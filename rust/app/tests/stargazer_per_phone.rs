//! Stargazer keeps its settings per phone, and a config from before that picks its old global copy up.

use serde_json::{json, Value};
use telescope_core::config::ConfigFile;
use telescope_core::testing::scratch_dir;
use telescope_effects::stargazer::StargazerPlugin;
use telescope_shell::shell::State;

fn state_with(name: &str, config: Value) -> State {
    let file = ConfigFile::at(scratch_dir(name).join("telescope_config.json"));
    let mut config = config;
    file.save(&mut config);
    State::new(ConfigFile::at(file.path()))
}

fn frames(s: &mut State) -> Value {
    s.plugin_config("stargazer").expect("a plugin")["frames"].clone()
}

#[test]
fn two_phones_keep_separate_stargazer_settings() {
    let mut s = state_with("stargazer-two", json!({"selected_device": "A", "devices": {
        "A": {"plugin_configs": {"stargazer": {"frames": 400}}},
        "B": {"plugin_configs": {"stargazer": {"frames": 700}}},
    }}));
    s.register(Box::new(StargazerPlugin::new())).unwrap();
    s.apply_saved_config();
    assert_eq!(frames(&mut s), 400);
    s.switch_device(Some("A"), Some("B"));
    assert_eq!(frames(&mut s), 700);
    s.switch_device(Some("B"), Some("A"));
    assert_eq!(frames(&mut s), 400);
    assert!(s.save_now());
    let saved = s.file.load();
    assert!(saved["plugin_configs"].get("stargazer").is_none(), "nothing left in the shared section");
    assert_eq!(saved["devices"]["A"]["plugin_configs"]["stargazer"]["frames"], 400);
    assert_eq!(saved["devices"]["B"]["plugin_configs"]["stargazer"]["frames"], 700);
}

#[test]
fn an_old_shared_config_is_picked_up_by_a_phone_with_no_settings_of_its_own() {
    let mut s = state_with("stargazer-old", json!({"selected_device": "A",
        "plugin_configs": {"stargazer": {"frames": 500, "mode": "trails"}},
        "devices": {"A": {"plugin_configs": {"transforms": {"zoom": 2}}}},
    }));
    s.register(Box::new(StargazerPlugin::new())).unwrap();
    s.apply_saved_config();
    let cfg = s.plugin_config("stargazer").unwrap();
    assert_eq!((cfg["frames"].clone(), cfg["mode"].clone()), (json!(500), json!("trails")));
    let saved = s.file.load();
    assert_eq!(saved["devices"]["A"]["plugin_configs"]["stargazer"]["frames"], 500);
    assert!(saved["plugin_configs"].get("stargazer").is_none());
}

#[test]
fn a_paired_phone_that_is_not_picked_gets_the_old_shared_config_too() {
    let mut s = state_with("stargazer-paired", json!({"selected_device": "A",
        "plugin_configs": {"stargazer": {"frames": 500}, "connection": {"phones": [{"id": "A"}, {"id": "B"}]}},
    }));
    s.register(Box::new(StargazerPlugin::new())).unwrap();
    s.apply_saved_config();
    s.switch_device(Some("A"), Some("B"));
    assert_eq!(frames(&mut s), 500);
}

#[test]
fn an_old_night_mode_on_a_phone_that_is_not_picked_reads_as_it_does_on_the_picked_one() {
    let mut s = state_with("stargazer-night", json!({"selected_device": "A",
        "plugin_configs": {"stargazer": {"mode": "night", "frames": 500}, "connection": {"phones": [{"id": "A"}, {"id": "B"}]}},
    }));
    s.register(Box::new(StargazerPlugin::new())).unwrap();
    s.apply_saved_config();
    let first = s.plugin_config("stargazer").unwrap();
    assert_eq!((first["mode"].clone(), first["input"].clone(), first["photo_count"].clone()), (json!("live"), json!("photos"), json!(1)));
    s.switch_device(Some("A"), Some("B"));
    assert_eq!(s.plugin_config("stargazer").unwrap(), first);
}
