//! The window's side of the stream flow (app.py's _FlowHost and _SourceHost, and the worker's signals): sources by
//! id, workers, the footer and tiles, and what phones' threads report back.

use crate::session::{Source, SourceRef, Worker};
use crate::sources::{camera_error_text, heat_text, PhoneKind, PluginKind, Report};
use crate::stream_flow::{FlowHost, Hook, Later, StreamFlow, MAX_SLOTS};
use crate::texts::fill;
use crate::window::{with_window, Window};
use crate::worker::{StreamWorker, WorkerEvent, WorkerSetup};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, VecModel};
use slint_interpreter::{Struct, Value as SValue};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use telescope_core::event::Event;
use telescope_core::i18n::tr;
use telescope_core::link::PhoneStatus;
use telescope_core::media::FrameSlot;
use telescope_core::source::{RouteSpec, StreamSource};
use telescope_core::ui::{Issue, IssueAction, IssueDoes};

/// A running worker as the flow sees it.
pub struct ShellWorker {
    pub id: u64,
    pub inner: Arc<StreamWorker>,
    live: Cell<bool>,
    /// The panels moved: its frame steps are taken again once the flow's call is through.
    steps_due: Cell<bool>,
    /// The plugins' packet step makers, shared with Streams: set_packets hands the tap those of the owners it's given.
    makers: Rc<RefCell<crate::shell::PacketMakers>>,
}

impl Worker for ShellWorker {
    fn set_pipeline(&self, live: bool) {
        self.live.set(live);
        self.steps_due.set(true);
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
    pub plugin_sources: RefCell<BTreeMap<String, Arc<dyn StreamSource>>>,
    workers: RefCell<Vec<Rc<ShellWorker>>>,
    next_worker: Cell<u64>,
    later: RefCell<Vec<Later>>,
    reconnecting: RefCell<Option<(slint::Timer, String, Cell<u32>)>>,
    /// The packet steps plugins asked for, which go with the panels from stream to stream.
    packet_makers: Rc<RefCell<crate::shell::PacketMakers>>,
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
            return Box::new(crate::sources::PluginKind { source: Arc::new(NoSource) });
        };
        let weak = self.ui.as_weak();
        let post: crate::sources::Post = Arc::new(move |report| {
            let _ = weak.upgrade_in_event_loop(move |_| with_window(|w| w.phone_report(report)));
        });
        let me = Rc::downgrade(self);
        let on_timer: Rc<dyn Fn(&str, &str)> = Rc::new(move |what, sid| {
            if let Some(w) = me.upgrade() {
                w.source_timer(what, sid);
            }
        });
        let me = Rc::downgrade(self);
        let format: Rc<dyn Fn() -> telescope_core::link::StreamFormat> = Rc::new(move || {
            // Light (H.264) until the Stream output card is ported and says otherwise
            let cfg = me.upgrade().and_then(|w| w.state.borrow().plugin_config("stream_output"))
                .unwrap_or_else(|| serde_json::json!({"format": "h264"}));
            let dynamic = cfg.get("bitrate_mbps").and_then(Value::as_f64) == Some(-1.0);
            telescope_core::link::StreamFormat { h264: cfg.get("format").and_then(Value::as_str) == Some("h264"), dynamic }
        });
        Box::new(PhoneKind::new(id.map(String::from), services.link.clone(), format, opts.request, post, on_timer))
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
            "fps" => self.set("fps", msg.split("  ").next().unwrap_or("").trim_end_matches(" fps").to_string()),
            "net" => {
                source.state.borrow_mut().arrival_slow = false;
                self.set("net", msg.to_string());
                self.note_throughput(source, false);
            }
            "net_warn" => {
                source.state.borrow_mut().arrival_slow = true;
                self.set("net", msg.to_string());
                // The phone's camera rate isn't asked yet, so slow frames count as the link's
                self.note_throughput(source, true);
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
                self.set("fps", "—");
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
        if let Some((timer, _, _)) = self.streams.reconnecting.borrow().as_ref() {
            timer.stop();
        }
        let kind = match kind { "warn" => "status_warn", "err" => "status_err", _ => "status_dim" };
        self.set("status", msg.to_string());
        self.set("status-kind", kind);
    }

