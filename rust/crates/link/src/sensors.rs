//! The phone's motion and light sensors for plugins (HostSensors), from its /v1/sensors (sensors.py).
//!
//! Plugins say which kinds they want, each under its own owner name. The shown stream's phone streams what anyone
//! wants of what it has, and stops as soon as nobody wants anything, so the phone's sensors are only on while used.

use crate::https::{Aborter, Error, PhoneAuth, Request};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use telescope_core::link::PhoneSensors;
use telescope_core::source::PairedPhone;

/// What a plugin can ask for. orientation is the shown picture's roll and pitch, worked out on the phone; the rest are
/// Android's own readings in the phone's axes.
pub const KINDS: [&str; 7] = ["orientation", "accelerometer", "gyroscope", "magnetometer", "gravity", "rotation", "light"];
const VECTORS: [&str; 5] = ["accelerometer", "gyroscope", "magnetometer", "gravity", "rotation"];
/// Trying to reach the sensors for this long before saying it can't.
const FAIL_AFTER_S: f64 = 6.0;
/// The phone sends a line at least every second.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the phone waits for a request before it stops, when its hello doesn't say.
const ALIVE_DEFAULT_MS: u64 = 15_000;
const MAX_LINE: usize = 64 * 1024;

fn now() -> f64 {
    telescope_core::time::monotonic_ns() as f64 / 1e9
}

/// The newest of each kind streamed (None for one that isn't, or hasn't come in yet).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SensorReading {
    /// Monotonic seconds when the last of these came in.
    pub at: f64,
    /// Degrees the horizon is turned clockwise in the picture as the phone sends it, -180 to 180; None while the camera
    /// points nearly straight up or down.
    pub roll: Option<f64>,
    /// Degrees the camera looks above the horizon.
    pub pitch: Option<f64>,
    /// The phone's id of the camera roll and pitch are for.
    pub camera: String,
    /// When roll and pitch last came in (at also moves for other kinds).
    pub orientation_at: f64,
    pub accelerometer: Option<Vec<Option<f64>>>,
    pub gyroscope: Option<Vec<Option<f64>>>,
    pub magnetometer: Option<Vec<Option<f64>>>,
    pub gravity: Option<Vec<Option<f64>>>,
    /// (x, y, z, w) quaternion of the game rotation vector.
    pub rotation: Option<Vec<Option<f64>>>,
    /// Lux, at most twice a second.
    pub light: Option<f64>,
}

impl SensorReading {
    fn vector_mut(&mut self, kind: &str) -> &mut Option<Vec<Option<f64>>> {
        match kind {
            "accelerometer" => &mut self.accelerometer,
            "gyroscope" => &mut self.gyroscope,
            "magnetometer" => &mut self.magnetometer,
            "gravity" => &mut self.gravity,
            _ => &mut self.rotation,
        }
    }

    pub fn to_value(&self) -> Value {
        json!({"at": self.at, "roll": self.roll, "pitch": self.pitch, "camera": self.camera, "orientation_at": self.orientation_at,
               "accelerometer": self.accelerometer, "gyroscope": self.gyroscope, "magnetometer": self.magnetometer,
               "gravity": self.gravity, "rotation": self.rotation, "light": self.light})
    }
}

fn number(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

/// reading with what one line of /v1/sensors brought: only the kinds that changed are in it.
pub fn apply_line(reading: &SensorReading, line: &Value, at: f64) -> SensorReading {
    let mut out = reading.clone();
    let mut changed = false;
    if let Some(o) = line.get("orientation").and_then(Value::as_object) {
        out.roll = number(o.get("roll"));
        out.pitch = number(o.get("pitch"));
        out.camera = match o.get("camera") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Bool(true)) => "True".into(),
            Some(v) if crate::session_client::truthy(Some(v)) => v.to_string(),
            _ => String::new(),
        };
        out.orientation_at = at;
        changed = true;
    }
    for kind in VECTORS {
        if let Some(values) = line.get(kind).and_then(Value::as_array).filter(|v| !v.is_empty()) {
            *out.vector_mut(kind) = Some(values.iter().map(|v| number(Some(v))).collect());
            changed = true;
        }
    }
    if let Some(light) = line.get("light") {
        out.light = number(Some(light));
        changed = true;
    }
    if !changed {
        return reading.clone();
    }
    out.at = at;
    out
}

/// reading without the kinds that aren't streamed any more.
fn only(reading: &SensorReading, kinds: &[&'static str]) -> SensorReading {
    let mut out = reading.clone();
    for kind in VECTORS {
        if !kinds.contains(&kind) {
            *out.vector_mut(kind) = None;
        }
    }
    if !kinds.contains(&"light") {
        out.light = None;
    }
    if !kinds.contains(&"orientation") {
        out.roll = None;
        out.pitch = None;
        out.camera.clear();
        out.orientation_at = 0.0;
    }
    out
}

/// Where the shown stream's sensors stand.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SensorStatus {
    /// "off" (no stream, or nobody wants any), "starting", "on" or "unavailable".
    pub state: &'static str,
    /// What the phone has, from its /v1/state.
    pub kinds: Vec<&'static str>,
    /// The kinds wanted that it streams now.
    pub streaming: Vec<&'static str>,
    /// With "unavailable": not_phone, old_phone, none, camera or failed.
    pub reason: &'static str,
}

