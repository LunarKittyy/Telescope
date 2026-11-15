//! The 3D plugin: two phones streaming at once become one stereo picture on the Telescope camera.
//!
//! The picture is made in the frame step of the stream going to the Telescope camera (slot 0), like One picture's. Each of that step's frames
//! is paired with the other eye's frame taken at about the same moment (stereo::pair_frames), the pair is packed side by side or as red/cyan
//! and goes out in place of the stream's frame. The target stream's own frames are kept here, since what the host keeps of it afterwards is
//! the picture this step made. While it's switched off, or fewer than two streams run, the streams go out as they are.

use crate::stereo::{self, Eye, Look, SyncStatus, DEFAULT_FRAME_NS, HISTORY_FRAMES, SHIFT_LIMIT};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use telescope_core::actions;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, StreamFrame, Touch};
use telescope_core::host::{FrameStore, Host};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "stereo";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/stereo.slint"));
/// What the Telescope camera opens at before any stream has said how big it is.
pub const DEFAULT_SIZE: (u32, u32) = (1920, 1080);
/// How old a stream's newest picture can be and still be an eye.
const STALE_NS: u64 = 3_000_000_000;
/// How long a sync reading stays on the card once pictures stop coming.
const SEEN_NS: u64 = 2_000_000_000;
/// The timer that keeps the sync badge up to date while it's on.
const STATUS_TIMER: &str = "status";
const STATUS_MS: u64 = 1000;

const SWITCH_TIP: &str = n_("Turn two running streams into one 3D picture.");
const LOOK_TIP: &str = n_("Side by side suits a headset or a 3D screen and red and cyan suits glasses.");
const DEPTH_TIP: &str = n_("Slide the two pictures apart or together to change how far forward the 3D reaches.");
const HEIGHT_TIP: &str = n_("Lift or lower the right picture when the phones don't sit level.");
const SWAP_TIP: &str = n_("Swap which phone is the left eye and which is the right if the 3D looks inside out.");
const LINK_TIP: &str = n_("Copy the shown phone's exposure, white balance and focus to both phones so the two pictures match.");

/// What the frame step of every stream and the plugin share.
pub(crate) struct Hub {
    pub(crate) on: bool,
    /// The source id of the stream going to the Telescope camera, where the picture goes out.
    pub(crate) target: Option<String>,
    pub(crate) look: Look,
    /// Percent of the width, positive moves the eyes together.
    pub(crate) depth: f64,
    /// Percent of the height, positive lifts the right eye.
    pub(crate) height: f64,
    pub(crate) swap: bool,
    /// The picture's size.
    size: (u32, u32),
    /// The target stream's last few frames as they came in, before the picture replaced them.
    ring: VecDeque<Frame>,
    /// How far apart the target stream's frames come, nanoseconds.
    frame_ns: u64,
    last_in: Option<u64>,
    /// How the last picture's pair lined up, and when it was made.
    pub(crate) seen: Option<(SyncStatus, u64)>,
    /// How many pictures have gone out, for the tests.
    pub(crate) pictures: u64,
}

pub(crate) struct Shared {
    pub(crate) hub: Mutex<Hub>,
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

/// Which two pictures go together, and how well they line up.
pub(crate) struct Chosen {
    pub(crate) pair: stereo::Pair<Frame>,
    pub(crate) sync: SyncStatus,
}

/// The left and the right eye from the running streams: the first two that started (swapped on request), each with its newest frame; the target
/// stream's is `frame`, the one the step is handling, since its published frame is the picture itself. `ring` is the target's recent frames.
pub(crate) fn choose(store: &dyn FrameStore, frame: &Frame, target: &str, ring: &[Frame], swap: bool, frame_ns: u64, now: u64) -> Option<Chosen> {
    let mut eyes: Vec<(String, Frame)> = Vec::new();
    for sf in store.stream_frames() {
        if sf.source_id == target {
            eyes.push((sf.source_id, frame.clone()));
        } else if sf.age(now) < STALE_NS {
            eyes.push((sf.source_id, sf.frame));
        }
    }
    // it has no published frame yet, but this is one
    if !eyes.iter().any(|(id, _)| id == target) {
        eyes.push((target.to_string(), frame.clone()));
    }
    eyes.truncate(2);
    if eyes.len() < 2 {
        return None;
    }
    if swap {
        eyes.swap(0, 1);
    }
    let near = |eye: Eye, at: u64| -> Option<Frame> {
        let id = &eyes[if eye == Eye::Left { 0 } else { 1 }].0;
        if id == target {
            let captures: Vec<Option<u64>> = ring.iter().map(|f| f.capture_ns).collect();
            stereo::nearest_capture(&captures, at).map(|i| ring[i].clone())
        } else {
            store.stream_frame_near(Some(id), at).map(|s| s.frame)
        }
    };
    let pair = stereo::pair_frames(&eyes[0].1, &eyes[1].1, |f| f.capture_ns, near);
    let gap = pair.left.capture_ns.zip(pair.right.capture_ns).map(|(a, b)| a.abs_diff(b));
    let error = |id: &str| store.clock_sync(Some(id)).map(|c| c.error_ns);
    let sync = stereo::sync_status([error(&eyes[0].0), error(&eyes[1].0)], gap, frame_ns);
    Some(Chosen { pair, sync })
}

impl Hub {
    /// The target stream's next frame: kept for pairing, and its spacing noted for the sync.
    fn take_in(&mut self, frame: &Frame, now: u64) {
        if let Some(last) = self.last_in {
            self.frame_ns = stereo::smooth_frame_ns(self.frame_ns, now.saturating_sub(last));
        }
        self.last_in = Some(now);
        self.ring.push_back(frame.clone());
        while self.ring.len() > HISTORY_FRAMES {
            self.ring.pop_front();
        }
    }

