// The kit follow-up after round 2: Segmented that fits, Status that explains itself when cut, the lens row that reflows, the Peek that fades between
// drawings, LinkText on the same line as other words, SwitchRow's word, ChoiceField's width. KIT2_SHOTS=dir also writes pictures.
mod motion_film;

use motion_film::*;
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_shell::offscreen;


fn shots2() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("KIT2_SHOTS")?);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn labelled(ui: &slint_interpreter::ComponentInstance, label: &str) -> Vec<i_slint_backend_testing::ElementHandle> {
    i_slint_backend_testing::ElementHandle::find_by_accessible_label(ui, label).collect()
}

const SEG: &str = r#"
import { Segmented, Button, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    in property <length> w: 336px;
    in property <string> side: "Add";
    width: 420px;
    height: 120px;
    background: Theme.card;
    s := Segmented { x: 10px; y: 10px; width: root.w; label: "S"; items: ["Wi-Fi", "USB", "Browser camera", "Another"]; current: 0; }
    HorizontalLayout {
        x: 10px; y: 60px; width: 400px; height: 32px; spacing: 6px;
        Segmented { label: "N"; items: ["Off", "Fast", "High quality"]; current: 2; }
        Button { text: root.side; }
    }
}
"#;

// No name is cut while the row has room for all of them, at any width a card gives it (a camera card's rail is 336 px), and a row too narrow
// for them shrinks them together and stays inside its width
#[test]
fn a_segmented_row_fits_its_names_below_336_px_and_a_neighbour_that_grows_does_not_cut_them() {
    let _still = offscreen::freeze_clock();
    let ui = make(SEG, 420.0, 120.0);
    for w in [336.0, 300.0, 280.0] {
        set(&ui, "w", Value::Number(w as f64));
        step(FRAME);
        let names = ["Wi-Fi", "USB", "Browser camera", "Another"];
        let tiles: Vec<f32> = names.iter().map(|n| labelled(&ui, &format!("S: {n}"))[0].size().width).collect();
        // The longest name's room is more than the others' when it needs it, and the row adds up to its width
        let total: f32 = tiles.iter().sum::<f32>() + 3.0 * 6.0;
        assert!((total - w).abs() < 1.5, "{w}: {tiles:?} adds to {total}");
        assert!(tiles[2] >= 100.0, "{w}: 'Browser camera' keeps its room: {tiles:?}");
        assert!(tiles[1] >= 36.0, "{w}: {tiles:?}");
    }
    // Narrower than all of them need: every tile gives way, none is lost or pushed out
    set(&ui, "w", Value::Number(200.0));
    step(FRAME);
    let tiles: Vec<f32> = ["Wi-Fi", "USB", "Browser camera", "Another"].iter().map(|n| labelled(&ui, &format!("S: {n}"))[0].size().width).collect();
    assert!(tiles.iter().all(|t| *t > 20.0) && tiles.iter().sum::<f32>() + 18.0 <= 200.5, "{tiles:?}");
    // Beside a button whose word grows the row gives up room down to what its names need and no further
    let seg = |ui: &slint_interpreter::ComponentInstance| labelled(ui, "N: High quality")[0].size().width;
    let before = seg(&ui);
    set(&ui, "side", Value::String("Add another phone to this computer".into()));
    step(FRAME);
    let after = seg(&ui);
    assert!(after >= 60.0, "the picked name keeps its room: {after} (was {before})");
    if let Some(dir) = shots2() {
        save(&dir, "segmented.png", &offscreen::draw(ui.window(), 420.0, 120.0, 2.0).unwrap());
    }
}

const STATUS: &str = r#"
import { Status, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    in property <length> w: 300px;
    in property <string> detail;
    width: 400px;
    height: 60px;
    background: Theme.card;
    out property <bool> elided: s.elided;
    s := Status { x: 10px; y: 10px; width: root.w; text: "Waiting for the phone to say it is ready"; detail: root.detail; }
}
"#;

