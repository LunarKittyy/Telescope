//! The Several cameras plugin (plugins/composite.py): puts the running streams on one canvas, which goes out on the
//! Telescope camera.
//!
//! The picture is drawn in the frame steps of the stream going to the Telescope camera (slot 0, what call apps watch), from
//! that stream's frame and the newest frame of each other stream (host.frames). While that stream stalls, a clock of its own
//! keeps drawing from its last frame (FrameStore::replace_frame), so the other streams don't freeze with it. The other
//! streams carry on to their own cameras as before. It's a CPU step: a shader step's picture keeps the size it came in at and
//! its textures are fixed when it's handed over, while this one is a new size and reads every stream's newest frame.

use telescope_core::i18n::Fields;
use crate::composite::{clean_layout, clean_layouts, clean_name, layout_dict, same_arrangement, Layout, MAX_LAYOUTS, STALE_S};
use crate::compositor::{Compositor, Frames};
use crate::shared::{no_pictures, PictureReader};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;
use telescope_core::actions::{self, Choice};
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, StreamFrame, Touch};
use telescope_core::host::{FrameStore, Host};
use telescope_core::i18n::{n_, tr};
use telescope_core::output::Outputs;
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "composite";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/composite.slint"));
/// How often the picture is drawn while the stream it goes out on stalls.
pub const FALLBACK_FPS: u64 = 30;
/// What the Telescope camera opens at before any stream has said how big it is (vcam.DEFAULT_SIZE).
pub const DEFAULT_SIZE: (u32, u32) = (1920, 1080);
const STALE_NS: u64 = (STALE_S * 1e9) as u64;
const SWITCH_TIP: &str = n_("Send the streams out as one picture");
const LAYOUT_TIP: &str = n_("A layout you saved in the editor");

/// What the frame steps of every stream, the clock and the plugin share.
pub(crate) struct Hub {
    pub(crate) on: bool,
    /// The source id of the stream going to the Telescope camera, where it goes out.
    pub(crate) target: Option<String>,
    /// (frame, when) the target stream had before the picture.
    pub(crate) main_frame: Option<(Frame, u64)>,
    /// What the step last handed on in the target stream, picture or not.
    pub(crate) handed: Option<Frame>,
    pub(crate) last_picture_at: Option<u64>,
    /// The frames the clock last drew from, so it doesn't draw the same picture again.
    fallback_key: Option<BTreeSet<(String, Option<u64>)>>,
    pub(crate) compositor: Compositor,
    shut: bool,
    /// How often the clock woke to draw, for the tests.
    pub(crate) ticks: u64,
}

pub(crate) struct Shared {
    pub(crate) hub: Mutex<Hub>,
    wake: Condvar,
    store: Mutex<Arc<dyn FrameStore>>,
}

struct NoFrames;

impl FrameStore for NoFrames {
    fn stream_frames(&self) -> Vec<StreamFrame> {
        Vec::new()
    }
    fn stream_frame(&self, _source_id: Option<&str>) -> Option<StreamFrame> {
        None
    }
    fn replace_frame(&self, _source_id: Option<&str>, _frame: Frame, _number: u64) -> bool {
        false
    }
}

