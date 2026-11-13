//! Going back to the Python version: the pinned release, what each kind of copy does, and that a failure leaves this version working.

use super::*;
use crate::updates::archives::{tarball, zip_of};
use crate::updates::{Reply, Updates};
use serde_json::json;
use telescope_core::config::ConfigFile;
use telescope_core::testing::scratch_dir;

/// The assets of v4.4.2's manifest.json, as published.
const MANIFEST: &str = r#"{"schema": 1, "version": "4.4.2", "build": 1441, "channel": "stable", "versionName": "4.4.2", "protocol": 3,
    "notes": "https://github.com/LunarKittyy/Telescope/releases/tag/v4.4.2", "assets": [
    {"name": "Telescope-windows-portable.zip", "url": "https://github.com/LunarKittyy/Telescope/releases/download/v4.4.2/Telescope-windows-portable.zip",
     "sha256": "2ef523d90a7ca1fe1e029dd4fd23e93bfd392090ce71b4f4b7389baceab00853", "size": 148795816},
    {"name": "Telescope-linux.tar.gz", "url": "https://github.com/LunarKittyy/Telescope/releases/download/v4.4.2/Telescope-linux.tar.gz",
     "sha256": "6acbc37dd8cb8fdf494ca02978a92d679129e3571a707c6124754d1b1bfcf909", "size": 7108274}]}"#;

pub(super) fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

pub(super) fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

pub(super) fn rename() -> impl FnMut(&Path, &Path) -> io::Result<()> {
    |a: &Path, b: &Path| std::fs::rename(a, b)
}

/// This version's Linux package folder.
pub(super) fn rust_folder(root: &Path) -> PathBuf {
    let app = root.join("app");
    write(&app.join("telescope-rs"), b"rust binary");
    write(&app.join("start.sh"), b"rust start");
    write(&app.join("main.py"), b"rust stub");
    write(&app.join("telescope/_build.py"), b"rust stamp");
    write(&app.join("lib/fallback/libva.so"), b"lib");
    app
}

/// What the 4.4.2 tarball holds.
const PYTHON_LINUX: [(&str, &[u8]); 9] = [
    ("main.py", b"python main"), ("start.sh", b"#!/bin/sh\n"), ("update_guard.py", b"python guard"), ("telescope/app.py", b"python app"),
    ("requirements.txt", b"x"), ("requirements-nodeps.txt", b"x"), ("constraints.txt", b"x"), ("Telescope.apk", b"apk"),
    ("THIRD_PARTY_NOTICES.txt", b"notices"),
];

/// What 4.4.2's main.py does first, update_guard.recover(), for a trial: the first start marks it started, the next one rolls it back
/// (this app's own guard would hand over to the Python build instead; going_back_python_tests runs the real one).
fn python_start(app: &Path) -> Option<Vec<String>> {
    let mut journal = update_guard::read_journal(app)?;
    if journal["started"] == true {
        update_guard::roll_back(app, &journal, true, &mut rename()).unwrap();
        return Some(vec![app.join("start.sh").to_string_lossy().into_owned()]);
    }
    journal["started"] = json!(true);
    update_guard::write_journal(app, &journal).unwrap();
    None
}

#[test]
fn the_pinned_files_are_the_ones_in_the_releases_manifest() {
    let manifest = updates::parse_manifest(MANIFEST.as_bytes()).unwrap();
    assert_eq!((manifest.version.as_str(), manifest.build, manifest.channel.as_str()), (VERSION, BUILD, "stable"));
    assert_eq!(manifest.assets[updates::LINUX_ASSET], pinned_asset(false));
    assert_eq!(manifest.assets[updates::WINDOWS_ASSET], pinned_asset(true));
    assert!(release_url().ends_with("/releases/tag/v4.4.2"));
}

#[test]
fn a_file_that_isnt_the_pinned_one_is_refused() {
    let dir = scratch_dir("going-back-checksum");
    let mut asset = pinned_asset(false);
    asset.size = 4096;
    // Right size, wrong contents: what a swapped release file would look like
    let opener = |_: &str| Ok(Reply { status: 200, body: Box::new(io::Cursor::new(vec![7u8; 4096])) });
    let updates = Updates { opener: &opener, build: 1, directory: dir.clone() };
    let e = updates.download(&asset, &dir.join("dl"), &mut |_, _| {}, &|| false).unwrap_err();
    assert!(e.0.contains("checksum"), "{e}");
    assert!(!dir.join("dl").join(&asset.name).exists());
}

