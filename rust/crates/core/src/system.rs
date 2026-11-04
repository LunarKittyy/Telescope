//! The desktop around the window: tray, notifications, autostart, one copy at a time, global shortcuts and updates. The system crate implements these.

use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A tray menu entry; plugins describe theirs with ui::TrayAction and the shell builds the menu.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrayItem {
    pub id: String,
    pub title: String,
    pub checkable: bool,
    pub checked: bool,
    pub visible: bool,
    pub enabled: bool,
}

pub trait Tray: Send {
    fn set_items(&mut self, items: &[TrayItem]);
    fn set_tooltip(&mut self, text: &str);
    /// What the icon shows: the camera streaming, and the mic streaming as Some(muted) (create_tray_icon).
    fn set_state(&mut self, _camera: bool, _mic: Option<bool>) {}
    /// The tray item clicked since last asked, or the icon itself as "".
    fn poll_click(&mut self) -> Option<String>;
}

pub trait Notifier: Send + Sync {
    /// Urgent ones stay on screen until dismissed (Linux).
    fn notify(&self, title: &str, body: &str, urgent: bool);
}

pub trait Autostart: Send + Sync {
    fn enabled(&self) -> bool;
    /// minimized: start hidden in the tray.
    fn set_enabled(&self, on: bool, minimized: bool) -> Result<(), String>;
}

/// What a second copy asked the first for (main.py's --action and plain starts).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstanceMessage {
    /// Someone started Telescope again: bring the window up.
    Show,
    /// --action NAME: run that shortcut action.
    Action { name: String },
}

/// Being the one running copy, and hearing from later ones.
pub trait SingleInstance: Send {
    /// Messages later copies sent since last asked.
    fn poll(&mut self) -> Vec<InstanceMessage>;
}

/// Registering global shortcuts with the desktop.
pub trait GlobalShortcuts: Send {
    /// (action id, key sequence like "Ctrl+Alt+S") pairs; returns the ones the desktop refused.
    fn register(&mut self, bindings: &[(String, String)]) -> Vec<String>;
    /// Action ids pressed since last asked.
    fn poll(&mut self) -> Vec<String>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub channel: String,
    pub notes: String,
}

pub trait Updater: Send + Sync {
    fn check(&self, channel: &str) -> Result<Option<Release>, String>;
    /// Downloads and swaps it in; the app restarts after.
    fn install(&self, release: &Release) -> Result<(), String>;
    /// The update guard's start-up step (update_guard.py): finish a cut-short swap, roll back one that never started.
    fn on_start(&self) -> Result<(), String>;
    /// The window is up, so a trial update counts as good.
    fn confirm_started(&self);
}

/// A local file or folder as a link (QUrl.fromLocalFile), for the system to open.
pub fn file_url(path: &std::path::Path) -> String {
    file_url_for(&path.display().to_string(), cfg!(windows))
}

/// file_url with the path as text and the system given.
pub fn file_url_for(path: &str, windows: bool) -> String {
    let text = if windows { path.replace('\\', "/") } else { path.to_string() };
    let mut out = String::from(if text.starts_with("//") { "file:" } else if windows { "file:///" } else { "file://" });
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::file_url_for;

    #[test]
    fn a_path_becomes_a_link_on_either_system() {
        assert_eq!(file_url_for("/tmp/telescope-me/telescope.log", false), "file:///tmp/telescope-me/telescope.log");
        assert_eq!(file_url_for("/home/me/My Logs/#1.log", false), "file:///home/me/My%20Logs/%231.log");
        assert_eq!(file_url_for(r"C:\Users\Åsa\AppData\telescope.log", true), "file:///C:/Users/%C3%85sa/AppData/telescope.log");
        assert_eq!(file_url_for(r"\\server\share\a b.log", true), "file://server/share/a%20b.log");
    }
}

/// A path without the `\\?\` that canonicalize puts in front on Windows, which Slint, a Run key or a shortcut can't use. A `\\?\UNC\` path stays as it is.
pub fn plain_path(path: PathBuf) -> PathBuf {
    let plain = path.to_string_lossy().strip_prefix(r"\\?\").filter(|rest| !rest.starts_with("UNC\\")).map(PathBuf::from);
    plain.unwrap_or(path)
}

#[cfg(test)]
mod plain_path_tests {
    use super::plain_path;
    use std::path::PathBuf;

    #[test]
    fn a_path_loses_its_verbatim_prefix() {
        assert_eq!(plain_path(PathBuf::from(r"\\?\C:\b\telescope.exe")), PathBuf::from(r"C:\b\telescope.exe"));
        assert_eq!(plain_path(PathBuf::from(r"\\?\UNC\pc\share\t.exe")), PathBuf::from(r"\\?\UNC\pc\share\t.exe"));
        assert_eq!(plain_path(PathBuf::from("/usr/bin/telescope")), PathBuf::from("/usr/bin/telescope"));
    }
}

/// An io error in the system's words, like Python's strerror: no "(os error N)", and none of the full stop Windows ends them with.
pub fn os_words(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].trim_end_matches('.').to_string(),
        None => text,
    }
}

