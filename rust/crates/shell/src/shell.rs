//! The window's state without the window: the plugins, their cards and settings, banners, folds and saving (app.py's
//! TelescopeWindow minus Qt). The Slint side (window.rs) drains `effects` after each call and shows the result.

use telescope_core::i18n::Fields;
use crate::banners::Banners;
use crate::collapse::Collapsing;
use crate::layout::{Panels, Region};
use crate::{profiles, switches};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::panic::{catch_unwind, AssertUnwindSafe};
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
    /// HostSources: a plugin's stream source offered, taken back, or to start.
    AddSource(OfferedSource),
    RemoveSource(String),
    StreamSource(String),
    /// HostUi::open_window and close_window: a window of the plugin's own.
    OpenWindow { plugin: String, id: String, window: telescope_core::ui::WindowSpec },
    CloseWindow { plugin: String, id: String },
    Tray(TrayAction),
    FrameStep(String),
    OpenSettings(String),
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

/// Keeps a copy of each frame on its way into the colour step, for the preview's before and after (colour_split).
struct KeepBefore(Arc<std::sync::Mutex<BTreeMap<String, telescope_core::Frame>>>);

impl telescope_core::frame::CpuStep for KeepBefore {
    fn touch(&self, _ctx: &dyn telescope_core::frame::FrameContext) -> telescope_core::frame::Touch {
        telescope_core::frame::Touch::Reads
    }

    fn process(&mut self, frame: &mut telescope_core::Frame, ctx: &dyn telescope_core::frame::FrameContext) {
        self.0.lock().unwrap().insert(ctx.source_id().to_string(), frame.clone());
    }
}

/// Monotonic seconds, the view controller's clock.
pub fn now_s() -> f64 {
    telescope_core::time::monotonic_ns() as f64 / 1e9
}

pub struct State {
    pub slots: Vec<Slot>,
    pub panels: Panels,
    pub collapse: Collapsing,
    pub banners: Banners,
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
    /// A pass asked for the preview to be drawn again (refresh_preview, or an analysis that finished).
    pub preview_dirty: Arc<std::sync::atomic::AtomicBool>,
    /// The stream flow's slot_memory: which virtual camera each source went to last.
    pub slot_memory: Rc<RefCell<std::collections::BTreeMap<String, u32>>>,
    /// Sources only remembered once their settings change (a browser).
    pub changes_only: Vec<String>,
    pub save_failure_notified: bool,
    /// Posts from plugins' threads (host.poster), delivered to their owner after the bus's events.
    pub posts: Arc<Posts>,
    /// Built-in plugins that are off this run (not the optional ones never switched on), by name.
    pub off: Vec<PluginInfo>,
    /// Every built-in plugin, on or off, for the Plugins tab.
    pub builtins: Vec<PluginInfo>,
    /// Added plugins that are on but didn't start: name to why.
    pub problems: BTreeMap<String, String>,
    /// What the banner calls each of them.
    problem_titles: BTreeMap<String, String>,
    /// The added plugins that are running: they're not built in, so the Plugins tab lists them from their folder.
    pub added: Vec<PluginInfo>,
    /// The plugins folder and the switches, from the plugin host (the settings file's switches alone without one).
    pub admin: Arc<dyn telescope_core::plugin_admin::PluginAdmin>,
    pub keep_in_tray: bool,
    /// Link, for what the host has to answer from it straight away (hand_over_stream's prepared route).
    pub link: Option<Arc<dyn telescope_core::link::Link>>,
    /// Every running stream's newest frame, for plugins that use other streams' pictures (host.frames).
    pub frames: Option<Arc<dyn telescope_core::host::FrameStore>>,
    /// Person masks and people (HostVision), None without the vision crate.
    pub vision: Option<Arc<dyn telescope_core::vision::VisionService>>,
    /// How the workers turn frames upright for vision, kept up with the view.
    pub upright: Arc<std::sync::Mutex<telescope_core::vision::Upright>>,
    /// Motion in zones of the shown stream's picture, for the plugins that watch it; the workers feed it.
    pub motion: Arc<telescope_core::motion::Motion>,
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
    pub tray_close_notified: bool,
    /// Host::set_shortcut_menu: the plugin behind the controls' right-click menu, and the shortcuts it lists.
    pub shortcut_menu: Option<(String, Vec<telescope_core::ui::ShortcutEntry>)>,
    /// The photos being taken, by request: (plugin, the stream's id) for each, to say what became of them when they end.
    pub stills: Vec<(RequestId, String, String)>,
    /// Photos' "still running" flags, by the stream they're taken on: cleared when that stream ends, which ends the photo.
    pub still_alive: Vec<(String, Arc<std::sync::atomic::AtomicBool>)>,
    /// Reads what's on the clipboard (the window pastes it into a hidden box); None before there is a window.
    pub clipboard: Option<Box<dyn Fn() -> Option<String>>>,
    next_request: u64,
    // Plugin hooks running right now, and whether one asked for save_now
    calls: u32,
    pub(crate) save_after_call: bool,
}

