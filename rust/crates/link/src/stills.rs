//! Full-quality photos from the phone (stills.py, /v1/still in docs/protocol.md): what the lens can take, and taking
//! them into a folder, one at a time per phone, each on a thread of its own.

use crate::https::{Error, PhoneAuth, Request};
use crate::phones::auth_of;
use crate::util::python_json;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::i18n::tr;
use telescope_core::link::PhoneStills;
use telescope_core::source::PairedPhone;

/// The /v1/still this desktop speaks; a phone sends stills: 1 in /v1/state when it takes it.
pub const STILLS_VERSION: i64 = 1;
const CHUNK: usize = 1 << 20;
const MAX_REPLY: usize = 1 << 20;

/// How long things may take; tests shorten them.
#[derive(Clone, Copy, Debug)]
pub struct Timings {
    pub poll: Duration,
    pub request: Duration,
    /// Between reads of one file, not for all of it.
    pub file: Duration,
    /// The phone not answering for this long ends the photo.
    pub lost_after: Duration,
    /// A photo nobody hears the end of is given up on after this.
    pub longest: Duration,
    /// A photo whose status doesn't move for this long (plus its exposures) is given up on.
    pub stall: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings { poll: Duration::from_millis(400), request: Duration::from_secs(10), file: Duration::from_secs(30),
                  lost_after: Duration::from_secs(30), longest: Duration::from_secs(15 * 60), stall: Duration::from_secs(45) }
    }
}

/// Why a photo didn't happen, in the desktop's language; the phone's English for codes this desktop doesn't know.
pub fn reason_text(code: &str, english: &str) -> String {
    let known = match code {
        "unsupported" => "This phone's camera can't take that photo.",
        "bad_option" => "The phone didn't accept those photo settings.",
        "not_ready" => "The phone isn't ready for photos yet. Try again in a moment.",
        "busy" => "The phone is already taking a photo.",
        "camera_off" => "The phone's camera is off.",
        "interrupted" => "The photo was interrupted by a change to the camera.",
        "timeout" => "The phone took too long to take the photo.",
        "failed" => "The phone couldn't take the photo.",
        "lost" => "Lost the phone while it was taking the photo.",
        "dropped" => "The phone gave up on the photo, maybe because its stream stopped.",
        "save" => "Telescope couldn't save the photo.",
        "no_photos" => "This phone can't take photos from Telescope. Update the app on the phone.",
        _ if english.is_empty() => "The phone couldn't take the photo.",
        _ => return english.to_string(),
    };
    tr(known)
}

/// Python's int(value or 0): None for what int() refuses.
fn py_int(value: Option<&Value>) -> Option<i64> {
    match value {
        None | Some(Value::Null) => Some(0),
        Some(Value::Bool(b)) => Some(*b as i64),
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        Some(Value::String(s)) if s.is_empty() => Some(0),
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(_) => None,
    }
}

fn size(raw: &Map<String, Value>, w: &str, h: &str) -> Option<(i64, i64)> {
    let (width, height) = (py_int(raw.get(w))?, py_int(raw.get(h))?);
    (width > 0 && height > 0).then_some((width, height))
}

/// What the lens a stream is on can take, from the phone's /v1/state. Anything not here isn't offered.
#[derive(Clone, Debug, PartialEq, Serialize)]
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
    /// (mode, (width, height)) for the maker's modes that said their size.
    pub extension_sizes: Vec<(String, (i64, i64))>,
}

impl StillCaps {
    /// The size a photo in this mode comes out at; a maker's mode that didn't say is taken at the plain size.
    pub fn size_for(&self, mode: &str) -> Option<(i64, i64)> {
        if mode == "max_res" && self.max_res.is_some() {
            return self.max_res;
        }
        self.extension_sizes.iter().find(|(m, _)| m == mode).map(|(_, s)| *s).or(self.size)
    }

    /// The current lens's, or None when the phone or this lens can't take photos (or hasn't said yet).
    pub fn from_state(state: Option<&Value>) -> Option<StillCaps> {
        let state = state?.as_object()?;
        if py_int(state.get("stills"))? < STILLS_VERSION {
            return None;
        }
        let current = state.get("cameras").and_then(Value::as_array).into_iter().flatten()
            .filter_map(Value::as_object).find(|c| c.get("current").is_some_and(truthy))?;
        let raw = current.get("stills")?.as_object()?;
        let words = |key: &str| -> Vec<String> {
            raw.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
        };
        let number = |key: &str| py_int(raw.get(key)).map_or(0, |n| n.max(0));
        let flag = |key: &str| raw.get(key) == Some(&Value::Bool(true));
        let mut formats = words("formats");
        if !formats.iter().any(|f| f == "jpeg") {
            formats.insert(0, "jpeg".into());
        }
        let extension_sizes = raw.get("extensionSizes").and_then(Value::as_object).into_iter().flatten()
            .filter_map(|(m, v)| Some((m.clone(), size(v.as_object()?, "width", "height")?))).collect();
        Some(StillCaps {
            formats, size: size(raw, "width", "height"), max_res: size(raw, "maxResWidth", "maxResHeight"),
            max_res_raw: flag("maxResRaw"), extensions: words("extensions"), extension_sizes,
            exposure_max_ns: number("exposureMaxNs"), bracketing: flag("bracketing"), focus_bracketing: flag("focusBracketing"),
            noise_reduction_hq: flag("noiseReductionHq"), sharpening_hq: flag("sharpeningHq"), hot_pixel_hq: flag("hotPixelHq"),
            distortion: flag("distortionCorrection"), burst_max: number("burstMax").max(1),
        })
    }
}

fn truthy(v: &Value) -> bool {
    crate::session_client::truthy(Some(v))
}

