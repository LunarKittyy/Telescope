//! Browser camera (plugins/browser_camera.py): stream from any device with a browser, no Telescope app on it.
//!
//! "Browser camera" in the phone picker runs the server and shows this card with the code to scan. Each browser that
//! connects is a StreamSource of its own, listed in the picker by its device while it's there, and starts streaming by
//! itself the way opening the app on a phone would: the first as the stream, the next ones next to it, each to a
//! virtual camera of its own. The server stops once Browser camera isn't picked and no browser streams.
//!
//! A browser's settings are kept like a phone's, but only once something was changed from the defaults (the host
//! drops the rest), so scanning once from someone's phone leaves nothing behind. Remembered browsers are listed with
//! the phones, where they can be removed, and are forgotten after REMEMBER_DAYS without connecting.
//!
//! The sources run on the host's thread without the plugin to hand, so what they need to know (whether Start waits,
//! which browser is idle, the rate) is shared, and what they ask for (prepare, cancel) is posted to the plugin.

use telescope_core::i18n::Fields;
use super::feed::{BrowserFeed, BrowserHub, BrowserVideo, Sent};
use super::rtc::{RtcMaker, RtcSession, StrSession};
use super::server::{BrowserServer, BROWSER_PORT};
use serde_json::{json, Map, Value};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use telescope_core::audio::{AudioFeed, SourceAudio};
use telescope_core::event::{Event, RememberedSource};
use telescope_core::host::{Host, OutputChange, Poster};
use telescope_core::i18n::{n_, tr};
use telescope_core::media::{Codec, Media, VideoFeed};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::source::{ControlClient, StreamSource};
use telescope_core::ui::{CardSpec, Issue, IssueAction, IssueDoes, SlintSource, UiEvent, UiSpec};
use telescope_core::web::{ServerStats, Step, Web};

pub const NAME: &str = "browser_camera";
pub const SOURCE_ID: &str = "browser";
/// Then the id the browser's page keeps.
pub const BROWSER_PREFIX: &str = "browser:";
pub const REMEMBER_DAYS: f64 = 60.0;
pub const SIZES: [(&str, u32, u32); 3] = [("480p", 854, 480), ("720p", 1280, 720), ("1080p", 1920, 1080)];
pub const FPS_CHOICES: [u32; 3] = [15, 24, 30];
const DEFAULT_SIZE: &str = "720p";
const DEFAULT_FPS: u32 = 30;
/// How many cameras streams can go to at once (vcam.MAX_SLOTS).
const MAX_SLOTS: i64 = 4;
/// Waiting this long with nothing reaching the server is worth a hint: scanning and loading the page take seconds.
pub const HINT_AFTER: Duration = Duration::from_secs(30);
const CPU_LIMIT: &str = n_("The device's processor can't keep up, so the browser is sending less. Try a lower resolution or frame rate.");
const BANDWIDTH_LIMIT: &str = n_("The browser is lowering the picture to fit the connection or the bitrate for this resolution.");
// Translators: Show Details, visit this website, Advanced and Proceed are what Safari and Chrome call their buttons:
// use the names those browsers show in your language
const CERT_NOTE: &str = n_("The browser warns that the connection isn't private: it's this computer's own certificate. On iPhone tap Show Details, then visit this website. In Chrome tap Advanced, then Proceed.");
const BACKGROUND_NOTE: &str = n_("Keep the page open with the screen on: phones pause the camera when you leave the browser.");
const POST_CHANGED: &str = "changed";
const POST_PREPARE: &str = "prepare";
const POST_CANCEL: &str = "cancel";
const TIMER_HINT: &str = "hint";

/// What the stream says while it waits for a browser (BrowserReader.waiting_text).
pub fn waiting_text() -> String {
    tr("Waiting for the browser. Scan the code on the Browser camera card and tap Start.")
}

pub fn is_browser(source_id: &str) -> bool {
    source_id == SOURCE_ID || source_id.starts_with(BROWSER_PREFIX)
}

fn codec_name(codec: &str) -> Option<&'static str> {
    match codec {
        "H264" => Some("H.264"),
        "VP8" => Some("VP8"),
        _ => None,
    }
}

/// ", H.264, 1280×720 at 30 fps" from what WebRTC settled on and the page's stats; "" before either is known.
pub fn sending_text(codec: &str, sent: &Sent) -> String {
    let mut parts: Vec<String> = codec_name(codec).map(String::from).into_iter().collect();
    if let (Some(w), Some(h)) = (sent.width.filter(|w| *w > 0), sent.height.filter(|h| *h > 0)) {
        let size = format!("{w}×{h}");
        parts.push(match sent.fps.filter(|f| *f > 0) {
            Some(fps) => tr("{size} at {fps} fps").field("size", &size).field("fps", &fps.to_string()),
            None => size,
        });
    }
    parts.iter().map(|p| format!(", {p}")).collect()
}

/// What to check when no browser has connected yet, from how far browsers got; "" when there's nothing to say.
pub fn waiting_hint(stats: Option<&ServerStats>, connected: bool, now: Instant) -> String {
    let Some(stats) = stats.filter(|_| !connected) else { return String::new() };
    if stats.count(Step::Browsers) > 0 || stats.count(Step::Pages) > 0 {
        return String::new(); // one got through before, or is on the page now
    }
    if stats.count(Step::TlsFailed) > 0 {
        return tr("A browser reached this computer but hasn't accepted its certificate yet. Tap through the warning.");
    }
    if stats.nothing_arrived() && now.saturating_duration_since(stats.started) >= HINT_AFTER {
        return tr("Nothing has reached this computer yet. Check that the device is on the same network as the address in the \
                   code, and that the firewall allows port {port}.").field("port", &stats.port.to_string());
    }
    String::new()
}

fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// A date as the phones list says when a browser last connected.
fn ymd(unix: f64) -> String {
    let days = (unix / 86400.0).floor() as i64;
    // Howard Hinnant's civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

// ── What the plugin needs of the window ──────────────────────────────────────

/// The browsers' mics, for the Microphone plugin.
pub struct BrowserMics {
    hub: Arc<BrowserHub>,
}

impl SourceAudio for BrowserMics {
    fn open(&self, source_id: &str) -> Option<Result<Box<dyn AudioFeed>, String>> {
        let bid = source_id.strip_prefix(BROWSER_PREFIX)?;
        let Some(feed) = self.hub.feed(bid) else { return Some(Err(tr("The browser has gone."))) };
        Some(feed.open_audio().map(|mic| Box::new(mic) as Box<dyn AudioFeed>))
    }
    fn has(&self, source_id: &str) -> bool {
        source_id.starts_with(BROWSER_PREFIX)
    }
    fn key(&self, _source_id: &str) -> String {
        "browser:/audio".into()
    }
}

/// What the plugin asks of the window, so tests can stand in one that keeps its own list of streams.
pub trait BrowserHost {
    fn add_stream_source(&mut self, source: Arc<dyn StreamSource>);
    fn remove_stream_source(&mut self, source_id: &str);
    fn stream_source(&mut self, source_id: &str);
    fn stop_stream(&mut self, source_id: &str);
    fn stream_output(&self, source_id: &str) -> String;
    fn stream_count(&self) -> u32;
    fn focused_source_id(&self) -> Option<String>;
    fn has_device_settings(&self, source_id: &str) -> bool;
    fn forget_device_settings(&mut self, source_id: &str);
    fn show_issue(&mut self, key: &str, issue: Issue);
    fn clear_issue(&mut self, key: &str);
    fn is_streaming_from(&self, source_id: &str) -> bool;
    fn is_starting(&self) -> bool;
    fn set_fps(&mut self, source_id: &str, fps: u32);
    fn schedule_save(&mut self);
    fn set_card(&mut self, props: &Value);
    fn emit(&mut self, event: Event);
    fn schedule(&mut self, after_ms: u64, token: &str);
    fn copy_text(&mut self, text: &str);
}

/// The real window.
pub struct Hosted<'a>(pub &'a mut dyn Host);

impl BrowserHost for Hosted<'_> {
    fn add_stream_source(&mut self, source: Arc<dyn StreamSource>) {
        self.0.add_stream_source(source)
    }
    fn remove_stream_source(&mut self, source_id: &str) {
        self.0.remove_stream_source(source_id)
    }
    fn stream_source(&mut self, source_id: &str) {
        self.0.stream_source(source_id)
    }
    fn stop_stream(&mut self, source_id: &str) {
        self.0.stop_stream(Some(source_id))
    }
    fn stream_output(&self, source_id: &str) -> String {
        self.0.stream_output(source_id)
    }
    fn stream_count(&self) -> u32 {
        self.0.stream_count()
    }
    fn focused_source_id(&self) -> Option<String> {
        self.0.focused_source_id()
    }
    fn has_device_settings(&self, source_id: &str) -> bool {
        self.0.has_device_settings(source_id)
    }
    fn forget_device_settings(&mut self, source_id: &str) {
        self.0.forget_device_settings(source_id)
    }
    fn show_issue(&mut self, key: &str, issue: Issue) {
        self.0.show_issue(key, issue)
    }
    fn clear_issue(&mut self, key: &str) {
        self.0.clear_issue(Some(key), None)
    }
    fn is_streaming_from(&self, source_id: &str) -> bool {
        self.0.is_streaming_from(Some(source_id))
    }
    fn is_starting(&self) -> bool {
        self.0.is_starting()
    }
    fn set_fps(&mut self, source_id: &str, fps: u32) {
        self.0.update_stream_output(OutputChange { fps: Some(Some(fps)), ..OutputChange::default() }, Some(source_id))
    }
    fn schedule_save(&mut self) {
        self.0.schedule_save()
    }
    fn set_card(&mut self, props: &Value) {
        self.0.set_ui_props("card", props)
    }
    fn emit(&mut self, event: Event) {
        self.0.emit(event)
    }
    fn schedule(&mut self, after_ms: u64, token: &str) {
        self.0.schedule(after_ms, token)
    }
    fn copy_text(&mut self, text: &str) {
        self.0.copy_text(text)
    }
}

// ── The server, however it's made ────────────────────────────────────────────

/// What the plugin does with its server (BrowserServer; tests stand one in).
pub trait Serving: Send {
    fn start(&self) -> Result<(), String>;
    fn stop(&self);
    fn url_for(&self, host: &str) -> String;
    fn new_token(&self);
    fn stats(&self) -> Option<Arc<ServerStats>>;
}

impl Serving for BrowserServer {
    fn start(&self) -> Result<(), String> {
        BrowserServer::start(self)
    }
    fn stop(&self) {
        BrowserServer::stop(self)
    }
    fn url_for(&self, host: &str) -> String {
        BrowserServer::url_for(self, host)
    }
    fn new_token(&self) {
        BrowserServer::new_token(self)
    }
    fn stats(&self) -> Option<Arc<ServerStats>> {
        BrowserServer::stats(self)
    }
}

