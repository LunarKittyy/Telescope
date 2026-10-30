use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use telescope_core::web::ServerStats;

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

// ── What counts as on air ────────────────────────────────────────────────────

#[test]
fn each_trigger_reads_what_it_says() {
    // (trigger, watched, streaming, (mic enabled, muted), expected)
    let cases = [
        ("camera", true, false, (false, false), true), // a call watching the wait screen counts
        ("camera", false, true, (true, false), false),
        ("camera_mic", false, true, (true, false), true),
        ("camera_mic", false, true, (true, true), false), // muted
        ("camera_mic", false, false, (true, false), false), // the mic only counts while it streams
        ("mic", true, true, (false, false), false),
        ("mic", false, true, (true, false), true),
        ("streaming", false, true, (false, false), true),
        ("streaming", true, false, (true, false), false),
    ];
    for (trigger, watched, streaming, (enabled, muted), expected) in cases {
        assert_eq!(is_on_air(trigger, watched, streaming, enabled, muted), expected, "{trigger} {watched} {streaming} {enabled} {muted}");
    }
    assert!(is_on_air("moon", true, true, true, false), "anything else is the camera");
}

#[test]
fn going_on_waits_a_moment_and_a_blip_never_counts() {
    let mut d = Debounce::new(1.0, 6.0);
    d.feed(true, 0.0);
    assert!(d.due_in(0.0) == Some(1.0) && !d.settle(0.5));
    d.feed(false, 0.6); // gone before it counted
    assert!(d.due_in(0.7).is_none() && !d.settle(5.0) && !d.state);
    d.feed(true, 10.0);
    assert!(d.settle(11.0) && d.state);
    assert!(d.due_in(11.0).is_none());
}

#[test]
fn going_off_waits_longer_so_an_app_reopening_the_camera_doesnt_flap() {
    let mut d = Debounce::new(1.0, 6.0);
    d.feed(true, 0.0);
    d.settle(1.0);
    d.feed(false, 2.0);
    assert!(!d.settle(7.9));
    assert!(approx(d.due_in(7.0).unwrap(), 1.0));
    d.feed(true, 7.5); // back before the off counted: nothing changed
    assert!(!d.settle(20.0) && d.state);
    d.feed(false, 30.0);
    assert!(d.settle(36.0) && !d.state);
}

// ── Webhook requests ─────────────────────────────────────────────────────────

#[test]
fn post_sends_json_by_default_and_plain_text_otherwise() {
    let req = webhook_request("http://hub.local/hook", "POST", "", true);
    assert!(req.method == Method::Post && req.url == "http://hub.local/hook");
    assert_eq!(req.body.as_deref(), Some(&b"{\"on_air\": true}"[..]));
    assert_eq!(req.content_type.as_deref(), Some("application/json"));
    let req = webhook_request("http://hub.local/hook", "POST", "busy", false);
    assert_eq!(req.body.as_deref(), Some(&b"busy"[..]));
    assert!(req.content_type.unwrap().starts_with("text/plain"));
    assert!(req.user_agent.starts_with("Telescope/"));
    // What parses as JSON is JSON, however it's spaced; and what Python's json takes that serde's doesn't
    assert_eq!(webhook_request("http://h/x", "POST", "  {\"a\": 1}  ", true).content_type.as_deref(), Some("application/json"));
    assert_eq!(webhook_request("http://h/x", "POST", "NaN", true).content_type.as_deref(), Some("application/json"));
    assert!(webhook_request("http://h/x", "POST", "{a: 1}", true).content_type.unwrap().starts_with("text/plain"));
    assert_eq!(webhook_request("http://h/x", "POST", "", false).body.as_deref(), Some(&b"{\"on_air\": false}"[..]));
    // An unknown method is a POST
    assert_eq!(webhook_request("http://h/x", "PUT", "", true).method, Method::Post);
}

#[test]
fn a_post_never_sends_the_fragment() {
    assert_eq!(webhook_request("http://hub.local/hook?a=1#frag", "POST", "x", true).url, "http://hub.local/hook?a=1");
}

