//! The plugin trait (TelescopePlugin in plugin.py): built-in plugins implement it in Rust, the plugin host implements it for Lua and WASM ones.

use crate::event::Event;
use crate::frame::{FrameStage, FrameStep};
use crate::host::Host;
use crate::i18n::n_;
use crate::ui::{ActionSpec, CardSpec, KeyInput, SettingsSection, TrayAction, UiEvent, UiSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The tabs of the settings window, in order: (id, name).
pub const SETTINGS_TABS: &[(&str, &str)] = &[
    ("general", n_("General")),
    ("streaming", n_("Streaming")),
    ("video", n_("Video")),
    ("audio", n_("Audio")),
    ("devices", n_("Virtual camera & phone")),
    ("shortcuts", n_("Keyboard shortcuts")),
    ("plugins", n_("Plugins")),
    ("about", n_("Updates & About")),
];

/// The plugin groups, in order: (id, name). An id that isn't here goes under Extras.
pub const PLUGIN_CATEGORIES: &[(&str, &str)] = &[
    ("camera", n_("Camera")),
    ("look", n_("Look")),
    ("vision", n_("People")),
    ("preview", n_("Preview tools")),
    ("on_screen", n_("On screen")),
    ("sound", n_("Sound")),
    ("streaming", n_("Streaming and devices")),
    ("extras", n_("Extras")),
];

/// What a plugin is, as plain data: the class attributes of TelescopePlugin. A Lua or WASM plugin gives it in its plugin.json.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginInfo {
    /// Its id: where its settings are saved, and what the Plugins tab knows it by.
    pub name: String,
    /// What the Plugins tab calls it, in English (the window translates it).
    pub title: String,
    /// One plain sentence for the Plugins tab: what it does.
    pub about: String,
    /// What goes away while it's turned off.
    pub when_off: String,
    /// Telescope can't work without it (only Connection).
    pub required: bool,
    /// A built-in one that's off until switched on in Plugins.
    pub optional: bool,
    /// It uses person masks or people: switching it on downloads the vision pack first.
    pub needs_vision: bool,
    /// An id from PLUGIN_CATEGORIES.
    pub category: String,
    pub icon: String,
    /// Its card stays at the top of its column, above the groups.
    pub pinned: bool,
    /// Presets save its get_config() and bring it back through apply_preset().
    pub in_presets: bool,
    /// Each phone (or other source) gets its own copy of its settings.
    pub device_local: bool,
    #[serde(deserialize_with = "stage_or_effects")]
    pub frame_stage: FrameStage,
    /// With several streams, hear about the one the panels show (false: the main one, like the mic).
    pub follows_focus: bool,
    /// Where the header widget goes: "left" (with the phone picker) or "right".
    pub header_side: String,
}

/// A frame_stage that names no stage counts as "effects", like frame_stage_index() in Python.
fn stage_or_effects<'de, D: serde::Deserializer<'de>>(d: D) -> Result<FrameStage, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(d)?).unwrap_or_default())
}

impl Default for PluginInfo {
    fn default() -> Self {
        PluginInfo {
            name: String::new(),
            title: String::new(),
            about: String::new(),
            when_off: String::new(),
            required: false,
            optional: false,
            needs_vision: false,
            category: "extras".into(),
            icon: "plug".into(),
            pinned: false,
            in_presets: false,
            device_local: false,
            frame_stage: FrameStage::Effects,
            follows_focus: true,
            header_side: "left".into(),
        }
    }
}

/// A stream a plugin hears about, in place of on_stream_start's (stream_url, ctrl); control goes through Host::phone_control.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub source_id: String,
    pub url: String,
    /// The same for as long as this stream runs, through a hand-over that changes its url, and new when it starts again
    /// (what Python's ctrl object tells apart).
    #[serde(default)]
    pub session: u64,
}

/// A set_config that couldn't take what it was given; the host then loads what fields it can (load_config_tolerantly).
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigRejected(pub String);

/// One plugin. Every method gets the host it may call; nothing is shared between plugins except through it and the bus.
pub trait Plugin: Send {
    fn info(&self) -> PluginInfo;

    /// Plugins are set up one at a time; state others follow is announced again from on_plugins_ready.
    fn setup(&mut self, _host: &mut dyn Host) {}
    /// Every plugin is set up and the saved settings are in: emit again any state other plugins follow.
    /// A device's settings just went in through set_config, which has no host: the moment to tell the host what they change
    /// (the view), before the stream they belong to hears it's on the panels.
    fn on_profile_applied(&mut self, _host: &mut dyn Host) {}
    fn on_plugins_ready(&mut self, _host: &mut dyn Host) {}
    /// App is quitting: stop background services the plugin started.
    fn shutdown(&mut self, _host: &mut dyn Host) {}

    /// Every event on the bus, its own included. A Lua or WASM adapter filters before crossing over.
    fn on_event(&mut self, _host: &mut dyn Host, _event: &Event) {}

