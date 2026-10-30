//! Moving an unzipped Windows copy to an installed one (updates.py, "Moving an unzipped copy to an installed one (Windows)").
//!
//! Updating an unzipped copy runs the release's TelescopeSetup.exe instead of swapping files in place: the installed copy starts, and
//! once it has started fine it removes the folder it replaced. What decides this is plain data (`Place`), so the rules can be checked
//! from any system; only gathering that data and running the installer is Windows-only.

use crate::update_guard::{self, EXE_NAME, FAILED_EXE_NAME, OLD_EXE_NAME, PREVIOUS_DIR, STAGING_DIR};
use crate::updates::{InstallResult, UpdateError};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use telescope_core::i18n::tr;

/// installer/Telescope.iss; never changes.
pub const SETUP_APP_ID: &str = "{BF097F77-F8B9-4F70-B23C-0B500DB1A07C}";
/// Tells the installed copy which unzipped folder it replaced.
pub const MOVED_FROM: &str = "--moved-from";
/// The same, kept in the installed folder until that folder is gone.
pub const PENDING_MOVE: &str = ".moved-from";
/// How long the installer may take.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(600);

/// The facts is_unzipped_copy goes by.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub windows: bool,
    /// The packaged app (its exe is in the folder) rather than a build run from a source checkout.
    pub packaged: bool,
    /// The folder the setup installs to.
    pub is_install_dir: bool,
    /// On a removable or network drive.
    pub carried_around: bool,
    /// An unins*.exe sits in the folder: the installer made it.
    pub has_uninstaller: bool,
}

/// The packaged Windows app, run from a folder someone unzipped rather than one the installer made.
pub fn is_unzipped_copy(place: &Place) -> bool {
    // Unzipped where the setup installs: running the setup would close this copy mid-update
    place.windows && place.packaged && !place.is_install_dir
        // On a USB stick or a share it's portable on purpose: moving it would empty the stick
        && !place.carried_around && !place.has_uninstaller
}

/// Whether updating goes through the installer: an unzipped copy, not kept portable, with a setup to use. An older version registered
/// the camera driver from this folder, so deleting it would take the camera away: until Advanced's reinstall fixes that, this copy
/// updates in place.
pub fn moves_on_update(keep_portable: bool, setup_offered: bool, unzipped: bool, driver_in_app_folder: impl FnOnce() -> bool) -> bool {
    if keep_portable || !setup_offered || !unzipped {
        return false;
    }
    !driver_in_app_folder()
}

/// Where TelescopeSetup.exe installs for one user (%LOCALAPPDATA%\Programs\Telescope).
pub fn default_install_dir(local_app_data: Option<&str>) -> Option<PathBuf> {
    local_app_data.filter(|v| !v.is_empty()).map(|v| Path::new(v).join("Programs").join("Telescope"))
}

fn normal(path: &Path) -> String {
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()).to_string_lossy().into_owned();
    // os.path.normcase: case only stops mattering on Windows
    if cfg!(windows) { full.to_lowercase() } else { full }
}

/// Whether two folders are the same place (case does not matter on Windows).
pub fn same_place(a: &Path, b: Option<&Path>) -> bool {
    b.is_some_and(|b| normal(a) == normal(b))
}

pub(crate) fn has_uninstaller(directory: &Path) -> bool {
    std::fs::read_dir(directory).is_ok_and(|list| {
        list.flatten().any(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            name.starts_with("unins") && name.ends_with(".exe")
        })
    })
}

/// On a removable drive (2) or a network one (4), going by GetDriveTypeW of the root ("C:\\"); a \\server\share is one too.
pub fn carried_by(root: &str, drive_type: &dyn Fn(&str) -> u32) -> bool {
    root.starts_with(r"\\") || matches!(drive_type(&format!("{root}\\")), 2 | 4)
}

#[cfg(windows)]
fn carried_around(directory: &Path) -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDriveTypeW(root: *const u16) -> u32;
    }
    let full = std::fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf()).to_string_lossy().into_owned();
    let full = full.strip_prefix(r"\\?\UNC\").map(|rest| format!(r"\\{rest}")).unwrap_or_else(|| full.trim_start_matches(r"\\?\").to_string());
    let root: String = if full.starts_with(r"\\") { full.clone() } else { full.chars().take(2).collect() };
    carried_by(&root, &|root| {
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: a NUL-terminated string that outlives the call
        unsafe { GetDriveTypeW(wide.as_ptr()) }
    })
}

#[cfg(not(windows))]
fn carried_around(_directory: &Path) -> bool {
    false
}