/// Makes the server for a hub, telling on_change (from its threads) whenever a browser connects, leaves or says something.
pub type ServerMaker = Box<dyn Fn(Arc<BrowserHub>, Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn Serving>, String> + Send>;

/// The real server: this computer's certificate in `folder`, WebRTC on str0m, the mic decoded by media.
pub fn system_server(web: Arc<dyn Web>, folder: PathBuf, media: Arc<dyn Media>) -> ServerMaker {
    Box::new(move |hub, on_change| {
        let hosts = web.addresses();
        let certificate = web.certificate(&folder, &hosts)?;
        let addresses: Vec<IpAddr> = hosts.iter().filter_map(|h| h.parse().ok()).collect();
        let media = media.clone();
        let opus: Arc<dyn Fn() -> Option<Box<dyn telescope_core::media::AudioDecoder>> + Send + Sync> =
            Arc::new(move || media.opus_decoder().map_err(|e| log::warn!("Browser camera: no mic: {e}")).ok());
        let rtc: RtcMaker = Arc::new(move |video, audio| {
            Box::new(StrSession::new(addresses.clone(), opus.clone(), video, audio)) as Box<dyn RtcSession>
        });
        let server = BrowserServer::new(hub, web.clone(), certificate, move || on_change(), BROWSER_PORT, "", rtc);
        Ok(Box::new(server) as Box<dyn Serving>)
    })
}

// ── The sources ──────────────────────────────────────────────────────────────

/// What the sources and the server's threads share with the plugin.
#[derive(Default)]
struct Shared {
    /// Start or + asked for a browser and none has joined yet: the server stays up for it.
    waiting: AtomicBool,
    /// A connected browser that doesn't stream, for Start with Browser camera picked.
    idle: Mutex<Option<String>>,
    server_up: AtomicBool,
    fps: AtomicU32,
    poster: Mutex<Option<Arc<dyn Poster>>>,
}

impl Shared {
    fn post(&self, token: &str, data: Value) {
        if let Some(poster) = self.poster.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            poster.post(token, data);
        }
    }
}

/// Stands in for the phone's control client while a browser streams: there's no phone to control or ask for state.
struct NoControl;

impl ControlClient for NoControl {
    fn send(&self, _body: &Value) -> Result<(), String> {
        Ok(())
    }
    fn state(&self) -> Result<Value, String> {
        Err("a browser has no state to ask for".into())
    }
    fn has_audio(&self) -> bool {
        true
    }
}

/// Browser camera in the picker: Start streams a browser that's there, or waits for one to connect.
struct Launcher {
    shared: Arc<Shared>,
}

impl StreamSource for Launcher {
    fn id(&self) -> String {
        SOURCE_ID.into()
    }
    fn name(&self) -> String {
        tr("Browser camera")
    }
    fn url(&self) -> String {
        "browser:".into()
    }
    fn prepare(&self, interactive: bool) -> bool {
        // Never streams itself: says to scan the code instead, and the first browser to connect starts
        self.shared.waiting.store(true, Ordering::SeqCst);
        self.shared.post(POST_PREPARE, json!({"interactive": interactive}));
        false
    }
    fn open_feed(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        Err(waiting_text())
    }
    fn control_client(&self) -> Option<Box<dyn ControlClient>> {
        Some(Box::new(NoControl))
    }
    fn fps(&self) -> u32 {
        self.shared.fps.load(Ordering::SeqCst)
    }
    fn redirect(&self) -> Option<String> {
        self.shared.idle.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn remember_changes_only(&self) -> bool {
        true
    }
    fn waiting(&self) -> bool {
        self.shared.waiting.load(Ordering::SeqCst)
    }
    fn cancel_wait(&self) {
        self.shared.waiting.store(false, Ordering::SeqCst);
        self.shared.post(POST_CANCEL, json!({}));
    }
    fn waiting_text(&self) -> String {
        waiting_text()
    }
}

/// One connected browser.
pub struct BrowserSource {
    shared: Arc<Shared>,
    pub bid: String,
    pub feed: Arc<BrowserFeed>,
    pub name: String,
}

impl BrowserSource {
    fn source_id(&self) -> String {
        format!("{BROWSER_PREFIX}{}", self.bid)
    }
}

impl StreamSource for BrowserSource {
    fn id(&self) -> String {
        self.source_id()
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn url(&self) -> String {
        "browser:".into()
    }
    fn prepare(&self, _interactive: bool) -> bool {
        self.shared.server_up.load(Ordering::SeqCst)
    }
    fn open_feed(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        let (codec, video) = BrowserVideo::open(self.feed.clone()).ok_or_else(waiting_text)?;
        Ok((codec, Box::new(video)))
    }
    fn control_client(&self) -> Option<Box<dyn ControlClient>> {
        Some(Box::new(NoControl))
    }
    fn fps(&self) -> u32 {
        self.shared.fps.load(Ordering::SeqCst)
    }
    fn remember_changes_only(&self) -> bool {
        true
    }
    fn waiting_text(&self) -> String {
        waiting_text()
    }
}

/// A browser the plugin knows: its source (made again when its name changes) and what it runs on.
struct Known {
    source: Arc<BrowserSource>,
    /// What the page said it runs on, without the number a second one of its kind gets.
    device: String,
}

/// A browser that connected, for the phones list.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub name: String,
    pub seen: f64,
}

// ── The plugin ───────────────────────────────────────────────────────────────

pub struct BrowserCameraPlugin {
    make_server: ServerMaker,
    /// Seconds since 1970, for when a browser was last seen.
    clock: Box<dyn Fn() -> f64 + Send>,
    addresses: Box<dyn Fn() -> Vec<String> + Send>,
    pub hub: Arc<BrowserHub>,
    shared: Arc<Shared>,
    /// Browsers that may still join: the host thread sets it, the server's reads and takes from it.
    room: Arc<AtomicI64>,
    /// By the id the page keeps, in the order they came, while there or streaming.
    browsers: Vec<(String, Known)>,
    /// The connection that started streaming, by id, so a stop isn't undone right away.
    started: Vec<(String, u64)>,
    joining: bool,
    /// Streams changed during a join pass.
    join_again: bool,
    /// New link is cutting the browsers off.
    cutting: bool,
    /// The browser starting in the waiting tile's place, until it streams.
    handover: String,
    pub known: Vec<(String, Seen)>,
    server: Option<Box<dyn Serving>>,
    /// Why the server isn't running.
    problem: String,
    /// The picked source, when it's Browser camera or a browser.
    selected_id: String,
    size: String,
    fps: u32,
    /// The address picked for the code, if there's more than one.
    address: String,
    candidates: Vec<String>,
}

impl BrowserCameraPlugin {
    pub fn new(make_server: ServerMaker, addresses: impl Fn() -> Vec<String> + Send + 'static) -> BrowserCameraPlugin {
        let room = Arc::new(AtomicI64::new(MAX_SLOTS));
        let taken = room.clone();
        let hub = Arc::new(BrowserHub::new(move |_| {
            // Server thread: whether a browser that isn't known yet gets a feed (there's a camera left for it)
            taken.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |r| (r > 0).then_some(r - 1)).is_ok()
        }));
        let shared = Arc::new(Shared { fps: AtomicU32::new(DEFAULT_FPS), ..Shared::default() });
        BrowserCameraPlugin {
            make_server, clock: Box::new(unix_now), addresses: Box::new(addresses), hub, shared, room, browsers: Vec::new(), started: Vec::new(),
            joining: false, join_again: false, cutting: false, handover: String::new(), known: Vec::new(), server: None,
            problem: String::new(), selected_id: String::new(), size: DEFAULT_SIZE.into(), fps: DEFAULT_FPS,
            address: String::new(), candidates: Vec::new(),
        }
    }

