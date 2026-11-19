//! Spout send: shares a stream's picture with other programs on this computer, the way Telescope's virtual camera does for call apps. On Windows that is a Spout sender
//! (a shared graphics texture programs like OBS list), on Linux a PipeWire video node. The picture is taken in the last frame step, so it is exactly what the virtual camera
//! gets: a stream's own picture, or with Several cameras on, the combined one the stream going to the Telescope camera shows.
//!
//! The step only copies a handle to the picture and leaves it for a thread of the stream's own, so a slow program reading the sender never holds the stream up. The sender is
//! made when the first picture comes and goes when they stop. Pictures leave opaque unless a stream is switched to Transparent: then the steps that cut a person out are
//! asked to (Event::CutOutWantedBy) and the alpha they leave on the frame goes out as the picture's alpha.

#[cfg(target_os = "linux")]
pub mod pipewire;
pub mod pack;
#[cfg(windows)]
pub mod windows;
pub mod worker;

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, Touch};
use telescope_core::host::Host;
use telescope_core::i18n::{n_, tr, Fields};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::ui::{CardSpec, SlintSource, UiEvent, UiSpec};
use worker::{Problem, State, Worker};

pub const NAME: &str = "spout_send";
const UI: &str = include_str!("../../ui/spout_send.slint");
/// Re-reads how the senders are doing for the card.
const T_STATUS: &str = "status";
const STATUS_MS: u64 = 1000;
/// What every sender's name starts with.
const PREFIX: &str = "Telescope";
/// Spout keeps a sender's name in 256 bytes with its end.
const NAME_BYTES: usize = 255;

/// A way to share pictures: one sender at a size.
pub trait Backend: Send + Sync {
    /// A sender called `name` for pictures of this size. Err says why not.
    fn open(&self, name: &str, width: u32, height: u32) -> Result<Box<dyn Sender>, String>;
    /// Whether a sender stays up while no pictures come (a program that bound to it would lose it otherwise), instead of going a few seconds after they stop.
    fn keeps_idle_senders(&self) -> bool {
        false
    }
}

/// One open sender, gone when dropped.
pub trait Sender: Send {
    /// Whether to pack and send the next picture.
    fn wants_frame(&self) -> bool {
        true
    }
    /// Changes the size the sender takes where it is, so whoever reads it stays connected. Ok(false) when it can't and has to be made again.
    fn resize(&mut self, _width: u32, _height: u32) -> Result<bool, String> {
        Ok(false)
    }
    /// The next picture, B, G, R, A per pixel, top row first, at the size the sender was opened with.
    fn send(&mut self, bgra: &[u8]) -> Result<(), String>;
    /// send for a picture the caller won't keep: the sender may take the buffer and leave another (or an empty one) in its place, which the caller fills with the next picture.
    fn send_buffer(&mut self, bgra: &mut Vec<u8>) -> Result<(), String> {
        self.send(bgra)
    }
}

/// What this system shares pictures through: Spout on Windows, PipeWire on Linux.
pub fn system_backend() -> Option<Arc<dyn Backend>> {
    #[cfg(windows)]
    return windows::backend();
    #[cfg(target_os = "linux")]
    return pipewire::backend();
    #[cfg(not(any(windows, target_os = "linux")))]
    None
}

/// The sender name for a stream called `stream`: "Telescope - Pixel 7", cut at a character boundary to what fits, and with " (2)" and up when another stream already has it.
pub fn sender_name(stream: &str, taken: &[&str]) -> String {
    let clean: String = stream.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    let base = if clean.is_empty() { PREFIX.to_string() } else { format!("{PREFIX} - {clean}") };
    let fit = |text: &str, room: usize| -> String {
        let mut end = text.len().min(room);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_string()
    };
    let mut n = 1;
    loop {
        let name = if n == 1 { fit(&base, NAME_BYTES) } else { let tail = format!(" ({n})"); format!("{}{tail}", fit(&base, NAME_BYTES - tail.len())) };
        if !taken.contains(&name.as_str()) {
            return name;
        }
        n += 1;
    }
}

