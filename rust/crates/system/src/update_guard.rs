//! Makes installing an update survive being cut short, and undoes one that won't start (update_guard.py). The
//! journal and folders are the Python app's, so a Python build can update to this one and back.
//!
//! The updater unpacks an update into STAGING_DIR, writes the journal (.update.json) and calls finish_swap(). Every
//! start calls recover() first:
//! - "swapping": the install was cut short; finish_swap() is safe to run again, so it picks up where it stopped (or,
//!   if that fails, roll_back() puts the old version back).
//! - "trial": the new version's first start. The old one is kept until confirm(), once the window is up. A start
//!   that finds the trial already started rolls back and remembers the build so it isn't offered again. A SIGTERM or
//!   SIGHUP first (a logout) confirms.
//! - "rolling_back": a roll-back was cut short; it's safe to run again too.

use serde_json::{json, Value};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const JOURNAL: &str = ".update.json";
/// The build that was rolled back: {"build": n}.
pub const FAILED: &str = ".update-failed";
pub const STAGING_DIR: &str = ".update-staging";
/// Linux: the replaced entries, until the new version is confirmed.
pub const PREVIOUS_DIR: &str = ".previous";
pub const EXE_NAME: &str = "TelescopeDesktop.exe";
pub const OLD_EXE_NAME: &str = "TelescopeDesktop.old.exe";
pub const FAILED_EXE_NAME: &str = "TelescopeDesktop.failed.exe";
/// + the build number: the Python exe's library folder (telescope.spec). This app has none.
pub const LIB_PREFIX: &str = "lib-";
/// A new version's first start may take this long to take the lock.
pub const TRIAL_GRACE: Duration = Duration::from_secs(30);

/// How files move: fs::rename (os.replace) normally, something that fails on cue in tests.
pub type Mover<'a> = &'a mut dyn FnMut(&Path, &Path) -> io::Result<()>;

fn rename(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::rename(src, dst)
}

// ── The journal ───────────────────────────────────────────────────────────────

pub fn read_journal(directory: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(directory.join(JOURNAL)).ok()?;
    serde_json::from_str::<Value>(&text).ok().filter(Value::is_object)
}

/// Replaces the journal in one step, so a cut-short write leaves the old one.
pub fn write_journal(directory: &Path, data: &Value) -> io::Result<()> {
    use std::io::Write;
    let tmp = directory.join(format!("{JOURNAL}.tmp"));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(data.to_string().as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, directory.join(JOURNAL))
}

