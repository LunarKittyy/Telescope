//! The real window, with a fake phone link and fake stream workers: what a stream does, read back from the window's own
//! properties by name. Reports from threads come in through offscreen::pump, and timers go by offscreen::skip.
#![allow(dead_code)]

use serde_json::{json, Value};
use slint::{Model, ModelRc};
use slint_interpreter::Value as SValue;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::event::Event;
use telescope_core::frame::{Frame, FrameStep, PixelFormat};
use telescope_core::host::Host;
use telescope_core::link::{Link, PairingOffer, PhoneControl, PhoneSensors, PhoneSession, PhoneStatus, PhoneStills, Resolution, RoutePrefs, StreamFormat, StreamRequest, StreamTarget, Unreachable};
use telescope_core::media::{Codec, FeedEnd, FeedStopper, FrameSlot, Packet, PacketTap, StagedStep, VideoFeed};
use telescope_core::output::{Outputs, SetupAction, SetupResult, Slot, VideoOutput};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::services::Services;
use telescope_core::system::{Notifier, Tray, TrayItem};
use telescope_core::ui::Issue;
use telescope_core::source::{ControlClient, PairedPhone, RouteSpec, StreamSource};
use telescope_shell::offscreen;
use telescope_shell::session::SourceRef;
use telescope_shell::sources::Report;
use telescope_shell::window::{self, Window, WindowOptions};
use telescope_shell::worker::{Events, Opener, VideoWorker, WorkerEvent, WorkerMaker, WorkerSetup};

/// offscreen's job queue is one per process, so these tests take turns.
static TURN: Mutex<()> = Mutex::new(());

pub type Log = Arc<Mutex<Vec<String>>>;

fn note(log: &Log, what: impl Into<String>) {
    log.lock().unwrap().push(what.into());
}

/// A stream's controls: what went to the phone, and what it answers.
#[derive(Default)]
pub struct Control {
    pub sent: Mutex<Vec<Value>>,
    pub state: Mutex<Option<Value>>,
    /// get_state waits while this is on: a phone slow to answer.
    pub stalled: AtomicBool,
    /// Asks by the checks a test drives; the two below count on threads of their own, so they aren't mixed up with them.
    pub asked: AtomicU32,
    /// The ask a moment after a stream began, on a clock of its own.
    pub fetched: AtomicU32,
    /// The ask after the panels came back to a stream.
    pub refreshed: AtomicU32,
    /// What that ask alone is answered.
    pub refresh_answer: Mutex<Option<Value>>,
    pub resent: AtomicU32,
    pub closed: AtomicBool,
    pub retargeted: Mutex<Vec<String>>,
}

impl Control {
    /// The actions sent, by name.
    pub fn actions(&self) -> Vec<(String, Value)> {
        self.sent.lock().unwrap().iter().map(|b| (b["action"].as_str().unwrap_or("").to_string(), b["value"].clone())).collect()
    }
}

impl PhoneControl for Control {
    fn send(&self, body: &Value) {
        self.sent.lock().unwrap().push(body.clone());
    }
    fn get_state(&self) -> Option<Value> {
        match std::thread::current().name() {
            Some("state-fetch") => &self.fetched,
            Some("state-refresh") => &self.refreshed,
            _ => &self.asked,
        }.fetch_add(1, Ordering::SeqCst);
        while self.stalled.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        if std::thread::current().name() == Some("state-refresh") {
            return self.refresh_answer.lock().unwrap().clone();
        }
        self.state.lock().unwrap().clone()
    }
    fn retarget(&self, stream_base: &str) {
        self.retargeted.lock().unwrap().push(stream_base.into());
    }
    fn resend_settings(&self) {
        self.resent.fetch_add(1, Ordering::SeqCst);
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

struct Session {
    route: RouteSpec,
    url: String,
    controls: Arc<Control>,
    released: Arc<AtomicBool>,
}

impl ControlClient for Session {
    fn send(&self, _body: &Value) -> Result<(), String> {
        Ok(())
    }
    fn state(&self) -> Result<Value, String> {
        Ok(json!({}))
    }
    fn has_audio(&self) -> bool {
        true
    }
}

impl PhoneSession for Session {
    fn route(&self) -> RouteSpec {
        self.route.clone()
    }
    fn describe(&self) -> String {
        String::new()
    }
    fn start(&self, _request: StreamRequest) -> Result<(), String> {
        Ok(())
    }
    fn stop(&self) -> Result<(), String> {
        Ok(())
    }
    fn open_video(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        Err("no video in tests".into())
    }
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
    }
    fn video_url(&self) -> String {
        self.url.clone()
    }
    fn stream_base(&self) -> String {
        self.url.clone()
    }
    fn controls(&self) -> Option<Arc<dyn PhoneControl>> {
        Some(self.controls.clone())
    }
}

/// Paired phones behind a link that does what the test says.
#[derive(Default)]
pub struct FakeLink {
    pub log: Log,
    phones: Mutex<Vec<PairedPhone>>,
    pub picked: Mutex<Option<String>>,
    /// The next start finds no route.
    pub no_route: AtomicBool,
    /// Why the next wakes fail.
    pub wake_error: Mutex<Option<String>>,
    /// Wakes wait here until it's false.
    held: Mutex<bool>,
    /// Wakes of these phones wait here until they're taken out.
    held_phones: Mutex<std::collections::BTreeSet<String>>,
    release: Condvar,
    /// What a dropped stream's phone looks like, by source id.
    pub recovery: Mutex<BTreeMap<String, Resolution>>,
    /// The openings wakes asked for: (source, what, after_stop).
    pub openings: Mutex<Vec<(String, StreamRequest, bool)>>,
    pub formats: Mutex<Vec<StreamFormat>>,
    pub controls: Mutex<BTreeMap<String, Arc<Control>>>,
    pub released: Mutex<Vec<Arc<AtomicBool>>>,
    pub progress: Mutex<Vec<String>>,
    /// A wake that said its progress waits here, while this is on, so a test can read what it said.
    pub hold_after_progress: AtomicBool,
    pub sensors: Mutex<Option<Arc<dyn PhoneSensors>>>,
    pub stills: Mutex<Option<Arc<dyn PhoneStills>>>,
    /// What prepare_route answers: the video URL on the route it's asked for.
    pub prepared: Mutex<Option<String>>,
    /// Stops take this long to get there: a phone that's gone.
    pub stops_hang: AtomicBool,
}

impl FakeLink {
    pub fn pair(&self, id: &str, name: &str) {
        let phone = PairedPhone { id: id.into(), name: name.into(), token: String::new(), ips: vec!["10.0.0.2".into()], active_ip: None, cert_sha256: String::new() };
        self.phones.lock().unwrap().push(phone);
    }

