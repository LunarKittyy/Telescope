// The held bubble on the kit's slider (the Peek): it is only there while the slider is touched (held with the pointer, or keys used on
// it a moment ago) and only when the owner says there is something to show, it takes no press and no key so the drag goes on, it
// never moves anything under it, and it stands in front of the tooltip. Real events through the offscreen platform.
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use std::time::Duration;

const DOC: &str = r#"
import { Slider, Peeks, PeekLayer, PeekBox, TipLayer, Tips, WindowBox, Theme } from "@ui-kit/ui-kit.slint";
export { Peeks, PeekBox, Tips, WindowBox, Theme }
export component W inherits Window {
    width: 420px;
    height: 330px;
    background: Theme.ground;
    in property <string> kind: "picture";
    in property <bool> on: true;
    in property <[float]> numbers: [];
    in property <string> words;
    in-out property <int> moves;
    in-out property <int> releases;
    in-out property <int> opened;
    in-out property <int> closed;
    in-out property <float> at: 50;
    out property <length> column-h: column.height;
    column := VerticalLayout {
        x: 10px;
        y: 160px;
        width: 300px;
        spacing: 10px;
        tall := Slider {
            label: "Level";
            value: root.at;
            enabled: root.on;
            peek: root.kind;
            peek-values: root.numbers;
            peek-text: root.words;
            tip: "How loud";
            moved(v) => { root.at = v; root.moves += 1; }
            released(v) => { root.releases += 1; }
            peek-changed(o) => { if (o) { root.opened += 1; } else { root.closed += 1; } }
        }
        Slider {
            label: "Other";
            value: 20;
            enabled: root.on;
            peek: root.kind;
            peek-changed(o) => { if (o) { root.opened += 1; } else { root.closed += 1; } }
        }
    }
    // Near the top of the window, where a bubble has no room above
    Slider {
        x: 10px;
        y: 4px;
        width: 300px;
        label: "Top";
        value: 30;
        enabled: root.on;
        peek: root.kind;
    }
    // Something far below, which no bubble may touch
    Rectangle { x: 10px; y: 280px; width: 200px; height: 30px; background: #336699; }
    TipLayer { }
    PeekLayer { }
}
"#;

const W: f32 = 420.0;
const H: f32 = 330.0;
/// The slider's track runs from its left edge to the readout; its middle is a row's height (32px) down from the slider's top.
const TRACK_X: f32 = 100.0;
const TALL_Y: f32 = 160.0 + 16.0;
const TOP_Y: f32 = 4.0 + 16.0;
// Where the top slider's handle is, so a press takes hold of it and the handle doesn't glide to the pointer
const TOP_X: f32 = 84.0;

fn open() -> ComponentInstance {
    telescope_shell::offscreen::install();
    let w = telescope_shell::compile::compile("peek.slint", DOC, vec![], "W").expect("the document compiles").create().unwrap();
    w.set_global_property("WindowBox", "width", Value::Number(W as f64)).unwrap();
    w.set_global_property("WindowBox", "height", Value::Number(H as f64)).unwrap();
    w.show().unwrap();
    draw(&w);
    w
}

fn draw(w: &ComponentInstance) -> telescope_shell::offscreen::Picture {
    telescope_shell::offscreen::draw(w.window(), W, H, 1.0).unwrap()
}

fn wait(w: &ComponentInstance, ms: u64) {
    // In steps, since a timer that comes due several times in one jump only fires once
    for _ in 0..ms / 50 {
        telescope_shell::offscreen::skip(Duration::from_millis(50));
        draw(w);
    }
}

fn at(x: f32, y: f32) -> LogicalPosition {
    LogicalPosition::new(x, y)
}

fn press_at(w: &ComponentInstance, x: f32, y: f32) {
    w.window().dispatch_event(WindowEvent::PointerMoved { position: at(x, y) });
    w.window().dispatch_event(WindowEvent::PointerPressed { position: at(x, y), button: PointerEventButton::Left });
    draw(w);
}

fn move_to(w: &ComponentInstance, x: f32, y: f32) {
    w.window().dispatch_event(WindowEvent::PointerMoved { position: at(x, y) });
    draw(w);
}

fn release_at(w: &ComponentInstance, x: f32, y: f32) {
    w.window().dispatch_event(WindowEvent::PointerReleased { position: at(x, y), button: PointerEventButton::Left });
    draw(w);
}

fn key(w: &ComponentInstance, key: impl Into<slint::SharedString>) {
    let text: slint::SharedString = key.into();
    w.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    w.window().dispatch_event(WindowEvent::KeyReleased { text });
    draw(w);
}

fn up(w: &ComponentInstance) -> bool {
    matches!(w.get_global_property("Peeks", "shown"), Ok(Value::Bool(true)))
}

fn number(w: &ComponentInstance, name: &str) -> f64 {
    match w.get_property(name) {
        Ok(Value::Number(n)) => n,
        other => panic!("{name}: {other:?}"),
    }
}

fn peek_kind(w: &ComponentInstance) -> String {
    match w.get_global_property("Peeks", "kind") {
        Ok(Value::String(s)) => s.to_string(),
        other => panic!("{other:?}"),
    }
}

fn tip_up(w: &ComponentInstance) -> bool {
    matches!(w.get_global_property("Tips", "shown"), Ok(Value::Bool(true)))
}

fn differing(a: &telescope_shell::offscreen::Picture, b: &telescope_shell::offscreen::Picture, rows: std::ops::Range<usize>, cols: std::ops::Range<usize>) -> usize {
    let w = a.width as usize;
    rows.flat_map(|y| cols.clone().map(move |x| (x, y))).filter(|(x, y)| a.rgba[(y * w + x) * 4..(y * w + x) * 4 + 3] != b.rgba[(y * w + x) * 4..(y * w + x) * 4 + 3]).count()
}

#[test]
fn a_bubble_is_up_only_while_the_slider_is_held() {
    let w = open();
    assert!(!up(&w));
    press_at(&w, TRACK_X, TALL_Y);
    assert!(up(&w) && peek_kind(&w) == "picture");
    assert_eq!(number(&w, "opened"), 1.0);
    move_to(&w, TRACK_X + 40.0, TALL_Y);
    assert!(up(&w), "still held");
    release_at(&w, TRACK_X + 40.0, TALL_Y);
    assert!(!up(&w));
    assert_eq!((number(&w, "opened"), number(&w, "closed")), (1.0, 1.0));
}

#[test]
fn hovering_alone_brings_the_tooltip_and_never_the_bubble() {
    let w = open();
    w.window().dispatch_event(WindowEvent::PointerMoved { position: at(TRACK_X, TALL_Y) });
    wait(&w, 700);
    assert!(tip_up(&w) && !up(&w));
}

#[test]
fn nothing_is_shown_when_the_owner_says_the_preview_does() {
    let w = open();
    w.set_property("kind", Value::String("".into())).unwrap();
    press_at(&w, TRACK_X, TALL_Y);
    assert!(!up(&w));
    assert_eq!(number(&w, "opened"), 0.0);
    release_at(&w, TRACK_X, TALL_Y);
    // the rule changes mid-touch: the bubble comes up the moment there is something to show, and goes when there isn't
    press_at(&w, TRACK_X, TALL_Y);
    w.set_property("kind", Value::String("picture".into())).unwrap();
    draw(&w);
    assert!(up(&w));
    w.set_property("kind", Value::String("".into())).unwrap();
    draw(&w);
    assert!(!up(&w));
    release_at(&w, TRACK_X, TALL_Y);
}

#[test]
fn a_disabled_slider_shows_nothing() {
    let w = open();
    w.set_property("on", Value::Bool(false)).unwrap();
    press_at(&w, TRACK_X, TALL_Y);
    assert!(!up(&w));
    release_at(&w, TRACK_X, TALL_Y);
    key(&w, Key::Tab);
    key(&w, Key::RightArrow);
    assert!(!up(&w));
}

#[test]
fn keys_on_the_focused_slider_show_it_for_a_moment() {
    // Real time passes while each step draws, so on a busy machine a second of waiting is more: only the test's own skips move the clock
    let _still = telescope_shell::offscreen::freeze_clock();
    let w = open();
    // the first Tab lands on the first slider in tree order, which is the one with the tooltip
    key(&w, Key::Tab);
    assert!(!up(&w), "focus alone shows no bubble");
    key(&w, Key::RightArrow);
    assert!(up(&w) && number(&w, "at") != 50.0);
    wait(&w, 1000);
    assert!(up(&w), "a key a second ago still counts");
    key(&w, Key::RightArrow);
    wait(&w, 1000);
    assert!(up(&w), "each key starts the moment over");
    wait(&w, 700);
    assert!(!up(&w), "and a moment after the last one it goes");
    key(&w, Key::Home);
    assert!(up(&w));
    key(&w, Key::Tab);
    assert!(!up(&w), "the focus moving on takes it down at once");
}

#[test]
fn the_drag_goes_on_under_the_bubble_and_says_what_a_drag_says() {
    let w = open();
    press_at(&w, TRACK_X, TALL_Y);
    assert!(up(&w));
    let first = number(&w, "moves");
    move_to(&w, TRACK_X + 30.0, TALL_Y);
    move_to(&w, TRACK_X + 60.0, TALL_Y);
    assert!(number(&w, "moves") >= first + 2.0, "the bubble took none of the moves");
    let at_end = number(&w, "at");
    assert!(at_end > 50.0, "{at_end}");
    release_at(&w, TRACK_X + 60.0, TALL_Y);
    assert_eq!(number(&w, "releases"), 1.0);
    // and with no bubble to show the same drag says the same
    let again = open();
    again.set_property("kind", Value::String("".into())).unwrap();
    press_at(&again, TRACK_X, TALL_Y);
    move_to(&again, TRACK_X + 30.0, TALL_Y);
    move_to(&again, TRACK_X + 60.0, TALL_Y);
    release_at(&again, TRACK_X + 60.0, TALL_Y);
    assert_eq!((number(&again, "moves"), number(&again, "releases"), number(&again, "at")), (number(&w, "moves"), 1.0, at_end));
}

#[test]
fn opening_a_bubble_moves_nothing_in_the_card() {
    // the same press with no bubble to show is the baseline: only the bubble may differ. The clock is still, so no colour is mid-ease.
    let _still = telescope_shell::offscreen::freeze_clock();
    let plain = open();
    plain.set_property("kind", Value::String("".into())).unwrap();
    press_at(&plain, TRACK_X, TALL_Y);
    // A bubble takes a moment to pop in, and a handle's edge to come up under the pointer
    wait(&plain, 250);
    let held = draw(&plain);
    let height = number(&plain, "column-h");
    release_at(&plain, TRACK_X, TALL_Y);
    wait(&plain, 300);
    let released = draw(&plain);

    let w = open();
    press_at(&w, TRACK_X, TALL_Y);
    assert!(up(&w));
    assert_eq!(number(&w, "column-h"), height);
    wait(&w, 250);
    assert!(up(&w));
    let during = draw(&w);
    // everything from the slider down is exactly as it is without the bubble
    assert_eq!(differing(&held, &during, 165..330, 0..420), 0);
    // while the bubble itself is above it, where there was nothing
    assert!(differing(&held, &during, 20..160, 0..320) > 1000, "the bubble is drawn above the slider");
    release_at(&w, TRACK_X, TALL_Y);
    assert_eq!(number(&w, "column-h"), height);
    wait(&w, 300);
    assert_eq!(differing(&released, &draw(&w), 0..330, 0..420), 0, "closed, the picture is what it was");
}

#[test]
fn near_the_top_it_opens_below_and_stays_in_the_window() {
    // the held slider's colour fades in, so the clock is still and both are drawn at the same point of it
    let _still = telescope_shell::offscreen::freeze_clock();
    let plain = open();
    plain.set_property("kind", Value::String("".into())).unwrap();
    press_at(&plain, TOP_X, TOP_Y);
    wait(&plain, 250);
    let held = draw(&plain);
    let w = open();
    press_at(&w, TOP_X, TOP_Y);
    assert!(up(&w));
    wait(&w, 250);
    let shown = draw(&w);
    // there is no room above the slider: the bubble is below it, over what the first slider has under it
    assert!(differing(&held, &shown, 40..170, 0..320) > 1000);
    assert_eq!(differing(&held, &shown, 0..32, 0..420), 0, "and nothing above the slider");
    release_at(&w, TOP_X, TOP_Y);
    assert!(!up(&w));
}

#[test]
fn the_bubble_stands_in_front_of_a_tooltip_and_the_tooltip_waits_for_it() {
    let w = open();
    w.window().dispatch_event(WindowEvent::PointerMoved { position: at(TRACK_X, TALL_Y) });
    wait(&w, 700);
    assert!(tip_up(&w));
    press_at(&w, TRACK_X, TALL_Y);
    wait(&w, 700);
    assert!(up(&w) && !tip_up(&w));
    release_at(&w, TRACK_X, TALL_Y);
    wait(&w, 700);
    assert!(!up(&w) && !tip_up(&w), "a press keeps the tooltip down until the pointer leaves");
}

#[test]
fn each_kind_draws_something_of_its_own_in_the_same_box() {
    let w = open();
    w.set_property("numbers", Value::Model(slint::ModelRc::new(slint::VecModel::from(vec![Value::Number(0.2), Value::Number(0.8), Value::Number(0.6), Value::Number(0.4)])))).unwrap();
    w.set_property("words", Value::String("Hello there".into())).unwrap();
    let mut shots = Vec::new();
    for kind in ["picture", "chase", "text"] {
        w.set_property("kind", Value::String(kind.into())).unwrap();
        press_at(&w, TRACK_X, TALL_Y);
        assert!(up(&w) && peek_kind(&w) == kind, "{kind}");
        shots.push(draw(&w));
        release_at(&w, TRACK_X, TALL_Y);
    }
    assert!(differing(&shots[0], &shots[1], 0..330, 0..420) > 100);
    assert!(differing(&shots[1], &shots[2], 0..330, 0..420) > 100);
}
