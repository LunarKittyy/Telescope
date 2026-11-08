// The controls of the Camera, Phone, Photos and Connection cards say what was done to them: each callback comes back with the value the
// plugin reads (the plugins' own tests cover what they do with it). Driven through the controls' accessible actions, as a screen reader would.
use i_slint_backend_testing::ElementHandle;
use serde_json::{json, Value as Json};
use slint::ComponentHandle;
use slint_interpreter::{ComponentInstance, Value};
use std::cell::RefCell;
use std::rc::Rc;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_effects::photos::PhotosPlugin;
use telescope_setup::monitoring::MonitoringPlugin;
use telescope_shell::plugins::camera_control::CameraControl;
use telescope_shell::source::to_property;

type Heard = Rc<RefCell<Vec<(String, Vec<String>)>>>;

/// A card up with `props` set and every callback in `names` recorded as (name, its arguments as text).
fn card(source: &str, component: &str, props: &Json, names: &[&str]) -> (ComponentInstance, Heard) {
    i_slint_backend_testing::init_no_event_loop();
    let def = telescope_shell::compile::compile("card.slint", source, vec![], component).expect("the card compiles");
    let ui = def.create().unwrap();
    let heard: Heard = Rc::default();
    for name in names {
        let (h, n) = (heard.clone(), name.to_string());
        ui.set_callback(name, move |args| {
            let said = args.iter().map(|a| match a {
                Value::Number(v) => format!("{v}"),
                Value::String(s) => s.to_string(),
                Value::Bool(b) => b.to_string(),
                other => format!("{other:?}"),
            });
            h.borrow_mut().push((n.clone(), said.collect()));
            Value::Void
        }).unwrap();
    }
    for (key, value) in props.as_object().unwrap() {
        ui.set_property(key, to_property(&ui, key, value)).unwrap_or_else(|e| panic!("{key}: {e:?}"));
    }
    ui.window().set_size(slint::LogicalSize::new(406.0, 1400.0));
    ui.show().unwrap();
    (ui, heard)
}

fn source_of(plugin: &mut dyn Plugin) -> (String, String, Json) {
    let spec = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = spec.source else { panic!("source text") };
    (source, spec.component, spec.props)
}

/// The control with this accessible label that has `what` (a slider's value range, a switch's check).
fn find(ui: &ComponentInstance, label: &str, what: impl Fn(&ElementHandle) -> bool) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label).find(|e| what(e)).unwrap_or_else(|| panic!("no control called {label}"))
}

fn said(heard: &Heard, name: &str) -> Vec<Vec<String>> {
    heard.borrow().iter().filter(|(n, _)| n == name).map(|(_, a)| a.clone()).collect()
}

#[test]
fn the_camera_card_dials_tiles_and_segments_say_what_they_picked() {
    let (source, component, base) = source_of(&mut CameraControl::new());
    let names = ["iso-picked", "shutter-picked", "ois-toggled", "low-light-toggled", "torch-toggled", "exposure-picked", "nr-picked", "edge-picked", "focus-picked"];
    let (ui, heard) = card(&source, &component, &base, &names);
    for (key, value) in json!({"active": true, "iso-enabled": true, "shutter-enabled": true, "iso-stops": ["100", "200", "400", "800"], "iso-index": 2,
                               "shutter-stops": ["1/30", "1/60", "1/125"], "shutter-index": 1, "ois": {"checked": true, "enabled": true, "tip": ""},
                               "low-light": {"checked": false, "enabled": true, "tip": ""}, "torch": {"checked": false, "enabled": true, "tip": ""},
                               "noise-reduction": 1, "sharpening": 1}).as_object().unwrap() {
        ui.set_property(key, to_property(&ui, key, value)).unwrap();
    }
    let is_dial = |e: &ElementHandle| e.accessible_value_maximum().is_some();
    find(&ui, "ISO", is_dial).invoke_accessible_increment_action();
    find(&ui, "Shutter", is_dial).invoke_accessible_decrement_action();
    assert_eq!((said(&heard, "iso-picked"), said(&heard, "shutter-picked")), (vec![vec!["3".to_string()]], vec![vec!["0".to_string()]]));
    let tile = |label: &str| find(&ui, label, |e| e.accessible_checkable() == Some(true));
    tile("Stabilization").invoke_accessible_default_action();
    tile("Low light").invoke_accessible_default_action();
    tile("Torch").invoke_accessible_default_action();
    assert_eq!(said(&heard, "ois-toggled"), [["false"]]);
    assert_eq!((said(&heard, "low-light-toggled"), said(&heard, "torch-toggled")), (vec![vec!["true".to_string()]], vec![vec!["true".to_string()]]));
    // The words of the two rows of Off, Fast and High quality are the same, so each segment is named with its row
    let manual = ElementHandle::find_by_accessible_label(&ui, "Exposure: Manual").next().expect("the Manual segment");
    manual.invoke_accessible_default_action();
    assert_eq!(said(&heard, "exposure-picked"), [["1"]]);
    let high = ElementHandle::find_by_accessible_label(&ui, "Noise reduction: High quality").next().expect("the noise reduction segment");
    high.invoke_accessible_default_action();
    assert_eq!(said(&heard, "nr-picked"), [["2"]]);
    let sharp = ElementHandle::find_by_accessible_label(&ui, "Sharpening: Off").next().expect("the sharpening segment");
    sharp.invoke_accessible_default_action();
    assert_eq!(said(&heard, "edge-picked"), [["0"]]);
}

