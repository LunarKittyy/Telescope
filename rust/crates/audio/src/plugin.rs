//! The Microphone plugin (plugins/microphone.py): the phone's microphone as a microphone on this computer.
//!
//! Per phone. While it's on, it follows the stream: it starts with it and stops with it. Mute keeps the virtual mic
//! and the stream running and writes silence; it isn't saved. Setting up and removing the virtual mic, and stopping the
//! worker, all wait on other processes or threads, so they run in order on one background thread.

use crate::backend::{default_backend, MicBackend, INSTALL};
use crate::meter::{LevelMeter, FLOOR_DB};
use crate::vb_cable::{VB_CABLE_DONATIONWARE, VB_CABLE_ORIGIN, VB_CABLE_URL};
use crate::worker::{AudioWorker, OnStatus, OpenSink, Opener};
use serde_json::{json, Value};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use telescope_core::audio::{AudioFeed, AudioPacket, SourceAudio};
use telescope_core::media::FeedStopper;
use telescope_core::event::Event;
use telescope_core::host::{Host, Poster};
use telescope_core::i18n::{n_, tr, tr_ctx};
use telescope_core::link::{Link, PhoneSession};
use telescope_core::media::FeedEnd;
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::source::RouteSpec;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, TrayAction, UiEvent, UiSpec};

pub const NAME: &str = "microphone";
const UI: &str = include_str!("../ui/microphone.slint");
const NO_MIC_NOTE: &str = n_("This camera's microphone isn't passed on. Pick it in your call app instead.");
pub const METER_MS: u64 = 33;
pub const GAIN_MIN_DB: i32 = -24;
/// Until Advanced says otherwise (max_gain_changed).
pub const GAIN_MAX_DB: i32 = 12;
const T_METER: &str = "meter";
const P_PREPARED: &str = "prepared";
const P_INSTALLED: &str = "installed";
const P_STATUS: &str = "status";
const TRAY_MUTE: &str = "microphone.mute";
const DONATE: &str = n_("<a href=\"{url}\">Donate or read more</a>");

/// The audio worker as the plugin uses it, so tests can stand one in.
pub trait MicWorker: Send {
    fn start(&mut self);
    /// Blocking: runs on the job thread.
    fn stop(&mut self);
    fn hand_over(&self, url: &str, open: Opener);
    fn set_gain(&self, gain: f32);
    fn set_muted(&self, on: bool);
    fn set_limit(&self, on: bool);
    fn set_auto_level(&self, on: bool);
    fn set_noise_reduction(&self, on: bool);
    fn take_level(&self) -> (f32, f32, bool);
}

impl MicWorker for AudioWorker {
    fn start(&mut self) {
        AudioWorker::start(self)
    }
    fn stop(&mut self) {
        AudioWorker::stop(self)
    }
    fn hand_over(&self, url: &str, open: Opener) {
        AudioWorker::hand_over(self, url, open)
    }
    fn set_gain(&self, gain: f32) {
        self.controls().set_gain(gain)
    }
    fn set_muted(&self, on: bool) {
        self.controls().set_muted(on)
    }
    fn set_limit(&self, on: bool) {
        self.controls().set_limit(on)
    }
    fn set_auto_level(&self, on: bool) {
        self.controls().set_auto_level(on)
    }
    fn set_noise_reduction(&self, on: bool) {
        self.controls().set_noise_reduction(on)
    }
    fn take_level(&self) -> (f32, f32, bool) {
        AudioWorker::take_level(self)
    }
}

pub type MakeWorker = Box<dyn Fn(&str, Opener, OpenSink, OnStatus) -> Box<dyn MicWorker> + Send>;
pub type Job = Box<dyn FnOnce() + Send>;
pub type RunJob = Box<dyn Fn(Job) + Send>;

/// Where a stream's microphone is: a key that changes when the stream moves to another route (its URL, whose host
/// says USB or Wi-Fi), and how to open it.
pub trait AudioRoutes: Send + Sync {
    fn audio(&self, source_id: &str) -> Option<(String, Opener)>;
}

/// Through the link: a session of its own on the route the stream holds, so the phone's /v1/audio comes over the same
/// pinned connection.
pub struct LinkAudio(pub Arc<dyn Link>);

/// The phone's audio, keeping its session (and the route's forward) until it's done.
struct SessionFeed {
    feed: Box<dyn AudioFeed>,
    session: Box<dyn PhoneSession>,
}

impl AudioFeed for SessionFeed {
    fn read(&mut self) -> Result<AudioPacket, FeedEnd> {
        self.feed.read()
    }

    fn stopper(&self) -> Option<Box<dyn FeedStopper>> {
        self.feed.stopper()
    }
}

impl Drop for SessionFeed {
    fn drop(&mut self) {
        self.session.release();
    }
}

impl AudioRoutes for LinkAudio {
    fn audio(&self, source_id: &str) -> Option<(String, Opener)> {
        let (phone, route) = self.0.session_target(Some(source_id))?;
        let key = match &route {
            RouteSpec::Usb { serial } => format!("https://127.0.0.1/usb/{serial}/v1/audio"),
            RouteSpec::Network { address } => format!("https://{address}/v1/audio"),
        };
        let link = self.0.clone();
        let open: Opener = Arc::new(move || {
            let session = link.open_route(&phone, &route, "/v1/video.h264", None)?;
            match session.open_audio() {
                Ok(feed) => Ok(Box::new(SessionFeed { feed, session }) as Box<dyn AudioFeed>),
                Err(e) => {
                    session.release();
                    Err(e)
                }
            }
        });
        Some((key, open))
    }
}

