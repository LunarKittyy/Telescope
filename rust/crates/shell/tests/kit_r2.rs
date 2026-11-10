// The kit additions of round 2: Status, ListRow, DialogButtons, Button.reserve, the Card's reset, active and header-only look,
// the gridded PictureSegmented, SwitchRow's problem mark and Unfolding.
mod motion_film;

use motion_film::*;
use slint::platform::{Key, WindowEvent};
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_shell::offscreen;

fn key(ui: &slint_interpreter::ComponentInstance, k: Key) {
    let text: slint::SharedString = k.into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}

const PARTS: &str = r#"
import { Button, Status, Tag, ListRow, DialogButtons, DialogFrame, SwitchRow, Icons, Theme, IconButton } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 400px;
    height: 300px;
    background: Theme.card;
    in property <string> word: "Auto";
    out property <length> button-w: b.width;
    out property <length> plain-w: p.width;
    out property <length> status-w: s.width;
    out property <length> row-h: r.height;
    HorizontalLayout {
        x: 10px;
        y: 10px;
        height: 32px;
        width: 380px;
        alignment: start;
        spacing: 10px;
        b := Button { text: root.word; reserve: ["Auto", "Looking", "No stream"]; }
        p := Button { text: root.word; }
    }
    s := Status { x: 10px; y: 60px; width: 150px; text: "Stopped"; kind: "err"; detail: "The virtual microphone stopped taking audio."; }
    r := ListRow { x: 10px; y: 100px; width: 300px; title: "Luna's phone"; sub: "Wi-Fi"; IconButton { icon: Icons.reset; label: "Rename"; } }
    DialogButtons { x: 10px; y: 180px; width: 300px; height: 40px; Button { text: "Cancel"; dialog: true; } Button { text: "Save"; role: "primary"; dialog: true; } }
    SwitchRow { x: 10px; y: 230px; width: 300px; icon: Icons.denoise; label: "Noise reduction"; problem: "RNNoise is missing"; problem-title: "Unavailable"; }
}
"#;

#[test]
fn a_button_with_reserved_texts_never_changes_width_and_a_status_and_a_row_are_drawn() {
    let _still = offscreen::freeze_clock();
    let ui = make(PARTS, 400.0, 300.0);
    let w = num(&ui, "button-w");
    let plain = num(&ui, "plain-w");
    set(&ui, "word", Value::String("No stream".into()));
    step(FRAME);
    assert_eq!(num(&ui, "button-w"), w, "the reserved button keeps its width");
    assert!(num(&ui, "plain-w") > plain, "and an ordinary one follows its text");
    set(&ui, "word", Value::String("Looking".into()));
    step(FRAME);
    assert_eq!(num(&ui, "button-w"), w);
    assert_eq!(num(&ui, "status-w"), 150.0);
    assert!(num(&ui, "row-h") >= 36.0, "a list row is a strip with room");
    offscreen::draw(ui.window(), 400.0, 300.0, 2.0).unwrap();
    if let Some(dir) = shots() {
        save(&dir, "r2-parts.png", &offscreen::draw(ui.window(), 400.0, 300.0, 2.0).unwrap());
    }
}

const CARDS: &str = r#"
import { Card, Slider, Icons, Folds, Theme } from "@ui-kit/ui-kit.slint";
export { Folds }
export component T inherits Window {
    width: 400px;
    height: 400px;
    background: Theme.ground;
    in property <bool> lit;
    in property <bool> live: true;
    in-out property <int> resets;
    out property <float> presence: full.presence;
    out property <bool> bodied: full.has-body;
    out property <bool> bare-bodied: bare.has-body;
    out property <length> bare-h: bare.height;
    VerticalLayout {
        alignment: start;
        x: 10px;
        y: 10px;
        width: 380px;
        spacing: 10px;
        full := Card { title: "Colour"; icon: Icons.sun; fold-key: "p/Colour"; idle: !root.live; has-reset: true; reset-lit: root.lit; reset-label: "Reset"; reset => { root.resets += 1; }
            Slider { label: "Contrast"; value: 30; } }
        bare := Card { title: "Expose for my face"; icon: Icons.sun; fold-key: "p/Face"; }
    }
}
"#;

