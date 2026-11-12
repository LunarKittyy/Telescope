//! The Expose for my face plugin (plugins/face_exposure.py): moves the phone's metering point (Tap a point's, or a meter-only one) to your face, calmly.

use crate::shared::Paired;
use crate::vision_row;
use serde_json::{json, Value};
use std::collections::VecDeque;
use telescope_core::actions;
use telescope_core::event::Event;
use telescope_core::host::Host;
use telescope_core::i18n::{n_, tr};
use telescope_core::people::{self, Track};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::view::{upright_to_shown, Geometry};

pub const NAME: &str = "face_exposure";
const TITLE: &str = n_("Expose for my face");
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/vision_status.slint"), "\n", include_str!("../ui/face_exposure.slint"));

/// The face moving less than this (of the picture) from the point leaves the point where it is.
pub const DEAD_ZONE: f64 = 0.08;
/// The face counts as settled while it stays within this of where it stopped.
pub const STILL: f64 = 0.04;
/// And has to stay settled this long before the point moves (not while you walk in or out).
pub const SETTLE_S: f64 = 0.8;
/// While settled, the face's position is smoothed with this time constant.
pub const SMOOTH_S: f64 = 0.5;
/// The point moves at most this often: each move has the phone meter and focus again.
pub const EVERY_S: f64 = 1.5;

const FOLLOW_TIP: &str = n_("Go back to metering the exposure on your face.");
const PICKED_NOTE: &str = n_("The phone meters on the point you picked by hand.");
const MANUAL_NOTE: &str = n_("Focus is on Manual in the Camera card.");
const CANT_METER: &str = n_("This lens can't meter on a point.");

type Point = (f64, f64);

fn dist(a: Point, b: Point) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Where the metering point goes: only once the face has settled somewhere new, not for small moves, and nowhere while there's no face.
#[derive(Clone, Debug)]
pub struct FaceMeter {
    /// Where it was last moved to.
    pub point: Option<Point>,
    smooth: Option<Point>,
    at: Option<f64>,
    /// Where the face last stopped, and since when it's been there.
    stopped: Option<Point>,
    since: f64,
    moved_at: f64,
}

impl Default for FaceMeter {
    fn default() -> Self {
        FaceMeter { point: None, smooth: None, at: None, stopped: None, since: 0.0, moved_at: f64::NEG_INFINITY }
    }
}

impl FaceMeter {
    /// face: (x, y) of your face, or None while you're not there. Returns where to move the point now, or None to leave it.
    pub fn update(&mut self, face: Option<Point>, now: f64) -> Option<Point> {
        let Some(face) = face else {
            (self.stopped, self.smooth) = (None, None);
            return None;
        };
        let mut smooth = match (self.smooth, self.at) {
            (Some(old), Some(at)) => {
                let a = 1.0 - (-(now - at).max(0.0) / SMOOTH_S).exp();
                (old.0 + (face.0 - old.0) * a, old.1 + (face.1 - old.1) * a)
            }
            _ => face,
        };
        self.at = Some(now);
        // Moving: smoothing starts over where it stops
        if self.stopped.is_none_or(|stopped| dist(face, stopped) > STILL) {
            (self.stopped, self.since, smooth) = (Some(face), now, face);
        }
        self.smooth = Some(smooth);
        if now - self.since < SETTLE_S || now - self.moved_at < EVERY_S {
            return None;
        }
        if self.point.is_some_and(|point| dist(smooth, point) <= DEAD_ZONE) {
            return None;
        }
        (self.point, self.moved_at) = (Some(smooth), now);
        self.point
    }

    pub fn reset(&mut self) {
        *self = FaceMeter::default();
    }
}

pub struct FaceExposurePlugin {
    meter: FaceMeter,
    /// The card's switch; `on` follows it once there's a host.
    switch: bool,
    on: bool,
    /// A point was picked by hand, which wins until the plugin is switched off and on again.
    manual: bool,
    /// The Camera card is on Manual focus, which a point would undo.
    camera_manual: bool,
    /// Follow my face was clicked, so a point goes out even then.
    forced: bool,
    /// The lens can focus (and meter) on a point, and a stream is there to send it to.
    focus_ok: bool,
    /// The lens can meter on a point without focusing there.
    meter_ok: bool,
    /// Points we said that haven't come back round the bus yet, which is how ours are told from one picked by hand.
    ours: VecDeque<(f32, f32)>,
    paired: Paired,
    held: Option<bool>,
    /// set_config has no host: asking for people and the card's update wait for the next hook that has one.
    pending: bool,
    /// Settings that switched it off came in before that hook, as a device change's defaults do: it goes through off first.
    cycle: bool,
    clock: Box<dyn Fn() -> f64 + Send>,
}