/// A source's own mic when it has one, a phone's route otherwise.
struct WithSources {
    sources: Arc<dyn SourceAudio>,
    rest: Option<Arc<dyn AudioRoutes>>,
}

impl AudioRoutes for WithSources {
    fn audio(&self, source_id: &str) -> Option<(String, Opener)> {
        if !self.sources.has(source_id) {
            return self.rest.as_ref()?.audio(source_id);
        }
        let (sources, sid) = (self.sources.clone(), source_id.to_string());
        let open: Opener = Arc::new(move || sources.open(&sid).unwrap_or_else(|| Err(tr("That source has gone."))));
        Some((self.sources.key(source_id), open))
    }
}

/// What the plugin asks of the window, so tests can stand in a small one.
pub trait MicHost {
    fn schedule_save(&mut self);
    fn set_card(&mut self, props: &Value);
    fn update_tray(&mut self, action: &TrayAction);
    fn emit(&mut self, event: Event);
    fn main_stream(&self) -> Option<(String, String)>;
    fn focused_source_id(&self) -> Option<String>;
    fn is_streaming_from(&self, source_id: Option<&str>) -> bool;
    fn stream_count(&self) -> u32;
    fn make_main(&mut self, source_id: &str) -> bool;
    fn schedule(&mut self, after_ms: u64, token: &str);
    fn cancel_schedule(&mut self, token: &str);
}

/// The real window.
pub struct Hosted<'a>(pub &'a mut dyn Host);

impl MicHost for Hosted<'_> {
    fn schedule_save(&mut self) {
        self.0.schedule_save()
    }
    fn set_card(&mut self, props: &Value) {
        self.0.set_ui_props("card", props)
    }
    fn update_tray(&mut self, action: &TrayAction) {
        self.0.update_tray_action(action)
    }
    fn emit(&mut self, event: Event) {
        self.0.emit(event)
    }
    fn main_stream(&self) -> Option<(String, String)> {
        self.0.main_stream()
    }
    fn focused_source_id(&self) -> Option<String> {
        self.0.focused_source_id()
    }
    fn is_streaming_from(&self, source_id: Option<&str>) -> bool {
        self.0.is_streaming_from(source_id)
    }
    fn stream_count(&self) -> u32 {
        self.0.stream_count()
    }
    fn make_main(&mut self, source_id: &str) -> bool {
        self.0.make_main(source_id)
    }
    fn schedule(&mut self, after_ms: u64, token: &str) {
        self.0.schedule(after_ms, token)
    }
    fn cancel_schedule(&mut self, token: &str) {
        self.0.cancel_schedule(token)
    }
}

/// Runs jobs in order on one thread of their own; shutdown waits for what's queued.
pub struct JobThread {
    tx: Mutex<Option<mpsc::Sender<Job>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl JobThread {
    pub fn new() -> Arc<JobThread> {
        let (tx, rx) = mpsc::channel::<Job>();
        let thread = std::thread::Builder::new().name("mic-setup".into()).spawn(move || {
            for job in rx {
                job();
            }
        }).ok();
        Arc::new(JobThread { tx: Mutex::new(Some(tx)), thread: Mutex::new(thread) })
    }

    pub fn run(&self, job: Job) {
        match self.tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(tx) => {
                let _ = tx.send(job);
            }
            None => job(),
        }
    }

    pub fn shutdown(&self) {
        drop(self.tx.lock().unwrap_or_else(|e| e.into_inner()).take());
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }
}

/// Opens a link in the browser: a stand-in until the window offers this to plugins.
pub fn open_url(url: &str) {
    #[cfg(windows)]
    let result = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(not(windows))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = result {
        log::warn!("Couldn't open {url}: {e}");
    }
}

/// The source picked: a browser's mic is called by what it is, and a webcam's isn't brought over.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Phone,
    Browser,
    Other,
}

fn signed_db(db: i32) -> String {
    if db != 0 { format!("{db:+}") } else { "0".into() }
}

pub struct MicrophonePlugin {
    backend: Arc<dyn MicBackend>,
    routes: Option<Arc<dyn AudioRoutes>>,
    make_worker: MakeWorker,
    run_job: RunJob,
    jobs: Option<Arc<JobThread>>,
    open_link: Box<dyn Fn(&str) + Send>,
    /// "" when RNNoise can be loaded, else why not.
    nr_problem: String,
    denoise_problem: Box<dyn Fn() -> String + Send>,
    poster: Option<Arc<dyn Poster>>,
    meter: LevelMeter,
    max_gain_db: i32,
    limit: bool,
    picked: String,
    phones: u32,
    enabled: bool,
    /// set_config came without a host: the enabled state it asked for, applied on the next hook.
    pending: Option<bool>,
    /// Its stream stopped with the mic on while others carry on: the next one gets it on.
    handing_on: bool,
    /// (id, name) of the stream the mic is on, while the panels show another.
    elsewhere: Option<(String, String)>,
    /// The stream it hears about.
    stream: Option<String>,
    worker: Option<Box<dyn MicWorker>>,
    /// Where the running worker listens; the stream can move under it.
    audio_key: String,
    /// Bumped by every start and stop, so a late setup result or status can't revive a stopped mic.
    gen: u64,
    preparing: bool,
    installing: bool,
    muted: bool,
    gain_db: i32,
    auto_level: bool,
    /// What's saved; it only reaches the worker while RNNoise can be loaded.
    noise_reduction: bool,
    /// Shown again when the panels come back to this mic's stream: (text, kind, action).
    said: (String, String, Option<(String, String)>),
    action_url: String,
    readout: String,
    /// Where a held key started, for the "hold" mode (button_action's held dict).
    held_from: Option<bool>,
    held_on_from: Option<bool>,
}