#[test]
fn get_adds_the_payload_to_the_query() {
    let req = webhook_request("http://10.0.0.9/relay/0?x=1#frag", "GET", "turn=on", true);
    assert!(req.method == Method::Get && req.body.is_none() && req.content_type.is_none());
    assert_eq!(req.url, "http://10.0.0.9/relay/0?x=1&turn=on");
    assert_eq!(webhook_request("http://10.0.0.9/r", "GET", "", false).url, "http://10.0.0.9/r?on_air=false");
    assert_eq!(webhook_request("http://10.0.0.9", "GET", "?&a=b", true).url, "http://10.0.0.9?a=b");
}

#[test]
fn a_get_payload_is_encoded_so_it_cant_break_the_request() {
    let req = webhook_request("http://hub.local/x?a=1", "GET", "state=on air&note=\"hi\"", true);
    assert_eq!(req.url, "http://hub.local/x?a=1&state=on%20air&note=%22hi%22");
    assert!(webhook_request("http://hub.local/x", "GET", "light=100%25", true).url.ends_with("light=100%25"));
    assert!(webhook_request("http://hub.local/x", "GET", "name=caf\u{e9}", true).url.ends_with("name=caf%C3%A9"));
}

#[test]
fn only_http_addresses_are_used() {
    for (url, ok) in [
        ("http://hub.local/x", true), ("https://1.2.3.4:8123/api", true), ("HTTP://Hub.Local/x", true), ("http://[::1]:8123/x", true),
        ("", false), ("hub.local/x", false), ("ftp://hub/x", false), ("http://", false), ("http://hub/a b", false), ("http:///x", false),
        ("http://hub/a\u{200b}b", false),
    ] {
        assert_eq!(webhook_problem(url).is_empty(), ok, "{url:?}: {}", webhook_problem(url));
    }
}

#[test]
fn an_address_half_typed_is_a_problem_not_a_crash() {
    assert!(!webhook_problem("http://[").is_empty());
    assert!(!webhook_problem("http://[::1").is_empty());
    assert!(!webhook_problem("http://[not an address]/x").is_empty());
    assert!(webhook_problem("http://[2001:db8::1]/x").is_empty());
}

// ── Sending ──────────────────────────────────────────────────────────────────

/// One request's text and body, as a local server saw it.
#[derive(Clone, Debug)]
struct Seen {
    head: String,
    body: Vec<u8>,
}

/// Answers each request with the next status in `answers` (the last repeating), and records what it was sent.
fn hub(answers: &[u16]) -> (u16, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let (record, answers) = (seen.clone(), answers.to_vec());
    std::thread::spawn(move || {
        for (n, conn) in listener.incoming().flatten().enumerate() {
            let status = answers[n.min(answers.len() - 1)];
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let (mut head, mut length) = (String::new(), 0usize);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
                head.push_str(&line);
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            record.lock().unwrap().push(Seen { head, body });
            let reply = format!("HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            let mut conn: TcpStream = conn;
            let _ = conn.write_all(reply.as_bytes());
        }
    });
    (port, seen)
}

#[test]
fn a_webhook_arrives_as_built_and_a_failure_says_what_went_wrong_without_the_address() {
    let (port, seen) = hub(&[200, 404, 500]);
    let url = format!("http://127.0.0.1:{port}/api/webhook/SECRET");
    let post = webhook_request(&url, "POST", "", true);
    assert_eq!(send_webhook(&post), "");
    {
        let seen = seen.lock().unwrap();
        assert!(seen[0].head.starts_with("POST /api/webhook/SECRET HTTP/1.1"), "{}", seen[0].head);
        assert!(seen[0].head.to_ascii_lowercase().contains("content-type: application/json"));
        assert!(seen[0].head.to_ascii_lowercase().contains("user-agent: telescope/"));
        assert_eq!(seen[0].body, b"{\"on_air\": true}");
    }
    assert_eq!(send_webhook(&post), "HTTP 404");
    assert_eq!(send_webhook(&post), "HTTP 500");
    let get = webhook_request(&url, "GET", "turn=on", true);
    send_webhook(&get);
    assert!(seen.lock().unwrap()[3].head.starts_with("GET /api/webhook/SECRET?turn=on HTTP/1.1"));
}

#[test]
fn a_refused_connection_says_so_in_the_systems_words() {
    // A port nothing listens on
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let said = send_webhook(&webhook_request(&format!("http://127.0.0.1:{port}/api/webhook/SECRET"), "POST", "", true));
    assert_eq!(said, if cfg!(windows) { "No connection could be made because the target machine actively refused it." } else { "Connection refused" });
    assert!(!said.contains("SECRET"));
}

#[test]
fn what_goes_wrong_is_said_without_quoting_the_address() {
    let io = |kind: std::io::ErrorKind, text: &str| ureq::Error::Io(std::io::Error::new(kind, text));
    assert_eq!(failure_text(&ureq::Error::StatusCode(503)), "HTTP 503");
    assert_eq!(failure_text(&ureq::Error::Timeout(ureq::Timeout::Global)), "timed out");
    assert_eq!(failure_text(&ureq::Error::HostNotFound), if cfg!(windows) { "getaddrinfo failed" } else { "Name or service not known" });
    assert_eq!(failure_text(&ureq::Error::Io(std::io::Error::from_raw_os_error(if cfg!(unix) { 113 } else { 10065 }))),
               if cfg!(unix) { "No route to host" } else { "A socket operation was attempted to an unreachable host." });
    assert_eq!(failure_text(&io(std::io::ErrorKind::TimedOut, "timed out")), "timed out");
    assert_eq!(failure_text(&ureq::Error::BadUri("/api/webhook/SECRET".into())), "InvalidURL");
    assert!(!failure_text(&ureq::Error::Other("boom at /api/webhook/SECRET".into())).contains("SECRET"));
}

/// A server that answers one request with `reply`, holds the connection for `then`, and drops it.
fn odd_hub(reply: &'static [u8], then: fn(&mut TcpStream)) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let Ok((mut conn, _)) = listener.accept() else { return };
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let _ = reader.read_exact(&mut vec![0; length]);
        let _ = conn.write_all(reply);
        then(&mut conn);
    });
    port
}

