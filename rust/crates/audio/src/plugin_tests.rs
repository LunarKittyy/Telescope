//! test_microphone.py and the card half of test_vb_cable.py.

use super::*;
use crate::backend::{Problem, WindowsMic};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

/// The window as the plugin sees it: which stream the mic is on and which one the panels show.
#[derive(Default)]
struct TestHost {
    saves: u32,
    card: serde_json::Map<String, Value>,
    tray: Option<TrayAction>,
    events: Vec<Event>,
    main: Option<(String, String)>,
    shown: Option<String>,
    not_streaming: bool,
    streams: u32,
    refuse_main: bool,
    scheduled: Vec<String>,
}

impl TestHost {
    fn prop(&self, k: &str) -> Value {
        self.card.get(k).cloned().unwrap_or(Value::Null)
    }
    fn text(&self, k: &str) -> String {
        self.prop(k).as_str().unwrap_or_default().to_string()
    }
    fn on(&self, k: &str) -> bool {
        self.prop(k).as_bool().unwrap_or(false)
    }
    fn status(&self) -> String {
        self.text("status-text")
    }
}

impl MicHost for TestHost {
    fn schedule_save(&mut self) {
        self.saves += 1;
    }
    fn set_card(&mut self, props: &Value) {
        for (k, v) in props.as_object().into_iter().flatten() {
            self.card.insert(k.clone(), v.clone());
        }
    }
    fn update_tray(&mut self, action: &TrayAction) {
        self.tray = Some(action.clone());
    }
    fn emit(&mut self, event: Event) {
        self.events.push(event);
    }
    fn main_stream(&self) -> Option<(String, String)> {
        self.main.clone()
    }
    fn focused_source_id(&self) -> Option<String> {
        self.shown.clone()
    }
    fn is_streaming_from(&self, _source_id: Option<&str>) -> bool {
        !self.not_streaming
    }
    fn stream_count(&self) -> u32 {
        self.streams
    }
    fn make_main(&mut self, source_id: &str) -> bool {
        if self.refuse_main {
            return false;
        }
        self.main = Some((source_id.to_string(), source_id.to_string()));
        true
    }
    fn schedule(&mut self, _after_ms: u64, token: &str) {
        self.scheduled.retain(|t| t != token);
        self.scheduled.push(token.into());
    }
    fn cancel_schedule(&mut self, token: &str) {
        self.scheduled.retain(|t| t != token);
    }
}

#[derive(Default, Debug)]
struct WorkerState {
    url: String,
    started: bool,
    stopped: bool,
    muted: bool,
    gain: f32,
    limit: bool,
    auto_level: bool,
    noise_reduction: bool,
    level: (f32, f32, bool),
    handed_over: u32,
}

type Shared<T> = Arc<Mutex<T>>;

struct FakeWorker(Shared<WorkerState>);

impl MicWorker for FakeWorker {
    fn start(&mut self) {
        self.0.lock().unwrap().started = true;
    }
    fn stop(&mut self) {
        self.0.lock().unwrap().stopped = true;
    }
    fn hand_over(&self, url: &str, _open: Opener) {
        let mut s = self.0.lock().unwrap();
        s.url = url.into();
        s.handed_over += 1;
    }
    fn set_gain(&self, gain: f32) {
        self.0.lock().unwrap().gain = gain;
    }
    fn set_muted(&self, on: bool) {
        self.0.lock().unwrap().muted = on;
    }
    fn set_limit(&self, on: bool) {
        self.0.lock().unwrap().limit = on;
    }
    fn set_auto_level(&self, on: bool) {
        self.0.lock().unwrap().auto_level = on;
    }
    fn set_noise_reduction(&self, on: bool) {
        self.0.lock().unwrap().noise_reduction = on;
    }
    fn take_level(&self) -> (f32, f32, bool) {
        self.0.lock().unwrap().level
    }
}

#[derive(Default)]
struct FakeBackend {
    problem: Option<Problem>,
    prepare_err: String,
    prepared: AtomicU32,
    torn_down: AtomicU32,
}

impl MicBackend for FakeBackend {
    fn pick_name(&self) -> String {
        "Telescope Microphone".into()
    }
    fn problem(&self) -> Option<Problem> {
        self.problem.clone()
    }
    fn prepare(&self) -> String {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        self.prepare_err.clone()
    }
    fn open_sink(&self) -> OpenSink {
        Arc::new(|| Err("none".into()))
    }
    fn teardown(&self) {
        self.torn_down.fetch_add(1, Ordering::SeqCst);
    }
}

/// Each source's audio: phones at their route's URL, nothing for a webcam.
#[derive(Default)]
struct Routes(Mutex<HashMap<String, String>>);

impl Routes {
    fn set(&self, sid: &str, url: &str) {
        self.0.lock().unwrap().insert(sid.into(), url.into());
    }
}

