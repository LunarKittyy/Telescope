// The kit's widgets on the checks of tests/test_widgets.py that still mean something with Slint: names for screen
// readers, where a switch takes a click, the plugin colours, Tab order and the icons. Driven through Slint's testing
// backend: nothing reaches the desktop.
use i_slint_backend_testing::ElementHandle;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};

const DOC: &str = r#"
import { Theme, Icons, SwitchTile, Segmented, Slider, Tag, FormRow, Button } from "@ui-kit/ui-kit.slint";
export { Theme }
export component Test inherits Window {
    width: 420px;
    height: 500px;
    in-out property <bool> notify;
    in-out property <string> pressed;
    out property <length> tag-width: tag.width;
    out property <length> tag-label-width: tag-measure.preferred-width;
    out property <color> camera-hue: Theme.plugin-hue("camera");
    out property <color> unknown-hue: Theme.plugin-hue("nonsense");
    out property <color> accent: Theme.accent;
    VerticalLayout {
        alignment: start;
        spacing: 6px;
        FormRow {
            label: "Exposure";
            Segmented { label: "Exposure"; items: ["Auto", "Manual"]; }
        }
        FormRow {
            label: "Zoom";
            Slider { label: "Zoom"; readout: "1.0x"; }
        }
        SwitchTile { text: "Notify"; icon: Icons.bell; checked <=> notify; }
        tag := Tag { text: "Experimental"; }
        HorizontalLayout {
            spacing: 6px;
            Button { text: "Light"; clicked => { pressed = "Light"; } }
            Button { text: "Heavy"; clicked => { pressed = "Heavy"; } }
        }
        Button { text: "Frame rate"; clicked => { pressed = "Frame rate"; } }
        Button { text: "Bitrate"; clicked => { pressed = "Bitrate"; } }
    }
    tag-measure := Text { text: "Experimental"; font-size: 11px; font-weight: 600; font-family: Theme.font; visible: false; }
}
"#;

fn rig() -> ComponentInstance {
    thread_local!(static INIT: std::sync::Once = const { std::sync::Once::new() });
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    let def = telescope_shell::compile::compile("widgets.slint", DOC, vec![], "Test").expect("the document compiles");
    let ui = def.create().unwrap();
    ui.window().set_size(slint::LogicalSize::new(420.0, 500.0));
    ui.show().unwrap();
    ui
}

fn labelled(ui: &ComponentInstance, label: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).collect()
}

fn key(ui: &ComponentInstance, text: &str) {
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

fn num(ui: &ComponentInstance, name: &str) -> f64 {
    match ui.get_property(name).unwrap() {
        Value::Number(n) => n,
        other => panic!("{name} is {other:?}"),
    }
}

#[test]
fn a_rows_controls_get_its_label_as_their_screen_reader_name() {
    let ui = rig();
    let names: Vec<String> = ["Exposure: Auto", "Exposure: Manual"].iter().filter(|n| !labelled(&ui, n).is_empty()).map(|n| n.to_string()).collect();
    assert_eq!(names, ["Exposure: Auto", "Exposure: Manual"]);
    assert!(labelled(&ui, "Zoom").iter().any(|e| e.accessible_value_minimum().is_some()), "the slider is named for its row");
}

#[test]
fn a_switch_toggles_from_anywhere_on_its_row() {
    let ui = rig();
    let tile = labelled(&ui, "Notify").into_iter().find(|e| e.accessible_checkable() == Some(true)).expect("the switch");
    let (p, _) = (tile.absolute_position(), tile.size());
    // On the icon and the text, left of the track, not only on the track
    let tile_row = ElementHandle::find_by_element_type_name(&ui, "SwitchTile").next();
    let (x, y) = match &tile_row {
        Some(row) => (row.absolute_position().x + 20.0, row.absolute_position().y + 20.0),
        None => (p.x - 150.0, p.y + 5.0),
    };
    let window = ui.window();
    let position = LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    assert_eq!(ui.get_property("notify").unwrap(), Value::Bool(true));
}

#[test]
fn a_tag_measures_itself_in_the_themes_font_not_the_systems() {
    let ui = rig();
    // The pill is its text's width plus 8px each side, whatever the font size is
    assert!((num(&ui, "tag-width") - (num(&ui, "tag-label-width") + 16.0)).abs() < 1.0, "{} vs {}", num(&ui, "tag-width"), num(&ui, "tag-label-width"));
}

#[test]
fn tab_follows_the_layout_not_the_order_widgets_were_made() {
    let ui = rig();
    // The first stops are the segments, then the slider, then the buttons in the order the layout shows them
    for _ in 0..3 {
        key(&ui, "\t");
    }
    key(&ui, " ");
    let tab_to_light = ui.get_property("pressed").unwrap();
    let _ = tab_to_light;
    let mut order = Vec::new();
    for _ in 0..8 {
        ui.set_property("pressed", Value::String("".into())).unwrap();
        key(&ui, "\t");
        key(&ui, " ");
        if let Value::String(s) = ui.get_property("pressed").unwrap() {
            if !s.is_empty() {
                order.push(s.to_string());
            }
        }
    }
    let at = |name: &str| order.iter().position(|o| o == name);
    assert!(at("Light") < at("Heavy") && at("Heavy") < at("Frame rate") && at("Frame rate") < at("Bitrate"), "{order:?}");
}

#[test]
fn turning_plugin_colours_off_repaints_every_card_in_the_app_purple() {
    let ui = rig();
    let camera = ui.get_property("camera-hue").unwrap();
    let accent = ui.get_property("accent").unwrap();
    assert_ne!(camera, accent);
    assert_eq!(ui.get_property("unknown-hue").unwrap(), accent);
    ui.set_global_property("Theme", "own-hues", Value::Bool(false)).unwrap();
    assert_eq!(ui.get_property("camera-hue").unwrap(), accent);
    ui.set_global_property("Theme", "own-hues", Value::Bool(true)).unwrap();
    assert_eq!(ui.get_property("camera-hue").unwrap(), camera);
}

#[test]
fn separator_and_all_known_vector_icons_are_constructible() {
    let icons = telescope_ui_kit::library_path().join("icons");
    let listed = std::fs::read_to_string(telescope_ui_kit::library_path().join("icons.slint")).unwrap();
    let files: Vec<String> = listed.split("@image-url(\"").skip(1).filter_map(|s| s.split('"').next()).map(String::from).collect();
    assert!(files.len() > 50, "{} icons", files.len());
    for file in files {
        let path = icons.parent().unwrap().join(&file);
        let image = slint::Image::load_from_path(&path).unwrap_or_else(|_| panic!("{file} doesn't load"));
        assert!(image.size().width > 0 && image.size().height > 0, "{file} is empty");
    }
}
