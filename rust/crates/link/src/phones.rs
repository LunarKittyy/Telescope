//! Paired phones and how to reach them (phones.py). adb, the session client and LAN discovery are passed in.
//!
//! Every resolve starts from scratch and prefers USB whenever this phone answers over a cable. "Answers" is checked: a
//! plugged-in device only counts once its /v1/hello says the paired phone's id. Whenever the result isn't USB the
//! resolution says why, so the window can show it next to the route.

use crate::https::{is_fingerprint, PhoneAuth};
use crate::net;
use crate::session_client::{Hello, HelloStatus, PingStatus, SessionApi, SessionClient, PING_PORT, SESSION_PROTOCOL};
use crate::util::clean_name;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::i18n::tr;
use telescope_core::link::{PhoneStatus, Resolution, RoutePreference, UsbNote};
use telescope_core::source::{PairedPhone, RouteSpec};

/// The phone's stream server; fixed on the phone, so not a setting here.
pub const STREAM_PORT: u16 = 8080;

pub fn auth_of(phone: &PairedPhone) -> PhoneAuth {
    PhoneAuth::new(&phone.token, &phone.cert_sha256)
}

/// A saved phone entry, checked the way Phone.from_dict does: id, name and token needed, a bad pin dropped.
pub fn phone_from_value(raw: &Value) -> Result<PairedPhone, String> {
    let obj = raw.as_object().ok_or("phone entry is not an object")?;
    let text = |k: &str| obj.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let (Some(id), Some(name), Some(token)) = (text("id"), text("name"), text("token")) else {
        return Err("phone entry needs id, name and token".into());
    };
    let name = Some(clean_name(&name)).filter(|n| !n.is_empty()).unwrap_or_else(|| "Phone".into());
    let ips = match obj.get("ips") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(|i| i.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>().ok_or("phone entry 'ips' must be a list of strings")?,
        Some(_) => return Err("phone entry 'ips' must be a list of strings".into()),
    };
    let active_ip = obj.get("active_ip").and_then(Value::as_str).map(str::to_string);
    let pin = obj.get("cert_sha256").and_then(Value::as_str).filter(|p| is_fingerprint(p)).unwrap_or("");
    Ok(PairedPhone { id, name, token, ips, active_ip, cert_sha256: pin.to_string() })
}

/// The entry as Phone.to_dict saves it.
pub fn phone_to_value(phone: &PairedPhone) -> Value {
    serde_json::json!({"id": phone.id, "name": phone.name, "token": phone.token, "ips": phone.ips,
                       "active_ip": phone.active_ip, "cert_sha256": phone.cert_sha256})
}

/// The host a route reaches the phone at: its address, or localhost through an adb forward.
pub fn route_host(route: &RouteSpec) -> &str {
    match route {
        RouteSpec::Usb { .. } => "127.0.0.1",
        RouteSpec::Network { address } => address,
    }
}

pub fn is_usb(route: &RouteSpec) -> bool {
    matches!(route, RouteSpec::Usb { .. })
}

type Discover = Box<dyn Fn(&str) -> Vec<String> + Send + Sync>;
type Forward = Box<dyn Fn(&str, u16) -> Option<u16> + Send + Sync>;
type Unforward = Arc<dyn Fn(&str, u16) + Send + Sync>;

/// Refcounted `adb forward tcp:0 tcp:<port>` per (serial, port).
///
/// The status probe, a phone wake and a remote stop can overlap, and each one closing "its" forward used to tear down
/// the tunnel another was still using. tcp:0 lets adb pick a free local port, so two plugged-in phones don't fight
/// over one.
///
/// A port given a linger keeps its forward for that long after the last release, so a check followed by a start (or
/// the next check) doesn't run `adb forward` and `adb forward --remove` again and again.
pub struct UsbTunnels {
    forward: Forward,
    unforward: Unforward,
    /// Removes a forward before returning (the normal one hands it to a thread), for quitting.
    unforward_now: Option<Unforward>,
    held: Arc<Mutex<HashMap<(String, u16), Held>>>,
    /// Held while `adb forward` runs, so two opens of one forward don't both run it without blocking `held` meanwhile.
    opening: Mutex<()>,
    lingers: HashMap<u16, Duration>,
}

struct Held {
    local: u16,
    /// Zero while the forward only lingers.
    refs: u32,
    /// Bumped on each release to zero, so a linger's expiry knows whether it is still the latest.
    idle: u64,
}

impl UsbTunnels {
    pub fn new(forward: impl Fn(&str, u16) -> Option<u16> + Send + Sync + 'static,
               unforward: impl Fn(&str, u16) + Send + Sync + 'static) -> UsbTunnels {
        UsbTunnels { forward: Box::new(forward), unforward: Arc::new(unforward), unforward_now: None, held: Arc::default(), opening: Mutex::new(()), lingers: HashMap::new() }
    }

    /// How shutdown removes forwards without a thread that quitting would cut short.
    pub fn with_blocking_unforward(mut self, unforward: impl Fn(&str, u16) + Send + Sync + 'static) -> UsbTunnels {
        self.unforward_now = Some(Arc::new(unforward));
        self
    }