impl AudioRoutes for Routes {
    fn audio(&self, source_id: &str) -> Option<(String, Opener)> {
        let url = self.0.lock().unwrap().get(source_id).cloned()?;
        let sid = source_id.to_string();
        Some((url, Arc::new(move || Err(format!("not in tests: {sid}")))))
    }
}

#[derive(Default)]
struct Posts(Mutex<Vec<(String, Value)>>);

impl Poster for Posts {
    fn post(&self, token: &str, data: Value) {
        self.0.lock().unwrap().push((token.into(), data));
    }
}

struct Rig {
    p: MicrophonePlugin,
    h: TestHost,
    made: Shared<Vec<Shared<WorkerState>>>,
    jobs: Shared<Vec<Job>>,
    posts: Arc<Posts>,
    routes: Arc<Routes>,
    clock: Shared<f64>,
    opened: Shared<Vec<String>>,
    /// What each worker was given to open the audio with (how it signs in to the phone).
    openers: Shared<Vec<Opener>>,
}

const WIFI: &str = "https://10.0.0.5:8080/v1/audio";
const USB: &str = "https://127.0.0.1:41000/v1/audio";

impl Rig {
    fn new(backend: Arc<dyn MicBackend>) -> Rig {
        Rig::build(backend, true, "")
    }

    /// inline: jobs run at once (run_job=lambda fn: fn()); otherwise they queue in jobs.
    fn build(backend: Arc<dyn MicBackend>, inline: bool, denoise_problem: &str) -> Rig {
        let made: Shared<Vec<Shared<WorkerState>>> = Default::default();
        let jobs: Shared<Vec<Job>> = Default::default();
        let routes = Arc::new(Routes::default());
        routes.set("phone", WIFI);
        let m = made.clone();
        let openers: Shared<Vec<Opener>> = Default::default();
        let given = openers.clone();
        let make_worker: MakeWorker = Box::new(move |url, open, _sink, _status| {
            let state = Arc::new(Mutex::new(WorkerState { url: url.into(), gain: 1.0, limit: true, ..Default::default() }));
            m.lock().unwrap().push(state.clone());
            given.lock().unwrap().push(open);
            Box::new(FakeWorker(state))
        });
        let j = jobs.clone();
        let run_job: RunJob = if inline { Box::new(|job| job()) } else { Box::new(move |job| j.lock().unwrap().push(job)) };
        let problem = denoise_problem.to_string();
        let clock = Arc::new(Mutex::new(100.0));
        let c = clock.clone();
        let opened: Shared<Vec<String>> = Default::default();
        let o = opened.clone();
        let mut p = MicrophonePlugin::new(backend.clone(), Some(routes.clone() as Arc<dyn AudioRoutes>))
            .with_parts(make_worker, run_job, Box::new(move || problem.clone()))
            .with_meter_clock(Box::new(move || *c.lock().unwrap()))
            .with_link_opener(Box::new(move |url| o.lock().unwrap().push(url.into())));
        let posts = Arc::new(Posts::default());
        p.poster = Some(posts.clone());
        p.nr_problem = (p.denoise_problem)();
        let mut h = TestHost { shown: Some("phone".into()), ..Default::default() };
        p.render(&mut h);
        Rig { p, h, made, jobs, posts, routes, clock, opened, openers }
    }

    /// Hands the plugin what its threads posted, as the window does.
    fn deliver(&mut self) {
        loop {
            let posts: Vec<(String, Value)> = std::mem::take(&mut *self.posts.0.lock().unwrap());
            if posts.is_empty() {
                return;
            }
            for (token, data) in posts {
                self.p.handle_event(&mut self.h, &Event::Posted { owner: NAME.into(), token, data });
            }
        }
    }

    fn run_jobs(&mut self) {
        let jobs: Vec<Job> = std::mem::take(&mut *self.jobs.lock().unwrap());
        for job in jobs {
            job();
        }
        self.deliver();
    }

    fn set_config(&mut self, cfg: Value) {
        self.p.set_config(&cfg).unwrap();
        self.p.catch_up(&mut self.h);
        self.deliver();
    }

    fn toggle(&mut self, on: bool) {
        self.ui("toggled", vec![json!(on)]);
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.p.handle_ui(&mut self.h, &UiEvent { ui: "card".into(), callback: callback.into(), args });
        self.deliver();
    }

    fn event(&mut self, e: Event) {
        self.p.handle_event(&mut self.h, &e);
        self.deliver();
    }

    fn start(&mut self, sid: &str) {
        self.p.stream_started(&mut self.h, sid);
        self.deliver();
    }

    fn stop(&mut self) {
        self.p.stream_stopped(&mut self.h);
        self.deliver();
    }

    fn worker(&self) -> Shared<WorkerState> {
        self.made.lock().unwrap().last().cloned().expect("a worker")
    }