#[test]
fn a_status_says_its_whole_text_in_a_tooltip_only_when_the_text_is_cut() {
    let _still = offscreen::freeze_clock();
    let ui = make(STATUS, 400.0, 60.0);
    assert_eq!(ui.get_property("elided").unwrap(), Value::Bool(false), "room for all of it");
    set(&ui, "w", Value::Number(120.0));
    step(FRAME);
    assert_eq!(ui.get_property("elided").unwrap(), Value::Bool(true), "cut, so the tooltip carries the text");
    set(&ui, "w", Value::Number(380.0));
    step(FRAME);
    assert_eq!(ui.get_property("elided").unwrap(), Value::Bool(false));
}

const LINK: &str = r#"
import { LinkText, Status, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 200px;
    height: 60px;
    background: Theme.card;
    HorizontalLayout {
        x: 10px; y: 10px; height: Theme.row; width: 180px; spacing: 20px; alignment: start;
        Status { text: "HEH"; width: 40px; font-weight: 400; }
        VerticalLayout { alignment: center; width: 40px; LinkText { text: "HEH"; font-size: Theme.body; } }
    }
}
"#;

fn ink(pic: &offscreen::Picture, x0: usize, x1: usize) -> (usize, usize) {
    let w = pic.width as usize;
    let bg = [pic.rgba[0], pic.rgba[1], pic.rgba[2]];
    let (mut top, mut bottom) = (usize::MAX, 0);
    for y in 0..pic.height as usize {
        for x in x0..x1 {
            let p = &pic.rgba[(y * w + x) * 4..(y * w + x) * 4 + 3];
            if p.iter().zip(bg).any(|(a, b)| (*a as i32 - b as i32).abs() > 40) {
                top = top.min(y);
                bottom = bottom.max(y);
            }
        }
    }
    (top, bottom)
}

// The kit's own metric: a link's words sit on the same line as a Status's beside it
#[test]
fn link_text_sits_on_the_same_line_as_the_words_beside_it() {
    let _still = offscreen::freeze_clock();
    let ui = make(LINK, 200.0, 60.0);
    step(300);
    let pic = offscreen::draw(ui.window(), 200.0, 60.0, 2.0).unwrap();
    assert_eq!(ink(&pic, 20, 100), ink(&pic, 100 + 20, 100 + 100), "the link's capitals are where the status's are");
}

const ROWS: &str = r#"
import { SwitchRow, ChoiceField, Card, Icons, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 420px;
    height: 200px;
    background: Theme.card;
    in property <string> word: "Soon";
    out property <length> field-w: f.preferred-width;
    out property <length> control: Theme.control-w;
    VerticalLayout {
        x: 10px; y: 10px; width: 400px; spacing: 6px;
        SwitchRow { icon: Icons.bell; label: "Notify me"; word: root.word; }
        f := ChoiceField { compact: true; items: ["One", "Two"]; current: 0; name: "Pick"; }
        Card { title: "Colour"; icon: Icons.colour; has-reset: true; reset-lit: true; reset-label: "Reset"; reset-shortcut: "transforms.reset"; }
    }
}
"#;

#[test]
fn a_switch_row_has_a_quiet_word_a_picker_has_a_real_width_and_the_reset_button_takes_a_shortcut() {
    let _still = offscreen::freeze_clock();
    let ui = make(ROWS, 420.0, 200.0);
    assert_eq!(num(&ui, "field-w"), num(&ui, "control"), "a picker's preferred width is the settings token");
    assert!(num(&ui, "control") > 200.0);
    let word = labelled(&ui, "Soon");
    assert_eq!(word.len(), 1, "the word is there for a screen reader too");
    let row = labelled(&ui, "Notify me")[0].absolute_position();
    assert!(word[0].absolute_position().x > row.x + 100.0, "next to the switch, at the right");
    set(&ui, "word", Value::String("".into()));
    step(FRAME);
    assert!(labelled(&ui, "Soon").is_empty(), "no word, no room taken");
    if let Some(dir) = shots2() {
        save(&dir, "rows.png", &offscreen::draw(ui.window(), 420.0, 200.0, 2.0).unwrap());
    }
}

