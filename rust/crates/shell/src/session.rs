//! One stream's session, and the bookkeeping of every stream the window runs (session.py).
//!
//! Sessions holds which sources stream, which one the panels show, which one the plugins that stay put (the mic) are
//! on, and what's starting or waiting to. It knows nothing about widgets or plugins' hooks.

use serde_json::Value;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use telescope_core::i18n::tr;

/// What the stream flow asks of a stream's video worker (StreamWorker).
pub trait Worker {
    /// Swap the plugin steps from the next frame on; live: they follow the plugins' changes (the panels show it).
    fn set_pipeline(&self, live: bool);
    /// The plugins' packet steps by owner; true if any came on or off, so the phone is asked for a keyframe.
    fn set_packets(&self, owners: &[String]) -> bool;
    fn parked(&self) -> bool;
    fn set_parked(&self, parked: bool);
    /// Reconnects that failed on the address it was pointed at then.
    fn failed_tries(&self) -> u32 {
        0
    }
}

/// Talking to the phone while a stream runs (PhoneControlClient), as far as the flow needs.
pub trait Client {
    fn close(&self);
    /// Sends every setting again, for a stream that came back.
    fn resend_settings(&self) {}
    /// One /v1/control body (host.phone_control).
    fn send(&self, _body: &Value) {}
}

/// Owns worker and client for one stream's lifetime; id discards stale results from threads.
#[derive(Clone)]
pub struct StreamSession {
    pub id: u64,
    pub url: String,
    pub client: Rc<dyn Client>,
    pub worker: Option<Rc<dyn Worker>>,
}

/// The frame rate out of a worker's fps report ("30 fps  1920x1080" is "30"): the footer says the size on its own.
pub fn fps_reading(msg: &str) -> String {
    msg.split("  ").next().unwrap_or("").trim_end_matches(" fps").to_string()
}

/// What a source keeps about its own stream (Source's attributes).
pub struct SourceState {
    pub session: Option<StreamSession>,
    /// Which virtual camera it streams to.
    pub slot: u32,
    /// The slot a restart comes back to.
    pub keep_slot: Option<u32>,
    /// The stream is the mic alone, with no video worker. Before a start it means Start mic only.
    pub camera_off: bool,
    /// Automatic streaming turned it off, so an app opening the camera turns it on.
    pub camera_off_auto: bool,
    /// Whether this stream's phone can turn its camera off; None: not known yet.
    pub camera_toggle: Option<bool>,
    /// A dropped stream looking for its way back.
    pub recovering: bool,
    pub behind: bool,
    /// Throughput reports to skip before judging whether it falls behind.
    pub settling_reports: u32,
    /// Frames coming in under the rate asked for.
    pub arrival_slow: bool,
    /// What the footer shows for it while the panels show another: (text, kind).
    pub status: (String, String),
    pub fps_text: String,
    pub net_text: String,
    pub connected: bool,
    /// An in-flight resolution change; cleared on confirm or timeout.
    pub pending_resolution: Option<(u32, u32)>,
}

impl Default for SourceState {
    fn default() -> Self {
        let mut st = SourceState {
            session: None, slot: 0, keep_slot: None, camera_off: false, camera_off_auto: false, camera_toggle: None,
            recovering: false, behind: false, settling_reports: 0, arrival_slow: false, status: Default::default(),
            fps_text: String::new(), net_text: String::new(), connected: false, pending_resolution: None,
        };
        st.forget_stream_status();
        st
    }
}

impl SourceState {
    /// What the footer shows for this stream while the panels show another, back to before it started.
    pub fn forget_stream_status(&mut self) {
        self.status = (tr("Not streaming"), "dim".into());
        self.fps_text = "-".into();
        self.net_text = "-".into();
        self.connected = false;
    }