    fn made(&self) -> usize {
        self.made.lock().unwrap().len()
    }

    fn running_urls(&self) -> Vec<String> {
        self.made.lock().unwrap().iter().filter(|w| !w.lock().unwrap().stopped).map(|w| w.lock().unwrap().url.clone()).collect()
    }

    fn status_from_worker(&mut self, kind: &str, text: &str) {
        let gen = self.p.gen;
        self.p.on_worker_status(&mut self.h, gen, kind, text);
    }
}

fn backend() -> Arc<FakeBackend> {
    Arc::new(FakeBackend::default())
}

fn config(enabled: bool, gain: i64, auto: bool, nr: bool) -> Value {
    json!({"enabled": enabled, "gain_db": gain, "auto_level": auto, "noise_reduction": nr})
}

#[test]
fn the_card_is_greyed_out_until_a_phone_is_paired() {
    let mut r = Rig::new(backend());
    assert!(!r.h.on("active"));
    r.event(Event::PhonesChanged { count: 1 });
    assert!(r.h.on("active"));
}

#[test]
fn once_every_plugin_is_in_the_mic_says_again_whether_its_on() {
    let mut r = Rig::new(backend());
    let mut host = telescope_core::testing::FakeHost::new();
    r.p.on_plugins_ready(&mut host);
    assert_eq!(host.events, [Event::MicChanged { enabled: false, muted: false }]);
}

#[test]
fn on_follows_the_stream() {
    let mut r = Rig::new(backend());
    r.toggle(true);
    assert_eq!(r.h.status(), "Starts with the stream.");
    r.start("phone");
    let w = r.worker();
    assert!(w.lock().unwrap().started && w.lock().unwrap().url == WIFI);
    // Signing in is the route's: the worker opens through the opener of the stream's own route, and audio.rs sends the token
    let open = r.openers.lock().unwrap().last().unwrap().clone();
    assert_eq!(open().err().unwrap(), "not in tests: phone");
    r.status_from_worker("ok", "");
    assert_eq!(r.h.status(), ""); // the meter shows it's working
    r.stop();
    assert!(w.lock().unwrap().stopped);
    assert_eq!(r.p.get_config(), config(true, 0, false, false));
}

#[test]
fn audio_follows_the_stream_onto_another_route() {
    let mut r = Rig::new(backend());
    r.toggle(true);
    r.start("phone");
    r.start("phone"); // reconnected over the same route: keep listening
    assert_eq!(r.made(), 1);
    r.routes.set("phone", USB);
    r.start("phone");
    // the same worker moves over without a gap rather than a new one starting from silence
    let w = r.worker();
    assert!(r.made() == 1 && !w.lock().unwrap().stopped && w.lock().unwrap().handed_over == 1);
    assert_eq!(w.lock().unwrap().url, USB);
    r.start("phone");
    assert_eq!(w.lock().unwrap().handed_over, 1); // already there
}

#[test]
fn audio_follows_a_route_change_on_the_same_control_client() {
    let mut r = Rig::new(backend());
    r.routes.set("phone", USB);
    r.toggle(true);
    r.start("phone");
    let usb = r.worker();
    r.routes.set("phone", WIFI); // retargeted onto Wi-Fi while the stream carries on
    r.event(Event::StreamConnected { source_id: "phone".into() });
    let s = usb.lock().unwrap();
    assert!(!s.stopped && s.handed_over == 1 && s.url == WIFI);
}

#[test]
fn switching_off_mid_stream_stops_and_removes_the_virtual_mic() {
    let b = backend();
    let mut r = Rig::new(b.clone());
    r.start("phone");
    assert_eq!(r.made(), 0); // off: nothing listens
    r.toggle(true);
    let w = r.worker();
    r.toggle(false);
    assert!(w.lock().unwrap().stopped && b.torn_down.load(Ordering::SeqCst) == 1);
    assert_eq!(r.h.status(), "");
}

#[test]
fn a_missing_piece_is_shown_with_its_fix_and_nothing_starts() {
    let b = Arc::new(FakeBackend {
        problem: Some(("Needs VB-Audio Virtual Cable (free).".into(), Some(("Get VB-Cable".into(), "https://vb-audio.com/Cable/".into())))),
        ..Default::default()
    });
    let mut r = Rig::new(b);
    r.set_config(json!({"enabled": true}));
    r.start("phone");
    assert_eq!(r.made(), 0);
    assert!(r.h.status().contains("VB-Audio"));
    assert!(r.h.text("fix-text") == "Get VB-Cable" && r.h.on("fix-shown"));
    r.ui("fix-clicked", vec![]);
    assert_eq!(*r.opened.lock().unwrap(), vec!["https://vb-audio.com/Cable/".to_string()]);
}

