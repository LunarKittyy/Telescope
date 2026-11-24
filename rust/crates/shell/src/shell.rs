//! The window's state without the window: the plugins, their cards and settings, banners, folds and saving (the
//! TelescopeWindow minus Qt). The Slint side (window.rs) drains `effects` after each call and shows the result.

use telescope_core::i18n::Fields;
use crate::banners::Banners;
use crate::collapse::Collapsing;
use crate::layout::{CardLayout, Panels, Region};
use crate::{profiles, switches};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use telescope_core::colour::ColourPipeline;
use telescope_core::config::{merge_for_save, ConfigFile, PluginSection, WindowState};
use telescope_core::event::Event;
use telescope_core::host::{RequestId, View};
use telescope_core::i18n::tr;
use telescope_core::plugin::{load_config_tolerantly, Plugin, PluginInfo};
use telescope_core::services::PluginProblem;
use telescope_core::ui::{ActionSpec, CardSpec, Issue, IssueAction, IssueDoes, TrayAction, UiEvent, UiSpec};
use telescope_core::EventBus;

/// One registered plugin and what it put in the window. `plugin` is out (None) only while one of its hooks runs.
/// How many events one drain hands out before letting the window draw, so plugins feeding each other can't freeze it.
const DELIVERY_BATCH: u32 = 10_000;

pub struct Slot {
    pub plugin: Option<Box<dyn Plugin>>,
    pub info: PluginInfo,
    pub card: Option<CardSpec>,
    pub header: Option<UiSpec>,
    pub actions: Vec<ActionSpec>,
    pub tray: Vec<TrayAction>,
    /// get_config() right after setup, which a device profile starts from.
    pub defaults: Value,
    /// Its hook timer in the stats hub, looked up the first time one is timed.
    pub timer: Option<Arc<telescope_core::stats::Timer>>,
    /// What get_config() was when its settings were last looked at for what moved (a save it asked for, a preset handed to it), by camera for a
    /// device-local plugin (see `State::setting_scope`).
    pub seen: Vec<(Option<String>, Value)>,
    /// When each dotted path of its config last moved, by the state's setting clock (Host::setting_revision), by camera for a device-local plugin.
    pub moved: Vec<(Option<String>, String, u64)>,
    /// Why it was switched off this run after panicking (None while it runs). Its hooks aren't called again until the next start.
    pub stopped: Option<String>,
    /// It has handed out a step that hides something (Background, Privacy, Cover, or a CPU step that says it hides) at some point this run.
    pub hides: bool,
    /// Its shutdown() was called (Telescope quitting, or it stopped): never called twice.
    pub shut_down: bool,
    /// Telescope is quitting and its shutdown() has finished: it hears nothing more.
    pub ended: bool,
    /// Its setup() hasn't finished: a panic there leaves it out of the start instead of stopping it.
    starting: bool,
}

/// The plugins' packet step makers by owner, in the order they came on (HostPackets).
#[derive(Clone, Default)]
pub struct PacketMakers(pub Vec<(String, Arc<dyn telescope_core::host::PacketStepFactory>)>);

impl PacketMakers {
    pub fn owners(&self) -> Vec<String> {
        self.0.iter().map(|(o, _)| o.clone()).collect()
    }
}

impl std::fmt::Debug for PacketMakers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PacketMakers({:?})", self.owners())
    }
}

impl PartialEq for PacketMakers {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len() && self.0.iter().zip(&other.0).all(|(a, b)| a.0 == b.0 && Arc::ptr_eq(&a.1, &b.1))
    }
}

/// A source a plugin offers, which Effect compares by which one it is.
#[derive(Clone)]
pub struct OfferedSource(pub Arc<dyn telescope_core::source::StreamSource>);

impl std::fmt::Debug for OfferedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OfferedSource({})", self.0.id())
    }
}

impl PartialEq for OfferedSource {
    fn eq(&self, other: &OfferedSource) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A picture a plugin set on its UI, which Effect compares by what it shows.
#[derive(Clone, Debug)]
pub struct Picture(pub telescope_core::Frame);

impl PartialEq for Picture {
    fn eq(&self, other: &Picture) -> bool {
        let (a, b) = (&self.0, &other.0);
        (a.width, a.height, a.format) == (b.width, b.height, b.format) && (Arc::ptr_eq(&a.data, &b.data) || a.data == b.data)
    }
}

/// Something a plugin asked for that the window (or the stream flow) carries out once the hook returns.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Props { plugin: String, ui: String, props: Value },
    /// Host::set_ui_image: a picture for an image property of plugin's piece of UI.
    Image { plugin: String, ui: String, prop: String, picture: Picture },
    /// Host::refresh_actions: plugin's actions are asked for again.
    RefreshActions(String),
    /// A plugin panicked and is off until the next start: its card, header widget and windows go and its frame steps come out.
    Stopped(String),
    /// HostSources: a plugin's stream source offered, taken back, or to start.
    AddSource(OfferedSource),
    RemoveSource(String),
    StreamSource(String),
    /// HostUi::open_window and close_window: a window of the plugin's own.
    OpenWindow { plugin: String, id: String, window: telescope_core::ui::WindowSpec },
    CloseWindow { plugin: String, id: String },
    Tray(TrayAction),
    FrameStep(String),
    /// A plugin began hiding something: streams behind the panels run the steps they have on their last frame (Blur now follows them), before the shown stream's frame steps are asked for again.
    HideBehind,
    OpenSettings(String),
    /// Event::SettingsTouched: the controls showing these settings get an outline for a moment.
    TouchSettings { ids: Vec<String>, changed: bool },
    Notify { title: String, body: String, urgent: bool },
    Start { interactive: bool },
    StartAgain(Option<String>),
    Stop(Option<String>),
    StopAll,
    /// Restart a stream for new connection settings: source_id's, or the panels' without one.
    Reconnect(Option<String>),
    PhoneControl { source_id: Option<String>, body: Value },
    RequestPhoneState { source_id: Option<String> },
    /// The packet steps the shown stream's H.264 goes through now, after one plugin's change_packets.
    Packets(PacketMakers),
    CameraOn { on: bool, auto: bool },
    /// A resolution change went to the phone, which has to reopen its camera.
    ResolutionPending { width: u32, height: u32 },
    /// The phone is restarting its camera (a lens switch, a new size).
    SettleStream,
    /// The mic was switched, or another source was picked: what the camera can do may have changed.
    MicChanged,
    SourceSelected,
    ToggleWindow,
    /// Host::update_stream_output: a new size or rate for a stream's camera (the focused one's without an id).
    UpdateOutput { change: telescope_core::host::OutputChange, source_id: Option<String> },
    /// Host::make_main: the mic and the plugins that stay with it move to this stream.
    MakeMain(String),
    /// Host::pick_file: plugin's request, answered by a RequestDone once the person picks.
    PickFile { plugin: String, request: RequestId, dialog: telescope_core::host::FileDialog },
    /// Host::copy_text: this goes on the clipboard.
    CopyText(String),
    /// Host::take_still: the photo for `plugin`'s request on stream `source`, answered by RequestProgress and RequestDone.
    TakeStill { plugin: String, request: RequestId, source: String, options: Value, folder: String },
    /// Host::set_plugin_hues: every card in its group's colour, or all in the accent.
    PluginHues(bool),
    /// Host::set_look: the look the windows draw.
    Look(telescope_core::host::Look),
    /// Host::set_appearance: corners, text size, density, contrast, motion and tooltip timing.
    Appearance(telescope_core::host::Appearance),
    /// Host::restart_vcam_canvas: plugin's request, answered by a RequestDone once the camera is back.
    RestartCanvas { plugin: String, request: RequestId, shows_command: bool },
    /// Host::remove_extra_cameras: plugin's request, answered by a RequestDone.
    RemoveExtras { plugin: String, request: RequestId },
    /// Host::open_url: a link for the browser.
    OpenUrl(String),
    /// Host::hand_over_stream: link prepared url on route next to the stream's running one; the worker tries it.
    HandOver { source_id: String, route: telescope_core::source::RouteSpec, url: String },
    /// Host::set_shortcut_menu: the controls' right-click menu changed.
    ShortcutMenu,
    Quit,
    Restart,
    KeepInTray(bool),
    SaveSoon,
    PickSource(Option<String>),
    /// switch_device's second half, once StopAll before it has run.
    FinishSwitch { new: Option<String>, start: bool },
    /// host.schedule: Event::TimerFired for plugin after ms (moves a pending one with the same token).
    Schedule { plugin: String, token: String, ms: u64 },
    CancelSchedule { plugin: String, token: String },
}

impl Effect {
    /// The plugin an effect is for, when it's one plugin's own.
    fn plugin(&self) -> Option<&str> {
        match self {
            Effect::Props { plugin, .. } | Effect::Image { plugin, .. } | Effect::OpenWindow { plugin, .. } | Effect::CloseWindow { plugin, .. }
            | Effect::PickFile { plugin, .. } | Effect::TakeStill { plugin, .. } | Effect::RestartCanvas { plugin, .. }
            | Effect::RemoveExtras { plugin, .. } | Effect::Schedule { plugin, .. } | Effect::CancelSchedule { plugin, .. } => Some(plugin),
            Effect::RefreshActions(plugin) | Effect::FrameStep(plugin) => Some(plugin),
            _ => None,
        }
    }
}

/// Stage-level hiding remains the default except for the built-in Background comfort effect.
fn stage_hides(info: &PluginInfo, stage: telescope_core::frame::FrameStage) -> bool {
    stage.hides() && !(stage == telescope_core::frame::FrameStage::Background && !info.added && info.name == "background")
}

/// Whether a plugin's step hides something, asked inside the panic guard: a step that panics answering is the plugin's error, not the window's.
fn step_hides(info: &PluginInfo, stage: telescope_core::frame::FrameStage, step: &telescope_core::frame::FrameStep) -> Result<bool, String> {
    use telescope_core::frame::FrameStep;
    if stage_hides(info, stage) {
        return Ok(true);
    }
    match step {
        FrameStep::Cpu(c) | FrameStep::Either(_, c) => telescope_core::diagnostics::catch_plugin(|| c.hides()).map_err(|panic| format!("hides: {}", panic_text(panic.as_ref()))),
        _ => Ok(false),
    }
}

/// What happens when a plugin that hides part of the picture panics: Telescope closes. Its state can't be trusted to go on hiding anything, and
/// an app that's gone shows nothing, where one that carries on might show what was hidden for a frame or more.
fn close_rather_than_show(name: &str, why: &str) {
    log::error!("Plugin {name} was hiding part of the picture and failed ({why}); closing Telescope rather than show what it hid");
    telescope_core::diagnostics::report_closing(telescope_core::diagnostics::events(), &format!("plugin {name}: {why}"));
    std::process::abort();
}

/// Why an added plugin with these built-in plugin names in its needs can't start, None when they're all there. started says whether the
/// built-in plugin of that name runs; one in no built-in's name is a plugin this Telescope doesn't have.
pub fn needs_reason(builtins: &[PluginInfo], needs: &[String], started: impl Fn(&str) -> bool) -> Option<String> {
    let title = |b: &PluginInfo| tr(if b.title.is_empty() { &b.name } else { &b.title });
    let unknown: Vec<String> = needs.iter().filter(|n| !builtins.iter().any(|b| b.name == **n)).cloned().collect();
    let off: Vec<String> = needs.iter().filter(|n| !started(n)).filter_map(|n| builtins.iter().find(|b| b.name == *n)).map(title).collect();
    let mut parts = Vec::new();
    if !unknown.is_empty() {
        parts.push(crate::texts::trn("It needs a plugin called {plugins}, which this version of Telescope doesn't have.",
                                    "It needs plugins called {plugins}, which this version of Telescope doesn't have.", unknown.len())
            .field("plugins", crate::texts::and_list(&unknown)));
    }
    if !off.is_empty() {
        parts.push(crate::texts::trn("It needs {plugins}, which is switched off.", "It needs {plugins}, which are switched off.", off.len())
            .field("plugins", crate::texts::and_list(&off)));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// needs_reason() as the plugin host asks it before running an added plugin's code: against the built-in plugins and the switches saved
/// now (which is what the next start runs).
pub fn needs_gate(builtins: Vec<PluginInfo>, admin: Arc<dyn telescope_core::plugin_admin::PluginAdmin>) -> impl Fn(&[String]) -> Option<String> {
    let (off, on) = (admin.builtin_off(), admin.optional_on());
    let started: std::collections::BTreeSet<String> = builtins.iter()
        .filter(|b| b.required || if b.optional { on.contains(&b.name) } else { !off.contains(&b.name) }).map(|b| b.name.clone()).collect();
    move |needs| needs_reason(&builtins, needs, |n| started.contains(n))
}

/// What a caught panic said, for the log and diagnostics.
pub fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned()).unwrap_or_else(|| "it panicked".into())
}

/// How streaming stands, as the stream flow last said.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Streams {
    pub streaming: bool,
    pub starting: bool,
    pub restarting: bool,
    pub camera_off: bool,
    /// A rule turned the camera off, not the person.
    pub camera_off_auto: bool,
    /// Whether the camera can go off now, and why not.
    pub can_turn_off: (bool, String),
    /// The source the panels speak for is a plugin's, with no phone's video to work on.
    pub plugin_source: bool,
    pub source_id: Option<String>,
    pub phone_state: Option<Value>,
    /// (id, name) of the stream the mic follows, and the ids of every stream that's up.
    pub main: Option<(String, String)>,
    pub live: Vec<String>,
    /// Stop all is under way: nothing becomes the main stream meanwhile.
    pub stopping_all: bool,
    /// Every stream the flow has, up or still waking, and whether a start is waking or preparing one.
    pub ids: Vec<String>,
    /// The stream a start is waking.
    pub waking_id: Option<String>,
    pub waking: bool,
    pub preparing: bool,
    /// Which camera each stream that's up feeds, as other apps list it, and the size it opened at.
    pub cameras: Vec<CameraUse>,
    /// Phone streams that could move to another route now: up, not recovering, not moving already, and with a worker
    /// that can take it (or the camera off).
    pub can_hand_over: Vec<String>,
    /// The streams the panels don't show (Sessions.background), for host.background_streams.
    pub background: Vec<telescope_core::host::BackgroundStream>,
    /// The shown stream's URL, for host.packets_problem.
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CameraUse {
    pub source_id: String,
    pub slot: u32,
    pub label: String,
    pub size: Option<(u32, u32)>,
}

/// Posts waiting for the host thread, and how to wake it.
#[derive(Default)]
pub struct Posts {
    queue: std::sync::Mutex<Vec<(String, String, Value)>>,
    /// Events for one plugin from a thread of the host's own (a photo's progress), delivered after the posts.
    events: std::sync::Mutex<Vec<(String, Event)>>,
    waker: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

/// What's been posted: (owner, token, data) and (owner, event).
type Taken = (Vec<(String, String, Value)>, Vec<(String, Event)>);

impl Posts {
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        *self.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(waker));
    }

    pub fn push(&self, owner: &str, token: &str, data: Value) {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).push((owner.into(), token.into(), data));
        let waker = self.waker.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(w) = waker {
            w();
        }
    }

    /// An event for plugin `owner` alone, from any thread.
    pub fn push_event(&self, owner: &str, event: Event) {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).push((owner.into(), event));
        let waker = self.waker.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(w) = waker {
            w();
        }
    }

    fn take(&self) -> Taken {
        (std::mem::take(&mut *self.queue.lock().unwrap_or_else(|e| e.into_inner())), std::mem::take(&mut *self.events.lock().unwrap_or_else(|e| e.into_inner())))
    }
}

/// A plugin's handle for posting from its threads.
pub struct PluginPoster {
    pub owner: String,
    pub posts: Arc<Posts>,
}

impl telescope_core::host::Poster for PluginPoster {
    fn post(&self, token: &str, data: Value) {
        self.posts.push(&self.owner, token, data);
    }
}

/// The pictures from just before the colour step that haven't gone out yet, each with whether a step that hides things has had something to hide since.
type PendingBefore = Arc<std::sync::Mutex<BTreeMap<String, (telescope_core::Frame, bool)>>>;

/// Keeps a copy of each frame on its way into the colour step, for the preview's before and after (colour_split). The copy waits in `pending` and only reaches the preview once the frame is out (PublishBefore), so it isn't on show while the steps that hide things are still to run.
struct KeepBefore(PendingBefore);

impl telescope_core::frame::CpuStep for KeepBefore {
    fn touch(&self, _ctx: &dyn telescope_core::frame::FrameContext) -> telescope_core::frame::Touch {
        telescope_core::frame::Touch::Reads
    }

    fn process(&mut self, frame: &mut telescope_core::Frame, ctx: &dyn telescope_core::frame::FrameContext) {
        self.0.lock().unwrap().insert(ctx.source_id().to_string(), (frame.clone(), false));
    }
}

/// The last step of the frame: the picture kept before the colour step goes to the preview, unless a step that hides things had something to hide in this frame.
struct PublishBefore {
    pending: PendingBefore,
    before: Arc<std::sync::Mutex<BTreeMap<String, telescope_core::Frame>>>,
}

impl telescope_core::frame::CpuStep for PublishBefore {
    fn touch(&self, _ctx: &dyn telescope_core::frame::FrameContext) -> telescope_core::frame::Touch {
        telescope_core::frame::Touch::Reads
    }

    fn process(&mut self, _frame: &mut telescope_core::Frame, ctx: &dyn telescope_core::frame::FrameContext) {
        let kept = self.pending.lock().unwrap().remove(ctx.source_id());
        let mut before = self.before.lock().unwrap();
        match kept {
            Some((frame, false)) => {
                before.insert(ctx.source_id().to_string(), frame);
            }
            _ => {
                before.remove(ctx.source_id());
            }
        }
    }
}

/// A step that hides things (Privacy blur, the wait screen), watched while Colour grading compares. The picture kept from before the colour step is clear, so the moment this step has something to hide, or is skipped and sends a dark frame, the kept one goes too and the preview shows no compare side.
struct DropsBeforeWhenHiding {
    inner: Box<dyn telescope_core::frame::CpuStep>,
    before: Arc<std::sync::Mutex<BTreeMap<String, telescope_core::Frame>>>,
    pending: PendingBefore,
}

impl DropsBeforeWhenHiding {
    fn hiding(&self, ctx: &dyn telescope_core::frame::FrameContext) {
        self.before.lock().unwrap().remove(ctx.source_id());
        if let Some(kept) = self.pending.lock().unwrap().get_mut(ctx.source_id()) {
            kept.1 = true;
        }
    }
}

impl telescope_core::frame::CpuStep for DropsBeforeWhenHiding {
    fn hides(&self) -> bool {
        self.inner.hides()
    }

    fn touch(&self, ctx: &dyn telescope_core::frame::FrameContext) -> telescope_core::frame::Touch {
        let touch = self.inner.touch(ctx);
        if touch != telescope_core::frame::Touch::Nothing {
            self.hiding(ctx);
        }
        touch
    }

    fn withheld(&self, ctx: &dyn telescope_core::frame::FrameContext) {
        self.hiding(ctx);
        self.inner.withheld(ctx);
    }

    fn process(&mut self, frame: &mut telescope_core::Frame, ctx: &dyn telescope_core::frame::FrameContext) {
        self.hiding(ctx);
        self.inner.process(frame, ctx);
    }
}

/// Monotonic seconds, the view controller's clock.
pub fn now_s() -> f64 {
    telescope_core::time::monotonic_ns() as f64 / 1e9
}

pub struct State {
    pub slots: Vec<Slot>,
    pub panels: Panels,
    /// The custom card order the settings file holds. The running order only follows it from the next start.
    saved_layout: Option<CardLayout>,
    pub collapse: Collapsing,
    pub banners: Banners,
    /// The pins in the bottom bar and what plugins offer it (bar.rs).
    pub bar: crate::bar::Bar,
    pub file: ConfigFile,
    pub bus: EventBus,
    pub effects: Vec<Effect>,
    pub packet_makers: PacketMakers,
    /// The picked source (Connection's selected_device), whose device profile the panels show.
    pub selected: Option<String>,
    pub streams: Streams,
    pub view: View,
    /// Splits the view's zoom between the phone and this computer (view_control.rs).
    pub vc: crate::view_control::ViewController,
    /// Every plugin's colour change, baked into the one LUT the Colour stage runs.
    pub colour: ColourPipeline,
    /// While Colour grading compares: each stream's newest frame from just before the colour step, for the preview.
    pub colour_before: Arc<std::sync::Mutex<BTreeMap<String, telescope_core::Frame>>>,
    /// The pick under way on the preview, and the warp a plugin moving the picture told it about.
    pub overlays: crate::overlays::Overlays,
    /// What plugins put on the preview besides shapes (tint, analyse, paint), by owner.
    pub passes: Vec<(String, Arc<dyn telescope_core::preview::PreviewPass>)>,
    /// The shapes plugins draw on the preview (add_preview_layer), by owner.
    pub layers: BTreeMap<String, telescope_core::host::PreviewLayer>,
    pub(crate) preview_messages: crate::preview_message::Messages,
    pub(crate) photo_messages: BTreeMap<u64, crate::preview_message::Photo>,
    /// A pass asked for the preview to be drawn again (refresh_preview, or an analysis that finished).
    pub preview_dirty: Arc<std::sync::atomic::AtomicBool>,
    /// The stream flow's slot_memory: which virtual camera each source went to last.
    pub slot_memory: Rc<RefCell<std::collections::BTreeMap<String, u32>>>,
    /// Sources only remembered once their settings change (a browser).
    pub changes_only: Vec<String>,
    pub save_failure_notified: bool,
    /// A reset has written the settings file and saving is shut until the restart: the copy it set aside first (None: there was no file).
    pub(crate) reset_pending: Option<Option<std::path::PathBuf>>,
    /// Settings of phones that were switched away from while the file couldn't be written: saved again with the next save, and handed back if their phone comes back first.
    unsaved_profiles: Vec<PluginSection>,
    /// Phones whose settings were copied to a new id: the old entries go once a save has written the new pairing.
    forget_after_save: Vec<String>,
    /// Copies (old id, new id) that the file doesn't hold yet: each save retries them before it lets the old entries go.
    unsaved_copies: Vec<(String, String)>,
    /// Posts from plugins' threads (host.poster), delivered to their owner after the bus's events.
    pub posts: Arc<Posts>,
    /// Built-in plugins that are off this run (not the optional ones never switched on), by name.
    pub off: Vec<PluginInfo>,
    /// Every built-in plugin, on or off, for the Plugins tab.
    pub builtins: Vec<PluginInfo>,
    /// Added plugins that are on but didn't start: name to why.
    pub problems: BTreeMap<String, String>,
    /// The ones in problems that wait for a built-in plugin they need, which switching them again won't fix.
    pub blocked: std::collections::BTreeSet<String>,
    /// What plugins share with each other (Host::share).
    pub registry: telescope_core::host::Registry,
    /// Plugins that panicked where they couldn't be stopped on the spot (a read through &self): stopped when the hook that read them ends.
    late_stops: RefCell<Vec<(usize, String)>>,
    /// What a plugin that hides things panicking does (close_rather_than_show; tests put a recorder here).
    pub hiding_panic: fn(&str, &str),
    /// What the banner calls each of them.
    problem_titles: BTreeMap<String, String>,
    /// The added plugins that are running: they're not built in, so the Plugins tab lists them from their folder.
    pub added: Vec<PluginInfo>,
    /// The plugins folder and the switches, from the plugin host (the settings file's switches alone without one).
    pub admin: Arc<dyn telescope_core::plugin_admin::PluginAdmin>,
    pub keep_in_tray: bool,
    /// Link, for what the host has to answer from it straight away (hand_over_stream's prepared route).
    pub link: Option<Arc<dyn telescope_core::link::Link>>,
    /// The virtual cameras, asked whether the watch's "nobody reads" can be trusted (idle.rs).
    pub outputs: Option<Arc<dyn telescope_core::output::Outputs>>,
    /// Every running stream's newest frame, for plugins that use other streams' pictures (host.frames).
    pub frames: Option<Arc<dyn telescope_core::host::FrameStore>>,
    /// The same frames from the window's side, which is told how many past frames to keep (Host::want_frame_history).
    pub stream_frames: Option<Arc<dyn telescope_core::media::StreamFrames>>,
    /// How many frames of history each plugin asked for, by plugin.
    pub frame_history: BTreeMap<String, usize>,
    /// Person masks and people (HostVision), None without the vision crate.
    pub vision: Option<Arc<dyn telescope_core::vision::VisionService>>,
    /// How the workers turn frames upright for vision, kept up with the view.
    pub upright: Arc<std::sync::Mutex<telescope_core::vision::Upright>>,
    /// Motion in zones of the shown stream's picture, for the plugins that watch it; the workers feed it.
    pub motion: Arc<telescope_core::motion::Motion>,
    /// Which worker each stream's vision and motion state belongs to, so a slow-ending one leaves a replacement's alone.
    pub state_owners: Arc<crate::worker::StateOwners>,
    /// Each stream's frames before the view, as (width, height), from its worker (view_controller._frame_size).
    pub frame_sizes: Arc<std::sync::Mutex<std::collections::HashMap<String, (u32, u32)>>>,
    /// One of Telescope's windows is in front (keys.rs watches it).
    pub window_active: bool,
    /// The Microphone card's (enabled, muted), for the tray's tooltip.
    pub mic: (bool, bool),
    /// What the last camera_on_changed said, so picking another source only tells when it differs.
    pub camera_on_shown: bool,
    /// Whether the plugins last heard the window is on screen (WindowShown), and whether closing to the tray was said.
    pub shown: Option<bool>,
    /// Whether the preview is popped out, or a plugin has a window open (the composite editor draws from the streams' frames): places the picture is looked at.
    pub other_windows: bool,
    /// The owners that asked for frames with Host::want_frames.
    pub frame_wants: std::collections::BTreeSet<String>,
    /// Host::preview_follows by owner: (the stream the panels show, the stream whose output the preview draws instead).
    pub preview_follows: std::collections::BTreeMap<String, (String, String)>,
    /// Host::preview_composited by owner: the stream whose picture is made of several.
    pub preview_composited: std::collections::BTreeMap<String, String>,
    /// Whether the plugins were last told the preview draws the shown stream's own picture (Event::PreviewSourceChanged).
    pub preview_own_told: bool,
    /// Whether the main camera's frame steps can pause while nobody reads it (idle.rs), which the stream's worker looks at.
    pub idle: Arc<crate::idle::IdleGate>,
    /// The plugin that last said whether an app reads the camera (Event::CameraWatched): once it stops, nothing says, and the steps never pause.
    pub camera_watcher: Option<String>,
    /// The watcher stopped and no plugin has said anything since: a report it queued before stopping is dropped on its way in.
    pub watch_cut: bool,
    pub tray_close_notified: bool,
    /// Host::set_shortcut_menu: the plugin behind the controls' right-click menu, and the shortcuts it lists.
    pub shortcut_menu: Option<(String, Vec<telescope_core::ui::ShortcutEntry>)>,
    /// The photos being taken, by request: (plugin, the stream's id) for each, to say what became of them when they end.
    pub stills: Vec<(RequestId, String, String)>,
    /// Which plugin offered each stream source (HostSources), so a plugin that stops takes its sources with it: (source id, plugin).
    pub source_owners: Vec<(String, String)>,
    /// The owners whose frame steps hide the picture, for each stream (its id, or "" for the main one), as of the last time they were built.
    pub hiding_steps: BTreeMap<String, std::collections::BTreeSet<String>>,
    /// Host::set_hiding: by owner, whether it hides now, for each stream (its id, or "" for all of them).
    pub hiding: BTreeMap<String, BTreeMap<String, bool>>,
    /// What a photo's thread reads right before its request leaves, by stream: whether the picture is hidden, kept up with every change.
    pub hidden_flags: BTreeMap<String, Arc<std::sync::atomic::AtomicBool>>,
    /// The streams that take photos with a camera of their own (a DSLR), by their id: the photo service of each, as of the last snapshot.
    pub source_stills: Vec<(String, Arc<dyn telescope_core::source::SourceStills>)>,
    /// Photos' "still running" flags, by request and stream: cleared on cancellation or when that stream ends.
    pub still_alive: Vec<(RequestId, String, Arc<std::sync::atomic::AtomicBool>)>,
    /// Reads what's on the clipboard (the window pastes it into a hidden box); None before there is a window.
    pub clipboard: Option<Box<dyn Fn() -> Option<String>>>,
    next_request: u64,
    // Plugin hooks running right now, and whether one asked for save_now
    calls: u32,
    pub(crate) save_after_call: bool,
    /// Counts how often a plugin's settings moved, so Host::setting_revision can tell later edits from earlier ones.
    pub(crate) setting_clock: u64,
    /// The plugins that asked for a save during the hook running now, whose settings are looked at when the hook ends.
    pub(crate) saved_in_call: Vec<usize>,
}

