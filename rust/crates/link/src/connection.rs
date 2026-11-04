//! What the Connection plugin decides, without its window (plugins/connection.py): the saved phones and route choice,
//! which checks to run and which answers count, the routes each stream holds (and their adb forwards), hand-overs and
//! recovery, waking the phone's camera, and the texts that explain a phone that can't stream.
//!
//! The plugin owns the card, dialogs and bus; it calls in here and carries out what comes back. Clocks come in as
//! arguments so tests can move them.

use telescope_core::i18n::Fields;
use crate::https::PhoneAuth;
use crate::phones::{auth_of, is_usb, phone_from_value, phone_to_value, route_host, usb_note_text, UsbTunnels, STREAM_PORT};
use crate::session_client::{Ping, PingStatus, SessionClient, SessionResult, PING_PORT, START_FAST_POLL_FOR,
                            START_FAST_POLL_INTERVAL, START_POLL_INTERVAL, START_TIMEOUT};
use crate::util::clean_name;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use telescope_core::i18n::tr;
use telescope_core::link::{PhoneStatus, Resolution, RoutePreference, UsbNote};
use telescope_core::source::{PairedPhone, RouteSpec};

/// Idle re-check of the selected phone, only while the window is on screen.
pub const STATUS_POLL: Duration = Duration::from_secs(3);
/// An idle check that never answers stops blocking new ones.
pub const CHECK_GIVE_UP_S: f64 = 60.0;
/// Tray, after starting once the phone was ready: only its going away matters.
pub const HELD_POLL_S: f64 = 60.0;
/// While streaming over Wi-Fi: has the phone been plugged in?
pub const USB_WATCH: Duration = Duration::from_secs(5);
/// USB that just failed a stream isn't tried again for this long, so it can't flap.
pub const USB_SIT_OUT_S: f64 = 10.0;
/// Add phone dialog: look for a phone on USB to pair with.
pub const PAIR_USB_POLL: Duration = Duration::from_secs(2);
/// A phone's addresses kept for Wi-Fi probes, newest last.
pub const MAX_REMEMBERED_IPS: usize = 8;
/// Unreachable pings tolerated while the camera starts; startup can briefly starve the phone's HTTP server.
const UNREACHABLE_STREAK_LIMIT: u32 = 3;
/// How long a wake right after a stop waits for the phone to finish stopping.
const STOP_SETTLE_S: f64 = 3.0;

/// The adb forwards streams hold; UsbTunnels for real.
pub trait Tunnels: Send + Sync {
    fn acquire(&self, serial: &str, remote_port: u16) -> Option<u16>;
    fn release(&self, serial: &str, remote_port: u16);
}

impl Tunnels for UsbTunnels {
    fn acquire(&self, serial: &str, remote_port: u16) -> Option<u16> {
        UsbTunnels::acquire(self, serial, remote_port)
    }

    fn release(&self, serial: &str, remote_port: u16) {
        UsbTunnels::release(self, serial, remote_port);
    }
}

pub use telescope_core::link::StreamFormat;

/// The StreamFormat from host.plugin_config("stream_output"); h264_available says whether this computer decodes H.264.
pub fn stream_format(cfg: Option<&Value>, h264_available: bool) -> StreamFormat {
    let cfg = cfg.and_then(Value::as_object);
    let h264 = cfg.and_then(|c| c.get("format")).and_then(Value::as_str) == Some("h264") && h264_available;
    let dynamic = cfg.and_then(|c| c.get("bitrate_mbps")).is_some_and(|b| b.as_f64() == Some(-1.0));
    StreamFormat { h264, dynamic }
}

pub const VIDEO_MJPEG: &str = "/v1/video";
pub const VIDEO_H264: &str = "/v1/video.h264";
pub const VIDEO_UDP: &str = "/v1/video.udp";

/// What a Connect via choice comes to.
#[derive(Clone, Debug, PartialEq)]
pub enum RouteChange {
    /// Nothing changes.
    Nothing,
    /// Back to the one in use before the check answered.
    Cancelled,
    /// Check the phone answers the new way first (the stream that works now keeps going meanwhile).
    Probe { probe_id: u64, phone: PairedPhone, preference: RoutePreference },
    /// Taken: reconnect a running stream, or check the phone again.
    Use(UseRoute),
}

#[derive(Clone, Debug, PartialEq)]
pub enum UseRoute {
    Reconnect,
    Check(Option<Check>),
}

/// A check to run off the window's thread, then hand to on_resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub check_id: u64,
    pub phone: PairedPhone,
    pub preference: RoutePreference,
}

/// What a probe for a new Connect via came to.
#[derive(Clone, Debug, PartialEq)]
pub enum ProbeOutcome {
    Stale,
    Use(UseRoute),
    /// It can't be reached that way: keep streaming as now and say so (a "route" banner with Try again).
    Refuse { title: String, text: String, preference: RoutePreference },
}

/// What a USB check during a Wi-Fi stream came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsbWatch {
    Ignore,
    /// Not on USB (any more): hide Switch to USB.
    HideButton,
    /// Hand the stream over to this route; if the host can't, show Switch to USB.
    HandOver(RouteSpec),
}

/// The banner buttons for a phone Start couldn't reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    AddPhone,
    UpdateDesktop,
    UpdateOverUsb,
    SwitchToAutomatic,
    TryAgain,
}

impl Fix {
    pub fn label(self) -> String {
        match self {
            Fix::AddPhone => tr("Add phone"),
            Fix::UpdateDesktop => tr("Update this app"),
            Fix::UpdateOverUsb => tr("Update over USB"),
            Fix::SwitchToAutomatic => tr("Switch to Automatic"),
            Fix::TryAgain => tr("Try again"),
        }
    }
}

/// What pairing a phone did to the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paired {
    /// It was in the list already: its token and pin are new.
    pub existing: bool,
    /// Its stream runs on the old token and needs a reconnect.
    pub reconnect: bool,
}

pub struct Connection {
    pub computer_id: String,
    pub computer_name: String,
    pub route_pref: RoutePreference,
    /// Experimental H.264 over UDP on Wi-Fi.
    pub udp: bool,
    pub phones: Vec<PairedPhone>,
    pub selected_id: Option<String>,
    pub resolution: Option<Resolution>,
    pub check_id: u64,
    /// The idle check still resolving; the poll waits for it.
    pub running_check: Option<u64>,
    pub check_started: f64,
    /// Phones that run Dynamic over UDP, from their last check.
    udp_dynamic: HashSet<String>,
    /// The route each streaming phone's video comes over, and the USB forward it holds (None over Wi-Fi), by id.
    holds: HashMap<String, (Option<RouteSpec>, Option<String>)>,
    /// The video path each stream last opened, which says its format.
    paths: HashMap<String, String>,
    /// (route, forward serial, path) a hand-over is trying.
    prepared: HashMap<String, (RouteSpec, Option<String>, String)>,
    /// When USB last failed a stream; it then sits out USB_SIT_OUT_S.
    pub usb_sat_out: f64,
    pub streaming: bool,
    /// The first frame arrived.
    pub connected: bool,
    /// Frames stopped; the host is reconnecting.
    pub lost: bool,
    pub transport_udp: bool,
    /// A Connect via choice checked before the stream moves to it.
    pub pending_route: Option<RoutePreference>,
    pub route_probe_id: u64,
    pub watch_id: u64,
    /// Watching for a cable while streaming over Wi-Fi.
    pub usb_watch_active: bool,
    pub switch_usb_visible: bool,
    // Each idle check wakes the phone, so a phone left waiting isn't polled while nobody looks
    pub shown: bool,
    pub watched: bool,
    pub wanted: bool,
    /// It already started for this arrival: slow checks will do.
    pub wanted_held: bool,
    tunnels: std::sync::Arc<dyn Tunnels>,
}

fn key(pid: Option<&str>) -> String {
    pid.unwrap_or("").to_string()
}

impl Connection {
    pub fn new(computer_id: String, computer_name: String, tunnels: std::sync::Arc<dyn Tunnels>) -> Connection {
        Connection {
            computer_id, computer_name, route_pref: RoutePreference::Auto, udp: false, phones: Vec::new(), selected_id: None,
            resolution: None, check_id: 0, running_check: None, check_started: 0.0, udp_dynamic: HashSet::new(), holds: HashMap::new(),
            paths: HashMap::new(), prepared: HashMap::new(), usb_sat_out: -1e9, streaming: false, connected: false, lost: false,
            transport_udp: false, pending_route: None, route_probe_id: 0, watch_id: 0, usb_watch_active: false,
            switch_usb_visible: false, shown: true, watched: false, wanted: false, wanted_held: false, tunnels,
        }
    }

    pub fn phone(&self, pid: Option<&str>) -> Option<&PairedPhone> {
        self.phones.iter().find(|p| Some(p.id.as_str()) == pid)
    }

    pub fn selected_phone(&self) -> Option<&PairedPhone> {
        self.phone(self.selected_id.as_deref())
    }

    fn pid<'a>(&'a self, pid: Option<&'a str>) -> Option<&'a str> {
        pid.or(self.selected_id.as_deref())
    }

    /// The route the selected phone streams over.
    pub fn stream_route(&self) -> Option<RouteSpec> {
        self.holds.get(&key(self.selected_id.as_deref())).and_then(|(r, _)| r.clone())
    }

    /// Sets the selected phone's stream route without a forward (the plugin's tests set it this way).
    pub fn set_stream_route(&mut self, route: Option<RouteSpec>) {
        let k = key(self.selected_id.as_deref());
        let forward = self.holds.get(&k).and_then(|(_, f)| f.clone());
        self.holds.insert(k, (route, forward));
    }

    /// The route held for pid's stream and its forward, if it holds one.
    pub fn hold(&self, pid: &str) -> Option<&(Option<RouteSpec>, Option<String>)> {
        self.holds.get(pid)
    }

    pub fn set_hold(&mut self, pid: &str, route: Option<RouteSpec>, forward: Option<String>) {
        self.holds.insert(pid.to_string(), (route, forward));
    }

    pub fn set_path(&mut self, pid: &str, path: &str) {
        self.paths.insert(pid.to_string(), path.to_string());
    }

    pub fn set_computer_name(&mut self, name: &str) {
        let cleaned = clean_name(name);
        if !cleaned.is_empty() {
            self.computer_name = cleaned;
        }
        self.hide_names();
    }

    /// Tells the log which names are the person's own, so a copied report shows "phone 1" and "computer 1" instead.
    fn hide_names(&self) {
        let phones: Vec<String> = self.phones.iter().map(|p| p.name.clone()).collect();
        telescope_core::redact::set_hidden("phone", &phones);
        telescope_core::redact::set_hidden("computer", std::slice::from_ref(&self.computer_name));
    }

    // ── Settings ──────────────────────────────────────────────────────────

    pub fn get_config(&self) -> Value {
        json!({
            "computer_id": self.computer_id,
            "computer_name": self.computer_name,
            "route": self.route_pref.as_str(),
            "udp": self.udp,
            "phones": self.phones.iter().map(phone_to_value).collect::<Vec<_>>(),
            "selected_phone": self.selected_id,
        })
    }

