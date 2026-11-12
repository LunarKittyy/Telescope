use super::*;
use crate::https::{PhoneAuth, Request as Get};
use rustls::pki_types::ServerName;
use rustls::RootCertStore;
use std::io::Read;
use std::net::TcpStream;
use telescope_core::testing::scratch_dir;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn hosts(list: &[&str]) -> Vec<String> {
    list.iter().map(|h| h.to_string()).collect()
}

/// A TLS client that trusts only this certificate, the way a browser told to trust it does.
fn client_for(cert: &Certificate) -> Arc<rustls::ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from_pem_file(&cert.cert).unwrap()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Arc::new(rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap()
        .with_root_certificates(roots).with_no_client_auth())
}

fn handshake(cert: &Certificate, name: &str) -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = tls_config(&cert.cert, &cert.key).unwrap();
    let serving = std::thread::spawn(move || {
        let (mut tcp, _) = listener.accept().unwrap();
        let mut tls = rustls::ServerConnection::new(config).unwrap();
        let _ = tls.complete_io(&mut tcp);
    });
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut conn = rustls::ClientConnection::new(client_for(cert), ServerName::try_from(name.to_string()).unwrap()).unwrap();
    let result = conn.complete_io(&mut tcp).map(|_| ()).map_err(|e| e.to_string());
    let _ = serving.join();
    result
}

#[test]
fn a_certificate_is_good_for_localhost_and_the_addresses_it_was_made_for() {
    let cert = ensure_certificate(&scratch_dir("cert"), &hosts(&["127.0.0.1", "not an address", "fe80::1"]), unix_now()).unwrap();
    assert_eq!(handshake(&cert, "127.0.0.1"), Ok(()));
    assert_eq!(handshake(&cert, "localhost"), Ok(()));
    assert!(handshake(&cert, "10.9.9.9").is_err(), "an address it wasn't made for");
}

#[test]
fn the_private_key_is_for_this_user_only() {
    let cert = ensure_certificate(&scratch_dir("cert"), &[], unix_now()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&cert.key).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(std::fs::read_to_string(&cert.key).unwrap().starts_with("-----BEGIN PRIVATE KEY-----\n"));
    assert!(std::fs::read_to_string(&cert.cert).unwrap().starts_with("-----BEGIN CERTIFICATE-----\n"));
    assert!(cert.cert.ends_with("browser_camera_cert.pem") && cert.key.ends_with("browser_camera_key.pem"));
}

#[test]
fn the_certificate_is_kept_until_it_nears_its_end() {
    let folder = scratch_dir("cert");
    let now = unix_now();
    let cert = ensure_certificate(&folder, &hosts(&["192.168.1.5"]), now).unwrap();
    let first = std::fs::read(&cert.cert).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&cert.key).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // A browser that trusted it keeps doing so
    ensure_certificate(&folder, &[], now).unwrap();
    assert_eq!(std::fs::read(&cert.cert).unwrap(), first);
    ensure_certificate(&folder, &[], now + 380 * 86400).unwrap();
    assert_ne!(std::fs::read(&cert.cert).unwrap(), first);
}

#[test]
fn an_unreadable_certificate_is_replaced() {
    let folder = scratch_dir("cert");
    let cert = ensure_certificate(&folder, &[], unix_now()).unwrap();
    std::fs::write(&cert.cert, "not a certificate").unwrap();
    ensure_certificate(&folder, &[], unix_now()).unwrap();
    assert!(std::fs::read_to_string(&cert.cert).unwrap().contains("BEGIN CERTIFICATE"));
    // A key that isn't this certificate's counts as unreadable too
    let other = ensure_certificate(&scratch_dir("cert"), &[], unix_now()).unwrap();
    std::fs::copy(&other.key, &cert.key).unwrap();
    let before = std::fs::read(&cert.cert).unwrap();
    ensure_certificate(&folder, &[], unix_now()).unwrap();
    assert!(tls_config(&cert.cert, &cert.key).is_ok());
    assert_ne!(std::fs::read(&cert.cert).unwrap(), before);
}

