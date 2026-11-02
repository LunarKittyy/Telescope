//! The virtual microphone other apps record from (platform/virtual_mic.py).
//!
//! Linux: a PulseAudio / PipeWire pipe source named "Telescope Microphone", fed through a FIFO. It's an input only, so
//! it never shows up as a speaker. Needs only pactl, and it's gone after a reboot (or when the mic is switched off).
//! Windows: VB-Audio Virtual Cable, installed separately (vb_cable.rs); Telescope plays into "CABLE Input" and apps
//! record from "CABLE Output".

use telescope_core::i18n::Fields;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use telescope_core::config::dev_profile;
use telescope_core::i18n::tr;

pub const LINUX_TOOLS: &[&str] = &["pactl"];
pub const VB_CABLE_PLAYBACK: &str = "CABLE Input";
pub const VB_CABLE_RECORD: &str = "CABLE Output";

/// A dev profile gets its own source, so setting it up doesn't unload the real app's.
pub fn source() -> &'static str {
    if dev_profile().is_some() { "telescope_dev_mic" } else { "telescope_mic" }
}

pub fn source_name() -> &'static str {
    if dev_profile().is_some() { "Telescope Microphone (dev)" } else { "Telescope Microphone" }
}

/// Tags of every module an earlier run may have left loaded (the first builds used a null sink plus a remapped source).
fn ours() -> Vec<String> {
    let mut tags = vec![format!("source_name={}", source())];
    if dev_profile().is_none() {
        tags.push("sink_name=telescope_mic_sink".into());
    }
    tags
}

/// Runs a command: (ok, its output or its error).
pub type Run<'a> = &'a dyn Fn(&[String]) -> (bool, String);

/// subprocess.run with a 5 s limit.
pub fn run(cmd: &[String]) -> (bool, String) {
    run_within(cmd, Duration::from_secs(5))
}

/// subprocess.run with a limit.
pub fn run_within(cmd: &[String], limit: Duration) -> (bool, String) {
    let Some((exe, args)) = cmd.split_first() else { return (false, String::new()) };
    let mut command = std::process::Command::new(exe);
    command.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return (false, e.to_string()),
    };
    let until = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return (false, format!("Command '{}' timed out after {} seconds", cmd.join(" "), limit.as_secs()));
            }
            Err(e) => return (false, e.to_string()),
        }
    }
    match child.wait_with_output() {
        Ok(out) => {
            let ok = out.status.success();
            let text = String::from_utf8_lossy(if ok { &out.stdout } else { &out.stderr }).trim().to_string();
            (ok, text)
        }
        Err(e) => (false, e.to_string()),
    }
}

/// shutil.which.
pub fn which(tool: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|p| p.join(tool)).find(|p| p.is_file()))
}

pub fn linux_tools_missing(which: &dyn Fn(&str) -> Option<PathBuf>) -> Vec<String> {
    LINUX_TOOLS.iter().filter(|t| which(t).is_none()).map(|t| t.to_string()).collect()
}

/// XDG_RUNTIME_DIR is private to this user by spec; without it, a folder of our own, never a shared temp folder itself.
pub fn fifo_path() -> std::io::Result<PathBuf> {
    fifo_path_in(std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).map(PathBuf::from), &std::env::temp_dir())
}

pub fn fifo_path_in(runtime: Option<PathBuf>, temp: &Path) -> std::io::Result<PathBuf> {
    let dir = match runtime {
        Some(d) => d,
        None => private_temp_dir(temp)?,
    };
    Ok(dir.join(format!("{}.fifo", source().replace('_', "-"))))
}

