//! What the photo plugins share about the phone's stills: what a lens can take, the options a set of picks asks for and the folder photos
//! go to unless told otherwise. Plain helpers with no plugin in them, so Photos and Phototrap each use them without using each other.

use crate::shared::py_float;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use telescope_core::host::Host;
use telescope_core::i18n::{tr, Fields};

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
    pub shot: Option<ShotCaps>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ShotCaps {
    pub shutter: Option<ShotShutter>,
    pub iso: Option<ShotIso>,
    pub focus: Option<ShotFocus>,
    pub freezes_from_ns: i64,
    pub shutter_values_s: Vec<f64>,
    pub iso_values: Vec<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ShotShutter {
    pub min_ns: i64,
    pub max_ns: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ShotIso {
    pub min: i64,
    pub max: i64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ShotFocus {
    pub near_diopters: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct ShotChoice {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shutter_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iso: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_diopters: Option<f64>,
}

impl ShotChoice {
    pub fn to_options_value(&self) -> Value {
        json!(self)
    }

    pub fn summary(&self, focus_text: impl Fn(f64) -> String) -> String {
        let mut parts = Vec::new();
        if let Some(seconds) = self.shutter_s {
            parts.push(exposure_text(seconds));
        }
        if let Some(iso) = self.iso {
            parts.push(tr("ISO {iso}").field("iso", iso));
        }
        if let Some(diopters) = self.focus_diopters {
            let distance = if diopters == 0.0 { tr("∞") } else { focus_text(diopters) };
            parts.push(tr("focus {distance}").field("distance", distance));
        }
        if parts.is_empty() { tr("Like the camera") } else { parts.join(" · ") }
    }
}

/// "Video" for none, else the shutter speed (1/250 s, 8 s).
pub fn exposure_text(seconds: f64) -> String {
    if seconds == 0.0 {
        tr("Video")
    } else if let Some(n) = shutter_n(seconds) {
        tr("1/{n} s").field("n", n)
    } else {
        tr("{seconds} s").field("seconds", seconds)
    }
}

pub fn shot_focus_text(diopters: f64) -> String {
    match distance_parts(diopters) {
        None => tr("∞"),
        Some((cm, false)) => tr("{cm} cm").field("cm", cm),
        Some((tenths, true)) => {
            let metres = if tenths % 10 == 0 { (tenths / 10).to_string() } else { format!("{}.{}", tenths / 10, tenths % 10) };
            tr("{m} m").field("m", metres)
        }
    }
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
            distortion: false, burst_max: 1, extension_sizes: Vec::new(), camera: false, iso: None, shutter: None, shot: None,
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
                                    "focus_steps": 5, "bracket_merge": "frames", "merge_brightness": 0, "exposure_s": 0, "timer": 0, "best": true, "use_zoom": false, "distortion": "camera"}) else { unreachable!() };
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
    speeds(fastest, longest)
}

fn speeds(fastest_s: f64, longest_s: f64) -> Vec<f64> {
    let fast = FAST_N.iter().map(|n| 1.0 / f64::from(*n));
    fast.chain(LONG_S).filter(|s| *s >= fastest_s * 0.999 && *s <= longest_s * 1.0001).collect()
}

pub fn shot_shutter_stops(caps: &ShotCaps) -> Vec<f64> {
    if !caps.shutter_values_s.is_empty() {
        let mut stops: Vec<_> = caps.shutter_values_s.iter().copied().filter(|s| s.is_finite() && *s > 0.0).collect();
        stops.sort_by(f64::total_cmp);
        stops.dedup();
        return stops;
    }
    let Some(range) = &caps.shutter else { return Vec::new() };
    if range.min_ns <= 0 || range.max_ns < range.min_ns {
        return Vec::new();
    }
    let (min, max) = (range.min_ns as f64 / 1e9, range.max_ns as f64 / 1e9);
    let mut stops = speeds(min, max);
    stops.extend([10.0, 20.0]);
    stops.retain(|s| *s >= min && *s <= max);
    for end in [min, max] {
        if !stops.iter().any(|s| (*s - end).abs() <= end * 0.001) {
            stops.push(end);
        }
    }
    stops.sort_by(f64::total_cmp);
    stops.dedup();
    stops
}

pub fn shot_iso_stops(caps: &ShotCaps) -> Vec<i64> {
    if !caps.iso_values.is_empty() {
        let mut stops: Vec<_> = caps.iso_values.iter().copied().filter(|v| *v > 0).collect();
        stops.sort_unstable();
        stops.dedup();
        return stops;
    }
    let Some(range) = &caps.iso else { return Vec::new() };
    if range.max <= 0 || range.max < range.min {
        return Vec::new();
    }
    let mut stops: Vec<i64> = [50, 64, 80, 100, 125, 160, 200, 250, 320, 400, 500, 640, 800, 1000, 1250, 1600,
        2000, 2500, 3200, 4000, 5000, 6400, 8000, 10000, 12800, 16000, 20000, 25600]
        .into_iter().filter(|v| *v >= range.min && *v <= range.max).collect();
    if stops.last() != Some(&range.max) {
        stops.push(range.max);
    }
    stops
}

pub fn shot_shutter_labels(stops: &[f64], value: Option<f64>) -> (Vec<String>, usize) {
    let mut labels = vec![tr("Camera")];
    labels.extend(stops.iter().map(|s| exposure_text(*s)));
    // An override the camera has no stops for stays visible, so Camera is a pick away from it
    if let (true, Some(v)) = (stops.is_empty(), value) {
        labels.push(exposure_text(v));
        return (labels, 1);
    }
    let index = value.and_then(|v| stops.iter().enumerate()
        .min_by(|(_, a), (_, b)| (**a - v).abs().total_cmp(&(**b - v).abs())).map(|(i, _)| i + 1)).unwrap_or(0);
    (labels, index)
}

pub fn shot_iso_labels(stops: &[i64], value: Option<i64>) -> (Vec<String>, usize) {
    let mut labels = vec![tr("Camera")];
    labels.extend(stops.iter().map(ToString::to_string));
    if let (true, Some(v)) = (stops.is_empty(), value) {
        labels.push(v.to_string());
        return (labels, 1);
    }
    let index = value.and_then(|v| stops.iter().enumerate().min_by_key(|(_, n)| (**n).abs_diff(v)).map(|(i, _)| i + 1)).unwrap_or(0);
    (labels, index)
}

// The shared shot controls use their own property names beside Photos' existing camera dials and focus bracket.
pub fn shot_control_props(caps: Option<&StillCaps>, choice: &ShotChoice, shutter: bool, suppressed: bool) -> Value {
    let shot = caps.and_then(|c| c.shot.as_ref());
    let shutters = shot.map(shot_shutter_stops).unwrap_or_default();
    let isos = shot.map(shot_iso_stops).unwrap_or_default();
    let (shutter_labels, shutter_index) = shot_shutter_labels(&shutters, choice.shutter_s);
    let (iso_labels, iso_index) = shot_iso_labels(&isos, choice.iso);
    let tip = |offered: bool, action: String, missing: String| {
        if suppressed { tr("Night mode sets its own exposure") }
        else if caps.is_none() { tr("Waiting for the picture") }
        else if !offered { missing }
        else { action }
    };
    let near = shot_focus_near(caps);
    let focus = shot_focus(caps, choice);
    let summary = ShotChoice { shutter_s: choice.shutter_s.filter(|_| shutter), focus_diopters: focus, ..choice.clone() }.summary(shot_focus_text);
    json!({
        "shot-summary": summary,
        "shot-shutter-stops": shutter_labels, "shot-shutter-index": shutter_index,
        // An override the camera has no stops for stays enabled so Camera can clear it
        "shot-shutter-enabled": shutter && !suppressed && (!shutters.is_empty() || choice.shutter_s.is_some()),
        "shot-shutter-tip": tip(!shutters.is_empty(), tr("Set the shutter speed for the photo."), tr("This camera can't set the shutter speed.")),
        "shot-iso-stops": iso_labels, "shot-iso-index": iso_index,
        "shot-iso-enabled": !suppressed && (!isos.is_empty() || choice.iso.is_some()),
        "shot-iso-tip": tip(!isos.is_empty(), tr("Set the ISO for the photo."), tr("This camera can't set the ISO.")),
        "shot-focus-enabled": !suppressed && near > 0.0,
        "shot-focus-tip": tip(near > 0.0, tr("Set the focus for the photos."), tr("This camera can't set the focus for a photo.")),
        "shot-focus-value": focus_position(near, focus.unwrap_or(0.0)),
        "shot-focus-unset": focus.is_none(),
        "shot-focus-readout": focus.map_or_else(|| tr("Camera"), shot_focus_text),
    })
}

pub fn pick_shot(choice: &mut ShotChoice, caps: &StillCaps, shutter: bool, index: usize, suppressed: bool) -> bool {
    if suppressed { return false; }
    let before = choice.clone();
    // Camera always clears the override, whatever the camera says it can set
    if index == 0 {
        if shutter { choice.shutter_s = None } else { choice.iso = None }
        return *choice != before;
    }
    let Some(caps) = &caps.shot else { return false };
    if shutter {
        let stops = shot_shutter_stops(caps);
        if index > stops.len() { return false; }
        choice.shutter_s = Some(stops[index - 1]);
    } else {
        let stops = shot_iso_stops(caps);
        if index > stops.len() { return false; }
        choice.iso = Some(stops[index - 1]);
    }
    *choice != before
}

/// The nearest focus a photo can be set to, in diopters; 0 when this camera takes no focus for a photo (an older phone, a DSLR).
pub fn shot_focus_near(caps: Option<&StillCaps>) -> f64 {
    caps.and_then(|c| c.shot.as_ref()).and_then(|c| c.focus.as_ref()).map_or(0.0, |f| f.near_diopters).max(0.0)
}

/// The focus the photo is taken at: none on a camera that can't set it, which then uses its own.
pub fn shot_focus(caps: Option<&StillCaps>, choice: &ShotChoice) -> Option<f64> {
    choice.focus_diopters.filter(|_| shot_focus_near(caps) > 0.0)
}

/// Where the phone on screen is focused now in diopters, while it is on manual focus: the only time it says.
pub fn focus_now(host: &dyn Host) -> Option<f64> {
    let state = host.phone_state(None)?;
    (state.get("focus_mode").and_then(Value::as_str) == Some("manual")).then(|| state.get("focus_distance").and_then(Value::as_f64)).flatten()
}

/// A dot on the Focus slider where the phone is focused now, which a dragged handle sticks to; none when that isn't known.
pub fn shot_focus_marks(caps: Option<&StillCaps>, now: Option<f64>) -> Vec<f64> {
    let near = shot_focus_near(caps);
    now.filter(|_| near > 0.0).map(|d| focus_position(near, d)).into_iter().collect()
}

/// The focus slider moved (position on the focus scale) or was reset to the camera's own (None); true when the choice changed.
pub fn move_shot_focus(choice: &mut ShotChoice, caps: Option<&StillCaps>, position: Option<f64>, suppressed: bool) -> bool {
    if suppressed { return false; }
    // Camera always clears the override, like on the dials
    let focus = match position {
        None => None,
        Some(p) => {
            let near = shot_focus_near(caps);
            if near <= 0.0 { return false; }
            Some((p.clamp(0.0, FOCUS_SCALE) / FOCUS_SCALE * near * 100.0).round() / 100.0)
        }
    };
    let changed = focus != choice.focus_diopters;
    choice.focus_diopters = focus;
    changed
}

pub fn shot_choice(value: Option<&Value>) -> ShotChoice {
    value.cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

// Keep requested overrides so an unavailable setting is refused instead of silently falling back.
// The focus is left out on a camera that can't set it, since an older phone would refuse the photo.
pub fn add_shot(options: &mut Value, caps: &StillCaps, choice: &ShotChoice, shutter: bool, suppressed: bool) {
    if suppressed { return; }
    let shot = ShotChoice {
        shutter_s: choice.shutter_s.filter(|_| shutter),
        iso: choice.iso,
        focus_diopters: shot_focus(Some(caps), choice),
    };
    if shot != ShotChoice::default() { options["shot"] = shot.to_options_value(); }
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

pub fn focus_position(near_diopters: f64, diopters: f64) -> f64 {
    if near_diopters > 0.0 { (diopters / near_diopters * FOCUS_SCALE).clamp(0.0, FOCUS_SCALE) } else { 0.0 }
}

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

/// Whether the photo can be cropped to the zoom without changing its picture. HEIC and Ultra HDR only get turned, by their orientation.
pub fn can_use_zoom(format: &str) -> bool {
    !matches!(format, "heic" | "ultrahdr")
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
    opts.insert("use_zoom".into(), json!(choices.get("use_zoom") == Some(&Value::Bool(true)) && can_use_zoom(format)));
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

#[cfg(test)]
#[path = "stills_shot_tests.rs"]
mod shot_tests;

