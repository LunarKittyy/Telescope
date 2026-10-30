//! The desktop log and Copy diagnostics (diagnostics.py): every line is sanitized first, so no tokens, addresses or home paths.

use regex::Regex;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Per file; the one before is kept as telescope.log.1.
pub const MAX_BYTES: u64 = 1_000_000;
/// 200 lines of this still fit in a GitHub issue field.
pub const MAX_LINE: usize = 300;
pub const REPORT_LINES: usize = 200;

fn pattern(cell: &'static OnceLock<Regex>, source: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(source).expect("a fixed pattern"))
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Strip what shouldn't end up in a public issue: URLs, IPs, tokens and the user's home path.
pub fn sanitize(text: &str) -> String {
    sanitize_at(text, std::env::home_dir().as_deref())
}

/// sanitize with the home folder given, so a test can choose it.
pub fn sanitize_at(text: &str, home: Option<&Path>) -> String {
    static URL: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static IPV4: OnceLock<Regex> = OnceLock::new();
    static LONG_SECRET: OnceLock<Regex> = OnceLock::new();
    let mut text = text.to_string();
    if let Some(home) = home.map(|h| h.to_string_lossy().into_owned()).filter(|h| h.chars().count() > 1) {
        text = text.replace(&home, "~");
    }
    let text = pattern(&URL, r"(?i)\b[a-z][a-z0-9+.-]*://\S+").replace_all(&text, "<url>");
    let text = pattern(&BEARER, r#"(?i)(bearer|token|nonce|secret)(["'=: ]+)[^\s"',}]+"#).replace_all(&text, "${1}${2}<redacted>");
    let text = pattern(&IPV4, r"\b\d{1,3}(?:\.\d{1,3}){3}(?::\d+)?\b").replace_all(&text, "<ip>");
    let text = replace_ipv6(&text);
    pattern(&LONG_SECRET, r"\b[A-Za-z0-9_\-+/]{24,}={0,2}").replace_all(&text, "<redacted>").into_owned()
}

/// An IPv6 address standing alone, in full or with "::": only a whole run of hex digits and colons can be one.
fn replace_ipv6(text: &str) -> String {
    let in_run = |c: char| c.is_ascii_hexdigit() || c == ':';
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if !in_run(chars[i].1) {
            out.push(chars[i].1);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && in_run(chars[i].1) {
            i += 1;
        }
        let run: String = chars[start..i].iter().map(|(_, c)| c).collect();
        let alone = start == 0 || !is_word(chars[start - 1].1);
        let ends = i == chars.len() || !is_word(chars[i].1);
        if alone && ends && (run.contains("::") || full_address(&run)) {
            out.push_str("<ip>");
        } else {
            out.push_str(&run);
        }
    }
    out
}

/// Eight groups of one to four hex digits.
fn full_address(run: &str) -> bool {
    let groups: Vec<&str> = run.split(':').collect();
    groups.len() == 8 && groups.iter().all(|g| (1..=4).contains(&g.len()) && g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// "    (repeated N more times)": what a run of the same line becomes.
fn repeated(n: u64) -> String {
    format!("    (repeated {n} more times)")
}

/// str.splitlines(): a line ends at any of these.
fn python_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let (mut start, mut chars) = (0, text.char_indices().peekable());
    while let Some((i, c)) = chars.next() {
        if !matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            continue;
        }
        lines.push(&text[start..i]);
        start = i + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|(_, n)| *n == '\n') {
            chars.next();
            start += 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

#[derive(Default)]
struct State {
    file: Option<File>,
    path: Option<PathBuf>,
    /// How much the open file holds.
    size: u64,
    memory: VecDeque<String>,
    /// (level, text) of the latest line.
    last: Option<(String, String)>,
    repeats: u64,
}

/// The event log: a file in the temp folder rotated at MAX_BYTES, repeats counted, or memory only without a file.
pub struct EventLog {
    state: Mutex<State>,
    max_bytes: u64,
}

impl Default for EventLog {
    fn default() -> Self {
        EventLog::new()
    }
}

impl EventLog {
    pub fn new() -> EventLog {
        EventLog { state: Mutex::new(State::default()), max_bytes: MAX_BYTES }
    }

    /// One that writes to path from the start, rotating at max_bytes.
    pub fn with_file(path: &Path, max_bytes: u64) -> EventLog {
        let log = EventLog { state: Mutex::new(State::default()), max_bytes };
        log.open(path);
        log
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn open(&self, path: &Path) -> bool {
        let Ok(file) = make_log_file(path) else { return false };
        let size = file.metadata().map_or(0, |m| m.len());
        let mut state = self.lock();
        state.file = Some(file);
        state.path = Some(path.to_path_buf());
        state.size = size;
        true
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.lock().path.clone()
    }

    pub fn previous_path(&self) -> Option<PathBuf> {
        self.lock().path.as_deref().map(previous_of)
    }

    /// Something the user saw (a status or banner), not a log record.
    pub fn note(&self, text: &str) {
        // Drop the seconds counter and unify the ellipsis so ticking statuses collapse into one count
        static SECONDS: OnceLock<Regex> = OnceLock::new();
        let text = text.replace('\u{2026}', "...");
        self.add("NOTE", &pattern(&SECONDS, r"\s*\(\d+s\)$").replace(&text, ""));
    }

    pub fn add(&self, level: &str, text: &str) {
        self.add_at(level, text, chrono::Local::now());
    }

    fn add_at(&self, level: &str, text: &str, when: chrono::DateTime<chrono::Local>) {
        let text: String = sanitize(text).chars().take(MAX_LINE).collect();
        let mut state = self.lock();
        if state.last.as_ref().is_some_and(|(l, t)| l == level && *t == text) {
            state.repeats += 1;
            return;
        }
        self.flush_repeats(&mut state);
        let line = format!("{} {level} {text}", when.format("%Y-%m-%d %H:%M:%S"));
        state.last = Some((level.to_string(), text));
        self.write(&mut state, &line);
    }

    /// Writes out what's still pending and closes the file; later lines stay in memory.
    pub fn close(&self) {
        let mut state = self.lock();
        self.flush_repeats(&mut state);
        state.file = None;
    }

    fn flush_repeats(&self, state: &mut State) {
        if state.repeats > 0 {
            let line = repeated(state.repeats);
            state.repeats = 0;
            self.write(state, &line);
        }
    }

    fn write(&self, state: &mut State, line: &str) {
        state.memory.push_back(line.to_string());
        if state.memory.len() > REPORT_LINES {
            state.memory.pop_front();
        }
        if state.file.is_none() {
            return;
        }
        if self.write_to_file(state, line).is_err() {
            state.file = None; // disk full or the folder was cleaned: carry on in memory
        }
    }

    fn write_to_file(&self, state: &mut State, line: &str) -> std::io::Result<()> {
        let path = state.path.clone().unwrap_or_default();
        if state.size > self.max_bytes {
            state.file = None;
            // Another process has it open (Windows): keep writing, and rotate on a later line
            let _ = fs::rename(&path, previous_of(&path));
            let file = OpenOptions::new().append(true).create(true).open(&path)?;
            state.size = file.metadata().map_or(0, |m| m.len());
            state.file = Some(file);
        }
        let Some(file) = state.file.as_mut() else { return Ok(()) };
        file.write_all(format!("{line}\n").as_bytes())?;
        file.flush()?;
        state.size += line.len() as u64 + 1;
        Ok(())
    }

    /// The latest lines, across a restart when the file has them.
    pub fn lines(&self, limit: usize) -> Vec<String> {
        let state = self.lock();
        let pending: Vec<String> = (state.repeats > 0).then(|| repeated(state.repeats)).into_iter().collect();
        let mut out: Vec<String> = Vec::new();
        match state.path.as_deref().filter(|_| state.file.is_some()) {
            None => out.extend(state.memory.iter().cloned()),
            Some(path) => {
                for file in [previous_of(path), path.to_path_buf()] {
                    if let Ok(bytes) = fs::read(file) {
                        out.extend(python_splitlines(&String::from_utf8_lossy(&bytes)).into_iter().map(str::to_string));
                    }
                }
            }
        }
        out.extend(pending);
        out.split_off(out.len().saturating_sub(limit))
    }
}

fn previous_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".1");
    path.with_file_name(name)
}

fn make_log_file(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if let Some(above) = parent.parent() {
            fs::create_dir_all(above)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            match fs::DirBuilder::new().mode(0o700).create(parent) {
                Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
                _ => {}
            }
            // SAFETY: getuid has no preconditions and can't fail
            if parent.metadata()?.uid() != unsafe { libc::getuid() } {
                return Err(std::io::Error::other("someone else's folder in a shared /tmp"));
            }
        }
        #[cfg(not(unix))]
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new().append(true).create(true).open(path)
}

impl log::Log for EventLog {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        // Other libraries' info is noise here
        let ours = record.target().starts_with("telescope") || record.target() == "__main__";
        if !self.enabled(record.metadata()) || (record.level() > log::Level::Warn && !ours) {
            return;
        }
        let level = match record.level() {
            log::Level::Error => "ERROR",
            log::Level::Warn => "WARNING",
            _ => "INFO",
        };
        self.add(level, &format!("{}: {}", record.target(), record.args()));
    }

    fn flush(&self) {}
}

/// The log everything in this program goes to.
pub fn events() -> &'static EventLog {
    static EVENTS: OnceLock<EventLog> = OnceLock::new();
    EVENTS.get_or_init(EventLog::new)
}

/// Linux: a per-user folder in /tmp (cleared at reboot). Windows: %TEMP%\Telescope, which Storage Sense and Disk Cleanup clear.
pub fn log_dir() -> PathBuf {
    log_dir_for(crate::config::dev_profile(), &std::env::temp_dir(), cfg!(windows), &user_name())
}

/// log_dir with everything it goes by given.
pub fn log_dir_for(dev_profile: Option<PathBuf>, temp: &Path, windows: bool, user: &str) -> PathBuf {
    if let Some(folder) = dev_profile {
        return folder.join("logs");
    }
    if windows { temp.join("Telescope") } else { temp.join(format!("telescope-{user}")) }
}

/// getpass.getuser(): the first of these set, else the account's name, else its number.
fn user_name() -> String {
    for name in ["LOGNAME", "USER", "LNAME", "USERNAME"] {
        if let Some(user) = std::env::var(name).ok().filter(|u| !u.is_empty()) {
            return user;
        }
    }
    #[cfg(unix)]
    {
        // SAFETY: getpwuid returns null or a record that stays valid until the next call; its name is copied out at once
        unsafe {
            let uid = libc::getuid();
            let entry = libc::getpwuid(uid);
            if entry.is_null() {
                uid.to_string()
            } else {
                std::ffi::CStr::from_ptr((*entry).pw_name).to_string_lossy().into_owned()
            }
        }
    }
    #[cfg(not(unix))]
    String::new()
}

/// A telescope.log shortcut next to the app, skipped where it can't be made or a real file has that name.
pub fn link_log(link: &Path, target: &Path) {
    let made = || -> std::io::Result<()> {
        if fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink()) {
            let current = fs::read_link(link)?;
            // Windows prefixes \\?\
            let current = current.to_string_lossy().into_owned();
            if Path::new(current.strip_prefix(r"\\?\").unwrap_or(&current)) == target {
                return Ok(());
            }
            fs::remove_file(link)?;
        } else if link.exists() {
            return Ok(());
        }
        make_symlink(target, link)
    };
    let _ = made();
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// What a panic says about itself: the message and where it was raised (file name and line only, no full path).
pub fn describe(message: &str, file: &str, line: u32) -> String {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    format!("panic: {message} at {name}:{line}")
}

fn describe_panic(info: &std::panic::PanicHookInfo) -> String {
    let payload = info.payload();
    let message = payload.downcast_ref::<&str>().copied().or_else(|| payload.downcast_ref::<String>().map(String::as_str)).unwrap_or("?");
    match info.location() {
        Some(at) => describe(message, at.file(), at.line()),
        None => format!("panic: {message}"),
    }
}

/// Log to the file from here on, including panics (in any thread), and link it into app_dir. `version` is what START says.
pub fn start(events: &'static EventLog, dir: &Path, app_dir: Option<&Path>, version: &str) {
    if events.open(&dir.join("telescope.log")) {
        // The shortcut next to the app belongs to the real one
        if let (Some(app_dir), Some(path), false) = (app_dir, events.path(), crate::config::dev_profile().is_some()) {
            link_log(&app_dir.join("telescope.log"), &path);
        }
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        events.add("CRASH", &describe_panic(info));
        previous(info);
    }));
    events.add("START", &format!("Telescope {version}"));
}

