//! What `telescope --action` and the MCP server's run_action share: the actions' params and how they are checked, the app's own actions and the
//! request `--action` sends. Not a plugin, so Shortcuts and MCP use it without knowing each other.

use serde_json::{json, Map, Value};
use telescope_core::actions;
use telescope_core::host::Host;
use telescope_core::i18n::{n_, tr, Fields};
use telescope_core::ui::ActionSpec;

pub(crate) const WINDOW_SHOWN: &str = n_("Telescope: shown");

pub(crate) fn choice(name: &str, label: &str, default: &str, choices: &[(&str, String)]) -> Value {
    json!({"name": name, "label": label, "kind": "choice", "default": default,
           "choices": choices.iter().map(|(v, l)| json!([v, l])).collect::<Vec<_>>()})
}

/// The app's own actions, which belong to no card (create_actions).
pub(crate) fn own_actions() -> Vec<ActionSpec> {
    let streaming = [("toggle", tr("Start or stop")), ("start", tr("Start")), ("stop", tr("Stop"))];
    let camera = [("toggle", tr("Switch on or off")), ("on", tr("Turn on")), ("off", tr("Turn off"))];
    let spec = |id: &str, label: String, params: Vec<Value>| ActionSpec {
        id: id.into(), label, group: "Telescope".into(), params, repeats: false, on_release: false, current: None,
        keywords: String::new(),
    };
    vec![
        spec("streaming.start_stop", tr("Streaming"), vec![choice("mode", &tr("Does"), "toggle", &streaming)]),
        spec("streaming.camera", tr("Phone camera"), vec![choice("mode", &tr("Does"), "toggle", &camera)]),
        spec("window.show_hide", tr("Show or hide Telescope"), Vec::new()),
    ]
}

pub(crate) fn run_own(host: &mut dyn Host, id: &str, p: &Value) -> Option<String> {
    let mode = p["mode"].as_str().unwrap_or("toggle");
    match id {
        "streaming.start_stop" => {
            let running = host.is_streaming() || host.is_starting();
            if mode == "start" || (mode == "toggle" && !running) {
                if running {
                    return Some(tr("Already streaming"));
                }
                // A key or --action from another app: not a click in the window, so no questions in dialogs
                host.start_stream(false);
                return Some(tr("Streaming: starting"));
            }
            if !running {
                return Some(tr("Not streaming"));
            }
            host.stop_all_streams();
            Some(tr("Streaming: stopped"))
        }
        "streaming.camera" => {
            let on = host.is_camera_on();
            let want = match mode {
                "on" => true,
                "off" => false,
                _ => !on,
            };
            if want != on {
                if !want && !host.can_turn_camera_off().0 {
                    return None;
                }
                host.set_camera_on(want, false);
            }
            Some(if host.is_camera_on() { tr("Phone camera: on") } else { tr("Phone camera: off") })
        }
        "window.show_hide" => Some(if host.toggle_window() { tr(WINDOW_SHOWN) } else { tr("Telescope: hidden") }),
        _ => None,
    }
}

/// Every action by its id in the order the plugins give them, this one's own where it sits (first when the host
/// doesn't list it), then grouped by card with the app's own group first (start()'s grouping); the plugin that runs
/// each, None for this one.
pub(crate) fn all_actions(host: &dyn Host) -> Vec<(Option<String>, ActionSpec)> {
    let listed = host.shortcut_actions();
    let mut out: Vec<(Option<String>, ActionSpec)> = Vec::new();
    if !listed.iter().any(|(plugin, _)| plugin == "shortcuts") {
        out.extend(own_actions().into_iter().map(|a| (None, a)));
    }
    let mut own_added = !out.is_empty();
    for (plugin, action) in listed {
        if plugin == "shortcuts" {
            if !own_added {
                own_added = true;
                out.extend(own_actions().into_iter().filter(|o| !out.iter().any(|(_, a)| a.id == o.id)).map(|a| (None, a)).collect::<Vec<_>>());
            }
        } else if !out.iter().any(|(_, a)| a.id == action.id) {
            out.push((Some(plugin), action));
        }
    }
    let mut groups = vec!["Telescope".to_string()];
    for (_, a) in &out {
        if !groups.contains(&a.group) {
            groups.push(a.group.clone());
        }
    }
    out.sort_by_key(|(_, a)| groups.iter().position(|g| *g == a.group));
    out
}

