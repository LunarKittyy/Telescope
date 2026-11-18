//! What the photo plugins share about the phone's stills: what a lens can take, the options a set of picks asks for and the folder photos
//! go to unless told otherwise. Plain helpers with no plugin in them, so Photos and Phototrap each use them without using each other.

use crate::shared::py_float;
use telescope_core::i18n::n_;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const EXTENSIONS: [&str; 4] = ["night", "hdr", "bokeh", "auto"];
/// Exposure brackets: (key, label, EV steps).
pub const BRACKETS: [(&str, &str, &[i32]); 4] = [
    ("3x1", n_("3 photos, 1 EV apart"), &[-1, 0, 1]), ("3x2", n_("3 photos, 2 EV apart"), &[-2, 0, 2]),
    ("5x1", n_("5 photos, 1 EV apart"), &[-2, -1, 0, 1, 2]), ("7x1", n_("7 photos, 1 EV apart"), &[-3, -2, -1, 0, 1, 2, 3]),
];
/// Long exposures offered, up to what the sensor allows.
pub const EXPOSURES_S: [f64; 9] = [0.5, 1.0, 2.0, 4.0, 8.0, 15.0, 30.0, 60.0, 120.0];
pub const TIMERS: [i64; 3] = [0, 3, 10];

/// What the lens a stream is on can take (stills.StillCaps), as the link crate describes it. Anything not here isn't offered.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct StillCaps {
    pub formats: Vec<String>,
    /// The largest JPEG.
    pub size: Option<(i64, i64)>,
    /// The maximum resolution sensor mode's JPEG, when it's bigger.
    pub max_res: Option<(i64, i64)>,
    pub max_res_raw: bool,
    pub extensions: Vec<String>,
    /// Long exposures up to this; 0 = no manual exposure.
    pub exposure_max_ns: i64,
    pub bracketing: bool,
    pub focus_bracketing: bool,
    pub noise_reduction_hq: bool,
    pub sharpening_hq: bool,
    pub hot_pixel_hq: bool,
    pub distortion: bool,
    pub burst_max: i64,
    pub extension_sizes: Vec<(String, (i64, i64))>,
    /// A camera with settings of its own (a DSLR): no JPEG quality to pick, and ISO and shutter speed are its own lists.
    pub camera: bool,
    pub iso: Option<CameraSetting>,
    pub shutter: Option<CameraSetting>,
}

/// One of a camera's own lists: what to send back for each entry, how to show it, and which one the camera is on (None: none listed).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct CameraSetting {
    pub values: Vec<String>,
    pub labels: Vec<String>,
    pub current: Option<usize>,
}

impl Default for StillCaps {
    fn default() -> StillCaps {
        StillCaps {
            formats: vec!["jpeg".into()], size: None, max_res: None, max_res_raw: false, extensions: Vec::new(), exposure_max_ns: 0,
            bracketing: false, focus_bracketing: false, noise_reduction_hq: false, sharpening_hq: false, hot_pixel_hq: false,
            distortion: false, burst_max: 1, extension_sizes: Vec::new(), camera: false, iso: None, shutter: None,
        }
    }
}

impl StillCaps {
    /// The size a photo in this mode comes out at; a maker's mode that didn't say is taken at the plain size.
    pub fn size_for(&self, mode: &str) -> Option<(i64, i64)> {
        if mode == "max_res" && self.max_res.is_some() {
            return self.max_res;
        }
        self.extension_sizes.iter().find(|(m, _)| m == mode).map(|(_, s)| *s).or(self.size)
    }
}

/// The picks, by the names saved settings use. Numbers stay as they were saved (95 or 95.0).
pub type Choices = Map<String, Value>;

pub fn defaults() -> Choices {
    let Value::Object(map) = json!({"format": "jpeg", "quality": 95, "mode": "normal", "shots": "single", "burst": 5, "bracket": "3x1",
                                    "focus_steps": 5, "bracket_merge": "frames", "merge_brightness": 0, "exposure_s": 0, "timer": 0, "best": true, "distortion": "camera"}) else { unreachable!() };
    map
}

pub fn text<'a>(choices: &'a Choices, key: &str) -> &'a str {
    choices.get(key).and_then(Value::as_str).unwrap_or("")
}

pub fn number(choices: &Choices, key: &str, default: f64) -> f64 {
    choices.get(key).and_then(py_float).filter(|v| v.is_finite()).unwrap_or(default)
}

