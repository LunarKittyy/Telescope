//! The Phototrap plugin: zones drawn on the preview are watched for movement while it's armed, and each time something moves in one
//! the phone takes a short burst of photos. Nothing is saved until something sets it off, and only photos, never video.

use crate::stills::{build_options, default_folder, defaults, offered, StillCaps};
use crate::shared::Paired;
use serde_json::{json, Value};
use telescope_core::event::Event;
use telescope_core::host::{Host, PickKind, PreviewLayer, RequestId, Shape};
use telescope_core::i18n::{n_, tr, trn, Fields};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::time::{local_clock, monotonic_ns};
use telescope_core::ui::{CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "phototrap";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/phototrap.slint"));

/// Zones that can be drawn.
pub const MAX_ZONES: usize = 4;
pub const DEFAULT_SENSITIVITY: u32 = 5;
pub const DEFAULT_COUNT: u32 = 3;
pub const DEFAULT_PAUSE_S: u32 = 10;
const MAX_PAUSE_S: u32 = 120;
/// A zone smaller than this (a fraction of the picture, each way) is a stray click.
const MIN_ZONE: f32 = 0.02;
const WHOLE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

const SWITCH_TIP: &str = n_("Watch the zones and take photos when something moves in them.");
const DRAW_TIP: &str = n_("Drag across the preview to mark a place to watch. Up to four.");
const CLEAR_TIP: &str = n_("Remove every zone. With none, the whole picture is watched.");
const NIGHT_TIP: &str = n_("Use the phone maker's Night mode for the photos.");
const NIGHT_OFF_TIP: &str = n_("This lens has no Night mode to offer");
const COUNT_TIP: &str = n_("Number of photos each time it goes off.");
const COUNT_OFF_TIP: &str = n_("This lens takes one photo at a time");

pub struct PhototrapPlugin {
    armed: bool,
    zones: Vec<[f32; 4]>,
    sensitivity: u32,
    count: u32,
    pause_s: u32,
    night: bool,
    paired: Paired,
    /// A watch is running on the host.
    watching: bool,
    /// The stream ended and no start has been heard since.
    down: bool,
    window_shown: bool,
    caps: Option<StillCaps>,
    folder: String,
    triggers: u32,
    photos: u32,
    last: String,
    /// The bursts asked for that haven't finished.
    taking: Vec<RequestId>,
    /// When the next trigger is allowed, in seconds on the clock.
    ready_at: f64,
    /// What went wrong with the last burst, until the next one works.
    problem: Option<String>,
    /// A pick on the preview is under way.
    drawing: bool,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
    clock: Box<dyn Fn() -> f64 + Send>,
}

impl Default for PhototrapPlugin {
    fn default() -> Self {
        PhototrapPlugin::new()
    }
}

impl PhototrapPlugin {
    pub fn new() -> PhototrapPlugin {
        PhototrapPlugin {
            armed: false,
            zones: Vec::new(),
            sensitivity: DEFAULT_SENSITIVITY,
            count: DEFAULT_COUNT,
            pause_s: DEFAULT_PAUSE_S,
            night: false,
            paired: Paired::default(),
            watching: false,
            down: false,
            window_shown: true,
            caps: None,
            folder: std::path::Path::new(&default_folder()).join("Phototrap").display().to_string(),
            triggers: 0,
            photos: 0,
            last: String::new(),
            taking: Vec::new(),
            ready_at: 0.0,
            problem: None,
            drawing: false,
            pending: false,
            clock: Box::new(|| monotonic_ns() as f64 / 1e9),
        }
    }

    #[cfg(test)]
    fn with_clock(clock: impl Fn() -> f64 + Send + 'static) -> PhototrapPlugin {
        PhototrapPlugin { clock: Box::new(clock), ..PhototrapPlugin::new() }
    }

    #[cfg(test)]
    fn with_folder(folder: &str) -> PhototrapPlugin {
        PhototrapPlugin { folder: folder.into(), ..PhototrapPlugin::new() }
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn burst_max(&self) -> i64 {
        self.caps.as_ref().map_or(1, |c| c.burst_max)
    }

    /// The photos per trigger in use: what was picked within what the lens offers, 1 where it has no bursts.
    fn count_now(&self) -> u32 {
        let max = self.burst_max();
        if max < 2 { 1 } else { i64::from(self.count).clamp(2, max) as u32 }
    }

    fn night_offered(&self) -> bool {
        let Some(caps) = &self.caps else { return false };
        let mut choices = defaults();
        choices.insert("mode".into(), json!("night"));
        offered(caps, &choices).modes.contains(&"night")
    }

    /// Whether a phone that can take photos is streaming.
    fn can_snap(&self, host: &dyn Host) -> bool {
        !self.down && host.is_streaming() && host.still_caps(None).is_some()
    }

    fn zones_to_watch(&self) -> Vec<[f32; 4]> {
        if self.zones.is_empty() { vec![WHOLE] } else { self.zones.clone() }
    }

    /// The photo options for the picks and what the lens offers.
    fn options(&self, caps: &StillCaps, host: &dyn Host) -> Value {
        let mut choices = defaults();
        let burst = caps.burst_max >= 2;
        choices.insert("shots".into(), json!(if burst { "burst" } else { "single" }));
        choices.insert("burst".into(), json!(self.count_now().max(2)));
        if self.night && self.night_offered() {
            choices.insert("mode".into(), json!("night"));
        }
        let rotation = host.view().rotation.filter(|r| [90, 180, 270].contains(r)).unwrap_or(0);
        build_options(caps, &choices, rotation)
    }

    // ── What the card shows ───────────────────────────────────────────

    fn status(&self, host: &dyn Host) -> (String, &'static str) {
        if !self.armed {
            return (tr("Not armed"), "status_dim");
        }
        if let Some(problem) = &self.problem {
            return (problem.clone(), "status_err");
        }
        if !self.can_snap(host) {
            return (tr("Waiting for a phone that can take photos"), "status_warn");
        }
        let text = if self.zones.is_empty() {
            tr("Armed and watching the whole picture")
        } else {
            trn("Armed and watching {n} zone", "Armed and watching {n} zones", self.zones.len() as i64)
        };
        (text, "status_ok")
    }

    fn tally(&self) -> String {
        if self.triggers > 0 {
            tr("Triggers: {triggers}, photos: {photos}, last at {time}")
                .field("triggers", self.triggers).field("photos", self.photos).field("time", &self.last)
        } else if self.armed {
            tr("Nothing has set it off yet")
        } else {
            String::new()
        }
    }

    fn fixed_props() -> Value {
        json!({"switch-name": tr("Armed"), "switch-help": tr(SWITCH_TIP), "draw-text": tr("Draw zone"), "draw-tip": tr(DRAW_TIP),
               "clear-text": tr("Clear zones"), "clear-tip": tr(CLEAR_TIP)})
    }

    fn state_props(&self, host: &dyn Host) -> Value {
        let (status, kind) = self.status(host);
        let max = self.burst_max();
        let night_offered = self.night_offered();
        json!({
            "active": self.paired.on(), "armed": self.armed, "status": status, "status-kind": kind, "tally": self.tally(),
            "zone-count": self.zones.len(), "draw-enabled": self.zones.len() < MAX_ZONES, "clear-enabled": !self.zones.is_empty(),
            "sensitivity": self.sensitivity, "sensitivity-text": self.sensitivity.to_string(),
            "count": self.count_now().max(2), "count-max": max.max(3), "count-text": self.count_now().to_string(), "count-enabled": max >= 2,
            "count-tip": tr(if max >= 2 { COUNT_TIP } else { COUNT_OFF_TIP }),
            "pause": self.pause_s, "pause-text": tr("{seconds} s").field("seconds", self.pause_s),
            "night": self.night, "night-enabled": night_offered,
            "night-tip": tr(if night_offered { NIGHT_TIP } else { NIGHT_OFF_TIP }),
        })
    }

    fn all_props(&self, host: &dyn Host) -> Value {
        let mut all = Self::fixed_props();
        if let (Value::Object(a), Value::Object(s)) = (&mut all, self.state_props(host)) {
            a.extend(s);
        }
        all
    }

    fn show(&self, host: &mut dyn Host) {
        let props = self.state_props(host);
        host.set_ui_props("card", &props);
    }

    fn layer(&self) -> PreviewLayer {
        let mut shapes = Vec::new();
        for (i, z) in self.zones.iter().enumerate() {
            shapes.push(Shape::Rect { x0: z[0], y0: z[1], x1: z[2], y1: z[3], stroke: "#f2b441".into(), width: 2.0, fill: Some("#f2b44126".into()) });
            shapes.push(Shape::Text { x: z[0] + 0.01, y: z[1] + 0.01, text: (i + 1).to_string(), color: "#f2b441".into(), size: 14.0 });
        }
        PreviewLayer { shapes }
    }

    /// The zones are on the preview while the window shows or it's armed.
    fn show_layer(&self, host: &mut dyn Host) {
        if self.armed || self.window_shown {
            host.add_preview_layer(NAME, self.layer());
        } else {
            host.remove_preview_layer(NAME);
        }
        host.refresh_preview();
    }

    // ── Watching ──────────────────────────────────────────────────────

    /// Watches the zones while armed and a phone can take photos, and stops otherwise.
    fn sync(&mut self, host: &mut dyn Host) {
        self.caps = host.still_caps(None).and_then(|v| serde_json::from_value(v).ok());
        if self.armed && self.can_snap(host) {
            host.watch_motion(NAME, &self.zones_to_watch(), self.sensitivity);
            self.watching = true;
        } else if self.watching {
            host.watch_motion(NAME, &[], self.sensitivity);
            self.watching = false;
        }
        self.show(host);
    }

    fn arm(&mut self, host: &mut dyn Host, on: bool) {
        self.armed = on;
        self.problem = None;
        if on {
            self.ready_at = 0.0;
        }
        self.sync(host);
        self.show_layer(host);
    }

    fn motion(&mut self, host: &mut dyn Host) {
        let now = self.now();
        if !self.armed || now < self.ready_at || host.still_busy(None) {
            return;
        }
        let Some(raw) = host.still_caps(None) else { return };
        let caps: StillCaps = serde_json::from_value(raw).unwrap_or_default();
        let options = self.options(&caps, host);
        let Some(id) = host.take_still(&options, &self.folder, None) else { return };
        self.taking.push(id);
        self.triggers += 1;
        self.last = local_clock();
        self.ready_at = now + f64::from(self.pause_s);
        self.show(host);
    }

    fn done(&mut self, host: &mut dyn Host, ok: bool, data: &Value) {
        let files = data.get("files").and_then(Value::as_array).map_or(0, Vec::len);
        if ok && files > 0 {
            self.photos += files as u32;
            self.problem = None;
        } else {
            let error = data.get("error").and_then(Value::as_str).filter(|e| !e.is_empty());
            self.problem = Some(error.map_or_else(|| tr("The phone couldn't take the photos"), String::from));
        }
        // The pause counts from when the burst ended
        self.ready_at = self.now() + f64::from(self.pause_s);
        self.show(host);
    }

    fn picked(&mut self, host: &mut dyn Host, pick: &telescope_core::host::Pick) {
        self.drawing = false;
        let &[a, b] = pick.shown.as_slice() else { return };
        let zone = [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])].map(|v| v.clamp(0.0, 1.0));
        if zone[2] - zone[0] < MIN_ZONE || zone[3] - zone[1] < MIN_ZONE || self.zones.len() >= MAX_ZONES {
            return;
        }
        self.zones.push(zone);
        self.zones_changed(host);
    }

    fn zones_changed(&mut self, host: &mut dyn Host) {
        self.sync(host);
        self.show_layer(host);
        host.schedule_save();
    }

    fn draw(&mut self, host: &mut dyn Host) {
        if self.zones.len() >= MAX_ZONES {
            return;
        }
        self.drawing = host.pick_on_preview(NAME, PickKind::Rect, None, &tr("Drag across the picture to mark a zone. Esc cancels."));
    }

    fn set_number(&mut self, host: &mut dyn Host, which: &str, value: f64) {
        let v = value.round() as u32;
        match which {
            "sensitivity" => self.sensitivity = v.clamp(1, 10),
            "count" => self.count = v.clamp(2, 64),
            _ => self.pause_s = v.clamp(1, MAX_PAUSE_S),
        }
        self.sync(host);
        host.schedule_save();
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            self.caps = host.still_caps(None).and_then(|v| serde_json::from_value(v).ok());
            let props = self.all_props(host);
            host.set_ui_props("card", &props);
            self.show_layer(host);
        }
    }
}

