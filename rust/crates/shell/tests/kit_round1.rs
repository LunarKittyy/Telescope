// Round 1 of the motion pass: the bubble's one outline, the held bubble following the handle, the switch's ease-in, the dropdown and the
// button's press. With MOTION_SHOTS=dir set it also draws pictures of them (both looks) for looking at.
mod motion_film;

use motion_film::*;
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_shell::offscreen;

const BUBBLES: &str = r#"
import { BubbleFrame, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component T inherits Window {
    width: 360px;
    height: 330px;
    background: Theme.card;
    // Mid-window, a quarter in, hard against the left edge, hard against the right edge; the last opens below
    BubbleFrame { ax: 170px; ay: 130px; aw: 14px; ah: 24px; window-w: 360px; window-h: 330px; content-width: 100px; hue: Theme.accent;
        Rectangle { height: 30px; } }
    BubbleFrame { ax: 14px; ay: 200px; aw: 14px; ah: 24px; window-w: 360px; window-h: 330px; content-width: 100px; hue: Theme.accent;
        Rectangle { height: 30px; } }
    BubbleFrame { ax: 335px; ay: 270px; aw: 14px; ah: 24px; window-w: 360px; window-h: 330px; content-width: 100px; hue: Theme.accent;
        Rectangle { height: 30px; } }
    BubbleFrame { ax: 70px; ay: 10px; aw: 14px; ah: 24px; window-w: 360px; window-h: 330px; content-width: 100px; hue: Theme.accent;
        Rectangle { height: 30px; } }
}
"#;

#[test]
fn shots_of_the_bubble_outline() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let ui = make(BUBBLES, 360.0, 330.0);
        ui.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
        let pic = offscreen::draw(ui.window(), 360.0, 330.0, 3.0).unwrap();
        save(&dir, if classic { "bubble-outline-classic.png" } else { "bubble-outline-new.png" }, &pic);
    }
}

const HELD: &str = r#"
import { Slider, PeekLayer, Peeks, Theme } from "@ui-kit/ui-kit.slint";
export { Peeks, Theme }
export component T inherits Window {
    width: 420px;
    height: 200px;
    background: Theme.ground;
    in-out property <float> v: 50;
    sl := Slider { x: 10px; y: 100px; width: 300px; label: "Level"; minimum: 0; maximum: 100; value: root.v; peek: "text"; peek-text: "Aa"; peek-values: [14]; moved(x) => { root.v = x; } }
    PeekLayer { }
}
"#;

fn global_length(ui: &slint_interpreter::ComponentInstance, name: &str) -> f32 {
    match ui.get_global_property("Peeks", name).unwrap() {
        Value::Number(n) => n as f32,
        v => panic!("{name}: {v:?}"),
    }
}

#[test]
fn a_held_bubble_points_at_the_handle_and_follows_it() {
    let _still = offscreen::freeze_clock();
    let ui = make(HELD, 420.0, 200.0);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    // The handle is 14 px wide and its left edge is 111 px along the track at the middle; the track starts at x 10
    press(&ui, 10.0 + 7.0 + 111.0, 116.0);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    step(FRAME);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    assert_eq!(ui.get_global_property("Peeks", "shown").unwrap(), Value::Bool(true));
    let (ax, aw) = (global_length(&ui, "ax"), global_length(&ui, "aw"));
    assert!((ax - (10.0 + 111.0)).abs() < 1.5 && (aw - 14.0).abs() < 0.1, "it points at the handle: {ax} {aw}");
    // Dragged to the right, the thing it points at goes with the handle
    pointer_to(&ui, 10.0 + 7.0 + 0.8 * 222.0, 116.0);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    step(FRAME);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    let now = global_length(&ui, "ax");
    assert!((now - (10.0 + 0.8 * 222.0)).abs() < 1.5, "it follows the handle: {now}");
    release(&ui, 10.0 + 7.0 + 0.8 * 222.0, 116.0);
}

// ---- the rest of round 1: scenes that are drawn in both looks, and numbers for what they show

fn themed(doc: &str, w: f32, h: f32, classic: bool) -> slint_interpreter::ComponentInstance {
    let ui = make(doc, w, h);
    ui.set_global_property("Theme", "classic", Value::Bool(classic)).unwrap();
    ui
}

fn name(base: &str, classic: bool) -> String {
    format!("{base}-{}.png", if classic { "classic" } else { "new" })
}

