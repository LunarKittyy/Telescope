// How the kit's controls take real pointer and key events: scroll bars under a drag, number boxes, and focus rings.

use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use telescope_shell::{compile, input, offscreen};

fn window(doc: &str) -> ComponentInstance {
    offscreen::install();
    let ui = compile::compile("test.slint", doc, Vec::new(), "T").map(|d| d.create().unwrap()).unwrap_or_else(|e| panic!("{e}"));
    ui.window().set_size(slint::LogicalSize::new(600.0, 500.0));
    ui.show().unwrap();
    ui
}

fn num(ui: &ComponentInstance, name: &str) -> f32 {
    match ui.get_property(name).unwrap() {
        Value::Number(n) => n as f32,
        v => panic!("{name}: {v:?}"),
    }
}

fn text(ui: &ComponentInstance, name: &str) -> String {
    match ui.get_property(name).unwrap() {
        Value::String(s) => s.to_string(),
        v => panic!("{name}: {v:?}"),
    }
}

/// Changed handlers run as a window draws.
fn settle(ui: &ComponentInstance) {
    offscreen::draw(ui.window(), 600.0, 500.0, 1.0);
}

const AT: LogicalPosition = LogicalPosition { x: 105.0, y: 0.0 };

fn move_to(ui: &ComponentInstance, y: f32) {
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(AT.x, y) });
}

fn press(ui: &ComponentInstance, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position });
    input::pressed_at(ui, position);
    ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
}

fn release(ui: &ComponentInstance, x: f32, y: f32) {
    ui.window().dispatch_event(WindowEvent::PointerReleased { position: LogicalPosition::new(x, y), button: PointerEventButton::Left });
}

fn click(ui: &ComponentInstance, x: f32, y: f32) {
    press(ui, x, y);
    release(ui, x, y);
    settle(ui);
}

fn keys(ui: &ComponentInstance, text: &str) {
    for c in text.chars() {
        ui.window().dispatch_event(WindowEvent::KeyPressed { text: c.to_string().into() });
        ui.window().dispatch_event(WindowEvent::KeyReleased { text: c.to_string().into() });
    }
    settle(ui);
}

fn empty_box(ui: &ComponentInstance) {
    key(ui, Key::End);
    for _ in 0..10 {
        key(ui, Key::Backspace);
    }
}

fn key(ui: &ComponentInstance, key: Key) {
    keys(ui, &slint::SharedString::from(key));
}

// A bar 480 tall over 1600 of content in a 480 view: a thumb 144 tall with 336 of travel for 1120 of range
const BAR: &str = r#"
import { ColumnScrollbar } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    in-out property <length> offset;
    out property <float> position: offset / 1px;
    ColumnScrollbar { x: 100px; height: 480px; content: 1600px; viewport: 480px; offset <=> root.offset; }
}
"#;

const THUMB: f32 = 144.0;
const TRAVEL: f32 = 480.0 - THUMB;
const RANGE: f32 = 1600.0 - 480.0;

fn thumb_top(ui: &ComponentInstance) -> f32 {
    num(ui, "position") * TRAVEL / RANGE
}

#[test]
fn a_dragged_thumb_stays_under_the_cursor_all_the_way() {
    let ui = &window(BAR);
    press(ui, AT.x, 20.0);
    for step in 1..=40 {
        let y = 20.0 + step as f32 * 8.0;
        move_to(ui, y);
        settle(ui);
        assert!((thumb_top(&ui) - (y - 20.0)).abs() < 0.01, "cursor at {y}: thumb top {}", thumb_top(&ui));
    }
    // Past the end it waits at the end, and coming back it's under the cursor again
    move_to(ui, 470.0);
    assert!((num(ui, "position") - RANGE).abs() < 0.01);
    for step in 1..=20 {
        let y = 470.0 - step as f32 * 15.0;
        move_to(ui, y);
        let wanted = (y - 20.0).clamp(0.0, TRAVEL);
        assert!((thumb_top(&ui) - wanted).abs() < 0.01, "cursor at {y}: thumb top {}", thumb_top(&ui));
    }
    release(ui, AT.x, 20.0);
}

#[test]
fn one_drag_covers_the_whole_height() {
    let ui = &window(BAR);
    press(ui, AT.x, THUMB - 1.0);
    move_to(ui, 479.0);
    assert!((num(ui, "position") - RANGE).abs() < 0.01, "{}", num(ui, "position"));
    release(ui, AT.x, 479.0);
}

#[test]
fn a_click_in_the_trough_pages() {
    let ui = &window(BAR);
    click(ui, AT.x, 300.0);
    assert_eq!(num(ui, "position"), 480.0);
}