pub(crate) fn choices(p: &Value) -> Vec<(String, String)> {
    p["choices"].as_array().map(|list| list.iter().filter_map(|c| {
        let value = match &c[0] { Value::String(s) => s.clone(), other => other.to_string() };
        Some((value, c[1].as_str()?.to_string()))
    }).collect()).unwrap_or_default()
}

pub(crate) fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

pub(crate) fn shown(p: &Value, full: &Map<String, Value>) -> bool {
    match p.get("shown_if").and_then(Value::as_array) {
        Some(cond) => {
            let value = cond.first().and_then(Value::as_str).and_then(|n| full.get(n)).cloned().unwrap_or(Value::Null);
            cond.get(1).and_then(Value::as_array).is_some_and(|vals| vals.contains(&value))
        }
        None => true,
    }
}

/// A param's value as it takes it, or its default when it can't be (Param.clean).
pub(crate) fn clean_value(p: &Value, given: Option<&Value>) -> Value {
    let default = p["default"].clone();
    let Some(given) = given else { return default };
    if p["kind"] == "number" {
        let Some(n) = (match given { Value::Bool(_) => None, v => number(v) }).filter(|n| n.is_finite()) else { return default };
        let lo = p["minimum"].as_f64().unwrap_or(f64::NEG_INFINITY);
        let hi = p["maximum"].as_f64().unwrap_or(f64::INFINITY).max(lo);
        let n = n.clamp(lo, hi);
        return json!(actions::round_to(n, p["decimals"].as_u64().unwrap_or(0) as usize));
    }
    if choices(p).iter().any(|(v, _)| Value::String(v.clone()) == *given) {
        return given.clone();
    }
    // A choice that's gone from a list that changes (a deleted preset) stays, so the binding still says what it was for
    if p["dynamic"] == true && given.as_str().is_some_and(|s| !s.is_empty()) {
        return given.clone();
    }
    default
}

/// params with every one of the action's settings filled in and cleaned (full_params).
pub(crate) fn full_params(action: &ActionSpec, params: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for p in &action.params {
        let Some(name) = p["name"].as_str() else { continue };
        out.insert(name.into(), clean_value(p, params.get(name)));
    }
    out
}

/// An action's params as it runs with them, or why it won't: a name it doesn't have, a value it can't take, a number it needs and wasn't
/// given. A typo must not quietly run the default. `strict_range` turns a number outside its range into a problem where `--action` carries on
/// with the nearest one it takes.
pub fn checked_params(action: &ActionSpec, given: &Map<String, Value>, strict_range: bool) -> Result<Map<String, Value>, String> {
    let mut unknown: Vec<&String> = given.keys().filter(|k| !action.params.iter().any(|p| p["name"] == **k)).collect();
    unknown.sort();
    if let Some(name) = unknown.first() {
        return Err(tr("{action} has no setting {name}.").field("action", &action.id).field("name", format!("'{name}'")));
    }
    match param_problem(action, given, strict_range) {
        Some(problem) => Err(problem),
        None => Ok(full_params(action, given)),
    }
}