#[test]
fn the_certificate_python_made_is_taken_as_it_is() {
    // Made by the Python app (cryptography) on 2026-10-29, so a browser that trusted it still does after the switch
    let folder = scratch_dir("cert");
    std::fs::copy(fixture("python_cert.pem"), folder.join(CERT_FILE)).unwrap();
    std::fs::copy(fixture("python_key.pem"), folder.join(KEY_FILE)).unwrap();
    let before = std::fs::read(folder.join(CERT_FILE)).unwrap();
    let end = days_from_civil(2027, 11, 30) * 86400 + 12 * 3600;
    let der = CertificateDer::from_pem_file(folder.join(CERT_FILE)).unwrap();
    assert_eq!(not_after(&der), Some(end));
    let cert = ensure_certificate(&folder, &[], end - 100 * 86400).unwrap();
    assert_eq!(std::fs::read(&cert.cert).unwrap(), before);
    // Close to its end it's made again
    ensure_certificate(&folder, &[], end - 10 * 86400).unwrap();
    assert_ne!(std::fs::read(&cert.cert).unwrap(), before);
}

#[test]
fn the_end_of_a_certificate_is_read_whichever_way_its_time_is_written() {
    for now in [0, 946_684_800, 1_793_000_000, 2_524_608_000, 2_700_000_000, 4_102_444_800] {
        let (cert, _) = make_certificate(&[], now).unwrap();
        assert_eq!(not_after(&cert), Some(now + CERT_DAYS * 86400), "{now}");
    }
    assert_eq!(http_date(1_793_000_000), "Mon, 26 Oct 2026 07:33:20 GMT");
    assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
}

// ── The server ───────────────────────────────────────────────────────────────

/// Answers /hello with "hi", /slow once told to go, /boom with a panic, and counts what it saw.
struct Pages {
    seen: Mutex<Vec<(String, String)>>,
    go: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl PageHandler for Pages {
    fn handle(&self, request: &Request, stats: &ServerStats) -> Reply {
        self.seen.lock().unwrap().push((request.method.clone(), request.target.clone()));
        match request.path() {
            "/hello" => {
                stats.note(Step::Pages);
                Reply::new(200, "hi", "text/html; charset=utf-8")
            }
            "/auth" => Reply::text(200, request.header("authorization").unwrap_or("")),
            "/slow" => {
                if let Some(go) = self.go.lock().unwrap().take() {
                    let _ = go.recv_timeout(Duration::from_secs(5));
                }
                Reply::text(200, "late")
            }
            "/boom" => panic!("on purpose"),
            _ => Reply::text(404, "Not found"),
        }
    }
}

struct Running {
    server: Box<dyn WebServer>,
    cert: Certificate,
    pages: Arc<Pages>,
    release: std::sync::mpsc::Sender<()>,
}

fn start(port: u16) -> Running {
    let cert = ensure_certificate(&scratch_dir("serve"), &hosts(&["127.0.0.1"]), unix_now()).unwrap();
    let (release, go) = std::sync::mpsc::channel();
    let pages = Arc::new(Pages { seen: Mutex::default(), go: Mutex::new(Some(go)) });
    let serve = Serve { port, host: "127.0.0.1".into(), label: "Test".into() };
    let server = SystemWeb.serve(&cert, &serve, pages.clone()).unwrap();
    Running { server, cert, pages, release }
}

/// One TLS connection that trusts the server's certificate, for sending requests by hand.
struct Client {
    stream: rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
}

struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

impl Client {
    fn connect(run: &Running) -> Client {
        let tcp = TcpStream::connect(("127.0.0.1", run.server.port())).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let conn = rustls::ClientConnection::new(client_for(&run.cert), ServerName::try_from("127.0.0.1".to_string()).unwrap()).unwrap();
        Client { stream: rustls::StreamOwned::new(conn, tcp) }
    }

    fn send(&mut self, raw: &str) {
        self.stream.write_all(raw.as_bytes()).unwrap();
    }

    /// The next answer; `head` leaves out the body a HEAD doesn't get.
    fn read(&mut self, head: bool) -> Option<Answer> {
        let mut reader = BufReader::new(&mut self.stream);
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let status = line.split_whitespace().nth(1)?.parse().ok()?;
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            reader.read_line(&mut h).ok()?;
            if h.trim().is_empty() {
                break;
            }
            let (k, v) = h.split_once(':')?;
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        let length: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length"))?.1.parse().ok()?;
        let mut body = vec![0; if head { 0 } else { length }];
        reader.read_exact(&mut body).ok()?;
        Some(Answer { status, headers, body: String::from_utf8_lossy(&body).into_owned() })
    }

    fn get(&mut self, target: &str) -> Answer {
        self.send(&format!("GET {target} HTTP/1.1\r\nHost: x\r\n\r\n"));
        self.read(false).expect("an answer")
    }

