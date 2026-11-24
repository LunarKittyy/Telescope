// Appearance's options in the real window: every step of corners, text size, density and contrast (and contrast in each look) draws the
// main window with every card and each Settings tab, and in each of them no text is cut without an ellipsis, nothing leaves its card,
// the columns stay where they were and the default is the picture that was there before the option was touched. The longest
// languages are drawn at the largest text. APPEARANCE_SHOTS=dir writes the pictures and a list of the texts that end in an ellipsis.
// Offscreen: nothing reaches the desktop.
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementRoot};
use i_slint_core::item_tree::ItemRc;
use i_slint_core::items::{ComplexText, SimpleText, TextOverflow, TextWrap};
use i_slint_core::layout::Orientation;
use slint::ComponentHandle;
use slint_interpreter::{ComponentInstance, Value};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::frame::PixelFormat;
use telescope_core::i18n::tr;
use telescope_core::host::{Appearance, Corners, Density, Look, MotionChoice, TextSize};
use telescope_core::link::{Link, PairingOffer, PhoneSession, RoutePrefs, Unreachable};
use telescope_core::media::Media;
use telescope_core::output::{Outputs, Slot, VideoOutput};
use telescope_core::plugin::Plugin;
use telescope_core::services::Services;
use telescope_core::source::PairedPhone;
use telescope_shell::offscreen::{self, Picture};
use telescope_shell::plugins::ORDER;
use telescope_shell::shell::Effect;
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
    /// Whether the Setup plugin builds the Windows cards (the rest of the tests draw the Linux ones).
    static WINDOWS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
        Box::new(telescope_setup::setup::SetupPlugin::new(outputs.clone(), link.clone(), media, "1.0", telescope_setup::setup::Language::found(&[])).for_platform(!WINDOWS.with(|w| w.get()))),
        Box::new(telescope_link::plugin::ConnectionPlugin::new(link, telescope_link::shared::Shared::new(Arc::new(NoTunnels))).with_outputs(outputs.clone())),
        Box::new(telescope_link::browser::plugin::BrowserCameraPlugin::new(server, Vec::new)),
        Box::new(telescope_outputs::webcam::WebcamPlugin::system()),
        Box::new(telescope_outputs::spout::SpoutPlugin::system()),
        Box::new(telescope_outputs::spout_send::SpoutSendPlugin::system()),
        Box::new(telescope_effects::cinema::CinemaPlugin::new()),
        Box::new(telescope_effects::background::BackgroundPlugin::with_reader(pictures.clone())),
        Box::new(telescope_effects::face_exposure::FaceExposurePlugin::new()),
        Box::new(telescope_effects::chroma_key::ChromaKeyPlugin::with_reader(pictures.clone())),
        Box::new(telescope_effects::fun_pack::FunPackPlugin::new()),
        Box::new(telescope_effects::privacy_blur::PrivacyBlurPlugin::new()),
        Box::new(telescope_effects::colour_grading::ColourGradingPlugin::new()),
        Box::new(telescope_effects::filters::FiltersPlugin::new()),
        Box::new(telescope_outputs::stream_output::StreamOutputPlugin::new(true)),
        Box::new(telescope_effects::transforms::TransformsPlugin::new()),
        Box::new(telescope_effects::framing::FramingPlugin::new()),
        Box::new(telescope_effects::desk::DeskPlugin::new()),
        Box::new(telescope_effects::name_tag::NameTagPlugin::new()),
        Box::new(telescope_effects::notes::NotesPlugin::new()),
        Box::new(telescope_effects::stereo_plugin::StereoPlugin::new()),
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
    ];
    plugins.extend(telescope_shell::plugins::builtins(None, bus));
    plugins
}

const MAIN_W: f32 = 1300.0;
const MAIN_H: f32 = 3000.0;
const SETTINGS_W: f32 = 940.0;
/// The width the sheets draw Settings at: 940, or what APPEARANCE_SETTINGS_W says (720 for a look at the least width)
fn settings_w() -> f32 {
    std::env::var("APPEARANCE_SETTINGS_W").ok().and_then(|w| w.parse().ok()).unwrap_or(SETTINGS_W)
}
/// The Settings window's least width (shell settings.rs LEAST)
const SETTINGS_LEAST_W: f32 = 720.0;

/// A window with every built-in plugin, every card on and every fold open, saving to a folder of its own.
fn build(name: &str) -> Rc<Window> {
    build_saved(name, None)
}

/// build, with an appearance already saved in the settings file (an app that is starting again).
fn build_saved(name: &str, saved: Option<Appearance>) -> Rc<Window> {
    offscreen::install();
    let folder = telescope_core::testing::scratch_dir(name);
    let services = Arc::new(Services {
        link: Arc::new(NoLink), media: Arc::new(telescope_media::FfmpegMedia), outputs: Arc::new(OneCamera), audio: None, notifier: None, autostart: None,
        updater: None, tray: None, plugins: Box::new(Vec::new), vision: None, plugin_problems: Default::default(), plugin_admin: None,
    });
    let file = ConfigFile::at(folder.join(CONFIG_FILENAME));
    assert!(file.update(|cfg| {
        cfg["plugins"] = serde_json::json!({"on": ORDER});
        if let Some(json) = saved.and_then(|a| a.to_json()) {
            cfg["plugin_configs"] = serde_json::json!({"setup": {"appearance": json}});
        }
    }));
    let w = window::build(file, move |bus| telescope_shell::plugins::in_order(every_plugin(bus)), Some(services), None).unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(MAIN_W, MAIN_H));
    w.ui.show().unwrap();
    for name in ["browser_camera", "webcam", "spout", "spout_send"] {
        w.state.borrow_mut().effects.push(Effect::Props { plugin: name.into(), ui: "card".into(), props: serde_json::json!({"visible": true}) });
    }
    w.process();
    settle(&w);
    for _ in 0..3 {
        for e in w.ui.root_element().query_descendants().match_predicate(|e| e.accessible_checkable() == Some(true) && e.accessible_checked() == Some(false) && e.accessible_role() == Some(AccessibleRole::Button)).find_all() {
            e.invoke_accessible_default_action();
        }
        settle(&w);
    }
    w
}

/// Lets what a change started finish, drawing as the window would.
fn settle(w: &Rc<Window>) {
    for _ in 0..30 {
        offscreen::skip(std::time::Duration::from_millis(50));
        w.process();
        offscreen::redraw(w.ui.window());
    }
}

/// What the person's pick does to the windows, through the real effect.
fn pick(w: &Rc<Window>, appearance: Appearance) {
    w.state.borrow_mut().effects.push(Effect::Appearance(appearance));
    w.process();
    settle(w);
}