/// Why --action's settings won't do, or None (_remote_param_problem): a typo must not quietly run the default.
pub(crate) fn param_problem(action: &ActionSpec, params: &Map<String, Value>, strict_range: bool) -> Option<String> {
    for p in &action.params {
        let Some(name) = p["name"].as_str() else { continue };
        let Some(value) = params.get(name) else { continue };
        let shown_value = match value { Value::String(s) => format!("'{s}'"), other => other.to_string() };
        if p["kind"] == "number" {
            if !number(value).is_some_and(f64::is_finite) {
                return Some(tr("{name} has to be a number, not {value}.").field("name", name).field("value", &shown_value));
            }
            let n = number(value).unwrap_or_default();
            let (lo, hi) = (p["minimum"].as_f64().unwrap_or(f64::NEG_INFINITY), p["maximum"].as_f64().unwrap_or(f64::INFINITY));
            if strict_range && (n < lo || n > hi) {
                return Some(tr("{name} goes from {low} to {high}, so {value} is out of range.").field("name", name).field("low", fmt_number(lo))
                    .field("high", fmt_number(hi)).field("value", fmt_number(n)));
            }
        } else {
            let options = choices(p);
            if !options.iter().any(|(v, _)| Value::String(v.clone()) == *value) {
                let listed = if options.is_empty() { tr("nothing right now") } else {
                    options.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>().join(" | ")
                };
                return Some(tr("{name} can be {options}, not {value}.").field("name", name).field("options", &listed)
                    .field("value", &shown_value));
            }
        }
        if value == "hold" {
            return Some(tr("hold needs a key to let go of; use toggle, on or off with --action."));
        }
    }
    let full = full_params(action, params);
    for p in &action.params {
        let Some(name) = p["name"].as_str() else { continue };
        if shown(p, &full) && p["kind"] == "number" && full.get(name).is_none_or(Value::is_null) {
            return Some(tr("{action} needs {name}=… for that.").field("action", &action.id).field("name", name));
        }
    }
    None
}

pub(crate) fn fmt_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { format!("{n}") }
}

/// What --list-actions prints (list_text).
pub(crate) fn list_text(actions: &[(Option<String>, ActionSpec)]) -> String {
    let mut lines = Vec::new();
    for (_, a) in actions {
        lines.push(format!("{}  ({}: {})", a.id, a.group, a.label));
        for p in &a.params {
            let name = p["name"].as_str().unwrap_or("");
            if p["kind"] == "number" {
                let (lo, hi) = (p["minimum"].as_f64().unwrap_or(0.0), p["maximum"].as_f64().unwrap_or(0.0));
                lines.push(format!("    {name}=<number {} to {}{}>", fmt_number(lo), fmt_number(hi), p["suffix"].as_str().unwrap_or("")));
            } else {
                let values = choices(p).into_iter().map(|(v, _)| v).collect::<Vec<_>>().join(" | ");
                let values = if values.is_empty() { "…".to_string() } else { values };
                let default = match &p["default"] { Value::String(s) => s.clone(), Value::Null => String::new(), v => v.to_string() };
                lines.push(if default.is_empty() { format!("    {name}={values}") } else { format!("    {name}={values}  (default {default})") });
            }
        }
    }
    lines.join("\n")
}

/// A request's answer; a crash in it is still answered, or the asking copy waits out its time.
pub(crate) fn answered(run: impl FnOnce() -> Value) -> Value {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|_| {
        log::error!("A --action request failed");
        json!({"ok": false, "text": tr("The action failed. Copy diagnostics has the details.")})
    })
}

/// A `telescope --action` request: {"list": true}, or {"action": id, "params": {name: value}} (handle_remote).
pub fn handle_remote(host: &mut dyn Host, request: &Value) -> Value {
    let actions = all_actions(host);
    if request["list"].as_bool() == Some(true) {
        return json!({"ok": true, "text": list_text(&actions)});
    }
    let wanted = request["action"].as_str().unwrap_or("");
    let Some((plugin, action)) = actions.into_iter().find(|(_, a)| a.id == wanted) else {
        let shown = match &request["action"] { Value::String(s) => format!("'{s}'"), Value::Null => "None".into(), v => v.to_string() };
        return json!({"ok": false, "text": tr("No action called {action}. telescope --list-actions lists them.").field("action", &shown)});
    };
    let params = request["params"].as_object().cloned().unwrap_or_default();
    let full = match checked_params(&action, &params, false) {
        Ok(full) => Value::Object(full),
        Err(text) => return json!({"ok": false, "text": text}),
    };
    let said = match &plugin {
        None => run_own(host, &action.id, &full),
        Some(owner) => host.run_action(owner, &action.id, &full),
    };
    match said {
        Some(text) => json!({"ok": true, "text": text}),
        None => json!({"ok": false, "text": tr("{action} can't be used right now (no device, or the control is off).").field("action", &action.label)}),
    }
}