    /// A status report from this stream's worker, kept for the footer and its tile.
    pub fn note_status(&mut self, kind: &str, msg: &str) {
        match kind {
            "fps" => self.fps_text = fps_reading(msg),
            "net" | "net_warn" => self.net_text = msg.into(),
            "ok" => {
                self.connected = true;
                self.status = (msg.into(), "ok".into());
            }
            "waiting" | "reconnecting" => {
                self.connected = false;
                self.status = (msg.into(), "warn".into());
            }
            "idle" => self.status = (msg.into(), "dim".into()),
            _ => self.status = (msg.into(), if kind == "warn" || kind == "err" { kind } else { "dim" }.into()),
        }
    }
}

/// Somewhere to stream from (sources.Source): who it is, its stream's state, and what it does at each step.
pub struct Source {
    pub id: Option<String>,
    pub state: RefCell<SourceState>,
    pub kind: Box<dyn SourceKind>,
}

pub type SourceRef = Rc<Source>;

impl Source {
    pub fn new(id: Option<String>, kind: Box<dyn SourceKind>) -> SourceRef {
        Rc::new(Source { id, state: RefCell::new(SourceState::default()), kind })
    }

    pub fn id_str(&self) -> &str {
        self.id.as_deref().unwrap_or("")
    }

    pub fn session(&self) -> Option<StreamSession> {
        self.state.borrow().session.clone()
    }

    pub fn has_session(&self) -> bool {
        self.state.borrow().session.is_some()
    }

    pub fn camera_off(&self) -> bool {
        self.state.borrow().camera_off
    }

    pub fn recovering(&self) -> bool {
        self.state.borrow().recovering
    }

    pub fn slot(&self) -> u32 {
        self.state.borrow().slot
    }
}

/// What a kind of source does (the Source methods a phone or a plugin's StreamSource fill in). The defaults suit a
/// source with nothing to wake, no other route to look for and no state to ask about.
pub trait SourceKind {
    fn name(&self) -> String;
    /// What stream_started and on_stream_start get as the stream's URL.
    fn url(&self) -> String;
    /// Start waits for wake() to report back before the stream begins.
    fn wakes(&self) -> bool {
        false
    }
    /// The virtual camera's size when Setup's canvas is Auto; None takes the first frame's.
    fn auto_canvas(&self) -> Option<(u32, u32)> {
        None
    }
    /// At Start with nothing streaming: another source's id to stream instead.
    fn redirect(&self) -> Option<String> {
        None
    }
    /// At Start: get ready, or show a banner and return false.
    fn prepare(&self, _interactive: bool) -> bool {
        true
    }
    /// prepare() said no, but something is on its way to it: added next to other streams, it keeps a tile until then.
    fn waiting(&self) -> bool {
        false
    }
    /// Its waiting tile was closed.
    fn cancel_wait(&self) {}
    /// Only for a source that wakes: bring it up off the UI thread, then report to the flow's wake_done.
    fn wake(&self, _source: &Source) {}
    fn control_client(&self, source: &Source) -> Rc<dyn Client>;
    /// (width, height, fps) the worker asks for; None width and height pass the frame size through.
    fn stream_params(&self) -> (Option<u32>, Option<u32>, u32) {
        (None, None, 30)
    }
    /// The stream began.
    fn started(&self, _source: &Source, _session_id: u64) {}
    /// The panels came back to this stream: its state again, as it is now.
    fn refresh_state(&self, _source: &Source, _session_id: u64) {}
    /// A plugin asked for its state (host.request_phone_state): fetched now, on a thread.
    fn fetch_state(&self, _source: &Source, _session_id: u64) {}
    /// The stream ended, or a start was given up; active: there was a stream or a wake to stop.
    fn stopped(&self, _source: &Source, _remote_stop: bool, _active: bool) {}
    /// The stream dropped. With no other route, the reader waits for the source to come back.
    fn lost(&self, _source: &Source) {}
    /// The stream just dropped: after a while the wait screen takes over, and later a banner says so.
    fn start_lost_clock(&self, _source: &Source) {}
    /// Whether its "can't reach" banner is up, set to shown: what it was.
    fn note_unreachable(&self, _shown: bool) -> bool {
        false
    }
    /// Its H.264 may go to the graphics card when that's on.
    fn card_decoding(&self) -> bool {
        true
    }
    /// Frames arrive this slowly because its camera makes no more; a phone is asked instead (as_phone).
    fn camera_limited(&self, _arrival: f32) -> bool {
        false
    }
    /// What to do when it can't be reached for a while.
    fn lost_hint(&self) -> String {
        tr("Check that it's still on.")
    }
    /// The stream is back or over: what start_lost_clock started stops.
    fn end_recovery(&self, source: &Source) {
        source.state.borrow_mut().recovering = false;
    }
    fn forget_stream_status(&self, source: &Source) {
        source.state.borrow_mut().forget_stream_status();
    }
    /// Opens a fresh feed of its video for the worker; None while there's nothing to open.
    fn opener(&self) -> Option<crate::worker::Opener> {
        None
    }
    /// What the stream says while it waits to try again, when the source has its own words for it.
    fn waiting_text(&self) -> Option<crate::worker::WaitingText> {
        None
    }
    /// It can turn its camera off and keep streaming its mic.
    fn mic_alone(&self) -> bool {
        false
    }
    /// With no video coming in, keep asking whether it's still there.
    fn watch_alive(&self) {}
    fn stop_watching_alive(&self) {}
    /// The camera was turned on: a moment later, ask whether it came on.
    fn camera_turned_on(&self, _session_id: u64) {}
    fn as_phone(&self) -> Option<&crate::sources::PhoneKind> {
        None
    }
    /// Photos its own camera takes (a DSLR), where the stills service of the phones doesn't apply.
    fn stills(&self) -> Option<std::sync::Arc<dyn telescope_core::source::SourceStills>> {
        None
    }
}

