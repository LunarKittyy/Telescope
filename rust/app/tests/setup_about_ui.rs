// Setup's About section drawn with the kit's real widgets: the button a plugin test can only reach by its callback.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use slint_interpreter::Value;
use std::cell::Cell;
use std::rc::Rc;

const COMPONENT: &str = "AboutSettings";

#[test]
fn the_diagnostics_button_is_there_to_press() {
    i_slint_backend_testing::init_no_event_loop();
    let source = concat!(include_str!("../../crates/setup/ui/parts.slint"), "\n", include_str!("../../crates/setup/ui/setup.slint"));
    let def = telescope_shell::compile::compile("setup.slint", source, vec![], COMPONENT).expect("the section compiles");
    let ui = def.create().unwrap();
    let pressed = Rc::new(Cell::new(0));
    let p = pressed.clone();
    ui.set_callback("diag-clicked", move |_| {
        p.set(p.get() + 1);
        Value::Void
    }).unwrap();
    ui.set_property("diag-text", Value::String("Copy diagnostics".into())).unwrap();
    ui.window().set_size(slint::LogicalSize::new(460.0, 600.0));
    ui.show().unwrap();
    ElementHandle::find_by_accessible_label(&ui, "Copy diagnostics").next().expect("the button").invoke_accessible_default_action();
    assert_eq!(pressed.get(), 1);
}
