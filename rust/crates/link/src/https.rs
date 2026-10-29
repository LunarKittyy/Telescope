//! HTTPS to a paired phone, trusting only the certificate fingerprint it gave at pairing, checked before any request
//! byte is sent (pinned_https.py). A small HTTP/1.1 client: one request per connection, never following redirects and
//! never going through a proxy. Plain http only goes out without a pin, for tests' local servers.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use serde_json::Value;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// Largest JSON reply taken from anything answering on a phone's ports; whoever answers may not be the phone.
pub const MAX_REPLY_BYTES: usize = 256 * 1024;
/// How often a blocked read looks whether it was aborted (Windows doesn't wake a read on shutdown).
const ABORT_CHECK: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// Something answered, but not with the certificate the phone paired with (or there's no pin to check).
    PinMismatch,
    /// The TCP connection opened but TLS didn't: a listener that doesn't speak it, like an app from before TLS.
    HandshakeFailed(String),
    /// Answered with a status other than 2xx.
    Http(u16),
    /// A pinned phone's token would have gone out without TLS.
    Refused,
    Bad(String),
}

impl Error {
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Http(code) => Some(*code),
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::PinMismatch => write!(f, "the phone's certificate doesn't match the one it paired with"),
            Error::HandshakeFailed(e) => write!(f, "TLS handshake failed: {e}"),
            Error::Http(code) => write!(f, "HTTP {code}"),
            Error::Refused => write!(f, "refusing to send a pinned phone's token without TLS"),
            Error::Bad(s) => write!(f, "{s}"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub fn fingerprint(der: &[u8]) -> String {
    crate::util::sha256_hex(der)
}

pub fn is_fingerprint(value: &str) -> bool {
    crate::util::is_fingerprint(value)
}

#[derive(Debug)]
struct Pin {
    sha256: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pin {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>],
                          _server_name: &ServerName<'_>, _ocsp: &[u8], _now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        // No CA or hostname checks: the phone's certificate is self-signed and the pin replaces both
        if crate::util::same(&fingerprint(end_entity.as_ref()), &self.sha256) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct)
                              -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct)
                              -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_config(pin: &str) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(Pin { sha256: pin.to_string(), provider: provider.clone() });
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring offers TLS 1.2 and 1.3")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Arc::new(config)
}

/// The parts of an http(s) URL this client needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// The path and query, starting with "/".
    pub target: String,
}

impl Url {
    pub fn parse(url: &str) -> Result<Url, Error> {
        let (tls, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err(Error::Bad(format!("not an http(s) URL: {url}")));
        };
        let (authority, target) = match rest.find(['/', '?']) {
            Some(i) if rest[i..].starts_with('/') => (&rest[..i], rest[i..].to_string()),
            Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
            None => (rest, "/".to_string()),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => {
                (h.to_string(), p.parse().map_err(|_| Error::Bad(format!("bad port in {url}")))?)
            }
            _ => (authority.to_string(), if tls { 443 } else { 80 }),
        };
        let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
        if host.is_empty() {
            return Err(Error::Bad(format!("no host in {url}")));
        }
        Ok(Url { tls, host, port, target })
    }

    /// The URL's hostname, like urlsplit().hostname.
    pub fn hostname(url: &str) -> Option<String> {
        Url::parse(url).ok().map(|u| u.host)
    }
}

/// A TCP socket whose blocked reads can be ended from another thread, on every platform.
struct Socket {
    stream: TcpStream,
    aborted: Arc<AtomicBool>,
    timeout: Duration,
}

impl Read for Socket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let started = Instant::now();
        loop {
            if self.aborted.load(Ordering::SeqCst) {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "aborted"));
            }
            match self.stream.read(buf) {
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    if started.elapsed() >= self.timeout {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                other => return other,
            }
        }
    }
}

impl Write for Socket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Ends a response's reads from any thread, without touching its TLS state.
#[derive(Clone)]
pub struct Aborter {
    aborted: Arc<AtomicBool>,
    /// Weak, so a response that's dropped closes its connection whoever still holds this.
    socket: Option<Weak<TcpStream>>,
}