impl Default for MicrophonePlugin {
    fn default() -> Self {
        MicrophonePlugin::new(default_backend(), None)
    }
}

impl MicrophonePlugin {
    /// routes: where streams' audio comes from (LinkAudio in the app).
    pub fn new(backend: Arc<dyn MicBackend>, routes: Option<Arc<dyn AudioRoutes>>) -> MicrophonePlugin {
        let jobs = JobThread::new();
        let j = jobs.clone();
        MicrophonePlugin {
            backend,
            routes,
            make_worker: Box::new(|url, open, sink, status| Box::new(AudioWorker::new(url, open, sink, status))),
            run_job: Box::new(move |job| j.run(job)),
            jobs: Some(jobs),
            open_link: Box::new(open_url),
            nr_problem: String::new(),
            denoise_problem: Box::new(crate::denoise::problem),
            poster: None,
            meter: LevelMeter::default(),
            max_gain_db: GAIN_MAX_DB,
            limit: true,
            picked: String::new(),
            phones: 0,
            enabled: false,
            pending: None,
            handing_on: false,
            elsewhere: None,
            stream: None,
            worker: None,
            audio_key: String::new(),
            gen: 0,
            preparing: false,
            installing: false,
            muted: false,
            gain_db: 0,
            auto_level: false,
            noise_reduction: false,
            said: (String::new(), "dim".into(), None),
            action_url: String::new(),
            readout: String::new(),
            held_from: None,
            held_on_from: None,
        }
    }

    /// The app's: the default backend, audio through the link.
    pub fn with_link(link: Arc<dyn Link>) -> MicrophonePlugin {
        MicrophonePlugin::new(default_backend(), Some(Arc::new(LinkAudio(link))))
    }

    /// Sources that aren't phones bring their own mic (a browser's).
    pub fn with_source_audio(mut self, sources: Arc<dyn SourceAudio>) -> Self {
        self.routes = Some(Arc::new(WithSources { sources, rest: self.routes.take() }));
        self
    }

    /// Stand-ins for tests and scenes.
    pub fn with_parts(mut self, make_worker: MakeWorker, run_job: RunJob, denoise_problem: Box<dyn Fn() -> String + Send>) -> Self {
        self.make_worker = make_worker;
        self.run_job = run_job;
        self.jobs = None;
        self.denoise_problem = denoise_problem;
        self
    }

    /// Why noise reduction isn't available ("" when it is), in place of looking for RNNoise.
    pub fn with_denoise_problem(mut self, problem: Box<dyn Fn() -> String + Send>) -> Self {
        self.denoise_problem = problem;
        self
    }

    pub fn with_meter_clock(mut self, clock: Box<dyn Fn() -> f64 + Send>) -> Self {
        self.meter = LevelMeter::new(clock);
        self
    }

    pub fn with_link_opener(mut self, open: Box<dyn Fn(&str) + Send>) -> Self {
        self.open_link = open;
        self
    }

    fn kind(&self) -> Kind {
        let sid = &self.picked;
        if sid.is_empty() {
            Kind::Phone
        } else if sid == "browser" || sid.starts_with("browser:") {
            Kind::Browser
        } else {
            Kind::Other
        }
    }

    // ── State ─────────────────────────────────────────────────────────────

    fn show(&mut self, h: &mut dyn MicHost, text: &str, kind: &str, action: Option<(String, String)>) {
        self.said = (text.to_string(), kind.to_string(), action.clone());
        let (mut text, mut kind, mut action) = (text.to_string(), kind.to_string(), action);
        if let Some((_, name)) = &self.elsewhere {
            // this card is about another stream's mic: it says where this one is
            text = if self.enabled { tr("On for {name}. Turning it on here turns it off there.").replace("{name}", name) } else { String::new() };
            kind = "dim".into();
            action = None;
        }
        self.action_url = action.as_ref().map(|a| a.1.clone()).unwrap_or_default();
        h.set_card(&json!({
            "status-text": text,
            "status-kind": format!("status_{kind}"),
            "fix-text": action.as_ref().map(|a| a.0.clone()).unwrap_or_default(),
            "fix-shown": action.is_some(),
        }));
    }

    fn reshow(&mut self, h: &mut dyn MicHost) {
        let (text, kind, action) = self.said.clone();
        self.show(h, &text, &kind, action);
    }

    fn here(&self) -> bool {
        self.enabled && self.elsewhere.is_none()
    }

    fn show_gain(&mut self, h: &mut dyn MicHost) {
        h.set_card(&json!({"gain": self.gain_db, "gain-max": self.max_gain_db, "gain-min": GAIN_MIN_DB,
                           "gain-text": format!("{} dB", signed_db(self.gain_db))}));
    }

