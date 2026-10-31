// Plugins really are plugins (tests/test_plugin_removal.py): the window starts, streams and quits with any one of them left
// out, Connection aside. Every built-in plugin as main.rs lists them, with what reaches this computer stood in for.
use i_slint_backend_testing::ElementHandle;
use serde_json::Value;
use slint::ComponentHandle;
use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::frame::PixelFormat;
use telescope_core::host::Host;
use telescope_core::link::{Link, PairingOffer, PhoneSession, RoutePrefs, Unreachable};
use telescope_core::media::{Codec, Media, VideoFeed};
use telescope_core::output::{Outputs, Slot, VideoOutput};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::services::Services;
use telescope_core::source::{ControlClient, PairedPhone, StreamSource};
use telescope_core::ui::SlintSource;
use telescope_shell::plugins::ORDER;
use telescope_shell::window::{self, Window};
use telescope_system::shortcuts::{RemoteActions, ShortcutsPlugin};
use telescope_system::updates::{Asset, InstallResult, Manifest};
use telescope_system::updates_plugin::{Backend, UpdatesPlugin};

const REQUIRED: [&str; 1] = ["connection"];

struct NoLink;

impl Link for NoLink {
    fn new_identity(&self) -> (String, String) {
        Default::default()
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        Err(Unreachable::NoAnswer)
    }
    fn pair(&self, _id: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        Err("no pairing in tests".into())
    }
}

struct OneCamera;

impl Outputs for OneCamera {
    fn slots(&self) -> Vec<Slot> {
        vec![Slot { slot: 0, name: "Telescope".into(), device: String::new() }]
    }
    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::Rgba]
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no cameras in tests".into())
    }
}

struct NoTunnels;

impl telescope_link::connection::Tunnels for NoTunnels {
    fn acquire(&self, _serial: &str, _remote_port: u16) -> Option<u16> {
        None
    }
    fn release(&self, _serial: &str, _remote_port: u16) {}
}

struct NoAutostart;

impl telescope_core::system::Autostart for NoAutostart {
    fn enabled(&self) -> bool {
        false
    }
    fn set_enabled(&self, _on: bool, _minimized: bool) -> Result<(), String> {
        Ok(())
    }
}

struct NoUpdates;

impl Backend for NoUpdates {
    fn fetch(&self, _channel: &str) -> Result<Option<Manifest>, String> {
        Ok(None)
    }
    fn install(&self, _m: &Manifest, _a: &Asset, _p: telescope_system::updates_plugin::Progress, _c: &dyn Fn() -> bool, _s: &dyn Fn() -> bool) -> Result<(i64, InstallResult), String> {
        Err(String::new())
    }
    fn blocker(&self) -> Option<String> {
        None
    }
    fn is_newer(&self, _m: Option<&Manifest>) -> bool {
        false
    }
    fn was_rolled_back(&self, _m: Option<&Manifest>) -> bool {
        false
    }
    fn forget_rolled_back(&self) {}
    fn relaunch(&self, _argv: &[String]) -> std::io::Result<()> {
        Ok(())
    }
    fn open_url(&self, _url: &str) {}
    fn threaded(&self) -> bool {
        false
    }
}

struct TestMic;

impl telescope_audio::backend::MicBackend for TestMic {
    fn pick_name(&self) -> String {
        "Test mic".into()
    }
    fn problem(&self) -> Option<telescope_audio::backend::Problem> {
        None
    }
    fn prepare(&self) -> String {
        String::new()
    }
    fn open_sink(&self) -> telescope_audio::worker::OpenSink {
        Arc::new(|| Err("no virtual microphone in tests".into()))
    }
    fn teardown(&self) {}
}

