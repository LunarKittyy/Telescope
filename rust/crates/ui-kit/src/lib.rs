//! The Slint theme and shared widgets. A crate that builds .slint files maps "@ui-kit" to library_path() in its build script.

use std::path::PathBuf;

/// The name .slint files import the kit by: import { Card } from "@ui-kit/ui-kit.slint";
pub const LIBRARY: &str = "ui-kit";

/// The folder holding ui-kit.slint, theme.slint, icons.slint, widgets/ and icons/.
pub fn library_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui")
}

/// The kit's .slint files, for a build script's rerun-if-changed and for the plugin host's interpreter.
pub fn slint_files() -> Vec<PathBuf> {
    let ui = library_path();
    let mut files = vec![ui.join("ui-kit.slint"), ui.join("theme.slint"), ui.join("icons.slint")];
    if let Ok(entries) = std::fs::read_dir(ui.join("widgets")) {
        let mut widgets: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        widgets.sort();
        files.extend(widgets);
    }
    files
}
