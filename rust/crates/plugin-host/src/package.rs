//! Plugins that don't come with Telescope: finding them, installing them from a .zip, switching them on and removing them (plugin_loader.py).
//!
//! Each lives in a folder of its own under the plugins folder: a plugin.json describing it, a main.lua and whatever else it ships (a card.slint,
//! shaders, locale/). Nothing in a folder runs until the user switches that plugin on, and switching it on remembers a fingerprint of its
//! files: a plugin that changes afterwards stays off until it's switched on again.

use crate::manifest::{self, Manifest, MANIFEST, OLDEST_PLUGIN_API, PLUGIN_API, TEXT_MAX};
use crate::switches::Switches;
use crate::text::tr;
use ring::digest::{Context, SHA256};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

pub const MAX_FILES: usize = 500;
/// A plugin folder, or what a .zip unpacks to.
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;
/// TELESCOPE_DEV_PLUGINS: folders `dev_desktop --plugin` loads straight from where they're written, only in a dev profile.
pub const DEV_PLUGINS_ENV: &str = "TELESCOPE_DEV_PLUGINS";

const SHARED: &str = "Other users on this computer could change it, so it isn't loaded.";
const LINK: &str = "It contains a link to somewhere else, which plugins can't have.";
const BAD_NAME: &str = "The folder has to have the same name as the plugin, using only lowercase letters, digits and _ (starting with a letter).";
const TOO_BIG: &str = "It's too big for a plugin (over 500 files or 20 MB).";
const NOT_A_ZIP: &str = "It isn't a .zip file Telescope can open.";
const NOT_IN_FOLDER: &str = "That isn't a folder in the plugins folder.";
const TAKEN: &str = "A plugin that comes with Telescope already has that name.";
/// Files that are compiled code: never in a plugin people can read before they switch it on.
const COMPILED: &[&str] = &[".pyc", ".pyo", ".so", ".pyd", ".dll", ".dylib", ".luac", ".exe"];

/// Where plugins are installed: a "plugins" folder next to the settings.
pub fn plugins_dir() -> PathBuf {
    telescope_core::config::config_path().parent().map(|p| p.join("plugins")).unwrap_or_else(|| PathBuf::from("plugins"))
}

/// Plugins being written, loaded from their own folders (dev_profile.plugin_folders()).
pub fn dev_plugin_folders() -> Vec<PathBuf> {
    if telescope_core::config::dev_profile().is_none() {
        return Vec::new();
    }
    std::env::var_os(DEV_PLUGINS_ENV).map(|raw| std::env::split_paths(&raw).filter(|p| !p.as_os_str().is_empty()).collect())
        .unwrap_or_default()
}

/// A relative path that stays inside the folder it's relative to: no .., no root, no drive.
pub fn safe_relative(path: &str) -> bool {
    !path.is_empty() && !path.contains('\\') && !path.contains(':')
        && Path::new(path).components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// A plugin folder and what its plugin.json says; problem is why it can't be loaded ("" if it can).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Installed {
    pub folder: PathBuf,
    pub name: String,
    pub title: String,
    pub version: String,
    pub author: String,
    pub description: String,
    /// Its source's https address, "" when it gives none or one that isn't valid.
    pub repo: String,
    /// The Plugins tab group it asks for, an id from PLUGIN_CATEGORIES ("" for Extras).
    pub category: String,
    pub api: i64,
    /// The script it starts from, relative to folder.
    pub main: String,
    pub permissions: Vec<String>,
    pub manifest: Manifest,
    pub digest: String,
    pub problem: String,
    pub enabled: bool,
    /// Switched on, but its files differ from when it was.
    pub changed: bool,
    /// Loaded from where it's written (TELESCOPE_DEV_PLUGINS), always on.
    pub dev: bool,
    /// What went wrong loading it this run.
    pub error: String,
}

impl Installed {
    pub fn info(&self) -> telescope_core::PluginInfo {
        self.manifest.info(&self.name)
    }
}

