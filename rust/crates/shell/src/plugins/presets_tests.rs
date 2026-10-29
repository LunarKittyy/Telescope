use super::*;
use telescope_core::testing::FakeHost;

struct Rig {
    plugin: PresetsPlugin,
    host: FakeHost,
}

impl Rig {
    /// The plugin with these plugins in presets (name, settings), in registration order.
    fn new(plugins: &[(&str, Value)]) -> Rig {
        let mut host = FakeHost::new();
        for (name, cfg) in plugins {
            host.preset_plugins.push(name.to_string());
            host.plugin_configs.insert(name.to_string(), cfg.clone());
        }
        Rig { plugin: PresetsPlugin::new(), host }
    }

    fn set(&mut self, plugin: &str, cfg: Value) {
        self.host.plugin_configs.insert(plugin.to_string(), cfg);
    }

    fn config_of(&self, plugin: &str) -> Value {
        self.host.plugin_configs[plugin].clone()
    }

    fn save(&mut self, name: &str) {
        self.plugin.save(&mut self.host, name);
    }

    fn load(&mut self, presets: Value) {
        self.plugin.set_config(&json!({"presets": presets})).unwrap();
    }

    fn saves(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "schedule_save").count()
    }

    /// The applies the host was asked for, as plugin names.
    fn applied(&self) -> Vec<String> {
        self.host.calls.iter().filter_map(|c| c.strip_prefix("apply_preset ")).map(|c| c.split(' ').next().unwrap().to_string()).collect()
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "header".into(), callback: callback.into(), args });
    }

    /// The newest value the header was given for a property.
    fn shown(&self, prop: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props header ")).filter_map(|p| serde_json::from_str::<Value>(p).ok())
            .find_map(|p| p.get(prop).cloned()).unwrap_or(Value::Null)
    }

    fn names(&self) -> Vec<String> {
        self.plugin.names()
    }
}

// test_presets.py::test_save_snapshots_each_section_and_replaces_by_name
#[test]
fn save_snapshots_each_section_and_replaces_by_name() {
    let mut r = Rig::new(&[("camera_control", json!({"iso": 100})), ("transforms", json!({"zoom": 2.0}))]);
    r.save("  Desk  ");
    r.set("camera_control", json!({"iso": 800}));
    r.save("Night");
    r.set("camera_control", json!({"iso": 200}));
    r.save("Desk");

    assert_eq!(r.names(), ["Desk", "Night"]);
    assert_eq!(Value::Object(r.plugin.find("Desk").unwrap().clone()), json!({"name": "Desk", "camera": {"iso": 200}, "transforms": {"zoom": 2.0}}));
    assert_eq!(r.plugin.find("Night").unwrap()["camera"], json!({"iso": 800}));
    assert_eq!(r.saves(), 3);
}

// test_presets.py::test_any_plugin_in_presets_is_saved_and_an_older_preset_leaves_it_alone
#[test]
fn any_plugin_in_presets_is_saved_and_an_older_preset_leaves_it_alone() {
    let mut r = Rig::new(&[("transforms", json!({"zoom": 2.0})), ("framing", json!({"on": true}))]);
    // A plugin that isn't in presets has settings the host knows but presets never save
    r.host.plugin_configs.insert("microphone".into(), json!({"gain": 3}));
    r.save("Talking");
    assert_eq!(r.plugin.get_config()["presets"][0], json!({"name": "Talking", "transforms": {"zoom": 2.0}, "framing": {"on": true}}));
    r.load(json!([{"name": "Old", "transforms": {"zoom": 1.0}},
                  {"name": "Talking", "framing": {"on": true}, "gone": {"x": 1}, "junk": 3}]));
    assert_eq!(Value::Object(r.plugin.find("Talking").unwrap().clone()), json!({"name": "Talking", "framing": {"on": true}, "gone": {"x": 1}}));
    r.set("framing", json!({"on": false}));
    r.plugin.apply(&mut r.host, "Old");
    assert_eq!(r.applied(), ["transforms"]);
    assert_eq!(r.config_of("framing"), json!({"on": false}));
    assert_eq!(r.config_of("transforms"), json!({"zoom": 1.0}));
}

// test_presets.py::test_blank_names_are_ignored
#[test]
fn blank_names_are_ignored() {
    let mut r = Rig::new(&[]);
    r.save("   ");
    assert!(r.names().is_empty());
    assert_eq!(r.saves(), 0);
}

