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
    /// Lens correction, -1 to 1: above 0 straightens the barrel of a wide lens, below 0 the pincushion of a long one. 0 changes nothing.
    #[serde(default)]
    pub distortion: f32,
}

impl Default for View {
    fn default() -> Self {
        View { zoom: 1.0, pan_x: 0.0, pan_y: 0.0, flip_h: false, flip_v: false, rotation: None, stretch_x: 1.0, stretch_y: 1.0, distortion: 0.0 }
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
    pub distortion: Option<f32>,
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
    /// The picture the preview showed when it ended, before any tint: colours are read from this very frame. None without
    /// one; it stays in the process, a plugin elsewhere gets the points alone.
    #[serde(skip)]
    pub picture: Option<crate::frame::Frame>,
}

/// Something drawn on the preview only (telescope.overlays.PreviewLayer), in 0..1 of the frame as shown.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PreviewLayer {
    pub shapes: Vec<Shape>,
}

/// A translated message on this stream's preview only; max_ms limits how long the host keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreviewMessage {
    pub text: String,
    pub detail: String,
    pub step: String,
    pub remaining_ms: Option<i64>,
    pub total_ms: Option<i64>,
    pub dim: bool,
    pub max_ms: i64,
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

/// The function inside a StageFn.
pub type StageFnBox = Arc<dyn Fn(&mut [f32]) + Send + Sync>;

/// How to work a colour stage out exactly at any colours: RGB triples 0..1 in, the changed ones out in place (colour.ColourStage.fn).
/// A stage that has one is evaluated at the previous stage's output when stages are put one after the other; one that doesn't
/// (a plugin in another process) is read off its baked lattice.
#[derive(Clone, Default)]
pub struct StageFn(pub Option<StageFnBox>);

impl std::fmt::Debug for StageFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "StageFn(..)" } else { "StageFn(none)" })
    }
}

impl PartialEq for StageFn {
    fn eq(&self, other: &StageFn) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
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
    /// The change itself, when the plugin can hand it over (it can't cross into another process).
    #[serde(skip)]
    pub eval: StageFn,
}

impl ColourStage {
    /// A stage known only by its baked lattice (and table).
    pub fn new(size: u32, rgb: Arc<Vec<f32>>, table: Option<Arc<Vec<f32>>>) -> ColourStage {
        ColourStage { size, rgb, table, eval: StageFn::default() }
    }

    /// The same stage with the change itself, so stages after another one are worked out at its output.
    pub fn with_eval(mut self, eval: impl Fn(&mut [f32]) + Send + Sync + 'static) -> ColourStage {
        self.eval = StageFn(Some(Arc::new(eval)));
        self
    }
}

/// The newest frames of every stream, from any thread and without copying (HostFrames).
pub trait FrameStore: Send + Sync {
    /// Every running stream that has sent a picture, in the order they started.
    fn stream_frames(&self) -> Vec<StreamFrame>;
    fn stream_frame(&self, source_id: Option<&str>) -> Option<StreamFrame>;
    /// Sends frame out on source_id's virtual camera in place of the stream's newest one, numbered number; false once it's newer.
    fn replace_frame(&self, source_id: Option<&str>, frame: Frame, number: u64) -> bool;
    /// The frame of source_id's stream (None: the one with no phone picked) from its recent history whose Frame::capture_ns is nearest
    /// to capture_ns (this computer's monotonic clock; the newer one when two are as near). None when the stream has no frames with a capture
    /// time, or nothing asked to keep any (HostFrames::want_frame_history): without that only its newest frame is held. Frames made before the
    /// latest pipeline_epoch change are never handed out: they may have gone out before a step that hides things came on.
    fn stream_frame_near(&self, _source_id: Option<&str>, _capture_ns: u64) -> Option<StreamFrame> {
        None
    }
    /// What source_id's virtual camera is sending right now: the stream's own newest frame, or what replace_frame put in its place. None until
    /// the stream has sent one. The number is what replace_frame wants.
    fn outgoing_frame(&self, source_id: Option<&str>) -> Option<StreamFrame> {
        self.stream_frame(source_id)
    }
    /// The same as replace_frame, but only while what the camera sends is still the picture whose pixels are `expected` (outgoing_frame's frame.data):
    /// something else put another one out since, and this must not undo that. A store that can do both at once should; this one looks first.
    fn replace_frame_if(&self, source_id: Option<&str>, expected: &Arc<Vec<u8>>, frame: Frame, number: u64) -> bool {
        self.outgoing_frame(source_id).is_some_and(|s| Arc::ptr_eq(&s.frame.data, expected)) && self.replace_frame(source_id, frame, number)
    }
    /// Goes up whenever any stream's frame steps are handed out again (a Blur came on, a plugin changed its step), for every stream at once:
    /// what a plugin kept of a stream's frames from an earlier epoch may have missed a step that hides things, so it must not be put on show.
    fn pipeline_epoch(&self) -> u64 {
        0
    }
    /// How well source_id's capture times line up with this computer's clock, None while that isn't known: the stream has no capture times,
    /// or its phone hasn't answered a round trip yet.
    fn clock_sync(&self, _source_id: Option<&str>) -> Option<crate::clock::ClockSync> {
        None
    }
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

/// The looks the window can wear, in the order Appearance lists them. The index is what the window's `Theme.look` takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Look {
    /// The dark look
    #[default]
    Dark,
    /// The deep blue of 5.0
    Classic,
    /// True black, layers told apart by their outlines
    Oled,
    Light,
}

