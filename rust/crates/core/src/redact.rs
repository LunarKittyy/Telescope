//! What Copy diagnostics hides on top of tokens and addresses: home folders in every spelling, MAC addresses, emails, ids,
//! computer and device names. diagnostics::sanitize runs all of it on every log line and again over the whole report.

use regex::{Match, Regex};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

fn pattern(cell: &'static OnceLock<Regex>, source: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(source).expect("a fixed pattern"))
}

/// Replaces what re finds where keep says so; keep gets the text and the match, and gives the replacement or None to leave it.
fn replace_where(text: &str, re: &Regex, keep: impl Fn(&str, Match) -> Option<&'static str>) -> String {
    let (mut out, mut last) = (String::with_capacity(text.len()), 0);
    for m in re.find_iter(text) {
        if let Some(with) = keep(text, m) {
            out.push_str(&text[last..m.start()]);
            out.push_str(with);
            last = m.end();
        }
    }
    out.push_str(&text[last..]);
    out
}

fn next_char(text: &str, at: usize) -> Option<char> {
    text[at..].chars().next()
}

/// A name that stands for a machine or a person: the home folder, the account and the computer.
pub struct Who {
    home: Option<Regex>,
    user: Option<Regex>,
    host: Option<Regex>,
}

/// Computer names that are ordinary words, which would hide too much.
const PLAIN_HOSTS: [&str; 9] = ["localhost", "fedora", "ubuntu", "debian", "archlinux", "nobara", "windows", "linux", "android"];

impl Who {
    /// For this home folder, account name and computer name; any of them may be unknown.
    pub fn new(home: Option<&Path>, user: Option<&str>, host: Option<&str>) -> Who {
        let home = home.map(|h| h.to_string_lossy().into_owned()).filter(|h| h.chars().count() > 1);
        let short = home.as_deref().and_then(short_path);
        let variants: Vec<String> = [home.as_deref(), short.as_deref()].into_iter().flatten().filter_map(path_pattern).collect();
        let home = (!variants.is_empty()).then(|| format!(r"(?i)(?:[\\/]{{2}}[?.][\\/])?(?:{})", variants.join("|"))).and_then(|p| Regex::new(&p).ok());
        let word = |name: &str| Regex::new(&format!(r"(?i)\b{}\b", regex::escape(name))).ok();
        let user_re = user.filter(|u| u.chars().count() >= 3).and_then(|u| Regex::new(&format!(r"(?i)\b(telescope-){}\b", regex::escape(u))).ok());
        let host = host.map(|h| h.split('.').next().unwrap_or(h)).filter(|h| h.chars().count() >= 4 && !PLAIN_HOSTS.contains(&h.to_lowercase().as_str())).and_then(word);
        Who { home, user: user_re, host }
    }

    /// The same Who again for the same home, so a line doesn't compile patterns.
    pub fn cached(home: Option<&Path>) -> Arc<Who> {
        static CACHE: Mutex<Option<HashMap<Option<std::path::PathBuf>, Arc<Who>>>> = Mutex::new(None);
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.get_or_insert_with(HashMap::new).entry(home.map(Path::to_path_buf)).or_insert_with(|| Arc::new(Who::new(home, None, None))).clone()
    }

    /// This computer's: the home folder, account and computer name.
    pub fn this_machine(user: impl FnOnce() -> String) -> Arc<Who> {
        static THIS: OnceLock<Arc<Who>> = OnceLock::new();
        THIS.get_or_init(|| Arc::new(Who::new(std::env::home_dir().as_deref(), Some(&user()), host_name().as_deref()))).clone()
    }
}

/// A regex source for a path in any spelling: either slash, doubled slashes, any case.
fn path_pattern(path: &str) -> Option<String> {
    let path = path.strip_prefix(r"\\?\").or_else(|| path.strip_prefix(r"\\.\")).unwrap_or(path);
    let parts: Vec<String> = path.split(['/', '\\']).filter(|p| !p.is_empty()).map(regex::escape).collect();
    if parts.is_empty() {
        return None;
    }
    let body = parts.join(r"[\\/]+");
    Some(if path.starts_with(['/', '\\']) { format!(r"[\\/]+{body}") } else { body })
}

/// The 8.3 name of a folder ("C:\Users\LUNAKI~1"), which Windows shows for long names; None elsewhere or when it has none.
#[cfg(windows)]
fn short_path(path: &str) -> Option<String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetShortPathNameW(long: *const u16, short: *mut u16, len: u32) -> u32;
    }
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 1024];
    // SAFETY: wide ends in a NUL and buf's length is what the call is told
    let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    Some(String::from_utf16_lossy(&buf[..n])).filter(|s| n > 0 && n < buf.len() && s != path)
}

#[cfg(not(windows))]
fn short_path(_path: &str) -> Option<String> {
    None
}

/// This computer's name, if the system gives one.
fn host_name() -> Option<String> {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: buf is a valid buffer of the length given, and one byte is left for the NUL
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len() - 1) } != 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).into_owned()).filter(|h| !h.is_empty())
    }
    #[cfg(not(unix))]
    std::env::var("COMPUTERNAME").ok().filter(|h| !h.is_empty())
}

