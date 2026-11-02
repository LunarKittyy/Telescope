//! The settings file, read and saved exactly as the Python app's config.py did, so updating keeps everyone's setup.

use crate::json::{self, Style};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub const CONFIG_VERSION: i64 = 3;
pub const CONFIG_FILENAME: &str = "telescope_config.json";
const APP_NAME: &str = "Telescope";

/// The dev profile's environment variable (dev_profile.ENV): its folder holds this copy's config and log.
pub const DEV_PROFILE_ENV: &str = "TELESCOPE_DEV_PROFILE";

/// The dev profile's folder, when one is set (dev_profile.folder()).
pub fn dev_profile() -> Option<PathBuf> {
    std::env::var_os(DEV_PROFILE_ENV).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

pub fn home() -> PathBuf {
    std::env::home_dir().unwrap_or_default()
}

/// Where config_path looks, so the Linux and the Windows rules can both be checked from either system.
pub struct PathEnv {
    pub windows: bool,
    pub dev_profile: Option<PathBuf>,
    pub appdata: Option<String>,
    pub xdg_config_home: Option<String>,
    pub home: PathBuf,
}

impl PathEnv {
    pub fn current() -> PathEnv {
        PathEnv {
            windows: cfg!(windows),
            dev_profile: dev_profile(),
            appdata: env_nonempty("APPDATA"),
            xdg_config_home: env_nonempty("XDG_CONFIG_HOME"),
            home: home(),
        }
    }

    /// The real app's settings folder, which a dev profile does not change: %APPDATA%\Telescope or $XDG_CONFIG_HOME/telescope.
    pub fn app_folder(&self) -> PathBuf {
        if self.windows {
            return self.appdata.clone().map(PathBuf::from).unwrap_or_else(|| self.home.clone()).join(APP_NAME);
        }
        self.xdg_config_home.clone().map(PathBuf::from).unwrap_or_else(|| self.home.join(".config")).join(APP_NAME.to_lowercase())
    }

    /// config_path: the dev profile's file, or the real app's.
    pub fn config_path(&self) -> PathBuf {
        self.dev_profile.clone().unwrap_or_else(|| self.app_folder()).join(CONFIG_FILENAME)
    }

    /// Where this app keeps its own settings while the rewrite is unfinished: next to the real folder, called telescope-rs.
    pub fn own_profile_folder(&self) -> PathBuf {
        self.app_folder().with_file_name("telescope-rs")
    }
}

/// Where the settings live (config_path): the dev profile's folder, %APPDATA%\Telescope, or $XDG_CONFIG_HOME/telescope.
pub fn config_path() -> PathBuf {
    PathEnv::current().config_path()
}

/// What a fresh start saves (_empty).
pub fn empty() -> Value {
    json!({"version": CONFIG_VERSION, "selected_device": null, "plugin_configs": {}, "devices": {}})
}

/// One settings file. ConfigFile::default() is the app's own; tests and config-snapshot point one elsewhere.
#[derive(Clone, Debug)]
pub struct ConfigFile {
    path: PathBuf,
    /// The timestamp in a set-aside copy's name.
    clock: fn() -> String,
}

fn local_stamp() -> String {
    chrono::Local::now().format("%Y%m%dT%H%M%S").to_string()
}

impl Default for ConfigFile {
    fn default() -> Self {
        ConfigFile::at(config_path())
    }
}

/// Per file: the last unreadable contents backed up (one copy is enough) and where the copy went, until it's asked for.
#[derive(Default)]
struct SetAside {
    last: Option<Vec<u8>>,
    notice: Option<String>,
}

static SET_ASIDE: Mutex<Option<HashMap<PathBuf, SetAside>>> = Mutex::new(None);

fn with_set_aside<T>(path: &Path, f: impl FnOnce(&mut SetAside) -> T) -> T {
    let mut all = SET_ASIDE.lock().unwrap_or_else(|e| e.into_inner());
    f(all.get_or_insert_with(HashMap::new).entry(path.to_path_buf()).or_default())
}

impl ConfigFile {
    pub fn at(path: impl Into<PathBuf>) -> ConfigFile {
        ConfigFile { path: path.into(), clock: local_stamp }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// load_config(): the settings, or a fresh start when the file is missing, unreadable or unusable (set aside first).
    pub fn load(&self) -> Value {
        self.read(false).unwrap_or_else(|_| empty())
    }

    /// load_config(strict=True): an error when the file is there but can't be read, for callers about to save over it.
    pub fn load_strict(&self) -> io::Result<Value> {
        self.read(true)
    }

    fn read(&self, strict: bool) -> io::Result<Value> {
        self.read_with(strict, &|path| fs::read(path), &std::thread::sleep)
    }

    // The file read and the wait between tries are passed in so a test can make the file briefly locked
    fn read_with(&self, strict: bool, read: &dyn Fn(&Path) -> io::Result<Vec<u8>>, sleep: &dyn Fn(Duration)) -> io::Result<Value> {
        let mut attempt = 0;
        let data = loop {
            match read(&self.path) {
                Ok(data) => break data,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(empty()),
                // Often brief (a virus scanner or sync app holding the file); starting fresh would save over it later
                Err(e) if attempt == 2 => {
                    if strict {
                        return Err(e);
                    }
                    log::error!("Failed to read config from {} - starting fresh: {e}", self.path.display());
                    return Ok(empty());
                }
                Err(_) => {
                    attempt += 1;
                    sleep(Duration::from_millis(200));
                }
            }
        };
        // Notepad saves a byte order mark
        let text = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&data);
        let parsed = std::str::from_utf8(text).map_err(|e| e.to_string()).and_then(|t| json::loads(t).map_err(|e| e.to_string()));
        let raw = match parsed {
            Ok(raw) => raw,
            Err(e) => {
                log::error!("Config at {} is not valid JSON - backing up and starting fresh: {e}", self.path.display());
                self.back_up_invalid(&data);
                return Ok(empty());
            }
        };
        let raw = upgrade_from_v2(raw);
        if !is_whole_config_valid(&raw) {
            log::warn!("Config at {} is missing, malformed, or an unsupported older version - backing up and starting fresh",
                       self.path.display());
            self.back_up_invalid(&data);
            return Ok(empty());
        }
        let Value::Object(map) = raw else { unreachable!() };
        Ok(Value::Object(validate_sections(map)))
    }

    /// save_config(): writes cfg atomically (version set to the current one), only this user able to read it.
    pub fn save(&self, cfg: &mut Value) -> bool {
        match self.try_save(cfg) {
            Ok(()) => true,
            Err(e) => {
                log::error!("Failed to save config to {}: {e}", self.path.display());
                false
            }
        }
    }

    /// save, with what went wrong for a caller that shows it.
    pub fn try_save(&self, cfg: &mut Value) -> io::Result<()> {
        if let Value::Object(map) = cfg {
            map.insert("version".into(), CONFIG_VERSION.into());
        }
        let text = json::dumps(cfg, Style::CONFIG);
        // Python writes the file in text mode, which turns each newline into the platform's
        let text = if cfg!(windows) { text.replace('\n', "\r\n") } else { text };
        self.write_atomically(text.as_bytes())
    }

    fn write_atomically(&self, bytes: &[u8]) -> io::Result<()> {
        self.write_with(bytes, &set_mode)
    }

    // chmod is passed in so a test can stand in for a filesystem without Unix permissions
    fn write_with(&self, bytes: &[u8], chmod: Chmod) -> io::Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        private_dir(dir, chmod)?;
        let mut tmp_name = self.path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".tmp");
        let tmp = self.path.with_file_name(tmp_name);
        let mut f = open_private(&tmp, chmod)?;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, &self.path)
    }

    /// Reads the file again, changes it and saves it, as every save in the window does; false when it couldn't be read or written.
    pub fn update(&self, change: impl FnOnce(&mut Value)) -> bool {
        match self.load_strict() {
            Ok(mut cfg) => {
                change(&mut cfg);
                self.save(&mut cfg)
            }
            Err(e) => {
                log::error!("Couldn't read the settings file; not saving over it: {e}");
                false
            }
        }
    }

    /// take_reset_notice(): once, after the file was set aside, where the copy went ("" when it couldn't be kept).
    pub fn take_reset_notice(&self) -> Option<String> {
        with_set_aside(&self.path, |s| s.notice.take())
    }

    fn back_up_invalid(&self, original: &[u8]) {
        let fresh = with_set_aside(&self.path, |s| {
            // Every load before the first save sees the same file; one copy is enough
            if s.last.as_deref() == Some(original) {
                return false;
            }
            s.last = Some(original.to_vec());
            s.notice = Some(String::new());
            true
        });
        if !fresh {
            return;
        }
        let stamp = (self.clock)();
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".invalid-{stamp}"));
        let backup = self.path.with_file_name(name);
        match open_private(&backup, &set_mode).and_then(|mut f| f.write_all(original)) {
            Ok(()) => {
                log::info!("Backed up invalid config to {}", backup.display());
                with_set_aside(&self.path, |s| s.notice = Some(backup.display().to_string()));
            }
            Err(e) => log::error!("Failed to back up invalid config to {}: {e}", backup.display()),
        }
    }
}