struct Entry {
    /// Tells this entry from an earlier one of the same stream, so a thread that outlives its entry can't write into the next one's state.
    generation: u64,
    /// What the stream is called to the person, kept so the name shows while the stream is stopped.
    stream: String,
    /// The sender's name.
    name: String,
    /// Whether the cut-out goes out as the picture's alpha, so what is behind the person is see-through.
    transparent: bool,
    worker: Option<Worker>,
}

/// What the plugin and the frame steps of every stream share.
pub struct Shared {
    backend: Option<Arc<dyn Backend>>,
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
    generations: std::sync::atomic::AtomicU64,
    states: Arc<Mutex<BTreeMap<String, State>>>,
    idle: Duration,
    retry: Duration,
}

impl Shared {
    fn new(backend: Option<Arc<dyn Backend>>) -> Arc<Shared> {
        Arc::new(Shared { backend, entries: Arc::default(), generations: std::sync::atomic::AtomicU64::new(1), states: Arc::default(), idle: worker::IDLE, retry: worker::RETRY })
    }

    fn wanted(&self, source: &str) -> bool {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).contains_key(source)
    }

    fn any(&self) -> bool {
        !self.entries.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }

    /// Hands a stream's newest picture to its sender thread, starting one if the last ended.
    fn offer(&self, source: &str, frame: &Frame) {
        let Some(backend) = self.backend.clone() else { return };
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = entries.get_mut(source) else { return };
        // The step leaves the cut-out on the picture it hands over only for a sender that asked for it
        let mut frame = frame.clone();
        if !entry.transparent {
            frame.clear_alpha();
        }
        if entry.worker.as_ref().is_some_and(|w| !w.alive()) {
            entry.worker = None;
        }
        if entry.worker.is_none() {
            let (states, live, id, generation) = (self.states.clone(), self.entries.clone(), source.to_string(), entry.generation);
            let tell: Arc<dyn Fn(State) + Send + Sync> = Arc::new(move |state| {
                // Only while its entry is the one that is there now
                let entries = live.lock().unwrap_or_else(|e| e.into_inner());
                if entries.get(&id).is_some_and(|e| e.generation == generation) {
                    states.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), state);
                }
            });
            let keep = backend.keeps_idle_senders();
            entry.worker = Worker::spawn(entry.name.clone(), backend, tell, self.idle, self.retry, keep);
        }
        if let Some(worker) = &entry.worker {
            worker.put(frame);
        }
    }

    /// Sets whether a shared stream's cut-out goes out as alpha. Whether that changed anything.
    fn set_transparent(&self, source: &str, on: bool) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match entries.get_mut(source) {
            Some(entry) if entry.transparent != on => {
                entry.transparent = on;
                true
            }
            _ => false,
        }
    }

    /// The streams whose senders want their cut-out.
    fn transparent(&self) -> std::collections::BTreeSet<String> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, e)| e.transparent).map(|(id, _)| id.clone()).collect()
    }

    fn state(&self, source: &str) -> State {
        self.states.lock().unwrap_or_else(|e| e.into_inner()).get(source).cloned().unwrap_or(State::Idle)
    }

    /// Starts or stops sharing a stream. Returns whether anything changed.
    fn set(&self, source: &str, stream: &str, on: bool) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if !on {
            let gone = entries.remove(source);
            drop(entries);
            self.states.lock().unwrap_or_else(|e| e.into_inner()).remove(source);
            // The thread is stopped and joined outside the lock the frame steps take
            drop(gone);
            return true;
        }
        if entries.contains_key(source) {
            return false;
        }
        let taken: Vec<&str> = entries.values().map(|e| e.name.as_str()).collect();
        let name = sender_name(stream, &taken);
        let generation = self.generations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        entries.insert(source.to_string(), Entry { generation, stream: stream.to_string(), name, transparent: false, worker: None });
        true
    }

    fn stop_all(&self) {
        let all = std::mem::take(&mut *self.entries.lock().unwrap_or_else(|e| e.into_inner()));
        self.states.lock().unwrap_or_else(|e| e.into_inner()).clear();
        drop(all);
    }
}

/// The last frame step: reads each stream's picture and leaves it as it is.
struct Tap(Arc<Shared>);

