//! The desktop log and Copy diagnostics (diagnostics.py): every line is sanitized first, so no tokens, addresses or home paths.

use crate::i18n::{tr, Fields};
use crate::redact::{self, Hidden, Who};
use regex::bytes::Regex as Bytes;
use regex::Regex;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Per file; the one before is kept as telescope.log.1.
pub const MAX_BYTES: u64 = 1_000_000;
/// A line this long still leaves room for the rest of a GitHub comment (the timing lines are the longest).
pub const MAX_LINE: usize = 450;
/// How many of the latest lines the report shows.
pub const REPORT_LINES: usize = 400;
/// How many lines are kept in memory, from which the report also takes every warning and error.
pub const RING_LINES: usize = 1000;
/// What the report aims to stay under, in bytes; GitHub's limit for a comment is 65,536 characters.
pub const REPORT_BUDGET: usize = 40_000;

fn pattern(cell: &'static OnceLock<Regex>, source: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(source).expect("a fixed pattern"))
}

fn ascii_pattern(cell: &'static OnceLock<Bytes>, source: &str) -> &'static Bytes {
    cell.get_or_init(|| Bytes::new(source).expect("a fixed pattern"))
}

/// Python's `\w`: letters, numbers and "_" (Rust's also takes marks, joiners and connectors, which Python's doesn't).
fn is_word(c: char) -> bool {
    static WORD: OnceLock<Regex> = OnceLock::new();
    if c.is_ascii() { c.is_ascii_alphanumeric() || c == '_' } else { pattern(&WORD, r"^[\p{L}\p{N}_]$").is_match(c.encode_utf8(&mut [0; 4])) }
}

/// Python's `\d`: decimal digits of any script.
fn is_digit(c: char) -> bool {
    static DIGIT: OnceLock<Regex> = OnceLock::new();
    if c.is_ascii() { c.is_ascii_digit() } else { pattern(&DIGIT, r"^\p{Nd}$").is_match(c.encode_utf8(&mut [0; 4])) }
}

/// What a pattern needs told apart in the text it reads.
#[derive(Clone, Copy, PartialEq)]
enum View {
    /// Letters, where Python's ignore-case also takes four letters of other scripts.
    Letters,
    /// Digits of any script.
    Digits,
    /// Only whether a character is one of [A-Za-z0-9_].
    Plain,
}

/// One ASCII byte for c, so the patterns below see word characters, spaces and digits as Python's `\w`, `\s` and `\d` do.
fn project(c: char, view: View) -> u8 {
    if c.is_ascii() {
        return match c {
            '\x1c'..='\x1f' => b'\t',
            _ if view == View::Plain && (c.is_ascii_alphanumeric() || c == '_') => b'a',
            _ => c as u8,
        };
    }
    match c {
        _ if c.is_whitespace() => b'\t',
        _ if !is_word(c) => b'~',
        '\u{130}' | '\u{131}' if view == View::Letters => b'i',
        '\u{17f}' if view == View::Letters => b's',
        '\u{212a}' if view == View::Letters => b'k',
        _ if view == View::Digits && is_digit(c) => b'0',
        _ => b'_',
    }
}

/// Replaces what re finds in text as seen through `view` (a byte per character, so a match maps back to the text); with `keep`
/// set, the text up to the end of that group stays in front of the replacement.
fn replace_in(text: &str, view: View, re: &Bytes, keep: Option<usize>, with: &str) -> String {
    let seen: Vec<u8> = text.chars().map(|c| project(c, view)).collect();
    let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).chain([text.len()]).collect();
    let (mut out, mut last) = (String::with_capacity(text.len()), 0);
    for caps in re.captures_iter(&seen) {
        let whole = caps.get(0).expect("the whole match");
        out.push_str(&text[last..starts[whole.start()]]);
        if let Some(group) = keep {
            out.push_str(&text[starts[whole.start()]..starts[caps.get(group).expect("a group in every match").end()]]);
        }
        out.push_str(with);
        last = starts[whole.end()];
    }
    out.push_str(&text[last..]);
    out
}

/// Strip what shouldn't end up in a public issue: URLs, IPs, tokens, the user's home path, MAC addresses, emails, ids and names.
pub fn sanitize(text: &str) -> String {
    sanitize_with(text, &Who::this_machine(user_name), redact::hidden().as_deref())
}

/// sanitize with the home folder given, so a test can choose it.
pub fn sanitize_at(text: &str, home: Option<&Path>) -> String {
    sanitize_with(text, &Who::cached(home), None)
}