#[test]
fn a_source_with_no_mic_to_bring_over_starts_nothing_and_says_where_to_pick_it() {
    let mut r = Rig::new(backend());
    r.set_config(json!({"enabled": true}));
    r.start("webcam:usb-c920"); // a webcam: no audio route
    assert_eq!(r.made(), 0);
    assert!(r.h.status().contains("call app"));
}

#[test]
fn a_webcam_picked_greys_the_switch_out_and_says_where_its_mic_is() {
    let mut r = Rig::new(backend());
    r.event(Event::SourceSelected { source_id: "webcam:usb-c920".into() });
    assert_eq!(r.h.text("mic-label"), "Camera mic");
    assert!(!r.h.on("toggle-enabled"));
    assert!(r.h.status().contains("call app"));
    r.event(Event::SourceSelected { source_id: "browser:b1".into() });
    assert!(r.h.text("mic-label") == "Browser mic" && r.h.on("toggle-enabled"));
    r.event(Event::SourceSelected { source_id: String::new() });
    assert_eq!(r.h.text("mic-label"), "Phone mic");
}

#[test]
fn phone_refusal_is_shown() {
    let mut r = Rig::new(backend());
    r.set_config(json!({"enabled": true}));
    r.start("phone");
    r.status_from_worker("err", "Allow the microphone in Telescope on the phone");
    assert_eq!(r.h.status(), "Allow the microphone in Telescope on the phone.");
    assert_eq!(r.h.text("status-kind"), "status_err");
}

#[test]
fn setup_failure_is_shown() {
    let b = Arc::new(FakeBackend { prepare_err: "Couldn't create the virtual microphone: no pulse".into(), ..Default::default() });
    let mut r = Rig::new(b);
    r.set_config(json!({"enabled": true}));
    r.start("phone");
    assert!(r.made() == 0 && r.h.status().contains("no pulse"));
}

/// A backend whose setup and install fail with an error, as Python's raised exceptions did.
struct Crashing;

impl MicBackend for Crashing {
    fn pick_name(&self) -> String {
        "Telescope Microphone".into()
    }
    fn problem(&self) -> Option<Problem> {
        None
    }
    fn prepare(&self) -> String {
        panic!("pactl is not responding")
    }
    fn open_sink(&self) -> OpenSink {
        Arc::new(|| Err("none".into()))
    }
    fn teardown(&self) {}
    fn install(&self) -> String {
        panic!("no disk")
    }
}

#[test]
fn a_setup_or_install_that_fails_with_an_error_says_so() {
    let mut r = Rig::new(Arc::new(Crashing));
    r.set_config(json!({"enabled": true}));
    r.start("phone");
    assert_eq!(r.h.status(), "Couldn't create the virtual microphone: pactl is not responding");
    r.p.action_url = INSTALL.into();
    r.ui("fix-clicked", vec![]);
    r.ui("confirm-answered", vec![json!(true)]);
    assert_eq!(r.h.status(), "Couldn't install VB-Cable: no disk");
}

#[test]
fn shutdown_stops_and_tears_down() {
    let b = backend();
    let mut r = Rig::new(b.clone());
    r.set_config(json!({"enabled": true}));
    r.start("phone");
    r.p.shutdown(&mut telescope_core::testing::FakeHost::new());
    assert!(r.worker().lock().unwrap().stopped && b.torn_down.load(Ordering::SeqCst) == 1);
}

#[test]
fn the_card_calls_the_mic_by_the_source_picked() {
    let mut r = Rig::new(backend());
    assert!(r.h.text("mic-label") == "Phone mic" && r.h.text("toggle-tip").contains("phone"));
    r.event(Event::SourceSelected { source_id: "browser:abc".into() });
    assert!(r.h.text("mic-label") == "Browser mic" && r.h.text("toggle-tip").contains("browser"));
    assert!(!r.h.text("gain-tip").contains("phone"));
    r.event(Event::SourceSelected { source_id: String::new() });
    assert!(r.h.text("mic-label") == "Phone mic" && r.h.text("gain-tip").contains("phone"));
}

#[test]
fn setup_runs_off_the_ui_thread_and_a_late_result_is_ignored() {
    let b = backend();
    let mut r = Rig::build(b.clone(), false, "");
    r.set_config(json!({"enabled": true}));
    r.start("phone");
    assert!(b.prepared.load(Ordering::SeqCst) == 0 && r.h.status() == "Setting up…"); // queued, not run here
    r.stop(); // the stream ends before setup finishes
    r.run_jobs(); // setup finishes late
    assert!(r.made() == 0 && b.prepared.load(Ordering::SeqCst) == 1);
    r.start("phone");
    r.run_jobs();
    assert!(r.worker().lock().unwrap().started);
}

