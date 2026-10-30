//! Keyboard shortcuts (plugins/shortcuts.py): keys bound to any plugin's action, inside Telescope or everywhere.
//!
//! Bindings are global settings, each running an action with its own settings, so "Exposure +1" and "Exposure -2" are
//! two bindings of one action. A global one is also claimed from the system (hotkeys.rs); one the system wouldn't give
//! still works inside Telescope, where the shell hands every key to Plugin::on_key first. `telescope --action ID
//! name=value` runs an action in the running copy too (handle_remote).
//!
//! An action's params are JSON: {"name", "label", "kind": "choice", "default", "choices": [[value, label], ...],
//! "shown_if": [name, [values]], "dynamic": true for a list that changes (a saved choice that's gone is kept)} or
//! {"name", "label", "kind": "number", "default", "min", "max", "decimals", "suffix"}.

use crate::hotkeys::{self, Hotkey, HotkeyBackend, Signals, HOTKEY};
use crate::keys;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::event::Event;
use telescope_core::host::{Host, Poster};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, KeyInput, SettingsSection, ShortcutEntry, SlintSource, UiEvent, UiSpec};

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

pub const MAX_BINDINGS: usize = 200;
/// A global and an in-app press of one binding this close together are one key press.
const DOUBLED: Duration = Duration::from_millis(150);

/// A key bound to an action: keys in Qt's portable text, global when it should also work in other apps. The id stays
/// the same across restarts, so the system remembers the key a user picked for it (Plasma keys them by it).
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub id: String,
    pub keys: String,
    pub action: String,
    pub params: Map<String, Value>,
    pub global: bool,
}

impl Binding {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "keys": self.keys, "action": self.action, "params": self.params, "global": self.global})
    }
}

pub fn new_binding_id() -> String {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    format!("b{:010x}", h.finish() & 0xff_ffff_ffff)
}

