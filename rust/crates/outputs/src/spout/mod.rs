//! The Spout capture plugin: stream from the Spout senders on this computer (games, OBS, VTuber and 3D programs share their picture
//! that way).
//!
//! Each sender is a StreamSource of its own, listed in the device picker by its name while it's there, and streams like a webcam does. One
//! that goes away while it streams keeps its place, apps see the wait screen and the stream carries on when it's back; one that isn't
//! streaming just leaves the picker. What reads the senders is a `Backend` (Windows reads Spout's shared textures itself), so this part
//! knows nothing of the system. The card shows while a sender is picked and holds its frame rate, kept per sender.
//!
//! Looking for senders can wait on the system (a sender's lock, a PipeWire daemon that is slow to answer), so a thread of its own does it,
//! one look at a time, and posts what it found back to the plugin.

#[cfg(any(windows, test))]
mod frame_count;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
mod pipewire;

use crate::convert::{bgra_to_i420, rgb_to_i420};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::frame::Frame;
use telescope_core::host::{Host, Poster};
use telescope_core::i18n::{n_, tr, Fields};
use telescope_core::event::Event;
use telescope_core::media::{Codec, Decoded, Decoder, FeedEnd, FeedStopper, OpenStop, Packet, VideoFeed};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::source::{ControlClient, StreamSource};
use telescope_core::ui::{CardSpec, Issue, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "spout";
pub const SOURCE_PREFIX: &str = "spout:";
const UI: &str = include_str!("../../ui/spout.slint");
/// How often to look for senders coming and going.
pub const SCAN_MS: u64 = 2000;
pub const DEFAULT_FPS: u32 = 30;
/// What the card offers; a rate saved from elsewhere is offered as well.
pub const FPS_CHOICES: [u32; 4] = [15, 24, 30, 60];
const T_SCAN: &str = "scan";
const P_REPORTED: &str = "reported";
const P_GONE: &str = "gone";
const P_SCANNED: &str = "scanned";
/// How long opening a sender waits for its first picture.
const FIRST_PICTURE: Duration = Duration::from_secs(5);
/// A sender that's idle gets its last picture sent again this often, so the stream doesn't take it for gone.
const KEEP_ALIVE: Duration = Duration::from_secs(1);

const GONE_TEXT: &str = n_("The sender isn't running. Start it again to continue.");
const SILENT_TEXT: &str = n_("The sender isn't sharing a picture. Check that it's running and sending.");
const CANT_READ_TEXT: &str = n_("Telescope can't read this sender's picture. Try another sender, or check the sender's output settings.");

// ── What reads the senders ──────────────────────────────────────────────

/// A Spout sender running now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sender {
    /// Stays the same for a sender across restarts of the program sending, since settings are saved under it.
    pub key: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
}

impl Sender {
    fn to_json(&self) -> Value {
        json!({"key": self.key, "name": self.name, "width": self.width, "height": self.height})
    }

    fn from_json(value: &Value) -> Option<Sender> {
        let number = |field: &str| value.get(field).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
        Some(Sender { key: value.get("key")?.as_str()?.into(), name: value.get("name")?.as_str()?.into(), width: number("width")?, height: number("height")? })
    }
}

/// One picture from a sender, rows packed with no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// "BGRA", "RGBA", "BGRx" or "RGBx": the byte order of a pixel, the fourth byte (alpha or unused) being dropped.
    pub fourcc: &'static str,
    pub data: Vec<u8>,
}

/// Where senders come from on this system.
pub trait Backend: Send + Sync {
    /// The senders there are now, asked every 2 s from a thread of the plugin's own, so it may wait. A backend that can't reach what
    /// it reads yet (PipeWire not started) tries again here each time and lists nothing until it can.
    fn senders(&self) -> Vec<Sender>;
    fn open(&self, key: &str) -> Result<Box<dyn Reader>, String>;
    /// Why there are no senders to see right now, for the diagnostics; None when nothing is wrong.
    fn problem(&self) -> Option<String> {
        None
    }
}