/// A phone state that needs the panels, held until the start under way is through: (source, session id, state).
pub type HeldState = (SourceRef, u64, Value);

/// Every stream the window runs, and where starting and stopping them stands.
#[derive(Default)]
pub struct Sessions {
    /// The sources streaming now, in the order they started.
    pub streams: Vec<SourceRef>,
    /// The stream the panels show (and plugins that follow them hear about).
    pub focus: Option<SourceRef>,
    /// The first stream: plugins that don't follow the panels (the mic) hear about it.
    pub main: Option<SourceRef>,
    /// Added next to the streams, waiting for something to stream (a browser).
    pub pending: Option<SourceRef>,
    pub waking: bool,
    /// The source a start is waking, which a stop meanwhile has to reach.
    pub wake_source: Option<SourceRef>,
    /// A start is finding the phone; timers still fire meanwhile, so it must not start again.
    pub preparing: bool,
    /// Stopping only to start again (reconnect, virtual camera resize).
    pub restarting: bool,
    pub stopping_all: bool,
    /// Streams to restart once the start under way is through.
    pub restart_later: BTreeSet<String>,
    /// By source id: phone states that need the panels once that start is through.
    pub held_states: BTreeMap<String, HeldState>,
    /// Sources whose add_stream waited for the extra cameras, one at a time once they're there.
    pub start_later: Vec<String>,
    /// A canvas resize stopped the stream and starts it once done; a Stop cancels it.
    pub canvas_restart: bool,
}

pub fn same(a: &Option<SourceRef>, b: &SourceRef) -> bool {
    a.as_ref().is_some_and(|a| Rc::ptr_eq(a, b))
}

impl Sessions {
    /// source streams with session now (a new stream, or the same one with a new worker or URL).
    pub fn add(&mut self, source: &SourceRef, session: StreamSession) {
        source.state.borrow_mut().session = Some(session);
        if !self.streams.iter().any(|s| Rc::ptr_eq(s, source)) {
            self.streams.push(source.clone());
        }
    }

    pub fn forget(&mut self, source: &SourceRef) {
        source.state.borrow_mut().session = None;
        self.streams.retain(|s| !Rc::ptr_eq(s, source));
    }

    /// The session of the stream the panels show.
    pub fn shown(&self) -> Option<StreamSession> {
        self.focus.as_ref().and_then(|f| f.session())
    }

