// The kit's widgets on the checks of tests/test_widgets.py that still mean something with Slint: names for screen
// readers, where a switch takes a click, the plugin colours, Tab order and the icons. Driven through Slint's testing
// backend: nothing reaches the desktop.
use i_slint_backend_testing::ElementHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};

const DOC: &str = r#"
import { Theme, Icons, Switch, SwitchTile, Segmented, Slider, Tag, FormRow, Button, ChoiceField, TipBody, SettingRow, DialogFrame } from "@ui-kit/ui-kit.slint";
export { Theme }
export component Test inherits Window {
    width: 420px;
    height: 560px;
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
            Button { text: "Reset"; }
        }
        SwitchTile { text: "Notify"; icon: Icons.bell; checked: notify; }
        Slider { label: "Bound zoom"; minimum: 0; maximum: 10; value: zoom; width: 300px; }
        Switch { label: "Bound flag"; checked: flag; }
        SwitchTile { text: "Bound tile"; icon: Icons.bell; checked: second-tile-on; }
        Segmented { label: "Bound mode"; items: ["a", "b", "c"]; current: mode; }
        tag := Tag { text: "Experimental"; }
        Rectangle { width: 200px; height: 40px; Segmented { label: "Seg"; items: ["Auto", "Tap a point", "Manual"]; current: 0; } }
        Rectangle { width: 400px; height: 40px; Segmented { label: "Wide"; items: ["Auto", "Tap a point", "Manual"]; current: 0; } }
        HorizontalLayout {
            spacing: 6px;
            Button { text: "Light"; clicked => { pressed = "Light"; } }
            Button { text: "Heavy"; clicked => { pressed = "Heavy"; } }
            Button { text: "Off button"; enabled: false; clicked => { pressed = "Off button"; } }
            Switch { label: "Off switch"; enabled: false; toggled => { pressed = "Off switch"; } }
        }
        Button { text: "Frame rate"; clicked => { pressed = "Frame rate"; } }
        Button { text: "Bitrate"; clicked => { pressed = "Bitrate"; } }
        Button { text: "Tipped"; tip: "About the owner"; }
        Rectangle { width: 300px; height: 36px; HorizontalLayout { Slider { label: "Wide room"; has-readout: false; } } }
        Rectangle { width: 40px; height: 36px; HorizontalLayout { Slider { label: "Narrow room"; has-readout: false; } } }
        ChoiceField { caption: "Picked"; items: ["a", "b", "c"]; gone: [false, true, false]; tip: "About the field"; picked(i) => { pressed = "row " + i; } }
        ChoiceField { caption: "Bound list"; items: ["a", "b", "c"]; current: mode; }
        HorizontalLayout { alignment: start; Slider { width: 200px; label: "Typed slider"; readout: "x"; typing: "1.00"; typed(t) => { pressed = "typed " + t; } } }
    }
    // One tooltip body at the size a Tooltip gives it, below the window: its text changes in place, like Qt's reused tip window
    in property <string> tip-text: "Settings";
    shown-tip := TipBody { y: 1000px; says: root.tip-text; width: self.preferred-width; height: self.preferred-height; }
    out property <length> tip-width: shown-tip.width;
    out property <length> tip-height: shown-tip.height;
    out property <length> bold-width: bold-measure.preferred-width;
    bold-measure := Text { text: "Tap a point"; font-size: Theme.body; font-weight: 600; font-family: Theme.font; visible: false; }
    tag-measure := Text { text: "Experimental"; font-size: floor(Theme.body * 0.88 / 1px) * 1px; font-weight: 600; font-family: Theme.font; visible: false; }
}
"#;

fn rig() -> ComponentInstance {
    thread_local!(static INIT: std::sync::Once = const { std::sync::Once::new() });
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    let def = telescope_shell::compile::compile("widgets.slint", DOC, vec![], "Test").expect("the document compiles");
    let ui = def.create().unwrap();
    ui.window().set_size(slint::LogicalSize::new(420.0, 560.0));
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
    assert!(!labelled(&ui, "Reset").is_empty(), "a button with words keeps them");
    assert!(labelled(&ui, "Zoom: Reset").is_empty());
}

const ROWS: &str = r#"
import { Icons, Switch, SwitchTile, SettingRow } from "@ui-kit/ui-kit.slint";
export component Rows inherits Window {
    width: 420px;
    height: 120px;
    in property <length> row-width: 300px;
    SwitchTile { x: 0; y: 0; width: row-width; text: "Notify"; icon: Icons.bell; }
    SettingRow { x: 0; y: 50px; width: row-width; title: "Battery"; Switch { label: "Battery"; } }
}
"#;