impl Aborter {
    pub fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        if let Some(s) = self.socket.as_ref().and_then(Weak::upgrade) {
            let _ = s.shutdown(Shutdown::Both);
        }
    }

    pub fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    /// One that only ever says it was aborted (stand-ins in tests).
    pub fn detached() -> Aborter {
        Aborter { aborted: Arc::new(AtomicBool::new(false)), socket: None }
    }
}

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// The body as it arrives: up to Content-Length, chunk by chunk, or until the phone closes.
enum Body {
    Length(u64),
    Chunked { left: u64, done: bool },
    UntilClose,
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    reader: BufReader<Box<dyn Stream>>,
    body: Body,
    aborter: Aborter,
    _socket: Arc<TcpStream>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn content_type(&self) -> &str {
        self.header("content-type").unwrap_or("")
    }

    pub fn aborter(&self) -> Aborter {
        self.aborter.clone()
    }

    fn next_chunk(&mut self) -> io::Result<()> {
        if let Body::Chunked { left, done } = &mut self.body {
            if *left == 0 && !*done {
                let mut line = String::new();
                self.reader.read_line(&mut line)?;
                if line.trim().is_empty() {
                    line.clear();
                    self.reader.read_line(&mut line)?;
                }
                let size = line.trim().split(';').next().unwrap_or("");
                *left = u64::from_str_radix(size, 16).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk"))?;
                if *left == 0 {
                    *done = true;
                }
            }
        }
        Ok(())
    }

    /// The body, or Bad when it's over limit.
    pub fn read_capped(&mut self, limit: usize) -> Result<Vec<u8>, Error> {
        let mut raw = Vec::new();
        self.take((limit + 1) as u64).read_to_end(&mut raw)?;
        if raw.len() > limit {
            return Err(Error::Bad(format!("reply over {limit} bytes")));
        }
        Ok(raw)
    }

    /// A capped body as JSON.
    pub fn json(&mut self) -> Result<Value, Error> {
        let raw = self.read_capped(MAX_REPLY_BYTES)?;
        serde_json::from_slice(&raw).map_err(|e| Error::Bad(e.to_string()))
    }

    /// One line with its "\n", at most limit bytes; empty at the end of the body.
    pub fn readline(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while line.len() < limit {
            if self.read(&mut byte)? == 0 {
                break;
            }
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        Ok(line)
    }
}

impl Read for Response {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.next_chunk()?;
        let want = match &self.body {
            Body::Length(0) | Body::Chunked { done: true, .. } => return Ok(0),
            Body::Length(left) | Body::Chunked { left, .. } => (*left).min(buf.len() as u64) as usize,
            Body::UntilClose => buf.len(),
        };
        let n = match self.reader.read(&mut buf[..want]) {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && matches!(self.body, Body::UntilClose) => 0,
            other => other?,
        };
        match &mut self.body {
            Body::Length(left) | Body::Chunked { left, .. } => {
                if n == 0 && want > 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "body cut short"));
                }
                *left -= n as u64;
            }
            Body::UntilClose => {}
        }
        Ok(n)
    }
}

/// One request's details.
pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub body: Option<&'a [u8]>,
    /// Sends the bearer token (everything but /v1/hello).
    pub token: bool,
    pub timeout: Duration,
}

impl<'a> Request<'a> {
    pub fn get(url: &'a str, timeout: Duration) -> Request<'a> {
        Request { method: "GET", url, body: None, token: true, timeout }
    }

    pub fn post(url: &'a str, body: &'a [u8], timeout: Duration) -> Request<'a> {
        Request { method: "POST", url, body: Some(body), token: true, timeout }
    }

    pub fn without_token(mut self) -> Request<'a> {
        self.token = false;
        self
    }
}

/// A paired phone's bearer token plus its certificate pin; opens authenticated requests to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PhoneAuth {
    pub token: String,
    pub pin: String,
}

impl PhoneAuth {
    pub fn new(token: &str, pin: &str) -> PhoneAuth {
        PhoneAuth { token: token.to_string(), pin: pin.to_string() }
    }

    /// Like urlopen: a 2xx response with its body unread, else the error. https needs the pin; plain http only goes
    /// out when there's no pin.
    pub fn open(&self, req: Request<'_>) -> Result<Response, Error> {
        let resp = self.open_any(req)?;
        if !(200..300).contains(&resp.status) {
            return Err(Error::Http(resp.status));
        }
        Ok(resp)
    }

