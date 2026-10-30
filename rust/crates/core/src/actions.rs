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

/// Python's round(v, decimals): v's exact value rounded to that many places (a half goes to the even neighbour), which a
/// multiply and round gets wrong for a number like 2.675 that is just under its half.
pub fn round_to(v: f64, decimals: usize) -> f64 {
    format!("{v:.decimals$}").parse().unwrap_or(v)
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

/// log_pos_to_val: a log slider's position (0..steps) as its value; a range of one value (or none) stays at its bottom.
pub fn log_pos_to_val(pos: f64, steps: f64, v_min: f64, v_max: f64) -> f64 {
    let v_min = if v_min <= 0.0 { 1.0 } else { v_min };
    if v_max <= v_min {
        return v_min;
    }
    let t = pos / steps.max(1.0);
    (v_min.ln() + t * (v_max.ln() - v_min.ln())).exp().clamp(v_min, v_max)
}

/// val_to_log_pos: where a value sits on a log slider of `steps` positions.
pub fn val_to_log_pos(val: f64, steps: f64, v_min: f64, v_max: f64) -> f64 {
    if val <= 0.0 || v_min <= 0.0 || v_max <= v_min {
        return 0.0;
    }
    let val = val.clamp(v_min, v_max);
    round_half_even((val.ln() - v_min.ln()) / (v_max.ln() - v_min.ln()) * steps)
}

/// What a log slider's shortcut needs (log_slider_action): its range and value in shown units (the row's value times
/// the box's display scale, like nanoseconds shown as milliseconds).
#[derive(Clone, Debug)]
pub struct LogSliderSpec<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub group: &'a str,
    pub keywords: &'a str,
    pub minimum: f64,
    pub maximum: f64,
    pub decimals: usize,
    pub suffix: &'a str,
}

/// The action for a log slider (ISO, shutter): up or down in stops, each one doubling or halving it, or to a value.
pub fn log_slider_action(s: &LogSliderSpec, current: f64) -> ActionSpec {
    ActionSpec {
        id: s.id.into(),
        label: s.label.into(),
        group: s.group.into(),
        params: vec![
            json!({"name": "op", "label": tr("Does"), "kind": "choice", "default": "up",
                   "choices": [["up", tr("Raise by")], ["down", tr("Lower by")], ["set", tr("Set to")]]}),
            // Translators: the unit after a number of stops, with its space: "1.5 stops"
            json!({"name": "stops", "label": tr("Stops"), "kind": "number", "default": 1.0, "minimum": 0.1, "maximum": 10.0,
                   "decimals": 1, "suffix": tr(" stops"), "shown_if": ["op", ["up", "down"]]}),
            json!({"name": "value", "label": tr("Value"), "kind": "number", "default": null, "minimum": s.minimum,
                   "maximum": s.maximum, "decimals": s.decimals, "suffix": s.suffix, "shown_if": ["op", ["set"]]}),
        ],
        repeats: true,
        on_release: false,
        current: Some(round_to(current, s.decimals)),
        keywords: s.keywords.into(),
    }
}

/// A log slider's value after a key with these params, clamped to v_min..v_max, or None for a "Set to" with no value.
/// `display_scale` turns the value into what its box shows.
pub fn log_slider_target(params: &Value, current: f64, v_min: f64, v_max: f64, display_scale: f64) -> Option<f64> {
    let new = match params.get("op").and_then(Value::as_str).unwrap_or("up") {
        "set" => params.get("value").and_then(Value::as_f64)? / display_scale,
        op => {
            let stops = params.get("stops").and_then(Value::as_f64).unwrap_or(0.0);
            current * 2f64.powf(if op == "up" { stops } else { -stops })
        }
    };
    Some(new.max(v_min).min(v_max))
}

