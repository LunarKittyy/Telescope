// The outline round a control an outside caller (the MCP server) just touched: drawn for a setting's id, faded out, and absent for a control
// that carries another id. Read from the pixels of the kit's own slider and switch.

use slint::ComponentHandle;
use slint_interpreter::{ComponentInstance, Struct, Value};
use telescope_shell::{compile, offscreen};

const DOC: &str = r#"
import { Slider, Switch, Touched } from "@ui-kit/ui-kit.slint";
export { Touched }
export component T inherits Window {
    background: #000000;
    width: 400px;
    height: 200px;
    Slider { x: 20px; y: 20px; width: 200px; setting: "colour_grading.contrast"; }
    Slider { x: 20px; y: 80px; width: 200px; setting: "colour_grading.tint"; }
    Switch { x: 20px; y: 140px; setting: "transforms.flip_h"; }
}
"#;

fn window() -> ComponentInstance {
    offscreen::install();
    let ui = compile::compile("test.slint", DOC, Vec::new(), "T").map(|d| d.create().unwrap()).unwrap_or_else(|e| panic!("{e}"));
    ui.window().set_size(slint::LogicalSize::new(400.0, 200.0));
    ui.show().unwrap();
    ui
}

fn touched(ui: &ComponentInstance, items: &[(&str, bool, bool)]) {
    let rows: Vec<Value> = items.iter().map(|(id, on, changed)| Value::Struct(Struct::from_iter([
        ("id".to_string(), Value::String((*id).into())),
        ("on".to_string(), Value::Bool(*on)),
        ("changed".to_string(), Value::Bool(*changed)),
    ]))).collect();
    ui.set_global_property("Touched", "items", Value::Model(slint::ModelRc::new(slint::VecModel::from(rows)))).unwrap();
}

/// How much of the accent (violet) outline colour is at a pixel of the drawn window: 1 for the accent itself, 0 for black.
fn accent_at(ui: &ComponentInstance, x: u32, y: u32) -> f32 {
    let picture = offscreen::draw(ui.window(), 400.0, 200.0, 1.0).unwrap();
    let at = ((y * picture.width + x) * 4) as usize;
    let (r, g, b) = (picture.rgba[at] as f32, picture.rgba[at + 1] as f32, picture.rgba[at + 2] as f32);
    // #aa9cf5 is the accent
    ((r / 170.0).min(1.0) + (g / 156.0).min(1.0) + (b / 245.0).min(1.0)) / 3.0
}

// The slider's track is 36px tall at y 20, its outline 3px out and 2px wide for a change: the top edge is at y 17 to 18, over the middle of the track
const SLIDER_TOP: (u32, u32) = (120, 17);
const OTHER_SLIDER_TOP: (u32, u32) = (120, 77);

#[test]
fn a_touched_setting_is_outlined_on_the_control_that_carries_its_id_and_on_no_other() {
    let ui = &window();
    assert!(accent_at(ui, SLIDER_TOP.0, SLIDER_TOP.1) < 0.2, "nothing touched, nothing outlined");
    touched(ui, &[("colour_grading.contrast", true, true)]);
    assert!(accent_at(ui, SLIDER_TOP.0, SLIDER_TOP.1) > 0.8, "the contrast slider has its outline");
    assert!(accent_at(ui, OTHER_SLIDER_TOP.0, OTHER_SLIDER_TOP.1) < 0.2, "the tint slider next to it has none");
}

#[test]
fn a_read_is_a_thinner_outline_than_a_change() {
    let ui = &window();
    touched(ui, &[("colour_grading.contrast", true, false)]);
    // A one pixel line at y 17, where a change's two pixel line is at 17 and 18
    assert!(accent_at(ui, SLIDER_TOP.0, 17) > 0.8);
    assert!(accent_at(ui, SLIDER_TOP.0, 18) < 0.2, "a read draws one pixel, not two");
    touched(ui, &[("colour_grading.contrast", true, true)]);
    assert!(accent_at(ui, SLIDER_TOP.0, 18) > 0.8);
}

#[test]
fn an_outline_that_is_told_to_fade_goes_and_a_cleared_list_has_none() {
    let ui = &window();
    touched(ui, &[("colour_grading.contrast", true, true)]);
    assert!(accent_at(ui, SLIDER_TOP.0, SLIDER_TOP.1) > 0.8);
    touched(ui, &[("colour_grading.contrast", false, true)]);
    // The fade is an animation: run the window's clock past it
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert!(accent_at(ui, SLIDER_TOP.0, SLIDER_TOP.1) < 0.2, "faded out");
    touched(ui, &[]);
    assert!(accent_at(ui, SLIDER_TOP.0, SLIDER_TOP.1) < 0.2);
}

#[test]
fn a_switch_is_outlined_by_its_setting_too() {
    let ui = &window();
    touched(ui, &[("transforms.flip_h", true, true)]);
    // The switch is 36 by 20 at (20, 140): its outline 3px above the top edge, over the middle
    assert!(accent_at(ui, 38, 137) > 0.8);
}

#[test]
fn a_control_with_no_setting_id_is_never_outlined() {
    let ui = &window();
    touched(ui, &[("", true, true)]);
    assert!(accent_at(ui, SLIDER_TOP.0, SLIDER_TOP.1) < 0.2);
}