#[test]
fn a_header_only_card_is_not_a_fold_and_a_card_fades_to_inactive_and_back_and_resets_when_lit() {
    let _still = offscreen::freeze_clock();
    let ui = make(CARDS, 400.0, 400.0);
    ui.set_global_callback("Folds", "is-folded", |_| Value::Bool(true)).unwrap();
    ui.set_global_property("Folds", "version", Value::Number(1.0)).unwrap();
    step(FRAME);
    assert_eq!(ui.get_property("bodied").unwrap(), Value::Bool(true));
    assert_eq!(ui.get_property("bare-bodied").unwrap(), Value::Bool(false), "nothing under the header");
    // The header-only card's title is no fold: nothing to toggle, folded or not, and its height is a header's
    let titles: Vec<_> = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Expose for my face").collect();
    assert!(titles.iter().all(|t| t.accessible_checkable() != Some(true)), "no fold on a card with nothing in it");
    assert!(num(&ui, "bare-h") < 58.0, "{}", num(&ui, "bare-h"));
    // Active fades both ways
    assert!(num(&ui, "presence") > 0.99);
    set(&ui, "live", Value::Bool(false));
    let mut t = vec![num(&ui, "presence")];
    for _ in 0..20 {
        step(FRAME);
        t.push(num(&ui, "presence"));
    }
    assert!(t.windows(2).all(|w| w[1] <= w[0] + 0.0001) && *t.last().unwrap() < 0.001 && t[1] > 0.1, "fades out over frames: {t:?}");
    set(&ui, "live", Value::Bool(true));
    let mut u = vec![num(&ui, "presence")];
    for _ in 0..20 {
        step(FRAME);
        u.push(num(&ui, "presence"));
    }
    assert!(u.windows(2).all(|w| w[1] >= w[0] - 0.0001) && *u.last().unwrap() > 0.99 && u[1] < 0.9, "and back in: {u:?}");
    // The reset button: inert until something is off its default
    let reset = || i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Reset").next().unwrap();
    assert_eq!(reset().accessible_enabled(), Some(false));
    set(&ui, "lit", Value::Bool(true));
    step(FRAME);
    assert_eq!(reset().accessible_enabled(), Some(true));
    let (p, s) = (reset().absolute_position(), reset().size());
    press(&ui, p.x + s.width / 2.0, p.y + s.height / 2.0);
    release(&ui, p.x + s.width / 2.0, p.y + s.height / 2.0);
    assert_eq!(ui.get_property("resets").unwrap(), Value::Number(1.0));
}

const GRID: &str = r#"
import { PictureSegmented, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 140px;
    background: Theme.card;
    in-out property <int> cur: 0;
    out property <int> shown: pics.shown;
    out property <length> t: pics.worm-top;
    out property <length> l: pics.worm-left;
    out property <length> h: pics.height;
    pics := PictureSegmented {
        x: 10px; y: 8px; width: 280px; label: "Style"; items: ["Bar", "Pill", "Light", "Shadow"]; disabled: [false, false, false, false];
        current: root.cur; columns: 2; hue: Theme.accent;
        Rectangle { }
    }
}
"#;

#[test]
fn a_gridded_picture_segmented_is_one_control_that_arrows_move_across_and_up_and_down() {
    let _still = offscreen::freeze_clock();
    let ui = make(GRID, 300.0, 140.0);
    step(FRAME);
    assert_eq!(num(&ui, "h"), 2.0 * 48.0 + 6.0, "two rows of tiles and a gap");
    // Focus the first tile with Tab, then Right, Down, Left, Up
    key(&ui, Key::Tab);
    key(&ui, Key::RightArrow);
    assert_eq!(num(&ui, "shown"), 1.0);
    key(&ui, Key::DownArrow);
    assert_eq!(num(&ui, "shown"), 3.0, "down goes to the tile under it");
    key(&ui, Key::LeftArrow);
    assert_eq!(num(&ui, "shown"), 2.0);
    key(&ui, Key::RightArrow);
    key(&ui, Key::RightArrow);
    assert_eq!(num(&ui, "shown"), 3.0, "an arrow never wraps round to another row");
    key(&ui, Key::UpArrow);
    assert_eq!(num(&ui, "shown"), 1.0);
    key(&ui, Key::UpArrow);
    assert_eq!(num(&ui, "shown"), 1.0, "nothing above the first row");
    // Light to Pill goes up and across in one move, and every picked place is on the grid
    step(600);
    press(&ui, 10.0 + 20.0, 8.0 + 78.0);
    release(&ui, 10.0 + 20.0, 8.0 + 78.0);
    step(600);
    assert_eq!((num(&ui, "l"), num(&ui, "t")), (0.0, 54.0), "Light is the second row's first tile");
    press(&ui, 10.0 + 200.0, 8.0 + 24.0);
    release(&ui, 10.0 + 200.0, 8.0 + 24.0);
    step(FRAME);
    step(FRAME);
    assert!(num(&ui, "t") < 54.0 && num(&ui, "t") > 0.0, "up is under way, not jumped: {}", num(&ui, "t"));
    step(600);
    assert_eq!((num(&ui, "l"), num(&ui, "t")), (143.0, 0.0), "and lands on Pill");
}

