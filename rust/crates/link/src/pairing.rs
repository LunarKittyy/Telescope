//! The QR pairing server: binds a port, makes a nonce and bearer token, waits for the phone's POST at
//! /pair/<nonce> proving it read the token, and hands back the phone that paired.

use crate::net::{self, PairingAddress};
use crate::util;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub const PAIRING_PORT: u16 = 7479;
/// How many names of phones with an older app wait to be shown at once (a replayed request can't grow the list).
const MAX_OLD_PHONES: usize = 4;
/// Where phones from before the port move pair. Listen only: whoever knocks there is told to update, nothing pairs.
pub const OLD_PAIRING_PORT: u16 = 8765;
/// Desktop and app ship together.
/// 3: the offer names this computer (computer_id/computer_name), the phone answers with its phone_id.
/// 4: the phone answers with its TLS certificate's fingerprint and a proof of the token, never the token itself.
pub const PAIRING_PROTOCOL_VERSION: u32 = 4;
const MAX_BODY_BYTES: i64 = 16 * 1024;
/// Read before closing an error response: Windows can reset a socket that still has unread bytes.
const DRAIN_LIMIT: i64 = 1024 * 1024;
/// How much a refused connection may still send, and for how long, before it is closed anyway.
const LINGER_BYTES: u64 = 1024 * 1024;
const LINGER_TIME: Duration = Duration::from_secs(2);

/// How many connections it takes and how long it waits, like the phone's PendingLimiter.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_total: usize,
    /// USB pairing all arrives from 127.0.0.1 through adb reverse.
    pub max_per_address: usize,
    /// A connection that stalls (a phone dropping off Wi-Fi mid-request, a port scanner) is cut off after this...
    pub request_timeout: Duration,
    /// ...and one still sending, however slowly, after this. The phone's request is a few hundred bytes.
    pub deadline: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_total: 32, max_per_address: 4, request_timeout: Duration::from_secs(10), deadline: Duration::from_secs(15) }
    }
}

/// What to show as a QR code, and what the phone's POST is checked against.
#[derive(Clone, Debug)]
pub struct PairingOffer {
    pub payload: String,
    pub port: u16,
    /// Where phones from before the port move knock (they read the offer's "port"), when that port could be taken.
    pub old_port: Option<u16>,
    pub nonce: String,
    pub token: String,
    pub candidates: Vec<PairingAddress>,
    /// The same offer advertising only 127.0.0.1: what the adb broadcast carries (reached through adb reverse).
    pub usb_payload: String,
}

/// The phone that paired.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingResult {
    pub name: String,
    pub ips: Vec<String>,
    pub token: String,
    /// Where the POST came from: one of the phone's addresses that reaches this computer.
    pub source_ip: String,
    pub phone_id: String,
    /// The phone's certificate fingerprint; every later connection must present this certificate.
    pub cert_sha256: String,
}

impl PairingResult {
    /// The phone to save: the address the POST came from is its active one when the phone listed it.
    pub fn phone(&self) -> telescope_core::source::PairedPhone {
        let active = self.ips.contains(&self.source_ip).then(|| self.source_ip.clone());
        telescope_core::source::PairedPhone { id: self.phone_id.clone(), name: self.name.clone(), token: self.token.clone(),
                                              ips: self.ips.clone(), active_ip: active, cert_sha256: self.cert_sha256.clone() }
    }
}

/// The HMAC under the token over what the phone claims; the phone's pairingProof() must match.
pub fn pairing_proof(token: &str, nonce: &str, phone_id: &str, cert_sha256: &str) -> String {
    util::pairing_proof(token, nonce, phone_id, cert_sha256)
}

struct Shared {
    nonce: String,
    token: String,
    limits: Limits,
    open: Mutex<HashMap<String, usize>>,
    /// One offer pairs one phone; a second (two plugged in, both sent the USB offer) is refused.
    claimed: Mutex<Option<String>>,
    on_paired: Box<dyn Fn(PairingResult) + Send + Sync>,
    /// Names of phones that knocked on the old port, until taken.
    old_phones: Mutex<Vec<String>>,
    stop: AtomicBool,
}

impl Shared {
    /// Stops serving. It waits for a request being accepted, so what was accepted is queued before this returns.
    fn halt(&self) {
        let _accepting = self.claimed.lock().unwrap();
        self.stop.store(true, Ordering::SeqCst);
    }

    fn claim(&self, host: &str) -> bool {
        let mut open = self.open.lock().unwrap();
        if open.values().sum::<usize>() >= self.limits.max_total || open.get(host).copied().unwrap_or(0) >= self.limits.max_per_address {
            return false;
        }
        *open.entry(host.to_string()).or_insert(0) += 1;
        true
    }