    fn card(&mut self) -> Option<CardSpec> {
        None
    }
    fn header_widget(&mut self) -> Option<UiSpec> {
        None
    }
    /// Asked for once when the settings window first opens.
    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        Vec::new()
    }
    /// Asked for once at startup; kept up to date with HostUi::update_tray_action.
    fn tray_actions(&mut self) -> Vec<TrayAction> {
        Vec::new()
    }
    fn on_tray_action(&mut self, _host: &mut dyn Host, _id: &str) {}
    /// Asked for once, after card().
    fn actions(&mut self) -> Vec<ActionSpec> {
        Vec::new()
    }
    /// Does an action; what to say about it ("Microphone: muted"), or None when it can't right now.
    fn run_action(&mut self, _host: &mut dyn Host, _id: &str, _params: &Value) -> Option<String> {
        None
    }
    /// A key held for an ActionSpec with on_release came back up.
    fn release_action(&mut self, _host: &mut dyn Host, _id: &str, _params: &Value) {}
    /// A key pressed or let go in one of Telescope's windows, before the control with focus sees it; true keeps it from
    /// that control (Shortcuts' key filter).
    fn on_key(&mut self, _host: &mut dyn Host, _key: &KeyInput) -> bool {
        false
    }
    /// Keys held in Telescope's windows won't come back up there: it lost focus.
    fn on_keys_lost(&mut self, _host: &mut dyn Host) {}
    fn on_ui(&mut self, _host: &mut dyn Host, _event: &UiEvent) {}
    /// A banner button with IssueDoes::Plugin, or "dismiss" for an Issue with tell_dismiss.
    fn on_issue_action(&mut self, _host: &mut dyn Host, _key: &str, _id: &str) {}

    /// At Start, before source_id gets ready (None: the picked one): whether the virtual camera can take a stream. False
    /// ends this Start, and this plugin has said why or is asking the person, who can start again from there.
    fn ensure_virtual_camera(&mut self, _host: &mut dyn Host, _interactive: bool, _source_id: Option<&str>) -> bool {
        true
    }
    /// (width, height, fps) a stream's worker asks for, from the plugin that shapes the stream (Stream output).
    fn stream_params(&self) -> Option<(Option<u32>, Option<u32>, u32)> {
        None
    }
    /// What the phone should open at when a stream starts, from the plugin that shapes the stream.
    fn opening(&self) -> Option<crate::link::StreamRequest> {
        None
    }
    /// A stream is about to open the virtual camera; anything holding it while idle lets go now.
    fn on_stream_starting(&mut self, _host: &mut dyn Host) {}
    /// The stream this plugin hears about started, or came back after a drop.
    fn on_stream_start(&mut self, _host: &mut dyn Host, _stream: &StreamInfo) {}
    /// The stream this plugin hears about ended (or stops to start again: host.is_restarting()).
    fn on_stream_stop(&mut self, _host: &mut dyn Host) {}
    /// Moved onto a stream that was already running. By default treated like on_stream_start.
    fn on_focus_gained(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.on_stream_start(host, stream);
    }
    /// Moved off a stream that carries on without it. By default treated like on_stream_stop.
    fn on_focus_lost(&mut self, host: &mut dyn Host) {
        self.on_stream_stop(host);
    }
    /// The stream carries on with the camera off: no frames until on_camera_on.
    fn on_camera_off(&mut self, _host: &mut dyn Host) {}
    fn on_camera_on(&mut self, _host: &mut dyn Host) {}
    fn on_phone_state(&mut self, _host: &mut dyn Host, _state: &Value) {}

    /// process_frame frozen at the current settings; asked for again after HostUi::refresh_frame_step. None: frames pass untouched.
    fn frame_step(&mut self) -> Option<FrameStep> {
        None
    }

    fn get_config(&self) -> Value {
        Value::Object(Map::new())
    }
    fn set_config(&mut self, _cfg: &Value) -> Result<(), ConfigRejected> {
        Ok(())
    }
    /// Load settings from a preset; plugins that drive the phone also send them while streaming.
    fn apply_preset(&mut self, _host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.set_config(cfg)
    }
    /// The settings a tool outside the window (the MCP server) may list and change, with their ranges. They are keys of get_config(),
    /// and a change goes in through apply_preset() so the plugin's own checks apply. Nothing by default.
    fn settings(&self) -> Vec<crate::settings::SettingSpec> {
        Vec::new()
    }
    /// A few ("Label", "value") lines for Copy diagnostics. No tokens, addresses or names.
    fn diagnostics(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// What a plugin following the panels keeps about a stream they moved away from while it streams on, so coming back to it
/// picks up where it was instead of starting over (StreamsBehind in plugin.py). A stream is told apart by its
/// StreamInfo::session, as the phone's ctrl object was.
#[derive(Clone, Debug)]
pub struct StreamsBehind<T> {
    kept: std::collections::HashMap<Option<String>, (u64, T)>,
}

impl<T> Default for StreamsBehind<T> {
    fn default() -> Self {
        StreamsBehind { kept: std::collections::HashMap::new() }
    }
}

impl<T> StreamsBehind<T> {
    /// From on_focus_lost: `value` is kept for the stream the panels are leaving (`stream` its session, None when it has none).
    pub fn keep(&mut self, host: &dyn crate::host::HostStreams, stream: Option<u64>, value: T) {
        let sid = host.focused_source_id();
        match stream {
            Some(session) => {
                self.kept.insert(sid, (session, value));
            }
            None => {
                self.kept.remove(&sid);
            }
        }
    }

    /// From on_focus_gained: what keep() held for this stream, or None if it has started again since.
    pub fn take(&mut self, host: &dyn crate::host::HostStreams, stream: u64) -> Option<T> {
        let kept = self.kept.remove(&host.focused_source_id())?;
        (stream == kept.0).then_some(kept.1)
    }

    /// From on_stream_stop: the shown stream ended, so nothing about it is kept (and nothing at all once idle).
    pub fn drop_shown(&mut self, host: &dyn crate::host::HostStreams) {
        self.kept.remove(&host.focused_source_id());
        if !host.is_streaming() {
            self.kept.clear();
        }
    }
}

/// _set_plugin_config: load cfg; if the plugin rejects it, keep the fields that load on their own and the defaults for the rest.
pub fn load_config_tolerantly(plugin: &mut dyn Plugin, defaults: &Value, cfg: &Value) {
    let Err(why) = plugin.set_config(cfg) else { return };
    log::warn!("Saved settings for {} couldn't be loaded ({}); keeping what does load", plugin.info().name, why.0);
    let mut good = defaults.as_object().cloned().unwrap_or_default();
    for (key, value) in cfg.as_object().into_iter().flatten() {
        let mut trial = good.clone();
        trial.insert(key.clone(), value.clone());
        if plugin.set_config(&Value::Object(trial)).is_ok() {
            good.insert(key.clone(), value.clone());
        } else {
            log::warn!("Saved {} setting {key:?} couldn't be loaded; using its default", plugin.info().name);
        }
    }
    let _ = plugin.set_config(&Value::Object(good));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct Plain(Vec<String>);

    impl Plugin for Plain {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: "plain".into(), ..Default::default() }
        }
        fn on_stream_start(&mut self, _host: &mut dyn Host, stream: &StreamInfo) {
            self.0.push(format!("start {}", stream.url));
        }
        fn on_stream_stop(&mut self, _host: &mut dyn Host) {
            self.0.push("stop".into());
        }
    }

    #[test]
    fn the_focus_hooks_default_to_the_stream_hooks() {
        let (mut plain, mut host) = (Plain::default(), crate::testing::FakeHost::new());
        plain.on_focus_gained(&mut host, &StreamInfo { source_id: "p".into(), url: "url".into(), session: 1 });
        plain.on_focus_lost(&mut host);
        assert_eq!(plain.0, ["start url", "stop"]);
    }

    #[derive(Default)]
    struct Picky {
        cfg: Value,
    }

    impl Plugin for Picky {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: "picky".into(), ..Default::default() }
        }
        fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
            if cfg.get("size").is_some_and(|s| !s.is_u64()) {
                return Err(ConfigRejected("size".into()));
            }
            self.cfg = cfg.clone();
            Ok(())
        }
    }

    #[test]
    fn one_bad_field_keeps_the_rest() {
        let mut p = Picky::default();
        load_config_tolerantly(&mut p, &json!({"size": 3, "on": false}), &json!({"size": "big", "on": true, "extra": 1}));
        assert_eq!(p.cfg, json!({"size": 3, "on": true, "extra": 1}));
    }

    #[test]
    fn info_reads_from_plugin_json_with_defaults() {
        let info: PluginInfo = serde_json::from_value(json!({"name": "lower_thirds", "frame_stage": "overlay"})).unwrap();
        assert_eq!(info.frame_stage, FrameStage::Overlay);
        assert_eq!(info.category, "extras");
        assert!(info.follows_focus);
    }

    #[test]
    fn a_stream_left_behind_is_picked_up_where_it_was() {
        let mut host = crate::testing::FakeHost::new();
        host.streaming = true;
        host.focused = Some("a".into());
        let mut behind: StreamsBehind<i32> = StreamsBehind::default();
        behind.keep(&host, Some(1), 7);
        assert_eq!(behind.take(&host, 1), Some(7));
        assert_eq!(behind.take(&host, 1), None, "taken once");
        // It started again in between: a new url, so what was kept doesn't apply
        behind.keep(&host, Some(1), 8);
        assert_eq!(behind.take(&host, 2), None);
        // Nothing to keep without a stream, and nothing kept once idle
        behind.keep(&host, Some(1), 9);
        behind.keep(&host, None, 10);
        assert_eq!(behind.take(&host, 1), None);
        behind.keep(&host, Some(1), 11);
        host.focused = Some("b".into());
        host.streaming = false;
        behind.drop_shown(&host);
        host.focused = Some("a".into());
        assert_eq!(behind.take(&host, 1), None);
    }

    #[test]
    fn an_unknown_frame_stage_counts_as_effects() {
        let info: PluginInfo = serde_json::from_value(json!({"name": "x", "frame_stage": "sparkles"})).unwrap();
        assert_eq!(info.frame_stage, FrameStage::Effects);
    }
}
