//! What a plugin's settings are, as plain data: key, kind, range, default, unit and one line about it. Nothing here is shown in
//! the window, so the words stay English like Copy diagnostics. The MCP server lists and checks changes with it, and the
//! writes still go through the plugin's own `apply_preset` so its own checks apply.

use serde_json::{json, Map, Value};

/// What values a setting takes.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingKind {
    /// True or false.
    Switch,
    /// A number from `min` to `max`.
    Number { min: f64, max: f64, decimals: usize },
    /// One of these values, each with what it means.
    Choice(Vec<(String, String)>),
}

/// One setting of a plugin. `key` is where it sits in the plugin's get_config(), with dots going into nested objects
/// ("masters.gain").
#[derive(Clone, Debug, PartialEq)]
pub struct SettingSpec {
    pub key: String,
    pub about: String,
    pub kind: SettingKind,
    pub default: Value,
    pub unit: String,
}

impl SettingSpec {
    pub fn switch(key: &str, about: &str, default: bool) -> SettingSpec {
        SettingSpec { key: key.into(), about: about.into(), kind: SettingKind::Switch, default: json!(default), unit: String::new() }
    }

    pub fn number(key: &str, about: &str, min: f64, max: f64, decimals: usize, default: f64) -> SettingSpec {
        SettingSpec { key: key.into(), about: about.into(), kind: SettingKind::Number { min, max, decimals }, default: json!(default), unit: String::new() }
    }

    pub fn choice(key: &str, about: &str, options: &[(&str, &str)], default: &str) -> SettingSpec {
        let options = options.iter().map(|(v, m)| (v.to_string(), m.to_string())).collect();
        SettingSpec { key: key.into(), about: about.into(), kind: SettingKind::Choice(options), default: json!(default), unit: String::new() }
    }

    pub fn unit(mut self, unit: &str) -> SettingSpec {
        self.unit = unit.into();
        self
    }

    /// The value to store for `value`, or what is wrong with it. Never bends a number into range: someone who asks for 5 where the top is 2 should hear it.
    pub fn check(&self, value: &Value) -> Result<Value, String> {
        match &self.kind {
            SettingKind::Switch => match value {
                Value::Bool(_) => Ok(value.clone()),
                other => Err(format!("{} is a switch: send true or false, not {other}", self.key)),
            },
            SettingKind::Number { min, max, decimals } => match value.as_f64().filter(|n| n.is_finite()) {
                Some(n) if n >= *min && n <= *max => {
                    let places = 10f64.powi(*decimals as i32);
                    Ok(json!((n * places).round() / places))
                }
                Some(n) => Err(format!("{} goes from {} to {}{}, so {} is out of range", self.key, shown(*min), shown(*max), self.unit_text(), shown(n))),
                None => Err(format!("{} is a number from {} to {}{}, not {value}", self.key, shown(*min), shown(*max), self.unit_text())),
            },
            SettingKind::Choice(options) => match value.as_str().filter(|s| options.iter().any(|(v, _)| v == s)) {
                Some(_) => Ok(value.clone()),
                None => {
                    let names: Vec<&str> = options.iter().map(|(v, _)| v.as_str()).collect();
                    Err(format!("{} can be {}, not {value}", self.key, names.join(" | ")))
                }
            },
        }
    }

    fn unit_text(&self) -> String {
        if self.unit.is_empty() { String::new() } else { format!(" {}", self.unit) }
    }

    /// The setting as a tool lists it, with where it is now in `config` (the default when the plugin doesn't hold it).
    pub fn describe(&self, config: &Value) -> Value {
        let mut out = Map::new();
        out.insert("key".into(), json!(self.key));
        out.insert("about".into(), json!(self.about));
        match &self.kind {
            SettingKind::Switch => {
                out.insert("type".into(), json!("switch"));
            }
            SettingKind::Number { min, max, decimals } => {
                out.insert("type".into(), json!("number"));
                out.insert("min".into(), json!(min));
                out.insert("max".into(), json!(max));
                out.insert("decimals".into(), json!(decimals));
            }
            SettingKind::Choice(options) => {
                out.insert("type".into(), json!("choice"));
                out.insert("choices".into(), Value::Object(options.iter().map(|(v, m)| (v.clone(), json!(m))).collect()));
            }
        }
        if !self.unit.is_empty() {
            out.insert("unit".into(), json!(self.unit));
        }
        out.insert("default".into(), self.default.clone());
        out.insert("value".into(), self.read(config));
        Value::Object(out)
    }

