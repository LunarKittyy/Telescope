//! Going back to the Python version: one action, pinned to the last Python stable release (4.4.2), for anyone who wants that back.
//!
//! Nothing about the target comes from the network except the file itself. The release, its build number and the SHA-256 of each
//! package are written down here, so a changed or swapped release file fails the checksum instead of being trusted. The swap is the
//! normal update's (updates::install_linux and install_windows_if, through update_guard's journal), so a cut-short or failed one puts
//! this version back, and Python's own update_guard.py reads the same journal: if 4.4.2 doesn't start, its next launch rolls back to this.
//! What its start.sh would do before it gets to that guard (make the Python environment) is done first, so a failure leaves nothing swapped.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use telescope_core::i18n::{tr, Fields};

use crate::update_guard::{self, Mover};
use crate::updates::{self, Asset, InstallResult, UpdateError};
use crate::version;

/// The last Python stable release.
pub const VERSION: &str = "4.4.2";
/// Its build number (manifest.json), which the journal and the rolled-back marker carry.
pub const BUILD: i64 = 1441;
/// What each package of that release is, from its manifest.json: nothing here is read from the network.
const LINUX_SHA256: &str = "6acbc37dd8cb8fdf494ca02978a92d679129e3571a707c6124754d1b1bfcf909";
const LINUX_SIZE: u64 = 7_108_274;
const WINDOWS_SHA256: &str = "2ef523d90a7ca1fe1e029dd4fd23e93bfd392090ce71b4f4b7389baceab00853";
const WINDOWS_SIZE: u64 = 148_795_816;
/// Written in the settings folder once the swap is in; see settle().
pub const MARKER: &str = ".going-back.json";
/// What the settings backup's name says it is: `telescope_config.json.rust-<time>`.
const BACKUP_LABEL: &str = "rust";

pub fn release_url() -> String {
    format!("https://github.com/{}/releases/tag/v{VERSION}", version::REPO)
}

/// The package of the pinned release for this system.
pub fn pinned_asset(windows: bool) -> Asset {
    let (name, sha256, size) = if windows {
        (updates::WINDOWS_ASSET, WINDOWS_SHA256, WINDOWS_SIZE)
    } else {
        (updates::LINUX_ASSET, LINUX_SHA256, LINUX_SIZE)
    };
    Asset { name: name.into(), url: format!("https://github.com/{}/releases/download/v{VERSION}/{name}", version::REPO), sha256: sha256.into(), size }
}

/// What the Python build's launcher (start.sh) can find on this computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PythonState {
    /// The environment it made before still works.
    Ready,
    /// There's a Python 3.12 or newer to make it with, so the first start sets it up again (internet, a few minutes).
    NeedsSetup,
    /// Neither: start.sh would stop at once, and nothing could roll back from there.
    Missing,
}

/// What the action does in this copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Not offered (a source checkout).
    Hidden,
    /// Swap the files here, as an update does.
    Swap { python_setup: bool },
    /// Can't swap here: say what to download instead (an AppImage, a folder this user can't write to).
    Download { appimage: bool },
    /// The Python build wouldn't start on this computer.
    NoPython,
}

impl Plan {
    pub fn offered(self) -> bool {
        self != Plan::Hidden
    }
}

/// Where Telescope's data lives on Linux ($XDG_DATA_HOME or ~/.local/share): the Python build's environment is under it.
pub fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute())
        .unwrap_or_else(|| telescope_core::config::home().join(".local").join("share"))
}

const NEW_ENOUGH: &str = "import sys; sys.exit(0 if sys.version_info >= (3, 12) else 1)";

/// The interpreter start.sh makes its environment with: the first of these that is new enough. run(program, code) says whether `program -c code` succeeded.
fn launcher_python(run: &dyn Fn(&str, &str) -> bool) -> Option<&'static str> {
    ["python3.14", "python3.13", "python3.12", "python3"].into_iter().find(|candidate| run(candidate, NEW_ENOUGH))
}

