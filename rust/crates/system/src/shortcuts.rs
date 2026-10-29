//! Keyboard shortcuts (plugins/shortcuts.py), so far the part `telescope --action` needs: the app's own actions
//! (streaming, the phone camera, the window) and running any plugin's action from a request the single-instance
//! lock hands over. Bindings, the key filter and global shortcuts come later.
//!
//! An action's params are JSON: {"name", "label", "kind": "choice", "default", "choices": [[value, label], ...],
//! "shown_if": [name, [values]]} or {"name", "label", "kind": "number", "default", "min", "max", "suffix"}.

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::event::Event;
use telescope_core::host::{Host, Poster};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::ActionSpec;

const WINDOW_SHOWN: &str = n_("Telescope: shown");
const REMOTE: &str = "remote";

#[derive(Default)]
struct Remote {
    poster: Option<Arc<dyn Poster>>,
    next: u64,
    waiting: HashMap<u64, Sender<Value>>,
}

/// Hands `--action` requests from the lock's thread to the plugin on the window's thread and waits for the answer
/// (TelescopeWindow.run_remote_action).
#[derive(Clone, Default)]
pub struct RemoteActions(Arc<Mutex<Remote>>);

impl RemoteActions {
    /// From the single-instance thread: run request on the window's thread, waiting up to 5 s.
    pub fn handle(&self, request: Value) -> Value {
        let (tx, rx) = channel();
        let (poster, id) = {
            let mut r = self.0.lock().unwrap();
            let Some(poster) = r.poster.clone() else {
                return json!({"ok": false, "text": tr("This Telescope has no shortcuts.")});
            };
            r.next += 1;
            let id = r.next;
            r.waiting.insert(id, tx);
            (poster, id)
        };
        poster.post(REMOTE, json!({"id": id, "request": request}));
        let answer = rx.recv_timeout(Duration::from_secs(5));
        self.0.lock().unwrap().waiting.remove(&id);
        answer.unwrap_or_else(|_| json!({"ok": false, "text": tr("Telescope didn't answer in time.")}))
    }

    fn reply(&self, id: u64, answer: Value) {
        if let Some(tx) = self.0.lock().unwrap().waiting.remove(&id) {
            let _ = tx.send(answer);
        }
    }
}

pub struct ShortcutsPlugin {
    remote: RemoteActions,
}

impl ShortcutsPlugin {
    pub fn new(remote: RemoteActions) -> ShortcutsPlugin {
        ShortcutsPlugin { remote }
    }
}

fn choice(name: &str, label: &str, default: &str, choices: &[(&str, String)]) -> Value {
    json!({"name": name, "label": label, "kind": "choice", "default": default,
           "choices": choices.iter().map(|(v, l)| json!([v, l])).collect::<Vec<_>>()})
}

