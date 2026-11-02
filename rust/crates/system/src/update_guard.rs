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
/// The replaced entries (on Windows, files), until the new version is confirmed.
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
    write_journal_with(directory, data, &mut rename)
}

/// write_journal, its last step going through mv like every other move of an install or a roll-back.
pub fn write_journal_with(directory: &Path, data: &Value, mv: Mover) -> io::Result<()> {
    use std::io::Write;
    let tmp = directory.join(format!("{JOURNAL}.tmp"));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(data.to_string().as_bytes())?;
    f.sync_all()?;
    mv(&tmp, &directory.join(JOURNAL))
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
    let previous = directory.join(PREVIOUS_DIR);
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
        if !replace_keeping_old(&src, &dst, &previous.join(&rel), mv) {
            // e.g. a UnityCapture DLL held open by OBS right now; it keeps working, just unchanged
            skipped.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(swapped)
}

/// Moves dst to prev, then src to dst. A roll-back needs the old file, and a DLL that's loaded right now can be moved but not overwritten.
fn replace_keeping_old(src: &Path, dst: &Path, prev: &Path, mv: Mover) -> bool {
    let moved = dst.exists() && !prev.exists();
    if moved && (prev.parent().is_some_and(|p| std::fs::create_dir_all(p).is_err()) || mv(dst, prev).is_err()) {
        return false;
    }
    if mv(src, dst).is_ok() {
        return true;
    }
    if moved {
        let _ = mv(prev, dst);
    }
    false
}

/// Puts back what finish_windows moved aside, so the old exe doesn't run on the new version's ui-kit and libraries.
fn restore_previous(directory: &Path, mv: Mover) -> io::Result<()> {
    let previous = directory.join(PREVIOUS_DIR);
    let mut files = Vec::new();
    files_under(&previous, &mut files);
    for src in files {
        let dst = directory.join(src.strip_prefix(&previous).unwrap_or(&src));
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        mv(&src, &dst)?;
    }
    remove(&previous)
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
    write_journal_with(directory, &with(journal, json!({"state": "rolling_back"})), mv)?;
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
        restore_previous(directory, mv)?;
        // The new lib folder may be the running one; the old version's clean-up deletes it
    } else {
        let previous = directory.join(PREVIOUS_DIR);
        let added = names(journal, "added");
        for name in names(journal, "entries") {
            let (dst, prev) = (directory.join(&name), previous.join(&name));
            if prev.exists() {
                // A file goes back over the new one in one step, so an AppImage's path never stands empty
                if dst.exists() && (is_real_dir(&dst) || is_real_dir(&prev)) {
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
    let journal = read_journal(directory).filter(|j| is_ours(j, appimage_name().as_deref()))?;
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
            write_journal_with(directory, &with(&journal, json!({"state": "trial", "started": false})), mv)?;
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
            write_journal_with(directory, &with(&journal, json!({"started": true, "started_at": now()})), mv)?;
        }
        Ok(None)
    })();
    // Can't fix it from here: start what's there, and try again next time
    result.unwrap_or(None)
}

/// This copy's AppImage file name, which tells its update apart in a folder other things share.
fn appimage_name() -> Option<String> {
    telescope_core::system::appimage().map(|image| image.name())
}

/// Whether a journal is this copy's. An AppImage's lies in whatever folder holds the file, which can also hold another AppImage or an unpacked tarball.
fn is_ours(journal: &Value, appimage: Option<&str>) -> bool {
    journal["appimage"].as_str() == appimage
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
    if read_journal(directory).is_some_and(|j| j["state"] == "trial" && is_ours(&j, appimage_name().as_deref())) {
        let _ = clear_journal(directory);
    }
}

/// The write end of the pipe the terminate handler wakes its thread through.
#[cfg(unix)]
static TERMINATE_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// A signal handler may do next to nothing, so it hands the signal to the thread that confirms.
#[cfg(unix)]
extern "C" fn on_terminate(signal: libc::c_int) {
    let byte = signal as u8;
    let fd = TERMINATE_PIPE.load(std::sync::atomic::Ordering::SeqCst);
    // SAFETY: write is async-signal-safe and this is one byte from a local
    unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
}

/// A logout or shutdown (SIGTERM, SIGHUP) before confirm() is the system's doing, not a failed start: confirm, then die the way the signal would have.
/// A handler rather than a blocked mask, which every program this one starts would inherit. Not on Windows, where a shutdown closes the window instead.
#[cfg(unix)]
pub fn confirm_on_terminate(directory: PathBuf) {
    let mut ends = [0; 2];
    // SAFETY: room for the two descriptors
    if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return;
    }
    TERMINATE_PIPE.store(ends[1], std::sync::atomic::Ordering::SeqCst);
    let spawned = std::thread::Builder::new().name("terminate".into()).spawn(move || {
        let mut byte = 0u8;
        // SAFETY: reads one byte into a local from a pipe this thread's handler end keeps open
        if unsafe { libc::read(ends[0], (&mut byte as *mut u8).cast(), 1) } == 1 {
            confirm(&directory);
            // SAFETY: back to the default action, then that action
            unsafe {
                libc::signal(byte as libc::c_int, libc::SIG_DFL);
                libc::kill(libc::getpid(), byte as libc::c_int);
            }
        }
    });
    if spawned.is_err() {
        return;
    }
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler has the signature signal expects and stays for the life of the process
        unsafe { libc::signal(signal, on_terminate as *const () as libc::sighandler_t) };
    }
}

