// The settings window with a plugin from another crate, driven through Slint's testing backend: nothing reaches the desktop.
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_shell::window;

#[test]
fn a_settings_section_hears_its_plugin_after_the_settings_window_opens() {
    use telescope_system::shortcuts::{RemoteActions, ShortcutsPlugin};
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("window-settings-props");
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |bus| {
        let mut plugins = telescope_shell::plugins::builtins(None, bus);
        plugins.push(Box::new(ShortcutsPlugin::with_backend(RemoteActions::default(), Box::new(|_| None))));
        telescope_shell::plugins::in_order(plugins)
    }, None, None).unwrap();
    telescope_shell::settings::open(&w, "shortcuts");
    let settings = telescope_shell::settings::current().expect("the settings window");
    let open = || {
        let name = settings.ui.definition().properties().map(|p| p.0).find(|n| n.ends_with("-editor-open")).expect("the shortcuts section");
        settings.ui.get_property(&name).unwrap()
    };
    assert_eq!(open(), slint_interpreter::Value::Bool(false));
    // New shortcut: the plugin opens its editor by setting the section's props
    assert!(settings.call("shortcuts", "shortcuts", "add"));
    assert_eq!(open(), slint_interpreter::Value::Bool(true));
    telescope_shell::settings::forget();
}

struct Heard(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl telescope_core::plugin::Plugin for Heard {
    fn info(&self) -> telescope_core::plugin::PluginInfo {
        telescope_core::plugin::PluginInfo { name: "heard".into(), ..Default::default() }
    }
    fn on_key(&mut self, _host: &mut dyn telescope_core::host::Host, key: &telescope_core::ui::KeyInput) -> bool {
        if !key.released {
            self.0.lock().unwrap().push(key.text.to_string());
        }
        false
    }
}

// test_shortcuts_plugin.py::test_keys_reach_a_window_where_nothing_takes_focus: Settings is a window of its own
#[test]
fn keys_reach_a_window_where_nothing_takes_focus() {
    use slint::ComponentHandle;
    use slint::platform::WindowEvent;
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("window-settings-keys");
    let heard: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let h = heard.clone();
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| vec![Box::new(Heard(h))], None, None).unwrap();
    telescope_shell::settings::open(&w, "plugins");
    let settings = telescope_shell::settings::current().expect("the settings window");
    settings.ui.window().dispatch_event(WindowEvent::KeyPressed { text: "t".into() });
    assert_eq!(*heard.lock().unwrap(), ["t"]);
    telescope_shell::settings::forget();
}
