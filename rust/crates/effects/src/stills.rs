//! What the photo plugins share about the phone's stills: what a lens can take, the options a set of picks asks for and the folder photos
//! go to unless told otherwise. Plain helpers with no plugin in them, so Photos and Phototrap each use them without using each other.

use crate::shared::py_float;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const EXTENSIONS: [&str; 4] = ["night", "hdr", "bokeh", "auto"];
/// The shutter speeds offered, in whole stops: the fast ones as fractions of a second (the n of 1/n), then the long ones in seconds.
const FAST_N: [u32; 12] = [4000, 2000, 1000, 500, 250, 125, 60, 30, 15, 8, 4, 2];
const LONG_S: [f64; 8] = [1.0, 2.0, 4.0, 8.0, 15.0, 30.0, 60.0, 120.0];
/// What a phone that doesn't say takes for a bracket: the most photos and the most EV away from the middle.
pub const BRACKET_MAX: i64 = 9;
pub const EV_MAX: i64 = 6;
/// The most steps of a focus bracket when the phone doesn't say.
pub const FOCUS_STEPS_MAX: i64 = 30;
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
    /// The fastest shutter the sensor has; 0 when the phone didn't say, which keeps the list to long exposures.
    pub exposure_min_ns: i64,
    /// The most photos in an exposure bracket and the most EV a photo is from the middle (the phone's own, else what older phones take).
    pub bracket_max: i64,
    pub ev_max: i64,
    /// The most steps of a focus bracket.
    pub focus_steps_max: i64,
    /// The nearest the lens focuses, in diopters (1 over metres); 0 when it didn't say.
    pub focus_near_max: f64,
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
            exposure_min_ns: 0, bracket_max: BRACKET_MAX, ev_max: EV_MAX, focus_steps_max: FOCUS_STEPS_MAX, focus_near_max: 0.0,
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
    let Value::Object(map) = json!({"format": "jpeg", "quality": 95, "mode": "normal", "shots": "single", "burst": 5, "bracket_photos": 3, "bracket_gap": 3,
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
        exposures.extend(shutter_speeds(caps));
    }
    Offered { formats, modes, shots, exposures }
}

/// The shutter speeds this sensor takes, in seconds, slowest last. A phone that doesn't say its fastest shutter gets the long ones only.
pub fn shutter_speeds(caps: &StillCaps) -> Vec<f64> {
    let fastest = if caps.exposure_min_ns > 0 { caps.exposure_min_ns as f64 / 1e9 } else { 1.0 };
    let longest = caps.exposure_max_ns as f64 / 1e9;
    let fast = FAST_N.iter().map(|n| 1.0 / f64::from(*n));
    fast.chain(LONG_S).filter(|s| *s >= fastest * 0.999 && *s <= longest * 1.0001).collect()
}

/// How a shutter speed reads: 1/250 s for the fast ones, 8 s for the long ones.
pub fn shutter_n(seconds: f64) -> Option<u32> {
    (seconds < 1.0).then(|| (1.0 / seconds).round() as u32)
}

/// The photos of a focus bracket the phone can take.
pub fn focus_steps_cap(caps: &StillCaps) -> i64 {
    // A phone that says it takes more steps than a burst has them as a bracket of their own; otherwise the burst limit holds
    if caps.focus_steps_max > FOCUS_STEPS_MAX { caps.focus_steps_max } else { caps.burst_max.min(caps.focus_steps_max.max(2)) }
}

/// The counts offered for a focus bracket: every one up to 10, then by 2 to 20, by 5 to 50 and by 10 to 100, up to what the phone takes.
pub fn focus_counts(caps: &StillCaps) -> Vec<i64> {
    let max = focus_steps_cap(caps).max(2);
    let counts = (2..=10).chain((12..=20).step_by(2)).chain((25..=50).step_by(5)).chain((60..=100).step_by(10));
    counts.filter(|n| *n <= max).collect()
}