    pub fn hold_wakes(&self, held: bool) {
        *self.held.lock().unwrap() = held;
        self.release.notify_all();
    }

    /// This phone's wakes wait, or go on; the others' don't.
    pub fn hold_wakes_of(&self, id: &str, held: bool) {
        let _guard = self.held.lock().unwrap();
        let mut phones = self.held_phones.lock().unwrap();
        if held {
            phones.insert(id.into());
        } else {
            phones.remove(id);
        }
        drop(phones);
        self.release.notify_all();
    }

    /// This phone paired again.
    pub fn set_token(&self, id: &str, token: &str) {
        self.phones.lock().unwrap().iter_mut().filter(|p| p.id == id).for_each(|p| p.token = token.into());
    }

    /// This phone's controls.
    pub fn control(&self, id: &str) -> Arc<Control> {
        self.controls.lock().unwrap().entry(id.into()).or_default().clone()
    }

    pub fn picked_now(&self) -> Option<String> {
        self.picked.lock().unwrap().clone()
    }

    pub fn logged(&self, what: &str) -> usize {
        self.log.lock().unwrap().iter().filter(|l| l.as_str() == what).count()
    }

    pub fn answers_recovery(&self, id: &str, res: Resolution) {
        self.recovery.lock().unwrap().insert(id.into(), res);
    }
}

impl Link for FakeLink {
    fn new_identity(&self) -> (String, String) {
        Default::default()
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        Err(Unreachable::NoAnswer)
    }
    fn pair(&self, _id: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        Err("not in tests".into())
    }
    fn phones(&self) -> Vec<PairedPhone> {
        self.phones.lock().unwrap().clone()
    }
    fn picked_source(&self) -> Option<String> {
        self.picked.lock().unwrap().clone()
    }
    fn start_route(&self, source_id: Option<&str>, format: StreamFormat) -> Result<StreamTarget, String> {
        note(&self.log, format!("start_route {}", source_id.unwrap_or("")));
        self.formats.lock().unwrap().push(format);
        if self.no_route.load(Ordering::SeqCst) {
            return Err("no route".into());
        }
        let id = source_id.map(String::from).or_else(|| self.picked_source()).ok_or("nothing picked")?;
        let phone = self.phones().into_iter().find(|p| p.id == id).ok_or("not paired")?;
        Ok(StreamTarget { phone, route: RouteSpec::Network { address: "10.0.0.2".into() }, video_url: format!("https://{id}:8080/v1/video.h264") })
    }
    fn open_route(&self, phone: &PairedPhone, route: &RouteSpec, video_path: &str, controls: Option<Arc<dyn PhoneControl>>)
                  -> Result<Box<dyn PhoneSession>, String> {
        let host = match route {
            RouteSpec::Network { address } if address == "10.0.0.2" => phone.id.clone(),
            RouteSpec::Network { address } => address.clone(),
            RouteSpec::Usb { serial } => format!("usb-{serial}"),
        };
        let _ = controls;
        note(&self.log, format!("open {} with {}", phone.id, phone.token));
        let released = Arc::<AtomicBool>::default();
        self.released.lock().unwrap().push(released.clone());
        Ok(Box::new(Session { route: route.clone(), url: format!("https://{host}:8080{video_path}"), controls: self.control(&phone.id), released }))
    }
    fn ensure_streaming(&self, phone: &PairedPhone, _route: &RouteSpec, opening: Option<StreamRequest>, after_stop: bool,
                        progress: &dyn Fn(&str)) -> Result<(), String> {
        if let Some(opening) = opening {
            self.openings.lock().unwrap().push((phone.id.clone(), opening, after_stop));
        }
        note(&self.log, format!("wake {}", phone.id));
        let mut held = self.held.lock().unwrap();
        while *held || self.held_phones.lock().unwrap().contains(&phone.id) {
            held = self.release.wait(held).unwrap();
        }
        drop(held);
        for text in self.progress.lock().unwrap().iter() {
            progress(text);
        }
        while self.hold_after_progress.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        match self.wake_error.lock().unwrap().clone() {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }
    fn stop_streaming(&self, phone: &PairedPhone, _route: &RouteSpec) {
        note(&self.log, format!("stop {}", phone.id));
        while self.stops_hang.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn session_target(&self, source_id: Option<&str>) -> Option<(PairedPhone, RouteSpec)> {
        let id = source_id.map(String::from).or_else(|| self.picked_source())?;
        let phone = self.phones().into_iter().find(|p| p.id == id)?;
        Some((phone, RouteSpec::Network { address: "10.0.0.2".into() }))
    }
    fn release_stream(&self, source_id: &str) {
        note(&self.log, format!("release {source_id}"));
    }
    fn sensors(&self) -> Option<Arc<dyn PhoneSensors>> {
        self.sensors.lock().unwrap().clone()
    }
    fn stills(&self) -> Option<Arc<dyn PhoneStills>> {
        self.stills.lock().unwrap().clone()
    }
    fn prepare_route(&self, source_id: &str, route: &RouteSpec) -> Option<String> {
        note(&self.log, format!("prepare {source_id} {route:?}"));
        self.prepared.lock().unwrap().clone()
    }
    fn commit_route(&self, source_id: &str) {
        note(&self.log, format!("commit {source_id}"));
    }
    fn drop_route(&self, source_id: &str, failed: bool) {
        note(&self.log, format!("drop_route {source_id} {failed}"));
    }
    fn recovery_check(&self, source_id: Option<&str>) -> Resolution {
        note(&self.log, format!("recovery_check {}", source_id.unwrap_or("")));
        self.recovery.lock().unwrap().get(source_id.unwrap_or("")).cloned().unwrap_or_else(|| Resolution::new(PhoneStatus::Unreachable))
    }
    fn adopt_route(&self, source_id: Option<&str>, route: &RouteSpec) -> Option<String> {
        note(&self.log, format!("adopt {} {route:?}", source_id.unwrap_or("")));
        Some("https://moved:8080/v1/video.h264".to_string())
    }
    fn show_problem(&self, source_id: Option<&str>, res: &Resolution) {
        note(&self.log, format!("problem {} {:?}", source_id.unwrap_or(""), res.status));
    }
}

/// What a stream asks of its worker, written down.
pub struct FakeWorker {
    pub url: Mutex<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: u32,
    pub canvas: (Option<u32>, Option<u32>),
    pub camera_slot: u32,
    source: String,
    events: Events,
    slot: Arc<dyn FrameSlot>,
    pub log: Log,
    pub stopped: AtomicBool,
    pub parked_now: AtomicBool,
    pub tries: AtomicU32,
    pub arrival: Mutex<f32>,
    pub settled: AtomicU32,
    pub steps: Mutex<Vec<Vec<String>>>,
    pub first_steps: Vec<String>,
    pub size: Mutex<Option<(u32, u32)>>,
    /// hand_over() says no.
    pub refuses_hand_over: AtomicBool,
    /// hand_over() says no though it said it could, what changes between asking and trying.
    pub declines_hand_over: AtomicBool,
    /// How it opens its stream now.
    pub opener: Mutex<Opener>,
    pub packets: Arc<telescope_media::packets::Tap>,
    /// How it was told to open what it's handed over to.
    pub handed: Mutex<Option<Opener>>,
    /// Every worker before it had been stopped when this one was made.
    pub after_the_others: bool,
}

impl FakeWorker {
    /// The worker says something, as its thread would.
    pub fn says(&self, kind: &str, msg: &str) {
        (self.events)(WorkerEvent::Status(kind.into(), msg.into()));
    }

    pub fn event(&self, event: WorkerEvent) {
        (self.events)(event);
    }

    pub fn did(&self, what: &str) -> bool {
        self.log.lock().unwrap().iter().any(|l| l == what)
    }

    pub fn is_parked(&self) -> bool {
        self.parked_now.load(Ordering::SeqCst)
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

impl VideoWorker for FakeWorker {
    fn source_id(&self) -> Option<&str> {
        Some(self.source.as_str()).filter(|s| !s.is_empty())
    }
    fn tap(&self) -> Option<Arc<dyn PacketTap>> {
        Some(self.packets.clone())
    }
    fn slot(&self) -> Arc<dyn FrameSlot> {
        self.slot.clone()
    }
    fn set_steps(&self, steps: Vec<StagedStep>) {
        self.steps.lock().unwrap().push(steps.iter().map(|s| s.owner.clone()).collect());
    }
    fn set_colour(&self, steps: Vec<FrameStep>) {
        note(&self.log, format!("colour {}", steps.len()));
    }
    fn update_output(&self, width: Option<Option<u32>>, height: Option<Option<u32>>, fps: Option<u32>) {
        note(&self.log, format!("update_output {width:?} {height:?} {fps:?}"));
    }
    fn settle(&self) {
        self.settled.fetch_add(1, Ordering::SeqCst);
    }
    fn hold_for_photo(&self, seconds: f32) {
        note(&self.log, format!("hold_for_photo {seconds}"));
    }
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
    fn parked(&self) -> bool {
        self.parked_now.load(Ordering::SeqCst)
    }
    fn set_parked(&self, parked: bool) {
        self.parked_now.store(parked, Ordering::SeqCst);
    }
    fn retarget(&self, url: &str, opener: Opener) {
        *self.opener.lock().unwrap() = opener;
        *self.url.lock().unwrap() = url.into();
        note(&self.log, format!("retarget {url}"));
    }
    fn can_hand_over(&self) -> bool {
        !self.refuses_hand_over.load(Ordering::SeqCst)
    }
    fn set_opener(&self, opener: Opener) {
        *self.opener.lock().unwrap() = opener;
        note(&self.log, "set_opener");
    }
    fn failed_tries(&self) -> u32 {
        self.tries.load(Ordering::SeqCst)
    }
    fn last_arrival_fps(&self) -> f32 {
        *self.arrival.lock().unwrap()
    }
    fn camera_size(&self) -> Option<(u32, u32)> {
        *self.size.lock().unwrap()
    }
    fn hand_over(&self, url: &str, opener: Opener) -> bool {
        note(&self.log, format!("hand_over {url}"));
        *self.handed.lock().unwrap() = Some(opener);
        self.can_hand_over() && !self.declines_hand_over.load(Ordering::SeqCst)
    }
}

/// Hears a stream's hooks and events, in order; it follows the panels unless it's the mic.
pub struct Listener {
    pub name: &'static str,
    pub follows: bool,
    pub heard: Log,
    pub config: Value,
    /// Its settings are the same for every phone.
    pub global: bool,
}

impl Listener {
    pub fn new(name: &'static str, heard: &Log) -> Listener {
        Listener { name, follows: name != "microphone", heard: heard.clone(), config: json!({}), global: false }
    }
}

impl Plugin for Listener {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: self.name.into(), title: self.name.into(), follows_focus: self.follows, device_local: !self.global, ..Default::default() }
    }
    fn get_config(&self) -> Value {
        self.config.clone()
    }
    fn set_config(&mut self, config: &Value) -> Result<(), ConfigRejected> {
        self.config = config.clone();
        Ok(())
    }
    fn on_issue_action(&mut self, host: &mut dyn Host, key: &str, id: &str) {
        note(&self.heard, format!("{} issue {key} {id} on {} zoom {}", self.name, host.focused_source_id().unwrap_or_default(), self.config["zoom"]));
    }
    fn on_stream_start(&mut self, _host: &mut dyn Host, info: &StreamInfo) {
        note(&self.heard, format!("{} start {}", self.name, info.source_id));
        note(&self.heard, format!("{} url {} session {}", self.name, info.url, info.session));
    }
    fn on_stream_starting(&mut self, _host: &mut dyn Host) {
        note(&self.heard, format!("{} starting", self.name));
    }
    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        note(&self.heard, format!("{} stop", self.name));
        note(&self.heard, format!("{} sees streaming {} camera {}", self.name, host.is_streaming(), host.is_camera_on()));
    }
    fn on_focus_gained(&mut self, _host: &mut dyn Host, info: &StreamInfo) {
        note(&self.heard, format!("{} focus {}", self.name, info.source_id));
        note(&self.heard, format!("{} url {} session {}", self.name, info.url, info.session));
    }
    fn on_focus_lost(&mut self, _host: &mut dyn Host) {
        note(&self.heard, format!("{} left", self.name));
    }
    fn on_phone_state(&mut self, _host: &mut dyn Host, state: &Value) {
        note(&self.heard, format!("{} state {state}", self.name));
    }
    fn on_camera_off(&mut self, host: &mut dyn Host) {
        note(&self.heard, format!("{} camera off", self.name));
        note(&self.heard, format!("{} sees streaming {} camera {}", self.name, host.is_streaming(), host.is_camera_on()));
    }
    fn on_camera_on(&mut self, host: &mut dyn Host) {
        note(&self.heard, format!("{} camera on", self.name));
        note(&self.heard, format!("{} sees streaming {} camera {}", self.name, host.is_streaming(), host.is_camera_on()));
    }
    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::StreamBehind { source_id, behind } => note(&self.heard, format!("{} behind {source_id} {behind}", self.name)),
            Event::CameraOnChanged { on } => {
                note(&self.heard, format!("{} camera_on_changed {on}", self.name));
                note(&self.heard, format!("{} sees camera {} can turn off {}", self.name, host.is_camera_on(), host.can_turn_camera_off().0));
            }
            Event::StreamLost { source_id } => note(&self.heard, format!("{} lost {source_id}", self.name)),
            Event::StreamConnected { source_id } => note(&self.heard, format!("{} connected {source_id}", self.name)),
            Event::PhoneStateUpdated { state } => note(&self.heard, format!("{} bus state {state}", self.name)),
            Event::StreamStarted { url } => note(&self.heard, format!("{} stream_started {url}", self.name)),
            Event::StreamStopped => note(&self.heard, format!("{} stream_stopped", self.name)),
            Event::StreamStartFailed => note(&self.heard, format!("{} start_failed", self.name)),
            Event::IdleOutputs { slots } => note(&self.heard, format!("{} idle {slots:?}", self.name)),
            Event::RequestDone { ok, message, data, .. } => note(&self.heard, format!("{} done {ok} {message} {}", self.name, data["command"].as_str().unwrap_or(""))),
            Event::WindowShown { shown } => note(&self.heard, format!("{} shown {shown}", self.name)),
            Event::FocusModeChanged { mode } => note(&self.heard, format!("{} focus mode {mode}", self.name)),
            Event::SensorsChanged { source_id } => note(&self.heard, format!("{} sensors {source_id}", self.name)),
            Event::StillBusyChanged { source_id, busy } => note(&self.heard, format!("{} busy {source_id} {busy}", self.name)),
            Event::StillSaved { source_id, paths } => note(&self.heard, format!("{} saved {source_id} {paths:?}", self.name)),
            _ => {}
        }
    }
}

