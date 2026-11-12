// The kit's motions (ui-kit/ui/widgets/motion.slint) on a clock the test moves itself: where each named motion goes, frame by frame, and what
// the controls built on them do. MOTION_SHOTS=dir also writes a filmstrip of each (frames side by side, 16 ms apart) to look at.
use slint::ComponentHandle;
use slint_interpreter::{ComponentInstance, Value};
use std::time::Duration;
use telescope_shell::offscreen;
use telescope_shell::compile;

const FRAME: u64 = 16;

fn make(doc: &str, w: f32, h: f32) -> ComponentInstance {
    offscreen::install();
    let def = compile::compile("t.slint", doc, Vec::new(), "T").unwrap_or_else(|e| panic!("{e}"));
    let ui = def.create().unwrap();
    ui.window().set_size(slint::LogicalSize::new(w, h));
    ui.show().unwrap();
    offscreen::pump();
    ui
}

fn num(ui: &ComponentInstance, name: &str) -> f32 {
    match ui.get_property(name).unwrap_or_else(|e| panic!("{name}: {e:?}")) {
        Value::Number(n) => n as f32,
        v => panic!("{name}: {v:?}"),
    }
}

fn set(ui: &ComponentInstance, name: &str, v: Value) {
    ui.set_property(name, v).unwrap_or_else(|e| panic!("{name}: {e:?}"));
}

fn flag(ui: &ComponentInstance, name: &str, on: bool) {
    set(ui, name, Value::Bool(on));
}

fn step(ms: u64) {
    offscreen::skip(Duration::from_millis(ms));
}

/// One number sampled every frame while the clock runs on, starting with its value now.
fn trace(ui: &ComponentInstance, name: &str, frames: usize) -> Vec<f32> {
    let mut seen = vec![num(ui, name)];
    for _ in 0..frames {
        step(FRAME);
        seen.push(num(ui, name));
    }
    seen
}

fn speeds(trace: &[f32]) -> Vec<f32> {
    trace.windows(2).map(|w| (w[1] - w[0]) / (FRAME as f32 / 1000.0)).collect()
}

const MOTIONS: &str = r#"
import { Settle, Flick, Pop, Tone, Fade, Unfold, Motion } from "@ui-kit/ui-kit.slint";
export { Motion }
export component T inherits Window {
    width: 100px;
    height: 100px;
    in property <float> to;
    in property <bool> on;
    in property <bool> snap;
    out property <float> settle: settle-m.value;
    out property <float> flick: flick-m.value;
    out property <float> pop: pop-m.value;
    out property <float> scale: pop-m.scale;
    out property <float> tone: tone-m.value;
    out property <float> fade: fade-m.value;
    out property <float> unfold: unfold-m.value;
    settle-m := Settle { to: root.to; snap: root.snap; }
    flick-m := Flick { to: root.to; snap: root.snap; }
    pop-m := Pop { to: root.to; snap: root.snap; }
    tone-m := Tone { on: root.on; snap: root.snap; }
    fade-m := Fade { shown: root.on; snap: root.snap; }
    unfold-m := Unfold { open: root.on; snap: root.snap; }
}
"#;

/// The motions, every value read once so the next change animates, the clock still.
fn motions() -> (ComponentInstance, offscreen::FrozenClock) {
    let still = offscreen::freeze_clock();
    let ui = make(MOTIONS, 100.0, 100.0);
    for name in ["settle", "flick", "pop", "scale", "tone", "fade", "unfold"] {
        num(&ui, name);
    }
    (ui, still)
}

fn go(ui: &ComponentInstance, to: f32) {
    set(ui, "to", Value::Number(to as f64));
    flag(ui, "on", to > 0.5);
    // Reading starts the move
    for name in ["settle", "flick", "pop", "tone", "fade", "unfold"] {
        num(ui, name);
    }
}

/// How many frames until a trace stops moving for good (within a thousandth of where it ends), or None while it still does at the end.
fn rests_after(t: &[f32]) -> Option<usize> {
    let end = *t.last().unwrap();
    t.iter().rposition(|v| (v - end).abs() > 0.001).map(|i| i + 1).or(Some(0)).filter(|i| *i < t.len() - 1)
}

#[test]
fn settle_goes_calmly_and_never_past_its_place() {
    let (ui, _still) = motions();
    go(&ui, 1.0);
    let t = trace(&ui, "settle", 30);
    assert!(t.windows(2).all(|w| w[1] >= w[0]), "a move that only ever goes on: {t:?}");
    assert!(t.iter().all(|v| *v <= 1.0001), "no overshoot at all: {t:?}");
    assert!(t[3] > 0.3 && t[3] < 0.6, "about half there after three frames: {t:?}");
    let frames = rests_after(&t).expect("at rest");
    assert!(frames as u64 * FRAME <= 320, "at rest within a third of a second, not {} ms: {t:?}", frames as u64 * FRAME);
    go(&ui, 0.0);
    let back = trace(&ui, "settle", 30);
    assert!(back.iter().all(|v| *v >= -0.0001), "and back the same way: {back:?}");
}

