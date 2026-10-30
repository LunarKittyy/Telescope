// The kit's widgets on the checks of tests/test_widgets.py that still mean something with Slint: names for screen
// readers, where a switch takes a click, the plugin colours, Tab order and the icons. Driven through Slint's testing
// backend: nothing reaches the desktop.
use i_slint_backend_testing::ElementHandle;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};

const DOC: &str = r#"
import { Theme, Icons, Switch, SwitchTile, Segmented, Slider, Tag, FormRow, Button, TipBody } from "@ui-kit/ui-kit.slint";
export { Theme }
export component Test inherits Window {
    width: 420px;
    height: 500px;
    in-out property <bool> notify;
    // What a plugin says, bound one way into the widgets below
    in property <float> zoom: 1;
    in property <bool> flag;
    in property <int> mode;
    in property <bool> second-tile-on;
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
        SwitchTile { text: "Notify"; icon: Icons.bell; checked: notify; }
        Slider { label: "Bound zoom"; minimum: 0; maximum: 10; value: zoom; width: 300px; }
        Switch { label: "Bound flag"; checked: flag; }
        SwitchTile { text: "Bound tile"; icon: Icons.bell; checked: second-tile-on; }
        Segmented { label: "Bound mode"; items: ["a", "b", "c"]; current: mode; }
        tag := Tag { text: "Experimental"; }
        Rectangle { width: 200px; height: 40px; Segmented { label: "Seg"; items: ["Auto", "Tap a point", "Manual"]; current: 0; } }
        HorizontalLayout {
            spacing: 6px;
            Button { text: "Light"; clicked => { pressed = "Light"; } }
            Button { text: "Heavy"; clicked => { pressed = "Heavy"; } }
        }
        Button { text: "Frame rate"; clicked => { pressed = "Frame rate"; } }
        Button { text: "Bitrate"; clicked => { pressed = "Bitrate"; } }
    }
    short-tip := TipBody { says: "Settings"; visible: false; }
    long-tip := TipBody { says: "All the way right is Dynamic: as much as the connection carries, lowered by itself All the way right is Dynamic: as much as the connection carries, lowered by itself All the way right is Dynamic: as much as the connection carries, lowered by itself"; visible: false; }
    out property <length> short-tip-width: short-tip.preferred-width;
    out property <length> short-tip-height: short-tip.preferred-height;
    out property <length> long-tip-width: long-tip.preferred-width;
    out property <length> long-tip-height: long-tip.preferred-height;
    out property <length> bold-width: bold-measure.preferred-width;
    bold-measure := Text { text: "Tap a point"; font-size: Theme.body; font-weight: 600; font-family: Theme.font; visible: false; }
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
    assert_eq!(tile.accessible_checked(), Some(true));
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
    for _ in 0..20 {
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

// test_tooltips.py::test_a_long_tooltip_wraps_and_a_short_one_stays_on_one_line
#[test]
fn a_long_tooltip_wraps_and_a_short_one_stays_on_one_line() {
    let ui = rig();
    let (short_w, short_h) = (num(&ui, "short-tip-width"), num(&ui, "short-tip-height"));
    let (long_w, long_h) = (num(&ui, "long-tip-width"), num(&ui, "long-tip-height"));
    // About 64 characters to a line: the long one stops at a readable width and grows downwards, none of it cut off
    assert!(long_w <= 470.0 + 16.0 + 1.0 && long_w > short_w, "{long_w} wide");
    assert!(short_w < 120.0, "a short one hugs its text: {short_w}");
    assert!(long_h >= short_h + 2.0 * 15.0, "several lines: {long_h} against {short_h}");
}

// test_widgets.py::test_a_picked_segment_keeps_room_for_its_bold_text
#[test]
fn a_picked_segment_keeps_room_for_its_bold_text() {
    let ui = rig();
    let segment = labelled(&ui, "Seg: Tap a point").into_iter().next().expect("the middle segment");
    assert!(segment.size().width as f64 >= num(&ui, "bold-width") + 16.0, "{} against {}", segment.size().width, num(&ui, "bold-width") + 16.0);
}

fn value_of(ui: &ComponentInstance, label: &str) -> String {
    labelled(ui, label).into_iter().find_map(|e| e.accessible_value()).map(|v| v.to_string()).unwrap_or_default()
}

fn click(ui: &ComponentInstance, e: &ElementHandle) {
    let (p, s) = (e.absolute_position(), e.size());
    let position = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
}

fn settle(ui: &ComponentInstance) {
    slint::platform::update_timers_and_animations();
    let _ = ui;
}

#[test]
fn a_slider_dragged_by_hand_still_follows_the_value_bound_to_it() {
    let ui = rig();
    assert_eq!(value_of(&ui, "Bound zoom"), "1");
    let slider = labelled(&ui, "Bound zoom").into_iter().find(|e| e.accessible_value().is_some()).unwrap();
    let (p, s) = (slider.absolute_position(), slider.size());
    let y = p.y + s.height / 2.0;
    let window = ui.window();
    for x in [p.x + 40.0, p.x + 90.0, p.x + 150.0] {
        let position = LogicalPosition::new(x, y);
        window.dispatch_event(WindowEvent::PointerMoved { position });
        if x == p.x + 40.0 {
            window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        }
    }
    window.dispatch_event(WindowEvent::PointerReleased { position: LogicalPosition::new(p.x + 150.0, y), button: PointerEventButton::Left });
    settle(&ui);
    let dragged: f64 = value_of(&ui, "Bound zoom").parse().unwrap();
    assert!(dragged > 3.0, "the handle went with the hand: {dragged}");
    // The plugin then says another value (a scroll zoom, a preset): the handle goes there
    ui.set_property("zoom", Value::Number(2.0)).unwrap();
    settle(&ui);
    assert_eq!(value_of(&ui, "Bound zoom"), "2");
    ui.set_property("zoom", Value::Number(9.0)).unwrap();
    settle(&ui);
    assert_eq!(value_of(&ui, "Bound zoom"), "9");
}

#[test]
fn a_switch_clicked_by_hand_still_follows_the_value_bound_to_it() {
    let ui = rig();
    let switch = labelled(&ui, "Bound flag").into_iter().find(|e| e.accessible_checkable() == Some(true)).unwrap();
    click(&ui, &switch);
    assert_eq!(switch.accessible_checked(), Some(true));
    ui.set_property("flag", Value::Bool(true)).unwrap();
    settle(&ui);
    ui.set_property("flag", Value::Bool(false)).unwrap();
    settle(&ui);
    assert_eq!(switch.accessible_checked(), Some(false));
    // A tile the same
    let tile = labelled(&ui, "Bound tile").into_iter().find(|e| e.accessible_checkable() == Some(true)).unwrap();
    key(&ui, "\t");
    click(&ui, &tile);
    assert_eq!(tile.accessible_checked(), Some(true));
    ui.set_property("second-tile-on", Value::Bool(true)).unwrap();
    settle(&ui);
    ui.set_property("second-tile-on", Value::Bool(false)).unwrap();
    settle(&ui);
    assert_eq!(tile.accessible_checked(), Some(false));
}

#[test]
fn a_segment_picked_by_hand_still_follows_the_value_bound_to_it() {
    let ui = rig();
    let checked = |name: &str| labelled(&ui, name).into_iter().next().and_then(|e| e.accessible_checked());
    assert_eq!(checked("Bound mode: a"), Some(true));
    click(&ui, &labelled(&ui, "Bound mode: b").into_iter().next().unwrap());
    assert_eq!((checked("Bound mode: a"), checked("Bound mode: b")), (Some(false), Some(true)));
    ui.set_property("mode", Value::Number(2.0)).unwrap();
    settle(&ui);
    assert_eq!((checked("Bound mode: b"), checked("Bound mode: c")), (Some(false), Some(true)));
}
