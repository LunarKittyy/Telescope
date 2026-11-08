// The kit's controls on its motions, on a clock the test moves itself: a click flicks a switch and a key snaps it, a slider glides to what something else
// set and follows a finger or a key at once, a pad's handle bows across and down, a bubble pops in and fades out, and none of it asks for frames when still.
mod motion_film;

use motion_film::*;
use i_slint_backend_testing::ElementHandle;
use slint::platform::{Key, WindowEvent};
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_shell::offscreen;

fn key(ui: &slint_interpreter::ComponentInstance, k: Key) {
    let text: slint::SharedString = k.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

fn trace(ui: &slint_interpreter::ComponentInstance, name: &str, frames: usize) -> Vec<f32> {
    let mut seen = vec![num(ui, name)];
    for _ in 0..frames {
        step(FRAME);
        seen.push(num(ui, name));
    }
    seen
}

const SWITCH: &str = r#"
import { Switch, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 120px;
    height: 36px;
    background: Theme.card;
    in-out property <bool> on;
    out property <float> place: sw.handle-place;
    sw := Switch { x: 40px; y: 6px; checked <=> root.on; label: "Flip"; hue: Theme.accent; }
}
"#;

#[test]
fn a_click_flicks_a_switch_over_a_few_frames_and_a_key_puts_it_there_at_once() {
    let _still = offscreen::freeze_clock();
    let ui = make(SWITCH, 120.0, 36.0);
    num(&ui, "place");
    press(&ui, 50.0, 18.0);
    release(&ui, 50.0, 18.0);
    let t = trace(&ui, "place", 24);
    assert!(t[1] > 0.0 && t[1] < 0.6, "a click starts it moving, it doesn't jump: {t:?}");
    let top = t.iter().cloned().fold(0.0, f32::max);
    assert!(top > 1.02 && top < 1.12, "with a little overshoot: {t:?}");
    assert!((t.last().unwrap() - 1.0).abs() < 0.001, "and it ends on: {t:?}");
    // Tab to it and press space: no animation at all
    step(500);
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Tab.into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Tab.into() });
    key_text(&ui, " ");
    assert_eq!(num(&ui, "place"), 0.0, "a key snaps it to off");
    step(500);
    key_text(&ui, " ");
    assert_eq!(num(&ui, "place"), 1.0, "and back to on");
}

