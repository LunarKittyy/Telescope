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
