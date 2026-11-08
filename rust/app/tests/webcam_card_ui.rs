// The Webcam card fed what the plugin builds for a webcam with every kind of picture setting, shown and drawn on the
// offscreen platform (nothing reaches the desktop). The rows leave out what a kind doesn't use, and Slint panics on a field
// a struct doesn't have, which took the app down the moment a laptop's webcam was picked.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use slint_interpreter::Value;
use std::sync::{Arc, Mutex};
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_core::testing::FakeHost;
use telescope_core::ui::SlintSource;
use telescope_outputs::webcam::{System, WebcamPlugin, NAME, SOURCE_PREFIX};
use telescope_outputs::webcam_controls::{Control, Controls};
use telescope_outputs::webcams::Device;
use telescope_shell::offscreen;
use telescope_shell::source::to_property;

struct Settings;

impl Controls for Settings {
    fn list(&mut self) -> Vec<Control> {
        let mut time = Control::new("exposure_time", "int", 3, 2047, 1, 250, 250);
        time.unit = "100us".into();
        let mut flicker = Control::new("anti_flicker", "menu", 0, 2, 1, 2, 2);
        flicker.menu = vec![0, 1, 2];
        // Saturation is under Image, so the card has more than one kind of settings and shows their headings
        vec![Control::new("exposure_auto", "bool", 0, 1, 1, 1, 1), time, Control::new("gain", "int", 0, 255, 1, 0, 40), flicker,
             Control::new("frame_rate_drop", "bool", 0, 1, 1, 0, 1), Control::new("saturation", "int", 0, 255, 1, 128, 128)]
    }
    fn set(&mut self, _: &str, _: i64) -> bool {
        true
    }
}

fn laptop_webcam() -> Device {
    Device { key: "laptop".into(), name: "Integrated Camera".into(), location: "/dev/video0".into(), modes: Vec::new() }
}

/// The plugin with one webcam picked, whose settings are `Settings`, and the props it hands its card.
fn props_with_controls() -> (WebcamPlugin, serde_json::Value) {
    let plugged = Arc::new(Mutex::new(vec![laptop_webcam()]));
    let system = System { opener: Arc::new(|_, _| Err((false, "no camera in tests".into()))), users: Arc::new(|_| String::new()), exists: Arc::new(|_| true) };
    let mut plugin = WebcamPlugin::new(Box::new(move || Ok(plugged.lock().unwrap().clone())), Box::new(|_| Some(Box::new(Settings) as Box<dyn Controls>)), system);
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    plugin.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: "scan".into() });
    plugin.on_event(&mut host, &Event::SourceSelected { source_id: format!("{SOURCE_PREFIX}laptop") });
    let props = plugin.props(false);
    (plugin, props)
}