    /// Like open, but an answer with any status comes back, so an error's body can be read (urllib's HTTPError).
    pub fn open_any(&self, req: Request<'_>) -> Result<Response, Error> {
        let url = Url::parse(req.url)?;
        if url.tls && !is_fingerprint(&self.pin) {
            return Err(Error::PinMismatch);
        }
        if !url.tls && !self.pin.is_empty() {
            return Err(Error::Refused);
        }
        let addr = (url.host.as_str(), url.port).to_socket_addrs()?.next()
            .ok_or_else(|| Error::Bad(format!("no address for {}", url.host)))?;
        let tcp = TcpStream::connect_timeout(&addr, req.timeout)?;
        tcp.set_read_timeout(Some(req.timeout.min(ABORT_CHECK)))?;
        tcp.set_write_timeout(Some(req.timeout))?;
        tcp.set_nodelay(true)?;
        let shared = Arc::new(tcp.try_clone()?);
        let aborted = Arc::new(AtomicBool::new(false));
        let mut socket = Socket { stream: tcp, aborted: aborted.clone(), timeout: req.timeout };
        let mut stream: Box<dyn Stream> = if url.tls {
            let name = ServerName::try_from(url.host.clone()).map_err(|e| Error::Bad(e.to_string()))?;
            let mut conn = ClientConnection::new(tls_config(&self.pin), name).map_err(|e| Error::HandshakeFailed(e.to_string()))?;
            while conn.is_handshaking() {
                if let Err(e) = conn.complete_io(&mut socket) {
                    let _ = shared.shutdown(Shutdown::Both);
                    return Err(handshake_error(e));
                }
            }
            Box::new(StreamOwned::new(conn, socket))
        } else {
            Box::new(socket)
        };
        let mut head = format!("{} {} HTTP/1.1\r\nHost: {}:{}\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
                               req.method, url.target, url.host, url.port);
        if req.token {
            head += &format!("Authorization: Bearer {}\r\n", self.token);
        }
        if let Some(body) = req.body {
            head += &format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len());
        }
        head += "\r\n";
        stream.write_all(head.as_bytes())?;
        if let Some(body) = req.body {
            stream.write_all(body)?;
        }
        stream.flush()?;

        let mut reader = BufReader::with_capacity(64 * 1024, stream);
        let (status, headers) = loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            let status: u16 = line.split_whitespace().nth(1).filter(|_| line.starts_with("HTTP/")).and_then(|s| s.parse().ok())
                .ok_or_else(|| Error::Bad(format!("not HTTP: {:?}", line.chars().take(40).collect::<String>())))?;
            let mut headers = Vec::new();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            if status != 100 {
                break (status, headers);
            }
        };
        let find = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone());
        let body = if find("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
            Body::Chunked { left: 0, done: false }
        } else if let Some(n) = find("content-length").and_then(|v| v.parse().ok()) {
            Body::Length(n)
        } else {
            Body::UntilClose
        };
        let aborter = Aborter { aborted, socket: Some(Arc::downgrade(&shared)) };
        Ok(Response { status, headers, reader, body, aborter, _socket: shared })
    }

    /// A GET or POST whose answer is a capped JSON body.
    pub fn json(&self, req: Request<'_>) -> Result<Value, Error> {
        self.open(req)?.json()
    }
}

fn handshake_error(e: io::Error) -> Error {
    if let Some(inner) = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
        if matches!(inner, rustls::Error::InvalidCertificate(_)) {
            return Error::PinMismatch;
        }
    }
    if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::ConnectionAborted) {
        return Error::Io(e);
    }
    Error::HandshakeFailed(e.to_string())
}

/// Whether a failed open reached a listener that doesn't speak TLS.
pub fn handshake_failed(e: &Error) -> bool {
    matches!(e, Error::HandshakeFailed(_))
}

/// Whether a failed open was the pin check: something answered, but not the paired phone.
pub fn pin_rejected(e: &Error) -> bool {
    matches!(e, Error::PinMismatch)
}