#[derive(Default)]
pub struct Cameras {
    pub asked: Mutex<Vec<SetupAction>>,
    /// Slots set up, past the usual one.
    pub extras: AtomicU32,
    /// Why setting them up fails, with the command to run by hand.
    pub fails: Mutex<Option<(String, String)>>,
    /// Setting up waits while this is on.
    pub held: AtomicBool,
    /// Setting up falls over.
    pub panics: AtomicBool,
}

impl Outputs for Cameras {
    fn slots(&self) -> Vec<Slot> {
        (0..=self.extras.load(Ordering::SeqCst)).map(|slot| Slot { slot, name: format!("Telescope {slot}"), device: String::new() }).collect()
    }
    fn set_up(&self, action: SetupAction, _progress: &dyn Fn(&str)) -> SetupResult {
        self.asked.lock().unwrap().push(action);
        while self.held.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!self.panics.load(Ordering::SeqCst), "modprobe gone");
        if let Some((message, command)) = self.fails.lock().unwrap().clone() {
            return SetupResult { ok: false, message, command: Some(command) };
        }
        self.extras.store(3, Ordering::SeqCst);
        SetupResult { ok: true, message: String::new(), command: None }
    }
    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::Rgba]
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no cameras in tests".into())
    }
}

/// The tray, as the window sets it, with clicks a test queues.
#[derive(Default)]
pub struct TrayState {
    pub items: Vec<String>,
    pub tips: Vec<String>,
    pub clicks: Vec<String>,
}

