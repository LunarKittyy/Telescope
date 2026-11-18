// The preview's pick hint (in the card's stage and in the pop-out) takes its text size and corners from the window's Theme, so
// Appearance's Text size and Corners reach it.
use slint::ComponentHandle;
use slint_interpreter::ComponentInstance;
use telescope_core::host::{Appearance, Corners, TextSize};

const DOC: &str = r#"
import { PreviewPick } from "shell/preview.slint";
export { Theme } from "@ui-kit/ui-kit.slint";
export component W inherits Window {
    width: 300px;
    height: 120px;
    background: white;
    PreviewPick {
        pic-x: 0px;
        pic-y: 0px;
        pic-width: 300px;
        pic-height: 120px;
        pick-hint: "Click and drag to pick a spot";
        font: "";
    }
}
"#;

/// The chip's height in pixels, and whether the pixel at its top-left corner is drawn.
fn chip(ui: &ComponentInstance, a: Appearance) -> (u32, bool) {
    telescope_shell::appearance::apply_to(ui, &a);
    let picture = telescope_shell::offscreen::draw(ui.window(), 300.0, 120.0, 1.0).unwrap();
    let dark = |x: u32, y: u32| {
        let i = ((y * picture.width + x) * 4) as usize;
        picture.rgba[i] < 200
    };
    let rows: Vec<u32> = (0..picture.height).filter(|&y| (0..picture.width).any(|x| dark(x, y))).collect();
    let (top, bottom) = (*rows.first().expect("a chip"), *rows.last().unwrap());
    let left = (0..picture.width).find(|&x| (top..=bottom).any(|y| dark(x, y))).unwrap();
    (bottom - top + 1, dark(left, top))
}

#[test]
fn the_pick_hint_follows_the_text_size_and_the_corners() {
    telescope_shell::offscreen::install();
    let ui = telescope_shell::compile::compile("hint.slint", DOC, vec![], "W").expect("the document compiles").create().unwrap();
    ui.show().unwrap();
    let base = Appearance::default();
    let (small, _) = chip(&ui, Appearance { text: TextSize::Small, ..base });
    let (normal, rounded) = chip(&ui, base);
    let (large, _) = chip(&ui, Appearance { text: TextSize::Large, ..base });
    assert!(small <= normal && normal < large, "the chip grows with the text: {small} {normal} {large}");
    assert!(!rounded, "the default corners are round");
    let (_, square) = chip(&ui, Appearance { corners: Corners::Sharp, ..base });
    assert!(square, "sharp corners are square");
}
