// Plugins really are plugins (tests/test_plugin_removal.py): the window starts, streams and quits with any one of them left
// out, Connection aside. Every built-in plugin as main.rs lists them, with what reaches this computer stood in for.
mod rig;

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

thread_local! {
    /// Which computer Setup shows its rows for.
    static LINUX: Cell<bool> = const { Cell::new(cfg!(target_os = "linux")) };
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
        Box::new(telescope_setup::setup::SetupPlugin::new(outputs.clone(), link.clone(), media, "1.0", telescope_setup::setup::Language::found(&[])).for_platform(LINUX.get())),
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
        Box::new(telescope_setup::stats::StatsPlugin::new()),
        Box::new(telescope_system::mcp::McpPlugin::new(telescope_system::mcp::McpHandle::default())),
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

    let workers: Arc<Mutex<Vec<Arc<rig::FakeWorker>>>> = Arc::default();
    *w.streams.maker.borrow_mut() = Some(rig::worker_maker(workers.clone()));
    w.stream_plugin_source("test:cam");
    w.process();
    assert!(w.is_streaming(), "without {left_out:?}");
    // A worker runs from the moment it's made
    let worker = workers.lock().unwrap().last().expect("a worker").clone();
    assert!(!worker.is_stopped());
    let source = w.streams.flow.borrow().sessions.find(Some("test:cam")).expect("the stream");
    w.worker_status(&source, "ok", "Streaming");
    assert!(w.state.borrow_mut().save_now());
    assert!(!w.state.borrow().diagnostics().is_empty());
    w.stop_all_streams();
    w.process();
    assert!(!w.is_streaming());
    assert!(worker.stops());
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
fn every_card_in_the_window_folds() {
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
    // Two show only while their source is picked, which Python's findChildren doesn't mind
    for name in ["browser_camera", "webcam"] {
        w.state.borrow_mut().effects.push(telescope_shell::shell::Effect::Props { plugin: name.into(), ui: "card".into(), props: serde_json::json!({"visible": true}) });
    }
    w.process();
    let unfolding: Vec<&String> = titles.iter().filter(|t| !ElementHandle::find_by_accessible_label(&w.ui, t).any(|e| e.accessible_checkable() == Some(true))).collect();
    // The Microphone card has no rows until a phone is paired, and a card with nothing in it is not a fold
    let unfolding: Vec<&&String> = unfolding.iter().filter(|t| t.as_str() != "Microphone").collect();
    assert!(unfolding.is_empty(), "these don't fold: {unfolding:?}");
    let toggle = |label: &str, key: &str| {
        let e = ElementHandle::find_by_accessible_label(&w.ui, label).find(|e| e.accessible_checkable() == Some(true)).unwrap_or_else(|| panic!("{label}"));
        e.invoke_accessible_default_action();
        assert!(w.state.borrow().collapse.is_folded(key), "{key}");
    };
    // The cards fold by their titles now: no card keeps a heading of its own for a section
    toggle("Camera", "camera_control/Camera");
    toggle("Phone", "monitoring/Phone");
    w.shut_down();
}

#[test]
fn a_plugin_switched_off_and_on_again_has_its_folds_as_it_left_them() {
    let dir = telescope_core::testing::scratch_dir("plugin-removal-off-and-on");
    let first = build(&dir, None);
    first.state.borrow_mut().collapse.set_folded("transforms/Transforms", true);
    first.state.borrow_mut().collapse.set_folded("camera_control/Camera", true);
    assert!(first.state.borrow_mut().save_now());
    first.shut_down();

    let without = build(&dir, Some("transforms")); // Transforms is off for this run
    assert!(without.state.borrow().index("transforms").is_none());
    assert!(without.state.borrow().collapse.is_folded("camera_control/Camera"));
    assert!(without.state.borrow_mut().save_now());
    let saved = without.state.borrow().file.load();
    assert!(saved["collapsed"].as_array().unwrap().contains(&Value::String("transforms/Transforms".into())));
    without.shut_down();

    let again = build(&dir, None);
    assert!(again.state.borrow().collapse.is_folded("transforms/Transforms"));
    assert!(!again.state.borrow().collapse.is_folded("monitoring/Phone"));
    again.ui.window().set_size(slint::LogicalSize::new(1300.0, 9000.0));
    again.ui.show().unwrap();
    // Open on screen: a title's toggle is checked while what's under it shows
    let open = |label: &str| ElementHandle::find_by_accessible_label(&again.ui, label).find(|e| e.accessible_checkable() == Some(true)).unwrap_or_else(|| panic!("{label}")).accessible_checked();
    assert_eq!((open("Transforms"), open("Camera"), open("Phone")), (Some(false), Some(false), Some(true)));
    let height = |label: &str| ElementHandle::find_by_accessible_label(&again.ui, label).map(|e| e.size().height).fold(0.0, f32::max);
    assert_eq!((height("Rotation"), height("Sharpening")), (0.0, 0.0), "what's under a folded title takes up no room");
    assert!(height("Battery under") > 0.0, "the Phone card stays open");
    let title = ElementHandle::find_by_accessible_label(&again.ui, "Transforms").find(|e| e.accessible_checkable() == Some(true)).expect("its title");
    title.invoke_accessible_default_action();
    // A person's fold glides
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(400));
    assert!(height("Rotation") > 0.0, "and unfolded, it's back");
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

// test_settings_search.py: real queries against the real tabs of a window with every plugin
fn opened(name: &str) -> (Rc<Window>, Rc<telescope_shell::settings::Settings>) {
    let w = build(&telescope_core::testing::scratch_dir(name), None);
    telescope_shell::settings::open(&w, "general");
    (w, telescope_shell::settings::current().expect("the settings window"))
}

fn names(settings: &telescope_shell::settings::Settings, query: &str) -> Vec<String> {
    settings.search_results(query).into_iter().map(|e| e.name).collect()
}

/// (what someone might type, the setting that should come up, how far down the list it may be)
const FINDS: &[(&str, &str, usize)] = &[
    // the name, or a part of it
    ("hardware acceleration", "Hardware acceleration", 1),
    ("canvas", "Canvas size", 1),
    ("max zoom", "Max zoom", 1),
    ("limiter", "Limiter", 1),
    ("wait screen", "Wait screen", 1),
    ("card colours", "Card colours", 1),
    ("copy diagnostics", "Bug reports", 2),
    ("wait before", "Wait before stopping", 1),
    ("sign in", "Open Telescope when I sign in", 1),
    ("keyboard", "Keyboard shortcuts", 1),
    // typos and the other spelling
    ("hardwre accel", "Hardware acceleration", 1),
    ("reslution", "Canvas size", 2),
    ("colour", "Card colours", 2),
    ("color", "Card colours", 2),
    ("limitr", "Limiter", 1),
    ("zomm", "Max zoom", 2),
    ("notificaton", "Tell me when it stops a stream", 3),
    ("mirorr", "Mirror it, for apps that flip the camera", 2),
    ("web page", "Show the sign on a web page", 3),
    // what it does, in other words
    ("gpu", "Hardware acceleration", 1),
    ("nvidia", "Hardware acceleration", 2),
    ("hw accel", "Hardware acceleration", 1),
    ("hwaccel", "Hardware acceleration", 1),
    ("resolution", "Canvas size", 1),
    ("1080p", "Canvas size", 2),
    ("4k", "Canvas size", 3),
    ("portrait", "Canvas size", 2),
    ("aspect ratio", "Canvas size", 2),
    ("volume", "Max gain", 2),
    ("microphone boost", "Max gain", 2),
    ("quiet mic", "Max gain", 3),
    ("clipping", "Limiter", 2),
    ("distortion", "Limiter", 2),
    ("startup", "Open Telescope when I sign in", 2),
    ("run on startup", "Open Telescope when I sign in", 2),
    ("autostart", "Open Telescope when I sign in", 1),
    ("login", "Open Telescope when I sign in", 2),
    ("tray", "Open Telescope when I sign in", 3),
    ("start automatically", "Start", 3),
    ("stop when idle", "Stop", 3),
    ("delay", "Wait before stopping", 2),
    ("timeout", "Wait before stopping", 2),
    ("notify", "Tell me when it stops a stream", 3),
    ("black screen", "Wait screen", 2),
    ("custom image", "Wait screen", 3),
    ("placeholder", "Wait screen", 2),
    ("flip", "Mirror it, for apps that flip the camera", 3),
    ("theme", "Card colours", 2),
    ("monochrome", "Card colours", 1),
    ("purple", "Card colours", 2),
    ("update", "Check daily", 4),
    ("nightly", "Channel", 1),
    ("stable", "Channel", 2),
    ("auto update", "Check daily", 2),
    ("version", "This version", 2),
    ("bug report", "Bug reports", 1),
    ("logs", "Bug reports", 2),
    ("debug", "Bug reports", 2),
    ("hotkey", "Shortcuts", 2),
    ("push to talk", "Shortcuts", 3),
    ("keybinding", "Shortcuts", 2),
    ("global shortcuts", "Desktop shortcut settings", 3),
    ("install plugin", "Added plugins", 2),
    ("extensions", "Added plugins", 4),
    ("apk", "Telescope.apk", 1),
    ("install the phone app", "Telescope.apk", 2),
    // asked as a question
    ("how do i change the resolution", "Canvas size", 1),
    ("where is the gpu setting", "Hardware acceleration", 1),
    ("turn on autostart", "Open Telescope when I sign in", 2),
];

const LINUX_ONLY: &[(&str, &str, usize)] = &[
    ("kernel module", "Kernel module", 1),
    ("v4l2loopback", "Kernel module", 2),
    ("load at boot", "Load when the computer starts", 1),
    ("reboot", "Load when the computer starts", 2),
    ("virtual camera not showing", "Kernel module", 4),
];

const WINDOWS_ONLY: &[(&str, &str, usize)] = &[
    ("driver", "Driver", 1),
    ("unitycapture", "Driver", 1),
    ("adb", "ADB", 1),
    ("usb", "ADB", 3),
    ("virtual camera not showing", "Driver", 3),
];

fn finds(name: &str, linux: bool, more: &[(&str, &str, usize)]) {
    LINUX.set(linux);
    let (_w, settings) = opened(name);
    // Python's fixture has no plugin tiles or groups on its Plugins tab
    let entries: Vec<_> = settings.entries().into_iter().filter(|e| e.tab != "plugins" || e.name == "Plugins" || e.name == "Added plugins").collect();
    let misses: Vec<String> = FINDS.iter().chain(more).filter_map(|(query, wanted, within)| {
        let found: Vec<String> = telescope_shell::search::search_ex(&entries, query, 12).0.into_iter().map(|e| e.name).collect();
        let shown = &found[..found.len().min(*within)];
        (!shown.iter().any(|n| n == wanted)).then(|| format!("{query:?}: wanted {wanted:?} in the first {within}, got {:?}", &found[..found.len().min(5)]))
    }).collect();
    assert!(misses.is_empty(), "\n{}", misses.join("\n"));
    telescope_shell::settings::forget();
}

#[test]
fn finds_what_people_would_look_for_on_linux() {
    finds("search-linux", true, LINUX_ONLY);
}

#[test]
fn finds_what_people_would_look_for_on_windows() {
    finds("search-windows", false, WINDOWS_ONLY);
}

#[test]
fn nonsense_finds_nothing() {
    let (_w, settings) = opened("search-nonsense");
    for query in ["zzzzzz", "qwertyuiop", "banana bread", "xx", "kubernetes"] {
        assert_eq!(names(&settings, query), Vec::<String>::new(), "{query}");
    }
    telescope_shell::settings::forget();
}

#[test]
fn results_say_where_the_setting_is() {
    let (_w, settings) = opened("search-where");
    let first = settings.search_results("canvas size").remove(0);
    assert_eq!((first.name.as_str(), first.tab.as_str(), first.path().as_str()), ("Canvas size", "video", "Video"));
    telescope_shell::settings::forget();
}

#[test]
fn one_letter_slips_and_swapped_letters_still_find_it() {
    let (_w, settings) = opened("search-slips");
    for query in ["hardwrae", "acceleraton", "lmiter", "cavnas", "micorphone"] {
        assert!(!names(&settings, query).is_empty(), "{query}");
    }
    telescope_shell::settings::forget();
}

#[test]
fn every_setting_can_be_found_by_its_own_name() {
    let (_w, settings) = opened("search-own-name");
    let entries = settings.entries();
    assert!(entries.len() > 25, "{}", entries.len());
    let lost: Vec<&str> = entries.iter().filter(|e| !telescope_shell::search::search_ex(&entries, &e.name, 5).0.contains(e)).map(|e| e.name.as_str()).collect();
    assert!(lost.is_empty(), "{lost:?}");
    telescope_shell::settings::forget();
}
