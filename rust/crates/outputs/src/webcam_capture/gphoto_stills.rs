//! Full-resolution photos from a DSLR while it streams live view (gphoto_lib.rs). The camera's thread does the shooting between two
//! live view frames, since only it can talk to the camera: live view off, the settings the card asked for, the picture straight to
//! the computer (capture target Internal RAM, not the card), then live view on again. The service the Photos card reaches through
//! core's SourceStills counts down a self-timer, hands the job to that thread, reports progress while it waits and saves the files.
//! What the camera offers (the picture formats it has, ISO and shutter speed where its mode dial allows them) is read again with
//! the other settings, so a turn of the dial shows up on the card.

use super::gphoto_lib::{resolve, stills_of, Incomplete, Kind, Rig, Setting, Shot};
use crate::webcam_controls::shutter_text;
use crate::webcams::gphoto_port;
use serde_json::{json, Value};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use telescope_core::i18n::{n_, tr};
use telescope_core::source::SourceStills;

/// The camera's own names for what a photo needs, each listing the names to try in turn (Nikon, then Canon and others).
const QUALITY: [&str; 2] = ["imagequality", "imageformat"];
const ISO: [&str; 1] = ["iso"];
const SHUTTER: [&str; 2] = ["shutterspeed", "shutterspeed2"];
const TARGET: [&str; 1] = ["capturetarget"];
/// A photo that hasn't come back by now is given up on (a 30 s exposure with noise reduction took 67 s on a D5100). A longer exposure
/// waits for longer than this, see `wait_for_photo`.
const LONGEST: Duration = Duration::from_secs(4 * 60);
/// How often the job looks at its camera's thread, and how many looks make one progress report.
const LOOK: Duration = Duration::from_millis(250);
const REPORT_EVERY: u32 = 4;
/// How long a camera that was just told another image quality gets before its first shot is tried again.
#[cfg(not(test))]
const AFTER_SWITCH: Duration = Duration::from_millis(1500);
#[cfg(test)]
const AFTER_SWITCH: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Raw,
    RawJpeg,
}

/// What kind of picture one of the camera's image quality choices makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Jpeg,
    Raw,
    Both,
    Other,
}

pub fn classify(choice: &str) -> Class {
    let c = choice.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| c.contains(w));
    let raw = has(&["raw", "nef", "cr2", "cr3", "arw", "dng", "orf", "raf", "rw2"]);
    let jpeg = has(&["jpeg", "jpg"]);
    if raw {
        if jpeg || c.contains('+') { Class::Both } else { Class::Raw }
    } else if jpeg || has(&["fine", "normal", "basic", "large", "medium", "small"]) {
        Class::Jpeg
    } else {
        Class::Other
    }
}

/// How good a quality choice is: the finest and the largest score most.
fn score(choice: &str) -> i32 {
    let c = choice.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| c.contains(w));
    let quality = if has(&["fine", "high"]) { 30 } else if c.contains("normal") { 20 } else if c.contains("basic") { 10 } else { 0 };
    let size = if has(&["tiny", "smaller"]) { 0 } else if c.contains("medium") { 2 } else if c.contains("small") { 1 } else { 3 };
    quality + size
}

/// The camera's image quality setting.
#[derive(Clone, Debug, PartialEq)]
pub struct Quality {
    pub name: &'static str,
    pub choices: Vec<String>,
    pub current: String,
    pub writable: bool,
}

impl Quality {
    /// What to write for a format: the choice the camera is on when it already makes that kind of picture (the maker's settings stay
    /// as they were), else the finest of that kind. None when it can't make that kind now.
    pub fn target(&self, format: Format) -> Option<&String> {
        let class = match format {
            Format::Jpeg => Class::Jpeg,
            Format::Raw => Class::Raw,
            Format::RawJpeg => Class::Both,
        };
        if classify(&self.current) == class {
            return Some(&self.current);
        }
        if !self.writable {
            return None;
        }
        let mut best: Option<&String> = None;
        for c in self.choices.iter().filter(|c| classify(c) == class) {
            if best.is_none_or(|b| score(c) > score(b)) {
                best = Some(c);
            }
        }
        best
    }
}

/// ISO or shutter speed as the camera lists them: what to write, how to show it, and where the camera is now.
#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub name: &'static str,
    pub values: Vec<String>,
    pub labels: Vec<String>,
    /// None when the camera is on something not listed (Bulb).
    pub current: Option<usize>,
}

impl Menu {
    fn json(&self) -> Value {
        json!({"values": self.values, "labels": self.labels, "current": self.current})
    }
}

/// A shutter speed the way cameras show it: "0,0250s" and "1/40" are 1/40, "10/25" is 0.4 s, "30" is 30 s. None for what isn't a time.
pub fn shutter_label(raw: &str) -> Option<String> {
    shutter_seconds(raw).map(shutter_text)
}

/// How long a shutter speed in the camera's words keeps the shutter open, in seconds. None for what isn't a time.
pub fn shutter_seconds(raw: &str) -> Option<f64> {
    telescope_core::shot::dslr_shutter_seconds(raw)
}

/// How long to wait for a photo: `base` for an ordinary one, and for a long exposure (10 s and up) at least twice the exposure and a minute
/// (the camera's own noise reduction takes about as long again as the exposure).
pub fn wait_for_photo(base: Duration, exposure_s: Option<f64>) -> Duration {
    match exposure_s.filter(|s| s.is_finite() && *s >= 10.0) {
        Some(exposure) => base.max(Duration::from_secs_f64(exposure.min(24.0 * 3600.0) * 2.0 + 60.0)),
        None => base,
    }
}

