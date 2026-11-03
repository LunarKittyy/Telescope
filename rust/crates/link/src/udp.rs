//! Experimental H.264 over UDP for Wi-Fi (/v1/video.udp, docs/protocol.md), falling back to the TCP stream
//! (udp_reader.py). Each packet handed on is one whole frame, in order.

use crate::feeds::{H264Feed, StopHandle, MAX_FRAME_BYTES};
use crate::https::{Aborter, PhoneAuth, Request, Url};
use base64::Engine;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::net::{SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use telescope_core::media::{FeedEnd, FeedStopper, Packet, VideoFeed};

/// frame (u32), index (u16), count (u16), flags (u8).
pub const HEADER_LEN: usize = 9;
pub const FLAG_KEY: u8 = 1;
pub const TO_DESKTOP: u8 = 1;
pub const TO_PHONE: u8 = 2;
pub const NACK: u8 = 1;
pub const KEYFRAME: u8 = 2;
pub const HELLO: u8 = 3;
pub const REPORT: u8 = 4;
/// Replay window, in datagrams.
pub const WINDOW: i64 = 4096;
/// Keeps the desktop's firewall open for the phone's datagrams.
pub const HELLO_S: f64 = 0.5;
pub const REPORT_S: f64 = 0.1;
/// No datagram by then: they don't get here (usually a firewall), use TCP.
pub const FIRST_FRAME_S: f64 = 3.0;
/// Datagrams arrive but no frame comes whole by then (a link too thin for a keyframe): TCP too.
pub const FIRST_DECODE_S: f64 = 10.0;
/// A frame behind newer ones this long is missing pieces...
pub const NACK_AFTER_S: f64 = 0.015;
/// ...asked for again this often...
pub const NACK_AGAIN_S: f64 = 0.05;
/// ...and given up on after this: freeze until a keyframe.
pub const GIVE_UP_S: f64 = 0.25;
pub const KEY_ASK_S: f64 = 0.3;
/// In reassembly at once.
pub const MAX_FRAMES: usize = 300;
/// The most video bytes a datagram carries (the phone's fragment size).
pub const MAX_PAYLOAD: usize = 1100;
/// A frame declaring more pieces than a frame of the TCP feeds' largest size could need is bogus.
pub const MAX_PARTS: usize = MAX_FRAME_BYTES.div_ceil(MAX_PAYLOAD);
/// Bytes held across all frames in reassembly.
pub const MAX_HELD_BYTES: usize = 64 * 1024 * 1024;

/// Phones UDP didn't get through from, for the rest of this run.
static UDP_FAILED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub fn udp_failed(host: &str) -> bool {
    UDP_FAILED.lock().unwrap().as_ref().is_some_and(|s| s.contains(host))
}

fn mark_failed(host: &str) {
    UDP_FAILED.lock().unwrap().get_or_insert_with(HashSet::new).insert(host.to_string());
}

#[cfg(test)]
fn forget_failures() {
    *UDP_FAILED.lock().unwrap() = None;
}

/// The frame header as the phone packs it.
pub fn header(frame: u32, index: u16, count: u16, flags: u8) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..4].copy_from_slice(&frame.to_be_bytes());
    h[4..6].copy_from_slice(&index.to_be_bytes());
    h[6..8].copy_from_slice(&count.to_be_bytes());
    h[8] = flags;
    h
}

/// AES-GCM for one direction: each datagram is its counter (8 bytes) then the sealed plaintext.
pub struct Seal {
    key: LessSafeKey,
    direction: u8,
    counter: i64,
    highest: i64,
    seen: HashSet<i64>,
}

impl Seal {
    pub fn new(key: &[u8], direction: u8) -> Result<Seal, String> {
        let key = UnboundKey::new(&AES_256_GCM, key).map_err(|_| "the UDP key isn't 32 bytes".to_string())?;
        Ok(Seal { key: LessSafeKey::new(key), direction, counter: 0, highest: -1, seen: HashSet::new() })
    }

    /// The highest counter opened so far, -1 before any.
    pub fn highest(&self) -> i64 {
        self.highest
    }

    fn nonce(&self, n: i64) -> Nonce {
        let mut b = [0u8; 12];
        b[0] = self.direction;
        b[4..].copy_from_slice(&n.to_be_bytes());
        Nonce::assume_unique_for_key(b)
    }

    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        let n = self.counter;
        self.counter += 1;
        let mut sealed = plain.to_vec();
        self.key.seal_in_place_append_tag(self.nonce(n), Aad::empty(), &mut sealed).expect("AES-GCM seals any length here");
        [n.to_be_bytes().to_vec(), sealed].concat()
    }

    /// The plaintext, or None for anything forged, damaged, replayed or far too old.
    pub fn open(&mut self, datagram: &[u8]) -> Option<Vec<u8>> {
        if datagram.len() < 8 + 16 {
            return None;
        }
        let n = i64::from_be_bytes(datagram[..8].try_into().ok()?);
        if n < 0 || n <= self.highest - WINDOW || self.seen.contains(&n) {
            return None;
        }
        let mut sealed = datagram[8..].to_vec();
        let plain = self.key.open_in_place(self.nonce(n), Aad::empty(), &mut sealed).ok()?.to_vec();
        self.seen.insert(n);
        if n > self.highest {
            self.highest = n;
            if self.seen.len() as i64 > 2 * WINDOW {
                self.seen.retain(|s| *s > n - WINDOW);
            }
        }
        Some(plain)
    }
}

#[derive(Default)]
struct Frame {
    /// 0 until a piece of it arrived.
    count: u16,
    parts: BTreeMap<u16, Vec<u8>>,
    key: bool,
    /// When it was first seen holding up newer frames.
    noticed: Option<f64>,
    asked: Option<f64>,
    asks: u32,
    progress: usize,
}

impl Frame {
    fn complete(&self) -> bool {
        self.count > 0 && self.parts.len() == self.count as usize
    }

    fn bytes(&self) -> usize {
        self.parts.values().map(Vec::len).sum()
    }

    fn missing(&self) -> Vec<u16> {
        (0..self.count).filter(|i| !self.parts.contains_key(i)).collect()
    }

