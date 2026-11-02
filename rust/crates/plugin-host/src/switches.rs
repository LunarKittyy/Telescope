//! Which plugins are on, kept under "plugins" in the settings file exactly as the Python app keeps it, so an update brings every switch along.
//!
//! "off" lists the built-in plugins turned off, "noticed" the ones of those the user has been told about, "added" maps each added plugin
//! that's on to the fingerprint of its files, "on" lists the optional built-in plugins switched on. "check_updates" and "auto_update" are
//! the two update switches (updates.rs).

use crate::text::tr;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use telescope_core::config::ConfigFile;

pub const CHECK_UPDATES: &str = "check_updates";
pub const AUTO_UPDATE: &str = "auto_update";
/// What saving the switches leaves as it is.
const KEPT: [&str; 2] = [CHECK_UPDATES, AUTO_UPDATE];

fn section(cfg: &Value) -> Option<&Map<String, Value>> {
    cfg.get("plugins").and_then(Value::as_object)
}

fn names(cfg: &Value, key: &str) -> BTreeSet<String> {
    let list = section(cfg).and_then(|s| s.get(key)).and_then(Value::as_array);
    list.into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

fn added(cfg: &Value) -> BTreeMap<String, String> {
    let map = section(cfg).and_then(|s| s.get("added")).and_then(Value::as_object);
    map.into_iter().flatten().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()
}

fn not_saved() -> io::Error {
    io::Error::other(tr("Couldn't save the settings.", &[]))
}

/// The plugin switches in one settings file. Every change reads the file again first and fails rather than saving over one it can't read.
#[derive(Clone, Debug, Default)]
pub struct Switches {
    pub file: ConfigFile,
}

impl Switches {
    pub fn new(file: ConfigFile) -> Switches {
        Switches { file }
    }

    /// The built-in plugins the user turned off.
    pub fn builtin_off(&self) -> BTreeSet<String> {
        names(&self.file.load(), "off")
    }

    /// Turn a built-in plugin off (or back on) from the next start.
    pub fn set_builtin_off(&self, name: &str, off: bool) -> io::Result<()> {
        let cfg = self.file.load_strict()?;
        let mut names = names(&cfg, "off");
        if off {
            names.insert(name.to_string());
        } else {
            names.remove(name);
        }
        self.save(cfg, names, None, None, None)
    }

    /// Built-in plugins turned off that the user hasn't been reminded of yet (a banner at start).
    pub fn unnoticed_off(&self) -> BTreeSet<String> {
        let cfg = self.file.load();
        let noticed = names(&cfg, "noticed");
        names(&cfg, "off").difference(&noticed).cloned().collect()
    }

    /// The user has seen which built-in plugins are off: don't remind them again until another one is.
    pub fn notice_off(&self) -> io::Result<()> {
        let cfg = self.file.load_strict()?;
        let off = names(&cfg, "off");
        self.save(cfg, off.clone(), None, Some(off), None)
    }

    /// The standard selection from the next start: every built-in plugin on, the optional ones off.
    pub fn reset_to_defaults(&self) -> io::Result<()> {
        let cfg = self.file.load_strict()?;
        self.save(cfg, BTreeSet::new(), None, Some(BTreeSet::new()), Some(BTreeSet::new()))
    }

    /// The optional built-in plugins the user switched on (they start out off).
    pub fn optional_on(&self) -> BTreeSet<String> {
        names(&self.file.load(), "on")
    }

    pub fn set_optional_on(&self, name: &str, on: bool) -> io::Result<()> {
        let cfg = self.file.load_strict()?;
        let mut names_on = names(&cfg, "on");
        if on {
            names_on.insert(name.to_string());
        } else {
            names_on.remove(name);
        }
        let off = names(&cfg, "off");
        self.save(cfg, off, None, None, Some(names_on))
    }

    /// Each added plugin that's on, with the fingerprint its files had when it was switched on.
    pub fn added(&self) -> BTreeMap<String, String> {
        added(&self.file.load())
    }

    /// Switch an added plugin on with its files' fingerprint, or off with None.
    pub fn set_added(&self, name: &str, digest: Option<&str>) -> io::Result<()> {
        let cfg = self.file.load_strict()?;
        let mut on = added(&cfg);
        match digest {
            Some(d) => on.insert(name.to_string(), d.to_string()),
            None => on.remove(name),
        };
        let off = names(&cfg, "off");
        self.save(cfg, off, Some(on), None, None)
    }

    /// One of the update switches (CHECK_UPDATES, AUTO_UPDATE), default when it isn't saved as true or false.
    pub fn switch(&self, key: &str, default: bool) -> bool {
        section(&self.file.load()).and_then(|s| s.get(key)).and_then(Value::as_bool).unwrap_or(default)
    }

    pub fn set_switch(&self, key: &str, on: bool) -> io::Result<()> {
        let mut cfg = self.file.load_strict()?;
        let mut raw = section(&cfg).cloned().unwrap_or_default();
        raw.insert(key.to_string(), Value::Bool(on));
        cfg["plugins"] = Value::Object(raw);
        if self.file.save(&mut cfg) { Ok(()) } else { Err(not_saved()) }
    }

    /// _save_state: the section rewritten in Python's key order, the update switches kept.
    fn save(&self, mut cfg: Value, off: BTreeSet<String>, on_added: Option<BTreeMap<String, String>>, noticed: Option<BTreeSet<String>>,
            optional: Option<BTreeSet<String>>) -> io::Result<()> {
        let noticed = noticed.unwrap_or_else(|| names(&cfg, "noticed"));
        let optional = optional.unwrap_or_else(|| names(&cfg, "on"));
        let on_added = on_added.unwrap_or_else(|| added(&cfg));
        let mut out = Map::new();
        out.insert("off".into(), off.iter().cloned().collect());
        out.insert("noticed".into(), noticed.intersection(&off).cloned().collect());
        out.insert("added".into(), Value::Object(on_added.into_iter().map(|(k, v)| (k, Value::String(v))).collect()));
        for (k, v) in section(&cfg).into_iter().flatten() {
            if KEPT.contains(&k.as_str()) {
                out.insert(k.clone(), v.clone());
            }
        }
        if !optional.is_empty() {
            out.insert("on".into(), optional.into_iter().collect());
        }
        cfg["plugins"] = Value::Object(out);
        if self.file.save(&mut cfg) { Ok(()) } else { Err(not_saved()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use telescope_core::testing::scratch_dir;

    fn switches() -> Switches {
        Switches::new(ConfigFile::at(scratch_dir("switches").join("telescope_config.json")))
    }

    // test_plugin_loader.py::test_built_in_plugins_switch_off_and_back_on
    #[test]
    fn built_in_plugins_switch_off_and_back_on() {
        let s = switches();
        s.set_builtin_off("presets", true).unwrap();
        s.set_builtin_off("preview", true).unwrap();
        let set = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>();
        assert_eq!(s.builtin_off(), set(&["presets", "preview"]));
        assert_eq!(s.unnoticed_off(), set(&["presets", "preview"]));
        s.notice_off().unwrap();
        assert!(s.unnoticed_off().is_empty());
        s.set_builtin_off("monitoring", true).unwrap();
        assert_eq!(s.unnoticed_off(), set(&["monitoring"]));
        s.set_optional_on("framing", true).unwrap();
        s.reset_to_defaults().unwrap();
        assert!(s.builtin_off().is_empty() && s.optional_on().is_empty());
    }

    // test_plugin_loader.py::test_the_switches_survive_a_settings_save
    #[test]
    fn the_switches_survive_a_settings_save() {
        let s = switches();
        s.set_builtin_off("presets", true).unwrap();
        assert!(s.file.update(|cfg| cfg["selected_device"] = json!("x")));
        assert_eq!(s.builtin_off().into_iter().collect::<Vec<_>>(), ["presets"]);
    }

    #[test]
    fn the_section_is_written_as_python_writes_it() {
        let s = switches();
        s.set_switch(AUTO_UPDATE, true).unwrap();
        s.set_optional_on("cinema", true).unwrap();
        s.set_added("extra", Some("abc")).unwrap();
        s.set_builtin_off("presets", true).unwrap();
        let cfg = s.file.load();
        assert_eq!(cfg["plugins"], json!({"off": ["presets"], "noticed": [], "added": {"extra": "abc"}, "auto_update": true, "on": ["cinema"]}));
        assert_eq!(serde_json::to_string(&cfg["plugins"]).unwrap(),
                   r#"{"off":["presets"],"noticed":[],"added":{"extra":"abc"},"auto_update":true,"on":["cinema"]}"#);
        assert!(s.switch(AUTO_UPDATE, false) && s.switch(CHECK_UPDATES, true));
    }

    #[test]
    fn the_python_apps_switches_carry_over_from_every_fixture() {
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../desktop/tests/fixtures/configs");
        let mut seen = 0;
        for entry in std::fs::read_dir(fixtures).unwrap().flatten() {
            let (input, expected) = (entry.path().join("telescope_config.json"), entry.path().join("expected.json"));
            let Ok(expected) = std::fs::read_to_string(expected) else { continue };
            if !input.exists() {
                continue;
            }
            let saved: Value = serde_json::from_str(&expected).unwrap();
            let Some(plugins) = saved["saved"].get("plugins") else { continue };
            // A copy, since loading a damaged file backs it up next to itself
            let copy = scratch_dir("fixture").join("telescope_config.json");
            std::fs::copy(&input, &copy).unwrap();
            let s = Switches::new(ConfigFile::at(copy));
            let want = |key: &str| plugins.get(key).and_then(Value::as_array).map(|l| l.iter().filter_map(Value::as_str).count()).unwrap_or(0);
            // Junk ("off": "everything", numbers in "on") reads as nothing, the way plugin_loader._state reads it
            assert_eq!(s.builtin_off().len(), want("off"), "{}", entry.path().display());
            assert_eq!(s.optional_on().len(), want("on"), "{}", entry.path().display());
            seen += 1;
        }
        assert!(seen >= 2);
    }
}
