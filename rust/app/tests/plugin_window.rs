// A plugin's window of its own (HostUi::open_window) through the real window, on Slint's testing backend.
use serde_json::json;
use slint_interpreter::Value as SValue;
use std::sync::{Arc, Mutex};
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::host::Host;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{SlintSource, UiEvent, UiSpec, WindowSpec};
use telescope_shell::window;

const UI: &str = r#"
export component Editor inherits Window {
    title: @tr("Edit picture");
    in property <string> note;
    callback done(int);
    Text { text: note; }
}
"#;

struct Windowed(Arc<Mutex<Vec<String>>>);

impl Plugin for Windowed {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "windowed".into(), title: "Windowed".into(), ..Default::default() }
    }
    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        let ui = UiSpec { source: SlintSource::Text(UI.into()), component: "Editor".into(), props: json!({"note": "first"}) };
        host.open_window("editor", &WindowSpec { ui, size: [960.0, 600.0] });
    }
    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        self.0.lock().unwrap().push(format!("{} {} {:?}", event.ui, event.callback, event.args));
        if event.callback == "done" {
            host.set_ui_props("editor", &json!({"note": "second"}));
            host.close_window("editor");
        }
    }
}

#[test]
fn a_plugins_window_takes_props_sends_its_callbacks_and_says_when_it_closed() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("plugin-window");
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| vec![Box::new(Windowed(h)) as Box<dyn Plugin>], None, None).unwrap();
    w.process();
    let editor = w.plugin_window("windowed", "editor").expect("the plugin's window is open");
    assert_eq!(editor.get_property("note").unwrap(), SValue::String("first".into()));
    editor.invoke("done", &[SValue::Number(3.0)]).unwrap();
    assert!(w.plugin_window("windowed", "editor").is_none(), "closed");
    assert_eq!(*heard.lock().unwrap(), ["editor done [Number(3)]", "editor closed []"]);
}
