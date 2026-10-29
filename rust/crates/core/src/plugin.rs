//! The plugin trait (TelescopePlugin in plugin.py): built-in plugins implement it in Rust, the plugin host implements it for Lua and WASM ones.

use crate::event::Event;
use crate::frame::{FrameStage, FrameStep};
use crate::host::Host;
use crate::ui::{ActionSpec, CardSpec, SettingsSection, TrayAction, UiEvent, UiSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The tabs of the settings window, in order: (id, name).
pub const SETTINGS_TABS: &[(&str, &str)] = &[
    ("general", "General"),
    ("streaming", "Streaming"),
    ("video", "Video"),
    ("audio", "Audio"),
    ("devices", "Virtual camera & phone"),
    ("shortcuts", "Keyboard shortcuts"),
    ("plugins", "Plugins"),
    ("about", "Updates & About"),
];

/// The plugin groups, in order: (id, name). An id that isn't here goes under Extras.
pub const PLUGIN_CATEGORIES: &[(&str, &str)] = &[
    ("camera", "Camera"),
    ("look", "Look"),
    ("vision", "People"),
    ("preview", "Preview tools"),
    ("on_screen", "On screen"),
    ("sound", "Sound"),
    ("streaming", "Streaming and devices"),
    ("extras", "Extras"),
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
    fn on_ui(&mut self, _host: &mut dyn Host, _event: &UiEvent) {}
    /// A banner button with IssueDoes::Plugin, or "dismiss" for an Issue with tell_dismiss.
    fn on_issue_action(&mut self, _host: &mut dyn Host, _key: &str, _id: &str) {}

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
    /// A few ("Label", "value") lines for Copy diagnostics. No tokens, addresses or names.
    fn diagnostics(&self) -> Vec<(String, String)> {
        Vec::new()
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
    fn an_unknown_frame_stage_counts_as_effects() {
        let info: PluginInfo = serde_json::from_value(json!({"name": "x", "frame_stage": "sparkles"})).unwrap();
        assert_eq!(info.frame_stage, FrameStage::Effects);
    }
}