pub fn clear_journal(directory: &Path) -> io::Result<()> {
    match std::fs::remove_file(directory.join(JOURNAL)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub fn failed_build(directory: &Path) -> Option<i64> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(directory.join(FAILED)).ok()?).ok()?;
    match &v["build"] {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// A file, link or whole folder, gone; fine if it wasn't there.
pub fn remove(path: &Path) -> io::Result<()> {
    let result = match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn with(journal: &Value, changes: Value) -> Value {
    let mut out = journal.clone();
    if let (Some(o), Some(c)) = (out.as_object_mut(), changes.as_object()) {
        for (k, v) in c {
            o.insert(k.clone(), v.clone());
        }
    }
    out
}

fn names(journal: &Value, key: &str) -> Vec<String> {
    journal[key].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn sorted_entries(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut list: Vec<PathBuf> = std::fs::read_dir(dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    list.sort();
    Ok(list)
}

fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

// ── Swapping the new version in (safe to run again after being cut short) ─────

/// Moves what's left in the staging folder into place. True if the exe was swapped by this call (Windows: the running
/// copy is then the old one under another name). Files in use that keep their old version go in skipped.
pub fn finish_swap(directory: &Path, journal: &Value, skipped: &mut Vec<String>, mv: Mover) -> io::Result<bool> {
    let staging = directory.join(STAGING_DIR);
    let swapped = if journal["platform"] == "windows" {
        finish_windows(directory, &staging, skipped, mv)?
    } else {
        finish_linux(directory, &staging, mv)?
    };
    let _ = std::fs::remove_dir_all(&staging);
    Ok(swapped)
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(list) = sorted_entries(dir) {
        for p in list {
            if is_real_dir(&p) {
                files_under(&p, out);
            } else if p.is_file() {
                out.push(p);
            }
        }
    }
}

fn finish_windows(directory: &Path, staging: &Path, skipped: &mut Vec<String>, mv: Mover) -> io::Result<bool> {
    if !staging.is_dir() {
        return Ok(false);
    }
    // The libraries first, each folder in one rename, so the new exe never sits next to a half-copied one
    for lib in sorted_entries(staging)? {
        let name = lib.file_name().unwrap_or_default().to_string_lossy().to_string();
        if lib.is_dir() && name.starts_with(LIB_PREFIX) && !directory.join(&name).exists() {
            mv(&lib, &directory.join(&name))?;
        }
    }
    let mut swapped = false;
    let (new_exe, current, old) = (staging.join(EXE_NAME), directory.join(EXE_NAME), directory.join(OLD_EXE_NAME));
    if new_exe.exists() {
        if current.exists() {
            remove(&old)?;
            mv(&current, &old)?; // allowed while it runs; overwriting isn't
        }
        mv(&new_exe, &current)?;
        swapped = true;
    }
    let mut files = Vec::new();
    files_under(staging, &mut files);
    for src in files {
        let rel = src.strip_prefix(staging).unwrap_or(&src).to_path_buf();
        let dst = directory.join(&rel);
        if dst.exists() && same(&dst, &src) {
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if mv(&src, &dst).is_err() {
            // e.g. a UnityCapture DLL loaded by OBS right now; it keeps working, just unchanged
            skipped.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(swapped)
}

fn finish_linux(directory: &Path, staging: &Path, mv: Mover) -> io::Result<bool> {
    if !staging.is_dir() {
        return Ok(false);
    }
    let previous = directory.join(PREVIOUS_DIR);
    if !previous.exists() {
        std::fs::create_dir(&previous)?;
    }
    for src in sorted_entries(staging)? {
        let name = src.file_name().unwrap_or_default().to_os_string();
        let (dst, prev) = (directory.join(&name), previous.join(&name));
        if is_real_dir(&src) {
            // A folder can't be replaced in one step: the old one moves aside, then the new one in
            if dst.exists() && !prev.exists() {
                mv(&dst, &prev)?;
            } else if dst.exists() {
                remove(&dst)?; // left from a cut-short run; the old version is safe in .previous
            }
            mv(&src, &dst)?;
        } else {
            // A file is replaced in one step, so start.sh is never missing
            #[cfg(unix)]
            if name == "start.sh" {
                use std::os::unix::fs::PermissionsExt;
                // Before it's in place: a start.sh that won't run couldn't finish this
                std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755))?;
            }
            if dst.exists() && !prev.exists() {
                std::fs::copy(&dst, &prev)?;
                if let Ok(m) = std::fs::metadata(&dst) {
                    let _ = std::fs::set_permissions(&prev, m.permissions());
                }
            }
            mv(&src, &dst)?;
        }
    }
    Ok(false)
}

fn same(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) if ma.len() == mb.len() => std::fs::read(a).ok() == std::fs::read(b).ok(),
        _ => false,
    }
}

// ── Putting the old version back (safe to run again after being cut short) ───

/// remember: the new build itself is to blame (it didn't start), so the updater stops offering it.
pub fn roll_back(directory: &Path, journal: &Value, remember: bool, mv: Mover) -> io::Result<()> {
    write_journal(directory, &with(journal, json!({"state": "rolling_back"})))?;
    if journal["platform"] == "windows" {
        let (current, old) = (directory.join(EXE_NAME), directory.join(OLD_EXE_NAME));
        if old.exists() {
            let failed = directory.join(FAILED_EXE_NAME);
            if current.exists() {
                remove(&failed)?;
                mv(&current, &failed)?; // may be the running copy: renaming is allowed
            }
            if let Err(e) = mv(&old, &current) {
                if failed.exists() && !current.exists() {
                    // Never leave no exe at all: nothing could start to retry this
                    mv(&failed, &current)?;
                }
                return Err(e);
            }
        }
        // The new lib folder may be the running one; the old version's clean-up deletes it
    } else {
        let previous = directory.join(PREVIOUS_DIR);
        let added = names(journal, "added");
        for name in names(journal, "entries") {
            let (dst, prev) = (directory.join(&name), previous.join(&name));
            if prev.exists() {
                if dst.exists() {
                    remove(&dst)?;
                }
                mv(&prev, &dst)?;
            } else if added.contains(&name) && dst.exists() {
                remove(&dst)?;
            }
        }
    }
    let _ = std::fs::remove_dir_all(directory.join(STAGING_DIR));
    if remember {
        if let Some(build) = journal["to_build"].as_i64().filter(|b| *b != 0) {
            std::fs::write(directory.join(FAILED), json!({"build": build}).to_string())?;
        }
    }
    clear_journal(directory)
}

// ── At start ──────────────────────────────────────────────────────────────────

/// Whether another copy holds the real app's lock. legacy: also a Linux copy from before the per-user lock, which
/// holds the port; only right after an update, as another program could hold that port for good.
pub fn another_copy_running(legacy: bool) -> bool {
    use crate::single_instance::{Address, INSTANCE_PORT};
    copy_running_at(&Address::current(), (legacy && cfg!(unix)).then_some(INSTANCE_PORT))
}

/// Whether the lock at address is held, or (legacy_port) that port on 127.0.0.1.
fn copy_running_at(address: &crate::single_instance::Address, legacy_port: Option<u16>) -> bool {
    crate::single_instance::held_elsewhere(address) || legacy_port.is_some_and(|port| std::net::TcpListener::bind(("127.0.0.1", port)).is_err())
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// Deals with an update in progress. None: carry on starting. Some: the command that starts the right version of
/// Telescope; the caller runs it and exits. wait: how long to give the copy that installed the update to exit.
pub fn recover(directory: &Path, wait: Duration, running_elsewhere: &dyn Fn() -> bool, grace: Duration) -> Option<Vec<String>> {
    recover_with(directory, wait, running_elsewhere, grace, &mut rename)
}

pub fn recover_with(directory: &Path, wait: Duration, running_elsewhere: &dyn Fn() -> bool, grace: Duration,
                    mv: Mover) -> Option<Vec<String>> {
    let journal = read_journal(directory)?;
    let deadline = Instant::now() + wait;
    while running_elsewhere() {
        if Instant::now() >= deadline {
            return None; // the running copy owns the journal; this one just asks it to show itself
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let state = journal["state"].as_str().unwrap_or("");
    let started = journal["started"].as_bool() == Some(true);
    let result = (|| -> io::Result<Option<Vec<String>>> {
        if state == "swapping" {
            let swapped = match finish_swap(directory, &journal, &mut Vec::new(), mv) {
                Ok(s) => s,
                Err(_) => {
                    roll_back(directory, &journal, false, mv)?;
                    return Ok(Some(relaunch_command(directory, false)));
                }
            };
            write_journal(directory, &with(&journal, json!({"state": "trial", "started": false})))?;
            // Windows: a copy that just renamed itself out of the way is the old version, so start the new one
            return Ok(swapped.then(|| relaunch_command(directory, true)));
        }
        if state == "trial" && started && first_start_still_loading(&journal, running_elsewhere, grace) {
            return Ok(None); // a second launch while the new version was still starting up: that one takes the lock
        }
        if state == "rolling_back" || (state == "trial" && started) {
            roll_back(directory, &journal, true, mv)?;
            return Ok(Some(relaunch_command(directory, false)));
        }
        if state == "trial" {
            write_journal(directory, &with(&journal, json!({"started": true, "started_at": now()})))?;
        }
        Ok(None)
    })();
    // Can't fix it from here: start what's there, and try again next time
    result.unwrap_or(None)
}

/// A started trial with no running copy usually means it crashed, unless it's still loading before it takes the lock.
fn first_start_still_loading(journal: &Value, running_elsewhere: &dyn Fn() -> bool, grace: Duration) -> bool {
    let Some(started_at) = journal["started_at"].as_f64() else { return false };
    while now() - started_at < grace.as_secs_f64() {
        if running_elsewhere() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    running_elsewhere()
}

/// The new version started fine: stop being ready to roll it back. Its leftovers go with the next clean-up.
pub fn confirm(directory: &Path) {
    if read_journal(directory).is_some_and(|j| j["state"] == "trial") {
        let _ = clear_journal(directory);
    }
}

/// A logout or shutdown (SIGTERM, SIGHUP) before confirm() is the system's doing, not a failed start: confirm, then die
/// the way the signal would have. Call first thing in main, before any other thread starts, so they all leave the two
/// signals to the thread that waits for them. Not on Windows, where a shutdown closes the window instead.
#[cfg(unix)]
pub fn confirm_on_terminate(directory: PathBuf) {
    // SAFETY: plain libc signal-set calls on a local set; the mask is set before other threads exist
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGHUP);
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return;
        }
        let _ = std::thread::Builder::new().name("terminate".into()).spawn(move || {
            let mut sig: libc::c_int = 0;
            if libc::sigwait(&set, &mut sig) == 0 {
                confirm(&directory);
                libc::signal(sig, libc::SIG_DFL);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
                libc::kill(libc::getpid(), sig);
            }
        });
    }
}

#[cfg(not(unix))]
pub fn confirm_on_terminate(_directory: PathBuf) {}

/// The command that starts the version in place: the exe on Windows, start.sh (else this program) on Linux.
pub fn relaunch_command(directory: &Path, after_update: bool) -> Vec<String> {
    let start = if cfg!(windows) {
        directory.join(EXE_NAME)
    } else if directory.join("start.sh").exists() {
        directory.join("start.sh")
    } else {
        std::env::current_exe().unwrap_or_else(|_| directory.join("telescope-rs"))
    };
    let mut argv = vec![start.to_string_lossy().to_string()];
    if after_update {
        argv.push("--after-update".into());
    }
    argv
}

/// Starts argv on its own, not as a child of this process (which is about to exit).
pub fn launch(argv: &[String]) -> io::Result<()> {
    let (program, args) = argv.split_first().ok_or(io::ErrorKind::InvalidInput)?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).stdin(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A session of its own (start_new_session), so a closing terminal or this process's group going takes nothing with it
        // SAFETY: setsid only touches the child's own session and is safe between fork and exec
        unsafe {
            cmd.pre_exec(|| if libc::setsid() < 0 { Err(io::Error::last_os_error()) } else { Ok(()) });
        }
    }
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0000_0008 | 0x0000_0200);
    cmd.spawn().map(|_| ())
}

/// A lib-<build> folder of the Python app, which holds the Qt it can't run without. A folder only counts when its name and its contents
/// both match (updates._ours), since a portable copy unpacked into Downloads shares it with the user's own things.
fn is_python_build_lib(p: &Path) -> bool {
    let digits = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix(LIB_PREFIX)).unwrap_or("");
    is_real_dir(p) && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) && p.join("PyQt6").is_dir()
}

/// Deletes what earlier versions left behind (updates.clean_up_after_update): the old and failed exes, the staging
/// folder, .previous and the Python build's lib-<build> folders (this app has none of its own). Best effort: what's
/// still held is tried again next start. this_build: a copy rolled back while it was still starting leaves them.
pub fn clean_up_after_update(directory: &Path, this_build: Option<i64>) {
    if read_journal(directory).is_some() {
        return; // an update isn't confirmed yet: these are what it would roll back to
    }
    if this_build.is_some_and(|b| b != 0 && failed_build(directory) == Some(b)) {
        return; // this copy was rolled back underneath it: the version in place needs these
    }
    let mut leftovers: Vec<PathBuf> = [OLD_EXE_NAME, FAILED_EXE_NAME, STAGING_DIR, PREVIOUS_DIR].iter().map(|n| directory.join(n)).collect();
    if let Ok(list) = std::fs::read_dir(directory) {
        leftovers.extend(list.filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| is_python_build_lib(p)));
    }
    for p in leftovers {
        let _ = remove(&p);
    }
}

