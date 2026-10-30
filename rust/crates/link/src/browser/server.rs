//! The Browser camera's server (BrowserServer in browser_server.py): any device with a browser streams its camera and
//! mic here, no app needed.
//!
//! The device opens https://<this computer>:<port>/#<token> and allows the camera. Browsers only allow that on HTTPS
//! (or localhost), so the server has the self-signed certificate core's Web shares, which the browser warns about
//! once. The token is new every time the server starts. The page then opens a WebSocket here, which says the browser
//! is there and carries the WebRTC offer and answer, the settings and the page's stats; the camera and mic themselves
//! go over WebRTC (rtc.rs).
//!
//! Several browsers can connect at once. Each page keeps an id of its own (in the browser's storage), so a reload or a
//! second tab on the same device takes over that browser's feed, while another device gets a feed of its own.

use super::feed::{BrowserFeed, BrowserHub, Sent, DEFAULT_ID};
use super::rtc::{RtcMaker, RtcSession};
use super::ws::{self, Closed, WsConnection};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::i18n::tr;
use telescope_core::web::{Certificate, PageHandler, Reply, Request, Serve, ServerStats, Socket, Step, Upgrade, Web, WebServer};

pub const BROWSER_PORT: u16 = 8767;
/// Browsers answer pings by themselves, so a page that's quiet but there isn't closed as idle.
const PING_EVERY: Duration = Duration::from_secs(3);
/// How often the connection looks for something to send while nothing arrives.
const TICK: Duration = Duration::from_millis(500);
/// Opened again on the same browser.
pub const CLOSE_REPLACED: u16 = 4000;
/// Every camera is taken.
pub const CLOSE_FULL: u16 = 4001;
/// New link: this code doesn't work any more, so the page stops instead of retrying.
pub const CLOSE_NEW_LINK: u16 = 4002;

const PAGE: &[u8] = include_bytes!("../../web/index.html");
const SCRIPT: &[u8] = include_bytes!("../../web/app.js");

/// What the server and each connection's thread share.
struct Shared {
    hub: Arc<BrowserHub>,
    token: Mutex<String>,
    on_change: Box<dyn Fn() + Send + Sync>,
    rtc: RtcMaker,
    /// Connections open now, by an id of their own: the close code each is to end with, once it's asked to.
    open: Mutex<HashMap<u64, Option<u16>>>,
    next_id: AtomicU64,
    /// (how often to ping, how long a quiet connection lasts): PING_EVERY and ws::IDLE_TIMEOUT, shorter in tests.
    timings: Mutex<(Duration, Duration)>,
}