impl Look {
    pub const ALL: [Look; 4] = [Look::Dark, Look::Classic, Look::Oled, Look::Light];

    /// The look at this place in Appearance's list; Dark for anything else.
    pub fn from_index(index: i64) -> Look {
        Self::ALL.get(usize::try_from(index).unwrap_or(usize::MAX)).copied().unwrap_or_default()
    }

    pub fn index(self) -> usize {
        self as usize
    }

    /// What the settings file calls it.
    pub fn name(self) -> &'static str {
        match self {
            Look::Dark => "dark",
            Look::Classic => "classic",
            Look::Oled => "oled",
            Look::Light => "light",
        }
    }

    pub fn from_name(name: &str) -> Option<Look> {
        Self::ALL.into_iter().find(|l| l.name() == name)
    }
}

/// Corner roundness: what `Theme.radius` takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Corners {
    Sharp,
    #[default]
    Normal,
    Round,
}

/// Text size: what `Theme.body` takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextSize {
    Small,
    #[default]
    Normal,
    Large,
}

/// Density: what `Theme.unit` takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Density {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

/// Which motions may play: the system's own setting, or one the person picked over it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MotionChoice {
    #[default]
    System,
    Full,
    Reduced,
    Off,
}

/// The longest wait before a tooltip, in milliseconds (the slider's far end).
pub const TIP_DELAY_MAX_MS: u32 = 1500;

/// The options of Appearance that scale or swap a design token, each in a few fixed steps. The window sets its globals from this;
/// the look and the card colours have their own calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Appearance {
    pub corners: Corners,
    pub text: TextSize,
    pub density: Density,
    /// Full colours at rest, near-white text and lines
    pub contrast: bool,
    pub motion: MotionChoice,
    /// How long the pointer rests before a tooltip shows, in milliseconds
    pub tip_delay_ms: u32,
    /// The next tooltip shows at once while one is up or just went down
    pub tip_warm: bool,
}

impl Default for Appearance {
    fn default() -> Appearance {
        Appearance { corners: Corners::Normal, text: TextSize::Normal, density: Density::Normal, contrast: false, motion: MotionChoice::System, tip_delay_ms: 500, tip_warm: true }
    }
}

impl Appearance {
    /// `Theme.radius` in logical pixels.
    pub fn radius(&self) -> f64 {
        match self.corners {
            Corners::Sharp => 0.0,
            Corners::Normal => 6.0,
            Corners::Round => 10.0,
        }
    }

    /// `Theme.body` in logical pixels.
    pub fn body(&self) -> f64 {
        match self.text {
            TextSize::Small => 12.0,
            TextSize::Normal => 13.0,
            TextSize::Large => 15.0,
        }
    }

    /// `Theme.unit` in logical pixels.
    pub fn unit(&self) -> f64 {
        match self.density {
            Density::Compact => 1.75,
            Density::Normal => 2.0,
            Density::Comfortable => 2.25,
        }
    }

