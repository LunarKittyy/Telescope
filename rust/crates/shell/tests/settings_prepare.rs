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

mod late {
    use super::*;
    use serde_json::json;
    use telescope_core::host::Host;
    use telescope_core::plugin::PluginInfo;
    use telescope_core::ui::{SettingsSection, SlintSource, UiEvent, UiSpec};

    struct Notes;

    impl Plugin for Notes {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: "notes".into(), title: "Notes".into(), ..Default::default() }
        }
        fn settings_sections(&mut self) -> Vec<SettingsSection> {
            let source = "export component Part inherits Rectangle { in property <string> word; height: 40px; accessible-role: text; accessible-label: word; }";
            vec![SettingsSection {
                id: "part".into(), tab: "general".into(), order: 0, tab_title: String::new(), keywords: String::new(), footer: None,
                ui: UiSpec { source: SlintSource::Text(source.into()), component: "Part".into(), props: json!({"word": "old"}) },
            }]
        }
        fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
            // Not for "shown", which the window says itself when it comes up
            if event.callback == "go" {
                host.set_ui_props("part", &json!({"word": "new"}));
            }
        }
    }

    // Whether a part with this word is on the page
    fn says(shown: &settings::Settings, word: &str) -> bool {
        i_slint_backend_testing::ElementHandle::find_by_accessible_label(&shown.ui, word).next().is_some()
    }

    #[test]
    fn what_a_section_was_told_after_the_page_was_prepared_is_there_when_the_window_opens() {
        i_slint_backend_testing::init_no_event_loop();
        let dir = telescope_core::testing::scratch_dir("settings-late");
        let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |_| vec![Box::new(Notes) as Box<dyn Plugin>], None, None).unwrap();
        settings::prepare(&w);
        assert!(settings::prepared());
        // A plugin has news (the update check finding a release) before anyone opens the window
        w.state.borrow_mut().ui_event("notes", UiEvent { ui: "part".into(), callback: "go".into(), args: vec![] });
        w.process();
        settings::open(&w, "general");
        let shown = settings::current().expect("the settings window");
        assert!(says(&shown, "new"), "the news is on the page");
        assert!(!says(&shown, "old"), "not the words from when the page was prepared");
        settings::forget();
    }
}