    /// Whether the server has hung up.
    fn closed(&mut self) -> bool {
        let mut byte = [0u8; 1];
        matches!(self.stream.read(&mut byte), Ok(0) | Err(_))
    }
}

#[test]
fn a_page_is_served_with_the_headers_every_page_gets() {
    let run = start(0);
    let answer = Client::connect(&run).get("/hello");
    assert_eq!((answer.status, answer.body.as_str()), (200, "hi"));
    assert_eq!(answer.header("Content-Type"), Some("text/html; charset=utf-8"));
    assert_eq!(answer.header("Content-Length"), Some("2"));
    assert_eq!(answer.header("Cache-Control"), Some("no-store"));
    assert_eq!(answer.header("X-Content-Type-Options"), Some("nosniff"));
    assert_eq!(answer.header("Referrer-Policy"), Some("no-referrer"));
    assert_eq!(answer.header("Server"), Some("Telescope"));
    assert!(answer.header("Content-Security-Policy").unwrap().starts_with("default-src 'self'; connect-src 'self' wss:;"));
    assert!(answer.header("Content-Security-Policy").unwrap().ends_with("frame-ancestors 'none'"));
    assert!(answer.header("Date").unwrap().ends_with(" GMT"));
    assert_eq!(Client::connect(&run).get("/nowhere").status, 404);
    assert_eq!(run.server.stats().count(Step::Pages), 1);
}

#[test]
fn the_token_header_reaches_the_page() {
    let run = start(0);
    let fingerprint = crate::https::fingerprint(CertificateDer::from_pem_file(&run.cert.cert).unwrap().as_ref());
    let url = format!("https://127.0.0.1:{}/auth", run.server.port());
    let mut response = PhoneAuth::new("secret-token", &fingerprint).open(Get::get(&url, Duration::from_secs(5))).unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.read_capped(100).unwrap(), b"Bearer secret-token");
}

#[test]
fn a_head_gets_the_headers_and_no_body() {
    let run = start(0);
    let mut client = Client::connect(&run);
    client.send("HEAD /hello HTTP/1.1\r\nHost: x\r\n\r\n");
    let answer = client.read(true).unwrap();
    assert_eq!((answer.status, answer.header("Content-Length")), (200, Some("2")));
    // The connection is still good for the next request, so nothing of the body was sent
    assert_eq!(client.get("/hello").body, "hi");
}

#[test]
fn a_connection_stays_for_the_next_request_unless_asked_to_close() {
    let run = start(0);
    let mut client = Client::connect(&run);
    assert_eq!(client.get("/hello").status, 200);
    assert_eq!(client.get("/hello?again").status, 200);
    client.send("GET /hello HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert_eq!(client.read(false).unwrap().body, "hi");
    assert!(client.closed());
    // An older client has to ask to keep it
    let mut old = Client::connect(&run);
    old.send("GET /hello HTTP/1.0\r\n\r\n");
    assert_eq!(old.read(false).unwrap().status, 200);
    assert!(old.closed());
    assert_eq!(run.pages.seen.lock().unwrap().len(), 4);
}

#[test]
fn only_get_and_head_are_answered() {
    let run = start(0);
    let mut client = Client::connect(&run);
    client.send("POST /hello HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n");
    let answer = client.read(false).unwrap();
    assert_eq!(answer.status, 501);
    assert!(answer.body.contains("POST") && client.closed());
    assert!(run.pages.seen.lock().unwrap().is_empty());
}

#[test]
fn nonsense_is_a_bad_request() {
    let run = start(0);
    let mut client = Client::connect(&run);
    client.send("what is this\r\n\r\n");
    assert_eq!(client.read(false).unwrap().status, 400);
    let mut client = Client::connect(&run);
    client.send("GET /hello HTTP/2.0\r\n\r\n");
    assert_eq!(client.read(false).unwrap().status, 505);
    let mut client = Client::connect(&run);
    client.send(&format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(70_000)));
    assert_eq!(client.read(false).unwrap().status, 414);
    let mut client = Client::connect(&run);
    let many: String = (0..101).map(|i| format!("X-{i}: y\r\n")).collect();
    client.send(&format!("GET /hello HTTP/1.1\r\n{many}\r\n"));
    assert_eq!(client.read(false).unwrap().status, 431);
}