/// A control of the Settings window by its accessible name (an exact match).
fn control(settings: &ComponentInstance, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(settings, label).next().unwrap_or_else(|| {
        let word = label.split(':').next().unwrap_or(label);
        let near: Vec<String> = settings.root_element().query_descendants().find_all().iter().filter_map(|e| e.accessible_label()).filter(|l| l.contains(word)).map(|l| l.to_string()).collect();
        let all: Vec<String> = settings.root_element().query_descendants().find_all().iter().filter_map(|e| e.accessible_label()).map(|l| l.to_string()).collect();
        panic!("no control called {label}; near: {near:?}; {} labelled in all: {:?}", all.len(), &all[..all.len().min(60)])
    })
}

/// A switch of the Settings window by its accessible name (the row's marker has the same name, and cannot be checked).
fn switch(settings: &ComponentInstance, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(settings, label).find(|e| e.accessible_checkable() == Some(true)).unwrap_or_else(|| panic!("no switch called {label}"))
}

/// Opens Settings on General, where the Look card is.
fn look_card(w: &Rc<Window>) -> Rc<telescope_shell::settings::Settings> {
    telescope_shell::settings::open(w, "general");
    let settings = telescope_shell::settings::current().expect("the settings window");
    assert!(settings.show_tab("general"));
    // As tall as the content, so the whole card is there to be found
    settings.ui.window().set_size(slint::LogicalSize::new(settings_w(), 9000.0));
    offscreen::pump();
    w.process();
    settle(w);
    settings
}

/// Picks a look the way a person does, with the Theme chip of the Look card.
fn look(w: &Rc<Window>, look: Look) {
    let settings = look_card(w);
    let names = ["Dark", "Classic", "OLED", "Light"];
    control(&settings.ui, &format!("{}: {}", tr("Theme"), tr(names[look.index()]))).invoke_accessible_default_action();
    w.process();
    settle(w);
    let shown = settings.get_prop("setup", "look", "theme-current");
    assert_eq!(shown, Some(Value::Number(look.index() as f64)), "the Theme chip shows {look:?}");
    for (what, ui) in [("main window", &w.ui), ("Settings", &settings.ui)] {
        assert_eq!(ui.get_global_property("Theme", "look").unwrap(), Value::Number(look.index() as f64), "{what} wears {look:?}");
    }
}

/// Picks an appearance the way a person does (the Look card's chips, switches, slider and Motion list), so the plugin state, the controls and the tokens all change.
fn choose(w: &Rc<Window>, a: Appearance) {
    let settings = look_card(w);
    let chip = |row: &str, item: &str| format!("{}: {}", tr(row), tr(item));
    let (corners, text, density) = (["Sharp", "Default", "Round"], ["Small", "Default", "Large"], ["Compact", "Default", "Comfortable"]);
    control(&settings.ui, &chip("Corners", corners[a.corners as usize])).invoke_accessible_default_action();
    control(&settings.ui, &chip("Text size", text[a.text as usize])).invoke_accessible_default_action();
    control(&settings.ui, &chip("Density", density[a.density as usize])).invoke_accessible_default_action();
    for (name, on) in [("Higher contrast", a.contrast), ("Show the next tooltip sooner", a.tip_warm)] {
        let flip = switch(&settings.ui, &tr(name));
        if flip.accessible_checked() != Some(on) {
            flip.invoke_accessible_default_action();
        }
    }
    // The Motion list and the slider have no accessible action: their callbacks go to the plugin as the controls would send them
    assert!(settings.call_with("setup", "look", "motion-picked", &[Value::Number(a.motion as usize as f64)]));
    assert!(settings.call_with("setup", "look", "tip-delay-moved", &[Value::Number(f64::from(a.tip_delay_ms))]));
    w.process();
    settle(w);
    applied(w, &a);
}

/// That the main window and the Settings window wear an appearance's tokens (and the tooltip and motion globals).
fn wears(w: &Rc<Window>, a: &Appearance) {
    let settings = telescope_shell::settings::current().expect("the settings window");
    let motion = telescope_shell::appearance::motion_of(a);
    let (reduced, off) = (motion != telescope_shell::motion::Motion::Full, motion == telescope_shell::motion::Motion::Off);
    for (what, ui) in [("main window", &w.ui), ("Settings", &settings.ui)] {
        let number = |g: &str, n: &str| match ui.get_global_property(g, n) {
            Ok(Value::Number(v)) => v,
            other => panic!("{what}: {g}.{n} is {other:?}"),
        };
        let flag = |g: &str, n: &str| ui.get_global_property(g, n) == Ok(Value::Bool(true));
        assert_eq!(number("Theme", "radius"), a.radius(), "{what}: Theme.radius for {a:?}");
        assert_eq!(number("Theme", "body"), a.body(), "{what}: Theme.body for {a:?}");
        assert_eq!(number("Theme", "unit"), a.unit(), "{what}: Theme.unit for {a:?}");
        assert_eq!(flag("Theme", "contrast"), a.contrast, "{what}: Theme.contrast for {a:?}");
        assert_eq!(number("Tips", "delay"), f64::from(a.tip_delay_ms), "{what}: Tips.delay for {a:?}");
        assert_eq!(flag("Tips", "warm"), a.tip_warm, "{what}: Tips.warm for {a:?}");
        assert_eq!((flag("Motion", "reduced"), flag("Motion", "off")), (reduced, off), "{what}: Motion for {a:?}");
    }
}

/// That an appearance is what the windows wear and the Look card shows: the tokens, the plugin state behind the controls, and the controls as drawn.
fn applied(w: &Rc<Window>, a: &Appearance) {
    wears(w, a);
    let settings = telescope_shell::settings::current().expect("the settings window");
    // The plugin's own state, which is what the card's controls show
    let prop = |n: &str| settings.get_prop("setup", "look", n);
    assert_eq!(prop("corners-current"), Some(Value::Number(a.corners as usize as f64)), "the Corners chip");
    assert_eq!(prop("text-current"), Some(Value::Number(a.text as usize as f64)), "the Text size chip");
    assert_eq!(prop("density-current"), Some(Value::Number(a.density as usize as f64)), "the Density chip");
    assert_eq!(prop("contrast-checked"), Some(Value::Bool(a.contrast)), "the Higher contrast switch");
    assert_eq!(prop("tip-warm-checked"), Some(Value::Bool(a.tip_warm)), "the next tooltip switch");
    // And the controls as drawn: the chips and switches say the same through their accessibility
    let (corners, text, density) = (["Sharp", "Default", "Round"], ["Small", "Default", "Large"], ["Compact", "Default", "Comfortable"]);
    for (row, items, at) in [("Corners", corners, a.corners as usize), ("Text size", text, a.text as usize), ("Density", density, a.density as usize)] {
        for (i, item) in items.iter().enumerate() {
            let checked = control(&settings.ui, &format!("{}: {}", tr(row), tr(item))).accessible_checked();
            assert_eq!(checked, Some(i == at), "the {row} chip {item} for {a:?}");
        }
    }
    assert_eq!(switch(&settings.ui, &tr("Higher contrast")).accessible_checked(), Some(a.contrast), "the Higher contrast switch as drawn");
}

