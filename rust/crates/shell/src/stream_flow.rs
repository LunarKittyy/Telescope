//! Starting, stopping and moving the panels between streams (stream_flow.py).
//!
//! StreamFlow drives Sessions: it starts a source (waking a phone first), stops one, moves the panels' focus between
//! streams that keep running and keeps the plugins that stay put (the mic) on one stream. It reaches the window only
//! through FlowHost, so the tests drive it with a stand-in and no window.

use crate::session::{same, Sessions, Source, SourceRef, StreamSession, Worker};
use serde_json::Value;
use std::collections::BTreeMap;
use std::rc::Rc;
use telescope_core::event::Event;
use telescope_core::i18n::tr;
use telescope_core::plugin::StreamInfo;

/// Virtual cameras streams can go to at once: the usual one and up to three more.
pub const MAX_SLOTS: u32 = 4;
/// Before any stream has said how big the camera is (most phones stream 1080p).
pub const DEFAULT_SIZE: (u32, u32) = (1920, 1080);

/// A stream hook to call on plugins.
#[derive(Clone, Debug, PartialEq)]
pub enum Hook {
    StreamStarting,
    StreamStart(StreamInfo),
    StreamStop,
    FocusGained(StreamInfo),
    FocusLost,
    CameraOff,
    CameraOn,
}

/// What the flow asks the window to run once the current call is through (host.later).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Later {
    AfterStart,
}

/// What StreamFlow asks of the window.
pub trait FlowHost {
    /// Every plugin as (name, follows_focus), in registration order.
    fn plugins(&self) -> Vec<(String, bool)>;
    fn call(&mut self, plugins: &[String], hook: &Hook);
    fn emit(&mut self, event: Event);
    /// Connection is there to start from (phones need it, and so does the virtual camera check).
    fn can_start(&self) -> bool;
    /// Connection's virtual camera check, before any source gets ready.
    fn camera_ready(&mut self, _interactive: bool, _source_id: Option<&str>) -> bool {
        true
    }
    /// The phone picker's choice, and moving it.
    fn selected_device(&self) -> Option<String>;
    fn select_device(&mut self, source_id: Option<&str>);
    /// Connection's card shows source_id as the panels' stream.
    fn show_selected(&mut self, _source_id: Option<&str>) {}
    fn source_by_id(&mut self, source_id: Option<&str>) -> SourceRef;
    /// A plugin's StreamSource by id, if one has that id.
    fn plugin_source(&mut self, source_id: &str) -> Option<SourceRef>;
    fn later(&mut self, what: Later);
    fn start_problem(&mut self, title: &str, text: &str);
    fn clear_issue(&mut self, key: &str);
    fn set_start_button(&mut self, streaming: bool);
    fn enable_start(&mut self, enabled: bool);
    fn source_status(&mut self, source: &SourceRef, msg: &str, kind: &str);
    fn show_stream(&mut self, source: &SourceRef);
    fn show_stopped(&mut self);
    fn show_camera_off(&mut self);
    fn refresh_tiles(&mut self);
    fn clear_pending_resolution(&mut self, source: &SourceRef);
    fn set_behind(&mut self, behind: bool, source: &SourceRef);
    fn save_profile(&mut self, source_id: Option<&str>, plugins: &[String]);
    fn apply_device_profile(&mut self, source_id: Option<&str>, plugins: &[String]);
    fn schedule_save(&mut self);
    /// The virtual camera's size from Setup, None for Auto.
    fn canvas_size(&self) -> Option<(u32, u32)>;
    /// Whether slot's virtual camera is set up to stream to.
    fn slot_ready(&self, slot: u32) -> bool;
    /// Sets up the extra virtual cameras (asking first), then add_stream(source_id) goes again.
    fn set_up_slots(&mut self, source_id: &str);
    fn set_camera_on(&mut self, on: bool);
    fn apply_state(&mut self, session_id: u64, state: &Value, source: &SourceRef);
    fn new_worker(&mut self, source: &SourceRef, url: &str, params: (Option<u32>, Option<u32>, u32), live: bool,
                  canvas: (Option<u32>, Option<u32>)) -> Rc<dyn Worker>;
    fn end_worker(&mut self, worker: Rc<dyn Worker>);
    /// The owners of the packet steps plugins asked for on the shown stream.
    fn packet_steps(&self) -> Vec<String>;
    fn ask_keyframe(&mut self, source: &SourceRef);
    /// The panels leave source's stream, which carries on behind: its frame steps keep the settings the plugins have now
    /// (pipeline(live=False)), before the next stream's settings reach them.
    fn freeze_steps(&mut self, source: &SourceRef);
}

/// Every stream's start, stop and focus; the state itself is in Sessions.
#[derive(Default)]
pub struct StreamFlow {
    pub sessions: Sessions,
    next_session_id: u64,
    /// What the last start was for, so its banner's Try again starts it again.
    pub start_id: Option<String>,
    /// Source id: the extra virtual camera it went to last, so it goes there again.
    pub slot_memory: BTreeMap<String, u32>,
}

fn info(source: &Source, session: &StreamSession) -> StreamInfo {
    StreamInfo { source_id: source.id_str().into(), url: session.url.clone() }
}

impl StreamFlow {
    pub fn new() -> StreamFlow {
        StreamFlow { next_session_id: 1, ..StreamFlow::default() }
    }

    /// The panels' stream's source, the one a start is waking, or while idle the one Start would stream from.
    pub fn active_source(&self, host: &mut dyn FlowHost) -> SourceRef {
        self.sessions.active(|| {
            let id = host.selected_device();
            host.source_by_id(id.as_deref())
        })
    }

    /// Call a stream hook on the plugins that hear about source (Sessions.hearing).
    pub fn tell(&self, host: &mut dyn FlowHost, source: &SourceRef, hook: Hook) {
        let hearing = self.sessions.hearing(&host.plugins(), source);
        host.call(&hearing, &hook);
    }

    fn followers(host: &dyn FlowHost) -> Vec<String> {
        Sessions::followers(&host.plugins())
    }

    fn stays(host: &dyn FlowHost) -> Vec<String> {
        Sessions::stays(&host.plugins())
    }