    /// Reconnecting status with its dots counting up once a second.
    fn start_reconnecting_animation(self: &Rc<Self>, base: &str) {
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

    fn phone_report(self: &Rc<Self>, report: Report) {
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
                        if !source.has_session() {
                            phone.stop_late(session);
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
                if let Some(worker) = current.worker.as_ref().and_then(|w| self.shell_worker(w)) {
                    worker.inner.retarget(&url, opener);
                }
                let mut moved = current.clone();
                moved.url = url;
                self.streams.flow.borrow_mut().sessions.add(&source, moved);
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
        self.streams.flow.borrow_mut().sessions.add(source, moved);
        self.with_flow(|flow, host| flow.stream_reconnected(host, source));
    }

    fn shell_worker(&self, w: &Rc<dyn Worker>) -> Option<Rc<ShellWorker>> {
        self.streams.workers.borrow().iter().find(|x| Rc::as_ptr(x) as *const () == Rc::as_ptr(w) as *const ()).cloned()
    }

    /// _apply_state: the phone's state, to the plugins that hear about its stream.
    fn apply_state(self: &Rc<Self>, source: &SourceRef, session_id: u64, state: Value) {
        if source.session().is_none_or(|s| s.id != session_id) {
            return; // a session stopped or switched since the fetch began: its state isn't this stream's
        }
        let focused = crate::session::same(&self.streams.flow.borrow().sessions.focus, source);
        // What each lens can take, from every stream's state and not only the shown one's
        if let Some(stills) = self.state.borrow().link.as_ref().and_then(|l| l.stills()) {
            stills.note_state(Some(source.id_str()), &state);
        }
        if focused {
            let mut st = self.state.borrow_mut();
            st.streams.phone_state = Some(state.clone());
            st.bus.emit(Event::PhoneStateUpdated { state: state.clone() });
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
        if state.as_object().is_some_and(|o| !o.is_empty()) && source.has_session() {
            self.on_phone_state(source, &state);
        }
        self.process();
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
        source.state.borrow_mut().camera_toggle = Some(state.get("camera_toggle").and_then(Value::as_bool) == Some(true));
        let _ = camera_error_text;
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
                let tries = session.worker.as_ref().map_or(0, |w| w.failed_tries());
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
    fn call(&mut self, plugins: &[String], hook: &Hook) {
        self.w.state.borrow_mut().call_hook(plugins, hook);
    }
    fn emit(&mut self, event: Event) {
        self.w.state.borrow_mut().bus.emit(event);
    }
    fn can_start(&self) -> bool {
        self.w.services.is_some()
    }
    fn selected_device(&self) -> Option<String> {
        self.w.services.as_ref().and_then(|s| s.link.picked_source()).or_else(|| self.w.state.borrow().selected.clone())
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
            title: title.into(), text: text.into(), actions: vec![start_action(None)],
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
        let st = source.state.borrow();
        self.w.set("fps", st.fps_text.clone());
        self.w.set("net", st.net_text.clone());
        let (msg, kind) = st.status.clone();
        drop(st);
        self.w.set_status(&msg, &kind);
    }
    fn show_stopped(&mut self) {
        self.w.set("fps", "—");
        self.w.set("net", "—");
        self.w.set_status(&tr("Not streaming"), "dim");
    }
    fn show_camera_off(&mut self) {
        self.w.set("fps", "—");
        self.w.set("net", "—");
        self.w.set_status(&tr("Streaming the mic, camera off"), "ok");
    }
    fn refresh_tiles(&mut self) {}
    fn clear_pending_resolution(&mut self, source: &SourceRef) {
        source.state.borrow_mut().pending_resolution = None;
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
        let setup = self.w.state.borrow().plugin_config("setup");
        let dim = |k: &str| setup.as_ref().and_then(|c| c.get(k)).and_then(Value::as_u64).map(|v| v as u32);
        dim("canvas_width").zip(dim("canvas_height"))
    }
    fn slot_ready(&self, slot: u32) -> bool {
        slot == 0 || self.w.services.as_ref().is_some_and(|s| s.outputs.slots().iter().any(|x| x.slot == slot))
    }
    fn set_up_slots(&mut self, source_id: &str) {
        log::info!("Setting up more virtual cameras isn't in this build yet; {source_id} doesn't start");
    }
    fn set_camera_on(&mut self, on: bool) {
        self.w.state.borrow_mut().effects.push(crate::shell::Effect::CameraOn { on, auto: false });
    }
    fn apply_state(&mut self, session_id: u64, state: &Value, source: &SourceRef) {
        let (w, source, state) = (self.w.clone(), source.clone(), state.clone());
        slint::Timer::single_shot(Duration::ZERO, move || w.apply_state(&source, session_id, state));
    }
    fn new_worker(&mut self, source: &SourceRef, url: &str, params: (Option<u32>, Option<u32>, u32), live: bool,
                  canvas: (Option<u32>, Option<u32>)) -> Rc<dyn Worker> {
        let services = self.w.services.clone().expect("streams need services");
        let hardware = self.w.opts.as_ref().is_some_and(|o| o.hardware_decode);
        let id = self.w.streams.next_worker.get() + 1;
        self.w.streams.next_worker.set(id);
        let opener = source.kind.opener().unwrap_or_else(|| Arc::new(|| Err(tr("Couldn't reach the phone."))));
        let setup = WorkerSetup { url: url.into(), opener, width: params.0, height: params.1, fps: params.2, canvas,
                                  slot: source.slot(), source_id: source.id_str().into(), hardware_decode: hardware,
                                  plugin_source: source.kind.as_phone().is_none(), waiting_text: source.kind.waiting_text() };
        let slot: Arc<dyn FrameSlot> = services.media.stream_frames().map(|f| f.new_slot()).unwrap_or_else(local_slot);
        let weak = self.w.ui.as_weak();
        let events: crate::worker::Events = Arc::new(move |e| {
            if matches!(e, WorkerEvent::Preview(..)) {
                return;
            }
            let _ = weak.upgrade_in_event_loop(move |_| with_window(|w| w.worker_event(id, e)));
        });
        let inner = Arc::new(StreamWorker::start(setup, services.media.clone(), services.outputs.clone(), slot, events)
            .expect("a thread for the stream worker"));
        let steps = self.w.state.borrow_mut().pipeline_steps(inner.source_id());
        inner.set_steps(steps);
        let worker = Rc::new(ShellWorker { id, inner, live: Cell::new(live), steps_due: Cell::new(false), makers: self.w.streams.packet_makers.clone() });
        self.w.streams.workers.borrow_mut().push(worker.clone());
        worker
    }
    fn end_worker(&mut self, worker: Rc<dyn Worker>) {
        let Some(w) = self.w.shell_worker(&worker) else { return };
        self.w.streams.workers.borrow_mut().retain(|x| x.id != w.id);
        let inner = w.inner.clone();
        // Off the UI thread, so a stalled read can't freeze the window.
        std::thread::spawn(move || inner.stop());
    }
    fn packet_steps(&self) -> Vec<String> {
        self.w.streams.packet_makers.borrow().owners()
    }
    fn ask_keyframe(&mut self, source: &SourceRef) {
        ask_keyframe(source);
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
