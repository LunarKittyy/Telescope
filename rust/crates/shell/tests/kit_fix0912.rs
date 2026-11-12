// Four kit rules from the owner's 12:26 feedback: a ProblemMark lines up with its row's control (circle and "!" both centred), an icon that is part of a row
// shows the row's own tooltip, the lens row reflows slowly enough to see, and a Smooth-like tile keeps its word at any width.
// FIX_SHOTS=dir also writes pictures.
mod motion_film;

use motion_film::*;
use slint::ComponentHandle;
use slint_interpreter::{ComponentInstance, Value};
use telescope_shell::offscreen::{self, Picture};

fn shots() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("FIX_SHOTS")?);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn px(p: &Picture, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * p.width + x) * 4) as usize;
    [p.rgba[i], p.rgba[i + 1], p.rgba[i + 2]]
}

fn reddish(c: [u8; 3]) -> bool {
    c[0] > 170 && c[1] < 110 && c[2] < 110
}

const MARK: &str = r#"
import { SwitchRow, ProblemMark, Icons, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 360px;
    height: 96px;
    background: Theme.card;
    SwitchRow { x: 10px; y: 8px; width: 340px; icon: Icons.mic; label: "Noise reduction"; hue: Theme.accent; problem: "RNNoise is missing."; problem-title: "Noise reduction unavailable"; }
    SwitchRow { x: 10px; y: 52px; width: 340px; icon: Icons.mic; label: "Plain"; hue: Theme.accent; }
}
"#;

// Measured on the picture at 1x, in physical pixels: the circle's rows against the switch's, and the "!" inside the circle
#[test]
fn the_problem_mark_is_centred_on_its_switch_and_its_glyph_on_its_circle() {
    let _still = offscreen::freeze_clock();
    let ui = make(MARK, 360.0, 96.0);
    step(FRAME);
    let pic = offscreen::draw(ui.window(), 360.0, 96.0, 1.0).unwrap();
    // The first row only (its bottom is at 8 + 32)
    let (w, h) = (pic.width, 44.min(pic.height));
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if reddish(px(&pic, x, y)) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    assert!(x1 > x0 && y1 > y0, "a red circle is drawn");
    let circle_centre = (y0 as f32 + (y1 + 1) as f32) / 2.0;
    // The switch is what is drawn to the right of the mark and differs from the card behind it
    let back = px(&pic, 5, 5);
    let (mut s0, mut s1) = (u32::MAX, 0);
    for y in 0..h {
        for x in x1 + 6..w - 6 {
            let c = px(&pic, x, y);
            if c.iter().zip(back.iter()).any(|(a, b)| a.abs_diff(*b) > 6) {
                (s0, s1) = (s0.min(y), s1.max(y));
            }
        }
    }
    let switch_centre = (s0 as f32 + (s1 + 1) as f32) / 2.0;
    assert!((circle_centre - switch_centre).abs() <= 0.5, "circle centre {circle_centre} (rows {y0}..={y1}) against the switch's {switch_centre} (rows {s0}..={s1})");
    // The "!" is the non-red ink in the circle's middle columns
    let mid = (x0 + x1) / 2;
    let (mut g0, mut g1) = (u32::MAX, 0);
    for y in y0 + 1..y1 {
        for x in mid.saturating_sub(1)..=mid {
            if !reddish(px(&pic, x, y)) {
                (g0, g1) = (g0.min(y), g1.max(y));
            }
        }
    }
    let above = g0 - y0;
    let below = y1 - g1;
    assert!(above == below, "{above} rows of red above the glyph and {below} below it");
    println!("problem mark: circle rows {y0}..={y1} centre {circle_centre}, switch centre {switch_centre}, glyph rows {g0}..={g1}, red above {above} below {below}");
    if let Some(dir) = shots() {
        save(&dir, "problem-mark-1x.png", &pic);
        let big = offscreen::draw(ui.window(), 360.0, 96.0, 4.0).unwrap();
        let crop = strip(&[big], (230.0, 6.0, 120.0, 40.0), 4.0, 1);
        save(&dir, "problem-mark-zoom.png", &crop);
    }
}