impl SensorStatus {
    fn off() -> SensorStatus {
        SensorStatus { state: "off", ..Default::default() }
    }

    pub fn to_value(&self) -> Value {
        json!({"state": self.state, "kinds": self.kinds, "streaming": self.streaming, "reason": self.reason})
    }
}

/// One line stream from /v1/sensors.
pub trait LineStream: Send {
    /// One line with its "\n"; empty when the stream ended.
    fn readline(&mut self, limit: usize) -> std::io::Result<Vec<u8>>;
    fn aborter(&self) -> Aborter;
}

impl LineStream for crate::https::Response {
    fn readline(&mut self, limit: usize) -> std::io::Result<Vec<u8>> {
        crate::https::Response::readline(self, limit)
    }

    fn aborter(&self) -> Aborter {
        crate::https::Response::aborter(self)
    }
}

/// How the hub reaches a phone's sensors; tests stand in for a phone.
pub trait SensorLink: Send + Sync {
    fn open(&self, url: &str) -> Result<Box<dyn LineStream>, Error>;
    /// GET .../alive, telling the phone this computer is still here.
    fn alive(&self, url: &str);
    /// Two links with the same key reach the phone the same way.
    fn key(&self) -> String;
}

impl SensorLink for PhoneAuth {
    fn open(&self, url: &str) -> Result<Box<dyn LineStream>, Error> {
        Ok(Box::new(PhoneAuth::open(self, Request::get(url, READ_TIMEOUT))?))
    }

    fn alive(&self, url: &str) {
        if let Ok(mut r) = PhoneAuth::open(self, Request::get(url, Duration::from_secs(4))) {
            let _ = r.read_capped(256);
        }
    }

    fn key(&self) -> String {
        format!("{}:{}", self.token, self.pin)
    }
}

struct Target {
    source_id: String,
    base: Option<String>,
    link: Option<Arc<dyn SensorLink>>,
    phone: bool,
    /// From its /v1/state; None until that comes.
    offered: Option<Vec<&'static str>>,
    /// Its /v1/state has no sensors: a phone from before /v1/sensors.
    old: bool,
}

struct ReaderHandle {
    id: u64,
    url: String,
    key: String,
    stopped: Arc<AtomicBool>,
    current: Arc<Mutex<Option<Aborter>>>,
}

impl ReaderHandle {
    /// Any thread, without waiting for the read in progress.
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(a) = self.current.lock().unwrap().as_ref() {
            a.abort();
        }
    }
}

struct Hub {
    wants: BTreeMap<String, BTreeSet<&'static str>>,
    target: Option<Target>,
    reader: Option<ReaderHandle>,
    reading: Option<SensorReading>,
    state: &'static str,
    reason: &'static str,
    streaming: Vec<&'static str>,
}

type OnChange = Arc<dyn Fn(&str) + Send + Sync>;

struct Inner {
    hub: Mutex<Hub>,
    on_change: Mutex<OnChange>,
    next_id: AtomicU64,
    retry: Duration,
}

/// Who wants which sensors, and the shown stream's phone streaming them.
#[derive(Clone)]
pub struct SensorHub {
    inner: Arc<Inner>,
}

impl SensorHub {
    pub fn new(on_change: impl Fn(&str) + Send + Sync + 'static) -> SensorHub {
        SensorHub::with_retry(on_change, Duration::from_secs(2))
    }

    pub fn with_retry(on_change: impl Fn(&str) + Send + Sync + 'static, retry: Duration) -> SensorHub {
        SensorHub { inner: Arc::new(Inner {
            hub: Mutex::new(Hub { wants: BTreeMap::new(), target: None, reader: None, reading: None, state: "off", reason: "", streaming: vec![] }),
            on_change: Mutex::new(Arc::new(on_change)), next_id: AtomicU64::new(1), retry,
        }) }
    }

    pub fn set_on_change(&self, on_change: impl Fn(&str) + Send + Sync + 'static) {
        *self.inner.on_change.lock().unwrap() = Arc::new(on_change);
    }

    pub fn want(&self, owner: &str, kinds: &[&str]) {
        let wanted: BTreeSet<&'static str> = KINDS.iter().copied().filter(|k| kinds.contains(k)).collect();
        {
            let mut hub = self.inner.hub.lock().unwrap();
            if wanted.is_empty() {
                hub.wants.remove(owner);
            } else {
                hub.wants.insert(owner.to_string(), wanted);
            }
        }
        self.sync();
    }