impl Default for FaceExposurePlugin {
    fn default() -> Self {
        FaceExposurePlugin::new()
    }
}

impl FaceExposurePlugin {
    pub fn new() -> FaceExposurePlugin {
        FaceExposurePlugin::with_clock(|| monotonic_ns() as f64 / 1e9)
    }

    /// With its own clock in seconds, for a test to run time by hand.
    pub fn with_clock(clock: impl Fn() -> f64 + Send + 'static) -> FaceExposurePlugin {
        FaceExposurePlugin {
            meter: FaceMeter::default(),
            switch: false,
            on: false,
            manual: false,
            camera_manual: false,
            forced: false,
            focus_ok: false,
            meter_ok: false,
            ours: VecDeque::new(),
            paired: Paired::default(),
            held: None,
            pending: false,
            cycle: false,
            clock: Box::new(clock),
        }
    }

    fn fixed_props() -> Value {
        json!({"switch-name": tr(TITLE), "follow-tip": tr(FOLLOW_TIP)})
    }

    fn state_props(&self) -> Value {
        json!({"on": self.switch, "active": self.paired.on(), "phone-only-hidden": self.paired.source()})
    }

    fn all_props(&self) -> Value {
        let mut all = Self::fixed_props();
        if let (Value::Object(a), Value::Object(s)) = (&mut all, self.state_props()) {
            a.extend(s);
        }
        all
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            host.set_ui_props("card", &self.state_props());
            if std::mem::take(&mut self.cycle) {
                self.apply(host, false);
            }
            self.apply(host, self.switch);
        }
    }

    fn apply(&mut self, host: &mut dyn Host, on: bool) {
        if on != self.on {
            self.on = on;
            (self.manual, self.forced) = (false, false);
            self.meter.reset();
            host.want_people(TITLE, on);
        }
        self.show(host);
    }

    fn switched_by_hand(&mut self, host: &mut dyn Host, on: bool) {
        self.switch = on;
        host.set_ui_props("card", &json!({"on": on}));
        host.schedule_save();
        self.apply(host, on);
    }

    /// Back to the face after a point was picked by hand.
    fn follow_again(&mut self, host: &mut dyn Host) {
        (self.manual, self.forced) = (false, true);
        self.meter.reset();
        self.show(host);
    }

    /// Something the user chose keeps the point where it is.
    fn is_held(&self) -> bool {
        self.manual || (self.camera_manual && !self.forced)
    }

    fn show(&mut self, host: &mut dyn Host) {
        let held = self.on && self.is_held();
        let note = tr(if self.manual { PICKED_NOTE } else { MANUAL_NOTE });
        let problem = if self.on && self.how().is_empty() && host.is_streaming() { tr(CANT_METER) } else { String::new() };
        let mut props = json!({"held": held, "held-note": note});
        if let (Value::Object(p), Value::Object(row)) = (&mut props, vision_row::props(&*host, self.on, &problem)) {
            p.extend(row);
        }
        host.set_ui_props("card", &props);
    }

    /// How the point goes to the phone: "focus" (it also focuses there), "meter" (only exposure) or "" (it can't).
    fn how(&self) -> &'static str {
        if self.focus_ok { "focus" } else if self.meter_ok { "meter" } else { "" }
    }

    fn set_available(&mut self, host: &mut dyn Host, focus: bool, meter: bool) {
        let was = self.how();
        (self.focus_ok, self.meter_ok) = (focus, meter);
        // The phone forgot the point (another lens, or the stream ended)
        if self.how() != was {
            self.meter.reset();
        }
        self.show(host);
    }

    fn on_point_picked(&mut self, host: &mut dyn Host) {
        if self.on {
            (self.manual, self.forced) = (true, false);
            self.show(host);
        }
    }

    fn on_mode_picked(&mut self, host: &mut dyn Host, mode: &str) {
        if mode != "manual" {
            self.on_point_picked(host);
        } else if self.on {
            // Held by Manual, not by a point
            (self.manual, self.camera_manual, self.forced) = (false, true, false);
            self.show(host);
        }
    }

    fn on_tracked(&mut self, host: &mut dyn Host, source_id: &str, tracks: &[Value]) {
        let how = self.how();
        if !self.on || self.is_held() || how.is_empty() {
            return;
        }
        if source_id != host.focused_source_id().unwrap_or_default() {
            return;
        }
        let tracks = Track::all_from(tracks);
        let face = people::main(&tracks).filter(|you| you.missing_s == 0.0).and_then(|you| you.face);
        let point = self.meter.update(face, (self.clock)());
        let Some(point) = point else { return };
        let Some((u, v)) = Self::shown(host, point) else {
            // Outside the picture as shown: tried again once you're back in it
            self.meter.point = None;
            return;
        };
        let (x, y) = (u as f32, v as f32);
        if how == "meter" {
            host.emit(Event::MeterPointPicked { x, y });
        } else {
            self.ours.push_back((x, y));
            host.emit(Event::FocusPointPicked { x, y });
        }
    }

    /// face, from the picture at zoom 1, in the picture as shown now; None when it's outside it.
    fn shown(host: &dyn Host, face: Point) -> Option<Point> {
        let (u, v) = upright_to_shown(&Geometry::from(&host.view()), face.0, face.1);
        ((0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)).then_some((u, v))
    }
}

