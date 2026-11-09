// Shift while dragging goes finer on every control that is dragged (one factor, `Fine.factor`, the colour wheel's tenth), and the dial's ticks are real values: a plain drag steps through them, Shift goes between them, a click on the value types one.
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};

const FACTOR: f64 = 0.1;

fn rig(doc: &str, size: (f32, f32)) -> ComponentInstance {
    thread_local!(static INIT: std::sync::Once = const { std::sync::Once::new() });
    INIT.with(|once| once.call_once(i_slint_backend_testing::init_no_event_loop));
    let ui = telescope_shell::compile::compile("kit_fine.slint", doc, vec![], "Test").expect("the document compiles").create().unwrap();
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

fn text(ui: &ComponentInstance, name: &str) -> String {
    match ui.get_property(name).unwrap() {
        Value::String(s) => s.to_string(),
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

fn shift(ui: &ComponentInstance, held: bool) {
    let text: slint::SharedString = Key::Shift.into();
    ui.window().dispatch_event(if held { WindowEvent::KeyPressed { text } } else { WindowEvent::KeyReleased { text } });
}

fn press(ui: &ComponentInstance, key: impl Into<slint::SharedString>) {
    let text: slint::SharedString = key.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

/// What `read` says after a press at `from` and a drag to `to`, with Shift held for the drag or not, as a change from just after the press.
fn travelled(doc: &str, size: (f32, f32), from: (f32, f32), to: (f32, f32), read: &str, held: bool) -> f64 {
    let ui = rig(doc, size);
    down(&ui, at(from.0, from.1));
    let before = number(&ui, read);
    shift(&ui, held);
    drag(&ui, at(to.0, to.1));
    let after = number(&ui, read);
    shift(&ui, false);
    up(&ui, at(to.0, to.1));
    after - before
}

/// A Shift drag moves what a plain one does, a tenth as far (to within a hair).
fn assert_fine(doc: &str, size: (f32, f32), from: (f32, f32), to: (f32, f32), read: &str) {
    let plain = travelled(doc, size, from, to, read, false);
    let fine = travelled(doc, size, from, to, read, true);
    assert!(plain.abs() > 0.0001, "{read}: a plain drag moves it ({plain})");
    let ratio = fine / plain;
    assert!((ratio - FACTOR).abs() < 0.01, "{read}: Shift moves {fine} against {plain}, a share of {ratio}, not {FACTOR}");
}

const SLIDER: &str = r#"
import { Slider } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 340px;
    height: 60px;
    out property <float> now: s.shown;
    s := Slider { x: 10px; y: 10px; width: 320px; label: "Level"; minimum: 0; maximum: 1000; step: 0.001; value: 500; }
}
"#;

#[test]
fn a_slider_goes_a_tenth_as_far_with_shift() {
    assert_fine(SLIDER, (340.0, 60.0), (100.0, 25.0), (150.0, 25.0), "now");
}

#[test]
fn a_slider_does_not_jump_when_shift_is_pressed_or_let_go() {
    let ui = rig(SLIDER, (340.0, 60.0));
    down(&ui, at(100.0, 25.0));
    drag(&ui, at(130.0, 25.0));
    let a = number(&ui, "now");
    shift(&ui, true);
    drag(&ui, at(130.0, 25.0));
    assert!((number(&ui, "now") - a).abs() < 0.0001, "pressing Shift on its own moves nothing");
    drag(&ui, at(190.0, 25.0));
    let b = number(&ui, "now");
    shift(&ui, false);
    drag(&ui, at(190.0, 25.0));
    assert!((number(&ui, "now") - b).abs() < 0.0001, "and neither does letting it go: the value stays where Shift left it");
    drag(&ui, at(200.0, 25.0));
    let c = number(&ui, "now");
    assert!(c > b, "a plain move goes on from there at the plain pace ({b} to {c})");
    up(&ui, at(200.0, 25.0));
}

const PAD: &str = r#"
import { Pad } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 340px;
    height: 230px;
    out property <float> now-x: p.shown-x;
    out property <float> now-y: p.shown-y;
    p := Pad { x: 10px; y: 10px; width: 300px; height: 200px; label: "Pad"; x-minimum: 0; x-maximum: 1000; step-x: 0.01; y-minimum: 0; y-maximum: 1000; step-y: 0.01; value-x: 500; value-y: 500; }
}
"#;

#[test]
fn a_pad_goes_a_tenth_as_far_with_shift_across_and_up() {
    assert_fine(PAD, (340.0, 230.0), (160.0, 110.0), (200.0, 110.0), "now-x");
    assert_fine(PAD, (340.0, 230.0), (160.0, 110.0), (160.0, 150.0), "now-y");
}

const JOG: &str = r#"
import { Jog } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 240px;
    height: 60px;
    out property <float> now: j.offset;
    j := Jog { x: 10px; y: 10px; width: 220px; label: "Jog"; }
}
"#;

#[test]
fn a_jog_goes_a_tenth_as_far_with_shift() {
    assert_fine(JOG, (240.0, 60.0), (120.0, 25.0), (150.0, 25.0), "now");
}

const BAR: &str = r#"
import { ScrollBar } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 60px;
    height: 220px;
    in-out property <float> now;
    ScrollBar { x: 10px; y: 10px; width: 11px; height: 200px; content: 1000px; viewport: 200px; offset: 0px; scrolled(to) => { now = to / 1px; } }
}
"#;

