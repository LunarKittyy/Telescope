// A plugin's window of its own (HostUi::open_window) through the real window, on Slint's testing backend.
use serde_json::{json, Value};
use slint_interpreter::Value as SValue;
use std::sync::{Arc, Mutex};
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::event::{Event, SourceName};
use telescope_core::host::Host;
use telescope_core::frame::PixelFormat;
use telescope_core::link::{Link, PairingOffer, PhoneSensors, PhoneSession, RoutePrefs, Unreachable};
use telescope_core::media::{Codec, VideoFeed};
use telescope_core::output::{Outputs, Slot, VideoOutput};
use telescope_core::services::Services;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::source::{ControlClient, PairedPhone, StreamSource};
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

struct Offers(Arc<Mutex<Vec<Vec<SourceName>>>>);

struct Browser(&'static str);

impl StreamSource for Browser {
    fn id(&self) -> String {
        self.0.into()
    }
    fn name(&self) -> String {
        "Browser camera".into()
    }
    fn url(&self) -> String {
        "browser:".into()
    }
    fn prepare(&self, _interactive: bool) -> bool {
        true
    }
    fn open_feed(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        Err("no feed in tests".into())
    }
    fn control_client(&self) -> Option<Box<dyn ControlClient>> {
        None
    }
    fn fps(&self) -> u32 {
        24
    }
}

impl Plugin for Offers {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "offers".into(), title: "Offers".into(), ..Default::default() }
    }
    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        host.add_stream_source(Arc::new(Browser("browser:")));
    }
    fn on_event(&mut self, _host: &mut dyn Host, event: &Event) {
        if let Event::StreamSourcesChanged { sources } = event {
            self.0.lock().unwrap().push(sources.clone());
        }
    }
}

#[test]
fn sources_are_announced_on_the_bus() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("window-sources");
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| vec![Box::new(Offers(h)) as Box<dyn Plugin>], None, None).unwrap();
    w.process();
    assert_eq!(*heard.lock().unwrap(), [[SourceName { id: "browser:".into(), name: "Browser camera".into() }]]);
    w.remove_plugin_source("browser:");
    w.process();
    assert_eq!(heard.lock().unwrap().last().unwrap(), &Vec::<SourceName>::new());
}

/// Writes down what the sensors are told to follow.
#[derive(Default)]
struct Sensors(Mutex<Vec<Option<String>>>);

impl PhoneSensors for Sensors {
    fn want(&self, _owner: &str, _kinds: &[String]) {}
    fn wanted(&self) -> Vec<String> {
        Vec::new()
    }
    fn status(&self, _source_id: Option<&str>) -> Value {
        json!({})
    }
    fn reading(&self, _source_id: Option<&str>) -> Option<Value> {
        None
    }
    fn follow(&self, source_id: Option<&str>, _base: Option<&str>, _phone: Option<&PairedPhone>, _state: Option<&Value>) {
        let mut seen = self.0.lock().unwrap();
        if seen.last() != Some(&source_id.map(String::from)) {
            seen.push(source_id.map(String::from));
        }
    }
    fn diagnostics(&self) -> Value {
        json!({})
    }
    fn shutdown(&self) {}
}

struct SensorLink(Arc<Sensors>);

impl Link for SensorLink {
    fn new_identity(&self) -> (String, String) {
        Default::default()
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        unimplemented!()
    }
    fn pair(&self, _id: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        Err("no".into())
    }
    fn sensors(&self) -> Option<Arc<dyn PhoneSensors>> {
        Some(self.0.clone())
    }
}

struct Cameras;

impl Outputs for Cameras {
    fn slots(&self) -> Vec<Slot> {
        (0..4).map(|slot| Slot { slot, name: format!("Telescope {slot}"), device: String::new() }).collect()
    }
    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::Rgba]
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no cameras in tests".into())
    }
}

#[test]
fn the_sensors_follow_the_shown_stream_at_once_and_let_go_when_it_stops() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("window-sensors");
    let sensors = Arc::new(Sensors::default());
    let services = Arc::new(Services {
        link: Arc::new(SensorLink(sensors.clone())), media: Arc::new(telescope_media::FfmpegMedia), outputs: Arc::new(Cameras), audio: None,
        notifier: None, autostart: None, updater: None, tray: None, plugins: Box::new(Vec::new), vision: None, plugin_problems: Default::default(),
    });
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |_| Vec::new(), Some(services), None).unwrap();
    w.add_plugin_source(Arc::new(Browser("browser:a")));
    w.add_plugin_source(Arc::new(Browser("browser:b")));
    w.stream_plugin_source("browser:a");
    assert_eq!(*sensors.0.lock().unwrap(), [Some("browser:a".to_string())]);
    w.stream_plugin_source("browser:b");
    assert_eq!(sensors.0.lock().unwrap().last().unwrap().as_deref(), Some("browser:b"), "not waiting for its /v1/state");
    w.with_flow(|flow, host| flow.stop_stream(host, Some("browser:b")));
    w.with_flow(|flow, host| flow.stop_stream(host, Some("browser:a")));
    assert_eq!(*sensors.0.lock().unwrap().last().unwrap(), None);
}
