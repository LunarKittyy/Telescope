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

/// What `read` says after a press at `from` and a Shift drag far past `to` (the pointer leaves the control, the drag is still held).
fn shift_dragged_far(doc: &str, size: (f32, f32), from: (f32, f32), to: (f32, f32), read: &str) -> f64 {
    let ui = rig(doc, size);
    down(&ui, at(from.0, from.1));
    shift(&ui, true);
    // Many small moves, as a hand makes them, out to a pointer far outside the window
    let steps = 200;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        drag(&ui, at(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t));
    }
    let after = number(&ui, read);
    shift(&ui, false);
    up(&ui, at(to.0, to.1));
    after
}

#[test]
fn a_long_shift_drag_on_a_slider_reaches_both_ends_of_its_range() {
    assert_eq!(shift_dragged_far(SLIDER, (340.0, 60.0), (170.0, 25.0), (5000.0, 25.0), "now"), 1000.0);
    assert_eq!(shift_dragged_far(SLIDER, (340.0, 60.0), (170.0, 25.0), (-5000.0, 25.0), "now"), 0.0);
}

const STEPPED: &str = r#"
import { Slider } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    width: 240px;
    height: 60px;
    out property <float> now: s.shown;
    s := Slider { x: 10px; y: 10px; width: 220px; label: "Brightness"; minimum: -8; maximum: 8; step: 1; value: 0; }
}
"#;

