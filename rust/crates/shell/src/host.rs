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
        // This plugin is out of its slot while its hook runs, so the save waits until it's back
        self.state.save_after_call = true;
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
    /// PhoneSource.hand_over: false unless that stream is up, isn't recovering or moving already, and link has the
    /// route ready next to the running one; the worker then tries it and the Connection card hears how it went.
    fn hand_over_stream(&mut self, route: &RouteSpec, source_id: Option<&str>) -> bool {
        let st = &self.state.streams;
        let Some(id) = source_id.map(String::from).or_else(|| st.source_id.clone()) else { return false };
        if !st.can_hand_over.contains(&id) {
            return false;
        }
        let Some(url) = self.state.link.as_ref().and_then(|l| l.prepare_route(&id, route)) else { return false };
        self.state.streams.can_hand_over.retain(|s| *s != id);
        self.state.effects.push(Effect::HandOver { source_id: id, route: route.clone(), url });
        true
    }
    fn focused_source_id(&self) -> Option<String> {
        self.state.streams.source_id.clone().or_else(|| self.state.selected.clone())
    }
    fn background_streams(&self) -> Vec<BackgroundStream> {
        Vec::new()
    }
    /// StreamFlow::pick_source's answer, known now from the streams as they are: the host takes over (moving the panels
    /// to a stream that's up, or swapping the focused stream while several run); false leaves the pick to the caller.
    fn pick_source(&mut self, source_id: Option<&str>) -> bool {
        let st = &self.state.streams;
        let id = source_id.unwrap_or("");
        let takes = st.ids.iter().any(|s| s == id) || (st.ids.len() > 1 && !st.waking && !st.preparing);
        if takes {
            self.state.effects.push(Effect::PickSource(source_id.map(String::from)));
        }
        takes
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
    fn update_stream_output(&mut self, change: OutputChange, source_id: Option<&str>) {
        self.state.effects.push(Effect::UpdateOutput { change, source_id: source_id.map(String::from) });
    }
    fn canvas_size(&self) -> (Option<u32>, Option<u32>) {
        let setup = self.state.plugin_config("setup");
        let dim = |k: &str| setup.as_ref().and_then(|c| c.get(k)).and_then(Value::as_u64).map(|v| v as u32);
        (dim("canvas_width"), dim("canvas_height"))
    }
    fn restart_vcam_canvas(&mut self, _width: u32, _height: u32) -> RequestId {
        self.state.request_id()
    }
    fn stream_output(&self, source_id: &str) -> String {
        self.state.streams.cameras.iter().find(|c| c.source_id == source_id).map(|c| c.label.clone()).unwrap_or_default()
    }
    fn stream_on_camera(&self, slot: u32) -> Option<(String, Option<u32>, Option<u32>)> {
        let c = self.state.streams.cameras.iter().find(|c| c.slot == slot)?;
        Some((c.source_id.clone(), c.size.map(|s| s.0), c.size.map(|s| s.1)))
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
    fn run_action(&mut self, plugin: &str, id: &str, params: &Value) -> Option<String> {
        if plugin == self.me {
            return None;
        }
        let i = self.state.index(plugin)?;
        self.state.call(i, |p, host| p.run_action(host, id, params)).flatten()
    }
    fn release_action(&mut self, plugin: &str, id: &str, params: &Value) {
        if plugin == self.me {
            return;
        }
        if let Some(i) = self.state.index(plugin) {
            self.state.call(i, |p, host| p.release_action(host, id, params));
        }
    }
    fn window_active(&self) -> bool {
        self.state.window_active
    }
    fn copy_text(&mut self, text: &str) {
        self.state.effects.push(Effect::CopyText(text.into()));
    }
    fn clipboard_text(&mut self) -> Option<String> {
        // The copy is an effect the window carries out after this hook, so it's done by the time a later hook reads it back
        self.state.clipboard.as_ref().and_then(|read| read())
    }
    fn set_shortcut_menu(&mut self, bindings: Option<Vec<telescope_core::ui::ShortcutEntry>>) {
        self.state.shortcut_menu = bindings.map(|b| (self.me.clone(), b));
        self.state.effects.push(Effect::ShortcutMenu);
    }
    fn pick_file(&mut self, dialog: &FileDialog) -> Option<RequestId> {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let request = self.state.request_id();
        self.state.effects.push(Effect::PickFile { plugin: self.me.clone(), request, dialog: dialog.clone() });
        Some(request)
    }
}

impl HostUi for Ctx<'_> {
    fn set_ui_props(&mut self, ui: &str, props: &Value) {
        self.state.effects.push(Effect::Props { plugin: self.me.clone(), ui: ui.into(), props: props.clone() });
    }
    fn update_tray_action(&mut self, action: &TrayAction) {
        if let Some(i) = self.state.index(&self.me) {
            match self.state.slots[i].tray.iter_mut().find(|a| a.id == action.id) {
                Some(a) => *a = action.clone(),
                None => self.state.slots[i].tray.push(action.clone()),
            }
        }
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
        let st = &mut *self.state;
        let said = st.vc.set_view(&mut st.view, &change, crate::shell::now_s());
        st.apply_said(said);
    }
    fn take_view(&mut self, owner: &str) -> bool {
        let (took, said) = self.state.vc.take(owner);
        self.state.apply_said(said);
        took
    }
    fn steer_view(&mut self, owner: &str, zoom: f32, pan_x: f32, pan_y: f32) {
        let st = &mut *self.state;
        let said = st.vc.steer(&mut st.view, owner, zoom, pan_x, pan_y, crate::shell::now_s());
        st.apply_said(said);
    }
    fn release_view(&mut self, owner: &str) {
        let st = &mut *self.state;
        let said = st.vc.release(&mut st.view, owner, crate::shell::now_s());
        st.apply_said(said);
    }
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

impl Ctx<'_> {
    /// The stream a photo is for: the one asked about, or the panels', if it's running.
    fn still_source(&self, source_id: Option<&str>) -> Option<String> {
        let id = match source_id {
            Some(id) => id.to_string(),
            None => self.focused_source_id().unwrap_or_default(),
        };
        self.state.streams.live.contains(&id).then_some(id)
    }

    fn stills(&self) -> Option<Arc<dyn telescope_core::link::PhoneStills>> {
        self.state.link.as_ref().and_then(|l| l.stills())
    }
}