    pub fn wanted(&self) -> BTreeSet<&'static str> {
        self.inner.hub.lock().unwrap().wants.values().flatten().copied().collect()
    }

    /// source_id's sensors; None: the stream followed.
    pub fn status(&self, source_id: Option<&str>) -> SensorStatus {
        let hub = self.inner.hub.lock().unwrap();
        match &hub.target {
            Some(t) if source_id.is_none_or(|s| s == t.source_id) => SensorStatus {
                state: hub.state, kinds: t.offered.clone().unwrap_or_default(), streaming: hub.streaming.clone(),
                reason: if hub.state == "unavailable" { hub.reason } else { "" },
            },
            _ => SensorStatus::off(),
        }
    }

    pub fn reading(&self, source_id: Option<&str>) -> Option<SensorReading> {
        let hub = self.inner.hub.lock().unwrap();
        match &hub.target {
            Some(t) if source_id.is_none_or(|s| s == t.source_id) => hub.reading.clone(),
            _ => None,
        }
    }

    /// The panels show source_id's stream (None: no stream), reached at base (the phone's https://.../v1). state: its
    /// /v1/state when one just came in ({} when it didn't answer).
    pub fn follow(&self, source_id: Option<&str>, base: Option<&str>, link: Option<Arc<dyn SensorLink>>, phone: bool, state: Option<&Value>) {
        let mut changed_source = None;
        {
            let mut hub = self.inner.hub.lock().unwrap();
            match source_id {
                None => changed_source = hub.target.take().map(|t| t.source_id),
                Some(sid) if hub.target.as_ref().is_none_or(|t| t.source_id != sid) => {
                    changed_source = hub.target.as_ref().map(|t| t.source_id.clone());
                    hub.target = Some(Target { source_id: sid.to_string(), base: base.map(str::to_string), link, phone, offered: None, old: false });
                    hub.reading = None;
                }
                Some(_) => {
                    let t = hub.target.as_mut().unwrap();
                    t.base = base.map(str::to_string);
                    t.link = link;
                    t.phone = phone;
                }
            }
            // {} is a /v1/state that didn't answer, not an old phone
            if let (Some(t), Some(state)) = (hub.target.as_mut(), state.filter(|s| s.as_object().is_some_and(|o| !o.is_empty()))) {
                match state.get("sensors").and_then(Value::as_array) {
                    Some(list) => {
                        t.old = false;
                        t.offered = Some(KINDS.iter().copied().filter(|k| list.iter().any(|v| v.as_str() == Some(k))).collect());
                    }
                    None => {
                        t.old = true;
                        t.offered = Some(Vec::new());
                    }
                }
            }
        }
        if let Some(sid) = changed_source {
            self.notify(&sid); // its readings are gone
        }
        self.sync();
    }

    pub fn shutdown(&self) {
        let reader = self.inner.hub.lock().unwrap().reader.take();
        if let Some(r) = reader {
            r.stop();
        }
    }

    pub fn diagnostics(&self) -> Value {
        let hub = self.inner.hub.lock().unwrap();
        let wanted: BTreeSet<&str> = hub.wants.values().flatten().copied().collect();
        if wanted.is_empty() {
            return json!({});
        }
        let mut text = hub.state.to_string();
        if hub.state == "unavailable" {
            text += &format!(" ({})", hub.reason);
        }
        let mut sorted: Vec<&str> = wanted.into_iter().collect();
        sorted.sort();
        json!({"Phone sensors": format!("{text}, wanted: {}", sorted.join(", "))})
    }

    /// Starts, restarts or stops the reader for what's wanted now, and works out the status.
    fn sync(&self) {
        let mut stop = None;
        let (before, after, source_id) = {
            let mut guard = self.inner.hub.lock().unwrap();
            let hub = &mut *guard;
            let before = (hub.state, hub.reason, hub.streaming.clone());
            let wanted: BTreeSet<&'static str> = hub.wants.values().flatten().copied().collect();
            let mut kinds: Vec<&'static str> = Vec::new();
            let (mut state, mut reason) = match &hub.target {
                None => ("off", ""),
                Some(t) if !t.phone => ("unavailable", "not_phone"),
                Some(t) if t.old => ("unavailable", "old_phone"),
                // Its /v1/state isn't in yet
                Some(t) if t.offered.is_none() => (if wanted.is_empty() { "off" } else { "starting" }, ""),
                Some(t) => {
                    let offered = t.offered.as_ref().unwrap();
                    kinds = KINDS.iter().copied().filter(|k| wanted.contains(k) && offered.contains(k)).collect();
                    if !wanted.is_empty() && kinds.is_empty() {
                        // Orientation needs one of the phone's own cameras streaming, not just its motion sensors
                        let camera = wanted.contains("orientation") && offered.iter().any(|k| *k == "gravity" || *k == "accelerometer");
                        ("unavailable", if camera { "camera" } else { "none" })
                    } else {
                        (if kinds.is_empty() { "off" } else { "starting" }, "")
                    }
                }
            };
            let url = match &hub.target {
                Some(Target { base: Some(base), .. }) if !kinds.is_empty() => Some(format!("{base}/sensors?kinds={}", kinds.join(","))),
                _ => None,
            };
            let link = hub.target.as_ref().and_then(|t| t.link.clone());
            let key = link.as_ref().map(|l| l.key()).unwrap_or_default();
            if let Some(r) = &hub.reader {
                if url.as_deref() != Some(r.url.as_str()) || r.key != key || r.stopped.load(Ordering::SeqCst) {
                    stop = hub.reader.take();
                }
            }
            match (&url, &link, hub.reader.is_some()) {
                (Some(url), Some(link), false) => hub.reader = Some(self.spawn_reader(url, link.clone(), kinds.clone())),
                (_, _, true) => (state, reason) = (hub.state, hub.reason), // the reader says how it's going
                _ => {}
            }
            hub.streaming = if state == "on" { kinds.clone() } else { Vec::new() };
            hub.state = state;
            hub.reason = reason;
            if kinds.is_empty() {
                hub.reading = None;
            } else if let Some(r) = &hub.reading {
                hub.reading = Some(only(r, &kinds));
            }
            let after = (hub.state, hub.reason, hub.streaming.clone());
            (before, after, hub.target.as_ref().map(|t| t.source_id.clone()))
        };
        if let Some(r) = stop {
            r.stop();
        }
        if let Some(sid) = source_id.filter(|_| before != after) {
            self.notify(&sid);
        }
    }

    fn notify(&self, source_id: &str) {
        let f = self.inner.on_change.lock().unwrap().clone();
        f(source_id);
    }

    fn spawn_reader(&self, url: &str, link: Arc<dyn SensorLink>, _kinds: Vec<&'static str>) -> ReaderHandle {
        let handle = ReaderHandle { id: self.inner.next_id.fetch_add(1, Ordering::SeqCst), url: url.to_string(), key: link.key(),
                                    stopped: Arc::default(), current: Arc::default() };
        let run = Reader { hub: Arc::downgrade(&self.inner), id: handle.id, url: url.to_string(), link, stopped: handle.stopped.clone(),
                           current: handle.current.clone() };
        std::thread::Builder::new().name("phone-sensors".into()).spawn(move || run.run()).expect("sensors thread");
        handle
    }

    /// The followed source's id while reader id is the one that counts.
    fn current(hub: &Hub, id: u64) -> Option<String> {
        match (&hub.reader, &hub.target) {
            (Some(r), Some(t)) if r.id == id => Some(t.source_id.clone()),
            _ => None,
        }
    }

    fn reader_hello(&self, id: u64, kinds: Option<&Value>) {
        let sid = {
            let mut hub = self.inner.hub.lock().unwrap();
            let Some(sid) = SensorHub::current(&hub, id) else { return };
            hub.state = "on";
            hub.reason = "";
            let list = kinds.and_then(Value::as_array);
            hub.streaming = KINDS.iter().copied().filter(|k| list.is_some_and(|l| l.iter().any(|v| v.as_str() == Some(k)))).collect();
            sid
        };
        self.notify(&sid);
    }

    fn reader_line(&self, id: u64, line: &Value) {
        let sid = {
            let mut hub = self.inner.hub.lock().unwrap();
            let Some(sid) = SensorHub::current(&hub, id) else { return };
            hub.reading = Some(apply_line(&hub.reading.clone().unwrap_or_default(), line, now()));
            sid
        };
        self.notify(&sid);
    }

    fn reader_state(&self, id: u64, state: &'static str, reason: &'static str) {
        let sid = {
            let mut hub = self.inner.hub.lock().unwrap();
            let Some(sid) = SensorHub::current(&hub, id) else { return };
            if (hub.state, hub.reason) == (state, reason) {
                return;
            }
            hub.state = state;
            hub.reason = reason;
            hub.streaming.clear();
            hub.reading = None;
            sid
        };
        self.notify(&sid);
    }

    /// Whether the reader that's running stops when told (tests).
    #[cfg(test)]
    fn reader_stopped(&self) -> Option<Arc<AtomicBool>> {
        self.inner.hub.lock().unwrap().reader.as_ref().map(|r| r.stopped.clone())
    }
}

