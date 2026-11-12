// The Camera card drawn with the kit's real widgets (test_camera_control.py's slider reset).
use i_slint_backend_testing::ElementHandle;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::Value;
use std::cell::RefCell;
use std::rc::Rc;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_shell::plugins::camera_control::CameraControl;

fn double_click(window: &slint::Window, e: &ElementHandle) {
    let (p, s) = (e.absolute_position(), e.size());
    let position = LogicalPosition::new(p.x + s.width / 3.0, p.y + s.height / 2.0);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    for _ in 0..2 {
        window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
        window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    }
}

#[test]
fn compensation_snaps_and_all_three_reset_on_a_double_click() {
    i_slint_backend_testing::init_no_event_loop();
    let card = CameraControl::new().card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let def = telescope_shell::compile::compile("camera.slint", &source, vec![], &card.component).expect("the card compiles");
    let ui = def.create().unwrap();
    let moved: Rc<RefCell<Vec<(&str, f64)>>> = Rc::default();
    for name in ["ae-moved", "kelvin-moved", "tint-moved"] {
        let m = moved.clone();
        ui.set_callback(name, move |args| {
            if let Some(Value::Number(v)) = args.first() {
                m.borrow_mut().push((name, *v));
            }
            Value::Void
        }).unwrap();
    }
    for (name, value) in [("active", Value::Bool(true)), ("show-manual-wb", Value::Bool(true)), ("wb-enabled", Value::Bool(true)),
                          ("ae-comp", Value::Number(3.0)), ("kelvin", Value::Number(3300.0)), ("tint", Value::Number(40.0))] {
        ui.set_property(name, value).unwrap();
    }
    ui.window().set_size(slint::LogicalSize::new(460.0, 900.0));
    ui.show().unwrap();
    for (label, callback, value) in [("Brightness", "ae-moved", 0.0), ("Temperature", "kelvin-moved", 5500.0), ("Tint", "tint-moved", 0.0)] {
        let slider = ElementHandle::find_by_accessible_label(&ui, label).find(|e| e.accessible_value_maximum().is_some()).expect("the slider");
        double_click(ui.window(), &slider);
        assert_eq!(moved.borrow().last(), Some(&(callback, value)), "{label}");
        assert!(slider.accessible_description().is_some_and(|tip| tip.ends_with("Double-click to reset")), "{label}");
    }
}
