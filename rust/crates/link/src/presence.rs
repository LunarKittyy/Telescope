//! Authenticated reachability hints, independent of camera and stream ownership.

use crate::net::presence_addresses;
use ring::hmac;
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use telescope_core::source::PairedPhone;

pub const PORT: u16 = 8769;
const MAX_PACKET: usize = 512;
const MAX_PEERS: usize = 64;
const MAX_PENDING: usize = 256;
const CHALLENGE_LIFE: f64 = 10.0;
const REPLAY_LIFE: f64 = 300.0;

#[derive(Clone, Debug, PartialEq)]
pub enum PresenceEvent {
    Probe { phone_id: String },
    Reachable { phone_id: String, address: String, addresses: Vec<String>, version: String },
    Degraded(String),
    Restored,
}

#[derive(Clone)]
struct Config {
    id: String,
    version: String,
    phones: Vec<PairedPhone>,
}

#[derive(PartialEq)]
struct Credentials {
    id: String,
    tokens: HashMap<String, String>,
}

#[derive(Default)]
struct Controls {
    config: Mutex<Option<(u64, Config)>>,
    credentials: Mutex<Option<Credentials>>,
    revision: AtomicU64,
    capabilities: Mutex<HashMap<String, bool>>,
    events: Mutex<VecDeque<PresenceEvent>>,
    hint: AtomicBool,
    stopped: AtomicBool,
}

pub struct Presence {
    controls: Arc<Controls>,
}

impl Presence {
    pub fn start() -> Self {
        let controls = Arc::new(Controls::default());
        let worker = controls.clone();
        std::thread::spawn(move || run(worker));
        Self { controls }
    }

    pub fn configure(&self, computer_id: String, version: String, phones: Vec<PairedPhone>) {
        let credentials = Credentials { id: computer_id.clone(), tokens: phones.iter().map(|phone| (phone.id.clone(), phone.token.clone())).collect() };
        let mut previous = self.controls.credentials.lock().unwrap();
        if previous.as_ref() != Some(&credentials) {
            self.controls.revision.fetch_add(1, Ordering::AcqRel);
            self.controls.events.lock().unwrap().clear();
            *previous = Some(credentials);
        }
        let revision = self.controls.revision.load(Ordering::Acquire);
        *self.controls.config.lock().unwrap() = Some((revision, Config { id: computer_id, version, phones }));
    }

    pub fn capability(&self, phone_id: &str, enabled: bool) {
        let mut capabilities = self.controls.capabilities.lock().unwrap();
        if capabilities.len() < MAX_PEERS || capabilities.contains_key(phone_id) {
            capabilities.insert(phone_id.to_string(), enabled);
        }
    }

    pub fn hint(&self) {
        self.controls.hint.store(true, Ordering::Relaxed);
    }

    pub fn take_events(&self) -> Vec<PresenceEvent> {
        self.controls.events.lock().unwrap().drain(..).collect()
    }

    pub fn stop(&self) {
        self.controls.stopped.store(true, Ordering::Relaxed);
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        self.stop();
    }
}

fn identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn version(value: &str) -> bool {
    value.len() <= 64 && value.bytes().all(|b| (32..=126).contains(&b) && b != b',')
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn address(value: &str) -> bool {
    crate::net::classify_ip(value).is_some()
}

struct AdapterAddresses {
    bound: Vec<String>,
    advertised: Vec<String>,
}

impl AdapterAddresses {
    fn new(values: impl IntoIterator<Item = String>) -> Self {
        let mut bound = Vec::new();
        for value in values {
            if address(&value) && !bound.contains(&value) {
                bound.push(value);
                if bound.len() == crate::net::MAX_PAIRING_CANDIDATES { break; }
            }
        }
        let advertised = presence_addresses(bound.iter().cloned());
        Self { bound, advertised }
    }
}

fn key(token: &str) -> hmac::Key {
    let seed = hmac::Key::new(hmac::HMAC_SHA256, token.as_bytes());
    hmac::Key::new(hmac::HMAC_SHA256, hmac::sign(&seed, b"telescope-presence-key-v1").as_ref())
}

fn hex_bytes(value: &[u8]) -> String {
    value.iter().map(|b| format!("{b:02x}")).collect()
}

struct Packet<'a> {
    kind: &'a str,
    sender: &'a str,
    recipient: &'a str,
    challenge: &'a str,
    version: &'a str,
    addresses: Vec<String>,
    body: &'a str,
    mac: Vec<u8>,
}