    /// What the settings file holds: only what is not at its default, so a config that never touched Appearance stays as it was.
    pub fn to_json(&self) -> Option<Value> {
        let was = Appearance::default();
        let mut out = serde_json::Map::new();
        let mut put = |key: &str, changed: bool, value: Value| {
            if changed {
                out.insert(key.into(), value);
            }
        };
        put("corners", self.corners != was.corners, Value::from(match self.corners { Corners::Sharp => "sharp", Corners::Normal => "default", Corners::Round => "round" }));
        put("text", self.text != was.text, Value::from(match self.text { TextSize::Small => "small", TextSize::Normal => "default", TextSize::Large => "large" }));
        put("density", self.density != was.density, Value::from(match self.density { Density::Compact => "compact", Density::Normal => "default", Density::Comfortable => "comfortable" }));
        put("contrast", self.contrast != was.contrast, Value::from(self.contrast));
        put("motion", self.motion != was.motion, Value::from(match self.motion { MotionChoice::System => "system", MotionChoice::Full => "full", MotionChoice::Reduced => "reduced", MotionChoice::Off => "off" }));
        put("tip_delay_ms", self.tip_delay_ms != was.tip_delay_ms, Value::from(self.tip_delay_ms));
        put("tip_warm", self.tip_warm != was.tip_warm, Value::from(self.tip_warm));
        (!out.is_empty()).then_some(Value::Object(out))
    }

    /// What `to_json` wrote; anything missing or not understood is the default.
    pub fn from_json(value: Option<&Value>) -> Appearance {
        let get = |key: &str| value.and_then(|v| v.get(key));
        let word = |key: &str| get(key).and_then(Value::as_str).unwrap_or("");
        Appearance {
            corners: match word("corners") { "sharp" => Corners::Sharp, "round" => Corners::Round, _ => Corners::Normal },
            text: match word("text") { "small" => TextSize::Small, "large" => TextSize::Large, _ => TextSize::Normal },
            density: match word("density") { "compact" => Density::Compact, "comfortable" => Density::Comfortable, _ => Density::Normal },
            contrast: get("contrast") == Some(&Value::Bool(true)),
            motion: match word("motion") { "full" => MotionChoice::Full, "reduced" => MotionChoice::Reduced, "off" => MotionChoice::Off, _ => MotionChoice::System },
            tip_delay_ms: get("tip_delay_ms").and_then(Value::as_u64).map_or(500, |ms| ms.min(u64::from(TIP_DELAY_MAX_MS)) as u32),
            tip_warm: get("tip_warm") != Some(&Value::Bool(false)),
        }
    }
}