    fn notice(&mut self, now: f64) {
        // A large keyframe gets time to finish while new pieces are still arriving
        if self.noticed.is_none() || (self.key && self.progress != self.parts.len()) {
            self.noticed = Some(now);
            self.progress = self.parts.len();
        }
    }

    fn asked_long_ago(&self, now: f64) -> bool {
        self.asked.is_none_or(|t| now - t >= NACK_AGAIN_S)
    }
}

/// Puts datagrams back into whole frames, in order. Pure, so tests drive it with their own clock.
#[derive(Default)]
pub struct Reassembler {
    frames: BTreeMap<u32, Frame>,
    /// The next frame to hand on; None while waiting for a keyframe.
    next: Option<u32>,
    newest: i64,
    /// Bytes of pieces held across all frames.
    held: usize,
    /// Frames given up on.
    pub dropped: u32,
    /// Video taken, each piece once.
    pub bytes: u64,
    /// The newest frame that's whole...
    pub whole: i64,
    /// ...and when it became whole.
    pub whole_at: f64,
}

/// A NACK: the frame and its missing pieces ([] for all of it).
pub type Nack = (u32, Vec<u16>);

impl Reassembler {
    pub fn new() -> Reassembler {
        Reassembler { newest: -1, whole: -1, ..Default::default() }
    }

    pub fn frames_held(&self) -> usize {
        self.frames.len()
    }

    pub fn held_bytes(&self) -> usize {
        self.held
    }

    pub fn add(&mut self, plain: &[u8], now: f64) {
        if plain.len() < HEADER_LEN {
            return;
        }
        let seq = u32::from_be_bytes(plain[..4].try_into().unwrap());
        let idx = u16::from_be_bytes(plain[4..6].try_into().unwrap());
        let count = u16::from_be_bytes(plain[6..8].try_into().unwrap());
        let flags = plain[8];
        if count == 0 || idx >= count || self.next.is_some_and(|n| seq < n) {
            return;
        }
        if count as usize > MAX_PARTS {
            return;
        }
        let incoming = plain.len() - HEADER_LEN;
        let replaced = self.frames.get(&seq).and_then(|f| f.parts.get(&idx)).map_or(0, Vec::len);
        if self.held - replaced + incoming > MAX_HELD_BYTES {
            // Only a keyframe may push older frames out; the stream resyncs on it
            if flags & FLAG_KEY == 0 {
                return;
            }
            let mut evicted = false;
            while self.held - replaced + incoming > MAX_HELD_BYTES {
                let Some(oldest) = self.frames.keys().copied().find(|s| *s != seq) else { break };
                self.held -= self.frames.remove(&oldest).map_or(0, |f| f.bytes());
                evicted = true;
            }
            if evicted {
                self.next = None;
            }
            if self.held - replaced + incoming > MAX_HELD_BYTES {
                return;
            }
        }
        if !self.frames.contains_key(&seq) {
            if self.frames.len() >= MAX_FRAMES {
                if flags & FLAG_KEY == 0 {
                    return;
                }
                // A fresh keyframe is the way out of a backlog, so make room for it
                self.forget_before(seq);
                if self.frames.len() >= MAX_FRAMES {
                    return;
                }
                self.next = None;
            }
            self.frames.insert(seq, Frame::default());
        }
        let f = self.frames.get_mut(&seq).unwrap();
        f.count = count;
        f.key = flags & FLAG_KEY != 0;
        if !f.parts.contains_key(&idx) {
            self.bytes += (plain.len() - HEADER_LEN) as u64;
        }
        if let Some(old) = f.parts.insert(idx, plain[HEADER_LEN..].to_vec()) {
            self.held -= old.len();
        }
        self.held += incoming;
        self.newest = self.newest.max(seq as i64);
        if seq as i64 > self.whole && f.complete() {
            self.whole = seq as i64;
            self.whole_at = now;
        }
    }

    /// (frame bytes, how many frames it stands for) for the next whole frame in order.
    pub fn take(&mut self) -> Option<(Vec<u8>, u32)> {
        let skipped = match self.next {
            Some(_) => 0,
            None => {
                let start = self.frames.iter().filter(|(_, f)| f.key && f.complete()).map(|(s, _)| *s).max()?;
                let skipped = self.frames.keys().filter(|s| **s < start).count() as u32;
                self.forget_before(start);
                self.next = Some(start);
                skipped
            }
        };
        let next = self.next?;
        if !self.frames.get(&next).is_some_and(Frame::complete) {
            return None;
        }
        let f = self.frames.remove(&next).unwrap();
        self.held -= f.bytes();
        self.next = Some(next + 1);
        Some((f.parts.into_values().flatten().collect(), 1 + skipped))
    }

    /// (NACKs to send, whether to ask for a keyframe).
    pub fn chase(&mut self, now: f64) -> (Vec<Nack>, bool) {
        let Some(next) = self.next else {
            let Some(seq) = self.frames.iter().filter(|(_, f)| f.key).map(|(s, _)| *s).max() else {
                return (Vec::new(), self.newest >= 0);
            };
            // Repair the newest partial keyframe instead of replacing it with another large burst
            let f = self.frames.get_mut(&seq).unwrap();
            if f.complete() {
                return (Vec::new(), false);
            }
            f.notice(now);
            let noticed = f.noticed.unwrap();
            if now - noticed >= GIVE_UP_S {
                self.forget_before(seq + 1);
                return (Vec::new(), true);
            }
            if now - noticed >= NACK_AFTER_S && f.asked_long_ago(now) {
                f.asked = Some(now);
                return (vec![(seq, f.missing())], false);
            }
            return (Vec::new(), false);
        };
        let mut nacks = Vec::new();
        let mut seq = next as i64;
        while seq <= self.newest {
            let s = seq as u32;
            seq += 1;
            if !self.frames.contains_key(&s) {
                if self.frames.len() >= MAX_FRAMES {
                    break;
                }
                self.frames.insert(s, Frame::default()); // none of it came, but newer ones did
            }
            let newest = self.newest;
            let f = self.frames.get_mut(&s).unwrap();
            if f.complete() {
                continue;
            }
            f.notice(now);
            let waited = now - f.noticed.unwrap();
            if waited >= GIVE_UP_S {
                self.dropped += 1;
                self.next = None; // freeze until a keyframe rather than decode on a broken reference
                self.forget_before(s + 1);
                return (Vec::new(), true);
            }
            // The newest may still be arriving
            let patience = if (s as i64) < newest { NACK_AFTER_S } else { NACK_AFTER_S * 4.0 };
            if waited >= patience && f.asked_long_ago(now) {
                f.asked = Some(now);
                f.asks += 1;
                nacks.push((s, f.missing()));
            }
        }
        (nacks, false)
    }