/// The home folder in any spelling becomes ~, other people's home folders lose their name, and the account's name goes from the log folder's.
pub fn hide_homes(text: &str, who: &Who) -> String {
    static OTHER: OnceLock<Regex> = OnceLock::new();
    let mut text = match &who.home {
        // A longer name that starts the same ("Luna" and "LUNAKI~1") is somebody else's, which the pattern below takes
        Some(home) => replace_where(text, home, |t, m| (!next_char(t, m.end()).is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '~')).then_some("~")),
        None => text.to_string(),
    };
    if let Some(user) = &who.user {
        text = user.replace_all(&text, "${1}<user>").into_owned();
    }
    let other = pattern(&OTHER, r#"(?i)([\\/](?:home|users)[\\/]+)([^\\/\s"'<>|:*?,;()\[\]{}]+)"#);
    other.replace_all(&text, |caps: &regex::Captures| {
        let shared = ["public", "default", "all", "shared"].contains(&caps[2].to_lowercase().as_str());
        if shared { caps[0].to_string() } else { format!("{}<user>", &caps[1]) }
    }).into_owned()
}

/// MAC addresses, emails, UUIDs, long hex serials, local host names and Windows computer names.
pub fn hide_ids(text: &str) -> String {
    static MAC: OnceLock<Regex> = OnceLock::new();
    static UUID: OnceLock<Regex> = OnceLock::new();
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    static HEX: OnceLock<Regex> = OnceLock::new();
    static HOST: OnceLock<Regex> = OnceLock::new();
    static FIELD: OnceLock<Regex> = OnceLock::new();
    static DEFAULT_PC: OnceLock<Regex> = OnceLock::new();
    // Six pairs, and not the start of something longer such as eight groups of an IPv6 address
    let mac = pattern(&MAC, r"(?i)\b[0-9a-f]{2}(?:[:-][0-9a-f]{2}){5}\b");
    let text = replace_where(text, mac, |t, m| {
        let longer = next_char(t, m.end()).is_some_and(|c| matches!(c, ':' | '-')) && t[m.end() + 1..].chars().next().is_some_and(|c| c.is_ascii_hexdigit());
        (!longer).then_some("<mac>")
    });
    let text = pattern(&UUID, r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b").replace_all(&text, "<uuid>").into_owned();
    // The domain's first label being "2x" makes it a picture's name (icon@2x.png), not an address
    let email = pattern(&EMAIL, r"(?i)\b[a-z0-9._%+-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,}\b");
    let text = replace_where(&text, email, |t, m| {
        let domain = t[m.start()..m.end()].split_once('@').map_or("", |(_, d)| d).split('.').next().unwrap_or("").to_lowercase();
        let picture = domain.strip_suffix('x').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        (!picture).then_some("<email>")
    });
    // Twelve or more hex characters is a serial or a hash; a plain number that long is a count or a time
    let text = replace_where(&text, pattern(&HEX, r"(?i)\b[0-9a-f]{12,}\b"), |t, m| (!t[m.range()].chars().all(|c| c.is_ascii_digit())).then_some("<id>"));
    // settings.local.json is a file, phone.local is a computer
    let host = pattern(&HOST, r"(?i)\b[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)*\.(?:local|lan|localdomain|internal|home\.arpa)\b");
    let text = replace_where(&text, host, |t, m| {
        let goes_on = next_char(t, m.end()).is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
            || (next_char(t, m.end()) == Some('.') && next_char(t, m.end() + 1).is_some_and(char::is_alphanumeric));
        (!goes_on).then_some("<host>")
    });
    let text = pattern(&FIELD, r#"(?i)\b(computer[ _]?name|computer|host[ _]?name|machine[ _]?name|pc[ _]name|user[ _]?domain|user[ _]?name)(\s*[=:]\s*)[^,;\r\n<")\]]+"#).replace_all(&text, "${1}${2}<name>").into_owned();
    pattern(&DEFAULT_PC, r"\b(?:(?:DESKTOP|LAPTOP)-[A-Z0-9]{5,9}|WIN-[A-Z0-9]{8,11})\b").replace_all(&text, "<computer>").into_owned()
}

/// This computer's own name, wherever it shows.
pub fn hide_host(text: &str, who: &Who) -> String {
    match &who.host {
        Some(host) => host.replace_all(text, "<host>").into_owned(),
        None => text.to_string(),
    }
}

/// Words that name nothing in particular, so a device called this isn't hidden (and a text with them isn't rewritten).
const PLAIN_NAMES: [&str; 12] = ["phone", "camera", "webcam", "android", "device", "computer", "mobile", "unknown", "default", "none", "video", "usb"];
/// Who makes it, or what kind of thing it is: a name starting with one of these and going on with model words is a model.
const MAKERS: [&str; 53] = [
    "logitech", "microsoft", "razer", "elgato", "avermedia", "creative", "anker", "obsbot", "insta360", "dell", "hp", "lenovo", "asus", "acer",
    "samsung", "galaxy", "pixel", "google", "xiaomi", "redmi", "poco", "oneplus", "oppo", "vivo", "realme", "huawei", "honor", "motorola", "moto",
    "nokia", "sony", "xperia", "nothing", "iphone", "ipad", "apple", "integrated", "usb", "uvc", "obs", "v4l2loopback", "unitycapture", "telescope",
    "virtual", "generic", "hd", "full", "facetime", "brio", "easycamera", "camera", "webcam", "logi",
];
const MODEL_WORDS: [&str; 24] = [
    "pro", "max", "ultra", "plus", "mini", "lite", "hd", "fe", "se", "series", "webcam", "camera", "usb", "uvc", "stream", "cam", "edition", "gen",
    "wireless", "pad", "virtual", "full", "integrated", "video",
];

/// Whether a name is a maker's model ("Pixel 7 Pro", "Logitech HD Pro Webcam C920") and so says nothing about its owner.
pub fn is_model_string(name: &str) -> bool {
    let lower = name.to_lowercase().replace('_', " ");
    let mut words = lower.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()));
    let Some(first) = words.next() else { return false };
    let maker = |w: &str| MAKERS.contains(&w) || w.starts_with("usb") || w.starts_with("uvc");
    let model_word = |w: &str| w.chars().count() <= 2 || w.chars().any(|c| c.is_ascii_digit()) || MODEL_WORDS.contains(&w) || maker(w);
    maker(first) && !lower.contains('\'') && !lower.contains('\u{2019}') && words.all(model_word)
}