#[test]
fn header_lines_without_a_colon_count_too() {
    let run = start(0);
    let mut client = Client::connect(&run);
    let many: String = (0..101).map(|_| "x\r\n").collect();
    client.send(&format!("GET /hello HTTP/1.1\r\n{many}\r\n"));
    assert_eq!(client.read(false).unwrap().status, 431);
}

/// Hands out one short line per read, slowly, and never finishes the head.
struct Dripper {
    line: &'static [u8],
    pos: usize,
    pace: Duration,
}

impl Read for Dripper {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(self.pace);
        let n = buf.len().min(self.line.len() - self.pos);
        buf[..n].copy_from_slice(&self.line[self.pos..self.pos + n]);
        self.pos = (self.pos + n) % self.line.len();
        Ok(n)
    }
}

impl Write for Dripper {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Timed for Dripper {
    fn limit_reads(&mut self, _: Option<Instant>) {}
}

#[test]
fn a_request_that_never_finishes_its_head_is_dropped_on_time() {
    let pages = Pages { seen: Mutex::default(), go: Mutex::new(None) };
    let stats = ServerStats::new(0, 0, false, "Test");
    // A request line, then colonless lines forever, each one inside any per-read timeout
    let mut stream = BufReader::new(Dripper { line: b"GET /hello HTTP/1.1\r\nx\r\n", pos: 0, pace: Duration::from_millis(40) });
    let started = Instant::now();
    let served = serve_one(&mut stream, &pages, &stats, Duration::from_secs(5), Duration::from_millis(300));
    assert!(matches!(served, Ok(false)));
    assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());
}

/// Sends `bytes` one at a time, `pace` apart, until the other end is gone.
fn drip(tcp: &mut TcpStream, bytes: &[u8], pace: Duration) {
    for b in bytes {
        if tcp.write_all(&[*b]).is_err() {
            return;
        }
        std::thread::sleep(pace);
    }
}

fn pending_tls(conn: &mut rustls::ClientConnection) -> Vec<u8> {
    let mut out = Vec::new();
    while conn.wants_write() {
        conn.write_tls(&mut out).unwrap();
    }
    out
}

/// The server's end of a connection where the client has the TLS handshake done and then drips `payload` as one record.
fn server_end(payload: &'static [u8], pace: Duration) -> rustls::StreamOwned<rustls::ServerConnection, Limited> {
    let cert = ensure_certificate(&scratch_dir("drip"), &hosts(&["127.0.0.1"]), unix_now()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let client_config = client_for(&cert);
    std::thread::spawn(move || {
        let mut tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut conn = rustls::ClientConnection::new(client_config, ServerName::try_from("127.0.0.1".to_string()).unwrap()).unwrap();
        while conn.is_handshaking() {
            if conn.complete_io(&mut tcp).is_err() {
                return;
            }
        }
        conn.writer().write_all(payload).unwrap();
        drip(&mut tcp, &pending_tls(&mut conn), pace);
    });
    let (tcp, _) = listener.accept().unwrap();
    let mut tcp = Limited::new(tcp);
    let mut tls = rustls::ServerConnection::new(tls_config(&cert.cert, &cert.key).unwrap()).unwrap();
    finish_handshake(&mut tls, &mut tcp, Duration::from_secs(10)).unwrap();
    rustls::StreamOwned::new(tls, tcp)
}

const A_REQUEST: &[u8] = b"GET /hello HTTP/1.1\r\nHost: x\r\nX-Padding: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n\r\n";

#[test]
fn a_handshake_dripped_past_its_deadline_is_dropped_on_time() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let cert = ensure_certificate(&scratch_dir("drip-hello"), &hosts(&["127.0.0.1"]), unix_now()).unwrap();
    let client_config = client_for(&cert);
    // Each byte comes well inside any per-read timeout, the whole hello takes seconds
    std::thread::spawn(move || {
        let mut tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut conn = rustls::ClientConnection::new(client_config, ServerName::try_from("127.0.0.1".to_string()).unwrap()).unwrap();
        drip(&mut tcp, &pending_tls(&mut conn), Duration::from_millis(25));
    });
    let (tcp, _) = listener.accept().unwrap();
    let mut tcp = Limited::new(tcp);
    let mut tls = rustls::ServerConnection::new(tls_config(&cert.cert, &cert.key).unwrap()).unwrap();
    let started = Instant::now();
    let done = finish_handshake(&mut tls, &mut tcp, Duration::from_millis(150));
    assert_eq!(done.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(1), "took {:?}", started.elapsed());
}