/// Saving and reading settings: this plugin's, another's, and each device's.
pub trait HostSettings {
    /// Persist all plugin config soon, coalescing rapid successive calls.
    fn schedule_save(&mut self);
    /// Persist all plugin config immediately, bypassing the debounce.
    fn save_now(&mut self);
    /// Another plugin's current get_config(), or None if it isn't registered.
    fn plugin_config(&self, name: &str) -> Option<Value>;
    /// What a preset saves of a plugin (Plugin::preset_config).
    fn plugin_preset_config(&self, name: &str) -> Option<Value> {
        self.plugin_config(name)
    }
    /// The questions a plugin adds to the Save preset dialog (Plugin::preset_options).
    fn plugin_preset_options(&self, _name: &str) -> Vec<crate::plugin::PresetOption> {
        Vec::new()
    }
    /// What a preset saves of a plugin given the answers to its options (Plugin::preset_config_with).
    fn plugin_preset_config_with(&self, name: &str, _picks: &[(String, String)]) -> Option<Value> {
        self.plugin_preset_config(name)
    }
    /// Hand a saved config to another plugin's apply_preset(); unknown names are ignored.
    fn apply_preset(&mut self, name: &str, cfg: &Value);
    /// Whether the person asked for less motion: (reduced, off). Motion in the picture follows it as the windows do.
    fn reduced_motion(&self) -> (bool, bool) {
        (false, false)
    }
    /// The names of the running plugins that are in_presets, in registration order.
    fn preset_plugins(&self) -> Vec<String>;
    /// A device-local plugin's settings saved for source_id's stream; {} without any.
    fn device_config(&self, source_id: Option<&str>, plugin_name: &str) -> Value;
    fn has_device_settings(&self, name: &str) -> bool;
    fn switch_device(&mut self, prev_name: Option<&str>, new_name: Option<&str>);
    fn forget_device_settings(&mut self, name: &str);
    /// Store a device's settings (presets included) and its virtual camera under a new id too. The old id's stay until forget_device_settings.
    fn move_device_settings(&mut self, old: &str, new: &str);
    /// The running plugins that declare settings a tool outside the window may change (Plugin::settings), in registration order.
    fn plugin_settings(&self) -> Vec<PluginSettings> {
        Vec::new()
    }
    /// When a plugin's setting last moved through the plugin itself (its own save, or a preset handed to it), as a number that only goes up: a setting is
    /// still as somebody left it while this is the same as it was then, even when the value has been the same all along. `key` is a dotted path in its
    /// get_config(), and anything inside it or around it counts. 0 for a plugin that never moved or a host that doesn't keep track.
    fn setting_revision(&self, _plugin: &str, _key: &str) -> u64 {
        0
    }
    /// The plugins that are running, as (name, title).
    fn running_plugins(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// How each registered plugin is shown to people: its display title (in English) and its icon by name, for a list that names plugins (Stats).
    fn plugin_looks(&self) -> Vec<PluginLook> {
        Vec::new()
    }
    /// Changes declared settings of a running plugin, given as {"key": value}. All of them are checked first and nothing changes unless
    /// every one is fine (the problems come back together). The change goes in through the plugin's own apply_preset, so its checks
    /// apply too, and is saved. Gives the plugin's settings afterwards.
    fn change_plugin_settings(&mut self, name: &str, changes: &serde_json::Map<String, Value>) -> Result<Value, Vec<String>> {
        let Some(group) = self.plugin_settings().into_iter().find(|g| g.name == name) else {
            return Err(vec![format!("{name} has no settings to change here (it may be switched off)")]);
        };
        let config = self.plugin_config(name).unwrap_or(Value::Null);
        let merged = crate::settings::merge_changes(&group.settings, &config, changes)?;
        self.apply_preset(name, &merged);
        self.schedule_save();
        let after = self.plugin_config(name).unwrap_or(merged);
        let kept: Vec<String> = changes.keys().filter_map(|key| {
            let spec = group.settings.iter().find(|s| s.key == *key)?;
            let (wanted, got) = (spec.check(&changes[key]).ok()?, spec.read(&after));
            (!crate::settings::same_value(&wanted, &got)).then(|| format!("{name} kept {key} at {got} instead of {wanted}"))
        }).collect();
        if kept.is_empty() { Ok(after) } else { Err(kept) }
    }
}

/// A plugin as people see it: what the Plugins tab calls it and its icon.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginLook {
    pub name: String,
    pub title: String,
    pub icon: String,
}

