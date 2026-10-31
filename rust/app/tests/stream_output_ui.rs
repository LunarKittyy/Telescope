// The Stream output card drawn with the kit's real widgets (test_plugins.py's test_stream_quality_and_bitrate_reset_on_a_double_click).
use i_slint_backend_testing::ElementHandle;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::Value;
use std::cell::RefCell;
use std::rc::Rc;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;
use telescope_outputs::stream_output::StreamOutputPlugin;

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
fn stream_quality_and_bitrate_reset_on_a_double_click() {
    i_slint_backend_testing::init_no_event_loop();
    let card = StreamOutputPlugin::new(true).card().expect("a card").ui;
    let SlintSource::Text(source) = card.source else { panic!("source text") };
    let def = telescope_shell::compile::compile("stream_output.slint", &source, vec![], &card.component).expect("the card compiles");
    let moved: Rc<RefCell<Vec<(String, f64)>>> = Rc::default();
    for name in ["quality-moved", "bitrate-moved"] {
        let (m, n) = (moved.clone(), name.to_string());
        let ui = def.create().unwrap();
        ui.set_callback(name, move |args| {
            if let Some(Value::Number(v)) = args.first() {
                m.borrow_mut().push((n.clone(), *v));
            }
            Value::Void
        }).unwrap();
        let (label, at) = if name == "quality-moved" { ("JPEG quality", 40.0) } else { ("Bitrate", 12.0) };
        ui.set_property("h264", Value::Bool(name == "bitrate-moved")).unwrap();
        ui.set_property(if name == "quality-moved" { "quality" } else { "bitrate" }, Value::Number(at)).unwrap();
        ui.set_property("bitrate-tip", Value::String("All the way right is Dynamic".into())).unwrap();
        ui.window().set_size(slint::LogicalSize::new(460.0, 400.0));
        ui.show().unwrap();
        let slider = ElementHandle::find_by_accessible_label(&ui, label).find(|e| e.accessible_value_maximum().is_some()).expect("the slider");
        double_click(ui.window(), &slider);
        if name == "bitrate-moved" {
            assert_eq!(slider.accessible_description().as_deref(), Some("All the way right is Dynamic"), "its own tip stays");
        }
    }
    let moved = moved.borrow();
    assert_eq!(moved.iter().rev().find(|(n, _)| n == "quality-moved").map(|m| m.1), Some(85.0));
    assert_eq!(moved.iter().rev().find(|(n, _)| n == "bitrate-moved").map(|m| m.1), Some(0.0), "0 is Auto");
}