    fn each_plugin(host: &mut dyn FlowHost, hook: Hook) {
        let all: Vec<String> = host.plugins().into_iter().map(|(n, _)| n).collect();
        host.call(&all, &hook);
    }

    fn each_follower(host: &mut dyn FlowHost, hook: Hook) {
        let f = Self::followers(host);
        host.call(&f, &hook);
    }

    // ── Starting ──

    /// Ensure the phone's camera runs before reading frames; the wake happens off the UI thread.
    pub fn start(&mut self, host: &mut dyn FlowHost, interactive: bool, source: Option<SourceRef>) {
        if !host.can_start() {
            return;
        }
        host.clear_issue("start");
        let mut source = source.unwrap_or_else(|| {
            let id = host.selected_device();
            host.source_by_id(id.as_deref())
        });
        let target = if self.sessions.streams.is_empty() { source.kind.redirect() } else { None };
        if let Some(target) = target {
            if let Some(redirected) = host.plugin_source(&target) {
                // e.g. Browser camera picked with a browser already there
                if host.selected_device().as_deref() != Some(target.as_str()) {
                    host.select_device(Some(&target));
                }
                source = redirected;
            }
        }
        self.start_id = source.id.clone();
        host.clear_issue(&format!("stopped:{}", source.id_str()));
        self.sessions.preparing = true;
        let ok = host.camera_ready(interactive, source.id.as_deref()) && source.kind.prepare(interactive);
        self.sessions.preparing = false;
        host.later(Later::AfterStart);
        if !ok {
            self.start_failed(host, &source);
            return;
        }
        if !source.kind.wakes() {
            self.begin_stream(host, &source);
            return;
        }
        self.sessions.waking = true;
        self.sessions.wake_source = Some(source.clone());
        host.set_start_button(true);
        host.enable_start(false);
        let msg = if source.camera_off() { tr("Starting the phone's mic…") } else { tr("Starting the phone's camera…") };
        host.source_status(&source, &msg, "dim");
        if !self.sessions.streams.is_empty() {
            host.refresh_tiles(); // its tile says it's starting
        }
        source.kind.wake(&source);
    }

    pub fn start_failed(&mut self, host: &mut dyn FlowHost, source: &SourceRef) {
        source.state.borrow_mut().keep_slot = None;
        let streams = !self.sessions.streams.is_empty();
        if streams && !source.has_session() && source.kind.waiting() {
            self.sessions.pending = Some(source.clone()); // a tile of its own, with the panels on it, until what it waits for comes
            host.emit(Event::StreamStartFailed);
            host.refresh_tiles();
            return;
        }
        if streams && same(&self.sessions.focus, source) && !source.has_session() {
            let first = self.sessions.streams[0].clone();
            self.move_focus(host, &first); // an added camera that didn't start: back to one that streams
        }
        if same(&self.sessions.main, source) && !source.has_session() {
            self.sync_main(host); // the mic's stream didn't come back from a restart: it moves to one that streams
        }
        host.emit(Event::StreamStartFailed);
    }

    pub fn wake_done(&mut self, host: &mut dyn FlowHost, source: &SourceRef, ok: bool, reason: &str) {
        self.sessions.waking = false;
        self.sessions.wake_source = None;
        host.enable_start(true);
        host.later(Later::AfterStart);
        if !ok {
            host.set_start_button(!self.sessions.streams.is_empty());
            host.source_status(source, &tr("Not streaming"), "dim");
            host.start_problem(&tr("Couldn't start the phone's camera"), reason);
            self.start_failed(host, source);
            return;
        }
        self.begin_stream(host, source);
    }

    /// No route to start on (the Connection plugin says why): the start ends like a prepare that said no.
    pub fn wake_refused(&mut self, host: &mut dyn FlowHost, source: &SourceRef) {
        self.sessions.waking = false;
        self.sessions.wake_source = None;
        host.enable_start(true);
        host.later(Later::AfterStart);
        host.set_start_button(!self.sessions.streams.is_empty());
        host.source_status(source, &tr("Not streaming"), "dim");
        self.start_failed(host, source);
    }

    pub fn begin_stream(&mut self, host: &mut dyn FlowHost, source: &SourceRef) {
        let url = source.kind.url();
        let ctrl = source.kind.control_client(source);
        let session_id = self.next_session_id.max(1);
        self.next_session_id = session_id + 1;
        source.state.borrow_mut().camera_toggle = None;
        let first = self.sessions.streams.is_empty();
        if first {
            self.sessions.focus = Some(source.clone());
            self.sessions.main = Some(source.clone());
        }
        let slot = self.free_slot(host, source);
        {
            let mut st = source.state.borrow_mut();
            st.slot = slot;
            st.keep_slot = None;
        }
        let worker = if source.camera_off() { None } else { Some(self.start_worker(host, &url, source)) };
        let session = StreamSession { id: session_id, url: url.clone(), client: ctrl, worker };
        self.sessions.add(source, session.clone());

        if first {
            host.emit(Event::StreamStarted { url: url.clone() });
            Self::each_plugin(host, Hook::StreamStart(info(source, &session)));
        } else {
            self.tell(host, source, Hook::StreamStart(info(source, &session))); // the panels if they show it, the mic if it's back
        }
        if source.camera_off() {
            self.tell(host, source, Hook::CameraOff);
        }
        source.kind.started(source, session_id);
        host.set_start_button(true);
        if source.camera_off() {
            host.show_camera_off();
        } else {
            host.source_status(source, &tr("Connecting..."), "dim");
        }
        self.streams_changed(host);
    }

    /// The virtual camera source streams to: the usual one alone, else the one it had before if it's free.
    pub fn free_slot(&mut self, host: &mut dyn FlowHost, source: &SourceRef) -> u32 {
        let used: Vec<u32> = self.sessions.streams.iter().filter(|s| !Rc::ptr_eq(s, source)).map(|s| s.slot()).collect();
        if let Some(keep) = source.state.borrow().keep_slot {
            if !used.contains(&keep) {
                return keep;
            }
        }
        if used.is_empty() {
            return 0;
        }
        if let Some(&remembered) = source.id.as_ref().and_then(|id| self.slot_memory.get(id)) {
            if 0 < remembered && remembered < MAX_SLOTS && !used.contains(&remembered) {
                return remembered;
            }
        }
        let slot = (0..MAX_SLOTS).find(|n| !used.contains(n)).unwrap_or(0);
        if slot != 0 {
            if let Some(id) = source.id.as_ref().filter(|id| !id.is_empty()) {
                self.slot_memory.insert(id.clone(), slot);
                host.schedule_save();
            }
        }
        slot
    }

