//! Writes the main window as one .slint document at startup: the frame from ui/frame.slint with every plugin's card
//! and header widget placed in it, then compiles it with the interpreter (decided in rust/ARCHITECTURE.md).
//!
//! Cards sit in one clipped body and each card works out its own place from the window's mode, so a card never moves
//! to another parent when the window crosses 1300 or 900 px. Each column scrolls on its own like Qt's three scroll
//! areas. Every property and callback a card declares is aliased on the window as p<n>-<name>, which is how
//! set_ui_props and UiEvents reach it by name.

use telescope_core::i18n::Fields;
use crate::layout::{Mode, Panels, Region, WIDTH_THREE_COL, WIDTH_TWO_COL};
use crate::source::{quote, Interface};

/// Spacing between cards in a column and between columns (app.py _build_body).
const GAP: f32 = 14.0;
/// The column's right inset, so its scrollbar doesn't cover a card's border.
const INSET: f32 = 6.0;
const BAR: f32 = 11.0;

/// One plugin's piece of UI as the window holds it.
#[derive(Clone, Debug)]
pub struct Piece {
    /// The element id in the document: p0, p1… for cards, h0… for header widgets.
    pub id: String,
    /// The file the generated document imports it from (a path the file loader knows).
    pub file: String,
    pub component: String,
    pub iface: Interface,
}

#[derive(Clone, Debug)]
pub struct CardPlace {
    pub piece: Piece,
    pub region: Region,
    /// "<plugin>/<title in English>" (collapse.py's key).
    pub fold_key: String,
    /// The plugin group, for its colour.
    pub category: String,
}

#[derive(Clone, Debug)]
pub struct HeaderPlace {
    pub piece: Piece,
    pub right: bool,
}

/// The window's document. cards are in registration order; panels says where each one shows.
pub fn window(cards: &[CardPlace], panels: &Panels, headers: &[HeaderPlace]) -> String {
    let mut out = String::new();
    out.push_str("import { Theme, Icons, Folds, Reveal, Shortcuts, TextCap, WindowBox, ColumnScrollbar, KeyFocus, Touched } from \"@ui-kit/ui-kit.slint\";\n");
    out.push_str("import { Footer, BannerArea, StreamTiles, HeaderIconButton, StartButton, BannerData, TileData } from \"shell/frame.slint\";\n");
    let mut imported = Vec::new();
    for piece in cards.iter().map(|c| &c.piece).chain(headers.iter().map(|h| &h.piece)) {
        let line = format!("import {{ {} as {}C }} from {};\n", piece.component, piece.id, quote(&piece.file));
        if !imported.contains(&line) {
            imported.push(line.clone());
            out.push_str(&line);
        }
    }
    out.push_str("export { Folds, Theme, Reveal, Shortcuts, TextCap, WindowBox, KeyFocus, Touched }\n");
    out.push_str(&FRAME_TOP.field("three", WIDTH_THREE_COL).field("two", WIDTH_TWO_COL));

    // Column geometry for each mode, relative to the body's left edge
    let cols = [Mode::Three, Mode::Two, Mode::One].map(|m| panels.columns(m));
    for c in 0..3 {
        let x = mode_expr(|m| col_x(&cols[m], c));
        let w = mode_expr(|m| col_w(&cols[m], c));
        out.push_str(&format!("    out property <length> c{c}x: {x};\n    out property <length> c{c}w: {w};\n"));
        out.push_str(&format!("    property <length> card{c}w: c{c}w - {INSET}px - (bar{c} ? {BAR}px : 0px);\n"));
    }
    // Preferred content height and stretch room of each column in the current mode
    #[allow(clippy::needless_range_loop)] // c names the column in the generated text as well
    for c in 0..3 {
        let pref = mode_expr(|m| sum_heights(cards, &cols[m][c].0));
        let centers = mode_expr(|m| {
            let n: Vec<String> = cols[m][c].0.iter().filter(|&&i| cards[i].region == Region::Center)
                .map(|&i| format!("({}.visible ? 1 : 0)", cards[i].piece.id)).collect();
            if n.is_empty() { "0".into() } else { n.join(" + ") }
        });
        let hint = mode_expr(|m| sum_hints(cards, &cols[m][c].0));
        out.push_str(&format!("    property <length> col{c}-pref: {pref};\n"));
        out.push_str(&format!("    property <int> col{c}-centers: {centers};\n"));
        out.push_str(&format!(
            "    property <length> col{c}-full: col{c}-centers > 0 ? max(col{c}-pref, {hint}) : col{c}-pref;\n"));
        out.push_str(&format!(
            "    property <length> extra{c}: col{c}-centers > 0 ? max(0px, max(view-h, col{c}-full) - col{c}-pref) / col{c}-centers : 0px;\n"));
        out.push_str(&format!("    property <length> s{c}: clamp(scroll{c}, 0px, max(0px, col{c}-full - view-h));\n"));
        out.push_str(&format!("    changed col{c}-full => {{ check-bars(); }}\n"));
    }
    out.push_str("    changed view-h => { check-bars(); }\n    property <bool> bars-checked;\n    Timer { interval: 1ms; running: !root.bars-checked; triggered => { root.check-bars(); root.bars-checked = true; } }\n");
    out.push_str("    public function check-bars() {\n");
    for c in 0..3 {
        out.push_str(&format!("        bar{c} = col{c}-full > view-h;\n"));
    }
    out.push_str("    }\n");

    // Aliases the shell reaches each piece by
    for piece in cards.iter().map(|c| &c.piece).chain(headers.iter().map(|h| &h.piece)) {
        out.push_str(&aliases(piece));
    }

    // The header
    let header = |right: bool| -> String {
        headers.iter().filter(|h| h.right == right)
            .map(|h| format!("                {} := {}C {{ y: (parent.height - self.height) / 2; }}\n", h.piece.id, h.piece.id))
            .collect()
    };
    out.push_str(&HEADER.field("left", header(false)).field("right", header(true)));

    // The body: cards placed by their column in the current mode
    out.push_str(BODY_TOP);
    // In the three-column order, which is the order Tab takes (Qt goes by layout order, column by column)
    let mut order: Vec<usize> = cols[0].iter().flat_map(|(list, _)| list.iter().copied()).collect();
    let rest: Vec<usize> = (0..cards.len()).filter(|i| !order.contains(i)).collect();
    order.extend(rest);
    for i in order {
        out.push_str(&card_element(i, &cards[i], cards, &cols));
    }
    out.push_str(BODY_BOTTOM);
    out
}

