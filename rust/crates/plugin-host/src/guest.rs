//! The runtime-neutral boundary: a runtime implements Guest, sees plain JSON both ways and reaches the host only through PluginContext::call.
//!
//! Nothing here knows about Lua. A hook is a name and a JSON array of arguments, its answer is JSON, and a call back into the host is a method
//! name from HOST_API with its arguments. A WASM runtime implements the same trait (rust/docs/plugin-wasm.md).

use crate::i18n::{self, Catalog};
use crate::manifest::PERMISSIONS;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use telescope_core::frame::{FrameStep, ShaderStep};
use telescope_core::host::{ColourStage, Host, OutputChange, PickKind, PreviewLayer, ViewChange};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::source::RouteSpec;
use telescope_core::ui::{ActionSpec, CardSpec, Issue, IssueDoes, SettingsSection, SlintSource, TrayAction, UiEvent, UiSpec};
use telescope_core::Event;

/// A plugin running in a runtime. Hooks are Plugin's method names; args is a JSON array of the hook's arguments.
pub trait Guest: Send {
    /// Runs hook with args. Every call back into the host goes through calls, which knows whether this hook has a host: core's Plugin
    /// calls some hooks without one (card, get_config, frame_step...), and then queues what can wait and refuses the rest.
    fn call(&mut self, calls: &mut HostCalls<'_>, hook: &str, args: Vec<Value>) -> Result<Value, String>;
    /// Whether the plugin defined hook, so nothing crosses over for the rest.
    fn defines(&self, hook: &str) -> bool;
    /// Whether the plugin listens for this event (a name from Event::NAMES, or a custom "<plugin>.<what>"). It hears it as
    /// call(calls, "on_event", [event as JSON]).
    fn listens(&self, event: &str) -> bool;
}

/// Every hook GuestPlugin calls, with its arguments and whether the host is there (core's Plugin, plus on_event for the bus).
/// Frozen in tests/plugin_api.txt with HOST_API.
pub const HOOKS: &[(&str, &str, bool)] = &[
    ("setup", "", true),
    ("on_plugins_ready", "", true),
    ("shutdown", "", true),
    ("on_event", "event", true),
    ("card", "", false),
    ("header_widget", "", false),
    ("settings_sections", "", false),
    ("tray_actions", "", false),
    ("on_tray_action", "id", true),
    ("actions", "", false),
    ("run_action", "id, params", true),
    ("release_action", "id, params", true),
    ("on_ui", "event", true),
    ("on_issue_action", "key, id", true),
    ("on_stream_starting", "", true),
    ("on_stream_start", "stream", true),
    ("on_stream_stop", "", true),
    ("on_focus_gained", "stream", true),
    ("on_focus_lost", "", true),
    ("on_camera_off", "", true),
    ("on_camera_on", "", true),
    ("on_phone_state", "state", true),
    ("frame_step", "", false),
    ("get_config", "", false),
    ("set_config", "cfg", false),
    ("apply_preset", "cfg", true),
    ("diagnostics", "", false),
];

/// A guest's way back into the host for the length of one hook: the only door, whatever the runtime.
pub struct HostCalls<'a> {
    context: &'a mut PluginContext,
    host: Option<&'a mut dyn Host>,
}

impl<'a> HostCalls<'a> {
    pub fn new(context: &'a mut PluginContext, host: Option<&'a mut dyn Host>) -> HostCalls<'a> {
        HostCalls { context, host }
    }

    /// One host method from HOST_API, arguments by name.
    pub fn call(&mut self, method: &str, args: Value) -> Result<Value, String> {
        let host = self.host.as_mut().map(|h| &mut **h as &mut dyn Host);
        self.context.call(host, method, args)
    }

    /// The plugin's own name, folder, translations and files.
    pub fn context(&self) -> &PluginContext {
        self.context
    }

    pub fn has_host(&self) -> bool {
        self.host.is_some()
    }
}

/// A host method a guest may call: its name, its arguments in order (optional ones end in ?), the permission it needs and whether a hook
/// without a host may still call it (it's then done at the start of the next hook that has one).
pub struct HostMethod {
    pub name: &'static str,
    pub params: &'static [&'static str],
    pub permission: Option<&'static str>,
    pub deferrable: bool,
}

