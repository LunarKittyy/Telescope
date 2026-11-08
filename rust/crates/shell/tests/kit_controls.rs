// The kit controls of the card redesigns (toggle tile, picture segmented, pad, dial, threshold bar, icon rail): values, keys, pointer, and no size change between states, on the testing backend.
use i_slint_backend_testing::ElementHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};

const TILE: &str = r#"
import { ToggleTile, Icons } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 220px;
    height: 60px;
    in property <bool> on: true;
    in property <bool> lit;
    out property <int> flips;
    out property <bool> last;
    out property <bool> now: tile.shown;
    tile := ToggleTile {
        x: 10px;
        y: 10px;
        width: 200px;
        name: "Torch";
        text: "Torch";
        icon: Icons.torch;
        checked: lit;
        enabled: on;
        toggled(v) => { flips += 1; last = v; }
    }
}
"#;

const PICTURES: &str = r#"
import { PictureSegmented } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 400px;
    height: 80px;
    in property <bool> on: true;
    in property <int> start: 0;
    out property <int> picks;
    out property <int> last;
    out property <int> now: ps.shown;
    out property <length> pitch: ps.pitch;
    ps := PictureSegmented {
        x: 10px;
        y: 10px;
        width: 380px;
        label: "Paper";
        items: ["A", "B", "C"];
        disabled: [false, false, !on];
        current: start;
        picked(i) => { picks += 1; last = i; }
        Rectangle { width: 10px; height: 10px; background: red; }
    }
}
"#;

const PAD: &str = r#"
import { Pad, Button } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 340px;
    height: 230px;
    in property <bool> on: true;
    in property <bool> up: true;
    in property <float> sx: 5500;
    in property <float> sy: 10;
    out property <int> moves;
    out property <int> releases;
    out property <int> resets;
    out property <float> mx;
    out property <float> my;
    out property <float> rx;
    out property <float> ry;
    out property <float> now-x: pad.shown-x;
    out property <float> now-y: pad.shown-y;
    out property <length> hx: pad.handle-x;
    out property <length> hy: pad.handle-y;
    pad := Pad {
        x: 10px;
        y: 10px;
        width: 300px;
        height: 200px;
        label: "Balance";
        x-minimum: 2000;
        x-maximum: 10000;
        step-x: 100;
        y-minimum: -50;
        y-maximum: 50;
        step-y: 1;
        value-x: sx;
        value-y: sy;
        y-up: up;
        has-default: true;
        default-x: 5500;
        default-y: 0;
        enabled: on;
        readout: "5500 K";
        moved(a, b) => { moves += 1; mx = a; my = b; }
        released(a, b) => { releases += 1; rx = a; ry = b; }
        Button { x: 230px; y: 4px; width: 60px; height: 24px; text: "Reset"; clicked => { resets += 1; } }
    }
}
"#;

const DIAL: &str = r#"
import { Dial } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 220px;
    height: 70px;
    in property <bool> on: true;
    in property <bool> empty;
    in property <int> start: 2;
    out property <int> moves;
    out property <int> releases;
    out property <int> last-moved;
    out property <int> last-released;
    out property <int> now: dial.shown;
    dial := Dial {
        x: 10px;
        y: 10px;
        width: 200px;
        label: "ISO";
        stops: empty ? [] : ["100", "200", "400", "800", "1600"];
        current: start;
        enabled: on;
        moved(i) => { moves += 1; last-moved = i; }
        released(i) => { releases += 1; last-released = i; }
    }
}
"#;

