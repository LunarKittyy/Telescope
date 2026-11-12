//! The Presets plugin (plugins/presets.py): named snapshots of the settings of every plugin that is in_presets (the camera,
//! stream output, transforms and so on). They are per phone (lens ids and sizes differ between phones), so the host keeps
//! them with the other per-device settings. Applying one hands each section to its plugin's apply_preset(), which also
//! sends it to the phone while streaming.

use telescope_core::i18n::Fields;
use serde_json::{json, Map, Value};
use telescope_core::event::Event;
use telescope_core::host::Host;
use telescope_core::i18n::{n_, tr};
use telescope_core::names::{clean_name, py_str};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "presets";
const UI: &str = include_str!("../../ui/cards/presets.slint");

/// The keys the first three sections were saved under; any other plugin's section is under its name.
const KEYS: [(&str, &str); 3] = [("camera_control", "camera"), ("stream_output", "stream"), ("transforms", "transforms")];
const MAX_NAME: usize = 40;

/// One preset: its name first, then the section of each plugin it saved.
type Preset = Map<String, Value>;

fn key_of(section: &str) -> &str {
    KEYS.iter().find(|(plugin, _)| *plugin == section).map_or(section, |(_, key)| key)
}

/// A preset name as safe display text (no control characters or < >), capped at MAX_NAME.
pub fn clean(raw: &str) -> String {
    clean_name(raw).chars().take(MAX_NAME).collect::<String>().trim().to_string()
}

/// Keep well-formed entries with a unique non-empty name, in order.
pub fn clean_presets(raw: Option<&Value>) -> Vec<Preset> {
    let mut out: Vec<Preset> = Vec::new();
    for p in raw.and_then(Value::as_array).into_iter().flatten() {
        let Some(p) = p.as_object() else { continue };
        let name = clean(&p.get("name").map(py_str).unwrap_or_default());
        if name.is_empty() || out.iter().any(|e| e.get("name") == Some(&Value::String(name.clone()))) {
            continue;
        }
        let mut entry = Preset::new();
        entry.insert("name".into(), Value::String(name));
        for (key, section) in p {
            // A switched-off plugin's section stays for when it's back
            if key != "name" && section.is_object() {
                entry.insert(key.clone(), section.clone());
            }
        }
        out.push(entry);
    }
    out
}

/// What the header's question is about, while one is open.
#[derive(Clone, Debug, PartialEq)]
enum Asking {
    Nothing,
    /// A name for a new preset, or the new name of `rename`.
    Name { rename: Option<String> },
    /// The name is taken: replace that preset?
    Replace { rename: Option<String>, name: String },
}

pub struct PresetsPlugin {
    presets: Vec<Preset>,
    /// The preset applied last, for Next and Previous.
    last: Option<String>,
    asking: Asking,
    /// set_config has no host: the header and the actions hear the new presets with the next hook that has one.
    pending: bool,
}

impl Default for PresetsPlugin {
    fn default() -> Self {
        PresetsPlugin::new()
    }
}

impl PresetsPlugin {
    pub fn new() -> PresetsPlugin {
        PresetsPlugin { presets: Vec::new(), last: None, asking: Asking::Nothing, pending: false }
    }

    // ── Data ──────────────────────────────────────────────────────────

    pub fn names(&self) -> Vec<String> {
        self.presets.iter().filter_map(|p| p.get("name").and_then(Value::as_str).map(String::from)).collect()
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.presets.iter().position(|p| p.get("name").and_then(Value::as_str) == Some(name))
    }

    pub fn find(&self, name: &str) -> Option<&Preset> {
        self.index(name).map(|i| &self.presets[i])
    }

    /// Snapshot the current settings under name, replacing a preset of the same name in place.
    pub fn save(&mut self, host: &mut dyn Host, name: &str) {
        let name = clean(name);
        if name.is_empty() {
            return;
        }
        let existing = self.index(&name);
        let mut entry = existing.map_or_else(|| Preset::from_iter([("name".to_string(), Value::String(name.clone()))]), |i| self.presets[i].clone());
        for section in host.preset_plugins() {
            if let Some(cfg) = host.plugin_config(&section) {
                entry.insert(key_of(&section).to_string(), cfg);
            }
        }
        match existing {
            Some(i) => self.presets[i] = entry,
            None => self.presets.push(entry),
        }
        self.changed(host);
    }

