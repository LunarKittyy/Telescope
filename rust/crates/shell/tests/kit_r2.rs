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
