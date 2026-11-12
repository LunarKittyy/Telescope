// A plugin's window of its own (HostUi::open_window) through the real window, on Slint's testing backend.
use serde_json::{json, Value};
use slint_interpreter::{ComponentHandle, Value as SValue};
use slint::Model;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::event::{Event, SourceName};
use telescope_core::host::{Host, PreviewLayer, RequestId, Shape};
use telescope_core::frame::PixelFormat;
use telescope_core::link::{Link, PairingOffer, PhoneSensors, PhoneSession, RoutePrefs, Unreachable};
use telescope_core::media::{Codec, VideoFeed};
use telescope_core::output::{Outputs, SetupAction, SetupResult, Slot, VideoOutput};
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

/// One picture with its editor opened once the plugins are up.
struct Editing(telescope_effects::composite_plugin::CompositePlugin, Arc<Mutex<Vec<String>>>);

impl Plugin for Editing {
    fn info(&self) -> PluginInfo {
        PluginInfo { optional: false, ..self.0.info() }
    }
    fn setup(&mut self, host: &mut dyn Host) {
        self.0.setup(host);
    }
    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.0.open_editor(host);
    }
    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        self.1.lock().unwrap().push(event.callback.clone());
        self.0.on_ui(host, event);
    }
}

fn editing(name: &str) -> (Rc<window::Window>, Arc<Mutex<Vec<String>>>) {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir(name);
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| {
        vec![Box::new(Editing(telescope_effects::composite_plugin::CompositePlugin::new(), h)) as Box<dyn Plugin>]
    }, None, None).unwrap();
    w.process();
    (w, heard)
}

// test_composite.py::test_a_closed_editor_is_freed
#[test]
fn a_closed_editor_is_freed() {
    use slint::ComponentHandle as _;
    let (w, heard) = editing("composite-editor-closed");
    let editor = w.plugin_window("composite", "editor").expect("the editor is open");
    let weak = editor.as_weak();
    editor.window().dispatch_event(slint::platform::WindowEvent::CloseRequested);
    drop(editor);
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(2));
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(10));
        slint::platform::update_timers_and_animations();
    }
    assert!(w.plugin_window("composite", "editor").is_none());
    assert!(weak.upgrade().is_none(), "the window itself is gone");
    assert!(heard.lock().unwrap().contains(&"closed".to_string()));
}

// test_composite.py::test_starting_from_a_layout_and_saving_it: Rename and Delete work once the picture is a saved layout
#[test]
fn rename_and_delete_come_on_with_a_saved_layout() {
    use i_slint_backend_testing::ElementHandle;
    let (w, _heard) = editing("composite-editor-buttons");
    let editor = w.plugin_window("composite", "editor").expect("the editor is open");
    let asking = || editor.get_property("asking-name").unwrap();
    let click = |label: &str| {
        use slint::ComponentHandle as _;
        let button = ElementHandle::find_by_accessible_label(&editor, label).next().unwrap_or_else(|| panic!("no {label}"));
        let (p, s) = (button.absolute_position(), button.size());
        let (position, left) = (slint::LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0), slint::platform::PointerEventButton::Left);
        editor.window().dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
        editor.window().dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button: left });
        editor.window().dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button: left });
        w.process();
    };
    click("Rename");
    assert_eq!(asking(), SValue::Bool(false), "nothing saved to rename");
    editor.invoke("save-clicked", &[]).unwrap();
    w.process();
    assert_eq!(asking(), SValue::Bool(true));
    editor.invoke("name-entered", &[SValue::String("Talk".into())]).unwrap();
    w.process();
    assert_eq!((asking(), editor.get_property("saved").unwrap()), (SValue::Bool(false), SValue::Bool(true)));
    click("Rename");
    assert_eq!(asking(), SValue::Bool(true), "Rename asks for a name now");
    editor.invoke("name-cancelled", &[]).unwrap();
    w.process();
    click("Delete");
    assert_eq!(editor.get_property("saved").unwrap(), SValue::Bool(false), "and Delete takes it away");
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

#[derive(Default)]
struct Cameras {
    /// What it was asked to set up.
    asked: Mutex<Vec<SetupAction>>,
    /// Only the main camera is there.
    just_one: AtomicBool,
}

