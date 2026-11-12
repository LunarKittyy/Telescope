// The real program started the way main.py is in test_main.py. Rust has no PyAV build to stage, so what a command or a duplicate launch must leave
// alone is what Rust has of that kind: the stale mic source a fresh start clears, here a pactl that leaves a mark.
#![cfg(target_os = "linux")]

use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::config::{self, DEV_PROFILE_ENV};
use telescope_system::single_instance as si;

/// A PATH that finds this script as pactl first.
fn path_with_pactl(dir: &Path, script: &str) -> String {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let pactl = bin.join("pactl");
    std::fs::write(&pactl, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&pactl, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default())
}

/// The program in this profile, with no display to open a window on.
fn run(profile: &Path, path: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_telescope-rs")).args(args).env("PATH", path).env(DEV_PROFILE_ENV, profile)
        .env(si::CLAIM_SCOPE_ENV, profile.parent().and_then(|d| d.file_name()).unwrap_or_default()).env_remove("DISPLAY").env_remove("WAYLAND_DISPLAY").output().unwrap()
}

/// One at a time: the mic's source name is claimed machine-wide, and a copy that finds it taken leaves it alone whatever else it does wrong.
static STARTS: Mutex<()> = Mutex::new(());

const COMMANDS: [&[&str]; 2] = [&["--list-actions"], &["--action", "microphone.mute", "mode=toggle"]];

// test_main.py::test_commands_and_duplicate_launches_leave_a_staged_build_alone, with nothing running
#[test]
fn commands_leave_what_the_running_copy_owns_alone() {
    let dir = telescope_core::testing::scratch_dir("commands");
    let called = dir.join("pactl-called");
    let path = path_with_pactl(&dir, &format!("touch '{}'", called.display()));
    for args in COMMANDS {
        let out = run(&dir.join("profile"), &path, args);
        // Nothing is running in this profile, so it says so and stops
        assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(!called.exists(), "{args:?}");
    }
}

// The same test's other half: commands forwarded to a running copy, and a second launch that only raises it
#[test]
fn commands_and_a_duplicate_launch_leave_a_running_copy_alone() {
    let _one = STARTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = telescope_core::testing::scratch_dir("commands-running");
    let (called, profile) = (dir.join("pactl-called"), dir.join("profile"));
    let path = path_with_pactl(&dir, &format!("touch '{}'", called.display()));
    // The running copy: this test, on the lock of that profile
    std::env::set_var(DEV_PROFILE_ENV, &profile);
    std::fs::create_dir_all(&profile).unwrap();
    let si::Acquired::Lock(lock) = si::acquire(&si::Address::current(), Duration::ZERO) else { panic!("the lock") };
    let key = si::write_control_key(&si::control_key_path(&config::config_path()));
    assert!(key.is_some());
    let asked: Arc<Mutex<Vec<Value>>> = Arc::default();
    let raised = Arc::new(AtomicU32::new(0));
    let (a, r) = (asked.clone(), raised.clone());
    si::listen(lock, move || { r.fetch_add(1, Ordering::SeqCst); }, move |request| {
        a.lock().unwrap().push(request);
        json!({"ok": true, "text": "done"})
    }, key);

    for args in COMMANDS {
        let out = run(&profile, &path, args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "done");
    }
    assert_eq!(asked.lock().unwrap().len(), 2, "both reached the running copy");
    assert_eq!(raised.load(Ordering::SeqCst), 0);

    let out = run(&profile, &path, &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(raised.load(Ordering::SeqCst), 1, "a second launch raises the first and goes");
    assert!(!called.exists(), "what the running copy owns stays");
}

// test_main.py::test_new_instance_activates_pyav_after_getting_the_lock_and_before_importing_it: the real startup cleans up only once it owns the lock.
// The cleanup's pactl asks the program itself whether a copy is answering on the lock, then ends it there, before a window.
#[test]
fn a_new_copy_cleans_up_once_it_has_the_lock() {
    let _one = STARTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = telescope_core::testing::scratch_dir("commands-new");
    let (profile, answer) = (dir.join("profile"), dir.join("answer"));
    let (program, saying) = (env!("CARGO_BIN_EXE_telescope-rs"), answer.display());
    let path = path_with_pactl(&dir, &format!("'{program}' --list-actions > '{saying}.part' 2>&1\nmv '{saying}.part' '{saying}'\nkill -9 $PPID"));
    // A copy of the mic's source name elsewhere (another test run) makes it leave the source alone, so it gets a few tries
    for _ in 0..5 {
        run(&profile, &path, &[]);
        if answer.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let said = std::fs::read_to_string(&answer).expect("the start cleaned up");
    assert!(!said.contains("isn't running"), "it cleaned up before it had the lock: {said}");
}