#[cfg(test)]
mod os_words_tests {
    use super::os_words;
    use std::io::Error;

    #[test]
    fn only_the_systems_own_words_are_left() {
        let words = os_words(&Error::from_raw_os_error(2));
        assert!(!words.is_empty() && !words.contains("os error") && !words.ends_with('.'), "{words}");
        assert_eq!(os_words(&Error::other("our own words.")), "our own words.");
    }
}

/// This copy, running from an AppImage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppImage {
    /// The .AppImage file, which an update replaces and everything that starts Telescope points at.
    pub file: PathBuf,
    /// Started with --appimage-extract-and-run because this computer can't mount it, so starting it again needs that too.
    pub extracted: bool,
}

impl AppImage {
    /// The command that starts this file.
    pub fn command(&self) -> Vec<String> {
        let mut argv = vec![self.file.to_string_lossy().into_owned()];
        if self.extracted {
            argv.push("--appimage-extract-and-run".into());
        }
        argv
    }

    pub fn name(&self) -> String {
        self.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The folder holding the file, where an update's journal and the previous copy go.
    pub fn folder(&self) -> PathBuf {
        self.file.parent().map(Path::to_path_buf).unwrap_or_default()
    }
}

static APPIMAGE: OnceLock<AppImage> = OnceLock::new();

/// The AppImage this copy runs from, once leave_appimage_environment has looked.
pub fn appimage() -> Option<&'static AppImage> {
    APPIMAGE.get()
}

/// What the runtime's variables describe. Another AppImage's program can start a plain copy with these set, so they only count when this program is inside that mount.
fn appimage_from(file: Option<OsString>, mount: Option<OsString>, exe: Option<&Path>) -> Option<AppImage> {
    let (file, mount) = (PathBuf::from(file?), PathBuf::from(mount?));
    if !exe?.starts_with(&mount) || !file.is_file() {
        return None;
    }
    let mounted = mount.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".mount_"));
    Some(AppImage { file, extracted: !mounted })
}

/// LD_LIBRARY_PATH as the user had it: without the folders AppRun put in front.
fn without_mount(paths: &OsStr, mount: &Path) -> Option<OsString> {
    let theirs: Vec<PathBuf> = std::env::split_paths(paths).filter(|p| !p.starts_with(mount)).collect();
    if theirs.is_empty() { None } else { std::env::join_paths(theirs).ok() }
}

/// The runtime keeps its mount up for as long as anything holds a pipe it leaves open in here. Nothing started from here on may inherit that,
/// or an adb server or the next version would keep this copy's mount around after it quits.
#[cfg(unix)]
fn stop_passing_descriptors_on() {
    let Ok(list) = std::fs::read_dir("/proc/self/fd") else { return };
    let open: Vec<i32> = list.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect();
    for fd in open.into_iter().filter(|fd| *fd > 2) {
        // SAFETY: sets a flag on a descriptor number; one that's gone by now just fails
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
}

/// Windows slows and coalesces the timers of a program that's in the background, which a game in front makes this one. Asks it not to,
/// since a stream that has to hit every frame time can't wait. Does nothing elsewhere.
pub fn opt_out_of_power_throttling() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::*;
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
            StateMask: 0,
        };
        // SAFETY: state lives past the call and the size given is its own
        let done = unsafe { SetProcessInformation(GetCurrentProcess(), ProcessPowerThrottling, (&raw const state).cast(), size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32) };
        match done {
            Ok(()) => log::info!("Windows power throttling is off for this app, so it keeps its speed and timers in the background"),
            Err(e) => log::info!("Couldn't turn off Windows power throttling for this app: {e}"),
        }
    }
}

/// Asks Windows to schedule this program's graphics work ahead of normal programs', since a game in front keeps the card busy. Windows may
/// refuse without enough rights, which the log says. Does nothing elsewhere.
pub fn raise_gpu_scheduling_priority() {
    #[cfg(windows)]
    {
        use windows::Wdk::Graphics::Direct3D::{D3DKMTSetProcessSchedulingPriorityClass, D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH};
        use windows::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: both arguments are plain values
        let status = unsafe { D3DKMTSetProcessSchedulingPriorityClass(GetCurrentProcess(), D3DKMT_SCHEDULINGPRIORITYCLASS_HIGH) };
        if status.0 >= 0 {
            log::info!("This app's graphics work runs at a high GPU scheduling priority");
        } else {
            log::info!("Couldn't raise this app's GPU scheduling priority (status {:#x})", status.0);
        }
    }
}

/// A thread's place in Windows' multimedia scheduler, given back when this is dropped. Holds nothing elsewhere.
pub struct ThreadPriority {
    #[cfg(windows)]
    handle: Option<windows::Win32::Foundation::HANDLE>,
}

