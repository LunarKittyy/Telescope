// The kit slider from the keyboard: arrows, Shift and the Page keys, Home and End, and a typed value, each saying what a drag says
// (moved, then released). Driven through Slint's testing backend: nothing reaches the desktop.
use i_slint_backend_testing::ElementHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};

const DOC: &str = r#"
import { Slider } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 420px;
    height: 60px;
    in property <float> lo: 0;
    in property <float> hi: 100;
    in property <float> st: 1;
    in property <float> start: 50;
    in property <bool> on: true;
    in property <string> box;
    in property <string> words;
    out property <int> moves;
    out property <int> releases;
    out property <float> last-moved;
    out property <float> last-released;
    out property <string> typed-text;
    Slider {
        x: 10px;
        y: 10px;
        width: 300px;
        label: "Level";
        minimum: lo;
        maximum: hi;
        step: st;
        value: start;
        enabled: on;
        typing: box;
        readout: words;
        moved(v) => { moves += 1; last-moved = v; }
        released(v) => { releases += 1; last-released = v; }
        typed(t) => { typed-text = t; }
    }
}
"#;

fn rig(set: &[(&str, Value)]) -> ComponentInstance {
    thread_local!(static INIT: std::sync::Once = const { std::sync::Once::new() });
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    let ui = telescope_shell::compile::compile("slider_keys.slint", DOC, vec![], "Test").expect("the document compiles").create().unwrap();
    for (name, value) in set {
        ui.set_property(name, value.clone()).unwrap();
    }
    ui.window().set_size(slint::LogicalSize::new(420.0, 60.0));
    ui.show().unwrap();
    // The first Tab lands on the slider, the only thing here that takes the focus
    press(&ui, Key::Tab);
    ui
}