    /// Its browsers' mics, however many join: hand them to the Microphone plugin.
    pub fn mics(&self) -> BrowserMics {
        BrowserMics { hub: self.hub.clone() }
    }

    /// Picker entry and the source the host starts with Browser camera picked.
    pub fn launcher(&self) -> Arc<dyn StreamSource> {
        Arc::new(Launcher { shared: self.shared.clone() })
    }

    pub fn setup_on(&mut self, h: &mut dyn BrowserHost, poster: Option<Arc<dyn Poster>>) {
        *self.shared.poster.lock().unwrap_or_else(|e| e.into_inner()) = poster;
        h.add_stream_source(self.launcher());
    }

    fn selected(&self) -> bool {
        !self.selected_id.is_empty()
    }

    fn waiting(&self) -> bool {
        self.shared.waiting.load(Ordering::SeqCst)
    }

    /// Whether the server runs even with no browser streaming.
    fn needed(&self) -> bool {
        self.selected() || self.waiting()
    }

    fn browser(&self, bid: &str) -> Option<&Known> {
        self.browsers.iter().find(|(id, _)| id == bid).map(|(_, k)| k)
    }

    /// The feed of the browser the panels show, if they show one.
    fn shown_feed(&self) -> Option<Arc<BrowserFeed>> {
        let bid = self.selected_id.strip_prefix(BROWSER_PREFIX)?;
        self.browser(bid).map(|k| k.source.feed.clone())
    }

    pub fn connected(&self) -> Vec<Arc<BrowserSource>> {
        self.browsers.iter().filter(|(_, k)| k.source.feed.connected()).map(|(_, k)| k.source.clone()).collect()
    }

    fn started_gen(&self, bid: &str) -> Option<u64> {
        self.started.iter().find(|(id, _)| id == bid).map(|(_, g)| *g)
    }

    fn set_started(&mut self, bid: &str, gen: u64) {
        self.started.retain(|(id, _)| id != bid);
        self.started.push((bid.into(), gen));
    }

    fn any_streaming(&self, h: &dyn BrowserHost) -> bool {
        self.browsers.iter().any(|(_, k)| h.is_streaming_from(&k.source.source_id()))
    }

    fn url(&self) -> String {
        match &self.server {
            Some(server) if !self.address.is_empty() => server.url_for(&self.address),
            _ => String::new(),
        }
    }

    // ── The card ──

    pub fn props(&self, h: &dyn BrowserHost) -> Value {
        let server = self.server.as_ref();
        let feed = self.shown_feed();
        let connected = if server.is_some() { self.connected() } else { Vec::new() };
        let mut tip = String::new();
        let (kind, text) = if server.is_none() {
            if self.problem.is_empty() { ("status_dim", tr("Off")) } else { ("status_err", self.problem.clone()) }
        } else if let Some(feed) = feed.as_ref().filter(|f| f.connected()) {
            let device = feed.device();
            let shown = self.connected().iter().find(|s| Arc::ptr_eq(&s.feed, feed)).map(|s| s.name.clone()).unwrap_or(device.clone());
            let rtc_error = feed.rtc_error();
            if !rtc_error.is_empty() {
                ("status_err", rtc_error)
            } else {
                let sent = feed.sent();
                let text = if device.is_empty() { tr("Connected") } else { shown } + &sending_text(&feed.codec(), &sent);
                tip = match sent.limit.as_str() {
                    "cpu" => tr(CPU_LIMIT),
                    "bandwidth" => tr(BANDWIDTH_LIMIT),
                    _ => String::new(),
                };
                (if sent.limit == "cpu" { "status_warn" } else { "status_ok" }, text)
            }
        } else if connected.len() > 1 {
            ("status_ok", tr("{n} connected").field("n", &connected.len().to_string()))
        } else if let Some(only) = connected.first() {
            ("status_ok", only.name.clone())
        } else {
            ("status_dim", tr("Waiting for a browser"))
        };
        let lines: Vec<String> = connected.iter().map(|s| {
            let out = h.stream_output(&s.source_id());
            let value = if out.is_empty() { tr("not streaming") } else { out };
            tr("{label}: {value}").field("label", &s.name).field("value", &value)
        }).collect();
        let mic = match (&server, &feed) {
            (Some(_), Some(feed)) if feed.connected() => feed.mic_error(),
            _ => String::new(),
        };
        let stats = server.and_then(|s| s.stats());
        let hint = waiting_hint(stats.as_deref(), !connected.is_empty(), Instant::now());
        let url = self.url();
        let (cells, size) = if url.is_empty() { (Vec::new(), 0) } else { telescope_core::qr::encode(&url) };
        json!({
            "shown": self.selected(),
            "status-text": text,
            "status-kind": kind,
            "status-tip": tip,
            "list-text": lines.join("\n"),
            "show-list": lines.len() > 1,
            "mic-text": if mic.is_empty() { String::new() } else { tr("No microphone: {problem}").field("problem", &mic) },
            "hint-text": hint,
            "url": url,
            "qr-cells": cells,
            "qr-size": size,
            "addresses": self.candidates,
            "address-index": self.candidates.iter().position(|a| *a == self.address).map_or(-1, |i| i as i64),
            "show-addresses": server.is_some() && self.candidates.len() > 1,
            "size-index": SIZES.iter().position(|s| s.0 == self.size).unwrap_or(1),
            "fps-index": FPS_CHOICES.iter().position(|f| *f == self.fps).unwrap_or(2),
            "note-text": format!("{} {}", tr(CERT_NOTE), tr(BACKGROUND_NOTE)),
            "new-link-enabled": server.is_some(),
        })
    }