    pub fn apply(&mut self, host: &mut dyn Host, name: &str) {
        let Some(preset) = self.find(name).cloned() else { return };
        self.last = Some(name.to_string());
        // In registration order: the lens switch goes out before stream output looks up its sizes. A section that
        // won't apply is logged by the host and leaves the others applied.
        for section in host.preset_plugins() {
            if let Some(cfg) = preset.get(key_of(&section)) {
                host.apply_preset(&section, cfg);
            }
        }
        host.schedule_save();
    }

    pub fn rename(&mut self, host: &mut dyn Host, old: &str, new: &str) {
        let new = clean(new);
        let Some(at) = self.index(old) else { return };
        if new.is_empty() || new == old {
            return;
        }
        // Taking over a name takes the other preset's place
        if let Some(clash) = self.index(&new) {
            self.presets.remove(clash);
        }
        let at = self.index(old).unwrap_or(at);
        self.presets[at].insert("name".into(), Value::String(new));
        self.changed(host);
    }

    pub fn delete(&mut self, host: &mut dyn Host, name: &str) {
        if let Some(i) = self.index(name) {
            self.presets.remove(i);
            self.changed(host);
        }
    }

    /// The menu and the shortcuts' list follow the presets, and they're saved.
    fn changed(&mut self, host: &mut dyn Host) {
        self.show_names(host);
        host.refresh_actions();
        host.schedule_save();
    }

    /// A name no preset has: "Preset 3" while there are two.
    pub fn unused_name(&self) -> String {
        let mut n = self.presets.len() + 1;
        while self.find(&tr("Preset {n}").field("n", n.to_string())).is_some() {
            n += 1;
        }
        tr("Preset {n}").field("n", n.to_string())
    }

    /// Whether saving as name has to ask first: another preset has it (renaming to its own name, keep, is fine).
    fn clashes(&self, name: &str, keep: Option<&str>) -> bool {
        let name = clean(name);
        !name.is_empty() && Some(name.as_str()) != keep && self.find(&name).is_some()
    }

    // ── The header ────────────────────────────────────────────────────

    fn fixed_props() -> Value {
        json!({
            "label": tr("Presets"),
            "tip": tr("Saved camera, output, transform, framing and background settings for this phone."),
            "save-text": tr("Save current as…"), "rename-text": tr("Rename"), "delete-text": tr("Delete"),
            "dialog-label": tr("Name:"), "replace-title": tr("Replace preset"), "replace-text": tr("Replace"),
            "cancel-text": tr("Cancel"),
        })
    }

    fn all_props(&self) -> Value {
        let mut all = Self::fixed_props();
        if let Value::Object(o) = &mut all {
            o.insert("names".into(), json!(self.names()));
            o.insert("dialog".into(), json!(""));
        }
        all
    }

    fn show_names(&self, host: &mut dyn Host) {
        host.set_ui_props("header", &json!({"names": self.names()}));
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if std::mem::take(&mut self.pending) {
            self.asking = Asking::Nothing;
            host.set_ui_props("header", &json!({"dialog": ""}));
            self.show_names(host);
            host.refresh_actions();
        }
    }

    fn ask_name(&mut self, host: &mut dyn Host, rename: Option<String>) {
        let (title, text, ok) = match &rename {
            Some(old) => (tr("Rename preset"), old.clone(), tr("Rename")),
            None => (tr("Save preset"), self.unused_name(), tr("Save")),
        };
        self.asking = Asking::Name { rename };
        // Shut first, so asking again (the same question twice) opens it again
        host.set_ui_props("header", &json!({"dialog": ""}));
        host.set_ui_props("header", &json!({"dialog-title": title, "dialog-text": text, "ok-text": ok, "dialog": "name"}));
    }

    fn close_dialog(&mut self, host: &mut dyn Host) {
        self.asking = Asking::Nothing;
        host.set_ui_props("header", &json!({"dialog": ""}));
    }

    /// The name was typed: do it, or ask whether to replace the preset that has it.
    fn name_given(&mut self, host: &mut dyn Host, text: &str) {
        let Asking::Name { rename } = self.asking.clone() else { return };
        if self.clashes(text, rename.as_deref()) {
            let name = clean(text);
            let ask = tr("There's already a preset called \"{name}\". Replace it?").field("name", &name);
            self.asking = Asking::Replace { rename, name };
            host.set_ui_props("header", &json!({"dialog": ""}));
            host.set_ui_props("header", &json!({"confirm-text": ask, "dialog": "replace"}));
            return;
        }
        self.close_dialog(host);
        match rename {
            Some(old) => self.rename(host, &old, text),
            None => self.save(host, text),
        }
    }

