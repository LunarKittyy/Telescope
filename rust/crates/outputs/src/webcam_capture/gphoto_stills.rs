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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use telescope_core::i18n::tr;
use telescope_core::source::SourceStills;

/// The camera's own names for what a photo needs, each listing the names to try in turn (Nikon, then Canon and others).
const QUALITY: [&str; 2] = ["imagequality", "imageformat"];
const ISO: [&str; 1] = ["iso"];
const SHUTTER: [&str; 2] = ["shutterspeed", "shutterspeed2"];
const TARGET: [&str; 1] = ["capturetarget"];
/// A photo that hasn't come back by now is given up on (a 30 s exposure with noise reduction took 67 s on a D5100).
const LONGEST: Duration = Duration::from_secs(4 * 60);
/// How often the job looks at its camera's thread, and how many looks make one progress report.
const LOOK: Duration = Duration::from_millis(250);
const REPORT_EVERY: u32 = 4;

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
    pub bulb: bool,
}

impl Menu {
    fn json(&self) -> Value {
        json!({"values": self.values, "labels": self.labels, "current": self.current})
    }
}

/// A shutter speed the way cameras show it: "0,0250s" and "1/40" are 1/40, "10/25" is 0.4 s, "30" is 30 s. None for what isn't a time.
pub fn shutter_label(raw: &str) -> Option<String> {
    let t = raw.trim().trim_end_matches(['s', 'S', '"']).replace(',', ".");
    let seconds = match t.split_once('/') {
        Some((n, d)) => n.trim().parse::<f64>().ok()? / d.trim().parse::<f64>().ok().filter(|d| *d != 0.0)?,
        None => t.parse::<f64>().ok()?,
    };
    (seconds > 0.0 && seconds.is_finite()).then(|| shutter_text(seconds))
}

/// What the camera takes right now.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PhotoCaps {
    pub quality: Option<Quality>,
    pub iso: Option<Menu>,
    pub shutter: Option<Menu>,
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
    let (name, s): (&'static str, Setting) = resolve(rig, names)?;
    if s.readonly || !matches!(s.kind, Kind::Choice) {
        return None;
    }
    let bulb = s.value.to_lowercase().contains("bulb");
    let (mut values, mut labels) = (Vec::new(), Vec::new());
    for choice in s.choices.iter().filter(|c| !skip(c)) {
        labels.push(label(choice).unwrap_or_else(|| choice.clone()));
        values.push(choice.clone());
    }
    let current = values.iter().position(|v| *v == s.value);
    (values.len() > 1).then_some(Menu { name, values, labels, current, bulb })
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
    let shutter = menu(rig, &SHUTTER, shutter_label, |c| c.to_lowercase().contains("bulb"));
    PhotoCaps { quality, iso, shutter }
}

