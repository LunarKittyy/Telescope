//! Going back with 4.4.2's own start.sh and update_guard.py (rust/fixtures/python-4.4.2, as tagged), a crash in the middle of the swap,
//! and the Python environment made before it. The launcher tests need a Python 3.12 or newer, which start.sh needs too, and skip without one.

use super::tests::{read, rename, rust_folder, write};
use super::*;
use crate::updates::archives::tarball;
use std::cell::{Cell, RefCell};
use std::os::unix::fs::PermissionsExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::Command;
use std::time::Duration;
use telescope_core::testing::scratch_dir;

const START_SH: &[u8] = include_bytes!("../../../fixtures/python-4.4.2/start.sh");
const UPDATE_GUARD: &[u8] = include_bytes!("../../../fixtures/python-4.4.2/update_guard.py");
/// What main.py does first, with the real guard: recover, then confirm once "the window is up" unless the test says it never gets there.
const MAIN_PY: &[u8] = b"import os, sys\nfrom pathlib import Path\nhere = Path(__file__).resolve().parent\nsys.path.insert(0, str(here))\nimport update_guard\n\
update_guard.TRIAL_GRACE_S = 0\nargv = update_guard.recover(here, 0, lambda: False)\nprint('RECOVER', argv, flush=True)\n\
if argv is None and not os.environ.get('TEST_NEVER_STARTS'):\n    update_guard.confirm(here)\n    print('CONFIRMED', flush=True)\n";

fn python_package(root: &Path) -> PathBuf {
    tarball(&root.join("Telescope-linux.tar.gz"), &[
        ("main.py", MAIN_PY), ("start.sh", START_SH), ("update_guard.py", UPDATE_GUARD), ("telescope/app.py", b"python app"),
        ("requirements.txt", b"numpy>=1\n"), ("requirements-nodeps.txt", b"aiortc==1\n"), ("constraints.txt", b"numpy==1\n"), ("Telescope.apk", b"apk"),
    ])
}

/// The path of a Python 3.12 or newer, as start.sh would find one.
fn python312() -> Option<PathBuf> {
    let candidate = ["python3.14", "python3.13", "python3.12", "python3"].into_iter().find(|c| run_python(c, NEW_ENOUGH))?;
    let out = Command::new(candidate).args(["-c", "import sys; print(sys.executable)"]).output().ok()?;
    Some(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// A stand-in for the commands prepare_python runs: `-m venv` makes a folder whose python3 is the real interpreter, which is what a venv's is; the rest succeed.
fn fake_run<'a>(interpreter: &'a Path, log: &'a RefCell<Vec<Vec<String>>>, fails_on: &'a str) -> impl Fn(&Path, &[String]) -> bool + 'a {
    move |_, argv| {
        log.borrow_mut().push(argv.to_vec());
        if argv.iter().any(|a| a == fails_on) {
            return false;
        }
        if argv[1..3] == ["-m", "venv"] {
            let bin = Path::new(&argv[3]).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::os::unix::fs::symlink(interpreter, bin.join("python3")).unwrap();
        }
        true
    }
}

fn start_sh(app: &Path, data: &Path, never_starts: bool) -> String {
    let mut start = Command::new(app.join("start.sh"));
    start.arg("--after-update").env("XDG_DATA_HOME", data).env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY");
    if never_starts {
        start.env("TEST_NEVER_STARTS", "1");
    }
    let out = start.output().unwrap();
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn checked(python: &str) -> impl Fn(&str, &str) -> bool + '_ {
    move |program, _| program == python
}