/// Why a .zip couldn't be installed.
#[derive(Debug)]
pub enum InstallError {
    /// What's wrong with it, worded for the person installing it.
    Invalid(String),
    /// A plugin of that name is already there and replace wasn't asked for.
    Exists(String),
    Io(io::Error),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Invalid(why) => f.write_str(why),
            InstallError::Exists(name) => write!(f, "{name} is already installed"),
            InstallError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<io::Error> for InstallError {
    fn from(e: io::Error) -> Self {
        InstallError::Io(e)
    }
}

fn invalid(text: &str) -> InstallError {
    InstallError::Invalid(tr(text, &[]))
}

type Rename = Box<dyn Fn(&Path, &Path) -> io::Result<()> + Send + Sync>;

/// The plugins folder with the settings its switches are kept in and the names the built-in plugins already take.
pub struct PluginFolder {
    pub folder: PathBuf,
    pub switches: Switches,
    pub reserved: BTreeSet<String>,
    pub dev_folders: Vec<PathBuf>,
    rename: Rename,
}

impl PluginFolder {
    /// The app's own: plugins_dir(), the settings file and any plugins being written.
    pub fn app(reserved: impl IntoIterator<Item = String>) -> PluginFolder {
        PluginFolder::new(plugins_dir(), Switches::default(), reserved).with_dev_folders(dev_plugin_folders())
    }

    pub fn new(folder: impl Into<PathBuf>, switches: Switches, reserved: impl IntoIterator<Item = String>) -> PluginFolder {
        PluginFolder { folder: folder.into(), switches, reserved: reserved.into_iter().collect(), dev_folders: Vec::new(), rename: Box::new(|a, b| fs::rename(a, b)) }
    }

    pub fn with_dev_folders(mut self, folders: Vec<PathBuf>) -> PluginFolder {
        self.dev_folders = folders;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_rename(mut self, rename: impl Fn(&Path, &Path) -> io::Result<()> + Send + Sync + 'static) -> PluginFolder {
        self.rename = Box::new(rename);
        self
    }

    /// Every plugin folder, by name, with whether it's on and why it can't load. A plugin being written stands in for an installed copy.
    pub fn find_installed(&self) -> Vec<Installed> {
        let dev: Vec<Installed> = self.dev_folders.iter().map(|p| self.inspect(p, &Default::default(), false, true)).collect();
        let Ok(entries) = fs::read_dir(&self.folder) else { return dev };
        let enabled = self.switches.added();
        let shared = unsafe_permissions(&self.folder);
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path())
            .filter(|p| p.is_dir() && !p.file_name().unwrap_or_default().to_string_lossy().starts_with('.')).collect();
        paths.sort();
        let mut found: Vec<Installed> = paths.iter()
            .filter(|p| !dev.iter().any(|d| d.name == file_name(p)))
            .map(|p| self.inspect(p, &enabled, shared, false)).collect();
        found.extend(dev);
        found
    }

    /// The installed plugin called name, if there is one.
    pub fn find(&self, name: &str) -> Option<Installed> {
        self.find_installed().into_iter().find(|i| i.name == name)
    }

    fn inspect(&self, path: &Path, enabled: &std::collections::BTreeMap<String, String>, shared: bool, dev: bool) -> Installed {
        let name = file_name(path);
        let mut plugin = Installed { folder: path.to_path_buf(), title: name.clone(), name: name.clone(), dev, ..Default::default() };
        let problem = |mut plugin: Installed, text: String| {
            plugin.problem = text;
            plugin
        };
        if is_link(path) {
            return problem(plugin, tr("It's a link to another folder, which plugins can't be.", &[]));
        }
        if !manifest::valid_name(&name) {
            return problem(plugin, tr(BAD_NAME, &[]));
        }
        let raw = match fs::read(path.join(MANIFEST)) {
            Err(_) => return problem(plugin, tr("It has no {file}.", &[("file", &MANIFEST)])),
            Ok(raw) => raw,
        };
        let parsed: Value = match std::str::from_utf8(&raw).ok().and_then(|t| serde_json::from_str(t.trim_start_matches('\u{feff}')).ok()) {
            None => return problem(plugin, tr("Its {file} isn't valid JSON.", &[("file", &MANIFEST)])),
            Some(v) => v,
        };
        if !parsed.is_object() {
            return problem(plugin, tr("Its {file} isn't valid.", &[("file", &MANIFEST)]));
        }
        let m = Manifest::new(parsed);
        plugin.title = Some(manifest::text(m.get("title"))).filter(|t| !t.is_empty()).unwrap_or_else(|| name.clone());
        plugin.version = manifest::text(m.get("version"));
        plugin.author = manifest::text(m.get("author"));
        plugin.description = manifest::text(m.get("description"));
        plugin.category = manifest::text(m.get("category"));
        plugin.repo = if manifest::repo_problem(m.get("repo")).is_empty() { m.get("repo").and_then(Value::as_str).unwrap_or("").to_string() } else { String::new() };
        plugin.api = m.api();
        plugin.permissions = m.permissions();
        plugin.manifest = m.clone();
        match digest(path, dev) {
            Ok(d) => plugin.digest = d,
            Err(e) => {
                // It was on: say why it didn't start
                plugin.changed = enabled.contains_key(&name);
                plugin.enabled = dev;
                let why = if e.kind() == io::ErrorKind::InvalidData { e.to_string() } else { tr("Its files couldn't be read.", &[]) };
                return problem(plugin, why);
            }
        }
        plugin.enabled = dev || enabled.get(&name) == Some(&plugin.digest);
        plugin.changed = !dev && enabled.contains_key(&name) && !plugin.enabled;
        if m.get("name").and_then(Value::as_str) != Some(name.as_str()) {
            return problem(plugin, tr("Its {file} has to give the folder's name as \"name\".", &[("file", &MANIFEST)]));
        }
        if self.reserved.contains(&name) {
            return problem(plugin, tr(TAKEN, &[]));
        }
        let api = plugin.api;
        if api > PLUGIN_API {
            return problem(plugin, tr("It needs a newer Telescope (plugin API {wanted}; this one has {have}).", &[("wanted", &api), ("have", &PLUGIN_API)]));
        }
        if api < OLDEST_PLUGIN_API {
            let api: &dyn std::fmt::Display = if api == 0 { &"?" } else { &api };
            return problem(plugin, tr("It was made for an older Telescope (plugin API {api}) and needs an update.", &[("api", api)]));
        }
        let Some(main) = m.main() else {
            return problem(plugin, tr("Its {file} isn't valid.", &[("file", &MANIFEST)]));
        };
        if !path.join(&main).is_file() {
            return problem(plugin, tr("It has no {file}.", &[("file", &main)]));
        }
        plugin.main = main;
        if shared || unsafe_permissions(path) {
            return problem(plugin, tr(SHARED, &[]));
        }
        plugin
    }