const BAR: &str = r#"
import { Slider, Icons } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 340px;
    height: 60px;
    in property <bool> on: true;
    in property <bool> empty;
    in property <string> text: "20%";
    out property <int> moves;
    out property <int> releases;
    out property <float> last-moved;
    out property <float> last-released;
    out property <float> now: bar.shown;
    bar := Slider {
        x: 10px;
        y: 10px;
        width: 320px;
        label: "Level";
        fill: "threshold";
        minimum: 0;
        maximum: 100;
        value: 20;
        has-default: true;
        default-value: 20;
        level: 74;
        has-level: !empty;
        level-text: empty ? "-" : "74%";
        level-icon: Icons.bolt;
        has-level-icon: !empty;
        readout: text;
        enabled: on;
        moved(v) => { moves += 1; last-moved = v; }
        released(v) => { releases += 1; last-released = v; }
    }
}
"#;

const RAIL: &str = r#"
import { RailRow, Slider, Icons } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 400px;
    height: 120px;
    in property <bool> on: true;
    RailRow {
        x: 10px;
        y: 10px;
        width: 380px;
        icon: Icons.sun;
        label: "Brightness";
        tip: "How bright the picture is";
        enabled: on;
        Slider { label: "Bright"; value: 40; enabled: on; }
    }
    RailRow {
        x: 10px;
        y: 60px;
        width: 380px;
        has-icon: false;
        enabled: on;
        Slider { label: "Warm"; value: 60; enabled: on; }
    }
}
"#;

fn rig(doc: &str, size: (f32, f32), set: &[(&str, Value)]) -> ComponentInstance {
    thread_local!(static INIT: std::sync::Once = const { std::sync::Once::new() });
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    let ui = telescope_shell::compile::compile("kit_controls.slint", doc, vec![], "Test").expect("the document compiles").create().unwrap();
    for (name, value) in set {
        ui.set_property(name, value.clone()).unwrap();
    }
    ui.window().set_size(slint::LogicalSize::new(size.0, size.1));
    ui.show().unwrap();
    ui
}

fn yes(on: bool) -> Value {
    Value::Bool(on)
}

fn num(n: f64) -> Value {
    Value::Number(n)
}

fn press(ui: &ComponentInstance, key: impl Into<slint::SharedString>) {
    let text: slint::SharedString = key.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

fn shifted(ui: &ComponentInstance, key: Key) {
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
    press(ui, key);
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
}

fn number(ui: &ComponentInstance, name: &str) -> f64 {
    match ui.get_property(name).unwrap() {
        Value::Number(n) => n,
        other => panic!("{name} is {other:?}"),
    }
}

fn truth(ui: &ComponentInstance, name: &str) -> bool {
    match ui.get_property(name).unwrap() {
        Value::Bool(b) => b,
        other => panic!("{name} is {other:?}"),
    }
}

fn at(x: f32, y: f32) -> LogicalPosition {
    LogicalPosition::new(x, y)
}

fn down(ui: &ComponentInstance, position: LogicalPosition) {
    ui.window().dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
}

fn drag(ui: &ComponentInstance, position: LogicalPosition) {
    ui.window().dispatch_event(WindowEvent::PointerMoved { position });
}

fn up(ui: &ComponentInstance, position: LogicalPosition) {
    ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
}

fn click(ui: &ComponentInstance, position: LogicalPosition) {
    down(ui, position);
    up(ui, position);
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.01
}

/// The size of the element a screen reader calls `label`.
fn size_of(ui: &ComponentInstance, label: &str) -> (f32, f32) {
    let found = ElementHandle::find_by_accessible_label(ui, label).next().unwrap_or_else(|| panic!("no {label}"));
    let size = found.size();
    (size.width, size.height)
}

// ---- toggle tile

#[test]
fn a_toggle_tile_flips_on_a_click_and_says_so() {
    let ui = rig(TILE, (220.0, 60.0), &[]);
    assert!(!truth(&ui, "now"));
    click(&ui, at(100.0, 30.0));
    assert_eq!((number(&ui, "flips"), truth(&ui, "last"), truth(&ui, "now")), (1.0, true, true));
    let tile = ElementHandle::find_by_accessible_label(&ui, "Torch").next().expect("the tile");
    assert_eq!(tile.accessible_checked(), Some(true));
    click(&ui, at(100.0, 30.0));
    assert_eq!((number(&ui, "flips"), truth(&ui, "last"), truth(&ui, "now")), (2.0, false, false));
}

#[test]
fn a_toggle_tile_flips_on_space_and_enter() {
    let ui = rig(TILE, (220.0, 60.0), &[]);
    press(&ui, Key::Tab);
    press(&ui, " ");
    assert_eq!((number(&ui, "flips"), truth(&ui, "now")), (1.0, true));
    press(&ui, "\n");
    assert_eq!((number(&ui, "flips"), truth(&ui, "now")), (2.0, false));
}

#[test]
fn a_toggle_tile_follows_its_parent_and_a_disabled_one_does_nothing() {
    let ui = rig(TILE, (220.0, 60.0), &[("lit", yes(true))]);
    assert!(truth(&ui, "now"), "the parent says it is on");
    click(&ui, at(100.0, 30.0));
    assert!(!truth(&ui, "now"), "a click turned it off");
    ui.set_property("lit", yes(false)).unwrap();
    // The parent's word only counts once it has been looked at, as in the running window
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: at(5.0, 5.0) });
    ui.set_property("lit", yes(true)).unwrap();
    assert!(truth(&ui, "now"), "and the parent saying on again turns it on");
    ui.set_property("on", yes(false)).unwrap();
    click(&ui, at(100.0, 30.0));
    press(&ui, Key::Tab);
    press(&ui, " ");
    assert_eq!(number(&ui, "flips"), 1.0, "a disabled tile says nothing");
    assert!(truth(&ui, "now"));
}

