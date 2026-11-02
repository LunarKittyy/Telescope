//! Presets through the real shell state with the real plugins it saves and applies, not a host that stands in for them:
//! what a preset holds, what applying one leaves in each plugin, what ends up in the settings file and which phone owns it.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use telescope_core::config::ConfigFile;
use telescope_core::host::Host;
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::testing::scratch_dir;
use telescope_core::ui::UiEvent;
use telescope_effects::name_tag::NameTagPlugin;
use telescope_effects::transforms::TransformsPlugin;
use telescope_outputs::stream_output::StreamOutputPlugin;
use telescope_shell::plugins::camera_control::CameraControl;
use telescope_shell::plugins::presets::PresetsPlugin;
use telescope_shell::shell::{Effect, State};

/// A plugin that is in presets and refuses what it's given, or breaks while taking it.
struct Picky {
    name: &'static str,
    panics: bool,
}

impl Plugin for Picky {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: self.name.into(), in_presets: true, ..Default::default() }
    }
    fn get_config(&self) -> Value {
        json!({"x": 1})
    }
    fn apply_preset(&mut self, _host: &mut dyn Host, _cfg: &Value) -> Result<(), ConfigRejected> {
        if self.panics {
            panic!("broken");
        }
        Err(ConfigRejected("no".into()))
    }
}

/// A plugin in presets after the ones that fail, keeping what it's given.
struct Taker(Arc<Mutex<Vec<Value>>>);

impl Plugin for Taker {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "taker".into(), in_presets: true, ..Default::default() }
    }
    fn get_config(&self) -> Value {
        json!({"y": 2})
    }
    fn apply_preset(&mut self, _host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.0.lock().unwrap().push(cfg.clone());
        Ok(())
    }
}

fn state_on(path: &std::path::Path, extra: Vec<Box<dyn Plugin>>) -> State {
    let mut s = State::new(ConfigFile::at(path));
    // The built-ins that go in presets, in the Python app's registration order
    let mut plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(StreamOutputPlugin::new(true)),
        Box::new(TransformsPlugin::new()),
        Box::new(NameTagPlugin::new()),
        Box::new(PresetsPlugin::new()),
    ];
    plugins.extend(extra);
    for p in telescope_shell::plugins::in_order(plugins) {
        s.register(p).unwrap();
    }
    s.apply_saved_config();
    s
}

fn fresh(extra: Vec<Box<dyn Plugin>>) -> (State, std::path::PathBuf) {
    let path = scratch_dir("presets-state").join("telescope_config.json");
    (state_on(&path, extra), path)
}

fn ui(s: &mut State, callback: &str, args: Vec<Value>) {
    s.ui_event("presets", UiEvent { ui: "header".into(), callback: callback.into(), args });
}

fn save_as(s: &mut State, name: &str) {
    ui(s, "save-clicked", vec![]);
    ui(s, "name-accepted", vec![json!(name)]);
}