fn sent_to(port: u16) -> String {
    send_webhook_within(&webhook_request(&format!("http://127.0.0.1:{port}/x"), "POST", "", true), Duration::from_secs(1))
}

// Python (urllib, read(4096), timeout 1 s) said: TimeoutError, IncompleteRead, and "" for a body cut short. A reset mid-body is
// "Connection reset by peer" there, but ureq reports it as the body ending, so it reads as a short body here.
#[test]
fn a_body_that_stalls_or_is_garbled_is_a_failed_send() {
    let stall = odd_hub(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nok", |_| std::thread::sleep(Duration::from_secs(3)));
    assert_eq!(sent_to(stall), "TimeoutError");
    let bad_chunk = odd_hub(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nok\r\n", |conn| {
        std::thread::sleep(Duration::from_millis(200));
        let _ = conn.shutdown(std::net::Shutdown::Write);
        std::thread::sleep(Duration::from_millis(200));
    });
    assert_eq!(sent_to(bad_chunk), "IncompleteRead");
    let short = odd_hub(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nok", |conn| {
        std::thread::sleep(Duration::from_millis(200));
        let _ = conn.shutdown(std::net::Shutdown::Write);
        std::thread::sleep(Duration::from_millis(200));
    });
    assert_eq!(sent_to(short), "");
}

#[test]
fn a_failing_send_never_leaks_the_address() {
    let (tx, rx) = std::sync::mpsc::channel();
    let sender = WebhookSender::with_send(move |tag, error| tx.send((tag.to_string(), error.to_string())).unwrap(),
                                          |_| panic!("boom at /api/webhook/SECRET"));
    sender.send(webhook_request("http://hub.local/api/webhook/SECRET", "POST", "", true), "state", None);
    let (tag, error) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(tag, "state");
    assert!(!error.is_empty() && !error.contains("SECRET"));
    assert!(sender.wait_idle(Duration::from_secs(2)), "it carries on after a failure");
}

type Bodies = Arc<Mutex<Vec<String>>>;
type Results = Arc<Mutex<Vec<(String, String)>>>;

/// A sender that holds each send until released, and notes what went out.
fn held_sender() -> (WebhookSender, Bodies, std::sync::mpsc::Sender<()>, Results) {
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let (sent, results): (Bodies, Results) = Default::default();
    let (note, done) = (sent.clone(), results.clone());
    let wait = Mutex::new(wait);
    let sender = WebhookSender::with_send(move |tag, error| done.lock().unwrap().push((tag.into(), error.into())), move |req| {
        note.lock().unwrap().push(String::from_utf8_lossy(req.body.as_deref().unwrap_or(b"")).into_owned());
        let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(2));
        String::new()
    });
    (sender, sent, release, results)
}

