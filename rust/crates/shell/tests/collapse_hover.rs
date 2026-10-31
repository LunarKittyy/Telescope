// A card's or section's title brightens under the pointer, but not while its card is dimmed. Drawn with the offscreen
// renderer, since the look is the only thing that tells.
use i_slint_backend_testing::ElementHandle;
use slint::platform::WindowEvent;
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use telescope_shell::offscreen;

const DOC: &str = r#"
import { Card, Folds, Icons, Section } from "@ui-kit/ui-kit.slint";
export { Folds }
export component Test inherits Window {
    width: 300px;
    height: 200px;
    in property <bool> dim;
    Card {
        title: "Things";
        icon: Icons.sun;
        fold-key: "p/Things";
        enabled: !dim;
        blocked: dim;
        Text { text: "loose"; }
        Section { scope: "p/Things"; key: "Light"; title: "Light"; icon: Icons.sun; Text { text: "row"; } }
    }
}
"#;

fn title(ui: &ComponentInstance, name: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, name).find(|e| e.accessible_checkable() == Some(true)).unwrap_or_else(|| panic!("no title {name}"))
}

/// A title's strip of the card, as pixels.
fn strip(ui: &ComponentInstance, name: &str) -> Vec<u8> {
    let pic = offscreen::draw(ui.window(), 300.0, 200.0, 1.0).expect("a drawn window");
    let t = title(ui, name);
    let (p, s) = (t.absolute_position(), t.size());
    let (x0, x1, width) = (p.x as usize, (p.x + s.width) as usize, pic.width as usize);
    (p.y as usize..(p.y + s.height) as usize).flat_map(|y| pic.rgba[(y * width + x0) * 4..(y * width + x1) * 4].to_vec()).collect()
}

fn built() -> ComponentInstance {
    assert!(offscreen::install());
    let def = telescope_shell::compile::compile("hover.slint", DOC, vec![], "Test").unwrap();
    let ui = def.create().unwrap();
    ui.set_global_callback("Folds", "is-folded", |_| Value::Bool(false)).unwrap();
    ui.show().unwrap();
    ui
}

fn pointer(ui: &ComponentInstance, x: f32, y: f32) {
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(x, y) });
}

/// The pointer over the middle of a title.
fn over(ui: &ComponentInstance, name: &str) {
    let t = title(ui, name);
    let (p, s) = (t.absolute_position(), t.size());
    pointer(ui, p.x + s.width / 2.0, p.y + s.height / 2.0);
}

// One process draws one scene at a time, so every check shares a window
#[test]
fn hovering_a_title_marks_it_and_a_dimmed_cards_title_stays_as_it_was() {
    let ui = built();
    let plain = strip(&ui, "Things");
    over(&ui, "Things");
    assert_ne!(strip(&ui, "Things"), plain, "the title looks different under the pointer");
    ui.window().dispatch_event(WindowEvent::PointerExited);
    assert_eq!(strip(&ui, "Things"), plain, "leaving puts it back");
    let section = strip(&ui, "Light");
    let t = title(&ui, "Light");
    // Anywhere on the heading's row marks its title, here the rule beyond it
    pointer(&ui, 270.0, t.absolute_position().y + t.size().height / 2.0);
    assert_ne!(strip(&ui, "Light"), section, "the section's title looks different too");
    ui.window().dispatch_event(WindowEvent::PointerExited);
    assert_eq!(strip(&ui, "Light"), section);
    ui.set_property("dim", Value::Bool(true)).unwrap();
    let dim = strip(&ui, "Things");
    over(&ui, "Things");
    assert_eq!(strip(&ui, "Things"), dim, "a title that goes dim with its card stops looking hovered");
}
