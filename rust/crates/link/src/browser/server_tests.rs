//! tests/test_browser_server.py, test for test: a client stands in for the browser over TLS and a WebSocket, and FakeRtc
//! for its WebRTC side (rtc_tests.rs runs the real one).

use super::feed::{BrowserAudio, BrowserFeed, BrowserHub, BrowserVideo, Frame, Sent};
use super::rtc::{max_kbps_for, AudioSink, RtcError, RtcMaker, RtcSession, VideoSink};
use super::server::*;
use super::ws;
use crate::web::{ensure_certificate, SystemWeb};
use rustls::pki_types::{CertificateDer, ServerName};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use telescope_core::audio::AudioFeed;
use telescope_core::media::{Codec, VideoFeed};
use telescope_core::testing::scratch_dir;
use telescope_core::web::{Certificate, Step};

fn cert() -> Certificate {
    static CERT: OnceLock<Certificate> = OnceLock::new();
    CERT.get_or_init(|| ensure_certificate(&scratch_dir("browser-cert"), &["127.0.0.1".into()], now()).unwrap()).clone()
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// What a FakeRtc was made with and has been through; the test hands frames and audio in through it.
#[derive(Default)]
struct Made {
    sinks: Mutex<Option<(VideoSink, AudioSink)>>,
    max_kbps: Mutex<u32>,
    closed: AtomicBool,
}

impl Made {
    fn video(&self, data: &[u8], size: u64) {
        if let Some((video, _)) = self.sinks.lock().unwrap().as_mut().filter(|_| !self.closed.load(Ordering::SeqCst)) {
            video(Codec::H264, data.to_vec(), size);
        }
    }

    fn audio(&self, pcm: &[u8]) {
        if let Some((_, audio)) = self.sinks.lock().unwrap().as_mut().filter(|_| !self.closed.load(Ordering::SeqCst)) {
            audio(pcm.to_vec());
        }
    }

    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// Stands in for StrSession: answers any offer (or none, failing).
struct FakeRtc {
    made: Arc<Made>,
    fails: bool,
    codec: String,
}

impl RtcSession for FakeRtc {
    fn answer(&mut self, offer: &str, max_kbps: u32) -> Result<String, RtcError> {
        if self.fails {
            return Err(RtcError("no codec in common".into()));
        }
        self.codec = "H264".into();
        *self.made.max_kbps.lock().unwrap() = max_kbps;
        Ok(format!("answer to {offer}"))
    }

    fn codec(&self) -> String {
        self.codec.clone()
    }

    fn close(&mut self) {
        self.made.closed.store(true, Ordering::SeqCst);
    }
}

fn fake_rtc(fails: bool) -> (RtcMaker, Arc<Mutex<Vec<Arc<Made>>>>) {
    let made: Arc<Mutex<Vec<Arc<Made>>>> = Arc::default();
    let all = made.clone();
    let maker: RtcMaker = Arc::new(move |video, audio| {
        let m = Arc::new(Made { sinks: Mutex::new(Some((video, audio))), ..Made::default() });
        all.lock().unwrap().push(m.clone());
        Box::new(FakeRtc { made: m, fails, codec: String::new() }) as Box<dyn RtcSession>
    });
    (maker, made)
}

struct Run {
    server: BrowserServer,
    made: Arc<Mutex<Vec<Arc<Made>>>>,
    changes: Arc<AtomicUsize>,
}

impl Run {
    fn last(&self) -> Arc<Made> {
        self.made.lock().unwrap().last().unwrap().clone()
    }

    fn feed(&self) -> Arc<BrowserFeed> {
        self.server.feed().unwrap()
    }

    fn browser(&self, bid: &str) -> Browser {
        Browser::connect(self.server.port(), &self.server.token(), bid)
    }
}

fn serve(hub: Arc<BrowserHub>, fails: bool) -> Run {
    let (maker, made) = fake_rtc(fails);
    let changes = Arc::new(AtomicUsize::new(0));
    let counted = changes.clone();
    let server = BrowserServer::new(hub, Arc::new(SystemWeb), cert(), move || {
        counted.fetch_add(1, Ordering::SeqCst);
    }, 0, "127.0.0.1", maker);
    server.start().unwrap();
    Run { server, made, changes }
}

fn server() -> Run {
    serve(Arc::new(BrowserHub::single(Arc::new(BrowserFeed::new()))), false)
}

/// What's logged goes through one process-wide logger, so a test that reads it runs in a process of its own: true in the first
/// run, once that process has passed; false in the process itself, which goes on with the test.
fn on_its_own(test: &str) -> bool {
    const ALONE: &str = "TELESCOPE_TEST_ALONE";
    if std::env::var_os(ALONE).is_some() {
        return false;
    }
    let name = format!("browser::server_tests::{test}");
    let out = std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact", &name, "--nocapture"]).env(ALONE, "1").output().unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && said.contains("1 passed"), "{said}\n{}", String::from_utf8_lossy(&out.stderr));
    true
}

struct Lines(Arc<Mutex<Vec<String>>>);

impl log::Log for Lines {
    fn enabled(&self, _meta: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        if record.target().starts_with("telescope") {
            self.0.lock().unwrap().push(record.args().to_string());
        }
    }
    fn flush(&self) {}
}

/// Everything logged from here on.
fn log_lines() -> Arc<Mutex<Vec<String>>> {
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    log::set_boxed_logger(Box::new(Lines(lines.clone()))).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    lines
}

/// Just enough of a browser's WebSocket: masked frames out, unmasked in.
struct Browser {
    tls: rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
    status: u16,
    head: String,
    accept: String,
    buf: Vec<u8>,
}

#[derive(Debug)]
struct NoCheck;

impl rustls::client::danger::ServerCertVerifier for NoCheck {
    fn verify_server_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8],
                          _: rustls::pki_types::UnixTime) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &rustls::DigitallySignedStruct)
                              -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &rustls::DigitallySignedStruct)
                              -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