#[test]
fn a_dial_that_is_off_says_nothing_and_the_white_balance_sliders_never_snap() {
    let (source, component, base) = source_of(&mut CameraControl::new());
    let (ui, heard) = card(&source, &component, &base, &["iso-picked", "kelvin-moved", "tint-moved"]);
    ui.set_property("active", Value::Bool(true)).unwrap();
    // Auto exposure: the dial is there and dim, and turning it does nothing
    find(&ui, "ISO", |e| e.accessible_value_maximum().is_some()).invoke_accessible_increment_action();
    assert!(said(&heard, "iso-picked").is_empty());
    for (key, value) in [("wb-enabled", Value::Bool(true)), ("kelvin", Value::Number(5500.0)), ("tint", Value::Number(0.0))] {
        ui.set_property(key, value).unwrap();
    }
    // Two ordinary sliders: a step is a step of the slider, with no mark to stick to (Brightness is the one slider with one)
    find(&ui, "Temperature", |e| e.accessible_value_maximum().is_some()).invoke_accessible_increment_action();
    find(&ui, "Tint", |e| e.accessible_value_maximum().is_some()).invoke_accessible_increment_action();
    assert_eq!((said(&heard, "kelvin-moved"), said(&heard, "tint-moved")), (vec![vec!["5600".to_string()]], vec![vec!["1".to_string()]]));
}

#[test]
fn the_phone_card_handles_and_tiles_say_what_they_set() {
    let (source, component, base) = source_of(&mut MonitoringPlugin::new());
    let (ui, heard) = card(&source, &component, &base, &["batt-moved", "temp-moved", "alert-toggled"]);
    for (key, value) in json!({"active": true, "batt-alert": 20, "temp-alert": 45, "battery-level": 74, "battery-has-level": true}).as_object().unwrap() {
        ui.set_property(key, to_property(&ui, key, value)).unwrap();
    }
    find(&ui, "Battery under", |e| e.accessible_value_maximum().is_some()).invoke_accessible_increment_action();
    find(&ui, "Hotter than", |e| e.accessible_value_maximum().is_some()).invoke_accessible_decrement_action();
    assert_eq!((said(&heard, "batt-moved"), said(&heard, "temp-moved")), (vec![vec!["21".to_string()]], vec![vec!["44".to_string()]]));
    find(&ui, "Battery under: Stop the stream", |e| e.accessible_checkable() == Some(true)).invoke_accessible_default_action();
    find(&ui, "Hotter than: Notify me", |e| e.accessible_checkable() == Some(true)).invoke_accessible_default_action();
    assert_eq!(said(&heard, "alert-toggled"), [["batt-stop", "true"], ["temp-notify", "false"]]);
}

#[test]
fn the_photos_card_pictures_and_steppers_say_what_they_picked() {
    let (source, component, base) = source_of(&mut PhotosPlugin::new());
    let (ui, heard) = card(&source, &component, &base, &["tile-picked", "tile-step", "timer-picked", "best-toggled", "quality-moved", "take-clicked"]);
    for (key, value) in json!({"active": true, "shown": true, "take-enabled": true, "shots-offered": [true, true, false, true], "shots-index": 0,
                               "slot-key": "burst", "slot-label": "How many", "slot-options": ["2", "3", "4"], "slot-index": 1,
                               "distortion-enabled": true, "best-enabled": true, "best": true, "quality-enabled": true,
                               "timer-items": ["Off", "3 s", "10 s"]}).as_object().unwrap() {
        ui.set_property(key, to_property(&ui, key, value)).unwrap();
    }
    // The four ways to shoot are drawings with the way as their name; the one the lens lacks can't be picked
    find(&ui, "Shots: Burst", |e| e.accessible_checkable() == Some(true)).invoke_accessible_default_action();
    find(&ui, "Shots: Exposure bracket", |e| e.accessible_checkable() == Some(true)).invoke_accessible_default_action();
    find(&ui, "Shots: Focus bracket", |e| e.accessible_checkable() == Some(true)).invoke_accessible_default_action();
    assert_eq!(said(&heard, "tile-picked"), [["shots", "1"], ["shots", "3"]], "the bracket the lens doesn't offer said nothing");
    find(&ui, "Timer: 3 s", |e| e.accessible_checkable() == Some(true)).invoke_accessible_default_action();
    assert_eq!(said(&heard, "timer-picked"), [["1"]]);
    find(&ui, "Distortion fix: On", |e| e.accessible_checkable() == Some(true)).invoke_accessible_default_action();
    assert_eq!(said(&heard, "tile-picked").last().unwrap(), &["distortion", "1"]);
    find(&ui, "Best quality", |e| e.accessible_checkable() == Some(true)).invoke_accessible_default_action();
    assert_eq!(said(&heard, "best-toggled"), [["false"]]);
}

#[test]
fn the_connection_card_route_row_and_fix_button_say_what_was_picked() {
    // Only the last component a file exports can be asked for by name, so the card is cut out of the file
    let file = include_str!("../../crates/link/ui/connection.slint");
    let card_text = &file[..file.find("// The phone picker and the Your phones button").unwrap()];
    let (ui, heard) = card(card_text, "ConnectionCard", &json!({"status-text": "Phone app needs an update", "status-kind": "status_warn", "route-shown": true,
                                                              "update-shown": true, "update-text": "Update over USB"}), &["route-picked", "update", "add-phone", "guide"]);
    find(&ui, "Connect via: USB", |e| e.accessible_checkable() == Some(true)).invoke_accessible_default_action();
    assert_eq!(said(&heard, "route-picked"), [["1"]]);
    find(&ui, "Update over USB", |e| e.accessible_label().is_some()).invoke_accessible_default_action();
    assert_eq!(said(&heard, "update").len(), 1);
    // The header has the plus (named Add device) and the guide
    find(&ui, "Add device", |_| true).invoke_accessible_default_action();
    assert_eq!(said(&heard, "add-phone").len(), 1);
}
