//! What the window streams from (sources.py): a paired phone, reached through link, or a plugin's StreamSource.
//!
//! A phone does what only a phone can: wake its camera, look for another route when the stream drops and report its
//! state. Its blocking calls run on threads of their own and come back to the window on the UI thread (Report).

use crate::session::{Client, Source, SourceKind, SourceRef};
use crate::texts::fill;
use crate::worker::{Opened, Opener};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::i18n::tr;
use telescope_core::link::{Link, PhoneControl, PhoneSession, PhoneStatus, Resolution, StreamFormat, StreamRequest};
use telescope_core::source::{ControlClient, PairedPhone, RouteSpec, StreamSource};
use telescope_core::ui::{IssueAction, IssueDoes};

/// A dropped stream: how often to look for a route back to the phone.
pub const RECOVER_RETRY: Duration = Duration::from_secs(3);
/// A stream gone this long: its virtual camera shows the wait screen, not a frozen frame.
pub const PARK_AFTER: Duration = Duration::from_secs(10);
/// Gone this long: a banner says so.
pub const UNREACHABLE_AFTER: Duration = Duration::from_secs(30);
/// How often a streaming phone is asked whether its heat dropped the size.
pub const HEAT_CHECK: Duration = Duration::from_secs(15);
/// Camera off: how often to check the phone is still there, with no video to tell.
pub const ALIVE_POLL: Duration = Duration::from_secs(5);
/// Checks in a row the phone didn't answer before looking for it like a dropped stream.
pub const ALIVE_MISSES: u32 = 2;
/// After turning the camera on: when to ask the phone whether it came on.
pub const CAMERA_ON_CHECK: Duration = Duration::from_secs(6);
/// The phone's camera rate holds this long, so it isn't asked every report.
pub const CAMERA_FPS_KEEP: Duration = Duration::from_secs(10);
/// Frames arriving at this share of what the phone's camera makes are the camera's doing, not the link's.
pub const CAMERA_LIMITED_SHARE: f32 = 0.9;
/// A wake waits this long at most for the phone's earlier stop to get there.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// A stop that got there this recently may still be winding the phone's stream down.
const STOP_SETTLE: Duration = Duration::from_secs(3);

/// What a phone's thread hands back to the window (the Qt signals of PhoneSource).
pub enum Report {
    WakeProgress { source_id: String, wake_id: u64, text: String },
    /// Err(None): no route to start on, and the Connection plugin says why; Err(Some(reason)): the wake failed.
    WakeDone { source_id: String, wake_id: u64, result: Result<Arc<dyn PhoneSession>, Option<String>> },
    State { source_id: String, session_id: u64, state: Value },
    /// A poll's answer (host.request_phone_state).
    Polled { source_id: String, session_id: u64, state: Value },
    RecoveryProbed { source_id: String, session_id: u64, generation: u64, resolution: Resolution },
    /// recovery moved the stream to a new route: its session there.
    Moved { source_id: String, session_id: u64, generation: u64, route: RouteSpec, session: Arc<dyn PhoneSession> },
    /// A hand-over with the camera off opened its session on url (None: it couldn't).
    HandedOver { source_id: String, session_id: u64, url: String, session: Option<Arc<dyn PhoneSession>> },
    /// The regular heat check's answer.
    Heat { source_id: String, session_id: u64, state: Option<Value> },
    /// How fast the phone's camera goes, for frames arriving slowly (0: it doesn't say).
    CameraRate { source_id: String, session_id: u64, arrival: f32, rates: CameraRates },
    /// Recovery couldn't open the stream on route.
    MoveFailed { source_id: String },
    /// Camera off: whether the phone answered.
    Alive { source_id: String, session_id: u64, ok: bool },
    /// The phone's state after turning the camera on (or None).
    CameraCheck { source_id: String, session_id: u64, state: Option<Value> },
}

/// What a source asks of the window (sources.SourceHost), so what it decides can be tested without one.
pub trait SourceHost {
    fn show_issue(&self, key: &str, title: &str, text: &str, actions: Vec<IssueAction>);
    fn clear_issue(&self, key: &str);
    /// Whether any stream runs.
    fn is_streaming(&self) -> bool;
    /// Whether a start is waking a phone now.
    fn is_waking(&self) -> bool;
    /// A step of source's wake, for its status line.
    fn wake_progress(&self, source: &SourceRef, msg: &str);
    /// Stop source's stream, leaving the phone alone (it's gone, or someone else's now).
    fn stop_source(&self, source: &SourceRef);
    /// Turn the shown stream's camera on or off.
    fn set_camera_on(&self, on: bool);
    /// The camera didn't come back on: source's stream carries on with the mic alone.
    fn camera_stayed_off(&self, source: &SourceRef);
    /// source's stream, with the camera off, is back (or moved to another route).
    fn mic_reconnected(&self, source: &SourceRef);
    /// source's camera is off and its phone stopped answering: look for it like a dropped stream.
    fn lost_contact(&self, source: &SourceRef);
}

pub fn start_action(source_id: Option<String>) -> IssueAction {
    IssueAction { label: tr("Start"), does: IssueDoes::StartAgain { source_id, before: None } }
}

/// The first letter in capitals (i18n.sentence), for a text that starts with a name.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().collect::<String>() + chars.as_str())
}

/// PhoneSource._on_wake_progress: a step of a wake, while the start is still waiting for it.
pub fn wake_progress(host: &dyn SourceHost, source: &SourceRef, wake_id: u64, text: &str) {
    if source.kind.as_phone().is_some_and(|p| p.wake_id() == wake_id) && host.is_waking() {
        host.wake_progress(source, text);
    }
}

/// PhoneSource.on_phone_state: the camera taken by another app, the heat note, and the phone's camera on or off as this
/// stream wants it (a restart keeps it off, and an older phone can't).
pub fn on_phone_state(host: &dyn SourceHost, source: &SourceRef, state: &Value) {
    let Some(session) = source.session() else { return };
    let key = format!("camera_taken:{}", source.id_str());
    if truthy(state, "camera_taken") {
        // Android gives the camera to the app on screen; the phone opens it again once that app lets go
        host.show_issue(&key, &tr("Another app on the phone is using the camera"),
                        &tr("The stream comes back by itself once that app closes or lets go of it."), vec![]);
    } else {
        host.clear_issue(&key);
    }
    show_heat(host, source, state);
    let toggle = truthy(state, "camera_toggle");
    source.state.borrow_mut().camera_toggle = Some(toggle);
    if !toggle {
        if source.camera_off() {
            host.set_camera_on(true);
            host.show_issue("camera", &tr("This phone can't turn its camera off yet"),
                            &tr("Update Telescope on the phone to stream the mic with the camera off."), vec![]);
        }
        return;
    }
    if truthy(state, "camera_off") != source.camera_off() {
        session.client.send(&serde_json::json!({"action": "camera_on", "value": if source.camera_off() { 0 } else { 1 }}));
    }
}

/// Says so while the phone streams a smaller picture than asked because it's critically hot.
pub fn show_heat(host: &dyn SourceHost, source: &SourceRef, state: &Value) {
    let heat_key = format!("heat:{}", source.id_str());
    match heat_text(state) {
        Some(text) => {
            host.show_issue(&heat_key, &tr("The phone is very hot"), &text, vec![]);
            if let Some(p) = source.kind.as_phone() {
                p.note_heat(true);
            }
        }
        None => {
            if source.kind.as_phone().is_some_and(|p| p.note_heat(false)) {
                host.clear_issue(&heat_key);
            }
        }
    }
}

/// PhoneSource._on_recovery_probed: how a dropped stream's look for the phone went.
pub fn recovery_probed(host: &dyn SourceHost, source: &SourceRef, session_id: u64, generation: u64, res: &Resolution) {
    let Some(phone) = source.kind.as_phone() else { return };
    let Some(session) = source.session() else { return };
    if !source.recovering() || generation != phone.recovery_generation() || session.id != session_id {
        return;
    }
    let stopped_key = format!("stopped:{}", source.id_str());
    match res.status {
        PhoneStatus::NotPaired | PhoneStatus::LocalOnly | PhoneStatus::PhoneOutdated | PhoneStatus::DesktopOutdated => {
            // The phone answers but won't take the stream back: say why instead of retrying forever.
            host.stop_source(source);
            phone.show_problem(res);
            return;
        }
        PhoneStatus::Ready if !res.streaming_for.is_empty() => {
            // Another computer started the phone while this one couldn't reach it: that stream is theirs.
            host.stop_source(source);
            let title = if !host.is_streaming() {
                fill(&tr("The phone is streaming to {computer}"), &[("computer", &res.streaming_for)])
            } else {
                sentence(&fill(&tr("{phone} is streaming to {computer}"), &[("phone", &source.kind.name()), ("computer", &res.streaming_for)]))
            };
            host.show_issue(&stopped_key, &title, &tr("Stop it there or on the phone, then start again."), vec![start_action(source.id.clone())]);
            return;
        }
        PhoneStatus::Ready if !res.streaming && !res.busy => {
            // Stopped on the phone, or by its idle watchdog while we couldn't reach it.
            host.stop_source(source);
            let title = if !host.is_streaming() {
                tr("The phone stopped streaming")
            } else {
                sentence(&fill(&tr("{phone} stopped streaming"), &[("phone", &source.kind.name())]))
            };
            host.show_issue(&stopped_key, &title, &tr("Start again when you're ready."), vec![start_action(source.id.clone())]);
            return;
        }
        PhoneStatus::Ready if res.streaming => {
            let tries = session.worker.as_ref().map(|w| w.failed_tries());
            if let Some(route) = res.route.clone() {
                if Some(&route) != phone.recovery_route().as_ref() || phone.forward_failed(tries) {
                    phone.move_to(session_id, route);
                }
            }
        }
        _ => {}
    }
    phone.retry_recovery();
}