const ROWS: &str = r#"
import { RailRow, SwitchRow, Card, Icons, Theme, Tip, TipLayer, KeyFocus, Shortcuts, WindowBox, Tips, TipSpot, Switch } from "@ui-kit/ui-kit.slint";
export { Shortcuts, WindowBox, Tips, TipSpot, KeyFocus }
export component T inherits Window {
    width: 420px;
    height: 220px;
    background: Theme.card;
    // A row named by its text, with the tip handed to the row
    RailRow { x: 10px; y: 10px; width: 400px; height: 32px; icon: Icons.eye; label: "Preview"; named: true; tip: "Preview"; tip-detail: "The live picture in the middle of the window."; Switch { label: "Preview"; tip: "Preview"; tip-detail: "The live picture in the middle of the window."; } }
    // A bare icon row: its own tip is its word
    RailRow { x: 10px; y: 60px; width: 400px; height: 32px; icon: Icons.eye; label: "Eye"; tip: "Looking"; tip-detail: "What the icon stands for."; Rectangle { } }
    SwitchRow { x: 10px; y: 110px; width: 400px; icon: Icons.mic; label: "Mic"; tip: "Microphone"; tip-detail: "Takes sound from the phone."; }
    Card { x: 10px; y: 150px; width: 400px; title: "Titled"; icon: Icons.chip; tip: "Titled card"; fold-key: "t.card"; hue: Theme.accent; Rectangle { height: 20px; } }
    TipLayer { }
}
"#;

fn tip_state(ui: &ComponentInstance) -> (bool, String, String) {
    let get = |n: &str| match ui.get_global_property("Tips", n).unwrap() {
        Value::String(s) => s.to_string(),
        Value::Bool(b) => b.to_string(),
        v => panic!("{n}: {v:?}"),
    };
    (get("shown") == "true", get("says"), get("detail"))
}

fn hover_for_tip(ui: &ComponentInstance, x: f32, y: f32) -> (bool, String, String) {
    hover_pic(ui, x, y).0
}

fn hover_pic(ui: &ComponentInstance, x: f32, y: f32) -> ((bool, String, String), Picture) {
    pointer_to(ui, 410.0, 215.0);
    step(900);
    offscreen::draw(ui.window(), 420.0, 220.0, 1.0).unwrap();
    pointer_to(ui, x, y);
    step(900);
    let pic = offscreen::draw(ui.window(), 420.0, 220.0, 1.0).unwrap();
    (tip_state(ui), pic)
}

// The icon in front of a named row, and in a card's header, is part of the row: hovered it says what the row says, never a short name of its own
#[test]
fn an_icon_that_is_part_of_a_row_shows_the_rows_tooltip() {
    let _still = offscreen::freeze_clock();
    offscreen::install();
    let def = telescope_shell::compile::compile("t.slint", ROWS, Vec::new(), "T").unwrap_or_else(|e| panic!("{e}"));
    let ui = def.create().unwrap();
    ui.set_global_property("WindowBox", "width", Value::Number(420.0)).unwrap();
    ui.set_global_property("WindowBox", "height", Value::Number(220.0)).unwrap();
    ui.set_global_callback("Shortcuts", "bindings-for", |_| Value::Model(slint::ModelRc::new(slint::VecModel::from(Vec::<Value>::new())))).unwrap();
    ui.window().set_size(slint::LogicalSize::new(420.0, 220.0));
    ui.show().unwrap();
    offscreen::pump();
    // The named row's icon and its switch carry the same tip
    let (on_icon, on_icon_pic) = hover_pic(&ui, 17.0, 26.0);
    assert_eq!((on_icon.0, on_icon.1.as_str(), on_icon.2.as_str()), (true, "Preview", "The live picture in the middle of the window."), "the icon of a named row: {on_icon:?}");
    if let Some(dir) = shots() {
        save(&dir, "tooltip-on-icon.png", &on_icon_pic);
    }
    let on_switch = hover_for_tip(&ui, 395.0, 26.0);
    assert_eq!((on_switch.1.as_str(), on_switch.2.as_str()), (on_icon.1.as_str(), on_icon.2.as_str()), "the control of the row says the same");
    // A switch row: the icon, as the switch
    let mic_icon = hover_for_tip(&ui, 17.0, 126.0);
    assert_eq!((mic_icon.0, mic_icon.1.as_str(), mic_icon.2.as_str()), (true, "Microphone", "Takes sound from the phone."), "{mic_icon:?}");
    // A bare icon row keeps its own word's tip: the icon is all there is to read
    let bare = hover_for_tip(&ui, 17.0, 76.0);
    assert_eq!((bare.0, bare.1.as_str()), (true, "Looking"), "{bare:?}");
    // A card header's icon says what its title says
    let header_icon = hover_for_tip(&ui, 28.0, 172.0);
    let header_title = hover_for_tip(&ui, 70.0, 172.0);
    assert_eq!((header_icon.0, header_icon.1.as_str()), (true, "Titled"), "{header_icon:?}");
    assert_eq!((header_icon.1.as_str(), header_icon.2.as_str()), (header_title.1.as_str(), header_title.2.as_str()), "icon and title of a card say the same");
}

