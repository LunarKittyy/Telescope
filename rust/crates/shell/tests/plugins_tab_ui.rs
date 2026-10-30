// The Plugins tab in the real settings window, driven through Slint's testing backend: nothing reaches the desktop.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_shell::{settings, window};

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

#[test]
fn the_plugins_tab_opens_without_blocking_and_a_second_open_reuses_the_window() {
    let (w, _dir) = built("plugins-tab-open");
    settings::open(&w, "plugins");
    let first = settings::current().expect("the settings window was built");
    assert_eq!(first.current_tab(), "plugins");
    settings::open(&w, "plugins");
    assert!(std::rc::Rc::ptr_eq(&first, &settings::current().unwrap()));
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

#[test]
fn the_list_stays_where_it_was_when_a_switch_is_changed() {
    let (w, _dir) = built("plugins-tab-scroll");
    settings::open(&w, "plugins");
    let s = settings::current().unwrap();
    let t = s.tabs().iter().position(|t| t == "plugins").unwrap();
    let offset = format!("page{t}-offset");
    s.ui.window().set_size(slint::LogicalSize::new(700.0, 500.0));
    s.ui.set_property(&offset, Value::Number(120.0)).unwrap();
    let switch = labelled(&s.ui, "Presets").into_iter().find(|e| e.accessible_checkable() == Some(true)).unwrap();
    switch.invoke_accessible_default_action();
    s.ui.invoke("answered", &[Value::Bool(true)]).unwrap();
    assert_eq!(s.ui.get_property(&offset).unwrap(), Value::Number(120.0));
    settings::forget();
}