impl Outputs for Cameras {
    fn slots(&self) -> Vec<Slot> {
        let count = if self.just_one.load(Ordering::SeqCst) { 1 } else { 4 };
        (0..count).map(|slot| Slot { slot, name: format!("Telescope {slot}"), device: String::new() }).collect()
    }
    fn set_up(&self, action: SetupAction, _progress: &dyn Fn(&str)) -> SetupResult {
        self.asked.lock().unwrap().push(action);
        SetupResult { ok: true, message: String::new(), command: None }
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
        link: Arc::new(SensorLink(sensors.clone())), media: Arc::new(telescope_media::FfmpegMedia), outputs: Arc::new(Cameras::default()), audio: None,
        notifier: None, autostart: None, updater: None, tray: None, plugins: Box::new(Vec::new), vision: None, plugin_problems: Default::default(), plugin_admin: None,
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

/// What the plugin that asked hears.
struct Asks(Arc<Mutex<Vec<(bool, String)>>>);

impl Plugin for Asks {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "asks".into(), title: "Asks".into(), ..Default::default() }
    }
    fn on_event(&mut self, _host: &mut dyn Host, event: &Event) {
        if let Event::RequestDone { ok, message, .. } = event {
            self.0.lock().unwrap().push((*ok, message.clone()));
        }
    }
}

type Heard = Arc<Mutex<Vec<(bool, String)>>>;

fn camera_window(name: &str) -> (Rc<window::Window>, Arc<Cameras>, Heard) {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir(name);
    let cameras = Arc::new(Cameras::default());
    let services = Arc::new(Services {
        link: Arc::new(SensorLink(Arc::default())), media: Arc::new(telescope_media::FfmpegMedia), outputs: cameras.clone(), audio: None,
        notifier: None, autostart: None, updater: None, tray: None, plugins: Box::new(Vec::new), vision: None, plugin_problems: Default::default(), plugin_admin: None,
    });
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| vec![Box::new(Asks(h)) as Box<dyn Plugin>], Some(services), None).unwrap();
    w.add_plugin_source(Arc::new(Browser("browser:a")));
    w.add_plugin_source(Arc::new(Browser("browser:b")));
    (w, cameras, heard)
}

/// How many streams run, and whether one starts again once the canvas restart is through.
fn streams(w: &Rc<window::Window>) -> (usize, bool) {
    let mut out = (0, false);
    w.with_flow(|flow, _| out = (flow.sessions.streams.len(), flow.sessions.canvas_restart));
    out
}

fn set_up_asked(cameras: &Cameras, action: SetupAction) -> bool {
    for _ in 0..200 {
        if cameras.asked.lock().unwrap().contains(&action) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

#[test]
fn the_canvas_waits_until_only_one_camera_streams_and_then_the_cameras_reload() {
    let (w, cameras, heard) = camera_window("window-canvas");
    w.stream_plugin_source("browser:a");
    w.stream_plugin_source("browser:b");
    w.restart_canvas("asks", RequestId(1), true);
    w.process();
    assert_eq!(*heard.lock().unwrap(), [(false, "Stop all but one camera first, then apply again".to_string())]);
    assert!(cameras.asked.lock().unwrap().is_empty());
    assert_eq!(streams(&w), (2, false), "both carry on");
    w.with_flow(|flow, host| flow.stop_stream(host, Some("browser:b")));
    w.restart_canvas("asks", RequestId(2), true);
    assert_eq!(streams(&w), (0, true), "it stops for the restart, to start again after");
    if cfg!(target_os = "linux") {
        assert!(set_up_asked(&cameras, SetupAction::Reload));
    }
}

#[test]
fn extra_cameras_go_only_once_nothing_streams_to_them() {
    let (w, cameras, heard) = camera_window("window-extras");
    w.stream_plugin_source("browser:a");
    w.stream_plugin_source("browser:b");
    w.remove_extras("asks", RequestId(1));
    w.process();
    assert_eq!(heard.lock().unwrap().last().unwrap(), &(false, "Stop the streams going to them first".to_string()));
    w.with_flow(|flow, host| flow.stop_stream(host, Some("browser:b")));
    w.remove_extras("asks", RequestId(2));
    assert!(set_up_asked(&cameras, SetupAction::RemoveSlots));
    cameras.just_one.store(true, Ordering::SeqCst);
    w.remove_extras("asks", RequestId(3));
    w.process();
    assert_eq!(heard.lock().unwrap().last().unwrap(), &(false, "There are none to remove".to_string()));
}

/// Hears what happens around a start, in order.
struct Order(Arc<Mutex<Vec<String>>>);

impl Plugin for Order {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "order".into(), title: "Order".into(), ..Default::default() }
    }
    fn on_event(&mut self, _host: &mut dyn Host, event: &Event) {
        match event {
            Event::SelectSource { source_id } => self.0.lock().unwrap().push(format!("select {source_id}")),
            Event::StreamStarted { url } => self.0.lock().unwrap().push(format!("started {url}")),
            _ => {}
        }
    }
}