/// What the camera takes right now.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PhotoCaps {
    pub quality: Option<Quality>,
    pub iso: Option<Menu>,
    pub shutter: Option<Menu>,
    /// The shutter speed the camera is on, even when it is one the card can't pick (read-only on a mode dial setting): what the wait for a photo goes by.
    pub shutter_now: Option<String>,
}

impl PhotoCaps {
    /// The JSON the Photos card reads, in the shape the phone's stills service gives (formats, burst_max) plus the camera's own.
    pub fn json(&self) -> Value {
        let raw = self.quality.as_ref().is_some_and(|q| {
            if q.writable { q.choices.iter().any(|c| matches!(classify(c), Class::Raw | Class::Both)) } else { matches!(classify(&q.current), Class::Raw | Class::Both) }
        });
        let formats = if raw { vec!["jpeg", "raw"] } else { vec!["jpeg"] };
        let mut out = json!({"formats": formats, "camera": true, "burst_max": 1});
        for (key, menu) in [("iso", &self.iso), ("shutter", &self.shutter)] {
            if let Some(menu) = menu {
                out[key] = menu.json();
            }
        }
        out
    }
}

/// A setting the camera lets the card pick from now: a list of choices it isn't holding read-only.
fn menu(rig: &mut dyn Rig, names: &'static [&'static str], label: fn(&str) -> Option<String>, skip: fn(&str) -> bool) -> Option<Menu> {
    let (name, s) = resolve(rig, names)?;
    menu_of(name, &s, label, skip)
}

fn menu_of(name: &'static str, s: &Setting, label: fn(&str) -> Option<String>, skip: fn(&str) -> bool) -> Option<Menu> {
    if s.readonly || !matches!(s.kind, Kind::Choice) {
        return None;
    }
    let (mut values, mut labels) = (Vec::new(), Vec::new());
    for choice in s.choices.iter().filter(|c| !skip(c)) {
        labels.push(label(choice).unwrap_or_else(|| choice.clone()));
        values.push(choice.clone());
    }
    let current = values.iter().position(|v| *v == s.value);
    (values.len() > 1).then_some(Menu { name, values, labels, current })
}

pub fn read_photo_caps(rig: &mut dyn Rig) -> PhotoCaps {
    let quality = resolve(rig, &QUALITY).filter(|(_, s)| matches!(s.kind, Kind::Choice)).map(|(name, s)| Quality {
        name,
        writable: !s.readonly && s.choices.len() > 1,
        choices: s.choices,
        current: s.value,
    });
    let iso = menu(rig, &ISO, |_| None, |_| false);
    // Bulb is a button held down, not a speed: it isn't offered
    let setting = resolve(rig, &SHUTTER);
    let shutter_now = setting.as_ref().map(|(_, s)| s.value.clone());
    let shutter = setting.and_then(|(name, s)| menu_of(name, &s, shutter_label, |c| c.to_lowercase().contains("bulb")));
    PhotoCaps { quality, iso, shutter, shutter_now }
}

/// What the card asked a photo to be (the /v1/still options it sends, as far as a camera takes them).
#[derive(Clone, Debug, PartialEq)]
pub struct Wish {
    pub format: Format,
    /// The camera's own value to put ISO and shutter speed to, when the card picked one.
    pub iso: Option<String>,
    pub shutter: Option<String>,
    pub timer_s: u64,
    pub shot: Option<Value>,
    pub photo_transform: Option<telescope_core::photo::PhotoTransform>,
}

impl Wish {
    pub fn from_options(o: &Value) -> Wish {
        let text = |key: &str| o.get(key).and_then(Value::as_str).filter(|v| !v.is_empty()).map(String::from);
        let raw = o.get("raw").is_some_and(|r| r.as_i64() == Some(1) || r.as_bool() == Some(true));
        let format = match (raw, o.get("format").and_then(Value::as_str)) {
            (true, Some("none")) => Format::Raw,
            (true, _) => Format::RawJpeg,
            _ => Format::Jpeg,
        };
        Wish { format, iso: text("iso"), shutter: text("shutter"), shot: o.get("shot").cloned(), timer_s: o.get("timer_s").and_then(Value::as_u64).unwrap_or(0).min(60),
               photo_transform: o.get("_photo_transform").and_then(|v| serde_json::from_value(v.clone()).ok()) }
    }
}

/// Why a photo didn't happen.
#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    Busy,
    /// The camera won't take that setting in its mode now.
    Setting(&'static str),
    Bulb,
    Unsupported,
    Shot(telescope_core::shot::ShotRefused),
    Restore,
    Camera(String),
    Save,
    Interrupted,
    Timeout,
    /// The picture went hidden before the shutter opened or while the photo was being taken or saved.
    Hidden,
    /// The camera's image quality is read-only on a setting that also makes RAW, so a JPEG-only photo can't be had.
    Locked(Class),
    /// A RAW and JPEG photo with a file missing: the files that did come are still saved.
    Partial(Vec<Shot>),
}