impl<'a> Packet<'a> {
    fn parse(bytes: &'a [u8]) -> Option<Self> {
        if bytes.len() > MAX_PACKET { return None; }
        let text = std::str::from_utf8(bytes).ok()?;
        let parts: Vec<_> = text.split('\n').collect();
        if parts.len() != 8 || parts[0] != "TP1" || !matches!(parts[1], "q" | "a")
            || !identity(parts[2]) || !identity(parts[3]) || !hex(parts[4], 32)
            || !version(parts[5]) || !hex(parts[7], 64) { return None; }
        let ips: Vec<_> = if parts[6].is_empty() { vec![] } else { parts[6].split(',').map(str::to_string).collect() };
        if ips.len() > 4 || ips.iter().any(|ip| !address(ip)) { return None; }
        let body = text.rsplit_once('\n')?.0;
        let mac = (0..64).step_by(2).map(|i| u8::from_str_radix(&parts[7][i..i + 2], 16).ok()).collect::<Option<Vec<_>>>()?;
        Some(Self { kind: parts[1], sender: parts[2], recipient: parts[3], challenge: parts[4], version: parts[5], addresses: ips, body, mac })
    }

    fn authenticated(&self, token: &str) -> bool {
        hmac::verify(&key(token), self.body.as_bytes(), &self.mac).is_ok()
    }
}

fn encode(kind: &str, sender: &str, recipient: &str, challenge: &str, app_version: &str, ips: &[String], token: &str) -> Option<Vec<u8>> {
    if !identity(sender) || !identity(recipient) || !hex(challenge, 32) || !version(app_version) { return None; }
    let body = format!("TP1\n{kind}\n{sender}\n{recipient}\n{challenge}\n{app_version}\n{}", ips.join(","));
    let packet = format!("{body}\n{}", hex_bytes(hmac::sign(&key(token), body.as_bytes()).as_ref())).into_bytes();
    (packet.len() <= MAX_PACKET).then_some(packet)
}

#[derive(Clone)]
struct Schedule {
    interacted: f64,
    next: f64,
}

impl Schedule {
    fn new(now: f64) -> Self { Self { interacted: now, next: now } }

    fn hint(&mut self, now: f64) {
        if now - self.interacted >= 0.5 {
            self.interacted = now;
            self.next = self.next.min(now);
        }
    }

    fn due(&mut self, now: f64, jitter: f64) -> bool {
        if now < self.next { return false; }
        let age = now - self.interacted;
        let jitter = jitter.clamp(0.0, 1.0);
        let delay = if age < 10.0 { 1.8 + 0.4 * jitter } else if age < 180.0 { 4.5 + jitter } else { 30.0 + 30.0 * jitter };
        self.next = now + delay;
        true
    }
}

struct Peer {
    phone: PairedPhone,
    schedule: Schedule,
    capable: bool,
    replay: VecDeque<(String, f64)>,
    last_request: f64,
    confirmed: Option<(SocketAddr, f64)>,
    cursor: usize,
}

struct Pending {
    peer: String,
    source: SocketAddr,
    expires: f64,
}

struct Send {
    bytes: Vec<u8>,
    target: SocketAddr,
    local: Option<IpAddr>,
}

#[derive(Default)]
struct Engine {
    id: String,
    version: String,
    local: Vec<String>,
    peers: HashMap<String, Peer>,
    pending: HashMap<String, Pending>,
    cursor: usize,
    events: Vec<PresenceEvent>,
}