#[test]
fn a_request_dripped_inside_tls_is_dropped_on_time() {
    let pages = Pages { seen: Mutex::default(), go: Mutex::new(None) };
    let stats = ServerStats::new(0, 0, false, "Test");
    let mut stream = BufReader::new(server_end(A_REQUEST, Duration::from_millis(25)));
    let started = Instant::now();
    let served = serve_one(&mut stream, &pages, &stats, Duration::from_millis(150), Duration::from_millis(150));
    assert!(matches!(served, Ok(false)));
    assert!(started.elapsed() < Duration::from_secs(1), "took {:?}", started.elapsed());
    assert!(pages.seen.lock().unwrap().is_empty());
}

#[test]
fn a_taken_connection_gets_its_tick_while_a_record_drips() {
    let mut taken = Taken(BufReader::new(server_end(A_REQUEST, Duration::from_millis(25))));
    taken.set_read_timeout(Some(Duration::from_millis(150))).unwrap();
    let started = Instant::now();
    let mut buf = [0u8; 256];
    let err = taken.read(&mut buf).unwrap_err();
    assert!(matches!(err.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(1), "took {:?}", started.elapsed());
}

#[test]
fn a_page_that_fails_is_a_500_and_the_server_carries_on() {
    let run = start(0);
    let mut client = Client::connect(&run);
    client.send("GET /boom HTTP/1.1\r\n\r\n");
    assert_eq!(client.read(false).unwrap().status, 500);
    assert_eq!(Client::connect(&run).get("/hello").status, 200);
}

#[test]
fn a_page_can_wait_without_holding_up_another_connection() {
    let run = start(0);
    let mut waiting = Client::connect(&run);
    waiting.send("GET /slow HTTP/1.1\r\n\r\n");
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(Client::connect(&run).get("/hello").status, 200);
    run.release.send(()).unwrap();
    assert_eq!(waiting.read(false).unwrap().body, "late");
}

#[test]
fn a_browser_that_hangs_up_in_the_handshake_is_counted() {
    let run = start(0);
    let mut tcp = TcpStream::connect(("127.0.0.1", run.server.port())).unwrap();
    tcp.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
    drop(tcp);
    let stats = run.server.stats();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while stats.count(Step::TlsFailed) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!((stats.count(Step::TlsFailed), stats.count(Step::Connections)), (1, 1));
}

#[test]
fn no_address_has_more_than_six_connections_open() {
    let run = start(0);
    let mut kept: Vec<Client> = (0..MAX_PER_ADDRESS).map(|_| Client::connect(&run)).collect();
    for client in &mut kept {
        assert_eq!(client.get("/hello").status, 200);
    }
    // The seventh is dropped before it's even greeted
    let tcp = TcpStream::connect(("127.0.0.1", run.server.port())).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut conn = rustls::ClientConnection::new(client_for(&run.cert), ServerName::try_from("127.0.0.1".to_string()).unwrap()).unwrap();
    let mut tcp = tcp;
    assert!(conn.complete_io(&mut tcp).is_err());
    // A place frees up when one of the six ends
    kept.pop();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(Client::connect(&run).get("/hello").status, 200);
}

#[test]
fn a_taken_port_falls_back_to_a_free_one() {
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let wanted = taken.local_addr().unwrap().port();
    let run = start(wanted);
    let stats = run.server.stats();
    assert!(stats.fell_back && stats.wanted_port == wanted && stats.port != wanted && run.server.port() == stats.port);
    assert_eq!(Client::connect(&run).get("/hello").status, 200);
    let free = start(0);
    assert!(!free.server.stats().fell_back);
}

#[test]
fn stopping_ends_the_server() {
    let run = start(0);
    let port = run.server.port();
    assert_eq!(Client::connect(&run).get("/hello").status, 200);
    run.server.stop();
    run.server.stop();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_ok() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err(), "nothing listens any more");
}

#[test]
fn a_certificate_that_isnt_there_is_why_it_wont_start() {
    let missing = Certificate { cert: "/nowhere/cert.pem".into(), key: "/nowhere/key.pem".into() };
    let pages = Arc::new(Pages { seen: Mutex::default(), go: Mutex::default() });
    let serve = Serve { port: 0, host: "127.0.0.1".into(), label: "Test".into() };
    assert!(SystemWeb.serve(&missing, &serve, pages).is_err());
}
