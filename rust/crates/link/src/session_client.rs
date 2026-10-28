//! The phone's session port (8766), reachable whenever the app is up, unlike the stream port (session_client.py).

use crate::https::{self, Error, PhoneAuth, Request, MAX_REPLY_BYTES};
use crate::util::clean_name;
use serde_json::{json, Map, Value};
use std::time::Duration;

pub const PING_PORT: u16 = 8766;
/// Shape of the phone's session API (its SessionServer.PROTOCOL_VERSION); both apps must match.
/// 3: TLS on both of the phone's ports, pinned to the certificate fingerprint it gave at pairing.
pub const SESSION_PROTOCOL: i64 = 3;
/// Ping timeout: long enough for slow Wi-Fi, short enough for polling.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// Waiting for the camera to come up after the phone accepted a start.
pub const START_TIMEOUT: Duration = Duration::from_secs(12);
pub const START_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Polls this often at first, since most phones are up within a second.
pub const START_FAST_POLL_INTERVAL: Duration = Duration::from_millis(100);
pub const START_FAST_POLL_FOR: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PingStatus {
    Paired,
    NotPaired,
    Unreachable,
}

/// What GET /v1/ping said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ping {
    pub status: PingStatus,
    pub streaming: bool,
    pub busy: bool,
    pub local_only: bool,
    /// Which phone answered, so a USB probe can tell this phone from any other one plugged in.
    pub phone_id: Option<String>,
    pub phone_name: Option<String>,
    /// The other paired computer the phone streams to (or starts for); None when it's this one or nobody.
    pub streaming_for: Option<String>,
    /// Runs Dynamic over UDP from this desktop's reports; an older phone leaves it out.
    pub udp_dynamic: bool,
}

impl Ping {
    pub fn status(status: PingStatus) -> Ping {
        Ping { status, streaming: false, busy: false, local_only: false, phone_id: None, phone_name: None,
               streaming_for: None, udp_dynamic: false }
    }

    pub fn paired(&self) -> bool {
        self.status == PingStatus::Paired
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelloStatus {
    /// A Telescope app answered with its identity.
    Ok,
    /// Something answered on the session port but has no /v1/hello: an app too old.
    Missing,
    /// Nothing answered.
    None,
    /// The phone has Local only on and answers only ping off USB (403).
    UsbOnly,
}

/// What GET /v1/hello said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub status: HelloStatus,
    pub phone_id: String,
    pub phone_name: String,
    pub protocol: i64,
    pub app_version: String,
    pub build: i64,
}

impl Hello {
    pub fn status(status: HelloStatus) -> Hello {
        Hello { status, phone_id: String::new(), phone_name: String::new(), protocol: 0, app_version: String::new(), build: 0 }
    }

    pub fn ok(&self) -> bool {
        self.status == HelloStatus::Ok
    }
}

/// What POST /v1/session said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionResult {
    pub ok: bool,
    pub error: Option<String>,
    /// With busy_other: the computer the phone is streaming to.
    pub computer: Option<String>,
}

impl SessionResult {
    fn ok() -> SessionResult {
        SessionResult { ok: true, error: None, computer: None }
    }

    fn failed(error: &str) -> SessionResult {
        SessionResult { ok: false, error: Some(error.to_string()), computer: None }
    }
}

/// A computer's name from the phone as display text; None when it's missing or empty.
fn computer_name(raw: Option<&Value>) -> Option<String> {
    raw.and_then(Value::as_str).map(clean_name).filter(|n| !n.is_empty())
}

/// What the route resolver asks a phone, so tests can stand in for one.
pub trait SessionApi {
    fn hello(&self, timeout: Duration) -> Hello;
    fn ping(&self) -> Ping;
}

/// Talks to a phone's session port at a base URL (its address, or localhost through an adb forward).
#[derive(Clone, Debug)]
pub struct SessionClient {
    pub base: String,
    pub auth: PhoneAuth,
}

impl SessionClient {
    pub fn new(base: &str, auth: PhoneAuth) -> SessionClient {
        SessionClient { base: base.trim_end_matches('/').to_string(), auth }
    }