    fn forget_before(&mut self, seq: u32) {
        let kept = self.frames.split_off(&seq);
        let gone = std::mem::replace(&mut self.frames, kept);
        self.held -= gone.values().map(Frame::bytes).sum::<usize>();
    }
}

/// What got through so far, for the phone's Dynamic: counts that only grow (wrapping at 32 bits).
pub fn report(received: u64, highest: i64, frames: &Reassembler, now: f64) -> Vec<u8> {
    let since: u16 = if frames.whole < 0 { 0xFFFF } else { ((now - frames.whole_at) * 1000.0).clamp(0.0, 65535.0) as u16 };
    let mut out = vec![REPORT];
    out.extend((received as u32).to_be_bytes());
    out.extend((highest.max(0) as u32).to_be_bytes());
    out.extend((frames.bytes as u32).to_be_bytes());
    out.extend((((now * 1000.0) as i64) as u32).to_be_bytes());
    out.extend((frames.whole as u32).to_be_bytes());
    out.extend(since.to_be_bytes());
    out.extend((frames.dropped as u16).to_be_bytes());
    out
}

/// A NACK's body: the frame then up to 400 missing pieces.
pub fn nack(seq: u32, missing: &[u16]) -> Vec<u8> {
    let mut body = vec![NACK];
    body.extend(seq.to_be_bytes());
    for i in missing.iter().take(400) {
        body.extend(i.to_be_bytes());
    }
    body
}

fn clock(origin: Instant) -> f64 {
    origin.elapsed().as_secs_f64()
}

/// The session over UDP while it lasts.
struct Udp {
    sock: UdpSocket,
    /// The session's HTTP response: closing it ends the phone's side too.
    _response: crate::https::Response,
    phone: SocketAddr,
    seal_in: Seal,
    seal_out: Seal,
    frames: Reassembler,
    opened_at: f64,
    received: u64,
    last_hello: f64,
    last_report: f64,
    last_key_ask: f64,
    /// The reader made a picture from what came over it (Python's _decoded_any).
    decoded_any: bool,
}

impl Udp {
    fn send(&mut self, plain: &[u8]) {
        let sealed = self.seal_out.seal(plain);
        let _ = self.sock.send_to(&sealed, self.phone);
    }

    fn hello(&mut self, now: f64, force: bool) {
        if force || now - self.last_hello >= HELLO_S {
            self.last_hello = now;
            self.send(&[HELLO]);
        }
    }

    fn report(&mut self, now: f64) {
        if now - self.last_report >= REPORT_S {
            self.last_report = now;
            let body = report(self.received, self.seal_in.highest(), &self.frames, now);
            self.send(&body);
        }
    }

    fn receive(&mut self, origin: Instant) {
        let mut buf = [0u8; 2048];
        for i in 0..256 {
            if i == 1 {
                let _ = self.sock.set_nonblocking(true);
            }
            let Ok((n, addr)) = self.sock.recv_from(&mut buf) else { break };
            if addr.ip() != self.phone.ip() {
                continue;
            }
            if let Some(plain) = self.seal_in.open(&buf[..n]) {
                // The phone's port as a NAT on the way sees it, like the phone does for ours
                self.phone = addr;
                self.received += 1;
                self.frames.add(&plain, clock(origin));
            }
        }
        let _ = self.sock.set_nonblocking(false);
    }

    fn chase(&mut self, now: f64) {
        let (nacks, want_key) = self.frames.chase(now);
        for (seq, missing) in nacks {
            self.send(&nack(seq, &missing));
        }
        if want_key && now - self.last_key_ask >= KEY_ASK_S {
            self.last_key_ask = now;
            self.send(&[KEYFRAME]);
        }
    }
}

/// /v1/video.udp, or /v1/video.h264 once UDP didn't work out.
pub struct UdpFeed {
    url: String,
    auth: PhoneAuth,
    timeout: Duration,
    origin: Instant,
    udp: Option<Udp>,
    tcp: Option<H264Feed>,
    pub fell_back: bool,
    stop: StopHandle,
    /// How long frames that arrive may take to make a picture before UDP gives up (FIRST_DECODE_S).
    first_decode_s: f64,
}

impl UdpFeed {
    pub fn open(url: &str, auth: &PhoneAuth, timeout: Duration, stop: StopHandle) -> Result<UdpFeed, String> {
        let mut feed = UdpFeed { url: url.to_string(), auth: auth.clone(), timeout, origin: Instant::now(), udp: None, tcp: None,
                                 fell_back: false, stop, first_decode_s: FIRST_DECODE_S };
        let host = Url::hostname(url).unwrap_or_default();
        if !udp_failed(&host) {
            match feed.open_udp() {
                Ok(()) => return Ok(feed),
                Err(e) => log::info!("UDP video didn't start ({e}); using TCP"),
            }
        }
        feed.fall_back()?;
        Ok(feed)
    }

    pub fn tcp_url(&self) -> String {
        match self.url.strip_suffix(".udp") {
            Some(base) => format!("{base}.h264"),
            None => self.url.replacen(".udp?", ".h264?", 1),
        }
    }