    /// A video worker on url, after whatever holds the virtual camera while idle has let go of it.
    fn start_worker(&mut self, host: &mut dyn FlowHost, url: &str, source: &SourceRef) -> Rc<dyn Worker> {
        let params = source.kind.stream_params();
        let mut canvas = host.canvas_size().map(|(w, h)| (Some(w), Some(h))).unwrap_or((None, None));
        if canvas.0.is_none() {
            if let Some((w, h)) = source.kind.auto_canvas() {
                canvas = (Some(w), Some(h));
            }
        }
        let slot = source.slot();
        if slot != 0 && !(canvas.0.is_some() && canvas.1.is_some()) {
            canvas = (Some(DEFAULT_SIZE.0), Some(DEFAULT_SIZE.1)); // an extra camera keeps one size, whatever streams to it
        }
        self.free_vcam_for(host, slot);
        let live = same(&self.sessions.focus, source) || self.sessions.streams.is_empty();
        let worker = host.new_worker(source, url, params, live, canvas);
        if live {
            worker.set_packets(&host.packet_steps());
        }
        worker
    }

    /// Whatever holds slot's virtual camera while idle (the wait screen) lets go, for a stream about to open it.
    pub fn free_vcam_for(&self, host: &mut dyn FlowHost, slot: u32) {
        if slot == 0 {
            Self::each_plugin(host, Hook::StreamStarting);
        }
        self.announce_idle_outputs(host, Some(slot));
    }

    /// What waited for the start under way to be through: phone states that need the panels, restarts, then starts
    /// that waited for the extra cameras.
    pub fn after_start(&mut self, host: &mut dyn FlowHost) {
        loop {
            let s = &mut self.sessions;
            if s.is_starting() || (s.held_states.is_empty() && s.restart_later.is_empty() && s.start_later.is_empty()) {
                return;
            }
            if let Some(key) = s.held_states.keys().next().cloned() {
                let (source, session_id, state) = s.held_states.remove(&key).unwrap();
                host.apply_state(session_id, &state, &source);
            } else if let Some(id) = s.restart_later.pop_last() {
                self.reconnect_stream(host, Some(&id));
            } else {
                let id = s.start_later.remove(0);
                self.add_stream(host, &id);
            }
        }
    }

    /// Restart the panels' stream, or source_id's, to pick up changed connection settings.
    pub fn reconnect_stream(&mut self, host: &mut dyn FlowHost, source_id: Option<&str>) {
        let waking = self.sessions.is_waking(source_id);
        let source = if source_id.is_none() || waking {
            if self.sessions.shown().is_none() && !self.sessions.waking {
                return; // a start still waking would use the old settings
            }
            self.active_source(host)
        } else {
            let Some(source) = self.sessions.find(source_id) else { return };
            if self.sessions.is_starting() {
                self.sessions.restart_later.insert(source_id.unwrap().into()); // one start at a time: this one goes after it
                return;
            }
            self.move_focus(host, &source); // it starts with the panels' settings, so they have to be its own
            source
        };
        self.stop_for_restart(host, Some(source.clone()));
        self.start(host, true, Some(source));
    }

    /// Stop a stream that starts again right after.
    pub fn stop_for_restart(&mut self, host: &mut dyn FlowHost, source: Option<SourceRef>) {
        let source = source.unwrap_or_else(|| self.active_source(host));
        if source.has_session() {
            let slot = source.slot();
            source.state.borrow_mut().keep_slot = Some(slot); // back on the virtual camera it had, which the call app watches
        }
        self.sessions.restarting = true;
        self.stop(host, false, Some(source), true);
        self.sessions.restarting = false;
    }

    // ── Several streams: which one the panels show ──

    /// Point the panels at source_id's stream (a tile was clicked).
    pub fn focus_stream(&mut self, host: &mut dyn FlowHost, source_id: &str) {
        let mut source = self.sessions.find(Some(source_id));
        if source.is_none() {
            source = self.sessions.pending.clone().filter(|p| p.id.as_deref() == Some(source_id));
        }
        if let Some(source) = source {
            if !self.sessions.waking {
                self.move_focus(host, &source);
            }
        }
    }

    /// The picker moved to source_id. True if that's handled here: its stream gets the panels, or with several running
    /// it takes the place of the one they show.
    pub fn pick_source(&mut self, host: &mut dyn FlowHost, source_id: Option<&str>) -> bool {
        if let Some(source) = self.sessions.find(source_id) {
            if !self.sessions.waking {
                self.move_focus(host, &source);
            }
            return true;
        }
        if self.sessions.streams.len() > 1 && !self.sessions.waking && !self.sessions.preparing {
            let old = self.sessions.focus.clone();
            self.stop(host, true, old, true);
            let new = host.source_by_id(source_id);
            self.move_focus(host, &new);
            self.start(host, true, Some(new));
            return true;
        }
        false
    }

    /// Start streaming source_id next to what streams already, to a virtual camera of its own.
    pub fn add_stream(&mut self, host: &mut dyn FlowHost, source_id: &str) {
        let s = &self.sessions;
        if s.is_starting() || s.streams.is_empty() || s.streams.len() as u32 >= MAX_SLOTS
            || s.is_streaming_from(Some(source_id)) {
            return;
        }
        let source = host.source_by_id(Some(source_id));
        let slot = self.free_slot(host, &source);
        if !host.slot_ready(slot) {
            host.set_up_slots(source_id);
            return;
        }
        if self.sessions.focus.as_ref().is_some_and(|f| f.camera_off()) {
            host.set_camera_on(true); // one camera off is for the mic alone; with two it would just be confusing
        }
        self.move_focus(host, &source); // its own settings first, so it starts with them
        self.start(host, true, Some(source));
    }

