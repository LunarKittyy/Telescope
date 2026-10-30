//! The window in the same states as the Qt scene renders, drawn offscreen (offscreen.rs). Each scene gets a fresh
//! settings file in a scratch folder, like translation_scenes.use_scratch_profile.

use crate::window;
use serde_json::{json, Value};
use slint::ComponentHandle;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use telescope_core::config::ConfigFile;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::services::Services;

/// (scene id, window width) of the main window scenes parity/qt_shell_scenes.py makes.
pub const SHELL_SCENES: &[(&str, f32)] = &[("shell-wide", 1300.0), ("shell-medium", 900.0), ("shell-narrow", 560.0)];

/// app._WIDTH_THREE_COL and _WIDTH_TWO_COL.
pub const WIDTH_THREE_COL: f32 = 1300.0;
pub const WIDTH_TWO_COL: f32 = 900.0;

/// What a scene draws: the main window, one tab of the settings window (settings_window._SIZE), or a popup on its own,
/// cut out of the window (Qt draws dialogs as windows of their own).
pub enum Page {
    Window,
    Settings(String),
    /// The type name of the component that frames the popup, and of the one that opens it at its own top left (positions
    /// inside a popup start from the popup).
    Dialog { frame: String, owner: String },
    /// A popup the settings window opens on one of its tabs, by running a plugin's settings callback (plugin, section,
    /// callback), cut out like Dialog.
    SettingsDialog { tab: String, call: (String, String, String), frame: String, owner: String },
    /// One plugin's card alone, CARD_WIDTH wide with CARD_MARGIN around it, as translation_scenes_audio.show_card and
    /// the other card scenes draw a card's panel in a widget of its own.
    Card { plugin: String },
}