#[test]
fn a_lost_release_lets_go_of_the_thumb() {
    let ui = &window(BAR);
    press(ui, AT.x, 20.0);
    move_to(ui, 120.0);
    let held = num(ui, "position");
    assert!(held > 0.0);
    input::end_press(ui.window());
    move_to(ui, 300.0);
    settle(ui);
    assert_eq!(num(ui, "position"), held, "the thumb no longer follows");
    click(ui, AT.x, 5.0);
    assert!(num(ui, "position") < held, "and the bar works as usual after it");
}

// A value held to -24..24 by its owner, as the plugins do, with a slider moving it too
const NUMBER: &str = r#"
import { NumberField, Slider, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    in-out property <int> level: 6;
    out property <string> box-focus: field.has-focus ? "yes" : "no";
    in-out property <int> commits;
    public function focus-home() { keys.focus(); }
    keys := FocusScope {
        focus-on-click: false;
        field := NumberField {
            x: 10px;
            y: 10px;
            width: 100px;
            value: root.level + " dB";
            committed(t) => {
                root.commits += 1;
                if (t.to-float() != 0 || t == "0") { root.level = clamp(t.to-float(), -24, 24); }
            }
        }
        Slider { x: 10px; y: 60px; width: 300px; minimum: -24; maximum: 24; value: level; moved(v) => { root.level = v; } }
    }
}
"#;

#[test]
fn an_empty_box_gets_its_value_back_on_enter() {
    let ui = &window(NUMBER);
    click(ui, 60.0, 25.0);
    empty_box(ui);
    key(ui, Key::Return);
    settle(ui);
    assert_eq!(boxed(ui), "6 dB");
}

/// What the box holds, by its accessible value.
fn boxed(ui: &ComponentInstance) -> String {
    use i_slint_backend_testing::ElementHandle;
    let found = ElementHandle::find_by_element_type_name(ui, "TextField").next().expect("the box");
    found.accessible_value().unwrap_or_default().to_string()
}

#[test]
fn a_slider_refills_a_box_left_empty() {
    let ui = &window(NUMBER);
    click(ui, 60.0, 25.0);
    empty_box(ui);
    assert_eq!(boxed(ui), "");
    // A drag on the slider with the box still holding the focus
    press(ui, 100.0, 78.0);
    release(ui, 100.0, 78.0);
    settle(ui);
    assert_ne!(num(ui, "level"), 6.0);
    assert_eq!(boxed(ui), format!("{} dB", num(ui, "level")));
}

#[test]
fn a_typed_value_past_the_limit_ends_at_the_limit_and_shows_it() {
    let ui = &window(NUMBER);
    ui.set_property("level", Value::Number(-24.0)).unwrap();
    settle(ui);
    click(ui, 60.0, 25.0);
    empty_box(ui);
    keys(ui, "-500");
    key(ui, Key::Return);
    settle(ui);
    assert_eq!(num(ui, "level"), -24.0, "the value didn't change");
    assert_eq!(boxed(ui), "-24 dB", "and the box says so, though nothing changed to tell it");
}

#[test]
fn nonsense_goes_back_on_leaving_the_box() {
    let ui = &window(NUMBER);
    click(ui, 60.0, 25.0);
    empty_box(ui);
    keys(ui, "abc");
    click(ui, 500.0, 400.0);
    assert_eq!(text(ui, "box-focus"), "no", "a click elsewhere took the focus");
    assert_eq!(boxed(ui), "6 dB");
}

#[test]
fn clicking_in_the_box_keeps_the_focus_in_it() {
    let ui = &window(NUMBER);
    click(ui, 60.0, 25.0);
    click(ui, 70.0, 25.0);
    assert_eq!(text(ui, "box-focus"), "yes");
}

const RING: &str = r#"
import { FocusRing, KeyFocus } from "@ui-kit/ui-kit.slint";
export { KeyFocus }
export component T inherits Window {
    out property <length> ring: ring.border-width;
    public function focus-home() { first.focus(); }
    first := FocusScope { x: 10px; y: 10px; width: 100px; height: 30px; }
    ring := FocusRing { shown: second.has-focus; width: 100px; height: 30px; }
    second := FocusScope { x: 10px; y: 60px; width: 100px; height: 30px; }
}
"#;

#[test]
fn focus_rings_show_for_the_keyboard_and_not_after_a_click() {
    let ui = &window(RING);
    click(ui, 50.0, 75.0);
    assert_eq!(num(ui, "ring"), 0.0, "focused by a click: no ring");
    input::focus_key(&ui);
    assert_eq!(num(ui, "ring"), 2.0, "the keyboard came into it");
    click(ui, 500.0, 400.0);
    assert_eq!(num(ui, "ring"), 0.0, "and a click anywhere ends that");
}
