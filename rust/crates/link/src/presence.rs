//! Authenticated reachability hints, independent of camera and stream ownership.

use crate::net::presence_addresses;
use polling::{Event, Events, Poller};
use ring::hmac;
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use telescope_core::source::PairedPhone;

pub const PORT: u16 = 7483;
const MAX_PACKET: usize = 512;
const MAX_PEERS: usize = 64;
const MAX_PENDING: usize = 256;
const CHALLENGE_LIFE: f64 = 10.0;
const REPLAY_LIFE: f64 = 300.0;
const HEARD_TIMEOUT: f64 = 120.0;

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
    poller: OnceLock<Arc<Poller>>,
    config: Mutex<Option<(u64, Config)>>,
    credentials: Mutex<Option<Credentials>>,
    revision: AtomicU64,
    capabilities: Mutex<HashMap<String, bool>>,
    events: Mutex<VecDeque<PresenceEvent>>,
    checks: Mutex<VecDeque<(PairedPhone, bool)>>,
    hint: AtomicBool,
    wake: Mutex<Option<Arc<dyn Fn() + std::marker::Send + Sync>>>,
    stopped: AtomicBool,
}

impl Controls {
    fn notify(&self) {
        if let Some(poller) = self.poller.get() { let _ = poller.notify(); }
    }
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
            self.controls.checks.lock().unwrap().clear();
            *previous = Some(credentials);
        }
        let revision = self.controls.revision.load(Ordering::Acquire);
        *self.controls.config.lock().unwrap() = Some((revision, Config { id: computer_id, version, phones }));
        self.controls.notify();
    }

    pub fn capability(&self, phone_id: &str, enabled: bool) {
        let mut capabilities = self.controls.capabilities.lock().unwrap();
        if capabilities.len() < MAX_PEERS || capabilities.contains_key(phone_id) {
            capabilities.insert(phone_id.to_string(), enabled);
            self.controls.notify();
        }
    }

    pub fn hint(&self) {
        self.controls.hint.store(true, Ordering::Relaxed);
        self.controls.notify();
    }

    pub fn set_wake(&self, wake: Option<Arc<dyn Fn() + std::marker::Send + Sync>>) {
        *self.controls.wake.lock().unwrap() = wake;
    }

    pub fn take_events(&self) -> Vec<PresenceEvent> {
        self.controls.events.lock().unwrap().drain(..).collect()
    }

    // Keep route-check diagnostics on the presence worker's clock.
    pub fn checked(&self, phone: &PairedPhone, unreachable: bool) {
        let mut checks = self.controls.checks.lock().unwrap();
        if checks.len() == MAX_PEERS { checks.pop_front(); }
        checks.push_back((phone.clone(), unreachable));
        self.controls.notify();
    }

    pub fn stop(&self) {
        self.controls.stopped.store(true, Ordering::Relaxed);
        self.controls.notify();
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
    last_heard: Option<Heard>,
    quiet: bool,
    unreachable: bool,
    cursor: usize,
}

struct Heard {
    source: SocketAddr,
    at: f64,
    initiator: &'static str,
}

impl Peer {
    fn heard(&mut self, source: SocketAddr, now: f64, initiator: &'static str) {
        if self.last_heard.is_none() || self.quiet {
            let state = if self.last_heard.is_none() { "first heard" } else { "heard again" };
            log::info!("Presence: {} {state} from {source} ({initiator} started the exchange)", self.phone.name);
        }
        self.last_heard = Some(Heard { source, at: now, initiator });
        self.quiet = false;
    }

    fn timeout(&mut self, now: f64) {
        if let Some(heard) = &self.last_heard {
            if !self.quiet && now - heard.at >= HEARD_TIMEOUT {
                log::info!("Presence: {} went quiet; last heard {:.1}s ago from {} ({} started the exchange)",
                    self.phone.name, now - heard.at, heard.source, heard.initiator);
                self.quiet = true;
            }
        }
    }

