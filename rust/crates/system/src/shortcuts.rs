//! Keyboard shortcuts (plugins/shortcuts.py): keys bound to any plugin's action, inside Telescope or everywhere.
//!
//! Bindings are global settings, each running an action with its own settings, so "Exposure +1" and "Exposure -2" are
//! two bindings of one action. A global one is also claimed from the system (hotkeys.rs); one the system wouldn't give
//! still works inside Telescope, where the shell hands every key to Plugin::on_key first. `telescope --action ID
//! name=value` runs an action in the running copy too (handle_remote).
//!
//! An action's params are JSON: {"name", "label", "kind": "choice", "default", "choices": [[value, label], ...],
//! "shown_if": [name, [values]], "dynamic": true for a list that changes (a saved choice that's gone is kept)} or
//! {"name", "label", "kind": "number", "default", "minimum", "maximum", "decimals", "suffix"}. The first param can carry
//! "describe": a text with {name} for what the binding does ("Preset: {name}", with "lower": true for the choice's label in
//! lower case), where the usual wording ("Zoom: increase by 5%") doesn't fit.

use telescope_core::i18n::Fields;
use crate::remote::{all_actions, answered, choices, clean_value, fmt_number, full_params, handle_remote, number, own_actions, run_own, shown, WINDOW_SHOWN};
use crate::hotkeys::{self, Hotkey, HotkeyBackend, Signals, HOTKEY};
use crate::keys;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::actions;
use telescope_core::event::Event;
use telescope_core::fuzzy::{self, Doc};
use telescope_core::host::{Host, Poster};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, KeyInput, SettingsSection, ShortcutEntry, SlintSource, UiEvent, UiSpec};

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

/// Starts a program with an argument; false when it isn't there.
type Launcher = Box<dyn Fn(&[&str]) -> bool + Send>;

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
    launch: Launcher,
}

impl ShortcutsPlugin {
    pub fn new(remote: RemoteActions) -> ShortcutsPlugin {
        ShortcutsPlugin::with_backend(remote, Box::new(|signals| hotkeys::create_backend(&portal_app_id(), signals)))
    }

    pub fn with_backend(remote: RemoteActions, factory: BackendFactory) -> ShortcutsPlugin {
        ShortcutsPlugin {
            remote, factory: Some(factory), poster: None, bindings: Vec::new(), notify: false, actions: Vec::new(),
            backend: None, started: false, held: HashMap::new(), held_scan: HashMap::new(), editing: false, last_global: None, recent: HashMap::new(),
            editor: None, section_made: false, note_override: None, menu: None, from_menu: None, launch: Box::new(spawn_detached),
        }
    }

