// A card must not jump when its state changes (STYLE.md, Stability): stepped through the change one loop turn at a time, where a control
// near the foot of the card sits only ever goes from where it was to where it ends up, once, with no dip or flicker between.
use i_slint_backend_testing::ElementHandle;
use serde_json::{json, Value as Json};
use slint::ComponentHandle;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::desk::DeskPlugin;
use telescope_effects::photos::PhotosPlugin;
use telescope_shell::offscreen;
use telescope_shell::plugins::camera_control::CameraControl;
use telescope_shell::source::to_property;

/// The card of `plugin` shown, then `change` applied to it; the y of the control labelled `probe` after the change and after each
/// of the loop turns that follow.
fn steps(plugin: &mut dyn Plugin, before: Json, change: Json, probe: &str) -> Vec<f32> {
    offscreen::install();
    let card = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let def = telescope_shell::compile::compile("card.slint", &source, vec![], &card.component).expect("the card compiles");
    let ui = def.create().unwrap();
    let set = |props: &Json| {
        for (name, value) in props.as_object().unwrap() {
            ui.set_property(name, to_property(&ui, name, value)).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
    };
    set(&json!({"active": true}));
    set(&before);
    ui.window().set_size(slint::LogicalSize::new(460.0, 1200.0));
    ui.show().unwrap();
    let y = || ElementHandle::find_by_accessible_label(&ui, probe).next().unwrap_or_else(|| panic!("no {probe}")).absolute_position().y;
    offscreen::pump();
    let mut seen = vec![y()];
    set(&change);
    for _ in 0..8 {
        seen.push(y());
        offscreen::pump();
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
    let seen = steps(&mut PhotosPlugin::new(), json!({"take-enabled": false, "info": "", "info-shown": false}), json!({"take-enabled": true}), "Save in");
    goes_from_one_to_the_other(&seen, "the Photos card");
    assert!((seen[0] - *seen.last().unwrap()).abs() < 0.5, "starting a stream moved the card's rows: {seen:?}");
}

#[test]
fn the_desk_card_keeps_its_rows_when_the_corners_are_being_placed() {
    let seen = steps(&mut DeskPlugin::new(), json!({"picking": false}), json!({"picking": true}), "Height");
    goes_from_one_to_the_other(&seen, "the Desk card");
    assert!((seen[0] - *seen.last().unwrap()).abs() < 0.5, "Done and Cancel took the place of Mark at another height: {seen:?}");
}