    pub fn move_focus(&mut self, host: &mut dyn FlowHost, source: &SourceRef) {
        let old = self.sessions.focus.clone();
        if same(&old, source) {
            return;
        }
        if let Some(old) = old {
            if let Some(session) = old.session() {
                host.save_profile(old.id.as_deref(), &Self::followers(host)); // as it was, before the stop hooks
                if let Some(worker) = &session.worker {
                    host.freeze_steps(&old);
                    worker.set_pipeline(false);
                    if worker.set_packets(&[]) {
                        host.ask_keyframe(&old); // packet steps stay with the panels
                    }
                }
                Self::each_follower(host, Hook::FocusLost); // it streams on, behind
            }
            if old.state.borrow().behind {
                old.state.borrow_mut().behind = false;
                host.emit(Event::StreamBehind { source_id: old.id_str().into(), behind: false });
            }
        }
        self.sessions.focus = Some(source.clone());
        host.show_selected(source.id.as_deref());
        host.apply_device_profile(source.id.as_deref(), &Self::followers(host));
        host.emit(Event::DeviceChanged { source_id: source.id_str().into() });
        self.show_focus(host);
    }

    /// Bring the panels and footer up to date with the stream they show now.
    pub fn show_focus(&mut self, host: &mut dyn FlowHost) {
        let Some(source) = self.sessions.focus.clone() else { return };
        host.show_stream(&source);
        if let Some(session) = source.session() {
            if let Some(worker) = &session.worker {
                worker.set_pipeline(true);
                if worker.set_packets(&host.packet_steps()) {
                    host.ask_keyframe(&source);
                }
            }
            Self::each_follower(host, Hook::FocusGained(info(&source, &session)));
            source.kind.refresh_state(&source, session.id);
            let (recovering, connected) = { let st = source.state.borrow(); (st.recovering, st.connected) };
            if recovering {
                host.emit(Event::StreamLost { source_id: source.id_str().into() });
            } else if connected {
                host.emit(Event::StreamConnected { source_id: source.id_str().into() });
            }
        }
        host.refresh_tiles();
    }

    // ── The main stream: where the plugins that stay put (the mic) are ──

    /// The first stream left is the one plugins that don't follow the panels (the mic) hear about.
    pub fn sync_main(&mut self, host: &mut dyn FlowHost) {
        let main = self.sessions.streams.first().cloned();
        let unchanged = match (&main, &self.sessions.main) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if unchanged || self.sessions.stopping_all {
            return;
        }
        self.set_main(host, main);
    }

    /// Move the plugins that don't follow the panels (the mic) to source_id's stream. False if it doesn't stream.
    pub fn make_main(&mut self, host: &mut dyn FlowHost, source_id: &str) -> bool {
        let main = self.sessions.streams.iter().find(|s| s.id.as_deref() == Some(source_id) && s.has_session()).cloned();
        let Some(main) = main else { return false };
        if self.sessions.stopping_all || self.sessions.restarting {
            return false;
        }
        if !same(&self.sessions.main, &main) {
            self.set_main(host, Some(main));
        }
        true
    }

    /// (id, name) of the stream the mic is on, while one streams.
    pub fn main_stream(&self) -> Option<(String, String)> {
        self.sessions.main.as_ref().filter(|m| m.has_session()).map(|m| (m.id_str().into(), m.kind.name()))
    }

    fn set_main(&mut self, host: &mut dyn FlowHost, main: Option<SourceRef>) {
        let stays = Self::stays(host);
        let old = self.sessions.main.take();
        if let Some(old) = old {
            host.call(&stays, &if old.has_session() { Hook::FocusLost } else { Hook::StreamStop });
            host.save_profile(old.id.as_deref(), &stays);
        }
        self.sessions.main = main.clone();
        if let Some(main) = main {
            host.apply_device_profile(main.id.as_deref(), &stays);
            // Only those staying with it: the panels' plugins already hear about it if they show it
            if let Some(session) = main.session() {
                host.call(&stays, &Hook::FocusGained(info(&main, &session)));
            }
        }
    }

    pub fn streams_changed(&self, host: &mut dyn FlowHost) {
        self.announce_idle_outputs(host, None);
        host.emit(Event::StreamsChanged { count: self.sessions.streams.len() as u32 });
        host.refresh_tiles();
    }

    /// Tell the wait screen which cameras nothing streams to; opening is one a stream is about to take. The main one
    /// counts only while others stream: alone, its wait screen follows the stream.
    pub fn announce_idle_outputs(&self, host: &mut dyn FlowHost, opening: Option<u32>) {
        // Camera off, or a stream gone a while (parked): free
        let mut used: Vec<u32> = self.sessions.streams.iter().filter(|s| {
            s.session().and_then(|x| x.worker).is_some_and(|w| !w.parked())
        }).map(|s| s.slot()).collect();
        used.extend(opening);
        let first = if self.sessions.streams.is_empty() { 1 } else { 0 };
        let slots = (first..MAX_SLOTS).filter(|n| !used.contains(n) && (*n == 0 || host.slot_ready(*n))).collect();
        host.emit(Event::IdleOutputs { slots });
    }

    // ── Stopping ──

    /// Stop the panels' stream, or source_id's; a no-op when idle. Waking counts as active, and so does a source
    /// waiting next to the streams.
    pub fn stop_stream(&mut self, host: &mut dyn FlowHost, source_id: Option<&str>) {
        if source_id.is_some() && self.sessions.pending.as_ref().is_some_and(|p| p.id.as_deref() == source_id) {
            self.drop_pending(host);
            return;
        }
        if source_id.is_some() {
            let mut source = self.sessions.find(source_id);
            if source.is_none() && self.sessions.is_waking(source_id) {
                source = self.sessions.wake_source.clone(); // its Starting tile's stop
            }
            if let Some(source) = source {
                self.stop(host, true, Some(source), false);
            }
        } else if !self.sessions.streams.is_empty() || self.sessions.waking {
            self.stop(host, true, None, false);
        }
    }