/// A value that differs per mode, as one expression: [three, two, one].
fn mode_expr(f: impl Fn(usize) -> String) -> String {
    let (a, b, c) = (f(0), f(1), f(2));
    if a == b && b == c { a } else { format!("(root.mode == 0 ? {a} : root.mode == 1 ? {b} : {c})") }
}

/// Which columns show and their widths in a mode: Some(px) fixed, None takes the rest.
fn shown(cols: &[(Vec<usize>, Option<f32>); 3]) -> Vec<(usize, Option<f32>)> {
    (0..3).filter(|&c| !cols[c].0.is_empty()).map(|c| (c, cols[c].1)).collect()
}

fn width_of(w: Option<f32>, shown: &[(usize, Option<f32>)]) -> String {
    match w {
        Some(px) => format!("{px}px"),
        None => {
            let fixed: f32 = shown.iter().filter_map(|(_, w)| *w).sum();
            let gaps = GAP * (shown.len().saturating_sub(1)) as f32;
            let stretchy = shown.iter().filter(|(_, w)| w.is_none()).count().max(1) as f32;
            format!("max(0px, (body-w - {}px) / {stretchy})", fixed + gaps)
        }
    }
}

fn col_x(cols: &[(Vec<usize>, Option<f32>); 3], c: usize) -> String {
    let shown = shown(cols);
    let mut parts = vec!["0px".to_string()];
    for (sc, w) in &shown {
        if *sc == c {
            return parts.join(" + ");
        }
        parts.push(format!("{} + {GAP}px", width_of(*w, &shown)));
    }
    "0px".into()
}

fn col_w(cols: &[(Vec<usize>, Option<f32>); 3], c: usize) -> String {
    let shown = shown(cols);
    shown.iter().find(|(sc, _)| *sc == c).map_or("0px".into(), |(_, w)| width_of(*w, &shown))
}

fn sum_heights(cards: &[CardPlace], list: &[usize]) -> String {
    if list.is_empty() {
        return "0px".into();
    }
    let parts: Vec<String> = list.iter().map(|&i| format!("({p}.visible ? {p}.preferred-height + {GAP}px : 0px)", p = cards[i].piece.id)).collect();
    format!("max(0px, {} - {GAP}px)", parts.join(" + "))
}

/// A column's height from its cards' size hints: a QVBoxLayout column adds them up, then the stage card (the only one that
/// stretches) takes what that leaves over. A card's hint is its height plus its out property hint-extra, if it has one.
fn sum_hints(cards: &[CardPlace], list: &[usize]) -> String {
    if list.is_empty() {
        return "0px".into();
    }
    let parts: Vec<String> = list.iter().map(|&i| {
        let p = &cards[i].piece;
        let more = if p.iface.outs.iter().any(|o| o == "hint-extra") { format!(" + {}.hint-extra", p.id) } else { String::new() };
        format!("({id}.visible ? {id}.preferred-height{more} + {GAP}px : 0px)", id = p.id)
    }).collect();
    format!("max(0px, {} - {GAP}px)", parts.join(" + "))
}