/// The action for a row of choices only one of which is on: the next one, the previous one, or a named one
/// (choice_action). `options` are (stable value, shown text), already translated.
pub fn choice_action(id: &str, label: &str, group: &str, keywords: &str, options: &[(&str, String)]) -> ActionSpec {
    let choices: Vec<Value> = options.iter().map(|(v, shown)| json!([v, shown])).collect();
    ActionSpec {
        id: id.into(),
        label: label.into(),
        group: group.into(),
        params: vec![
            json!({"name": "op", "label": tr("Does"), "kind": "choice", "default": "next",
                   "choices": [["next", tr("Next")], ["previous", tr("Previous")], ["set", tr("Pick")]]}),
            json!({"name": "option", "label": tr("Option"), "kind": "choice", "default": "", "choices": choices, "shown_if": ["op", ["set"]]}),
        ],
        repeats: true,
        on_release: false,
        current: None,
        keywords: keywords.into(),
    }
}

/// One choice as a key sees it.
pub struct Choice<'a> {
    pub value: &'a str,
    pub shown: &'a str,
    pub enabled: bool,
}

/// What a choice key does: (the choice to say something about, whether to pick it). None when there's nothing to say.
/// `current` is the one that's on; `wrap` goes round at the ends.
pub fn choice_target(params: &Value, options: &[Choice], current: Option<usize>, wrap: bool) -> Option<(usize, bool)> {
    if options.is_empty() {
        return None;
    }
    let op = params.get("op").and_then(Value::as_str).unwrap_or("next");
    if op == "set" {
        let want = params.get("option").and_then(Value::as_str);
        let target = options.iter().position(|o| o.enabled && (want == Some(o.value) || want == Some(o.shown)))?;
        return Some((target, current != Some(target)));
    }
    let n = options.len() as i64;
    let step: i64 = if op == "next" { 1 } else { -1 };
    let cur = current.map_or(-1, |c| c as i64);
    let mut i = cur;
    for _ in 0..n {
        i += step;
        if !wrap && !(0..n).contains(&i) {
            break;
        }
        i = i.rem_euclid(n);
        if options[i as usize].enabled && i != cur {
            return Some((i as usize, true));
        }
    }
    current.filter(|c| *c < options.len()).map(|c| (c, false))
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
    fn a_choice_goes_next_previous_or_to_a_named_one() {
        let opts = [Choice { value: "None", shown: "0\u{b0}", enabled: true }, Choice { value: "90 CW", shown: "90\u{b0}", enabled: true },
                    Choice { value: "180", shown: "180\u{b0}", enabled: false }, Choice { value: "90 CCW", shown: "270\u{b0}", enabled: true }];
        let go = |p: Value, cur: Option<usize>, wrap: bool| choice_target(&p, &opts, cur, wrap);
        assert_eq!(go(json!({}), Some(0), true), Some((1, true)));
        assert_eq!(go(json!({"op": "next"}), Some(1), true), Some((3, true)), "the one that's off is skipped");
        assert_eq!(go(json!({"op": "next"}), Some(3), true), Some((0, true)));
        assert_eq!(go(json!({"op": "next"}), Some(3), false), Some((3, false)));
        assert_eq!(go(json!({"op": "previous"}), Some(0), true), Some((3, true)));
        assert_eq!(go(json!({"op": "set", "option": "90 CW"}), Some(0), true), Some((1, true)));
        assert_eq!(go(json!({"op": "set", "option": "270\u{b0}"}), Some(3), true), Some((3, false)), "by what it shows too");
        assert_eq!(go(json!({"op": "set", "option": "180"}), Some(0), true), None);
        assert_eq!(choice_target(&json!({}), &[], None, true), None);
        let spec = choice_action("t.rot", "Rotation", "T", "", &[("None", "0\u{b0}".into())]);
        assert!(spec.repeats && spec.params[1]["choices"][0] == json!(["None", "0\u{b0}"]));
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

    #[test]
    fn a_log_slider_steps_in_stops_and_sets_in_shown_units() {
        let spec = LogSliderSpec { id: "camera.shutter", label: "Shutter", group: "Camera", keywords: "", minimum: 0.1,
                                   maximum: 1000.0, decimals: 2, suffix: " ms" };
        let a = log_slider_action(&spec, 16.666_667);
        assert_eq!((a.current, a.params[1]["name"].clone(), a.params[2]["maximum"].clone()), (Some(16.67), json!("stops"), json!(1000.0)));
        assert_eq!(log_slider_target(&json!({"op": "up", "stops": 1.0}), 400.0, 50.0, 6400.0, 1.0), Some(800.0));
        assert_eq!(log_slider_target(&json!({"op": "down", "stops": 10.0}), 400.0, 50.0, 6400.0, 1.0), Some(50.0));
        assert_eq!(log_slider_target(&json!({"op": "set", "value": 2.0}), 1.0, 100_000.0, 1e9, 1e-6), Some(2_000_000.0));
        assert_eq!(log_slider_target(&json!({"op": "set"}), 1.0, 1.0, 2.0, 1.0), None);
    }

    #[test]
    fn log_positions_go_both_ways_and_a_flat_range_stays_put() {
        assert_eq!(val_to_log_pos(400.0, 2000.0, 50.0, 6400.0), 857.0);
        assert!((log_pos_to_val(857.0, 2000.0, 50.0, 6400.0) - 400.0).abs() < 1.0);
        assert_eq!((log_pos_to_val(5.0, 2000.0, 80.0, 80.0), val_to_log_pos(80.0, 2000.0, 80.0, 80.0)), (80.0, 0.0));
    }

    fn near(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= b.abs() * rel + 1e-9
    }

    // test_widgets.py::test_log_scale_endpoints_midpoint_and_clamping
    #[test]
    fn log_scale_endpoints_midpoint_and_clamping() {
        assert!(near(log_pos_to_val(0.0, 100.0, 10.0, 1000.0), 10.0, 1e-6));
        assert!(near(log_pos_to_val(100.0, 100.0, 10.0, 1000.0), 1000.0, 1e-6));
        assert!(near(log_pos_to_val(50.0, 100.0, 10.0, 1000.0), 100.0, 1e-6));
        assert!(near(log_pos_to_val(-20.0, 100.0, 10.0, 1000.0), 10.0, 1e-6));
        assert!(near(log_pos_to_val(120.0, 100.0, 10.0, 1000.0), 1000.0, 1e-6));
        assert!(near(log_pos_to_val(0.0, 0.0, 0.0, 100.0), 1.0, 1e-6));
    }

    // test_widgets.py::test_value_to_log_position_endpoints_and_invalid_values
    #[test]
    fn value_to_log_position_endpoints_and_invalid_values() {
        assert_eq!(val_to_log_pos(10.0, 100.0, 10.0, 1000.0), 0.0);
        assert_eq!(val_to_log_pos(1000.0, 100.0, 10.0, 1000.0), 100.0);
        assert_eq!(val_to_log_pos(100.0, 100.0, 10.0, 1000.0), 50.0);
        assert_eq!(val_to_log_pos(-1.0, 100.0, 10.0, 1000.0), 0.0);
        assert_eq!(val_to_log_pos(50.0, 100.0, 0.0, 1000.0), 0.0);
    }

    // test_widgets.py::test_log_scale_survives_a_camera_reporting_an_empty_or_single_value_range
    #[test]
    fn log_scale_survives_a_camera_reporting_an_empty_or_single_value_range() {
        assert_eq!(log_pos_to_val(50.0, 100.0, 50.0, 0.0), 50.0);
        assert_eq!(log_pos_to_val(50.0, 100.0, 100.0, 100.0), 100.0);
        assert_eq!(val_to_log_pos(100.0, 100.0, 100.0, 100.0), 0.0);
        assert_eq!(val_to_log_pos(100.0, 100.0, 50.0, 0.0), 0.0);
    }

    // test_widgets.py::test_log_scale_round_trip_is_close_across_range
    #[test]
    fn log_scale_round_trip_is_close_across_range() {
        for value in [50.0, 100.0, 400.0, 1600.0, 6400.0] {
            let pos = val_to_log_pos(value, 2000.0, 50.0, 6400.0);
            assert!(near(log_pos_to_val(pos, 2000.0, 50.0, 6400.0), value, 0.003), "{value}");
        }
    }
}
