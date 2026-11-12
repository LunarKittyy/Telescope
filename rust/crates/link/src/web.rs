//! Pages for other devices over HTTPS (browser_server.py's ensure_certificate, TlsServer and PageHandler, behind core's Web):
//! the self-signed certificate every such server shares, and a small HTTP/1.1 server on rustls with the same caps on
//! connections and the same headers on every answer.

use crate::net;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use telescope_core::web::{Certificate, PageHandler, Reply, Request, Serve, ServerStats, Socket, Step, Upgrade, Web, WebServer};

/// Browsers refuse certificates valid for more than 398 days, so it's renewed a month before it runs out.
const CERT_DAYS: i64 = 397;
const CERT_RENEW_DAYS: i64 = 30;
/// The same files Python wrote, so a browser that was told to trust the certificate keeps doing so across the switch.
const CERT_FILE: &str = "browser_camera_cert.pem";
const KEY_FILE: &str = "browser_camera_key.pem";

/// Handshake and HTTP request: nothing for this long ends the connection, a quiet kept-alive one included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// All of one request's line and headers, from its first byte: a slow drip can't keep the connection past this.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECTIONS: usize = 16;
const MAX_PER_ADDRESS: usize = 6;
/// The longest request line or header line (http.server's limit).
const MAX_LINE: u64 = 65536;
const MAX_HEADERS: usize = 100;

/// What an answer's headers say besides its type and length (PageHandler._reply).
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; connect-src 'self' wss:; img-src 'self' data:; \
                                       style-src 'self' 'unsafe-inline'; media-src 'self' blob: mediastream:; frame-ancestors 'none'";

/// The system's Web: addresses from the network adapters, a certificate made with ring and a server on rustls.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWeb;

impl Web for SystemWeb {
    fn addresses(&self) -> Vec<String> {
        net::pairing_addresses().into_iter().map(|a| a.ip).collect()
    }

    fn certificate(&self, folder: &Path, hosts: &[String]) -> Result<Certificate, String> {
        ensure_certificate(folder, hosts, unix_now())
    }

    fn serve(&self, certificate: &Certificate, serve: &Serve, handler: Arc<dyn PageHandler>) -> Result<Box<dyn WebServer>, String> {
        let config = tls_config(&certificate.cert, &certificate.key)?;
        Ok(Box::new(Server::start(config, serve, handler).map_err(|e| e.to_string())?))
    }
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

// ── The certificate ──────────────────────────────────────────────────────────

/// (cert, key) in folder, made or renewed when missing, unreadable or about to run out. The same certificate is kept as long as
/// it's valid. `now` is unix seconds.
pub fn ensure_certificate(folder: &Path, hosts: &[String], now: i64) -> Result<Certificate, String> {
    let found = Certificate { cert: folder.join(CERT_FILE), key: folder.join(KEY_FILE) };
    if certificate_fresh(&found, now) {
        return Ok(found);
    }
    let (cert, key) = make_certificate(hosts, now)?;
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    write_private(&found.key, pem("PRIVATE KEY", &key).as_bytes())?;
    write_private(&found.cert, pem("CERTIFICATE", &cert).as_bytes())?;
    Ok(found)
}

fn certificate_fresh(found: &Certificate, now: i64) -> bool {
    if !found.cert.is_file() || !found.key.is_file() || tls_config(&found.cert, &found.key).is_err() {
        return false;
    }
    let Ok(der) = CertificateDer::from_pem_file(&found.cert) else { return false };
    not_after(&der).is_some_and(|end| end - now > CERT_RENEW_DAYS * 86400)
}

/// The private key's file is only for us (os.open(..., 0o600)), written beside it and moved over, so a cut-off write never
/// leaves half a file.
fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    let tmp = path.with_file_name(name);
    let written = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(&tmp)?.write_all(data)?;
        std::fs::rename(&tmp, path)
    })();
    written.map_err(|e| e.to_string())
}

