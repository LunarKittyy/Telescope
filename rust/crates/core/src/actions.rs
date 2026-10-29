//! The shortcut actions a switch or a slider offers (telescope.shortcuts.button_action and slider_action): what the shortcut
//! editor asks for (an ActionSpec with its params) and the arithmetic of running one. The card keeps its own control; the
//! plugin hands these the control's value and gets the new one back.

use crate::i18n::{n_, number, percent, tr};
use crate::ui::ActionSpec;
use serde_json::{json, Value};

/// How a switch can be flipped, as (mode, label).
pub const TOGGLE_MODES: [(&str, &str); 4] = [
    ("toggle", n_("Switch on or off")),
    ("on", n_("Turn on")),
    ("off", n_("Turn off")),
    ("hold", n_("Switch while held, back on release")),
];

/// Python's round(): halves go to the even neighbour.
pub fn round_half_even(v: f64) -> f64 {
    let r = v.round();
    if (v - v.trunc()).abs() == 0.5 && r % 2.0 != 0.0 { r - v.signum() } else { r }
}

/// The action for a switch (button_action on a checkable button). `verbs` are labels for the modes in place of the generic
/// ones; they are already translated.
pub fn toggle_action(id: &str, label: &str, group: &str, keywords: &str, verbs: &[(&str, String)]) -> ActionSpec {
    let modes: Vec<Value> = TOGGLE_MODES.iter().map(|(m, text)| {
        let shown = verbs.iter().find(|(v, _)| v == m).map_or_else(|| tr(text), |(_, t)| t.clone());
        json!([m, shown])
    }).collect();
    ActionSpec {
        id: id.into(),
        label: label.into(),
        group: group.into(),
        params: vec![json!({"name": "mode", "label": tr("Does"), "kind": "choice", "default": "toggle", "choices": modes})],
        repeats: false,
        on_release: true,
        current: None,
        keywords: keywords.into(),
    }
}

/// What a switch should be after a key with these params, given where it is: `held` remembers where it was when a hold began
/// (the opposite of that, however often the key repeats) and is cleared by `hold_released`.
pub fn toggle_wanted(params: &Value, is_on: bool, held: &mut Option<bool>) -> bool {
    match params.get("mode").and_then(Value::as_str).unwrap_or("toggle") {
        "on" => true,
        "off" => false,
        "hold" => !*held.get_or_insert(is_on),
        _ => !is_on,
    }
}

/// A held key came back up: where the switch was before the hold, to put it back there (None when it wasn't a hold).
pub fn hold_released(params: &Value, held: &mut Option<bool>) -> Option<bool> {
    if params.get("mode").and_then(Value::as_str) != Some("hold") {
        return None;
    }
    held.take()
}

/// "Colour grading: on" for a switch.
pub fn toggle_text(label: &str, on: bool) -> String {
    tr("{label}: {value}").replace("{label}", label).replace("{value}", &if on { tr("on") } else { tr("off") })
}

/// What a slider's shortcut needs to know, in the units the control shows (its own steps times `scale`).
#[derive(Clone, Debug)]
pub struct SliderSpec<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub group: &'a str,
    pub keywords: &'a str,
    /// The control's range in shown units.
    pub minimum: f64,
    pub maximum: f64,
    /// A new binding's amount to start with.
    pub step: f64,
    pub decimals: usize,
    pub suffix: &'a str,
    /// The control can go back to a default ("Reset to default").
    pub has_default: bool,
}

/// The action for a slider: up or down by an amount, to a value, or back to its default. `current` is where the control is
/// now in shown units.
pub fn slider_action(s: &SliderSpec, current: f64) -> ActionSpec {
    let mut ops = vec![json!(["up", tr("Increase by")]), json!(["down", tr("Decrease by")]), json!(["set", tr("Set to")])];
    if s.has_default {
        ops.push(json!(["reset", tr("Reset to default")]));
    }
    let span = (s.maximum - s.minimum).max(0.0);
    ActionSpec {
        id: s.id.into(),
        label: s.label.into(),
        group: s.group.into(),
        params: vec![
            json!({"name": "op", "label": tr("Does"), "kind": "choice", "default": "up", "choices": ops}),
            json!({"name": "amount", "label": tr("By"), "kind": "number", "default": s.step, "minimum": 0.0, "maximum": span,
                   "decimals": s.decimals, "suffix": s.suffix, "shown_if": ["op", ["up", "down"]]}),
            json!({"name": "value", "label": tr("Value"), "kind": "number", "default": null, "minimum": s.minimum,
                   "maximum": s.maximum, "decimals": s.decimals, "suffix": s.suffix, "shown_if": ["op", ["set"]]}),
        ],
        repeats: true,
        on_release: false,
        current: Some(round_to(current, s.decimals)),
        keywords: s.keywords.into(),
    }
}