impl Shared {
    fn token_ok(&self, token: &str) -> bool {
        let ours = self.token.lock().unwrap_or_else(|e| e.into_inner()).clone();
        // hmac.compare_digest: as long whatever the first difference
        !token.is_empty() && token.len() == ours.len() && token.bytes().zip(ours.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
    }

    fn close_all(&self, code: u16) {
        // Each connection's own thread notices within TICK and closes with the code: a socket isn't touched from here
        for kicked in self.open.lock().unwrap_or_else(|e| e.into_inner()).values_mut() {
            kicked.get_or_insert(code);
        }
    }

    fn kicked(&self, id: u64) -> Option<u16> {
        self.open.lock().unwrap_or_else(|e| e.into_inner()).get(&id).copied().flatten()
    }

    /// One browser's connection, on its own thread, until it ends or a newer one takes over.
    fn serve_browser(self: &Arc<Self>, sock: Box<dyn Socket>, bid: &str) {
        let Some(feed) = self.hub.join(bid) else {
            let mut ws = WsConnection::new(sock, |_| Ok(()));
            ws.send_quietly(&ws::close_frame(CLOSE_FULL, "Every camera is in use"));
            return;
        };
        let gen = feed.connect();
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.open.lock().unwrap_or_else(|e| e.into_inner()).insert(id, None);
        let rtc: RefCell<Option<Box<dyn RtcSession>>> = RefCell::new(None);
        let (mut sent_seq, mut pinged) = (None, Instant::now());
        let (ping_every, idle) = *self.timings.lock().unwrap_or_else(|e| e.into_inner());
        let tick = |send: &mut dyn FnMut(&[u8]) -> Result<(), Closed>| -> Result<(), Closed> {
            if !feed.current(gen) {
                return Err(Closed("replaced".into()));
            }
            if self.kicked(id).is_some() {
                return Err(Closed("ended here".into()));
            }
            let (seq, mut cfg) = feed.page_config();
            if sent_seq != Some(seq) {
                sent_seq = Some(seq);
                cfg["type"] = json!("config");
                send(&ws::frame(ws::TEXT, cfg.to_string().as_bytes()))?;
            }
            if pinged.elapsed() >= ping_every {
                pinged = Instant::now();
                send(&ws::frame(ws::PING, b""))?;
            }
            if feed.take_keyframe_wish() {
                if let Some(session) = rtc.borrow().as_ref() {
                    session.want_keyframe();
                }
            }
            Ok(())
        };
        let mut ws = WsConnection::new(sock, tick);
        ws.set_read_timeout(Some(TICK.min(ping_every)));
        ws.set_idle(idle);
        (self.on_change)();
        let ended = (|| -> Result<(), Closed> {
            ws.tick()?;
            loop {
                let (opcode, payload) = ws.read_message()?;
                ws.tick()?;
                if opcode == ws::TEXT {
                    if let Some(reply) = self.on_text(&feed, gen, &rtc, &payload) {
                        ws.send(&ws::frame(ws::TEXT, reply.to_string().as_bytes()))?;
                    }
                }
            }
        })();
        if let Err(Closed(why)) = ended {
            match why.as_str() {
                "replaced" => ws.send_quietly(&ws::close_frame(CLOSE_REPLACED, "Opened somewhere else")),
                "ended here" => ws.send_quietly(&ws::close_frame(self.kicked(id).unwrap_or(1001), "")),
                _ => {}
            }
        }
        drop(ws);
        if let Some(mut session) = rtc.borrow_mut().take() {
            session.close();
        }
        self.open.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        feed.disconnect(gen);
        (self.on_change)();
    }

    /// A text message from the page; what to send back as a text frame, if anything.
    fn on_text(&self, feed: &Arc<BrowserFeed>, gen: u64, rtc: &RefCell<Option<Box<dyn RtcSession>>>, payload: &[u8]) -> Option<Value> {
        let msg: Value = serde_json::from_slice(payload).ok()?;
        let msg = msg.as_object()?;
        match msg.get("type").and_then(Value::as_str) {
            Some("hello") => {
                feed.note_hello(gen, &clean(msg.get("device"), 48), &clean(msg.get("mic_error"), 160));
                (self.on_change)();
                None
            }
            Some("offer") => {
                let sdp = msg.get("sdp")?.as_str()?;
                let reply = self.answer(feed, gen, rtc, sdp);
                (self.on_change)();
                Some(reply)
            }
            Some("stats") => {
                let sent = sent_of(msg);
                if sent != feed.sent() {
                    feed.note_sent(gen, sent);
                    (self.on_change)();
                }
                None
            }
            _ => None,
        }
    }

    /// Answers the page's offer with a new peer connection (ending the one before), as the message to send back.
    fn answer(&self, feed: &Arc<BrowserFeed>, gen: u64, rtc: &RefCell<Option<Box<dyn RtcSession>>>, offer: &str) -> Value {
        if let Some(mut old) = rtc.borrow_mut().take() {
            old.close();
        }
        let (video, audio) = (feed.clone(), feed.clone());
        let mut session = (self.rtc)(Box::new(move |codec, data, size| video.put_video(gen, codec, data, size)),
                                     Box::new(move |pcm| audio.put_audio(gen, &pcm)));
        let answered = session.answer(offer, feed.max_kbps());
        let codec = session.codec();
        *rtc.borrow_mut() = Some(session);
        match answered {
            Ok(sdp) => {
                feed.note_rtc(gen, &codec, "");
                json!({"type": "answer", "sdp": sdp})
            }
            Err(e) => {
                log::warn!("Browser camera: couldn't answer the browser's offer: {}", e.0);
                feed.note_rtc(gen, "", &tr("This computer couldn't set up the video connection."));
                json!({"type": "rtc_error", "message": "The computer couldn't set up the video connection."})
            }
        }
    }
}

/// The page's stats, kept to numbers and a known limit.
fn sent_of(msg: &serde_json::Map<String, Value>) -> Sent {
    let number = |key: &str| {
        let v = msg.get(key).filter(|v| !v.is_boolean())?.as_f64()?;
        (0.0..100_000.0).contains(&v).then(|| telescope_core::actions::round_half_even(v) as u32)
    };
    let limit = msg.get("limit").and_then(Value::as_str).filter(|l| matches!(*l, "cpu" | "bandwidth")).unwrap_or("");
    Sent { width: number("width"), height: number("height"), fps: number("fps"), limit: limit.into() }
}

/// A string from the browser as plain display text.
fn clean(value: Option<&Value>, limit: usize) -> String {
    let Some(text) = value.and_then(Value::as_str) else { return String::new() };
    text.chars().filter(|c| !c.is_control() && !matches!(c, '<' | '>' | '\u{2028}' | '\u{2029}')).take(limit).collect()
}

/// A browser id as its page keeps one, or the default for a page that sends none (or nonsense).
fn browser_id(raw: &str) -> String {
    let ok = (8..=40).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok { raw.into() } else { DEFAULT_ID.into() }
}

struct Handler(Arc<Shared>);

impl PageHandler for Handler {
    fn handle(&self, request: &Request, stats: &ServerStats) -> Reply {
        match request.path() {
            "/ws" => {
                if !websocket_request(request) {
                    return Reply::text(400, "WebSocket only");
                }
                stats.note(Step::Refused);
                Reply::text(403, "This link has expired. Scan the code on the computer again.")
            }
            "/" => {
                stats.note(Step::Pages);
                Reply::new(200, PAGE, "text/html; charset=utf-8")
            }
            "/app.js" => Reply::new(200, SCRIPT, "text/javascript; charset=utf-8"),
            _ => Reply::text(404, "Not found"),
        }
    }