const SWITCH: &str = r#"
import { Switch, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
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
fn a_switch_eases_in_from_rest_and_barely_passes_its_place() {
    let _still = offscreen::freeze_clock();
    let ui = make(SWITCH, 120.0, 36.0);
    offscreen::draw(ui.window(), 120.0, 36.0, 1.0).unwrap();
    num(&ui, "place");
    press(&ui, 50.0, 18.0);
    release(&ui, 50.0, 18.0);
    offscreen::draw(ui.window(), 120.0, 36.0, 1.0).unwrap();
    let mut t = vec![num(&ui, "place")];
    for _ in 0..24 {
        step(FRAME);
        t.push(num(&ui, "place"));
    }
    // 60 frames a second: the first frames move little, most of the way is gone by the middle
    assert!(t[1] < 0.1, "the first frame hardly moves: {t:?}");
    assert!(t[2] < 0.25 && t[3] < 0.45, "and it picks up speed: {t:?}");
    assert!(t[7] > 0.6 && t[7] < 0.95, "{t:?}");
    let top = t.iter().cloned().fold(0.0, f32::max);
    assert!(top > 1.005 && top < 1.03, "a small overshoot: {top}");
    assert!((t.last().unwrap() - 1.0).abs() < 0.001, "{t:?}");
    // 180 to 220 ms to get there
    let settled = t.iter().position(|v| (v - 1.0).abs() < 0.02).unwrap();
    assert!((8..=14).contains(&settled), "settles in {} ms", settled * 16);
}

#[test]
fn shots_of_the_switch_in_both_looks() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let ui = themed(SWITCH, 120.0, 36.0, classic);
        let mut frames = vec![offscreen::draw(ui.window(), 120.0, 36.0, 4.0).unwrap()];
        press(&ui, 50.0, 18.0);
        release(&ui, 50.0, 18.0);
        frames.extend(film(&ui, (120.0, 36.0), 4.0, 15, |_| {}));
        save(&dir, &name("switch-flick", classic), &strip(&frames, (30.0, 0.0, 60.0, 36.0), 4.0, 8));
    }
}

const TILES: &str = r#"
import { ToggleTile, Theme, Icons } from "@ui-kit/ui-kit.slint";
export { Theme }
export component T inherits Window {
    width: 200px;
    height: 56px;
    background: Theme.card;
    out property <float> lit: tile.lit;
    tile := ToggleTile { x: 10px; y: 8px; width: 180px; text: "Tone"; icon: Icons.sun; hue: Theme.accent; }
}
"#;

#[test]
fn a_toggle_tile_fades_its_outline_and_inside_in_and_out() {
    let _still = offscreen::freeze_clock();
    let ui = make(TILES, 200.0, 56.0);
    offscreen::draw(ui.window(), 200.0, 56.0, 1.0).unwrap();
    num(&ui, "lit");
    press(&ui, 100.0, 28.0);
    release(&ui, 100.0, 28.0);
    offscreen::draw(ui.window(), 200.0, 56.0, 1.0).unwrap();
    let mut on = vec![num(&ui, "lit")];
    for _ in 0..14 {
        step(FRAME);
        on.push(num(&ui, "lit"));
    }
    assert!(on[1] > 0.0 && on[1] < 0.9, "not a jump from 0 to 1: {on:?}");
    assert!(on.windows(2).all(|w| w[1] >= w[0] - 0.0001) && (0.0..=1.0).contains(&on[3]) && on.iter().all(|v| (0.0..=1.0).contains(v)), "{on:?}");
    assert_eq!(*on.last().unwrap(), 1.0);
    step(500);
    press(&ui, 100.0, 28.0);
    release(&ui, 100.0, 28.0);
    offscreen::draw(ui.window(), 200.0, 56.0, 1.0).unwrap();
    let mut off = vec![num(&ui, "lit")];
    for _ in 0..14 {
        step(FRAME);
        off.push(num(&ui, "lit"));
    }
    assert!(off[1] < 1.0 && off[1] > 0.1, "and fades out the same way: {off:?}");
    assert_eq!(*off.last().unwrap(), 0.0);
}

#[test]
fn shots_of_a_toggle_tile_in_both_looks() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let ui = themed(TILES, 200.0, 56.0, classic);
        let mut frames = vec![offscreen::draw(ui.window(), 200.0, 56.0, 3.0).unwrap()];
        press(&ui, 100.0, 28.0);
        release(&ui, 100.0, 28.0);
        frames.extend(film(&ui, (200.0, 56.0), 3.0, 11, |_| {}));
        step(500);
        press(&ui, 100.0, 28.0);
        release(&ui, 100.0, 28.0);
        frames.extend(film(&ui, (200.0, 56.0), 3.0, 11, |_| {}));
        save(&dir, &name("toggle-tile-fade", classic), &strip(&frames, (0.0, 0.0, 200.0, 56.0), 3.0, 4));
    }
}

