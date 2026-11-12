// Closing the settings window tells its sections, and a question open in it keeps bindings from running behind it.
use serde_json::json;
use slint::platform::WindowEvent;
use slint::ComponentHandle;
use slint_interpreter::Value;
use std::sync::{Arc, Mutex};
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::host::Host;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{SettingsSection, SlintSource, UiEvent, UiSpec};
use telescope_shell::{settings, window};

struct Watch(Arc<Mutex<Vec<String>>>);

impl Plugin for Watch {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "watch".into(), title: "Watch".into(), ..Default::default() }
    }
    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        let source = "export component Part inherits Rectangle { height: 40px; }";
        vec![SettingsSection {
            id: "part".into(), tab: "general".into(), order: 0, tab_title: String::new(), keywords: String::new(), footer: None,
            ui: UiSpec { source: SlintSource::Text(source.into()), component: "Part".into(), props: json!({}) },
        }]
    }
    fn on_ui(&mut self, _host: &mut dyn Host, event: &UiEvent) {
        self.0.lock().unwrap().push(event.callback.clone());
    }
}

#[test]
fn closing_the_window_tells_the_sections_and_a_question_makes_it_modal() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("settings-close");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let plugin = seen.clone();
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| vec![Box::new(Watch(plugin)) as Box<dyn Plugin>], None, None).unwrap();
    settings::open(&w, "general");
    let shown = settings::current().expect("the settings window");
    assert!(!shown.modal());
    shown.ui.set_property("asking", Value::Bool(true)).unwrap();
    assert!(shown.modal(), "a confirmation inside the settings window");
    shown.ui.set_property("asking", Value::Bool(false)).unwrap();
    assert!(!shown.modal());
    seen.lock().unwrap().clear();
    shown.ui.window().dispatch_event(WindowEvent::CloseRequested);
    assert!(seen.lock().unwrap().iter().any(|c| c == "hidden"), "{:?}", seen.lock().unwrap());
    settings::forget();
}
