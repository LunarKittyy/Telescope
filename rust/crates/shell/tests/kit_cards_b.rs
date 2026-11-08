// The kit pieces of the second group of card redesigns: the anchor frame, the text area and the change mark on a rail row. On the testing backend.
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};

const ANCHORS: &str = r#"
import { AnchorFrame } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 200px;
    height: 120px;
    in property <bool> on: true;
    in property <int> start: 0;
    out property <int> picks;
    out property <int> last;
    out property <int> now: frame.shown;
    frame := AnchorFrame {
        x: 10px;
        y: 10px;
        width: 140px;
        height: 98px;
        label: "Position";
        items: ["Bottom left", "Bottom centre", "Bottom right", "Top left", "Top right"];
        across: [0, 0.5, 1, 0, 1];
        down: [1, 1, 1, 0, 0];
        moves: [-1, 1, 3, -1, 0, 2, 3, -1, 1, -1, 4, -1, -1, 4, -1, 0, 3, -1, -1, 2];
        current: start;
        enabled: on;
        picked(i) => { picks += 1; last = i; }
    }
}
"#;

const AREA: &str = r#"
import { TextArea } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 220px;
    height: 120px;
    in property <bool> on: true;
    in-out property <string> body;
    out property <int> edits;
    TextArea {
        x: 10px;
        y: 10px;
        width: 200px;
        height: 100px;
        label: "Notes";
        skeleton: true;
        text <=> body;
        enabled: on;
        edited(t) => { edits += 1; }
    }
}
"#;