const BUTTON: &str = r#"
import { Button, IconButton, Theme, Icons } from "@ui-kit/ui-kit.slint";
export { Theme }
export component T inherits Window {
    width: 220px;
    height: 44px;
    background: Theme.card;
    in-out property <int> clicks;
    out property <float> tone: b.pressed-tone;
    b := Button { x: 10px; y: 6px; width: 120px; text: "Reset"; clicked => { root.clicks += 1; } }
    IconButton { x: 150px; y: 6px; icon: Icons.sun; label: "Sun"; }
}
"#;

#[test]
fn a_click_on_a_button_leaves_a_press_that_is_seen_for_a_moment() {
    let _still = offscreen::freeze_clock();
    let ui = make(BUTTON, 220.0, 44.0);
    offscreen::draw(ui.window(), 220.0, 44.0, 1.0).unwrap();
    num(&ui, "tone");
    // A quick click: held for two frames
    press(&ui, 70.0, 22.0);
    offscreen::draw(ui.window(), 220.0, 44.0, 1.0).unwrap();
    step(FRAME);
    step(FRAME);
    offscreen::draw(ui.window(), 220.0, 44.0, 1.0).unwrap();
    release(&ui, 70.0, 22.0);
    offscreen::draw(ui.window(), 220.0, 44.0, 1.0).unwrap();
    let mut t = vec![num(&ui, "tone")];
    for _ in 0..16 {
        step(FRAME);
        t.push(num(&ui, "tone"));
    }
    assert_eq!(num(&ui, "clicks"), 1.0);
    assert!(t[0] > 0.8, "the press is there at once: {t:?}");
    assert!(t[6] > 0.2, "and is still visible a tenth of a second later: {t:?}");
    assert!(t.windows(2).all(|w| w[1] <= w[0] + 0.0001) && *t.last().unwrap() < 0.05, "then it goes back: {t:?}");
}

#[test]
fn shots_of_a_button_press_in_both_looks() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let ui = themed(BUTTON, 220.0, 44.0, classic);
        let mut frames = vec![offscreen::draw(ui.window(), 220.0, 44.0, 3.0).unwrap()];
        press(&ui, 70.0, 22.0);
        frames.extend(film(&ui, (220.0, 44.0), 3.0, 3, |_| {}));
        release(&ui, 70.0, 22.0);
        frames.extend(film(&ui, (220.0, 44.0), 3.0, 12, |_| {}));
        save(&dir, &name("button-press", classic), &strip(&frames, (0.0, 0.0, 220.0, 44.0), 3.0, 4));
    }
}

const LIST: &str = r#"
import { ChoiceField, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component T inherits Window {
    width: 260px;
    height: 200px;
    background: Theme.card;
    in-out property <int> picked;
    out property <bool> open: f.open;
    out property <float> shown: f.list-shown;
    f := ChoiceField { x: 10px; y: 10px; width: 220px; compact: true; items: ["Off", "Low", "Medium", "High"]; current: root.picked; picked(i) => { root.picked = i; } }
}
"#;

#[test]
fn a_dropdown_fades_in_and_out_and_a_key_puts_it_away_at_once() {
    let _still = offscreen::freeze_clock();
    let ui = make(LIST, 260.0, 200.0);
    offscreen::draw(ui.window(), 260.0, 200.0, 1.0).unwrap();
    num(&ui, "shown");
    press(&ui, 100.0, 26.0);
    release(&ui, 100.0, 26.0);
    let mut open = Vec::new();
    for _ in 0..16 {
        offscreen::draw(ui.window(), 260.0, 200.0, 1.0).unwrap();
        open.push(num(&ui, "shown"));
        step(FRAME);
    }
    assert!(open[0] < 0.2, "starts clear: {open:?}");
    assert!(open.windows(2).all(|w| w[1] >= w[0] - 0.0001) && open.iter().all(|v| (0.0..=1.0).contains(v)), "{open:?}");
    assert_eq!(*open.last().unwrap(), 1.0);
    // Pick the third row with the pointer: it fades out and is gone a little after
    let row_y = 10.0 + 32.0 + 4.0 + 4.0 + 26.0 * 2.5;
    press(&ui, 100.0, row_y);
    release(&ui, 100.0, row_y);
    offscreen::draw(ui.window(), 260.0, 200.0, 1.0).unwrap();
    assert_eq!(ui.get_property("open").unwrap(), Value::Bool(false), "it counts as closed as soon as it is picked");
    let mut out = Vec::new();
    for _ in 0..12 {
        offscreen::draw(ui.window(), 260.0, 200.0, 1.0).unwrap();
        out.push(num(&ui, "shown"));
        step(FRAME);
    }
    assert!(out[0] > 0.3 && out.windows(2).all(|w| w[1] <= w[0] + 0.0001) && *out.last().unwrap() < 0.05, "fades out: {out:?}");
}

#[test]
fn shots_of_a_dropdown_in_both_looks() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let ui = themed(LIST, 260.0, 200.0, classic);
        let mut frames = vec![offscreen::draw(ui.window(), 260.0, 200.0, 2.0).unwrap()];
        press(&ui, 100.0, 26.0);
        release(&ui, 100.0, 26.0);
        frames.extend(film(&ui, (260.0, 200.0), 2.0, 9, |_| {}));
        let row_y = 10.0 + 32.0 + 4.0 + 4.0 + 26.0 * 2.5;
        press(&ui, 100.0, row_y);
        release(&ui, 100.0, row_y);
        frames.extend(film(&ui, (260.0, 200.0), 2.0, 8, |_| {}));
        save(&dir, &name("dropdown-open-close", classic), &strip(&frames, (0.0, 0.0, 260.0, 200.0), 2.0, 6));
    }
}

