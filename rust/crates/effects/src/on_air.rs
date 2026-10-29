//! On Air (telescope/on_air.py): whether you're on a call from what Telescope knows, and the two ways of telling the people around
//! you. Debounce turns the raw answer into one that doesn't flap while an app reopens the camera. webhook_request builds what goes to
//! a smart home hub and WebhookSender sends it off the window's thread. Sign answers a device that keeps the sign page open over
//! HTTPS, with a link of its own that keeps working across restarts; it answers nothing but the page, its script and the state.

use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use telescope_core::json::{dumps, loads, Style};
use telescope_core::web::{PageHandler, Reply, Request, ServerStats, Step};

pub const ON_AIR_PORT: u16 = 8768;
/// The raw answer has to hold this long before the sign goes on.
pub const ON_AFTER_S: f64 = 1.0;
/// ...and this long before it goes off (apps close and reopen the camera when settings change).
pub const OFF_AFTER_S: f64 = 6.0;
pub const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(5);
/// A failed state is sent again after these, the last one repeating until it works.
pub const RETRY_S: [u64; 4] = [5, 15, 60, 300];
/// How long a page's request for the state waits for a change before answering anyway.
const POLL_WAIT: Duration = Duration::from_secs(20);

pub const TRIGGERS: [&str; 4] = ["camera", "camera_mic", "mic", "streaming"];
pub const DEFAULT_TRIGGER: &str = "camera";
pub const METHODS: [&str; 2] = ["POST", "GET"];

const PAGE: &[u8] = include_bytes!("../web/on-air.html");
const SCRIPT: &[u8] = include_bytes!("../web/on-air.js");

/// The raw answer: `watched` is an app reading the virtual camera, the mic counts while it streams unmuted.
pub fn is_on_air(trigger: &str, watched: bool, streaming: bool, mic_enabled: bool, mic_muted: bool) -> bool {
    let mic = streaming && mic_enabled && !mic_muted;
    match trigger {
        "mic" => mic,
        "streaming" => streaming,
        "camera_mic" => watched || mic,
        _ => watched,
    }
}

/// A raw on or off only counts once it has held for a moment: `on_after` going on, `off_after` going off.
#[derive(Clone, Debug)]
pub struct Debounce {
    pub on_after: f64,
    pub off_after: f64,
    pub state: bool,
    raw: bool,
    since: f64,
}

impl Default for Debounce {
    fn default() -> Debounce {
        Debounce::new(ON_AFTER_S, OFF_AFTER_S)
    }
}

impl Debounce {
    pub fn new(on_after: f64, off_after: f64) -> Debounce {
        Debounce { on_after, off_after, state: false, raw: false, since: 0.0 }
    }

    pub fn feed(&mut self, raw: bool, now: f64) {
        if raw != self.raw {
            self.raw = raw;
            self.since = now;
        }
    }

    /// Seconds until the raw answer counts, or None when it already does.
    pub fn due_in(&self, now: f64) -> Option<f64> {
        if self.raw == self.state {
            return None;
        }
        let wait = if self.raw { self.on_after } else { self.off_after };
        Some((self.since + wait - now).max(0.0))
    }

    /// Takes the raw answer once it has held long enough; true when the state changed.
    pub fn settle(&mut self, now: f64) -> bool {
        match self.due_in(now) {
            Some(left) if left <= 0.0 => {
                self.state = self.raw;
                true
            }
            _ => false,
        }
    }
}

// ── Webhook ──────────────────────────────────────────────────────────────────

pub fn default_payload(method: &str, on: bool) -> String {
    if method == "GET" {
        format!("on_air={}", if on { "true" } else { "false" })
    } else {
        format!("{{\"on_air\": {}}}", if on { "true" } else { "false" })
    }
}

/// urllib.parse.urlsplit's pieces of a URL: (scheme in lower case, netloc, path, query, fragment). None when Python would raise
/// (a bracketed address that isn't one, which is what "http://[" is while an IPv6 address is being typed).
fn urlsplit(url: &str) -> Option<(String, String, String, String, String)> {
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let url: String = url.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let mut rest = url.as_str();
    let mut scheme = String::new();
    if let Some(i) = rest.find(':') {
        let head = &rest[..i];
        if !head.is_empty() && head.starts_with(|c: char| c.is_ascii_alphabetic())
            && head.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            scheme = head.to_ascii_lowercase();
            rest = &rest[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..end].to_string();
        rest = &after[end..];
        let open = netloc.contains('[');
        let close = netloc.contains(']');
        if open != close {
            return None;
        }
        if open {
            // The address inside the brackets has to be an IPv6 one (or an IPvFuture)
            let inner = netloc.split_once('[').and_then(|(_, r)| r.split_once(']')).map(|(inside, _)| inside)?;
            let future = inner.starts_with(['v', 'V']);
            let address = inner.split('%').next().unwrap_or("");
            if !future && address.parse::<std::net::Ipv6Addr>().is_err() {
                return None;
            }
        }
    }
    let (rest, fragment) = rest.split_once('#').map_or((rest, ""), |(a, b)| (a, b));
    let (path, query) = rest.split_once('?').map_or((rest, ""), |(a, b)| (a, b));
    Some((scheme, netloc, path.to_string(), query.to_string(), fragment.to_string()))
}