impl CpuStep for Tap {
    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        self.0.offer(ctx.source_id(), frame);
    }

    fn touch(&self, ctx: &dyn FrameContext) -> Touch {
        if self.0.wanted(ctx.source_id()) { Touch::Reads } else { Touch::Nothing }
    }
}

pub struct SpoutSendPlugin {
    shared: Arc<Shared>,
    /// The stream the panels show, which the card's switch is for.
    shown: String,
    stopped: bool,
    config_changed: bool,
    /// The streams the producers of the cut-out have been told to cut out for (Event::CutOutWantedBy), so a change is told once.
    told: std::collections::BTreeSet<String>,
}

impl SpoutSendPlugin {
    pub fn new(backend: Option<Arc<dyn Backend>>) -> SpoutSendPlugin {
        SpoutSendPlugin { shared: Shared::new(backend), shown: String::new(), stopped: false, config_changed: false, told: Default::default() }
    }

    pub fn system() -> SpoutSendPlugin {
        SpoutSendPlugin::new(system_backend())
    }

    /// What a stream is called, when the host knows: the main stream's name, a background stream's, else None.
    fn stream_name(host: &dyn Host, source: &str) -> Option<String> {
        if let Some((id, name)) = host.main_stream() {
            if id == source {
                return Some(name);
            }
        }
        host.background_streams().into_iter().find(|s| s.source_id == source).map(|s| s.name)
    }

    fn props(&self) -> Value {
        let entries = self.shared.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get(&self.shown);
        let (kind, text) = match (&self.shared.backend, entry) {
            (None, _) => ("status_warn", tr("Sharing pictures isn't available on this system.")),
            (Some(_), None) => ("status_dim", String::new()),
            (Some(_), Some(entry)) => match self.shared.state(&self.shown) {
                State::Sending(w, h) => ("status_ok", tr("Sharing {size} as {name}").field("size", format!("{w} × {h}")).field("name", &entry.name)),
                State::Failed(problem) => ("status_warn", tr(match problem {
                    Problem::Open => n_("The sender couldn't be started. It tries again with the next picture."),
                    Problem::Send => n_("The sender stopped. It tries again with the next picture."),
                    Problem::Format => n_("This picture can't be shared."),
                })),
                State::Idle => ("status_dim", tr("Shares as {name} while this stream runs").field("name", &entry.name)),
            },
        };
        json!({
            "visible": !self.shown.is_empty(),
            "checked": entry.is_some(),
            "enabled": self.shared.backend.is_some(),
            "transparent": entry.is_some_and(|e| e.transparent),
            "transparent-shown": entry.is_some(),
            "status-text": text, "status-kind": kind,
        })
    }

    /// Tells the steps that cut a person out which streams want it, now and again, since a step that starts later hasn't heard.
    fn tell_cut_outs(&mut self, host: &mut dyn Host, again: bool) {
        let now = self.shared.transparent();
        for id in now.difference(&self.told).cloned().collect::<Vec<_>>() {
            host.emit(Event::CutOutWantedBy { owner: NAME.into(), source_id: id.clone(), on: true });
            host.emit(Event::CutOutWanted { source_id: id, on: true });
        }
        for id in self.told.difference(&now).cloned().collect::<Vec<_>>() {
            host.emit(Event::CutOutWantedBy { owner: NAME.into(), source_id: id.clone(), on: false });
            host.emit(Event::CutOutWanted { source_id: id, on: false });
        }
        if again {
            for id in now.intersection(&self.told).cloned().collect::<Vec<_>>() {
                host.emit(Event::CutOutWantedBy { owner: NAME.into(), source_id: id.clone(), on: true });
                host.emit(Event::CutOutWanted { source_id: id, on: true });
            }
        }
        self.told = now;
    }

    fn render(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.props());
    }

    fn sync_sharing(&mut self, host: &mut dyn Host) {
        let on = !self.stopped && self.shared.any();
        host.want_frames(NAME, on);
        if on {
            host.schedule(STATUS_MS, T_STATUS);
        } else {
            host.cancel_schedule(T_STATUS);
        }
        self.config_changed = false;
        host.refresh_frame_step();
    }

    fn follow(&mut self, host: &mut dyn Host, source: &str) {
        if self.shown != source {
            self.shown = source.to_string();
            self.render(host);
        }
    }
}

