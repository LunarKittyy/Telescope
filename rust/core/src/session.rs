//! The phone's session port (8766: hello, ping, session start/stop) and stream port (8080: state, control), plus
//! picking the route to it the way phones.py does: USB when this phone answers over the cable, else Wi-Fi.

use crate::config::Phone;
use crate::discovery::LanDiscovery;
use crate::https::{Error, PhoneAuth};
use crate::adb;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const SESSION_PORT: u16 = 8766;
pub const STREAM_PORT: u16 = 8080;
pub const SESSION_PROTOCOL: i64 = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const START_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Debug, PartialEq)]
pub enum RouteKind {
    Usb { serial: String },
    Wifi,
}

/// How this computer reaches one phone right now: an address with the session and stream ports open on it.
#[derive(Clone, Debug)]
pub struct Route {
    pub kind: RouteKind,
    pub auth: PhoneAuth,
    pub session_port: u16,
    pub stream_port: u16,
}

#[derive(Debug, Default)]
pub struct Ping {
    pub streaming: bool,
    pub busy: bool,
    pub local_only: bool,
    pub phone_id: String,
    pub phone_name: String,
    pub streaming_for: Option<String>,
}

impl Route {
    pub fn describe(&self) -> String {
        match &self.kind {
            RouteKind::Usb { serial } => format!("USB ({serial})"),
            RouteKind::Wifi => format!("Wi-Fi ({})", self.auth.host),
        }
    }

    pub fn hello(&self) -> Result<Value, Error> {
        self.auth.json(self.session_port, "GET", "/v1/hello", None, REQUEST_TIMEOUT)
    }

    pub fn ping(&self) -> Result<Ping, Error> {
        let body = self.auth.json(self.session_port, "GET", "/v1/ping", None, REQUEST_TIMEOUT)?;
        let s = |k: &str| body.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let b = |k: &str| body.get(k).and_then(Value::as_bool).unwrap_or(false);
        Ok(Ping {
            streaming: b("streaming"),
            busy: b("busy"),
            local_only: b("localOnly"),
            phone_id: s("phoneId"),
            phone_name: crate::util::clean_name(&s("phoneName")),
            streaming_for: body.get("streamingFor").and_then(Value::as_str).map(crate::util::clean_name),
        })
    }

    fn session(&self, body: Value) -> Result<(), String> {
        match self.auth.json(self.session_port, "POST", "/v1/session", Some(&body), REQUEST_TIMEOUT) {
            Ok(v) if v.get("ok").and_then(Value::as_bool) == Some(true) => Ok(()),
            Ok(v) => Err(v.get("error").and_then(Value::as_str).unwrap_or("refused").to_string()),
            Err(Error::Http(401)) => Err("not_paired".into()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Starts the phone's camera at this size and rate and waits until it streams (the service answers before the
    /// capture session is up, so this polls /v1/ping like session_client does).
    pub fn start(&self, width: u32, height: u32, fps: u32) -> Result<(), String> {
        self.session(json!({"action": "start", "width": width, "height": height, "fps": fps}))?;
        let started = Instant::now();
        while started.elapsed() < START_TIMEOUT {
            if let Ok(p) = self.ping() {
                if let Some(other) = p.streaming_for {
                    return Err(format!("The phone is streaming to {other}"));
                }
                if p.streaming {
                    return Ok(());
                }
            }
            let fast = started.elapsed() < Duration::from_secs(2);
            std::thread::sleep(Duration::from_millis(if fast { 100 } else { 500 }));
        }
        Err("The camera didn't start in time".into())
    }

    pub fn stop(&self) -> Result<(), String> {
        self.session(json!({"action": "stop"}))
    }

    pub fn state(&self) -> Result<Value, Error> {
        self.auth.json(self.stream_port, "GET", "/v1/state", None, Duration::from_secs(4))
    }

    /// One /v1/control action, e.g. control(json!({"action": "torch", "value": 1})).
    pub fn control(&self, body: &Value) -> Result<(), Error> {
        self.auth.request(self.stream_port, "POST", "/v1/control", Some(body), REQUEST_TIMEOUT).map(|_| ())
    }

    pub fn release(&self) {
        if let RouteKind::Usb { serial } = &self.kind {
            adb::unforward(serial, self.session_port);
            adb::unforward(serial, self.stream_port);
        }
    }
}

/// What resolve() found when it found no route.
#[derive(Debug)]
pub enum Unreachable {
    NotPaired,
    LocalOnly,
    NoAnswer,
    Outdated(String),
}

fn check_hello(route: &Route, phone: &Phone) -> Option<Result<(), Unreachable>> {
    let hello = route.hello().ok()?;
    if hello.get("phoneId").and_then(Value::as_str) != Some(phone.id.as_str()) {
        return None;
    }
    let protocol = hello.get("protocol").and_then(Value::as_i64).unwrap_or(0);
    if protocol != SESSION_PROTOCOL {
        let side = if protocol < SESSION_PROTOCOL { "phone app" } else { "desktop" };
        return Some(Err(Unreachable::Outdated(format!("Update the {side} (session protocol {protocol})"))));
    }
    Some(Ok(()))
}

/// USB first when this phone answers over a cable, else its LAN addresses (mDNS, then the stored ones).
pub fn resolve(phone: &Phone, lan: Option<&LanDiscovery>, allow_usb: bool, allow_wifi: bool) -> Result<Route, Unreachable> {
    let auth = |host: &str| PhoneAuth { host: host.to_string(), token: phone.token.clone(), pin: phone.cert_sha256.clone() };
    if allow_usb {
        for serial in adb::devices() {
            let Some(session_port) = adb::forward(&serial, SESSION_PORT) else { continue };
            let Some(stream_port) = adb::forward(&serial, STREAM_PORT) else {
                adb::unforward(&serial, session_port);
                continue;
            };
            let route = Route { kind: RouteKind::Usb { serial: serial.clone() }, auth: auth("127.0.0.1"), session_port, stream_port };
            match check_hello(&route, phone) {
                Some(Ok(())) => match route.ping() {
                    Ok(_) => return Ok(route),
                    Err(Error::Http(401)) => {
                        route.release();
                        return Err(Unreachable::NotPaired);
                    }
                    Err(_) => route.release(),
                },
                Some(Err(e)) => {
                    route.release();
                    return Err(e);
                }
                None => route.release(),
            }
        }
    }
    if !allow_wifi {
        return Err(Unreachable::NoAnswer);
    }
    let mut hosts: Vec<String> = lan.map(|l| l.lookup(&phone.id)).unwrap_or_default();
    hosts.extend(phone.active_ip.iter().cloned());
    hosts.extend(phone.ips.iter().cloned());
    hosts.dedup();
    let mut local_only = false;
    for host in hosts {
        let route = Route { kind: RouteKind::Wifi, auth: auth(&host), session_port: SESSION_PORT, stream_port: STREAM_PORT };
        match route.ping() {
            Ok(p) if p.phone_id.is_empty() || p.phone_id == phone.id => return Ok(route),
            Ok(_) => {}
            Err(Error::Http(401)) => return Err(Unreachable::NotPaired),
            Err(Error::Http(403)) => local_only = true,
            Err(_) => {}
        }
    }
    Err(if local_only { Unreachable::LocalOnly } else { Unreachable::NoAnswer })
}