impl Shared {
    pub(crate) fn hub(&self) -> MutexGuard<'_, Hub> {
        self.hub.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn store(&self) -> Arc<dyn FrameStore> {
        self.store.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// {source id: (frame, number)} of every stream with a fresh frame; main is the target stream's own.
    fn frames(&self, hub: &Hub, main: Option<&Frame>, now: u64) -> Frames {
        let kept = hub.main_frame.as_ref().filter(|(_, at)| now.saturating_sub(*at) < STALE_NS).map(|(f, _)| f);
        let main = main.or(kept);
        let mut out = Frames::new();
        for sf in self.store().stream_frames() {
            if Some(&sf.source_id) == hub.target.as_ref() {
                if let Some(m) = main {
                    out.insert(sf.source_id, (m.clone(), None));
                } else if !hub.on {
                    // with the picture on, what the main stream sends out is the picture itself
                    let n = sf.frame.number;
                    out.insert(sf.source_id, (sf.frame, Some(n)));
                }
            } else if sf.age(now) < STALE_NS {
                let n = sf.frame.number;
                out.insert(sf.source_id, (sf.frame, Some(n)));
            }
        }
        if let (Some(m), Some(t)) = (main, &hub.target) {
            out.entry(t.clone()).or_insert((m.clone(), None));
        }
        out
    }

    /// The frame step of whichever stream source_id is: the picture in place of the target stream's frame.
    fn step(&self, frame: &mut Frame, source_id: &str) {
        let mut hub = self.hub();
        if !hub.on || hub.target.as_deref() != Some(source_id) {
            return;
        }
        let now = monotonic_ns();
        hub.main_frame = Some((frame.clone(), now));
        let frames = self.frames(&hub, Some(frame), now);
        let out = if hub.compositor.live_tiles(&frames) >= 2 { hub.compositor.render(&frames, frame.format) } else { None };
        let Some(mut out) = out else {
            hub.handed = Some(frame.clone());
            return;
        };
        (out.number, out.time_ns) = (frame.number, frame.time_ns);
        hub.handed = Some(out.clone());
        hub.last_picture_at = Some(now);
        hub.fallback_key = None;
        *frame = out;
    }

    /// Draws the picture from the target stream's last frame when that's more than two frames old, if any other stream moved
    /// since; the target's frame steps draw it otherwise. True if a picture went out.
    pub(crate) fn draw_stalled(&self, now: u64) -> bool {
        let mut hub = self.hub();
        let (Some(target), Some((main, at))) = (hub.target.clone(), hub.main_frame.clone()) else { return false };
        if !hub.on || now.saturating_sub(at) < 2_000_000_000 / FALLBACK_FPS {
            return false;
        }
        let store = self.store();
        // its camera is off, or it hasn't sent a frame yet
        let Some(current) = store.stream_frame(Some(&target)) else { return false };
        // a later step changed what it sent (the wait screen covers it): drawing would undo that
        if !hub.handed.as_ref().is_some_and(|h| Arc::ptr_eq(&h.data, &current.frame.data)) {
            return false;
        }
        let frames = self.frames(&hub, None, now);
        let key: BTreeSet<(String, Option<u64>)> = frames.iter().map(|(sid, (_, n))| (sid.clone(), *n)).collect();
        let others: Frames = frames.iter().filter(|(sid, _)| **sid != target).map(|(k, v)| (k.clone(), v.clone())).collect();
        // nothing else to keep moving
        if hub.fallback_key.as_ref() == Some(&key) || hub.compositor.live_tiles(&others) == 0 {
            return false;
        }
        let Some(out) = hub.compositor.render(&frames, main.format) else { return false };
        // not if the stream sent a frame in the meantime: that one has the newer picture
        if !store.replace_frame(Some(&target), out, current.frame.number) {
            return false;
        }
        hub.fallback_key = Some(key);
        hub.last_picture_at = Some(now);
        true
    }

    /// The clock: rests until there's a stream on the camera to keep moving, then draws while that stream stalls.
    fn run_clock(&self) {
        loop {
            {
                let mut hub = self.hub();
                if hub.shut {
                    return;
                }
                if !hub.on || hub.target.is_none() {
                    let _rested = self.wake.wait(hub).unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                hub.ticks += 1;
            }
            std::thread::sleep(Duration::from_nanos(1_000_000_000 / FALLBACK_FPS));
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.draw_stalled(monotonic_ns()))).is_err() {
                log::error!("Drawing the picture while its stream stalls failed");
            }
        }
    }
}

/// The step every stream gets; only the target stream's does anything.
#[path = "composite_editor.rs"]
mod editor;

struct CompositeStep(Arc<Shared>);

impl CpuStep for CompositeStep {
    fn touch(&self, ctx: &dyn FrameContext) -> Touch {
        let hub = self.0.hub();
        if hub.on && hub.target.as_deref() == Some(ctx.source_id()) { Touch::Changes } else { Touch::Nothing }
    }

    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        self.0.step(frame, ctx.source_id());
    }
}

pub struct CompositePlugin {
    on: bool,
    layout: Layout,
    layouts: Vec<Layout>,
    target: Option<String>,
    /// What the Telescope camera opened at, for Auto canvas size.
    camera_size: Option<(u32, u32)>,
    /// Setup's canvas as last read, for a prepare without a host.
    canvas: (Option<u32>, Option<u32>),
    pub(crate) shared: Arc<Shared>,
    clock: Option<JoinHandle<()>>,
    camera_label: String,
    /// What the card says under the layout, "" while it's hidden.
    pub(crate) note: String,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
    /// Where the switch was when a held key began.
    held: Option<bool>,
    /// The background pictures' reader, for the editor's own compositor.
    reader: PictureReader,
    /// The editor's state while its window is open.
    editor: Option<editor::Editor>,
}