fn aliases(piece: &Piece) -> String {
    let mut out = format!("    in-out property {id}-visible <=> {id}.visible;\n", id = piece.id);
    for p in &piece.iface.props {
        out.push_str(&format!("    in-out property {}-{p} <=> {}.{p};\n", piece.id, piece.id));
    }
    for p in &piece.iface.outs {
        out.push_str(&format!("    out property {}-{p} <=> {}.{p};\n", piece.id, piece.id));
    }
    for c in &piece.iface.callbacks {
        out.push_str(&format!("    callback {}-{c} <=> {}.{c};\n", piece.id, piece.id));
    }
    out
}

/// Where card i sits in each mode: its column and the card above it there.
fn card_element(i: usize, card: &CardPlace, cards: &[CardPlace], cols: &[[(Vec<usize>, Option<f32>); 3]; 3]) -> String {
    let id = &card.piece.id;
    let place = |m: usize| -> (usize, Option<usize>) {
        for (c, (list, _)) in cols[m].iter().enumerate() {
            if let Some(at) = list.iter().position(|&k| k == i) {
                return (c, at.checked_sub(1).map(|a| list[a]));
            }
        }
        (0, None)
    };
    let x = mode_expr(|m| format!("root.c{}x", place(m).0));
    let w = mode_expr(|m| format!("root.card{}w", place(m).0));
    let y = mode_expr(|m| match place(m) {
        (c, None) => format!("(-root.s{c})"),
        (_, Some(prev)) => {
            let p = &cards[prev].piece.id;
            format!("({p}.y + ({p}.visible ? {p}.height + {GAP}px : 0px))")
        }
    });
    let extra = if card.region == Region::Center { mode_expr(|m| format!("root.extra{}", place(m).0)) } else { "0px".into() };
    format!(
        "            {id} := {id}C {{\n                x: {x};\n                y: {y};\n                width: {w};\n                height: self.preferred-height + {extra};\n                fold-key: root.card-folds ? {key} : \"\";\n                hue: Theme.plugin-hue({cat});\n            }}\n",
        key = quote(&card.fold_key),
        cat = quote(&card.category),
    )
}

const FRAME_TOP: &str = r#"
struct AddChoice {
    id: string,
    name: string,
}

