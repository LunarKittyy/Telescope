//! What the window streams from (sources.py): a paired phone, reached through link, or a plugin's StreamSource.
//!
//! A phone does what only a phone can: wake its camera, look for another route when the stream drops and report its
//! state. Its blocking calls run on threads of their own and come back to the window on the UI thread (Report).

use crate::session::{Client, Source, SourceKind};
use crate::texts::fill;
use crate::worker::Opener;
use serde_json::Value;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::i18n::tr;
use telescope_core::link::{Link, PhoneControl, PhoneSession, Resolution, StreamFormat, StreamRequest};
use telescope_core::source::{ControlClient, PairedPhone, RouteSpec, StreamSource};

/// A dropped stream: how often to look for a route back to the phone.
pub const RECOVER_RETRY: Duration = Duration::from_secs(3);
/// A stream gone this long: its virtual camera shows the wait screen, not a frozen frame.
pub const PARK_AFTER: Duration = Duration::from_secs(10);
/// Gone this long: a banner says so.
pub const UNREACHABLE_AFTER: Duration = Duration::from_secs(30);
/// How often a streaming phone is asked whether its heat dropped the size.
pub const HEAT_CHECK: Duration = Duration::from_secs(15);

/// What a phone's thread hands back to the window (the Qt signals of PhoneSource).
pub enum Report {
    WakeProgress { source_id: String, wake_id: u64, text: String },
    /// Err(None): no route to start on, and the Connection plugin says why; Err(Some(reason)): the wake failed.
    WakeDone { source_id: String, wake_id: u64, result: Result<Arc<dyn PhoneSession>, Option<String>> },
    State { source_id: String, session_id: u64, state: Value },
    RecoveryProbed { source_id: String, session_id: u64, generation: u64, resolution: Resolution },
    /// recovery moved the stream to a new route: its session there.
    Moved { source_id: String, session_id: u64, route: RouteSpec, session: Arc<dyn PhoneSession> },
}

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
    unreachable_shown: bool,
    heat_shown: bool,
}

/// A paired phone, by id.
pub struct PhoneKind {
    pub id: Option<String>,
    link: Arc<dyn Link>,
    /// What the Stream output card picked, read at each start.
    format: Rc<dyn Fn() -> StreamFormat>,
    request: StreamRequest,
    post: Post,
    inner: RefCell<PhoneInner>,
    /// Stops still on their way to the phone; a wake waits for them so it doesn't join the stream they end.
    stopping: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
    park_timer: slint::Timer,
    gone_timer: slint::Timer,
    recovery_timer: slint::Timer,
    heat_timer: slint::Timer,
    /// The window's hooks the timers call: park(source id), unreachable(source id), probe(source id), heat(source id).
    pub on_timer: Rc<dyn Fn(&str, &str)>,
}