    pub fn drop_pending(&mut self, host: &mut dyn FlowHost) {
        let Some(source) = self.sessions.pending.take() else { return };
        if same(&self.sessions.focus, &source) && !self.sessions.streams.is_empty() {
            let first = self.sessions.streams[0].clone();
            self.move_focus(host, &first);
        }
        host.refresh_tiles();
        source.kind.cancel_wait();
    }

    /// Stop every stream, and a start still waking a phone (the Stop button).
    pub fn stop_all(&mut self, host: &mut dyn FlowHost, remote_stop: bool) {
        if self.sessions.pending.is_some() {
            self.drop_pending(host);
        }
        self.sessions.drop_queued();
        let shown = self.active_source(host);
        let main = self.sessions.main.clone();
        let mut others: Vec<SourceRef> = self.sessions.streams.iter().filter(|s| !Rc::ptr_eq(s, &shown)).cloned().collect();
        if self.sessions.waking {
            if let Some(w) = self.sessions.wake_source.clone().filter(|w| !Rc::ptr_eq(w, &shown)) {
                others.push(w); // one restarting next to the shown stream
            }
        }
        self.sessions.stopping_all = true;
        for source in others {
            self.stop(host, remote_stop, Some(source), false);
        }
        self.sessions.stopping_all = false;
        self.stop(host, remote_stop, None, false);
        if let Some(main) = main.filter(|m| !Rc::ptr_eq(m, &shown)) {
            // The mic has the first stream's settings; idle, they'd be saved as the shown one's
            let stays = Self::stays(host);
            host.save_profile(main.id.as_deref(), &stays);
            host.apply_device_profile(shown.id.as_deref(), &stays);
        }
    }

    /// Tear down a stream, the panels' one unless source says; remote_stop=false for reconnects. keep_focus: it starts
    /// again right after, so the panels stay on it.
    pub fn stop(&mut self, host: &mut dyn FlowHost, remote_stop: bool, source: Option<SourceRef>, keep_focus: bool) {
        let source = source.unwrap_or_else(|| self.active_source(host));
        source.kind.end_recovery(&source);
        let was_waking = self.sessions.waking
            && self.sessions.wake_source.as_ref().is_none_or(|w| Rc::ptr_eq(w, &source));
        if was_waking {
            self.sessions.waking = false;
            self.sessions.wake_source = None;
            host.enable_start(true);
            host.later(Later::AfterStart);
        }
        let shown = same(&self.sessions.focus, &source) || self.sessions.focus.is_none();

        // Forget the session before the teardown, so a state fetch on its way sees no session at once.
        let session = source.session();
        self.sessions.forget(&source);
        source.kind.stopped(&source, remote_stop, session.is_some() || was_waking);
        if let Some(session) = session {
            if let Some(worker) = session.worker {
                host.end_worker(worker);
            }
            session.client.close();
        }
        let camera_was_off = source.camera_off();
        if !self.sessions.restarting {
            self.sessions.canvas_restart = false; // stopped for real: a canvas resize under way doesn't start it again
            let mut st = source.state.borrow_mut();
            st.camera_off = false; // each stream starts with the camera on
            st.camera_off_auto = false;
            st.keep_slot = None;
            drop(st);
            if let Some(id) = &source.id {
                self.sessions.restart_later.remove(id);
                self.sessions.held_states.remove(id);
            }
        }
        host.clear_pending_resolution(&source);
        {
            let mut st = source.state.borrow_mut();
            st.settling_reports = 0;
            st.arrival_slow = false;
        }
        source.kind.forget_stream_status(&source);
        host.set_behind(false, &source);

        if !self.sessions.streams.is_empty() {
            // others carry on
            if shown {
                host.save_profile(source.id.as_deref(), &Self::followers(host));
                Self::each_follower(host, Hook::StreamStop);
                host.show_stopped();
                if !keep_focus {
                    let first = self.sessions.streams[0].clone();
                    self.move_focus(host, &first);
                }
            }
            if same(&self.sessions.main, &source) {
                if self.sessions.restarting {
                    let stays = Self::stays(host);
                    host.call(&stays, &Hook::StreamStop); // back in a moment: the mic waits for it
                } else {
                    self.sync_main(host);
                }
            }
            self.streams_changed(host);
            return;
        }

        if shown {
            host.clear_issue("camera");
        }
        self.sessions.focus = None;
        self.sessions.main = None;
        // The last stream next to it stopped: it's picked
        if !self.sessions.restarting {
            if let Some(pending) = self.sessions.pending.take() {
                host.select_device(pending.id.as_deref());
            }
        }
        host.set_start_button(self.sessions.waking);
        host.show_stopped();
        host.emit(Event::StreamStopped);
        Self::each_plugin(host, Hook::StreamStop);
        if camera_was_off && !source.camera_off() {
            host.emit(Event::CameraOnChanged { on: true });
        }
        self.streams_changed(host);
    }

    // ── A stream that dropped ──

    /// The stream dropped: the source looks for its way back (a phone may be reachable another way now).
    pub fn begin_recovery(&mut self, host: &mut dyn FlowHost, source: Option<SourceRef>) {
        let Some(source) = source.or_else(|| self.sessions.focus.clone().filter(|f| f.has_session())) else { return };
        if !source.has_session() || source.recovering() {
            return;
        }
        source.state.borrow_mut().recovering = true;
        source.kind.start_lost_clock(&source);
        host.refresh_tiles();
        host.set_behind(false, &source);
        host.emit(Event::StreamLost { source_id: source.id_str().into() });
        source.kind.lost(&source);
    }

    pub fn stream_reconnected(&mut self, host: &mut dyn FlowHost, source: &SourceRef) {
        source.kind.end_recovery(source);
        host.refresh_tiles();
        source.state.borrow_mut().settling_reports = 1; // the next throughput report still counts the gap
        let Some(session) = source.session() else { return };
        if let Some(worker) = session.worker.as_ref().filter(|w| w.parked()) {
            self.free_vcam_for(host, source.slot()); // the wait screen lets go before the stream takes the camera back
            worker.set_parked(false);
        }
        if !same(&self.sessions.focus, source) {
            session.client.resend_settings(); // the panels' plugins send them for the stream they show
        }
        self.tell(host, source, Hook::StreamStart(info(source, &session)));
    }