/// The host part of a netloc, as `SplitResult.hostname` has it (None for none).
fn hostname(netloc: &str) -> Option<String> {
    let info = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let host = match info.split_once('[') {
        Some((_, bracketed)) => bracketed.split_once(']').map_or(bracketed, |(h, _)| h),
        None => info.split_once(':').map_or(info, |(h, _)| h),
    };
    if host.is_empty() {
        return None;
    }
    let (name, zone) = host.split_once('%').map_or((host, None), |(n, z)| (n, Some(z)));
    Some(match zone {
        Some(z) => format!("{}%{z}", name.to_lowercase()),
        None => name.to_lowercase(),
    })
}

/// str.isprintable()'s idea of a character that can't be in an address: whitespace, controls and the invisible formatting ones.
fn unprintable(c: char) -> bool {
    c.is_whitespace() || c.is_control() || matches!(c, '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}' | '\u{200b}'..='\u{200f}'
        | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{e000}'..='\u{f8ff}')
}

/// Why `url` can't be used as a webhook (English, for the log and tests), or "" when it can.
pub fn webhook_problem(url: &str) -> &'static str {
    if url.is_empty() {
        return "no address";
    }
    let Some((scheme, netloc, ..)) = urlsplit(url) else { return "it can't be read as an address" };
    if scheme != "http" && scheme != "https" {
        return "it has to start with http:// or https://";
    }
    if hostname(&netloc).is_none() {
        return "there's no host in it";
    }
    if url.chars().any(unprintable) {
        return "there's a space in it";
    }
    ""
}

/// urllib.parse.quote(text, safe="=&%+"): everything but letters, digits, `_.-~` and the safe ones as %XX of its UTF-8 bytes.
fn quote(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~=&%+".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// What goes to the hub: where, how and with what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebhookRequest {
    /// Without a fragment, which is never sent.
    pub url: String,
    pub method: Method,
    pub body: Option<Vec<u8>>,
    /// A POST's Content-Type.
    pub content_type: Option<String>,
    pub user_agent: String,
}

/// What's sent for on or off: POST sends `payload` as the body (JSON when it parses as JSON), GET adds it to the query. An empty
/// payload means default_payload.
pub fn webhook_request(url: &str, method: &str, payload: &str, on: bool) -> WebhookRequest {
    let method = if METHODS.contains(&method) { method } else { METHODS[0] };
    let trimmed = payload.trim();
    let payload = if trimmed.is_empty() { default_payload(method, on) } else { trimmed.to_string() };
    let user_agent = format!("Telescope/{}", telescope_core::version::version());
    let (scheme, netloc, path, query, _) = urlsplit(url).unwrap_or_default();
    let without_fragment = |query: &str| {
        let mut out = format!("{scheme}://{netloc}{path}");
        if !query.is_empty() {
            out.push('?');
            out.push_str(query);
        }
        out
    };
    if method == "GET" {
        // A space or a quote can't break the request line
        let payload = quote(payload.trim_start_matches(['?', '&']));
        let joined = [query.as_str(), payload.as_str()].into_iter().filter(|q| !q.is_empty()).collect::<Vec<_>>().join("&");
        return WebhookRequest { url: without_fragment(&joined), method: Method::Get, body: None, content_type: None, user_agent };
    }
    let content_type = if loads(&payload).is_ok() { "application/json" } else { "text/plain; charset=utf-8" };
    WebhookRequest { url: without_fragment(&query), method: Method::Post, body: Some(payload.into_bytes()), content_type: Some(content_type.into()), user_agent }
}