#[test]
fn the_launcher_finds_the_environment_ready_and_the_guard_keeps_a_build_that_starts() {
    let Some(interpreter) = python312() else { return eprintln!("skipped: no Python 3.12 or newer") };
    let root = scratch_dir("going-back-real-start");
    let (app, data, log) = (rust_folder(&root), root.join("data"), RefCell::new(Vec::new()));
    let prepare = |staging: &Path| prepare_python(staging, &data, &checked("python3"), &fake_run(&interpreter, &log, ""));
    install_from(&python_package(&root), &app, false, &prepare, &mut rename()).unwrap();
    assert_eq!(read(&app.join("main.py")), String::from_utf8_lossy(MAIN_PY));
    // The way start.sh would have made it: the interpreter it picked, pip, then the two requirement files with the constraints
    let log = log.borrow();
    assert_eq!(log[0][1..3], ["-m", "venv"]);
    assert!(log[1].ends_with(&["--upgrade".into(), "pip".into()]));
    assert!(log[2].ends_with(&["-r".into(), "requirements.txt".into(), "-c".into(), "constraints.txt".into()]), "{:?}", log[2]);
    assert!(log[3].ends_with(&["--no-deps".into(), "-r".into(), "requirements-nodeps.txt".into(), "-c".into(), "constraints.txt".into()]), "{:?}", log[3]);
    // The real launcher skips pip (its stamp matches what was written) and the real guard confirms the trial
    let out = start_sh(&app, &data, false);
    assert!(out.contains("RECOVER None") && out.contains("CONFIRMED") && out.contains("Launching..."), "{out}");
    assert!(!out.contains("Checking dependencies") && !out.contains("Setting up Telescope's Python environment"), "{out}");
    assert!(update_guard::read_journal(&app).is_none());
    assert_eq!(read(&app.join("telescope/app.py")), "python app");
}

#[test]
fn a_build_that_never_starts_is_rolled_back_by_the_real_guard_and_this_version_gets_its_settings_back() {
    let Some(interpreter) = python312() else { return eprintln!("skipped: no Python 3.12 or newer") };
    let root = scratch_dir("going-back-real-rollback");
    let (app, data, config) = (rust_folder(&root), root.join("data"), root.join("config"));
    std::fs::create_dir_all(&config).unwrap();
    let prepare = |staging: &Path| prepare_python(staging, &data, &checked("python3"), &fake_run(&interpreter, &RefCell::new(Vec::new()), ""));
    install_from(&python_package(&root), &app, false, &prepare, &mut rename()).unwrap();
    remember(&config, "nightly", true, None, 5000).unwrap();
    // First start: it takes the trial and never gets a window. The next one gives up on it
    assert!(start_sh(&app, &data, true).contains("RECOVER None"));
    assert_eq!(update_guard::read_journal(&app).unwrap()["started"], true);
    let out = start_sh(&app, &data, true);
    assert!(out.contains("RECOVER ['") && out.contains("start.sh"), "{out}");
    assert_eq!(read(&app.join("main.py")), "rust stub");
    assert_eq!(read(&app.join("start.sh")), "rust start");
    assert!(!app.join("update_guard.py").exists());
    assert_eq!(update_guard::failed_build(&app), Some(BUILD));
    assert_eq!(settle(&config, &app, 5000), Some(("nightly".to_string(), true, true)));
}