impl Place {
    /// directory: the folder this copy runs from.
    pub fn of(directory: &Path) -> Place {
        let local = std::env::var("LOCALAPPDATA").ok();
        Place::of_with(directory, cfg!(windows), local.as_deref(), &carried_around)
    }

    /// The same with whether it's Windows, %LOCALAPPDATA% and the drive check given.
    pub fn of_with(directory: &Path, windows: bool, local_app_data: Option<&str>, carried: &dyn Fn(&Path) -> bool) -> Place {
        Place {
            windows,
            packaged: directory.join(EXE_NAME).is_file(),
            is_install_dir: same_place(directory, default_install_dir(local_app_data).as_deref()),
            carried_around: windows && carried(directory),
            has_uninstaller: has_uninstaller(directory),
        }
    }
}

// ── Where the installer put it ───────────────────────────────────────────────

/// The installer's uninstall entry, under HKEY_CURRENT_USER.
pub fn uninstall_key() -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{SETUP_APP_ID}_is1")
}

/// The text of a registry value's data (UTF-16, as winreg gets it) up to its first NUL; None unless it is a REG_SZ or REG_EXPAND_SZ.
pub fn registry_text(kind: u32, data: &[u16]) -> Option<String> {
    matches!(kind, 1 | 2).then(|| String::from_utf16_lossy(data.split(|&unit| unit == 0).next().unwrap_or_default()))
}

/// A string value under hive ("HKCU" or "HKCR") at key, "" for the key's default value, read from the registry itself (winreg's way)
/// rather than from reg.exe's text, which comes out in a code page.
#[cfg(windows)]
fn read_registry(hive: &str, key: &str, name: &str) -> Option<String> {
    use std::ffi::c_void;
    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(key: *mut c_void, sub: *const u16, options: u32, access: u32, out: *mut *mut c_void) -> i32;
        fn RegQueryValueExW(key: *mut c_void, name: *const u16, reserved: *mut u32, kind: *mut u32, data: *mut u8, len: *mut u32) -> i32;
        fn RegCloseKey(key: *mut c_void) -> i32;
    }
    const KEY_READ: u32 = 0x20019;
    let root = match hive {
        "HKCR" => 0x8000_0000_usize,
        "HKCU" => 0x8000_0001,
        _ => return None,
    } as *mut c_void;
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let (key, name) = (wide(key), wide(name));
    // SAFETY: the strings are NUL-terminated and outlive the calls; the first query only asks the size, the second reads into a buffer that big
    unsafe {
        let mut open = std::ptr::null_mut();
        if RegOpenKeyExW(root, key.as_ptr(), 0, KEY_READ, &mut open) != 0 {
            return None;
        }
        let (mut kind, mut len) = (0u32, 0u32);
        let mut text = None;
        if RegQueryValueExW(open, name.as_ptr(), std::ptr::null_mut(), &mut kind, std::ptr::null_mut(), &mut len) == 0 {
            let mut buf = vec![0u16; (len as usize).div_ceil(2) + 1];
            let mut size = (buf.len() * 2) as u32;
            if RegQueryValueExW(open, name.as_ptr(), std::ptr::null_mut(), &mut kind, buf.as_mut_ptr().cast(), &mut size) == 0 {
                text = registry_text(kind, &buf[..size as usize / 2]);
            }
        }
        RegCloseKey(open);
        text
    }
}

/// What reading the uninstall entry needs of the registry.
pub trait Uninstall {
    /// A string value under hive ("HKCU") at key.
    fn read(&self, hive: &str, key: &str, name: &str) -> Option<String>;
}