export component TelescopeWindow inherits Window {
    in property <string> window-title: "Telescope";
    in property <string> start-text;
    // Translators' short form of the start button, for a narrow window
    in property <string> start-short;
    in property <bool> streaming;
    in property <bool> start-enabled: true;
    in property <bool> add-visible;
    in property <bool> add-enabled: true;
    in property <[AddChoice]> add-choices;
    in property <[BannerData]> banners;
    in property <[TileData]> tiles;
    in property <string> status;
    in property <string> status-kind: "status_dim";
    in property <string> fps: "-";
    in property <string> fps-tone;
    property <color> fps-color: fps-tone == "warn" ? Theme.warn : fps-tone == "err" ? Theme.err : Theme.text;
    in property <string> fps-tip;
    in property <string> net: "-";
    in property <string> net-tone;
    property <color> net-color: net-tone == "warn" ? Theme.warn : Theme.text;
    callback start-clicked;
    callback settings-clicked;
    callback add-picked(string);
    callback banner-action(string, int);
    callback banner-dismissed(string);
    callback tile-focus(string);
    callback tile-stop(string);
    // A key pressed (or let go) anywhere in the window, before the focused control: true takes it (Plugin::on_key)
    callback key(KeyEvent, bool, bool) -> bool;
    // Host::copy_text, through a hidden TextInput's copy()
    public function copy-text(text: string) {
        clipboard.text = text;
        clipboard.select-all();
        clipboard.copy();
    }
    // Host::clipboard_text: what the hidden box pastes from the clipboard
    public function paste-text() -> string {
        clipboard.text = "";
        clipboard.paste();
        return clipboard.text;
    }
    out property <int> mode: root.width >= {three}px ? 0 : root.width >= {two}px ? 1 : 2;
    in-out property <length> scroll0;
    in-out property <length> scroll1;
    in-out property <length> scroll2;
    in-out property <bool> bar0;
    in-out property <bool> bar1;
    in-out property <bool> bar2;
    property <length> body-top: 60px + banner-area.preferred-height + tiles-row.preferred-height;
    property <length> view-h: max(0px, root.height - 40px - body-top - 32px);
    property <length> body-w: max(0px, root.width - 32px);
    out property <length> content-height: max(col0-full, max(col1-full, col2-full));
    // False for a card shown on its own (the card scenes), which doesn't fold
    in property <bool> card-folds: true;
    out property <length> view-top: body-top + 16px;
    out property <length> view-height: view-h;

    init => {
        WindowBox.width = self.width;
        WindowBox.height = self.height;
    }
    changed width => { WindowBox.width = self.width; }
    changed height => { WindowBox.height = self.height; }
    // Where the focus goes when a text box lets go of it: the scope all the window's keys pass through
    public function focus-home() { keys.focus(); }
    property <int> given-back: KeyFocus.given-back;
    changed given-back => { keys.focus(); }

    title: window-title;
    in property <image> app-icon;
    icon: app-icon;
    preferred-width: 1380px;
    preferred-height: 900px;
    min-width: 560px;
    min-height: 520px;
    background: Theme.bg;
    default-font-family: Theme.font;
    default-font-size: Theme.body;
    clipboard := TextInput { visible: false; width: 0px; height: 0px; }
"#;

const HEADER: &str = r#"
    add-menu := ContextMenuArea {
        x: 0;
        y: 0;
        width: 0;
        height: 0;
        Menu {
            for choice in root.add-choices: MenuItem {
                title: choice.name;
                activated => { root.add-picked(choice.id); }
            }
        }
    }
    keys := FocusScope {
        x: 0;
        y: 0;
        width: root.width;
        height: root.height;
        focus-on-click: false;
        focus-on-tab-navigation: false;
        init => { self.focus(); }
        capture-key-pressed(e) => { root.key(e, false, TextInputInterface.text-input-focused) ? accept : reject }
        capture-key-released(e) => { root.key(e, true, TextInputInterface.text-input-focused) ? accept : reject }
    Rectangle {
        x: 0;
        y: 0;
        width: root.width;
        height: 60px;
        background: Theme.chrome;
        Rectangle { y: parent.height - 1px; height: 1px; background: Theme.border; }
        HorizontalLayout {
            padding-left: 16px;
            padding-right: 16px;
            padding-top: 10px;
            padding-bottom: 10px;
            spacing: 12px;
            HorizontalLayout {
                // Header widgets keep their own width, like Qt's header row; the gap takes the rest
                horizontal-stretch: 0;
                spacing: 14px;
{left}            }
            Rectangle { horizontal-stretch: 1; }
            HorizontalLayout {
                horizontal-stretch: 0;
                spacing: 8px;
{right}            }
            HeaderIconButton {
                y: (parent.height - self.height) / 2;
                icon: Icons.v-gear;
                icon-size: 19px;
                label: @tr("Settings");
                tip: @tr("Settings");
                clicked => { root.settings-clicked(); }
            }
            if add-visible: HeaderIconButton {
                y: (parent.height - self.height) / 2;
                icon: Icons.v-plus;
                label: @tr("Add camera");
                tip: @tr("Stream another camera at the same time, to a virtual camera of its own");
                enabled: add-enabled;
                clicked => { add-menu.show({ x: self.absolute-position.x, y: self.absolute-position.y + self.height + 6px }); }
            }
            StartButton {
                y: (parent.height - self.height) / 2;
                text: root.mode == 2 ? start-short : start-text;
                streaming: root.streaming;
                shortcut: "streaming.start_stop";
                enabled: start-enabled;
                clicked => { root.start-clicked(); }
            }
        }
    }
    banner-area := BannerArea {
        x: 0;
        y: 60px;
        width: root.width;
        height: self.preferred-height;
        banners: root.banners;
        action(key, i) => { root.banner-action(key, i); }
        dismissed(key) => { root.banner-dismissed(key); }
    }
    tiles-row := StreamTiles {
        x: 0;
        y: 60px + banner-area.preferred-height;
        width: root.width;
        height: self.preferred-height;
        tiles: root.tiles;
        focus-requested(id) => { root.tile-focus(id); }
        stop-requested(id) => { root.tile-stop(id); }
    }
"#;

// A Flickable and not a clipping Rectangle: Slint's Tab skips whatever a plain clip hides, so the cycle would be only the
// controls in view and change with the scroll. A Flickable's clip still lets the focus in (it is told to scroll the
// column through Reveal). It never moves itself: the viewport is as big as the view and the columns scroll by `scroll{c}`.
const BODY_TOP: &str = r#"
    Flickable {
        x: 16px;
        y: body-top + 16px;
        width: body-w;
        height: view-h;
        interactive: false;
        viewport-width: self.width;
        viewport-height: self.height;
        TouchArea {
            scroll-event(e) => {
                if (self.mouse-x >= root.c2x && root.c2w > 0) {
                    root.scroll2 = clamp(root.s2 - e.delta-y, 0px, max(0px, root.col2-full - root.view-h));
                } else if (self.mouse-x >= root.c1x && root.c1w > 0) {
                    root.scroll1 = clamp(root.s1 - e.delta-y, 0px, max(0px, root.col1-full - root.view-h));
                } else {
                    root.scroll0 = clamp(root.s0 - e.delta-y, 0px, max(0px, root.col0-full - root.view-h));
                }
                accept
            }
"#;

const BODY_BOTTOM: &str = r#"        }
        ColumnScrollbar { x: root.c0x + root.c0w - 11px; height: root.view-h; visible: root.bar0 && root.c0w > 0; content: root.col0-full; viewport: root.view-h; offset <=> root.scroll0; }
        ColumnScrollbar { x: root.c1x + root.c1w - 11px; height: root.view-h; visible: root.bar1 && root.c1w > 0; content: root.col1-full; viewport: root.view-h; offset <=> root.scroll1; }
        ColumnScrollbar { x: root.c2x + root.c2w - 11px; height: root.view-h; visible: root.bar2 && root.c2w > 0; content: root.col2-full; viewport: root.view-h; offset <=> root.scroll2; }
    }
    Footer {
        x: 0;
        y: root.height - 40px;
        width: root.width;
        status: root.status;
        status-kind: root.status-kind;
        fps: root.fps;
        fps-color: root.fps-color;
        fps-tip: root.fps-tip;
        net: root.net;
        net-color: root.net-color;
    }
    }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::scan;
    use slint_interpreter::{ComponentHandle, Value};

    const STAGE: &str = r#"
import { Card } from "@ui-kit/ui-kit.slint";
export component Stage inherits Card {
    in property <string> words;
    callback poked(int);
    title: "Stage";
    Text { text: words; height: 50px; }
}
"#;

    const HINTED: &str = r#"
import { Card } from "@ui-kit/ui-kit.slint";
export component Hinted inherits Card {
    out property <length> hint-extra: 16px;
    title: "Hinted";
    Text { text: "x"; height: 50px; }
}
"#;

    #[test]
    fn every_card_takes_its_plugins_group_colour() {
        let (piece, _) = piece("p0", "plugins/one/card.slint", "Stage", STAGE);
        let cards = vec![CardPlace { piece, region: Region::Right, fold_key: "one/Stage".into(), category: "sound".into() }];
        let doc = window(&cards, &Panels::default(), &[]);
        assert!(doc.contains("hue: Theme.plugin-hue(\"sound\");"), "{doc}");
    }

    #[test]
    fn a_stage_card_takes_what_its_size_hint_adds_and_a_rails_card_doesnt() {
        i_slint_backend_testing::init_no_event_loop();
        let height = |region: Region| {
            let (p0, f0) = piece("p0", "plugins/one/card.slint", "Hinted", HINTED);
            let cards = vec![CardPlace { piece: p0, region, fold_key: "one/Hinted".into(), category: "preview".into() }];
            let mut panels = Panels::default();
            panels.place(region, 0, false, "preview");
            let text = window(&cards, &panels, &[]);
            let def = crate::compile::compile("window.slint", &text, vec![f0], "TelescopeWindow").unwrap_or_else(|e| panic!("{e}\n{text}"));
            let win = def.create().unwrap();
            win.set_global_callback("Folds", "is-folded", |_| Value::Bool(false)).unwrap();
            win.window().set_size(slint::LogicalSize::new(1300.0, 40.0)); // shorter than the card: nothing to fill
            match win.get_property("content-height").unwrap() { Value::Number(n) => n, v => panic!("{v:?}") }
        };
        assert_eq!(height(Region::Center) - height(Region::Right), 16.0);
    }

    fn piece(id: &str, file: &str, component: &str, text: &str) -> (Piece, (String, String)) {
        (Piece { id: id.into(), file: file.into(), component: component.into(), iface: scan(text, component) }, (file.into(), text.into()))
    }

    #[test]
    fn the_window_changes_mode_at_the_widths_the_layout_names() {
        i_slint_backend_testing::init_no_event_loop();
        let text = window(&[], &Panels::default(), &[]);
        let def = crate::compile::compile("window.slint", &text, vec![], "TelescopeWindow").unwrap_or_else(|e| panic!("{e}\n{text}"));
        let win = def.create().unwrap();
        win.set_global_callback("Folds", "is-folded", |_| Value::Bool(false)).unwrap();
        let mode = |width: f32| {
            win.window().set_size(slint::LogicalSize::new(width, 800.0));
            win.get_property("mode").unwrap()
        };
        let (three, two) = (WIDTH_THREE_COL, WIDTH_TWO_COL);
        assert_eq!((three, two), (1300.0, 900.0));
        for (width, want) in [(1400.0, 0.0), (three, 0.0), (three - 1.0, 1.0), (two, 1.0), (two - 1.0, 2.0), (600.0, 2.0)] {
            assert_eq!(mode(width), Value::Number(want), "at {width}");
        }
    }

    #[test]
    fn a_narrow_window_stacks_every_card_in_one_column_the_stage_first() {
        i_slint_backend_testing::init_no_event_loop();
        let (p0, f0) = piece("p0", "plugins/a/card.slint", "Stage", STAGE);
        let (p1, f1) = piece("p1", "plugins/b/card.slint", "Stage", STAGE);
        let (p2, f2) = piece("p2", "plugins/c/card.slint", "Stage", STAGE);
        let place = |piece, region, name: &str| CardPlace { piece, region, fold_key: format!("{name}/Stage"), category: "preview".into() };
        let cards = vec![place(p0, Region::Left, "a"), place(p1, Region::Center, "b"), place(p2, Region::Right, "c")];
        let mut panels = Panels::default();
        panels.place(Region::Left, 0, false, "preview");
        panels.place(Region::Center, 1, false, "preview");
        panels.place(Region::Right, 2, false, "preview");
        let text = window(&cards, &panels, &[]);
        let def = crate::compile::compile("window.slint", &text, vec![f0, f1, f2], "TelescopeWindow").unwrap_or_else(|e| panic!("{e}\n{text}"));
        let win = def.create().unwrap();
        win.set_global_callback("Folds", "is-folded", |_| Value::Bool(false)).unwrap();
        win.window().set_size(slint::LogicalSize::new(700.0, 800.0));
        assert_eq!(win.get_property("mode").unwrap(), Value::Number(2.0));
        assert_eq!(panels.columns(Mode::One).map(|c| c.0), [vec![1, 0, 2], vec![], vec![]]);
        let at = |id: &str| {
            let e = i_slint_backend_testing::ElementHandle::find_by_element_id(&win, &format!("TelescopeWindow::{id}")).next().unwrap();
            let p = e.absolute_position();
            (p.x, p.y)
        };
        let (b, a, c) = (at("p1"), at("p0"), at("p2"));
        assert!(b.0 == a.0 && a.0 == c.0, "one column: {a:?} {b:?} {c:?}");
        assert!(b.1 < a.1 && a.1 < c.1, "the stage, then left, then right: {a:?} {b:?} {c:?}");
    }

    #[test]
    fn cards_find_their_column_in_every_mode_and_reach_the_shell_by_name() {
        i_slint_backend_testing::init_no_event_loop();
        let camera = include_str!("../ui/cards/camera.slint");
        let (p0, f0) = piece("p0", "plugins/camera_control/card.slint", "CameraCard", camera);
        let (p1, f1) = piece("p1", "plugins/stage/card.slint", "Stage", STAGE);
        let cards = vec![
            CardPlace { piece: p0, region: Region::Right, fold_key: "camera_control/Camera".into(), category: "camera".into() },
            CardPlace { piece: p1, region: Region::Center, fold_key: "stage/Stage".into(), category: "preview".into() },
        ];
        let mut panels = Panels::default();
        panels.place(Region::Right, 0, false, "camera");
        panels.place(Region::Center, 1, false, "preview");
        let text = window(&cards, &panels, &[]);
        let def = crate::compile::compile("window.slint", &text, vec![f0, f1], "TelescopeWindow").unwrap_or_else(|e| panic!("{e}\n{text}"));
        let win = def.create().unwrap();
        win.set_global_callback("Folds", "is-folded", |_| Value::Bool(false)).unwrap();
        win.window().set_size(slint::LogicalSize::new(1300.0, 900.0));
        let num = |name: &str| match win.get_property(name).unwrap() { Value::Number(n) => n, v => panic!("{name}: {v:?}") };
        assert_eq!(num("mode"), 0.0);
        // Three columns: the stage takes what the right rail leaves, as in the Qt render (16..858, 872..1284)
        assert_eq!(num("c1x"), 0.0);
        assert_eq!(num("c1w"), 842.0);
        assert_eq!(num("c2x"), 856.0);
        win.set_property("p1-words", Value::String("hello".into())).unwrap();
        let got = std::rc::Rc::new(std::cell::RefCell::new(0.0));
        let g = got.clone();
        win.set_callback("p1-poked", move |a| { if let Value::Number(n) = a[0] { *g.borrow_mut() = n; } Value::Void }).unwrap();
        win.invoke("p1-poked", &[Value::Number(3.0)]).unwrap();
        assert_eq!(*got.borrow(), 3.0);
        win.window().set_size(slint::LogicalSize::new(560.0, 900.0));
        assert_eq!(num("mode"), 2.0);
        assert_eq!(num("c0w"), 528.0);
    }
}

/// A settings section as the settings window places it.
#[derive(Clone, Debug)]
pub struct SectionPlace {
    pub piece: Piece,
    /// Index into the tabs.
    pub tab: usize,
    pub footer: Option<Piece>,
    /// The plugin's category, for sections that are cards: their icon takes its hue like the main window's cards.
    pub hue: Option<String>,
}

/// The settings window's document: tabs in order (plugins_tab is the index of the Plugins tab, if shown) and every
/// plugin's sections on them.
pub fn settings(tabs: usize, plugins_tab: Option<usize>, sections: &[SectionPlace]) -> String {
    let mut out = String::from("import { Theme, Icons, Folds, Reveal, TextField, WindowBox, KeyFocus } from \"@ui-kit/ui-kit.slint\";\n");
    out.push_str("import { SettingsNav, ScrollPage, ResultsList, PluginsPanel, PluginsFooter, Ask, NavTab, Result, GroupView, UpdateRowView } from \"shell/settings.slint\";\n");
    let mut imported = Vec::new();
    for piece in sections.iter().flat_map(|s| std::iter::once(&s.piece).chain(s.footer.as_ref())) {
        let line = format!("import {{ {} as {}C }} from {};\n", piece.component, piece.id, quote(&piece.file));
        if !imported.contains(&line) {
            imported.push(line.clone());
            out.push_str(&line);
        }
    }
    out.push_str("export { Folds, Theme, Reveal, WindowBox, KeyFocus }\n");
    out.push_str(SETTINGS_TOP);
    for t in 0..tabs {
        out.push_str(&format!("    in-out property page{t}-offset <=> page{t}.offset;\n    out property page{t}-height <=> page{t}.height;\n"));
    }
    for s in sections {
        out.push_str(&format!("    out property <length> {id}-top: {id}.y;\n    out property <length> {id}-tall: {id}.height;\n", id = s.piece.id));
    }
    for piece in sections.iter().flat_map(|s| std::iter::once(&s.piece).chain(s.footer.as_ref())) {
        out.push_str(&aliases(piece));
    }
    out.push_str(SETTINGS_KEYS);
    out.push_str(SETTINGS_RAIL);
    for t in 0..tabs {
        let footers: Vec<&Piece> = sections.iter().filter(|s| s.tab == t).filter_map(|s| s.footer.as_ref()).collect();
        let footer_h = if Some(t) == plugins_tab {
            "(root.footer-shown ? 64px : 0px)".to_string()
        } else {
            footers.iter().map(|f| format!("({}.preferred-height + 10px)", f.id)).chain(std::iter::once("0px".to_string())).collect::<Vec<_>>().join(" + ")
        };
        out.push_str(&format!(
            "    page{t} := ScrollPage {{\n        x: 260px;\n        y: 53px;\n        width: root.width - 276px;\n        height: root.height - 69px - {footer_h};\n        visible: !root.searching && root.current == {t};\n        index: {t};\n"));
        for s in sections.iter().filter(|s| s.tab == t) {
            let hue = s.hue.as_ref().map(|c| format!("hue: Theme.plugin-hue({c:?}); ")).unwrap_or_default();
            out.push_str(&format!("        {} := {}C {{ {hue}}}\n", s.piece.id, s.piece.id));
        }
        if Some(t) == plugins_tab {
            out.push_str(PLUGINS_PANEL);
        }
        out.push_str("    }\n");
        if Some(t) == plugins_tab {
            out.push_str(&format!(
                "    if root.footer-shown && !root.searching && root.current == {t}: PluginsFooter {{\n        x: 260px;\n        y: root.height - 70px;\n        width: root.width - 284px;\n        note: root.footer-note;\n        restart => {{ root.restart(); }}\n    }}\n"));
        }
        let mut below = String::from("root.height - 16px");
        for f in footers.iter().rev() {
            out.push_str(&format!(
                "    {id} := {id}C {{\n        x: 260px;\n        y: {below} - self.preferred-height;\n        width: root.width - 284px;\n        visible: !root.searching && root.current == {t};\n    }}\n", id = f.id));
            below = format!("{}.y - 10px", f.id);
        }
    }
    out.push_str(SETTINGS_BOTTOM.trim_end().strip_suffix('}').unwrap_or(SETTINGS_BOTTOM));
    out.push_str("    }\n}\n");
    out
}

const SETTINGS_TOP: &str = r#"
export component TelescopeSettings inherits Window {
    in property <[NavTab]> tabs;
    in-out property <int> current;
    in property <string> page-title;
    in-out property <string> query;
    in property <bool> searching;
    in property <[Result]> results;
    in-out property <int> result;
    init => {
        WindowBox.width = self.width;
        WindowBox.height = self.height;
    }
    changed width => { WindowBox.width = self.width; }
    changed height => { WindowBox.height = self.height; }
    // Where the focus goes when a text box lets go of it: the scope all the window's keys pass through
    public function focus-home() { keys.focus(); }
    property <int> given-back: KeyFocus.given-back;
    changed given-back => { keys.focus(); }
    in property <string> results-note;
    in property <string> results-empty;
    callback key(KeyEvent, bool, bool) -> bool;
    callback query-changed(string);
    callback step(int);
    callback open-result(int);
    callback open-current;
    callback tab-picked(int);
    in property <[GroupView]> groups;
    in property <bool> all-on-enabled;
    in property <string> empty-note;
    in property <bool> narrowed;
    in-out property <string> plugin-query;
    in-out property <int> plugin-filter;
    in property <bool> footer-shown;
    in property <string> footer-note;
    in property <bool> has-added;
    in property <[UpdateRowView]> update-rows;
    in property <[string]> filter-names;
    callback plugin-switched(string, bool);
    callback plugin-removed(string);
    callback remove-pack;
    callback update-switched(string, bool);
    callback open-url(string);
    callback all-on;
    callback show-all;
    callback plugins-filter-changed;
    callback install;
    callback open-folder;
    callback open-docs;
    callback restart;
    in property <bool> asking;
    in property <string> ask-title;
    in property <string> ask-text;
    in property <string> ask-yes;
    in property <bool> ask-danger;
    in property <bool> ask-info;
    callback answered(bool);
    in property <bool> flashing;
    in property <length> flash-x;
    in property <length> flash-y;
    in property <length> flash-w;
    in property <length> flash-h;
    public function focus-search() { search.focus-all(); }

    title: @tr("Settings");
    in property <image> app-icon;
    icon: app-icon;
    preferred-width: 940px;
    preferred-height: 680px;
    in property <length> least-width: 720px;
    in property <length> least-height: 480px;
    min-width: least-width;
    min-height: least-height;
    background: Theme.bg;
    default-font-family: Theme.font;
    default-font-size: Theme.body;
"#;

/// Holds everything in the window, so plugins hear a key before the control with focus, as in the main window.
const SETTINGS_KEYS: &str = r#"    keys := FocusScope {
        x: 0;
        y: 0;
        width: root.width;
        height: root.height;
        focus-on-click: false;
        focus-on-tab-navigation: false;
        init => { self.focus(); }
        capture-key-pressed(e) => { root.key(e, false, TextInputInterface.text-input-focused) ? accept : reject }
        capture-key-released(e) => { root.key(e, true, TextInputInterface.text-input-focused) ? accept : reject }
"#;

const SETTINGS_RAIL: &str = r#"    Rectangle {
        x: 0;
        y: 0;
        width: 236px;
        height: root.height;
        background: Theme.chrome;
        Rectangle { x: parent.width - 1px; width: 1px; background: Theme.border; }
        search := TextField {
            x: 10px;
            y: 14px;
            width: 215px;
            text <=> root.query;
            placeholder: @tr("Search settings");
            label: @tr("Search settings");
            has-icon: true;
            icon: Icons.search;
            clearable: true;
            edited(t) => { root.query-changed(t); }
            step(d) => { root.step(d); }
            accepted => { root.open-current(); }
        }
        SettingsNav {
            x: 10px;
            y: 54px;
            width: 215px;
            height: root.height - 54px;
            tabs: root.tabs;
            current <=> root.current;
            picked(i) => { root.tab-picked(i); }
        }
    }
    Text {
        x: 260px;
        y: 18px;
        width: root.width - 276px;
        height: 23px;
        text: root.page-title;
        font-family: Theme.font;
        font-size: 19px; // 14pt, which Qt draws at 19
        font-weight: 600;
        color: Theme.text;
        vertical-alignment: center;
        accessible-role: text;
    }
"#;

const PLUGINS_PANEL: &str = r#"        PluginsPanel {
            groups: root.groups;
            all-on-enabled: root.all-on-enabled;
            empty-note: root.empty-note;
            narrowed: root.narrowed;
            query <=> root.plugin-query;
            filter <=> root.plugin-filter;
            columns: max(parent.inner-width < 400px ? 1 : 2, min(3, floor((parent.inner-width + 10px) / 240px)));
            has-added: root.has-added;
            update-rows: root.update-rows;
            filter-names: root.filter-names;
            switched(name, on) => { root.plugin-switched(name, on); }
            removed(name) => { root.plugin-removed(name); }
            remove-pack => { root.remove-pack(); }
            update-switched(id, on) => { root.update-switched(id, on); }
            open-url(u) => { root.open-url(u); }
            all-on => { root.all-on(); }
            show-all => { root.show-all(); }
            filter-changed => { root.plugins-filter-changed(); }
            install => { root.install(); }
            open-folder => { root.open-folder(); }
            open-docs => { root.open-docs(); }
        }
"#;

const SETTINGS_BOTTOM: &str = r#"    ResultsList {
        x: 260px;
        y: 53px;
        width: root.width - 276px;
        height: root.height - 69px;
        visible: root.searching;
        results: root.results;
        current <=> root.result;
        note: root.results-note;
        empty: root.results-empty;
        opened(i) => { root.open-result(i); }
    }
    if root.flashing: Rectangle {
        x: root.flash-x;
        y: root.flash-y;
        width: root.flash-w;
        height: root.flash-h;
        border-radius: 9px;
        border-width: 1px;
        border-color: Theme.accent;
    }
    if root.asking: Ask {
        width: root.width;
        height: root.height;
        title: root.ask-title;
        text: root.ask-text;
        yes: root.ask-yes;
        danger: root.ask-danger;
        info: root.ask-info;
        answered(yes) => { root.answered(yes); }
    }
}
"#;
