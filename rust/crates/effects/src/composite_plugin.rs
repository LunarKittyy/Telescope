//! The Several cameras plugin: puts the running streams on one canvas, which goes out on the
//! Telescope camera.
//!
//! The picture is drawn in the frame steps of the stream going to the Telescope camera (slot 0, what call apps watch), from
//! that stream's frame and the newest frame of each other stream (host.frames). While that stream stalls, a clock of its own
//! keeps drawing from its last frame (FrameStore::replace_frame), so the other streams don't freeze with it. The other
//! streams carry on to their own cameras as before. It's a CPU step: a shader step's picture keeps the size it came in at and
//! its textures are fixed when it's handed over, while this one is a new size and reads every stream's newest frame.

use telescope_core::i18n::Fields;
use crate::composite::{clean_layout, clean_layouts, clean_name, layout_dict, same_arrangement, Layout, Tile, MAX_LAYOUTS, STALE_S};
use crate::compositor::{Compositor, Frames};
use crate::shared::{no_pictures, PictureReader};
use crate::transition::{Ease, Kind as TransitionKind, Spec};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;
use telescope_core::actions::{self, Choice};
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, StreamFrame, Touch};
use telescope_core::host::{FrameStore, Host};
use telescope_core::i18n::{n_, tr};
use telescope_core::output::Outputs;
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, PresetOption, StreamInfo};
use telescope_core::settings::{off_default, reset_config};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "composite";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/composite.slint"));
/// How often the picture is drawn while the stream it goes out on stalls.
pub const FALLBACK_FPS: u64 = 30;
/// What the Telescope camera opens at before any stream has said how big it is (vcam.DEFAULT_SIZE).
pub const DEFAULT_SIZE: (u32, u32) = (1920, 1080);
const STALE_NS: u64 = (STALE_S * 1e9) as u64;
const SWITCH_TIP: &str = n_("Combine the running streams into one picture.");
const DIRECTOR_NAME: &str = n_("Auto director");
const DIRECTOR_TIP: &str = n_("Swaps between your saved layouts as people move. Hands held in view of another stream bring in the layout that shows that stream biggest. The layout you picked is shown again after the hands leave.");
const LAYOUT_TIP: &str = n_("A layout you saved in the editor.");

/// What the card calls a kind of transition.
fn kind_name(kind: TransitionKind) -> String {
    match kind {
        TransitionKind::Cut => tr("Cut"),
        TransitionKind::Crossfade => tr("Crossfade"),
        TransitionKind::Slide => tr("Slide"),
        TransitionKind::Push => tr("Push"),
        TransitionKind::Zoom => tr("Zoom"),
        TransitionKind::Wipe => tr("Wipe"),
        TransitionKind::Glide => tr("Glide"),
        TransitionKind::Shuffle => tr("Shuffle"),
        TransitionKind::Bounce => tr("Bounce"),
        TransitionKind::Iris => tr("Iris"),
        TransitionKind::Blinds => tr("Blinds"),
        TransitionKind::Curtain => tr("Curtain"),
        TransitionKind::Pixelate => tr("Pixelate"),
        TransitionKind::Dissolve => tr("Dissolve"),
    }
}

