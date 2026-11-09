//! tests/test_shortcuts.py on real controls: Python builds a widget and an action for it, a Rust action belongs to its plugin, so
//! these run the Camera card's own actions and read the card and what went to the phone. What a binding says of itself is
//! `shortcuts.rs` in the system crate.

use super::*;
use telescope_core::testing::FakeHost;

struct Rig {
    cam: CameraControl,
    host: FakeHost,
}

impl Rig {
    /// The card on a stream with a phone in this state.
    fn new(state: Value) -> Rig {
        let mut r = Rig { cam: CameraControl::new(), host: FakeHost::new() };
        r.cam.on_event(&mut r.host, &Event::PhonesChanged { count: 1 });
        r.cam.stream = Some("url".into());
        r.state(state);
        r
    }

    fn state(&mut self, state: Value) {
        self.cam.on_phone_state(&mut self.host, &state);
        self.host.calls.clear();
    }

    fn action(&mut self, id: &str) -> ActionSpec {
        self.cam.actions().into_iter().find(|a| a.id == id).expect("the action")
    }

    /// action.run(action.full_params(given)).
    fn run(&mut self, id: &str, given: Value) -> Option<String> {
        let mut full = serde_json::Map::new();
        for p in self.action(id).params {
            full.insert(p["name"].as_str().unwrap().to_string(), p["default"].clone());
        }
        full.extend(given.as_object().unwrap().clone());
        self.cam.run_action(&mut self.host, id, &Value::Object(full))
    }

    fn sent(&self) -> Vec<Value> {
        self.host.calls.iter().filter_map(|c| c.strip_prefix("phone_control None ")).map(|b| serde_json::from_str(b).unwrap()).collect()
    }
}

fn choices(action: &ActionSpec) -> Vec<String> {
    action.params[1]["choices"].as_array().unwrap().iter().map(|c| c[0].as_str().unwrap().to_string()).collect()
}

fn one_lens(lens: Value, rest: Value) -> Value {
    let mut state = json!({"cameras": [lens], "auto": true});
    state.as_object_mut().unwrap().extend(rest.as_object().unwrap().clone());
    state
}

#[test]
fn checkable_button_modes() {
    let mut r = Rig::new(one_lens(json!({"id": "0", "label": "Main", "current": true, "supportsFlash": true}), json!({})));
    assert_eq!(r.run("camera.torch", json!({"mode": "toggle"})).as_deref(), Some("Torch: on"));
    assert!(r.cam.torch.checked);
    assert_eq!(r.run("camera.torch", json!({"mode": "on"})).as_deref(), Some("Torch: on"));
    assert_eq!(r.run("camera.torch", json!({"mode": "off"})).as_deref(), Some("Torch: off"));
    assert!(!r.cam.torch.checked);
    let torch = [json!({"action": "torch", "value": "1"}), json!({"action": "torch", "value": "0"})];
    assert_eq!(r.sent(), torch, "already on, a second on is no change");
    r.cam.torch.enabled = false;
    assert_eq!(r.run("camera.torch", json!({"mode": "toggle"})), None);
    assert_eq!(r.sent(), torch);
}

#[test]
fn slider_steps_in_shown_units() {
    let lens = json!({"id": "0", "label": "Main", "current": true, "aeCompMin": -8, "aeCompMax": 8, "aeCompStep": 1.0 / 3.0});
    let mut r = Rig::new(one_lens(lens, json!({})));
    assert_eq!(r.run("camera.compensation", json!({"op": "up", "amount": 1.0})).as_deref(), Some("Compensation: +1.00 EV"));
    assert_eq!(r.cam.ae.value, 3);
    // Never less than one step
    assert_eq!(r.run("camera.compensation", json!({"op": "down", "amount": 0.3})).as_deref(), Some("Compensation: +0.67 EV"));
    assert_eq!(r.cam.ae.value, 2);
    r.run("camera.compensation", json!({"op": "up", "amount": 20}));
    assert_eq!(r.cam.ae.value, 8, "clamped to the end");
    r.run("camera.compensation", json!({"op": "set", "value": -1.0}));
    assert_eq!(r.cam.ae.value, -3);
    r.run("camera.compensation", json!({"op": "reset"}));
    assert_eq!(r.cam.ae.value, 0);
    let seen: Vec<Value> = [3, 2, 8, -3, 0].iter().map(|v| json!({"action": "ae_comp", "value": v})).collect();
    assert_eq!(r.sent(), seen);
    let action = r.action("camera.compensation");
    assert_eq!(action.current, Some(0.0));
    let bound = |k: &str| (action.params[2][k].as_f64().unwrap() * 100.0).round() / 100.0;
    assert_eq!((bound("minimum"), bound("maximum")), (-2.67, 2.67));
    r.cam.ae.enabled = false;
    assert_eq!(r.run("camera.compensation", json!({"op": "up", "amount": 1.0})), None);
    assert_eq!(r.sent(), seen);
}