struct FakeTray(Arc<Mutex<TrayState>>);

impl Tray for FakeTray {
    fn set_items(&mut self, items: &[TrayItem]) {
        self.0.lock().unwrap().items = items.iter().map(|i| i.title.clone()).collect();
    }
    fn set_tooltip(&mut self, text: &str) {
        let tips = &mut self.0.lock().unwrap().tips;
        if tips.last().map(String::as_str) != Some(text) {
            tips.push(text.into());
        }
    }
    fn poll_click(&mut self) -> Option<String> {
        self.0.lock().unwrap().clicks.pop()
    }
}

#[derive(Default)]
pub struct Notes(pub Mutex<Vec<(String, String, bool)>>);

impl Notifier for Notes {
    fn notify(&self, title: &str, body: &str, urgent: bool) {
        self.0.lock().unwrap().push((title.into(), body.into(), urgent));
    }
}

/// The window, its link, and the workers it started, oldest first.
pub struct Rig {
    pub w: Rc<Window>,
    pub link: Arc<FakeLink>,
    pub cameras: Arc<Cameras>,
    pub workers: Arc<Mutex<Vec<Arc<FakeWorker>>>>,
    pub heard: Log,
    pub dir: std::path::PathBuf,
    pub tray: Arc<Mutex<TrayState>>,
    pub notes: Arc<Notes>,
    _turn: MutexGuard<'static, ()>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        // Threads asking these phones about their state stop now, not on the next test's window
        self.link.controls.lock().unwrap().values().for_each(|c| c.close());
        window::forget();
        offscreen::drop_jobs();
    }
}