/// Windows 11 still reports itself as release "10"; only the build number tells them apart.
pub fn windows_line(release: &str, build: u32, version: &str) -> String {
    let release = if release == "10" && build >= 22000 { "11" } else { release };
    format!("OS: Windows {release} ({version})")
}

/// PRETTY_NAME out of an os-release file.
pub fn pretty_name(os_release: &str) -> Option<String> {
    os_release.lines().find_map(|line| {
        let value = line.strip_prefix("PRETTY_NAME=")?.trim();
        let unquoted = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))).unwrap_or(value);
        Some(unquoted.to_string())
    })
}

#[cfg(target_os = "linux")]
fn kernel_release() -> String {
    // SAFETY: uname fills the struct it's given; a zeroed one is a valid thing to hand it
    unsafe {
        let mut name: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut name) != 0 {
            return String::new();
        }
        std::ffi::CStr::from_ptr(name.release.as_ptr()).to_string_lossy().into_owned()
    }
}

/// The OS and desktop lines of a report.
pub fn system_lines() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        let os_name = ["/etc/os-release", "/usr/lib/os-release"].iter().find_map(|p| fs::read_to_string(p).ok())
            .and_then(|text| pretty_name(&text)).unwrap_or_else(|| "Linux".into());
        let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "?".into());
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "?".into());
        vec![format!("OS: {os_name} ({})", kernel_release()), format!("Desktop: {desktop}, {session}")]
    }
    #[cfg(windows)]
    {
        let (major, minor, build) = windows_version();
        let release = match (major, minor) {
            (10, _) => "10",
            (6, 3) => "8.1",
            (6, 2) => "8",
            (6, 1) => "7",
            _ => "Vista",
        };
        vec![windows_line(release, build, &format!("{major}.{minor}.{build}"))]
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    vec![format!("OS: {}", std::env::consts::OS)]
}

