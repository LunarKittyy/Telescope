// The message before any window (a start that can't go on): the title and words show, and OK closes it.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use slint_interpreter::Value;

#[test]
fn a_notice_shows_its_words_and_ok_closes_it() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = telescope_shell::notice::build("Telescope can't start", "Something else is holding its lock.\n\nClose that program.").unwrap();
    ui.show().unwrap();
    let labels: Vec<String> = ElementHandle::find_by_element_type_name(&ui, "Text").filter_map(|e| e.accessible_label().map(|l| l.to_string())).collect();
    assert!(labels.iter().any(|l| l == "Telescope can't start"), "{labels:?}");
    assert!(labels.iter().any(|l| l.starts_with("Something else is holding its lock.")), "{labels:?}");
    let ok = ElementHandle::find_by_accessible_label(&ui, "OK").next().expect("an OK button");
    assert!(ui.window().is_visible());
    ok.invoke_accessible_default_action();
    assert!(!ui.window().is_visible());
    assert_eq!(ui.get_property("heading").unwrap(), Value::String("Telescope can't start".into()));
}