#[test]
fn switching_off_queues_teardown_after_the_stop() {
    let b = backend();
    let mut r = Rig::build(b.clone(), false, "");
    r.start("phone");
    r.toggle(true);
    r.run_jobs();
    let w = r.worker();
    r.toggle(false);
    assert!(!w.lock().unwrap().stopped && b.torn_down.load(Ordering::SeqCst) == 0); // nothing ran on the UI thread
    r.run_jobs();
    assert!(w.lock().unwrap().stopped && b.torn_down.load(Ordering::SeqCst) == 1);
}

#[test]
fn mute_writes_silence_without_dropping_the_mic_and_isnt_saved() {
    let mut r = Rig::new(backend());
    assert!(!r.h.on("here")); // off: nothing to mute
    r.toggle(true);
    assert!(r.h.on("here"));
    r.ui("mute-toggled", vec![json!(true)]); // before the stream: the worker starts muted
    r.start("phone");
    let w = r.worker();
    assert!(w.lock().unwrap().muted && !w.lock().unwrap().stopped);
    assert!(r.h.text("mute-text") == "Unmute" && r.h.prop("meter")["readout"] == "Muted");
    r.ui("mute-toggled", vec![json!(false)]);
    assert!(!w.lock().unwrap().muted && r.h.text("mute-text") == "Mute");
    assert_eq!(r.p.get_config(), config(true, 0, false, false));
}

#[test]
fn meter_follows_the_worker_and_clears_when_the_stream_stops() {
    let mut r = Rig::new(backend());
    r.set_config(json!({"enabled": true}));
    r.start("phone");
    assert!(r.h.scheduled.contains(&T_METER.to_string()));
    r.worker().lock().unwrap().level = (0.5, 0.2, false);
    r.event(Event::TimerFired { owner: NAME.into(), token: T_METER.into() });
    assert_eq!(r.h.prop("meter")["readout"], "-6 dB");
    r.stop();
    assert!(!r.h.scheduled.contains(&T_METER.to_string()) && r.h.prop("meter")["readout"] == "");
}

#[test]
fn gain_reaches_the_worker_and_is_saved_per_phone() {
    let mut r = Rig::new(backend());
    r.set_config(json!({"enabled": true, "gain_db": 6}));
    assert!(r.h.text("gain-text") == "+6 dB" && r.h.on("here"));
    r.start("phone");
    let w = r.worker();
    assert!((w.lock().unwrap().gain - 1.995).abs() < 0.001);
    r.ui("gain-moved", vec![json!(-30)]); // past the end: held at the minimum
    assert!((w.lock().unwrap().gain - 10f32.powf(-24.0 / 20.0)).abs() < 1e-6 && r.h.text("gain-text") == "-24 dB");
    assert_eq!(r.p.get_config(), config(true, -24, false, false));
    r.set_config(json!({"enabled": true, "gain_db": "junk"}));
    assert!(w.lock().unwrap().gain == 1.0 && r.h.text("gain-text") == "0 dB");
}

#[test]
fn typing_a_gain_moves_the_slider() {
    let mut r = Rig::new(backend());
    r.set_config(json!({"enabled": true}));
    r.ui("gain-typed", vec![json!("+9")]); // what Enter does after typing +9
    assert!(r.h.prop("gain") == 9 && r.p.get_config()["gain_db"] == 9);
    r.ui("gain-typed", vec![json!("-3 dB")]);
    assert_eq!(r.p.get_config()["gain_db"], -3);
}

#[test]
fn max_gain_from_advanced_sets_the_range_and_pulls_a_higher_gain_down() {
    let mut r = Rig::new(backend());
    r.h.card.clear();
    r.event(Event::MaxGainChanged { max_gain: 48 }); // Setup's config loads before the card is built
    let card = Plugin::card(&mut r.p).unwrap();
    assert_eq!(card.ui.props["gain-max"], 48);
    r.set_config(json!({"enabled": true, "gain_db": 40}));
    assert!(r.h.prop("gain-max") == 48 && r.p.get_config()["gain_db"] == 40);
    r.event(Event::MaxGainChanged { max_gain: 12 });
    assert!(r.h.prop("gain-max") == 12 && r.p.get_config()["gain_db"] == 12 && r.h.prop("gain") == 12);
}

#[test]
fn tray_mute_follows_the_card_both_ways() {
    let mut r = Rig::new(backend());
    assert!(!r.p.tray_actions()[0].visible); // mic off: nothing to mute
    r.set_config(json!({"enabled": true}));
    let tray = r.h.tray.clone().unwrap();
    assert!(tray.visible && !tray.checked);
    let mut host = telescope_core::testing::FakeHost::new();
    r.p.on_tray_action(&mut host, "microphone.mute");
    assert!(r.p.muted);
    assert!(host.calls.iter().any(|c| c.starts_with("set_ui_props card") && c.contains("\"mute-text\":\"Unmute\"")));
    r.ui("mute-toggled", vec![json!(false)]);
    assert!(!r.h.tray.clone().unwrap().checked);
}

