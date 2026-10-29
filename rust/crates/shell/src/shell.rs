//! The window's state without the window: the plugins, their cards and settings, banners, folds and saving (app.py's
//! TelescopeWindow minus Qt). The Slint side (window.rs) drains `effects` after each call and shows the result.

use crate::banners::Banners;
use crate::collapse::Collapsing;
use crate::layout::{Panels, Region};
use crate::{profiles, switches};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::panic::{catch_unwind, AssertUnwindSafe};
use telescope_core::config::{merge_for_save, ConfigFile, PluginSection, WindowState};
use telescope_core::event::Event;
use telescope_core::host::{RequestId, View};
use telescope_core::i18n::tr;
use telescope_core::plugin::{load_config_tolerantly, Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, CardSpec, Issue, IssueAction, IssueDoes, TrayAction, UiEvent, UiSpec};
use telescope_core::EventBus;

/// One registered plugin and what it put in the window. `plugin` is out (None) only while one of its hooks runs.
pub struct Slot {
    pub plugin: Option<Box<dyn Plugin>>,
    pub info: PluginInfo,
    pub card: Option<CardSpec>,
    pub header: Option<UiSpec>,
    pub actions: Vec<ActionSpec>,
    pub tray: Vec<TrayAction>,
    /// get_config() right after setup, which a device profile starts from.
    pub defaults: Value,
}

/// Something a plugin asked for that the window (or the stream flow) carries out once the hook returns.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Props { plugin: String, ui: String, props: Value },
    Tray(TrayAction),
    FrameStep(String),
    OpenSettings(String),
    Notify { title: String, body: String, urgent: bool },
    Start { interactive: bool },
    StartAgain(Option<String>),
    Stop(Option<String>),
    StopAll,
    Reconnect,
    PhoneControl { source_id: Option<String>, body: Value },
    CameraOn { on: bool, auto: bool },
    ToggleWindow,
    /// Host::open_url: a link for the browser.
    OpenUrl(String),
    Quit,
    Restart,
    KeepInTray(bool),
    SaveSoon,
    PickSource(Option<String>),
    /// host.schedule: Event::TimerFired for plugin after ms (moves a pending one with the same token).
    Schedule { plugin: String, token: String, ms: u64 },
    CancelSchedule { plugin: String, token: String },
}

/// How streaming stands, as the stream flow last said.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Streams {
    pub streaming: bool,
    pub starting: bool,
    pub restarting: bool,
    pub camera_off: bool,
    pub source_id: Option<String>,
    pub phone_state: Option<Value>,
}