    /// Takes a saved section; source_known says whether a picked id is one of the plugins' sources (a browser).
    /// Malformed phone entries are dropped.
    pub fn set_config(&mut self, cfg: &Value, source_known: impl Fn(&str) -> bool) {
        let get = |k: &str| cfg.get(k);
        if let Some(cid) = get("computer_id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            self.computer_id = cid.to_string();
        }
        if let Some(name) = get("computer_name").and_then(Value::as_str).map(clean_name).filter(|n| !n.is_empty()) {
            self.computer_name = name;
        }
        self.route_pref = get("route").and_then(Value::as_str).and_then(RoutePreference::parse).unwrap_or_default();
        self.udp = get("udp") == Some(&Value::Bool(true));
        self.phones = get("phones").and_then(Value::as_array).into_iter().flatten().filter_map(|raw| match phone_from_value(raw) {
            Ok(p) => Some(p),
            Err(_) => {
                log::warn!("Discarding malformed phone entry in config: {raw}");
                None
            }
        }).collect();
        let sel = get("selected_phone").and_then(Value::as_str);
        let known = sel.is_some_and(|s| self.phone(Some(s)).is_some() || source_known(s));
        self.selected_id = if known { sel.map(str::to_string) } else { self.phones.first().map(|p| p.id.clone()) };
        self.resolution = None;
        self.hide_names();
    }

    // ── Checks ────────────────────────────────────────────────────────────

    pub fn polling(&self) -> bool {
        self.shown || self.watched || self.wanted
    }

    pub fn polling_fast(&self) -> bool {
        self.shown || self.watched || (self.wanted && !self.wanted_held)
    }

    /// The check to run now, if any (_check_status).
    pub fn check_status(&mut self, now: f64) -> Option<Check> {
        if self.streaming {
            return None;
        }
        let phone = self.selected_phone()?.clone();
        if self.running_check == Some(self.check_id) && now - self.check_started < CHECK_GIVE_UP_S {
            return None; // a slow check would only be made stale by a new one, and then nothing ever lands
        }
        self.check_id += 1;
        self.running_check = Some(self.check_id);
        self.check_started = now;
        Some(Check { check_id: self.check_id, phone, preference: self.route_pref })
    }

    /// The status timer's tick.
    pub fn poll_status(&mut self, now: f64) -> Option<Check> {
        if self.polling_fast() || (self.polling() && now - self.check_started >= HELD_POLL_S) {
            return self.check_status(now);
        }
        None
    }

    /// One of the reasons to keep checking changed ("shown", "watched" or "wanted"); a check to run straight away
    /// when that started the checks.
    pub fn set_poll_reason(&mut self, reason: &str, on: bool, now: f64) -> Option<Check> {
        let was = self.polling();
        match reason {
            "shown" => self.shown = on,
            "watched" => self.watched = on,
            _ => self.wanted = on,
        }
        if self.polling() && !was { self.check_status(now) } else { None }
    }

    /// phone_wanted on the bus.
    pub fn phone_wanted(&mut self, wanted: bool, held: bool, now: f64) -> Option<Check> {
        self.wanted_held = held;
        self.set_poll_reason("wanted", wanted, now)
    }

    /// A check's answer. Some((phone id, ready)) when phone_ready should go out on the bus.
    pub fn on_resolved(&mut self, check_id: u64, phone_id: &str, res: Resolution) -> (bool, Option<(String, bool)>) {
        if Some(check_id) == self.running_check {
            self.running_check = None;
        }
        if check_id != self.check_id || Some(phone_id) != self.selected_id.as_deref() {
            return (false, None); // stale: a newer check, or another phone picked meanwhile
        }
        let ready = res.status == PhoneStatus::Ready;
        let save = self.apply_resolution(res);
        // Only from the idle check, never from Start's own resolve, which would start it again
        (save, (!self.streaming).then(|| (phone_id.to_string(), ready)))
    }

    /// Takes a check's answer for the selected phone; true when the phone list changed and should be saved.
    pub fn apply_resolution(&mut self, res: Resolution) -> bool {
        if res.status == PhoneStatus::Ready {
            if let Some(sel) = self.selected_id.clone() {
                if res.udp_dynamic { self.udp_dynamic.insert(sel); } else { self.udp_dynamic.remove(&sel); }
            }
        }
        let mut save = false;
        if let (Some(RouteSpec::Network { address }), Some(sel)) = (&res.route, self.selected_id.clone()) {
            if let Some(phone) = self.phones.iter_mut().find(|p| p.id == sel) {
                if phone.active_ip.as_deref() != Some(address) {
                    // Remember the address that answered (after DHCP moved it, say)
                    phone.active_ip = Some(address.clone());
                    if !phone.ips.contains(address) {
                        // Every check probes each one, so addresses from networks long gone drop off the front
                        phone.ips.push(address.clone());
                        let excess = phone.ips.len().saturating_sub(MAX_REMEMBERED_IPS);
                        phone.ips.drain(..excess);
                    }
                    save = true;
                }
            }
        }
        self.resolution = Some(res);
        save
    }

    /// Start's own check answered (get_stream_info): anything in flight is older.
    pub fn took_start_check(&mut self, pid: Option<&str>, res: Resolution) -> bool {
        if pid.is_none() || pid == self.selected_id.as_deref() {
            self.check_id += 1;
            return self.apply_resolution(res);
        }
        false
    }

    // ── Connect via ───────────────────────────────────────────────────────

    /// streaming_from_selected: the host streams from the selected phone now.
    pub fn set_route_preference(&mut self, preference: RoutePreference, streaming_from_selected: bool, now: f64) -> RouteChange {
        if preference == self.route_pref {
            if self.pending_route.take().is_some() {
                self.route_probe_id += 1;
                return RouteChange::Cancelled;
            }
            return RouteChange::Nothing;
        }
        if self.streaming && streaming_from_selected {
            if let Some(phone) = self.selected_phone().cloned() {
                self.pending_route = Some(preference);
                self.route_probe_id += 1;
                return RouteChange::Probe { probe_id: self.route_probe_id, phone, preference };
            }
        }
        RouteChange::Use(self.use_route_preference(preference, now))
    }

    /// Takes the choice: clear the "route" banner, save, then reconnect or check.
    pub fn use_route_preference(&mut self, preference: RoutePreference, now: f64) -> UseRoute {
        self.pending_route = None;
        self.route_pref = preference;
        self.resolution = None;
        if self.streaming {
            return UseRoute::Reconnect;
        }
        self.running_check = None; // a check for the old route mustn't hold this one back, then land as current
        UseRoute::Check(self.check_status(now))
    }

    pub fn on_route_probed(&mut self, probe_id: u64, pid: &str, preference: RoutePreference, res: &Resolution,
                           streaming_from: bool, now: f64) -> ProbeOutcome {
        if probe_id != self.route_probe_id || Some(preference) != self.pending_route || Some(pid) != self.selected_id.as_deref() {
            return ProbeOutcome::Stale; // another choice or another phone since
        }
        self.pending_route = None;
        let Some(phone) = self.phone(Some(pid)).cloned() else { return ProbeOutcome::Use(self.use_route_preference(preference, now)) };
        if res.status == PhoneStatus::Ready || !self.streaming || !streaming_from {
            return ProbeOutcome::Use(self.use_route_preference(preference, now));
        }
        let name = &phone.name;
        let title = match preference {
            RoutePreference::Usb => tr("Can't reach {phone} over USB").field("phone", name),
            RoutePreference::Wifi => tr("Can't reach {phone} over Wi-Fi").field("phone", name),
            RoutePreference::Auto => tr("Can't reach {phone}").field("phone", name),
        };
        let now_on = if self.stream_route().is_some_and(|r| is_usb(&r)) { "USB" } else { "Wi-Fi" };
        let kept = route_choice_label(self.route_pref);
        let hint = match preference {
            RoutePreference::Usb => format!(" {}", tr("Check the cable and that Telescope is open on the phone.")),
            RoutePreference::Wifi => format!(" {}", tr("Check that it's on the same network as this computer.")),
            RoutePreference::Auto => String::new(),
        };
        let text = tr("Still streaming over {now}, so Connect via stays on {kept}.").field("now", now_on).field("kept", &kept) + &hint;
        ProbeOutcome::Refuse { title, text, preference }
    }

    // ── Routes streams hold ───────────────────────────────────────────────