/// A running plugin's declared settings.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginSettings {
    pub name: String,
    pub title: String,
    pub settings: Vec<crate::settings::SettingSpec>,
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
    /// Its session: a stream restarted since a reading was asked for has a new one.
    #[serde(default)]
    pub session: u64,
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
    /// A RequestDone says how it went. shows_command: the plugin shows a command to run by hand itself; otherwise a banner does.
    fn restart_vcam_canvas(&mut self, width: u32, height: u32, shows_command: bool) -> RequestId;
    fn stream_output(&self, source_id: &str) -> String;
    /// (source id, width, height) of the stream going to this virtual camera slot.
    fn stream_on_camera(&self, slot: u32) -> Option<(String, Option<u32>, Option<u32>)>;
    fn remove_extra_cameras(&mut self) -> RequestId;
    /// Replaces ctrl.send(): one /v1/control body to source_id's stream (the shown one by default).
    fn phone_control(&mut self, source_id: Option<&str>, body: &Value);
    /// The last /v1/state source_id's stream answered with.
    fn phone_state(&self, source_id: Option<&str>) -> Option<Value>;
    /// Asks source_id's phone (the shown one by default) for its /v1/state now; it comes to on_phone_state as the
    /// stream's other states do, and not at all once that stream has stopped.
    fn request_phone_state(&mut self, source_id: Option<&str>);
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
    /// own: the lines of the plugin asking, which is busy with this call.
    fn diagnostics_report(&self, own: &[(String, String)]) -> String;
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
    /// What's on the clipboard now, to check that a copy_text reached it (QGuiApplication.clipboard().text(): a window in
    /// the background can't put anything there on Wayland). None when the host can't read it.
    fn clipboard_text(&mut self) -> Option<String> {
        None
    }
    /// Opens a link in the person's browser (QDesktopServices.openUrl). A host without one does nothing.
    fn open_url(&mut self, _url: &str) {}
    /// "Add keyboard shortcut…" on a right-click of controls that name an action, with the saved shortcuts listed
    /// under it; None takes it off. Its items come back to this plugin as on_ui with ui "shortcut-menu" and callback
    /// "add" (the action id) or "edit" (the binding id).
    fn set_shortcut_menu(&mut self, _bindings: Option<Vec<crate::ui::ShortcutEntry>>) {}
    /// Whether every card gets its group's colour (true) or the accent colour (theme.set_plugin_hues). A host without a
    /// window ignores it.
    fn set_plugin_hues(&mut self, _on: bool) {}
    /// Which look the window draws (Appearance in Settings). A host without a window ignores it.
    fn set_look(&mut self, _look: Look) {}
    /// The other Appearance options: corners, text size, density, contrast, motion and tooltip timing. A host without a window ignores it.
    fn set_appearance(&mut self, _appearance: Appearance) {}
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
    /// Sets an image property (`in property <image>`) on one of this plugin's pieces of UI to this picture, which a JSON
    /// property can't carry. Only the RGBA and BGRA formats show; a host with no window ignores it.
    fn set_ui_image(&mut self, _ui: &str, _prop: &str, _frame: &Frame) {}
    /// This plugin's actions changed (a list a param offers, say): the host asks Plugin::actions again.
    fn refresh_actions(&mut self) {}
    /// Opens a window of this plugin's own beside the main one (a dialog that isn't modal, like Composite's Edit picture), or
    /// brings it forward when it's open. window.ui's component inherits Window and sets its title. It goes by id as a piece of UI: set_ui_props(id)
    /// reaches it, its callbacks come to Plugin::on_ui with ui = id, and "closed" comes once it's closed. A host with no window
    /// ignores it.
    fn open_window(&mut self, _id: &str, _window: &crate::ui::WindowSpec) {}
    /// Closes this plugin's window with this id, if it's open ("closed" still comes).
    fn close_window(&mut self, _id: &str) {}
    /// The readouts this plugin can put in the bottom bar, as they read now; each call replaces the last. A pinned one shows its value
    /// and one with an empty value takes no room. Offer nothing (or switch the plugin off) and its pins vanish quietly, to come back
    /// with the plugin. The ids are kept in the settings, so they stay the same from run to run.
    fn offer_bar_stats(&mut self, _stats: &[crate::bar::BarStat]) {}
    /// The ids pinned to the bottom bar now, left side first, each side in the order it is drawn.
    fn bar_pins(&self) -> Vec<String> {
        Vec::new()
    }
    /// Pins a readout to the bar (at the inner end of its side, the right one for a new pin) or takes it off. Event::BarPinsChanged
    /// follows when something changed. The camera stays pinned while two or more streams run.
    fn pin_to_bar(&mut self, _id: &str, _pinned: bool) {}
    /// What the bar reads for this id now, from any plugin or the window's own (see core::bar), or None while nothing offers it.
    fn bar_stat(&self, _id: &str) -> Option<crate::bar::BarStat> {
        None
    }
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
    /// Whether the owner's people are enough as faces with shoulders (Auto framing at Head or Chest), so lighter effects can follow faces while every owner says so.
    fn want_heads(&mut self, _owner: &str, _on: bool) {}
    /// (Track records, how many people) on source_id's stream, as the last people_tracked said.
    fn tracked_people(&self, source_id: Option<&str>) -> (Vec<Value>, u32);
    /// Power-saving effects (Settings > Video): the models run less often. A host without vision ignores it.
    fn set_power_saving(&mut self, _on: bool) {}
    /// Where the vision pack's download is, for the card's status row (crate::vision::status_text).
    fn vision_pack(&self) -> crate::vision::VisionPack {
        crate::vision::VisionPack::default()
    }
    /// The status row's Download button: fetch the vision pack in the background (vision_download_finished says how it went).
    fn download_vision(&mut self) {}
    /// How the high quality cutout's model is doing: "off" until a frame step asks for it, then "loading", "ready", "missing" (not downloaded) or
    /// "failed". A host without it says "off".
    fn matting_state(&self) -> String {
        crate::vision::OFF.into()
    }
    /// Where that model's own download is, for the Background card's note.
    fn matting_pack(&self) -> crate::vision::VisionPack {
        crate::vision::VisionPack::default()
    }
    /// The note's Download button: fetch the high quality cutout in the background (vision_download_progress and vision_download_finished
    /// say how it goes, with no plugin to switch on).
    fn download_matting(&mut self) {}
}

