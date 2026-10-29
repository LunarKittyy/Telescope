//! Everything a plugin may call on the host (HostServices in plugin.py), split the same way. Arguments and results are plain data.

use crate::event::Event;
use crate::frame::{Frame, StreamFrame};
use crate::source::{RouteSpec, StreamSource};
use crate::ui::{Issue, TrayAction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// What a file dialog asks for (QFileDialog's getOpenFileName, getSaveFileName and getExistingDirectory).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileDialogKind {
    Open,
    Save,
    Folder,
}

/// A file or folder to ask the person for. Texts are already translated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileDialog {
    pub kind: FileDialogKind,
    pub title: String,
    /// The folder it opens in, or a folder and file name for a Save.
    pub start: String,
    /// Qt's filter strings ("LUT files (*.cube)"); the one first is picked to begin with.
    #[serde(default)]
    pub filters: Vec<String>,
}

/// A request that finishes later; its Event::RequestDone (and any RequestProgress) carries the same id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub u64);

/// What's done to a stream's picture on this computer, before the plugins see it (telescope.view.View).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
    pub flip_h: bool,
    pub flip_v: bool,
    /// Degrees, one of view.ROTATIONS' values, or None for the phone's own.
    pub rotation: Option<i32>,
    pub stretch_x: f32,
    pub stretch_y: f32,
}

impl Default for View {
    fn default() -> Self {
        View { zoom: 1.0, pan_x: 0.0, pan_y: 0.0, flip_h: false, flip_v: false, rotation: None, stretch_x: 1.0, stretch_y: 1.0 }
    }
}

/// set_view's changes: only the fields that are Some change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewChange {
    pub zoom: Option<f32>,
    pub pan_x: Option<f32>,
    pub pan_y: Option<f32>,
    pub flip_h: Option<bool>,
    pub flip_v: Option<bool>,
    /// Some(None) goes back to the phone's own rotation.
    pub rotation: Option<Option<i32>>,
    pub stretch_x: Option<f32>,
    pub stretch_y: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickKind {
    Point,
    Rect,
    Quad,
}

/// A finished pick on the preview (telescope.overlays.Pick), points as (u, v) 0..1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pick {
    pub kind: PickKind,
    /// In the frame as shown when the pick ended.
    pub shown: Vec<[f32; 2]>,
    /// In the main lens's whole picture, before zoom, pan, flip and rotation.
    pub full: Vec<[f32; 2]>,
    /// Where they are in the picture the preview showed (the same as shown unless something moved the picture after).
    pub in_picture: Vec<[f32; 2]>,
}

/// Something drawn on the preview only (telescope.overlays.PreviewLayer), in 0..1 of the frame as shown.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PreviewLayer {
    pub shapes: Vec<Shape>,
}

/// Colours are "#rrggbb" or "#rrggbbaa".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum Shape {
    Rect { x0: f32, y0: f32, x1: f32, y1: f32, stroke: String, width: f32, fill: Option<String> },
    Polygon { points: Vec<[f32; 2]>, stroke: String, width: f32, fill: Option<String> },
    Line { points: Vec<[f32; 2]>, stroke: String, width: f32 },
    Text { x: f32, y: f32, text: String, color: String, size: f32 },
    /// Tints the picture outside the given area (a quad or rect being picked).
    Dim { points: Vec<[f32; 2]>, color: String },
}

/// One plugin's colour change for the colour pipeline (telescope.colour.ColourStage), baked into a 3D LUT.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColourStage {
    /// Points per side.
    pub size: u32,
    /// size³ RGB triples, red changing fastest, 0..1.
    pub rgb: Arc<Vec<f32>>,
    /// Only for a change where each channel's output depends on that channel's input alone (curves, gains): its
    /// output for every input level 0..255 as 256 RGB triples. A pipeline of only such stages is applied as exact
    /// tables instead of a 3D LUT, as per_channel does in Python.
    #[serde(default)]
    pub table: Option<Arc<Vec<f32>>>,
}

/// The newest frames of every stream, from any thread and without copying (HostFrames).
pub trait FrameStore: Send + Sync {
    /// Every running stream that has sent a picture, in the order they started.
    fn stream_frames(&self) -> Vec<StreamFrame>;
    fn stream_frame(&self, source_id: Option<&str>) -> Option<StreamFrame>;
    /// Sends frame out on source_id's virtual camera in place of the stream's newest one, numbered number; false once it's newer.
    fn replace_frame(&self, source_id: Option<&str>, frame: Frame, number: u64) -> bool;
}