/// Reads one /v1/sensors on its own thread until stopped, opening it again when it drops.
struct Reader {
    hub: Weak<Inner>,
    id: u64,
    url: String,
    link: Arc<dyn SensorLink>,
    stopped: Arc<AtomicBool>,
    current: Arc<Mutex<Option<Aborter>>>,
}

impl Reader {
    fn hub(&self) -> Option<SensorHub> {
        self.hub.upgrade().map(|inner| SensorHub { inner })
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn run(self) {
        let mut failing_since: Option<f64> = None;
        let retry = self.hub().map(|h| h.inner.retry).unwrap_or(Duration::from_secs(2));
        while !self.stopped() {
            let problem = self.read_once();
            if self.stopped() {
                break;
            }
            let Some(hub) = self.hub() else { break };
            if let Some(p @ ("old_phone" | "none")) = problem {
                hub.reader_state(self.id, "unavailable", p);
                return; // asking again won't change the answer
            }
            let t = now();
            failing_since = match (failing_since, problem) {
                (Some(since), Some(_)) => Some(since),
                _ => Some(t),
            };
            if t - failing_since.unwrap() >= FAIL_AFTER_S {
                hub.reader_state(self.id, "unavailable", "failed");
            } else {
                hub.reader_state(self.id, "starting", "");
            }
            drop(hub);
            let deadline = std::time::Instant::now() + retry;
            while std::time::Instant::now() < deadline && !self.stopped() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// Streams until the connection ends; None when it got through at all, else what went wrong.
    fn read_once(&self) -> Option<&'static str> {
        let mut resp = match self.link.open(&self.url) {
            Ok(r) => r,
            Err(Error::Http(404)) => return Some("old_phone"),
            Err(Error::Http(409)) => return Some("none"),
            Err(_) => return Some("failed"),
        };
        *self.current.lock().unwrap() = Some(resp.aborter());
        if self.stopped() {
            resp.aborter().abort();
        }
        let mut got_through = false;
        let alive_stop = Arc::new(AtomicBool::new(false));
        // One keep-alive worker for the connection: a later greeting only changes how often it pings
        let mut alive_every: Option<Arc<AtomicU64>> = None;
        while !self.stopped() {
            let Ok(raw) = resp.readline(MAX_LINE) else { break };
            if raw.is_empty() {
                break;
            }
            let Ok(line) = serde_json::from_slice::<Value>(&raw) else { continue };
            if !line.is_object() {
                continue;
            }
            got_through = true;
            let Some(hub) = self.hub() else { break };
            if line.get("sensors").is_some() {
                let alive_ms = line.get("alive_ms").and_then(Value::as_u64).filter(|n| *n > 0).unwrap_or(ALIVE_DEFAULT_MS);
                let every = (alive_ms / 3).max(100); // a tiny alive_ms from a hostile phone must not ping in a tight loop
                match &alive_every {
                    Some(shared) => shared.store(every, Ordering::SeqCst),
                    None => alive_every = self.keep_alive(every, alive_stop.clone()),
                }
                hub.reader_hello(self.id, line.get("sensors"));
            } else {
                hub.reader_line(self.id, &line);
            }
        }
        alive_stop.store(true, Ordering::SeqCst);
        *self.current.lock().unwrap() = None;
        if got_through { None } else { Some("failed") }
    }

    /// Tells the phone this computer is still here: it stops the sensors of one that went quiet. Returns the interval in ms the worker reads, None when it couldn't start.
    fn keep_alive(&self, every_ms: u64, done: Arc<AtomicBool>) -> Option<Arc<AtomicU64>> {
        let url = format!("{}/alive", self.url.split('?').next().unwrap_or(&self.url));
        let (link, stopped) = (self.link.clone(), self.stopped.clone());
        let every = Arc::new(AtomicU64::new(every_ms));
        let shared = every.clone();
        let spawned = std::thread::Builder::new().name("phone-sensors-alive".into()).spawn(move || loop {
            let last = std::time::Instant::now();
            while last.elapsed() < Duration::from_millis(shared.load(Ordering::SeqCst)) {
                if done.load(Ordering::SeqCst) || stopped.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            link.alive(&url); // the stream itself notices a phone that's gone
        });
        match spawned {
            Ok(_) => Some(every),
            Err(e) => {
                log::warn!("The sensors keep-alive couldn't get a thread: {e}");
                None
            }
        }
    }
}

impl PhoneSensors for SensorHub {
    fn want(&self, owner: &str, kinds: &[String]) {
        let kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
        SensorHub::want(self, owner, &kinds);
    }

    fn wanted(&self) -> Vec<String> {
        SensorHub::wanted(self).into_iter().map(str::to_string).collect()
    }

    fn status(&self, source_id: Option<&str>) -> Value {
        SensorHub::status(self, source_id).to_value()
    }

    fn reading(&self, source_id: Option<&str>) -> Option<Value> {
        SensorHub::reading(self, source_id).map(|r| r.to_value())
    }

    fn follow(&self, source_id: Option<&str>, stream_base: Option<&str>, phone: Option<&PairedPhone>, state: Option<&Value>) {
        let link = phone.map(|p| Arc::new(crate::phones::auth_of(p)) as Arc<dyn SensorLink>);
        SensorHub::follow(self, source_id, stream_base, link, phone.is_some(), state);
    }

    fn diagnostics(&self) -> Value {
        SensorHub::diagnostics(self)
    }

    fn shutdown(&self) {
        SensorHub::shutdown(self);
    }

    fn set_on_change(&self, on_change: Box<dyn Fn(&str) + Send + Sync>) {
        SensorHub::set_on_change(self, on_change);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    const ALL: [&str; 7] = KINDS;

    fn all() -> Value {
        json!({"sensors": ALL})
    }

    /// A /v1/sensors response the test writes lines into.
    struct FakeResponse {
        lines: Mutex<mpsc::Receiver<Vec<u8>>>,
        aborter: Aborter,
    }

    impl LineStream for Arc<FakeResponse> {
        fn readline(&mut self, _limit: usize) -> std::io::Result<Vec<u8>> {
            let lines = self.lines.lock().unwrap();
            while !self.aborter.aborted() {
                match lines.recv_timeout(Duration::from_millis(20)) {
                    Ok(line) => return Ok(line),
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => return Ok(Vec::new()),
                }
            }
            Ok(Vec::new())
        }

        fn aborter(&self) -> Aborter {
            self.aborter.clone()
        }
    }

    struct Opened {
        url: String,
        send: mpsc::Sender<Vec<u8>>,
        resp: Arc<FakeResponse>,
    }

    impl Opened {
        fn send(&self, line: Value) {
            let _ = self.send.send(format!("{line}\n").into_bytes());
        }

        fn close(&self) {
            self.resp.aborter.abort();
        }

        fn live(&self) -> bool {
            !self.resp.aborter.aborted()
        }
    }

    /// Answers each /v1/sensors with a FakeResponse (or the HTTP error in refuse) and keeps them.
    #[derive(Default)]
    struct FakeLink {
        refuse: u16,
        opened: Mutex<Vec<Arc<Opened>>>,
        pings: Mutex<Vec<String>>,
        id: String,
    }

    impl FakeLink {
        fn new(id: &str) -> Arc<FakeLink> {
            Arc::new(FakeLink { id: id.into(), ..Default::default() })
        }

        fn refusing(code: u16) -> Arc<FakeLink> {
            Arc::new(FakeLink { refuse: code, id: format!("refuse{code}"), ..Default::default() })
        }

        fn live(&self) -> Vec<Arc<Opened>> {
            self.opened.lock().unwrap().iter().filter(|o| o.live()).cloned().collect()
        }

        fn live_urls(&self) -> Vec<String> {
            self.live().iter().map(|o| o.url.clone()).collect()
        }

        fn count(&self) -> usize {
            self.opened.lock().unwrap().len()
        }
    }

    impl SensorLink for FakeLink {
        fn open(&self, url: &str) -> Result<Box<dyn LineStream>, Error> {
            if self.refuse != 0 {
                return Err(Error::Http(self.refuse));
            }
            let (send, recv) = mpsc::channel();
            let resp = Arc::new(FakeResponse { lines: Mutex::new(recv), aborter: Aborter::detached() });
            self.opened.lock().unwrap().push(Arc::new(Opened { url: url.into(), send, resp: resp.clone() }));
            Ok(Box::new(resp))
        }

        fn alive(&self, url: &str) {
            self.pings.lock().unwrap().push(url.into());
        }

        fn key(&self) -> String {
            self.id.clone()
        }
    }

    fn wait_for(cond: impl Fn() -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        cond()
    }

    fn soon(cond: impl Fn() -> bool) -> bool {
        wait_for(cond, Duration::from_secs(3))
    }

    fn hub() -> (SensorHub, Arc<Mutex<Vec<String>>>) {
        let changes: Arc<Mutex<Vec<String>>> = Arc::default();
        let record = changes.clone();
        (SensorHub::with_retry(move |s| record.lock().unwrap().push(s.into()), Duration::from_millis(50)), changes)
    }

    fn follow(hub: &SensorHub, sid: &str, base: &str, link: &Arc<FakeLink>, state: Option<Value>) {
        hub.follow(Some(sid), Some(base), Some(link.clone() as Arc<dyn SensorLink>), true, state.as_ref());
    }

    #[test]
    fn nothing_streams_until_something_wants_it() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        follow(&hub, "phone", "https://p/v1", &link, Some(all()));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(link.count(), 0);
        assert_eq!((hub.status(None).state, hub.status(None).kinds), ("off", ALL.to_vec()));
    }

    #[test]
    fn wants_are_counted_by_owner_and_the_stream_stops_when_nobody_wants_any() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        follow(&hub, "phone", "https://p/v1", &link, Some(all()));
        hub.want("level", &["orientation"]);
        assert!(soon(|| link.live().len() == 1));
        assert_eq!(link.live_urls(), ["https://p/v1/sensors?kinds=orientation"]);
        assert_eq!(hub.status(None).state, "starting");
        hub.want("meter", &["light", "orientation"]);
        assert!(soon(|| link.live_urls() == ["https://p/v1/sensors?kinds=orientation,light"]));
        hub.want("level", &["orientation"]);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(link.count(), 2);
        hub.want("meter", &[]);
        assert!(soon(|| link.live_urls() == ["https://p/v1/sensors?kinds=orientation"]));
        hub.want("level", &[]);
        assert!(soon(|| link.live().is_empty()));
        assert!(hub.status(None).state == "off" && hub.wanted().is_empty());
    }

    #[test]
    fn readings_come_in_after_hello_and_only_for_that_stream() {
        let (hub, changes) = hub();
        let link = FakeLink::new("a");
        follow(&hub, "phone", "https://p/v1", &link, Some(all()));
        hub.want("level", &["orientation", "light"]);
        assert!(soon(|| link.live().len() == 1));
        let resp = link.live()[0].clone();
        resp.send(json!({"version": 1, "sensors": ["orientation", "light"]}));
        assert!(soon(|| hub.status(None).state == "on"));
        assert_eq!(hub.status(None).streaming, ["orientation", "light"]);
        resp.send(json!({"t": 1, "orientation": {"roll": 3.5, "pitch": -1.0, "camera": "0"}, "light": 120.0}));
        assert!(soon(|| hub.reading(None).is_some()));
        resp.send(json!({"t": 2, "orientation": {"roll": null, "pitch": 85.0, "camera": "0"}}));
        assert!(soon(|| hub.reading(None).unwrap().pitch == Some(85.0)));
        let reading = hub.reading(Some("phone")).unwrap();
        assert!(reading.roll.is_none() && reading.light == Some(120.0) && reading.camera == "0");
        assert!(hub.reading(Some("other")).is_none() && hub.status(Some("other")).state == "off");
        assert!(changes.lock().unwrap().contains(&"phone".to_string()));
        hub.shutdown();
    }

    #[test]
    fn a_kind_nobody_wants_any_more_leaves_the_reading() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        follow(&hub, "phone", "https://p/v1", &link, Some(all()));
        hub.want("level", &["orientation"]);
        hub.want("meter", &["light"]);
        let found = Mutex::new(None);
        // Taken inside the wait: the stream for orientation alone may still be letting go meanwhile
        assert!(soon(|| {
            let live = link.live();
            let ours = live.iter().find(|o| o.url.ends_with("orientation,light")).cloned();
            let only = live.len() == 1 && ours.is_some();
            *found.lock().unwrap() = ours;
            only
        }));
        let resp = found.into_inner().unwrap().unwrap();
        resp.send(json!({"version": 1, "sensors": ["orientation", "light"]}));
        resp.send(json!({"t": 1, "orientation": {"roll": 3.5, "pitch": -1.0, "camera": "0"}, "light": 120.0}));
        assert!(soon(|| hub.reading(None).is_some_and(|r| r.roll == Some(3.5))));
        hub.want("level", &[]);
        let reading = hub.reading(None).unwrap();
        assert!(reading.roll.is_none() && reading.pitch.is_none() && reading.orientation_at == 0.0);
        assert_eq!(reading.light, Some(120.0));
        hub.shutdown();
    }

    #[test]
    fn an_old_phone_is_unavailable_with_a_reason_and_never_asked() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        hub.want("level", &["orientation"]);
        follow(&hub, "old", "https://p/v1", &link, Some(json!({"cameras": []})));
        assert_eq!((hub.status(None).state, hub.status(None).reason), ("unavailable", "old_phone"));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(link.count(), 0);
    }