const PICTURES: &str = r#"
import { PictureSegmented, AnchorFrame, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component T inherits Window {
    width: 300px;
    height: 150px;
    background: Theme.card;
    in-out property <int> cur: 0;
    in-out property <int> anchor: 0;
    out property <length> l: pics.worm-left;
    out property <length> r: pics.worm-right;
    out property <length> al: frame.worm-left;
    out property <length> ar: frame.worm-right;
    out property <length> at: frame.worm-top;
    out property <length> ab: frame.worm-bottom;
    pics := PictureSegmented {
        x: 10px; y: 8px; width: 280px; label: "Shape"; items: ["A", "B", "C", "D"]; disabled: [false, false, false, false]; current: root.cur; hue: Theme.accent;
        Rectangle { }
    }
    frame := AnchorFrame {
        x: 10px; y: 64px; width: 280px; height: 80px; label: "Corner";
        items: ["Top left", "Top right", "Bottom left", "Bottom right"];
        across: [0.0, 1.0, 0.0, 1.0];
        down: [0.0, 0.0, 1.0, 1.0];
        moves: [-1, 1, -1, 2, 0, -1, -1, 3, -1, 3, 0, -1, 2, -1, 1, -1];
        current: root.anchor;
        hue: Theme.accent;
    }
}
"#;

#[test]
fn picture_tiles_and_the_corner_picker_wear_the_worm_too_and_a_grid_goes_across_then_down() {
    let _still = offscreen::freeze_clock();
    let ui = make(PICTURES, 300.0, 150.0);
    offscreen::draw(ui.window(), 300.0, 150.0, 1.0).unwrap();
    step(FRAME);
    offscreen::draw(ui.window(), 300.0, 150.0, 1.0).unwrap();
    let start = (num(&ui, "al"), num(&ui, "ar"), num(&ui, "at"), num(&ui, "ab"));
    set(&ui, "cur", Value::Number(3.0));
    set(&ui, "anchor", Value::Number(3.0));
    offscreen::draw(ui.window(), 300.0, 150.0, 1.0).unwrap();
    step(FRAME);
    offscreen::draw(ui.window(), 300.0, 150.0, 1.0).unwrap();
    let (mut across_first, mut tiles_stretch) = (false, false);
    let mut seen = Vec::new();
    for _ in 0..30 {
        let (l, r, t, b) = (num(&ui, "al"), num(&ui, "ar"), num(&ui, "at"), num(&ui, "ab"));
        seen.push((l, r, t, b));
        // Across first: the right edge has done a good deal more of its way than the bottom edge (no delay: one spring a longer period)
        if (r - start.1) / (268.0 - start.1) - (b - start.3) / (68.0 - start.3) > 0.12 {
            across_first = true;
        }
        if num(&ui, "r") - num(&ui, "l") > 100.0 {
            tiles_stretch = true;
        }
        step(FRAME);
    }
    assert!(across_first, "across leads, down follows: {seen:?}");
    assert!(tiles_stretch, "the picture tiles' outline stretches between two tiles");
    let end = seen.last().unwrap();
    assert!((end.1 - 280.0 + 6.0 + 0.0).abs() < 40.0, "the bar ends in the far corner: {end:?}");
    assert!(end.3 > start.3 + 40.0, "and at the bottom: {end:?}");
}

#[test]
fn shots_of_the_picture_tiles_and_the_corner_picker_in_both_looks() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let ui = themed(PICTURES, 300.0, 150.0, classic);
        let mut frames = vec![offscreen::draw(ui.window(), 300.0, 150.0, 2.0).unwrap()];
        step(FRAME);
        offscreen::draw(ui.window(), 300.0, 150.0, 1.0).unwrap();
        set(&ui, "cur", Value::Number(3.0));
        set(&ui, "anchor", Value::Number(3.0));
        offscreen::draw(ui.window(), 300.0, 150.0, 1.0).unwrap();
        frames.extend(film(&ui, (300.0, 150.0), 2.0, 19, |_| {}));
        save(&dir, &name("worm-tiles-and-corners", classic), &strip(&frames, (0.0, 0.0, 300.0, 150.0), 2.0, 4));
    }
}

