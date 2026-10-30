//! The Photos plugin (plugins/photos.py): full-quality photos taken on the phone through the host's stills service and saved in a
//! folder. What the card offers follows the lens the shown stream is on (RAW, the maker's Night mode, bursts and brackets where it
//! has them); the picks of another lens are kept for when it comes back.

use crate::shared::{py_float, Paired};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use telescope_core::actions::{self, Choice};
use telescope_core::event::Event;
use telescope_core::host::{FileDialog, FileDialogKind, Host, RequestId};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "photos";
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
pub const DISTORTION: [(&str, &str); 3] = [("camera", n_("As the camera does it")), ("on", n_("On")), ("off", n_("Off"))];

const TAKE_TIP: &str = n_("Take a photo on the phone with the settings below");
const OPEN_TIP: &str = n_("Show the photos taken last");
const MODE_TIP: &str = n_("The phone maker's own Night, HDR and Bokeh modes pause the video while they shoot");
const FOCUS_TIP: &str = n_("Photos from the nearest focus to the farthest, for focus stacking");
const EXPOSURE_TIP: &str = n_("A long exposure keeps the video's ISO and the video stops while the shutter is open");
const BEST_TIP: &str = n_("Takes a little longer and only changes the photo, not the video");
const FOLDER_TIP: &str = n_("Where photos are saved");

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
}