    fn tray(&self) -> TrayAction {
        TrayAction { id: TRAY_MUTE.into(), title: tr("Mute microphone"), checkable: true, checked: self.muted, visible: self.enabled }
    }

    fn show_mute(&mut self, h: &mut dyn MicHost) {
        h.set_card(&json!({
            "here": self.here(),
            "muted": self.muted,
            "mute-text": if self.muted { tr("Unmute") } else { tr("Mute") },
            "mute-tip": if self.muted { tr("Apps hear the mic again.") } else { tr("Apps hear silence, but the microphone stays connected.") },
        }));
        h.update_tray(&self.tray());
        self.meter.muted = self.muted;
        self.show_readout(h);
        h.emit(Event::MicChanged { enabled: self.enabled, muted: self.muted });
    }

    fn show_readout(&mut self, h: &mut dyn MicHost) {
        let red = self.muted || self.meter.clipping();
        let text = if self.muted {
            tr("Muted")
        } else if self.worker.is_none() || self.meter.held_db() <= FLOOR_DB {
            String::new()
        } else {
            format!("{} dB", self.meter.held_db().round_ties_even() as i64)
        };
        self.readout = text.clone();
        let mut meter = self.meter.props();
        meter["readout"] = json!(text);
        meter["red"] = json!(red);
        h.set_card(&json!({"meter": meter}));
    }

    fn on_max_gain(&mut self, h: &mut dyn MicHost, db: i32) {
        self.max_gain_db = db;
        if self.gain_db > db {
            self.set_gain(h, db); // a gain past the new end comes down to it
        }
        self.show_gain(h);
    }

    fn set_gain(&mut self, h: &mut dyn MicHost, db: i32) {
        let db = db.clamp(GAIN_MIN_DB, self.max_gain_db);
        if db == self.gain_db {
            self.show_gain(h);
            return;
        }
        self.gain_db = db;
        if let Some(w) = &self.worker {
            w.set_gain(10f32.powf(db as f32 / 20.0));
        }
        self.show_gain(h);
        h.schedule_save();
    }

    fn set_auto_level(&mut self, h: &mut dyn MicHost, on: bool) {
        if on == self.auto_level {
            return;
        }
        self.auto_level = on;
        if let Some(w) = &self.worker {
            w.set_auto_level(on);
        }
        h.set_card(&json!({"auto-level": on}));
        h.schedule_save();
    }

    fn set_noise_reduction(&mut self, h: &mut dyn MicHost, on: bool) {
        if on == self.noise_reduction || !self.nr_problem.is_empty() {
            return;
        }
        self.noise_reduction = on;
        if let Some(w) = &self.worker {
            w.set_noise_reduction(on);
        }
        h.set_card(&json!({"nr": on}));
        h.schedule_save();
    }

    fn set_muted(&mut self, h: &mut dyn MicHost, on: bool) {
        self.muted = on;
        if let Some(w) = &self.worker {
            w.set_muted(on);
        }
        self.show_mute(h);
    }

    fn tick_meter(&mut self, h: &mut dyn MicHost) {
        let Some(w) = &self.worker else { return };
        let (peak, rms, limited) = w.take_level();
        self.meter.set_level(peak as f64, rms as f64, limited);
        self.show_readout(h);
        h.schedule(METER_MS, T_METER);
    }

    fn refresh(&mut self, h: &mut dyn MicHost) {
        self.show_mute(h);
        if self.kind() == Kind::Other {
            self.show(h, &tr(NO_MIC_NOTE), "dim", None);
            return;
        }
        if !self.enabled {
            self.show(h, "", "dim", None);
            return;
        }
        if self.installing {
            return; // says how the download is going until it's done
        }
        if let Some((text, action)) = self.backend.problem() {
            self.show(h, &text, "warn", action);
        } else if self.worker.is_none() && !self.preparing {
            self.show(h, &tr("Starts with the stream."), "dim", None);
        }
    }

    fn on_toggled(&mut self, h: &mut dyn MicHost, on: bool) {
        if self.elsewhere.is_none() {
            self.set_enabled(h, on);
        } else if on {
            self.take_mic(h);
        }
        self.show_toggle(h);
    }

    fn set_enabled(&mut self, h: &mut dyn MicHost, on: bool) {
        if on == self.enabled {
            return;
        }
        self.enabled = on;
        h.schedule_save();
        if on {
            self.refresh(h);
            self.start_worker(h);
        } else {
            self.stop_worker(h);
            let backend = self.backend.clone();
            (self.run_job)(Box::new(move || backend.teardown()));
            self.refresh(h);
        }
    }

    fn show_toggle(&mut self, h: &mut dyn MicHost) {
        let shown = h.focused_source_id();
        let enabled = (self.elsewhere.is_none() || h.is_streaming_from(shown.as_deref())) && self.kind() != Kind::Other;
        h.set_card(&json!({"on": self.here(), "toggle-enabled": enabled}));
    }

    fn on_source_selected(&mut self, h: &mut dyn MicHost, sid: &str) {
        self.picked = sid.to_string();
        self.show_source_words(h);
        self.show_toggle(h);
        self.refresh(h);
    }

