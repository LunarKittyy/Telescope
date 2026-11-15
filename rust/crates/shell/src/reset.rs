//! Reset all settings: the file as it is after one, and what the running app does around it.
//! The save trap is closed by rewriting the file with a pure function and dropping every later save until the restart, so nothing writes the old settings back.

use crate::shell::State;
use serde_json::{json, Map, Value};
use std::io;
use telescope_core::config::{self, CONFIG_VERSION};
use telescope_core::settings::{get_path, put_path};
use telescope_core::ui::UiEvent;

/// What a reset keeps of one plugin's settings.
#[derive(Clone, Copy, Debug)]
pub enum Keep {
    /// All of them.
    Whole,
    /// Only these spots (dotted paths, like "sets").
    Only(&'static [&'static str]),
}

/// The one table of what survives a reset, by plugin: paired phones, presets, notes, layouts, the journal and the links and names people typed. Everything else goes back to its default.
pub const KEPT: &[(&str, Keep)] = &[
    ("connection", Keep::Whole),
    ("presets", Keep::Whole),
    ("notes", Keep::Only(&["sets", "current"])),
    ("composite", Keep::Only(&["layouts"])),
    ("mcp", Keep::Only(&["journal"])),
    ("browser_camera", Keep::Only(&["known", "address"])),
    ("on_air", Keep::Only(&["token", "address", "webhook_url"])),
    ("name_tag", Keep::Only(&["name", "pronouns", "second"])),
];

/// The plugin that holds the "Open Telescope when I sign in" switch, which lives in the operating system and not in the settings file.
const STARTUP: &str = "startup";

/// What is left of one plugin's saved settings, None when nothing is.
fn kept_of(section: &Value, keep: Keep) -> Option<Value> {
    match keep {
        Keep::Whole => Some(section.clone()),
        Keep::Only(spots) => {
            let mut out = Value::Object(Map::new());
            for spot in spots {
                if let Some(value) = get_path(section, spot) {
                    put_path(&mut out, spot, value.clone());
                }
            }
            out.as_object().is_some_and(|m| !m.is_empty()).then_some(out)
        }
    }
}

/// The kept parts of a `plugin_configs` object, by plugin.
fn kept_sections(configs: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (plugin, keep) in KEPT {
        if let Some(kept) = configs.and_then(|c| c.get(*plugin)).and_then(|section| kept_of(section, *keep)) {
            out.insert((*plugin).to_string(), kept);
        }
    }
    out
}

/// The settings file after a reset: defaults except what KEPT lists, the phones' entries (reset the same way), the picked phone, the added plugins and each phone's virtual camera.
pub fn reset_file(old: &Value) -> Value {
    let mut out = config::empty();
    if let Some(selected) = old.get("selected_device").filter(|s| s.is_string()) {
        out["selected_device"] = selected.clone();
    }
    out["plugin_configs"] = Value::Object(kept_sections(old.get("plugin_configs")));
    if let Some(devices) = old.get("devices").and_then(Value::as_object) {
        let reset: Map<String, Value> = devices.iter().filter(|(_, entry)| entry.is_object()).map(|(id, entry)| {
            let mut entry = entry.clone();
            if let Some(map) = entry.as_object_mut() {
                let kept = kept_sections(map.get("plugin_configs"));
                map.shift_remove("plugin_configs");
                if !kept.is_empty() {
                    map.insert("plugin_configs".into(), Value::Object(kept));
                }
            }
            (id.clone(), entry)
        }).collect();
        out["devices"] = Value::Object(reset);
    }
    if let Some(slots) = old.get("output_slots").filter(|s| s.is_object()) {
        out["output_slots"] = slots.clone();
    }
    // The switches go back to the standard selection (every plugin on, the optional ones off), the added plugins stay installed
    if let Some(added) = old.pointer("/plugins/added") {
        out["plugins"] = json!({"added": added});
    }
    crate::switches::reset_to_defaults(&mut out);
    out["version"] = json!(CONFIG_VERSION);
    out
}

impl State {
    /// Resets the settings file (after setting a copy aside) and drops every save until the restart the caller does next; undo_reset() if that fails.
    pub fn reset_all_settings(&mut self) -> io::Result<()> {
        let old = self.file.load_strict()?;
        let copy = self.file.copy_aside("before-reset")?;
        self.file.try_save(&mut reset_file(&old))?;
        self.file.discard_saves();
        self.reset_pending = Some(copy);
        Ok(())
    }

    /// The restart didn't happen: the old file is back and saving works again, so the app goes on with the settings it still holds.
    pub fn undo_reset(&mut self) {
        let Some(copy) = self.reset_pending.take() else { return };
        if let Err(e) = self.file.restore_copy(copy.as_deref()) {
            log::error!("Couldn't put the settings back after a reset that didn't restart: {e}");
        }
        self.file.resume_saves();
    }