    fn forget(&mut self) {
        (self.ring, self.last_in, self.seen) = (VecDeque::new(), None, None);
    }
}

impl Shared {
    pub(crate) fn hub(&self) -> MutexGuard<'_, Hub> {
        self.hub.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn store(&self) -> Arc<dyn FrameStore> {
        self.store.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The frame step of whichever stream source_id is: the 3D picture in place of the target stream's frame.
    fn step(&self, frame: &mut Frame, source_id: &str) {
        let (look, depth, height, swap, size, ring, frame_ns) = {
            let mut hub = self.hub();
            if !hub.on || hub.target.as_deref() != Some(source_id) {
                return;
            }
            hub.take_in(frame, monotonic_ns());
            (hub.look, hub.depth, hub.height, hub.swap, hub.size, hub.ring.iter().cloned().collect::<Vec<_>>(), hub.frame_ns)
        };
        // the pictures are worked on without holding the hub, so the card never waits for them
        let now = monotonic_ns();
        let chosen = choose(&*self.store(), frame, source_id, &ring, swap, frame_ns, now);
        let out = chosen.as_ref().and_then(|c| stereo::render(&c.pair.left, &c.pair.right, look, size, depth, height, frame));
        let mut hub = self.hub();
        let (Some(chosen), Some(mut out)) = (chosen, out) else {
            hub.seen = None;
            return;
        };
        (out.number, out.time_ns, out.capture_ns) = (frame.number, frame.time_ns, frame.capture_ns);
        hub.seen = Some((chosen.sync, now));
        hub.pictures += 1;
        *frame = out;
    }
}

/// The step every stream gets; only the target stream's does anything.
struct StereoStep(Arc<Shared>);

impl CpuStep for StereoStep {
    fn touch(&self, ctx: &dyn FrameContext) -> Touch {
        let hub = self.0.hub();
        if hub.on && hub.target.as_deref() == Some(ctx.source_id()) { Touch::Changes } else { Touch::Nothing }
    }

    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        self.0.step(frame, ctx.source_id());
    }
}

pub struct StereoPlugin {
    on: bool,
    look: Look,
    depth: f64,
    height: f64,
    swap: bool,
    target: Option<String>,
    /// What the Telescope camera opened at, for the size of the picture.
    camera_size: Option<(u32, u32)>,
    /// Setup's canvas as last read, for a prepare without a host.
    canvas: (Option<u32>, Option<u32>),
    pub(crate) shared: Arc<Shared>,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
    /// Where the switch was when a held key began.
    held: Option<bool>,
}

impl Default for StereoPlugin {
    fn default() -> Self {
        StereoPlugin::new()
    }
}

/// A slider's value as the card reads it out: 0.0%, +1.5%, -2.0%.
fn percent_text(v: f64) -> String {
    if v.abs() < 0.05 { "0.0%".to_string() } else { format!("{v:+.1}%") }
}

/// A slider value held to the slider's range and its steps of a tenth.
fn clean_shift(v: f64) -> f64 {
    if v.is_finite() { (v.clamp(-SHIFT_LIMIT, SHIFT_LIMIT) * 10.0).round() / 10.0 } else { 0.0 }
}

impl StereoPlugin {
    pub fn new() -> StereoPlugin {
        let hub = Hub {
            on: false, target: None, look: Look::default(), depth: 0.0, height: 0.0, swap: false, size: DEFAULT_SIZE, ring: VecDeque::new(),
            frame_ns: DEFAULT_FRAME_NS, last_in: None, seen: None, pictures: 0,
        };
        let shared = Arc::new(Shared { hub: Mutex::new(hub), store: Mutex::new(Arc::new(NoFrames)) });
        StereoPlugin {
            on: false, look: Look::default(), depth: 0.0, height: 0.0, swap: false, target: None, camera_size: None, canvas: (None, None), shared,
            pending: false, held: None,
        }
    }

