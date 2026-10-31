//! The window's side of the stream flow (app.py's _FlowHost and _SourceHost, and the worker's signals): sources by
//! id, workers, the footer and tiles, and what phones' threads report back.

use crate::session::{Source, SourceRef, Worker};
use crate::sources::{camera_error_text, heat_text, limited_by_camera, truthy, PhoneKind, PlanNow, PluginKind, Report, StreamPlan};
use crate::stream_flow::{FlowHost, Hook, Later, StreamFlow, MAX_SLOTS};
use crate::texts::fill;
use crate::window::{with_window, Window};
use crate::worker::{StreamWorker, VideoWorker, WorkerEvent, WorkerMaker, WorkerSetup};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, VecModel};
use slint_interpreter::{Struct, Value as SValue};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::event::Event;
use telescope_core::i18n::tr;
use telescope_core::link::PhoneStatus;
use telescope_core::media::FrameSlot;
use telescope_core::source::{RouteSpec, StreamSource};
use telescope_core::ui::{Issue, IssueAction, IssueDoes};

/// How long a phone gets to reopen its camera at a new size: a camera reopen and a possible brief reconnect.
const RESOLUTION_WAIT: Duration = Duration::from_secs(8);
/// How long the footer stays red after a size that didn't land.
const RESOLUTION_FAILED_SHOWN: Duration = Duration::from_secs(4);
/// A banner action's id: turn the camera on again on the source after it.
pub(crate) const CAMERA_AGAIN: &str = "camera_again:";

/// A running worker as the flow sees it.
pub struct ShellWorker {
    pub id: u64,
    pub inner: Arc<dyn VideoWorker>,
    live: Cell<bool>,
    /// The panels moved: its frame steps are taken again once the flow's call is through.
    steps_due: Cell<bool>,
    /// The plugins' packet step makers, shared with Streams: set_packets hands the tap those of the owners it's given.
    makers: Rc<RefCell<crate::shell::PacketMakers>>,
}

impl Worker for ShellWorker {
    fn set_pipeline(&self, live: bool) {
        self.live.set(live);
        // A stream left behind already has its steps (freeze_steps); the one the panels show takes theirs as they are
        if live {
            self.steps_due.set(true);
        }
    }
    fn set_packets(&self, owners: &[String]) -> bool {
        let Some(tap) = self.inner.tap() else { return false };
        let makers = self.makers.borrow();
        tap.set(owners.iter().filter_map(|o| makers.0.iter().find(|(m, _)| m == o).cloned()).collect())
    }
    fn parked(&self) -> bool {
        self.inner.parked()
    }
    fn set_parked(&self, parked: bool) {
        self.inner.set_parked(parked);
    }
    fn failed_tries(&self) -> u32 {
        self.inner.failed_tries()
    }
}

/// Everything the window keeps about streams.
#[derive(Default)]
pub struct Streams {
    pub flow: RefCell<StreamFlow>,
    sources: RefCell<BTreeMap<String, SourceRef>>,
    /// Sources plugins offer (HostSources), by id.
    pub plugin_sources: RefCell<BTreeMap<String, crate::sources::SharedSource>>,
    /// The ids waiting for the extra cameras while they're set up (app.py _slots_then), and the question about them.
    pub(crate) slots: crate::extra_cameras::SlotSetup,
    workers: RefCell<Vec<Rc<ShellWorker>>>,
    next_worker: Cell<u64>,
    later: RefCell<Vec<Later>>,
    reconnecting: RefCell<Option<(slint::Timer, String, Cell<u32>)>>,
    /// Takes the red off the frame rate again after a size that didn't land.
    fps_tone_timer: slint::Timer,
    /// By source: how long a resolution change may take before the footer says it didn't land.
    resolution_timers: RefCell<std::collections::HashMap<String, slint::Timer>>,
    /// The packet steps plugins asked for, which go with the panels from stream to stream.
    packet_makers: Rc<RefCell<crate::shell::PacketMakers>>,
    /// Starts workers in place of StreamWorker (tests).
    pub maker: RefCell<Option<WorkerMaker>>,
}

impl Streams {
    pub fn new() -> Streams {
        Streams { flow: RefCell::new(StreamFlow::new()), ..Streams::default() }
    }
}

/// The window as StreamFlow sees it, for one call.
struct Host<'a> {
    w: &'a Rc<Window>,
}

fn key(id: Option<&str>) -> String {
    id.unwrap_or("").to_string()
}

fn slot_label(slot: u32) -> String {
    if slot == 0 { "Telescope".into() } else { format!("Telescope {}", slot + 1) }
}

impl Window {
    /// Runs f on the flow, then what it asked to run later, then brings the window up to date.
    pub fn with_flow(self: &Rc<Self>, f: impl FnOnce(&mut StreamFlow, &mut dyn FlowHost)) {
        {
            let mut host = Host { w: self };
            let mut flow = self.streams.flow.borrow_mut();
            f(&mut flow, &mut host);
            loop {
                let later: Vec<Later> = std::mem::take(&mut *self.streams.later.borrow_mut());
                if later.is_empty() {
                    break;
                }
                for l in later {
                    match l {
                        Later::AfterStart => flow.after_start(&mut host),
                    }
                }
            }
        }
        self.follow_sensors(None);
        self.sync_streams();
        self.process();
    }

    pub fn source(self: &Rc<Self>, id: Option<&str>) -> SourceRef {
        let k = key(id);
        if let Some(s) = self.streams.sources.borrow().get(&k) {
            return s.clone();
        }
        let kind: Box<dyn crate::session::SourceKind> = match self.streams.plugin_sources.borrow().get(&k) {
            Some(src) => Box::new(PluginKind { source: src.clone() }),
            None => self.phone_kind(id),
        };
        let source = Source::new(id.map(String::from), kind);
        self.streams.sources.borrow_mut().insert(k, source.clone());
        source
    }

    fn phone_kind(self: &Rc<Self>, id: Option<&str>) -> Box<dyn crate::session::SourceKind> {
        let (Some(services), Some(opts)) = (self.services.clone(), self.opts.clone()) else {
            return Box::new(crate::sources::PluginKind { source: Arc::new(Mutex::new(Arc::new(NoSource))) });
        };
        let weak = self.ui.as_weak();
        let post: crate::sources::Post = Arc::new(move |report| {
            let _ = weak.upgrade_in_event_loop(move |_| with_window(|w| w.phone_report(report)));
        });
        let me = Rc::downgrade(self);
        let on_timer: crate::sources::TimerHook = Rc::new(move |what, sid| {
            if let Some(w) = me.upgrade() {
                w.source_timer(what, sid);
            }
        });
        let me = Rc::downgrade(self);
        let plan: PlanNow = Rc::new(move || {
            let Some(w) = me.upgrade() else { return StreamPlan::default() };
            let st = w.state.borrow();
            // Light (H.264) without a Stream output card to say otherwise
            let cfg = st.plugin_config("stream_output").unwrap_or_else(|| serde_json::json!({"format": "h264"}));
            let dynamic = cfg.get("bitrate_mbps").and_then(Value::as_f64) == Some(-1.0);
            let format = telescope_core::link::StreamFormat { h264: cfg.get("format").and_then(Value::as_str) == Some("h264"), dynamic };
            let output = st.index("stream_output").and_then(|i| st.slots[i].plugin.as_ref());
            StreamPlan { format, params: output.and_then(|p| p.stream_params()), opening: output.and_then(|p| p.opening()) }
        });
        Box::new(PhoneKind::new(id.map(String::from), services.link.clone(), plan, opts.request, post, on_timer))
    }

    /// The panels' stream's source, the one a start is waking, or while idle the one Start would stream from.
    pub fn active_source(self: &Rc<Self>) -> SourceRef {
        let flow = self.streams.flow.borrow();
        let mut host = Host { w: self };
        flow.active_source(&mut host)
    }

    /// Whether the footer speaks for source now.
    fn is_shown(self: &Rc<Self>, source: &SourceRef) -> bool {
        match self.streams.flow.try_borrow() {
            Ok(_) => Rc::ptr_eq(&self.active_source(), source),
            Err(_) => self.state.borrow().streams.source_id.as_deref().is_none_or(|id| id == source.id_str()), // from inside the flow
        }
    }

    /// What the camera can do now, as plugins read it, and the Start button that goes with it.
    pub(crate) fn sync_camera(self: &Rc<Self>) {
        let Ok(flow) = self.streams.flow.try_borrow() else { return };
        let mut host = Host { w: self };
        let source = flow.active_source(&mut host);
        let can_turn_off = flow.can_turn_camera_off(&mut host);
        let (off, auto) = { let st = source.state.borrow(); (st.camera_off, st.camera_off_auto) };
        let idle = flow.sessions.streams.is_empty() && !flow.sessions.waking;
        drop(flow);
        let changed = {
            let streams = &mut self.state.borrow_mut().streams;
            let changed = streams.camera_off != off;
            (streams.camera_off, streams.camera_off_auto, streams.can_turn_off) = (off, auto, can_turn_off);
            changed
        };
        if changed && idle {
            self.show_idle_button();
        }
    }

    /// Host::set_camera_on, and the bridge's Camera switch.
    pub fn set_camera_on(self: &Rc<Self>, on: bool, auto: bool) {
        self.with_flow(move |flow, host| flow.set_camera_on(host, on, auto));
    }