    fn source_words(&self) -> Value {
        if self.kind() == Kind::Other {
            return json!({"mic-label": tr("Camera mic"), "toggle-name": tr("Camera microphone"), "toggle-tip": tr(NO_MIC_NOTE),
                          "gain-tip": ""});
        }
        let phone = self.kind() == Kind::Phone;
        let name = self.backend.pick_name();
        json!({
            "mic-label": if phone { tr("Phone mic") } else { tr("Browser mic") },
            "toggle-name": if phone { tr("Phone microphone") } else { tr("Browser microphone") },
            "toggle-tip": if phone {
                tr("Use the phone's microphone on this computer while streaming. Apps list it as “{name}”.")
            } else {
                tr("Use the browser's microphone on this computer while streaming. Apps list it as “{name}”.")
            }.replace("{name}", &name),
            "gain-tip": if phone {
                tr("Makes the phone mic louder or quieter for other apps. The range is in Advanced.")
            } else {
                tr("Makes the browser mic louder or quieter for other apps. The range is in Advanced.")
            },
        })
    }

    fn show_source_words(&mut self, h: &mut dyn MicHost) {
        let words = self.source_words();
        h.set_card(&words);
    }

    /// Whether the panels show the stream the mic is on: with several, the card is about the shown one's.
    fn sync_place(&mut self, h: &mut dyn MicHost) {
        let main = h.main_stream();
        let focused = h.focused_source_id();
        let elsewhere = main.filter(|m| Some(&m.0) != focused.as_ref());
        if elsewhere != self.elsewhere {
            self.elsewhere = elsewhere;
            self.reshow(h);
            self.show_mute(h);
        }
        self.show_toggle(h);
    }

    /// On for the stream the panels show: the mic moves over from the one it was on, which keeps it off.
    fn take_mic(&mut self, h: &mut dyn MicHost) {
        let was = self.enabled;
        self.enabled = false; // saved like this with the stream it leaves; its stop hook stops the worker
        let focused = h.focused_source_id().unwrap_or_default();
        if !h.make_main(&focused) {
            self.enabled = was;
            self.show_toggle(h);
            return;
        }
        // The window moves the stream hooks over (focus lost there, gained here); until then nothing listens
        self.stream = None;
        self.stop_worker(h);
        self.sync_place(h);
        self.set_enabled(h, true);
        self.show_toggle(h);
    }