fn ease_name(ease: Ease) -> String {
    match ease {
        Ease::Springy => tr("Springy"),
        Ease::Smooth => tr("Smooth"),
        Ease::Bouncy => tr("Bouncy"),
        Ease::Even => tr("Even"),
    }
}

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
    /// A copy of the hub's (on, target), so touch() never waits for a drawing that holds the hub.
    gate: RwLock<(bool, Option<String>)>,
    /// When the target stream's frame last reached the step or left it, so the clock doesn't draw over a frame that is on its way.
    fed_at: AtomicU64,
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

    fn set_gate(&self, on: Option<bool>, target: Option<Option<String>>) {
        let mut gate = self.gate.write().unwrap_or_else(|e| e.into_inner());
        if let Some(on) = on {
            gate.0 = on;
        }
        if let Some(target) = target {
            gate.1 = target;
        }
    }

    /// Whether the picture is on and source_id is the stream it goes out on, without the hub lock.
    pub(crate) fn is_target(&self, source_id: &str) -> bool {
        let gate = self.gate.read().unwrap_or_else(|e| e.into_inner());
        gate.0 && gate.1.as_deref() == Some(source_id)
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
        // the lock goes back only after this, so the clock waiting for it sees the frame just handed on
        let _fed = Fed(&self.fed_at);
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
        (out.number, out.time_ns, out.capture_ns) = (frame.number, frame.time_ns, frame.capture_ns);
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
        // a frame that reached the step but still waits for the lock or its drawing counts as new
        let at = at.max(self.fed_at.load(Ordering::Relaxed));
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
        // nothing else to keep moving, but a transition does
        if !hub.compositor.has_transition() && (hub.fallback_key.as_ref() == Some(&key) || hub.compositor.live_tiles(&others) == 0) {
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
    /// While frames arrive it sleeps until the newest one would count as stalled, instead of looking every frame time.
    fn run_clock(&self) {
        let period = 1_000_000_000 / FALLBACK_FPS;
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
                let last = hub.main_frame.as_ref().map(|(_, at)| (*at).max(self.fed_at.load(Ordering::Relaxed)));
                let wait = Self::clock_wait(last, monotonic_ns(), period);
                // the wake lets shutdown cut the sleep short
                let (hub, _) = self.wake.wait_timeout(hub, Duration::from_nanos(wait)).unwrap_or_else(|e| e.into_inner());
                if hub.shut {
                    return;
                }
            }
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.draw_stalled(monotonic_ns()))).is_err() {
                log::error!("Drawing the picture while its stream stalls failed");
            }
        }
    }

    /// How long the clock sleeps (ns): a frame time while the stream is stalled or unheard from, else until its newest frame counts as
    /// stalled (two frame times old, as `draw_stalled` sees it). A frame stamped in the future counts as new, so it never exceeds that.
    fn clock_wait(last_frame: Option<u64>, now: u64, period: u64) -> u64 {
        match last_frame {
            Some(at) if now.saturating_sub(at) < 2 * period => 2 * period - now.saturating_sub(at),
            _ => period,
        }
    }
}

/// The step every stream gets; only the target stream's does anything.
#[path = "composite_editor.rs"]
mod editor;
#[path = "composite_director.rs"]
pub mod director;

/// Stamps the time it is dropped at, in the target stream's fed_at.
struct Fed<'a>(&'a AtomicU64);

impl Drop for Fed<'_> {
    fn drop(&mut self) {
        self.0.store(monotonic_ns(), Ordering::Relaxed);
    }
}

struct CompositeStep(Arc<Shared>);