/// Sets a file or folder's Unix permission bits.
type Chmod<'a> = &'a dyn Fn(&Path, u32) -> io::Result<()>;

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

// The config holds pairing tokens: only this user may read it (nothing to do on Windows, where %APPDATA% is per-user)
fn private_dir(dir: &Path, chmod: Chmod) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        tighten(chmod(dir, 0o700), dir);
    }
    #[cfg(not(unix))]
    {
        let _ = chmod;
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

// 0600 from creation, and again for a file left from before, since the mode only applies to new files
fn open_private(path: &Path, chmod: Chmod) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        let f = options.open(path)?;
        tighten(chmod(path, 0o600), path);
        Ok(f)
    }
    #[cfg(not(unix))]
    {
        let _ = chmod;
        options.open(path)
    }
}

// A filesystem without Unix permissions (a FAT drive, some sync folders) shouldn't stop the config saving
#[cfg(unix)]
fn tighten(result: io::Result<()>, path: &Path) {
    if result.is_err() {
        log::warn!("Couldn't restrict permissions on {}", path.display());
    }
}

/// Whether a saved value counts as a Python int (bools don't, floats don't).
fn is_int(v: &Value) -> bool {
    v.is_i64() || v.is_u64()
}

// LEGACY MIGRATION PATH, as in config.py: configs from v2.3 and earlier lose their pairings and per-phone settings
fn upgrade_from_v2(cfg: Value) -> Value {
    let Value::Object(mut map) = cfg else { return cfg };
    // Python's == makes 2.0 count as 2 too
    if map.get("version").and_then(Value::as_f64) != Some(2.0) {
        return Value::Object(map);
    }
    if let Some(Value::Object(pcfg)) = map.get_mut("plugin_configs") {
        pcfg.shift_remove("connection");
    }
    map.insert("devices".into(), json!({}));
    map.insert("selected_device".into(), Value::Null);
    map.insert("version".into(), CONFIG_VERSION.into());
    log::info!("Migrated config from version 2; phones need pairing again");
    Value::Object(map)
}