#[test]
fn flick_passes_its_place_a_little_and_comes_back() {
    let (ui, _still) = motions();
    go(&ui, 1.0);
    let t = trace(&ui, "flick", 30);
    let top = t.iter().cloned().fold(0.0, f32::max);
    assert!(top > 1.01 && top < 1.06, "a little overshoot, not a bounce: {top} in {t:?}");
    assert!(t.iter().filter(|v| **v > 1.0).count() >= 2, "long enough to see");
    // It comes back to 1 and stays: one rise and one fall, no second overshoot
    let after_top = t.iter().position(|v| *v == top).unwrap();
    assert!(t[after_top..].iter().all(|v| *v > 0.99), "{t:?}");
    assert!(t[after_top..].windows(2).all(|w| w[1] <= w[0] + 0.0001), "one rise and one fall, then it settles: {t:?}");
    // A short move is a calm one: the first frame under 8% of the travel and none over 17% (3.4 px of a switch's 20)
    let steps: Vec<f32> = t.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(steps[0] < 0.08 && steps.iter().all(|s| *s < 0.17), "calm steps: {steps:?}");
    let frames = rests_after(&t).expect("at rest");
    assert!(frames as u64 * FRAME <= 500, "at rest within 400 ms, not {} ms: {t:?}", frames as u64 * FRAME);
}

#[test]
fn pop_scales_in_with_a_small_overshoot() {
    let (ui, _still) = motions();
    go(&ui, 1.0);
    let t = trace(&ui, "scale", 30);
    assert!((t[0] - 0.9).abs() < 0.001, "starts small: {t:?}");
    let top = t.iter().cloned().fold(0.0, f32::max);
    assert!(top > 1.0 && top < 1.04, "overshoots by a hair, at most four percent of its size: {top}");
    assert!(rests_after(&t).is_some_and(|f| f as u64 * FRAME <= 400), "{t:?}");
}

#[test]
fn colour_and_opacity_motions_never_leave_zero_to_one_even_when_turned_round() {
    let (ui, _still) = motions();
    for name in ["tone", "fade", "unfold"] {
        let mut seen = Vec::new();
        // Up for 50 ms, down for 30, up for 40, down for 200: each reversal carries the speed it had
        for (to, frames) in [(1.0, 3), (0.0, 2), (1.0, 3), (0.0, 14)] {
            go(&ui, to);
            seen.extend(trace(&ui, name, frames));
        }
        assert!(seen.iter().all(|v| (0.0..=1.0).contains(v)), "{name} stays inside 0 to 1: {seen:?}");
    }
}

#[test]
fn a_new_target_bends_the_move_and_keeps_its_speed() {
    let (ui, _still) = motions();
    go(&ui, 1.0);
    let mut seen = trace(&ui, "settle", 4);
    let before = speeds(&seen);
    let fastest = before.iter().cloned().fold(0.0, f32::max);
    // Turned back to 0.5 while it is racing for 1
    go(&ui, 0.5);
    seen.extend(trace(&ui, "settle", 30).into_iter().skip(1));
    let all = speeds(&seen);
    let i = before.len();
    // No jump in the place: the first step after the turn is no longer than the one before it
    assert!(all[i] <= all[i - 1] * 1.05 + 0.01 && all[i] > 0.0, "the speed carries on into the turn: {all:?}");
    // No jump in the speed either: it changes by a fraction between frames, never to rest and off again
    for w in all.windows(2) {
        assert!((w[1] - w[0]).abs() < fastest * 0.5, "no kink in the speed: {all:?}");
    }
    // It may run on past 0.5 a little, as a thing with weight does, but it ends there and never leaves 0 to 1
    assert!(seen.iter().all(|v| (0.0..=1.0001).contains(v)), "{seen:?}");
    assert!((seen.last().unwrap() - 0.5).abs() < 0.001, "{seen:?}");
}

#[test]
fn a_finger_or_a_key_takes_over_at_once() {
    let (ui, _still) = motions();
    go(&ui, 1.0);
    trace(&ui, "settle", 2);
    flag(&ui, "snap", true);
    set(&ui, "to", Value::Number(0.2));
    for name in ["settle", "flick", "pop", "tone"] {
        let v = num(&ui, name);
        if name != "tone" && name != "pop" {
            assert!((v - 0.2).abs() < 0.0001, "{name} is at the new target on the very next read: {v}");
        }
    }
    go(&ui, 0.2);
    flag(&ui, "snap", false);
}

#[test]
fn reduced_motion_snaps_the_moves_and_keeps_the_colours() {
    let (ui, _still) = motions();
    telescope_shell::motion::set(&ui, telescope_shell::motion::Motion::Reduced);
    go(&ui, 1.0);
    assert_eq!(num(&ui, "settle"), 1.0);
    assert_eq!(num(&ui, "flick"), 1.0);
    assert_eq!(num(&ui, "unfold"), 1.0);
    step(FRAME);
    let tone = num(&ui, "tone");
    let fade = num(&ui, "fade");
    assert!(tone > 0.0 && tone < 1.0, "a colour still eases: {tone}");
    assert!(fade > 0.0 && fade < 1.0, "an opacity still fades: {fade}");
    telescope_shell::motion::set(&ui, telescope_shell::motion::Motion::Off);
    go(&ui, 0.0);
    for name in ["settle", "flick", "tone", "fade", "unfold"] {
        assert_eq!(num(&ui, name), 0.0, "{name} with motion off");
    }
}

#[test]
fn nothing_asks_for_frames_once_everything_is_at_rest() {
    let (ui, _still) = motions();
    let busy = || i_slint_core::animations::CURRENT_ANIMATION_DRIVER.with(|d| d.has_active_animations());
    go(&ui, 1.0);
    step(FRAME);
    for name in ["settle", "flick", "tone"] {
        num(&ui, name);
    }
    assert!(busy(), "while it moves it wants frames");
    for _ in 0..40 {
        step(FRAME);
        for name in ["settle", "flick", "pop", "tone", "fade", "unfold"] {
            num(&ui, name);
        }
    }
    step(FRAME);
    assert!(!busy(), "and when it has settled it asks for none");
}
