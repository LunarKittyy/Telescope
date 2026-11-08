// A card must not jump when its state changes (STYLE.md, Stability): stepped through the change one loop turn at a time, where a control
// near the foot of the card sits only ever goes from where it was to where it ends up, once, with no dip or flicker between.
use i_slint_backend_testing::ElementHandle;
use serde_json::{json, Value as Json};
use slint::ComponentHandle;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::desk::DeskPlugin;
use telescope_effects::photos::PhotosPlugin;
use telescope_setup::monitoring::MonitoringPlugin;
use telescope_shell::offscreen;
use telescope_shell::plugins::camera_control::CameraControl;
use telescope_shell::source::to_property;

/// The card of `plugin` shown, then `change` applied to it; the y of the control labelled `probe` after the change and after each
/// of the loop turns that follow.
fn steps(plugin: &mut dyn Plugin, before: Json, change: Json, probe: &str) -> Vec<f32> {
    let card = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    steps_of(&source, &card.component, before, change, probe)
}

/// The same for a card taken from its source text.
fn steps_of(source: &str, component: &str, before: Json, change: Json, probe: &str) -> Vec<f32> {
    offscreen::install();
    let def = telescope_shell::compile::compile("card.slint", source, vec![], component).expect("the card compiles");
    let ui = def.create().unwrap();
    let set = |props: &Json| {
        for (name, value) in props.as_object().unwrap() {
            ui.set_property(name, to_property(&ui, name, value)).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    };
    if ui.get_property("active").is_ok() {
        set(&json!({"active": true}));
    }
    set(&before);
    ui.window().set_size(slint::LogicalSize::new(460.0, 1200.0));
    ui.show().unwrap();
    let y = || ElementHandle::find_by_accessible_label(&ui, probe).next().unwrap_or_else(|| panic!("no {probe}")).absolute_position().y;
    offscreen::pump();
    let mut seen = vec![y()];
    set(&change);
    // Each turn is a frame's worth of time more, so every ease the change starts (an arrival's fade, a handle's glide) is stepped through too
    for _ in 0..8 {
        seen.push(y());
        offscreen::skip(std::time::Duration::from_millis(40));
    }
    seen.push(y());
    ui.hide().unwrap();
    seen
}

/// Before, then after: no value but those two, and a single change between them.
fn goes_from_one_to_the_other(seen: &[f32], what: &str) {
    let (first, last) = (seen[0], *seen.last().unwrap());
    assert!(seen.iter().all(|y| (*y - first).abs() < 0.5 || (*y - last).abs() < 0.5), "{what} dipped on the way: {seen:?}");
    let flips = seen.windows(2).filter(|w| (w[0] - w[1]).abs() >= 0.5).count();
    assert!(flips <= 1, "{what} flickered: {seen:?}");
}

#[test]
fn the_camera_card_keeps_its_height_when_the_lenses_arrive() {
    let lenses = json!([
        {"id": "wide", "number": "1", "unit": "x", "rest": "Wide", "name": "Wide"},
        {"id": "tele", "number": "3", "unit": "x", "rest": "Tele", "name": "Tele"},
    ]);
    let seen = steps(&mut CameraControl::new(), json!({"lenses": [], "lens-placeholder": "Start streaming to load lenses", "lens-columns": 3}),
                     json!({"lenses": lenses, "current-lens": 0, "lens-placeholder": ""}), "Sharpening");
    goes_from_one_to_the_other(&seen, "the Camera card");
    // The placeholder tiles are one row of the real tiles' height, so for up to three lenses nothing moves at all
    assert!((seen[0] - *seen.last().unwrap()).abs() < 0.5, "the lenses pushed the card down: {seen:?}");
}

#[test]
fn the_photos_card_keeps_its_height_when_streaming_starts() {
    let seen = steps(&mut PhotosPlugin::new(), json!({"take-enabled": false}), json!({"take-enabled": true}), "Save in");
    goes_from_one_to_the_other(&seen, "the Photos card");
    assert!((seen[0] - *seen.last().unwrap()).abs() < 0.5, "starting a stream moved the card's rows: {seen:?}");
}

#[test]
fn the_photos_card_keeps_its_rows_when_the_lens_brings_its_choices_and_a_way_to_shoot_is_picked() {
    let before = json!({"take-enabled": false});
    let lens = json!({"take-enabled": true, "mode-enabled": true, "format-enabled": true, "mode-options": ["Normal", "Night"], "format-options": ["JPEG", "RAW"],
                      "shots-offered": [true, true, true, true], "distortion-enabled": true, "exposure-options": ["Like the video", "1 s"], "best-enabled": true});
    let seen = steps(&mut PhotosPlugin::new(), before, lens.clone(), "Save in");
    assert!((seen[0] - *seen.last().unwrap()).abs() < 0.5, "the lens's choices moved the card's rows: {seen:?}");
    let seen = steps(&mut PhotosPlugin::new(), lens, json!({"shots-index": 1, "slot-key": "burst", "slot-label": "How many", "slot-options": ["2", "3", "4"]}), "Save in");
    assert!((seen[0] - *seen.last().unwrap()).abs() < 0.5, "How many took a row of its own: {seen:?}");
}

#[test]
fn the_camera_card_keeps_its_rows_when_exposure_goes_manual() {
    let seen = steps(&mut CameraControl::new(), json!({}), json!({"exposure-mode": 1, "iso-enabled": true, "shutter-enabled": true, "wb-mode": 1, "wb-enabled": true,
                                                              "focus-mode": 2, "focus-enabled": true}), "Sharpening");
    assert!((seen[0] - *seen.last().unwrap()).abs() < 0.5, "the manual controls moved the rows below them: {seen:?}");
}

#[test]
fn the_phone_card_keeps_its_bars_when_the_readings_arrive() {
    let seen = steps(&mut MonitoringPlugin::new(), json!({"active": true}), json!({"battery-level": 74, "battery-has-level": true, "battery-value": "74%",
                                                                                "temp-level": 38.0, "temp-has-level": true, "temp-value": "38 \u{b0}C"}), "Hotter than");
    assert!((seen[0] - *seen.last().unwrap()).abs() < 0.5, "the readings moved the bars: {seen:?}");
}

#[test]
fn the_connection_card_keeps_its_rows_when_a_device_connects() {
    // Only the last component a file exports can be asked for by name, so the card is cut out of the file
    let file = include_str!("../../crates/link/ui/connection.slint");
    let card = &file[..file.find("// The phone picker and the Your phones button").unwrap()];
    let seen = steps_of(card, "ConnectionCard", json!({"status-text": "No device yet", "status-kind": "status_dim"}),
                        json!({"status-text": "Streaming over Wi-Fi", "status-kind": "", "route-shown": true, "using-shown": true, "using-text": "Wi-Fi"}), "Connect via");
    assert!((seen[0] - *seen.last().unwrap()).abs() < 0.5, "a device moved the route row: {seen:?}");
    let seen = steps_of(card, "ConnectionCard", json!({"status-text": "Checking", "status-kind": "status_dim", "route-shown": true}),
                        json!({"status-text": "Phone app needs an update", "status-kind": "status_warn", "update-shown": true, "update-text": "Update over USB"}), "Connect via");
    assert!((seen[0] - *seen.last().unwrap()).abs() < 0.5, "the fix button moved the route row: {seen:?}");
}

#[test]
fn the_desk_card_keeps_its_rows_when_the_corners_are_being_placed() {
    let seen = steps(&mut DeskPlugin::new(), json!({"picking": false}), json!({"picking": true}), "Height");
    goes_from_one_to_the_other(&seen, "the Desk card");
    assert!((seen[0] - *seen.last().unwrap()).abs() < 0.5, "Done and Cancel took the place of Mark at another height: {seen:?}");
}
