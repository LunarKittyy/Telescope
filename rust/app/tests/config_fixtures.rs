// `telescope-rs config-snapshot` starts the real window offscreen with every
// built-in plugin in a throwaway profile, applies the fixture's settings, saves and prints what came of it. Nothing is left out of the comparison.
// Linux only: the expected files are recorded there
#![cfg(target_os = "linux")]

use serde_json::Value;
use std::path::{Path, PathBuf};
use telescope_core::json;

fn fixtures() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/configs");
    let mut folders: Vec<PathBuf> = std::fs::read_dir(root).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    folders.sort();
    folders
}

/// The first path where two snapshots differ, with both values.
fn first_difference(path: &str, want: &Value, got: &Value) -> Option<String> {
    let (Some(w), Some(g)) = (want.as_object(), got.as_object()) else {
        return (want != got).then(|| format!("{path}: expected {want}, got {got}"));
    };
    w.keys().chain(g.keys()).find_map(|k| match (w.get(k), g.get(k)) {
        (Some(a), Some(b)) => first_difference(&format!("{path}.{k}"), a, b),
        (a, b) => Some(format!("{path}.{k}: expected {a:?}, got {b:?}")),
    })
}

#[test]
fn the_app_ends_up_where_the_fixture_says() {
    let folders = fixtures();
    assert!(folders.len() >= 9, "{folders:?}");
    for folder in folders {
        let name = folder.file_name().unwrap().to_string_lossy().into_owned();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_telescope-rs")).arg("config-snapshot").arg(&folder).output().unwrap();
        assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
        let got = json::loads(&String::from_utf8(out.stdout).unwrap()).unwrap();
        let want = json::loads(&std::fs::read_to_string(folder.join("expected.json")).unwrap()).unwrap();
        // As JSON values, so 50 and 50.0 differ: the two apps must save the same text
        assert_eq!(first_difference("", &want, &got), None, "{name}");
    }
}

/// Where a damaged value can go: every value below a plugin's section or a phone's.
fn places(value: &Value, here: &mut Vec<String>, found: &mut Vec<Vec<String>>) {
    found.push(here.clone());
    match value {
        Value::Object(map) => map.iter().for_each(|(k, v)| {
            here.push(k.clone());
            places(v, here, found);
            here.pop();
        }),
        Value::Array(items) => items.iter().take(40).enumerate().for_each(|(i, v)| {
            here.push(i.to_string());
            places(v, here, found);
            here.pop();
        }),
        _ => {}
    }
}

fn put(value: &mut Value, at: &[String], new: Value) {
    let Some((last, parents)) = at.split_last() else { return };
    let mut node = value;
    // An earlier change may have replaced a parent, and then there is nothing left to damage there
    for key in parents {
        let next = match node {
            Value::Object(map) => map.get_mut(key),
            Value::Array(items) => key.parse::<usize>().ok().and_then(|i| items.get_mut(i)),
            _ => None,
        };
        let Some(next) = next else { return };
        node = next;
    }
    match node {
        Value::Object(map) => drop(map.insert(last.clone(), new)),
        Value::Array(items) => {
            if let Some(slot) = last.parse::<usize>().ok().and_then(|i| items.get_mut(i)) {
                *slot = new;
            }
        }
        _ => {}
    }
}

#[test]
fn values_of_the_wrong_kind_in_the_settings_never_stop_the_app_starting() {
    use serde_json::json;
    let weird = [
        json!(null), json!(true), json!(false), json!(0), json!(-1), json!(2147483648u64), json!(i64::MIN), json!(u64::MAX), json!(1e308), json!(-1e308), json!(0.5),
        json!(""), json!("x"), json!("nan"), json!("-5"), json!("12abc"), json!("\u{0}"), json!("a".repeat(5000)), json!([]), json!({}), json!([1, 2, 3]),
        json!([null]), json!({"a": 1}), json!([[]]), json!({"": {}}), json!([1e308, -1e308]), json!(vec!["x"; 50]),
    ];
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut pick = |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as usize % n
    };
    let mut cases = Vec::new();
    for name in ["linux-two-phones", "windows-usb-phone", "linux-dev-profile"] {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/configs").join(name);
        let text = std::fs::read_to_string(folder.join("telescope_config.json")).unwrap();
        let base = json::loads(text.trim_start_matches('\u{feff}')).unwrap();
        let mut spots = Vec::new();
        for section in ["plugin_configs", "devices"] {
            places(&base[section], &mut vec![section.to_string()], &mut spots);
        }
        spots.retain(|s| s.len() >= 2);
        for round in 0..8 {
            let mut config = base.clone();
            for _ in 0..[1, 1, 2, 3, 6, 12][pick(6)] {
                let spot = &spots[pick(spots.len())];
                put(&mut config, spot, weird[pick(weird.len())].clone());
            }
            cases.push((format!("{name} {round}"), folder.join("expected.json"), json::dumps(&config, json::Style::CONFIG)));
        }
    }
    let failures = std::sync::Mutex::new(Vec::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some((name, expected, config)) = cases.get(i) else { break };
                let dir = telescope_core::testing::scratch_dir("damaged-values");
                std::fs::write(dir.join("telescope_config.json"), config).unwrap();
                std::fs::copy(expected, dir.join("expected.json")).unwrap();
                let out = std::process::Command::new(env!("CARGO_BIN_EXE_telescope-rs")).arg("config-snapshot").arg(&dir).output().unwrap();
                let stderr = String::from_utf8_lossy(&out.stderr);
                // A plugin that panics is caught and the app carries on, so the log is where it shows
                if !out.status.success() || stderr.contains("panicked") {
                    failures.lock().unwrap().push(format!("{name}: {}\n{config}", stderr.lines().rev().take(5).collect::<Vec<_>>().join("\n")));
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(failures.is_empty(), "{} of {} damaged settings stopped the app:\n{}", failures.len(), cases.len(), failures.join("\n\n"));
}