    /// _on_mic_changed: with neither, the stream would carry nothing.
    pub(crate) fn mic_changed(self: &Rc<Self>) {
        self.sync_camera();
        let (enabled, camera_off) = { let st = self.state.borrow(); (st.mic.0, st.streams.camera_off) };
        if !enabled && camera_off {
            self.set_camera_on(true, false);
        }
    }

    /// _on_source_selected: each source keeps its own camera choice, so picking another can change what Start does.
    pub(crate) fn source_selected(self: &Rc<Self>) {
        self.sync_camera();
        let (on, shown) = { let st = self.state.borrow(); (!st.streams.camera_off, st.camera_on_shown) };
        if self.streams.flow.borrow().sessions.streams.is_empty() && on != shown {
            self.emit(Event::CameraOnChanged { on });
        }
    }

    /// _camera_on_again: the switch is the panels' stream's.
    pub(crate) fn camera_on_again(self: &Rc<Self>, source_id: &str) {
        let id = source_id.to_string();
        self.with_flow(move |flow, host| flow.focus_stream(host, &id));
        let source = self.source(Some(source_id).filter(|s| !s.is_empty()));
        if self.is_shown(&source) {
            self.set_camera_on(true, false);
        }
    }

    /// _on_resolution_pending: the phone has to reopen its camera, so the frame rate says it's waiting.
    pub(crate) fn resolution_pending(self: &Rc<Self>, width: u32, height: u32) {
        let source = self.active_source();
        source.state.borrow_mut().pending_resolution = Some((width, height));
        self.streams.fps_tone_timer.stop();
        self.set("fps-tone", "warn");
        let (me, id) = (Rc::downgrade(self), source.id.clone());
        self.streams.resolution_timers.borrow_mut().entry(source.id_str().into()).or_default().start(slint::TimerMode::SingleShot, RESOLUTION_WAIT, move || {
            if let Some(w) = me.upgrade() {
                w.resolution_timed_out(id.as_deref());
            }
        });
    }

    fn resolution_timed_out(self: &Rc<Self>, id: Option<&str>) {
        let source = self.source(id);
        source.state.borrow_mut().pending_resolution = None;
        if self.is_shown(&source) {
            self.set("fps-tone", "err");
            let me = Rc::downgrade(self);
            self.streams.fps_tone_timer.start(slint::TimerMode::SingleShot, RESOLUTION_FAILED_SHOWN, move || {
                if let Some(w) = me.upgrade() {
                    w.set("fps-tone", "");
                }
            });
        }
    }

    fn clear_pending_resolution(self: &Rc<Self>, source: &SourceRef) {
        if let Some(timer) = self.streams.resolution_timers.borrow().get(source.id_str()) {
            timer.stop();
        }
        source.state.borrow_mut().pending_resolution = None;
        if self.is_shown(source) {
            self.set("fps-tone", "");
        }
    }

    /// _settle_stream: the phone is restarting its camera, so the shown stream isn't judged until it's back.
    pub(crate) fn settle_stream(self: &Rc<Self>) {
        let focus = self.streams.flow.borrow().sessions.focus.clone();
        if let Some(worker) = focus.and_then(|f| self.worker_of(&f)) {
            worker.inner.settle();
        }
    }

    fn find_worker(&self, id: u64) -> Option<(SourceRef, Rc<ShellWorker>)> {
        let worker = self.streams.workers.borrow().iter().find(|w| w.id == id).cloned()?;
        let as_dyn: Rc<dyn Worker> = worker.clone();
        let source = self.streams.flow.borrow().sessions.by_worker(&as_dyn)?;
        Some((source, worker))
    }

    /// The state plugins read, the frames they see, and the steps workers run, as the flow left them.
    fn sync_streams(self: &Rc<Self>) {
        let flow = self.streams.flow.borrow();
        let s = &flow.sessions;
        {
            let mut st = self.state.borrow_mut();
            st.streams.streaming = !s.streams.is_empty();
            st.streams.starting = s.is_starting();
            st.streams.restarting = s.restarting;
            st.streams.source_id = s.focus.as_ref().map(|f| f.id_str().to_string());
            st.streams.main = flow.main_stream();
            st.streams.live = s.streams.iter().filter(|x| x.has_session()).map(|x| x.id_str().to_string()).collect();
            st.streams.stopping_all = s.stopping_all;
            st.streams.ids = s.streams.iter().map(|x| x.id_str().to_string()).collect();
            st.streams.url = s.focus.as_ref().and_then(|f| f.session()).map(|x| x.url);
            st.streams.background = s.streams.iter().filter(|x| !crate::session::same(&s.focus, x))
                .map(|x| telescope_core::host::BackgroundStream { source_id: x.id_str().to_string(), name: x.kind.name() }).collect();
            (st.streams.waking, st.streams.preparing) = (s.waking, s.preparing);
            let labels = self.services.as_ref().map(|sv| sv.outputs.slots()).unwrap_or_default();
            st.streams.can_hand_over = s.streams.iter().filter(|src| {
                let Some(session) = src.session() else { return false };
                let worker_can = session.worker.is_none() || self.worker_of(src).is_some_and(|w| w.inner.can_hand_over());
                !src.recovering() && worker_can && src.kind.as_phone().is_some_and(|p| !p.handing_over())
            }).map(|x| x.id_str().to_string()).collect();
            st.streams.cameras = s.streams.iter().filter(|x| x.has_session()).map(|src| {
                let slot = src.slot();
                crate::shell::CameraUse {
                    source_id: src.id_str().to_string(),
                    slot,
                    label: labels.iter().find(|l| l.slot == slot).map(|l| l.name.clone()).unwrap_or_default(),
                    size: self.worker_of(src).and_then(|w| w.inner.camera_size()),
                }
            }).collect();
            if s.streams.is_empty() {
                st.streams.phone_state = None;
            }
        }
        let due: Vec<Rc<ShellWorker>> = self.streams.workers.borrow().iter().filter(|w| w.steps_due.replace(false)).cloned().collect();
        for w in due {
            let steps = self.state.borrow_mut().pipeline_steps(w.inner.source_id());
            w.inner.set_steps(steps);
        }
        if let Some(frames) = self.services.as_ref().and_then(|sv| sv.media.stream_frames()) {
            let list: Vec<(String, String, Arc<dyn FrameSlot>)> = s.streams.iter().filter_map(|src| {
                let session = src.session()?;
                let worker = session.worker?;
                let w = self.streams.workers.borrow().iter().find(|x| Rc::as_ptr(x) as *const () == Rc::as_ptr(&worker) as *const ()).cloned()?;
                Some((src.id_str().to_string(), src.kind.name(), w.inner.slot()))
            }).collect();
            frames.set_streams(list);
        }
        drop(flow);
        self.refresh_tiles();
    }

    /// The newest frame of the stream the panels show (its worker is the live one), and its number.
    pub(crate) fn preview_frame(&self) -> Option<(telescope_core::Frame, u64)> {
        let workers = self.streams.workers.borrow();
        workers.iter().find(|w| w.live.get()).and_then(|w| w.inner.slot().latest())
    }

    /// The worker a source's session streams through, while it has one.
    fn worker_of(&self, src: &crate::session::Source) -> Option<Rc<ShellWorker>> {
        let worker = src.session()?.worker?;
        self.streams.workers.borrow().iter().find(|x| Rc::as_ptr(x) as *const () == Rc::as_ptr(&worker) as *const ()).cloned()
    }

    /// Host::update_stream_output: the new size or rate goes to that stream's worker (the focused one without an id).
    pub(crate) fn update_output(self: &Rc<Self>, change: &telescope_core::host::OutputChange, source_id: Option<&str>) {
        let flow = self.streams.flow.borrow();
        let source = match source_id {
            Some(id) => flow.sessions.find(Some(id)),
            None => flow.sessions.focus.clone(),
        };
        if let Some(w) = source.and_then(|s| self.worker_of(&s)) {
            w.inner.update_output(change.width, change.height, change.fps.flatten());
        }
    }

    /// A plugin changed its frame step: the stream the panels show takes it from the next frame.
    pub fn refresh_frame_steps(self: &Rc<Self>) {
        let live: Vec<Rc<ShellWorker>> = self.streams.workers.borrow().iter().filter(|w| w.live.get()).cloned().collect();
        for w in live {
            let steps = self.state.borrow_mut().pipeline_steps(w.inner.source_id());
            w.inner.set_steps(steps);
        }
    }

    pub fn is_streaming(&self) -> bool {
        let flow = self.streams.flow.borrow();
        !flow.sessions.streams.is_empty() || flow.sessions.waking
    }

    /// The Start button: stop everything, or start what's picked.
    pub fn toggle_stream(self: &Rc<Self>) {
        if self.is_streaming() {
            self.with_flow(|flow, host| flow.stop_all(host, true));
        } else if !self.streams.flow.borrow().sessions.preparing {
            self.with_flow(|flow, host| flow.start(host, true, None));
        }
    }

    /// _start_source: a banner's Start, by itself or next to the others.
    pub fn start_source(self: &Rc<Self>, id: Option<String>) {
        let id = id.or_else(|| self.streams.flow.borrow().start_id.clone());
        let streaming = !self.streams.flow.borrow().sessions.streams.is_empty();
        if !streaming {
            self.select_first(id.as_deref());
        }
        self.with_flow(move |flow, host| {
            if streaming && !flow.sessions.is_streaming_from(id.as_deref()) {
                if let Some(id) = id.as_deref() {
                    flow.add_stream(host, id);
                }
            } else if !streaming {
                let source = host.source_by_id(id.as_deref());
                flow.start(host, true, Some(source));
            }
        });
    }

