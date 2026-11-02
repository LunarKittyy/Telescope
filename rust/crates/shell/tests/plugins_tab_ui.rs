// The Plugins tab in the real settings window, driven through Slint's testing backend: nothing reaches the desktop.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_shell::{settings, switches, window};

fn built(label: &str) -> (std::rc::Rc<window::Window>, std::path::PathBuf) {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir(label);
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |bus| telescope_shell::plugins::builtins(None, bus), None, None).unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 900.0));
    (w, dir)
}

fn labelled(ui: &slint_interpreter::ComponentInstance, label: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).collect()
}

struct Named(PluginInfo);

impl Plugin for Named {
    fn info(&self) -> PluginInfo {
        self.0.clone()
    }
}

/// Connection, Camera and Presets, as the Python tests' _builtins.
fn three() -> Vec<Box<dyn Plugin>> {
    let info = |name: &str, title: &str, required: bool| PluginInfo { name: name.into(), title: title.into(), required, ..Default::default() };
    vec![info("connection", "Connection", true), info("camera_control", "Camera", false), info("presets", "Presets", false)]
        .into_iter().map(|i| Box::new(Named(i)) as Box<dyn Plugin>).collect()
}

fn switched_off(dir: &std::path::Path, names: &[&str]) {
    let file = ConfigFile::at(dir.join(CONFIG_FILENAME));
    let mut cfg = file.load();
    for name in names {
        switches::set_builtin_off(&mut cfg, name, true);
    }
    file.save(&mut cfg);
}

#[test]
fn a_built_in_plugin_switched_off_isnt_loaded_and_the_window_says_so_once() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("plugins-off-banner");
    switched_off(&dir, &["presets", "connection"]);
    let open = || window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |_| three(), None, None).unwrap();
    let w = open();
    w.process();
    let s = w.state.borrow();
    assert_eq!(s.slots.iter().map(|p| p.info.name.as_str()).collect::<Vec<_>>(), ["connection", "camera_control"], "required: loaded anyway");
    assert_eq!(s.plugins_off().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["presets"]);
    assert!(s.diagnostics().contains(&("Plugins off".to_string(), "presets".to_string())));
    drop(s);
    assert!(!labelled(&w.ui, "Presets is turned off").is_empty(), "the banner says so");
    assert!(!labelled(&w.ui, "Plugins…").is_empty(), "with a way there");
    w.ui.invoke("banner-dismissed", &[Value::String("plugins_off".into())]).unwrap();
    w.process();
    assert!(labelled(&w.ui, "Presets is turned off").is_empty());
    let again = open();
    again.process();
    assert!(again.state.borrow().banners.issue("plugins_off").is_none(), "seen: not again until another one goes off");
    assert_eq!(again.state.borrow().plugins_off().len(), 1, "it's still off");
}

#[test]
fn the_plugins_tab_says_how_many_are_off() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("plugins-tab-count");
    switched_off(&dir, &["presets", "camera_control"]);
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |_| three(), None, None).unwrap();
    settings::open(&w, "");
    let s = settings::current().unwrap();
    assert!(!labelled(&s.ui, "Plugins (2 off)").is_empty());
    settings::forget();
}

#[test]
fn the_plugins_tab_opens_without_blocking_and_a_second_open_reuses_the_window() {
    let (w, _dir) = built("plugins-tab-open");
    settings::open(&w, "plugins");
    let first = settings::current().expect("the settings window was built");
    assert!(first.ui.window().is_visible());
    assert_eq!(first.current_tab(), "plugins");
    // The main window still takes a click while it's up
    let title = labelled(&w.ui, "Camera").into_iter().find(|e| e.accessible_checkable() == Some(true)).expect("a folding title");
    let (p, size) = (title.absolute_position(), title.size());
    let position = slint::LogicalPosition::new(p.x + size.width / 2.0, p.y + size.height / 2.0);
    let button = slint::platform::PointerEventButton::Left;
    w.ui.window().dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
    w.ui.window().dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
    w.ui.window().dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
    assert_eq!(w.state.borrow().collapse.keys(), ["camera_control/Camera"]);
    settings::open(&w, "plugins");
    assert!(std::rc::Rc::ptr_eq(&first, &settings::current().unwrap()), "brought back up, not a second one");
    settings::forget();
}

#[test]
fn switching_a_plugin_off_asks_first_and_cancel_puts_the_switch_back() {
    let (w, dir) = built("plugins-tab-ask");
    settings::open(&w, "plugins");
    let s = settings::current().unwrap();
    let switch = labelled(&s.ui, "Presets").into_iter().find(|e| e.accessible_checkable() == Some(true)).expect("a switch named Presets");
    assert_eq!(switch.accessible_checked(), Some(true));
    switch.invoke_accessible_default_action();
    assert_eq!(s.ui.get_property("asking").unwrap(), Value::Bool(true));
    assert_eq!(s.ui.get_property("ask-title").unwrap(), Value::String("Switch off Presets?".into()));
    // Cancel: nothing saved, the switch is on again
    s.ui.invoke("answered", &[Value::Bool(false)]).unwrap();
    let switch = labelled(&s.ui, "Presets").into_iter().find(|e| e.accessible_checkable() == Some(true)).unwrap();
    assert_eq!(switch.accessible_checked(), Some(true));
    let saved = ConfigFile::at(dir.join(CONFIG_FILENAME)).load();
    assert!(saved.pointer("/plugins/off").is_none_or(|o| o.as_array().is_some_and(|a| a.is_empty())));
    // Agree: saved, and the footer asks for a restart
    switch.invoke_accessible_default_action();
    s.ui.invoke("answered", &[Value::Bool(true)]).unwrap();
    let saved = ConfigFile::at(dir.join(CONFIG_FILENAME)).load();
    assert_eq!(saved["plugins"]["off"], serde_json::json!(["presets"]));
    assert_eq!(s.ui.get_property("footer-shown").unwrap(), Value::Bool(true));
    assert_eq!(s.ui.get_property("footer-note").unwrap(), Value::String("Restart to apply 1 change".into()));
    settings::forget();
}