// test_presets.py::test_overwriting_a_preset_keeps_the_settings_of_plugins_that_are_off
#[test]
fn overwriting_a_preset_keeps_the_settings_of_plugins_that_are_off() {
    let mut r = Rig::new(&[("transforms", json!({"zoom": 3.0}))]);
    r.load(json!([{"name": "Desk", "transforms": {"zoom": 2.0}, "background": {"mode": "picture", "picture": "desk.png"}}]));
    r.save("Desk");
    assert_eq!(Value::Object(r.plugin.find("Desk").unwrap().clone()),
               json!({"name": "Desk", "transforms": {"zoom": 3.0}, "background": {"mode": "picture", "picture": "desk.png"}}));
    // Background is back on: its saved section applies
    r.host.preset_plugins.push("background".into());
    r.host.plugin_configs.insert("background".into(), json!({}));
    r.plugin.apply(&mut r.host, "Desk");
    assert_eq!(r.config_of("background"), json!({"mode": "picture", "picture": "desk.png"}));
}

// test_presets.py::test_names_lose_control_characters_when_saved_renamed_and_loaded
#[test]
fn names_lose_control_characters_when_saved_renamed_and_loaded() {
    let mut r = Rig::new(&[]);
    r.save("Des\u{0}k\n<b>");
    assert_eq!(r.names(), ["Desk b"]);
    r.plugin.rename(&mut r.host, "Desk b", &format!("Ni\u{7}ght\t{}", "x".repeat(80)));
    let names = r.names();
    assert_eq!(names.len(), 1);
    assert!(names[0].starts_with("Night x") && names[0].chars().count() == 40 && !names[0].chars().any(char::is_control), "{names:?}");
    r.load(json!([{"name": "Lo\u{0}ad\u{1b}"}, {"name": "\u{0}\u{1}"}]));
    assert_eq!(r.names(), ["Load"]);
}

// test_presets.py::test_apply_hands_sections_to_the_host_camera_first
#[test]
fn apply_hands_sections_to_the_host_in_registration_order() {
    let mut r = Rig::new(&[("camera_control", json!({})), ("stream_output", json!({})), ("transforms", json!({}))]);
    r.load(json!([{"name": "A", "transforms": {"zoom": 3.0}, "stream": {"fps": 24}, "camera": {"iso": 400}}]));

    r.plugin.apply(&mut r.host, "A");
    r.plugin.apply(&mut r.host, "missing");

    assert_eq!(r.applied(), ["camera_control", "stream_output", "transforms"]);
    assert_eq!(r.config_of("camera_control"), json!({"iso": 400}));
    assert_eq!(r.config_of("stream_output"), json!({"fps": 24}));
    assert_eq!(r.config_of("transforms"), json!({"zoom": 3.0}));
    assert_eq!(r.saves(), 1, "only the preset that exists is saved");
}

// test_presets.py::test_rename_and_delete
#[test]
fn rename_and_delete() {
    let mut r = Rig::new(&[]);
    r.load(json!([{"name": "A"}, {"name": "B"}, {"name": "C"}]));

    r.plugin.rename(&mut r.host, "A", "Desk");
    r.plugin.rename(&mut r.host, "B", "C"); // takes over the name; the old C goes
    r.plugin.delete(&mut r.host, "Desk");
    r.plugin.delete(&mut r.host, "nope");

    assert_eq!(r.names(), ["C"]);
    assert_eq!(r.plugin.get_config(), json!({"presets": [{"name": "C"}]}));
}

// test_presets.py::test_clean_presets_drops_malformed_and_duplicate_entries
#[test]
fn clean_presets_drops_malformed_and_duplicate_entries() {
    let raw = json!([{"name": "A", "camera": {"iso": 1}, "stream": "bad"}, {"name": ""}, "junk", {"name": "A"}, {"name": "B", "extra": 1}]);
    assert_eq!(Value::Array(clean_presets(Some(&raw)).into_iter().map(Value::Object).collect()), json!([{"name": "A", "camera": {"iso": 1}}, {"name": "B"}]));
    assert!(clean_presets(None).is_empty());
    assert!(clean_presets(Some(&json!("not a list"))).is_empty());
    // A name that isn't text is read the way str() reads it
    assert_eq!(clean_presets(Some(&json!([{"name": 5}, {"x": 1}]))).len(), 1);
}