/// Every built-in plugin there is.
fn every_plugin(bus: &telescope_core::BusSender) -> Vec<Box<dyn Plugin>> {
    let link: Arc<dyn Link> = Arc::new(NoLink);
    let outputs: Arc<dyn Outputs> = Arc::new(OneCamera);
    let media: Arc<dyn Media> = Arc::new(telescope_media::FfmpegMedia);
    let pictures = telescope_effects::shared::picture_reader(media.clone());
    let web: Arc<dyn telescope_core::web::Web> = Arc::new(telescope_link::web::SystemWeb);
    let server = telescope_link::browser::plugin::system_server(web, std::env::temp_dir(), media.clone());
    let mut plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(telescope_setup::setup::SetupPlugin::new(outputs.clone(), link.clone(), media, "1.0", telescope_setup::setup::Language::found(&[]))),
        Box::new(telescope_link::plugin::ConnectionPlugin::new(link, telescope_link::shared::Shared::new(Arc::new(NoTunnels))).with_outputs(outputs.clone())),
        Box::new(telescope_link::browser::plugin::BrowserCameraPlugin::new(server, Vec::new)),
        Box::new(telescope_outputs::webcam::WebcamPlugin::system()),
        Box::new(telescope_effects::cinema::CinemaPlugin::new()),
        Box::new(telescope_effects::background::BackgroundPlugin::with_reader(pictures.clone())),
        Box::new(telescope_effects::face_exposure::FaceExposurePlugin::new()),
        Box::new(telescope_effects::chroma_key::ChromaKeyPlugin::with_reader(pictures.clone())),
        Box::new(telescope_effects::fun_pack::FunPackPlugin::new()),
        Box::new(telescope_effects::privacy_blur::PrivacyBlurPlugin::new()),
        Box::new(telescope_effects::colour_grading::ColourGradingPlugin::new()),
        Box::new(telescope_outputs::stream_output::StreamOutputPlugin::new(true)),
        Box::new(telescope_effects::transforms::TransformsPlugin::new()),
        Box::new(telescope_effects::framing::FramingPlugin::new()),
        Box::new(telescope_effects::desk::DeskPlugin::new()),
        Box::new(telescope_effects::name_tag::NameTagPlugin::new()),
        Box::new(telescope_effects::notes::NotesPlugin::new()),
        Box::new(telescope_effects::composite_plugin::CompositePlugin::with(pictures, Some(outputs.clone()))),
        Box::new(telescope_audio::plugin::MicrophonePlugin::new(Arc::new(TestMic), None).with_denoise_problem(Box::new(String::new))),
        Box::new(UpdatesPlugin::new(Arc::new(NoUpdates), "stable")),
        Box::new(telescope_outputs::wait_screen_plugin::WaitScreenPlugin::new(outputs.clone())),
        Box::new(ShortcutsPlugin::with_backend(RemoteActions::default(), Box::new(|_| None))),
        Box::new(telescope_system::startup::StartupPlugin::new(Arc::new(NoAutostart))),
        Box::new(telescope_effects::photos::PhotosPlugin::new()),
        Box::new(telescope_effects::qr_reader::QrReaderPlugin::new()),
        Box::new(telescope_effects::on_air_plugin::OnAirPlugin::new(Arc::new(telescope_link::web::SystemWeb), Some(outputs))),
        Box::new(telescope_setup::monitoring::MonitoringPlugin::new()),
    ];
    plugins.extend(telescope_shell::plugins::builtins(None, bus));
    plugins
}

/// A source of the test's own, so streaming needs neither a phone nor Browser camera.
struct TestCamera;

impl StreamSource for TestCamera {
    fn id(&self) -> String {
        "test:cam".into()
    }
    fn name(&self) -> String {
        "Test camera".into()
    }
    fn url(&self) -> String {
        "test://cam".into()
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
        30
    }
}

struct Offers;

impl Plugin for Offers {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "test_camera".into(), ..Default::default() }
    }
    fn setup(&mut self, host: &mut dyn Host) {
        host.add_stream_source(Arc::new(TestCamera));
    }
}

fn init() {
    thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
    if !DONE.replace(true) {
        i_slint_backend_testing::init_no_event_loop();
    }
}