fn pem(label: &str, der: &[u8]) -> String {
    use base64::Engine;
    let text = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in text.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// A DER element: tag, length and content.
fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    match content.len() {
        n if n < 0x80 => out.push(n as u8),
        n if n < 0x100 => out.extend([0x81, n as u8]),
        n => out.extend([0x82, (n >> 8) as u8, n as u8]),
    }
    out.extend_from_slice(content);
    out
}

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.iter().flatten().copied().collect()
}

/// days since 1970-01-01 to (year, month, day), proleptic Gregorian (Howard Hinnant's civil_from_days).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (month <= 2) as i64, month, day)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = year - (month <= 2) as i64;
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A time as X.509 writes it: UTCTime up to 2049, GeneralizedTime after.
fn der_time(unix: i64) -> Vec<u8> {
    let (year, month, day) = civil_from_days(unix.div_euclid(86400));
    let secs = unix.rem_euclid(86400);
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if (1950..2050).contains(&year) {
        der(0x17, format!("{:02}{month:02}{day:02}{h:02}{m:02}{s:02}Z", year % 100).as_bytes())
    } else {
        der(0x18, format!("{year:04}{month:02}{day:02}{h:02}{m:02}{s:02}Z").as_bytes())
    }
}

/// Reads one DER element: (tag, content, what follows).
fn read_der(data: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = data.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        (rest[..n].iter().fold(0usize, |acc, b| acc << 8 | *b as usize), &rest[n..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// When the certificate stops being valid, in unix seconds.
fn not_after(cert: &[u8]) -> Option<i64> {
    let (_, cert, _) = read_der(cert)?;
    let (_, tbs, _) = read_der(cert)?;
    // The version is optional; then the serial, the signature algorithm and the issuer come before the validity
    let (tag, _, after) = read_der(tbs)?;
    let mut fields = if tag == 0xa0 { after } else { tbs };
    for _ in 0..3 {
        fields = read_der(fields)?.2;
    }
    let (_, validity, _) = read_der(fields)?;
    let (_, _, ends) = read_der(validity)?;
    let (tag, text, _) = read_der(ends)?;
    let text = std::str::from_utf8(text).ok()?.strip_suffix('Z')?;
    let digits: Vec<i64> = text.as_bytes().chunks(2).map(|c| std::str::from_utf8(c).ok()?.parse().ok()).collect::<Option<_>>()?;
    let (year, rest) = match (tag, digits.as_slice()) {
        (0x17, [yy, rest @ ..]) => (if *yy < 50 { 2000 + yy } else { 1900 + yy }, rest),
        (0x18, [hi, lo, rest @ ..]) => (hi * 100 + lo, rest),
        _ => return None,
    };
    let [month, day, h, m, s] = rest else { return None };
    Some(days_from_civil(year, *month, *day) * 86400 + h * 3600 + m * 60 + s)
}

/// A self-signed certificate and its PKCS#8 key, both DER: an ECDSA P-256 key, "Telescope" as its name and localhost and `hosts`
/// as the names it's good for, valid from a day ago for 397 days, for a server and nothing else.
fn make_certificate(hosts: &[String], now: i64) -> Result<(Vec<u8>, Vec<u8>), String> {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).map_err(|e| e.to_string())?;
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).map_err(|e| e.to_string())?;

    let oid = |bytes: &[u8]| der(0x06, bytes);
    let ecdsa_sha256 = der(0x30, &oid(&[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02]));
    let name = der(0x30, &der(0x31, &der(0x30, &concat(&[oid(&[0x55, 0x04, 0x03]), der(0x0c, b"Telescope")]))));
    let mut public = vec![0u8];
    public.extend_from_slice(key.public_key().as_ref());
    let spki = der(0x30, &concat(&[
        der(0x30, &concat(&[oid(&[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01]), oid(&[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07])])),
        der(0x03, &public),
    ]));

    let mut serial = [0u8; 16];
    rng.fill(&mut serial).map_err(|e| e.to_string())?;
    serial[0] = serial[0] & 0x3f | 0x40;

    let mut names = der(0x82, b"localhost");
    for host in hosts {
        match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => names.extend(der(0x87, &ip.octets())),
            Ok(IpAddr::V6(ip)) => names.extend(der(0x87, &ip.octets())),
            Err(_) => {}
        }
    }
    let extension = |id: &[u8], critical: bool, value: Vec<u8>| {
        let mut parts = vec![oid(id)];
        if critical {
            parts.push(der(0x01, &[0xff]));
        }
        parts.push(der(0x04, &value));
        der(0x30, &concat(&parts))
    };
    let extensions = der(0xa3, &der(0x30, &concat(&[
        extension(&[0x55, 0x1d, 0x11], false, der(0x30, &names)),
        extension(&[0x55, 0x1d, 0x13], true, der(0x30, &[])),
        extension(&[0x55, 0x1d, 0x25], false, der(0x30, &oid(&[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01]))),
    ])));

    let tbs = der(0x30, &concat(&[
        der(0xa0, &der(0x02, &[2])),
        der(0x02, &serial),
        ecdsa_sha256.clone(),
        name.clone(),
        der(0x30, &concat(&[der_time(now - 86400), der_time(now + CERT_DAYS * 86400)])),
        name,
        spki,
        extensions,
    ]));
    let signature = key.sign(&rng, &tbs).map_err(|e| e.to_string())?;
    let mut bits = vec![0u8];
    bits.extend_from_slice(signature.as_ref());
    let cert = der(0x30, &concat(&[tbs, ecdsa_sha256, der(0x03, &bits)]));
    Ok((cert, pkcs8.as_ref().to_vec()))
}