/// What a failed send says, short and never the address (it can hold a secret): Python's strerror for the things that go wrong on
/// a network, so "Connection refused" reads the same.
fn failure_text(e: &ureq::Error) -> String {
    use std::io::ErrorKind;
    let os_text = |e: &std::io::Error| {
        let text = e.to_string();
        match text.rsplit_once(" (os error ") {
            Some((words, _)) => words.to_string(),
            None if e.kind() == ErrorKind::TimedOut => "timed out".into(),
            None => text,
        }
    };
    match e {
        ureq::Error::StatusCode(code) => format!("HTTP {code}"),
        // A POST can't follow a 307 or 308 on, which urllib raises as the status itself
        ureq::Error::RedirectFailed => "HTTP 307".into(),
        ureq::Error::Timeout(_) => "timed out".into(),
        ureq::Error::HostNotFound => if cfg!(windows) { "getaddrinfo failed" } else { "Name or service not known" }.into(),
        ureq::Error::Io(io) => match io.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>()) {
            Some(tls) => tls.to_string(),
            None => os_text(io),
        },
        ureq::Error::Rustls(tls) => tls.to_string(),
        ureq::Error::ConnectionFailed => "couldn't connect".into(),
        // Their text can quote the address
        ureq::Error::BadUri(_) | ureq::Error::Http(_) => "InvalidURL".into(),
        _ => "HTTPException".into(),
    }
}

/// Sends it; "" when the other end took it, else a short reason (English, never the address: it can hold a secret).
pub fn send_webhook(request: &WebhookRequest) -> String {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .user_agent(request.user_agent.clone())
        .timeout_resolve(Some(WEBHOOK_TIMEOUT))
        .timeout_connect(Some(WEBHOOK_TIMEOUT))
        .timeout_send_request(Some(WEBHOOK_TIMEOUT))
        .timeout_recv_response(Some(WEBHOOK_TIMEOUT))
        .timeout_recv_body(Some(WEBHOOK_TIMEOUT))
        .build()
        .into();
    let sent = match (request.method, &request.body) {
        (Method::Post, Some(body)) => {
            let mut post = agent.post(&request.url);
            if let Some(kind) = &request.content_type {
                post = post.header("Content-Type", kind);
            }
            post.send(body.as_slice())
        }
        _ => agent.get(&request.url).call(),
    };
    match sent {
        Ok(mut response) => {
            use std::io::Read;
            let mut sink = Vec::new();
            let _ = response.body_mut().as_reader().take(4096).read_to_end(&mut sink);
            String::new()
        }
        Err(e) => failure_text(&e),
    }
}

type SendFn = Arc<dyn Fn(&WebhookRequest) -> String + Send + Sync>;
type ResultFn = Arc<dyn Fn(&str, &str) + Send + Sync>;

#[derive(Default)]
struct Queue {
    /// slot: (request, tag), oldest first.
    pending: Vec<(String, WebhookRequest, String)>,
    busy: bool,
}

/// Sends requests one at a time on a thread of its own. Each slot holds one waiting request and a newer one replaces it, so a quick
/// on-off-on only sends what's true at the end and a test never takes the place of a state. on_result(tag, error) runs on that
/// thread ("" error: it worked).
#[derive(Clone)]
pub struct WebhookSender {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    on_result: ResultFn,
    send: SendFn,
}

impl WebhookSender {
    pub fn new(on_result: impl Fn(&str, &str) + Send + Sync + 'static) -> WebhookSender {
        WebhookSender::with_send(on_result, send_webhook)
    }

    pub fn with_send(on_result: impl Fn(&str, &str) + Send + Sync + 'static,
                     send: impl Fn(&WebhookRequest) -> String + Send + Sync + 'static) -> WebhookSender {
        WebhookSender { queue: Arc::default(), on_result: Arc::new(on_result), send: Arc::new(send) }
    }

    /// `slot` names where it waits (a tag by default).
    pub fn send(&self, request: WebhookRequest, tag: &str, slot: Option<&str>) {
        let slot = slot.unwrap_or(tag).to_string();
        {
            let mut queue = self.queue.0.lock().unwrap_or_else(|e| e.into_inner());
            match queue.pending.iter_mut().find(|(s, ..)| *s == slot) {
                Some(waiting) => *waiting = (slot, request, tag.to_string()),
                None => queue.pending.push((slot, request, tag.to_string())),
            }
            if queue.busy {
                return;
            }
            queue.busy = true;
        }
        let sender = self.clone();
        let spawned = std::thread::Builder::new().name("on-air-webhook".into()).spawn(move || sender.run());
        if let Err(e) = spawned {
            log::warn!("On Air webhook couldn't get a thread: {e}");
            self.queue.0.lock().unwrap_or_else(|e| e.into_inner()).busy = false;
        }
    }

    /// Waits up to `timeout` for everything sent to be through; false if it isn't.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (lock, cond) = &*self.queue;
        let mut queue = lock.lock().unwrap_or_else(|e| e.into_inner());
        while queue.busy {
            let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else { return false };
            queue = cond.wait_timeout(queue, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        true
    }

    fn run(&self) {
        loop {
            let (_, request, tag) = {
                let (lock, cond) = &*self.queue;
                let mut queue = lock.lock().unwrap_or_else(|e| e.into_inner());
                if queue.pending.is_empty() {
                    queue.busy = false;
                    cond.notify_all();
                    return;
                }
                queue.pending.remove(0)
            };
            let sent = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.send)(&request)));
            let error = sent.unwrap_or_else(|_| {
                // No details: they would show the address
                log::warn!("On Air webhook failed");
                "InternalError".into()
            });
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.on_result)(&tag, &error))).is_err() {
                log::error!("On Air webhook result callback failed");
            }
        }
    }
}

