//! The host a plugin's hook gets: the shell's state, seen from that one plugin. What the window has to carry out goes
//! into State::effects. Services that live in crates still being ported (vision, sensors, stills, colour, motion,
//! packets, the preview's layers) answer as "not available" until those land.

use crate::shell::{Effect, State};
use crate::{banners::COPY_SCHEME, profiles};
use serde_json::Value;
use std::sync::Arc;
use telescope_core::event::Event;
use telescope_core::frame::{Frame, StreamFrame};
use telescope_core::host::*;
use telescope_core::source::{RouteSpec, StreamSource};
use telescope_core::ui::{ActionSpec, Issue, TrayAction};

pub struct Ctx<'a> {
    pub state: &'a mut State,
    /// The plugin whose hook this is.
    pub me: String,
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

struct NoSensors;

impl SensorReader for NoSensors {
    fn reading(&self, _source_id: Option<&str>) -> Option<Value> {
        None
    }
}

impl HostSettings for Ctx<'_> {
    fn schedule_save(&mut self) {
        self.state.effects.push(Effect::SaveSoon);
    }
    fn save_now(&mut self) {
        self.state.save_now();
    }
    fn plugin_config(&self, name: &str) -> Option<Value> {
        self.state.plugin_config(name)
    }
    fn apply_preset(&mut self, name: &str, cfg: &Value) {
        if let Some(i) = self.state.index(name) {
            let cfg = cfg.clone();
            self.state.call(i, |p, host| {
                let _ = p.apply_preset(host, &cfg);
            });
        }
    }
    fn preset_plugins(&self) -> Vec<String> {
        self.state.slots.iter().filter(|s| s.info.in_presets && !s.info.name.is_empty()).map(|s| s.info.name.clone()).collect()
    }
    fn device_config(&self, source_id: Option<&str>, plugin_name: &str) -> Value {
        profiles::device_config(&self.state.file.load(), source_id, plugin_name)
    }
    fn has_device_settings(&self, name: &str) -> bool {
        profiles::has_device_settings(&self.state.file.load(), name)
    }
    fn switch_device(&mut self, prev_name: Option<&str>, new_name: Option<&str>) {
        self.state.switch_device(prev_name, new_name);
    }
    fn forget_device_settings(&mut self, name: &str) {
        self.state.forget_device_settings(name);
    }
    fn move_device_settings(&mut self, old: &str, new: &str) {
        self.state.move_device_settings(old, new);
    }
}

impl HostStreams for Ctx<'_> {
    fn is_streaming(&self) -> bool {
        self.state.streams.streaming
    }
    fn is_starting(&self) -> bool {
        self.state.streams.starting
    }
    fn is_restarting(&self) -> bool {
        self.state.streams.restarting
    }
    fn is_streaming_from(&self, source_id: Option<&str>) -> bool {
        self.state.streams.streaming && self.state.streams.source_id.as_deref() == source_id
    }
    fn stream_count(&self) -> u32 {
        self.state.streams.live.len().max(self.state.streams.streaming as usize) as u32
    }
    fn start_stream(&mut self, interactive: bool) {
        self.state.effects.push(Effect::Start { interactive });
    }
    fn start_again(&mut self, source_id: Option<&str>) {
        self.state.effects.push(Effect::StartAgain(source_id.map(String::from)));
    }
    fn stop_stream(&mut self, source_id: Option<&str>) {
        self.state.effects.push(Effect::Stop(source_id.map(String::from)));
    }
    fn stop_all_streams(&mut self) {
        self.state.effects.push(Effect::StopAll);
    }
    fn reconnect_stream(&mut self) {
        self.state.effects.push(Effect::Reconnect);
    }
    fn hand_over_stream(&mut self, _route: &RouteSpec, _source_id: Option<&str>) -> bool {
        false
    }
    fn focused_source_id(&self) -> Option<String> {
        self.state.streams.source_id.clone().or_else(|| self.state.selected.clone())
    }
    fn background_streams(&self) -> Vec<BackgroundStream> {
        Vec::new()
    }
    fn pick_source(&mut self, source_id: Option<&str>) -> bool {
        self.state.effects.push(Effect::PickSource(source_id.map(String::from)));
        true
    }
    fn main_stream(&self) -> Option<(String, String)> {
        self.state.streams.main.clone()
    }
    fn make_main(&mut self, source_id: &str) -> bool {
        let st = &self.state.streams;
        if st.stopping_all || st.restarting || !st.live.iter().any(|id| id == source_id) {
            return false;
        }
        self.state.effects.push(Effect::MakeMain(source_id.into()));
        true
    }
    fn update_stream_output(&mut self, _change: OutputChange, _source_id: Option<&str>) {}
    fn canvas_size(&self) -> (Option<u32>, Option<u32>) {
        let setup = self.state.plugin_config("setup");
        let dim = |k: &str| setup.as_ref().and_then(|c| c.get(k)).and_then(Value::as_u64).map(|v| v as u32);
        (dim("canvas_width"), dim("canvas_height"))
    }
    fn restart_vcam_canvas(&mut self, _width: u32, _height: u32) -> RequestId {
        self.state.request_id()
    }
    fn stream_output(&self, _source_id: &str) -> String {
        String::new()
    }
    fn stream_on_camera(&self, _slot: u32) -> Option<(String, Option<u32>, Option<u32>)> {
        None
    }
    fn remove_extra_cameras(&mut self) -> RequestId {
        self.state.request_id()
    }
    fn phone_control(&mut self, source_id: Option<&str>, body: &Value) {
        self.state.effects.push(Effect::PhoneControl { source_id: source_id.map(String::from), body: body.clone() });
    }
    fn phone_state(&self, _source_id: Option<&str>) -> Option<Value> {
        self.state.streams.phone_state.clone()
    }
}