fn shots() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("APPEARANCE_SHOTS")?);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

// ── What a window is checked for ──────────────────────────────────────────────

struct Cut {
    text: String,
    width: f32,
    wanted: f32,
}

/// The texts that are shown cut without an ellipsis (a Text that neither elides nor wraps and has less room than its words), and the
/// ones that end in an ellipsis.
fn text_fit(ui: &ComponentInstance) -> (Vec<Cut>, Vec<String>) {
    let (mut cut, mut elided) = (Vec::new(), Vec::new());
    let adapter = i_slint_core::window::WindowInner::from_pub(ui.window()).window_adapter();
    let root = ItemRc::new_root(i_slint_core::window::WindowInner::from_pub(ui.window()).component());
    fn walk(item: &ItemRc, adapter: &i_slint_core::window::WindowAdapterRc, cut: &mut Vec<Cut>, elided: &mut Vec<String>) {
        if item.is_visible() {
            let wanted = |item: &ItemRc| item.borrow().as_ref().layout_info(Orientation::Horizontal, -1.0, adapter, item).preferred;
            if let Some(t) = item.downcast::<SimpleText>() {
                let (text, width) = (i_slint_core::items::SimpleText::FIELD_OFFSETS.text().apply_pin(t.as_pin_ref()).get().to_string(), item.geometry().size.width);
                if !text.is_empty() && width >= 1.0 && wanted(item) > width + 0.75 {
                    cut.push(Cut { text, width, wanted: wanted(item) });
                }
            } else if let Some(t) = item.downcast::<ComplexText>() {
                let pin = t.as_pin_ref();
                let text = ComplexText::FIELD_OFFSETS.text().apply_pin(pin).get().to_string();
                let overflow = ComplexText::FIELD_OFFSETS.overflow().apply_pin(pin).get();
                let wrap = ComplexText::FIELD_OFFSETS.wrap().apply_pin(pin).get();
                let width = item.geometry().size.width;
                if !text.is_empty() && width >= 1.0 && wrap == TextWrap::NoWrap && wanted(item) > width + 0.75 {
                    if overflow == TextOverflow::Elide {
                        elided.push(text);
                    } else {
                        cut.push(Cut { text, width, wanted: wanted(item) });
                    }
                }
            }
        }
        let mut child = item.first_child();
        while let Some(c) = child {
            walk(&c, adapter, cut, elided);
            child = c.next_sibling();
        }
    }
    walk(&root, &adapter, &mut cut, &mut elided);
    (cut, elided)
}

/// The accessible things that are drawn partly outside the card they sit in (their middle is in it, an edge is not).
fn leaving_cards(ui: &ComponentInstance) -> Vec<String> {
    let all = ui.root_element().query_descendants().find_all();
    let cards: Vec<(f32, f32, f32, f32)> = all.iter()
        .filter(|e| e.accessible_role() == Some(AccessibleRole::Groupbox) && e.accessible_id().as_deref() == Some("card"))
        .map(|e| {
            let (p, s) = (e.absolute_position(), e.size());
            (p.x, p.y, s.width, s.height)
        })
        .collect();
    let mut found = Vec::new();
    for e in &all {
        let (p, s) = (e.absolute_position(), e.size());
        if s.width <= 0.0 || s.height <= 0.0 || matches!(e.accessible_role(), None | Some(AccessibleRole::None) | Some(AccessibleRole::Groupbox)) {
            continue;
        }
        let (mx, my) = (p.x + s.width / 2.0, p.y + s.height / 2.0);
        let Some((cx, cy, cw, ch)) = cards.iter().filter(|(cx, cy, cw, ch)| mx >= *cx && mx <= cx + cw && my >= *cy && my <= cy + ch).min_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3))) else { continue };
        let slack = 1.0;
        if p.x < cx - slack || p.y < cy - slack || p.x + s.width > cx + cw + slack || p.y + s.height > cy + ch + slack {
            found.push(format!("{:?} \"{}\" at {:.0},{:.0} {:.0}x{:.0} leaves its card {:.0},{:.0} {:.0}x{:.0}", e.accessible_role(), e.accessible_label().unwrap_or_default(), p.x, p.y, s.width, s.height, cx, cy, cw, ch));
        }
    }
    found
}