    fn open_udp(&mut self) -> Result<(), String> {
        let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
        set_receive_buffer(&sock, 4 << 20);
        sock.set_read_timeout(Some(Duration::from_millis(5))).map_err(|e| e.to_string())?;
        let port = sock.local_addr().map_err(|e| e.to_string())?.port();
        let url = format!("{}?port={port}", self.url);
        let watch = |a: Aborter| self.stop.watch(a);
        let mut resp = self.auth.open(Request::get(&url, self.timeout).watched(&watch)).map_err(|e| e.to_string())?;
        // Dropping resp on the way out closes it: the phone holds the session (and a stream slot) while it's open
        if !resp.content_type().contains("application/x-telescope-udp") {
            return Err("not a UDP session".into());
        }
        let line = resp.readline(4096).map_err(|e| e.to_string())?;
        let hello: Value = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
        let key = hello.get("key").and_then(Value::as_str)
            .and_then(|k| base64::engine::general_purpose::STANDARD.decode(k).ok()).ok_or("no key")?;
        let phone_port = hello.get("port").and_then(Value::as_u64).and_then(|p| u16::try_from(p).ok()).ok_or("no port")?;
        let host = Url::parse(&self.url).map_err(|e| e.to_string())?.host;
        let ip: std::net::IpAddr = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), phone_port)).ok()
            .and_then(|mut a| a.next()).map(|a| a.ip()).ok_or("no address")?;
        let now = clock(self.origin);
        let mut udp = Udp { sock, _response: resp, phone: SocketAddr::new(ip, phone_port), seal_in: Seal::new(&key, TO_DESKTOP)?,
                            seal_out: Seal::new(&key, TO_PHONE)?, frames: Reassembler::new(), opened_at: now, received: 0,
                            last_hello: 0.0, last_report: 0.0, last_key_ask: 0.0, decoded_any: false };
        udp.hello(now, true);
        self.udp = Some(udp);
        Ok(())
    }

    fn fall_back(&mut self) -> Result<(), String> {
        self.udp = None;
        if !self.fell_back {
            self.fell_back = true;
            log::info!("UDP video fell back to TCP");
        }
        self.tcp = Some(H264Feed::open(&self.tcp_url(), &self.auth, self.timeout, self.stop.clone())?);
        Ok(())
    }
}

#[cfg(unix)]
fn set_receive_buffer(sock: &UdpSocket, bytes: i32) {
    use std::os::fd::AsRawFd;
    // SAFETY: a valid socket and a pointer to an int that lives for the call
    unsafe {
        libc::setsockopt(sock.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVBUF, (&bytes as *const i32).cast(), size_of::<i32>() as _);
    }
}

#[cfg(not(unix))]
fn set_receive_buffer(_sock: &UdpSocket, _bytes: i32) {
    // Windows keeps its default buffer for now (setsockopt needs the WinSock bindings)
}