#[test]
fn a_switch_toggles_from_anywhere_on_its_row_and_sits_at_the_row_end() {
    let ui = open(ROWS, "Rows", 420.0, 120.0);
    let switch = |name: &str| labelled(&ui, name).into_iter().find(|e| e.accessible_checkable() == Some(true)).expect("the switch");
    let right = |name: &str| { let (p, s) = (switch(name).absolute_position(), switch(name).size()); p.x + s.width };
    // It takes the rest of its row, so its track lands at the end, and goes on landing there as the row grows
    for width in [300.0, 400.0] {
        ui.set_property("row-width", Value::Number(width)).unwrap();
        settle(&ui);
        assert_eq!((right("Notify"), right("Battery")), (width as f32 - 11.0, width as f32 - 13.0), "at {width}");
    }
    // On the icon and the text, left of the track, not only on the track
    let position = LogicalPosition::new(20.0, 20.0);
    let window = ui.window();
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    assert_eq!(switch("Notify").accessible_checked(), Some(true));
}

#[test]
fn a_tag_measures_itself_in_the_themes_font_not_the_systems() {
    let ui = rig();
    // The pill is its text's width plus 8px each side, whatever the font size is
    let fits = |ui: &ComponentInstance| assert!((num(ui, "tag-width") - (num(ui, "tag-label-width") + 16.0)).abs() < 1.0, "{} vs {}", num(ui, "tag-width"), num(ui, "tag-label-width"));
    fits(&ui);
    let small = num(&ui, "tag-width");
    assert!(small < 120.0, "{small}");
    // Follows a bigger font, as Python's follows a styled one
    ui.set_global_property("Theme", "body", Value::Number(26.0)).unwrap();
    settle(&ui);
    fits(&ui);
    assert!(num(&ui, "tag-width") > 120.0, "{small} to {}", num(&ui, "tag-width"));
}

const ORDER: &str = r#"
import { Button } from "@ui-kit/ui-kit.slint";
export component Order inherits Window {
    width: 300px;
    height: 300px;
    // Light is made after the window, so it's the last control made and comes second in the layout
    in property <bool> late;
    in-out property <string> pressed;
    VerticalLayout {
        alignment: start;
        Button { text: "Frame rate"; clicked => { pressed = "Frame rate"; } }
        HorizontalLayout {
            if late: Button { text: "Light"; clicked => { pressed = "Light"; } }
            Button { text: "Heavy"; clicked => { pressed = "Heavy"; } }
            Button { text: "Off"; enabled: false; clicked => { pressed = "Off"; } }
        }
        Button { text: "Bitrate"; clicked => { pressed = "Bitrate"; } }
    }
    Button { x: 0; y: 200px; text: "Hidden"; visible: false; clicked => { pressed = "Hidden"; } }
}
"#;

const DIALOG: &str = r#"
import { Button, DialogFrame, Segmented } from "@ui-kit/ui-kit.slint";
export component Gain inherits Window {
    width: 320px;
    height: 200px;
    in-out property <string> pressed;
    in property <[bool]> disabled: [false, false, false];
    DialogFrame {
        title: "Max gain";
        Segmented { label: "Gain"; items: ["+12 dB", "+24 dB", "+36 dB"]; disabled: disabled; }
        Button { text: "Close"; clicked => { pressed = "Close"; } }
    }
}
"#;

fn open(doc: &str, component: &str, width: f32, height: f32) -> ComponentInstance {
    thread_local!(static INIT: std::sync::Once = const { std::sync::Once::new() });
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    let ui = telescope_shell::compile::compile("doc.slint", doc, vec![], component).expect("the document compiles").create().unwrap();
    ui.window().set_size(slint::LogicalSize::new(width, height));
    ui.show().unwrap();
    ui
}

fn shift_tab(ui: &ComponentInstance) {
    let back: slint::SharedString = slint::platform::Key::Backtab.into();
    key(ui, &back);
}

