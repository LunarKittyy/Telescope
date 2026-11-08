//! The specimen sheet: every kit control kind in every state, in the new look and in Classic, drawn offscreen as one picture
//! per look. Rows are kinds and columns are states, so a kind that lacks a hover or a disabled look shows as a gap in the grid
//! or as a cell that looks like its neighbour. The last column is a long text, what a German translation would give.
//! `cargo run -p telescope-shell --bin shell-specimen -- OUT` in rust/ writes OUT/new.png and OUT/classic.png (STYLE.md).

use crate::offscreen::{self, Picture};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentDefinition, Value};
use std::path::Path;

/// A kind's row: its name in specimen.slint, the name on the sheet, the cell's height, where the pointer goes for hover and
/// held (as fractions of the cell), and the states it has. A kind without a state has no cell there.
struct Kind {
    name: &'static str,
    label: &'static str,
    height: f32,
    at: (f32, f32),
    states: &'static [&'static str],
}

const COLUMNS: &[(&str, &str)] = &[
    ("rest", "Rest"),
    ("on", "On"),
    ("hover", "Hover"),
    ("focus", "Focus"),
    ("held", "Held"),
    ("off", "Disabled"),
    ("empty", "Placeholder"),
    ("long", "Long text"),
];

const MID: (f32, f32) = (0.5, 0.5);
const PRESSABLE: &[&str] = &["rest", "hover", "focus", "held", "off", "long"];

const KINDS: &[Kind] = &[
    Kind { name: "button", label: "button", height: 56.0, at: MID, states: PRESSABLE },
    Kind { name: "button-primary", label: "button (primary)", height: 56.0, at: MID, states: PRESSABLE },
    Kind { name: "button-danger", label: "button (danger)", height: 56.0, at: MID, states: PRESSABLE },
    Kind { name: "button-quiet", label: "button (quiet)", height: 56.0, at: MID, states: PRESSABLE },
    Kind { name: "icon-button", label: "icon-button", height: 56.0, at: (0.08, 0.5), states: &["rest", "hover", "focus", "held", "off"] },
    Kind { name: "link", label: "link", height: 44.0, at: (0.08, 0.5), states: &["rest", "hover", "focus", "long"] },
    Kind { name: "switch", label: "switch", height: 48.0, at: (0.08, 0.5), states: &["rest", "on", "hover", "focus", "held", "off"] },
    Kind { name: "toggle", label: "toggle", height: 56.0, at: MID, states: &["rest", "on", "hover", "focus", "held", "off", "long"] },
    Kind { name: "slider", label: "slider", height: 56.0, at: (0.3, 0.5), states: PRESSABLE },
    Kind { name: "slider-key", label: "slider (key fills)", height: 124.0, at: (0.3, 0.2), states: &["rest", "hover", "held", "off"] },
    Kind { name: "segmented", label: "segmented", height: 56.0, at: (0.8, 0.5), states: PRESSABLE },
    Kind { name: "picker", label: "picker", height: 64.0, at: MID, states: &["rest", "hover", "focus", "held", "off", "empty", "long"] },
    Kind { name: "field", label: "field (text)", height: 56.0, at: (0.06, 0.5), states: &["rest", "hover", "focus", "off", "empty", "long"] },
    Kind { name: "number", label: "field (number)", height: 56.0, at: (0.06, 0.5), states: &["rest", "hover", "focus", "off", "empty", "long"] },
    Kind { name: "jog", label: "jog", height: 56.0, at: MID, states: PRESSABLE },
    Kind { name: "stepper", label: "stepper", height: 76.0, at: (0.07, 0.5), states: PRESSABLE },
    Kind { name: "strip", label: "strip", height: 64.0, at: MID, states: &["rest", "on", "hover", "off", "long"] },
    Kind { name: "badge", label: "badge", height: 44.0, at: MID, states: &["rest", "long"] },
    Kind { name: "tip", label: "tip", height: 96.0, at: MID, states: &["rest", "long"] },
    Kind { name: "row", label: "row", height: 56.0, at: MID, states: &["rest", "off", "long"] },
    Kind { name: "section", label: "section", height: 44.0, at: MID, states: &["rest", "long"] },
    Kind { name: "scrollbar", label: "scrollbar", height: 104.0, at: (0.06, 0.2), states: &["rest", "hover", "held"] },
    Kind { name: "card", label: "card", height: 150.0, at: MID, states: &["rest", "off", "long"] },
];

