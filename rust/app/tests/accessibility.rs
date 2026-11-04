// What a screen reader gets from the window: every control says what it is, so "Off" is never all there is to hear. The
// window is built with every plugin (and the example plugin from the plugins folder) and read through Slint's testing
// backend, which sees the same accessible tree AccessKit hands to Orca, NVDA and Narrator. Nothing reaches the desktop.
mod rig;

use i_slint_backend_testing::ElementHandle;

use slint::ComponentHandle;
use std::cell::Cell;

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

use telescope_shell::plugins::ORDER;
use telescope_shell::window::{self, Window};
use telescope_system::shortcuts::{RemoteActions, ShortcutsPlugin};
use telescope_system::updates::{Asset, InstallResult, Manifest};
use telescope_system::updates_plugin::{Backend, UpdatesPlugin};

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

/// The example plugin as the plugins folder hands it over, switched on.
fn installed(root: &Path) -> Vec<Box<dyn Plugin>> {
    use telescope_plugin_host::package::PluginFolder;
    use telescope_plugin_host::switches::Switches;
    let folder = root.join("plugins");
    std::fs::create_dir_all(&folder).unwrap();
    #[cfg(unix)]
    for path in [root, folder.as_path()] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/plugin-host/example-plugins/black_and_white");
    let target = folder.join("black_and_white");
    std::fs::create_dir_all(target.join("shaders")).unwrap();
    for entry in std::fs::read_dir(&example).unwrap().flatten() {
        let to = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&to).unwrap();
            for inner in std::fs::read_dir(entry.path()).unwrap().flatten() {
                std::fs::copy(inner.path(), to.join(inner.file_name())).unwrap();
            }
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
    let p = PluginFolder::new(&folder, Switches::new(ConfigFile::at(root.join("telescope_config.json"))), []);
    let [found] = &p.find_installed()[..] else { panic!("one plugin") };
    p.set_enabled(found, true).unwrap();
    let (loaded, failed) = p.load_enabled("en");
    assert!(failed.is_empty(), "{failed:?}");
    loaded
}

/// A window with every built-in plugin and the example one, saving to folder.
fn build(folder: &Path) -> Rc<Window> {
    init();
    let services = Arc::new(Services {
        link: Arc::new(NoLink), media: Arc::new(telescope_media::FfmpegMedia), outputs: Arc::new(OneCamera), audio: None, notifier: None, autostart: None,
        updater: None, tray: None, plugins: Box::new(Vec::new), vision: None, plugin_problems: Default::default(), plugin_admin: None,
    });
    let file = ConfigFile::at(folder.join(CONFIG_FILENAME));
    assert!(file.update(|cfg| {
        cfg["plugins"] = serde_json::json!({"on": ORDER}); // the optional ones start out off
    }));
    let added = Mutex::new(Some(installed(folder)));
    let w = window::build(file, move |bus| {
        let mut plugins = every_plugin(bus);
        plugins.push(Box::new(Offers));
        plugins.extend(added.lock().unwrap().take().unwrap_or_default());
        telescope_shell::plugins::in_order(plugins)
    }, Some(services), None).unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 900.0));
    w.process();
    w
}

use i_slint_backend_testing::{AccessibleRole, ElementRoot};
use std::collections::BTreeMap;

/// Roles a person can land on with Tab or the arrow keys.
fn focusable(role: AccessibleRole) -> bool {
    use AccessibleRole::*;
    matches!(role, Button | Switch | Checkbox | Slider | Combobox | Spinbox | TextInput | Tab | ListItem)
}

/// What a bare value says on its own: On, Off, a number, a dash.
fn only_a_value(label: &str) -> bool {
    let l = label.trim().to_lowercase();
    ["on", "off", "yes", "no", "true", "false", "auto", "none", "-", ""].contains(&l.as_str()) || l.chars().all(|c| c.is_ascii_digit() || " .,%+-\u{b0}\u{d7}".contains(c))
}

fn centre(e: &ElementHandle) -> (f32, f32) {
    let (p, s) = (e.absolute_position(), e.size());
    (p.x + s.width / 2.0, p.y + s.height / 2.0)
}