    #[test]
    fn a_state_that_couldnt_be_fetched_isnt_taken_for_an_old_phone() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        hub.want("level", &["orientation"]);
        follow(&hub, "phone", "https://p/v1", &link, Some(all()));
        assert!(soon(|| link.live().len() == 1));
        follow(&hub, "phone", "https://p/v1", &link, Some(json!({})));
        assert!(hub.status(None).reason != "old_phone" && hub.status(None).kinds == ALL.to_vec());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(link.live().len(), 1);
        hub.shutdown();
    }

    #[test]
    fn a_phone_answering_404_is_taken_for_an_old_one() {
        let (hub, _) = hub();
        hub.want("level", &["orientation"]);
        follow(&hub, "phone", "https://p/v1", &FakeLink::refusing(404), Some(all()));
        assert!(soon(|| hub.status(None).state == "unavailable"));
        assert_eq!(hub.status(None).reason, "old_phone");
    }

    #[test]
    fn a_phone_without_the_sensor_says_so() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        hub.want("meter", &["light"]);
        follow(&hub, "phone", "https://p/v1", &link, Some(json!({"sensors": ["orientation"]})));
        assert_eq!((hub.status(None).state, hub.status(None).reason), ("unavailable", "none"));
        assert_eq!(link.count(), 0);
        follow(&hub, "phone", "https://p/v1", &FakeLink::refusing(409), Some(json!({"sensors": ["light"]})));
        assert!(soon(|| hub.status(None).state == "unavailable"));
        assert_eq!(hub.status(None).reason, "none");
    }

    #[test]
    fn something_other_than_a_phone_has_no_sensors() {
        let (hub, _) = hub();
        hub.want("level", &["orientation"]);
        hub.follow(Some("browser"), None, None, false, None);
        assert_eq!((hub.status(None).state, hub.status(None).reason), ("unavailable", "not_phone"));
    }

    #[test]
    fn it_waits_for_the_phones_state_before_asking() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        hub.want("level", &["orientation"]);
        follow(&hub, "phone", "https://p/v1", &link, None);
        assert_eq!(hub.status(None).state, "starting");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(link.count(), 0);
        follow(&hub, "phone", "https://p/v1", &link, Some(all()));
        assert!(soon(|| link.live().len() == 1));
        hub.shutdown();
    }

    #[test]
    fn moving_to_another_phone_stops_the_first_and_streams_from_the_second() {
        let (hub, changes) = hub();
        let (first, second) = (FakeLink::new("a"), FakeLink::new("b"));
        hub.want("level", &["orientation"]);
        follow(&hub, "a", "https://a/v1", &first, Some(all()));
        assert!(soon(|| first.live().len() == 1));
        first.live()[0].send(json!({"t": 1, "orientation": {"roll": 1.0, "pitch": 0.0}}));
        assert!(soon(|| hub.reading(Some("a")).is_some()));
        changes.lock().unwrap().clear();
        follow(&hub, "b", "https://b/v1", &second, None);
        assert!(soon(|| first.live().is_empty()));
        assert!(changes.lock().unwrap().contains(&"a".to_string()));
        assert!(hub.reading(Some("a")).is_none() && hub.reading(None).is_none());
        follow(&hub, "b", "https://b/v1", &second, Some(all()));
        assert!(soon(|| second.live_urls() == ["https://b/v1/sensors?kinds=orientation"]));
        hub.shutdown();
    }

    #[test]
    fn a_new_route_opens_the_stream_there() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        hub.want("level", &["orientation"]);
        follow(&hub, "phone", "https://usb/v1", &link, Some(all()));
        assert!(soon(|| link.live().len() == 1));
        follow(&hub, "phone", "https://wifi/v1", &link, None);
        assert!(soon(|| link.live_urls() == ["https://wifi/v1/sensors?kinds=orientation"]));
        hub.shutdown();
    }

    #[test]
    fn the_stream_stopping_turns_it_off() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        hub.want("level", &["orientation"]);
        follow(&hub, "phone", "https://p/v1", &link, Some(all()));
        assert!(soon(|| link.live().len() == 1));
        hub.follow(None, None, None, true, None);
        assert!(soon(|| link.live().is_empty()));
        assert!(hub.status(None).state == "off" && hub.reading(None).is_none());
    }

    #[test]
    fn a_dropped_stream_is_opened_again() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        hub.want("level", &["orientation"]);
        follow(&hub, "phone", "https://p/v1", &link, Some(all()));
        assert!(soon(|| link.live().len() == 1));
        let resp = link.live()[0].clone();
        resp.send(json!({"version": 1, "sensors": ["orientation"]}));
        assert!(soon(|| hub.status(None).state == "on"));
        resp.close(); // the phone went away
        assert!(soon(|| link.count() == 2 && link.live().len() == 1));
        assert_eq!(hub.status(None).state, "starting");
        hub.shutdown();
    }

    #[test]
    fn diagnostics_only_while_wanted() {
        let (hub, _) = hub();
        assert_eq!(hub.diagnostics(), json!({}));
        hub.want("level", &["orientation"]);
        assert!(hub.diagnostics()["Phone sensors"].as_str().unwrap().contains("orientation"));
    }

    #[test]
    fn unknown_kinds_are_ignored() {
        let (hub, _) = hub();
        hub.want("x", &["sonar"]);
        assert!(hub.wanted().is_empty());
    }

    #[test]
    fn a_line_only_changes_what_it_carries() {
        let reading = apply_line(&SensorReading::default(), &json!({"t": 1, "gravity": [0.1, 9.8, 0.2], "light": 30}), 5.0);
        assert_eq!(reading.gravity, Some(vec![Some(0.1), Some(9.8), Some(0.2)]));
        assert!(reading.light == Some(30.0) && reading.at == 5.0 && reading.orientation_at == 0.0);
        let reading = apply_line(&reading, &json!({"t": 2, "orientation": {"roll": -4, "pitch": 2.5, "camera": "1"}}), 6.0);
        assert_eq!((reading.roll, reading.pitch, reading.camera.as_str(), reading.light), (Some(-4.0), Some(2.5), "1", Some(30.0)));
        assert_eq!(reading.orientation_at, 6.0);
        assert_eq!(apply_line(&reading, &json!({"t": 3, "light": 31}), 7.0).orientation_at, 6.0);
        assert_eq!(apply_line(&reading, &json!({"t": 3}), 7.0), reading);
        assert_eq!(apply_line(&reading, &json!({"orientation": {"roll": "x", "pitch": true}}), 8.0).roll, None);
    }

    #[test]
    fn an_external_camera_is_its_own_reason() {
        let (hub, _) = hub();
        hub.want("level", &["orientation"]);
        follow(&hub, "phone", "https://p/v1", &FakeLink::new("a"), Some(json!({"sensors": ["accelerometer", "gravity", "light"]})));
        assert_eq!((hub.status(None).state, hub.status(None).reason), ("unavailable", "camera"));
    }

    #[test]
    fn it_tells_the_phone_it_is_still_there() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        hub.want("level", &["orientation"]);
        follow(&hub, "phone", "https://p/v1", &link, Some(all()));
        assert!(soon(|| link.live().len() == 1));
        link.live()[0].send(json!({"version": 1, "sensors": ["orientation"], "alive_ms": 300}));
        assert!(wait_for(|| link.pings.lock().unwrap().len() >= 3, Duration::from_secs(2)));
        assert_eq!(link.pings.lock().unwrap()[0], "https://p/v1/sensors/alive");
        hub.want("level", &[]);
        assert!(soon(|| link.live().is_empty()));
        let pings = link.pings.lock().unwrap().len();
        std::thread::sleep(Duration::from_millis(400));
        assert!(link.pings.lock().unwrap().len() <= pings + 1);
    }

    #[test]
    fn greeting_again_does_not_start_another_keep_alive() {
        let (hub, _) = hub();
        let link = FakeLink::new("a");
        hub.want("level", &["orientation"]);
        follow(&hub, "phone", "https://p/v1", &link, Some(all()));
        assert!(soon(|| link.live().len() == 1));
        // A slow greeting first, then a flood of them: the last one sets the pace and there is still one worker
        link.live()[0].send(json!({"version": 1, "sensors": ["orientation"], "alive_ms": 60000}));
        for _ in 0..30 {
            link.live()[0].send(json!({"version": 1, "sensors": ["orientation"], "alive_ms": 300}));
        }
        assert!(wait_for(|| !link.pings.lock().unwrap().is_empty(), Duration::from_secs(2)));
        std::thread::sleep(Duration::from_millis(500));
        let pings = link.pings.lock().unwrap().len();
        // One worker every 100 ms is about six; thirty-one of them would be over a hundred
        assert!(pings <= 12, "{pings} pings");
    }

    #[test]
    fn letting_go_never_waits_for_a_read_in_progress() {
        // A real HTTP response blocked reading a line that never comes
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\n\r\n{\"version\": 1, \"sensors\": [\"orientation\"]}\n");
            std::thread::sleep(Duration::from_secs(10));
        });
        let (hub, _) = hub();
        hub.want("level", &["orientation"]);
        let auth: Arc<dyn SensorLink> = Arc::new(PhoneAuth::new("t", ""));
        hub.follow(Some("phone"), Some(&format!("http://127.0.0.1:{port}/v1")), Some(auth), true, Some(&all()));
        assert!(soon(|| hub.status(None).state == "on"));
        // Only the reader's threads and the hub hold this, so it goes when the blocked thread has really ended
        let reader_alive = Arc::downgrade(&hub.reader_stopped().unwrap());
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        hub.want("level", &[]);
        assert!(started.elapsed() < Duration::from_millis(300));
        assert!(wait_for(|| reader_alive.strong_count() == 0, Duration::from_secs(2)));
    }
}