/// How a photo is getting on: the phone's state ("waiting", "capturing", "done", "failed"), and what's saved.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StillProgress {
    pub state: String,
    pub shots: i64,
    pub taken: i64,
    pub countdown_ms: i64,
    /// The phone paused the video to take it.
    pub paused: bool,
    pub saved: Vec<String>,
}

/// How a photo ended. files: the paths it saved, in the order taken (some even when it failed later).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StillResult {
    pub ok: bool,
    pub files: Vec<String>,
    pub code: String,
    /// In the desktop's language.
    pub error: String,
}

impl StillResult {
    pub fn failed(code: &str, english: &str, files: &[String]) -> StillResult {
        StillResult { ok: false, files: files.to_vec(), code: code.into(), error: reason_text(code, english) }
    }
}

fn split_ext(name: &str) -> (&str, Option<&str>) {
    match name.rsplit_once('.') {
        Some((stem, ext)) => (stem, Some(ext)),
        None => (name, None),
    }
}

/// folder/name, or name with (2), (3) and so on before its extension when that's taken.
pub fn free_path(folder: &Path, name: &str) -> PathBuf {
    let (stem, ext) = split_ext(name);
    let mut path = folder.join(name);
    let mut n = 2;
    let part = |p: &Path| p.with_file_name(format!("{}.part", p.file_name().unwrap_or_default().to_string_lossy()));
    while path.exists() || part(&path).exists() {
        path = folder.join(match ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        });
        n += 1;
    }
    path
}

/// A free name for name in folder, taken by creating it empty so another photo saving at the same time can't.
pub fn reserve_path(folder: &Path, name: &str) -> std::io::Result<PathBuf> {
    for _ in 0..1000 {
        let path = free_path(folder, name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other(format!("no free name for {name}")))
}

/// A file name from the phone, kept to plain characters and no folders.
pub fn safe_name(name: &str, index: usize) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    let cleaned: String = base.chars()
        .map(|c| if c.is_ascii_alphanumeric() || "_.()-".contains(c) { c } else { '_' })
        .take(120).collect();
    let cleaned = cleaned.trim_start_matches('.');
    if cleaned.is_empty() { format!("photo_{}.jpg", index + 1) } else { cleaned.to_string() }
}

/// How long the sensor is open in all for a photo with these /v1/still options (0 for the video's exposure).
pub fn exposure_seconds(options: &Value) -> f64 {
    let float = |key: &str| match options.get(key) {
        None | Some(Value::Null) => Some(0.0),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if s.is_empty() => Some(0.0),
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(Value::Bool(b)) => Some(*b as i64 as f64),
        Some(_) => None,
    };
    let (Some(exposure_ns), Some(count), Some(steps)) = (float("exposure_ns"), py_int(options.get("count")), py_int(options.get("focus_steps"))) else {
        return 0.0;
    };
    let bracket = match options.get("bracket_ev") {
        Some(Value::String(s)) => s.split(',').filter(|p| !p.trim().is_empty()).count(),
        _ => 0,
    }.max(1);
    (exposure_ns / 1e9).max(0.0) * count.max(1) as f64 * bracket as f64 * steps.max(1) as f64
}

type Progress = Box<dyn Fn(StillProgress) + Send>;
type Alive = Box<dyn Fn() -> bool + Send>;
/// Runs before each file download: false fails it (tests, to stand in for a disk that won't take it).
pub type BeforeFetch = Arc<dyn Fn() -> bool + Send + Sync>;

/// Why a request failed, as far as the photo cares.
enum Failure {
    Http(u16),
    Other(String),
}

impl Failure {
    fn text(&self) -> String {
        match self {
            Failure::Http(code) => format!("HTTP Error {code}"),
            Failure::Other(e) => e.clone(),
        }
    }
}

/// Takes photos on a stream's phone and saves them, one at a time per phone.
#[derive(Clone, Default)]
pub struct Stills {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Source id: the stop flag of the photo being taken there.
    busy: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// Source id: the newest whole /v1/state from its phone.
    states: Mutex<HashMap<String, Value>>,
    timings: Mutex<Timings>,
    before_fetch: Mutex<Option<BeforeFetch>>,
}

fn key(source_id: Option<&str>) -> String {
    source_id.unwrap_or("").to_string()
}

impl Stills {
    pub fn new() -> Stills {
        Stills::default()
    }

    pub fn set_timings(&self, timings: Timings) {
        *self.inner.timings.lock().unwrap() = timings;
    }

    pub fn set_before_fetch(&self, hook: Option<BeforeFetch>) {
        *self.inner.before_fetch.lock().unwrap() = hook;
    }

    pub fn note_state(&self, source_id: Option<&str>, state: &Value) {
        if state.get("cameras").is_some() {
            self.inner.states.lock().unwrap().insert(key(source_id), state.clone());
        }
    }

    /// The stream moved to another lens: its stored state says so, so caps describes that lens.
    pub fn note_lens(&self, source_id: Option<&str>, cam: &Value) {
        let mut states = self.inner.states.lock().unwrap();
        let Some(state) = states.get_mut(&key(source_id)).and_then(Value::as_object_mut) else { return };
        let (Some(cam), Some(id)) = (cam.as_object(), cam.get("id")) else { return };
        let mut cameras: Vec<Value> = state.get("cameras").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().map(|c| match c {
            Value::Object(mut m) if m.get("id") == Some(id) => {
                m.extend(cam.clone());
                m.insert("current".into(), Value::Bool(true));
                Value::Object(m)
            }
            Value::Object(mut m) => {
                m.insert("current".into(), Value::Bool(false));
                Value::Object(m)
            }
            other => other,
        }).collect();
        if !cameras.iter().any(|c| c.get("current").is_some_and(truthy)) {
            let mut added = cam.clone();
            added.insert("current".into(), Value::Bool(true));
            cameras.push(Value::Object(added));
        }
        state.insert("cameras".into(), Value::Array(cameras));
    }