impl State {
    pub fn new(file: ConfigFile) -> State {
        let admin = Arc::new(crate::admin::ShellAdmin::new(file.path()));
        let bus = EventBus::new();
        let sender = bus.sender();
        State {
            slots: Vec::new(),
            panels: Panels::default(),
            collapse: Collapsing::default(),
            banners: Banners::default(),
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
            preview_dirty: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            slot_memory: Rc::default(),
            changes_only: Vec::new(),
            save_failure_notified: false,
            posts: Arc::default(),
            off: Vec::new(),
            builtins: Vec::new(),
            problems: BTreeMap::new(),
            problem_titles: BTreeMap::new(),
            added: Vec::new(),
            admin,
            frames: None,
            vision: None,
            upright: Arc::default(),
            motion: Arc::new(telescope_core::motion::Motion::new(Box::new(move |key, owner, zones| {
                sender.emit(Event::MotionSeen { source_id: key.to_string(), owner: owner.to_string(), zones: zones.to_vec() });
            }))),
            frame_sizes: Arc::default(),
            keep_in_tray: false,
            link: None,
            window_active: false,
            mic: (false, false),
            camera_on_shown: true,
            shown: None,
            tray_close_notified: false,
            shortcut_menu: None, stills: Vec::new(), still_alive: Vec::new(), clipboard: None, next_request: 1, calls: 0, save_after_call: false,
        }
    }