/// What the card offers for this lens, given what's picked, as keys. A maker's mode only saves single JPEGs or bursts of them and sets
/// its own exposure; full resolution only saves JPEG and RAW.
#[derive(Clone, Debug, PartialEq)]
pub struct Offered {
    pub formats: Vec<&'static str>,
    pub modes: Vec<&'static str>,
    pub shots: Vec<&'static str>,
    /// Seconds; 0 is the video's own.
    pub exposures: Vec<f64>,
}

pub fn offered(caps: &StillCaps, choices: &Choices) -> Offered {
    let has = |list: &[String], what: &str| list.iter().any(|f| f == what);
    let mut modes: Vec<&'static str> = vec!["normal"];
    if caps.max_res.is_some() {
        modes.push("max_res");
    }
    modes.extend(EXTENSIONS.iter().filter(|m| has(&caps.extensions, m)));
    let mode = modes.iter().copied().find(|m| *m == text(choices, "mode")).unwrap_or("normal");
    let raw = has(&caps.formats, "raw") && (mode != "max_res" || caps.max_res_raw);
    let mut formats: Vec<&'static str> = vec!["jpeg"];
    if EXTENSIONS.contains(&mode) {
        // Only JPEG
    } else {
        if mode == "normal" {
            formats.extend(["heic", "ultrahdr"].into_iter().filter(|f| has(&caps.formats, f)));
        }
        if raw {
            formats.extend(["raw", "raw+jpeg"]);
        }
    }
    let mut shots: Vec<&'static str> = vec!["single"];
    if caps.burst_max > 1 {
        shots.push("burst");
    }
    if !EXTENSIONS.contains(&mode) {
        if caps.bracketing {
            shots.push("bracket");
        }
        if caps.focus_bracketing {
            shots.push("focus");
        }
    }
    let long = !EXTENSIONS.contains(&mode) && text(choices, "shots") != "bracket" && caps.exposure_max_ns > 0;
    let mut exposures = vec![0.0];
    if long {
        exposures.extend(EXPOSURES_S.iter().filter(|s| **s * 1e9 <= caps.exposure_max_ns as f64));
    }
    Offered { formats, modes, shots, exposures }
}

/// value when it's allowed, else the first that is.
pub fn pick<'a>(value: &str, allowed: &[&'a str]) -> &'a str {
    allowed.iter().copied().find(|a| *a == value).unwrap_or(allowed[0])
}

pub fn pick_number(value: f64, allowed: &[f64]) -> f64 {
    allowed.iter().copied().find(|a| *a == value).unwrap_or(allowed[0])
}

/// Python's int(): the number cut towards zero (the picks are never anything else once they've been loaded).
pub fn int(choices: &Choices, key: &str, default: i64) -> i64 {
    number(choices, key, default as f64) as i64
}

/// The /v1/still options for the picks, fitted to what this lens offers.
pub fn build_options(caps: &StillCaps, choices: &Choices, rotation: i32) -> Value {
    let can = offered(caps, choices);
    let mode = pick(text(choices, "mode"), &can.modes);
    let format = pick(text(choices, "format"), &can.formats);
    let shots = pick(text(choices, "shots"), &can.shots);
    let mut opts = Map::new();
    opts.insert("format".into(), json!(if format == "raw" { "none".to_string() } else { format.replace("raw+", "") }));
    opts.insert("rotation".into(), json!(rotation));
    if format.starts_with("raw") {
        opts.insert("raw".into(), json!(1));
    }
    if format != "raw" {
        opts.insert("quality".into(), json!(int(choices, "quality", 95).clamp(1, 100)));
    }
    if mode == "max_res" {
        opts.insert("max_res".into(), json!(1));
    } else if EXTENSIONS.contains(&mode) {
        opts.insert("extension".into(), json!(mode));
    }
    match shots {
        "burst" => {
            opts.insert("count".into(), json!(caps.burst_max.min(int(choices, "burst", 5).max(2))));
        }
        "bracket" => {
            let steps = BRACKETS.iter().find(|(k, ..)| *k == text(choices, "bracket")).map_or(BRACKETS[0].2, |(.., ev)| *ev);
            opts.insert("bracket_ev".into(), json!(steps.iter().map(i32::to_string).collect::<Vec<_>>().join(",")));
        }
        "focus" => {
            opts.insert("focus_steps".into(), json!(caps.burst_max.min(int(choices, "focus_steps", 5).max(2))));
        }
        _ => {}
    }
    let exposure = pick_number(number(choices, "exposure_s", 0.0), &can.exposures);
    if exposure != 0.0 {
        opts.insert("exposure_ns".into(), json!((exposure * 1e9) as i64));
    }
    let timer = number(choices, "timer", 0.0);
    if TIMERS.iter().any(|t| *t as f64 == timer) && timer != 0.0 {
        opts.insert("timer_s".into(), choices["timer"].clone());
    }
    if choices.get("best") == Some(&json!(true)) && !EXTENSIONS.contains(&mode) {
        for (key, has) in [("nr_hq", caps.noise_reduction_hq), ("edge_hq", caps.sharpening_hq), ("hot_pixel_hq", caps.hot_pixel_hq)] {
            if has {
                opts.insert(key.into(), json!(1));
            }
        }
    }
    let distortion = text(choices, "distortion");
    if caps.distortion && !EXTENSIONS.contains(&mode) && (distortion == "on" || distortion == "off") {
        opts.insert("distortion".into(), json!(i32::from(distortion == "on")));
    }
    Value::Object(opts)
}