/// Frames from other streams.
pub trait HostFrames {
    /// A handle to keep and use from any thread.
    fn frames(&self) -> Arc<dyn FrameStore>;
    /// owner needs the stream's frames to keep going through the frame steps even while no app reads the virtual camera and nobody looks at the
    /// window: a step that reads pictures and reports what it found (the QR code reader), or a plugin that draws from other streams' frames. While
    /// any owner wants them nothing is paused; without one the host may stop running the steps and vision on frames nobody sees, and carries on at
    /// once when an app opens the camera. Ask only while the work is needed and let go after.
    fn want_frames(&mut self, _owner: &str, _on: bool) {}
    /// owner needs each stream's last `frames` frames kept, so FrameStore::stream_frame_near can find the one taken at a given moment (pairing the
    /// pictures of two cameras). Frames are shared, not copied, so a few cost nothing to hold, but they aren't recycled while held: ask only while
    /// the work is needed and let go with 0. The host keeps as many as the biggest ask, and none while nobody asks.
    fn want_frame_history(&mut self, _owner: &str, _frames: usize) {}
}

/// Something moving in parts of the shown stream's picture.
pub trait HostMotion {
    /// zones as (x0, y0, x1, y1) 0..1, sensitivity 1 to 10; empty zones stop it.
    fn watch_motion(&mut self, owner: &str, zones: &[[f32; 4]], sensitivity: u32);
    fn motion(&self, owner: &str) -> Vec<u32>;
}

/// Drawing on the preview without it reaching the virtual camera, and asking for points and areas there.
pub trait HostPreview {
    /// Replaces this owner's message on source_id (None: the shown stream); None clears it, and hosts without a preview ignore it.
    fn set_preview_message(&mut self, _owner: &str, _source_id: Option<&str>, _message: Option<PreviewMessage>) {}
    fn add_preview_layer(&mut self, owner: &str, layer: PreviewLayer);
    /// Takes off whatever this owner put on the preview, a layer or a pass.
    fn remove_preview_layer(&mut self, owner: &str);
    fn refresh_preview(&mut self);
    /// Puts a pass on the preview for this owner (PreviewLayer's tint, analyse and paint), in place of its earlier one. A host
    /// without a preview ignores it.
    fn add_preview_pass(&mut self, _owner: &str, _pass: Arc<dyn crate::preview::PreviewPass>) {}
    /// Starts a pick; Event::PreviewPicked brings the result. False if there's no preview to pick on.
    fn pick_on_preview(&mut self, owner: &str, kind: PickKind, start: Option<[[f32; 2]; 4]>, hint: &str) -> bool;
    fn finish_pick(&mut self, owner: &str) -> bool;
    fn cancel_pick(&mut self, owner: &str);
    /// A 3 x 3 homography (row-major) from the frame as shown to what comes out, or None once it stops.
    fn set_preview_warp(&mut self, owner: &str, matrix: Option<[f32; 9]>);
    /// A point on the preview back to the frame as shown, past a warp.
    fn unwarp(&self, u: f64, v: f64) -> (f64, f64) {
        (u, v)
    }
    /// While source_id's stream is the one the panels show, the preview draws target's output (what its virtual camera sends) instead of its own:
    /// the cards still act on source_id's stream. None as the target takes the owner's redirect off, and so does the owner going away. An owner
    /// has one at a time; a host without a preview ignores it.
    fn preview_follows(&mut self, _owner: &str, _source_id: Option<&str>, _target: Option<&str>) {}
    /// Whether the preview draws the shown stream's own camera picture. It doesn't while a plugin has it draw another running stream
    /// (preview_follows) or while the picture is made of several streams (preview_composited): a spot picked on it is then not a spot of
    /// that stream's picture, and the people found in it are not the ones on screen.
    fn preview_shows_own_picture(&self) -> bool {
        true
    }
    /// A plugin says the picture stream source_id sends is made of several (Stereo's 3D picture, Composite's picture), not one camera's
    /// view, or None that it no longer is. An owner has one at a time.
    fn preview_composited(&mut self, _owner: &str, _source_id: Option<&str>) {}
}

