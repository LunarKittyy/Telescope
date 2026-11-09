// Preparing the settings page ahead of time must not make a window: on Wayland a made window is a real, empty toplevel.
use slint::ComponentHandle;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::plugin::Plugin;
use telescope_shell::{settings, window};

#[test]
fn preparing_settings_makes_no_window_until_it_opens() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("settings-prepare");
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |_| Vec::<Box<dyn Plugin>>::new(), None, None).unwrap();
    settings::prepare(&w);
    assert!(settings::prepared(), "the page is compiled and waiting");
    assert!(settings::current().is_none() && settings::window().is_none(), "but no window exists yet");
    settings::open(&w, "");
    assert!(!settings::prepared(), "opening used the prepared page");
    assert!(settings::window().is_some_and(|s| s.window().is_visible()), "and the window is up");
    settings::forget();
    assert!(!settings::prepared());
}