/// A camera's own picks go with a photo as it is asked for: its ISO and shutter speed as the camera words them, and no JPEG quality
/// (it keeps its own).
pub fn camera_options(options: &mut Value, iso: Option<&str>, shutter: Option<&str>) {
    let Some(map) = options.as_object_mut() else { return };
    map.remove("quality");
    for (key, pick) in [("iso", iso), ("shutter", shutter)] {
        if let Some(pick) = pick {
            map.insert(key.into(), json!(pick));
        }
    }
}

/// Where photos go unless told otherwise: Pictures/Telescope (QStandardPaths' PicturesLocation), written the way pathlib joins it.
pub fn default_folder() -> String {
    pictures().join("Telescope").components().collect::<PathBuf>().display().to_string()
}

/// The user's Pictures folder: the known folder on Windows (so a redirected or OneDrive one counts), user-dirs.dirs elsewhere.
pub fn pictures() -> PathBuf {
    let home = std::env::home_dir().unwrap_or_default();
    #[cfg(windows)]
    {
        known_pictures().unwrap_or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or(home).join("Pictures"))
    }
    #[cfg(not(windows))]
    {
        let config = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
        let file = std::fs::read(config.join("user-dirs.dirs")).ok().map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
        pictures_in(&home, file.as_deref())
    }
}

/// SHGetKnownFolderPath(FOLDERID_Pictures), which is what Qt asks for and which follows a folder moved to another drive or OneDrive.
#[cfg(windows)]
pub fn known_pictures() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Pictures, SHGetKnownFolderPath, KF_FLAG_DONT_VERIFY};
    // SAFETY: the call hands back a string it allocated, which is copied and freed here
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DONT_VERIFY, None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        text.filter(|t| !t.is_empty()).map(PathBuf::from)
    }
}

/// Qt's reading of XDG_PICTURES_DIR in a user-dirs.dirs file (the last such line, with its quotes taken off, $HOME filled in and one
/// trailing slash dropped), ~/Pictures when there's none. The home folder itself is a fine answer.
#[cfg_attr(windows, allow(dead_code))]
pub fn pictures_in(home: &Path, user_dirs: Option<&str>) -> PathBuf {
    let mut value = String::new();
    for line in user_dirs.unwrap_or("").lines() {
        let Some(rest) = line.strip_prefix("XDG_") else { continue };
        // The pattern is ^XDG_(.*)_DIR=(.*)$, greedy
        let Some(at) = rest.rfind("_DIR=") else { continue };
        if &rest[..at] == "PICTURES" {
            value = rest[at + "_DIR=".len()..].to_string();
        }
    }
    if value.chars().count() > 2 && value.starts_with('"') && value.ends_with('"') {
        value = value[1..value.len() - 1].to_string();
    }
    if value.is_empty() {
        return home.join("Pictures");
    }
    if let Some(rest) = value.strip_prefix("$HOME") {
        value = format!("{}{rest}", home.display());
    }
    if value.len() > 1 && value.ends_with('/') {
        value.pop();
    }
    PathBuf::from(value)
}