/// TLS that takes any certificate, as a browser does once told to go ahead.
fn tls(port: u16) -> rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap()
        .dangerous().with_custom_certificate_verifier(Arc::new(NoCheck)).with_no_client_auth();
    let tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let conn = rustls::ClientConnection::new(Arc::new(config), ServerName::try_from("127.0.0.1").unwrap()).unwrap();
    rustls::StreamOwned::new(conn, tcp)
}

/// (status, content type, body) of a GET.
fn get(port: u16, path: &str) -> (u16, String, Vec<u8>) {
    let mut s = tls(port);
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
    let mut all = Vec::new();
    let _ = s.read_to_end(&mut all);
    let split = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&all[..split]).into_owned();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let ctype = head.lines().find_map(|l| l.strip_prefix("Content-Type: ")).unwrap_or("").to_string();
    (status, ctype, all[split + 4..].to_vec())
}

impl Browser {
    fn connect(port: u16, token: &str, bid: &str) -> Browser {
        let mut tls = tls(port);
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let query = if bid.is_empty() { format!("token={token}") } else { format!("token={token}&id={bid}") };
        tls.write_all(format!("GET /ws?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                               Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").as_bytes()).unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match tls.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => break,
            }
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        Browser { tls, status, head, accept: ws::accept_key(key), buf: Vec::new() }
    }

    fn send(&mut self, opcode: u8, payload: &[u8], fin: bool) {
        let mask = [1u8, 2, 3, 4];
        let n = payload.len();
        let mut out = vec![if fin { 0x80 } else { 0 } | opcode];
        if n < 126 {
            out.push(0x80 | n as u8);
        } else if n < 1 << 16 {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            out.push(0x80 | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
        out.extend_from_slice(&mask);
        out.extend_from_slice(&ws::unmask(payload, mask));
        self.tls.write_all(&out).unwrap();
    }

    fn text(&mut self, msg: Value) {
        self.send(ws::TEXT, msg.to_string().as_bytes(), true);
    }

    fn take(&mut self, n: usize) -> Vec<u8> {
        let mut chunk = [0u8; 65536];
        while self.buf.len() < n {
            let got = self.tls.read(&mut chunk).expect("more from the server");
            assert!(got > 0, "the server hung up");
            self.buf.extend_from_slice(&chunk[..got]);
        }
        self.buf.drain(..n).collect()
    }

    fn recv(&mut self) -> (u8, Vec<u8>) {
        let head = self.take(2);
        let mut n = (head[1] & 0x7F) as usize;
        if n == 126 {
            n = u16::from_be_bytes(self.take(2).try_into().unwrap()) as usize;
        } else if n == 127 {
            n = u64::from_be_bytes(self.take(8).try_into().unwrap()) as usize;
        }
        (head[0] & 0x0F, self.take(n))
    }

    fn recv_json(&mut self) -> Value {
        let (op, payload) = self.recv();
        assert_eq!(op, ws::TEXT);
        serde_json::from_slice(&payload).unwrap()
    }

    /// Skips what the server sends until its close frame, and gives its code.
    fn next_close(&mut self) -> u16 {
        loop {
            let (op, payload) = self.recv();
            if op == ws::CLOSE {
                return u16::from_be_bytes([payload[0], payload[1]]);
            }
        }
    }

    fn offer(&mut self, sdp: &str) -> Value {
        self.text(json!({"type": "offer", "sdp": sdp}));
        self.recv_json()
    }
}

fn wait(cond: impl Fn() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

const FIVE: Duration = Duration::from_secs(5);

// ── Framing ──────────────────────────────────────────────────────────────────

#[test]
fn accept_key_matches_the_rfc_example() {
    assert_eq!(ws::accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
}

#[test]
fn unmask_is_its_own_inverse_at_any_length() {
    let mask = [1, 2, 3, 4];
    for n in [0usize, 1, 3, 4, 5, 200, 70000] {
        let data: Vec<u8> = (0..n).map(|i| (i * 7 % 251) as u8).collect();
        assert_eq!(ws::unmask(&ws::unmask(&data, mask), mask), data);
    }
}

#[test]
fn server_frames_use_the_right_length_encoding() {
    assert_eq!(ws::frame(0x1, b"hi")[..2], [0x81, 0x02]);
    assert_eq!(ws::frame(0x2, &[0; 300])[..4], [0x82, 0x7e, 0x01, 0x2c]);
    assert_eq!(ws::frame(0x2, &vec![0; 70000])[..2], [0x82, 0x7f]);
}

// ── Server ───────────────────────────────────────────────────────────────────

#[test]
fn serves_the_page_and_its_script() {
    let run = server();
    let port = run.server.port();
    let (status, ctype, body) = get(port, "/");
    assert!(status == 200 && ctype.starts_with("text/html") && String::from_utf8_lossy(&body).contains("/app.js"));
    assert_eq!(get(port, "/app.js").0, 200);
    assert_eq!(get(port, "/mic-worklet.js").0, 404); // the mic goes over WebRTC now
    assert_eq!(get(port, "/../browser_server.py").0, 404);
}

#[test]
fn the_page_is_the_python_apps_own() {
    // The page tests run on desktop/telescope/web; the copy served here has to be the same
    let desktop = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../desktop/telescope/web");
    for name in ["index.html", "app.js"] {
        let ours = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web").join(name)).unwrap();
        assert_eq!(ours, std::fs::read(desktop.join(name)).unwrap(), "{name} differs from the desktop app's");
    }
}

#[test]
fn url_carries_the_token_after_the_hash() {
    let run = server();
    assert_eq!(run.server.url_for("10.0.0.2"), format!("https://10.0.0.2:{}/#{}", run.server.port(), run.server.token()));
}

#[test]
fn wrong_token_is_refused() {
    let run = server();
    let b = Browser::connect(run.server.port(), "nope", "");
    assert_eq!(b.status, 403);
    drop(b);
    assert!(!run.feed().connected());
    assert_eq!(run.server.stats().unwrap().count(Step::Refused), 1);
}

#[test]
fn stats_count_each_step_a_browser_gets_to() {
    if on_its_own("stats_count_each_step_a_browser_gets_to") {
        return;
    }
    let lines = log_lines();
    let run = server();
    let port = run.server.port();
    let stats = run.server.stats().unwrap();
    assert!(stats.nothing_arrived() && stats.port == port);
    get(port, "/");
    let mut raw = TcpStream::connect(("127.0.0.1", port)).unwrap();
    raw.write_all(b"not a TLS handshake").unwrap(); // what a browser that won't trust the certificate amounts to here
    drop(raw);
    let mut b = run.browser("");
    b.recv_json();
    drop(b);
    assert!(wait(|| stats.count(Step::TlsFailed) == 1, FIVE));
    assert_eq!((stats.count(Step::Connections), stats.count(Step::Pages), stats.count(Step::Browsers)), (3, 1, 1));
    let logged = lines.lock().unwrap();
    // only the first time
    assert_eq!(logged.iter().filter(|l| *l == "Browser camera: a device reached this computer").count(), 1, "{logged:?}");
    let token = run.server.token();
    assert!(!logged.iter().any(|l| l.contains("127.0.0.1") || l.contains(&token)), "{logged:?}");
}

#[test]
fn a_taken_port_is_logged_and_shown() {
    if on_its_own("a_taken_port_is_logged_and_shown") {
        return;
    }
    let lines = log_lines();
    let first = server();
    let (maker, _) = fake_rtc(false);
    let second = BrowserServer::new(Arc::new(BrowserHub::single(Arc::new(BrowserFeed::new()))), Arc::new(SystemWeb), cert(), || {}, first.server.port(), "127.0.0.1", maker);
    second.start().unwrap();
    assert_ne!(second.port(), first.server.port());
    let stats = second.stats().unwrap();
    assert!(stats.fell_back && stats.port == second.port());
    assert!(!first.server.stats().unwrap().fell_back);
    assert!(lines.lock().unwrap().iter().any(|l| l.contains("was taken")), "{:?}", lines.lock().unwrap());
    second.stop();
}

#[test]
fn an_offer_is_answered_and_the_browsers_video_and_mic_come_out() {
    let run = server();
    let feed = run.feed();
    let mut b = run.browser("");
    assert_eq!(b.status, 101);
    assert!(b.head.contains(&format!("Sec-WebSocket-Accept: {}", b.accept)), "{}", b.head);
    let config = b.recv_json();
    assert_eq!(config, json!({"type": "config", "width": 1280, "height": 720, "fps": 30, "audio": false,
                              "max_kbps": max_kbps_for(1280, 720, 30)}));

    b.text(json!({"type": "hello", "device": "Safari on <iPhone>", "mic_error": ""}));
    assert!(wait(|| feed.device() == "Safari on iPhone", FIVE));

    assert_eq!(b.offer("offer-sdp"), json!({"type": "answer", "sdp": "answer to offer-sdp"}));
    let rtc = run.last();
    assert_eq!(json!(*rtc.max_kbps.lock().unwrap()), config["max_kbps"]);
    assert_eq!(feed.codec(), "H264");

    let (codec, mut reader) = BrowserVideo::open(feed.clone()).unwrap();
    assert_eq!(codec, Codec::H264);
    rtc.video(&[0, 0, 0, 1, 0x65, 7], 2345);
    assert_eq!(reader.read().unwrap().data, [0, 0, 0, 1, 0x65, 7]);
    assert_eq!(feed.frame_bytes(), 2345);

    // The mic asks for audio: the page hears it should send some, and it comes out of the stream
    let mic = feed.open_audio().unwrap();
    assert_eq!(b.recv_json()["audio"], true);
    let pcm: Vec<u8> = [1i16, -2, 300, -32768].iter().flat_map(|s| s.to_le_bytes()).collect();
    rtc.audio(&pcm);
    assert_eq!(mic.read_some(4096), pcm);
    drop(mic);
    assert_eq!(b.recv_json()["audio"], false);

    drop(b);
    assert!(wait(|| !feed.connected(), FIVE));
    assert!(wait(|| rtc.closed(), FIVE)); // the browser left, so its WebRTC side ends too
    assert!(reader.read().is_err());
}

#[test]
fn a_new_offer_replaces_the_connection_before_it() {
    let run = server();
    let mut b = run.browser("");
    b.recv_json();
    b.offer("first");
    let first = run.last();
    assert_eq!(b.offer("second"), json!({"type": "answer", "sdp": "answer to second"}));
    assert!(first.closed() && !run.last().closed());
}

#[test]
fn an_offer_that_cant_be_answered_tells_the_page_and_the_card() {
    let run = serve(Arc::new(BrowserHub::single(Arc::new(BrowserFeed::new()))), true);
    let mut b = run.browser("");
    b.recv_json();
    let reply = b.offer("offer-sdp");
    assert!(reply["type"] == "rtc_error" && reply["message"].as_str().is_some_and(|m| !m.is_empty()));
    assert!(!run.feed().rtc_error().is_empty() && run.feed().codec().is_empty());
}

#[test]
fn the_pages_stats_reach_the_card_cleaned_up() {
    let run = server();
    let feed = run.feed();
    let mut b = run.browser("");
    b.recv_json();
    let before = run.changes.load(Ordering::SeqCst);
    b.text(json!({"type": "stats", "width": 1280, "height": 720, "fps": 29.6, "limit": "cpu"}));
    let full = Sent { width: Some(1280), height: Some(720), fps: Some(30), limit: "cpu".into() };
    assert!(wait(|| feed.sent() == full, FIVE));
    assert!(wait(|| run.changes.load(Ordering::SeqCst) > before, FIVE));
    b.text(json!({"type": "stats", "width": "<b>", "fps": -5, "limit": "everything"}));
    assert!(wait(|| feed.sent() == Sent::default(), FIVE));
}

#[test]
fn fragmented_messages_and_pings_are_handled() {
    let run = server();
    let feed = run.feed();
    let mut b = run.browser("");
    b.recv_json();
    b.send(ws::PING, b"hey", true);
    assert_eq!(b.recv(), (ws::PONG, b"hey".to_vec()));
    let data = json!({"type": "hello", "device": "Firefox on Android", "mic_error": ""}).to_string();
    b.send(ws::TEXT, &data.as_bytes()[..10], false);
    b.send(0x0, &data.as_bytes()[10..], true);
    assert!(wait(|| feed.device() == "Firefox on Android", FIVE));
    b.send(ws::BINARY, b"\x01old page media", true); // a page from before WebRTC: ignored, not an error
    b.text(json!({"type": "hello", "device": "still here", "mic_error": ""}));
    assert!(wait(|| feed.device() == "still here", FIVE));
}

#[test]
fn a_quiet_page_that_answers_pings_stays_and_one_that_doesnt_goes() {
    let run = server();
    run.server.set_timings(Duration::from_millis(100), Duration::from_millis(600));
    let feed = run.feed();
    let mut b = run.browser("");
    b.recv_json();
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut pings = 0;
    while Instant::now() < deadline {
        let (op, payload) = b.recv();
        if op == ws::PING {
            pings += 1;
            b.send(ws::PONG, &payload, true); // what a browser does by itself
        }
    }
    assert!(pings >= 3 && feed.connected(), "{pings} pings");
    assert!(wait(|| !feed.connected(), Duration::from_secs(3))); // nothing answers now
}

#[test]
fn a_newer_browser_takes_over_and_the_older_is_told() {
    let run = server();
    let mut first = run.browser("");
    first.recv_json();
    let mut second = run.browser("");
    second.recv_json();
    assert_eq!(first.next_close(), CLOSE_REPLACED);
    assert!(run.feed().connected());
}

fn hub_server() -> (Run, Arc<BrowserHub>) {
    let joined = Arc::new(AtomicUsize::new(0));
    let hub = Arc::new(BrowserHub::new(move |_| joined.fetch_add(1, Ordering::SeqCst) < 2));
    (serve(hub.clone(), false), hub)
}

fn ids(hub: &BrowserHub) -> Vec<String> {
    let mut ids: Vec<String> = hub.feeds().into_iter().map(|(id, _)| id).collect();
    ids.sort();
    ids
}

#[test]
fn each_browser_gets_a_feed_of_its_own() {
    let (run, hub) = hub_server();
    let mut a = run.browser("pixel-aaaaaaaa");
    a.recv_json();
    let mut b = run.browser("iphone-bbbbbbbb");
    b.recv_json();
    assert_eq!(ids(&hub), ["iphone-bbbbbbbb", "pixel-aaaaaaaa"]);
    assert!(hub.feeds().iter().all(|(_, f)| f.connected()));
    a.offer("offer-sdp");
    run.last().video(&[0, 0, 0, 1, 0x65], 10);
    let (pixel, iphone) = (hub.feed("pixel-aaaaaaaa").unwrap(), hub.feed("iphone-bbbbbbbb").unwrap());
    let took = |feed: &BrowserFeed| feed.next_frame(feed.generation(), Duration::from_millis(50)).map(|f: Frame| f.data);
    assert!(wait(|| took(&pixel).is_some(), FIVE));
    assert!(took(&iphone).is_none());

    let mut again = run.browser("pixel-aaaaaaaa"); // a reload: the same browser, not a third
    again.recv_json();
    assert_eq!(a.next_close(), CLOSE_REPLACED);
    assert_eq!(hub.feeds().len(), 2);
}

#[test]
fn a_browser_with_no_room_is_told_every_camera_is_taken() {
    let (run, hub) = hub_server();
    let mut kept: Vec<Browser> = (0..2).map(|n| run.browser(&format!("browser-{n}0000000"))).collect();
    for k in &mut kept {
        k.recv_json();
    }
    let mut late = run.browser("browser-20000000");
    assert_eq!(late.next_close(), CLOSE_FULL);
    assert!(!ids(&hub).contains(&"browser-20000000".to_string()));
}

#[test]
fn the_page_hears_which_camera_it_streams_to() {
    let (run, hub) = hub_server();
    let mut b = run.browser("pixel-aaaaaaaa");
    assert!(b.recv_json().get("camera").is_none());
    hub.feed("pixel-aaaaaaaa").unwrap().set_camera("Phone Camera 2");
    assert_eq!(b.recv_json()["camera"], "Phone Camera 2");
    drop(b);
    assert!(wait(|| !hub.feed("pixel-aaaaaaaa").unwrap().connected(), FIVE));
    hub.drop_feed("pixel-aaaaaaaa");
    assert!(hub.feeds().is_empty());
}

#[test]
fn a_new_link_drops_the_browser_and_the_old_token() {
    let run = server();
    let feed = run.feed();
    let mut b = run.browser("");
    b.recv_json();
    let old = run.server.token();
    run.server.new_token();
    assert!(wait(|| !feed.connected(), FIVE));
    assert_eq!(b.next_close(), CLOSE_NEW_LINK); // ended by its own thread, with the code that tells the page to stop
    assert_eq!(Browser::connect(run.server.port(), &old, "").status, 403);
}

#[test]
fn capture_changes_reach_a_connected_page() {
    let run = server();
    let mut b = run.browser("");
    b.recv_json();
    run.feed().set_capture(1920, 1080, 24);
    assert_eq!(b.recv_json(), json!({"type": "config", "width": 1920, "height": 1080, "fps": 24, "audio": false,
                                     "max_kbps": max_kbps_for(1920, 1080, 24)}));
}

#[test]
fn mic_is_refused_with_the_browsers_reason() {
    let run = server();
    let feed = run.feed();
    let mut b = run.browser("");
    b.recv_json();
    b.text(json!({"type": "hello", "device": "x", "mic_error": "The browser didn't allow the microphone."}));
    assert!(wait(|| !feed.mic_error().is_empty(), FIVE));
    assert_eq!(feed.open_audio().err().as_deref(), Some("The browser didn't allow the microphone."));
}

#[test]
fn stop_ends_a_connected_browser() {
    let run = server();
    let feed = run.feed();
    let mut b = run.browser("");
    b.recv_json();
    b.offer("offer-sdp");
    let rtc = run.last();
    run.server.stop();
    assert!(wait(|| !feed.connected(), FIVE));
    assert_eq!(b.next_close(), 1001);
    assert!(wait(|| rtc.closed(), FIVE));
}

// ── Into the pipeline ────────────────────────────────────────────────────────

#[test]
fn the_feed_hands_a_stream_every_frame_in_order() {
    let run = server();
    let mut b = run.browser("");
    b.recv_json();
    b.offer("offer-sdp");
    let rtc = run.last();
    let (codec, mut video) = BrowserVideo::open(run.feed()).unwrap();
    assert_eq!(codec, Codec::H264);
    for n in 0..5u8 {
        rtc.video(&[0, 0, 0, 1, 0x41, n], 1000);
    }
    let got: Vec<u8> = (0..5).map(|_| video.read().unwrap().data[5]).collect();
    assert_eq!(got, [0, 1, 2, 3, 4]);
    let stopper = video.stopper();
    stopper.stop();
    assert!(video.read().is_err());
}

#[test]
fn the_browsers_mic_comes_out_as_it_went_in() {
    let run = server();
    let mut b = run.browser("");
    b.recv_json();
    b.offer("offer-sdp");
    let rtc = run.last();
    let mut mic: BrowserAudio = run.feed().open_audio().unwrap();
    assert_eq!(b.recv_json()["audio"], true);
    let tone: Vec<u8> = (0..4800).flat_map(|i| (((i as f64 / 10.0).sin() * 10000.0) as i16).to_le_bytes()).collect();
    for chunk in tone.chunks(1920) {
        rtc.audio(chunk);
    }
    let mut heard = Vec::new();
    while heard.len() < tone.len() {
        heard.extend(mic.read().unwrap().data);
    }
    assert_eq!(heard, tone);
}

#[test]
fn reader_waits_for_a_browser_and_says_what_its_waiting_for() {
    let feed = Arc::new(BrowserFeed::new());
    let started = Instant::now();
    assert!(BrowserVideo::open(feed).is_none());
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(super::plugin::waiting_text().contains("Browser camera"));
}

#[test]
fn a_reader_that_falls_behind_starts_again_at_a_keyframe() {
    // Frames are still encoded here, so none can be skipped on its own: a reader that falls far behind starts again
    // at the next keyframe, and the browser is asked for one
    let feed = Arc::new(BrowserFeed::new());
    let gen = feed.connect();
    feed.note_rtc(gen, "H264", "");
    let (_, mut reader) = BrowserVideo::open(feed.clone()).unwrap();
    for n in 0..200u32 {
        feed.put_video(gen, Codec::H264, vec![0, 0, 0, 1, 0x41, n as u8], 0);
    }
    assert!(feed.take_keyframe_wish());
    feed.put_video(gen, Codec::H264, vec![0, 0, 0, 1, 0x65, 9], 0);
    let first = reader.read().unwrap().data;
    assert_eq!(first, [0, 0, 0, 1, 0x65, 9], "the old frames went and the keyframe is next");
    assert!(feed.next_frame(gen, Duration::from_millis(50)).is_none()); // nothing newer
}