impl Engine {
    fn configure(&mut self, config: Config, now: f64) {
        if self.id != config.id {
            self.peers.clear();
            self.pending.clear();
        }
        self.pending.retain(|_, pending| {
            self.peers.get(&pending.peer).is_some_and(|old| config.phones.iter().any(|phone| phone.id == pending.peer && phone.token == old.phone.token))
        });
        self.events.clear();
        self.id = config.id;
        self.version = config.version;
        let mut old = std::mem::take(&mut self.peers);
        for phone in config.phones.into_iter().take(MAX_PEERS) {
            let id = phone.id.clone();
            let mut peer = old.remove(&id).filter(|p| p.phone.token == phone.token).unwrap_or_else(|| Peer {
                phone: phone.clone(), schedule: Schedule::new(now), capable: false, replay: VecDeque::new(), last_request: f64::NEG_INFINITY, confirmed: None, cursor: 0,
            });
            peer.phone = phone;
            self.peers.insert(id, peer);
        }

    }

    fn hint(&mut self, now: f64) {
        for peer in self.peers.values_mut() { peer.schedule.hint(now); }
    }

    fn question(&mut self, id: &str, target: SocketAddr, local: Option<IpAddr>, now: f64, nonce: String) -> Option<Send> {
        if self.pending.len() >= MAX_PENDING { return None; }
        let peer = self.peers.get(id)?;
        let bytes = encode("q", &self.id, id, &nonce, &self.version, &self.local, &peer.phone.token)?;
        self.pending.insert(nonce, Pending { peer: id.to_string(), source: target, expires: now + CHALLENGE_LIFE });
        Some(Send { bytes, target, local })
    }

    fn tick(&mut self, now: f64, mut nonce: impl FnMut() -> String) -> Vec<Send> {
        self.pending.retain(|_, value| value.expires > now);
        let mut ids: Vec<_> = self.peers.keys().cloned().collect();
        ids.sort();
        let count = ids.len();
        if count == 0 { return vec![]; }
        ids.rotate_left(self.cursor % count);
        self.cursor = (self.cursor + 16) % count;
        let mut sends = Vec::new();
        for id in ids.into_iter().take(16) {
            let peer = self.peers.get_mut(&id).unwrap();
            if now < peer.schedule.next { continue; }
            let jitter = nonce().chars().next().and_then(|digit| digit.to_digit(16)).unwrap_or(0) as f64 / 15.0;
            if !peer.schedule.due(now, jitter) { continue; }
            self.events.push(PresenceEvent::Probe { phone_id: id.clone() });
            if !peer.capable { continue; }
            let ips = presence_addresses(peer.phone.active_ip.iter().cloned().chain(peer.phone.ips.iter().cloned()));
            if ips.is_empty() { continue; }
            let ip = ips[peer.cursor % ips.len()].parse::<IpAddr>().unwrap();
            peer.cursor = (peer.cursor + 1) % ips.len();
            let rotating = SocketAddr::new(ip, PORT);
            let confirmed = peer.confirmed.filter(|(_, at)| now - at < 120.0).map(|(source, _)| source);
            if let Some(target) = confirmed.filter(|source| *source != rotating) {
                if let Some(send) = self.question(&id, target, None, now, nonce()) { sends.push(send); }
            }
            if let Some(send) = self.question(&id, rotating, None, now, nonce()) { sends.push(send); }
        }
        sends
    }

    fn receive(&mut self, bytes: &[u8], source: SocketAddr, local: IpAddr, now: f64, nonce: String) -> (Vec<Send>, Option<PresenceEvent>) {
        if !address(&source.ip().to_string()) { return (vec![], None); }
        let Some(packet) = Packet::parse(bytes) else { return (vec![], None); };
        if packet.recipient != self.id { return (vec![], None); }
        let Some(peer) = self.peers.get_mut(packet.sender) else { return (vec![], None); };
        if !packet.authenticated(&peer.phone.token) { return (vec![], None); }
        if packet.kind == "a" {
            let Some(pending) = self.pending.get(packet.challenge) else { return (vec![], None); };
            if pending.peer != packet.sender || pending.source != source || pending.expires <= now { return (vec![], None); }
            self.pending.remove(packet.challenge);
            peer.confirmed = Some((source, now));
            return (vec![], Some(PresenceEvent::Reachable { phone_id: packet.sender.into(), address: source.ip().to_string(),
                addresses: packet.addresses, version: packet.version.into() }));
        }
        while peer.replay.front().is_some_and(|(_, at)| now - at >= REPLAY_LIFE) { peer.replay.pop_front(); }
        if now - peer.last_request < 1.0 || peer.replay.iter().any(|(nonce, _)| nonce == packet.challenge) { return (vec![], None); }
        peer.last_request = now;
        peer.replay.push_back((packet.challenge.into(), now));
        if peer.replay.len() > 512 { peer.replay.pop_front(); }
        let mut sends = Vec::new();
        if let Some(bytes) = encode("a", &self.id, packet.sender, packet.challenge, &self.version, &self.local, &peer.phone.token) {
            sends.push(Send { bytes, target: source, local: Some(local) });
        }
        let confirmed = peer.confirmed.is_some_and(|(address, at)| address == source && now - at < CHALLENGE_LIFE);
        if !confirmed && !self.pending.values().any(|pending| pending.peer == packet.sender && pending.source == source && pending.expires > now) {
            if let Some(send) = self.question(packet.sender, source, Some(local), now, nonce) { sends.push(send); }
        }
        (sends, None)
    }
}