// ---- picture segmented

#[test]
fn a_picture_segmented_picks_on_a_click_and_on_keys() {
    let ui = rig(PICTURES, (400.0, 80.0), &[]);
    let pitch = number(&ui, "pitch") as f32;
    let cell = |i: f32| at(10.0 + i * pitch + 50.0, 34.0);
    click(&ui, cell(1.0));
    assert_eq!((number(&ui, "now"), number(&ui, "last"), number(&ui, "picks")), (1.0, 1.0, 1.0));
    // The focus is on the clicked option, so the arrows go on from it and pick as they go
    press(&ui, Key::RightArrow);
    assert_eq!((number(&ui, "now"), number(&ui, "picks")), (2.0, 2.0));
    press(&ui, Key::RightArrow);
    assert_eq!(number(&ui, "picks"), 2.0, "nothing past the last");
    press(&ui, Key::LeftArrow);
    press(&ui, Key::LeftArrow);
    assert_eq!((number(&ui, "now"), number(&ui, "last")), (0.0, 0.0));
    press(&ui, Key::Tab);
    press(&ui, " ");
    assert_eq!(number(&ui, "last"), 1.0, "Space picks the option the focus is on");
}

#[test]
fn a_picture_segmented_follows_its_parent_and_skips_a_disabled_option() {
    let ui = rig(PICTURES, (400.0, 80.0), &[("on", yes(false))]);
    let pitch = number(&ui, "pitch") as f32;
    ui.set_property("start", num(1.0)).unwrap();
    assert_eq!(number(&ui, "now"), 1.0);
    click(&ui, at(10.0 + 2.0 * pitch + 50.0, 34.0));
    assert_eq!((number(&ui, "now"), number(&ui, "picks")), (1.0, 0.0), "a disabled option can't be clicked");
    click(&ui, at(10.0 + 1.0 * pitch + 50.0, 34.0));
    press(&ui, Key::RightArrow);
    assert_eq!(number(&ui, "now"), 1.0, "nor reached by an arrow");
}

// ---- pad