    fn upgrade(&self, request: &Request, stats: &ServerStats) -> Option<Upgrade> {
        if request.path() != "/ws" || !websocket_request(request) || !self.0.token_ok(request.param("token").unwrap_or("")) {
            return None;
        }
        let key = request.header("Sec-WebSocket-Key")?;
        stats.note(Step::Browsers);
        let (shared, bid) = (self.0.clone(), browser_id(request.param("id").unwrap_or("")));
        Some(Upgrade {
            headers: vec![("Upgrade".into(), "websocket".into()), ("Connection".into(), "Upgrade".into()),
                          ("Sec-WebSocket-Accept".into(), ws::accept_key(key))],
            run: Box::new(move |sock| shared.serve_browser(sock, &bid)),
        })
    }
}

fn websocket_request(request: &Request) -> bool {
    request.header("Upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
        && request.header("Sec-WebSocket-Key").is_some_and(|k| !k.is_empty())
        && request.header("Sec-WebSocket-Version") == Some("13")
}

/// Serves the page and takes each browser's camera into its feed in the hub. start() binds, stop() ends everything.
pub struct BrowserServer {
    shared: Arc<Shared>,
    web: Arc<dyn Web>,
    certificate: Certificate,
    want_port: u16,
    host: String,
    server: Mutex<Option<Box<dyn WebServer>>>,
}

impl BrowserServer {
    /// `rtc` makes a browser's WebRTC side for each offer (rtc::StrSession; tests stand one in).
    pub fn new(hub: Arc<BrowserHub>, web: Arc<dyn Web>, certificate: Certificate, on_change: impl Fn() + Send + Sync + 'static,
               port: u16, host: &str, rtc: RtcMaker) -> BrowserServer {
        let shared = Arc::new(Shared {
            hub, token: Mutex::new(crate::util::token_urlsafe(18)), on_change: Box::new(on_change), rtc, open: Mutex::default(),
            next_id: AtomicU64::new(1), timings: Mutex::new((PING_EVERY, ws::IDLE_TIMEOUT)),
        });
        BrowserServer { shared, web, certificate, want_port: port, host: host.into(), server: Mutex::new(None) }
    }

    pub fn hub(&self) -> &Arc<BrowserHub> {
        &self.shared.hub
    }

    /// The feed of a browser that sent no id of its own (and, given one feed, every browser's).
    pub fn feed(&self) -> Option<Arc<BrowserFeed>> {
        self.shared.hub.feed(DEFAULT_ID)
    }

    pub fn port(&self) -> u16 {
        self.server.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(0, |s| s.port())
    }

    pub fn stats(&self) -> Option<Arc<ServerStats>> {
        self.server.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|s| s.stats())
    }

    pub fn token(&self) -> String {
        self.shared.token.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn url_for(&self, host: &str) -> String {
        format!("https://{host}:{}/#{}", self.port(), self.token())
    }

    pub fn token_ok(&self, token: &str) -> bool {
        self.shared.token_ok(token)
    }

    /// Make the old link stop working (and drop whoever used it).
    pub fn new_token(&self) {
        *self.shared.token.lock().unwrap_or_else(|e| e.into_inner()) = crate::util::token_urlsafe(18);
        self.shared.close_all(CLOSE_NEW_LINK);
    }

    /// How often a page is pinged and how long one that's quiet lasts, for connections from now on.
    pub fn set_timings(&self, ping_every: Duration, idle: Duration) {
        *self.shared.timings.lock().unwrap_or_else(|e| e.into_inner()) = (ping_every, idle);
    }

    pub fn start(&self) -> Result<(), String> {
        let serve = Serve { port: self.want_port, host: self.host.clone(), label: "Browser camera".into() };
        let server = self.web.serve(&self.certificate, &serve, Arc::new(Handler(self.shared.clone())))?;
        *self.server.lock().unwrap_or_else(|e| e.into_inner()) = Some(server);
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(server) = self.server.lock().unwrap_or_else(|e| e.into_inner()).take() {
            server.stop();
        }
        self.shared.close_all(1001);
    }
}

impl Drop for BrowserServer {
    fn drop(&mut self) {
        self.stop();
    }
}