trait Transport {
    fn send(&mut self, send: Send);
}

struct Sockets(Vec<(IpAddr, UdpSocket)>);

impl Transport for Sockets {
    fn send(&mut self, send: Send) {
        let preferred = send.local.or_else(|| {
            let route = UdpSocket::bind("0.0.0.0:0").ok()?;
            route.connect(send.target).ok()?;
            Some(route.local_addr().ok()?.ip())
        });
        let socket = self.0.iter().find(|(ip, _)| Some(*ip) == preferred).or_else(|| self.0.first());
        if let Some((_, socket)) = socket { let _ = socket.send_to(&send.bytes, send.target); }
    }
}

fn dispatch(transport: &mut impl Transport, sends: Vec<Send>) {
    for send in sends { transport.send(send); }
}

fn event(controls: &Controls, value: PresenceEvent, revision: u64) {
    let mut events = controls.events.lock().unwrap();
    if controls.revision.load(Ordering::Acquire) != revision { return; }
    if events.len() == 64 { events.pop_front(); }
    events.push_back(value);
}

fn run(controls: Arc<Controls>) {
    let start = Instant::now();
    let mut engine = Engine::default();
    let mut sockets = Sockets(Vec::new());
    let mut adapter_ips = Vec::new();
    let mut scan_at = 0.0;
    let mut initialized = false;
    let mut degraded = false;
    let mut previous = 0.0;
    let mut previous_wall = SystemTime::now();
    let mut revision = 0;
    while !controls.stopped.load(Ordering::Relaxed) {
        let now = start.elapsed().as_secs_f64();
        let wall = SystemTime::now();
        if now - previous > 10.0 || wall.duration_since(previous_wall).is_ok_and(|gap| gap > Duration::from_secs(10)) {
            engine.hint(now);
            scan_at = 0.0;
        }
        previous = now;
        previous_wall = wall;
        let config = controls.config.lock().unwrap().take();
        if let Some((configured_revision, config)) = config {
            revision = configured_revision;
            initialized = true;
            let rotated: Vec<_> = config.phones.iter().filter(|phone| {
                (!engine.id.is_empty() && engine.id != config.id) || engine.peers.get(&phone.id).is_some_and(|old| old.phone.token != phone.token)
            }).map(|phone| phone.id.clone()).collect();
            engine.configure(config, now);
            let mut capabilities = controls.capabilities.lock().unwrap();
            capabilities.retain(|id, _| engine.peers.contains_key(id) && !rotated.contains(id));
        }
        if controls.hint.swap(false, Ordering::Relaxed) {
            engine.hint(now);
            scan_at = 0.0;
        }
        if !initialized { std::thread::sleep(Duration::from_millis(50)); continue; }
        if now >= scan_at {
            scan_at = now + 5.0;
            let adapters = AdapterAddresses::new(crate::net::pairing_addresses().into_iter().map(|item| item.ip));
            let changed = adapters.bound != adapter_ips;
            if changed || sockets.0.len() != adapters.bound.len() || sockets.0.is_empty() {
                let mut fresh = Vec::new();
                for ip in &adapters.bound {
                    let addr: IpAddr = ip.parse().unwrap();
                    if let Some(index) = sockets.0.iter().position(|(old, _)| *old == addr) {
                        fresh.push(sockets.0.swap_remove(index));
                    } else if let Ok(socket) = UdpSocket::bind(SocketAddr::new(addr, PORT)) {
                        if socket.set_nonblocking(true).is_ok() { fresh.push((addr, socket)); }
                    }
                }
                sockets = Sockets(fresh);
                adapter_ips = adapters.bound;
                engine.local = adapters.advertised;
                if changed { engine.hint(now); }
            }
            let failed = sockets.0.len() < adapter_ips.len() || sockets.0.is_empty();
            if failed && !degraded { event(&controls, PresenceEvent::Degraded("UDP presence could not bind every network interface".into()), revision); }
            if !failed && degraded { event(&controls, PresenceEvent::Restored, revision); }
            degraded = failed;
        }
        {
            let capabilities = controls.capabilities.lock().unwrap();
            for (id, peer) in &mut engine.peers {
                peer.capable = capabilities.get(id).copied().unwrap_or(false);
            }
        }
        dispatch(&mut sockets, engine.tick(now, || crate::util::random_hex(16)));
        for scheduled in engine.events.drain(..) { event(&controls, scheduled, revision); }
        let mut incoming = Vec::new();
        for (local, socket) in &sockets.0 {
            for _ in 0..16 {
                let mut bytes = [0; MAX_PACKET + 1];
                match socket.recv_from(&mut bytes) {
                    Ok((len, source)) => incoming.push((bytes[..len].to_vec(), source, *local)),
                    Err(_) => break,
                }
            }
        }
        for (bytes, source, local) in incoming.into_iter().take(64) {
            let (sends, received) = engine.receive(&bytes, source, local, now, crate::util::random_hex(16));
            dispatch(&mut sockets, sends);
            if let Some(received) = received { event(&controls, received, revision); }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phone(token: &str) -> PairedPhone {
        PairedPhone { id: "phone".into(), name: "Phone".into(), token: token.into(), ips: vec!["192.168.1.20".into()], active_ip: None, cert_sha256: String::new() }
    }

    fn engine() -> Engine {
        let mut engine = Engine::default();
        engine.configure(Config { id: "desktop".into(), version: "3.6.0".into(), phones: vec![phone("secret")] }, 0.0);
        engine.local = vec!["192.168.1.10".into()];
        engine.peers.get_mut("phone").unwrap().capable = true;
        engine
    }

    fn nonce(n: u32) -> String { format!("{n:032x}") }
    fn source() -> SocketAddr { "192.168.1.20:8769".parse().unwrap() }
    fn local() -> IpAddr { "192.168.1.10".parse().unwrap() }

    #[derive(Default)]
    struct FakeTransport(Vec<Send>);
    impl Transport for FakeTransport {
        fn send(&mut self, send: Send) { self.0.push(send); }
    }

    #[test]
    fn fake_clock_visits_each_schedule_phase_and_coalesces_hints() {
        let mut schedule = Schedule::new(0.0);
        assert!(schedule.due(0.0, 0.5));
        assert!(!schedule.due(1.9, 0.5));
        assert!(schedule.due(2.0, 0.5));
        assert!(schedule.due(10.0, 0.5));
        assert_eq!(schedule.next, 15.0);
        assert!(schedule.due(180.0, 0.5));
        assert_eq!(schedule.next, 225.0);
        schedule.hint(181.0);
        assert!(schedule.due(181.0, 0.5));
        schedule.hint(181.1);
        assert!(!schedule.due(181.1, 0.5));
        assert!(schedule.due(183.0, 0.5));
    }

    #[test]
    fn mac_covers_all_fields_and_never_contains_the_token() {
        let encoded = encode("q", "phone", "desktop", &nonce(1), "3.6.0", &["192.168.1.20".into()], "secret").unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("secret"));
        assert!(String::from_utf8_lossy(&encoded).ends_with("4ffb45918a4d12c2148314eb126b1eb4416743880ce41e4453c338836814f1f9"));
        assert!(Packet::parse(&encoded).unwrap().authenticated("secret"));
        assert!(!Packet::parse(&encoded).unwrap().authenticated("other"));
        for (old, new) in [("TP1", "TP2"), ("\nq\n", "\na\n"), ("phone", "other"), ("desktop", "another"), ("3.6.0", "3.7.0"), ("192.168.1.20", "192.168.1.21")] {
            let changed = String::from_utf8(encoded.clone()).unwrap().replace(old, new);
            assert!(!Packet::parse(changed.as_bytes()).is_some_and(|packet| packet.authenticated("secret")));
        }
    }

    #[test]
    fn unsolicited_answers_expiry_and_replays_do_not_prove_reachability() {
        let mut engine = engine();
        let answer = encode("a", "phone", "desktop", &nonce(1), "3.7.0", &[], "secret").unwrap();
        assert!(engine.receive(&answer, source(), local(), 0.0, nonce(2)).1.is_none());
        engine.question("phone", source(), Some(local()), 0.0, nonce(1)).unwrap();
        let event = engine.receive(&answer, source(), local(), 1.0, nonce(2)).1.unwrap();
        assert_eq!(event, PresenceEvent::Reachable { phone_id: "phone".into(), address: "192.168.1.20".into(), addresses: vec![], version: "3.7.0".into() });
        assert!(engine.receive(&answer, source(), local(), 2.0, nonce(2)).1.is_none());
        let request = encode("q", "phone", "desktop", &nonce(4), "3.7.0", &[], "secret").unwrap();
        assert_eq!(engine.receive(&request, source(), local(), 2.0, nonce(5)).0.len(), 1, "a confirmed return path does not start a reply loop");
        engine.question("phone", source(), Some(local()), 2.0, nonce(1)).unwrap();
        assert!(engine.receive(&answer, source(), local(), 12.0, nonce(2)).1.is_none());
    }

    #[test]
    fn request_gets_bounded_answer_and_return_challenge_without_changing_schedule() {
        let mut engine = engine();
        let request = encode("q", "phone", "desktop", &nonce(1), "", &[], "secret").unwrap();
        let mut transport = FakeTransport::default();
        let (sends, event) = engine.receive(&request, source(), local(), 50.0, nonce(2));
        assert!(event.is_none());
        dispatch(&mut transport, sends);
        assert_eq!(transport.0.len(), 2);
        assert!(transport.0.iter().all(|send| send.local == Some(local()) && send.target == source()));
        assert_eq!(engine.peers["phone"].schedule.interacted, 0.0);
        assert!(engine.receive(&request, source(), local(), 52.0, nonce(3)).0.is_empty());
        assert!(engine.receive(&request, source(), local(), 52.0, nonce(3)).1.is_none());
    }

    #[test]
    fn revocation_and_token_rotation_invalidate_pending_and_replay_state() {
        let mut engine = engine();
        engine.question("phone", source(), None, 0.0, nonce(1));
        engine.configure(Config { id: "desktop".into(), version: "3.6.0".into(), phones: vec![phone("new-secret")] }, 1.0);
        assert!(engine.pending.is_empty());
        let old = encode("a", "phone", "desktop", &nonce(1), "3.6.0", &[], "secret").unwrap();
        assert!(engine.receive(&old, source(), local(), 2.0, nonce(2)).1.is_none());
        engine.configure(Config { id: "desktop".into(), version: "3.6.0".into(), phones: vec![] }, 3.0);
        assert!(engine.pending.is_empty());
        assert!(engine.receive(&old, source(), local(), 4.0, nonce(2)).1.is_none());
    }

    #[test]
    fn unknown_and_legacy_capabilities_send_nothing_but_still_allow_authenticated_answers() {
        let mut engine = engine();
        engine.peers.get_mut("phone").unwrap().capable = false;
        assert!(engine.tick(0.0, || nonce(1)).is_empty());
        let request = encode("q", "phone", "desktop", &nonce(1), "", &[], "secret").unwrap();
        assert_eq!(engine.receive(&request, source(), local(), 1.0, nonce(2)).0.len(), 2);
        engine.peers.get_mut("phone").unwrap().capable = true;
        let mut transport = FakeTransport::default();
        dispatch(&mut transport, engine.tick(2.0, || nonce(3)));
        assert_eq!(transport.0.len(), 1);
        assert_eq!(transport.0[0].target, source());
    }

    #[test]
    fn a_working_route_is_kept_fresh_while_other_candidates_rotate() {
        let mut engine = engine();
        let peer = engine.peers.get_mut("phone").unwrap();
        peer.phone.ips = vec!["192.168.1.21".into(), "192.168.1.22".into(), "100.64.1.2".into()];
        peer.confirmed = Some((source(), 180.0));
        let mut sequence = 0;
        let sends = engine.tick(180.0, || { sequence += 1; nonce(sequence) });
        assert_eq!(sends.len(), 2);
        assert!(sends.iter().any(|send| send.target == source()));
        assert!(sends.iter().any(|send| send.target.ip().to_string() == "192.168.1.21"));
    }

    #[test]
    fn credential_revision_discards_queued_and_late_events() {
        let controls = Controls::default();
        controls.revision.store(1, Ordering::Release);
        event(&controls, PresenceEvent::Probe { phone_id: "phone".into() }, 1);
        assert_eq!(controls.events.lock().unwrap().len(), 1);
        controls.revision.store(2, Ordering::Release);
        controls.events.lock().unwrap().clear();
        event(&controls, PresenceEvent::Probe { phone_id: "phone".into() }, 1);
        assert!(controls.events.lock().unwrap().is_empty());
        event(&controls, PresenceEvent::Probe { phone_id: "phone".into() }, 2);
        assert_eq!(controls.events.lock().unwrap().len(), 1);
    }

    #[test]
    fn reconfiguration_keeps_schedule_and_version_differences_are_data() {
        let mut engine = engine();
        engine.peers.get_mut("phone").unwrap().schedule.due(0.0, 0.5);
        engine.configure(Config { id: "desktop".into(), version: "3.7.0".into(), phones: vec![phone("secret")] }, 1.0);
        assert_eq!(engine.peers["phone"].schedule.next, 2.0);
        assert_eq!(engine.peers["phone"].schedule.interacted, 0.0);
        let packet = encode("q", "phone", "desktop", &nonce(1), "", &[], "secret").unwrap();
        assert_eq!(Packet::parse(&packet).unwrap().version, "");
        let packet = encode("q", "phone", "desktop", &nonce(1), "3.8.0-nightly.12", &[], "secret").unwrap();
        assert_eq!(Packet::parse(&packet).unwrap().version, "3.8.0-nightly.12");
    }

    #[test]
    fn bounded_candidates_keep_a_tailscale_route() {
        let ips = presence_addresses(["192.168.1.1", "10.0.0.1", "10.1.0.1", "10.2.0.1", "100.64.0.2"].map(str::to_string));
        assert_eq!(ips.len(), 4);
        assert!(ips.contains(&"100.64.0.2".into()));
    }

    #[test]
    fn socket_candidates_keep_all_adapters_while_packets_keep_four_addresses() {
        let scan = || AdapterAddresses::new(["192.168.1.1", "10.0.0.1", "10.1.0.1", "10.2.0.1", "10.3.0.1", "100.64.0.2"].map(str::to_string));
        let initial = scan();
        assert_eq!(initial.bound.len(), 6);
        assert_eq!(initial.advertised.len(), 4);
        assert!(initial.advertised.contains(&"100.64.0.2".into()));
        assert_eq!(scan().bound, initial.bound, "an unchanged scan does not rebuild sockets or reset the fast schedule");
        let mut changed = initial.bound.clone();
        changed[4] = "10.4.0.1".into();
        assert_ne!(AdapterAddresses::new(changed).bound, initial.bound, "changes beyond the advertised four still rebuild sockets");
    }

    #[test]
    fn malformed_oversized_or_unroutable_packets_are_rejected() {
        assert!(Packet::parse(&vec![b'a'; MAX_PACKET + 1]).is_none());
        assert!(encode("q", "bad\nidentity", "desktop", &nonce(1), "3.6.0", &[], "secret").is_none());
        let bad = encode("q", "phone", "desktop", &nonce(1), "3.6.0", &["127.0.0.1".into()], "secret").unwrap();
        assert!(Packet::parse(&bad).is_none());
    }
}