#[test]
fn a_scroll_bar_goes_a_tenth_as_far_with_shift() {
    assert_fine(BAR, (60.0, 220.0), (15.0, 20.0), (15.0, 60.0), "now");
}

const WHEEL: &str = r#"
import { ColourWheel } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 200px;
    height: 160px;
    out property <int> reports;
    out property <int> fine-reports;
    w := ColourWheel { x: 10px; y: 10px; width: 180px; height: 140px; label: "Lift"; dragged(dx, dy, shift) => { reports += 1; if (shift) { fine-reports += 1; } } }
}
"#;

#[test]
fn a_colour_wheel_says_whether_shift_is_held_and_its_owner_goes_a_tenth_as_far() {
    let ui = rig(WHEEL, (200.0, 160.0));
    down(&ui, at(100.0, 80.0));
    drag(&ui, at(110.0, 80.0));
    shift(&ui, true);
    drag(&ui, at(120.0, 80.0));
    shift(&ui, false);
    up(&ui, at(120.0, 80.0));
    assert_eq!((number(&ui, "reports"), number(&ui, "fine-reports")), (2.0, 1.0));
    // The wheel's owner (effects/src/editors.rs) takes its pace from the same tenth
    let editors = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../effects/src/editors.rs")).unwrap();
    assert!(editors.contains("const WHEEL_FINE: f64 = 0.1;"), "the wheel's factor is the kit's Fine.factor");
}

const DIAL: &str = r#"
import { Dial } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 220px;
    height: 70px;
    in property <bool> free: true;
    in property <int> start: 3;
    in property <float> start-between: 0;
    in property <string> text;
    in property <int> bad;
    out property <int> moves;
    out property <int> betweens;
    out property <int> last-moved;
    out property <int> last-between-i;
    out property <float> last-between-off;
    out property <string> last-typed;
    out property <int> now: d.shown;
    out property <float> place: d.shown-place;
    d := Dial {
        x: 10px;
        y: 10px;
        width: 200px;
        label: "ISO";
        stops: ["25", "32", "40", "50", "64", "80", "100", "125", "160", "200", "250"];
        per-label: 3;
        label-from: 0;
        free: free;
        current: start;
        between: start-between;
        value-text: text;
        rejected: bad;
        moved(i) => { moves += 1; last-moved = i; }
        moved-between(i, off) => { betweens += 1; last-between-i = i; last-between-off = off; }
        typed(t) => { last-typed = t; }
    }
}
"#;

// 200 wide: the labelled ticks are 68 apart, so each tick is a third of that
const TICK: f32 = 68.0 / 3.0;

