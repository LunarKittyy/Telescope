//! The Photos plugin (plugins/photos.py): full-quality photos taken on the phone through the host's stills service and saved in a
//! folder. What the card offers follows the lens the shown stream is on (RAW, the maker's Night mode, bursts and brackets where it
//! has them); the picks of another lens are kept for when it comes back. A stream with a camera of its own (a DSLR) answers the same
//! service, and then the card also offers the camera's own ISO and shutter speed lists, where its mode allows them. Values that run in
//! order (ISO, shutter speed, how many, a long exposure) step along their list, named choices open one. Every control has its place on the
//! card whatever the lens offers, and is greyed out when there is nothing to pick, so nothing moves when a stream starts.

use telescope_core::i18n::Fields;
use crate::shared::{py_float, Paired};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use telescope_core::actions::{self, Choice};
use telescope_core::event::Event;
use telescope_core::hold::{self, Hold, Tick, TICK_MS};
use telescope_core::host::{FileDialog, FileDialogKind, Host, RequestId};
use telescope_core::i18n::{n_, tr, trn_open};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "photos";
const T_HOLD: &str = "hold";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/photos.slint"));

pub const FORMATS: [(&str, &str); 5] = [("jpeg", n_("JPEG")), ("heic", n_("HEIC")), ("ultrahdr", n_("Ultra HDR")), ("raw", n_("RAW (DNG)")), ("raw+jpeg", n_("RAW and JPEG"))];
pub const MODES: [(&str, &str); 6] = [
    ("normal", n_("Normal")), ("max_res", n_("Full resolution")), ("night", n_("Night")), ("hdr", n_("HDR")), ("bokeh", n_("Bokeh")), ("auto", n_("Auto")),
];
pub const EXTENSIONS: [&str; 4] = ["night", "hdr", "bokeh", "auto"];
pub const SHOTS: [(&str, &str); 4] = [("single", n_("Single")), ("burst", n_("Burst")), ("bracket", n_("Exposure bracket")), ("focus", n_("Focus bracket"))];
/// Exposure brackets: (key, label, EV steps).
pub const BRACKETS: [(&str, &str, &[i32]); 4] = [
    ("3x1", n_("3 photos, 1 EV apart"), &[-1, 0, 1]), ("3x2", n_("3 photos, 2 EV apart"), &[-2, 0, 2]),
    ("5x1", n_("5 photos, 1 EV apart"), &[-2, -1, 0, 1, 2]), ("7x1", n_("7 photos, 1 EV apart"), &[-3, -2, -1, 0, 1, 2, 3]),
];
/// Long exposures offered, up to what the sensor allows.
pub const EXPOSURES_S: [f64; 9] = [0.5, 1.0, 2.0, 4.0, 8.0, 15.0, 30.0, 60.0, 120.0];
pub const TIMERS: [i64; 3] = [0, 3, 10];
pub const DISTORTION: [(&str, &str); 3] = [("camera", n_("Camera")), ("on", n_("On")), ("off", n_("Off"))];
/// What each distortion choice means, for its tooltip.
const DISTORTION_TIPS: [&str; 3] = [n_("Let the camera decide."), n_("Always fix lens distortion."), n_("Never fix lens distortion.")];

const TAKE_TIP: &str = n_("Take a photo on the phone with the settings below.");
const OPEN_TIP: &str = n_("Open the folder with the last photos taken.");
const MODE_TIP: &str = n_("The video pauses while the phone maker's Night, HDR and Bokeh modes take a photo.");
/// Past this many entries a held arrow takes bigger steps after a moment (a list in thirds of a stop is a stop at three).
const LONG_LIST: usize = 12;
const BIG_STEP: usize = 3;
const FOCUS_TIP: &str = n_("Number of photos from the nearest focus to the farthest, for focus stacking.");
const BURST_TIP: &str = n_("Number of photos in the burst.");
const SHOTS_TIP: &str = n_("Take one photo, a burst or a set at different exposures or focus distances.");
const TIMER_TIP: &str = n_("Wait a few seconds before taking the photo.");
const EXPOSURE_TIP: &str = n_("A long exposure keeps the video's ISO and the video stops while the shutter is open.");
const BEST_TIP: &str = n_("Use extra noise reduction and sharpening on the photo only, not the video. Taking the photo takes a little longer.");
const BEST_OFF_TIP: &str = n_("This lens and mode have no extra noise reduction or sharpening to offer");
const QUALITY_CAMERA_TIP: &str = n_("JPEG quality is set by the camera");
const QUALITY_RAW_TIP: &str = n_("RAW photos have no quality setting");
const CAMERA_BUSY: &str = n_("The camera is already taking a photo.");
const CAMERA_FAILED: &str = n_("The camera couldn't take the photo.");

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
                                    "focus_steps": 5, "exposure_s": 0, "timer": 0, "best": true, "distortion": "camera"}) else { unreachable!() };
    map
}

