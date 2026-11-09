//! The host a plugin's hook gets: the shell's state, seen from that one plugin. What the window has to carry out goes
//! into State::effects. Services that live in crates still being ported (vision, sensors, stills, colour, motion,
//! packets, the preview's layers) answer as "not available" until those land.

use crate::shell::{Effect, Picture, State};
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

/// The phone sensors' newest reading, readable from any thread.
struct HubReader(Arc<dyn telescope_core::link::PhoneSensors>);

impl SensorReader for HubReader {
    fn reading(&self, source_id: Option<&str>) -> Option<Value> {
        self.0.reading(source_id)
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
            let rejected = self.state.call(i, |p, host| p.apply_preset(host, &cfg));
            if let Some(Err(why)) = rejected {
                log::warn!("{name} couldn't take its part of a preset: {}", why.0);
            }
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
    fn plugin_settings(&self) -> Vec<PluginSettings> {
        self.state.slots.iter().filter_map(|s| {
            let settings = s.plugin.as_ref()?.settings();
            (!settings.is_empty()).then(|| PluginSettings { name: s.info.name.clone(), title: s.info.title.clone(), settings })
        }).collect()
    }
    fn running_plugins(&self) -> Vec<(String, String)> {
        self.state.slots.iter().filter(|s| !s.info.name.is_empty()).map(|s| (s.info.name.clone(), s.info.title.clone())).collect()
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
        // Shown or not, up or still waking, as Python's Sessions.is_streaming_from
        let (st, id) = (&self.state.streams, source_id.unwrap_or_default());
        st.waking_id.as_deref() == Some(id) || st.ids.iter().any(|s| s == id)
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
        self.state.effects.push(Effect::Reconnect(None));
    }
    fn reconnect_source(&mut self, source_id: &str) {
        self.state.effects.push(Effect::Reconnect(Some(source_id.into())));
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
        self.state.streams.background.clone()
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
        self.state.plugin_config("setup").map_or((None, None), |setup| telescope_core::canvas::canvas_dims(&setup))
    }
    fn restart_vcam_canvas(&mut self, _width: u32, _height: u32, shows_command: bool) -> RequestId {
        // The sizes aren't needed: the restart reads the canvas from Setup's settings, as app.py's does
        let request = self.state.request_id();
        self.state.effects.push(Effect::RestartCanvas { plugin: self.me.clone(), request, shows_command });
        request
    }
    fn stream_output(&self, source_id: &str) -> String {
        self.state.streams.cameras.iter().find(|c| c.source_id == source_id).map(|c| c.label.clone()).unwrap_or_default()
    }
    fn stream_on_camera(&self, slot: u32) -> Option<(String, Option<u32>, Option<u32>)> {
        let c = self.state.streams.cameras.iter().find(|c| c.slot == slot)?;
        Some((c.source_id.clone(), c.size.map(|s| s.0), c.size.map(|s| s.1)))
    }
    fn remove_extra_cameras(&mut self) -> RequestId {
        let request = self.state.request_id();
        self.state.effects.push(Effect::RemoveExtras { plugin: self.me.clone(), request });
        request
    }
    fn phone_control(&mut self, source_id: Option<&str>, body: &Value) {
        self.state.effects.push(Effect::PhoneControl { source_id: source_id.map(String::from), body: body.clone() });
    }
    fn phone_state(&self, _source_id: Option<&str>) -> Option<Value> {
        self.state.streams.phone_state.clone()
    }
    fn request_phone_state(&mut self, source_id: Option<&str>) {
        self.state.effects.push(Effect::RequestPhoneState { source_id: source_id.map(String::from) });
    }
}

impl HostCamera for Ctx<'_> {
    fn is_camera_on(&self) -> bool {
        !self.state.streams.camera_off
    }
    fn is_camera_off_auto(&self) -> bool {
        self.state.streams.camera_off_auto
    }
    fn can_turn_camera_off(&self) -> (bool, String) {
        self.state.streams.can_turn_off.clone()
    }
    fn set_camera_on(&mut self, on: bool, auto: bool) {
        // Asked from inside a plugin, so it happens after this call; a plugin reading it back has the answer now
        self.state.streams.camera_off = !on;
        self.state.streams.camera_off_auto = !on && auto;
        self.state.effects.push(Effect::CameraOn { on, auto });
    }
}