/// PhoneSource._on_alive: camera off, whether the phone answered.
pub fn on_alive(host: &dyn SourceHost, source: &SourceRef, session_id: u64, ok: bool) {
    let Some(phone) = source.kind.as_phone() else { return };
    let Some(session) = source.session().filter(|s| s.id == session_id && s.worker.is_none()) else { return };
    drop(session);
    if ok {
        phone.inner.borrow_mut().alive_misses = 0;
        if source.recovering() {
            host.mic_reconnected(source);
        }
        return;
    }
    let misses = {
        let mut inner = phone.inner.borrow_mut();
        inner.alive_misses += 1;
        inner.alive_misses
    };
    if misses >= ALIVE_MISSES && !source.recovering() {
        host.lost_contact(source);
    }
}

/// PhoneSource._on_camera_check: the phone says whether the camera came back on; one that couldn't (in use elsewhere) stays
/// off.
pub fn on_camera_check(host: &dyn SourceHost, source: &SourceRef, session_id: u64, state: Option<&Value>) {
    let Some(state) = state.filter(|s| s.as_object().is_some_and(|o| !o.is_empty())) else { return };
    if source.session().is_none_or(|s| s.id != session_id) || source.camera_off() {
        return;
    }
    if truthy(state, "camera_off") && truthy(state, "camera_error") {
        source.state.borrow_mut().camera_off = true;
        host.camera_stayed_off(source);
        host.show_issue("camera", &tr("The camera didn't turn back on"), &camera_error_text(state),
                        vec![IssueAction { label: tr("Try again"), does: IssueDoes::CameraOnAgain { source_id: source.id.clone() } }]);
    }
}

/// The rates the phone says it runs at: the camera's, what Dynamic stepped down to, and its cap for the heat (0: none).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CameraRates {
    pub camera: f32,
    pub dynamic: u32,
    pub heat: u32,
}

/// What the Stream output card wants of a stream, read at each start.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StreamPlan {
    pub format: StreamFormat,
    /// None without a Stream output plugin.
    pub params: Option<(Option<u32>, Option<u32>, u32)>,
    pub opening: Option<StreamRequest>,
}

pub(crate) fn truthy(state: &Value, key: &str) -> bool {
    state.get(key).is_some_and(|v| match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    })
}

/// A positive whole number the phone sent (a bool isn't one).
fn rate(state: &Value, key: &str) -> u32 {
    state.get(key).filter(|v| !v.is_boolean()).and_then(Value::as_u64).unwrap_or(0) as u32
}

/// What the camera rate question gets out of the phone's state; None state: no answer.
pub fn camera_rates(state: Option<&Value>) -> CameraRates {
    let Some(state) = state else { return CameraRates::default() };
    CameraRates { camera: state.get("camera_fps").and_then(Value::as_f64).unwrap_or(0.0) as f32, dynamic: rate(state, "dynamic_fps"), heat: rate(state, "heat_fps") }
}

/// The phone's state to the window, a moment after the stream began: a phone that's not ready yet is asked again, and one
/// that never answers gets an empty state when `report_failure` says the plugins are waiting for one.
pub(crate) fn fetch_state(controls: &dyn PhoneControl, pause: impl Fn(Duration), report_failure: bool, report: impl Fn(Value)) {
    pause(STATE_WAIT);
    for _ in 0..3 {
        if controls.closed() {
            return;
        }
        if let Some(state) = controls.get_state().filter(|s| s.as_object().is_none_or(|o| !o.is_empty())) {
            return report(state);
        }
        pause(STATE_RETRY);
    }
    if report_failure && !controls.closed() {
        report(Value::Object(Default::default()));
    }
}

/// 0: a phone too old to say, or no answer: the link, as before.
pub fn limited_by_camera(rates: CameraRates, arrival: f32) -> bool {
    rates.camera > 0.0 && arrival >= rates.camera * CAMERA_LIMITED_SHARE
}

/// How long after a stream began its state is asked for, and how long between tries when the phone isn't ready.
const STATE_WAIT: Duration = Duration::from_millis(1500);
const STATE_RETRY: Duration = Duration::from_secs(2);

/// Hands a report to the window's thread.
pub type Post = Arc<dyn Fn(Report) + Send + Sync>;

/// The path of a video URL ("https://host:port/v1/video.h264" -> "/v1/video.h264").
pub fn video_path(url: &str) -> &str {
    url.find("/v1/").map_or("/v1/video.h264", |i| &url[i..])
}

/// Why the phone's camera didn't turn back on, in the current language.
pub fn camera_error_text(state: &Value) -> String {
    if state.get("camera_error_code").and_then(Value::as_str) == Some("camera_in_use") {
        return tr("The camera didn't turn back on. Another app may be using it.");
    }
    state.get("camera_error").map(|e| e.as_str().map_or_else(|| e.to_string(), String::from)).unwrap_or_default()
}

/// The heat banner's text while the phone streams smaller than asked because it's critically hot, or None.
pub fn heat_text(state: &Value) -> Option<String> {
    let int = |k: &str| state.get(k).filter(|v| !v.is_boolean()).and_then(Value::as_i64).filter(|n| *n > 0);
    let (w, h) = (int("heat_width")?, int("heat_height")?);
    let size = format!("{w} × {h}");
    Some(match int("heat_fps") {
        Some(fps) => fill(&tr("It's streaming at {size} and {fps} fps until it cools down."), &[("size", &size), ("fps", &fps.to_string())]),
        None => fill(&tr("It's streaming at {size} until it cools down."), &[("size", &size)]),
    })
}

/// A stream's camera controls (PhoneControlClient), as the flow and the plugins' phone_control use them.
pub struct PhoneClient(pub Option<Arc<dyn PhoneControl>>);

impl Client for PhoneClient {
    fn close(&self) {
        if let Some(c) = &self.0 {
            c.close();
        }
    }
    fn resend_settings(&self) {
        if let Some(c) = &self.0 {
            c.resend_settings();
        }
    }
    fn send(&self, body: &Value) {
        if let Some(c) = &self.0 {
            c.send(body);
        }
    }
}

/// A plugin source's control client.
pub struct PluginClient(pub Option<Arc<dyn ControlClient>>);

impl Client for PluginClient {
    fn close(&self) {}
    fn send(&self, body: &Value) {
        if let Some(c) = self.0.clone() {
            let body = body.clone();
            std::thread::spawn(move || {
                let _ = c.send(&body);
            });
        }
    }
}

#[derive(Default)]
struct PhoneInner {
    wake_id: u64,
    /// The session the latest wake opened, while it streams.
    session: Option<Arc<dyn PhoneSession>>,
    route: Option<RouteSpec>,
    url: String,
    recovery_gen: u64,
    recovery_route: Option<RouteSpec>,
    route_tries: u32,
    /// Recovery is opening the stream on a route: the next look waits for how that went.
    moving: bool,
    /// Checks in a row the phone didn't answer, camera off.
    alive_misses: u32,
    camera_on_session: u64,
    /// A wake that lands after a stop has to be stopped too, unless the stop left the phone streaming on purpose.
    stop_late_wake: bool,
    unreachable_shown: bool,
    heat_shown: bool,
    handover: Option<Handover>,
    /// What the phone said about its rates, while it holds, and whether it's being asked.
    rates: Option<CameraRates>,
    rate_check_busy: bool,
}

/// A hand-over under way: the session it's for, the route and URL it tries, and the session it opened there.
struct Handover {
    session_id: u64,
    route: RouteSpec,
    url: String,
    opened: Arc<Mutex<Option<Arc<dyn PhoneSession>>>>,
}

/// The window's hooks the timers call, with what fired and the source id.
pub type TimerHook = Rc<dyn Fn(&str, &str)>;
/// What the settings want of a stream now.
pub type PlanNow = Rc<dyn Fn() -> StreamPlan>;
/// A plugin's source, swapped in place when it's offered again.
pub type SharedSource = Arc<Mutex<Arc<dyn StreamSource>>>;

/// Stops on their way to a phone, and when the last one got there. A wake waits for them, and so does quitting: whoever
/// waits only looks, so the next one still sees a stop that outlasted its wait.
#[derive(Clone, Default)]
struct Stops {
    running: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    finished: Arc<Mutex<Option<Instant>>>,
}

/// Marks a stop done however its thread ends.
struct Done(Arc<AtomicBool>);

impl Drop for Done {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Stops {
    /// remote: it tells the phone, so a wake right after it goes more carefully.
    fn spawn(&self, remote: bool, f: impl FnOnce() + Send + 'static) {
        let (finished, done) = (self.finished.clone(), Arc::new(AtomicBool::new(false)));
        let mut stops = self.running.lock().unwrap();
        stops.retain(|d| !d.load(Ordering::SeqCst));
        stops.push(done.clone());
        std::thread::spawn(move || {
            let _done = Done(done);
            f();
            if remote {
                *finished.lock().unwrap() = Some(Instant::now());
            }
        });
    }

    /// The quit's last wait: stops still under way are left to end by themselves, and nothing waits for them after this.
    fn drain(&self, timeout: Duration) {
        self.finish(timeout);
        self.running.lock().unwrap().clear();
    }

