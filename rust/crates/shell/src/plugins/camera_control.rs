//! The Camera card (plugins/camera_control.py): lenses, exposure, focus and image settings sent to the phone. So far
//! the prototype's controls, now as a plugin: the card is camera.slint plus properties, its callbacks come back as
//! UiEvents and its settings go out through Host::phone_control.

use serde_json::{json, Value};
use telescope_core::event::Event;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::{FrameStage, Host};

#[derive(Default)]
pub struct CameraControl {
    phones: u32,
}

impl CameraControl {
    fn active(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &json!({"active": self.phones > 0}));
    }
}

impl Plugin for CameraControl {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "camera_control".into(),
            title: "Camera".into(),
            about: "Exposure, white balance, focus, lenses and the phone camera's other settings.".into(),
            when_off: "Without it there's no Camera card and the phone uses its own automatic settings.".into(),
            category: "camera".into(),
            icon: "camera".into(),
            in_presets: true,
            device_local: true,
            frame_stage: FrameStage::Effects,
            ..Default::default()
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec {
            ui: UiSpec {
                source: SlintSource::Text(include_str!("../../ui/cards/camera.slint").into()),
                component: "CameraCard".into(),
                props: json!({"active": false}),
            },
            region: "right".into(),
        })
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        if let Event::PhonesChanged { count } = event {
            self.phones = *count;
            self.active(host);
        }
    }

    fn on_phone_state(&mut self, host: &mut dyn Host, state: &Value) {
        host.set_ui_props("card", &card_props(state));
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        let s = |i: usize| event.args.get(i).and_then(Value::as_str).unwrap_or("").to_string();
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        let bodies = match event.callback.as_str() {
            "lens-picked" => vec![json!({"action": "camera", "id": s(0)})],
            "control" => vec![control_body(&s(0), n(1))],
            "manual-wb" => {
                let (r, g, b) = kelvin_to_rggb(n(0), n(1));
                vec![json!({"action": "wb_gains", "r": r, "ge": g, "go": g, "b": b})]
            }
            "manual-exposure" => vec![json!({"action": "iso", "value": n(0).round() as i64}),
                                      json!({"action": "shutter", "value": n(1).round() as i64})],
            _ => return,
        };
        for body in bodies {
            host.phone_control(None, &body);
        }
    }
}

/// Python's _WB_NEUTRAL: the temperature where the red and blue gains are equal.
const WB_NEUTRAL: f64 = 5500.0;

/// _kelvin_to_rggb: Camera2 channel gains (red, both greens, blue) for a temperature and a tint from -150 (green) to +150 (magenta).
pub fn kelvin_to_rggb(kelvin: f64, tint: f64) -> (f64, f64, f64) {
    let t = ((kelvin - WB_NEUTRAL) / WB_NEUTRAL).clamp(-1.0, 1.0);
    let r = (2.0 * 2f64.powf(t)).max(0.3);
    let b = (2.0 * 2f64.powf(-t)).max(0.3);
    let g = (1.0 - tint / 500.0).clamp(0.5, 2.5);
    (r, g, b)
}

/// One control from the card as the /v1/control body the phone takes.
pub fn control_body(action: &str, value: f64) -> Value {
    match action {
        "focus_mode" => json!({"action": "focus_mode", "value": if value > 0.5 { "manual" } else { "continuous" }}),
        "focus_distance" => json!({"action": "focus_distance", "value": value}),
        "auto" | "wb_auto" => json!({"action": action}),
        _ => json!({"action": action, "value": value.round() as i64}),
    }
}

/// A lens label from /v1/state as the tile shows it: "Back ~24mm OIS" is ("24", "mm", "OIS").
pub fn lens_tile(cam: &Value) -> Value {
    let label = cam.get("label").and_then(Value::as_str).unwrap_or("");
    let (mut number, mut unit, mut rest) = (String::new(), String::new(), Vec::<String>::new());
    for word in label.split_whitespace() {
        let w = word.trim_start_matches('~');
        let digits: String = w.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        if number.is_empty() && !digits.is_empty() && (w.ends_with("mm") || w.ends_with('x')) {
            unit = w[digits.len()..].to_string();
            number = digits;
        } else if word == "[auto]" {
            rest.insert(0, "Auto".into());
        } else if word == "Front" || word.starts_with("F/") {
            rest.insert(0, "Front".into());
        } else if word != "Back" && word != "[phys]" {
            rest.push(word.to_string());
        }
    }
    if number.is_empty() {
        number = cam.get("id").and_then(Value::as_str).unwrap_or("?").to_string();
    }
    json!({"id": cam.get("id").and_then(Value::as_str).unwrap_or(""), "number": number, "unit": unit, "rest": rest.join(" · ")})
}

