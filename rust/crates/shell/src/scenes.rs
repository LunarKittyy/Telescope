//! The window in the same states as the Qt scene renders, drawn offscreen (offscreen.rs). Each scene gets a fresh
//! settings file in a scratch folder, like translation_scenes.use_scratch_profile.

use crate::window;
use std::path::Path;
use telescope_core::config::ConfigFile;

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
    Ok(n)
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