const RAIL: &str = r#"
import { RailRow, Icons, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component Test inherits Window {
    width: 120px;
    height: 40px;
    background: Theme.card;
    in property <bool> changed;
    in property <bool> on: true;
    RailRow {
        x: 4px;
        y: 4px;
        width: 100px;
        icon: Icons.sun;
        label: "Brightness";
        hue: #7fd3c4;
        changed: changed;
        enabled: on;
        Rectangle { }
    }
}
"#;

fn rig(doc: &str, size: (f32, f32), set: &[(&str, Value)]) -> ComponentInstance {
    telescope_shell::offscreen::install();
    let ui = telescope_shell::compile::compile("kit_cards_b.slint", doc, vec![], "Test").expect("the document compiles").create().unwrap();
    for (name, value) in set {
        ui.set_property(name, value.clone()).unwrap();
    }
    ui.window().set_size(slint::LogicalSize::new(size.0, size.1));
    ui.show().unwrap();
    ui
}

fn number(ui: &ComponentInstance, name: &str) -> f64 {
    match ui.get_property(name).unwrap() {
        Value::Number(n) => n,
        other => panic!("{name} is {other:?}"),
    }
}

fn press(ui: &ComponentInstance, key: impl Into<slint::SharedString>) {
    let text: slint::SharedString = key.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

fn click(ui: &ComponentInstance, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
}

/// The frame's corners and middle in window coordinates: the bar of an anchor sits 8px in from the edge.
const BOTTOM_LEFT: (f32, f32) = (10.0 + 8.0 + 15.0, 10.0 + 98.0 - 8.0 - 5.0);
const BOTTOM_CENTRE: (f32, f32) = (10.0 + 70.0, 10.0 + 98.0 - 8.0 - 5.0);
const TOP_RIGHT: (f32, f32) = (10.0 + 140.0 - 8.0 - 15.0, 10.0 + 8.0 + 5.0);

#[test]
fn an_anchor_frame_picks_the_anchor_that_is_clicked() {
    let ui = rig(ANCHORS, (200.0, 120.0), &[]);
    click(&ui, TOP_RIGHT.0, TOP_RIGHT.1);
    assert_eq!((number(&ui, "now"), number(&ui, "last"), number(&ui, "picks")), (4.0, 4.0, 1.0));
    click(&ui, BOTTOM_CENTRE.0, BOTTOM_CENTRE.1);
    assert_eq!((number(&ui, "now"), number(&ui, "last")), (1.0, 1.0));
    // A click on the bare frame picks nothing
    click(&ui, 80.0, 50.0);
    assert_eq!(number(&ui, "picks"), 2.0);
}

#[test]
fn an_anchor_frame_goes_where_the_arrows_send_it_and_nowhere_past_the_edge() {
    let ui = rig(ANCHORS, (200.0, 120.0), &[]);
    click(&ui, BOTTOM_LEFT.0, BOTTOM_LEFT.1);
    // Bottom left: nothing to the left or below, the top left above, the bottom centre to the right
    press(&ui, Key::LeftArrow);
    press(&ui, Key::DownArrow);
    assert_eq!(number(&ui, "picks"), 1.0, "nothing to the left of or below bottom left: only the click picked");
    press(&ui, Key::UpArrow);
    assert_eq!(number(&ui, "now"), 3.0);
    press(&ui, Key::RightArrow);
    assert_eq!(number(&ui, "now"), 4.0);
    press(&ui, Key::DownArrow);
    assert_eq!(number(&ui, "now"), 2.0);
    press(&ui, Key::LeftArrow);
    assert_eq!(number(&ui, "now"), 1.0);
}

#[test]
fn an_anchor_frame_follows_its_parent_and_a_disabled_one_does_nothing() {
    let ui = rig(ANCHORS, (200.0, 120.0), &[("start", Value::Number(2.0))]);
    assert_eq!(number(&ui, "now"), 2.0);
    ui.set_property("start", Value::Number(3.0)).unwrap();
    assert_eq!(number(&ui, "now"), 3.0);
    ui.set_property("on", Value::Bool(false)).unwrap();
    click(&ui, BOTTOM_CENTRE.0, BOTTOM_CENTRE.1);
    assert_eq!((number(&ui, "now"), number(&ui, "picks")), (3.0, 0.0));
}

#[test]
fn a_text_area_takes_typing_and_a_disabled_one_does_not() {
    let ui = rig(AREA, (220.0, 120.0), &[]);
    click(&ui, 100.0, 60.0);
    press(&ui, "h");
    press(&ui, "i");
    assert_eq!(ui.get_property("body").unwrap(), Value::String("hi".into()));
    assert_eq!(number(&ui, "edits"), 2.0);
    ui.set_property("on", Value::Bool(false)).unwrap();
    click(&ui, 100.0, 60.0);
    press(&ui, "x");
    assert_eq!(ui.get_property("body").unwrap(), Value::String("hi".into()));
}

fn drawn(ui: &ComponentInstance) -> Vec<u8> {
    telescope_shell::offscreen::draw(ui.window(), 120.0, 40.0, 2.0).expect("a picture").rgba
}

/// The brightest red-channel among the icon's pixels (the rail's first 28px): what the mark changes.
fn icon_brightness(p: &[u8]) -> u32 {
    let width = 240;
    (0..80).flat_map(|y| (0..60).map(move |x| (x, y))).map(|(x, y)| p[(y * width + x) * 4] as u32).max().unwrap()
}

#[test]
fn a_changed_row_brightens_its_icon_or_adds_a_dot_and_a_default_row_has_neither() {
    let ui = rig(RAIL, (120.0, 40.0), &[]);
    let rest = drawn(&ui);
    ui.set_property("changed", Value::Bool(true)).unwrap();
    let bright = drawn(&ui);
    assert!(icon_brightness(&bright) > icon_brightness(&rest), "the icon is brighter");
    // With the dot the icon stays as it was at rest and a dot appears at its corner
    ui.set_global_property("Theme", "mark-dot", Value::Bool(true)).unwrap();
    let dot = drawn(&ui);
    assert_ne!(dot, rest, "the dot is drawn");
    assert_ne!(dot, bright, "and it is not the brighter icon");
    // Disabled, the mark gives way to the grey
    ui.set_property("on", Value::Bool(false)).unwrap();
    ui.set_global_property("Theme", "mark-dot", Value::Bool(false)).unwrap();
    let off = drawn(&ui);
    ui.set_property("changed", Value::Bool(false)).unwrap();
    assert_eq!(off, drawn(&ui), "a disabled row looks the same changed or not");
}

const SWITCH_ROW: &str = r#"
import { SwitchRow, Icons, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component Test inherits Window {
    width: 300px;
    height: 80px;
    background: Theme.card;
    in property <bool> on: true;
    in property <string> name: "Widen for something in your hand";
    in-out property <bool> lit;
    out property <int> flips;
    out property <length> row-h: row.height;
    row := SwitchRow {
        x: 10px;
        y: 10px;
        width: 280px;
        icon: Icons.sun;
        label: name;
        hue: #7fd3c4;
        enabled: on;
        checked: lit;
        toggled(v) => { flips += 1; lit = v; }
    }
}
"#;

const NAMES: &str = r#"
import { PictureSegmented, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component Test inherits Window {
    width: 320px;
    height: 80px;
    background: Theme.card;
    in property <[string]> words: ["Auto", "A4", "Letter"];
    out property <bool> names: row.names-drawn;
    out property <length> row-h: row.height;
    row := PictureSegmented {
        x: 10px;
        y: 10px;
        width: 300px;
        label: "Shape";
        items: words;
        disabled: [false, false, false];
        show-names: true;
    }
}
"#;

fn flag(ui: &ComponentInstance, name: &str) -> bool {
    matches!(ui.get_property(name), Ok(Value::Bool(true)))
}

#[test]
fn a_lone_switch_row_carries_its_label_and_puts_the_switch_at_the_right_edge_centred() {
    let ui = rig(SWITCH_ROW, (300.0, 80.0), &[]);
    let picture = telescope_shell::offscreen::draw(ui.window(), 300.0, 80.0, 1.0).unwrap();
    // text between the icon rail (up to x = 10 + 28 + 8) and the switch: ink2 on the card, so some pixels are light
    let light = (20..60).flat_map(|y| (50..230).map(move |x| (x, y))).filter(|(x, y)| picture.rgba[(y * 300 + x) * 4] > 150).count();
    assert!(light > 150, "the label is drawn beside the icon ({light} light pixels)");
    let switch = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Widen for something in your hand")
        .find(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Switch))
        .expect("the switch");
    let (p, s) = (switch.absolute_position(), switch.size());
    assert!((p.x + s.width - 290.0).abs() < 1.5, "its right edge is the row's: {}", p.x + s.width);
    assert!(((p.y + s.height / 2.0) - (10.0 + number(&ui, "row-h") as f32 / 2.0)).abs() < 1.0, "centred on the row");
    // a click flips it and says so, and the row keeps its height
    click(&ui, p.x + s.width / 2.0, p.y + s.height / 2.0);
    assert_eq!(number(&ui, "flips"), 1.0);
    assert!(flag(&ui, "lit"));
    assert_eq!(number(&ui, "row-h"), 32.0);
}

#[test]
fn a_long_label_in_a_switch_row_is_cut_short_and_stays_in_its_row() {
    let ui = rig(SWITCH_ROW, (300.0, 80.0), &[("name", Value::String("Bildausschnitt vergr\u{f6}\u{df}ern, wenn du etwas in der Hand h\u{e4}ltst und noch mehr".into()))]);
    let picture = telescope_shell::offscreen::draw(ui.window(), 300.0, 80.0, 1.0).unwrap();
    // nothing of the label reaches into the switch's place at the right edge, and nothing is drawn below the row
    let below = (52..80).flat_map(|y| (0..300).map(move |x| (x, y))).filter(|(x, y)| picture.rgba[(y * 300 + x) * 4] > 150).count();
    assert_eq!(below, 0, "the text stays on its row");
    assert_eq!(number(&ui, "row-h"), 32.0);
}

#[test]
fn a_row_of_pictures_draws_every_name_or_none() {
    let ui = rig(NAMES, (320.0, 80.0), &[]);
    assert!(flag(&ui, "names"), "short names fit under their drawings");
    // one name too long for its tile takes every name away, not only its own
    let words = slint::ModelRc::new(slint::VecModel::from(vec![Value::String("Auto".into()), Value::String("A4".into()), Value::String("Quadratisch-extra-lang".into())]));
    ui.set_property("words", Value::Model(words)).unwrap();
    drawn(&ui);
    assert!(!flag(&ui, "names"), "one name doesn't fit: none is drawn");
    assert_eq!(number(&ui, "row-h"), 48.0, "and the tiles keep their height");
    let again = slint::ModelRc::new(slint::VecModel::from(vec![Value::String("Auto".into()), Value::String("A4".into()), Value::String("Letter".into())]));
    ui.set_property("words", Value::Model(again)).unwrap();
    drawn(&ui);
    assert!(flag(&ui, "names"), "they come back when they fit");
}