    /// Who answers on the session port (no token sent).
    pub fn hello(&self, timeout: Duration) -> Hello {
        let url = format!("{}/v1/hello", self.base);
        let body = match self.auth.json(Request::get(&url, timeout).without_token()) {
            Ok(body) => body,
            // The session server answers 404 for routes it doesn't know: an app from before /v1/hello
            Err(Error::Http(404)) => return Hello::status(HelloStatus::Missing),
            Err(Error::Http(403)) => return Hello::status(HelloStatus::UsbOnly),
            // LEGACY MIGRATION PATH: only a listener that took the connection but not TLS gets a plain look
            Err(e) if https::handshake_failed(&e) && self.answers_plain_http(timeout) => return Hello::status(HelloStatus::Missing),
            Err(_) => return Hello::status(HelloStatus::None),
        };
        let Some(phone_id) = body.get("phoneId").and_then(Value::as_str).filter(|id| !id.is_empty()) else {
            return Hello::status(HelloStatus::None);
        };
        let text = |key: &str| body.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let int = |key: &str| body.get(key).and_then(Value::as_i64).unwrap_or(0);
        Hello { status: HelloStatus::Ok, phone_id: phone_id.to_string(), phone_name: clean_name(&text("phoneName")),
                protocol: int("protocol"), app_version: text("appVersion"), build: int("build") }
    }

    /// LEGACY MIGRATION PATH: whether an app from before TLS answers here, over plain HTTP with nothing that matters sent.
    pub fn answers_plain_http(&self, timeout: Duration) -> bool {
        let Some(rest) = self.base.strip_prefix("https://") else { return false };
        let url = format!("http://{rest}/v1/hello");
        match PhoneAuth::default().open(Request::get(&url, timeout).without_token()) {
            Ok(mut r) => r.read_capped(MAX_REPLY_BYTES).is_ok(),
            Err(Error::Http(_)) => true,
            Err(_) => false,
        }
    }

    /// Whether the token is still paired, and how the phone is (200 paired, 401 not paired, anything else unreachable).
    pub fn ping(&self) -> Ping {
        let url = format!("{}/v1/ping", self.base);
        match self.auth.open(Request::get(&url, REQUEST_TIMEOUT)) {
            Ok(mut r) if r.status == 200 => match r.read_capped(MAX_REPLY_BYTES) {
                Ok(raw) => parse_ping(&raw),
                Err(_) => Ping::status(PingStatus::Unreachable),
            },
            Err(Error::Http(401)) => Ping::status(PingStatus::NotPaired),
            _ => Ping::status(PingStatus::Unreachable),
        }
    }

    /// opening: the size and rate to open at; else the phone's last.
    pub fn start(&self, opening: Option<&Map<String, Value>>) -> SessionResult {
        let mut body = Map::new();
        body.insert("action".into(), json!("start"));
        for (k, v) in opening.into_iter().flatten() {
            body.insert(k.clone(), v.clone());
        }
        self.session(Value::Object(body))
    }

    pub fn stop(&self) -> SessionResult {
        self.session(json!({"action": "stop"}))
    }

    /// Asks the phone to revoke this computer's token (best effort; true if it confirmed).
    pub fn unpair(&self) -> bool {
        let url = format!("{}/v1/unpair", self.base);
        match self.auth.open(Request::post(&url, b"{}", REQUEST_TIMEOUT)) {
            Ok(r) => r.status == 200,
            Err(e) => {
                log::debug!("unpair failed: {e}");
                false
            }
        }
    }

    fn session(&self, body: Value) -> SessionResult {
        let url = format!("{}/v1/session", self.base);
        let payload = crate::util::python_json(&body);
        match self.auth.json(Request::post(&url, payload.as_bytes(), REQUEST_TIMEOUT)) {
            Ok(reply) if truthy(reply.get("ok")) => SessionResult::ok(),
            Ok(reply) if reply.is_object() => {
                let error = reply.get("error").filter(|e| truthy(Some(e))).map(|e| match e {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                });
                SessionResult { ok: false, error: Some(error.unwrap_or_else(|| "refused".into())), computer: computer_name(reply.get("computer")) }
            }
            Ok(_) => SessionResult::failed("unreachable"),
            Err(Error::Http(401)) => SessionResult::failed("not_paired"),
            Err(Error::Http(code)) => SessionResult::failed(&format!("http_{code}")),
            Err(e) => {
                log::debug!("session {} failed: {e}", body["action"]);
                SessionResult::failed("unreachable")
            }
        }
    }
}