const LENSES: &str = r#"
import { LensRow, LensTile, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 400px;
    height: 90px;
    background: Theme.card;
    in property <[LensTile]> lenses;
    out property <[length]> ws: r.tile-w;
    out property <bool> settled: r.settled;
    r := LensRow { x: 10px; y: 10px; width: 380px; label: "Lens"; lenses: root.lenses; current: 0; }
}
"#;

fn tiles(n: usize) -> Value {
    let all = [("0.5", "Ultra"), ("0.6", "Wide"), ("1", "Main"), ("2", "Zoom"), ("5", "Tele")];
    Value::Model(slint::ModelRc::new(slint::VecModel::from(all[..n].iter().enumerate().map(|(i, (n, name))| Value::Struct(slint_interpreter::Struct::from_iter([
        ("id".to_string(), Value::String(i.to_string().into())), ("number".to_string(), Value::String((*n).into())),
        ("unit".to_string(), Value::String("x".into())), ("rest".to_string(), Value::String((*name).into())), ("name".to_string(), Value::String((*name).into())),
    ]))).collect::<Vec<_>>())))
}

fn widths(ui: &ComponentInstance) -> Vec<f32> {
    // Read as the window is drawn: that is what asks for the next frame of a move
    offscreen::draw(ui.window(), 400.0, 90.0, 1.0).unwrap();
    match ui.get_property("ws").unwrap() {
        Value::Model(m) => {
            use slint::Model;
            (0..m.row_count()).map(|i| match m.row_data(i).unwrap() { Value::Number(n) => n as f32, v => panic!("{v:?}") }).collect()
        }
        v => panic!("{v:?}"),
    }
}

// How long a tile takes to get most of the way: the lens row is a motion the eye can follow, both ways
#[test]
fn the_lens_row_takes_a_visible_time_to_slide_in_and_out_both_ways() {
    let _still = offscreen::freeze_clock();
    let ui = make(LENSES, 400.0, 90.0);
    set(&ui, "lenses", tiles(5));
    step(1000);
    let rest5 = widths(&ui)[4];
    set(&ui, "lenses", tiles(3));
    step(1000);
    set(&ui, "lenses", tiles(5));
    let mut open_frames = 0;
    while widths(&ui)[4] < rest5 * 0.9 && open_frames < 200 {
        step(FRAME);
        open_frames += 1;
    }
    // And back to three: how long the fifth takes to be down to a tenth
    step(1000);
    set(&ui, "lenses", tiles(3));
    let mut close_frames = 0;
    while widths(&ui)[4] > rest5 * 0.1 && close_frames < 200 {
        step(FRAME);
        close_frames += 1;
    }
    println!("lens row: nine tenths open after {} ms, a tenth left after {} ms", open_frames as u64 * FRAME, close_frames as u64 * FRAME);
    assert!(open_frames as u64 * FRAME >= 200, "opening is slow enough to see: {} ms", open_frames as u64 * FRAME);
    assert!(close_frames as u64 * FRAME >= 200, "closing is slow enough to see: {} ms", close_frames as u64 * FRAME);
    // Stopping the stream: the lenses go (none leaves three dim tiles) and the extra tiles slide out rather than cut
    step(1000);
    set(&ui, "lenses", tiles(5));
    step(1000);
    set(&ui, "lenses", tiles(0));
    step(FRAME);
    let first = widths(&ui);
    assert!(first[3] > 1.0 && first[4] > 1.0, "the tiles that go are still there a frame later, going: {first:?}");
    step(1000);
    let last = widths(&ui);
    assert!(last[3] < 0.5 && last[4] < 0.5 && last[2] > 10.0, "three dim tiles in the end: {last:?}");
}