/// A window with the built-in plugins but left_out, saving to folder.
fn build(folder: &Path, left_out: Option<&str>) -> Rc<Window> {
    init();
    let left_out = left_out.map(String::from);
    let services = Arc::new(Services {
        link: Arc::new(NoLink), media: Arc::new(telescope_media::FfmpegMedia), outputs: Arc::new(OneCamera), audio: None, notifier: None, autostart: None,
        updater: None, tray: None, plugins: Box::new(Vec::new), vision: None, plugin_problems: Default::default(), plugin_admin: None,
    });
    let file = ConfigFile::at(folder.join(CONFIG_FILENAME));
    assert!(file.update(|cfg| {
        cfg["plugins"] = serde_json::json!({"on": ORDER}); // the optional ones start out off
    }));
    let w = window::build(file, move |bus| {
        let mut plugins = every_plugin(bus);
        plugins.retain(|p| Some(p.info().name) != left_out);
        plugins.push(Box::new(Offers));
        telescope_shell::plugins::in_order(plugins)
    }, Some(services), None).unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 900.0));
    w.process();
    w
}

fn every_name() -> BTreeSet<String> {
    init();
    let bus = telescope_core::EventBus::default();
    every_plugin(&bus.sender()).iter().map(|p| p.info().name).collect()
}

struct Errors(Mutex<Vec<String>>);

impl log::Log for Errors {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Error
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            self.0.lock().unwrap().push(format!("{}", record.args()));
        }
    }
    fn flush(&self) {}
}

static ERRORS: Errors = Errors(Mutex::new(Vec::new()));

fn starts_streams_and_quits(left_out: Option<&str>) {
    let dir = telescope_core::testing::scratch_dir("plugin-removal");
    let w = build(&dir, left_out);
    let names: Vec<String> = w.state.borrow().slots.iter().map(|s| s.info.name.clone()).collect();
    assert!(left_out.is_none_or(|n| !names.iter().any(|x| x == n)));
    assert!(names.iter().any(|n| n == "connection"));

    // Every tab builds and comes up, whatever's missing
    telescope_shell::settings::open(&w, "general");
    let settings = telescope_shell::settings::current().expect("the settings window");
    let tabs = settings.tabs();
    for tab in &tabs {
        assert!(settings.show_tab(tab), "{tab}");
        w.process();
    }
    assert_eq!(tabs.iter().any(|t| t == "shortcuts"), left_out != Some("shortcuts"), "{tabs:?}");
    let has = |t: &str| tabs.iter().any(|x| x == t);
    assert_eq!(has("video") && has("audio") && has("devices"), left_out != Some("setup"), "{tabs:?}");
    assert!(has("plugins"));
    telescope_shell::settings::forget();

    w.stream_plugin_source("test:cam");
    w.process();
    assert!(w.is_streaming(), "without {left_out:?}");
    assert!(w.state.borrow_mut().save_now());
    assert!(!w.state.borrow().diagnostics().is_empty());
    w.stop_all_streams();
    w.process();
    assert!(!w.is_streaming());
    w.shut_down();
}

#[test]
fn the_app_starts_streams_and_quits_without_any_one_optional_plugin() {
    let _ = log::set_logger(&ERRORS);
    log::set_max_level(log::LevelFilter::Error);
    let mut all: Vec<Option<String>> = vec![None];
    all.extend(every_name().into_iter().filter(|n| !REQUIRED.contains(&n.as_str())).map(Some));
    let queue = Arc::new(Mutex::new(all));
    let helpers: Vec<_> = (0..3).map(|_| {
        let queue = queue.clone();
        std::thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(move || loop {
            let Some(left_out) = queue.lock().unwrap().pop() else { return };
            starts_streams_and_quits(left_out.as_deref());
        }).unwrap()
    }).collect();
    for helper in helpers {
        helper.join().unwrap();
    }
    // The host logs a plugin hook that panics instead of letting it through, so the log is where breakage shows
    assert_eq!(*ERRORS.0.lock().unwrap(), Vec::<String>::new());
}