    /// restart_vcam_canvas: stop the stream, reload the driver on Linux, and start it again at the new canvas.
    pub fn restart_canvas(self: &Rc<Self>, plugin: &str, request: telescope_core::host::RequestId) {
        let posts = self.state.borrow().posts.clone();
        if self.streams.flow.borrow().sessions.streams.len() > 1 {
            // The reload needs every virtual camera free, and would drop the extra ones
            let event = Event::RequestDone { request, ok: false, message: tr("Stop all but one camera first, then apply again"), data: serde_json::json!({}) };
            return posts.push_event(plugin, event);
        }
        let was_streaming = {
            let mut flow = self.streams.flow.borrow_mut();
            let was = flow.sessions.shown().is_some() || flow.sessions.waking; // a start still waking restarts after it too
            flow.sessions.canvas_restart = flow.sessions.canvas_restart || was;
            was
        };
        let old: Vec<_> = self.streams.workers.borrow().iter().map(|w| w.inner.clone()).collect();
        if was_streaming {
            self.with_flow(|flow, host| flow.stop_for_restart(host, None));
        }
        let linux = cfg!(target_os = "linux");
        self.set_status(&if linux { tr("Reloading v4l2loopback…") } else { tr("Restarting stream…") }, "dim");
        let outputs = self.services.as_ref().map(|s| s.outputs.clone());
        let plugin = plugin.to_string();
        std::thread::spawn(move || {
            // A second stop waits for the one the flow started, so the camera is free after this
            old.iter().for_each(|worker| worker.stop());
            let result = match outputs.filter(|_| linux) {
                Some(outputs) => outputs.set_up(telescope_core::output::SetupAction::Reload, &|_| {}),
                None => telescope_core::output::SetupResult { ok: true, message: String::new(), command: None },
            };
            let _ = slint::invoke_from_event_loop(move || crate::window::with_window(|w| w.canvas_reloaded(&plugin, request, result, was_streaming)));
        });
    }

    /// _on_canvas_reload_done.
    fn canvas_reloaded(self: &Rc<Self>, plugin: &str, request: telescope_core::host::RequestId, result: telescope_core::output::SetupResult, restart_stream: bool) {
        let restart = {
            let mut flow = self.streams.flow.borrow_mut();
            let restart = restart_stream && flow.sessions.canvas_restart; // not after a Stop since
            if restart_stream {
                flow.sessions.canvas_restart = false;
            }
            restart
        };
        let command = result.command.clone().unwrap_or_default();
        if result.ok {
            let linux = cfg!(target_os = "linux");
            self.set_status(&if linux { tr("Loopback reloaded: {message}").replace("{message}", &result.message) } else { tr("Canvas updated") }, "ok");
            if restart {
                self.with_flow(|flow, host| flow.start(host, true, None)); // not over a stream started while the reload ran
            }
        } else if command.is_empty() {
            self.set_status(&tr("Reload failed: {message}").replace("{message}", &result.message), "err");
        } else {
            self.set_status(&tr("Not streaming"), "dim");
        }
        let data = serde_json::json!({"command": command});
        self.state.borrow_mut().deliver_to(plugin, &Event::RequestDone { request, ok: result.ok, message: result.message, data });
        self.process();
    }

    /// remove_extra_cameras: the extra virtual cameras go, once nothing streams to them.
    pub fn remove_extras(self: &Rc<Self>, plugin: &str, request: telescope_core::host::RequestId) {
        let posts = self.state.borrow().posts.clone();
        let Some(outputs) = self.services.as_ref().map(|s| s.outputs.clone()) else { return };
        let extras = (1..outputs.max_slots()).any(|n| outputs.slot_ready(n));
        if !extras || self.streams.flow.borrow().sessions.streams.iter().any(|s| s.slot() != 0) {
            let message = if extras { tr("Stop the streams going to them first") } else { tr("There are none to remove") };
            return posts.push_event(plugin, Event::RequestDone { request, ok: false, message, data: serde_json::json!({}) });
        }
        let plugin = plugin.to_string();
        std::thread::spawn(move || {
            let result = outputs.set_up(telescope_core::output::SetupAction::RemoveSlots, &|_| {});
            let _ = slint::invoke_from_event_loop(move || crate::window::with_window(|w| {
                w.with_flow(|flow, host| flow.announce_idle_outputs(host, None));
                w.state.borrow_mut().deliver_to(&plugin, &Event::RequestDone { request, ok: result.ok, message: result.message, data: serde_json::json!({}) });
                w.process();
            }));
        });
    }

    /// A control body to source_id's stream, the shown one by default.
    pub fn phone_control(&self, source_id: Option<&str>, body: &Value) {
        let flow = self.streams.flow.borrow();
        let source = match source_id {
            Some(id) => flow.sessions.find(Some(id)),
            None => flow.sessions.focus.clone(),
        };
        if let Some(session) = source.and_then(|s| s.session()) {
            session.client.send(body);
        }
    }

    /// host.change_packets: the makers as they are now, on the shown stream's tap.
    pub fn change_packets(&self, makers: crate::shell::PacketMakers) {
        let owners = makers.owners();
        *self.streams.packet_makers.borrow_mut() = makers;
        let focus = self.streams.flow.borrow().sessions.focus.clone();
        let Some(source) = focus else { return };
        let Some(worker) = source.session().and_then(|s| s.worker).and_then(|w| self.shell_worker(&w)) else { return };
        if worker.set_packets(&owners) {
            ask_keyframe(&source);
        }
    }

    /// host.request_phone_state: the phone fetches its state on a thread, and apply_state drops it if the stream stopped.
    pub fn request_phone_state(&self, source_id: Option<&str>) {
        let flow = self.streams.flow.borrow();
        let source = match source_id {
            Some(id) => flow.sessions.find(Some(id)),
            None => flow.sessions.focus.clone(),
        };
        if let Some((source, session)) = source.and_then(|s| s.session().map(|session| (s, session))) {
            source.kind.fetch_state(&source, session.id);
        }
    }

    /// Host::take_still: the photo starts on its stream's phone; its progress and end come back to `plugin` as events, from the
    /// photo's own thread through the posts.
    pub fn take_still(self: &Rc<Self>, plugin: &str, request: telescope_core::host::RequestId, source: &str, options: &Value, folder: &str) {
        let posts = self.state.borrow().posts.clone();
        let fail = |why: &str| {
            let data = serde_json::json!({"ok": false, "files": [], "code": "failed", "error": why});
            posts.push_event(plugin, Event::RequestDone { request, ok: false, message: why.into(), data });
        };
        let Some(stills) = self.state.borrow().link.as_ref().and_then(|l| l.stills()) else { return fail(&tr("The phone couldn't take the photo.")) };
        let found = self.streams.flow.borrow().sessions.find(Some(source));
        let target = found.as_ref().and_then(|src| Some((src.kind.as_phone()?.still_target()?, self.worker_of(src).map(|w| w.inner.clone()))));
        let Some(((base, phone), worker)) = target else { return fail(&tr("The phone couldn't take the photo.")) };
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.state.borrow_mut().still_alive.push((source.to_string(), alive.clone()));
        let (progress_posts, done_posts, owner, done_owner) = (posts.clone(), posts.clone(), plugin.to_string(), plugin.to_string());
        let started = stills.take(
            Some(source), &base, &phone, options, folder,
            Box::new(move |data| {
                // The phone may pause the video for it, or a long exposure holds it up: that's no dropped stream
                if data.get("state").and_then(Value::as_str) == Some("capturing") {
                    if let Some(worker) = &worker {
                        worker.hold_for_photo(3.0);
                    }
                }
                progress_posts.push_event(&owner, Event::RequestProgress { request, data });
            }),
            Box::new(move |data| {
                let ok = data.get("ok").and_then(Value::as_bool) == Some(true);
                let message = data.get("error").and_then(Value::as_str).unwrap_or("").to_string();
                done_posts.push_event(&done_owner, Event::RequestDone { request, ok, message, data });
            }),
            Box::new(move || alive.load(std::sync::atomic::Ordering::SeqCst)),
        );
        if started {
            self.state.borrow().bus.emit(Event::StillBusyChanged { source_id: source.to_string(), busy: true });
        } else {
            fail(&tr("The phone is already taking a photo."));
        }
    }

    pub fn stop_all_streams(self: &Rc<Self>) {
        if self.is_streaming() {
            self.with_flow(|flow, host| flow.stop_all(host, true));
        }
    }

    // ── What workers report ──