#[test]
fn a_dial_has_a_value_at_every_tick_and_a_plain_drag_steps_through_them() {
    let ui = rig(DIAL, (220.0, 70.0));
    down(&ui, at(110.0, 34.0));
    drag(&ui, at(110.0 - TICK, 34.0));
    assert_eq!((number(&ui, "now"), number(&ui, "last-moved")), (4.0, 4.0), "one tick is a value of its own");
    drag(&ui, at(110.0 - 3.0 * TICK, 34.0));
    assert_eq!(number(&ui, "now"), 6.0, "three ticks, a full stop");
    assert_eq!(number(&ui, "betweens"), 0.0, "a plain drag never lands between ticks");
    up(&ui, at(110.0 - 3.0 * TICK, 34.0));
    // A notch of the wheel is a tick too, and so is an arrow
    ui.window().dispatch_event(WindowEvent::PointerScrolled { position: at(110.0, 34.0), delta_x: 0.0, delta_y: 60.0 });
    assert_eq!(number(&ui, "now"), 7.0);
    press(&ui, Key::Tab);
    press(&ui, Key::LeftArrow);
    assert_eq!(number(&ui, "now"), 6.0);
}

#[test]
fn a_dial_goes_between_the_ticks_with_shift_a_tenth_as_far() {
    let ui = rig(DIAL, (220.0, 70.0));
    down(&ui, at(110.0, 34.0));
    shift(&ui, true);
    drag(&ui, at(110.0 - 3.0 * TICK, 34.0));
    assert_eq!(number(&ui, "betweens"), 1.0, "between two ticks it says so");
    assert_eq!(number(&ui, "last-between-i"), 3.0);
    assert!((number(&ui, "last-between-off") - 0.3).abs() < 0.01, "three ticks of the pointer are three tenths of a tick: {}", number(&ui, "last-between-off"));
    assert!((number(&ui, "place") - 3.3).abs() < 0.01);
    // Letting Shift go on its own moves nothing; a plain move after it goes by whole ticks again, from where Shift left it
    shift(&ui, false);
    assert!((number(&ui, "place") - 3.3).abs() < 0.01);
    drag(&ui, at(110.0 - 4.0 * TICK, 34.0));
    assert_eq!(number(&ui, "now"), 4.0, "a tick further is the next whole tick");
    assert_eq!(number(&ui, "betweens"), 1.0, "and says nothing between ticks any more");
    up(&ui, at(110.0 - 4.0 * TICK, 34.0));
}

#[test]
fn a_dial_that_is_not_free_goes_between_nothing() {
    let ui = rig(DIAL, (220.0, 70.0));
    ui.set_property("free", Value::Bool(false)).unwrap();
    down(&ui, at(110.0, 34.0));
    shift(&ui, true);
    drag(&ui, at(110.0 - 3.0 * TICK, 34.0));
    shift(&ui, false);
    assert_eq!(number(&ui, "betweens"), 0.0);
    up(&ui, at(110.0, 34.0));
}

#[test]
fn shift_and_the_wheel_go_between_the_ticks_on_a_free_dial() {
    let ui = rig(DIAL, (220.0, 70.0));
    shift(&ui, true);
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: at(110.0, 34.0) });
    ui.window().dispatch_event(WindowEvent::PointerScrolled { position: at(110.0, 34.0), delta_x: 0.0, delta_y: 60.0 });
    shift(&ui, false);
    assert!((number(&ui, "place") - 3.1).abs() < 0.001, "a notch with Shift is a tenth of a tick: {}", number(&ui, "place"));
    assert_eq!(number(&ui, "betweens"), 1.0);
}

#[test]
fn a_click_on_the_dials_value_types_one_and_enter_hands_it_over() {
    let ui = rig(DIAL, (220.0, 70.0));
    down(&ui, at(110.0, 34.0));
    up(&ui, at(110.0, 34.0));
    // The box is up: type over the selected value
    for c in "40".chars() {
        press(&ui, c.to_string());
    }
    press(&ui, "\n");
    // The dial hands over the text, as it was written; the owner puts the value in
    assert_eq!(text(&ui, "last-typed"), "40");
}

#[test]
fn escape_leaves_the_typed_value_and_a_click_beside_the_mark_still_picks_that_tick() {
    let ui = rig(DIAL, (220.0, 70.0));
    down(&ui, at(110.0, 34.0));
    up(&ui, at(110.0, 34.0));
    press(&ui, "9");
    press(&ui, Key::Escape);
    assert_eq!(text(&ui, "last-typed"), "", "Escape says nothing");
    down(&ui, at(110.0 + TICK, 34.0));
    up(&ui, at(110.0 + TICK, 34.0));
    assert_eq!(number(&ui, "now"), 4.0);
}