#[test]
fn the_webcam_card_shows_and_draws_a_row_of_every_kind() {
    offscreen::install();
    let (mut plugin, props) = props_with_controls();
    let kinds: Vec<&str> = props["rows"].as_array().unwrap().iter().map(|r| r["kind"].as_str().unwrap()).collect();
    for kind in ["heading", "auto", "switch", "menu", "slider"] {
        assert!(kinds.contains(&kind), "the rows have no {kind}: {kinds:?}");
    }
    let card = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let def = telescope_shell::compile::compile("webcam.slint", &source, vec![], &card.component).expect("the card compiles");
    let ui = def.create().unwrap();
    for (name, value) in props.as_object().unwrap().iter().filter(|(k, _)| *k != "visible") {
        ui.set_property(name, to_property(&ui, name, value)).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
    ui.window().set_size(slint::LogicalSize::new(460.0, 900.0));
    ui.show().unwrap();
    assert!(offscreen::draw(ui.window(), 460.0, 900.0, 1.0).is_some());
    // What a screen reader reads of every row, the sliders hidden in rows of another kind too
    for row in props["rows"].as_array().unwrap() {
        for element in ElementHandle::find_by_accessible_label(&ui, row["label"].as_str().unwrap()) {
            let _ = (element.accessible_value(), element.accessible_value_minimum(), element.accessible_value_maximum(), element.accessible_checked());
        }
    }
    let Ok(Value::Model(rows)) = ui.get_property("rows") else { panic!("rows is not a list") };
    assert_eq!(slint::Model::row_count(&rows), kinds.len());
    // A heading has no slider range or options: they come as the defaults
    let Some(Value::Struct(heading)) = slint::Model::row_data(&rows, 0) else { panic!("no first row") };
    assert_eq!(heading.get_field("kind"), Some(&Value::String("heading".into())));
    assert!(matches!(heading.get_field("options"), Some(Value::Model(m)) if slint::Model::row_count(m) == 0));
    assert_eq!(heading.get_field("position"), Some(&Value::Number(0.0)));
    assert_eq!(heading.get_field("tip"), Some(&Value::String("".into())));
    ui.hide().unwrap();
}

/// A DSLR's settings: focus buttons and menus the camera words itself, two of which run in order.
struct Dslr;

impl Controls for Dslr {
    fn list(&mut self) -> Vec<Control> {
        let mut focus = Control::new("dslr_focus", "buttons", 0, 4, 1, 1, 1);
        focus.transient = true;
        let menu = |name: &str, labels: &[&str], at: i64, unit: &str, ordered: bool| {
            let mut c = Control::new(name, "menu", 0, labels.len() as i64 - 1, 1, at, at);
            c.menu = (0..labels.len() as i64).collect();
            c.labels = labels.iter().map(|s| s.to_string()).collect();
            (c.transient, c.unit, c.ordered) = (true, unit.into(), ordered);
            c
        };
        vec![focus, menu("dslr_aperture", &["f/3,5", "f/4", "f/5,6"], 1, "fstop", true), menu("dslr_exposure_comp", &["-0,333", "0", "0,333"], 1, "ev", true),
             menu("dslr_white_balance", &["Automatic", "Daylight"], 0, "", false)]
    }
    fn set(&mut self, _: &str, _: i64) -> bool {
        true
    }
}

// The strip of a DSLR's values reaches the card as it is built (tiles of a row struct and a list of numbers) and draws at the widths
// the columns give it, from a card too narrow for three tiles to one wide enough for them
#[test]
fn a_dslrs_card_draws_its_strip_of_tiles_at_every_width() {
    offscreen::install();
    let plugged = Arc::new(Mutex::new(vec![Device { key: "d5100".into(), name: "Nikon DSC D5100".into(), location: "gphoto2:usb:001,004".into(), modes: Vec::new() }]));
    let system = System { opener: Arc::new(|_, _| Err((false, "no camera in tests".into()))), users: Arc::new(|_| String::new()), exists: Arc::new(|_| true) };
    let mut plugin = WebcamPlugin::new(Box::new(move || Ok(plugged.lock().unwrap().clone())), Box::new(|_| Some(Box::new(Dslr) as Box<dyn Controls>)), system);
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    plugin.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: "scan".into() });
    plugin.on_event(&mut host, &Event::SourceSelected { source_id: format!("{SOURCE_PREFIX}d5100") });
    let props = plugin.props(false);
    assert_eq!(props["tiles"].as_array().unwrap().len(), 3);
    assert_eq!(props["tile-indexes"], serde_json::json!([1, 1, 0]));
    let card = plugin.card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let def = telescope_shell::compile::compile("webcam.slint", &source, vec![], &card.component).expect("the card compiles");
    let ui = def.create().unwrap();
    for (name, value) in props.as_object().unwrap().iter().filter(|(k, _)| *k != "visible") {
        ui.set_property(name, to_property(&ui, name, value)).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
    for width in [300.0, 360.0, 460.0, 640.0] {
        ui.window().set_size(slint::LogicalSize::new(width, 500.0));
        ui.show().unwrap();
        assert!(offscreen::draw(ui.window(), width, 500.0, 1.0).is_some());
        // The three values are all on the card whatever its width: two side by side and one under, or all three in a line
        for label in ["Aperture", "Exposure", "White balance"] {
            let shown: Vec<_> = ElementHandle::find_by_accessible_label(&ui, label).filter(|e| e.size().width > 0.0).collect();
            assert!(!shown.is_empty(), "no {label} at {width}");
            assert!(shown.iter().all(|e| e.absolute_position().x + e.size().width <= width), "{label} runs off a card {width} wide");
        }
    }
    let Ok(Value::Model(tiles)) = ui.get_property("tiles") else { panic!("tiles is not a list") };
    let Some(Value::Struct(first)) = slint::Model::row_data(&tiles, 0) else { panic!("no first tile") };
    assert_eq!(first.get_field("kind"), Some(&Value::String("stepper".into())));
    ui.hide().unwrap();
}