/// What has the focus: Space presses it.
fn focused(ui: &ComponentInstance) -> String {
    ui.set_property("pressed", Value::String("".into())).unwrap();
    key(ui, " ");
    match ui.get_property("pressed").unwrap() {
        Value::String(s) => s.to_string(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn tab_follows_the_layout_not_the_order_widgets_were_made() {
    let ui = open(ORDER, "Order", 300.0, 300.0);
    ui.set_property("late", Value::Bool(true)).unwrap();
    settle(&ui);
    key(&ui, "\t");
    assert_eq!(focused(&ui), "Frame rate");
    let mut walked = Vec::new();
    for _ in 0..4 {
        key(&ui, "\t");
        walked.push(focused(&ui));
    }
    assert_eq!(walked, ["Light", "Heavy", "Bitrate", "Frame rate"], "in the layout's order, past what's off or hidden, and round");
    key(&ui, "\t");
    assert_eq!(focused(&ui), "Light");
    shift_tab(&ui);
    assert_eq!(focused(&ui), "Frame rate");
    shift_tab(&ui);
    assert_eq!(focused(&ui), "Bitrate", "backwards it goes round too");
}

#[test]
fn tab_in_a_dialog_stops_on_every_option_of_a_group() {
    let ui = open(DIALOG, "Gain", 320.0, 200.0);
    let picked = |name: &str| labelled(&ui, name)[0].accessible_checked();
    key(&ui, "\t");
    for name in ["Gain: +24 dB", "Gain: +36 dB"] {
        key(&ui, "\t");
        assert_eq!(picked(name), Some(false), "Tab only moves");
        key(&ui, " ");
        assert_eq!(picked(name), Some(true), "Space picks the option Tab stopped on");
    }
    key(&ui, "\t");
    assert_eq!(focused(&ui), "Close");
    shift_tab(&ui);
    shift_tab(&ui);
    key(&ui, " ");
    assert_eq!(picked("Gain: +24 dB"), Some(true), "and back through them the same way");
}

#[test]
fn an_arrow_moves_along_a_group_and_picks_only_from_the_picked_option() {
    let ui = open(DIALOG, "Gain", 320.0, 200.0);
    let picked = |name: &str| labelled(&ui, name)[0].accessible_checked();
    let arrow = |k: slint::platform::Key| {
        let text: slint::SharedString = k.into();
        key(&ui, &text);
        settle(&ui);
    };
    let right = slint::platform::Key::RightArrow;
    let left = slint::platform::Key::LeftArrow;
    key(&ui, "\t"); // the picked +12 dB
    key(&ui, "\t"); // +24 dB, not picked
    arrow(right);
    assert_eq!((picked("Gain: +12 dB"), picked("Gain: +36 dB")), (Some(true), Some(false)), "from there it only moves");
    arrow(left);
    arrow(left);
    assert_eq!(picked("Gain: +12 dB"), Some(true), "back along the row, still only moving");
    arrow(right);
    assert_eq!((picked("Gain: +12 dB"), picked("Gain: +24 dB")), (Some(false), Some(true)), "from the picked one the pick goes with the focus");
}

#[test]
fn a_disabled_option_isnt_a_tab_stop() {
    let ui = open(DIALOG, "Gain", 320.0, 200.0);
    let flags: Vec<Value> = [false, true, false].map(Value::Bool).to_vec();
    ui.set_property("disabled", Value::Model(slint::ModelRc::new(slint::VecModel::from(flags)))).unwrap();
    settle(&ui);
    key(&ui, "\t");
    key(&ui, "\t");
    key(&ui, " ");
    assert_eq!(labelled(&ui, "Gain: +36 dB")[0].accessible_checked(), Some(true));
    key(&ui, "\t");
    assert_eq!(focused(&ui), "Close");
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

// test_tooltips.py::test_a_long_tooltip_wraps_and_a_short_one_stays_on_one_line
#[test]
fn a_long_tooltip_wraps_and_a_short_one_stays_on_one_line() {
    let ui = rig();
    let long_text = "All the way right is Dynamic: as much as the connection carries, lowered by itself ".repeat(3);
    ui.set_property("tip-text", Value::String(long_text.trim_end().into())).unwrap();
    settle(&ui);
    let (long_w, long_h) = (num(&ui, "tip-width"), num(&ui, "tip-height"));
    // About 64 characters to a line: the long one stops at a readable width and grows downwards
    assert!(long_w <= 470.0 + 16.0 + 1.0, "{long_w} wide");
    let text = labelled(&ui, long_text.trim_end()).into_iter().next().expect("the tip's text");
    assert!(f64::from(text.size().height) <= long_h - 10.0 + 0.5 && f64::from(text.size().height) >= long_h - 10.0 - 0.5, "none of it is cut off: {} in {long_h}", text.size().height);

    // The same body, reused
    ui.set_property("tip-text", Value::String("Settings".into())).unwrap();
    settle(&ui);
    let (short_w, short_h) = (num(&ui, "tip-width"), num(&ui, "tip-height"));
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

// Qt's equal stretches: three segments of different lengths come out as wide as each other
#[test]
fn segments_share_their_row_equally_when_their_text_fits() {
    let ui = rig();
    let widths: Vec<f32> = ["Auto", "Tap a point", "Manual"].iter().map(|s| labelled(&ui, &format!("Wide: {s}"))[0].size().width).collect();
    assert!((widths[0] - widths[1]).abs() < 1.0 && (widths[1] - widths[2]).abs() < 1.0, "{widths:?}");
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

#[test]
fn a_dropdown_picked_by_hand_still_follows_the_value_bound_to_it() {
    let ui = rig();
    let shown = || labelled(&ui, "Bound list").into_iter().next().and_then(|e| e.accessible_value()).map(|v| v.to_string());
    assert_eq!(shown().as_deref(), Some("a"));
    click(&ui, &labelled(&ui, "Bound list")[0]);
    settle(&ui);
    for key in [Key::Escape, Key::DownArrow] {
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        ui.window().dispatch_event(WindowEvent::KeyReleased { text: key.into() });
        settle(&ui);
    }
    assert_eq!(shown().as_deref(), Some("b"), "the arrows picked it");
    ui.set_property("mode", Value::Number(2.0)).unwrap();
    settle(&ui);
    assert_eq!(shown().as_deref(), Some("c"));
}

#[test]
fn a_slider_can_take_a_typed_value_over_its_readout() {
    let ui = rig();
    let box_at = |ui: &ComponentInstance| {
        let slider = labelled(ui, "Typed slider").into_iter().next().unwrap();
        let (p, s) = (slider.absolute_position(), slider.size());
        LogicalPosition::new(p.x + s.width + 30.0, p.y + s.height / 2.0)
    };
    let press = |ui: &ComponentInstance, text: &str| {
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.into() });
        ui.window().dispatch_event(WindowEvent::KeyReleased { text: text.into() });
    };
    let click = |ui: &ComponentInstance| {
        let position = box_at(ui);
        ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        settle(ui);
    };
    click(&ui);
    press(&ui, "2");
    press(&ui, "0");
    press(&ui, ".");
    press(&ui, "5");
    press(&ui, "\n");
    settle(&ui);
    assert_eq!(ui.get_property("pressed").unwrap(), Value::String("typed 20.5".into()));
    ui.set_property("pressed", Value::String("".into())).unwrap();
    click(&ui);
    press(&ui, "9");
    press(&ui, &slint::SharedString::from(Key::Escape));
    press(&ui, "\n");
    settle(&ui);
    assert_eq!(ui.get_property("pressed").unwrap(), Value::String("".into()), "Escape leaves it as it was");
}

#[test]
fn a_screen_reader_cant_press_a_disabled_button_or_switch() {
    let ui = rig();
    for name in ["Off button", "Off switch"] {
        labelled(&ui, name)[0].invoke_accessible_default_action();
        assert_eq!(ui.get_property("pressed").unwrap(), Value::String("".into()), "{name}");
    }
    labelled(&ui, "Light")[0].invoke_accessible_default_action();
    assert_eq!(ui.get_property("pressed").unwrap(), Value::String("Light".into()));
}

fn hover(ui: &ComponentInstance, e: &ElementHandle) {
    let (p, s) = (e.absolute_position(), e.size());
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0) });
}

fn wait(ms: u64) {
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(ms));
}