fn text<'a>(choices: &'a Choices, key: &str) -> &'a str {
    choices.get(key).and_then(Value::as_str).unwrap_or("")
}

fn number(choices: &Choices, key: &str, default: f64) -> f64 {
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

fn pick_number(value: f64, allowed: &[f64]) -> f64 {
    allowed.iter().copied().find(|a| *a == value).unwrap_or(allowed[0])
}

/// Python's int(): the number cut towards zero (the picks are never anything else once they've been loaded).
fn int(choices: &Choices, key: &str, default: i64) -> i64 {
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
fn pictures() -> PathBuf {
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
fn known_pictures() -> Option<PathBuf> {
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
fn pictures_in(home: &Path, user_dirs: Option<&str>) -> PathBuf {
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

/// "Like the video" for none, else the seconds (8 s, 0.5 s).
pub fn exposure_text(seconds: f64) -> String {
    if seconds == 0.0 { tr("Like the video") } else { tr("{seconds} s").field("seconds", format!("{seconds}")) }
}

fn index_of(list: &[&str], value: &str) -> usize {
    list.iter().position(|v| *v == value).unwrap_or(0)
}

/// One control of the card that picks from a list. A dial steps along its list (ISO, shutter speed, how many, a long exposure) and
/// the rest are named choices that open their list.
struct Tile {
    key: &'static str,
    label: String,
    dial: bool,
    options: Vec<String>,
    index: usize,
    tip: String,
}

pub struct PhotosPlugin {
    choices: Choices,
    folder: String,
    /// Where photos go until another folder is picked, to tell when one was
    default_folder: String,
    caps: Option<StillCaps>,
    /// What still_caps last answered, to tell when it changed.
    caps_raw: Option<Value>,
    /// What was picked of the camera's own ISO (0) and shutter speed (1) lists, as the camera words them. They aren't saved: the
    /// camera holds them, so they only stand until it reports the same.
    camera_picks: [Option<String>; 2],
    /// The stream the card last showed, which the picks above were made on.
    source: Option<String>,
    busy: bool,
    last_saved: Vec<String>,
    /// The photos being taken (one per phone, each with its own end) and the folder dialog being shown.
    taking: Vec<RequestId>,
    asked: Option<RequestId>,
    status: (String, &'static str),
    paired: Paired,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
    /// The lists the card's controls have now, to tell when one changed (the arrow held down on a list that changes is let go)
    shown_tiles: Option<Vec<Value>>,
    /// The arrow held down on the card, if any
    held: Option<Hold>,
}

impl Default for PhotosPlugin {
    fn default() -> Self {
        PhotosPlugin::new()
    }
}

impl PhotosPlugin {
    pub fn new() -> PhotosPlugin {
        PhotosPlugin {
            choices: defaults(),
            folder: default_folder(),
            default_folder: default_folder(),
            caps: None,
            caps_raw: None,
            camera_picks: [None, None],
            source: None,
            busy: false,
            last_saved: Vec::new(),
            taking: Vec::new(),
            asked: None,
            status: (String::new(), "status_dim"),
            paired: Paired::default(),
            pending: false,
            shown_tiles: None,
            held: None,
        }
    }

    fn effective_caps(&self) -> StillCaps {
        self.caps.clone().unwrap_or_default()
    }

    fn can(&self) -> Offered {
        offered(&self.effective_caps(), &self.choices)
    }

    /// A camera's own ISO (0) or shutter speed (1) list, when it lets them be picked now.
    fn camera_setting(&self, which: usize) -> Option<&CameraSetting> {
        let caps = self.caps.as_ref().filter(|c| c.camera)?;
        [&caps.iso, &caps.shutter][which].as_ref().filter(|s| s.values.len() > 1)
    }

    /// Which entry of a camera's list the card shows: the pick, else where the camera is.
    fn camera_index(&self, which: usize) -> usize {
        let Some(setting) = self.camera_setting(which) else { return 0 };
        self.camera_picks[which].as_ref().and_then(|p| setting.values.iter().position(|v| v == p)).or(setting.current).unwrap_or(0)
    }

    /// A pick the camera now agrees with, or no longer lists, is dropped, so the card follows the camera again.
    fn settle_camera_picks(&mut self) {
        for which in 0..2 {
            let keep = match (&self.camera_picks[which], self.camera_setting(which)) {
                (Some(pick), Some(setting)) => setting.values.contains(pick) && setting.current.and_then(|i| setting.values.get(i)) != Some(pick),
                _ => false,
            };
            if !keep {
                self.camera_picks[which] = None;
            }
        }
    }

    /// Whether the card is for what's shown: always for a phone, and for another source only while it's a camera that takes photos.
    fn card_shown(&self) -> bool {
        !self.paired.source() || self.caps.as_ref().is_some_and(|c| c.camera)
    }

    fn camera_words(&self, text: &'static str, phone: &'static str) -> String {
        tr(if self.caps.as_ref().is_some_and(|c| c.camera) { text } else { phone })
    }

    // ── What the card shows ───────────────────────────────────────────

    fn fixed_props() -> Value {
        json!({
            "take-text": tr("Take photo"), "take-tip": tr(TAKE_TIP), "open-text": tr("Open folder"), "open-tip": tr(OPEN_TIP),
            "best-name": tr("Best quality"), "distortion-tips": DISTORTION_TIPS.map(tr),
            "shots-names": SHOTS.map(|(_, name)| tr(name)), "mode-tip": tr(MODE_TIP), "shots-tip": tr(SHOTS_TIP), "timer-tip": tr(TIMER_TIP), "exposure-tip": tr(EXPOSURE_TIP),
            "previous-label": tr("Previous"), "next-label": tr("Next"),
            "timer-items": TIMERS.iter().map(|s| Self::timer_text(*s)).collect::<Vec<_>>(),
        })
    }

    fn timer_text(seconds: i64) -> String {
        if seconds == 0 { tr("Off") } else { tr("{seconds} s").field("seconds", seconds.to_string()) }
    }

    fn labelled(list: &[&str], labels: &[(&str, &str)]) -> Vec<String> {
        list.iter().map(|k| labels.iter().find(|(key, _)| key == k).map_or_else(|| k.to_string(), |(_, t)| tr(t))).collect()
    }

    /// The range of the How many (0) and Steps (1) boxes.
    fn number_range(&self, which: usize) -> (i64, i64) {
        let burst_max = self.effective_caps().burst_max;
        if which == 0 { (2, burst_max.max(2)) } else { (2, burst_max.clamp(2, 30)) }
    }

    /// The tiles for what this lens offers, in the order they're shown: the camera's own dials first.
    fn tile_list(&self) -> Vec<Tile> {
        let caps = self.effective_caps();
        let can = self.can();
        let c = &self.choices;
        let mode = pick(text(c, "mode"), &can.modes);
        let format = pick(text(c, "format"), &can.formats);
        let shots = pick(text(c, "shots"), &can.shots);
        let extension = EXTENSIONS.contains(&mode);
        let exposure = pick_number(number(c, "exposure_s", 0.0), &can.exposures);
        let tile = |key, label: String, dial, options: Vec<String>, index| Tile { key, label, dial, options, index, tip: String::new() };
        let mut tiles = Vec::new();
        for (which, key, label) in [(0, "iso", tr("ISO")), (1, "shutter", tr("Shutter"))] {
            if let Some(setting) = self.camera_setting(which) {
                tiles.push(tile(key, label, true, setting.labels.clone(), self.camera_index(which)));
            }
        }
        if can.modes.len() > 1 {
            tiles.push(Tile { tip: tr(MODE_TIP), ..tile("mode", tr("Mode"), false, Self::labelled(&can.modes, &MODES), index_of(&can.modes, mode)) });
        }
        if can.formats.len() > 1 {
            tiles.push(tile("format", tr("Format"), false, Self::labelled(&can.formats, &FORMATS), index_of(&can.formats, format)));
        }
        if can.shots.len() > 1 {
            tiles.push(tile("shots", tr("Shots"), false, Self::labelled(&can.shots, &SHOTS), index_of(&can.shots, shots)));
        }
        for (which, key, label, tip) in [(0, "burst", n_("How many"), BURST_TIP), (1, "focus_steps", n_("Steps"), FOCUS_TIP)] {
            if shots == ["burst", "focus"][which] {
                let (min, max) = self.number_range(which);
                let options = (min..=max).map(|n| n.to_string()).collect();
                let index = int(c, key, 5).clamp(min, max) - min;
                tiles.push(Tile { tip: tr(tip), ..tile(key, tr(label), true, options, index as usize) });
            }
        }
        if shots == "bracket" {
            let options = BRACKETS.iter().map(|(_, t, _)| tr(t)).collect();
            let index = BRACKETS.iter().position(|(k, ..)| *k == text(c, "bracket")).unwrap_or(0);
            tiles.push(tile("bracket", tr("Bracket"), true, options, index));
        }
        if can.exposures.len() > 1 {
            let options = can.exposures.iter().map(|s| exposure_text(*s)).collect();
            let index = can.exposures.iter().position(|s| *s == exposure).unwrap_or(0);
            tiles.push(Tile { tip: tr(EXPOSURE_TIP), ..tile("exposure_s", tr("Exposure"), true, options, index) });
        }
        if !extension && caps.distortion {
            let options = DISTORTION.iter().map(|(_, t)| tr(t)).collect();
            let index = DISTORTION.iter().position(|(k, _)| *k == text(c, "distortion")).unwrap_or(0);
            tiles.push(tile("distortion", tr("Distortion fix"), false, options, index));
        }
        tiles
    }

    /// The controls for what this lens offers, keeping the picks it can't do for another lens (_show_choices). Every control has props
    /// whatever the lens offers: a list that is empty, or a control that says it can't be used, is greyed out in its place.
    fn choice_props(&self) -> Value {
        let caps = self.effective_caps();
        let can = self.can();
        let c = &self.choices;
        let mode = pick(text(c, "mode"), &can.modes);
        let format = pick(text(c, "format"), &can.formats);
        let shots = pick(text(c, "shots"), &can.shots);
        let extension = EXTENSIONS.contains(&mode);
        let quality = int(c, "quality", 95).clamp(50, 100);
        let best = !extension && (caps.noise_reduction_hq || caps.sharpening_hq || caps.hot_pixel_hq);
        let quality_tip = if caps.camera {
            tr(QUALITY_CAMERA_TIP)
        } else if format == "raw" {
            tr(QUALITY_RAW_TIP)
        } else {
            String::new()
        };
        let tiles = self.tile_list();
        let find = |key: &str| tiles.iter().find(|t| t.key == key);
        let options = |key: &str| find(key).map_or_else(Vec::new, |t| t.options.clone());
        let index = |key: &str| find(key).map_or(0, |t| t.index);
        // What the slot under Shots holds: how many, how many steps or which bracket, by the shots picked
        let slot = match shots {
            "burst" => "burst",
            "focus" => "focus_steps",
            "bracket" => "bracket",
            _ => "",
        };
        json!({
            "iso-options": options("iso"), "iso-index": index("iso"), "shutter-options": options("shutter"), "shutter-index": index("shutter"),
            "mode-options": Self::labelled(&can.modes, &MODES), "mode-index": index_of(&can.modes, mode), "mode-enabled": can.modes.len() > 1,
            "format-options": Self::labelled(&can.formats, &FORMATS), "format-index": index_of(&can.formats, format), "format-enabled": can.formats.len() > 1,
            "shots-offered": SHOTS.map(|(key, _)| can.shots.contains(&key)), "shots-index": SHOTS.iter().position(|(key, _)| *key == shots).unwrap_or(0),
            "slot-key": slot, "slot-label": find(slot).map_or_else(String::new, |t| t.label.clone()), "slot-options": options(slot),
            "slot-index": index(slot), "slot-tip": find(slot).map_or_else(String::new, |t| t.tip.clone()),
            "exposure-options": options("exposure_s"), "exposure-index": index("exposure_s"),
            "distortion-enabled": find("distortion").is_some(), "distortion-index": index("distortion"),
            "quality": quality, "quality-text": quality.to_string(), "quality-enabled": format != "raw" && !caps.camera, "quality-tip": quality_tip,
            "timer-current": TIMERS.iter().position(|t| *t as f64 == number(c, "timer", 0.0)).unwrap_or(0),
            "best": c.get("best") == Some(&json!(true)), "best-enabled": best, "best-tip": tr(if best { BEST_TIP } else { BEST_OFF_TIP }),
            "folder-name": Path::new(&self.folder).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| self.folder.clone()),
            "folder-tip": self.folder, "folder-changed": self.folder != self.default_folder,
        })
    }

    /// The lists the card's controls have, which a held arrow has to see unchanged.
    fn lists(&self) -> Vec<Value> {
        self.tile_list().iter().map(|t| json!({"key": t.key, "options": t.options})).collect()
    }

    /// The Take photo button, the note under it and Open folder (_refresh).
    fn state_props(&self, host: &dyn Host) -> Value {
        // Not streaming yet is no news: the button is just off, and its tip says why
        let waiting = self.caps.is_none() && !host.is_streaming();
        let tip = if waiting {
            tr("Start streaming to take photos.")
        } else if self.caps.is_none() {
            tr("This phone can't take photos from Telescope yet. Update the app on the phone.")
        } else {
            tr(TAKE_TIP)
        };
        json!({
            "active": self.paired.on(), "shown": self.card_shown(),
            "take-enabled": self.caps.is_some() && !self.busy, "take-tip": tip, "open-enabled": !self.last_saved.is_empty(),
        })
    }

    fn status_props(&self) -> Value {
        json!({"status": self.status.0, "status-kind": self.status.1})
    }

    fn all_props(&mut self, host: &dyn Host) -> Value {
        let mut all = Self::fixed_props();
        let choices = self.choice_props();
        self.shown_tiles = Some(self.lists());
        for part in [choices, self.state_props(host), self.status_props()] {
            if let (Value::Object(a), Value::Object(p)) = (&mut all, part) {
                a.extend(p);
            }
        }
        all
    }

    fn show_status(&mut self, host: &mut dyn Host, text: String, kind: &'static str) {
        self.status = (text, kind);
        host.set_ui_props("card", &self.status_props());
    }

    /// What changed in the picks. A list that changed under an arrow held down lets the arrow go first: it would step along a list it
    /// no longer has.
    fn show_choices(&mut self, host: &mut dyn Host) {
        let props = self.choice_props();
        let lists = Some(self.lists());
        if lists != self.shown_tiles {
            self.release_hold(host);
            self.shown_tiles = lists;
        }
        host.set_ui_props("card", &props);
    }

    /// Brings the card up to date with the shown stream's lens and whether a photo is being taken.
    fn refresh(&mut self, host: &mut dyn Host) {
        // What was picked for the next photo is the camera's it was picked on: another camera starts clean, whatever its lists say
        let mut other_camera = false;
        if let Some(id) = host.focused_source_id() {
            if self.source.as_ref() != Some(&id) {
                other_camera = self.camera_picks.iter().any(Option::is_some);
                self.source = Some(id);
                self.camera_picks = [None, None];
                self.release_hold(host);
            }
        }
        let raw = host.still_caps(None);
        if raw == self.caps_raw && other_camera {
            self.show_choices(host);
        }
        if raw != self.caps_raw {
            self.caps = raw.as_ref().and_then(|v| serde_json::from_value(v.clone()).ok());
            self.caps_raw = raw;
            self.settle_camera_picks();
            self.show_choices(host);
        }
        self.busy = host.still_busy(None);
        host.set_ui_props("card", &self.state_props(host));
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            self.refresh(host);
            // All the tiles go out again
            self.release_hold(host);
            let props = self.all_props(host);
            host.set_ui_props("card", &props);
        }
    }

    // ── Picks ─────────────────────────────────────────────────────────

    fn same(a: Option<&Value>, b: &Value) -> bool {
        match (a, b) {
            (Some(Value::Number(x)), Value::Number(y)) => x.as_f64() == y.as_f64(),
            (Some(x), y) => x == y,
            (None, _) => false,
        }
    }

    fn pick_value(&mut self, host: &mut dyn Host, key: &str, value: Value) {
        if value.is_null() || Self::same(self.choices.get(key), &value) {
            return;
        }
        self.choices.insert(key.into(), value);
        self.show_choices(host);
        host.schedule_save();
    }

    /// A pick from one of the tiles: the index in the list the card is showing.
    fn pick_index(&mut self, host: &mut dyn Host, key: &str, index: usize) {
        let can = self.can();
        let value = match key {
            "mode" => can.modes.get(index).map(|v| json!(v)),
            "format" => can.formats.get(index).map(|v| json!(v)),
            "shots" => SHOTS.get(index).map(|(k, _)| k).filter(|k| can.shots.contains(k)).map(|k| json!(k)),
            "exposure_s" => can.exposures.get(index).map(|s| if s.fract() == 0.0 { json!(*s as i64) } else { json!(s) }),
            "bracket" => BRACKETS.get(index).map(|(k, ..)| json!(k)),
            "distortion" => DISTORTION.get(index).map(|(k, _)| json!(k)),
            "burst" | "focus_steps" => {
                let (min, max) = self.number_range(usize::from(key == "focus_steps"));
                Some(min + index as i64).filter(|n| *n <= max).map(|n| json!(n))
            }
            // The camera's own lists: picked for the next photo, and not saved
            "iso" | "shutter" => {
                let which = usize::from(key == "shutter");
                let pick = self.camera_setting(which).and_then(|s| s.values.get(index)).cloned();
                if pick.is_some() {
                    self.camera_picks[which] = pick;
                    self.show_choices(host);
                }
                return;
            }
            _ => None,
        };
        if let Some(value) = value {
            self.pick_value(host, key, value);
        }
    }

    /// One step of a dial, `by` entries toward the end of its list (dir) or its start; false when it was already there.
    fn step_tile(&mut self, host: &mut dyn Host, key: &str, dir: i32, by: usize) -> bool {
        let tiles = self.tile_list();
        let Some(tile) = tiles.iter().find(|t| t.key == key && t.dial) else { return false };
        match hold::stepped(tile.index, tile.options.len(), dir, by) {
            Some(to) => {
                self.pick_index(host, key, to);
                true
            }
            None => false,
        }
    }

    /// A tick of the arrow held down: another step, bigger after a moment on a long list. Nothing here waits on anything, the picks are
    /// only for the next photo.
    fn hold_tick(&mut self, host: &mut dyn Host) {
        let Some(mut held) = self.held.take() else { return };
        let long = self.tile_list().iter().any(|t| t.key == held.name && t.options.len() > LONG_LIST);
        match held.tick(true) {
            Tick::Release => return,
            Tick::Wait => {}
            Tick::Step { big } => {
                if !self.step_tile(host, &held.name, held.dir, if big && long { BIG_STEP } else { 1 }) {
                    return;
                }
            }
        }
        self.held = Some(held);
        host.schedule(TICK_MS, T_HOLD);
    }

    fn release_hold(&mut self, host: &mut dyn Host) {
        self.held = None;
        host.cancel_schedule(T_HOLD);
    }

    // ── Taking one ────────────────────────────────────────────────────

    fn rotation(host: &dyn Host) -> i32 {
        host.view().rotation.filter(|r| [90, 180, 270].contains(r)).unwrap_or(0)
    }

    fn take(&mut self, host: &mut dyn Host) {
        let Some(raw) = host.still_caps(None) else { return };
        if host.still_busy(None) {
            return;
        }
        let caps: StillCaps = serde_json::from_value(raw).unwrap_or_default();
        let mut options = build_options(&caps, &self.choices, Self::rotation(host));
        if caps.camera {
            camera_options(&mut options, self.camera_picks[0].as_deref(), self.camera_picks[1].as_deref());
        }
        match host.take_still(&options, &self.folder, None) {
            Some(id) => {
                self.taking.push(id);
                self.show_status(host, tr("Taking the photo…"), "status_dim");
                self.refresh(host);
            }
            None => {
                let text = self.camera_words(CAMERA_BUSY, "The phone is already taking a photo.");
                self.show_status(host, text, "status_err");
            }
        }
    }

    fn progress(&mut self, host: &mut dyn Host, p: &Value) {
        let int = |key: &str| p.get(key).and_then(Value::as_i64).unwrap_or(0);
        let (state, shots, taken, countdown) = (p.get("state").and_then(Value::as_str).unwrap_or(""), int("shots"), int("taken"), int("countdown_ms"));
        let text = if state == "waiting" && countdown > 0 {
            tr("Taking the photo in {seconds} s").field("seconds", ((countdown + 999) / 1000).max(1).to_string())
        } else if shots > 1 {
            tr("Photo {n} of {total}").field("n", shots.min(taken + 1).to_string()).field("total", shots.to_string())
        } else if p.get("paused").and_then(Value::as_bool) == Some(true) {
            tr("Taking the photo, the video is paused…")
        } else {
            tr("Taking the photo…")
        };
        self.show_status(host, text, "status_dim");
    }

    fn done(&mut self, host: &mut dyn Host, result: &Value) {
        let files: Vec<String> = result.get("files").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
        self.last_saved = files.clone();
        if result.get("ok").and_then(Value::as_bool) == Some(true) && !files.is_empty() {
            let name = Path::new(&files[0]).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let more = files.len() - 1;
            let text = if more > 0 {
                trn_open("Saved {name} and {n} more", "Saved {name} and {n} more", more as i64).field("name", &name)
            } else {
                tr("Saved {name}").field("name", &name)
            };
            self.show_status(host, text.clone(), "status_ok");
            // Taken with a global shortcut from another app
            if !host.window_active() {
                host.send_notification(&tr("Photo saved"), &text, false);
            }
        } else {
            let error = result.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()).map_or_else(|| self.camera_words(CAMERA_FAILED, "The phone couldn't take the photo."), String::from);
            self.show_status(host, error, "status_err");
        }
        self.refresh(host);
    }

    /// The files of a photo that came late: they are saved, and the person is told even with the window in front, since they stopped waiting.
    fn late(&mut self, host: &mut dyn Host, data: &Value) {
        let files: Vec<String> = data.get("files").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
        let Some(first) = files.first() else { return };
        let name = Path::new(first).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let more = files.len() - 1;
        let text = if more > 0 {
            trn_open("The photos arrived late. Saved {name} and {n} more", "The photos arrived late. Saved {name} and {n} more", more as i64).field("name", &name)
        } else {
            tr("The photo arrived late. Saved {name}").field("name", &name)
        };
        self.last_saved = files;
        self.show_status(host, text.clone(), "status_ok");
        host.send_notification(&tr("Photo saved"), &text, false);
        self.refresh(host);
    }

    fn open_folder(&self, host: &mut dyn Host) {
        let folder = self.last_saved.first().and_then(|f| Path::new(f).parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from(&self.folder));
        host.open_url(&telescope_core::system::file_url(&folder));
    }

    fn choose_folder(&mut self, host: &mut dyn Host) {
        let dialog = FileDialog { kind: FileDialogKind::Folder, title: tr("Save photos in"), start: self.folder.clone(), filters: Vec::new() };
        self.asked = host.pick_file(&dialog);
    }

    // ── Shortcuts ─────────────────────────────────────────────────────

    fn timer_options() -> Vec<(String, String)> {
        TIMERS.iter().map(|s| (s.to_string(), Self::timer_text(*s))).collect()
    }
}

impl Plugin for PhotosPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Photos").into(),
            about: n_("Take full-quality photos on the phone or a DSLR, including RAW if the camera supports it.").into(),
            when_off: n_("Without the Photos plugin there's no Photos card and no Take photo shortcut.").into(),
            category: "camera".into(),
            icon: "photo".into(),
            optional: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.all_props(host));
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
        self.refresh(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.release_hold(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Event::TimerFired { owner, token } = event {
            if owner == NAME && token == T_HOLD {
                self.hold_tick(host);
            }
        }
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on, "shown": self.card_shown()}));
        }
        match event {
            Event::PhoneStateUpdated { .. } | Event::StreamStopped | Event::DeviceChanged { .. } | Event::CameraSwitched { .. }
            | Event::StillBusyChanged { .. } | Event::StillCapsChanged { .. } | Event::SourceSelected { .. }
            // A camera that streams again may take photos again, whether or not it said so while nobody listened
            | Event::StreamConnected { .. } | Event::StreamLost { .. } => {
                // The tile under a held arrow may be gone with the camera
                self.refresh(host);
                if self.held.as_ref().is_some_and(|h| !self.tile_list().iter().any(|t| t.key == h.name)) {
                    self.release_hold(host);
                }
            }
            // A photo that came after its request had ended (a very long exposure): the camera saved it and says so
            Event::RequestProgress { data, .. } if data.get("state").and_then(Value::as_str) == Some("late") => self.late(host, data),
            Event::RequestProgress { request, data } if self.taking.contains(request) => self.progress(host, data),
            Event::RequestDone { request, data, .. } if self.taking.contains(request) => {
                self.taking.retain(|r| r != request);
                self.done(host, data);
            }
            Event::RequestDone { request, ok, data, .. } if Some(*request) == self.asked => {
                self.asked = None;
                if let (true, Some(path)) = (*ok, data.get("path").and_then(Value::as_str).filter(|p| !p.is_empty())) {
                    self.folder = path.into();
                    self.show_choices(host);
                    host.schedule_save();
                }
            }
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        let mut props = Self::fixed_props();
        let choices = self.choice_props();
        self.shown_tiles = Some(self.lists());
        for part in [choices, json!({"active": self.paired.on(), "shown": true, "take-enabled": false,
                                                  "take-tip": tr("Start streaming to take photos."), "open-enabled": false})] {
            if let (Value::Object(a), Value::Object(p)) = (&mut props, part) {
                a.extend(p);
            }
        }
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "PhotosCard".into(), props }, region: "right".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let index = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0).max(0.0) as usize;
        match event.callback.as_str() {
            "take-clicked" => self.take(host),
            "open-clicked" => self.open_folder(host),
            "tile-picked" => self.pick_index(host, event.args.first().and_then(Value::as_str).unwrap_or(""), index(1)),
            "tile-step" => {
                let (key, by) = (event.args.first().and_then(Value::as_str).unwrap_or(""), event.args.get(1).and_then(Value::as_f64).unwrap_or(0.0));
                self.step_tile(host, key, if by < 0.0 { -1 } else { 1 }, 1);
            }
            "tile-hold-down" => {
                let (key, dir) = (event.args.first().and_then(Value::as_str).unwrap_or("").to_string(), event.args.get(1).and_then(Value::as_f64).unwrap_or(0.0));
                let dir = if dir < 0.0 { -1 } else { 1 };
                // One step as it goes down; the timer takes over if it stays down
                self.release_hold(host);
                if self.step_tile(host, &key, dir, 1) {
                    self.held = Some(Hold::new(&key, dir));
                    host.schedule(TICK_MS, T_HOLD);
                }
            }
            "tile-hold-up" => self.release_hold(host),
            "quality-moved" => {
                let value = event.args.first().and_then(Value::as_f64).unwrap_or(95.0).round() as i64;
                host.set_ui_props("card", &json!({"quality-text": value.to_string()}));
                self.pick_value(host, "quality", json!(value));
            }
            "timer-picked" => {
                if let Some(seconds) = TIMERS.get(index(0)) {
                    self.pick_value(host, "timer", json!(seconds));
                }
            }
            "best-toggled" => self.pick_value(host, "best", json!(event.args.first().and_then(Value::as_bool).unwrap_or(false))),
            "folder-clicked" => self.choose_folder(host),
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Photos");
        let modes: Vec<(&str, String)> = MODES.iter().map(|(k, t)| (*k, tr(t))).collect();
        let mut mode = actions::choice_action("photos.mode", &tr("Photo mode"), &group, "", &modes);
        // What the lens offers changes, and a saved choice it lacks right now is kept
        mode.params[1]["dynamic"] = json!(true);
        let timers = Self::timer_options();
        let timer_options: Vec<(&str, String)> = timers.iter().map(|(v, t)| (v.as_str(), t.clone())).collect();
        let mut timer = actions::choice_action("photos.timer", &tr("Photo timer"), &group, "", &timer_options);
        timer.params[1]["dynamic"] = json!(true);
        let take = ActionSpec { id: "photos.take".into(), label: tr("Take photo"), group, params: Vec::new(), repeats: false, on_release: false, current: None, keywords: String::new() };
        vec![take, mode, timer]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        // The card is out of use until a phone is paired, or a camera of its own is the source
        if !self.paired.on() || !self.card_shown() {
            return None;
        }
        match id {
            "photos.take" => {
                if self.caps.is_none() || self.busy {
                    return None;
                }
                self.take(host);
                Some(tr("Take photo"))
            }
            "photos.mode" => {
                let can = self.can();
                // The drop-down is hidden with one mode, and a hidden control isn't ready
                if can.modes.len() < 2 {
                    return None;
                }
                let shown = Self::labelled(&can.modes, &MODES);
                let options: Vec<Choice> = can.modes.iter().zip(&shown).map(|(v, s)| Choice { value: v, shown: s, enabled: true }).collect();
                let current = index_of(&can.modes, pick(text(&self.choices, "mode"), &can.modes));
                let (said, pick) = actions::choice_target(params, &options, Some(current), true)?;
                if pick {
                    self.pick_index(host, "mode", said);
                }
                Some(actions::slider_text(&tr("Photo mode"), &shown[said]))
            }
            "photos.timer" => {
                let timers = Self::timer_options();
                let options: Vec<Choice> = timers.iter().map(|(v, s)| Choice { value: v, shown: s, enabled: true }).collect();
                let current = TIMERS.iter().position(|t| *t as f64 == number(&self.choices, "timer", 0.0));
                let (said, pick) = actions::choice_target(params, &options, current, true)?;
                if pick {
                    self.pick_value(host, "timer", json!(TIMERS[said]));
                }
                Some(actions::slider_text(&tr("Photo timer"), &timers[said].1))
            }
            _ => None,
        }
    }

    fn get_config(&self) -> Value {
        let mut cfg = self.choices.clone();
        cfg.insert("folder".into(), json!(self.folder));
        Value::Object(cfg)
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let Value::Object(cfg) = cfg else { return Err(ConfigRejected("settings".into())) };
        let mut choices = defaults();
        for (key, default) in defaults() {
            let Some(value) = cfg.get(&key) else { continue };
            let kept = match default {
                Value::Bool(_) => value.is_boolean(),
                Value::Number(_) => value.is_number(),
                _ => value.is_string(),
            };
            if kept {
                choices.insert(key, value.clone());
            }
        }
        if !TIMERS.iter().any(|t| *t as f64 == number(&choices, "timer", 0.0)) {
            choices.insert("timer".into(), json!(0));
        }
        self.choices = choices;
        self.folder = cfg.get("folder").and_then(Value::as_str).filter(|f| !f.is_empty()).map_or_else(default_folder, String::from);
        self.pending = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let Some(caps) = &self.caps else { return vec![("Photos".into(), "not available".into())] };
        let mut have: Vec<String> = vec!["jpeg".into()];
        have.extend(caps.formats.iter().filter(|f| *f != "jpeg").cloned());
        if caps.max_res.is_some() {
            have.push("full resolution".into());
        }
        have.extend(caps.extensions.iter().cloned());
        if caps.camera {
            // The camera's own lists, as far as its mode gives them now
            let lists: Vec<&str> = [("ISO", &caps.iso), ("shutter speed", &caps.shutter)].iter().filter(|(_, s)| s.is_some()).map(|(name, _)| *name).collect();
            let formats = if caps.formats.iter().any(|f| f == "raw") { "jpeg, raw" } else { "jpeg" };
            return vec![("Photos".into(), format!("camera, {formats}")), ("Photo settings".into(), if lists.is_empty() { "none".into() } else { lists.join(", ") })];
        }
        let sizes: Vec<String> = offered(caps, &Map::new()).modes.iter().filter_map(|m| caps.size_for(m).map(|(w, h)| format!("{m} {w}x{h}"))).collect();
        vec![("Photos".into(), have.join(", ")), ("Photo sizes".into(), if sizes.is_empty() { "unknown".into() } else { sizes.join(", ") })]
    }
}

#[cfg(test)]
#[path = "photos_tests.rs"]
mod tests;
