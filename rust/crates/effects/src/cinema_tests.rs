//! The Cinema tools plugin: tests/test_cinema.py's card, settings, level guide and sensor tests.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::View;
use telescope_core::preview::PreviewPass;
use telescope_core::testing::FakeHost;

struct Rig {
    plugin: CinemaPlugin,
    host: FakeHost,
    now: Arc<AtomicU64>,
}

impl Rig {
    fn new() -> Rig {
        let now = Arc::new(AtomicU64::new(100f64.to_bits()));
        let read = now.clone();
        let mut rig = Rig { plugin: CinemaPlugin::with_clock(Arc::new(move || f64::from_bits(read.load(Ordering::SeqCst)))), host: FakeHost::new(), now };
        rig.host.focused = Some("phone".into());
        rig.plugin.setup(&mut rig.host);
        // the card is dimmed until a phone is paired
        rig.event(Event::PhonesChanged { count: 1 });
        rig.config(json!({}));
        rig
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
    }

    /// set_config, then the first hook that has a host.
    fn config(&mut self, cfg: Value) {
        self.plugin.set_config(&cfg).unwrap();
        self.plugin.on_plugins_ready(&mut self.host);
    }

    fn ui(&mut self, ui: &str, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: ui.into(), callback: callback.into(), args });
    }

    fn toggle(&mut self, which: usize, on: bool) {
        self.ui("card", "tool-toggled", vec![json!(which), json!(on)]);
    }

    /// The newest value a piece of UI was given for a property.
    fn shown(&self, ui: &str, key: &str) -> Value {
        let prefix = format!("set_ui_props {ui} ");
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix(prefix.as_str())).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    /// The kinds of sensors asked for last, as the host heard them.
    fn wanted(&self) -> String {
        self.host.calls.iter().rev().find_map(|c| c.strip_prefix("want_sensors cinema ")).unwrap_or("[]").to_string()
    }

    fn refreshes(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "refresh_preview").count()
    }

    fn on_preview(&self) -> bool {
        self.host.passes.contains_key(NAME)
    }

    fn reading(&self, reading: Option<Value>) {
        *self.host.sensor_reading.lock().unwrap() = reading;
    }

    fn set_now(&self, seconds: f64) {
        self.now.store(seconds.to_bits(), Ordering::SeqCst);
    }
}

#[test]
fn the_layer_is_on_the_preview_only_while_a_tool_is() {
    let mut r = Rig::new();
    assert!(!r.on_preview());
    r.toggle(1, true);
    assert!(r.on_preview() && r.plugin.layer().look().zebras);
    r.toggle(1, false);
    assert!(!r.on_preview());
}

#[test]
fn settings_round_trip_and_bad_values_fall_back() {
    let mut r = Rig::new();
    r.config(json!({"peaking": true, "scope": "vectorscope", "peaking_colour": "blue", "peaking_level": 9, "zebra_level": 80, "scope_size": "large"}));
    assert_eq!(
        r.plugin.get_config(),
        json!({"peaking": true, "zebras": false, "false_colour": false, "level": false, "scope": "vectorscope", "guides": "off",
               "peaking_colour": "blue", "peaking_level": 9, "zebra_level": 80, "scope_size": "large"})
    );
    // the card and the settings window show them
    assert_eq!(r.shown("card", "scope-current"), json!(4));
    assert_eq!(r.shown(NAME, "colour-current"), json!(2));
    r.config(json!({"scope": "oscilloscope", "peaking_colour": "plaid", "zebra_level": 3, "peaking_level": "x"}));
    let cfg = r.plugin.get_config();
    assert_eq!((&cfg["scope"], &cfg["peaking_colour"], &cfg["zebra_level"], &cfg["peaking_level"]), (&json!("off"), &json!("red"), &json!(70), &json!(5)));
    r.config(json!({"guides": "golden"}));
    assert_eq!(r.plugin.get_config()["guides"], json!("golden"));
    assert_eq!(r.shown("card", "guide-current"), json!(4));
    r.config(json!({"guides": "spiral"}));
    assert_eq!(r.plugin.get_config()["guides"], json!("off"));
}

#[test]
fn settings_are_saved_in_the_order_python_saves_them() {
    let r = Rig::new();
    let keys: Vec<String> = r.plugin.get_config().as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["peaking", "zebras", "false_colour", "level", "scope", "guides", "peaking_colour", "peaking_level", "zebra_level", "scope_size"]);
}