#[test]
fn a_try_again_picks_the_failed_source_before_it_starts_so_its_settings_are_in() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("window-try-again");
    let services = Arc::new(Services {
        link: Arc::new(SensorLink(Arc::default())), media: Arc::new(telescope_media::FfmpegMedia), outputs: Arc::new(Cameras::default()), audio: None,
        plugin_admin: None, notifier: None, autostart: None, updater: None, tray: None, plugins: Box::new(Vec::new), vision: None, plugin_problems: Default::default(),
    });
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| vec![Box::new(Order(h)) as Box<dyn Plugin>], Some(services), None).unwrap();
    w.add_plugin_source(Arc::new(Browser("browser:a")));
    w.start_source(Some("browser:a".into()), None);
    w.process();
    assert_eq!(*heard.lock().unwrap(), ["select browser:a", "started browser:"]);
}

/// Hears the points the preview picks.
struct Focus(Arc<Mutex<Vec<(f32, f32)>>>);

impl Plugin for Focus {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "focus".into(), title: "Focus".into(), ..Default::default() }
    }
    fn on_event(&mut self, _host: &mut dyn Host, event: &Event) {
        if let Event::FocusPointPicked { x, y } = event {
            self.0.lock().unwrap().push((*x, *y));
        }
    }
}

#[test]
fn a_real_click_on_the_stage_comes_out_as_a_focus_point() {
    use slint::platform::{PointerEventButton, WindowEvent};
    use slint::{LogicalPosition, LogicalSize};
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("window-focus");
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| {
        vec![Box::new(Focus(h)) as Box<dyn Plugin>, Box::new(telescope_shell::plugins::preview::Preview::default())]
    }, None, None).unwrap();
    w.ui.window().set_size(LogicalSize::new(1200.0, 800.0));
    w.ui.set_property("p1-has-frame", SValue::Bool(true)).unwrap();
    w.state.borrow_mut().bus.emit(Event::FocusPointAvailable { available: true });
    w.process();
    let at = LogicalPosition::new(600.0, 300.0);
    let window = w.ui.window();
    window.dispatch_event(WindowEvent::PointerMoved { position: at });
    window.dispatch_event(WindowEvent::PointerPressed { position: at, button: PointerEventButton::Left });
    window.dispatch_event(WindowEvent::PointerReleased { position: at, button: PointerEventButton::Left });
    w.process();
    let heard = heard.lock().unwrap();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert!((0.0..=1.0).contains(&heard[0].0) && (0.0..=1.0).contains(&heard[0].1), "{heard:?}");
    assert_eq!(w.ui.get_property("p1-marker-shown").unwrap(), SValue::Bool(true));
}

/// Keeps an outline on the preview, as QR codes does.
struct Outlines;

impl Plugin for Outlines {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "outlines".into(), title: "Outlines".into(), ..Default::default() }
    }
    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        let points = vec![[0.25, 0.25], [0.75, 0.25], [0.75, 0.75], [0.25, 0.75]];
        host.add_preview_layer("outlines", PreviewLayer { shapes: vec![Shape::Polygon { points, stroke: "#5fc98c".into(), width: 3.0, fill: None }] });
    }
}