/// Where the installer put Telescope for this user, from its uninstall entry; None if it isn't installed.
pub fn installed_location_in(registry: &dyn Uninstall) -> Option<PathBuf> {
    registry.read("HKCU", &uninstall_key(), "InstallLocation").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// This computer's registry.
#[cfg(windows)]
struct Machine;

#[cfg(windows)]
impl Uninstall for Machine {
    fn read(&self, hive: &str, key: &str, name: &str) -> Option<String> {
        read_registry(hive, key, name)
    }
}

/// Where the installer put Telescope for this user, from its uninstall entry; None if it isn't installed.
#[cfg(windows)]
pub fn installed_location() -> Option<PathBuf> {
    installed_location_in(&Machine)
}

#[cfg(not(windows))]
pub fn installed_location() -> Option<PathBuf> {
    None
}

/// The DLL paths the UnityCapture camera filters are registered with are in this folder: an older Telescope registered the driver
/// straight from the app's folder (windows.uc_in_app_folder), which Program Files' copy is not.
pub fn driver_registered_here(registered: &[String], app_dir: &Path) -> bool {
    let ours = app_dir.join("unitycapture").join("UnityCaptureFilter64.dll").to_string_lossy().to_lowercase();
    registered.iter().any(|p| p.to_lowercase().replace('/', "\\") == ours.replace('/', "\\"))
}

/// The same check against this computer's registry: the DLL each of the camera filters Telescope registers points to.
#[cfg(windows)]
pub fn driver_in_app_folder(app_dir: &Path) -> bool {
    let found: Vec<String> = (0..4u32).filter_map(|num| {
        let last = 0x10 + num + u32::from(num != 0);
        read_registry("HKCR", &format!(r"CLSID\{{5C2CD55C-92AD-4999-8666-912BD3E700{last:02X}}}\InprocServer32"), "")
    }).collect();
    driver_registered_here(&found, app_dir)
}

#[cfg(not(windows))]
pub fn driver_in_app_folder(_app_dir: &Path) -> bool {
    false
}

// ── Desktop shortcuts to the copy that goes away ─────────────────────────────

/// Where Windows keeps desktop shortcuts for this user: the profile's Desktop, and OneDrive's when it backs that up.
pub fn desktop_folders() -> Vec<PathBuf> {
    ["USERPROFILE", "OneDrive"].iter().filter_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
        .map(|base| Path::new(&base).join("Desktop")).filter(|p| p.is_dir()).collect()
}

fn utf16_text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    String::from_utf16_lossy(&units).to_lowercase()
}

/// The bytes as the system's ANSI text (the "mbcs" Python reads a .lnk's link info with on Windows), latin-1 elsewhere.
#[cfg(windows)]
fn ansi_text(bytes: &[u8]) -> String {
    #[link(name = "kernel32")]
    extern "system" {
        fn MultiByteToWideChar(page: u32, flags: u32, from: *const u8, from_len: i32, to: *mut u16, to_len: i32) -> i32;
    }
    let Ok(len) = i32::try_from(bytes.len()) else { return String::new() };
    // SAFETY: the first call only asks how much room is needed, the second fills a buffer of that size
    unsafe {
        let n = MultiByteToWideChar(0, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0);
        if n <= 0 {
            return String::new();
        }
        let mut wide = vec![0u16; n as usize];
        MultiByteToWideChar(0, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), n);
        String::from_utf16_lossy(&wide).to_lowercase()
    }
}

#[cfg(not(windows))]
fn ansi_text(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect::<String>().to_lowercase()
}

/// Desktop shortcuts (.lnk) that start exe. A .lnk keeps its target's path as text (ANSI in its link info, UTF-16 in its strings), so a
/// plain search for it is enough to tell, without reading the format.
pub fn shortcuts_to(exe: &Path, folders: &[PathBuf]) -> Vec<PathBuf> {
    let target = exe.to_string_lossy().to_lowercase();
    let mut found = Vec::new();
    for folder in folders {
        let Ok(list) = std::fs::read_dir(folder) else { continue };
        for link in list.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| if cfg!(windows) { e.eq_ignore_ascii_case("lnk") } else { e == "lnk" })) {
            let Ok(mut data) = std::fs::read(&link) else { continue };
            data.truncate(64 * 1024);
            let odd = data.get(1..).unwrap_or_default();
            let texts = [utf16_text(&data), utf16_text(odd), ansi_text(&data)];
            if texts.iter().any(|t| t.contains(&target)) {
                found.push(link);
            }
        }
    }
    found.sort();
    found
}

// ── Running the installer ────────────────────────────────────────────────────

/// How a run of the installer ended.
#[derive(Debug)]
pub enum SetupRun {
    Exited(i32),
    TimedOut,
    CouldNotStart(io::Error),
}

/// Starts argv without a window and waits for it, up to `timeout`.
pub fn run_setup(argv: &[String], timeout: Duration) -> SetupRun {
    let Some((program, args)) = argv.split_first() else { return SetupRun::CouldNotStart(io::ErrorKind::InvalidInput.into()) };
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return SetupRun::CouldNotStart(e),
    };
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return SetupRun::Exited(status.code().unwrap_or(-1)),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return SetupRun::TimedOut;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return SetupRun::CouldNotStart(e),
        }
    }
}

/// What an OSError's strerror says: the system's words for it, without Rust's "(os error N)" after.
fn strerror(e: &io::Error) -> String {
    let text = e.to_string();
    match (e.raw_os_error(), text.rfind(" (os error ")) {
        (Some(_), Some(at)) => text[..at].to_string(),
        _ => text,
    }
}

/// What install_with_setup needs from the computer, so a test can stand in for it.
pub struct SetupEnv<'a> {
    pub run: &'a dyn Fn(&[String]) -> SetupRun,
    pub locate: &'a dyn Fn() -> Option<PathBuf>,
    /// Where desktop shortcuts are.
    pub folders: &'a [PathBuf],
}

