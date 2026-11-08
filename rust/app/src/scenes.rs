//! telescope-rs scenes OUT [ids] [--scale 1 1.5] (or --list): every card, dialog and Settings page from the Rust window, drawn offscreen
//! as OUT/<scale>x/<id>.png. Each scene is the real window with stand-ins for the phone
//! and the virtual camera.

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
    /// A phone pairs as soon as Add device is open (translation_scenes.add_phone_done).
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

/// What Setup's checks find: translation_scenes_setup.fake_linux's machine, with v4l2loopback not loaded.
struct SetupOutputs;

impl Outputs for SetupOutputs {
    fn slots(&self) -> Vec<Slot> {
        Vec::new()
    }
    fn formats(&self) -> Vec<PixelFormat> {
        Vec::new()
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no virtual camera in scenes".into())
    }
    fn setup_status(&self) -> telescope_core::output::SetupStatus {
        telescope_core::output::SetupStatus::default()
    }
}

/// translation_scenes_setup.advanced's phone app: adb is found, and no apk came with this copy.
struct SetupLink;

impl Link for SetupLink {
    fn new_identity(&self) -> (String, String) {
        (String::new(), String::new())
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        Err(Unreachable::NoAnswer)
    }
    fn pair(&self, _cid: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        Err("no pairing here".into())
    }
    fn adb_available(&self) -> bool {
        true
    }
}