/// Changes a stream's H.264 access units before they're decoded (HostPackets); one per stream, on its thread.
pub trait PacketStep: Send {
    /// The units to decode in place of this one ([] drops it).
    fn step(&mut self, unit: &[u8]) -> Vec<Vec<u8>>;
}

/// Makes a fresh PacketStep for each stream it's put on.
pub trait PacketStepFactory: Send + Sync {
    fn make(&self) -> Box<dyn PacketStep>;
}

/// The latest phone sensor readings, safe from any thread.
pub trait SensorReader: Send + Sync {
    fn reading(&self, source_id: Option<&str>) -> Option<Value>;
}

/// Saving and reading settings: this plugin's, another's, and each device's.
pub trait HostSettings {
    /// Persist all plugin config soon, coalescing rapid successive calls.
    fn schedule_save(&mut self);
    /// Persist all plugin config immediately, bypassing the debounce.
    fn save_now(&mut self);
    /// Another plugin's current get_config(), or None if it isn't registered.
    fn plugin_config(&self, name: &str) -> Option<Value>;
    /// Hand a saved config to another plugin's apply_preset(); unknown names are ignored.
    fn apply_preset(&mut self, name: &str, cfg: &Value);
    /// The names of the running plugins that are in_presets, in registration order.
    fn preset_plugins(&self) -> Vec<String>;
    /// A device-local plugin's settings saved for source_id's stream; {} without any.
    fn device_config(&self, source_id: Option<&str>, plugin_name: &str) -> Value;
    fn has_device_settings(&self, name: &str) -> bool;
    fn switch_device(&mut self, prev_name: Option<&str>, new_name: Option<&str>);
    fn forget_device_settings(&mut self, name: &str);
    /// Store a device's settings (presets included) and its virtual camera under a new id.
    fn move_device_settings(&mut self, old: &str, new: &str);
}

/// Output geometry for update_stream_output: None keeps the current value, Some(None) is pass-through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputChange {
    pub width: Option<Option<u32>>,
    pub height: Option<Option<u32>>,
    pub fps: Option<Option<u32>>,
}

/// A stream running behind the panels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackgroundStream {
    pub source_id: String,
    pub name: String,
}

/// Starting, stopping and following streams, the one the panels show and those behind it.
pub trait HostStreams {
    fn is_streaming(&self) -> bool;
    fn is_starting(&self) -> bool;
    fn is_restarting(&self) -> bool;
    fn is_streaming_from(&self, source_id: Option<&str>) -> bool;
    fn stream_count(&self) -> u32;
    /// As the Start button does; interactive=false never asks anything (problems go to a banner).
    fn start_stream(&mut self, interactive: bool);
    /// A banner's Try again: starts source_id (by default what is starting now) once more, next to any other stream.
    fn start_again(&mut self, source_id: Option<&str>);
    fn stop_stream(&mut self, source_id: Option<&str>);
    fn stop_all_streams(&mut self);
    fn reconnect_stream(&mut self);
    /// Restart source_id's stream, shown or not, to pick up changed connection settings (a new token).
    fn reconnect_source(&mut self, source_id: &str) {
        let _ = source_id;
        self.reconnect_stream();
    }
    /// Move a running phone stream onto another route without a gap; false if it can't try now.
    fn hand_over_stream(&mut self, route: &RouteSpec, source_id: Option<&str>) -> bool;
    fn focused_source_id(&self) -> Option<String>;
    fn background_streams(&self) -> Vec<BackgroundStream>;
    /// The phone picker moved to source_id; true if the host handled it.
    fn pick_source(&mut self, source_id: Option<&str>) -> bool;
    /// (id, name) of the stream plugins that don't follow the panels are on.
    fn main_stream(&self) -> Option<(String, String)>;
    fn make_main(&mut self, source_id: &str) -> bool;
    fn update_stream_output(&mut self, change: OutputChange, source_id: Option<&str>);
    /// The canvas Setup asks for, or (None, None) for Auto.
    fn canvas_size(&self) -> (Option<u32>, Option<u32>);
    /// Recreate the virtual camera at a new size; RequestDone says how it went (data: {"command": ...} to run by hand).
    fn restart_vcam_canvas(&mut self, width: u32, height: u32) -> RequestId;
    fn stream_output(&self, source_id: &str) -> String;
    /// (source id, width, height) of the stream going to this virtual camera slot.
    fn stream_on_camera(&self, slot: u32) -> Option<(String, Option<u32>, Option<u32>)>;
    fn remove_extra_cameras(&mut self) -> RequestId;
    /// Replaces ctrl.send(): one /v1/control body to source_id's stream (the shown one by default).
    fn phone_control(&mut self, source_id: Option<&str>, body: &Value);
    /// The last /v1/state source_id's stream answered with.
    fn phone_state(&self, source_id: Option<&str>) -> Option<Value>;
}