/// (major, minor, build) of the running Windows, from RtlGetVersion (GetVersionEx lies to programs without a manifest).
#[cfg(windows)]
fn windows_version() -> (u32, u32, u32) {
    #[repr(C)]
    struct OsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
    }
    // SAFETY: the struct is the one RtlGetVersion documents, with its size set
    unsafe {
        let mut info: OsVersionInfo = std::mem::zeroed();
        info.size = std::mem::size_of::<OsVersionInfo>() as u32;
        RtlGetVersion(&mut info);
        (info.major, info.minor, info.build)
    }
}

/// The text Copy diagnostics copies: `state` is what the plugins report, `windowing` the backend the window came up on.
pub fn report_for(events: &EventLog, build: &str, commit: &str, state: &[(String, String)], windowing: &str) -> String {
    let mut head = format!("Telescope {build}");
    if !commit.is_empty() {
        head.push_str(&format!(" ({})", commit.chars().take(7).collect::<String>()));
    }
    let mut lines = vec![head];
    lines.extend(system_lines());
    let slint = if windowing.is_empty() { String::new() } else { format!(", Slint {windowing}") };
    lines.push(format!("Rust{slint}{}", if cfg!(debug_assertions) { ", from source" } else { ", bundled" }));
    lines.extend(state.iter().map(|(key, value)| sanitize(&format!("{key}: {value}"))));
    let recent = events.lines(REPORT_LINES);
    lines.push(String::new());
    lines.push(if recent.is_empty() { "Log: empty".to_string() } else { format!("Log (last {REPORT_LINES} lines):") });
    lines.extend(recent);
    lines.join("\n")
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
