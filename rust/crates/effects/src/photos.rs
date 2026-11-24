//! The Photos plugin: full-quality photos taken on the phone through the host's stills service and saved in a
//! folder. What the card offers follows the lens the shown stream is on (RAW, the maker's Night mode, bursts and brackets where it
//! has them); the picks of another lens are kept for when it comes back. A stream with a camera of its own (a DSLR) answers the same
//! service, and then the card also offers the camera's own ISO and shutter speed lists, where its mode allows them. Values that run in
//! order (ISO, shutter speed, how many, a long exposure) step along their list, named choices open one. Every control has its place on the
//! card whatever the lens offers, and is greyed out when there is nothing to pick, so nothing moves when a stream starts.

use telescope_core::i18n::Fields;
use crate::shared::Paired;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use telescope_core::actions::{self, Choice};
use telescope_core::event::Event;
use telescope_core::hold::{self, Hold, Tick, TICK_MS};
use telescope_core::host::{FileDialog, FileDialogKind, Host, RequestId};
use telescope_core::i18n::{n_, tr, trn_open};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::settings::{off_default, reset_config};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

// What the photo plugins share lives in stills; the old paths still work
pub use crate::stills::*;

pub const NAME: &str = "photos";
const T_HOLD: &str = "hold";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/photos.slint"));

pub const FORMATS: [(&str, &str); 5] = [("jpeg", n_("JPEG")), ("heic", n_("HEIC")), ("ultrahdr", n_("Ultra HDR")), ("raw", n_("RAW (DNG)")), ("raw+jpeg", n_("RAW and JPEG"))];
pub const MODES: [(&str, &str); 6] = [
    ("normal", n_("Normal")), ("max_res", n_("Full resolution")), ("night", n_("Night")), ("hdr", n_("HDR")), ("bokeh", n_("Bokeh")), ("auto", n_("Auto")),
];
pub const SHOTS: [(&str, &str); 4] = [("single", n_("Single")), ("burst", n_("Burst")), ("bracket", n_("Exposure bracket")), ("focus", n_("Focus bracket"))];
/// What a focus or exposure bracket saves: the photos only, the photos and one merged picture, or the merged picture only (crate::bracket_merge).
pub const BRACKET_MERGE: [(&str, &str); 3] = [("frames", n_("Photos")), ("both", n_("Both")), ("merged", n_("Merged"))];
const BRACKET_MERGE_TIPS: [&str; 3] = [
    n_("Save only the photos of the bracket."),
    n_("Save the photos and one picture merged from them."),
    n_("Save only the merged picture and delete the photos."),
];
const BRACKET_MERGE_OFF_TIP: &str = n_("Merging works on focus and exposure brackets saved as JPEG.");
const BRIGHTNESS_TIP: &str = n_("Favour the darker or the lighter photos of an exposure bracket in the merged picture.");
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
const FOCUS_RANGE_TIP: &str = n_("Take the focus bracket between the two handles, from the farthest focus on the left to the nearest on the right. The mark shows where the phone is focused now (in manual focus).");
const BRACKET_PHOTOS_TIP: &str = n_("Number of photos in the exposure bracket, spread evenly around the video's exposure.");
const BRACKET_GAP_TIP: &str = n_("EV between photos");
const BURST_TIP: &str = n_("Number of photos in the burst.");
const SHOTS_TIP: &str = n_("Take one photo, a burst or a set at different exposures or focus distances.");
const TIMER_TIP: &str = n_("Wait a few seconds before taking the photo.");
const EXPOSURE_TIP: &str = n_("Set the shutter speed for the photo. ISO is the camera's unless you set it below. The video pauses while the shutter is open.");
const BEST_TIP: &str = n_("Use extra noise reduction and sharpening on the photo only, not the video. Taking the photo takes a little longer.");
const USE_ZOOM_OFF_TIP: &str = n_("Switch to JPEG to use zoom. HEIC and Ultra HDR can't be cropped losslessly.");
const BEST_OFF_TIP: &str = n_("This lens and mode have no extra noise reduction or sharpening to offer");
const QUALITY_CAMERA_TIP: &str = n_("JPEG quality is set by the camera");
const QUALITY_RAW_TIP: &str = n_("RAW photos have no quality setting");
const CAMERA_BUSY: &str = n_("The camera is already taking a photo.");
const MERGE_QUEUE_FULL: &str = n_("Still merging the last brackets. Try again in a moment.");
const CAMERA_FAILED: &str = n_("The camera couldn't take the photo.");