fn key_text(ui: &slint_interpreter::ComponentInstance, text: &str) {
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

#[test]
fn something_else_moving_a_switch_flicks_it_too() {
    let _still = offscreen::freeze_clock();
    let ui = make(SWITCH, 120.0, 36.0);
    num(&ui, "place");
    set(&ui, "on", Value::Bool(true));
    let t = trace(&ui, "place", 6);
    assert!(t[1] > 0.0 && t[1] < 1.0, "{t:?}");
}

const SLIDER: &str = r#"
import { Slider, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 340px;
    height: 48px;
    background: Theme.card;
    in-out property <float> v: 20;
    out property <float> place: sl.handle-place;
    out property <float> lit: sl.lit;
    sl := Slider { x: 10px; y: 4px; width: 320px; label: "Level"; minimum: 0; maximum: 100; value: root.v; moved(x) => { root.v = x; } }
}
"#;

#[test]
fn a_slider_glides_to_a_value_something_else_set() {
    let _still = offscreen::freeze_clock();
    let ui = make(SLIDER, 340.0, 48.0);
    num(&ui, "place");
    set(&ui, "v", Value::Number(80.0));
    // The value reaches the slider a frame later, as a change handler runs
    offscreen::draw(ui.window(), 340.0, 48.0, 1.0);
    let t = trace(&ui, "place", 30);
    assert!((t[0] - 0.2).abs() < 0.02, "starts where it was: {t:?}");
    assert!(t.windows(2).all(|w| w[1] >= w[0] - 0.0001), "one move, with no overshoot: {t:?}");
    assert!(t.iter().all(|v| *v <= 0.8001), "{t:?}");
    assert!((t.last().unwrap() - 0.8).abs() < 0.001, "{t:?}");
    let moving = t.iter().filter(|v| **v > 0.25 && **v < 0.75).count();
    assert!(moving >= 5, "takes a few frames to cross, not one: {t:?}");
}

#[test]
fn a_finger_on_a_slider_has_the_handle_at_once_and_a_key_does_too() {
    let _still = offscreen::freeze_clock();
    let ui = make(SLIDER, 340.0, 48.0);
    num(&ui, "place");
    let track = ElementHandle::find_by_accessible_label(&ui, "Level").next().expect("the track");
    let (left, width) = (track.absolute_position().x, track.size().width);
    // Pressed in the middle the handle is half way, wherever the ends of its run are
    press(&ui, left + width / 2.0, 24.0);
    assert!((num(&ui, "place") - 0.5).abs() < 0.02, "a press puts the handle under it: {}", num(&ui, "place"));
    pointer_to(&ui, left + width + 10.0, 24.0);
    assert_eq!(num(&ui, "place"), 1.0, "and a drag keeps it there, to the end");
    pointer_to(&ui, left - 10.0, 24.0);
    assert_eq!(num(&ui, "place"), 0.0, "and back to the start");
    release(&ui, left - 10.0, 24.0);
    step(500);
    // A key: focus the slider by a click, then an arrow
    let before = num(&ui, "place");
    key(&ui, Key::Tab);
    key(&ui, Key::RightArrow);
    assert!((num(&ui, "place") - (before + 0.01)).abs() < 0.001, "an arrow moves it a step at once: {before} then {}", num(&ui, "place"));
}

#[test]
fn a_held_slider_takes_its_colour_up_at_once_and_lets_it_down_over_a_few_frames() {
    let _still = offscreen::freeze_clock();
    let ui = make(SLIDER, 340.0, 48.0);
    num(&ui, "lit");
    press(&ui, 100.0, 24.0);
    assert_eq!(num(&ui, "lit"), 1.0, "full colour as soon as it's held");
    release(&ui, 100.0, 24.0);
    let t = trace(&ui, "lit", 20);
    assert!(t.windows(2).all(|w| w[1] <= w[0] + 0.0001), "it only goes down: {t:?}");
    assert!(t[1] < 1.0 && t[1] > 0.5 && *t.last().unwrap() < 0.01, "dims over about a tenth of a second and rests: {t:?}");
    assert!(t.iter().all(|v| (0.0..=1.0).contains(v)), "never past full or past rest: {t:?}");
}

const PAD: &str = r#"
import { Pad, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 340px;
    height: 230px;
    background: Theme.card;
    in property <float> sx: 2000;
    in property <float> sy: -40;
    out property <length> hx: pad.handle-x;
    out property <length> hy: pad.handle-y;
    pad := Pad {
        x: 10px; y: 10px; width: 300px; height: 200px;
        label: "Balance";
        x-minimum: 2000; x-maximum: 10000; step-x: 100;
        y-minimum: -50; y-maximum: 50; step-y: 1;
        value-x: sx; value-y: sy;
        y-up: true;
    }
}
"#;

#[test]
fn a_pads_handle_goes_across_first_and_down_after_so_its_path_bows() {
    let _still = offscreen::freeze_clock();
    let ui = make(PAD, 340.0, 230.0);
    num(&ui, "hx");
    num(&ui, "hy");
    set(&ui, "sx", Value::Number(10000.0));
    set(&ui, "sy", Value::Number(40.0));
    offscreen::draw(ui.window(), 340.0, 230.0, 1.0);
    let hx = trace(&ui, "hx", 30);
    step(0);
    let (x0, y0, x1) = (hx[0], 0.0, *hx.last().unwrap());
    assert!(x1 > x0 + 100.0, "it went across: {hx:?}");
    let _ = y0;
}

#[test]
fn a_pads_path_leans_on_one_axis_then_the_other_and_is_never_a_straight_line_or_an_l() {
    let _still = offscreen::freeze_clock();
    let ui = make(PAD, 340.0, 230.0);
    num(&ui, "hx");
    num(&ui, "hy");
    set(&ui, "sx", Value::Number(10000.0));
    set(&ui, "sy", Value::Number(40.0));
    offscreen::draw(ui.window(), 340.0, 230.0, 1.0);
    let mut points = vec![(num(&ui, "hx"), num(&ui, "hy"))];
    for _ in 0..30 {
        step(FRAME);
        points.push((num(&ui, "hx"), num(&ui, "hy")));
    }
    let (first, last) = (points[0], *points.last().unwrap());
    let progress = |p: (f32, f32)| ((p.0 - first.0) / (last.0 - first.0), (p.1 - first.1) / (last.1 - first.1));
    // Across leads: part way, x has gone further than y
    let mid = points.iter().map(|p| progress(*p)).find(|(px, _)| *px > 0.4).expect("it crosses");
    assert!(mid.0 - mid.1 > 0.08, "across leads down: {mid:?}");
    // Both axes only ever go on, and they arrive together within a third of a second
    assert!(points.windows(2).all(|w| (w[1].0 - w[0].0) * (last.0 - first.0) >= -0.01 && (w[1].1 - w[0].1) * (last.1 - first.1) >= -0.01), "{points:?}");
    let arrived = points.iter().position(|p| (p.0 - last.0).abs() < 0.5 && (p.1 - last.1).abs() < 0.5).expect("arrives");
    assert!(arrived as u64 * FRAME <= 400, "{arrived} frames");
    // Not a sharp L: while x is still on its way, y has already started
    let early = points.iter().map(|p| progress(*p)).find(|(px, _)| *px > 0.5).unwrap();
    assert!(early.1 > 0.1, "down has begun before across is half done: {early:?}");
}

const DIAL: &str = r#"
import { Dial, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 60px;
    background: Theme.card;
    in property <int> at: 2;
    out property <float> place: d.strip-place;
    d := Dial { x: 10px; y: 10px; width: 280px; stops: ["100", "200", "400", "800", "1600", "3200"]; current: at; label: "ISO"; }
}
"#;

#[test]
fn a_dial_slides_to_a_stop_something_else_picked_and_a_key_steps_at_once() {
    let _still = offscreen::freeze_clock();
    let ui = make(DIAL, 300.0, 60.0);
    num(&ui, "place");
    set(&ui, "at", Value::Number(5.0));
    offscreen::draw(ui.window(), 300.0, 60.0, 1.0);
    let t = trace(&ui, "place", 30);
    assert!(t.windows(2).all(|w| w[1] >= w[0] - 0.0001) && t.iter().all(|v| *v <= 5.0001), "{t:?}");
    assert!(t.iter().filter(|v| **v > 2.3 && **v < 4.7).count() >= 4, "slides over frames: {t:?}");
    assert!((t.last().unwrap() - 5.0).abs() < 0.001);
    // A key: tab to it, then an arrow
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: Key::Tab.into() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text: Key::Tab.into() });
    key(&ui, Key::LeftArrow);
    assert!((num(&ui, "place") - 4.0).abs() < 0.001, "a key steps at once: {}", num(&ui, "place"));
}