    /// A stream gone a while: its virtual camera gets the wait screen instead of the last frame, until it's back.
    pub fn park(&self, host: &mut dyn FlowHost, source: &SourceRef) {
        let Some(worker) = source.session().and_then(|s| s.worker) else { return };
        if !source.recovering() || worker.parked() {
            return;
        }
        log::info!("{} has been gone a while; the virtual camera shows the wait screen", source.kind.name());
        worker.set_parked(true);
        self.announce_idle_outputs(host, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Client, SourceKind};
    use std::cell::{Cell, RefCell};

    type Log = Rc<RefCell<Vec<String>>>;

    struct FakeWorker {
        live: Cell<bool>,
        parked: Cell<bool>,
        stopped: Cell<bool>,
        packets: RefCell<Vec<String>>,
    }

    impl Worker for FakeWorker {
        fn set_pipeline(&self, live: bool) {
            self.live.set(live);
        }
        fn set_packets(&self, owners: &[String]) -> bool {
            let changed = *self.packets.borrow() != owners;
            *self.packets.borrow_mut() = owners.to_vec();
            changed
        }
        fn parked(&self) -> bool {
            self.parked.get()
        }
        fn set_parked(&self, parked: bool) {
            self.parked.set(parked);
        }
    }

    struct NoClient;

    impl Client for NoClient {
        fn close(&self) {}
    }

    struct Stand {
        id: String,
        wakes: bool,
        woken: Cell<bool>,
        cancelled: Cell<bool>,
        refuse: Cell<bool>,
    }

    impl SourceKind for Rc<Stand> {
        fn name(&self) -> String {
            self.id.clone()
        }
        fn url(&self) -> String {
            format!("http://{}/", self.id)
        }
        fn wakes(&self) -> bool {
            self.wakes
        }
        fn prepare(&self, _interactive: bool) -> bool {
            !self.refuse.get()
        }
        fn waiting(&self) -> bool {
            self.refuse.get()
        }
        fn cancel_wait(&self) {
            self.cancelled.set(true);
        }
        fn wake(&self, _source: &Source) {
            self.woken.set(true);
        }
        fn control_client(&self, _source: &Source) -> Rc<dyn Client> {
            Rc::new(NoClient)
        }
        fn stream_params(&self) -> (Option<u32>, Option<u32>, u32) {
            (Some(1280), Some(720), 30)
        }
    }

    struct Host {
        log: Log,
        plugins: Vec<(String, bool)>,
        sources: BTreeMap<String, SourceRef>,
        stands: BTreeMap<String, Rc<Stand>>,
        selected: String,
        problems: Vec<String>,
        saved: Vec<(String, Vec<String>)>,
        applied: Vec<(String, Vec<String>)>,
        packet_makers: Vec<String>,
        /// (source, how many device profiles had been applied) for each stream whose frame steps were frozen.
        frozen: Vec<(String, usize)>,
        keyframes: Vec<String>,
        later: Vec<Later>,
        workers: Vec<Rc<FakeWorker>>,
        /// Whether the virtual camera is ready, and what asked.
        no_camera: bool,
        camera_asked: Vec<(bool, Option<String>)>,
    }

    impl Host {
        fn fake(&self, w: &Rc<dyn Worker>) -> Rc<FakeWorker> {
            self.workers.iter().find(|f| Rc::as_ptr(f) as *const () == Rc::as_ptr(w) as *const ()).unwrap().clone()
        }

        fn worker(&self, id: &str) -> Rc<FakeWorker> {
            self.fake(&self.sources[id].session().unwrap().worker.unwrap())
        }
    }

    impl FlowHost for Host {
        fn plugins(&self) -> Vec<(String, bool)> {
            self.plugins.clone()
        }
        fn call(&mut self, plugins: &[String], hook: &Hook) {
            let name = match hook {
                Hook::StreamStarting => "on_stream_starting",
                Hook::StreamStart(_) => "on_stream_start",
                Hook::StreamStop => "on_stream_stop",
                Hook::FocusGained(_) => "on_focus_gained",
                Hook::FocusLost => "on_focus_lost",
                Hook::CameraOff => "on_camera_off",
                Hook::CameraOn => "on_camera_on",
            };
            for p in plugins {
                self.log.borrow_mut().push(format!("{p} {name}"));
            }
        }
        fn camera_ready(&mut self, interactive: bool, source_id: Option<&str>) -> bool {
            self.camera_asked.push((interactive, source_id.map(String::from)));
            !self.no_camera
        }
        fn emit(&mut self, event: Event) {
            let text = match &event {
                Event::StreamStarted { url } => format!("stream_started {url}"),
                other => other.name().to_string(),
            };
            self.log.borrow_mut().push(text);
        }
        fn can_start(&self) -> bool {
            true
        }
        fn selected_device(&self) -> Option<String> {
            Some(self.selected.clone())
        }
        fn select_device(&mut self, source_id: Option<&str>) {
            self.selected = source_id.unwrap_or_default().into();
        }
        fn source_by_id(&mut self, source_id: Option<&str>) -> SourceRef {
            self.sources[source_id.unwrap()].clone()
        }
        fn plugin_source(&mut self, _source_id: &str) -> Option<SourceRef> {
            None
        }
        fn later(&mut self, what: Later) {
            self.later.push(what);
        }
        fn start_problem(&mut self, title: &str, _text: &str) {
            self.problems.push(title.into());
        }
        fn clear_issue(&mut self, _key: &str) {}
        fn set_start_button(&mut self, _streaming: bool) {}
        fn enable_start(&mut self, _enabled: bool) {}
        fn source_status(&mut self, _source: &SourceRef, _msg: &str, _kind: &str) {}
        fn show_stream(&mut self, _source: &SourceRef) {}
        fn show_stopped(&mut self) {}
        fn show_camera_off(&mut self) {}
        fn refresh_tiles(&mut self) {}
        fn clear_pending_resolution(&mut self, _source: &SourceRef) {}
        fn set_behind(&mut self, behind: bool, source: &SourceRef) {
            source.state.borrow_mut().behind = behind;
        }
        fn save_profile(&mut self, source_id: Option<&str>, plugins: &[String]) {
            self.saved.push((source_id.unwrap_or_default().into(), plugins.to_vec()));
        }
        fn apply_device_profile(&mut self, source_id: Option<&str>, plugins: &[String]) {
            self.applied.push((source_id.unwrap_or_default().into(), plugins.to_vec()));
        }
        fn freeze_steps(&mut self, source: &SourceRef) {
            self.frozen.push((source.id_str().into(), self.applied.len()));
        }
        fn schedule_save(&mut self) {}
        fn canvas_size(&self) -> Option<(u32, u32)> {
            Some((1920, 1080))
        }
        fn slot_ready(&self, _slot: u32) -> bool {
            true
        }
        fn set_up_slots(&mut self, _source_id: &str) {}
        fn set_camera_on(&mut self, _on: bool) {}
        fn apply_state(&mut self, _session_id: u64, _state: &Value, _source: &SourceRef) {}
        fn new_worker(&mut self, _source: &SourceRef, _url: &str, _params: (Option<u32>, Option<u32>, u32), live: bool,
                      _canvas: (Option<u32>, Option<u32>)) -> Rc<dyn Worker> {
            let w = Rc::new(FakeWorker { live: Cell::new(live), parked: Cell::new(false), stopped: Cell::new(false),
                                         packets: RefCell::new(Vec::new()) });
            self.workers.push(w.clone());
            w
        }
        fn end_worker(&mut self, worker: Rc<dyn Worker>) {
            self.fake(&worker).stopped.set(true);
        }
        fn packet_steps(&self) -> Vec<String> {
            self.packet_makers.clone()
        }
        fn ask_keyframe(&mut self, source: &SourceRef) {
            self.keyframes.push(source.id_str().into());
        }
    }

    fn setup() -> (StreamFlow, Host) {
        let log: Log = Rc::default();
        let mut sources = BTreeMap::new();
        let mut stands = BTreeMap::new();
        for (id, wakes) in [("a", false), ("b", false), ("phone", true)] {
            let stand = Rc::new(Stand { id: id.into(), wakes, woken: Cell::new(false), cancelled: Cell::new(false),
                                        refuse: Cell::new(false) });
            sources.insert(id.to_string(), Source::new(Some(id.into()), Box::new(stand.clone())));
            stands.insert(id.to_string(), stand);
        }
        let host = Host { log, plugins: vec![("camera".into(), true), ("mic".into(), false)], sources, stands,
                          selected: "a".into(), problems: vec![], saved: vec![], applied: vec![], packet_makers: vec![], frozen: vec![],
                          keyframes: vec![], later: vec![], workers: vec![], no_camera: false, camera_asked: vec![] };
        (StreamFlow::new(), host)
    }

    fn hooks(host: &Host) -> Vec<String> {
        let out = host.log.borrow().iter()
            .filter(|e| !(e.starts_with("stream") || e.starts_with("device") || e.starts_with("idle") || e.starts_with("camera_on")))
            .cloned().collect();
        host.log.borrow_mut().clear();
        out
    }

    fn logged(host: &Host, what: &str) -> bool {
        host.log.borrow().iter().any(|e| e == what)
    }

    fn ids(v: &[SourceRef]) -> Vec<&str> {
        v.iter().map(|s| s.id_str()).collect()
    }

    fn is(a: &Option<SourceRef>, id: &str) -> bool {
        a.as_ref().is_some_and(|s| s.id_str() == id)
    }

    #[test]
    fn a_source_that_needs_no_waking_starts_right_away() {
        let (mut flow, mut host) = setup();
        flow.start(&mut host, true, None);
        assert_eq!(ids(&flow.sessions.streams), ["a"]);
        assert!(is(&flow.sessions.focus, "a") && is(&flow.sessions.main, "a"));
        assert!(host.worker("a").live.get() && host.sources["a"].slot() == 0);
        assert!(logged(&host, "stream_started http://a/"));
        assert_eq!(hooks(&host), ["camera on_stream_starting", "mic on_stream_starting", "camera on_stream_start",
                                  "mic on_stream_start"]);
    }

    #[test]
    fn a_phone_wakes_first_and_a_failed_wake_says_so() {
        let (mut flow, mut host) = setup();
        host.select_device(Some("phone"));
        flow.start(&mut host, true, None);
        let phone = host.sources["phone"].clone();
        assert!(host.stands["phone"].woken.get() && flow.sessions.waking && is(&flow.sessions.wake_source, "phone"));
        assert!(flow.sessions.streams.is_empty());
        flow.wake_done(&mut host, &phone, false, "The phone didn't answer");
        assert!(!flow.sessions.waking && host.problems == ["Couldn't start the phone's camera"]);
        assert!(logged(&host, "stream_start_failed") && flow.sessions.streams.is_empty());
        flow.start(&mut host, true, None);
        flow.wake_done(&mut host, &phone, true, "");
        assert_eq!(ids(&flow.sessions.streams), ["phone"]);
    }

    fn two_streams(flow: &mut StreamFlow, host: &mut Host) {
        flow.start(host, true, None);
        flow.add_stream(host, "b");
        host.log.borrow_mut().clear();
        host.saved.clear();
        host.applied.clear();
    }

    #[test]
    fn a_second_stream_gets_its_own_camera_and_the_panels() {
        let (mut flow, mut host) = setup();
        flow.start(&mut host, true, None);
        host.log.borrow_mut().clear();
        flow.add_stream(&mut host, "b");
        assert_eq!(ids(&flow.sessions.streams), ["a", "b"]);
        assert!(is(&flow.sessions.focus, "b") && is(&flow.sessions.main, "a"));
        assert_eq!(host.sources["b"].slot(), 1);
        assert_eq!(flow.slot_memory, BTreeMap::from([("b".to_string(), 1)]));
        assert!(!host.worker("a").live.get() && host.worker("b").live.get());
        assert!(logged(&host, "camera on_focus_lost") && !logged(&host, "mic on_focus_lost"));
        assert!(logged(&host, "camera on_stream_start") && !logged(&host, "mic on_stream_start")); // the mic stays on a
    }

    #[test]
    fn clicking_a_tile_moves_the_panels_and_not_the_mic() {
        let (mut flow, mut host) = setup();
        two_streams(&mut flow, &mut host);
        flow.focus_stream(&mut host, "a");
        assert!(is(&flow.sessions.focus, "a") && is(&flow.sessions.main, "a"));
        assert_eq!(hooks(&host), ["camera on_focus_lost", "camera on_focus_gained"]);
        assert_eq!(host.saved, [("b".to_string(), vec!["camera".to_string()])]);
        assert_eq!(host.applied, [("a".to_string(), vec!["camera".to_string()])]);
        assert!(host.worker("a").live.get() && !host.worker("b").live.get());
        assert_eq!(ids(&flow.sessions.streams), ["a", "b"]); // nothing stopped
    }

    #[test]
    fn stopping_the_shown_stream_moves_the_panels_to_one_left() {
        let (mut flow, mut host) = setup();
        two_streams(&mut flow, &mut host);
        let worker = host.worker("b");
        flow.stop_stream(&mut host, None);
        assert!(worker.stopped.get() && ids(&flow.sessions.streams) == ["a"] && is(&flow.sessions.focus, "a"));
        assert!(!logged(&host, "stream_stopped")); // a still streams
        assert_eq!(hooks(&host), ["camera on_stream_stop", "camera on_focus_gained"]);
    }

    #[test]
    fn stopping_the_mics_stream_moves_the_mic_to_the_next() {
        let (mut flow, mut host) = setup();
        two_streams(&mut flow, &mut host);
        flow.stop_stream(&mut host, Some("a"));
        assert!(ids(&flow.sessions.streams) == ["b"] && is(&flow.sessions.main, "b") && is(&flow.sessions.focus, "b"));
        assert_eq!(hooks(&host), ["mic on_stream_stop", "mic on_focus_gained"]);
        assert_eq!(host.applied, [("b".to_string(), vec!["mic".to_string()])]);
    }

    #[test]
    fn a_restart_of_the_mics_stream_keeps_the_mic_waiting_for_it() {
        let (mut flow, mut host) = setup();
        two_streams(&mut flow, &mut host);
        let a = host.sources["a"].clone();
        flow.stop_for_restart(&mut host, Some(a.clone()));
        assert!(is(&flow.sessions.main, "a") && ids(&flow.sessions.streams) == ["b"]); // the mic waits for a
        assert_eq!(hooks(&host), ["mic on_stream_stop"]);
        flow.start(&mut host, true, Some(a.clone()));
        assert!(is(&flow.sessions.main, "a") && a.slot() == 0); // back on its own camera
        assert!(hooks(&host).contains(&"mic on_stream_start".to_string()));
    }

    #[test]
    fn making_another_stream_main_moves_only_the_mic() {
        let (mut flow, mut host) = setup();
        two_streams(&mut flow, &mut host);
        assert!(flow.make_main(&mut host, "b") && is(&flow.sessions.main, "b"));
        assert_eq!(hooks(&host), ["mic on_focus_lost", "mic on_focus_gained"]);
        assert_eq!(host.saved, [("a".to_string(), vec!["mic".to_string()])]);
        assert_eq!(host.applied, [("b".to_string(), vec!["mic".to_string()])]);
        assert!(!flow.make_main(&mut host, "nope"));
    }

    #[test]
    fn stop_all_stops_everything_and_puts_the_mic_settings_back() {
        let (mut flow, mut host) = setup();
        two_streams(&mut flow, &mut host);
        flow.stop_all(&mut host, true);
        assert!(flow.sessions.streams.is_empty() && flow.sessions.focus.is_none() && flow.sessions.main.is_none());
        assert!(logged(&host, "stream_stopped"));
        assert_eq!(host.saved.last().unwrap(), &("a".to_string(), vec!["mic".to_string()]));
        assert_eq!(host.applied.last().unwrap(), &("b".to_string(), vec!["mic".to_string()])); // idle, the panels show b
    }

    #[test]
    fn a_camera_that_isnt_ready_ends_the_start_before_the_source_is_asked() {
        let (mut flow, mut host) = setup();
        host.no_camera = true;
        flow.start(&mut host, false, None);
        assert_eq!(host.camera_asked, [(false, Some("a".to_string()))]);
        assert!(flow.sessions.streams.is_empty() && !flow.sessions.preparing);
        assert!(logged(&host, "stream_start_failed"));
    }

    #[test]
    fn a_pending_source_can_be_cancelled_from_its_tile() {
        let (mut flow, mut host) = setup();
        flow.start(&mut host, true, None);
        host.stands["b"].refuse.set(true);
        flow.add_stream(&mut host, "b");
        assert!(is(&flow.sessions.pending, "b") && is(&flow.sessions.focus, "b"));
        flow.stop_stream(&mut host, Some("b"));
        assert!(flow.sessions.pending.is_none() && host.stands["b"].cancelled.get() && is(&flow.sessions.focus, "a"));
    }

    #[test]
    fn packet_steps_follow_the_panels_and_the_stream_left_gets_a_keyframe() {
        let (mut flow, mut host) = setup();
        host.packet_makers = vec!["fun".into()];
        two_streams(&mut flow, &mut host);
        assert!(!host.worker("b").packets.borrow().is_empty() && host.worker("a").packets.borrow().is_empty());
        assert_eq!(host.keyframes, ["a"]); // the panels moved to the new stream, which started with one anyway
        host.keyframes.clear();
        let a = host.sources["a"].clone();
        flow.move_focus(&mut host, &a);
        assert!(!host.worker("a").packets.borrow().is_empty() && host.worker("b").packets.borrow().is_empty());
        assert_eq!(host.keyframes, ["b", "a"]);
    }

    #[test]
    fn a_stream_left_behind_keeps_its_own_settings_not_the_next_phones() {
        let (mut flow, mut host) = setup();
        two_streams(&mut flow, &mut host);
        host.frozen.clear();
        let applied = host.applied.len();
        let a = host.sources["a"].clone();
        flow.move_focus(&mut host, &a);
        // pipeline(live=False) before a's profile reaches the plugins, or b would stream on with a's name tag and sheet
        assert_eq!(host.frozen, [("b".to_string(), applied)]);
        assert_eq!(host.applied.last().map(|(id, _)| id.as_str()), Some("a"));
    }
}