impl Default for CompositePlugin {
    fn default() -> Self {
        CompositePlugin::new()
    }
}

impl CompositePlugin {
    pub fn new() -> CompositePlugin {
        CompositePlugin::with(no_pictures(), None)
    }

    /// With a reader for the background picture, and the virtual cameras to name the Telescope camera by.
    pub fn with(reader: PictureReader, outputs: Option<Arc<dyn Outputs>>) -> CompositePlugin {
        let camera_label = outputs.map_or_else(|| "Telescope".to_string(), |o| o.slot_label(0));
        let hub = Hub {
            on: false, target: None, main_frame: None, handed: None, last_picture_at: None, fallback_key: None,
            compositor: Compositor::new(reader.clone()), shut: false, ticks: 0,
        };
        let shared = Arc::new(Shared { hub: Mutex::new(hub), wake: Condvar::new(), store: Mutex::new(Arc::new(NoFrames)) });
        let mut plugin = CompositePlugin {
            on: false, layout: Layout::default(), layouts: Vec::new(), target: None, camera_size: None, canvas: (None, None), shared,
            clock: None, camera_label, note: String::new(), pending: false, held: None, reader, editor: None,
        };
        plugin.prepare();
        plugin
    }

    // ── The picture ───────────────────────────────────────────────────

    /// The picture's size: the canvas size Setup asks for, else what the main camera opened at, else 1920x1080.
    pub fn size(&self) -> (u32, u32) {
        match self.canvas {
            (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
            _ => self.camera_size.unwrap_or(DEFAULT_SIZE),
        }
    }

    fn prepare(&mut self) {
        let size = self.size();
        let on = self.on;
        let mut hub = self.shared.hub();
        hub.compositor.prepare(&self.layout, size);
        hub.on = on;
    }

    fn follow_camera(&mut self, host: &mut dyn Host) {
        self.canvas = host.canvas_size();
        let on_camera = host.stream_on_camera(0);
        let target = on_camera.as_ref().map(|c| c.0.clone());
        let size = on_camera.as_ref().and_then(|c| match (c.1, c.2) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
            _ => None,
        });
        if target != self.target {
            self.target = target.clone();
            let mut hub = self.shared.hub();
            hub.target = target.clone();
            // another stream's frame isn't this one's
            (hub.main_frame, hub.handed, hub.fallback_key) = (None, None, None);
            drop(hub);
            if target.is_some() {
                self.wake_clock();
            }
        }
        if size.is_some() && size != self.camera_size {
            self.camera_size = size;
        }
        self.prepare();
        self.show(host);
    }

    fn wake_clock(&mut self) {
        if !self.on || self.shared.hub().shut {
            return;
        }
        if self.clock.is_none() {
            let shared = self.shared.clone();
            match std::thread::Builder::new().name("composite-clock".into()).spawn(move || shared.run_clock()) {
                Ok(handle) => self.clock = Some(handle),
                Err(e) => log::error!("The clock that draws the picture while its stream stalls didn't start: {e}"),
            }
        }
        self.shared.wake.notify_all();
    }

    // ── Layouts ───────────────────────────────────────────────────────

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The picture as the editor has it now; the name says which saved layout it started from.
    pub fn set_layout(&mut self, host: &mut dyn Host, layout: Layout) {
        self.layout = layout;
        self.canvas = host.canvas_size();
        self.prepare();
        self.show(host);
        host.schedule_save();
    }

    pub fn layouts(&self) -> &[Layout] {
        &self.layouts
    }

    pub fn names(&self) -> Vec<String> {
        self.layouts.iter().map(|l| l.name.clone()).collect()
    }

    pub fn pick(&mut self, host: &mut dyn Host, name: &str) -> bool {
        let Some(layout) = self.layouts.iter().find(|l| l.name == name).cloned() else { return false };
        self.set_layout(host, layout);
        true
    }

    /// Save the picture as it is under name, replacing one of the same name; the name it got, "" for none.
    pub fn save_layout(&mut self, host: &mut dyn Host, name: &str) -> String {
        let name = clean_name(name);
        if name.is_empty() {
            return name;
        }
        let layout = Layout { name: name.clone(), ..self.layout.clone() };
        if let Some(at) = self.layouts.iter().position(|l| l.name == name) {
            self.layouts[at] = layout.clone();
        } else if self.layouts.len() < MAX_LAYOUTS {
            self.layouts.push(layout.clone());
        } else {
            return String::new();
        }
        self.set_layout(host, layout);
        host.refresh_actions();
        name
    }