    /// Quitting: removes every forward still held or lingering, waiting up to a few seconds for adb.
    pub fn shutdown(&self) {
        let entries: Vec<(String, u16)> = self.held.lock().unwrap().drain().map(|((serial, _), e)| (serial, e.local)).collect();
        if entries.is_empty() {
            return;
        }
        let unforward = self.unforward_now.clone().unwrap_or_else(|| self.unforward.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let count = entries.len();
        for (serial, local) in entries {
            let (unforward, tx) = (unforward.clone(), tx.clone());
            std::thread::spawn(move || {
                unforward(&serial, local);
                let _ = tx.send(());
            });
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        for _ in 0..count {
            if rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).is_err() {
                break;
            }
        }
    }

    /// Keep the forward to remote_port for linger after its last release.
    pub fn with_linger(mut self, remote_port: u16, linger: Duration) -> UsbTunnels {
        self.lingers.insert(remote_port, linger);
        self
    }

    pub fn acquire(&self, serial: &str, remote_port: u16) -> Option<u16> {
        self.acquire_noting(serial, remote_port).map(|(local, _)| local)
    }

    /// Like acquire, and whether the forward was one that only lingered (so an adb restart may have dropped it).
    pub fn acquire_noting(&self, serial: &str, remote_port: u16) -> Option<(u16, bool)> {
        if let Some(found) = self.join(serial, remote_port) {
            return Some(found);
        }
        // adb can take seconds: `held` stays free meanwhile so a release on the window's thread never waits for it
        let _opening = self.opening.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(found) = self.join(serial, remote_port) {
            return Some(found);
        }
        let local = (self.forward)(serial, remote_port)?;
        self.held.lock().unwrap().insert((serial.to_string(), remote_port), Held { local, refs: 1, idle: 0 });
        Some((local, false))
    }

    /// Takes another hold on a forward that exists: its local port and whether it only lingered.
    fn join(&self, serial: &str, remote_port: u16) -> Option<(u16, bool)> {
        let mut held = self.held.lock().unwrap();
        let entry = held.get_mut(&(serial.to_string(), remote_port))?;
        let lingered = entry.refs == 0;
        entry.refs += 1;
        Some((entry.local, lingered))
    }

    /// The local port of a forward someone holds, without holding it too.
    pub fn held_port(&self, serial: &str, remote_port: u16) -> Option<u16> {
        self.held.lock().unwrap().get(&(serial.to_string(), remote_port)).filter(|e| e.refs > 0).map(|e| e.local)
    }

    pub fn release(&self, serial: &str, remote_port: u16) {
        self.let_go(serial, remote_port, false);
    }

    /// release for a forward that didn't work: it goes at once rather than lingering, so the next acquire opens a fresh one.
    pub fn release_broken(&self, serial: &str, remote_port: u16) {
        self.let_go(serial, remote_port, true);
    }

    /// Drops a forward nobody holds right now (one that lingers), e.g. after a start over it failed.
    pub fn drop_idle(&self, serial: &str, remote_port: u16) {
        let mut held = self.held.lock().unwrap();
        let key = (serial.to_string(), remote_port);
        if held.get(&key).is_some_and(|e| e.refs == 0) {
            if let Some(entry) = held.remove(&key) {
                (self.unforward)(serial, entry.local);
            }
        }
    }

    fn let_go(&self, serial: &str, remote_port: u16, broken: bool) {
        let mut held = self.held.lock().unwrap();
        let key = (serial.to_string(), remote_port);
        let Some(entry) = held.get_mut(&key) else { return };
        entry.refs = entry.refs.saturating_sub(1);
        if entry.refs > 0 {
            return;
        }
        let linger = self.lingers.get(&remote_port).copied().filter(|d| !d.is_zero() && !broken);
        let Some(linger) = linger else {
            let local = entry.local;
            held.remove(&key);
            (self.unforward)(serial, local);
            return;
        };
        entry.idle += 1;
        let (idle, held_map, unforward) = (entry.idle, self.held.clone(), self.unforward.clone());
        let serial = serial.to_string();
        drop(held);
        let _ = std::thread::Builder::new().name("adb-linger".into()).spawn(move || {
            std::thread::sleep(linger);
            let mut held = held_map.lock().unwrap();
            let key = (serial.clone(), remote_port);
            if held.get(&key).is_some_and(|e| e.refs == 0 && e.idle == idle) {
                if let Some(entry) = held.remove(&key) {
                    unforward(&serial, entry.local);
                }
            }
        });
    }
}

/// Whether something still listens at the local end of an adb forward (adb's server, as long as it hasn't been restarted).
fn forward_is_up(local: u16) -> bool {
    std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], local).into(), Duration::from_millis(300)).is_ok()
}

/// A phone that speaks another session protocol: which side needs the update.
fn version_mismatch(hello: &Hello, route: RouteSpec) -> Option<Resolution> {
    if hello.protocol == SESSION_PROTOCOL {
        return None;
    }
    let status = if hello.protocol < SESSION_PROTOCOL { PhoneStatus::PhoneOutdated } else { PhoneStatus::DesktopOutdated };
    Some(Resolution { route: Some(route), phone_version: hello.app_version.clone(), ..Resolution::new(status) })
}

fn ready(route: RouteSpec, hello: &Hello, ping: &crate::session_client::Ping) -> Resolution {
    Resolution { route: Some(route), streaming: ping.streaming, busy: ping.busy, streaming_for: ping.streaming_for.clone().unwrap_or_default(),
                 phone_version: hello.app_version.clone(), udp_dynamic: ping.udp_dynamic, presence: ping.presence && !ping.local_only, ..Resolution::new(PhoneStatus::Ready) }
}

pub type ClientFactory = Arc<dyn Fn(&str, PhoneAuth) -> Box<dyn SessionApi> + Send + Sync>;

/// Decides how to reach a paired phone right now. Blocking: call off the window's thread.
pub struct RouteResolver {
    pub adb_available: Box<dyn Fn() -> bool + Send + Sync>,
    /// (serial, state) for everything adb sees.
    pub adb_device_states: Box<dyn Fn() -> Vec<(String, String)> + Send + Sync>,
    pub tunnels: Arc<UsbTunnels>,
    /// Phone id to its current LAN addresses from mDNS.
    pub discover: Discover,
    pub client: ClientFactory,
    pub wifi_timeout: Duration,
}

impl RouteResolver {
    pub fn real_client() -> ClientFactory {
        Arc::new(|base, auth| Box::new(SessionClient::new(base, auth)))
    }