/// One sender opened for reading.
pub trait Reader: Send {
    /// The next new picture, waiting up to `timeout` for it: Ok(None) on timeout (the sender is idle), Err once the sender is gone.
    fn next(&mut self, timeout: Duration) -> Result<Option<Picture>, String>;
}

/// The backend this system has, if any.
fn system_backend() -> Option<Arc<dyn Backend>> {
    #[cfg(windows)]
    return self::windows::backend().map(Arc::from);
    #[cfg(target_os = "linux")]
    return Some(Arc::from(pipewire::backend()));
    #[cfg(not(any(windows, target_os = "linux")))]
    None
}

// ── Pictures as packets ─────────────────────────────────────────────────

/// A picture as the bytes the stream carries: width and height (u32 little-endian), the fourcc, then the pixels. Each packet says its own
/// size, so a sender that resizes needs no new decoder.
fn packet_of(picture: &Picture) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + picture.data.len());
    out.extend_from_slice(&picture.width.to_le_bytes());
    out.extend_from_slice(&picture.height.to_le_bytes());
    out.extend_from_slice(picture.fourcc.as_bytes());
    out.extend_from_slice(&picture.data);
    out
}

/// Turns the packets of `packet_of` into I420 frames, alpha dropped.
#[derive(Default)]
struct SpoutDecoder;

impl Decoder for SpoutDecoder {
    fn decode(&mut self, packet: &Packet) -> Decoded {
        match self.frame(&packet.data) {
            Some(frame) => Decoded { newest: Some(frame), count: 1 },
            None => Decoded::default(),
        }
    }
    fn hardware(&self) -> bool {
        false
    }
}

impl SpoutDecoder {
    fn frame(&self, bytes: &[u8]) -> Option<Frame> {
        let (head, pixels) = bytes.split_at_checked(12)?;
        let (width, height) = (u32::from_le_bytes(head[..4].try_into().ok()?), u32::from_le_bytes(head[4..8].try_into().ok()?));
        if width == 0 || height == 0 || pixels.len() != width as usize * height as usize * 4 {
            return None;
        }
        match &head[8..12] {
            b"RGBA" | b"RGBx" => Some(rgb_to_i420(pixels, width, height, 4, false)),
            b"BGRA" | b"BGRx" => Some(bgra_to_i420(pixels, width, height)),
            _ => None,
        }
    }
}

// ── What the plugin and its streams share ───────────────────────────────

/// What the streams' threads read: the senders as there are now and what's picked for each.
#[derive(Default)]
struct Shared {
    senders: Mutex<HashMap<String, Sender>>,
    fps: Mutex<HashMap<String, u32>>,
}

impl Shared {
    fn sender(&self, key: &str) -> Option<Sender> {
        self.senders.lock().unwrap().get(key).cloned()
    }

    fn fps(&self, key: &str) -> u32 {
        self.fps.lock().unwrap().get(key).copied().unwrap_or(DEFAULT_FPS)
    }
}

struct NoControl;

impl ControlClient for NoControl {
    fn send(&self, _body: &Value) -> Result<(), String> {
        Ok(())
    }
    fn state(&self) -> Result<Value, String> {
        Err("a Spout sender has no phone to ask".into())
    }
    fn has_audio(&self) -> bool {
        false
    }
}

/// One Spout sender in the picker.
pub struct SpoutSource {
    key: String,
    name: Mutex<String>,
    backend: Arc<dyn Backend>,
    shared: Arc<Shared>,
    poster: Option<Arc<dyn Poster>>,
    waiting: Arc<Mutex<String>>,
}

impl SpoutSource {
    fn report(&self, state: &str, detail: &str) {
        if let Some(p) = &self.poster {
            p.post(P_REPORTED, json!([self.key, state, detail]));
        }
    }

    fn say(&self, text: &str, state: &str) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        *self.waiting.lock().unwrap() = tr(text);
        self.report(state, "");
        Err(tr(text))
    }
}