/// _migrate: a loaded value upgraded from version 2 and checked, or a fresh start when it isn't a usable config.
pub fn migrate(cfg: Value) -> Value {
    let cfg = upgrade_from_v2(cfg);
    if !is_whole_config_valid(&cfg) {
        return empty();
    }
    let Value::Object(map) = cfg else { unreachable!() };
    Value::Object(validate_sections(map))
}

fn is_whole_config_valid(cfg: &Value) -> bool {
    let Value::Object(map) = cfg else { return false };
    match map.get("version") {
        None => false,
        Some(v) => is_int(v) && v.as_i64().is_none_or(|n| n >= CONFIG_VERSION),
    }
}

fn valid_device_entry(v: &Value) -> bool {
    let Value::Object(entry) = v else { return false };
    if entry.get("active_ip").is_some_and(|ip| !ip.is_null() && !ip.is_string()) {
        return false;
    }
    !entry.get("plugin_configs").is_some_and(|p| !p.is_object())
}

// Each section on its own: a bad one resets to its default and the rest stays
fn validate_sections(mut cfg: Map<String, Value>) -> Map<String, Value> {
    if !cfg.get("plugin_configs").is_some_and(Value::is_object) {
        if cfg.contains_key("plugin_configs") {
            log::warn!("Config 'plugin_configs' section is malformed - resetting to defaults");
        }
        cfg.insert("plugin_configs".into(), json!({}));
    }
    let devices_ok = match cfg.get("devices") {
        Some(Value::Object(devices)) => devices.values().all(valid_device_entry),
        _ => false,
    };
    if !devices_ok {
        if cfg.contains_key("devices") {
            log::warn!("Config 'devices' section is malformed - resetting to defaults");
        }
        cfg.insert("devices".into(), json!({}));
    }
    if let Some(collapsed) = cfg.get("collapsed") {
        let ok = collapsed.as_array().is_some_and(|keys| keys.iter().all(Value::is_string));
        if !ok {
            log::warn!("Config 'collapsed' section is malformed - resetting to defaults");
            cfg.insert("collapsed".into(), json!([]));
        }
    }
    match cfg.get("selected_device") {
        None => {
            cfg.insert("selected_device".into(), Value::Null);
        }
        Some(Value::Null | Value::String(_)) => {}
        Some(_) => {
            log::warn!("Config 'selected_device' is malformed - resetting to default");
            cfg.insert("selected_device".into(), Value::Null);
        }
    }
    cfg
}