/// What start.sh would find. Same order and checks as start.sh: the interpreter it picks must be able to make the environment.
pub fn python_state(data_home: &Path, run: &dyn Fn(&str, &str) -> bool) -> PythonState {
    const CAN_MAKE_ENVIRONMENT: &str = "import ensurepip, venv";
    let environment = data_home.join("Telescope").join("venv").join("bin").join("python3");
    if environment.is_file() && run(&environment.to_string_lossy(), NEW_ENOUGH) {
        return PythonState::Ready;
    }
    match launcher_python(run) {
        Some(python) if run(python, CAN_MAKE_ENVIRONMENT) => PythonState::NeedsSetup,
        _ => PythonState::Missing,
    }
}

/// What start.sh does before it launches 4.4.2 when the environment is missing: makes it and installs the package's requirements. start.sh
/// runs that under `set -e` before its guard, so a failure there (no internet, a data folder it can't write) would leave a copy that can't
/// start and can't roll itself back. Done here instead, on the unpacked package and before anything of this copy is swapped, it leaves
/// start.sh a ready environment and a matching stamp to skip. A failure removes what it made and the install stops with nothing changed.
/// check is run_python; run(folder, argv) runs a command there and says whether it worked.
pub fn prepare_python(staging: &Path, data_home: &Path, check: &dyn Fn(&str, &str) -> bool, run: &dyn Fn(&Path, &[String]) -> bool) -> Result<(), UpdateError> {
    let folder = data_home.join("Telescope");
    let venv = folder.join("venv");
    let failed = || {
        let _ = update_guard::remove(&venv);
        UpdateError(tr("Couldn't set up the Python libraries {version} needs. That takes the internet and a folder it can write to ({folder}). Nothing was changed.")
            .field("version", VERSION).field("folder", folder.display().to_string()))
    };
    let Some(python) = launcher_python(check) else { return Err(UpdateError(no_python())) };
    // An environment that's there but can't run is what start.sh would delete and make again
    update_guard::remove(&venv).map_err(|_| failed())?;
    let (own, venv_arg) = (venv.join("bin").join("python3").to_string_lossy().into_owned(), venv.to_string_lossy().into_owned());
    if !run(staging, &[python.into(), "-m".into(), "venv".into(), venv_arg]) {
        return Err(failed());
    }
    let pip = |extra: &[&str], files: &[&str]| -> Vec<String> {
        let mut argv: Vec<String> = [own.as_str(), "-m", "pip", "install", "--quiet", "--disable-pip-version-check"].iter().map(|s| s.to_string()).collect();
        argv.extend(extra.iter().map(|s| s.to_string()));
        for file in files {
            argv.extend(["-r", file].map(String::from));
        }
        if !files.is_empty() && staging.join("constraints.txt").is_file() {
            argv.extend(["-c", "constraints.txt"].map(String::from));
        }
        argv
    };
    // start.sh carries on without a newer pip
    let _ = run(staging, &pip(&["--upgrade", "pip"], &[]));
    if !run(staging, &pip(&[], &["requirements.txt"])) {
        return Err(failed());
    }
    // aiortc goes in on its own, without its dependencies (requirements-nodeps.txt)
    if staging.join("requirements-nodeps.txt").is_file() && !run(staging, &pip(&["--no-deps"], &["requirements-nodeps.txt"])) {
        return Err(failed());
    }
    // The stamp start.sh compares: `cat requirements.txt requirements-nodeps.txt constraints.txt | cksum`
    let wanted: Vec<u8> = ["requirements.txt", "requirements-nodeps.txt", "constraints.txt"].iter().filter_map(|f| std::fs::read(staging.join(f)).ok()).flatten().collect();
    std::fs::write(venv.join(".telescope-requirements"), format!("{}\n", cksum(&wanted))).map_err(|_| failed())
}