    /// How the desktop's own settings get started, for a test to stand in for it.
    pub fn with_launcher(mut self, launch: impl Fn(&[&str]) -> bool + Send + 'static) -> ShortcutsPlugin {
        self.launch = Box::new(launch);
        self
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

    /// The actions as the plugins give them now: a range can move while this runs (Max zoom), and Python reads it live.
    fn sync_actions(&mut self, host: &dyn Host) {
        if self.started {
            self.actions = all_actions(host);
        }
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
        if let (true, Some(_)) = (self.started, self.backend.as_ref()) {
            let skip = clashes(&self.bindings);
            let hotkeys: Vec<Hotkey> = self.bindings.iter()
                .filter(|b| b.global && !skip.contains(&b.id) && self.action(&b.action).is_some())
                .map(|b| Hotkey { id: b.id.clone(), keys: b.keys.clone(), description: self.describe(b) })
                .collect();
            // Rebinding can ask the user to confirm them all again (Plasma)
            if self.last_global.as_ref() != Some(&hotkeys) {
                if let Some(backend) = self.backend.as_mut() {
                    backend.apply(&hotkeys);
                }
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
            Some(st) if st.reaches_app => (tr("In Telescope: {status}").field("status", &st.text), false),
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
        self.sync_actions(host);
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
        self.sync_actions(host);
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
        let answer = answered(|| handle_remote(host, request));
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


fn clean_full_params(action: &ActionSpec, params: &Map<String, Value>) -> Map<String, Value> {
    full_params(action, params)
}

/// Whether holding the key repeats it: a step does, a "set to" doesn't.
fn repeats(action: &ActionSpec, params: &Map<String, Value>) -> bool {
    action.repeats && !matches!(params.get("op").and_then(Value::as_str), Some("set" | "reset"))
}

fn fill(text: String, fields: &[(&str, &str)]) -> String {
    fields.iter().fold(text, |t, (k, v)| t.field(k, v))
}

fn param<'a>(action: &'a ActionSpec, name: &str) -> Option<&'a Value> {
    action.params.iter().find(|p| p["name"] == name)
}

/// A number param's value as shown: with its suffix, signed where the range goes below zero (the control's own readout does).
fn shown_number(p: &Value, value: &Value) -> Option<String> {
    let n = value.as_f64()?;
    let signed = p["minimum"].as_f64().is_some_and(|lo| lo < 0.0);
    Some(actions::shown(n, p["decimals"].as_u64().unwrap_or(0) as usize, p["suffix"].as_str().unwrap_or(""), signed))
}

/// What the actions made by core::actions' builders say they do (describe in shortcuts.py): "Compensation: increase by 1.0 EV",
/// "ISO: raise by 1 stop", "Lens: next". None for any other shape of params.
fn builder_summary(action: &ActionSpec, params: &Map<String, Value>) -> Option<String> {
    let names: Vec<&str> = action.params.iter().filter_map(|p| p["name"].as_str()).collect();
    let label = action.label.as_str();
    let text = |name: &str| params.get(name).and_then(Value::as_str).unwrap_or("");
    let value_text = |name: &str| -> String {
        let shown = param(action, name).and_then(|p| params.get(name).and_then(|v| shown_number(p, v)));
        shown.unwrap_or_else(|| "…".into())
    };
    match names.as_slice() {
        ["mode"] => {
            // A card's own words for a mode ("Mute") stand alone; the generic ones read on from the label in lower case
            let mode = text("mode");
            let generic = actions::TOGGLE_MODES.iter().find(|(m, _)| *m == mode).map(|(_, t)| tr(t))?;
            let own = choices(param(action, "mode")?).into_iter().find(|(v, _)| *v == mode).map(|(_, l)| l)?;
            (own != generic).then_some(own)
        }
        ["op", "amount", "value"] => Some(match text("op") {
            "set" => fill(tr("{label}: set to {value}"), &[("label", label), ("value", &value_text("value"))]),
            "reset" => fill(tr("{label}: reset to default"), &[("label", label)]),
            op => {
                let p = param(action, "amount")?;
                let amount = params.get("amount").filter(|v| v.as_f64().is_some_and(|n| n != 0.0)).cloned().unwrap_or(json!(0));
                // The amount is never signed
                let unsigned = json!({"decimals": p["decimals"], "suffix": p["suffix"]});
                let amount = shown_number(&unsigned, &amount)?;
                let template = if op == "up" { tr("{label}: increase by {amount}") } else { tr("{label}: decrease by {amount}") };
                fill(template, &[("label", label), ("amount", &amount)])
            }
        }),
        ["op", "stops", "value"] => Some(match text("op") {
            "set" => fill(tr("{label}: set to {value}"), &[("label", label), ("value", &value_text("value"))]),
            op => {
                let stops = params.get("stops").and_then(Value::as_f64).unwrap_or(0.0);
                let up = op == "up";
                if stops.fract() == 0.0 {
                    // Whole counts take the plural form of the language
                    let n = stops as i64;
                    if up {
                        telescope_core::i18n::trn_with("{label}: raise by {n} stop", "{label}: raise by {n} stops", n, "", &[("label", &label)])
                    } else {
                        telescope_core::i18n::trn_with("{label}: lower by {n} stop", "{label}: lower by {n} stops", n, "", &[("label", &label)])
                    }
                } else {
                    // gettext plurals take whole numbers, so a fraction like 1.5 keeps its own text
                    let amount = telescope_core::i18n::number(stops, 1, false);
                    let template = if up { tr("{label}: raise by {stops} stops") } else { tr("{label}: lower by {stops} stops") };
                    fill(template, &[("label", label), ("stops", &amount)])
                }
            }
        }),
        ["op", "option"] => Some(match text("op") {
            "set" => {
                let option = text("option");
                let shown = choices(param(action, "option")?).into_iter().find(|(v, _)| v == option).map_or_else(|| option.to_string(), |(_, l)| l);
                fill(tr("{label}: {value}"), &[("label", label), ("value", if shown.is_empty() { "…" } else { &shown })])
            }
            op => fill(tr("{label}: {value}"), &[("label", label), ("value", &if op == "next" { tr("next") } else { tr("previous") })]),
        }),
        _ => None,
    }
}

/// What a binding with these params does: "Streaming: start or stop", "ISO: raise by 1 stop".
fn summary(action: &ActionSpec, params: &Map<String, Value>) -> String {
    if let Some(first) = action.params.first() {
        if let Some(template) = first["describe"].as_str() {
            let name = first["name"].as_str().unwrap_or("");
            let v = match params.get(name) { Some(Value::String(s)) => s.clone(), _ => String::new() };
            let value = if first["lower"] == true { choices(first).into_iter().find(|(c, _)| *c == v).map_or(v, |(_, l)| l.to_lowercase()) } else { v };
            return fill(tr(template), &[("name", if value.is_empty() { "…" } else { &value })]);
        }
    }
    if let Some(text) = builder_summary(action, params) {
        return text;
    }
    let mut parts = Vec::new();
    for p in &action.params {
        let Some(name) = p["name"].as_str() else { continue };
        if !shown(p, params) {
            continue;
        }
        let value = params.get(name).cloned().unwrap_or(Value::Null);
        let text = if p["kind"] == "number" {
            shown_number(p, &value).unwrap_or_else(|| "…".into())
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


const UI: &str = include_str!("../ui/shortcuts.slint");
const SECTION: &str = "shortcuts";
const COPIED: &str = "copied";
// A right-click asked for a shortcut: the editor opens this long after Settings, once its page is built
const FROM_MENU: &str = "from_menu";
const FROM_MENU_MS: u64 = 150;

/// Starts a program that carries on without us; false when it can't be started.
fn spawn_detached(cmd: &[&str]) -> bool {
    let Some((program, args)) = cmd.split_first() else { return false };
    telescope_core::launch::detached(program, args).is_ok()
}

/// The command line that runs this action in the running Telescope (for a desktop's own shortcut settings).
pub fn action_command(action: &str, params: &Map<String, Value>) -> Vec<String> {
    let mut out = telescope_core::system::start_command().unwrap_or_else(|_| vec!["telescope-rs".into()]);
    out.extend(["--action".into(), action.into()]);
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

/// Whether the query fits the action's card, name, id, keywords or the names of what it can do (forgiving about typos).
fn matches(action: &ActionSpec, query: &str) -> bool {
    if fuzzy::split(query).is_empty() {
        return true;
    }
    let mut extra = action.id.clone();
    for p in action.params.iter().filter(|p| p["kind"] != "number" && p["dynamic"] != true) {
        for (_, label) in choices(p) {
            extra.push(' ');
            extra.push_str(&label);
        }
    }
    let doc = Doc::new(&[("name", &action.label), ("keywords", &action.keywords), ("card", &action.group), ("extra", &extra)]);
    doc.score(query, &[("name", 1.0), ("keywords", 0.9), ("card", 0.7), ("extra", 0.6)], false) > 0.0
}

/// The shortcut being added or changed.
struct Editor {
    id: String,
    query: String,
    /// Kept while a search hides it.
    action: Option<String>,
    params: Map<String, Value>,
    keys: String,
    /// The last key pressed was a modifier alone, which changes nothing.
    bare_modifier: bool,
    global: bool,
    copied: bool,
}

impl ShortcutsPlugin {
    fn shown_actions(&self, query: &str) -> Vec<&ActionSpec> {
        self.actions.iter().map(|(_, a)| a).filter(|a| matches(a, query)).collect()
    }

    /// _fill_actions: the Control list's rows, each group's name in capitals over its actions; the action each row picks.
    fn control_rows(listed: &[&ActionSpec]) -> Vec<(String, Option<usize>)> {
        let mut rows = Vec::new();
        let mut group = None;
        for (i, a) in listed.iter().enumerate() {
            if group != Some(&a.group) {
                group = Some(&a.group);
                rows.push((a.group.to_uppercase(), None));
            }
            rows.push((a.label.clone(), Some(i)));
        }
        rows
    }

    fn open_editor(&mut self, host: &mut dyn Host, binding: Binding) {
        let action = self.action(&binding.action).map(|(_, a)| a.clone());
        let params = action.as_ref().map_or_else(|| binding.params.clone(), |a| self.start_params(a, &binding.params));
        let action = (!binding.action.is_empty()).then(|| binding.action.clone());
        self.editor = Some(Editor { id: binding.id, query: String::new(), action, params, keys: binding.keys, bare_modifier: false, global: binding.global, copied: false });
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
                let start = action.current.or_else(|| p["minimum"].as_f64()).unwrap_or(0.0);
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
        let bare = || tr("A modifier can't be a shortcut on its own. Add a key to it.");
        if e.keys.is_empty() {
            let text = if e.bare_modifier { bare() } else { tr("Press a key, with Ctrl, Alt, Shift or {meta} if you like.").field("meta", meta) };
            return (text, true);
        }
        if let Some(other) = self.bindings.iter().find(|b| b.keys == e.keys && b.id != e.id) {
            let text = tr("{keys} already does \"{action}\". Pick another key.")
                .field("keys", keys::native_keys(&e.keys)).field("action", self.describe(other));
            return (text, true);
        }
        // The key it had still stands, so it can be saved as it is
        if e.bare_modifier {
            return (bare(), false);
        }
        if e.global && keys::edits_text(&e.keys) {
            let text = tr("Everywhere, {keys} would stop typing it in other apps. Adding Ctrl, Alt or {meta} is safer.")
                .field("keys", keys::native_keys(&e.keys)).field("meta", meta);
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
        let rows = Self::control_rows(&listed);
        let mut controls: Vec<String> = rows.iter().map(|(t, _)| t.clone()).collect();
        let mut headings: Vec<bool> = rows.iter().map(|(_, a)| a.is_none()).collect();
        let picked = e.action.as_deref().and_then(|id| listed.iter().position(|a| a.id == id));
        let mut control = picked.and_then(|i| rows.iter().position(|(_, a)| *a == Some(i))).map_or(-1, |r| r as i64);
        if let (Some(id), true, -1) = (e.action.as_deref(), e.query.is_empty(), control) {
            // An action this version doesn't have: kept
            controls.push(tr("Unknown ({action})").field("action", id));
            headings.push(false);
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
                options.push((v.clone(), tr("{value} (not there now)").field("value", &v)));
                current = Some(options.len() - 1);
            }
            let items: Vec<String> = options.into_iter().map(|(_, l)| l).collect();
            Some(json!({"label": p["label"], "number": false, "items": items, "current": current.unwrap_or(0), "text": "", "shown": shown(p, &e.params)}))
        }).collect()).unwrap_or_default();
        // Hidden ones are left out rather than shrunk (a shrunk row still gets the form's spacing); `at` keeps each one's place
        let params: Vec<Value> = params.into_iter().enumerate().filter(|(_, p)| p["shown"] == true)
            .map(|(i, mut p)| { p["at"] = json!(i); p }).collect();
        let (problem, blocking) = self.editor_problem(e, listed.len());
        let hold = e.params.get("mode").and_then(Value::as_str) == Some("hold");
        json!({
            "editor-open": true,
            "query": e.query,
            "controls": controls,
            "control-headings": headings,
            "control": control,
            "params": params,
            "keys-text": keys::native_keys(&e.keys),
            "global": e.global,
            "problem": problem,
            "save-enabled": problem.is_empty() || !blocking,
            "copy-enabled": !hold,
            "copy-text": if e.copied { tr("Copied") } else { tr("Copy command") },
            "copy-tip": if hold { tr("A command can't hold a key down. Pick another choice to copy its command.") }
                        else { tr("A command line that does the same, for your desktop's own keyboard settings") },
        })
    }

    fn section_props(&self) -> Value {
        let rows: Vec<Value> = self.bindings.iter().map(|b| {
            let (where_text, ok) = self.where_works(b);
            let does = self.describe(b);
            json!({"keys": keys::native_keys(&b.keys), "does": does, "where": where_text, "ok": ok,
                   "copy-ok": b.params.get("mode").and_then(Value::as_str) != Some("hold"),
                   "edit-label": tr("Edit {shortcut}").field("shortcut", &does),
                   "remove-label": tr("Remove {shortcut}").field("shortcut", &does)})
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
        self.sync_actions(host);
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
        self.sync_actions(host);
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
                if (self.launch)(&cmd) {
                    return true;
                }
            }
        }
        false
    }

    fn on_section(&mut self, host: &mut dyn Host, callback: &str, args: &[Value]) {
        self.sync_actions(host);
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
                let listed = self.shown_actions(&query);
                let row = usize::try_from(int(0)).ok().and_then(|r| Self::control_rows(&listed).get(r).and_then(|(_, a)| *a));
                let id = row.map(|i| listed[i].id.clone());
                if let Some(id) = id {
                    self.pick_action(&id);
                }
                self.refresh(host);
            }
            "param-picked" | "param-edited" | "param-committed" => {
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
                    let Some(n) = number(&json!(text(1))) else {
                        if callback == "param-committed" {
                            self.refresh(host);
                        }
                        return;
                    };
                    clean_value(p, Some(&json!(n)))
                };
                let name = name.to_string();
                if let Some(e) = self.editor.as_mut() {
                    e.params.insert(name, value);
                }
                // A number being typed keeps its box as it is; a pick can show or hide other settings
                if callback != "param-edited" {
                    self.refresh(host);
                }
            }
            // The arrow keys in a number's box: Up raises it and Down lowers it by one in the last place it shows (the box sends Down as +1)
            "param-step" => {
                let Some(e) = self.editor.as_ref() else { return };
                let Some((_, action)) = e.action.as_deref().and_then(|id| self.action(id)).cloned() else { return };
                let Some(p) = usize::try_from(int(0)).ok().and_then(|i| action.params.get(i)) else { return };
                let Some(name) = p["name"].as_str().filter(|_| p["kind"] == "number") else { return };
                let by = -int(1).signum() as f64;
                let step = 10f64.powi(-(p["decimals"].as_u64().unwrap_or(0) as i32));
                // An empty box takes its lowest value first
                let value = match e.params.get(name).and_then(number) {
                    Some(n) => n + by * step,
                    None => p["minimum"].as_f64().unwrap_or(0.0),
                };
                let value = clean_value(p, Some(&json!(value)));
                let name = name.to_string();
                if let Some(e) = self.editor.as_mut() {
                    e.params.insert(name, value);
                }
                self.refresh(host);
            }
            "key-pressed" => {
                let keys = keys::keys_from_slint(&text(0), flag(1), flag(2), flag(3), flag(4));
                // A bare modifier waits for the key that comes with it; anything else without a name changes nothing
                let bare = keys::is_bare_modifier(&text(0));
                if keys.is_empty() && !bare {
                    return;
                }
                if let Some(e) = self.editor.as_mut() {
                    e.bare_modifier = bare;
                    if !bare {
                        e.keys = keys;
                    }
                }
                self.refresh(host);
            }
            "key-cleared" => {
                if let Some(e) = self.editor.as_mut() {
                    e.keys.clear();
                    e.bare_modifier = false;
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
            keywords: String::new(),
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
    use crate::remote::{checked_params, list_text, param_problem};

    #[derive(Default)]
    struct Seen {
        applied: Vec<Vec<Hotkey>>,
        statuses: HashMap<String, HotkeyStatus>,
        suspended: Vec<bool>,
        rebind: bool,
        stopped: bool,
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
        fn stop(&mut self) {
            self.0.lock().unwrap().stopped = true;
        }
    }

    fn mute() -> ActionSpec {
        actions::toggle_action("microphone.mute", "Mute", "Microphone", "", &[])
    }

    fn zoom() -> ActionSpec {
        let spec = actions::SliderSpec { id: "transforms.zoom", label: "Zoom", group: "Transforms", keywords: "", minimum: 0.0, maximum: 100.0, step: 10.0,
                                         decimals: 0, suffix: "", has_default: false };
        actions::slider_action(&spec, 0.0)
    }

    fn torch() -> ActionSpec {
        actions::toggle_action("camera.torch", "Torch", "Camera", "", &[])
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

    /// The Control list's rows, a group's heading as "# GROUP".
    fn controls(plugin: &ShortcutsPlugin) -> Vec<String> {
        let props = plugin.section_props();
        let rows: Vec<String> = serde_json::from_value(props["controls"].clone()).unwrap();
        let headings: Vec<bool> = serde_json::from_value(props["control-headings"].clone()).unwrap();
        rows.into_iter().zip(headings).map(|(r, h)| if h { format!("# {r}") } else { r }).collect()
    }

    /// The Control list without its group headings.
    fn listed(plugin: &ShortcutsPlugin) -> Vec<String> {
        controls(plugin).into_iter().filter(|r| !r.starts_with("# ")).collect()
    }

    /// The label of the control picked now, by what the list shows; None when it isn't listed.
    fn picked(plugin: &ShortcutsPlugin) -> Option<String> {
        let control = plugin.section_props()["control"].as_i64()?;
        usize::try_from(control).ok().and_then(|i| controls(plugin).get(i).cloned())
    }

    /// The labels of the settings shown for the picked control.
    fn shown_params(plugin: &ShortcutsPlugin) -> Vec<String> {
        plugin.section_props()["params"].as_array().unwrap().iter().map(|p| p["label"].as_str().unwrap().to_string()).collect()
    }

    /// Picks an option of a setting by the words on them.
    fn choose(plugin: &mut ShortcutsPlugin, host: &mut FakeHost, setting: &str, option: &str) {
        let params = plugin.section_props()["params"].clone();
        let param = params.as_array().unwrap().iter().find(|p| p["label"] == setting).unwrap_or_else(|| panic!("no setting {setting}"));
        let at = param["items"].as_array().unwrap().iter().position(|i| i == option).unwrap_or_else(|| panic!("no option {option}"));
        ui(plugin, host, "param-picked", json!([param["at"], at]));
    }

    fn notes(host: &FakeHost) -> Vec<(&str, &str, bool)> {
        host.notifications.iter().map(|(t, b, u)| (t.as_str(), b.as_str(), *u)).collect()
    }

    /// The row of the action with this label under this group's heading.
    fn row_of(plugin: &ShortcutsPlugin, group: &str, label: &str) -> usize {
        let rows = controls(plugin);
        let start = rows.iter().position(|r| *r == format!("# {}", group.to_uppercase())).unwrap();
        start + rows[start..].iter().position(|r| r == label).unwrap()
    }

    #[test]
    fn the_arrow_keys_step_a_number_in_the_editor() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+T", "camera.torch", json!({}), false)]), false, true);
        host.actions[2].1.current = Some(30.0);
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "add", json!([]));
        let zoom = row_of(&plugin, "Transforms", "Zoom");
        ui(&mut plugin, &mut host, "control-picked", json!([zoom]));
        choose(&mut plugin, &mut host, "Does", "Set to");
        let at = plugin.section_props()["params"][1]["at"].clone();
        // The box sends Down as +1, so Up raises the number
        ui(&mut plugin, &mut host, "param-step", json!([at, -1]));
        assert_eq!(plugin.section_props()["params"][1]["text"], json!("31"));
        ui(&mut plugin, &mut host, "param-step", json!([at, 1]));
        ui(&mut plugin, &mut host, "param-step", json!([at, 1]));
        assert_eq!(plugin.section_props()["params"][1]["text"], json!("29"));
        // Past its range it stays at the end
        for _ in 0..400 {
            ui(&mut plugin, &mut host, "param-step", json!([at, -1]));
        }
        let top = plugin.section_props()["params"][1]["text"].as_str().unwrap().to_string();
        ui(&mut plugin, &mut host, "param-step", json!([at, -1]));
        assert_eq!(plugin.section_props()["params"][1]["text"], json!(top));
        // A step on a choice, or on nothing, changes nothing
        ui(&mut plugin, &mut host, "param-step", json!([0, 1]));
        ui(&mut plugin, &mut host, "param-step", json!([99, 1]));
        assert_eq!(plugin.section_props()["params"][1]["text"], json!(top));
    }

    #[test]
    fn editor_builds_a_binding() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+T", "camera.torch", json!({}), false)]), false, true);
        host.actions[2].1.current = Some(30.0);
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "add", json!([]));
        let zoom = row_of(&plugin, "Transforms", "Zoom");
        ui(&mut plugin, &mut host, "control-picked", json!([zoom]));
        assert_eq!(plugin.section_props()["save-enabled"], json!(false), "no key yet");
        assert_eq!(shown_params(&plugin), ["Does", "By"]);
        choose(&mut plugin, &mut host, "Does", "Set to");
        assert_eq!(shown_params(&plugin), ["Does", "Value"], "the amount goes and the value comes");
        assert_eq!(plugin.section_props()["params"][1]["text"], json!("30"), "starts where the control is");
        ui(&mut plugin, &mut host, "key-pressed", json!(["t", true, false, false, false]));
        let props = plugin.section_props();
        assert_eq!(props["save-enabled"], json!(false));
        assert!(props["problem"].as_str().unwrap().contains("already does"), "{}", props["problem"]);
        ui(&mut plugin, &mut host, "key-pressed", json!(["q", false, false, false, false]));
        let props = plugin.section_props();
        assert_eq!(props["save-enabled"], json!(true), "allowed, with a warning, everywhere");
        assert!(props["problem"].as_str().unwrap().contains("stop typing"), "{}", props["problem"]);
        ui(&mut plugin, &mut host, "key-pressed", json!(["z", true, false, true, false]));
        assert_eq!(plugin.section_props()["problem"], json!(""));
        ui(&mut plugin, &mut host, "save", json!([]));
        let saved = plugin.bindings().last().unwrap();
        assert_eq!((saved.keys.as_str(), saved.action.as_str(), saved.global), ("Ctrl+Shift+Z", "transforms.zoom", true));
        assert_eq!(Value::Object(saved.params.clone()), json!({"op": "set", "amount": 10.0, "value": 30.0}));
    }

    #[test]
    fn a_bare_modifier_leaves_a_saved_key_alone_and_says_why() {
        let (mut plugin, mut host, _) = make(json!([{"id": "a", "keys": "Ctrl+T", "action": "camera.torch", "params": {"mode": "toggle"}}]), false, true);
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "edit", json!([0]));
        for key in ["\u{14}", "\u{F8FF}"] {
            ui(&mut plugin, &mut host, "key-pressed", json!([key, false, false, false, false]));
            assert_eq!(plugin.section_props()["problem"], json!(""), "Caps Lock and the like are no modifiers");
        }
        ui(&mut plugin, &mut host, "key-pressed", json!(["\u{11}", true, false, false, false]));
        let props = plugin.section_props();
        assert_eq!((props["keys-text"].clone(), props["save-enabled"].clone()), (json!("Ctrl+T"), json!(true)));
        assert_eq!(props["problem"], json!("A modifier can't be a shortcut on its own. Add a key to it."));
        ui(&mut plugin, &mut host, "key-pressed", json!(["k", true, false, false, false]));
        let props = plugin.section_props();
        assert_eq!((props["keys-text"].clone(), props["problem"].clone()), (json!("Ctrl+K"), json!("")));
        ui(&mut plugin, &mut host, "key-pressed", json!(["\u{11}", true, false, false, false]));
        ui(&mut plugin, &mut host, "key-cleared", json!([]));
        assert!(plugin.section_props()["problem"].as_str().unwrap().starts_with("Press a key"));
    }

    #[test]
    fn editor_saves_a_binding_and_a_bare_modifier_waits_for_its_key() {
        let (mut plugin, mut host, seen) = make(json!([]), false, true);
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "add", json!([]));
        assert_eq!(seen.lock().unwrap().suspended, [true], "global keys let go while one is being picked");
        let props = plugin.section_props();
        assert_eq!(props["editor-open"], json!(true));
        let mute = row_of(&plugin, "Microphone", "Mute");
        ui(&mut plugin, &mut host, "control-picked", json!([mute]));
        let meta = if cfg!(windows) { "Win" } else { "Meta" };
        assert_eq!(plugin.section_props()["problem"], json!(format!("Press a key, with Ctrl, Alt, Shift or {meta} if you like.")));
        ui(&mut plugin, &mut host, "param-picked", json!([0, 3]));
        assert_eq!(plugin.section_props()["copy-enabled"], json!(false), "a command can't hold a key down");
        ui(&mut plugin, &mut host, "key-pressed", json!(["\u{11}", true, false, false, false]));
        let props = plugin.section_props();
        assert_eq!(props["keys-text"], json!(""), "a bare modifier waits for its key");
        assert_eq!((props["problem"].clone(), props["save-enabled"].clone()), (json!("A modifier can't be a shortcut on its own. Add a key to it."), json!(false)));
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
        assert_eq!(props["problem"], json!("Ctrl+T already does \"Torch: switch on or off\". Pick another key."));
        assert_eq!(props["save-enabled"], json!(false));
        ui(&mut plugin, &mut host, "key-pressed", json!(["z", false, false, false, false]));
        let props = plugin.section_props();
        assert!(props["problem"].as_str().unwrap().starts_with("Everywhere, Z would stop typing it in other apps."));
        assert_eq!(props["save-enabled"], json!(true), "only a warning");
        ui(&mut plugin, &mut host, "global-toggled", json!([false]));
        assert_eq!(plugin.section_props()["problem"], json!(""));
    }

    #[test]
    fn editor_warns_about_typing_keys_only() {
        let (mut plugin, mut host, _) = make(json!([]), false, true);
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "add", json!([]));
        let torch = row_of(&plugin, "Camera", "Torch");
        ui(&mut plugin, &mut host, "control-picked", json!([torch]));
        choose(&mut plugin, &mut host, "Does", "Switch while held, back on release");
        ui(&mut plugin, &mut host, "key-pressed", json!(["\u{F708}", false, false, false, false]));
        let props = plugin.section_props();
        assert_eq!((props["keys-text"].clone(), props["problem"].clone()), (json!("F5"), json!("")), "F5 types nothing");
        ui(&mut plugin, &mut host, "key-pressed", json!(["q", false, false, false, false]));
        let props = plugin.section_props();
        assert!(props["problem"].as_str().unwrap().contains("stop typing"), "{}", props["problem"]);
        assert_eq!(props["copy-enabled"], json!(false), "a command can't hold a key");
        choose(&mut plugin, &mut host, "Does", "Switch on or off");
        assert_eq!(plugin.section_props()["copy-enabled"], json!(true));
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
    fn the_editors_control_list_marks_group_titles_so_they_look_different() {
        let (mut plugin, mut host, _) = make(json!([]), false, true);
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "add", json!([]));
        let rows = controls(&plugin);
        let titles: Vec<&String> = rows.iter().filter(|r| r.starts_with("# ")).collect();
        assert!(!titles.is_empty() && titles.iter().all(|t| *t == &t.to_uppercase()), "{rows:?}");
        assert!(rows.iter().any(|r| !r.starts_with("# ")));
        let before = picked(&plugin);
        let title = rows.iter().rposition(|r| r.starts_with("# ")).unwrap();
        ui(&mut plugin, &mut host, "control-picked", json!([title]));
        assert_eq!(picked(&plugin), before, "a group's title can't be picked");
    }