#[cfg(not(unix))]
pub fn confirm_on_terminate(_directory: PathBuf) {}

/// The command that starts the version in place: the exe on Windows, the AppImage, or start.sh (else this program) on Linux.
pub fn relaunch_command(directory: &Path, after_update: bool) -> Vec<String> {
    let start = if cfg!(windows) {
        directory.join(EXE_NAME)
    } else if directory.join("start.sh").exists() {
        directory.join("start.sh")
    } else {
        std::env::current_exe().unwrap_or_else(|_| directory.join("telescope-rs"))
    };
    let mut argv = telescope_core::system::appimage().map_or_else(|| vec![start.to_string_lossy().to_string()], |image| image.command());
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
        // SAFETY: setsid (start_new_session) only touches the child's own session and is safe between fork and exec
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
    // An AppImage's folder is the user's own, so only what its update made there goes
    if telescope_core::system::appimage().is_some() {
        let _ = remove(&directory.join(STAGING_DIR));
        let _ = remove(&directory.join(PREVIOUS_DIR));
        return;
    }
    let mut leftovers: Vec<PathBuf> = [OLD_EXE_NAME, FAILED_EXE_NAME, STAGING_DIR, PREVIOUS_DIR].iter().map(|n| directory.join(n)).collect();
    // What the Python tarball had and this one doesn't, which an update leaves in place. Only beside its update_guard.py, since the
    // others are common names in a folder a tarball was unpacked into
    if !cfg!(windows) && directory.join("update_guard.py").is_file() {
        leftovers.extend(["requirements.txt", "requirements-nodeps.txt", "constraints.txt", "__pycache__", "update_guard.py"].map(|n| directory.join(n)));
    }
    if let Ok(list) = std::fs::read_dir(directory) {
        leftovers.extend(list.filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| is_python_build_lib(p)));
    }
    for p in leftovers {
        let _ = if is_python_build_lib(&p) { remove_python_lib(&p) } else { remove(&p) };
    }
}

/// PyQt6 goes last: it's what says a folder is the Python app's, so a file still held leaves one the next start still recognises.
fn remove_python_lib(lib: &Path) -> io::Result<()> {
    let mut failed = None;
    for entry in sorted_entries(lib)?.iter().filter(|e| !e.ends_with("PyQt6")) {
        if let Err(e) = remove(entry) {
            failed = Some(e);
        }
    }
    failed.map_or_else(|| remove(lib), Err)
}