/// Everything wrong with what a screen reader hears in this window, one line each. A control is read together with the card
/// it sits in, so a name only has to be told apart from the others in its own card.
fn problems(ui: &slint_interpreter::ComponentInstance, window: &str) -> Vec<String> {
    let all = ui.root_element().query_descendants().find_all();
    let cards: Vec<(String, (f32, f32, f32, f32))> = all.iter()
        .filter(|e| e.accessible_role() == Some(AccessibleRole::Groupbox) && e.accessible_id().as_deref() == Some("card"))
        .map(|e| {
            let (p, s) = (e.absolute_position(), e.size());
            (e.accessible_label().unwrap_or_default().to_string(), (p.x, p.y, s.width, s.height))
        })
        .collect();
    let card_of = |e: &ElementHandle| -> String {
        let (x, y) = centre(e);
        cards.iter().filter(|(_, (cx, cy, w, h))| x >= *cx && x <= cx + w && y >= *cy && y <= cy + h)
            .min_by(|a, b| (a.1 .2 * a.1 .3).total_cmp(&(b.1 .2 * b.1 .3)))
            .map_or_else(String::new, |c| c.0.clone())
    };
    let mut found = Vec::new();
    let mut seen: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for e in &all {
        let Some(role) = e.accessible_role() else { continue };
        let label = e.accessible_label().unwrap_or_default().to_string();
        let value = e.accessible_value().unwrap_or_default().to_string();
        let at = format!("{window}, {} {:?} \"{label}\"", card_of(e), role);
        if role == AccessibleRole::Image && label.is_empty() {
            found.push(format!("{at}: a picture with nothing to say is announced (give it accessible-role: none)"));
        }
        if !focusable(role) {
            continue;
        }
        if label.trim().is_empty() {
            found.push(format!("{at}: no name"));
        } else if only_a_value(&label) {
            found.push(format!("{at}: the name is only a value"));
        } else {
            *seen.entry((card_of(e), format!("{role:?}"), label.clone())).or_default() += 1;
        }
        if matches!(role, AccessibleRole::Switch | AccessibleRole::Checkbox) && e.accessible_checked().is_none() {
            found.push(format!("{at}: on or off isn't exposed"));
        }
        if role == AccessibleRole::Slider && value.trim().is_empty() {
            found.push(format!("{at}: a slider with no value to speak"));
        }
        if role == AccessibleRole::Slider && value.trim().is_empty() == false && only_a_value(&value) && e.accessible_value_minimum().is_none() {
            found.push(format!("{at}: a number with no range"));
        }
    }
    // A switch is named by its own label, so a text beside it that says the same thing would be heard twice
    for sw in all.iter().filter(|e| e.accessible_role() == Some(AccessibleRole::Switch)) {
        let name = sw.accessible_label().unwrap_or_default().trim().to_lowercase();
        let (sp, ss) = (sw.absolute_position(), sw.size());
        if name.is_empty() {
            continue;
        }
        let twice = all.iter().any(|t| {
            let (tp, ts) = (t.absolute_position(), t.size());
            t.accessible_role() == Some(AccessibleRole::Text)
                && t.accessible_label().unwrap_or_default().trim().to_lowercase() == name
                && tp.y < sp.y + ss.height && sp.y < tp.y + ts.height
                && (tp.x - sp.x).abs() < 700.0
        });
        if twice {
            found.push(format!("{window}, {} Switch \"{}\": its text beside it is read as well, so the name is heard twice", card_of(sw), sw.accessible_label().unwrap_or_default()));
        }
    }
    for ((card, role, label), n) in seen {
        if n > 1 {
            found.push(format!("{window}, {card} {role} \"{label}\": {n} of them in one card, which one is it?"));
        }
    }
    found
}

#[test]
fn every_control_in_every_card_says_what_it_is() {
    let dir = telescope_core::testing::scratch_dir("a11y");
    let w = build(&dir);
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 9000.0)); // a card scrolled out of view has no place to be found at
    w.ui.show().unwrap();
    for name in ["browser_camera", "webcam"] {
        w.state.borrow_mut().effects.push(telescope_shell::shell::Effect::Props { plugin: name.into(), ui: "card".into(), props: serde_json::json!({"visible": true}) });
    }
    w.process();
    // Open every fold, so what's under a heading is in the tree too
    for _ in 0..3 {
        for e in w.ui.root_element().query_descendants().match_predicate(|e| e.accessible_checkable() == Some(true) && e.accessible_checked() == Some(false) && e.accessible_role() == Some(AccessibleRole::Button)).find_all() {
            e.invoke_accessible_default_action();
        }
    }
    let mut found = problems(&w.ui, "main window");
    let names: Vec<String> = w.state.borrow().slots.iter().map(|s| s.info.name.clone()).collect();
    assert!(names.iter().any(|n| n == "black_and_white"), "the example plugin's card is in the window: {names:?}");
    assert!(ElementHandle::find_by_accessible_label(&w.ui, "Black and white").next().is_some());

    telescope_shell::settings::open(&w, "general");
    let settings = telescope_shell::settings::current().expect("the settings window");
    settings.ui.window().set_size(slint::LogicalSize::new(940.0, 9000.0));
    for tab in settings.tabs() {
        assert!(settings.show_tab(&tab), "{tab}");
        w.process();
        found.extend(problems(&settings.ui, &format!("settings tab {tab}")));
    }
    telescope_shell::settings::forget();
    w.shut_down();
    found.sort();
    found.dedup();
    assert!(found.is_empty(), "{} things a screen reader can't make sense of:\n{}", found.len(), found.join("\n"));
}