impl VideoFeed for UdpFeed {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        if let Some(tcp) = self.tcp.as_mut() {
            return tcp.read();
        }
        let origin = self.origin;
        let deadline = clock(origin) + self.timeout.as_secs_f64();
        loop {
            if self.stop.stopped() {
                return Err(FeedEnd::Stopped);
            }
            let Some(udp) = self.udp.as_mut() else { return Err(FeedEnd::Closed) };
            let now = clock(origin);
            let limit = if udp.received > 0 { self.first_decode_s } else { FIRST_FRAME_S };
            if !udp.decoded_any && now - udp.opened_at > limit {
                // Likely a firewall: don't wait for it again
                mark_failed(&udp.phone.ip().to_string());
                if let Some(host) = Url::hostname(&self.url) {
                    mark_failed(&host);
                }
                self.fall_back().map_err(FeedEnd::Error)?;
                return self.read();
            }
            if now > deadline && udp.decoded_any {
                return Err(FeedEnd::Closed); // the phone stopped sending: the session's over
            }
            udp.hello(now, false);
            udp.report(now);
            udp.receive(origin);
            if let Some((data, _frames)) = udp.frames.take() {
                return Ok(Packet { data, time_ns: telescope_core::time::monotonic_ns() });
            }
            udp.chase(now);
        }
    }

    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(self.stop.clone())
    }

    fn decoded(&mut self) {
        if let Some(udp) = self.udp.as_mut() {
            udp.decoded_any = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    const KEY: [u8; 32] = {
        let mut k = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            k[i] = i as u8;
            i += 1;
        }
        k
    };

    fn fragments(seq: u32, data: &[u8], key: bool, size: usize) -> Vec<Vec<u8>> {
        let count = data.len().div_ceil(size).max(1);
        (0..count).map(|i| {
            let end = ((i + 1) * size).min(data.len());
            [header(seq, i as u16, count as u16, key as u8).to_vec(), data[(i * size).min(data.len())..end].to_vec()].concat()
        }).collect()
    }

    #[test]
    fn seals_exactly_like_the_phone() {
        // The same vector is in the phone's UdpVideoTest.kt
        let mut seal = Seal::new(&KEY, TO_DESKTOP).unwrap();
        for _ in 0..5 {
            seal.seal(b"");
        }
        let hex: String = seal.seal(b"telescope").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "00000000000000055e76b1e3a4fb9044e263c4c95177c93d06d25e7b83374ba48c");
    }

    #[test]
    fn opens_once_and_nothing_forged_or_from_the_other_direction() {
        let (mut phone, mut desktop) = (Seal::new(&KEY, TO_DESKTOP).unwrap(), Seal::new(&KEY, TO_DESKTOP).unwrap());
        let (a, b) = (phone.seal(b"a"), phone.seal(b"b"));
        assert_eq!(desktop.open(&b).as_deref(), Some(&b"b"[..]));
        assert_eq!(desktop.open(&a).as_deref(), Some(&b"a"[..]));
        assert_eq!(desktop.open(&a), None);
        let mut damaged = b.clone();
        *damaged.last_mut().unwrap() ^= 1;
        assert_eq!(desktop.open(&damaged), None);
        assert_eq!(Seal::new(&KEY, TO_PHONE).unwrap().open(&phone.seal(b"c")), None);
        assert_eq!(Seal::new(&[0; 32], TO_DESKTOP).unwrap().open(&phone.seal(b"d")), None);
    }

    #[test]
    fn frames_come_out_whole_and_in_order_whatever_order_the_pieces_came_in() {
        let mut r = Reassembler::new();
        let mut pieces = fragments(0, &[b'K'; 25], true, 10);
        pieces.extend(fragments(1, &[b'P'; 15], false, 10));
        for p in pieces.iter().rev() {
            r.add(p, 0.0);
        }
        assert_eq!(r.take(), Some((vec![b'K'; 25], 1)));
        assert_eq!(r.take(), Some((vec![b'P'; 15], 1)));
        assert_eq!(r.take(), None);
    }

    #[test]
    fn a_lost_piece_is_asked_for_and_the_frame_waits_for_it() {
        let mut r = Reassembler::new();
        for p in fragments(0, &[b'K'; 10], true, 10) {
            r.add(&p, 0.0);
        }
        assert!(r.take().is_some());
        let one = fragments(1, &[b'P'; 30], false, 10);
        for p in [&one[0], &one[2]].into_iter().chain(&fragments(2, &[b'Q'; 10], false, 10)) {
            r.add(p, 0.0);
        }
        assert_eq!(r.take(), None);
        assert_eq!(r.chase(100.0), (vec![], false));
        assert_eq!(r.chase(100.0 + NACK_AFTER_S), (vec![(1, vec![1])], false));
        assert_eq!(r.chase(100.0 + NACK_AFTER_S + 0.001), (vec![], false));
        r.add(&one[1], 0.0);
        assert_eq!(r.take(), Some((vec![b'P'; 30], 1)));
        assert_eq!(r.take(), Some((vec![b'Q'; 10], 1)));
    }

    #[test]
    fn a_whole_frame_lost_is_asked_for_in_full() {
        let mut r = Reassembler::new();
        for p in fragments(0, b"K", true, 10).into_iter().chain(fragments(2, b"Q", false, 10)) {
            r.add(&p, 0.0);
        }
        r.take();
        r.chase(0.0);
        assert_eq!(r.chase(NACK_AFTER_S), (vec![(1, vec![])], false));
    }

    #[test]
    fn a_frame_that_never_comes_freezes_until_a_keyframe() {
        let mut r = Reassembler::new();
        for p in fragments(0, b"K", true, 10).into_iter().chain(fragments(2, b"Q", false, 10)) {
            r.add(&p, 0.0);
        }
        r.take();
        r.chase(0.0);
        assert_eq!(r.chase(GIVE_UP_S), (vec![], true));
        assert_eq!(r.dropped, 1);
        for p in fragments(3, b"R", false, 10) {
            r.add(&p, 0.0);
        }
        assert_eq!(r.take(), None);
        for p in fragments(4, b"K2", true, 10) {
            r.add(&p, 0.0);
        }
        assert_eq!(r.take(), Some((b"K2".to_vec(), 3)));
        assert_eq!(r.chase(1000.0), (vec![], false));
    }

    #[test]
    fn a_partial_first_keyframe_is_repaired_without_asking_for_another() {
        let mut r = Reassembler::new();
        let data = b"keyframe".repeat(100);
        let pieces = fragments(0, &data, true, 200);
        for p in &pieces[..pieces.len() - 1] {
            r.add(p, 0.0);
        }
        assert_eq!(r.chase(1.0), (vec![], false));
        assert_eq!(r.chase(1.1), (vec![(0, vec![pieces.len() as u16 - 1])], false));
        r.add(pieces.last().unwrap(), 0.0);
        assert_eq!(r.take(), Some((data, 1)));
    }

    #[test]
    fn a_frame_declaring_too_many_pieces_is_dropped() {
        let mut r = Reassembler::new();
        let mut p = header(0, 0, (MAX_PARTS + 1) as u16, FLAG_KEY).to_vec();
        p.extend_from_slice(b"x");
        r.add(&p, 0.0);
        assert_eq!(r.frames_held(), 0);
        assert_eq!(r.held_bytes(), 0);
        let mut p = header(1, 0, MAX_PARTS as u16, 0).to_vec();
        p.extend_from_slice(b"x");
        r.add(&p, 0.0);
        assert_eq!(r.frames_held(), 1);
    }

    #[test]
    fn a_flood_of_large_incomplete_frames_stays_under_the_byte_cap() {
        let mut r = Reassembler::new();
        let big = vec![0u8; 1024 * 1024];
        for seq in 0..200u32 {
            let mut p = header(seq, 0, 2, (seq % 50 == 0) as u8).to_vec();
            p.extend_from_slice(&big);
            r.add(&p, 0.0);
            assert!(r.held_bytes() <= MAX_HELD_BYTES, "held {} after frame {seq}", r.held_bytes());
            assert_eq!(r.held_bytes(), r.frames.values().map(Frame::bytes).sum::<usize>());
        }
        let key = fragments(1000, b"fresh keyframe", true, 10);
        for p in &key {
            r.add(p, 0.0);
        }
        assert!(r.take().is_some_and(|(data, _)| data == b"fresh keyframe"));
        assert_eq!(r.held_bytes(), r.frames.values().map(Frame::bytes).sum::<usize>());
        let next = fragments(1001, b"delta", false, 10);
        r.add(&next[0], 0.0);
        assert_eq!(r.take(), Some((b"delta".to_vec(), 1)));
    }

    #[test]
    fn the_byte_total_follows_frames_that_complete_expire_or_repeat() {
        let mut r = Reassembler::new();
        let pieces = fragments(0, &[7u8; 50], true, 10);
        r.add(&pieces[0], 0.0);
        r.add(&pieces[0], 0.0);
        assert_eq!(r.held_bytes(), 10);
        for p in &pieces[1..] {
            r.add(p, 0.0);
        }
        assert_eq!(r.held_bytes(), 50);
        assert!(r.take().is_some());
        assert_eq!(r.held_bytes(), 0);
        r.add(&fragments(1, &[1u8; 30], false, 10)[0], 0.0);
        r.add(&fragments(2, &[1u8; 30], false, 10)[0], 0.0);
        assert_eq!(r.held_bytes(), 20);
        r.chase(0.0);
        r.chase(GIVE_UP_S + 0.1);
        assert_eq!(r.held_bytes(), r.frames.values().map(Frame::bytes).sum::<usize>());
    }

    #[test]
    fn a_keyframe_can_replace_a_full_backlog() {
        let mut r = Reassembler::new();
        for seq in 0..MAX_FRAMES as u32 {
            r.add(&fragments(seq, b"p", false, 10)[0], 0.0);
        }
        r.add(&fragments(MAX_FRAMES as u32, b"key", true, 10)[0], 0.0);
        assert!(r.take().is_some());
        assert_eq!(r.frames_held(), 0);
    }

    #[test]
    fn a_large_keyframe_is_not_discarded_while_it_is_still_arriving() {
        for established in [false, true] {
            let mut r = Reassembler::new();
            if established {
                r.add(&fragments(0, b"first", true, 10)[0], 0.0);
                assert_eq!(r.take(), Some((b"first".to_vec(), 1)));
            }
            let pieces = fragments(established as u32, &[b'K'; 80], true, 10);
            for (i, p) in pieces.iter().enumerate() {
                r.add(p, 0.0);
                assert!(!r.chase(i as f64 * 0.1).1);
            }
            assert_eq!(r.take(), Some((vec![b'K'; 80], 1)));
        }
    }

    #[test]
    fn a_keyframe_that_stops_progressing_still_times_out() {
        for established in [false, true] {
            let mut r = Reassembler::new();
            if established {
                r.add(&fragments(0, b"first", true, 10)[0], 0.0);
                r.take();
            }
            let piece = fragments(established as u32, &[b'K'; 80], true, 10)[0].clone();
            r.add(&piece, 0.0);
            r.chase(0.0);
            r.add(&piece, 0.0); // a duplicate isn't progress
            assert_eq!(r.chase(GIVE_UP_S + 0.01), (vec![], true));
        }
    }

    fn unpack(body: &[u8]) -> (u8, u32, u32, u32, u32, u32, u16, u16) {
        let u32_at = |i: usize| u32::from_be_bytes(body[i..i + 4].try_into().unwrap());
        let u16_at = |i: usize| u16::from_be_bytes(body[i..i + 2].try_into().unwrap());
        (body[0], u32_at(1), u32_at(5), u32_at(9), u32_at(13), u32_at(17), u16_at(21), u16_at(23))
    }

    #[test]
    fn a_report_packs_what_the_reassembler_saw() {
        let mut r = Reassembler::new();
        assert_eq!(unpack(&report(0, -1, &r, 1.0)), (4, 0, 0, 0, 1000, 0xFFFF_FFFF, 0xFFFF, 0));
        for p in fragments(0, &[b'x'; 30], true, 10) {
            r.add(&p, 2.0);
        }
        r.add(&fragments(0, &[b'x'; 30], true, 10)[1], 2.1);
        r.add(&fragments(1, &[b'y'; 30], false, 10)[0], 2.2);
        assert_eq!(unpack(&report(5, 6, &r, 2.25)), (4, 5, 6, 40, 2250, 0, 250, 0));
        let big = unpack(&report((1u64 << 32) + 3, (1i64 << 32) + 4, &r, 2f64.powi(32) / 1000.0 + 100.0));
        assert_eq!((big.1, big.2), (3, 4));
    }

    /// A phone serving /v1/video.udp and /v1/video.h264 the way the Python test's _Phone does.
    struct FakePhone {
        port: u16,
        nacks: Arc<AtomicU32>,
        reports: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    #[derive(Clone, Copy, Default)]
    struct Behaviour {
        loss: f64,
        refused: bool,
        silent: bool,
        slow_start: f64,
    }

    fn fake_phone(frames: Vec<(Vec<u8>, bool)>, how: Behaviour) -> FakePhone {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let nacks = Arc::new(AtomicU32::new(0));
        let reports: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let (n, rep) = (nacks.clone(), reports.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let (frames, n, rep) = (frames.clone(), n.clone(), rep.clone());
                std::thread::spawn(move || serve_phone(conn, frames, how, n, rep));
            }
        });
        FakePhone { port, nacks, reports }
    }

    fn serve_phone(mut conn: std::net::TcpStream, frames: Vec<(Vec<u8>, bool)>, how: Behaviour, nacks: Arc<AtomicU32>,
                   reports: Arc<Mutex<Vec<Vec<u8>>>>) {
        use std::io::{BufRead, BufReader};
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                break;
            }
        }
        if path.starts_with("/v1/video.h264") {
            let body: Vec<u8> = frames.iter().flat_map(|(f, _)| f.clone()).collect();
            let _ = write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: video/h264\r\nContent-Length: {}\r\n\r\n", body.len());
            let _ = conn.write_all(&body);
            return;
        }
        if how.refused {
            let _ = conn.write_all(b"HTTP/1.1 409 Conflict\r\nContent-Length: 0\r\n\r\n");
            return;
        }
        let port: u16 = path.split("port=").nth(1).unwrap().parse().unwrap();
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let (mut out, mut back) = (Seal::new(&KEY, TO_DESKTOP).unwrap(), Seal::new(&KEY, TO_PHONE).unwrap());
        let hello = format!("{{\"key\": \"{}\", \"port\": {}}}\n", base64::engine::general_purpose::STANDARD.encode(KEY), sock.local_addr().unwrap().port());
        let _ = write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: application/x-telescope-udp\r\n\r\n{hello}");
        if how.silent {
            std::thread::sleep(Duration::from_secs(2));
            return;
        }
        let peer: SocketAddr = ([127, 0, 0, 1], port).into();
        let mut rnd: u64 = 5;
        let mut lose = || {
            rnd = rnd.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((rnd >> 33) as f64 / (1u64 << 31) as f64) < how.loss
        };
        sock.set_read_timeout(Some(Duration::from_millis(2))).unwrap();
        let end = Instant::now() + Duration::from_secs_f64(how.slow_start);
        while Instant::now() < end {
            // A keyframe too big for the link: pieces arrive, it never completes
            let _ = sock.send_to(&out.seal(&[header(0, 0, 2, 1).to_vec(), b"x".to_vec()].concat()), peer);
            std::thread::sleep(Duration::from_millis(50));
        }
        let first = if how.slow_start > 0.0 { 1 } else { 0 };
        let mut sent: BTreeMap<u32, Vec<Vec<u8>>> = BTreeMap::new();
        for (i, (frame, key)) in frames.iter().enumerate() {
            let seq = first + i as u32;
            let pieces = fragments(seq, frame, *key, 200);
            for p in &pieces {
                let sealed = out.seal(p); // lost on the way like on a real link: its counter is gone too
                if !lose() {
                    let _ = sock.send_to(&sealed, peer);
                }
            }
            sent.insert(seq, pieces);
            let end = Instant::now() + Duration::from_secs_f64(1.0 / 30.0);
            let mut buf = [0u8; 2048];
            while Instant::now() < end {
                let Ok((n, _)) = sock.recv_from(&mut buf) else { continue };
                let Some(plain) = back.open(&buf[..n]) else { continue };
                match plain.first() {
                    Some(&REPORT) => reports.lock().unwrap().push(plain.clone()),
                    Some(&NACK) => {
                        nacks.fetch_add(1, Ordering::SeqCst);
                        let nseq = u32::from_be_bytes(plain[1..5].try_into().unwrap());
                        let idx: Vec<usize> = plain[5..].chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]]) as usize).collect();
                        let pieces = &sent[&nseq];
                        let all: Vec<usize> = (0..pieces.len()).collect();
                        for i in if idx.is_empty() { all } else { idx } {
                            let _ = sock.send_to(&out.seal(&pieces[i]), peer);
                        }
                    }
                    _ => {}
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    /// Frames that span several datagrams, a keyframe every 5, each ending with an access unit delimiter.
    fn frames(n: usize) -> Vec<(Vec<u8>, bool)> {
        (0..n).map(|i| {
            let mut f = vec![0, 0, 0, 1, 0x65, i as u8];
            f.extend((0..900).map(|j| ((i * 31 + j * 7) % 251) as u8));
            f.extend([0, 0, 0, 1, 9, 0xf0]);
            (f, i % 5 == 0)
        }).collect()
    }

    /// Reads as the media reader does: every packet decodes (decodes: false, none does).
    fn read_decoding(feed: &mut UdpFeed, limit: usize, decodes: bool) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while out.len() < limit {
            match feed.read() {
                Ok(p) => out.push(p.data),
                Err(_) => break,
            }
            if decodes {
                feed.decoded();
            }
        }
        out
    }

    fn read_all(feed: &mut UdpFeed, limit: usize) -> Vec<Vec<u8>> {
        read_decoding(feed, limit, true)
    }

    fn url(phone: &FakePhone) -> String {
        format!("http://127.0.0.1:{}/v1/video.udp", phone.port)
    }

    // The tests that open a feed share UDP_FAILED, so they run one at a time
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn frames_arrive_over_udp_and_lost_datagrams_are_sent_again() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget_failures();
        let sent = frames(40);
        let phone = fake_phone(sent.clone(), Behaviour { loss: 0.1, ..Default::default() });
        let mut feed = UdpFeed::open(&url(&phone), &PhoneAuth::new("secret", ""), Duration::from_secs(1), StopHandle::default()).unwrap();
        assert!(!feed.fell_back);
        let got = read_all(&mut feed, 200);
        assert!(got.len() >= sent.len() - 5, "{} of {}", got.len(), sent.len());
        assert!(phone.nacks.load(Ordering::SeqCst) > 0);
        // In order, and every one exactly as sent
        let idx: Vec<usize> = got.iter().map(|g| sent.iter().position(|(f, _)| f == g).expect("a frame as sent")).collect();
        assert!(idx.windows(2).all(|w| w[0] < w[1]));
        let reports = phone.reports.lock().unwrap();
        assert!(reports.len() >= 5);
        let (_kind, received, highest, taken, _clock, whole, since, gave_up) = unpack(reports.last().unwrap());
        let lost = 1.0 - received as f64 / (highest as f64 + 1.0);
        assert!((0.03..0.2).contains(&lost), "{lost}");
        assert!(taken > 0 && whole > 30 && since < 1000 && gave_up == 0);
        let counts: Vec<u32> = reports.iter().map(|r| unpack(r).1).collect();
        assert!(counts.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn a_phone_that_refuses_udp_streams_over_tcp() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget_failures();
        let phone = fake_phone(frames(3), Behaviour { refused: true, ..Default::default() });
        let mut feed = UdpFeed::open(&url(&phone), &PhoneAuth::new("secret", ""), Duration::from_secs(3), StopHandle::default()).unwrap();
        assert!(feed.fell_back);
        assert!(!read_all(&mut feed, 10).is_empty());
    }

    #[test]
    fn datagrams_that_never_arrive_fall_back_to_tcp_and_stay_there() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget_failures();
        let phone = fake_phone(frames(3), Behaviour { silent: true, ..Default::default() });
        let mut feed = UdpFeed::open(&url(&phone), &PhoneAuth::new("secret", ""), Duration::from_secs(3), StopHandle::default()).unwrap();
        assert!(!feed.fell_back);
        assert!(!read_all(&mut feed, 10).is_empty() && feed.fell_back);
        assert!(udp_failed("127.0.0.1"));
        let again = UdpFeed::open(&url(&phone), &PhoneAuth::new("secret", ""), Duration::from_secs(3), StopHandle::default()).unwrap();
        assert!(again.fell_back);
        forget_failures();
    }

    #[test]
    fn datagrams_that_arrive_but_take_a_while_to_make_a_frame_stay_on_udp() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget_failures();
        // Longer than FIRST_FRAME_S, short of FIRST_DECODE_S
        let phone = fake_phone(frames(3), Behaviour { slow_start: FIRST_FRAME_S + 0.5, ..Default::default() });
        let mut feed = UdpFeed::open(&url(&phone), &PhoneAuth::new("secret", ""), Duration::from_secs(1), StopHandle::default()).unwrap();
        assert!(!read_all(&mut feed, 10).is_empty() && !feed.fell_back);
    }

    #[test]
    fn frames_that_arrive_but_never_decode_fall_back_to_tcp() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget_failures();
        let sent = frames(3);
        let phone = fake_phone(sent.clone(), Behaviour::default());
        let mut feed = UdpFeed::open(&url(&phone), &PhoneAuth::new("secret", ""), Duration::from_secs(1), StopHandle::default()).unwrap();
        feed.first_decode_s = 1.5;
        // Whole frames come off UDP, but the first key unit is broken, so nothing decodes
        let got = read_decoding(&mut feed, sent.len() * 2, false);
        assert!(feed.fell_back, "{} frames and still on UDP", got.len());
        assert!(got.len() > sent.len(), "TCP's frames follow UDP's");
        forget_failures();
    }

    /// A feed the test keeps hold of while the media reader reads it.
    struct Shared(Arc<Mutex<UdpFeed>>);

    impl VideoFeed for Shared {
        fn read(&mut self) -> Result<Packet, FeedEnd> {
            self.0.lock().unwrap().read()
        }
        fn stopper(&self) -> Box<dyn FeedStopper> {
            self.0.lock().unwrap().stopper()
        }
        fn decoded(&mut self) {
            self.0.lock().unwrap().decoded();
        }
    }

    /// Real H.264 as the fake phone's frames: Python's greys, in datagrams of 200 bytes, a keyframe every 5.
    fn video(values: &[u8]) -> Option<Vec<(Vec<u8>, bool)>> {
        let packets = telescope_media::test_h264(values, 64, 48, 5, true)?;
        Some(packets.into_iter().enumerate().map(|(i, p)| (p, i % 5 == 0)).collect())
    }

    /// The mean brightness of each frame the real reader and decoder make of a feed, till it ends.
    fn read_video(feed: UdpFeed) -> (Vec<f64>, Arc<Mutex<UdpFeed>>) {
        use telescope_core::media::{Codec, DecodeOptions, Media};
        let media = telescope_media::FfmpegMedia;
        let feed = Arc::new(Mutex::new(feed));
        let decoder = media.decoder(Codec::H264, &DecodeOptions::default()).unwrap();
        let mut reader = media.frame_reader(Box::new(Shared(feed.clone())), decoder).unwrap();
        let mut means = Vec::new();
        while let Ok(read) = reader.read() {
            let (w, h) = (read.frame.width as usize, read.frame.height as usize);
            let y = &read.frame.data[..w * h];
            means.push(y.iter().map(|&v| v as f64).sum::<f64>() / y.len() as f64);
        }
        (means, feed)
    }

    #[test]
    fn real_video_arrives_over_udp_and_lost_datagrams_are_sent_again() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget_failures();
        let values: Vec<u8> = (40..200).step_by(4).map(|v| v as u8).collect();
        let Some(sent) = video(&values) else { return };
        let phone = fake_phone(sent, Behaviour { loss: 0.1, ..Default::default() });
        let feed = UdpFeed::open(&url(&phone), &PhoneAuth::new("secret", ""), Duration::from_secs(1), StopHandle::default()).unwrap();
        assert!(!feed.fell_back);
        let (means, _) = read_video(feed);
        // Nearly every frame, lost pieces and all
        assert!(means.len() >= values.len() - 5, "{} of {}", means.len(), values.len());
        assert!(phone.nacks.load(Ordering::SeqCst) > 0);
        // In order, nothing decoded from a broken frame
        assert!(means.windows(2).all(|w| w[1] >= w[0] - 3.0), "{means:?}");
    }

    #[test]
    fn a_phone_that_refuses_udp_gives_real_video_over_tcp() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget_failures();
        let Some(sent) = video(&[60, 120, 180]) else { return };
        let phone = fake_phone(sent, Behaviour { refused: true, ..Default::default() });
        let feed = UdpFeed::open(&url(&phone), &PhoneAuth::new("secret", ""), Duration::from_secs(3), StopHandle::default()).unwrap();
        assert!(feed.fell_back);
        assert!(!read_video(feed).0.is_empty());
    }

    #[test]
    fn datagrams_that_never_arrive_give_real_video_over_tcp_and_stay_there() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget_failures();
        let Some(sent) = video(&[60, 120, 180]) else { return };
        let phone = fake_phone(sent, Behaviour { silent: true, ..Default::default() });
        let feed = UdpFeed::open(&url(&phone), &PhoneAuth::new("secret", ""), Duration::from_secs(3), StopHandle::default()).unwrap();
        assert!(!feed.fell_back);
        let (means, feed) = read_video(feed);
        assert!(!means.is_empty() && feed.lock().unwrap().fell_back);
        assert!(udp_failed("127.0.0.1"));
        // Straight to TCP: no waiting on the firewall a second time
        let again = UdpFeed::open(&url(&phone), &PhoneAuth::new("secret", ""), Duration::from_secs(3), StopHandle::default()).unwrap();
        assert!(again.fell_back);
        forget_failures();
    }

    #[test]
    fn real_video_that_takes_a_while_to_make_a_frame_stays_on_udp() {
        let _one = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget_failures();
        let Some(sent) = video(&[60, 120, 180]) else { return };
        // Longer than FIRST_FRAME_S and the read timeout, short of FIRST_DECODE_S
        let phone = fake_phone(sent, Behaviour { slow_start: FIRST_FRAME_S + 0.5, ..Default::default() });
        let feed = UdpFeed::open(&url(&phone), &PhoneAuth::new("secret", ""), Duration::from_secs(1), StopHandle::default()).unwrap();
        let (means, feed) = read_video(feed);
        assert!(!means.is_empty() && !feed.lock().unwrap().fell_back);
    }

    #[test]
    fn a_session_whose_hello_doesnt_parse_is_closed_before_falling_back() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let closed = Arc::new(Mutex::new(false));
        let seen = closed.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-telescope-udp\r\n\r\n{\"key\": \"bm90IGEga2V5\"\n");
            conn.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            *seen.lock().unwrap() = matches!(conn.read(&mut buf), Ok(0) | Err(_));
        });
        let mut feed = UdpFeed { url: format!("http://127.0.0.1:{port}/v1/video.udp"), auth: PhoneAuth::new("t", ""),
                                 timeout: Duration::from_secs(3), origin: Instant::now(), udp: None, tcp: None, fell_back: false,
                                 stop: StopHandle::default(), first_decode_s: FIRST_DECODE_S };
        assert!(feed.open_udp().is_err());
        std::thread::sleep(Duration::from_millis(200));
        assert!(*closed.lock().unwrap(), "the phone holds the session and a stream slot until it times out");
    }

    #[test]
    fn the_tcp_url_is_the_same_path_as_h264() {
        let feed = UdpFeed { url: "https://h:8080/v1/video.udp".into(), auth: PhoneAuth::default(), timeout: Duration::from_secs(1),
                             origin: Instant::now(), udp: None, tcp: None, fell_back: false, stop: StopHandle::default(),
                             first_decode_s: FIRST_DECODE_S };
        assert_eq!(feed.tcp_url(), "https://h:8080/v1/video.h264");
    }
}