/// The venv the Python tarball's start.sh made (about 650 MB), once nothing can go back to it. Asked before clean_up_after_update takes
/// update_guard.py away. Only a folder with start.sh's stamp counts; data_home is $XDG_DATA_HOME or ~/.local/share.
pub fn python_venv_left(directory: &Path, this_build: Option<i64>, data_home: &Path) -> Option<PathBuf> {
    let unconfirmed = read_journal(directory).is_some() || this_build.is_some_and(|b| b != 0 && failed_build(directory) == Some(b));
    let venv = data_home.join("Telescope").join("venv");
    (!cfg!(windows) && telescope_core::system::appimage().is_none() && !unconfirmed && directory.join("update_guard.py").is_file() && venv.join(".telescope-requirements").is_file()).then_some(venv)
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
mod appimage_tests {
    use super::*;
    use crate::updates::install_appimage;

    fn image(body: &str) -> Vec<u8> {
        let mut bytes = b"\x7fELF\x02\x01\x01\x00AI\x02".to_vec();
        bytes.extend_from_slice(body.as_bytes());
        bytes
    }

    fn folder(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("telescope-appimage-guard-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// old.AppImage in a folder, a download beside it, and the folder it's in.
    fn placed(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = folder(name);
        let (current, new) = (dir.join("Telescope.AppImage"), dir.join("download"));
        std::fs::write(&current, image("old")).unwrap();
        std::fs::write(&new, image("new")).unwrap();
        (dir, current, new)
    }

    #[test]
    fn a_new_appimage_replaces_the_file_keeps_the_old_one_and_can_go_back() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, current, new) = placed("swap");
        std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o750)).unwrap();
        let tried = std::cell::RefCell::new(Vec::new());
        let result = install_appimage(&new, &current, false, 131, &mut rename, &|argv| {
            tried.borrow_mut().push(argv.to_vec());
            true
        }).unwrap();
        assert_eq!(tried.borrow()[0], [dir.join(STAGING_DIR).join("Telescope.AppImage").to_string_lossy().into_owned()]);
        assert_eq!(result.relaunch, [current.to_string_lossy().into_owned(), "--after-update".into()]);
        assert_eq!(std::fs::read(&current).unwrap(), image("new"));
        assert_eq!(std::fs::metadata(&current).unwrap().permissions().mode() & 0o777, 0o750);
        assert_eq!(std::fs::read(dir.join(PREVIOUS_DIR).join("Telescope.AppImage")).unwrap(), image("old"));
        assert!(!dir.join(STAGING_DIR).exists());
        let journal = read_journal(&dir).unwrap();
        assert_eq!((journal["state"].as_str(), journal["appimage"].as_str()), (Some("trial"), Some("Telescope.AppImage")));

        roll_back(&dir, &journal, true, &mut rename).unwrap();
        assert_eq!(std::fs::read(&current).unwrap(), image("old"));
        assert_eq!(std::fs::metadata(&current).unwrap().permissions().mode() & 0o777, 0o750);
        assert_eq!(failed_build(&dir), Some(131));
        assert!(read_journal(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_appimage_that_cant_start_here_is_refused_before_anything_changes() {
        let (dir, current, new) = placed("refused");
        let e = install_appimage(&new, &current, false, 131, &mut rename, &|_| false).unwrap_err();
        assert!(e.0.contains("can't start"), "{e}");
        assert_eq!(std::fs::read(&current).unwrap(), image("old"));
        assert!(!dir.join(STAGING_DIR).exists() && !dir.join(PREVIOUS_DIR).exists() && read_journal(&dir).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_download_that_isnt_an_appimage_is_refused() {
        let (dir, current, new) = placed("notimage");
        std::fs::write(&new, "<html>not one</html>").unwrap();
        assert!(install_appimage(&new, &current, false, 131, &mut rename, &|_| panic!("never run")).is_err());
        assert_eq!(std::fs::read(&current).unwrap(), image("old"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_swap_that_fails_leaves_the_old_file_in_place() {
        let (dir, current, new) = placed("fails");
        let mut mv = |a: &Path, b: &Path| if b == current { Err(io::Error::other("no")) } else { std::fs::rename(a, b) };
        assert!(install_appimage(&new, &current, false, 131, &mut mv, &|_| true).is_err());
        assert_eq!(std::fs::read(&current).unwrap(), image("old"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_copy_that_cant_mount_passes_its_flag_on() {
        let (dir, current, new) = placed("extracted");
        let result = install_appimage(&new, &current, true, 131, &mut rename, &|argv| argv[1] == "--appimage-extract-and-run").unwrap();
        assert_eq!(result.relaunch[1..], ["--appimage-extract-and-run", "--after-update"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_journal_belongs_to_the_copy_that_wrote_it() {
        let image = json!({"state": "trial", "appimage": "Telescope.AppImage"});
        assert!(is_ours(&image, Some("Telescope.AppImage")));
        assert!(!is_ours(&image, Some("Telescope-old.AppImage")), "another AppImage in the folder");
        assert!(!is_ours(&image, None), "a tarball unpacked in the same folder");
        assert!(is_ours(&json!({"state": "trial"}), None));
        assert!(!is_ours(&json!({"state": "trial"}), Some("Telescope.AppImage")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updates::archives::{tarball, zip_of};
    use crate::updates::{install_linux, install_windows, parse_manifest, NetError, Opener, Reply, Updates, UpdateError, WINDOWS_ASSET};
    use std::collections::BTreeMap;
    use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

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

    /// Dies at the k-th move, like the process being killed: nothing after it runs, not even the installer's own roll-back.
    fn killed_at(k: usize) -> impl FnMut(&Path, &Path) -> io::Result<()> {
        let mut seen = 0;
        move |s, d| {
            seen += 1;
            if seen == k {
                resume_unwind(Box::new("killed"));
            }
            rename(s, d)
        }
    }

    /// How many moves run does, journal writes included.
    fn count_moves(run: impl FnOnce(Mover)) -> usize {
        let mut n = 0;
        run(&mut |s: &Path, d: &Path| {
            n += 1;
            rename(s, d)
        });
        n
    }

    // ── Windows ───────────────────────────────────────────────────────────────

    /// A Windows install at build 130. PyQt6 is what makes clean-up count a lib folder as the Python build's.
    fn windows_app(dir: &Path) -> PathBuf {
        let app = dir.join("app");
        write(&app.join("lib-130/python311.dll"), "py 130");
        write(&app.join("lib-130/PyQt6/qt.dll"), "qt 130");
        write(&app.join(EXE_NAME), "exe 130");
        write(&app.join("Telescope.apk"), "apk 130");
        app
    }

    fn windows_zip(dir: &Path) -> PathBuf {
        zip_of(&dir.join(WINDOWS_ASSET), &[(EXE_NAME, b"exe 131"), ("lib-131/python311.dll", b"py 131"),
                                           ("lib-131/PyQt6/qt.dll", b"qt 131"), ("Telescope.apk", b"apk 131")])
    }

    /// A Windows app at 130 with 131 installed by the real installer.
    fn updated_windows_app(dir: &Path) -> PathBuf {
        let app = windows_app(dir);
        install_windows(&windows_zip(dir), &app, 131, &mut rename).unwrap();
        app
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
        let total = count_moves(|mv| {
            let dir = base.join("count");
            std::fs::create_dir_all(&dir).unwrap();
            install_windows(&windows_zip(&dir), &windows_app(&dir), 131, mv).unwrap();
        });
        assert!(total >= 6, "the journal twice, the lib, the exe twice, the apk: {total}");
        let mut before_the_journal = 0;
        for k in 1..=total {
            let dir = base.join(format!("k{k}"));
            std::fs::create_dir_all(&dir).unwrap();
            let app = windows_app(&dir);
            let archive = windows_zip(&dir);
            let _ = catch_unwind(AssertUnwindSafe(|| install_windows(&archive, &app, 131, &mut killed_at(k))));
            // Only the instant between the exe's two renames leaves nothing to start (the test runs the guard anyway)
            let exe_was_new = read(&app.join(EXE_NAME)) == "exe 131";
            if read_journal(&app).is_none() {
                // Cut short before the journal was written: the update never began, and the old version is intact
                assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None, "k={k}");
                assert_eq!(read(&app.join(EXE_NAME)), "exe 130", "k={k}");
                before_the_journal += 1;
                continue;
            }
            let relaunch = recover(&app, Duration::ZERO, &nobody, Duration::ZERO);
            assert_windows_new(&app);
            // A start by the old exe moves it aside for the new one, and hands over to that
            assert_eq!(relaunch.is_none(), exe_was_new, "k={k}");
            assert!(relaunch.as_ref().is_none_or(|r| r.last().unwrap() == "--after-update"));
            assert_eq!(read_journal(&app).unwrap()["state"], "trial");
            // The old version is still there to go back to
            assert_eq!(read(&app.join("lib-130/python311.dll")), "py 130");
        }
        assert_eq!(before_the_journal, 1, "only the first move comes before the journal");
        std::fs::remove_dir_all(&base).unwrap();
    }

    fn manifest(build: i64) -> Vec<u8> {
        format!(r#"{{"schema": 1, "version": "0.6.0", "build": {build}, "channel": "nightly", "versionName": "x",
                     "commit": "abc", "protocol": 2, "notes": "", "assets": []}}"#).into_bytes()
    }

    struct Offline;

    impl Opener for Offline {
        fn open(&self, _url: &str) -> Result<Reply, NetError> {
            Err(NetError::Other("offline".into()))
        }
    }

    #[test]
    fn a_windows_update_that_never_confirms_rolls_back_and_isnt_offered_again() {
        let base = root("win-roll");
        let app = updated_windows_app(&base);
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None); // the new version's first start
        assert_eq!(read_journal(&app).unwrap()["started"], true);
        // It crashed before confirming, so the next start goes back (once it's had its time to start up)
        let relaunch = recover(&app, Duration::ZERO, &nobody, Duration::ZERO).expect("rolls back");
        assert!(!relaunch.contains(&"--after-update".to_string()));
        assert_eq!(read(&app.join(EXE_NAME)), "exe 130");
        assert_eq!(read(&app.join(FAILED_EXE_NAME)), "exe 131");
        assert_eq!(read(&app.join("Telescope.apk")), "apk 130"); // and what else the new version replaced
        assert!(!app.join(PREVIOUS_DIR).exists());
        assert!(read_journal(&app).is_none());
        assert_eq!(failed_build(&app), Some(131));
        let at_130 = Updates { opener: &Offline, build: 130, directory: app.clone() };
        assert!(!at_130.is_newer(Some(&parse_manifest(&manifest(131)).unwrap())));
        assert!(at_130.is_newer(Some(&parse_manifest(&manifest(132)).unwrap())));
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

    #[cfg(not(windows))]
    #[test]
    fn the_python_tarballs_leftovers_go_but_not_lookalikes() {
        let base = root("py-leftovers");
        let app = base.join("app");
        write(&app.join("requirements.txt"), "someone's project");
        clean_up_after_update(&app, None);
        assert_eq!(read(&app.join("requirements.txt")), "someone's project");
        for name in ["update_guard.py", "requirements-nodeps.txt", "constraints.txt", "__pycache__/update_guard.pyc"] {
            write(&app.join(name), "python's");
        }
        clean_up_after_update(&app, None);
        assert_eq!(std::fs::read_dir(&app).unwrap().count(), 0);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_confirmed_update_stays_and_its_leftovers_go() {
        let base = root("win-confirm");
        let app = windows_app(&base);
        // Without PyQt6: this app has no lib folder of its own, so a Python build's next to it would count as a leftover
        let archive = zip_of(&base.join(WINDOWS_ASSET), &[(EXE_NAME, b"exe 131"), ("lib-131/python311.dll", b"py 131"), ("Telescope.apk", b"apk 131")]);
        install_windows(&archive, &app, 131, &mut rename).unwrap();
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None);
        confirm(&app);
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None);
        clean_up_after_update(&app, Some(131));
        assert_eq!(read(&app.join(EXE_NAME)), "exe 131");
        assert_eq!(read(&app.join("lib-131/python311.dll")), "py 131");
        assert_eq!(read(&app.join("Telescope.apk")), "apk 131");
        assert!(!app.join(STAGING_DIR).exists());
        assert!(!app.join(OLD_EXE_NAME).exists() && !app.join("lib-130").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_second_copy_starting_leaves_the_update_to_the_running_one() {
        let base = root("second");
        let app = updated_windows_app(&base);
        write_journal(&app, &with(&read_journal(&app).unwrap(), json!({"started": true}))).unwrap();
        let before = read_journal(&app);
        assert_eq!(recover(&app, Duration::ZERO, &|| true, Duration::ZERO), None);
        assert_eq!(read(&app.join(EXE_NAME)), "exe 131"); // not rolled back under the running copy
        assert_eq!(read_journal(&app), before);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_second_launch_while_the_new_version_is_still_loading_leaves_it_alone() {
        let base = root("loading");
        let app = updated_windows_app(&base);
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None); // the new version starts loading
        // The double-click lands before it takes the lock
        let bound_yet = std::cell::RefCell::new([false, false, true].into_iter());
        let running = || bound_yet.borrow_mut().next().expect("asked again after it was found running");
        assert_eq!(recover(&app, Duration::ZERO, &running, TRIAL_GRACE), None);
        assert_eq!(read(&app.join(EXE_NAME)), "exe 131");
        assert_eq!(failed_build(&app), None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_first_start_rolled_back_underneath_it_leaves_the_old_version_whole() {
        let base = root("underneath");
        let app = updated_windows_app(&base);
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None); // the new version starts, slowly
        // A second launch gives up waiting for it to take the lock, and puts the old version back
        assert!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO).is_some());
        assert_eq!(read(&app.join(EXE_NAME)), "exe 130");
        // The slow first start carries on regardless (renamed, still 131) and gets as far as its clean-up
        clean_up_after_update(&app, Some(131));
        assert_eq!(read(&app.join("lib-130/python311.dll")), "py 130"); // the exe in place still starts
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_roll_back_cut_short_is_finished_by_the_next_start() {
        let base = root("roll-cut");
        let counting = updated_windows_app(&base.join("count"));
        let trial = read_journal(&counting).unwrap();
        let total = count_moves(|mv| roll_back(&counting, &trial, true, mv).unwrap());
        assert_eq!(total, 4, "the journal, the new exe aside, the old one back, the old apk back");
        // Move 1 is the journal saying a roll-back began: dying on it, none did
        for k in 2..=total {
            let app = updated_windows_app(&base.join(format!("k{k}")));
            let journal = with(&read_journal(&app).unwrap(), json!({"started": true}));
            write_journal(&app, &journal).unwrap();
            let _ = catch_unwind(AssertUnwindSafe(|| roll_back(&app, &journal, true, &mut killed_at(k))));
            assert_eq!(read_journal(&app).unwrap()["state"], "rolling_back");
            assert!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO).is_some());
            assert_eq!(read(&app.join(EXE_NAME)), "exe 130", "k={k}");
            assert_eq!(read(&app.join("Telescope.apk")), "apk 130", "k={k}");
            assert!(read_journal(&app).is_none());
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_loaded_dll_is_moved_aside_for_the_new_one_and_comes_back_on_a_roll_back() {
        let base = root("loaded-dll");
        let app = windows_app(&base);
        write(&app.join("ui-kit/ui-kit.slint"), "kit 130");
        write(&app.join("avcodec-62.dll"), "codec 130");
        let archive = zip_of(&base.join(WINDOWS_ASSET), &[(EXE_NAME, b"exe 131"), ("ui-kit/ui-kit.slint", b"kit 131"), ("avcodec-62.dll", b"codec 131")]);
        // What Windows does with a loaded DLL: it can move, but nothing can be written over it
        let staging = app.join(STAGING_DIR);
        let mut loaded = |s: &Path, d: &Path| {
            if s.starts_with(&staging) && d.ends_with("avcodec-62.dll") && d.exists() {
                return Err(io::Error::from_raw_os_error(if cfg!(windows) { 5 } else { 13 }));
            }
            rename(s, d)
        };
        let result = install_windows(&archive, &app, 131, &mut loaded).unwrap();
        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
        assert_eq!(read(&app.join("avcodec-62.dll")), "codec 131");
        assert_eq!(read(&app.join(PREVIOUS_DIR).join("avcodec-62.dll")), "codec 130");
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None);
        assert!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO).is_some());
        assert_eq!(read(&app.join("avcodec-62.dll")), "codec 130");
        assert_eq!(read(&app.join("ui-kit/ui-kit.slint")), "kit 130");
        assert!(!app.join(PREVIOUS_DIR).exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_file_that_cant_be_moved_keeps_its_old_version() {
        let base = root("held");
        let app = windows_app(&base);
        write(&app.join("held.dll"), "held 130");
        let archive = zip_of(&base.join(WINDOWS_ASSET), &[(EXE_NAME, b"exe 131"), ("held.dll", b"held 131")]);
        let held = app.join("held.dll");
        let mut opened_elsewhere = |s: &Path, d: &Path| if s == held || d == held { Err(io::Error::from_raw_os_error(5)) } else { rename(s, d) };
        let result = install_windows(&archive, &app, 131, &mut opened_elsewhere).unwrap();
        assert_eq!(result.skipped, vec!["held.dll".to_string()]);
        assert_eq!(read(&held), "held 130");
        assert_eq!(read(&app.join(EXE_NAME)), "exe 131");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_held_file_leaves_a_python_lib_folder_the_next_start_still_recognises() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let base = root("lib-held");
        if std::fs::metadata(&base).unwrap().uid() == 0 {
            return; // root removes anything
        }
        let lib = base.join("lib-130");
        write(&lib.join("PyQt6/qt.dll"), "qt");
        write(&lib.join("VCRUNTIME140.dll"), "goes");
        write(&lib.join("A/held.dll"), "stays for now");
        std::fs::set_permissions(lib.join("A"), std::fs::Permissions::from_mode(0o500)).unwrap();
        clean_up_after_update(&base, None);
        std::fs::set_permissions(lib.join("A"), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(lib.join("PyQt6/qt.dll").exists(), "the mark goes last");
        assert!(!lib.join("VCRUNTIME140.dll").exists(), "and one held file doesn't stop the rest");
        clean_up_after_update(&base, None);
        assert!(!lib.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn the_python_venv_counts_as_left_only_beside_a_confirmed_python_update() {
        let base = root("venv");
        let (app, data) = (base.join("app"), base.join("data"));
        write(&data.join("Telescope/venv/.telescope-requirements"), "stamp");
        write(&app.join("telescope-rs"), "new");
        assert_eq!(python_venv_left(&app, Some(131), &data), None, "not after a Rust update");
        write(&app.join("update_guard.py"), "python's");
        assert_eq!(python_venv_left(&app, Some(131), &data), Some(data.join("Telescope/venv")));
        write(&app.join(JOURNAL), r#"{"state": "trial"}"#);
        assert_eq!(python_venv_left(&app, Some(131), &data), None, "not while the update can still roll back");
        std::fs::remove_file(app.join(JOURNAL)).unwrap();
        write(&app.join(FAILED), r#"{"build": 131}"#);
        assert_eq!(python_venv_left(&app, Some(131), &data), None, "not from a copy rolled back underneath it");
        std::fs::remove_file(app.join(FAILED)).unwrap();
        std::fs::remove_file(data.join("Telescope/venv/.telescope-requirements")).unwrap();
        assert_eq!(python_venv_left(&app, Some(131), &data), None, "not someone's own folder");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn clean_up_waits_while_an_update_is_unconfirmed() {
        let base = root("wait");
        let app = updated_windows_app(&base);
        clean_up_after_update(&app, Some(131));
        assert_eq!(read(&app.join(OLD_EXE_NAME)), "exe 130");
        assert!(app.join("lib-130").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn after_an_update_the_new_copy_waits_for_the_old_one_to_exit() {
        let base = root("waits");
        let app = updated_windows_app(&base);
        let still_running = std::cell::RefCell::new([true, true, false].into_iter());
        let running = || still_running.borrow_mut().next().unwrap();
        assert_eq!(recover(&app, Duration::from_secs(5), &running, Duration::ZERO), None);
        assert_eq!(read_journal(&app).unwrap()["started"], true);
        std::fs::remove_dir_all(&base).unwrap();
    }

    // ── Linux ─────────────────────────────────────────────────────────────────

    fn linux_app(dir: &Path) -> PathBuf {
        let app = dir.join("app");
        write(&app.join("main.py"), "main 130");
        write(&app.join("update_guard.py"), "guard 130");
        write(&app.join("start.sh"), "start 130");
        write(&app.join("telescope/app.py"), "app 130");
        write(&app.join("telescope/gone.py"), "only in 130");
        write(&app.join("notes.txt"), "the user's");
        app
    }

    fn linux_tarball(path: &Path) -> PathBuf {
        tarball(path, &[("main.py", b"main 131"), ("update_guard.py", b"guard 131"), ("start.sh", b"start 131"),
                        ("telescope/app.py", b"app 131"), ("requirements.txt", b"new in 131")])
    }

    /// The whole folder but the updater's own things.
    fn tree(app: &Path) -> BTreeMap<String, String> {
        fn walk(dir: &Path, app: &Path, out: &mut BTreeMap<String, String>) {
            for p in sorted_entries(dir).unwrap_or_default() {
                let name = p.file_name().unwrap().to_string_lossy();
                if name.starts_with(".update") || name.starts_with(".previous") {
                    continue;
                }
                if p.is_dir() {
                    walk(&p, app, out);
                } else {
                    out.insert(p.strip_prefix(app).unwrap().to_string_lossy().replace('\\', "/"), read(&p));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(app, app, &mut out);
        out
    }

    fn tree_of(files: &[(&str, &str)]) -> BTreeMap<String, String> {
        files.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn new_tree() -> BTreeMap<String, String> {
        tree_of(&[("main.py", "main 131"), ("update_guard.py", "guard 131"), ("start.sh", "start 131"), ("telescope/app.py", "app 131"),
                  ("requirements.txt", "new in 131"), ("notes.txt", "the user's")])
    }

    fn old_tree() -> BTreeMap<String, String> {
        tree_of(&[("main.py", "main 130"), ("update_guard.py", "guard 130"), ("start.sh", "start 130"), ("telescope/app.py", "app 130"),
                  ("telescope/gone.py", "only in 130"), ("notes.txt", "the user's")])
    }

    #[test]
    fn a_linux_install_cut_short_anywhere_is_finished_by_the_next_start() {
        let base = root("lin-cut");
        let total = count_moves(|mv| {
            let dir = base.join("count");
            let app = linux_app(&dir);
            install_linux(&linux_tarball(&dir.join("u.tar.gz")), &app, 131, mv).unwrap();
        });
        assert!(total >= 8, "the journal twice, five entries, telescope aside: {total}");
        let mut before_the_journal = 0;
        for k in 1..=total {
            let dir = base.join(format!("k{k}"));
            let app = linux_app(&dir);
            let archive = linux_tarball(&dir.join("u.tar.gz"));
            let _ = catch_unwind(AssertUnwindSafe(|| install_linux(&archive, &app, 131, &mut killed_at(k))));
            // start.sh, main.py and the guard are files, replaced in one step: whatever happened, the next start runs
            for name in ["start.sh", "main.py", "update_guard.py"] {
                assert!(app.join(name).exists(), "k={k} {name}");
            }
            let began = read_journal(&app).is_some();
            assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None, "k={k}");
            // Cut short before the journal was written, the update never began; after, it's always finished
            assert_eq!(tree(&app), if began { new_tree() } else { old_tree() }, "k={k}");
            if began {
                assert_eq!(read_journal(&app).unwrap()["state"], "trial");
                assert_eq!(read(&app.join(PREVIOUS_DIR).join("main.py")), "main 130");
                assert!(!app.join(STAGING_DIR).exists());
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(std::fs::metadata(app.join("start.sh")).unwrap().permissions().mode() & 0o111, 0o111);
                }
            } else {
                before_the_journal += 1;
            }
        }
        assert_eq!(before_the_journal, 1, "only the first move comes before the journal");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_linux_update_that_never_confirms_rolls_back() {
        let base = root("lin-roll");
        let app = linux_app(&base);
        install_linux(&linux_tarball(&base.join("u.tar.gz")), &app, 131, &mut rename).unwrap();
        assert_eq!(recover(&app, Duration::ZERO, &nobody, Duration::ZERO), None);
        assert_eq!(tree(&app), new_tree());
        let relaunch = recover(&app, Duration::ZERO, &nobody, Duration::ZERO);
        let start = app.join(if cfg!(windows) { EXE_NAME } else { "start.sh" });
        assert_eq!(relaunch, Some(vec![start.to_string_lossy().into_owned()]));
        assert_eq!(tree(&app), old_tree()); // including the module 131 dropped, and without the file 131 added
        assert_eq!(failed_build(&app), Some(131));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_linux_install_that_fails_outright_is_undone_but_not_blamed_on_the_build() {
        let base = root("lin-fail");
        let app = linux_app(&base);
        let staging = app.join(STAGING_DIR);
        let mut flaky = |s: &Path, d: &Path| {
            if d.ends_with("telescope") && s.starts_with(&staging) {
                return Err(io::Error::from_raw_os_error(if cfg!(windows) { 5 } else { 13 }));
            }
            rename(s, d)
        };
        let UpdateError(message) = install_linux(&linux_tarball(&base.join("u.tar.gz")), &app, 131, &mut flaky).unwrap_err();
        assert!(message.contains("denied") && !message.contains("os error"), "{message}");
        assert_eq!(tree(&app), old_tree());
        assert!(read_journal(&app).is_none());
        assert_eq!(failed_build(&app), None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    // ── A logout during the first start is not a failed start: tests/terminate.rs, with real signals ──

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
        assert!(!another_copy_running(false));
        let Acquired::Lock(lock) = acquire(&Address::current(), Duration::ZERO) else { panic!("no lock") };
        assert!(another_copy_running(false));
        drop(lock);
        assert!(!another_copy_running(false));
        std::env::remove_var(telescope_core::config::DEV_PROFILE_ENV);
        std::fs::remove_dir_all(&folder).unwrap();
        // Another user's copy doesn't count
        #[cfg(target_os = "linux")]
        {
            use crate::single_instance::instance_name;
            let (mine, theirs) = (Address::Abstract(instance_name(4242425, false)), Address::Abstract(instance_name(4242426, false)));
            let Acquired::Lock(lock) = acquire(&mine, Duration::ZERO) else { panic!("no lock") };
            assert!(copy_running_at(&mine, None));
            assert!(!copy_running_at(&theirs, None));
            drop(lock);
        }
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