pub fn options() -> WindowOptions {
    WindowOptions { prefs: RoutePrefs::default(), request: StreamRequest { width: 0, height: 0, fps: 30, camera_off: false }, hardware_decode: false,
                    package: String::new(), autostart: false, quit_after: None, snapshot: None, minimized: false, on_shown: None }
}

impl Rig {
    /// A window with "pixel" and "oneplus" paired and "pixel" picked, and the plugins listening.
    pub fn new(name: &str) -> Rig {
        Rig::with(name, |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(Listener::new("microphone", heard))])
    }

    pub fn with(name: &str, plugins: impl FnOnce(&Arc<FakeLink>, &Log) -> Vec<Box<dyn Plugin>>) -> Rig {
        Rig::built(name, false, plugins)
    }

    /// As new, with a tray.
    pub fn with_tray(name: &str) -> Rig {
        Rig::built(name, true, |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(Listener::new("microphone", heard))])
    }

    /// As with_tray, with the mic turned on.
    pub fn built_with_mic_tray(name: &str) -> Rig {
        Rig::built(name, true, |_, heard| {
            let mut mic = Listener::new("microphone", heard);
            mic.config = json!({"enabled": true});
            vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(mic)]
        })
    }

    pub fn built_tray(name: &str, plugins: impl FnOnce(&Arc<FakeLink>, &Log) -> Vec<Box<dyn Plugin>>) -> Rig {
        Rig::built(name, true, plugins)
    }

    /// A window with no phones, cameras or anything else from outside, as with no Connection.
    pub fn bare(name: &str) -> Rig {
        Rig::made(name, false, |_, _| Vec::new(), false)
    }

    fn built(name: &str, has_tray: bool, plugins: impl FnOnce(&Arc<FakeLink>, &Log) -> Vec<Box<dyn Plugin>>) -> Rig {
        Rig::made(name, has_tray, plugins, true)
    }

    fn made(name: &str, has_tray: bool, plugins: impl FnOnce(&Arc<FakeLink>, &Log) -> Vec<Box<dyn Plugin>>, with_services: bool) -> Rig {
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        offscreen::install();
        offscreen::drop_jobs();
        let dir = telescope_core::testing::scratch_dir(name);
        let link = Arc::new(FakeLink::default());
        link.pair("pixel", "Pixel");
        link.pair("oneplus", "OnePlus");
        *link.picked.lock().unwrap() = Some("pixel".into());
        let cameras = Arc::new(Cameras::default());
        cameras.extras.store(3, Ordering::SeqCst);
        let (tray, notes) = (Arc::<Mutex<TrayState>>::default(), Arc::<Notes>::default());
        let services = Arc::new(Services {
            link: link.clone(), media: Arc::new(telescope_media::FfmpegMedia), outputs: cameras.clone(), audio: None,
            notifier: Some(notes.clone()), autostart: None, updater: None,
            tray: has_tray.then(|| Mutex::new(Box::new(FakeTray(tray.clone())) as Box<dyn Tray>)), plugins: Box::new(Vec::new), vision: None,
            plugin_problems: Default::default(), plugin_admin: None,
        });
        let heard: Log = Log::default();
        let list = plugins(&link, &heard);
        let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| list, with_services.then_some(services), Some(options())).unwrap();
        let workers: Arc<Mutex<Vec<Arc<FakeWorker>>>> = Arc::default();
        let made = workers.clone();
        let maker: WorkerMaker = Rc::new(move |setup: WorkerSetup, slot, events| {
            let after_the_others = made.lock().unwrap().iter().all(|w| w.is_stopped());
            let worker = Arc::new(FakeWorker {
                url: Mutex::new(setup.url), width: setup.width, height: setup.height, fps: setup.fps, canvas: setup.canvas,
                camera_slot: setup.slot, source: setup.source_id, events, slot, log: Log::default(), stopped: AtomicBool::new(false),
                parked_now: AtomicBool::new(false), tries: AtomicU32::new(0), arrival: Mutex::new(0.0), settled: AtomicU32::new(0),
                steps: Mutex::default(), first_steps: setup.steps.iter().map(|s| s.owner.clone()).collect(), size: Mutex::new(None),
                refuses_hand_over: AtomicBool::new(false), declines_hand_over: AtomicBool::new(false), opener: Mutex::new(setup.opener), handed: Mutex::new(None), packets: telescope_media::packets::Tap::new(), after_the_others,
            });
            made.lock().unwrap().push(worker.clone());
            worker
        });
        *w.streams.maker.borrow_mut() = Some(maker);
        Rig { w, link, cameras, workers, heard, dir, tray, notes, _turn: turn }
    }

    /// Runs what threads sent, until cond holds.
    pub fn until(&self, what: &str, cond: impl Fn(&Rig) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond(self) {
            assert!(Instant::now() < deadline, "never happened: {what}");
            if !offscreen::pump() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Lets what's in flight land, for checking that something didn't happen.
    pub fn pump(&self) {
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(5));
            offscreen::pump();
        }
    }

    /// Moves the clock on and lets what came due run.
    pub fn skip(&self, ms: u64) {
        offscreen::skip(Duration::from_millis(ms));
        self.pump();
    }

    /// A plugin offers this source.
    pub fn offer(&self, source: FakeSource) -> Arc<FakeSource> {
        let source = Arc::new(source);
        self.w.add_plugin_source(source.clone());
        source
    }

    pub fn pick(&self, id: &str) {
        *self.link.picked.lock().unwrap() = Some(id.into());
    }

    /// A banner up, as a plugin would put it.
    pub fn show_issue(&self, key: &str, title: &str) {
        self.w.state.borrow_mut().banners.show(key, issue(title));
        self.w.show_banners();
    }

    /// What a plugin's settings are now changed to, as its panel would.
    pub fn configure(&self, plugin: &str, config: Value) {
        let mut state = self.w.state.borrow_mut();
        let at = state.index(plugin).unwrap();
        state.slots[at].plugin.as_mut().unwrap().set_config(&config).unwrap();
    }

    /// `id` stops streaming.
    pub fn stop(&self, id: Option<&str>) {
        self.w.with_flow(|flow, host| flow.stop_stream(host, id));
    }

    /// Start pressed, and the phone woke.
    pub fn start(&self) {
        let workers = self.workers.lock().unwrap().len();
        self.w.toggle_stream();
        self.until("the stream's worker", |r| r.workers.lock().unwrap().len() > workers);
    }

    /// `id` starts next to what streams, and woke.
    pub fn add(&self, id: &str) {
        let workers = self.workers.lock().unwrap().len();
        self.w.with_flow(|flow, host| flow.add_stream(host, id));
        self.until("the second stream's worker", |r| r.workers.lock().unwrap().len() > workers);
    }

    pub fn worker(&self, n: usize) -> Arc<FakeWorker> {
        self.workers.lock().unwrap()[n].clone()
    }

    pub fn last_worker(&self) -> Arc<FakeWorker> {
        self.workers.lock().unwrap().last().unwrap().clone()
    }

    pub fn workers_made(&self) -> usize {
        self.workers.lock().unwrap().len()
    }

    /// The source with that id, streaming or not.
    pub fn source(&self, id: &str) -> SourceRef {
        self.w.source(Some(id))
    }

    pub fn session_id(&self, id: &str) -> u64 {
        self.source(id).session().expect("a stream").id
    }

    /// The phone's state coming back for its stream.
    pub fn phone_says(&self, id: &str, state: Value) {
        let session_id = self.session_id(id);
        self.w.phone_report(Report::State { source_id: id.into(), session_id, state });
    }

    /// A window property's text, by name.
    pub fn text(&self, name: &str) -> String {
        match self.w.ui.get_property(name) {
            Ok(SValue::String(s)) => s.to_string(),
            other => panic!("{name} is {other:?}"),
        }
    }

    pub fn flag(&self, name: &str) -> bool {
        self.w.ui.get_property(name) == Ok(SValue::Bool(true))
    }

    /// The rows of a window property, each as (field, value) text by name.
    pub fn rows(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        let Ok(SValue::Model(model)) = self.w.ui.get_property(name) else { panic!("{name} isn't a list") };
        rows_of(&model)
    }

    /// The banners up now, by key.
    pub fn banner(&self, key: &str) -> Option<BTreeMap<String, String>> {
        self.rows("banners").into_iter().find(|b| b["key"] == key)
    }

    pub fn tile(&self, id: &str) -> Option<BTreeMap<String, String>> {
        self.rows("tiles").into_iter().find(|t| t["id"] == id)
    }

    pub fn heard(&self) -> Vec<String> {
        self.heard.lock().unwrap().clone()
    }

    pub fn forget_heard(&self) {
        self.heard.lock().unwrap().clear();
    }

    /// How many times something was heard.
    pub fn times_heard(&self, what: &str) -> usize {
        self.heard.lock().unwrap().iter().filter(|l| *l == what).count()
    }

    pub fn did_hear(&self, what: &str) -> bool {
        self.heard.lock().unwrap().iter().any(|l| l == what)
    }

    /// A plugin's event, as if it was sent on the bus.
    pub fn emit(&self, event: Event) {
        self.w.emit(event);
    }

    /// The streams running, by id.
    pub fn streaming(&self) -> Vec<String> {
        self.w.streams.flow.borrow().sessions.streams.iter().map(|s| s.id_str().to_string()).collect()
    }

    pub fn shown(&self) -> Option<String> {
        self.w.streams.flow.borrow().sessions.focus.as_ref().map(|s| s.id_str().to_string())
    }

    /// The virtual cameras last said to be free, as the listeners heard it.
    pub fn idle_outputs(&self) -> String {
        self.heard().into_iter().rev().find_map(|l| l.strip_prefix("panel idle ").map(String::from)).unwrap_or_default()
    }

    pub fn host_says_camera_on(&self) -> bool {
        !self.w.state.borrow().streams.camera_off
    }
}

