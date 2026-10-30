//! telescope-rs scenes OUT [ids] [--scale 1 1.5] (or --list): parity/render_scenes.py's scenes from the Rust window, drawn offscreen
//! as OUT/<scale>x/<id>.png for parity/compare_scenes.py. Each scene is the real window with stand-ins for the phone
//! and the virtual camera, like the Python ones (desktop/scripts/translation_scenes*.py).

use serde_json::{json, Value};
use std::sync::Arc;
use telescope_core::link::{Link, PairingOffer, PhoneStatus, Resolution, RoutePreference, RoutePrefs, Unreachable, UsbNote};
use telescope_core::frame::PixelFormat;
use telescope_core::output::{Outputs, Slot, VideoOutput};
use telescope_core::services::Services;
use telescope_core::source::{PairedPhone, RouteSpec};
use telescope_core::link::PhoneSession;
use telescope_system::updates::{Asset, InstallResult, Manifest};
use telescope_system::shortcuts::{RemoteActions, ShortcutsPlugin};
use telescope_system::updates_plugin::{Backend, UpdatesPlugin};
use telescope_shell::scenes::{main_window_config, Page, Scene, Setup, CARD_WIDTH, WIDTH_THREE_COL, WIDTH_TWO_COL};

/// translation_scenes_connection.WIFI_IP and USB_SERIAL.
const WIFI_IP: &str = "192.168.1.20";
const USB_SERIAL: &str = "R5CT123ABC";

/// The phone as a scene needs it: every check answers `answer` (None: never answers, like a patched _check_status).
struct SceneLink {
    answer: Option<Resolution>,
    adb: bool,
    /// This computer is on a network, so pairing offers an address.
    network: bool,
    /// A phone pairs as soon as Add phone is open (translation_scenes.add_phone_done).
    pairs: bool,
}

/// translation_scenes_connection.pairing_dialog's Server: a fixed payload, and nobody ever pairs.
struct SceneOffer {
    network: bool,
    pairs: std::sync::atomic::AtomicBool,
}

impl PairingOffer for SceneOffer {
    fn payload(&self) -> String {
        "telescope://pair/example".into()
    }
    fn port(&self) -> u16 {
        47000
    }
    fn addresses(&self) -> Vec<String> {
        if self.network { vec![WIFI_IP.into()] } else { Vec::new() }
    }
    fn offer_over_usb(&self, _package: &str) -> usize {
        0
    }
    fn poll(&self) -> Option<PairedPhone> {
        self.pairs.swap(false, std::sync::atomic::Ordering::Relaxed).then(|| PairedPhone {
            id: "phone-1".into(),
            name: "Pixel 8".into(),
            token: "token".into(),
            ips: Vec::new(),
            active_ip: None,
            cert_sha256: String::new(),
        })
    }
}

impl Link for SceneLink {
    fn new_identity(&self) -> (String, String) {
        ("computer-id".into(), telescope_link::connection::default_computer_name())
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        Err(Unreachable::NoAnswer)
    }
    fn pair(&self, _cid: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        Ok(Arc::new(SceneOffer { network: self.network, pairs: self.pairs.into() }))
    }
    fn check(&self, _phone: &PairedPhone, _preference: RoutePreference) -> Resolution {
        match &self.answer {
            Some(res) => res.clone(),
            None => loop {
                std::thread::park();
            },
        }
    }
    fn adb_available(&self) -> bool {
        self.adb
    }
    fn bundled_apk(&self) -> Option<String> {
        Some("telescope.apk".into())
    }
}

/// translation_scenes.quiet_system's Linux: v4l2loopback set up, but the Get set up card's check never answers (the
/// scenes patch check_virtual_camera out), so it says Checking… like the Qt renders.
struct SceneOutputs;

impl Outputs for SceneOutputs {
    fn slots(&self) -> Vec<Slot> {
        // Only onboarding asks off the UI thread
        while std::thread::current().name() != Some("main") {
            std::thread::park();
        }
        vec![Slot { slot: 0, name: "Telescope".into(), device: "/dev/video11".into() }]
    }
    fn formats(&self) -> Vec<PixelFormat> {
        Vec::new()
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no virtual camera in scenes".into())
    }
}

struct NoTunnels;

impl telescope_link::connection::Tunnels for NoTunnels {
    fn acquire(&self, _serial: &str, _remote_port: u16) -> Option<u16> {
        None
    }
    fn release(&self, _serial: &str, _remote_port: u16) {}
}

/// translation_scenes._MicBackend: a virtual mic that's always there and never sets anything up.
struct SceneMic;

impl telescope_audio::backend::MicBackend for SceneMic {
    fn pick_name(&self) -> String {
        "Microphone".into()
    }
    fn problem(&self) -> Option<telescope_audio::backend::Problem> {
        None
    }
    fn prepare(&self) -> String {
        String::new()
    }
    fn open_sink(&self) -> telescope_audio::worker::OpenSink {
        Arc::new(|| Err("no virtual microphone in scenes".into()))
    }
    fn teardown(&self) {}
}