const TILE: &str = r#"
import { ToggleTile, IconButton, Icons, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    in property <length> w: 340px;
    in property <bool> on;
    out property <length> smooth-w: sm.width;
    out property <length> smooth-right: sm.absolute-position.x + sm.width;
    width: 420px;
    height: 56px;
    background: Theme.card;
    HorizontalLayout {
        x: 10px; y: 10px; width: root.w; height: 36px; spacing: 6px;
        IconButton { icon: Icons.eye; label: "Hide"; }
        ToggleTile { width: 36px; height: 36px; horizontal-stretch: 0; icon: Icons.camera; name: "Camera"; }
        Rectangle { horizontal-stretch: 1; }
        ToggleTile { width: 36px; height: 36px; horizontal-stretch: 0; icon: Icons.grid; name: "Outlines"; }
        sm := ToggleTile { height: 36px; natural: true; icon: Icons.film; text: "Smooth"; checked: root.on; hue: #e5a158; }
        IconButton { icon: Icons.pop-out; label: "Pop out"; }
    }
}
"#;

// A tile with `natural` is as wide as its icon and whole word, however narrow the row: the row gives way elsewhere
#[test]
fn a_natural_tile_keeps_its_whole_word_when_the_row_narrows() {
    let _still = offscreen::freeze_clock();
    let ui = make(TILE, 420.0, 56.0);
    let mut seen = Vec::new();
    for w in [400.0, 340.0, 300.0] {
        set(&ui, "w", Value::Number(w as f64));
        step(FRAME);
        let (tile_w, right) = (num(&ui, "smooth-w"), num(&ui, "smooth-right"));
        seen.push((w, tile_w));
        assert!(tile_w >= 80.0, "{w}: the label has its room: {tile_w}");
        assert!(right <= 10.0 + w + 0.5, "{w}: inside the row (right edge {right})");
    }
    assert!(seen.windows(2).all(|p| (p[0].1 - p[1].1).abs() < 0.5), "its width does not follow the row: {seen:?}");
    if let Some(dir) = shots() {
        for (name, on) in [("off", false), ("on", true)] {
            set(&ui, "w", Value::Number(400.0));
            set(&ui, "on", Value::Bool(on));
            step(600);
            save(&dir, &format!("smooth-tile-{name}.png"), &offscreen::draw(ui.window(), 420.0, 56.0, 2.0).unwrap());
        }
    }
}

const STAR: &str = r#"
import { PinStar, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    in property <bool> row-on;
    in property <bool> pinned;
    out property <bool> wanted: s.wanted;
    width: 60px;
    height: 40px;
    background: Theme.card;
    s := PinStar { x: 10px; y: 10px; reveal-on: true; row-hover: root.row-on; pinned: root.pinned; label: "Star"; }
}
"#;

// An unpinned star of a quiet list is out of sight until its row is hovered; a pinned one is always there
#[test]
fn an_unpinned_star_shows_with_its_row_and_a_pinned_one_always() {
    let _still = offscreen::freeze_clock();
    let ui = make(STAR, 60.0, 40.0);
    let wanted = |ui: &ComponentInstance| matches!(ui.get_property("wanted").unwrap(), Value::Bool(true));
    assert!(!wanted(&ui), "hidden at rest");
    set(&ui, "row-on", Value::Bool(true));
    assert!(wanted(&ui), "shown with its row hovered");
    set(&ui, "row-on", Value::Bool(false));
    assert!(!wanted(&ui));
    set(&ui, "pinned", Value::Bool(true));
    assert!(wanted(&ui), "a pinned one always shows");
}

const BAR: &str = r#"
import { ColumnScrollbar, Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 140px;
    height: 220px;
    background: Theme.card;
    in-out property <length> offset;
    ColumnScrollbar { x: 100px; y: 10px; before: 8px; after: 4px; height: 200px; content: 800px; viewport: 200px; offset <=> root.offset; }
}
"#;

// The bar's box is the whole gutter: a press beside the drawn handle, in the room before or after it, takes hold and a drag scrolls the content
#[test]
fn a_press_in_the_gutter_beside_the_handle_drags_the_content() {
    let _still = offscreen::freeze_clock();
    for (name, x) in [("before", 102.0), ("after", 100.0 + 8.0 + 11.0 + 2.0)] {
        let ui = make(BAR, 140.0, 220.0);
        step(FRAME);
        // The handle is at the top: its first rows
        press(&ui, x, 20.0);
        for i in 1..=4 {
            pointer_to(&ui, x, 20.0 + 10.0 * i as f32);
        }
        release(&ui, x, 60.0);
        // 40 px down the track is 40 * (600 / 150) of the content
        let offset = num(&ui, "offset");
        assert!((offset - 160.0).abs() < 4.0, "{name}: dragged 40 px, the content went {offset}");
    }
    // The drawn handle is only the 11 px: the gutter itself is bare
    let ui = make(BAR, 140.0, 220.0);
    step(FRAME);
    let pic = offscreen::draw(ui.window(), 140.0, 220.0, 1.0).unwrap();
    let back = px(&pic, 5, 5);
    assert_eq!(px(&pic, 102, 30), back, "no handle in the gutter before it");
    assert_ne!(px(&pic, 115, 30), back, "the handle is drawn");
    assert_eq!(px(&pic, 125, 30), back, "no handle in the gutter after it");
}