    /// The restart is on its way: sign-in autostart is not in the file but in the system, so it goes off like the rest (not in a dev profile, whose entry would be the real app's).
    pub fn reset_sign_in(&mut self) {
        if telescope_core::config::dev_profile().is_some() {
            return;
        }
        let off = UiEvent { ui: "sign-in".into(), callback: "open-toggled".into(), args: vec![json!(false)] };
        self.ui_event(STARTUP, off);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
    use telescope_core::plugin::{Plugin, PluginInfo};
    use telescope_core::testing::scratch_dir;

    fn lived_in() -> Value {
        json!({
            "version": 3,
            "selected_device": "phone-a",
            "collapsed": ["card:desk"],
            "bar_pins": {"left": ["fps"], "right": []},
            "output_slots": {"phone-a": 2},
            "plugins": {"off": ["notes"], "on": ["browser_camera"], "noticed": ["notes"], "added": {"mine": "abc"}, "auto_update": true, "check_updates": false},
            "plugin_configs": {
                "connection": {"phones": [{"id": "phone-a", "token": "t0ps3cret"}], "computer_id": "pc-1", "route": "usb"},
                "presets": {"items": [{"name": "Desk"}]},
                "notes": {"sets": ["my notes"], "current": 0, "size": 40, "speed": 3.0},
                "composite": {"on": true, "layout": {"x": 1}, "layouts": [{"name": "Two"}]},
                "mcp": {"access": "read_and_change", "journal": [{"when": 1}]},
                "browser_camera": {"size": "1080p", "fps": 60, "address": "cam.local", "known": {"b1": {"name": "Pixel"}}},
                "on_air": {"token": "a".repeat(32), "address": "tally.local", "webhook_url": "https://hook", "webhook_method": "PUT", "page": false},
                "setup": {"language": "de", "max_zoom": 8},
                "shortcuts": {"bindings": {"mute": "F9"}},
            },
            "devices": {
                "phone-a": {"active_ip": "10.0.0.5", "plugin_configs": {
                    "name_tag": {"on": true, "name": "Luna", "pronouns": "she/her", "second": "Streamer", "style": "bold"},
                    "transforms": {"zoom": 2.5},
                    "presets": {"items": [{"name": "Phone preset"}]},
                }},
                "phone-b": {"plugin_configs": {"transforms": {"zoom": 1.5}}},
            },
        })
    }

    #[test]
    fn a_reset_file_keeps_what_the_table_says_and_nothing_else() {
        let old = lived_in();
        let new = reset_file(&old);
        assert_eq!(new["version"], json!(3));
        assert_eq!(new["selected_device"], json!("phone-a"), "the picked phone stays");
        assert_eq!(new["output_slots"], json!({"phone-a": 2}));
        let configs = &new["plugin_configs"];
        assert_eq!(configs["connection"], old["plugin_configs"]["connection"], "phones, tokens and the computer id stay whole");
        assert_eq!(configs["presets"], old["plugin_configs"]["presets"]);
        assert_eq!(configs["notes"], json!({"sets": ["my notes"], "current": 0}), "the notes stay, their size and speed go");
        assert_eq!(configs["composite"], json!({"layouts": [{"name": "Two"}]}), "the layouts stay, the live layout and switch go");
        assert_eq!(configs["mcp"], json!({"journal": [{"when": 1}]}));
        assert_eq!(configs["browser_camera"], json!({"address": "cam.local", "known": {"b1": {"name": "Pixel"}}}));
        assert_eq!(configs["on_air"], json!({"token": "a".repeat(32), "address": "tally.local", "webhook_url": "https://hook"}));
        assert!(configs.get("setup").is_none() && configs.get("shortcuts").is_none(), "the rest is back to its defaults");
        assert_eq!(configs.as_object().unwrap().len(), 7);
    }

    #[test]
    fn a_reset_keeps_each_phone_and_its_typed_name_but_resets_its_settings() {
        let new = reset_file(&lived_in());
        assert_eq!(new["devices"]["phone-a"], json!({
            "active_ip": "10.0.0.5",
            "plugin_configs": {"name_tag": {"name": "Luna", "pronouns": "she/her", "second": "Streamer"}, "presets": {"items": [{"name": "Phone preset"}]}},
        }));
        assert_eq!(new["devices"]["phone-b"], json!({}), "the phone is still there with nothing of its own saved");
    }

    #[test]
    fn a_reset_puts_the_switches_back_and_keeps_added_plugins_and_drops_window_state() {
        let new = reset_file(&lived_in());
        assert_eq!(new["plugins"], json!({"off": [], "noticed": [], "added": {"mine": "abc"}}), "on, off and the update switches are defaults again");
        assert!(new.get("collapsed").is_none() && new.get("bar_pins").is_none());
        assert!(crate::switches::builtin_off(&new).is_empty() && crate::switches::optional_on(&new).is_empty());
    }

    #[test]
    fn resetting_twice_changes_nothing_more_and_a_bare_file_stays_bare() {
        let once = reset_file(&lived_in());
        assert_eq!(reset_file(&once), once);
        let bare = reset_file(&config::empty());
        assert_eq!(bare["plugin_configs"], json!({}));
        assert_eq!(bare["devices"], json!({}));
        assert!(bare["selected_device"].is_null());
        assert_eq!(bare["plugins"], json!({"off": [], "noticed": [], "added": {}}));
        let junk = reset_file(&json!({"plugin_configs": [1], "devices": {"p": 3}, "selected_device": 4}));
        assert_eq!(junk["plugin_configs"], json!({}));
        assert!(junk["selected_device"].is_null());
        assert_eq!(junk["devices"], json!({}));
    }

    /// A plugin that holds what it was given and records the sign-in switch it is told about.
    struct Live {
        name: &'static str,
        local: bool,
        config: Value,
        told: Arc<Mutex<Vec<UiEvent>>>,
    }

    impl Plugin for Live {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: self.name.into(), device_local: self.local, ..Default::default() }
        }
        fn get_config(&self) -> Value {
            self.config.clone()
        }
        fn set_config(&mut self, cfg: &Value) -> Result<(), telescope_core::plugin::ConfigRejected> {
            self.config = cfg.clone();
            Ok(())
        }
        fn on_ui(&mut self, _host: &mut dyn telescope_core::host::Host, event: &UiEvent) {
            self.told.lock().unwrap().push(event.clone());
        }
    }

    fn running() -> (State, ConfigFile, Arc<Mutex<Vec<UiEvent>>>) {
        let dir = scratch_dir("reset");
        let file = ConfigFile::at(dir.join(CONFIG_FILENAME));
        file.save(&mut lived_in());
        let mut s = State::new(ConfigFile::at(file.path()));
        let told = Arc::new(Mutex::new(Vec::new()));
        s.register(Box::new(Live { name: "notes", local: false, config: json!({"sets": ["my notes"], "size": 99}), told: told.clone() })).unwrap();
        s.register(Box::new(Live { name: STARTUP, local: false, config: json!({"start_on": "ready"}), told: told.clone() })).unwrap();
        s.register(Box::new(Live { name: "transforms", local: true, config: json!({"zoom": 9.0}), told: told.clone() })).unwrap();
        (s, file, told)
    }

    #[test]
    fn nothing_the_running_app_saves_afterwards_brings_the_old_settings_back() {
        let (mut s, file, _) = running();
        s.reset_all_settings().unwrap();
        let reset = file.load();
        assert_eq!(reset["plugin_configs"]["notes"], json!({"sets": ["my notes"], "current": 0}));
        assert!(reset["plugin_configs"].get("shortcuts").is_none());
        // What the app does on its way out: a save of what the plugins hold, a phone's profile, a save through another file handle
        assert!(s.save_now(), "a save that is dropped still counts as done, so the window doesn't retry it");
        s.save_profile(Some("phone-a"), &["transforms".to_string()]);
        assert!(ConfigFile::at(file.path()).update(|cfg| cfg["plugin_configs"]["notes"]["size"] = json!(99)));
        assert_eq!(file.load(), reset, "the file is still the reset one");
        assert_eq!(std::fs::read_dir(file.path().parent().unwrap()).unwrap().flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".before-reset-")).count(), 1, "the old file was set aside once");
    }

    #[test]
    fn the_copy_set_aside_is_the_old_file() {
        let (mut s, file, _) = running();
        let before = file.load();
        s.reset_all_settings().unwrap();
        let copy = std::fs::read_dir(file.path().parent().unwrap()).unwrap().flatten().map(|e| e.path())
            .find(|p| p.to_string_lossy().contains(".before-reset-")).unwrap();
        assert_eq!(ConfigFile::at(copy).load(), before);
    }

    #[test]
    fn a_restart_that_fails_gives_the_old_settings_back_and_saving_again() {
        let (mut s, file, _) = running();
        let before = file.load();
        s.reset_all_settings().unwrap();
        s.undo_reset();
        assert_eq!(file.load(), before);
        assert!(s.save_now());
        assert_eq!(file.load()["plugin_configs"]["notes"]["size"], json!(99), "saves reach the file again");
        s.undo_reset();
    }

    #[test]
    fn a_reset_with_no_file_yet_undoes_to_no_file() {
        let dir = scratch_dir("reset-bare");
        let mut s = State::new(ConfigFile::at(dir.join(CONFIG_FILENAME)));
        s.reset_all_settings().unwrap();
        assert!(s.file.path().exists());
        s.undo_reset();
        assert!(!s.file.path().exists());
    }

    #[test]
    fn a_file_that_cannot_be_read_is_not_reset_over() {
        let dir = scratch_dir("reset-unreadable");
        let path = dir.join(CONFIG_FILENAME);
        std::fs::create_dir(&path).unwrap();
        let mut s = State::new(ConfigFile::at(&path));
        assert!(s.reset_all_settings().is_err());
        assert!(s.reset_pending.is_none());
    }

    #[test]
    fn sign_in_autostart_is_switched_off_through_the_startup_plugin() {
        let (mut s, _, told) = running();
        s.reset_sign_in();
        let told = told.lock().unwrap();
        let sign_in: Vec<_> = told.iter().filter(|e| e.ui == "sign-in").collect();
        assert_eq!(sign_in.len(), 1);
        assert_eq!((sign_in[0].callback.as_str(), &sign_in[0].args[..]), ("open-toggled", &[json!(false)][..]));
    }
}