    pub fn caps(&self, source_id: Option<&str>) -> Option<StillCaps> {
        StillCaps::from_state(self.inner.states.lock().unwrap().get(&key(source_id)))
    }

    pub fn busy(&self, source_id: Option<&str>) -> bool {
        self.inner.busy.lock().unwrap().contains_key(&key(source_id))
    }

    /// Starts a photo against the stream's base ("https://host:port/v1"); false while one is being taken there.
    #[allow(clippy::too_many_arguments)]
    pub fn take(&self, source_id: Option<&str>, base: &str, auth: PhoneAuth, options: Map<String, Value>, folder: PathBuf,
                on_done: Box<dyn FnOnce(StillResult) + Send>, on_progress: Option<Progress>, alive: Option<Alive>) -> bool {
        let stop = Arc::new(AtomicBool::new(false));
        let k = key(source_id);
        {
            let mut busy = self.inner.busy.lock().unwrap();
            if busy.contains_key(&k) {
                return false;
            }
            busy.insert(k.clone(), stop.clone());
        }
        let inner = self.inner.clone();
        let base = base.to_string();
        std::thread::Builder::new().name("still".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Runner { inner: &inner, base: &base, auth: &auth, stop: &stop }.run(options, &folder, on_progress, alive)
            })).unwrap_or_else(|_| StillResult::failed("failed", "", &[]));
            inner.busy.lock().unwrap().remove(&k);
            on_done(result);
        }).is_ok()
    }

    /// Stops every photo; each tells its phone to forget it on the way out.
    pub fn shutdown(&self) {
        for stop in self.inner.busy.lock().unwrap().values() {
            stop.store(true, Ordering::SeqCst);
        }
    }
}

/// One photo, on its own thread.
struct Runner<'a> {
    inner: &'a Inner,
    base: &'a str,
    auth: &'a PhoneAuth,
    stop: &'a AtomicBool,
}