impl Default for StillCaps {
    fn default() -> StillCaps {
        StillCaps {
            formats: vec!["jpeg".into()], size: None, max_res: None, max_res_raw: false, extensions: Vec::new(), exposure_max_ns: 0,
            bracketing: false, focus_bracketing: false, noise_reduction_hq: false, sharpening_hq: false, hot_pixel_hq: false,
            distortion: false, burst_max: 1, extension_sizes: Vec::new(),
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

/// Where photos go unless told otherwise: Pictures/Telescope (QStandardPaths' PicturesLocation, or the home folder when that is
/// empty).
pub fn default_folder() -> String {
    pictures().join("Telescope").display().to_string()
}

fn pictures() -> PathBuf {
    let home = std::env::home_dir().unwrap_or_default();
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or(home).join("Pictures")
    }
    #[cfg(not(windows))]
    {
        let config = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
        let from_file = std::fs::read_to_string(config.join("user-dirs.dirs")).ok().and_then(|text| {
            let line = text.lines().map(str::trim).find(|l| l.starts_with("XDG_PICTURES_DIR="))?;
            let value = line.split_once('=')?.1.trim().trim_matches('"');
            let value = value.strip_prefix("$HOME").map_or_else(|| PathBuf::from(value), |rest| home.join(rest.trim_start_matches('/')));
            Some(value).filter(|p| p != &home)
        });
        from_file.unwrap_or_else(|| home.join("Pictures"))
    }
}

/// "Like the video" for none, else the seconds (8 s, 0.5 s).
pub fn exposure_text(seconds: f64) -> String {
    if seconds == 0.0 { tr("Like the video") } else { tr("{seconds} s").replace("{seconds}", &format!("{seconds}")) }
}

/// A file or folder as a link the browser opens.
fn file_url(path: &Path) -> String {
    let mut out = String::from(if cfg!(windows) { "file:///" } else { "file://" });
    let text = path.display().to_string().replace('\\', "/");
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn index_of(list: &[&str], value: &str) -> usize {
    list.iter().position(|v| *v == value).unwrap_or(0)
}

/// The kinds of choice the card sends back: which list a pick came from.
const PICKS: [&str; 6] = ["mode", "format", "shots", "exposure_s", "bracket", "distortion"];

pub struct PhotosPlugin {
    choices: Choices,
    folder: String,
    caps: Option<StillCaps>,
    /// What still_caps last answered, to tell when it changed.
    caps_raw: Option<Value>,
    busy: bool,
    last_saved: Vec<String>,
    /// The photos being taken (one per phone, each with its own end) and the folder dialog being shown.
    taking: Vec<RequestId>,
    asked: Option<RequestId>,
    status: (String, &'static str),
    paired: Paired,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
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
            caps: None,
            caps_raw: None,
            busy: false,
            last_saved: Vec::new(),
            taking: Vec::new(),
            asked: None,
            status: (String::new(), "status_dim"),
            paired: Paired::default(),
            pending: false,
        }
    }

    fn effective_caps(&self) -> StillCaps {
        self.caps.clone().unwrap_or_default()
    }

    fn can(&self) -> Offered {
        offered(&self.effective_caps(), &self.choices)
    }

    // ── What the card shows ───────────────────────────────────────────

    fn fixed_props() -> Value {
        json!({
            "take-text": tr("Take photo"), "take-tip": tr(TAKE_TIP), "open-text": tr("Open folder"), "open-tip": tr(OPEN_TIP),
            "mode-tip": tr(MODE_TIP), "focus-tip": tr(FOCUS_TIP), "exposure-tip": tr(EXPOSURE_TIP), "best-name": tr("Best quality noise reduction and sharpening"),
            "best-tip": tr(BEST_TIP), "folder-tip-text": tr(FOLDER_TIP), "change-text": tr("Change…"),
            "bracket-items": BRACKETS.iter().map(|(_, t, _)| tr(t)).collect::<Vec<_>>(),
            "distortion-items": DISTORTION.iter().map(|(_, t)| tr(t)).collect::<Vec<_>>(),
            "timer-items": TIMERS.iter().map(|s| Self::timer_text(*s)).collect::<Vec<_>>(),
            "reset-hint": tr("Double-click to reset"),
        })
    }

    fn timer_text(seconds: i64) -> String {
        if seconds == 0 { tr("Off") } else { tr("{seconds} s").replace("{seconds}", &seconds.to_string()) }
    }

    fn labelled(list: &[&str], labels: &[(&str, &str)]) -> Vec<String> {
        list.iter().map(|k| labels.iter().find(|(key, _)| key == k).map_or_else(|| k.to_string(), |(_, t)| tr(t))).collect()
    }

    /// The controls for what this lens offers, keeping the picks it can't do for another lens (_show_choices).
    fn choice_props(&self) -> Value {
        let caps = self.effective_caps();
        let can = self.can();
        let c = &self.choices;
        let mode = pick(text(c, "mode"), &can.modes);
        let format = pick(text(c, "format"), &can.formats);
        let shots = pick(text(c, "shots"), &can.shots);
        let extension = EXTENSIONS.contains(&mode);
        let exposure = pick_number(number(c, "exposure_s", 0.0), &can.exposures);
        let quality = int(c, "quality", 95).clamp(50, 100);
        let burst_max = caps.burst_max.max(2);
        let focus_max = caps.burst_max.min(30).max(2);
        json!({
            "mode-items": Self::labelled(&can.modes, &MODES), "mode-current": index_of(&can.modes, mode), "mode-shown": can.modes.len() > 1,
            "format-items": Self::labelled(&can.formats, &FORMATS), "format-current": index_of(&can.formats, format), "format-shown": can.formats.len() > 1,
            "quality": quality, "quality-text": quality.to_string(), "quality-shown": format != "raw",
            "shots-items": Self::labelled(&can.shots, &SHOTS), "shots-current": index_of(&can.shots, shots), "shots-shown": can.shots.len() > 1,
            "burst-text": int(c, "burst", 5).clamp(2, burst_max).to_string(), "burst-shown": shots == "burst",
            "bracket-current": BRACKETS.iter().position(|(k, ..)| *k == text(c, "bracket")).unwrap_or(0), "bracket-shown": shots == "bracket",
            "focus-text": int(c, "focus_steps", 5).clamp(2, focus_max).to_string(), "focus-shown": shots == "focus",
            "exposure-items": can.exposures.iter().map(|s| exposure_text(*s)).collect::<Vec<_>>(),
            "exposure-current": can.exposures.iter().position(|s| *s == exposure).unwrap_or(0), "exposure-shown": can.exposures.len() > 1,
            "timer-current": TIMERS.iter().position(|t| *t as f64 == number(c, "timer", 0.0)).unwrap_or(0),
            "best": c.get("best") == Some(&json!(true)),
            "best-shown": !extension && (caps.noise_reduction_hq || caps.sharpening_hq || caps.hot_pixel_hq),
            "distortion-current": DISTORTION.iter().position(|(k, _)| *k == text(c, "distortion")).unwrap_or(0),
            "distortion-shown": !extension && caps.distortion,
            "folder-name": Path::new(&self.folder).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| self.folder.clone()),
            "folder-tip": self.folder,
        })
    }

    /// The Take photo button, the note under it and Open folder (_refresh).
    fn state_props(&self, host: &dyn Host) -> Value {
        let note = if self.caps.is_some() {
            String::new()
        } else if !host.is_streaming() {
            tr("Start streaming to take photos.")
        } else {
            tr("This phone can't take photos from Telescope yet. Update the app on the phone.")
        };
        json!({
            "active": self.paired.on(), "shown": !self.paired.source(),
            "take-enabled": self.caps.is_some() && !self.busy,
            "info": note, "info-shown": self.caps.is_none(), "open-shown": !self.last_saved.is_empty(),
        })
    }

    fn status_props(&self) -> Value {
        json!({"status": self.status.0, "status-kind": self.status.1})
    }

    fn all_props(&self, host: &dyn Host) -> Value {
        let mut all = Self::fixed_props();
        for part in [self.choice_props(), self.state_props(host), self.status_props()] {
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

    fn show_choices(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.choice_props());
    }

    /// Brings the card up to date with the shown stream's lens and whether a photo is being taken.
    fn refresh(&mut self, host: &mut dyn Host) {
        let raw = host.still_caps(None);
        if raw != self.caps_raw {
            self.caps = raw.as_ref().and_then(|v| serde_json::from_value(v.clone()).ok());
            self.caps_raw = raw;
            self.show_choices(host);
        }
        self.busy = host.still_busy(None);
        host.set_ui_props("card", &self.state_props(host));
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            self.refresh(host);
            host.set_ui_props("card", &self.all_props(host));
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

    /// A pick from one of the drop-downs: the index in the list the card is showing.
    fn pick_index(&mut self, host: &mut dyn Host, kind: usize, index: usize) {
        let can = self.can();
        let value = match PICKS.get(kind).copied() {
            Some("mode") => can.modes.get(index).map(|v| json!(v)),
            Some("format") => can.formats.get(index).map(|v| json!(v)),
            Some("shots") => can.shots.get(index).map(|v| json!(v)),
            Some("exposure_s") => can.exposures.get(index).map(|s| if s.fract() == 0.0 { json!(*s as i64) } else { json!(s) }),
            Some("bracket") => BRACKETS.get(index).map(|(k, ..)| json!(k)),
            Some("distortion") => DISTORTION.get(index).map(|(k, _)| json!(k)),
            _ => None,
        };
        if let (Some(key), Some(value)) = (PICKS.get(kind), value) {
            self.pick_value(host, key, value);
        }
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
        let options = build_options(&caps, &self.choices, Self::rotation(host));
        match host.take_still(&options, &self.folder, None) {
            Some(id) => {
                self.taking.push(id);
                self.show_status(host, tr("Taking the photo…"), "status_dim");
                self.refresh(host);
            }
            None => self.show_status(host, tr("The phone is already taking a photo."), "status_err"),
        }
    }

    fn progress(&mut self, host: &mut dyn Host, p: &Value) {
        let int = |key: &str| p.get(key).and_then(Value::as_i64).unwrap_or(0);
        let (state, shots, taken, countdown) = (p.get("state").and_then(Value::as_str).unwrap_or(""), int("shots"), int("taken"), int("countdown_ms"));
        let text = if state == "waiting" && countdown > 0 {
            tr("Taking the photo in {seconds} s").replace("{seconds}", &((countdown + 999) / 1000).max(1).to_string())
        } else if shots > 1 {
            tr("Photo {n} of {total}").replace("{n}", &shots.min(taken + 1).to_string()).replace("{total}", &shots.to_string())
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
                tr("Saved {name} and {n} more").replace("{name}", &name).replace("{n}", &more.to_string())
            } else {
                tr("Saved {name}").replace("{name}", &name)
            };
            self.show_status(host, text.clone(), "status_ok");
            // Taken with a global shortcut from another app
            if !host.window_active() {
                host.send_notification(&tr("Photo saved"), &text, false);
            }
        } else {
            let error = result.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()).map_or_else(|| tr("The phone couldn't take the photo."), String::from);
            self.show_status(host, error, "status_err");
        }
        self.refresh(host);
    }

    fn open_folder(&self, host: &mut dyn Host) {
        let folder = self.last_saved.first().and_then(|f| Path::new(f).parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from(&self.folder));
        host.open_url(&file_url(&folder));
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
            about: n_("Takes full-quality photos on the phone, RAW and the phone's Night mode included where it has them.").into(),
            when_off: n_("Without it there's no Photos card and no Take photo shortcut.").into(),
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

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on, "shown": !self.paired.source()}));
        }
        match event {
            Event::PhoneStateUpdated { .. } | Event::StreamStopped | Event::DeviceChanged { .. } | Event::CameraSwitched { .. }
            | Event::StillBusyChanged { .. } => self.refresh(host),
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
        for part in [self.choice_props(), json!({"active": self.paired.on(), "shown": true, "take-enabled": false,
                                                  "info": tr("Start streaming to take photos."), "info-shown": true, "open-shown": false})] {
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
            "choice-picked" => self.pick_index(host, index(0), index(1)),
            "quality-moved" => {
                let value = event.args.first().and_then(Value::as_f64).unwrap_or(95.0).round() as i64;
                host.set_ui_props("card", &json!({"quality-text": value.to_string()}));
                self.pick_value(host, "quality", json!(value));
            }
            "number-edited" => {
                // A spin box takes what's typed once it's a number in range
                let typed = event.args.get(1).and_then(Value::as_str).unwrap_or("").trim().parse::<i64>();
                let caps = self.effective_caps();
                if let Ok(value) = typed {
                    match index(0) {
                        0 if (2..=caps.burst_max.max(2)).contains(&value) => self.pick_value(host, "burst", json!(value)),
                        1 if (2..=caps.burst_max.min(30).max(2)).contains(&value) => self.pick_value(host, "focus_steps", json!(value)),
                        _ => {}
                    }
                }
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
        // The card is out of use until a phone is paired
        if !self.paired.on() || self.paired.source() {
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
                    self.pick_index(host, 0, said);
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
        let sizes: Vec<String> = offered(caps, &Map::new()).modes.iter().filter_map(|m| caps.size_for(m).map(|(w, h)| format!("{m} {w}x{h}"))).collect();
        vec![("Photos".into(), have.join(", ")), ("Photo sizes".into(), if sizes.is_empty() { "unknown".into() } else { sizes.join(", ") })]
    }
}

#[cfg(test)]
#[path = "photos_tests.rs"]
mod tests;
