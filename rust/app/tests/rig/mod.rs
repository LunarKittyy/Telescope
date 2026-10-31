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
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::Host;
use telescope_core::link::{Link, PairingOffer, PhoneControl, PhoneSession, PhoneStatus, Resolution, RoutePrefs, StreamFormat, StreamRequest, StreamTarget, Unreachable};
use telescope_core::media::{Codec, FrameSlot, PacketTap, StagedStep, VideoFeed};
use telescope_core::output::{Outputs, SetupAction, SetupResult, Slot, VideoOutput};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::services::Services;
use telescope_core::ui::Issue;
use telescope_core::source::{ControlClient, PairedPhone, RouteSpec};
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
    pub asked: AtomicU32,
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
        self.asked.fetch_add(1, Ordering::SeqCst);
        while self.stalled.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
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
    release: Condvar,
    /// What a dropped stream's phone looks like, by source id.
    pub recovery: Mutex<BTreeMap<String, Resolution>>,
    /// The openings wakes asked for: (source, what, after_stop).
    pub openings: Mutex<Vec<(String, StreamRequest, bool)>>,
    pub formats: Mutex<Vec<StreamFormat>>,
    pub controls: Mutex<BTreeMap<String, Arc<Control>>>,
    pub released: Mutex<Vec<Arc<AtomicBool>>>,
    pub progress: Mutex<Vec<String>>,
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

    /// This phone's controls.
    pub fn control(&self, id: &str) -> Arc<Control> {
        self.controls.lock().unwrap().entry(id.into()).or_default().clone()
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
            RouteSpec::Network { .. } => phone.id.clone(),
            RouteSpec::Usb { serial } => format!("usb-{serial}"),
        };
        let _ = controls;
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
        while *held {
            held = self.release.wait(held).unwrap();
        }
        drop(held);
        for text in self.progress.lock().unwrap().iter() {
            progress(text);
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
        None
    }
    fn slot(&self) -> Arc<dyn FrameSlot> {
        self.slot.clone()
    }
    fn set_steps(&self, steps: Vec<StagedStep>) {
        self.steps.lock().unwrap().push(steps.iter().map(|s| s.owner.clone()).collect());
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
    fn retarget(&self, url: &str, _opener: Opener) {
        *self.url.lock().unwrap() = url.into();
        note(&self.log, format!("retarget {url}"));
    }
    fn can_hand_over(&self) -> bool {
        !self.refuses_hand_over.load(Ordering::SeqCst)
    }
    fn set_opener(&self, _opener: Opener) {
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
    fn hand_over(&self, url: &str, _opener: Opener) -> bool {
        note(&self.log, format!("hand_over {url}"));
        self.can_hand_over()
    }
}

/// Hears a stream's hooks and events, in order; it follows the panels unless it's the mic.
pub struct Listener {
    pub name: &'static str,
    pub follows: bool,
    pub heard: Log,
    pub config: Value,
}

impl Listener {
    pub fn new(name: &'static str, heard: &Log) -> Listener {
        Listener { name, follows: name != "microphone", heard: heard.clone(), config: json!({}) }
    }
}

impl Plugin for Listener {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: self.name.into(), title: self.name.into(), follows_focus: self.follows, device_local: true, ..Default::default() }
    }
    fn get_config(&self) -> Value {
        self.config.clone()
    }
    fn set_config(&mut self, config: &Value) -> Result<(), ConfigRejected> {
        self.config = config.clone();
        Ok(())
    }
    fn on_stream_start(&mut self, _host: &mut dyn Host, info: &StreamInfo) {
        note(&self.heard, format!("{} start {}", self.name, info.source_id));
    }
    fn on_stream_stop(&mut self, _host: &mut dyn Host) {
        note(&self.heard, format!("{} stop", self.name));
    }
    fn on_focus_gained(&mut self, _host: &mut dyn Host, info: &StreamInfo) {
        note(&self.heard, format!("{} focus {}", self.name, info.source_id));
    }
    fn on_focus_lost(&mut self, _host: &mut dyn Host) {
        note(&self.heard, format!("{} left", self.name));
    }
    fn on_phone_state(&mut self, _host: &mut dyn Host, state: &Value) {
        note(&self.heard, format!("{} state {state}", self.name));
    }
    fn on_camera_off(&mut self, _host: &mut dyn Host) {
        note(&self.heard, format!("{} camera off", self.name));
    }
    fn on_camera_on(&mut self, _host: &mut dyn Host) {
        note(&self.heard, format!("{} camera on", self.name));
    }
    fn on_event(&mut self, _host: &mut dyn Host, event: &Event) {
        match event {
            Event::StreamBehind { source_id, behind } => note(&self.heard, format!("{} behind {source_id} {behind}", self.name)),
            Event::CameraOnChanged { on } => note(&self.heard, format!("{} camera_on_changed {on}", self.name)),
            Event::StreamLost { source_id } => note(&self.heard, format!("{} lost {source_id}", self.name)),
            Event::StreamConnected { source_id } => note(&self.heard, format!("{} connected {source_id}", self.name)),
            Event::StreamStopped => note(&self.heard, format!("{} stream_stopped", self.name)),
            Event::StreamStartFailed => note(&self.heard, format!("{} start_failed", self.name)),
            Event::IdleOutputs { slots } => note(&self.heard, format!("{} idle {slots:?}", self.name)),
            Event::RequestDone { ok, message, .. } => note(&self.heard, format!("{} done {ok} {message}", self.name)),
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
}

impl Outputs for Cameras {
    fn slots(&self) -> Vec<Slot> {
        (0..=self.extras.load(Ordering::SeqCst)).map(|slot| Slot { slot, name: format!("Telescope {slot}"), device: String::new() }).collect()
    }
    fn set_up(&self, action: SetupAction, _progress: &dyn Fn(&str)) -> SetupResult {
        self.asked.lock().unwrap().push(action);
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

/// The window, its link, and the workers it started, oldest first.
pub struct Rig {
    pub w: Rc<Window>,
    pub link: Arc<FakeLink>,
    pub cameras: Arc<Cameras>,
    pub workers: Arc<Mutex<Vec<Arc<FakeWorker>>>>,
    pub heard: Log,
    pub dir: std::path::PathBuf,
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
        let services = Arc::new(Services {
            link: link.clone(), media: Arc::new(telescope_media::FfmpegMedia), outputs: cameras.clone(), audio: None,
            notifier: None, autostart: None, updater: None, tray: None, plugins: Box::new(Vec::new), vision: None,
            plugin_problems: Default::default(), plugin_admin: None,
        });
        let heard: Log = Log::default();
        let list = plugins(&link, &heard);
        let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| list, Some(services), Some(options())).unwrap();
        let workers: Arc<Mutex<Vec<Arc<FakeWorker>>>> = Arc::default();
        let made = workers.clone();
        let maker: WorkerMaker = Rc::new(move |setup: WorkerSetup, slot, events| {
            let worker = Arc::new(FakeWorker {
                url: Mutex::new(setup.url), width: setup.width, height: setup.height, fps: setup.fps, canvas: setup.canvas,
                camera_slot: setup.slot, source: setup.source_id, events, slot, log: Log::default(), stopped: AtomicBool::new(false),
                parked_now: AtomicBool::new(false), tries: AtomicU32::new(0), arrival: Mutex::new(0.0), settled: AtomicU32::new(0),
                steps: Mutex::default(), first_steps: setup.steps.iter().map(|s| s.owner.clone()).collect(), size: Mutex::new(None),
                refuses_hand_over: AtomicBool::new(false),
            });
            made.lock().unwrap().push(worker.clone());
            worker
        });
        *w.streams.maker.borrow_mut() = Some(maker);
        Rig { w, link, cameras, workers, heard, dir, _turn: turn }
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

    /// A banner up, as a plugin would put it.
    pub fn show_issue(&self, key: &str, title: &str) {
        self.w.state.borrow_mut().banners.show(key, issue(title));
        self.w.show_banners();
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