#[test]
fn a_source_checkout_never_offers_it_and_an_appimage_says_what_to_download() {
    let dir = scratch_dir("going-back-plan");
    let python = || PythonState::Ready;
    assert_eq!(plan("dev", &dir, false, false, &python), Plan::Hidden);
    assert!(!plan("dev", &dir, false, true, &python).offered());
    assert_eq!(plan("nightly", &dir, true, false, &python), Plan::Download { appimage: true });
    assert_eq!(plan("stable", &dir, false, false, &python), Plan::Swap { python_setup: false });
    assert_eq!(plan("stable", &dir, false, true, &python), Plan::Swap { python_setup: false });
    // Windows never asks about Python
    assert_eq!(plan("nightly", &dir, false, true, &|| panic!("asked Python on Windows")), Plan::Swap { python_setup: false });
    assert_eq!(plan("nightly", &dir.join("missing"), false, false, &python), Plan::Download { appimage: false });
}

#[test]
fn linux_needs_a_python_the_launcher_can_use_or_it_says_so() {
    let dir = scratch_dir("going-back-python");
    let state = |p: PythonState| plan("stable", &dir, false, false, &|| p);
    assert_eq!(state(PythonState::NeedsSetup), Plan::Swap { python_setup: true });
    assert_eq!(state(PythonState::Missing), Plan::NoPython);
    assert_eq!(state(PythonState::Ready), Plan::Swap { python_setup: false });
}

#[test]
fn the_launchers_own_checks_decide_the_python_state() {
    let data = scratch_dir("going-back-data");
    let tried = std::cell::RefCell::new(Vec::<String>::new());
    let with = |works: &[&str], venv_ok: bool| {
        tried.borrow_mut().clear();
        python_state(&data, &|program, code| {
            tried.borrow_mut().push(program.to_string());
            works.contains(&program) && (!code.contains("ensurepip") || venv_ok)
        })
    };
    // Nothing new enough anywhere: the old 3.10 a Debian box has
    assert_eq!(with(&[], true), PythonState::Missing);
    assert_eq!(*tried.borrow(), ["python3.14", "python3.13", "python3.12", "python3"]);
    // The first one that's new enough is the one the launcher makes the environment with, so that one must be able to
    assert_eq!(with(&["python3.13", "python3"], true), PythonState::NeedsSetup);
    assert_eq!(with(&["python3.13", "python3"], false), PythonState::Missing);
    // The environment from before, if it still runs
    let venv = data.join("Telescope").join("venv").join("bin").join("python3");
    write(&venv, b"#!/bin/sh");
    let venv_name = venv.to_str().unwrap();
    assert_eq!(with(&[venv_name], false), PythonState::Ready);
    // One made with a Python that has since been replaced by an old one isn't Ready: the launcher needs a new Python to make it again
    assert_eq!(with(&[], false), PythonState::Missing);
    assert_eq!(with(&["python3"], true), PythonState::NeedsSetup);
}

#[test]
fn the_confirmation_says_what_happens_what_is_lost_and_what_the_phone_needs() {
    let folder = Path::new("/home/me/.config/telescope");
    let text = dialog(Plan::Swap { python_setup: false }, folder).text;
    for wanted in ["4.4.2", "/home/me/.config/telescope", "copy of your current settings", "Lua", "ignored", "phone app has to match", "Telescope.apk",
                   "uninstall Telescope on the phone", "doesn't check for updates", "brings this version back"] {
        assert!(text.contains(wanted), "missing {wanted:?}: {text}");
    }
    assert!(!text.contains("Python libraries"));
    let first_start = dialog(Plan::Swap { python_setup: true }, folder);
    assert!(first_start.text.contains("the Python libraries 4.4.2 needs are set up") && first_start.text.contains("nothing is changed") && first_start.enabled && first_start.action == "Go back");
    let appimage = dialog(Plan::Download { appimage: true }, folder);
    assert!(appimage.text.contains("Telescope-linux.tar.gz") && appimage.text.contains("Python 3.12") && appimage.text.contains("phone app has to match"));
    assert!(appimage.enabled && appimage.action == "Open download page");
    let blocked = dialog(Plan::NoPython, folder);
    assert!(!blocked.enabled && blocked.text.contains("python3-venv") && blocked.text.contains("Nothing was changed"));
    assert_eq!(dialog(Plan::Hidden, folder).text, "");
}