impl HostStills for Ctx<'_> {
    fn still_caps(&self, source_id: Option<&str>) -> Option<Value> {
        let id = self.still_source(source_id)?;
        self.stills()?.caps(Some(&id))
    }
    fn take_still(&mut self, options: &Value, folder: &str, source_id: Option<&str>) -> Option<RequestId> {
        let id = self.still_source(source_id)?;
        let stills = self.stills()?;
        if stills.caps(Some(&id)).is_none() || stills.busy(Some(&id)) {
            return None;
        }
        let request = self.state.request_id();
        self.state.stills.push((request, self.me.clone(), id.clone()));
        self.state.effects.push(Effect::TakeStill { plugin: self.me.clone(), request, source: id, options: options.clone(), folder: folder.into() });
        Some(request)
    }
    fn still_busy(&self, source_id: Option<&str>) -> bool {
        let Some(id) = self.still_source(source_id) else { return false };
        self.stills().is_some_and(|s| s.busy(Some(&id)))
    }
}

impl HostColour for Ctx<'_> {
    fn set_colour_stage(&mut self, owner: &str, stage: Option<ColourStage>, order: i32, source_id: Option<&str>) {
        self.state.colour.set_stage(owner, stage, order, source_id);
        // The pipeline bakes the stages into a new LUT for the stream's step
        self.state.effects.push(Effect::FrameStep("colour".into()));
    }
    fn colour_split(&mut self, owner: &str, position: Option<f32>) {
        self.state.colour.set_split(owner, position);
        // A compare needs the picture from before the colour step kept for the preview
        self.state.effects.push(Effect::FrameStep("colour".into()));
    }
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