impl Plugin for PhototrapPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Phototrap").into(),
            about: n_("Take a burst of photos when something moves in the zones you draw, for wildlife and anything that shows up while nobody's looking.").into(),
            when_off: n_("Without the Phototrap plugin there's no Phototrap card and nothing is watched for movement.").into(),
            category: "camera".into(),
            icon: "eye".into(),
            optional: true,
            lab: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.caps = host.still_caps(None).and_then(|v| serde_json::from_value(v).ok());
        let props = self.all_props(host);
        host.set_ui_props("card", &props);
        self.show_layer(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        if self.watching {
            host.watch_motion(NAME, &[], self.sensitivity);
            self.watching = false;
        }
        if self.drawing {
            host.cancel_pick(NAME);
        }
        host.remove_preview_layer(NAME);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        match event {
            Event::MotionSeen { owner, zones, .. } if owner == NAME && !zones.is_empty() => self.motion(host),
            Event::PreviewPicked { owner, pick } if owner == NAME => match pick {
                Some(pick) => self.picked(host, pick),
                None => self.drawing = false,
            },
            Event::RequestDone { request, ok, data, .. } if self.taking.contains(request) => {
                self.taking.retain(|r| r != request);
                self.done(host, *ok, data);
            }
            Event::WindowShown { shown } => {
                self.window_shown = *shown;
                self.show_layer(host);
            }
            Event::StreamStopped => {
                self.down = true;
                self.taking.clear();
                self.sync(host);
            }
            Event::StreamStarted { .. } | Event::StreamConnected { .. } => {
                self.down = false;
                self.sync(host);
            }
            Event::StillCapsChanged { .. } | Event::StreamLost { .. } | Event::SourceSelected { .. } | Event::CameraSwitched { .. } => self.sync(host),
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        let props = json!({"switch-name": tr("Armed"), "switch-help": tr(SWITCH_TIP), "draw-text": tr("Draw zone"), "draw-tip": tr(DRAW_TIP),
                           "clear-text": tr("Clear zones"), "clear-tip": tr(CLEAR_TIP), "active": self.paired.on(), "armed": self.armed,
                           "status": tr("Not armed"), "status-kind": "status_dim", "count-max": 8, "count": 3, "count-enabled": false, "night-enabled": false,
                           "count-text": "1", "night-tip": tr(NIGHT_OFF_TIP), "pause-text": tr("{seconds} s").field("seconds", self.pause_s),
                           "sensitivity-text": self.sensitivity.to_string(), "sensitivity": self.sensitivity, "pause": self.pause_s});
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "PhototrapCard".into(), props }, region: "right".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let flag = event.args.first().and_then(Value::as_bool).unwrap_or(false);
        let number = event.args.first().and_then(Value::as_f64).unwrap_or(0.0);
        match event.callback.as_str() {
            "switch-toggled-by-hand" => self.arm(host, flag),
            "draw-clicked" => self.draw(host),
            "clear-clicked" => {
                self.zones.clear();
                self.zones_changed(host);
            }
            "sensitivity-moved" => self.set_number(host, "sensitivity", number),
            "count-moved" => self.set_number(host, "count", number),
            "pause-moved" => self.set_number(host, "pause", number),
            "night-toggled" => {
                self.night = flag;
                self.show(host);
                host.schedule_save();
            }
            _ => {}
        }
    }

    /// Armed isn't saved, so a restart never starts taking photos.
    fn get_config(&self) -> Value {
        json!({"zones": self.zones, "sensitivity": self.sensitivity, "count": self.count, "pause": self.pause_s, "night": self.night})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let number = |key: &str, default: u32, low: u32, high: u32| {
            cfg.get(key).and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(default, |v| (v.round() as i64).clamp(i64::from(low), i64::from(high)) as u32)
        };
        self.zones = cfg.get("zones").and_then(Value::as_array).map_or_else(Vec::new, |list| {
            list.iter()
                .filter_map(|z| serde_json::from_value::<[f32; 4]>(z.clone()).ok())
                .filter(|z| z.iter().all(|v| (0.0..=1.0).contains(v)) && z[2] - z[0] >= MIN_ZONE && z[3] - z[1] >= MIN_ZONE)
                .take(MAX_ZONES)
                .collect()
        });
        self.sensitivity = number("sensitivity", DEFAULT_SENSITIVITY, 1, 10);
        self.count = number("count", DEFAULT_COUNT, 2, 64);
        self.pause_s = number("pause", DEFAULT_PAUSE_S, 1, MAX_PAUSE_S);
        self.night = cfg.get("night") == Some(&Value::Bool(true));
        self.pending = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("Phototrap".into(), format!("{}, {} zones, {} triggers", if self.armed { "armed" } else { "off" }, self.zones.len(), self.triggers))]
    }
}

#[cfg(test)]
#[path = "phototrap_tests.rs"]
mod tests;