impl Plugin for SpoutSendPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Spout send").into(),
            about: n_("Share a stream's picture with other programs on this computer, such as OBS.").into(),
            when_off: n_("Without Spout send the Spout send card is gone and no picture is shared.").into(),
            optional: true,
            lab: true,
            category: "streaming".into(),
            icon: "arrow_out".into(),
            frame_stage: FrameStage::Final,
            ..PluginInfo::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        // Switched on mid-session, the stream in focus was selected before this plugin heard of it
        if let Some(source) = host.focused_source_id() {
            self.shown = source;
        }
        self.sync_sharing(host);
        self.render(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.stopped = true;
        host.want_frames(NAME, false);
        host.cancel_schedule(T_STATUS);
        self.shared.stop_all();
        // The steps that cut a person out stop doing it, and Background goes back from the Blur it was given for this
        for id in std::mem::take(&mut self.told) {
            host.emit(Event::CutOutWantedBy { owner: NAME.into(), source_id: id.clone(), on: false });
            host.emit(Event::CutOutWanted { source_id: id, on: false });
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "SpoutSendCard".into(), props: self.props() }, region: "right".into() })
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        self.shared.any().then(|| FrameStep::Cpu(Box::new(Tap(self.shared.clone()))))
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        if self.config_changed {
            self.on_profile_applied(host);
        }
        match event {
            Event::TimerFired { owner, token } if owner == NAME && token == T_STATUS => {
                if !self.stopped && self.shared.any() {
                    self.tell_cut_outs(host, false);
                    self.render(host);
                    host.schedule(STATUS_MS, T_STATUS);
                }
            }
            Event::SourceSelected { source_id } => self.follow(host, &source_id.clone()),
            Event::SourceShown { .. } => self.tell_cut_outs(host, true),
            _ => {}
        }
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.sync_sharing(host);
        self.tell_cut_outs(host, true);
        self.render(host);
    }

    fn on_profile_applied(&mut self, host: &mut dyn Host) {
        self.sync_sharing(host);
        self.tell_cut_outs(host, false);
        self.render(host);
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.set_config(cfg)?;
        self.on_profile_applied(host);
        Ok(())
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        if self.config_changed {
            self.on_profile_applied(host);
        }
        self.follow(host, &stream.source_id);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if self.config_changed {
            self.on_profile_applied(host);
        }
        if self.shown.is_empty() {
            return;
        }
        let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
        match event.callback.as_str() {
            "toggled" => {
                let stream = Self::stream_name(host, &self.shown).unwrap_or_default();
                if self.shared.set(&self.shown, &stream, on) {
                    self.sync_sharing(host);
                    host.schedule_save();
                    self.tell_cut_outs(host, false);
                }
            }
            "transparent-toggled" => {
                if self.shared.set_transparent(&self.shown, on) {
                    host.schedule_save();
                    self.tell_cut_outs(host, false);
                }
            }
            _ => return,
        }
        self.render(host);
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let entries = self.shared.entries.lock().unwrap_or_else(|e| e.into_inner());
        let text = if self.shared.backend.is_none() { "no way to share here".to_string() } else { format!("{} shared", entries.len()) };
        vec![("Spout send".to_string(), text)]
    }

    fn get_config(&self) -> Value {
        let entries = self.shared.entries.lock().unwrap_or_else(|e| e.into_inner());
        json!({"streams": entries.iter().map(|(id, e)| (id.clone(), if e.transparent { json!({"name": e.stream, "transparent": true}) } else { json!({"name": e.stream}) })).collect::<Map<String, Value>>()})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.config_changed = true;
        self.shared.stop_all();
        if let Some(streams) = cfg.get("streams").and_then(Value::as_object) {
            for (id, entry) in streams {
                self.shared.set(id, entry.get("name").and_then(Value::as_str).unwrap_or(""), true);
                self.shared.set_transparent(id, entry.get("transparent").and_then(Value::as_bool).unwrap_or(false));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