/// What `cksum` prints for these bytes (the POSIX CRC): "<checksum> <length>".
pub fn cksum(data: &[u8]) -> String {
    let table: Vec<u32> = (0..256u32).map(|i| (0..8).fold(i << 24, |c, _| if c & 0x8000_0000 != 0 { (c << 1) ^ 0x04C1_1DB7 } else { c << 1 })).collect();
    let step = |crc: u32, byte: u8| (crc << 8) ^ table[((crc >> 24) as u8 ^ byte) as usize];
    let mut crc = data.iter().fold(0, |crc, b| step(crc, *b));
    let mut length = data.len();
    while length != 0 {
        crc = step(crc, (length & 0xff) as u8);
        length >>= 8;
    }
    format!("{} {}", !crc, data.len())
}

/// `argv` in folder, which a cancelled() stops (the child is killed and that counts as a failure). stdin and output are discarded.
pub fn run_command(folder: &Path, argv: &[String], cancelled: &dyn Fn() -> bool) -> bool {
    use std::process::{Command, Stdio};
    let Some((program, args)) = argv.split_first() else { return false };
    let Ok(mut child) = Command::new(program).args(args).current_dir(folder).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if cancelled() => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(_) => return false,
        }
    }
}

/// `program -c code` for real.
pub fn run_python(program: &str, code: &str) -> bool {
    use std::process::{Command, Stdio};
    Command::new(program).args(["-c", code]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// What the action does for a copy on this channel in this folder. python is only asked on Linux, for a copy that swaps.
pub fn plan(channel: &str, directory: &Path, appimage: bool, windows: bool, python: &dyn Fn() -> PythonState) -> Plan {
    if channel == "dev" {
        return Plan::Hidden;
    }
    if appimage {
        return Plan::Download { appimage: true };
    }
    if !updates::writable(directory) {
        return Plan::Download { appimage: false };
    }
    if windows {
        return Plan::Swap { python_setup: false };
    }
    match python() {
        PythonState::Missing => Plan::NoPython,
        PythonState::NeedsSetup => Plan::Swap { python_setup: true },
        PythonState::Ready => Plan::Swap { python_setup: false },
    }
}

/// The confirmation: what happens and what's lost, in plain words. settings_folder is where the settings copy goes.
pub struct Dialog {
    pub text: String,
    /// The button that goes ahead.
    pub action: String,
    pub enabled: bool,
}

pub fn dialog(plan: Plan, settings_folder: &Path) -> Dialog {
    let version = || VERSION;
    let paragraphs: Vec<String>;
    let (action, enabled) = match plan {
        Plan::Hidden => {
            paragraphs = Vec::new();
            (String::new(), false)
        }
        Plan::Swap { python_setup } => {
            let mut text = vec![
                tr("Telescope restarts as version {version}, the last Python version. Your settings and paired phones come with it, and a copy of your current settings is saved first in {folder}.")
                    .field("version", version()).field("folder", settings_folder.display().to_string()),
                tr("You lose what's new since then. Plugins written in Lua don't run in {version}, and settings that didn't exist then are ignored.").field("version", version()),
                phone_note(),
                tr("{version} won't check for updates by itself, so it doesn't offer this version back. You can turn that on again in its settings.").field("version", version()),
            ];
            if python_setup {
                text.push(tr("First it sets up the Python libraries {version} needs. That needs the internet and takes a few minutes. If that fails, nothing is changed.").field("version", version()));
            }
            text.push(tr("If it doesn't start, opening Telescope again brings this version back."));
            paragraphs = text;
            (tr("Go back"), true)
        }
        Plan::Download { appimage } => {
            let first = if appimage {
                tr("An AppImage can't be swapped for the Python version. Download Telescope-linux.tar.gz from the {version} release, unpack it and run start.sh in it. It needs Python 3.12 or newer and finds your settings in the same place.")
            } else {
                tr("Telescope can't swap its own files here, because its folder isn't writable. Download {version} from its release page and install it yourself.")
            };
            paragraphs = vec![first.field("version", version()), phone_note()];
            (tr("Open download page"), true)
        }
        Plan::NoPython => {
            paragraphs = vec![no_python()];
            (tr("Go back"), false)
        }
    };
    Dialog { text: paragraphs.join("\n\n"), action, enabled }
}

pub fn no_python() -> String {
    tr("{version} needs Python 3.12 or newer with venv, and this computer doesn't have it. Install it first (python3 and python3-venv on Debian or Ubuntu), then try again. Nothing was changed.").field("version", VERSION)
}

fn phone_note() -> String {
    tr("The phone app has to match. A newer one may not connect to {version}. Android won't install an older app over a newer one, so uninstall Telescope on the phone, install Telescope.apk from the {version} release and pair again.").field("version", VERSION)
}

// ── Installing ────────────────────────────────────────────────────────────────

/// Puts the unpacked pinned package in place of this copy through update_guard's journal, so a failure midway puts this version back.
/// Windows skips the "does it load" run that updates make, which would start the Python exe as if it were the whole app.
/// Linux runs prepare on the unpacked package first (prepare_python): a failure from it leaves everything as it was.
pub fn install_from(archive: &Path, directory: &Path, windows: bool, prepare: &dyn Fn(&Path) -> Result<(), UpdateError>, mv: Mover) -> Result<InstallResult, UpdateError> {
    if windows {
        updates::install_windows_if(archive, directory, BUILD, mv, &|_| true)
    } else {
        updates::install_linux_after(archive, directory, BUILD, mv, prepare)
    }
}

/// A dated copy of the settings next to them, taken before anything changes. None: there is no settings file yet.
pub fn back_up_settings(file: &telescope_core::config::ConfigFile) -> Result<Option<PathBuf>, UpdateError> {
    file.copy_aside(BACKUP_LABEL).map_err(|e| {
        log::error!("Couldn't copy the settings aside: {e}");
        UpdateError(tr("Couldn't save a copy of your settings, so nothing was changed."))
    })
}

// ── Around the swap ───────────────────────────────────────────────────────────

/// Remembers the update settings this copy had, in the settings folder (Python's loader leaves a file it doesn't know alone), and the build
/// that wrote it. Written before the swap, together with turning updates off for 4.4.2: if 4.4.2 never starts and its guard puts this version
/// back, or the swap never happened, settle() gives them back.
pub fn remember(settings_folder: &Path, channel: &str, auto_check: bool, backup: Option<&Path>, this_build: i64) -> io::Result<()> {
    let text = json!({"channel": channel, "auto_check": auto_check, "backup": backup.map(|b| b.display().to_string()), "build": this_build}).to_string();
    std::fs::write(settings_folder.join(MARKER), text)
}

/// The swap failed and this version stays: the note has nothing left to do.
pub fn forget(settings_folder: &Path) {
    let _ = std::fs::remove_file(settings_folder.join(MARKER));
}

/// At start: Some((channel, auto_check, python_failed)) when a going back didn't last, which is the settings to have again. Either 4.4.2
/// didn't start (its guard left the rolled-back marker for that build: python_failed), or the swap never happened (this is still the build that
/// wrote the note, say after a crash while downloading). A leftover note from one that worked and was then updated away from is dropped.
pub fn settle(settings_folder: &Path, directory: &Path, this_build: i64) -> Option<(String, bool, bool)> {
    let text = std::fs::read_to_string(settings_folder.join(MARKER)).ok()?;
    let _ = std::fs::remove_file(settings_folder.join(MARKER));
    let python_failed = update_guard::failed_build(directory) == Some(BUILD);
    if python_failed {
        // This copy is back in place: the marker has done its job, and a later try must not find it
        let _ = std::fs::remove_file(directory.join(update_guard::FAILED));
    }
    let note: Value = serde_json::from_str(&text).ok()?;
    if !python_failed && note["build"].as_i64() != Some(this_build) {
        return None;
    }
    Some((note["channel"].as_str().filter(|c| updates::CHANNELS.contains(c))?.to_string(), note["auto_check"].as_bool().unwrap_or(true), python_failed))
}

#[cfg(test)]
#[path = "going_back_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "going_back_python_tests.rs"]
mod python_tests;