/// The card's properties for a /v1/state answer.
pub fn card_props(state: &Value) -> Value {
    let cams: Vec<Value> = state.get("cameras").and_then(Value::as_array).cloned().unwrap_or_default();
    let current = cams.iter().position(|c| c.get("current").and_then(Value::as_bool) == Some(true)).unwrap_or(0);
    let b = |k: &str, d: bool| state.get(k).and_then(Value::as_bool).unwrap_or(d);
    let n = |k: &str, d: i64| state.get(k).and_then(Value::as_i64).unwrap_or(d);
    let mut props = json!({
        "lenses": cams.iter().map(lens_tile).collect::<Vec<_>>(),
        "current-lens": current,
        "exposure-mode": if b("auto", true) { 0 } else { 1 },
        "wb-mode": if b("wb_manual", false) { 1 } else { 0 },
        "ae-comp": n("ae_comp", 0),
        "ois": b("ois", true),
        "black-level-lock": b("black_level_lock", false),
        "torch": b("torch", false),
        "noise-reduction": n("nr_mode", 1).clamp(0, 2),
        "sharpening": n("edge_mode", 1).clamp(0, 2),
        "focus-mode": match state.get("focus_mode").and_then(Value::as_str) { Some("manual") => 2, Some("point") => 1, _ => 0 },
    });
    if let Some(cam) = cams.get(current) {
        let i = |k: &str, d: i64| cam.get(k).and_then(Value::as_i64).unwrap_or(d);
        props["ae-min"] = json!(i("aeCompMin", -8));
        props["ae-max"] = json!(i("aeCompMax", 8));
        props["ae-step"] = json!(cam.get("aeCompStep").and_then(Value::as_f64).unwrap_or(0.167));
        let manual_wb = cam.get("supportsManualWB").and_then(Value::as_bool).unwrap_or(true);
        props["manual-wb-ok"] = json!(manual_wb);
        if !manual_wb {
            props["wb-mode"] = json!(0);
        }
        props["point-focus"] = json!(cam.get("supportsFocusPoint").and_then(Value::as_bool).unwrap_or(false));
    }
    props
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::testing::FakeHost;

    #[test]
    fn white_balance_gains_follow_python() {
        assert_eq!(kelvin_to_rggb(5500.0, 0.0), (2.0, 1.0, 2.0));
        let (r, g, b) = kelvin_to_rggb(11000.0, 150.0);
        assert_eq!((r, b), (4.0, 1.0));
        assert!((g - 0.7).abs() < 1e-9);
        assert_eq!(kelvin_to_rggb(2000.0, -2000.0).1, 2.5);
    }

    #[test]
    fn a_lens_without_manual_white_balance_greys_manual_out() {
        let p = card_props(&json!({"wb_manual": true, "cameras": [{"id": "0", "current": true, "supportsManualWB": false}]}));
        assert_eq!((p["wb-mode"].as_i64(), p["manual-wb-ok"].as_bool()), (Some(0), Some(false)));
        assert_eq!(card_props(&json!({"wb_manual": true}))["wb-mode"], 1);
    }

    #[test]
    fn lens_labels_become_tiles() {
        let t = lens_tile(&json!({"id": "2", "label": "Back ~24mm OIS"}));
        assert_eq!(t, json!({"id": "2", "number": "24", "unit": "mm", "rest": "OIS"}));
        assert_eq!(lens_tile(&json!({"id": "5", "label": "Front [auto]"}))["rest"], "Auto · Front");
        assert_eq!(lens_tile(&json!({"id": "7", "label": ""}))["number"], "7");
    }

    #[test]
    fn controls_go_to_the_phone_as_its_bodies() {
        let mut host = FakeHost::new();
        let mut cam = CameraControl::default();
        cam.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "control".into(), args: vec![json!("ois"), json!(0.0)] });
        cam.on_ui(&mut host, &UiEvent { ui: "card".into(), callback: "control".into(), args: vec![json!("focus_mode"), json!(1.0)] });
        assert_eq!(control_body("ae_comp", 2.6), json!({"action": "ae_comp", "value": 3}));
        assert_eq!(control_body("wb_auto", 0.0), json!({"action": "wb_auto"}));
        let sent = host.calls.join("\n");
        assert!(sent.contains("ois") && sent.contains("manual"), "{sent}");
    }

    #[test]
    fn the_phones_state_fills_the_card() {
        let props = card_props(&json!({"auto": false, "ae_comp": 2, "focus_mode": "point",
            "cameras": [{"id": "0", "label": "Back ~24mm", "current": false}, {"id": "1", "label": "Back ~13mm", "current": true, "aeCompMin": -12}]}));
        assert_eq!(props["current-lens"], 1);
        assert_eq!(props["exposure-mode"], 1);
        assert_eq!(props["focus-mode"], 1);
        assert_eq!(props["ae-min"], -12);
    }
}