impl CpuStep for CompositeStep {
    fn touch(&self, ctx: &dyn FrameContext) -> Touch {
        if self.0.is_target(ctx.source_id()) {
            self.0.fed_at.store(monotonic_ns(), Ordering::Relaxed);
            Touch::Changes
        } else {
            Touch::Nothing
        }
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
    /// The short word the card shows for `note`, which is its tooltip.
    note_short: String,
    /// The kind of `note_short` ("status_ok", "status_warn" or "status_dim").
    note_kind: &'static str,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
    /// Where the switch was when a held key began.
    held: Option<bool>,
    /// The background pictures' reader, for the editor's own compositor.
    reader: PictureReader,
    /// The editor's state while its window is open.
    editor: Option<editor::Editor>,
    /// Setup's Max zoom: a tile zooms in at most this far.
    max_zoom: f64,
    /// The streams whose Cut out was last announced on (Event::CutOutWantedBy), so only a change is announced again.
    cut_outs_told: BTreeSet<String>,
    /// How the picture goes from one layout to the next when one is switched to.
    transition: Spec,
    /// The Auto director's switch, and what it has seen while it's on.
    director_on: bool,
    director: director::Director,
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
        let shared = Arc::new(Shared { hub: Mutex::new(hub), gate: RwLock::new((false, None)), fed_at: AtomicU64::new(0), wake: Condvar::new(), store: Mutex::new(Arc::new(NoFrames)) });
        let mut plugin = CompositePlugin {
            on: false, layout: Layout::default(), layouts: Vec::new(), target: None, camera_size: None, canvas: (None, None), shared,
            clock: None, camera_label, note: String::new(), note_short: String::new(), note_kind: "status_dim", pending: false, held: None, reader, editor: None,
            max_zoom: crate::composite::DEFAULT_MAX_ZOOM, cut_outs_told: BTreeSet::new(), transition: Spec::default(), director_on: false,
            director: director::Director::new(),
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

    /// A tile as it is drawn: no zoom goes past the Max zoom setting. The layout keeps the zoom it was given, so raising Max zoom brings it back.
    fn capped(&self, tile: &Tile) -> Tile {
        Tile { zoom: tile.zoom.min(self.max_zoom), ..tile.clone() }
    }

    /// The layout as it is drawn.
    pub(crate) fn shown_layout(&self) -> Layout {
        Layout { tiles: self.layout.tiles.iter().map(|t| self.capped(t)).collect(), ..self.layout.clone() }
    }

    fn prepare(&mut self) {
        self.prepare_with(None);
    }

    /// prepare(), the picture going to the new layout the way `transition` says when there is one.
    fn prepare_with(&mut self, transition: Option<Spec>) {
        let size = self.size();
        let on = self.on;
        let shown = self.shown_layout();
        let mut hub = self.shared.hub();
        // Off, nothing draws: the background (a canvas-sized picture) is only worked out while it's on
        if on {
            match transition {
                Some(spec) => hub.compositor.prepare_with(&shown, size, spec, monotonic_ns()),
                None => hub.compositor.prepare(&shown, size),
            }
            // a held fallback picture was drawn with the old layout, and the clock can't cache one from it before this plan is in
            hub.fallback_key = None;
        } else {
            hub.compositor = Compositor::new(self.reader.clone());
        }
        hub.on = on;
        self.shared.set_gate(Some(on), None);
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
            self.shared.set_gate(None, Some(target.clone()));
            // another stream's frame isn't this one's
            (hub.main_frame, hub.handed, hub.fallback_key) = (None, None, None);
            // and neither is its stamp, which would hold back the new target's first stall redraw
            self.shared.fed_at.store(0, Ordering::Relaxed);
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

    /// set_layout() for switching to another layout (one picked, a preset, the Auto director): the picture goes there the way the
    /// transition says, where an edit in the editor takes effect at once.
    pub fn switch_layout(&mut self, host: &mut dyn Host, layout: Layout) {
        self.layout = layout;
        self.canvas = host.canvas_size();
        let (reduced, off) = host.reduced_motion();
        self.prepare_with(Some(self.transition.for_motion(reduced, off)));
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
        self.switch_layout(host, layout);
        // a layout picked by hand is home for the director
        self.director.picked(monotonic_ns() as f64 / 1e9, name);
        true
    }

    // ── Auto director ─────────────────────────────────────────────────

    const DIRECTOR_TIMER: &'static str = "director";
    const DIRECTOR_MS: u64 = 500;

    /// Whether the director is working: switched on, with the picture on and a layout to come home to.
    fn directing(&self) -> bool {
        self.on && self.director_on && self.layouts.len() >= 2
    }

    /// The streams with a recent frame, from the store.
    fn fresh_streams(&self) -> BTreeSet<String> {
        let now = monotonic_ns();
        self.shared.store().stream_frames().into_iter().filter(|f| f.age(now) < STALE_NS).map(|f| f.source_id).collect()
    }

    /// Whether a layout can be drawn now: at least two of its streams are sending.
    fn drawable(layout: &Layout, fresh: &BTreeSet<String>) -> bool {
        layout.tiles.iter().filter(|t| fresh.contains(&t.source_id)).count() >= 2
    }

    /// Asks for people on every stream and the half second clock while it's working, gives them back when it isn't.
    fn sync_director(&mut self, host: &mut dyn Host) {
        let working = self.directing();
        host.want_people(NAME, working);
        if working {
            if self.director.home().is_empty() {
                // the layout in use is home, if it's a saved one
                let saved = self.saved_match();
                if !saved.is_empty() {
                    self.director.picked(monotonic_ns() as f64 / 1e9, &saved);
                }
            }
            host.schedule(Self::DIRECTOR_MS, Self::DIRECTOR_TIMER);
        } else {
            host.cancel_schedule(Self::DIRECTOR_TIMER);
            // a cut that is on is put back to the layout picked by hand, and the next time it starts from what is shown then
            let home = self.director.is_away().then(|| self.layouts.iter().find(|l| l.name == self.director.home()).cloned()).flatten();
            self.director.reset();
            if let Some(layout) = home {
                self.switch_layout(host, layout);
            }
        }
    }

    /// One look at what the streams show, at now (seconds): switches to a layout when the director says it's time.
    pub(crate) fn direct(&mut self, host: &mut dyn Host, now: f64) {
        // someone editing the picture is in charge of it
        if !self.directing() || self.editor.is_some() {
            return;
        }
        let fresh = self.fresh_streams();
        self.director.retain(&fresh);
        if let Some(name) = self.director.decide(now, &self.layouts, &|l| Self::drawable(l, &fresh)) {
            if let Some(layout) = self.layouts.iter().find(|l| l.name == name).cloned() {
                self.switch_layout(host, layout);
            }
        }
    }

    fn hear_people(&mut self, host: &mut dyn Host, source_id: &str, people: &[Value]) {
        if !self.directing() {
            return;
        }
        let now = monotonic_ns() as f64 / 1e9;
        let fresh = self.fresh_streams();
        // hands count where the layout that would cut to this stream shows them
        let hands = self.director.layout_for(&self.layouts, source_id, &|l| Self::drawable(l, &fresh))
            .and_then(|l| l.tiles.iter().find(|t| t.source_id == source_id))
            .zip(self.shared.store().stream_frame(Some(source_id)))
            .is_some_and(|(tile, sf)| {
                let (sw, sh) = (sf.frame.width as f64, sf.frame.height as f64);
                let (cw, ch) = self.size();
                let (x0, y0, x1, y1) = crate::composite::shown(tile, sw, sh, tile.w * cw as f64, tile.h * ch as f64);
                people.iter().filter_map(telescope_core::pose::Person::from_value).any(|p| {
                    ["left_wrist", "right_wrist"].iter().filter_map(|w| p.point(w, 0.5)).any(|(x, y)| (x0..=x1).contains(&(x * sw)) && (y0..=y1).contains(&(y * sh)))
                })
            });
        self.director.hear(now, source_id, hands);
        self.direct(host, now);
    }

    fn set_director(&mut self, host: &mut dyn Host, on: bool) {
        self.director_on = on;
        self.sync_director(host);
        self.show(host);
        host.schedule_save();
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
            self.layout.name = new.clone();
        }
        self.director.rename_home(old, &new);
        self.show(host);
        host.refresh_actions();
        host.schedule_save();
    }

    pub fn delete_layout(&mut self, host: &mut dyn Host, name: &str) {
        self.layouts.retain(|l| l.name != name);
        // the home layout is gone: the next one picked, or the one shown when it's looked at again, is home
        if self.director.home() == name {
            self.director.reset();
        }
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
        json!({"switch-name": tr("One picture"), "switch-help": tr(SWITCH_TIP), "layout-tip": tr(LAYOUT_TIP), "edit-shown": true,
            "director-name": tr(DIRECTOR_NAME), "director-detail": tr(DIRECTOR_TIP)})
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
                ("director".to_string(), json!(self.director_on)),
                ("director-enabled".to_string(), json!(names.len() >= 2 || self.director_on)),
                ("status".to_string(), json!(self.note_short)),
                ("status-kind".to_string(), json!(self.note_kind)),
                ("status-detail".to_string(), json!(self.note)),
                ("reset-shown".to_string(), json!(self.reset_shown())),
                ("transition-items".to_string(), json!(TransitionKind::ALL.iter().map(|k| kind_name(*k)).collect::<Vec<_>>())),
                ("transition-current".to_string(), json!(TransitionKind::ALL.iter().position(|k| *k == self.transition.kind).unwrap_or(0))),
                ("ease-items".to_string(), json!(Ease::ALL.iter().map(|e| ease_name(*e)).collect::<Vec<_>>())),
                ("ease-current".to_string(), json!(Ease::ALL.iter().position(|e| *e == self.transition.ease).unwrap_or(0))),
                ("transition-ms".to_string(), json!(self.transition.ms)),
                ("transition-ms-text".to_string(), json!(format!("{} ms", self.transition.ms))),
                ("transition-shown".to_string(), json!(self.transition.kind != TransitionKind::Cut)),
            ]);
        }
        props
    }

    /// What a Reset leaves alone: the switch and the saved layouts.
    const RESET_KEPT: &'static [&'static str] = &["on", "layouts", "director"];

    fn reset_shown(&self) -> bool {
        off_default(&self.get_config(), &CompositePlugin::new().get_config(), Self::RESET_KEPT)
    }

    fn reset(&mut self, host: &mut dyn Host) {
        let cfg = reset_config(&self.get_config(), &CompositePlugin::new().get_config(), Self::RESET_KEPT);
        if self.apply_preset(host, &cfg).is_ok() {
            host.schedule_save();
        }
    }

    /// The streams whose tile has Cut out on while the picture is on.
    fn cut_outs(&self) -> BTreeSet<String> {
        if !self.on {
            return BTreeSet::new();
        }
        self.layout.tiles.iter().filter(|t| t.cut_out).map(|t| t.source_id.clone()).collect()
    }

    const CUT_OUT_TIMER: &'static str = "cut-outs";
    const CUT_OUT_CHECK_MS: u64 = 200;

    /// Tells whoever cuts people out which streams want it: what changed since last time, or every one again for a plugin that missed it.
    /// A tile a transition is still drawing keeps its cut-out until the transition ends, which a timer looks for.
    fn tell_cut_outs(&mut self, host: &mut dyn Host, again: bool) {
        let mut want = self.cut_outs();
        let leaving = if self.on { self.shared.hub().compositor.leaving_cut_outs() } else { BTreeSet::new() };
        if leaving.is_empty() {
            host.cancel_schedule(Self::CUT_OUT_TIMER);
        } else {
            host.schedule(Self::CUT_OUT_CHECK_MS, Self::CUT_OUT_TIMER);
            want.extend(leaving);
        }
        let off: Vec<String> = self.cut_outs_told.difference(&want).cloned().collect();
        let on: Vec<String> = want.iter().filter(|s| again || !self.cut_outs_told.contains(*s)).cloned().collect();
        self.cut_outs_told = want;
        for source_id in off {
            host.emit(Event::CutOutWantedBy { owner: NAME.into(), source_id: source_id.clone(), on: false });
            host.emit(Event::CutOutWanted { source_id, on: false });
        }
        for source_id in on {
            host.emit(Event::CutOutWantedBy { owner: NAME.into(), source_id: source_id.clone(), on: true });
            host.emit(Event::CutOutWanted { source_id, on: true });
        }
    }

    fn show(&mut self, host: &mut dyn Host) {
        self.tell_cut_outs(host, false);
        // While it's on it draws from other streams' frames, which only keep coming when the steps run
        host.want_frames(NAME, self.on);
        let live = self.layout.tiles.iter().filter(|t| host.is_streaming_from(Some(&t.source_id))).count();
        // the stream it draws on sends the picture of several, not one camera's view
        host.preview_composited(NAME, self.target.as_deref().filter(|_| self.on && live >= 2));
        let camera = self.camera_label.as_str();
        self.note = if !self.on {
            String::new()
        } else if self.layout.tiles.is_empty() {
            tr("Nothing's in the picture yet.")
        } else if live < 2 {
            tr("Active once two streams are running.")
        } else if self.target.is_none() {
            tr("Set to output on {camera} but no stream is running there.").field("camera", camera)
        } else if self.shared.store().stream_frame(self.target.as_deref()).is_none() {
            tr("Will output on {camera} once the stream there sends a picture.").field("camera", camera)
        } else {
            tr("Outputting on {camera}.").field("camera", camera)
        };
        (self.note_short, self.note_kind) = if !self.on {
            (String::new(), "status_dim")
        } else if self.layout.tiles.is_empty() {
            (tr("Nothing in it"), "status_dim")
        } else if live < 2 {
            (tr("Two streams required"), "status_warn")
        } else if self.target.is_none() || self.shared.store().stream_frame(self.target.as_deref()).is_none() {
            (tr("Not going out"), "status_warn")
        } else {
            (tr("Going out"), "status_ok")
        };
        host.set_ui_props("card", &self.card_props());
        self.sync_director(host);
        self.show_editor(host);
    }

    /// A reader of how the last picture's time split up (scaling, drawing, canvas setup) and whether a transition was playing, for the speed tests.
    pub fn render_times(&self) -> impl Fn() -> (crate::compositor::RenderTimes, bool) + Send + Sync + 'static {
        let shared = self.shared.clone();
        move || {
            let hub = shared.hub();
            (hub.compositor.last_times(), hub.compositor.has_transition())
        }
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
        self.prepare();
        self.wake_clock();
        self.show(host);
        host.schedule_save();
    }

    /// The picture's look and whether it's on, from settings or a preset (the saved layouts aside).
    fn apply(&mut self, cfg: &Value) {
        self.take(cfg, false);
        self.prepare();
        self.wake_clock();
    }

    /// The settings of cfg, without drawing anything yet. A preset saved before transitions had none and leaves the card's alone.
    fn take(&mut self, cfg: &Value, keep_transition: bool) {
        self.layout = cfg.get("layout").and_then(clean_layout).unwrap_or_default();
        self.on = cfg.get("on") == Some(&Value::Bool(true));
        self.director_on = cfg.get("director") == Some(&Value::Bool(true));
        // another picture is home from here on
        self.director.reset();
        if !(keep_transition && cfg.get("transition").is_none()) {
            self.transition = Spec::clean(cfg.get("transition"));
        }
    }
}