    fn release(&self, host: &str) {
        let mut open = self.open.lock().unwrap();
        match open.get(host).copied() {
            Some(n) if n > 1 => {
                open.insert(host.to_string(), n - 1);
            }
            _ => {
                open.remove(host);
            }
        }
    }
}

/// The pairing server for one Add device dialog.
pub struct PairingServer {
    pub offer: PairingOffer,
    shared: Arc<Shared>,
}

impl PairingServer {
    /// Binds and starts serving. candidates is empty when this computer has no usable network; usb_payload still works
    /// then, over adb reverse. advertise replaces the adapter scan.
    pub fn start(old_port: Option<u16>, computer_id: &str, computer_name: &str, advertise: Option<Vec<PairingAddress>>,
                 on_paired: impl Fn(PairingResult) + Send + Sync + 'static) -> std::io::Result<PairingServer> {
        PairingServer::start_scanning(PAIRING_PORT, old_port, computer_id, computer_name, advertise, net::pairing_addresses,
                                      Limits::default(), on_paired)
    }

    /// The same on a given port (0: any), falling back to any free one.
    pub fn start_on(port: u16, computer_id: &str, computer_name: &str, advertise: Option<Vec<PairingAddress>>, limits: Limits,
                    on_paired: impl Fn(PairingResult) + Send + Sync + 'static) -> std::io::Result<PairingServer> {
        PairingServer::start_scanning(port, None, computer_id, computer_name, advertise, net::pairing_addresses, limits, on_paired)
    }

    /// The same with scan standing for the adapter scan, and old_port (when given) also listening for phones that still
    /// use the old port. Without it, or when it's taken, old phones are sent to the pairing port and pair as usual.
    pub(crate) fn start_scanning(port: u16, old_port: Option<u16>, computer_id: &str, computer_name: &str, advertise: Option<Vec<PairingAddress>>,
                      scan: impl FnOnce() -> Vec<PairingAddress>, limits: Limits,
                      on_paired: impl Fn(PairingResult) + Send + Sync + 'static) -> std::io::Result<PairingServer> {
        let candidates = advertise.unwrap_or_else(scan);
        // A fresh nonce per offer, so a LAN peer that hasn't scanned the current code can't add itself
        let nonce = util::token_urlsafe(16);
        // The token the phone requires on every /v1 request once paired; it only travels in the QR code and over pinned TLS
        let token = util::token_urlsafe(32);
        // The fixed port (reused straight after the last pairing, which leaves it in TIME_WAIT), else any free one
        let listener = bind(port).or_else(|_| bind(0))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let old_listener = old_port.filter(|old| *old != port).and_then(|old| bind(old).ok()).and_then(|l| l.set_nonblocking(true).ok().map(|_| l));
        let old_port = old_listener.as_ref().and_then(|l| l.local_addr().ok()).map(|a| a.port());
        let payload_for = |addrs: &[PairingAddress]| {
            util::python_json(&json!({
                "version": PAIRING_PROTOCOL_VERSION, "port": old_port.unwrap_or(port), "pairing_port": port, "candidates": addrs,
                "nonce": nonce, "token": token, "computer_id": computer_id, "computer_name": computer_name,
            }))
        };
        let mut usb = vec![PairingAddress { ip: "127.0.0.1".into(), interface: "USB (adb)".into(), kind: "other" }];
        usb.extend(candidates.iter().cloned());
        let offer = PairingOffer { payload: payload_for(&candidates), usb_payload: payload_for(&usb), port, old_port, nonce: nonce.clone(),
                                   token: token.clone(), candidates };
        let shared = Arc::new(Shared { nonce, token, limits, open: Mutex::default(), claimed: Mutex::default(), on_paired: Box::new(on_paired),
                                          old_phones: Mutex::default(), stop: AtomicBool::new(false) });
        let serving = shared.clone();
        std::thread::Builder::new().name("pairing-server".into()).spawn(move || serve(listener, serving, false))?;
        // From here the server's drop stops the thread above, if the one below can't start
        let server = PairingServer { offer, shared: shared.clone() };
        if let Some(listener) = old_listener {
            std::thread::Builder::new().name("pairing-server-old".into()).spawn(move || serve(listener, shared, true))?;
        }
        Ok(server)
    }

    /// Refuses every request from now on, ones on a connection already open too (idempotent); the port closes within a
    /// moment.
    pub fn stop(&self) {
        self.shared.halt();
    }

    /// The name of a phone that knocked on the old port since the last call: its app is older than this one.
    pub fn take_old_phone(&self) -> Option<String> {
        let mut seen = self.shared.old_phones.lock().unwrap();
        (!seen.is_empty()).then(|| seen.remove(0))
    }

    /// How many connections each address has open now.
    pub fn open_connections(&self) -> HashMap<String, usize> {
        self.shared.open.lock().unwrap().clone()
    }
}

impl Drop for PairingServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(("0.0.0.0", port))
}