    /// Where this setting is in `config`, or its default.
    pub fn read(&self, config: &Value) -> Value {
        get_path(config, &self.key).cloned().unwrap_or_else(|| self.default.clone())
    }
}

/// A number without a trailing ".0".
fn shown(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { format!("{n}") }
}

/// The value at a dotted key ("masters.gain") inside a config.
pub fn get_path<'a>(config: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(config, |at, part| at.get(part))
}

/// Puts `value` at a dotted key, making the objects on the way.
pub fn put_path(config: &mut Value, key: &str, value: Value) {
    let mut at = config;
    let mut parts = key.split('.').peekable();
    while let Some(part) = parts.next() {
        if !at.is_object() {
            *at = Value::Object(Map::new());
        }
        let map = at.as_object_mut().expect("just made an object");
        if parts.peek().is_none() {
            map.insert(part.into(), value);
            return;
        }
        at = map.entry(part.to_string()).or_insert_with(|| Value::Object(Map::new()));
    }
}

/// Every spot of a config as a dotted path with the value there: objects are walked into, everything else (numbers, text, lists, an object whose
/// own keys have dots in them) is one value.
fn leaves(config: &Value, prefix: &str, out: &mut std::collections::BTreeMap<String, Value>) {
    match config {
        Value::Object(map) if !map.is_empty() && !map.keys().any(|k| k.contains('.') || k.is_empty()) => {
            for (key, value) in map {
                let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                leaves(value, &path, out);
            }
        }
        _ if !prefix.is_empty() => {
            out.insert(prefix.to_string(), config.clone());
        }
        _ => {}
    }
}

/// What differs between two configs, as (dotted path, before, after): a value that wasn't there is null.
pub fn leaf_changes(before: &Value, after: &Value) -> Vec<(String, Value, Value)> {
    let (mut was, mut now) = (std::collections::BTreeMap::new(), std::collections::BTreeMap::new());
    leaves(before, "", &mut was);
    leaves(after, "", &mut now);
    let paths: std::collections::BTreeSet<&String> = was.keys().chain(now.keys()).collect();
    paths.into_iter()
        .filter(|p| was.get(*p) != now.get(*p))
        .map(|p| (p.clone(), was.get(p).cloned().unwrap_or(Value::Null), now.get(p).cloned().unwrap_or(Value::Null)))
        .collect()
}

/// Whether two dotted paths are the same spot or one is inside the other.
pub fn paths_overlap(a: &str, b: &str) -> bool {
    a == b || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('.')) || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('.'))
}