/// Full-quality photos from a phone or a camera that supports stills.
pub trait HostStills {
    /// Available photo settings, including optional shot ranges and numeric DSLR choices with their raw labels.
    fn still_caps(&self, source_id: Option<&str>) -> Option<Value>;
    /// Takes a photo with /v1/still options and optional neutral shot overrides into folder; None when it can't start, otherwise progress and results include applied settings and DSLR restore errors.
    fn take_still(&mut self, options: &Value, folder: &str, source_id: Option<&str>) -> Option<RequestId>;
    /// Cancels this plugin's pending photo; its thread discards the result and tells the phone to forget it.
    fn cancel_still(&mut self, request: RequestId);
    fn still_busy(&self, source_id: Option<&str>) -> bool;
    /// Whether pixels of that stream (the shown one for None) are being hidden now (a privacy blur, the wait screen, a background). A
    /// plugin that hides tells the host through `set_hiding`. One whose frame steps exist and that never has told it counts as hiding. A
    /// photo comes straight off the phone and never passes through those steps. The options a photo is taken with can say
    /// `"skip_if_hidden": true`, which makes the host check again just before the request leaves for the phone and end it with a RequestDone
    /// whose data has `"code": "hidden"` instead of taking it.
    fn picture_hidden(&self, _source_id: Option<&str>) -> bool {
        false
    }
    /// A plugin with a Privacy, Cover or Background frame step says whether it hides the stream's pixels now (None: every stream), each
    /// time that changes. It counts at once, not when the steps are next built. The host keeps it per owner and stream. A device-local plugin
    /// holds the shown camera's settings, so its report without a stream is that camera's only, and it reports again whenever the shown camera changes.
    fn set_hiding(&mut self, _owner: &str, _source_id: Option<&str>, _on: bool) {}
    /// A flag that says what picture_hidden says for that stream, kept current by the host as hiding changes, for a frame step to read on
    /// the stream's own thread. None from a host that has none.
    fn hidden_flag(&mut self, _source_id: &str) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        None
    }
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
    /// The expensive stats (each plugin's frame step and hook times) are measured while any plugin wants them; the cheap ones are always counted.
    fn want_stats(&mut self, _owner: &str, _on: bool) {}
    /// The whole computer's load (processor, graphics card, memory, power, the user's busiest programs) is read into each stats snapshot while any plugin wants it. Live only: never logged, saved or sent.
    fn want_system(&mut self, _owner: &str, _on: bool) {}
    /// Whether the effects step down by themselves while frames run late (a saved setting). Off, only Power-saving effects holds them down.
    fn set_effects_step_down(&mut self, _on: bool) {}
    /// Keeps the effects at full quality for this run of the app, whatever stepping down says. Not saved.
    fn hold_effects_full(&mut self, _on: bool) {}
    /// Whether people may be followed by their faces while the effects are lighter (a saved switch, or kept off for one run). Not saved here.
    fn allow_heads(&mut self, _on: bool) {}
    /// Keeps frame steps that can run either way on the graphics card instead of letting them move to the processor while it is busy. Not saved here: the asking plugin saves it.
    fn keep_steps_on_card(&mut self, _on: bool) {}

    /// Puts value in the host's registry as "<owner>.<name>" for other plugins to read, or takes it out with None. Everyone hears
    /// Event::SharedChanged. Only the owner writes its keys; the values go when it stops, and nothing is kept past a restart.
    fn share(&mut self, _owner: &str, _name: &str, _value: Option<Value>) -> Result<(), String> {
        Err("this host has no registry".into())
    }
    /// A value from the registry by its full key ("<plugin>.<name>"); None when nobody shares it (now).
    fn shared(&self, _key: &str) -> Option<Value> {
        None
    }
    /// The registry's keys that start with prefix ("" for all), sorted.
    fn shared_keys(&self, _prefix: &str) -> Vec<String> {
        Vec::new()
    }
}

/// The values plugins share through the host (Host::share), for a host to keep.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Registry {
    values: std::collections::BTreeMap<String, Value>,
}

impl Registry {
    /// Longest value, as JSON text.
    pub const VALUE_MAX: usize = 64 * 1024;
    /// Most keys one plugin may hold.
    pub const KEYS_MAX: usize = 256;