impl StreamSource for SpoutSource {
    fn id(&self) -> String {
        format!("{SOURCE_PREFIX}{}", self.key)
    }
    fn name(&self) -> String {
        self.name.lock().unwrap().clone()
    }
    fn url(&self) -> String {
        SOURCE_PREFIX.into()
    }
    fn prepare(&self, _interactive: bool) -> bool {
        // At Start: a sender that stopped since it was picked can't stream
        if self.shared.sender(&self.key).is_some() {
            return true;
        }
        if let Some(p) = &self.poster {
            p.post(P_GONE, json!(self.name()));
        }
        false
    }
    fn open_feed(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        self.open_feed_stoppable(&OpenStop::default())
    }
    fn open_feed_stoppable(&self, open: &OpenStop) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        if self.shared.sender(&self.key).is_none() {
            return self.say(GONE_TEXT, "gone");
        }
        let mut reader = match self.backend.open(&self.key) {
            Ok(r) => r,
            Err(why) => {
                log::info!("Spout sender {} didn't open: {why}", self.name());
                return if self.shared.sender(&self.key).is_none() { self.say(GONE_TEXT, "gone") } else { self.say(CANT_READ_TEXT, "silent") };
            }
        };
        let started = Instant::now();
        let first = loop {
            if open.stopped() {
                return Err("stopped".into());
            }
            match reader.next(Duration::from_millis(100)) {
                Ok(Some(picture)) => break picture,
                Ok(None) if started.elapsed() < FIRST_PICTURE => {}
                Ok(None) => return self.say(SILENT_TEXT, "silent"),
                Err(why) => {
                    log::info!("Spout sender {} gave nothing: {why}", self.name());
                    return self.say(GONE_TEXT, "gone");
                }
            }
        };
        self.report("ok", &format!("{}×{}", first.width, first.height));
        let stop = Arc::new(AtomicBool::new(false));
        let feed = SpoutFeed {
            reader, first: Some(first), stop, shared: self.shared.clone(), key: self.key.clone(), poster: self.poster.clone(), waiting: self.waiting.clone(),
            last: None, last_sent: Instant::now(), asked: Instant::now(),
        };
        Ok((Codec::Mjpeg, Box::new(feed)))
    }
    fn decoder(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(SpoutDecoder::default()))
    }
    fn control_client(&self) -> Option<Box<dyn ControlClient>> {
        Some(Box::new(NoControl))
    }
    fn fps(&self) -> u32 {
        self.shared.fps(&self.key)
    }
    fn lost_hint(&self) -> String {
        tr("Check that the sender is still running.")
    }
    fn waiting_text(&self) -> String {
        self.waiting.lock().unwrap().clone()
    }
}

/// The pictures of one opened sender, handed out no faster than the frame rate picked.
struct SpoutFeed {
    reader: Box<dyn Reader>,
    first: Option<Picture>,
    stop: Arc<AtomicBool>,
    shared: Arc<Shared>,
    key: String,
    poster: Option<Arc<dyn Poster>>,
    waiting: Arc<Mutex<String>>,
    /// The last picture's packet and its size, for a sender that goes quiet.
    last: Option<(Vec<u8>, (u32, u32))>,
    last_sent: Instant,
    /// When the reader was asked for the last picture it gave, which the rate counts from so the copy's time isn't added to each frame's.
    asked: Instant,
}

struct Stop(Arc<AtomicBool>);