    pub fn rename_layout(&mut self, host: &mut dyn Host, old: &str, new: &str) {
        let new = clean_name(new);
        if new.is_empty() || new == old {
            return;
        }
        self.layouts.retain(|l| l.name != new);
        for l in &mut self.layouts {
            if l.name == old {
                l.name = new.clone();
            }
        }
        if self.layout.name == old {
            self.layout.name = new;
        }
        self.show(host);
        host.refresh_actions();
        host.schedule_save();
    }

    pub fn delete_layout(&mut self, host: &mut dyn Host, name: &str) {
        self.layouts.retain(|l| l.name != name);
        self.show(host);
        host.refresh_actions();
        host.schedule_save();
    }

    /// The name of the saved layout the picture is now, or "" when it's been changed since.
    pub fn saved_match(&self) -> String {
        self.layouts.iter().find(|l| l.name == self.layout.name && same_arrangement(l, &self.layout)).map_or_else(String::new, |l| l.name.clone())
    }

    // ── Card ──────────────────────────────────────────────────────────

    fn fixed_props() -> Value {
        json!({"switch-name": tr("One picture"), "switch-help": tr(SWITCH_TIP), "layout-tip": tr(LAYOUT_TIP), "edit-shown": true})
    }

    fn card_props(&self) -> Value {
        let names = self.names();
        let current = names.iter().position(|n| *n == self.saved_match()).map_or(-1, |i| i as i64);
        let mut props = Self::fixed_props();
        if let Value::Object(o) = &mut props {
            o.extend([
                ("on".to_string(), json!(self.on)),
                ("layout-items".to_string(), json!(names)),
                ("layout-current".to_string(), json!(current)),
                ("layout-enabled".to_string(), json!(!names.is_empty())),
                ("status".to_string(), json!(self.note)),
            ]);
        }
        props
    }

    fn show(&mut self, host: &mut dyn Host) {
        let live = self.layout.tiles.iter().filter(|t| host.is_streaming_from(Some(&t.source_id))).count();
        let camera = self.camera_label.as_str();
        self.note = if !self.on {
            String::new()
        } else if self.layout.tiles.is_empty() {
            tr("Nothing's in the picture yet.")
        } else if live < 2 {
            tr("It takes over once two of its streams are running.")
        } else if self.target.is_none() {
            tr("It goes out on {camera}, and no stream goes there now.").field("camera", camera)
        } else if self.shared.store().stream_frame(self.target.as_deref()).is_none() {
            tr("It goes out on {camera} once the stream there sends a picture.").field("camera", camera)
        } else {
            tr("Goes out on {camera}.").field("camera", camera)
        };
        host.set_ui_props("card", &self.card_props());
        self.show_editor(host);
    }

    /// draw_stalled at now (monotonic nanoseconds): what the clock does while the target stream stalls.
    pub fn draw_stalled(&self, now: u64) -> bool {
        self.shared.draw_stalled(now)
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            if self.canvas != host.canvas_size() {
                self.canvas = host.canvas_size();
                self.prepare();
            }
            self.wake_clock();
            self.show(host);
            host.refresh_actions();
        }
    }

    fn set_on(&mut self, host: &mut dyn Host, on: bool) {
        self.on = on;
        self.shared.hub().on = on;
        self.wake_clock();
        self.show(host);
        host.schedule_save();
    }

    /// The picture's look and whether it's on, from settings or a preset (the saved layouts aside).
    fn apply(&mut self, cfg: &Value) {
        self.layout = cfg.get("layout").and_then(clean_layout).unwrap_or_default();
        self.on = cfg.get("on") == Some(&Value::Bool(true));
        self.prepare();
        self.wake_clock();
    }
}