impl SessionApi for SessionClient {
    fn hello(&self, timeout: Duration) -> Hello {
        SessionClient::hello(self, timeout)
    }

    fn ping(&self) -> Ping {
        SessionClient::ping(self)
    }
}

/// Python's truthiness for a JSON value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Anything but a JSON object isn't Telescope (or is an app too old to pair with this computer).
pub fn parse_ping(raw: &[u8]) -> Ping {
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(raw) else { return Ping::status(PingStatus::Unreachable) };
    Ping {
        status: PingStatus::Paired,
        streaming: truthy(body.get("streaming")),
        busy: truthy(body.get("busy")),
        local_only: truthy(body.get("localOnly")),
        phone_id: body.get("phoneId").and_then(Value::as_str).map(str::to_string),
        phone_name: body.get("phoneName").and_then(Value::as_str).map(clean_name),
        streaming_for: computer_name(body.get("streamingFor")),
        udp_dynamic: body.get("udpDynamic") == Some(&Value::Bool(true)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::https::testing::*;
    use std::sync::{Arc, Mutex};

    /// A plain-HTTP phone answering every request with (status, body), recording what it got.
    fn phone(status: u16, body: &'static [u8]) -> (Server, SessionClient) {
        let server = serve(None, Arc::new(move |_req| (status, "application/json".into(), body.to_vec())));
        let client = SessionClient::new(&server.base(false), PhoneAuth::new("tok", ""));
        (server, client)
    }

    fn sent(server: &Server) -> Vec<Seen> {
        server.seen.lock().unwrap().clone()
    }

    #[test]
    fn ping_parses_the_phone_state_body() {
        let (server, client) = phone(200, br#"{"protocol": 1, "streaming": true, "busy": false, "localOnly": true}"#);
        let ping = client.ping();
        assert_eq!((ping.status, ping.streaming, ping.busy, ping.local_only), (PingStatus::Paired, true, false, true));
        assert!(ping.paired());
        let seen = sent(&server);
        assert_eq!((seen[0].path.as_str(), seen[0].auth.as_deref()), ("/v1/ping", Some("Bearer tok")));
    }

    #[test]
    fn ping_treats_a_body_that_is_not_telescope_as_unreachable() {
        assert_eq!(phone(200, b"OK").1.ping().status, PingStatus::Unreachable);
    }

    #[test]
    fn ping_maps_401_to_not_paired_and_other_failures_to_unreachable() {
        assert_eq!(phone(401, b"").1.ping().status, PingStatus::NotPaired);
        assert_eq!(phone(500, b"").1.ping().status, PingStatus::Unreachable);
        assert_eq!(SessionClient::new("http://127.0.0.1:1", PhoneAuth::new("tok", "")).ping().status, PingStatus::Unreachable);
    }

    #[test]
    fn ping_survives_a_body_that_is_valid_json_but_not_an_object() {
        assert_eq!(phone(200, b"[1, 2, 3]").1.ping().status, PingStatus::Unreachable);
    }

    #[test]
    fn start_posts_the_action_as_json() {
        let (server, client) = phone(200, br#"{"ok": true}"#);
        assert_eq!(client.start(None), SessionResult::ok());
        let seen = sent(&server);
        assert_eq!((seen[0].method.as_str(), seen[0].path.as_str()), ("POST", "/v1/session"));
        assert_eq!(serde_json::from_slice::<Value>(&seen[0].body).unwrap(), json!({"action": "start"}));
    }

    #[test]
    fn start_can_say_what_to_open_at() {
        let (server, client) = phone(200, br#"{"ok": true}"#);
        let opening = json!({"width": 1280, "height": 720, "fps": 60});
        client.start(opening.as_object());
        assert_eq!(String::from_utf8(sent(&server)[0].body.clone()).unwrap(),
                   r#"{"action": "start", "width": 1280, "height": 720, "fps": 60}"#);
    }

    #[test]
    fn stop_posts_the_stop_action() {
        let (server, client) = phone(200, br#"{"ok": true}"#);
        assert!(client.stop().ok);
        assert_eq!(serde_json::from_slice::<Value>(&sent(&server)[0].body).unwrap(), json!({"action": "stop"}));
    }

    #[test]
    fn an_http_error_is_reported_with_its_code() {
        assert_eq!(phone(404, b"").1.start(None), SessionResult::failed("http_404"));
    }

    #[test]
    fn a_refusal_body_carries_the_phone_s_reason_through() {
        let (_s, client) = phone(200, br#"{"ok": false, "error": "no_camera_permission"}"#);
        assert_eq!(client.start(None), SessionResult::failed("no_camera_permission"));
    }

    #[test]
    fn a_stream_another_computer_has_comes_back_with_its_name() {
        let (_s, client) = phone(200, br#"{"ok": false, "error": "busy_other", "computer": "Office <PC>\n"}"#);
        assert_eq!(client.start(None), SessionResult { ok: false, error: Some("busy_other".into()), computer: Some("Office PC".into()) });
    }

    #[test]
    fn ping_says_which_other_computer_the_phone_streams_to() {
        assert_eq!(phone(200, br#"{"streaming": true, "streamingFor": "Office PC"}"#).1.ping().streaming_for.as_deref(), Some("Office PC"));
        assert_eq!(phone(200, br#"{"streaming": true}"#).1.ping().streaming_for, None);
    }

    #[test]
    fn ping_says_whether_the_phone_runs_dynamic_over_udp() {
        assert!(phone(200, br#"{"udpDynamic": true}"#).1.ping().udp_dynamic);
        assert!(!phone(200, br#"{"streaming": true}"#).1.ping().udp_dynamic);
    }

    #[test]
    fn transport_and_auth_failures_are_distinguished() {
        assert_eq!(phone(401, b"").1.start(None), SessionResult::failed("not_paired"));
        assert_eq!(phone(503, b"").1.start(None), SessionResult::failed("http_503"));
        assert_eq!(SessionClient::new("http://127.0.0.1:1", PhoneAuth::new("tok", "")).start(None), SessionResult::failed("unreachable"));
    }

    #[test]
    fn an_ok_false_body_without_a_reason_still_fails_closed() {
        let result = phone(200, br#"{"ok": false}"#).1.start(None);
        assert!(!result.ok);
        assert_eq!(result.error.as_deref(), Some("refused"));
    }

    #[test]
    fn a_base_url_with_a_trailing_slash_does_not_double_up() {
        let (server, _) = phone(200, b"OK");
        SessionClient::new(&format!("{}/", server.base(false)), PhoneAuth::new("tok", "")).ping();
        assert_eq!(sent(&server)[0].path, "/v1/ping");
    }

    #[test]
    fn hello_reads_identity_and_version() {
        let (server, client) = phone(200, br#"{"protocol": 2, "phoneId": "p1", "phoneName": "Pixel", "appVersion": "0.5.0", "build": 60}"#);
        assert_eq!(client.hello(REQUEST_TIMEOUT), Hello { status: HelloStatus::Ok, phone_id: "p1".into(), phone_name: "Pixel".into(),
                                                          protocol: 2, app_version: "0.5.0".into(), build: 60 });
        assert_eq!(sent(&server)[0].auth, None);
    }

    #[test]
    fn hello_from_an_app_without_version_fields_still_identifies_it() {
        let hello = phone(200, br#"{"protocol": 2, "phoneId": "p1", "phoneName": "Pixel"}"#).1.hello(REQUEST_TIMEOUT);
        assert!(hello.ok() && hello.phone_id == "p1" && hello.app_version.is_empty() && hello.build == 0);
    }

    #[test]
    fn hello_tells_an_old_app_from_nothing_at_all() {
        assert_eq!(phone(404, b"").1.hello(REQUEST_TIMEOUT).status, HelloStatus::Missing);
        assert_eq!(SessionClient::new("http://127.0.0.1:1", PhoneAuth::new("tok", "")).hello(REQUEST_TIMEOUT).status, HelloStatus::None);
        assert_eq!(phone(200, b"[1]").1.hello(REQUEST_TIMEOUT).status, HelloStatus::None);
    }

    #[test]
    fn hello_refused_off_usb_means_local_only() {
        assert_eq!(phone(403, b"").1.hello(REQUEST_TIMEOUT).status, HelloStatus::UsbOnly);
    }

    #[test]
    fn an_oversized_reply_is_treated_as_no_phone() {
        let mut huge = br#"{"phoneId": "p1", "pad": ""#.to_vec();
        huge.extend(std::iter::repeat_n(b'x', MAX_REPLY_BYTES + 10));
        huge.extend(br#""}"#);
        let huge: &'static [u8] = Box::leak(huge.into_boxed_slice());
        let (_s, client) = phone(200, huge);
        assert_eq!(client.hello(REQUEST_TIMEOUT).status, HelloStatus::None);
        assert_eq!(client.ping().status, PingStatus::Unreachable);
        assert_eq!(client.start(None), SessionResult::failed("unreachable"));
    }

    #[test]
    fn a_name_from_the_phone_arrives_cleaned() {
        let hello = phone(200, br#"{"protocol": 3, "phoneId": "p1", "phoneName": "<img src=x>Pixel"}"#).1.hello(REQUEST_TIMEOUT);
        assert_eq!(hello.phone_name, "img src=xPixel");
    }

    #[test]
    fn an_address_nobody_answers_on_costs_one_timeout_not_two() {
        // Nothing listening: no plain-HTTP look follows, so it's just None
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let client = SessionClient::new(&format!("https://127.0.0.1:{port}"), PhoneAuth::new("tok", &"ab".repeat(32)));
        assert_eq!(client.hello(Duration::from_millis(500)).status, HelloStatus::None);
    }

    #[test]
    fn the_look_for_an_old_app_skips_a_configured_proxy() {
        // This client never reads proxy settings; a closed port reads as no old app
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let client = SessionClient::new(&format!("https://127.0.0.1:{port}"), PhoneAuth::new("tok", &"ab".repeat(32)));
        assert!(!client.answers_plain_http(Duration::from_secs(1)));
    }

    #[test]
    fn a_listener_that_answers_with_something_other_than_http_is_not_a_phone() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let _ = conn.write_all(b"\x15\x03\x03\x00\x02\x02\x28");
        });
        let client = SessionClient::new(&format!("https://127.0.0.1:{port}"), PhoneAuth::new("tok", &"ab".repeat(32)));
        assert!(!client.answers_plain_http(Duration::from_secs(2)));
    }

    /// Answers like a phone's session port: hello names ph-1, ping needs a token.
    fn phone_handler() -> Handler {
        json_handler(r#"{"protocol": 3, "phoneId": "ph-1", "phoneName": "Pixel"}"#)
    }

    #[test]
    fn the_real_phone_is_found_over_tls() {
        let server = serve(Some("phone"), phone_handler());
        let client = SessionClient::new(&server.base(true), PhoneAuth::new("secret", &pin_of("phone")));
        let hello = client.hello(REQUEST_TIMEOUT);
        assert!(hello.ok() && hello.phone_id == "ph-1");
        assert!(client.ping().paired());
        assert_eq!(sent(&server).last().unwrap().auth.as_deref(), Some("Bearer secret"));
    }

    #[test]
    fn an_app_from_before_tls_reads_as_outdated_without_being_sent_the_token() {
        let server = serve(None, phone_handler());
        let client = SessionClient::new(&format!("https://127.0.0.1:{}", server.port), PhoneAuth::new("secret", &pin_of("phone")));
        assert_eq!(client.hello(Duration::from_secs(1)).status, HelloStatus::Missing);
        assert!(sent(&server).iter().all(|s| s.auth.is_none()));
    }

    #[test]
    fn unpair_posts_an_empty_object() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let record = seen.clone();
        let server = serve(None, Arc::new(move |req| {
            record.lock().unwrap().push(String::from_utf8_lossy(&req.body).into_owned());
            (200, "application/json".into(), br#"{"ok": true}"#.to_vec())
        }));
        assert!(SessionClient::new(&server.base(false), PhoneAuth::new("tok", "")).unpair());
        assert_eq!(seen.lock().unwrap().as_slice(), ["{}"]);
    }
}