    /// The path a stream opens: the format's, and over UDP where it applies.
    pub fn video_path(format: StreamFormat) -> &'static str {
        if format.h264 { VIDEO_H264 } else { VIDEO_MJPEG }
    }

    /// The H.264 path over UDP where it applies: Wi-Fi and switched on. Dynamic only on a phone that runs it from this
    /// desktop's reports; an older one measures its TCP queue, so Dynamic stays on TCP there.
    pub fn path_for(&self, route: &RouteSpec, path: &str, pid: Option<&str>, format: StreamFormat) -> String {
        let dynamic_ok = !format.dynamic || pid.is_some_and(|p| self.udp_dynamic.contains(p));
        if path == VIDEO_H264 && !is_usb(route) && self.udp && dynamic_ok {
            return VIDEO_UDP.to_string();
        }
        path.to_string()
    }

    fn url_on(&self, route: &RouteSpec, path: &str, pid: Option<&str>, format: StreamFormat) -> Option<(String, Option<String>)> {
        match route {
            RouteSpec::Usb { serial } => {
                // Always a fresh forward: adb drops them when the cable goes, even if it comes back
                let local = self.tunnels.acquire(serial, STREAM_PORT)?;
                Some((format!("https://127.0.0.1:{local}{path}"), Some(serial.clone())))
            }
            RouteSpec::Network { address } => Some((format!("https://{address}:{STREAM_PORT}{}", self.path_for(route, path, pid, format)), None)),
        }
    }

    /// Makes route pid's stream's, letting go of the previous one's forward. The video URL, or None when adb couldn't
    /// open a forward.
    pub fn hold_stream_route(&mut self, route: &RouteSpec, pid: Option<&str>, format: StreamFormat) -> Option<String> {
        let pid = self.pid(pid).map(str::to_string);
        // The panels' format is the picked phone's; one streaming behind them keeps the one it opened
        let path = if pid == self.selected_id { Connection::video_path(format).to_string() }
                   else { self.paths.get(&key(pid.as_deref())).cloned().unwrap_or_else(|| Connection::video_path(format).to_string()) };
        self.release_stream(pid.as_deref(), true);
        let (url, forward) = self.url_on(route, &path, pid.as_deref(), format)?;
        let k = key(pid.as_deref());
        self.holds.insert(k.clone(), (Some(route.clone()), forward));
        self.paths.insert(k, path);
        Some(url)
    }

    /// pid's stream is over (or moving onto another route): let go of its route and forward.
    pub fn release_stream(&mut self, pid: Option<&str>, moving: bool) {
        let k = key(self.pid(pid));
        if let Some((_, Some(serial))) = self.holds.remove(&k) {
            self.tunnels.release(&serial, STREAM_PORT);
        }
        if !moving {
            self.drop_stream_route(Some(&k), false, 0.0); // a hand-over still under way when it moves can still finish
        }
    }

    /// Lets go of the forward pid's stream holds, keeping its route held (recovery, ahead of a fresh one).
    pub fn let_go_of_forward(&mut self, pid: Option<&str>) {
        let k = key(self.pid(pid));
        if let Some((_, forward)) = self.holds.get_mut(&k) {
            if let Some(serial) = forward.take() {
                self.tunnels.release(&serial, STREAM_PORT);
            }
        }
    }

    pub fn tunnels(&self) -> std::sync::Arc<dyn Tunnels> {
        self.tunnels.clone()
    }

    /// The video URL for pid's stream on route while it still runs on its old one (a hand-over).
    pub fn prepare_stream_route(&mut self, route: &RouteSpec, pid: &str, format: StreamFormat) -> Option<String> {
        let held = self.holds.get(pid)?;
        if self.prepared.contains_key(pid) {
            return None;
        }
        if held.0.as_ref() == Some(route) {
            return None; // already there
        }
        let path = self.paths.get(pid).cloned().unwrap_or_else(|| Connection::video_path(format).to_string());
        let (url, forward) = self.url_on(route, &path, Some(pid), format)?;
        self.prepared.insert(pid.to_string(), (route.clone(), forward, path));
        Some(url)
    }

    /// The hand-over worked: the prepared route is pid's stream's now, and the old one's forward goes.
    pub fn commit_stream_route(&mut self, pid: &str) {
        let Some((route, forward, path)) = self.prepared.remove(pid) else { return };
        if let Some((_, Some(old))) = self.holds.get(pid) {
            self.tunnels.release(old, STREAM_PORT);
        }
        self.holds.insert(pid.to_string(), (Some(route), forward));
        self.paths.insert(pid.to_string(), path);
    }

    /// The hand-over didn't happen: let go of the route it prepared. failed: USB sits out a while, and Switch to USB
    /// shows for the shown stream.
    pub fn drop_stream_route(&mut self, pid: Option<&str>, failed: bool, now: f64) {
        let k = key(pid);
        let Some((route, forward, _)) = self.prepared.remove(&k) else { return };
        if let Some(serial) = forward {
            self.tunnels.release(&serial, STREAM_PORT);
        }
        if failed && is_usb(&route) {
            self.usb_sat_out = now;
            if Some(k.as_str()) == self.selected_id.as_deref() && self.streaming {
                self.switch_usb_visible = true; // reconnecting over it by hand still might work
            }
        }
    }

    /// Moves pid's running stream onto route (a recovery probe's answer). The new video URL, or None.
    pub fn adopt_stream_route(&mut self, route: &RouteSpec, pid: Option<&str>, format: StreamFormat) -> Option<String> {
        let k = key(self.pid(pid));
        if !self.holds.contains_key(&k) {
            return None;
        }
        let url = self.hold_stream_route(route, Some(&k), format);
        if url.is_none() && !self.holds.contains_key(&k) {
            self.holds.insert(k, (None, None)); // the forward failed: keep its place so the next probe tries again
        }
        url
    }

    /// The UDP switch or the stream's format changed: whether a running Wi-Fi stream has to reconnect.
    pub fn transport_needs_reconnect(&self, format: StreamFormat) -> bool {
        let Some(route) = self.stream_route() else { return false };
        if !self.streaming || is_usb(&route) || Connection::video_path(format) != VIDEO_H264 {
            return false;
        }
        let wants_udp = self.path_for(&route, VIDEO_H264, self.selected_id.as_deref(), format) == VIDEO_UDP;
        wants_udp != self.transport_udp
    }

    /// (phone, route) to reach pid's session port: its stream's route, else the last check's.
    pub fn session_target(&self, pid: Option<&str>) -> (Option<PairedPhone>, Option<RouteSpec>) {
        let selected = pid.is_none() || pid == self.selected_id.as_deref();
        let phone = self.phone(self.pid(pid)).cloned();
        let mut route = self.holds.get(&key(self.pid(pid))).and_then(|(r, _)| r.clone());
        if route.is_none() && selected {
            route = self.resolution.as_ref().and_then(|r| r.route.clone());
        }
        (phone, route)
    }

    // ── Stream lifecycle ──────────────────────────────────────────────────

    pub fn on_stream_start(&mut self, stream_url: &str) {
        self.transport_udp = stream_url.ends_with(".udp");
        if !self.streaming {
            self.connected = false; // also called after an auto-reconnect; that isn't a fresh start
        }
        self.streaming = true;
        self.lost = false;
        self.switch_usb_visible = false;
        self.watch_id += 1; // a USB check still running is about the route before this one
        self.usb_watch_active = self.stream_route().is_some_and(|r| !is_usb(&r)) && self.route_pref == RoutePreference::Auto;
    }

    /// The stream stopped; still_streaming says which phones the host still streams from. A check to run after.
    pub fn on_stream_stop(&mut self, still_streaming: impl Fn(&str) -> bool, now: f64) -> Option<Check> {
        self.streaming = false;
        self.usb_watch_active = false;
        self.watch_id += 1;
        self.switch_usb_visible = false;
        let gone: Vec<String> = self.holds.keys().filter(|pid| !still_streaming(pid)).cloned().collect();
        for pid in gone {
            self.release_stream(Some(&pid), false);
        }
        self.lost = false;
        self.check_status(now)
    }

    pub fn on_stream_connected(&mut self, shows: bool) {
        if shows {
            self.connected = true;
            self.lost = false;
        }
    }

    pub fn on_stream_lost(&mut self, shows: bool, now: f64) {
        if !shows {
            return;
        }
        self.lost = true;
        if self.stream_route().is_some_and(|r| is_usb(&r)) {
            self.usb_sat_out = now; // so Wi-Fi recovery doesn't hand straight back to a flaky cable
        }
        // Recovery picks USB by itself when the phone answers on it, so no Switch to USB meanwhile
        self.usb_watch_active = false;
        self.watch_id += 1;
        self.switch_usb_visible = false;
    }

    /// The USB watch's tick: (watch id, phone) to check over USB only.
    pub fn watch_usb(&mut self) -> Option<(u64, PairedPhone)> {
        let phone = self.selected_phone()?.clone();
        if !self.streaming {
            return None;
        }
        self.watch_id += 1;
        Some((self.watch_id, phone))
    }

    /// The USB check's answer: the USB route the phone answers on, or None.
    pub fn on_usb_available(&mut self, watch_id: u64, route: Option<RouteSpec>, now: f64) -> UsbWatch {
        if watch_id != self.watch_id || !self.streaming {
            return UsbWatch::Ignore;
        }
        let Some(route) = route else {
            self.switch_usb_visible = false;
            return UsbWatch::HideButton;
        };
        if self.switch_usb_visible || now - self.usb_sat_out < USB_SIT_OUT_S {
            return UsbWatch::Ignore; // it just failed: the button's there to try by hand
        }
        // Opens next to the Wi-Fi stream and moves once it's healthy; Wi-Fi only closes then
        UsbWatch::HandOver(route)
    }

    // ── Phones ────────────────────────────────────────────────────────────

    /// A phone paired. streaming_from says whether the host streams from a phone id now.
    pub fn on_phone_paired(&mut self, phone: PairedPhone, streaming_from: impl Fn(&str) -> bool) -> Paired {
        let reconnect = streaming_from(&phone.id) && self.phone(Some(&phone.id)).is_some();
        if let Some(existing) = self.phones.iter_mut().find(|p| p.id == phone.id) {
            // Pairing again replaced this computer's token on the phone
            existing.token = phone.token;
            existing.cert_sha256 = phone.cert_sha256;
            existing.ips = phone.ips;
            existing.active_ip = phone.active_ip;
            return Paired { existing: true, reconnect };
        }
        self.phones.push(phone);
        self.hide_names();
        Paired { existing: false, reconnect }
    }

    /// A listed phone with phone's name that no longer answers as itself: maybe the same phone after Telescope was
    /// reinstalled or its data cleared, which gives it a new id. check resolves one over Automatic.
    pub fn earlier_install(&self, phone: &PairedPhone, streaming_from: impl Fn(&str) -> bool,
                           check: impl Fn(&PairedPhone) -> Option<PhoneStatus>) -> Option<PairedPhone> {
        for old in &self.phones {
            if old.id == phone.id || old.name != phone.name || streaming_from(&old.id) {
                continue;
            }
            let status = match &self.resolution {
                Some(res) if Some(old.id.as_str()) == self.selected_id.as_deref() => Some(res.status),
                _ => check(old),
            };
            if matches!(status, Some(PhoneStatus::NotPaired | PhoneStatus::Unreachable | PhoneStatus::UsbNeedsAttention)) {
                return Some(old.clone());
            }
        }
        None
    }

    /// phone is old again with a new id: it takes old's place (the host moves the settings).
    pub fn take_over(&mut self, old_id: &str, phone: PairedPhone) {
        if let Some(slot) = self.phones.iter_mut().find(|p| p.id == old_id) {
            *slot = phone;
        }
        self.hide_names();
        self.release_stream(Some(old_id), false);
        self.paths.remove(old_id);
    }

    pub fn rename_phone(&mut self, pid: &str, name: &str) -> bool {
        let Some(phone) = self.phones.iter_mut().find(|p| p.id == pid) else { return false };
        let cleaned = clean_name(name);
        if !cleaned.is_empty() {
            phone.name = cleaned;
        }
        self.hide_names();
        true
    }

    /// Drops pid from the list; the phone to unpair on the phone too, and the id to pick instead when it was picked.
    pub fn forget_phone(&mut self, pid: &str) -> Option<(PairedPhone, Option<Option<String>>)> {
        let phone = self.phone(Some(pid))?.clone();
        self.phones.retain(|p| p.id != pid);
        self.hide_names();
        let reselect = (Some(pid) == self.selected_id.as_deref()).then(|| self.phones.first().map(|p| p.id.clone()));
        Some((phone, reselect))
    }

    /// pid is picked now (by hand, or the host moved the panels there).
    pub fn mark_selected(&mut self, pid: Option<&str>) {
        self.selected_id = pid.map(str::to_string);
        self.pending_route = None; // a Connect via check under way was about the phone before
        self.route_probe_id += 1;
        self.resolution = None;
        self.running_check = None; // the other phone's check is stale now; don't wait for it
    }

    pub fn diagnostics(&self, source_name: Option<&str>) -> Value {
        let res = self.resolution.as_ref();
        let route = if self.streaming { self.stream_route() } else { res.and_then(|r| r.route.clone()) };
        let mut out = Map::new();
        out.insert("Paired phones".into(), json!(self.phones.len().to_string()));
        out.insert("Streaming from".into(), json!(match source_name {
            None => "a phone".to_string(),
            Some(_) => self.selected_id.clone().unwrap_or_default(),
        }));
        out.insert("Connect via".into(), json!(self.route_pref.as_str()));
        out.insert("Connection".into(), json!(match route {
            Some(RouteSpec::Usb { .. }) => "usb",
            Some(RouteSpec::Network { .. }) => "wifi",
            None => "none",
        }));
        out.insert("Phone status".into(), json!(res.map(|r| status_name(r.status)).unwrap_or("not checked")));
        if let Some(note) = res.and_then(|r| r.usb_note) {
            out.insert("USB".into(), serde_json::to_value(note).unwrap_or_default());
        }
        if let Some(v) = res.map(|r| r.phone_version.as_str()).filter(|v| !v.is_empty()) {
            out.insert("Phone app".into(), json!(v));
        }
        Value::Object(out)
    }
}

/// phones.py's name for a status.
pub fn status_name(status: PhoneStatus) -> &'static str {
    match status {
        PhoneStatus::Ready => "ready",
        PhoneStatus::Unreachable => "unreachable",
        PhoneStatus::NotPaired => "not_paired",
        PhoneStatus::LocalOnly => "local_only",
        PhoneStatus::UsbNeedsAttention => "usb_attention",
        PhoneStatus::PhoneOutdated => "phone_outdated",
        PhoneStatus::DesktopOutdated => "desktop_outdated",
    }
}