/// The range of a focus bracket in one unit, nearest first: 0.25-2 m, 0.25 m-∞.
pub fn range_text(near: f64, far: f64) -> String {
    let metres = |d: f64| {
        let m = (1.0 / d * 100.0).round() / 100.0;
        if m.fract() == 0.0 { format!("{m:.0}") } else { format!("{m}") }
    };
    if far < 0.01 {
        tr("{near} m-∞").field("near", metres(near))
    } else {
        tr("{near}-{far} m").field("near", metres(near)).field("far", metres(far))
    }
}

/// A focus distance: 25 cm, 1.5 m, Infinity.
pub fn distance_text(diopters: f64) -> String {
    match distance_parts(diopters) {
        None => tr("Infinity"),
        Some((cm, false)) => tr("{cm} cm").field("cm", cm.to_string()),
        Some((tenths, true)) => tr("{m} m").field("m", if tenths % 10 == 0 { (tenths / 10).to_string() } else { format!("{}.{}", tenths / 10, tenths % 10) }),
    }
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
    customize_open: bool,
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
    /// The brackets being taken that are to be merged: the request, what to save, the JPEG quality and what kind of merge.
    merges: Vec<(RequestId, String, u8, crate::bracket_merge::Kind, usize, crate::bracket_merge::Reservation)>,
    /// The newest photo asked for: a merge of an older one finishing later doesn't take Open folder back to its photos.
    latest_take: Option<RequestId>,
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
            customize_open: false,
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
            merges: Vec::new(),
            latest_take: None,
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
            "best-name": tr("Best quality"), "distortion-tips": DISTORTION_TIPS.map(tr), "merge-tips": BRACKET_MERGE_TIPS.map(tr), "merge-off-tip": tr(BRACKET_MERGE_OFF_TIP), "brightness-tip": tr(BRIGHTNESS_TIP),
            "shots-names": SHOTS.map(|(_, name)| tr(name)), "mode-tip": tr(MODE_TIP), "shots-tip": tr(SHOTS_TIP), "timer-tip": tr(TIMER_TIP), 
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

    /// The range of the How many box.
    fn burst_range(&self) -> (i64, i64) {
        (2, self.effective_caps().burst_max.max(2))
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
        if shots == "burst" {
            let (min, max) = self.burst_range();
            let options = (min..=max).map(|n| n.to_string()).collect();
            let index = int(c, "burst", 5).clamp(min, max) - min;
            tiles.push(Tile { tip: tr(BURST_TIP), ..tile("burst", tr("How many"), true, options, index as usize) });
        }
        if shots == "focus" {
            let counts = focus_counts(&caps);
            let wanted = int(c, "focus_steps", 5);
            let index = counts.iter().rposition(|n| *n <= wanted).unwrap_or(0);
            tiles.push(Tile { tip: tr(FOCUS_TIP), ..tile("focus_steps", tr("Steps"), true, counts.iter().map(i64::to_string).collect(), index) });
        }
        if shots == "bracket" {
            let (photos, gap) = bracket_shape(&caps, c);
            let options = (2..=caps.bracket_max.max(2)).map(|n| n.to_string()).collect();
            tiles.push(Tile { tip: tr(BRACKET_PHOTOS_TIP), ..tile("bracket_photos", tr("Photos"), true, options, (photos - 2) as usize) });
            let gaps = bracket_gaps(&caps, photos);
            let options = gaps.iter().map(|g| gap_text(*g)).collect();
            let index = gaps.iter().position(|g| *g == gap).unwrap_or(0);
            tiles.push(Tile { tip: tr(BRACKET_GAP_TIP), ..tile("bracket_gap", tr("EV"), true, options, index) });
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
            "bracket" => "bracket_photos",
            _ => "",
        };
        let second = if shots == "bracket" { "bracket_gap" } else { "exposure_s" };
        let focus_range_shown = shots == "focus" && caps.focus_near_max > 0.0;
        let (near, far) = focus_range(&caps, c);
        let (low, high) = focus_handles(&caps, c);
        let full_range = !c.contains_key("focus_near") && !c.contains_key("focus_far");
        let range = json!({
            "second-key": second, "second-label": if shots == "bracket" { tr("EV") } else { tr("Exposure") },
            "second-per": if shots == "bracket" { 3 } else { 1 }, "second-from": if shots == "bracket" { 2 } else { 0 },
            "second-options": options(second), "second-index": index(second), "second-tip": find(second).map_or_else(String::new, |t| t.tip.clone()),
            "focus-range-shown": focus_range_shown, "focus-range-enabled": true, "focus-range-changed": !full_range,
            "focus-low": low, "focus-high": high, "focus-low-text": distance_text(far), "focus-high-text": distance_text(near),
            "focus-readout": if full_range { tr("All") } else { range_text(near, far) },
            "focus-range-tip": tr(FOCUS_RANGE_TIP),
        });
        let mut props = json!({
            "iso-options": options("iso"), "iso-index": index("iso"), "shutter-options": options("shutter"), "shutter-index": index("shutter"),
            "mode-options": Self::labelled(&can.modes, &MODES), "mode-index": index_of(&can.modes, mode), "mode-enabled": can.modes.len() > 1,
            "format-options": Self::labelled(&can.formats, &FORMATS), "format-index": index_of(&can.formats, format), "format-enabled": can.formats.len() > 1,
            "shots-offered": SHOTS.map(|(key, _)| can.shots.contains(&key)), "shots-index": SHOTS.iter().position(|(key, _)| *key == shots).unwrap_or(0),
            "slot-key": slot, "slot-label": find(slot).map_or_else(String::new, |t| t.label.clone()), "slot-options": options(slot),
            "slot-index": index(slot), "slot-tip": find(slot).map_or_else(String::new, |t| t.tip.clone()),
            "distortion-enabled": find("distortion").is_some(), "distortion-index": index("distortion"),
            "merge-enabled": self.merge_possible(), "brightness-enabled": self.merge_possible() && shots == "bracket", "brightness": self.merge_brightness(), "brightness-text": Self::brightness_text(self.merge_brightness()), "merge-index": BRACKET_MERGE.iter().position(|(k, _)| *k == text(c, "bracket_merge")).unwrap_or(0),
            "quality": quality, "quality-text": quality.to_string(), "quality-enabled": format != "raw" && !caps.camera, "quality-tip": quality_tip,
            "timer-current": TIMERS.iter().position(|t| *t as f64 == number(c, "timer", 0.0)).unwrap_or(0),
            "use-zoom": c.get("use_zoom") == Some(&Value::Bool(true)), "use-zoom-enabled": can_use_zoom(format),
            "use-zoom-tip": if can_use_zoom(format) { String::new() } else { tr(USE_ZOOM_OFF_TIP) },
            "best": c.get("best") == Some(&json!(true)), "best-enabled": best, "best-tip": tr(if best { BEST_TIP } else { BEST_OFF_TIP }),
            "folder-name": Path::new(&self.folder).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| self.folder.clone()),
            "folder-tip": self.folder, "folder-changed": self.folder != self.default_folder,
            "reset-shown": self.reset_shown(),
        });
        if let (Value::Object(all), Value::Object(more)) = (&mut props, range) {
            all.extend(more);
        }
        if let Some(all) = props.as_object_mut() {
            if let Value::Object(more) = shot_control_props(self.caps.as_ref(), &shot_choice(c.get("shot")), false, extension) {
                all.extend(more);
            }
            all.insert("customize-open".into(), json!(self.customize_open));
            all.insert("customize-shown".into(), json!(!caps.camera));
            if extension { all.insert("shot-iso-tip".into(), json!(tr("This mode sets its own exposure"))); }
        }
        props
    }

    /// What a Reset leaves alone: where the photos go.
    const RESET_KEPT: &'static [&'static str] = &["folder"];

    /// A config with every number as a float, so a saved 95 and a default 95.0 are the same pick.
    fn alike(config: Value) -> Value {
        match config {
            Value::Number(n) => n.as_f64().map_or(Value::Number(n), |f| json!(f)),
            Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, Self::alike(v))).collect()),
            other => other,
        }
    }

    fn reset_shown(&self) -> bool {
        // the merge pick isn't written while it is the photos, so the defaults are compared without it
        let mut base = defaults();
        base.shift_remove("bracket_merge");
        base.shift_remove("merge_brightness");
        off_default(&Self::alike(self.get_config()), &Self::alike(Value::Object(base)), Self::RESET_KEPT)
    }

    fn reset(&mut self, host: &mut dyn Host) {
        let cfg = reset_config(&self.get_config(), &Value::Object(defaults()), Self::RESET_KEPT);
        if self.set_config(&cfg).is_ok() {
            self.catch_up(host);
            host.schedule_save();
        }
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
            "focus-live": Self::focus_now(host).is_some(), "focus-live-pos": Self::live_pos(&self.effective_caps(), Self::focus_now(host)).unwrap_or(0.0),
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
        if key == "shot" && value.as_object().is_some_and(|v| v.is_empty()) {
            self.choices.shift_remove(key);
        } else {
            self.choices.insert(key.into(), value);
        }
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
            "bracket_photos" => Some(2 + index as i64).filter(|n| *n <= self.effective_caps().bracket_max.max(2)).map(|n| json!(n)),
            "bracket_gap" => {
                let caps = self.effective_caps();
                let (photos, _) = bracket_shape(&caps, &self.choices);
                bracket_gaps(&caps, photos).get(index).map(|g| json!(g))
            }
            "focus_steps" => focus_counts(&self.effective_caps()).get(index).map(|n| json!(n)),
            "distortion" => DISTORTION.get(index).map(|(k, _)| json!(k)),
            "bracket_merge" => BRACKET_MERGE.get(index).map(|(k, _)| json!(k)),
            "burst" => {
                let (min, max) = self.burst_range();
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

    /// Where the phone is focused now in diopters, while it is on manual focus (the number may not follow the lens otherwise).
    fn focus_now(host: &dyn Host) -> Option<f64> {
        let state = host.phone_state(None)?;
        (state.get("focus_mode").and_then(Value::as_str) == Some("manual")).then(|| state.get("focus_distance").and_then(Value::as_f64)).flatten()
    }

    /// The ends of the focus bracket dragged on the slider (positions on the focus scale). A handle at its end is the lens's end, so
    /// both at the ends saves no range at all.
    fn move_focus_range(&mut self, host: &mut dyn Host, low: f64, high: f64) {
        let caps = self.effective_caps();
        if caps.focus_near_max <= 0.0 {
            return;
        }
        let diopters = |pos: f64| ((pos / FOCUS_SCALE).clamp(0.0, 1.0) * caps.focus_near_max * 1000.0).round() / 1000.0;
        if low <= 0.0 {
            self.choices.shift_remove("focus_far");
        } else {
            self.choices.insert("focus_far".into(), json!(diopters(low)));
        }
        if high >= FOCUS_SCALE {
            self.choices.shift_remove("focus_near");
        } else {
            self.choices.insert("focus_near".into(), json!(diopters(high)));
        }
        self.show_choices(host);
        host.schedule_save();
    }

    /// Where the phone is focused now on the focus scale, while it is on manual focus.
    fn live_pos(caps: &StillCaps, seen: Option<f64>) -> Option<f64> {
        seen.filter(|_| caps.focus_near_max > 0.0).map(|d| (d / caps.focus_near_max * FOCUS_SCALE).clamp(0.0, FOCUS_SCALE))
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

    /// The darker to lighter pick for exposure merging, -100 to 100.
    fn merge_brightness(&self) -> i64 {
        int(&self.choices, "merge_brightness", 0).clamp(-100, 100)
    }

    fn brightness_text(value: i64) -> String {
        if value < 0 { tr("Darker") } else if value > 0 { tr("Lighter") } else { tr("Middle") }
    }

    /// How many shots the bracket asked the phone for: its focus steps, or its exposure steps.
    fn shots_expected(options: &Value) -> usize {
        if let Some(n) = options.get("focus_steps").and_then(Value::as_u64) {
            return n as usize;
        }
        options.get("bracket_ev").and_then(Value::as_str).map_or(0, |ev| ev.split(',').count())
    }

    /// What kind of merge the bracket about to be taken gets.
    fn merge_kind(&self) -> crate::bracket_merge::Kind {
        if text(&self.choices, "shots") == "bracket" {
            crate::bracket_merge::Kind::Exposure { brightness: self.merge_brightness() as i32 }
        } else {
            crate::bracket_merge::Kind::Focus
        }
    }

    /// Whether a bracket can be merged as the card is set: the photos are JPEGs (a RAW photo can't be read, and the JPEG of RAW and JPEG can).
    fn merge_possible(&self) -> bool {
        let can = self.can();
        let (shots, format) = (pick(text(&self.choices, "shots"), &can.shots), pick(text(&self.choices, "format"), &can.formats));
        (shots == "focus" || shots == "bracket") && (format == "jpeg" || format == "raw+jpeg")
    }

    /// What the focus bracket about to be taken saves besides its photos: "both" or "merged", None for the photos only.
    fn merge_mode(&self) -> Option<&'static str> {
        let wanted = text(&self.choices, "bracket_merge");
        BRACKET_MERGE.iter().map(|(k, _)| *k).find(|k| *k == wanted && *k != crate::bracket_merge::FRAMES).filter(|_| self.merge_possible())
    }

    /// The photos of a bracket are in: merge them on a thread of its own, which says when it's done.
    fn start_merge(&mut self, host: &mut dyn Host, files: &[String], mode: &str, quality: u8, kind: crate::bracket_merge::Kind, request: RequestId, expected: usize, reservation: crate::bracket_merge::Reservation) {
        let Some(poster) = host.poster() else { return };
        reservation.submit(poster, files.to_vec(), quality, mode != crate::bracket_merge::MERGED, kind, request.0, expected);
        self.show_status(host, tr("Merging the bracket…"), "status_dim");
    }

    fn merged(&mut self, host: &mut dyn Host, data: &Value) {
        // a merge of an older photo that comes in after a newer one was asked for says so but leaves Open folder alone
        let newest = data.get("request").and_then(Value::as_u64) == self.latest_take.map(|r| r.0);
        if data.get("ok").and_then(Value::as_bool) == Some(true) {
            let path = data.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let name = Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let (frames, used) = (data.get("frames").and_then(Value::as_i64).unwrap_or(0), data.get("used").and_then(Value::as_i64).unwrap_or(0));
            let text = if data.get("kept").and_then(Value::as_bool) == Some(true) {
                tr("Saved {name} from {used} of {frames} photos. The photos are still saved.").field("name", &name).field("used", used.to_string()).field("frames", frames.to_string())
            } else if data.get("left").and_then(Value::as_i64).unwrap_or(0) > 0 {
                tr("Saved the merged picture {name}. Some photos couldn't be deleted.").field("name", &name)
            } else if used < frames {
                tr("Saved the merged picture {name} from {used} of {frames} photos").field("name", &name).field("used", used.to_string()).field("frames", frames.to_string())
            } else {
                tr("Saved the merged picture {name}").field("name", &name)
            };
            if newest {
                // the photos that are still there come after the merged picture
                self.last_saved.retain(|f| Path::new(f).exists());
                self.last_saved.insert(0, path);
            }
            self.show_status(host, text.clone(), "status_ok");
            if !host.window_active() {
                host.send_notification(&tr("Photo saved"), &text, false);
            }
        } else {
            log::warn!("The bracket couldn't be merged: {}", data.get("error").and_then(Value::as_str).unwrap_or("?"));
            self.show_status(host, tr("The photos are saved but couldn't be merged."), "status_err");
        }
        self.refresh(host);
    }

    fn take(&mut self, host: &mut dyn Host) {
        let Some(raw) = host.still_caps(None) else { return };
        if host.still_busy(None) {
            return;
        }
        let caps: StillCaps = serde_json::from_value(raw).unwrap_or_default();
        let mut options = build_options(&caps, &self.choices, Self::rotation(host));
        if !caps.camera {
            let can = offered(&caps, &self.choices);
            let maker = EXTENSIONS.contains(&pick(text(&self.choices, "mode"), &can.modes));
            add_shot(&mut options, &shot_choice(self.choices.get("shot")), false, maker);
        }
        if caps.camera {
            camera_options(&mut options, self.camera_picks[0].as_deref(), self.camera_picks[1].as_deref());
        }
        let merge = if let Some(mode) = self.merge_mode() {
            let Some(reservation) = crate::bracket_merge::reserve() else {
                self.show_status(host, tr(MERGE_QUEUE_FULL), "status_err");
                return;
            };
            Some((mode, reservation))
        } else {
            None
        };
        match host.take_still(&options, &self.folder, None) {
            Some(id) => {
                if let Some((mode, reservation)) = merge {
                    self.merges.push((id, mode.to_string(), int(&self.choices, "quality", 95).clamp(50, 100) as u8, self.merge_kind(), Self::shots_expected(&options), reservation));
                }
                self.taking.push(id);
                self.latest_take = Some(id);
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
            tr("The video is paused while the photo is taken…")
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
        if data.get("code").and_then(Value::as_str) == Some("restore") {
            self.done(host, data);
            return;
        }
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
            in_presets: true,
            device_local: true,
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
        self.merges.clear();
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
                if let Some(at) = self.merges.iter().position(|(r, ..)| r == request) {
                    let (_, mode, quality, kind, expected, reservation) = self.merges.remove(at);
                    if data.get("ok").and_then(Value::as_bool) == Some(true) {
                        let files = self.last_saved.clone();
                        self.start_merge(host, &files, &mode, quality, kind, *request, expected, reservation);
                    }
                }
            }
            Event::Posted { owner, token, data } if owner == NAME && token == "merged" => self.merged(host, data),
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
            "customize-toggled" => {
                self.customize_open = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.show_choices(host);
            }
            "shot-iso-picked" => {
                let caps = self.effective_caps();
                let can = offered(&caps, &self.choices);
                let maker = EXTENSIONS.contains(&pick(text(&self.choices, "mode"), &can.modes));
                let mut shot = shot_choice(self.choices.get("shot"));
                if !caps.camera && pick_shot(&mut shot, &caps, false, index(0), maker) {
                    self.pick_value(host, "shot", shot.to_options_value());
                }
            }
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
            "focus-range-moved" => {
                let at = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
                self.move_focus_range(host, at(0), at(1));
            }
            "brightness-moved" => {
                let value = event.args.first().and_then(Value::as_f64).unwrap_or(0.0).round() as i64;
                host.set_ui_props("card", &json!({"brightness-text": Self::brightness_text(value)}));
                self.pick_value(host, "merge_brightness", json!(value));
            }
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
            "use-zoom-toggled" => self.pick_value(host, "use_zoom", json!(event.args.first().and_then(Value::as_bool).unwrap_or(false))),
            "best-toggled" => self.pick_value(host, "best", json!(event.args.first().and_then(Value::as_bool).unwrap_or(false))),
            "folder-clicked" => self.choose_folder(host),
            "reset-clicked" => self.reset(host),
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
        // written only when something other than the photos is asked for, so settings that never used it read as they always did
        if text(&cfg, "bracket_merge") == crate::bracket_merge::FRAMES {
            cfg.shift_remove("bracket_merge");
        }
        if int(&cfg, "merge_brightness", 0) == 0 {
            cfg.shift_remove("merge_brightness");
        }
        cfg.insert("folder".into(), json!(self.folder));
        Value::Object(cfg)
    }

    /// The still settings; the folder is this computer's own.
    fn preset_config(&self) -> Value {
        Value::Object(self.choices.clone())
    }

    /// A key the preset lacks keeps its value, and the folder never changes.
    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        let Value::Object(cfg) = cfg else { return Err(ConfigRejected("settings".into())) };
        let mut merged = self.get_config();
        for (key, value) in cfg.iter().filter(|(key, _)| *key != "folder") {
            merged[key] = value.clone();
        }
        self.set_config(&merged)?;
        self.catch_up(host);
        Ok(())
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
        // A bracket saved as one pick ("3x2") is the same bracket as the two picks that replaced it
        if !cfg.contains_key("bracket_photos") && !cfg.contains_key("bracket_gap") {
            if let Some((photos, gap)) = cfg.get("bracket").and_then(Value::as_str).and_then(old_bracket) {
                choices.insert("bracket_photos".into(), json!(photos));
                choices.insert("bracket_gap".into(), json!(gap));
            }
        }
        // The ends of a focus bracket have no default: none saved is the lens's whole range
        for key in ["focus_near", "focus_far"] {
            if let Some(d) = cfg.get(key).filter(|v| v.as_f64().is_some_and(|d| d.is_finite() && d >= 0.0)) {
                choices.insert(key.into(), d.clone());
            }
        }
        if !TIMERS.iter().any(|t| *t as f64 == number(&choices, "timer", 0.0)) {
            choices.insert("timer".into(), json!(0));
        }
        if let Some(value) = cfg.get("shot") {
            let shot = shot_choice(Some(value));
            if shot != ShotChoice::default() {
                choices.insert("shot".into(), shot.to_options_value());
            }
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