// ── The sign page ────────────────────────────────────────────────────────────

struct SignState {
    token: String,
    /// What pages show, as a JSON object with its "seq".
    state: Map<String, Value>,
}

/// Serves the sign. publish(state) hands it what pages show (a JSON object, with "seq" added); a page's request for the state waits
/// until it changes, so the sign updates as soon as it does.
pub struct Sign {
    inner: Mutex<SignState>,
    changed: Condvar,
    stopping: AtomicBool,
    poll_wait: Duration,
}

/// hmac.compare_digest
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// int() of a query value: spaces around it, a sign and underscores between digits are fine.
fn py_int(text: &str) -> Option<i64> {
    let text = text.trim();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let ok = !digits.is_empty() && !digits.starts_with('_') && !digits.ends_with('_') && !digits.contains("__")
        && digits.bytes().all(|b| b.is_ascii_digit() || b == b'_');
    ok.then(|| digits.replace('_', "").parse::<i64>().ok().map(|n| sign * n)).flatten()
}

/// parse_qs's reading of a value: plus is a space and %XX is a byte.
fn query_value(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if bytes.get(i + 1..i + 3).is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) => {
                out.push(u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("0"), 16).unwrap_or(b'%'));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Sign {
    pub fn new(token: &str) -> Sign {
        Sign::with_wait(token, POLL_WAIT)
    }

    /// With another time for a page's request to wait (tests).
    pub fn with_wait(token: &str, poll_wait: Duration) -> Sign {
        let state = Map::from_iter([("seq".to_string(), json!(0))]);
        Sign { inner: Mutex::new(SignState { token: token.into(), state }), changed: Condvar::new(), stopping: AtomicBool::new(false), poll_wait }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SignState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn token(&self) -> String {
        self.lock().token.clone()
    }

    pub fn token_ok(&self, token: &str) -> bool {
        !token.is_empty() && same(token, &self.lock().token)
    }

    /// A new link: pages on the old one get refused from their next request on.
    pub fn set_token(&self, token: &str) {
        self.lock().token = token.into();
        self.changed.notify_all();
    }

    pub fn publish(&self, state: Value) {
        let mut inner = self.lock();
        let seq = inner.state.get("seq").and_then(Value::as_i64).unwrap_or(0) + 1;
        let mut next = state.as_object().cloned().unwrap_or_default();
        next.insert("seq".into(), json!(seq));
        inner.state = next;
        drop(inner);
        self.changed.notify_all();
    }

    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.changed.notify_all();
    }

    /// The state once its seq isn't `after` any more (right away for a page that hasn't seen it), or as it is after `timeout`. A new
    /// link or stop() also ends the wait.
    pub fn wait_state(&self, after: i64, timeout: Duration) -> Map<String, Value> {
        let deadline = Instant::now() + timeout;
        let mut inner = self.lock();
        let token = inner.token.clone();
        while inner.state.get("seq").and_then(Value::as_i64) == Some(after) && !self.stopping.load(Ordering::SeqCst) && inner.token == token {
            let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else { break };
            inner = self.changed.wait_timeout(inner, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        inner.state.clone()
    }
}

impl PageHandler for Sign {
    fn handle(&self, request: &Request, stats: &ServerStats) -> Reply {
        match request.path() {
            "/state" => {
                let auth = request.header("Authorization").unwrap_or("");
                if !self.token_ok(auth.strip_prefix("Bearer ").unwrap_or("")) {
                    stats.note(Step::Refused);
                    return Reply::text(403, "This link doesn't work any more.");
                }
                let after = request.param("after").map(query_value).and_then(|v| py_int(&v)).unwrap_or(-1);
                let wait = if request.method == "HEAD" { Duration::ZERO } else { self.poll_wait };
                let state = self.wait_state(after, wait);
                let body = dumps(&Value::Object(state), Style { indent: None, sort_keys: false, ensure_ascii: true });
                Reply::new(200, body, "application/json")
            }
            "/" => {
                stats.note(Step::Pages);
                Reply::new(200, PAGE, "text/html; charset=utf-8")
            }
            "/on-air.js" => Reply::new(200, SCRIPT, "text/javascript; charset=utf-8"),
            _ => Reply::text(404, "Not found"),
        }
    }
}

#[cfg(test)]
#[path = "on_air_tests.rs"]
mod tests;