#[test]
fn a_pad_puts_its_handle_where_its_numbers_say() {
    let ui = rig(PAD, (340.0, 230.0), &[]);
    assert!(close(number(&ui, "now-x"), 5500.0) && close(number(&ui, "now-y"), 10.0));
    // 12 px in, then 4375 of 8000 across the other 276, and 60 of 100 up the other 176 with the top for the most
    assert!(close(number(&ui, "hx"), 12.0 + 0.4375 * 276.0), "x: {}", number(&ui, "hx"));
    assert!(close(number(&ui, "hy"), 12.0 + 0.4 * 176.0), "y: {}", number(&ui, "hy"));
    ui.set_property("up", yes(false)).unwrap();
    assert!(close(number(&ui, "hy"), 12.0 + 0.6 * 176.0), "y with y down: {}", number(&ui, "hy"));
}

#[test]
fn a_pad_takes_a_drag_and_says_moved_then_released_with_both_numbers() {
    let ui = rig(PAD, (340.0, 230.0), &[]);
    // The middle of the pad's room: 6000 across, 0 up and down
    down(&ui, at(10.0 + 150.0, 10.0 + 100.0));
    assert_eq!((number(&ui, "moves"), number(&ui, "mx"), number(&ui, "my")), (1.0, 6000.0, 0.0));
    drag(&ui, at(10.0 + 12.0, 10.0 + 12.0));
    assert_eq!((number(&ui, "mx"), number(&ui, "my")), (2000.0, 50.0), "the top left, with y up");
    drag(&ui, at(10.0 + 299.0, 10.0 + 199.0));
    assert_eq!((number(&ui, "mx"), number(&ui, "my")), (10000.0, -50.0), "held to the ends past the pad's edge");
    assert_eq!(number(&ui, "releases"), 0.0);
    up(&ui, at(10.0 + 299.0, 10.0 + 199.0));
    assert_eq!((number(&ui, "releases"), number(&ui, "rx"), number(&ui, "ry")), (1.0, 10000.0, -50.0));
    assert!(close(number(&ui, "now-x"), 10000.0) && close(number(&ui, "now-y"), -50.0));
}

#[test]
fn a_pad_goes_back_to_its_default_on_a_double_click_and_on_home() {
    let ui = rig(PAD, (340.0, 230.0), &[]);
    for _ in 0..2 {
        click(&ui, at(10.0 + 20.0, 10.0 + 20.0));
    }
    // The double-click's own two presses moved it, then it went back to the default
    assert_eq!((number(&ui, "now-x"), number(&ui, "now-y")), (5500.0, 0.0));
    press(&ui, Key::RightArrow);
    assert_eq!(number(&ui, "now-x"), 5600.0);
    press(&ui, Key::Home);
    assert_eq!((number(&ui, "now-x"), number(&ui, "now-y")), (5500.0, 0.0));
}

#[test]
fn a_pad_moves_by_the_arrows_and_shift_and_says_what_a_drag_says() {
    let ui = rig(PAD, (340.0, 230.0), &[]);
    press(&ui, Key::Tab);
    press(&ui, Key::RightArrow);
    assert_eq!((number(&ui, "now-x"), number(&ui, "now-y")), (5600.0, 10.0));
    assert_eq!((number(&ui, "moves"), number(&ui, "releases"), number(&ui, "mx"), number(&ui, "rx")), (1.0, 1.0, 5600.0, 5600.0));
    press(&ui, Key::UpArrow);
    assert_eq!(number(&ui, "now-y"), 11.0, "with y up, Up is more");
    press(&ui, Key::DownArrow);
    press(&ui, Key::DownArrow);
    assert_eq!(number(&ui, "now-y"), 9.0);
    press(&ui, Key::LeftArrow);
    assert_eq!(number(&ui, "now-x"), 5500.0);
    // A tenth of the range in whole steps: 800 across, 10 up and down
    shifted(&ui, Key::RightArrow);
    assert_eq!(number(&ui, "now-x"), 6300.0);
    shifted(&ui, Key::UpArrow);
    assert_eq!(number(&ui, "now-y"), 19.0);
    assert_eq!(number(&ui, "moves"), number(&ui, "releases"));
}