impl HostCamera for Ctx<'_> {
    fn is_camera_on(&self) -> bool {
        !self.state.streams.camera_off
    }
    fn is_camera_off_auto(&self) -> bool {
        false
    }
    fn can_turn_camera_off(&self) -> (bool, String) {
        (false, telescope_core::i18n::tr("Only a phone can turn its camera off and keep streaming its mic."))
    }
    fn set_camera_on(&mut self, on: bool, auto: bool) {
        self.state.effects.push(Effect::CameraOn { on, auto });
    }
}

impl HostSources for Ctx<'_> {
    fn add_stream_source(&mut self, _source: Arc<dyn StreamSource>) {}
    fn remove_stream_source(&mut self, _source_id: &str) {}
    fn stream_source(&mut self, _source_id: &str) {}
}

impl HostShell for Ctx<'_> {
    fn show_issue(&mut self, key: &str, issue: Issue) {
        self.state.banners.show(key, issue);
    }
    fn clear_issue(&mut self, key: Option<&str>, issue: Option<&Issue>) {
        self.state.banners.clear(key, issue);
    }
    fn send_notification(&mut self, title: &str, body: &str, urgent: bool) {
        self.state.effects.push(Effect::Notify { title: title.into(), body: body.into(), urgent });
    }
    fn set_keep_in_tray(&mut self, keep: bool) {
        self.state.keep_in_tray = keep;
        self.state.effects.push(Effect::KeepInTray(keep));
    }
    fn toggle_window(&mut self) -> bool {
        self.state.effects.push(Effect::ToggleWindow);
        true
    }
    fn quit_app(&mut self) {
        self.state.effects.push(Effect::Quit);
    }
    fn restart_app(&mut self) -> bool {
        self.state.effects.push(Effect::Restart);
        true
    }
    fn diagnostics_report(&self) -> String {
        self.state.diagnostics().iter().map(|(k, v)| format!("{k}: {v}\n")).collect()
    }
    fn open_url(&mut self, url: &str) {
        self.state.effects.push(Effect::OpenUrl(url.into()));
    }
    fn open_settings(&mut self, tab: &str) {
        self.state.effects.push(Effect::OpenSettings(tab.into()));
    }
    fn shortcut_actions(&self) -> Vec<(String, ActionSpec)> {
        self.state.slots.iter().flat_map(|s| s.actions.iter().map(|a| (s.info.name.clone(), a.clone()))).collect()
    }
}

impl HostUi for Ctx<'_> {
    fn set_ui_props(&mut self, ui: &str, props: &Value) {
        self.state.effects.push(Effect::Props { plugin: self.me.clone(), ui: ui.into(), props: props.clone() });
    }
    fn update_tray_action(&mut self, action: &TrayAction) {
        self.state.effects.push(Effect::Tray(action.clone()));
    }
    fn refresh_frame_step(&mut self) {
        self.state.effects.push(Effect::FrameStep(self.me.clone()));
    }
}