    pub fn resolve(&self, phone: &PairedPhone, preference: RoutePreference) -> Resolution {
        // LEGACY MIGRATION PATH: a phone paired before TLS has no pin to check
        if !is_fingerprint(&phone.cert_sha256) {
            return Resolution { before_tls: true, ..Resolution::new(PhoneStatus::NotPaired) };
        }
        let mut usb_note = None;
        if preference != RoutePreference::Wifi {
            let (usb, note) = self.try_usb(phone);
            if let Some(found) = usb {
                return found;
            }
            usb_note = note;
            if preference == RoutePreference::Usb {
                return Resolution { usb_note, ..Resolution::new(PhoneStatus::UsbNeedsAttention) };
            }
        }
        let wifi = self.try_wifi(phone);
        Resolution { usb_note: if preference == RoutePreference::Auto { usb_note } else { None }, ..wifi }
    }

    fn try_usb(&self, phone: &PairedPhone) -> (Option<Resolution>, Option<UsbNote>) {
        if !(self.adb_available)() {
            return (None, Some(UsbNote::NoAdb));
        }
        let states = (self.adb_device_states)();
        if states.is_empty() {
            return (None, Some(UsbNote::NoCable));
        }
        let note = states.iter().any(|(_, s)| s == "unauthorized").then_some(UsbNote::Unauthorized);
        let mut saw_other = false;
        let mut old_app_serial: Option<String> = None;
        for (serial, state) in &states {
            if state != "device" {
                continue;
            }
            let Some((local, lingered)) = self.tunnels.acquire_noting(serial, PING_PORT) else { continue };
            let before = (saw_other, old_app_serial.clone());
            let mut found = self.probe_usb(phone, serial, local, &mut saw_other, &mut old_app_serial);
            if found.is_none() && lingered && !forward_is_up(local) {
                // A forward kept from the last check is gone (adb restarted): one try on a fresh one before saying no.
                // One that adb still serves just means the phone's app isn't answering, and a fresh one would say the same
                // while starting two adb processes every check
                self.tunnels.release_broken(serial, PING_PORT);
                (saw_other, old_app_serial) = before;
                let Some(fresh) = self.tunnels.acquire(serial, PING_PORT) else { continue };
                found = self.probe_usb(phone, serial, fresh, &mut saw_other, &mut old_app_serial);
            }
            self.tunnels.release(serial, PING_PORT);
            if found.is_some() {
                return (found, None);
            }
        }
        if let Some(serial) = old_app_serial {
            return (Some(Resolution { route: Some(RouteSpec::Usb { serial }), ..Resolution::new(PhoneStatus::PhoneOutdated) }), None);
        }
        if note.is_some() {
            return (None, note);
        }
        (None, Some(if saw_other { UsbNote::OtherPhone } else { UsbNote::AppClosed }))
    }

    fn probe_usb(&self, phone: &PairedPhone, serial: &str, local: u16, saw_other: &mut bool, old_app: &mut Option<String>)
                 -> Option<Resolution> {
        let route = RouteSpec::Usb { serial: serial.to_string() };
        let client = (self.client)(&format!("https://127.0.0.1:{local}"), auth_of(phone));
        let hello = client.hello(crate::session_client::REQUEST_TIMEOUT);
        if hello.status == HelloStatus::Missing {
            // Too old to say who it is
            old_app.get_or_insert_with(|| serial.to_string());
            return None;
        }
        if matches!(hello.status, HelloStatus::OtherCert | HelloStatus::NotTelescope) {
            *saw_other = true;
        }
        if !hello.ok() {
            return None;
        }
        if hello.phone_id != phone.id {
            *saw_other = true;
            return None;
        }
        if let Some(mismatch) = version_mismatch(&hello, route.clone()) {
            return Some(mismatch);
        }
        let ping = client.ping();
        match ping.status {
            PingStatus::NotPaired => Some(Resolution::new(PhoneStatus::NotPaired)),
            PingStatus::Paired => Some(ready(route, &hello, &ping)),
            PingStatus::Unreachable => None,
        }
    }

    pub fn wifi_candidates(&self, phone: &PairedPhone) -> Vec<String> {
        let mut ordered: Vec<String> = (self.discover)(&phone.id);
        ordered.extend(phone.active_ip.iter().cloned());
        let mut stored = phone.ips.clone();
        stored.sort_by_key(|ip| net::rank_ip(ip));
        ordered.extend(stored);
        let mut out: Vec<String> = Vec::new();
        for ip in ordered {
            if !ip.is_empty() && !out.contains(&ip) && net::valid_ipv4(&ip) {
                out.push(ip);
            }
        }
        out
    }

    fn probe_wifi(client_of: &ClientFactory, timeout: Duration, phone: &PairedPhone, ip: &str) -> Option<Resolution> {
        let client = client_of(&format!("https://{ip}:{PING_PORT}"), auth_of(phone));
        let route = RouteSpec::Network { address: ip.to_string() };
        let hello = client.hello(timeout);
        match hello.status {
            HelloStatus::Missing => return Some(Resolution { route: Some(route), ..Resolution::new(PhoneStatus::PhoneOutdated) }),
            HelloStatus::UsbOnly => {
                // Only ping answers off USB in Local only; the pinned certificate already says it's this phone
                let ping = client.ping();
                return (ping.paired() && ping.local_only).then(|| Resolution::new(PhoneStatus::LocalOnly));
            }
            _ => {}
        }
        if hello.status == HelloStatus::NotTelescope {
            return Some(Resolution { other_app: true, ..Resolution::new(PhoneStatus::Unreachable) });
        }
        if hello.status == HelloStatus::OtherCert || (hello.ok() && hello.phone_id != phone.id) {
            return Some(Resolution { other_telescope: true, ..Resolution::new(PhoneStatus::Unreachable) });
        }
        if !hello.ok() {
            return None;
        }
        if let Some(mismatch) = version_mismatch(&hello, route.clone()) {
            return Some(mismatch);
        }
        let ping = client.ping();
        match ping.status {
            PingStatus::NotPaired => Some(Resolution::new(PhoneStatus::NotPaired)),
            PingStatus::Unreachable => None,
            PingStatus::Paired if ping.local_only => Some(Resolution::new(PhoneStatus::LocalOnly)),
            PingStatus::Paired => Some(ready(route, &hello, &ping)),
        }
    }