impl PhoneKind {
    pub fn new(id: Option<String>, link: Arc<dyn Link>, format: Rc<dyn Fn() -> StreamFormat>, request: StreamRequest,
               post: Post, on_timer: Rc<dyn Fn(&str, &str)>) -> PhoneKind {
        PhoneKind { id, link, format, request, post, inner: RefCell::default(), stopping: Arc::default(),
                    park_timer: slint::Timer::default(), gone_timer: slint::Timer::default(),
                    recovery_timer: slint::Timer::default(), heat_timer: slint::Timer::default(), on_timer }
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

    /// A wake that finished after a stop: the phone may have started anyway, so it's told to stop.
    pub fn stop_late(&self, session: Arc<dyn PhoneSession>) {
        let (link, phone, route) = (self.link.clone(), self.phone(), session.route());
        self.spawn_stop(move || {
            if let Some(phone) = phone {
                link.stop_streaming(&phone, &route);
            }
            session.release();
        });
        if let Some(id) = &self.id {
            self.link.release_stream(id);
        }
    }

    fn spawn_stop(&self, f: impl FnOnce() + Send + 'static) {
        let mut stops = self.stopping.lock().unwrap();
        stops.retain(|t| !t.is_finished());
        stops.push(std::thread::spawn(f));
    }

    /// Quitting: give stops still on their way until deadline.
    pub fn drain_stops(&self, deadline: std::time::Instant) {
        for t in self.stopping.lock().unwrap().drain(..) {
            while !t.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// Recovery moved the stream to session's route.
    pub fn moved(&self, route: RouteSpec, session: Arc<dyn PhoneSession>, failed_tries: u32) {
        let mut inner = self.inner.borrow_mut();
        inner.recovery_route = Some(route.clone());
        inner.route_tries = failed_tries;
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

    /// The stream couldn't connect over the USB forward recovery made: the same cable back needs a fresh one.
    pub fn forward_failed(&self, failed_tries: u32) -> bool {
        let inner = self.inner.borrow();
        matches!(inner.recovery_route, Some(RouteSpec::Usb { .. })) && failed_tries > inner.route_tries
    }

    /// Looks for the phone again on a thread; the answer comes back as RecoveryProbed.
    pub fn probe(&self, session_id: u64) {
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
        let controls = self.session().and_then(|s| s.controls());
        let (link, post, id, sid) = (self.link.clone(), self.post.clone(), self.id.clone(), self.sid());
        std::thread::spawn(move || {
            let Some(url) = link.adopt_route(id.as_deref(), &route) else { return };
            match link.open_route(&phone, &route, video_path(&url), controls) {
                Ok(session) => post(Report::Moved { source_id: sid, session_id, route, session: session.into() }),
                Err(e) => log::info!("Moving the stream to {route:?} failed: {e}"),
            }
        });
    }

    /// A dropped stream won't come back: the Connection plugin says why, as for a failed Start.
    pub fn show_problem(&self, res: &Resolution) {
        self.link.show_problem(self.id.as_deref(), res);
    }

    /// Asks the phone for its state on a thread, as a heat check.
    pub fn check_heat(&self, session_id: u64) {
        let Some(controls) = self.session().and_then(|s| s.controls()) else { return };
        let (post, sid) = (self.post.clone(), self.sid());
        std::thread::spawn(move || {
            if let Some(state) = controls.get_state() {
                post(Report::State { source_id: sid, session_id, state });
            }
        });
    }

    /// Says so while the phone streams a smaller picture than asked because it's critically hot; true if it shows.
    pub fn note_heat(&self, shown: bool) -> bool {
        std::mem::replace(&mut self.inner.borrow_mut().heat_shown, shown)
    }

    pub fn note_unreachable(&self, shown: bool) -> bool {
        std::mem::replace(&mut self.inner.borrow_mut().unreachable_shown, shown)
    }
}

impl SourceKind for PhoneKind {
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
        let format = (self.format)();
        let wake_id = {
            let mut inner = self.inner.borrow_mut();
            inner.wake_id += 1;
            inner.wake_id
        };
        let mut opening = Some(self.request);
        if source.camera_off() {
            opening = None; // the phone opens its camera off with the mic alone; an older one ignores it
        }
        let (link, post, id, sid, stopping) = (self.link.clone(), self.post.clone(), self.id.clone(), self.sid(), self.stopping.clone());
        std::thread::spawn(move || {
            // A stop still on its way would land after this wake joined the stream it ends, so the wake goes after it
            let pending: Vec<_> = stopping.lock().unwrap().drain(..).collect();
            let after_stop = !pending.is_empty();
            for t in pending {
                let _ = t.join();
            }
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
                if let Err(e) = link.ensure_streaming(&target.phone, &target.route, opening, after_stop, &progress) {
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
        (None, None, self.request.fps)
    }

    fn started(&self, _source: &Source, session_id: u64) {
        let Some(controls) = self.session().and_then(|s| s.controls()) else { return };
        let (post, sid) = (self.post.clone(), self.sid());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            for _ in 0..3 {
                if controls.closed() {
                    return;
                }
                if let Some(state) = controls.get_state() {
                    post(Report::State { source_id: sid, session_id, state });
                    return;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            post(Report::State { source_id: sid, session_id, state: Value::Object(Default::default()) });
        });
        let (cb, sid) = (self.on_timer.clone(), self.sid());
        self.heat_timer.start(slint::TimerMode::Repeated, HEAT_CHECK, move || cb("heat", &sid));
    }

    fn refresh_state(&self, _source: &Source, session_id: u64) {
        // After the panels' settings went out, so it holds them rather than the ones the stream started with
        let Some(controls) = self.session().and_then(|s| s.controls()) else { return };
        let (post, sid) = (self.post.clone(), self.sid());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            if let Some(state) = controls.get_state() {
                post(Report::State { source_id: sid, session_id, state });
            }
        });
    }

    fn stopped(&self, _source: &Source, remote_stop: bool, active: bool) {
        self.heat_timer.stop();
        let session = {
            let mut inner = self.inner.borrow_mut();
            inner.wake_id += 1;
            inner.session.take()
        };
        let target = self.link.session_target(self.id.as_deref())
            .or_else(|| Some((self.phone()?, session.as_ref()?.route())));
        let link = self.link.clone();
        if session.is_some() || (remote_stop && active) {
            self.spawn_stop(move || {
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
        Some(Arc::new(move || session.open_video()))
    }

    fn as_phone(&self) -> Option<&PhoneKind> {
        Some(self)
    }
}

/// A plugin's StreamSource: ready once it is, with nothing to wake.
pub struct PluginKind {
    pub source: Arc<dyn StreamSource>,
}

impl SourceKind for PluginKind {
    fn name(&self) -> String {
        self.source.name()
    }
    fn url(&self) -> String {
        self.source.url()
    }
    fn auto_canvas(&self) -> Option<(u32, u32)> {
        // A browser's first frame can be any shape (even square), and on Linux apps then keep the camera at that size
        self.source.auto_canvas().or(if cfg!(target_os = "linux") { Some((1920, 1080)) } else { None })
    }
    fn redirect(&self) -> Option<String> {
        self.source.redirect()
    }
    fn prepare(&self, interactive: bool) -> bool {
        self.source.prepare(interactive)
    }
    fn control_client(&self, _source: &Source) -> Rc<dyn Client> {
        Rc::new(PluginClient(self.source.control_client().map(Arc::from)))
    }
    fn stream_params(&self) -> (Option<u32>, Option<u32>, u32) {
        (None, None, self.source.fps())
    }
    fn opener(&self) -> Option<Opener> {
        let source = self.source.clone();
        Some(Arc::new(move || source.open_feed()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