impl Plugin for CompositePlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Several cameras in one picture").into(),
            about: n_("Puts the streams you run side by side or one inside another, out on one virtual camera.").into(),
            when_off: n_("Without it there's no One picture card and each stream only goes to its own camera.").into(),
            category: "streaming".into(),
            icon: "layout".into(),
            frame_stage: FrameStage::Combine,
            optional: true,
            // a preset can be a scene: "face big, desk small" for talking, side by side for a demo
            in_presets: true,
            // it's about the Telescope camera's stream, whichever one the panels show
            follows_focus: false,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        *self.shared.store.lock().unwrap_or_else(|e| e.into_inner()) = host.frames();
        self.canvas = host.canvas_size();
        self.prepare();
        self.show(host);
    }

    fn shutdown(&mut self, _host: &mut dyn Host) {
        self.shared.hub().shut = true;
        self.shared.wake.notify_all();
        // so no picture goes out after quitting
        if let Some(clock) = self.clock.take() {
            let _ = clock.join();
        }
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        match event {
            Event::StreamsChanged { .. } => self.follow_camera(host),
            Event::CameraOpened { slot: 0, .. } => self.follow_camera(host),
            Event::TimerFired { owner, token } if owner == NAME && token == Self::EDITOR_TIMER => self.editor_timer(host),
            Event::RequestDone { request, ok, data, .. } => {
                self.editor_picked(host, *request, *ok, data);
            }
            _ => {}
        }
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, _stream: &StreamInfo) {
        self.catch_up(host);
        self.follow_camera(host);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
        self.follow_camera(host);
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "CompositeCard".into(), props: self.card_props() }, region: "left".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui == editor::WINDOW {
            return self.editor_ui(host, event);
        }
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        match event.callback.as_str() {
            "edit-clicked" => self.open_editor(host),
            "switch-toggled-by-hand" => {
                let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.set_on(host, on);
            }
            "layout-picked" => {
                let i = event.args.first().and_then(Value::as_f64).unwrap_or(-1.0);
                if let Some(name) = (i >= 0.0).then(|| self.names().get(i as usize).cloned()).flatten() {
                    self.pick(host, &name);
                }
            }
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("One picture");
        let options: Vec<(String, String)> = self.names().into_iter().map(|n| (n.clone(), n)).collect();
        let options: Vec<(&str, String)> = options.iter().map(|(v, s)| (v.as_str(), s.clone())).collect();
        vec![
            actions::toggle_action("composite.on", &tr("One picture"), &group, "", &[]),
            actions::choice_action("composite.layout", &tr("Picture layout"), &group, "", &options),
        ]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        match id {
            "composite.on" => {
                let want = actions::toggle_wanted(params, self.on, &mut self.held);
                if want != self.on {
                    self.set_on(host, want);
                }
                Some(actions::toggle_text(&tr("One picture"), want))
            }
            "composite.layout" => {
                let names = self.names();
                let options: Vec<Choice> = names.iter().map(|n| Choice { value: n, shown: n, enabled: true }).collect();
                let matched = self.saved_match();
                let current = names.iter().position(|n| *n == matched);
                let (target, pick) = actions::choice_target(params, &options, current, true)?;
                let name = names[target].clone();
                if pick {
                    self.pick(host, &name);
                }
                Some(tr("{label}: {value}").field("label", tr("Picture layout")).field("value", &name))
            }
            _ => None,
        }
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        if id != "composite.on" {
            return;
        }
        if let Some(before) = actions::hold_released(params, &mut self.held) {
            if before != self.on {
                self.set_on(host, before);
            }
        }
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        // the settings are the app's, not the stream's, so it stays live behind the panels too
        Some(FrameStep::Cpu(Box::new(CompositeStep(self.shared.clone()))))
    }

    fn get_config(&self) -> Value {
        json!({"on": self.on, "layout": layout_dict(&self.layout), "layouts": self.layouts.iter().map(layout_dict).collect::<Vec<_>>()})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.layouts = clean_layouts(cfg.get("layouts").unwrap_or(&Value::Null));
        self.apply(cfg);
        self.pending = true;
        Ok(())
    }

    /// A preset brings back whether the picture is on and how it looks, and leaves the saved layouts alone.
    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.catch_up(host);
        self.canvas = host.canvas_size();
        self.apply(cfg);
        self.show(host);
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        if !self.on {
            return vec![("One picture".into(), "off".into())];
        }
        let drawing = self.shared.hub().last_picture_at.is_some_and(|at| monotonic_ns().saturating_sub(at) < 1_000_000_000);
        vec![("One picture".into(), format!("on, {} streams, {}", self.layout.tiles.len(), if drawing { "drawing" } else { "waiting" }))]
    }
}

#[cfg(test)]
#[path = "composite_plugin_tests.rs"]
mod tests;