    /// Switch an added plugin on (as its files are now) or off, from the next start.
    pub fn set_enabled(&self, plugin: &Installed, on: bool) -> io::Result<()> {
        if on {
            let d = digest(&plugin.folder, false)?;
            self.switches.set_added(&plugin.name, Some(&d))
        } else {
            self.switches.set_added(&plugin.name, None)
        }
    }

    /// Every plugin that should run: (those that are on and can load, those that are on but can't, with error filled in).
    pub fn enabled(&self) -> (Vec<Installed>, Vec<Installed>) {
        let (mut ok, mut failed) = (Vec::new(), Vec::new());
        for mut plugin in self.find_installed() {
            if !plugin.enabled && !plugin.changed {
                continue;
            }
            if plugin.dev && !plugin.problem.is_empty() {
                plugin.error = plugin.problem.clone();
                failed.push(plugin);
            } else if plugin.changed {
                plugin.error = if plugin.problem.is_empty() {
                    tr("Its files changed since it was turned on. Turn it on again if you trust this version.", &[])
                } else {
                    plugin.problem.clone()
                };
                failed.push(plugin);
            } else if !plugin.problem.is_empty() {
                plugin.error = plugin.problem.clone();
                failed.push(plugin);
            } else {
                ok.push(plugin);
            }
        }
        (ok, failed)
    }

    /// Unpack a plugin .zip (one folder, named as the plugin) into the plugins folder, switched off. replace: put it in place of the one
    /// there, switched off; its settings are kept since they're saved by name.
    pub fn install_zip(&self, archive: &Path, replace: bool) -> Result<Installed, InstallError> {
        let file = fs::File::open(archive)?;
        let mut zip = zip::ZipArchive::new(file).map_err(|_| invalid(NOT_A_ZIP))?;
        let name = zip_name(&mut zip)?;
        if self.reserved.contains(&name) {
            return Err(invalid(TAKEN));
        }
        let target = self.folder.join(&name);
        let there = target.exists() || is_link(&target);
        if there && !replace {
            return Err(InstallError::Exists(name));
        }
        if is_link(&target) || (target.exists() && !target.is_dir()) {
            return Err(invalid(NOT_IN_FOLDER));
        }
        make_private_dir(&self.folder)?;
        let staging = make_staging(&self.folder)?;
        let result = self.unpack_and_swap(&mut zip, &staging, &name, &target);
        let _ = fs::remove_dir_all(&staging);
        result?;
        let enabled = self.switches.added();
        Ok(self.inspect(&target, &enabled, unsafe_permissions(&self.folder), false))
    }