// ---- held bubbles at the edges, the wheel's puck, the tooltip's hint, a shut card, a slider mid-fold

const EDGE: &str = r#"
import { Slider, PeekLayer, Peeks, Theme } from "@ui-kit/ui-kit.slint";
export { Peeks, Theme }
export component T inherits Window {
    width: 420px;
    height: 200px;
    background: Theme.ground;
    in-out property <float> v: 50;
    sl := Slider { x: 110px; y: 100px; width: 300px; label: "Level"; minimum: 0; maximum: 100; value: root.v; peek: "text"; peek-text: "Aa"; peek-values: [14]; moved(x) => { root.v = x; } }
    PeekLayer { }
}
"#;

#[test]
fn shots_of_a_held_bubble_at_both_ends_and_the_middle_in_both_looks() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let mut pictures = Vec::new();
        for v in [3.0, 30.0, 60.0, 97.0] {
            let ui = themed(EDGE, 420.0, 200.0, classic);
            set(&ui, "v", Value::Number(v));
            offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
            step(500);
            offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
            let x = 110.0 + 7.0 + v as f32 / 100.0 * 222.0;
            press(&ui, x, 116.0);
            offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
            for _ in 0..12 {
                step(FRAME);
                offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
            }
            pictures.push(offscreen::draw(ui.window(), 420.0, 200.0, 2.0).unwrap());
        }
        save(&dir, &name("held-bubble-places", classic), &strip(&pictures, (0.0, 20.0, 420.0, 130.0), 2.0, 2));
    }
}

#[test]
fn a_held_bubbles_pointer_slides_along_its_edge_while_the_bubble_is_held_in_by_the_window() {
    let _still = offscreen::freeze_clock();
    let ui = make(EDGE, 420.0, 200.0);
    set(&ui, "v", Value::Number(97.0));
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    step(500);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    press(&ui, 110.0 + 7.0 + 0.97 * 222.0, 116.0);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    step(FRAME);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    let at = |ui: &slint_interpreter::ComponentInstance| global_length(ui, "ax");
    let near = at(&ui);
    // Dragged on to the very end: the handle goes 4 px further, and the thing the bubble points at with it
    pointer_to(&ui, 110.0 + 7.0 + 222.0, 116.0);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    step(FRAME);
    offscreen::draw(ui.window(), 420.0, 200.0, 1.0).unwrap();
    assert!(at(&ui) > near + 3.0, "the pointer's target follows the handle to the end: {near} then {}", at(&ui));
    release(&ui, 110.0 + 7.0 + 222.0, 116.0);
}