impl FeedStopper for Stop {
    fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl SpoutFeed {
    fn send(&mut self, picture: Picture) -> Packet {
        let size = (picture.width, picture.height);
        if self.last.as_ref().is_some_and(|(_, was)| *was != size) {
            self.post("ok", &format!("{}×{}", size.0, size.1));
        }
        let data = packet_of(&picture);
        self.last = Some((data.clone(), size));
        self.packet(data)
    }

    fn packet(&mut self, data: Vec<u8>) -> Packet {
        self.last_sent = Instant::now();
        Packet { data, time_ns: telescope_core::time::monotonic_ns() }
    }

    fn post(&self, state: &str, detail: &str) {
        if let Some(p) = &self.poster {
            p.post(P_REPORTED, json!([self.key, state, detail]));
        }
    }
}

impl VideoFeed for SpoutFeed {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return Err(FeedEnd::Stopped);
            }
            if let Some(first) = self.first.take() {
                return Ok(self.send(first));
            }
            let interval = Duration::from_secs_f64(1.0 / self.shared.fps(&self.key).max(1) as f64);
            let wait = interval.saturating_sub(self.asked.elapsed());
            if !wait.is_zero() {
                std::thread::sleep(wait.min(Duration::from_millis(20)));
                continue;
            }
            let asked = Instant::now();
            match self.reader.next(Duration::from_millis(50)) {
                Ok(Some(picture)) => {
                    self.asked = asked;
                    return Ok(self.send(picture));
                }
                Ok(None) if self.last_sent.elapsed() >= KEEP_ALIVE => {
                    if let Some((data, _)) = self.last.clone() {
                        return Ok(self.packet(data));
                    }
                }
                Ok(None) => {}
                Err(why) => {
                    *self.waiting.lock().unwrap() = tr(GONE_TEXT);
                    self.post("gone", "");
                    return Err(FeedEnd::Error(why));
                }
            }
        }
    }

    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(Stop(self.stop.clone()))
    }
}

// ── The plugin ──────────────────────────────────────────────────────────

pub struct SpoutPlugin {
    backend: Option<Arc<dyn Backend>>,
    shared: Arc<Shared>,
    poster: Option<Arc<dyn Poster>>,
    sources: HashMap<String, Arc<SpoutSource>>,
    /// How each sender's stream is going, as the stream last said: ("ok", "1920×1080"), ("gone", "") or ("silent", "").
    states: HashMap<String, (String, String)>,
    selected_key: String,
    stopped: bool,
    /// Asks the scanning thread for a look; the thread ends when this is dropped.
    scanner: Option<mpsc::Sender<()>>,
    /// Whether a look is under way, so there is never a second one beside it.
    scanning: bool,
    /// What the backend said last time it found nothing to read, for the diagnostics.
    problem: Option<String>,
}

/// Runs `backend.senders()` for each request on a thread of its own and posts the listing; it ends once `asked` has no sender left.
fn spawn_scanner(backend: Arc<dyn Backend>, poster: Arc<dyn Poster>) -> Option<mpsc::Sender<()>> {
    let (ask, asked) = mpsc::channel::<()>();
    let run = move || {
        while asked.recv().is_ok() {
            let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (backend.senders(), backend.problem())));
            let data = match found {
                Ok((senders, problem)) => json!({"senders": senders.iter().map(Sender::to_json).collect::<Vec<_>>(), "problem": problem}),
                Err(_) => {
                    log::error!("Looking for Spout senders failed");
                    json!({"senders": [], "problem": "looking for senders failed"})
                }
            };
            poster.post(P_SCANNED, data);
        }
    };
    std::thread::Builder::new().name("spout-scan".into()).spawn(run).ok()?;
    Some(ask)
}

impl SpoutPlugin {
    /// The plugin reading senders through `backend`; None for a system that has no way to.
    pub fn new(backend: Option<Arc<dyn Backend>>) -> SpoutPlugin {
        SpoutPlugin { backend, shared: Arc::default(), poster: None, sources: HashMap::new(), states: HashMap::new(), selected_key: String::new(), stopped: false,
            scanner: None, scanning: false, problem: None,
        }
    }

    /// The plugin as the app runs it: this system's backend, if it has one.
    pub fn system() -> SpoutPlugin {
        SpoutPlugin::new(system_backend())
    }

    /// Asks for a look at the senders, unless one is still under way.
    fn request_scan(&mut self) {
        if self.scanning {
            return;
        }
        if self.scanner.as_ref().is_some_and(|ask| ask.send(()).is_ok()) {
            self.scanning = true;
        }
    }

    /// The senders as the scanning thread found them.
    fn on_scanned(&mut self, host: &mut dyn Host, data: &Value) {
        self.scanning = false;
        if self.stopped {
            return;
        }
        let found: Vec<Sender> = data.get("senders").and_then(Value::as_array).map(|list| list.iter().filter_map(Sender::from_json).collect()).unwrap_or_default();
        self.problem = data.get("problem").and_then(Value::as_str).map(String::from);
        self.apply_senders(host, found);
    }