/// What the card asked a photo to be (the /v1/still options it sends, as far as a camera takes them).
#[derive(Clone, Debug, PartialEq)]
pub struct Wish {
    pub format: Format,
    /// The camera's own value to put ISO and shutter speed to, when the card picked one.
    pub iso: Option<String>,
    pub shutter: Option<String>,
    pub timer_s: u64,
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
        Wish { format, iso: text("iso"), shutter: text("shutter"), timer_s: o.get("timer_s").and_then(Value::as_u64).unwrap_or(0).min(60) }
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
    Camera(String),
    Save,
    Interrupted,
    Timeout,
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
            Failure::Camera(_) => "failed",
            Failure::Save => "save",
            Failure::Interrupted => "interrupted",
            Failure::Timeout => "timeout",
            Failure::Partial(_) => "partial",
        }
    }

    fn text(&self) -> String {
        tr(match self {
            Failure::Busy => "The camera is busy. Try again in a moment.",
            Failure::Setting(_) => "The camera's mode doesn't allow that setting.",
            Failure::Bulb => "The camera is on Bulb. Pick a shutter speed first.",
            Failure::Unsupported => "This camera can't take that kind of photo.",
            Failure::Camera(_) => "The camera couldn't take the photo.",
            Failure::Save => "Telescope couldn't save the photo.",
            Failure::Interrupted => "The photo was interrupted because the camera stopped.",
            Failure::Timeout => "The camera took too long to take the photo.",
            Failure::Partial(_) => "The camera only sent part of the photo. What came is saved.",
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
    let _ = rig.write("viewfinder", "0");
    let caps = read_photo_caps(rig);
    let mut expect = 1;
    if let Some(q) = &caps.quality {
        match q.target(wish.format) {
            Some(choice) => {
                if *choice != q.current {
                    rig.write(q.name, choice).map_err(|_| Failure::Setting("format"))?;
                }
                if classify(choice) == Class::Both {
                    expect = 2;
                }
            }
            // The camera makes whatever it is set to, which is all a JPEG ask can mean here
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
        None if caps.shutter.as_ref().is_some_and(|m| m.bulb) => return Err(Failure::Bulb),
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
    rig.capture(expect).map_err(|Incomplete { why, shots }| {
        log::info!("the camera couldn't take the photo: {why}");
        if !shots.is_empty() {
            Failure::Partial(shots)
        } else if why.to_lowercase().contains("busy") {
            Failure::Busy
        } else {
            Failure::Camera(why)
        }
    })
}

struct Job {
    wish: Wish,
    reply: SyncSender<Result<Vec<Shot>, Failure>>,
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
        let result = take_photo(rig, &job.wish);
        let _ = rig.write("viewfinder", "1");
        self.capturing.store(false, Ordering::SeqCst);
        let _ = job.reply.send(result);
        true
    }
}

type OnChange = Arc<dyn Fn() + Send + Sync>;

/// Who wants to hear about each camera's changes, by port: set before the camera streams and kept after it stops.
fn listeners() -> &'static Mutex<HashMap<String, OnChange>> {
    static LISTENERS: OnceLock<Mutex<HashMap<String, OnChange>>> = OnceLock::new();
    LISTENERS.get_or_init(Mutex::default)
}

/// Tells whoever listens to this camera that what it takes may have changed.
pub fn notify_changed(port: &str) {
    let listener = listeners().lock().unwrap().get(port).cloned();
    if let Some(listener) = listener {
        listener();
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

/// The files of a photo into folder, named after the moment so a RAW and its JPEG share a name.
fn save(folder: &Path, shots: &[Shot]) -> Result<Vec<PathBuf>, (Failure, Vec<PathBuf>)> {
    let stem = format!("Telescope_{}", telescope_core::time::local_stamp());
    let mut files: Vec<PathBuf> = Vec::new();
    for shot in shots {
        let ext = Path::new(&shot.name).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase)
            .filter(|e| !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or_else(|| "jpg".into());
        let path = match reserve(folder, &stem, &ext) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("can't save the photo in {}: {e}", folder.display());
                return Err((Failure::Save, files));
            }
        };
        if let Err(e) = std::fs::write(&path, &shot.data) {
            log::warn!("can't save {}: {e}", path.display());
            let _ = std::fs::remove_file(&path);
            return Err((Failure::Save, files));
        }
        files.push(path);
    }
    Ok(files)
}

/// One photo from start to saved files, on the thread of its own: the countdown, the camera's thread doing the shooting, the files.
fn run_job(shared: &Shared, wish: Wish, folder: &Path, on_progress: &dyn Fn(Value), alive: &dyn Fn() -> bool, longest: Duration) -> Value {
    if let Err(e) = std::fs::create_dir_all(folder) {
        log::warn!("can't make the photo folder {}: {e}", folder.display());
        return failed(&Failure::Save, &[]);
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
    let (tx, rx) = sync_channel(1);
    *shared.pending.lock().unwrap() = Some(Job { wish, reply: tx });
    on_progress(progress("capturing", 0, true));
    let (began, mut looks) = (Instant::now(), 0u32);
    let shots = loop {
        match rx.recv_timeout(LOOK) {
            Ok(Ok(shots)) => break shots,
            // Whatever came of a photo that is only half there is kept, and the card says it is half
            Ok(Err(Failure::Partial(shots))) => {
                let files = match save(folder, &shots) {
                    Ok(files) => files,
                    Err((_, files)) => files,
                };
                return failed(&Failure::Partial(shots), &files);
            }
            Ok(Err(failure)) => return failed(&failure, &[]),
            Err(RecvTimeoutError::Disconnected) => return failed(&Failure::Interrupted, &[]),
            Err(RecvTimeoutError::Timeout) => {
                if !alive() {
                    shared.pending.lock().unwrap().take();
                    return failed(&Failure::Interrupted, &[]);
                }
                if began.elapsed() > longest {
                    shared.pending.lock().unwrap().take();
                    return failed(&Failure::Timeout, &[]);
                }
                looks += 1;
                if looks % REPORT_EVERY == 0 {
                    on_progress(progress("capturing", 0, true));
                }
            }
        }
    };
    match save(folder, &shots) {
        Ok(files) => result(true, &files, "", ""),
        Err((failure, files)) => failed(&failure, &files),
    }
}

/// The Photos card's way to a DSLR: only while the camera streams, since only the stream's thread can talk to it.
pub struct GphotoStills {
    port: String,
    longest: Duration,
}

impl GphotoStills {
    pub fn new(location: &str) -> GphotoStills {
        GphotoStills { port: gphoto_port(location).to_string(), longest: LONGEST }
    }
}

impl SourceStills for GphotoStills {
    fn caps(&self) -> Option<Value> {
        stills_of(&self.port)?.caps_json()
    }

    fn busy(&self) -> bool {
        stills_of(&self.port).is_some_and(|s| s.in_flight.load(Ordering::SeqCst))
    }

    fn take(&self, options: &Value, folder: &str, on_progress: Box<dyn Fn(Value) + Send>, on_done: Box<dyn FnOnce(Value) + Send>,
            alive: Box<dyn Fn() -> bool + Send>) -> bool {
        let Some(shared) = stills_of(&self.port) else { return false };
        if shared.caps_json().is_none() || shared.in_flight.swap(true, Ordering::SeqCst) {
            return false;
        }
        let (wish, folder, longest) = (Wish::from_options(options), PathBuf::from(folder), self.longest);
        let held = shared.clone();
        let spawned = std::thread::Builder::new().name("dslr-photo".into()).spawn(move || {
            let outcome = run_job(&held, wish, &folder, &*on_progress, &*alive, longest);
            held.in_flight.store(false, Ordering::SeqCst);
            on_done(outcome);
        });
        if spawned.is_err() {
            shared.in_flight.store(false, Ordering::SeqCst);
            return false;
        }
        true
    }

    fn set_on_change(&self, on_change: Box<dyn Fn() + Send + Sync>) {
        listeners().lock().unwrap().insert(self.port.clone(), Arc::from(on_change));
    }
}

#[cfg(test)]
#[path = "gphoto_stills_tests.rs"]
mod tests;
