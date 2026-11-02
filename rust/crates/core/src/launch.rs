//! Starting other people's programs (a browser, the desktop's settings). They shouldn't inherit what's private to this
//! process: our own TELESCOPE_ settings, and what an AppImage or start.sh added so that this program finds its bundled
//! libraries, which can break a Qt or GTK app that loads them instead of its own. The person's own environment stays.
//! Copies of Telescope itself keep everything: they don't start through here.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What AppRun tells the program it started, which a child would take for its own.
const APPIMAGE_MARKS: [&str; 4] = ["APPDIR", "APPIMAGE", "ARGV0", "OWD"];

/// What to change in `env` for a child, given the folders that are private to this program: names to take away, and
/// names to give a shorter value. In any variable the entries inside a private folder go and the rest stay.
fn changes(env: &[(OsString, OsString)], private: &[PathBuf]) -> (Vec<OsString>, Vec<(OsString, OsString)>) {
    let (mut remove, mut set) = (Vec::new(), Vec::new());
    let bundled = env.iter().any(|(name, _)| name == "APPDIR");
    for (name, value) in env {
        let text = name.to_string_lossy();
        if text.starts_with("TELESCOPE_") || (bundled && APPIMAGE_MARKS.contains(&&*text)) {
            remove.push(name.clone());
            continue;
        }
        let entries: Vec<PathBuf> = std::env::split_paths(value).collect();
        let kept: Vec<&PathBuf> = entries.iter().filter(|e| !private.iter().any(|p| e.starts_with(p))).collect();
        if kept.len() == entries.len() {
            continue;
        }
        match std::env::join_paths(kept) {
            Ok(shorter) if !shorter.is_empty() => set.push((name.clone(), shorter)),
            _ => remove.push(name.clone()),
        }
    }
    (remove, set)
}

/// The folders holding what only this program should load: the mounted AppImage, and start.sh's fallback libraries.
fn private_folders(appdir: Option<OsString>, exe: Option<&Path>) -> Vec<PathBuf> {
    let appdir = appdir.filter(|d| !d.is_empty()).map(PathBuf::from);
    let lib = exe.and_then(Path::parent).map(|dir| dir.join("lib"));
    let fallback = lib.iter().flat_map(|lib| [lib.join("fallback"), lib.join("fallback-fonts")]);
    appdir.into_iter().chain(fallback).collect()
}

/// A Command for a program that isn't ours, without what's private to this process.
pub fn external(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    let env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let private = private_folders(std::env::var_os("APPDIR"), std::env::current_exe().ok().as_deref());
    let (remove, set) = changes(&env, &private);
    for name in remove {
        command.env_remove(name);
    }
    command.envs(set);
    command
}

/// Starts a program that carries on without us, its output going nowhere.
pub fn detached(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> std::io::Result<std::process::Child> {
    external(program).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
}

/// Opens a link in the default browser.
pub fn open_url(url: &str) {
    let spawned = if cfg!(windows) {
        external("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn()
    } else {
        external("xdg-open").arg(url).spawn()
    };
    if let Err(e) = spawned {
        log::warn!("Couldn't open {url}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs.iter().map(|(a, b)| (a.into(), b.into())).collect()
    }

    #[test]
    fn our_own_settings_stay_here_and_the_persons_go_on() {
        let (remove, set) = changes(&env(&[("TELESCOPE_RS_PROFILE", "/x"), ("TELESCOPE_DEV", "1"), ("XDG_CONFIG_HOME", "/home/me/test"), ("QT_STYLE_OVERRIDE", "kvantum"), ("HOME", "/home/me")]), &[]);
        assert_eq!(remove, ["TELESCOPE_RS_PROFILE", "TELESCOPE_DEV"]);
        assert!(set.is_empty());
    }

    #[test]
    fn only_the_bundles_part_of_a_path_goes() {
        let private = [PathBuf::from("/tmp/.mount_tele123")];
        let (remove, set) = changes(&env(&[
            ("LD_LIBRARY_PATH", "/tmp/.mount_tele123/usr/lib:/opt/mine/lib"),
            ("QT_PLUGIN_PATH", "/tmp/.mount_tele123/usr/plugins"),
            ("QT_QPA_PLATFORMTHEME", "kde"),
            ("PATH", "/usr/bin:/bin"),
        ]), &private);
        assert_eq!(remove, ["QT_PLUGIN_PATH"]);
        assert_eq!(set, env(&[("LD_LIBRARY_PATH", "/opt/mine/lib")]));
    }

    #[test]
    fn appimage_marks_go_only_under_an_appimage() {
        let (remove, _) = changes(&env(&[("OWD", "/home/me")]), &[]);
        assert!(remove.is_empty(), "somebody else's OWD");
        let (remove, _) = changes(&env(&[("APPDIR", "/tmp/.mount_x"), ("APPIMAGE", "/home/me/t.AppImage"), ("OWD", "/home/me"), ("ARGV0", "t")]), &[PathBuf::from("/tmp/.mount_x")]);
        assert_eq!(remove.len(), 4);
    }

    #[test]
    fn the_private_folders_are_the_appimage_and_the_fallback_libraries() {
        let found = private_folders(Some("/tmp/.mount_x".into()), Some(Path::new("/opt/telescope/telescope-rs")));
        assert_eq!(found, [PathBuf::from("/tmp/.mount_x"), PathBuf::from("/opt/telescope/lib/fallback"), PathBuf::from("/opt/telescope/lib/fallback-fonts")]);
        assert_eq!(private_folders(Some("".into()), None), Vec::<PathBuf>::new());
    }
}