#[test]
fn a_pad_with_y_down_has_up_for_less() {
    let ui = rig(PAD, (340.0, 230.0), &[("up", yes(false))]);
    press(&ui, Key::Tab);
    press(&ui, Key::UpArrow);
    assert_eq!(number(&ui, "now-y"), 9.0);
    down(&ui, at(10.0 + 12.0, 10.0 + 12.0));
    assert_eq!(number(&ui, "my"), -50.0, "the top is the least with y down");
}

#[test]
fn a_pad_says_nothing_when_nothing_changed_and_a_disabled_one_ignores_everything() {
    let ui = rig(PAD, (340.0, 230.0), &[("sx", num(10000.0))]);
    press(&ui, Key::Tab);
    press(&ui, Key::RightArrow);
    assert_eq!(number(&ui, "moves"), 0.0, "already at the end");
    ui.set_property("on", yes(false)).unwrap();
    press(&ui, Key::LeftArrow);
    click(&ui, at(10.0 + 150.0, 10.0 + 100.0));
    assert_eq!((number(&ui, "moves"), number(&ui, "now-x")), (0.0, 10000.0));
}

#[test]
fn a_button_on_a_pad_is_pressed_and_the_pad_stays_put() {
    let ui = rig(PAD, (340.0, 230.0), &[]);
    click(&ui, at(10.0 + 260.0, 10.0 + 16.0));
    assert_eq!((number(&ui, "resets"), number(&ui, "moves"), number(&ui, "now-x")), (1.0, 0.0, 5500.0));
}

#[test]
fn a_pad_left_to_itself_is_three_across_to_two_high_and_in_a_layout_never_flatter_than_two_to_one() {
    let doc = |height: u32| {
        format!(
            r#"
import {{ Pad }} from "@ui-kit/ui-kit.slint";
export component Test inherits Window {{
    width: 300px;
    height: {height}px;
    VerticalLayout {{
        padding: 0px;
        alignment: start;
        Pad {{ label: "Free"; }}
    }}
}}
"#
        )
    };
    let ui = rig(&doc(400), (300.0, 400.0), &[]);
    let (w, h) = size_of(&ui, "Free");
    assert!((h / w - 2.0 / 3.0).abs() < 0.01, "free: {w} by {h}");
    let ui = rig(&doc(100), (300.0, 100.0), &[]);
    let (w, h) = size_of(&ui, "Free");
    assert!(h / w >= 0.5 - 0.001, "a layout can't squeeze it flatter than 2:1: {w} by {h}");
}

// ---- dial

#[test]
fn a_dial_reads_its_stop_and_steps_by_the_keys() {
    let ui = rig(DIAL, (220.0, 70.0), &[]);
    let dial = ElementHandle::find_by_accessible_label(&ui, "ISO").find(|e| e.accessible_value().is_some()).expect("the dial");
    assert_eq!((number(&ui, "now"), dial.accessible_value().as_deref()), (2.0, Some("400")));
    press(&ui, Key::Tab);
    press(&ui, Key::RightArrow);
    assert_eq!((number(&ui, "now"), number(&ui, "moves"), number(&ui, "releases"), number(&ui, "last-moved"), number(&ui, "last-released")), (3.0, 1.0, 1.0, 3.0, 3.0));
    press(&ui, Key::LeftArrow);
    press(&ui, Key::DownArrow);
    assert_eq!(number(&ui, "now"), 1.0);
    press(&ui, Key::Home);
    assert_eq!(number(&ui, "now"), 0.0);
    press(&ui, Key::LeftArrow);
    assert_eq!(number(&ui, "moves"), 4.0, "nothing before the first");
    press(&ui, Key::End);
    assert_eq!(number(&ui, "now"), 4.0);
    press(&ui, Key::PageDown);
    shifted(&ui, Key::LeftArrow);
    assert_eq!(number(&ui, "now"), 2.0, "a tenth of five stops is still one, so a page is a step");
    assert_eq!(number(&ui, "moves"), number(&ui, "releases"));
}