    // ── The picture ───────────────────────────────────────────────────

    /// The picture's size: the canvas size Setup asks for, else what the Telescope camera opened at, else 1920x1080.
    pub fn size(&self) -> (u32, u32) {
        match self.canvas {
            (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
            _ => self.camera_size.unwrap_or(DEFAULT_SIZE),
        }
    }

    /// Hands what the step needs to it.
    fn prepare(&mut self) {
        let size = self.size();
        let mut hub = self.shared.hub();
        hub.on = self.on;
        (hub.look, hub.depth, hub.height, hub.swap, hub.size) = (self.look, self.depth, self.height, self.swap, size);
        if !self.on {
            hub.forget();
        }
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
            hub.target = target;
            // another stream's frames aren't this one's
            hub.forget();
        }
        if size.is_some() && size != self.camera_size {
            self.camera_size = size;
        }
        self.prepare();
        self.show(host);
    }

    // ── Card ──────────────────────────────────────────────────────────

    fn fixed_props() -> Value {
        json!({
            "switch-name": tr("3D"), "switch-help": tr(SWITCH_TIP),
            "look-items": [tr("Side by side"), tr("Red/cyan")], "look-tip": tr(LOOK_TIP),
            "depth-tip": tr(DEPTH_TIP), "height-tip": tr(HEIGHT_TIP), "swap-tip": tr(SWAP_TIP), "link-tip": tr(LINK_TIP),
        })
    }

    /// What the badge says: (word, kind, sentence), blank while it's off.
    fn status(&self, host: &dyn Host) -> (String, &'static str, String) {
        let none = (String::new(), "status_dim", String::new());
        if !self.on {
            return none;
        }
        if host.stream_count() < 2 {
            return (tr("Needs two streams"), "status_dim", tr("Start a second stream to get the 3D picture."));
        }
        let seen = self.shared.hub().seen.filter(|(_, at)| monotonic_ns().saturating_sub(*at) < SEEN_NS);
        match seen {
            None => (tr("Waiting"), "status_dim", tr("Both streams have to send a picture, and one of them has to be on the Telescope camera.")),
            Some((SyncStatus::InSync, _)) => (tr("In sync"), "status_ok", tr("The two pictures were taken within half a frame of each other.")),
            Some((SyncStatus::Rough, _)) => (tr("Rough sync"), "status_warn", tr("The two pictures were taken a little apart, so fast movement can look doubled.")),
            Some((SyncStatus::NotSynced, _)) => (
                tr("Not synced"),
                "status_warn",
                tr("Telescope can't match the pictures by when they were taken (both phones need the newer phone app), so it uses the newest of each."),
            ),
        }
    }

    fn card_props(&self, host: Option<&dyn Host>) -> Value {
        let mut props = Self::fixed_props();
        let (status, kind, detail) = host.map_or((String::new(), "status_dim", String::new()), |h| self.status(h));
        let link = host.is_some_and(|h| h.stream_count() >= 2);
        if let Value::Object(o) = &mut props {
            o.extend([
                ("on".to_string(), json!(self.on)),
                ("look-current".to_string(), json!(self.look.index())),
                ("depth".to_string(), json!(self.depth)),
                ("depth-text".to_string(), json!(percent_text(self.depth))),
                ("height-shift".to_string(), json!(self.height)),
                ("height-text".to_string(), json!(percent_text(self.height))),
                ("swap".to_string(), json!(self.swap)),
                ("link-enabled".to_string(), json!(link)),
                ("status".to_string(), json!(status)),
                ("status-kind".to_string(), json!(kind)),
                ("status-detail".to_string(), json!(detail)),
            ]);
        }
        props
    }

    fn show(&mut self, host: &mut dyn Host) {
        // While it's on it pairs other streams' frames, which only keep coming when the steps run, and needs a few of each held
        let two = host.stream_count() >= 2;
        host.want_frames(NAME, self.on);
        host.want_frame_history(NAME, if self.on && two { HISTORY_FRAMES } else { 0 });
        if self.on {
            host.schedule(STATUS_MS, STATUS_TIMER);
        } else {
            host.cancel_schedule(STATUS_TIMER);
        }
        let props = self.card_props(Some(host));
        host.set_ui_props("card", &props);
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            self.canvas = host.canvas_size();
            self.prepare();
            self.show(host);
            host.refresh_actions();
        }
    }