    #[test]
    fn editor_search_narrows_the_controls() {
        let (mut plugin, mut host, _) = make(json!([]), false, true);
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "add", json!([]));
        let all = plugin.actions.len();
        assert_eq!(listed(&plugin).len(), all);
        ui(&mut plugin, &mut host, "search", json!(["torch"]));
        assert_eq!(listed(&plugin), ["Torch"]);
        assert_eq!(picked(&plugin).as_deref(), Some("Torch"), "the only match is picked");
        assert_eq!(shown_params(&plugin), ["Does"]);
        ui(&mut plugin, &mut host, "search", json!(["push talk"]));
        assert!(listed(&plugin).is_empty(), "what a setting can do counts, and this one can't: {:?}", listed(&plugin));
        ui(&mut plugin, &mut host, "search", json!(["held"]));
        assert_eq!(listed(&plugin), ["Mute", "Torch"], "what a setting can do counts too");
        assert_eq!(picked(&plugin).as_deref(), Some("Torch"), "still there, so still picked");
        ui(&mut plugin, &mut host, "search", json!(["zzz"]));
        let props = plugin.section_props();
        assert!(listed(&plugin).is_empty());
        assert_eq!(props["problem"], json!("No control matches that search."));
        assert_eq!(props["save-enabled"], json!(false));
        ui(&mut plugin, &mut host, "search", json!([""]));
        assert_eq!(listed(&plugin).len(), all);
    }

    #[test]
    fn editor_search_uses_keywords() {
        let (mut plugin, mut host, _) = make(json!([]), false, true);
        host.actions[2].1.keywords = "crop framing".into();
        plugin.settings_sections();
        ui(&mut plugin, &mut host, "add", json!([]));
        ui(&mut plugin, &mut host, "search", json!(["crop"]));
        assert_eq!(controls(&plugin), ["# TRANSFORMS", "Zoom"]);
        assert_eq!(picked(&plugin).as_deref(), Some("Zoom"));
    }

    #[test]
    fn dialog_lists_bindings() {
        let (mut plugin, mut host, seen) = make(json!([b("a", "Ctrl+Alt+M", "microphone.mute", json!({}), true), b("b", "F2", "camera.torch", json!({}), false)]), false, true);
        seen.lock().unwrap().statuses.insert("a".into(), HotkeyStatus::new(true, "Everywhere: Meta+M".into()));
        let rows = plugin.section_props()["rows"].clone();
        assert_eq!((rows[0]["where"].clone(), rows[1]["where"].clone()), (json!("Everywhere: Meta+M"), json!("In Telescope")));
        ui(&mut plugin, &mut host, "remove", json!([1]));
        assert_eq!(plugin.bindings().iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["a"]);
        let rows = plugin.section_props()["rows"].clone();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["remove-label"], json!("Remove Mute: switch on or off"));
    }

    #[test]
    fn dialog_says_what_a_binding_does_and_copies_its_line() {
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
        let number = json!({"name": "amount", "kind": "number", "default": 1.0, "minimum": 0.0, "maximum": 5.0, "decimals": 1});
        assert_eq!(clean_value(&number, Some(&json!(9))), json!(5.0));
        assert_eq!(clean_value(&number, Some(&json!("2.26"))), json!(2.3));
        assert_eq!(clean_value(&number, Some(&json!("lots"))), json!(1.0));
        assert_eq!(clean_value(&number, Some(&json!(true))), json!(1.0));
        // NaN from telescope --action; JSON has no NaN of its own
        assert_eq!(clean_value(&number, Some(&json!("nan"))), json!(1.0));
        assert_eq!(clean_value(&number, Some(&json!("inf"))), json!(1.0));
        let fixed = json!({"name": "mode", "kind": "choice", "default": "toggle", "choices": [["toggle", "T"], ["on", "On"]]});
        assert_eq!(clean_value(&fixed, Some(&json!("on"))), json!("on"));
        assert_eq!(clean_value(&fixed, Some(&json!("sideways"))), json!("toggle"));
        let dynamic = json!({"name": "name", "kind": "choice", "default": "", "choices": [["Desk", "Desk"]], "dynamic": true});
        assert_eq!(clean_value(&dynamic, Some(&json!("Gone"))), json!("Gone"));
        assert_eq!(clean_value(&dynamic, Some(&json!(3))), json!(""));
        let always = json!({"name": "op", "kind": "choice", "default": "a", "choices": [["a", "A"], ["b", "B"]]});
        assert!(shown(&always, &Map::new()));
        let set_only = json!({"name": "v", "kind": "number", "shown_if": ["op", ["set"]]});
        assert!(!shown(&set_only, json!({"op": "up"}).as_object().unwrap()));
        assert!(shown(&set_only, json!({"op": "set"}).as_object().unwrap()));
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
        host.action_answers.insert("camera.torch".into(), Some("Torch: on".into()));
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "a"}));
        assert_eq!(notes(&host), [("Telescope", "Torch: on", false)]);
        ui(&mut plugin, &mut host, "notify-toggled", json!([false]));
        plugin.on_hotkey(&mut host, &json!({"kind": "released", "id": "a"}));
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "a"}));
        assert_eq!(host.notifications.len(), 1);
        assert_eq!(ran(&host).iter().filter(|r| r.starts_with("run_action")).count(), 2, "it still ran");
    }

    #[test]
    fn a_press_with_the_window_in_front_doesnt_notify() {
        let (mut plugin, mut host, _) = make(json!([b("a", "F9", "camera.torch", json!({}), true)]), true, true);
        host.window_active = true;
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "a"}));
        assert_eq!(ran(&host).len(), 1);
        assert!(host.notifications.is_empty());
    }

    #[test]
    fn showing_the_window_from_the_background_doesnt_notify() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+Alt+H", "window.show_hide", json!({}), true)]), true, true);
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "a"}));
        assert!(host.calls.contains(&"toggle_window".to_string()));
        assert!(host.notifications.is_empty(), "shown: the window itself is the answer");
        plugin.on_hotkey(&mut host, &json!({"kind": "released", "id": "a"}));
        plugin.on_hotkey(&mut host, &json!({"kind": "pressed", "id": "a"}));
        assert_eq!(notes(&host), [("Telescope", "Telescope: hidden", false)]);
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

    // test_shortcuts_plugin.py::test_hold_comes_back_by_scan_code_when_the_key_changes_on_release
    #[test]
    fn hold_comes_back_by_scan_code_when_the_key_changes_on_release() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+Shift+1", "microphone.mute", json!({"mode": "hold"}), false)]), false, true);
        plugin.press(&mut host, "a", Source::App, false, 10);
        assert_eq!(ran(&host).len(), 1);
        // Shift came up first, so the key that comes up is "!" and not the binding's "1"
        plugin.release_app_keys(&mut host, Some("!"), 10);
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
        let (mut plugin, mut host, _) = make(json!([b("a", "Z", "transforms.zoom", json!({"op": "up", "amount": 10}), false)]), false, true);
        // The zoom can't be used right now: its plugin runs nothing
        host.action_answers.insert("transforms.zoom".into(), None);
        assert!(!plugin.on_key(&mut host, &KeyInput { shift: true, ..key("Z", false, false) }), "Shift+Z isn't bound");
        assert!(ran(&host).is_empty());
        assert!(!plugin.on_key(&mut host, &key("z", false, false)));
        assert!(!plugin.is_held("a"), "nothing happened, so there's nothing to let go of");
        host.action_answers.clear();
        assert!(plugin.on_key(&mut host, &key("z", false, false)));
        assert!(ran(&host).last().unwrap().ends_with(r#"{"op":"up","amount":10.0,"value":null}"#), "{:?}", ran(&host));
        plugin.on_key(&mut host, &KeyInput { released: true, ..key("z", false, false) });
        // Nor does a key do anything for a control this version no longer has
        host.actions.clear();
        assert!(!plugin.on_key(&mut host, &key("z", false, false)));
    }

    #[test]
    fn config_round_trip_and_editing() {
        let (mut plugin, mut host, seen) = make(json!([]), false, true);
        let binding = |id: &str, keys: &str, global: bool| Binding { id: id.into(), keys: keys.into(), action: "camera.torch".into(), params: Map::new(), global };
        plugin.set_bindings(&mut host, vec![binding("a", "ctrl+f1", true)]);
        assert_eq!(host.calls.iter().filter(|c| *c == "schedule_save").count(), 1);
        assert_eq!(plugin.get_config(), json!({"bindings": [b("a", "Ctrl+F1", "camera.torch", json!({}), true)], "notify": false}));
        let applied = |seen: &Arc<Mutex<Seen>>| seen.lock().unwrap().applied.last().unwrap().iter().map(|h| h.id.clone()).collect::<Vec<_>>();
        assert_eq!(applied(&seen), ["a"]);
        assert_eq!(seen.lock().unwrap().applied.len(), 2);
        let mut more = plugin.bindings().to_vec();
        more.push(binding("x", "F3", false));
        plugin.set_bindings(&mut host, more);
        assert_eq!(seen.lock().unwrap().applied.len(), 2, "not global: nothing to rebind");
        plugin.set_bindings(&mut host, plugin.bindings().to_vec());
        assert_eq!(seen.lock().unwrap().applied.len(), 2, "a same list isn't handed over again");
        assert_eq!(plugin.diagnostics()[0].1, "2 (1 everywhere, via FakeBackend)");
        ui(&mut plugin, &mut host, "remove", json!([0]));
        assert_eq!(plugin.get_config()["bindings"].as_array().unwrap().iter().map(|b| b["id"].clone()).collect::<Vec<_>>(), [json!("x")]);
        assert!(applied(&seen).is_empty());
        plugin.shutdown(&mut host);
        assert!(seen.lock().unwrap().stopped);
    }

    #[test]
    fn without_a_backend_global_bindings_still_work_inside() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Ctrl+Alt+M", "microphone.mute", json!({}), true)]), false, false);
        assert_eq!(plugin.where_works(&plugin.bindings()[0]), ("In Telescope (no global shortcuts here)".into(), false));
        assert!(plugin.on_key(&mut host, &key("m", true, true)));
    }

    #[test]
    fn remote_actions() {
        let (_plugin, mut host, _) = make(json!([]), false, true);
        let listing = handle_remote(&mut host, &json!({"list": true}));
        let text = listing["text"].as_str().unwrap();
        assert_eq!(listing["ok"], json!(true));
        assert!(text.contains("microphone.mute  (Microphone: Mute)"), "{text}");
        assert!(text.contains("    mode=toggle | on | off | hold  (default toggle)"), "{text}");
        assert!(text.contains("    amount=<number 0 to 100>"), "{text}");
        host.action_answers.insert("camera.torch".into(), Some("Torch: on".into()));
        assert_eq!(handle_remote(&mut host, &json!({"action": "camera.torch", "params": {"mode": "on"}})), json!({"ok": true, "text": "Torch: on"}));
        assert_eq!(ran(&host).last().unwrap(), r#"run_action camera_control camera.torch {"mode":"on"}"#);
        assert_eq!(handle_remote(&mut host, &json!({"action": "transforms.zoom", "params": {"op": "set", "value": "40"}}))["ok"], json!(true));
        assert_eq!(ran(&host).last().unwrap(), r#"run_action transforms transforms.zoom {"op":"set","amount":10.0,"value":40.0}"#);
        assert_eq!(handle_remote(&mut host, &json!({"action": "nope"}))["ok"], json!(false));
        assert!(handle_remote(&mut host, &json!({"action": "camera.torch", "params": {"colour": "red"}}))["text"].as_str().unwrap().contains("no setting 'colour'"));
        host.action_answers.insert("camera.torch".into(), None);
        assert_eq!(handle_remote(&mut host, &json!({"action": "camera.torch"}))["ok"], json!(false));
        assert_eq!(handle_remote(&mut host, &json!({"action": "streaming.start_stop"})), json!({"ok": true, "text": "Streaming: starting"}));
        assert_eq!(handle_remote(&mut host, &json!({"action": "streaming.start_stop", "params": {"mode": "start"}}))["text"], json!("Already streaming"));
        assert_eq!(handle_remote(&mut host, &json!({"action": "streaming.start_stop"}))["text"], json!("Streaming: stopped"));
        assert_eq!(host.calls.last().unwrap(), "stop_all_streams");
        host.can_turn_off = Some((true, String::new()));
        assert_eq!(handle_remote(&mut host, &json!({"action": "streaming.camera", "params": {"mode": "off"}}))["text"], json!("Phone camera: off"));
        assert!(host.camera_off);
        assert_eq!(handle_remote(&mut host, &json!({"action": "window.show_hide"})), json!({"ok": true, "text": "Telescope: shown"}));
        assert_eq!(handle_remote(&mut host, &json!({"action": "window.show_hide"})), json!({"ok": true, "text": "Telescope: hidden"}));
    }

    #[test]
    fn remote_settings_are_checked_not_defaulted() {
        for (params, says) in [(json!({"op": "dwon"}), "op can be up | down | set, not 'dwon'"), (json!({"amount": "lots"}), "amount has to be a number, not 'lots'"),
                               (json!({"op": "set"}), "needs value=")] {
            let (_plugin, mut host, _) = make(json!([]), false, true);
            let reply = handle_remote(&mut host, &json!({"action": "transforms.zoom", "params": params}));
            assert!(reply["ok"] == false && reply["text"].as_str().unwrap().contains(says), "{reply}");
            assert!(ran(&host).is_empty(), "nothing moved: {:?}", ran(&host));
        }
    }

    #[test]
    fn a_crash_in_a_remote_action_still_answers() {
        assert_eq!(answered(|| panic!("boom")), json!({"ok": false, "text": "That failed; Copy diagnostics has the details."}));
        assert_eq!(answered(|| json!({"ok": true})), json!({"ok": true}));
    }

    #[test]
    fn remote_hold_is_refused() {
        let (_plugin, mut host, _) = make(json!([]), false, true);
        let reply = handle_remote(&mut host, &json!({"action": "microphone.mute", "params": {"mode": "hold"}}));
        assert!(reply["ok"] == false && reply["text"].as_str().unwrap().contains("hold"), "{reply}");
        assert!(ran(&host).is_empty());
    }

    #[test]
    fn remote_numbers_must_be_finite() {
        for amount in ["nan", "inf"] {
            let (_plugin, mut host, _) = make(json!([]), false, true);
            let reply = handle_remote(&mut host, &json!({"action": "transforms.zoom", "params": {"op": "up", "amount": amount}}));
            assert!(reply["ok"] == false && reply["text"].as_str().unwrap().contains("has to be a number"), "{reply}");
            assert!(ran(&host).is_empty());
        }
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
        assert_eq!(param_problem(&spec, &params, false), Some("hold needs a key to let go of; use toggle, on or off with --action.".into()));
    }

    #[test]
    fn a_number_out_of_range_is_taken_at_the_nearest_unless_the_caller_wants_it_refused() {
        let spec = ActionSpec {
            id: "camera.iso".into(), label: "ISO".into(), group: "Camera".into(),
            params: vec![json!({"name": "value", "label": "To", "kind": "number", "default": 100, "minimum": 50, "maximum": 3200, "decimals": 0, "suffix": ""})],
            repeats: false, on_release: false, current: None, keywords: String::new(),
        };
        let given = json!({"value": "9000"}).as_object().unwrap().clone();
        assert_eq!(checked_params(&spec, &given, false).unwrap()["value"], json!(3200.0));
        assert_eq!(checked_params(&spec, &given, true).unwrap_err(), "value goes from 50 to 3200, so 9000 is out of range.");
        assert_eq!(checked_params(&spec, &json!({"valeu": 1}).as_object().unwrap().clone(), true).unwrap_err(), "camera.iso has no setting 'valeu'.");
    }

    #[test]
    fn numbers_are_checked_and_listed() {
        let spec = ActionSpec {
            id: "camera.iso".into(), label: "ISO".into(), group: "Camera".into(),
            params: vec![json!({"name": "op", "label": "Does", "kind": "choice", "default": "up", "choices": [["up", "Up"], ["set", "Set to"]]}),
                         json!({"name": "value", "label": "To", "kind": "number", "default": null, "minimum": 50, "maximum": 3200, "suffix": "", "shown_if": ["op", ["set"]]})],
            repeats: true, on_release: false, current: None, keywords: String::new(),
        };
        let params = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(param_problem(&spec, &params(json!({"op": "set", "value": "abc"})), false), Some("value has to be a number, not 'abc'.".into()));
        assert_eq!(param_problem(&spec, &params(json!({"op": "set"})), false), Some("camera.iso needs value=… for that.".into()));
        assert_eq!(param_problem(&spec, &params(json!({"op": "up"})), false), None);
        assert_eq!(full_params(&spec, &params(json!({"op": "set", "value": "400"})))["value"], json!(400.0));
        let text = list_text(&[(Some("camera_control".into()), spec)]);
        assert_eq!(text, "camera.iso  (Camera: ISO)\n    op=up | set  (default up)\n    value=<number 50 to 3200>");
    }

    // Param.clean: round(value, decimals), whose halves go to the even neighbour and which reads the number's exact value
    #[test]
    fn a_number_is_rounded_the_way_pythons_round_does() {
        let number = |decimals: Option<u64>| {
            let mut p = json!({"name": "value", "kind": "number", "default": null, "minimum": 0, "maximum": 1000});
            if let Some(d) = decimals {
                p["decimals"] = json!(d);
            }
            p
        };
        for (given, decimals, want) in [(100.5, Some(0), 100.0), (101.5, Some(0), 102.0), (100.5, None, 100.0), (0.125, Some(2), 0.12),
                                        (0.375, Some(2), 0.38), (2.675, Some(2), 2.67), (1.005, Some(2), 1.0), (7.25, Some(1), 7.2)] {
            assert_eq!(clean_value(&number(decimals), Some(&json!(given))), json!(want), "{given} to {decimals:?} places");
        }
    }

    #[test]
    fn a_bound_that_moves_after_start_is_the_one_a_key_press_cleans_to() {
        let (mut plugin, mut host, _) = make(json!([b("a", "Z", "transforms.zoom", json!({"op": "set", "value": 15}), false)]), false, true);
        plugin.on_key(&mut host, &key("z", false, false));
        assert!(ran(&host)[0].ends_with(r#""value":15.0}"#), "{:?}", ran(&host));
        // Max zoom comes down while Telescope runs (the bounds are read live, as Param.bounds does)
        host.actions[2].1.params[2]["maximum"] = json!(10);
        host.calls.clear();
        plugin.on_key(&mut host, &key("z", false, false));
        assert!(ran(&host)[0].ends_with(r#""value":10.0}"#), "{:?}", ran(&host));
        assert!(handle_remote(&mut host, &json!({"list": true}))["text"].as_str().unwrap().contains("value=<number 0 to 10>"));
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
        let control = props["control"].as_i64().unwrap() as usize;
        assert_eq!(control, row_of(&plugin, "Camera", "Torch"));
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

    /// What a binding with these settings does, as the list shows it.
    fn says(action: &ActionSpec, params: Value) -> String {
        summary(action, &full_params(action, params.as_object().unwrap()))
    }

    #[test]
    fn start_collects_actions_app_ones_first() {
        let (plugin, _host, _) = make(json!([]), false, true);
        let ids: Vec<&str> = plugin.actions.iter().map(|(_, a)| a.id.as_str()).collect();
        assert_eq!(ids, ["streaming.start_stop", "streaming.camera", "window.show_hide", "microphone.mute", "camera.torch", "transforms.zoom"]);
        let own: Vec<String> = own_actions().into_iter().map(|a| a.id).collect();
        assert_eq!(own, ids[..3]);
    }

    #[test]
    fn the_controls_menu_lists_the_saved_shortcuts() {
        let (plugin, _host, _) = make(json!([b("a", "Ctrl+T", "camera.torch", json!({}), false)]), false, true);
        let entries = plugin.menu_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!((entries[0].action.as_str(), entries[0].text.as_str()), ("camera.torch", "Ctrl+T: Torch: switch on or off"));
    }

    #[test]
    fn a_plain_button_has_no_settings_and_a_binding_for_it_reads_as_its_name() {
        let reset = ActionSpec { id: "x.reset".into(), label: "Reset".into(), group: "X".into(), params: Vec::new(), repeats: false,
                                 on_release: false, current: None, keywords: String::new() };
        assert_eq!(says(&reset, json!({})), "Reset");
    }

    #[test]
    fn a_switch_binding_reads_on_from_the_label_unless_the_card_has_words_of_its_own() {
        let torch = actions::toggle_action("t.torch", "Torch", "Camera", "", &[]);
        assert_eq!(says(&torch, json!({"mode": "on"})), "Torch: turn on");
        assert_eq!(says(&torch, json!({})), "Torch: switch on or off");
        assert_eq!(says(&torch, json!({"mode": "hold"})), "Torch: switch while held, back on release");
        let mute = actions::toggle_action("m", "Mute", "Microphone", "", &[("on", "Mute".into()), ("off", "Unmute".into())]);
        assert_eq!(says(&mute, json!({"mode": "on"})), "Mute");
        assert_eq!(says(&mute, json!({"mode": "off"})), "Unmute");
        assert_eq!(says(&mute, json!({"mode": "toggle"})), "Mute: switch on or off");
    }

    #[test]
    fn a_slider_steps_in_shown_units_and_its_bindings_read_in_them() {
        let spec = actions::SliderSpec { id: "c", label: "Compensation", group: "Camera", keywords: "", minimum: -8.0 / 3.0, maximum: 8.0 / 3.0, step: 1.0,
                                decimals: 1, suffix: " EV", has_default: true };
        let action = actions::slider_action(&spec, 0.0);
        let scale = 1.0 / 3.0;
        let go = |p: Value, cur: i64| actions::slider_target(&Value::Object(full_params(&action, p.as_object().unwrap())), cur, scale, -8, 8, Some(0));
        assert_eq!(go(json!({"op": "up", "amount": 1.0}), 0), Some(3));
        assert_eq!(go(json!({"op": "down", "amount": 0.3}), 3), Some(2), "never less than one step");
        assert_eq!(go(json!({"op": "up", "amount": 20}), 2), Some(8), "clamped to the end");
        assert_eq!(go(json!({"op": "set", "value": -1.0}), 8), Some(-3));
        assert_eq!(go(json!({"op": "reset"}), -3), Some(0));
        assert_eq!(says(&action, json!({"op": "down", "amount": 1.0})), "Compensation: decrease by 1.0 EV");
        assert_eq!(says(&action, json!({"op": "up", "amount": 1.0})), "Compensation: increase by 1.0 EV");
        assert_eq!(says(&action, json!({"op": "set", "value": -1})), "Compensation: set to -1.0 EV");
        assert_eq!(says(&action, json!({"op": "set", "value": 1})), "Compensation: set to +1.0 EV", "a range below zero shows its sign");
        assert_eq!(says(&action, json!({"op": "reset"})), "Compensation: reset to default");
        assert_eq!(action.current, Some(0.0));
        let (lo, hi) = (action.params[2]["minimum"].as_f64().unwrap(), action.params[2]["maximum"].as_f64().unwrap());
        assert_eq!(((lo * 100.0).round() / 100.0, (hi * 100.0).round() / 100.0), (-2.67, 2.67));
    }

    #[test]
    fn a_slider_without_a_default_offers_no_reset() {
        let spec = actions::SliderSpec { id: "s", label: "S", group: "G", keywords: "", minimum: 0.0, maximum: 100.0, step: 1.0, decimals: 0, suffix: "", has_default: false };
        let action = actions::slider_action(&spec, 0.0);
        let ops: Vec<&str> = action.params[0]["choices"].as_array().unwrap().iter().map(|c| c[0].as_str().unwrap()).collect();
        assert!(!ops.contains(&"reset"));
    }

    #[test]
    fn a_log_slider_moves_in_stops_and_reads_them_in_the_singular_or_plural() {
        let spec = actions::LogSliderSpec { id: "iso", label: "ISO", group: "Camera", keywords: "", minimum: 50.0, maximum: 6400.0, decimals: 0, suffix: "" };
        let action = actions::log_slider_action(&spec, 400.0);
        assert_eq!(actions::log_slider_target(&json!({"op": "up", "stops": 1}), 400.0, 50.0, 6400.0, 1.0).map(f64::round), Some(800.0));
        assert_eq!(actions::log_slider_target(&json!({"op": "down", "stops": 3}), 800.0, 50.0, 6400.0, 1.0).map(f64::round), Some(100.0));
        assert_eq!(actions::log_slider_target(&json!({"op": "set", "value": 99999}), 100.0, 50.0, 6400.0, 1.0), Some(6400.0));
        assert_eq!(says(&action, json!({"op": "up", "stops": 1})), "ISO: raise by 1 stop");
        assert_eq!(says(&action, json!({"op": "down", "stops": 3})), "ISO: lower by 3 stops");
        assert_eq!(says(&action, json!({"op": "up", "stops": 1.5})), "ISO: raise by 1.5 stops", "1.5 counts as plural");
        assert_eq!(says(&action, json!({"op": "set", "value": 800})), "ISO: set to 800");
    }

    #[test]
    fn a_choice_binding_reads_next_previous_or_the_option_it_picks() {
        let action = actions::choice_action("fps", "FPS", "Out", "", &[("15 fps", "15 fps".into()), ("30 fps", "30 fps".into()), ("60 fps", "60 fps".into())]);
        assert_eq!(says(&action, json!({"op": "next"})), "FPS: next");
        assert_eq!(says(&action, json!({"op": "previous"})), "FPS: previous");
        assert_eq!(says(&action, json!({"op": "set", "option": "30 fps"})), "FPS: 30 fps");
        assert_eq!(says(&action, json!({"op": "set", "option": ""})), "FPS: …");
        let values: Vec<&str> = action.params[1]["choices"].as_array().unwrap().iter().map(|c| c[0].as_str().unwrap()).collect();
        assert_eq!(values, ["15 fps", "30 fps", "60 fps"]);
    }

    #[test]
    fn choices_skip_the_greyed_out_and_stop_at_the_end_without_wrap() {
        let list = |off: &[usize]| -> Vec<(String, bool)> { (0..3).map(|i| (format!("{} fps", [15, 30, 60][i]), !off.contains(&i))).collect() };
        let run = |opts: &[(String, bool)], p: Value, cur: usize, wrap: bool| {
            let options: Vec<actions::Choice> = opts.iter().map(|(v, on)| actions::Choice { value: v, shown: v, enabled: *on }).collect();
            actions::choice_target(&p, &options, Some(cur), wrap).map(|(i, picked)| (opts[i].0.clone(), picked))
        };
        let opts = list(&[2]);
        assert_eq!(run(&opts, json!({"op": "next"}), 0, true), Some(("30 fps".into(), true)));
        assert_eq!(run(&opts, json!({"op": "next"}), 1, true), Some(("15 fps".into(), true)), "60 is greyed out, so it wraps round");
        assert_eq!(run(&opts, json!({"op": "previous"}), 0, true), Some(("30 fps".into(), true)));
        assert_eq!(run(&opts, json!({"op": "set", "option": "15 fps"}), 1, true), Some(("15 fps".into(), true)));
        assert_eq!(run(&opts, json!({"op": "set", "option": "60 fps"}), 0, true), None);
        let two: Vec<(String, bool)> = vec![("a".into(), true), ("b".into(), true)];
        assert_eq!(run(&two, json!({"op": "next"}), 1, false), Some(("b".into(), false)), "stays where it is");
        // Buttons that come and go: none yet, then a row with the first on
        assert_eq!(actions::choice_target(&json!({"op": "next"}), &[], None, true), None);
        let lenses = [actions::Choice { value: "Wide", shown: "Wide", enabled: true }, actions::Choice { value: "Tele", shown: "Tele", enabled: true }];
        assert_eq!(actions::choice_target(&json!({"op": "next"}), &lenses, Some(0), true), Some((1, true)));
        let exclusive = [actions::Choice { value: "Auto", shown: "Auto", enabled: true }, actions::Choice { value: "Manual", shown: "Manual", enabled: true }];
        assert_eq!(actions::choice_target(&json!({"op": "next"}), &exclusive, Some(0), true), Some((1, true)));
        assert_eq!(actions::choice_target(&json!({"op": "set", "option": "Auto"}), &exclusive, Some(1), true), Some((0, true)));
    }

    #[test]
    fn a_preset_binding_reads_as_the_preset_and_one_that_is_gone_stays_named() {
        let apply = ActionSpec {
            id: "presets.apply".into(), label: "Apply a preset".into(), group: "Presets".into(),
            params: vec![json!({"name": "name", "label": "Preset", "kind": "choice", "default": "", "choices": [["Desk", "Desk"]],
                                "dynamic": true, "describe": "Preset: {name}"})],
            repeats: false, on_release: false, current: None, keywords: String::new(),
        };
        assert_eq!(says(&apply, json!({"name": "Desk"})), "Preset: Desk");
        assert_eq!(says(&apply, json!({"name": "Gone"})), "Preset: Gone");
        assert_eq!(says(&apply, json!({})), "Preset: …");
        let cycle = ActionSpec {
            id: "presets.cycle".into(), label: "Next or previous preset".into(), group: "Presets".into(),
            params: vec![json!({"name": "op", "label": "Does", "kind": "choice", "default": "next",
                                "choices": [["next", "Next"], ["previous", "Previous"]], "describe": "Preset: {name}", "lower": true})],
            repeats: false, on_release: false, current: None, keywords: String::new(),
        };
        assert_eq!(says(&cycle, json!({"op": "previous"})), "Preset: previous");
    }

    #[test]
    fn the_editors_search_forgives_typos_and_still_wants_word_starts() {
        let mut mute = ActionSpec { id: "microphone.mute".into(), label: "Mute".into(), group: "Microphone".into(), params: Vec::new(),
                                    repeats: false, on_release: false, current: None, keywords: "silence".into() };
        assert!(matches(&mute, "") && matches(&mute, "mute") && matches(&mute, "mutee") && matches(&mute, "silence"));
        assert!(!matches(&mute, "crop"), "inside microphone is no match");
        mute.keywords.clear();
        assert!(!matches(&mute, "silence"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_desktop_settings_button_opens_plasmas_page_when_the_portal_cant_help() {
        let (mut plugin, mut host, seen) = make(json!([b("a", "F9", "camera.torch", json!({}), true)]), false, true);
        let launched = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let record = launched.clone();
        plugin.launch = Box::new(move |cmd| {
            record.lock().unwrap().push(cmd.iter().map(|s| s.to_string()).collect());
            true
        });
        let applies = seen.lock().unwrap().applied.len();
        assert!(plugin.open_system_settings(&mut host));
        assert_eq!(*launched.lock().unwrap(), vec![vec!["systemsettings".to_string(), "kcm_keys".to_string()]]);
        assert_eq!(seen.lock().unwrap().applied.len(), applies, "nothing was rebound");
        // A desktop without the first program tries the next
        launched.lock().unwrap().clear();
        let tried = launched.clone();
        plugin.launch = Box::new(move |cmd| {
            tried.lock().unwrap().push(cmd.iter().map(|s| s.to_string()).collect());
            cmd[0] == "kcmshell6"
        });
        assert!(plugin.open_system_settings(&mut host));
        assert_eq!(launched.lock().unwrap().len(), 2);
        plugin.launch = Box::new(|_| false);
        assert!(!plugin.open_system_settings(&mut host));
    }
}
