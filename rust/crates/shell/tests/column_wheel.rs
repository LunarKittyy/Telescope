// The wheel over a control scrolls the column it's in, the way NoScrollSlider and NoScrollComboBox pass it on.
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};

const DOC: &str = r#"
import { ChoiceField, Slider, Shortcuts } from "@ui-kit/ui-kit.slint";
export { Shortcuts }
export component W inherits Window {
    width: 400px;
    height: 300px;
    in-out property <length> scrolled;
    TouchArea {
        scroll-event(e) => {
            root.scrolled += e.delta-y;
            accept
        }
        VerticalLayout {
            alignment: start;
            padding: 10px;
            spacing: 10px;
            ChoiceField { compact: true; caption: "Plain"; items: ["a", "b"]; }
            ChoiceField { compact: true; caption: "Tipped"; items: ["a", "b"]; tip: "About it"; shortcut: "a.b"; }
            Slider { label: "Speed"; tip: "How fast"; shortcut: "a.c"; }
        }
    }
}"#;

fn open() -> ComponentInstance {
    telescope_shell::offscreen::install();
    let w = telescope_shell::compile::compile("wheel.slint", DOC, vec![], "W").expect("the document compiles").create().unwrap();
    w.set_global_callback("Shortcuts", "bindings-for", |_| Value::Model(slint::ModelRc::new(slint::VecModel::from(Vec::<Value>::new())))).unwrap();
    w.set_global_property("Shortcuts", "available", Value::Bool(true)).unwrap();
    w.show().unwrap();
    telescope_shell::offscreen::draw(w.window(), 400.0, 300.0, 1.0);
    w
}

fn middle(w: &ComponentInstance, name: &str) -> LogicalPosition {
    let e = i_slint_backend_testing::ElementHandle::find_by_accessible_label(w, name).next().unwrap_or_else(|| panic!("no {name}"));
    let (p, s) = (e.absolute_position(), e.size());
    LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0)
}

fn wheel(w: &ComponentInstance, position: LogicalPosition) -> f64 {
    let before = w.get_property("scrolled").unwrap();
    w.window().dispatch_event(WindowEvent::PointerMoved { position });
    w.window().dispatch_event(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: -40.0 });
    telescope_shell::offscreen::pump();
    let (Value::Number(a), Value::Number(b)) = (before, w.get_property("scrolled").unwrap()) else { panic!("not a number") };
    b - a
}

fn click(w: &ComponentInstance, position: LogicalPosition, button: PointerEventButton) {
    w.window().dispatch_event(WindowEvent::PointerMoved { position });
    w.window().dispatch_event(WindowEvent::PointerPressed { position, button });
    w.window().dispatch_event(WindowEvent::PointerReleased { position, button });
    telescope_shell::offscreen::pump();
}

#[test]
fn the_wheel_scrolls_the_column_from_over_a_dropdown_or_a_slider() {
    let w = open();
    for name in ["Plain", "Tipped", "Speed"] {
        assert_eq!(wheel(&w, middle(&w, name)), -40.0, "straight over {name}");
    }
    let _ = w.hide();
}

#[test]
fn the_wheel_scrolls_the_column_after_a_tooltip_showed() {
    let w = open();
    for name in ["Tipped", "Speed"] {
        let at = middle(&w, name);
        w.window().dispatch_event(WindowEvent::PointerMoved { position: at });
        telescope_shell::offscreen::skip(std::time::Duration::from_millis(900));
        telescope_shell::offscreen::draw(w.window(), 400.0, 300.0, 1.0);
        assert_eq!(wheel(&w, at), -40.0, "after {name}'s tooltip");
    }
    let _ = w.hide();
}

#[test]
fn the_wheel_scrolls_the_column_after_a_control_was_used() {
    let w = open();
    let at = middle(&w, "Speed");
    click(&w, at, PointerEventButton::Left);
    assert_eq!(wheel(&w, at), -40.0, "after moving the slider");
    let at = middle(&w, "Plain");
    click(&w, at, PointerEventButton::Left);
    telescope_shell::offscreen::draw(w.window(), 400.0, 300.0, 1.0);
    // The first row of the open list
    click(&w, LogicalPosition::new(at.x, at.y + 32.0), PointerEventButton::Left);
    telescope_shell::offscreen::draw(w.window(), 400.0, 300.0, 1.0);
    assert_eq!(wheel(&w, at), -40.0, "after picking from the list");
    let _ = w.hide();
}