/// Names the user chose (phones, webcams, this computer), told to us by the plugins that know them.
#[derive(Default)]
pub struct Hidden {
    /// (kind, names) in the order told; a name is shown as "kind N" by its place in its own kind's list.
    sets: Vec<(&'static str, Vec<String>)>,
    all: Option<Regex>,
}

static HIDDEN: Mutex<Option<Arc<Hidden>>> = Mutex::new(None);

/// kind's names are these from now on (replacing the last list for kind): "phone", "webcam", "computer". Model strings stay.
pub fn set_hidden(kind: &'static str, names: &[String]) {
    let names: Vec<String> = names.iter().map(|n| n.trim().to_string()).filter(|n| n.chars().count() >= 4 && !PLAIN_NAMES.contains(&n.to_lowercase().as_str()) && !is_model_string(n)).collect();
    let mut slot = HIDDEN.lock().unwrap_or_else(|e| e.into_inner());
    let mut sets: Vec<(&'static str, Vec<String>)> = slot.as_ref().map(|h| h.sets.clone()).unwrap_or_default();
    sets.retain(|(k, _)| *k != kind);
    sets.push((kind, names));
    let mut every: Vec<&String> = sets.iter().flat_map(|(_, n)| n).collect();
    every.sort_by_key(|n| std::cmp::Reverse(n.len()));
    let all = (!every.is_empty()).then(|| every.iter().map(|n| regex::escape(n)).collect::<Vec<_>>().join("|")).and_then(|p| Regex::new(&format!("(?i)(?:{p})")).ok());
    *slot = Some(Arc::new(Hidden { sets, all }));
}

/// What was last told to set_hidden.
pub fn hidden() -> Option<Arc<Hidden>> {
    HIDDEN.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

impl Hidden {
    /// A Hidden for these names, as set_hidden would make it (for tests and for anyone who wants it apart from the global).
    pub fn of(kind: &'static str, names: &[&str]) -> Hidden {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).filter(|n| n.chars().count() >= 4 && !PLAIN_NAMES.contains(&n.to_lowercase().as_str()) && !is_model_string(n)).collect();
        let all = (!names.is_empty()).then(|| names.iter().map(|n| regex::escape(n)).collect::<Vec<_>>().join("|")).and_then(|p| Regex::new(&format!("(?i)(?:{p})")).ok());
        Hidden { sets: vec![(kind, names)], all }
    }

    /// text with each of the names replaced by "kind N", where the name stands alone (not inside a longer word).
    pub fn apply(&self, text: &str) -> String {
        let Some(all) = &self.all else { return text.to_string() };
        let (mut out, mut last) = (String::with_capacity(text.len()), 0);
        for m in all.find_iter(text) {
            let before = text[..m.start()].chars().next_back();
            let after = next_char(text, m.end());
            let (edge_before, edge_after) = (text[m.start()..].chars().next(), text[..m.end()].chars().next_back());
            let inside = |near: Option<char>, edge: Option<char>| near.is_some_and(char::is_alphanumeric) && edge.is_some_and(char::is_alphanumeric);
            if inside(before, edge_before) || inside(after, edge_after) {
                continue;
            }
            let Some(label) = self.label(&text[m.range()]) else { continue };
            out.push_str(&text[last..m.start()]);
            out.push_str(&label);
            last = m.end();
        }
        out.push_str(&text[last..]);
        out
    }

    fn label(&self, matched: &str) -> Option<String> {
        let lower = matched.to_lowercase();
        self.sets.iter().find_map(|(kind, names)| names.iter().position(|n| n.to_lowercase() == lower).map(|i| format!("{kind} {}", i + 1)))
    }
}

#[cfg(test)]
#[path = "redact_tests.rs"]
mod tests;