    fn checked(&mut self, unreachable: bool, now: f64) {
        if unreachable && !self.unreachable {
            if let Some(heard) = &self.last_heard {
                log::info!("{} is unreachable; presence last heard {:.1}s ago from {}", self.phone.name, now - heard.at, heard.source);
            } else {
                log::info!("{} is unreachable; presence last heard: never since start", self.phone.name);
            }
        }
        self.unreachable = unreachable;
    }
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
    fn wait_for(&self, now: f64, scan_at: f64) -> Duration {
        let next = self.peers.values().map(|peer| peer.schedule.next).fold(scan_at, f64::min);
        Duration::from_secs_f64((next - now).max(0.0))
    }

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
                phone: phone.clone(), schedule: Schedule::new(now), capable: false, replay: VecDeque::new(), last_request: f64::NEG_INFINITY,
                confirmed: None, last_heard: None, quiet: false, unreachable: false, cursor: 0,
            });
            peer.phone = phone;
            self.peers.insert(id, peer);
        }

    }

    fn hint(&mut self, now: f64) {
        for peer in self.peers.values_mut() { peer.schedule.hint(now); }
    }

    fn checked(&mut self, phone: &PairedPhone, unreachable: bool, now: f64) {
        if let Some(peer) = self.peers.get_mut(&phone.id).filter(|peer| peer.phone.token == phone.token) {
            peer.checked(unreachable, now);
        }
    }

    fn question(&mut self, id: &str, target: SocketAddr, local: Option<IpAddr>, now: f64, nonce: String) -> Option<Send> {
        if self.pending.len() >= MAX_PENDING { return None; }
        let peer = self.peers.get(id)?;
        let bytes = encode("q", &self.id, id, &nonce, &self.version, &self.local, &peer.phone.token)?;
        self.pending.insert(nonce, Pending { peer: id.to_string(), source: target, expires: now + CHALLENGE_LIFE });
        Some(Send { bytes, target, local })
    }

    fn tick(&mut self, now: f64, mut nonce: impl FnMut() -> String) -> Vec<Send> {
        for peer in self.peers.values_mut() { peer.timeout(now); }
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
            peer.heard(source, now, "desktop");
            return (vec![], Some(PresenceEvent::Reachable { phone_id: packet.sender.into(), address: source.ip().to_string(),
                addresses: packet.addresses, version: packet.version.into() }));
        }
        while peer.replay.front().is_some_and(|(_, at)| now - at >= REPLAY_LIFE) { peer.replay.pop_front(); }
        if now - peer.last_request < 1.0 || peer.replay.iter().any(|(nonce, _)| nonce == packet.challenge) { return (vec![], None); }
        peer.last_request = now;
        peer.replay.push_back((packet.challenge.into(), now));
        if peer.replay.len() > 512 { peer.replay.pop_front(); }
        peer.heard(source, now, "phone");
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

struct Sockets(Vec<(IpAddr, UdpSocket)>, Arc<Poller>);

impl Sockets {
    fn wait(&self, events: &mut Events, timeout: Option<Duration>) -> std::io::Result<()> {
        for (index, (_, socket)) in self.0.iter().enumerate() {
            self.1.modify(socket, Event::readable(index))?;
        }
        events.clear();
        self.1.wait(events, timeout)?;
        Ok(())
    }
}

impl Drop for Sockets {
    fn drop(&mut self) {
        for (_, socket) in &self.0 { let _ = self.1.delete(socket); }
    }
}

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
    {
        let mut events = controls.events.lock().unwrap();
        if controls.revision.load(Ordering::Acquire) != revision { return; }
        if events.len() == 64 { events.pop_front(); }
        events.push_back(value);
    }
    let wake = controls.wake.lock().unwrap().clone();
    if let Some(wake) = wake { wake(); }
}

fn run(controls: Arc<Controls>) {
    let poller = match Poller::new() {
        Ok(poller) => Arc::new(poller),
        Err(error) => {
            event(&controls, PresenceEvent::Degraded(format!("UDP presence could not wait for sockets: {error}")), controls.revision.load(Ordering::Acquire));
            return;
        }
    };
    let _ = controls.poller.set(poller.clone());
    let mut ready = Events::new();
    let start = Instant::now();
    let mut engine = Engine::default();
    let mut sockets = Sockets(Vec::new(), poller.clone());
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
            scan_at = 0.0; // a phone may have come: its interfaces are looked at now
            let mut capabilities = controls.capabilities.lock().unwrap();
            capabilities.retain(|id, _| engine.peers.contains_key(id) && !rotated.contains(id));
        }
        if controls.hint.swap(false, Ordering::Relaxed) {
            engine.hint(now);
            scan_at = 0.0;
        }
        if !initialized {
            if sockets.wait(&mut ready, None).is_err() { break; }
            continue;
        }
        if now >= scan_at {
            // With no phone paired there is no one to hear from: the interfaces are looked at again when one is added (configure) or on a hint
            scan_at = now + if engine.peers.is_empty() { 3600.0 } else { 5.0 };
            let adapters = AdapterAddresses::new(crate::net::pairing_addresses().into_iter().map(|item| item.ip));
            let changed = adapters.bound != adapter_ips;
            if changed || sockets.0.len() != adapters.bound.len() || sockets.0.is_empty() {
                let mut fresh = Vec::new();
                for ip in &adapters.bound {
                    let addr: IpAddr = ip.parse().unwrap();
                    if let Some(index) = sockets.0.iter().position(|(old, _)| *old == addr) {
                        fresh.push(sockets.0.swap_remove(index));
                    } else if let Ok(socket) = UdpSocket::bind(SocketAddr::new(addr, PORT)) {
                        if socket.set_nonblocking(true).is_ok() {
                            // SAFETY: Sockets unregisters every added socket before dropping it.
                            if unsafe { poller.add(&socket, Event::readable(fresh.len())) }.is_ok() { fresh.push((addr, socket)); }
                        }
                    }
                }
                sockets = Sockets(fresh, poller.clone());
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
        for (phone, unreachable) in controls.checks.lock().unwrap().drain(..) {
            engine.checked(&phone, unreachable, now);
        }
        if sockets.wait(&mut ready, Some(engine.wait_for(start.elapsed().as_secs_f64(), scan_at))).is_err() { break; }
    }
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