impl Failure {
    fn code(&self) -> &'static str {
        match self {
            Failure::Busy => "busy",
            Failure::Setting(_) => "bad_option",
            Failure::Bulb => "bulb",
            Failure::Unsupported => "unsupported",
            Failure::Shot(why) => why.code,
            Failure::Restore => "restore",
            Failure::Camera(_) => "failed",
            Failure::Save => "save",
            Failure::Interrupted => "interrupted",
            Failure::Timeout => "timeout",
            Failure::Hidden => "hidden",
            Failure::Locked(_) => "unsupported",
            Failure::Partial(_) => "partial",
        }
    }

    fn text(&self) -> String {
        if let Failure::Shot(why) = self {
            return why.why.clone();
        }
        tr(match self {
            Failure::Busy => n_("The camera is busy. Try again in a moment."),
            Failure::Setting(_) => n_("The camera's mode doesn't allow that setting."),
            Failure::Bulb => n_("The camera is on Bulb. Pick a shutter speed first."),
            Failure::Unsupported => n_("This camera can't take that kind of photo."),
            Failure::Shot(_) => unreachable!(),
            Failure::Restore => n_("The camera's photo settings could not be restored. Check its ISO and shutter speed."),
            Failure::Camera(_) => n_("The camera couldn't take the photo."),
            Failure::Save => n_("Telescope couldn't save the photo."),
            Failure::Interrupted => n_("The photo was interrupted because the camera stopped."),
            Failure::Timeout => n_("The camera took too long to take the photo. The photo is saved and you get a notification if it still arrives."),
            Failure::Hidden => n_("No photo was taken while part of the picture is hidden."),
            Failure::Locked(Class::Both) => n_("The camera can't take a JPEG alone with its image quality locked to RAW and JPEG together."),
            Failure::Locked(_) => n_("The camera can't take a JPEG with its image quality locked to RAW."),
            Failure::Partial(_) => n_("The camera only sent part of the photo. The part that arrived is saved."),
        })
    }
}

/// Puts a menu to the value asked for, when it isn't there already.
fn put(rig: &mut dyn Rig, menu: Option<&Menu>, want: &str, what: &'static str) -> Result<(), Failure> {
    let menu = menu.ok_or(Failure::Setting(what))?;
    if !menu.values.iter().any(|v| v == want) {
        return Err(Failure::Setting(what));
    }
    if menu.current.and_then(|i| menu.values.get(i)).is_some_and(|v| v == want) {
        return Ok(());
    }
    rig.write(menu.name, want).map_err(|why| {
        log::debug!("the camera refused {what} = {want}: {why}");
        Failure::Setting(what)
    })
}

/// The shooting itself, on the camera's thread: live view off (a camera won't shoot with it on), the settings, the picture to this
/// computer, and every file it made. Live view is the caller's to bring back.
pub fn take_photo(rig: &mut dyn Rig, wish: &Wish) -> Result<Vec<Shot>, Failure> {
    let report = take_photo_guarded(rig, wish, None);
    if report.restore_errors.is_empty() { report.result } else { Err(Failure::Restore) }
}

/// `take_photo` that looks at the guard right before the shutter, after every setting: a hidden or stopped stream gets no photo.
fn take_photo_guarded(rig: &mut dyn Rig, wish: &Wish, guard: Option<&Guard>) -> CaptureReport {
    let mut transaction = Transaction { rig, changed: Vec::new() };
    let mut applied = Value::Null;
    let result = catch_unwind(AssertUnwindSafe(|| take_photo_inner(&mut transaction, wish, guard, &mut applied)))
        .unwrap_or_else(|_| Err(Failure::Camera("the camera panicked while taking the photo".into())));
    let mut readback = applied.clone();
    if catch_unwind(AssertUnwindSafe(|| refresh_applied(transaction.rig, &mut readback))).is_ok() {
        applied = readback;
    } else {
        log::warn!("the camera panicked while reading the photo settings");
    }
    let restore_errors = transaction.restore();
    CaptureReport { result, applied, restore_errors }
}

fn refresh_applied(rig: &mut dyn Rig, applied: &mut Value) {
    let Some(values) = applied.as_object_mut() else { return };
    if values.contains_key("iso") {
        if let Some(iso) = resolve(rig, &ISO).and_then(|(_, read)| telescope_core::shot::dslr_iso(&read.value)) {
            values.insert("iso".into(), json!(iso as i64));
        }
    }
    if values.contains_key("shutter_s") {
        if let Some(seconds) = resolve(rig, &SHUTTER).and_then(|(_, read)| shutter_seconds(&read.value)) {
            values.insert("shutter_s".into(), json!(seconds));
        }
    }
}

struct Transaction<'a> {
    rig: &'a mut dyn Rig,
    changed: Vec<(&'static str, String)>,
}