    /// Probes every address at once (four at a time) and answers as soon as the result can't change: the first Ready,
    /// in candidate order, once every address before it has answered too. An address that never answers (another
    /// network's, a dead VPN) after it no longer holds the check up for its whole timeout.
    fn try_wifi(&self, phone: &PairedPhone) -> Resolution {
        let candidates = Arc::new(self.wifi_candidates(phone));
        if candidates.is_empty() {
            return Resolution::new(PhoneStatus::Unreachable);
        }
        let (client_of, timeout, phone) = (self.client.clone(), self.wifi_timeout, Arc::new(phone.clone()));
        let (tx, rx) = std::sync::mpsc::channel::<(usize, Option<Resolution>)>();
        let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        for _ in 0..candidates.len().min(4) {
            let (tx, next, done, candidates, client_of, phone) = (tx.clone(), next.clone(), done.clone(), candidates.clone(), client_of.clone(), phone.clone());
            std::thread::spawn(move || loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(ip) = candidates.get(i) else { break };
                if done.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                if tx.send((i, RouteResolver::probe_wifi(&client_of, timeout, &phone, ip))).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        let mut results: Vec<Option<Option<Resolution>>> = vec![None; candidates.len()];
        let answer = loop {
            if let Some(ready) = first_decided_ready(&results) {
                break ready;
            }
            match rx.recv() {
                Ok((i, found)) => results[i] = Some(found),
                Err(_) => break pick_wifi_answer(results),
            }
        };
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        answer
    }
}

/// The first Ready in candidate order, if every candidate before it has answered.
fn first_decided_ready(results: &[Option<Option<Resolution>>]) -> Option<Resolution> {
    for slot in results {
        match slot {
            None => return None,
            Some(Some(r)) if r.status == PhoneStatus::Ready => return Some(r.clone()),
            Some(_) => {}
        }
    }
    None
}

/// Candidate order decides between several answers (the discovered address first); Ready wins, then the rest by how useful.
fn pick_wifi_answer(results: Vec<Option<Option<Resolution>>>) -> Resolution {
    let answers: Vec<Resolution> = results.into_iter().flatten().flatten().collect();
    for wanted in [PhoneStatus::Ready, PhoneStatus::LocalOnly, PhoneStatus::NotPaired, PhoneStatus::PhoneOutdated, PhoneStatus::DesktopOutdated] {
        if let Some(answer) = answers.iter().find(|a| a.status == wanted) {
            return answer.clone();
        }
    }
    Resolution { other_app: answers.iter().any(|a| a.other_app), other_telescope: answers.iter().any(|a| a.other_telescope), ..Resolution::new(PhoneStatus::Unreachable) }
}

/// Why the connection isn't using USB, in the words the window shows.
pub fn usb_note_text(note: Option<UsbNote>) -> String {
    match note {
        Some(UsbNote::NoAdb) if crate::platform::windows() => tr("USB can't be used until adb is installed (Add device can get it)"),
        Some(UsbNote::NoAdb) => tr("USB can't be used until adb is installed"),
        Some(UsbNote::NoCable) => tr("no phone plugged in over USB"),
        Some(UsbNote::Unauthorized) => tr("phone plugged in but USB debugging isn't allowed yet (accept the prompt on the phone)"),
        Some(UsbNote::OtherPhone) => tr("the phone plugged in over USB is a different one"),
        Some(UsbNote::AppClosed) => tr("phone plugged in but Telescope isn't open on it"),
        None => String::new(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::session_client::Ping;

    pub fn phone() -> PairedPhone {
        PairedPhone { id: "ph-1".into(), name: "Pixel".into(), token: "tok".into(), ips: vec!["192.168.1.40".into()],
                      active_ip: None, cert_sha256: "ab".repeat(32) }
    }

    #[derive(Clone)]
    pub enum Answer {
        /// (phone id, ping status, local only, protocol)
        Phone(&'static str, PingStatus, bool, i64),
        /// Answers on the session port, but from before /v1/hello.
        OldApp,
        /// Answers over TLS with another certificate.
        OtherCert,
        /// Answers without TLS, so not a Telescope app.
        Plain,
    }

    pub fn answers(id: &'static str, status: PingStatus, local_only: bool) -> Answer {
        Answer::Phone(id, status, local_only, SESSION_PROTOCOL)
    }

    /// Who answers at which base URL.
    #[derive(Clone, Default)]
    pub struct FakeNet {
        pub answers: HashMap<String, Answer>,
        pub usb_only_hello: Vec<String>,
        pub pinged: Arc<Mutex<Vec<String>>>,
        pub auths: Arc<Mutex<Vec<PhoneAuth>>>,
        /// How long a base URL takes to answer hello.
        pub delays: HashMap<String, Duration>,
    }

    struct Client {
        base: String,
        net: FakeNet,
    }

    impl SessionApi for Client {
        fn hello(&self, _timeout: Duration) -> Hello {
            if let Some(delay) = self.net.delays.get(&self.base) {
                std::thread::sleep(*delay);
            }
            let a = self.net.answers.get(&self.base);
            if matches!(a, Some(Answer::OldApp)) {
                return Hello::status(HelloStatus::Missing);
            }
            if matches!(a, Some(Answer::OtherCert)) {
                return Hello::status(HelloStatus::OtherCert);
            }
            if matches!(a, Some(Answer::Plain)) {
                return Hello::status(HelloStatus::NotTelescope);
            }
            if self.net.usb_only_hello.contains(&self.base) {
                return Hello::status(HelloStatus::UsbOnly);
            }
            match a {
                Some(Answer::Phone(id, _, _, protocol)) => Hello { status: HelloStatus::Ok, phone_id: id.to_string(),
                    phone_name: "Phone".into(), protocol: *protocol, app_version: "1.0".into(), build: 7 },
                _ => Hello::status(HelloStatus::None),
            }
        }

        fn ping(&self) -> Ping {
            self.net.pinged.lock().unwrap().push(self.base.clone());
            match self.net.answers.get(&self.base) {
                Some(Answer::Phone(id, status, local_only, _)) => Ping { local_only: *local_only, phone_id: Some(id.to_string()),
                                                                         ..Ping::status(*status) },
                _ => Ping::status(PingStatus::Unreachable),
            }
        }
    }

    impl FakeNet {
        pub fn new(list: &[(&str, Answer)]) -> FakeNet {
            FakeNet { answers: list.iter().map(|(b, a)| (b.to_string(), a.clone())).collect(), ..FakeNet::default() }
        }

        pub fn factory(&self) -> ClientFactory {
            let net = self.clone();
            Arc::new(move |base, auth| {
                net.auths.lock().unwrap().push(auth);
                Box::new(Client { base: base.to_string(), net: net.clone() })
            })
        }
    }

    pub fn resolver(net: &FakeNet, states: &[(&str, &str)], adb: bool, discovered: &[&str]) -> (RouteResolver, Arc<Mutex<HashMap<String, u16>>>) {
        let ports: Arc<Mutex<HashMap<String, u16>>> = Arc::default();
        let held = ports.clone();
        let tunnels = UsbTunnels::new(move |serial, _remote| {
            let mut ports = held.lock().unwrap();
            let next = 40000 + ports.len() as u16;
            Some(*ports.entry(serial.to_string()).or_insert(next))
        }, |_, _| {});
        let states: Vec<(String, String)> = states.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let discovered: Vec<String> = discovered.iter().map(|s| s.to_string()).collect();
        (RouteResolver {
            adb_available: Box::new(move || adb),
            adb_device_states: Box::new(move || states.clone()),
            tunnels: Arc::new(tunnels),
            discover: Box::new(move |_| discovered.clone()),
            client: net.factory(),
            wifi_timeout: Duration::from_millis(100),
        }, ports)
    }

    const USB0: &str = "https://127.0.0.1:40000";
    const USB1: &str = "https://127.0.0.1:40001";
    const WIFI: &str = "https://192.168.1.40:7480";

    fn wifi(ip: &str) -> Option<RouteSpec> {
        Some(RouteSpec::Network { address: ip.into() })
    }

    #[test]
    fn local_only_usb_contact_does_not_enable_network_presence() {
        let hello = Hello::status(HelloStatus::Ok);
        let route = RouteSpec::Usb { serial: "SER".into() };
        let mut ping = Ping { presence: true, ..Ping::status(PingStatus::Paired) };
        assert!(ready(route.clone(), &hello, &ping).presence);
        ping.local_only = true;
        let res = ready(route, &hello, &ping);
        assert_eq!(res.status, PhoneStatus::Ready);
        assert!(!res.presence);
    }

    #[test]
    fn usb_wins_whenever_this_phone_answers_over_the_cable() {
        let net = FakeNet::new(&[(USB0, answers("ph-1", PingStatus::Paired, false)), (WIFI, answers("ph-1", PingStatus::Paired, false))]);
        let res = resolver(&net, &[("SER", "device")], true, &[]).0.resolve(&phone(), RoutePreference::Auto);
        assert_eq!((res.status, res.route, res.usb_note), (PhoneStatus::Ready, Some(RouteSpec::Usb { serial: "SER".into() }), None));
    }

    #[test]
    fn falls_back_to_wifi_and_says_why() {
        let net = FakeNet::new(&[(WIFI, answers("ph-1", PingStatus::Paired, false))]);
        for (states, adb, note) in [(vec![], true, UsbNote::NoCable), (vec![("SER", "unauthorized")], true, UsbNote::Unauthorized),
                                    (vec![("SER", "device")], true, UsbNote::AppClosed), (vec![], false, UsbNote::NoAdb)] {
            let res = resolver(&net, &states, adb, &[]).0.resolve(&phone(), RoutePreference::Auto);
            assert_eq!((res.status, res.route, res.usb_note), (PhoneStatus::Ready, wifi("192.168.1.40"), Some(note)));
        }
    }

    #[test]
    fn a_different_phone_on_usb_is_not_mistaken_for_ours() {
        let net = FakeNet::new(&[(USB0, answers("someone-else", PingStatus::Paired, false)), (WIFI, answers("ph-1", PingStatus::Paired, false))]);
        let res = resolver(&net, &[("OTHER", "device")], true, &[]).0.resolve(&phone(), RoutePreference::Auto);
        assert_eq!((res.route, res.usb_note), (wifi("192.168.1.40"), Some(UsbNote::OtherPhone)));
    }

    #[test]
    fn picks_our_phone_out_of_several_plugged_in() {
        let net = FakeNet::new(&[(USB0, answers("someone-else", PingStatus::Paired, false)), (USB1, answers("ph-1", PingStatus::Paired, false))]);
        let res = resolver(&net, &[("A", "device"), ("B", "device")], true, &[]).0.resolve(&phone(), RoutePreference::Auto);
        assert_eq!(res.route, Some(RouteSpec::Usb { serial: "B".into() }));
    }

    #[test]
    fn forced_usb_never_silently_uses_wifi() {
        let net = FakeNet::new(&[(WIFI, answers("ph-1", PingStatus::Paired, false))]);
        let res = resolver(&net, &[("SER", "unauthorized")], true, &[]).0.resolve(&phone(), RoutePreference::Usb);
        assert_eq!((res.status, res.route, res.usb_note), (PhoneStatus::UsbNeedsAttention, None, Some(UsbNote::Unauthorized)));
    }

    #[test]
    fn forced_wifi_skips_usb() {
        let net = FakeNet::new(&[(USB0, answers("ph-1", PingStatus::Paired, false)), (WIFI, answers("ph-1", PingStatus::Paired, false))]);
        let (resolver, ports) = resolver(&net, &[("SER", "device")], true, &[]);
        let res = resolver.resolve(&phone(), RoutePreference::Wifi);
        assert_eq!(res.route, wifi("192.168.1.40"));
        assert!(ports.lock().unwrap().is_empty());
    }

    #[test]
    fn discovered_address_is_tried_so_a_new_dhcp_lease_still_works() {
        let net = FakeNet::new(&[("https://192.168.1.77:7480", answers("ph-1", PingStatus::Paired, false))]);
        let res = resolver(&net, &[], true, &["192.168.1.77"]).0.resolve(&phone(), RoutePreference::Auto);
        assert_eq!(res.route, wifi("192.168.1.77"));
    }

    #[test]
    fn phone_that_forgot_this_computer_says_so() {
        let net = FakeNet::new(&[(WIFI, answers("ph-1", PingStatus::NotPaired, false))]);
        assert_eq!(resolver(&net, &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto).status, PhoneStatus::NotPaired);
    }

    #[test]
    fn local_only_phone_reachable_on_wifi_asks_for_usb() {
        let net = FakeNet::new(&[(WIFI, answers("ph-1", PingStatus::Paired, true))]);
        assert_eq!(resolver(&net, &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto).status, PhoneStatus::LocalOnly);
    }

    #[test]
    fn local_only_phone_that_refuses_hello_off_usb_still_asks_for_usb() {
        let mut net = FakeNet::new(&[(WIFI, answers("ph-1", PingStatus::Paired, true))]);
        net.usb_only_hello = vec![WIFI.into()];
        assert_eq!(resolver(&net, &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto).status, PhoneStatus::LocalOnly);
        let mut net = FakeNet::new(&[(WIFI, answers("ph-1", PingStatus::Unreachable, true))]);
        net.usb_only_hello = vec![WIFI.into()];
        assert_eq!(resolver(&net, &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto).status, PhoneStatus::Unreachable);
    }

    #[test]
    fn another_app_on_the_session_port_is_told_from_nothing_answering() {
        let net = FakeNet::new(&[(WIFI, Answer::Plain)]);
        let res = resolver(&net, &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto);
        assert_eq!((res.status, res.other_app, res.other_telescope), (PhoneStatus::Unreachable, true, false));
        assert!(!resolver(&FakeNet::default(), &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto).other_app);
    }

    #[test]
    fn another_telescope_identity_is_told_from_a_foreign_program() {
        for answer in [Answer::OtherCert, answers("someone-else", PingStatus::Paired, false)] {
            let net = FakeNet::new(&[(WIFI, answer)]);
            let res = resolver(&net, &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto);
            assert_eq!((res.status, res.other_app, res.other_telescope), (PhoneStatus::Unreachable, false, true));
        }
    }

    #[test]
    fn nothing_answering_is_unreachable() {
        assert_eq!(resolver(&FakeNet::default(), &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto).status, PhoneStatus::Unreachable);
    }

    #[test]
    fn an_older_phone_app_is_named_instead_of_unreachable() {
        let net = FakeNet::new(&[(WIFI, Answer::Phone("ph-1", PingStatus::Paired, false, SESSION_PROTOCOL - 1))]);
        let res = resolver(&net, &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto);
        assert_eq!((res.status, res.phone_version.as_str()), (PhoneStatus::PhoneOutdated, "1.0"));
        assert!(net.pinged.lock().unwrap().is_empty());
    }

    #[test]
    fn a_newer_phone_app_asks_for_a_desktop_update() {
        let net = FakeNet::new(&[(WIFI, Answer::Phone("ph-1", PingStatus::Paired, false, SESSION_PROTOCOL + 1))]);
        assert_eq!(resolver(&net, &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto).status, PhoneStatus::DesktopOutdated);
    }

    #[test]
    fn an_app_from_before_hello_counts_as_outdated_and_keeps_its_usb_serial() {
        let net = FakeNet::new(&[(USB0, Answer::OldApp)]);
        let res = resolver(&net, &[("SER", "device")], true, &[]).0.resolve(&phone(), RoutePreference::Auto);
        assert_eq!((res.status, res.route), (PhoneStatus::PhoneOutdated, Some(RouteSpec::Usb { serial: "SER".into() })));
        let net = FakeNet::new(&[(WIFI, Answer::OldApp)]);
        assert_eq!(resolver(&net, &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto).status, PhoneStatus::PhoneOutdated);
    }

    #[test]
    fn a_working_address_beats_an_outdated_answer_elsewhere() {
        let net = FakeNet::new(&[(WIFI, Answer::OldApp), ("https://192.168.1.77:7480", answers("ph-1", PingStatus::Paired, false))]);
        let res = resolver(&net, &[], true, &["192.168.1.77"]).0.resolve(&phone(), RoutePreference::Auto);
        assert_eq!((res.status, res.phone_version.as_str()), (PhoneStatus::Ready, "1.0"));
    }

    fn resolve_with_delays(net: &FakeNet, discovered: &[&str]) -> (PhoneStatus, String, Duration) {
        let mut net = net.clone();
        net.delays = [(WIFI.to_string(), Duration::from_millis(600)), ("https://192.168.1.99:7480".to_string(), Duration::from_millis(600))].into();
        let started = std::time::Instant::now();
        let res = resolver(&net, &[], true, discovered).0.resolve(&phone(), RoutePreference::Auto);
        let address = match res.route { Some(RouteSpec::Network { address }) => address, _ => String::new() };
        (res.status, address, started.elapsed())
    }

    #[test]
    fn a_dead_address_after_the_answering_one_doesnt_hold_the_check_up() {
        // The discovered address answers at once; the remembered one never does (its answer takes 600 ms)
        let net = FakeNet::new(&[("https://192.168.1.77:7480", answers("ph-1", PingStatus::Paired, false))]);
        let (status, address, took) = resolve_with_delays(&net, &["192.168.1.77"]);
        assert_eq!((status, address.as_str()), (PhoneStatus::Ready, "192.168.1.77"));
        assert!(took < Duration::from_millis(300), "waited {took:?}");
    }

    #[test]
    fn the_first_candidate_still_wins_when_a_later_one_answers_first() {
        let net = FakeNet::new(&[(WIFI, answers("ph-1", PingStatus::Paired, false)), ("https://192.168.1.77:7480", answers("ph-1", PingStatus::Paired, false))]);
        // 192.168.1.40 is slow but ranks after the discovered .77? No: discovered comes first, so make the first one the slow one
        let (status, address, took) = resolve_with_delays(&net, &["192.168.1.40", "192.168.1.77"]);
        assert_eq!((status, address.as_str()), (PhoneStatus::Ready, "192.168.1.40"));
        assert!(took >= Duration::from_millis(600), "returned before the better candidate answered: {took:?}");
    }

    #[test]
    fn nothing_answering_still_ends_when_every_address_has() {
        let net = FakeNet::new(&[]);
        let (status, _, took) = resolve_with_delays(&net, &["192.168.1.77"]);
        assert_eq!(status, PhoneStatus::Unreachable);
        assert!(took >= Duration::from_millis(600));
    }

    #[test]
    fn quitting_removes_a_forward_that_still_lingers() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let b = calls.clone();
        let tunnels = UsbTunnels::new(|_, _| Some(5000), |_, _| {}).with_linger(7480, Duration::from_secs(30))
            .with_blocking_unforward(move |s, l| b.lock().unwrap().push(format!("unfwd {s} {l}")));
        tunnels.acquire("A", 7480);
        tunnels.release("A", 7480);
        assert!(calls.lock().unwrap().is_empty());
        tunnels.shutdown();
        assert_eq!(*calls.lock().unwrap(), ["unfwd A 5000"]);
        assert_eq!(tunnels.held_port("A", 7480), None);
    }

    #[test]
    fn a_session_forward_lingers_for_the_next_check_and_goes_when_unused() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let (a, b) = (calls.clone(), calls.clone());
        let tunnels = UsbTunnels::new(move |s, _| {
            a.lock().unwrap().push(format!("fwd {s}"));
            Some(5000)
        }, move |s, l| b.lock().unwrap().push(format!("unfwd {s} {l}"))).with_linger(7480, Duration::from_millis(150));
        assert_eq!(tunnels.acquire_noting("A", 7480), Some((5000, false)));
        tunnels.release("A", 7480);
        assert_eq!(tunnels.held_port("A", 7480), None);
        assert_eq!(tunnels.acquire_noting("A", 7480), Some((5000, true)));
        tunnels.release("A", 7480);
        // The first release's expiry passes while the second one is still the latest: only the last may remove it
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(*calls.lock().unwrap(), ["fwd A"]);
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(*calls.lock().unwrap(), ["fwd A", "unfwd A 5000"]);
        // A port without a linger lets go at once
        assert_eq!(tunnels.acquire("A", 8080), Some(5000));
        tunnels.release("A", 8080);
        assert_eq!(calls.lock().unwrap().len(), 4);
    }

    /// A resolver whose forwards go to `ports` in turn (and are counted), as a session port that lingers between checks.
    fn lingering_resolver(net: &FakeNet, ports: Vec<u16>) -> (RouteResolver, Arc<Mutex<usize>>) {
        let forwards: Arc<Mutex<usize>> = Arc::default();
        let counted = forwards.clone();
        let tunnels = UsbTunnels::new(move |_, _| {
            let mut n = counted.lock().unwrap();
            *n += 1;
            ports.get(*n - 1).or(ports.last()).copied()
        }, |_, _| {}).with_linger(PING_PORT, Duration::from_secs(30));
        (RouteResolver {
            adb_available: Box::new(|| true),
            adb_device_states: Box::new(|| vec![("SER".to_string(), "device".to_string())]),
            tunnels: Arc::new(tunnels),
            discover: Box::new(|_| Vec::new()),
            client: net.factory(),
            wifi_timeout: Duration::from_millis(100),
        }, forwards)
    }

    fn closed_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    #[test]
    fn a_plugged_in_phone_whose_app_is_closed_is_not_forwarded_again_on_every_check() {
        // adb is up (something listens at the forward's end) and nobody answers behind it
        let adb = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (resolver, forwards) = lingering_resolver(&FakeNet::default(), vec![adb.local_addr().unwrap().port()]);
        for _ in 0..4 {
            let res = resolver.resolve(&phone(), RoutePreference::Usb);
            assert_eq!((res.status, res.usb_note), (PhoneStatus::UsbNeedsAttention, Some(UsbNote::AppClosed)));
        }
        assert_eq!(*forwards.lock().unwrap(), 1, "every check started adb again");
    }

    #[test]
    fn a_forward_adb_lost_is_replaced_by_the_next_check() {
        let alive = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (dead, alive_port) = (closed_port(), alive.local_addr().unwrap().port());
        let net = FakeNet::new(&[(&format!("https://127.0.0.1:{alive_port}"), answers("ph-1", PingStatus::Paired, false))]);
        let (resolver, forwards) = lingering_resolver(&net, vec![dead, alive_port]);
        assert_eq!(resolver.resolve(&phone(), RoutePreference::Usb).status, PhoneStatus::UsbNeedsAttention, "the first forward was already dead");
        let res = resolver.resolve(&phone(), RoutePreference::Usb);
        assert_eq!((res.status, res.route), (PhoneStatus::Ready, Some(RouteSpec::Usb { serial: "SER".into() })), "the kept forward was dead and wasn't replaced");
        assert_eq!(*forwards.lock().unwrap(), 2);
    }

    #[test]
    fn a_broken_or_failed_forward_is_not_kept() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let (a, b) = (calls.clone(), calls.clone());
        let tunnels = UsbTunnels::new(move |s, _| {
            a.lock().unwrap().push(format!("fwd {s}"));
            Some(5000)
        }, move |s, l| b.lock().unwrap().push(format!("unfwd {s} {l}"))).with_linger(7480, Duration::from_secs(30));
        tunnels.acquire("A", 7480);
        tunnels.release_broken("A", 7480);
        assert_eq!(*calls.lock().unwrap(), ["fwd A", "unfwd A 5000"]);
        tunnels.acquire("A", 7480);
        tunnels.release("A", 7480);
        tunnels.drop_idle("A", 7480);
        assert_eq!(calls.lock().unwrap().len(), 4);
        assert_eq!(tunnels.acquire_noting("A", 7480), Some((5000, false)));
    }

    #[test]
    fn usb_tunnels_are_shared_and_released_last() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let (a, b) = (calls.clone(), calls.clone());
        let tunnels = UsbTunnels::new(move |s, _| {
            a.lock().unwrap().push(format!("fwd {s}"));
            Some(5000)
        }, move |s, l| b.lock().unwrap().push(format!("unfwd {s} {l}")));
        assert_eq!(tunnels.acquire("A", 7480), Some(5000));
        assert_eq!(tunnels.acquire("A", 7480), Some(5000));
        tunnels.release("A", 7480);
        assert_eq!(*calls.lock().unwrap(), ["fwd A"]);
        tunnels.release("A", 7480);
        assert_eq!(*calls.lock().unwrap(), ["fwd A", "unfwd A 5000"]);
    }

    #[test]
    fn a_slow_forward_does_not_block_releasing_another() {
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (gate_rx, started_tx) = (Mutex::new(gate_rx), Mutex::new(started_tx));
        let tunnels = Arc::new(UsbTunnels::new(move |s, _| {
            if s == "SLOW" {
                started_tx.lock().unwrap().send(()).unwrap();
                gate_rx.lock().unwrap().recv_timeout(Duration::from_secs(5)).ok()?;
            }
            Some(5000)
        }, |_, _| {}));
        assert_eq!(tunnels.acquire("A", 7480), Some(5000));
        let slow = { let t = tunnels.clone(); std::thread::spawn(move || t.acquire("SLOW", 7480)) };
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let release = { let t = tunnels.clone(); std::thread::spawn(move || { t.release("A", 7480); t.held_port("A", 7480) }) };
        let released = (0..50).any(|_| { std::thread::sleep(Duration::from_millis(40)); release.is_finished() });
        gate_tx.send(()).unwrap();
        assert!(released, "release waited for the slow adb forward");
        assert_eq!(release.join().unwrap(), None);
        assert_eq!(slow.join().unwrap(), Some(5000));
    }

    #[test]
    fn phone_round_trips_and_rejects_incomplete_entries() {
        assert_eq!(phone_from_value(&phone_to_value(&phone())).unwrap(), phone());
        assert!(phone_from_value(&serde_json::json!({"id": "x", "name": "y"})).is_err());
    }

    #[test]
    fn a_phone_paired_before_tls_has_to_pair_again_and_is_never_contacted() {
        let net = FakeNet::new(&[(WIFI, answers("ph-1", PingStatus::Paired, false))]);
        let old = PairedPhone { cert_sha256: String::new(), ..phone() };
        let res = resolver(&net, &[], true, &[]).0.resolve(&old, RoutePreference::Auto);
        assert!(res.status == PhoneStatus::NotPaired && res.before_tls);
        assert!(net.pinged.lock().unwrap().is_empty() && net.auths.lock().unwrap().is_empty());
    }

    #[test]
    fn every_probe_carries_the_paired_certificate_pin() {
        let net = FakeNet::new(&[(WIFI, answers("ph-1", PingStatus::Paired, false))]);
        assert_eq!(resolver(&net, &[], true, &[]).0.resolve(&phone(), RoutePreference::Auto).status, PhoneStatus::Ready);
        let auths = net.auths.lock().unwrap();
        assert!(!auths.is_empty() && auths.iter().all(|a| a.pin == "ab".repeat(32) && a.token == "tok"));
    }

    #[test]
    fn the_pin_round_trips_through_the_saved_phone_list() {
        let mut raw = phone_to_value(&phone());
        assert_eq!(phone_from_value(&raw).unwrap(), phone());
        raw["cert_sha256"] = "garbage".into();
        assert_eq!(phone_from_value(&raw).unwrap().cert_sha256, "");
    }

    #[test]
    fn a_fake_phone_repeating_the_right_id_is_ignored_and_never_sent_the_token() {
        use crate::https::testing::*;
        let server = serve(Some("impostor"), json_handler(r#"{"protocol": 3, "phoneId": "ph-1", "phoneName": "Pixel"}"#));
        let port = server.port;
        let resolver = RouteResolver {
            adb_available: Box::new(|| false), adb_device_states: Box::new(Vec::new),
            tunnels: Arc::new(UsbTunnels::new(|_, _| None, |_, _| {})), discover: Box::new(|_| vec!["127.0.0.1".into()]),
            client: Arc::new(move |base, auth| Box::new(SessionClient::new(&base.replace(":7480", &format!(":{port}")), auth))),
            wifi_timeout: Duration::from_secs(1),
        };
        let phone = PairedPhone { ips: vec![], cert_sha256: pin_of("phone"), ..phone() };
        assert_eq!(resolver.resolve(&phone, RoutePreference::Auto).status, PhoneStatus::Unreachable);
        assert!(server.seen.lock().unwrap().iter().all(|s| s.auth.is_none()));
    }
}