fn rows_of(model: &ModelRc<SValue>) -> Vec<BTreeMap<String, String>> {
    model.iter().map(|row| {
        let SValue::Struct(fields) = row else { panic!("not a row") };
        fields.iter().map(|(k, v)| (k.to_string(), match v {
            SValue::String(s) => s.to_string(),
            SValue::Bool(b) => b.to_string(),
            SValue::Number(n) => n.to_string(),
            SValue::Model(m) => m.iter().map(|x| if let SValue::String(s) = x { s.to_string() } else { String::new() }).collect::<Vec<_>>().join("|"),
            _ => String::new(),
        })).collect()
    }).collect()
}

/// A plugin's stream source, a browser in these tests.
#[derive(Default)]
pub struct FakeSource {
    pub id: String,
    pub name: String,
    pub goes_to: Option<String>,
    pub hint: String,
    /// prepare() says no, and something is on its way to it.
    pub waits: bool,
    pub changes_only: bool,
    pub cancels: AtomicU32,
}

impl FakeSource {
    pub fn new(id: &str) -> FakeSource {
        FakeSource { id: id.into(), name: "Browser camera".into(), ..Default::default() }
    }
}

struct NoFeed;

impl FeedStopper for NoFeed {
    fn stop(&self) {}
}

impl VideoFeed for NoFeed {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        Err(FeedEnd::Stopped)
    }
    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(NoFeed)
    }
}

