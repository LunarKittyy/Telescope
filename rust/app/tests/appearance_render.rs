// Appearance's options in the real window: every step of corners, text size, density and contrast (and contrast in each look) draws the
// main window with every card and each Settings tab, and in each of them no text is cut without an ellipsis, nothing leaves its card,
// the columns stay where they were and the default is the picture that was there before the option was touched. The longest
// languages are drawn at the largest text. APPEARANCE_SHOTS=dir writes the pictures and a list of the texts that end in an ellipsis.
// Offscreen: nothing reaches the desktop.
use i_slint_backend_testing::{AccessibleRole, ElementRoot};
use i_slint_core::item_tree::ItemRc;
use i_slint_core::items::{ComplexText, SimpleText, TextOverflow, TextWrap};
use i_slint_core::layout::Orientation;
use slint::ComponentHandle;
use slint_interpreter::ComponentInstance;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::frame::PixelFormat;
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

/// Every built-in plugin there is.
fn every_plugin(bus: &telescope_core::BusSender) -> Vec<Box<dyn Plugin>> {
    let link: Arc<dyn Link> = Arc::new(NoLink);
    let outputs: Arc<dyn Outputs> = Arc::new(OneCamera);
    let media: Arc<dyn Media> = Arc::new(telescope_media::FfmpegMedia);
    let pictures = telescope_effects::shared::picture_reader(media.clone());
    let web: Arc<dyn telescope_core::web::Web> = Arc::new(telescope_link::web::SystemWeb);
    let server = telescope_link::browser::plugin::system_server(web, std::env::temp_dir(), media.clone());
    let mut plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(telescope_setup::setup::SetupPlugin::new(outputs.clone(), link.clone(), media, "1.0", telescope_setup::setup::Language::found(&[])).for_platform(true)),
        Box::new(telescope_link::plugin::ConnectionPlugin::new(link, telescope_link::shared::Shared::new(Arc::new(NoTunnels))).with_outputs(outputs.clone())),
        Box::new(telescope_link::browser::plugin::BrowserCameraPlugin::new(server, Vec::new)),
        Box::new(telescope_outputs::webcam::WebcamPlugin::system()),
        Box::new(telescope_outputs::spout::SpoutPlugin::system()),
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

/// A window with every built-in plugin, every card on and every fold open, saving to a folder of its own.
fn build(name: &str) -> Rc<Window> {
    offscreen::install();
    let folder = telescope_core::testing::scratch_dir(name);
    let services = Arc::new(Services {
        link: Arc::new(NoLink), media: Arc::new(telescope_media::FfmpegMedia), outputs: Arc::new(OneCamera), audio: None, notifier: None, autostart: None,
        updater: None, tray: None, plugins: Box::new(Vec::new), vision: None, plugin_problems: Default::default(), plugin_admin: None,
    });
    let file = ConfigFile::at(folder.join(CONFIG_FILENAME));
    assert!(file.update(|cfg| {
        cfg["plugins"] = serde_json::json!({"on": ORDER});
    }));
    let w = window::build(file, move |bus| telescope_shell::plugins::in_order(every_plugin(bus)), Some(services), None).unwrap();
    w.ui.window().set_size(slint::LogicalSize::new(MAIN_W, MAIN_H));
    w.ui.show().unwrap();
    for name in ["browser_camera", "webcam", "spout"] {
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

fn look(w: &Rc<Window>, look: Look) {
    w.state.borrow_mut().effects.push(Effect::Look(look));
    w.process();
    settle(w);
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
        seen.push((format!("settings-{tab}-{step}"), look_at(&settings.ui, &format!("{step}, Settings tab {tab}"), SETTINGS_W, None)));
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
        pick(&w, appearance);
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
    pick(&w, Appearance::default());
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
        pick(&w, Appearance::default());
        let normal = everything(&w, &format!("{name}-normal"));
        pick(&w, Appearance { contrast: true, ..Default::default() });
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
        pick(&w, Appearance::default());
        let normal = everything(&w, &format!("{language}-default"));
        pick(&w, large);
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