const WHEEL: &str = r#"
import { ColourWheel, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component T inherits Window {
    width: 120px;
    height: 120px;
    background: Theme.card;
    ColourWheel { x: 0px; y: 0px; width: 120px; height: 120px; label: "Lift"; hue: Theme.accent; puck-colour: #808080; }
}
"#;

#[test]
fn shots_of_the_wheels_puck_at_the_middle_at_three_scales_in_both_looks() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let mut pictures = Vec::new();
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let ui = themed(WHEEL, 120.0, 120.0, classic);
            let p = offscreen::draw(ui.window(), 120.0, 120.0, scale).unwrap();
            // The middle 44 logical px, scaled up by nearest to 8 px a logical px
            let (c, half) = ((60.0 * scale) as usize, (22.0 * scale) as usize);
            let (w, h) = (2 * half, 2 * half);
            let mut rgba = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let i = ((c - half + y) * p.width as usize + c - half + x) * 4;
                    rgba.extend_from_slice(&p.rgba[i..i + 4]);
                }
            }
            let crop = offscreen::Picture { width: w as u32, height: h as u32, rgba };
            // nearest-neighbour up to about 300 px
            let k = (300 / w).max(1);
            let mut big = Vec::new();
            for y in 0..h * k {
                for x in 0..w * k {
                    let i = ((y / k) * w + x / k) * 4;
                    big.extend_from_slice(&crop.rgba[i..i + 4]);
                }
            }
            pictures.push(offscreen::Picture { width: (w * k) as u32, height: (h * k) as u32, rgba: big });
        }
        // Pad to a common size then lay out in a row
        let (cw, ch) = (pictures.iter().map(|p| p.width).max().unwrap() as usize, pictures.iter().map(|p| p.height).max().unwrap() as usize);
        let padded: Vec<offscreen::Picture> = pictures
            .iter()
            .map(|p| {
                let mut rgba = vec![96u8; cw * ch * 4];
                for y in 0..p.height as usize {
                    for x in 0..p.width as usize {
                        let (from, to) = ((y * p.width as usize + x) * 4, (y * cw + x) * 4);
                        rgba[to..to + 4].copy_from_slice(&p.rgba[from..from + 4]);
                    }
                }
                offscreen::Picture { width: cw as u32, height: ch as u32, rgba }
            })
            .collect();
        save(&dir, &name("wheel-puck-centre", classic), &strip(&padded, (0.0, 0.0, cw as f32, ch as f32), 1.0, 4));
    }
}

const TIPS: &str = r#"
import { TipBody, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component T inherits Window {
    width: 300px;
    height: 150px;
    background: Theme.card;
    TipBody { x: 20px; y: 10px; says: "Speed"; detail: "How quickly the framing follows you."; hint: "Double-click to reset"; keys: ["Ctrl", "E"]; }
    TipBody { x: 20px; y: 100px; says: "Noise reduction"; detail: "Smooths out grain in the picture."; }
}
"#;

#[test]
fn shots_of_the_tooltips_third_part_in_both_looks() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let ui = themed(TIPS, 300.0, 150.0, classic);
        let p = offscreen::draw(ui.window(), 300.0, 150.0, 3.0).unwrap();
        save(&dir, &name("tooltip-hint", classic), &p);
    }
}

const SHUT: &str = r#"
import { Card, Folds, Icons, Theme } from "@ui-kit/ui-kit.slint";
export { Folds, Theme }
export component T inherits Window {
    width: 420px;
    height: 120px;
    background: Theme.ground;
    out property <length> card-h: card.height;
    VerticalLayout {
        alignment: start;
        x: 10px;
        y: 10px;
        width: 400px;
        card := Card {
            title: "Photos";
            icon: Icons.sun;
            fold-key: "p/Photos";
            Rectangle { height: 40px; }
        }
    }
}
"#;

#[test]
fn a_shut_card_has_the_same_room_above_its_title_as_below_it() {
    let _still = offscreen::freeze_clock();
    let ui = make(SHUT, 420.0, 120.0);
    ui.set_global_callback("Folds", "is-folded", |_| Value::Bool(true)).unwrap();
    ui.set_global_property("Folds", "version", Value::Number(1.0)).unwrap();
    offscreen::draw(ui.window(), 420.0, 120.0, 1.0).unwrap();
    step(500);
    offscreen::draw(ui.window(), 420.0, 120.0, 1.0).unwrap();
    // 12 above the header row, the row, 12 below
    assert_eq!(num(&ui, "card-h"), 12.0 + 32.0 + 12.0);
}

#[test]
fn shots_of_a_shut_card() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let ui = themed(SHUT, 420.0, 120.0, classic);
        ui.set_global_callback("Folds", "is-folded", |_| Value::Bool(true)).unwrap();
        ui.set_global_property("Folds", "version", Value::Number(1.0)).unwrap();
        offscreen::draw(ui.window(), 420.0, 120.0, 3.0).unwrap();
        step(500);
        let p = offscreen::draw(ui.window(), 420.0, 120.0, 3.0).unwrap();
        save(&dir, &name(&std::env::var("SHUT_NAME").unwrap_or_else(|_| "card-shut-after".into()).replace(".png", ""), classic), &strip(&[p], (0.0, 0.0, 420.0, 90.0), 3.0, 1));
    }
}