const UNFOLD: &str = r#"
import { Unfolding, UnfoldGroup, Slider, Theme, Settle } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 300px;
    height: 300px;
    background: Theme.card;
    in property <bool> open;
    out property <float> value: u.value;
    out property <length> tail-y: tail.y;
    out property <length> u-h: u.height;
    VerticalLayout {
        alignment: start;
        spacing: Theme.gap;
        width: 300px;
        Rectangle { height: 32px; }
        UnfoldGroup {
            Rectangle { height: 32px; }
            u := Unfolding { open: root.open; Slider { label: "Strength"; value: 30; } Slider { label: "Detail"; value: 30; } }
        }
        tail := Rectangle { height: 32px; }
    }
}
"#;

#[test]
fn rows_that_come_and_go_unfold_both_ways_leave_no_gap_and_turn_round_mid_way() {
    let _still = offscreen::freeze_clock();
    let ui = make(UNFOLD, 300.0, 300.0);
    step(FRAME);
    let shut = num(&ui, "tail-y");
    assert_eq!(shut, 32.0 + 6.0 + 32.0 + 6.0, "shut: no gap left behind");
    set(&ui, "open", Value::Bool(true));
    let mut t = vec![num(&ui, "tail-y")];
    for _ in 0..8 {
        step(FRAME);
        t.push(num(&ui, "tail-y"));
    }
    assert!(t.windows(2).all(|w| w[1] >= w[0]) && t[2] > shut + 5.0 && t[2] < t[8], "opens over frames: {t:?}");
    // Turned round mid-way, and again, and again
    for i in 0..20 {
        set(&ui, "open", Value::Bool(i % 2 == 1));
        step(FRAME);
        step(FRAME);
    }
    let mid = num(&ui, "value");
    assert!(mid > 0.0 && mid < 1.0, "still in between after twenty turns: {mid}");
    set(&ui, "open", Value::Bool(true));
    step(800);
    let open = num(&ui, "tail-y");
    assert!(open > shut + 60.0, "open: {open}");
    set(&ui, "open", Value::Bool(false));
    step(800);
    assert_eq!(num(&ui, "tail-y"), shut, "and shuts all the way, no gap");
}