/// The Connect via choices as the card names them.
pub fn route_choice_label(pref: RoutePreference) -> String {
    match pref {
        RoutePreference::Auto => tr("Automatic"),
        RoutePreference::Usb => tr("USB only"),
        RoutePreference::Wifi => tr("Wi-Fi only"),
    }
}

/// The name this computer goes by until renamed.
pub fn default_computer_name() -> String {
    let host = crate::util::hostname();
    Some(clean_name(host.split('.').next().unwrap_or(""))).filter(|n| !n.is_empty()).unwrap_or_else(|| tr("Computer"))
}

// ── Texts ─────────────────────────────────────────────────────────────────

fn with_phone(text: String, phone: &str) -> String {
    text.field("phone", phone)
}

/// (status kind, short text) for the card's Phone row.
pub fn status_line(res: Option<&Resolution>) -> (&'static str, String) {
    let Some(res) = res else { return ("status_dim", tr("Checking…")) };
    match res.status {
        PhoneStatus::Ready => ("status_ok", tr("Ready")),
        PhoneStatus::Unreachable => ("status_warn", format!("○ {}", tr("Can't reach the phone"))),
        PhoneStatus::NotPaired => ("status_err", format!("○ {}", tr("Needs pairing again"))),
        PhoneStatus::LocalOnly => ("status_warn", format!("○ {}", tr("Phone accepts USB only"))),
        PhoneStatus::UsbNeedsAttention => ("status_warn", format!("○ {}", tr("USB not available"))),
        PhoneStatus::PhoneOutdated => ("status_warn", format!("○ {}", tr("Phone app needs an update"))),
        PhoneStatus::DesktopOutdated => ("status_warn", format!("○ {}", tr("This app needs an update"))),
    }
}

/// What's wrong and what to do about it, for the card note and Start failures.
pub fn problem_text(res: &Resolution, phone: &str, adb_available: bool) -> String {
    match res.status {
        PhoneStatus::Unreachable => with_phone(tr("Open Telescope on {phone} and keep it on screen. It needs to be on the same network as this computer, or plugged in with a USB cable."), phone),
        // LEGACY MIGRATION PATH
        PhoneStatus::NotPaired if res.before_tls => with_phone(tr("This version of Telescope encrypts the connection to {phone}, which needs a new pairing. Click Add phone and scan the code again."), phone),
        PhoneStatus::NotPaired => with_phone(tr("{phone} doesn't recognise this computer anymore (it was removed on the phone, or the app was reinstalled). Click Add phone to pair it again."), phone),
        PhoneStatus::LocalOnly => {
            let mut text = with_phone(tr("Local only is on in the phone app, so {phone} only accepts USB. Plug it in with a cable, or turn Local only off on the phone."), phone);
            if crate::platform::windows() && !adb_available {
                text += &format!(" {}", tr("USB needs adb, which Add phone can get."));
            }
            text
        }
        PhoneStatus::UsbNeedsAttention => {
            let why = Some(usb_note_text(res.usb_note)).filter(|w| !w.is_empty()).unwrap_or_else(|| tr("the phone isn't answering over USB"));
            tr("The connection is set to USB only: {why}. Switch to Automatic to use Wi-Fi instead.").field("why", &why)
        }
        PhoneStatus::PhoneOutdated => with_phone(tr("The Telescope app on {phone} is older than this one. Update it on the phone."), phone),
        PhoneStatus::DesktopOutdated => with_phone(tr("The Telescope app on {phone} is newer than this one. Update Telescope on this computer."), phone),
        PhoneStatus::Ready => String::new(),
    }
}

/// The card's note for a phone that's fine but isn't on a cable it's plugged in with.
pub fn not_using_usb(res: &Resolution) -> Option<String> {
    let wifi = res.route.as_ref().is_some_and(|r| !is_usb(r));
    match res.usb_note {
        Some(note) if wifi && !matches!(note, UsbNote::NoCable | UsbNote::NoAdb) => {
            Some(tr("Not using USB: {reason}.").field("reason", usb_note_text(Some(note))))
        }
        _ => None,
    }
}

pub fn route_text(route: Option<&RouteSpec>) -> String {
    match route {
        None => "-".into(),
        Some(RouteSpec::Usb { .. }) => tr("USB cable"),
        Some(RouteSpec::Network { address }) => format!("Wi-Fi · {address}"),
    }
}

/// Why Start didn't take the phone: another paired computer's stream runs on it.
pub fn busy_other_reason(computer: Option<&str>) -> String {
    let text = match computer {
        Some(c) => tr("The phone is streaming to {computer}.").field("computer", c),
        None => tr("The phone is streaming to another computer."),
    };
    format!("{text}\n\n{}", tr("Stop it there or on the phone, then try again."))
}

pub fn start_refused_reason(error: Option<&str>, computer: Option<&str>) -> String {
    match error.unwrap_or("") {
        "busy_other" => busy_other_reason(computer),
        "no_camera_permission" => tr("The phone hasn't given Telescope camera access.\n\nOpen the app on the phone and allow the camera permission."),
        "busy" => tr("The phone is busy starting or stopping a stream.\n\nTry again in a moment."),
        "start_refused" => tr("Android didn't let the camera start in the background.\n\nBring Telescope to the front on the phone and try again."),
        "not_paired" => tr("The phone doesn't recognise this computer anymore.\n\nClick Add phone to pair it again."),
        other => {
            let error = if other.is_empty() { tr("unknown error") } else { other.to_string() };
            tr("The phone refused to start streaming ({error}).").field("error", &error)
        }
    }
}

/// The button an app-version mismatch gets, if there's something to press.
pub fn outdated_action(res: Option<&Resolution>, apk_bundled: bool, adb_available: bool) -> Option<Fix> {
    let res = res?;
    match res.status {
        PhoneStatus::DesktopOutdated => Some(Fix::UpdateDesktop),
        PhoneStatus::PhoneOutdated if res.route.as_ref().is_some_and(is_usb) && apk_bundled && adb_available => Some(Fix::UpdateOverUsb),
        _ => None,
    }
}

/// The banner buttons for a phone Start couldn't reach (or a dropped stream that won't come back).
pub fn fix_actions(res: &Resolution, preference: RoutePreference, apk_bundled: bool, adb_available: bool) -> Vec<Fix> {
    if res.status == PhoneStatus::NotPaired {
        return vec![Fix::AddPhone];
    }
    if let Some(update) = outdated_action(Some(res), apk_bundled, adb_available) {
        return vec![update];
    }
    let mut actions = Vec::new();
    if res.status == PhoneStatus::UsbNeedsAttention || (preference != RoutePreference::Auto && res.status == PhoneStatus::Unreachable) {
        actions.push(Fix::SwitchToAutomatic);
    }
    if matches!(res.status, PhoneStatus::Unreachable | PhoneStatus::LocalOnly | PhoneStatus::UsbNeedsAttention) {
        actions.push(Fix::TryAgain);
    }
    actions
}

// ── Waking the phone ──────────────────────────────────────────────────────

/// The session-port calls a wake makes, so tests can stand in for the phone.
pub trait SessionCtl {
    fn ping(&self) -> Ping;
    fn start(&self, opening: Option<&Map<String, Value>>) -> SessionResult;
    fn stop(&self) -> SessionResult;
}

impl SessionCtl for SessionClient {
    fn ping(&self) -> Ping {
        SessionClient::ping(self)
    }

    fn start(&self, opening: Option<&Map<String, Value>>) -> SessionResult {
        SessionClient::start(self, opening)
    }

    fn stop(&self) -> SessionResult {
        SessionClient::stop(self)
    }
}

/// Sleeping and the clock, so tests can run a wake without waiting.
pub trait Pace {
    fn sleep(&self, d: Duration);
    fn now(&self) -> f64;
}

/// The real clock.
pub struct RealPace;

impl Pace for RealPace {
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }

    fn now(&self) -> f64 {
        telescope_core::time::monotonic_ns() as f64 / 1e9
    }
}

/// A client for phone's session port along route, holding a USB forward for as long as f runs (session_channel).
pub fn with_session<R>(auth: Option<&PhoneAuth>, route: Option<&RouteSpec>, tunnels: &dyn Tunnels,
                       f: impl FnOnce(Option<&SessionClient>) -> R) -> R {
    let (Some(auth), Some(route)) = (auth, route) else { return f(None) };
    match route {
        RouteSpec::Network { address } => f(Some(&SessionClient::new(&format!("https://{address}:{PING_PORT}"), auth.clone()))),
        RouteSpec::Usb { serial } => {
            let Some(local) = tunnels.acquire(serial, PING_PORT) else { return f(None) };
            struct Release<'a>(&'a dyn Tunnels, &'a str);
            impl Drop for Release<'_> {
                fn drop(&mut self) {
                    self.0.release(self.1, PING_PORT);
                }
            }
            let _release = Release(tunnels, serial);
            f(Some(&SessionClient::new(&format!("https://127.0.0.1:{local}"), auth.clone())))
        }
    }
}

/// Starts the phone's camera if it isn't running (at opening's size and rate) and waits until it streams. Blocking.
pub fn ensure_streaming(client: Option<&dyn SessionCtl>, opening: Option<&Map<String, Value>>, after_stop: bool,
                        progress: &dyn Fn(&str), pace: &dyn Pace) -> Result<(), String> {
    let Some(client) = client else { return Err(tr("Lost the connection to the phone. Try again.")) };
    let mut ping = client.ping();
    if after_stop {
        ping = await_stopped(client, ping, pace);
    }
    match ping.status {
        PingStatus::NotPaired => return Err(tr("The phone doesn't recognise this computer anymore.\n\nClick Add phone to pair it again.")),
        PingStatus::Unreachable => return Err(tr("Couldn't reach the phone. Open Telescope on it and try again.")),
        PingStatus::Paired => {}
    }
    if let Some(other) = &ping.streaming_for {
        return Err(busy_other_reason(Some(other)));
    }
    if ping.streaming {
        return Ok(());
    }
    if !ping.busy {
        progress(&tr("Starting the phone's camera..."));
        let result = client.start(opening);
        if !result.ok {
            return Err(start_refused_reason(result.error.as_deref(), result.computer.as_deref()));
        }
    }
    await_streaming(client, progress, pace)
}

/// The phone answers a stop before its stream has ended: the ping once it has, or once it's taking too long.
fn await_stopped(client: &dyn SessionCtl, mut ping: Ping, pace: &dyn Pace) -> Ping {
    let deadline = pace.now() + STOP_SETTLE_S;
    while (ping.streaming || ping.busy) && ping.streaming_for.is_none() && pace.now() < deadline {
        pace.sleep(START_FAST_POLL_INTERVAL);
        ping = client.ping();
    }
    ping
}