impl HostSources for Ctx<'_> {
    fn add_stream_source(&mut self, source: Arc<dyn StreamSource>) {
        self.state.effects.push(Effect::AddSource(crate::shell::OfferedSource(source)));
    }
    fn remove_stream_source(&mut self, source_id: &str) {
        self.state.effects.push(Effect::RemoveSource(source_id.into()));
    }
    fn stream_source(&mut self, source_id: &str) {
        self.state.effects.push(Effect::StreamSource(source_id.into()));
    }
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
    fn diagnostics_report(&self, own: &[(String, String)]) -> String {
        use telescope_core::diagnostics::{events, report_for};
        let hooks = crate::window::app_hooks();
        report_for(events(), &(hooks.build)(), (hooks.commit)(), &self.state.diagnostics_with(own), crate::window::windowing())
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
    fn set_plugin_hues(&mut self, on: bool) {
        self.state.effects.push(Effect::PluginHues(on));
    }
    fn set_classic_theme(&mut self, on: bool) {
        self.state.effects.push(Effect::ClassicTheme(on));
    }
    fn set_shortcut_menu(&mut self, bindings: Option<Vec<telescope_core::ui::ShortcutEntry>>) {
        self.state.shortcut_menu = bindings.map(|b| (self.me.clone(), b));
        self.state.effects.push(Effect::ShortcutMenu);
    }
    fn pick_file(&mut self, dialog: &FileDialog) -> Option<RequestId> {
        let request = self.state.request_id();
        self.state.effects.push(Effect::PickFile { plugin: self.me.clone(), request, dialog: dialog.clone() });
        Some(request)
    }
}

impl HostUi for Ctx<'_> {
    fn set_ui_props(&mut self, ui: &str, props: &Value) {
        self.state.effects.push(Effect::Props { plugin: self.me.clone(), ui: ui.into(), props: props.clone() });
    }
    fn offer_bar_stats(&mut self, stats: &[telescope_core::bar::BarStat]) {
        let me = self.me.clone();
        self.state.bar.offer(&me, stats);
    }
    fn bar_pins(&self) -> Vec<String> {
        self.state.bar.pinned()
    }
    fn pin_to_bar(&mut self, id: &str, pinned: bool) {
        if self.state.bar.pin(id, pinned) {
            if let Some(ids) = self.state.bar.pins_changed() {
                self.state.bus.emit(Event::BarPinsChanged { pinned: ids });
            }
            self.state.effects.push(Effect::SaveSoon);
        }
    }
    fn bar_stat(&self, id: &str) -> Option<telescope_core::bar::BarStat> {
        self.state.bar.stat(id).cloned()
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
    fn set_ui_image(&mut self, ui: &str, prop: &str, frame: &Frame) {
        self.state.effects.push(Effect::Image { plugin: self.me.clone(), ui: ui.into(), prop: prop.into(), picture: Picture(frame.clone()) });
    }
    fn refresh_actions(&mut self) {
        self.state.effects.push(Effect::RefreshActions(self.me.clone()));
    }
    fn open_window(&mut self, id: &str, window: &telescope_core::ui::WindowSpec) {
        self.state.effects.push(Effect::OpenWindow { plugin: self.me.clone(), id: id.into(), window: window.clone() });
    }
    fn close_window(&mut self, id: &str) {
        self.state.effects.push(Effect::CloseWindow { plugin: self.me.clone(), id: id.into() });
    }
}

impl HostView for Ctx<'_> {
    fn view(&self) -> View {
        self.state.view.clone()
    }
    fn view_aspect(&self) -> f32 {
        self.state.view_aspect()
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
    fn set_power_saving(&mut self, on: bool) {
        if let Some(vision) = &self.state.vision {
            vision.set_power_saving(on);
        }
    }
    fn vision_state(&self) -> String {
        self.state.vision.as_ref().map_or_else(|| telescope_core::vision::OFF.into(), |v| v.state())
    }
    fn want_people(&mut self, owner: &str, on: bool) {
        if let Some(vision) = &self.state.vision {
            vision.want_people(owner, on);
        }
    }
    fn tracked_people(&self, source_id: Option<&str>) -> (Vec<Value>, u32) {
        let Some(vision) = &self.state.vision else { return (Vec::new(), 0) };
        let id = source_id.map(str::to_string).or_else(|| self.focused_source_id()).unwrap_or_default();
        vision.tracked_people(&id)
    }
    fn vision_pack(&self) -> telescope_core::vision::VisionPack {
        self.state.vision.as_ref().map(|v| v.pack()).unwrap_or_default()
    }
    fn download_vision(&mut self) {
        if let Some(vision) = &self.state.vision {
            vision.download("");
        }
    }
}

impl HostFrames for Ctx<'_> {
    fn frames(&self) -> Arc<dyn FrameStore> {
        self.state.frames.clone().unwrap_or_else(|| Arc::new(NoFrames))
    }
}

impl HostMotion for Ctx<'_> {
    fn watch_motion(&mut self, owner: &str, zones: &[[f32; 4]], sensitivity: u32) {
        self.state.motion.watch(owner, &telescope_core::motion::widen(zones), i64::from(sensitivity));
    }
    fn motion(&self, owner: &str) -> Vec<u32> {
        self.focused_source_id().map(|id| self.state.motion.moving(&id, owner)).unwrap_or_default()
    }
}