impl Runner<'_> {
    fn timings(&self) -> Timings {
        *self.inner.timings.lock().unwrap()
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// stop.wait(t): sleeps up to t, waking early once stopped.
    fn wait(&self, t: Duration) {
        let until = Instant::now() + t;
        while !self.stopped() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20).min(until - Instant::now()));
        }
    }

    fn request(&self, path: &str, body: Option<&Value>) -> Result<Map<String, Value>, Failure> {
        let url = format!("{}{path}", self.base);
        let text = body.map(python_json);
        let timeout = self.timings().request;
        let req = match &text {
            Some(text) => Request::post(&url, text.as_bytes(), timeout),
            None => Request::get(&url, timeout),
        };
        let mut resp = self.auth.open_any(req).map_err(|e| Failure::Other(e.to_string()))?;
        let raw = resp.read_capped(MAX_REPLY).map_err(|e| Failure::Other(e.to_string()))?;
        if !(200..300).contains(&resp.status) {
            return Err(Failure::Http(resp.status));
        }
        match serde_json::from_slice::<Value>(&raw) {
            Ok(Value::Object(m)) => Ok(m),
            Ok(_) => Err(Failure::Other("not a JSON object".into())),
            Err(e) => Err(Failure::Other(e.to_string())),
        }
    }

    fn forget(&self, sid: &str) {
        if let Err(e) = self.request("/still", Some(&json!({"action": "forget", "id": sid}))) {
            log::debug!("The phone didn't forget photo {sid}: {}", e.text());
        }
    }

    fn run(&self, options: Map<String, Value>, folder: &Path, on_progress: Option<Progress>, alive: Option<Alive>) -> StillResult {
        let t = self.timings();
        let exposure = Duration::from_secs_f64(exposure_seconds(&Value::Object(options.clone())));
        let mut body = options;
        body.insert("action".into(), json!("take"));
        let started = match self.request("/still", Some(&Value::Object(body))) {
            Ok(started) => started,
            // An older phone has no /v1/still
            Err(Failure::Http(404)) => return StillResult::failed("no_photos", "", &[]),
            Err(e @ Failure::Http(..)) => return StillResult::failed("failed", &e.text(), &[]),
            Err(e) => return StillResult::failed("lost", &e.text(), &[]),
        };
        let sid = match (started.get("ok").is_some_and(truthy), started.get("id").and_then(Value::as_str)) {
            (true, Some(id)) => id.to_string(),
            _ => {
                let code = started.get("code").filter(|c| truthy(c)).map(py_str).unwrap_or_else(|| "failed".into());
                let english = started.get("error").filter(|e| truthy(e)).map(py_str).unwrap_or_default();
                return StillResult::failed(&code, &english, &[]);
            }
        };
        let mut saved: Vec<String> = Vec::new();
        let mut fetched = 0usize;
        let (begun, mut heard, mut moved) = (Instant::now(), Instant::now(), Instant::now());
        let mut last: Option<Value> = None;
        let (stall_after, longest) = (t.stall + exposure, t.longest + exposure);
        if let Err(e) = std::fs::create_dir_all(folder) {
            log::warn!("Can't make the photo folder {}: {e}", folder.display());
            return StillResult::failed("save", "", &[]);
        }
        let alive_now = || alive.as_ref().is_none_or(|a| a());
        while !self.stopped() {
            if !alive_now() {
                // Its stream stopped or restarted: the phone drops the photo with it, if it can still say so at all
                return StillResult::failed("interrupted", "", &saved);
            }
            let status = match self.request(&format!("/still?id={sid}"), None) {
                Ok(status) => {
                    heard = Instant::now();
                    status
                }
                Err(e) => {
                    if !alive_now() {
                        continue;
                    }
                    if matches!(e, Failure::Http(404)) {
                        return StillResult::failed("dropped", "", &saved); // the phone let go of it
                    }
                    if heard.elapsed() > t.lost_after {
                        self.forget(&sid); // in case it's only this computer's link that's gone
                        return StillResult::failed("lost", &e.text(), &saved);
                    }
                    self.wait(t.poll);
                    continue;
                }
            };
            let files: Vec<Map<String, Value>> = status.get("files").and_then(Value::as_array).into_iter().flatten()
                .filter_map(|f| f.as_object().cloned()).collect();
            let state = status.get("state").filter(|s| truthy(s)).map(py_str).unwrap_or_default();
            let countdown = if state == "waiting" { status.get("countdown_ms").cloned().unwrap_or(Value::Null) } else { Value::Null };
            let now = json!([state, status.get("taken"), files.len(), countdown]);
            if last.as_ref() != Some(&now) {
                last = Some(now);
                moved = Instant::now();
            }
            let report = |saved: &[String]| {
                if let Some(progress) = &on_progress {
                    progress(StillProgress {
                        state: state.clone(), shots: py_int(status.get("shots")).filter(|n| *n != 0).unwrap_or(1),
                        taken: py_int(status.get("taken")).unwrap_or(0), countdown_ms: py_int(status.get("countdown_ms")).unwrap_or(0),
                        paused: status.get("paused").is_some_and(truthy), saved: saved.to_vec(),
                    });
                }
            };
            report(&saved); // before the downloads, which can take a while for a batch of RAW files
            while fetched < files.len() && !self.stopped() {
                let Some(path) = self.fetch(&sid, fetched, &files[fetched], folder) else {
                    self.forget(&sid);
                    return StillResult::failed("save", "", &saved);
                };
                saved.push(path.to_string_lossy().into_owned());
                fetched += 1;
                // A download that took a while is the phone answering, not stuck
                heard = Instant::now();
                moved = Instant::now();
                report(&saved);
            }
            if (state == "done" || state == "failed") && fetched >= files.len() {
                self.forget(&sid);
                if state == "failed" {
                    let code = status.get("code").filter(|c| truthy(c)).map(py_str).unwrap_or_else(|| "failed".into());
                    let english = status.get("error").filter(|e| truthy(e)).map(py_str).unwrap_or_default();
                    return StillResult::failed(&code, &english, &saved);
                }
                return StillResult { ok: true, files: saved, code: String::new(), error: String::new() };
            }
            if begun.elapsed() > longest || moved.elapsed() > stall_after {
                log::warn!("Photo {sid} got nowhere on the phone ({state}), giving up on it");
                self.forget(&sid);
                return StillResult::failed("timeout", "", &saved);
            }
            self.wait(t.poll);
        }
        self.forget(&sid); // stopped: Telescope is quitting
        StillResult::failed("failed", "", &saved)
    }

    /// File n of the photo into folder under the phone's name (made free), or None when it couldn't be saved.
    fn fetch(&self, sid: &str, n: usize, info: &Map<String, Value>, folder: &Path) -> Option<PathBuf> {
        let hook = self.inner.before_fetch.lock().unwrap().clone();
        if hook.is_some_and(|h| !h()) {
            return None;
        }
        let name = safe_name(&info.get("name").filter(|v| truthy(v)).map(py_str).unwrap_or_default(), n);
        let path = match reserve_path(folder, &name) {
            Ok(path) => path,
            Err(e) => {
                log::warn!("Couldn't save photo {name}: {e}");
                return None;
            }
        };
        let part = path.with_file_name(format!("{}.part", path.file_name().unwrap_or_default().to_string_lossy()));
        let got = (|| -> Result<(), String> {
            let url = format!("{}/still/file?id={sid}&n={n}", self.base);
            let mut resp = self.auth.open(Request::get(&url, self.timings().file)).map_err(|e: Error| e.to_string())?;
            let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; CHUNK];
            loop {
                let k = resp.read(&mut buf).map_err(|e| e.to_string())?;
                if k == 0 {
                    break;
                }
                out.write_all(&buf[..k]).map_err(|e| e.to_string())?;
            }
            drop(out);
            if let Some(size) = info.get("size").filter(|v| v.is_i64() || v.is_u64()).and_then(Value::as_u64) {
                let have = std::fs::metadata(&part).map_err(|e| e.to_string())?.len();
                if have != size {
                    return Err(format!("got {have} of {size} bytes"));
                }
            }
            std::fs::rename(&part, &path).map_err(|e| e.to_string())
        })();
        match got {
            Ok(()) => Some(path),
            Err(e) => {
                log::warn!("Couldn't save photo {name}: {e}");
                let _ = std::fs::remove_file(&part);
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }
}