impl Plugin for FaceExposurePlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: TITLE.into(),
            about: n_("Moves the phone's metering point to your face to keep you well lit whatever's behind you.").into(),
            when_off: n_("Without it there's no Expose for my face card and the phone meters the point you tap or the whole picture.").into(),
            category: "vision".into(),
            icon: "sun".into(),
            optional: true,
            needs_vision: true,
            in_presets: true,
            // Metering on your face only makes sense on a camera that sees it
            device_local: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.all_props());
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.pending = false;
        if self.on {
            host.want_people(TITLE, false);
        }
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on, "phone-only-hidden": self.paired.source()}));
        }
        match event {
            Event::PeopleTracked { source_id, tracks, .. } => self.on_tracked(host, source_id, tracks),
            Event::FocusPointPicked { x, y } => {
                if self.ours.front() == Some(&(*x, *y)) {
                    self.ours.pop_front();
                } else {
                    self.on_point_picked(host);
                }
            }
            Event::FocusModePicked { mode } => self.on_mode_picked(host, mode),
            Event::FocusModeChanged { mode } => {
                self.camera_manual = mode == "manual";
                self.show(host);
            }
            Event::FocusPointAvailable { available } => self.set_available(host, *available, self.meter_ok),
            Event::MeterPointAvailable { available } => self.set_available(host, self.focus_ok, *available),
            // The phone forgets the point on another lens
            Event::CameraSwitched { .. } => self.meter.reset(),
            e if vision_row::changes_it(e) => self.show(host),
            _ => {}
        }
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, _stream: &StreamInfo) {
        self.catch_up(host);
        self.meter.reset();
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
        self.meter.reset();
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "FaceExposureCard".into(), props: self.all_props() }, region: "right".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        match event.callback.as_str() {
            "switch-toggled-by-hand" => self.switched_by_hand(host, event.args.first().and_then(Value::as_bool).unwrap_or(false)),
            "follow-clicked" => self.follow_again(host),
            "download-clicked" => {
                host.download_vision();
                self.show(host);
            }
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        vec![actions::toggle_action("face_exposure.on", &tr(TITLE), &tr(TITLE), "", &[])]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        // A dimmed or hidden card's switch can't be flipped, by a key either
        if id != "face_exposure.on" || !self.paired.on() || self.paired.source() {
            return None;
        }
        let want = actions::toggle_wanted(params, self.switch, &mut self.held);
        if want != self.switch {
            self.switched_by_hand(host, want);
        }
        Some(actions::toggle_text(&tr(TITLE), want))
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        if id != "face_exposure.on" {
            return;
        }
        if let Some(before) = actions::hold_released(params, &mut self.held) {
            if before != self.switch && self.paired.on() && !self.paired.source() {
                self.switched_by_hand(host, before);
            }
        }
    }

    fn get_config(&self) -> Value {
        json!({"on": self.switch})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.switch = cfg.get("on") == Some(&Value::Bool(true));
        self.cycle |= self.on && !self.switch;
        self.pending = true;
        Ok(())
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        let done = self.set_config(cfg);
        self.catch_up(host);
        done
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let state = if !self.on {
            "off"
        } else if self.manual {
            "point picked by hand"
        } else if self.is_held() {
            "waiting (manual focus)"
        } else {
            "following"
        };
        vec![("Expose for my face".into(), state.into())]
    }
}

#[cfg(test)]
#[path = "face_exposure_tests.rs"]
mod tests;