/// Runs the downloaded installer silently for this user; the result starts the installed copy, which tidies away old_dir once it has
/// started fine (it's noted in that folder too, see pending_move). This copy is left as it is until then, so a failed install loses
/// nothing. A desktop shortcut to this copy, which won't work once its folder goes, gets the installed copy's desktop icon in its place.
pub fn install_with_setup(setup: &Path, old_dir: &Path, env: &SetupEnv) -> Result<InstallResult, UpdateError> {
    let mut argv: Vec<String> = [setup.to_string_lossy().as_ref(), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"].map(String::from).to_vec();
    if !shortcuts_to(&old_dir.join(EXE_NAME), env.folders).is_empty() {
        argv.push("/MERGETASKS=desktopicon".into()); // installer/Telescope.iss [Tasks]
    }
    match (env.run)(&argv) {
        SetupRun::TimedOut => return Err(UpdateError(tr("The installer took too long, so this copy stays as it is."))),
        SetupRun::CouldNotStart(e) => {
            return Err(UpdateError(tr("Couldn't start the installer: {error}").replace("{error}", &strerror(&e))));
        }
        SetupRun::Exited(0) => {}
        SetupRun::Exited(code) => {
            return Err(UpdateError(tr("The installer stopped (code {code}), so this copy stays as it is.").replace("{code}", &code.to_string())));
        }
    }
    let Some(location) = (env.locate)().filter(|l| l.join(EXE_NAME).is_file()) else {
        return Err(UpdateError(tr("Telescope was installed, but its folder wasn't where expected. Open it from the Start menu.")));
    };
    let mut relaunch = vec![location.join(EXE_NAME).to_string_lossy().into_owned(), "--after-update".into()];
    if std::fs::canonicalize(&location).ok() != std::fs::canonicalize(old_dir).ok() {
        relaunch.extend([MOVED_FROM.into(), old_dir.to_string_lossy().into_owned()]);
        // A journal from that folder's own in-app updates is about files the setup just replaced; left there, the first start would
        // "roll back" the fresh install to them
        if update_guard::clear_journal(&location).is_err() {
            log::warn!("Couldn't clear the installed copy's old update journal");
        }
        // The argument alone is lost if the first start doesn't get as far as tidying up (a crash, a later reopen)
        let part = location.join(format!("{PENDING_MOVE}.tmp"));
        let noted = std::fs::write(&part, old_dir.to_string_lossy().as_bytes()).and_then(|()| std::fs::rename(&part, location.join(PENDING_MOVE)));
        if noted.is_err() {
            log::warn!("Couldn't note the old copy for the installed one");
        }
    }
    Ok(InstallResult { relaunch, skipped: Vec::new() })
}

/// Called once the installed copy has been started from here. Should it not start, this copy is opened again and shouldn't offer that
/// build straight back (Try again in Updates does); once it has, it deletes this folder and the marker with it.
pub fn mark_started_elsewhere(build: i64, old_dir: &Path) {
    if build != 0 {
        let _ = std::fs::write(old_dir.join(update_guard::FAILED), serde_json::json!({"build": build}).to_string());
    }
}

// ── Tidying away the replaced copy ───────────────────────────────────────────

/// The unzipped folder this installed copy replaced and hasn't removed yet, or None.
pub fn pending_move(directory: &Path) -> Option<PathBuf> {
    let marker = directory.join(PENDING_MOVE);
    let Ok(bytes) = std::fs::read(&marker) else { return None };
    // Not text: nothing to finish, and it would fail the same way at every start
    let text = String::from_utf8(bytes).map(|t| t.trim().to_string()).unwrap_or_default();
    if !text.is_empty() && !text.contains('\0') && Path::new(&text).is_absolute() {
        return Some(PathBuf::from(text));
    }
    let _ = std::fs::remove_file(&marker);
    None
}

/// What an unzipped copy holds that's unmistakably Telescope's. A zip unpacked straight into Downloads shares that folder with the
/// user's own things, which may include a platform-tools from Google's SDK or a lib-something folder, so a folder only counts as
/// Telescope's when its name and its contents both match.
fn ours(entry: &Path) -> io::Result<bool> {
    const FILES: [&str; 8] = [OLD_EXE_NAME, FAILED_EXE_NAME, update_guard::JOURNAL, ".update.json.tmp", update_guard::FAILED,
                              "THIRD_PARTY_NOTICES.txt", "Telescope.apk", "telescope.log"];
    let folders: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::from([
        ("platform-tools", BTreeSet::from(["adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll", "libwinpthread-1.dll", "NOTICE"])),
        ("unitycapture", BTreeSet::from(["UnityCaptureFilter32.dll", "UnityCaptureFilter64.dll", "LICENSE"])),
        ("virtualcamera", BTreeSet::from(["TelescopeCamera.dll"])),
    ]);
    let name = entry.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Ok(meta) = std::fs::symlink_metadata(entry) else { return Ok(false) };
    if meta.file_type().is_symlink() {
        return Ok(name == "telescope.log"); // the log shortcut; never follow anything else
    }
    if meta.is_file() {
        return Ok(FILES.contains(&name.as_str()));
    }
    if !meta.is_dir() {
        return Ok(false);
    }
    if name == STAGING_DIR || name == PREVIOUS_DIR {
        return Ok(true); // only Telescope's updater makes these dot-folders
    }
    if let Some(allowed) = folders.get(name.as_str()) {
        // A folder that can't be looked into is an error, not an empty one
        let inside = std::fs::read_dir(entry)?.map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned())).collect::<io::Result<BTreeSet<String>>>()?;
        return Ok(inside.iter().all(|n| allowed.contains(n.as_str())));
    }
    // The Python app's libraries: lib-<build>, with the Qt it can't run without
    Ok(name.strip_prefix(update_guard::LIB_PREFIX).is_some_and(|b| !b.is_empty() && b.bytes().all(|c| c.is_ascii_digit())) && entry.join("PyQt6").is_dir())
}