impl Transaction<'_> {
    fn apply(&mut self, changes: Vec<(&'static str, String)>) -> Result<(), Failure> {
        let mut pending = Vec::new();
        for (name, want) in changes {
            let before = self.rig.read(name).ok_or(Failure::Setting(name))?;
            if before.readonly || !before.choices.contains(&want) {
                return Err(Failure::Setting(name));
            }
            if before.value != want {
                pending.push((name, before.value, want));
            }
        }
        for (name, before, want) in pending {
            // Record before writing because a camera may change the value and still report an error.
            self.changed.push((name, before));
            self.rig.write(name, &want).map_err(|_| Failure::Setting(name))?;
        }
        Ok(())
    }

    fn restore(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        while let Some((name, before)) = self.changed.pop() {
            let restored = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
                self.rig.write(name, &before)?;
                match self.rig.read(name) {
                    Some(read) if read.value == before => Ok(()),
                    _ => Err("the camera did not confirm the restored value".into()),
                }
            }));
            let error = match restored {
                Ok(Ok(())) => continue,
                Ok(Err(why)) => why,
                Err(_) => "the camera panicked while restoring the setting".into(),
            };
            log::warn!("couldn't restore camera {name}: {error}");
            errors.push(format!("{name}: {error}"));
        }
        errors
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn take_photo_inner(transaction: &mut Transaction<'_>, wish: &Wish, guard: Option<&Guard>, applied: &mut Value) -> Result<Vec<Shot>, Failure> {
    let rig = &mut *transaction.rig;
    let _ = rig.write("viewfinder", "0");
    let caps = read_photo_caps(rig);
    let mut options = serde_json::Map::new();
    if let Some(shot) = &wish.shot {
        options.insert("shot".into(), shot.clone());
    }
    let mapped = telescope_core::shot::apply_to_dslr(&mut options, &telescope_core::shot::dslr_ranges(&caps.json())).map_err(Failure::Shot)?;
    let mut changes = Vec::new();
    if let Some(iso) = mapped.get("iso").and_then(Value::as_f64) {
        let menu = caps.iso.as_ref().ok_or(Failure::Setting("ISO"))?;
        let raw = menu.values.iter().find(|raw| telescope_core::shot::dslr_iso(raw) == Some(iso)).ok_or(Failure::Setting("ISO"))?;
        changes.push((menu.name, raw.clone()));
    }
    if let Some(seconds) = mapped.get("shutter_s").and_then(Value::as_f64) {
        let menu = caps.shutter.as_ref().ok_or(Failure::Setting("shutter"))?;
        let raw = menu.values.iter().find(|raw| shutter_seconds(raw) == Some(seconds)).ok_or(Failure::Setting("shutter"))?;
        changes.push((menu.name, raw.clone()));
    }
    let mut expect = 1;
    let mut switched = false;
    if let Some(q) = &caps.quality {
        match q.target(wish.format) {
            Some(choice) => {
                if *choice != q.current {
                    rig.write(q.name, choice).map_err(|_| Failure::Setting("format"))?;
                    switched = true;
                }
                if classify(choice) == Class::Both {
                    expect = 2;
                }
            }
            // A JPEG ask on a camera locked to something that makes RAW would save what wasn't picked
            None if wish.format == Format::Jpeg && matches!(classify(&q.current), Class::Raw | Class::Both) => return Err(Failure::Locked(classify(&q.current))),
            // Otherwise the camera makes whatever it is set to, which is all a JPEG ask can mean here
            None if wish.format == Format::Jpeg => {}
            None => return Err(Failure::Unsupported),
        }
    } else if wish.format != Format::Jpeg {
        return Err(Failure::Unsupported);
    }
    if let Some(iso) = &wish.iso {
        put(rig, caps.iso.as_ref(), iso, "ISO")?;
    }
    match &wish.shutter {
        Some(shutter) => put(rig, caps.shutter.as_ref(), shutter, "shutter")?,
        None if mapped.get("shutter_s").is_none() && caps.shutter_now.as_ref().is_some_and(|s| s.to_lowercase().contains("bulb")) => return Err(Failure::Bulb),
        None => {}
    }
    // To this computer, not the card
    if let Some((name, s)) = resolve(rig, &TARGET) {
        if let Some(ram) = s.choices.iter().find(|c| c.to_lowercase().contains("ram")).filter(|ram| **ram != s.value && !s.readonly) {
            if let Err(why) = rig.write(name, ram) {
                log::debug!("couldn't send photos to the computer: {why}");
            }
        }
    }
    transaction.apply(changes)?;
    let rig = &mut *transaction.rig;
    if wish.shot.is_some() {
        let mut readback = serde_json::Map::new();
        if mapped.get("iso").is_some() {
            let (_, read) = resolve(rig, &ISO).ok_or(Failure::Setting("ISO"))?;
            let iso = telescope_core::shot::dslr_iso(&read.value).ok_or(Failure::Setting("ISO"))?;
            readback.insert("iso".into(), json!(iso as i64));
        }
        if mapped.get("shutter_s").is_some() {
            let (_, read) = resolve(rig, &SHUTTER).ok_or(Failure::Setting("shutter"))?;
            let seconds = shutter_seconds(&read.value).ok_or(Failure::Setting("shutter"))?;
            readback.insert("shutter_s".into(), json!(seconds));
        }
        *applied = Value::Object(readback);
    }
    if let Some(stop) = guard.and_then(Guard::stop) {
        return Err(stop);
    }
    let covered = || guard.is_some_and(Guard::is_hidden);
    let mut shot = rig.capture(expect, &covered);
    // A camera that was just told another image quality can fail the first shot while it settles (a D5100 did on a RAW after a JPEG)
    // Only when the camera hadn't started the shot: after the shutter fired, a second try would be a second picture
    if switched && shot.as_ref().is_err_and(|e| e.before_shutter && e.shots.is_empty()) {
        log::info!("the camera wasn't ready for the first shot after changing the image quality, trying once more");
        std::thread::sleep(AFTER_SWITCH);
        if let Some(stop) = guard.and_then(Guard::stop) {
            return Err(stop);
        }
        if !covered() {
            shot = rig.capture(expect, &covered);
        }
    }
    let taken = shot.map_err(|Incomplete { why, shots, .. }| {
        log::info!("the camera couldn't take the photo: {why}");
        if !shots.is_empty() {
            Failure::Partial(shots)
        } else if why.to_lowercase().contains("busy") {
            Failure::Busy
        } else {
            Failure::Camera(why)
        }
    });
    // Hiding began after the shutter opened (a long exposure, the transfer): whatever came is discarded unseen
    if covered() {
        return Err(Failure::Hidden);
    }
    taken
}

