// The worm: the picked option's outline stretches onto a newly picked option. With MOTION_SHOTS=dir set it also draws filmstrips of it.
mod motion_film;

use motion_film::*;
use slint::ComponentHandle;
use slint_interpreter::Value;
use slint::platform::{Key, WindowEvent};
use telescope_shell::offscreen;

fn key(ui: &slint_interpreter::ComponentInstance, k: Key) {
    let text: slint::SharedString = k.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

const ROW: &str = r#"
import { Segmented, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 40px;
    background: Theme.card;
    in-out property <int> cur: 0;
    in property <length> w: 280px;
    out property <length> l: seg.worm-left;
    out property <length> r: seg.worm-right;
    out property <int> shown: seg.shown;
    seg := Segmented {
        x: 10px; y: 10px; width: root.w;
        label: "Pick";
        items: ["One", "Two", "Three", "Four"];
        disabled: [false, false, false, false];
        current: root.cur;
        hue: Theme.accent;
    }
}
"#;

fn row() -> slint_interpreter::ComponentInstance {
    let ui = make(ROW, 300.0, 40.0);
    // Settles into place at the start
    step(FRAME);
    step(FRAME);
    offscreen::draw(ui.window(), 300.0, 40.0, 1.0).unwrap();
    ui
}

/// Cell i of four in a row 280 px wide, in the row's own coordinates: each is (280 - 3 gaps) / 4 wide.
fn cell(i: usize) -> (f32, f32) {
    cell_in(i, 280.0)
}

fn cell_in(i: usize, row: f32) -> (f32, f32) {
    let gap = 6.0;
    let w = (row - 3.0 * gap) / 4.0;
    (i as f32 * (w + gap), i as f32 * (w + gap) + w)
}

fn edges(ui: &slint_interpreter::ComponentInstance) -> (f32, f32) {
    (num(ui, "l"), num(ui, "r"))
}

#[test]
fn the_worm_starts_on_the_picked_option_without_travelling_there() {
    let _still = offscreen::freeze_clock();
    let ui = row();
    let (l, r) = edges(&ui);
    let (cl, cr) = cell(0);
    assert!((l - cl).abs() < 0.6 && (r - cr).abs() < 0.6, "worm at {l}..{r}, cell at {cl}..{cr}");
}

#[test]
fn a_pick_stretches_the_leading_edge_first_and_both_land_exactly_in_the_same_time_however_far() {
    let _still = offscreen::freeze_clock();
    for target in [1usize, 3] {
        let ui = row();
        set(&ui, "cur", Value::Number(target as f64));
        step(FRAME);
        let (l0, r0) = edges(&ui);
        let (from_l, from_r) = cell(0);
        let mut max_width = r0 - l0;
        let mut lead_first = false;
        let mut lead_done = None;
        let mut all_done = None;
        let (cl, cr) = cell(target);
        for f in 0..40 {
            let (l, r) = edges(&ui);
            max_width = max_width.max(r - l);
            // Moving right: the right edge is the leading one, so it has gone further than the left edge has
            if (r - from_r) > (l - from_l) + 1.0 {
                lead_first = true;
            }
            if lead_done.is_none() && (r - cr).abs() < 0.6 {
                lead_done = Some(f);
            }
            if all_done.is_none() && (l - cl).abs() < 0.6 && (r - cr).abs() < 0.6 {
                all_done = Some(f);
            }
            step(FRAME);
        }
        assert!(lead_first, "the leading edge goes first (target {target})");
        assert!(max_width > (from_r - from_l) + (cl - from_l) * 0.5, "it stretches across the gap before it contracts (target {target}): widest {max_width}");
        let (l, r) = edges(&ui);
        assert!((l - cl).abs() < 0.1 && (r - cr).abs() < 0.1, "it lands exactly: {l}..{r} for {cl}..{cr}");
        let done = all_done.expect("it arrives");
        // About 330 ms at most, and it doesn't take longer for a longer way
        assert!(done <= 24, "target {target} took {done} frames");
        let lead = lead_done.expect("the leading edge arrives");
        assert!(lead <= done, "the leading edge lands first");
    }
}

#[test]
fn how_far_it_jumps_changes_the_time_by_no_more_than_a_frame_or_two() {
    let _still = offscreen::freeze_clock();
    let mut frames = Vec::new();
    for target in [1usize, 3] {
        let ui = row();
        set(&ui, "cur", Value::Number(target as f64));
        step(FRAME);
        let (cl, cr) = cell(target);
        let mut n = 0usize;
        for f in 0..60 {
            let (l, r) = edges(&ui);
            if (l - cl).abs() > 1.0 || (r - cr).abs() > 1.0 {
                n = f;
            }
            step(FRAME);
        }
        frames.push(n);
    }
    assert!(frames[0].abs_diff(frames[1]) <= 2, "{frames:?}");
}

#[test]
fn the_trailing_edge_passes_its_place_a_little_on_the_way_in() {
    let _still = offscreen::freeze_clock();
    let ui = row();
    set(&ui, "cur", Value::Number(3.0));
    step(FRAME);
    let (cl, _) = cell(3);
    let mut past = 0.0f32;
    for _ in 0..40 {
        let (l, _) = edges(&ui);
        past = past.max(l - cl);
        step(FRAME);
    }
    assert!(past > 0.5 && past < 14.0, "the trailing edge passed its place by {past}");
}

#[test]
fn a_click_glides_to_the_option_and_a_key_puts_the_worm_there_at_once() {
    let _still = offscreen::freeze_clock();
    let ui = row();
    // A click on the third option: under way after a frame, not there yet
    let (cl, cr) = cell(2);
    let x = 10.0 + (cl + cr) / 2.0;
    press(&ui, x, 20.0);
    release(&ui, x, 20.0);
    step(FRAME);
    step(FRAME);
    let (l, r) = edges(&ui);
    assert!(r > cell(0).1 + 1.0, "a click starts it moving: {l}..{r}");
    assert!(r < cl + (cr - cl) || l < cl - 1.0, "and it is not there yet: {l}..{r}");
    for _ in 0..60 {
        step(FRAME);
    }
    // Right arrow from the focused option goes to the next at once
    key(&ui, Key::RightArrow);
    step(FRAME);
    let (l, r) = edges(&ui);
    let (nl, nr) = cell(3);
    assert!((l - nl).abs() < 0.6 && (r - nr).abs() < 0.6, "a key puts it there: {l}..{r} for {nl}..{nr}");
}

#[test]
fn a_resize_moves_the_worm_with_the_layout_not_after_it() {
    let _still = offscreen::freeze_clock();
    let ui = row();
    set(&ui, "cur", Value::Number(1.0));
    for _ in 0..60 {
        step(FRAME);
    }
    set(&ui, "w", Value::Number(240.0));
    step(FRAME);
    let (l, r) = edges(&ui);
    let (cl, cr) = cell_in(1, 240.0);
    assert!((l - cl).abs() < 0.6 && (r - cr).abs() < 0.6, "{l}..{r} for {cl}..{cr}");
}

#[test]
fn shots_of_the_worm() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    let ui = row();
    let mut frames = vec![offscreen::draw(ui.window(), 300.0, 40.0, 3.0).unwrap()];
    set(&ui, "cur", Value::Number(3.0));
    step(FRAME);
    let mut text = String::new();
    for _ in 0..24 {
        let (l, r) = edges(&ui);
        text.push_str(&format!("{l:.1} {r:.1}\n"));
        step(FRAME);
    }
    std::fs::write(dir.join("worm-trace.txt"), text).unwrap();
    for _ in 0..40 {
        step(FRAME);
    }
    set(&ui, "cur", Value::Number(0.0));
    offscreen::draw(ui.window(), 300.0, 40.0, 1.0).unwrap();
    for _ in 0..60 {
        step(FRAME);
    }
    offscreen::draw(ui.window(), 300.0, 40.0, 1.0).unwrap();
    set(&ui, "cur", Value::Number(3.0));
    frames.extend(film(&ui, (300.0, 40.0), 3.0, 23, |_| {}));
    save(&dir, "worm-segmented-far.png", &strip(&frames, (0.0, 0.0, 300.0, 40.0), 3.0, 2));
    let mut back = vec![offscreen::draw(ui.window(), 300.0, 40.0, 3.0).unwrap()];
    step(500);
    set(&ui, "cur", Value::Number(2.0));
    back.extend(film(&ui, (300.0, 40.0), 3.0, 17, |_| {}));
    save(&dir, "worm-segmented-back-one.png", &strip(&back, (0.0, 0.0, 300.0, 40.0), 3.0, 2));
}