const FOLDING: &str = r#"
import { Card, Slider, Folds, Icons, Theme, Motion } from "@ui-kit/ui-kit.slint";
export { Folds, Theme, Motion }
export component T inherits Window {
    width: 420px;
    height: 220px;
    background: Theme.ground;
    VerticalLayout {
        alignment: start;
        x: 10px;
        y: 10px;
        width: 400px;
        card := Card {
            title: "Things";
            icon: Icons.sun;
            fold-key: "p/Things";
            Slider { label: "One"; minimum: 0; maximum: 100; value: 30; hue: Theme.accent; }
            Slider { label: "Two"; minimum: 0; maximum: 100; value: 70; hue: Theme.accent; }
            Slider { label: "Three"; minimum: 0; maximum: 100; value: 50; hue: Theme.accent; }
        }
    }
}
"#;

#[test]
fn shots_of_sliders_folding_away_in_both_looks() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let ui = themed(FOLDING, 420.0, 220.0, classic);
        let folded = std::rc::Rc::new(std::cell::Cell::new(false));
        let version = std::rc::Rc::new(std::cell::Cell::new(1.0));
        let f = folded.clone();
        ui.set_global_callback("Folds", "is-folded", move |_| Value::Bool(f.get())).unwrap();
        ui.set_global_property("Folds", "version", Value::Number(1.0)).unwrap();
        let mut frames = vec![offscreen::draw(ui.window(), 420.0, 220.0, 3.0).unwrap()];
        // Fold it as a click would: the same toggle the title makes
        ui.set_global_property("Motion", "by-pointer", Value::Bool(true)).unwrap();
        folded.set(true);
        version.set(2.0);
        ui.set_global_property("Folds", "version", Value::Number(2.0)).unwrap();
        frames.extend(film(&ui, (420.0, 220.0), 3.0, 11, |_| {}));
        save(&dir, &name("sliders-folding", classic), &strip(&frames, (0.0, 0.0, 210.0, 160.0), 3.0, 4));
    }
}

const HINTED: &str = r#"
import { Button, TipLayer, Tips, Theme } from "@ui-kit/ui-kit.slint";
export { Tips, Theme }
export component T inherits Window {
    width: 300px;
    height: 120px;
    background: Theme.card;
    Button { x: 20px; y: 20px; width: 100px; text: "Reset"; tip: "Reset"; tip-detail: "Puts the value back."; tip-hint: "Double-click to reset"; }
    TipLayer { }
}
"#;

#[test]
fn a_controls_hint_reaches_the_tooltip_as_its_own_part() {
    let _still = offscreen::freeze_clock();
    let ui = make(HINTED, 300.0, 120.0);
    offscreen::draw(ui.window(), 300.0, 120.0, 1.0).unwrap();
    pointer_to(&ui, 60.0, 36.0);
    for _ in 0..50 {
        step(FRAME);
        offscreen::draw(ui.window(), 300.0, 120.0, 1.0).unwrap();
    }
    assert_eq!(ui.get_global_property("Tips", "says").unwrap(), Value::String("Reset".into()));
    assert_eq!(ui.get_global_property("Tips", "detail").unwrap(), Value::String("Puts the value back.".into()));
    assert_eq!(ui.get_global_property("Tips", "hint").unwrap(), Value::String("Double-click to reset".into()));
}