    fn start_worker(&mut self, h: &mut dyn MicHost) {
        if self.worker.is_some() || self.preparing || self.stream.is_none() || !self.enabled {
            return;
        }
        if self.backend.problem().is_some() {
            return;
        }
        if self.audio_route().is_none() {
            // a webcam: its mic is picked in the call app
            self.show(h, &tr(NO_MIC_NOTE), "dim", None);
            return;
        }
        self.gen += 1;
        self.preparing = true;
        self.show(h, &tr("Setting up…"), "dim", None);
        let (gen, backend, poster) = (self.gen, self.backend.clone(), self.poster.clone());
        (self.run_job)(Box::new(move || {
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| backend.prepare())).unwrap_or_else(|e| {
                log::error!("Microphone setup failed: {}", panic_text(&e));
                tr("Couldn't create the virtual microphone: {error}").replace("{error}", &panic_text(&e))
            });
            if let Some(p) = poster {
                p.post(P_PREPARED, json!({"gen": gen, "error": err}));
            }
        }));
    }

    fn audio_route(&self) -> Option<(String, Opener)> {
        let sid = self.stream.as_ref()?;
        self.routes.as_ref()?.audio(sid)
    }

    fn on_prepared(&mut self, h: &mut dyn MicHost, gen: u64, err: &str) {
        if gen != self.gen {
            return; // stopped or restarted meanwhile
        }
        self.preparing = false;
        if !err.is_empty() {
            self.show(h, err, "err", None);
            return;
        }
        if self.stream.is_none() || !self.enabled {
            self.refresh(h);
            return;
        }
        let Some((key, open)) = self.audio_route() else {
            self.show(h, &tr(NO_MIC_NOTE), "dim", None);
            return;
        };
        self.show(h, &tr("Connecting…"), "dim", None);
        self.audio_key = key.clone();
        let poster = self.poster.clone();
        let status: OnStatus = Arc::new(move |kind, text| {
            if let Some(p) = &poster {
                p.post(P_STATUS, json!({"gen": gen, "kind": kind, "text": text}));
            }
        });
        let mut worker = (self.make_worker)(&key, open, self.backend.open_sink(), status);
        worker.set_gain(10f32.powf(self.gain_db as f32 / 20.0));
        worker.set_muted(self.muted);
        worker.set_limit(self.limit);
        worker.set_auto_level(self.auto_level);
        worker.set_noise_reduction(self.noise_reduction && self.nr_problem.is_empty());
        worker.start();
        self.worker = Some(worker);
        h.schedule(METER_MS, T_METER);
    }

    fn stop_worker(&mut self, h: &mut dyn MicHost) {
        self.gen += 1;
        self.preparing = false;
        let worker = self.worker.take();
        h.cancel_schedule(T_METER);
        self.meter.clear();
        self.show_readout(h);
        if let Some(mut w) = worker {
            (self.run_job)(Box::new(move || w.stop()));
        }
    }

    fn on_worker_status(&mut self, h: &mut dyn MicHost, gen: u64, kind: &str, text: &str) {
        if self.worker.is_none() || gen != self.gen {
            return; // a late report from a worker that was stopped
        }
        if kind == "ok" {
            self.show(h, "", "dim", None); // the meter shows it's working
        } else {
            let text = if text.ends_with('.') { text.to_string() } else { format!("{text}.") };
            self.show(h, &text, "err", None);
        }
    }

    fn open_action(&mut self, h: &mut dyn MicHost) {
        if self.action_url == INSTALL {
            if !self.installing {
                h.set_card(&json!({"confirm-open": true}));
            }
        } else if !self.action_url.is_empty() {
            (self.open_link)(&self.action_url);
        }
    }

    fn install(&mut self, h: &mut dyn MicHost) {
        if self.installing {
            return;
        }
        self.installing = true;
        self.show(h, &tr("Downloading VB-Cable from vb-audio.com…"), "dim", None);
        let (backend, poster) = (self.backend.clone(), self.poster.clone());
        (self.run_job)(Box::new(move || {
            let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| backend.install())).unwrap_or_else(|e| {
                log::error!("VB-Cable install failed: {}", panic_text(&e));
                tr("Couldn't install VB-Cable: {error}").replace("{error}", &panic_text(&e))
            });
            if let Some(p) = poster {
                p.post(P_INSTALLED, json!({"error": err}));
            }
        }));
    }

    fn on_installed(&mut self, h: &mut dyn MicHost, err: &str) {
        self.installing = false;
        if !err.is_empty() && self.enabled {
            self.show(h, err, "err", Some((tr("Get VB-Cable"), VB_CABLE_URL.into())));
        } else {
            self.refresh(h);
        }
    }

    /// The stream it hears about started, came back, or moved to another route.
    fn stream_started(&mut self, h: &mut dyn MicHost, source_id: &str) {
        self.stream = Some(source_id.to_string());
        self.follow_route();
        self.handing_on = false;
        self.start_worker(h);
        self.sync_place(h);
    }

    /// The stream moved to another route and its audio lives there too: follow it without a gap.
    fn follow_route(&mut self) {
        if self.worker.is_none() {
            return;
        }
        let Some((key, open)) = self.audio_route() else { return };
        if key != self.audio_key {
            self.audio_key = key.clone();
            if let Some(w) = &self.worker {
                w.hand_over(&key, open);
            }
        }
    }

    fn stream_stopped(&mut self, h: &mut dyn MicHost) {
        self.handing_on = self.enabled && h.stream_count() > 0;
        self.stream = None;
        self.stop_worker(h);
        self.refresh(h);
        self.sync_place(h);
    }

    /// Everything the card shows, for a fresh card or after set_config.
    fn render(&mut self, h: &mut dyn MicHost) {
        let mut props = self.source_words();
        let nr_tip = if self.nr_problem.is_empty() { tr("Takes out typing, fans and room hiss.") } else { self.nr_problem.clone() };
        props["active"] = json!(self.phones > 0);
        props["auto-level"] = json!(self.auto_level);
        props["nr"] = json!(self.noise_reduction && self.nr_problem.is_empty());
        props["nr-enabled"] = json!(self.nr_problem.is_empty());
        props["nr-note"] = json!(self.nr_problem);
        props["nr-tip"] = json!(nr_tip);
        h.set_card(&props);
        self.show_gain(h);
        self.reshow(h);
        self.show_mute(h);
        self.show_toggle(h);
    }

    /// What set_config asked for, now that there's a host.
    pub fn catch_up(&mut self, h: &mut dyn MicHost) {
        if let Some(on) = self.pending.take() {
            self.render(h);
            self.set_enabled(h, on || self.handing_on);
            self.show_toggle(h);
            self.refresh(h);
        }
    }

    fn handle_event(&mut self, h: &mut dyn MicHost, event: &Event) {
        self.catch_up(h);
        match event {
            Event::Posted { owner, token, data } if owner == NAME => {
                let s = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let gen = data.get("gen").and_then(Value::as_u64).unwrap_or(0);
                match token.as_str() {
                    P_PREPARED => self.on_prepared(h, gen, &s("error")),
                    P_INSTALLED => self.on_installed(h, &s("error")),
                    P_STATUS => self.on_worker_status(h, gen, &s("kind"), &s("text")),
                    _ => {}
                }
            }
            Event::TimerFired { owner, token } if owner == NAME && token == T_METER => self.tick_meter(h),
            Event::MaxGainChanged { max_gain } => self.on_max_gain(h, *max_gain),
            Event::LimiterChanged { on } => {
                self.limit = *on;
                if let Some(w) = &self.worker {
                    w.set_limit(*on);
                }
            }
            Event::DeviceChanged { .. } | Event::StreamsChanged { .. } => self.sync_place(h),
            Event::SourceSelected { source_id } => self.on_source_selected(h, source_id),
            Event::PhonesChanged { count } => {
                self.phones = *count;
                h.set_card(&json!({"active": *count > 0}));
            }
            Event::StreamConnected { source_id } if Some(source_id) == self.stream.as_ref() => self.follow_route(),
            _ => {}
        }
    }

    fn handle_ui(&mut self, h: &mut dyn MicHost, event: &UiEvent) {
        self.catch_up(h);
        let b = |i: usize| event.args.get(i).and_then(Value::as_bool).unwrap_or(false);
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        match event.callback.as_str() {
            "toggled" => self.on_toggled(h, b(0)),
            "mute-toggled" => self.set_muted(h, b(0)),
            "gain-moved" => self.set_gain(h, n(0).round() as i32),
            "gain-typed" => {
                // takes +6, 6, "6 dB" or "+6 dB"
                let text = event.args.first().and_then(Value::as_str).unwrap_or_default();
                match text.replace("dB", "").trim().trim_start_matches('+').parse::<i32>() {
                    Ok(db) => self.set_gain(h, db),
                    Err(_) => self.show_gain(h),
                }
            }
            "auto-toggled" => self.set_auto_level(h, b(0)),
            "nr-toggled" => self.set_noise_reduction(h, b(0)),
            "fix-clicked" => self.open_action(h),
            "meter-clicked" => {
                self.meter.click();
                self.show_readout(h);
            }
            "confirm-answered" => {
                h.set_card(&json!({"confirm-open": false}));
                if b(0) {
                    self.install(h);
                }
            }
            "donate" => (self.open_link)(VB_CABLE_URL),
            _ => {}
        }
    }

    fn control_ready_mute(&self) -> bool {
        self.here()
    }

    fn run(&mut self, h: &mut dyn MicHost, id: &str, params: &Value) -> Option<String> {
        let group_label = |label: String, value: String| tr("{label}: {value}").replace("{label}", &label).replace("{value}", &value);
        match id {
            "microphone.mute" => {
                if !self.control_ready_mute() {
                    return None;
                }
                let mode = params.get("mode").and_then(Value::as_str).unwrap_or("toggle");
                let want = match mode {
                    "on" => true,
                    "off" => false,
                    "hold" => !*self.held_from.get_or_insert(self.muted),
                    _ => !self.muted,
                };
                if want != self.muted {
                    self.set_muted(h, want);
                }
                Some(group_label(tr("Mute"), if self.muted { tr("muted") } else { tr("unmuted") }))
            }
            "microphone.on" => {
                if self.kind() == Kind::Other || (self.elsewhere.is_some() && !h.is_streaming_from(h.focused_source_id().as_deref())) {
                    return None;
                }
                let checked = self.here();
                let mode = params.get("mode").and_then(Value::as_str).unwrap_or("toggle");
                let want = match mode {
                    "on" => true,
                    "off" => false,
                    "hold" => !*self.held_on_from.get_or_insert(checked),
                    _ => !checked,
                };
                if want != checked {
                    self.on_toggled(h, want);
                }
                Some(group_label(tr("Phone mic"), if self.here() { tr("on") } else { tr("off") }))
            }
            "microphone.gain" => {
                if !self.here() {
                    return None;
                }
                let op = params.get("op").and_then(Value::as_str).unwrap_or("up");
                let cur = self.gain_db;
                let new = match op {
                    "set" => params.get("value").and_then(Value::as_f64)?.round() as i32,
                    "reset" => 0,
                    _ => {
                        let amount = params.get("amount").and_then(Value::as_f64).unwrap_or(0.0);
                        let steps = if amount != 0.0 { (amount.round() as i32).max(1) } else { 0 };
                        if op == "up" { cur + steps } else { cur - steps }
                    }
                };
                self.set_gain(h, new);
                Some(group_label(tr("Gain"), format!("{} dB", signed_db(self.gain_db))))
            }
            _ => None,
        }
    }
}