/// translation_scenes_setup.about makes the log file; the other scenes have none, so no Open log.
static HAS_LOG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Setup as the scenes have it.
fn setup_plugin() -> telescope_setup::setup::SetupPlugin {
    let locale = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../locale"));
    let mut plugin = telescope_setup::setup::SetupPlugin::new(Arc::new(SetupOutputs), Arc::new(SetupLink), Arc::new(telescope_media::FfmpegMedia),
                                                              &telescope_system::version::display_version(),
                                                              telescope_setup::setup::Language::found(&[locale]));
    plugin.log_path = HAS_LOG.load(std::sync::atomic::Ordering::Relaxed).then(|| "/tmp/telescope.log".into());
    // translation_scenes' hardware_device: no card, so the picture doesn't depend on this computer
    plugin.panel.hw_looked = true;
    plugin.show_graphics_card(None, false);
    plugin
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

/// A Nikon D5100 in M mode as it was probed (libgphoto2's settings, in the Nikon driver's own wording with its decimal commas), run
/// through the real mapping to the card's controls.
#[cfg(target_os = "linux")]
struct SceneRig;

#[cfg(target_os = "linux")]
impl telescope_outputs::webcam_capture::gphoto_lib::Rig for SceneRig {
    fn preview(&mut self) -> Result<Vec<u8>, String> {
        Err("no live view in scenes".into())
    }
    fn read(&mut self, name: &str) -> Option<telescope_outputs::webcam_capture::gphoto_lib::Setting> {
        use telescope_outputs::webcam_capture::gphoto_lib::{Kind, Setting};
        let choice = |value: &str, list: &[&str]| Setting { kind: Kind::Choice, readonly: false, value: value.into(), choices: list.iter().map(|s| s.to_string()).collect() };
        Some(match name {
            "autofocusdrive" => Setting { kind: Kind::Toggle, readonly: false, value: "0".into(), choices: Vec::new() },
            "manualfocusdrive" => Setting { kind: Kind::Range { min: -32767.0, max: 32767.0, step: 1.0 }, readonly: false, value: "0".into(), choices: Vec::new() },
            "f-number" => choice("f/5,6", &["f/3,5", "f/4", "f/4,5", "f/5", "f/5,6", "f/6,3", "f/7,1", "f/8", "f/9", "f/10", "f/11", "f/13", "f/14", "f/16", "f/18", "f/20", "f/22"]),
            "exposurecompensation" => choice("0", &["-5", "-4,666", "-4,333", "-4", "-3,666", "-3,333", "-3", "-2,666", "-2,333", "-2", "-1,666", "-1,333", "-1", "-0,666",
                                                    "-0,333", "0", "0,333", "0,666", "1", "1,333", "1,666", "2", "2,333", "2,666", "3", "3,333", "3,666", "4", "4,333", "4,666", "5"]),
            "whitebalance" => choice("Automatic", &["Automatic", "Daylight", "Fluorescent", "Tungsten", "Flash", "Cloudy", "Shade", "Preset"]),
            _ => return None,
        })
    }
    fn write(&mut self, _name: &str, _value: &str) -> Result<(), String> {
        Ok(())
    }
    fn restart_live_view(&mut self) {}
    fn close(&mut self) {}
}

/// The D5100's settings for the Webcam card; a set is taken and shown, as the camera would.
#[cfg(target_os = "linux")]
struct SceneDslr(Vec<telescope_outputs::webcam_controls::Control>);

#[cfg(target_os = "linux")]
impl telescope_outputs::webcam_controls::Controls for SceneDslr {
    fn list(&mut self) -> Vec<telescope_outputs::webcam_controls::Control> {
        self.0.clone()
    }
    fn set(&mut self, name: &str, value: i64) -> bool {
        if let Some(c) = self.0.iter_mut().find(|c| c.name == name && c.kind == "menu") {
            c.value = value;
        }
        true
    }
}

/// What the Photos card asks a stream's camera: a fixed answer for what it can take, and no photo ever taken.
struct SceneStills(serde_json::Value);

impl telescope_core::source::SourceStills for SceneStills {
    fn caps(&self) -> Option<serde_json::Value> {
        Some(self.0.clone())
    }
    fn busy(&self) -> bool {
        false
    }
    fn take(&self, _: &serde_json::Value, _: &str, _: Box<dyn Fn(serde_json::Value) + Send>, _: Box<dyn FnOnce(serde_json::Value) + Send>, _: Box<dyn Fn() -> bool + Send>) -> bool {
        false
    }
    fn set_on_change(&self, _: Box<dyn Fn() + Send + Sync>) {}
}

/// What a Nikon D5100 in M mode offers the Photos card (probe/allconfig_M.txt): its ISO and shutter speed lists as the camera words them.
fn dslr_stills() -> serde_json::Value {
    let iso = ["100", "125", "160", "200", "250", "320", "400", "500", "640", "800", "1000", "1250", "1600", "2000", "2500", "3200", "4000", "5000", "6400", "8000", "10000", "12800", "25600"];
    let shutter = ["0,0002s", "0,0003s", "0,0004s", "0,0005s", "0,0006s", "0,0008s", "0,0010s", "0,0012s", "0,0015s", "0,0020s", "0,0025s", "0,0031s", "0,0040s", "0,0050s",
                   "0,0062s", "0,0080s", "0,0100s", "0,0125s", "0,0166s", "0,0200s", "0,0250s", "0,0333s", "0,0400s", "0,0500s", "0,0666s", "0,0769s", "0,1000s", "0,1250s",
                   "0,1666s", "0,2000s", "0,2500s", "0,3333s", "0,4000s", "0,5000s", "0,6250s", "0,7692s", "1,0000s", "1,3000s", "1,6000s", "2,0000s", "2,5000s", "3,0000s",
                   "4,0000s", "5,0000s", "6,0000s", "8,0000s", "10,0000s", "13,0000s", "15,0000s", "20,0000s", "25,0000s", "30,0000s"];
    // The gphoto code is Linux's: elsewhere the camera's own words stand in for the labels
    #[cfg(target_os = "linux")]
    let labels: Vec<String> = shutter.iter().map(|s| telescope_outputs::webcam_capture::gphoto_stills::shutter_label(s).unwrap()).collect();
    #[cfg(not(target_os = "linux"))]
    let labels: Vec<String> = shutter.iter().map(|s| s.to_string()).collect();
    json!({"formats": ["jpeg", "raw"], "camera": true, "burst_max": 1,
           "iso": {"values": iso, "labels": iso, "current": 6},
           "shutter": {"values": shutter.to_vec(), "labels": labels, "current": 20}})
}

/// A flagship phone's lens: every mode, shot and extra the Photos card has.
fn phone_stills() -> serde_json::Value {
    json!({"formats": ["jpeg", "heic", "ultrahdr", "raw"], "size": [4080, 3072], "max_res": [8160, 6144], "max_res_raw": true,
           "extensions": ["night", "hdr", "bokeh"], "exposure_max_ns": 30_000_000_000i64, "bracketing": true, "focus_bracketing": true,
           "noise_reduction_hq": true, "sharpening_hq": true, "hot_pixel_hq": true, "distortion": true, "burst_max": 60})
}

/// A webcam with the settings a good USB one offers, for the Webcam card.
struct SceneUsb;

impl telescope_outputs::webcam_controls::Controls for SceneUsb {
    fn list(&mut self) -> Vec<telescope_outputs::webcam_controls::Control> {
        use telescope_outputs::webcam_controls::Control;
        let mut time = Control::new("exposure_time", "int", 3, 2047, 1, 250, 250);
        time.unit = "100us".into();
        time.inactive = true;
        let mut kelvin = Control::new("white_balance", "int", 2000, 6500, 1, 4000, 4000);
        kelvin.unit = "kelvin".into();
        kelvin.inactive = true;
        let mut flicker = Control::new("anti_flicker", "menu", 0, 2, 1, 1, 1);
        flicker.menu = vec![0, 1, 2];
        vec![Control::new("exposure_auto", "bool", 0, 1, 1, 1, 1), time, Control::new("gain", "int", 0, 255, 1, 0, 0), flicker,
             Control::new("white_balance_auto", "bool", 0, 1, 1, 1, 1), kelvin, Control::new("saturation", "int", 0, 255, 1, 128, 128),
             Control::new("contrast", "int", 0, 255, 1, 128, 128), Control::new("focus_auto", "bool", 0, 1, 1, 1, 1)]
    }
    fn set(&mut self, _name: &str, _value: i64) -> bool {
        true
    }
}

#[cfg(target_os = "linux")]
fn dslr_controls() -> Box<dyn telescope_outputs::webcam_controls::Controls> {
    Box::new(SceneDslr(telescope_outputs::webcam_capture::gphoto_lib::read_controls(&mut SceneRig)))
}

/// DSLRs are Linux's for now, so a scene of one has no settings to show elsewhere.
#[cfg(not(target_os = "linux"))]
fn dslr_controls() -> Box<dyn telescope_outputs::webcam_controls::Controls> {
    Box::new(SceneUsb)
}

/// A webcam plugged in whose card a scene shows: the D5100 on a USB cable, or a webcam.
#[derive(Clone, Copy, PartialEq)]
enum Webcam {
    Dslr,
    Usb,
}

impl Webcam {
    fn key(self) -> &'static str {
        if self == Webcam::Dslr { "gphoto2-d5100" } else { "usb-brio" }
    }

    fn plugin(self) -> telescope_outputs::webcam::WebcamPlugin {
        use telescope_outputs::webcam::{System, WebcamPlugin};
        use telescope_outputs::webcams::{Device, Mode};
        let dslr = self == Webcam::Dslr;
        let device = if dslr {
            Device { key: self.key().into(), name: "Nikon DSC D5100".into(), location: "gphoto2:usb:001,004".into(), modes: Vec::new() }
        } else {
            let modes = vec![Mode::new("MJPG", 1280, 720, &[30.0, 60.0]), Mode::new("MJPG", 1920, 1080, &[30.0]), Mode::new("MJPG", 640, 480, &[30.0])];
            Device { key: self.key().into(), name: "Brio 100".into(), location: "/dev/video0".into(), modes }
        };
        let system = System { opener: Arc::new(|_, _| Err((false, "no camera in scenes".into()))), users: Arc::new(|_| String::new()), exists: Arc::new(|_| true) };
        let controls: telescope_outputs::webcam::ControlsFor = Box::new(move |_| {
            Some(if dslr { dslr_controls() } else { Box::new(SceneUsb) })
        });
        WebcamPlugin::new(Box::new(move || Ok(vec![device.clone()])), controls, system)
    }
}

/// What a scene changes in the plugins services() loads.
#[derive(Clone, Copy, Default)]
struct Changes {
    /// The Webcam plugin is there with this camera plugged in (and picked, by the scene).
    webcam: Option<Webcam>,
    /// RNNoise's library isn't there (translation_scenes_audio.rnnoise_problem).
    no_rnnoise: bool,
    /// Stream output is there, on a computer that can't decode H.264 (translation_scenes_camera.stream_no_pyav).
    no_h264: bool,
    /// The Stats card is there, which the window scenes leave out because its Tab stops move what they walk to.
    stats: bool,
    /// The MCP server is there, on the program's own handle (mcp_scene).
    mcp: bool,
    /// The On Air sign is there, with a web server that only pretends to listen.
    on_air: bool,
    /// The Wait screen and Startup plugins are there, which put their cards on the Streaming page.
    streaming: bool,
}

/// An autostart entry that is never written.
struct SceneAutostart;

impl telescope_core::system::Autostart for SceneAutostart {
    fn enabled(&self) -> bool {
        false
    }
    fn set_enabled(&self, _on: bool, _minimized: bool) -> Result<(), String> {
        Ok(())
    }
}