#[cfg(test)]
pub(crate) mod testing {
    //! A local HTTPS server presenting desktop/tests/fixtures' test certificates, as the Python tests use.

    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    pub fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../desktop/tests/fixtures").join(name)
    }

    pub fn pin_of(name: &str) -> String {
        let cert = CertificateDer::from_pem_file(fixture(&format!("test_{name}.crt"))).unwrap();
        super::fingerprint(cert.as_ref())
    }

    /// A request as the server saw it.
    #[derive(Clone, Debug)]
    pub struct Seen {
        pub method: String,
        pub path: String,
        pub auth: Option<String>,
        pub body: Vec<u8>,
    }

    /// The handler gets the request and returns (status, content type, body).
    pub type Handler = Arc<dyn Fn(&Seen) -> (u16, String, Vec<u8>) + Send + Sync>;

    pub struct Server {
        pub port: u16,
        pub seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Server {
        pub fn base(&self, tls: bool) -> String {
            format!("{}://127.0.0.1:{}", if tls { "https" } else { "http" }, self.port)
        }
    }

    fn read_request(stream: &mut dyn Read) -> Option<Seen> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next()?.to_string(), parts.next()?.to_string());
        let (mut auth, mut length) = (None, 0usize);
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).ok()? == 0 || h.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                if k.eq_ignore_ascii_case("authorization") {
                    auth = Some(v.trim().to_string());
                }
                if k.eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).ok()?;
        Some(Seen { method, path, auth, body })
    }

    fn reason(status: u16) -> &'static str {
        match status {
            200 => "OK",
            302 => "Found",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            409 => "Conflict",
            _ => "Status",
        }
    }

    fn answer(stream: &mut dyn ReadWrite, handler: &Handler, seen: &Mutex<Vec<Seen>>) {
        let Some(req) = read_request(stream) else { return };
        seen.lock().unwrap().push(req.clone());
        let (status, kind, body) = handler(&req);
        let head = format!("HTTP/1.1 {status} {}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                           reason(status), body.len());
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&body);
        let _ = stream.flush();
    }

    pub trait ReadWrite: Read + Write {}
    impl<T: Read + Write> ReadWrite for T {}

    /// Serves until the test ends; cert None serves plain HTTP.
    pub fn serve(cert: Option<&str>, handler: Handler) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let config = cert.map(|name| {
            let certs = vec![CertificateDer::from_pem_file(fixture(&format!("test_{name}.crt"))).unwrap()];
            let key = PrivateKeyDer::from_pem_file(fixture(&format!("test_{name}.key"))).unwrap();
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            Arc::new(rustls::ServerConfig::builder_with_provider(provider).with_safe_default_protocol_versions().unwrap()
                .with_no_client_auth().with_single_cert(certs, key).unwrap())
        });
        let record = seen.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let (handler, seen, config) = (handler.clone(), record.clone(), config.clone());
                std::thread::spawn(move || serve_one(conn, config, &handler, &seen));
            }
        });
        Server { port, seen }
    }

    fn serve_one(conn: TcpStream, config: Option<Arc<rustls::ServerConfig>>, handler: &Handler, seen: &Mutex<Vec<Seen>>) {
        let _ = conn.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        match config {
            Some(config) => {
                let Ok(tls) = rustls::ServerConnection::new(config) else { return };
                let mut stream = rustls::StreamOwned::new(tls, conn);
                answer(&mut stream, handler, seen);
                stream.conn.send_close_notify();
                let _ = stream.flush();
            }
            None => {
                let mut conn = conn;
                answer(&mut conn, handler, seen);
            }
        }
    }

    pub fn json_handler(body: &'static str) -> Handler {
        Arc::new(move |_req| (200, "application/json".into(), body.as_bytes().to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::sync::Arc;

    const T: Duration = Duration::from_secs(3);

    fn ok_or_redirect() -> Handler {
        Arc::new(|req| if req.path == "/redirect" {
            (302, "text/plain".into(), Vec::new())
        } else {
            (200, "application/json".into(), br#"{"ok": true}"#.to_vec())
        })
    }

    fn auths(server: &Server) -> Vec<Option<String>> {
        server.seen.lock().unwrap().iter().map(|s| s.auth.clone()).collect()
    }

    #[test]
    fn the_paired_phone_is_reached_and_gets_the_token() {
        let server = serve(Some("phone"), ok_or_redirect());
        let auth = PhoneAuth::new("secret", &pin_of("phone"));
        let url = format!("{}/v1/ping", server.base(true));
        let body = auth.open(Request::get(&url, T)).unwrap().read_capped(100).unwrap();
        assert_eq!(body, br#"{"ok": true}"#);
        assert_eq!(auths(&server), [Some("Bearer secret".to_string())]);
    }

    #[test]
    fn an_impostor_never_sees_the_token() {
        let server = serve(Some("impostor"), ok_or_redirect());
        let auth = PhoneAuth::new("secret", &pin_of("phone"));
        let err = auth.open(Request::get(&format!("{}/v1/ping", server.base(true)), T)).err().unwrap();
        assert!(pin_rejected(&err), "{err}");
        assert!(auths(&server).is_empty());
    }

    #[test]
    fn a_redirect_is_not_followed() {
        let server = serve(Some("phone"), ok_or_redirect());
        let auth = PhoneAuth::new("secret", &pin_of("phone"));
        let err = auth.open(Request::get(&format!("{}/redirect", server.base(true)), T)).err().unwrap();
        assert_eq!(err.status(), Some(302));
        assert_eq!(auths(&server), [Some("Bearer secret".to_string())]);
    }

    #[test]
    fn https_without_a_pin_fails_closed() {
        let server = serve(Some("phone"), ok_or_redirect());
        let err = PhoneAuth::new("secret", "").open(Request::get(&format!("{}/v1/ping", server.base(true)), T)).err().unwrap();
        assert!(pin_rejected(&err));
        assert!(auths(&server).is_empty());
    }

    #[test]
    fn a_pinned_phone_is_never_sent_plain_http() {
        let auth = PhoneAuth::new("secret", &pin_of("phone"));
        assert!(matches!(auth.open(Request::get("http://127.0.0.1:1/v1/ping", T)), Err(Error::Refused)));
    }

    #[test]
    fn fingerprint_shape() {
        assert!(is_fingerprint(&pin_of("phone")));
        assert!(!is_fingerprint(&"AB".repeat(32)));
        assert!(!is_fingerprint(&"ab".repeat(31)));
        assert!(!is_fingerprint(""));
    }

    #[test]
    fn a_listener_that_doesnt_speak_tls_is_a_failed_handshake() {
        let server = serve(None, ok_or_redirect());
        let auth = PhoneAuth::new("secret", &pin_of("phone"));
        let err = auth.open(Request::get(&format!("https://127.0.0.1:{}/v1/hello", server.port), T)).err().unwrap();
        assert!(handshake_failed(&err), "{err}");
        assert!(auths(&server).iter().all(Option::is_none));
    }

    #[test]
    fn nobody_there_is_not_a_failed_handshake() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let auth = PhoneAuth::new("secret", &pin_of("phone"));
        let err = auth.open(Request::get(&format!("https://127.0.0.1:{port}/v1/hello", ), T)).err().unwrap();
        assert!(!handshake_failed(&err) && !pin_rejected(&err), "{err}");
    }

    #[test]
    fn urls_split_into_their_parts() {
        let u = Url::parse("https://192.168.1.20:8080/v1/video.udp?port=5").unwrap();
        assert_eq!(u, Url { tls: true, host: "192.168.1.20".into(), port: 8080, target: "/v1/video.udp?port=5".into() });
        assert_eq!(Url::parse("http://phone").unwrap().port, 80);
        assert_eq!(Url::parse("http://phone?x=1").unwrap().target, "/?x=1");
        assert!(Url::parse("ftp://x").is_err());
    }

    #[test]
    fn an_abort_ends_a_read_blocked_on_another_thread() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = conn.read(&mut buf);
            conn.write_all(b"HTTP/1.1 200 OK\r\n\r\nfirst\n").unwrap();
            std::thread::sleep(Duration::from_secs(10));
        });
        let url = format!("http://127.0.0.1:{port}/v1/sensors");
        let mut resp = PhoneAuth::new("t", "").open(Request::get(&url, Duration::from_secs(30))).unwrap();
        assert_eq!(resp.readline(100).unwrap(), b"first\n");
        let aborter = resp.aborter();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            aborter.abort();
        });
        let started = Instant::now();
        assert!(resp.readline(100).map(|l| l.is_empty()).unwrap_or(true));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