const CELL_WIDTH: f32 = 300.0;
const LABEL_WIDTH: f32 = 140.0;
const HEADING_HEIGHT: f32 = 28.0;
const GAP: usize = 4;

fn fail(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// One control in one state, drawn on its own.
fn cell(def: &ComponentDefinition, classic: bool, kind: &Kind, state: &str) -> Result<Picture, String> {
    let ui = def.create().map_err(fail)?;
    ui.set_global_property("Theme", "classic", Value::Bool(classic)).map_err(fail)?;
    ui.set_property("kind", Value::String(kind.name.into())).map_err(fail)?;
    for (flag, name) in [("off", "off"), ("empty", "empty"), ("long", "long"), ("on", "lit")] {
        ui.set_property(name, Value::Bool(state == flag)).map_err(fail)?;
    }
    ui.show().map_err(fail)?;
    ui.window().set_size(slint::LogicalSize::new(CELL_WIDTH, kind.height));
    let window = ui.window();
    let position = LogicalPosition::new(kind.at.0 * CELL_WIDTH, kind.at.1 * kind.height);
    match state {
        "hover" => window.dispatch_event(WindowEvent::PointerMoved { position }),
        "held" => {
            window.dispatch_event(WindowEvent::PointerMoved { position });
            window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        }
        "focus" => {
            ui.set_global_property("KeyFocus", "keyboard", Value::Bool(true)).map_err(fail)?;
            window.dispatch_event(WindowEvent::KeyPressed { text: Key::Tab.into() });
            window.dispatch_event(WindowEvent::KeyReleased { text: Key::Tab.into() });
        }
        _ => {}
    }
    let picture = offscreen::draw(window, CELL_WIDTH, kind.height, 1.0).ok_or("nothing drawn")?;
    let _ = ui.hide();
    Ok(picture)
}

/// A heading of the sheet, in a cell this wide.
fn heading(def: &ComponentDefinition, classic: bool, text: &str, column: bool, width: f32) -> Result<Picture, String> {
    let ui = def.create().map_err(fail)?;
    ui.set_global_property("Theme", "classic", Value::Bool(classic)).map_err(fail)?;
    ui.set_property("text", Value::String(text.into())).map_err(fail)?;
    ui.set_property("column", Value::Bool(column)).map_err(fail)?;
    ui.show().map_err(fail)?;
    let picture = offscreen::draw(ui.window(), width, HEADING_HEIGHT, 1.0).ok_or("nothing drawn")?;
    let _ = ui.hide();
    Ok(picture)
}

/// Puts picture on the sheet with its top left at (x, y).
fn blit(sheet: &mut Picture, picture: &Picture, x: usize, y: usize) {
    let row_bytes = picture.width as usize * 4;
    for row in 0..picture.height as usize {
        let from = row * row_bytes;
        let to = ((y + row) * sheet.width as usize + x) * 4;
        sheet.rgba[to..to + row_bytes].copy_from_slice(&picture.rgba[from..from + row_bytes]);
    }
}

fn sheet(cells: &ComponentDefinition, headings: &ComponentDefinition, classic: bool) -> Result<Picture, String> {
    let (label_w, cell_w, head_h) = (LABEL_WIDTH as usize, CELL_WIDTH as usize, HEADING_HEIGHT as usize);
    let width = label_w + GAP + COLUMNS.len() * (cell_w + GAP);
    let height = head_h + GAP + KINDS.iter().map(|k| k.height as usize + GAP).sum::<usize>();
    let background: [u8; 4] = if classic { [10, 12, 15, 255] } else { [13, 13, 13, 255] };
    let rgba = background.iter().copied().cycle().take(width * height * 4).collect();
    let mut out = Picture { width: width as u32, height: height as u32, rgba };
    for (i, (_, name)) in COLUMNS.iter().enumerate() {
        blit(&mut out, &heading(headings, classic, name, true, CELL_WIDTH)?, label_w + GAP + i * (cell_w + GAP), 0);
    }
    let mut y = head_h + GAP;
    for kind in KINDS {
        blit(&mut out, &heading(headings, classic, kind.label, false, LABEL_WIDTH)?, 0, y + (kind.height as usize - head_h) / 2);
        for (i, (state, _)) in COLUMNS.iter().enumerate() {
            if kind.states.contains(state) {
                blit(&mut out, &cell(cells, classic, kind, state)?, label_w + GAP + i * (cell_w + GAP), y);
            }
        }
        y += kind.height as usize + GAP;
    }
    Ok(out)
}

/// Writes out/new.png and out/classic.png; how many pictures.
pub fn render(out: &Path) -> Result<usize, String> {
    if !offscreen::install() {
        return Err("a window system was already set up".into());
    }
    std::fs::create_dir_all(out).map_err(fail)?;
    let doc = include_str!("../ui/specimen.slint");
    let cells = crate::compile::compile("specimen.slint", doc, vec![], "Cell")?;
    let headings = crate::compile::compile("specimen.slint", doc, vec![], "Heading")?;
    for (name, classic) in [("new", false), ("classic", true)] {
        sheet(&cells, &headings, classic)?.save_png(&out.join(format!("{name}.png")))?;
    }
    Ok(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How many pixels of the cell differ between two states.
    fn differing(a: &Picture, b: &Picture) -> usize {
        assert_eq!((a.width, a.height), (b.width, b.height));
        a.rgba.chunks(4).zip(b.rgba.chunks(4)).filter(|(x, y)| x != y).count()
    }

    // A kind whose hover or held cell looks like its rest cell is the usual fault (STYLE.md, Specimen sheet)
    #[test]
    fn hover_and_held_look_unlike_rest_and_each_other_in_both_looks() {
        assert!(offscreen::install());
        let doc = include_str!("../ui/specimen.slint");
        let cells = crate::compile::compile("specimen.slint", doc, vec![], "Cell").expect("the cells compile");
        let draw = |classic: bool, name: &str, state: &str| {
            let kind = KINDS.iter().find(|k| k.name == name).expect("a kind of the sheet");
            cell(&cells, classic, kind, state).expect("the cell draws")
        };
        for classic in [false, true] {
            // The text boxes hover over their text too (the box swallows the pointer), the link and the strip answer it
            for name in ["field", "number", "link", "strip", "icon-button"] {
                let (rest, hover) = (draw(classic, name, "rest"), draw(classic, name, "hover"));
                assert!(differing(&rest, &hover) > 50, "{name} (classic {classic}): hover looks like rest");
            }
            // Held is not hover, and not rest
            for name in ["switch", "toggle", "picker", "segmented", "stepper"] {
                let (rest, hover, held) = (draw(classic, name, "rest"), draw(classic, name, "hover"), draw(classic, name, "held"));
                assert!(differing(&rest, &held) > 50, "{name} (classic {classic}): held looks like rest");
                assert!(differing(&hover, &held) > 50, "{name} (classic {classic}): held looks like hover");
            }
            // A disabled icon button is there, on a surface of its own
            let (rest, off) = (draw(classic, "icon-button", "rest"), draw(classic, "icon-button", "off"));
            assert!(differing(&rest, &off) > 200, "icon-button (classic {classic}): disabled can't be told from rest");
            // A box with nothing in it shows its placeholder
            for name in ["picker", "number"] {
                let (rest, empty) = (draw(classic, name, "rest"), draw(classic, name, "empty"));
                assert!(differing(&rest, &empty) > 20, "{name} (classic {classic}): no placeholder");
            }
        }
    }
}