    fn apply_senders(&mut self, host: &mut dyn Host, found: Vec<Sender>) {
        *self.shared.senders.lock().unwrap() = found.iter().map(|s| (s.key.clone(), s.clone())).collect();
        for sender in &found {
            match self.sources.get(&sender.key) {
                None => {
                    let Some(backend) = self.backend.clone() else { continue };
                    let src = Arc::new(SpoutSource {
                        key: sender.key.clone(), name: Mutex::new(sender.name.clone()), backend, shared: self.shared.clone(), poster: self.poster.clone(),
                        waiting: Arc::new(Mutex::new(tr(GONE_TEXT))),
                    });
                    self.sources.insert(sender.key.clone(), src.clone());
                    host.add_stream_source(src);
                }
                Some(src) if src.name() != sender.name => {
                    *src.name.lock().unwrap() = sender.name.clone();
                    host.add_stream_source(src.clone());
                }
                _ => {}
            }
            if self.states.get(&sender.key).is_some_and(|s| s.0 == "gone") {
                self.states.remove(&sender.key); // back: its stream opens it again on its next try
            }
        }
        self.drop_gone(host);
        self.render(host);
    }

    /// Senders that are gone leave the picker, unless they stream: those wait to be back.
    fn drop_gone(&mut self, host: &mut dyn Host) {
        let keys: Vec<String> = self.sources.keys().cloned().collect();
        for key in keys {
            if self.shared.sender(&key).is_some() {
                continue;
            }
            let id = self.sources[&key].id();
            if host.is_streaming_from(Some(&id)) {
                self.states.insert(key, ("gone".into(), String::new()));
                continue;
            }
            self.sources.remove(&key);
            self.states.remove(&key);
            host.remove_stream_source(&id);
        }
    }

    fn on_streams_changed(&mut self, host: &mut dyn Host) {
        let keys: Vec<String> = self.sources.keys().cloned().collect();
        for key in keys {
            let id = self.sources[&key].id();
            if !host.is_streaming_from(Some(&id)) {
                self.states.remove(&key);
            }
        }
        self.drop_gone(host);
        self.render(host);
    }

    fn set_fps(&mut self, host: &mut dyn Host, fps: u32) {
        let key = self.selected_key.clone();
        if key.is_empty() || self.shared.fps(&key) == fps {
            return;
        }
        self.shared.fps.lock().unwrap().insert(key.clone(), fps);
        host.schedule_save();
        if self.sources.get(&key).is_some_and(|src| host.is_streaming_from(Some(&src.id()))) {
            host.reconnect_stream(); // the virtual camera runs at the new rate from the next opening
        }
    }

    fn fps_items(&self) -> (Vec<u32>, usize) {
        let fps = self.shared.fps(&self.selected_key);
        let mut offered: Vec<u32> = FPS_CHOICES.to_vec();
        if !offered.contains(&fps) {
            offered.push(fps);
        }
        offered.sort_unstable_by(|a, b| b.cmp(a));
        let at = offered.iter().position(|f| *f == fps).unwrap_or(0);
        (offered, at)
    }

    fn props(&self, streaming: bool) -> Value {
        let key = &self.selected_key;
        let (state, detail) = self.states.get(key).cloned().unwrap_or_default();
        let (kind, text) = if key.is_empty() {
            ("status_dim", String::new())
        } else if state == "gone" || self.shared.sender(key).is_none() {
            ("status_warn", tr("Sender not running"))
        } else if state == "silent" {
            ("status_warn", tr("Not sending anything"))
        } else if streaming {
            ("status_ok", if state == "ok" { detail } else { tr("Starting…") })
        } else {
            ("status_dim", tr("Not streaming"))
        };
        let (fps, fps_i) = self.fps_items();
        json!({
            "visible": !key.is_empty(),
            "status-text": text, "status-kind": kind,
            "fps-items": fps.iter().map(|f| format!("{f} fps")).collect::<Vec<_>>(), "fps-current": fps_i, "fps-enabled": !key.is_empty(),
            "fps-tip": tr("Set how many pictures a second to take. Spout senders don't share their own rate."),
        })
    }

