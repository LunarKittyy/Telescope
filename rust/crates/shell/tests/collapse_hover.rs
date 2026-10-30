// A card's title brightens under the pointer, but not while its card is dimmed. Drawn with the offscreen renderer, since
// the look is the only thing that tells.
use slint::platform::WindowEvent;
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::Value;
use telescope_shell::offscreen;

const DOC: &str = r#"
import { Card, Folds, Icons } from "@ui-kit/ui-kit.slint";
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
    }
}
"#;

/// The title's strip of the card, as pixels.
fn title_strip(ui: &slint_interpreter::ComponentInstance) -> Vec<u8> {
    let pic = offscreen::draw(ui.window(), 300.0, 200.0, 1.0).expect("a drawn window");
    let (x0, x1) = (58usize, 130usize);
    (16..46).flat_map(|y| pic.rgba[(y * pic.width as usize + x0) * 4..(y * pic.width as usize + x1) * 4].to_vec()).collect()
}

fn built() -> slint_interpreter::ComponentInstance {
    assert!(offscreen::install());
    let def = telescope_shell::compile::compile("hover.slint", DOC, vec![], "Test").unwrap();
    let ui = def.create().unwrap();
    ui.set_global_callback("Folds", "is-folded", |_| Value::Bool(false)).unwrap();
    ui.show().unwrap();
    ui
}

fn pointer(ui: &slint_interpreter::ComponentInstance, x: f32, y: f32) {
    ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(x, y) });
}

// One process draws one scene at a time, so both checks share a window
#[test]
fn hovering_a_title_marks_it_and_a_dimmed_cards_title_stays_as_it_was() {
    let ui = built();
    let plain = title_strip(&ui);
    pointer(&ui, 80.0, 30.0);
    let hot = title_strip(&ui);
    assert_ne!(plain, hot, "the title looks different under the pointer");
    ui.window().dispatch_event(WindowEvent::PointerExited);
    assert_eq!(title_strip(&ui), plain, "leaving puts it back");
    ui.set_property("dim", Value::Bool(true)).unwrap();
    let dim = title_strip(&ui);
    pointer(&ui, 80.0, 30.0);
    assert_eq!(title_strip(&ui), dim, "a title that goes dim with its card stops looking hovered");
}
