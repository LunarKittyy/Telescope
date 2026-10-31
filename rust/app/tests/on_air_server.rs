// The sign over a real HTTPS server, as a device reaches it (test_on_air.py's OnAirServer tests): the real TLS listener,
// header parsing and answers, with On Air's own page handler behind them.
use base64::Engine;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use serde_json::{json, Value};
use telescope_core::testing::scratch_dir;
use telescope_core::web::{Serve, Step, Web, WebServer};
use telescope_effects::on_air::Sign;
use telescope_link::https::{fingerprint, PhoneAuth, Request};
use telescope_link::web::SystemWeb;

struct Running {
    sign: Arc<Sign>,
    server: Box<dyn WebServer>,
    pin: String,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.sign.stop();
        self.server.stop();
    }
}

fn server() -> Running {
    let certificate = SystemWeb.certificate(&scratch_dir("on-air-server"), &["127.0.0.1".to_string()]).unwrap();
    let sign = Arc::new(Sign::new(&"a".repeat(24)));
    let serve = Serve { port: 0, host: "127.0.0.1".into(), label: "On Air".into() };
    let server = SystemWeb.serve(&certificate, &serve, sign.clone()).unwrap();
    let pem = std::fs::read_to_string(&certificate.cert).unwrap();
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let pin = fingerprint(&base64::engine::general_purpose::STANDARD.decode(body).unwrap());
    Running { sign, server, pin }
}

/// (status, content type, body, Content-Security-Policy), as _get reads them off the wire.
type Got = (u16, String, Vec<u8>, String);

fn fetch(port: u16, pin: &str, path: &str, token: &str) -> Got {
    let url = format!("https://127.0.0.1:{port}{path}");
    let mut request = Request::get(&url, Duration::from_secs(25));
    if token.is_empty() {
        request = request.without_token();
    }
    let mut response = PhoneAuth::new(token, pin).open_any(request).unwrap();
    let csp = response.header("Content-Security-Policy").unwrap_or("").to_string();
    let content_type = response.content_type().to_string();
    let body = response.read_capped(1 << 20).unwrap();
    (response.status, content_type, body, csp)
}

impl Running {
    fn get(&self, path: &str, token: &str) -> Got {
        fetch(self.server.port(), &self.pin, path, token)
    }

    /// The same request on a thread of its own, for one that's held.
    fn get_later(&self, path: &'static str, token: String) -> mpsc::Receiver<Got> {
        let (port, pin) = (self.server.port(), self.pin.clone());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(fetch(port, &pin, path, &token));
        });
        rx
    }
}

fn parsed(got: &Got) -> Value {
    serde_json::from_slice(&got.2).unwrap()
}

#[test]
fn serves_the_sign_and_its_script_and_nothing_else() {
    let s = server();
    let (status, content_type, body, csp) = s.get("/", "");
    assert_eq!(status, 200);
    assert!(content_type.starts_with("text/html"), "{content_type}");
    assert!(String::from_utf8_lossy(&body).contains("/on-air.js"));
    assert!(csp.contains("default-src 'self'"), "{csp}");
    assert_eq!(s.get("/on-air.js", "").0, 200);
    // None of the Browser camera's
    for path in ["/app.js", "/ws", "/../on_air.py", "/index.html"] {
        assert_eq!(s.get(path, "").0, 404, "{path}");
    }
}

#[test]
fn the_state_needs_the_token() {
    let s = server();
    s.sign.publish(json!({"on": true, "text": "On air"}));
    assert_eq!(s.get("/state?after=-1", "").0, 403);
    assert_eq!(s.get("/state?after=-1", &"b".repeat(24)).0, 403);
    let got = s.get("/state?after=-1", &s.sign.token());
    assert_eq!((got.0, got.1.as_str()), (200, "application/json"));
    assert_eq!(parsed(&got), json!({"on": true, "text": "On air", "seq": 1}));
    assert_eq!(s.server.stats().count(Step::Refused), 2);
}

#[test]
fn a_page_waits_for_the_next_change() {
    let s = server();
    s.sign.publish(json!({"on": false}));
    let waiting = s.get_later("/state?after=1", s.sign.token());
    assert!(waiting.recv_timeout(Duration::from_millis(300)).is_err(), "nothing new yet: the request is held");
    s.sign.publish(json!({"on": true}));
    let got = waiting.recv_timeout(Duration::from_secs(3)).expect("the change ends the wait");
    assert_eq!(parsed(&got), json!({"on": true, "seq": 2}));
}

#[test]
fn a_new_link_ends_the_wait_and_refuses_the_old_one() {
    let s = server();
    s.sign.publish(json!({"on": false}));
    let old = s.sign.token();
    let waiting = s.get_later("/state?after=1", old.clone());
    assert!(waiting.recv_timeout(Duration::from_millis(300)).is_err());
    s.sign.set_token(&"c".repeat(24));
    waiting.recv_timeout(Duration::from_secs(3)).expect("the new link ends the wait");
    assert_eq!(s.get("/state?after=1", &old).0, 403);
}