/// Posts waiting for the host thread, and how to wake it.
#[derive(Default)]
pub struct Posts {
    queue: std::sync::Mutex<Vec<(String, String, Value)>>,
    waker: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Posts {
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        *self.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(waker));
    }

    pub fn push(&self, owner: &str, token: &str, data: Value) {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).push((owner.into(), token.into(), data));
        let waker = self.waker.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(w) = waker {
            w();
        }
    }

    fn take(&self) -> Vec<(String, String, Value)> {
        std::mem::take(&mut *self.queue.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// A plugin's handle for posting from its threads.
pub struct PluginPoster {
    pub owner: String,
    pub posts: Arc<Posts>,
}

impl telescope_core::host::Poster for PluginPoster {
    fn post(&self, token: &str, data: Value) {
        self.posts.push(&self.owner, token, data);
    }
}

pub struct State {
    pub slots: Vec<Slot>,
    pub panels: Panels,
    pub collapse: Collapsing,
    pub banners: Banners,
    pub file: ConfigFile,
    pub bus: EventBus,
    pub effects: Vec<Effect>,
    /// The picked source (Connection's selected_device), whose device profile the panels show.
    pub selected: Option<String>,
    pub streams: Streams,
    pub view: View,
    pub output_slots: Map<String, Value>,
    /// Sources only remembered once their settings change (a browser).
    pub changes_only: Vec<String>,
    pub save_failure_notified: bool,
    /// Posts from plugins' threads (host.poster), delivered to their owner after the bus's events.
    pub posts: Arc<Posts>,
    /// Built-in plugins that are off this run (not the optional ones never switched on), by name.
    pub off: Vec<PluginInfo>,
    /// Every built-in plugin, on or off, for the Plugins tab.
    pub builtins: Vec<PluginInfo>,
    /// Added plugins that are on but didn't start: name to why.
    pub problems: BTreeMap<String, String>,
    pub keep_in_tray: bool,
    next_request: u64,
}

impl State {
    pub fn new(file: ConfigFile) -> State {
        State {
            slots: Vec::new(),
            panels: Panels::default(),
            collapse: Collapsing::default(),
            banners: Banners::default(),
            file,
            bus: EventBus::new(),
            effects: Vec::new(),
            selected: None,
            streams: Streams::default(),
            view: View::default(),
            output_slots: Map::new(),
            changes_only: Vec::new(),
            save_failure_notified: false,
            posts: Arc::default(),
            off: Vec::new(),
            builtins: Vec::new(),
            problems: BTreeMap::new(),
            keep_in_tray: false,
            next_request: 1,
        }
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.info.name == name)
    }

    pub fn request_id(&mut self) -> RequestId {
        self.next_request += 1;
        RequestId(self.next_request)
    }

    /// Runs f with plugin i taken out and a host for it; a plugin that panics is logged and the others carry on, as
    /// app.py's _call does with exceptions.
    pub fn call<R>(&mut self, i: usize, f: impl FnOnce(&mut dyn Plugin, &mut crate::host::Ctx) -> R) -> Option<R> {
        let mut plugin = self.slots.get_mut(i)?.plugin.take()?;
        let name = self.slots[i].info.name.clone();
        let result = {
            let mut ctx = crate::host::Ctx { state: self, me: name.clone() };
            catch_unwind(AssertUnwindSafe(|| f(plugin.as_mut(), &mut ctx)))
        };
        self.slots[i].plugin = Some(plugin);
        match result {
            Ok(r) => Some(r),
            Err(_) => {
                log::error!("Plugin {name} failed");
                None
            }
        }
    }

    /// The same hook on every plugin, in registration order.
    pub fn each(&mut self, mut f: impl FnMut(&mut dyn Plugin, &mut crate::host::Ctx)) {
        for i in 0..self.slots.len() {
            self.call(i, &mut f);
        }
    }

    /// register_plugin: sets it up and gives it its place. A plugin that panics while setting up is left out.
    pub fn register(&mut self, plugin: Box<dyn Plugin>) -> Result<(), String> {
        let info = plugin.info();
        let i = self.slots.len();
        self.slots.push(Slot { plugin: Some(plugin), info: info.clone(), card: None, header: None, actions: vec![], tray: vec![], defaults: json!({}) });
        let made = self.call(i, |p, host| {
            p.setup(host);
            let card = p.card();
            let header = p.header_widget();
            let tray = p.tray_actions();
            let defaults = p.get_config();
            (card, header, tray, defaults)
        });
        let Some((card, header, tray, defaults)) = made else {
            self.slots.pop();
            return Err(format!("{} failed while starting", info.name));
        };
        if let Some(card) = &card {
            let region = Region::from_name(&card.region);
            self.panels.place(region, i, info.pinned, &info.category);
        }
        let slot = &mut self.slots[i];
        (slot.card, slot.header, slot.tray, slot.defaults) = (card, header, tray, defaults);
        if let Some(actions) = self.call(i, |p, _| p.actions()) {
            self.slots[i].actions = actions;
        } else {
            log::error!("Plugin {} couldn't list its shortcut actions", info.name);
        }
        Ok(())
    }

    /// load_plugins for the built-in ones: all but those turned off in Plugins, never the required ones.
    pub fn load_builtins(&mut self, builtins: Vec<Box<dyn Plugin>>) {
        let cfg = self.file.load();
        for plugin in builtins {
            let info = plugin.info();
            self.builtins.push(info.clone());
            if switches::starts(&cfg, &info.name, info.required, info.optional) {
                if let Err(e) = self.register(plugin) {
                    log::error!("{e}");
                }
            } else if !info.optional {
                self.off.push(info);
            }
        }
        self.remind_plugins_off(&cfg);
    }

    /// Once per change: which built-in plugins are off, so a card that's gone doesn't look like a bug.
    fn remind_plugins_off(&mut self, cfg: &Value) {
        let unnoticed = switches::unnoticed_off(cfg);
        let titles: Vec<String> = self.builtins.iter().filter(|p| unnoticed.contains(&p.name) && !p.required)
            .map(|p| tr(if p.title.is_empty() { &p.name } else { &p.title })).collect();
        if titles.is_empty() {
            return;
        }
        let n = titles.len();
        let list = crate::texts::and_list(&titles);
        let title = crate::texts::trn("{plugins} is turned off", "{plugins} are turned off", n).replace("{plugins}", &list);
        let text = crate::texts::trn(
            "You turned this off in Plugins. To get it back, open Plugins in Settings and switch it on.",
            "You turned these off in Plugins. To get them back, open Plugins in Settings and switch them on.", n);
        self.banners.show("plugins_off", Issue {
            title, text, kind: "warn".into(), details: String::new(), tell_dismiss: true,
            actions: vec![IssueAction { label: tr("Plugins…"), does: IssueDoes::OpenSettings { tab: "plugins".into() } }],
        });
    }

    fn device_local(&self) -> Vec<String> {
        self.slots.iter().filter(|s| s.info.device_local && !s.info.name.is_empty()).map(|s| s.info.name.clone()).collect()
    }

    /// apply_saved_config: the saved settings into every plugin, then on_plugins_ready, then a banner if the file
    /// had to be set aside.
    pub fn apply_saved_config(&mut self) {
        let mut cfg = self.file.load();
        self.apply_config(&mut cfg);
        self.each(|p, host| p.on_plugins_ready(host));
        if let Some(backup) = self.file.take_reset_notice() {
            let where_ = if backup.is_empty() { String::new() } else { format!(" {}", tr("A copy of the old file is at {path}.").replace("{path}", &backup)) };
            self.banners.show("config", Issue {
                title: tr("Your settings couldn't be read"),
                text: tr("Telescope started with its defaults, so paired phones need pairing again.") + &where_,
                actions: vec![], kind: "warn".into(), details: String::new(), tell_dismiss: false,
            });
        }
    }

    fn apply_config(&mut self, cfg: &mut Value) {
        let window = WindowState::from_config(cfg);
        self.collapse.load(cfg.get("collapsed"));
        self.output_slots = window.output_slots.clone();
        self.selected = window.selected_device.clone();
        if profiles::spread_shared_settings(cfg, &self.device_local()) {
            self.file.save(cfg);
        }
        let global = cfg.get("plugin_configs").and_then(Value::as_object).cloned().unwrap_or_default();
        for i in 0..self.slots.len() {
            let (name, local, defaults) = (self.slots[i].info.name.clone(), self.slots[i].info.device_local, self.slots[i].defaults.clone());
            if name.is_empty() || local {
                continue;
            }
            if let Some(saved) = global.get(&name) {
                self.with_plugin(i, |p| load_config_tolerantly(p, &defaults, saved));
            }
        }
        self.apply_device_profile(window.selected_device.as_deref(), cfg);
    }

    fn with_plugin(&mut self, i: usize, f: impl FnOnce(&mut dyn Plugin)) {
        if let Some(p) = self.slots[i].plugin.as_mut() {
            if catch_unwind(AssertUnwindSafe(|| f(p.as_mut()))).is_err() {
                log::error!("Plugin {} failed", self.slots[i].info.name);
            }
        }
    }

    /// _apply_device_profile: each device-local plugin back to its defaults, then the device's saved settings.
    pub fn apply_device_profile(&mut self, id: Option<&str>, cfg: &Value) {
        let saved = profiles::device_plugin_configs(cfg, id);
        for i in 0..self.slots.len() {
            let (name, local, defaults) = (self.slots[i].info.name.clone(), self.slots[i].info.device_local, self.slots[i].defaults.clone());
            if name.is_empty() || !local {
                continue;
            }
            self.with_plugin(i, |p| {
                let _ = p.set_config(&defaults);
                if let Some(s) = saved.get(&name) {
                    load_config_tolerantly(p, &defaults, s);
                }
            });
        }
    }

    /// _save_profile: the named plugins' device-local settings stored as id's.
    pub fn save_profile(&mut self, id: Option<&str>, names: &[String]) {
        let Some(id) = id.filter(|i| !i.is_empty()) else { return };
        let Ok(mut cfg) = self.file.load_strict() else { return };
        let saved = profiles::device_plugin_configs(&cfg, Some(id));
        let sections: Vec<PluginSection> = (0..self.slots.len())
            .filter(|&i| self.slots[i].info.device_local && names.contains(&self.slots[i].info.name))
            .filter_map(|i| {
                let name = &self.slots[i].info.name;
                self.config_of(i, saved.get(name)).map(|config| PluginSection { plugin: name.clone(), device: Some(id.into()), config })
            }).collect();
        let window = WindowState::from_config(&cfg);
        merge_for_save(&mut cfg, &window, &sections);
        profiles::drop_unchanged(&mut cfg, &self.changes_only, &self.defaults());
        self.file.save(&mut cfg);
    }

    /// _apply_device_profile for the named plugins only: their defaults, then id's saved settings.
    pub fn apply_profile(&mut self, id: Option<&str>, names: &[String]) {
        let cfg = self.file.load();
        let saved = profiles::device_plugin_configs(&cfg, id);
        for i in 0..self.slots.len() {
            let (name, local, defaults) = (self.slots[i].info.name.clone(), self.slots[i].info.device_local, self.slots[i].defaults.clone());
            if !local || !names.contains(&name) {
                continue;
            }
            self.with_plugin(i, |p| {
                let _ = p.set_config(&defaults);
                if let Some(s) = saved.get(&name) {
                    load_config_tolerantly(p, &defaults, s);
                }
            });
        }
    }

    /// A stream hook on the named plugins, in registration order.
    pub fn call_hook(&mut self, names: &[String], hook: &crate::stream_flow::Hook) {
        use crate::stream_flow::Hook;
        for i in 0..self.slots.len() {
            if !names.contains(&self.slots[i].info.name) {
                continue;
            }
            self.call(i, |p, host| match hook {
                Hook::StreamStarting => p.on_stream_starting(host),
                Hook::StreamStart(info) => p.on_stream_start(host, info),
                Hook::StreamStop => p.on_stream_stop(host),
                Hook::FocusGained(info) => p.on_focus_gained(host, info),
                Hook::FocusLost => p.on_focus_lost(host),
                Hook::CameraOff => p.on_camera_off(host),
                Hook::CameraOn => p.on_camera_on(host),
            });
        }
    }

    /// Every plugin's frame step as it is now, in registration order (the pipeline sorts them by stage).
    pub fn pipeline_steps(&mut self) -> Vec<telescope_core::media::StagedStep> {
        let mut steps = Vec::new();
        for i in 0..self.slots.len() {
            let (owner, stage) = (self.slots[i].info.name.clone(), self.slots[i].info.frame_stage);
            let Some(p) = self.slots[i].plugin.as_mut() else { continue };
            match catch_unwind(AssertUnwindSafe(|| p.frame_step())) {
                Ok(Some(step)) => steps.push(telescope_core::media::StagedStep { owner, stage, step }),
                Ok(None) => {}
                Err(_) => log::error!("Plugin {owner} couldn't give its frame step; leaving it out"),
            }
        }
        steps
    }

    /// _config_of: a plugin's settings, or what was saved before when it gives something the file can't hold.
    fn config_of(&self, i: usize, saved: Option<&Value>) -> Option<Value> {
        let p = self.slots[i].plugin.as_ref()?;
        match catch_unwind(AssertUnwindSafe(|| p.get_config())) {
            Ok(v @ Value::Object(_)) => Some(v),
            _ => {
                log::error!("Plugin {} gave settings that can't be saved; keeping the saved ones", self.slots[i].info.name);
                saved.cloned()
            }
        }
    }

    /// Every device-local plugin's settings as `id`'s.
    fn device_sections(&self, cfg: &Value, id: &str) -> Vec<PluginSection> {
        let saved = profiles::device_plugin_configs(cfg, Some(id));
        (0..self.slots.len()).filter(|&i| self.slots[i].info.device_local && !self.slots[i].info.name.is_empty())
            .filter_map(|i| {
                let name = &self.slots[i].info.name;
                self.config_of(i, saved.get(name)).map(|config| PluginSection { plugin: name.clone(), device: Some(id.into()), config })
            }).collect()
    }

    /// save_now. False when the file couldn't be read (saving over it would lose every other phone's settings) or
    /// written; the window tries again shortly.
    pub fn save_now(&mut self) -> bool {
        let Ok(mut cfg) = self.file.load_strict() else {
            log::error!("Couldn't read the settings file; not saving over it");
            return false;
        };
        let global = cfg.get("plugin_configs").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut sections: Vec<PluginSection> = (0..self.slots.len())
            .filter(|&i| !self.slots[i].info.device_local && !self.slots[i].info.name.is_empty())
            .filter_map(|i| {
                let name = self.slots[i].info.name.clone();
                self.config_of(i, global.get(&name)).map(|config| PluginSection { plugin: name, device: None, config })
            }).collect();
        if let Some(id) = self.selected.clone() {
            sections.extend(self.device_sections(&cfg, &id));
        }
        let window = WindowState { selected_device: self.selected.clone(), collapsed: self.collapse.keys(), output_slots: self.output_slots.clone() };
        merge_for_save(&mut cfg, &window, &sections);
        profiles::drop_unchanged(&mut cfg, &self.changes_only, &self.defaults());
        if self.file.save(&mut cfg) {
            self.save_failure_notified = false;
            true
        } else {
            if !self.save_failure_notified {
                self.save_failure_notified = true;
                log::error!("Failed to save settings");
                self.effects.push(Effect::Notify {
                    title: tr("Telescope - Save failed"),
                    body: tr("Could not save settings. Check disk space and permissions."),
                    urgent: true,
                });
            }
            false
        }
    }

    fn defaults(&self) -> Map<String, Value> {
        self.slots.iter().filter(|s| !s.info.name.is_empty()).map(|s| (s.info.name.clone(), s.defaults.clone())).collect()
    }

    /// switch_device: the old device's settings saved, the new one's applied, and a running stream started again
    /// on the new one (by the stream flow, from the effects).
    pub fn switch_device(&mut self, prev: Option<&str>, new: Option<&str>) {
        let loaded = self.file.load_strict().ok();
        if let Some(mut cfg) = loaded.clone() {
            if let Some(prev) = prev.filter(|p| !p.is_empty()) {
                let sections = self.device_sections(&cfg, prev);
                let window = WindowState::from_config(&cfg);
                merge_for_save(&mut cfg, &window, &sections);
            }
            profiles::drop_unchanged(&mut cfg, &self.changes_only, &self.defaults());
            cfg["selected_device"] = new.map_or(Value::Null, |n| json!(n));
            self.file.save(&mut cfg);
        }
        let was_streaming = self.streams.streaming || self.streams.starting;
        if was_streaming {
            self.effects.push(Effect::StopAll);
        }
        self.selected = new.map(String::from);
        let cfg = self.file.load();
        self.apply_device_profile(new, &cfg);
        self.bus.emit(Event::DeviceChanged { source_id: new.unwrap_or("").to_string() });
        if was_streaming {
            self.effects.push(Effect::Start { interactive: true });
        }
    }

    pub fn forget_device_settings(&mut self, id: &str) {
        self.output_slots.remove(id);
        if let Ok(mut cfg) = self.file.load_strict() {
            if profiles::forget_device(&mut cfg, id) {
                self.file.save(&mut cfg);
            }
        }
    }

    pub fn move_device_settings(&mut self, old: &str, new: &str) {
        if let Some(v) = self.output_slots.remove(old) {
            self.output_slots.insert(new.to_string(), v);
        }
        if let Ok(mut cfg) = self.file.load_strict() {
            profiles::move_device(&mut cfg, old, new);
            self.file.save(&mut cfg);
        }
    }

    /// Hands every queued event to every plugin, in order; events emitted while delivering go after. Then each post
    /// from a plugin's threads goes to that plugin alone.
    pub fn deliver_events(&mut self) {
        loop {
            while let Some(event) = self.bus.next() {
                self.each(|p, host| p.on_event(host, &event));
            }
            let posts = self.posts.take();
            if posts.is_empty() {
                break;
            }
            for (owner, token, data) in posts {
                self.deliver_to(&owner, &Event::Posted { owner: owner.clone(), token, data });
            }
        }
    }

    /// An event only plugin `owner` hears (a timer it scheduled, a post from its threads); dropped once it's gone.
    pub fn deliver_to(&mut self, owner: &str, event: &Event) {
        if let Some(i) = self.index(owner) {
            self.call(i, |p, host| p.on_event(host, event));
        }
    }

    /// A callback from one of plugin `name`'s pieces of UI.
    pub fn ui_event(&mut self, name: &str, event: UiEvent) {
        if let Some(i) = self.index(name) {
            self.call(i, |p, host| p.on_ui(host, &event));
            self.deliver_events();
        }
    }

    /// A plugin's current settings, if it's running.
    pub fn plugin_config(&self, name: &str) -> Option<Value> {
        let i = self.index(name)?;
        self.slots[i].plugin.as_ref().map(|p| p.get_config())
    }

    /// The built-in plugins off this run, the way the Plugins tab and diagnostics count them.
    pub fn plugins_off(&self) -> &[PluginInfo] {
        &self.off
    }

    /// The plugins that have a card, in registration order (the index the window's p<n> ids use).
    pub fn cards(&self) -> Vec<usize> {
        (0..self.slots.len()).filter(|&i| self.slots[i].card.is_some()).collect()
    }

    /// Diagnostics' window lines (diagnostics_report's state before the plugins' own).
    pub fn diagnostics(&self) -> Vec<(String, String)> {
        let mut lines = vec![("Streaming".to_string(),
                              if !self.streams.streaming { "no".into() } else if self.streams.camera_off { "yes, camera off".into() } else { "yes".into() })];
        if !self.off.is_empty() {
            lines.push(("Plugins off".into(), self.off.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")));
        }
        for i in 0..self.slots.len() {
            if let Some(p) = self.slots[i].plugin.as_ref() {
                match catch_unwind(AssertUnwindSafe(|| p.diagnostics())) {
                    Ok(more) => lines.extend(more),
                    Err(_) => lines.push((self.slots[i].info.name.clone(), "couldn't read (panic)".into())),
                }
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::plugin::ConfigRejected;
    use telescope_core::testing::scratch_dir;

    struct Stub {
        info: PluginInfo,
        config: Value,
        applied: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    }

    fn stub(name: &str, config: Value, local: bool) -> (Box<dyn Plugin>, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        let applied = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let info = PluginInfo { name: name.into(), device_local: local, ..Default::default() };
        (Box::new(Stub { info, config, applied: applied.clone() }), applied)
    }

    impl Plugin for Stub {
        fn info(&self) -> PluginInfo {
            self.info.clone()
        }
        fn get_config(&self) -> Value {
            self.config.clone()
        }
        fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
            if cfg.get("zoom").is_some_and(|z| !z.is_number()) {
                return Err(ConfigRejected("zoom".into()));
            }
            self.applied.lock().unwrap().push(cfg.clone());
            let mut merged = self.config.as_object().cloned().unwrap_or_default();
            merged.extend(cfg.as_object().cloned().unwrap_or_default());
            self.config = Value::Object(merged);
            Ok(())
        }
    }

    fn state() -> State {
        State::new(ConfigFile::at(scratch_dir("shell").join("telescope_config.json")))
    }

    #[test]
    fn save_config_separates_global_and_device_local_plugins() {
        let mut s = state();
        s.register(stub("setup", json!({"canvas": "auto"}), false).0).unwrap();
        s.register(stub("transforms", json!({"zoom": 2}), true).0).unwrap();
        s.selected = Some("PhoneA".into());
        assert!(s.save_now());
        let cfg = s.file.load();
        assert_eq!(cfg["selected_device"], "PhoneA");
        assert_eq!(cfg["plugin_configs"], json!({"setup": {"canvas": "auto"}}));
        assert_eq!(cfg["devices"]["PhoneA"]["plugin_configs"], json!({"transforms": {"zoom": 2}}));
    }

    #[test]
    fn save_config_without_a_picked_device_skips_device_profiles() {
        let mut s = state();
        s.register(stub("transforms", json!({"zoom": 2}), true).0).unwrap();
        s.save_now();
        assert_eq!(s.file.load()["devices"], json!({}));
    }

    #[test]
    fn apply_device_profile_resets_defaults_before_saved_values() {
        let mut s = state();
        let (p, applied) = stub("transforms", json!({"zoom": 1, "pan_x": 0}), true);
        s.register(p).unwrap();
        let cfg = json!({"devices": {"Phone": {"plugin_configs": {"transforms": {"zoom": 3}}}}});
        s.apply_device_profile(Some("Phone"), &cfg);
        assert_eq!(*applied.lock().unwrap(), vec![json!({"zoom": 1, "pan_x": 0}), json!({"zoom": 3})]);
        assert_eq!(s.plugin_config("transforms").unwrap(), json!({"zoom": 3, "pan_x": 0}));
    }

    #[test]
    fn apply_device_profile_none_uses_defaults_only() {
        let mut s = state();
        let (p, applied) = stub("monitoring", json!({"battery_alert": 20}), true);
        s.register(p).unwrap();
        s.apply_device_profile(None, &json!({}));
        assert_eq!(*applied.lock().unwrap(), vec![json!({"battery_alert": 20})]);
    }

    #[test]
    fn switch_device_saves_the_old_profile_and_applies_the_new_one() {
        let mut s = state();
        let (p, _) = stub("transforms", json!({"zoom": 2}), true);
        s.register(p).unwrap();
        let mut cfg = s.file.load();
        cfg["devices"] = json!({"B": {"plugin_configs": {"transforms": {"zoom": 7}}}});
        s.file.save(&mut cfg);
        s.selected = Some("A".into());
        s.streams.streaming = true;
        s.switch_device(Some("A"), Some("B"));
        let cfg = s.file.load();
        assert_eq!(cfg["devices"]["A"]["plugin_configs"]["transforms"], json!({"zoom": 2}));
        assert_eq!(cfg["selected_device"], "B");
        assert_eq!(s.plugin_config("transforms").unwrap()["zoom"], 7);
        assert_eq!(s.effects, vec![Effect::StopAll, Effect::Start { interactive: true }]);
        assert!(matches!(s.bus.next(), Some(Event::DeviceChanged { source_id }) if source_id == "B"));
    }

    #[test]
    fn a_saved_config_that_wont_load_keeps_the_fields_that_do() {
        let mut s = state();
        s.register(stub("setup", json!({"zoom": 1, "canvas": "auto"}), false).0).unwrap();
        let mut cfg = s.file.load();
        cfg["plugin_configs"]["setup"] = json!({"zoom": "big", "canvas": "1080p"});
        s.file.save(&mut cfg);
        s.apply_saved_config();
        assert_eq!(s.plugin_config("setup").unwrap(), json!({"zoom": 1, "canvas": "1080p"}));
    }

    #[test]
    fn folds_are_saved_and_come_back() {
        let mut s = state();
        s.collapse.set_folded("camera_control/Camera", true);
        s.save_now();
        let mut t = State::new(ConfigFile::at(s.file.path()));
        t.apply_saved_config();
        assert!(t.collapse.is_folded("camera_control/Camera"));
    }

    #[test]
    fn a_built_in_plugin_switched_off_isnt_loaded_and_the_window_says_so_once() {
        let mut s = state();
        let mut cfg = s.file.load();
        switches::set_builtin_off(&mut cfg, "notes", true);
        s.file.save(&mut cfg);
        let mut notes = stub("notes", json!({}), false).0;
        let _ = &mut notes;
        s.load_builtins(vec![stub("setup", json!({}), false).0, notes]);
        assert!(s.index("notes").is_none() && s.index("setup").is_some());
        assert_eq!(s.plugins_off().len(), 1);
        assert!(s.banners.issue("plugins_off").is_some());
    }

    #[test]
    fn a_plugin_that_panics_doesnt_stop_the_others() {
        struct Boom;
        impl Plugin for Boom {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "boom".into(), ..Default::default() }
            }
            fn on_event(&mut self, _h: &mut dyn telescope_core::Host, _e: &Event) {
                panic!("boom");
            }
        }
        let mut s = state();
        s.register(Box::new(Boom)).unwrap();
        s.register(stub("after", json!({}), false).0).unwrap();
        s.bus.emit(Event::StreamStopped);
        s.deliver_events();
        assert!(s.slots.iter().all(|slot| slot.plugin.is_some()));
    }

    struct Hears(String, Arc<std::sync::Mutex<Vec<String>>>);

    impl Plugin for Hears {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: self.0.clone(), ..PluginInfo::default() }
        }
        fn on_event(&mut self, host: &mut dyn telescope_core::host::Host, event: &Event) {
            match event {
                Event::Posted { token, .. } => self.1.lock().unwrap().push(format!("{} {token}", self.0)),
                Event::StreamStopped => {
                    if let Some(poster) = host.poster() {
                        std::thread::spawn(move || poster.post("from_thread", json!({}))).join().unwrap();
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn a_post_from_a_plugins_thread_reaches_only_that_plugin() {
        let mut s = state();
        let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
        s.register(Box::new(Hears("connection".into(), heard.clone()))).unwrap();
        s.register(Box::new(Hears("other".into(), heard.clone()))).unwrap();
        s.posts.push("connection", "resolved", json!({"check_id": 3}));
        s.posts.push("gone", "dropped", json!({}));
        s.deliver_events();
        assert_eq!(*heard.lock().unwrap(), ["connection resolved"]);
        heard.lock().unwrap().clear();
        s.bus.emit(Event::StreamStopped);
        s.deliver_events(); // both post from a thread; each hears its own
        assert_eq!(*heard.lock().unwrap(), ["connection from_thread", "other from_thread"]);
    }

    #[test]
    fn a_plugins_link_goes_to_the_browser() {
        let mut s = state();
        s.register(Box::new(Hears("microphone".into(), Arc::default()))).unwrap();
        s.call(0, |_, host| telescope_core::host::HostShell::open_url(host, "https://vb-audio.com/Cable/"));
        assert!(s.effects.iter().any(|e| matches!(e, Effect::OpenUrl(u) if u == "https://vb-audio.com/Cable/")));
    }

}