const LENSES: &str = r#"
import { LensRow, LensTile, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 400px;
    height: 90px;
    background: Theme.card;
    in property <[LensTile]> lenses;
    in property <int> current: 1;
    in-out property <int> picks;
    out property <bool> settled: r.settled;
    out property <[length]> xs: r.tile-x;
    out property <[length]> ws: r.tile-w;
    out property <length> worm-left: r.worm-left;
    out property <length> worm-right: r.worm-right;
    r := LensRow { x: 10px; y: 10px; width: 380px; label: "Lens"; lenses: root.lenses; current: root.current; picked(i) => { root.picks += 1; } }
}
"#;

fn tiles(n: usize) -> Value {
    let all = [("0.5", "Ultra"), ("0.6", "Wide"), ("1", "Main"), ("2", "Zoom"), ("5", "Tele")];
    Value::Model(slint::ModelRc::new(slint::VecModel::from(all[..n].iter().enumerate().map(|(i, (n, name))| Value::Struct(slint_interpreter::Struct::from_iter([
        ("id".to_string(), Value::String(i.to_string().into())), ("number".to_string(), Value::String((*n).into())),
        ("unit".to_string(), Value::String("x".into())), ("rest".to_string(), Value::String((*name).into())), ("name".to_string(), Value::String((*name).into())),
    ]))).collect::<Vec<_>>())))
}

fn lengths(ui: &slint_interpreter::ComponentInstance, name: &str) -> Vec<f32> {
    match ui.get_property(name).unwrap() {
        Value::Model(m) => (0..m.row_count()).map(|i| match m.row_data(i).unwrap() { Value::Number(n) => n as f32, v => panic!("{v:?}") }).collect(),
        v => panic!("{v:?}"),
    }
}

use slint::Model;