thread_local! {
    static EVERY_OPTIONAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// While a scene's window is built: the translation scenes' main_window registers every plugin, optional ones as well, where
/// Telescope starts only the optional ones switched on.
pub fn every_optional() -> bool {
    EVERY_OPTIONAL.with(|e| e.get())
}

/// The card scenes' widget: 480 px wide, the card inside it 16 px in from every edge.
pub const CARD_WIDTH: f32 = 480.0;
pub const CARD_MARGIN: f32 = 16.0;

/// How to put the window in a scene's state.
pub struct Setup {
    /// What the plugins run on (a stand-in link answering what the scene needs); None for the built-ins alone.
    pub services: Option<Arc<Services>>,
    /// telescope_config.json as the window finds it.
    pub config: Value,
    /// Window width, and the height it starts at before growing to fit its columns (text_fit._grow_to_content).
    pub width: f32,
    pub height: f32,
    pub page: Page,
    /// Builds the plugins in place of the built-ins and the services' ones.
    pub plugins: Option<fn(&telescope_core::BusSender) -> Vec<Box<dyn Plugin>>>,
    /// Called once the window has settled, to do what the Python scene does after building it.
    pub then: Option<Box<dyn FnOnce(&std::rc::Rc<window::Window>)>>,
}

impl Setup {
    /// The main window at width with this config.
    pub fn window(services: Option<Arc<Services>>, config: Value, width: f32, height: f32) -> Setup {
        Setup { services, config, width, height, page: Page::Window, plugins: None, then: None }
    }
}

/// One of parity/render_scenes.py's scenes: its id and how to build it.
pub struct Scene {
    pub id: String,
    pub build: Box<dyn Fn() -> Setup>,
}

impl Scene {
    pub fn new(id: impl Into<String>, build: impl Fn() -> Setup + 'static) -> Scene {
        Scene { id: id.into(), build: Box::new(build) }
    }
}

/// translation_scenes.main_window's settings: Pixel 8 paired and picked (phone=false: nothing paired).
pub fn main_window_config(phone: bool) -> Value {
    let phones = if phone {
        json!([{"id": "phone-1", "name": "Pixel 8", "token": "token", "ips": [], "active_ip": null}])
    } else {
        json!([])
    };
    let connection = if phone { json!({"selected_phone": "phone-1", "phones": phones}) } else { json!({"phones": phones}) };
    json!({"version": 3, "plugin_configs": {"connection": connection}})
}

fn scratch(scene: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("telescope-rs-scenes-{}", std::process::id())).join(scene);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Builds scenes and writes out/<scale>x/<id>.png for each scale, like render_scenes.py; (pictures written, "<scale>x
/// <id>: why" for the ones that failed).
pub fn render(scenes: &[Scene], scales: &[f32], out: &Path) -> Result<(usize, Vec<String>), String> {
    if !crate::offscreen::install() {
        return Err("a window system was already set up".into());
    }
    let (mut written, mut failed) = (0, Vec::new());
    for &scale in scales {
        let folder = out.join(format!("{scale}x"));
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        for (n, scene) in scenes.iter().enumerate() {
            match render_one(scene, scale) {
                Ok(picture) => match picture.save_png(&folder.join(format!("{}.png", scene.id))) {
                    Ok(()) => written += 1,
                    Err(e) => failed.push(format!("{scale}x {}: {e}", scene.id)),
                },
                Err(e) => failed.push(format!("{scale}x {}: {e}", scene.id)),
            }
            eprint!("\r{scale}x: {} of {}", n + 1, scenes.len());
        }
        eprintln!();
    }
    Ok((written, failed))
}

fn render_one(scene: &Scene, scale: f32) -> Result<crate::offscreen::Picture, String> {
    let setup = (scene.build)();
    let dir = scratch(&scene.id);
    let path = dir.join(telescope_core::config::CONFIG_FILENAME);
    std::fs::write(&path, serde_json::to_vec_pretty(&setup.config).unwrap_or_default()).map_err(|e| e.to_string())?;
    let services = setup.services.clone();
    let own = setup.plugins;
    EVERY_OPTIONAL.with(|e| e.set(true));
    let w = window::build(ConfigFile::at(path), |bus| {
        if let Some(own) = own {
            return own(bus);
        }
        let mut plugins = crate::plugins::builtins(services.as_deref(), bus);
        if let Some(s) = &services {
            plugins.extend((s.plugins)());
        }
        crate::plugins::in_order(plugins)
    }, setup.services, None);
    EVERY_OPTIONAL.with(|e| e.set(false));
    let w = w?;
    settle(&w);
    if let Some(then) = setup.then {
        then(&w);
        settle(&w);
    }
    let picture = match &setup.page {
        Page::Window => grown(&w, setup.width, setup.height, scale),
        Page::Card { plugin } => {
            if !w.only_card(plugin) {
                return Err(format!("{plugin} has no card"));
            }
            settle(&w);
            // One column at this width; the column keeps room for its scrollbar, so widen the window by what the card lacks
            grown(&w, CARD_WIDTH, setup.height, scale).ok_or("nothing drawn")?;
            let lack = w.card_rect(plugin).map_or(0.0, |(_, _, cw, _)| CARD_WIDTH - 2.0 * CARD_MARGIN - cw);
            grown(&w, CARD_WIDTH + lack, setup.height, scale).and_then(|p| {
                let (x, y, cw, ch) = w.card_rect(plugin)?;
                let m = CARD_MARGIN;
                crop(&p, (x - m) * scale, (y - m) * scale, (cw + 2.0 * m) * scale, (ch + 2.0 * m) * scale)
            })
        }
        Page::Dialog { frame, owner } => grown(&w, setup.width, setup.height, scale).and_then(|p| cut_out(&w.ui, &p, frame, owner, scale)),
        Page::SettingsDialog { tab, call, frame, owner } => {
            // Opened as the window opens it, so what plugins say to their sections reaches it
            crate::settings::open(&w, tab);
            let settings = crate::settings::current().ok_or("no settings window")?;
            settings.call(&call.0, &call.1, &call.2);
            settle(&w);
            // The popup opens from a changed handler, which runs as the window draws
            crate::offscreen::draw(settings.ui.window(), setup.width, setup.height, scale);
            let picture = crate::offscreen::draw(settings.ui.window(), setup.width, setup.height, scale)
                .and_then(|p| cut_out(&settings.ui, &p, frame, owner, scale));
            crate::settings::forget();
            picture
        }
        Page::Settings(tab) => {
            let settings = crate::settings::build(&w)?;
            settings.show_tab(tab);
            // As settings::open does: the search box has focus when the window opens
            let _ = settings.ui.show();
            let _ = settings.ui.invoke("focus-search", &[]);
            crate::offscreen::draw(settings.ui.window(), setup.width, setup.height, scale)
        }
    };
    w.shut_down();
    window::forget();
    picture.ok_or_else(|| "nothing drawn".into())
}

/// Lets what plugins started on their threads come back: runs the event loop's jobs until none came for a while.
fn settle(w: &std::rc::Rc<window::Window>) {
    let (start, mut quiet) = (Instant::now(), Instant::now());
    while start.elapsed() < Duration::from_secs(3) && quiet.elapsed() < Duration::from_millis(150) {
        if crate::offscreen::pump() {
            quiet = Instant::now();
        }
        w.process();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Renders the scenes parity/qt_shell_scenes.py makes into out/<scale>x; how many it wrote.
pub fn render_all(out: &Path, scale: f32) -> Result<usize, String> {
    let mut scenes: Vec<Scene> = SHELL_SCENES.iter()
        .map(|&(id, width)| Scene::new(id, move || Setup::window(None, json!({}), width, 900.0)))
        .collect();
    scenes.push(Scene::new("shell-settings-plugins", || Setup {
        page: Page::Settings("plugins".into()),
        plugins: Some(stand_ins),
        ..Setup::window(None, json!({}), 940.0, 680.0)
    }));
    let (n, failed) = render(&scenes, &[scale], out)?;
    match failed.first() {
        Some(e) => Err(e.clone()),
        None => Ok(n),
    }
}

/// The Python app's built-in plugins as they describe themselves (parity/builtins.json), standing in for the ones
/// not ported yet; the ported ones are the real thing.
pub fn stand_ins(bus: &telescope_core::BusSender) -> Vec<Box<dyn Plugin>> {
    let infos: Vec<PluginInfo> = serde_json::from_str(include_str!("../parity/builtins.json")).unwrap_or_default();
    let mut real = crate::plugins::builtins(None, bus);
    infos.into_iter().map(|info| {
        match real.iter().position(|p| p.info().name == info.name) {
            Some(i) => real.remove(i),
            None => Box::new(StandIn(info)) as Box<dyn Plugin>,
        }
    }).collect()
}

struct StandIn(PluginInfo);

impl Plugin for StandIn {
    fn info(&self) -> PluginInfo {
        self.0.clone()
    }
}

/// The part of picture the last open element of type frame covers, in a popup opened by an element of type owner.
fn cut_out(ui: &slint_interpreter::ComponentInstance, picture: &crate::offscreen::Picture, frame: &str, owner: &str, scale: f32) -> Option<crate::offscreen::Picture> {
    use i_slint_backend_testing::ElementHandle;
    let found = ElementHandle::find_by_element_type_name(ui, frame).last()?;
    let at = ElementHandle::find_by_element_type_name(ui, owner).next()?.absolute_position();
    let (mut pos, size) = (found.absolute_position(), found.size());
    pos.x += at.x;
    pos.y += at.y;
    crop(picture, pos.x * scale, pos.y * scale, size.width * scale, size.height * scale)
}

/// The part of picture in this rectangle, in its pixels (clamped to the picture).
fn crop(picture: &crate::offscreen::Picture, x: f32, y: f32, w: f32, h: f32) -> Option<crate::offscreen::Picture> {
    let px = |v: f32| v.round().max(0.0) as u32;
    let (x, y) = (px(x).min(picture.width), px(y).min(picture.height));
    let (width, height) = (px(w).min(picture.width - x), px(h).min(picture.height - y));
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in y..y + height {
        let start = ((row * picture.width + x) * 4) as usize;
        rgba.extend_from_slice(&picture.rgba[start..start + (width * 4) as usize]);
    }
    Some(crate::offscreen::Picture { width, height, rgba })
}

/// text_fit._grow_to_content: the window at width, as tall as its columns need (height at least).
fn grown(w: &window::Window, width: f32, height: f32, scale: f32) -> Option<crate::offscreen::Picture> {
    crate::offscreen::draw(w.ui.window(), width, height, scale)?;
    let content = match w.ui.get_property("content-height") {
        Ok(slint_interpreter::Value::Number(h)) => h as f32,
        _ => 0.0,
    };
    let height = (60.0 + 32.0 + content + 40.0).max(height);
    crate::offscreen::draw(w.ui.window(), width, height, scale)
}