    fn render(&self, h: &mut dyn BrowserHost) {
        let props = self.props(h);
        h.set_card(&props);
    }

    fn fill_addresses(&mut self) {
        self.candidates = (self.addresses)();
        if !self.candidates.contains(&self.address) {
            self.address = self.candidates.first().cloned().unwrap_or_default();
        }
    }

    pub fn on_address_picked(&mut self, h: &mut dyn BrowserHost, i: usize) {
        let Some(ip) = self.candidates.get(i).cloned() else { return };
        if ip != self.address {
            self.address = ip;
            h.schedule_save();
            self.render(h);
        }
    }

    // ── The server ──

    pub fn on_stream_behind(&mut self, h: &mut dyn BrowserHost, sid: &str, behind: bool) {
        if !self.selected() {
            return;
        }
        if !behind {
            h.clear_issue("behind"); // whichever stream: only one the panels showed can have been behind
            return;
        }
        if sid != h.focused_source_id().unwrap_or_default() {
            return;
        }
        let text = if self.size != SIZES[0].0 || self.fps != FPS_CHOICES[0] {
            tr("Try a lower resolution or frame rate on the Browser camera card.")
        } else {
            tr("The device's browser can't send any faster.")
        };
        h.show_issue("behind", Issue { title: tr("Can't keep up"), text, kind: "warn".into(), ..issue() });
    }

    pub fn on_source_selected(&mut self, h: &mut dyn BrowserHost, sid: &str) {
        let sid = if is_browser(sid) { sid.to_string() } else { String::new() };
        if sid == self.selected_id {
            return;
        }
        let was = self.selected();
        self.selected_id = sid;
        if self.selected() {
            self.start_server(h);
        } else if was && !self.waiting() && !self.any_streaming(h) {
            // one still streaming keeps going while the panels move on
            self.stop_server(h);
        }
        self.render(h);
    }

    pub fn on_streams_changed(&mut self, h: &mut dyn BrowserHost) {
        for (_, k) in &self.browsers {
            k.source.feed.set_camera(&h.stream_output(&k.source.source_id()));
        }
        self.update_room(h);
        if !self.needed() && !self.any_streaming(h) {
            self.stop_server(h);
        } else {
            self.tidy_browsers(h);
            self.end_wait_if_joined(h);
            self.join_waiting(h);
        }
        self.note_remembered(h);
        self.render(h);
    }

    fn update_room(&mut self, h: &dyn BrowserHost) {
        // From the hub, so one let in since the last count holds its camera before it's offered as a source
        let waiting = self.hub.feeds().iter().filter(|(bid, feed)| {
            feed.connected() && !h.is_streaming_from(&format!("{BROWSER_PREFIX}{bid}"))
                && self.started_gen(bid) != Some(feed.generation()) // stopped here: it doesn't come back by itself
        }).count() as i64;
        let starting = i64::from(h.is_starting()); // a start under way has a camera coming too
        self.room.store(MAX_SLOTS - i64::from(h.stream_count()) - starting - waiting, Ordering::SeqCst);
    }

    /// A connected browser that doesn't stream, for Start with Browser camera picked.
    pub fn idle_browser(&self, h: &dyn BrowserHost) -> Option<String> {
        self.connected().iter().map(|s| s.source_id()).find(|sid| !h.is_streaming_from(sid))
    }

    /// Offer each connected browser as a source (by its device, once it says), and take back the ones that left and
    /// don't stream.
    fn tidy_browsers(&mut self, h: &mut dyn BrowserHost) {
        // device name: how many browsers have it, so a second "Chrome on Linux" is "Chrome on Linux (2)"
        let mut taken: Vec<(String, u32)> = Vec::new();
        for (bid, feed) in self.hub.feeds() {
            let known = self.browsers.iter().position(|(id, _)| *id == bid);
            if !feed.connected() && known.is_none() {
                continue;
            }
            // kept across a reconnect
            let device = Some(feed.device()).filter(|d| !d.is_empty())
                .or_else(|| known.map(|i| self.browsers[i].1.device.clone()).filter(|d| !d.is_empty()))
                .unwrap_or_else(|| tr("Browser"));
            let n = match taken.iter_mut().find(|(d, _)| *d == device) {
                Some((_, n)) => {
                    *n += 1;
                    *n
                }
                None => {
                    taken.push((device.clone(), 1));
                    1
                }
            };
            if feed.connected() {
                let name = if n == 1 { device.clone() } else { format!("{device} ({n})") };
                match known {
                    Some(i) if self.browsers[i].1.source.name == name => self.browsers[i].1.device = device.clone(),
                    _ => {
                        let source = Arc::new(BrowserSource { shared: self.shared.clone(), bid: bid.clone(), feed: feed.clone(), name });
                        h.add_stream_source(source.clone());
                        let k = Known { source, device: device.clone() };
                        match known {
                            Some(i) => self.browsers[i].1 = k,
                            None => self.browsers.push((bid.clone(), k)),
                        }
                    }
                }
                self.note_seen(h, &bid, &device);
            } else if let Some(i) = known {
                if !h.is_streaming_from(&self.browsers[i].1.source.source_id()) {
                    self.drop_browser(h, &bid);
                }
            }
        }
        *self.shared.idle.lock().unwrap_or_else(|e| e.into_inner()) = self.idle_browser(h);
    }