/// A web that makes a certificate out of nothing and a server that never listens, for the On Air page's scenes.
struct SceneWeb;

struct SceneServer(Arc<telescope_core::web::ServerStats>);

impl telescope_core::web::WebServer for SceneServer {
    fn port(&self) -> u16 {
        8768
    }
    fn stats(&self) -> Arc<telescope_core::web::ServerStats> {
        self.0.clone()
    }
    fn stop(&self) {}
}

impl telescope_core::web::Web for SceneWeb {
    fn addresses(&self) -> Vec<String> {
        vec!["192.168.50.25".into(), "100.64.0.7".into()]
    }
    fn certificate(&self, _folder: &std::path::Path, _hosts: &[String]) -> Result<telescope_core::web::Certificate, String> {
        Ok(telescope_core::web::Certificate { cert: Default::default(), key: Default::default() })
    }
    fn serve(&self, _certificate: &telescope_core::web::Certificate, serve: &telescope_core::web::Serve, _handler: Arc<dyn telescope_core::web::PageHandler>) -> Result<Box<dyn telescope_core::web::WebServer>, String> {
        Ok(Box::new(SceneServer(Arc::new(telescope_core::web::ServerStats::new(8768, serve.port, false, &serve.label)))))
    }
}

fn services(link: SceneLink) -> Arc<Services> {
    services_with(link, Changes::default())
}

fn services_with(link: SceneLink, changes: Changes) -> Arc<Services> {
    HAS_LOG.store(false, std::sync::atomic::Ordering::Relaxed);
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
        plugin_problems: Default::default(),
        plugin_admin: None,
        vision: None,
        plugins: Box::new(move || {
            let shared = telescope_link::shared::Shared::new(Arc::new(NoTunnels));
            let pictures = telescope_effects::shared::picture_reader(Arc::new(telescope_media::FfmpegMedia));
            let denoise: Box<dyn Fn() -> String + Send> = if changes.no_rnnoise {
                Box::new(|| telescope_audio::denoise::problem_text(&("missing", String::new())))
            } else {
                Box::new(String::new)
            };
            // The effects as main.rs has them; the ones that reach this computer's desktop or cameras are left out
            let stats = changes.stats.then(|| Box::new(telescope_setup::stats::StatsPlugin::new()) as Box<dyn telescope_core::plugin::Plugin>);
            let mut plugins = vec![
                Box::new(setup_plugin()) as Box<dyn telescope_core::plugin::Plugin>,
                Box::new(telescope_setup::monitoring::MonitoringPlugin::new()),
                Box::new(telescope_link::plugin::ConnectionPlugin::new(connection.clone(), shared)),
                Box::new(telescope_effects::chroma_key::ChromaKeyPlugin::with_reader(pictures)),
                Box::new(telescope_effects::face_exposure::FaceExposurePlugin::new()),
                Box::new(telescope_effects::framing::FramingPlugin::new()),
                Box::new(telescope_effects::fun_pack::FunPackPlugin::new()),
                Box::new(telescope_effects::colour_grading::ColourGradingPlugin::new()),
                Box::new(telescope_effects::transforms::TransformsPlugin::new()),
                Box::new(telescope_effects::desk::DeskPlugin::new()),
                Box::new(telescope_effects::name_tag::NameTagPlugin::new()),
                Box::new(telescope_effects::notes::NotesPlugin::new()),
                Box::new(telescope_audio::plugin::MicrophonePlugin::new(Arc::new(SceneMic), None).with_denoise_problem(denoise)),
                Box::new(UpdatesPlugin::new(Arc::new(NoUpdates), "stable")),
                // No global shortcuts: the scenes stay away from this computer's desktop
                Box::new(ShortcutsPlugin::with_backend(RemoteActions::default(), Box::new(|_| None))),
                Box::new(telescope_effects::photos::PhotosPlugin::new()),
                Box::new(telescope_effects::qr_reader::QrReaderPlugin::new()),
            ];
            if changes.no_h264 {
                plugins.push(Box::new(telescope_outputs::stream_output::StreamOutputPlugin::new(false)));
            }
            plugins.extend(stats);
            if changes.mcp {
                plugins.push(Box::new(telescope_system::mcp::McpPlugin::new(telescope_system::mcp::McpHandle::global())));
            }
            if changes.streaming {
                plugins.push(Box::new(telescope_outputs::wait_screen_plugin::WaitScreenPlugin::new(Arc::new(SceneOutputs))));
                plugins.push(Box::new(telescope_system::startup::StartupPlugin::new(Arc::new(SceneAutostart))));
            }
            if changes.on_air {
                plugins.push(Box::new(telescope_effects::on_air_plugin::OnAirPlugin::new(Arc::new(SceneWeb), None)));
            }
            plugins.extend(changes.webcam.map(|w| Box::new(w.plugin()) as Box<dyn telescope_core::plugin::Plugin>));
            plugins
        }),
    })
}