    fn worker_event(self: &Rc<Self>, id: u64, event: WorkerEvent) {
        let Some((source, worker)) = self.find_worker(id) else { return }; // a worker that's been let go
        match event {
            WorkerEvent::Status(kind, msg) => self.worker_status(&source, &kind, &msg),
            WorkerEvent::Reconnected => self.with_flow(|flow, host| flow.stream_reconnected(host, &source)),
            WorkerEvent::Moved(url) => self.moved_over(&source, Some(&worker), &url),
            WorkerEvent::MoveFailed(url) => {
                if let Some(phone) = source.kind.as_phone() {
                    phone.hand_over_failed(&url, true);
                    self.sync_streams();
                }
            }
            WorkerEvent::VcamOpened(w, h) => {
                let slot = source.slot();
                {
                    let st = &mut *self.state.borrow_mut();
                    if let Some(c) = st.streams.cameras.iter_mut().find(|c| c.source_id == source.id_str()) {
                        c.size = Some((w, h));
                    }
                    if slot == 0 {
                        st.bus.emit(Event::VcamOpened { width: w, height: h });
                    }
                    st.bus.emit(Event::CameraOpened { source_id: source.id_str().into(), slot, width: w, height: h });
                }
                self.process();
            }
            WorkerEvent::Preview(..) => {}
        }
        drop(worker);
    }

    /// _on_worker_status: the footer for the shown stream, the tile and recovery for the others.
    fn worker_status(self: &Rc<Self>, source: &SourceRef, kind: &str, msg: &str) {
        source.state.borrow_mut().note_status(kind, msg);
        let (active, several) = {
            let flow = self.streams.flow.borrow();
            (crate::session::same(&flow.sessions.focus, source), flow.sessions.streams.len() > 1)
        };
        if several && matches!(kind, "ok" | "waiting" | "reconnecting") {
            self.refresh_tiles(); // its tile says how it's doing
        }
        if !active {
            match kind {
                "waiting" | "reconnecting" => self.with_flow(|flow, host| flow.begin_recovery(host, Some(source.clone()))),
                "ok" => {
                    self.state.borrow_mut().bus.emit(Event::StreamConnected { source_id: source.id_str().into() });
                    self.process();
                }
                "idle" if source.has_session() => self.ended_by_itself(source, true),
                _ => {}
            }
            return;
        }
        match kind {
            "fps" => {
                self.set("fps", msg.split("  ").next().unwrap_or("").trim_end_matches(" fps").to_string()); // the status line already gives the size
                let pending = source.state.borrow().pending_resolution;
                let size = msg.rsplit(' ').next().and_then(|t| t.split_once('x')).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                if pending.is_some() && size == pending {
                    self.clear_pending_resolution(source);
                }
            }
            "net" => {
                source.state.borrow_mut().arrival_slow = false;
                self.set("net", msg.to_string());
                self.show_slow(source, false, false);
            }
            "net_warn" => {
                source.state.borrow_mut().arrival_slow = true;
                self.set("net", msg.to_string());
                self.on_slow_arrival(source);
            }
            "ok" => {
                self.set_status(msg, "ok");
                self.set_behind(source, false);
                let keep_others = |k: &str| k == "h264" || (k.starts_with("stopped:") && k != format!("stopped:{}", source.id_str()));
                {
                    let mut st = self.state.borrow_mut();
                    for k in st.banners.keys() {
                        if !keep_others(&k) {
                            st.banners.clear(Some(&k), None);
                        }
                    }
                    st.bus.emit(Event::StreamConnected { source_id: source.id_str().into() });
                }
                self.process();
            }
            "warn" => self.set_status(msg, "warn"),
            "waiting" => {
                // No first frame yet: maybe the route died between waking the phone and now
                self.set_status(msg, "warn");
                self.with_flow(|flow, host| flow.begin_recovery(host, None));
            }
            "reconnecting" => {
                self.start_reconnecting_animation(msg);
                self.with_flow(|flow, host| flow.begin_recovery(host, None));
            }
            "idle" => {
                self.clear_pending_resolution(source);
                self.set("fps", "—");
                self.set("net-tone", "");
                self.set("net", "—");
                self.set_status(msg, "dim");
                if source.has_session() {
                    // Stop takes the worker out first, so this is a worker that ended by itself: tidy up like a Stop.
                    self.ended_by_itself(source, false);
                }
            }
            _ => self.set_status(msg, "dim"),
        }
    }

    fn ended_by_itself(self: &Rc<Self>, source: &SourceRef, background: bool) {
        log::warn!("The stream worker for {} ended by itself", source.kind.name());
        let s = source.clone();
        self.with_flow(move |flow, host| flow.stop(host, true, Some(s), false));
        let title = if background {
            fill(&tr("The stream from {name} stopped unexpectedly"), &[("name", &source.kind.name())])
        } else {
            tr("The stream stopped unexpectedly")
        };
        self.show_issue(&format!("stopped:{}", source.id_str()), &title, &tr("Copy diagnostics has the details."),
                        vec![start_action(source.id.clone())]);
    }

    /// _on_slow_arrival: frames arrive under the rate asked for. If the phone's camera makes about that many, it's not the link.
    fn on_slow_arrival(self: &Rc<Self>, source: &SourceRef) {
        let arrival = self.worker_of(source).map(|w| w.inner.last_arrival_fps()).filter(|a| *a > 0.0);
        let quiet = { let st = source.state.borrow(); !st.recovering && st.settling_reports == 0 };
        match (arrival.filter(|_| quiet), source.kind.as_phone(), source.session()) {
            (Some(arrival), Some(phone), Some(session)) => match phone.camera_limits(arrival) {
                Some(limited) => self.show_slow(source, true, limited),
                None => phone.ask_camera_rate(session.id, arrival),
            },
            _ => self.show_slow(source, true, false), // nothing to ask, or nothing to say yet
        }
    }

    fn show_slow(self: &Rc<Self>, source: &SourceRef, slow: bool, camera_limited: bool) {
        let behind = slow && !camera_limited;
        self.set("net-tone", if behind { "warn" } else { "" });
        let rates = source.kind.as_phone().map(|p| p.rates()).unwrap_or_default();
        let tip = if !slow || !camera_limited {
            String::new()
        } else if rates.heat > 0 && (rates.dynamic == 0 || rates.heat <= rates.dynamic) {
            fill(&tr("The phone got hot and lowered the frame rate to {fps} fps for now."), &[("fps", &rates.heat.to_string())])
        } else if rates.dynamic > 0 {
            fill(&tr("Dynamic lowered the frame rate to {fps} fps to keep up with a slow connection."), &[("fps", &rates.dynamic.to_string())])
        } else {
            tr("The phone's camera is making fewer frames than asked for. Dim light slows it down.")
        };
        self.set("fps-tip", tip);
        self.note_throughput(source, behind);
    }

    /// A dropped stream isn't slow, and the first report after it's back still counts the gap, so neither says.
    fn note_throughput(self: &Rc<Self>, source: &SourceRef, behind: bool) {
        if source.recovering() {
            return;
        }
        let settling = source.state.borrow().settling_reports;
        if settling > 0 {
            source.state.borrow_mut().settling_reports -= 1;
            return;
        }
        self.set_behind(source, behind);
    }

    fn set_behind(self: &Rc<Self>, source: &SourceRef, behind: bool) {
        if source.state.borrow().behind != behind {
            source.state.borrow_mut().behind = behind;
            self.state.borrow_mut().bus.emit(Event::StreamBehind { source_id: source.id_str().into(), behind });
        }
    }

    /// _set_status: only trouble gets a colour.
    pub fn set_status(&self, msg: &str, kind: &str) {
        telescope_core::diagnostics::events().note(&format!("Status: {msg}"));
        if let Some((timer, _, _)) = self.streams.reconnecting.borrow().as_ref() {
            timer.stop();
        }
        let kind = match kind { "warn" => "status_warn", "err" => "status_err", _ => "status_dim" };
        self.set("status", msg.to_string());
        self.set("status-kind", kind);
    }