/// Where the adb Telescope downloads goes for this user (adb_download.downloaded_dir): outside the app's folder, so moves leave it alone.
pub fn downloaded_adb_dir(local_app_data: Option<&str>, home: &Path) -> PathBuf {
    let base = local_app_data.filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join("AppData").join("Local"));
    base.join("Telescope").join("platform-tools")
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// What removing the old copy does to the computer, so a test can stand in for the parts that fail (a file the exiting copy still holds).
pub struct Ops<'a> {
    pub remove: &'a dyn Fn(&Path) -> io::Result<()>,
    pub ours: &'a dyn Fn(&Path) -> io::Result<bool>,
    /// Where an adb that came with the old copy is kept for the installed one.
    pub adb_dest: PathBuf,
}

impl Ops<'static> {
    pub fn system() -> Ops<'static> {
        let local = std::env::var("LOCALAPPDATA").ok();
        Ops { remove: &update_guard::remove, ours: &ours, adb_dest: downloaded_adb_dir(local.as_deref(), &std::env::home_dir().unwrap_or_default()) }
    }
}

/// Keeps an adb that came with the old copy where Get adb would put it (adb_download.downloaded_dir).
fn keep_adb(bundled: &Path, ops: &Ops) {
    let kept = || -> io::Result<()> {
        let dest = &ops.adb_dest;
        if !(ops.ours)(bundled)? || !bundled.join("adb.exe").is_file() || dest.join("adb.exe").exists() {
            return Ok(());
        }
        let parent = dest.parent().ok_or_else(|| io::Error::other("no folder to keep it in"))?;
        std::fs::create_dir_all(parent)?;
        let part = parent.join(".adb-moving");
        let _ = std::fs::remove_dir_all(&part);
        copy_tree(bundled, &part)?;
        std::fs::rename(&part, dest)
    };
    if let Err(e) = kept() {
        log::warn!("Couldn't keep the old copy's adb: {e}");
    }
}

/// Deletes the unzipped copy this one replaced: only entries that are unmistakably Telescope's, the exe last so a copy cut short still
/// runs, then the folder if that left it empty. Refuses anything that isn't an unzipped copy of Telescope. True once Telescope's part
/// of it is gone.
pub fn remove_unzipped_copy(old_dir: &Path, current_dir: &Path, tries: u32, pause: Duration, folders: &[PathBuf]) -> bool {
    remove_unzipped_copy_with(old_dir, current_dir, tries, pause, folders, &Ops::system())
}