// Three lenses becoming two or five: every tile's width is its own motion, so the row reflows (the ones that stay slide, a new one grows, a lost one
// shrinks), never jumps, always adds up to the row, and the outline follows the picked tile the whole way
#[test]
fn the_lens_row_reflows_with_motion_when_the_count_changes_and_turns_round_mid_way() {
    let _still = offscreen::freeze_clock();
    let ui = make(LENSES, 400.0, 90.0);
    set(&ui, "lenses", tiles(3));
    step(600);
    assert_eq!(ui.get_property("settled").unwrap(), Value::Bool(true));
    let rest3 = lengths(&ui, "ws");
    assert!((rest3[0] - rest3[1]).abs() < 0.5 && rest3[3] < 0.5, "three equal tiles: {rest3:?}");
    let film = |ui: &slint_interpreter::ComponentInstance, frames: usize| -> Vec<(Vec<f32>, Vec<f32>)> {
        (0..frames).map(|_| { step(FRAME); (lengths(ui, "xs"), lengths(ui, "ws")) }).collect()
    };
    // 3 to 5
    set(&ui, "lenses", tiles(5));
    let grow = film(&ui, 30);
    for (xs, ws) in &grow {
        let used: f32 = ws.iter().sum::<f32>() + (0..4).map(|i| 6.0 * (ws[i] / rest3[0]).min(ws[i + 1] / rest3[0]).min(1.0)).sum::<f32>();
        assert!(used <= 380.5, "inside the row: {ws:?}");
        assert!(xs.windows(2).all(|p| p[1] >= p[0]), "in order: {xs:?}");
    }
    // The tile that comes grows over several frames, and nothing jumps more than a third of the row in one
    let new_tile: Vec<f32> = grow.iter().map(|(_, ws)| ws[4]).collect();
    assert!(new_tile[0] < 40.0 && new_tile.windows(2).all(|p| p[1] >= p[0] - 0.01) && new_tile[5] < *new_tile.last().unwrap(), "{new_tile:?}");
    assert!(new_tile.windows(2).all(|p| p[1] - p[0] < 50.0), "{new_tile:?}");
    step(600);
    assert_eq!(ui.get_property("settled").unwrap(), Value::Bool(true));
    let rest5 = lengths(&ui, "ws");
    assert!((rest5[0] - rest5[4]).abs() < 0.5 && (rest5[0] * 5.0 + 24.0 - 380.0).abs() < 1.5, "five equal tiles: {rest5:?}");
    // 5 to 2, changing its mind to 4 half way: the widths turn round from where they are
    set(&ui, "lenses", tiles(2));
    let down = film(&ui, 6);
    let mid = down.last().unwrap().1.clone();
    assert!(mid[4] < rest5[4] && mid[4] > 0.5, "the tile that goes is still going: {mid:?}");
    set(&ui, "lenses", tiles(4));
    let back = film(&ui, 3);
    assert!(back[0].1[4] <= mid[4] + 5.0, "no jump at the turn: {:?} after {mid:?}", back[0].1);
    step(600);
    let rest4 = lengths(&ui, "ws");
    assert!((rest4[0] - rest4[3]).abs() < 0.5 && rest4[4] < 0.5, "four tiles in the end: {rest4:?}");
    // The outline is on the picked tile once it is all over
    let (xs, ws) = (lengths(&ui, "xs"), lengths(&ui, "ws"));
    assert!((num(&ui, "worm-left") - xs[1]).abs() < 0.6 && (num(&ui, "worm-right") - (xs[1] + ws[1])).abs() < 0.6);
    // And with no lenses yet there are three dim tiles that take no pick
    set(&ui, "lenses", tiles(0));
    step(600);
    assert_eq!(lengths(&ui, "ws").iter().filter(|w| **w > 1.0).count(), 3);
    assert!(labelled(&ui, "Lens: Ultra").is_empty());
    if let Some(dir) = shots2() {
        set(&ui, "lenses", tiles(3));
        step(600);
        save(&dir, "lens-row.png", &offscreen::draw(ui.window(), 400.0, 90.0, 2.0).unwrap());
    }
}

const PEEK: &str = r#"
import { PeekLayer, Peeks, WindowBox, Theme } from "@ui-kit/ui-kit.slint";
export { Peeks, WindowBox }
export component T inherits Window {
    width: 300px;
    height: 200px;
    background: Theme.ground;
    out property <float> arrived: layer.arrived;
    out property <float> presence: layer.presence;
    layer := PeekLayer { }
}
"#;

#[test]
fn a_held_bubble_fades_to_another_drawing_instead_of_cutting_to_it() {
    let _still = offscreen::freeze_clock();
    let ui = make(PEEK, 300.0, 200.0);
    ui.set_global_property("WindowBox", "width", Value::Number(300.0)).unwrap();
    ui.set_global_property("WindowBox", "height", Value::Number(200.0)).unwrap();
    let show = |kind: &str| {
        ui.invoke_global("Peeks", "show", &[Value::Number(1.0), Value::String(kind.into()), Value::Model(slint::ModelRc::new(slint::VecModel::from(vec![Value::Number(0.5); 6]))), Value::Image(slint::Image::default()), Value::String("Aa".into()), Value::Brush(slint::Brush::SolidColor(slint::Color::from_rgb_u8(100, 200, 100))),
            Value::Number(100.0), Value::Number(150.0), Value::Number(60.0), Value::Number(20.0)]).unwrap();
    };
    show("chase");
    step(400);
    assert!(num(&ui, "presence") > 0.99 && num(&ui, "arrived") > 0.99);
    // The same bubble now draws something else: the new drawing comes up over frames
    show("text");
    let mut seen = Vec::new();
    for _ in 0..16 {
        step(FRAME);
        seen.push(num(&ui, "arrived"));
    }
    assert!(seen[0] < 0.6 && seen.windows(2).all(|p| p[1] >= p[0] - 1e-4) && *seen.last().unwrap() > 0.97, "fades in: {seen:?}");
}