/// old: the listen-only socket on the old port.
fn serve(listener: TcpListener, shared: Arc<Shared>, old: bool) {
    while !shared.stop.load(Ordering::SeqCst) {
        let (conn, addr) = match listener.accept() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(_) => {
                // Out of descriptors fails at once every time
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
        };
        let host = addr.ip().to_string();
        if !shared.claim(&host) {
            let _ = conn.shutdown(Shutdown::Both);
            continue;
        }
        let shared = shared.clone();
        std::thread::spawn(move || {
            let _ = conn.set_nonblocking(false);
            // A deadline on the whole request: each byte resets the read timeout, so only this stops a trickle
            let (done, finished) = mpsc::channel::<()>();
            if let Ok(guard) = conn.try_clone() {
                let deadline = shared.limits.deadline;
                std::thread::spawn(move || {
                    if finished.recv_timeout(deadline) == Err(mpsc::RecvTimeoutError::Timeout) {
                        let _ = guard.shutdown(Shutdown::Both);
                    }
                });
            }
            let _ = handle(&conn, &host, &shared, old);
            let _ = done.send(());
            shared.release(&host);
        });
    }
}

fn respond(mut conn: &TcpStream, status: &str, body: &[u8]) -> std::io::Result<()> {
    write!(conn, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
    conn.write_all(body)?;
    conn.flush()
}

/// Answers an early refusal, then reads and drops what the client is still sending before closing. Closing with unread
/// bytes makes Windows reset the connection, which throws away the answer before the client has read it.
fn refuse(conn: &TcpStream, status: &str) -> std::io::Result<()> {
    respond(conn, status, b"")?;
    let _ = conn.shutdown(Shutdown::Write);
    let end = Instant::now() + LINGER_TIME;
    let (mut reader, mut left) = (conn, LINGER_BYTES);
    let mut buf = [0u8; 8192];
    while left > 0 {
        let Some(wait) = end.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else { break };
        if conn.set_read_timeout(Some(wait)).is_err() {
            break;
        }
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => left = left.saturating_sub(n as u64),
        }
    }
    Ok(())
}

fn drain(reader: &mut impl Read, length: Option<i64>) {
    if let Some(n) = length.filter(|n| (0..=DRAIN_LIMIT).contains(n)) {
        let _ = std::io::copy(&mut reader.take(n as u64), &mut std::io::sink());
    }
}

fn handle(conn: &TcpStream, source_ip: &str, shared: &Shared, old: bool) -> std::io::Result<()> {
    conn.set_read_timeout(Some(shared.limits.request_timeout))?;
    let mut reader = BufReader::new(conn.try_clone()?);
    // The request line and headers are cut off at the same sizes as an answer from a phone (https.rs)
    let (deadline, mut head_bytes) = (Instant::now() + shared.limits.deadline, 0usize);
    let line = match crate::https::read_head_line(&mut reader, deadline, &mut head_bytes) {
        Ok(line) => String::from_utf8_lossy(&line).into_owned(),
        Err(crate::https::Error::Bad(_)) => return refuse(conn, "431 Request Header Fields Too Large"),
        Err(_) => return Ok(()),
    };
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let mut length_header: Option<String> = None;
    loop {
        let header = match crate::https::read_head_line(&mut reader, deadline, &mut head_bytes) {
            Ok(header) => String::from_utf8_lossy(&header).into_owned(),
            Err(crate::https::Error::Bad(_)) => return refuse(conn, "431 Request Header Fields Too Large"),
            Err(_) => return Ok(()),
        };
        if header.is_empty() || header.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = header.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                length_header = Some(v.trim().to_string());
            }
        }
    }
    match method.as_str() {
        "GET" => return respond(conn, "200 OK", b"Telescope pairing server"),
        "POST" => {}
        _ => return refuse(conn, "501 Not Implemented"),
    }
    let length: Option<i64> = length_header.and_then(|v| v.parse().ok());
    if path != format!("/pair/{}", shared.nonce) {
        drain(&mut reader, length);
        return refuse(conn, "404 Not Found");
    }
    let Some(length) = length else { return refuse(conn, "411 Length Required") };
    if !(0..=MAX_BODY_BYTES).contains(&length) {
        drain(&mut reader, Some(length));
        return refuse(conn, "413 Payload Too Large");
    }
    let mut body = vec![0u8; length as usize];
    reader.read_exact(&mut body)?;
    let Some(result) = check(&body, &shared.nonce, &shared.token, source_ip) else { return respond(conn, "400 Bad Request", b"") };
    if old && shared.stop.load(Ordering::SeqCst) {
        return respond(conn, "404 Not Found", b"");
    }
    if old {
        // Nothing pairs here: the phone proved it read this offer, so its name is worth showing
        let mut seen = shared.old_phones.lock().unwrap();
        if seen.len() < MAX_OLD_PHONES && !seen.contains(&result.name) {
            seen.push(result.name);
        }
        drop(seen);
        return respond(conn, "426 Upgrade Required", b"");
    }
    // Checked and queued under one lock with stop(), so a 200 always has its result waiting for the one who revoked
    let status = {
        let mut claimed = shared.claimed.lock().unwrap();
        if shared.stop.load(Ordering::SeqCst) {
            "404 Not Found"
        } else if claimed.as_deref().is_some_and(|id| id != result.phone_id) {
            "409 Conflict"
        } else {
            *claimed = Some(result.phone_id.clone());
            (shared.on_paired)(result);
            "200 OK"
        }
    };
    respond(conn, status, if status == "200 OK" { b"OK" } else { b"" })
}