/// Polls until the phone reports a live stream (the start request returns before the camera is up).
fn await_streaming(client: &dyn SessionCtl, progress: &dyn Fn(&str), pace: &dyn Pace) -> Result<(), String> {
    let wait_start = pace.now();
    let deadline = wait_start + START_TIMEOUT.as_secs_f64();
    let mut started = false;
    let mut unreachable_streak = 0;
    let mut elapsed = 0.0;
    while pace.now() < deadline {
        let fast = elapsed < START_FAST_POLL_FOR.as_secs_f64() && unreachable_streak == 0; // a quiet phone gets the slow pace
        pace.sleep(if fast { START_FAST_POLL_INTERVAL } else { START_POLL_INTERVAL });
        let ping = client.ping();
        elapsed = pace.now() - wait_start;
        if ping.streaming {
            return Ok(());
        }
        if ping.status == PingStatus::NotPaired {
            return Err(tr("Lost contact with the phone while its camera was starting."));
        }
        if let Some(other) = &ping.streaming_for {
            return Err(busy_other_reason(Some(other))); // another computer's start got there first
        }
        let seconds = format!("{elapsed:.0}");
        if ping.status != PingStatus::Paired {
            unreachable_streak += 1; // tolerate brief blips; bail on sustained failure
            if unreachable_streak >= UNREACHABLE_STREAK_LIMIT {
                return Err(tr("Lost contact with the phone while its camera was starting."));
            }
            progress(&tr("Phone went quiet for a moment, still waiting... ({seconds}s)").field("seconds", &seconds));
            continue;
        }
        unreachable_streak = 0;
        if ping.busy {
            started = true;
        } else if started {
            return Err(tr("The phone's camera stopped before it finished starting.\n\nCheck the phone for a permission prompt or an error."));
        }
        let text = if started { tr("Phone's camera is opening... ({seconds}s)") } else { tr("Waiting for the phone's camera... ({seconds}s)") };
        progress(&text.field("seconds", &seconds));
    }
    Err(tr("The phone's camera didn't finish starting in time.\n\nTry again. If it keeps happening, open Telescope on the phone."))
}

/// The opening body for a StreamRequest.
pub fn opening_of(width: u32, height: u32, fps: u32) -> Map<String, Value> {
    let mut m = Map::new();
    // No size picked (Stream output's Auto): the fps alone, as StreamOutputPlugin.opening() sends it
    if width > 0 && height > 0 {
        m.insert("width".into(), json!(width));
        m.insert("height".into(), json!(height));
    }
    m.insert("fps".into(), json!(fps));
    m
}

/// Phone auth for a phone, if there is one.
pub fn auth_for(phone: Option<&PairedPhone>) -> Option<PhoneAuth> {
    phone.map(auth_of)
}