    /// Reconnecting status with its dots counting up once a second.
    fn start_reconnecting_animation(self: &Rc<Self>, base: &str) {
        telescope_core::diagnostics::events().note(&format!("Status: {base}"));
        let render = |w: &Window, base: &str, dots: u32| {
            w.set("status-kind", "status_warn");
            w.set("status", format!("{base}{}", ".".repeat(dots as usize)));
        };
        render(self, base, 1);
        let timer = slint::Timer::default();
        let weak = Rc::downgrade(self);
        timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), move || {
            if let Some(w) = weak.upgrade() {
                if let Some((_, base, dots)) = w.streams.reconnecting.borrow().as_ref() {
                    dots.set(dots.get() % 3 + 1);
                    render(&w, base, dots.get());
                }
            }
        });
        *self.streams.reconnecting.borrow_mut() = Some((timer, base.to_string(), Cell::new(1)));
    }

    /// The tile row: one per stream while there's more than one.
    pub fn refresh_tiles(&self) {
        let flow = self.streams.flow.borrow();
        let s = &flow.sessions;
        let mut entries: Vec<(SourceRef, u32, String, &str)> = s.streams.iter().map(|src| {
            let st = src.state.borrow();
            let has_worker = st.session.as_ref().is_some_and(|x| x.worker.is_some());
            let (state, kind) = if st.recovering {
                (tr("Reconnecting"), "status_warn")
            } else if st.connected || !has_worker {
                (tr("Live"), "status_ok")
            } else {
                (tr("Connecting"), "status_dim")
            };
            (src.clone(), st.slot, state, kind)
        }).collect();
        let free = |src: &SourceRef| {
            let used: Vec<u32> = s.streams.iter().filter(|x| !Rc::ptr_eq(x, src)).map(|x| x.slot()).collect();
            (0..MAX_SLOTS).find(|n| !used.contains(n)).unwrap_or(0)
        };
        if !s.streams.is_empty() {
            if let Some(src) = s.wake_source.clone().filter(|_| s.waking) {
                entries.push((src.clone(), free(&src), tr("Starting"), "status_dim"));
            }
            if let Some(src) = s.pending.clone() {
                entries.push((src.clone(), free(&src), tr("Waiting"), "status_dim"));
            }
        }
        let rows: Vec<SValue> = if entries.len() > 1 {
            entries.iter().map(|(src, slot, state, kind)| {
                let name = src.kind.name();
                SValue::Struct(Struct::from_iter([
                    ("id".to_string(), SValue::String(src.id_str().into())),
                    ("name".to_string(), SValue::String(name.as_str().into())),
                    // Translators: under a stream's tile, the virtual camera it goes to
                    ("output".to_string(), SValue::String(fill(&tr("to {output}"), &[("output", &slot_label(*slot))]).into())),
                    ("stop-label".to_string(), SValue::String(fill(&tr("Stop streaming {name}"), &[("name", &name)]).into())),
                    ("state".to_string(), SValue::String(state.as_str().into())),
                    ("kind".to_string(), SValue::String((*kind).into())),
                    ("focused".to_string(), SValue::Bool(crate::session::same(&s.focus, src))),
                    ("has-thumb".to_string(), SValue::Bool(false)),
                ]))
            }).collect()
        } else {
            Vec::new()
        };
        let _ = self.ui.set_property("tiles", SValue::Model(ModelRc::new(VecModel::from(rows))));
    }

    fn show_issue(self: &Rc<Self>, key: &str, title: &str, text: &str, actions: Vec<IssueAction>) {
        self.state.borrow_mut().banners.show(key, Issue {
            title: title.into(), text: text.into(), actions, kind: "warn".into(), details: String::new(), tell_dismiss: false,
        });
        self.show_banners();
    }

    fn clear_issue(&self, key: &str) {
        self.state.borrow_mut().banners.clear(Some(key), None);
        self.show_banners();
    }

    // ── What phones' threads report ──

    pub fn phone_report(self: &Rc<Self>, report: Report) {
        match report {
            Report::WakeProgress { source_id, wake_id, text } => {
                let source = self.source(Some(&source_id).filter(|s| !s.is_empty()).map(|s| s.as_str()));
                let current = source.kind.as_phone().is_some_and(|p| p.wake_id() == wake_id);
                if current && self.streams.flow.borrow().sessions.waking {
                    self.with_flow(|_, host| host.source_status(&source, &text, "dim"));
                }
            }
            Report::WakeDone { source_id, wake_id, result } => {
                let source = self.source(Some(source_id.as_str()).filter(|s| !s.is_empty()));
                let Some(phone) = source.kind.as_phone() else { return };
                let waking = self.streams.flow.borrow().sessions.is_waking(source.id.as_deref());
                if wake_id != phone.wake_id() || !waking {
                    // Stopped while it woke: the stop may have reached the phone first, leaving its camera on
                    if let Ok(session) = result {
                        let its_own_wake = crate::session::same(&self.streams.flow.borrow().sessions.wake_source, &source);
                        if phone.stops_late_wake() && !its_own_wake && !source.has_session() {
                            phone.stop_late(session);
                        } else {
                            std::thread::spawn(move || session.release());
                        }
                    }
                    return;
                }
                match result {
                    Ok(session) => {
                        phone.woke(session);
                        self.with_flow(|flow, host| flow.wake_done(host, &source, true, ""));
                    }
                    Err(Some(reason)) => self.with_flow(|flow, host| flow.wake_done(host, &source, false, &reason)),
                    Err(None) => self.with_flow(|flow, host| flow.wake_refused(host, &source)),
                }
            }
            Report::State { source_id, session_id, state } => {
                let source = self.source(Some(source_id.as_str()).filter(|s| !s.is_empty()));
                self.apply_state(&source, session_id, state);
            }
            Report::RecoveryProbed { source_id, session_id, generation, resolution } => {
                let source = self.source(Some(source_id.as_str()).filter(|s| !s.is_empty()));
                self.recovery_probed(&source, session_id, generation, resolution);
            }
            Report::HandedOver { source_id, session_id, url, session } => {
                let source = self.source(Some(source_id.as_str()).filter(|s| !s.is_empty()));
                let Some(phone) = source.kind.as_phone().filter(|p| p.handing_over()) else {
                    if let Some(s) = session {
                        std::thread::spawn(move || s.release()); // stopped meanwhile
                    }
                    return;
                };
                match session {
                    Some(_) if source.session().is_some_and(|s| s.id == session_id) => self.moved_over(&source, None, &url),
                    _ => phone.hand_over_failed(&url, true),
                }
            }
            Report::Moved { source_id, session_id, route, session } => {
                let source = self.source(Some(source_id.as_str()).filter(|s| !s.is_empty()));
                let Some(current) = source.session().filter(|s| s.id == session_id) else {
                    session.release();
                    return;
                };
                let Some(phone) = source.kind.as_phone() else { return };
                let url = session.video_url();
                let opener: crate::worker::Opener = { let s = session.clone(); Arc::new(move || s.open_video().map(crate::worker::Opened::from)) };
                if let Some(controls) = session.controls() {
                    controls.retarget(&session.stream_base()); // the same client, so its settings carry on
                }
                let tries = current.worker.as_ref().map_or(0, |w| w.failed_tries());
                phone.moved(route, session, tries);
                let worker = current.worker.as_ref().and_then(|w| self.shell_worker(w));
                if let Some(worker) = &worker {
                    worker.inner.retarget(&url, opener.clone());
                }
                let mut moved = current.clone();
                moved.url = url;
                self.streams.flow.borrow_mut().sessions.add(&source, moved);
                if worker.is_none() {
                    self.with_flow(|flow, host| flow.mic_reconnected(host, &source)); // the mic follows the new route, and nothing else waits for it
                }
            }
            Report::Heat { source_id, session_id, state } => {
                let source = self.source(Some(source_id.as_str()).filter(|s| !s.is_empty()));
                if let Some(state) = state.filter(|_| source.session().is_some_and(|s| s.id == session_id)) {
                    self.show_heat(&source, &state);
                }
            }
            Report::Alive { source_id, session_id, ok } => {
                let source = self.source(Some(source_id.as_str()).filter(|s| !s.is_empty()));
                let Some(phone) = source.kind.as_phone() else { return };
                if source.session().is_none_or(|s| s.id != session_id || s.worker.is_some()) {
                    return;
                }
                let lost = phone.note_alive(ok);
                if ok && source.recovering() {
                    self.with_flow(|flow, host| flow.mic_reconnected(host, &source));
                } else if lost && !source.recovering() {
                    self.start_reconnecting_animation(&tr("Lost the phone - reconnecting"));
                    self.with_flow(|flow, host| flow.begin_recovery(host, Some(source.clone())));
                }
            }
            Report::CameraCheck { source_id, session_id, state } => {
                let source = self.source(Some(source_id.as_str()).filter(|s| !s.is_empty()));
                let Some(state) = state else { return };
                if source.session().is_none_or(|s| s.id != session_id) || source.camera_off() {
                    return;
                }
                // The phone says whether the camera came back on; one that couldn't (in use elsewhere) stays off
                if truthy(&state, "camera_off") && truthy(&state, "camera_error") {
                    source.state.borrow_mut().camera_off = true;
                    self.with_flow(|flow, host| flow.camera_stayed_off(host, &source));
                    let again = IssueAction { label: tr("Try again"), does: IssueDoes::Plugin { id: format!("{CAMERA_AGAIN}{}", source.id_str()) } };
                    self.show_issue("camera", &tr("The camera didn't turn back on"), &camera_error_text(&state), vec![again]);
                }
            }
            Report::CameraRate { source_id, session_id, arrival, rates } => {
                let source = self.source(Some(source_id.as_str()).filter(|s| !s.is_empty()));
                let Some(phone) = source.kind.as_phone() else { return };
                let current = source.session().is_some_and(|s| s.id == session_id) && self.is_shown(&source);
                phone.rates_came(current.then_some(rates));
                if current && source.state.borrow().arrival_slow {
                    self.show_slow(&source, true, limited_by_camera(rates, arrival));
                }
            }
        }
    }

    /// Host::hand_over_stream (PhoneSource.hand_over): the worker tries link's prepared url next to the running stream.
    pub fn hand_over(self: &Rc<Self>, source_id: &str, route: RouteSpec, url: String) {
        let source = self.source(Some(source_id));
        let worker = source.session().and_then(|s| s.worker).and_then(|w| self.shell_worker(&w));
        let phone = source.kind.as_phone();
        let opener = phone.zip(source.session()).and_then(|(p, s)| p.begin_hand_over(s.id, route, url.clone(), worker.is_none()));
        if !phone.is_some_and(PhoneKind::handing_over) {
            if let Some(link) = self.services.as_ref().map(|s| s.link.clone()) {
                link.drop_route(source_id, false);
            }
        } else if let (Some(opener), Some(worker)) = (opener, worker) {
            if !worker.inner.hand_over(&url, opener) {
                phone.unwrap().hand_over_failed(&url, false);
            }
        }
        self.sync_streams();
    }

    /// PhoneSource.on_moved: the stream runs on url now, with the same control client, and the plugins hear about it as
    /// a reconnect.
    fn moved_over(self: &Rc<Self>, source: &SourceRef, worker: Option<&Rc<ShellWorker>>, url: &str) {
        let Some(phone) = source.kind.as_phone() else { return };
        let Some(current) = source.session() else { return };
        let Some(session) = phone.moved_over(current.id, url) else {
            self.sync_streams();
            return;
        };
        log::info!("{} moved to {url}", source.kind.name());
        if let Some(controls) = session.controls() {
            controls.retarget(&session.stream_base());
        }
        if let Some(worker) = worker {
            let s = session.clone();
            worker.inner.set_opener(Arc::new(move || s.open_video().map(crate::worker::Opened::from)));
        }
        let mut moved = current.clone();
        moved.url = session.video_url();
        let camera_off = moved.worker.is_none();
        self.streams.flow.borrow_mut().sessions.add(source, moved);
        self.with_flow(|flow, host| if camera_off { flow.mic_reconnected(host, source) } else { flow.stream_reconnected(host, source) });
    }

    /// Picks id as Connection does before a start, so its settings are in (app.py _start_source's conn.select): the event is
    /// delivered here, as the start runs right after.
    fn selected_now(&self) -> Option<String> {
        self.services.as_ref().and_then(|s| s.link.picked_source()).or_else(|| self.state.borrow().selected.clone())
    }

    fn select_first(self: &Rc<Self>, id: Option<&str>) {
        let Some(id) = id else { return };
        if self.selected_now().as_deref() != Some(id) {
            self.state.borrow_mut().bus.emit(Event::SelectSource { source_id: id.to_string() });
            self.process();
        }
    }

    /// HostSources::add_stream_source. The same source offered again (a new name) carries on streaming.
    pub fn add_plugin_source(self: &Rc<Self>, source: Arc<dyn StreamSource>) {
        let id = source.id();
        let known = self.streams.plugin_sources.borrow().get(&id).cloned();
        match known {
            Some(cell) => *cell.lock().unwrap() = source,
            None => {
                self.streams.plugin_sources.borrow_mut().insert(id.clone(), Arc::new(Mutex::new(source)));
                // Asked for before it was offered, it was taken for a phone
                self.streams.sources.borrow_mut().remove(&id);
            }
        }
        self.announce_sources();
        if self.streams.flow.borrow().sessions.streams.len() > 1 {
            self.refresh_tiles(); // a new name for one that streams
        }
    }

    /// HostSources::remove_stream_source.
    pub fn remove_plugin_source(self: &Rc<Self>, id: &str) {
        if !self.streams.plugin_sources.borrow().contains_key(id) {
            return;
        }
        if self.streams.flow.borrow().sessions.is_streaming_from(Some(id)) {
            self.with_flow(|flow, host| flow.stop_stream(host, Some(id)));
        }
        let idle = self.streams.flow.borrow().sessions.streams.is_empty();
        let next = self.streams.plugin_sources.borrow().keys().find(|s| s.as_str() != id).cloned();
        if idle && self.selected_now().as_deref() == Some(id) {
            // Connection first, while it still knows the source it's leaving
            self.state.borrow_mut().bus.emit(Event::SelectSource { source_id: next.clone().unwrap_or_default() });
            self.process();
            self.with_flow(|_, host| host.select_device(next.as_deref()));
        }
        self.streams.plugin_sources.borrow_mut().remove(id);
        self.streams.sources.borrow_mut().remove(id);
        self.announce_sources();
    }

    fn announce_sources(&self) {
        let sources = self.streams.plugin_sources.borrow().iter()
            .map(|(id, s)| telescope_core::event::SourceName { id: id.clone(), name: s.lock().unwrap().name() }).collect();
        self.state.borrow_mut().bus.emit(Event::StreamSourcesChanged { sources });
    }

    /// HostSources::stream_source: picked and streamed when nothing streams yet, else next to the others.
    pub fn stream_plugin_source(self: &Rc<Self>, id: &str) {
        let flow = self.streams.flow.borrow();
        let busy = flow.sessions.is_starting() || flow.sessions.is_streaming_from(Some(id));
        let streaming = !flow.sessions.streams.is_empty();
        drop(flow);
        if busy || !self.streams.plugin_sources.borrow().contains_key(id) {
            return;
        }
        if !streaming {
            self.select_first(Some(id));
        }
        self.with_flow(|flow, host| {
            if streaming {
                return flow.add_stream(host, id);
            }
            if host.selected_device().as_deref() != Some(id) {
                host.select_device(Some(id));
            }
            let source = host.source_by_id(Some(id));
            flow.start(host, false, Some(source));
        });
    }

    fn shell_worker(&self, w: &Rc<dyn Worker>) -> Option<Rc<ShellWorker>> {
        self.streams.workers.borrow().iter().find(|x| Rc::as_ptr(x) as *const () == Rc::as_ptr(w) as *const ()).cloned()
    }

    /// _sync_sensors: the phone sensors follow the stream the panels show (SensorHub.follow), none once it stops, and tell
    /// the plugins when its readings change.
    fn follow_sensors(&self, state: Option<&Value>) {
        let focus = self.streams.flow.borrow().sessions.focus.clone().filter(|s| s.session().is_some());
        let st = self.state.borrow();
        let Some(sensors) = st.link.as_ref().and_then(|l| l.sensors()) else { return };
        let bus = st.bus.sender();
        sensors.set_on_change(Box::new(move |id| bus.emit(Event::SensorsChanged { source_id: id.to_string() })));
        let Some(source) = focus else { return sensors.follow(None, None, None, None) };
        match source.kind.as_phone().and_then(|p| p.still_target()) {
            Some((base, phone)) => sensors.follow(Some(source.id_str()), Some(&base), Some(&phone), state),
            None => sensors.follow(Some(source.id_str()), None, None, state),
        }
    }

    /// _apply_state: the phone's state, to the plugins that hear about its stream.
    fn apply_state(self: &Rc<Self>, source: &SourceRef, session_id: u64, state: Value) {
        if source.session().is_none_or(|s| s.id != session_id) {
            return; // a session stopped or switched since the fetch began: its state isn't this stream's
        }
        if let Err(e) = telescope_core::models::PhoneState::from_value(&state) {
            log::error!("Phone sent a malformed /v1/state response - not applying it: {e}");
            self.set_status(&tr("The phone sent data this version can't read. Update both apps to the same version."), "err");
            return;
        }
        let shown = crate::session::same(&self.streams.flow.borrow().sessions.focus, source);
        if !shown && self.codec_trouble(source, &state) {
            if self.streams.flow.borrow().sessions.is_starting() {
                // The panels belong to that start for now
                let held = (source.clone(), session_id, state.clone());
                self.streams.flow.borrow_mut().sessions.held_states.insert(source.id_str().into(), held);
            } else {
                let s = source.clone();
                self.with_flow(move |flow, host| flow.move_focus(host, &s)); // Stream output falls back, or stops and says why, for the panels' stream
            }
        }
        let focused = crate::session::same(&self.streams.flow.borrow().sessions.focus, source);
        self.state.borrow().bus.emit(Event::StreamState { source_id: source.id_str().into(), state: state.clone() });
        // What each lens can take, from every stream's state and not only the shown one's
        if let Some(stills) = self.state.borrow().link.as_ref().and_then(|l| l.stills()) {
            stills.note_state(Some(source.id_str()), &state);
        }
        if focused {
            self.follow_sensors(Some(&state));
            let mut st = self.state.borrow_mut();
            st.streams.phone_state = Some(state.clone());
            st.bus.emit(Event::PhoneStateUpdated { state: state.clone() });
            drop(st);
            self.process();
            if source.session().is_none_or(|s| s.id != session_id) {
                return; // something listening (Monitoring, too hot) stopped the stream just now
            }
        }
        let hearing = self.streams.flow.borrow().sessions.hearing(&self.plugin_list(), source);
        {
            let mut st = self.state.borrow_mut();
            for i in 0..st.slots.len() {
                if hearing.contains(&st.slots[i].info.name) {
                    st.call(i, |p, host| p.on_phone_state(host, &state));
                }
            }
            if hearing.iter().any(|n| n == "view") {
                let st = &mut *st;
                let said = st.vc.phone_state(&st.view, &state, crate::shell::now_s());
                st.apply_said(said);
            }
        }
        self.process();
        if state.as_object().is_some_and(|o| !o.is_empty()) && source.has_session() {
            self.on_phone_state(source, &state);
            self.process();
        }
    }

    /// _codec_trouble: a stream the panels don't show asks its phone for H.264 (Light, the default) and the phone says it
    /// can't: no encoder at all, or one that failed.
    fn codec_trouble(&self, source: &SourceRef, state: &Value) -> bool {
        // A phone leaves codecs out when it's only MJPEG
        let no_h264 = state.get("cameras").is_some()
            && !state.get("codecs").and_then(Value::as_array).is_some_and(|c| c.iter().any(|x| x == "h264"));
        if !no_h264 && !truthy(state, "codec_error") {
            return false;
        }
        let cfg = self.state.borrow().file.load();
        crate::profiles::device_config(&cfg, source.id.as_deref(), "stream_output").get("format").and_then(Value::as_str) != Some("mjpeg")
    }

    /// PhoneSource.on_phone_state: the camera taken by another app, and the heat note.
    fn on_phone_state(self: &Rc<Self>, source: &SourceRef, state: &Value) {
        let key = format!("camera_taken:{}", source.id_str());
        if state.get("camera_taken").and_then(Value::as_bool) == Some(true) {
            // Android gives the camera to the app on screen; the phone opens it again once that app lets go
            self.show_issue(&key, &tr("Another app on the phone is using the camera"),
                            &tr("The stream comes back by itself once that app closes or lets go of it."), vec![]);
        } else {
            self.clear_issue(&key);
        }
        self.show_heat(source, state);
        let toggle = truthy(state, "camera_toggle");
        source.state.borrow_mut().camera_toggle = Some(toggle);
        if !toggle {
            if source.camera_off() {
                self.set_camera_on(true, false);
                self.show_issue("camera", &tr("This phone can't turn its camera off yet"),
                                &tr("Update Telescope on the phone to stream the mic with the camera off."), vec![]);
            }
            return;
        }
        if truthy(state, "camera_off") != source.camera_off() {
            if let Some(session) = source.session() {
                session.client.send(&serde_json::json!({"action": "camera_on", "value": if source.camera_off() { 0 } else { 1 }}));
            }
        }
    }

    /// Says so while the phone streams a smaller picture than asked because it's critically hot.
    fn show_heat(self: &Rc<Self>, source: &SourceRef, state: &Value) {
        let heat_key = format!("heat:{}", source.id_str());
        match heat_text(state) {
            Some(text) => {
                self.show_issue(&heat_key, &tr("The phone is very hot"), &text, vec![]);
                if let Some(p) = source.kind.as_phone() {
                    p.note_heat(true);
                }
            }
            None => {
                if source.kind.as_phone().is_some_and(|p| p.note_heat(false)) {
                    self.clear_issue(&heat_key);
                }
            }
        }
    }

    fn recovery_probed(self: &Rc<Self>, source: &SourceRef, session_id: u64, generation: u64, res: telescope_core::link::Resolution) {
        let Some(phone) = source.kind.as_phone() else { return };
        let Some(session) = source.session() else { return };
        if !source.recovering() || generation != phone.recovery_generation() || session.id != session_id {
            return;
        }
        let streaming = self.streams.flow.borrow().sessions.streams.len() > 1;
        let name = source.kind.name();
        match res.status {
            PhoneStatus::NotPaired | PhoneStatus::LocalOnly | PhoneStatus::PhoneOutdated | PhoneStatus::DesktopOutdated => {
                // The phone answers but won't take the stream back: say why instead of retrying forever.
                let s = source.clone();
                self.with_flow(move |flow, host| flow.stop(host, false, Some(s), false));
                phone.show_problem(&res);
                return;
            }
            PhoneStatus::Ready if !res.streaming_for.is_empty() => {
                // Another computer started the phone while this one couldn't reach it: that stream is theirs.
                let s = source.clone();
                self.with_flow(move |flow, host| flow.stop(host, false, Some(s), false));
                let title = if !streaming {
                    fill(&tr("The phone is streaming to {computer}"), &[("computer", &res.streaming_for)])
                } else {
                    sentence(&fill(&tr("{phone} is streaming to {computer}"), &[("phone", &name), ("computer", &res.streaming_for)]))
                };
                self.show_issue(&format!("stopped:{}", source.id_str()), &title,
                                &tr("Stop it there or on the phone, then start again."), vec![start_action(source.id.clone())]);
                return;
            }
            PhoneStatus::Ready if !res.streaming && !res.busy => {
                // Stopped on the phone, or by its idle watchdog while we couldn't reach it.
                let s = source.clone();
                self.with_flow(move |flow, host| flow.stop(host, false, Some(s), false));
                let title = if !streaming {
                    tr("The phone stopped streaming")
                } else {
                    sentence(&fill(&tr("{phone} stopped streaming"), &[("phone", &name)]))
                };
                self.show_issue(&format!("stopped:{}", source.id_str()), &title, &tr("Start again when you're ready."),
                                vec![start_action(source.id.clone())]);
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

    /// A phone source's timers: park, the unreachable banner, the next recovery probe, the heat check.
    fn source_timer(self: &Rc<Self>, what: &str, sid: &str) {
        let source = self.source(Some(sid).filter(|s| !s.is_empty()));
        match what {
            "park" => {
                let flow = self.streams.flow.borrow();
                let mut host = Host { w: self };
                flow.park(&mut host, &source);
                drop(flow);
                self.process();
            }
            "unreachable" => {
                if !source.recovering() || !source.has_session() {
                    return;
                }
                if let Some(p) = source.kind.as_phone() {
                    p.note_unreachable(true);
                }
                let title = fill(&tr("Can't reach {name}"), &[("name", &source.kind.name())]);
                self.show_issue(&format!("stopped:{}", source.id_str()), &title, &tr("Open Telescope on it."), vec![]);
            }
            "clear_unreachable" => self.clear_issue(&format!("stopped:{}", source.id_str())),
            "clear_camera_taken" => self.clear_issue(&format!("camera_taken:{}", source.id_str())),
            "clear_heat" => self.clear_issue(&format!("heat:{}", source.id_str())),
            "probe" => {
                if let (Some(p), Some(session)) = (source.kind.as_phone(), source.session()) {
                    if source.recovering() {
                        p.probe(session.id);
                    }
                }
            }
            "heat" => {
                if let (Some(p), Some(session)) = (source.kind.as_phone(), source.session()) {
                    p.check_heat(session.id);
                }
            }
            "alive" => {
                if let (Some(p), Some(session)) = (source.kind.as_phone(), source.session()) {
                    if session.worker.is_none() {
                        p.check_alive(session.id);
                    }
                }
            }
            "camera_check" => {
                if let (Some(p), Some(session)) = (source.kind.as_phone(), source.session().filter(|_| !source.camera_off())) {
                    p.check_camera_on(session.id);
                }
            }
            _ => {}
        }
    }

    fn plugin_list(&self) -> Vec<(String, bool)> {
        // The window's view controller hears the shown stream like a plugin that follows the panels
        let mut list: Vec<(String, bool)> = self.state.borrow().slots.iter().map(|s| (s.info.name.clone(), s.info.follows_focus)).collect();
        list.push(("view".into(), true));
        list
    }

    /// At quit: every stream stopped and the phones told, waiting a moment for those stops.
    pub fn stop_streams_for_quit(self: &Rc<Self>) {
        self.stop_all_streams();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        for source in self.streams.sources.borrow().values() {
            if let Some(p) = source.kind.as_phone() {
                p.drain_stops(deadline);
            }
        }
    }
}

fn start_action(source_id: Option<String>) -> IssueAction {
    IssueAction { label: tr("Start"), does: IssueDoes::StartAgain { source_id, before: None } }
}

/// The first letter in capitals (i18n.sentence), for a text that starts with a name.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().collect::<String>() + chars.as_str())
}

/// A source for a window without services (scene renders): nothing to stream.
struct NoSource;

impl StreamSource for NoSource {
    fn id(&self) -> String {
        String::new()
    }
    fn name(&self) -> String {
        tr("the phone")
    }
    fn url(&self) -> String {
        String::new()
    }
    fn prepare(&self, _interactive: bool) -> bool {
        false
    }
    fn open_feed(&self) -> Result<(telescope_core::media::Codec, Box<dyn telescope_core::media::VideoFeed>), String> {
        Err("no services".into())
    }
    fn control_client(&self) -> Option<Box<dyn telescope_core::source::ControlClient>> {
        None
    }
    fn fps(&self) -> u32 {
        30
    }
}

impl FlowHost for Host<'_> {
    fn plugins(&self) -> Vec<(String, bool)> {
        self.w.plugin_list()
    }
    fn show_selected(&mut self, source_id: Option<&str>) {
        let mut st = self.w.state.borrow_mut();
        st.selected = source_id.map(str::to_string);
        st.bus.emit(Event::SourceShown { source_id: source_id.unwrap_or("").to_string() });
    }
    fn call(&mut self, plugins: &[String], hook: &Hook) {
        self.w.state.borrow_mut().call_hook(plugins, hook);
    }
    fn emit(&mut self, event: Event) {
        self.w.state.borrow_mut().bus.emit(event);
    }
    fn can_start(&self) -> bool {
        self.w.services.is_some()
    }
    fn camera_ready(&mut self, interactive: bool, source_id: Option<&str>) -> bool {
        let mut state = self.w.state.borrow_mut();
        let Some(i) = state.index("connection") else { return true };
        state.call(i, |plugin, ctx| plugin.ensure_virtual_camera(ctx, interactive, source_id)).unwrap_or(true)
    }
    fn selected_device(&self) -> Option<String> {
        self.w.selected_now()
    }
    fn select_device(&mut self, source_id: Option<&str>) {
        let prev = self.w.state.borrow().selected.clone();
        if prev.as_deref() != source_id {
            self.w.state.borrow_mut().selected = source_id.map(String::from);
            self.w.state.borrow_mut().effects.push(crate::shell::Effect::SaveSoon);
        }
    }
    fn source_by_id(&mut self, source_id: Option<&str>) -> SourceRef {
        self.w.source(source_id)
    }
    fn plugin_source(&mut self, source_id: &str) -> Option<SourceRef> {
        let known = self.w.streams.plugin_sources.borrow().contains_key(source_id);
        known.then(|| self.w.source(Some(source_id)))
    }
    fn later(&mut self, what: Later) {
        self.w.streams.later.borrow_mut().push(what);
    }
    fn start_problem(&mut self, title: &str, text: &str) {
        self.w.state.borrow_mut().banners.show("start", Issue {
            title: title.into(), text: text.into(), actions: vec![IssueAction { label: tr("Try again"), does: IssueDoes::StartAgain { source_id: None, before: None } }],
            kind: "warn".into(), details: String::new(), tell_dismiss: false,
        });
    }
    fn clear_issue(&mut self, key: &str) {
        self.w.state.borrow_mut().banners.clear(Some(key), None);
    }
    fn set_start_button(&mut self, streaming: bool) {
        if streaming {
            self.w.show_streaming();
        } else {
            self.w.show_idle_button();
        }
    }
    fn enable_start(&mut self, enabled: bool) {
        let _ = self.w.ui.set_property("start-enabled", SValue::Bool(enabled));
    }
    fn source_status(&mut self, source: &SourceRef, msg: &str, kind: &str) {
        // The flow only reports on the source it's starting, which is the one the footer shows
        source.state.borrow_mut().status = (msg.into(), kind.into());
        self.w.set_status(msg, kind);
    }
    fn show_stream(&mut self, source: &SourceRef) {
        self.w.clear_pending_resolution(source);
        self.w.set("fps-tone", "");
        self.w.set("fps-tip", "");
        self.w.set("net-tone", "");
        let st = source.state.borrow();
        self.w.set("fps", st.fps_text.clone());
        self.w.set("net", st.net_text.clone());
        let (msg, kind) = st.status.clone();
        drop(st);
        self.w.set_status(&msg, &kind);
    }
    fn show_stopped(&mut self) {
        self.w.set("fps", "—");
        self.w.set("fps-tip", "");
        self.w.set("net-tone", "");
        self.w.set("net", "—");
        self.w.set_status(&tr("Not streaming"), "dim");
    }
    fn show_camera_off(&mut self) {
        self.w.set("fps", "—");
        self.w.set("fps-tip", "");
        self.w.set("net-tone", "");
        self.w.set("net", "—");
    }
    fn refresh_tiles(&mut self) {}
    fn clear_pending_resolution(&mut self, source: &SourceRef) {
        self.w.clear_pending_resolution(source);
    }
    fn set_behind(&mut self, behind: bool, source: &SourceRef) {
        if source.state.borrow().behind != behind {
            source.state.borrow_mut().behind = behind;
            self.w.state.borrow_mut().bus.emit(Event::StreamBehind { source_id: source.id_str().into(), behind });
        }
    }
    fn save_profile(&mut self, source_id: Option<&str>, plugins: &[String]) {
        self.w.state.borrow_mut().save_profile(source_id, plugins);
    }
    fn apply_device_profile(&mut self, source_id: Option<&str>, plugins: &[String]) {
        self.w.state.borrow_mut().apply_profile(source_id, plugins);
        // set_config has no host to refresh its frame step, so the shown stream takes every step again
        self.w.streams.workers.borrow().iter().filter(|w| w.live.get()).for_each(|w| w.steps_due.set(true));
    }
    fn schedule_save(&mut self) {
        self.w.state.borrow_mut().effects.push(crate::shell::Effect::SaveSoon);
    }
    fn canvas_size(&self) -> Option<(u32, u32)> {
        let setup = self.w.state.borrow().plugin_config("setup")?;
        let (width, height) = telescope_core::canvas::canvas_dims(&setup);
        width.zip(height)
    }
    fn slot_ready(&self, slot: u32) -> bool {
        slot == 0 || self.w.services.as_ref().is_some_and(|s| s.outputs.slots().iter().any(|x| x.slot == slot))
    }
    fn set_up_slots(&mut self, source_id: &str) {
        self.w.set_up_slots(source_id);
    }
    fn mic_enabled(&self) -> bool {
        let st = self.w.state.borrow();
        st.plugin_config("microphone").is_some_and(|c| c.get("enabled") == Some(&Value::Bool(true)))
    }
    fn apply_state(&mut self, session_id: u64, state: &Value, source: &SourceRef) {
        let (w, source, state) = (self.w.clone(), source.clone(), state.clone());
        slint::Timer::single_shot(Duration::ZERO, move || w.apply_state(&source, session_id, state));
    }
    fn new_worker(&mut self, source: &SourceRef, url: &str, params: (Option<u32>, Option<u32>, u32), live: bool,
                  canvas: (Option<u32>, Option<u32>)) -> Rc<dyn Worker> {
        let services = self.w.services.clone().expect("streams need services");
        // Setup's switch counts from the next stream on, as h264_reader.set_hardware does; --vaapi turns it on for a run
        let setup = self.w.state.borrow().plugin_config("setup");
        let chosen = setup.is_some_and(|c| c.get("hw_decode") == Some(&Value::Bool(true)));
        let hardware = chosen || self.w.opts.as_ref().is_some_and(|o| o.hardware_decode);
        let id = self.w.streams.next_worker.get() + 1;
        self.w.streams.next_worker.set(id);
        let opener = source.kind.opener().unwrap_or_else(|| Arc::new(|| Err(tr("Couldn't reach the phone."))));
        // Before the thread starts, so no frame goes out without them
        let steps = self.w.state.borrow_mut().pipeline_steps(source.id.as_deref().filter(|id| !id.is_empty()));
        let setup = WorkerSetup { url: url.into(), opener, width: params.0, height: params.1, fps: params.2, canvas,
                                  slot: source.slot(), source_id: source.id_str().into(), hardware_decode: hardware,
                                  plugin_source: source.kind.as_phone().is_none(), waiting_text: source.kind.waiting_text(),
                                  vision: self.w.state.borrow().vision_feed(), steps, gate: Some(self.w.state.borrow().vc.gate()) };
        let slot: Arc<dyn FrameSlot> = services.media.stream_frames().map(|f| f.new_slot()).unwrap_or_else(local_slot);
        let weak = self.w.ui.as_weak();
        let events: crate::worker::Events = Arc::new(move |e| {
            if matches!(e, WorkerEvent::Preview(..)) {
                return;
            }
            let _ = weak.upgrade_in_event_loop(move |_| with_window(|w| w.worker_event(id, e)));
        });
        let maker = self.w.streams.maker.borrow().clone();
        let inner: Arc<dyn VideoWorker> = match maker {
            Some(make) => make(setup, slot, events),
            None => Arc::new(StreamWorker::start(setup, services.media.clone(), services.outputs.clone(), slot, events)
                .expect("a thread for the stream worker")),
        };
        let worker = Rc::new(ShellWorker { id, inner, live: Cell::new(live), steps_due: Cell::new(false), makers: self.w.streams.packet_makers.clone() });
        self.w.streams.workers.borrow_mut().push(worker.clone());
        worker
    }
    fn end_worker(&mut self, worker: Rc<dyn Worker>) {
        let Some(w) = self.w.shell_worker(&worker) else { return };
        self.w.streams.workers.borrow_mut().retain(|x| x.id != w.id);
        let inner = w.inner.clone();
        if let (Some(vision), Some(id)) = (self.w.state.borrow().vision.clone(), inner.source_id()) {
            vision.drop_stream(id);
        }
        // Off the UI thread, so a stalled read can't freeze the window.
        std::thread::spawn(move || inner.stop());
    }
    fn packet_steps(&self) -> Vec<String> {
        self.w.streams.packet_makers.borrow().owners()
    }
    fn ask_keyframe(&mut self, source: &SourceRef) {
        ask_keyframe(source);
    }
    fn freeze_steps(&mut self, source: &SourceRef) {
        if let Some(w) = self.w.worker_of(source) {
            let steps = self.w.state.borrow_mut().pipeline_steps(w.inner.source_id());
            w.inner.set_steps(steps);
        }
    }
}