const HELD: &str = r#"
import { Slider, PeekLayer, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 340px;
    height: 400px;
    background: Theme.ground;
    Slider { x: 10px; y: 330px; width: 320px; label: "Level"; value: 50; peek: "text"; peek-text: "Aa Bb"; peek-values: [14]; hue: #aa9cf5; }
    PeekLayer { }
}
"#;

/// Rows of the outline's colour along a straight edge of the held bubble: every column of the top edge must put it on the same pixel row.
#[test]
fn the_held_bubbles_top_edge_is_one_pixel_row_from_end_to_end_at_every_scale() {
    for scale in [1.0f32, 1.25, 1.5, 2.0] {
        let _still = offscreen::freeze_clock();
        let ui = make(HELD, 340.0, 400.0);
        let (w, h) = (340.0, 400.0);
        offscreen::draw(ui.window(), w, h, scale).unwrap();
        // The handle is at the middle of the slider: move it a fraction of a pixel so nothing is on a whole number
        press(&ui, 100.3, 346.0);
        for _ in 0..10 {
            step(FRAME);
            offscreen::draw(ui.window(), w, h, scale).unwrap();
        }
        pointer_to(&ui, 100.3 + 37.37, 346.0);
        for _ in 0..30 {
            step(FRAME);
            offscreen::draw(ui.window(), w, h, scale).unwrap();
        }
        let pic = offscreen::draw(ui.window(), w, h, scale).unwrap();
        let accent = [0xaa, 0x9c, 0xf5];
        let near = |i: usize| (0..3).all(|c| (pic.rgba[i + c] as i32 - accent[c] as i32).abs() < 40);
        let width = pic.width as usize;
        // For each column, the first row (from the top of the bubble area) that is the outline's colour
        let mut tops = Vec::new();
        for x in 0..width {
            if let Some(y) = (0..(300.0 * scale) as usize).find(|y| near((y * width + x) * 4)) {
                tops.push((x, y));
            }
        }
        assert!(!tops.is_empty(), "scale {scale}: an outline was drawn");
        // The top edge: the most common top row, and the columns on it; columns of the corners and the pointer are higher or lower, the
        // straight stretch between them must be a single row
        let mut counts = std::collections::BTreeMap::new();
        for (_, y) in &tops {
            *counts.entry(*y).or_insert(0usize) += 1;
        }
        let (&row, &n) = counts.iter().max_by_key(|(_, n)| **n).unwrap();
        let on_row: Vec<usize> = tops.iter().filter(|(_, y)| *y == row).map(|(x, _)| *x).collect();
        let (first, last) = (*on_row.first().unwrap(), *on_row.last().unwrap());
        // Between the first and the last column on the row, no column may be off it by a pixel (a step)
        let stepped: Vec<_> = tops.iter().filter(|(x, y)| *x >= first && *x <= last && *y != row).collect();
        assert!(n > 100 && stepped.is_empty(), "scale {scale}: the top edge steps: row {row} for {n} columns, off it: {:?}", &stepped[..stepped.len().min(12)]);
        if let Some(dir) = shots() {
            let crop = (0.0, 150.0, 340.0, 200.0);
            save(&dir, &format!("held-bubble-top-{scale}.png"), &strip(&[pic], crop, scale, 1));
        }
    }
}

const PAGES: &str = r#"
import { Switch, TipLayer, Tips, Theme } from "@ui-kit/ui-kit.slint";
export { Tips }
export component T inherits Window {
    width: 300px;
    height: 200px;
    background: Theme.card;
    in-out property <int> page: 0;
    in-out property <bool> built: true;
    Rectangle {
        visible: root.page == 0;
        Switch { x: 20px; y: 20px; label: "Load at start"; tip: "Load when the computer starts"; }
    }
    Rectangle {
        visible: root.page == 1;
        Switch { x: 120px; y: 120px; label: "Other"; tip: "Another page"; }
    }
    if root.built: Switch { x: 20px; y: 80px; label: "Gone soon"; tip: "Built by an if"; }
    TipLayer { }
}
"#;

fn tip_up(ui: &slint_interpreter::ComponentInstance) -> bool {
    matches!(ui.get_global_property("Tips", "shown"), Ok(Value::Bool(true)))
}

#[test]
fn a_tooltip_never_outlives_its_owner() {
    // The pointer rests on a switch, the page changes under it
    {
        let _still = offscreen::freeze_clock();
        let ui = make(PAGES, 300.0, 200.0);
        pointer_to(&ui, 40.0, 32.0);
        for _ in 0..60 {
            step(FRAME);
            offscreen::draw(ui.window(), 300.0, 200.0, 1.0).unwrap();
        }
        assert!(tip_up(&ui), "a resting pointer shows the tip");
        // A page that hides with `visible` leaves the pointer where it was (so the control still reads as hovered): the page says so
        set(&ui, "page", Value::Number(1.0));
        ui.invoke_global("Tips", "dismiss", &[]).unwrap();
        for _ in 0..90 {
            step(FRAME);
            offscreen::draw(ui.window(), 300.0, 200.0, 1.0).unwrap();
        }
        assert!(!tip_up(&ui), "the page changed: the tip is gone, and does not come back under the resting pointer");
        // Leaving and coming back shows it again
        pointer_to(&ui, 200.0, 190.0);
        pointer_to(&ui, 40.0, 92.0);
        for _ in 0..60 {
            step(FRAME);
            offscreen::draw(ui.window(), 300.0, 200.0, 1.0).unwrap();
        }
        assert!(tip_up(&ui), "a control entered again shows its tip");
    }
    // The keyboard focus is on a switch, the page changes
    {
        let _still = offscreen::freeze_clock();
        let ui = make(PAGES, 300.0, 200.0);
        key(&ui, Key::Tab);
        for _ in 0..30 {
            step(FRAME);
            offscreen::draw(ui.window(), 300.0, 200.0, 1.0).unwrap();
        }
        assert!(tip_up(&ui), "the focus shows the tip");
        set(&ui, "page", Value::Number(1.0));
        for _ in 0..60 {
            step(FRAME);
            offscreen::draw(ui.window(), 300.0, 200.0, 1.0).unwrap();
        }
        assert!(!tip_up(&ui), "the focused control is on a hidden page: the tip is gone");
    }
    // The owner is built by an `if` and removed while its tip is up
    {
        let _still = offscreen::freeze_clock();
        let ui = make(PAGES, 300.0, 200.0);
        pointer_to(&ui, 40.0, 92.0);
        for _ in 0..60 {
            step(FRAME);
            offscreen::draw(ui.window(), 300.0, 200.0, 1.0).unwrap();
        }
        assert!(tip_up(&ui));
        set(&ui, "built", Value::Bool(false));
        for _ in 0..60 {
            step(FRAME);
            offscreen::draw(ui.window(), 300.0, 200.0, 1.0).unwrap();
        }
        assert!(!tip_up(&ui), "removed: the tip is gone");
    }
}

const PADS: &str = r#"
import { Pad, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 340px;
    height: 230px;
    background: Theme.card;
    in-out property <float> vx: 6000;
    in-out property <float> vy: 0;
    in-out property <int> moves;
    out property <length> hx: pad.handle-x;
    out property <length> hy: pad.handle-y;
    pad := Pad {
        x: 10px; y: 10px; width: 300px; height: 200px; label: "Balance";
        x-minimum: 2000; x-maximum: 10000; step-x: 100; y-minimum: -50; y-maximum: 50; step-y: 1; y-up: true;
        has-default: true; default-x: 6000; default-y: 0; value-x: vx; value-y: vy;
        moved(x, y) => { root.vx = x; root.vy = y; root.moves += 1; }
    }
}
"#;

// A double-click on the handle itself is a reset, not a drag: the handle glides back over several frames instead of jumping under the pointer
#[test]
fn a_double_click_on_the_pan_maps_handle_glides_back_to_the_default() {
    let _still = offscreen::freeze_clock();
    let ui = make(PADS, 340.0, 230.0);
    set(&ui, "vx", Value::Number(9000.0));
    step(800);
    let (hx, hy) = (10.0 + num(&ui, "hx"), 10.0 + num(&ui, "hy"));
    press(&ui, hx, hy);
    release(&ui, hx, hy);
    step(60);
    press(&ui, hx, hy);
    release(&ui, hx, hy);
    let mut t = vec![num(&ui, "hx")];
    for _ in 0..40 {
        step(FRAME);
        t.push(num(&ui, "hx"));
    }
    let start = 10.0 + t[0];
    assert!(start > 150.0, "the handle was well to the right, and the first frame hasn't jumped to the default: {t:?}");
    assert!(t.iter().filter(|v| **v < t[0] - 8.0 && **v > t.last().unwrap() + 8.0).count() >= 4, "several frames on the way: {t:?}");
}

#[test]
fn a_double_click_on_the_pan_map_resets_without_the_handle_first_jumping_to_the_click() {
    let _still = offscreen::freeze_clock();
    let ui = make(PADS, 340.0, 230.0);
    set(&ui, "vx", Value::Number(9000.0));
    step(600);
    let before = num(&ui, "hx");
    // First click away from the handle: nothing moves while a second may follow
    press(&ui, 60.0, 60.0);
    release(&ui, 60.0, 60.0);
    for _ in 0..8 {
        step(FRAME);
        assert_eq!(num(&ui, "hx"), before, "the handle waits for the double-click interval");
    }
    assert_eq!(num(&ui, "moves"), 0.0);
    // The second press: a reset to the default, never to the click
    press(&ui, 60.0, 60.0);
    release(&ui, 60.0, 60.0);
    ui.window().dispatch_event(WindowEvent::PointerPressed { position: at(60.0, 60.0), button: slint::platform::PointerEventButton::Left });
    step(800);
    // Without a second click, a click alone moves it after the interval
    let ui = make(PADS, 340.0, 230.0);
    press(&ui, 60.0, 60.0);
    release(&ui, 60.0, 60.0);
    step(500);
    assert_eq!(num(&ui, "moves"), 1.0, "a lone click takes effect after the interval");
    // A drag takes hold at once
    let ui = make(PADS, 340.0, 230.0);
    press(&ui, 60.0, 60.0);
    pointer_to(&ui, 120.0, 90.0);
    assert!(num(&ui, "moves") >= 1.0, "a drag does not wait");
}

const CAPS: &str = r#"
import { Button, Segmented, PictureSegmented, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 320px;
    height: 160px;
    background: Theme.card;
    Button { x: 10px; y: 10px; width: 100px; text: "HHH"; }
    Button { x: 120px; y: 10px; width: 100px; text: "HHH"; has-icon: false; font-weight: 500; }
    Segmented { x: 10px; y: 60px; width: 200px; label: "S"; items: ["HHH", "HHH"]; disabled: [false, false]; current: 1; }
    PictureSegmented { x: 230px; y: 10px; width: 80px; label: "P"; items: ["HHH"]; disabled: [false]; show-names: true; current: -1; tile-height: 48px; art-size: 20px; Rectangle { Rectangle { width: parent.width; height: 20px; background: #ff0000; } } }
}
"#;

#[test]
fn centred_words_sit_with_their_capitals_on_the_middle_of_their_box_and_a_tile_block_is_centred() {
    for scale in [1.0f32, 1.25, 1.5, 2.0] {
        let _still = offscreen::freeze_clock();
        let ui = make(CAPS, 320.0, 160.0);
        step(FRAME);
        step(FRAME);
        let pic = offscreen::draw(ui.window(), 320.0, 160.0, scale).unwrap();
        let w = pic.width as usize;
        // Rows (in logical px) where text-bright pixels are within the box: x range, y range
        let ink = |x0: f32, x1: f32, y0: f32, y1: f32, bright: bool| {
            let (mut top, mut bot) = (usize::MAX, 0usize);
            for y in (y0 * scale) as usize..(y1 * scale) as usize {
                for x in (x0 * scale) as usize..(x1 * scale) as usize {
                    let p = &pic.rgba[(y * w + x) * 4..];
                    let lum = (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3;
                    if (bright && lum > 170 && !(p[0] > 200 && p[1] < 80)) || (!bright && p[0] > 200 && p[1] < 80) {
                        top = top.min(y);
                        bot = bot.max(y);
                    }
                }
            }
            (top as f32 / scale, (bot + 1) as f32 / scale)
        };
        let middle = |(t, b): (f32, f32)| (t + b) / 2.0;
        // A button 32 high at y 10: its middle is 26
        let button = middle(ink(20.0, 100.0, 10.0, 42.0, true));
        // A segment of the row 32 high at y 60: its middle is 76
        let seg = middle(ink(110.0, 210.0, 60.0, 92.0, true));
        let tolerance = 0.5 + 0.5 / scale;
        assert!((button - 26.0).abs() <= tolerance, "scale {scale}: a button's capitals' middle is {button}, the box's 26");
        assert!((seg - 76.0).abs() <= tolerance, "scale {scale}: a picked segment's capitals' middle is {seg}, the row's 76");
        // The tile block: the red drawing's top to the name's bottom, centred in the tile 48 high at y 10 (middle 34)
        let art = ink(230.0, 310.0, 10.0, 58.0, false);
        let name = ink(230.0, 310.0, 10.0, 58.0, true);
        let block = (art.0, name.1);
        let (above, below) = (block.0 - 10.0, 58.0 - block.1);
        assert!((above - below).abs() <= 1.0 + 1.0 / scale, "scale {scale}: the block has {above} above and {below} below");
    }
}

const LEAVING_PAGES: &str = r#"
import { PageGroup, Page, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 200px;
    height: 100px;
    background: Theme.card;
    in property <int> tab: 1;
    out property <int> a-clicks;
    out property <int> b-clicks;
    out property <float> b-value: b.value;
    PageGroup {
        x: 0px;
        y: 0px;
        width: 200px;
        clip: a.moving || b.moving;
        a := Page { shown: tab == 0; lead: 0px; Rectangle { height: 40px; TouchArea { clicked => { a-clicks += 1; } } } }
        b := Page { shown: tab == 1; lead: 0px; Rectangle { height: 40px; TouchArea { clicked => { b-clicks += 1; } } } }
    }
}
"#;

fn tap(ui: &slint_interpreter::ComponentInstance, x: f32, y: f32) {
    pointer_to(ui, x, y);
    press(ui, x, y);
    release(ui, x, y);
}

#[test]
fn a_page_that_is_leaving_takes_no_clicks_and_the_one_that_comes_takes_them() {
    let _still = offscreen::freeze_clock();
    let ui = make(LEAVING_PAGES, 200.0, 100.0);
    step(800);
    tap(&ui, 100.0, 20.0);
    assert_eq!((num(&ui, "a-clicks"), num(&ui, "b-clicks")), (0.0, 1.0), "the second page is up");
    // The second page is the later one, so it is drawn over the first: going back, it must not keep the pointer
    set(&ui, "tab", Value::Number(0.0));
    for _ in 0..40 {
        step(FRAME);
        tap(&ui, 100.0, 20.0);
        assert_eq!(num(&ui, "b-clicks"), 1.0, "the leaving page took a click at value {}", num(&ui, "b-value"));
    }
    step(800);
    tap(&ui, 100.0, 20.0);
    assert_eq!(num(&ui, "b-clicks"), 1.0);
    assert!(num(&ui, "a-clicks") > 0.0, "the page that came answers once it is up");
}

const KEYED_PAGES: &str = r#"
import { PageGroup, Page, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 200px;
    height: 160px;
    background: Theme.card;
    in property <int> tab: 0;
    out property <int> a-keys;
    out property <int> b-keys;
    out property <float> a-value: a.value;
    out property <bool> a-focused: af.has-focus;
    PageGroup {
        x: 0px;
        y: 0px;
        width: 200px;
        clip: a.moving || b.moving;
        a := Page { shown: tab == 0; lead: 0px; Rectangle { height: 40px; af := FocusScope { key-pressed(e) => { a-keys += 1; accept } } } }
        b := Page { shown: tab == 1; lead: 0px; Rectangle { height: 40px; bf := FocusScope { key-pressed(e) => { b-keys += 1; accept } } } }
    }
}
"#;

const KEYED_ROWS: &str = r#"
import { Unfolding, UnfoldGroup, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 200px;
    height: 80px;
    background: Theme.card;
    in property <bool> more: true;
    out property <int> u-keys;
    out property <float> u-value: u.value;
    UnfoldGroup {
        x: 0px;
        y: 0px;
        width: 200px;
        u := Unfolding { open: more; lead: 0px; Rectangle { height: 40px; uf := FocusScope { key-pressed(e) => { u-keys += 1; accept } } } }
    }
}
"#;

#[test]
fn a_page_that_is_leaving_takes_no_keys_while_it_still_fades() {
    let _still = offscreen::freeze_clock();
    let ui = make(KEYED_PAGES, 200.0, 160.0);
    step(800);
    key(&ui, Key::Tab);
    assert!(truth(&ui, "a-focused"), "Tab lands on the first page's control, so the wrappers add no stop");
    space(&ui);
    assert_eq!(num(&ui, "a-keys"), 1.0);
    set(&ui, "tab", Value::Number(1.0));
    step(FRAME);
    assert!(num(&ui, "a-value") > 0.5 && num(&ui, "a-value") < 1.0, "still fading at {}", num(&ui, "a-value"));
    space(&ui);
    assert_eq!(num(&ui, "a-keys"), 1.0, "a key to the page that is going does nothing");
    // Going back, the page takes its keys again
    set(&ui, "tab", Value::Number(0.0));
    step(800);
    space(&ui);
    assert_eq!(num(&ui, "a-keys"), 2.0);
}

#[test]
fn rows_that_are_folding_away_take_no_keys_either() {
    let _still = offscreen::freeze_clock();
    let ui = make(KEYED_ROWS, 200.0, 80.0);
    step(800);
    key(&ui, Key::Tab);
    space(&ui);
    assert_eq!(num(&ui, "u-keys"), 1.0, "the row is the one stop");
    set(&ui, "more", Value::Bool(false));
    step(FRAME);
    assert!(num(&ui, "u-value") > 0.0 && num(&ui, "u-value") < 1.0);
    space(&ui);
    assert_eq!(num(&ui, "u-keys"), 1.0);
}

fn truth(ui: &slint_interpreter::ComponentInstance, name: &str) -> bool {
    matches!(ui.get_property(name).unwrap(), Value::Bool(true))
}

fn space(ui: &slint_interpreter::ComponentInstance) {
    let text: slint::SharedString = " ".into();
    ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window().dispatch_event(WindowEvent::KeyReleased { text });
}