#[test]
fn a_layer_on_the_preview_moves_to_the_pop_out() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("window-layers");
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| {
        vec![Box::new(Outlines) as Box<dyn Plugin>, Box::new(telescope_shell::plugins::preview::Preview::default())]
    }, None, None).unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
    w.process();
    w.paint_layers();
    let paths = |ui: &slint_interpreter::ComponentInstance, name: &str| match ui.get_property(name) {
        Ok(SValue::Model(m)) => m.row_count(),
        other => panic!("{other:?}"),
    };
    assert_eq!(paths(&w.ui, "p1-layer-paths"), 1);
    assert!(w.call_ui("preview", "card", "pop-out"));
    w.paint_preview();
    w.paint_layers();
    let popout = w.popout_window().expect("it opened");
    assert_eq!(paths(&popout, "layer-paths"), 1);
}

/// Hears Connection's two events.
struct Picks(Arc<Mutex<Vec<String>>>);

impl Plugin for Picks {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "picks".into(), title: "Picks".into(), ..Default::default() }
    }
    fn on_event(&mut self, _host: &mut dyn Host, event: &Event) {
        match event {
            Event::SelectSource { source_id } => self.0.lock().unwrap().push(format!("select {source_id}")),
            Event::SourceShown { source_id } => self.0.lock().unwrap().push(format!("shown {source_id}")),
            _ => {}
        }
    }
}

fn picks_window(name: &str) -> (Rc<window::Window>, Arc<Mutex<Vec<String>>>) {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir(name);
    let services = Arc::new(Services {
        link: Arc::new(SensorLink(Arc::default())), media: Arc::new(telescope_media::FfmpegMedia), outputs: Arc::new(Cameras::default()), audio: None,
        notifier: None, autostart: None, updater: None, tray: None, plugins: Box::new(Vec::new), vision: None, plugin_problems: Default::default(), plugin_admin: None,
    });
    let heard = Arc::new(Mutex::new(Vec::new()));
    let h = heard.clone();
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| vec![Box::new(Picks(h)) as Box<dyn Plugin>], Some(services), None).unwrap();
    (w, heard)
}

#[test]
fn moving_the_panels_moves_the_selection_with_them() {
    let (w, heard) = picks_window("window-focus-selection");
    w.add_plugin_source(Arc::new(Browser("browser:a")));
    w.add_plugin_source(Arc::new(Browser("browser:b")));
    w.stream_plugin_source("browser:a");
    w.stream_plugin_source("browser:b");
    w.with_flow(|flow, host| flow.focus_stream(host, "browser:a"));
    w.process();
    assert_eq!(w.state.borrow().selected.as_deref(), Some("browser:a"));
    assert_eq!(heard.lock().unwrap().last().map(String::as_str), Some("shown browser:a"));
    w.with_flow(|flow, host| flow.focus_stream(host, "browser:b"));
    w.process();
    assert_eq!(w.state.borrow().selected.as_deref(), Some("browser:b"), "a save now files the panels' settings under b");
}

#[test]
fn removing_the_picked_source_picks_the_next_through_connection_first() {
    let (w, heard) = picks_window("window-remove-selected");
    w.add_plugin_source(Arc::new(Browser("browser:a")));
    w.add_plugin_source(Arc::new(Browser("browser:b")));
    w.state.borrow_mut().selected = Some("browser:a".into());
    w.remove_plugin_source("browser:a");
    assert_eq!(heard.lock().unwrap().as_slice(), ["select browser:b"]);
    assert_eq!(w.state.borrow().selected.as_deref(), Some("browser:b"));
}

#[test]
fn the_phone_card_folds_away_under_its_own_heading() {
    i_slint_backend_testing::init_no_event_loop();
    let dir = telescope_core::testing::scratch_dir("window-warnings");
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), |_| vec![Box::new(telescope_setup::monitoring::MonitoringPlugin::new()) as Box<dyn Plugin>], None, None).unwrap();
    w.ui.show().unwrap();
    let heading = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&w.ui, "Phone").find(|e| e.accessible_checkable() == Some(true)).expect("Phone folds");
    heading.invoke_accessible_default_action();
    assert_eq!(w.state.borrow().collapse.keys(), ["monitoring/Phone"]);
}