/// main.py's start: recover first and hand over to the right version when needed. True: carry on starting.
pub fn guard_start(directory: &Path, after_update: bool) -> bool {
    let wait = Duration::from_secs(if after_update { 15 } else { 0 });
    match recover(directory, wait, &|| another_copy_running(after_update), TRIAL_GRACE) {
        None => true,
        Some(argv) => {
            // Nothing more this copy can do if it fails; starting Telescope again runs whatever version is in place
            let _ = launch(&argv);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn nobody() -> bool {
        false
    }

    fn root(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("telescope-guard-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write(path: &Path, data: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// A Windows install at build 130 with build 131 unpacked into staging (what install_windows extracts).
    fn windows_app(dir: &Path) -> PathBuf {
        let app = dir.join("app");
        write(&app.join("lib-130/python311.dll"), "py 130");
        write(&app.join("lib-130/PyQt6/qt.dll"), "qt 130");
        write(&app.join(EXE_NAME), "exe 130");
        write(&app.join("Telescope.apk"), "apk 130");
        app
    }

    fn stage_windows(app: &Path) {
        let s = app.join(STAGING_DIR);
        write(&s.join(EXE_NAME), "exe 131");
        write(&s.join("lib-131/python311.dll"), "py 131");
        write(&s.join("lib-131/PyQt6/qt.dll"), "qt 131");
        write(&s.join("Telescope.apk"), "apk 131");
    }

    fn linux_app(dir: &Path) -> PathBuf {
        let app = dir.join("app");
        write(&app.join("main.py"), "main 130");
        write(&app.join("start.sh"), "start 130");
        write(&app.join("telescope/app.py"), "app 130");
        write(&app.join("old_only.txt"), "gone in 131");
        app
    }

    fn stage_linux(app: &Path) -> Value {
        let s = app.join(STAGING_DIR);
        write(&s.join("main.py"), "main 131");
        write(&s.join("start.sh"), "start 131");
        write(&s.join("telescope/app.py"), "app 131");
        write(&s.join("new_in_131.txt"), "new");
        let entries = vec!["main.py", "new_in_131.txt", "start.sh", "telescope"];
        let added: Vec<&str> = entries.iter().copied().filter(|n| !app.join(n).exists()).collect();
        json!({"platform": "linux", "to_build": 131, "entries": entries, "added": added})
    }

    /// updates._swap: journal, swap, and on failure the old version back; the mover can fail on cue.
    fn install(app: &Path, journal: &Value, mv: Mover) -> io::Result<()> {
        write_journal(app, &with(journal, json!({"state": "swapping", "from_build": 130})))?;
        if let Err(e) = finish_swap(app, journal, &mut Vec::new(), mv) {
            let _ = roll_back(app, journal, false, &mut rename);
            return Err(e);
        }
        write_journal(app, &with(journal, json!({"state": "trial", "started": false})))
    }

    /// Dies (panics) at the k-th move, like a process killed mid-install.
    fn install_killed_at(app: &Path, journal: &Value, k: usize) {
        let mut seen = 0;
        let mut mv = |s: &Path, d: &Path| {
            seen += 1;
            if seen == k {
                panic!("killed");
            }
            rename(s, d)
        };
        let _ = catch_unwind(AssertUnwindSafe(|| install(app, journal, &mut mv)));
    }

    fn count_moves(app: &Path, journal: &Value) -> usize {
        let mut n = 0;
        let mut mv = |s: &Path, d: &Path| {
            n += 1;
            rename(s, d)
        };
        install(app, journal, &mut mv).unwrap();
        n
    }

    fn assert_windows_new(app: &Path) {
        assert_eq!(read(&app.join(EXE_NAME)), "exe 131");
        assert_eq!(read(&app.join("lib-131/python311.dll")), "py 131");
        assert_eq!(read(&app.join("lib-131/PyQt6/qt.dll")), "qt 131");
        assert_eq!(read(&app.join("Telescope.apk")), "apk 131");
        assert!(!app.join(STAGING_DIR).exists());
    }

    #[test]
    fn a_windows_install_cut_short_anywhere_is_finished_by_the_next_start() {
        let base = root("win-cut");
        let counting = windows_app(&base.join("count"));
        stage_windows(&counting);
        let journal = json!({"platform": "windows", "to_build": 131});
        let total = count_moves(&counting, &journal);
        assert!(total >= 4);
        for k in 1..=total {
            let app = windows_app(&base.join(format!("k{k}")));
            stage_windows(&app);
            install_killed_at(&app, &journal, k);
            let exe_was_new = read(&app.join(EXE_NAME)) == "exe 131";
            let relaunch = recover(&app, Duration::ZERO, &nobody, Duration::ZERO);
            assert_windows_new(&app);
            // A start by the old exe moves it aside for the new one, and hands over to that
            assert_eq!(relaunch.is_none(), exe_was_new, "k={k}");
            assert!(relaunch.as_ref().is_none_or(|r| r.last().unwrap() == "--after-update"));
            assert_eq!(read_journal(&app).unwrap()["state"], "trial");
            assert_eq!(read(&app.join("lib-130/python311.dll")), "py 130");
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_windows_update_that_never_confirms_rolls_back_and_isnt_offered_again() {
        let base = root("win-roll");
        let app = windows_app(&base);
        stage_windows(&app);
        install(&app, &json!({"platform": "windows", "to_build": 131}), &mut rename).unwrap();
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None); // the new version's first start
        assert_eq!(read_journal(&app).unwrap()["started"], true);
        let relaunch = recover(&app, Duration::ZERO, &nobody, Duration::ZERO).expect("rolls back");
        assert!(!relaunch.contains(&"--after-update".to_string()));
        assert_eq!(read(&app.join(EXE_NAME)), "exe 130");
        assert_eq!(read(&app.join(FAILED_EXE_NAME)), "exe 131");
        assert!(read_journal(&app).is_none());
        assert_eq!(failed_build(&app), Some(131));
        // The old version's clean-up takes the failed one's leftovers, lib folders and all
        clean_up_after_update(&app, Some(130));
        assert!(!app.join(FAILED_EXE_NAME).exists() && !app.join("lib-131").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn clean_up_leaves_other_lib_folders_alone() {
        let base = root("lib-others");
        let app = base.join("app");
        write(&app.join(EXE_NAME), "exe");
        write(&app.join("lib-project/main.rs"), "a user's own folder");
        write(&app.join("lib-130/python311.dll"), "no Qt in here");
        write(&app.join("lib-x1/PyQt6/qt.dll"), "not a build number");
        write(&app.join("lib-129/PyQt6/qt.dll"), "an old build");
        clean_up_after_update(&app, None);
        assert_eq!(read(&app.join("lib-project/main.rs")), "a user's own folder");
        assert!(app.join("lib-130").exists() && app.join("lib-x1").exists());
        assert!(!app.join("lib-129").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_confirmed_update_stays_and_its_leftovers_go() {
        let base = root("win-confirm");
        let app = windows_app(&base);
        stage_windows(&app);
        install(&app, &json!({"platform": "windows", "to_build": 131}), &mut rename).unwrap();
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None);
        confirm(&app);
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None);
        clean_up_after_update(&app, Some(131));
        assert_eq!(read(&app.join(EXE_NAME)), "exe 131");
        assert!(!app.join(OLD_EXE_NAME).exists() && !app.join("lib-130").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_second_copy_starting_leaves_the_update_to_the_running_one() {
        let base = root("second");
        let app = windows_app(&base);
        stage_windows(&app);
        install(&app, &json!({"platform": "windows", "to_build": 131}), &mut rename).unwrap();
        let before = read_journal(&app);
        assert_eq!(recover(&app, Duration::ZERO, &|| true, Duration::ZERO), None);
        assert_eq!(read_journal(&app), before);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_second_launch_while_the_new_version_is_still_loading_leaves_it_alone() {
        let base = root("loading");
        let app = windows_app(&base);
        write_journal(&app, &json!({"platform": "windows", "to_build": 131, "state": "trial", "started": true, "started_at": now()})).unwrap();
        // Nobody holds the lock when recover starts, then the first start takes it within the grace time
        let calls = std::cell::Cell::new(0);
        let running = || {
            calls.set(calls.get() + 1);
            calls.get() > 1
        };
        assert_eq!(recover(&app, Duration::ZERO, &running, Duration::from_secs(5)), None);
        assert_eq!(read_journal(&app).unwrap()["state"], "trial");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_first_start_rolled_back_underneath_it_leaves_the_old_version_whole() {
        let base = root("underneath");
        let app = windows_app(&base);
        write(&app.join("lib-131/python311.dll"), "py 131");
        std::fs::write(app.join(FAILED), "{\"build\": 131}").unwrap();
        clean_up_after_update(&app, Some(131));
        assert!(app.join("lib-130").exists() && app.join("lib-131").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_roll_back_cut_short_is_finished_by_the_next_start() {
        let base = root("roll-cut");
        let journal = json!({"platform": "windows", "to_build": 131});
        let counting = windows_app(&base.join("count"));
        stage_windows(&counting);
        install(&counting, &journal, &mut rename).unwrap();
        let mut total = 0;
        roll_back(&counting, &read_journal(&counting).unwrap(), true, &mut |s: &Path, d: &Path| { total += 1; rename(s, d) }).unwrap();
        for k in 1..=total {
            let app = windows_app(&base.join(format!("k{k}")));
            stage_windows(&app);
            install(&app, &journal, &mut rename).unwrap();
            let trial = read_journal(&app).unwrap();
            let mut seen = 0;
            let _ = catch_unwind(AssertUnwindSafe(|| roll_back(&app, &trial, true, &mut |s: &Path, d: &Path| {
                seen += 1;
                if seen == k { panic!("killed") }
                rename(s, d)
            })));
            assert_eq!(read_journal(&app).unwrap()["state"], "rolling_back");
            assert!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO).is_some());
            assert_eq!(read(&app.join(EXE_NAME)), "exe 130", "k={k}");
            assert!(read_journal(&app).is_none());
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn clean_up_waits_while_an_update_is_unconfirmed() {
        let base = root("wait");
        let app = windows_app(&base);
        stage_windows(&app);
        install(&app, &json!({"platform": "windows", "to_build": 131}), &mut rename).unwrap();
        clean_up_after_update(&app, Some(131));
        assert_eq!(read(&app.join(OLD_EXE_NAME)), "exe 130");
        assert!(app.join("lib-130").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    fn assert_linux_new(app: &Path) {
        assert_eq!(read(&app.join("main.py")), "main 131");
        assert_eq!(read(&app.join("start.sh")), "start 131");
        assert_eq!(read(&app.join("telescope/app.py")), "app 131");
        assert_eq!(read(&app.join("new_in_131.txt")), "new");
        assert!(!app.join(STAGING_DIR).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(app.join("start.sh")).unwrap().permissions().mode() & 0o111, 0o111);
        }
    }

    fn assert_linux_old(app: &Path) {
        assert_eq!(read(&app.join("main.py")), "main 130");
        assert_eq!(read(&app.join("start.sh")), "start 130");
        assert_eq!(read(&app.join("telescope/app.py")), "app 130");
        assert!(!app.join("new_in_131.txt").exists());
        assert_eq!(read(&app.join("old_only.txt")), "gone in 131");
    }

    #[test]
    fn a_linux_install_cut_short_anywhere_is_finished_by_the_next_start() {
        let base = root("lin-cut");
        let counting = linux_app(&base.join("count"));
        let journal = stage_linux(&counting);
        let total = count_moves(&counting, &journal);
        for k in 1..=total {
            let app = linux_app(&base.join(format!("k{k}")));
            let journal = stage_linux(&app);
            install_killed_at(&app, &journal, k);
            assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None, "k={k}");
            assert_linux_new(&app);
            assert_eq!(read_journal(&app).unwrap()["state"], "trial");
            assert_eq!(read(&app.join(PREVIOUS_DIR).join("main.py")), "main 130");
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_linux_update_that_never_confirms_rolls_back() {
        let base = root("lin-roll");
        let app = linux_app(&base);
        let journal = stage_linux(&app);
        install(&app, &journal, &mut rename).unwrap();
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None);
        assert!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO).is_some());
        assert_linux_old(&app);
        assert_eq!(failed_build(&app), Some(131));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_linux_install_that_fails_outright_is_undone_but_not_blamed_on_the_build() {
        let base = root("lin-fail");
        let app = linux_app(&base);
        let journal = stage_linux(&app);
        let mut mv = |s: &Path, d: &Path| {
            if s.file_name().is_some_and(|n| n == "telescope") && s.starts_with(app.join(STAGING_DIR)) {
                return Err(io::Error::other("in use"));
            }
            rename(s, d)
        };
        assert!(install(&app, &journal, &mut mv).is_err());
        assert_linux_old(&app);
        assert!(read_journal(&app).is_none());
        assert_eq!(failed_build(&app), None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn after_an_update_the_new_copy_waits_for_the_old_one_to_exit() {
        let base = root("waits");
        let app = windows_app(&base);
        stage_windows(&app);
        install(&app, &json!({"platform": "windows", "to_build": 131}), &mut rename).unwrap();
        let until = Instant::now() + Duration::from_millis(600);
        let old_running = move || Instant::now() < until;
        assert_eq!(recover(&app, Duration::from_secs(5), &old_running, Duration::ZERO), None);
        assert_eq!(read_journal(&app).unwrap()["started"], true);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn confirming_with_no_update_in_progress_leaves_no_journal_behind() {
        let base = root("confirm-none");
        confirm(&base);
        assert!(!base.join(JOURNAL).exists());
        write_journal(&base, &json!({"platform": "linux", "state": "trial", "started": true})).unwrap();
        confirm(&base);
        assert!(read_journal(&base).is_none());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_guard_sees_a_running_copy_through_the_same_address_the_app_binds() {
        use crate::single_instance::{acquire, Acquired, Address};
        // A dev profile's address is its own, so this doesn't meet a real Telescope running here
        let folder = root("guard-address");
        std::env::set_var(telescope_core::config::DEV_PROFILE_ENV, &folder);
        let Acquired::Lock(lock) = acquire(&Address::current(), Duration::ZERO) else { panic!("no lock") };
        assert!(another_copy_running(false));
        drop(lock);
        assert!(!another_copy_running(false));
        std::env::remove_var(telescope_core::config::DEV_PROFILE_ENV);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    // test_update_guard.py::test_right_after_an_update_a_copy_from_before_the_per_user_lock_counts
    #[cfg(unix)]
    #[test]
    fn right_after_an_update_a_copy_from_before_the_per_user_lock_counts() {
        use crate::single_instance::Address;
        let address = Address::Abstract(format!("telescope-guard-legacy-{}", std::process::id()));
        let holder = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = holder.local_addr().unwrap().port();
        assert!(!copy_running_at(&address, None), "only right after an update does the old port count");
        assert!(copy_running_at(&address, Some(port)));
        drop(holder);
        assert!(!copy_running_at(&address, Some(port)));
    }

    // subprocess.Popen(start_new_session=True): the new copy has to outlive this one and its terminal
    #[cfg(target_os = "linux")]
    #[test]
    fn launching_starts_a_session_of_its_own() {
        let base = root("session");
        let out = base.join("stat");
        let script = format!("cat /proc/self/stat > '{0}.tmp' && mv '{0}.tmp' '{0}'", out.display());
        launch(&["/bin/sh".to_string(), "-c".to_string(), script]).unwrap();
        let waited = Instant::now();
        while !out.exists() {
            assert!(waited.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        let stat = read(&out);
        let fields: Vec<&str> = stat.rsplit(')').next().unwrap().split_whitespace().collect();
        // After the name: state, parent, group, session; cat runs in the shell's session, which is the shell's own number
        assert_eq!(fields[2], fields[3], "its group leads the session");
        let mine = std::fs::read_to_string("/proc/self/stat").unwrap();
        let my_session = mine.rsplit(')').next().unwrap().split_whitespace().nth(3).unwrap().to_string();
        assert_ne!(fields[3], my_session);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_relaunch_runs_start_sh_where_there_is_one() {
        let base = root("relaunch");
        if cfg!(windows) {
            assert_eq!(relaunch_command(&base, true), vec![base.join(EXE_NAME).to_string_lossy().to_string(), "--after-update".into()]);
        } else {
            write(&base.join("start.sh"), "#!/bin/sh");
            assert_eq!(relaunch_command(&base, false), vec![base.join("start.sh").to_string_lossy().to_string()]);
        }
        std::fs::remove_dir_all(&base).unwrap();
    }
}