impl HostView for Ctx<'_> {
    fn view(&self) -> View {
        self.state.view.clone()
    }
    fn view_aspect(&self) -> f32 {
        0.0
    }
    fn set_view(&mut self, change: ViewChange) {
        let v = &mut self.state.view;
        if let Some(z) = change.zoom { v.zoom = z; }
        if let Some(x) = change.pan_x { v.pan_x = x; }
        if let Some(y) = change.pan_y { v.pan_y = y; }
        if let Some(f) = change.flip_h { v.flip_h = f; }
        if let Some(f) = change.flip_v { v.flip_v = f; }
        if let Some(r) = change.rotation { v.rotation = r; }
        if let Some(s) = change.stretch_x { v.stretch_x = s; }
        if let Some(s) = change.stretch_y { v.stretch_y = s; }
        let view = v.clone();
        self.state.bus.emit(Event::ViewChanged { view });
    }
    fn take_view(&mut self, _owner: &str) -> bool {
        false
    }
    fn steer_view(&mut self, _owner: &str, _zoom: f32, _pan_x: f32, _pan_y: f32) {}
    fn release_view(&mut self, _owner: &str) {}
}

impl HostVision for Ctx<'_> {
    fn vision_state(&self) -> String {
        "off".into()
    }
    fn want_people(&mut self, _owner: &str, _on: bool) {}
    fn tracked_people(&self, _source_id: Option<&str>) -> (Vec<Value>, u32) {
        (Vec::new(), 0)
    }
}

impl HostFrames for Ctx<'_> {
    fn frames(&self) -> Arc<dyn FrameStore> {
        Arc::new(NoFrames)
    }
}

impl HostMotion for Ctx<'_> {
    fn watch_motion(&mut self, _owner: &str, _zones: &[[f32; 4]], _sensitivity: u32) {}
    fn motion(&self, _owner: &str) -> Vec<u32> {
        Vec::new()
    }
}

impl HostPreview for Ctx<'_> {
    fn add_preview_layer(&mut self, _owner: &str, _layer: PreviewLayer) {}
    fn remove_preview_layer(&mut self, _owner: &str) {}
    fn refresh_preview(&mut self) {}
    fn pick_on_preview(&mut self, _owner: &str, _kind: PickKind, _start: Option<[[f32; 2]; 4]>, _hint: &str) -> bool {
        false
    }
    fn finish_pick(&mut self, _owner: &str) -> bool {
        false
    }
    fn cancel_pick(&mut self, _owner: &str) {}
    fn set_preview_warp(&mut self, _owner: &str, _matrix: Option<[f32; 9]>) {}
}

impl HostStills for Ctx<'_> {
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

impl HostColour for Ctx<'_> {
    fn set_colour_stage(&mut self, _owner: &str, _stage: Option<ColourStage>, _order: i32, _source_id: Option<&str>) {}
    fn colour_split(&mut self, _owner: &str, _position: Option<f32>) {}
}

impl HostSensors for Ctx<'_> {
    fn want_sensors(&mut self, _owner: &str, _kinds: &[String]) {}
    fn sensor_status(&self, _source_id: Option<&str>) -> Value {
        Value::Null
    }
    fn sensors(&self) -> Arc<dyn SensorReader> {
        Arc::new(NoSensors)
    }
}

impl HostPackets for Ctx<'_> {
    fn packets_problem(&self) -> String {
        String::new()
    }
    fn change_packets(&mut self, _owner: &str, _factory: Option<Arc<dyn PacketStepFactory>>) {}
}

impl Host for Ctx<'_> {
    fn emit(&mut self, event: Event) {
        self.state.bus.emit(event);
    }
    fn has_plugin(&self, name: &str) -> bool {
        self.state.index(name).is_some() || name == self.me
    }
    fn schedule(&mut self, after_ms: u64, token: &str) {
        self.state.effects.push(Effect::Schedule { plugin: self.me.clone(), token: token.into(), ms: after_ms });
    }
    fn cancel_schedule(&mut self, token: &str) {
        self.state.effects.push(Effect::CancelSchedule { plugin: self.me.clone(), token: token.into() });
    }
    fn poster(&self) -> Option<Arc<dyn telescope_core::host::Poster>> {
        Some(Arc::new(crate::shell::PluginPoster { owner: self.me.clone(), posts: self.state.posts.clone() }))
    }
}

/// copy_action's Rust form, for a plugin's Issue: a button that copies text and leaves the banner up.
pub fn copy_url(text: &str) -> String {
    format!("{COPY_SCHEME}{text}")
}