impl MicrophonePlugin {
    fn release(&mut self, h: &mut dyn MicHost, id: &str, params: &Value) {
        if params.get("mode").and_then(Value::as_str) != Some("hold") {
            return;
        }
        match id {
            "microphone.mute" => {
                if let Some(before) = self.held_from.take() {
                    if self.muted != before && self.control_ready_mute() {
                        self.set_muted(h, before);
                    }
                }
            }
            "microphone.on" => {
                if let Some(before) = self.held_on_from.take() {
                    if self.here() != before {
                        self.on_toggled(h, before);
                    }
                }
            }
            _ => {}
        }
    }
}

fn toggle_params(verbs: Option<[(&str, String); 4]>) -> Vec<Value> {
    let generic = [("toggle", tr("Switch on or off")), ("on", tr("Turn on")), ("off", tr("Turn off")),
                   ("hold", tr("Switch while held, back on release"))];
    let modes: Vec<Value> = match verbs {
        Some(v) => v.iter().map(|(m, t)| json!([m, t])).collect(),
        None => generic.iter().map(|(m, t)| json!([m, t])).collect(),
    };
    vec![json!({"name": "mode", "label": tr("Does"), "kind": "choice", "default": "toggle", "choices": modes})]
}

impl Plugin for MicrophonePlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Microphone").into(),
            about: n_("Using the phone's microphone on this computer.").into(),
            when_off: n_("Without it there's no Microphone card or Mute in the tray menu and Telescope doesn't make a virtual microphone.").into(),
            category: "sound".into(),
            icon: "mic".into(),
            device_local: true,
            // stays on one stream (the first, until another one's card takes it) whichever the panels show
            follows_focus: false,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.poster = host.poster();
        self.nr_problem = (self.denoise_problem)();
        let mut h = Hosted(host);
        self.render(&mut h);
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        let mut h = Hosted(host);
        self.catch_up(&mut h);
        h.emit(Event::MicChanged { enabled: self.enabled, muted: self.muted });
    }

    fn shutdown(&mut self, _host: &mut dyn Host) {
        // Quitting may wait: the virtual mic has to be gone before the app is.
        self.gen += 1;
        let worker = self.worker.take();
        if let Some(jobs) = &self.jobs {
            jobs.shutdown();
        }
        if let Some(mut w) = worker {
            w.stop();
        }
        self.backend.teardown();
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.handle_event(&mut Hosted(host), event);
    }

    fn card(&mut self) -> Option<CardSpec> {
        let mut props = self.source_words();
        props["origin"] = json!(VB_CABLE_ORIGIN);
        props["donationware"] = json!(VB_CABLE_DONATIONWARE);
        props["donate-text"] = json!(strip_tags(&tr(DONATE)));
        props["gain-max"] = json!(self.max_gain_db);
        props["gain-min"] = json!(GAIN_MIN_DB);
        props["gain-text"] = json!("0 dB");
        props["mute-text"] = json!(tr("Mute"));
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "MicrophoneCard".into(), props }, region: "left".into() })
    }

    fn tray_actions(&mut self) -> Vec<TrayAction> {
        vec![self.tray()]
    }

    fn on_tray_action(&mut self, host: &mut dyn Host, id: &str) {
        if id == TRAY_MUTE {
            let on = !self.muted;
            self.set_muted(&mut Hosted(host), on);
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Microphone");
        let verbs = [("toggle", tr("Mute or unmute")), ("on", tr("Mute")), ("off", tr("Unmute")),
                     ("hold", tr("Switch while held (push to talk while muted)"))];
        let span = (self.max_gain_db - GAIN_MIN_DB) as f64;
        vec![
            ActionSpec { id: "microphone.mute".into(), label: tr("Mute"), group: group.clone(), params: toggle_params(Some(verbs)),
                         repeats: false, on_release: true, current: None, keywords: tr_ctx("search", "push to talk silence") },
            ActionSpec { id: "microphone.on".into(), label: tr("Phone mic"), group: group.clone(), params: toggle_params(None),
                         repeats: false, on_release: true, current: None, keywords: String::new() },
            ActionSpec {
                id: "microphone.gain".into(), label: tr("Gain"), group,
                params: vec![
                    json!({"name": "op", "label": tr("Does"), "kind": "choice", "default": "up",
                           "choices": [["up", tr("Increase by")], ["down", tr("Decrease by")], ["set", tr("Set to")], ["reset", tr("Reset to default")]]}),
                    json!({"name": "amount", "label": tr("By"), "kind": "number", "default": 3, "minimum": 0, "maximum": span,
                           "decimals": 0, "suffix": " dB", "shown_if": ["op", ["up", "down"]]}),
                    json!({"name": "value", "label": tr("Value"), "kind": "number", "default": null, "minimum": GAIN_MIN_DB,
                           "maximum": self.max_gain_db, "decimals": 0, "suffix": " dB", "shown_if": ["op", ["set"]]}),
                ],
                repeats: true, on_release: false, current: Some(self.gain_db as f64), keywords: tr_ctx("search", "volume loudness"),
            },
        ]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        let mut h = Hosted(host);
        self.catch_up(&mut h);
        self.run(&mut h, id, params)
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        self.release(&mut Hosted(host), id, params);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui == "card" {
            self.handle_ui(&mut Hosted(host), event);
        }
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        let mut h = Hosted(host);
        self.catch_up(&mut h);
        self.stream_started(&mut h, &stream.source_id);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        let mut h = Hosted(host);
        self.catch_up(&mut h);
        self.stream_stopped(&mut h);
    }

    fn get_config(&self) -> Value {
        json!({"enabled": self.pending.unwrap_or(self.enabled), "gain_db": self.gain_db, "auto_level": self.auto_level,
               "noise_reduction": self.noise_reduction})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let gain = match cfg.get("gain_db") {
            Some(Value::Number(n)) => n.as_f64().map_or(0, |f| f as i32),
            Some(Value::String(s)) => s.trim().parse::<i32>().unwrap_or(0),
            Some(Value::Bool(b)) => *b as i32,
            _ => 0,
        };
        // the slider holds it inside the range
        self.gain_db = gain.clamp(GAIN_MIN_DB, self.max_gain_db);
        let truthy = |k: &str| match cfg.get(k) {
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64() != Some(0.0),
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Object(o)) => !o.is_empty(),
            _ => false,
        };
        self.auto_level = truthy("auto_level");
        // kept for when RNNoise can be loaded again
        self.noise_reduction = truthy("noise_reduction");
        if let Some(w) = &self.worker {
            w.set_gain(10f32.powf(self.gain_db as f32 / 20.0));
            w.set_auto_level(self.auto_level);
            w.set_noise_reduction(self.noise_reduction && self.nr_problem.is_empty());
        }
        self.pending = Some(truthy("enabled"));
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let enabled = self.pending.unwrap_or(self.enabled);
        if !enabled {
            return vec![("Microphone".into(), "off".into())];
        }
        let mut text = format!("on, {}", if self.worker.is_some() { "running" } else { "not running" });
        if self.gain_db != 0 {
            text += &format!(", gain {:+} dB", self.gain_db);
        }
        if self.auto_level {
            text += ", auto level";
        }
        if self.noise_reduction {
            text += if self.nr_problem.is_empty() { ", noise reduction" } else { ", noise reduction unavailable" };
        }
        if self.muted {
            text += ", muted";
        }
        vec![("Microphone".into(), text)]
    }
}

/// What a panic said, as Python's str(exception).
fn panic_text(e: &Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
}

/// A link's words without its markup: the card shows it as a button that opens VB-Audio's page.
fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod tests;
