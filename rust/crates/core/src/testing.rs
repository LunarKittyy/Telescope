//! Helpers for any crate's tests: a host that records what plugins ask of it, and scratch folders.

use crate::event::Event;
use crate::frame::{Frame, StreamFrame};
use crate::host::*;
use crate::source::{RouteSpec, StreamSource};
use crate::ui::{ActionSpec, Issue, TrayAction};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A new empty folder under the system's temp folder, unique to this call.
pub fn scratch_dir(label: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("telescope-test-{}-{label}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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

/// The frames FakeHost::stream_frames holds.
struct HeldFrames(Vec<StreamFrame>);

impl FrameStore for HeldFrames {
    fn stream_frames(&self) -> Vec<StreamFrame> {
        self.0.clone()
    }
    fn stream_frame(&self, source_id: Option<&str>) -> Option<StreamFrame> {
        self.0.iter().find(|f| source_id.is_none_or(|id| f.source_id == id)).cloned()
    }
    fn replace_frame(&self, _source_id: Option<&str>, _frame: Frame, _number: u64) -> bool {
        false
    }
}

impl SensorReader for NoFrames {
    fn reading(&self, _source_id: Option<&str>) -> Option<Value> {
        None
    }
}

/// The reading FakeHost::sensor_reading holds.
struct HeldReading(Arc<Mutex<Option<Value>>>);

impl SensorReader for HeldReading {
    fn reading(&self, _source_id: Option<&str>) -> Option<Value> {
        self.0.lock().unwrap().clone()
    }
}

/// A host with nothing streaming that writes down every call (calls) and event (events), for testing a plugin on its own.
#[derive(Default)]
pub struct FakeHost {
    pub calls: Vec<String>,
    pub events: Vec<Event>,
    /// Every pick_file, in order.
    pub dialogs: Vec<crate::host::FileDialog>,
    /// Every set_colour_stage, in order, as (owner, stage).
    pub colour_stages: Vec<(String, Option<ColourStage>)>,
    /// What each owner last put on change_packets.
    pub packets: HashMap<String, Option<Arc<dyn PacketStepFactory>>>,
    /// What plugin_config() answers, by plugin name.
    pub plugin_configs: HashMap<String, Value>,
    /// Plugins has_plugin() says are there.
    pub plugins: Vec<String>,
    pub streaming: bool,
    pub focused: Option<String>,
    pub view: View,
    /// What view_aspect() answers.
    pub aspect: f32,
    /// What took the view ("" for nobody): take_view fails for anyone else, as the real host's does.
    pub view_owner: String,
    /// Every steer_view, as (zoom, pan_x, pan_y).
    pub steered: Vec<(f32, f32, f32)>,
    /// Timers pending from Host::schedule, as (after_ms, token), oldest first.
    pub scheduled: Vec<(u64, String)>,
    /// What the poster's posts carried, as (token, data), in order; take them out to deliver them.
    pub posted: Arc<Mutex<Vec<(String, Value)>>>,
    /// What shortcut_actions() lists; run_action answers "{id} ran" for these.
    pub actions: Vec<(String, ActionSpec)>,
    /// What run_action answers for an action id instead of "ran": a text, or None for a control that's off.
    pub action_answers: HashMap<String, Option<String>>,
    /// How often toggle_window ran: it shows, then hides.
    pub window_toggles: u32,
    pub window_active: bool,
    /// The phone's camera is off (set_camera_on(false) turns it off too).
    pub camera_off: bool,
    /// What background_streams() lists.
    pub background: Vec<BackgroundStream>,
    /// Whether pick_on_preview() says there is a preview to pick on.
    pub can_pick: bool,
    /// The corners each pick_on_preview() started from.
    pub picks: Vec<Option<[[f32; 2]; 4]>>,
    /// The warp the preview was last told about.
    pub warp: Option<[f32; 9]>,
    /// What can_turn_camera_off() answers instead of (false, "Not streaming").
    pub can_turn_off: Option<(bool, String)>,
    /// What vision_state() answers instead of "off".
    pub vision: Option<String>,
    /// What vision_pack() answers.
    pub pack: crate::vision::VisionPack,
    /// What tracked_people() answers for each source id.
    pub tracked: HashMap<String, (Vec<Value>, u32)>,
    /// What packets_problem() answers instead of "".
    pub packets_problem: String,
    /// The newest frame of each stream, as frames() hands them out.
    pub stream_frames: Vec<StreamFrame>,
    /// The picture each set_ui_image last set, by (ui, property).
    pub images: HashMap<(String, String), Frame>,
    /// A start is waking the phone (is_starting), or the stream is stopping to start again (is_restarting).
    pub starting: bool,
    pub restarting: bool,
    /// How many streams run (stream_count) while one does; 1 when this is left alone.
    pub streams: Option<u32>,
    /// The camera was turned off by a rule, not by hand (is_camera_off_auto).
    pub camera_off_auto: bool,
    /// What canvas_size() answers: Setup's canvas, or (None, None) for Auto.
    pub canvas: (Option<u32>, Option<u32>),
    /// Every send_notification, as (title, body, urgent).
    pub notifications: Vec<(String, String, bool)>,
    /// What preset_plugins() lists, in order; apply_preset hands each one's config to the plugin_configs entry.
    pub preset_plugins: Vec<String>,
    /// What's on the clipboard: copy_text puts text there unless the clipboard is deaf (Wayland ignores a copy from a window
    /// in the background).
    pub clipboard: Option<String>,
    pub clipboard_deaf: bool,
    /// The banners that are up, as (key, issue): show_issue puts one up in place of that key's, clear_issue takes it down.
    pub issues: Vec<(String, Issue)>,
    /// What each plugin has on the preview, by owner.
    pub layers: HashMap<String, PreviewLayer>,
    /// The passes plugins put on the preview, by owner.
    pub passes: HashMap<String, Arc<dyn crate::preview::PreviewPass>>,
    /// What sensor_status() answers instead of {"state": "off"}.
    pub sensor_status: Option<Value>,
    /// What the sensors' reading() answers, as SensorReading's fields.
    pub sensor_reading: Arc<Mutex<Option<Value>>>,
    /// What still_caps() answers: the lens's photo options as JSON, or None for a stream that can't take photos.
    pub still_caps: Option<Value>,
    /// What still_busy() answers.
    pub still_busy: bool,
    /// The photos asked for, as (options, folder) in order.
    pub stills_taken: Vec<(Value, String)>,
    /// What stream_on_camera() answers, as (slot, source id, width, height).
    pub cameras: Vec<(u32, String, Option<u32>, Option<u32>)>,
    /// Streams is_streaming_from() says run, besides the focused one while streaming.
    pub live: Vec<String>,
    /// What frames() hands out in place of stream_frames, for a test that changes the frames as it goes.
    pub frame_store: Option<Arc<dyn FrameStore>>,
    /// What device_config() answers for a source id, as (source id, plugin) to settings; {} for the rest.
    pub device_configs: HashMap<(String, String), Value>,
    /// What diagnostics_report() answers.
    pub report: String,
    /// What set_plugin_hues last said (None until it's called).
    pub plugin_hues: Option<bool>,
    /// What set_power_saving last said (None until it's called).
    pub power_saving: Option<bool>,
    next_request: u64,
}

struct FakePoster(Arc<Mutex<Vec<(String, Value)>>>);

impl Poster for FakePoster {
    fn post(&self, token: &str, data: Value) {
        self.0.lock().unwrap().push((token.to_string(), data));
    }
}

impl FakeHost {
    pub fn new() -> FakeHost {
        FakeHost::default()
    }

    fn note(&mut self, call: impl Into<String>) {
        self.calls.push(call.into());
    }

    fn request(&mut self) -> RequestId {
        self.next_request += 1;
        RequestId(self.next_request)
    }
}

impl HostSettings for FakeHost {
    fn schedule_save(&mut self) {
        self.note("schedule_save");
    }
    fn save_now(&mut self) {
        self.note("save_now");
    }
    fn plugin_config(&self, name: &str) -> Option<Value> {
        self.plugin_configs.get(name).cloned()
    }
    fn apply_preset(&mut self, name: &str, cfg: &Value) {
        self.note(format!("apply_preset {name} {cfg}"));
        if self.plugin_configs.contains_key(name) {
            self.plugin_configs.insert(name.to_string(), cfg.clone());
        }
    }
    fn preset_plugins(&self) -> Vec<String> {
        self.preset_plugins.clone()
    }
    fn device_config(&self, source_id: Option<&str>, plugin_name: &str) -> Value {
        source_id.and_then(|id| self.device_configs.get(&(id.to_string(), plugin_name.to_string()))).cloned().unwrap_or_else(|| json!({}))
    }
    fn has_device_settings(&self, _name: &str) -> bool {
        false
    }
    fn switch_device(&mut self, prev: Option<&str>, new: Option<&str>) {
        self.note(format!("switch_device {prev:?} {new:?}"));
    }
    fn forget_device_settings(&mut self, name: &str) {
        self.note(format!("forget_device_settings {name}"));
    }
    fn move_device_settings(&mut self, old: &str, new: &str) {
        self.note(format!("move_device_settings {old} {new}"));
    }
}

impl HostStreams for FakeHost {
    fn is_streaming(&self) -> bool {
        self.streaming
    }
    fn is_starting(&self) -> bool {
        self.starting
    }
    fn is_restarting(&self) -> bool {
        self.restarting
    }
    fn is_streaming_from(&self, source_id: Option<&str>) -> bool {
        (self.streaming && source_id.map(str::to_string) == self.focused) || source_id.is_some_and(|id| self.live.iter().any(|l| l == id))
    }
    fn stream_count(&self) -> u32 {
        if self.streaming { self.streams.unwrap_or(1) } else { 0 }
    }
    fn start_stream(&mut self, interactive: bool) {
        self.note(format!("start_stream {interactive}"));
        self.starting = true; // like the real host: waking the phone until the stream starts or fails
    }
    fn start_again(&mut self, source_id: Option<&str>) {
        self.note(format!("start_again {source_id:?}"));
    }
    fn stop_stream(&mut self, source_id: Option<&str>) {
        self.note(format!("stop_stream {source_id:?}"));
        self.starting = false;
    }
    fn stop_all_streams(&mut self) {
        self.note("stop_all_streams");
    }
    fn reconnect_stream(&mut self) {
        self.note("reconnect_stream");
    }
    fn reconnect_source(&mut self, source_id: &str) {
        self.note(format!("reconnect_source {source_id}"));
    }
    fn hand_over_stream(&mut self, route: &RouteSpec, _source_id: Option<&str>) -> bool {
        self.note(format!("hand_over_stream {route:?}"));
        false
    }
    fn focused_source_id(&self) -> Option<String> {
        self.focused.clone()
    }
    fn background_streams(&self) -> Vec<BackgroundStream> {
        self.background.clone()
    }
    fn pick_source(&mut self, source_id: Option<&str>) -> bool {
        self.note(format!("pick_source {source_id:?}"));
        false
    }
    fn main_stream(&self) -> Option<(String, String)> {
        None
    }
    fn make_main(&mut self, _source_id: &str) -> bool {
        false
    }
    fn update_stream_output(&mut self, change: OutputChange, _source_id: Option<&str>) {
        self.note(format!("update_stream_output {change:?}"));
    }
    fn canvas_size(&self) -> (Option<u32>, Option<u32>) {
        self.canvas
    }
    fn restart_vcam_canvas(&mut self, width: u32, height: u32, _shows_command: bool) -> RequestId {
        self.note(format!("restart_vcam_canvas {width}x{height}"));
        self.request()
    }
    fn stream_output(&self, _source_id: &str) -> String {
        String::new()
    }
    fn stream_on_camera(&self, slot: u32) -> Option<(String, Option<u32>, Option<u32>)> {
        self.cameras.iter().find(|c| c.0 == slot).map(|c| (c.1.clone(), c.2, c.3))
    }
    fn remove_extra_cameras(&mut self) -> RequestId {
        self.note("remove_extra_cameras");
        self.request()
    }
    fn phone_control(&mut self, source_id: Option<&str>, body: &Value) {
        self.note(format!("phone_control {source_id:?} {body}"));
    }
    fn phone_state(&self, _source_id: Option<&str>) -> Option<Value> {
        None
    }
    fn request_phone_state(&mut self, source_id: Option<&str>) {
        self.note(format!("request_phone_state {source_id:?}"));
    }
}

impl HostCamera for FakeHost {
    fn is_camera_on(&self) -> bool {
        !self.camera_off
    }
    fn is_camera_off_auto(&self) -> bool {
        self.camera_off && self.camera_off_auto
    }
    fn can_turn_camera_off(&self) -> (bool, String) {
        self.can_turn_off.clone().unwrap_or((false, "Not streaming".into()))
    }
    fn set_camera_on(&mut self, on: bool, auto: bool) {
        self.camera_off = !on;
        self.camera_off_auto = auto && !on;
        self.note(format!("set_camera_on {on} {auto}"));
    }
}

impl HostSources for FakeHost {
    fn add_stream_source(&mut self, source: Arc<dyn StreamSource>) {
        self.note(format!("add_stream_source {}", source.id()));
    }
    fn remove_stream_source(&mut self, source_id: &str) {
        self.note(format!("remove_stream_source {source_id}"));
    }
    fn stream_source(&mut self, source_id: &str) {
        self.note(format!("stream_source {source_id}"));
    }
}

impl HostShell for FakeHost {
    fn show_issue(&mut self, key: &str, issue: Issue) {
        self.note(format!("show_issue {key} {}", issue.title));
        self.issues.retain(|(k, _)| k != key);
        self.issues.push((key.to_string(), issue));
    }
    fn clear_issue(&mut self, key: Option<&str>, _issue: Option<&Issue>) {
        self.note(format!("clear_issue {key:?}"));
        self.issues.retain(|(k, _)| key.is_some_and(|key| k != key));
    }
    fn send_notification(&mut self, title: &str, body: &str, urgent: bool) {
        self.note(format!("send_notification {title}"));
        self.notifications.push((title.to_string(), body.to_string(), urgent));
    }
    fn set_keep_in_tray(&mut self, keep: bool) {
        self.note(format!("set_keep_in_tray {keep}"));
    }
    fn toggle_window(&mut self) -> bool {
        self.note("toggle_window");
        self.window_toggles += 1;
        self.window_toggles % 2 == 1
    }
    fn quit_app(&mut self) {
        self.note("quit_app");
    }
    fn restart_app(&mut self) -> bool {
        self.note("restart_app");
        false
    }
    fn diagnostics_report(&self, _own: &[(String, String)]) -> String {
        self.report.clone()
    }
    fn open_settings(&mut self, tab: &str) {
        self.note(format!("open_settings {tab}"));
    }
    fn shortcut_actions(&self) -> Vec<(String, ActionSpec)> {
        self.actions.clone()
    }
    fn run_action(&mut self, plugin: &str, id: &str, params: &Value) -> Option<String> {
        self.note(format!("run_action {plugin} {id} {params}"));
        if let Some(answer) = self.action_answers.get(id) {
            return answer.clone();
        }
        self.actions.iter().any(|(p, a)| p == plugin && a.id == id).then(|| format!("{id} ran"))
    }
    fn release_action(&mut self, plugin: &str, id: &str, params: &Value) {
        self.note(format!("release_action {plugin} {id} {params}"));
    }
    fn window_active(&self) -> bool {
        self.window_active
    }
    fn copy_text(&mut self, text: &str) {
        self.note(format!("copy_text {text}"));
        if !self.clipboard_deaf {
            self.clipboard = Some(text.to_string());
        }
    }
    fn clipboard_text(&mut self) -> Option<String> {
        self.clipboard.clone()
    }
    fn set_shortcut_menu(&mut self, bindings: Option<Vec<crate::ui::ShortcutEntry>>) {
        self.note(format!("set_shortcut_menu {}", serde_json::to_string(&bindings).unwrap_or_default()));
    }
    fn open_url(&mut self, url: &str) {
        self.note(format!("open_url {url}"));
    }
    fn set_plugin_hues(&mut self, on: bool) {
        self.note(format!("set_plugin_hues {on}"));
        self.plugin_hues = Some(on);
    }
    fn pick_file(&mut self, dialog: &FileDialog) -> Option<RequestId> {
        self.note(format!("pick_file {:?} {}", dialog.kind, dialog.title));
        self.dialogs.push(dialog.clone());
        Some(self.request())
    }
}

impl HostUi for FakeHost {
    fn set_ui_props(&mut self, ui: &str, props: &Value) {
        self.note(format!("set_ui_props {ui} {props}"));
    }
    fn update_tray_action(&mut self, action: &TrayAction) {
        self.note(format!("update_tray_action {}", action.id));
    }
    fn refresh_frame_step(&mut self) {
        self.note("refresh_frame_step");
    }
    fn set_ui_image(&mut self, ui: &str, prop: &str, frame: &Frame) {
        self.note(format!("set_ui_image {ui} {prop} {}x{}", frame.width, frame.height));
        self.images.insert((ui.to_string(), prop.to_string()), frame.clone());
    }
    fn refresh_actions(&mut self) {
        self.note("refresh_actions");
    }
    fn open_window(&mut self, id: &str, window: &crate::ui::WindowSpec) {
        self.note(format!("open_window {id} {}", window.ui.component));
    }
    fn close_window(&mut self, id: &str) {
        self.note(format!("close_window {id}"));
    }
}

impl HostView for FakeHost {
    fn view(&self) -> View {
        self.view.clone()
    }
    fn view_aspect(&self) -> f32 {
        self.aspect
    }
    fn set_view(&mut self, change: ViewChange) {
        self.note(format!("set_view {change:?}"));
        let v = &mut self.view;
        if let Some(z) = change.zoom { v.zoom = z; }
        if let Some(x) = change.pan_x { v.pan_x = x; }
        if let Some(y) = change.pan_y { v.pan_y = y; }
        if let Some(f) = change.flip_h { v.flip_h = f; }
        if let Some(f) = change.flip_v { v.flip_v = f; }
        if let Some(r) = change.rotation { v.rotation = r; }
        if let Some(s) = change.stretch_x { v.stretch_x = s; }
        if let Some(s) = change.stretch_y { v.stretch_y = s; }
    }
    fn take_view(&mut self, owner: &str) -> bool {
        self.note(format!("take_view {owner}"));
        if !self.view_owner.is_empty() && self.view_owner != owner {
            return false;
        }
        self.view_owner = owner.to_string();
        true
    }
    fn steer_view(&mut self, owner: &str, zoom: f32, pan_x: f32, pan_y: f32) {
        self.note(format!("steer_view {owner} {zoom} {pan_x} {pan_y}"));
        self.steered.push((zoom, pan_x, pan_y));
    }
    fn release_view(&mut self, owner: &str) {
        self.note(format!("release_view {owner}"));
        if self.view_owner == owner {
            self.view_owner.clear();
        }
    }
}

impl HostVision for FakeHost {
    fn set_power_saving(&mut self, on: bool) {
        self.note(format!("set_power_saving {on}"));
        self.power_saving = Some(on);
    }
    fn vision_state(&self) -> String {
        self.vision.clone().unwrap_or_else(|| "off".into())
    }
    fn want_people(&mut self, owner: &str, on: bool) {
        self.note(format!("want_people {owner} {on}"));
    }
    fn tracked_people(&self, source_id: Option<&str>) -> (Vec<Value>, u32) {
        self.tracked.get(source_id.unwrap_or("")).cloned().unwrap_or_default()
    }
    fn vision_pack(&self) -> crate::vision::VisionPack {
        self.pack.clone()
    }
    fn download_vision(&mut self) {
        self.note("download_vision");
    }
}

impl HostFrames for FakeHost {
    fn frames(&self) -> Arc<dyn FrameStore> {
        if let Some(store) = &self.frame_store {
            return store.clone();
        }
        if self.stream_frames.is_empty() { Arc::new(NoFrames) } else { Arc::new(HeldFrames(self.stream_frames.clone())) }
    }
}

impl HostMotion for FakeHost {
    fn watch_motion(&mut self, owner: &str, zones: &[[f32; 4]], sensitivity: u32) {
        self.note(format!("watch_motion {owner} {} {sensitivity}", zones.len()));
    }
    fn motion(&self, _owner: &str) -> Vec<u32> {
        Vec::new()
    }
}

impl HostPreview for FakeHost {
    fn add_preview_layer(&mut self, owner: &str, layer: PreviewLayer) {
        self.note(format!("add_preview_layer {owner}"));
        self.layers.insert(owner.to_string(), layer);
    }
    fn remove_preview_layer(&mut self, owner: &str) {
        self.note(format!("remove_preview_layer {owner}"));
        self.layers.remove(owner);
        self.passes.remove(owner);
    }
    fn add_preview_pass(&mut self, owner: &str, pass: Arc<dyn crate::preview::PreviewPass>) {
        self.note(format!("add_preview_pass {owner}"));
        self.passes.insert(owner.to_string(), pass);
    }
    fn refresh_preview(&mut self) {
        self.note("refresh_preview");
    }
    fn pick_on_preview(&mut self, owner: &str, kind: PickKind, start: Option<[[f32; 2]; 4]>, _hint: &str) -> bool {
        self.note(format!("pick_on_preview {owner} {kind:?}"));
        self.picks.push(start);
        self.can_pick
    }
    fn finish_pick(&mut self, _owner: &str) -> bool {
        false
    }
    fn cancel_pick(&mut self, owner: &str) {
        self.note(format!("cancel_pick {owner}"));
    }
    fn set_preview_warp(&mut self, owner: &str, matrix: Option<[f32; 9]>) {
        self.warp = matrix;
        self.note(format!("set_preview_warp {owner} {}", matrix.is_some()));
    }
}

impl HostStills for FakeHost {
    fn still_caps(&self, _source_id: Option<&str>) -> Option<Value> {
        self.still_caps.clone()
    }
    fn take_still(&mut self, options: &Value, folder: &str, _source_id: Option<&str>) -> Option<RequestId> {
        if self.still_caps.is_none() || self.still_busy {
            return None;
        }
        self.stills_taken.push((options.clone(), folder.to_string()));
        Some(self.request())
    }
    fn still_busy(&self, _source_id: Option<&str>) -> bool {
        self.still_busy
    }
}

impl HostColour for FakeHost {
    fn set_colour_stage(&mut self, owner: &str, stage: Option<ColourStage>, order: i32, _source_id: Option<&str>) {
        self.note(format!("set_colour_stage {owner} {} {order}", stage.is_some()));
        self.colour_stages.push((owner.to_string(), stage));
    }
    fn colour_split(&mut self, owner: &str, position: Option<f32>) {
        self.note(format!("colour_split {owner} {position:?}"));
    }
}

impl HostSensors for FakeHost {
    fn want_sensors(&mut self, owner: &str, kinds: &[String]) {
        self.note(format!("want_sensors {owner} {kinds:?}"));
    }
    fn sensor_status(&self, _source_id: Option<&str>) -> Value {
        self.sensor_status.clone().unwrap_or_else(|| json!({"state": "off"}))
    }
    fn sensors(&self) -> Arc<dyn SensorReader> {
        Arc::new(HeldReading(self.sensor_reading.clone()))
    }
}

impl HostPackets for FakeHost {
    fn packets_problem(&self) -> String {
        self.packets_problem.clone()
    }
    fn change_packets(&mut self, owner: &str, factory: Option<Arc<dyn PacketStepFactory>>) {
        self.note(format!("change_packets {owner} {}", factory.is_some()));
        self.packets.insert(owner.to_string(), factory);
    }
}

impl Host for FakeHost {
    fn emit(&mut self, event: Event) {
        self.events.push(event);
    }
    fn has_plugin(&self, name: &str) -> bool {
        self.plugins.iter().any(|p| p == name)
    }
    fn schedule(&mut self, after_ms: u64, token: &str) {
        self.scheduled.retain(|(_, t)| t != token);
        self.scheduled.push((after_ms, token.to_string()));
    }
    fn cancel_schedule(&mut self, token: &str) {
        self.scheduled.retain(|(_, t)| t != token);
    }
    fn poster(&self) -> Option<Arc<dyn Poster>> {
        Some(Arc::new(FakePoster(self.posted.clone())))
    }
}

impl FakeHost {
    /// Whether a timer with this token is pending.
    pub fn is_scheduled(&self, token: &str) -> bool {
        self.scheduled.iter().any(|(_, t)| t == token)
    }

    /// The posts so far, taken out.
    pub fn take_posted(&self) -> Vec<(String, Value)> {
        std::mem::take(&mut *self.posted.lock().unwrap())
    }
}

/// One server a FakeWeb was asked to start.
pub struct FakeServed {
    pub serve: crate::web::Serve,
    pub handler: Arc<dyn crate::web::PageHandler>,
    pub stats: Arc<crate::web::ServerStats>,
    /// How many times it was stopped.
    pub stopped: Arc<AtomicU64>,
}

struct FakeServer {
    stats: Arc<crate::web::ServerStats>,
    stopped: Arc<AtomicU64>,
}

impl crate::web::WebServer for FakeServer {
    fn port(&self) -> u16 {
        self.stats.port
    }
    fn stats(&self) -> Arc<crate::web::ServerStats> {
        self.stats.clone()
    }
    fn stop(&self) {
        self.stopped.fetch_add(1, Ordering::SeqCst);
    }
}

/// A Web that opens nothing: it hands out the addresses it's given and keeps what was served, for tests of what a plugin
/// serves.
#[derive(Default)]
pub struct FakeWeb {
    pub addresses: Mutex<Vec<String>>,
    /// Why certificate() fails, instead of answering.
    pub certificate_error: Mutex<Option<String>>,
    /// Why serve() fails, instead of starting.
    pub serve_error: Mutex<Option<String>>,
    /// Every folder and set of hosts a certificate was asked for.
    pub certificates: Mutex<Vec<(PathBuf, Vec<String>)>>,
    pub served: Mutex<Vec<FakeServed>>,
}

impl crate::web::Web for FakeWeb {
    fn addresses(&self) -> Vec<String> {
        self.addresses.lock().unwrap().clone()
    }
    fn certificate(&self, folder: &std::path::Path, hosts: &[String]) -> Result<crate::web::Certificate, String> {
        self.certificates.lock().unwrap().push((folder.to_path_buf(), hosts.to_vec()));
        match self.certificate_error.lock().unwrap().clone() {
            Some(why) => Err(why),
            None => Ok(crate::web::Certificate { cert: folder.join("cert.pem"), key: folder.join("key.pem") }),
        }
    }
    fn serve(&self, _certificate: &crate::web::Certificate, serve: &crate::web::Serve, handler: Arc<dyn crate::web::PageHandler>)
        -> Result<Box<dyn crate::web::WebServer>, String> {
        if let Some(why) = self.serve_error.lock().unwrap().clone() {
            return Err(why);
        }
        let port = if serve.port == 0 { 8768 } else { serve.port };
        let stats = Arc::new(crate::web::ServerStats::new(port, serve.port, false, &serve.label));
        let stopped = Arc::new(AtomicU64::new(0));
        self.served.lock().unwrap().push(FakeServed { serve: serve.clone(), handler, stats: stats.clone(), stopped: stopped.clone() });
        Ok(Box::new(FakeServer { stats, stopped }))
    }
}