#[test]
fn limiter_switch_from_advanced_reaches_the_worker() {
    let mut r = Rig::new(backend());
    r.event(Event::LimiterChanged { on: false });
    r.set_config(json!({"enabled": true}));
    r.start("phone");
    let w = r.worker();
    assert!(!w.lock().unwrap().limit);
    r.event(Event::LimiterChanged { on: true });
    assert!(w.lock().unwrap().limit);
    w.lock().unwrap().level = (0.89, 0.3, true);
    r.event(Event::TimerFired { owner: NAME.into(), token: T_METER.into() });
    assert!(r.p.meter.limiting() && !r.p.meter.clipping());
    *r.clock.lock().unwrap() += 10.0;
    assert!(!r.p.meter.limiting());
}

// Several streams: one mic, on the stream whose card has it on

/// Two phones streaming, the mic on for the front one, and the panels moved to the back one.
fn two_cams(b: Arc<FakeBackend>) -> Rig {
    let mut r = Rig::new(b);
    r.routes.set("front", "https://10.0.0.5/v1/audio");
    r.routes.set("back", "https://10.0.0.6/v1/audio");
    r.h.streams = 2;
    r.h.shown = Some("front".into());
    r.event(Event::PhonesChanged { count: 2 });
    r.toggle(true); // on for the first
    r.h.main = Some(("front".into(), "Front".into()));
    r.start("front");
    r.h.shown = Some("back".into()); // the panels move to the back camera
    r.event(Event::StreamsChanged { count: 2 });
    r
}

#[test]
fn another_streams_card_shows_its_mic_off_and_turning_it_on_moves_the_mic() {
    let b = backend();
    let mut r = two_cams(b.clone());
    assert!(!r.h.on("on") && r.h.on("toggle-enabled"));
    assert!(!r.h.on("here") && r.h.status().contains("On for Front"));
    assert_eq!(r.running_urls(), vec!["https://10.0.0.5/v1/audio".to_string()]);

    r.toggle(true); // the mic moves to the back camera
    assert_eq!(r.h.main, Some(("back".into(), "back".into())));
    assert!(r.running_urls().is_empty());
    r.stop(); // the window moves the hooks: focus lost on the front one, gained on the back one
    r.start("back");
    assert!(r.h.on("on") && r.h.on("here") && r.h.status() == "Connecting…");
    assert_eq!(r.running_urls(), vec!["https://10.0.0.6/v1/audio".to_string()]);
    assert_eq!(b.torn_down.load(Ordering::SeqCst), 0); // apps keep the same microphone throughout

    r.h.shown = Some("front".into());
    r.event(Event::StreamsChanged { count: 2 });
    assert!(!r.h.on("on") && r.h.status().contains("On for back"));

    r.h.shown = Some("back".into());
    r.event(Event::StreamsChanged { count: 2 });
    r.toggle(false); // off on the one that has it: off, and it stays there
    assert!(r.h.main.as_ref().unwrap().0 == "back" && !r.p.enabled);
    r.h.shown = Some("front".into());
    r.event(Event::StreamsChanged { count: 2 });
    assert!(!r.h.on("on") && r.h.status().is_empty());
    r.toggle(true); // on again, here
    r.stop();
    r.start("front");
    assert!(r.h.main.as_ref().unwrap().0 == "front" && r.p.enabled);
}

#[test]
fn the_mic_stays_on_when_the_stream_with_it_stops() {
    let mut r = two_cams(backend());
    r.h.streams = 1; // the front one stops; the back one carries on
    r.stop();
    assert!(r.p.handing_on);
    r.set_config(json!({"enabled": false})); // the next stream's settings come in, saved with it off
    assert!(r.p.enabled);
    r.h.main = Some(("back".into(), "Back".into()));
    r.start("back");
    assert_eq!(r.running_urls(), vec!["https://10.0.0.6/v1/audio".to_string()]);
}

#[test]
fn a_card_whose_camera_is_still_starting_cant_take_the_mic() {
    let mut r = two_cams(backend());
    r.h.not_streaming = true; // the back one is still starting
    r.event(Event::StreamsChanged { count: 2 });
    assert!(!r.h.on("on") && !r.h.on("toggle-enabled"));
}

#[test]
fn auto_level_reaches_the_worker_and_is_saved_per_phone() {
    let mut r = Rig::new(backend());
    r.set_config(json!({"enabled": true}));
    assert!(!r.h.on("auto-level") && r.h.on("here")); // off until asked for
    r.start("phone");
    let w = r.worker();
    assert!(!w.lock().unwrap().auto_level);
    r.ui("auto-toggled", vec![json!(true)]);
    assert!(w.lock().unwrap().auto_level && r.p.get_config()["auto_level"] == true);
    assert!(r.p.diagnostics()[0].1.contains("auto level"));
    r.set_config(json!({"enabled": true, "auto_level": false}));
    assert!(!w.lock().unwrap().auto_level);
}