impl Plugin for CompositePlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Several cameras in one picture").into(),
            about: n_("Put the streams you run side by side or inside each other on one virtual camera.").into(),
            when_off: n_("Without the Several cameras in one picture plugin there's no One picture card and each stream only goes to its own camera.").into(),
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

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        // forgetting what was told makes catch_up's show, or the call after it, say it all again
        self.cut_outs_told.clear();
        self.catch_up(host);
        self.tell_cut_outs(host, false);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        for source_id in std::mem::take(&mut self.cut_outs_told) {
            host.emit(Event::CutOutWantedBy { owner: NAME.into(), source_id: source_id.clone(), on: false });
            host.emit(Event::CutOutWanted { source_id, on: false });
        }
        host.cancel_schedule(Self::CUT_OUT_TIMER);
        host.want_frames(NAME, false);
        host.want_people(NAME, false);
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
            Event::MaxZoomChanged { max_zoom } => {
                self.max_zoom = (*max_zoom as f64).max(1.0);
                self.canvas = host.canvas_size();
                self.prepare();
                self.show(host);
            }
            Event::PeopleSeen { source_id, people } => self.hear_people(host, source_id, people),
            Event::TimerFired { owner, token } if owner == NAME && token == Self::DIRECTOR_TIMER => {
                self.direct(host, monotonic_ns() as f64 / 1e9);
                self.sync_director(host);
            }
            Event::TimerFired { owner, token } if owner == NAME && token == Self::CUT_OUT_TIMER => self.tell_cut_outs(host, false),
            Event::StreamsChanged { .. } => self.follow_camera(host),
            Event::SourceShown { .. } => self.tell_cut_outs(host, true),
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
            "director-toggled" => {
                let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.set_director(host, on);
            }
            "reset-clicked" => self.reset(host),
            "switch-toggled-by-hand" => {
                let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.set_on(host, on);
            }
            "transition-picked" | "ease-picked" | "transition-length-moved" => {
                let n = event.args.first().and_then(Value::as_f64).unwrap_or(-1.0);
                match event.callback.as_str() {
                    "transition-picked" => {
                        if let Some(k) = (n >= 0.0).then(|| TransitionKind::ALL.get(n as usize)).flatten() {
                            self.transition.kind = *k;
                        }
                    }
                    "ease-picked" => {
                        if let Some(e) = (n >= 0.0).then(|| Ease::ALL.get(n as usize)).flatten() {
                            self.transition.ease = *e;
                        }
                    }
                    _ if n.is_finite() => self.transition.ms = (n.round() as i64).clamp(i64::from(crate::transition::MIN_MS), i64::from(crate::transition::MAX_MS)) as u32,
                    _ => {}
                }
                self.show(host);
                host.schedule_save();
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
                // choosing the layout the director cut to is a pick by hand too
                if pick || self.director.is_away() {
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

    fn settings(&self) -> Vec<telescope_core::settings::SettingSpec> {
        use telescope_core::settings::SettingSpec;
        let kinds: Vec<(&str, &str)> = TransitionKind::ALL.iter().map(|k| (k.id(), k.id())).collect();
        let eases: Vec<(&str, &str)> = Ease::ALL.iter().map(|e| (e.id(), e.id())).collect();
        let d = Spec::default();
        vec![
            SettingSpec::switch("on", "Whether the streams are put into one picture", false),
            SettingSpec::choice("transition.kind", "How the picture goes to another layout: cut, crossfade, slide, push, zoom, wipe, iris, blinds, curtain, pixelate, dissolve, or glide, shuffle and bounce (tiles travel to their new places)", &kinds, d.kind.id()),
            SettingSpec::number("transition.ms", "How long that takes", f64::from(crate::transition::MIN_MS), f64::from(crate::transition::MAX_MS), 0, f64::from(d.ms)).unit("ms"),
            SettingSpec::choice("transition.ease", "How it eases: springy, smooth, bouncy or even", &eases, d.ease.id()),
        ]
    }

    fn get_config(&self) -> Value {
        // a layout the director cut to isn't what's saved: the one picked by hand is
        let manual = if self.director.is_away() { self.layouts.iter().find(|l| l.name == self.director.home()).unwrap_or(&self.layout) } else { &self.layout };
        let mut cfg = json!({"on": self.on, "layout": layout_dict(manual), "layouts": self.layouts.iter().map(layout_dict).collect::<Vec<_>>(), "transition": self.transition.dict()});
        // only written while it's on, so settings that never used it read as they always did
        if self.director_on {
            cfg["director"] = json!(true);
        }
        cfg
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.layouts = clean_layouts(cfg.get("layouts").unwrap_or(&Value::Null));
        self.apply(cfg);
        self.pending = true;
        Ok(())
    }

    /// A preset saves whether the picture is on and how it looks, not the saved layouts (which it never brings back, and a copy of them would only go out of date).
    fn preset_config(&self) -> Value {
        let mut cfg = self.get_config();
        if let Value::Object(map) = &mut cfg {
            map.remove("layouts");
        }
        cfg
    }

    fn preset_options(&self) -> Vec<PresetOption> {
        vec![PresetOption {
            id: "layout".into(),
            label: tr("One picture layout"),
            items: vec![("layout".into(), tr("Bring this layout")), ("keep".into(), tr("Leave the layout"))],
        }]
    }

    /// "Leave the layout" saves no layout, so applying it leaves whatever the picture shows when it's applied.
    fn preset_config_with(&self, picks: &[(String, String)]) -> Value {
        let mut cfg = self.preset_config();
        if picks.iter().any(|(id, v)| id == "layout" && v == "keep") {
            if let Value::Object(map) = &mut cfg {
                map.remove("layout");
            }
        }
        cfg
    }

    /// A preset brings back whether the picture is on and how it looks, and leaves the saved layouts alone. One saved without a layout leaves the current one.
    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.catch_up(host);
        self.canvas = host.canvas_size();
        let current = self.layout.clone();
        self.take(cfg, true);
        if cfg.get("layout").is_none() {
            self.layout = current;
            self.prepare();
        } else {
            // a preset that brings another layout goes there the way a switch does
            let (reduced, off) = host.reduced_motion();
            self.prepare_with(Some(self.transition.for_motion(reduced, off)));
        }
        self.wake_clock();
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