#[test]
fn a_crash_before_the_launcher_moved_is_finished_by_this_version_which_hands_over_and_keeps_out_of_the_way() {
    let root = scratch_dir("going-back-crash");
    let app = rust_folder(&root);
    // Killed just as start.sh would be replaced, the last of the files: by then everything it starts must be in place
    let ready = Cell::new(false);
    let crashing = &mut |a: &Path, b: &Path| {
        if a.file_name().is_some_and(|n| n == "start.sh") {
            ready.set(read(&app.join("main.py")) == String::from_utf8_lossy(MAIN_PY) && app.join("update_guard.py").is_file() && app.join("telescope/app.py").is_file());
            panic!("killed");
        }
        std::fs::rename(a, b)
    };
    let died = catch_unwind(AssertUnwindSafe(|| install_from(&python_package(&root), &app, false, &|_| Ok(()), crashing)));
    assert!(died.is_err());
    assert!(ready.get(), "the launcher must be the commit point");
    assert_eq!(read(&app.join("start.sh")), "rust start");
    assert_eq!(update_guard::read_journal(&app).unwrap()["state"], "swapping");
    // Opening Telescope again runs this version (start.sh is still its own): it finishes the swap and starts the Python build through its launcher
    let relaunch = update_guard::recover(&app, Duration::ZERO, &|| false, Duration::ZERO).expect("hands over");
    assert_eq!(relaunch, vec![app.join("start.sh").to_string_lossy().into_owned(), "--after-update".into()]);
    assert!(read(&app.join("start.sh")).contains("Telescope launcher"));
    let journal = update_guard::read_journal(&app).unwrap();
    assert_eq!((journal["state"].as_str(), journal["started"].as_bool()), (Some("trial"), Some(false)));
    // And nothing of this version's start-up confirms or cleans up the Python build's trial
    update_guard::confirm(&app);
    update_guard::clean_up_after_update(&app, Some(5000));
    assert!(update_guard::read_journal(&app).is_some());
    for kept in ["update_guard.py", "requirements.txt", "requirements-nodeps.txt", "constraints.txt"] {
        assert!(app.join(kept).is_file(), "{kept} was deleted");
    }
    assert_eq!(update_guard::python_venv_left(&app, Some(5000), &root.join("data")), None);
}

#[test]
fn this_binary_started_directly_while_the_python_build_is_on_trial_hands_over_instead_of_confirming_it() {
    let root = scratch_dir("going-back-direct");
    let app = rust_folder(&root);
    install_from(&python_package(&root), &app, false, &|_| Ok(()), &mut rename()).unwrap();
    let wanted = vec![app.join("start.sh").to_string_lossy().into_owned(), "--after-update".into()];
    assert_eq!(update_guard::recover(&app, Duration::ZERO, &|| false, Duration::ZERO), Some(wanted.clone()));
    assert_eq!(update_guard::read_journal(&app).unwrap()["started"], false, "the trial is the Python build's to start");
    // Without the Python guard beside it there'd be nothing to hand over to, so this copy carries on as for any update
    std::fs::remove_file(app.join("update_guard.py")).unwrap();
    assert_eq!(update_guard::recover(&app, Duration::ZERO, &|| false, Duration::ZERO), None);
}

#[test]
fn a_launcher_is_replaced_last_by_any_update() {
    let root = scratch_dir("going-back-order");
    let app = rust_folder(&root);
    let moved = RefCell::new(Vec::new());
    let mut recording = |a: &Path, b: &Path| {
        if a.to_string_lossy().contains(update_guard::STAGING_DIR) {
            moved.borrow_mut().push(a.file_name().unwrap().to_string_lossy().into_owned());
        }
        std::fs::rename(a, b)
    };
    install_from(&python_package(&root), &app, false, &|_| Ok(()), &mut recording).unwrap();
    let moved = moved.borrow();
    assert_eq!(moved.iter().position(|n| n == "start.sh"), Some(moved.len() - 1), "{moved:?}");
}

#[test]
fn the_environment_is_made_like_start_sh_would_and_stamped_for_it() {
    let root = scratch_dir("going-back-prepare");
    let (data, staging) = (root.join("data"), root.join("staging"));
    write(&staging.join("requirements.txt"), b"a\n");
    write(&staging.join("requirements-nodeps.txt"), b"b\n");
    let python = std::env::current_exe().unwrap();
    let log = RefCell::new(Vec::new());
    prepare_python(&staging, &data, &checked("python3.13"), &fake_run(&python, &log, "")).unwrap();
    assert_eq!(log.borrow()[0][0], "python3.13");
    // No constraints.txt in the package: no -c, and no stamp part for it
    assert!(log.borrow().iter().all(|argv| !argv.contains(&"-c".to_string())));
    let stamp = read(&data.join("Telescope/venv/.telescope-requirements"));
    assert_eq!(stamp, format!("{}\n", cksum(b"a\nb\n")));
    if let Ok(out) = Command::new("cksum").arg("-").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().and_then(|mut child| {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"a\nb\n")?;
        child.wait_with_output()
    }) {
        assert_eq!(stamp.trim(), String::from_utf8_lossy(&out.stdout).trim().trim_end_matches(" -"));
    }
    // The POSIX check value
    assert_eq!(cksum(b"123456789"), "930766865 9");
    assert_eq!(cksum(b""), "4294967295 0");
}

