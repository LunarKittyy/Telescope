// test_config_fixtures.py::test_the_app_ends_up_where_the_fixture_says: `telescope-rs config-snapshot` starts the real window offscreen with every
// built-in plugin in a throwaway profile, applies the fixture's settings, saves and prints what came of it. Nothing is left out of the comparison.
use serde_json::Value;
use std::path::{Path, PathBuf};
use telescope_core::json;

fn fixtures() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../desktop/tests/fixtures/configs");
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

// The expected files are recorded on Linux
#[cfg(target_os = "linux")]
#[test]
fn the_app_ends_up_where_the_fixture_says() {
    let folders = fixtures();
    assert!(folders.len() >= 9, "{folders:?}");
    for folder in folders {
        let name = folder.file_name().unwrap().to_string_lossy().into_owned();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_telescope-rs")).arg("config-snapshot").arg(&folder).output().unwrap();
        assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
        let got = json::loads(&String::from_utf8(out.stdout).unwrap()).unwrap();
        let mut want = json::loads(&std::fs::read_to_string(folder.join("expected.json")).unwrap()).unwrap();
        // The one place Rust means to differ: with nothing saved, or Python's false, hardware decoding is on
        if want["saved"]["plugin_configs"]["setup"]["hw_decode"] == false.into() {
            want["saved"]["plugin_configs"]["setup"]["hw_decode"] = true.into();
        }
        // As JSON values, so 50 and 50.0 differ: the two apps must save the same text
        assert_eq!(first_difference("", &want, &got), None, "{name}");
    }
}