    /// The streaming source with that id.
    pub fn find(&self, source_id: Option<&str>) -> Option<SourceRef> {
        self.streams.iter().find(|s| s.id.as_deref() == source_id).cloned()
    }

    /// The streaming sources the panels don't show.
    pub fn background(&self) -> Vec<SourceRef> {
        self.streams.iter().filter(|s| !same(&self.focus, s)).cloned().collect()
    }

    /// The streaming source whose worker that is (None: one that's been let go).
    pub fn by_worker(&self, worker: &Rc<dyn Worker>) -> Option<SourceRef> {
        self.streams.iter().find(|s| {
            s.state.borrow().session.as_ref().and_then(|x| x.worker.as_ref()).is_some_and(|w| Rc::ptr_eq(w, worker))
        }).cloned()
    }

    pub fn is_starting(&self) -> bool {
        self.waking || self.preparing
    }

    /// Whether the start under way is waking source_id.
    pub fn is_waking(&self, source_id: Option<&str>) -> bool {
        self.waking && self.wake_source.as_ref().is_some_and(|w| w.id.as_deref() == source_id)
    }

    /// Whether source_id streams, or a start is waking it, whether or not the panels show it.
    pub fn is_streaming_from(&self, source_id: Option<&str>) -> bool {
        self.is_waking(source_id) || self.find(source_id).is_some()
    }

    /// The panels' stream's source, the one a start is waking, or while idle the one Start would stream from
    /// (selected(), only asked for then).
    pub fn active(&self, selected: impl FnOnce() -> SourceRef) -> SourceRef {
        if let Some(focus) = &self.focus {
            if focus.has_session() || !self.waking {
                return focus.clone();
            }
        }
        if self.waking {
            if let Some(w) = &self.wake_source {
                return w.clone(); // the picker may have moved on already: a switch stops the wake after that
            }
        }
        selected()
    }

    /// Forget the restarts, held phone states and starts waiting for the start under way (a Stop).
    pub fn drop_queued(&mut self) {
        self.restart_later.clear();
        self.held_states.clear();
        self.start_later.clear();
    }

    /// Of plugins as (name, follows_focus): those hearing about source, following the panels if they show it and
    /// staying with the main stream (the mic) if it's that one.
    pub fn hearing(&self, plugins: &[(String, bool)], source: &SourceRef) -> Vec<String> {
        plugins.iter().filter(|(_, follows)| same(if *follows { &self.focus } else { &self.main }, source))
            .map(|(n, _)| n.clone()).collect()
    }

    /// The plugins that follow the panels from stream to stream.
    pub fn followers(plugins: &[(String, bool)]) -> Vec<String> {
        plugins.iter().filter(|(_, f)| *f).map(|(n, _)| n.clone()).collect()
    }

