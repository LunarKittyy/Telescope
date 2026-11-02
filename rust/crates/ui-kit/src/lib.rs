//! The Slint theme and shared widgets. A crate that builds .slint files maps "@ui-kit" to library_path() in its build script.

use std::path::{Path, PathBuf};

/// The name .slint files import the kit by: import { Card } from "@ui-kit/ui-kit.slint";
pub const LIBRARY: &str = "ui-kit";

/// The folder holding ui-kit.slint, theme.slint, icons.slint, widgets/ and icons/: the ui-kit folder beside the program in a
/// shipped build, where tools/build_windows.ps1 puts it, else the source tree's.
pub fn library_path() -> PathBuf {
    let exe = std::env::current_exe().ok();
    library_in(exe.as_deref().and_then(Path::parent))
}

/// library_path for a program in folder.
fn library_in(folder: Option<&Path>) -> PathBuf {
    let beside = folder.map(|f| f.join("ui-kit")).filter(|kit| kit.join("ui-kit.slint").is_file());
    beside.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shipped_build_finds_the_kit_beside_the_program() {
        let folder = std::env::temp_dir().join(format!("telescope-kit-{}", std::process::id()));
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
        assert_eq!(library_in(Some(&folder)), source, "a dev build: nothing beside it");
        std::fs::create_dir_all(folder.join("ui-kit")).unwrap();
        assert_eq!(library_in(Some(&folder)), source, "an empty folder isn't the kit");
        std::fs::write(folder.join("ui-kit/ui-kit.slint"), "").unwrap();
        assert_eq!(library_in(Some(&folder)), folder.join("ui-kit"));
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(library_in(None), source);
    }
}