    fn unpack_and_swap(&self, zip: &mut zip::ZipArchive<fs::File>, staging: &Path, name: &str, target: &Path) -> Result<(), InstallError> {
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|_| invalid(NOT_A_ZIP))?;
            let entry_name = entry.name().map_err(|_| invalid(NOT_A_ZIP))?.to_string();
            let out = staging.join(entry_parts(&entry_name).iter().collect::<PathBuf>());
            if entry.is_dir() {
                fs::create_dir_all(&out)?;
                continue;
            }
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut data = Vec::with_capacity(entry.size().min(MAX_BYTES) as usize);
            // What it says it unpacks to was checked; a file that unpacks to more than that is cut off there
            (&mut entry).take(MAX_BYTES + 1).read_to_end(&mut data).map_err(|_| invalid(NOT_A_ZIP))?;
            if data.len() as u64 > entry.size() {
                return Err(invalid(TOO_BIG));
            }
            fs::write(&out, data)?;
        }
        set_plain_permissions(&staging.join(name))?;
        if target.exists() {
            let old = staging.join(".old");
            (self.rename)(target, &old)?;
            if let Err(e) = (self.rename)(&staging.join(name), target) {
                // The old one stays, and stays as switched on as it was
                let _ = (self.rename)(&old, target);
                return Err(e.into());
            }
            self.switch_off_replaced(name);
        } else {
            (self.rename)(&staging.join(name), target)?;
        }
        Ok(())
    }

    /// A new version is never on by itself. If the switch can't be saved its files no longer match the fingerprint, which keeps it off anyway.
    fn switch_off_replaced(&self, name: &str) {
        if self.switches.added().contains_key(name) && self.switches.set_added(name, None).is_err() {
            log::warn!("Couldn't switch off the replaced plugin {name}; its new files keep it off");
        }
    }

    /// Delete a plugin's folder (it has to be one directly in the plugins folder) and forget it was on.
    pub fn remove(&self, plugin: &Installed) -> Result<(), InstallError> {
        let target = &plugin.folder;
        let inside = target.canonicalize().ok().and_then(|t| t.parent().map(Path::to_path_buf)) == self.folder.canonicalize().ok();
        if is_link(target) || !inside || file_name(target) != plugin.name {
            return Err(invalid(NOT_IN_FOLDER));
        }
        self.set_enabled(plugin, false)?;
        fs::remove_dir_all(target)?;
        Ok(())
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// The parts of a path inside a .zip the way PurePosixPath reads them: empty and "." parts dropped.
fn entry_parts(name: &str) -> Vec<&str> {
    name.split('/').filter(|p| !p.is_empty() && *p != ".").collect()
}

/// The name of the plugin in a .zip (its one top folder), checked as install_zip checks it.
pub fn zip_plugin_name(archive: &Path) -> Result<String, InstallError> {
    let file = fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| invalid(NOT_A_ZIP))?;
    zip_name(&mut zip)
}

fn zip_name<R: io::Read + io::Seek>(zip: &mut zip::ZipArchive<R>) -> Result<String, InstallError> {
    let mut total = 0u64;
    let mut tops = BTreeSet::new();
    if zip.len() > MAX_FILES {
        return Err(invalid(TOO_BIG));
    }
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).map_err(|_| invalid(NOT_A_ZIP))?;
        total += entry.size();
        let name = entry.name().map_err(|_| invalid(NOT_A_ZIP))?.to_string();
        let parts = entry_parts(&name);
        if parts.is_empty() || name.starts_with(['/', '\\']) || name.contains('\\') || parts.iter().any(|p| *p == ".." || p.contains(':')) {
            return Err(invalid("It holds a file with a path that points outside the plugin."));
        }
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(invalid(LINK));
        }
        tops.insert(parts[0].to_string());
    }
    if total > MAX_BYTES {
        return Err(invalid(TOO_BIG));
    }
    if tops.len() != 1 {
        return Err(invalid("It has to hold one folder, named as the plugin."));
    }
    let name = tops.into_iter().next().unwrap();
    if !manifest::valid_name(&name) {
        return Err(invalid(BAD_NAME));
    }
    Ok(name)
}

/// The plugin's files relative to folder, sorted. An InvalidData error says why it can't be a plugin: a link, compiled code, a file other
/// users could change, or too big. skip_hidden leaves out .git and the like (a plugin being written).
pub fn files(folder: &Path, skip_hidden: bool) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut total = 0u64;
    walk(folder, folder, skip_hidden, &mut found, &mut total)?;
    Ok(found)
}

fn refuse(text: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, tr(text, &[]))
}