#[cfg(unix)]
fn private_temp_dir(temp: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // SAFETY: getuid has no failure mode
    let uid = unsafe { libc::getuid() };
    let path = temp.join(format!("telescope-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    // Someone else could have made it first in a shared temp folder: use it only if it's a real folder, ours, and closed
    let st = std::fs::symlink_metadata(&path)?;
    if !st.file_type().is_dir() || st.uid() != uid || st.mode() & 0o077 != 0 {
        return Err(std::io::Error::other(format!(
            "{} isn't a private folder of this user's, so the microphone won't use it", path.display())));
    }
    Ok(path)
}

#[cfg(not(unix))]
fn private_temp_dir(temp: &Path) -> std::io::Result<PathBuf> {
    Ok(temp.to_path_buf())
}

/// The loaded modules (a `pactl list short modules` answer) an earlier run of this profile left behind.
pub fn stale_modules(out: &str) -> Vec<u32> {
    let tags = ours();
    out.lines()
        .filter(|line| tags.iter().any(|t| line.contains(t.as_str())))
        .filter_map(|line| line.split('\t').next().and_then(|id| id.parse().ok()))
        .collect()
}

/// Whether every one of these modules is still loaded as one of ours (a restarted sound server forgets them, and may
/// number others the same).
pub fn modules_loaded(ids: &[u32], run: Run) -> bool {
    let (ok, out) = run(&strings(&["pactl", "list", "short", "modules"]));
    if !ok {
        return false;
    }
    let ours = stale_modules(&out);
    ids.iter().all(|id| ours.contains(id))
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// Unloads a source a crashed run left behind. Quiet when there is no sound server or no pactl.
pub fn remove_stale_source(run: Run) {
    let (ok, out) = run(&strings(&["pactl", "list", "short", "modules"]));
    if ok {
        linux_teardown(&stale_modules(&out), run);
    }
}

/// Creates the source. Anything a crashed run left behind is unloaded first, so the FIFO is always the one this run
/// writes to. (module ids to unload later, error text or "").
pub fn linux_setup(run: Run, fifo: Option<PathBuf>) -> (Vec<u32>, String) {
    linux_setup_at(run, || fifo.map(Ok).unwrap_or_else(fifo_path))
}

fn linux_setup_at(run: Run, fifo: impl FnOnce() -> std::io::Result<PathBuf>) -> (Vec<u32>, String) {
    let (ok, out) = run(&strings(&["pactl", "list", "short", "modules"]));
    if !ok {
        return (Vec::new(), tr("The sound server didn't answer (pactl list failed)."));
    }
    linux_teardown(&stale_modules(&out), run);
    let fifo = match fifo() {
        Ok(f) => f,
        Err(e) => return (Vec::new(), tr("Couldn't create the virtual microphone: {error}").field("error", e.to_string())),
    };
    let _ = std::fs::remove_file(&fifo);
    let base = vec![
        "pactl".to_string(), "load-module".into(), "module-pipe-source".into(), format!("source_name={}", source()),
        format!("file={}", fifo.display()), "format=s16le".into(), "rate=48000".into(), "channels=1".into(),
    ];
    // The inner quotes keep the space; PulseAudio rejects the other quoting styles. If a sound server rejects this one
    // too, a mic listed as "telescope_mic" beats no mic.
    let mut last = String::new();
    for extra in [vec![format!("source_properties=\"device.description='{}'\"", source_name())], vec![]] {
        let mut cmd = base.clone();
        cmd.extend(extra);
        let (ok, out) = run(&cmd);
        if let Ok(id) = out.trim().parse::<u32>() {
            if ok && out.trim().chars().all(|c| c.is_ascii_digit()) {
                return (vec![id], String::new());
            }
        }
        last = out;
    }
    let error = if last.is_empty() { "pactl failed".to_string() } else { last };
    (Vec::new(), tr("Couldn't create the virtual microphone: {error}").field("error", &error))
}

pub fn linux_teardown(module_ids: &[u32], run: Run) {
    for id in module_ids.iter().rev() {
        run(&["pactl".to_string(), "unload-module".into(), id.to_string()]);
    }
}

/// Index of VB-Cable's playback end among (name, output channels), or None.
pub fn find_vb_cable(devices: &[(String, u32)]) -> Option<usize> {
    devices.iter().position(|(name, outs)| name.to_lowercase().contains(&VB_CABLE_PLAYBACK.to_lowercase()) && *outs > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Pactl {
        modules: String,
        fail: bool,
        calls: RefCell<Vec<Vec<String>>>,
        next_id: RefCell<u32>,
    }

    impl Pactl {
        fn new(modules: &str, fail: bool) -> Pactl {
            Pactl { modules: modules.into(), fail, calls: RefCell::new(Vec::new()), next_id: RefCell::new(40) }
        }
        fn run(&self, cmd: &[String]) -> (bool, String) {
            self.calls.borrow_mut().push(cmd.to_vec());
            if cmd[1..3] == ["list", "short"] {
                return (true, self.modules.clone());
            }
            if cmd[1] == "load-module" {
                if self.fail {
                    return (false, "Module initialization failed".into());
                }
                *self.next_id.borrow_mut() += 1;
                return (true, self.next_id.borrow().to_string());
            }
            (true, String::new())
        }
        fn called(&self, cmd: &[&str]) -> bool {
            self.calls.borrow().iter().any(|c| c == &strings(cmd))
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("telescope-vmic-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn linux_setup_creates_an_input_only_source() {
        let p = Pactl::new("", false);
        let fifo = scratch("setup").join("mic.fifo");
        let (ids, err) = linux_setup(&|c| p.run(c), Some(fifo.clone()));
        assert!(err.is_empty() && ids == vec![41]);
        let load = p.calls.borrow()[1].clone();
        assert_eq!(load[2], "module-pipe-source");
        assert!(load.contains(&"source_name=telescope_mic".into()) && load.contains(&format!("file={}", fifo.display())));
        for arg in ["format=s16le", "rate=48000", "channels=1"] {
            assert!(load.contains(&arg.to_string()));
        }
        assert!(load.contains(&"source_properties=\"device.description='Telescope Microphone'\"".to_string()));
        assert!(!load.iter().any(|a| a.contains("sink"))); // nothing that shows up as a speaker
        linux_teardown(&ids, &|c| p.run(c));
        assert_eq!(p.calls.borrow().last().unwrap(), &strings(&["pactl", "unload-module", "41"]));
    }

    #[test]
    fn linux_setup_clears_what_an_earlier_run_left_loaded() {
        let p = Pactl::new(&["7\tmodule-null-sink\tsink_name=telescope_mic_sink sink_properties=...",
                             "8\tmodule-remap-source\tmaster=telescope_mic_sink.monitor source_name=telescope_mic",
                             "9\tmodule-alsa-card\tdevice_id=0"].join("\n"), false);
        let stale = scratch("stale").join("mic.fifo");
        std::fs::write(&stale, "left over").unwrap();
        let (ids, err) = linux_setup(&|c| p.run(c), Some(stale.clone()));
        assert!(err.is_empty() && ids == vec![41]);
        assert!(p.called(&["pactl", "unload-module", "8"]) && p.called(&["pactl", "unload-module", "7"]));
        assert!(!p.called(&["pactl", "unload-module", "9"]));
        assert!(!stale.exists()); // the module makes a fresh FIFO
    }

    #[test]
    fn linux_setup_reports_a_failed_load() {
        let p = Pactl::new("", true);
        let (ids, err) = linux_setup(&|c| p.run(c), Some(scratch("fail").join("f")));
        assert!(ids.is_empty() && err.contains("Module initialization failed"));
        let loads: Vec<Vec<String>> = p.calls.borrow().iter().filter(|c| c[1] == "load-module").cloned().collect();
        assert!(loads.len() == 2 && !loads[1].iter().any(|a| a.contains("source_properties"))); // retried plain
    }

    #[test]
    fn linux_needs_only_pactl() {
        assert_eq!(linux_tools_missing(&|_| None), vec!["pactl".to_string()]);
        assert!(linux_tools_missing(&|t| Some(PathBuf::from("/usr/bin").join(t))).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn without_a_runtime_dir_the_fifo_goes_in_a_private_folder() {
        use std::os::unix::fs::PermissionsExt;
        let temp = scratch("private");
        let path = fifo_path_in(None, &temp).unwrap();
        let folder = path.parent().unwrap();
        // SAFETY: getuid has no failure mode
        assert_eq!(folder, temp.join(format!("telescope-{}", unsafe { libc::getuid() })));
        assert_eq!(std::fs::metadata(folder).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn a_shared_or_linked_fallback_folder_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let temp = scratch("shared");
        // SAFETY: getuid has no failure mode
        let folder = temp.join(format!("telescope-{}", unsafe { libc::getuid() }));
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(fifo_path_in(None, &temp).is_err());
        std::fs::remove_dir(&folder).unwrap();
        let elsewhere = temp.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &folder).unwrap();
        let err = fifo_path_in(None, &temp).unwrap_err();
        assert!(err.to_string().contains("private folder"));
        let p = Pactl::new("", false);
        let (ids, err) = linux_setup_at(&|c| p.run(c), || fifo_path_in(None, &temp));
        assert!(ids.is_empty() && err.contains("private folder"), "{err}");
    }

    #[test]
    fn vb_cable_is_found_by_its_playback_end() {
        let devices = vec![("Speakers".to_string(), 2), ("CABLE Output (VB-Audio Virtual Cable)".to_string(), 0),
                           ("CABLE Input (VB-Audio Virtual Cable)".to_string(), 2)];
        assert_eq!(find_vb_cable(&devices), Some(2));
        assert_eq!(find_vb_cable(&devices[..2]), None);
    }

    #[test]
    fn remove_stale_source_unloads_only_ours() {
        let p = Pactl::new(&["8\tmodule-pipe-source\tsource_name=telescope_mic file=/run/user/1000/telescope-mic.fifo",
                             "10\tmodule-pipe-source\tsource_name=telescope_dev_mic file=/run/user/1000/telescope-dev-mic.fifo",
                             "9\tmodule-alsa-card\tdevice_id=0"].join("\n"), false);
        remove_stale_source(&|c| p.run(c));
        assert!(p.called(&["pactl", "unload-module", "8"]));
        assert!(!p.calls.borrow().iter().any(|c| c[1] == "load-module"));
        assert!(!p.called(&["pactl", "unload-module", "9"]));
        assert!(!p.called(&["pactl", "unload-module", "10"])); // a running dev profile's source stays
    }

    #[test]
    fn remove_stale_source_is_quiet_without_a_sound_server() {
        let calls = RefCell::new(Vec::new());
        remove_stale_source(&|c| {
            calls.borrow_mut().push(c.to_vec());
            (false, "No such file or directory".into())
        });
        assert_eq!(calls.into_inner(), vec![strings(&["pactl", "list", "short", "modules"])]);
    }
}
