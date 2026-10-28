//! The window in the same states as the Qt scene renders, drawn offscreen (offscreen.rs). Each scene gets a fresh
//! settings file in a scratch folder, like translation_scenes.use_scratch_profile.

use crate::window;
use slint::ComponentHandle;
use std::path::Path;
use telescope_core::config::ConfigFile;
use telescope_core::plugin::{Plugin, PluginInfo};

/// (scene id, window width) of the main window scenes parity/qt_shell_scenes.py makes.
pub const SHELL_SCENES: &[(&str, f32)] = &[("shell-wide", 1300.0), ("shell-medium", 900.0), ("shell-narrow", 560.0)];

fn scratch(scene: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("telescope-rs-scenes-{}", std::process::id())).join(scene);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Renders every scene into out/<scale>x; how many it wrote.
pub fn render_all(out: &Path, scale: f32) -> Result<usize, String> {
    if !crate::offscreen::install() {
        return Err("a window system was already set up".into());
    }
    let folder = out.join(format!("{scale}x"));
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let mut n = 0;
    for &(scene, width) in SHELL_SCENES {
        let file = ConfigFile::at(scratch(scene).join(telescope_core::config::CONFIG_FILENAME));
        let w = window::build(file, crate::plugins::builtins(), None, None)?;
        let picture = grown(&w, width, scale).ok_or("nothing drawn")?;
        picture.save_png(&folder.join(format!("{scene}.png")))?;
        w.shut_down();
        n += 1;
    }
    let file = ConfigFile::at(scratch("shell-settings-plugins").join(telescope_core::config::CONFIG_FILENAME));
    let w = window::build(file, stand_ins(), None, None)?;
    let settings = crate::settings::build(&w)?;
    settings.show_tab("plugins");
    let picture = crate::offscreen::draw(settings.ui.window(), 940.0, 680.0, scale).ok_or("nothing drawn")?;
    picture.save_png(&folder.join("shell-settings-plugins.png"))?;
    Ok(n + 1)
}

/// The Python app's built-in plugins as they describe themselves (parity/builtins.json), standing in for the ones
/// not ported yet; the ported ones are the real thing.
pub fn stand_ins() -> Vec<Box<dyn Plugin>> {
    let infos: Vec<PluginInfo> = serde_json::from_str(include_str!("../parity/builtins.json")).unwrap_or_default();
    let mut real = crate::plugins::builtins();
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

/// text_fit._grow_to_content: the window at width, as tall as its columns need (900 at least).
fn grown(w: &window::Window, width: f32, scale: f32) -> Option<crate::offscreen::Picture> {
    use slint::ComponentHandle;
    crate::offscreen::draw(w.ui.window(), width, 900.0, scale)?;
    let content = match w.ui.get_property("content-height") {
        Ok(slint_interpreter::Value::Number(h)) => h as f32,
        _ => 0.0,
    };
    let height = (60.0 + 32.0 + content + 40.0).max(900.0);
    crate::offscreen::draw(w.ui.window(), width, height, scale)
}