fn walk(root: &Path, here: &Path, skip_hidden: bool, found: &mut Vec<PathBuf>, total: &mut u64) -> io::Result<()> {
    if unsafe_permissions(here) {
        return Err(refuse(SHARED));
    }
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(here)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    let (mut dirs, mut names) = (Vec::new(), Vec::new());
    for e in entries {
        let hidden = e.file_name().to_string_lossy().starts_with('.');
        if skip_hidden && hidden {
            continue;
        }
        let kind = e.file_type()?;
        if kind.is_symlink() {
            return Err(refuse(LINK));
        }
        if kind.is_dir() { dirs.push(e.path()) } else { names.push(e.path()) }
    }
    for path in names {
        let lower = file_name(&path).to_lowercase();
        if COMPILED.iter().any(|ext| lower.ends_with(ext)) {
            return Err(refuse("It contains compiled code (like .pyc files). Plugins have to be source code."));
        }
        if unsafe_permissions(&path) {
            return Err(refuse(SHARED));
        }
        *total += fs::metadata(&path)?.len();
        found.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
        if found.len() > MAX_FILES || *total > MAX_BYTES {
            return Err(refuse(TOO_BIG));
        }
    }
    for dir in dirs {
        walk(root, &dir, skip_hidden, found, total)?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fingerprint of every file in folder (names and contents), to notice a plugin that changed. The same as plugin_loader.digest.
pub fn digest(folder: &Path, skip_hidden: bool) -> io::Result<String> {
    let mut all = Context::new(&SHA256);
    for rel in files(folder, skip_hidden)? {
        let posix: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        all.update(posix.join("/").as_bytes());
        all.update(b"\0");
        all.update(ring::digest::digest(&SHA256, &fs::read(folder.join(&rel))?).as_ref());
    }
    Ok(hex(all.finish().as_ref()))
}

/// The sha256 of some bytes as hex, for checking a download.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(ring::digest::digest(&SHA256, data).as_ref())
}

/// Plain text from a plugin file that was shown, capped like manifest text (an error message from a plugin).
pub fn capped(text: &str) -> String {
    text.chars().take(TEXT_MAX).collect()
}

fn make_private_dir(folder: &Path) -> io::Result<()> {
    if folder.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(folder)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn make_staging(folder: &Path) -> io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    for n in 0..100 {
        let path = folder.join(format!(".adding-{}-{stamp}-{n}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("couldn't make a folder to unpack into"))
}

/// Folders 755, files 644: readable by anyone, changeable only by this user.
fn set_plain_permissions(root: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if root.is_dir() { 0o755 } else { 0o644 };
        fs::set_permissions(root, fs::Permissions::from_mode(mode))?;
        if root.is_dir() {
            for e in fs::read_dir(root)? {
                set_plain_permissions(&e?.path())?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Stands in for private_group() in tests: Some(answer) instead of asking the system.
    pub(crate) static PRIVATE_GROUP: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// On Linux, whether another user could change path (so it could run as this one).
#[cfg(unix)]
pub fn unsafe_permissions(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(st) = fs::metadata(path) else { return false };
    // SAFETY: getuid has no preconditions
    if st.uid() != unsafe { libc::getuid() } || st.mode() & 0o002 != 0 {
        return true;
    }
    st.mode() & 0o020 != 0 && !private_group(st.gid())
}

#[cfg(not(unix))]
pub fn unsafe_permissions(_path: &Path) -> bool {
    false
}

/// Whether gid is this user's own group with nobody else in it (Fedora's umask 002 makes folders group-writable).
#[cfg(unix)]
fn private_group(gid: u32) -> bool {
    #[cfg(test)]
    if let Some(answer) = PRIVATE_GROUP.with(|p| p.get()) {
        return answer;
    }
    unix_users::private_group(gid)
}

#[cfg(unix)]
mod unix_users {
    use std::ffi::CStr;
    use std::sync::Mutex;

    /// getpwent walks one list per process.
    static PASSWD: Mutex<()> = Mutex::new(());

    pub fn private_group(gid: u32) -> bool {
        let _guard = PASSWD.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the libc user and group calls below return pointers into static storage, read before the next call while PASSWD is held
        unsafe {
            let me = libc::getuid();
            let pw = libc::getpwuid(me);
            let group = libc::getgrgid(gid);
            if pw.is_null() || group.is_null() {
                return false;
            }
            let my_name = CStr::from_ptr((*pw).pw_name).to_owned();
            if CStr::from_ptr((*group).gr_name) != my_name.as_c_str() || !(*(*group).gr_mem).is_null() {
                return false;
            }
            libc::setpwent();
            let mut alone = true;
            loop {
                let user = libc::getpwent();
                if user.is_null() {
                    break;
                }
                if (*user).pw_gid == gid && (*user).pw_uid != me {
                    alone = false;
                    break;
                }
            }
            libc::endpwent();
            alone
        }
    }
}