/// Puts the calling thread in the multimedia scheduler's Capture class, which keeps it running on time while a game takes the
/// processor. Keep the result for as long as the thread works. what names the thread in the log.
pub fn prioritise_thread(what: &str) -> ThreadPriority {
    #[cfg(windows)]
    {
        let mut index = 0u32;
        // SAFETY: the name is a constant string and index is a valid out-pointer
        let joined = unsafe { windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW(windows::core::w!("Capture"), &mut index) };
        match joined {
            Ok(handle) => {
                log::info!("The {what} thread runs with multimedia scheduling (Capture)");
                ThreadPriority { handle: Some(handle) }
            }
            Err(e) => {
                log::info!("Couldn't give the {what} thread multimedia scheduling: {e}");
                ThreadPriority { handle: None }
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = what;
        ThreadPriority {}
    }
}

#[cfg(windows)]
impl Drop for ThreadPriority {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: the handle came from AvSetMmThreadCharacteristicsW on this thread
            let _ = unsafe { windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(handle) };
        }
    }
}

/// Call this first in main, before any thread starts. Notes the AppImage this copy runs from and clears what its runtime and AppRun added
/// to the environment, which every program started from here on would inherit: another app's own libraries must not lose to ours.
/// Starting this copy again has to go through start_command, since the mount ends with this process.
pub fn leave_appimage_environment() {
    let mount = std::env::var_os("APPDIR");
    let exe = std::env::current_exe().ok();
    let Some(image) = appimage_from(std::env::var_os("APPIMAGE"), mount.clone(), exe.as_deref()) else { return };
    let _ = APPIMAGE.set(image);
    #[cfg(unix)]
    stop_passing_descriptors_on();
    let mount = PathBuf::from(mount.unwrap_or_default());
    for name in ["APPIMAGE", "APPDIR", "ARGV0", "OWD", "APPIMAGE_EXTRACT_AND_RUN"] {
        std::env::remove_var(name);
    }
    // FONTCONFIG_FILE stays: AppRun only sets it on a system with no fontconfig of its own, and libraries here load it whenever they first want fonts
    // The loader read this when the program started, so changing it now only reaches what runs next
    if let Some(paths) = std::env::var_os("LD_LIBRARY_PATH") {
        match without_mount(&paths, &mount) {
            Some(theirs) => std::env::set_var("LD_LIBRARY_PATH", theirs),
            None => std::env::remove_var("LD_LIBRARY_PATH"),
        }
    }
}

/// The command that starts this copy: the AppImage's file, since the program itself sits in a mount that goes when it quits.
pub fn start_command() -> std::io::Result<Vec<String>> {
    match appimage() {
        Some(image) => Ok(image.command()),
        None => std::env::current_exe().map(|exe| vec![exe.to_string_lossy().into_owned()]),
    }
}

#[cfg(all(test, unix))]
mod appimage_tests {
    use super::*;

    #[test]
    fn the_runtimes_variables_only_count_for_a_program_inside_that_mount() {
        let file = std::env::temp_dir().join(format!("telescope-appimage-{}", std::process::id()));
        std::fs::write(&file, "x").unwrap();
        let exe = Path::new("/tmp/.mount_TelescAbCdEf/telescope-rs");
        let found = |f: &Path, m: &str, e: &Path| appimage_from(Some(f.into()), Some(m.into()), Some(e));
        assert_eq!(found(&file, "/tmp/.mount_TelescAbCdEf", exe), Some(AppImage { file: file.clone(), extracted: false }));
        assert_eq!(found(&file, "/tmp/.mount_Other", exe), None, "started by another AppImage");
        assert_eq!(found(Path::new("/nonexistent"), "/tmp/.mount_TelescAbCdEf", exe), None);
        assert_eq!(appimage_from(None, Some("/tmp/.mount_TelescAbCdEf".into()), Some(exe)), None);
        let extracted = found(&file, "/tmp/appimage_extracted_abc", Path::new("/tmp/appimage_extracted_abc/telescope-rs")).unwrap();
        assert!(extracted.extracted);
        assert_eq!(extracted.command(), [file.to_string_lossy().as_ref(), "--appimage-extract-and-run"]);
        std::fs::remove_file(&file).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn what_the_runtime_left_open_doesnt_reach_other_programs() {
        let mut ends = [0; 2];
        // SAFETY: room for the two descriptors
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        stop_passing_descriptors_on();
        for fd in ends {
            // SAFETY: reads a flag, then closes this test's own descriptor
            unsafe {
                assert_eq!(libc::fcntl(fd, libc::F_GETFD), libc::FD_CLOEXEC);
                libc::close(fd);
            }
        }
    }

    #[test]
    fn the_users_library_path_comes_back_without_ours() {
        let mount = Path::new("/tmp/.mount_T");
        assert_eq!(without_mount(OsStr::new("/tmp/.mount_T/lib/fallback:/opt/mine:/usr/lib"), mount), Some("/opt/mine:/usr/lib".into()));
        assert_eq!(without_mount(OsStr::new("/tmp/.mount_T/lib/fallback"), mount), None);
    }
}