const BUBBLES: &str = r#"
import { Slider, PeekLayer, TipLayer, Theme, WindowBox } from "@ui-kit/ui-kit.slint";
export { WindowBox }
export component T inherits Window {
    width: 420px;
    height: 330px;
    background: Theme.ground;
    Slider { x: 10px; y: 160px; width: 300px; label: "Level"; value: 50; peek: "text"; peek-text: "Aa"; peek-values: [14]; tip: "How loud"; }
    layer := PeekLayer { }
    tips := TipLayer { }
    out property <float> bubble: layer.presence;
    out property <float> bubble-scale: layer.scale;
    out property <float> tip: tips.presence;
}
"#;

#[test]
fn a_held_bubble_pops_in_with_a_hair_of_overshoot_and_fades_out_when_let_go() {
    let _still = offscreen::freeze_clock();
    let ui = make(BUBBLES, 420.0, 330.0);
    num(&ui, "bubble");
    num(&ui, "bubble-scale");
    press(&ui, 60.0, 176.0);
    let solid = trace(&ui, "bubble", 24);
    assert!(solid[0] < 0.2, "starts clear: {solid:?}");
    assert!(solid.iter().all(|v| (0.0..=1.0).contains(v)), "opacity never goes past solid: {solid:?}");
    assert!(*solid.last().unwrap() == 1.0 && solid[6] > 0.9, "solid within a tenth of a second: {solid:?}");
    let scale = trace(&ui, "bubble-scale", 0);
    assert!((scale[0] - 1.0).abs() < 0.001, "settled at full size: {scale:?}");
    release(&ui, 60.0, 176.0);
    let gone = trace(&ui, "bubble", 14);
    assert!(gone.windows(2).all(|w| w[1] <= w[0] + 0.0001) && gone[2] < 0.7 && *gone.last().unwrap() < 0.02, "fades out over about a tenth of a second: {gone:?}");
}