fn tls_config(cert: &Path, key: &Path) -> Result<Arc<rustls::ServerConfig>, String> {
    let certs = CertificateDer::pem_file_iter(cert).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| e.to_string())?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(config))
}

// ── The server ───────────────────────────────────────────────────────────────

/// Connections open now, in total and for each address.
#[derive(Default)]
struct Open(Mutex<Vec<IpAddr>>);

impl Open {
    /// Takes a place for a connection from this address, unless the caps say no.
    fn enter(self: &Arc<Self>, from: IpAddr) -> Option<Place> {
        let mut open = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if open.len() >= MAX_CONNECTIONS || open.iter().filter(|a| **a == from).count() >= MAX_PER_ADDRESS {
            return None;
        }
        open.push(from);
        Some(Place { open: self.clone(), from })
    }
}

struct Place {
    open: Arc<Open>,
    from: IpAddr,
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut open = self.open.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = open.iter().position(|a| *a == self.from) {
            open.swap_remove(i);
        }
    }
}

struct Server {
    stats: Arc<ServerStats>,
    stopping: Arc<AtomicBool>,
    address: SocketAddr,
}

impl Server {
    fn start(config: Arc<rustls::ServerConfig>, serve: &Serve, handler: Arc<dyn PageHandler>) -> std::io::Result<Server> {
        let host = if serve.host.is_empty() { "0.0.0.0" } else { serve.host.as_str() };
        let (listener, fell_back) = match TcpListener::bind((host, serve.port)) {
            Ok(listener) => (listener, false),
            // Taken: any free port
            Err(_) => (TcpListener::bind((host, 0))?, true),
        };
        let address = listener.local_addr()?;
        let stats = Arc::new(ServerStats::new(address.port(), serve.port, fell_back, &serve.label));
        if fell_back {
            log::warn!("{}: port {} was taken, listening on {} instead", serve.label, serve.port, address.port());
        } else {
            log::info!("{}: listening on port {}", serve.label, address.port());
        }
        let stopping = Arc::new(AtomicBool::new(false));
        let (seen, quit, open) = (stats.clone(), stopping.clone(), Arc::new(Open::default()));
        std::thread::Builder::new().name("web-server".into()).spawn(move || {
            for conn in listener.incoming() {
                if quit.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(conn) = conn else { continue };
                let Some(place) = conn.peer_addr().ok().and_then(|a| open.enter(a.ip())) else {
                    let _ = conn.shutdown(Shutdown::Both);
                    continue;
                };
                seen.note(Step::Connections);
                let (config, handler, stats) = (config.clone(), handler.clone(), seen.clone());
                let spawned = std::thread::Builder::new().name("web-connection".into()).spawn(move || {
                    let _place = place;
                    connection(conn, config, &*handler, &stats);
                });
                if let Err(e) = spawned {
                    log::warn!("A connection couldn't get a thread: {e}");
                }
            }
        })?;
        Ok(Server { stats, stopping, address })
    }
}