/// clamped_int(): a saved whole number kept within lo..=hi, or default when it isn't one (hand-edited or damaged).
pub fn clamped_int(value: &Value, default: i64, lo: i64, hi: i64) -> i64 {
    python_int(value).map_or(default, |n| n.clamp(lo, hi))
}

// Python's int(value) for what JSON holds: bools are 0 and 1, floats are cut toward zero, strings are read as digits
fn python_int(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n.as_i64().or_else(|| n.as_u64().map(|_| i64::MAX)).or_else(|| {
            let f = n.as_f64()?;
            f.is_finite().then(|| f.trunc().clamp(i64::MIN as f64, i64::MAX as f64) as i64)
        }),
        Value::String(s) => {
            let t = s.trim();
            let (neg, digits) = match t.as_bytes().first() {
                Some(b'-') => (true, &t[1..]),
                Some(b'+') => (false, &t[1..]),
                _ => (false, t),
            };
            let ok = !digits.is_empty() && !digits.starts_with('_') && !digits.ends_with('_') && !digits.contains("__")
                && digits.chars().all(|c| c.is_ascii_digit() || c == '_');
            if !ok {
                return None;
            }
            let n: i64 = digits.replace('_', "").parse().unwrap_or(i64::MAX);
            Some(if neg { -n } else { n })
        }
        _ => None,
    }
}

/// What the window keeps outside the plugins' own sections, as TelescopeWindow.save_now() writes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowState {
    pub selected_device: Option<String>,
    /// The folded cards and sections, saved sorted.
    pub collapsed: Vec<String>,
    /// Which virtual camera each source streamed to last (the stream flow's slot_memory).
    pub output_slots: Map<String, Value>,
}