    /// Waits for the stops under way (timeout at most); whether one got there just now, so the phone may still be stopping.
    fn finish(&self, timeout: Duration) -> bool {
        let running: Vec<_> = self.running.lock().unwrap().iter().filter(|d| !d.load(Ordering::SeqCst)).cloned().collect();
        let waited = !running.is_empty();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && running.iter().any(|d| !d.load(Ordering::SeqCst)) {
            std::thread::sleep(Duration::from_millis(10));
        }
        waited || self.finished.lock().unwrap().is_some_and(|at| at.elapsed() < STOP_SETTLE)
    }
}

/// A paired phone, by id.
pub struct PhoneKind {
    pub id: Option<String>,
    link: Arc<dyn Link>,
    /// What the Stream output card picked, read at each start.
    plan: PlanNow,
    /// What the command line asked for, without a Stream output plugin.
    request: StreamRequest,
    post: Post,
    inner: RefCell<PhoneInner>,
    /// Stops still on their way to the phone; a wake waits for them so it doesn't join the stream they end.
    stopping: Stops,
    /// One recovery state poll at a time: an unreachable phone takes 4 s to time out.
    state_poll_busy: Arc<AtomicBool>,
    park_timer: slint::Timer,
    gone_timer: slint::Timer,
    recovery_timer: slint::Timer,
    heat_timer: slint::Timer,
    alive_timer: slint::Timer,
    camera_on_timer: slint::Timer,
    /// Runs while the phone's rates hold.
    rates_timer: slint::Timer,
    heat_check_busy: Arc<AtomicBool>,
    alive_busy: Arc<AtomicBool>,
    /// The window's hooks the timers call: park(source id), unreachable(source id), probe(source id), heat(source id).
    pub on_timer: TimerHook,
}

impl PhoneKind {
    pub fn new(id: Option<String>, link: Arc<dyn Link>, plan: PlanNow, request: StreamRequest,
               post: Post, on_timer: TimerHook) -> PhoneKind {
        PhoneKind { id, link, plan, request, post, inner: RefCell::default(), stopping: Stops::default(),
                    park_timer: slint::Timer::default(), gone_timer: slint::Timer::default(),
                    recovery_timer: slint::Timer::default(), heat_timer: slint::Timer::default(),
                    alive_timer: slint::Timer::default(), camera_on_timer: slint::Timer::default(),
                    rates_timer: slint::Timer::default(), state_poll_busy: Arc::default(),
                    heat_check_busy: Arc::default(), alive_busy: Arc::default(), on_timer }
    }

    /// The phone as the Connection plugin has it (None id: the picked one).
    fn phone(&self) -> Option<PairedPhone> {
        let id = self.id.clone().or_else(|| self.link.picked_source())?;
        self.link.phones().into_iter().find(|p| p.id == id)
    }

    fn sid(&self) -> String {
        self.id.clone().unwrap_or_default()
    }

    pub fn session(&self) -> Option<Arc<dyn PhoneSession>> {
        self.inner.borrow().session.clone()
    }

    pub fn wake_id(&self) -> u64 {
        self.inner.borrow().wake_id
    }

    /// A wake came back: the session it opened is the stream's now.
    pub fn woke(&self, session: Arc<dyn PhoneSession>) {
        let mut inner = self.inner.borrow_mut();
        inner.url = session.video_url();
        inner.route = Some(session.route());
        inner.session = Some(session);
    }

    /// A wake that finished after a stop: the phone may have started anyway, so it's told to stop, unless that stop meant
    /// it to keep going.
    pub fn stop_late(&self, session: Arc<dyn PhoneSession>) {
        if !self.inner.borrow().stop_late_wake {
            std::thread::spawn(move || session.release());
            return;
        }
        let (link, phone, route) = (self.link.clone(), self.phone(), session.route());
        self.stopping.spawn(true, move || {
            if let Some(phone) = phone {
                link.stop_streaming(&phone, &route);
            }
            session.release();
        });
        if let Some(id) = &self.id {
            self.link.release_stream(id);
        }
    }

    /// Quitting: give stops still on their way until deadline.
    pub fn drain_stops(&self, deadline: std::time::Instant) {
        self.stopping.drain(deadline.saturating_duration_since(Instant::now()));
    }

    /// A hand-over to route begins: the opener the worker tries it with, which opens the phone's session there (passing
    /// the controls on) and keeps it for moved_over. With the camera off (camera_off) the session opens on a thread
    /// straight away and comes back as HandedOver, as there's no video to wait for.
    pub fn begin_hand_over(&self, session_id: u64, route: RouteSpec, url: String, camera_off: bool) -> Option<Opener> {
        let phone = self.phone()?;
        let controls = self.session().and_then(|s| s.controls());
        let opened: Arc<Mutex<Option<Arc<dyn PhoneSession>>>> = Arc::default();
        self.inner.borrow_mut().handover = Some(Handover { session_id, route: route.clone(), url: url.clone(), opened: opened.clone() });
        let link = self.link.clone();
        let open = move || -> Result<Arc<dyn PhoneSession>, String> {
            let mut slot = opened.lock().unwrap();
            if let Some(s) = slot.as_ref() {
                return Ok(s.clone());
            }
            let session: Arc<dyn PhoneSession> = link.open_route(&phone, &route, video_path(&url), controls.clone())?.into();
            *slot = Some(session.clone());
            Ok(session)
        };
        if camera_off {
            let (post, sid, url) = (self.post.clone(), self.sid(), self.inner.borrow().handover.as_ref().map(|h| h.url.clone()).unwrap_or_default());
            std::thread::spawn(move || {
                let session = open().map_err(|e| log::info!("Moving the stream failed: {e}")).ok();
                post(Report::HandedOver { source_id: sid, session_id, url, session });
            });
            return None;
        }
        Some(Arc::new(move || open()?.open_video().map(Opened::from)))
    }

    /// The hand-over to url worked for session_id: the session it opened is the stream's now, and link lets the old
    /// route go. None if it isn't the one under way (a stop or another start came first).
    pub fn moved_over(&self, session_id: u64, url: &str) -> Option<Arc<dyn PhoneSession>> {
        let pending = self.inner.borrow_mut().handover.take()?;
        let session = pending.opened.lock().unwrap().take();
        let Some(session) = session.filter(|_| pending.session_id == session_id && pending.url == url) else {
            self.drop_hand_over(pending, false);
            return None;
        };
        if let Some(id) = &self.id {
            self.link.commit_route(id);
        }
        // The same controls, so their settings carry on; only now, as the new route kept frames coming
        if let Some(controls) = session.controls() {
            controls.retarget(&session.stream_base());
        }
        let mut inner = self.inner.borrow_mut();
        inner.route = Some(pending.route);
        inner.url = session.video_url();
        if let Some(old) = inner.session.replace(session.clone()) {
            std::thread::spawn(move || old.release());
        }
        Some(session)
    }

    /// The hand-over to url didn't happen (failed: the route didn't keep frames coming, so it sits out a while).
    pub fn hand_over_failed(&self, url: &str, failed: bool) {
        let pending = self.inner.borrow_mut().handover.take_if(|h| h.url == url);
        if let Some(pending) = pending {
            self.drop_hand_over(pending, failed);
        }
    }

    pub fn handing_over(&self) -> bool {
        self.inner.borrow().handover.is_some()
    }

    /// Where this stream's phone takes photos: its stream base and the phone's pairing, once it streams.
    pub fn still_target(&self) -> Option<(String, PairedPhone)> {
        let base = self.inner.borrow().session.as_ref()?.stream_base();
        Some((base, self.phone()?))
    }

    fn drop_hand_over(&self, pending: Handover, failed: bool) {
        if let Some(session) = pending.opened.lock().unwrap().take() {
            std::thread::spawn(move || session.release());
        }
        if let Some(id) = &self.id {
            self.link.drop_route(id, failed);
        }
    }

    /// Recovery moved the stream to session's route.
    pub fn moved(&self, route: RouteSpec, session: Arc<dyn PhoneSession>, failed_tries: u32) {
        if let Some(controls) = session.controls() {
            controls.retarget(&session.stream_base()); // the old route is gone, so they follow straight away
        }
        let mut inner = self.inner.borrow_mut();
        inner.moving = false;
        inner.recovery_route = Some(route.clone());
        inner.route_tries = failed_tries;
        inner.alive_misses = 0;
        inner.route = Some(route);
        inner.url = session.video_url();
        if let Some(old) = inner.session.replace(session) {
            std::thread::spawn(move || old.release());
        }
    }

    pub fn recovery_generation(&self) -> u64 {
        self.inner.borrow().recovery_gen
    }

    pub fn recovery_route(&self) -> Option<RouteSpec> {
        self.inner.borrow().recovery_route.clone()
    }

    /// Recovery's move didn't happen: the next look tries again.
    pub fn move_failed(&self) {
        self.inner.borrow_mut().moving = false;
    }

    /// A move that landed after its recovery ended: the stream stays on the route it's on, which goes back to being the one
    /// the link holds for it (the move took that over before it opened anything).
    pub fn move_too_late(&self, session: Arc<dyn PhoneSession>) {
        self.move_failed();
        let (link, id, route) = (self.link.clone(), self.id.clone(), self.inner.borrow().route.clone());
        std::thread::spawn(move || {
            session.release();
            if let Some(route) = route {
                link.adopt_route(id.as_deref(), &route);
            }
        });
    }

    /// The stream couldn't connect over the USB forward recovery made: the same cable back needs a fresh one. failed_tries:
    /// the worker's, None with the camera off, where the mic has no tries of its own to count.
    pub fn forward_failed(&self, failed_tries: Option<u32>) -> bool {
        let inner = self.inner.borrow();
        matches!(inner.recovery_route, Some(RouteSpec::Usb { .. })) && failed_tries.map_or(inner.alive_misses > 0, |t| t > inner.route_tries)
    }