/// The cards' columns: where each card sits across, so a step that only changes how things look can be told not to move them.
fn columns(ui: &ComponentInstance) -> Vec<(i32, i32)> {
    let mut found: Vec<(i32, i32)> = ui.root_element().query_descendants().match_predicate(|e| e.accessible_role() == Some(AccessibleRole::Groupbox) && e.accessible_id().as_deref() == Some("card"))
        .find_all().iter().map(|e| (e.absolute_position().x.round() as i32, e.size().width.round() as i32)).collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// Where the cards end, to draw the window no taller than what is on it.
fn content_height(ui: &ComponentInstance) -> f32 {
    ui.root_element().query_descendants().match_predicate(|e| e.accessible_role() == Some(AccessibleRole::Groupbox) && e.accessible_id().as_deref() == Some("card"))
        .find_all().iter().map(|e| e.absolute_position().y + e.size().height).fold(0.0, f32::max)
}

fn draw(ui: &ComponentInstance, width: f32, height: f32) -> Picture {
    offscreen::draw(ui.window(), width, height, 1.0).expect("a picture")
}

/// A drawn window, the problems found in it and the texts it ends in an ellipsis.
struct Seen {
    picture: Picture,
    /// How many pixels at the left are left out of a comparison (Settings' list of tabs, which keeps a focus ring and a caret that blink)
    skip_x: u32,
    columns: Vec<(i32, i32)>,
    problems: Vec<String>,
    elided: Vec<String>,
}

fn look_at(ui: &ComponentInstance, label: &str, width: f32, fixed_height: Option<f32>) -> Seen {
    // As tall as the content, with a little over so a card's shadow isn't cut
    ui.window().set_size(slint::LogicalSize::new(width, fixed_height.unwrap_or(9000.0)));
    offscreen::pump();
    let height = fixed_height.unwrap_or_else(|| (content_height(ui) + 24.0).ceil().max(200.0));
    let picture = draw(ui, width, height);
    let (cut, elided) = text_fit(ui);
    let mut problems: Vec<String> = cut.iter().map(|c| format!("{label}: \"{}\" is cut without an ellipsis ({:.0} px of room for {:.0})", c.text.replace('\n', " "), c.width, c.wanted)).collect();
    problems.extend(leaving_cards(ui).into_iter().map(|p| format!("{label}: {p}")));
    Seen { picture, skip_x: if fixed_height.is_some() { 0 } else { 240 }, columns: columns(ui), problems, elided }
}

/// The main window and every Settings tab, as they are now.
fn everything(w: &Rc<Window>, step: &str) -> Vec<(String, Seen)> {
    let mut seen = vec![(format!("main-{step}"), look_at(&w.ui, &format!("{step}, main window"), MAIN_W, Some(MAIN_H)))];
    telescope_shell::settings::open(w, "general");
    let settings = telescope_shell::settings::current().expect("the settings window");
    for tab in settings.tabs() {
        assert!(settings.show_tab(&tab), "{tab}");
        w.process();
        settle(w);
        seen.push((format!("settings-{tab}-{step}"), look_at(&settings.ui, &format!("{step}, Settings tab {tab}"), settings_w(), None)));
    }
    seen
}

fn same(a: &Seen, b: &Seen) -> bool {
    differ(a, b).is_empty()
}

/// Where two drawn windows differ, as "x0,y0 to x1,y1" (or their sizes when those differ); empty when they are the same.
fn differ(a: &Seen, b: &Seen) -> String {
    let (a, b, skip) = (&a.picture, &b.picture, a.skip_x);
    if (a.width, a.height) != (b.width, b.height) {
        return format!("{}x{} against {}x{}", a.width, a.height, b.width, b.height);
    }
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..a.height {
        for x in skip..a.width {
            let i = ((y * a.width + x) * 4) as usize;
            if a.rgba[i..i + 3] != b.rgba[i..i + 3] {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    if x0 == u32::MAX { String::new() } else { format!("{x0},{y0} to {x1},{y1}") }
}

/// A grid of pictures, shrunk to half size and set side by side, `across` to a row.
fn sheet(pictures: &[&Picture], across: usize) -> Picture {
    let half = |p: &Picture| ((p.width / 2) as usize, (p.height / 2) as usize);
    let gap = 8usize;
    let rows: Vec<&[&Picture]> = pictures.chunks(across).collect();
    let width = rows.iter().map(|r| r.iter().map(|p| half(p).0 + gap).sum::<usize>()).max().unwrap_or(1);
    let height: usize = rows.iter().map(|r| r.iter().map(|p| half(p).1).max().unwrap_or(0) + gap).sum();
    let mut rgba = vec![24u8; width * height * 4];
    let mut y0 = 0usize;
    for row in rows {
        let mut x0 = 0usize;
        for p in row {
            let (w, h) = half(p);
            for y in 0..h {
                for x in 0..w {
                    let mut sum = [0u32; 3];
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let i = (((y * 2 + dy) * p.width as usize) + x * 2 + dx) * 4;
                        for (c, total) in sum.iter_mut().enumerate() {
                            *total += u32::from(p.rgba[i + c]);
                        }
                    }
                    let o = ((y0 + y) * width + x0 + x) * 4;
                    for (c, total) in sum.iter().enumerate() {
                        rgba[o + c] = (total / 4) as u8;
                    }
                    rgba[o + 3] = 255;
                }
            }
            x0 += w + gap;
        }
        y0 += row.iter().map(|p| half(p).1).max().unwrap_or(0) + gap;
    }
    for px in rgba.chunks_mut(4) {
        px[3] = 255;
    }
    Picture { width: width as u32, height: height as u32, rgba }
}

/// Writes every picture and the sheets, the ellipsis list too.
fn save(dir: &Path, group: &str, seen: &[(String, Seen)]) {
    for (name, s) in seen {
        s.picture.save_png(&dir.join(format!("{name}.png"))).unwrap();
    }
    let mut elisions = String::new();
    for (name, s) in seen {
        let mut texts = s.elided.clone();
        texts.sort();
        texts.dedup();
        elisions.push_str(&format!("{name}: {}\n", texts.iter().map(|t| format!("\"{}\"", t.replace('\n', " "))).collect::<Vec<_>>().join(", ")));
    }
    std::fs::write(dir.join(format!("{group}-ellipsis.txt")), elisions).unwrap();
    let _ = sheet; // sheets are made by the callers that know what belongs together
}

fn steps() -> Vec<(&'static str, Appearance)> {
    let base = Appearance::default();
    vec![
        ("default", base),
        ("corners-sharp", Appearance { corners: Corners::Sharp, ..base }),
        ("corners-round", Appearance { corners: Corners::Round, ..base }),
        ("text-small", Appearance { text: TextSize::Small, ..base }),
        ("text-large", Appearance { text: TextSize::Large, ..base }),
        ("density-compact", Appearance { density: Density::Compact, ..base }),
        ("density-comfortable", Appearance { density: Density::Comfortable, ..base }),
        ("contrast", Appearance { contrast: true, ..base }),
        ("motion-off", Appearance { motion: MotionChoice::Off, ..base }),
        ("tips-slow", Appearance { tip_delay_ms: 1500, tip_warm: false, ..base }),
        ("everything-largest", Appearance { corners: Corners::Round, text: TextSize::Large, density: Density::Comfortable, contrast: true, ..base }),
        ("everything-smallest", Appearance { corners: Corners::Sharp, text: TextSize::Small, density: Density::Compact, ..base }),
    ]
}

/// Whether two sets of columns differ only in how wide the window's own scrollbar room makes them (never by more than a pixel).
fn columns_match(a: &[(i32, i32)], b: &[(i32, i32)]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x.0 - y.0).abs() <= 1 && (x.1 - y.1).abs() <= 1)
}

#[test]
fn every_step_draws_the_main_window_and_every_settings_tab_inside_their_boxes() {
    let w = build("appearance-render");
    let before = everything(&w, "before");
    let mut problems: Vec<String> = Vec::new();
    let mut all: Vec<(String, Seen)> = Vec::new();
    for (name, appearance) in steps() {
        choose(&w, appearance);
        let seen = everything(&w, name);
        // The columns stay where they are: only text size and density may change what is inside a card, never where the cards sit across
        for ((n, s), (_, b)) in seen.iter().zip(&before) {
            if !columns_match(&s.columns, &b.columns) {
                problems.push(format!("{n}: the cards moved sideways: {:?} against {:?}", s.columns, b.columns));
            }
        }
        if name == "default" {
            for ((n, s), (_, b)) in seen.iter().zip(&before) {
                if !same(s, b) {
                    problems.push(format!("{n}: the default is not the picture that was there before ({})", differ(s, b)));
                }
            }
        }
        // The largest text may not cut anything the default text does not (the English labels all still fit)
        if name == "text-large" || name == "everything-largest" {
            for ((n, s), (_, b)) in seen.iter().zip(&before) {
                let was: std::collections::BTreeSet<&String> = b.elided.iter().collect();
                // A web address is content that differs from one window to the next, not a text of the app
                let new: Vec<&String> = s.elided.iter().filter(|t| !was.contains(t) && !t.starts_with("http")).collect();
                if !new.is_empty() {
                    problems.push(format!("{n}: newly ends in an ellipsis: {new:?}"));
                }
            }
        }
        all.extend(seen);
    }
    // Back to the default after all of them: nothing stays behind
    choose(&w, Appearance::default());
    for ((n, s), (_, b)) in everything(&w, "after").iter().zip(&before) {
        if !same(s, b) {
            problems.push(format!("{n}: it does not come back to the default ({})", differ(s, b)));
        }
    }
    for (_, s) in &all {
        problems.extend(s.problems.iter().cloned());
    }
    if let Some(dir) = shots() {
        save(&dir, "steps", &all);
        save(&dir, "before", &before);
        for tab in ["general", "devices", "video", "audio", "plugins", "shortcuts", "about"] {
            let pictures: Vec<&Picture> = all.iter().filter(|(n, _)| n.starts_with(&format!("settings-{tab}-"))).map(|(_, s)| &s.picture).collect();
            if !pictures.is_empty() {
                sheet(&pictures, 4).save_png(&dir.join(format!("sheet-settings-{tab}.png"))).unwrap();
            }
        }
        let mains: Vec<&Picture> = all.iter().filter(|(n, _)| n.starts_with("main-")).map(|(_, s)| &s.picture).collect();
        sheet(&mains, 4).save_png(&dir.join("sheet-main.png")).unwrap();
    }
    telescope_shell::settings::forget();
    w.shut_down();
    problems.sort();
    problems.dedup();
    assert!(problems.is_empty(), "{} things wrong:\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn contrast_is_drawn_in_each_look() {
    let w = build("appearance-contrast");
    let mut all: Vec<(String, Seen)> = Vec::new();
    let mut problems = Vec::new();
    for (name, l) in [("dark", Look::Dark), ("classic", Look::Classic), ("oled", Look::Oled), ("light", Look::Light)] {
        look(&w, l);
        choose(&w, Appearance::default());
        let normal = everything(&w, &format!("{name}-normal"));
        choose(&w, Appearance { contrast: true, ..Default::default() });
        let high = everything(&w, &format!("{name}-contrast"));
        for ((n, a), (_, b)) in normal.iter().zip(&high) {
            if same(a, b) {
                problems.push(format!("{n}: higher contrast draws the same picture"));
            }
            if !columns_match(&a.columns, &b.columns) {
                problems.push(format!("{n}: higher contrast moved the cards: {:?} against {:?}", a.columns, b.columns));
            }
        }
        for (_, s) in normal.iter().chain(&high) {
            problems.extend(s.problems.iter().cloned());
        }
        all.extend(normal);
        all.extend(high);
    }
    if let Some(dir) = shots() {
        save(&dir, "contrast", &all);
        for tab in ["main", "settings-general", "settings-video", "settings-plugins"] {
            let pictures: Vec<&Picture> = all.iter().filter(|(n, _)| n.starts_with(&format!("{tab}-"))).map(|(_, s)| &s.picture).collect();
            sheet(&pictures, 4).save_png(&dir.join(format!("sheet-contrast-{tab}.png"))).unwrap();
        }
    }
    telescope_shell::settings::forget();
    w.shut_down();
    problems.sort();
    problems.dedup();
    assert!(problems.is_empty(), "{} things wrong:\n{}", problems.len(), problems.join("\n"));
}

/// The longest languages at the largest text: German's compounds, Russian's and French's length, Hindi's tall script, Polish and Turkish.
#[test]
fn the_longest_languages_fit_at_the_largest_text() {
    telescope_core::i18n::use_folder(Path::new(env!("CARGO_MANIFEST_DIR")).join("../locale"));
    let mut problems = Vec::new();
    let mut all: Vec<(String, Seen)> = Vec::new();
    let mut newly = String::new();
    for language in ["de", "ru", "fr", "hi", "pl", "tr"] {
        assert_eq!(telescope_core::i18n::load(Some(language)), language);
        let w = build(&format!("appearance-lang-{language}"));
        let large = Appearance { text: TextSize::Large, ..Default::default() };
        choose(&w, Appearance::default());
        let normal = everything(&w, &format!("{language}-default"));
        choose(&w, large);
        let big = everything(&w, &format!("{language}-large"));
        // What the largest text cuts or ends in an ellipsis that the default text does not
        for ((n, a), (_, b)) in normal.iter().zip(&big) {
            let was: std::collections::BTreeSet<_> = a.elided.iter().collect();
            let new: Vec<&String> = b.elided.iter().filter(|t| !was.contains(t)).collect();
            if !new.is_empty() {
                // An ellipsis is what a text that does not fit is meant to do: listed for a look, not failed
                newly.push_str(&format!("{n}: {new:?}\n"));
            }
        }
        for (_, s) in &big {
            problems.extend(s.problems.iter().cloned());
        }
        all.extend(big);
        telescope_shell::settings::forget();
        w.shut_down();
    }
    if let Some(dir) = shots() {
        std::fs::write(dir.join("languages-newly-elided.txt"), &newly).unwrap();
        save(&dir, "languages", &all);
        for tab in ["main", "settings-general", "settings-video", "settings-audio", "settings-devices"] {
            let pictures: Vec<&Picture> = all.iter().filter(|(n, _)| n.starts_with(&format!("{tab}-"))).map(|(_, s)| &s.picture).collect();
            if !pictures.is_empty() {
                sheet(&pictures, 3).save_png(&dir.join(format!("sheet-languages-{tab}.png"))).unwrap();
            }
        }
    }
    telescope_core::i18n::set_translator(|_| None);
    problems.sort();
    problems.dedup();
    assert!(problems.is_empty(), "{} things wrong:\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn the_settings_window_at_its_least_width_keeps_its_names_at_the_largest_text_and_loosest_density() {
    let w = build("appearance-narrow");
    let mut all: Vec<(String, Seen)> = Vec::new();
    let mut problems = Vec::new();
    let big = Appearance { text: TextSize::Large, density: Density::Comfortable, contrast: true, ..Default::default() };
    for (name, l) in [("dark", Look::Dark), ("light", Look::Light), ("oled", Look::Oled), ("classic", Look::Classic)] {
        look(&w, l);
        choose(&w, big);
        telescope_shell::settings::open(&w, "general");
        let settings = telescope_shell::settings::current().expect("the settings window");
        for tab in settings.tabs() {
            assert!(settings.show_tab(&tab), "{tab}");
            w.process();
            settle(&w);
            let seen = look_at(&settings.ui, &format!("{name}, Settings tab {tab} at {SETTINGS_LEAST_W}"), SETTINGS_LEAST_W, None);
            // The names beside the Look card's controls are shown whole
            for text in ["Theme", "Card colours", "Corners", "Text size", "Density", "Motion", "Tooltip delay", "Show the next tooltip sooner", "Language", "Higher contrast"] {
                if seen.elided.iter().any(|t| t == text) {
                    problems.push(format!("{name}, tab {tab}: \"{text}\" ends in an ellipsis at {SETTINGS_LEAST_W} px"));
                }
            }
            problems.extend(seen.problems.iter().cloned());
            all.push((format!("w720-{name}-{tab}"), seen));
        }
    }
    if let Some(dir) = shots() {
        save(&dir, "w720", &all);
        for tab in ["general", "devices", "video", "audio", "plugins", "shortcuts", "about"] {
            let pictures: Vec<&Picture> = all.iter().filter(|(n, _)| n.ends_with(&format!("-{tab}"))).map(|(_, s)| &s.picture).collect();
            if !pictures.is_empty() {
                sheet(&pictures, 4).save_png(&dir.join(format!("sheet-w720-{tab}.png"))).unwrap();
            }
        }
    }
    choose(&w, Appearance::default());
    look(&w, Look::Dark);
    telescope_shell::settings::forget();
    w.shut_down();
    problems.sort();
    problems.dedup();
    assert!(problems.is_empty(), "{} things wrong:\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn the_motion_choice_overrides_the_system_and_reaches_every_window() {
    let w = build("appearance-motion");
    let main = &w.ui;
    telescope_shell::settings::open(&w, "general");
    let settings = telescope_shell::settings::current().expect("the settings window");
    let flags = |ui: &ComponentInstance| {
        let get = |name: &str| ui.get_global_property("Motion", name).unwrap();
        (get("reduced"), get("off"))
    };
    let (yes, no) = (slint_interpreter::Value::Bool(true), slint_interpreter::Value::Bool(false));
    pick(&w, Appearance { motion: MotionChoice::System, ..Default::default() });
    let system = telescope_shell::motion::current();
    let expected = |mode: telescope_shell::motion::Motion| match mode {
        telescope_shell::motion::Motion::Full => (no.clone(), no.clone()),
        telescope_shell::motion::Motion::Reduced => (yes.clone(), no.clone()),
        telescope_shell::motion::Motion::Off => (yes.clone(), yes.clone()),
    };
    assert_eq!(flags(main), expected(system), "System follows the desktop");
    for (choice, mode) in [(MotionChoice::Full, telescope_shell::motion::Motion::Full), (MotionChoice::Reduced, telescope_shell::motion::Motion::Reduced), (MotionChoice::Off, telescope_shell::motion::Motion::Off)] {
        pick(&w, Appearance { motion: choice, ..Default::default() });
        assert_eq!(flags(main), expected(mode), "{choice:?} on the main window");
        assert_eq!(flags(&settings.ui), expected(mode), "{choice:?} on the Settings window");
    }
    pick(&w, Appearance::default());
    assert_eq!(flags(main), expected(system));
    telescope_shell::settings::forget();
    w.shut_down();
}

/// A Settings window made after the pick, or after a restart with the pick saved, wears it and its Look card shows it.
#[test]
fn a_settings_window_made_after_the_pick_wears_it() {
    let big = Appearance { corners: Corners::Round, text: TextSize::Large, density: Density::Comfortable, contrast: true, tip_delay_ms: 900, tip_warm: false, motion: MotionChoice::Reduced };
    // The effect alone (nobody used the card) dresses the open Settings window and the main window, and leaves the card's own state behind
    let w = build("appearance-effect");
    look_card(&w);
    w.state.borrow_mut().effects.push(Effect::Appearance(big));
    w.process();
    wears(&w, &big);
    let settings = telescope_shell::settings::current().expect("the settings window");
    assert_eq!(settings.get_prop("setup", "look", "text-current"), Some(Value::Number(1.0)), "the card is not told by the effect alone");
    telescope_shell::settings::forget();
    w.shut_down();
    // Picked in the running app, with Settings not yet made
    let w = build("appearance-late");
    w.state.borrow_mut().effects.push(Effect::Appearance(big));
    w.process();
    assert!(telescope_shell::settings::current().is_none(), "Settings is not made yet");
    telescope_shell::settings::open(&w, "general");
    wears(&w, &big);
    telescope_shell::settings::forget();
    w.shut_down();
    // Saved, and the app starting again: the plugin tells the windows before Settings exists, and the card shows what was saved
    let w = build_saved("appearance-restart", Some(big));
    w.process();
    settle(&w);
    assert!(telescope_shell::settings::current().is_none(), "Settings is not made yet");
    look_card(&w);
    applied(&w, &big);
    telescope_shell::settings::forget();
    w.shut_down();
}

// ── A work area smaller than the window ───────────────────────────────────────

/// The chips (the tab-role controls of a Segmented) whose word has less than `pad` of room at each side, from the chip's own size and the
/// width its word asks for. A chip named in `required` that has no drawn word to measure is a problem too.
fn crowded_chips(ui: &ComponentInstance, pad: f32, required: &[String]) -> Vec<String> {
    let inner = i_slint_core::window::WindowInner::from_pub(ui.window());
    let adapter = inner.window_adapter();
    let root = ItemRc::new_root(inner.component());
    // Every drawn word with where it is in the window and the width it asks for
    fn words(item: &ItemRc, adapter: &i_slint_core::window::WindowAdapterRc, found: &mut Vec<(String, f32, f32, f32)>) {
        if item.is_visible() {
            let text = if let Some(t) = item.downcast::<SimpleText>() {
                Some(i_slint_core::items::SimpleText::FIELD_OFFSETS.text().apply_pin(t.as_pin_ref()).get().to_string())
            } else {
                item.downcast::<ComplexText>().map(|t| ComplexText::FIELD_OFFSETS.text().apply_pin(t.as_pin_ref()).get().to_string())
            };
            if let Some(text) = text.filter(|t| !t.is_empty()) {
                let (size, at) = (item.geometry().size, item.map_to_window(Default::default()));
                let wanted = item.borrow().as_ref().layout_info(Orientation::Horizontal, -1.0, adapter, item).preferred;
                found.push((text, at.x + size.width / 2.0, at.y + size.height / 2.0, wanted));
            }
        }
        let mut child = item.first_child();
        while let Some(c) = child {
            words(&c, adapter, found);
            child = c.next_sibling();
        }
    }
    let mut found = Vec::new();
    words(&root, &adapter, &mut found);
    let (mut crowded, mut measured) = (Vec::new(), Vec::<String>::new());
    for chip in ui.root_element().query_descendants().match_predicate(|e| e.accessible_role() == Some(AccessibleRole::Tab)).find_all() {
        let (at, size, label) = (chip.absolute_position(), chip.size(), chip.accessible_label().unwrap_or_default().to_string());
        let item = label.rsplit(": ").next().unwrap_or(&label).to_string();
        let Some((_, _, _, wanted)) = found.iter().find(|(t, x, y, _)| *t == item && *x >= at.x && *x <= at.x + size.width && *y >= at.y && *y <= at.y + size.height) else {
            if required.contains(&item) {
                crowded.push(format!("{label}: no drawn word \"{item}\" inside the chip, so its room could not be measured"));
            }
            continue;
        };
        if required.contains(&item) {
            measured.push(item.clone());
        }
        let room = (size.width - wanted) / 2.0;
        if room < pad {
            crowded.push(format!("{label}: {room:.1} px at each side of its word (a chip {:.0} px wide, the word {wanted:.0}), wanted {pad:.1}", size.width));
        }
    }
    for name in required {
        if !measured.contains(name) && !crowded.iter().any(|c| c.contains(&format!("\"{name}\""))) {
            crowded.push(format!("no chip named \"{name}\" was found to measure"));
        }
    }
    crowded
}

/// Where the choice rows of the Look card begin and end across (their names, the `Segmented` rows), so they can be compared.
fn choice_rows(ui: &ComponentInstance, names: &[String]) -> Vec<(String, f32, f32)> {
    names.iter().filter_map(|n| {
        let e = ElementHandle::find_by_accessible_label(ui, n).find(|e| e.accessible_role() == Some(AccessibleRole::TabList))?;
        Some((n.clone(), e.absolute_position().x, e.size().width))
    }).collect()
}

fn scroll_settings(ui: &ComponentInstance, by: f32) {
    use slint::platform::WindowEvent;
    ui.window().dispatch_event(WindowEvent::PointerScrolled { position: slint::LogicalPosition::new(400.0, 400.0), delta_x: 0.0, delta_y: by });
}

/// The control of this role with this name: where its box is in the window, none while it is clipped away (a page scrolled past it).
fn found_at(ui: &ComponentInstance, label: &str, role: AccessibleRole) -> Option<(f32, f32)> {
    let e = ElementHandle::find_by_accessible_label(ui, label).find(|e| e.accessible_role() == Some(role))?;
    Some((e.absolute_position().y, e.absolute_position().y + e.size().height))
}

/// Settings opened on a screen whose work area is 1920x1032 physical pixels at 150% (a 1920x1080 screen less a taskbar of 48, 688 logical high), its
/// frame 22x56 as Windows gives it, at Large text, Comfortable density and Higher contrast: the window takes no more than the work area, the page
/// scrolls to the Language row and Help translate beneath it, no chip's word touches its edges, and the Look card's choice rows are all as wide.
#[test]
fn settings_on_a_1280_by_720_work_area_scrolls_to_language_and_its_chips_keep_their_room() {
    let w = build("appearance-workarea");
    let big = Appearance { text: TextSize::Large, density: Density::Comfortable, contrast: true, ..Default::default() };
    let mut problems = Vec::new();
    let mut seen_scroll = false;
    for (name, l) in [("dark", Look::Dark), ("light", Look::Light), ("oled", Look::Oled), ("classic", Look::Classic)] {
        look(&w, l);
        choose(&w, big);
        let settings = look_card(&w);
        let size = settings.fit_to_work_area((1920, 1032), 1.5, (22, 56));
        offscreen::pump();
        w.process();
        settle(&w);
        assert_eq!(size, (1410, 904), "940x680 at 150% cut to the work area less its frame and the margin (48 logical)");
        let height = settings.ui.window().size().height as f32 / settings.ui.window().scale_factor();
        assert!((height - size.1 as f32 / 1.5).abs() < 1.0, "the window is {height} logical high");
        let (lang, translate) = (tr("Language"), tr("Help translate"));
        let below = |at: Option<(f32, f32)>| at.is_none_or(|(_, to)| to > height + 0.5);
        let before = below(found_at(&settings.ui, &translate, AccessibleRole::Button));
        let top = draw(&settings.ui, 940.0, height);
        // The chips keep room around their words, on every tab at this size
        let pad = match settings.ui.get_global_property("Theme", "unit") { Ok(Value::Number(unit)) => unit as f32 * 3.0, other => panic!("Theme.unit is {other:?}") };
        for tab in settings.tabs() {
            assert!(settings.show_tab(&tab), "{tab}");
            w.process();
            settle(&w);
            let required = if tab == "general" { vec![tr("Comfortable")] } else { Vec::new() };
            problems.extend(crowded_chips(&settings.ui, pad, &required).into_iter().map(|p| format!("{name}, tab {tab}: {p}")));
            if tab == "general" {
                let rows = choice_rows(&settings.ui, &[tr("Theme"), tr("Card colours"), tr("Corners"), tr("Text size"), tr("Density")]);
                if rows.len() != 5 {
                    problems.push(format!("{name}: only {} of the Look card's five choice rows were found: {rows:?}", rows.len()));
                }
                if let Some((_, x, width)) = rows.first() {
                    for (row, rx, rw) in &rows {
                        if (rx - x).abs() > 0.5 || (rw - width).abs() > 0.5 {
                            problems.push(format!("{name}: {row} starts at {rx:.1} and is {rw:.1} wide, the first row {x:.1} and {width:.1}"));
                        }
                    }
                }
            }
        }
        assert!(settings.show_tab("general"));
        w.process();
        settle(&w);
        // Scrolled to the end, the Language row and Help translate are in the window
        for _ in 0..4 {
            scroll_settings(&settings.ui, -400.0);
        }
        offscreen::pump();
        w.process();
        settle(&w);
        for (what, label, role) in [("Language", &lang, AccessibleRole::Combobox), ("Help translate", &translate, AccessibleRole::Button)] {
            match found_at(&settings.ui, label, role) {
                Some((from, to)) if from >= 0.0 && to <= height + 0.5 => {}
                other => problems.push(format!("{name}: {what} is at {other:?} of a window {height:.0} tall after scrolling to the end")),
            }
        }
        seen_scroll |= before;
        if !before {
            problems.push(format!("{name}: Help translate already fits before scrolling, so the test shows nothing"));
        }
        let bottom = draw(&settings.ui, 940.0, height);
        if let Some(dir) = shots() {
            top.save_png(&dir.join(format!("workarea-{name}-top.png"))).unwrap();
            bottom.save_png(&dir.join(format!("workarea-{name}-bottom.png"))).unwrap();
            if name == "dark" {
                // The same window on the laptop's screen at 150%
                let picture = offscreen::draw(settings.ui.window(), 940.0, height, 1.5).expect("a picture");
                picture.save_png(&dir.join("workarea-dark-bottom-150.png")).unwrap();
                offscreen::draw(settings.ui.window(), 940.0, height, 1.0);
            }
        }
        scroll_settings(&settings.ui, 5000.0);
    }
    assert!(seen_scroll);
    choose(&w, Appearance::default());
    look(&w, Look::Dark);
    telescope_shell::settings::forget();
    w.shut_down();
    problems.sort();
    problems.dedup();
    assert!(problems.is_empty(), "{} things wrong:\n{}", problems.len(), problems.join("\n"));
}

fn number(ui: &ComponentInstance, name: &str) -> f32 {
    match ui.get_property(name) {
        Ok(Value::Number(n)) => n as f32,
        other => panic!("{name} is {other:?}"),
    }
}

/// The main window on a 1366x768 screen at 150% (910x512 logical) less a taskbar of 40 and a frame of 22x56 physical pixels: the window's own
/// minimum height follows what the screen leaves (448 logical), the columns scroll, and what is below the fold is reachable at the end.
#[test]
fn the_main_window_on_a_short_screen_gets_as_short_as_the_screen_and_its_columns_scroll() {
    let w = build("appearance-short-main");
    let size = w.fit_to_work_area((1366, 728), 1.5, (22, 56));
    assert_eq!(size, (1344, 672), "1380x900 logical cut to the work area less its frame");
    settle(&w);
    let (width, height) = (size.0 as f32 / 1.5, size.1 as f32 / 1.5);
    assert_eq!((number(&w.ui, "least-width"), number(&w.ui, "least-height")), (560.0, height), "the minimum is what the screen leaves, no more than 520");
    let mut problems = Vec::new();
    for (name, l) in [("dark", Look::Dark), ("light", Look::Light), ("oled", Look::Oled), ("classic", Look::Classic)] {
        look(&w, l);
        // As short as the minimum lets it be
        w.ui.window().set_size(slint::LogicalSize::new(width, height));
        offscreen::pump();
        w.process();
        settle(&w);
        let (view_top, view_height) = (number(&w.ui, "view-top"), number(&w.ui, "view-height"));
        let below = content_height(&w.ui) > view_top + view_height + 0.5;
        if !below {
            problems.push(format!("{name}: everything fits in {height:.0} logical, so the test shows nothing"));
        }
        let top = draw(&w.ui, width, height);
        let (cut, _) = text_fit(&w.ui);
        problems.extend(cut.iter().map(|c| format!("{name}: \"{}\" is cut without an ellipsis ({:.0} px of room for {:.0})", c.text.replace('\n', " "), c.width, c.wanted)));
        problems.extend(leaving_cards(&w.ui).into_iter().map(|p| format!("{name}: {p}")));
        // Scrolled to the end every column has its last card whole inside the view
        for column in ["scroll0", "scroll1", "scroll2"] {
            let _ = w.ui.set_property(column, Value::Number(100_000.0));
        }
        offscreen::pump();
        w.process();
        settle(&w);
        let end = content_height(&w.ui);
        if end > view_top + view_height + 0.5 {
            problems.push(format!("{name}: after scrolling to the end the last card still ends at {end:.0}, the view at {:.0}", view_top + view_height));
        }
        let bottom = draw(&w.ui, width, height);
        if let Some(dir) = shots() {
            top.save_png(&dir.join(format!("short-main-{name}-top.png"))).unwrap();
            bottom.save_png(&dir.join(format!("short-main-{name}-bottom.png"))).unwrap();
        }
        for column in ["scroll0", "scroll1", "scroll2"] {
            let _ = w.ui.set_property(column, Value::Number(0.0));
        }
    }
    look(&w, Look::Dark);
    w.shut_down();
    problems.sort();
    problems.dedup();
    assert!(problems.is_empty(), "{} things wrong:\n{}", problems.len(), problems.join("\n"));
}

// The Windows-only Graphics priority row, in the narrowest Settings window in every look: it fits, it stays in its card, and the picture is saved
// when APPEARANCE_SHOTS is set (also at the default width).
#[test]
fn the_graphics_priority_row_fits_the_narrowest_settings_window_on_windows() {
    WINDOWS.with(|w| w.set(true));
    let w = build("appearance-render-windows");
    telescope_shell::settings::open(&w, "video");
    let settings = telescope_shell::settings::current().expect("the settings window");
    assert!(settings.show_tab("video"));
    let mut problems = Vec::new();
    for (name, look) in [("dark", 0.0), ("light", 3.0)] {
        w.ui.set_global_property("Theme", "look", Value::Number(look)).unwrap();
        settings.ui.set_global_property("Theme", "look", Value::Number(look)).unwrap();
        w.process();
        settle(&w);
        let seen = look_at(&settings.ui, &format!("windows {name}"), SETTINGS_LEAST_W, None);
        problems.extend(seen.problems.iter().cloned());
        if let Some(dir) = shots() {
            seen.picture.save_png(&dir.join(format!("graphics-priority-{name}-narrowest.png"))).unwrap();
        }
        let wide = look_at(&settings.ui, &format!("windows {name} at {SETTINGS_W}"), SETTINGS_W, None);
        problems.extend(wide.problems.iter().cloned());
        if let Some(dir) = shots() {
            wide.picture.save_png(&dir.join(format!("graphics-priority-{name}-default.png"))).unwrap();
        }
    }
    // The same at the largest text and loosest density, picked through the Look card on the General tab
    let big = Appearance { text: TextSize::Large, density: Density::Comfortable, contrast: true, ..Default::default() };
    for (name, l) in [("dark", Look::Dark), ("light", Look::Light)] {
        assert!(settings.show_tab("general"));
        look(&w, l);
        choose(&w, big);
        assert!(settings.show_tab("video"));
        w.process();
        settle(&w);
        let seen = look_at(&settings.ui, &format!("windows {name} largest at {SETTINGS_LEAST_W}"), SETTINGS_LEAST_W, None);
        problems.extend(seen.problems.iter().cloned());
        if let Some(dir) = shots() {
            seen.picture.save_png(&dir.join(format!("graphics-priority-{name}-narrowest-largest.png"))).unwrap();
        }
    }
    assert!(settings.show_tab("general"));
    choose(&w, Appearance::default());
    look(&w, Look::Dark);
    telescope_shell::settings::forget();
    w.shut_down();
    WINDOWS.with(|w| w.set(false));
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