impl HostPreview for Ctx<'_> {
    fn add_preview_layer(&mut self, owner: &str, layer: PreviewLayer) {
        self.state.layers.insert(owner.to_string(), layer);
    }
    fn remove_preview_layer(&mut self, owner: &str) {
        self.state.passes.retain(|(o, _)| o != owner);
        self.state.layers.remove(owner);
        self.state.preview_dirty.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    fn refresh_preview(&mut self) {
        self.state.preview_dirty.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    fn add_preview_pass(&mut self, owner: &str, pass: Arc<dyn telescope_core::preview::PreviewPass>) {
        self.state.passes.retain(|(o, _)| o != owner);
        self.state.passes.push((owner.to_string(), pass));
        self.state.preview_dirty.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    /// Only on a stream the preview card shows.
    fn pick_on_preview(&mut self, owner: &str, kind: PickKind, start: Option<[[f32; 2]; 4]>, hint: &str) -> bool {
        if !self.is_streaming() || !self.has_plugin("preview") {
            return false;
        }
        let st = &mut *self.state;
        st.overlays.start_pick(&st.view, owner, kind, start, hint, &st.bus);
        true
    }
    fn finish_pick(&mut self, owner: &str) -> bool {
        let st = &mut *self.state;
        st.overlays.finish_pick(&st.view, Some(owner), &st.bus)
    }
    fn cancel_pick(&mut self, owner: &str) {
        let st = &mut *self.state;
        st.overlays.cancel_pick(Some(owner), &st.bus);
    }
    fn set_preview_warp(&mut self, owner: &str, matrix: Option<[f32; 9]>) {
        self.state.overlays.set_warp(owner, matrix);
    }
    fn unwarp(&self, u: f64, v: f64) -> (f64, f64) {
        self.state.overlays.unwarp(u, v)
    }
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

    /// The photo service of a stream's own camera (a DSLR), for a stream that has one.
    fn camera_stills(&self, id: &str) -> Option<Arc<dyn telescope_core::source::SourceStills>> {
        self.state.source_stills.iter().find(|(s, _)| s == id).map(|(_, stills)| stills.clone())
    }
}

impl HostStills for Ctx<'_> {
    fn still_caps(&self, source_id: Option<&str>) -> Option<Value> {
        let id = self.still_source(source_id)?;
        if let Some(camera) = self.camera_stills(&id) {
            return camera.caps();
        }
        self.stills()?.caps(Some(&id))
    }
    fn take_still(&mut self, options: &Value, folder: &str, source_id: Option<&str>) -> Option<RequestId> {
        let id = self.still_source(source_id)?;
        let ready = match self.camera_stills(&id) {
            Some(camera) => camera.caps().is_some() && !camera.busy(),
            None => {
                let stills = self.stills()?;
                stills.caps(Some(&id)).is_some() && !stills.busy(Some(&id))
            }
        };
        if !ready {
            return None;
        }
        let request = self.state.request_id();
        self.state.stills.push((request, self.me.clone(), id.clone()));
        self.state.effects.push(Effect::TakeStill { plugin: self.me.clone(), request, source: id, options: options.clone(), folder: folder.into() });
        Some(request)
    }
    fn still_busy(&self, source_id: Option<&str>) -> bool {
        let Some(id) = self.still_source(source_id) else { return false };
        if let Some(camera) = self.camera_stills(&id) {
            return camera.busy();
        }
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
    fn want_sensors(&mut self, owner: &str, kinds: &[String]) {
        if let Some(sensors) = self.state.link.as_ref().and_then(|l| l.sensors()) {
            sensors.want(owner, kinds);
        }
    }
    fn sensor_status(&self, source_id: Option<&str>) -> Value {
        self.state.link.as_ref().and_then(|l| l.sensors()).map_or(Value::Null, |s| s.status(source_id))
    }
    fn sensors(&self) -> Arc<dyn SensorReader> {
        match self.state.link.as_ref().and_then(|l| l.sensors()) {
            Some(hub) => Arc::new(HubReader(hub)),
            None => Arc::new(NoSensors),
        }
    }
}

impl HostPackets for Ctx<'_> {
    fn packets_problem(&self) -> String {
        if self.state.streams.plugin_source {
            return telescope_core::i18n::tr("Only a phone's stream has the video this needs.");
        }
        let heavy = match &self.state.streams.url {
            Some(url) => url.ends_with(".mjpeg"),
            None => self.state.plugin_config("stream_output").is_some_and(|c| c["format"] == "mjpeg"),
        };
        if heavy { telescope_core::i18n::tr("Needs Light: Heavy sends whole pictures, with no keyframes to work with.") } else { String::new() }
    }
    /// Each change reaches the stream on its own, so taking a step off and on again gives it a fresh one.
    fn change_packets(&mut self, owner: &str, factory: Option<Arc<dyn PacketStepFactory>>) {
        let makers = &mut self.state.packet_makers.0;
        match (makers.iter().position(|(o, _)| o == owner), factory) {
            (Some(i), Some(f)) => makers[i].1 = f,
            (None, Some(f)) => makers.push((owner.to_string(), f)),
            (Some(i), None) => {
                makers.remove(i);
            }
            (None, None) => {}
        }
        self.state.effects.push(Effect::Packets(self.state.packet_makers.clone()));
    }
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
    fn want_stats(&mut self, owner: &str, on: bool) {
        telescope_core::stats::Hub::shared().want(owner, on);
    }
}

/// copy_action's Rust form, for a plugin's Issue: a button that copies text and leaves the banner up.
pub fn copy_url(text: &str) -> String {
    format!("{COPY_SCHEME}{text}")
}