    fn drop_browser(&mut self, h: &mut dyn BrowserHost, bid: &str) {
        let known = self.browsers.iter().position(|(id, _)| id == bid).map(|i| self.browsers.remove(i));
        self.started.retain(|(id, _)| id != bid);
        self.hub.drop_feed(bid);
        if let Some((_, k)) = known {
            h.remove_stream_source(&k.source.source_id());
        }
    }

    /// A browser that just connected starts streaming, the way opening the app on a phone would.
    fn join_waiting(&mut self, h: &mut dyn BrowserHost) {
        if self.cutting {
            return;
        }
        if self.joining {
            self.join_again = true; // a start just went through: the next browser waiting goes once this pass ends
            return;
        }
        if h.is_starting() {
            return;
        }
        self.joining = true;
        self.join_again = false;
        let list: Vec<(String, Arc<BrowserSource>)> = self.browsers.iter().map(|(id, k)| (id.clone(), k.source.clone())).collect();
        for (bid, source) in list {
            let feed = &source.feed;
            if !feed.connected() || self.started_gen(&bid) == Some(feed.generation()) {
                continue;
            }
            let sid = source.source_id();
            if h.is_streaming_from(&sid) {
                self.set_started(&bid, feed.generation());
                continue;
            }
            if i64::from(h.stream_count()) >= MAX_SLOTS {
                break;
            }
            self.set_started(&bid, feed.generation());
            h.clear_issue("start");
            // The wait ends once it streams: ended first, the waiting tile would go and take the panels (and with
            // them the server, and this browser) back to a phone before it starts
            if self.waiting() {
                self.handover = bid.clone();
            }
            h.stream_source(&sid);
            self.end_wait_if_joined(h);
            break; // one at a time: the next once this one is through (streams_changed)
        }
        self.joining = false;
        if self.join_again {
            self.join_waiting(h);
        }
    }

    /// The browser that came for the wait streams now (or it left before it could).
    fn end_wait_if_joined(&mut self, h: &mut dyn BrowserHost) {
        if self.handover.is_empty() {
            return;
        }
        let source = self.browser(&self.handover).map(|k| k.source.source_id());
        if source.as_ref().is_none_or(|sid| h.is_streaming_from(sid)) {
            self.handover.clear();
            if source.is_some() {
                self.set_waiting(h, false);
            }
        }
    }

    fn start_server(&mut self, h: &mut dyn BrowserHost) -> bool {
        if self.server.is_some() {
            return true;
        }
        self.problem.clear();
        self.fill_addresses();
        self.update_room(h);
        let shared = self.shared.clone();
        let on_change: Arc<dyn Fn() + Send + Sync> = Arc::new(move || shared.post(POST_CHANGED, json!({})));
        let server = match (self.make_server)(self.hub.clone(), on_change) {
            Ok(server) => server,
            Err(e) => {
                log::warn!("Browser camera server didn't start: {e}");
                self.problem = tr("Couldn't start: {error}").field("error", &e);
                return false;
            }
        };
        if let Err(e) = server.start() {
            log::warn!("Browser camera server didn't start: {e}");
            self.problem = tr("Couldn't start: {error}").field("error", &e);
            return false;
        }
        self.server = Some(server);
        self.shared.server_up.store(true, Ordering::SeqCst);
        self.apply_capture();
        h.schedule(HINT_AFTER.as_millis() as u64, TIMER_HINT);
        true
    }