fn services(link: SceneLink) -> Arc<Services> {
    let link: Arc<dyn Link> = Arc::new(link);
    let connection = link.clone();
    Arc::new(Services {
        tray: None,
        link,
        media: Arc::new(telescope_media::FfmpegMedia),
        outputs: Arc::new(SceneOutputs),
        audio: None,
        notifier: None,
        autostart: None,
        updater: None,
        plugins: Box::new(move || {
            let shared = telescope_link::shared::Shared::new(Arc::new(NoTunnels));
            let pictures = telescope_effects::shared::picture_reader(Arc::new(telescope_media::FfmpegMedia));
            // The effects as main.rs has them; the ones that reach this computer's desktop or cameras are left out
            vec![
                Box::new(telescope_link::plugin::ConnectionPlugin::new(connection.clone(), shared)) as Box<dyn telescope_core::plugin::Plugin>,
                Box::new(telescope_effects::chroma_key::ChromaKeyPlugin::with_reader(pictures)),
                Box::new(telescope_effects::fun_pack::FunPackPlugin::new()),
                Box::new(telescope_effects::colour_grading::ColourGradingPlugin::new()),
                Box::new(telescope_effects::transforms::TransformsPlugin::new()),
                Box::new(telescope_effects::desk::DeskPlugin::new()),
                Box::new(telescope_effects::name_tag::NameTagPlugin::new()),
                Box::new(telescope_effects::notes::NotesPlugin::new()),
                Box::new(telescope_audio::plugin::MicrophonePlugin::new(Arc::new(SceneMic), None).with_denoise_problem(Box::new(String::new))),
                Box::new(UpdatesPlugin::new(Arc::new(NoUpdates), "stable")),
                // No global shortcuts: the scenes stay away from this computer's desktop
                Box::new(ShortcutsPlugin::with_backend(RemoteActions::default(), Box::new(|_| None))),
                Box::new(telescope_effects::photos::PhotosPlugin::new()),
                Box::new(telescope_effects::qr_reader::QrReaderPlugin::new()),
            ]
        }),
    })
}

/// translation_scenes_window.turned_off on the wide window.
fn turned_off(names: &[&str]) -> Setup {
    let mut config = main_window_config(true);
    config["plugins"] = json!({"off": names});
    Setup::window(Some(services(quiet())), config, WIDTH_THREE_COL, 900.0)
}

/// Never checks: the scenes show the card before its first check.
struct NoUpdates;