fn round_to(v: f64, decimals: usize) -> f64 {
    let k = 10f64.powi(decimals as i32);
    round_half_even(v * k) / k
}

/// The slider's steps after a key with these params, from where it is (`current`), or None when there's nothing to do (a
/// "Set to" with no value). `scale` is shown units per step; `default` is the step a reset goes to.
pub fn slider_target(params: &Value, current: i64, scale: f64, minimum: i64, maximum: i64, default: Option<i64>) -> Option<i64> {
    let op = params.get("op").and_then(Value::as_str).unwrap_or("up");
    let new = match op {
        "set" => {
            let value = params.get("value").and_then(Value::as_f64)?;
            round_half_even(value / scale) as i64
        }
        "reset" => default.unwrap_or(current),
        _ => {
            let amount = params.get("amount").and_then(Value::as_f64).unwrap_or(0.0);
            let steps = if amount != 0.0 { (round_half_even(amount / scale) as i64).max(1) } else { 0 };
            if op == "up" { current + steps } else { current - steps }
        }
    };
    Some(new.clamp(minimum, maximum))
}

/// A value as shown: percent for a "%" suffix, the number and its suffix otherwise.
pub fn shown(value: f64, decimals: usize, suffix: &str, signed: bool) -> String {
    if suffix == "%" { percent(value, decimals, signed) } else { number(value, decimals, signed) + suffix }
}

/// "Contrast: 1.40" after a slider moved, from its readout text.
pub fn slider_text(label: &str, readout: &str) -> String {
    tr("{label}: {value}").replace("{label}", label).replace("{value}", readout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switch_toggles_turns_on_or_off_and_flips_while_held() {
        let mut held = None;
        assert!(toggle_wanted(&json!({"mode": "toggle"}), false, &mut held));
        assert!(!toggle_wanted(&json!({"mode": "off"}), true, &mut held));
        assert!(toggle_wanted(&json!({}), false, &mut held));
        // A hold is the opposite of where it began, however often the key repeats
        let hold = json!({"mode": "hold"});
        assert!(toggle_wanted(&hold, false, &mut held));
        assert!(toggle_wanted(&hold, true, &mut held));
        assert_eq!(hold_released(&hold, &mut held), Some(false));
        assert_eq!(hold_released(&hold, &mut held), None);
        assert_eq!(hold_released(&json!({"mode": "on"}), &mut Some(true)), None);
    }

    #[test]
    fn the_switch_action_asks_how_to_flip_it() {
        let a = toggle_action("x.on", "X", "G", "", &[("on", "Mute".into())]);
        assert!(a.on_release && !a.repeats);
        let choices = a.params[0]["choices"].as_array().unwrap();
        assert_eq!(choices[0], json!(["toggle", "Switch on or off"]));
        assert_eq!(choices[1], json!(["on", "Mute"]));
        assert_eq!(toggle_text("Colour grading", true), "Colour grading: on");
    }

    #[test]
    fn a_slider_goes_up_down_to_a_value_or_back() {
        let at = |p: Value, cur: i64| slider_target(&p, cur, 1.0, -100, 100, Some(0));
        assert_eq!(at(json!({"op": "up", "amount": 5.0}), 10), Some(15));
        assert_eq!(at(json!({"op": "down", "amount": 5.0}), -98), Some(-100));
        assert_eq!(at(json!({"op": "set", "value": 30.0}), 0), Some(30));
        assert_eq!(at(json!({"op": "set"}), 0), None);
        assert_eq!(at(json!({"op": "reset"}), 40), Some(0));
        assert_eq!(at(json!({"op": "up", "amount": 0.2}), 3), Some(4), "a step is at least one");
        // Contrast shows hundredths of the slider's steps
        assert_eq!(slider_target(&json!({"op": "up", "amount": 0.05}), 100, 0.01, 0, 200, Some(100)), Some(105));
        assert_eq!(round_half_even(2.5), 2.0);
        assert_eq!(round_half_even(3.5), 4.0);
        assert_eq!(round_half_even(-2.5), -2.0);
    }

    #[test]
    fn the_slider_action_says_where_it_is() {
        let spec = SliderSpec { id: "x.zoom", label: "Zoom", group: "G", keywords: "", minimum: 1.0, maximum: 10.0, step: 0.25, decimals: 2, suffix: "\u{d7}", has_default: true };
        let a = slider_action(&spec, 2.5);
        assert!(a.repeats && a.current == Some(2.5));
        assert_eq!(a.params[0]["choices"].as_array().unwrap().len(), 4);
        assert_eq!(a.params[1]["maximum"], json!(9.0));
        assert_eq!(shown(1.5, 2, "\u{d7}", false), "1.50\u{d7}");
        assert_eq!(shown(80.0, 0, "%", false), "80%");
    }
}