#[test]
fn a_name_that_is_a_list_is_refused_as_python_refuses_it() {
    let mut p = CinemaPlugin::new();
    for key in ["scope", "guides", "peaking_colour", "scope_size"] {
        assert!(p.set_config(&json!({ key: ["histogram"] })).is_err(), "{key}");
        assert!(p.set_config(&json!({ key: {"a": 1} })).is_err(), "{key}");
        // a number or nothing is just not a name
        assert!(p.set_config(&json!({ key: 3 })).is_ok(), "{key}");
        assert!(p.set_config(&json!({ key: null })).is_ok(), "{key}");
    }
}

#[test]
fn the_settings_window_changes_what_the_layer_draws() {
    let mut r = Rig::new();
    r.toggle(0, true);
    r.ui(NAME, "colour-picked", vec![json!(3)]);
    r.ui(NAME, "zebra-moved", vec![json!(85.0)]);
    r.ui(NAME, "size-picked", vec![json!(0)]);
    let look = r.plugin.layer().look();
    assert_eq!((look.peaking_colour, look.zebra_level, look.scope_size), ("white", 85, "small"));
    assert_eq!(r.shown(NAME, "zebra-readout"), json!("85%"));
    r.ui(NAME, "peaking-moved", vec![json!(9.4)]);
    assert_eq!(r.shown(NAME, "peaking-readout"), json!("9"));
    assert!(r.host.calls.iter().any(|c| c == "schedule_save"));
}

#[test]
fn diagnostics_say_what_is_on() {
    let mut r = Rig::new();
    assert_eq!(r.plugin.diagnostics(), vec![("Cinema tools".to_string(), "nothing on".to_string())]);
    r.config(json!({"zebras": true, "scope": "parade", "guides": "thirds"}));
    assert_eq!(r.plugin.diagnostics(), vec![("Cinema tools".to_string(), "zebras, parade, thirds guides".to_string())]);
}

// ── Framing guides ───────────────────────────────────────────────────────────

#[test]
fn guides_alone_put_the_layer_on_and_leave_the_picture_alone() {
    let mut r = Rig::new();
    r.ui("card", "guide-picked", vec![json!(1)]);
    assert!(r.on_preview() && r.plugin.layer().look().guides == "thirds");
    let layer = r.plugin.layer();
    assert!(!layer.tints());
    let mut frame = Frame::new(8, 4, PixelFormat::Rgba, (0..8 * 4 * 4).map(|i| (i * 7) as u8).collect());
    let before = frame.data.clone();
    layer.tint(&mut frame);
    assert_eq!(frame.data, before);
    r.ui("card", "guide-picked", vec![json!(0)]);
    assert!(!r.on_preview());
}

// ── Level guide ──────────────────────────────────────────────────────────────

#[test]
fn the_level_switch_asks_for_the_sensors_only_while_on() {
    let mut r = Rig::new();
    assert_eq!(r.wanted(), "[]");
    r.toggle(3, true);
    assert_eq!(r.wanted(), "[\"orientation\"]");
    assert!(r.on_preview());
    r.toggle(3, false);
    assert_eq!(r.wanted(), "[]");
    assert!(!r.on_preview());
    r.config(json!({"level": true}));
    assert_eq!(r.wanted(), "[\"orientation\"]");
    assert_eq!(r.plugin.get_config()["level"], json!(true));
    r.plugin.shutdown(&mut r.host);
    assert_eq!(r.wanted(), "[]");
}

#[test]
fn a_frozen_horizon_isnt_drawn() {
    let mut r = Rig::new();
    r.toggle(3, true);
    r.reading(Some(json!({"at": 10.0, "roll": 3.0, "pitch": 0.0, "orientation_at": 10.0})));
    r.event(Event::SensorsChanged { source_id: "phone".into() });
    let layer = r.plugin.layer();
    r.now.store(10.5f64.to_bits(), Ordering::SeqCst);
    assert!(layer.paint_canvas(64, 36, 1.0).is_some());
    // at 12 s the newest roll is 2 s old
    r.now.store(12.0f64.to_bits(), Ordering::SeqCst);
    assert!(layer.paint_canvas(64, 36, 1.0).is_none());
}