/// The phone's camera going off while its mic streams on, and back.
pub trait HostCamera {
    fn is_camera_on(&self) -> bool;
    fn is_camera_off_auto(&self) -> bool;
    /// Whether set_camera_on(false) would work now, and if not, why.
    fn can_turn_camera_off(&self) -> (bool, String);
    fn set_camera_on(&mut self, on: bool, auto: bool);
}

/// Places other than a paired phone to stream from.
pub trait HostSources {
    fn add_stream_source(&mut self, source: Arc<dyn StreamSource>);
    fn remove_stream_source(&mut self, source_id: &str);
    fn stream_source(&mut self, source_id: &str);
}

/// The window around the plugins: banners, notifications, the tray and quitting.
pub trait HostShell {
    fn show_issue(&mut self, key: &str, issue: Issue);
    /// One banner, or all of them; with issue, only while key still shows that one.
    fn clear_issue(&mut self, key: Option<&str>, issue: Option<&Issue>);
    fn send_notification(&mut self, title: &str, body: &str, urgent: bool);
    fn set_keep_in_tray(&mut self, keep: bool);
    fn toggle_window(&mut self) -> bool;
    fn quit_app(&mut self);
    fn restart_app(&mut self) -> bool;
    fn diagnostics_report(&self) -> String;
    fn open_settings(&mut self, tab: &str);
    /// Every plugin's actions, in registration order, as (plugin, action).
    fn shortcut_actions(&self) -> Vec<(String, crate::ui::ActionSpec)>;
    /// Runs another plugin's action as a key would (Shortcuts and `--action` use it): what to say about it, or None
    /// when it can't right now.
    fn run_action(&mut self, _plugin: &str, _id: &str, _params: &Value) -> Option<String> {
        None
    }
    /// The key held for another plugin's action came back up (Plugin::release_action).
    fn release_action(&mut self, _plugin: &str, _id: &str, _params: &Value) {}
    /// Whether one of Telescope's windows is the one in front (QApplication.activeWindow()).
    fn window_active(&self) -> bool {
        false
    }
    fn copy_text(&mut self, _text: &str) {}
    /// Opens a link in the person's browser (QDesktopServices.openUrl). A host without one does nothing.
    fn open_url(&mut self, _url: &str) {}
    /// "Add keyboard shortcut…" on a right-click of controls that name an action, with the saved shortcuts listed
    /// under it; None takes it off. Its items come back to this plugin as on_ui with ui "shortcut-menu" and callback
    /// "add" (the action id) or "edit" (the binding id).
    fn set_shortcut_menu(&mut self, _bindings: Option<Vec<crate::ui::ShortcutEntry>>) {}
    /// Asks the person for a file or folder. Event::RequestDone says how it went: ok with data {"path", "filter"} for
    /// the pick, not ok with an empty message when it was cancelled. None on a host that can't show a dialog.
    fn pick_file(&mut self, _dialog: &FileDialog) -> Option<RequestId> {
        None
    }
}

/// Changing what a plugin put on screen after it's built.
pub trait HostUi {
    /// Sets properties on one of this plugin's pieces of UI ("card", "header" or a section id).
    fn set_ui_props(&mut self, ui: &str, props: &Value);
    fn update_tray_action(&mut self, action: &TrayAction);
    /// This plugin's frame step changed; the host asks for a fresh one with Plugin::frame_step.
    fn refresh_frame_step(&mut self);
}

/// The shown stream's view: zoom, pan, flip, rotation and stretch, done before the plugins see the frame.
pub trait HostView {
    fn view(&self) -> View;
    /// Width over height of the shown stream's picture as shown, 0 before its first frame.
    fn view_aspect(&self) -> f32;
    fn set_view(&mut self, change: ViewChange);
    fn take_view(&mut self, owner: &str) -> bool;
    fn steer_view(&mut self, owner: &str, zoom: f32, pan_x: f32, pan_y: f32);
    fn release_view(&mut self, owner: &str);
}

/// People in the picture. person_mask is on FrameContext, since it's only for frame steps.
pub trait HostVision {
    /// "off", "loading", "ready", "missing" or "failed".
    fn vision_state(&self) -> String;
    fn want_people(&mut self, owner: &str, on: bool);
    /// (Track records, how many people) on source_id's stream, as the last people_tracked said.
    fn tracked_people(&self, source_id: Option<&str>) -> (Vec<Value>, u32);
}