/// What an older setting for a bracket ("3x2": 3 photos, 2 EV apart) is now: the photos and the gap in thirds of a stop.
pub fn old_bracket(pick: &str) -> Option<(i64, i64)> {
    let (photos, ev) = pick.split_once('x')?;
    Some((photos.parse().ok()?, ev.parse::<i64>().ok()? * 3))
}

/// The photos of an exposure bracket and the gap between them in thirds of a stop, as picked and fitted to what the phone takes.
pub fn bracket_shape(caps: &StillCaps, choices: &Choices) -> (i64, i64) {
    let photos = int(choices, "bracket_photos", 3).clamp(2, caps.bracket_max.max(2));
    let gap = int(choices, "bracket_gap", 3);
    let gaps = bracket_gaps(caps, photos);
    (photos, gaps.iter().copied().find(|g| *g >= gap).or(gaps.last().copied()).unwrap_or(1))
}

/// The gaps offered between photos, in thirds of a stop, that keep the outer photos inside the phone's reach.
pub fn bracket_gaps(caps: &StillCaps, photos: i64) -> Vec<i64> {
    let reach = caps.ev_max.max(1) * 6;
    let gaps: Vec<i64> = (1..=9).filter(|g| (photos - 1) * g <= reach).collect();
    if gaps.is_empty() { vec![1] } else { gaps }
}

/// The EV of each photo of a bracket: centred on the video's exposure, so an odd count has one at 0 and an even count has none.
pub fn bracket_steps(photos: i64, gap: i64) -> Vec<f64> {
    (0..photos).map(|i| ((2 * i - (photos - 1)) * gap) as f64 / 6.0).map(|ev| (ev * 100.0).round() / 100.0).collect()
}

/// How far apart bracket photos are, 1/3 EV to 3 EV.
pub fn gap_text(gap: i64) -> String {
    match (gap / 3, gap % 3) {
        (whole, 0) => whole.to_string(),
        (0, third) => format!("{third}/3"),
        (whole, third) => format!("{whole} {third}/3"),
    }
}

/// The ends of a focus bracket in diopters (near, far): what was picked, kept in order and inside what the lens does.
pub fn focus_range(caps: &StillCaps, choices: &Choices) -> (f64, f64) {
    let closest = caps.focus_near_max;
    let near = number(choices, "focus_near", closest).clamp(0.0, closest.max(0.0));
    let far = number(choices, "focus_far", 0.0).clamp(0.0, near);
    (near, far)
}

/// The steps of the focus scale the Camera card's slider and the range slider share: 0 is infinity, this many is the nearest focus.
pub const FOCUS_SCALE: f64 = 1000.0;

/// The range as the slider has it: where the far end (low) and the near end (high) are on the focus scale.
pub fn focus_handles(caps: &StillCaps, choices: &Choices) -> (f64, f64) {
    let (near, far) = focus_range(caps, choices);
    let along = |d: f64| if caps.focus_near_max > 0.0 { (d / caps.focus_near_max * FOCUS_SCALE).round() } else { 0.0 };
    (along(far), along(near))
}

/// A focus distance as people say it: 25 cm, 1.5 m, infinity.
pub fn distance_parts(diopters: f64) -> Option<(i64, bool)> {
    if diopters < 0.01 {
        return None;
    }
    let metres = 1.0 / diopters;
    Some(if metres < 1.0 { ((metres * 100.0).round() as i64, false) } else { ((metres * 10.0).round() as i64, true) })
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
            let (photos, gap) = bracket_shape(caps, choices);
            opts.insert("bracket_ev".into(), json!(bracket_steps(photos, gap).iter().map(|ev| ev.to_string()).collect::<Vec<_>>().join(",")));
        }
        "focus" => {
            opts.insert("focus_steps".into(), json!(focus_steps_cap(caps).min(int(choices, "focus_steps", 5).max(2))));
            let (near, far) = focus_range(caps, choices);
            if (near, far) != (caps.focus_near_max, 0.0) {
                opts.insert("focus_from".into(), json!(near));
                opts.insert("focus_to".into(), json!(far));
            }
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