    fn render(&mut self, host: &mut dyn Host) {
        let streaming = self.sources.get(&self.selected_key).is_some_and(|s| host.is_streaming_from(Some(&s.id())));
        host.set_ui_props("card", &self.props(streaming));
    }
}

impl Plugin for SpoutPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Spout capture").into(),
            about: n_("Stream from games, OBS and other programs on this computer that share their picture through Spout.").into(),
            when_off: n_("Without Spout capture the senders aren't in the device picker and the Spout capture card is gone too.").into(),
            optional: true,
            lab: true,
            category: "streaming".into(),
            icon: "monitor".into(),
            pinned: true,
            ..PluginInfo::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.poster = host.poster();
        if let (Some(backend), Some(poster)) = (self.backend.clone(), self.poster.clone()) {
            self.scanner = spawn_scanner(backend, poster);
        }
        host.schedule(0, T_SCAN); // after the window is up
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.stopped = true;
        self.scanner = None; // ends the thread once the look it may be in the middle of is done
        host.cancel_schedule(T_SCAN);
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "SpoutCard".into(), props: self.props(false) }, region: "right".into() })
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::TimerFired { owner, token } if owner == NAME && token == T_SCAN => {
                if !self.stopped {
                    self.request_scan();
                    host.schedule(SCAN_MS, T_SCAN);
                }
            }
            Event::Posted { owner, token, data } if owner == NAME => {
                if token == P_SCANNED {
                    self.on_scanned(host, data);
                } else if token == P_REPORTED {
                    let s = |i: usize| data.get(i).and_then(Value::as_str).unwrap_or("").to_string();
                    let key = s(0);
                    if self.sources.contains_key(&key) {
                        self.states.insert(key, (s(1), s(2)));
                        self.render(host);
                    }
                } else if token == P_GONE {
                    let name = data.as_str().unwrap_or("");
                    host.show_issue("start", Issue { title: tr("{name} isn't running").field("name", name), text: tr("Start it again or pick another source."),
                                                     actions: Vec::new(), kind: "warn".into(), details: String::new(), tell_dismiss: false });
                }
            }
            Event::SourceSelected { source_id } => {
                let key = source_id.strip_prefix(SOURCE_PREFIX).unwrap_or("").to_string();
                if key != self.selected_key {
                    self.selected_key = key;
                    self.render(host);
                }
            }
            Event::StreamsChanged { .. } => self.on_streams_changed(host),
            _ => {}
        }
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.callback == "fps-picked" {
            let Some(i) = event.args.first().and_then(Value::as_u64).map(|i| i as usize) else { return };
            if let Some(fps) = self.fps_items().0.get(i).copied() {
                self.set_fps(host, fps);
            }
            self.render(host);
        }
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let found = self.shared.senders.lock().unwrap().len();
        let text = match (&self.backend, &self.problem) {
            (None, _) => "no way to read them here".to_string(),
            (Some(_), Some(problem)) => problem.clone(),
            (Some(_), None) => format!("{found} found"),
        };
        vec![("Spout senders".to_string(), text)]
    }

    fn get_config(&self) -> Value {
        let fps = self.shared.fps.lock().unwrap();
        let mut keys: Vec<&String> = fps.keys().collect();
        keys.sort();
        json!({"senders": keys.into_iter().map(|k| (k.clone(), json!({"fps": fps[k]}))).collect::<Map<String, Value>>()})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let mut fps = HashMap::new();
        if let Some(senders) = cfg.get("senders").and_then(Value::as_object) {
            for (key, entry) in senders {
                if let Some(f) = entry.get("fps").filter(|v| v.is_u64()).and_then(Value::as_u64).filter(|f| (1..=240).contains(f)) {
                    fps.insert(key.clone(), f as u32);
                }
            }
        }
        *self.shared.fps.lock().unwrap() = fps;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