/// str(value) as Python would print a JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl PhoneStills for Stills {
    fn note_state(&self, source_id: Option<&str>, state: &Value) {
        Stills::note_state(self, source_id, state);
    }

    fn note_lens(&self, source_id: Option<&str>, camera: &Value) {
        Stills::note_lens(self, source_id, camera);
    }

    fn caps(&self, source_id: Option<&str>) -> Option<Value> {
        Stills::caps(self, source_id).and_then(|c| serde_json::to_value(c).ok())
    }

    fn busy(&self, source_id: Option<&str>) -> bool {
        Stills::busy(self, source_id)
    }

    fn take(&self, source_id: Option<&str>, stream_base: &str, phone: &PairedPhone, options: &Value, folder: &str,
            on_progress: Box<dyn Fn(Value) + Send>, on_done: Box<dyn FnOnce(Value) + Send>, alive: Box<dyn Fn() -> bool + Send>) -> bool {
        let options = options.as_object().cloned().unwrap_or_default();
        Stills::take(self, source_id, stream_base, auth_of(phone), options, PathBuf::from(folder),
                     Box::new(move |r| on_done(serde_json::to_value(r).unwrap_or(Value::Null))),
                     Some(Box::new(move |p| on_progress(serde_json::to_value(p).unwrap_or(Value::Null)))), Some(alive))
    }

    fn shutdown(&self) {
        Stills::shutdown(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::https::testing::{serve, Seen, Server};
    use std::sync::mpsc;

    fn full() -> Value {
        json!({"formats": ["jpeg", "heic", "ultrahdr", "raw"], "width": 4080, "height": 3072, "maxResWidth": 8160,
               "maxResHeight": 6144, "maxResRaw": true, "extensions": ["night", "hdr"], "exposureMaxNs": 30_000_000_000i64,
               "bracketing": true, "focusBracketing": true, "noiseReductionHq": true, "sharpeningHq": true,
               "hotPixelHq": true, "distortionCorrection": true, "burstMax": 60})
    }

    fn state_with(stills: Value, version: Option<i64>, current: bool) -> Value {
        let mut state = json!({"cameras": [{"id": "0", "current": !current}, {"id": "2", "current": current, "stills": stills}]});
        if let Some(v) = version {
            state["stills"] = json!(v);
        }
        state
    }

    fn caps(stills: Value) -> Option<StillCaps> {
        StillCaps::from_state(Some(&state_with(stills, Some(1), true)))
    }

    #[test]
    fn the_current_lens_says_what_it_takes() {
        let c = caps(full()).unwrap();
        assert_eq!(c.formats, ["jpeg", "heic", "ultrahdr", "raw"]);
        assert_eq!((c.size, c.max_res, c.max_res_raw), (Some((4080, 3072)), Some((8160, 6144)), true));
        assert_eq!((c.extensions.clone(), c.exposure_max_ns), (vec!["night".to_string(), "hdr".into()], 30_000_000_000));
        assert!(c.bracketing && c.focus_bracketing && c.distortion && c.burst_max == 60);
    }

    #[test]
    fn a_makers_mode_says_its_own_size_and_an_older_phone_falls_back_to_the_plain_one() {
        let mut stills = full();
        stills["extensionSizes"] = json!({"night": {"width": 3840, "height": 2160}, "hdr": {"width": "x"}, "bokeh": 5});
        let c = caps(stills).unwrap();
        assert_eq!((c.size_for("night"), c.size_for("hdr")), (Some((3840, 2160)), Some((4080, 3072))));
        assert_eq!((c.size_for("normal"), c.size_for("max_res")), (Some((4080, 3072)), Some((8160, 6144))));
        let older = caps(full()).unwrap();
        assert!(older.extension_sizes.is_empty() && older.size_for("night") == Some((4080, 3072)));
    }

    #[test]
    fn an_older_phone_or_a_lens_without_photos_takes_none() {
        assert!(StillCaps::from_state(Some(&state_with(full(), None, true))).is_none());
        assert!(StillCaps::from_state(Some(&state_with(full(), Some(0), true))).is_none());
        assert!(StillCaps::from_state(Some(&state_with(Value::Null, Some(1), true))).is_none());
        assert!(StillCaps::from_state(Some(&state_with(full(), Some(1), false))).is_none());
        assert!(StillCaps::from_state(Some(&json!({}))).is_none() && StillCaps::from_state(None).is_none());
    }

    #[test]
    fn odd_values_fall_back_to_the_least_it_can_do() {
        let c = caps(json!({"formats": "raw", "width": "x", "burstMax": -3, "maxResRaw": "yes"})).unwrap();
        assert!(c.formats == ["jpeg"] && c.size.is_none() && c.burst_max == 1 && !c.max_res_raw);
        assert!(c.max_res.is_none() && c.extensions.is_empty() && c.exposure_max_ns == 0);
    }

    #[test]
    fn a_name_already_taken_gets_a_number() {
        let dir = telescope_core::testing::scratch_dir("stills-names");
        std::fs::write(dir.join("IMG_1.jpg"), b"x").unwrap();
        assert_eq!(free_path(&dir, "IMG_1.jpg"), dir.join("IMG_1 (2).jpg"));
        std::fs::write(dir.join("IMG_1 (2).jpg.part"), b"x").unwrap();
        assert_eq!(free_path(&dir, "IMG_1.jpg"), dir.join("IMG_1 (3).jpg"));
        assert_eq!(free_path(&dir, "notes"), dir.join("notes"));
    }

    #[test]
    fn names_from_the_phone_stay_in_the_folder() {
        assert_eq!(safe_name("../../etc/passwd", 0), "passwd");
        assert_eq!(safe_name("IMG 1.jpg", 0), "IMG_1.jpg");
        assert_eq!(safe_name("...", 2), "photo_3.jpg");
        assert_eq!(safe_name("", 0), "photo_1.jpg");
    }

    #[test]
    fn reasons_are_worded_here_and_unknown_ones_keep_the_phones_english() {
        assert_eq!(reason_text("busy", "whatever"), "The phone is already taking a photo.");
        assert_eq!(reason_text("new_code", "Something new"), "Something new");
        assert_eq!(reason_text("new_code", ""), "The phone couldn't take the photo.");
    }

    #[test]
    fn reserving_a_name_takes_it_for_this_photo() {
        let dir = telescope_core::testing::scratch_dir("stills-reserve");
        let (first, second) = (reserve_path(&dir, "IMG_1.jpg").unwrap(), reserve_path(&dir, "IMG_1.jpg").unwrap());
        assert_eq!((first.file_name().unwrap(), second.file_name().unwrap()), ("IMG_1.jpg".as_ref(), "IMG_1 (2).jpg".as_ref()));
        assert!(first.exists() && second.exists());
    }

    #[test]
    fn a_long_exposure_gets_its_exposure_on_top_of_the_stall_time() {
        assert_eq!(exposure_seconds(&json!({"format": "jpeg"})), 0.0);
        assert_eq!(exposure_seconds(&json!({"exposure_ns": 30_000_000_000i64})), 30.0);
        assert_eq!(exposure_seconds(&json!({"exposure_ns": 2_000_000_000i64, "count": 2, "bracket_ev": "-1,0,1"})), 12.0);
        assert_eq!(exposure_seconds(&json!({"exposure_ns": "x"})), 0.0);
    }

    /// /v1/still as the phone serves it: a photo that takes a couple of polls and leaves the files given.
    #[derive(Clone)]
    struct Phone {
        files: Vec<(String, Vec<u8>)>,
        polls: i64,
        start: Option<Value>,
        fail: Option<String>,
        missing: bool,
        unreachable: bool,
        dropped: bool,
        early: bool,
        posts: Vec<Value>,
        forgotten: Vec<String>,
    }

    impl Default for Phone {
        fn default() -> Self {
            Phone { files: vec![("IMG_1.jpg".into(), b"jpeg".repeat(100)), ("IMG_2.dng".into(), b"dng!".repeat(5000))], polls: 2,
                    start: None, fail: None, missing: false, unreachable: false, dropped: false, early: false,
                    posts: Vec::new(), forgotten: Vec::new() }
        }
    }

    fn answer(phone: &Mutex<Phone>, req: &Seen) -> (u16, String, Vec<u8>) {
        let json = |code: u16, v: Value| (code, "application/json".to_string(), v.to_string().into_bytes());
        let mut p = phone.lock().unwrap();
        assert_eq!(req.auth.as_deref(), Some("Bearer token"));
        if req.method == "POST" {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            if p.missing {
                return json(404, json!({"error": "Not Found"}));
            }
            if body["action"] == "forget" {
                p.forgotten.push(body["id"].as_str().unwrap().into());
                return json(200, json!({"ok": true}));
            }
            p.posts.push(body);
            return json(200, p.start.clone().unwrap_or(json!({"ok": true, "id": "abc"})));
        }
        if req.path.starts_with("/v1/still/file") {
            let n: usize = req.path.rsplit("n=").next().unwrap().parse().unwrap();
            return (200, "image/jpeg".into(), p.files[n].1.clone());
        }
        if p.unreachable {
            return json(503, json!({"error": "gone"}));
        }
        if p.dropped && p.polls <= 1 {
            return json(404, json!({"error": "Not Found"}));
        }
        p.polls -= 1;
        let done = p.polls <= 0;
        let all: Vec<Value> = p.files.iter().map(|(k, v)| json!({"name": k, "size": v.len(), "type": "image/jpeg"})).collect();
        let files: Vec<Value> = if done { all } else if p.early { all.into_iter().take(1).collect() } else { Vec::new() };
        let state = if done { if p.fail.is_some() { "failed" } else { "done" } } else { "capturing" };
        let mut body = json!({"id": "abc", "state": state, "shots": 2, "taken": if done { 2 } else { 1 }, "files": files});
        if let (Some(code), true) = (&p.fail, done) {
            body["code"] = json!(code);
            body["error"] = json!("English");
        }
        json(200, body)
    }

    fn phone(p: Phone) -> (Server, Arc<Mutex<Phone>>) {
        let state = Arc::new(Mutex::new(p));
        let s = state.clone();
        (serve(None, Arc::new(move |req| answer(&s, req))), state)
    }

    fn quick() -> Timings {
        Timings { poll: Duration::from_millis(20), ..Timings::default() }
    }

    fn take_with(stills: &Stills, server: &Server, folder: &Path, options: Value, alive: Option<Alive>) -> (StillResult, Vec<StillProgress>) {
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(Mutex::new(Vec::new()));
        let seen = progress.clone();
        assert!(stills.take(Some("phone-1"), &format!("{}/v1", server.base(false)), PhoneAuth::new("token", ""),
                            options.as_object().unwrap().clone(), folder.to_path_buf(), Box::new(move |r| tx.send(r).unwrap()),
                            Some(Box::new(move |p| seen.lock().unwrap().push(p))), alive));
        let result = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let progress = progress.lock().unwrap().clone();
        (result, progress)
    }

    fn take(p: Phone, folder: &Path) -> (StillResult, Vec<StillProgress>, Phone) {
        let stills = Stills::new();
        stills.set_timings(quick());
        let (server, state) = phone(p);
        let (r, progress) = take_with(&stills, &server, folder, json!({"format": "jpeg", "raw": 1}), None);
        let p = state.lock().unwrap().clone();
        (r, progress, p)
    }

    fn names(files: &[String]) -> Vec<String> {
        files.iter().map(|f| Path::new(f).file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_photo_is_saved_file_by_file_and_the_phone_forgets_it() {
        let dir = telescope_core::testing::scratch_dir("stills-take").join("Photos");
        let (r, progress, p) = take(Phone::default(), &dir);
        assert!(r.ok, "{r:?}");
        assert_eq!(names(&r.files), ["IMG_1.jpg", "IMG_2.dng"]);
        assert_eq!(std::fs::read(dir.join("IMG_2.dng")).unwrap(), b"dng!".repeat(5000));
        assert_eq!(p.posts, vec![json!({"format": "jpeg", "raw": 1, "action": "take"})]);
        assert_eq!(p.forgotten, ["abc"]);
        assert_eq!((progress[0].state.as_str(), progress.last().unwrap().state.as_str()), ("capturing", "done"));
        assert_eq!(progress.last().unwrap().saved, r.files);
        assert!(!std::fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".part")));
    }

    #[test]
    fn the_take_body_goes_out_as_python_writes_it() {
        let dir = telescope_core::testing::scratch_dir("stills-body");
        let stills = Stills::new();
        stills.set_timings(quick());
        let (server, _) = phone(Phone::default());
        take_with(&stills, &server, &dir, json!({"format": "jpeg", "raw": 1}), None);
        let first = server.seen.lock().unwrap()[0].clone();
        assert_eq!(String::from_utf8(first.body).unwrap(), r#"{"format": "jpeg", "raw": 1, "action": "take"}"#);
    }

    #[test]
    fn files_with_the_same_name_dont_overwrite_earlier_photos() {
        let dir = telescope_core::testing::scratch_dir("stills-same");
        std::fs::write(dir.join("IMG_1.jpg"), b"older").unwrap();
        let (r, _, _) = take(Phone::default(), &dir);
        assert!(r.files[0].ends_with("IMG_1 (2).jpg") && std::fs::read(dir.join("IMG_1.jpg")).unwrap() == b"older");
    }

    #[test]
    fn a_refusal_is_worded_from_its_code() {
        let p = Phone { start: Some(json!({"ok": false, "code": "unsupported", "error": "This lens can't save HEIC"})), ..Phone::default() };
        let (r, _, _) = take(p, &telescope_core::testing::scratch_dir("stills-refused"));
        assert!(!r.ok && r.code == "unsupported" && r.error.contains("can't take that photo"));
    }

    #[test]
    fn a_photo_that_failed_on_the_phone_keeps_what_was_saved() {
        let p = Phone { fail: Some("interrupted".into()), files: vec![("IMG_1.jpg".into(), b"x".to_vec())], ..Phone::default() };
        let (r, _, p) = take(p, &telescope_core::testing::scratch_dir("stills-failed"));
        assert!(!r.ok && r.code == "interrupted" && r.files.len() == 1);
        assert_eq!(p.forgotten, ["abc"]);
    }

    #[test]
    fn an_older_phone_without_photos() {
        let (r, _, _) = take(Phone { missing: true, ..Phone::default() }, &telescope_core::testing::scratch_dir("stills-old"));
        assert_eq!(r.code, "no_photos");
    }

    #[test]
    fn one_photo_at_a_time_per_phone() {
        let dir = telescope_core::testing::scratch_dir("stills-one");
        let stills = Stills::new();
        stills.set_timings(quick());
        let (server, _) = phone(Phone { polls: 4, ..Phone::default() });
        let base = format!("{}/v1", server.base(false));
        let (tx, rx) = mpsc::channel();
        assert!(stills.take(Some("phone-1"), &base, PhoneAuth::new("token", ""), Map::new(), dir.clone(), Box::new(move |_| tx.send(()).unwrap()), None, None));
        assert!(stills.busy(Some("phone-1")) && !stills.busy(Some("phone-2")));
        assert!(!stills.take(Some("phone-1"), &base, PhoneAuth::new("token", ""), Map::new(), dir, Box::new(|_| {}), None, None));
        rx.recv_timeout(Duration::from_secs(20)).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(!stills.busy(Some("phone-1")));
    }

    #[test]
    fn a_folder_that_cant_be_made_fails_cleanly() {
        let dir = telescope_core::testing::scratch_dir("stills-blocked");
        std::fs::write(dir.join("file"), "not a folder").unwrap();
        let (r, _, _) = take(Phone::default(), &dir.join("file").join("Photos"));
        assert!(!r.ok && r.code == "save");
    }

    #[test]
    fn a_file_that_cant_be_saved_makes_the_phone_forget_the_photo() {
        let stills = Stills::new();
        stills.set_timings(quick());
        stills.set_before_fetch(Some(Arc::new(|| false)));
        let (server, state) = phone(Phone::default());
        let (r, _) = take_with(&stills, &server, &telescope_core::testing::scratch_dir("stills-nosave"), json!({"format": "jpeg"}), None);
        assert_eq!(r.code, "save");
        assert_eq!(state.lock().unwrap().forgotten, ["abc"]);
    }

    #[test]
    fn progress_is_reported_before_the_downloads() {
        let stills = Stills::new();
        stills.set_timings(quick());
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let o = order.clone();
        stills.set_before_fetch(Some(Arc::new(move || {
            o.lock().unwrap().push("fetch".into());
            true
        })));
        let (server, _) = phone(Phone { polls: 1, ..Phone::default() });
        let (tx, rx) = mpsc::channel();
        let o = order.clone();
        assert!(stills.take(Some("phone-1"), &format!("{}/v1", server.base(false)), PhoneAuth::new("token", ""), Map::new(),
                            telescope_core::testing::scratch_dir("stills-order"), Box::new(move |_| tx.send(()).unwrap()),
                            Some(Box::new(move |p| o.lock().unwrap().push(format!("progress {}", p.saved.len())))), None));
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(*order.lock().unwrap(), ["progress 0", "fetch", "progress 1", "fetch", "progress 2"]);
    }

    fn timed(p: Phone, timings: Timings, options: Value, alive: Option<Alive>) -> (StillResult, Vec<StillProgress>, Phone) {
        let stills = Stills::new();
        stills.set_timings(timings);
        let (server, state) = phone(p);
        let (r, progress) = take_with(&stills, &server, &telescope_core::testing::scratch_dir("stills-timed"), options, alive);
        let p = state.lock().unwrap().clone();
        (r, progress, p)
    }

    #[test]
    fn a_phone_that_stops_answering_is_told_to_forget_the_photo() {
        let (r, _, p) = timed(Phone { unreachable: true, ..Phone::default() }, Timings { lost_after: Duration::ZERO, ..quick() }, json!({}), None);
        assert_eq!(r.code, "lost");
        assert_eq!(p.forgotten, ["abc"]);
    }

    #[test]
    fn a_photo_the_phone_let_go_of_ends_at_once() {
        let (r, progress, _) = timed(Phone { dropped: true, ..Phone::default() }, Timings { lost_after: Duration::from_secs(60), ..quick() }, json!({}), None);
        assert!(r.code == "dropped" && r.error.contains("gave up") && progress.len() == 1);
    }

    #[test]
    fn a_photo_that_never_ends_is_given_up_on_and_forgotten() {
        let (r, _, p) = timed(Phone { polls: 1000, ..Phone::default() }, Timings { longest: Duration::ZERO, ..quick() }, json!({}), None);
        assert_eq!(r.code, "timeout");
        assert_eq!(p.forgotten, ["abc"]);
    }

    #[test]
    fn a_photo_stopped_on_quit_is_forgotten() {
        let stills = Stills::new();
        stills.set_timings(quick());
        let (server, state) = phone(Phone { polls: 1000, ..Phone::default() });
        let (polled_tx, polled_rx) = mpsc::channel();
        let polled_tx = Mutex::new(polled_tx);
        let (tx, rx) = mpsc::channel();
        assert!(stills.take(Some("phone-1"), &format!("{}/v1", server.base(false)), PhoneAuth::new("token", ""), Map::new(),
                            telescope_core::testing::scratch_dir("stills-quit"), Box::new(move |r| tx.send(r).unwrap()),
                            Some(Box::new(move |_| { let _ = polled_tx.lock().unwrap().send(()); })), None));
        polled_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        stills.shutdown();
        let r = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(r.code, "failed");
        assert_eq!(state.lock().unwrap().forgotten, ["abc"]);
    }

    #[test]
    fn a_photo_that_stops_moving_on_the_phone_is_given_up_on_and_forgotten() {
        let (r, progress, p) = timed(Phone { polls: 1000, ..Phone::default() }, Timings { stall: Duration::from_millis(500), ..quick() }, json!({}), None);
        assert!(r.code == "timeout" && r.error.contains("too long"));
        assert_eq!(p.forgotten, ["abc"]);
        assert_eq!(progress.last().unwrap().state, "capturing");
    }

    #[test]
    fn a_slow_download_doesnt_count_as_the_photo_standing_still() {
        let stills = Stills::new();
        stills.set_timings(Timings { stall: Duration::from_millis(500), ..quick() });
        // A big RAW over Wi-Fi
        stills.set_before_fetch(Some(Arc::new(|| {
            std::thread::sleep(Duration::from_millis(800));
            true
        })));
        let (server, _) = phone(Phone { polls: 2, early: true, ..Phone::default() });
        let (r, _) = take_with(&stills, &server, &telescope_core::testing::scratch_dir("stills-slow"), json!({"format": "jpeg"}), None);
        assert!(r.ok && r.files.len() == 2, "{r:?}");
    }

    #[test]
    fn a_long_exposure_batch_gets_its_exposures_on_top_of_the_longest_wait() {
        let (r, _, _) = timed(Phone { polls: 2, ..Phone::default() }, Timings { longest: Duration::ZERO, ..quick() },
                              json!({"format": "jpeg", "exposure_ns": 5_000_000_000i64}), None);
        assert!(r.ok && r.files.len() == 2);
    }

    #[test]
    fn a_photo_whose_stream_restarts_ends_as_interrupted_at_once() {
        let stills = Stills::new();
        stills.set_timings(Timings { lost_after: Duration::from_secs(60), ..quick() });
        let (server, state) = phone(Phone { polls: 1000, ..Phone::default() });
        let streaming = Arc::new(AtomicBool::new(true));
        let on = streaming.clone();
        let (polled_tx, polled_rx) = mpsc::channel();
        let polled_tx = Mutex::new(polled_tx);
        let (tx, rx) = mpsc::channel();
        assert!(stills.take(Some("phone-1"), &format!("{}/v1", server.base(false)), PhoneAuth::new("token", ""),
                            json!({"format": "heic"}).as_object().unwrap().clone(), telescope_core::testing::scratch_dir("stills-restart"),
                            Box::new(move |r| tx.send(r).unwrap()), Some(Box::new(move |_| { let _ = polled_tx.lock().unwrap().send(()); })),
                            Some(Box::new(move || on.load(Ordering::SeqCst)))));
        polled_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        streaming.store(false, Ordering::SeqCst);
        // The old stream's server went with it
        state.lock().unwrap().unreachable = true;
        let r = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(r.code == "interrupted" && r.error.contains("interrupted"));
    }

    #[test]
    fn a_restart_while_the_phone_cant_be_reached_is_still_interrupted_not_lost() {
        let (r, _, _) = timed(Phone { unreachable: true, ..Phone::default() }, Timings { lost_after: Duration::ZERO, ..quick() },
                              json!({}), Some(Box::new(|| false)));
        assert_eq!(r.code, "interrupted");
    }

    #[test]
    fn a_lens_switch_moves_what_caps_describes() {
        let stills = Stills::new();
        stills.note_state(Some("p"), &state_with(full(), Some(1), false));
        assert!(stills.caps(Some("p")).is_none());
        stills.note_lens(Some("p"), &json!({"id": "2"}));
        assert_eq!(stills.caps(Some("p")).unwrap().burst_max, 60);
        stills.note_state(Some("p"), &json!({"stills": 1}));
        assert!(stills.caps(Some("p")).is_some());
    }
}