    fn changed(&mut self, host: &mut dyn Host) {
        self.prepare();
        self.show(host);
        host.schedule_save();
    }

    fn set_on(&mut self, host: &mut dyn Host, on: bool) {
        self.on = on;
        self.changed(host);
    }

    /// Both phones get the shown phone's exposure, white balance and focus.
    fn link_exposure(&mut self, host: &mut dyn Host) {
        let ids: Vec<String> = host.frames().stream_frames().into_iter().map(|s| s.source_id).take(2).collect();
        let Some(state) = host.phone_state(None).filter(|_| ids.len() == 2) else { return };
        for body in stereo::link_bodies(&state) {
            for id in &ids {
                host.phone_control(Some(id), &body);
            }
        }
    }

    /// Whether it's on and how it looks, from settings or a preset.
    fn apply(&mut self, cfg: &Value) {
        self.on = cfg.get("on") == Some(&Value::Bool(true));
        self.look = cfg.get("look").and_then(Value::as_str).and_then(Look::from_name).unwrap_or_default();
        let shift = |key: &str| clean_shift(cfg.get(key).and_then(Value::as_f64).unwrap_or(0.0));
        (self.depth, self.height) = (shift("depth"), shift("height"));
        self.swap = cfg.get("swap") == Some(&Value::Bool(true));
        self.prepare();
    }
}

impl Plugin for StereoPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("3D").into(),
            about: n_("Turn two phones into one 3D picture, side by side for a headset or red and cyan for glasses.").into(),
            when_off: n_("Without the 3D plugin there's no 3D card and two streams only go to their own cameras.").into(),
            category: "streaming".into(),
            icon: "cube".into(),
            frame_stage: FrameStage::Combine,
            optional: true,
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

    fn shutdown(&mut self, host: &mut dyn Host) {
        host.want_frames(NAME, false);
        host.want_frame_history(NAME, 0);
        host.cancel_schedule(STATUS_TIMER);
        // so no picture goes out after quitting, and the settings stay as they were
        let mut hub = self.shared.hub();
        hub.on = false;
        hub.forget();
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        match event {
            Event::StreamsChanged { .. } => self.follow_camera(host),
            Event::CameraOpened { slot: 0, .. } => self.follow_camera(host),
            Event::TimerFired { owner, token } if owner == NAME && token == STATUS_TIMER => self.show(host),
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
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "StereoCard".into(), props: self.card_props(None) }, region: "left".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let number = event.args.first().and_then(Value::as_f64);
        match event.callback.as_str() {
            "switch-toggled-by-hand" => self.set_on(host, event.args.first().and_then(Value::as_bool).unwrap_or(false)),
            "look-picked" => {
                if let Some(look) = number.filter(|i| *i >= 0.0).and_then(|i| Look::ALL.get(i as usize)) {
                    self.look = *look;
                    self.changed(host);
                }
            }
            "depth-moved" => {
                self.depth = clean_shift(number.unwrap_or(0.0));
                self.changed(host);
            }
            "height-moved" => {
                self.height = clean_shift(number.unwrap_or(0.0));
                self.changed(host);
            }
            "swap-toggled" => {
                self.swap = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.changed(host);
            }
            "link-clicked" => self.link_exposure(host),
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        vec![actions::toggle_action("stereo.on", &tr("3D"), &tr("3D"), "", &[])]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        if id != "stereo.on" {
            return None;
        }
        let want = actions::toggle_wanted(params, self.on, &mut self.held);
        if want != self.on {
            self.set_on(host, want);
        }
        Some(actions::toggle_text(&tr("3D"), want))
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        if id != "stereo.on" {
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
        Some(FrameStep::Cpu(Box::new(StereoStep(self.shared.clone()))))
    }

    fn get_config(&self) -> Value {
        json!({"on": self.on, "look": self.look.name(), "depth": self.depth, "height": self.height, "swap": self.swap})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.apply(cfg);
        self.pending = true;
        Ok(())
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.catch_up(host);
        self.canvas = host.canvas_size();
        self.apply(cfg);
        self.show(host);
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        if !self.on {
            return vec![("3D".into(), "off".into())];
        }
        let hub = self.shared.hub();
        let sync = match hub.seen.filter(|(_, at)| monotonic_ns().saturating_sub(*at) < SEEN_NS) {
            Some((SyncStatus::InSync, _)) => "in sync",
            Some((SyncStatus::Rough, _)) => "rough sync",
            Some((SyncStatus::NotSynced, _)) => "not synced",
            None => "waiting",
        };
        vec![("3D".into(), format!("on, {}, {sync}", self.look.name()))]
    }
}

#[cfg(test)]
#[path = "stereo_plugin_tests.rs"]
mod tests;