#[test]
fn the_settings_copy_is_dated_private_and_never_replaces_an_earlier_one() {
    let dir = scratch_dir("going-back-backup");
    let file = ConfigFile::at(dir.join("telescope_config.json"));
    assert_eq!(back_up_settings(&file).unwrap(), None, "nothing to copy yet");
    write(&dir.join("telescope_config.json"), br#"{"version": 3}"#);
    let first = back_up_settings(&file).unwrap().unwrap();
    let second = back_up_settings(&file).unwrap().unwrap();
    assert_ne!(first, second);
    for copy in [&first, &second] {
        assert_eq!(read(copy), r#"{"version": 3}"#);
        let name = copy.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("telescope_config.json.rust-20"), "{name}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(copy).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
    assert_eq!(read(&dir.join("telescope_config.json")), r#"{"version": 3}"#);
}

#[test]
fn a_settings_file_that_cant_be_copied_stops_everything_before_a_file_is_touched() {
    let dir = scratch_dir("going-back-backup-fails");
    // A folder where the file should be: reading it fails, as a locked or unreadable file does
    std::fs::create_dir_all(dir.join("telescope_config.json")).unwrap();
    let e = back_up_settings(&ConfigFile::at(dir.join("telescope_config.json"))).unwrap_err();
    assert!(e.0.contains("nothing was changed"), "{e}");
}

#[test]
fn going_back_on_linux_swaps_the_python_files_in_and_keeps_this_version_to_roll_back_to() {
    let root = scratch_dir("going-back-linux");
    let app = rust_folder(&root);
    let archive = tarball(&root.join("Telescope-linux.tar.gz"), &PYTHON_LINUX);
    let result = install_from(&archive, &app, false, &|_| Ok(()), &mut rename()).unwrap();
    assert_eq!(read(&app.join("main.py")), "python main");
    assert_eq!(read(&app.join("telescope/app.py")), "python app");
    assert!(!app.join("telescope/_build.py").exists());
    assert_eq!(read(&app.join("Telescope.apk")), "apk");
    // The package's own entries only: the binary and its libraries stay, which is what a roll-back or a later return needs
    assert_eq!(read(&app.join("telescope-rs")), "rust binary");
    assert!(app.join("lib/fallback/libva.so").exists());
    assert_eq!(read(&app.join(".previous/start.sh")), "rust start");
    assert_eq!(result.relaunch, vec![app.join("start.sh").to_string_lossy().into_owned(), "--after-update".into()]);
    let journal = update_guard::read_journal(&app).unwrap();
    assert_eq!((journal["state"].as_str(), journal["to_build"].as_i64(), journal["platform"].as_str()), (Some("trial"), Some(BUILD), Some("linux")));
    assert!(journal.get("appimage").is_none(), "the Python build's guard doesn't know AppImages");
}

#[test]
fn going_back_on_windows_swaps_the_exe_and_the_libraries_without_starting_the_new_exe() {
    let root = scratch_dir("going-back-windows");
    let app = root.join("app");
    write(&app.join(update_guard::EXE_NAME), b"rust exe");
    write(&app.join("ffmpeg.dll"), b"rust only");
    let archive = zip_of(&root.join(updates::WINDOWS_ASSET), &[(update_guard::EXE_NAME, b"this is not a program"), ("lib-1441/PyQt6/Qt6Core.dll", b"qt"), ("Telescope.apk", b"apk")]);
    let result = install_from(&archive, &app, true, &|_| Ok(()), &mut rename()).unwrap();
    assert_eq!(read(&app.join(update_guard::EXE_NAME)), "this is not a program");
    assert_eq!(read(&app.join(update_guard::OLD_EXE_NAME)), "rust exe");
    assert!(app.join("lib-1441/PyQt6/Qt6Core.dll").exists());
    assert_eq!(read(&app.join("ffmpeg.dll")), "rust only");
    assert_eq!(result.relaunch, vec![app.join(update_guard::EXE_NAME).to_string_lossy().into_owned(), "--after-update".into()]);
    assert_eq!(update_guard::read_journal(&app).unwrap()["to_build"], BUILD);
}

#[cfg(unix)]
#[test]
fn a_swap_that_fails_midway_leaves_this_version_working() {
    let root = scratch_dir("going-back-midway");
    let app = rust_folder(&root);
    let archive = tarball(&root.join("Telescope-linux.tar.gz"), &PYTHON_LINUX);
    let moved = std::cell::Cell::new(0);
    // The disk fills up after some of the Python files are in
    let mut flaky = |a: &Path, b: &Path| {
        if a.to_string_lossy().contains(update_guard::STAGING_DIR) {
            moved.set(moved.get() + 1);
            if moved.get() == 4 {
                return Err(io::Error::from_raw_os_error(28));
            }
        }
        std::fs::rename(a, b)
    };
    let e = install_from(&archive, &app, false, &|_| Ok(()), &mut flaky).unwrap_err();
    assert!(e.0.contains("No space left"), "{e}");
    assert!(moved.get() >= 4);
    assert_eq!(read(&app.join("main.py")), "rust stub");
    assert_eq!(read(&app.join("start.sh")), "rust start");
    assert_eq!(read(&app.join("telescope/_build.py")), "rust stamp");
    assert_eq!(read(&app.join("telescope-rs")), "rust binary");
    for added in ["update_guard.py", "requirements.txt", "constraints.txt", "Telescope.apk", "THIRD_PARTY_NOTICES.txt", ".update-staging"] {
        assert!(!app.join(added).exists(), "{added} was left behind");
    }
    assert!(update_guard::read_journal(&app).is_none());
}

#[test]
fn a_python_version_that_never_starts_is_rolled_back_by_its_own_guard_and_the_settings_come_back() {
    let root = scratch_dir("going-back-rollback");
    let (app, config) = (rust_folder(&root), root.join("config"));
    std::fs::create_dir_all(&config).unwrap();
    install_from(&tarball(&root.join("l.tar.gz"), &PYTHON_LINUX), &app, false, &|_| Ok(()), &mut rename()).unwrap();
    remember(&config, "nightly", true, Some(&config.join("telescope_config.json.rust-1")), 5000).unwrap();
    // 4.4.2's first start takes the trial and dies before its window; the next start gives up on it
    assert_eq!(python_start(&app), None);
    assert_eq!(read(&app.join("main.py")), "python main");
    let relaunch = python_start(&app).expect("rolled back");
    assert_eq!(relaunch, vec![app.join("start.sh").to_string_lossy().into_owned()]);
    assert_eq!(read(&app.join("main.py")), "rust stub");
    assert_eq!(read(&app.join("start.sh")), "rust start");
    assert_eq!(read(&app.join("telescope/_build.py")), "rust stamp");
    assert!(!app.join("update_guard.py").exists());
    assert_eq!(update_guard::failed_build(&app), Some(BUILD));
    // This version starts again and finds out
    assert_eq!(settle(&config, &app, 5000), Some(("nightly".to_string(), true, true)));
    assert_eq!(update_guard::failed_build(&app), None, "a later try isn't held back");
    assert!(!config.join(MARKER).exists());
    assert_eq!(settle(&config, &app, 5000), None);
}

#[test]
fn a_going_back_that_started_fine_stays_and_this_versions_guard_does_not_undo_it() {
    let root = scratch_dir("going-back-stays");
    let (app, config) = (rust_folder(&root), root.join("config"));
    std::fs::create_dir_all(&config).unwrap();
    install_from(&tarball(&root.join("l.tar.gz"), &PYTHON_LINUX), &app, false, &|_| Ok(()), &mut rename()).unwrap();
    remember(&config, "stable", true, None, 5000).unwrap();
    assert_eq!(python_start(&app), None);
    // 4.4.2's confirm() is clearing the journal; this app's own never does it for a Python build on trial
    update_guard::confirm(&app);
    assert!(update_guard::read_journal(&app).is_some());
    update_guard::clear_journal(&app).unwrap();
    // Any later start of either version finds nothing to undo
    assert_eq!(python_start(&app), None);
    assert!(update_guard::guard_start(&app, false));
    assert_eq!(read(&app.join("main.py")), "python main");
    // Months later the person updates to a newer Rust build through 4.4.2's Update button: the old note is just dropped
    assert_eq!(settle(&config, &app, 6000), None);
    assert!(!config.join(MARKER).exists());
}

#[test]
fn the_note_survives_what_a_hand_edit_would_break() {
    let root = scratch_dir("going-back-note");
    let (app, config) = (root.join("app"), root.join("config"));
    std::fs::create_dir_all(&app).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(app.join(update_guard::FAILED), json!({"build": BUILD}).to_string()).unwrap();
    std::fs::write(config.join(MARKER), r#"{"channel": "nope", "auto_check": "x"}"#).unwrap();
    assert_eq!(settle(&config, &app, 5000), None);
    std::fs::write(app.join(update_guard::FAILED), json!({"build": BUILD}).to_string()).unwrap();
    std::fs::write(config.join(MARKER), "not json").unwrap();
    assert_eq!(settle(&config, &app, 5000), None);
    // Rolled back, but for another build: not ours to answer
    std::fs::write(app.join(update_guard::FAILED), json!({"build": 99999}).to_string()).unwrap();
    std::fs::write(config.join(MARKER), r#"{"channel": "stable", "auto_check": false}"#).unwrap();
    assert_eq!(settle(&config, &app, 5000), None);
    assert_eq!(update_guard::failed_build(&app), Some(99999));
}