const fn m(name: &'static str, params: &'static [&'static str], permission: Option<&'static str>) -> HostMethod {
    HostMethod { name, params, permission, deferrable: false }
}

const fn later(name: &'static str, params: &'static [&'static str]) -> HostMethod {
    HostMethod { name, params, permission: None, deferrable: true }
}

const PHONE: Option<&str> = Some("phone");
const STREAMS: Option<&str> = Some("streams");
const APP: Option<&str> = Some("app");
const SETTINGS: Option<&str> = Some("settings");

/// Everything a guest may ask of the host. Frozen in tests/host_api.txt: add, never change or remove (ARCHITECTURE.md's rule for core).
/// Methods that take an owner (take_view, watch_motion, add_preview_layer...) use the plugin's id (a title can be set to a built-in's), so one plugin can't act as another.
pub const HOST_API: &[HostMethod] = &[
    // Settings
    later("schedule_save", &[]),
    m("save_now", &[], None),
    m("plugin_config", &["name"], SETTINGS),
    m("apply_preset", &["name", "cfg"], SETTINGS),
    m("preset_plugins", &[], None),
    m("device_config", &["source_id?"], None),
    m("has_device_settings", &["name"], SETTINGS),
    m("forget_device_settings", &["name"], SETTINGS),
    m("move_device_settings", &["old", "new"], SETTINGS),
    // Streams
    m("is_streaming", &[], None),
    m("is_starting", &[], None),
    m("is_restarting", &[], None),
    m("is_streaming_from", &["source_id?"], None),
    m("stream_count", &[], None),
    m("start_stream", &["interactive?"], STREAMS),
    m("start_again", &["source_id?"], STREAMS),
    m("stop_stream", &["source_id?"], STREAMS),
    m("stop_all_streams", &[], STREAMS),
    m("reconnect_stream", &[], STREAMS),
    m("hand_over_stream", &["route", "source_id?"], STREAMS),
    m("focused_source_id", &[], None),
    m("background_streams", &[], None),
    m("pick_source", &["source_id?"], STREAMS),
    m("main_stream", &[], None),
    m("make_main", &["source_id"], STREAMS),
    m("update_stream_output", &["change", "source_id?"], STREAMS),
    m("canvas_size", &[], None),
    m("restart_vcam_canvas", &["width", "height"], STREAMS),
    m("stream_output", &["source_id"], None),
    m("stream_on_camera", &["slot?"], None),
    m("remove_extra_cameras", &[], STREAMS),
    m("phone_control", &["body", "source_id?"], PHONE),
    m("phone_state", &["source_id?"], None),
    // Camera
    m("is_camera_on", &[], None),
    m("is_camera_off_auto", &[], None),
    m("can_turn_camera_off", &[], None),
    m("set_camera_on", &["on", "auto?"], STREAMS),
    // Shell
    m("show_issue", &["key", "issue"], None),
    m("clear_issue", &["key?"], None),
    m("send_notification", &["title", "body", "urgent?"], Some("notifications")),
    m("set_keep_in_tray", &["keep"], APP),
    m("toggle_window", &[], APP),
    m("quit_app", &[], APP),
    m("restart_app", &[], APP),
    m("diagnostics_report", &[], None),
    m("open_settings", &["tab?"], None),
    m("shortcut_actions", &[], None),
    // UI
    later("set_ui_props", &["ui", "props"]),
    later("update_tray_action", &["action"]),
    later("refresh_frame_step", &[]),
    // View
    m("view", &[], None),
    m("view_aspect", &[], None),
    m("set_view", &["change"], None),
    m("take_view", &[], None),
    m("steer_view", &["zoom", "pan_x", "pan_y"], None),
    m("release_view", &[], None),
    // Vision
    m("vision_state", &[], None),
    m("want_people", &["on"], None),
    m("tracked_people", &["source_id?"], None),
    // Motion
    m("watch_motion", &["zones", "sensitivity?"], None),
    m("motion", &[], None),
    // Preview
    m("add_preview_layer", &["layer"], None),
    m("remove_preview_layer", &[], None),
    m("refresh_preview", &[], None),
    m("pick_on_preview", &["kind", "start?", "hint?"], None),
    m("finish_pick", &[], None),
    m("cancel_pick", &[], None),
    m("set_preview_warp", &["matrix?"], None),
    // Stills
    m("still_caps", &["source_id?"], None),
    m("take_still", &["options", "folder", "source_id?"], PHONE),
    m("still_busy", &["source_id?"], None),
    // Colour
    m("set_colour_stage", &["stage?", "order?", "source_id?"], None),
    m("colour_split", &["position?"], None),
    // Sensors
    m("want_sensors", &["kinds"], None),
    m("sensor_status", &["source_id?"], None),
    m("sensor_reading", &["source_id?"], None),
    // Packets (reading why they can't change; changing them needs Rust)
    m("packets_problem", &[], None),
    // The bus and other plugins
    later("emit", &["name", "data?"]),
    // Timers (core's schedule service): Event::TimerFired with the token reaches this plugin only
    later("schedule", &["after_ms", "token"]),
    later("cancel_schedule", &["token"]),
    m("has_plugin", &["name"], None),
];

/// The method called name, if guests may call it.
pub fn host_method(name: &str) -> Option<&'static HostMethod> {
    HOST_API.iter().find(|m| m.name == name)
}

/// Positional arguments as the named object call_host takes, by HOST_API's order. Extra ones are an error, missing optional ones are null.
pub fn named_args(method: &HostMethod, positional: Vec<Value>) -> Result<Value, String> {
    if positional.len() > method.params.len() {
        return Err(format!("host.{} takes {} argument(s), not {}", method.name, method.params.len(), positional.len()));
    }
    let mut out = serde_json::Map::new();
    let mut given = positional.into_iter();
    for p in method.params {
        let (name, optional) = p.strip_suffix('?').map_or((*p, false), |n| (n, true));
        let value = given.next().unwrap_or(Value::Null);
        if value.is_null() && !optional {
            return Err(format!("host.{} needs {name}", method.name));
        }
        out.insert(name.to_string(), value);
    }
    Ok(Value::Object(out))
}

/// What a guest's calls are made as: which plugin, its folder, permissions and translations. Shared by every runtime.
pub struct PluginContext {
    pub info: PluginInfo,
    pub folder: PathBuf,
    pub permissions: Vec<String>,
    catalog: Option<Catalog>,
    /// Calls a hook without a host made, done at the start of the next hook that has one.
    deferred: Vec<(String, Value)>,
}

impl PluginContext {
    pub fn new(info: PluginInfo, folder: impl Into<PathBuf>, permissions: Vec<String>) -> PluginContext {
        PluginContext { info, folder: folder.into(), permissions, catalog: None, deferred: Vec::new() }
    }

    /// Loads the plugin's locale/<lang>.po for lang ("en" or "" for English).
    pub fn set_language(&mut self, lang: &str) {
        self.catalog = Catalog::for_plugin(&self.info.name, &self.folder, lang);
    }

    pub fn tr(&self, msgid: &str, ctx: &str, fields: &[(&str, &dyn Display)]) -> String {
        i18n::tr(self.catalog.as_ref(), msgid, ctx, fields)
    }

    pub fn trn(&self, singular: &str, plural: &str, n: i64, ctx: &str, fields: &[(&str, &dyn Display)]) -> String {
        i18n::trn(self.catalog.as_ref(), singular, plural, n, ctx, fields)
    }

    /// A file of the plugin's own, by its path inside the plugin (a shader, a .slint file, data). Nothing outside the folder.
    pub fn resolve(&self, relative: &str) -> Result<PathBuf, String> {
        if !crate::package::safe_relative(relative) {
            return Err(format!("{relative:?} isn't a path inside the plugin"));
        }
        let path = self.folder.join(relative);
        let real = path.canonicalize().map_err(|_| format!("the plugin has no {relative}"))?;
        let root = self.folder.canonicalize().map_err(|e| e.to_string())?;
        if !real.starts_with(&root) {
            return Err(format!("{relative:?} leads outside the plugin"));
        }
        // Slint can't import from the \\?\ form canonicalize gives on Windows
        Ok(telescope_core::system::plain_path(real))
    }

    pub fn read_text(&self, relative: &str) -> Result<String, String> {
        std::fs::read_to_string(self.resolve(relative)?).map_err(|e| format!("couldn't read {relative}: {e}"))
    }

    /// The one door from a guest into the host: method from HOST_API with its arguments by name.
    pub fn call(&mut self, host: Option<&mut dyn Host>, method: &str, args: Value) -> Result<Value, String> {
        let spec = host_method(method).ok_or_else(|| format!("host has no method {method}"))?;
        if let Some(p) = spec.permission {
            if !self.permissions.iter().any(|have| have == p) {
                return Err(format!("host.{method} needs \"{p}\" in the \"permissions\" of plugin.json"));
            }
        }
        if method == "emit" {
            self.check_emit(&args)?;
        }
        if method == "show_issue" {
            self.check_issue(&args)?;
        }
        match host {
            Some(host) => call_host(host, &self.info, method, &args),
            None if spec.deferrable => {
                self.deferred.push((method.to_string(), args));
                Ok(Value::Null)
            }
            None => Err(format!("host.{method} can't be called here: this hook runs without the host")),
        }
    }

    /// Holds a banner's buttons to what they'd do when clicked: a button that starts a stream or turns the camera on needs "streams"
    /// like the calls do, and a link can only be a web page or text to copy (the system would open any other as a file or a program).
    fn check_issue(&self, args: &Value) -> Result<(), String> {
        let Some(issue) = args.get("issue").and_then(|i| serde_json::from_value::<Issue>(i.clone()).ok()) else { return Ok(()) };
        for action in &issue.actions {
            match &action.does {
                IssueDoes::StartAgain { .. } | IssueDoes::CameraOnAgain { .. } if !self.permissions.iter().any(|have| have == "streams") => {
                    return Err("host.show_issue has a button that starts a stream or turns the camera on, which needs \"streams\" in the \"permissions\" of plugin.json".into());
                }
                IssueDoes::OpenUrl { url } if !["https://", "http://", "copy:"].iter().any(|scheme| url.starts_with(scheme)) => {
                    return Err("host.show_issue can only have a button that opens an http or https link".into());
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Holds an emitted built-in event to the permission of the host operation it triggers; events that name an owner are never a plugin's to send.
    fn check_emit(&self, args: &Value) -> Result<(), String> {
        let name = args.get("name").and_then(Value::as_str).unwrap_or("");
        if !Event::NAMES.contains(&name) || name == "custom" {
            return Ok(());
        }
        let needs = match name {
            "focus_point" | "meter_point" | "resolution_change_requested" => PHONE,
            "select_source" => STREAMS,
            "update_requested" | "add_phone_requested" | "setup_guide_requested" => APP,
            "forget_source_requested" => SETTINGS,
            "cover_requested" | "timer_fired" | "posted" | "preview_picked" | "view_taken" => {
                return Err(format!("host.emit can't send {name}: it speaks for one plugin, and that isn't an added plugin"));
            }
            // The rest is the app telling plugins what happened (stream_stopped, phone_state_updated...), which a plugin can't fake
            _ => return Err(format!("host.emit can't send {name}: the app sends it itself. A plugin's own events are called \"{}.<what>\"", self.info.name)),
        };
        match needs {
            Some(p) if !self.permissions.iter().any(|have| have == p) => {
                Err(format!("host.emit(\"{name}\") needs \"{p}\" in the \"permissions\" of plugin.json"))
            }
            _ => Ok(()),
        }
    }

    /// Does what hooks without a host queued, in order.
    pub fn flush(&mut self, host: &mut dyn Host) {
        for (method, args) in std::mem::take(&mut self.deferred) {
            if let Err(e) = call_host(host, &self.info, &method, &args) {
                log::error!("Plugin {}: host.{method} failed: {e}", self.info.name);
            }
        }
    }
}

fn arg<T: DeserializeOwned>(args: &Value, name: &str) -> Result<T, String> {
    serde_json::from_value(args.get(name).cloned().unwrap_or(Value::Null)).map_err(|e| format!("{name}: {e}"))
}

fn out<T: serde::Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

/// The event a guest emits: a built-in one by its signal name and fields, or "<plugin>.<what>" of its own as Event::Custom.
fn guest_event(info: &PluginInfo, name: &str, data: &Value) -> Result<Event, String> {
    if Event::NAMES.contains(&name) && name != "custom" {
        let mut fields = data.as_object().cloned().unwrap_or_default();
        fields.insert("event".into(), name.into());
        return serde_json::from_value(Value::Object(fields)).map_err(|e| format!("{name}: {e}"));
    }
    if name.strip_prefix(&info.name).is_some_and(|rest| rest.len() > 1 && rest.starts_with('.')) {
        return Ok(Event::Custom { name: name.to_string(), data: data.clone() });
    }
    Err(format!("{name:?} isn't an event; a plugin's own events are called \"{}.<what>\"", info.name))
}

/// Does one host method for a plugin; args by name as HOST_API lists them. Checks no permissions (PluginContext::call does).
pub fn call_host(host: &mut dyn Host, info: &PluginInfo, method: &str, args: &Value) -> Result<Value, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
    let src = s("source_id");
    let src = src.as_deref();
    let owner = info.name.as_str();
    let done = Ok(Value::Null);
    match method {
        "schedule_save" => host.schedule_save(),
        "save_now" => host.save_now(),
        "plugin_config" => return out(host.plugin_config(&arg::<String>(args, "name")?)),
        "apply_preset" => host.apply_preset(&arg::<String>(args, "name")?, args.get("cfg").unwrap_or(&Value::Null)),
        "preset_plugins" => return out(host.preset_plugins()),
        "device_config" => return Ok(host.device_config(src, &info.name)),
        "has_device_settings" => return out(host.has_device_settings(&arg::<String>(args, "name")?)),
        "forget_device_settings" => host.forget_device_settings(&arg::<String>(args, "name")?),
        "move_device_settings" => host.move_device_settings(&arg::<String>(args, "old")?, &arg::<String>(args, "new")?),
        "is_streaming" => return out(host.is_streaming()),
        "is_starting" => return out(host.is_starting()),
        "is_restarting" => return out(host.is_restarting()),
        "is_streaming_from" => return out(host.is_streaming_from(src)),
        "stream_count" => return out(host.stream_count()),
        "start_stream" => host.start_stream(arg::<Option<bool>>(args, "interactive")?.unwrap_or(true)),
        "start_again" => host.start_again(src),
        "stop_stream" => host.stop_stream(src),
        "stop_all_streams" => host.stop_all_streams(),
        "reconnect_stream" => host.reconnect_stream(),
        "hand_over_stream" => return out(host.hand_over_stream(&arg::<RouteSpec>(args, "route")?, src)),
        "focused_source_id" => return out(host.focused_source_id()),
        "background_streams" => return out(host.background_streams()),
        "pick_source" => return out(host.pick_source(src)),
        "main_stream" => return out(host.main_stream()),
        "make_main" => return out(host.make_main(&arg::<String>(args, "source_id")?)),
        "update_stream_output" => host.update_stream_output(arg::<OutputChange>(args, "change")?, src),
        "canvas_size" => return out(host.canvas_size()),
        "restart_vcam_canvas" => return out(host.restart_vcam_canvas(arg(args, "width")?, arg(args, "height")?, false)),
        "stream_output" => return out(host.stream_output(&arg::<String>(args, "source_id")?)),
        "stream_on_camera" => return out(host.stream_on_camera(arg::<Option<u32>>(args, "slot")?.unwrap_or(0))),
        "remove_extra_cameras" => return out(host.remove_extra_cameras()),
        "phone_control" => host.phone_control(src, args.get("body").unwrap_or(&Value::Null)),
        "phone_state" => return out(host.phone_state(src)),
        "is_camera_on" => return out(host.is_camera_on()),
        "is_camera_off_auto" => return out(host.is_camera_off_auto()),
        "can_turn_camera_off" => return out(host.can_turn_camera_off()),
        "set_camera_on" => host.set_camera_on(arg(args, "on")?, arg::<Option<bool>>(args, "auto")?.unwrap_or(false)),
        "show_issue" => host.show_issue(&arg::<String>(args, "key")?, arg::<Issue>(args, "issue")?),
        "clear_issue" => host.clear_issue(s("key").as_deref(), None),
        "send_notification" => host.send_notification(&arg::<String>(args, "title")?, &arg::<String>(args, "body")?,
                                                      arg::<Option<bool>>(args, "urgent")?.unwrap_or(true)),
        "set_keep_in_tray" => host.set_keep_in_tray(arg(args, "keep")?),
        "toggle_window" => return out(host.toggle_window()),
        "quit_app" => host.quit_app(),
        "restart_app" => return out(host.restart_app()),
        "diagnostics_report" => return out(host.diagnostics_report(&[])),
        "open_settings" => host.open_settings(s("tab").as_deref().unwrap_or("")),
        "shortcut_actions" => return out(host.shortcut_actions()),
        "set_ui_props" => host.set_ui_props(&arg::<String>(args, "ui")?, args.get("props").unwrap_or(&Value::Null)),
        "update_tray_action" => host.update_tray_action(&arg::<TrayAction>(args, "action")?),
        "refresh_frame_step" => host.refresh_frame_step(),
        "view" => return out(host.view()),
        "view_aspect" => return out(host.view_aspect()),
        "set_view" => host.set_view(arg::<ViewChange>(args, "change")?),
        "take_view" => return out(host.take_view(owner)),
        "steer_view" => host.steer_view(owner, arg(args, "zoom")?, arg(args, "pan_x")?, arg(args, "pan_y")?),
        "release_view" => host.release_view(owner),
        "vision_state" => return out(host.vision_state()),
        "want_people" => host.want_people(owner, arg(args, "on")?),
        "tracked_people" => {
            let (tracks, count) = host.tracked_people(src);
            return Ok(json!({"tracks": tracks, "count": count}));
        }
        "watch_motion" => host.watch_motion(owner, &arg::<Vec<[f32; 4]>>(args, "zones")?, arg::<Option<u32>>(args, "sensitivity")?.unwrap_or(5)),
        "motion" => return out(host.motion(owner)),
        "add_preview_layer" => host.add_preview_layer(owner, arg::<PreviewLayer>(args, "layer")?),
        "remove_preview_layer" => host.remove_preview_layer(owner),
        "refresh_preview" => host.refresh_preview(),
        "pick_on_preview" => return out(host.pick_on_preview(owner, arg::<PickKind>(args, "kind")?, arg(args, "start")?,
                                                             s("hint").as_deref().unwrap_or(""))),
        "finish_pick" => return out(host.finish_pick(owner)),
        "cancel_pick" => host.cancel_pick(owner),
        "set_preview_warp" => host.set_preview_warp(owner, arg(args, "matrix")?),
        "still_caps" => return out(host.still_caps(src)),
        "take_still" => return out(host.take_still(args.get("options").unwrap_or(&Value::Null), &arg::<String>(args, "folder")?, src)),
        "still_busy" => return out(host.still_busy(src)),
        "set_colour_stage" => host.set_colour_stage(owner, arg::<Option<ColourStage>>(args, "stage")?,
                                                    arg::<Option<i32>>(args, "order")?.unwrap_or(0), src),
        "colour_split" => host.colour_split(owner, arg(args, "position")?),
        "want_sensors" => host.want_sensors(owner, &arg::<Vec<String>>(args, "kinds")?),
        "sensor_status" => return Ok(host.sensor_status(src)),
        "sensor_reading" => return out(host.sensors().reading(src)),
        "packets_problem" => return out(host.packets_problem()),
        "emit" => host.emit(guest_event(info, &arg::<String>(args, "name")?, args.get("data").unwrap_or(&Value::Null))?),
        "has_plugin" => return out(host.has_plugin(&arg::<String>(args, "name")?)),
        "schedule" => host.schedule(arg(args, "after_ms")?, &arg::<String>(args, "token")?),
        "cancel_schedule" => host.cancel_schedule(&arg::<String>(args, "token")?),
        other => return Err(format!("host has no method {other}")),
    }
    done
}

/// Any guest as a core Plugin. The guest sits in a RefCell so &self hooks (get_config, diagnostics) can still call into it.
pub struct GuestPlugin<G: Guest> {
    guest: RefCell<G>,
    context: RefCell<PluginContext>,
    info: PluginInfo,
    /// The hook that failed last, so the same failure isn't logged every frame.
    failing: RefCell<Option<String>>,
}

impl<G: Guest> GuestPlugin<G> {
    pub fn new(context: PluginContext, guest: G) -> Self {
        GuestPlugin { info: context.info.clone(), guest: RefCell::new(guest), context: RefCell::new(context), failing: RefCell::new(None) }
    }

    pub(crate) fn guest_ref(&self) -> std::cell::Ref<'_, G> {
        self.guest.borrow()
    }

    pub fn context(&self) -> std::cell::Ref<'_, PluginContext> {
        self.context.borrow()
    }

    /// Runs hook whether or not the guest defines it, after what earlier hooks without a host queued.
    pub fn call(&self, host: Option<&mut dyn Host>, hook: &str, args: Vec<Value>) -> Result<Value, String> {
        let mut context = self.context.borrow_mut();
        let host: Option<&mut dyn Host> = match host {
            Some(h) => {
                context.flush(&mut *h);
                Some(&mut *h)
            }
            None => None,
        };
        let mut calls = HostCalls::new(&mut context, host);
        self.guest.borrow_mut().call(&mut calls, hook, args)
    }

    /// Calls hook if the guest defines it; a failure is logged and treated as no answer, as Telescope treats a plugin's exceptions.
    pub fn hook(&self, host: Option<&mut dyn Host>, hook: &str, args: Vec<Value>) -> Option<Value> {
        let mut host = host;
        if let Some(h) = host.as_mut() {
            self.context.borrow_mut().flush(&mut **h);
        }
        // on_event's caller already asked listens()
        if hook != "on_event" && !self.guest.borrow().defines(hook) {
            return None;
        }
        match self.call(host, hook, args) {
            Ok(v) => {
                self.failing.borrow_mut().take();
                Some(v)
            }
            Err(e) => {
                let mut failing = self.failing.borrow_mut();
                if failing.as_deref() != Some(hook) {
                    log::error!("Plugin {} failed in {hook}: {e}", self.info.name);
                    *failing = Some(hook.to_string());
                }
                None
            }
        }
    }

    fn with_host(&mut self, host: &mut dyn Host, hook: &str, args: Vec<Value>) -> Option<Value> {
        self.hook(Some(host), hook, args)
    }

    fn parse<T: DeserializeOwned>(&self, hook: &str, v: Value) -> Option<T> {
        if v.is_null() {
            return None;
        }
        serde_json::from_value(v).map_err(|e| log::error!("Plugin {}: {hook} returned something unusable: {e}", self.info.name)).ok()
    }

    /// A UI piece as a guest returns it: {file, component, props}, the file inside the plugin's folder.
    pub fn ui_spec(&self, v: &Value) -> Result<UiSpec, String> {
        let file = v.get("file").and_then(Value::as_str).ok_or("it needs a \"file\" (the .slint file)")?;
        let component = v.get("component").and_then(Value::as_str).ok_or("it needs a \"component\" (the exported component)")?;
        let path = self.context.borrow().resolve(file)?;
        Ok(UiSpec { source: SlintSource::File(path.to_string_lossy().into_owned()), component: component.to_string(),
                    props: v.get("props").cloned().unwrap_or_else(|| json!({})) })
    }

    fn ui_or_log(&self, hook: &str, v: &Value) -> Option<UiSpec> {
        self.ui_spec(v).map_err(|e| log::error!("Plugin {}: {hook}: {e}", self.info.name)).ok()
    }

    /// A frame step as a guest returns it: {shader = "file.wgsl"} or {wgsl = "..."}, with uniforms. Checked against core's shader contract.
    pub fn shader_step(&self, v: &Value) -> Result<ShaderStep, String> {
        let wgsl = match (v.get("shader").and_then(Value::as_str), v.get("wgsl").and_then(Value::as_str)) {
            (Some(file), _) => self.context.borrow().read_text(file)?,
            (None, Some(code)) => code.to_string(),
            _ => return Err("it needs a \"shader\" (a .wgsl file in the plugin)".into()),
        };
        let uniforms = v.get("uniforms").cloned().unwrap_or_else(|| json!({}));
        let step = ShaderStep { wgsl, uniforms, textures: Vec::new() };
        telescope_core::shader::assemble(&step)?;
        telescope_core::shader::pack_uniforms(&step.wgsl, &step.uniforms)?;
        Ok(step)
    }
}

/// A stream as a guest sees it.
fn stream_json(stream: &StreamInfo) -> Value {
    serde_json::to_value(stream).unwrap_or_default()
}

impl<G: Guest> Plugin for GuestPlugin<G> {
    fn info(&self) -> PluginInfo {
        self.info.clone()
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.with_host(host, "setup", vec![]);
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.with_host(host, "on_plugins_ready", vec![]);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.with_host(host, "shutdown", vec![]);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        let name = match event {
            Event::Custom { name, .. } => name.as_str(),
            // Another plugin's timer or post, should a host hand it round
            Event::TimerFired { owner, .. } | Event::Posted { owner, .. } if *owner != self.info.name => return,
            other => other.name(),
        };
        if self.guest.borrow().listens(name) {
            let args = serde_json::to_value(event).unwrap_or_default();
            self.with_host(host, "on_event", vec![args]);
        } else {
            // Calls a hook without a host queued go out with the next event, whether or not the plugin hears it
            self.context.borrow_mut().flush(host);
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        let v = self.hook(None, "card", vec![])?;
        let ui = self.ui_or_log("card", &v)?;
        let region = v.get("region").and_then(Value::as_str).unwrap_or("left").to_string();
        Some(CardSpec { ui, region })
    }

    fn header_widget(&mut self) -> Option<UiSpec> {
        let v = self.hook(None, "header_widget", vec![])?;
        self.ui_or_log("header_widget", &v)
    }

    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        let Some(Value::Array(items)) = self.hook(None, "settings_sections", vec![]) else { return Vec::new() };
        items.iter().filter_map(|v| {
            let ui = self.ui_or_log("settings_sections", v)?;
            let footer = v.get("footer").and_then(|f| self.ui_or_log("settings_sections", f));
            let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            Some(SettingsSection { id: text("id"), tab: text("tab"), ui, order: v.get("order").and_then(Value::as_i64).unwrap_or(0) as i32,
                                   tab_title: text("tab_title"), keywords: text("keywords"), footer })
        }).collect()
    }

    fn tray_actions(&mut self) -> Vec<TrayAction> {
        self.hook(None, "tray_actions", vec![]).and_then(|v| self.parse("tray_actions", v)).unwrap_or_default()
    }

    fn on_tray_action(&mut self, host: &mut dyn Host, id: &str) {
        self.with_host(host, "on_tray_action", vec![json!(id)]);
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        self.hook(None, "actions", vec![]).and_then(|v| self.parse("actions", v)).unwrap_or_default()
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.with_host(host, "run_action", vec![json!(id), params.clone()])?.as_str().map(str::to_string)
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        self.with_host(host, "release_action", vec![json!(id), params.clone()]);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        // Slint treats - and _ in names alike; a guest always sees _, which every runtime's names allow
        let event = UiEvent { callback: event.callback.replace('-', "_"), ..event.clone() };
        self.with_host(host, "on_ui", vec![serde_json::to_value(event).unwrap_or_default()]);
    }

    fn on_issue_action(&mut self, host: &mut dyn Host, key: &str, id: &str) {
        self.with_host(host, "on_issue_action", vec![json!(key), json!(id)]);
    }

    fn on_stream_starting(&mut self, host: &mut dyn Host) {
        self.with_host(host, "on_stream_starting", vec![]);
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.with_host(host, "on_stream_start", vec![stream_json(stream)]);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.with_host(host, "on_stream_stop", vec![]);
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        let hook = if self.guest.borrow().defines("on_focus_gained") { "on_focus_gained" } else { "on_stream_start" };
        self.with_host(host, hook, vec![stream_json(stream)]);
    }

    fn on_focus_lost(&mut self, host: &mut dyn Host) {
        let hook = if self.guest.borrow().defines("on_focus_lost") { "on_focus_lost" } else { "on_stream_stop" };
        self.with_host(host, hook, vec![]);
    }

    fn on_camera_off(&mut self, host: &mut dyn Host) {
        self.with_host(host, "on_camera_off", vec![]);
    }

    fn on_camera_on(&mut self, host: &mut dyn Host) {
        self.with_host(host, "on_camera_on", vec![]);
    }

    fn on_phone_state(&mut self, host: &mut dyn Host, state: &Value) {
        self.with_host(host, "on_phone_state", vec![state.clone()]);
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        let v = self.hook(None, "frame_step", vec![])?;
        if v.is_null() {
            return None;
        }
        match self.shader_step(&v) {
            Ok(step) => Some(FrameStep::Shader(step)),
            Err(e) => {
                log::error!("Plugin {}: frame_step: {e}", self.info.name);
                None
            }
        }
    }

    fn get_config(&self) -> Value {
        if !self.guest.borrow().defines("get_config") {
            return json!({});
        }
        // A hook that threw, timed out or gave no table is not an empty section: null makes the host keep what it saved
        match self.hook(None, "get_config", vec![]) {
            Some(v @ Value::Object(_)) => v,
            _ => Value::Null,
        }
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        if !self.guest.borrow().defines("set_config") {
            return Ok(());
        }
        self.call(None, "set_config", vec![cfg.clone()]).map(|_| ()).map_err(ConfigRejected)
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        if !self.guest.borrow().defines("apply_preset") {
            return self.set_config(cfg);
        }
        self.call(Some(host), "apply_preset", vec![cfg.clone()]).map(|_| ()).map_err(ConfigRejected)
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let Some(Value::Array(rows)) = self.hook(None, "diagnostics", vec![]) else { return Vec::new() };
        rows.iter().filter_map(|r| {
            let pair = r.as_array()?;
            let text = |v: &Value| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
            Some((text(pair.first()?), text(pair.get(1)?)))
        }).collect()
    }
}

/// Where a plugin's files are, for runtimes that load more than one script.
pub fn plugin_file(folder: &Path, relative: &str) -> Option<PathBuf> {
    crate::package::safe_relative(relative).then(|| folder.join(relative))
}

/// Every permission name, for the Plugins tab and the docs.
pub fn permission_names() -> impl Iterator<Item = &'static str> {
    PERMISSIONS.iter().map(|(name, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::testing::FakeHost;

    /// A guest that records what it was asked and calls phone_control from every hook.
    struct Echo {
        heard: Vec<String>,
    }

    impl Guest for Echo {
        fn call(&mut self, calls: &mut HostCalls<'_>, hook: &str, args: Vec<Value>) -> Result<Value, String> {
            self.heard.push(format!("{hook} {}", Value::Array(args)));
            calls.call("phone_control", json!({"body": {"action": "torch", "value": 1}}))?;
            Ok(Value::Null)
        }

        fn defines(&self, hook: &str) -> bool {
            hook == "on_event"
        }

        fn listens(&self, event: &str) -> bool {
            event == "stream_started" || event == "echo.ping"
        }
    }

    fn info() -> PluginInfo {
        PluginInfo { name: "echo".into(), title: "Echo".into(), ..Default::default() }
    }

    fn echo(permissions: &[&str]) -> GuestPlugin<Echo> {
        let perms: Vec<String> = permissions.iter().map(|p| p.to_string()).collect();
        GuestPlugin::new(PluginContext::new(info(), ".", perms), Echo { heard: Vec::new() })
    }

    #[test]
    fn a_guest_only_hears_what_it_listens_for() {
        let mut p = echo(&["phone"]);
        let mut host = FakeHost::new();
        p.on_event(&mut host, &Event::StreamStopped);
        assert!(host.calls.is_empty());
        p.on_event(&mut host, &Event::StreamStarted { url: "u".into() });
        p.on_event(&mut host, &Event::Custom { name: "echo.ping".into(), data: json!(1) });
        assert_eq!(host.calls.len(), 2);
        assert_eq!(host.calls[0], "phone_control None {\"action\":\"torch\",\"value\":1}");
        assert_eq!(p.guest.borrow().heard[1], r#"on_event [{"event":"custom","name":"echo.ping","data":1}]"#);
    }

    #[test]
    fn a_method_needing_a_permission_is_refused_without_it() {
        let mut p = echo(&[]);
        let mut host = FakeHost::new();
        p.on_event(&mut host, &Event::StreamStarted { url: "u".into() });
        assert!(host.calls.is_empty());
        let mut ctx = PluginContext::new(info(), ".", vec![]);
        let err = ctx.call(Some(&mut host), "quit_app", json!({})).unwrap_err();
        assert!(err.contains("\"app\""), "{err}");
    }

    #[test]
    fn calls_without_a_host_wait_for_one_or_are_refused() {
        let mut ctx = PluginContext::new(info(), ".", vec![]);
        ctx.call(None, "set_ui_props", json!({"ui": "card", "props": {"on": true}})).unwrap();
        assert!(ctx.call(None, "is_streaming", json!({})).unwrap_err().contains("without the host"));
        let mut host = FakeHost::new();
        ctx.flush(&mut host);
        assert_eq!(host.calls, ["set_ui_props card {\"on\":true}"]);
    }

    #[test]
    fn positional_arguments_become_named_ones() {
        let send = host_method("send_notification").unwrap();
        assert_eq!(named_args(send, vec![json!("Hi"), json!("There")]).unwrap(), json!({"title": "Hi", "body": "There", "urgent": null}));
        assert!(named_args(send, vec![json!("Hi")]).unwrap_err().contains("needs body"));
        assert!(named_args(send, vec![json!(1), json!(2), json!(3), json!(4)]).is_err());
    }

    #[test]
    fn a_guest_emits_built_in_events_and_its_own() {
        let mut host = FakeHost::new();
        let info = info();
        call_host(&mut host, &info, "emit", &json!({"name": "cover_requested", "data": {"owner": "Echo", "on": true}})).unwrap();
        call_host(&mut host, &info, "emit", &json!({"name": "echo.ping", "data": [1]})).unwrap();
        assert!(call_host(&mut host, &info, "emit", &json!({"name": "other.ping"})).is_err());
        assert_eq!(host.events[0], Event::CoverRequested { owner: "Echo".into(), on: true });
        assert_eq!(host.events[1], Event::Custom { name: "echo.ping".into(), data: json!([1]) });
    }

    #[test]
    fn emitting_a_built_in_event_needs_the_permission_of_what_it_triggers() {
        let mut host = FakeHost::new();
        let mut none = PluginContext::new(info(), ".", vec![]);
        let err = none.call(Some(&mut host), "emit", json!({"name": "focus_point", "data": {"x": 0.2, "y": 0.2}})).unwrap_err();
        assert!(err.contains("\"phone\""), "{err}");
        assert!(none.call(Some(&mut host), "emit", json!({"name": "timer_fired", "data": {"owner": "x", "token": "t"}})).is_err());
        assert!(host.events.is_empty());
        none.call(Some(&mut host), "emit", json!({"name": "echo.ping"})).unwrap();
        let mut phone = PluginContext::new(info(), ".", vec!["phone".into()]);
        phone.call(Some(&mut host), "emit", json!({"name": "focus_point", "data": {"x": 0.2, "y": 0.2}})).unwrap();
        assert_eq!(host.events.len(), 2);
    }

    fn banner(does: Value) -> Value {
        json!({"key": "k", "issue": {"title": "T", "actions": [{"label": "Go", "does": does}]}})
    }

    #[test]
    fn a_banner_button_needs_the_permission_of_what_it_does() {
        let mut host = FakeHost::new();
        let mut none = PluginContext::new(info(), ".", vec![]);
        for does in [json!({"kind": "start_again"}), json!({"kind": "start_again", "source_id": "a", "before": "x"}), json!({"kind": "camera_on_again"})] {
            let err = none.call(Some(&mut host), "show_issue", banner(does)).unwrap_err();
            assert!(err.contains("\"streams\""), "{err}");
        }
        assert!(host.issues.is_empty());
        // A button among others is held to it too
        let two = json!({"key": "k", "issue": {"title": "T", "actions": [
            {"label": "A", "does": {"kind": "plugin", "id": "a"}}, {"label": "B", "does": {"kind": "camera_on_again"}}]}});
        assert!(none.call(Some(&mut host), "show_issue", two).is_err());
        let mut streams = PluginContext::new(info(), ".", vec!["streams".into()]);
        streams.call(Some(&mut host), "show_issue", banner(json!({"kind": "start_again"}))).unwrap();
        streams.call(Some(&mut host), "show_issue", banner(json!({"kind": "camera_on_again"}))).unwrap();
        assert_eq!(host.issues.len(), 1);
    }

    #[test]
    fn a_banner_button_that_changes_nothing_needs_no_permission() {
        let mut host = FakeHost::new();
        let mut none = PluginContext::new(info(), ".", vec![]);
        for does in [json!({"kind": "plugin", "id": "a"}), json!({"kind": "open_settings", "tab": "video"}),
                     json!({"kind": "open_url", "url": "https://example.com/help"}), json!({"kind": "open_url", "url": "copy:ls"})] {
            none.call(Some(&mut host), "show_issue", banner(does)).unwrap();
        }
    }

    #[test]
    fn a_banner_link_is_a_web_page_or_text_to_copy() {
        let mut host = FakeHost::new();
        let everything = ["phone", "streams", "app", "settings", "notifications"].map(String::from).to_vec();
        let mut ctx = PluginContext::new(info(), ".", everything);
        for url in ["file:///etc/passwd", "/usr/bin/xcalc", r"C:\\Windows\\notepad.exe", "smb://host/share", "javascript:alert(1)"] {
            let err = ctx.call(Some(&mut host), "show_issue", banner(json!({"kind": "open_url", "url": url}))).unwrap_err();
            assert!(err.contains("http"), "{url}: {err}");
        }
        assert!(host.issues.is_empty());
    }

    #[test]
    fn a_plugin_cant_send_the_apps_own_events_whatever_it_is_allowed() {
        let mut host = FakeHost::new();
        let everything = ["phone", "streams", "app", "settings", "notifications"].map(String::from).to_vec();
        let mut ctx = PluginContext::new(info(), ".", everything);
        for name in ["stream_stopped", "stream_started", "stream_lost", "phone_state_updated", "people_tracked"] {
            let err = ctx.call(Some(&mut host), "emit", json!({"name": name, "data": {}})).unwrap_err();
            assert!(err.contains("the app sends it itself"), "{name}: {err}");
        }
        assert!(host.events.is_empty());
    }

    #[test]
    fn view_ownership_follows_the_id_not_the_title() {
        let mut host = FakeHost::new();
        let sly = PluginInfo { name: "sly".into(), title: "Auto framing".into(), ..Default::default() };
        call_host(&mut host, &sly, "take_view", &json!({})).unwrap();
        assert_eq!(host.calls, ["take_view sly"]);
    }

    #[test]
    fn every_host_method_in_the_table_is_handled() {
        // Each method with arguments that are all missing either does its thing or says which argument it needs, never "no method"
        let mut host = FakeHost::new();
        let info = info();
        for method in HOST_API {
            if let Err(e) = call_host(&mut host, &info, method.name, &json!({})) {
                assert!(!e.contains("no method"), "{}: {e}", method.name);
            }
        }
    }
}
