//! Checks the settings a plugin declares against what the plugin does with them.

use serde_json::{json, Value};
use telescope_core::host::Host;
use telescope_core::plugin::Plugin;
use telescope_core::settings::{get_path, merge_changes, SettingKind, SettingSpec};

/// Equal, with numbers compared by value so 5 and 5.0 are the same.
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
        _ => a == b,
    }
}

/// A value the setting takes that is not `now`.
fn other(spec: &SettingSpec, now: &Value) -> Value {
    match &spec.kind {
        SettingKind::Switch => json!(!now.as_bool().expect("a switch holds true or false")),
        SettingKind::Number { min, max, decimals } => {
            let places = 10f64.powi(*decimals as i32);
            let at = |share: f64| ((min + (max - min) * share) * places).round() / places;
            let now = now.as_f64().expect("a number setting holds a number");
            json!(if (at(0.25) - now).abs() > 1e-9 { at(0.25) } else { at(0.75) })
        }
        SettingKind::Choice(options) => json!(options.iter().map(|(v, _)| v).find(|v| Some(v.as_str()) != now.as_str()).expect("a choice has two options")),
    }
}

/// Every declared key is in get_config() at its default, the default passes its own check, and a changed value goes in through apply_preset and comes back out.
pub fn round_trips(plugin: &mut dyn Plugin, host: &mut dyn Host) {
    let specs = plugin.settings();
    assert!(!specs.is_empty(), "the plugin declares settings");
    let fresh = plugin.get_config();
    for spec in &specs {
        let at = get_path(&fresh, &spec.key).unwrap_or_else(|| panic!("{} is in get_config()", spec.key));
        assert!(same(at, &spec.default), "{} starts at {at}, declared {}", spec.key, spec.default);
        let checked = spec.check(&spec.default).unwrap_or_else(|why| panic!("the default of {} fails its check: {why}", spec.key));
        assert!(same(&checked, &spec.default), "{} default changes in its check", spec.key);
    }
    for spec in &specs {
        let config = plugin.get_config();
        let want = spec.check(&other(spec, &spec.read(&config))).expect("a value inside the range passes");
        let changes = json!({ spec.key.clone(): want.clone() });
        let merged = merge_changes(&specs, &config, changes.as_object().unwrap()).expect("the change is accepted");
        plugin.apply_preset(host, &merged).unwrap_or_else(|_| panic!("{} is accepted by apply_preset", spec.key));
        let now = plugin.get_config();
        let got = get_path(&now, &spec.key).unwrap_or_else(|| panic!("{} is still in get_config()", spec.key));
        assert!(same(got, &want), "{} was set to {want} and reads {got}", spec.key);
    }
}