/// What to do with a photo that came after its job gave up waiting: save it and say so.
type Late = Arc<dyn Fn(CaptureReport) + Send + Sync>;

struct Job {
    wish: Wish,
    reply: Reply,
    late: Option<Late>,
    guard: Option<Guard>,
}

/// What stops a photo from being taken: the picture going hidden, or the stream going away. Shared by the job and the camera's thread.
#[derive(Clone)]
struct Guard {
    hidden: Option<Arc<AtomicBool>>,
    alive: Arc<Mutex<Box<dyn Fn() -> bool + Send>>>,
}

impl Guard {
    fn is_hidden(&self) -> bool {
        self.hidden.as_ref().is_some_and(|h| h.load(Ordering::SeqCst))
    }

    fn is_alive(&self) -> bool {
        (*self.alive.lock().unwrap_or_else(|e| e.into_inner()))()
    }

    /// Why the shutter must stay shut, if it must.
    fn stop(&self) -> Option<Failure> {
        if !self.is_alive() {
            Some(Failure::Interrupted)
        } else if self.is_hidden() {
            Some(Failure::Hidden)
        } else {
            None
        }
    }
}

type Outcome = Result<Vec<Shot>, Failure>;

#[derive(Debug)]
struct CaptureReport {
    result: Outcome,
    applied: Value,
    restore_errors: Vec<String>,
}

#[cfg(test)]
impl From<Outcome> for CaptureReport {
    fn from(result: Outcome) -> Self {
        Self { result, applied: Value::Null, restore_errors: Vec::new() }
    }
}

impl CaptureReport {
    fn annotate(&self, mut data: Value) -> Value {
        if !self.applied.is_null() {
            data["applied"] = self.applied.clone();
        }
        data["restore"] = json!({"ok": self.restore_errors.is_empty(), "errors": self.restore_errors});
        if !self.restore_errors.is_empty() {
            data["ok"] = json!(false);
            data["code"] = json!(Failure::Restore.code());
            let earlier = data.get("error").and_then(Value::as_str).unwrap_or("");
            data["error"] = json!(if earlier.is_empty() { Failure::Restore.text() } else { format!("{earlier} {}", Failure::Restore.text()) });
        }
        data
    }
}

enum Slot {
    Waiting,
    Ready(CaptureReport),
    /// The camera's side was dropped without an answer.
    Closed,
    /// The job stopped waiting, so a photo that comes now goes the late way.
    Abandoned,
}

struct Handover {
    slot: Mutex<Slot>,
    changed: Condvar,
}

/// The camera side of one photo's answer. The photo is either taken by the waiting job or handed back to be saved late, never both and never neither.
struct Reply(Arc<Handover>);

/// The job's side: waits for the photo, and gives up in one step that also takes a photo that has just come.
struct Waiter(Arc<Handover>);

enum Wait {
    Got(CaptureReport),
    Timeout,
    Closed,
}

fn handover() -> (Reply, Waiter) {
    let inner = Arc::new(Handover { slot: Mutex::new(Slot::Waiting), changed: Condvar::new() });
    (Reply(inner.clone()), Waiter(inner))
}

impl Reply {
    /// Gives the photo to the job, or back to the caller when the job has stopped waiting.
    fn send(self, outcome: CaptureReport) -> Result<(), Box<CaptureReport>> {
        let mut slot = self.0.slot.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*slot, Slot::Abandoned) {
            return Err(Box::new(outcome));
        }
        *slot = Slot::Ready(outcome);
        self.0.changed.notify_all();
        Ok(())
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        let mut slot = self.0.slot.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*slot, Slot::Waiting) {
            *slot = Slot::Closed;
            self.0.changed.notify_all();
        }
    }
}

impl Waiter {
    fn wait(&self, timeout: Duration) -> Wait {
        let slot = self.0.slot.lock().unwrap_or_else(|e| e.into_inner());
        let (mut slot, _) = self.0.changed.wait_timeout_while(slot, timeout, |s| matches!(s, Slot::Waiting)).unwrap_or_else(|e| e.into_inner());
        match std::mem::replace(&mut *slot, Slot::Waiting) {
            Slot::Ready(outcome) => {
                *slot = Slot::Abandoned;
                Wait::Got(outcome)
            }
            Slot::Closed => {
                *slot = Slot::Closed;
                Wait::Closed
            }
            other => {
                *slot = other;
                Wait::Timeout
            }
        }
    }

    /// Stops waiting. A photo that came in the meantime is returned; one that comes after this is saved late.
    fn abandon(&self) -> Option<CaptureReport> {
        let mut slot = self.0.slot.lock().unwrap_or_else(|e| e.into_inner());
        match std::mem::replace(&mut *slot, Slot::Abandoned) {
            Slot::Ready(outcome) => Some(outcome),
            Slot::Closed => {
                *slot = Slot::Closed;
                None
            }
            _ => None,
        }
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let _ = self.abandon();
    }
}

/// What a camera's thread shares with its photo service while it streams.
#[derive(Default)]
pub struct Shared {
    caps: Mutex<Option<PhotoCaps>>,
    pending: Mutex<Option<Job>>,
    /// The camera's thread is shooting now.
    capturing: AtomicBool,
    /// A photo is being taken, from the countdown to the saved files.
    in_flight: AtomicBool,
}