impl WebServer for Server {
    fn port(&self) -> u16 {
        self.address.port()
    }

    fn stats(&self) -> Arc<ServerStats> {
        self.stats.clone()
    }

    fn stop(&self) {
        if !self.stopping.swap(true, Ordering::SeqCst) {
            // The accept loop is waiting for a connection: this is the one that lets it see it's over
            let ip = if self.address.ip().is_unspecified() { IpAddr::from([127, 0, 0, 1]) } else { self.address.ip() };
            let _ = TcpStream::connect_timeout(&SocketAddr::new(ip, self.address.port()), Duration::from_secs(1));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        414 => "Request-URI Too Long",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Unknown",
    }
}

/// An HTTP date ("Thu, 29 Oct 2026 16:00:00 GMT").
fn http_date(unix: i64) -> String {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let days = unix.div_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    let secs = unix.rem_euclid(86400);
    format!("{}, {day:02} {} {year} {:02}:{:02}:{:02} GMT", DAYS[(days + 3).rem_euclid(7) as usize], MONTHS[month as usize - 1],
            secs / 3600, secs % 3600 / 60, secs % 60)
}

/// A socket whose reads end at a deadline, however many reads TLS makes to get one record: the timeout of each read is
/// what's left of it, so a peer dripping a byte at a time can't hold a connection past it.
struct Limited {
    tcp: TcpStream,
    /// The longest one read waits, deadline or not; None for as long as it takes.
    timeout: Option<Duration>,
    deadline: Option<Instant>,
}

impl Limited {
    fn new(tcp: TcpStream) -> Limited {
        Limited { tcp, timeout: Some(REQUEST_TIMEOUT), deadline: None }
    }
}

impl Read for Limited {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut wait = self.timeout;
        if let Some(deadline) = self.deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            wait = Some(wait.map_or(left, |w| w.min(left)));
        }
        self.tcp.set_read_timeout(wait)?;
        self.tcp.read(buf)
    }
}

impl Write for Limited {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.tcp.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.tcp.flush()
    }
}

/// The TLS handshake, all of it inside `within`.
fn finish_handshake(tls: &mut rustls::ServerConnection, tcp: &mut Limited, within: Duration) -> std::io::Result<()> {
    tcp.deadline = Some(Instant::now() + within);
    while tls.is_handshaking() {
        tls.complete_io(tcp)?;
    }
    tcp.deadline = None;
    Ok(())
}

/// One connection on its own thread: the TLS handshake, then requests until the other end or a timeout ends it.
fn connection(tcp: TcpStream, config: Arc<rustls::ServerConfig>, handler: &dyn PageHandler, stats: &ServerStats) {
    let _ = tcp.set_write_timeout(Some(REQUEST_TIMEOUT));
    let _ = tcp.set_nodelay(true);
    let Ok(mut tls) = rustls::ServerConnection::new(config) else { return };
    let mut tcp = Limited::new(tcp);
    if finish_handshake(&mut tls, &mut tcp, REQUEST_TIMEOUT).is_err() {
        // A browser that hasn't accepted the certificate yet hangs up here, which is expected
        stats.note(Step::TlsFailed);
        return;
    }
    let mut stream = BufReader::new(rustls::StreamOwned::new(tls, tcp));
    let upgrade = loop {
        match serve_request(&mut stream, handler, stats) {
            Served::Again => {}
            Served::Done => break None,
            Served::Upgrade(upgrade) => break Some(upgrade),
        }
    };
    if let Some(upgrade) = upgrade {
        let mut socket = Taken(stream);
        if switch_protocols(&mut socket, &upgrade.headers) {
            let run = upgrade.run;
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || run(Box::new(socket))));
        }
        return;
    }
    let tls = stream.get_mut();
    tls.conn.send_close_notify();
    let _ = tls.flush();
}