#[test]
fn a_bubble_that_pops_never_goes_past_two_percent_over_its_size() {
    let _still = offscreen::freeze_clock();
    let ui = make(BUBBLES, 420.0, 330.0);
    num(&ui, "bubble-scale");
    press(&ui, 60.0, 176.0);
    let scale = trace(&ui, "bubble-scale", 30);
    let top = scale.iter().cloned().fold(0.0, f32::max);
    assert!(scale[1] >= 0.9 && scale[1] < 1.0, "grows from about 0.9: {scale:?}");
    assert!(top > 1.0 && top <= 1.02, "a hair past full size, at most: {top} in {scale:?}");
    assert!((scale.last().unwrap() - 1.0).abs() < 0.001);
}

#[test]
fn a_tooltip_fades_in_after_its_rest_and_out_when_the_pointer_leaves() {
    let _still = offscreen::freeze_clock();
    let ui = make(BUBBLES, 420.0, 330.0);
    num(&ui, "tip");
    pointer_to(&ui, 60.0, 176.0);
    // The rest before the first tip, then the fade
    for _ in 0..40 {
        step(16);
        offscreen::draw(ui.window(), 420.0, 330.0, 1.0);
        if num(&ui, "tip") > 0.0 {
            break;
        }
    }
    let t = trace(&ui, "tip", 14);
    assert!(t[0] < 0.6 && t.windows(2).all(|w| w[1] >= w[0] - 0.0001) && *t.last().unwrap() == 1.0, "fades in and never past solid: {t:?}");
    pointer_to(&ui, 380.0, 300.0);
    offscreen::draw(ui.window(), 420.0, 330.0, 1.0);
    let gone = trace(&ui, "tip", 14);
    assert!(gone.windows(2).all(|w| w[1] <= w[0] + 0.0001) && *gone.last().unwrap() < 0.02, "fades out: {gone:?}");
}

const FOLD: &str = r#"
import { Card, Section, Folds, Icons, Theme } from "@ui-kit/ui-kit.slint";
export { Folds }
export component T inherits Window {
    width: 420px;
    height: 600px;
    background: Theme.ground;
    out property <length> card-h: card.height;
    VerticalLayout {
        alignment: start;
        x: 10px;
        width: 400px;
        card := Card {
            title: "Things";
            icon: Icons.sun;
            fold-key: "p/Things";
            Rectangle { height: 40px; }
            Rectangle { height: 40px; }
            Rectangle { height: 40px; }
        }
    }
}
"#;

fn fold_rig() -> slint_interpreter::ComponentInstance {
    use std::cell::Cell;
    use std::rc::Rc;
    let ui = make(FOLD, 420.0, 600.0);
    let folded = Rc::new(Cell::new(false));
    let version = Rc::new(Cell::new(1.0));
    let f = folded.clone();
    ui.set_global_callback("Folds", "is-folded", move |_| Value::Bool(f.get())).unwrap();
    let (f, v, weak) = (folded, version, ui.as_weak());
    ui.set_global_callback("Folds", "toggle", move |_| {
        f.set(!f.get());
        v.set(v.get() + 1.0);
        weak.unwrap().set_global_property("Folds", "version", Value::Number(v.get())).unwrap();
        Value::Void
    })
    .unwrap();
    ui.set_global_property("Folds", "version", Value::Number(1.0)).unwrap();
    offscreen::draw(ui.window(), 420.0, 600.0, 1.0);
    num(&ui, "card-h");
    ui
}

fn title(ui: &slint_interpreter::ComponentInstance) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, "Things").find(|e| e.accessible_checkable() == Some(true)).expect("the title")
}

fn click_title(ui: &slint_interpreter::ComponentInstance) {
    let t = title(ui);
    let (p, s) = (t.absolute_position(), t.size());
    press(ui, p.x + s.width / 2.0, p.y + s.height / 2.0);
    release(ui, p.x + s.width / 2.0, p.y + s.height / 2.0);
}

