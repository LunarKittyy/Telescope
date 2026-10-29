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

impl SensorReader for NoFrames {
    fn reading(&self, _source_id: Option<&str>) -> Option<Value> {
        None
    }
}

/// A host with nothing streaming that writes down every call (calls) and event (events), for testing a plugin on its own.
#[derive(Default)]
pub struct FakeHost {
    pub calls: Vec<String>,
    pub events: Vec<Event>,
    /// What plugin_config() answers, by plugin name.
    pub plugin_configs: HashMap<String, Value>,
    /// Plugins has_plugin() says are there.
    pub plugins: Vec<String>,
    pub streaming: bool,
    pub focused: Option<String>,
    pub view: View,
    /// Timers pending from Host::schedule, as (after_ms, token), oldest first.
    pub scheduled: Vec<(u64, String)>,
    /// What the poster's posts carried, as (token, data), in order; take them out to deliver them.
    pub posted: Arc<Mutex<Vec<(String, Value)>>>,
    /// What shortcut_actions() lists; run_action answers "{id} ran" for these.
    pub actions: Vec<(String, ActionSpec)>,
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
    }
    fn preset_plugins(&self) -> Vec<String> {
        Vec::new()
    }
    fn device_config(&self, _source_id: Option<&str>, _plugin_name: &str) -> Value {
        json!({})
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
        false
    }
    fn is_restarting(&self) -> bool {
        false
    }
    fn is_streaming_from(&self, source_id: Option<&str>) -> bool {
        self.streaming && source_id.map(str::to_string) == self.focused
    }
    fn stream_count(&self) -> u32 {
        self.streaming as u32
    }
    fn start_stream(&mut self, interactive: bool) {
        self.note(format!("start_stream {interactive}"));
    }
    fn start_again(&mut self, source_id: Option<&str>) {
        self.note(format!("start_again {source_id:?}"));
    }
    fn stop_stream(&mut self, source_id: Option<&str>) {
        self.note(format!("stop_stream {source_id:?}"));
    }
    fn stop_all_streams(&mut self) {
        self.note("stop_all_streams");
    }
    fn reconnect_stream(&mut self) {
        self.note("reconnect_stream");
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
        (None, None)
    }
    fn restart_vcam_canvas(&mut self, width: u32, height: u32) -> RequestId {
        self.note(format!("restart_vcam_canvas {width}x{height}"));
        self.request()
    }
    fn stream_output(&self, _source_id: &str) -> String {
        String::new()
    }
    fn stream_on_camera(&self, _slot: u32) -> Option<(String, Option<u32>, Option<u32>)> {
        None
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
}

impl HostCamera for FakeHost {
    fn is_camera_on(&self) -> bool {
        !self.camera_off
    }
    fn is_camera_off_auto(&self) -> bool {
        false
    }
    fn can_turn_camera_off(&self) -> (bool, String) {
        self.can_turn_off.clone().unwrap_or((false, "Not streaming".into()))
    }
    fn set_camera_on(&mut self, on: bool, auto: bool) {
        self.camera_off = !on;
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
    }
    fn clear_issue(&mut self, key: Option<&str>, _issue: Option<&Issue>) {
        self.note(format!("clear_issue {key:?}"));
    }
    fn send_notification(&mut self, title: &str, _body: &str, _urgent: bool) {
        self.note(format!("send_notification {title}"));
    }
    fn set_keep_in_tray(&mut self, keep: bool) {
        self.note(format!("set_keep_in_tray {keep}"));
    }
    fn toggle_window(&mut self) -> bool {
        self.note("toggle_window");
        true
    }
    fn quit_app(&mut self) {
        self.note("quit_app");
    }
    fn restart_app(&mut self) -> bool {
        self.note("restart_app");
        false
    }
    fn diagnostics_report(&self) -> String {
        String::new()
    }
    fn open_settings(&mut self, tab: &str) {
        self.note(format!("open_settings {tab}"));
    }
    fn shortcut_actions(&self) -> Vec<(String, ActionSpec)> {
        self.actions.clone()
    }
    fn run_action(&mut self, plugin: &str, id: &str, params: &Value) -> Option<String> {
        self.note(format!("run_action {plugin} {id} {params}"));
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
    }
    fn open_url(&mut self, url: &str) {
        self.note(format!("open_url {url}"));
    }
    fn pick_file(&mut self, dialog: &FileDialog) -> Option<RequestId> {
        self.note(format!("pick_file {:?} {}", dialog.kind, dialog.title));
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
}

impl HostView for FakeHost {
    fn view(&self) -> View {
        self.view.clone()
    }
    fn view_aspect(&self) -> f32 {
        0.0
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
        true
    }
    fn steer_view(&mut self, owner: &str, zoom: f32, pan_x: f32, pan_y: f32) {
        self.note(format!("steer_view {owner} {zoom} {pan_x} {pan_y}"));
    }
    fn release_view(&mut self, owner: &str) {
        self.note(format!("release_view {owner}"));
    }
}

impl HostVision for FakeHost {
    fn vision_state(&self) -> String {
        "off".into()
    }
    fn want_people(&mut self, owner: &str, on: bool) {
        self.note(format!("want_people {owner} {on}"));
    }
    fn tracked_people(&self, _source_id: Option<&str>) -> (Vec<Value>, u32) {
        (Vec::new(), 0)
    }
}

impl HostFrames for FakeHost {
    fn frames(&self) -> Arc<dyn FrameStore> {
        Arc::new(NoFrames)
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
    fn add_preview_layer(&mut self, owner: &str, _layer: PreviewLayer) {
        self.note(format!("add_preview_layer {owner}"));
    }
    fn remove_preview_layer(&mut self, owner: &str) {
        self.note(format!("remove_preview_layer {owner}"));
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
        None
    }
    fn take_still(&mut self, _options: &Value, _folder: &str, _source_id: Option<&str>) -> Option<RequestId> {
        None
    }
    fn still_busy(&self, _source_id: Option<&str>) -> bool {
        false
    }
}

impl HostColour for FakeHost {
    fn set_colour_stage(&mut self, owner: &str, stage: Option<ColourStage>, order: i32, _source_id: Option<&str>) {
        self.note(format!("set_colour_stage {owner} {} {order}", stage.is_some()));
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
        json!({"state": "off"})
    }
    fn sensors(&self) -> Arc<dyn SensorReader> {
        Arc::new(NoFrames)
    }
}

impl HostPackets for FakeHost {
    fn packets_problem(&self) -> String {
        String::new()
    }
    fn change_packets(&mut self, owner: &str, factory: Option<Arc<dyn PacketStepFactory>>) {
        self.note(format!("change_packets {owner} {}", factory.is_some()));
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