/// A connection a page took over: what was read ahead stays in the buffer, so nothing the other end sent is lost.
struct Taken(BufReader<rustls::StreamOwned<rustls::ServerConnection, Limited>>);

impl Read for Taken {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // The timeout covers this whole read, so a record dripped in pieces still gives the caller its turn
        let sock = &mut self.0.get_mut().sock;
        sock.deadline = sock.timeout.map(|t| Instant::now() + t);
        self.0.read(buf)
    }
}

impl Write for Taken {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let tls = self.0.get_mut();
        tls.sock.deadline = None;
        tls.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let tls = self.0.get_mut();
        tls.sock.deadline = None;
        tls.flush()
    }
}

impl Socket for Taken {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.0.get_mut().sock.timeout = timeout;
        Ok(())
    }
}

impl Drop for Taken {
    fn drop(&mut self) {
        let tls = self.0.get_mut();
        tls.conn.send_close_notify();
        let _ = tls.flush();
    }
}

/// The 101 answer an upgrade starts with; false when the other end is gone.
fn switch_protocols(out: &mut impl Write, headers: &[(String, String)]) -> bool {
    let mut text = String::from("HTTP/1.1 101 Switching Protocols\r\nServer: Telescope\r\n");
    text.push_str(&format!("Date: {}\r\n", http_date(unix_now())));
    for (k, v) in headers {
        text.push_str(&format!("{k}: {v}\r\n"));
    }
    text.push_str("\r\n");
    out.write_all(text.as_bytes()).and_then(|_| out.flush()).is_ok()
}

/// What one request left the connection to do.
enum Served {
    Again,
    Done,
    Upgrade(Upgrade),
}

/// A stream whose next reads can be given a deadline, past which they fail whatever trickles in.
trait Timed {
    fn limit_reads(&mut self, until: Option<Instant>);
}

impl Timed for rustls::StreamOwned<rustls::ServerConnection, Limited> {
    fn limit_reads(&mut self, until: Option<Instant>) {
        self.sock.deadline = until;
    }
}