    fn stop_server(&mut self, h: &mut dyn BrowserHost) {
        if let Some(server) = self.server.take() {
            server.stop();
        }
        self.shared.server_up.store(false, Ordering::SeqCst);
        let ids: Vec<String> = self.browsers.iter().map(|(id, _)| id.clone()).collect();
        for bid in ids {
            let streaming = self.browser(&bid).is_some_and(|k| h.is_streaming_from(&k.source.source_id()));
            if !streaming {
                self.drop_browser(h, &bid);
            }
        }
        for (bid, _) in self.hub.feeds() {
            self.hub.drop_feed(&bid);
        }
        *self.shared.idle.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The old code stops working and every browser on it is cut off: their streams end (they wouldn't come back).
    /// The same with its own idea of the time, for what it forgets after a while.
    pub fn with_clock(mut self, clock: impl Fn() -> f64 + Send + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    pub fn new_link(&mut self, h: &mut dyn BrowserHost) {
        let Some(server) = &self.server else { return };
        self.cutting = true; // a browser still connected must not start in the place of one that was stopped
        server.new_token();
        for (_, k) in &self.browsers {
            k.source.feed.disconnect(k.source.feed.generation());
        }
        let ids: Vec<String> = self.browsers.iter().map(|(_, k)| k.source.source_id()).collect();
        for sid in ids {
            h.stop_stream(&sid);
        }
        self.tidy_browsers(h);
        self.cutting = false;
        self.update_room(h);
        self.render(h);
    }

    /// At Start: the server has to be up, or a browser has nowhere to connect. With Browser camera picked and no
    /// browser there yet, nothing starts: the first browser to connect does. The launcher set waiting already.
    pub fn prepare(&mut self, h: &mut dyn BrowserHost) -> bool {
        if !self.start_server(h) {
            self.shared.waiting.store(false, Ordering::SeqCst);
            self.render(h);
            let again = IssueAction { label: tr("Try again"), does: IssueDoes::StartAgain { source_id: None, before: None } };
            h.show_issue("start", Issue { title: tr("Browser camera isn't available"), text: self.problem.clone(), actions: vec![again],
                                          ..issue() });
            return false;
        }
        self.set_waiting(h, true);
        if h.stream_count() == 0 {
            // next to other streams, its tile says it's waiting
            let cancel = IssueAction { label: tr("Cancel"), does: IssueDoes::Plugin { id: POST_CANCEL.into() } };
            h.show_issue("start", Issue {
                title: tr("Waiting for a browser"),
                text: tr("Scan the code on the Browser camera card with the device to use. It starts streaming as soon as it connects."),
                actions: vec![cancel], kind: "warn".into(), tell_dismiss: true, ..issue()
            });
        }
        false
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting()
    }

    fn set_waiting(&mut self, h: &mut dyn BrowserHost, waiting: bool) {
        if self.shared.waiting.swap(waiting, Ordering::SeqCst) != waiting {
            if !waiting {
                h.stop_stream(SOURCE_ID); // its waiting tile goes
            }
            self.render(h);
        }
    }

    /// The banner's Cancel or the tile's X: nobody's coming, so the card goes unless Browser camera is picked.
    pub fn cancel_waiting(&mut self, h: &mut dyn BrowserHost) {
        h.clear_issue("start");
        self.set_waiting(h, false);
        if !self.needed() && !self.any_streaming(h) {
            self.stop_server(h);
        }
        self.render(h);
    }

    pub fn on_server_changed(&mut self, h: &mut dyn BrowserHost) {
        if self.server.is_none() {
            return;
        }
        self.update_room(h);
        self.tidy_browsers(h);
        self.join_waiting(h);
        self.render(h);
    }

    // ── Capture settings ──

    pub fn set_capture(&mut self, h: &mut dyn BrowserHost, size: Option<&str>, fps: Option<u32>) {
        if let Some(size) = size {
            self.size = size.into();
        }
        if let Some(fps) = fps.filter(|f| *f != self.fps) {
            self.fps = fps;
            self.shared.fps.store(fps, Ordering::SeqCst);
            // every page gets the new rate, so every stream of one does too
            let ids: Vec<String> = self.browsers.iter().map(|(_, k)| k.source.source_id()).collect();
            for sid in ids {
                if h.is_streaming_from(&sid) {
                    h.set_fps(&sid, fps);
                }
            }
        }
        self.apply_capture();
        h.schedule_save();
    }

    fn apply_capture(&self) {
        let (_, w, h) = SIZES.iter().find(|s| s.0 == self.size).copied().unwrap_or(SIZES[1]);
        self.hub.set_capture(w, h, self.fps);
    }

    // ── Remembered browsers ──

    fn note_seen(&mut self, h: &mut dyn BrowserHost, bid: &str, name: &str) {
        let now = (self.clock)();
        let entry = self.known.iter_mut().find(|(id, _)| id == bid);
        match entry {
            Some((_, seen)) if seen.name == name && now - seen.seen <= 3600.0 => {}
            Some((_, seen)) => {
                *seen = Seen { name: name.into(), seen: now };
                h.schedule_save();
            }
            None => {
                self.known.push((bid.into(), Seen { name: name.into(), seen: now }));
                h.schedule_save();
            }
        }
    }

    /// Forget browsers that haven't connected for REMEMBER_DAYS, and ones that never had a setting changed.
    pub fn tidy_known(&mut self, h: &mut dyn BrowserHost) {
        let cutoff = (self.clock)() - REMEMBER_DAYS * 86400.0;
        let entries = std::mem::take(&mut self.known);
        for (bid, seen) in entries {
            let sid = format!("{BROWSER_PREFIX}{bid}");
            if self.browser(&bid).is_some() {
                self.known.push((bid, seen));
                continue;
            }
            if seen.seen < cutoff {
                h.forget_device_settings(&sid);
            } else if h.has_device_settings(&sid) {
                self.known.push((bid, seen));
            }
        }
        h.schedule_save();
        self.note_remembered(h);
    }

    /// Browsers with settings of their own, last seen first.
    pub fn remembered(&self, h: &dyn BrowserHost) -> Vec<RememberedSource> {
        let mut list: Vec<&(String, Seen)> = self.known.iter().collect();
        list.sort_by(|a, b| b.1.seen.total_cmp(&a.1.seen));
        list.into_iter().filter_map(|(bid, seen)| {
            let sid = format!("{BROWSER_PREFIX}{bid}");
            h.has_device_settings(&sid).then(|| RememberedSource {
                id: sid, name: seen.name.clone(), detail: tr("Browser, last connected {date}").field("date", &ymd(seen.seen)),
            })
        }).collect()
    }

    fn note_remembered(&self, h: &mut dyn BrowserHost) {
        let sources = self.remembered(h);
        h.emit(Event::RememberedSources { sources });
    }

    pub fn forget_browser(&mut self, h: &mut dyn BrowserHost, sid: &str) {
        let Some(bid) = sid.strip_prefix(BROWSER_PREFIX) else { return };
        if h.is_streaming_from(sid) {
            h.stop_stream(sid);
        }
        h.forget_device_settings(sid);
        self.known.retain(|(id, _)| id != bid);
        h.schedule_save();
        self.note_remembered(h);
    }

    // ── Config ──

    fn config(&self) -> Value {
        let known: Map<String, Value> = self.known.iter().map(|(bid, s)| (bid.clone(), json!({"name": s.name, "seen": s.seen}))).collect();
        json!({"size": self.size, "fps": self.fps, "address": self.address, "known": known})
    }

    fn load(&mut self, cfg: &Value) {
        let size = cfg.get("size").and_then(Value::as_str).filter(|s| SIZES.iter().any(|z| z.0 == *s));
        self.size = size.unwrap_or(DEFAULT_SIZE).into();
        let fps = cfg.get("fps").filter(|v| !v.is_boolean()).and_then(Value::as_f64)
            .and_then(|f| FPS_CHOICES.iter().copied().find(|c| f64::from(*c) == f));
        self.fps = fps.unwrap_or(DEFAULT_FPS);
        self.shared.fps.store(self.fps, Ordering::SeqCst);
        self.address = cfg.get("address").and_then(Value::as_str).unwrap_or("").into();
        self.known = cfg.get("known").and_then(Value::as_object).map(|known| known.iter().filter_map(|(bid, e)| {
            let name = e.get("name")?.as_str()?.to_string();
            let seen = e.get("seen").filter(|v| !v.is_boolean())?.as_f64()?;
            Some((bid.clone(), Seen { name, seen }))
        }).collect()).unwrap_or_default();
        if self.server.is_some() {
            self.fill_addresses();
        }
        self.apply_capture();
    }

    fn diagnostic_lines(&self) -> Vec<(String, String)> {
        let Some(server) = &self.server else {
            let state = if self.problem.is_empty() { "off" } else { "not available" };
            return vec![("Browser camera".into(), state.into())];
        };
        let sending: Vec<String> = self.connected().iter().map(|s| {
            let feed = &s.feed;
            let error = feed.rtc_error();
            let sent = feed.sent();
            let what = if !error.is_empty() {
                error
            } else {
                let text = sending_text(&feed.codec(), &sent);
                if text.is_empty() { "nothing yet".into() } else { text[2..].to_string() }
            };
            if sent.limit.is_empty() { what } else { format!("{what} (held back: {})", sent.limit) }
        }).collect();
        let mut out = vec![("Browser camera".to_string(), if sending.is_empty() {
            "waiting for a browser".to_string()
        } else {
            format!("{} browser{} connected, sending {}", sending.len(), if sending.len() > 1 { "s" } else { "" }, sending.join("; "))
        })];
        if let Some(stats) = server.stats() {
            let port = if stats.fell_back { format!("{} ({} was taken)", stats.port, stats.wanted_port) } else { stats.port.to_string() };
            out.push(("Browser camera port".into(), port));
            // How far browsers got: none at all points at the network or firewall, handshakes alone at the certificate
            out.push(("Browser camera reached".into(), format!(
                "{} connections, {} certificate refusals, {} page loads, {} old links, {} browsers",
                stats.count(Step::Connections), stats.count(Step::TlsFailed), stats.count(Step::Pages), stats.count(Step::Refused),
                stats.count(Step::Browsers))));
        }
        out
    }

    // ── What the host hands over ──

    pub fn hear(&mut self, h: &mut dyn BrowserHost, event: &Event) {
        match event {
            Event::SourceSelected { source_id } => self.on_source_selected(h, source_id),
            Event::StreamBehind { source_id, behind } => self.on_stream_behind(h, source_id, *behind),
            Event::StreamsChanged { .. } | Event::StreamStartFailed => self.on_streams_changed(h),
            Event::ForgetSourceRequested { source_id } => self.forget_browser(h, source_id),
            Event::Posted { owner, token, .. } if owner == NAME => match token.as_str() {
                POST_CHANGED => self.on_server_changed(h),
                POST_PREPARE => {
                    self.prepare(h);
                }
                POST_CANCEL => self.cancel_waiting(h),
                _ => {}
            },
            Event::TimerFired { owner, token } if owner == NAME && token == TIMER_HINT => self.render(h),
            _ => {}
        }
    }

    pub fn ui(&mut self, h: &mut dyn BrowserHost, callback: &str, args: &[Value]) {
        let i = args.first().and_then(Value::as_f64).map_or(0, |v| v.max(0.0) as usize);
        match callback {
            "new-link" => self.new_link(h),
            "copy" => {
                let url = self.url();
                h.copy_text(&url);
            }
            "address-picked" => self.on_address_picked(h, i),
            "size-picked" => {
                if let Some((size, _, _)) = SIZES.get(i) {
                    self.set_capture(h, Some(size), None);
                }
            }
            "fps-picked" => {
                if let Some(fps) = FPS_CHOICES.get(i) {
                    self.set_capture(h, None, Some(*fps));
                }
            }
            _ => return,
        }
        self.render(h);
    }

    pub fn shutdown_on(&mut self, h: &mut dyn BrowserHost) {
        self.stop_server(h);
    }
}

fn issue() -> Issue {
    Issue { title: String::new(), text: String::new(), actions: Vec::new(), kind: "err".into(), details: String::new(), tell_dismiss: false }
}

impl Plugin for BrowserCameraPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: "Browser camera".into(),
            about: "Streaming from any device's web browser, by scanning a code.".into(),
            when_off: "Without it Browser camera isn't in the phone picker and its card is gone too.".into(),
            category: "streaming".into(),
            icon: "wifi".into(),
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        let poster = host.poster();
        self.setup_on(&mut Hosted(host), poster);
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.tidy_known(&mut Hosted(host)); // once the saved list is in
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.shutdown_on(&mut Hosted(host));
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec {
            ui: UiSpec {
                source: SlintSource::Text(include_str!("../../ui/browser_camera.slint").into()),
                component: "BrowserCameraCard".into(),
                props: json!({"shown": false}),
            },
            // where the Camera card is, which hides while this streams
            region: "right".into(),
        })
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.hear(&mut Hosted(host), event);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui == "card" {
            self.ui(&mut Hosted(host), &event.callback, &event.args);
        }
    }

    fn on_issue_action(&mut self, host: &mut dyn Host, key: &str, id: &str) {
        if key == "start" && (id == POST_CANCEL || id == "dismiss") {
            self.cancel_waiting(&mut Hosted(host));
        }
    }

    fn get_config(&self) -> Value {
        self.config()
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.load(cfg);
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        self.diagnostic_lines()
    }
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod tests;