#[test]
fn every_card_and_section_in_the_window_folds() {
    let dir = telescope_core::testing::scratch_dir("plugin-removal-folds");
    let w = build(&dir, None);
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 9000.0)); // a card scrolled out of view can't be found
    w.ui.show().unwrap();
    let titles: Vec<String> = {
        let st = w.state.borrow();
        st.cards().into_iter().filter_map(|i| {
            let card = &st.slots[i].card.as_ref()?.ui;
            let SlintSource::Text(text) = &card.source else { return None };
            telescope_shell::source::heading(text, &card.component)
        }).collect()
    };
    assert!(titles.len() > 20, "{titles:?}");
    // These two show only while their source is picked
    let unfolding: Vec<&String> = titles.iter().filter(|t| !["Browser camera", "Webcam"].contains(&t.as_str()) && !ElementHandle::find_by_accessible_label(&w.ui, t).any(|e| e.accessible_checkable() == Some(true))).collect();
    assert!(unfolding.is_empty(), "these don't fold: {unfolding:?}");
    let toggle = |label: &str, key: &str| {
        let e = ElementHandle::find_by_accessible_label(&w.ui, label).find(|e| e.accessible_checkable() == Some(true)).unwrap_or_else(|| panic!("{label}"));
        e.invoke_accessible_default_action();
        assert!(w.state.borrow().collapse.is_folded(key), "{key}");
    };
    toggle("Light", "camera_control/Camera/Light");
    toggle("Image", "camera_control/Camera/Image");
    toggle("Warnings", "monitoring/Phone/Warnings");
    w.shut_down();
}

#[test]
fn a_plugin_switched_off_and_on_again_has_its_folds_as_it_left_them() {
    let dir = telescope_core::testing::scratch_dir("plugin-removal-off-and-on");
    let first = build(&dir, None);
    first.state.borrow_mut().collapse.set_folded("transforms/Transforms", true);
    first.state.borrow_mut().collapse.set_folded("camera_control/Camera/Image", true);
    assert!(first.state.borrow_mut().save_now());
    first.shut_down();

    let without = build(&dir, Some("transforms")); // Transforms is off for this run
    assert!(without.state.borrow().index("transforms").is_none());
    assert!(without.state.borrow().collapse.is_folded("camera_control/Camera/Image"));
    assert!(without.state.borrow_mut().save_now());
    let saved = without.state.borrow().file.load();
    assert!(saved["collapsed"].as_array().unwrap().contains(&Value::String("transforms/Transforms".into())));
    without.shut_down();

    let again = build(&dir, None);
    assert!(again.state.borrow().collapse.is_folded("transforms/Transforms"));
    assert!(!again.state.borrow().collapse.is_folded("camera_control/Camera/Light"));
    again.shut_down();
}

#[test]
fn only_connection_is_required() {
    let names = every_name();
    assert_eq!(names, ORDER.iter().map(|n| n.to_string()).collect(), "the built-in plugins are the Python app's");
    assert!(REQUIRED.iter().all(|n| names.contains(*n)));
    assert_eq!(names.len() - REQUIRED.len(), ORDER.len() - 1);
}

#[test]
fn every_built_in_plugin_says_what_it_is_for_the_plugins_tab() {
    init();
    let bus = telescope_core::EventBus::default();
    let infos: Vec<PluginInfo> = every_plugin(&bus.sender()).iter().map(|p| p.info()).collect();
    for p in &infos {
        assert!(!p.title.is_empty() && !p.about.is_empty(), "{}", p.name);
        assert!(p.required || !p.when_off.is_empty(), "{}: say what goes away while it's off", p.name);
    }
    let required: BTreeSet<&str> = infos.iter().filter(|p| p.required).map(|p| p.name.as_str()).collect();
    assert_eq!(required, BTreeSet::from(["connection", "setup", "updates"]));
}