fn post(body: &str) -> WebhookRequest {
    webhook_request("http://h/x", "POST", body, true)
}

#[test]
fn a_test_never_takes_the_place_of_a_state() {
    let (sender, sent, release, _) = held_sender();
    sender.send(post("first"), "state:1", Some("state"));
    std::thread::sleep(Duration::from_millis(50));
    sender.send(post("on"), "state:2", Some("state"));
    sender.send(post("test"), "test", None);
    for _ in 0..3 {
        release.send(()).unwrap();
    }
    assert!(sender.wait_idle(Duration::from_secs(2)));
    assert_eq!(*sent.lock().unwrap(), ["first", "on", "test"]);
}

#[test]
fn waiting_for_the_sender_gives_up_at_the_deadline() {
    let (sender, _, release, _) = held_sender();
    sender.send(post(""), "quit", None);
    assert!(!sender.wait_idle(Duration::from_millis(100)));
    release.send(()).unwrap();
    assert!(sender.wait_idle(Duration::from_secs(2)));
    // Nothing sent is nothing to wait for
    assert!(WebhookSender::new(|_, _| {}).wait_idle(Duration::ZERO));
}

#[test]
fn the_sender_only_sends_whats_true_at_the_end() {
    let (sender, sent, release, results) = held_sender();
    sender.send(post("1"), "state", None);
    std::thread::sleep(Duration::from_millis(50));
    sender.send(post("2"), "state", None); // waits behind the first...
    sender.send(post("3"), "state", None); // ...and this replaces it
    for _ in 0..2 {
        release.send(()).unwrap();
    }
    assert!(sender.wait_idle(Duration::from_secs(2)));
    assert_eq!(*sent.lock().unwrap(), ["1", "3"]);
    assert_eq!(*results.lock().unwrap(), [("state".to_string(), String::new()), ("state".to_string(), String::new())]);
}

// ── The sign page ────────────────────────────────────────────────────────────

fn get(sign: &Sign, stats: &ServerStats, target: &str, token: Option<&str>) -> Reply {
    let headers = token.map(|t| vec![("Authorization".to_string(), format!("Bearer {t}"))]).unwrap_or_default();
    sign.handle(&Request { method: "GET".into(), target: target.into(), headers }, stats)
}

fn sign() -> (Arc<Sign>, ServerStats) {
    (Arc::new(Sign::with_wait(&"a".repeat(24), Duration::from_secs(5))), ServerStats::new(8768, 8768, false, "On Air"))
}

#[test]
fn serves_the_sign_and_its_script_and_nothing_else() {
    let (sign, stats) = sign();
    let page = get(&sign, &stats, "/", None);
    assert!(page.status == 200 && page.content_type.starts_with("text/html") && String::from_utf8_lossy(&page.body).contains("/on-air.js"));
    assert_eq!(get(&sign, &stats, "/on-air.js", None).status, 200);
    // None of the Browser camera's
    for path in ["/app.js", "/ws", "/../on_air.py", "/index.html"] {
        assert_eq!(get(&sign, &stats, path, None).status, 404, "{path}");
    }
    assert_eq!(stats.count(Step::Pages), 1);
}

#[test]
fn the_page_and_script_are_the_python_apps() {
    // Until desktop/ goes, a change there has to come here too
    let web = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../desktop/telescope/web");
    if web.exists() {
        assert_eq!(std::fs::read(web.join("on-air.html")).unwrap(), PAGE);
        assert_eq!(std::fs::read(web.join("on-air.js")).unwrap(), SCRIPT);
    }
}