#[test]
fn a_dial_follows_its_parent_and_scrolls_a_stop_to_a_notch() {
    let ui = rig(DIAL, (220.0, 70.0), &[]);
    ui.set_property("start", num(1.0)).unwrap();
    assert_eq!(number(&ui, "now"), 1.0);
    let over = at(110.0, 34.0);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: over });
    // A mouse wheel's notch is 60
    ui.window().dispatch_event(WindowEvent::PointerScrolled { position: over, delta_x: 0.0, delta_y: 60.0 });
    assert_eq!((number(&ui, "now"), number(&ui, "last-moved"), number(&ui, "releases")), (2.0, 2.0, 1.0));
    ui.window().dispatch_event(WindowEvent::PointerScrolled { position: over, delta_x: 0.0, delta_y: -60.0 });
    ui.window().dispatch_event(WindowEvent::PointerScrolled { position: over, delta_x: 0.0, delta_y: -60.0 });
    assert_eq!(number(&ui, "now"), 0.0);
    // Smaller turns, as a touchpad gives, add up to one
    for _ in 0..3 {
        ui.window().dispatch_event(WindowEvent::PointerScrolled { position: over, delta_x: 0.0, delta_y: 20.0 });
    }
    assert_eq!(number(&ui, "now"), 1.0);
}

#[test]
fn a_dial_follows_a_drag_and_settles_on_a_stop() {
    let ui = rig(DIAL, (220.0, 70.0), &[]);
    // 200 wide: the stops are 68 apart; the strip follows the pointer, so dragging left goes on to the next stop
    down(&ui, at(110.0, 34.0));
    assert_eq!(number(&ui, "moves"), 0.0, "a press alone changes nothing");
    drag(&ui, at(110.0 - 68.0, 34.0));
    assert_eq!((number(&ui, "now"), number(&ui, "last-moved")), (3.0, 3.0));
    drag(&ui, at(110.0 - 68.0 * 3.0, 34.0));
    assert_eq!(number(&ui, "now"), 4.0, "held to the last stop");
    assert_eq!(number(&ui, "releases"), 0.0);
    up(&ui, at(110.0 - 68.0 * 3.0, 34.0));
    assert_eq!((number(&ui, "releases"), number(&ui, "last-released")), (1.0, 4.0));
    // The other way
    down(&ui, at(110.0, 34.0));
    drag(&ui, at(110.0 + 68.0, 34.0));
    up(&ui, at(110.0 + 68.0, 34.0));
    assert_eq!(number(&ui, "now"), 3.0);
}

#[test]
fn a_dial_picks_the_name_beside_its_mark_on_a_click() {
    let ui = rig(DIAL, (220.0, 70.0), &[]);
    click(&ui, at(110.0 + 68.0, 34.0));
    assert_eq!((number(&ui, "now"), number(&ui, "moves"), number(&ui, "releases")), (3.0, 1.0, 1.0));
    click(&ui, at(110.0, 34.0));
    assert_eq!((number(&ui, "now"), number(&ui, "releases")), (3.0, 2.0), "a click on the mark's own stop changes nothing");
}

#[test]
fn a_dial_with_no_stops_or_disabled_says_nothing() {
    let ui = rig(DIAL, (220.0, 70.0), &[("empty", yes(true))]);
    press(&ui, Key::Tab);
    press(&ui, Key::RightArrow);
    press(&ui, Key::End);
    click(&ui, at(110.0 + 68.0, 34.0));
    assert_eq!((number(&ui, "moves"), number(&ui, "now")), (0.0, 0.0));
    let ui = rig(DIAL, (220.0, 70.0), &[("on", yes(false))]);
    press(&ui, Key::Tab);
    press(&ui, Key::RightArrow);
    click(&ui, at(110.0 + 68.0, 34.0));
    assert_eq!((number(&ui, "moves"), number(&ui, "now")), (0.0, 2.0));
}

// ---- threshold bar