fn press(ui: &ComponentInstance, key: impl Into<slint::SharedString>) {
    let text: slint::SharedString = key.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

fn type_in(ui: &ComponentInstance, text: &str) {
    for c in text.chars() {
        press(ui, c.to_string());
    }
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

/// The slider's own value, as a screen reader reads it.
fn value(ui: &ComponentInstance) -> f64 {
    let slider = ElementHandle::find_by_accessible_label(ui, "Level").find(|e| e.accessible_value().is_some()).expect("the slider");
    slider.accessible_value().unwrap().parse().unwrap()
}

/// How many times it said moved and released, and the last of each: always the same, whatever set the value.
fn said(ui: &ComponentInstance) -> (f64, f64, f64, f64) {
    (number(ui, "moves"), number(ui, "releases"), number(ui, "last-moved"), number(ui, "last-released"))
}

fn on_readout(ui: &ComponentInstance) -> LogicalPosition {
    // The readout column is the last 56 px of the slider, which is 300 px wide from x 10, in a row 32 px tall from y 10
    let _ = ui;
    LogicalPosition::new(10.0 + 300.0 - 28.0, 10.0 + 16.0)
}

fn click(ui: &ComponentInstance, position: LogicalPosition) {
    ui.window().dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    ui.window().dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
}

#[test]
fn the_arrows_step_by_one_and_say_what_a_drag_says() {
    let ui = rig(&[]);
    assert_eq!(value(&ui), 50.0);
    press(&ui, Key::RightArrow);
    assert_eq!((value(&ui), said(&ui)), (51.0, (1.0, 1.0, 51.0, 51.0)));
    press(&ui, Key::UpArrow);
    assert_eq!(value(&ui), 52.0);
    press(&ui, Key::LeftArrow);
    press(&ui, Key::DownArrow);
    assert_eq!((value(&ui), said(&ui)), (50.0, (4.0, 4.0, 50.0, 50.0)));
}

#[test]
fn the_big_step_follows_the_range_and_the_small_one_the_step() {
    let wide = rig(&[("hi", Value::Number(1000.0)), ("start", Value::Number(500.0))]);
    press(&wide, Key::RightArrow);
    assert_eq!(value(&wide), 501.0);
    press(&wide, Key::PageUp);
    assert_eq!(value(&wide), 601.0);
    let fine = rig(&[("hi", Value::Number(1.0)), ("st", Value::Number(0.01)), ("start", Value::Number(0.5))]);
    press(&fine, Key::RightArrow);
    assert!((value(&fine) - 0.51).abs() < 1e-4, "{}", value(&fine));
    press(&fine, Key::PageUp);
    assert!((value(&fine) - 0.61).abs() < 1e-4, "{}", value(&fine));
}

#[test]
fn shift_with_an_arrow_and_the_page_keys_step_by_a_tenth_of_the_range() {
    let ui = rig(&[]);
    shifted(&ui, Key::RightArrow);
    assert_eq!(value(&ui), 60.0);
    press(&ui, Key::PageUp);
    assert_eq!(value(&ui), 70.0);
    press(&ui, Key::PageDown);
    press(&ui, Key::PageDown);
    assert_eq!((value(&ui), said(&ui)), (50.0, (4.0, 4.0, 50.0, 50.0)));
    shifted(&ui, Key::LeftArrow);
    assert_eq!(value(&ui), 40.0);
}

#[test]
fn the_keys_stop_at_the_ends_and_say_nothing_when_nothing_moved() {
    let ui = rig(&[("start", Value::Number(95.0))]);
    press(&ui, Key::PageUp);
    assert_eq!((value(&ui), said(&ui).0), (100.0, 1.0));
    press(&ui, Key::PageUp);
    press(&ui, Key::RightArrow);
    assert_eq!((value(&ui), said(&ui).0), (100.0, 1.0), "already at the end");
    let low = rig(&[("start", Value::Number(4.0))]);
    press(&low, Key::PageDown);
    assert_eq!(value(&low), 0.0);
}

#[test]
fn home_and_end_go_to_the_ends() {
    let ui = rig(&[("lo", Value::Number(-24.0)), ("hi", Value::Number(24.0)), ("start", Value::Number(3.0))]);
    press(&ui, Key::End);
    assert_eq!((value(&ui), said(&ui)), (24.0, (1.0, 1.0, 24.0, 24.0)));
    press(&ui, Key::Home);
    assert_eq!((value(&ui), said(&ui)), (-24.0, (2.0, 2.0, -24.0, -24.0)));
}

#[test]
fn a_disabled_slider_takes_no_keys_and_no_typing() {
    let ui = rig(&[("on", Value::Bool(false))]);
    for key in [Key::RightArrow, Key::PageUp, Key::Home, Key::End] {
        press(&ui, key);
    }
    press(&ui, "\n");
    type_in(&ui, "5\n");
    click(&ui, on_readout(&ui));
    type_in(&ui, "5\n");
    assert_eq!((value(&ui), said(&ui).0), (50.0, 0.0));
}

#[test]
fn enter_opens_a_box_for_the_value_and_enter_sets_it_like_a_drag_would() {
    let ui = rig(&[]);
    press(&ui, "\n");
    type_in(&ui, "72");
    press(&ui, "\n");
    assert_eq!((value(&ui), said(&ui)), (72.0, (1.0, 1.0, 72.0, 72.0)));
    // The focus is back on the slider: the arrows go on from there
    press(&ui, Key::RightArrow);
    assert_eq!(value(&ui), 73.0);
}

#[test]
fn escape_leaves_the_value_as_it_was_and_the_slider_keeps_its_keys() {
    let ui = rig(&[]);
    press(&ui, "\n");
    type_in(&ui, "9");
    press(&ui, Key::Escape);
    assert_eq!((value(&ui), said(&ui).0), (50.0, 0.0));
    press(&ui, Key::RightArrow);
    assert_eq!((value(&ui), said(&ui).0), (51.0, 1.0), "the arrows reach the slider, so the box is gone");
    // Enter opens it again, with the number it holds now
    press(&ui, "\n");
    type_in(&ui, "7");
    press(&ui, Key::Escape);
    assert_eq!(value(&ui), 51.0);
}

#[test]
fn a_click_on_the_value_opens_the_box_too() {
    let ui = rig(&[]);
    click(&ui, on_readout(&ui));
    type_in(&ui, "20");
    press(&ui, "\n");
    assert_eq!((value(&ui), said(&ui)), (20.0, (1.0, 1.0, 20.0, 20.0)));
    let again = rig(&[]);
    click(&again, on_readout(&again));
    type_in(&again, "20");
    press(&again, Key::Escape);
    assert_eq!((value(&again), said(&again).0), (50.0, 0.0));
}

#[test]
fn a_typed_value_is_held_to_the_range_and_the_step_and_a_non_number_is_dropped() {
    let ui = rig(&[]);
    for (typed, wanted) in [("250", 100.0), ("-5", 0.0), ("20.4", 20.0)] {
        press(&ui, "\n");
        type_in(&ui, typed);
        press(&ui, "\n");
        assert_eq!(value(&ui), wanted, "typed {typed}");
    }
    let before = said(&ui);
    press(&ui, "\n");
    type_in(&ui, "abc");
    press(&ui, "\n");
    assert_eq!((value(&ui), said(&ui)), (20.0, before), "words say nothing");
    press(&ui, "\n");
    type_in(&ui, "20");
    press(&ui, "\n");
    assert_eq!(said(&ui), before, "the same value says nothing");
}

#[test]
fn a_slider_with_its_own_box_hands_the_text_to_its_owner_instead() {
    let ui = rig(&[("box", Value::String("5".into()))]);
    press(&ui, "\n");
    type_in(&ui, "9");
    press(&ui, "\n");
    assert_eq!(ui.get_property("typed-text").unwrap(), Value::String("9".into()));
    assert_eq!((value(&ui), said(&ui).0), (50.0, 0.0), "the owner reads it and moves the slider itself");
}

// With a readout the slider's accessible value is the readout ("Chest"), so these read the value from what the slider said instead
#[test]
fn a_readout_that_is_a_word_offers_no_box_for_the_raw_number_but_keeps_its_keys() {
    let ui = rig(&[("words", Value::String("Chest".into()))]);
    press(&ui, "\n");
    type_in(&ui, "72\n");
    assert_eq!(said(&ui).0, 0.0, "Enter opened no box, so the typed digits went nowhere");
    click(&ui, on_readout(&ui));
    type_in(&ui, "72\n");
    assert_eq!(said(&ui).0, 0.0, "a click on the word opens nothing either");
    for (key, wanted) in [(Key::RightArrow, 51.0), (Key::PageUp, 61.0), (Key::End, 100.0), (Key::Home, 0.0)] {
        press(&ui, key);
        assert_eq!(said(&ui).3, wanted);
    }
}

#[test]
fn a_readout_that_leads_with_a_number_still_offers_the_box() {
    for words in ["40%", "+1.0 EV", "-3", "\u{2212}2 stops"] {
        let ui = rig(&[("words", Value::String(words.into()))]);
        press(&ui, "\n");
        type_in(&ui, "72");
        press(&ui, "\n");
        assert_eq!(said(&ui).3, 72.0, "a readout of {words}");
    }
}