impl StreamSource for FakeSource {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn url(&self) -> String {
        format!("{}://camera", self.id)
    }
    fn prepare(&self, _interactive: bool) -> bool {
        !self.waits
    }
    fn open_feed(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        Ok((Codec::H264, Box::new(NoFeed)))
    }
    fn control_client(&self) -> Option<Box<dyn ControlClient>> {
        None
    }
    fn fps(&self) -> u32 {
        30
    }
    fn redirect(&self) -> Option<String> {
        self.goes_to.clone()
    }
    fn lost_hint(&self) -> String {
        self.hint.clone()
    }
    fn waiting(&self) -> bool {
        self.waits
    }
    fn cancel_wait(&self) {
        self.cancels.fetch_add(1, Ordering::SeqCst);
    }
    fn remember_changes_only(&self) -> bool {
        self.changes_only
    }
}

/// Connection's part in picking a source: the pick moves when the window asks.
pub struct Picker(pub Arc<FakeLink>);

impl Plugin for Picker {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "picker".into(), title: "picker".into(), ..Default::default() }
    }
    fn on_event(&mut self, _host: &mut dyn Host, event: &Event) {
        if let Event::SelectSource { source_id } | Event::SourceShown { source_id } = event {
            *self.0.picked.lock().unwrap() = Some(source_id.clone()).filter(|s| !s.is_empty());
        }
    }
}