/// The line up to its newline without it, or None when the connection ended or the line is past `MAX_LINE` (Err(true)). Err(false) is a failed read or `deadline` passing.
fn read_line<S: Read + Timed>(stream: &mut BufReader<S>, deadline: Instant) -> Result<Option<String>, bool> {
    let mut line = Vec::new();
    loop {
        if stream.buffer().is_empty() {
            if deadline <= Instant::now() {
                return Err(false);
            }
            stream.get_mut().limit_reads(Some(deadline));
        }
        let chunk = stream.fill_buf().map_err(|_| false)?;
        if chunk.is_empty() {
            return Ok(if line.is_empty() { None } else { Some(finish_line(line)) });
        }
        let (used, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        line.extend_from_slice(&chunk[..used]);
        stream.consume(used);
        if line.len() as u64 > MAX_LINE {
            return Err(true);
        }
        if done {
            return Ok(Some(finish_line(line)));
        }
    }
}

fn finish_line(mut line: Vec<u8>) -> String {
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    String::from_utf8_lossy(&line).into_owned()
}

/// One request and its answer: another request after it, the connection's end, or a page taking it over.
fn serve_request<S: Read + Write + Timed>(stream: &mut BufReader<S>, handler: &dyn PageHandler, stats: &ServerStats) -> Served {
    match serve_one(stream, handler, stats, REQUEST_TIMEOUT, HEAD_TIMEOUT) {
        Ok(true) => Served::Again,
        Ok(false) => Served::Done,
        Err(upgrade) => Served::Upgrade(upgrade),
    }
}

/// serve_request, with an upgrade as the error so the early returns stay plain bools.
fn serve_one<S: Read + Write + Timed>(stream: &mut BufReader<S>, handler: &dyn PageHandler, stats: &ServerStats, quiet: Duration, head_timeout: Duration) -> Result<bool, Upgrade> {
    // A quiet kept-alive connection gets the usual timeout, the clock for the head starts with its first byte
    stream.get_mut().limit_reads(Some(Instant::now() + quiet));
    match stream.fill_buf() {
        Ok(chunk) if !chunk.is_empty() => {}
        _ => return Ok(false),
    }
    let deadline = Instant::now() + head_timeout;
    let line = match read_line(stream, deadline) {
        Ok(Some(line)) if !line.is_empty() => line,
        Ok(Some(_)) => return Ok(false),
        Ok(None) | Err(false) => return Ok(false),
        Err(true) => return Ok(refuse(stream.get_mut(), 414, "Request-URI Too Long")),
    };
    let words: Vec<&str> = line.split_whitespace().collect();
    let [method, target, version] = words.as_slice() else {
        return Ok(refuse(stream.get_mut(), 400, "Bad request syntax"));
    };
    let Some((major, minor)) = version.strip_prefix("HTTP/").and_then(|v| v.split_once('.')).and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))) else {
        return Ok(refuse(stream.get_mut(), 400, "Bad request version"));
    };
    if major >= 2 {
        return Ok(refuse(stream.get_mut(), 505, "Invalid HTTP version"));
    }
    let mut headers = Vec::new();
    let mut lines = 0;
    loop {
        match read_line(stream, deadline) {
            Ok(Some(l)) if l.is_empty() => break,
            Ok(Some(l)) => {
                lines += 1;
                if lines > MAX_HEADERS {
                    return Ok(refuse(stream.get_mut(), 431, "Too many headers"));
                }
                if let Some((k, v)) = l.split_once(':') {
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            Ok(None) | Err(false) => return Ok(false),
            Err(true) => return Ok(refuse(stream.get_mut(), 431, "Line too long")),
        }
    }
    stream.get_mut().limit_reads(None);
    let request = Request { method: method.to_string(), target: target.to_string(), headers };
    let connection = request.header("Connection").unwrap_or("").to_ascii_lowercase();
    // HTTP/1.1 stays open unless asked to close; older ones close unless asked to keep alive
    let keep = if (major, minor) >= (1, 1) { connection != "close" } else { connection == "keep-alive" };
    if !matches!(request.method.as_str(), "GET" | "HEAD") {
        let text = format!("Unsupported method ('{}')", request.method);
        return Ok(refuse(stream.get_mut(), 501, &text));
    }
    let head = request.method == "HEAD";
    if let Ok(Some(upgrade)) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.upgrade(&request, stats))) {
        return Err(upgrade);
    }
    let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.handle(&request, stats)));
    Ok(match reply {
        Ok(reply) => answer(stream.get_mut(), head, reply, false) && keep,
        Err(_) => {
            log::error!("A page handler failed");
            refuse(stream.get_mut(), 500, "Something went wrong")
        }
    })
}

/// An error that ends the connection (send_error); always false, so a caller can return it.
fn refuse(out: &mut impl Write, status: u16, text: &str) -> bool {
    answer(out, false, Reply::text(status, text), true);
    false
}

/// Writes an answer with the headers every page gets; false when the other end is gone. `close` says so in the answer.
fn answer(out: &mut impl Write, head: bool, reply: Reply, close: bool) -> bool {
    let mut text = format!(
        "HTTP/1.1 {} {}\r\nServer: Telescope\r\nDate: {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: {CONTENT_SECURITY_POLICY}\r\n",
        reply.status, reason(reply.status), http_date(unix_now()), reply.content_type, reply.body.len());
    if close {
        text.push_str("Connection: close\r\n");
    }
    text.push_str("\r\n");
    let mut bytes = text.into_bytes();
    if !head {
        bytes.extend_from_slice(&reply.body);
    }
    out.write_all(&bytes).and_then(|_| out.flush()).is_ok()
}

#[cfg(test)]
#[path = "web_tests.rs"]
mod tests;