fn bar_value(ui: &ComponentInstance) -> f64 {
    ElementHandle::find_by_accessible_label(ui, "Level").find(|e| e.accessible_value().is_some()).expect("the bar").accessible_value().unwrap().trim_end_matches('%').parse().unwrap()
}

#[test]
fn a_threshold_bar_is_a_slider_by_the_keys_and_the_pointer() {
    let ui = rig(BAR, (340.0, 60.0), &[]);
    assert_eq!((number(&ui, "now"), bar_value(&ui)), (20.0, 20.0));
    press(&ui, Key::Tab);
    press(&ui, Key::RightArrow);
    assert_eq!((number(&ui, "now"), number(&ui, "moves"), number(&ui, "releases"), number(&ui, "last-released")), (21.0, 1.0, 1.0, 21.0));
    shifted(&ui, Key::LeftArrow);
    assert_eq!(number(&ui, "now"), 11.0);
    press(&ui, Key::End);
    assert_eq!(number(&ui, "now"), 100.0);
    press(&ui, Key::Home);
    assert_eq!(number(&ui, "now"), 0.0);
    // The track is the whole 320 px, the handle's centre running from 7 px in to 7 px short of the end
    down(&ui, at(10.0 + 7.0 + 0.5 * 306.0, 26.0));
    up(&ui, at(10.0 + 7.0 + 0.5 * 306.0, 26.0));
    // This bar has a default, so a click on its track waits to see whether it is a double-click
    assert_eq!(number(&ui, "now"), 0.0, "the click waits for the double-click interval");
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    slint::platform::update_timers_and_animations();
    assert_eq!(number(&ui, "now"), 50.0);
}

#[test]
fn a_threshold_bars_live_value_and_handle_number_stay_clear_of_the_handle_at_both_ends() {
    let ui = rig(BAR, (340.0, 60.0), &[]);
    let span = |id: &str| {
        let e = ElementHandle::find_by_element_id(&ui, id).next().unwrap_or_else(|| panic!("no {id}"));
        (e.absolute_position().x, e.absolute_position().x + e.size().width)
    };
    press(&ui, Key::Tab);
    for (key, name, handle_at_max) in [(Key::End, "End", true), (Key::Home, "Home", false)] {
        press(&ui, key);
        // The handle glides to the end a key sent it to: let it arrive
        let _ = span("Slider::handle");
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(600));
        slint::platform::update_timers_and_animations();
        let (hl, hr) = span("Slider::handle");
        let (ll, lr) = span("Slider::level-box");
        let (nl, nr) = span("Slider::level-number");
        assert!(lr <= hl || ll >= hr, "the live value overlaps the handle at {name}: {ll}..{lr} vs {hl}..{hr}");
        assert!(nr <= hl || nl >= hr, "the handle number overlaps the handle at {name}");
        assert!(lr <= nl || ll >= nr, "the live value and the handle number overlap at {name}");
        // The live value is at the end away from the handle
        assert_eq!(ll < hl, handle_at_max);
    }
}

#[test]
fn a_threshold_bar_takes_a_typed_number_beside_the_handle() {
    let ui = rig(BAR, (340.0, 60.0), &[]);
    press(&ui, Key::Tab);
    press(&ui, "\n");
    press(&ui, "3");
    press(&ui, "5");
    press(&ui, "\n");
    assert_eq!((number(&ui, "now"), number(&ui, "moves"), number(&ui, "releases")), (35.0, 1.0, 1.0));
}