    pub fn index(&self, name: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.info.name == name)
    }

    pub fn request_id(&mut self) -> RequestId {
        self.next_request += 1;
        RequestId(self.next_request)
    }

    /// Runs f with plugin i taken out and a host for it; a plugin that panics is logged and the others carry on, as
    /// app.py's _call does with exceptions.
    pub fn call<R>(&mut self, i: usize, f: impl FnOnce(&mut dyn Plugin, &mut crate::host::Ctx) -> R) -> Option<R> {
        let mut plugin = self.slots.get_mut(i)?.plugin.take()?;
        let name = self.slots[i].info.name.clone();
        self.calls += 1;
        let result = {
            let mut ctx = crate::host::Ctx { state: self, me: name.clone() };
            catch_unwind(AssertUnwindSafe(|| f(plugin.as_mut(), &mut ctx)))
        };
        self.slots[i].plugin = Some(plugin);
        self.calls -= 1;
        if self.calls == 0 && std::mem::take(&mut self.save_after_call) && !self.save_now() {
            self.effects.push(Effect::SaveSoon); // a file that can't be read now isn't saved over, and is tried again
        }
        match result {
            Ok(r) => Some(r),
            Err(_) => {
                log::error!("Plugin {name} failed");
                None
            }
        }
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
        let i = self.slots.len();
        self.slots.push(Slot { plugin: Some(plugin), info: info.clone(), card: None, header: None, actions: vec![], tray: vec![], defaults: json!({}) });
        let made = self.call(i, |p, host| {
            p.setup(host);
            let card = p.card();
            let header = p.header_widget();
            let tray = p.tray_actions();
            let defaults = p.get_config();
            (card, header, tray, defaults)
        });
        let Some((card, header, tray, defaults)) = made else {
            self.slots.pop();
            return Err(format!("{} failed while starting", info.name));
        };
        if let Some(card) = &card {
            let region = Region::from_name(&card.region);
            self.panels.place(region, i, info.pinned, &info.category);
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
        let added: std::collections::BTreeSet<String> = self.admin.find_installed().into_iter().map(|p| p.name).collect();
        let mut broke = Vec::new();
        for plugin in plugins {
            let info = plugin.info();
            if added.contains(&info.name) {
                // One that can't start is left out and says why (load_plugins)
                match self.register(plugin) {
                    Ok(()) => self.added.push(info),
                    Err(e) => broke.push(PluginProblem { title: if info.title.is_empty() { info.name.clone() } else { info.title.clone() }, name: info.name, error: e }),
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
                .map(|(name, error)| PluginProblem { name: name.clone(), title: self.problem_titles.get(name).cloned().unwrap_or_else(|| name.clone()), error: error.clone() }).collect();
            all.extend(broke);
            self.set_plugin_problems(&all);
        }
        crate::settings::start_plugin_updates(self.admin.clone());
        self.remind_plugins_off(&cfg);
        Ok(())
    }

    /// Added plugins that are on but didn't start: remembered for the Plugins tab, with a warning banner saying which (load_plugins).
    pub fn set_plugin_problems(&mut self, problems: &[PluginProblem]) {
        self.problems = problems.iter().map(|p| (p.name.clone(), p.error.clone())).collect();
        self.problem_titles = problems.iter().map(|p| (p.name.clone(), p.title.clone())).collect();
        if problems.is_empty() {
            return;
        }
        let titles: Vec<String> = problems.iter().map(|p| tr(&p.title)).collect();
        let n = titles.len();
        let title = if n < 4 {
            crate::texts::trn("{plugins} didn't start", "{plugins} didn't start", n).field("plugins", crate::texts::and_list(&titles))
        } else {
            crate::texts::trn("{n} plugin didn't start", "{n} plugins didn't start", n)
        };
        let text = crate::texts::trn("Telescope works without it. The Plugins tab in Settings says what went wrong.",
                                     "Telescope works without them. The Plugins tab in Settings says what went wrong.", n);
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
        let mut cfg = self.file.load();
        self.apply_config(&mut cfg);
        self.each(|p, host| p.on_plugins_ready(host));
        if let Some(backup) = self.file.take_reset_notice() {
            let where_ = if backup.is_empty() { String::new() } else { format!(" {}", tr("A copy of the old file is at {path}.").field("path", &backup)) };
            self.banners.show("config", Issue {
                title: tr("Your settings couldn't be read"),
                text: tr("Telescope started with its defaults, so paired phones need pairing again.") + &where_,
                actions: vec![], kind: "warn".into(), details: String::new(), tell_dismiss: false,
            });
        }
    }

    fn apply_config(&mut self, cfg: &mut Value) {
        let window = WindowState::from_config(cfg);
        self.collapse.load(cfg.get("collapsed"));
        *self.slot_memory.borrow_mut() = window.output_slots.iter().filter_map(|(id, slot)| Some((id.clone(), u32::try_from(slot.as_u64()?).ok()?))).collect();
        self.selected = window.selected_device.clone();
        if profiles::spread_shared_settings(cfg, &self.device_local()) {
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
        if let Some(p) = self.slots[i].plugin.as_mut() {
            if catch_unwind(AssertUnwindSafe(|| f(p.as_mut()))).is_err() {
                log::error!("Plugin {} failed", self.slots[i].info.name);
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
        self.vision.clone().map(|service| crate::worker::VisionFeed { service, upright: self.upright.clone(), frame_sizes: self.frame_sizes.clone(), motion: Some(self.motion.clone()) })
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
            *upright = telescope_core::vision::Upright { turn, sent };
            old
        };
        if let Some(vision) = self.vision.as_ref().filter(|_| old != turn) {
            vision.view_turned(old, turn);
        }
        if old != turn {
            // The picture turned, so everything is somewhere else
            self.motion.reset(0.0, now_s());
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
    }

    /// Every plugin's frame step as it is now, in registration order (the pipeline sorts them by stage), with the colour
    /// stages source_id's stream gets.
    pub fn pipeline_steps(&mut self, source_id: Option<&str>) -> Vec<telescope_core::media::StagedStep> {
        use telescope_core::frame::FrameStage;
        use telescope_core::media::StagedStep;
        let mut steps = Vec::new();
        // The window's own steps: its view first, and the colour pipeline's LUT after the effects
        for step in telescope_core::view_step::steps(&self.vc.frame_view(&self.view)) {
            steps.push(StagedStep { owner: "view".into(), stage: FrameStage::View, step });
        }
        let colour = self.colour.step(source_id);
        // The picture from before is kept only while there's a colour step to compare with
        if colour.is_none() || self.colour.split().is_none() {
            self.colour_before.lock().unwrap().clear();
        }
        if let Some(step) = colour {
            if self.colour.split().is_some() {
                let keep = KeepBefore(self.colour_before.clone());
                steps.push(StagedStep { owner: "colour".into(), stage: FrameStage::Colour, step: telescope_core::frame::FrameStep::Cpu(Box::new(keep)) });
            }
            steps.push(StagedStep { owner: "colour".into(), stage: FrameStage::Colour, step });
        }
        for i in 0..self.slots.len() {
            let (owner, stage) = (self.slots[i].info.name.clone(), self.slots[i].info.frame_stage);
            let Some(p) = self.slots[i].plugin.as_mut() else { continue };
            match catch_unwind(AssertUnwindSafe(|| p.frame_step())) {
                Ok(Some(step)) => steps.push(telescope_core::media::StagedStep { owner, stage, step }),
                Ok(None) => {}
                Err(_) => log::error!("Plugin {owner} couldn't give its frame step; leaving it out"),
            }
        }
        steps
    }

    /// _config_of: a plugin's settings, or what was saved before when it gives something the file can't hold.
    fn config_of(&self, i: usize, saved: Option<&Value>) -> Option<Value> {
        let p = self.slots[i].plugin.as_ref()?;
        match catch_unwind(AssertUnwindSafe(|| p.get_config())) {
            Ok(v @ Value::Object(_)) => Some(v),
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
        // The start couldn't read the file, so this app holds defaults: it takes in what's there instead of saving over it
        if let Some(read) = self.file.reread_after_unread_start() {
            return match read {
                Ok(mut cfg) => {
                    self.apply_config(&mut cfg);
                    true
                }
                Err(e) => {
                    log::error!("Couldn't read the settings file; not saving over it: {e}");
                    false
                }
            };
        }
        let Ok(mut cfg) = self.file.load_strict() else {
            log::error!("Couldn't read the settings file; not saving over it");
            return false;
        };
        let global = cfg.get("plugin_configs").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut sections: Vec<PluginSection> = (0..self.slots.len())
            .filter(|&i| !self.slots[i].info.device_local && !self.slots[i].info.name.is_empty())
            .filter_map(|i| {
                let name = self.slots[i].info.name.clone();
                self.config_of(i, global.get(&name)).map(|config| PluginSection { plugin: name, device: None, config })
            }).collect();
        sections.extend(self.profile_sections(&cfg, self.selected.as_deref()));
        let window = WindowState { selected_device: self.selected.clone(), collapsed: self.collapse.keys(), output_slots: self.slot_memory.borrow().iter().map(|(id, slot)| (id.clone(), json!(slot))).collect() };
        merge_for_save(&mut cfg, &window, &sections);
        profiles::drop_unchanged(&mut cfg, &self.changes_only, &self.defaults());
        if self.file.save(&mut cfg) {
            self.save_failure_notified = false;
            true
        } else {
            if !self.save_failure_notified {
                self.save_failure_notified = true;
                log::error!("Failed to save settings");
                self.effects.push(Effect::Notify {
                    title: tr("Telescope - Save failed"),
                    body: tr("Could not save settings. Check disk space and permissions."),
                    urgent: true,
                });
            }
            false
        }
    }

    fn defaults(&self) -> Map<String, Value> {
        self.slots.iter().filter(|s| !s.info.name.is_empty()).map(|s| (s.info.name.clone(), s.defaults.clone())).collect()
    }

    /// switch_device: the old device's settings saved, the new one's applied, and a running stream started again
    /// on the new one (by the stream flow, from the effects). Streams stop first: stopping saves what the panels show
    /// under the stream that had it, and the new phone's settings would be filed there.
    pub fn switch_device(&mut self, prev: Option<&str>, new: Option<&str>) {
        if let Ok(mut cfg) = self.file.load_strict() {
            let sections = self.profile_sections(&cfg, prev);
            let window = WindowState::from_config(&cfg);
            merge_for_save(&mut cfg, &window, &sections);
            profiles::drop_unchanged(&mut cfg, &self.changes_only, &self.defaults());
            self.file.save(&mut cfg);
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
        let cfg = self.file.load();
        self.apply_device_profile(new, &cfg);
        self.bus.emit(Event::DeviceChanged { source_id: new.unwrap_or("").to_string() });
        if start {
            self.effects.push(Effect::Start { interactive: true });
        }
    }

    pub fn forget_device_settings(&mut self, id: &str) {
        self.slot_memory.borrow_mut().remove(id);
        if let Ok(mut cfg) = self.file.load_strict() {
            if profiles::forget_device(&mut cfg, id) {
                self.file.save(&mut cfg);
            }
        }
    }

    pub fn move_device_settings(&mut self, old: &str, new: &str) {
        let mut memory = self.slot_memory.borrow_mut();
        if let Some(slot) = memory.remove(old) {
            memory.insert(new.to_string(), slot);
        }
        drop(memory);
        if let Ok(mut cfg) = self.file.load_strict() {
            profiles::move_device(&mut cfg, old, new);
            self.file.save(&mut cfg);
        }
    }

    /// Hands every queued event to every plugin, in order; events emitted while delivering go after. Then each post
    /// from a plugin's threads goes to that plugin alone. Past DELIVERY_BATCH the rest waits for the next loop turn.
    pub fn deliver_events(&mut self) {
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
                    self.effects.push(Effect::SettleStream);
                    let said = self.vc.camera_caps(&self.view, Some(camera), now_s());
                    self.apply_said(said);
                    // Another lens isn't motion
                    self.motion.reset(telescope_core::motion::LENS_SETTLE_S, now_s());
                    // Before the plugins hear it, so what the lens can take is fresh for them
                    self.note_lens(camera);
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

    /// A photo's end: its stream is free for another, and every plugin hears that and what was saved.
    fn still_ended(&mut self, event: &Event) {
        let Event::RequestDone { request, data, .. } = event else { return };
        let Some(i) = self.stills.iter().position(|(r, ..)| r == request) else { return };
        let (_, _, source) = self.stills.remove(i);
        let busy = self.link.as_ref().and_then(|l| l.stills()).is_some_and(|s| s.busy(Some(&source)));
        self.bus.emit(Event::StillBusyChanged { source_id: source.clone(), busy });
        let files: Vec<String> = data.get("files").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
        if !files.is_empty() {
            self.bus.emit(Event::StillSaved { source_id: source, paths: files });
        }
    }

    /// Photos on `source` (all of them for None) end as interrupted: the stream they were taken from is gone.
    pub fn let_go_of_photos(&mut self, source: Option<&str>) {
        self.still_alive.retain(|(id, alive)| {
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
        self.slots[i].plugin.as_ref().map(|p| p.get_config())
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
        lines.extend(self.colour.diagnostics());
        for i in 0..self.slots.len() {
            match self.slots[i].plugin.as_ref() {
                Some(p) => match catch_unwind(AssertUnwindSafe(|| p.diagnostics())) {
                    Ok(more) => lines.extend(more),
                    Err(_) => lines.push((self.slots[i].info.name.clone(), "couldn't read (panic)".into())),
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

    // test_vision_pack.py::test_the_plugin_waiting_for_the_download_is_switched_on_with_no_dialog_open
    #[test]
    fn the_plugin_waiting_for_the_download_is_switched_on_with_no_dialog_open() {
        let mut s = state();
        s.bus.emit(Event::VisionDownloadFinished { error: String::new(), switch_on: "framing".into() });
        s.deliver_events();
        assert_eq!(crate::switches::optional_on(&s.file.load()).into_iter().collect::<Vec<_>>(), ["framing"]);
    }

    // test_vision_pack.py::test_a_failed_download_remembers_why_for_the_plugin_that_waited (vision's host_tests has what it remembers)
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

    /// The plugin host's answer to "which plugins are added": these, all switched on.
    struct Added(Vec<&'static str>);

    impl telescope_core::plugin_admin::PluginAdmin for Added {
        fn builtin_off(&self) -> std::collections::BTreeSet<String> { Default::default() }
        fn set_builtin_off(&self, _: &str, _: bool) -> std::io::Result<()> { Ok(()) }
        fn reset_to_defaults(&self) -> std::io::Result<()> { Ok(()) }
        fn optional_on(&self) -> std::collections::BTreeSet<String> { Default::default() }
        fn set_optional_on(&self, _: &str, _: bool) -> std::io::Result<()> { Ok(()) }
        fn switch(&self, _: &str, default: bool) -> bool { default }
        fn set_switch(&self, _: &str, _: bool) -> std::io::Result<()> { Ok(()) }
        fn find_installed(&self) -> Vec<telescope_core::plugin_admin::AddedPlugin> {
            self.0.iter().map(|n| telescope_core::plugin_admin::AddedPlugin { name: n.to_string(), title: n.to_string(), enabled: true, ..Default::default() }).collect()
        }
    }

    /// An added plugin is one the plugin host lists in the plugins folder.
    fn added_folder(s: &mut State, name: &'static str) {
        s.admin = Arc::new(Added(vec![name]));
    }

    struct Quitter;

    impl Plugin for Quitter {
        fn info(&self) -> PluginInfo {
            PluginInfo { name: "quitter".into(), title: "Quitter".into(), ..Default::default() }
        }
        fn setup(&mut self, _h: &mut dyn telescope_core::Host) {
            panic!("no card today");
        }
    }

    #[test]
    fn added_plugins_are_listed_in_the_diagnostics_as_running_or_not() {
        let mut s = state();
        added_folder(&mut s, "extra");
        s.load_builtins(vec![stub("setup", json!({}), false).0, stub("extra", json!({}), false).0]).unwrap();
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
        assert!(s.load_builtins(vec![Box::new(Quitter)]).unwrap_err().contains("quitter"));
    }

    #[test]
    fn an_added_plugin_that_breaks_while_starting_is_taken_back_out() {
        let mut s = state();
        added_folder(&mut s, "quitter");
        s.load_builtins(vec![stub("setup", json!({}), false).0, Box::new(Quitter)]).unwrap();
        assert!(s.index("quitter").is_none() && s.index("setup").is_some());
        assert_eq!(s.banners.issue("plugins").unwrap().title, "Quitter didn't start");
        assert!(s.problems["quitter"].contains("quitter"));
        assert_eq!(s.diagnostics().into_iter().find(|(k, _)| k == "Added plugins").unwrap().1, "quitter (didn't start)");
    }

    fn broken(name: &str, title: &str) -> PluginProblem {
        PluginProblem { name: name.into(), title: title.into(), error: format!("{name} broke") }
    }

    #[test]
    fn added_plugins_that_didnt_start_get_a_banner_with_a_plugins_action() {
        let mut s = state();
        s.set_plugin_problems(&[]);
        assert!(s.banners.issue("plugins").is_none());
        s.set_plugin_problems(&[broken("a", "Alpha")]);
        let issue = s.banners.issue("plugins").unwrap().clone();
        assert_eq!(issue.title, "Alpha didn't start");
        assert_eq!(issue.text, "Telescope works without it. The Plugins tab in Settings says what went wrong.");
        assert_eq!(issue.kind, "warn");
        assert_eq!(issue.actions, vec![IssueAction { label: "Plugins…".into(), does: IssueDoes::OpenSettings { tab: "plugins".into() } }]);
        assert_eq!(s.problems.get("a").map(String::as_str), Some("a broke"));
        s.set_plugin_problems(&[broken("a", "Alpha"), broken("b", "Beta"), broken("c", "Gamma")]);
        let issue = s.banners.issue("plugins").unwrap();
        assert_eq!(issue.title, "Alpha, Beta and Gamma didn't start");
        assert_eq!(issue.text, "Telescope works without them. The Plugins tab in Settings says what went wrong.");
        s.set_plugin_problems(&[broken("a", "A"), broken("b", "B"), broken("c", "C"), broken("d", "D")]);
        assert_eq!(s.banners.issue("plugins").unwrap().title, "4 plugins didn't start");
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
        s.streams.cameras = vec![CameraUse { source_id: "phone-1".into(), slot: 0, label: "Phone Camera".into(), size: Some((1920, 1080)) }];
        let answers = s.call(0, |_, host| (host.stream_output("phone-1"), host.stream_on_camera(0), host.stream_on_camera(1)));
        assert_eq!(answers, Some(("Phone Camera".to_string(), Some(("phone-1".to_string(), Some(1920), Some(1080))), None)));
        let change = OutputChange { width: None, height: None, fps: Some(Some(60)) };
        s.call(0, |_, host| host.update_stream_output(change, None));
        assert!(s.effects.iter().any(|e| matches!(e, Effect::UpdateOutput { change: c, source_id: None } if c.fps == Some(Some(60)))));
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
}