const STEPPED: &str = r#"
import { Slider, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component T inherits Window {
    width: 340px;
    height: 44px;
    background: Theme.card;
    in-out property <float> v: 1;
    out property <float> place: sl.handle-place;
    out property <float> shown: sl.shown;
    sl := Slider { x: 10px; y: 6px; width: 320px; label: "Steps"; minimum: 1; maximum: 5; step: 1; value: root.v; hue: Theme.accent; readout: "\{root.v}"; moved(x) => { root.v = x; } }
}
"#;

/// The window x of the handle's middle at step `n` (1 to 5): the track is 320 - 56 (readout) - 8 wide, the handle 14 across.
fn step_x(n: f32) -> f32 {
    10.0 + 7.0 + (n - 1.0) / 4.0 * (320.0 - 56.0 - 8.0 - 14.0)
}

#[test]
fn a_stepped_slider_is_a_magnet_while_dragged_and_springs_into_its_slot_when_let_go() {
    let _still = offscreen::freeze_clock();
    let ui = make(STEPPED, 340.0, 44.0);
    offscreen::draw(ui.window(), 340.0, 44.0, 1.0).unwrap();
    num(&ui, "place");
    let stride = step_x(2.0) - step_x(1.0);
    // A press on the handle holds it, then the pointer goes a quarter of a step: the handle is held back near its step
    press(&ui, step_x(1.0), 22.0);
    pointer_to(&ui, step_x(1.0) + stride * 0.25, 22.0);
    let quarter = num(&ui, "place") * 4.0;
    assert!(quarter > 0.04 && quarter < 0.2, "held back near its step, not 1 to 1: {quarter}");
    assert_eq!(num(&ui, "shown"), 1.0);
    // Just short of the midpoint: the value hasn't changed and the handle is nearly at the pointer
    pointer_to(&ui, step_x(1.0) + stride * 0.47, 22.0);
    assert_eq!(num(&ui, "shown"), 1.0, "the value waits for the midpoint");
    assert!(num(&ui, "place") * 4.0 > 0.4, "nearly 1 to 1 by the midpoint: {}", num(&ui, "place") * 4.0);
    // Past it: the value changes and the handle has not jumped (it is the pointer's place at the midpoint from either side)
    let before = num(&ui, "place");
    pointer_to(&ui, step_x(1.0) + stride * 0.53, 22.0);
    assert_eq!(num(&ui, "shown"), 2.0, "the value changes at the midpoint");
    assert!((num(&ui, "place") - before).abs() * 4.0 < 0.2, "no jump at the midpoint: {before} to {}", num(&ui, "place"));
    // Let go: it springs into the slot, over frames and never back
    pointer_to(&ui, step_x(1.0) + stride * 0.6, 22.0);
    release(&ui, step_x(1.0) + stride * 0.6, 22.0);
    let mut t = vec![num(&ui, "place")];
    for _ in 0..24 {
        step(FRAME);
        t.push(num(&ui, "place"));
    }
    assert!(t.windows(2).all(|w| w[1] >= w[0] - 0.0001), "one slide: {t:?}");
    assert!((t.last().unwrap() - 0.25).abs() < 0.002, "into the slot of step 2: {t:?}");
    assert!(t.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max) * 320.0 < 6.0, "calm: {t:?}");
}

const SLIDER_SMALL: &str = r#"
import { Slider, Theme } from "@ui-kit/ui-kit.slint";
export { Theme }
export component T inherits Window {
    width: 340px;
    height: 44px;
    background: Theme.card;
    in-out property <float> v: 20;
    out property <float> place: sl.handle-place;
    sl := Slider { x: 10px; y: 6px; width: 320px; label: "Fine"; minimum: 0; maximum: 100; step: 1; value: root.v; hue: Theme.accent; moved(x) => { root.v = x; } }
}
"#;

#[test]
fn a_slider_with_small_steps_still_sticks_to_the_pointer() {
    let _still = offscreen::freeze_clock();
    let ui = make(SLIDER_SMALL, 340.0, 44.0);
    offscreen::draw(ui.window(), 340.0, 44.0, 1.0).unwrap();
    press(&ui, 10.0 + 7.0 + 0.2 * 242.0, 22.0);
    pointer_to(&ui, 10.0 + 7.0 + 0.9 * 242.0, 22.0);
    assert!((num(&ui, "place") - 0.9).abs() < 0.02, "a fine slider has the handle under the pointer at once: {}", num(&ui, "place"));
    release(&ui, 10.0 + 7.0 + 0.9 * 242.0, 22.0);
}

#[test]
fn shots_of_a_stepped_slider_dragged_across_its_steps_in_both_looks() {
    let Some(dir) = shots() else { return };
    let _still = offscreen::freeze_clock();
    for classic in [false, true] {
        let ui = themed(STEPPED, 340.0, 44.0, classic);
        let mut frames = vec![offscreen::draw(ui.window(), 340.0, 44.0, 2.0).unwrap()];
        press(&ui, step_x(1.0), 22.0);
        // The pointer crosses one step every 5 frames, then goes back to the start in one move
        for target in [2.0, 3.0, 4.0, 5.0] {
            pointer_to(&ui, step_x(target), 22.0);
            frames.extend(film(&ui, (340.0, 44.0), 2.0, 5, |_| {}));
        }
        pointer_to(&ui, step_x(1.0), 22.0);
        frames.extend(film(&ui, (340.0, 44.0), 2.0, 12, |_| {}));
        release(&ui, step_x(1.0), 22.0);
        save(&dir, &name("stepped-slider-drag", classic), &strip(&frames, (0.0, 0.0, 340.0, 44.0), 2.0, 3));
    }
}