impl WindowState {
    /// What the window takes from loaded settings at start (TelescopeWindow._apply_config).
    pub fn from_config(cfg: &Value) -> WindowState {
        let mut collapsed: Vec<String> = cfg.get("collapsed").and_then(Value::as_array)
            .map(|keys| keys.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        collapsed.sort();
        collapsed.dedup();
        // Python's isinstance(v, int) lets True and False through as well
        let output_slots = cfg.get("output_slots").and_then(Value::as_object)
            .map(|slots| slots.iter().filter(|(_, v)| is_int(v) || v.is_boolean()).map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        WindowState {
            selected_device: cfg.get("selected_device").and_then(Value::as_str).map(str::to_string),
            collapsed,
            output_slots,
        }
    }
}

/// One plugin's settings to save: the whole app's copy, or a device's.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginSection {
    pub plugin: String,
    /// None for the app-wide copy, else the source id it belongs to.
    pub device: Option<String>,
    pub config: Value,
}

/// save_now(): the freshly loaded cfg updated with the window's state and the plugins' sections, ready for save().
pub fn merge_for_save(cfg: &mut Value, window: &WindowState, sections: &[PluginSection]) {
    let Value::Object(map) = cfg else { return };
    if !map.get("plugin_configs").is_some_and(Value::is_object) {
        map.insert("plugin_configs".into(), json!({}));
    }
    map.insert("selected_device".into(), window.selected_device.clone().map_or(Value::Null, Value::String));
    map.insert("collapsed".into(), window.collapsed.iter().cloned().map(Value::String).collect());
    for s in sections.iter().filter(|s| s.device.is_none()) {
        map["plugin_configs"][&s.plugin] = s.config.clone();
    }
    for s in sections {
        if let Some(device) = &s.device {
            let devices = map.entry("devices").or_insert_with(|| json!({}));
            let entry = devices.as_object_mut().map(|d| d.entry(device.clone()).or_insert_with(|| json!({})));
            if let Some(Value::Object(entry)) = entry {
                let pcfg = entry.entry("plugin_configs").or_insert_with(|| json!({}));
                if let Value::Object(pcfg) = pcfg {
                    pcfg.insert(s.plugin.clone(), s.config.clone());
                }
            }
        }
    }
    if !window.output_slots.is_empty() {
        map.insert("output_slots".into(), Value::Object(window.output_slots.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch_dir;

    fn file_with(text: &[u8]) -> (PathBuf, ConfigFile) {
        let dir = scratch_dir("config");
        let path = dir.join(CONFIG_FILENAME);
        fs::write(&path, text).unwrap();
        (dir, ConfigFile::at(path))
    }

    fn backups(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir).unwrap().flatten().map(|e| e.path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().contains(".invalid-")).collect()
    }

    fn env(windows: bool) -> PathEnv {
        PathEnv {
            windows,
            dev_profile: None,
            appdata: None,
            xdg_config_home: None,
            home: PathBuf::from(if windows { "C:/Users/luna" } else { "/home/luna" }),
        }
    }

    #[test]
    fn the_real_config_path_is_where_config_py_puts_it() {
        let mut linux = env(false);
        assert_eq!(linux.config_path(), PathBuf::from("/home/luna/.config/telescope/telescope_config.json"));
        linux.xdg_config_home = Some("/data/cfg".into());
        assert_eq!(linux.config_path(), PathBuf::from("/data/cfg/telescope/telescope_config.json"));
        let mut windows = env(true);
        // Without %APPDATA% config.py falls back to the home folder itself
        assert_eq!(windows.config_path(), PathBuf::from("C:/Users/luna/Telescope/telescope_config.json"));
        windows.appdata = Some("C:/Users/luna/AppData/Roaming".into());
        assert_eq!(windows.config_path(), PathBuf::from("C:/Users/luna/AppData/Roaming/Telescope/telescope_config.json"));
        // $XDG_CONFIG_HOME means nothing on Windows
        windows.xdg_config_home = Some("/data/cfg".into());
        assert_eq!(windows.config_path(), PathBuf::from("C:/Users/luna/AppData/Roaming/Telescope/telescope_config.json"));
    }

    #[test]
    fn a_dev_profile_wins_on_both_systems() {
        for windows in [false, true] {
            let mut e = env(windows);
            e.dev_profile = Some(PathBuf::from("/tmp/dev"));
            assert_eq!(e.config_path(), PathBuf::from("/tmp/dev/telescope_config.json"));
        }
    }

    #[test]
    fn the_own_profile_sits_beside_the_real_folder() {
        let mut linux = env(false);
        assert_eq!(linux.own_profile_folder(), PathBuf::from("/home/luna/.config/telescope-rs"));
        linux.xdg_config_home = Some("/data/cfg".into());
        assert_eq!(linux.own_profile_folder(), PathBuf::from("/data/cfg/telescope-rs"));
        let mut windows = env(true);
        windows.appdata = Some("C:/Users/luna/AppData/Roaming".into());
        assert_eq!(windows.own_profile_folder(), PathBuf::from("C:/Users/luna/AppData/Roaming/telescope-rs"));
    }

    #[test]
    fn missing_file_is_a_fresh_start() {
        let dir = scratch_dir("config");
        let cfg = ConfigFile::at(dir.join(CONFIG_FILENAME)).load();
        assert_eq!(cfg, empty());
        assert_eq!(json::dumps(&cfg, Style::CONFIG),
                   "{\n  \"version\": 3,\n  \"selected_device\": null,\n  \"plugin_configs\": {},\n  \"devices\": {}\n}");
    }

    #[test]
    fn byte_order_mark_is_accepted() {
        let (dir, f) = file_with(b"\xef\xbb\xbf{\"version\": 3, \"plugin_configs\": {\"a\": {}}}");
        assert_eq!(f.load()["plugin_configs"], json!({"a": {}}));
        assert!(backups(&dir).is_empty());
    }

    #[test]
    fn broken_file_is_set_aside_once() {
        let (dir, f) = file_with(b"{\"version\": 3,");
        assert_eq!(f.load(), empty());
        assert_eq!(f.load(), empty());
        let copies = backups(&dir);
        assert_eq!(copies.len(), 1);
        assert_eq!(fs::read(&copies[0]).unwrap(), b"{\"version\": 3,");
        assert_eq!(f.take_reset_notice(), Some(copies[0].display().to_string()));
        assert_eq!(f.take_reset_notice(), None);
    }

    #[test]
    fn old_versions_and_wrong_shapes_are_set_aside() {
        for text in ["{\"version\": 1}", "[]", "{}", "{\"version\": true}", "{\"version\": 3.0}", "\"x\""] {
            let (dir, f) = file_with(text.as_bytes());
            assert_eq!(f.load(), empty(), "{text}");
            assert_eq!(backups(&dir).len(), 1, "{text}");
        }
    }

    #[test]
    fn version_2_keeps_global_settings_only() {
        let (_, f) = file_with(br#"{"version": 2.0, "selected_device": "Pixel", "plugin_configs": {"connection": {}, "setup": {"x": 1}}, "devices": {"Pixel": {}}}"#);
        assert_eq!(f.load(), json!({"version": 3, "selected_device": null, "plugin_configs": {"setup": {"x": 1}}, "devices": {}}));
    }

    #[test]
    fn bad_sections_reset_on_their_own() {
        let (_, f) = file_with(br#"{"version": 4, "selected_device": 5, "plugin_configs": [], "devices": {"a": {"active_ip": 3}}, "collapsed": "x", "new": {"keep": 1}}"#);
        let cfg = f.load();
        assert_eq!(cfg, json!({"version": 4, "selected_device": null, "plugin_configs": {}, "devices": {}, "collapsed": [], "new": {"keep": 1}}));
        let keys: Vec<&String> = cfg.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["version", "selected_device", "plugin_configs", "devices", "collapsed", "new"]);
    }

    #[test]
    fn save_is_private_and_atomic() {
        let dir = scratch_dir("config").join("nested");
        let f = ConfigFile::at(dir.join(CONFIG_FILENAME));
        let mut cfg = json!({"selected_device": null, "version": 1});
        assert!(f.save(&mut cfg));
        let want = "{\n  \"selected_device\": null,\n  \"version\": 3\n}";
        let want = if cfg!(windows) { want.replace('\n', "\r\n") } else { want.to_string() };
        assert_eq!(fs::read_to_string(f.path()).unwrap(), want);
        assert!(!dir.join("telescope_config.json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(f.path()).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
    }

    #[test]
    fn clamped_int_reads_like_python_int() {
        assert_eq!(clamped_int(&json!(true), 5, 0, 10), 1);
        assert_eq!(clamped_int(&json!(7.9), 5, 0, 10), 7);
        assert_eq!(clamped_int(&json!(" 1_2 "), 5, 0, 100), 12);
        assert_eq!(clamped_int(&json!("1.0"), 5, 0, 10), 5);
        assert_eq!(clamped_int(&json!(null), 5, 0, 10), 5);
        assert_eq!(clamped_int(&json!(-40), 5, 0, 10), 0);
    }

    #[test]
    fn window_state_round_trip() {
        let cfg = json!({"collapsed": ["b", "a", "a"], "output_slots": {"x": 1, "y": "2", "z": true}, "selected_device": "p"});
        let w = WindowState::from_config(&cfg);
        assert_eq!(w.collapsed, ["a", "b"]);
        assert_eq!(w.output_slots, json!({"x": 1, "z": true}).as_object().unwrap().clone());
        let mut out = empty();
        merge_for_save(&mut out, &w, &[PluginSection { plugin: "t".into(), device: Some("p".into()), config: json!({"k": 1}) }]);
        assert_eq!(out["devices"], json!({"p": {"plugin_configs": {"t": {"k": 1}}}}));
        assert_eq!(out["output_slots"], json!({"x": 1, "z": true}));
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod python_tests;