fn clean_params(raw: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in raw.as_object().into_iter().flatten() {
        let fits = match v {
            Value::Bool(_) | Value::Number(_) => true,
            Value::String(s) => s.chars().count() <= 200,
            _ => false,
        };
        if k.chars().count() <= 40 && fits {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// Well-formed bindings in order: real keys, an action id, a unique id (a missing or repeated one gets a new one, so
/// the system's memory of it can't cross over to another binding).
pub fn clean_bindings(raw: &Value) -> Vec<Binding> {
    let mut out: Vec<Binding> = Vec::new();
    for b in raw.as_array().into_iter().flatten() {
        if !b.is_object() || out.len() >= MAX_BINDINGS {
            continue;
        }
        let keys = b["keys"].as_str().map(keys::normalize_keys).unwrap_or_default();
        let action = b["action"].as_str().unwrap_or("");
        if keys.is_empty() || action.is_empty() || action.chars().count() > 80 {
            continue;
        }
        let id = match b["id"].as_str() {
            Some(id) if !id.is_empty() && id.chars().count() <= 24 && id.chars().all(char::is_alphanumeric)
                && !out.iter().any(|o| o.id == id) => id.to_string(),
            _ => loop {
                let id = new_binding_id();
                if !out.iter().any(|o| o.id == id) {
                    break id;
                }
            },
        };
        out.push(Binding { id, keys, action: action.into(), params: clean_params(&b["params"]), global: b["global"] == true });
    }
    out
}

/// Ids of the bindings whose keys an earlier binding already uses (they don't do anything).
pub fn clashes(bindings: &[Binding]) -> HashSet<String> {
    let mut seen = HashSet::new();
    bindings.iter().filter(|b| !seen.insert(b.keys.clone())).map(|b| b.id.clone()).collect()
}

/// The id Telescope registers with the desktop portal: its menu entry's name (a dev profile's own).
pub fn portal_app_id() -> String {
    format!("{}{}", crate::autostart::APP_ID, if telescope_core::config::dev_profile().is_some() { "-dev" } else { "" })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    App,
    Global,
}

type BackendFactory = Box<dyn FnOnce(Signals) -> Option<Box<dyn HotkeyBackend>> + Send>;

pub struct ShortcutsPlugin {
    remote: RemoteActions,
    factory: Option<BackendFactory>,
    poster: Option<Arc<dyn Poster>>,
    bindings: Vec<Binding>,
    notify: bool,
    actions: Vec<(Option<String>, ActionSpec)>,
    backend: Option<Box<dyn HotkeyBackend>>,
    started: bool,
    /// Binding id: where it was pressed, while held.
    held: HashMap<String, Source>,
    /// Binding id: the scan code of the key that pressed it inside Telescope (0 when the system gave none).
    held_scan: HashMap<String, u32>,
    /// The shortcut editor is open: keys are being pressed to pick one.
    editing: bool,
    /// What the system was last handed, so a same list isn't rebound.
    last_global: Option<Vec<Hotkey>>,
    /// Binding id: (source, time) of its last press, to drop a doubled one.
    recent: HashMap<String, (Source, Instant)>,
    editor: Option<Editor>,
    /// Settings asked for the page, so its props are worth sending.
    section_made: bool,
    /// Why the desktop's settings didn't open, in place of the backend's note.
    note_override: Option<String>,
    /// What the controls' right-click menu was last handed.
    menu: Option<Vec<ShortcutEntry>>,
    /// A shortcut a right-click asked for, to edit once Settings is up.
    from_menu: Option<Binding>,
}

impl ShortcutsPlugin {
    pub fn new(remote: RemoteActions) -> ShortcutsPlugin {
        ShortcutsPlugin::with_backend(remote, Box::new(|signals| hotkeys::create_backend(&portal_app_id(), signals)))
    }

    pub fn with_backend(remote: RemoteActions, factory: BackendFactory) -> ShortcutsPlugin {
        ShortcutsPlugin {
            remote, factory: Some(factory), poster: None, bindings: Vec::new(), notify: false, actions: Vec::new(),
            backend: None, started: false, held: HashMap::new(), held_scan: HashMap::new(), editing: false, last_global: None, recent: HashMap::new(),
            editor: None, section_made: false, note_override: None, menu: None, from_menu: None,
        }
    }

    fn start(&mut self, host: &mut dyn Host) {
        if self.started {
            return;
        }
        self.started = true;
        self.actions = all_actions(host);
        if let (Some(factory), Some(poster)) = (self.factory.take(), self.poster.clone()) {
            self.backend = factory(Signals(poster));
        }
        self.apply(host);
    }

    fn action(&self, id: &str) -> Option<&(Option<String>, ActionSpec)> {
        self.actions.iter().find(|(_, a)| a.id == id)
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn set_bindings(&mut self, host: &mut dyn Host, bindings: Vec<Binding>) {
        let raw = Value::Array(bindings.iter().map(Binding::to_json).collect());
        self.bindings = clean_bindings(&raw);
        host.schedule_save();
        self.apply(host);
    }

    /// Hand the global bindings to the system; the rest are matched as keys come in.
    fn apply(&mut self, host: &mut dyn Host) {
        for id in self.held.keys().cloned().collect::<Vec<_>>() {
            self.release(host, &id);
        }
        self.apply_backend();
        self.refresh(host);
    }

    fn apply_backend(&mut self) {
        if self.started && self.backend.is_some() {
            let skip = clashes(&self.bindings);
            let hotkeys: Vec<Hotkey> = self.bindings.iter()
                .filter(|b| b.global && !skip.contains(&b.id) && self.action(&b.action).is_some())
                .map(|b| Hotkey { id: b.id.clone(), keys: b.keys.clone(), description: self.describe(b) })
                .collect();
            // Rebinding can ask the user to confirm them all again (Plasma)
            if self.last_global.as_ref() != Some(&hotkeys) {
                self.backend.as_mut().unwrap().apply(&hotkeys);
                self.last_global = Some(hotkeys);
            }
        }
    }

    /// What a binding does, for the list: "Streaming: start or stop".
    pub fn describe(&self, binding: &Binding) -> String {
        match self.action(&binding.action) {
            Some((_, action)) => summary(action, &clean_full_params(action, &binding.params)),
            None => binding.action.clone(),
        }
    }

    /// (text, ok) for where this binding works, or why it doesn't.
    pub fn where_works(&self, binding: &Binding) -> (String, bool) {
        if clashes(&self.bindings).contains(&binding.id) {
            return (tr("Key used by a shortcut above"), false);
        }
        if self.action(&binding.action).is_none() {
            return (tr("Not in this version of Telescope"), false);
        }
        if !binding.global {
            return (tr("In Telescope"), true);
        }
        let Some(backend) = &self.backend else { return (tr("In Telescope (no global shortcuts here)"), false) };
        match backend.status(&binding.id) {
            None => (tr("Everywhere (setting up)"), true),
            Some(st) if st.working => (st.text, true),
            Some(st) if st.reaches_app => (tr("In Telescope: {status}").replace("{status}", &st.text), false),
            Some(st) => (st.text, false),
        }
    }

    /// The binding keys pressed inside Telescope run: the first with those keys, if this version has its action. A
    /// global one too: a system that took the key never lets it reach us, and the portal may have bound another key.
    fn binding_for_keys(&self, keys: &str) -> Option<&Binding> {
        let b = self.bindings.iter().find(|b| b.keys == keys)?;
        self.action(&b.action).is_some().then_some(b)
    }

    fn press(&mut self, host: &mut dyn Host, id: &str, source: Source, repeat: bool, scan: u32) -> Option<String> {
        let binding = self.bindings.iter().find(|b| b.id == id)?.clone();
        if source == Source::Global && self.editing {
            // The user is pressing keys to pick one, not to run them
            return None;
        }
        let (owner, action) = self.action(&binding.action)?.clone();
        let params = clean_full_params(&action, &binding.params);
        if repeat && !repeats(&action, &params) {
            return None;
        }
        let now = Instant::now();
        if let Some((last, at)) = self.recent.get(id) {
            if !repeat && *last != source && now.duration_since(*at) < DOUBLED {
                // The same key press, delivered both by the system and inside Telescope
                return None;
            }
        }
        self.recent.insert(id.into(), (source, now));
        if !repeat {
            self.held.insert(id.into(), source);
            self.held_scan.insert(id.into(), scan);
        }
        let params = Value::Object(params);
        let said = match &owner {
            None => run_own(host, &action.id, &params),
            Some(plugin) => host.run_action(plugin, &action.id, &params),
        };
        let Some(said) = said else {
            if !repeat {
                // Nothing happened, so there's nothing to undo on release
                self.held.remove(id);
            }
            return None;
        };
        // Showing the window says it all, and Windows only activates it a moment later
        if source != Source::App && self.notify && !host.window_active() && said != tr(WINDOW_SHOWN) {
            host.send_notification("Telescope", &said, false);
        }
        Some(said)
    }

    fn is_held(&self, id: &str) -> bool {
        self.held.contains_key(id)
    }

    fn release(&mut self, host: &mut dyn Host, id: &str) {
        self.held_scan.remove(id);
        if self.held.remove(id).is_none() {
            return;
        }
        let Some(binding) = self.bindings.iter().find(|b| b.id == id) else { return };
        let Some((Some(plugin), action)) = self.action(&binding.action).cloned() else { return };
        if action.on_release {
            host.release_action(&plugin, &action.id, &Value::Object(clean_full_params(&action, &binding.params)));
        }
    }

    /// Keys held inside Telescope come up: the ones on this key (whatever the modifiers) or with this scan code (a shifted key can come
    /// up as another key once Shift is let go first), or all of them (focus moved away).
    fn release_app_keys(&mut self, host: &mut dyn Host, key: Option<&str>, scan: u32) {
        let ids: Vec<String> = self.held.iter().filter(|(_, s)| **s == Source::App).map(|(id, _)| id.clone()).collect();
        for id in ids {
            let on_key = |b: &Binding| key.is_none_or(|k| keys::parse(&b.keys).is_some_and(|c| c.key.eq_ignore_ascii_case(k)));
            let same_scan = scan != 0 && self.held_scan.get(&id) == Some(&scan);
            if self.bindings.iter().find(|b| b.id == id).is_none_or(on_key) || same_scan {
                self.release(host, &id);
            }
        }
    }

    /// The key filter (_KeyFilter): runs the binding a key in Telescope's window belongs to; true keeps the key from
    /// the control with focus.
    fn key(&mut self, host: &mut dyn Host, key: &KeyInput) -> bool {
        let keys = keys::keys_from_slint(&key.text, key.ctrl, key.alt, key.shift, key.meta);
        if key.released {
            if !key.repeat {
                // A key with no name of ours (Shift on its own) lets go of nothing by its name
                let name = keys::parse(&keys).map(|c| c.key);
                if name.is_some() || key.scan != 0 {
                    self.release_app_keys(host, Some(name.as_deref().unwrap_or("")), key.scan);
                }
            }
            return false;
        }
        if keys.is_empty() || key.modal || self.editing {
            // A dialog is asking something (the shortcut editor among them), or a menu is open
            return false;
        }
        let Some(binding) = self.binding_for_keys(&keys) else { return false };
        if key.typing && keys::edits_text(&binding.keys) {
            // A plain key belongs to the text box
            return false;
        }
        let id = binding.id.clone();
        let said = self.press(host, &id, Source::App, key.repeat, key.scan);
        // A binding that couldn't run (its control greyed out) leaves the key to the control with focus
        said.is_some() || self.is_held(&id)
    }

    /// A `telescope --action` request, which also notifies from the background like a global key.
    fn run_remote(&mut self, host: &mut dyn Host, request: &Value) -> Value {
        let answer = handle_remote(host, request);
        if request["list"] != true && answer["ok"] == true && self.notify && !host.window_active() {
            host.send_notification("Telescope", answer["text"].as_str().unwrap_or(""), false);
        }
        answer
    }

    fn on_hotkey(&mut self, host: &mut dyn Host, data: &Value) {
        let id = data["id"].as_str().unwrap_or("");
        match data["kind"].as_str() {
            Some("pressed") => {
                self.press(host, id, Source::Global, false, 0);
            }
            Some("released") => self.release(host, id),
            Some("changed") => self.refresh(host),
            _ => {}
        }
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

/// A param's value as it takes it, or its default when it can't be (Param.clean).
fn clean_value(p: &Value, given: Option<&Value>) -> Value {
    let default = p["default"].clone();
    let Some(given) = given else { return default };
    if p["kind"] == "number" {
        let Some(n) = (match given { Value::Bool(_) => None, v => number(v) }).filter(|n| n.is_finite()) else { return default };
        let lo = p["min"].as_f64().unwrap_or(f64::NEG_INFINITY);
        let hi = p["max"].as_f64().unwrap_or(f64::INFINITY).max(lo);
        let n = n.clamp(lo, hi);
        let n = match p["decimals"].as_u64() {
            Some(d) => (n * 10f64.powi(d as i32)).round() / 10f64.powi(d as i32),
            None => n,
        };
        return json!(n);
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
fn full_params(action: &ActionSpec, params: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for p in &action.params {
        let Some(name) = p["name"].as_str() else { continue };
        out.insert(name.into(), clean_value(p, params.get(name)));
    }
    out
}

fn clean_full_params(action: &ActionSpec, params: &Map<String, Value>) -> Map<String, Value> {
    full_params(action, params)
}

/// Whether holding the key repeats it: a step does, a "set to" doesn't.
fn repeats(action: &ActionSpec, params: &Map<String, Value>) -> bool {
    action.repeats && !matches!(params.get("op").and_then(Value::as_str), Some("set" | "reset"))
}

/// What a binding with these params does: "Streaming: start or stop", "ISO: raise by 1 stops".
fn summary(action: &ActionSpec, params: &Map<String, Value>) -> String {
    let mut parts = Vec::new();
    for p in &action.params {
        let Some(name) = p["name"].as_str() else { continue };
        if !shown(p, params) {
            continue;
        }
        let value = params.get(name).cloned().unwrap_or(Value::Null);
        let text = if p["kind"] == "number" {
            match value.as_f64() {
                Some(n) => {
                    let n = match p["decimals"].as_u64() {
                        Some(d) => format!("{n:.*}", d as usize),
                        None => fmt_number(n),
                    };
                    format!("{n}{}", p["suffix"].as_str().unwrap_or(""))
                }
                None => "…".into(),
            }
        } else {
            let v = match &value { Value::String(s) => s.clone(), Value::Null => String::new(), v => v.to_string() };
            let label = choices(p).into_iter().find(|(c, _)| *c == v).map(|(_, l)| l);
            match label {
                // The first choice is what the key does ("Increase by"): it reads on from the label in lower case
                Some(l) if parts.is_empty() => l.to_lowercase(),
                Some(l) => l,
                None if v.is_empty() => "…".into(),
                None => v,
            }
        };
        parts.push(text);
    }
    if parts.is_empty() { action.label.clone() } else { format!("{}: {}", action.label, parts.join(" ")) }
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

const UI: &str = include_str!("../ui/shortcuts.slint");
const SECTION: &str = "shortcuts";
const COPIED: &str = "copied";
// A right-click asked for a shortcut: the editor opens this long after Settings, once its page is built
const FROM_MENU: &str = "from_menu";
const FROM_MENU_MS: u64 = 150;

/// The command line that runs this action in the running Telescope (for a desktop's own shortcut settings).
pub fn action_command(action: &str, params: &Map<String, Value>) -> Vec<String> {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "telescope-rs".into());
    let mut out = vec![exe, "--action".into(), action.into()];
    for (k, v) in params {
        let v = match v {
            Value::Null => continue,
            Value::String(s) => s.clone(),
            Value::Bool(b) => if *b { "True".into() } else { "False".into() },
            Value::Number(n) => n.as_f64().map(fmt_number).unwrap_or_else(|| n.to_string()),
            other => other.to_string(),
        };
        out.push(format!("{k}={v}"));
    }
    out
}

/// One line for a shell (shlex.join) or for Windows (list2cmdline).
pub fn command_text(command: &[String]) -> String {
    command.iter().map(|w| if cfg!(windows) { windows_quote(w) } else { shell_quote(w) }).collect::<Vec<_>>().join(" ")
}

fn shell_quote(word: &str) -> String {
    if !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./_-".contains(c)) {
        return word.into();
    }
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

fn windows_quote(word: &str) -> String {
    if !word.is_empty() && !word.contains([' ', '\t', '"']) {
        return word.into();
    }
    let mut out = String::from('"');
    let mut slashes = 0;
    for c in word.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

/// Whether every word of the query starts a word of the action's name, keywords, card, id or choices.
fn matches(action: &ActionSpec, query: &str) -> bool {
    let mut doc = format!("{} {} {} {}", action.label, action.keywords, action.group, action.id);
    for p in action.params.iter().filter(|p| p["kind"] != "number" && p["dynamic"] != true) {
        for (_, label) in choices(p) {
            doc.push(' ');
            doc.push_str(&label);
        }
    }
    let doc = doc.to_lowercase();
    let words: Vec<&str> = doc.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    query.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|q| !q.is_empty())
        .all(|q| words.iter().any(|w| w.starts_with(q)))
}

/// The shortcut being added or changed.
struct Editor {
    id: String,
    query: String,
    /// Kept while a search hides it.
    action: Option<String>,
    params: Map<String, Value>,
    keys: String,
    global: bool,
    copied: bool,
}

impl ShortcutsPlugin {
    fn shown_actions(&self, query: &str) -> Vec<&ActionSpec> {
        self.actions.iter().map(|(_, a)| a).filter(|a| matches(a, query)).collect()
    }

    fn open_editor(&mut self, host: &mut dyn Host, binding: Binding) {
        let action = self.action(&binding.action).map(|(_, a)| a.clone());
        let params = action.as_ref().map_or_else(|| binding.params.clone(), |a| self.start_params(a, &binding.params));
        let action = (!binding.action.is_empty()).then(|| binding.action.clone());
        self.editor = Some(Editor { id: binding.id, query: String::new(), action, params, keys: binding.keys, global: binding.global, copied: false });
        // A new shortcut starts on the first control
        if self.editor.as_ref().is_some_and(|e| e.action.is_none()) {
            if let Some(first) = self.actions.first().map(|(_, a)| a.id.clone()) {
                self.pick_action(&first);
            }
        }
        // Global keys let go where that's cheap, so pressing one records it instead of running it
        self.editing = true;
        if let Some(b) = self.backend.as_mut() {
            b.suspend(true);
        }
        self.refresh(host);
    }

    fn close_editor(&mut self, host: &mut dyn Host) {
        self.editor = None;
        self.editing = false;
        if let Some(b) = self.backend.as_mut() {
            b.suspend(false);
        }
        self.refresh(host);
    }

    /// A binding's params as the editor starts them: cleaned, and a "Set to" with no value at where the control is.
    fn start_params(&self, action: &ActionSpec, given: &Map<String, Value>) -> Map<String, Value> {
        let mut params = full_params(action, given);
        for p in &action.params {
            let Some(name) = p["name"].as_str() else { continue };
            if p["kind"] == "number" && params.get(name).is_none_or(Value::is_null) {
                let start = action.current.or_else(|| p["min"].as_f64()).unwrap_or(0.0);
                params.insert(name.into(), clean_value(p, Some(&json!(start))));
            }
        }
        params
    }

    fn pick_action(&mut self, id: &str) {
        let Some((_, action)) = self.action(id).cloned() else { return };
        let params = self.start_params(&action, &Map::new());
        if let Some(e) = self.editor.as_mut() {
            if e.action.as_deref() != Some(id) {
                e.action = Some(id.into());
                e.params = params;
            }
        }
    }

    /// (problem, blocking) for the editor (_check).
    fn editor_problem(&self, e: &Editor, listed: usize) -> (String, bool) {
        let meta = keys::META_NAME;
        if e.action.is_none() || (listed == 0 && !e.query.is_empty() && !self.action_listed(e)) {
            return (if listed == 0 { tr("No control matches that search.") } else { tr("Pick a control.") }, true);
        }
        if e.keys.is_empty() {
            return (tr("Press a key, with Ctrl, Alt, Shift or {meta} if you like.").replace("{meta}", meta), true);
        }
        if let Some(other) = self.bindings.iter().find(|b| b.keys == e.keys && b.id != e.id) {
            let text = tr("{keys} already does \"{action}\". Pick another key.")
                .replace("{keys}", &keys::native_keys(&e.keys)).replace("{action}", &self.describe(other));
            return (text, true);
        }
        if e.global && keys::edits_text(&e.keys) {
            let text = tr("Everywhere, {keys} would stop typing it in other apps. Adding Ctrl, Alt or {meta} is safer.")
                .replace("{keys}", &keys::native_keys(&e.keys)).replace("{meta}", meta);
            return (text, false);
        }
        (String::new(), true)
    }

    fn action_listed(&self, e: &Editor) -> bool {
        e.action.as_deref().is_some_and(|id| self.shown_actions(&e.query).iter().any(|a| a.id == id))
    }

    fn editor_props(&self) -> Value {
        let Some(e) = &self.editor else { return json!({"editor-open": false}) };
        let listed = self.shown_actions(&e.query);
        let mut controls: Vec<String> = listed.iter().map(|a| format!("{}: {}", a.group, a.label)).collect();
        let mut control = e.action.as_deref().and_then(|id| listed.iter().position(|a| a.id == id)).map_or(-1, |i| i as i64);
        if let (Some(id), true, -1) = (e.action.as_deref(), e.query.is_empty(), control) {
            // An action this version doesn't have: kept
            controls.push(tr("Unknown ({action})").replace("{action}", id));
            control = controls.len() as i64 - 1;
        }
        let action = e.action.as_deref().and_then(|id| self.action(id)).map(|(_, a)| a);
        let params: Vec<Value> = action.map(|a| a.params.iter().filter_map(|p| {
            let name = p["name"].as_str()?;
            let value = e.params.get(name).cloned().unwrap_or(Value::Null);
            if p["kind"] == "number" {
                let text = match (value.as_f64(), p["decimals"].as_u64()) {
                    (Some(n), Some(d)) => format!("{n:.*}", d as usize),
                    (Some(n), None) => fmt_number(n),
                    _ => String::new(),
                };
                return Some(json!({"label": p["label"], "number": true, "items": [], "current": 0, "text": text, "shown": shown(p, &e.params)}));
            }
            let mut options = choices(p);
            let v = match &value { Value::String(s) => s.clone(), Value::Null => String::new(), v => v.to_string() };
            let mut current = options.iter().position(|(c, _)| *c == v);
            if current.is_none() && !v.is_empty() {
                // A preset that's gone, say: kept, so Save keeps it
                options.push((v.clone(), tr("{value} (not there now)").replace("{value}", &v)));
                current = Some(options.len() - 1);
            }
            let items: Vec<String> = options.into_iter().map(|(_, l)| l).collect();
            Some(json!({"label": p["label"], "number": false, "items": items, "current": current.unwrap_or(0), "text": "", "shown": shown(p, &e.params)}))
        }).collect()).unwrap_or_default();
        let (problem, blocking) = self.editor_problem(e, listed.len());
        let hold = e.params.get("mode").and_then(Value::as_str) == Some("hold");
        json!({
            "editor-open": true,
            "query": e.query,
            "controls": controls,
            "control": control,
            "params": params,
            "keys-text": keys::native_keys(&e.keys),
            "global": e.global,
            "problem": problem,
            "save-enabled": problem.is_empty() || !blocking,
            "copy-enabled": !hold,
            "copy-text": if e.copied { tr("Copied") } else { tr("Copy command") },
            "copy-tip": if hold { tr("A command can't hold a key down; pick another choice to copy one") }
                        else { tr("A command line that does the same, for your desktop's own keyboard settings") },
        })
    }

    fn section_props(&self) -> Value {
        let rows: Vec<Value> = self.bindings.iter().map(|b| {
            let (where_text, ok) = self.where_works(b);
            let does = self.describe(b);
            json!({"keys": keys::native_keys(&b.keys), "does": does, "where": where_text, "ok": ok,
                   "copy-ok": b.params.get("mode").and_then(Value::as_str) != Some("hold"),
                   "edit-label": tr("Edit {shortcut}").replace("{shortcut}", &does),
                   "remove-label": tr("Remove {shortcut}").replace("{shortcut}", &does)})
        }).collect();
        let note = match (&self.note_override, &self.backend) {
            (Some(text), _) => text.clone(),
            (None, Some(backend)) => backend.note(),
            (None, None) => tr("Global shortcuts aren't available on this desktop, so these work inside Telescope. For keys that work everywhere, bind a shortcut's Copy command line in your desktop's keyboard settings."),
        };
        let system_shown = cfg!(target_os = "linux") && self.backend.as_ref().is_none_or(|b| b.system_picks_keys());
        let mut props = json!({"rows": rows, "note-text": note, "system-shown": system_shown, "notify": self.notify});
        if let (Value::Object(p), Value::Object(e)) = (&mut props, self.editor_props()) {
            p.extend(e);
        }
        props
    }

    fn refresh(&mut self, host: &mut dyn Host) {
        if self.section_made {
            host.set_ui_props(SECTION, &self.section_props());
        }
        let entries = self.started.then(|| self.menu_entries());
        if entries.is_some() && entries != self.menu {
            host.set_shortcut_menu(entries.clone());
            self.menu = entries;
        }
    }

    /// The saved shortcuts as a control's right-click lists them: "Ctrl+Alt+M: Mute".
    fn menu_entries(&self) -> Vec<ShortcutEntry> {
        self.bindings.iter().map(|b| ShortcutEntry {
            id: b.id.clone(), action: b.action.clone(), text: format!("{}: {}", keys::native_keys(&b.keys), self.describe(b)),
        }).collect()
    }

    /// A right-click menu's item: a new shortcut for an action ("add") or a saved one ("edit"), in Settings' editor.
    fn on_menu(&mut self, host: &mut dyn Host, callback: &str, args: &[Value]) {
        let id = args.first().and_then(Value::as_str).unwrap_or_default();
        let binding = match callback {
            "add" if self.action(id).is_some() => {
                Some(Binding { id: new_binding_id(), keys: String::new(), action: id.into(), params: Map::new(), global: true })
            }
            "edit" => self.bindings.iter().find(|b| b.id == id).cloned(),
            _ => None,
        };
        let Some(binding) = binding else { return };
        host.open_settings("shortcuts");
        // The editor opens on a change, which a page Settings is only now building wouldn't see
        self.from_menu = Some(binding);
        host.schedule(FROM_MENU_MS, FROM_MENU);
    }

    fn copy_command(&self, host: &mut dyn Host, action: &str, params: &Map<String, Value>) {
        host.copy_text(&command_text(&action_command(action, params)));
    }

    /// The desktop's own shortcut settings: the portal's dialog for these, or Plasma's Shortcuts page.
    fn open_system_settings(&mut self, host: &mut dyn Host) -> bool {
        if let Some(b) = self.backend.as_mut() {
            if b.configure() {
                return true;
            }
            if b.can_rebind() {
                // Some weren't confirmed: the desktop asks again
                self.last_global = None;
                self.apply(host);
                return true;
            }
        }
        if cfg!(target_os = "linux") {
            for cmd in [["systemsettings", "kcm_keys"], ["kcmshell6", "kcm_keys"], ["kcmshell5", "kcm_keys"]] {
                let started = std::process::Command::new(cmd[0]).arg(cmd[1])
                    .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
                if started.is_ok() {
                    return true;
                }
            }
        }
        false
    }

    fn on_section(&mut self, host: &mut dyn Host, callback: &str, args: &[Value]) {
        let int = |i: usize| args.get(i).and_then(Value::as_i64).unwrap_or(-1);
        let text = |i: usize| args.get(i).and_then(Value::as_str).unwrap_or("").to_string();
        let flag = |i: usize| args.get(i).and_then(Value::as_bool).unwrap_or(false);
        let binding_at = |s: &Self, i: i64| usize::try_from(i).ok().and_then(|i| s.bindings.get(i)).cloned();
        match callback {
            "add" => {
                let blank = Binding { id: new_binding_id(), keys: String::new(), action: String::new(), params: Map::new(), global: true };
                self.open_editor(host, blank);
            }
            "edit" => {
                if let Some(b) = binding_at(self, int(0)) {
                    self.open_editor(host, b);
                }
            }
            "remove" => {
                if let Some(b) = binding_at(self, int(0)) {
                    let rest = self.bindings.iter().filter(|o| o.id != b.id).cloned().collect();
                    self.set_bindings(host, rest);
                }
            }
            "copy-line" => {
                if let Some(b) = binding_at(self, int(0)).filter(|b| b.params.get("mode").and_then(Value::as_str) != Some("hold")) {
                    self.copy_command(host, &b.action, &b.params);
                }
            }
            "open-system" => {
                self.note_override = (!self.open_system_settings(host))
                    .then(|| tr("Couldn't open your desktop's shortcut settings. Look for Shortcuts in its System Settings."));
                self.refresh(host);
            }
            "notify-toggled" => {
                self.notify = flag(0);
                host.schedule_save();
            }
            "search" => {
                let query = text(0);
                let first = self.shown_actions(&query).first().map(|a| a.id.clone());
                let Some(e) = self.editor.as_mut() else { return };
                e.query = query;
                // A search that found nothing forgets no pick; one that hides the pick moves it to the first match
                let keep = e.action.clone();
                if !self.editor.as_ref().is_some_and(|e| self.action_listed(e)) || keep.is_none() {
                    if let Some(first) = first {
                        self.pick_action(&first);
                    }
                }
                self.refresh(host);
            }
            "control-picked" => {
                let query = self.editor.as_ref().map(|e| e.query.clone()).unwrap_or_default();
                let id = usize::try_from(int(0)).ok().and_then(|i| self.shown_actions(&query).get(i).map(|a| a.id.clone()));
                if let Some(id) = id {
                    self.pick_action(&id);
                }
                self.refresh(host);
            }
            "param-picked" | "param-edited" => {
                let Some(e) = self.editor.as_ref() else { return };
                let Some((_, action)) = e.action.as_deref().and_then(|id| self.action(id)).cloned() else { return };
                let Some(p) = usize::try_from(int(0)).ok().and_then(|i| action.params.get(i)) else { return };
                let Some(name) = p["name"].as_str() else { return };
                let value = if callback == "param-picked" {
                    let current = e.params.get(name).and_then(Value::as_str).unwrap_or("").to_string();
                    let mut options: Vec<String> = choices(p).into_iter().map(|(v, _)| v).collect();
                    if !current.is_empty() && !options.contains(&current) {
                        options.push(current);
                    }
                    match usize::try_from(int(1)).ok().and_then(|i| options.get(i)) {
                        Some(v) => json!(v),
                        None => return,
                    }
                } else {
                    let Some(n) = number(&json!(text(1))) else { return };
                    clean_value(p, Some(&json!(n)))
                };
                let name = name.to_string();
                if let Some(e) = self.editor.as_mut() {
                    e.params.insert(name, value);
                }
                // A number being typed keeps its box as it is; a pick can show or hide other settings
                if callback == "param-picked" {
                    self.refresh(host);
                }
            }
            "key-pressed" => {
                let keys = keys::keys_from_slint(&text(0), flag(1), flag(2), flag(3), flag(4));
                // A bare modifier waits for the key that comes with it
                if keys.is_empty() {
                    return;
                }
                if let Some(e) = self.editor.as_mut() {
                    e.keys = keys;
                }
                self.refresh(host);
            }
            "key-cleared" => {
                if let Some(e) = self.editor.as_mut() {
                    e.keys.clear();
                }
                self.refresh(host);
            }
            "global-toggled" => {
                if let Some(e) = self.editor.as_mut() {
                    e.global = flag(0);
                }
                self.refresh(host);
            }
            "copy-command" => {
                let Some(e) = self.editor.as_ref() else { return };
                let (Some(action), false) = (e.action.clone(), e.params.get("mode").and_then(Value::as_str) == Some("hold")) else { return };
                let params = e.params.clone();
                self.copy_command(host, &action, &params);
                if let Some(e) = self.editor.as_mut() {
                    e.copied = true;
                }
                host.schedule(1500, COPIED);
                self.refresh(host);
            }
            "save" => {
                let Some(e) = self.editor.as_ref() else { return };
                let (problem, blocking) = self.editor_problem(e, self.shown_actions(&e.query).len());
                let Some(action) = e.action.clone() else { return };
                if !problem.is_empty() && blocking {
                    return;
                }
                let binding = Binding { id: e.id.clone(), keys: e.keys.clone(), action, params: e.params.clone(), global: e.global };
                let mut bindings = self.bindings.clone();
                match bindings.iter_mut().find(|b| b.id == binding.id) {
                    Some(old) => *old = binding,
                    None => bindings.push(binding),
                }
                self.close_editor(host);
                self.set_bindings(host, bindings);
            }
            "cancel" => self.close_editor(host),
            _ => {}
        }
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
        self.poster = host.poster();
        self.remote.0.lock().unwrap().poster = self.poster.clone();
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.start(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        host.set_shortcut_menu(None);
        self.remote.0.lock().unwrap().poster = None;
        if let Some(backend) = self.backend.as_mut() {
            backend.stop();
        }
    }

    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        self.section_made = true;
        vec![SettingsSection {
            id: SECTION.into(),
            tab: "shortcuts".into(),
            ui: UiSpec { source: SlintSource::Text(UI.into()), component: "ShortcutsSettings".into(), props: self.section_props() },
            order: 0,
            tab_title: String::new(),
            keywords: [
                tr("hotkey hotkeys keybind keybinding key bindings keys push to talk mute add remove"),
                tr("global system wide hotkeys background other apps"),
                tr("global hotkeys portal kde plasma gnome desktop keys rebind"),
                tr("notification notify toast confirmation popup"),
            ].join(" "),
            footer: None,
        }]
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui == SECTION {
            self.on_section(host, &event.callback, &event.args);
        } else if event.ui == "shortcut-menu" {
            self.on_menu(host, &event.callback, &event.args);
        }
    }

    fn on_key(&mut self, host: &mut dyn Host, key: &KeyInput) -> bool {
        self.key(host, key)
    }

    fn on_keys_lost(&mut self, host: &mut dyn Host) {
        self.release_app_keys(host, None, 0);
    }

    fn get_config(&self) -> Value {
        json!({"bindings": self.bindings.iter().map(Binding::to_json).collect::<Vec<_>>(), "notify": self.notify})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), telescope_core::plugin::ConfigRejected> {
        self.bindings = clean_bindings(&cfg["bindings"]);
        self.notify = cfg["notify"] == true;
        if self.started {
            self.held.clear();
            self.held_scan.clear();
            self.apply_backend();
        }
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        if self.bindings.is_empty() {
            return vec![("Keyboard shortcuts".into(), "none".into())];
        }
        let everywhere = self.bindings.iter().filter(|b| b.global).count();
        let via = self.backend.as_ref().map_or("none", |b| b.name());
        vec![("Keyboard shortcuts".into(), format!("{} ({everywhere} everywhere, via {via})", self.bindings.len()))]
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        own_actions()
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        run_own(host, id, params)
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        if let Event::TimerFired { owner, token } = event {
            if owner == "shortcuts" && token == COPIED {
                if let Some(e) = self.editor.as_mut() {
                    e.copied = false;
                }
                self.refresh(host);
            } else if owner == "shortcuts" && token == FROM_MENU {
                if let Some(binding) = self.from_menu.take() {
                    self.open_editor(host, binding);
                }
            }
            return;
        }
        if let Event::Posted { owner, token, data } = event {
            if owner != "shortcuts" {
                return;
            }
            if token == REMOTE {
                let answer = self.run_remote(host, &data["request"]);
                self.remote.reply(data["id"].as_u64().unwrap_or(0), answer);
            } else if token == HOTKEY {
                self.on_hotkey(host, data);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::testing::FakeHost;
    use crate::hotkeys::HotkeyStatus;

    #[derive(Default)]
    struct Seen {
        applied: Vec<Vec<Hotkey>>,
        statuses: HashMap<String, HotkeyStatus>,
        suspended: Vec<bool>,
        rebind: bool,
    }

    struct FakeBackend(Arc<Mutex<Seen>>);

    impl HotkeyBackend for FakeBackend {
        fn apply(&mut self, hotkeys: &[Hotkey]) {
            self.0.lock().unwrap().applied.push(hotkeys.to_vec());
        }
        fn status(&self, id: &str) -> Option<HotkeyStatus> {
            self.0.lock().unwrap().statuses.get(id).cloned()
        }
        fn suspend(&mut self, on: bool) {
            self.0.lock().unwrap().suspended.push(on);
        }
        fn name(&self) -> &'static str {
            "FakeBackend"
        }
        fn can_rebind(&self) -> bool {
            self.0.lock().unwrap().rebind
        }
    }

    fn mute() -> ActionSpec {
        ActionSpec {
            id: "microphone.mute".into(), label: "Mute".into(), group: "Microphone".into(),
            params: vec![json!({"name": "mode", "label": "Does", "kind": "choice", "default": "toggle",
                                "choices": [["toggle", "Switch on or off"], ["on", "Turn on"], ["off", "Turn off"],
                                            ["hold", "Switch while held, back on release"]]})],
            repeats: false, on_release: true, current: None, keywords: String::new(),
        }
    }

    fn zoom() -> ActionSpec {
        ActionSpec {
            id: "transforms.zoom".into(), label: "Zoom".into(), group: "Transforms".into(),
            params: vec![json!({"name": "op", "label": "Does", "kind": "choice", "default": "up",
                                "choices": [["up", "Increase by"], ["down", "Decrease by"], ["set", "Set to"]]}),
                         json!({"name": "amount", "label": "By", "kind": "number", "default": 1, "min": 0, "max": 100,
                                "suffix": "%", "shown_if": ["op", ["up", "down"]]}),
                         json!({"name": "value", "label": "Value", "kind": "number", "default": null, "min": 0, "max": 100,
                                "suffix": "%", "shown_if": ["op", ["set"]]})],
            repeats: true, on_release: false, current: Some(0.0), keywords: String::new(),
        }
    }

    fn torch() -> ActionSpec {
        ActionSpec { id: "camera.torch".into(), label: "Torch".into(), group: "Camera".into(), params: Vec::new(),
                     repeats: false, on_release: false, current: None, keywords: String::new() }
    }

    fn b(id: &str, keys: &str, action: &str, params: Value, global: bool) -> Value {
        json!({"id": id, "keys": keys, "action": action, "params": params, "global": global})
    }

    /// A started plugin with these bindings, a host offering Mute, Torch and Zoom, and what its backend saw.
    fn make(bindings: Value, notify: bool, backend: bool) -> (ShortcutsPlugin, FakeHost, Arc<Mutex<Seen>>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let shared = seen.clone();
        let factory: BackendFactory = if backend {
            Box::new(move |_| Some(Box::new(FakeBackend(shared)) as Box<dyn HotkeyBackend>))
        } else {
            Box::new(|_| None)
        };
        let mut plugin = ShortcutsPlugin::with_backend(RemoteActions::default(), factory);
        let mut host = FakeHost::default();
        host.actions = vec![("microphone".into(), mute()), ("camera_control".into(), torch()), ("transforms".into(), zoom())];
        plugin.set_config(&json!({"bindings": bindings, "notify": notify})).unwrap();
        plugin.setup(&mut host);
        plugin.on_plugins_ready(&mut host);
        host.calls.clear();
        (plugin, host, seen)
    }

    fn key(text: &str, ctrl: bool, alt: bool) -> KeyInput {
        KeyInput { text: text.into(), ctrl, alt, ..Default::default() }
    }

    fn ran(host: &FakeHost) -> Vec<String> {
        host.calls.iter().filter(|c| c.starts_with("run_action") || c.starts_with("release_action")).cloned().collect()
    }

    fn ui(plugin: &mut ShortcutsPlugin, host: &mut FakeHost, callback: &str, args: Value) {
        let args = args.as_array().cloned().unwrap_or_default();
        plugin.on_ui(host, &UiEvent { ui: SECTION.into(), callback: callback.into(), args });
    }

    fn controls(plugin: &ShortcutsPlugin) -> Vec<String> {
        serde_json::from_value(plugin.section_props()["controls"].clone()).unwrap()
    }

    #[test]
    fn editor_builds_a_binding() {
        let (mut plugin, mut host, seen) = make(json!([]), false, true);
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "add", json!([]));
        assert_eq!(seen.lock().unwrap().suspended, [true], "global keys let go while one is being picked");
        let props = plugin.section_props();
        assert_eq!(props["editor-open"], json!(true));
        let mute = controls(&plugin).iter().position(|c| c == "Microphone: Mute").unwrap();
        ui(&mut plugin, &mut host, "control-picked", json!([mute]));
        assert_eq!(plugin.section_props()["problem"], json!("Press a key, with Ctrl, Alt, Shift or Meta if you like."));
        ui(&mut plugin, &mut host, "param-picked", json!([0, 3]));
        assert_eq!(plugin.section_props()["copy-enabled"], json!(false), "a command can't hold a key down");
        ui(&mut plugin, &mut host, "key-pressed", json!(["\u{11}", true, false, false, false]));
        assert_eq!(plugin.section_props()["keys-text"], json!(""), "a bare modifier waits for its key");
        ui(&mut plugin, &mut host, "key-pressed", json!([" ", true, false, false, false]));
        let props = plugin.section_props();
        assert_eq!((props["keys-text"].clone(), props["problem"].clone(), props["save-enabled"].clone()), (json!("Ctrl+Space"), json!(""), json!(true)));
        ui(&mut plugin, &mut host, "save", json!([]));
        let b = &plugin.bindings()[0];
        assert_eq!((b.keys.as_str(), b.action.as_str(), b.global), ("Ctrl+Space", "microphone.mute", true));
        assert_eq!(Value::Object(b.params.clone()), json!({"mode": "hold"}));
        assert_eq!(plugin.section_props()["editor-open"], json!(false));
        assert_eq!(seen.lock().unwrap().suspended, [true, false]);
        assert!(host.calls.iter().any(|c| c.starts_with("set_ui_props shortcuts")));
    }

    #[test]
    fn editor_refuses_a_key_another_shortcut_uses_and_warns_about_typing_keys() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+T", "camera.torch", json!({}), false)]), false, true);
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "add", json!([]));
        ui(&mut plugin, &mut host, "key-pressed", json!(["t", true, false, false, false]));
        let props = plugin.section_props();
        assert_eq!(props["problem"], json!("Ctrl+T already does \"Torch\". Pick another key."));
        assert_eq!(props["save-enabled"], json!(false));
        ui(&mut plugin, &mut host, "key-pressed", json!(["z", false, false, false, false]));
        let props = plugin.section_props();
        assert!(props["problem"].as_str().unwrap().starts_with("Everywhere, Z would stop typing it in other apps."));
        assert_eq!(props["save-enabled"], json!(true), "only a warning");
        ui(&mut plugin, &mut host, "global-toggled", json!([false]));
        assert_eq!(plugin.section_props()["problem"], json!(""));
    }

    #[test]
    fn editor_keeps_an_option_that_is_gone() {
        let preset = ActionSpec {
            id: "presets.apply".into(), label: "Preset".into(), group: "Presets".into(),
            params: vec![json!({"name": "name", "label": "Preset", "kind": "choice", "default": "", "choices": [["Desk", "Desk"]], "dynamic": true})],
            repeats: false, on_release: false, current: None, keywords: String::new(),
        };
        let (mut plugin, mut host, _) = make(json!([]), false, true);
        host.actions.push(("presets".into(), preset));
        plugin.actions = all_actions(&host);
        plugin.set_bindings(&mut host, clean_bindings(&json!([b("a", "F5", "presets.apply", json!({"name": "Gone"}), false)])));
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "edit", json!([0]));
        let params = plugin.section_props()["params"].clone();
        assert_eq!(params[0]["items"], json!(["Desk", "Gone (not there now)"]));
        assert_eq!(params[0]["current"], json!(1));
        ui(&mut plugin, &mut host, "save", json!([]));
        assert_eq!(plugin.bindings()[0].params["name"], json!("Gone"));
    }

    #[test]
    fn editor_search_narrows_the_controls_and_uses_keywords() {
        let (mut plugin, mut host, _) = make(json!([]), false, true);
        plugin.actions[3].1.keywords = "push to talk".into();
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "add", json!([]));
        assert!(controls(&plugin).len() > 3);
        ui(&mut plugin, &mut host, "search", json!(["zo"]));
        assert_eq!(controls(&plugin), ["Transforms: Zoom"]);
        assert_eq!(plugin.section_props()["control"], json!(0), "the only match is picked");
        ui(&mut plugin, &mut host, "search", json!(["talk"]));
        assert_eq!(controls(&plugin), ["Microphone: Mute"]);
        ui(&mut plugin, &mut host, "search", json!(["nothing like it"]));
        let props = plugin.section_props();
        assert_eq!((props["control"].clone(), props["problem"].clone()), (json!(-1), json!("No control matches that search.")));
    }

    #[test]
    fn dialog_lists_bindings() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+Alt+M", "microphone.mute", json!({"mode": "hold"}), true),
                                                    b("b", "Ctrl+T", "camera.torch", json!({}), false)]), false, false);
        let props = plugin.section_props();
        assert_eq!(props["rows"][0]["keys"], json!("Ctrl+Alt+M"));
        assert_eq!(props["rows"][0]["does"], json!("Mute: switch while held, back on release"));
        assert_eq!(props["rows"][0]["copy-ok"], json!(false));
        assert_eq!(props["rows"][1]["where"], json!("In Telescope"));
        assert!(props["note-text"].as_str().unwrap().starts_with("Global shortcuts aren't available on this desktop"));
        ui(&mut plugin, &mut host, "remove", json!([0]));
        assert_eq!(plugin.bindings().len(), 1);
        ui(&mut plugin, &mut host, "copy-line", json!([0]));
        assert!(host.calls.iter().any(|c| c.starts_with("copy_text ") && c.ends_with("--action camera.torch")));
    }

    #[test]
    fn action_command_runs_the_action_in_the_running_copy() {
        let params = json!({"op": "up", "amount": 2.5, "whole": 10.0, "none": null}).as_object().unwrap().clone();
        let cmd = action_command("transforms.zoom", &params);
        assert_eq!(cmd[1..], ["--action", "transforms.zoom", "op=up", "amount=2.5", "whole=10"]);
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(windows_quote(r#"C:\Program Files\a"b"#), r#""C:\Program Files\a\"b""#);
    }

    #[test]
    fn global_presses_are_ignored_while_the_editor_records_a_key() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+Alt+M", "microphone.mute", json!({}), true)]), false, true);
        ui(&mut plugin, &mut host, "add", json!([]));
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "a"}));
        assert!(!plugin.on_key(&mut host, &key("m", true, true)));
        assert!(ran(&host).is_empty());
        ui(&mut plugin, &mut host, "cancel", json!([]));
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "a"}));
        assert_eq!(ran(&host).len(), 1);
    }

    #[test]
    fn desktop_settings_button_asks_again_for_unconfirmed_ones() {
        let (mut plugin, mut host, seen) = make(json!([b("a", "Ctrl+Alt+M", "microphone.mute", json!({}), true)]), false, true);
        seen.lock().unwrap().rebind = true;
        let applies = seen.lock().unwrap().applied.len();
        ui(&mut plugin, &mut host, "open-system", json!([]));
        assert_eq!(seen.lock().unwrap().applied.len(), applies + 1);
        assert!(!plugin.section_props()["note-text"].as_str().unwrap().starts_with("Couldn't open"));
    }

    #[test]
    fn clean_bindings_keeps_good_ones_and_fixes_ids() {
        let raw = json!([
            {"id": "b1", "keys": "ctrl+m", "action": "microphone.mute", "params": {"mode": "toggle"}, "global": true},
            {"id": "b1", "keys": "Ctrl+N", "action": "presets.cycle"},
            {"keys": "Ctrl", "action": "x"},
            {"keys": "Ctrl+Q"},
            "junk",
            {"id": "../evil", "keys": "F2", "action": "camera.torch", "params": {"mode": "on", "bad": [1]}, "global": "yes"},
        ]);
        let out = clean_bindings(&raw);
        assert_eq!(out.iter().map(|b| b.keys.as_str()).collect::<Vec<_>>(), ["Ctrl+M", "Ctrl+N", "F2"]);
        assert_eq!(out[0].to_json(), json!({"id": "b1", "keys": "Ctrl+M", "action": "microphone.mute",
                                            "params": {"mode": "toggle"}, "global": true}));
        assert!(out[1].id != "b1" && out[1].id.chars().all(char::is_alphanumeric));
        assert_ne!(out[2].id, "../evil");
        assert_eq!(Value::Object(out[2].params.clone()), json!({"mode": "on"}));
        assert!(!out[2].global);
        assert!(clean_bindings(&json!("nope")).is_empty());
    }

    #[test]
    fn clashes_marks_later_bindings_on_the_same_keys() {
        let bindings = clean_bindings(&json!([{"id": "a", "keys": "F1", "action": "x"}, {"id": "b", "keys": "F2", "action": "x"},
                                              {"id": "c", "keys": "F1", "action": "y"}]));
        assert_eq!(clashes(&bindings), HashSet::from(["c".to_string()]));
    }

    #[test]
    fn params_are_cleaned() {
        let number = json!({"name": "amount", "kind": "number", "default": 1.0, "min": 0.0, "max": 5.0, "decimals": 1});
        assert_eq!(clean_value(&number, Some(&json!(9))), json!(5.0));
        assert_eq!(clean_value(&number, Some(&json!("2.26"))), json!(2.3));
        assert_eq!(clean_value(&number, Some(&json!("lots"))), json!(1.0));
        assert_eq!(clean_value(&number, Some(&json!(true))), json!(1.0));
        let fixed = json!({"name": "mode", "kind": "choice", "default": "toggle", "choices": [["toggle", "T"], ["on", "On"]]});
        assert_eq!(clean_value(&fixed, Some(&json!("on"))), json!("on"));
        assert_eq!(clean_value(&fixed, Some(&json!("sideways"))), json!("toggle"));
        let dynamic = json!({"name": "name", "kind": "choice", "default": "", "choices": [["Desk", "Desk"]], "dynamic": true});
        assert_eq!(clean_value(&dynamic, Some(&json!("Gone"))), json!("Gone"));
        assert_eq!(clean_value(&dynamic, Some(&json!(3))), json!(""));
    }

    #[test]
    fn only_global_bindings_without_clashes_go_to_the_system() {
        let (plugin, _host, seen) = make(json!([
            b("a", "Ctrl+Alt+M", "microphone.mute", json!({}), true),
            b("b", "Ctrl+T", "camera.torch", json!({}), false),
            b("c", "Ctrl+Alt+M", "camera.torch", json!({}), true),
            b("d", "Ctrl+Alt+X", "gone.action", json!({}), true),
        ]), false, true);
        let applied = seen.lock().unwrap().applied.last().cloned().unwrap();
        assert_eq!(applied.iter().map(|h| (h.id.as_str(), h.keys.as_str())).collect::<Vec<_>>(), [("a", "Ctrl+Alt+M")]);
        assert_eq!(applied[0].description, "Mute: switch on or off");
        let w = |i: usize| plugin.where_works(&plugin.bindings()[i]);
        assert_eq!(w(1), ("In Telescope".into(), true));
        assert_eq!(w(2), ("Key used by a shortcut above".into(), false));
        assert_eq!(w(3), ("Not in this version of Telescope".into(), false));
        assert_eq!(w(0), ("Everywhere (setting up)".into(), true));
        seen.lock().unwrap().statuses.insert("a".into(), HotkeyStatus::new(false, "Another app already uses this key".into()));
        assert_eq!(w(0), ("In Telescope: Another app already uses this key".into(), false));
        seen.lock().unwrap().statuses.get_mut("a").unwrap().reaches_app = false;
        assert_eq!(w(0), ("Another app already uses this key".into(), false));
    }

    #[test]
    fn a_global_press_runs_the_action_and_hold_comes_back_on_release() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+Alt+M", "microphone.mute", json!({"mode": "hold"}), true)]), false, true);
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "a"}));
        plugin.on_hotkey(&mut host, &json!({"kind": "released", "id": "a"}));
        assert_eq!(ran(&host), [r#"run_action microphone microphone.mute {"mode":"hold"}"#,
                                r#"release_action microphone microphone.mute {"mode":"hold"}"#]);
    }

    #[test]
    fn a_background_press_notifies_only_when_asked() {
        let (mut plugin, mut host, _) = make(json!([b("a", "F9", "camera.torch", json!({}), true)]), true, true);
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "a"}));
        assert!(host.calls.contains(&"send_notification Telescope".to_string()));
        host.calls.clear();
        host.window_active = true;
        plugin.on_hotkey(&mut host, &json!({"kind": "released", "id": "a"}));
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "a"}));
        assert!(!host.calls.iter().any(|c| c.starts_with("send_notification")));
    }

    #[test]
    fn showing_the_window_from_the_background_doesnt_notify() {
        let (mut plugin, mut host, _) = make(json!([b("a", "F9", "window.show_hide", json!({}), true)]), true, true);
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "a"}));
        assert!(host.calls.contains(&"toggle_window".to_string()));
        assert!(!host.calls.iter().any(|c| c.starts_with("send_notification")));
    }

    #[test]
    fn keys_inside_telescope_run_bindings_and_skip_text_boxes() {
        let (mut plugin, mut host, _) = make(json!([
            b("a", "Ctrl+T", "camera.torch", json!({}), false),
            b("b", "Z", "transforms.zoom", json!({"op": "up", "amount": 10}), false),
            b("c", "Ctrl+Alt+M", "microphone.mute", json!({}), true),
            b("d", "F8", "camera.torch", json!({}), false),
        ]), false, true);
        assert!(plugin.on_key(&mut host, &key("t", true, false)));
        assert!(plugin.on_key(&mut host, &key("z", false, false)));
        let typing = |k: KeyInput| KeyInput { typing: true, ..k };
        assert!(!plugin.on_key(&mut host, &typing(key("z", false, false))));
        assert!(plugin.on_key(&mut host, &typing(key("t", true, false))));
        assert!(plugin.on_key(&mut host, &typing(key("\u{F70B}", false, false))));
        // A global one works inside too: the desktop may have bound another key for it
        assert!(plugin.on_key(&mut host, &key("m", true, true)));
        assert!(!plugin.on_key(&mut host, &KeyInput { modal: true, ..key("t", true, false) }));
        assert!(!plugin.on_key(&mut host, &key("q", true, false)));
        assert_eq!(ran(&host).len(), 5);
    }

    #[test]
    fn one_key_press_seen_globally_and_inside_runs_once() {
        let (mut plugin, mut host, _) = make(json!([b("c", "Ctrl+Alt+M", "microphone.mute", json!({}), true)]), false, true);
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "c"}));
        plugin.on_key(&mut host, &key("m", true, true));
        assert_eq!(ran(&host).len(), 1);
    }

    #[test]
    fn holding_a_key_repeats_steps_but_not_toggles() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Z", "transforms.zoom", json!({"op": "up", "amount": 5}), false),
                                                    b("b", "T", "camera.torch", json!({}), false)]), false, true);
        for repeat in [false, true, true] {
            plugin.on_key(&mut host, &KeyInput { repeat, ..key("z", false, false) });
            plugin.on_key(&mut host, &KeyInput { repeat, ..key("t", false, false) });
        }
        let runs = ran(&host);
        assert_eq!(runs.iter().filter(|r| r.contains("transforms.zoom")).count(), 3);
        assert_eq!(runs.iter().filter(|r| r.contains("camera.torch")).count(), 1);
    }

    #[test]
    fn hold_inside_telescope_comes_back_when_the_key_is_let_go() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+Space", "microphone.mute", json!({"mode": "hold"}), false)]), false, true);
        plugin.on_key(&mut host, &key(" ", true, false));
        // Ctrl may well come up first
        plugin.on_key(&mut host, &KeyInput { released: true, ..key(" ", false, false) });
        assert_eq!(ran(&host).len(), 2);
        assert!(ran(&host)[1].starts_with("release_action"));
        plugin.on_key(&mut host, &key(" ", true, false));
        plugin.on_keys_lost(&mut host);
        assert_eq!(ran(&host).len(), 4);
    }

    #[test]
    fn a_shifted_key_let_go_after_shift_comes_up_by_its_scan_code() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Shift+!", "microphone.mute", json!({"mode": "hold"}), false)]), false, true);
        plugin.on_key(&mut host, &KeyInput { shift: true, scan: 10, ..key("!", false, false) });
        assert_eq!(ran(&host).len(), 1);
        // Shift comes up first, so the key is now a plain "1" (the shift key itself has no text of ours)
        plugin.on_key(&mut host, &KeyInput { released: true, scan: 50, ..key("\u{10}", false, false) });
        assert_eq!(ran(&host).len(), 1, "Shift coming up is not the held key");
        plugin.on_key(&mut host, &KeyInput { released: true, scan: 10, ..key("1", false, false) });
        assert_eq!(ran(&host).len(), 2);
        assert!(ran(&host)[1].starts_with("release_action"));
    }

    #[test]
    fn a_release_without_a_scan_code_still_matches_by_key() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+Space", "microphone.mute", json!({"mode": "hold"}), false)]), false, true);
        plugin.on_key(&mut host, &key(" ", true, false));
        plugin.on_key(&mut host, &KeyInput { released: true, ..key(" ", false, false) });
        assert_eq!(ran(&host).len(), 2);
        // A different key's scan code lets nothing go
        plugin.on_key(&mut host, &KeyInput { scan: 7, ..key(" ", true, false) });
        plugin.on_key(&mut host, &KeyInput { released: true, scan: 8, ..key("q", false, false) });
        assert_eq!(ran(&host).len(), 3);
    }

    #[test]
    fn a_key_whose_control_is_off_goes_to_the_control_with_focus() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+T", "camera.torch", json!({}), false)]), false, true);
        // The torch can't be used right now: the host runs nothing for it
        host.actions.clear();
        assert!(!plugin.on_key(&mut host, &key("t", true, false)));
    }

    #[test]
    fn config_round_trip_and_editing() {
        let (mut plugin, mut host, seen) = make(json!([b("a", "Ctrl+Alt+M", "microphone.mute", json!({}), true)]), true, true);
        let cfg = plugin.get_config();
        assert_eq!(cfg["notify"], json!(true));
        assert_eq!(cfg["bindings"][0]["keys"], json!("Ctrl+Alt+M"));
        let mut more = plugin.bindings().to_vec();
        more.push(clean_bindings(&json!([b("z", "Ctrl+Alt+Z", "camera.torch", json!({}), true)])).remove(0));
        plugin.set_bindings(&mut host, more);
        assert!(host.calls.contains(&"schedule_save".to_string()));
        assert_eq!(seen.lock().unwrap().applied.last().unwrap().len(), 2);
        let applies = seen.lock().unwrap().applied.len();
        plugin.set_bindings(&mut host, plugin.bindings().to_vec());
        assert_eq!(seen.lock().unwrap().applied.len(), applies, "a same list isn't handed over again");
        assert_eq!(plugin.diagnostics()[0].1, "2 (2 everywhere, via FakeBackend)");
    }

    #[test]
    fn without_a_backend_global_bindings_still_work_inside() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+Alt+M", "microphone.mute", json!({}), true)]), false, false);
        assert_eq!(plugin.where_works(&plugin.bindings()[0]), ("In Telescope (no global shortcuts here)".into(), false));
        assert!(plugin.on_key(&mut host, &key("m", true, true)));
    }

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

    #[test]
    fn controls_right_click_menu_lists_the_saved_shortcuts_and_opens_the_editor() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+M", "microphone.mute", json!({}), true)]), false, true);
        let menu = plugin.menu.clone().unwrap();
        assert_eq!((menu[0].id.as_str(), menu[0].action.as_str()), ("a", "microphone.mute"));
        assert!(menu[0].text.starts_with("Ctrl+M: "), "{}", menu[0].text);
        plugin.settings_sections();
        let event = |callback: &str, id: &str| UiEvent { ui: "shortcut-menu".into(), callback: callback.into(), args: vec![json!(id)] };
        plugin.on_ui(&mut host, &event("add", "camera.torch"));
        assert!(host.calls.iter().any(|c| c == "open_settings shortcuts"));
        assert_eq!(plugin.section_props()["editor-open"], json!(false), "waits for Settings to build its page");
        assert!(host.scheduled.iter().any(|(_, t)| t == FROM_MENU));
        plugin.on_event(&mut host, &Event::TimerFired { owner: "shortcuts".into(), token: FROM_MENU.into() });
        let props = plugin.section_props();
        assert_eq!(props["editor-open"], json!(true));
        assert_eq!(controls(&plugin)[props["control"].as_i64().unwrap() as usize], "Camera: Torch");
        // A saved one opens as it is; an action that isn't there does nothing
        plugin.on_ui(&mut host, &event("edit", "a"));
        plugin.on_event(&mut host, &Event::TimerFired { owner: "shortcuts".into(), token: FROM_MENU.into() });
        assert_eq!(plugin.section_props()["keys-text"], json!("Ctrl+M"));
        host.calls.clear();
        plugin.on_ui(&mut host, &event("add", "nothing.here"));
        assert!(host.calls.is_empty());
        plugin.set_bindings(&mut host, Vec::new());
        assert!(host.calls.iter().any(|c| c == "set_shortcut_menu []"));
        plugin.shutdown(&mut host);
        assert!(host.calls.iter().any(|c| c == "set_shortcut_menu null"));
    }
}