#[test]
fn readings_reach_the_layer_as_shown() {
    let mut r = Rig::new();
    r.toggle(3, true);
    r.host.view.flip_h = true;
    r.reading(Some(json!({"at": 1.0, "roll": 6.0, "pitch": -3.0, "camera": "0", "orientation_at": 1.0})));
    r.event(Event::SensorsChanged { source_id: "phone".into() });
    let (level, at) = r.plugin.layer().level().expect("a level");
    assert_eq!((level.roll, level.pitch, level.roll_level, at), (Some(-6.0), -3.0, false, 1.0));
    let refreshes = r.refreshes();
    r.reading(Some(json!({"at": 1.01, "roll": 6.0, "pitch": -3.0, "camera": "0", "orientation_at": 1.01})));
    r.event(Event::SensorsChanged { source_id: "phone".into() });
    // nothing new to draw
    assert_eq!(r.refreshes(), refreshes);
    r.reading(Some(json!({"at": 5.0, "roll": 6.0, "pitch": -3.0, "camera": "0", "light": 20.0, "orientation_at": 1.01})));
    r.event(Event::SensorsChanged { source_id: "phone".into() });
    // light alone doesn't keep a frozen horizon fresh
    assert_eq!(r.plugin.layer().level().unwrap().1, 1.01);
    // not the shown stream
    r.event(Event::SensorsChanged { source_id: "another phone".into() });
    r.reading(None);
    r.event(Event::SensorsChanged { source_id: "phone".into() });
    assert!(r.plugin.layer().level().is_none());
}

#[test]
fn why_the_level_cant_show_is_on_the_card() {
    let mut r = Rig::new();
    r.toggle(3, true);
    r.host.sensor_status = Some(json!({"state": "unavailable", "reason": "old_phone"}));
    r.event(Event::SensorsChanged { source_id: "phone".into() });
    assert_eq!(r.shown("card", "level-note-shown"), json!(true));
    assert!(r.shown("card", "level-note").as_str().unwrap().contains("Update Telescope"));
    r.host.sensor_status = Some(json!({"state": "on", "kinds": ["orientation"], "streaming": ["orientation"]}));
    r.event(Event::SensorsChanged { source_id: "phone".into() });
    assert_eq!(r.shown("card", "level-note-shown"), json!(false));
    r.host.sensor_status = Some(json!({"state": "unavailable", "reason": "not_phone"}));
    r.event(Event::SensorsChanged { source_id: "phone".into() });
    assert_eq!(r.shown("card", "level-note-shown"), json!(true));
    r.toggle(3, false);
    assert_eq!(r.shown("card", "level-note-shown"), json!(false));
}

#[test]
fn every_reason_the_level_cant_show_has_its_words() {
    for (reason, _) in LEVEL_PROBLEMS {
        let mut r = Rig::new();
        r.toggle(3, true);
        r.host.sensor_status = Some(json!({"state": "unavailable", "reason": reason}));
        r.event(Event::SensorsChanged { source_id: "phone".into() });
        assert_eq!(r.shown("card", "level-note-shown"), json!(true), "{reason}");
    }
    let mut r = Rig::new();
    r.toggle(3, true);
    r.host.sensor_status = Some(json!({"state": "unavailable", "reason": "something new"}));
    r.event(Event::SensorsChanged { source_id: "phone".into() });
    assert_eq!(r.shown("card", "level-note-shown"), json!(false));
}

#[test]
fn the_sensors_stop_while_nothing_draws_the_guide() {
    let mut r = Rig::new();
    r.toggle(3, true);
    assert_eq!(r.wanted(), "[\"orientation\"]");
    assert!(r.host.scheduled.iter().any(|(_, t)| t == T_SEEN));
    // the preview hid, or the window went to the tray: no paints
    r.set_now(103.0);
    r.event(Event::TimerFired { owner: NAME.into(), token: T_SEEN.into() });
    assert_eq!(r.wanted(), "[]");
    // another tool changing doesn't ask the phone while the guide is unseen
    r.toggle(1, true);
    assert_eq!(r.wanted(), "[]");
    // drawn again
    r.plugin.layer().set_painted_at(103.0);
    r.event(Event::TimerFired { owner: NAME.into(), token: T_SEEN.into() });
    assert_eq!(r.wanted(), "[\"orientation\"]");
    r.toggle(3, false);
    assert_eq!(r.wanted(), "[]");
    assert!(!r.host.scheduled.iter().any(|(_, t)| t == T_SEEN));
}

