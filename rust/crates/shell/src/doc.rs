//! Writes the main window as one .slint document at startup: the frame from ui/frame.slint with every plugin's card
//! and header widget placed in it, then compiles it with the interpreter (decided in rust/ARCHITECTURE.md).
//!
//! Cards sit in one clipped body and each card works out its own place from the window's mode, so a card never moves
//! to another parent when the window crosses 1300 or 900 px. Each column scrolls on its own like Qt's three scroll
//! areas. Every property and callback a card declares is aliased on the window as p<n>-<name>, which is how
//! set_ui_props and UiEvents reach it by name.

use crate::layout::{Mode, Panels, Region};
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
    out.push_str("import { Theme, Icons, Folds } from \"@ui-kit/ui-kit.slint\";\n");
    out.push_str("import { Footer, BannerArea, StreamTiles, HeaderIconButton, StartButton, ColumnScrollbar, BannerData, TileData } from \"shell/frame.slint\";\n");
    let mut imported = Vec::new();
    for piece in cards.iter().map(|c| &c.piece).chain(headers.iter().map(|h| &h.piece)) {
        let line = format!("import {{ {} as {}C }} from {};\n", piece.component, piece.id, quote(&piece.file));
        if !imported.contains(&line) {
            imported.push(line.clone());
            out.push_str(&line);
        }
    }
    out.push_str("export { Folds, Theme }\n");
    out.push_str(FRAME_TOP);

    // Column geometry for each mode, relative to the body's left edge
    let cols = [Mode::Three, Mode::Two, Mode::One].map(|m| panels.columns(m));
    for c in 0..3 {
        let x = mode_expr(|m| col_x(&cols[m], c));
        let w = mode_expr(|m| col_w(&cols[m], c));
        out.push_str(&format!("    out property <length> c{c}x: {x};\n    out property <length> c{c}w: {w};\n"));
        out.push_str(&format!("    property <length> card{c}w: c{c}w - {INSET}px - (bar{c} ? {BAR}px : 0px);\n"));
    }
    // Preferred content height and stretch room of each column in the current mode
    for c in 0..3 {
        let pref = mode_expr(|m| sum_heights(&cols[m][c].0));
        let centers = mode_expr(|m| {
            let n: Vec<String> = cols[m][c].0.iter().filter(|&&i| cards[i].region == Region::Center)
                .map(|&i| format!("({}.visible ? 1 : 0)", cards[i].piece.id)).collect();
            if n.is_empty() { "0".into() } else { n.join(" + ") }
        });
        out.push_str(&format!("    property <length> col{c}-pref: {pref};\n"));
        out.push_str(&format!("    property <int> col{c}-centers: {centers};\n"));
        out.push_str(&format!(
            "    property <length> extra{c}: col{c}-centers > 0 ? max(0px, view-h - col{c}-pref) / col{c}-centers : 0px;\n"));
        out.push_str(&format!("    property <length> s{c}: clamp(scroll{c}, 0px, max(0px, col{c}-pref - view-h));\n"));
        out.push_str(&format!("    changed col{c}-pref => {{ check-bars(); }}\n"));
    }
    out.push_str("    changed view-h => { check-bars(); }\n");
    out.push_str("    public function check-bars() {\n");
    for c in 0..3 {
        out.push_str(&format!("        bar{c} = col{c}-pref > view-h;\n"));
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
    out.push_str(&HEADER.replace("{left}", &header(false)).replace("{right}", &header(true)));

    // The body: cards placed by their column in the current mode
    out.push_str(BODY_TOP);
    for (i, card) in cards.iter().enumerate() {
        out.push_str(&card_element(i, card, cards, &cols));
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

fn sum_heights(list: &[usize]) -> String {
    if list.is_empty() {
        return "0px".into();
    }
    let parts: Vec<String> = list.iter().map(|i| format!("(p{i}.visible ? p{i}.preferred-height + {GAP}px : 0px)")).collect();
    format!("max(0px, {} - {GAP}px)", parts.join(" + "))
}

fn aliases(piece: &Piece) -> String {
    let mut out = format!("    in-out property {id}-visible <=> {id}.visible;\n", id = piece.id);
    for p in &piece.iface.props {
        out.push_str(&format!("    in-out property {}-{p} <=> {}.{p};\n", piece.id, piece.id));
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
        "            {id} := {id}C {{\n                x: {x};\n                y: {y};\n                width: {w};\n                height: self.preferred-height + {extra};\n                fold-key: {key};\n                hue: Theme.plugin-hue({cat});\n            }}\n",
        key = quote(&card.fold_key),
        cat = quote(&card.category),
    )
}

const FRAME_TOP: &str = r#"
export component TelescopeWindow inherits Window {
    in property <string> window-title: "Telescope";
    in property <string> start-text;
    // Translators' short form of the start button, for a narrow window
    in property <string> start-short;
    in property <bool> streaming;
    in property <bool> start-enabled: true;
    in property <bool> add-visible;
    in property <bool> add-enabled: true;
    in property <[BannerData]> banners;
    in property <[TileData]> tiles;
    in property <string> status;
    in property <string> status-kind: "status_dim";
    in property <string> fps: "—";
    in property <color> fps-color: Theme.text;
    in property <string> fps-tip;
    in property <string> net: "—";
    in property <color> net-color: Theme.text;
    callback start-clicked;
    callback settings-clicked;
    callback add-clicked;
    callback banner-action(string, int);
    callback banner-dismissed(string);
    callback tile-focus(string);
    callback tile-stop(string);
    out property <int> mode: root.width >= 1300px ? 0 : root.width >= 900px ? 1 : 2;
    in-out property <length> scroll0;
    in-out property <length> scroll1;
    in-out property <length> scroll2;
    in-out property <bool> bar0;
    in-out property <bool> bar1;
    in-out property <bool> bar2;
    property <length> body-top: 60px + banner-area.preferred-height + tiles-row.preferred-height;
    property <length> view-h: max(0px, root.height - 40px - body-top - 32px);
    property <length> body-w: max(0px, root.width - 32px);
    out property <length> content-height: max(col0-pref, max(col1-pref, col2-pref));

    title: window-title;
    icon: Icons.camera;
    preferred-width: 1380px;
    preferred-height: 900px;
    min-width: 560px;
    min-height: 520px;
    background: Theme.bg;
    default-font-family: Theme.font;
    default-font-size: Theme.body;
"#;

const HEADER: &str = r#"
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
                spacing: 14px;
{left}            }
            Rectangle { horizontal-stretch: 1; }
            HorizontalLayout {
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
                clicked => { root.add-clicked(); }
            }
            StartButton {
                y: (parent.height - self.height) / 2;
                text: root.mode == 2 ? start-short : start-text;
                streaming: root.streaming;
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

const BODY_TOP: &str = r#"
    Rectangle {
        x: 16px;
        y: body-top + 16px;
        width: body-w;
        height: view-h;
        clip: true;
        TouchArea {
            scroll-event(e) => {
                if (self.mouse-x >= root.c2x && root.c2w > 0) {
                    root.scroll2 = clamp(root.s2 - e.delta-y, 0px, max(0px, root.col2-pref - root.view-h));
                } else if (self.mouse-x >= root.c1x && root.c1w > 0) {
                    root.scroll1 = clamp(root.s1 - e.delta-y, 0px, max(0px, root.col1-pref - root.view-h));
                } else {
                    root.scroll0 = clamp(root.s0 - e.delta-y, 0px, max(0px, root.col0-pref - root.view-h));
                }
                accept
            }
"#;

const BODY_BOTTOM: &str = r#"        }
        ColumnScrollbar { x: root.c0x + root.c0w - 11px; height: root.view-h; visible: root.bar0 && root.c0w > 0; content: root.col0-pref; viewport: root.view-h; offset <=> root.scroll0; }
        ColumnScrollbar { x: root.c1x + root.c1w - 11px; height: root.view-h; visible: root.bar1 && root.c1w > 0; content: root.col1-pref; viewport: root.view-h; offset <=> root.scroll1; }
        ColumnScrollbar { x: root.c2x + root.c2w - 11px; height: root.view-h; visible: root.bar2 && root.c2w > 0; content: root.col2-pref; viewport: root.view-h; offset <=> root.scroll2; }
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

    fn piece(id: &str, file: &str, component: &str, text: &str) -> (Piece, (String, String)) {
        (Piece { id: id.into(), file: file.into(), component: component.into(), iface: scan(text, component) }, (file.into(), text.into()))
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