#[test]
fn noise_reduction_reaches_the_worker_and_is_saved_per_phone() {
    let mut r = Rig::new(backend());
    r.set_config(json!({"enabled": true}));
    assert!(!r.h.on("nr") && r.h.on("here") && r.h.on("nr-enabled")); // off until asked for
    r.start("phone");
    let w = r.worker();
    assert!(!w.lock().unwrap().noise_reduction);
    r.ui("nr-toggled", vec![json!(true)]);
    assert!(w.lock().unwrap().noise_reduction && r.p.get_config()["noise_reduction"] == true);
    assert!(r.p.diagnostics()[0].1.contains("noise reduction"));
    r.set_config(json!({"enabled": true, "noise_reduction": false}));
    assert!(!w.lock().unwrap().noise_reduction);
}

#[test]
fn without_rnnoise_the_switch_is_greyed_out_with_the_reason_and_the_audio_runs_as_before() {
    let mut r = Rig::build(backend(), true, "The pyrnnoise package isn't installed.");
    r.set_config(json!({"enabled": true, "noise_reduction": true}));
    assert!(!r.h.on("nr-enabled") && !r.h.on("nr"));
    assert_eq!(r.h.text("nr-note"), "The pyrnnoise package isn't installed.");
    r.start("phone");
    let w = r.worker();
    assert!(w.lock().unwrap().started && !w.lock().unwrap().noise_reduction);
    assert_eq!(r.p.get_config()["noise_reduction"], true); // still on for when it can be loaded again
    assert!(r.p.diagnostics()[0].1.contains("noise reduction unavailable"));
}

#[test]
fn diagnostics_say_off_and_on() {
    let mut r = Rig::new(backend());
    assert_eq!(r.p.diagnostics(), vec![("Microphone".to_string(), "off".to_string())]);
    r.set_config(json!({"enabled": true, "gain_db": 6}));
    r.ui("mute-toggled", vec![json!(true)]);
    assert_eq!(r.p.diagnostics()[0].1, "on, not running, gain +6 dB, muted");
}

// Shortcuts

#[test]
fn the_mute_key_toggles_holds_and_says_what_it_did() {
    let mut r = Rig::new(backend());
    let mut host = telescope_core::testing::FakeHost::new();
    assert_eq!(r.p.run_action(&mut host, "microphone.mute", &json!({"mode": "toggle"})), None); // off: nothing to mute
    r.set_config(json!({"enabled": true}));
    assert_eq!(r.p.run_action(&mut host, "microphone.mute", &json!({"mode": "toggle"})).as_deref(), Some("Mute: muted"));
    assert_eq!(r.p.run_action(&mut host, "microphone.mute", &json!({"mode": "off"})).as_deref(), Some("Mute: unmuted"));
    // push to talk while muted: held, it unmutes; let go, it mutes again
    r.p.run_action(&mut host, "microphone.mute", &json!({"mode": "on"}));
    r.p.run_action(&mut host, "microphone.mute", &json!({"mode": "hold"}));
    r.p.run_action(&mut host, "microphone.mute", &json!({"mode": "hold"})); // the key repeats
    assert!(!r.p.muted);
    r.p.release_action(&mut host, "microphone.mute", &json!({"mode": "hold"}));
    assert!(r.p.muted);
}

#[test]
fn the_gain_key_steps_sets_and_resets_within_the_range() {
    let mut r = Rig::new(backend());
    let mut host = telescope_core::testing::FakeHost::new();
    r.set_config(json!({"enabled": true}));
    assert_eq!(r.p.run_action(&mut host, "microphone.gain", &json!({"op": "up", "amount": 3})).as_deref(), Some("Gain: +3 dB"));
    r.p.run_action(&mut host, "microphone.gain", &json!({"op": "set", "value": 40}));
    assert_eq!(r.p.gain_db, 12);
    assert_eq!(r.p.run_action(&mut host, "microphone.gain", &json!({"op": "reset"})).as_deref(), Some("Gain: 0 dB"));
    let actions = r.p.actions();
    assert_eq!(actions.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["microphone.mute", "microphone.on", "microphone.gain"]);
    assert_eq!(actions[0].keywords, "push to talk silence");
}

// VB-Cable from the card (test_vb_cable.py)

fn windows_mic(downloaded: Result<std::path::PathBuf, String>, opened: &'static str, calls: &Shared<Vec<String>>) -> Arc<WindowsMic> {
    let (c1, c2) = (calls.clone(), calls.clone());
    Arc::new(WindowsMic::new(
        Box::new(|| Some(vec![("Speakers (Realtek)".to_string(), 2)])),
        Box::new(move || {
            c1.lock().unwrap().push("download".into());
            downloaded.clone()
        }),
        Box::new(move |setup| {
            c2.lock().unwrap().push(format!("run {}", setup.display()));
            opened.to_string()
        }),
    ))
}