/// Frames from other streams.
pub trait HostFrames {
    /// A handle to keep and use from any thread.
    fn frames(&self) -> Arc<dyn FrameStore>;
}

/// Something moving in parts of the shown stream's picture.
pub trait HostMotion {
    /// zones as (x0, y0, x1, y1) 0..1, sensitivity 1 to 10; empty zones stop it.
    fn watch_motion(&mut self, owner: &str, zones: &[[f32; 4]], sensitivity: u32);
    fn motion(&self, owner: &str) -> Vec<u32>;
}

/// Drawing on the preview without it reaching the virtual camera, and asking for points and areas there.
pub trait HostPreview {
    fn add_preview_layer(&mut self, owner: &str, layer: PreviewLayer);
    fn remove_preview_layer(&mut self, owner: &str);
    fn refresh_preview(&mut self);
    /// Starts a pick; Event::PreviewPicked brings the result. False if there's no preview to pick on.
    fn pick_on_preview(&mut self, owner: &str, kind: PickKind, start: Option<[[f32; 2]; 4]>, hint: &str) -> bool;
    fn finish_pick(&mut self, owner: &str) -> bool;
    fn cancel_pick(&mut self, owner: &str);
    /// A 3 x 3 homography (row-major) from the frame as shown to what comes out, or None once it stops.
    fn set_preview_warp(&mut self, owner: &str, matrix: Option<[f32; 9]>);
}

/// Full-quality photos taken on the phone.
pub trait HostStills {
    fn still_caps(&self, source_id: Option<&str>) -> Option<Value>;
    /// Takes a photo with the /v1/still options into folder; None when it can't start. Progress and the result come as events.
    fn take_still(&mut self, options: &Value, folder: &str, source_id: Option<&str>) -> Option<RequestId>;
    fn still_busy(&self, source_id: Option<&str>) -> bool;
}

/// The colour pipeline: every plugin's colour changes baked into one LUT per stream.
pub trait HostColour {
    fn set_colour_stage(&mut self, owner: &str, stage: Option<ColourStage>, order: i32, source_id: Option<&str>);
    fn colour_split(&mut self, owner: &str, position: Option<f32>);
}

/// The shown stream's phone's motion and light sensors.
pub trait HostSensors {
    /// Kinds from telescope.sensors.KINDS; an empty list lets go.
    fn want_sensors(&mut self, owner: &str, kinds: &[String]);
    fn sensor_status(&self, source_id: Option<&str>) -> Value;
    /// A handle for reading from any thread.
    fn sensors(&self) -> Arc<dyn SensorReader>;
}

/// The shown stream's H.264 before it's decoded.
pub trait HostPackets {
    /// Why the shown stream's packets can't be changed, or "" when they can.
    fn packets_problem(&self) -> String;
    fn change_packets(&mut self, owner: &str, factory: Option<Arc<dyn PacketStepFactory>>);
}

/// Hands results from a plugin's own threads back to the host thread (Python's queued Qt signals). Plain data, so a
/// Lua or WASM plugin's threads can use it too.
pub trait Poster: Send + Sync {
    /// From any thread: Event::Posted with this token and data soon reaches the plugin the poster belongs to, on the
    /// host thread. Posts arrive in the order sent; after the plugin is gone they're dropped.
    fn post(&self, token: &str, data: Value);
}

/// Everything a plugin may call on the host. Handed to each hook rather than kept, so nothing holds the host between calls.
pub trait Host:
    HostSettings + HostStreams + HostCamera + HostSources + HostShell + HostUi + HostView + HostVision + HostFrames
    + HostMotion + HostPreview + HostStills + HostColour + HostSensors + HostPackets
{
    /// Queue an event for every plugin (delivered after the current hook returns, in order).
    fn emit(&mut self, event: Event);
    /// Whether a plugin by this name is loaded and switched on.
    fn has_plugin(&self, name: &str) -> bool;

    /// Event::TimerFired with this token reaches this plugin (only) after after_ms, once (QTimer.singleShot; schedule
    /// again to repeat). Scheduling a token that's still pending moves it instead of adding a second one.
    fn schedule(&mut self, _after_ms: u64, _token: &str) {}
    /// Drops this plugin's pending timer with this token, if any.
    fn cancel_schedule(&mut self, _token: &str) {}
    /// A handle for this plugin's own threads to post results back through; None on a host without one.
    fn poster(&self) -> Option<Arc<dyn Poster>> {
        None
    }
}