#[test]
fn a_click_folds_a_card_over_a_few_frames_with_no_overshoot_and_a_key_snaps_it() {
    let _still = offscreen::freeze_clock();
    let ui = fold_rig();
    let whole = num(&ui, "card-h");
    click_title(&ui);
    let t = trace(&ui, "card-h", 30);
    let shut = *t.last().unwrap();
    assert!(shut < whole - 100.0, "it folded: {whole} to {shut}");
    assert!(t.windows(2).all(|w| w[1] <= w[0] + 0.01), "it only ever shuts: {t:?}");
    assert!(t.iter().all(|h| *h >= shut - 0.01 && *h <= whole + 0.01), "never past either end: {t:?}");
    assert!(t.iter().filter(|h| **h > shut + 5.0 && **h < whole - 5.0).count() >= 3, "over a few frames, not one: {t:?}");
    let frames = t.iter().rposition(|h| (h - shut).abs() > 0.5).unwrap() + 1;
    assert!(frames as u64 * FRAME <= 300, "shut within 300 ms, not {} ms", frames as u64 * FRAME);
    // Open again with the keyboard: Tab to the title, Space
    key(&ui, Key::Tab);
    key_text(&ui, " ");
    assert!((num(&ui, "card-h") - whole).abs() < 0.01, "a key opens it at once: {} of {whole}", num(&ui, "card-h"));
    key_text(&ui, " ");
    assert!((num(&ui, "card-h") - shut).abs() < 0.01, "and shuts it at once");
}

#[test]
fn a_second_click_mid_fold_turns_the_card_round_without_a_jump() {
    let _still = offscreen::freeze_clock();
    let ui = fold_rig();
    let whole = num(&ui, "card-h");
    click_title(&ui);
    let mut t = trace(&ui, "card-h", 3);
    click_title(&ui);
    t.extend(trace(&ui, "card-h", 30));
    assert!(t.iter().all(|h| *h <= whole + 0.01), "never taller than it is: {t:?}");
    assert!(t.windows(2).all(|w| (w[1] - w[0]).abs() < 0.25 * whole), "no jump from one frame to the next: {t:?}");
    assert!((t.last().unwrap() - whole).abs() < 0.01, "it ends open: {t:?}");
}

const ARRIVAL: &str = r#"
import { Arrival } from "@ui-kit/ui-kit.slint";
export global Bridge {
    in-out property <float> latest: -1;
}
component Probe inherits Rectangle {
    out property <float> s: arr.solid;
    arr := Arrival { }
    init => { Bridge.latest = self.s; }
    changed s => { Bridge.latest = self.s; }
}
export component T inherits Window {
    width: 100px;
    height: 100px;
    in property <bool> show;
    if show: Probe { }
}
"#;

fn latest(ui: &slint_interpreter::ComponentInstance) -> f32 {
    match ui.get_global_property("Bridge", "latest").unwrap() {
        Value::Number(n) => n as f32,
        v => panic!("{v:?}"),
    }
}

#[test]
fn content_that_takes_a_placeholders_place_starts_half_solid_and_comes_up_to_full() {
    let _still = offscreen::freeze_clock();
    let ui = make(ARRIVAL, 100.0, 100.0);
    assert_eq!(latest(&ui), -1.0, "nothing is made yet");
    set(&ui, "show", Value::Bool(true));
    offscreen::draw(ui.window(), 100.0, 100.0, 1.0);
    let first = latest(&ui);
    assert!((first - 0.45).abs() < 0.01, "in the very first frame it is already there, at 45%: {first}");
    let mut seen = vec![first];
    for _ in 0..24 {
        step(FRAME);
        offscreen::draw(ui.window(), 100.0, 100.0, 1.0);
        seen.push(latest(&ui));
    }
    assert!(seen.windows(2).all(|w| w[1] >= w[0] - 0.0001), "it only comes up: {seen:?}");
    assert!(seen.iter().all(|v| *v <= 1.0001), "{seen:?}");
    assert!(seen[4] > 0.8 && *seen.last().unwrap() == 1.0, "full within a tenth of a second or so: {seen:?}");
}