    /// Host::share: the full key when the registry changed, None when it already held that.
    pub fn set(&mut self, owner: &str, name: &str, value: Option<Value>) -> Result<Option<String>, String> {
        if owner.is_empty() {
            return Err("only a plugin can share values".into());
        }
        let ok_name = !name.is_empty() && name.len() <= 64 && name.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c));
        if !ok_name {
            return Err(format!("{name:?} can't be a shared name: up to 64 letters, digits, '_', '.' or '-'"));
        }
        let key = format!("{owner}.{name}");
        let Some(value) = value.filter(|v| !v.is_null()) else {
            return Ok(self.values.remove(&key).map(|_| key));
        };
        if serde_json::to_string(&value).map_or(usize::MAX, |t| t.len()) > Self::VALUE_MAX {
            return Err(format!("a shared value can be up to {} KB as JSON", Self::VALUE_MAX / 1024));
        }
        if self.values.get(&key) == Some(&value) {
            return Ok(None);
        }
        if !self.values.contains_key(&key) && self.keys(&format!("{owner}.")).len() >= Self::KEYS_MAX {
            return Err(format!("a plugin can share up to {} values", Self::KEYS_MAX));
        }
        self.values.insert(key.clone(), value);
        Ok(Some(key))
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.values.get(key).cloned()
    }

    pub fn keys(&self, prefix: &str) -> Vec<String> {
        self.values.keys().filter(|k| k.starts_with(prefix)).cloned().collect()
    }

    /// Takes out everything owner shared (it stopped or went away): the keys that went.
    pub fn drop_owner(&mut self, owner: &str) -> Vec<String> {
        let gone = self.keys(&format!("{owner}."));
        for key in &gone {
            self.values.remove(key);
        }
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_host_without_a_preview_can_ignore_messages() {
        struct WithoutPreview;
        impl HostPreview for WithoutPreview {
            fn add_preview_layer(&mut self, _owner: &str, _layer: PreviewLayer) {}
            fn remove_preview_layer(&mut self, _owner: &str) {}
            fn refresh_preview(&mut self) {}
            fn pick_on_preview(&mut self, _owner: &str, _kind: PickKind, _start: Option<[[f32; 2]; 4]>, _hint: &str) -> bool { false }
            fn finish_pick(&mut self, _owner: &str) -> bool { false }
            fn cancel_pick(&mut self, _owner: &str) {}
            fn set_preview_warp(&mut self, _owner: &str, _matrix: Option<[f32; 9]>) {}
        }
        let message = PreviewMessage { text: "Taking a picture".into(), detail: String::new(), step: String::new(),
            remaining_ms: Some(10_000), total_ms: Some(10_000), dim: true, max_ms: 80_000 };
        WithoutPreview.set_preview_message("photos", Some("phone"), Some(message));
        WithoutPreview.set_preview_message("photos", None, None);
    }

    #[test]
    fn the_registry_keeps_each_plugins_values_under_its_name() {
        let mut r = Registry::default();
        assert_eq!(r.set("trap", "last_seen", Some(json!({"t": 1}))), Ok(Some("trap.last_seen".into())));
        assert_eq!(r.set("trap", "last_seen", Some(json!({"t": 1}))), Ok(None), "the same value again changes nothing");
        assert_eq!(r.get("trap.last_seen"), Some(json!({"t": 1})));
        assert!(r.set("trap", "", Some(json!(1))).is_err());
        assert!(r.set("trap", "a b", Some(json!(1))).is_err());
        assert!(r.set("", "x", Some(json!(1))).is_err());
        assert!(r.set("trap", "big", Some(json!("x".repeat(Registry::VALUE_MAX)))).is_err());
        r.set("stars", "seeing", Some(json!(2.5))).unwrap();
        assert_eq!(r.keys("trap."), ["trap.last_seen"]);
        assert_eq!(r.set("trap", "last_seen", Some(Value::Null)), Ok(Some("trap.last_seen".into())), "null takes it out");
        assert_eq!(r.set("trap", "last_seen", None), Ok(None));
        r.set("trap", "a", Some(json!(1))).unwrap();
        assert_eq!(r.drop_owner("trap"), ["trap.a"]);
        assert_eq!(r.keys(""), ["stars.seeing"]);
        for i in 0..Registry::KEYS_MAX {
            r.set("many", &format!("k{i}"), Some(json!(i))).unwrap();
        }
        assert!(r.set("many", "one_more", Some(json!(1))).is_err());
        assert!(r.set("many", "k0", Some(json!(9))).is_ok(), "changing a key it has is fine");
    }
}