/// remove_unzipped_copy with what it does to the computer given.
pub fn remove_unzipped_copy_with(old_dir: &Path, current_dir: &Path, tries: u32, pause: Duration, folders: &[PathBuf], ops: &Ops) -> bool {
    let exe = old_dir.join(EXE_NAME);
    let same = std::fs::canonicalize(old_dir).ok() == std::fs::canonicalize(current_dir).ok();
    if same || !exe.is_file() || std::fs::symlink_metadata(&exe).is_ok_and(|m| m.file_type().is_symlink()) || has_uninstaller(old_dir) {
        return false; // an installed copy removes itself with its own uninstaller
    }
    keep_adb(&old_dir.join("platform-tools"), ops);
    let dead_links = shortcuts_to(&exe, folders); // read before the exe goes; the setup put a working one beside them
    let mut left: Vec<String> = Vec::new();
    for attempt in 0..tries {
        left.clear();
        let entries: Vec<PathBuf> = match ours_in(old_dir, &exe, ops) {
            Ok(entries) => entries,
            Err(_) => {
                left.push(old_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                Vec::new() // couldn't look inside, so the exe stays too
            }
        };
        for entry in entries.iter().chain(exe.exists().then_some(&exe)) {
            if entry == &exe && !left.is_empty() {
                break; // keep the exe until the rest has gone, so what's left can still start
            }
            if (ops.remove)(entry).is_err() {
                left.push(entry.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            }
        }
        if left.is_empty() {
            let _ = std::fs::remove_dir(old_dir); // only if empty
            for link in &dead_links {
                let _ = std::fs::remove_file(link); // they started the exe that just went
            }
            return true;
        }
        if attempt + 1 < tries {
            std::thread::sleep(pause); // the old copy may still be exiting and holding its files
        }
    }
    log::warn!("Couldn't remove the old unzipped copy's {}", left.join(", "));
    false
}

/// What's in old_dir, other than the exe, that's unmistakably Telescope's.
fn ours_in(old_dir: &Path, exe: &Path, ops: &Ops) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(old_dir)? {
        let path = entry?.path();
        if path != exe && (ops.ours)(&path)? {
            found.push(path);
        }
    }
    Ok(found)
}

/// Remove the replaced unzipped copy; forget it once its exe is gone (or it was never one), else try again at the next start, when
/// whatever held its files has let go.
pub fn finish_move(old_dir: &Path, current_dir: &Path, folders: &[PathBuf]) -> bool {
    finish_move_with(old_dir, current_dir, folders, 20, Duration::from_millis(500), &Ops::system())
}

/// finish_move with how often and how patiently it tries, and what it does to the computer, given.
pub fn finish_move_with(old_dir: &Path, current_dir: &Path, folders: &[PathBuf], tries: u32, pause: Duration, ops: &Ops) -> bool {
    let done = remove_unzipped_copy_with(old_dir, current_dir, tries, pause, folders, ops);
    if done || !old_dir.join(EXE_NAME).is_file() {
        let _ = std::fs::remove_file(current_dir.join(PENDING_MOVE));
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("telescope-move-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, data: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    fn unzipped() -> Place {
        Place { windows: true, packaged: true, ..Place::default() }
    }

    #[test]
    fn only_a_packaged_windows_copy_in_an_ordinary_folder_counts_as_unzipped() {
        assert!(is_unzipped_copy(&unzipped()));
        assert!(!is_unzipped_copy(&Place { windows: false, ..unzipped() }), "Linux never moves");
        assert!(!is_unzipped_copy(&Place { packaged: false, ..unzipped() }), "a source checkout");
        assert!(!is_unzipped_copy(&Place { is_install_dir: true, ..unzipped() }), "unzipped where the setup installs");
        assert!(!is_unzipped_copy(&Place { carried_around: true, ..unzipped() }), "on a stick or a share");
        assert!(!is_unzipped_copy(&Place { has_uninstaller: true, ..unzipped() }), "the installer made it");
    }

    #[test]
    fn a_move_needs_a_setup_an_unzipped_copy_and_no_driver_in_this_folder() {
        assert!(moves_on_update(false, true, true, || false));
        assert!(!moves_on_update(true, true, true, || false), "kept portable");
        assert!(!moves_on_update(false, false, true, || false), "a release from before the installer");
        assert!(!moves_on_update(false, true, false, || false), "an installed copy");
        assert!(!moves_on_update(false, true, true, || true), "deleting this folder would take the camera");
    }

    #[test]
    fn the_driver_check_is_asked_last() {
        let asked = RefCell::new(0);
        assert!(!moves_on_update(true, true, true, || { *asked.borrow_mut() += 1; false }));
        assert_eq!(*asked.borrow(), 0, "the registry is only read when it matters");
    }

    #[test]
    fn the_per_user_install_folder_comes_from_localappdata() {
        assert_eq!(default_install_dir(Some(r"C:\Users\l\AppData\Local")), Some(Path::new(r"C:\Users\l\AppData\Local").join("Programs").join("Telescope")));
        assert_eq!(default_install_dir(Some("")), None);
        assert_eq!(default_install_dir(None), None);
    }

    #[test]
    fn a_registered_driver_in_the_app_folder_is_found_whatever_the_case() {
        let app = Path::new("C:\\Users\\l\\Downloads\\Telescope");
        let here = app.join("unitycapture").join("UnityCaptureFilter64.dll").to_string_lossy().to_uppercase();
        assert!(driver_registered_here(&[here], app));
        assert!(!driver_registered_here(&[r"C:\Program Files\Telescope\UnityCapture\UnityCaptureFilter64.dll".into()], app));
        assert!(!driver_registered_here(&[], app));
    }

    #[test]
    fn the_uninstall_entry_is_the_installers() {
        assert!(uninstall_key().ends_with("{BF097F77-F8B9-4F70-B23C-0B500DB1A07C}_is1"));
    }

    // winreg gives the text as it is stored, so a folder with a letter outside the system's code page survives
    #[test]
    fn a_registry_string_keeps_every_character_of_the_path() {
        let stored = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        for path in [r"C:\Users\Åsa\AppData\Local\Programs\Telescope\", r"C:\Users\田中\Telescope", r"D:\Игра\Telescope"] {
            assert_eq!(registry_text(1, &stored(path)).as_deref(), Some(path));
            assert_eq!(registry_text(2, &stored(path)).as_deref(), Some(path), "REG_EXPAND_SZ is a string too");
        }
        // Whatever follows the NUL is not part of the value, and a value stored without one is all of it
        assert_eq!(registry_text(1, &[u16::from(b'a'), 0, u16::from(b'b')]).as_deref(), Some("a"));
        assert_eq!(registry_text(1, &[u16::from(b'a'), u16::from(b'b')]).as_deref(), Some("ab"));
        assert_eq!(registry_text(1, &[]).as_deref(), Some(""));
        // A number or binary value is not a path
        assert_eq!(registry_text(4, &stored("1")), None);
        assert_eq!(registry_text(3, &stored("1")), None);
    }

    /// An unzipped copy at root/old and an installed one at root/new.
    fn two_copies(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = tmp(name);
        let (old, new) = (root.join("old"), root.join("new"));
        write(&old.join(EXE_NAME), "old exe");
        write(&new.join(EXE_NAME), "new exe");
        (root, old, new)
    }

    #[test]
    fn the_installer_runs_silently_and_the_installed_copy_is_started() {
        let (root, old, new) = two_copies("setup");
        write(&new.join(update_guard::JOURNAL), "{}"); // from that folder's own in-app updates
        let seen = RefCell::new(Vec::new());
        let run = |argv: &[String]| { *seen.borrow_mut() = argv.to_vec(); SetupRun::Exited(0) };
        let locate = || Some(new.clone());
        let result = install_with_setup(Path::new("Setup.exe"), &old, &SetupEnv { run: &run, locate: &locate, folders: &[] }).unwrap();
        assert_eq!(*seen.borrow(), ["Setup.exe", "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"]);
        assert_eq!(result.relaunch, [new.join(EXE_NAME).to_string_lossy().into_owned(), "--after-update".into(), MOVED_FROM.into(), old.to_string_lossy().into_owned()]);
        assert!(!new.join(update_guard::JOURNAL).exists(), "the fresh install must not be rolled back to an old journal");
        assert_eq!(pending_move(&new), Some(old.clone()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_desktop_shortcut_to_the_old_copy_asks_for_the_new_ones_icon() {
        let (root, old, new) = two_copies("setup-icon");
        let desktop = root.join("Desktop");
        let target = old.join(EXE_NAME).to_string_lossy().into_owned();
        write(&desktop.join("Telescope.lnk"), &format!("....{target}...."));
        write(&desktop.join("Other.lnk"), "something else");
        let seen = RefCell::new(Vec::new());
        let run = |argv: &[String]| { *seen.borrow_mut() = argv.to_vec(); SetupRun::Exited(0) };
        let locate = || Some(new.clone());
        install_with_setup(Path::new("Setup.exe"), &old, &SetupEnv { run: &run, locate: &locate, folders: std::slice::from_ref(&desktop) }).unwrap();
        assert_eq!(seen.borrow().last().map(String::as_str), Some("/MERGETASKS=desktopicon"));
        assert_eq!(shortcuts_to(&old.join(EXE_NAME), &[desktop]).len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_shortcut_with_the_path_in_utf16_is_found_too() {
        let root = tmp("lnk16");
        let exe = root.join("Åsa").join(EXE_NAME);
        let bytes: Vec<u8> = std::iter::once(0u8).chain(exe.to_string_lossy().encode_utf16().flat_map(u16::to_le_bytes)).collect();
        std::fs::write(root.join("a.lnk"), bytes).unwrap();
        assert_eq!(shortcuts_to(&exe, std::slice::from_ref(&root)).len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_failed_installer_leaves_this_copy_alone_and_says_why() {
        let (root, old, new) = two_copies("setup-fail");
        let locate = || Some(new.clone());
        let fail = |run: &dyn Fn(&[String]) -> SetupRun| install_with_setup(Path::new("Setup.exe"), &old, &SetupEnv { run, locate: &locate, folders: &[] }).unwrap_err().0;
        assert!(fail(&|_| SetupRun::TimedOut).contains("took too long"));
        assert!(fail(&|_| SetupRun::CouldNotStart(io::Error::other("no such file"))).contains("Couldn't start the installer: no such file"));
        assert!(fail(&|_| SetupRun::Exited(5)).contains("code 5"));
        assert!(!new.join(PENDING_MOVE).exists());
        let nowhere = || None;
        let e = install_with_setup(Path::new("Setup.exe"), &old, &SetupEnv { run: &|_| SetupRun::Exited(0), locate: &nowhere, folders: &[] }).unwrap_err();
        assert!(e.0.contains("wasn't where expected"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn installing_over_the_same_folder_moves_nothing() {
        let (root, old, _new) = two_copies("setup-same");
        let locate = || Some(old.clone());
        let result = install_with_setup(Path::new("Setup.exe"), &old, &SetupEnv { run: &|_| SetupRun::Exited(0), locate: &locate, folders: &[] }).unwrap();
        assert_eq!(result.relaunch.len(), 2);
        assert!(!old.join(PENDING_MOVE).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_marker_that_names_nothing_usable_is_dropped() {
        let root = tmp("pending");
        write(&root.join(PENDING_MOVE), "relative/path");
        assert_eq!(pending_move(&root), None);
        assert!(!root.join(PENDING_MOVE).exists());
        assert_eq!(pending_move(&root), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_replaced_copy_goes_but_a_users_own_files_stay() {
        let (root, old, new) = two_copies("remove");
        write(&old.join("Telescope.apk"), "apk");
        write(&old.join("platform-tools/adb.exe"), "adb");
        write(&old.join("platform-tools/mine.txt"), "a SDK file");
        write(&old.join("lib-12/PyQt6/qt.dll"), "qt");
        write(&old.join("lib-project/main.rs"), "the user's");
        write(&old.join("notes.txt"), "the user's");
        write(&new.join(PENDING_MOVE), &old.to_string_lossy());
        let done = remove_unzipped_copy(&old, &new, 1, Duration::ZERO, &[]);
        assert!(done, "Telescope's part of it is gone, the rest is left to the user");
        assert!(!old.join("Telescope.apk").exists() && !old.join("lib-12").exists());
        assert!(old.join("platform-tools/mine.txt").exists() && old.join("lib-project/main.rs").exists() && old.join("notes.txt").exists());
        // Once the exe is gone there is nothing left to try again, so the marker goes
        assert!(!old.join(EXE_NAME).exists() && new.join(PENDING_MOVE).exists());
        assert!(!finish_move(&old, &new, &[]));
        assert!(!new.join(PENDING_MOVE).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_copy_with_only_telescope_in_it_is_removed_whole() {
        let (root, old, new) = two_copies("remove-all");
        write(&old.join("Telescope.apk"), "apk");
        write(&old.join(".update-staging/x"), "x");
        let desktop = root.join("Desktop");
        write(&desktop.join("Telescope.lnk"), &old.join(EXE_NAME).to_string_lossy());
        assert!(remove_unzipped_copy(&old, &new, 1, Duration::ZERO, std::slice::from_ref(&desktop)));
        assert!(!old.exists());
        assert!(!desktop.join("Telescope.lnk").exists(), "it started the exe that just went");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn nothing_that_is_not_an_unzipped_copy_is_removed() {
        let (root, old, new) = two_copies("refuse");
        assert!(!remove_unzipped_copy(&new, &new, 1, Duration::ZERO, &[]), "this very folder");
        write(&old.join("unins000.exe"), "uninstaller");
        assert!(!remove_unzipped_copy(&old, &new, 1, Duration::ZERO, &[]), "an installed copy has its own uninstaller");
        assert!(old.join(EXE_NAME).exists());
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert!(!remove_unzipped_copy(&empty, &new, 1, Duration::ZERO, &[]), "no Telescope in it");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_marker_in_the_old_copy_stops_it_being_offered_the_same_build_again() {
        let (root, old, _new) = two_copies("marker");
        mark_started_elsewhere(0, &old);
        assert!(!old.join(update_guard::FAILED).exists());
        mark_started_elsewhere(131, &old);
        assert_eq!(update_guard::failed_build(&old), Some(131));
        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
#[path = "setup_move_tests.rs"]
mod python_tests;