impl Backend for NoUpdates {
    fn fetch(&self, _channel: &str) -> Result<Option<Manifest>, String> {
        Ok(None)
    }
    fn install(&self, _m: &Manifest, _a: &Asset, _p: &mut dyn FnMut(Option<(u64, u64)>), _c: &dyn Fn() -> bool) -> Result<(i64, InstallResult), String> {
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

fn quiet() -> SceneLink {
    SceneLink { answer: None, adb: true, network: true, pairs: false }
}

fn res(status: PhoneStatus, route: Option<&str>, note: Option<UsbNote>) -> Resolution {
    let route = match route {
        Some("wifi") => Some(RouteSpec::Network { address: WIFI_IP.into() }),
        Some("usb") => Some(RouteSpec::Usb { serial: USB_SERIAL.into() }),
        _ => None,
    };
    Resolution { route, usb_note: note, ..Resolution::new(status) }
}

/// translation_scenes_connection.card_states: id, the check's answer, the route setting, adb there, as on Windows.
fn card_states() -> Vec<(&'static str, Resolution, &'static str, bool, bool)> {
    use PhoneStatus::*;
    use UsbNote::*;
    vec![
        ("ready", res(Ready, Some("wifi"), Some(NoCable)), "auto", true, false),
        ("ready-usb", res(Ready, Some("usb"), None), "auto", true, false),
        ("cable-unauthorized", res(Ready, Some("wifi"), Some(Unauthorized)), "auto", true, false),
        ("cable-other-phone", res(Ready, Some("wifi"), Some(OtherPhone)), "auto", true, false),
        ("cable-app-closed", res(Ready, Some("wifi"), Some(AppClosed)), "auto", true, false),
        ("unreachable", res(Unreachable, None, None), "auto", true, false),
        ("not-paired", res(NotPaired, None, None), "auto", true, false),
        ("not-paired-old", Resolution { before_tls: true, ..res(NotPaired, None, None) }, "auto", true, false),
        ("local-only", res(LocalOnly, None, None), "auto", true, false),
        ("local-only-windows", res(LocalOnly, None, None), "auto", false, true),
        ("usb-unknown", res(UsbNeedsAttention, None, None), "usb", true, false),
        ("usb-no-adb", res(UsbNeedsAttention, None, Some(NoAdb)), "usb", false, false),
        ("usb-no-adb-windows", res(UsbNeedsAttention, None, Some(NoAdb)), "usb", false, true),
        ("usb-no-cable", res(UsbNeedsAttention, None, Some(NoCable)), "usb", true, false),
        ("usb-unauthorized", res(UsbNeedsAttention, None, Some(Unauthorized)), "usb", true, false),
        ("usb-other-phone", res(UsbNeedsAttention, None, Some(OtherPhone)), "usb", true, false),
        ("usb-app-closed", res(UsbNeedsAttention, None, Some(AppClosed)), "usb", true, false),
        ("phone-outdated", res(PhoneOutdated, Some("wifi"), None), "auto", true, false),
        ("phone-outdated-usb", res(PhoneOutdated, Some("usb"), None), "auto", true, false),
        ("desktop-outdated", res(DesktopOutdated, Some("wifi"), None), "auto", true, false),
    ]
}

fn with_route(mut config: Value, route: &str) -> Value {
    config["plugin_configs"]["connection"]["route"] = json!(route);
    config
}

/// The Add phone dialog as translation_scenes.add_phone and pairing_dialog show it: (id, adb, on a network).
const ADD_PHONE: &[(&str, bool, bool)] = &[("dialog-add-phone", false, true), ("dialog-add-phone-usb", true, true),
                                            ("dialog-add-phone-no-network", true, false)];

/// The window with nothing paired on a computer called Studio PC, and the Add phone dialog open.
fn add_phone(adb: bool, network: bool) -> Setup {
    add_phone_pairing(adb, network, false)
}

/// Runs the event loop for a while, for what plugins do on timers (Add phone polls for a pairing every 250 ms).
fn wait(w: &std::rc::Rc<telescope_shell::window::Window>, ms: u64) {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_millis(ms) {
        telescope_shell::offscreen::pump();
        w.process();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn add_phone_pairing(adb: bool, network: bool, pairs: bool) -> Setup {
    let mut config = main_window_config(false);
    config["plugin_configs"]["connection"]["computer_id"] = json!("computer-id");
    config["plugin_configs"]["connection"]["computer_name"] = json!("Studio PC");
    let link = SceneLink { answer: Some(Resolution::new(PhoneStatus::Unreachable)), adb, network, pairs };
    Setup {
        page: Page::Dialog { frame: "DialogFrame".into(), owner: "ConnectionCard".into() },
        then: Some(Box::new(move |w| {
            w.call_ui("connection", "card", "add-phone");
            if pairs {
                wait(w, 600);
            }
        })),
        ..Setup::window(Some(services(link)), config, WIDTH_THREE_COL, 900.0)
    }
}

/// settings_window._SIZE.
const SETTINGS_SIZE: (f32, f32) = (940.0, 680.0);

/// translation_scenes.SETTINGS_PAGES.
const SETTINGS_PAGES: &[&str] = &["general", "streaming", "video", "audio", "devices", "shortcuts", "on_air", "plugins", "about"];

/// Every scene this app can draw so far.
pub fn all() -> Vec<Scene> {
    let mut scenes = vec![
        Scene::new("main-empty", || Setup::window(Some(services(quiet())), main_window_config(false), WIDTH_THREE_COL, 900.0)),
        Scene::new("main-wide", || Setup::window(Some(services(quiet())), main_window_config(true), WIDTH_THREE_COL, 900.0)),
        Scene::new("main-medium", || Setup::window(Some(services(quiet())), main_window_config(true), WIDTH_TWO_COL, 900.0)),
        Scene::new("main-narrow", || Setup::window(Some(services(quiet())), main_window_config(true), 560.0, 900.0)),
        // translation_scenes_window.banner_plugins_off*: built-ins switched off in Plugins
        Scene::new("banner-plugins-off", || turned_off(&["monitoring"])),
        Scene::new("banner-plugins-off-two", || turned_off(&["monitoring", "transforms"])),
        Scene::new("banner-plugins-off-several", || turned_off(&["monitoring", "transforms", "presets"])),
        // translation_scenes_audio.main_with_mic: the Pixel 8's settings have the mic on
        Scene::new("main-microphone", || {
            let mut config = main_window_config(true);
            config["selected_device"] = json!("phone-1");
            config["devices"] = json!({"phone-1": {"plugin_configs": {"microphone": {"enabled": true}}}});
            Setup::window(Some(services(quiet())), config, WIDTH_THREE_COL, 900.0)
        }),
    ];
    // translation_scenes_audio.mic_scene: the Microphone card alone, switched off (the worked example of a card scene)
    scenes.push(Scene::new("mic-off", || Setup {
        page: Page::Card { plugin: "microphone".into() },
        ..Setup::window(Some(services(quiet())), main_window_config(true), CARD_WIDTH, 400.0)
    }));
    for (id, answer, route, adb, windows) in card_states() {
        scenes.push(Scene::new(format!("connection-{id}"), move || {
            telescope_link::platform::set_windows(windows.then_some(true));
            let link = SceneLink { answer: Some(answer.clone()), adb, network: true, pairs: false };
            Setup::window(Some(services(link)), with_route(main_window_config(true), route), WIDTH_THREE_COL, 700.0)
        }));
    }
    for &(id, adb, network) in ADD_PHONE {
        scenes.push(Scene::new(id, move || add_phone(adb, network)));
    }
    scenes.push(Scene::new("dialog-add-phone-done", || add_phone_pairing(true, true, true)));
    // translation_scenes.phones_dialog: Pixel 8 on Studio PC, and a browser remembered as a camera
    scenes.push(Scene::new("dialog-phones", || {
        let mut config = main_window_config(true);
        config["plugin_configs"]["connection"]["computer_id"] = json!("computer-id");
        config["plugin_configs"]["connection"]["computer_name"] = json!("Studio PC");
        Setup {
            page: Page::Dialog { frame: "DialogFrame".into(), owner: "PhonePicker".into() },
            then: Some(Box::new(|w| {
                let browser = telescope_core::event::RememberedSource {
                    id: "browser:x".into(),
                    name: "Chrome on Android".into(),
                    detail: "Browser".into(),
                };
                w.emit(telescope_core::event::Event::RememberedSources { sources: vec![browser] });
                w.call_ui("connection", "header", "manage");
            })),
            ..Setup::window(Some(services(quiet())), config, WIDTH_THREE_COL, 900.0)
        }
    }));
    // translation_scenes.shortcut_editor: a new shortcut, nothing picked yet
    scenes.push(Scene::new("dialog-shortcut", || Setup {
        page: Page::SettingsDialog {
            tab: "shortcuts".into(),
            call: ("shortcuts".into(), "shortcuts".into(), "add".into()),
            frame: "DialogFrame".into(),
            owner: "ShortcutsSettings".into(),
        },
        ..Setup::window(Some(services(quiet())), main_window_config(true), SETTINGS_SIZE.0, SETTINGS_SIZE.1)
    }));
    for &tab in SETTINGS_PAGES {
        scenes.push(Scene::new(format!("settings-{tab}"), move || Setup {
            page: Page::Settings(tab.into()),
            ..Setup::window(Some(services(quiet())), main_window_config(true), SETTINGS_SIZE.0, SETTINGS_SIZE.1)
        }));
    }
    scenes
}

/// The plugins the scene window loads and the scenes it can draw, as JSON, for parity scripts to match on the Qt side.
fn list() -> Value {
    let bus = telescope_core::EventBus::new();
    let s = services(quiet());
    let mut plugins = telescope_shell::plugins::builtins(Some(&s), &bus.sender());
    plugins.extend((s.plugins)());
    let names: Vec<String> = telescope_shell::plugins::in_order(plugins).iter().map(|p| p.info().name).collect();
    json!({"plugins": names, "scenes": all().iter().map(|s| s.id.clone()).collect::<Vec<_>>()})
}

/// The command: OUT, then scene ids (all of them when none), then --scale with one or more factors (1 and 1.5); or
/// --list for what list() says.
pub fn run(words: &[String]) -> Result<(), String> {
    if words.first().map(String::as_str) == Some("--list") {
        println!("{}", list());
        return Ok(());
    }
    let Some(out) = words.first() else {
        return Err("usage: telescope-rs scenes OUT [ids] [--scale 1 1.5]".into());
    };
    let (mut ids, mut scales, mut in_scales) = (Vec::new(), Vec::new(), false);
    for w in &words[1..] {
        if w == "--scale" {
            in_scales = true;
        } else if in_scales {
            scales.push(w.parse::<f32>().map_err(|_| format!("not a scale: {w}"))?);
        } else {
            ids.push(w.clone());
        }
    }
    if scales.is_empty() {
        scales = vec![1.0, 1.5];
    }
    let mut scenes = all();
    if !ids.is_empty() {
        let unknown: Vec<&String> = ids.iter().filter(|id| !scenes.iter().any(|s| &s.id == *id)).collect();
        if !unknown.is_empty() {
            return Err(format!("no such scene: {}", unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
        }
        scenes.retain(|s| ids.contains(&s.id));
    }
    let (n, failed) = telescope_shell::scenes::render(&scenes, &scales, std::path::Path::new(out))?;
    for line in &failed {
        eprintln!("couldn't render {line}");
    }
    println!("{n} pictures in {out}");
    if failed.is_empty() { Ok(()) } else { Err(format!("{} failed", failed.len())) }
}