fn names(s: &State) -> Vec<String> {
    s.plugin_config("presets").unwrap()["presets"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect()
}

/// A phone's settings as they sit in the file.
fn device(path: &std::path::Path, id: &str) -> Value {
    ConfigFile::at(path).load()["devices"][id]["plugin_configs"].clone()
}

fn set(s: &mut State, plugin: &str, cfg: Value) {
    let i = s.index(plugin).unwrap();
    s.call(i, |p, _| p.set_config(&cfg).unwrap());
}

#[test]
fn a_preset_saves_what_the_plugins_hold_and_brings_it_back() {
    let (mut s, _) = fresh(vec![]);
    set(&mut s, "transforms", json!({"zoom": 2.0, "flip_h": true}));
    save_as(&mut s, "Wide");
    let saved = s.plugin_config("presets").unwrap()["presets"][0].clone();
    assert_eq!(saved["name"], "Wide");
    // Every plugin that is in presets has its section, camera's under "camera" and stream output's under "stream"
    assert_eq!(saved["transforms"], s.plugin_config("transforms").unwrap());
    assert_eq!(saved["stream"], s.plugin_config("stream_output").unwrap());
    assert_eq!(saved["name_tag"], s.plugin_config("name_tag").unwrap());
    assert!(saved.get("stream_output").is_none() && saved.get("presets").is_none());

    set(&mut s, "transforms", json!({}));
    assert_eq!(s.plugin_config("transforms").unwrap()["zoom"], 1.0);
    ui(&mut s, "apply", vec![json!("Wide")]);
    let t = s.plugin_config("transforms").unwrap();
    assert_eq!((t["zoom"].as_f64(), t["flip_h"].as_bool()), (Some(2.0), Some(true)));
    // The window's own view followed, as a preset applied while the preview is up does
    assert_eq!((s.view.zoom, s.view.flip_h), (2.0, true));
    assert!(s.effects.contains(&Effect::SaveSoon));
}

#[test]
fn presets_are_the_phones_own_and_come_back_from_the_settings_file() {
    let (mut s, path) = fresh(vec![]);
    s.selected = Some("phone-a".into());
    set(&mut s, "transforms", json!({"zoom": 1.5}));
    save_as(&mut s, "Close");
    assert!(s.save_now());

    // In the file: under the phone, not with the settings every phone shares
    let cfg = ConfigFile::at(&path).load();
    assert!(cfg["plugin_configs"].get("presets").is_none(), "{cfg}");
    let saved = device(&path, "phone-a");
    assert_eq!(saved["presets"]["presets"][0]["name"], "Close");
    assert_eq!(saved["presets"]["presets"][0]["transforms"]["zoom"], 1.5);

    // The next start finds them again, and applying one works as before
    let mut t = state_on(&path, vec![]);
    assert_eq!(names(&t), ["Close"]);
    set(&mut t, "transforms", json!({}));
    ui(&mut t, "apply", vec![json!("Close")]);
    assert_eq!(t.plugin_config("transforms").unwrap()["zoom"], 1.5);

    // Another phone starts with none of them, and this one has them again when it's picked
    t.switch_device(Some("phone-a"), Some("phone-b"));
    assert_eq!(names(&t), Vec::<String>::new());
    save_as(&mut t, "Other");
    t.switch_device(Some("phone-b"), Some("phone-a"));
    assert_eq!(names(&t), ["Close"]);
    assert_eq!(device(&path, "phone-b")["presets"]["presets"][0]["name"], "Other");
}

#[test]
fn the_header_hears_the_presets_after_a_phone_is_picked() {
    let (mut s, path) = fresh(vec![]);
    s.selected = Some("phone-a".into());
    save_as(&mut s, "Close");
    s.save_now();
    let mut t = state_on(&path, vec![]);
    t.effects.clear();
    t.switch_device(Some("phone-b"), Some("phone-a"));
    t.deliver_events();
    let names_shown = t.effects.iter().rev().find_map(|e| match e {
        Effect::Props { plugin, ui, props } if plugin == "presets" && ui == "header" && props.get("names").is_some() => Some(props["names"].clone()),
        _ => None,
    });
    assert_eq!(names_shown, Some(json!(["Close"])));
    assert!(t.effects.contains(&Effect::RefreshActions("presets".into())));
}

// test_presets.py::test_a_section_that_fails_to_apply_leaves_the_rest_applied
#[test]
fn a_section_that_fails_to_apply_leaves_the_rest_applied() {
    // One plugin that says no and one that panics, then one that takes its section, in presets like any other
    let taken: Arc<Mutex<Vec<Value>>> = Arc::default();
    let (mut s, _) = fresh(vec![Box::new(Picky { name: "refuses", panics: false }), Box::new(Picky { name: "breaks", panics: true }), Box::new(Taker(taken.clone()))]);
    set(&mut s, "transforms", json!({"zoom": 2.0}));
    save_as(&mut s, "A");
    assert_eq!(s.plugin_config("presets").unwrap()["presets"][0]["refuses"], json!({"x": 1}));
    set(&mut s, "transforms", json!({}));
    set(&mut s, "name_tag", json!({"name": "Sam", "on": true}));
    s.effects.clear();
    ui(&mut s, "apply", vec![json!("A")]);
    assert_eq!(s.plugin_config("transforms").unwrap()["zoom"], 2.0);
    // name_tag registers after transforms and before the two that fail: it took its section too
    assert_ne!(s.plugin_config("name_tag").unwrap()["name"], "Sam");
    assert_eq!(*taken.lock().unwrap(), [json!({"y": 2})], "the one after the two that failed took its section");
    assert_eq!(s.effects.iter().filter(|e| **e == Effect::SaveSoon).count(), 1);
    assert!(s.slots.iter().all(|slot| slot.plugin.is_some()), "a plugin that broke is back in its place");
}

// test_presets.py::test_stream_output_preset_sends_fps_quality_and_resolution
#[test]
fn a_stream_output_preset_sends_fps_quality_and_resolution_to_the_phone() {
    let (mut s, _) = fresh(vec![]);
    let i = s.index("stream_output").unwrap();
    s.call(i, |p, host| p.on_stream_start(host, &StreamInfo { source_id: "phone".into(), url: "https://phone/v1/video".into(), session: 0 }));
    let sizes = json!([{"width": 1920, "height": 1080}, {"width": 1280, "height": 720}]);
    let cams = json!([{"id": "0", "current": true, "maxFps": 60, "supportedSizes": sizes, "h264Sizes": sizes}]);
    s.call(i, |p, host| p.on_phone_state(host, &json!({"codecs": ["h264", "mjpeg"], "codec": "h264", "cameras": cams,
                                                        "stream_width": 1920, "stream_height": 1080})));
    s.effects.clear();

    let preset = json!({"name": "Small", "stream": {"fps": 24, "jpeg_quality": 60, "format": "h264", "resolution": "1280 x 720"}});
    set(&mut s, "presets", json!({"presets": [preset]}));
    ui(&mut s, "apply", vec![json!("Small")]);

    let sent: Vec<Value> = s.effects.iter().filter_map(|e| match e { Effect::PhoneControl { body, .. } => Some(body.clone()), _ => None }).collect();
    assert!(sent.contains(&json!({"action": "fps_target", "value": 24})), "{sent:?}");
    assert!(sent.contains(&json!({"action": "jpeg_quality", "value": 60})), "{sent:?}");
    let last = sent.last().unwrap();
    assert_eq!((last["action"].as_str(), last["width"].as_u64(), last["height"].as_u64()), (Some("resolution"), Some(1280), Some(720)));
    assert!(s.effects.iter().any(|e| matches!(e, Effect::UpdateOutput { change, .. } if change.fps == Some(Some(24)))));
}

// test_presets.py::test_full_round_trip_follows_the_lens_switch
#[test]
fn full_round_trip_follows_the_lens_switch() {
    let (mut s, _) = fresh(vec![Box::new(CameraControl::new())]);
    let stream = StreamInfo { source_id: "phone".into(), url: "https://phone/v1/video".into(), session: 0 };
    let sizes = json!([{"width": 1920, "height": 1080}, {"width": 1280, "height": 720}]);
    // Only the wide lens has this size, so stream output can only offer it once it follows the switch
    let wide = json!([{"width": 1920, "height": 1080}, {"width": 640, "height": 480}]);
    let cams = json!([{"id": "0", "label": "Back camera", "current": true, "maxFps": 60, "supportedSizes": sizes, "h264Sizes": sizes},
                      {"id": "2", "label": "Back camera (wide)", "current": false, "supportedSizes": wide, "h264Sizes": wide}]);
    let state = json!({"codecs": ["h264", "mjpeg"], "codec": "h264", "cameras": cams, "auto": true, "stream_width": 1920, "stream_height": 1080});
    for name in ["camera_control", "stream_output"] {
        let i = s.index(name).unwrap();
        s.call(i, |p, host| p.on_stream_start(host, &stream));
        s.call(i, |p, host| p.on_phone_state(host, &state));
    }

    set(&mut s, "transforms", json!({"zoom": 2.0, "flip_h": true}));
    save_as(&mut s, "Wide");
    set(&mut s, "transforms", json!({}));
    let mut presets = s.plugin_config("presets").unwrap();
    presets["presets"][0]["camera"]["lens"] = json!("2");
    presets["presets"][0]["stream"]["resolution"] = json!("640 x 480");
    set(&mut s, "presets", presets);
    s.effects.clear();
    ui(&mut s, "apply", vec![json!("Wide")]);

    let sent: Vec<Value> = s.effects.iter().filter_map(|e| match e { Effect::PhoneControl { body, .. } => Some(body.clone()), _ => None }).collect();
    assert_eq!(sent[0], json!({"action": "camera", "id": "2"}), "{sent:?}");
    assert_eq!(s.plugin_config("camera_control").unwrap()["lens"], "2");
    let t = s.plugin_config("transforms").unwrap();
    assert_eq!((t["zoom"].as_f64(), t["flip_h"].as_bool()), (Some(2.0), Some(true)));
    assert_eq!(s.plugin_config("stream_output").unwrap()["resolution"], "640 x 480");
}

#[test]
fn a_preset_made_before_a_plugin_existed_leaves_that_plugin_alone() {
    let (mut s, _) = fresh(vec![]);
    set(&mut s, "presets", json!({"presets": [{"name": "Old", "transforms": {"zoom": 1.2}}]}));
    set(&mut s, "name_tag", json!({"name": "Sam", "on": true}));
    ui(&mut s, "apply", vec![json!("Old")]);
    assert_eq!(s.plugin_config("transforms").unwrap()["zoom"], 1.2);
    assert_eq!(s.plugin_config("name_tag").unwrap()["name"], "Sam");
}