// test_tooltips.py::test_tooltips_show_sooner: Qt's own wait is 700 ms and Python's 250. Slint 1.18 keeps Tooltip's at 500 and
// doesn't let a program change it, so this holds it to sooner than Qt's.
#[test]
fn tooltips_show_sooner() {
    let ui = rig();
    let owner = labelled(&ui, "Tipped").remove(0);
    hover(&ui, &owner);
    wait(450);
    assert!(labelled(&ui, "About the owner").is_empty(), "not straight away");
    wait(100);
    assert!(!labelled(&ui, "About the owner").is_empty(), "up in under 700 ms");
}

#[test]
fn stretch_slider_sets_a_floor_not_a_fixed_width() {
    let ui = rig();
    let width = |name: &str| labelled(&ui, name).remove(0).size().width;
    assert_eq!(width("Wide room"), 300.0, "it takes the room there is");
    assert_eq!(width("Narrow room"), 84.0, "and doesn't go under its floor");
}

#[test]
fn a_tip_stays_while_the_mouse_is_on_its_control() {
    let ui = rig();
    let owner = labelled(&ui, "Tipped").remove(0);
    hover(&ui, &owner);
    wait(600);
    wait(60_000);
    assert!(!labelled(&ui, "About the owner").is_empty(), "no time limit while the mouse is on it");
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(2.0, 2.0) });
    wait(10);
    assert!(labelled(&ui, "About the owner").is_empty(), "moved off: it goes");
}

#[test]
fn a_dropdown_says_its_tip_and_its_arrows_skip_what_cant_be_picked() {
    let ui = rig();
    let field = labelled(&ui, "Picked").remove(0);
    assert_eq!(field.accessible_description().as_deref(), Some("About the field"));
    hover(&ui, &field);
    wait(600);
    assert!(!labelled(&ui, "About the field").is_empty() );
    click(&ui, &labelled(&ui, "Picked").remove(0));
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: slint::platform::Key::Escape.into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: slint::platform::Key::Escape.into() });
    let down: slint::SharedString = slint::platform::Key::DownArrow.into();
    key(&ui, &down);
    assert_eq!(ui.get_property("pressed").unwrap(), Value::String("row 2".into()));
}