#[test]
fn a_stepped_slider_moves_one_step_at_a_time_by_drag_with_or_without_shift_and_by_keys() {
    for held in [false, true] {
        let ui = rig(STEPPED, (240.0, 60.0));
        down(&ui, at(120.0, 25.0));
        shift(&ui, held);
        let mut last = number(&ui, "now");
        for i in 1..=400 {
            drag(&ui, at(120.0 + i as f32 * 0.5, 25.0));
            let now = number(&ui, "now");
            assert!((now - last).abs() <= 1.0, "Shift {held}: {last} to {now} in a half pixel step");
            last = now;
        }
        for i in 1..=400 {
            drag(&ui, at(320.0 - i as f32 * 1.5, 25.0));
            let now = number(&ui, "now");
            assert!((now - last).abs() <= 1.0, "Shift {held}: {last} to {now} going back");
            last = now;
        }
        shift(&ui, false);
        up(&ui, at(20.0, 25.0));
    }
    let ui = rig(STEPPED, (240.0, 60.0));
    press(&ui, Key::Tab);
    press(&ui, Key::RightArrow);
    assert_eq!(number(&ui, "now"), 1.0);
    press(&ui, Key::LeftArrow);
    press(&ui, Key::LeftArrow);
    assert_eq!(number(&ui, "now"), -1.0);
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

#[test]
fn a_long_shift_drag_on_a_pad_reaches_the_ends_of_both_ranges() {
    assert_eq!(shift_dragged_far(PAD, (340.0, 230.0), (160.0, 110.0), (6000.0, 110.0), "now-x"), 1000.0);
    assert_eq!(shift_dragged_far(PAD, (340.0, 230.0), (160.0, 110.0), (-6000.0, 110.0), "now-x"), 0.0);
    assert_eq!(shift_dragged_far(PAD, (340.0, 230.0), (160.0, 110.0), (160.0, 4000.0), "now-y"), 1000.0);
    assert_eq!(shift_dragged_far(PAD, (340.0, 230.0), (160.0, 110.0), (160.0, -4000.0), "now-y"), 0.0);
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

#[test]
fn a_long_shift_drag_on_a_jog_reaches_its_end() {
    assert_eq!(shift_dragged_far(JOG, (240.0, 60.0), (120.0, 25.0), (3000.0, 25.0), "now"), 1.0);
    assert_eq!(shift_dragged_far(JOG, (240.0, 60.0), (120.0, 25.0), (-3000.0, 25.0), "now"), -1.0);
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

#[test]
fn a_long_shift_drag_on_a_scroll_bar_reaches_its_end() {
    assert_eq!(shift_dragged_far(BAR, (60.0, 220.0), (15.0, 20.0), (15.0, 9000.0), "now"), 800.0);
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
fn a_long_shift_drag_on_a_dial_reaches_its_last_tick_and_its_first() {
    assert_eq!(shift_dragged_far(DIAL, (220.0, 70.0), (110.0, 34.0), (-9000.0, 34.0), "place"), 10.0);
    assert_eq!(shift_dragged_far(DIAL, (220.0, 70.0), (110.0, 34.0), (9000.0, 34.0), "place"), 0.0);
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
    in property <float> start-between: 0;
    in property <string> text;
    out property <int> now: d.shown;
    out property <string> read: d.read;
    out property <float> scale: d.grown;
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
        between: start-between;
        value-text: text;
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
fn the_readout_is_always_the_value_and_swells_when_the_dial_is_let_go() {
    let ui = rig(LONG, (220.0, 70.0));
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: at(110.0, 34.0) });
    assert_eq!((text(&ui, "read").as_str(), number(&ui, "scale")), ("50", 1.0), "at rest it is big");
    // Rapid wheel notches: the readout says the value after every one and is smaller while they come
    let names = ["25", "32", "40", "50", "64", "80", "100", "125", "160", "200", "250", "320", "400", "500", "640", "800", "1000", "1250", "1600"];
    for _ in 0..12 {
        notch(&ui);
        wait(30);
        assert_eq!(text(&ui, "read"), names[number(&ui, "now") as usize], "never blank, never behind");
    }
    assert!(number(&ui, "scale") < 0.9, "smaller while it moves: {}", number(&ui, "scale"));
    // Let go: about 200 ms after the last step it swells, over a moment
    wait(150);
    assert!(number(&ui, "scale") < 0.9, "not yet: {}", number(&ui, "scale"));
    wait(150);
    let mid = number(&ui, "scale");
    assert!(mid > 0.78 && mid < 1.0, "on its way: {mid}");
    wait(600);
    assert_eq!(number(&ui, "scale"), 1.0, "and at rest it is big and plain");
}

#[test]
fn a_held_drag_keeps_the_readout_small_and_a_release_swells_it() {
    let ui = rig(LONG, (220.0, 70.0));
    down(&ui, at(110.0, 34.0));
    wait(400);
    assert!(number(&ui, "scale") < 0.8, "small while held: {}", number(&ui, "scale"));
    drag(&ui, at(110.0 - 40.0, 34.0));
    wait(100);
    up(&ui, at(110.0 - 40.0, 34.0));
    wait(700);
    assert_eq!(number(&ui, "scale"), 1.0);
}

#[test]
fn the_readout_says_the_owners_words_between_two_ticks() {
    let ui = rig(LONG, (220.0, 70.0));
    ui.set_property("start-between", Value::Number(0.3)).unwrap();
    ui.set_property("text", Value::String("52".into())).unwrap();
    assert_eq!(text(&ui, "read"), "52");
}

#[test]
fn grabbing_the_dial_again_while_the_readout_swells_keeps_it_following() {
    // Press again at every moment of the swell, and after a wheel notch too: the readout says each new value and settles small, then big
    for gap in (0..400).step_by(40) {
        for by_wheel in [false, true] {
            let ui = rig(LONG, (220.0, 70.0));
            ui.window().dispatch_event(WindowEvent::PointerMoved { position: at(110.0, 34.0) });
            if by_wheel {
                notch(&ui);
            } else {
                down(&ui, at(110.0, 34.0));
                drag(&ui, at(110.0 - 30.0, 34.0));
                wait(40);
                up(&ui, at(110.0 - 30.0, 34.0));
            }
            wait(gap);
            let from = if by_wheel { 110.0 } else { 80.0 };
            down(&ui, at(from, 34.0));
            let mut seen = Vec::new();
            for k in 1..=8 {
                drag(&ui, at(from - k as f32 * 23.0, 34.0));
                wait(20);
                seen.push(text(&ui, "read"));
            }
            assert!(seen.windows(2).all(|w| w[0] != w[1]), "gap {gap} wheel {by_wheel}: every tick says its value: {seen:?}");
            wait(400);
            assert!(number(&ui, "scale") < 0.8, "small while held: {}", number(&ui, "scale"));
            up(&ui, at(from - 8.0 * 23.0, 34.0));
            wait(700);
            assert_eq!(number(&ui, "scale"), 1.0, "and big at rest");
        }
    }
}