    /// The phone's state, if it says why the stream stopped; the rest can wait for the stream to be back.
    fn poll_codec_state(&self, session_id: u64) {
        let Some(controls) = self.session().and_then(|s| s.controls()) else { return };
        if self.state_poll_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (post, sid, busy) = (self.post.clone(), self.sid(), self.state_poll_busy.clone());
        std::thread::spawn(move || {
            if let Some(state) = controls.get_state().filter(|s| truthy(s, "codec_error") || truthy(s, "camera_taken")) {
                post(Report::State { source_id: sid, session_id, state });
            }
            busy.store(false, Ordering::SeqCst);
        });
    }

    /// Looks for the phone again on a thread; the answer comes back as RecoveryProbed. The phone may have dropped the
    /// stream on purpose (H.264 it can't do at this size): its state says why, and the camera can take seconds to give up
    /// after the stream stops, so that's asked every round.
    pub fn probe(&self, session_id: u64) {
        self.poll_codec_state(session_id);
        let (link, post, id, sid, generation) = (self.link.clone(), self.post.clone(), self.id.clone(), self.sid(), self.recovery_generation());
        std::thread::spawn(move || {
            let resolution = link.recovery_check(id.as_deref());
            post(Report::RecoveryProbed { source_id: sid, session_id, generation, resolution });
        });
    }

    pub fn retry_recovery(&self) {
        let (cb, sid) = (self.on_timer.clone(), self.sid());
        self.recovery_timer.start(slint::TimerMode::SingleShot, RECOVER_RETRY, move || cb("probe", &sid));
    }

    /// Opens the stream on route next to the dropped one, on a thread; Moved comes back.
    pub fn move_to(&self, session_id: u64, route: RouteSpec) {
        let Some(phone) = self.phone() else { return };
        if std::mem::replace(&mut self.inner.borrow_mut().moving, true) {
            return;
        }
        let controls = self.session().and_then(|s| s.controls());
        let (link, post, id, sid, generation) = (self.link.clone(), self.post.clone(), self.id.clone(), self.sid(), self.recovery_generation());
        std::thread::spawn(move || {
            let opened = link.adopt_route(id.as_deref(), &route).ok_or_else(|| "no route".to_string())
                .and_then(|url| link.open_route(&phone, &route, video_path(&url), controls));
            match opened {
                Ok(session) => post(Report::Moved { source_id: sid, session_id, generation, route, session: session.into() }),
                Err(e) => {
                    log::info!("Moving the stream to {route:?} failed: {e}");
                    post(Report::MoveFailed { source_id: sid });
                }
            }
        });
    }

    /// A dropped stream won't come back: the Connection plugin says why, as for a failed Start.
    pub fn show_problem(&self, res: &Resolution) {
        self.link.show_problem(self.id.as_deref(), res);
    }

    /// Asks the phone for its state on a thread, for the plugin that polls it: the answer goes to that plugin alone.
    pub fn ask_state(&self, session_id: u64) {
        let Some(controls) = self.session().and_then(|s| s.controls()) else { return };
        let (post, sid) = (self.post.clone(), self.sid());
        std::thread::spawn(move || {
            if let Some(state) = controls.get_state() {
                post(Report::Polled { source_id: sid, session_id, state });
            }
        });
    }

    /// The phone can get hot mid-stream, and the state is otherwise only asked for at the start.
    pub fn check_heat(&self, session_id: u64) {
        let Some(controls) = self.session().and_then(|s| s.controls()) else { return };
        if self.heat_check_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (post, sid, busy) = (self.post.clone(), self.sid(), self.heat_check_busy.clone());
        std::thread::spawn(move || {
            let state = controls.get_state();
            busy.store(false, Ordering::SeqCst);
            post(Report::Heat { source_id: sid, session_id, state });
        });
    }

    /// With no video coming in, asks the phone whether it's still there.
    pub fn check_alive(&self, session_id: u64) {
        let Some(controls) = self.session().and_then(|s| s.controls()) else { return };
        if self.alive_busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (post, sid, busy) = (self.post.clone(), self.sid(), self.alive_busy.clone());
        std::thread::spawn(move || {
            let ok = controls.get_state().is_some();
            busy.store(false, Ordering::SeqCst);
            post(Report::Alive { source_id: sid, session_id, ok });
        });
    }

    /// Whether the phone answered; true once it missed enough checks in a row to go looking for it.
    pub fn note_alive(&self, ok: bool) -> bool {
        let mut inner = self.inner.borrow_mut();
        inner.alive_misses = if ok { 0 } else { inner.alive_misses + 1 };
        inner.alive_misses >= ALIVE_MISSES
    }

    /// Asks the phone whether the camera came on, once it's had a moment.
    pub fn check_camera_on(&self, session_id: u64) {
        let asked = self.inner.borrow().camera_on_session == session_id;
        let Some(controls) = self.session().and_then(|s| s.controls()).filter(|_| asked) else { return };
        let (post, sid) = (self.post.clone(), self.sid());
        std::thread::spawn(move || {
            let state = controls.get_state();
            post(Report::CameraCheck { source_id: sid, session_id, state });
        });
    }

    /// Whether the camera, not the link, limits frames arriving at this rate, while what the phone said still holds.
    pub fn camera_limits(&self, arrival: f32) -> Option<bool> {
        let rates = self.inner.borrow().rates.filter(|_| self.rates_timer.running())?;
        Some(limited_by_camera(rates, arrival))
    }

    /// Asks the phone how fast its camera goes; the readout and any note wait for the answer, a moment on a working link.
    pub fn ask_camera_rate(&self, session_id: u64, arrival: f32) {
        if std::mem::replace(&mut self.inner.borrow_mut().rate_check_busy, true) {
            return;
        }
        let controls = self.session().and_then(|s| s.controls());
        let (post, sid) = (self.post.clone(), self.sid());
        std::thread::spawn(move || {
            let state = controls.and_then(|c| c.get_state());
            post(Report::CameraRate { source_id: sid, session_id, arrival, rates: camera_rates(state.as_ref()) });
        });
    }

    /// The phone's answer holds for a while; None: it was for a stream that's gone.
    pub fn rates_came(&self, rates: Option<CameraRates>) {
        let mut inner = self.inner.borrow_mut();
        inner.rate_check_busy = false;
        if rates.is_some() {
            inner.rates = rates;
            self.rates_timer.start(slint::TimerMode::SingleShot, CAMERA_FPS_KEEP, || {});
        }
    }

    /// The phone's last answer about its rates, if it gave one.
    pub fn rates_now(&self) -> Option<CameraRates> {
        self.inner.borrow().rates
    }

    /// The rates the phone last said it stepped down to for a slow link and for its heat (0: none).
    pub fn rates(&self) -> CameraRates {
        self.inner.borrow().rates.unwrap_or_default()
    }

    /// Says so while the phone streams a smaller picture than asked because it's critically hot; true if it shows.
    pub fn note_heat(&self, shown: bool) -> bool {
        std::mem::replace(&mut self.inner.borrow_mut().heat_shown, shown)
    }

}

impl SourceKind for PhoneKind {
    fn note_unreachable(&self, shown: bool) -> bool {
        std::mem::replace(&mut self.inner.borrow_mut().unreachable_shown, shown)
    }
    fn lost_hint(&self) -> String {
        tr("Open Telescope on it.")
    }
    fn name(&self) -> String {
        self.phone().map_or_else(|| tr("the phone"), |p| p.name)
    }

    fn url(&self) -> String {
        self.inner.borrow().url.clone()
    }

    fn wakes(&self) -> bool {
        true
    }

    fn prepare(&self, _interactive: bool) -> bool {
        // The route is found on the wake's thread (start_route blocks), and the Connection plugin says why there's none
        true
    }

    fn wake(&self, source: &Source) {
        // Read here, on the UI thread: what the phone should open at, not the size it last used (which may have failed)
        let plan = (self.plan)();
        let wake_id = {
            let mut inner = self.inner.borrow_mut();
            inner.wake_id += 1;
            inner.stop_late_wake = true;
            inner.wake_id
        };
        let format = plan.format;
        let mut opening = plan.opening.unwrap_or(self.request);
        opening.camera_off = source.camera_off();
        let (link, post, id, sid, stopping) = (self.link.clone(), self.post.clone(), self.id.clone(), self.sid(), self.stopping.clone());
        std::thread::spawn(move || {
            // A stop still on its way would land after this wake joined the stream it ends, so the wake goes after it
            let after_stop = stopping.finish(STOP_WAIT);
            let progress_post = post.clone();
            let progress_sid = sid.clone();
            let progress = move |text: &str| progress_post(Report::WakeProgress { source_id: progress_sid.clone(), wake_id, text: text.into() });
            let result = (|| {
                let target = link.start_route(id.as_deref(), format).map_err(|e| {
                    log::info!("No route to start on: {e}");
                    None
                })?;
                let path = video_path(&target.video_url);
                let session: Arc<dyn PhoneSession> = link.open_route(&target.phone, &target.route, path, None).map_err(Some)?.into();
                if let Err(e) = link.ensure_streaming(&target.phone, &target.route, Some(opening), after_stop, &progress) {
                    session.release();
                    return Err(Some(e));
                }
                Ok(session)
            })();
            post(Report::WakeDone { source_id: sid, wake_id, result });
        });
    }

    fn control_client(&self, _source: &Source) -> Rc<dyn Client> {
        Rc::new(PhoneClient(self.session().and_then(|s| s.controls())))
    }

    fn stream_params(&self) -> (Option<u32>, Option<u32>, u32) {
        (self.plan)().params.unwrap_or((None, None, self.request.fps))
    }