impl Shared {
    /// Whether the camera is shooting, which sends no live view frames meanwhile.
    pub fn capturing(&self) -> bool {
        self.capturing.load(Ordering::SeqCst)
    }

    /// What the camera takes, as read now (None once it lets go); whether that's a change.
    pub fn set_caps(&self, caps: Option<PhotoCaps>) -> bool {
        let mut held = self.caps.lock().unwrap();
        let changed = *held != caps;
        *held = caps;
        changed
    }

    pub fn caps_json(&self) -> Option<Value> {
        self.caps.lock().unwrap().as_ref().map(PhotoCaps::json)
    }

    /// The camera's thread is ending: nothing is offered, and a job still waiting for it ends as interrupted.
    pub fn close(&self) {
        self.set_caps(None);
        self.pending.lock().unwrap().take();
    }

    /// Takes the photo asked for, if one is, and brings live view back. Whether there was one.
    pub fn run_pending(&self, rig: &mut dyn Rig) -> bool {
        let Some(job) = self.pending.lock().unwrap().take() else { return false };
        self.capturing.store(true, Ordering::SeqCst);
        let result = take_photo_guarded(rig, &job.wish, job.guard.as_ref());
        let caps = read_photo_caps(rig);
        self.set_caps(Some(caps));
        let _ = rig.write("viewfinder", "1");
        self.capturing.store(false, Ordering::SeqCst);
        // The job may have stopped waiting (a timeout, or the stream stopping): what the camera sent after that is not thrown away
        if let Err(result) = job.reply.send(result) {
            if let Some(late) = job.late {
                late(*result);
            }
        }
        true
    }
}

type OnChange = Arc<dyn Fn() + Send + Sync>;
/// Where a camera is now: the location its device has at this moment, which changes when it comes back on another USB port.
pub type Locate = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Someone who wants to hear about one camera's changes, wherever that camera is on the bus when they happen.
struct Hearer {
    locate: Locate,
    on_change: Mutex<Option<OnChange>>,
}

impl Hearer {
    fn port(&self) -> Option<String> {
        (self.locate)().map(|location| gphoto_port(&location).to_string())
    }
}

/// Every photo service there is, which hear what happens at the port their camera is at now.
fn hearers() -> &'static Mutex<Vec<std::sync::Weak<Hearer>>> {
    static HEARERS: OnceLock<Mutex<Vec<std::sync::Weak<Hearer>>>> = OnceLock::new();
    HEARERS.get_or_init(Mutex::default)
}

/// Tells whoever listens to the camera on this port that what it takes may have changed. A camera that comes back on another port
/// is told about there too, since it is found by where its device is now and not where it was when it was asked to listen.
pub fn notify_changed(port: &str) {
    let hearing: Vec<Arc<Hearer>> = {
        let mut all = hearers().lock().unwrap();
        all.retain(|h| h.strong_count() > 0);
        all.iter().filter_map(std::sync::Weak::upgrade).collect()
    };
    for hearer in hearing.into_iter().filter(|h| h.port().as_deref() == Some(port)) {
        let listener = hearer.on_change.lock().unwrap().clone();
        if let Some(listener) = listener {
            listener();
        }
    }
}

fn result(ok: bool, files: &[PathBuf], code: &str, error: &str) -> Value {
    json!({"ok": ok, "files": files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>(), "code": code, "error": error})
}

fn failed(failure: &Failure, files: &[PathBuf]) -> Value {
    result(false, files, failure.code(), &failure.text())
}

fn progress(state: &str, countdown_ms: u128, paused: bool) -> Value {
    json!({"state": state, "shots": 1, "taken": 0, "countdown_ms": countdown_ms as u64, "paused": paused, "saved": 0})
}

/// The extension a shot is saved under: the camera's own kind in lower case, else jpg.
fn ext_of(name: &str) -> String {
    Path::new(name).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase)
        .filter(|e| !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or_else(|| "jpg".into())
}

/// `stem.ext` in folder, taken by making it empty, if nothing has it.
fn reserve_exact(folder: &Path, stem: &str, ext: &str) -> std::io::Result<PathBuf> {
    let path = folder.join(format!("{stem}.{ext}"));
    std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map(|_| path)
}