#[test]
fn a_failed_setup_removes_what_it_made_and_says_nothing_was_changed() {
    let root = scratch_dir("going-back-prepare-fails");
    let (data, staging) = (root.join("data"), root.join("staging"));
    write(&staging.join("requirements.txt"), b"a\n");
    write(&staging.join("requirements-nodeps.txt"), b"b\n");
    let python = std::env::current_exe().unwrap();
    let venv = data.join("Telescope/venv");
    // No internet for pip, no room for the environment, or no aiortc: each leaves nothing behind
    for fails_on in ["venv", "requirements.txt", "requirements-nodeps.txt"] {
        let e = prepare_python(&staging, &data, &checked("python3"), &fake_run(&python, &RefCell::new(Vec::new()), fails_on)).unwrap_err();
        assert!(e.0.contains("Nothing was changed") && e.0.contains(&data.join("Telescope").display().to_string()), "{e}");
        assert!(!venv.exists(), "left behind after {fails_on}");
    }
    // A newer pip that can't be had doesn't stop it, as in start.sh
    prepare_python(&staging, &data, &checked("python3"), &fake_run(&python, &RefCell::new(Vec::new()), "--upgrade")).unwrap();
    // One that can't run any more is made again, from scratch
    write(&venv.join("old-file"), b"x");
    std::fs::remove_file(venv.join(".telescope-requirements")).unwrap();
    prepare_python(&staging, &data, &checked("python3"), &fake_run(&python, &RefCell::new(Vec::new()), "")).unwrap();
    assert!(!venv.join("old-file").exists() && venv.join(".telescope-requirements").is_file());
    // No Python that start.sh would pick
    assert!(prepare_python(&staging, &data, &|_, _| false, &|_, _| true).unwrap_err().0.contains("needs Python 3.12"));
}

#[test]
fn a_setup_that_fails_stops_the_swap_before_a_file_is_touched() {
    let root = scratch_dir("going-back-no-setup");
    let app = rust_folder(&root);
    let e = install_from(&python_package(&root), &app, false, &|_| Err(UpdateError("Couldn't set up".into())), &mut rename()).unwrap_err();
    assert_eq!(e.0, "Couldn't set up");
    assert_eq!(read(&app.join("main.py")), "rust stub");
    assert_eq!(read(&app.join("start.sh")), "rust start");
    for none in [".update-staging", ".previous", ".update.json", "update_guard.py", "Telescope.apk"] {
        assert!(!app.join(none).exists(), "{none}");
    }
}

#[test]
fn the_script_in_the_launcher_stays_executable_and_runnable() {
    let root = scratch_dir("going-back-mode");
    let app = rust_folder(&root);
    install_from(&python_package(&root), &app, false, &|_| Ok(()), &mut rename()).unwrap();
    assert_eq!(std::fs::metadata(app.join("start.sh")).unwrap().permissions().mode() & 0o111, 0o111);
}

#[test]
fn a_note_from_a_going_back_that_never_got_to_swap_gives_the_settings_back_without_saying_the_python_build_failed() {
    let root = scratch_dir("going-back-never-swapped");
    let (app, config) = (rust_folder(&root), root.join("config"));
    std::fs::create_dir_all(&config).unwrap();
    // Killed while it downloaded: updates were already off, and the note was written first
    remember(&config, "nightly", true, None, 5000).unwrap();
    assert_eq!(settle(&config, &app, 5000), Some(("nightly".to_string(), true, false)));
    assert!(!config.join(MARKER).exists());
    // A swap that failed forgets it itself
    remember(&config, "nightly", true, None, 5000).unwrap();
    forget(&config);
    assert_eq!(settle(&config, &app, 5000), None);
}