#[test]
fn a_click_on_a_threshold_bars_number_opens_the_box_and_a_double_click_on_the_track_resets() {
    let ui = rig(BAR, (340.0, 60.0), &[]);
    // The handle's number sits just right of the handle: the handle's centre is 7 + 0.2 * 306 in, the number 4 px beyond its edge
    click(&ui, at(10.0 + 7.0 + 0.2 * 306.0 + 7.0 + 4.0 + 12.0, 26.0));
    press(&ui, "3");
    press(&ui, "5");
    press(&ui, "\n");
    assert_eq!((number(&ui, "now"), number(&ui, "moves")), (35.0, 1.0), "a click on the number typed it");
    press(&ui, Key::Tab);
    press(&ui, Key::End);
    assert_eq!(number(&ui, "now"), 100.0);
    for _ in 0..2 {
        click(&ui, at(250.0, 26.0));
    }
    assert_eq!(number(&ui, "now"), 20.0, "a double-click on the track goes back to the default");
}

// ---- icon rail

#[test]
fn an_icon_rail_names_its_row_and_lines_the_controls_up() {
    let ui = rig(RAIL, (400.0, 120.0), &[]);
    let named = ElementHandle::find_by_accessible_label(&ui, "Brightness").next().expect("the row is found by its word");
    assert_eq!(named.accessible_id().as_deref(), Some("setting"));
    let x = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).find(|e| e.accessible_value().is_some()).expect(label).absolute_position().x;
    assert_eq!(x("Bright"), x("Warm"), "a row without an icon keeps the rail's room");
    assert!(x("Bright") > 10.0 + 28.0, "the controls start after the rail: {}", x("Bright"));
}

// ---- no jump between states

/// The size of what a screen reader calls `label` in every state the control has: it never changes.
fn same_in_every_state(doc: &str, window: (f32, f32), label: &str, states: &[&[(&str, Value)]], interact: impl Fn(&ComponentInstance)) {
    let mut seen: Vec<(String, (f32, f32))> = Vec::new();
    for set in states {
        let ui = rig(doc, window, set);
        let what = format!("{:?}", set.iter().map(|(n, v)| format!("{n}={v:?}")).collect::<Vec<_>>());
        seen.push((what.clone(), size_of(&ui, label)));
        interact(&ui);
        seen.push((format!("{what} then touched"), size_of(&ui, label)));
    }
    let first = seen[0].1;
    assert!(first.0 > 20.0 && first.1 > 20.0, "{label} has a size: {first:?}");
    for (what, size) in &seen {
        assert!((size.0 - first.0).abs() < 0.01 && (size.1 - first.1).abs() < 0.01, "{label} is {size:?} in {what} but {first:?} in {}", seen[0].0);
    }
}

#[test]
fn no_kit_control_changes_size_between_states() {
    let nothing = |_: &ComponentInstance| {};
    same_in_every_state(TILE, (220.0, 60.0), "Torch", &[&[], &[("lit", yes(true))], &[("on", yes(false))], &[("on", yes(false)), ("lit", yes(true))]], |ui| {
        click(ui, at(100.0, 30.0));
        press(ui, Key::Tab);
    });
    same_in_every_state(PICTURES, (400.0, 80.0), "Paper", &[&[], &[("start", num(2.0))], &[("on", yes(false))]], |ui| {
        down(ui, at(100.0, 34.0));
        up(ui, at(100.0, 34.0));
    });
    same_in_every_state(PAD, (340.0, 230.0), "Balance", &[&[], &[("up", yes(false))], &[("on", yes(false))], &[("sx", num(2000.0)), ("sy", num(-50.0))]], |ui| {
        down(ui, at(150.0, 110.0));
    });
    same_in_every_state(DIAL, (220.0, 70.0), "ISO", &[&[], &[("empty", yes(true))], &[("on", yes(false))], &[("start", num(0.0))], &[("start", num(4.0))]], |ui| {
        down(ui, at(110.0, 34.0));
        drag(ui, at(60.0, 34.0));
    });
    same_in_every_state(BAR, (340.0, 60.0), "Level", &[&[], &[("empty", yes(true))], &[("on", yes(false))], &[("text", Value::String("100%".into()))]], |ui| {
        down(ui, at(100.0, 26.0));
    });
    same_in_every_state(RAIL, (400.0, 120.0), "Bright", &[&[], &[("on", yes(false))]], nothing);
}