#[test]
fn the_card_offers_the_install_and_asks_first() {
    let calls: Shared<Vec<String>> = Default::default();
    let mut r = Rig::new(windows_mic(Ok("setup.exe".into()), "", &calls));
    r.event(Event::PhonesChanged { count: 1 });
    r.set_config(json!({"enabled": true}));
    r.start("phone");
    assert_eq!(r.h.text("fix-text"), "Install VB-Cable");
    assert!(r.h.status().contains("VB-Audio"));
    r.ui("fix-clicked", vec![]);
    assert!(r.h.on("confirm-open")); // it asks first
    r.ui("confirm-answered", vec![json!(false)]);
    assert!(calls.lock().unwrap().is_empty()); // nothing downloads without a yes
    r.ui("fix-clicked", vec![]);
    r.ui("confirm-answered", vec![json!(true)]);
    assert_eq!(*calls.lock().unwrap(), vec!["download".to_string(), "run setup.exe".into()]);
    assert!(r.h.status().contains("restart the computer"));
    assert_eq!(r.h.text("fix-text"), "Open setup again"); // in case it was closed without installing
    r.ui("fix-clicked", vec![]);
    r.ui("confirm-answered", vec![json!(true)]);
    assert_eq!(calls.lock().unwrap().len(), 4);
}

#[test]
fn a_failed_install_falls_back_to_the_link() {
    let calls: Shared<Vec<String>> = Default::default();
    let backend = windows_mic(Err("Couldn't download VB-Cable: offline".into()), "", &calls);
    let mut r = Rig::new(backend.clone());
    r.event(Event::PhonesChanged { count: 1 });
    r.set_config(json!({"enabled": true}));
    r.start("phone");
    r.ui("fix-clicked", vec![]);
    r.ui("confirm-answered", vec![json!(true)]);
    assert_eq!(r.h.status(), "Couldn't download VB-Cable: offline");
    assert!(r.h.text("fix-text") == "Get VB-Cable" && r.p.action_url == VB_CABLE_URL);
    assert!(!backend.setup_opened.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn a_failure_after_the_mic_was_switched_off_stays_quiet() {
    let calls: Shared<Vec<String>> = Default::default();
    let mut r = Rig::new(windows_mic(Ok("setup.exe".into()), "", &calls));
    r.event(Event::PhonesChanged { count: 1 });
    r.set_config(json!({"enabled": true}));
    r.p.installing = true;
    r.toggle(false);
    r.p.on_installed(&mut r.h, "Couldn't download VB-Cable: offline");
    assert!(r.h.status().is_empty() && !r.p.installing);
}

#[test]
fn the_question_names_vb_audio_and_says_what_they_ask() {
    let mut r = Rig::new(backend());
    let card = r.p.card().unwrap();
    assert_eq!(card.ui.props["origin"], VB_CABLE_ORIGIN);
    assert_eq!(card.ui.props["donationware"], VB_CABLE_DONATIONWARE);
    assert_eq!(card.ui.props["donate-text"], "Donate or read more");
    assert!(UI.contains("@tr(\"Download and install\")"));
    r.ui("donate", vec![]);
    assert_eq!(*r.opened.lock().unwrap(), vec![VB_CABLE_URL.to_string()]);
}

/// A browser's mic as Browser camera offers it.
struct Browsers;

impl telescope_core::audio::SourceAudio for Browsers {
    fn open(&self, source_id: &str) -> Option<Result<Box<dyn telescope_core::audio::AudioFeed>, String>> {
        self.has(source_id).then(|| Err("The browser didn't allow the microphone.".to_string()))
    }
    fn has(&self, source_id: &str) -> bool {
        source_id.starts_with("browser:")
    }
    fn key(&self, _source_id: &str) -> String {
        "browser:/audio".into()
    }
}

#[test]
fn a_source_with_a_mic_of_its_own_comes_before_the_phones_routes() {
    let phones = Arc::new(Routes(Mutex::default()));
    phones.set("phone-1", "https://10.0.0.5/v1/audio");
    let routes = WithSources { sources: Arc::new(Browsers), rest: Some(phones) };
    let (key, open) = routes.audio("browser:b1").unwrap();
    assert_eq!(key, "browser:/audio"); // what the jitter buffer takes for a browser
    assert_eq!(open().err().as_deref(), Some("The browser didn't allow the microphone."));
    assert_eq!(routes.audio("phone-1").unwrap().0, "https://10.0.0.5/v1/audio");
    assert!(routes.audio("nobody").is_none());
}