/// What shapes the stream: it follows the panels, and each phone has its own numbers.
pub struct Output {
    pub config: Value,
}

impl Plugin for Output {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "stream_output".into(), title: "Output".into(), follows_focus: true, device_local: true, ..Default::default() }
    }
    fn get_config(&self) -> Value {
        self.config.clone()
    }
    fn set_config(&mut self, config: &Value) -> Result<(), ConfigRejected> {
        self.config = config.clone();
        Ok(())
    }
    fn stream_params(&self) -> Option<(Option<u32>, Option<u32>, u32)> {
        let size = |key: &str| self.config[key].as_u64().map(|n| n as u32);
        Some((size("width"), size("height"), size("fps").unwrap_or(30)))
    }
    fn opening(&self) -> Option<StreamRequest> {
        let size = |key: &str| self.config[key].as_u64().unwrap_or(0) as u32;
        Some(StreamRequest { width: size("width"), height: size("height"), fps: size("fps"), camera_off: false })
    }
}

/// A plain banner, for what a test wants up.
pub fn issue(title: &str) -> Issue {
    Issue { title: title.into(), text: String::new(), actions: vec![], kind: "warn".into(), details: String::new(), tell_dismiss: false }
}

/// A phone state that passes the check, with these keys on top.
pub fn state_with(extra: Value) -> Value {
    let mut state = json!({"cameras": [], "auto": true, "wb_manual": false, "ois": true, "focus_mode": "continuous", "focus_distance": 0.0,
                           "ae_comp": 0, "nr_mode": 1, "edge_mode": 1, "black_level_lock": false, "torch": false, "battery": 80,
                           "charging": false, "battery_temp_c": 25.0});
    if let (Some(map), Value::Object(more)) = (state.as_object_mut(), extra) {
        map.extend(more);
    }
    state
}

/// A frame, for what wants one.
pub fn frame() -> Frame {
    Frame::new(2, 2, PixelFormat::Rgba, vec![0; 16])
}