/// A free name in folder, taken by making it empty so nothing else saving at the same time can have it.
fn reserve(folder: &Path, stem: &str, ext: &str) -> std::io::Result<PathBuf> {
    for n in 1..1000 {
        let name = if n == 1 { format!("{stem}.{ext}") } else { format!("{stem} ({n}).{ext}") };
        let path = folder.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free name"))
}

/// The files of a photo into folder, named after the moment so a RAW and its JPEG share a name. Each is written as a `.part` file and renamed
/// when whole. `covered` says the picture is hidden: it is looked at before and after every file, and the photo's files are all removed then.
#[cfg(test)]
fn save(folder: &Path, shots: &[Shot], covered: &dyn Fn() -> bool) -> Result<Vec<PathBuf>, (Failure, Vec<PathBuf>)> {
    save_transformed(folder, shots, covered, None)
}

fn save_transformed(folder: &Path, shots: &[Shot], covered: &dyn Fn() -> bool, transform: Option<&telescope_core::photo::PhotoTransform>) -> Result<Vec<PathBuf>, (Failure, Vec<PathBuf>)> {
    save_as(folder, &format!("Telescope_{}", telescope_core::time::local_stamp()), shots, covered, transform)
}

/// `save_transformed` under a name that is given.
fn save_as(folder: &Path, stem: &str, shots: &[Shot], covered: &dyn Fn() -> bool, transform: Option<&telescope_core::photo::PhotoTransform>) -> Result<Vec<PathBuf>, (Failure, Vec<PathBuf>)> {
    let mut files: Vec<PathBuf> = Vec::new();
    let discard = |files: &[PathBuf]| files.iter().for_each(|f| { let _ = std::fs::remove_file(f); });
    let exts: Vec<String> = shots.iter().map(|shot| ext_of(&shot.name)).collect();
    // One name for all the files of the photo, so a RAW and its JPEG never end up with a number on only one of them
    let using = telescope_core::names::free_stem(folder, stem, &exts.iter().map(String::as_str).collect::<Vec<_>>());
    for (shot, ext) in shots.iter().zip(&exts) {
        if covered() {
            discard(&files);
            return Err((Failure::Hidden, Vec::new()));
        }
        let path = match reserve_exact(folder, &using, ext).or_else(|_| reserve(folder, stem, ext)) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("can't save the photo in {}: {e}", folder.display());
                return Err((Failure::Save, files));
            }
        };
        let part = path.with_file_name(format!("{}.part", path.file_name().unwrap_or_default().to_string_lossy()));
        // A photo that can't be turned or cropped is kept as the camera took it
        let written = std::fs::write(&part, &shot.data).map(|()| {
            if let Some(t) = transform { telescope_core::photo::apply_file_or_keep(&part, t); }
        });
        // The last look before the file is kept
        let hidden = covered();
        let kept = if hidden { Ok(()) } else { written.and_then(|()| std::fs::rename(&part, &path)) };
        if hidden || kept.is_err() {
            let _ = std::fs::remove_file(&part);
            let _ = std::fs::remove_file(&path);
        }
        if hidden {
            discard(&files);
            return Err((Failure::Hidden, Vec::new()));
        }
        if let Err(e) = kept {
            log::warn!("can't save {}: {e}", path.display());
            return Err((Failure::Save, files));
        }
        files.push(path);
    }
    // A cover can land while the last file is renamed
    if covered() {
        discard(&files);
        return Err((Failure::Hidden, Vec::new()));
    }
    Ok(files)
}

/// How long the shutter will stay open for this photo: the speed asked for, else the one the camera is on.
fn exposure_of(shared: &Shared, wish: &Wish) -> Option<f64> {
    if let Some(seconds) = wish.shot.as_ref().and_then(|s| s.get("shutter_s")).and_then(Value::as_f64).filter(|s| s.is_finite() && *s > 0.0) {
        return Some(seconds);
    }
    if let Some(speed) = &wish.shutter {
        return shutter_seconds(speed);
    }
    let caps = shared.caps.lock().unwrap();
    let caps = caps.as_ref()?;
    match &caps.shutter {
        Some(menu) => shutter_seconds(menu.values.get(menu.current?)?),
        None => shutter_seconds(caps.shutter_now.as_deref()?),
    }
}

/// One photo from start to saved files, on the thread of its own: the countdown, the camera's thread doing the shooting, the files.
fn run_job(shared: &Shared, wish: Wish, folder: &Path, on_progress: &dyn Fn(Value), guard: &Guard, longest: Duration, late: Option<Late>) -> Value {
    let photo_transform = wish.photo_transform.clone();
    let is_hidden = || guard.is_hidden();
    let alive = || guard.is_alive();
    if let Err(e) = std::fs::create_dir_all(folder) {
        log::warn!("can't make the photo folder {}: {e}", folder.display());
        return failed(&Failure::Save, &[]);
    }
    if is_hidden() {
        return failed(&Failure::Hidden, &[]);
    }
    let until = Instant::now() + Duration::from_secs(wish.timer_s);
    let mut shown = u64::MAX;
    while Instant::now() < until {
        if !alive() {
            return failed(&Failure::Interrupted, &[]);
        }
        let left = until.saturating_duration_since(Instant::now());
        let seconds = left.as_millis().div_ceil(1000) as u64;
        if seconds != shown {
            shown = seconds;
            on_progress(progress("waiting", left.as_millis(), false));
        }
        std::thread::sleep(left.min(Duration::from_millis(50)));
    }
    // The last look before the shutter: a cover can land while the self-timer counts
    if is_hidden() {
        return failed(&Failure::Hidden, &[]);
    }
    let longest = wait_for_photo(longest, exposure_of(shared, &wish));
    let (reply, waiter) = handover();
    *shared.pending.lock().unwrap() = Some(Job { wish, reply, late, guard: Some(guard.clone()) });
    on_progress(progress("capturing", 0, true));
    let (began, mut looks) = (Instant::now(), 0u32);
    let outcome = loop {
        match waiter.wait(LOOK) {
            Wait::Got(outcome) => break outcome,
            Wait::Closed => return failed(&Failure::Interrupted, &[]),
            Wait::Timeout => {
                let why = if !alive() {
                    Failure::Interrupted
                } else if began.elapsed() > longest {
                    Failure::Timeout
                } else {
                    looks += 1;
                    if looks % REPORT_EVERY == 0 {
                        on_progress(progress("capturing", 0, true));
                    }
                    continue;
                };
                shared.pending.lock().unwrap().take();
                // A photo that arrived just now is still this job's, one that comes after this is saved late
                match waiter.abandon() {
                    Some(outcome) => break outcome,
                    None => return failed(&why, &[]),
                }
            }
        }
    };
    if let Some(stop) = guard.stop() {
        return outcome.annotate(failed(&stop, &[]));
    }
    let covered = || guard.stop().is_some();
    let data = match &outcome.result {
        Ok(shots) => match save_transformed(folder, shots, &covered, photo_transform.as_ref()) {
            Ok(files) => result(true, &files, "", ""),
            Err((failure, files)) => failed(&guard.stop().unwrap_or(failure), &files),
        },
        // Whatever came of a photo that is only half there is kept, and the card says it is half
        Err(Failure::Partial(shots)) => {
            let files = match save_transformed(folder, shots, &covered, photo_transform.as_ref()) {
                Ok(files) => files,
                Err((Failure::Hidden, _)) => return outcome.annotate(failed(&guard.stop().unwrap_or(Failure::Hidden), &[])),
                Err((_, files)) => files,
            };
            failed(&Failure::Partial(shots.clone()), &files)
        }
        Err(failure) => failed(failure, &[]),
    };
    outcome.annotate(data)
}