#[test]
fn a_dial_that_is_not_free_does_not_offer_to_type() {
    let ui = rig(DIAL, (220.0, 70.0));
    ui.set_property("free", Value::Bool(false)).unwrap();
    down(&ui, at(110.0, 34.0));
    up(&ui, at(110.0, 34.0));
    press(&ui, "4");
    press(&ui, "\n");
    assert_eq!(text(&ui, "last-typed"), "");
}

#[test]
fn a_dial_between_two_ticks_reads_as_the_owner_says_and_follows_its_between() {
    let ui = rig(DIAL, (220.0, 70.0));
    ui.set_property("start-between", Value::Number(0.3)).unwrap();
    ui.set_property("text", Value::String("37".into())).unwrap();
    assert!((number(&ui, "place") - 3.3).abs() < 0.001);
    let dial = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "ISO").find(|e| e.accessible_value().is_some()).expect("the dial");
    assert_eq!(dial.accessible_value().as_deref(), Some("37"));
}

const LONG: &str = r#"
import { Dial } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 220px;
    height: 70px;
    out property <int> now: d.shown;
    out property <float> pace: d.pace;
    out property <float> calm: d.name-calm;
    out property <bool> idle: d.idle;
    d := Dial {
        x: 10px;
        y: 10px;
        width: 200px;
        label: "ISO";
        stops: ["25", "32", "40", "50", "64", "80", "100", "125", "160", "200", "250", "320", "400", "500", "640", "800", "1000", "1250", "1600", "2000", "2500", "3200", "4000", "5000", "6400", "8000", "10000", "12800"];
        per-label: 3;
        label-from: 0;
        free: true;
        current: 3;
    }
}
"#;

/// Time passing a frame or so at a time, as it does: a timer fires once for a jump of any length.
fn wait(ms: u64) {
    let mut left = ms;
    while left > 0 {
        let by = left.min(20);
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(by));
        left -= by;
    }
}

fn notch(ui: &ComponentInstance) {
    ui.window().dispatch_event(WindowEvent::PointerScrolled { position: at(110.0, 34.0), delta_x: 0.0, delta_y: 60.0 });
}

#[test]
fn a_dial_going_fast_loses_its_big_names_and_gets_them_back_when_it_slows() {
    let ui = rig(LONG, (220.0, 70.0));
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: at(110.0, 34.0) });
    assert_eq!((number(&ui, "calm"), number(&ui, "pace")), (1.0, 0.0), "at rest the names are there");
    // Rapid wheel notches, one every 30 ms
    for _ in 0..16 {
        notch(&ui);
        wait(30);
    }
    assert!(number(&ui, "pace") > 14.0, "fast: {}", number(&ui, "pace"));
    wait(150);
    assert!(number(&ui, "calm") < 0.5, "the names have gone: {}", number(&ui, "calm"));
    // Stopped: they come back, and nothing keeps running
    wait(1500);
    assert!(number(&ui, "calm") > 0.99, "{}", number(&ui, "calm"));
    assert!(truth(&ui, "idle"), "a dial at rest has no timer running");
}

#[test]
fn a_careful_dial_keeps_its_names() {
    let ui = rig(LONG, (220.0, 70.0));
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: at(110.0, 34.0) });
    // A tick every quarter of a second
    let mut lowest = 1.0_f64;
    for _ in 0..8 {
        notch(&ui);
        for _ in 0..5 {
            wait(50);
            lowest = lowest.min(number(&ui, "calm"));
        }
    }
    assert!(lowest > 0.99, "a careful turn never loses the names: {lowest}");
    assert!(number(&ui, "pace") < 9.0, "{}", number(&ui, "pace"));
}

#[test]
fn a_dial_at_rest_runs_nothing() {
    let ui = rig(LONG, (220.0, 70.0));
    wait(2000);
    assert!(truth(&ui, "idle"));
}

fn truth(ui: &ComponentInstance, name: &str) -> bool {
    match ui.get_property(name).unwrap() {
        Value::Bool(b) => b,
        other => panic!("{name} is {other:?}"),
    }
}