    /// The plugins that stay with one stream (the mic) whichever the panels show.
    pub fn stays(plugins: &[(String, bool)]) -> Vec<String> {
        plugins.iter().filter(|(_, f)| !*f).map(|(n, _)| n.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Plain(String);

    struct NoClient;

    impl Client for NoClient {
        fn close(&self) {}
    }

    impl SourceKind for Plain {
        fn name(&self) -> String {
            self.0.clone()
        }
        fn url(&self) -> String {
            format!("http://{}/", self.0)
        }
        fn control_client(&self, _source: &Source) -> Rc<dyn Client> {
            Rc::new(NoClient)
        }
    }

    struct W;

    impl Worker for W {
        fn set_pipeline(&self, _live: bool) {}
        fn set_packets(&self, _owners: &[String]) -> bool {
            false
        }
        fn parked(&self) -> bool {
            false
        }
        fn set_parked(&self, _parked: bool) {}
    }

    fn source(id: &str) -> SourceRef {
        Source::new(Some(id.into()), Box::new(Plain(id.into())))
    }

    fn stream(sessions: &mut Sessions, id: &str, worker: Option<Rc<dyn Worker>>) -> SourceRef {
        let s = source(id);
        sessions.add(&s, StreamSession { id: 1, url: format!("http://{id}/"), client: Rc::new(NoClient), worker });
        s
    }

    fn names(v: &[SourceRef]) -> Vec<&str> {
        v.iter().map(|s| s.id_str()).collect()
    }

    fn plugins() -> Vec<(String, bool)> {
        vec![("follower".into(), true), ("mic".into(), false)]
    }

    #[test]
    fn streams_are_kept_in_the_order_they_started_and_forgotten_when_they_stop() {
        let mut sessions = Sessions::default();
        let a = stream(&mut sessions, "a", None);
        let _b = stream(&mut sessions, "b", None);
        let again = a.session().unwrap();
        sessions.add(&a, again); // a new worker for the same stream doesn't add it twice
        assert_eq!(names(&sessions.streams), ["a", "b"]);
        sessions.forget(&a);
        assert_eq!(names(&sessions.streams), ["b"]);
        assert!(!a.has_session());
        assert!(sessions.find(Some("b")).is_some() && sessions.find(Some("a")).is_none());
    }

    #[test]
    fn the_panels_follow_focus_and_the_mic_stays_with_main() {
        let mut sessions = Sessions::default();
        let a = stream(&mut sessions, "a", None);
        let b = stream(&mut sessions, "b", None);
        sessions.focus = Some(b.clone());
        sessions.main = Some(a.clone());
        assert_eq!(sessions.hearing(&plugins(), &b), ["follower"]);
        assert_eq!(sessions.hearing(&plugins(), &a), ["mic"]);
        sessions.focus = Some(a.clone());
        assert_eq!(sessions.hearing(&plugins(), &a), ["follower", "mic"]);
        assert_eq!(Sessions::followers(&plugins()), ["follower"]);
        assert_eq!(Sessions::stays(&plugins()), ["mic"]);
        assert_eq!(names(&sessions.background()), ["b"]);
        assert_eq!(sessions.shown().unwrap().url, "http://a/");
    }

    #[test]
    fn a_source_being_woken_counts_as_streaming_from_it() {
        let mut sessions = Sessions::default();
        let phone = source("phone");
        assert!(!sessions.is_streaming_from(Some("phone")));
        sessions.waking = true;
        sessions.wake_source = Some(phone);
        assert!(sessions.is_streaming_from(Some("phone")) && sessions.is_waking(Some("phone")) && sessions.is_starting());
        assert!(!sessions.is_waking(Some("other")));
    }

    #[test]
    fn the_active_source_is_the_shown_stream_then_the_wake_then_what_start_would_use() {
        let mut sessions = Sessions::default();
        let picked = source("picked");
        let asked = std::cell::Cell::new(0);
        let selected = || {
            asked.set(asked.get() + 1);
            picked.clone()
        };
        assert_eq!(sessions.active(selected).id_str(), "picked");
        sessions.waking = true;
        sessions.wake_source = Some(source("phone"));
        assert_eq!(sessions.active(selected).id_str(), "phone");
        let shown = stream(&mut sessions, "shown", None);
        sessions.focus = Some(shown);
        assert_eq!(sessions.active(selected).id_str(), "shown");
        assert_eq!(asked.get(), 1); // only asked while nothing else answers
    }

    #[test]
    fn a_worker_finds_its_stream() {
        let mut sessions = Sessions::default();
        let worker: Rc<dyn Worker> = Rc::new(W);
        stream(&mut sessions, "a", None);
        stream(&mut sessions, "b", Some(worker.clone()));
        assert_eq!(sessions.by_worker(&worker).unwrap().id_str(), "b");
        assert!(sessions.by_worker(&(Rc::new(W) as Rc<dyn Worker>)).is_none());
    }

    #[test]
    fn a_stop_drops_what_waited_for_the_start() {
        let mut sessions = Sessions::default();
        sessions.restart_later.insert("a".into());
        sessions.held_states.insert("a".into(), (source("a"), 1, json!({})));
        sessions.start_later.push("b".into());
        sessions.drop_queued();
        assert!(sessions.restart_later.is_empty() && sessions.held_states.is_empty() && sessions.start_later.is_empty());
    }
}