/// The Photos card's way to a DSLR: only while the camera streams, since only the stream's thread can talk to it.
pub struct GphotoStills {
    hearer: Arc<Hearer>,
    longest: Duration,
}

impl GphotoStills {
    /// The camera at this location, for good.
    pub fn new(location: &str) -> GphotoStills {
        let location = location.to_string();
        GphotoStills::following(Arc::new(move || Some(location.clone())))
    }

    /// The camera of a device, wherever it is on the bus at the moment it is asked: it can come back on another port after the
    /// stream dropped, and this service still has to be the one for it.
    pub fn following(locate: Locate) -> GphotoStills {
        let hearer = Arc::new(Hearer { locate, on_change: Mutex::new(None) });
        hearers().lock().unwrap().push(Arc::downgrade(&hearer));
        GphotoStills { hearer, longest: LONGEST }
    }

    fn shared(&self) -> Option<Arc<Shared>> {
        stills_of(&self.hearer.port()?)
    }
}

impl SourceStills for GphotoStills {
    fn caps(&self) -> Option<Value> {
        self.shared()?.caps_json()
    }

    fn busy(&self) -> bool {
        self.shared().is_some_and(|s| s.in_flight.load(Ordering::SeqCst))
    }

    fn take(&self, options: &Value, folder: &str, on_progress: Box<dyn Fn(Value) + Send>, on_done: Box<dyn FnOnce(Value) + Send>,
            alive: Box<dyn Fn() -> bool + Send>) -> bool {
        self.start(options, folder, on_progress, on_done, alive, None)
    }

    fn take_unless_hidden(&self, options: &Value, folder: &str, on_progress: Box<dyn Fn(Value) + Send>, on_done: Box<dyn FnOnce(Value) + Send>,
                          alive: Box<dyn Fn() -> bool + Send>, hidden: Arc<AtomicBool>) -> bool {
        self.start(options, folder, on_progress, on_done, alive, Some(hidden))
    }

    fn set_on_change(&self, on_change: Box<dyn Fn() + Send + Sync>) {
        *self.hearer.on_change.lock().unwrap() = Some(Arc::from(on_change));
    }
}

impl GphotoStills {
    fn start(&self, options: &Value, folder: &str, on_progress: Box<dyn Fn(Value) + Send>, on_done: Box<dyn FnOnce(Value) + Send>,
             alive: Box<dyn Fn() -> bool + Send>, hidden: Option<Arc<AtomicBool>>) -> bool {
        let Some(shared) = self.shared() else { return false };
        if shared.caps_json().is_none() || shared.in_flight.swap(true, Ordering::SeqCst) {
            return false;
        }
        let (wish, folder, longest) = (Wish::from_options(options), PathBuf::from(folder), self.longest);
        let held = shared.clone();
        // Progress can still be told after the job ends, for a photo that comes late
        let on_progress = Arc::new(Mutex::new(on_progress));
        let tell = on_progress.clone();
        let late_folder = folder.clone();
        let guard = Guard { hidden, alive: Arc::new(Mutex::new(alive)) };
        let late_guard = guard.clone();
        let late_transform = wish.photo_transform.clone();
        let late: Late = Arc::new(move |outcome| {
            let shots = match &outcome.result {
                Ok(shots) | Err(Failure::Partial(shots)) => shots.as_slice(),
                Err(_) => &[],
            };
            // A timeout keeps the photo, but cancellation or a hidden picture discards it.
            let covered = || late_guard.stop().is_some();
            let files = save_transformed(&late_folder, shots, &covered, late_transform.as_ref()).unwrap_or_else(|(_, files)| files);
            if !files.is_empty() || !outcome.restore_errors.is_empty() {
                let said = outcome.annotate(json!({"state": "late", "files": files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>() }));
                (*tell.lock().unwrap_or_else(|e| e.into_inner()))(said);
            }
        });
        let spawned = std::thread::Builder::new().name("dslr-photo".into()).spawn(move || {
            let report = |data: Value| (*on_progress.lock().unwrap_or_else(|e| e.into_inner()))(data);
            let outcome = run_job(&held, wish, &folder, &report, &guard, longest, Some(late));
            held.in_flight.store(false, Ordering::SeqCst);
            on_done(outcome);
        });
        if spawned.is_err() {
            shared.in_flight.store(false, Ordering::SeqCst);
            return false;
        }
        true
    }
}

#[cfg(test)]
#[path = "gphoto_stills_tests.rs"]
mod tests;