impl State {
    pub fn new(file: ConfigFile) -> State {
        let admin = Arc::new(crate::admin::ShellAdmin::new(file.path()));
        let bus = EventBus::new();
        let sender = bus.sender();
        State {
            slots: Vec::new(),
            panels: Panels::default(),
            saved_layout: None,
            collapse: Collapsing::default(),
            banners: Banners::default(),
            bar: crate::bar::Bar::new(),
            file,
            bus,
            effects: Vec::new(),
            packet_makers: PacketMakers::default(),
            selected: None,
            streams: Streams::default(),
            view: View::default(),
            vc: crate::view_control::ViewController::new(),
            colour: {
                let mut colour = ColourPipeline::new();
                // Python's frames pick a new LUT up by themselves; here the steps are made again for it
                colour.set_on_baked(Arc::new(|| {
                    let _ = slint::invoke_from_event_loop(|| crate::window::with_window(|w| w.colour_baked()));
                }));
                colour
            },
            colour_before: Arc::default(),
            overlays: Default::default(),
            passes: Vec::new(),
            layers: BTreeMap::new(),
            preview_messages: Default::default(),
            photo_messages: BTreeMap::new(),
            preview_dirty: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            slot_memory: Rc::default(),
            changes_only: Vec::new(),
            save_failure_notified: false,
            reset_pending: None,
            unsaved_profiles: Vec::new(),
            forget_after_save: Vec::new(),
            unsaved_copies: Vec::new(),
            posts: Arc::default(),
            off: Vec::new(),
            builtins: Vec::new(),
            problems: BTreeMap::new(),
            blocked: Default::default(),
            registry: Default::default(),
            late_stops: RefCell::default(),
            hiding_panic: close_rather_than_show,
            problem_titles: BTreeMap::new(),
            added: Vec::new(),
            admin,
            frames: None,
            stream_frames: None,
            frame_history: BTreeMap::new(),
            vision: None,
            upright: Arc::default(),
            motion: Arc::new(telescope_core::motion::Motion::new(Box::new(move |key, owner, zones| {
                sender.emit(Event::MotionSeen { source_id: key.to_string(), owner: owner.to_string(), zones: zones.to_vec() });
            }))),
            frame_sizes: Arc::default(),
            state_owners: Arc::default(),
            keep_in_tray: false,
            link: None,
            outputs: None,
            window_active: false,
            mic: (false, false),
            camera_on_shown: true,
            shown: None,
            other_windows: false,
            frame_wants: Default::default(),
            preview_follows: Default::default(),
            preview_composited: Default::default(),
            preview_own_told: true,
            idle: Arc::default(),
            tray_close_notified: false,
            shortcut_menu: None, stills: Vec::new(), hiding_steps: BTreeMap::new(), hiding: BTreeMap::new(), hidden_flags: BTreeMap::new(), source_owners: Vec::new(), camera_watcher: None, watch_cut: false, source_stills: Vec::new(), still_alive: Vec::new(), clipboard: None, next_request: 1, calls: 0, save_after_call: false, setting_clock: 0, saved_in_call: Vec::new(),
        }
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.info.name == name)
    }

    /// Tells the streams how many past frames to keep: as many as the plugin that wants the most.
    pub fn sync_history(&self) {
        if let Some(frames) = &self.stream_frames {
            frames.set_history(self.frame_history.values().copied().max().unwrap_or(0));
        }
    }

    /// The stream whose output the preview draws while source is the one the panels show, when a plugin asked for another (Host::preview_follows).
    pub fn preview_target(&self, source: &str) -> Option<&str> {
        self.preview_follows.values().find(|(shown, _)| shown == source).map(|(_, target)| target.as_str())
    }

    /// Whether the preview draws the stream the panels show (shown) as its own camera picture: not while that stream's picture is made of several, nor
    /// while a plugin has the preview draw another stream that is running (one that has stopped leaves the preview on its own picture).
    pub fn preview_shows_own(&self, shown: &str) -> bool {
        let running = |id: &str| self.streams.waking_id.as_deref() == Some(id) || self.streams.ids.iter().any(|s| s == id);
        !self.preview_composited.values().any(|s| s == shown) && !self.preview_target(shown).is_some_and(running)
    }

    /// After the shown stream or what the preview draws changed: tells the plugins when it moved between a stream's own picture and another or
    /// a combined one, and ends a pick under way (its spots are of the other picture).
    pub fn sync_preview_source(&mut self) {
        let now_own = self.streams.source_id.clone().or_else(|| self.selected.clone()).is_none_or(|shown| self.preview_shows_own(&shown));
        if now_own != self.preview_own_told {
            self.preview_own_told = now_own;
            self.overlays.cancel_pick(None, &self.bus);
            self.bus.emit(Event::PreviewSourceChanged { shows_own: now_own });
        }
    }

    /// Tells the idle gate whether anything needs the main camera's frames: a window someone can see (the preview, the tiles, the cards that
    /// read frames, all painted from the stream's newest picture), the pop-out, a plugin that asked (the costly stats count as asking, under `stats:` and the owner, people finding under `people:`), or a motion watch.
    /// A window that hasn't said yet counts as seen.
    pub fn sync_idle(&self) {
        let wanted = self.shown != Some(false) || self.other_windows || !self.frame_wants.is_empty() || self.motion.watching();
        self.idle.wanted(wanted);
        if let Some(outputs) = &self.outputs {
            self.idle.reliable(outputs.watch_reliable());
        }
    }

    pub fn request_id(&mut self) -> RequestId {
        self.next_request += 1;
        RequestId(self.next_request)
    }

    /// Runs f with plugin i taken out and a host for it; a plugin that panics is logged and the others carry on, as
    /// what the window's call does with exceptions.
    pub fn call<R>(&mut self, i: usize, f: impl FnOnce(&mut dyn Plugin, &mut crate::host::Ctx) -> R) -> Option<R> {
        let slot = self.slots.get_mut(i)?;
        if slot.stopped.is_some() || slot.ended {
            return None;
        }
        let mut plugin = slot.plugin.take()?;
        let name = self.slots[i].info.name.clone();
        self.calls += 1;
        // What a plugin's hooks cost is only timed while the Stats panel wants it
        let hooks = telescope_core::stats::Hub::global();
        let began = hooks.hooks().on().then(telescope_core::time::monotonic_ns);
        let (result, name) = {
            let mut ctx = crate::host::Ctx { state: self, me: name };
            let result = telescope_core::diagnostics::catch_plugin(|| f(plugin.as_mut(), &mut ctx));
            (result, ctx.me)
        };
        if let Some(began) = began {
            let timer = self.slots[i].timer.get_or_insert_with(|| hooks.hooks().timer(&name));
            timer.add(telescope_core::time::monotonic_ns().saturating_sub(began));
        }
        self.slots[i].plugin = Some(plugin);
        // A plugin that panicked is stopped before anything reads its settings: what it holds now isn't saved over what was
        let result = match result {
            Ok(r) => Some(r),
            Err(panic) => {
                self.saved_in_call.retain(|s| *s != i);
                self.stop_after_panic(i, &panic_text(panic.as_ref()));
                None
            }
        };
        if self.saved_in_call.contains(&i) {
            self.saved_in_call.retain(|s| *s != i);
            self.note_config(i);
        }
        self.calls -= 1;
        if self.calls == 0 && std::mem::take(&mut self.save_after_call) && !self.save_now() {
            self.effects.push(Effect::SaveSoon); // a file that can't be read now isn't saved over, and is tried again
        }
        if self.calls == 0 {
            self.stop_late();
        }
        result
    }

    /// Plugin i's settings as it gives them now, read so a panic stops it instead of unwinding into whoever asked; None when it can't.
    fn read_config(&self, i: usize) -> Option<Value> {
        let slot = self.slots.get(i).filter(|s| s.stopped.is_none() && !self.stop_queued(i))?;
        let plugin = slot.plugin.as_ref()?;
        match telescope_core::diagnostics::catch_plugin(|| plugin.get_config()) {
            Ok(v) => Some(v),
            Err(panic) => {
                self.stop_later(i, format!("get_config: {}", panic_text(panic.as_ref())));
                None
            }
        }
    }

    /// What plugin i leaves for a preset, read like read_config.
    fn read_preset_config(&self, i: usize, picks: &[(String, String)]) -> Option<Value> {
        let slot = self.slots.get(i).filter(|s| s.stopped.is_none() && !self.stop_queued(i))?;
        let plugin = slot.plugin.as_ref()?;
        match telescope_core::diagnostics::catch_plugin(|| plugin.preset_config_with(picks)) {
            Ok(v) => Some(v),
            Err(panic) => {
                self.stop_later(i, format!("preset_config: {}", panic_text(panic.as_ref())));
                None
            }
        }
    }

    /// Plugin i panicked in a read and is about to be stopped: nothing reads or saves its settings from now on.
    fn stop_queued(&self, i: usize) -> bool {
        self.late_stops.borrow().iter().any(|(s, _)| *s == i)
    }

    /// Plugin i panicked in a read through &self: it's stopped once the hook that read it ends.
    pub fn stop_later(&self, i: usize, why: String) {
        self.late_stops.borrow_mut().push((i, why));
    }

    /// Stops the plugins that panicked in a read.
    pub fn stop_late(&mut self) {
        // Each stays queued (and its settings unread) until its slot is marked stopped: stopping one can read the others' settings
        loop {
            let Some((i, why)) = self.late_stops.borrow().first().cloned() else { return };
            self.stop_after_panic(i, &why);
            self.late_stops.borrow_mut().retain(|(s, _)| *s != i);
        }
    }

    /// A plugin that panicked is switched off until the next start, as its state can't be trusted any more: it's told to shut down, gives back what
    /// it holds, and its card and frame steps go. A required plugin carries on (Telescope can't work without it), as does one still setting up,
    /// which register() leaves out instead.
    pub fn stop_after_panic(&mut self, i: usize, why: &str) {
        let Some(slot) = self.slots.get_mut(i) else { return };
        let name = slot.info.name.clone();
        // One that hides things (or is there to, before its first step) can't be trusted to go on hiding them, nor can a picture without it
        if !slot.starting && (slot.hides || stage_hides(&slot.info, slot.info.frame_stage)) {
            slot.hides = true;
            (self.hiding_panic)(&name, why);
            return;
        }
        if slot.info.required || slot.starting || slot.stopped.is_some() {
            log::error!("Plugin {name} failed: {why}");
            return;
        }
        log::error!("Plugin {name} failed ({why}); switching it off until Telescope restarts");
        self.stop_slot(i, why);
    }

    /// Switches plugin i off until the next start, with its shutdown() called and everything it held let go.
    fn stop_slot(&mut self, i: usize, why: &str) {
        let name = self.slots[i].info.name.clone();
        let slot = &mut self.slots[i];
        slot.stopped = Some(why.to_string());
        let shut_down = std::mem::replace(&mut self.slots[i].shut_down, true);
        if let Some(mut plugin) = self.slots[i].plugin.take().filter(|_| !shut_down) {
            let mut ctx = crate::host::Ctx { state: self, me: name.clone() };
            if telescope_core::diagnostics::catch_plugin(|| plugin.shutdown(&mut ctx)).is_err() {
                log::error!("Plugin {name} failed again while shutting down");
            }
            ctx.release_everything();
            // An added plugin's script state (up to 64 MB of it) goes now. A built-in plugin stays, as what reads it after may expect it there
            if !self.slots[i].info.added {
                self.slots[i].plugin = Some(plugin);
            }
        }
        // Its banners would have buttons nobody hears
        self.banners.clear_owned_by(&name);
        self.drop_shared(&name);
        // Whatever it asked for in the hook that failed is dropped with it (a photo it started is still followed to its end, so Photos hears it finish)
        let mine: Vec<String> = self.source_owners.iter().filter(|(_, o)| *o == name).map(|(id, _)| id.clone()).collect();
        self.effects.retain(|e| e.plugin() != Some(name.as_str()) && !matches!(e, Effect::AddSource(s) if mine.contains(&s.0.id().to_string())));
        if self.shortcut_menu.as_ref().is_some_and(|(plugin, _)| *plugin == name) {
            self.shortcut_menu = None;
        }
        // Its sources go, and a stream from one stops
        for id in mine {
            self.effects.push(Effect::RemoveSource(id));
        }
        self.source_owners.retain(|(_, o)| *o != name);
        for action in self.slots[i].tray.clone() {
            self.effects.push(Effect::Tray(TrayAction { visible: false, ..action }));
        }
        self.effects.push(Effect::Stopped(name));
        self.show_stopped_banner();
        // A built-in going down takes the added plugins that need it along
        if !self.slots[i].info.added {
            self.stop_unmet_needs();
        }
    }

    /// Takes what owner shared out of the registry, telling everyone.
    fn drop_shared(&mut self, owner: &str) {
        for key in self.registry.drop_owner(owner) {
            self.bus.emit(Event::SharedChanged { key });
        }
    }

    /// Quit: each plugin's shutdown() in turn. What it shared goes after its hook (which may still read it), and the plugins that haven't
    /// shut down yet hear that; the ones that have don't.
    pub fn shut_down_plugins(&mut self) {
        self.slots.iter_mut().for_each(|s| s.shut_down = true);
        for i in 0..self.slots.len() {
            self.call(i, |p, host| p.shutdown(host));
            self.slots[i].ended = true;
            let owner = self.slots[i].info.name.clone();
            for key in self.registry.drop_owner(&owner) {
                let event = Event::SharedChanged { key };
                self.each(|p, host| p.on_event(host, &event));
            }
        }
    }

    /// The plugins switched off this run after panicking, by name, with what went wrong.
    pub fn stopped(&self) -> BTreeMap<String, String> {
        self.slots.iter().filter_map(|s| Some((s.info.name.clone(), s.stopped.clone()?))).collect()
    }

    fn show_stopped_banner(&mut self) {
        let stopped: Vec<&Slot> = self.slots.iter().filter(|s| s.stopped.is_some()).collect();
        let titles: Vec<String> = stopped.iter().map(|s| tr(if s.info.title.is_empty() { &s.info.name } else { &s.info.title })).collect();
        let details = stopped.iter().map(|s| format!("{}: {}", s.info.name, s.stopped.as_deref().unwrap_or_default())).collect::<Vec<_>>().join("\n");
        let n = titles.len();
        let title = if n < 4 {
            crate::texts::trn("{plugins} stopped working", "{plugins} stopped working", n).field("plugins", crate::texts::and_list(&titles))
        } else {
            crate::texts::trn("{n} plugin stopped working", "{n} plugins stopped working", n)
        };
        let text = crate::texts::trn("It ran into an error and is off until Telescope restarts. Everything else carries on.",
                                         "They ran into errors and are off until Telescope restarts. Everything else carries on.", n);

        self.banners.show("plugins_stopped", Issue {
            title, text, kind: "warn".into(), details, tell_dismiss: false,
            actions: vec![IssueAction { label: tr("Plugins…"), does: IssueDoes::OpenSettings { tab: "plugins".into() } }],
        });
    }

    /// Whose settings plugin i holds: a device-local plugin holds the shown camera's, so what moved is told apart per camera, and any other plugin has
    /// the one set.
    pub fn setting_scope(&self, i: usize) -> Option<String> {
        if self.slots.get(i).is_some_and(|s| s.info.device_local) {
            self.streams.source_id.clone().or_else(|| self.selected.clone())
        } else {
            None
        }
    }

    /// Looks at what plugin i's settings are now against what they were when last looked at: whatever moved gets a new revision. The first look
    /// only remembers where they are.
    pub fn note_config(&mut self, i: usize) {
        let now = self.read_config(i);
        self.stop_late();
        let Some(now) = now.filter(Value::is_object) else { return };
        let scope = self.setting_scope(i);
        let slot = &mut self.slots[i];
        let was = match slot.seen.iter_mut().find(|(s, _)| *s == scope) {
            Some((_, seen)) => Some(std::mem::replace(seen, now.clone())),
            None => {
                slot.seen.push((scope.clone(), now.clone()));
                None
            }
        };
        let Some(was) = was else { return };
        let changed = telescope_core::settings::leaf_changes(&was, &now);
        if changed.is_empty() {
            return;
        }
        self.setting_clock += 1;
        let clock = self.setting_clock;
        let slot = &mut self.slots[i];
        for (path, _, _) in changed {
            slot.moved.retain(|(s, p, _)| !(*s == scope && *p == path));
            slot.moved.push((scope.clone(), path, clock));
        }
    }

    /// Remembers where plugin i's settings are now, unless that is already known for the camera shown, so a change made next is told from what came before.
    pub fn note_config_baseline(&mut self, i: usize) {
        let scope = self.setting_scope(i);
        if self.slots.get(i).is_some_and(|s| !s.seen.iter().any(|(sc, _)| *sc == scope)) {
            self.rebaseline(i, scope);
        }
    }

    /// Takes plugin i's settings as they are as the start for `scope`, whatever was seen before: they were put there (a camera's profile coming in),
    /// not edited.
    fn rebaseline(&mut self, i: usize, scope: Option<String>) {
        let now = self.read_config(i);
        self.stop_late();
        let Some(now) = now.filter(Value::is_object) else { return };
        let seen = &mut self.slots[i].seen;
        seen.retain(|(s, _)| *s != scope);
        seen.push((scope, now));
    }

    /// The same hook on every plugin, in registration order.
    pub fn each(&mut self, mut f: impl FnMut(&mut dyn Plugin, &mut crate::host::Ctx)) {
        for i in 0..self.slots.len() {
            self.call(i, &mut f);
        }
    }

    /// register_plugin: sets it up and gives it its place. A plugin that panics while setting up is left out.
    pub fn register(&mut self, plugin: Box<dyn Plugin>) -> Result<(), String> {
        let info = plugin.info();
        if self.slots.iter().any(|s| s.info.name == info.name) {
            return Err(format!("{} is already running", info.name));
        }
        let i = self.slots.len();
        self.slots.push(Slot { plugin: Some(plugin), info: info.clone(), card: None, header: None, actions: vec![], tray: vec![], defaults: json!({}), timer: None, seen: Vec::new(), moved: Vec::new(), stopped: None, hides: false, shut_down: false, ended: false, starting: true });
        let made = self.call(i, |p, host| {
            p.setup(host);
            let card = p.card();
            let header = p.header_widget();
            let tray = p.tray_actions();
            let defaults = Some(p.get_config()).filter(Value::is_object).unwrap_or_else(|| json!({}));
            (card, header, tray, defaults)
        });
        self.slots[i].starting = false;
        let Some((card, header, tray, defaults)) = made else {
            self.drop_last_slot();
            return Err(format!("{} failed while starting", info.name));
        };
        if let Some(card) = &card {
            let region = Region::from_name(&card.region);
            self.panels.place_named(region, i, info.pinned, &info.category, &info.name);
        }
        let slot = &mut self.slots[i];
        (slot.card, slot.header, slot.tray, slot.defaults) = (card, header, tray, defaults);
        if let Some(actions) = self.call(i, |p, _| p.actions()) {
            self.slots[i].actions = actions;
        } else {
            log::error!("Plugin {} couldn't list its shortcut actions", info.name);
        }
        Ok(())
    }

    /// load_plugins for the built-in ones: all but those turned off in Plugins, never the required ones. One of those that
    /// breaks stops the start, unlike an added plugin, which is left out with a banner.
    pub fn load_builtins(&mut self, plugins: Vec<Box<dyn Plugin>>) -> Result<(), String> {
        let cfg = self.file.load();
        let mut broke = Vec::new();
        for plugin in plugins {
            let info = plugin.info();
            if info.added {
                // One that can't start is left out and says why (load_plugins)
                let blocked = self.missing_needs(&info);
                let is_blocked = blocked.is_some();
                let registered = blocked.map_or_else(|| self.register(plugin).and_then(|()| self.take_out_if_ui_broken()), Err);
                match registered {
                    Ok(()) => self.added.push(info),
                    Err(e) => broke.push(PluginProblem { title: if info.title.is_empty() { info.name.clone() } else { info.title.clone() }, name: info.name, error: e, blocked: is_blocked }),
                }
                continue;
            }
            self.builtins.push(info.clone());
            if switches::starts(&cfg, &info.name, info.required, info.optional) || (info.optional && crate::scenes::every_optional()) {
                self.register(plugin)?;
            } else if !info.optional {
                self.off.push(info);
            }
        }
        if !broke.is_empty() {
            let mut all: Vec<PluginProblem> = self.problems.iter()
                .map(|(name, error)| PluginProblem { name: name.clone(), title: self.problem_titles.get(name).cloned().unwrap_or_else(|| name.clone()), error: error.clone(), blocked: self.blocked.contains(name) }).collect();
            all.extend(broke);
            self.set_plugin_problems(&all);
        }
        crate::settings::start_plugin_updates(self.admin.clone());
        self.remind_plugins_off(&cfg);
        Ok(())
    }

    /// Why an added plugin can't start for the built-in plugins it needs (plugin.json "needs"), None when they're all running now: a built-in
    /// that panicked and was stopped doesn't count. Built-ins register before added plugins, so the slots hold every one that's on.
    fn missing_needs(&self, info: &PluginInfo) -> Option<String> {
        needs_reason(&self.builtins, &info.needs, |n| self.index(n).is_some_and(|i| self.slots[i].stopped.is_none()))
    }

    /// Stops the added plugins whose built-in plugins aren't all running any more, whether one stopped while the saved settings went in or
    /// later (stop_slot calls this when a built-in goes down).
    fn stop_unmet_needs(&mut self) {
        for i in 0..self.slots.len() {
            let slot = &self.slots[i];
            if !slot.info.added || slot.stopped.is_some() {
                continue;
            }
            if let Some(why) = self.missing_needs(&self.slots[i].info) {
                self.blocked.insert(self.slots[i].info.name.clone());
                log::error!("Plugin {} stopped: {why}", self.slots[i].info.name);
                // The same path a panic takes, so a dependant that hides things closes Telescope instead of going quietly
                self.stop_after_panic(i, &why);
            }
        }
    }

    /// An added plugin's card, header widget and settings sections (with their footers), each compiled inside the document it
    /// really goes into, wrapper and all: a piece that compiles on its own can still break the shared window (the main one, or
    /// Settings) by lacking what the wrapper sets. The plugin just registered (the last slot) is taken back out and says why.
    fn take_out_if_ui_broken(&mut self) -> Result<(), String> {
        let i = self.slots.len() - 1;
        let sections = self.call(i, |p, _| p.settings_sections()).unwrap_or_default();
        let slot = &self.slots[i];
        let name = &slot.info.name;
        let mut tried: Vec<(String, Result<(), String>)> = Vec::new();
        if let Some(card) = &slot.card {
            let (place, file) = crate::window::card_place(&slot.info, card, "p0");
            let mut panels = Panels::default();
            panels.place(place.region, 0, false, &place.category);
            let text = crate::doc::window(&[place], &panels, &[]);
            tried.push(("card".into(), crate::compile::compile("window.slint", &text, vec![file], "TelescopeWindow").map(drop)));
        }
        if let Some(header) = &slot.header {
            let (place, file) = crate::window::header_place(&slot.info, header, "h0");
            let text = crate::doc::window(&[], &Panels::default(), &[place]);
            tried.push(("header".into(), crate::compile::compile("window.slint", &text, vec![file], "TelescopeWindow").map(drop)));
        }
        for s in &sections {
            let (place, files) = crate::settings::section_place(name, s, 0, 0, &slot.info.category);
            let text = crate::doc::settings(1, None, None, &[place]);
            let ui = format!("settings {}{}", s.id, if s.footer.is_some() { " (or its footer)" } else { "" });
            tried.push((ui, crate::compile::compile("settings.slint", &text, files, "TelescopeSettings").map(drop)));
        }
        let broken = tried.into_iter().find_map(|(ui, result)| result.err().map(|e| format!("{ui}: {}", e.trim_end())));
        let Some(error) = broken else { return Ok(()) };
        log::error!("Plugin {name}: {error}");
        self.panels.remove(i);
        self.drop_last_slot();
        Err(error)
    }

    /// Takes the plugin in the last slot out, after giving back what its setup() took (the view, a preview layer, a timer, what it asked
    /// the window for): a plugin that was left out can't keep any of it until the next start.
    fn drop_last_slot(&mut self) {
        let Some(name) = self.slots.last().map(|s| s.info.name.clone()) else { return };
        crate::host::Ctx { state: self, me: name.clone() }.release_everything();
        self.drop_shared(&name);
        self.effects.retain(|e| e.plugin() != Some(name.as_str()));
        self.stills.retain(|(_, plugin, _)| *plugin != name);
        if self.shortcut_menu.as_ref().is_some_and(|(plugin, _)| *plugin == name) {
            self.shortcut_menu = None;
        }
        self.slots.pop();
    }

    /// Added plugins that are on but didn't start: remembered for the Plugins tab, with a warning banner saying which (load_plugins).
    pub fn set_plugin_problems(&mut self, problems: &[PluginProblem]) {
        self.problems = problems.iter().map(|p| (p.name.clone(), p.error.clone())).collect();
        self.blocked = problems.iter().filter(|p| p.blocked).map(|p| p.name.clone()).collect();
        self.problem_titles = problems.iter().map(|p| (p.name.clone(), p.title.clone())).collect();
        if problems.is_empty() {
            return;
        }
        let titles: Vec<String> = problems.iter().map(|p| tr(&p.title)).collect();
        let n = titles.len();
        let title = if n < 4 {
            crate::texts::trn("Couldn't start {plugins}", "Couldn't start {plugins}", n).field("plugins", crate::texts::and_list(&titles))
        } else {
            crate::texts::trn("Couldn't start {n} plugin", "Couldn't start {n} plugins", n)
        };
        let text = crate::texts::trn("Telescope works without the plugin. The Plugins tab in Settings shows what went wrong.",
                                     "Telescope works without them. The Plugins tab in Settings shows what went wrong.", n);
        self.banners.show("plugins", Issue {
            title, text, kind: "warn".into(), details: String::new(), tell_dismiss: false,
            actions: vec![IssueAction { label: tr("Plugins…"), does: IssueDoes::OpenSettings { tab: "plugins".into() } }],
        });
    }

    /// Once per change: which built-in plugins are off, so a card that's gone doesn't look like a bug.
    fn remind_plugins_off(&mut self, cfg: &Value) {
        let unnoticed = switches::unnoticed_off(cfg);
        let titles: Vec<String> = self.builtins.iter().filter(|p| unnoticed.contains(&p.name) && !p.required)
            .map(|p| tr(if p.title.is_empty() { &p.name } else { &p.title })).collect();
        if titles.is_empty() {
            return;
        }
        let n = titles.len();
        let list = crate::texts::and_list(&titles);
        let title = crate::texts::trn("{plugins} is turned off", "{plugins} are turned off", n).field("plugins", &list);
        let text = crate::texts::trn(
            "You turned this off in Plugins. To get it back, open Plugins in Settings and switch it on.",
            "You turned these off in Plugins. To get them back, open Plugins in Settings and switch them on.", n);
        self.banners.show("plugins_off", Issue {
            title, text, kind: "warn".into(), details: String::new(), tell_dismiss: true,
            actions: vec![IssueAction { label: tr("Plugins…"), does: IssueDoes::OpenSettings { tab: "plugins".into() } }],
        });
    }

    fn device_local(&self) -> Vec<String> {
        self.slots.iter().filter(|s| s.info.device_local && !s.info.name.is_empty()).map(|s| s.info.name.clone()).collect()
    }

    /// apply_saved_config: the saved settings into every plugin, then on_plugins_ready, then a banner if the file
    /// had to be set aside.
    pub fn apply_saved_config(&mut self) {
        let mut cfg = self.file.load_at_start();
        self.apply_config(&mut cfg);
        if let Some(layout) = self.saved_layout.clone() {
            self.panels.apply_layout(&layout);
        }
        self.stop_unmet_needs();
        self.each(|p, host| p.on_plugins_ready(host));
        if let Some(backup) = self.file.take_reset_notice() {
            let where_ = if backup.is_empty() { String::new() } else { format!(" {}", tr("A copy of the old file is at {path}.").field("path", &backup)) };
            self.banners.show("config", Issue {
                title: tr("Your settings couldn't be read"),
                text: tr("Telescope started with its defaults and paired phones need pairing again.") + &where_,
                actions: vec![], kind: "warn".into(), details: String::new(), tell_dismiss: false,
            });
        }
        // Windows says it in a message box once the window has shown; elsewhere nothing else would
        if let Some(card) = telescope_core::gpu_marker::take_crash_notice().filter(|_| !cfg!(windows)) {
            let (title, text) = telescope_core::gpu_marker::crash_text(&card);
            self.banners.show("graphics_card_skipped", Issue { title, text, actions: vec![], kind: "warn".into(), details: String::new(), tell_dismiss: false });
        }
    }

    /// The side cards as they are on screen now, by plugin name.
    pub fn running_card_layout(&self) -> CardLayout {
        self.panels.layout()
    }

    /// The order the next start will give: the saved one as it will land on the loaded cards, or the default.
    pub fn saved_card_layout(&self) -> CardLayout {
        self.panels.layout_for(self.saved_layout.as_ref())
    }

    /// The saved card order as the settings file holds it, with the names of plugins that aren't loaded too (None: the default order).
    pub fn stored_card_layout(&self) -> Option<&CardLayout> {
        self.saved_layout.as_ref()
    }

    /// Keep a card order for the next start (the running order doesn't change).
    pub fn set_saved_card_layout(&mut self, layout: CardLayout) {
        self.saved_layout = Some(layout);
        self.save_now();
    }

    /// Go back to the default order at the next start.
    pub fn clear_saved_card_layout(&mut self) {
        self.saved_layout = None;
        self.save_now();
    }

    /// Whether the next start will order the cards differently from now.
    pub fn card_layout_restart_pending(&self) -> bool {
        self.saved_card_layout() != self.running_card_layout()
    }

    fn apply_config(&mut self, cfg: &mut Value) {
        // Settings of a phone removed on an earlier run go before anything reads them (Connection's list says who is paired)
        let cleaned = profiles::drop_unpaired_phones(cfg);
        let window = WindowState::from_config(cfg);
        self.collapse.load(cfg.get("collapsed"));
        for key in self.collapse.keys() {
            self.bus.emit(Event::CardFolded { key, folded: true });
        }
        *self.slot_memory.borrow_mut() = window.output_slots.iter().filter_map(|(id, slot)| Some((id.clone(), u32::try_from(slot.as_u64()?).ok()?))).collect();
        self.selected = window.selected_device.clone();
        self.bar.load(window.bar_pins.clone());
        self.saved_layout = window.card_layout.clone();
        if let Some(ids) = self.bar.pins_changed() {
            self.bus.emit(Event::BarPinsChanged { pinned: ids });
        }
        if profiles::spread_shared_settings(cfg, &self.device_local()) || cleaned {
            self.file.save(cfg);
        }
        let global = cfg.get("plugin_configs").and_then(Value::as_object).cloned().unwrap_or_default();
        for i in 0..self.slots.len() {
            let (name, local, defaults) = (self.slots[i].info.name.clone(), self.slots[i].info.device_local, self.slots[i].defaults.clone());
            if name.is_empty() || local {
                continue;
            }
            if let Some(saved) = global.get(&name) {
                self.with_plugin(i, |p| load_config_tolerantly(p, &defaults, saved));
            }
        }
        self.apply_device_profile(window.selected_device.as_deref(), cfg);
        // What's picked is Connection's to say (conn.selected_device): the file's own field may be damaged or out of step with it
        if let Some(connection) = self.plugin_config("connection") {
            self.selected = connection.get("selected_phone").and_then(Value::as_str).map(str::to_string);
        }
    }

    fn with_plugin(&mut self, i: usize, f: impl FnOnce(&mut dyn Plugin)) {
        if self.slots[i].stopped.is_some() {
            return;
        }
        if let Some(p) = self.slots[i].plugin.as_mut() {
            if let Err(panic) = telescope_core::diagnostics::catch_plugin(|| f(p.as_mut())) {
                self.stop_after_panic(i, &panic_text(panic.as_ref()));
            }
        }
    }

    /// _apply_device_profile: each device-local plugin back to its defaults, then the device's saved settings.
    pub fn apply_device_profile(&mut self, id: Option<&str>, cfg: &Value) {
        let saved = profiles::device_plugin_configs(cfg, id);
        for i in 0..self.slots.len() {
            let (name, local, defaults) = (self.slots[i].info.name.clone(), self.slots[i].info.device_local, self.slots[i].defaults.clone());
            if name.is_empty() || !local {
                continue;
            }
            self.with_plugin(i, |p| {
                let _ = p.set_config(&defaults);
                if let Some(s) = saved.get(&name) {
                    load_config_tolerantly(p, &defaults, s);
                }
            });
            self.rebaseline(i, id.map(String::from));
            self.call(i, |p, host| p.on_profile_applied(host));
        }
    }

    /// _save_profile: the named plugins' device-local settings stored as id's.
    pub fn save_profile(&mut self, id: Option<&str>, names: &[String]) {
        let Some(id) = id.filter(|i| !i.is_empty()) else { return };
        let Ok(mut cfg) = self.file.load_strict() else { return };
        let saved = profiles::device_plugin_configs(&cfg, Some(id));
        let sections: Vec<PluginSection> = (0..self.slots.len())
            .filter(|&i| self.slots[i].info.device_local && names.contains(&self.slots[i].info.name))
            .filter_map(|i| {
                let name = &self.slots[i].info.name;
                self.config_of(i, saved.get(name)).map(|config| PluginSection { plugin: name.clone(), device: Some(id.into()), config })
            }).collect();
        let window = WindowState::from_config(&cfg);
        merge_for_save(&mut cfg, &window, &sections);
        profiles::drop_unchanged(&mut cfg, &self.changes_only, &self.defaults());
        self.file.save(&mut cfg);
    }

    /// _apply_device_profile for the named plugins only: their defaults, then id's saved settings.
    pub fn apply_profile(&mut self, id: Option<&str>, names: &[String]) {
        let cfg = self.file.load();
        let saved = profiles::device_plugin_configs(&cfg, id);
        for i in 0..self.slots.len() {
            let (name, local, defaults) = (self.slots[i].info.name.clone(), self.slots[i].info.device_local, self.slots[i].defaults.clone());
            if !local || !names.contains(&name) {
                continue;
            }
            self.with_plugin(i, |p| {
                let _ = p.set_config(&defaults);
                if let Some(s) = saved.get(&name) {
                    load_config_tolerantly(p, &defaults, s);
                }
            });
            self.rebaseline(i, id.map(String::from));
            self.call(i, |p, host| p.on_profile_applied(host));
        }
    }

    /// A stream hook on the named plugins, in registration order.
    pub fn call_hook(&mut self, names: &[String], hook: &crate::stream_flow::Hook) {
        use crate::stream_flow::Hook;
        if let Hook::FocusGained(info) = hook {
            // The panels are on this stream now: what's shown says so before the plugins hear of it, as in Python,
            // where on_focus_gained reads the live focus (streams.rs syncs the rest once the flow is through)
            self.streams.source_id = Some(info.source_id.clone());
            self.streams.url = Some(info.url.clone());
            self.sync_preview_source();
        }
        if names.iter().any(|n| n == "view") {
            let (vc, view, now) = (&mut self.vc, &self.view, now_s());
            let said = match hook {
                Hook::StreamStart(info) => vc.stream_start(view, &info.source_id, info.session, now),
                Hook::FocusGained(info) => vc.focus_gained(view, &info.source_id, info.session, now),
                Hook::StreamStop => vc.stream_stop(view, now),
                Hook::FocusLost => vc.focus_lost(view, now),
                _ => Vec::new(),
            };
            self.apply_said(said);
        }
        for i in 0..self.slots.len() {
            if !names.contains(&self.slots[i].info.name) {
                continue;
            }
            self.call(i, |p, host| match hook {
                Hook::StreamStartBegan => p.on_stream_start_began(host),
                Hook::StreamStarting => p.on_stream_starting(host),
                Hook::StreamStart(info) => p.on_stream_start(host, info),
                Hook::StreamStop => p.on_stream_stop(host),
                Hook::FocusGained(info) => p.on_focus_gained(host, info),
                Hook::FocusLost => p.on_focus_lost(host),
                Hook::CameraOff => p.on_camera_off(host),
                Hook::CameraOn => p.on_camera_on(host),
            });
        }
    }

    /// What the view controller asked for: events on the bus, the phone's zoom, a new view step.
    /// What a new worker hands the vision service.
    pub fn vision_feed(&self) -> Option<crate::worker::VisionFeed> {
        self.vision.clone().map(|service| crate::worker::VisionFeed { service, upright: self.upright.clone(), frame_sizes: self.frame_sizes.clone(), motion: Some(self.motion.clone()), owners: self.state_owners.clone(), owner: 0 })
    }

    /// The shown stream's frames before the view, (0, 0) before its first one.
    fn frame_size(&self) -> (u32, u32) {
        let id = self.streams.source_id.clone().or_else(|| self.selected.clone()).unwrap_or_default();
        self.frame_sizes.lock().unwrap().get(&id).copied().unwrap_or((0, 0))
    }

    /// Width over height of the shown stream's picture as shown, 0 before its first frame (view_controller.aspect).
    pub fn view_aspect(&self) -> f32 {
        let (w, h) = self.frame_size();
        if h == 0 {
            0.0
        } else if matches!(self.view.rotation, Some(90 | 270)) {
            h as f32 / w as f32
        } else {
            w as f32 / h as f32
        }
    }

    /// A point in the frame as shown to the phone's own frame (view_controller._in_phone_frame).
    fn in_phone_frame(&self, u: f32, v: f32) -> (f32, f32) {
        let (w, h) = self.frame_size();
        let geometry = telescope_core::view::Geometry::from(&self.vc.shown(&self.view));
        let (x, y) = telescope_core::view::inverse_map(f64::from(u), f64::from(v), f64::from(w), f64::from(h), &geometry);
        (x as f32, y as f32)
    }

    /// The view turned or the phone's share changed: frames for vision are turned to match, and tracked people move with it.
    fn follow_view_for_vision(&mut self) {
        use telescope_core::vision::Turn;
        let turn = Turn { flip_h: self.view.flip_h, flip_v: self.view.flip_v, rotation: self.view.rotation };
        let sent = telescope_core::view::phone_window(self.vc.sent().as_ref());
        let old = {
            let mut upright = self.upright.lock().unwrap();
            let old = upright.turn;
            *upright = telescope_core::vision::Upright { turn, sent, distortion: f64::from(self.view.distortion), aspect: 0.0 };
            old
        };
        if let Some(vision) = self.vision.as_ref().filter(|_| old != turn) {
            vision.view_turned(old, turn);
        }
        if old != turn {
            // The picture turned, so everything is somewhere else
            self.motion.reset(0.0, now_s());
            // The steps are made again after the turn, which is what tells each stream its pictures are of the turned view
            if !self.effects.contains(&Effect::FrameStep("view".into())) {
                self.effects.push(Effect::FrameStep("view".into()));
            }
        }
    }

    pub fn apply_said(&mut self, said: Vec<crate::view_control::Said>) {
        use crate::view_control::Said;
        for s in said {
            match s {
                Said::Emit(event) => {
                    if matches!(event, Event::ViewChanged { .. }) {
                        self.follow_view_for_vision();
                    }
                    self.bus.emit(event)
                }
                Said::Zoom(p) => self.effects.push(Effect::PhoneControl { source_id: None, body: crate::view_control::zoom_body(&p) }),
                Said::Restep => {
                    if !self.effects.contains(&Effect::FrameStep("view".into())) {
                        self.effects.push(Effect::FrameStep("view".into()));
                    }
                }
            }
        }
        // The phone's share of the zoom moves without a ViewChanged (its capabilities arriving, a stream starting): the mapping follows it too
        if self.upright.lock().unwrap().sent != telescope_core::view::phone_window(self.vc.sent().as_ref()) {
            self.follow_view_for_vision();
        }
    }

    /// Whether pixels of this stream are being hidden now: a plugin said so with set_hiding, or one has a step that hides in the steps last built
    /// for it and has never said anything. The steps are the same for every stream, so ones built for another (or for the main one) answer when
    /// this stream's haven't been built yet.
    pub fn picture_hidden_for(&self, source_id: &str) -> bool {
        let said = |per: &BTreeMap<String, bool>| per.get(source_id).or_else(|| per.get("")).copied();
        if self.hiding.values().any(|per| said(per) == Some(true)) {
            return true;
        }
        let steps = self.hiding_steps.get(source_id).or_else(|| self.hiding_steps.get(""));
        steps.is_some_and(|owners| owners.iter().any(|o| !self.hiding.get(o).is_some_and(|per| said(per).is_some())))
    }

    /// Host::set_hiding: recorded at once, so a photo about to go sees a cover asked for in the same batch. A device-local plugin holds the shown
    /// camera's settings, so a report without a stream is that camera's own and no other stream's: only a plugin that isn't device-local speaks
    /// for all of them (""), and the streams a device-local one hasn't reported on count as hiding if its steps do.
    pub fn set_hiding(&mut self, owner: &str, source_id: Option<&str>, on: bool) {
        let local = self.slots.iter().any(|s| s.info.device_local && s.info.name == owner);
        let key = match source_id {
            Some(id) => id.to_string(),
            None if local => self.streams.source_id.clone().or_else(|| self.selected.clone()).unwrap_or_default(),
            None => String::new(),
        };
        self.hiding.entry(owner.to_string()).or_default().insert(key, on);
        self.sync_hidden_flags();
    }

    /// The flag a photo on this stream reads from its own thread.
    pub fn hidden_flag(&mut self, source_id: &str) -> Arc<std::sync::atomic::AtomicBool> {
        self.sync_hidden_flags();
        let hidden = self.picture_hidden_for(source_id);
        self.hidden_flags.entry(source_id.to_string()).or_insert_with(|| Arc::new(std::sync::atomic::AtomicBool::new(hidden))).clone()
    }

    /// Every flag handed out says what picture_hidden_for says now. A flag no photo holds any more goes.
    pub(crate) fn sync_hidden_flags(&mut self) {
        self.hidden_flags.retain(|_, flag| Arc::strong_count(flag) > 1);
        for (id, flag) in &self.hidden_flags {
            flag.store(self.picture_hidden_for(id), std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Whether a photo asked for with `"skip_if_hidden": true` has to be left untaken because the stream's picture is hidden now.
    pub fn still_skipped(&self, options: &Value, source_id: &str) -> bool {
        options.get("skip_if_hidden").and_then(Value::as_bool) == Some(true) && self.picture_hidden_for(source_id)
    }

    /// Every plugin's frame step as it is now, in registration order (the pipeline sorts them by stage), with the colour
    /// stages source_id's stream gets.
    pub fn pipeline_steps(&mut self, source_id: Option<&str>) -> Vec<telescope_core::media::StagedStep> {
        use telescope_core::frame::FrameStage;
        use telescope_core::media::StagedStep;
        let mut steps = Vec::new();
        // The window's own steps: its view first, and the colour pipeline's LUT after the effects
        let (view, source_crop) = self.vc.frame_snapshot(&self.view);
        for step in telescope_core::view_step::steps_in(&view, source_crop) {
            steps.push(StagedStep { owner: "view".into(), stage: FrameStage::View, step });
        }
        let colour = self.colour.step(source_id);
        // The picture from before is kept only while there's a colour step to compare with
        if colour.is_none() || self.colour.split().is_none() {
            self.colour_before.lock().unwrap().clear();
        }
        // Nothing clear is kept for the compare while everything is withheld
        let comparing = colour.is_some() && self.colour.split().is_some();
        // A step that hides things, which isn't a CPU one, can't say when it does: no compare then
        let mut unwatched = false;
        let mut background = false;
        let pending: PendingBefore = Arc::default();
        let mut plugin_steps = Vec::new();
        let mut failed = Vec::new();
        for i in 0..self.slots.len() {
            let info = self.slots[i].info.clone();
            let (owner, stage) = (info.name.clone(), info.frame_stage);
            if self.slots[i].stopped.is_some() {
                continue;
            }
            let mut hid_any = false;
            let Some(p) = self.slots[i].plugin.as_mut() else { continue };
            match telescope_core::diagnostics::catch_plugin(|| p.frame_step()) {
                Ok(Some(step)) => {
                    let hides = matches!(stage, FrameStage::Privacy | FrameStage::Cover);
                    match step_hides(&info, stage, &step) {
                        Ok(h) => hid_any |= h,
                        Err(why) => {
                            failed.push((i, why));
                            continue;
                        }
                    }
                    // A picture kept before it was switched on went out with the room in it
                    background |= comparing && stage == FrameStage::Background;
                    let step = match step {
                        telescope_core::frame::FrameStep::Cpu(inner) if comparing && hides => {
                            telescope_core::frame::FrameStep::Cpu(Box::new(DropsBeforeWhenHiding { inner, before: self.colour_before.clone(), pending: pending.clone() }))
                        }
                        other => {
                            unwatched |= comparing && hides;
                            other
                        }
                    };
                    plugin_steps.push(telescope_core::media::StagedStep { owner: owner.clone(), stage, step });
                }
                Ok(None) => {}
                Err(panic) => {
                    failed.push((i, format!("frame_step: {}", panic_text(panic.as_ref()))));
                    continue;
                }
            }
            match telescope_core::diagnostics::catch_plugin(|| p.staged_frame_steps()) {
                Ok(more) => {
                    for (stage, step) in more {
                        match step_hides(&info, stage, &step) {
                            Ok(h) => hid_any |= h,
                            Err(why) => failed.push((i, why)),
                        }
                        plugin_steps.push(telescope_core::media::StagedStep { owner: owner.clone(), stage, step });
                    }
                }
                Err(panic) => failed.push((i, format!("staged_frame_steps: {}", panic_text(panic.as_ref())))),
            }
            self.slots[i].hides |= hid_any;
        }
        for (i, why) in failed {
            // A step that can't say whether it hides is taken to hide
            if why.starts_with("hides:") {
                self.slots[i].hides = true;
            }
            let owner = self.slots[i].info.name.clone();
            plugin_steps.retain(|s| s.owner != owner);
            self.stop_after_panic(i, &why);
        }
        if unwatched || background {
            self.colour_before.lock().unwrap().clear();
        }
        let hiders = plugin_steps.iter().filter(|st| {
            self.slots.iter().find(|s| s.info.name == st.owner).is_none_or(|s| step_hides(&s.info, st.stage, &st.step).unwrap_or(true))
        }).map(|st| st.owner.clone()).collect();
        self.hiding_steps.insert(source_id.unwrap_or("").to_string(), hiders);
        self.sync_hidden_flags();
        if let Some(step) = colour {
            if comparing && !unwatched {
                let keep = KeepBefore(pending.clone());
                steps.push(StagedStep { owner: "colour".into(), stage: FrameStage::Colour, step: telescope_core::frame::FrameStep::Cpu(Box::new(keep)) });
            }
            steps.push(StagedStep { owner: "colour".into(), stage: FrameStage::Colour, step });
        }
        steps.extend(plugin_steps);
        if comparing && !unwatched {
            let publish = PublishBefore { pending, before: self.colour_before.clone() };
            steps.push(StagedStep { owner: "colour".into(), stage: FrameStage::Final, step: telescope_core::frame::FrameStep::Cpu(Box::new(publish)) });
        }
        steps
    }

    /// _config_of: a plugin's settings, or what was saved before when it gives something the file can't hold.
    fn config_of(&self, i: usize, saved: Option<&Value>) -> Option<Value> {
        // One that panicked (or is about to be stopped for it) may hold anything: what was saved before stays
        if self.slots[i].stopped.is_some() || self.stop_queued(i) {
            return saved.cloned();
        }
        let p = self.slots[i].plugin.as_ref()?;
        match telescope_core::diagnostics::catch_plugin(|| p.get_config()) {
            Ok(v @ Value::Object(_)) => Some(v),
            Err(panic) => {
                self.stop_later(i, format!("get_config: {}", panic_text(panic.as_ref())));
                saved.cloned()
            }
            _ => {
                log::error!("Plugin {} gave settings that can't be saved; keeping the saved ones", self.slots[i].info.name);
                saved.cloned()
            }
        }
    }

    /// _profile_key: whose settings a plugin has now, the picked source's, or for one that doesn't follow the panels, the main
    /// stream's.
    fn profile_key(&self, i: usize, picked: Option<&str>) -> Option<String> {
        match &self.streams.main {
            Some((main, _)) if !self.slots[i].info.follows_focus => Some(main.clone()),
            _ => picked.filter(|p| !p.is_empty()).map(String::from),
        }
    }

    /// Every device-local plugin's settings as those of whoever they belong to now, with `picked` as the picked source.
    fn profile_sections(&self, cfg: &Value, picked: Option<&str>) -> Vec<PluginSection> {
        (0..self.slots.len()).filter(|&i| self.slots[i].info.device_local && !self.slots[i].info.name.is_empty())
            .filter_map(|i| {
                let id = self.profile_key(i, picked)?;
                let saved = profiles::device_plugin_configs(cfg, Some(&id));
                let name = &self.slots[i].info.name;
                self.config_of(i, saved.get(name)).map(|config| PluginSection { plugin: name.clone(), device: Some(id), config })
            }).collect()
    }

    /// save_now. False when the file couldn't be read (saving over it would lose every other phone's settings) or
    /// written; the window tries again shortly.
    pub fn save_now(&mut self) -> bool {
        self.stop_late();
        // The start couldn't read the file, so this app holds defaults: it takes in what's there instead of saving over it
        if let Some(read) = self.file.reread_after_unread_start() {
            return match read {
                Ok(mut cfg) => {
                    self.apply_config(&mut cfg);
                    // As at the start: Connection takes the restored selection as the one whose settings the panels hold
                    self.each(|p, host| p.on_plugins_ready(host));
                    true
                }
                Err(e) => {
                    log::error!("Couldn't read the settings file; not saving over it: {e}");
                    self.note_save_failure();
                    false
                }
            };
        }
        let Ok(mut cfg) = self.file.load_strict() else {
            log::error!("Couldn't read the settings file; not saving over it");
            self.note_save_failure();
            return false;
        };
        let global = cfg.get("plugin_configs").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut sections: Vec<PluginSection> = (0..self.slots.len())
            .filter(|&i| !self.slots[i].info.device_local && !self.slots[i].info.name.is_empty())
            .filter_map(|i| {
                let name = self.slots[i].info.name.clone();
                self.config_of(i, global.get(&name)).map(|config| PluginSection { plugin: name, device: None, config })
            }).collect();
        // A copy that didn't land with the move goes in first, so the old entries never go without it
        for (old, new) in &self.unsaved_copies {
            profiles::copy_device(&mut cfg, old, new);
        }
        sections.extend(self.unsaved_profiles.iter().cloned());
        sections.extend(self.profile_sections(&cfg, self.selected.as_deref()));
        let window = WindowState { selected_device: self.selected.clone(), collapsed: self.collapse.keys(), output_slots: self.slot_memory.borrow().iter().map(|(id, slot)| (id.clone(), json!(slot))).collect(), bar_pins: Some(self.bar.persisted()), card_layout: self.saved_layout.clone() };
        merge_for_save(&mut cfg, &window, &sections);
        profiles::drop_unchanged(&mut cfg, &self.changes_only, &self.defaults());
        if self.file.save(&mut cfg) {
            self.save_failure_notified = false;
            self.unsaved_profiles.clear();
            self.unsaved_copies.clear();
            for old in std::mem::take(&mut self.forget_after_save) {
                self.forget_device_settings(&old);
            }
            true
        } else {
            self.note_save_failure();
            false
        }
    }

    /// The notice that settings couldn't be written, once until a save works again.
    fn note_save_failure(&mut self) {
        if !self.save_failure_notified {
            self.save_failure_notified = true;
            log::error!("Failed to save settings");
            self.effects.push(Effect::Notify {
                title: tr("Telescope - Save failed"),
                body: tr("Could not save settings. Check disk space and permissions."),
                urgent: true,
            });
        }
    }

    fn defaults(&self) -> Map<String, Value> {
        self.slots.iter().filter(|s| !s.info.name.is_empty()).map(|s| (s.info.name.clone(), s.defaults.clone())).collect()
    }

    /// switch_device: the old device's settings saved, the new one's applied, and a running stream started again
    /// on the new one (by the stream flow, from the effects). Streams stop first: stopping saves what the panels show
    /// under the stream that had it, and the new phone's settings would be filed there.
    pub fn switch_device(&mut self, prev: Option<&str>, new: Option<&str>) {
        let loaded = self.file.load_strict();
        let sections = self.profile_sections(loaded.as_ref().unwrap_or(&json!({})), prev);
        let saved = loaded.is_ok_and(|mut cfg| {
            let window = WindowState::from_config(&cfg);
            merge_for_save(&mut cfg, &window, &sections);
            profiles::drop_unchanged(&mut cfg, &self.changes_only, &self.defaults());
            self.file.save(&mut cfg)
        });
        self.unsaved_profiles.retain(|u| !sections.iter().any(|s| (&s.plugin, &s.device) == (&u.plugin, &u.device)));
        if !saved && !sections.is_empty() {
            // The old phone's settings are about to be replaced by the new one's: keep them here until a save works
            self.unsaved_profiles.extend(sections);
            self.note_save_failure();
        }
        if self.streams.streaming || self.streams.starting {
            self.effects.push(Effect::StopAll);
            self.effects.push(Effect::FinishSwitch { new: new.map(String::from), start: true });
        } else {
            self.finish_switch(new, false);
        }
    }

    /// The rest of switch_device, with nothing streaming any more.
    pub fn finish_switch(&mut self, new: Option<&str>, start: bool) {
        if let Ok(mut cfg) = self.file.load_strict() {
            cfg["selected_device"] = new.map_or(Value::Null, |n| json!(n));
            self.file.save(&mut cfg);
        }
        self.selected = new.map(String::from);
        let mut cfg = self.file.load();
        // A phone that was switched away from while saving failed gets back what it had, not the older file's
        let (back, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.unsaved_profiles).into_iter().partition(|s| s.device.is_some() && s.device.as_deref() == new);
        self.unsaved_profiles = rest;
        if !back.is_empty() {
            let window = WindowState::from_config(&cfg);
            merge_for_save(&mut cfg, &window, &back);
        }
        self.apply_device_profile(new, &cfg);
        self.bus.emit(Event::DeviceChanged { source_id: new.unwrap_or("").to_string() });
        if start {
            self.effects.push(Effect::Start { interactive: true });
        }
    }

    pub fn forget_device_settings(&mut self, id: &str) {
        self.slot_memory.borrow_mut().remove(id);
        self.unsaved_profiles.retain(|s| s.device.as_deref() != Some(id));
        self.unsaved_copies.retain(|(_, new)| new != id);
        if let Ok(mut cfg) = self.file.load_strict() {
            if profiles::forget_device(&mut cfg, id) {
                self.file.save(&mut cfg);
            }
        }
    }

    /// Copies old's settings and camera under new. The old entries go after the next save, which writes the new pairing,
    /// so a file written at any point in between holds them under a phone it lists.
    pub fn move_device_settings(&mut self, old: &str, new: &str) {
        self.forget_after_save.push(old.to_string());
        if self.selected.as_deref() == Some(old) {
            self.selected = Some(new.to_string()); // else the next save files the pick under the id that is going
        }
        let mut memory = self.slot_memory.borrow_mut();
        if let Some(slot) = memory.get(old).copied() {
            memory.insert(new.to_string(), slot);
        }
        drop(memory);
        let copies: Vec<_> = self.unsaved_profiles.iter().filter(|s| s.device.as_deref() == Some(old)).cloned()
            .map(|mut s| { s.device = Some(new.to_string()); s }).collect();
        self.unsaved_profiles.extend(copies);
        let copied = self.file.load_strict().is_ok_and(|mut cfg| {
            profiles::copy_device(&mut cfg, old, new);
            self.file.save(&mut cfg)
        });
        if !copied {
            self.unsaved_copies.push((old.to_string(), new.to_string()));
        }
    }

    /// Hands every queued event to every plugin, in order; events emitted while delivering go after. Then each post
    /// from a plugin's threads goes to that plugin alone. Past DELIVERY_BATCH the rest waits for the next loop turn.
    pub fn deliver_events(&mut self) {
        self.stop_late();
        let mut budget = DELIVERY_BATCH;
        loop {
            while let Some(event) = (budget > 0).then(|| self.bus.next()).flatten() {
                budget -= 1;
                if let Event::MicChanged { enabled, muted } = event {
                    self.mic = (enabled, muted);
                    self.effects.push(Effect::MicChanged);
                }
                if let Event::CameraOnChanged { on } = event {
                    self.camera_on_shown = on;
                }
                if let Event::CameraWatched { watched } = event {
                    // What needs frames is told first, so a quiet that starts here isn't counted from before it was known
                    self.sync_idle();
                    if !self.watch_cut {
                        self.idle.watched(watched);
                    }
                }
                if let Event::SettingsTouched { ids, changed } = &event {
                    self.effects.push(Effect::TouchSettings { ids: ids.clone(), changed: *changed });
                }
                if matches!(event, Event::SourceSelected { .. }) {
                    self.effects.push(Effect::SourceSelected);
                }
                if let Event::ResolutionChangeRequested { width, height } = event {
                    self.effects.push(Effect::ResolutionPending { width, height });
                    self.effects.push(Effect::SettleStream);
                }
                if matches!(event, Event::StreamStopped | Event::DeviceChanged { .. }) {
                    // A warp stays: only its plugin ends it
                    self.overlays.cancel_pick(None, &self.bus);
                }
                if let Event::CameraSwitched { camera } = &event {
                    if let Some(source) = self.streams.source_id.as_deref() {
                        self.vc.gate().note_camera(source, camera);
                    }
                    self.effects.push(Effect::SettleStream);
                    let said = self.vc.camera_caps(&self.view, Some(camera), now_s());
                    self.apply_said(said);
                    // Another lens isn't motion
                    self.motion.reset(telescope_core::motion::LENS_SETTLE_S, now_s());
                    // Before the plugins hear it, so what the lens can take is fresh for them
                    self.note_lens(camera);
                }
                if let Event::StreamState { source_id, state } = &event {
                    if let Some(camera) = state.get("cameras").and_then(Value::as_array).and_then(|c| c.iter().find(|c| c.get("current") == Some(&Value::Bool(true)))) {
                        self.vc.gate().note_camera(source_id, camera);
                    }
                }
                if matches!(event, Event::StreamStopped) {
                    self.let_go_of_photos(None);
                }
                // A point picked in the frame as shown goes to the phone in its own frame (view_controller._on_point_picked)
                if let Event::FocusPointPicked { x, y } = event {
                    let (x, y) = self.in_phone_frame(x, y);
                    self.bus.emit(Event::FocusPoint { x, y });
                }
                if let Event::MeterPointPicked { x, y } = event {
                    let (x, y) = self.in_phone_frame(x, y);
                    self.bus.emit(Event::MeterPoint { x, y });
                }
                if let Event::VisionDownloadFinished { error, switch_on } = &event {
                    // the optional plugin that waited for the download goes on, with no dialog open (VisionDownload._done)
                    if error.is_empty() && !switch_on.is_empty() {
                        let saved = self.file.load_strict().and_then(|mut cfg| {
                            crate::switches::set_optional_on(&mut cfg, switch_on, true);
                            self.file.try_save(&mut cfg)
                        });
                        if let (Err(e), Some(vision)) = (saved, &self.vision) {
                            vision.download_failed(&format!("The download worked but the switch couldn't be saved: {e}"), switch_on);
                        }
                    }
                }
                self.each(|p, host| p.on_event(host, &event));
            }
            if budget == 0 && !self.bus.is_empty() {
                log::warn!("Plugins keep answering events with more events; delivering the rest after the window catches up");
                self.bus.wake();
                return;
            }
            let (posts, events) = self.posts.take();
            if posts.is_empty() && events.is_empty() {
                break;
            }
            for (owner, token, data) in posts {
                self.deliver_to(&owner, &Event::Posted { owner: owner.clone(), token, data });
            }
            for (owner, event) in events {
                self.photo_message_event(&event);
                self.photo_restore_event(&event);
                self.still_ended(&event);
                self.deliver_to(&owner, &event);
            }
        }
    }

    /// The lens a stream moved to, for the stills service (the one the panels show).
    fn note_lens(&mut self, camera: &Value) {
        let Some(stills) = self.link.as_ref().and_then(|l| l.stills()) else { return };
        let id = self.streams.source_id.clone().or_else(|| self.selected.clone());
        stills.note_lens(id.as_deref(), camera);
    }

    // Cleanup can finish after the requesting plugin has stopped or the request has timed out.
    fn photo_restore_event(&mut self, event: &Event) {
        let (Event::RequestDone { data, .. } | Event::RequestProgress { data, .. }) = event else { return };
        if data.get("restore").and_then(|r| r.get("ok")).and_then(Value::as_bool) != Some(false) {
            return;
        }
        let errors = data.get("restore").and_then(|r| r.get("errors")).and_then(Value::as_array)
            .map(|errors| errors.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")).unwrap_or_default();
        self.banners.show("photo_restore", Issue {
            title: tr("Couldn't restore the camera settings."),
            text: data.get("error").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)
                .unwrap_or_else(|| tr("Check the camera settings before taking another photo.")),
            details: errors, kind: "warn".into(), actions: Vec::new(), tell_dismiss: false,
        });
    }

    /// A photo's end: its stream is free for another, and every plugin hears that and what was saved.
    fn still_ended(&mut self, event: &Event) {
        let Event::RequestDone { request, data, .. } = event else { return };
        let Some(i) = self.stills.iter().position(|(r, ..)| r == request) else { return };
        let (_, _, source) = self.stills.remove(i);
        self.still_alive.retain(|(id, ..)| id != request);
        let busy = match self.source_stills.iter().find(|(id, _)| *id == source) {
            Some((_, camera)) => camera.busy(),
            None => self.link.as_ref().and_then(|l| l.stills()).is_some_and(|s| s.busy(Some(&source))),
        };
        self.bus.emit(Event::StillBusyChanged { source_id: source.clone(), busy });
        let files: Vec<String> = data.get("files").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
        if !files.is_empty() {
            self.bus.emit(Event::StillSaved { source_id: source, paths: files });
        }
    }

    /// Photos on `source` (all of them for None) end as interrupted: the stream they were taken from is gone.
    pub fn let_go_of_photos(&mut self, source: Option<&str>) {
        self.drop_stream_messages(source);
        self.still_alive.retain(|(_, id, alive)| {
            let gone = source.is_none_or(|s| s == id);
            if gone {
                alive.store(false, std::sync::atomic::Ordering::SeqCst);
            }
            !gone
        });
    }

    /// An event only plugin `owner` hears (a timer it scheduled, a post from its threads); dropped once it's gone.
    pub fn deliver_to(&mut self, owner: &str, event: &Event) {
        if let Some(i) = self.index(owner) {
            self.call(i, |p, host| p.on_event(host, event));
        }
    }

    /// A callback from one of plugin `name`'s pieces of UI.
    pub fn ui_event(&mut self, name: &str, event: UiEvent) {
        if name == "preview" && event.callback.starts_with("pick-") {
            self.pick_input(&event);
            self.deliver_events();
            return;
        }
        if let Some(i) = self.index(name) {
            self.call(i, |p, host| p.on_ui(host, &event));
            self.deliver_events();
        }
    }

    /// The mouse and keys on the preview card's stage while a pick runs: pick-pointer(kind, u, v, width, height) with
    /// the point in the frame as shown and the frame's size on screen, and pick-key(text).
    fn pick_input(&mut self, event: &UiEvent) {
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        let s = |i: usize| event.args.get(i).and_then(Value::as_str).unwrap_or("");
        let (view, o, bus) = (&self.view, &mut self.overlays, &self.bus);
        let (u, v, size) = (n(1), n(2), (n(3), n(4)));
        match (event.callback.as_str(), s(0)) {
            ("pick-pointer", "down") => {
                o.press(view, u, v, size);
            }
            ("pick-pointer", "move") => {
                o.move_to(view, u, v);
                o.cursor = Some(o.cursor_at(view, u, v, size).unwrap_or(crate::overlays::Cursor::Default));
            }
            ("pick-pointer", "up") => {
                o.release(view, bus);
            }
            ("pick-pointer", "right") => o.cancel_pick(None, bus),
            ("pick-key", key) => {
                o.key(view, key, bus);
            }
            _ => {}
        }
    }

    /// A plugin's current settings, if it's running.
    pub fn plugin_config(&self, name: &str) -> Option<Value> {
        let i = self.index(name)?;
        self.read_config(i).filter(Value::is_object)
    }

    pub fn plugin_preset_config(&self, name: &str) -> Option<Value> {
        self.plugin_preset_config_with(name, &[])
    }

    /// What a plugin leaves for a preset given the answers to its preset_options().
    pub fn plugin_preset_config_with(&self, name: &str, picks: &[(String, String)]) -> Option<Value> {
        let i = self.index(name)?;
        self.read_preset_config(i, picks).filter(Value::is_object)
    }

    /// The questions plugin `name` adds to the Save preset dialog.
    pub fn plugin_preset_options(&self, name: &str) -> Vec<telescope_core::PresetOption> {
        let Some(plugin) = self.index(name).and_then(|i| self.slots.get(i)).filter(|s| s.stopped.is_none()).and_then(|s| s.plugin.as_ref()) else { return Vec::new() };
        telescope_core::diagnostics::catch_plugin(|| plugin.preset_options()).unwrap_or_default()
    }

    /// The built-in plugins off this run, the way the Plugins tab and diagnostics count them.
    pub fn plugins_off(&self) -> &[PluginInfo] {
        &self.off
    }

    /// The plugins that have a card, in registration order (the index the window's p<n> ids use).
    pub fn cards(&self) -> Vec<usize> {
        (0..self.slots.len()).filter(|&i| self.slots[i].card.is_some()).collect()
    }

    /// Diagnostics' window lines (diagnostics_report's state before the plugins' own).
    pub fn diagnostics(&self) -> Vec<(String, String)> {
        self.diagnostics_with(&[])
    }

    /// The same when a plugin asks: own stands in for the plugin that's busy asking.
    pub fn diagnostics_with(&self, own: &[(String, String)]) -> Vec<(String, String)> {
        let mut lines = vec![("Streaming".to_string(),
                              if !self.streams.streaming { "no".into() } else if self.streams.camera_off { "yes, camera off".into() } else { "yes".into() })];
        if !self.off.is_empty() {
            lines.push(("Plugins off".into(), self.off.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")));
        }
        let mut added: Vec<String> = self.added.iter().map(|p| format!("{} (running)", p.name)).collect();
        added.extend(self.problems.keys().map(|name| format!("{name} (didn't start)")));
        if !added.is_empty() {
            lines.push(("Added plugins".into(), added.join(", ")));
        }
        let stopped = self.stopped();
        if !stopped.is_empty() {
            lines.push(("Plugins stopped".into(), stopped.iter().map(|(name, why)| format!("{name} ({why})")).collect::<Vec<_>>().join(", ")));
        }
        lines.extend(self.colour.diagnostics());
        for i in 0..self.slots.len() {
            if self.slots[i].stopped.is_some() {
                continue;
            }
            match self.slots[i].plugin.as_ref() {
                Some(p) => match telescope_core::diagnostics::catch_plugin(|| p.diagnostics()) {
                    Ok(more) => lines.extend(more),
                    Err(panic) => {
                        self.stop_later(i, format!("diagnostics: {}", panic_text(panic.as_ref())));
                        lines.push((self.slots[i].info.name.clone(), "couldn't read (panic)".into()));
                    }
                },
                None => lines.extend_from_slice(own),
            }
        }
        if let Some(vision) = &self.vision {
            lines.extend(vision.diagnostics());
        }
        if let Some(motion) = self.motion.diagnostics() {
            lines.push(("Motion".into(), motion));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use telescope_core::plugin::ConfigRejected;
    use telescope_core::testing::scratch_dir;
    use std::sync::Mutex;

    struct Stub {
        info: PluginInfo,
        config: Value,
        applied: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    }

    fn stub(name: &str, config: Value, local: bool) -> (Box<dyn Plugin>, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        let applied = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let info = PluginInfo { name: name.into(), device_local: local, ..Default::default() };
        (Box::new(Stub { info, config, applied: applied.clone() }), applied)
    }

    impl Plugin for Stub {
        fn info(&self) -> PluginInfo {
            self.info.clone()
        }
        fn get_config(&self) -> Value {
            self.config.clone()
        }
        // what a preset saves leaves out "content", as a plugin with saved layouts does
        fn preset_config(&self) -> Value {
            let mut cfg = self.config.clone();
            if let Some(map) = cfg.as_object_mut() {
                map.remove("content");
            }
            cfg
        }
        fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
            if cfg.get("zoom").is_some_and(|z| !z.is_number()) {
                return Err(ConfigRejected("zoom".into()));
            }
            self.applied.lock().unwrap().push(cfg.clone());
            let mut merged = self.config.as_object().cloned().unwrap_or_default();
            merged.extend(cfg.as_object().cloned().unwrap_or_default());
            self.config = Value::Object(merged);
            Ok(())
        }
    }

    fn card_in(region: &str) -> CardSpec {
        let ui = UiSpec { source: telescope_core::ui::SlintSource::Text(String::new()), component: "Card".into(), props: json!({}) };
        CardSpec { ui, region: region.into() }
    }

    fn state() -> State {
        let mut s = State::new(ConfigFile::at(scratch_dir("shell").join("telescope_config.json")));
        s.colour = ColourPipeline::inline();
        s
    }

    #[cfg(not(windows))]
    #[test]
    fn a_graphics_card_the_last_start_closed_on_is_said_in_a_banner() {
        use telescope_core::gpu_marker::{begin_start, mark, Card, MARKER_FILE};
        let folder = scratch_dir("shell-gpu-notice");
        begin_start(&folder);
        mark(&Card::new("Some card", "Vulkan", "1.2"));
        assert!(folder.join(MARKER_FILE).exists());
        // The next start finds the marker
        begin_start(&folder);
        let mut s = state();
        s.apply_saved_config();
        let issue = s.banners.issue("graphics_card_skipped").expect("a banner");
        assert!(issue.text.contains("Some card"), "{}", issue.text);
        // Said once
        s.banners.clear(Some("graphics_card_skipped"), None);
        s.apply_saved_config();
        assert!(s.banners.issue("graphics_card_skipped").is_none());
    }

    /// Takes the view and a timer in setup(), then offers a card that isn't Slint.
    struct Grabber;

    impl Plugin for Grabber {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: "grabber".into(), ..Default::default() }
        }
        fn setup(&mut self, host: &mut dyn telescope_core::host::Host) {
            assert!(host.take_view("grabber"));
            host.schedule(1000, "tick");
            host.add_preview_layer("grabber", telescope_core::host::PreviewLayer::default());
        }
        fn card(&mut self) -> Option<CardSpec> {
            let ui = UiSpec { source: telescope_core::ui::SlintSource::Text("this isn't { slint".into()), component: "Nope".into(), props: json!({}) };
            Some(CardSpec { ui, region: "left".into() })
        }
    }

    #[test]
    fn a_plugin_left_out_for_its_ui_gives_back_what_its_setup_took() {
        let mut s = state();
        s.register(Box::new(Grabber)).unwrap();
        assert!(!s.vc.take("other").0 && s.effects.iter().any(|e| e.plugin() == Some("grabber")) && s.layers.contains_key("grabber"));
        assert!(s.take_out_if_ui_broken().is_err());
        assert!(s.slots.is_empty());
        assert!(s.vc.take("other").0, "the view is free again");
        assert!(s.effects.iter().all(|e| e.plugin() != Some("grabber")) && !s.layers.contains_key("grabber"));
    }

    #[test]
    fn the_plugin_waiting_for_the_download_is_switched_on_with_no_dialog_open() {
        let mut s = state();
        s.bus.emit(Event::VisionDownloadFinished { error: String::new(), switch_on: "framing".into() });
        s.deliver_events();
        assert_eq!(crate::switches::optional_on(&s.file.load()).into_iter().collect::<Vec<_>>(), ["framing"]);
    }

    // Vision's host_tests has what it remembers
    #[test]
    fn a_failed_download_switches_nothing_on() {
        let mut s = state();
        s.bus.emit(Event::VisionDownloadFinished { error: "The download failed.".into(), switch_on: "framing".into() });
        s.deliver_events();
        assert!(crate::switches::optional_on(&s.file.load()).is_empty());
    }

    #[test]
    fn a_config_that_was_set_aside_shows_a_banner() {
        let mut s = state();
        std::fs::create_dir_all(s.file.path().parent().unwrap()).unwrap();
        std::fs::write(s.file.path(), "not valid json").unwrap();
        s.apply_saved_config();
        let issue = s.banners.issue("config").expect("a banner");
        assert!(issue.title.contains("couldn't be read") && issue.text.contains(".invalid-"), "{issue:?}");
    }

    #[test]
    fn a_readable_config_shows_no_banner() {
        let mut s = state();
        s.apply_saved_config();
        assert!(s.banners.issue("config").is_none());
    }

    #[test]
    fn apply_config_empty_is_noop() {
        let mut s = state();
        s.file.save(&mut json!({}));
        s.apply_saved_config();
        assert!(s.banners.all().is_empty());
    }

    #[test]
    fn once_every_plugin_is_in_each_hears_on_plugins_ready_after_its_settings() {
        struct Ready(&'static str, Arc<Mutex<Vec<String>>>);
        impl Plugin for Ready {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: self.0.into(), ..Default::default() }
            }
            fn set_config(&mut self, _cfg: &Value) -> Result<(), ConfigRejected> {
                self.1.lock().unwrap().push(format!("config {}", self.0));
                Ok(())
            }
            fn on_plugins_ready(&mut self, _host: &mut dyn telescope_core::host::Host) {
                self.1.lock().unwrap().push(format!("ready {}", self.0));
            }
        }
        let mut s = state();
        s.file.save(&mut json!({"plugin_configs": {"first": {"a": 1}, "second": {"b": 2}}}));
        let heard = Arc::new(Mutex::new(Vec::new()));
        s.register(Box::new(Ready("first", heard.clone()))).unwrap();
        s.register(Box::new(Ready("second", heard.clone()))).unwrap();
        heard.lock().unwrap().clear();
        s.apply_saved_config();
        assert_eq!(*heard.lock().unwrap(), ["config first", "config second", "ready first", "ready second"]);
    }

    #[cfg(unix)]
    fn locked(s: &State, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(s.file.path(), std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_preset_takes_what_the_plugin_leaves_for_it_where_settings_take_all_of_it() {
        use telescope_core::host::HostSettings;
        let mut s = state();
        s.register(stub("composite", json!({"on": true, "content": [1, 2]}), false).0).unwrap();
        assert_eq!(s.plugin_config("composite"), Some(json!({"on": true, "content": [1, 2]})));
        assert_eq!(s.plugin_preset_config("composite"), Some(json!({"on": true})));
        assert_eq!(crate::host::Ctx { state: &mut s, me: "presets".into() }.plugin_preset_config("composite"), Some(json!({"on": true})));
        assert_eq!(s.plugin_preset_config("nothing"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_settings_file_that_cant_be_read_is_not_saved_over() {
        let mut s = state();
        s.file.save(&mut json!({"devices": {"PhoneB": {"plugin_configs": {"transforms": {"zoom": 3}}}}}));
        s.register(stub("transforms", json!({"zoom": 2}), true).0).unwrap();
        s.register(stub("connection", json!({"selected_phone": "PhoneA"}), false).0).unwrap();
        s.apply_saved_config();
        locked(&s, 0o000);
        assert!(!s.save_now(), "the window tries again");
        locked(&s, 0o600);
        assert_eq!(s.file.load()["devices"]["PhoneB"]["plugin_configs"]["transforms"]["zoom"], 3);
    }

    fn stub_added(name: &str, title: &str, added: bool) -> Box<dyn Plugin> {
        let info = PluginInfo { name: name.into(), title: title.into(), added, ..Default::default() };
        Box::new(Stub { info, config: json!({}), applied: Default::default() })
    }

    #[test]
    fn an_added_plugins_notice_and_banner_always_say_whose_they_are() {
        use telescope_core::host::HostShell;
        let issue = || telescope_core::ui::Issue { title: "Update available".into(), text: String::new(), actions: vec![], kind: "info".into(), details: String::new(), tell_dismiss: false };
        let mut s = state();
        s.register(stub_added("fake", "Telescope", true)).unwrap();
        s.register(stub_added("own", "Own", false)).unwrap();
        let titles = |s: &State| s.effects.iter().filter_map(|e| if let Effect::Notify { title, .. } = e { Some(title.clone()) } else { None }).collect::<Vec<_>>();
        crate::host::Ctx { state: &mut s, me: "fake".into() }.send_notification("Update available", "Click here", true);
        crate::host::Ctx { state: &mut s, me: "own".into() }.send_notification("Update available", "Click here", true);
        assert_eq!(titles(&s), ["Plugin “Telescope”: Update available", "Update available"]);
        // Even a key that isn't prefixed (a plugin that doesn't go through the guest door) is put under the plugin's name
        crate::host::Ctx { state: &mut s, me: "fake".into() }.show_issue("update", issue());
        crate::host::Ctx { state: &mut s, me: "own".into() }.show_issue("update", issue());
        let keys = s.banners.keys();
        assert_eq!(keys.iter().filter_map(|k| telescope_core::ui::issue_owner(k).map(|(o, _)| o)).collect::<Vec<_>>(), ["fake"]);
        assert!(keys.iter().any(|k| k == "update"), "a built-in's key stays as it is");
    }

    #[cfg(unix)]
    #[test]
    fn a_settings_file_that_cant_be_read_says_so_once_like_one_that_cant_be_written() {
        let notes = |s: &State| s.effects.iter().filter(|e| matches!(e, Effect::Notify { title, .. } if title == "Telescope - Save failed")).count();
        let mut s = state();
        s.file.save(&mut json!({}));
        s.register(stub("transforms", json!({"zoom": 2}), true).0).unwrap();
        s.apply_saved_config();
        locked(&s, 0o000);
        assert!(!s.save_now());
        assert!(!s.save_now());
        assert_eq!(notes(&s), 1, "the change that waits is not lost without a word");
        locked(&s, 0o600);
        assert!(s.save_now());
        // A start that couldn't read it either: the second read says so too
        let mut s = state();
        s.file.save(&mut json!({}));
        s.register(stub("transforms", json!({"zoom": 2}), true).0).unwrap();
        locked(&s, 0o000);
        s.apply_saved_config();
        assert!(!s.save_now());
        assert_eq!(notes(&s), 1);
        locked(&s, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn taking_in_a_file_that_was_unreadable_at_start_tells_plugins_they_are_ready_again() {
        struct Ready(Arc<Mutex<Vec<&'static str>>>);
        impl Plugin for Ready {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "watcher".into(), ..Default::default() }
            }
            fn set_config(&mut self, _cfg: &Value) -> Result<(), ConfigRejected> {
                self.0.lock().unwrap().push("config");
                Ok(())
            }
            fn on_plugins_ready(&mut self, _host: &mut dyn telescope_core::host::Host) {
                self.0.lock().unwrap().push("ready");
            }
        }
        let mut s = state();
        s.file.save(&mut json!({"plugin_configs": {"watcher": {"a": 1}}}));
        let heard = Arc::new(Mutex::new(Vec::new()));
        s.register(Box::new(Ready(heard.clone()))).unwrap();
        locked(&s, 0o000);
        s.apply_saved_config();
        locked(&s, 0o600);
        heard.lock().unwrap().clear();
        assert!(s.save_now());
        assert_eq!(*heard.lock().unwrap(), ["config", "ready"]);
    }

    #[test]
    fn save_config_failure_notifies_once_until_a_save_succeeds() {
        let mut s = state();
        s.file.save(&mut json!({}));
        let notes = |s: &State| s.effects.iter().filter(|e| matches!(e, Effect::Notify { title, .. } if title == "Telescope - Save failed")).count();
        // A folder where the save writes its copy first
        let in_the_way = PathBuf::from(format!("{}.tmp", s.file.path().display()));
        std::fs::create_dir(&in_the_way).unwrap();
        assert!(!s.save_now());
        assert!(!s.save_now());
        assert_eq!(notes(&s), 1);
        std::fs::remove_dir(&in_the_way).unwrap();
        assert!(s.save_now());
        std::fs::create_dir(&in_the_way).unwrap();
        assert!(!s.save_now());
        assert_eq!(notes(&s), 2);
    }

    #[test]
    fn a_plugin_with_settings_that_cant_be_saved_keeps_its_old_ones_and_the_rest_save() {
        struct Odd(&'static str, Arc<Mutex<Value>>);
        impl Plugin for Odd {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: self.0.into(), ..Default::default() }
            }
            fn get_config(&self) -> Value {
                self.1.lock().unwrap().clone()
            }
        }
        let mut s = state();
        s.file.save(&mut json!({}));
        let (odd, other) = (Arc::new(Mutex::new(json!({"level": 1}))), Arc::new(Mutex::new(json!({"x": 1}))));
        s.register(Box::new(Odd("odd", odd.clone()))).unwrap();
        s.register(Box::new(Odd("other", other.clone()))).unwrap();
        assert!(s.save_now());
        for (x, bad) in [json!(["not", "a", "dict"]), json!(7), Value::Null].into_iter().enumerate() {
            *odd.lock().unwrap() = bad;
            *other.lock().unwrap() = json!({"x": x + 2});
            assert!(s.save_now());
            assert_eq!(s.file.load()["plugin_configs"]["odd"], json!({"level": 1}));
            assert_eq!(s.file.load()["plugin_configs"]["other"], json!({"x": x + 2}));
        }
        assert!(!s.effects.iter().any(|e| matches!(e, Effect::Notify { .. })));
    }

    #[test]
    fn register_plugin_initializes_panel_and_captures_device_defaults() {
        struct Sets(Arc<Mutex<Vec<String>>>);
        impl Plugin for Sets {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "transforms".into(), device_local: true, ..Default::default() }
            }
            fn setup(&mut self, host: &mut dyn telescope_core::Host) {
                self.0.lock().unwrap().push(format!("setup, the host knows {}", host.plugin_config("other").is_some()));
                host.emit(Event::StreamStopped);
            }
            fn card(&mut self) -> Option<CardSpec> {
                Some(card_in("left"))
            }
            fn get_config(&self) -> Value {
                json!({"zoom": 1})
            }
        }
        let mut s = state();
        let heard = Arc::new(Mutex::new(Vec::new()));
        s.register(Box::new(Sets(heard.clone()))).unwrap();
        assert_eq!(*heard.lock().unwrap(), ["setup, the host knows false"]);
        assert!(matches!(s.bus.next(), Some(Event::StreamStopped)), "setup had the window's own bus");
        assert_eq!(s.slots.iter().map(|slot| slot.info.name.as_str()).collect::<Vec<_>>(), ["transforms"]);
        assert_eq!(s.slots[0].defaults, json!({"zoom": 1}));
        assert_eq!(s.panels.left, [0]);
    }

    #[test]
    fn register_plugin_routes_panel_to_its_declared_region() {
        struct Placed(&'static str);
        impl Plugin for Placed {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "regional".into(), ..Default::default() }
            }
            fn card(&mut self) -> Option<CardSpec> {
                Some(card_in(self.0))
            }
        }
        for (region, list) in [("left", 0), ("center", 1), ("right", 2)] {
            let mut s = state();
            s.register(Box::new(Placed(region))).unwrap();
            let mut want: [Vec<usize>; 3] = Default::default();
            want[list] = vec![0];
            assert_eq!([s.panels.left.clone(), s.panels.center.clone(), s.panels.right.clone()], want, "{region}");
        }
    }

    #[test]
    fn register_headless_global_plugin_does_not_add_panel() {
        let mut s = state();
        s.register(stub("global", json!({}), false).0).unwrap();
        assert!(s.panels.left.is_empty() && s.panels.center.is_empty() && s.panels.right.is_empty());
    }

    #[test]
    fn a_pick_on_the_preview_ends_when_the_panels_move_to_another_phone() {
        use telescope_core::host::PickKind;
        let mut s = state();
        let heard = Arc::new(Mutex::new(Vec::new()));
        s.register(Box::new(Watches(heard.clone()))).unwrap();
        s.overlays.start_pick(&s.view, "watcher", PickKind::Quad, None, "", &s.bus);
        assert_eq!(s.overlays.picking(), "watcher");
        s.bus.emit(Event::DeviceChanged { source_id: "phone-2".into() });
        s.deliver_events();
        assert_eq!(s.overlays.picking(), "");
        assert!(heard.lock().unwrap().iter().any(|e| matches!(e, Event::PreviewPicked { owner, pick: None } if owner == "watcher")));
    }

    #[test]
    fn stream_camera_changes_update_only_their_mask_source_geometry() {
        let mut s = state();
        s.streams.source_id = Some("a".into());
        s.bus.emit(Event::StreamState { source_id: "a".into(), state: json!({"cameras": [{"id": "main", "current": true}]}) });
        s.bus.emit(Event::StreamState { source_id: "b".into(), state: json!({"cameras": [{"id": "main", "current": true}]}) });
        s.deliver_events();
        let gate = s.vc.gate();
        let first = gate.source_epoch("a");
        s.bus.emit(Event::CameraSwitched { camera: json!({"id": "wide"}) });
        s.deliver_events();
        assert_ne!(gate.source_epoch("a"), first);
        assert_eq!(gate.source_epoch("b"), first);
    }

    #[test]
    fn a_lens_switch_starts_motion_over() {
        let mut s = state();
        let before = now_s();
        s.bus.emit(Event::CameraSwitched { camera: json!({"id": "2"}) });
        s.deliver_events();
        let quiet = s.motion.quiet_until() - before;
        let settle = telescope_core::motion::LENS_SETTLE_S;
        assert!((settle..settle + 1.0).contains(&quiet), "{quiet}");
    }

    #[test]
    fn a_panel_for_a_region_nobody_knows_goes_on_the_left_rail() {
        struct Lost;
        impl Plugin for Lost {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "regional".into(), ..Default::default() }
            }
            fn card(&mut self) -> Option<CardSpec> {
                Some(card_in("nowhere"))
            }
        }
        let mut s = state();
        s.register(Box::new(Lost)).unwrap();
        assert_eq!(s.panels.left, [0]);
    }

    #[test]
    fn the_canvas_comes_from_setups_own_settings() {
        use telescope_core::host::HostStreams;
        let mut s = state();
        s.register(stub("setup", json!({"canvas_preset": "720p 16:9 - 1280 x 720", "custom_canvas_w": 900, "custom_canvas_h": 600}), false).0).unwrap();
        s.register(stub("wait_screen", json!({}), false).0).unwrap();
        assert_eq!(s.call(1, |_, host| host.canvas_size()), Some((Some(1280), Some(720))));
        let mut s = state();
        s.register(stub("setup", json!({"canvas_preset": "Auto (from first frame)"}), false).0).unwrap();
        s.register(stub("wait_screen", json!({}), false).0).unwrap();
        assert_eq!(s.call(1, |_, host| host.canvas_size()), Some((None, None)));
    }

    // The damaged-sections fixture: a selected_device that isn't text, with Connection knowing what's picked
    #[test]
    fn whats_picked_is_connections_to_say() {
        let mut s = state();
        s.file.save(&mut json!({"selected_device": 5}));
        s.register(stub("connection", json!({"selected_phone": "PhoneB"}), false).0).unwrap();
        s.apply_saved_config();
        assert_eq!(s.selected.as_deref(), Some("PhoneB"));
        assert!(s.save_now());
        assert_eq!(s.file.load()["selected_device"], "PhoneB");
    }

    #[test]
    fn save_config_separates_global_and_device_local_plugins() {
        let mut s = state();
        s.file.save(&mut json!({"selected_device": "PhoneA"}));
        s.register(stub("connection", json!({"selected_phone": "PhoneA", "mode": "wifi"}), false).0).unwrap();
        s.register(stub("setup", json!({"canvas": "auto"}), false).0).unwrap();
        s.register(stub("transforms", json!({"zoom": 2}), true).0).unwrap();
        s.apply_saved_config();
        assert!(s.save_now());
        let cfg = s.file.load();
        assert_eq!(cfg["selected_device"], "PhoneA");
        assert_eq!(cfg["plugin_configs"], json!({"connection": {"selected_phone": "PhoneA", "mode": "wifi"}, "setup": {"canvas": "auto"}}));
        assert_eq!(cfg["devices"]["PhoneA"]["plugin_configs"], json!({"transforms": {"zoom": 2}}));
    }

    #[test]
    fn apply_config_routes_global_and_selected_device_config() {
        let mut s = state();
        s.register(stub("connection", json!({"mode": "wifi"}), false).0).unwrap();
        s.register(stub("setup", json!({"old": true}), false).0).unwrap();
        let (local, applied) = stub("transforms", json!({"zoom": 1}), true);
        s.register(local).unwrap();
        let (other, other_applied) = stub("monitoring", json!({"zoom": 1}), false);
        s.register(other).unwrap();
        s.apply_config(&mut json!({
            "selected_device": "PhoneB",
            "plugin_configs": {"connection": {"mode": "usb"}, "setup": {"canvas": "4k"}},
            "devices": {"PhoneA": {"plugin_configs": {"transforms": {"zoom": 5}}}, "PhoneB": {"plugin_configs": {"transforms": {"zoom": 3}}}},
        }));
        assert_eq!(s.plugin_config("connection").unwrap()["mode"], "usb");
        assert_eq!(s.plugin_config("setup").unwrap()["canvas"], "4k");
        assert_eq!(*applied.lock().unwrap(), [json!({"zoom": 1}), json!({"zoom": 3})], "defaults, then the picked phone's, nothing from the global section");
        assert!(other_applied.lock().unwrap().is_empty(), "no section, nothing to load");
    }

    #[test]
    fn save_config_without_a_picked_device_skips_device_profiles() {
        let mut s = state();
        s.register(stub("transforms", json!({"zoom": 2}), true).0).unwrap();
        s.save_now();
        assert_eq!(s.file.load()["devices"], json!({}));
    }

    #[test]
    fn settings_of_a_phone_that_took_a_new_id_stay_under_a_listed_phone_until_the_new_pairing_is_saved() {
        let mut s = state();
        s.register(stub("connection", json!({"phones": [{"id": "old"}]}), false).0).unwrap();
        s.register(stub("transforms", json!({"zoom": 1}), true).0).unwrap();
        s.file.save(&mut json!({"plugin_configs": {"connection": {"phones": [{"id": "old"}]}},
                                "devices": {"old": {"plugin_configs": {"transforms": {"zoom": 4}}}}, "output_slots": {"old": 2}}));
        s.apply_saved_config();
        s.move_device_settings("old", "new");
        // A start now would drop what isn't listed: "new" isn't yet, "old" still is and holds everything
        let mut gap = s.file.load();
        profiles::drop_unpaired_phones(&mut gap);
        assert_eq!(gap["devices"]["old"]["plugin_configs"]["transforms"]["zoom"], 4);
        assert_eq!(gap["output_slots"]["old"], 2);
        s.with_plugin(0, |p| p.set_config(&json!({"phones": [{"id": "new"}]})).unwrap());
        assert!(s.save_now());
        let saved = s.file.load();
        assert_eq!(saved["devices"]["new"]["plugin_configs"]["transforms"]["zoom"], 4);
        assert_eq!(saved["output_slots"], json!({"new": 2}));
        assert!(saved["devices"].get("old").is_none());
    }

    #[test]
    fn a_copy_that_could_not_be_saved_is_written_with_the_new_pairing_before_the_old_settings_go() {
        let mut s = state();
        s.register(stub("connection", json!({"phones": [{"id": "old"}]}), false).0).unwrap();
        s.register(stub("transforms", json!({"zoom": 1}), true).0).unwrap();
        s.file.save(&mut json!({"plugin_configs": {"connection": {"phones": [{"id": "old"}]}},
                                "devices": {"old": {"plugin_configs": {"transforms": {"zoom": 4}}}}, "output_slots": {"old": 2}}));
        s.apply_saved_config();
        // A folder where the temporary file goes makes the copy's save fail
        let blocker = s.file.path().with_file_name("telescope_config.json.tmp");
        std::fs::create_dir(&blocker).unwrap();
        s.move_device_settings("old", "new");
        assert!(s.file.load()["devices"].get("new").is_none(), "the copy didn't land");
        std::fs::remove_dir(&blocker).unwrap();
        s.with_plugin(0, |p| p.set_config(&json!({"phones": [{"id": "new"}]})).unwrap());
        assert!(s.save_now());
        let saved = s.file.load();
        assert_eq!(saved["devices"]["new"]["plugin_configs"]["transforms"]["zoom"], 4, "{saved}");
        assert_eq!(saved["output_slots"], json!({"new": 2}));
        assert!(saved["devices"].get("old").is_none());
    }

    #[test]
    fn old_settings_stay_when_neither_the_copy_nor_the_pairing_could_be_saved() {
        let mut s = state();
        s.register(stub("connection", json!({"phones": [{"id": "old"}]}), false).0).unwrap();
        s.register(stub("transforms", json!({"zoom": 1}), true).0).unwrap();
        s.file.save(&mut json!({"plugin_configs": {"connection": {"phones": [{"id": "old"}]}},
                                "devices": {"old": {"plugin_configs": {"transforms": {"zoom": 4}}}}}));
        s.apply_saved_config();
        let blocker = s.file.path().with_file_name("telescope_config.json.tmp");
        std::fs::create_dir(&blocker).unwrap();
        s.move_device_settings("old", "new");
        s.with_plugin(0, |p| p.set_config(&json!({"phones": [{"id": "new"}]})).unwrap());
        assert!(!s.save_now());
        assert_eq!(s.file.load()["devices"]["old"]["plugin_configs"]["transforms"]["zoom"], 4);
        std::fs::remove_dir(&blocker).unwrap();
        assert!(s.save_now());
        assert_eq!(s.file.load()["devices"]["new"]["plugin_configs"]["transforms"]["zoom"], 4);
    }

    #[test]
    fn a_reset_connection_section_does_not_cost_the_phones_their_settings_at_the_next_start() {
        let mut s = state();
        s.register(stub("connection", json!({"phones": []}), false).0).unwrap();
        s.register(stub("transforms", json!({"zoom": 1}), true).0).unwrap();
        let zoom = |n: u32| json!({"plugin_configs": {"transforms": {"zoom": n}}});
        s.file.save(&mut json!({"plugin_configs": {"connection": {"phones": [{"name": "damaged"}]}},
                                "devices": {"a": zoom(2), "b": zoom(3)}, "output_slots": {"a": 1, "b": 2}}));
        s.apply_saved_config();
        // Connection drops the entry it can't read, so what it saves is an empty list
        s.with_plugin(0, |p| p.set_config(&json!({"phones": []})).unwrap());
        assert!(s.save_now());
        assert_eq!(s.file.load()["plugin_configs"]["connection"]["phones"], json!([]));
        s.apply_saved_config();
        let saved = s.file.load();
        assert_eq!(saved["devices"], json!({"a": zoom(2), "b": zoom(3)}));
        assert_eq!(saved["output_slots"], json!({"a": 1, "b": 2}));
    }

    #[test]
    fn starting_drops_the_settings_of_a_phone_that_is_not_paired_and_keeps_browsers_and_webcams() {
        let mut s = state();
        s.register(stub("connection", json!({"phones": []}), false).0).unwrap();
        s.register(stub("transforms", json!({"zoom": 1}), true).0).unwrap();
        let zoom = |n: u32| json!({"plugin_configs": {"transforms": {"zoom": n}}});
        s.file.save(&mut json!({"plugin_configs": {"connection": {"phones": [{"id": "kept", "name": "Phone", "token": "tok"}]}},
                                "devices": {"kept": zoom(2), "gone": zoom(3), "browser:x": zoom(4), "webcam:y": zoom(5)},
                                "output_slots": {"kept": 1, "gone": 2, "webcam:y": 3}}));
        s.apply_saved_config();
        let saved = s.file.load();
        assert_eq!(saved["devices"], json!({"kept": zoom(2), "browser:x": zoom(4), "webcam:y": zoom(5)}));
        assert_eq!(saved["output_slots"], json!({"kept": 1, "webcam:y": 3}));
        assert!(s.save_now());
        assert!(s.file.load()["output_slots"].get("gone").is_none(), "what started with the file isn't written back");
    }

    #[test]
    fn apply_device_profile_resets_defaults_before_saved_values() {
        let mut s = state();
        let (p, applied) = stub("transforms", json!({"zoom": 1, "pan_x": 0}), true);
        s.register(p).unwrap();
        s.with_plugin(0, |p| p.set_config(&json!({"zoom": 5, "pan_x": 1})).unwrap());
        applied.lock().unwrap().clear();
        let cfg = json!({"devices": {"Phone": {"plugin_configs": {"transforms": {"zoom": 3}}}}});
        s.apply_device_profile(Some("Phone"), &cfg);
        assert_eq!(*applied.lock().unwrap(), vec![json!({"zoom": 1, "pan_x": 0}), json!({"zoom": 3})]);
        assert_eq!(s.plugin_config("transforms").unwrap(), json!({"zoom": 3, "pan_x": 0}));
    }

    #[test]
    fn apply_device_profile_none_uses_defaults_only() {
        let mut s = state();
        let (p, applied) = stub("monitoring", json!({"battery_alert": 20}), true);
        s.register(p).unwrap();
        s.with_plugin(0, |p| p.set_config(&json!({"battery_alert": 90})).unwrap());
        applied.lock().unwrap().clear();
        s.apply_device_profile(None, &json!({}));
        assert_eq!(*applied.lock().unwrap(), vec![json!({"battery_alert": 20})]);
        assert_eq!(s.plugin_config("monitoring").unwrap(), json!({"battery_alert": 20}));
    }

    #[test]
    fn switch_device_saves_the_old_profile_and_applies_the_new_one() {
        let mut s = state();
        let (p, _) = stub("transforms", json!({"zoom": 2}), true);
        s.register(p).unwrap();
        let mut cfg = s.file.load();
        cfg["devices"] = json!({"B": {"plugin_configs": {"transforms": {"zoom": 7}}}});
        s.file.save(&mut cfg);
        s.selected = Some("A".into());
        s.streams.streaming = true;
        s.switch_device(Some("A"), Some("B"));
        let cfg = s.file.load();
        assert_eq!(cfg["devices"]["A"]["plugin_configs"]["transforms"], json!({"zoom": 2}));
        assert_eq!(s.plugin_config("transforms").unwrap()["zoom"], 2, "the streams stop with the old phone's settings up");
        assert_eq!(s.effects, vec![Effect::StopAll, Effect::FinishSwitch { new: Some("B".into()), start: true }]);
        s.effects.clear();
        s.finish_switch(Some("B"), true);
        assert_eq!(s.file.load()["selected_device"], "B");
        assert_eq!(s.plugin_config("transforms").unwrap()["zoom"], 7);
        assert_eq!(s.effects, vec![Effect::Start { interactive: true }]);
        assert!(matches!(s.bus.next(), Some(Event::DeviceChanged { source_id }) if source_id == "B"));
    }

    /// Makes saving fail by putting a folder where the file is written first, until the returned path is removed.
    fn block_saving(s: &State) -> std::path::PathBuf {
        let mut name = s.file.path().file_name().unwrap().to_os_string();
        name.push(".tmp");
        let tmp = s.file.path().with_file_name(name);
        std::fs::create_dir(&tmp).unwrap();
        tmp
    }

    #[test]
    fn a_phone_left_while_saving_fails_is_saved_later_and_handed_back_if_it_returns_first() {
        let mut s = state();
        s.register(stub("transforms", json!({"zoom": 2}), true).0).unwrap();
        s.file.save(&mut json!({"devices": {"B": {"plugin_configs": {"transforms": {"zoom": 7}}}}}));
        s.selected = Some("A".into());
        s.with_plugin(0, |p| p.set_config(&json!({"zoom": 5})).unwrap());
        let blocked = block_saving(&s);
        s.switch_device(Some("A"), Some("B"));
        assert_eq!(s.plugin_config("transforms").unwrap()["zoom"], 7);
        assert!(s.effects.iter().any(|e| matches!(e, Effect::Notify { .. })), "the save-failure notice");
        // Back on A before any save works: its changes are still there
        s.switch_device(Some("B"), Some("A"));
        assert_eq!(s.plugin_config("transforms").unwrap()["zoom"], 5);
        s.with_plugin(0, |p| p.set_config(&json!({"zoom": 6})).unwrap());
        s.switch_device(Some("A"), Some("B"));
        std::fs::remove_dir(&blocked).unwrap();
        assert!(s.save_now(), "the next save takes the kept settings along");
        let devices = s.file.load()["devices"].clone();
        assert_eq!(devices["A"]["plugin_configs"]["transforms"]["zoom"], 6);
        assert_eq!(devices["B"]["plugin_configs"]["transforms"]["zoom"], 7);
        assert!(s.unsaved_profiles.is_empty());
    }

    #[test]
    fn a_switch_files_a_plugin_that_stays_with_the_main_stream_under_that_stream() {
        let mut s = state();
        s.file.save(&mut json!({}));
        struct Stays;
        impl Plugin for Stays {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "mic".into(), device_local: true, follows_focus: false, ..Default::default() }
            }
            fn get_config(&self) -> Value {
                json!({"gain": 3})
            }
        }
        s.register(stub("transforms", json!({"zoom": 2.0}), true).0).unwrap();
        s.register(Box::new(Stays)).unwrap();
        s.selected = Some("B".into());
        s.streams.main = Some(("A".into(), "Phone A".into()));
        s.switch_device(Some("B"), Some("C"));
        let cfg = s.file.load_strict().unwrap();
        assert_eq!(cfg["devices"]["B"]["plugin_configs"]["transforms"]["zoom"], json!(2.0));
        assert_eq!(cfg["devices"]["A"]["plugin_configs"]["mic"]["gain"], json!(3));
        assert!(cfg["devices"]["B"]["plugin_configs"].get("mic").is_none(), "A's mic isn't B's");
    }

    #[derive(Default)]
    struct Pack(std::sync::Mutex<(String, String)>, std::sync::Mutex<Vec<(telescope_core::vision::Turn, telescope_core::vision::Turn)>>);

    impl telescope_core::vision::VisionService for Pack {
        fn attach(&self, _bus: telescope_core::BusSender) {}
        fn state(&self) -> String {
            String::new()
        }
        fn load(&self) {}
        fn person_mask(&self, _source_id: &str, _frame: &telescope_core::Frame) -> Option<Arc<Vec<u8>>> {
            None
        }
        fn want_people(&self, _owner: &str, _on: bool) {}
        fn wants_people(&self) -> bool {
            false
        }
        fn feed_raw(&self, _source_id: &str, _frame: &telescope_core::Frame, _upright: telescope_core::vision::Upright) {}
        fn tracked_people(&self, _source_id: &str) -> (Vec<Value>, u32) {
            (Vec::new(), 0)
        }
        fn view_turned(&self, old: telescope_core::vision::Turn, new: telescope_core::vision::Turn) {
            self.1.lock().unwrap().push((old, new));
        }
        fn drop_stream(&self, _source_id: &str) {}
        fn set_power_saving(&self, _on: bool) {}
        fn diagnostics(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn shutdown(&self) {}
        fn pack(&self) -> telescope_core::vision::VisionPack {
            Default::default()
        }
        fn download(&self, _switch_on: &str) {}
        fn cancel_download(&self) {}
        fn remove_pack(&self) -> Result<(), String> {
            Ok(())
        }
        fn download_failed(&self, error: &str, plugin: &str) {
            *self.0.lock().unwrap() = (error.to_string(), plugin.to_string());
        }
    }

    #[test]
    fn a_download_that_worked_says_so_when_the_switch_couldnt_be_saved() {
        let mut s = state();
        s.file.save(&mut json!({}));
        let pack = Arc::new(Pack::default());
        s.vision = Some(pack.clone());
        std::fs::create_dir(s.file.path().with_extension("json.tmp")).unwrap();
        s.bus.emit(Event::VisionDownloadFinished { error: String::new(), switch_on: "framing".into() });
        s.deliver_events();
        let (error, plugin) = pack.0.lock().unwrap().clone();
        assert!(error.starts_with("The download worked but the switch couldn't be saved: "), "{error}");
        assert_eq!(plugin, "framing");
    }

    #[test]
    fn the_aspect_is_the_picture_as_shown_once_a_frame_has_come() {
        let mut s = state();
        s.streams.source_id = Some("phone".into());
        assert_eq!(s.view_aspect(), 0.0);
        s.frame_sizes.lock().unwrap().insert("other".into(), (640, 480));
        s.frame_sizes.lock().unwrap().insert("phone".into(), (1920, 1080));
        assert_eq!(s.view_aspect(), 1920.0 / 1080.0);
        s.view.rotation = Some(90);
        assert_eq!(s.view_aspect(), 1080.0 / 1920.0);
        s.view.rotation = Some(180);
        assert_eq!(s.view_aspect(), 1920.0 / 1080.0);
    }

    #[test]
    fn a_picked_point_reaches_the_phone_in_its_own_frame() {
        struct Hears(Arc<Mutex<Vec<Event>>>);
        impl Plugin for Hears {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "camera_control".into(), ..Default::default() }
            }
            fn on_event(&mut self, _host: &mut dyn telescope_core::host::Host, event: &Event) {
                self.0.lock().unwrap().push(event.clone());
            }
        }
        let mut s = state();
        let heard = Arc::new(Mutex::new(Vec::new()));
        s.register(Box::new(Hears(heard.clone()))).unwrap();
        s.streams.source_id = Some("phone".into());
        s.frame_sizes.lock().unwrap().insert("phone".into(), (1920, 1080));
        s.view.flip_h = true;
        s.bus.emit(Event::FocusPointPicked { x: 0.25, y: 0.5 });
        s.deliver_events();
        let points = |heard: &Mutex<Vec<Event>>| {
            let heard = heard.lock().unwrap();
            let focus: Vec<Event> = heard.iter().filter(|e| matches!(e, Event::FocusPoint { .. })).cloned().collect();
            let meter: Vec<Event> = heard.iter().filter(|e| matches!(e, Event::MeterPoint { .. })).cloned().collect();
            (focus, meter)
        };
        assert_eq!(points(&heard), (vec![Event::FocusPoint { x: 0.75, y: 0.5 }], vec![]));
        s.bus.emit(Event::MeterPointPicked { x: 0.75, y: 0.25 });
        s.deliver_events();
        assert_eq!(points(&heard), (vec![Event::FocusPoint { x: 0.75, y: 0.5 }], vec![Event::MeterPoint { x: 0.25, y: 0.25 }]), "metering moves no focus");
    }

    #[test]
    fn a_click_on_a_warped_preview_focuses_on_what_shows_under_the_mouse() {
        struct Hears(Arc<Mutex<Vec<Event>>>);
        impl Plugin for Hears {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "hears".into(), title: "Hears".into(), ..Default::default() }
            }
            fn on_event(&mut self, _host: &mut dyn telescope_core::host::Host, event: &Event) {
                self.0.lock().unwrap().push(event.clone());
            }
        }
        let mut s = state();
        let heard = Arc::new(Mutex::new(Vec::new()));
        s.register(Box::new(Hears(heard.clone()))).unwrap();
        s.register(Box::new(crate::plugins::preview::Preview::default())).unwrap();
        s.bus.emit(Event::FocusPointAvailable { available: true });
        s.deliver_events();
        s.overlays.set_warp("Desk", Some([1.0, 0.0, 0.25, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]));
        for kind in ["down", "up"] {
            s.ui_event("preview", UiEvent { ui: "card".into(), callback: "view-pointer".into(), args: vec![json!(kind), json!(0.75), json!(0.5), json!(400.0), json!(200.0)] });
        }
        assert!(heard.lock().unwrap().contains(&Event::FocusPointPicked { x: 0.5, y: 0.5 }));
    }

    #[test]
    fn late_restore_failure_is_visible_even_without_the_requesting_plugin() {
        let mut s = state();
        let data = json!({"state": "late", "ok": false, "code": "restore", "error": "Camera settings could not be restored",
            "restore": {"ok": false, "errors": ["iso: unplugged"]}, "files": []});
        s.posts.push_event("absent", Event::RequestProgress { request: RequestId(7), data });
        s.deliver_events();
        let issue = s.banners.issue("photo_restore").expect("cleanup failure survives the absent plugin");
        assert_eq!(issue.text, "Camera settings could not be restored");
        assert_eq!(issue.details, "iso: unplugged");
    }

    #[test]
    fn copy_diagnostics_has_the_build_the_state_and_what_happened() {
        use telescope_core::diagnostics::events;
        use telescope_core::host::HostShell;
        struct Reports;
        impl Plugin for Reports {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "reports".into(), title: "Reports".into(), ..Default::default() }
            }
            fn diagnostics(&self) -> Vec<(String, String)> {
                vec![("Reports".into(), "fine".into())]
            }
        }
        let mut s = state();
        s.register(Box::new(Reports)).unwrap();
        s.register(stub("asks", json!({}), false).0).unwrap();
        events().note("Status: a line from before");
        s.banners.show("k", telescope_core::ui::Issue {
            title: "Phone hot".into(), text: "Let it cool".into(), actions: Vec::new(), kind: "warn".into(), details: String::new(), tell_dismiss: false,
        });
        let i = s.index("asks").unwrap();
        let mut report = String::new();
        s.call(i, |_, host| report = host.diagnostics_report(&[]));
        assert!(report.starts_with("Telescope "), "{report}");
        assert!(report.contains("Reports: fine") && report.contains("Status: a line from before"), "{report}");
        assert!(report.contains("Banner: Phone hot. Let it cool"), "{report}");
    }

    #[test]
    fn a_save_files_each_plugin_under_whose_settings_it_has_now() {
        let mut s = state();
        s.file.save(&mut json!({}));
        struct Stays;
        impl Plugin for Stays {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "mic".into(), device_local: true, follows_focus: false, ..Default::default() }
            }
            fn get_config(&self) -> Value {
                json!({"gain": 3})
            }
        }
        s.register(stub("transforms", json!({"zoom": 2.0}), true).0).unwrap();
        s.register(Box::new(Stays)).unwrap();
        s.selected = Some("B".into());
        s.streams.main = Some(("A".into(), "Phone A".into()));
        assert!(s.save_now());
        let cfg = s.file.load_strict().unwrap();
        assert_eq!(cfg["devices"]["B"]["plugin_configs"]["transforms"]["zoom"], json!(2.0));
        assert_eq!(cfg["devices"]["A"]["plugin_configs"]["mic"]["gain"], json!(3));
        assert!(cfg["devices"]["A"]["plugin_configs"].get("transforms").is_none(), "B's zoom isn't A's");
        assert!(cfg["devices"]["B"]["plugin_configs"].get("mic").is_none());
    }

    #[test]
    fn a_saved_config_that_wont_load_keeps_the_fields_that_do() {
        let mut s = state();
        s.register(stub("setup", json!({"zoom": 1, "canvas": "auto"}), false).0).unwrap();
        let mut cfg = s.file.load();
        cfg["plugin_configs"]["setup"] = json!({"zoom": "big", "canvas": "1080p"});
        s.file.save(&mut cfg);
        s.apply_saved_config();
        assert_eq!(s.plugin_config("setup").unwrap(), json!({"zoom": 1, "canvas": "1080p"}));
    }

    #[test]
    fn a_saved_config_that_wont_load_falls_back_to_defaults() {
        let mut s = state();
        s.register(stub("connection", json!({"selected_phone": "PhoneA"}), false).0).unwrap();
        s.register(stub("setup", json!({"zoom": 1}), false).0).unwrap();
        s.register(stub("transforms", json!({"zoom": 1}), true).0).unwrap();
        s.apply_config(&mut json!({"selected_device": "PhoneA", "plugin_configs": {"setup": "not a dict"}}));
        let cfg = json!({"selected_device": "PhoneA", "plugin_configs": {}, "devices": {"PhoneA": {"plugin_configs": {"transforms": {"zoom": "junk"}}}}});
        s.apply_device_profile(Some("PhoneA"), &cfg);
        assert_eq!(s.plugin_config("setup").unwrap(), json!({"zoom": 1}));
        assert_eq!(s.plugin_config("transforms").unwrap(), json!({"zoom": 1}));
    }

    #[test]
    fn settings_once_kept_for_the_whole_app_go_to_every_phone() {
        let mut s = state();
        s.register(stub("fun_pack", json!({}), true).0).unwrap();
        s.file.save(&mut json!({
            "selected_device": "Phone",
            "plugin_configs": {"fun_pack": {"effect": "vhs"}},
            "devices": {"Phone": {"plugin_configs": {"transforms": {"zoom": 2}}}, "B": {"plugin_configs": {"fun_pack": {"effect": "crt"}}}},
        }));
        s.apply_saved_config();
        assert_eq!(s.plugin_config("fun_pack").unwrap(), json!({"effect": "vhs"}));
        let saved = s.file.load();
        assert!(saved["plugin_configs"].get("fun_pack").is_none());
        assert_eq!(saved["devices"]["Phone"]["plugin_configs"], json!({"transforms": {"zoom": 2}, "fun_pack": {"effect": "vhs"}}));
        assert_eq!(saved["devices"]["B"]["plugin_configs"], json!({"fun_pack": {"effect": "crt"}}));
    }

    struct Side(&'static str, &'static str);
    impl Plugin for Side {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: self.0.into(), ..Default::default() }
        }
        fn card(&mut self) -> Option<CardSpec> {
            Some(card_in(self.1))
        }
    }

    fn with_side_cards() -> State {
        let mut s = state();
        for (name, region) in [("one", "left"), ("two", "left"), ("three", "right")] {
            s.register(Box::new(Side(name, region))).unwrap();
        }
        s
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_saved_card_order_comes_back_at_the_next_start_and_not_before() {
        let mut s = with_side_cards();
        assert!(!s.card_layout_restart_pending());
        s.set_saved_card_layout(CardLayout { left: names(&["three", "one"]), right: names(&["gone", "two"]) });
        assert_eq!(s.running_card_layout(), CardLayout { left: names(&["one", "two"]), right: names(&["three"]) });
        assert_eq!(s.saved_card_layout(), CardLayout { left: names(&["three", "one"]), right: names(&["two"]) });
        assert!(s.card_layout_restart_pending());
        assert_eq!(s.file.load()["card_layout"], json!({"left": ["three", "one"], "right": ["gone", "two"]}));
        let mut t = State::new(ConfigFile::at(s.file.path()));
        for (name, region) in [("one", "left"), ("two", "left"), ("three", "right")] {
            t.register(Box::new(Side(name, region))).unwrap();
        }
        t.apply_saved_config();
        assert_eq!(t.running_card_layout(), CardLayout { left: names(&["three", "one"]), right: names(&["two"]) });
        assert!(!t.card_layout_restart_pending());
        assert_eq!(t.saved_layout.as_ref().unwrap().right, names(&["gone", "two"]));
    }

    #[test]
    fn clearing_the_saved_card_order_goes_back_to_the_default() {
        let mut s = with_side_cards();
        s.set_saved_card_layout(CardLayout { left: names(&["two"]), right: names(&["one", "three"]) });
        s.clear_saved_card_layout();
        assert!(s.file.load().get("card_layout").is_none());
        assert_eq!(s.saved_card_layout(), s.running_card_layout());
        assert!(!s.card_layout_restart_pending());
        // A running custom order with nothing saved is a pending change back to the default
        s.panels.apply_layout(&CardLayout { left: names(&["two"]), right: names(&["one", "three"]) });
        assert!(s.card_layout_restart_pending());
    }

    #[test]
    fn an_old_config_without_a_card_order_gives_the_default() {
        let mut s = with_side_cards();
        s.file.save(&mut json!({"collapsed": []}));
        s.apply_saved_config();
        assert!(s.saved_layout.is_none());
        assert_eq!((s.panels.left.clone(), s.panels.right.clone()), (vec![0, 1], vec![2]));
        assert!(!s.card_layout_restart_pending());
    }

    #[test]
    fn folds_are_saved_and_come_back() {
        let mut s = state();
        s.collapse.set_folded("camera_control/Camera", true);
        s.save_now();
        let mut t = State::new(ConfigFile::at(s.file.path()));
        t.apply_saved_config();
        assert!(t.collapse.is_folded("camera_control/Camera"));
    }

    #[test]
    fn a_switched_off_plugins_folds_survive_saving_while_it_is_gone() {
        let mut s = state();
        s.collapse.load(Some(&json!(["gone/Card", "gone/Card/Section"])));
        s.save_now();
        assert_eq!(s.file.load()["collapsed"], json!(["gone/Card", "gone/Card/Section"]));
    }

    #[test]
    fn an_optional_plugin_starts_off_and_isnt_loaded_until_its_switched_on() {
        let mut s = state();
        let optional = PluginInfo { name: "framing".into(), title: "Auto framing".into(), optional: true, ..Default::default() };
        s.load_builtins(vec![stub("connection", json!({}), false).0, Box::new(Stub { info: optional, config: json!({}), applied: Default::default() })]).unwrap();
        assert!(s.index("framing").is_none() && s.index("connection").is_some());
        assert!(s.plugins_off().is_empty(), "never switched on isn't \"turned off\"");
        assert!(s.banners.issue("plugins_off").is_none());
        let mut cfg = s.file.load();
        switches::set_optional_on(&mut cfg, "framing", true);
        switches::reset_to_defaults(&mut cfg);
        assert!(switches::optional_on(&cfg).is_empty());
    }

    /// A stub that came from a plugin folder.
    fn added_stub(name: &str) -> Box<dyn Plugin> {
        Box::new(Stub { info: PluginInfo { name: name.into(), added: true, ..Default::default() }, config: json!({}), applied: Default::default() })
    }

    struct Quitter(bool);

    impl Plugin for Quitter {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: "quitter".into(), title: "Quitter".into(), added: self.0, ..Default::default() }
        }
        fn setup(&mut self, _h: &mut dyn telescope_core::Host) {
            panic!("no card today");
        }
    }

    #[test]
    fn added_plugins_are_listed_in_the_diagnostics_as_running_or_not() {
        let mut s = state();
        s.load_builtins(vec![stub("setup", json!({}), false).0, added_stub("extra")]).unwrap();
        assert!(s.banners.issue("plugins").is_none());
        assert_eq!(s.builtins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["setup"]);
        s.set_plugin_problems(&[broken("gone", "Gone")]);
        let line = s.diagnostics().into_iter().find(|(k, _)| k == "Added plugins").map(|(_, v)| v);
        assert_eq!(line.as_deref(), Some("extra (running), gone (didn't start)"));
        assert!(state().diagnostics().iter().all(|(k, _)| k != "Added plugins"));
    }

    #[test]
    fn a_built_in_plugin_that_breaks_still_stops_the_start() {
        let mut s = state();
        assert!(s.load_builtins(vec![Box::new(Quitter(false))]).unwrap_err().contains("quitter"));
    }

    #[test]
    fn an_added_plugin_that_breaks_while_starting_is_taken_back_out() {
        let mut s = state();
        s.load_builtins(vec![stub("setup", json!({}), false).0, Box::new(Quitter(true))]).unwrap();
        assert!(s.index("quitter").is_none() && s.index("setup").is_some());
        assert_eq!(s.banners.issue("plugins").unwrap().title, "Couldn't start Quitter");
        assert!(s.problems["quitter"].contains("quitter"));
        assert_eq!(s.diagnostics().into_iter().find(|(k, _)| k == "Added plugins").unwrap().1, "quitter (didn't start)");
    }

    #[test]
    fn a_plugin_whose_settings_hook_fails_keeps_what_was_saved() {
        let mut s = state();
        s.register(stub("shaky", Value::Null, false).0).unwrap();
        assert_eq!(s.slots[0].defaults, json!({}));
        assert!(s.plugin_config("shaky").is_none());
        let saved = json!({"zoom": 2});
        assert_eq!(s.config_of(0, Some(&saved)), Some(saved));
        assert_eq!(s.config_of(0, None), None);
    }

    fn broken(name: &str, title: &str) -> PluginProblem {
        PluginProblem { name: name.into(), title: title.into(), error: format!("{name} broke"), blocked: false }
    }

    #[test]
    fn added_plugins_that_didnt_start_get_a_banner_with_a_plugins_action() {
        let mut s = state();
        s.set_plugin_problems(&[]);
        assert!(s.banners.issue("plugins").is_none());
        s.set_plugin_problems(&[broken("a", "Alpha")]);
        let issue = s.banners.issue("plugins").unwrap().clone();
        assert_eq!(issue.title, "Couldn't start Alpha");
        assert_eq!(issue.text, "Telescope works without the plugin. The Plugins tab in Settings shows what went wrong.");
        assert_eq!(issue.kind, "warn");
        assert_eq!(issue.actions, vec![IssueAction { label: "Plugins…".into(), does: IssueDoes::OpenSettings { tab: "plugins".into() } }]);
        assert_eq!(s.problems.get("a").map(String::as_str), Some("a broke"));
        s.set_plugin_problems(&[broken("a", "Alpha"), broken("b", "Beta"), broken("c", "Gamma")]);
        let issue = s.banners.issue("plugins").unwrap();
        assert_eq!(issue.title, "Couldn't start Alpha, Beta and Gamma");
        assert_eq!(issue.text, "Telescope works without them. The Plugins tab in Settings shows what went wrong.");
        s.set_plugin_problems(&[broken("a", "A"), broken("b", "B"), broken("c", "C"), broken("d", "D")]);
        assert_eq!(s.banners.issue("plugins").unwrap().title, "Couldn't start 4 plugins");
    }

    #[test]
    fn a_plugin_that_panics_doesnt_stop_the_others() {
        struct Boom(Arc<std::sync::atomic::AtomicBool>);
        impl Plugin for Boom {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "boom".into(), ..Default::default() }
            }
            fn get_config(&self) -> Value {
                if self.0.load(std::sync::atomic::Ordering::SeqCst) {
                    panic!("boom");
                }
                json!({"kept": 1})
            }
            fn on_event(&mut self, _h: &mut dyn telescope_core::Host, _e: &Event) {
                panic!("boom");
            }
        }
        struct Last(Arc<Mutex<Vec<String>>>);
        impl Plugin for Last {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "last".into(), ..Default::default() }
            }
            fn get_config(&self) -> Value {
                json!({"x": self.0.lock().unwrap().len()})
            }
            fn on_event(&mut self, _h: &mut dyn telescope_core::Host, _e: &Event) {
                self.0.lock().unwrap().push("last".into());
            }
        }
        let mut s = state();
        let (breaks, heard) = (Arc::new(std::sync::atomic::AtomicBool::new(false)), Arc::new(Mutex::new(Vec::new())));
        s.register(Box::new(Boom(breaks.clone()))).unwrap();
        s.register(Box::new(Last(heard.clone()))).unwrap();
        assert!(s.save_now());
        breaks.store(true, std::sync::atomic::Ordering::SeqCst);
        s.bus.emit(Event::StreamStopped);
        s.deliver_events();
        assert_eq!(*heard.lock().unwrap(), ["last"]);
        assert!(s.slots.iter().all(|slot| slot.plugin.is_some()));
        assert!(s.save_now(), "its get_config doesn't stop the save");
        assert_eq!(s.file.load()["plugin_configs"]["boom"], json!({"kept": 1}));
        assert_eq!(s.file.load()["plugin_configs"]["last"], json!({"x": 1}));
        // It's off until the next start, and says so
        assert_eq!(s.stopped().get("boom").map(String::as_str), Some("boom"));
        assert!(s.effects.contains(&Effect::Stopped("boom".into())));
        assert_eq!(s.banners.issue("plugins_stopped").unwrap().title, "boom stopped working");
        assert!(s.diagnostics().iter().any(|(k, v)| k == "Plugins stopped" && v == "boom (boom)"));
        assert!(s.call(0, |_, _| ()).is_none(), "its hooks aren't called again");
        s.bus.emit(Event::StreamStopped);
        s.deliver_events();
        assert_eq!(*heard.lock().unwrap(), ["last", "last"]);
    }

    #[test]
    fn a_stopped_plugin_gives_back_what_it_held_and_its_frame_steps_go() {
        struct Holder;
        impl Plugin for Holder {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "holder".into(), ..Default::default() }
            }
            fn on_plugins_ready(&mut self, host: &mut dyn telescope_core::Host) {
                host.want_frames("holder", true);
            }
            fn on_event(&mut self, _h: &mut dyn telescope_core::Host, _e: &Event) {
                panic!("no");
            }
            fn frame_step(&mut self) -> Option<telescope_core::frame::FrameStep> {
                Some(telescope_core::frame::FrameStep::Shader(telescope_core::frame::ShaderStep { keeps_geometry: false, wgsl: String::new(), uniforms: Value::Null, textures: Vec::new() }))
            }
        }
        let mut s = state();
        s.register(Box::new(Holder)).unwrap();
        s.each(|p, host| p.on_plugins_ready(host));
        assert!(!s.frame_wants.is_empty());
        assert!(s.pipeline_steps(None).iter().any(|st| st.owner == "holder"));
        s.bus.emit(Event::StreamStopped);
        s.deliver_events();
        assert!(s.frame_wants.is_empty(), "what it asked for is given back");
        assert!(!s.pipeline_steps(None).iter().any(|st| st.owner == "holder"));
    }

    #[test]
    fn an_added_plugin_that_stops_loses_its_banners_and_its_state() {
        struct Banner(bool);
        impl Plugin for Banner {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "loud".into(), added: self.0, ..Default::default() }
            }
            fn setup(&mut self, host: &mut dyn telescope_core::Host) {
                let issue = Issue { title: "Hi".into(), text: String::new(), actions: Vec::new(), kind: "info".into(), details: String::new(), tell_dismiss: false };
                host.show_issue(&telescope_core::ui::added_issue_key("loud", "k"), issue);
            }
            fn on_event(&mut self, _h: &mut dyn telescope_core::Host, _e: &Event) {
                panic!("no");
            }
        }
        let key = telescope_core::ui::added_issue_key("loud", "k");
        let mut s = state();
        s.register(Box::new(Banner(true))).unwrap();
        assert!(s.banners.issue(&key).is_some());
        s.bus.emit(Event::StreamStopped);
        s.deliver_events();
        assert!(s.stopped().contains_key("loud"));
        assert!(s.banners.issue(&key).is_none(), "its buttons would answer nobody");
        assert!(s.slots[0].plugin.is_none(), "an added plugin's script state is dropped");
        // A built-in plugin keeps its object, and a banner of the app's own is nobody's to clear
        let mut s = state();
        s.register(Box::new(Banner(false))).unwrap();
        s.banners.show("plain", Issue { title: "Other".into(), text: String::new(), actions: Vec::new(), kind: "info".into(), details: String::new(), tell_dismiss: false });
        s.bus.emit(Event::StreamStopped);
        s.deliver_events();
        assert!(s.slots[0].plugin.is_some() && s.banners.issue("plain").is_some());
    }

    thread_local! {
        static CLOSED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    fn record_close(name: &str, _why: &str) {
        CLOSED.with(|c| c.borrow_mut().push(name.to_string()));
    }

    struct HidesOnCpu;

    impl telescope_core::frame::CpuStep for HidesOnCpu {
        fn process(&mut self, _frame: &mut telescope_core::Frame, _ctx: &dyn telescope_core::frame::FrameContext) {}
        fn hides(&self) -> bool {
            true
        }
    }

    /// A plugin at stage, handing out step (if any), that panics on any event.
    struct Panics(&'static str, telescope_core::frame::FrameStage, Option<fn() -> telescope_core::frame::FrameStep>);

    impl Plugin for Panics {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: self.0.into(), frame_stage: self.1, ..Default::default() }
        }
        fn on_event(&mut self, _h: &mut dyn telescope_core::Host, _e: &Event) {
            panic!("no");
        }
        fn frame_step(&mut self) -> Option<telescope_core::frame::FrameStep> {
            self.2.map(|make| make())
        }
    }

    #[test]
    fn a_plugin_that_hides_things_closes_the_app_when_it_panics_and_others_are_only_switched_off() {
        use telescope_core::frame::{FrameStage, FrameStep, ShaderStep};
        let mut s = state();
        s.hiding_panic = record_close;
        // Privacy blur before its first step, a step on the CPU that hides at the Effects stage, and its card-or-CPU kind
        s.register(Box::new(Panics("blur", FrameStage::Privacy, None))).unwrap();
        s.register(Box::new(Panics("cpu_hider", FrameStage::Effects, Some(|| FrameStep::Cpu(Box::new(HidesOnCpu)))))).unwrap();
        s.register(Box::new(Panics("either_hider", FrameStage::Effects, Some(move || FrameStep::Either(ShaderStep { keeps_geometry: false, wgsl: String::new(), uniforms: Value::Null, textures: Vec::new() }, Box::new(HidesOnCpu)))))).unwrap();
        s.register(Box::new(Panics("plain", FrameStage::Effects, Some(move || FrameStep::Shader(ShaderStep { keeps_geometry: false, wgsl: String::new(), uniforms: Value::Null, textures: Vec::new() }))))).unwrap();
        s.pipeline_steps(None);
        s.bus.emit(Event::StreamStopped);
        s.deliver_events();
        assert_eq!(CLOSED.with(|c| c.borrow().clone()), ["blur", "cpu_hider", "either_hider"]);
        assert_eq!(s.stopped().keys().collect::<Vec<_>>(), ["plain"], "one that hides nothing is switched off and the app carries on");
    }

    struct UnsureOnCpu;

    impl telescope_core::frame::CpuStep for UnsureOnCpu {
        fn process(&mut self, _frame: &mut telescope_core::Frame, _ctx: &dyn telescope_core::frame::FrameContext) {}
        fn hides(&self) -> bool {
            panic!("can't say")
        }
    }

    #[test]
    fn a_step_that_panics_saying_whether_it_hides_counts_as_hiding() {
        use telescope_core::frame::{FrameStage, FrameStep};
        let mut s = state();
        s.hiding_panic = record_close;
        s.register(Box::new(Panics("unsure", FrameStage::Effects, Some(|| FrameStep::Cpu(Box::new(UnsureOnCpu)))))).unwrap();
        s.pipeline_steps(None);
        assert_eq!(CLOSED.with(|c| c.borrow().clone()), ["unsure"]);
        assert!(!s.pipeline_steps(None).iter().any(|st| st.owner == "unsure"));
    }

    #[test]
    fn a_get_config_that_panics_while_saving_never_gets_saved_over_the_good_settings() {
        struct Breaks(Arc<std::sync::atomic::AtomicBool>);
        impl Plugin for Breaks {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "breaks".into(), ..Default::default() }
            }
            fn get_config(&self) -> Value {
                if self.0.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    panic!("half way");
                }
                json!({"good": false})
            }
        }
        let mut s = state();
        let breaks = Arc::new(std::sync::atomic::AtomicBool::new(false));
        s.register(Box::new(Breaks(breaks.clone()))).unwrap();
        let mut cfg = s.file.load();
        cfg["plugin_configs"]["breaks"] = json!({"good": true});
        s.file.save(&mut cfg);
        breaks.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(s.save_now(), "a card moved: the save reads the plugin, which panics");
        // the next save (another card moved) stops it first and keeps what was saved, though get_config would answer now
        assert!(s.save_now());
        assert!(s.stopped().contains_key("breaks"));
        assert_eq!(s.file.load()["plugin_configs"]["breaks"], json!({"good": true}));
    }

    #[test]
    fn a_stopped_plugin_takes_its_sources_with_it_and_reads_that_panic_stop_it_too() {
        struct Offers;
        impl Plugin for Offers {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "offers".into(), ..Default::default() }
            }
            fn get_config(&self) -> Value {
                panic!("can't say");
            }
        }
        let mut s = state();
        s.register(Box::new(Offers)).unwrap_err();
        let mut s = state();
        s.source_owners.push(("cam".into(), "offers".into()));
        s.slots.push(Slot { plugin: Some(Box::new(Offers)), info: Offers.info(), card: None, header: None, actions: vec![], tray: vec![], defaults: json!({}),
                            timer: None, seen: Vec::new(), moved: Vec::new(), stopped: None, hides: false, shut_down: false, ended: false, starting: false });
        assert!(s.plugin_config("offers").is_none());
        s.stop_late();
        assert!(s.stopped().contains_key("offers"), "a panicking get_config stops it");
        assert!(s.effects.contains(&Effect::RemoveSource("cam".into())));
        assert!(s.source_owners.is_empty());
    }

    #[test]
    fn an_added_plugin_that_needs_a_builtin_that_is_off_or_stopped_doesnt_start() {
        let mut s = state();
        s.builtins = vec![PluginInfo { name: "photos".into(), title: "Photos".into(), ..Default::default() }];
        let needs = |names: &[&str]| PluginInfo { name: "trap".into(), needs: names.iter().map(|n| n.to_string()).collect(), ..Default::default() };
        assert_eq!(s.missing_needs(&needs(&["photos"])).as_deref(), Some("It needs Photos, which is switched off."));
        assert_eq!(s.missing_needs(&needs(&["warp_drive"])).as_deref(), Some("It needs a plugin called warp_drive, which this version of Telescope doesn't have."));
        assert_eq!(s.missing_needs(&needs(&["photos", "warp_drive"])).as_deref(),
                   Some("It needs a plugin called warp_drive, which this version of Telescope doesn't have. It needs Photos, which is switched off."));
        s.register(stub("photos", json!({}), false).0).unwrap();
        assert_eq!(s.missing_needs(&needs(&["photos"])), None);
        assert_eq!(s.missing_needs(&needs(&[])), None);
        s.stop_after_panic(0, "boom");
        assert_eq!(s.missing_needs(&needs(&["photos"])).as_deref(), Some("It needs Photos, which is switched off."), "a stopped plugin isn't running");
    }

    #[test]
    fn the_needs_gate_judges_by_the_saved_switches_before_anything_runs() {
        let s = state();
        let photos = PluginInfo { title: "Photos".into(), ..stub("photos", json!({}), false).0.info() };
        s.admin.set_builtin_off("photos", true).unwrap();
        let gate = needs_gate(vec![photos.clone()], s.admin.clone());
        assert_eq!(gate(&["photos".into()]).as_deref(), Some("It needs Photos, which is switched off."));
        assert_eq!(gate(&[]), None);
        s.admin.set_builtin_off("photos", false).unwrap();
        assert_eq!(needs_gate(vec![photos], s.admin.clone())(&["photos".into()]), None);
    }

    #[test]
    fn a_dependent_is_stopped_before_the_ready_hooks_when_what_it_needs_stopped_meanwhile() {
        struct Dependent(Arc<Mutex<u32>>);
        impl Plugin for Dependent {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "trap".into(), added: true, needs: vec!["photos".into()], ..Default::default() }
            }
            fn on_plugins_ready(&mut self, _h: &mut dyn telescope_core::Host) {
                *self.0.lock().unwrap() += 1;
            }
        }
        let mut s = state();
        s.builtins = vec![PluginInfo { name: "photos".into(), title: "Photos".into(), ..Default::default() }];
        s.register(stub("photos", json!({}), false).0).unwrap();
        let ready = Arc::new(Mutex::new(0));
        s.register(Box::new(Dependent(ready.clone()))).unwrap();
        // Photos panics while the saved settings go in
        s.stop_after_panic(0, "settings: boom");
        s.apply_saved_config();
        assert_eq!(*ready.lock().unwrap(), 0, "it never hears on_plugins_ready");
        assert!(s.stopped().contains_key("trap") && s.blocked.contains("trap"));
    }

    #[test]
    fn an_added_plugin_stops_when_a_built_in_it_needs_stops_later() {
        struct Dependent;
        impl Plugin for Dependent {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "trap".into(), added: true, needs: vec!["photos".into()], ..Default::default() }
            }
        }
        let mut s = state();
        s.builtins = vec![PluginInfo { name: "photos".into(), title: "Photos".into(), ..Default::default() }];
        s.register(stub("photos", json!({}), false).0).unwrap();
        s.register(Box::new(Dependent)).unwrap();
        assert!(s.stopped().is_empty());
        s.stop_after_panic(0, "boom");
        assert!(s.stopped().contains_key("photos") && s.stopped().contains_key("trap"));
        assert_eq!(s.stopped()["trap"], "It needs Photos, which is switched off.");
        assert!(s.blocked.contains("trap"));
    }

    #[test]
    fn an_added_plugin_that_hides_things_closes_the_app_when_the_built_in_it_needs_stops() {
        struct Hider;
        impl Plugin for Hider {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "hider".into(), added: true, needs: vec!["photos".into()], frame_stage: telescope_core::frame::FrameStage::Privacy, ..Default::default() }
            }
        }
        let mut s = state();
        s.hiding_panic = record_close;
        CLOSED.with(|c| c.borrow_mut().clear());
        s.builtins = vec![PluginInfo { name: "photos".into(), title: "Photos".into(), ..Default::default() }];
        s.register(stub("photos", json!({}), false).0).unwrap();
        s.register(Box::new(Hider)).unwrap();
        s.stop_after_panic(0, "boom");
        assert_eq!(CLOSED.with(|c| c.borrow().clone()), ["hider"]);
        assert!(!s.stopped().contains_key("hider"), "it carries on hiding until the app closes");
    }

    #[test]
    fn built_in_or_added_comes_from_where_the_plugin_came_from_and_a_blocked_one_says_so() {
        let mut s = state();
        let mut cfg = s.file.load();
        switches::set_builtin_off(&mut cfg, "photos", true);
        s.file.save(&mut cfg);
        let photos = PluginInfo { name: "photos".into(), title: "Photos".into(), ..Default::default() };
        let trap = PluginInfo { name: "trap".into(), needs: vec!["photos".into()], added: true, ..Default::default() };
        s.load_builtins(vec![stub("setup", json!({}), false).0, Box::new(Stub { info: photos, config: json!({}), applied: Default::default() }),
                             Box::new(Stub { info: trap, config: json!({}), applied: Default::default() })]).unwrap();
        assert_eq!(s.builtins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["setup", "photos"]);
        assert_eq!(s.added.len(), 0);
        assert!(s.index("trap").is_none());
        assert!(s.blocked.contains("trap") && s.problems["trap"] == "It needs Photos, which is switched off.");
    }

    #[test]
    fn a_second_plugin_with_a_running_ones_name_is_refused_before_it_can_touch_anything() {
        struct Meter(&'static str, bool);
        impl Plugin for Meter {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "meter".into(), title: self.0.into(), ..Default::default() }
            }
            fn setup(&mut self, host: &mut dyn telescope_core::Host) {
                host.share("meter", "level", Some(json!(self.0))).unwrap();
                assert!(!self.1, "its setup must never run");
            }
        }
        let mut s = state();
        s.register(Box::new(Meter("first", false))).unwrap();
        assert!(s.register(Box::new(Meter("second", true))).unwrap_err().contains("already running"));
        assert_eq!(s.registry.get("meter.level"), Some(json!("first")));
        assert_eq!(s.slots.len(), 1);
    }

    #[test]
    fn a_plugins_values_go_after_its_shutdown_hook_and_only_those_still_running_hear_it() {
        struct Quiet {
            name: &'static str,
            log: Arc<Mutex<Vec<String>>>,
        }
        impl Plugin for Quiet {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: self.name.into(), ..Default::default() }
            }
            fn on_plugins_ready(&mut self, host: &mut dyn telescope_core::Host) {
                host.share(self.name, "endpoint", Some(json!(1))).unwrap();
            }
            fn on_event(&mut self, _h: &mut dyn telescope_core::Host, e: &Event) {
                if let Event::SharedChanged { key } = e {
                    self.log.lock().unwrap().push(format!("{} heard {key}", self.name));
                }
            }
            fn shutdown(&mut self, host: &mut dyn telescope_core::Host) {
                self.log.lock().unwrap().push(format!("{} sees a={:?} b={:?}", self.name, host.shared("a.endpoint"), host.shared("b.endpoint")));
            }
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut s = state();
        for name in ["a", "b", "c"] {
            s.register(Box::new(Quiet { name, log: log.clone() })).unwrap();
        }
        s.each(|p, host| p.on_plugins_ready(host));
        log.lock().unwrap().clear();
        s.shut_down_plugins();
        assert_eq!(*log.lock().unwrap(), [
            "a sees a=Some(Number(1)) b=Some(Number(1))",
            "b heard a.endpoint",
            "c heard a.endpoint",
            "b sees a=None b=Some(Number(1))",
            "c heard b.endpoint",
            "c sees a=None b=None",
        ]);
        assert!(s.registry.keys("").is_empty());
    }

    #[test]
    fn plugins_share_values_and_a_stopped_plugins_values_go() {
        struct Sharer;
        impl Plugin for Sharer {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "sharer".into(), ..Default::default() }
            }
            fn on_plugins_ready(&mut self, host: &mut dyn telescope_core::Host) {
                host.share("sharer", "count", Some(json!(3))).unwrap();
                assert!(host.share("other", "count", Some(json!(1))).is_err(), "only its own keys");
            }
            fn on_event(&mut self, _h: &mut dyn telescope_core::Host, e: &Event) {
                if matches!(e, Event::StreamStopped) {
                    panic!("gone");
                }
            }
        }
        let mut s = state();
        s.register(Box::new(Sharer)).unwrap();
        s.each(|p, host| p.on_plugins_ready(host));
        assert_eq!(s.registry.get("sharer.count"), Some(json!(3)));
        s.deliver_events();
        s.bus.emit(Event::StreamStopped);
        s.deliver_events();
        assert_eq!(s.registry.get("sharer.count"), None);
        assert!(s.registry.keys("").is_empty());
    }

    #[test]
    fn a_required_plugin_that_panics_carries_on() {
        struct Needed(Arc<Mutex<u32>>);
        impl Plugin for Needed {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "connection".into(), required: true, ..Default::default() }
            }
            fn on_event(&mut self, _h: &mut dyn telescope_core::Host, _e: &Event) {
                *self.0.lock().unwrap() += 1;
                panic!("still needed");
            }
        }
        let mut s = state();
        let heard = Arc::new(Mutex::new(0));
        s.register(Box::new(Needed(heard.clone()))).unwrap();
        for _ in 0..2 {
            s.bus.emit(Event::StreamStopped);
            s.deliver_events();
        }
        assert_eq!(*heard.lock().unwrap(), 2);
        assert!(s.stopped().is_empty());
        assert!(s.banners.issue("plugins_stopped").is_none());
    }

    #[test]
    fn save_now_from_a_hook_saves_what_the_plugin_has_just_changed() {
        struct Renames(&'static str);
        impl Plugin for Renames {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "renames".into(), ..Default::default() }
            }
            fn get_config(&self) -> Value {
                json!({"name": self.0})
            }
            fn on_event(&mut self, host: &mut dyn telescope_core::Host, _e: &Event) {
                self.0 = "new";
                host.save_now();
            }
        }
        let mut s = state();
        s.register(Box::new(Renames("old"))).unwrap();
        s.bus.emit(Event::StreamStopped);
        s.deliver_events();
        assert_eq!(s.file.load()["plugin_configs"]["renames"], json!({"name": "new"}));
    }

    #[test]
    fn plugins_feeding_each_other_events_cant_hold_the_window_forever() {
        struct Echo;
        impl Plugin for Echo {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "echo".into(), ..Default::default() }
            }
            fn on_event(&mut self, host: &mut dyn telescope_core::Host, e: &Event) {
                host.emit(e.clone());
            }
        }
        let mut s = state();
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let w = wakes.clone();
        s.bus.set_waker(move || {
            w.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        s.register(Box::new(Echo)).unwrap();
        s.bus.emit(Event::StreamStopped);
        s.deliver_events();
        assert!(!s.bus.is_empty());
        assert!(wakes.load(std::sync::atomic::Ordering::SeqCst) >= 2, "the rest is scheduled for later");
    }

    struct Hears(String, Arc<std::sync::Mutex<Vec<String>>>);

    impl Plugin for Hears {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: self.0.clone(), ..PluginInfo::default() }
        }
        fn on_event(&mut self, host: &mut dyn telescope_core::host::Host, event: &Event) {
            match event {
                Event::Posted { token, .. } => self.1.lock().unwrap().push(format!("{} {token}", self.0)),
                Event::StreamStopped => {
                    if let Some(poster) = host.poster() {
                        std::thread::spawn(move || poster.post("from_thread", json!({}))).join().unwrap();
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn a_post_from_a_plugins_thread_reaches_only_that_plugin() {
        let mut s = state();
        let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
        s.register(Box::new(Hears("connection".into(), heard.clone()))).unwrap();
        s.register(Box::new(Hears("other".into(), heard.clone()))).unwrap();
        s.posts.push("connection", "resolved", json!({"check_id": 3}));
        s.posts.push("gone", "dropped", json!({}));
        s.deliver_events();
        assert_eq!(*heard.lock().unwrap(), ["connection resolved"]);
        heard.lock().unwrap().clear();
        s.bus.emit(Event::StreamStopped);
        s.deliver_events(); // both post from a thread; each hears its own
        assert_eq!(*heard.lock().unwrap(), ["connection from_thread", "other from_thread"]);
    }

    #[test]
    fn a_plugins_link_goes_to_the_browser() {
        let mut s = state();
        s.register(Box::new(Hears("microphone".into(), Arc::default()))).unwrap();
        s.call(0, |_, host| telescope_core::host::HostShell::open_url(host, "https://vb-audio.com/Cable/"));
        assert!(s.effects.iter().any(|e| matches!(e, Effect::OpenUrl(u) if u == "https://vb-audio.com/Cable/")));
    }


    #[test]
    fn make_main_moves_the_mic_only_onto_a_stream_thats_up() {
        use telescope_core::host::HostStreams;
        let mut s = state();
        s.register(Box::new(Hears("microphone".into(), Arc::default()))).unwrap();
        s.streams.live = vec!["a".into(), "b".into()];
        s.streams.main = Some(("a".into(), "Pixel 8".into()));
        assert_eq!(s.call(0, |_, host| host.main_stream()).flatten(), Some(("a".to_string(), "Pixel 8".to_string())));
        assert_eq!(s.call(0, |_, host| (host.make_main("b"), host.make_main("gone"))), Some((true, false)));
        assert!(s.effects.iter().any(|e| matches!(e, Effect::MakeMain(id) if id == "b")));
        s.streams.stopping_all = true;
        assert_eq!(s.call(0, |_, host| host.make_main("b")), Some(false));
    }


    #[test]
    fn stream_output_plugins_reach_the_streams_camera_and_worker() {
        use telescope_core::host::{HostStreams, OutputChange};
        let mut s = state();
        s.register(Box::new(Hears("stream_output".into(), Arc::default()))).unwrap();
        s.streams.cameras = vec![CameraUse { source_id: "phone-1".into(), slot: 0, label: "Telescope".into(), size: Some((1920, 1080)) }];
        let answers = s.call(0, |_, host| (host.stream_output("phone-1"), host.stream_on_camera(0), host.stream_on_camera(1)));
        assert_eq!(answers, Some(("Telescope".to_string(), Some(("phone-1".to_string(), Some(1920), Some(1080))), None)));
        let change = OutputChange { width: None, height: None, fps: Some(Some(60)) };
        s.call(0, |_, host| host.update_stream_output(change, None));
        assert!(s.effects.iter().any(|e| matches!(e, Effect::UpdateOutput { change: c, source_id: None } if c.fps == Some(Some(60)))));
    }


    #[test]
    fn show_or_hide_answers_which_one_it_does() {
        use telescope_core::host::HostShell;
        let mut s = state();
        s.register(Box::new(Hears("shortcuts".into(), Arc::default()))).unwrap();
        (s.shown, s.window_active) = (Some(true), true);
        assert_eq!(s.call(0, |_, host| host.toggle_window()), Some(false), "up and in front: it goes away");
        (s.shown, s.window_active) = (Some(true), false);
        assert_eq!(s.call(0, |_, host| host.toggle_window()), Some(true), "behind another window or minimized: it comes up");
        (s.shown, s.window_active) = (Some(false), false);
        assert_eq!(s.call(0, |_, host| host.toggle_window()), Some(true));
        assert_eq!(s.effects.iter().filter(|e| matches!(e, Effect::ToggleWindow)).count(), 3);
    }

    #[test]
    fn picking_a_phone_while_nothing_streams_is_left_to_connection() {
        use telescope_core::host::HostStreams;
        let mut s = state();
        s.register(Box::new(Hears("connection".into(), Arc::default()))).unwrap();
        assert_eq!(s.call(0, |_, host| host.pick_source(Some("phone-2"))), Some(false));
        assert!(!s.effects.iter().any(|e| matches!(e, Effect::PickSource(_))));
        s.streams.ids = vec!["phone-1".into(), "phone-2".into()];
        assert_eq!(s.call(0, |_, host| host.pick_source(Some("phone-2"))), Some(true));
        s.streams.ids = vec!["phone-1".into()];
        assert_eq!(s.call(0, |_, host| host.pick_source(Some("phone-3"))), Some(false), "one stream: Connection restarts it");
        s.streams.ids = vec!["phone-1".into(), "phone-2".into()];
        s.streams.waking = true;
        assert_eq!(s.call(0, |_, host| host.pick_source(Some("phone-3"))), Some(false));
    }

    struct NoCtx;

    impl telescope_core::frame::FrameContext for NoCtx {
        fn source_id(&self) -> &str {
            "p1"
        }
        fn person_mask(&self, _frame: &telescope_core::Frame) -> Option<Arc<Vec<u8>>> {
            None
        }
        fn colour_preview(&self, frame: &telescope_core::Frame) -> telescope_core::Frame {
            frame.clone()
        }
    }

    #[test]
    fn a_stream_gets_a_step_from_each_plugin_that_has_one_and_a_broken_one_is_left_out() {
        use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStep, PixelFormat};
        struct AddsOne;
        impl CpuStep for AddsOne {
            fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
                frame.data_mut()[0] += 1;
            }
        }
        struct Frozen;
        impl Plugin for Frozen {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "transforms".into(), ..Default::default() }
            }
            fn frame_step(&mut self) -> Option<FrameStep> {
                Some(FrameStep::Cpu(Box::new(AddsOne)))
            }
        }
        struct Broken;
        impl Plugin for Broken {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "other".into(), ..Default::default() }
            }
            fn frame_step(&mut self) -> Option<FrameStep> {
                panic!("no");
            }
        }
        let mut s = state();
        s.register(Box::new(Frozen)).unwrap();
        s.register(stub("preview", json!({}), false).0).unwrap();
        s.register(Box::new(Broken)).unwrap();
        s.view.flip_h = true;
        let warm = telescope_core::host::ColourStage::new(2, Arc::new(telescope_core::colour::lattice(2).iter().map(|v| v * 0.5).collect()), None);
        s.colour.set_stage("grade", Some(warm), 0, None);
        let mut steps = s.pipeline_steps(None);
        assert_eq!(steps.iter().map(|st| st.owner.as_str()).collect::<Vec<_>>(), ["view", "colour", "transforms"], "the stages sort them");
        let FrameStep::Cpu(step) = &mut steps[2].step else { panic!("the plugin's own") };
        let mut frame = Frame::new(1, 1, PixelFormat::Rgba, vec![0; 4]);
        step.process(&mut frame, &NoCtx);
        assert_eq!(frame.data[0], 1);
    }

    #[test]
    fn the_picture_is_hidden_while_a_step_that_hides_is_in_the_pipeline_and_a_photo_can_ask_to_be_skipped() {
        use serde_json::json;
        use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep};
        use telescope_core::host::HostStills;
        struct Step(bool);
        impl CpuStep for Step {
            fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
            fn hides(&self) -> bool {
                self.0
            }
        }
        struct Panics;
        impl CpuStep for Panics {
            fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
            fn hides(&self) -> bool {
                panic!("no");
            }
        }
        struct Gives(&'static str, FrameStage, Option<Box<dyn CpuStep>>);
        impl Plugin for Gives {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: self.0.into(), frame_stage: self.1, ..Default::default() }
            }
            fn frame_step(&mut self) -> Option<FrameStep> {
                self.2.take().map(FrameStep::Cpu)
            }
        }
        let hidden_with = |stage: FrameStage, step: Box<dyn CpuStep>| {
            let mut s = state();
            s.register(Box::new(Gives("gives", stage, Some(step)))).unwrap();
            s.pipeline_steps(Some("phone"));
            s
        };
        let plain = hidden_with(FrameStage::Effects, Box::new(Step(false)));
        assert!(!plain.picture_hidden_for("phone"), "a step that hides nothing");
        for stage in [FrameStage::Privacy, FrameStage::Cover, FrameStage::Background] {
            assert!(hidden_with(stage, Box::new(Step(false))).picture_hidden_for("phone"), "{stage:?}");
        }
        assert!(hidden_with(FrameStage::Effects, Box::new(Step(true))).picture_hidden_for("phone"), "a step that says it hides");
        // One that can't say counts as hiding, and a hiding plugin that panics closes Telescope
        static CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let mut s = state();
        s.hiding_panic = |_, _| CLOSED.store(true, std::sync::atomic::Ordering::SeqCst);
        s.register(Box::new(Gives("gives", FrameStage::Effects, Some(Box::new(Panics))))).unwrap();
        s.pipeline_steps(Some("phone"));
        assert!(CLOSED.load(std::sync::atomic::Ordering::SeqCst), "one that can't say counts as hiding");
        // Steps built for no stream in particular answer for one whose own aren't built yet
        let mut s = state();
        s.register(Box::new(Gives("gives", FrameStage::Privacy, Some(Box::new(Step(false)))))).unwrap();
        s.pipeline_steps(None);
        assert!(s.picture_hidden_for("other"));
        // The steps going away lets the picture out again
        s.pipeline_steps(None);
        assert!(!s.picture_hidden_for("other"), "the plugin has no step any more");

        // A photo that said to check is skipped while hidden, any other is taken
        let hidden = hidden_with(FrameStage::Privacy, Box::new(Step(false)));
        assert!(hidden.still_skipped(&json!({"skip_if_hidden": true}), "phone"));
        assert!(!hidden.still_skipped(&json!({}), "phone"), "Photos doesn't ask");
        assert!(!hidden.still_skipped(&json!({"skip_if_hidden": false}), "phone"));
        assert!(!plain.still_skipped(&json!({"skip_if_hidden": true}), "phone"));
        // And the host's own answer for a stream, asked about it by id or as the panels' one
        let mut hidden = hidden;
        hidden.streams.source_id = Some("phone".into());
        let ctx = crate::host::Ctx { state: &mut hidden, me: "phototrap".into() };
        assert!(ctx.picture_hidden(Some("phone")) && ctx.picture_hidden(None));
    }

    #[test]
    fn only_the_built_in_background_is_exempt_from_stage_hiding() {
        use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep};
        struct Comfort;
        impl CpuStep for Comfort {
            fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
        }
        struct Gives(PluginInfo);
        impl Plugin for Gives {
            fn info(&self) -> PluginInfo {
                self.0.clone()
            }
            fn setup(&mut self, host: &mut dyn telescope_core::Host) {
                host.set_hiding(&self.0.name, None, false);
            }
            fn frame_step(&mut self) -> Option<FrameStep> {
                Some(FrameStep::Cpu(Box::new(Comfort)))
            }
        }
        for (name, added, stage, hidden) in [
            ("background", false, FrameStage::Background, false),
            ("background", true, FrameStage::Background, true),
            ("chroma_key", false, FrameStage::Background, true),
            ("privacy", false, FrameStage::Privacy, true),
            ("cover", false, FrameStage::Cover, true),
        ] {
            let mut s = state();
            s.hiding_panic = record_close;
            CLOSED.with(|c| c.borrow_mut().clear());
            s.streams.source_id = Some("a".into());
            s.register(Box::new(Gives(PluginInfo { name: name.into(), added, device_local: true, frame_stage: stage, ..Default::default() }))).unwrap();
            s.pipeline_steps(None);
            s.pipeline_steps(Some("b"));
            assert!(!s.picture_hidden_for("a"), "{name}: the shown stream explicitly reported false");
            assert_eq!(s.picture_hidden_for("b"), hidden, "{name}: another stream has not reported");
            assert_eq!(s.hidden_flag("b").load(std::sync::atomic::Ordering::SeqCst), hidden, "{name}: capture flag");
            assert_eq!(s.still_skipped(&json!({"skip_if_hidden": true}), "b"), hidden, "{name}: unattended photo");
            s.stop_after_panic(0, "boom");
            assert_eq!(CLOSED.with(|c| !c.borrow().is_empty()), hidden, "{name}: panic fallback");
            assert_eq!(s.stopped().contains_key(name), !hidden, "{name}: comfort effects stop normally");
        }
    }

    #[test]
    fn hiding_is_what_plugins_say_and_a_hiding_step_that_never_says_counts_as_hiding() {
        use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep};
        struct Step;
        impl CpuStep for Step {
            fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
        }
        struct Gives(&'static str, FrameStage);
        impl Plugin for Gives {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: self.0.into(), frame_stage: self.1, ..Default::default() }
            }
            fn frame_step(&mut self) -> Option<FrameStep> {
                Some(FrameStep::Cpu(Box::new(Step)))
            }
        }
        let mut s = state();
        s.register(Box::new(Gives("blur", FrameStage::Privacy))).unwrap();
        s.register(Box::new(Gives("cover", FrameStage::Cover))).unwrap();
        s.pipeline_steps(Some("phone"));
        assert!(s.picture_hidden_for("phone"), "steps that have never said");
        s.set_hiding("blur", None, false);
        assert!(s.picture_hidden_for("phone"), "the other has not said yet");
        s.set_hiding("cover", None, false);
        assert!(!s.picture_hidden_for("phone") && !s.picture_hidden_for("other"), "both said they're idle, whatever steps they hand out");
        // A flag handed to a photo's thread follows every change at once, with no step made again in between
        let flag = s.hidden_flag("phone");
        assert!(!flag.load(std::sync::atomic::Ordering::SeqCst));
        s.set_hiding("cover", None, true);
        assert!(flag.load(std::sync::atomic::Ordering::SeqCst) && s.picture_hidden_for("other"), "every stream");
        s.set_hiding("cover", Some("phone"), false);
        assert!(!flag.load(std::sync::atomic::Ordering::SeqCst) && s.picture_hidden_for("other"), "just not this one");
        s.set_hiding("blur", Some("phone"), true);
        assert!(flag.load(std::sync::atomic::Ordering::SeqCst), "another owner hides this one");
        s.set_hiding("blur", Some("phone"), false);
        s.set_hiding("cover", None, false);
        assert!(!flag.load(std::sync::atomic::Ordering::SeqCst) && !s.picture_hidden_for("other"));
        // A flag no photo holds any more is dropped
        drop(flag);
        s.set_hiding("cover", None, true);
        assert!(s.hidden_flags.is_empty());
    }

    #[test]
    fn a_plugin_that_stops_leaves_no_stream_hidden_behind_it() {
        use std::sync::atomic::Ordering::SeqCst;
        let mut s = state();
        s.register(stub("background", json!({}), true).0).unwrap();
        let (a, b) = (s.hidden_flag("a"), s.hidden_flag("b"));
        s.set_hiding("background", Some("a"), true);
        s.set_hiding("background", Some("b"), true);
        // Its shutdown only reports on the shown stream, b
        s.set_hiding("background", Some("b"), false);
        assert!(a.load(SeqCst));
        crate::host::Ctx { state: &mut s, me: "background".into() }.release_everything();
        assert!(!a.load(SeqCst) && !b.load(SeqCst) && !s.picture_hidden_for("a"), "a isn't left hidden by a plugin that's gone");
    }

    #[test]
    fn one_streams_hiding_report_never_clears_anothers() {
        use std::sync::atomic::Ordering::SeqCst;
        let mut s = state();
        s.register(stub("background", json!({}), true).0).unwrap();
        s.register(stub("blur", json!({}), false).0).unwrap();
        let (a, b) = (s.hidden_flag("a"), s.hidden_flag("b"));
        // A device-local plugin hides the stream the panels show
        s.streams.source_id = Some("a".into());
        s.set_hiding("background", None, true);
        s.set_hiding("blur", None, false);
        assert!(a.load(SeqCst) && s.picture_hidden_for("a"));
        // The panels move to a stream it isn't hiding: its report is about that one
        s.streams.source_id = Some("b".into());
        s.set_hiding("background", None, false);
        assert!(a.load(SeqCst) && s.picture_hidden_for("a"), "a keeps its own");
        assert!(!b.load(SeqCst) && !s.picture_hidden_for("b"), "b is not hidden");
        // One that hides every stream still does, and ends for all of them
        s.set_hiding("blur", None, true);
        assert!(a.load(SeqCst) && b.load(SeqCst));
        s.set_hiding("blur", None, false);
        assert!(a.load(SeqCst) && !b.load(SeqCst), "blur ending leaves a to background");
        // Back on a: what it says there is a's
        s.streams.source_id = Some("a".into());
        s.set_hiding("background", None, true);
        s.set_hiding("background", None, false);
        assert!(!a.load(SeqCst) && !s.picture_hidden_for("a") && !b.load(SeqCst));
        // A stream asked about by name is unaffected by the others, too
        s.set_hiding("background", Some("b"), true);
        assert!(!a.load(SeqCst) && b.load(SeqCst));
    }

    #[test]
    fn a_device_local_plugin_speaks_only_for_the_camera_it_was_shown_on() {
        use std::sync::atomic::Ordering::SeqCst;
        let mut s = state();
        s.register(stub("background", json!({}), true).0).unwrap();
        let (a, b) = (s.hidden_flag("a"), s.hidden_flag("b"));
        // Both cameras blur: a's report, then the panels move to b, which says the same
        s.streams.source_id = Some("a".into());
        s.set_hiding("background", None, true);
        assert!(a.load(SeqCst) && !b.load(SeqCst), "b has not been told about");
        s.streams.source_id = Some("b".into());
        s.set_hiding("background", None, true);
        assert!(a.load(SeqCst) && b.load(SeqCst));
        // Back on a, its blur goes off: b keeps its own
        s.streams.source_id = Some("a".into());
        s.set_hiding("background", None, false);
        assert!(!a.load(SeqCst) && b.load(SeqCst) && s.picture_hidden_for("b"), "b is still blurred");
        // With no camera shown, the report is everyone's
        s.streams.source_id = None;
        s.set_hiding("background", None, false);
        s.set_hiding("background", Some("b"), true);
        assert!(!a.load(SeqCst) && b.load(SeqCst));
    }

    #[test]
    fn a_cover_that_lands_after_the_photo_was_asked_for_is_seen_when_the_window_dispatches_it() {
        use serde_json::json;
        let mut s = state();
        let options = json!({"skip_if_hidden": true});
        assert!(!s.still_skipped(&options, "phone"));
        // Nothing has rebuilt the steps since: what the plugin said is what counts
        s.set_hiding("wait_screen", None, true);
        assert!(s.still_skipped(&options, "phone"));
        s.set_hiding("wait_screen", None, false);
        assert!(!s.still_skipped(&options, "phone"));
    }

    #[test]
    fn the_motion_detectors_mapping_follows_the_phones_share_of_the_zoom_with_no_view_change() {
        use crate::view_control::Said;
        let mut s = state();
        let said = s.vc.stream_start(&s.view, "phone", 1, 0.0);
        s.apply_said(said);
        // 2x zoom asked for before the phone has told what it can do: this computer crops all of it
        let said = s.vc.set_view(&mut s.view, &telescope_core::host::ViewChange { zoom: Some(2.0), ..Default::default() }, 0.0);
        s.apply_said(said);
        assert_eq!(s.upright.lock().unwrap().sent, (1.0, 0.0, 0.0));
        let caps = json!({"cameras": [{"id": "0", "current": true, "zoomRatioMax": 10.0, "cropZoomMax": 4.0, "freeformCrop": true}]});
        let said = s.vc.phone_state(&s.view, &caps, 0.0);
        assert!(said.iter().any(|x| matches!(x, Said::Zoom(_))), "the phone takes the crop over");
        assert!(!said.iter().any(|x| matches!(x, Said::Emit(Event::ViewChanged { .. }))), "and the view itself didn't change");
        s.apply_said(said);
        let sent = telescope_core::view::phone_window(s.vc.sent().as_ref());
        assert!(sent.0 > 1.0, "{sent:?}");
        assert_eq!(s.upright.lock().unwrap().sent, sent);
        let (x, y) = s.upright.lock().unwrap().for_frame(1920, 1080).to_shown(0.0, 0.0);
        assert!(x > 0.1 && y > 0.1, "the corner of what the phone sends is well inside the picture: ({x}, {y})");
        // A stream that starts again is sent nothing yet, which is no crop either
        let said = s.vc.stream_start(&s.view, "phone", 2, 1.0);
        s.apply_said(said);
        assert_eq!(s.upright.lock().unwrap().sent, telescope_core::view::phone_window(s.vc.sent().as_ref()));
    }

    #[test]
    fn a_plugin_can_put_steps_at_two_stages_and_one_in_the_colour_stage_follows_the_grade() {
        use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep};
        struct Nothing;
        impl CpuStep for Nothing {
            fn process(&mut self, _frame: &mut Frame, _ctx: &dyn FrameContext) {}
        }
        struct TwoPlaces;
        impl Plugin for TwoPlaces {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "filters".into(), frame_stage: FrameStage::Effects, ..Default::default() }
            }
            fn staged_frame_steps(&mut self) -> Vec<(FrameStage, FrameStep)> {
                vec![(FrameStage::Colour, FrameStep::Cpu(Box::new(Nothing))), (FrameStage::Effects, FrameStep::Cpu(Box::new(Nothing)))]
            }
        }
        let mut s = state();
        s.register(Box::new(TwoPlaces)).unwrap();
        let warm = telescope_core::host::ColourStage::new(2, Arc::new(telescope_core::colour::lattice(2).iter().map(|v| v * 0.5).collect()), None);
        s.colour.set_stage("grade", Some(warm), 0, None);
        let steps = s.pipeline_steps(None);
        // as the pipeline sorts them by stage (a stable sort), the grade's own step comes ahead of the plugin's in the colour stage
        let mut sorted: Vec<(&str, FrameStage)> = steps.iter().map(|st| (st.owner.as_str(), st.stage)).collect();
        sorted.sort_by_key(|(_, stage)| *stage);
        assert_eq!(sorted, [("filters", FrameStage::Effects), ("colour", FrameStage::Colour), ("filters", FrameStage::Colour)]);
    }

    #[test]
    fn comparing_keeps_the_picture_from_before_the_colour_step_for_the_preview() {
        use telescope_core::frame::{FrameStage, FrameStep};
        let mut s = state();
        let warm = telescope_core::host::ColourStage::new(2, Arc::new(telescope_core::colour::lattice(2).iter().map(|v| v * 0.5).collect()), None);
        s.colour.set_stage("grade", Some(warm), 0, None);
        let colour = |s: &mut State| s.pipeline_steps(None).into_iter().filter(|st| st.stage == FrameStage::Colour)
            .map(|st| matches!(st.step, FrameStep::Cpu(_))).collect::<Vec<bool>>();
        assert_eq!(colour(&mut s), [false], "just the LUT");
        s.colour.set_split("grade", Some(0.5));
        let mut steps = s.pipeline_steps(None);
        let keep = steps.iter_mut().find(|st| st.stage == FrameStage::Colour && matches!(st.step, FrameStep::Cpu(_))).unwrap();
        let FrameStep::Cpu(step) = &mut keep.step else { unreachable!() };
        let mut frame = telescope_core::Frame::new(2, 1, telescope_core::frame::PixelFormat::Rgba, vec![9; 8]);
        step.process(&mut frame, &NoCtx);
        assert!(s.colour_before.lock().unwrap().is_empty(), "kept for now: the frame isn't out yet");
        let last = steps.iter_mut().find(|st| st.stage == FrameStage::Final).expect("the step that lets the preview have it");
        let FrameStep::Cpu(publish) = &mut last.step else { unreachable!() };
        publish.process(&mut frame, &NoCtx);
        assert_eq!(*s.colour_before.lock().unwrap()["p1"].data, vec![9; 8]);
        s.colour.set_split("grade", None);
        assert_eq!(colour(&mut s), [false]);
        assert!(s.colour_before.lock().unwrap().is_empty());
        // The last stage going away while comparing lets the picture go too
        s.colour.set_split("grade", Some(0.5));
        let mut steps = s.pipeline_steps(None);
        let keep = steps.iter_mut().find(|st| st.stage == FrameStage::Colour && matches!(st.step, FrameStep::Cpu(_))).unwrap();
        let FrameStep::Cpu(step) = &mut keep.step else { unreachable!() };
        step.process(&mut frame, &NoCtx);
        s.colour.set_stage("grade", None, 0, None);
        assert!(colour(&mut s).is_empty());
        assert!(s.colour_before.lock().unwrap().is_empty());
    }

    #[test]
    fn comparing_shows_no_clear_picture_while_a_step_that_hides_things_is_hiding() {
        use telescope_core::frame::{CpuStep, FrameContext, FrameStage, FrameStep, Touch};
        struct Hides(Arc<std::sync::atomic::AtomicBool>);
        impl CpuStep for Hides {
            fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
                if self.0.load(std::sync::atomic::Ordering::Relaxed) { Touch::Changes } else { Touch::Nothing }
            }
            fn process(&mut self, _frame: &mut telescope_core::Frame, _ctx: &dyn FrameContext) {}
        }
        struct Privacy(Arc<std::sync::atomic::AtomicBool>);
        impl Plugin for Privacy {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "privacy".into(), frame_stage: FrameStage::Privacy, ..Default::default() }
            }
            fn frame_step(&mut self) -> Option<FrameStep> {
                Some(FrameStep::Cpu(Box::new(Hides(self.0.clone()))))
            }
        }
        let hiding = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut s = state();
        s.register(Box::new(Privacy(hiding.clone()))).unwrap();
        let warm = telescope_core::host::ColourStage::new(2, Arc::new(telescope_core::colour::lattice(2).iter().map(|v| v * 0.5).collect()), None);
        s.colour.set_stage("grade", Some(warm), 0, None);
        s.colour.set_split("grade", Some(0.5));
        let mut steps = s.pipeline_steps(None);
        let run = |steps: &mut Vec<telescope_core::media::StagedStep>| {
            let mut frame = telescope_core::Frame::new(2, 1, telescope_core::frame::PixelFormat::Rgba, vec![9; 8]);
            for st in steps.iter_mut().filter(|st| matches!(st.step, FrameStep::Cpu(_))) {
                let FrameStep::Cpu(step) = &mut st.step else { unreachable!() };
                if step.touch(&NoCtx) != Touch::Nothing {
                    step.process(&mut frame, &NoCtx);
                }
            }
        };
        run(&mut steps);
        assert!(s.colour_before.lock().unwrap().contains_key("p1"), "nothing hidden: the compare has its picture");
        hiding.store(true, std::sync::atomic::Ordering::Relaxed);
        run(&mut steps);
        assert!(s.colour_before.lock().unwrap().is_empty(), "hiding: no clear picture left for the compare");
    }

    /// A compare with a Privacy plugin whose step says whether it has something to hide, and the steps the pipeline would run for a stream in the order it would.
    fn comparing_with_privacy(hiding: Arc<std::sync::atomic::AtomicBool>) -> (State, Vec<telescope_core::media::StagedStep>) {
        use telescope_core::frame::{CpuStep, FrameContext, FrameStage, FrameStep, Touch};
        struct Hides(Arc<std::sync::atomic::AtomicBool>);
        impl CpuStep for Hides {
            fn hides(&self) -> bool {
                true
            }
            fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
                if self.0.load(std::sync::atomic::Ordering::Relaxed) { Touch::Changes } else { Touch::Nothing }
            }
            fn process(&mut self, _frame: &mut telescope_core::Frame, _ctx: &dyn FrameContext) {}
        }
        struct Privacy(Arc<std::sync::atomic::AtomicBool>);
        impl Plugin for Privacy {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "privacy".into(), frame_stage: FrameStage::Privacy, ..Default::default() }
            }
            fn frame_step(&mut self) -> Option<FrameStep> {
                Some(FrameStep::Cpu(Box::new(Hides(self.0.clone()))))
            }
        }
        let mut s = state();
        s.register(Box::new(Privacy(hiding))).unwrap();
        let warm = telescope_core::host::ColourStage::new(2, Arc::new(telescope_core::colour::lattice(2).iter().map(|v| v * 0.5).collect()), None);
        s.colour.set_stage("grade", Some(warm), 0, None);
        s.colour.set_split("grade", Some(0.5));
        let steps = s.pipeline_steps(None);
        (s, steps)
    }

    #[test]
    fn comparing_shows_nothing_of_a_frame_until_the_steps_that_hide_things_have_had_their_turn() {
        use telescope_core::frame::{FrameStage, FrameStep};
        let hiding = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (s, mut steps) = comparing_with_privacy(hiding);
        let mut frame = telescope_core::Frame::new(2, 1, telescope_core::frame::PixelFormat::Rgba, vec![9; 8]);
        let mut in_order = Vec::new();
        for st in steps.iter_mut().filter(|st| matches!(st.step, FrameStep::Cpu(_))) {
            let FrameStep::Cpu(step) = &mut st.step else { unreachable!() };
            if step.touch(&NoCtx) != telescope_core::frame::Touch::Nothing {
                step.process(&mut frame, &NoCtx);
            }
            // What the preview could read between any two steps
            in_order.push((st.stage, s.colour_before.lock().unwrap().contains_key("p1")));
        }
        assert!(in_order.iter().any(|(stage, _)| *stage == FrameStage::Final), "the last step ran");
        assert!(in_order.iter().all(|(_, shown)| !shown), "the clear picture is never on show: {in_order:?}");
    }

    #[test]
    fn comparing_shows_nothing_once_a_step_that_hides_things_is_skipped() {
        use telescope_core::frame::{FrameStage, FrameStep};
        let hiding = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, mut steps) = comparing_with_privacy(hiding);
        let mut frame = telescope_core::Frame::new(2, 1, telescope_core::frame::PixelFormat::Rgba, vec![9; 8]);
        let mut run = |steps: &mut Vec<telescope_core::media::StagedStep>, skipped: bool| {
            for st in steps.iter_mut().filter(|st| matches!(st.step, FrameStep::Cpu(_))) {
                let FrameStep::Cpu(step) = &mut st.step else { unreachable!() };
                if skipped && st.stage == FrameStage::Privacy {
                    // The pipeline sends a dark frame for a skipped one and doesn't ask it to touch or process
                    step.withheld(&NoCtx);
                } else if step.touch(&NoCtx) != telescope_core::frame::Touch::Nothing {
                    step.process(&mut frame, &NoCtx);
                }
            }
        };
        run(&mut steps, false);
        assert!(s.colour_before.lock().unwrap().contains_key("p1"), "nothing hidden: the compare has its picture");
        run(&mut steps, true);
        assert!(s.colour_before.lock().unwrap().is_empty(), "skipped, so dark: no clear picture left for the compare");
        run(&mut steps, true);
        assert!(s.colour_before.lock().unwrap().is_empty(), "and none stored again every frame after");
    }

    #[test]
    fn a_stream_gets_the_colour_stages_set_for_it_alone() {
        use telescope_core::frame::FrameStage;
        let mut s = state();
        let warm = telescope_core::host::ColourStage::new(2, Arc::new(telescope_core::colour::lattice(2).iter().map(|v| v * 0.5).collect()), None);
        s.colour.set_stage("grade", Some(warm), 0, Some("tablet"));
        let colour = |s: &mut State, id| s.pipeline_steps(id).into_iter().filter(|st| st.stage == FrameStage::Colour).count();
        assert_eq!(colour(&mut s, Some("tablet")), 1);
        assert_eq!(colour(&mut s, Some("phone")), 0);
        assert_eq!(colour(&mut s, None), 0);
    }

    /// A plugin named like the Camera card that writes down every event it hears.
    fn camera_hears(s: &mut State) -> Arc<Mutex<Vec<Event>>> {
        struct Hears(Arc<Mutex<Vec<Event>>>);
        impl Plugin for Hears {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "camera_control".into(), ..Default::default() }
            }
            fn on_event(&mut self, _host: &mut dyn telescope_core::host::Host, event: &Event) {
                self.0.lock().unwrap().push(event.clone());
            }
        }
        let heard = Arc::new(Mutex::new(Vec::new()));
        s.register(Box::new(Hears(heard.clone()))).unwrap();
        s.streams.source_id = Some("phone".into());
        s.frame_sizes.lock().unwrap().insert("phone".into(), (160, 90));
        heard
    }

    fn focus_points(heard: &Arc<Mutex<Vec<Event>>>) -> Vec<(f32, f32)> {
        heard.lock().unwrap().iter().filter_map(|e| if let Event::FocusPoint { x, y } = e { Some(((x * 1000.0).round() / 1000.0, (y * 1000.0).round() / 1000.0)) } else { None }).collect()
    }

    #[test]
    fn a_picked_point_lands_on_the_phone_frame_through_a_stretch() {
        let mut s = state();
        let heard = camera_hears(&mut s);
        s.view.stretch_x = 2.0;
        s.view.stretch_y = 0.5;
        s.bus.emit(Event::FocusPointPicked { x: 0.25, y: 0.6 });
        s.deliver_events();
        assert_eq!(focus_points(&heard), [(0.375, 0.7)]);
    }

    #[test]
    fn a_picked_point_lands_where_the_lens_correction_read_it_in_the_phone_frame() {
        let mut s = state();
        let heard = camera_hears(&mut s);
        s.view.distortion = 1.0;
        s.bus.emit(Event::FocusPointPicked { x: 0.95, y: 0.95 });
        s.bus.emit(Event::FocusPointPicked { x: 0.5, y: 0.5 });
        s.deliver_events();
        // About 229 pixels of a 1920 wide frame in from where the preview shows it
        assert_eq!(focus_points(&heard), [(0.846, 0.846), (0.5, 0.5)]);
    }

    #[test]
    fn vision_hears_the_lens_correction_so_people_land_in_the_corrected_picture() {
        let mut s = state();
        s.view.distortion = 0.8;
        s.follow_view_for_vision();
        let up = s.upright.lock().unwrap().for_frame(1920, 1080);
        assert_eq!(up.distortion, 0.8_f32 as f64);
        let (x, y) = up.to_shown(0.95, 0.95);
        assert!(x > 0.95 && y > 0.95, "a corner the barrel pulled in goes back out: ({x}, {y})");
        s.view.distortion = 0.0;
        s.follow_view_for_vision();
        assert_eq!(s.upright.lock().unwrap().for_frame(1920, 1080).to_shown(0.95, 0.95), (0.95, 0.95));
    }

    #[test]
    fn a_picked_point_only_undoes_the_desktop_share_of_the_zoom() {
        let mut s = state();
        let heard = camera_hears(&mut s);
        let caps = json!({"cameras": [{"id": "0", "current": true, "zoomRatioMax": 10.0, "cropZoomMax": 4.0, "freeformCrop": true}]});
        s.vc.set_view(&mut s.view, &telescope_core::host::ViewChange { zoom: Some(1.0), ..Default::default() }, 0.0);
        s.vc.stream_start(&s.view, "phone", 1, 0.0);
        s.vc.phone_state(&s.view, &caps, 0.0);
        s.vc.set_view(&mut s.view, &telescope_core::host::ViewChange { zoom: Some(2.0), ..Default::default() }, 0.0);
        s.bus.emit(Event::FocusPointPicked { x: 0.25, y: 0.5 });
        s.deliver_events();
        // The phone zoomed, so the point is already in its stream frame
        assert_eq!(focus_points(&heard), [(0.25, 0.5)]);
    }

    #[test]
    fn a_plugin_failing_on_its_pick_doesnt_break_the_next_pick() {
        use telescope_core::host::PickKind;
        struct Breaks;
        impl Plugin for Breaks {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "breaks".into(), ..Default::default() }
            }
            fn on_event(&mut self, _host: &mut dyn telescope_core::host::Host, event: &Event) {
                if matches!(event, Event::PreviewPicked { .. }) {
                    panic!("plugin bug");
                }
            }
        }
        let mut s = state();
        s.register(Box::new(Breaks)).unwrap();
        s.overlays.start_pick(&s.view, "breaks", PickKind::Point, None, "", &s.bus);
        assert!(s.overlays.press(&s.view, 0.5, 0.5, (640.0, 360.0)));
        s.overlays.release(&s.view, &s.bus);
        s.deliver_events();
        assert_eq!(s.overlays.picking(), "");
        s.overlays.start_pick(&s.view, "breaks", PickKind::Point, None, "", &s.bus);
        assert_eq!(s.overlays.picking(), "breaks");
    }

    #[test]
    fn nothing_to_pick_on_without_a_preview() {
        use telescope_core::host::{HostPreview, PickKind};
        struct Preview;
        impl Plugin for Preview {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "preview".into(), ..Default::default() }
            }
        }
        let mut s = state();
        let ask = |s: &mut State| crate::host::Ctx { state: s, me: "Test".into() }.pick_on_preview("Test", PickKind::Point, None, "");
        assert!(!ask(&mut s), "no stream, no preview");
        s.streams.streaming = true;
        assert!(!ask(&mut s), "a stream the preview card isn't there to show");
        s.register(Box::new(Preview)).unwrap();
        assert!(ask(&mut s));
        assert_eq!(s.overlays.picking(), "Test");
    }

    struct Watches(Arc<Mutex<Vec<Event>>>);

    impl Plugin for Watches {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: "watcher".into(), ..Default::default() }
        }
        fn on_event(&mut self, _host: &mut dyn telescope_core::host::Host, event: &Event) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    #[test]
    fn a_flip_tells_vision_and_starts_motion_over() {
        use telescope_core::frame::{Frame, PixelFormat};
        use telescope_core::host::HostMotion;
        use telescope_core::vision::Turn;
        let room = |square: bool| {
            let mut data = vec![100u8; 320 * 180 * 4];
            data.chunks_mut(4).for_each(|p| p[3] = 255);
            if square {
                for y in 60..120 {
                    data[(y * 320 + 220) * 4..(y * 320 + 280) * 4].chunks_mut(4).for_each(|p| p[..3].fill(230));
                }
            }
            Frame::new(320, 180, PixelFormat::Rgba, data)
        };
        let mut s = state();
        let (heard, pack) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Pack::default()));
        s.vision = Some(pack.clone());
        s.register(Box::new(Watches(heard.clone()))).unwrap();
        s.streams.source_id = Some("phone".into());
        crate::host::Ctx { state: &mut s, me: "watcher".into() }.watch_motion("watcher", &[[0.0, 0.0, 1.0, 1.0]], 5);
        for i in 1..=10 {
            s.motion.feed(&room(false), "phone", None, i as f64 * 0.1);
        }
        s.follow_view_for_vision();
        assert!(pack.1.lock().unwrap().is_empty(), "nothing turned");
        s.view.flip_h = true;
        s.follow_view_for_vision();
        let flipped = Turn { flip_h: true, ..Turn::default() };
        assert_eq!(*pack.1.lock().unwrap(), [(Turn::default(), flipped)]);
        s.motion.feed(&room(true), "phone", None, 1.1);
        s.deliver_events();
        assert!(!heard.lock().unwrap().iter().any(|e| matches!(e, Event::MotionSeen { .. })), "the picture turned: that's nothing moving");
    }

    #[test]
    fn a_plugin_watching_for_motion_hears_which_zone_moved() {
        use telescope_core::frame::{Frame, PixelFormat};
        use telescope_core::host::HostMotion;
        let room = |square: bool| {
            let mut data = vec![100u8; 320 * 180 * 4];
            for p in data.chunks_mut(4) {
                p[3] = 255;
            }
            if square {
                for y in 60..120 {
                    for x in 220..280 {
                        data[(y * 320 + x) * 4..(y * 320 + x) * 4 + 3].fill(230);
                    }
                }
            }
            Frame::new(320, 180, PixelFormat::Rgba, data)
        };
        let mut s = state();
        let heard = Arc::new(Mutex::new(Vec::new()));
        s.register(Box::new(Watches(heard.clone()))).unwrap();
        s.streams.source_id = Some("phone".into());
        crate::host::Ctx { state: &mut s, me: "watcher".into() }.watch_motion("watcher", &[[0.0, 0.0, 0.4, 1.0], [0.6, 0.0, 1.0, 1.0]], 5);
        for i in 1..=10 {
            s.motion.feed(&room(false), "phone", None, i as f64 * 0.1);
        }
        s.motion.feed(&room(true), "phone", None, 1.1);
        s.deliver_events();
        let seen: Vec<Event> = heard.lock().unwrap().iter().filter(|e| matches!(e, Event::MotionSeen { .. })).cloned().collect();
        assert_eq!(seen, [Event::MotionSeen { source_id: "phone".into(), owner: "watcher".into(), zones: vec![1] }]);
        assert_eq!(crate::host::Ctx { state: &mut s, me: "watcher".into() }.motion("watcher"), [1]);
    }
    #[test]
    fn a_setting_has_a_revision_that_moves_with_each_edit_even_when_the_value_comes_back() {
        use telescope_core::host::HostSettings;
        struct Dial(Arc<Mutex<Value>>);
        impl Plugin for Dial {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "dial".into(), ..Default::default() }
            }
            fn get_config(&self) -> Value {
                self.0.lock().unwrap().clone()
            }
            fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
                *self.0.lock().unwrap() = cfg.clone();
                Ok(())
            }
        }
        let cfg = Arc::new(Mutex::new(json!({"a": 1, "b": 1})));
        let mut s = state();
        s.register(Box::new(Dial(cfg.clone()))).unwrap();
        let rev = |s: &mut State, key: &str| crate::host::Ctx { state: s, me: String::new() }.setting_revision("dial", key);
        assert_eq!((rev(&mut s, "a"), rev(&mut s, "nope")), (0, 0), "nothing moved yet");
        // A tool writes a preset: the plugin never saved for it, and it counts all the same
        crate::host::Ctx { state: &mut s, me: "mcp".into() }.apply_preset("dial", &json!({"a": 2, "b": 1}));
        let by_tool = rev(&mut s, "a");
        assert!(by_tool > 0 && rev(&mut s, "b") == 0, "only what moved");
        // The person edits it twice, and the second lands on the tool's value again
        let edit = |s: &mut State, a: i64| {
            *cfg.lock().unwrap() = json!({"a": a, "b": 1});
            s.call(0, |_, host| host.schedule_save());
        };
        edit(&mut s, 3);
        let first = rev(&mut s, "a");
        edit(&mut s, 2);
        let second = rev(&mut s, "a");
        assert!(by_tool < first && first < second, "{by_tool} {first} {second}");
        assert_eq!(cfg.lock().unwrap()["a"], json!(2), "the same value as the tool left, and told apart by the revision");
        // A save that changed nothing, or an edit of another setting, leaves it
        s.call(0, |_, host| host.schedule_save());
        *cfg.lock().unwrap() = json!({"a": 2, "b": 5});
        s.call(0, |_, host| host.schedule_save());
        assert_eq!((rev(&mut s, "a"), rev(&mut s, "b") > second), (second, true));
    }
    #[test]
    fn a_camera_has_its_own_revisions_so_editing_another_camera_does_not_move_them() {
        use telescope_core::host::HostSettings;
        struct Dial(Arc<Mutex<Value>>);
        impl Plugin for Dial {
            fn info(&self) -> PluginInfo {
                PluginInfo { name: "dial".into(), device_local: true, ..Default::default() }
            }
            fn get_config(&self) -> Value {
                self.0.lock().unwrap().clone()
            }
            fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
                *self.0.lock().unwrap() = cfg.clone();
                Ok(())
            }
        }
        let cfg = Arc::new(Mutex::new(json!({"a": 1})));
        let mut s = state();
        s.register(Box::new(Dial(cfg.clone()))).unwrap();
        let rev = |s: &mut State| crate::host::Ctx { state: s, me: String::new() }.setting_revision("dial", "a");
        let switch_to = |s: &mut State, camera: &str, config: Value| {
            s.streams.source_id = Some(camera.into());
            *cfg.lock().unwrap() = config;
            s.rebaseline(0, Some(camera.into()));
        };
        switch_to(&mut s, "cam-a", json!({"a": 1}));
        crate::host::Ctx { state: &mut s, me: "mcp".into() }.apply_preset("dial", &json!({"a": 2}));
        let left_by_tool = rev(&mut s);
        assert!(left_by_tool > 0);
        // The person edits the other camera's setting of the same name
        switch_to(&mut s, "cam-b", json!({"a": 1}));
        assert_eq!(rev(&mut s), 0, "nothing moved on this camera");
        *cfg.lock().unwrap() = json!({"a": 5});
        s.call(0, |_, host| host.schedule_save());
        assert!(rev(&mut s) > left_by_tool);
        // Back on the first camera its revision is as the tool left it
        switch_to(&mut s, "cam-a", json!({"a": 2}));
        assert_eq!(rev(&mut s), left_by_tool);
    }

    fn quiet_state() -> State {
        let mut s = state();
        s.idle = Arc::new(crate::idle::IdleGate::after(0));
        s.shown = Some(false);
        s.sync_idle();
        s.idle.reliable(true);
        s.idle.watched(false);
        s
    }

    #[test]
    fn the_main_camera_goes_quiet_only_with_the_window_out_of_sight_and_nothing_asking_for_frames() {
        use telescope_core::host::{HostFrames, HostMotion};
        let mut s = quiet_state();
        assert!(s.idle.idle(), "nobody reads it, nobody looks, nothing asked");
        s.shown = Some(true);
        s.sync_idle();
        assert!(!s.idle.idle(), "the window shows the picture");
        s.shown = None;
        s.sync_idle();
        assert!(!s.idle.idle(), "a window that hasn't said yet counts as seen");
        s.shown = Some(false);
        s.other_windows = true;
        s.sync_idle();
        assert!(!s.idle.idle(), "a pop-out or plugin window shows it too");
        s.other_windows = false;
        s.sync_idle();
        assert!(s.idle.idle());
        let mut ctx = crate::host::Ctx { state: &mut s, me: "qr_reader".into() };
        ctx.want_frames("qr_reader", true);
        assert!(!s.idle.idle(), "a plugin that reads pictures asked");
        let mut ctx = crate::host::Ctx { state: &mut s, me: "qr_reader".into() };
        ctx.want_frames("qr_reader", false);
        assert!(s.idle.idle());
        let mut ctx = crate::host::Ctx { state: &mut s, me: "motion".into() };
        ctx.watch_motion("motion", &[[0.0, 0.0, 1.0, 1.0]], 5);
        assert!(!s.idle.idle(), "a motion watch needs frames");
        let mut ctx = crate::host::Ctx { state: &mut s, me: "motion".into() };
        ctx.watch_motion("motion", &[], 5);
        assert!(s.idle.idle());
        let mut ctx = crate::host::Ctx { state: &mut s, me: "auto_frame".into() };
        telescope_core::host::HostVision::want_people(&mut ctx, "auto_frame", true);
        assert!(!s.idle.idle(), "finding people reads frames");
        let mut ctx = crate::host::Ctx { state: &mut s, me: "auto_frame".into() };
        ctx.release_everything();
        assert!(s.idle.idle(), "and a plugin that goes away lets go of that too");
    }

    #[test]
    fn frame_history_is_kept_as_long_as_the_plugin_that_wants_the_most_asks() {
        use telescope_core::host::HostFrames;
        use telescope_core::media::{FrameSlot, StreamFrames};
        #[derive(Default)]
        struct Told(Mutex<Vec<usize>>);
        impl telescope_core::host::FrameStore for Told {
            fn stream_frames(&self) -> Vec<telescope_core::frame::StreamFrame> {
                Vec::new()
            }
            fn stream_frame(&self, _: Option<&str>) -> Option<telescope_core::frame::StreamFrame> {
                None
            }
            fn replace_frame(&self, _: Option<&str>, _: telescope_core::Frame, _: u64) -> bool {
                false
            }
        }
        impl StreamFrames for Told {
            fn new_slot(&self) -> Arc<dyn FrameSlot> {
                unreachable!("no streams here")
            }
            fn set_streams(&self, _: Vec<(String, String, Arc<dyn FrameSlot>)>) {}
            fn set_history(&self, frames: usize) {
                self.0.lock().unwrap().push(frames);
            }
        }
        let told = Arc::new(Told::default());
        let mut s = quiet_state();
        s.stream_frames = Some(told.clone());
        crate::host::Ctx { state: &mut s, me: "stereo".into() }.want_frame_history("stereo", 6);
        crate::host::Ctx { state: &mut s, me: "other".into() }.want_frame_history("other", 10);
        crate::host::Ctx { state: &mut s, me: "other".into() }.want_frame_history("other", 0);
        crate::host::Ctx { state: &mut s, me: "stereo".into() }.release_everything();
        assert_eq!(*told.0.lock().unwrap(), [6, 10, 6, 0]);
    }

    #[test]
    fn a_plugin_that_starts_hiding_has_the_streams_behind_the_panels_hide_before_its_frame_steps_are_asked_for() {
        use telescope_core::host::{HostStills, HostUi};
        let mut s = quiet_state();
        s.effects.clear();
        crate::host::Ctx { state: &mut s, me: "privacy".into() }.set_hiding("privacy", None, false);
        assert!(!s.effects.contains(&Effect::HideBehind), "nothing began hiding");
        let mut host = crate::host::Ctx { state: &mut s, me: "privacy".into() };
        host.set_hiding("privacy", None, true);
        host.refresh_frame_step();
        let at = |e: &Effect| s.effects.iter().position(|x| x == e).unwrap();
        assert!(at(&Effect::HideBehind) < at(&Effect::FrameStep("privacy".into())));
    }

    #[test]
    fn a_plugins_preview_redirect_is_kept_per_owner_asks_for_a_repaint_and_goes_with_the_plugin() {
        use std::sync::atomic::Ordering::SeqCst;
        use telescope_core::host::HostPreview;
        let mut s = quiet_state();
        assert_eq!(s.preview_target("b"), None);
        s.preview_dirty.store(false, SeqCst);
        crate::host::Ctx { state: &mut s, me: "stereo".into() }.preview_follows("stereo", Some("b"), Some("a"));
        assert_eq!((s.preview_target("b"), s.preview_target("a")), (Some("a"), None), "only the stream it names");
        s.streams.ids = vec!["a".into(), "b".into()];
        assert!(!s.preview_shows_own("b"), "the shown stream's preview draws another's output");
        assert!(s.preview_shows_own("a"), "a stream nobody redirects");
        s.streams.ids = vec!["b".into()];
        assert!(s.preview_shows_own("b"), "the one it follows has stopped: the preview is on its own picture");
        s.streams.ids = vec!["a".into(), "b".into()];
        crate::host::Ctx { state: &mut s, me: "comp".into() }.preview_composited("comp", Some("a"));
        assert!(!s.preview_shows_own("a"), "a stream whose picture is made of several");
        crate::host::Ctx { state: &mut s, me: "comp".into() }.preview_composited("comp", Some("a"));
        s.streams.source_id = Some("c".into());
        s.sync_preview_source();
        assert!(s.preview_own_told, "the shown stream c is on its own picture");
        s.streams.source_id = Some("a".into());
        s.sync_preview_source();
        assert!(!s.preview_own_told, "moving the panels to the combined stream tells the plugins");
        s.streams.source_id = Some("c".into());
        s.sync_preview_source();
        assert!(s.preview_own_told, "and moving away tells them again");
        crate::host::Ctx { state: &mut s, me: "comp".into() }.preview_composited("comp", None);
        assert!(s.preview_shows_own("a"));
        assert!(s.preview_dirty.swap(false, SeqCst), "the preview draws another picture now");
        crate::host::Ctx { state: &mut s, me: "stereo".into() }.preview_follows("stereo", Some("b"), Some("a"));
        assert!(!s.preview_dirty.load(SeqCst), "the same again changes nothing");
        crate::host::Ctx { state: &mut s, me: "stereo".into() }.preview_follows("stereo", Some("a"), None);
        assert_eq!(s.preview_target("b"), None, "no target takes the owner's redirect off");
        crate::host::Ctx { state: &mut s, me: "stereo".into() }.preview_follows("stereo", Some("b"), Some("a"));
        crate::host::Ctx { state: &mut s, me: "other".into() }.release_everything();
        assert_eq!(s.preview_target("b"), Some("a"), "another plugin going away leaves it");
        crate::host::Ctx { state: &mut s, me: "stereo".into() }.release_everything();
        assert_eq!(s.preview_target("b"), None, "and its own owner going away takes it off");
    }

    #[test]
    fn costly_stats_and_a_plugin_that_goes_away_keep_the_count_honest() {
        use telescope_core::host::{Host, HostFrames};
        let mut s = quiet_state();
        let mut ctx = crate::host::Ctx { state: &mut s, me: "idle-test-stats".into() };
        ctx.want_stats("idle-test-stats", true);
        assert!(!s.idle.idle(), "the Nerd view measures frame steps, which need frames");
        let mut ctx = crate::host::Ctx { state: &mut s, me: "idle-test-stats".into() };
        ctx.want_stats("idle-test-stats", false);
        ctx.want_frames("composite", true);
        ctx.release_everything();
        assert!(!s.frame_wants.contains("stats:idle-test-stats") && s.frame_wants.contains("composite"), "release gives back what that plugin held only");
        let mut ctx = crate::host::Ctx { state: &mut s, me: "composite".into() };
        ctx.release_everything();
        assert!(s.idle.idle());
    }

    #[test]
    fn an_app_opening_the_camera_ends_the_quiet_through_the_event_bus() {
        let mut s = quiet_state();
        assert!(s.idle.idle());
        s.bus.emit(Event::CameraWatched { watched: true });
        s.deliver_events();
        assert!(!s.idle.idle());
        s.bus.emit(Event::CameraWatched { watched: false });
        s.deliver_events();
        assert!(s.idle.idle());
    }

    #[test]
    fn the_watch_going_away_with_its_plugin_ends_the_quiet() {
        use telescope_core::host::Host;
        let mut s = state();
        s.idle = Arc::new(crate::idle::IdleGate::after(0));
        s.shown = Some(false);
        s.sync_idle();
        s.idle.reliable(true);
        crate::host::Ctx { state: &mut s, me: "wait_screen".into() }.emit(Event::CameraWatched { watched: false });
        s.deliver_events();
        assert!(s.idle.idle());
        crate::host::Ctx { state: &mut s, me: "other".into() }.release_everything();
        assert!(s.idle.idle(), "a plugin that never said anything about readers changes nothing");
        crate::host::Ctx { state: &mut s, me: "wait_screen".into() }.release_everything();
        assert!(!s.idle.idle(), "nothing says whether an app reads the camera any more, so it never pauses");
    }

    #[test]
    fn a_watch_report_still_queued_when_its_plugin_stops_doesnt_bring_the_watch_back() {
        use telescope_core::host::Host;
        let mut s = state();
        s.idle = Arc::new(crate::idle::IdleGate::after(0));
        s.shown = Some(false);
        s.sync_idle();
        s.idle.reliable(true);
        crate::host::Ctx { state: &mut s, me: "wait_screen".into() }.emit(Event::CameraWatched { watched: false });
        crate::host::Ctx { state: &mut s, me: "wait_screen".into() }.release_everything();
        s.deliver_events();
        assert!(!s.idle.idle(), "nothing says whether an app reads the camera any more, so it never pauses");
    }

    #[test]
    fn an_unreliable_watch_never_pauses_and_the_outputs_decide_which_it_is() {
        struct Cams(std::sync::atomic::AtomicBool);
        impl telescope_core::output::Outputs for Cams {
            fn slots(&self) -> Vec<telescope_core::output::Slot> {
                Vec::new()
            }
            fn formats(&self) -> Vec<telescope_core::PixelFormat> {
                Vec::new()
            }
            fn open(&self, _: u32, _: u32, _: u32, _: u32) -> Result<Box<dyn telescope_core::output::VideoOutput>, String> {
                Err(String::new())
            }
            fn watch_reliable(&self) -> bool {
                self.0.load(std::sync::atomic::Ordering::SeqCst)
            }
        }
        let cams = Arc::new(Cams(false.into()));
        let mut s = quiet_state();
        s.outputs = Some(cams.clone());
        s.sync_idle();
        assert!(!s.idle.idle(), "the /proc scan or UnityCapture may have missed a reader");
        cams.0.store(true, std::sync::atomic::Ordering::SeqCst);
        s.sync_idle();
        assert!(s.idle.idle(), "usage events or the Windows 11 heartbeat say nobody reads");
    }
}
