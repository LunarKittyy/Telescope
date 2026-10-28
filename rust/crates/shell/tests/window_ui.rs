// The window built from the real plugins, driven through Slint's testing backend: nothing reaches the desktop.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_shell::window;

#[test]
fn controls_are_named_and_cards_fold_from_the_keyboard() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("window-ui");
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |bus| telescope_shell::plugins::builtins(None, bus), None, None).unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 900.0));

    for label in ["Settings", "Start streaming", "Exposure", "Stabilization", "Get set up"] {
        assert!(ElementHandle::find_by_accessible_label(&w.ui, label).next().is_some(), "nothing named {label}");
    }
    let camera: Vec<ElementHandle> = ElementHandle::find_by_accessible_label(&w.ui, "Camera").collect();
    let title = camera.iter().find(|e| e.accessible_checkable() == Some(true)).expect("the Camera title folds");
    assert_eq!(title.accessible_checked(), Some(true));
    assert_eq!(title.accessible_description().as_deref(), Some("Expanded. Press Space or Enter to collapse."));
    title.invoke_accessible_default_action();
    assert!(w.state.borrow().collapse.is_folded("camera_control/Camera"));
    assert_eq!(title.accessible_checked(), Some(false));
    assert_eq!(title.accessible_description().as_deref(), Some("Collapsed. Press Space or Enter to expand."));

    // A section folds on its own, keyed under its card
    title.invoke_accessible_default_action();
    let light = ElementHandle::find_by_accessible_label(&w.ui, "Light").find(|e| e.accessible_checkable() == Some(true)).unwrap();
    light.invoke_accessible_default_action();
    assert!(w.state.borrow().collapse.is_folded("camera_control/Camera/Light"));
}
