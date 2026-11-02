//! Going back to the Python version: one action, pinned to the last Python stable release (4.4.2), for anyone who wants that back.
//!
//! Nothing about the target comes from the network except the file itself. The release, its build number and the SHA-256 of each
//! package are written down here, so a changed or swapped release file fails the checksum instead of being trusted. The swap is the
//! normal update's (updates::install_linux and install_windows_if, through update_guard's journal), so a cut-short or failed one puts
//! this version back, and Python's own update_guard.py reads the same journal: if 4.4.2 doesn't start, its next launch rolls back to this.

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

/// What start.sh would find. run(program, code) says whether `program -c code` succeeded. Same order and checks as start.sh: the first
/// interpreter that is new enough is the one it makes the environment with.
pub fn python_state(data_home: &Path, run: &dyn Fn(&str, &str) -> bool) -> PythonState {
    const NEW_ENOUGH: &str = "import sys; sys.exit(0 if sys.version_info >= (3, 12) else 1)";
    const CAN_MAKE_ENVIRONMENT: &str = "import ensurepip, venv";
    let environment = data_home.join("Telescope").join("venv").join("bin").join("python3");
    if environment.is_file() && run(&environment.to_string_lossy(), NEW_ENOUGH) {
        return PythonState::Ready;
    }
    for candidate in ["python3.14", "python3.13", "python3.12", "python3"] {
        if run(candidate, NEW_ENOUGH) {
            return if run(candidate, CAN_MAKE_ENVIRONMENT) { PythonState::NeedsSetup } else { PythonState::Missing };
        }
    }
    PythonState::Missing
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
                text.push(tr("On the first start it sets up Python's libraries again. That needs the internet and takes a few minutes."));
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
            paragraphs = vec![tr("{version} needs Python 3.12 or newer with venv, and this computer doesn't have it. Install it first (python3 and python3-venv on Debian or Ubuntu), then try again. Nothing was changed.").field("version", version())];
            (tr("Go back"), false)
        }
    };
    Dialog { text: paragraphs.join("\n\n"), action, enabled }
}

fn phone_note() -> String {
    tr("The phone app has to match. A newer one may not connect to {version}. Android won't install an older app over a newer one, so uninstall Telescope on the phone, install Telescope.apk from the {version} release and pair again.").field("version", VERSION)
}

// ── Installing ────────────────────────────────────────────────────────────────

/// Puts the unpacked pinned package in place of this copy through update_guard's journal, so a failure midway puts this version back.
/// Windows skips the "does it load" run that updates make, which would start the Python exe as if it were the whole app.
pub fn install_from(archive: &Path, directory: &Path, windows: bool, mv: Mover) -> Result<InstallResult, UpdateError> {
    if windows {
        updates::install_windows_if(archive, directory, BUILD, mv, &|_| true)
    } else {
        updates::install_linux(archive, directory, BUILD, mv)
    }
}

/// A dated copy of the settings next to them, taken before anything changes. None: there is no settings file yet.
pub fn back_up_settings(file: &telescope_core::config::ConfigFile) -> Result<Option<PathBuf>, UpdateError> {
    file.copy_aside(BACKUP_LABEL).map_err(|e| {
        log::error!("Couldn't copy the settings aside: {e}");
        UpdateError(tr("Couldn't save a copy of your settings, so nothing was changed."))
    })
}

// ── After the swap ────────────────────────────────────────────────────────────

/// Remembers the update settings this copy had, in the settings folder (Python's loader leaves a file it doesn't know alone).
/// The swap put 4.4.2 in place with updates off; if 4.4.2 never starts and its guard puts this version back, settle() gives them back.
pub fn remember(settings_folder: &Path, channel: &str, auto_check: bool, backup: Option<&Path>) -> io::Result<()> {
    let text = json!({"channel": channel, "auto_check": auto_check, "backup": backup.map(|b| b.display().to_string())}).to_string();
    std::fs::write(settings_folder.join(MARKER), text)
}

/// At start: Some((channel, auto_check)) when a going back was undone because 4.4.2 didn't start (its guard left the rolled-back marker
/// for that build), which is the settings to have again. A leftover note from one that worked and was then updated away from is dropped.
pub fn settle(settings_folder: &Path, directory: &Path) -> Option<(String, bool)> {
    let text = std::fs::read_to_string(settings_folder.join(MARKER)).ok()?;
    let _ = std::fs::remove_file(settings_folder.join(MARKER));
    if update_guard::failed_build(directory) != Some(BUILD) {
        return None;
    }
    // This copy is back in place: the marker has done its job, and a later try must not find it
    let _ = std::fs::remove_file(directory.join(update_guard::FAILED));
    let note: Value = serde_json::from_str(&text).ok()?;
    Some((note["channel"].as_str().filter(|c| updates::CHANNELS.contains(c))?.to_string(), note["auto_check"].as_bool().unwrap_or(true)))
}

#[cfg(test)]
#[path = "going_back_tests.rs"]
mod tests;