    fn started(&self, _source: &Source, session_id: u64) {
        let Some(controls) = self.session().and_then(|s| s.controls()) else { return };
        let (post, sid) = (self.post.clone(), self.sid());
        let _ = std::thread::Builder::new().name("state-fetch".into()).spawn(move || {
            fetch_state(&*controls, std::thread::sleep, true, |state| post(Report::State { source_id: sid.clone(), session_id, state }));
        });
        let (cb, sid) = (self.on_timer.clone(), self.sid());
        self.heat_timer.start(slint::TimerMode::Repeated, HEAT_CHECK, move || cb("heat", &sid));
    }

    fn fetch_state(&self, _source: &Source, session_id: u64) {
        self.ask_state(session_id);
    }

    fn refresh_state(&self, _source: &Source, session_id: u64) {
        // After the panels' settings went out, so it holds them rather than the ones the stream started with
        let Some(controls) = self.session().and_then(|s| s.controls()) else { return };
        let (post, sid) = (self.post.clone(), self.sid());
        let _ = std::thread::Builder::new().name("state-refresh".into()).spawn(move || {
            fetch_state(&*controls, std::thread::sleep, false, |state| post(Report::State { source_id: sid.clone(), session_id, state }));
        });
    }

    fn stopped(&self, _source: &Source, remote_stop: bool, active: bool) {
        self.heat_timer.stop();
        self.alive_timer.stop();
        self.camera_on_timer.stop();
        self.rates_timer.stop();
        let (session, handover) = {
            let mut inner = self.inner.borrow_mut();
            inner.wake_id += 1;
            inner.rates = None;
            inner.moving = false;
            if !remote_stop {
                inner.stop_late_wake = false; // the phone is meant to keep going, so a wake landing late isn't stopped either
            }
            (inner.session.take(), inner.handover.take())
        };
        if let Some(session) = handover.and_then(|h| h.opened.lock().unwrap().take()) {
            std::thread::spawn(move || session.release()); // a stopped worker says nothing more about it
        }
        let target = self.link.session_target(self.id.as_deref())
            .or_else(|| Some((self.phone()?, session.as_ref()?.route())));
        let link = self.link.clone();
        if session.is_some() || (remote_stop && active) {
            self.stopping.spawn(remote_stop && active, move || {
                if let (true, Some((phone, route))) = (remote_stop && active, target) {
                    link.stop_streaming(&phone, &route);
                }
                if let Some(session) = session {
                    session.release();
                }
            });
        }
        self.link.release_stream(&self.sid());
    }

    fn lost(&self, source: &Source) {
        self.inner.borrow_mut().recovery_route = None;
        if let Some(session) = source.session() {
            self.probe(session.id);
        }
    }

    fn start_lost_clock(&self, _source: &Source) {
        let (cb, sid) = (self.on_timer.clone(), self.sid());
        self.park_timer.start(slint::TimerMode::SingleShot, PARK_AFTER, move || cb("park", &sid));
        let (cb, sid) = (self.on_timer.clone(), self.sid());
        self.gone_timer.start(slint::TimerMode::SingleShot, UNREACHABLE_AFTER, move || cb("unreachable", &sid));
    }

    fn end_recovery(&self, source: &Source) {
        source.state.borrow_mut().recovering = false;
        self.park_timer.stop();
        self.gone_timer.stop();
        self.recovery_timer.stop();
        self.inner.borrow_mut().recovery_gen += 1;
        if self.note_unreachable(false) {
            (self.on_timer)("clear_unreachable", &self.sid());
        }
        (self.on_timer)("clear_camera_taken", &self.sid());
    }

    fn forget_stream_status(&self, source: &Source) {
        source.state.borrow_mut().forget_stream_status();
        if self.note_heat(false) {
            (self.on_timer)("clear_heat", &self.sid()); // a stopped stream isn't running small any more
        }
    }

    fn opener(&self) -> Option<Opener> {
        let session = self.session()?;
        Some(Arc::new(move || session.open_video().map(Opened::from)))
    }

    fn mic_alone(&self) -> bool {
        true
    }

    fn watch_alive(&self) {
        self.inner.borrow_mut().alive_misses = 0;
        let (cb, sid) = (self.on_timer.clone(), self.sid());
        self.alive_timer.start(slint::TimerMode::Repeated, ALIVE_POLL, move || cb("alive", &sid));
    }

    fn stop_watching_alive(&self) {
        self.alive_timer.stop();
    }

    fn camera_turned_on(&self, session_id: u64) {
        self.inner.borrow_mut().camera_on_session = session_id;
        let (cb, sid) = (self.on_timer.clone(), self.sid());
        self.camera_on_timer.start(slint::TimerMode::SingleShot, CAMERA_ON_CHECK, move || cb("camera_on_check", &sid));
    }

    fn as_phone(&self) -> Option<&PhoneKind> {
        Some(self)
    }
}

/// A plugin's StreamSource: ready once it is, with nothing to wake.
pub struct PluginKind {
    /// Shared with Streams, so the same source offered again (a new name) reaches a stream that already runs from it.
    pub source: SharedSource,
    /// The window's hook for its lost clock's timers: park(source id), unreachable(source id).
    pub on_timer: Option<TimerHook>,
    pub park_timer: slint::Timer,
    pub gone_timer: slint::Timer,
    pub unreachable_shown: Cell<bool>,
}

impl PluginKind {
    pub fn new(source: SharedSource, on_timer: Option<TimerHook>) -> PluginKind {
        PluginKind { source, on_timer, park_timer: slint::Timer::default(), gone_timer: slint::Timer::default(), unreachable_shown: Cell::default() }
    }