    fn replace_answered(&mut self, host: &mut dyn Host, yes: bool) {
        let Asking::Replace { rename, name } = self.asking.clone() else { return };
        self.close_dialog(host);
        if !yes {
            return;
        }
        match rename {
            Some(old) => self.rename(host, &old, &name),
            None => self.save(host, &name),
        }
    }

    // ── Shortcuts ─────────────────────────────────────────────────────

    fn cycle(&mut self, host: &mut dyn Host, op: &str) -> Option<String> {
        let names = self.names();
        if names.is_empty() {
            return None;
        }
        let next = op == "next";
        let at = match self.last.as_ref().and_then(|l| names.iter().position(|n| n == l)) {
            Some(i) => i as i64,
            None => if next { -1 } else { 0 },
        };
        let name = names[(at + if next { 1 } else { -1 }).rem_euclid(names.len() as i64) as usize].clone();
        self.apply(host, &name);
        Some(tr("Preset: {name}").field("name", &name))
    }
}

impl Plugin for PresetsPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Presets").into(),
            about: n_("Saving and loading groups of settings with one click.").into(),
            when_off: n_("Without it there's no Presets button in the header (your saved presets aren't deleted).").into(),
            category: "extras".into(),
            icon: "preset".into(),
            device_local: true,
            header_side: "left".into(),
            ..Default::default()
        }
    }

    fn header_widget(&mut self) -> Option<UiSpec> {
        Some(UiSpec { source: SlintSource::Text(UI.into()), component: "PresetsHeader".into(), props: self.all_props() })
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, _event: &Event) {
        self.catch_up(host);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "header" {
            return;
        }
        self.catch_up(host);
        let text = event.args.first().and_then(Value::as_str).unwrap_or("").to_string();
        match event.callback.as_str() {
            "apply" => self.apply(host, &text),
            "save-clicked" => self.ask_name(host, None),
            "rename-clicked" => self.ask_name(host, Some(text)),
            "delete-clicked" => self.delete(host, &text),
            "name-accepted" => self.name_given(host, &text),
            "replace-answered" => self.replace_answered(host, event.args.first().and_then(Value::as_bool).unwrap_or(false)),
            "dialog-cancelled" => self.close_dialog(host),
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Presets");
        let names: Vec<Value> = self.names().into_iter().map(|n| json!([n, n])).collect();
        let ops = [("next", tr("Next")), ("previous", tr("Previous"))];
        let click = |id: &str, label: String, params: Vec<Value>| ActionSpec {
            id: id.into(), label, group: group.clone(), params, repeats: false, on_release: false, current: None, keywords: String::new(),
        };
        vec![
            click("presets.apply", tr("Apply a preset"), vec![
                json!({"name": "name", "label": tr("Preset"), "kind": "choice", "default": "", "choices": names, "dynamic": true,
                       "describe": tr("Preset: {name}")}),
            ]),
            click("presets.cycle", tr("Next or previous preset"), vec![
                json!({"name": "op", "label": tr("Does"), "kind": "choice", "default": "next",
                       "choices": ops.iter().map(|(v, l)| json!([v, l])).collect::<Vec<_>>(),
                       "describe": tr("Preset: {name}"), "lower": true}),
            ]),
        ]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        match id {
            "presets.apply" => {
                let name = params.get("name").and_then(Value::as_str)?.to_string();
                self.find(&name)?;
                self.apply(host, &name);
                Some(tr("Preset: {name}").field("name", &name))
            }
            "presets.cycle" => {
                let op = params.get("op").and_then(Value::as_str).unwrap_or("next").to_string();
                self.cycle(host, &op)
            }
            _ => None,
        }
    }

    fn get_config(&self) -> Value {
        json!({"presets": self.presets.iter().map(|p| Value::Object(p.clone())).collect::<Vec<_>>()})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.presets = clean_presets(cfg.get("presets"));
        self.pending = true;
        Ok(())
    }
}

#[cfg(test)]
#[path = "presets_tests.rs"]
mod tests;