// test_presets.py::test_menu_lists_presets_then_save_rename_delete
#[test]
fn the_menu_lists_the_presets_and_delete_removes_one() {
    let mut r = Rig::new(&[]);
    let header = r.plugin.header_widget().unwrap();
    assert_eq!(header.props["names"], json!([]));
    assert_eq!(header.props["save-text"], "Save current as…");
    assert_eq!((header.props["rename-text"].as_str(), header.props["delete-text"].as_str()), (Some("Rename"), Some("Delete")));
    r.load(json!([{"name": "A"}, {"name": "B"}]));
    r.plugin.on_plugins_ready(&mut r.host);
    assert_eq!(r.shown("names"), json!(["A", "B"]));
    r.ui("delete-clicked", vec![json!("A")]);
    assert_eq!(r.names(), ["B"]);
    assert_eq!(r.shown("names"), json!(["B"]));
}

// test_presets.py::test_an_ampersand_in_a_name_shows_in_the_menu
#[test]
fn an_ampersand_in_a_name_shows_in_the_menu() {
    // Qt's menus read "&" as a shortcut marker, which Slint's don't, so the name goes in as it is
    let mut r = Rig::new(&[]);
    r.load(json!([{"name": "Desk & window"}]));
    r.plugin.on_plugins_ready(&mut r.host);
    assert_eq!(r.shown("names"), json!(["Desk & window"]));
}

// test_presets.py::test_save_suggests_a_name_no_preset_has
#[test]
fn save_suggests_a_name_no_preset_has() {
    let mut r = Rig::new(&[]);
    r.save("Preset 2");
    assert_eq!(r.plugin.unused_name(), "Preset 3");
    r.save("Preset 3");
    r.plugin.delete(&mut r.host, "Preset 2");
    assert_eq!(r.plugin.unused_name(), "Preset 2");
    r.ui("save-clicked", vec![]);
    assert_eq!((r.shown("dialog"), r.shown("dialog-text"), r.shown("dialog-title")), (json!("name"), json!("Preset 2"), json!("Save preset")));
}

// test_presets.py::test_saving_or_renaming_onto_another_preset_asks_first
#[test]
fn saving_or_renaming_onto_another_preset_asks_first() {
    let mut r = Rig::new(&[("transforms", json!({"zoom": 1.0}))]);
    r.save("Desk");
    r.set("transforms", json!({"zoom": 2.0}));
    r.save("Night");
    let saves = r.saves();

    // Saving as a name that's taken asks, and No leaves everything as it was
    r.ui("save-clicked", vec![]);
    r.ui("name-accepted", vec![json!("Night ")]);
    assert_eq!(r.shown("dialog"), "replace");
    assert_eq!(r.shown("confirm-text"), "There's already a preset called \"Night\". Replace it?");
    r.set("transforms", json!({"zoom": 3.0}));
    r.ui("replace-answered", vec![json!(false)]);
    assert_eq!(r.shown("dialog"), "");
    assert_eq!(r.plugin.find("Night").unwrap()["transforms"], json!({"zoom": 2.0}));
    assert_eq!(r.saves(), saves);

    // Yes replaces it
    r.ui("save-clicked", vec![]);
    r.ui("name-accepted", vec![json!("Night")]);
    r.ui("replace-answered", vec![json!(true)]);
    assert_eq!(r.plugin.find("Night").unwrap()["transforms"], json!({"zoom": 3.0}));

    // A name nobody has goes straight through
    r.ui("save-clicked", vec![]);
    r.ui("name-accepted", vec![json!("Sofa")]);
    assert_eq!(r.names(), ["Desk", "Night", "Sofa"]);
    assert_eq!(r.shown("dialog"), "");

    // Renaming to its own name is no clash; to another's asks, and Yes takes the name over
    r.ui("rename-clicked", vec![json!("Night")]);
    assert_eq!((r.shown("dialog-title"), r.shown("dialog-text"), r.shown("ok-text")), (json!("Rename preset"), json!("Night"), json!("Rename")));
    r.ui("name-accepted", vec![json!("Night")]);
    assert_eq!(r.shown("dialog"), "");
    r.ui("rename-clicked", vec![json!("Night")]);
    r.ui("name-accepted", vec![json!("Desk")]);
    assert_eq!(r.shown("dialog"), "replace");
    r.ui("replace-answered", vec![json!(true)]);
    assert_eq!(r.names(), ["Desk", "Sofa"]);
    assert_eq!(r.plugin.find("Desk").unwrap()["transforms"], json!({"zoom": 3.0}));
}