/// Steps came on or off source's H.264: a keyframe gives a new one a start and brings back a picture they broke.
fn ask_keyframe(source: &SourceRef) {
    if let Some(session) = source.session().filter(|s| s.url.ends_with(".h264") || s.url.ends_with(".udp")) {
        session.client.send(&serde_json::json!({"action": "keyframe"}));
    }
}

/// A frame slot for a media that keeps none (tests, builds without media): the newest frame only.
fn local_slot() -> Arc<dyn FrameSlot> {
    #[derive(Default)]
    struct S(std::sync::Mutex<(Option<telescope_core::frame::Frame>, u64)>, std::sync::Condvar);
    impl FrameSlot for S {
        fn publish(&self, frame: telescope_core::frame::Frame) -> u64 {
            let mut st = self.0.lock().unwrap();
            st.1 += 1;
            st.0 = Some(frame);
            self.1.notify_all();
            st.1
        }
        fn published(&self) -> Option<telescope_core::frame::Frame> {
            self.0.lock().unwrap().0.clone()
        }
        fn replace(&self, _frame: telescope_core::frame::Frame, _number: u64) -> bool {
            false
        }
        fn latest(&self) -> Option<(telescope_core::frame::Frame, u64)> {
            let st = self.0.lock().unwrap();
            st.0.clone().map(|f| (f, st.1))
        }
        fn wait_newer(&self, seen: u64, timeout: Duration) -> bool {
            let st = self.0.lock().unwrap();
            self.1.wait_timeout_while(st, timeout, |st| st.1 <= seen).unwrap().0 .1 > seen
        }
    }
    Arc::new(S::default())
}
