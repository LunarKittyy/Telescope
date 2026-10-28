//! The QR pairing server (pairing.py): one nonce and token per offer, waits for the phone's POST to /pair/<nonce>
//! proving it read the token, and hands back the phone it paired.

use crate::config::Phone;
use crate::net::{self, PairingAddress};
use crate::util;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

pub const PAIRING_PORT: u16 = 8765;
pub const PAIRING_PROTOCOL_VERSION: u32 = 4;
const MAX_BODY: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct PairingOffer {
    pub payload: String,
    pub usb_payload: String,
    pub port: u16,
    pub candidates: Vec<PairingAddress>,
}

pub struct PairingServer {
    pub offer: PairingOffer,
    pub paired: mpsc::Receiver<Phone>,
}

impl PairingServer {
    pub fn start(computer_id: &str, computer_name: &str) -> std::io::Result<PairingServer> {
        let candidates = net::pairing_addresses();
        let nonce = util::token_urlsafe(16);
        let token = util::token_urlsafe(32);
        let listener = TcpListener::bind(("0.0.0.0", PAIRING_PORT)).or_else(|_| TcpListener::bind(("0.0.0.0", 0)))?;
        let port = listener.local_addr()?.port();
        let payload_for = |addrs: &[PairingAddress]| {
            json!({
                "version": PAIRING_PROTOCOL_VERSION, "port": port, "candidates": addrs,
                "nonce": nonce, "token": token, "computer_id": computer_id, "computer_name": computer_name,
            })
            .to_string()
        };
        let usb = [PairingAddress { ip: "127.0.0.1".into(), interface: "USB (adb)".into(), kind: "other" }];
        let offer = PairingOffer { payload: payload_for(&candidates), usb_payload: payload_for(&usb), port, candidates };

        let (tx, rx) = mpsc::channel();
        let claimed: Arc<Mutex<Option<String>>> = Arc::default();
        let open = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                if open.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    continue;
                }
                open.fetch_add(1, Ordering::SeqCst);
                let (tx, claimed, open) = (tx.clone(), claimed.clone(), open.clone());
                let (nonce, token) = (nonce.clone(), token.clone());
                std::thread::spawn(move || {
                    let _ = handle(conn, &nonce, &token, &claimed, &tx);
                    open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Ok(PairingServer { offer, paired: rx })
    }
}

fn respond(mut conn: &TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    write!(conn, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn handle(conn: TcpStream, nonce: &str, token: &str, claimed: &Mutex<Option<String>>, tx: &mpsc::Sender<Phone>) -> std::io::Result<()> {
    conn.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    let source_ip = conn.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut length: Option<usize> = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = header.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().ok();
            }
        }
    }
    if method == "GET" {
        return respond(&conn, "200 OK", "Telescope pairing server");
    }
    if path != format!("/pair/{nonce}") {
        return respond(&conn, "404 Not Found", "");
    }
    let Some(length) = length else { return respond(&conn, "411 Length Required", "") };
    if length > MAX_BODY {
        return respond(&conn, "413 Payload Too Large", "");
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    match check(&body, nonce, token, &source_ip) {
        Some(phone) => {
            let mut held = claimed.lock().unwrap();
            if held.as_deref().is_some_and(|id| id != phone.id) {
                return respond(&conn, "409 Conflict", "");
            }
            *held = Some(phone.id.clone());
            let _ = tx.send(phone);
            respond(&conn, "200 OK", "OK")
        }
        None => respond(&conn, "400 Bad Request", ""),
    }
}

fn check(body: &[u8], nonce: &str, token: &str, source_ip: &str) -> Option<Phone> {
    let data: Value = serde_json::from_slice(body).ok()?;
    let name = util::clean_name(data.get("name").and_then(Value::as_str).unwrap_or("Phone"));
    let phone_id = data.get("phone_id")?.as_str()?.trim().to_string();
    let cert = data.get("cert_sha256")?.as_str()?.to_string();
    let proof = data.get("proof")?.as_str()?;
    let mut ips: Vec<String> = Vec::new();
    for ip in data.get("ips").and_then(Value::as_array).into_iter().flatten() {
        let ip = ip.as_str()?.trim().to_string();
        ip.parse::<std::net::Ipv4Addr>().ok()?;
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    if name.is_empty() || phone_id.is_empty() || !util::is_fingerprint(&cert) {
        return None;
    }
    if !util::same(proof, &util::pairing_proof(token, nonce, &phone_id, &cert)) {
        return None;
    }
    // The address the POST came from reaches the phone by construction; over USB it's adb reverse's loopback
    let active = (source_ip != "127.0.0.1").then(|| source_ip.to_string());
    Some(Phone { id: phone_id, name, token: token.to_string(), ips, active_ip: active, cert_sha256: cert })
}