#[test]
fn cancelling_the_name_question_does_nothing() {
    let mut r = Rig::new(&[]);
    r.ui("save-clicked", vec![]);
    r.ui("dialog-cancelled", vec![]);
    assert_eq!(r.shown("dialog"), "");
    // An answer that arrives with no question open is ignored
    r.ui("name-accepted", vec![json!("Late")]);
    assert!(r.names().is_empty());
    // A blank name closes the question and saves nothing
    r.ui("save-clicked", vec![]);
    r.ui("name-accepted", vec![json!("   ")]);
    assert!(r.names().is_empty());
    assert_eq!(r.shown("dialog"), "");
}

#[test]
fn the_header_and_the_shortcuts_hear_the_presets_that_were_loaded() {
    let mut r = Rig::new(&[]);
    r.load(json!([{"name": "Desk"}]));
    // set_config has no host: the next hook that has one passes it on
    r.plugin.on_event(&mut r.host, &Event::DeviceChanged { source_id: "phone".into() });
    assert_eq!(r.shown("names"), json!(["Desk"]));
    assert!(r.host.calls.iter().any(|c| c == "refresh_actions"));
    let actions = r.plugin.actions();
    assert_eq!(actions[0].params[0]["choices"], json!([["Desk", "Desk"]]));
}

#[test]
fn the_config_is_what_python_saves() {
    let mut r = Rig::new(&[]);
    assert_eq!(r.plugin.get_config(), json!({"presets": []}));
    let cfg = json!({"presets": [{"name": "Desk", "camera": {"iso": 100, "lens": "0"}, "stream": {"fps": 30}}]});
    r.plugin.set_config(&cfg).unwrap();
    assert_eq!(r.plugin.get_config(), cfg);
    // Nothing to say about presets: none
    r.plugin.set_config(&json!({})).unwrap();
    assert_eq!(r.plugin.get_config(), json!({"presets": []}));
}

#[test]
fn it_lives_in_the_header_on_the_left_and_is_kept_per_phone() {
    let info = PresetsPlugin::new().info();
    assert_eq!((info.name.as_str(), info.category.as_str(), info.icon.as_str(), info.header_side.as_str()), ("presets", "extras", "preset", "left"));
    assert!(info.device_local && !info.in_presets && !info.optional && !info.required);
}

#[test]
fn the_shortcuts_apply_a_preset_or_go_round_them() {
    let mut r = Rig::new(&[("transforms", json!({"zoom": 1.0}))]);
    r.load(json!([{"name": "A", "transforms": {"zoom": 2.0}}, {"name": "B", "transforms": {"zoom": 3.0}}, {"name": "C"}]));
    let ids: Vec<String> = r.plugin.actions().into_iter().map(|a| a.id).collect();
    assert_eq!(ids, ["presets.apply", "presets.cycle"]);

    assert_eq!(r.plugin.run_action(&mut r.host, "presets.apply", &json!({"name": "gone"})), None);
    assert_eq!(r.plugin.run_action(&mut r.host, "presets.apply", &json!({"name": "B"})), Some("Preset: B".into()));
    assert_eq!(r.config_of("transforms"), json!({"zoom": 3.0}));
    // Next goes on from the one applied last, round the end, and Previous back
    assert_eq!(r.plugin.run_action(&mut r.host, "presets.cycle", &json!({"op": "next"})), Some("Preset: C".into()));
    assert_eq!(r.plugin.run_action(&mut r.host, "presets.cycle", &json!({"op": "next"})), Some("Preset: A".into()));
    assert_eq!(r.config_of("transforms"), json!({"zoom": 2.0}));
    assert_eq!(r.plugin.run_action(&mut r.host, "presets.cycle", &json!({"op": "previous"})), Some("Preset: C".into()));
    // With nothing applied yet, Next starts at the first and Previous at the last
    let mut fresh = Rig::new(&[]);
    fresh.load(json!([{"name": "A"}, {"name": "B"}]));
    assert_eq!(fresh.plugin.run_action(&mut fresh.host, "presets.cycle", &json!({"op": "previous"})), Some("Preset: B".into()));
    let mut fresh = Rig::new(&[]);
    fresh.load(json!([{"name": "A"}, {"name": "B"}]));
    assert_eq!(fresh.plugin.run_action(&mut fresh.host, "presets.cycle", &json!({"op": "next"})), Some("Preset: A".into()));
    assert_eq!(Rig::new(&[]).plugin.run_action(&mut FakeHost::new(), "presets.cycle", &json!({"op": "next"})), None);
}