thread_local! {
    /// The AI apps a scene has connected, kept for as long as it is drawn.
    static MCP_CLIENTS: std::cell::RefCell<Vec<telescope_system::mcp::Client>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// What the MCP scenes have an AI app do before they are drawn.
#[derive(Clone, Copy)]
enum Mcp {
    /// Switched on, nothing connected.
    Nobody,
    /// Connected and at rest.
    Connected,
    /// Connected and a moment after it looked at something.
    Reading,
    /// Connected and a moment after it changed settings (so there is something to undo).
    Changing,
    /// Connected, at rest, with its changes there to undo.
    Undoable,
    /// Read only, with a refused change among what it did.
    ReadOnly,
}

/// Runs tool calls as an AI app's connection would, on a thread of their own while the window's thread keeps processing.
fn drive(w: &std::rc::Rc<telescope_shell::window::Window>, session: u64, calls: &[(&str, Value)]) {
    let handle = telescope_system::mcp::McpHandle::global();
    for (tool, args) in calls {
        let (handle, tool, args) = (handle.clone(), tool.to_string(), args.clone());
        let asked = std::thread::spawn(move || handle.call_as(session, &tool, args));
        while !asked.is_finished() {
            w.process();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let _ = asked.join();
    }
    w.process();
}

/// The window with the MCP server on, after an AI app (named Claude Desktop) did what state says.
fn mcp_scene(state: Mcp, page: Page, width: f32) -> Setup {
    MCP_CLIENTS.with(|c| c.borrow_mut().clear());
    let mut config = main_window_config(true);
    config["plugin_configs"]["mcp"] = json!({"access": if matches!(state, Mcp::ReadOnly) { "read_only" } else { "read_and_change" }});
    Setup {
        page,
        then: Some(Box::new(move |w| {
            let handle = telescope_system::mcp::McpHandle::global();
            if matches!(state, Mcp::Nobody) {
                return;
            }
            let Ok(client) = handle.open_client() else { return };
            let session = client.session();
            handle.name_client(session, "Claude Desktop");
            MCP_CLIENTS.with(|c| c.borrow_mut().push(client));
            w.process();
            let look = ("get", json!({"ids": ["colour_grading.contrast", "colour_grading.temperature"]}));
            let change = ("set", json!({"changes": {"colour_grading.contrast": 1.2, "colour_grading.temperature": -14, "colour_grading.vibrance": 18}}));
            match state {
                Mcp::Nobody | Mcp::Connected => {}
                Mcp::Reading => drive(w, session, &[("status", json!({})), look]),
                Mcp::Changing | Mcp::Undoable => drive(w, session, &[("status", json!({})), look, change]),
                Mcp::ReadOnly => {
                    drive(w, session, &[("status", json!({})), ("describe", json!({"plugin": "colour_grading"})), look]);
                    drive(w, session, &[("set", json!({"changes": {"colour_grading.contrast": 1.4}}))]);
                }
            }
            // The outline would fade while the picture is still being drawn
            w.keep_touches();
            if matches!(state, Mcp::Connected | Mcp::Undoable | Mcp::ReadOnly) {
                let calm = telescope_core::Event::TimerFired { owner: "mcp".into(), token: "calm".into() };
                w.state.borrow_mut().deliver_to("mcp", &calm);
                w.process();
            }
        })),
        every_optional: false,
        ..Setup::window(Some(services_with(quiet(), Changes { mcp: true, ..Changes::default() })), with_plugins_on(config, &["mcp", "colour_grading"]), width, 900.0)
    }
}

fn with_plugins_on(mut config: Value, on: &[&str]) -> Value {
    config["plugins"] = json!({"on": on});
    config
}

/// translation_scenes_window.turned_off on the wide window, started through load_plugins (loaded_window).
fn turned_off(names: &[&str]) -> Setup {
    let mut config = main_window_config(true);
    config["plugins"] = json!({"off": names});
    Setup { every_optional: false, ..Setup::window(Some(services(quiet())), config, WIDTH_THREE_COL, 900.0) }
}

/// What a stream that dropped a few frames over a Wi-Fi link looks like to the Stats card.
fn example_stats() -> telescope_core::stats::Snapshot {
    use telescope_core::stats::{DropReason, Hub};
    let hub = Hub::new();
    hub.want("scene", true);
    let now = telescope_core::time::monotonic_ns();
    hub.snapshot_at(now, Some(0), Some(0));
    let stats = hub.register("phone-1", 0);
    stats.set_link("https://192.168.1.20:8443/v1/video.udp", false);
    stats.set_target_fps(30);
    // A minute of a stream that was steady, stumbled from 40 s in
    let mut last = None;
    for second in 1..=60u64 {
        let rough = (40..=58).contains(&second);
        stats.received(if rough { 17 } else { 29 + second % 2 }, if rough { 300_000 } else { 560_000 });
        stats.decoded(if rough { 16 } else { 29 });
        for _ in 0..if rough { 15 } else { 29 } {
            stats.delivered();
        }
        if rough {
            stats.repeated();
            stats.dropped(DropReason::LostOnLink, 2);
            stats.dropped(DropReason::WaitingForKeyframe, 1);
        }
        stats.steps().timer("colour").add(2_000_000);
        stats.steps().timer("privacy_blur").add(7_000_000);
        hub.hooks().timer("monitoring").add(400_000);
        let cpu = 380_000_000 * second + if rough { 250_000_000 } else { 0 };
        last = Some(hub.snapshot_at(now + second * 1_000_000_000, Some(cpu), Some((240 + second / 4) << 20)));
    }
    last.expect("a snapshot")
}

/// Never checks: the scenes show the card before its first check.
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

/// The Add device dialog as translation_scenes.add_phone and pairing_dialog show it: (id, adb, on a network).
const ADD_PHONE: &[(&str, bool, bool)] = &[("dialog-add-phone", false, true), ("dialog-add-phone-usb", true, true),
                                            ("dialog-add-phone-no-network", true, false)];

/// The window with nothing paired on a computer called Studio PC, and the Add device dialog open.
fn add_phone(adb: bool, network: bool) -> Setup {
    add_phone_pairing(adb, network, false)
}

/// Runs the event loop for a while, for what plugins do on timers (Add device polls for a pairing every 250 ms).
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

/// The Add device dialog with Browser camera offered beside Android, on the page `page` (0 Android, 1 Browser camera).
fn add_phone_browser(page: i32) -> Setup {
    let mut setup = add_phone(false, true);
    setup.then = Some(Box::new(move |w| {
        w.emit(telescope_core::Event::StreamSourcesChanged { sources: vec![telescope_core::event::SourceName { id: telescope_core::source::BROWSER_SOURCE.into(), name: "Browser camera".into() }] });
        w.call_ui("connection", "card", "add-phone");
        wait(w, 200);
        let _ = w.set_card_property("connection", "card", "add-page", &json!(page));
    }));
    setup
}

/// translation_scenes.adb_licence: on Windows without adb, Get adb from Google pressed. Here the question is in Add device.
fn adb_licence() -> Setup {
    let link = SceneLink { answer: Some(Resolution::new(PhoneStatus::Unreachable)), adb: false, network: true, pairs: false };
    let mut setup = add_phone(false, true);
    setup.services = Some(services(link));
    setup.then = Some(Box::new(|w| {
        telescope_link::platform::set_windows(Some(true));
        w.call_ui("connection", "card", "add-phone");
        w.call_ui("connection", "card", "usb-action-clicked");
        telescope_link::platform::set_windows(None);
    }));
    setup
}

/// The window with the mic on, as translation_scenes_audio.main_with_mic has it.
fn mic_config() -> Value {
    let mut config = main_window_config(true);
    config["selected_device"] = json!("phone-1");
    config["devices"] = json!({"phone-1": {"plugin_configs": {"microphone": {"enabled": true}}}});
    config
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
        Scene::new("main-microphone", || Setup::window(Some(services(quiet())), mic_config(), WIDTH_THREE_COL, 900.0)),
    ];
    // translation_scenes_audio.mic_scene: the Microphone card alone, switched off (the worked example of a card scene)
    scenes.push(Scene::new("mic-off", || Setup {
        page: Page::Card { plugin: "microphone".into() },
        ..Setup::window(Some(services(quiet())), main_window_config(true), CARD_WIDTH, 400.0)
    }));
    scenes.push(Scene::new("mic-no-rnnoise", || Setup {
        page: Page::Card { plugin: "microphone".into() },
        ..Setup::window(Some(services_with(quiet(), Changes { no_rnnoise: true, ..Changes::default() })), mic_config(), CARD_WIDTH, 400.0)
    }));
    scenes.push(Scene::new("stream-no-pyav", || Setup {
        page: Page::Card { plugin: "stream_output".into() },
        ..Setup::window(Some(services_with(quiet(), Changes { no_h264: true, ..Changes::default() })), main_window_config(true), CARD_WIDTH, 400.0)
    }));
    // The Stats card with a stream's numbers in it: some drops, a slow step and a plugin that costs a little, in both views
    for (id, view, phone) in [("stats-standard", "standard", false), ("stats-nerd", "nerd", false), ("stats-phone-standard", "standard", true), ("stats-phone-nerd", "nerd", true)] {
        scenes.push(Scene::new(id, move || {
            let mut config = main_window_config(true);
            config["plugins"] = json!({"on": ["stats"]});
            config["plugin_configs"]["stats"] = json!({"view": view});
            Setup {
                page: Page::Card { plugin: "stats".into() },
                every_optional: false,
                then: Some(Box::new(move |w| {
                    w.state.borrow_mut().bus.emit(telescope_core::Event::StatsUpdated { snapshot: example_stats() });
                    // A phone from this version, a few seconds into being asked for its own numbers
                    if phone {
                        let state = json!({
                            "takes_stats": true, "phone_fps": 30, "camera_fps": 29.6, "battery_temp_c": 41.5, "wifi_band": "5", "wifi_rssi": -61, "wifi_mbps": 780,
                            "heat_fps": 24, "stats": {"encode_ms": 9.2, "encode_max_ms": 16.5, "queue_ms": 4.0, "encoder_fps": 29.4, "thermal": 3, "thermal_headroom": 0.97}
                        });
                        w.state.borrow_mut().bus.emit(telescope_core::Event::PhoneStateUpdated { state });
                    }
                    w.process();
                })),
                ..Setup::window(Some(services_with(quiet(), Changes { stats: true, ..Changes::default() })), config, CARD_WIDTH, 700.0)
            }
        }));
    }
    // The MCP server: its settings page and the header button in each state
    for (id, state) in [("mcp-settings", Mcp::Undoable), ("mcp-settings-read-only", Mcp::ReadOnly), ("mcp-settings-nobody", Mcp::Nobody)] {
        scenes.push(Scene::new(id, move || mcp_scene(state, Page::Settings("mcp".into()), 940.0)));
    }
    for (id, state) in [("mcp-header-nobody", Mcp::Nobody), ("mcp-header-connected", Mcp::Connected), ("mcp-header-reading", Mcp::Reading),
                        ("mcp-header-changing", Mcp::Changing), ("mcp-header-undoable", Mcp::Undoable)] {
        scenes.push(Scene::new(id, move || mcp_scene(state, Page::Window, WIDTH_THREE_COL)));
    }
    // The controls an AI app changes get an outline for a moment
    scenes.push(Scene::new("mcp-outline", || mcp_scene(Mcp::Changing, Page::Card { plugin: "colour_grading".into() }, CARD_WIDTH)));
    // The Webcam card for a DSLR, a webcam, and a narrow card (where the strip of tiles wraps)
    for (id, camera, width) in [("webcam-dslr", Webcam::Dslr, CARD_WIDTH), ("webcam-dslr-narrow", Webcam::Dslr, 350.0), ("webcam-usb", Webcam::Usb, CARD_WIDTH),
                                ("webcam-usb-narrow", Webcam::Usb, 350.0)] {
        scenes.push(Scene::new(id, move || {
            let mut config = main_window_config(true);
            config["plugins"] = json!({"on": ["webcam"]});
            Setup {
                page: Page::Card { plugin: "webcam".into() },
                every_optional: false,
                then: Some(Box::new(move |w| {
                    w.state.borrow_mut().bus.emit(telescope_core::Event::SourceSelected { source_id: format!("webcam:{}", camera.key()) });
                    w.process();
                })),
                ..Setup::window(Some(services_with(quiet(), Changes { webcam: Some(camera), ..Changes::default() })), config, width, 700.0)
            }
        }));
    }
    // The Photos card for a DSLR in M mode and for a phone, and with a narrow card (where the tiles wrap)
    for (id, dslr, width) in [("photos-dslr", true, CARD_WIDTH), ("photos-dslr-narrow", true, 350.0), ("photos-phone", false, CARD_WIDTH), ("photos-phone-narrow", false, 350.0)] {
        scenes.push(Scene::new(id, move || {
            let mut config = main_window_config(true);
            config["plugins"] = json!({"on": ["photos"]});
            config["plugin_configs"]["photos"] = json!({"folder": "/home/me/Pictures/Telescope", "shots": "burst", "burst": 8, "exposure_s": 4});
            Setup {
                page: Page::Card { plugin: "photos".into() },
                every_optional: false,
                then: Some(Box::new(move |w| {
                    let source = if dslr { "webcam:gphoto2-d5100" } else { "phone:a" };
                    {
                        let mut state = w.state.borrow_mut();
                        state.streams.live.push(source.into());
                        state.streams.source_id = Some(source.into());
                        let caps = if dslr { dslr_stills() } else { phone_stills() };
                        state.source_stills.push((source.into(), Arc::new(SceneStills(caps))));
                        if dslr {
                            state.bus.emit(telescope_core::Event::SourceSelected { source_id: source.into() });
                        } else {
                            state.bus.emit(telescope_core::Event::PhonesChanged { count: 1 });
                        }
                        state.bus.emit(telescope_core::Event::StillCapsChanged { source_id: source.into() });
                    }
                    w.process();
                })),
                ..Setup::window(Some(services(quiet())), config, width, 700.0)
            }
        }));
    }
    // translation_scenes_audio.control_menu: right-clicks on a control with its shortcuts saved
    scenes.push(Scene::new("menu-control-mute", || {
        let mut config = mic_config();
        let mute = |id: &str, keys: &str, mode: &str| json!({"id": id, "keys": keys, "action": "microphone.mute", "params": {"mode": mode}});
        config["plugin_configs"]["shortcuts"]["bindings"] = json!([mute("a", "Ctrl+Alt+M", "toggle"), mute("b", "Ctrl+Alt+P", "hold")]);
        Setup {
            page: Page::ControlMenu { label: "Mute".into(), size: (420.0, 130.0) },
            ..Setup::window(Some(services(quiet())), config, WIDTH_THREE_COL, 900.0)
        }
    }));
    // The tooltip of a control that has a saved shortcut (its keys as caps beside the name), and of one that has none
    scenes.push(Scene::new("tip-control-mute", || {
        let mut config = mic_config();
        config["plugin_configs"]["shortcuts"]["bindings"] = json!([{"id": "a", "keys": "Ctrl+Alt+M", "action": "microphone.mute", "params": {"mode": "toggle"}}]);
        Setup {
            page: Page::ControlTip { label: "Mute".into(), from: (330.0, 40.0), size: (540.0, 170.0) },
            ..Setup::window(Some(services(quiet())), config, WIDTH_THREE_COL, 900.0)
        }
    }));
    scenes.push(Scene::new("tip-control-auto-level", || Setup {
        page: Page::ControlTip { label: "Auto level".into(), from: (330.0, 40.0), size: (540.0, 170.0) },
        ..Setup::window(Some(services(quiet())), mic_config(), WIDTH_THREE_COL, 900.0)
    }));
    scenes.push(Scene::new("menu-control-pan", || Setup {
        page: Page::ControlMenu { label: "Pan".into(), size: (300.0, 90.0) },
        ..Setup::window(Some(services(quiet())), main_window_config(true), WIDTH_THREE_COL, 900.0)
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
    scenes.push(Scene::new("dialog-add-phone-browser", || add_phone_browser(0)));
    scenes.push(Scene::new("dialog-add-phone-browser-page", || add_phone_browser(1)));
    scenes.push(Scene::new("dialog-add-phone-done", || add_phone_pairing(true, true, true)));
    scenes.push(Scene::new("dialog-adb-licence", adb_licence));
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
    // translation_scenes_camera.composite_editor, with the streams stopped too, and the last one picked
    scenes.push(Scene::new("composite-editor", || {
        let mut config = main_window_config(true);
        config["plugin_configs"]["composite"] = json!({"layout": {"tiles": [
            {"source_id": "phone:a", "name": "Pixel 8"},
            {"source_id": "phone:b", "name": "Desk phone", "x": 0.71, "y": 0.6867, "w": 0.26, "h": 0.26, "border": 4, "radius": 24},
            {"source_id": "browser:c", "name": "Laptop", "x": 0.71, "y": 0.3733, "w": 0.26, "h": 0.26, "border": 4, "radius": 24},
        ]}});
        Setup {
            page: Page::PluginWindow { plugin: "composite".into(), open: "edit-clicked".into(), id: "editor".into(), click: Some((0.84, 0.5)) },
            plugins: Some(|_| vec![Box::new(telescope_effects::composite_plugin::CompositePlugin::new()) as Box<dyn telescope_core::plugin::Plugin>]),
            ..Setup::window(Some(services(quiet())), config, 1240.0, 760.0)
        }
    }));
    // translation_scenes_camera.composite_editor_alone, with both streams stopped: a scene has none to run
    scenes.push(Scene::new("composite-editor-alone", || {
        let mut config = main_window_config(true);
        config["plugin_configs"]["composite"] = json!({"layout": {"tiles": [
            {"source_id": "phone:a", "name": "Pixel 8"},
            {"source_id": "phone:b", "name": "Desk phone", "x": 0.62, "y": 0.6, "w": 0.33, "h": 0.33},
        ]}});
        Setup {
            page: Page::PluginWindow { plugin: "composite".into(), open: "edit-clicked".into(), id: "editor".into(), click: None },
            plugins: Some(|_| vec![Box::new(telescope_effects::composite_plugin::CompositePlugin::new()) as Box<dyn telescope_core::plugin::Plugin>]),
            ..Setup::window(Some(services(quiet())), config, 1240.0, 760.0)
        }
    }));
    // translation_scenes.shortcut_editor: a new shortcut, nothing picked yet
    scenes.push(Scene::new("dialog-shortcut", || Setup {
        page: Page::SettingsDialog {
            tab: "shortcuts".into(),
            call: ("shortcuts".into(), "shortcuts".into(), "add".into()),
            then: Vec::new(),
            frame: "DialogFrame".into(),
            owner: "ShortcutsSettings".into(),
        },
        ..Setup::window(Some(services(quiet())), main_window_config(true), SETTINGS_SIZE.0, SETTINGS_SIZE.1)
    }));
    // translation_scenes_audio's modifier editor scene: Mute picked and Ctrl pressed alone
    scenes.push(Scene::new("dialog-shortcut-modifier", || {
        let search = |words: &str| ("search".to_string(), vec![json!(words)]);
        let ctrl = vec![json!("\u{11}"), json!(true), json!(false), json!(false), json!(false)];
        Setup {
            page: Page::SettingsDialog {
                tab: "shortcuts".into(),
                call: ("shortcuts".into(), "shortcuts".into(), "add".into()),
                then: vec![search("Mute"), search(""), ("key-pressed".into(), ctrl)],
                frame: "DialogFrame".into(),
                owner: "ShortcutsSettings".into(),
            },
            ..Setup::window(Some(services(quiet())), main_window_config(true), SETTINGS_SIZE.0, SETTINGS_SIZE.1)
        }
    }));
    // translation_scenes_setup's pages: the settings window on a tab as Setup leaves it
    for (id, tab) in [("setup-about", "about"), ("setup-module-not-loaded", "devices")] {
        scenes.push(Scene::new(id, move || {
            let services = services(quiet());
            HAS_LOG.store(id == "setup-about", std::sync::atomic::Ordering::Relaxed);
            Setup { page: Page::Settings(tab.into()), ..Setup::window(Some(services), main_window_config(true), SETTINGS_SIZE.0, SETTINGS_SIZE.1) }
        }));
    }
    // The pages of the Settings redesign at a wide and a narrow window, for a look by eye (r2set-<page>-<2000|narrow>)
    for (tab, name) in [("streaming", "streaming"), ("about", "about"), ("on_air", "onair")] {
        for (label, size) in [("2000", (2000.0, 1000.0)), ("narrow", (720.0, 700.0))] {
            for state in ["", "-more"] {
                if state == "-more" && name == "streaming" {
                    continue;
                }
                scenes.push(Scene::new(format!("r2set-{name}{state}-{label}"), move || {
                    HAS_LOG.store(name == "about", std::sync::atomic::Ordering::Relaxed);
                    let services = services_with(quiet(), Changes { on_air: name == "onair", streaming: name == "streaming", ..Changes::default() });
                    let mut setup = Setup { page: Page::Settings(tab.into()), ..Setup::window(Some(services), main_window_config(true), size.0, size.1) };
                    if state == "-more" {
                        setup.then = Some(Box::new(move |w| {
                            telescope_shell::settings::open(w, tab);
                            if let Some(settings) = telescope_shell::settings::current() {
                                if name == "onair" {
                                    settings.set_props("on_air", "on_air", &json!({
                                        "webhook-url": "http://homeassistant.local:8123/api/webhook/light", "webhook-on": "{\"on_air\": true}", "webhook-off": "{\"on_air\": false}",
                                        "url-problem": "The address has to start with http:// or https://", "test-text": "Sent", "test-kind": "status_ok",
                                    }));
                                } else {
                                    settings.set_props("updates", "updates", &json!({
                                        "status-text": "Telescope 5.0.6 is out", "status-kind": "status_ok", "actions-shown": true,
                                        "update-text": "Update and restart", "update-enabled": true, "portable-shown": true,
                                    }));
                                }
                            }
                        }));
                    }
                    setup
                }));
            }
        }
    }
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

#[cfg(test)]
mod tests {
    // The plugins that say category = "extras" themselves in Python
    const IN_EXTRAS: [&str; 5] = ["mcp", "onboarding", "presets", "shortcuts", "updates"];

    // test_plugins_panel.py::test_every_built_in_plugin_picks_its_category_and_icon
    #[test]
    fn the_built_in_plugins_frame_order_is_the_one_written_down() {
        use telescope_core::frame::FrameStage;
        let bus = telescope_core::EventBus::new();
        let s = crate::services(crate::Machine::real(None), telescope_system::shortcuts::RemoteActions::default(), false);
        let mut plugins = telescope_shell::plugins::builtins(Some(&s), &bus.sender());
        plugins.extend((s.plugins)());
        let written = ["desk", "qr_reader", "background", "chroma_key", "fun_pack", "privacy_blur", "name_tag", "composite", "wait_screen"];
        let mut steps: Vec<_> = plugins.iter().map(|p| p.info()).filter(|i| written.contains(&i.name.as_str())).collect();
        assert_eq!(steps.len(), written.len());
        steps.sort_by_key(|i| (i.frame_stage, written.iter().position(|w| *w == i.name)));
        let stages: Vec<FrameStage> = steps.iter().map(|i| i.frame_stage).collect();
        assert!(stages.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(steps.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), written);
        // The window's own: its view before all of them and the colour step between fun_pack and privacy_blur
        assert!(FrameStage::View < stages[0]);
        assert!(steps[4].frame_stage < FrameStage::Colour && FrameStage::Colour < steps[5].frame_stage);
    }

    #[test]
    fn every_built_in_plugin_picks_its_category_and_icon() {
        let bus = telescope_core::EventBus::new();
        // The app's own list, as the real window loads it
        let s = crate::services(crate::Machine::real(None), telescope_system::shortcuts::RemoteActions::default(), false);
        let mut plugins = telescope_shell::plugins::builtins(Some(&s), &bus.sender());
        plugins.extend((s.plugins)());
        assert!(plugins.len() > 25, "{} plugins", plugins.len());
        let icons = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../crates/ui-kit/ui/icons"));
        for plugin in &plugins {
            let info = plugin.info();
            assert!(telescope_core::plugin::PLUGIN_CATEGORIES.iter().any(|(id, _)| *id == info.category), "{} needs a category", info.name);
            // Extras and the plug are what a plugin gets by saying nothing
            assert!(info.category != "extras" || IN_EXTRAS.contains(&info.name.as_str()), "{} needs a category, not Extras by default", info.name);
            assert_ne!(info.icon, "plug", "{} needs an icon, not the plug by default", info.name);
            // A file of that name, or a name icons.slint's `named` maps to another file ("preset" is the bookmark)
            let table = std::fs::read_to_string(icons.join("../icons.slint")).unwrap_or_default();
            let known = [format!("{}.svg", info.icon), format!("v_{}.svg", info.icon)].iter().any(|f| icons.join(f).is_file()) || table.contains(&format!("name == \"{}\"", info.icon));
            assert!(known, "{} needs an icon the kit has, not {:?}", info.name, info.icon);
        }
    }

    fn slint_text(source: &telescope_core::ui::SlintSource) -> String {
        match source {
            telescope_core::ui::SlintSource::Text(t) => t.clone(),
            telescope_core::ui::SlintSource::File(p) => std::fs::read_to_string(p).unwrap_or_default(),
        }
    }

    /// The icon a plugin's own card puts on its header, from its .slint (the first `icon: Icons.x;` after the card's component).
    fn card_icon(plugin: &mut dyn telescope_core::plugin::Plugin) -> Option<String> {
        use telescope_core::ui::SlintSource;
        let spec = plugin.card()?;
        let text = match &spec.ui.source {
            SlintSource::Text(t) => t.clone(),
            SlintSource::File(p) => std::fs::read_to_string(p).ok()?,
        };
        // The stage has no header of its own; only a standard card has one
        let at = text.find(&format!("component {} inherits Card", spec.ui.component))?;
        let rest = &text[at..];
        let icon = rest.split("icon: Icons.").nth(1)?;
        Some(icon.split(';').next()?.trim().replace('-', "_"))
    }

    // test_plugins_panel.py::test_every_card_takes_its_plugins_group_colour_and_a_main_card_its_icon
    #[test]
    fn every_card_takes_its_plugins_group_colour_and_a_main_card_its_icon() {
        let bus = telescope_core::EventBus::new();
        let s = crate::services(crate::Machine::real(None), telescope_system::shortcuts::RemoteActions::default(), false);
        let mut plugins = telescope_shell::plugins::builtins(Some(&s), &bus.sender());
        plugins.extend((s.plugins)());
        let mut found = 0;
        let mut wrong = Vec::new();
        let group = |g: &str| telescope_core::plugin::PLUGIN_CATEGORIES.iter().any(|(id, _)| *id == g);
        for plugin in &mut plugins {
            let info = plugin.info();
            // A card names another group's colour only as a group that exists; without one it's the app's purple instead
            let mut sources: Vec<String> = plugin.card().iter().map(|c| slint_text(&c.ui.source)).collect();
            for section in plugin.settings_sections() {
                sources.push(slint_text(&section.ui.source));
                if let Some(own) = section.ui.props.get("card-group") {
                    assert!(own.as_str().is_some_and(group), "{}: {own}", info.name);
                }
            }
            for line in sources.iter().flat_map(|t| t.lines()) {
                let Some(value) = line.trim().strip_prefix("hue: ") else { continue };
                assert!(!value.starts_with("Theme.accent"), "{}: a card with the app's purple instead of its group's colour", info.name);
                if let Some(name) = value.strip_prefix("Theme.plugin-hue(\"") {
                    assert!(group(name.split('"').next().unwrap_or("")), "{}: {value}", info.name);
                }
            }
            let Some(icon) = card_icon(plugin.as_mut()) else { continue };
            found += 1;
            if icon.trim_start_matches("v_") != info.icon.trim_start_matches("v_") {
                wrong.push(format!("{}: card {icon}, plugin {}", info.name, info.icon));
            }
        }
        assert!(found > 15, "{found} cards");
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    use super::{main_window_config, quiet, services, Scene, Setup};
    use slint_interpreter::{ComponentHandle, ComponentInstance, Value};

    fn number(ui: &ComponentInstance, name: &str) -> f64 {
        match ui.get_property(name) {
            Ok(Value::Number(n)) => n,
            other => panic!("{name} is {other:?}"),
        }
    }

    /// Presses `key` that many times in a window of this size, noting `read()` after each (what the window scrolls to says
    /// where the focus went).
    fn pressed(ui: &ComponentInstance, key: slint::platform::Key, times: usize, read: impl Fn() -> String) -> Vec<String> {
        use slint::platform::WindowEvent;
        let text: slint::SharedString = key.into();
        (0..times)
            .map(|_| {
                ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
                ui.window().dispatch_event(WindowEvent::KeyReleased { text: text.clone() });
                slint::platform::update_timers_and_animations();
                read()
            })
            .collect()
    }

    /// The length of the cycle: the number of presses after which the same stops come round again, three times over (the first round starts from where the window was, so it isn't counted).
    fn cycle(stops: &[String]) -> Option<usize> {
        (10..=stops.len() / 4).find(|&p| stops[p..2 * p] == stops[2 * p..3 * p] && stops[2 * p..3 * p] == stops[3 * p..4 * p])
    }

    // The columns scroll by an offset of their own and Slint skips what a plain clip hides, so Tab used to walk only the
    // controls in view: the cycle changed with the scroll, and never reached the rest
    #[test]
    fn tab_walks_the_whole_main_window_in_one_cycle_that_comes_round_the_same() {
        use slint::platform::Key;
        let size = (1240.0, 420.0);
        let scene = Scene::new("tab-main", move || Setup {
            then: Some(Box::new(move |w| {
                w.ui.window().set_size(slint::LogicalSize::new(size.0, size.1));
                telescope_shell::offscreen::draw(w.ui.window(), size.0, size.1, 1.0);
                let reads = || format!("{:?} {}", telescope_shell::input::focused_box(&w.ui), (0..3).map(|c| number(&w.ui, &format!("scroll{c}")).to_string()).collect::<Vec<_>>().join("/"));
                let (content, view) = (number(&w.ui, "content-height"), number(&w.ui, "view-height"));
                assert!(content > 3.0 * view, "the window has to be longer than its view: {content} in {view}");
                let forward = pressed(&w.ui, Key::Tab, 600, reads);
                let round = cycle(&forward).unwrap_or_else(|| panic!("Tab comes round to nothing it has been to: {forward:?}"));
                // The first pass starts with the columns at the top and the others from wherever the last one left them, which is
                // all the header's stops can show of it; from the first card on it is the same list of places each time
                assert!(forward[6..round] == forward[round + 6..2 * round], "the first pass went another way than the second");
                let scrolled = forward[..round].iter().filter_map(|s| s.rsplit(' ').next()?.split('/').next()?.parse::<f64>().ok()).fold(0.0, f64::max);
                assert!(scrolled > view * 3.0, "Tab brought the column up to {scrolled}, so it stayed in what was in view");
                // And back the way it came, from wherever it is: the same control, wherever it scrolled to (going back reveals it at the other edge)
                let place = || {
                    let scrolled: f64 = (0..3).map(|c| number(&w.ui, &format!("scroll{c}"))).sum();
                    format!("{:?}", telescope_shell::input::focused_box(&w.ui).map(|b| (b.0, b.1 as f64 + scrolled, b.2, b.3)))
                };
                let before = place();
                let ahead = pressed(&w.ui, Key::Tab, 1, reads).remove(0);
                pressed(&w.ui, Key::Backtab, 1, reads);
                assert_eq!(place(), before, "Shift+Tab undoes a Tab (after {ahead})");
            })),
            ..Setup::window(Some(services(quiet())), main_window_config(true), size.0, size.1)
        });
        let scenes = [scene];
        let out = std::env::temp_dir().join("telescope-tab-scenes");
        let (_, failed) = telescope_shell::scenes::render(&scenes, &[1.0], &out).unwrap();
        assert!(failed.is_empty() || failed.iter().all(|f| f.contains("nothing drawn")), "{failed:?}");
    }

    #[test]
    fn tab_walks_a_settings_page_in_one_cycle_and_scrolls_to_what_it_reaches() {
        use slint::platform::Key;
        let size = (940.0, 300.0);
        let scene = Scene::new("tab-settings", move || Setup {
            then: Some(Box::new(move |w| {
                for tab in ["video"] {
                    telescope_shell::settings::open(w, tab);
                    let settings = telescope_shell::settings::current().expect("the settings window");
                    settings.ui.window().set_size(slint::LogicalSize::new(size.0, size.1));
                    telescope_shell::offscreen::draw(settings.ui.window(), size.0, size.1, 1.0);
                    let page = format!("page{}-offset", settings.tabs().iter().position(|t| t == tab).unwrap());
                    let reads = || format!("{:?} {}", telescope_shell::input::focused_box(&settings.ui), number(&settings.ui, &page));
                    let forward = pressed(&settings.ui, Key::Tab, 400, reads);
                    let far = forward.iter().filter_map(|s| s.rsplit(' ').next()?.parse::<f64>().ok()).fold(0.0, f64::max);
                    assert!(cycle(&forward).is_some(), "{tab}: Tab comes round to nothing it has been to: {forward:?}");
                    assert!(far > 0.0, "{tab}: Tab reached what was out of sight and left the page where it was");
                }
            })),
            ..Setup::window(Some(services(quiet())), main_window_config(true), size.0, size.1)
        });
        let out = std::env::temp_dir().join("telescope-tab-scenes");
        let (_, failed) = telescope_shell::scenes::render(&[scene], &[1.0], &out).unwrap();
        assert!(failed.is_empty() || failed.iter().all(|f| f.contains("nothing drawn")), "{failed:?}");
    }
}