/// str(value) as Python would write it, for the few fields the server reads with str().
fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True".into() } else { "False".into() },
        other => other.to_string(),
    }
}

fn check(body: &[u8], nonce: &str, token: &str, source_ip: &str) -> Option<PairingResult> {
    let data: Value = serde_json::from_slice(body).ok()?;
    let data = data.as_object()?;
    let name = util::clean_name(&data.get("name").map(py_str).unwrap_or_else(|| "Phone".into()));
    let phone_id = data.get("phone_id").map(py_str).unwrap_or_default().trim().to_string();
    let mut ips: Vec<String> = Vec::new();
    for ip in data.get("ips").map(|v| v.as_array().cloned()).unwrap_or(Some(Vec::new()))? {
        let ip = py_str(&ip).trim().to_string();
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    let cert = data.get("cert_sha256").map(py_str).unwrap_or_default();
    let proof = data.get("proof").map(py_str).unwrap_or_default();
    if name.is_empty() || phone_id.is_empty() || !ips.iter().all(|ip| net::valid_ipv4(ip)) || !util::is_fingerprint(&cert) {
        return None;
    }
    if !util::same(&proof, &pairing_proof(token, nonce, &phone_id, &cert)) {
        return None;
    }
    Some(PairingResult { name, ips, token: token.to_string(), source_ip: source_ip.to_string(), phone_id, cert_sha256: cert })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const PIN: &str = "abababababababababababababababababababababababababababababababab";

    fn candidates() -> Vec<PairingAddress> {
        vec![PairingAddress { ip: "192.168.1.42".into(), interface: "Wi-Fi".into(), kind: "lan" },
             PairingAddress { ip: "100.90.12.34".into(), interface: "tailscale0".into(), kind: "tailscale" }]
    }

    fn server_with(limits: Limits, advertise: Vec<PairingAddress>) -> (PairingServer, Arc<Mutex<Vec<PairingResult>>>) {
        let paired: Arc<Mutex<Vec<PairingResult>>> = Arc::default();
        let record = paired.clone();
        let server = PairingServer::start_on(0, "", "", Some(advertise), limits, move |r| record.lock().unwrap().push(r)).unwrap();
        (server, paired)
    }

    fn server() -> (PairingServer, Arc<Mutex<Vec<PairingResult>>>) {
        server_with(Limits::default(), candidates())
    }

    fn body(offer: &PairingOffer, name: &str, ips: &[&str], phone_id: &str, cert: &str, proof: Option<&str>) -> Vec<u8> {
        let proof = proof.map(str::to_string).unwrap_or_else(|| pairing_proof(&offer.token, &offer.nonce, phone_id, cert));
        json!({"name": name, "ips": ips, "phone_id": phone_id, "cert_sha256": cert, "proof": proof}).to_string().into_bytes()
    }

    fn pair_body(offer: &PairingOffer) -> Vec<u8> {
        body(offer, "Phone", &[], "ph-1", PIN, None)
    }

    fn raw(port: u16, request: &[u8]) -> u16 {
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        conn.write_all(request).unwrap();
        let mut reply = String::new();
        let _ = conn.read_to_string(&mut reply);
        reply.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    fn post(port: u16, path: &str, body: &[u8]) -> u16 {
        let mut request = format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        request.extend(body);
        raw(port, &request)
    }

    fn wait_until(cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < deadline, "the pairing server did not reach the expected state");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn start_without_network_still_offers_usb_pairing() {
        let server = PairingServer::start_scanning(0, None, "", "", None, Vec::new, Limits::default(), |_| {}).unwrap();
        assert!(server.offer.candidates.is_empty());
        let usb: Value = serde_json::from_str(&server.offer.usb_payload).unwrap();
        assert_eq!(usb["candidates"], json!([{"ip": "127.0.0.1", "interface": "USB (adb)", "kind": "other"}]));
    }

    #[test]
    fn payload_is_version_4_with_every_candidate() {
        let (server, _) = server();
        let offer = &server.offer;
        let payload: Value = serde_json::from_str(&offer.payload).unwrap();
        assert_eq!(payload["version"], 4);
        assert!(payload.get("computer_id").is_some() && payload.get("computer_name").is_some());
        assert_eq!((payload["port"].as_u64(), payload["nonce"].as_str(), payload["token"].as_str()),
                   (Some(offer.port as u64), Some(offer.nonce.as_str()), Some(offer.token.as_str())));
        assert_eq!(payload["candidates"], json!([{"ip": "192.168.1.42", "interface": "Wi-Fi", "kind": "lan"},
                                                 {"ip": "100.90.12.34", "interface": "tailscale0", "kind": "tailscale"}]));
        assert_eq!(offer.candidates, candidates());
        let usb: Value = serde_json::from_str(&offer.usb_payload).unwrap();
        assert_eq!(usb["candidates"][0]["ip"], "127.0.0.1");
        assert_eq!(usb["candidates"][1], payload["candidates"][0]);
        assert_eq!(usb["candidates"][2], payload["candidates"][1]);
    }

    #[test]
    fn the_payload_is_written_like_pythons_json() {
        let (server, _) = server_with(Limits::default(), vec![]);
        assert!(server.offer.payload.starts_with(&format!("{{\"version\": 4, \"port\": {}, \"pairing_port\": {}, \"candidates\": [], \"nonce\": ", server.offer.port, server.offer.port)));
    }

    #[test]
    fn start_with_advertised_addresses_skips_discovery() {
        let usb = vec![PairingAddress { ip: "127.0.0.1".into(), interface: "USB (adb)".into(), kind: "other" }];
        let scan = || panic!("should not enumerate interfaces");
        let server = PairingServer::start_scanning(0, None, "", "", Some(usb), scan, Limits::default(), |_| {}).unwrap();
        let payload: Value = serde_json::from_str(&server.offer.payload).unwrap();
        assert_eq!(payload["candidates"], json!([{"ip": "127.0.0.1", "interface": "USB (adb)", "kind": "other"}]));
    }

    #[test]
    fn empty_ips_in_payload_is_accepted() {
        let (server, paired) = server();
        let offer = server.offer.clone();
        assert_eq!(post(offer.port, &format!("/pair/{}", offer.nonce), &pair_body(&offer)), 200);
        wait_until(|| !paired.lock().unwrap().is_empty());
        assert_eq!(*paired.lock().unwrap(), [PairingResult { name: "Phone".into(), ips: vec![], token: offer.token.clone(),
                                                             source_ip: "127.0.0.1".into(), phone_id: "ph-1".into(), cert_sha256: PIN.into() }]);
    }

    #[test]
    fn wrong_nonce_is_rejected() {
        let (server, _) = server();
        assert_eq!(post(server.offer.port, "/pair/not-the-nonce", b"{}"), 404);
    }

    #[test]
    fn oversized_body_is_rejected() {
        let (server, _) = server();
        assert_eq!(post(server.offer.port, &format!("/pair/{}", server.offer.nonce), &vec![b'x'; 17 * 1024]), 413);
    }

    /// Sends `request`, ignoring a reset from the server cutting in before the end, and gives the status it answered with.
    fn raw_cut_off(port: u16, request: &[u8]) -> u16 {
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        for chunk in request.chunks(4096) {
            if conn.write_all(chunk).is_err() {
                break;
            }
        }
        let mut reply = String::new();
        let _ = conn.read_to_string(&mut reply);
        reply.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    #[test]
    fn a_request_line_without_an_end_is_cut_off() {
        let (server, _) = server();
        let request = [b"GET /".as_slice(), &vec![b'a'; 200 * 1024]].concat();
        assert_eq!(raw_cut_off(server.offer.port, &request), 431);
    }

    #[test]
    fn a_header_line_past_the_limit_is_cut_off() {
        let (server, _) = server();
        let request = format!("GET / HTTP/1.1\r\nX-Long: {}\r\n\r\n", "a".repeat(20 * 1024));
        assert_eq!(raw_cut_off(server.offer.port, request.as_bytes()), 431);
    }

    #[test]
    fn a_head_made_of_many_short_headers_is_cut_off() {
        let (server, _) = server();
        let headers: String = (0..200).map(|i| format!("X-{i}: {}\r\n", "a".repeat(1000))).collect();
        let request = format!("GET / HTTP/1.1\r\n{headers}\r\n");
        assert_eq!(raw_cut_off(server.offer.port, request.as_bytes()), 431);
    }

    #[test]
    fn a_head_near_the_limit_still_pairs() {
        let (server, paired) = server();
        let offer = server.offer.clone();
        let body = pair_body(&offer);
        let request = [format!("POST /pair/{} HTTP/1.1\r\nX-Pad: {}\r\nContent-Length: {}\r\n\r\n", offer.nonce, "a".repeat(15 * 1024), body.len()).into_bytes(), body].concat();
        assert_eq!(raw(offer.port, &request), 200);
        wait_until(|| !paired.lock().unwrap().is_empty());
    }

    #[test]
    fn missing_content_length_is_rejected() {
        let (server, _) = server();
        let request = format!("POST /pair/{} HTTP/1.1\r\nContent-Length: notanumber\r\n\r\n", server.offer.nonce);
        assert_eq!(raw(server.offer.port, request.as_bytes()), 411);
    }

    #[test]
    fn invalid_ip_in_payload_is_rejected() {
        let (server, _) = server();
        let offer = &server.offer;
        assert_eq!(post(offer.port, &format!("/pair/{}", offer.nonce), &body(offer, "Phone", &["not-an-ip"], "ph-1", PIN, None)), 400);
    }

    #[test]
    fn a_proof_made_with_another_token_is_rejected() {
        let (server, paired) = server();
        let offer = &server.offer;
        let forged = pairing_proof("wrong-token", &offer.nonce, "ph-1", PIN);
        assert_eq!(post(offer.port, &format!("/pair/{}", offer.nonce), &body(offer, "Phone", &[], "ph-1", PIN, Some(&forged))), 400);
        assert!(paired.lock().unwrap().is_empty());
    }

    #[test]
    fn the_proof_covers_the_fingerprint() {
        let (server, paired) = server();
        let offer = &server.offer;
        let real = pairing_proof(&offer.token, &offer.nonce, "ph-1", PIN);
        assert_eq!(post(offer.port, &format!("/pair/{}", offer.nonce), &body(offer, "Phone", &[], "ph-1", &"cd".repeat(32), Some(&real))), 400);
        assert!(paired.lock().unwrap().is_empty());
    }

    #[test]
    fn a_phone_from_before_tls_is_rejected() {
        let (server, paired) = server();
        let offer = &server.offer;
        let v3 = json!({"name": "Phone", "ips": [], "phone_id": "ph-1", "token": offer.token}).to_string();
        assert_eq!(post(offer.port, &format!("/pair/{}", offer.nonce), v3.as_bytes()), 400);
        assert!(paired.lock().unwrap().is_empty());
    }

    #[test]
    fn a_malformed_fingerprint_is_rejected() {
        let (server, paired) = server();
        let offer = &server.offer;
        assert_eq!(post(offer.port, &format!("/pair/{}", offer.nonce), &body(offer, "Phone", &[], "ph-1", "not-hex", None)), 400);
        assert!(paired.lock().unwrap().is_empty());
    }

    fn with_old_port(old: Option<u16>) -> (PairingServer, Arc<Mutex<Vec<PairingResult>>>) {
        let paired: Arc<Mutex<Vec<PairingResult>>> = Arc::default();
        let record = paired.clone();
        let server = PairingServer::start_scanning(0, old, "", "", Some(candidates()), Vec::new, Limits::default(),
                                                   move |r| record.lock().unwrap().push(r)).unwrap();
        (server, paired)
    }

    #[test]
    fn the_ports_are_the_new_block_and_the_old_pairing_port() {
        assert_eq!((PAIRING_PORT, OLD_PAIRING_PORT), (7479, 8765));
    }

    #[test]
    fn a_phone_knocking_on_the_old_port_is_not_paired_and_is_named() {
        let (server, paired) = with_old_port(Some(0));
        let offer = server.offer.clone();
        let old = offer.old_port.unwrap();
        assert_ne!(old, offer.port);
        assert_eq!(server.take_old_phone(), None);
        assert_eq!(post(old, &format!("/pair/{}", offer.nonce), &body(&offer, "Old Phone", &[], "ph-1", PIN, None)), 426);
        assert_eq!(server.take_old_phone().as_deref(), Some("Old Phone"));
        assert!(paired.lock().unwrap().is_empty());
        assert_eq!(server.take_old_phone(), None);
        // The real port still pairs the same offer
        assert_eq!(post(offer.port, &format!("/pair/{}", offer.nonce), &pair_body(&offer)), 200);
    }

    #[test]
    fn a_phone_knocking_again_is_named_once() {
        let (server, _) = with_old_port(Some(0));
        let offer = server.offer.clone();
        let old = offer.old_port.unwrap();
        for _ in 0..3 {
            assert_eq!(post(old, &format!("/pair/{}", offer.nonce), &body(&offer, "Old Phone", &[], "ph-1", PIN, None)), 426);
        }
        assert_eq!(server.take_old_phone().as_deref(), Some("Old Phone"));
        assert_eq!(server.take_old_phone(), None);
    }

    #[test]
    fn only_a_few_names_wait_to_be_shown() {
        let (server, _) = with_old_port(Some(0));
        let offer = server.offer.clone();
        let old = offer.old_port.unwrap();
        for i in 0..MAX_OLD_PHONES + 3 {
            assert_eq!(post(old, &format!("/pair/{}", offer.nonce), &body(&offer, &format!("Phone {i}"), &[], "ph-1", PIN, None)), 426);
        }
        let mut taken = 0;
        while server.take_old_phone().is_some() {
            taken += 1;
        }
        assert_eq!(taken, MAX_OLD_PHONES);
    }

    #[test]
    fn the_old_port_names_only_phones_that_read_the_offer() {
        let (server, _) = with_old_port(Some(0));
        let offer = server.offer.clone();
        let old = offer.old_port.unwrap();
        assert_eq!(post(old, "/pair/not-the-nonce", b"{}"), 404);
        assert_eq!(post(old, &format!("/pair/{}", offer.nonce), &body(&offer, "Forger", &[], "ph-1", PIN, Some("bad"))), 400);
        assert_eq!(server.take_old_phone(), None);
    }

    #[test]
    fn the_offer_tells_old_phones_the_old_port_and_new_ones_the_pairing_port() {
        let (server, _) = with_old_port(Some(0));
        let offer = &server.offer;
        for raw in [&offer.payload, &offer.usb_payload] {
            let data: Value = serde_json::from_str(raw).unwrap();
            assert_eq!(data["port"], offer.old_port.unwrap());
            assert_eq!(data["pairing_port"], offer.port);
        }
    }

    #[test]
    fn an_old_port_of_zero_is_always_opened_even_when_every_other_port_is_taken() {
        // Asking for a port by number races with other processes; 0 lets the system pick one that is free right now
        let hogs: Vec<TcpListener> = (0..20).filter_map(|_| TcpListener::bind(("0.0.0.0", 0)).ok()).collect();
        let (server, _) = with_old_port(Some(0));
        let old = server.offer.old_port.expect("the old port is opened");
        assert!(hogs.iter().all(|h| h.local_addr().unwrap().port() != old));
    }

    #[test]
    fn without_the_old_port_old_phones_get_the_pairing_port() {
        let taken = TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let (server, _) = with_old_port(Some(taken.local_addr().unwrap().port()));
        assert_eq!(server.offer.old_port, None);
        let data: Value = serde_json::from_str(&server.offer.payload).unwrap();
        assert_eq!((data["port"].as_u64(), data["pairing_port"].as_u64()), (Some(server.offer.port as u64), Some(server.offer.port as u64)));
    }

    #[test]
    fn stopping_closes_the_old_port_too() {
        let (server, _) = with_old_port(Some(0));
        let offer = server.offer.clone();
        server.stop();
        let deadline = Instant::now() + Duration::from_secs(2);
        while TcpStream::connect(("127.0.0.1", offer.old_port.unwrap())).is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(TcpStream::connect(("127.0.0.1", offer.old_port.unwrap())).is_err());
    }

    #[test]
    fn valid_payload_pairs_and_invokes_callback() {
        let (server, paired) = server();
        let offer = server.offer.clone();
        assert_eq!(post(offer.port, &format!("/pair/{}", offer.nonce), &body(&offer, "MyPhone", &["192.168.1.55"], "ph-1", PIN, None)), 200);
        wait_until(|| !paired.lock().unwrap().is_empty());
        assert_eq!(*paired.lock().unwrap(), [PairingResult { name: "MyPhone".into(), ips: vec!["192.168.1.55".into()], token: offer.token,
                                                             source_ip: "127.0.0.1".into(), phone_id: "ph-1".into(), cert_sha256: PIN.into() }]);
    }

    #[test]
    fn a_stalled_connection_does_not_block_the_phone_pairing() {
        let (server, paired) = server();
        let offer = &server.offer;
        let _stalled = TcpStream::connect(("127.0.0.1", offer.port)).unwrap();
        assert_eq!(post(offer.port, &format!("/pair/{}", offer.nonce), &pair_body(offer)), 200);
        wait_until(|| paired.lock().unwrap().len() == 1);
    }

    fn closed_by_server(sock: &mut TcpStream, within: Duration) -> bool {
        sock.set_read_timeout(Some(within)).unwrap();
        let mut b = [0u8; 1];
        match sock.read(&mut b) {
            Ok(0) => true,
            Ok(_) => false,
            Err(e) => !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
        }
    }

    #[test]
    fn a_peer_trickling_bytes_is_cut_off_at_the_deadline() {
        let limits = Limits { deadline: Duration::from_millis(500), ..Limits::default() };
        let (server, _) = server_with(limits, vec![]);
        let mut sock = TcpStream::connect(("127.0.0.1", server.offer.port)).unwrap();
        let start = Instant::now();
        let _ = sock.write_all(b"POST /pair/guess HTTP/1.1\r\n");
        while start.elapsed() < Duration::from_secs(3) {
            if sock.write_all(b"X").is_err() || closed_by_server(&mut sock, Duration::from_millis(100)) {
                break;
            }
        }
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn too_many_connections_from_one_address_are_refused_and_freed_again() {
        let limits = Limits { max_per_address: 2, ..Limits::default() };
        let (server, paired) = server_with(limits, vec![]);
        let port = server.offer.port;
        let held: Vec<TcpStream> = (0..2).map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap()).collect();
        wait_until(|| server.open_connections().get("127.0.0.1") == Some(&2));
        let mut extra = TcpStream::connect(("127.0.0.1", port)).unwrap();
        assert!(closed_by_server(&mut extra, Duration::from_secs(2)));
        drop(held);
        wait_until(|| server.open_connections().is_empty());
        assert_eq!(post(port, &format!("/pair/{}", server.offer.nonce), &pair_body(&server.offer)), 200);
        wait_until(|| !paired.lock().unwrap().is_empty());
    }

    #[test]
    fn a_second_phone_is_refused_once_one_paired() {
        let (server, paired) = server();
        let offer = &server.offer;
        let path = format!("/pair/{}", offer.nonce);
        assert_eq!(post(offer.port, &path, &body(offer, "Phone", &[], "ph-1", PIN, None)), 200);
        assert_eq!(post(offer.port, &path, &body(offer, "Phone", &[], "ph-2", PIN, None)), 409);
        // The first phone retrying (its answer got lost) still gets through
        assert_eq!(post(offer.port, &path, &body(offer, "Phone", &[], "ph-1", PIN, None)), 200);
        wait_until(|| !paired.lock().unwrap().is_empty());
        assert!(paired.lock().unwrap().iter().all(|r| r.phone_id == "ph-1"));
    }

    #[test]
    fn pairing_without_a_phone_id_is_rejected() {
        let (server, paired) = server();
        let offer = &server.offer;
        assert_eq!(post(offer.port, &format!("/pair/{}", offer.nonce), &body(offer, "Phone", &[], "", PIN, None)), 400);
        assert!(paired.lock().unwrap().is_empty());
    }

    #[test]
    fn offer_names_this_computer() {
        let server = PairingServer::start_on(0, "pc-9", "Desk", Some(candidates()), Limits::default(), |_| {}).unwrap();
        for raw in [&server.offer.payload, &server.offer.usb_payload] {
            let data: Value = serde_json::from_str(raw).unwrap();
            assert_eq!((data["computer_id"].as_str(), data["computer_name"].as_str()), (Some("pc-9"), Some("Desk")));
            assert_eq!((data["nonce"].as_str(), data["token"].as_str()), (Some(server.offer.nonce.as_str()), Some(server.offer.token.as_str())));
        }
    }

    #[test]
    fn a_request_on_a_connection_from_before_the_stop_is_refused() {
        let (server, paired) = server();
        let offer = server.offer.clone();
        let body = pair_body(&offer);
        let mut conn = TcpStream::connect(("127.0.0.1", offer.port)).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        write!(conn, "POST /pair/{} HTTP/1.1\r\nContent-Length: {}\r\n\r\n", offer.nonce, body.len()).unwrap();
        wait_until(|| !server.open_connections().is_empty());
        server.stop();
        conn.write_all(&body).unwrap();
        let mut reply = String::new();
        let _ = conn.read_to_string(&mut reply);
        assert!(reply.starts_with("HTTP/1.1 404"), "{reply}");
        assert!(paired.lock().unwrap().is_empty());
    }

    #[test]
    fn stop_is_idempotent() {
        let (server, _) = server();
        let port = server.offer.port;
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
        server.stop();
        wait_until(|| TcpStream::connect(("127.0.0.1", port)).is_err());
        server.stop(); // must not panic
    }

    #[test]
    fn pairing_again_straight_away_keeps_the_fixed_port() {
        let (first, _) = server_with(Limits::default(), vec![]);
        let port = first.offer.port;
        assert_eq!(raw(port, b"GET / HTTP/1.1\r\n\r\n"), 200);
        first.stop();
        drop(first);
        // The server closed its side first, so the port may sit in TIME_WAIT; wait for the listener to go
        let deadline = Instant::now() + Duration::from_secs(2);
        let second = loop {
            let s = PairingServer::start_on(port, "", "", Some(vec![]), Limits::default(), |_| {}).unwrap();
            if s.offer.port == port || Instant::now() > deadline {
                break s;
            }
            std::thread::sleep(Duration::from_millis(30));
        };
        assert_eq!(second.offer.port, port);
    }

    #[test]
    fn the_saved_phone_keeps_the_source_address_only_when_the_phone_listed_it() {
        let result = PairingResult { name: "P".into(), ips: vec!["192.168.1.5".into()], token: "t".into(), source_ip: "192.168.1.5".into(),
                                     phone_id: "p".into(), cert_sha256: PIN.into() };
        assert_eq!(result.phone().active_ip.as_deref(), Some("192.168.1.5"));
        let usb = PairingResult { source_ip: "127.0.0.1".into(), ..result };
        assert_eq!(usb.phone().active_ip, None);
    }
}