#[test]
fn the_state_needs_the_token() {
    let (sign, stats) = sign();
    sign.publish(json!({"on": true, "text": "On air"}));
    assert_eq!(get(&sign, &stats, "/state?after=-1", None).status, 403);
    assert_eq!(get(&sign, &stats, "/state?after=-1", Some(&"b".repeat(24))).status, 403);
    assert_eq!(get(&sign, &stats, "/state?after=-1", Some("")).status, 403);
    let ok = get(&sign, &stats, "/state?after=-1", Some(&sign.token()));
    assert_eq!((ok.status, ok.content_type.as_str()), (200, "application/json"));
    assert_eq!(String::from_utf8_lossy(&ok.body), r#"{"on": true, "text": "On air", "seq": 1}"#);
    assert_eq!(stats.count(Step::Refused), 3);
}

#[test]
fn a_page_waits_for_the_next_change() {
    let (sign, stats) = sign();
    let stats = Arc::new(stats);
    sign.publish(json!({"on": false}));
    let token = sign.token();
    let (page, seen) = (sign.clone(), stats.clone());
    let waiting = std::thread::spawn(move || get(&page, &seen, "/state?after=1", Some(&token)));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!waiting.is_finished(), "nothing new yet: the request is held");
    sign.publish(json!({"on": true}));
    let answer = waiting.join().unwrap();
    assert_eq!(String::from_utf8_lossy(&answer.body), r#"{"on": true, "seq": 2}"#);
}

#[test]
fn a_page_that_has_seen_everything_gets_the_state_as_it_is_after_the_wait() {
    let sign = Sign::with_wait("t".repeat(24).as_str(), Duration::from_millis(200));
    let stats = ServerStats::new(1, 1, false, "On Air");
    sign.publish(json!({"on": true}));
    let started = Instant::now();
    let answer = get(&sign, &stats, "/state?after=1", Some(&sign.token()));
    assert!(started.elapsed() >= Duration::from_millis(150));
    assert_eq!(String::from_utf8_lossy(&answer.body), r#"{"on": true, "seq": 1}"#);
    // A HEAD doesn't wait, and a bad "after" counts as a page that hasn't seen anything
    let head = sign.handle(&Request { method: "HEAD".into(), target: "/state?after=1".into(),
                                      headers: vec![("authorization".into(), format!("Bearer {}", sign.token()))] }, &stats);
    assert_eq!(head.status, 200);
    let bad = get(&sign, &stats, "/state?after=soon", Some(&sign.token()));
    assert_eq!(String::from_utf8_lossy(&bad.body), r#"{"on": true, "seq": 1}"#);
}

#[test]
fn a_new_link_ends_the_wait_and_refuses_the_old_one() {
    let (sign, stats) = sign();
    let stats = Arc::new(stats);
    sign.publish(json!({"on": false}));
    let old = sign.token();
    let (page, seen, token) = (sign.clone(), stats.clone(), old.clone());
    let waiting = std::thread::spawn(move || get(&page, &seen, "/state?after=1", Some(&token)));
    std::thread::sleep(Duration::from_millis(300));
    sign.set_token(&"c".repeat(24));
    waiting.join().unwrap();
    assert_eq!(get(&sign, &stats, "/state?after=1", Some(&old)).status, 403);
    assert_eq!(get(&sign, &stats, "/state?after=1", Some(&"c".repeat(24))).status, 200);
}

#[test]
fn stopping_ends_a_wait() {
    let (sign, _) = sign();
    sign.publish(json!({"on": false}));
    let page = sign.clone();
    let waiting = std::thread::spawn(move || page.wait_state(1, Duration::from_secs(5)));
    std::thread::sleep(Duration::from_millis(100));
    sign.stop();
    assert_eq!(waiting.join().unwrap()["seq"], 1);
}

#[test]
fn texts_in_other_languages_are_sent_as_json_escapes() {
    let (sign, stats) = sign();
    sign.publish(json!({"on": true, "text": "Am Sender \u{fc}"}));
    let body = String::from_utf8(get(&sign, &stats, "/state?after=-1", Some(&sign.token())).body).unwrap();
    assert!(body.contains("Am Sender \\u00fc"), "{body}");
}

#[test]
fn a_query_value_is_read_the_way_parse_qs_does() {
    assert_eq!(py_int(" 3 "), Some(3));
    assert_eq!((py_int("+3"), py_int("-1"), py_int("1_0"), py_int("1__0"), py_int("_1"), py_int(""), py_int("3.5")), (Some(3), Some(-1), Some(10), None, None, None, None));
    assert_eq!(query_value("%2D1"), "-1");
    assert_eq!(query_value("a+b%zz%4"), "a b%zz%4");
}
