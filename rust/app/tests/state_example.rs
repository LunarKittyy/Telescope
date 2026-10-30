// docs/state.json is one /v1/state that the phone's StateExampleTest.kt checks it still sends; this checks the desktop still reads it
// (tests/test_state_example.py).
use serde_json::Value;
use std::path::{Path, PathBuf};
use telescope_core::models::PhoneState;
use telescope_core::view::PhoneZoomCaps;
use telescope_link::stills::StillCaps;
use telescope_shell::plugins::camera_control::derive_camera_control_view;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn example() -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join("docs/state.json")).unwrap()).unwrap()
}

// test_state_example.py::test_the_desktop_reads_the_phones_state
#[test]
fn the_desktop_reads_the_phones_state() {
    let raw = example();
    let state = PhoneState::from_value(&raw).unwrap();
    assert!(!state.is_empty());
    assert_eq!((state.iso, state.shutter_ns, state.battery), (Some(400), Some(16_666_666), Some(87)));
    let camera = &state.cameras[0];
    assert_eq!((camera.id.as_str(), camera.iso_max, camera.lens_zooms.clone()), ("2", 3200, vec![3.0]));
    assert_eq!((state.auto, state.wb_manual, state.ois, state.focus_mode.as_str(), state.focus_distance), (false, true, false, "manual", 2.5));
    assert_eq!((state.ae_comp, state.nr_mode, state.edge_mode, state.black_level_lock, state.torch), (-3, 2, 2, true, true));
    assert_eq!((state.charging, state.battery_temp_c), (Some(true), Some(32.5)));
    let first = &raw["cameras"][0];
    assert_eq!((first["facing"].as_str(), first["focalMm"].as_i64(), first["lensKind"].as_str()), (Some("back"), Some(24), Some("physical")));
    assert_eq!((camera.logical_id.as_deref(), camera.label.as_str(), camera.current, camera.has_ois, camera.iso_min), (Some("0"), "Back ~24mm OIS [phys]", true, true, 50));
    assert_eq!((camera.shutter_min_ns, camera.shutter_max_ns, camera.min_focus_distance), (100_000, 1_000_000_000, 10.0));
    assert!(camera.supports_manual_sensor && camera.supports_manual_wb && camera.supports_manual_focus);
    assert!(camera.supports_focus_point && camera.supports_meter_point);
    assert!(camera.supports_flash && camera.freeform_crop);
    assert_eq!((camera.zoom_ratio_max, camera.crop_zoom_max, camera.hw_level.as_str()), (10.0, 8.0, "FULL"));
    assert_eq!((camera.ae_comp_min, camera.ae_comp_max), (-12, 12));
    assert!((camera.ae_comp_step - 0.16666667).abs() < 1e-9);
    assert_eq!(state.raw, raw);
}

// test_state_example.py::test_nested_camera_and_control_consumers_read_the_same_contract
#[test]
fn nested_camera_and_control_consumers_read_the_same_contract() {
    let state = example();
    let view = derive_camera_control_view(&state).expect("a view");
    assert!(view.manual_exposure && view.manual_wb && view.manual_focus);
    assert!(view.low_light && view.low_light_available && view.torch);
    let caps = PhoneZoomCaps::from_camera(Some(&state["cameras"][0])).expect("zoom caps");
    assert_eq!((caps.ratio_max, caps.crop_max, caps.lens_zooms), (10.0, 8.0, vec![3.0]));
    let stills = StillCaps::from_state(Some(&state)).expect("still caps");
    assert_eq!(stills.formats, ["jpeg", "heic", "ultrahdr", "raw"]);
    assert_eq!(stills.max_res, Some((8064, 6048)));
    assert_eq!(stills.extensions, ["night", "hdr", "bokeh"]);
    assert_eq!((stills.burst_max, stills.distortion), (60, true));
}

/// The names asked of a state in this source, from `state.get("name")` and `state["name"]`.
fn asked(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for pattern in ["state.get(\"", "state[\""] {
        for (at, _) in text.match_indices(pattern) {
            let before = text[..at].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let name: String = text[at + pattern.len()..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            if text[at + pattern.len() + name.len()..].starts_with('"') && !name.is_empty() {
                found.push(name);
            }
        }
    }
    found
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if !["target", "tests", "fixtures"].contains(&name.as_str()) {
                sources(&path, out);
            }
        } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "tests.rs" {
            out.push(path);
        }
    }
}

// test_state_example.py::test_every_state_field_the_desktop_asks_for_is_one_the_phone_sends
#[test]
fn every_state_field_the_desktop_asks_for_is_one_the_phone_sends() {
    // A field renamed or dropped on the phone shows up here, instead of as a control that quietly stops working
    let sent = example();
    let sent = sent.as_object().unwrap();
    let mut files = Vec::new();
    sources(&root().join("rust/crates"), &mut files);
    let mut unknown: Vec<String> = Vec::new();
    let mut total = 0;
    for file in files {
        // The On Air sign's own page state, not the phone's
        if file.ends_with("effects/src/on_air.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        for name in asked(&text) {
            total += 1;
            if !sent.contains_key(&name) {
                unknown.push(format!("{}: {name}", file.strip_prefix(root()).unwrap_or(&file).display()));
            }
        }
    }
    assert!(total > 10, "the pattern no longer finds anything: update it");
    assert!(unknown.is_empty(), "asked of the state but not in docs/state.json:\n{}", unknown.join("\n"));
}
