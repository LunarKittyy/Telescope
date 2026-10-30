//! A dev profile (dev_desktop's launcher, desktop/telescope/dev_profile.py): with TELESCOPE_DEV_PROFILE set to a folder, this copy
//! keeps its config and log there, runs next to the real Telescope and pairs with the dev phone app (com.telescope.dev) without
//! touching the real one's settings, menu entry, sign-in entry or mic.

use std::path::{Path, PathBuf};

pub use crate::config::{dev_profile as folder, DEV_PROFILE_ENV as ENV};

pub const PHONE_PACKAGE: &str = "com.telescope.dev";
/// Folders of plugins being written, loaded from where they are (plugin-host reads it).
pub const PLUGINS_ENV: &str = "TELESCOPE_DEV_PLUGINS";
pub const SUPERVISED_ENV: &str = "TELESCOPE_DEV_SUPERVISED";
pub const RESUME_ENV: &str = "TELESCOPE_DEV_RESUME";
/// The launcher starts this copy again.
pub const RESTART_EXIT: i32 = 75;
/// The same, and it starts streaming again once it's up.
pub const RESTART_STREAMING: i32 = 76;

pub fn active() -> bool {
    folder().is_some()
}

/// Started by the launcher, which starts it again when it exits with RESTART_EXIT, instead of it relaunching itself (which
/// would leave the terminal and delete a throwaway profile).
pub fn supervised() -> bool {
    active() && std::env::var(SUPERVISED_ENV).as_deref() == Ok("1")
}

/// The copy before this one was streaming when it restarted.
pub fn resume_stream() -> bool {
    supervised() && std::env::var(RESUME_ENV).as_deref() == Ok("1")
}

/// One file in a stamp: its folder, name, size and modification time in nanoseconds.
pub type Stamped = (PathBuf, String, u64, u128);

/// What changes when a file in folders is saved (path, size, time), leaving out caches and hidden folders.
pub fn files_stamp(folders: &[PathBuf]) -> Vec<Stamped> {
    let mut stamp = Vec::new();
    for folder in folders {
        walk(folder, &mut stamp);
    }
    stamp
}

fn walk(root: &Path, stamp: &mut Vec<Stamped>) {
    let Ok(list) = std::fs::read_dir(root) else { return };
    let (mut dirs, mut names) = (Vec::new(), Vec::new());
    for entry in list.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Like os.walk, a link to a folder counts as a folder it doesn't go into
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir { dirs.push(name) } else { names.push(name) }
    }
    names.sort();
    dirs.retain(|d| d != "__pycache__" && !d.starts_with('.'));
    dirs.sort();
    for name in names {
        if name.starts_with('.') || [".pyc", ".swp", "~"].iter().any(|end| name.ends_with(end)) {
            continue;
        }
        // Saved by renaming over it: the next look sees the new one
        let Ok(meta) = std::fs::metadata(root.join(&name)) else { continue };
        let nanos = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
        stamp.push((root.to_path_buf(), name, meta.len(), nanos));
    }
    for dir in dirs {
        walk(&root.join(dir), stamp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch_dir;

    #[test]
    fn a_saved_file_changes_the_stamp_and_caches_and_hidden_things_do_not() {
        let dir = scratch_dir("stamp");
        std::fs::write(dir.join("plugin.json"), "{}").unwrap();
        let before = files_stamp(std::slice::from_ref(&dir));
        assert_eq!(before.len(), 1);
        for ignored in [".hidden", "x.pyc", "x.swp", "x~"] {
            std::fs::write(dir.join(ignored), "x").unwrap();
        }
        std::fs::create_dir_all(dir.join("__pycache__")).unwrap();
        std::fs::write(dir.join("__pycache__/a"), "x").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/a"), "x").unwrap();
        assert_eq!(files_stamp(std::slice::from_ref(&dir)), before);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/main.lua"), "print(1)").unwrap();
        assert_eq!(files_stamp(std::slice::from_ref(&dir)).len(), 2);
        std::fs::write(dir.join("plugin.json"), "{\"a\": 1}").unwrap();
        assert_ne!(files_stamp(&[dir]), before);
    }
}
