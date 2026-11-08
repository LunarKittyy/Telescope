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
}

/// Hears every key; takes "k" and anything pressed while typing is off.
struct Keys(std::sync::Arc<std::sync::Mutex<Vec<telescope_core::ui::KeyInput>>>);

impl telescope_core::plugin::Plugin for Keys {
    fn info(&self) -> telescope_core::plugin::PluginInfo {
        telescope_core::plugin::PluginInfo { name: "keys".into(), ..Default::default() }
    }
    fn on_key(&mut self, host: &mut dyn telescope_core::host::Host, key: &telescope_core::ui::KeyInput) -> bool {
        self.0.lock().unwrap().push(key.clone());
        if key.text == "c" && !key.released {
            host.copy_text("adb kill-server");
        }
        key.text == "k"
    }
}

#[test]
fn plugins_hear_keys_before_the_focused_control_and_can_copy_text() {
    use slint::platform::WindowEvent;
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("window-keys");
    let heard: std::sync::Arc<std::sync::Mutex<Vec<telescope_core::ui::KeyInput>>> = Default::default();
    let keys = Keys(heard.clone());
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| vec![Box::new(keys)], None, None).unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 900.0));
    w.ui.show().unwrap();
    let press = |t: &str| {
        w.ui.window().dispatch_event(WindowEvent::KeyPressed { text: t.into() });
        w.ui.window().dispatch_event(WindowEvent::KeyReleased { text: t.into() });
    };
    press("k");
    let got = heard.lock().unwrap().clone();
    assert_eq!(got.iter().map(|k| (k.text.as_str(), k.released, k.typing, k.modal)).collect::<Vec<_>>(),
               [("k", false, false, false), ("k", true, false, false)]);
    press("c");
    let copied = i_slint_core::context::with_global_context(|| unreachable!(), |c| c.platform().clipboard_text(i_slint_core::platform::Clipboard::DefaultClipboard)).unwrap();
    assert_eq!(copied.as_deref(), Some("adb kill-server"));
}