    fn source(&self) -> Arc<dyn StreamSource> {
        self.source.lock().unwrap().clone()
    }
}

impl SourceKind for PluginKind {
    fn name(&self) -> String {
        self.source().name()
    }
    fn url(&self) -> String {
        self.source().url()
    }
    fn auto_canvas(&self) -> Option<(u32, u32)> {
        // A browser's first frame can be any shape (even square), and on Linux apps then keep the camera at that size
        self.source().auto_canvas().or(if cfg!(target_os = "linux") { Some((1920, 1080)) } else { None })
    }
    fn redirect(&self) -> Option<String> {
        self.source().redirect()
    }
    fn waiting(&self) -> bool {
        self.source().waiting()
    }
    fn cancel_wait(&self) {
        self.source().cancel_wait();
    }
    fn prepare(&self, interactive: bool) -> bool {
        self.source().prepare(interactive)
    }
    fn card_decoding(&self) -> bool {
        self.source().card_decoding()
    }
    fn camera_limited(&self, arrival: f32) -> bool {
        self.source().camera_limited(arrival)
    }
    fn lost_hint(&self) -> String {
        Some(self.source().lost_hint()).filter(|h| !h.is_empty()).unwrap_or_else(|| tr("Open the Telescope page in it again."))
    }
    fn start_lost_clock(&self, source: &Source) {
        let Some(hook) = &self.on_timer else { return };
        let (cb, sid) = (hook.clone(), source.id_str().to_string());
        self.park_timer.start(slint::TimerMode::SingleShot, PARK_AFTER, move || cb("park", &sid));
        let (cb, sid) = (hook.clone(), source.id_str().to_string());
        self.gone_timer.start(slint::TimerMode::SingleShot, UNREACHABLE_AFTER, move || cb("unreachable", &sid));
    }
    fn end_recovery(&self, source: &Source) {
        source.state.borrow_mut().recovering = false;
        self.park_timer.stop();
        self.gone_timer.stop();
        if self.unreachable_shown.replace(false) {
            if let Some(hook) = &self.on_timer {
                hook("clear_unreachable", source.id_str());
            }
        }
    }
    fn note_unreachable(&self, shown: bool) -> bool {
        self.unreachable_shown.replace(shown)
    }
    fn control_client(&self, _source: &Source) -> Rc<dyn Client> {
        Rc::new(PluginClient(self.source().control_client().map(Arc::from)))
    }
    fn stream_params(&self) -> (Option<u32>, Option<u32>, u32) {
        (None, None, self.source().fps())
    }
    fn opener(&self) -> Option<Opener> {
        // The source as it is at each open: one offered again under the same id takes over a stream that reconnects
        let shared = self.source.clone();
        Some(Arc::new(move || {
            let source = shared.lock().unwrap().clone();
            let (codec, feed) = source.open_feed()?;
            Ok(Opened { codec, feed, own_decoder: source.decoder() })
        }))
    }
    fn waiting_text(&self) -> Option<crate::worker::WaitingText> {
        let shared = self.source.clone();
        Some(Arc::new(move || {
            let source = shared.lock().unwrap().clone();
            source.waiting_text()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Answers in turn, then nothing; the test closes it to end the session.
    #[derive(Default)]
    struct Answers {
        left: Mutex<Vec<Option<Value>>>,
        asked: std::sync::atomic::AtomicU32,
        closes_after: Option<u32>,
        closed: AtomicBool,
    }

    impl PhoneControl for Answers {
        fn send(&self, _body: &Value) {}
        fn get_state(&self) -> Option<Value> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.left.lock().unwrap().pop().flatten()
        }
        fn retarget(&self, _stream_base: &str) {}
        fn resend_settings(&self) {}
        fn close(&self) {}
        fn closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst) || self.closes_after.is_some_and(|n| self.asked.load(Ordering::SeqCst) >= n)
        }
    }

    fn fetched(controls: &Answers, report_failure: bool) -> Vec<Value> {
        let got = Mutex::new(Vec::new());
        fetch_state(controls, |_| {}, report_failure, |state| got.lock().unwrap().push(state));
        got.into_inner().unwrap()
    }

    #[test]
    fn fetch_state_retries_then_emits_success() {
        let controls = Answers { left: Mutex::new(vec![Some(json!({"auto": true})), None]), ..Default::default() };
        let (got, paused) = (Mutex::new(Vec::new()), Mutex::new(Vec::new()));
        fetch_state(&controls, |wait| paused.lock().unwrap().push(wait), true, |state| got.lock().unwrap().push(state));
        assert_eq!(*got.lock().unwrap(), [json!({"auto": true})]);
        assert_eq!(controls.asked.load(Ordering::SeqCst), 2);
        assert_eq!(*paused.lock().unwrap(), [Duration::from_millis(1500), Duration::from_secs(2)]);
    }

    #[test]
    fn fetch_state_emits_empty_after_three_failures() {
        let controls = Answers::default();
        assert_eq!(fetched(&controls, true), [json!({})]);
        assert_eq!(controls.asked.load(Ordering::SeqCst), 3);
        assert!(fetched(&Answers::default(), false).is_empty(), "nobody's waiting for a state");
    }

    #[test]
    fn fetch_state_exits_if_session_is_removed() {
        let controls = Answers { closes_after: Some(0), ..Default::default() };
        assert!(fetched(&controls, true).is_empty());
        assert_eq!(controls.asked.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn fetch_state_discards_result_from_a_superseded_session() {
        // Closed while it waited to ask again
        let controls = Answers { closes_after: Some(1), ..Default::default() };
        assert!(fetched(&controls, true).is_empty());
        assert_eq!(controls.asked.load(Ordering::SeqCst), 1);
        // A good state is there to be had, and the session goes while it waits to ask
        let controls = Answers { left: Mutex::new(vec![Some(json!({"battery": 10}))]), ..Default::default() };
        let got = Mutex::new(Vec::new());
        fetch_state(&controls, |_| controls.closed.store(true, Ordering::SeqCst), true, |state| got.lock().unwrap().push(state));
        assert!(got.lock().unwrap().is_empty());
    }

    #[test]
    fn the_heat_note_names_the_size_and_rate() {
        assert_eq!(heat_text(&json!({"heat_width": 1280, "heat_height": 720, "heat_fps": 24})).unwrap(),
                   "It's streaming at 1280 × 720 and 24 fps until it cools down.");
        assert_eq!(heat_text(&json!({"heat_width": 1280, "heat_height": 720, "heat_fps": true})).unwrap(),
                   "It's streaming at 1280 × 720 until it cools down.");
        assert!(heat_text(&json!({"heat_width": 0, "heat_height": 720})).is_none());
    }

    #[test]
    fn a_camera_in_use_is_said_in_the_current_language() {
        assert_eq!(camera_error_text(&json!({"camera_error_code": "camera_in_use", "camera_error": "busy"})),
                   "The camera didn't turn back on. Another app may be using it.");
        assert_eq!(camera_error_text(&json!({"camera_error": "Camera 2 failed"})), "Camera 2 failed");
    }

    struct Offered(&'static str);

    impl StreamSource for Offered {
        fn id(&self) -> String {
            "browser:".into()
        }
        fn name(&self) -> String {
            self.0.into()
        }
        fn url(&self) -> String {
            String::new()
        }
        fn prepare(&self, _interactive: bool) -> bool {
            true
        }
        fn open_feed(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
            Err(self.0.into())
        }
        fn control_client(&self) -> Option<Box<dyn ControlClient>> {
            None
        }
        fn fps(&self) -> u32 {
            24
        }
    }

    #[test]
    fn a_reconnect_opens_the_source_as_it_was_offered_last() {
        let first: Arc<dyn StreamSource> = Arc::new(Offered("first"));
        let shared: SharedSource = Arc::new(Mutex::new(first));
        let opener = PluginKind::new(shared.clone(), None).opener().unwrap();
        *shared.lock().unwrap() = Arc::new(Offered("second"));
        assert_eq!(opener().err().as_deref(), Some("second"));
    }

    use telescope_core::link::{PairingOffer, RoutePrefs, Unreachable};
    use telescope_core::media::{Codec, VideoFeed};

    struct FakeSession {
        url: String,
        released: AtomicBool,
        controls: Arc<FakeControl>,
    }

    /// The stream's camera controls, which a hand-over carries along; where they were retargeted.
    #[derive(Default)]
    struct FakeControl(Mutex<Vec<String>>);

    impl PhoneControl for FakeControl {
        fn send(&self, _body: &Value) {}
        fn get_state(&self) -> Option<Value> {
            None
        }
        fn retarget(&self, stream_base: &str) {
            self.0.lock().unwrap().push(stream_base.into());
        }
        fn resend_settings(&self) {}
        fn close(&self) {}
        fn closed(&self) -> bool {
            false
        }
    }

    impl ControlClient for FakeSession {
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

    impl PhoneSession for FakeSession {
        fn route(&self) -> RouteSpec {
            RouteSpec::Network { address: "192.168.1.20".into() }
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
            Err("no video here".into())
        }
        fn release(&self) {
            self.released.store(true, Ordering::SeqCst);
        }
        fn video_url(&self) -> String {
            self.url.clone()
        }
    }

    #[derive(Default)]
    struct FakeLink {
        opened: Mutex<Vec<Arc<FakeSession>>>,
        committed: Mutex<Vec<String>>,
        dropped: Mutex<Vec<(String, bool)>>,
        controls: Arc<FakeControl>,
        released: Mutex<Vec<String>>,
        problems: Mutex<Vec<PhoneStatus>>,
        /// Stops and wakes as they reach the phone, in order.
        reached: Mutex<Vec<String>>,
        /// A stop waits until this is true.
        stop_gate: Option<Arc<(Mutex<bool>, std::sync::Condvar)>>,
        adopted: Mutex<Vec<RouteSpec>>,
    }

    struct Shared(Arc<FakeSession>);

    impl ControlClient for Shared {
        fn send(&self, body: &Value) -> Result<(), String> {
            self.0.send(body)
        }
        fn state(&self) -> Result<Value, String> {
            self.0.state()
        }
        fn has_audio(&self) -> bool {
            true
        }
    }

    impl PhoneSession for Shared {
        fn route(&self) -> RouteSpec {
            self.0.route()
        }
        fn describe(&self) -> String {
            String::new()
        }
        fn start(&self, r: StreamRequest) -> Result<(), String> {
            self.0.start(r)
        }
        fn stop(&self) -> Result<(), String> {
            Ok(())
        }
        fn open_video(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
            self.0.open_video()
        }
        fn release(&self) {
            self.0.release()
        }
        fn video_url(&self) -> String {
            self.0.video_url()
        }
        fn stream_base(&self) -> String {
            format!("{}#base", self.0.url)
        }
        fn controls(&self) -> Option<Arc<dyn PhoneControl>> {
            Some(self.0.controls.clone())
        }
    }

    impl Link for FakeLink {
        fn new_identity(&self) -> (String, String) {
            Default::default()
        }
        fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
            unimplemented!()
        }
        fn pair(&self, _id: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
            Err("no".into())
        }
        fn open_route(&self, _phone: &PairedPhone, route: &RouteSpec, path: &str, _controls: Option<Arc<dyn PhoneControl>>)
                      -> Result<Box<dyn PhoneSession>, String> {
            let controls = self.controls.clone();
            let url = match route {
                RouteSpec::Usb { serial } => format!("https://127.0.0.1:41000{path}#{serial}"),
                RouteSpec::Network { address } => format!("https://{address}:8080{path}"),
            };
            let s = Arc::new(FakeSession { url, released: AtomicBool::new(false), controls });
            self.opened.lock().unwrap().push(s.clone());
            Ok(Box::new(Shared(s)))
        }
        fn start_route(&self, _source_id: Option<&str>, _format: StreamFormat) -> Result<telescope_core::link::StreamTarget, String> {
            Ok(telescope_core::link::StreamTarget { phone: self.phones().remove(0), route: wifi(), video_url: "https://192.168.1.20:8080/v1/video".into() })
        }
        fn ensure_streaming(&self, _phone: &PairedPhone, _route: &RouteSpec, _opening: Option<StreamRequest>, after_stop: bool,
                            _progress: &dyn Fn(&str)) -> Result<(), String> {
            self.reached.lock().unwrap().push(format!("wake after_stop={after_stop}"));
            Ok(())
        }
        fn stop_streaming(&self, _phone: &PairedPhone, _route: &RouteSpec) {
            if let Some(gate) = &self.stop_gate {
                let mut open = gate.0.lock().unwrap();
                while !*open {
                    open = gate.1.wait_timeout(open, Duration::from_secs(5)).unwrap().0;
                }
            }
            self.reached.lock().unwrap().push("stop".into());
        }
        fn release_stream(&self, source_id: &str) {
            self.released.lock().unwrap().push(source_id.into());
        }
        fn adopt_route(&self, _source_id: Option<&str>, route: &RouteSpec) -> Option<String> {
            self.adopted.lock().unwrap().push(route.clone());
            Some("https://127.0.0.1:41000/v1/video".into())
        }
        fn show_problem(&self, _source_id: Option<&str>, res: &Resolution) {
            self.problems.lock().unwrap().push(res.status);
        }
        fn commit_route(&self, source_id: &str) {
            self.committed.lock().unwrap().push(source_id.into());
        }
        fn drop_route(&self, source_id: &str, failed: bool) {
            self.dropped.lock().unwrap().push((source_id.into(), failed));
        }
        fn phones(&self) -> Vec<PairedPhone> {
            vec![PairedPhone { id: "p1".into(), name: "Pixel".into(), token: String::new(), ips: Vec::new(), active_ip: None, cert_sha256: String::new() }]
        }
    }

    const USB_URL: &str = "https://127.0.0.1:41000/v1/video";

    type Posted = Arc<Mutex<Vec<Report>>>;

    /// A phone streaming over Wi-Fi, and what it posts.
    fn wifi_phone() -> (PhoneKind, Arc<FakeLink>, Arc<FakeSession>, Posted) {
        let link = Arc::new(FakeLink::default());
        let posted: Arc<Mutex<Vec<Report>>> = Arc::default();
        let post: Post = { let p = posted.clone(); Arc::new(move |r| p.lock().unwrap().push(r)) };
        let request = StreamRequest { width: 0, height: 0, fps: 30, camera_off: false };
        let phone = PhoneKind::new(Some("p1".into()), link.clone(), Rc::new(StreamPlan::default), request, post, Rc::new(|_, _| {}));
        let wifi = Arc::new(FakeSession { url: "https://192.168.1.20:8080/v1/video".into(), released: AtomicBool::new(false),
                                          controls: link.controls.clone() });
        phone.woke(Arc::new(Shared(wifi.clone())));
        (phone, link, wifi, posted)
    }

    fn usb() -> RouteSpec {
        RouteSpec::Usb { serial: "SER".into() }
    }

    #[test]
    fn a_hand_over_moves_the_stream_and_tells_the_plugins_where_it_is_now() {
        let (phone, link, wifi, _) = wifi_phone();
        let opener = phone.begin_hand_over(1, usb(), USB_URL.into(), false).unwrap();
        assert!(phone.handing_over());
        assert!(opener().is_err()); // the fake has no video, but the session on the new route is open
        assert_eq!(phone.url(), "https://192.168.1.20:8080/v1/video"); // still Wi-Fi until it's moved
        let moved = phone.moved_over(1, USB_URL).unwrap();
        assert_eq!(moved.video_url(), "https://127.0.0.1:41000/v1/video#SER");
        assert_eq!(*link.committed.lock().unwrap(), ["p1"]);
        assert_eq!(phone.url(), moved.video_url());
        assert!(!phone.handing_over());
        assert_eq!(*link.controls.0.lock().unwrap(), [moved.stream_base()]); // only once the new route proved itself
        std::thread::sleep(Duration::from_millis(50));
        assert!(wifi.released.load(Ordering::SeqCst)); // the old route's session goes
    }

    #[test]
    fn a_failed_hand_over_leaves_the_stream_where_it_was() {
        let (phone, link, wifi, _) = wifi_phone();
        let opener = phone.begin_hand_over(1, usb(), USB_URL.into(), false).unwrap();
        let _ = opener();
        phone.hand_over_failed(USB_URL, true);
        assert_eq!(*link.dropped.lock().unwrap(), [("p1".to_string(), true)]);
        assert!(link.committed.lock().unwrap().is_empty());
        assert_eq!(phone.url(), "https://192.168.1.20:8080/v1/video");
        std::thread::sleep(Duration::from_millis(50));
        assert!(link.opened.lock().unwrap()[0].released.load(Ordering::SeqCst));
        assert!(!wifi.released.load(Ordering::SeqCst));
        assert!(link.controls.0.lock().unwrap().is_empty()); // the controls still go to the Wi-Fi stream
        assert!(!phone.handing_over()); // it can try again
        assert!(phone.begin_hand_over(1, usb(), USB_URL.into(), false).is_some());
    }

    #[test]
    fn a_move_for_a_stream_thats_been_replaced_lets_go_of_the_route() {
        let (phone, link, _, _) = wifi_phone();
        let opener = phone.begin_hand_over(1, usb(), USB_URL.into(), false).unwrap();
        let _ = opener();
        assert!(phone.moved_over(2, USB_URL).is_none());
        assert_eq!(*link.dropped.lock().unwrap(), [("p1".to_string(), false)]);
        assert!(link.committed.lock().unwrap().is_empty());
    }

    #[test]
    fn mic_only_session_can_hand_over_to_usb() {
        let (phone, link, _, posted) = wifi_phone();
        assert!(phone.begin_hand_over(1, usb(), USB_URL.into(), true).is_none());
        assert!(phone.handing_over());
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while posted.lock().unwrap().is_empty() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let Some(Report::HandedOver { session_id: 1, url, session: Some(_), .. }) = posted.lock().unwrap().pop() else { panic!("no session") };
        assert!(phone.moved_over(1, &url).is_some());
        assert_eq!(*link.committed.lock().unwrap(), ["p1"]);
        assert_eq!(phone.url(), "https://127.0.0.1:41000/v1/video#SER");
    }

    // test_sources.py: what a phone decides, against a stand-in window

    fn wifi() -> RouteSpec {
        RouteSpec::Network { address: "192.168.1.20".into() }
    }

    /// A banner's title, text and buttons.
    type Shown = (String, String, Vec<IssueAction>);

    /// The window as a source sees it, writing down what it's asked.
    #[derive(Default)]
    struct Stand {
        calls: RefCell<Vec<String>>,
        issues: RefCell<Vec<(String, Shown)>>,
        streaming: Cell<bool>,
        waking: Cell<bool>,
    }

    impl Stand {
        fn note(&self, call: impl Into<String>) {
            self.calls.borrow_mut().push(call.into());
        }
        fn called(&self, call: &str) -> bool {
            self.calls.borrow().iter().any(|c| c == call)
        }
        fn issue(&self, key: &str) -> Option<Shown> {
            self.issues.borrow().iter().find(|i| i.0 == key).map(|i| i.1.clone())
        }
    }

    impl SourceHost for Stand {
        fn show_issue(&self, key: &str, title: &str, text: &str, actions: Vec<IssueAction>) {
            self.issues.borrow_mut().push((key.into(), (title.into(), text.into(), actions)));
        }
        fn clear_issue(&self, key: &str) {
            self.issues.borrow_mut().retain(|i| i.0 != key);
        }
        fn is_streaming(&self) -> bool {
            self.streaming.get()
        }
        fn is_waking(&self) -> bool {
            self.waking.get()
        }
        fn wake_progress(&self, _source: &SourceRef, msg: &str) {
            self.note(format!("progress {msg}"));
        }
        fn stop_source(&self, source: &SourceRef) {
            self.note(format!("stop {}", source.id_str()));
            source.state.borrow_mut().session = None;
        }
        fn set_camera_on(&self, on: bool) {
            self.note(format!("camera_on {on}"));
        }
        fn camera_stayed_off(&self, source: &SourceRef) {
            self.note(format!("stayed_off {}", source.session().unwrap().id));
        }
        fn mic_reconnected(&self, source: &SourceRef) {
            self.note(format!("mic_back {}", source.id_str()));
        }
        fn lost_contact(&self, source: &SourceRef) {
            self.note(format!("lost {}", source.id_str()));
        }
    }

    struct NoClient;

    impl Client for NoClient {
        fn close(&self) {}
    }

    /// A worker that only counts its failed tries.
    #[derive(Default)]
    struct Tries(Cell<u32>);

    impl crate::session::Worker for Tries {
        fn set_pipeline(&self, _live: bool) {}
        fn set_packets(&self, _owners: &[String]) -> bool {
            false
        }
        fn parked(&self) -> bool {
            false
        }
        fn set_parked(&self, _parked: bool) {}
        fn failed_tries(&self) -> u32 {
            self.0.get()
        }
    }

    /// A phone streaming as session 1, and everything that hears from it.
    fn streaming_on(link: Arc<FakeLink>) -> (SourceRef, Arc<FakeLink>, Posted, Stand) {
        let posted: Posted = Arc::default();
        let post: Post = { let p = posted.clone(); Arc::new(move |r| p.lock().unwrap().push(r)) };
        let request = StreamRequest { width: 0, height: 0, fps: 30, camera_off: false };
        let phone = PhoneKind::new(Some("p1".into()), link.clone(), Rc::new(StreamPlan::default), request, post, Rc::new(|_, _| {}));
        let wifi_session = Arc::new(FakeSession { url: "https://192.168.1.20:8080/v1/video".into(), released: AtomicBool::new(false),
                                                  controls: link.controls.clone() });
        phone.woke(Arc::new(Shared(wifi_session)));
        let source = Source::new(Some("p1".into()), Box::new(phone));
        source.state.borrow_mut().session = Some(crate::session::StreamSession { id: 1, url: "http://phone/v1/video".into(), client: Rc::new(NoClient), worker: None });
        (source, link, posted, Stand::default())
    }

    fn streaming() -> (SourceRef, Arc<FakeLink>, Posted, Stand) {
        streaming_on(Arc::new(FakeLink::default()))
    }

    fn lost(source: &SourceRef) {
        source.state.borrow_mut().recovering = true;
    }

    fn ready(route: RouteSpec) -> Resolution {
        Resolution { route: Some(route), ..Resolution::new(PhoneStatus::Ready) }
    }

    fn take_moved(source: &SourceRef, posted: &Posted) {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let report = {
                let mut reports = posted.lock().unwrap();
                reports.iter().position(|r| matches!(r, Report::Moved { .. })).map(|i| reports.remove(i))
            };
            if let Some(Report::Moved { route, session, .. }) = report {
                let tries = source.session().and_then(|s| s.worker).map_or(0, |w| w.failed_tries());
                source.kind.as_phone().unwrap().moved(route, session, tries);
                return;
            }
            assert!(Instant::now() < until, "nothing moved");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_phone_that_wont_take_the_stream_back_stops_it_and_says_why() {
        let (source, link, _, host) = streaming();
        lost(&source);
        recovery_probed(&host, &source, 1, 0, &Resolution::new(PhoneStatus::NotPaired));
        assert!(host.called("stop p1"));
        assert_eq!(*link.problems.lock().unwrap(), [PhoneStatus::NotPaired]);
    }

    #[test]
    fn a_phone_stopped_meanwhile_gets_a_banner_whose_start_starts_it_again() {
        let (source, _, _, host) = streaming();
        lost(&source);
        recovery_probed(&host, &source, 1, 0, &ready(wifi()));
        let (title, _, actions) = host.issue("stopped:p1").unwrap();
        assert_eq!(title, "The phone stopped streaming");
        assert_eq!(actions[0].does, IssueDoes::StartAgain { source_id: Some("p1".into()), before: None });
    }

    #[test]
    fn with_others_streaming_the_banner_names_the_phone() {
        let (source, _, _, host) = streaming();
        host.streaming.set(true);
        lost(&source);
        recovery_probed(&host, &source, 1, 0, &Resolution { streaming_for: "Desk".into(), ..ready(wifi()) });
        assert_eq!(host.issue("stopped:p1").unwrap().0, "Pixel is streaming to Desk");
    }

    #[test]
    fn wake_progress_only_counts_while_the_host_is_waking() {
        let (source, _, _, host) = streaming();
        let id = source.kind.as_phone().unwrap().wake_id();
        wake_progress(&host, &source, id, "Waking…");
        assert!(host.calls.borrow().is_empty());
        host.waking.set(true);
        wake_progress(&host, &source, id + 1, "An old wake");
        wake_progress(&host, &source, id, "Waking…");
        assert_eq!(*host.calls.borrow(), ["progress Waking…"]);
    }

    #[test]
    fn a_quiet_phone_with_its_camera_off_is_looked_for() {
        let (source, _, _, host) = streaming();
        on_alive(&host, &source, 1, false);
        assert!(host.calls.borrow().is_empty(), "one miss isn't gone yet");
        on_alive(&host, &source, 1, false);
        assert_eq!(*host.calls.borrow(), ["lost p1"]);
    }

    #[test]
    fn a_camera_that_stays_off_says_so() {
        let (source, _, _, host) = streaming();
        on_camera_check(&host, &source, 1, Some(&json!({"camera_off": true, "camera_error": "In use"})));
        assert!(source.camera_off() && host.called("stayed_off 1"));
        let (title, text, actions) = host.issue("camera").unwrap();
        assert_eq!((title.as_str(), text.as_str()), ("The camera didn't turn back on", "In use"));
        assert_eq!(actions[0].does, IssueDoes::CameraOnAgain { source_id: Some("p1".into()) });
    }

    #[test]
    fn a_phone_that_cant_turn_its_camera_off_gets_it_back_on() {
        let (source, _, _, host) = streaming();
        source.state.borrow_mut().camera_off = true;
        on_phone_state(&host, &source, &json!({"camera_toggle": false}));
        assert!(host.called("camera_on true"));
        assert_eq!(host.issue("camera").unwrap().0, "This phone can't turn its camera off yet");
    }

    #[test]
    fn a_stop_hands_the_route_back() {
        let (source, link, _, _) = streaming();
        source.kind.stopped(&source, false, true);
        assert_eq!(*link.released.lock().unwrap(), ["p1"]);
    }

    fn wake_done(posted: &Posted) {
        let until = Instant::now() + Duration::from_secs(5);
        while !posted.lock().unwrap().iter().any(|r| matches!(r, Report::WakeDone { .. })) {
            assert!(Instant::now() < until, "the wake never came back");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_wake_right_after_stop_waits_for_the_stop_to_reach_the_phone() {
        // Otherwise the wake joins the stream that's about to end, and the late stop ends the new session too
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (source, link, posted, _) = streaming_on(Arc::new(FakeLink { stop_gate: Some(gate.clone()), ..FakeLink::default() }));
        source.kind.stopped(&source, true, true);
        source.kind.wake(&source);
        std::thread::sleep(Duration::from_millis(100));
        assert!(link.reached.lock().unwrap().is_empty(), "the wake waits for the stop");
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        wake_done(&posted);
        assert_eq!(*link.reached.lock().unwrap(), ["stop", "wake after_stop=true"]);
    }

    #[test]
    fn a_stop_stays_visible_to_everyone_waiting_for_it() {
        let stops = Stops::default();
        let release = Arc::new(AtomicBool::new(false));
        let held = release.clone();
        stops.spawn(true, move || {
            while !held.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        assert!(stops.finish(Duration::ZERO), "the first wake gave up on it");
        let waiter = { let stops = stops.clone(); std::thread::spawn(move || stops.finish(Duration::from_secs(5))) };
        std::thread::sleep(Duration::from_millis(30));
        assert!(!waiter.is_finished(), "the next one still waits for it, and so does quitting");
        release.store(true, Ordering::SeqCst);
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn a_wake_long_after_a_stop_doesnt_wait_for_the_phone_to_settle() {
        let (source, link, posted, _) = streaming();
        source.kind.wake(&source);
        wake_done(&posted);
        assert_eq!(*link.reached.lock().unwrap(), ["wake after_stop=false"]);

        let stops = Stops::default();
        stops.spawn(true, || {});
        assert!(stops.finish(STOP_WAIT), "one that just got there may still be winding down");
        *stops.finished.lock().unwrap() = Some(Instant::now() - STOP_SETTLE);
        assert!(!stops.finish(STOP_WAIT), "one that's long done says nothing");
    }

    #[test]
    fn a_wake_landing_after_a_stop_is_stopped_unless_the_phone_was_meant_to_keep_going() {
        let (source, link, _, _) = streaming();
        let phone = source.kind.as_phone().unwrap();
        source.kind.wake(&source);
        phone.stop_late(Arc::new(FakeSession { url: String::new(), released: AtomicBool::new(false), controls: link.controls.clone() }));
        phone.drain_stops(Instant::now() + Duration::from_secs(5));
        assert!(link.reached.lock().unwrap().iter().any(|c| c == "stop"));

        let (source, link, _, _) = streaming();
        let phone = source.kind.as_phone().unwrap();
        source.kind.stopped(&source, false, true);
        phone.drain_stops(Instant::now() + Duration::from_secs(5));
        link.reached.lock().unwrap().clear();
        phone.stop_late(Arc::new(FakeSession { url: String::new(), released: AtomicBool::new(false), controls: link.controls.clone() }));
        phone.drain_stops(Instant::now() + Duration::from_secs(5));
        assert!(link.reached.lock().unwrap().is_empty());
    }

    fn with_tries(source: &SourceRef) -> Rc<Tries> {
        let worker = Rc::new(Tries::default());
        source.state.borrow_mut().session.as_mut().unwrap().worker = Some(worker.clone());
        worker
    }

    #[test]
    fn the_same_cable_back_after_a_fresh_forward_died_gets_another() {
        // Unplugged again after recovery made a forward but before video came back: adb dropped that forward
        let (source, link, posted, host) = streaming();
        let worker = with_tries(&source);
        lost(&source);
        let res = Resolution { streaming: true, ..ready(usb()) };
        recovery_probed(&host, &source, 1, 0, &res);
        recovery_probed(&host, &source, 1, 0, &res); // the move is still under way: left alone
        take_moved(&source, &posted);
        assert_eq!(link.adopted.lock().unwrap().len(), 1);
        recovery_probed(&host, &source, 1, 0, &res); // still connecting on it: left alone
        assert_eq!(link.adopted.lock().unwrap().len(), 1);
        worker.0.set(1); // the cable went, and came back
        recovery_probed(&host, &source, 1, 0, &res);
        take_moved(&source, &posted);
        assert_eq!(link.adopted.lock().unwrap().len(), 2);
        recovery_probed(&host, &source, 1, 0, &res);
        assert_eq!(link.adopted.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_wifi_route_that_hasnt_connected_yet_isnt_moved_again() {
        let (source, link, posted, host) = streaming();
        let worker = with_tries(&source);
        lost(&source);
        let res = Resolution { streaming: true, ..ready(wifi()) };
        recovery_probed(&host, &source, 1, 0, &res);
        take_moved(&source, &posted);
        worker.0.set(1);
        recovery_probed(&host, &source, 1, 0, &res);
        assert_eq!(*link.adopted.lock().unwrap(), [wifi()]);
    }

    #[test]
    fn with_the_camera_off_a_missed_check_counts_as_the_forward_failing() {
        let (source, _, posted, host) = streaming();
        lost(&source);
        recovery_probed(&host, &source, 1, 0, &Resolution { streaming: true, ..ready(usb()) });
        take_moved(&source, &posted);
        let phone = source.kind.as_phone().unwrap();
        assert!(!phone.forward_failed(None));
        on_alive(&host, &source, 1, false);
        assert!(phone.forward_failed(None));
    }

    #[test]
    fn each_kind_of_source_says_what_to_do_when_its_gone() {
        struct Plain;
        impl SourceKind for Plain {
            fn name(&self) -> String {
                String::new()
            }
            fn url(&self) -> String {
                String::new()
            }
            fn control_client(&self, _source: &Source) -> Rc<dyn Client> {
                Rc::new(NoClient)
            }
        }
        assert_eq!(Plain.lost_hint(), "Check that it's still on.");
        let (source, ..) = streaming();
        assert_eq!(source.kind.lost_hint(), "Open Telescope on it.");
        let browser = PluginKind::new(Arc::new(Mutex::new(Arc::new(Offered("x")))), None);
        assert_eq!(browser.lost_hint(), "Open the Telescope page in it again.");
    }
}