/// Every line of a report through sanitize once more, so what the pieces missed between them doesn't go out.
pub fn sanitize_lines(report: &str) -> String {
    report.lines().map(sanitize).collect::<Vec<_>>().join("\n")
}

fn sanitize_with(text: &str, who: &Who, hidden: Option<&Hidden>) -> String {
    static URL: OnceLock<Bytes> = OnceLock::new();
    static BEARER: OnceLock<Bytes> = OnceLock::new();
    static IPV4: OnceLock<Bytes> = OnceLock::new();
    static LONG_SECRET: OnceLock<Bytes> = OnceLock::new();
    let text = redact::hide_homes(text, who);
    // The patterns are the Python ones, ASCII only: `project` has already made the text say what Python's classes would
    let text = replace_in(&text, View::Letters, ascii_pattern(&URL, r"(?i-u)\b[a-z][a-z0-9+.-]*://\S+"), None, "<url>");
    let text = replace_in(&text, View::Letters, ascii_pattern(&BEARER, r#"(?i-u)(bearer|token|nonce|secret)(["'=: ]+)[^\s"',}]+"#), Some(2), "<redacted>");
    let text = redact::hide_host(&redact::hide_ids(&text), who);
    let text = hidden.map_or(text.clone(), |h| h.apply(&text));
    let text = replace_in(&text, View::Digits, ascii_pattern(&IPV4, r"(?-u)\b\d{1,3}(?:\.\d{1,3}){3}(?::\d+)?\b"), None, "<ip>");
    let text = replace_ipv6(&text);
    // [A-Za-z0-9_] is all one letter here, so any other word character can't be taken for one
    replace_in(&text, View::Plain, ascii_pattern(&LONG_SECRET, r"(?-u)\b[a\-+/]{24,}={0,2}"), None, "<redacted>")
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

/// "    (repeated N more times)": what a run of the same line becomes, with the last one's text when its numbers differed.
fn repeated(n: u64, latest: Option<&str>) -> String {
    match latest {
        Some(text) => format!("    (repeated {n} more times, last: {text})"),
        None => format!("    (repeated {n} more times)"),
    }
}

/// text with every number (digits, with a fraction) as "#", so lines that differ only in their numbers compare equal.
fn without_numbers(text: &str) -> String {
    let (mut out, mut in_number) = (String::with_capacity(text.len()), false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || (in_number && c == '.' && chars.peek().is_some_and(char::is_ascii_digit)) {
            if !in_number {
                out.push('#');
            }
            in_number = true;
        } else {
            in_number = false;
            out.push(c);
        }
    }
    out
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
    /// (level, text with its numbers taken out, text) of the line a run of repeats belongs to.
    last: Option<(String, String, String)>,
    repeats: u64,
    /// The text of the newest repeat when its numbers differ from the first line's.
    latest: Option<String>,
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
        let plain = without_numbers(&text);
        let mut state = self.lock();
        if state.last.as_ref().is_some_and(|(l, p, _)| l == level && *p == plain) {
            state.repeats += 1;
            // Only the newest repeat's numbers are kept
            state.latest = state.last.as_ref().filter(|(_, _, first)| *first != text).map(|_| text);
            return;
        }
        self.flush_repeats(&mut state);
        let line = format!("{} {level} {text}", when.format("%Y-%m-%d %H:%M:%S"));
        state.last = Some((level.to_string(), plain, text));
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
            let line = repeated(state.repeats, state.latest.as_deref());
            state.repeats = 0;
            state.latest = None;
            self.write(state, &line);
        }
    }

    fn write(&self, state: &mut State, line: &str) {
        state.memory.push_back(line.to_string());
        if state.memory.len() > RING_LINES {
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
        let pending: Vec<String> = (state.repeats > 0).then(|| repeated(state.repeats, state.latest.as_deref())).into_iter().collect();
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

static STARTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The window is being started: a failure now ends in a message box (Windows), since nothing else would say what happened. Called
/// with true before the window starts and with false once it has shown.
pub fn set_starting(on: bool) {
    STARTING.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// The title and words of the box for a start that failed: what went wrong, and where the log is.
pub fn start_failed_text(reason: &str, log: &Path) -> (String, String) {
    (
        tr("Telescope can't start"),
        tr("Telescope ran into a problem while starting and has to close.\n\nWhat went wrong: {reason}\n\nThe log is at {path}\n\nOpen the log?").field("reason", reason).field("path", log.display()),
    )
}

/// Says a start failed in a message box, once and only while the window is starting (Windows; elsewhere the terminal has it).
pub fn show_start_failure(events: &EventLog, reason: &str) {
    if !STARTING.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let log = events.path().unwrap_or_else(|| log_dir().join("telescope.log"));
    let (title, text) = start_failed_text(reason, &log);
    if crate::system::ask_box(&title, &text) {
        crate::launch::open_url(&log.to_string_lossy());
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
        let what = describe_panic(info);
        events.add("CRASH", &what);
        // Only the main thread's panic ends the start; a worker's is caught and logged
        if std::thread::current().name() == Some("main") {
            // A panic has its reason in hand, so it isn't blamed on the graphics card
            crate::gpu_marker::clear();
            show_start_failure(events, &what);
        }
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

/// A line the report keeps however old it is: a warning, an error, a crash or the start of a run.
fn matters(line: &str) -> bool {
    matches!(line.splitn(4, ' ').nth(2), Some("WARNING" | "ERROR" | "CRASH" | "START"))
}

/// The lines (oldest first) the report keeps: the last `tail` and every one that matters, within room bytes by dropping the oldest ordinary ones first.
pub fn pick_log(lines: &[String], tail: usize, room: usize) -> Vec<String> {
    let first_tail = lines.len().saturating_sub(tail);
    let mut keep: Vec<bool> = lines.iter().enumerate().map(|(i, l)| i >= first_tail || matters(l)).collect();
    let mut size: usize = lines.iter().zip(&keep).filter(|(_, k)| **k).map(|(l, _)| l.len() + 1).sum();
    for want_matters in [false, true] {
        for i in 0..lines.len() {
            if size <= room {
                break;
            }
            if keep[i] && matters(&lines[i]) == want_matters {
                keep[i] = false;
                size -= lines[i].len() + 1;
            }
        }
    }
    let mut out = Vec::new();
    let mut left_out = 0;
    for (line, kept) in lines.iter().zip(&keep) {
        if *kept {
            if left_out > 0 {
                out.push(format!("    ({left_out} lines left out)"));
                left_out = 0;
            }
            out.push(line.clone());
        } else {
            left_out += 1;
        }
    }
    if left_out > 0 {
        out.push(format!("    ({left_out} lines left out)"));
    }
    out
}

/// The worst moments of the session, as report lines.
fn worst_lines() -> Vec<String> {
    let stalls = crate::perf::worst_stalls();
    if stalls.is_empty() {
        return vec![format!("Worst moments: none (no gap between frames over {} ms, no window under half the rate)", crate::perf::GAP_MS)];
    }
    let mut out = vec![format!("Worst moments of this session (gaps between frames over {} ms, or windows under half the rate before; worst first):", crate::perf::GAP_MS)];
    out.extend(stalls.iter().map(|s| format!("  {}", s.line())));
    out
}

/// How long each stage of a frame takes, as report lines.
fn stage_lines() -> Vec<String> {
    let summaries = crate::perf::stage_summaries();
    if summaries.is_empty() {
        return vec!["Stage times: nothing has streamed yet".to_string()];
    }
    let mut out = vec!["Stage times over the latest frames (ms: p50 / p99 / max, frames seen):".to_string()];
    out.extend(summaries.iter().map(|s| format!("  {:<10} {} / {} / {}  ({})", s.stage, crate::perf::ms(s.p50_ms), crate::perf::ms(s.p99_ms), crate::perf::ms(s.max_ms), s.count)));
    out
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
    lines.extend(crate::machine::lines());
    lines.extend(state.iter().map(|(key, value)| format!("{key}: {value}")));
    lines.push(String::new());
    lines.extend(worst_lines());
    let timeline = crate::perf::timeline_lines(crate::time::monotonic_ns());
    if !timeline.is_empty() {
        lines.push(String::new());
        lines.push(format!("Timeline of each stream, the last {} minutes oldest first, a character for {} s ('~' is no stream, '#' the most):", crate::perf::COLUMNS as u64 * crate::perf::STEP_S / 60, crate::perf::STEP_S));
        lines.extend(timeline);
    }
    lines.push(String::new());
    lines.extend(stage_lines());
    lines.push(String::new());
    let recent = events.lines(RING_LINES);
    // The log gets what the rest leaves of the budget
    let used: usize = lines.iter().map(|l| l.len() + 1).sum();
    let log = pick_log(&recent, REPORT_LINES, REPORT_BUDGET.saturating_sub(used + 100));
    lines.push(if recent.is_empty() { "Log: empty".to_string() } else if recent.len() <= REPORT_LINES { format!("Log ({} lines):", recent.len()) } else { format!("Log (the last {REPORT_LINES} lines, and every warning and error before them):") });
    lines.extend(log);
    // Everything once more, as one piece, so nothing a section let through goes out
    sanitize_lines(&lines.join("\n"))
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