/// Every piece of text the accessibility tree of this window carries, as the screen reader backends build it: only items
/// with an accessible property are nodes (the testing backend's ElementHandle also shows plain text items).
fn spoken(ui: &slint_interpreter::ComponentInstance) -> Vec<String> {
    use i_slint_core::accessibility::{accessible_descendents, AccessibleStringProperty as P};
    use i_slint_core::item_tree::ItemRc;
    fn walk(item: &ItemRc, said: &mut Vec<String>) {
        for node in accessible_descendents(item) {
            for p in [P::Label, P::Value, P::Description, P::Id, P::PlaceholderText] {
                said.extend(node.accessible_string_property(p).map(|s| s.to_string()));
            }
            walk(&node, said);
        }
    }
    let root = ItemRc::new_root(i_slint_core::window::WindowInner::from_pub(ui.window()).component());
    let mut said = Vec::new();
    walk(&root, &mut said);
    said
}

#[test]
fn search_keywords_never_reach_a_screen_reader() {
    let w = build(&telescope_core::testing::scratch_dir("a11y-keywords"));
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 9000.0));
    w.ui.show().unwrap();
    w.process();
    let mut tree = spoken(&w.ui);
    telescope_shell::settings::open(&w, "general");
    let settings = telescope_shell::settings::current().expect("the settings window");
    settings.ui.window().set_size(slint::LogicalSize::new(940.0, 9000.0));
    for tab in settings.tabs() {
        assert!(settings.show_tab(&tab), "{tab}");
        w.process();
        tree.extend(spoken(&settings.ui));
    }
    let keywords: Vec<String> = settings.entries().into_iter().flat_map(|e| [e.keywords, e.card_keywords, e.tab_keywords]).map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
    assert!(keywords.len() > 20, "the search has keywords to hide: {}", keywords.len());
    assert!(tree.len() > 200, "the tree was read: {} strings", tree.len());
    let mut leaked: Vec<String> = tree.iter().filter(|t| t.contains('\u{1f}') || t.contains("-words") || keywords.contains(&t.trim().to_string())).cloned().collect();
    leaked.sort();
    leaked.dedup();
    telescope_shell::settings::forget();
    w.shut_down();
    assert!(leaked.is_empty(), "search keywords in the accessibility tree: {leaked:?}");
}

#[test]
fn a_colour_wheel_says_where_its_puck_is_in_words() {
    let w = build(&telescope_core::testing::scratch_dir("a11y-wheel"));
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 9000.0));
    w.ui.show().unwrap();
    w.process();
    let wheel = |name: &str| ElementHandle::find_by_accessible_label(&w.ui, name).find(|e| e.accessible_role() == Some(AccessibleRole::Slider)).unwrap_or_else(|| panic!("no wheel {name}"));
    let value = |name: &str| wheel(name).accessible_value().unwrap_or_default().to_string();
    assert_eq!(value("Lift colour"), "neutral");
    assert!(wheel("Lift colour").accessible_description().unwrap_or_default().contains("Shift"), "the keys are described");
    let set = |x: [f64; 4], y: [f64; 4]| {
        w.state.borrow_mut().effects.push(telescope_shell::shell::Effect::Props { plugin: "colour_grading".into(), ui: "card".into(), props: serde_json::json!({"wheel-x": x, "wheel-y": y}) });
        w.process();
    };
    set([0.3, 0.0, 0.0, 0.0], [0.0, -0.5, 0.0, 1.0]);
    assert_eq!(value("Lift colour"), "towards blue, 30%");
    assert_eq!(value("Gamma colour"), "towards spring green, 50%");
    assert_eq!(value("Gain colour"), "neutral");
    assert_eq!(value("Offset colour"), "towards pink, 100%");
    set([-0.2, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(value("Lift colour"), "towards yellow, 20%");
    w.shut_down();
}

fn keys(w: &Window, text: slint::SharedString) {
    w.ui.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    w.ui.window().dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(20));
}