#[test]
fn a_new_view_or_device_forgets_the_old_tilt() {
    let mut r = Rig::new();
    r.toggle(3, true);
    r.reading(Some(json!({"roll": 9.0, "pitch": 0.0, "orientation_at": 1.0})));
    r.event(Event::SensorsChanged { source_id: "phone".into() });
    r.event(Event::ViewChanged { view: View::default() });
    r.reading(Some(json!({"roll": 0.0, "pitch": 0.0, "orientation_at": 2.0})));
    r.event(Event::SensorsChanged { source_id: "phone".into() });
    // not smoothed from the old reading
    assert_eq!(r.plugin.layer().level().unwrap().0.roll, Some(0.0));
}

// ── Card and shortcuts ───────────────────────────────────────────────────────

#[test]
fn the_card_is_in_the_right_column_and_has_what_the_plugin_sets() {
    let mut p = CinemaPlugin::new();
    let card = p.card().expect("a card");
    assert_eq!((card.region.as_str(), card.ui.component.as_str()), ("right", "CinemaCard"));
    let info = p.info();
    assert_eq!((info.name.as_str(), info.category.as_str(), info.icon.as_str(), info.optional), ("cinema", "preview", "focus", true));
    let sections = p.settings_sections();
    assert_eq!((sections[0].tab.as_str(), sections[0].order), ("video", 40));
}

#[test]
fn the_actions_are_the_six_python_lists() {
    let mut p = CinemaPlugin::new();
    let ids: Vec<String> = p.actions().into_iter().map(|a| a.id).collect();
    assert_eq!(ids, ["cinema.peaking", "cinema.zebras", "cinema.false_colour", "cinema.level", "cinema.guides", "cinema.scope"]);
    let guides = p.actions().into_iter().find(|a| a.id == "cinema.guides").unwrap();
    // a saved shortcut keeps the English name
    let choices = guides.params[1]["choices"].as_array().unwrap().iter().map(|c| c[0].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(choices, ["Off", "Thirds", "Centre cross", "4 × 4", "Golden ratio"]);
}

#[test]
fn shortcuts_switch_the_tools_and_say_so() {
    let mut r = Rig::new();
    assert_eq!(r.plugin.run_action(&mut r.host, "cinema.zebras", &json!({"mode": "on"})).as_deref(), Some("Zebras: on"));
    assert!(r.plugin.layer().look().zebras);
    assert_eq!(r.plugin.run_action(&mut r.host, "cinema.zebras", &json!({})).as_deref(), Some("Zebras: off"));
    assert_eq!(r.plugin.run_action(&mut r.host, "cinema.guides", &json!({"op": "next"})).as_deref(), Some("Guides: Thirds"));
    assert_eq!(r.plugin.layer().look().guides, "thirds");
    assert_eq!(r.plugin.run_action(&mut r.host, "cinema.guides", &json!({"op": "set", "option": "4 × 4"})).as_deref(), Some("Guides: 4 × 4"));
    assert_eq!(r.plugin.layer().look().guides, "4x4");
    assert_eq!(r.plugin.run_action(&mut r.host, "cinema.scope", &json!({"op": "previous"})).as_deref(), Some("Scope: Vectorscope"));
    assert_eq!(r.plugin.run_action(&mut r.host, "cinema.scope", &json!({"op": "previous"})).as_deref(), Some("Scope: RGB parade"));
    assert_eq!(r.plugin.layer().look().scope, "parade");
    // held: back where it was on release
    let hold = json!({"mode": "hold"});
    r.plugin.run_action(&mut r.host, "cinema.peaking", &hold);
    assert!(r.plugin.layer().look().peaking);
    r.plugin.release_action(&mut r.host, "cinema.peaking", &hold);
    assert!(!r.plugin.layer().look().peaking);
    assert_eq!(r.plugin.run_action(&mut r.host, "cinema.nothing", &json!({})), None);
}

#[test]
fn shortcuts_wait_for_a_phone_like_the_card() {
    let mut p = CinemaPlugin::new();
    let mut host = FakeHost::new();
    p.setup(&mut host);
    assert_eq!(p.run_action(&mut host, "cinema.zebras", &json!({"mode": "on"})), None);
    assert!(!p.layer().look().zebras);
}