/// The settings a change names, checked against what a plugin declares: the config with them put in, or what is wrong.
/// Every problem is reported at once, and nothing is applied unless all of them are fine.
pub fn merge_changes(specs: &[SettingSpec], config: &Value, changes: &Map<String, Value>) -> Result<Value, Vec<String>> {
    let mut merged = if config.is_object() { config.clone() } else { Value::Object(Map::new()) };
    let mut problems = Vec::new();
    for (key, value) in changes {
        match specs.iter().find(|s| s.key == *key) {
            None => {
                let near: Vec<&str> = specs.iter().map(|s| s.key.as_str()).filter(|k| k.contains(key.as_str()) || (k.len() >= 4 && key.contains(k))).collect();
                let hint = if near.is_empty() { String::new() } else { format!(" (did you mean {}?)", near.join(" or ")) };
                problems.push(format!("There's no setting called {key}{hint}"));
            }
            Some(spec) => match spec.check(value) {
                Ok(v) => put_path(&mut merged, key, v),
                Err(why) => problems.push(why),
            },
        }
    }
    if problems.is_empty() { Ok(merged) } else { Err(problems) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade() -> Vec<SettingSpec> {
        vec![
            SettingSpec::switch("on", "Whether it's applied", true),
            SettingSpec::number("contrast", "How far tones spread", 0.0, 2.0, 2, 1.0),
            SettingSpec::number("masters.gain", "Highlights", 0.0, 2.0, 2, 1.0),
            SettingSpec::choice("mode", "How", &[("fast", "Quick"), ("fine", "Careful")], "fast").unit("x"),
        ]
    }

    #[test]
    fn numbers_are_checked_against_their_range_and_never_bent_into_it() {
        let spec = &grade()[1];
        assert_eq!(spec.check(&json!(1.234)).unwrap(), json!(1.23));
        assert_eq!(spec.check(&json!(5)).unwrap_err(), "contrast goes from 0 to 2, so 5 is out of range");
        assert!(spec.check(&json!("1")).unwrap_err().contains("is a number from 0 to 2"));
        assert!(spec.check(&json!(null)).is_err());
        assert!(spec.check(&json!(f64::NAN)).is_err());
    }

    #[test]
    fn switches_and_choices_name_what_they_take() {
        let specs = grade();
        assert_eq!(specs[0].check(&json!(false)).unwrap(), json!(false));
        assert!(specs[0].check(&json!("yes")).unwrap_err().contains("true or false"));
        assert_eq!(specs[3].check(&json!("fine")).unwrap(), json!("fine"));
        assert_eq!(specs[3].check(&json!("slow")).unwrap_err(), "mode can be fast | fine, not \"slow\"");
    }

    #[test]
    fn leaf_changes_name_each_spot_that_moved_and_nothing_that_stayed() {
        let before = json!({"on": true, "masters": {"gain": 1.0, "lift": 0.0}, "list": [1, 2], "gone": 1});
        let after = json!({"on": false, "masters": {"gain": 1.0, "lift": 0.2}, "list": [1, 2], "new": "x"});
        assert_eq!(leaf_changes(&before, &after), vec![
            ("gone".to_string(), json!(1), Value::Null),
            ("masters.lift".to_string(), json!(0.0), json!(0.2)),
            ("new".to_string(), Value::Null, json!("x")),
            ("on".to_string(), json!(true), json!(false)),
        ]);
        assert!(leaf_changes(&before, &before).is_empty());
        assert!(paths_overlap("masters", "masters.gain") && paths_overlap("masters.gain", "masters") && paths_overlap("on", "on"));
        assert!(!paths_overlap("masters", "masterss.gain") && !paths_overlap("on", "one"));
    }

    #[test]
    fn dotted_keys_go_into_nested_objects() {
        let mut config = json!({"masters": {"lift": 0.0}});
        put_path(&mut config, "masters.gain", json!(1.5));
        put_path(&mut config, "deep.er.still", json!(true));
        assert_eq!(config, json!({"masters": {"lift": 0.0, "gain": 1.5}, "deep": {"er": {"still": true}}}));
        assert_eq!(get_path(&config, "masters.gain"), Some(&json!(1.5)));
        assert_eq!(get_path(&config, "masters.nope"), None);
    }

    #[test]
    fn a_change_is_all_or_nothing_and_every_problem_is_listed() {
        let specs = grade();
        let config = json!({"on": true, "contrast": 1.0});
        let good = json!({"contrast": 1.4, "masters.gain": 0.5});
        assert_eq!(merge_changes(&specs, &config, good.as_object().unwrap()).unwrap(),
                   json!({"on": true, "contrast": 1.4, "masters": {"gain": 0.5}}));
        let bad = json!({"contrast": 9, "on": "yes", "contras": 1});
        let problems = merge_changes(&specs, &config, bad.as_object().unwrap()).unwrap_err();
        assert_eq!(problems.len(), 3);
        assert!(problems.iter().any(|p| p.contains("did you mean contrast")), "{problems:?}");
    }

    #[test]
    fn describing_a_setting_shows_its_value_or_its_default() {
        let specs = grade();
        let shown = specs[1].describe(&json!({"contrast": 1.5}));
        assert_eq!((shown["value"].clone(), shown["max"].clone(), shown["type"].clone()), (json!(1.5), json!(2.0), json!("number")));
        assert_eq!(specs[2].describe(&json!({}))["value"], json!(1.0));
        assert_eq!(specs[3].describe(&json!({}))["choices"]["fine"], json!("Careful"));
        assert_eq!(specs[3].describe(&json!({}))["unit"], json!("x"));
    }
}