#[test]
fn log_slider_moves_in_stops() {
    let lens = json!({"id": "0", "label": "Main", "current": true, "isoMin": 50, "isoMax": 6400, "supportsManualSensor": true});
    let mut r = Rig::new(one_lens(lens, json!({"auto": false, "iso": 400})));
    assert_eq!(r.run("camera.iso", json!({"op": "up", "stops": 1})).as_deref(), Some("ISO: ISO 800"));
    // The value sent is the slider's nearest position cut to a whole number, as Python's int(val)
    let sent = r.sent().last().cloned().unwrap();
    assert!(sent["action"] == "iso" && (799..=800).contains(&sent["value"].as_i64().unwrap()), "{sent}");
    r.run("camera.iso", json!({"op": "down", "stops": 3}));
    assert_eq!(r.cam.iso.get_value().round(), 100.0);
    r.run("camera.iso", json!({"op": "set", "value": 99999}));
    assert_eq!(r.cam.iso.get_value().round(), 6400.0);
    r.cam.iso.enabled = false;
    assert_eq!(r.run("camera.iso", json!({"op": "up"})), None);
}

#[test]
fn choice_skips_disabled_items_and_says_it_was_picked() {
    // Manual focus is greyed out on this lens
    let lens = json!({"id": "0", "label": "Main", "current": true, "supportsFocusPoint": true, "supportsManualFocus": false});
    let mut r = Rig::new(one_lens(lens, json!({})));
    assert_eq!(r.run("camera.focus_mode", json!({"op": "next"})).as_deref(), Some("Focus mode: Tap a point"));
    // Manual is greyed out, so it wraps round
    assert_eq!(r.run("camera.focus_mode", json!({"op": "next"})).as_deref(), Some("Focus mode: Auto"));
    assert_eq!(r.run("camera.focus_mode", json!({"op": "previous"})).as_deref(), Some("Focus mode: Tap a point"));
    assert_eq!(r.run("camera.focus_mode", json!({"op": "set", "option": "Auto"})).as_deref(), Some("Focus mode: Auto"));
    assert_eq!(r.run("camera.focus_mode", json!({"op": "set", "option": "Manual"})), None);
    let picked: Vec<String> = r.host.events.iter().filter_map(|e| match e { Event::FocusModePicked { mode } => Some(mode.clone()), _ => None }).collect();
    assert_eq!(picked, ["point", "auto", "point", "auto"]);
    assert_eq!(choices(&r.action("camera.focus_mode")), FOCUS_VALUES.map(String::from), "the greyed out one is still listed");
}

#[test]
fn choice_on_exclusive_buttons_clicks_the_next() {
    let lens = json!({"id": "0", "label": "Main", "current": true, "supportsManualSensor": true});
    let mut r = Rig::new(one_lens(lens, json!({"iso": 400, "shutter_ns": 10_000_000})));
    assert_eq!(r.run("camera.exposure_mode", json!({"op": "next"})).as_deref(), Some("Exposure mode: Manual"));
    assert_eq!(r.cam.exp.index, 1);
    assert_eq!(r.run("camera.exposure_mode", json!({"op": "set", "option": "Auto"})).as_deref(), Some("Exposure mode: Auto"));
    assert_eq!(r.cam.exp.index, 0);
    let actions: Vec<Value> = r.sent().iter().map(|m| m["action"].clone()).collect();
    assert_eq!(actions, ["iso", "shutter", "auto"]);
    assert_eq!(choices(&r.action("camera.exposure_mode")), EXPOSURE_VALUES.map(String::from));
}

#[test]
fn choice_over_buttons_that_come_and_go() {
    let mut r = Rig::new(json!({"cameras": [], "auto": true}));
    assert_eq!(r.run("camera.lens", json!({"op": "next"})), None, "no lenses yet");
    assert!(choices(&r.action("camera.lens")).is_empty());
    r.state(json!({"cameras": [{"id": "0", "label": "Wide", "current": true}, {"id": "1", "label": "Tele"}], "auto": true}));
    assert_eq!(r.run("camera.lens", json!({"op": "next"})).as_deref(), Some("Lens: Tele"));
    assert_eq!(r.cam.lenses.checked, Some(1));
    assert_eq!(choices(&r.action("camera.lens")).len(), 2);
}