#[test]
fn a_real_cards_dropdown_works_from_the_keyboard_and_says_so() {
    use slint::platform::Key;
    let w = build(&telescope_core::testing::scratch_dir("a11y-dropdown"));
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 9000.0));
    w.ui.show().unwrap();
    for name in ["browser_camera", "webcam"] {
        w.state.borrow_mut().effects.push(telescope_shell::shell::Effect::Props { plugin: name.into(), ui: "card".into(), props: serde_json::json!({"visible": true}) });
    }
    w.process();
    let boxes = || w.ui.root_element().query_descendants().match_predicate(|e| e.accessible_role() == Some(AccessibleRole::Combobox)).find_all();
    let all = boxes();
    assert!(all.len() > 5, "{} dropdowns", all.len());
    for e in &all {
        assert_eq!(e.accessible_expandable(), Some(true), "{:?} says it can open", e.accessible_label());
        assert_eq!(e.accessible_expanded(), Some(false), "{:?} says it is closed", e.accessible_label());
    }
    // Tab to the first dropdown a person can use
    let mut got = None;
    for _ in 0..400 {
        keys(&w, Key::Tab.into());
        let Some((x, y, fw, fh)) = telescope_shell::input::focused_box(&w.ui) else { continue };
        got = boxes().into_iter().find(|e| {
            let (p, s) = (e.absolute_position(), e.size());
            e.accessible_enabled() != Some(false) && (p.x - x).abs() < 1.0 && (p.y - y).abs() < 1.0 && (s.width - fw).abs() < 1.0 && (s.height - fh).abs() < 1.0
        });
        if got.is_some() {
            break;
        }
    }
    let dropdown = got.expect("Tab reaches a dropdown");
    let name = dropdown.accessible_label().unwrap_or_default().to_string();
    let before = dropdown.accessible_value().unwrap_or_default().to_string();
    keys(&w, " ".into());
    assert_eq!(dropdown.accessible_expanded(), Some(true), "{name}: Space opens it");
    let items = w.ui.root_element().query_descendants().match_predicate(|e| e.accessible_role() == Some(AccessibleRole::ListItem)).find_all();
    assert!(items.len() >= 2, "{name}: its list items are exposed");
    assert_eq!(items.iter().filter(|i| i.accessible_item_selected() == Some(true)).count(), 1, "{name}: one item is the highlighted one");
    // (a dropdown with nothing picked yet reads empty, or its placeholder, which isn't one of the items)
    assert_eq!(items.iter().filter(|i| i.accessible_label().as_deref() == Some(before.as_str())).count(), usize::from(!before.is_empty() && before != "Pick a device"), "{name}: the current choice {before:?} is among them ({:?})", items.iter().map(|i| i.accessible_label()).collect::<Vec<_>>());
    keys(&w, Key::Escape.into());
    assert_eq!(dropdown.accessible_expanded(), Some(false), "{name}: Esc closes it");
    assert_eq!(dropdown.accessible_value().unwrap_or_default(), before, "{name}: and keeps the choice");
    keys(&w, Key::Return.into());
    assert_eq!(dropdown.accessible_expanded(), Some(true), "{name}: focus came back, so Enter opens it again");
    w.shut_down();
}

#[test]
fn the_zoom_slider_reads_its_value_with_units() {
    let w = build(&telescope_core::testing::scratch_dir("a11y-zoom"));
    w.ui.window().set_size(slint::LogicalSize::new(1300.0, 9000.0));
    w.ui.show().unwrap();
    w.process();
    let zoom: Vec<_> = w.ui.root_element().query_descendants().match_predicate(|e| e.accessible_role() == Some(AccessibleRole::Slider) && e.accessible_label().as_deref() == Some("Zoom")).find_all();
    assert!(!zoom.is_empty(), "a Zoom slider");
    for z in zoom {
        let value = z.accessible_value().unwrap_or_default().to_string();
        assert!(value.contains('\u{d7}') || value.contains('%'), "Zoom reads {value:?}, with its unit");
    }
    w.shut_down();
}