/// The host a route reaches the phone at.
pub fn host_of(route: &RouteSpec) -> String {
    route_host(route).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::{Arc, Mutex};

    const PIN: &str = "abababababababababababababababababababababababababababababababab";

    fn wifi() -> RouteSpec {
        RouteSpec::Network { address: "10.0.0.5".into() }
    }

    fn usb() -> RouteSpec {
        RouteSpec::Usb { serial: "serial-1".into() }
    }

    fn ready(route: RouteSpec) -> Resolution {
        Resolution { route: Some(route), ..Resolution::new(PhoneStatus::Ready) }
    }

    #[derive(Default)]
    struct FakeTunnels {
        local: Mutex<Option<u16>>,
        held: Mutex<HashMap<(String, u16), u32>>,
        acquires: Mutex<u32>,
    }

    impl FakeTunnels {
        fn new() -> Arc<FakeTunnels> {
            Arc::new(FakeTunnels { local: Mutex::new(Some(41000)), ..Default::default() })
        }

        fn held(&self) -> Vec<((String, u16), u32)> {
            self.held.lock().unwrap().iter().map(|(k, v)| (k.clone(), *v)).collect()
        }
    }

    impl Tunnels for FakeTunnels {
        fn acquire(&self, serial: &str, port: u16) -> Option<u16> {
            *self.acquires.lock().unwrap() += 1;
            let local = (*self.local.lock().unwrap())?;
            *self.held.lock().unwrap().entry((serial.into(), port)).or_insert(0) += 1;
            Some(local)
        }

        fn release(&self, serial: &str, port: u16) {
            let mut held = self.held.lock().unwrap();
            let k = (serial.to_string(), port);
            let n = held.get_mut(&k).unwrap();
            *n -= 1;
            if *n == 0 {
                held.remove(&k);
            }
        }
    }

    fn model() -> (Connection, Arc<FakeTunnels>) {
        let tunnels = FakeTunnels::new();
        (Connection::new("cid".into(), "desk".into(), tunnels.clone()), tunnels)
    }

    fn phone(pid: &str, name: &str, ips: &[&str]) -> PairedPhone {
        PairedPhone { id: pid.into(), name: name.into(), token: "tok".into(), ips: ips.iter().map(|s| s.to_string()).collect(),
                      active_ip: ips.first().map(|s| s.to_string()), cert_sha256: PIN.into() }
    }

    /// Pairs and picks a phone, as _add() in the Python tests does.
    fn add(c: &mut Connection, pid: &str) {
        c.on_phone_paired(phone(pid, "Pixel", &["10.0.0.5"]), |_| false);
        c.mark_selected(Some(pid));
    }

    fn h264() -> StreamFormat {
        StreamFormat { h264: true, dynamic: false }
    }

    #[test]
    fn status_line_covers_every_state() {
        assert_eq!(status_line(None), ("status_dim", "Checking…".to_string()));
        assert_eq!(status_line(Some(&ready(wifi()))).0, "status_ok");
        for s in [PhoneStatus::Unreachable, PhoneStatus::LocalOnly, PhoneStatus::UsbNeedsAttention, PhoneStatus::PhoneOutdated, PhoneStatus::DesktopOutdated] {
            assert_eq!(status_line(Some(&Resolution::new(s))).0, "status_warn");
        }
        assert_eq!(status_line(Some(&Resolution::new(PhoneStatus::NotPaired))).0, "status_err");
    }

    #[test]
    fn route_text_names_the_live_route() {
        assert_eq!(route_text(Some(&usb())), "USB cable");
        assert_eq!(route_text(Some(&wifi())), "Wi-Fi · 10.0.0.5");
        assert_eq!(route_text(None), "-");
    }

    #[test]
    fn problem_text_says_what_to_do() {
        let text = |s: PhoneStatus| problem_text(&Resolution::new(s), "Pixel", true);
        assert!(text(PhoneStatus::Unreachable).contains("Open Telescope on Pixel"));
        assert!(text(PhoneStatus::NotPaired).contains("Add phone"));
        assert!(text(PhoneStatus::LocalOnly).contains("Local only"));
        let forced = problem_text(&Resolution { usb_note: Some(UsbNote::AppClosed), ..Resolution::new(PhoneStatus::UsbNeedsAttention) }, "Pixel", true);
        assert!(forced.contains("isn't open") && forced.contains("Automatic"));
        assert!(text(PhoneStatus::PhoneOutdated).contains("Update it on the phone"));
        assert!(text(PhoneStatus::DesktopOutdated).contains("on this computer"));
        assert_eq!(problem_text(&ready(wifi()), "Pixel", true), "");
    }

    #[test]
    fn local_only_on_windows_without_adb_points_at_add_phone() {
        let _lock = crate::platform::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let without = problem_text(&Resolution::new(PhoneStatus::LocalOnly), "Pixel", false);
        assert_eq!(without.contains("Add phone can get"), cfg!(windows));
        assert!(!problem_text(&Resolution::new(PhoneStatus::LocalOnly), "Pixel", true).contains("adb"));
        crate::platform::set_windows(Some(true));
        assert!(problem_text(&Resolution::new(PhoneStatus::LocalOnly), "Pixel", false).contains("Add phone can get"));
        assert!(crate::phones::usb_note_text(Some(UsbNote::NoAdb)).contains("Add phone can get it"));
        crate::platform::set_windows(None);
    }

    #[test]
    fn config_round_trips_and_keeps_the_computer_identity() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.set_route_preference(RoutePreference::Wifi, false, 0.0);
        let cfg = c.get_config();
        let (mut other, _) = model();
        other.computer_id = "other".into();
        other.set_config(&cfg, |_| false);
        assert_eq!(other.get_config(), cfg);
        assert_eq!((cfg["computer_id"].as_str(), cfg["route"].as_str(), cfg["phones"][0]["id"].as_str()), (Some("cid"), Some("wifi"), Some("id-a")));
    }

    #[test]
    fn set_config_discards_malformed_phones_and_bad_values() {
        let (mut c, _) = model();
        c.set_config(&json!({"route": "carrier-pigeon", "phones": [{"id": "id-a", "name": "A", "token": "t"}, {"name": "no id"}, "junk"],
                             "selected_phone": "gone", "computer_name": "   "}), |_| false);
        assert_eq!(c.phones.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["id-a"]);
        assert_eq!(c.selected_id.as_deref(), Some("id-a"));
        assert_eq!(c.get_config()["route"], "auto");
        assert_eq!(c.get_config()["computer_name"], "desk");
    }

    #[test]
    fn a_picked_browser_is_kept_when_its_plugin_offers_it() {
        let (mut c, _) = model();
        c.set_config(&json!({"phones": [], "selected_phone": "browser:x"}), |s| s == "browser:x");
        assert_eq!(c.selected_id.as_deref(), Some("browser:x"));
    }

    #[test]
    fn pairing_again_replaces_the_token_instead_of_adding_a_duplicate() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        let again = PairedPhone { token: "new".into(), ..phone("id-a", "Pixel", &["10.0.0.9"]) };
        assert_eq!(c.on_phone_paired(again, |_| true), Paired { existing: true, reconnect: true });
        assert_eq!(c.phones.len(), 1);
        assert_eq!((c.phones[0].token.as_str(), c.phones[0].ips.as_slice()), ("new", &["10.0.0.9".to_string()][..]));
    }

    #[test]
    fn resolution_remembers_the_address_that_answered() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.check_id = 7;
        let (save, ready_now) = c.on_resolved(7, "id-a", Resolution { usb_note: Some(UsbNote::NoCable), ..ready(RouteSpec::Network { address: "10.0.0.77".into() }) });
        assert!(save && ready_now == Some(("id-a".into(), true)));
        assert_eq!(c.phones[0].active_ip.as_deref(), Some("10.0.0.77"));
        assert!(c.phones[0].ips.contains(&"10.0.0.77".to_string()));
        assert_eq!(not_using_usb(c.resolution.as_ref().unwrap()), None); // no cable, nothing to explain
    }

    #[test]
    fn addresses_from_old_networks_drop_off() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        for n in 1..13 {
            c.apply_resolution(ready(RouteSpec::Network { address: format!("10.0.1.{n}") }));
        }
        let ips = &c.phones[0].ips;
        assert_eq!(ips.len(), 8);
        assert!(ips.last().unwrap() == "10.0.1.12" && !ips.contains(&"10.0.0.5".to_string()));
    }

    #[test]
    fn a_cable_that_is_not_used_gets_explained() {
        let res = Resolution { usb_note: Some(UsbNote::AppClosed), ..ready(wifi()) };
        assert_eq!(not_using_usb(&res).unwrap(), "Not using USB: phone plugged in, but Telescope isn't open on it.");
    }

    #[test]
    fn stale_resolutions_are_ignored() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.check_id = 3;
        c.on_resolved(2, "id-a", ready(wifi()));
        c.on_resolved(3, "someone-else", ready(wifi()));
        assert!(c.resolution.is_none());
    }

    #[test]
    fn a_check_slower_than_the_poll_still_lands() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        let first = c.check_status(0.0).unwrap().check_id;
        assert!(c.check_status(1.0).is_none() && c.check_status(2.0).is_none());
        c.on_resolved(first, "id-a", ready(wifi()));
        assert_eq!(c.resolution.as_ref().unwrap().status, PhoneStatus::Ready);
        assert_eq!(c.check_status(3.0).unwrap().check_id, first + 1);
    }

    #[test]
    fn changing_the_route_while_a_check_runs_checks_the_new_route_and_ignores_the_old() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        let old = c.check_status(0.0).unwrap().check_id;
        let change = c.set_route_preference(RoutePreference::Usb, false, 1.0);
        assert!(matches!(change, RouteChange::Use(UseRoute::Check(Some(Check { check_id, preference: RoutePreference::Usb, .. }))) if check_id == old + 1));
        c.on_resolved(old, "id-a", ready(wifi()));
        assert!(c.resolution.is_none());
    }

    fn settle(c: &mut Connection) {
        let id = c.check_id;
        c.on_resolved(id, "id-a", ready(wifi()));
    }

    fn settled() -> Connection {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.check_status(0.0);
        settle(&mut c);
        c
    }

    #[test]
    fn the_poll_stops_while_the_window_is_hidden() {
        let mut c = settled();
        c.set_poll_reason("shown", false, 1.0);
        assert!(c.poll_status(2.0).is_none());
    }

    #[test]
    fn showing_the_window_checks_straight_away_and_polls_again() {
        let mut c = settled();
        c.set_poll_reason("shown", false, 1.0);
        let first = c.check_id;
        assert_eq!(c.set_poll_reason("shown", true, 2.0).unwrap().check_id, first + 1);
        settle(&mut c);
        assert_eq!(c.poll_status(3.0).unwrap().check_id, first + 2);
    }

    #[test]
    fn an_app_reading_the_camera_keeps_checking_the_phone_while_hidden() {
        let mut c = settled();
        c.set_poll_reason("shown", false, 1.0);
        let first = c.check_id;
        assert_eq!(c.set_poll_reason("watched", true, 2.0).unwrap().check_id, first + 1);
        settle(&mut c);
        assert_eq!(c.poll_status(3.0).unwrap().check_id, first + 2);
        settle(&mut c);
        c.set_poll_reason("watched", false, 4.0);
        assert!(c.poll_status(5.0).is_none());
    }

    #[test]
    fn waiting_to_start_when_ready_keeps_checking_the_phone_while_hidden() {
        let mut c = settled();
        c.set_poll_reason("shown", false, 1.0);
        let first = c.check_id;
        assert_eq!(c.phone_wanted(true, false, 2.0).unwrap().check_id, first + 1);
        settle(&mut c);
        c.phone_wanted(false, false, 3.0);
        assert!(c.poll_status(4.0).is_none());
    }

    #[test]
    fn once_started_for_the_ready_phone_the_tray_checks_it_slowly() {
        let mut c = settled();
        c.set_poll_reason("shown", false, 1.0);
        c.phone_wanted(true, true, 1.0);
        let first = c.check_id;
        assert!(c.poll_status(2.0).is_none());
        let later = c.check_started + HELD_POLL_S;
        assert_eq!(c.poll_status(later).unwrap().check_id, first + 1);
        settle(&mut c);
        assert!(c.set_poll_reason("shown", true, later + 1.0).is_none()); // on screen it's every tick again
        assert_eq!(c.poll_status(later + 2.0).unwrap().check_id, first + 2);
        settle(&mut c);
        c.set_poll_reason("shown", false, later + 3.0);
        c.phone_wanted(true, false, later + 3.0); // the phone left: its coming back is seen quickly
        assert_eq!(c.poll_status(later + 4.0).unwrap().check_id, first + 3);
    }

    #[test]
    fn an_app_reading_the_camera_keeps_quick_checks_while_ready_is_held() {
        let mut c = settled();
        c.set_poll_reason("shown", false, 1.0);
        c.phone_wanted(true, true, 1.0);
        c.set_poll_reason("watched", true, 1.0);
        let first = c.check_id;
        c.on_resolved(first, "id-a", ready(wifi()));
        assert_eq!(c.poll_status(2.0).unwrap().check_id, first + 1);
    }

    #[test]
    fn a_check_that_never_answers_gives_way_to_a_new_one() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        let first = c.check_status(0.0).unwrap().check_id;
        assert_eq!(c.check_status(61.0).unwrap().check_id, first + 1);
    }

    #[test]
    fn idle_checks_tell_whether_the_phone_is_ready() {
        let mut c = settled();
        let id = c.check_status(10.0).unwrap().check_id;
        assert_eq!(c.on_resolved(id, "id-a", Resolution::new(PhoneStatus::Unreachable)).1, Some(("id-a".into(), false)));
        c.streaming = true;
        let id = { c.check_id += 1; c.check_id };
        assert_eq!(c.on_resolved(id, "id-a", ready(wifi())).1, None);
    }

    #[test]
    fn a_connect_via_the_phone_cant_be_reached_on_keeps_the_stream() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.streaming = true;
        c.set_stream_route(Some(usb()));
        let RouteChange::Probe { probe_id, phone, preference } = c.set_route_preference(RoutePreference::Wifi, true, 0.0) else { panic!() };
        assert_eq!(c.route_pref, RoutePreference::Auto);
        let outcome = c.on_route_probed(probe_id, &phone.id, preference, &Resolution::new(PhoneStatus::Unreachable), true, 1.0);
        let ProbeOutcome::Refuse { title, text, .. } = outcome else { panic!("{outcome:?}") };
        assert_eq!(title, "Can't reach Pixel over Wi-Fi");
        assert!(text.contains("Still streaming over USB, so Connect via stays on Automatic."));
        assert_eq!(c.route_pref, RoutePreference::Auto);
        // Try again, on the same Wi-Fi now
        let RouteChange::Probe { probe_id, phone, preference } = c.set_route_preference(RoutePreference::Wifi, true, 2.0) else { panic!() };
        assert_eq!(c.on_route_probed(probe_id, &phone.id, preference, &ready(wifi()), true, 3.0), ProbeOutcome::Use(UseRoute::Reconnect));
        assert_eq!(c.route_pref, RoutePreference::Wifi);
    }

    #[test]
    fn picking_the_old_connect_via_back_cancels_the_check() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.streaming = true;
        let RouteChange::Probe { probe_id, phone, preference } = c.set_route_preference(RoutePreference::Usb, true, 0.0) else { panic!() };
        assert_eq!(c.set_route_preference(RoutePreference::Auto, true, 1.0), RouteChange::Cancelled);
        assert_eq!(c.on_route_probed(probe_id, &phone.id, preference, &ready(usb()), true, 2.0), ProbeOutcome::Stale);
        assert_eq!(c.route_pref, RoutePreference::Auto);
    }

    #[test]
    fn a_connect_via_check_for_one_phone_leaves_the_next_one_shown_alone() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.on_phone_paired(phone("id-b", "Other", &[]), |_| false);
        c.streaming = true;
        let RouteChange::Probe { probe_id, preference, .. } = c.set_route_preference(RoutePreference::Usb, true, 0.0) else { panic!() };
        c.mark_selected(Some("id-b"));
        assert_eq!(c.on_route_probed(probe_id, "id-a", preference, &ready(usb()), true, 1.0), ProbeOutcome::Stale);
    }

    #[test]
    fn changing_the_route_saves_and_reconnects_a_live_stream() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.streaming = true;
        assert_eq!(c.set_route_preference(RoutePreference::Usb, false, 0.0), RouteChange::Use(UseRoute::Reconnect));
        assert_eq!(c.get_config()["route"], "usb");
    }

    #[test]
    fn a_new_connect_via_is_checked_before_the_stream_moves() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.streaming = true;
        let RouteChange::Probe { probe_id: old_id, .. } = c.set_route_preference(RoutePreference::Usb, true, 0.0) else { panic!() };
        // What was picked shows meanwhile, but nothing moves yet
        assert_eq!(c.pending_route, Some(RoutePreference::Usb));
        let RouteChange::Probe { probe_id: new_id, .. } = c.set_route_preference(RoutePreference::Wifi, true, 0.0) else { panic!() };
        assert_eq!(c.on_route_probed(old_id, "id-a", RoutePreference::Usb, &ready(usb()), true, 0.0), ProbeOutcome::Stale);
        assert_eq!(c.get_config()["route"], "auto");
        assert_eq!(c.on_route_probed(new_id, "id-a", RoutePreference::Wifi, &ready(wifi()), true, 0.0),
                   ProbeOutcome::Use(UseRoute::Reconnect));
        assert_eq!(c.get_config()["route"], "wifi");
    }

    #[test]
    fn a_usb_check_that_finishes_after_the_move_doesnt_start_another() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.set_stream_route(Some(wifi()));
        c.on_stream_start("url");
        let (stale, _) = c.watch_usb().unwrap();
        // The hand-over moved it meanwhile
        c.set_hold("id-a", Some(usb()), Some("serial-1".into()));
        c.on_stream_start("url");
        assert_eq!(c.on_usb_available(stale, Some(usb()), 100.0), UsbWatch::Ignore);
        // It's on that one already
        assert_eq!(c.prepare_stream_route(&usb(), "id-a", h264()), None);
    }

    #[test]
    fn start_failures_offer_the_matching_fix() {
        assert_eq!(fix_actions(&Resolution::new(PhoneStatus::NotPaired), RoutePreference::Auto, false, true), [Fix::AddPhone]);
        assert_eq!(fix_actions(&Resolution::new(PhoneStatus::UsbNeedsAttention), RoutePreference::Usb, false, true),
                   [Fix::SwitchToAutomatic, Fix::TryAgain]);
        assert_eq!(fix_actions(&Resolution::new(PhoneStatus::DesktopOutdated), RoutePreference::Auto, false, true), [Fix::UpdateDesktop]);
        assert_eq!(fix_actions(&Resolution::new(PhoneStatus::Unreachable), RoutePreference::Auto, false, true), [Fix::TryAgain]);
        assert_eq!(Fix::SwitchToAutomatic.label(), "Switch to Automatic");
    }

    #[test]
    fn an_outdated_phone_on_usb_can_be_updated_from_here() {
        let outdated = |route| Resolution { route: Some(route), ..Resolution::new(PhoneStatus::PhoneOutdated) };
        assert_eq!(outdated_action(Some(&outdated(wifi())), true, true), None); // over Wi-Fi the phone updates itself
        assert_eq!(outdated_action(Some(&outdated(usb())), true, true), Some(Fix::UpdateOverUsb));
        assert_eq!(outdated_action(Some(&outdated(usb())), false, true), None);
        assert_eq!(outdated_action(Some(&Resolution::new(PhoneStatus::DesktopOutdated)), false, false), Some(Fix::UpdateDesktop));
    }

    #[test]
    fn start_over_wifi_uses_the_resolved_address() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.took_start_check(None, ready(wifi()));
        assert_eq!(c.hold_stream_route(&wifi(), None, StreamFormat::default()).as_deref(), Some("https://10.0.0.5:8080/v1/video"));
        assert_eq!(c.session_target(None), (Some(phone("id-a", "Pixel", &["10.0.0.5"])), Some(wifi())));
    }

    #[test]
    fn start_over_usb_holds_a_forward_until_the_stream_stops() {
        let (mut c, tunnels) = model();
        add(&mut c, "id-a");
        assert_eq!(c.hold_stream_route(&usb(), None, StreamFormat::default()).as_deref(), Some("https://127.0.0.1:41000/v1/video"));
        assert_eq!(tunnels.held(), [(("serial-1".into(), 8080), 1)]);
        c.on_stream_start("url");
        c.on_stream_stop(|_| false, 1.0);
        assert!(tunnels.held().is_empty());
    }

    #[test]
    fn start_uses_the_h264_route_only_when_chosen_and_decodable() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        let cfg = json!({"format": "h264"});
        assert!(c.hold_stream_route(&wifi(), None, stream_format(Some(&cfg), true)).unwrap().ends_with("/v1/video.h264"));
        assert!(c.hold_stream_route(&wifi(), None, stream_format(Some(&cfg), false)).unwrap().ends_with("/v1/video"));
    }

    #[test]
    fn start_over_usb_reports_a_failed_forward() {
        let (mut c, tunnels) = model();
        add(&mut c, "id-a");
        *tunnels.local.lock().unwrap() = None;
        assert_eq!(c.hold_stream_route(&usb(), None, StreamFormat::default()), None);
    }

    #[test]
    fn streaming_over_wifi_in_automatic_moves_to_usb_once_its_plugged_in() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.set_stream_route(Some(wifi()));
        c.on_stream_start("url");
        assert!(c.usb_watch_active);
        c.watch_id = 4;
        assert_eq!(c.on_usb_available(4, None, 100.0), UsbWatch::HideButton);
        assert_eq!(c.on_usb_available(4, Some(usb()), 100.0), UsbWatch::HandOver(usb()));
        assert!(!c.switch_usb_visible);
    }

    #[test]
    fn a_hand_over_holds_both_routes_until_it_moves_then_only_the_new_one() {
        let (mut c, tunnels) = model();
        add(&mut c, "id-a");
        c.set_hold("id-a", Some(wifi()), None);
        c.set_path("id-a", "/v1/video.h264");
        assert_eq!(c.prepare_stream_route(&usb(), "id-a", h264()).as_deref(), Some("https://127.0.0.1:41000/v1/video.h264"));
        assert_eq!(c.prepare_stream_route(&usb(), "id-a", h264()), None); // one at a time
        assert_eq!(tunnels.held(), [(("serial-1".into(), 8080), 1)]);
        assert_eq!(c.hold("id-a"), Some(&(Some(wifi()), None)));
        c.commit_stream_route("id-a");
        assert_eq!(c.hold("id-a"), Some(&(Some(usb()), Some("serial-1".into()))));
        c.release_stream(Some("id-a"), false);
        assert!(tunnels.held().is_empty());
    }

    #[test]
    fn a_failed_hand_over_lets_go_of_usb_and_offers_the_button_instead() {
        let (mut c, tunnels) = model();
        add(&mut c, "id-a");
        c.set_stream_route(Some(wifi()));
        c.on_stream_start("url");
        assert!(c.prepare_stream_route(&usb(), "id-a", StreamFormat::default()).is_some());
        c.drop_stream_route(Some("id-a"), true, 100.0);
        assert!(tunnels.held().is_empty());
        assert_eq!(c.hold("id-a"), Some(&(Some(wifi()), None)));
        assert!(c.switch_usb_visible);
        c.watch_id = 4;
        assert_eq!(c.on_usb_available(4, Some(usb()), 100.0), UsbWatch::Ignore); // USB sits out, and the button does it by hand
    }

    #[test]
    fn usb_that_dropped_a_stream_isnt_handed_straight_back_to() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.set_stream_route(Some(usb()));
        c.on_stream_start("url");
        c.connected = true;
        c.on_stream_lost(true, 50.0);
        c.set_stream_route(Some(wifi())); // recovery found it over Wi-Fi
        c.on_stream_start("url");
        c.watch_id = 4;
        assert_eq!(c.on_usb_available(4, Some(usb()), 50.0), UsbWatch::Ignore);
        assert_eq!(c.on_usb_available(4, Some(usb()), 61.0), UsbWatch::HandOver(usb()));
    }

    #[test]
    fn no_usb_watch_when_wifi_was_chosen_explicitly_or_already_on_usb() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.route_pref = RoutePreference::Wifi;
        c.set_stream_route(Some(wifi()));
        c.on_stream_start("url");
        assert!(!c.usb_watch_active);
        c.on_stream_stop(|_| true, 1.0);
        c.route_pref = RoutePreference::Auto;
        c.set_stream_route(Some(usb()));
        c.on_stream_start("url");
        assert!(!c.usb_watch_active);
    }

    #[test]
    fn a_usb_answer_after_the_stream_stopped_is_ignored() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.set_stream_route(Some(wifi()));
        c.on_stream_start("url");
        c.watch_id = 4;
        c.on_stream_stop(|_| false, 1.0);
        assert_eq!(c.on_usb_available(4, Some(usb()), 100.0), UsbWatch::Ignore);
        assert!(!c.switch_usb_visible);
    }

    #[test]
    fn a_lost_stream_says_reconnecting_until_frames_return() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.hold_stream_route(&wifi(), None, StreamFormat::default());
        c.on_stream_start("url");
        c.on_stream_connected(true);
        c.on_stream_lost(true, 1.0);
        assert!(c.lost && !c.usb_watch_active);
        c.on_stream_connected(true);
        assert!(!c.lost);
        c.on_stream_lost(false, 2.0); // another stream's drop
        assert!(!c.lost);
    }

    fn streaming_over(c: &mut Connection, route: RouteSpec, format: StreamFormat) {
        c.took_start_check(None, ready(route.clone()));
        let url = c.hold_stream_route(&route, None, format).unwrap();
        c.on_stream_start(&url);
        c.on_stream_connected(true);
    }

    #[test]
    fn recovery_moves_a_usb_stream_to_wifi_and_lets_go_of_the_forward() {
        let (mut c, tunnels) = model();
        add(&mut c, "id-a");
        streaming_over(&mut c, usb(), StreamFormat::default());
        assert_eq!(tunnels.held(), [(("serial-1".into(), 8080), 1)]);
        assert_eq!(c.adopt_stream_route(&wifi(), None, StreamFormat::default()).as_deref(), Some("https://10.0.0.5:8080/v1/video"));
        assert!(tunnels.held().is_empty());
        assert_eq!(route_text(c.stream_route().as_ref()), "Wi-Fi · 10.0.0.5");
        c.on_stream_stop(|_| false, 1.0);
        assert!(tunnels.held().is_empty());
    }

    #[test]
    fn recovery_keeps_the_format_the_stream_opened() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        streaming_over(&mut c, usb(), h264());
        c.selected_id = Some("id-b".into()); // the panels moved to another phone, on Heavy
        assert_eq!(c.adopt_stream_route(&wifi(), Some("id-a"), StreamFormat::default()).as_deref(), Some("https://10.0.0.5:8080/v1/video.h264"));
    }

    #[test]
    fn recovery_over_the_same_cable_opens_a_fresh_forward() {
        let (mut c, tunnels) = model();
        add(&mut c, "id-a");
        streaming_over(&mut c, usb(), StreamFormat::default());
        assert_eq!(c.adopt_stream_route(&usb(), None, StreamFormat::default()).as_deref(), Some("https://127.0.0.1:41000/v1/video"));
        assert_eq!(*tunnels.acquires.lock().unwrap(), 2);
        assert_eq!(tunnels.held(), [(("serial-1".into(), 8080), 1)]);
    }

    #[test]
    fn recovery_tries_again_after_a_usb_forward_failed() {
        let (mut c, tunnels) = model();
        add(&mut c, "id-a");
        streaming_over(&mut c, usb(), StreamFormat::default());
        *tunnels.local.lock().unwrap() = None;
        assert_eq!(c.adopt_stream_route(&usb(), None, StreamFormat::default()), None);
        *tunnels.local.lock().unwrap() = Some(41000);
        assert_eq!(c.adopt_stream_route(&usb(), None, StreamFormat::default()).as_deref(), Some("https://127.0.0.1:41000/v1/video"));
        assert_eq!(tunnels.held(), [(("serial-1".into(), 8080), 1)]);
    }

    #[test]
    fn no_route_is_adopted_once_the_stream_stopped() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        assert_eq!(c.adopt_stream_route(&wifi(), None, StreamFormat::default()), None);
    }

    #[test]
    fn session_channel_over_usb_releases_its_forward_even_on_error() {
        let tunnels = FakeTunnels::new();
        let auth = PhoneAuth::new("tok", "");
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_session(Some(&auth), Some(&usb()), &*tunnels, |client| {
                assert_eq!(client.unwrap().base, "https://127.0.0.1:41000");
                panic!("boom");
            })
        }));
        assert!(caught.is_err());
        assert!(tunnels.held().is_empty());
    }

    #[test]
    fn session_channel_without_a_route_or_token_yields_none() {
        let tunnels = FakeTunnels::new();
        assert!(with_session(Some(&PhoneAuth::new("tok", "")), None, &*tunnels, |c| c.is_none()));
        assert!(with_session(None, Some(&wifi()), &*tunnels, |c| c.is_none()));
    }

    fn ping(streaming: bool, busy: bool) -> Ping {
        Ping { streaming, busy, ..Ping::status(PingStatus::Paired) }
    }

    /// A phone answering pings from a script (the last answer repeats), recording calls.
    struct Script {
        pings: RefCell<Vec<Ping>>,
        start: SessionResult,
        calls: RefCell<Vec<&'static str>>,
        opening: RefCell<Option<Map<String, Value>>>,
    }

    impl Script {
        fn new(pings: Vec<Ping>) -> Script {
            Script { pings: RefCell::new(pings), start: SessionResult { ok: true, error: None, computer: None }, calls: RefCell::default(),
                     opening: RefCell::default() }
        }

        fn refusing(pings: Vec<Ping>, error: &str, computer: Option<&str>) -> Script {
            Script { start: SessionResult { ok: false, error: Some(error.into()), computer: computer.map(str::to_string) }, ..Script::new(pings) }
        }
    }

    impl SessionCtl for Script {
        fn ping(&self) -> Ping {
            self.calls.borrow_mut().push("ping");
            let mut pings = self.pings.borrow_mut();
            if pings.len() > 1 { pings.remove(0) } else { pings[0].clone() }
        }

        fn start(&self, opening: Option<&Map<String, Value>>) -> SessionResult {
            self.calls.borrow_mut().push("start");
            *self.opening.borrow_mut() = opening.cloned();
            self.start.clone()
        }

        fn stop(&self) -> SessionResult {
            self.calls.borrow_mut().push("stop");
            SessionResult { ok: true, error: None, computer: None }
        }
    }

    /// No real sleeping; the clock moves by step each time it's read (0: only by the sleeps).
    struct FakePace {
        t: Cell<f64>,
        step: f64,
        sleeps: RefCell<Vec<Duration>>,
    }

    impl FakePace {
        fn new(step: f64) -> FakePace {
            FakePace { t: Cell::new(0.0), step, sleeps: RefCell::default() }
        }
    }

    impl Pace for FakePace {
        fn sleep(&self, d: Duration) {
            self.sleeps.borrow_mut().push(d);
        }

        fn now(&self) -> f64 {
            let t = self.t.get();
            self.t.set(t + self.step);
            t
        }
    }

    fn ensure(script: &Script, after_stop: bool, pace: &FakePace) -> Result<(), String> {
        ensure_streaming(Some(script), None, after_stop, &|_| {}, pace)
    }

    #[test]
    fn ensure_streaming_is_a_no_op_when_already_streaming() {
        let s = Script::new(vec![ping(true, false)]);
        assert_eq!(ensure(&s, false, &FakePace::new(0.0)), Ok(()));
        assert_eq!(*s.calls.borrow(), ["ping"]);
    }

    #[test]
    fn ensure_streaming_starts_then_waits_for_the_camera() {
        let s = Script::new(vec![ping(false, false), ping(false, true), ping(true, false)]);
        let progress = RefCell::new(Vec::new());
        assert_eq!(ensure_streaming(Some(&s), None, false, &|p| progress.borrow_mut().push(p.to_string()), &FakePace::new(0.0)), Ok(()));
        assert_eq!(*s.calls.borrow(), ["ping", "start", "ping", "ping"]);
        assert_eq!(progress.borrow()[0], "Starting the phone's camera...");
    }

    #[test]
    fn ensure_streaming_right_after_a_stop_starts_again_once_the_old_stream_ends() {
        let s = Script::new(vec![ping(true, false), ping(false, true), ping(false, false), ping(false, true), ping(true, false)]);
        assert_eq!(ensure(&s, true, &FakePace::new(0.0)), Ok(()));
        assert_eq!(*s.calls.borrow(), ["ping", "ping", "ping", "start", "ping", "ping"]);
    }

    #[test]
    fn ensure_streaming_after_a_stop_joins_a_stream_that_doesnt_end() {
        let s = Script::new(vec![ping(true, false)]);
        assert_eq!(ensure(&s, true, &FakePace::new(1.0)), Ok(()));
        assert!(!s.calls.borrow().contains(&"start"));
    }

    #[test]
    fn ensure_streaming_asks_the_phone_to_open_at_the_size_picked() {
        let s = Script::new(vec![ping(false, false), ping(true, false)]);
        let opening = opening_of(1280, 720, 60);
        assert_eq!(ensure_streaming(Some(&s), Some(&opening), false, &|_| {}, &FakePace::new(0.0)), Ok(()));
        assert_eq!(s.opening.borrow().as_ref(), Some(&opening));
    }

    #[test]
    fn ensure_streaming_reports_a_refused_start() {
        let s = Script::refusing(vec![ping(false, false)], "no_camera_permission", None);
        assert!(ensure(&s, false, &FakePace::new(0.0)).unwrap_err().contains("camera access"));
    }

    #[test]
    fn ensure_streaming_leaves_another_computer_s_stream_alone() {
        let s = Script::new(vec![Ping { streaming_for: Some("Office PC".into()), ..ping(true, false) }]);
        assert!(ensure(&s, false, &FakePace::new(0.0)).unwrap_err().contains("streaming to Office PC"));
        assert_eq!(*s.calls.borrow(), ["ping"]);
    }

    #[test]
    fn ensure_streaming_names_the_computer_when_the_start_is_refused_for_it() {
        let s = Script::refusing(vec![ping(false, false)], "busy_other", Some("Office PC"));
        assert!(ensure(&s, false, &FakePace::new(0.0)).unwrap_err().contains("streaming to Office PC"));
    }

    #[test]
    fn ensure_streaming_stops_waiting_when_another_computer_s_start_won() {
        let s = Script::new(vec![ping(false, false), Ping { streaming_for: Some("Office PC".into()), ..ping(false, true) }]);
        assert!(ensure(&s, false, &FakePace::new(0.0)).unwrap_err().contains("streaming to Office PC"));
    }

    #[test]
    fn ensure_streaming_gives_up_when_the_camera_falls_back_to_idle() {
        let s = Script::new(vec![ping(false, false), ping(false, true), ping(false, false)]);
        assert!(ensure(&s, false, &FakePace::new(0.0)).unwrap_err().contains("stopped before it finished"));
    }

    #[test]
    fn ensure_streaming_tolerates_a_blip_but_not_a_sustained_outage() {
        let gone = Ping::status(PingStatus::Unreachable);
        let s = Script::new(vec![ping(false, false), gone.clone(), ping(false, true), ping(true, false)]);
        assert_eq!(ensure(&s, false, &FakePace::new(0.0)), Ok(()));
        let s = Script::new(vec![ping(false, false), gone.clone(), gone.clone(), gone]);
        assert!(ensure(&s, false, &FakePace::new(0.0)).unwrap_err().contains("Lost contact"));
    }

    #[test]
    fn ensure_streaming_checks_often_at_first_and_slows_down_when_the_phone_goes_quiet() {
        let gone = Ping::status(PingStatus::Unreachable);
        let s = Script::new(vec![ping(false, false), ping(false, true), gone, ping(false, true), ping(true, false)]);
        let pace = FakePace::new(0.0);
        assert_eq!(ensure(&s, false, &pace), Ok(()));
        let (fast, slow) = (START_FAST_POLL_INTERVAL, START_POLL_INTERVAL);
        assert_eq!(*pace.sleeps.borrow(), [fast, fast, slow, fast]);
    }

    #[test]
    fn ensure_streaming_times_out_rather_than_hanging() {
        let s = Script::new(vec![ping(false, false)]);
        assert!(ensure(&s, false, &FakePace::new(5.0)).unwrap_err().contains("didn't finish starting in time"));
    }

    #[test]
    fn ensure_streaming_explains_unreachable_and_unpaired() {
        assert!(ensure(&Script::new(vec![Ping::status(PingStatus::Unreachable)]), false, &FakePace::new(0.0)).unwrap_err().contains("Couldn't reach"));
        assert!(ensure(&Script::new(vec![Ping::status(PingStatus::NotPaired)]), false, &FakePace::new(0.0)).unwrap_err().contains("Add phone"));
        assert!(ensure_streaming(None, None, false, &|_| {}, &FakePace::new(0.0)).unwrap_err().contains("Lost the connection"));
    }

    #[test]
    fn computer_name_is_cleaned_and_capped() {
        let (mut c, _) = model();
        c.set_computer_name("  <b>Desk</b>\n");
        assert_eq!(c.computer_name, "bDesk/b");
        c.set_computer_name(&"x".repeat(200));
        assert_eq!(c.computer_name.chars().count(), 64);
        c.set_computer_name("   ");
        assert_eq!(c.computer_name.chars().count(), 64);
    }

    #[test]
    fn a_reinstalled_phone_is_found_by_name_while_the_old_one_doesnt_answer() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.apply_resolution(Resolution::new(PhoneStatus::NotPaired));
        let new = phone("id-new", "Pixel", &[]);
        assert_eq!(c.earlier_install(&new, |_| false, |_| None).map(|p| p.id), Some("id-a".into()));
        c.take_over("id-a", new.clone());
        assert_eq!(c.phones, [new]);
    }

    #[test]
    fn a_second_phone_with_the_same_name_that_still_answers_is_not_asked_about() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.on_phone_paired(phone("id-b", "Other", &[]), |_| false);
        c.mark_selected(Some("id-b"));
        let new = phone("id-new", "Pixel", &[]);
        assert_eq!(c.earlier_install(&new, |_| false, |_| Some(PhoneStatus::Ready)), None);
        assert_eq!(c.earlier_install(&new, |id| id == "id-a", |_| Some(PhoneStatus::Unreachable)), None);
    }

    #[test]
    fn forgetting_a_phone_picks_the_next_one() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.on_phone_paired(phone("id-b", "Other", &[]), |_| false);
        let (gone, reselect) = c.forget_phone("id-a").unwrap();
        assert_eq!((gone.id.as_str(), reselect), ("id-a", Some(Some("id-b".to_string()))));
        assert_eq!(c.forget_phone("id-b").unwrap().1, None); // not the picked one
    }

    #[test]
    fn udp_is_used_on_wifi_only_when_switched_on_and_dynamic_only_where_the_phone_runs_it() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        streaming_over(&mut c, usb(), h264());
        assert!(c.adopt_stream_route(&wifi(), Some("id-a"), h264()).unwrap().ends_with("/v1/video.h264")); // off by default
        c.udp = true;
        assert_eq!(c.adopt_stream_route(&wifi(), Some("id-a"), h264()).as_deref(), Some("https://10.0.0.5:8080/v1/video.udp"));
        assert!(c.adopt_stream_route(&usb(), Some("id-a"), h264()).unwrap().ends_with("/v1/video.h264")); // a cable: TCP
        c.set_hold("id-a", Some(wifi()), None);
        assert!(c.prepare_stream_route(&usb(), "id-a", h264()).unwrap().ends_with("/v1/video.h264"));
        c.drop_stream_route(Some("id-a"), false, 0.0);
        let dynamic = StreamFormat { h264: true, dynamic: true }; // an older phone measures TCP's queue
        assert!(c.adopt_stream_route(&wifi(), Some("id-a"), dynamic).unwrap().ends_with("/v1/video.h264"));
        c.apply_resolution(Resolution { udp_dynamic: true, ..ready(wifi()) }); // a newer one runs it from our reports
        assert!(c.adopt_stream_route(&wifi(), Some("id-a"), dynamic).unwrap().ends_with("/v1/video.udp"));
        c.apply_resolution(Resolution::new(PhoneStatus::Unreachable)); // a check that didn't reach it doesn't forget that
        assert!(c.adopt_stream_route(&wifi(), Some("id-a"), dynamic).unwrap().ends_with("/v1/video.udp"));
        c.apply_resolution(ready(wifi())); // downgraded
        assert!(c.adopt_stream_route(&wifi(), Some("id-a"), dynamic).unwrap().ends_with("/v1/video.h264"));
    }

    #[test]
    fn udp_switch_reconnects_a_running_wifi_stream() {
        let (mut c, _) = model();
        c.set_stream_route(Some(wifi()));
        c.on_stream_start("https://phone/v1/video.h264");
        c.udp = true;
        assert!(c.transport_needs_reconnect(h264()));
    }

    #[test]
    fn dynamic_keeps_udp_on_a_phone_that_runs_it_there() {
        let (mut c, _) = model();
        add(&mut c, "id-a");
        c.apply_resolution(Resolution { udp_dynamic: true, ..ready(wifi()) });
        c.udp = true;
        c.set_stream_route(Some(wifi()));
        c.on_stream_start("https://phone/v1/video.udp");
        assert!(!c.transport_needs_reconnect(StreamFormat { h264: true, dynamic: true }));
    }

    #[test]
    fn dynamic_replaces_a_running_udp_stream_with_tcp_on_an_older_phone() {
        let (mut c, _) = model();
        c.udp = true;
        c.set_stream_route(Some(wifi()));
        c.on_stream_start("https://phone/v1/video.udp");
        assert!(c.transport_needs_reconnect(StreamFormat { h264: true, dynamic: true }));
        c.set_config(&json!({"udp": false}), |_| false);
        assert!(!c.udp);
    }

    #[test]
    fn busy_and_refusal_reasons_read_like_pythons() {
        assert_eq!(busy_other_reason(None), "The phone is streaming to another computer.\n\nStop it there or on the phone, then try again.");
        assert_eq!(start_refused_reason(Some("weird"), None), "The phone refused to start streaming (weird).");
        assert_eq!(start_refused_reason(None, None), "The phone refused to start streaming (unknown error).");
    }
}