/// The app's own actions, which belong to no card (create_actions).
fn own_actions() -> Vec<ActionSpec> {
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

fn run_own(host: &mut dyn Host, id: &str, p: &Value) -> Option<String> {
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

/// Every action by its id, the app's own first, then the cards' in their order (start()'s grouping); the plugin
/// that runs each, None for this one.
fn all_actions(host: &dyn Host) -> Vec<(Option<String>, ActionSpec)> {
    let mut out: Vec<(Option<String>, ActionSpec)> = own_actions().into_iter().map(|a| (None, a)).collect();
    for (plugin, action) in host.shortcut_actions() {
        if plugin != "shortcuts" && !out.iter().any(|(_, a)| a.id == action.id) {
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

fn choices(p: &Value) -> Vec<(String, String)> {
    p["choices"].as_array().map(|list| list.iter().filter_map(|c| {
        let value = match &c[0] { Value::String(s) => s.clone(), other => other.to_string() };
        Some((value, c[1].as_str()?.to_string()))
    }).collect()).unwrap_or_default()
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn shown(p: &Value, full: &Map<String, Value>) -> bool {
    match p.get("shown_if").and_then(Value::as_array) {
        Some(cond) => {
            let value = cond.first().and_then(Value::as_str).and_then(|n| full.get(n)).cloned().unwrap_or(Value::Null);
            cond.get(1).and_then(Value::as_array).is_some_and(|vals| vals.contains(&value))
        }
        None => true,
    }
}

/// params with every one of the action's settings filled in (full_params): numbers as numbers.
fn full_params(action: &ActionSpec, params: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for p in &action.params {
        let Some(name) = p["name"].as_str() else { continue };
        let given = params.get(name).cloned().unwrap_or_else(|| p["default"].clone());
        let value = if p["kind"] == "number" { number(&given).map_or(Value::Null, |n| json!(n)) } else { given };
        out.insert(name.into(), value);
    }
    out
}

/// Why --action's settings won't do, or None (_remote_param_problem): a typo must not quietly run the default.
fn param_problem(action: &ActionSpec, params: &Map<String, Value>) -> Option<String> {
    for p in &action.params {
        let Some(name) = p["name"].as_str() else { continue };
        let Some(value) = params.get(name) else { continue };
        let shown_value = match value { Value::String(s) => format!("'{s}'"), other => other.to_string() };
        if p["kind"] == "number" {
            if !number(value).is_some_and(f64::is_finite) {
                return Some(tr("{name} has to be a number, not {value}.").replace("{name}", name).replace("{value}", &shown_value));
            }
        } else {
            let options = choices(p);
            if !options.iter().any(|(v, _)| Value::String(v.clone()) == *value) {
                let listed = if options.is_empty() { tr("nothing right now") } else {
                    options.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>().join(" | ")
                };
                return Some(tr("{name} can be {options}, not {value}.").replace("{name}", name).replace("{options}", &listed)
                    .replace("{value}", &shown_value));
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
            return Some(tr("{action} needs {name}=… for that.").replace("{action}", &action.id).replace("{name}", name));
        }
    }
    None
}

fn fmt_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { format!("{n}") }
}

/// What --list-actions prints (list_text).
fn list_text(actions: &[(Option<String>, ActionSpec)]) -> String {
    let mut lines = Vec::new();
    for (_, a) in actions {
        lines.push(format!("{}  ({}: {})", a.id, a.group, a.label));
        for p in &a.params {
            let name = p["name"].as_str().unwrap_or("");
            if p["kind"] == "number" {
                let (lo, hi) = (p["min"].as_f64().unwrap_or(0.0), p["max"].as_f64().unwrap_or(0.0));
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

/// A `telescope --action` request: {"list": true}, or {"action": id, "params": {name: value}} (handle_remote).
pub fn handle_remote(host: &mut dyn Host, request: &Value) -> Value {
    let actions = all_actions(host);
    if request["list"].as_bool() == Some(true) {
        return json!({"ok": true, "text": list_text(&actions)});
    }
    let wanted = request["action"].as_str().unwrap_or("");
    let Some((plugin, action)) = actions.into_iter().find(|(_, a)| a.id == wanted) else {
        let shown = match &request["action"] { Value::String(s) => format!("'{s}'"), Value::Null => "None".into(), v => v.to_string() };
        return json!({"ok": false, "text": tr("No action called {action}. telescope --list-actions lists them.").replace("{action}", &shown)});
    };
    let params = request["params"].as_object().cloned().unwrap_or_default();
    let mut unknown: Vec<&String> = params.keys().filter(|k| !action.params.iter().any(|p| p["name"] == **k)).collect();
    unknown.sort();
    if let Some(name) = unknown.first() {
        return json!({"ok": false, "text": tr("{action} has no setting {name}.").replace("{action}", &action.id).replace("{name}", &format!("'{name}'"))});
    }
    if let Some(problem) = param_problem(&action, &params) {
        return json!({"ok": false, "text": problem});
    }
    let full = Value::Object(full_params(&action, &params));
    let said = match &plugin {
        None => run_own(host, &action.id, &full),
        Some(owner) => host.run_action(owner, &action.id, &full),
    };
    match said {
        Some(text) => json!({"ok": true, "text": text}),
        None => json!({"ok": false, "text": tr("{action} can't be used right now (no phone, or the control is off).").replace("{action}", &action.label)}),
    }
}

impl Plugin for ShortcutsPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "shortcuts".into(),
            title: n_("Keyboard shortcuts").into(),
            about: n_("Keys for Telescope's controls, including ones that work in other apps.").into(),
            when_off: n_("Without it no shortcut does anything and Keyboard shortcuts isn't in Settings (your shortcuts are kept for when it's back).").into(),
            category: "extras".into(),
            icon: "keyboard".into(),
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.remote.0.lock().unwrap().poster = host.poster();
    }

    fn shutdown(&mut self, _host: &mut dyn Host) {
        self.remote.0.lock().unwrap().poster = None;
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        own_actions()
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        run_own(host, id, params)
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        if let Event::Posted { token, data, .. } = event {
            if token == REMOTE {
                let answer = handle_remote(host, &data["request"]);
                self.remote.reply(data["id"].as_u64().unwrap_or(0), answer);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::testing::FakeHost;

    #[test]
    fn the_apps_own_actions_start_and_stop_the_stream() {
        let mut host = FakeHost::default();
        let answer = handle_remote(&mut host, &json!({"action": "streaming.start_stop", "params": {}}));
        assert_eq!(answer, json!({"ok": true, "text": "Streaming: starting"}));
        assert!(host.calls.iter().any(|c| c.starts_with("start_stream")), "{:?}", host.calls);
    }

    #[test]
    fn bad_requests_say_what_was_wrong() {
        let mut host = FakeHost::default();
        let text = |h: &mut FakeHost, r: Value| handle_remote(h, &r)["text"].as_str().unwrap().to_string();
        assert_eq!(text(&mut host, json!({"action": "nope"})), "No action called 'nope'. telescope --list-actions lists them.");
        assert_eq!(text(&mut host, json!({"action": "streaming.start_stop", "params": {"speed": "1"}})),
                   "streaming.start_stop has no setting 'speed'.");
        assert_eq!(text(&mut host, json!({"action": "streaming.start_stop", "params": {"mode": "fast"}})),
                   "mode can be toggle | start | stop, not 'fast'.");
        assert_eq!(text(&mut host, json!({"action": "streaming.camera", "params": {"mode": "hold"}})),
                   "mode can be toggle | on | off, not 'hold'.");
    }

    #[test]
    fn hold_is_refused_without_a_key_to_let_go_of() {
        let spec = ActionSpec {
            id: "microphone.mute".into(), label: "Mute".into(), group: "Microphone".into(),
            params: vec![json!({"name": "mode", "label": "Does", "kind": "choice", "default": "toggle",
                                "choices": [["toggle", "Switch"], ["on", "On"], ["off", "Off"], ["hold", "While held"]]})],
            repeats: false, on_release: true, current: None, keywords: String::new(),
        };
        let params = json!({"mode": "hold"}).as_object().unwrap().clone();
        assert_eq!(param_problem(&spec, &params), Some("hold needs a key to let go of; use toggle, on or off with --action.".into()));
    }

    #[test]
    fn numbers_are_checked_and_listed() {
        let spec = ActionSpec {
            id: "camera.iso".into(), label: "ISO".into(), group: "Camera".into(),
            params: vec![json!({"name": "op", "label": "Does", "kind": "choice", "default": "up", "choices": [["up", "Up"], ["set", "Set to"]]}),
                         json!({"name": "value", "label": "To", "kind": "number", "default": null, "min": 50, "max": 3200, "suffix": "", "shown_if": ["op", ["set"]]})],
            repeats: true, on_release: false, current: None, keywords: String::new(),
        };
        let params = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(param_problem(&spec, &params(json!({"op": "set", "value": "abc"}))), Some("value has to be a number, not 'abc'.".into()));
        assert_eq!(param_problem(&spec, &params(json!({"op": "set"}))), Some("camera.iso needs value=… for that.".into()));
        assert_eq!(param_problem(&spec, &params(json!({"op": "up"}))), None);
        assert_eq!(full_params(&spec, &params(json!({"op": "set", "value": "400"})))["value"], json!(400.0));
        let text = list_text(&[(Some("camera_control".into()), spec)]);
        assert_eq!(text, "camera.iso  (Camera: ISO)\n    op=up | set  (default up)\n    value=<number 50 to 3200>");
    }

    #[test]
    fn a_request_from_another_thread_is_answered_through_the_plugin() {
        let remote = RemoteActions::default();
        assert_eq!(remote.handle(json!({"list": true}))["ok"], json!(false));
        let mut plugin = ShortcutsPlugin::new(remote.clone());
        let mut host = FakeHost::default();
        plugin.setup(&mut host);
        let worker = std::thread::spawn({
            let remote = remote.clone();
            move || remote.handle(json!({"action": "window.show_hide"}))
        });
        // The fake host's poster queues what's posted; deliver it as the window would
        let posted = loop {
            if let Some(p) = host.posted.lock().unwrap().pop() {
                break p;
            }
            std::thread::yield_now();
        };
        plugin.on_event(&mut host, &Event::Posted { owner: "shortcuts".into(), token: posted.0, data: posted.1 });
        assert_eq!(worker.join().unwrap()["ok"], json!(true));
    }
}
