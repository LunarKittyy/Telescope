//! The Transforms plugin (plugins/transforms.py): zoom, pan, flip, rotate and stretch the picture. The card keeps the manual
//! view and hands it to the window (host.set_view); the window's view step does the pixels (core::view_step) and shares the zoom
//! with the phone. What the card knows about the phone's lenses comes from the view_status events and the phone's state.

use telescope_core::i18n::Fields;
use crate::shared::{number_in, Paired};
use serde_json::{json, Map, Value};
use telescope_core::actions::{self, Choice, SliderSpec};
use telescope_core::event::Event;
use telescope_core::host::{Host, ViewChange};
use telescope_core::i18n::{n_, number, percent, tr, tr_ctx};
use telescope_core::lens::{lens_name, lens_label};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo, StreamsBehind};
use telescope_core::settings::SettingSpec;
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::view::{self, lens_step, pan_for, shown_to_view, window, window_centre, zoom_detent, Geometry, PhoneZoomCaps, ViewStatus, ROTATIONS};

pub const NAME: &str = "transforms";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/transforms.slint"));

/// Rotation's segments, by the names saved settings and the shortcut use.
const ROTATION_SEGMENTS: [(&str, &str); 4] = [("None", "0\u{b0}"), ("90 CW", "90\u{b0}"), ("180", "180\u{b0}"), ("90 CCW", "270\u{b0}")];
/// A preview drag sticks at the centre within this much of the frame, on each axis.
const PAN_SNAP: f64 = 0.02;
/// Width and Height, in percent of the picture's own shape.
const STRETCH_RANGE: (i32, i32) = (50, 200);
/// How long the phone gets to take a new ratio before the dot says it didn't.
const LENS_SETTLE_S: f64 = 2.0;
/// The pan sliders' steps either way.
const PAN_STEPS: f64 = 200.0;
const DEFAULT_MAX_ZOOM: i32 = 10;
const T_SETTLE: &str = "settle";

const RESET_TIP: &str = n_("Set flip, rotation, zoom, pan and stretch back to default.");
const PAN_TIP: &str = n_("Drag to move. Double-click to centre.");
const STRETCH_TIPS: [&str; 2] = [n_("Stretch or squeeze the picture horizontally."), n_("Stretch or squeeze the picture vertically.")];
const RESET_HINT: &str = n_("Double-click to reset");

/// A hand-edited or damaged config value becomes the default instead of breaking every frame.
fn number_of(value: Option<&Value>, default: f64, lo: f64, hi: f64) -> f64 {
    match value.and_then(crate::shared::py_float) {
        Some(v) if v.is_finite() => v.clamp(lo, hi),
        _ => default,
    }
}

fn now_s() -> f64 {
    monotonic_ns() as f64 / 1e9
}

/// What the lens dot shows: (level, hover text). Level "" is nothing worth saying, so no dot; "on" the phone is on a longer
/// lens; "off" it isn't on the lens this zoom would get.
///
/// live, default and tele are lens names: the one streaming now, the one used unzoomed, and the last longer lens seen live.
/// "Switched" goes by what the phone reports, not by what it was asked for. `settled` is whether the ratio sent last has had
/// time to take, so a normal switch doesn't count as the phone refusing.
pub fn lens_note(zoom: f64, sent_ratio: f64, caps: Option<&PhoneZoomCaps>, live: &str, default: &str, tele: &str, settled: bool) -> (&'static str, String) {
    let mut lines: Vec<String> = Vec::new();
    let mut level = "";
    if !live.is_empty() && !default.is_empty() && live != default {
        lines.push(tr("Switched to {lens}").field("lens", lens_name(live)));
        level = "on";
    }
    if let Some(caps) = caps.filter(|c| !c.lens_zooms.is_empty()) {
        let centred = caps.lens_zooms.iter().copied().filter(|l| zoom >= *l).fold(1.0, f64::max);
        let tele_name = || if tele.is_empty() { tr("the telephoto") } else { lens_name(tele) };
        if centred > sent_ratio + 1e-3 {
            let live_name = if live.is_empty() { tr("the main camera") } else { lens_name(live) };
            lines.push(tr("Panned past what {tele} can see, so it's on {live}. Pan back to switch again.").field("tele", tele_name()).field("live", &live_name));
            level = "off";
        } else if sent_ratio > 1.0 + 1e-3 && settled && !live.is_empty() && live == default {
            lines.push(tr("The phone stayed on {live} by itself, most likely too dark or too close for {tele}. It switches once it can.")
                .field("live", lens_name(live)).field("tele", tele_name()));
            level = "off";
        }
    }
    if !lines.is_empty() {
        lines.push(tr("It can wobble for a moment while it switches."));
    }
    (level, lines.join("\n"))
}

pub struct TransformsPlugin {
    flip_h: bool,
    flip_v: bool,
    rotation: Option<i32>,
    zoom: f64,
    pan_x: f64,
    pan_y: f64,
    stretch_x: f64,
    stretch_y: f64,
    /// The sliders' own values, which the settings are read back from: zoom in hundredths, pan in 200ths, stretch in percent.
    zoom_steps: i32,
    max_zoom_steps: i32,
    pan_steps: [i32; 2],
    stretch_steps: [i32; 2],
    status: ViewStatus,
    /// What moves zoom and pan instead of this card ("" = nobody).
    taken_by: String,
    /// The tooltip's first line: where the zoom happens (from the last split).
    zoom_where: String,
    /// The lens a multi-lens camera streams from right now, as the lens picker names it.
    live_lens: String,
    /// The lens it streams from unzoomed (seen live while the ratio was 1).
    default_lens: String,
    /// The last longer lens seen live, to name it once it's out of view.
    tele_lens: String,
    snaps: Vec<i32>,
    /// A stream's url while one is open (what ctrl was).
    stream: Option<String>,
    /// StreamInfo::session of that stream.
    session: Option<u64>,
    behind: StreamsBehind<(String, String, String)>,
    /// A preview drag's pan before the centre snap (see view_dragged).
    raw_pan: Option<(f64, f64)>,
    /// The scroll wheel's zoom, finer than the slider's 0.01 steps (trackpads).
    wheel_zoom: f64,
    paired: Paired,
    /// set_config has no host: the window hears the settings with the next hook that has one.
    pending_view: bool,
    held: [Option<bool>; 2],
}

impl Default for TransformsPlugin {
    fn default() -> Self {
        TransformsPlugin::new()
    }
}

impl TransformsPlugin {
    pub fn new() -> TransformsPlugin {
        TransformsPlugin {
            flip_h: false,
            flip_v: false,
            rotation: None,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            stretch_x: 1.0,
            stretch_y: 1.0,
            zoom_steps: 100,
            max_zoom_steps: DEFAULT_MAX_ZOOM * 100,
            pan_steps: [0, 0],
            stretch_steps: [100, 100],
            status: ViewStatus::default(),
            taken_by: String::new(),
            zoom_where: String::new(),
            live_lens: String::new(),
            default_lens: String::new(),
            tele_lens: String::new(),
            snaps: Vec::new(),
            stream: None,
            session: None,
            behind: StreamsBehind::default(),
            raw_pan: None,
            wheel_zoom: 1.0,
            paired: Paired::default(),
            pending_view: false,
            held: [None; 2],
        }
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    pub fn pan(&self) -> (f64, f64) {
        (self.pan_x, self.pan_y)
    }

    pub fn stretch(&self) -> (f64, f64) {
        (self.stretch_x, self.stretch_y)
    }

    pub fn rotation(&self) -> Option<i32> {
        self.rotation
    }

    pub fn flips(&self) -> (bool, bool) {
        (self.flip_h, self.flip_v)
    }

    fn geometry(&self) -> Geometry {
        Geometry { zoom: self.zoom, pan_x: self.pan_x, pan_y: self.pan_y, flip_h: self.flip_h, flip_v: self.flip_v, rotation: self.rotation,
                   stretch_x: self.stretch_x, stretch_y: self.stretch_y }
    }

    fn free(&self) -> bool {
        self.taken_by.is_empty()
    }

    fn pan_active(&self) -> bool {
        self.free() && self.zoom > 1.0
    }

    // ── What goes to the window ───────────────────────────────────────

    fn manual_view(&self) -> ViewChange {
        ViewChange {
            zoom: Some(self.zoom as f32),
            pan_x: Some(self.pan_x as f32),
            pan_y: Some(self.pan_y as f32),
            flip_h: Some(self.flip_h),
            flip_v: Some(self.flip_v),
            rotation: Some(self.rotation),
            stretch_x: Some(self.stretch_x as f32),
            stretch_y: Some(self.stretch_y as f32),
        }
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending_view {
            self.pending_view = false;
            // Unchanged widgets don't fire, so the window's view hears all of it here, once
            host.set_view(self.manual_view());
            self.show_all(host);
        }
    }

    // ── What the card shows ───────────────────────────────────────────

    fn zoom_text(&self) -> String {
        tr("{n}\u{d7}").field("n", number(self.zoom, 1, false))
    }

    fn pan_label(pan: f64) -> String {
        let pct = (pan * 100.0).round_ties_even();
        if pct != 0.0 { percent(pct, 0, true) } else { percent(0.0, 0, false) }
    }

    fn zoom_where_text(&self) -> String {
        let mut text = self.zoom_where.clone();
        if !self.snaps.is_empty() {
            text += &format!("\n{}", tr("Dots mark where the phone switches to another lens"));
        }
        if !self.taken_by.is_empty() {
            text = tr("{owner} is moving the zoom and pan").field("owner", tr(&self.taken_by));
        }
        text.trim_start_matches('\n').to_string()
    }

    fn lens(&self) -> (&'static str, String) {
        let streaming = self.stream.is_some() && self.status.streaming;
        let settled = now_s() - self.status.sent_ratio_at >= LENS_SETTLE_S;
        lens_note(self.zoom, self.status.sent.map_or(1.0, |s| s.ratio), self.status.caps.as_ref().filter(|_| streaming),
                  if streaming { &self.live_lens } else { "" }, &self.default_lens, &self.tele_lens, settled)
    }

    fn zoom_props(&self) -> Value {
        let (level, note) = self.lens();
        json!({
            "zoom": self.zoom_steps as f64 / 100.0,
            "zoom-max": self.max_zoom_steps as f64 / 100.0,
            "zoom-marks": self.snaps.iter().map(|s| *s as f64 / 100.0).collect::<Vec<_>>(),
            "zoom-free": self.free(),
            "zoom-text": self.zoom_text(),
            "zoom-tip": self.zoom_where_text(),
            "dot-level": level,
            "dot-tip": note,
        })
    }

    fn pan_props(&self) -> Value {
        json!({
            "pan-x": self.pan_x,
            "pan-y": self.pan_y,
            "pan-enabled": self.pan_active(),
        })
    }

    /// The map's shape follows the frame the window shows.
    fn show_map_aspect(&self, host: &mut dyn Host) {
        let aspect = self.map_aspect(host);
        self.show(host, &json!({"map-aspect": aspect}));
    }

    /// The map's shape: width over height of the phone's frame, as the window shows it.
    fn map_aspect(&self, host: &dyn Host) -> f64 {
        let shown = host.view_aspect() as f64;
        if shown <= 0.0 {
            return 16.0 / 9.0;
        }
        if matches!(self.rotation, Some(90 | 270)) { 1.0 / shown } else { shown }
    }

    fn stretch_props(&self) -> Value {
        json!({
            "stretch-x": self.stretch_steps[0],
            "stretch-y": self.stretch_steps[1],
            "stretch-x-text": percent(self.stretch_steps[0] as f64, 0, false),
            "stretch-y-text": percent(self.stretch_steps[1] as f64, 0, false),
        })
    }

    fn rotation_index(&self) -> usize {
        ROTATIONS.iter().position(|(_, r)| *r == self.rotation).unwrap_or(0)
    }

    fn flip_props(&self) -> Value {
        json!({"flips": [self.flip_h, self.flip_v], "rotation": self.rotation_index()})
    }

    fn fixed_props() -> Value {
        let add = |action: &str| tr("Add shortcut for {action}…").field("action", tr(action));
        json!({"reset-help": tr(RESET_TIP), "pan-tip": tr(PAN_TIP), "pan-add-x": add("Pan left/right"), "pan-add-y": add("Pan up/down"), "reset-hint": tr(RESET_HINT),
               "stretch-tips": STRETCH_TIPS.iter().map(|t| tr(t)).collect::<Vec<_>>()})
    }

    fn all_props(&self) -> Value {
        let mut all = Map::new();
        for part in [self.flip_props(), self.zoom_props(), self.pan_props(), self.stretch_props()] {
            if let Value::Object(m) = part {
                all.extend(m);
            }
        }
        all.insert("active".into(), json!(self.paired.on()));
        Value::Object(all)
    }

    fn show(&self, host: &mut dyn Host, props: &Value) {
        host.set_ui_props("card", props);
    }

    fn show_all(&self, host: &mut dyn Host) {
        self.show(host, &self.all_props());
        self.show_map_aspect(host);
    }

    fn show_zoom_where(&self, host: &mut dyn Host) {
        self.show(host, &self.zoom_props());
    }

    // ── Handlers ──────────────────────────────────────────────────────

    fn on_flip(&mut self, host: &mut dyn Host, which: usize, on: bool) {
        if which == 0 { self.flip_h = on } else { self.flip_v = on }
        host.set_view(ViewChange { flip_h: Some(self.flip_h), flip_v: Some(self.flip_v), ..Default::default() });
        host.schedule_save();
        self.show(host, &self.flip_props());
    }

    fn on_rotate(&mut self, host: &mut dyn Host, index: usize) {
        self.rotation = ROTATIONS.get(index).and_then(|(_, r)| *r);
        host.set_view(ViewChange { rotation: Some(self.rotation), ..Default::default() });
        host.schedule_save();
        self.show(host, &self.flip_props());
    }

    /// The zoom slider moved to this many hundredths (the slider holds it between 1x and Max zoom).
    fn on_zoom_changed(&mut self, host: &mut dyn Host, steps: i32) {
        let steps = steps.clamp(100, self.max_zoom_steps.max(100));
        self.zoom_steps = steps;
        self.zoom = steps as f64 / 100.0;
        if self.zoom <= 1.0 {
            self.pan_x = 0.0;
            self.pan_y = 0.0;
        }
        self.framing_changed(host, false);
    }

    fn on_stretch_changed(&mut self, host: &mut dyn Host) {
        self.stretch_x = self.stretch_steps[0] as f64 / 100.0;
        self.stretch_y = self.stretch_steps[1] as f64 / 100.0;
        host.set_view(ViewChange { stretch_x: Some(self.stretch_x as f32), stretch_y: Some(self.stretch_y as f32), ..Default::default() });
        host.schedule_save();
        self.show(host, &self.stretch_props());
    }

    /// The pan sliders' values, as the card reads them back (the pad hands it values and the sliders quantise).
    fn on_pan_changed(&mut self, host: &mut dyn Host) {
        self.pan_x = self.pan_steps[0] as f64 / PAN_STEPS;
        self.pan_y = self.pan_steps[1] as f64 / PAN_STEPS;
        self.framing_changed(host, false);
    }

    /// The sliders follow a pan: whole steps towards zero, as int() does.
    fn follow_pan(&mut self) {
        self.pan_steps = [(self.pan_x * PAN_STEPS).trunc() as i32, (self.pan_y * PAN_STEPS).trunc() as i32];
    }

    fn on_pan_pad(&mut self, host: &mut dyn Host, x: f64, y: f64) {
        let (x, y) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0));
        self.pan_steps = [(x * PAN_STEPS).trunc() as i32, (y * PAN_STEPS).trunc() as i32];
        self.on_pan_changed(host);
    }

    /// The map's crop was moved so its middle is at (cx, cy) of the frame: the pan that puts the window there.
    fn on_map_moved(&mut self, host: &mut dyn Host, cx: f64, cy: f64) {
        self.on_pan_pad(host, pan_for(cx, self.zoom), pan_for(cy, self.zoom));
    }

    /// Zoom or pan changed: the sliders follow and the window's view moves (the phone hears, the lens boxes move).
    fn framing_changed(&mut self, host: &mut dyn Host, keep_raw: bool) {
        if !keep_raw {
            // moved some other way: a drag starts from where the picture is
            self.raw_pan = None;
        }
        self.follow_pan();
        host.set_view(ViewChange { zoom: Some(self.zoom as f32), pan_x: Some(self.pan_x as f32), pan_y: Some(self.pan_y as f32), ..Default::default() });
        self.show_all_framing(host);
        host.schedule_save();
    }

    fn show_all_framing(&mut self, host: &mut dyn Host) {
        self.show(host, &self.zoom_props());
        self.show(host, &self.pan_props());
    }

    fn reset_all(&mut self, host: &mut dyn Host) {
        self.flip_h = false;
        self.flip_v = false;
        self.rotation = None;
        self.zoom_steps = 100;
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
        self.pan_steps = [0, 0];
        self.stretch_steps = [100, 100];
        self.stretch_x = 1.0;
        self.stretch_y = 1.0;
        self.raw_pan = None;
        host.set_view(self.manual_view());
        host.schedule_save();
        self.show_all(host);
    }

    /// The preview was dragged by (du, dv) of the frame as shown: the picture follows the mouse.
    fn view_dragged(&mut self, host: &mut dyn Host, du: f64, dv: f64) {
        if self.zoom <= 1.0 || !self.taken_by.is_empty() {
            return;
        }
        // Where the drag alone would put the pan; what's shown sticks at the centre near it (PAN_SNAP), and dragging on
        // past that lets go without a jump, since this keeps counting underneath.
        let (raw_x, raw_y) = self.raw_pan.unwrap_or((self.pan_x, self.pan_y));
        let g = Geometry { pan_x: raw_x, pan_y: raw_y, ..self.geometry() };
        let (x0, y0) = shown_to_view(0.5, 0.5, &g);
        let (x1, y1) = shown_to_view(0.5 + du, 0.5 + dv, &g);
        let raw_x = pan_for(window_centre(self.zoom, raw_x) - (x1 - x0), self.zoom);
        let raw_y = pan_for(window_centre(self.zoom, raw_y) - (y1 - y0), self.zoom);
        // a pan of 1, in frames as shown
        let off_centre = (self.zoom - 1.0) / 2.0;
        self.pan_x = if raw_x.abs() * off_centre <= PAN_SNAP { 0.0 } else { raw_x };
        self.pan_y = if raw_y.abs() * off_centre <= PAN_SNAP { 0.0 } else { raw_y };
        self.framing_changed(host, true);
        self.raw_pan = Some((raw_x, raw_y));
    }

    /// Zoom by `factor`, keeping the point under the mouse ((u, v) of the frame as shown) where it is.
    fn view_scrolled(&mut self, host: &mut dyn Host, factor: f64, u: f64, v: f64) {
        if !self.taken_by.is_empty() {
            return;
        }
        let old = self.zoom_steps;
        if (self.wheel_zoom * 100.0 - old as f64).abs() >= 1.0 {
            // moved some other way since the last scroll
            self.wheel_zoom = old as f64 / 100.0;
        }
        let target = (self.wheel_zoom * factor).max(1.0).min(self.max_zoom_steps as f64 / 100.0);
        let rounded = (target * 100.0).round_ties_even() as i32;
        let new = zoom_detent(old, rounded, &self.snaps);
        self.wheel_zoom = if new != rounded { new as f64 / 100.0 } else { target };
        if new == old {
            return;
        }
        // Where the point sits inside the window stays the same, so it stays under the mouse.
        let (px, py) = shown_to_view(u, v, &self.geometry());
        let ((x0, w), (y0, h)) = (window(self.zoom, self.pan_x), window(self.zoom, self.pan_y));
        let zoom = new as f64 / 100.0;
        let size = 1.0 / zoom;
        self.pan_x = pan_for(px - (px - x0) / w * size + size / 2.0, zoom);
        self.pan_y = pan_for(py - (py - y0) / h * size + size / 2.0, zoom);
        // the zoom path does the rest, with the pan set above
        self.on_zoom_changed(host, new);
    }

    fn view_status(&mut self, host: &mut dyn Host, status: &Value) {
        let status: ViewStatus = serde_json::from_value(status.clone()).unwrap_or_default();
        let old = std::mem::replace(&mut self.status, status.clone());
        if status.caps != old.caps {
            if status.caps.is_some() {
                // another camera: its lenses are learnt afresh
                self.default_lens.clear();
                self.tele_lens.clear();
            }
            // Dragging sticks to them: a lens's own ratio is the sharpest view it has.
            self.snaps = status.caps.as_ref().map(|c| c.lens_zooms.iter().map(|z| lens_step(*z)).collect()).unwrap_or_default();
        }
        if status.sent_ratio_at != old.sent_ratio_at {
            // Checks the dot again once a new ratio had time to take, in case no phone state comes in between
            host.schedule(((LENS_SETTLE_S * 1000.0) as u64) + 100, T_SETTLE);
        }
        self.zoom_where = if status.sent.is_none() {
            if status.streaming { tr("Zoomed on this computer: this lens can't zoom itself") } else { tr("Zoomed on this computer") }
        } else if status.desktop_zoom > 1.0 {
            tr("Zoomed on the phone's sensor as far as this lens allows, the rest on this computer")
        } else {
            tr("Zoomed on the phone's sensor")
        };
        self.show_zoom_where(host);
    }

    fn view_taken(&mut self, host: &mut dyn Host, owner: &str) {
        self.taken_by = owner.to_string();
        self.show(host, &self.pan_props());
        self.show_zoom_where(host);
    }

    // ── Which lens the phone is on ────────────────────────────────────

    fn leave_stream(&mut self, host: &mut dyn Host) {
        self.stream = None;
        self.session = None;
        self.live_lens.clear();
        self.show_map_aspect(host);
        self.show_zoom_where(host);
    }
}

impl Plugin for TransformsPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Transforms").into(),
            about: n_("Zoom, pan, flip, rotate and stretch the picture.").into(),
            when_off: n_("Without it there's no Transforms card and the picture goes out exactly as the phone sends it.").into(),
            category: "camera".into(),
            icon: "crop".into(),
            pinned: true,
            in_presets: true,
            device_local: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.show(host, &TransformsPlugin::fixed_props());
        self.show_all(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            self.show(host, &json!({"active": on}));
        }
        match event {
            Event::MaxZoomChanged { max_zoom } => {
                // a zoom past it is clamped, through the zoom handler
                self.max_zoom_steps = max_zoom * 100;
                // the Zoom action's range moves with it
                host.refresh_actions();
                if self.zoom_steps > self.max_zoom_steps {
                    self.on_zoom_changed(host, self.max_zoom_steps);
                } else {
                    self.show(host, &self.zoom_props());
                }
            }
            Event::ViewDragged { du, dv } => self.view_dragged(host, *du as f64, *dv as f64),
            Event::ViewScrolled { factor, u, v } => self.view_scrolled(host, *factor as f64, *u as f64, *v as f64),
            Event::ViewStatus { status } => self.view_status(host, status),
            Event::ViewTaken { owner } => self.view_taken(host, owner),
            Event::TimerFired { owner, token } if owner == NAME && token == T_SETTLE => {
                self.show_zoom_where(host);
            }
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        let mut props = TransformsPlugin::fixed_props();
        if let (Value::Object(p), Value::Object(all)) = (&mut props, self.all_props()) {
            p.extend(all);
        }
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "TransformsCard".into(), props }, region: "left".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        let b = |i: usize| event.args.get(i).and_then(Value::as_bool).unwrap_or(false);
        match event.callback.as_str() {
            "reset-clicked" => self.reset_all(host),
            "flip-toggled" => self.on_flip(host, n(0) as usize, b(1)),
            "rotation-picked" => self.on_rotate(host, n(0) as usize),
            "zoom-moved" => {
                if self.free() {
                    self.on_zoom_changed(host, (n(0) * 100.0).round() as i32);
                }
            }
            "map-moved" => {
                if self.pan_active() {
                    self.on_map_moved(host, n(0), n(1));
                }
            }
            "stretch-moved" => {
                let which = (n(0) as usize).min(1);
                self.stretch_steps[which] = (n(1).round() as i32).clamp(STRETCH_RANGE.0, STRETCH_RANGE.1);
                self.on_stretch_changed(host);
            }
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Transforms");
        let search = |words: &str| tr_ctx("search", words);
        let rotation_options: Vec<(&str, String)> = ROTATION_SEGMENTS.iter().map(|(v, shown)| (*v, shown.to_string())).collect();
        let zoom_label = tr("Zoom");
        let pan_x_label = tr("Pan left/right");
        let pan_y_label = tr("Pan up/down");
        let width_label = tr("Width");
        let height_label = tr("Height");
        let max = self.max_zoom_steps as f64 / 100.0;
        let slider = |id: &str, label: &str, keywords: &str, min: f64, max: f64, step: f64, decimals: usize, suffix: &str, current: f64| {
            let spec = SliderSpec { id, label, group: &group, keywords, minimum: min, maximum: max, step, decimals, suffix, has_default: true };
            actions::slider_action(&spec, current)
        };
        let mut out = vec![
            actions::toggle_action("transforms.flip_horizontal", &tr("Flip horizontal"), &group, &search("mirror"), &[]),
            actions::toggle_action("transforms.flip_vertical", &tr("Flip vertical"), &group, &search("mirror upside down"), &[]),
            actions::choice_action("transforms.rotation", &tr("Rotation"), &group, &search("rotate turn"), &rotation_options),
            slider("transforms.zoom", &zoom_label, &search("crop framing"), 1.0, max, 0.25, 2, "\u{d7}", self.zoom_steps as f64 / 100.0),
            slider("transforms.pan_x", &pan_x_label, &search("framing move"), -100.0, 100.0, 10.0, 0, "%", self.pan_steps[0] as f64 * 0.5),
            slider("transforms.pan_y", &pan_y_label, &search("framing move"), -100.0, 100.0, 10.0, 0, "%", self.pan_steps[1] as f64 * 0.5),
            slider("transforms.stretch_x", &width_label, &search("stretch squish aspect wide"), 50.0, 200.0, 10.0, 0, "%", self.stretch_steps[0] as f64),
            slider("transforms.stretch_y", &height_label, &search("stretch squish aspect tall"), 50.0, 200.0, 10.0, 0, "%", self.stretch_steps[1] as f64),
        ];
        out.push(ActionSpec { id: "transforms.reset".into(), label: tr("Reset transforms"), group, params: Vec::new(), repeats: false, on_release: false, current: None, keywords: String::new() });
        out
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        if !self.paired.on() {
            return None;
        }
        match id {
            "transforms.flip_horizontal" | "transforms.flip_vertical" => {
                let (which, label) = if id.ends_with("horizontal") { (0, tr("Flip horizontal")) } else { (1, tr("Flip vertical")) };
                let is_on = if which == 0 { self.flip_h } else { self.flip_v };
                let want = actions::toggle_wanted(params, is_on, &mut self.held[which]);
                if want != is_on {
                    self.on_flip(host, which, want);
                }
                Some(actions::toggle_text(&label, want))
            }
            "transforms.rotation" => {
                let options: Vec<Choice> = ROTATION_SEGMENTS.iter().map(|(v, shown)| Choice { value: v, shown, enabled: true }).collect();
                let (said, pick) = actions::choice_target(params, &options, Some(self.rotation_index()), true)?;
                if pick {
                    self.on_rotate(host, said);
                }
                Some(actions::slider_text(&tr("Rotation"), ROTATION_SEGMENTS[said].1))
            }
            "transforms.zoom" => {
                if !self.free() {
                    return None;
                }
                let target = actions::slider_target(params, self.zoom_steps as i64, 0.01, 100, self.max_zoom_steps as i64, Some(100))?;
                self.on_zoom_changed(host, target as i32);
                Some(actions::slider_text(&tr("Zoom"), &self.zoom_text()))
            }
            "transforms.pan_x" | "transforms.pan_y" => {
                if !self.pan_active() {
                    return None;
                }
                let which = usize::from(id.ends_with("y"));
                let target = actions::slider_target(params, self.pan_steps[which] as i64, 0.5, -200, 200, Some(0))?;
                self.pan_steps[which] = target as i32;
                self.on_pan_changed(host);
                let (label, pan) = if which == 0 { (tr("Pan left/right"), self.pan_x) } else { (tr("Pan up/down"), self.pan_y) };
                Some(actions::slider_text(&label, &Self::pan_label(pan)))
            }
            "transforms.stretch_x" | "transforms.stretch_y" => {
                let which = usize::from(id.ends_with("y"));
                let target = actions::slider_target(params, self.stretch_steps[which] as i64, 1.0, STRETCH_RANGE.0 as i64, STRETCH_RANGE.1 as i64, Some(100))?;
                self.stretch_steps[which] = target as i32;
                self.on_stretch_changed(host);
                let label = if which == 0 { tr("Width") } else { tr("Height") };
                Some(actions::slider_text(&label, &percent(target as f64, 0, false)))
            }
            "transforms.reset" => {
                self.reset_all(host);
                Some(tr("Reset transforms"))
            }
            _ => None,
        }
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        let which = match id {
            "transforms.flip_horizontal" => 0,
            "transforms.flip_vertical" => 1,
            _ => return,
        };
        if let Some(before) = actions::hold_released(params, &mut self.held[which]) {
            let is_on = if which == 0 { self.flip_h } else { self.flip_v };
            if before != is_on && self.paired.on() {
                self.on_flip(host, which, before);
            }
        }
    }

    fn on_profile_applied(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.stream = Some(stream.url.clone());
        self.session = Some(stream.session);
        self.show_map_aspect(host);
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.stream = Some(stream.url.clone());
        self.session = Some(stream.session);
        self.show_map_aspect(host);
        if let Some((live, default, tele)) = self.behind.take(host, stream.session) {
            self.live_lens = live;
            self.default_lens = default;
            self.tele_lens = tele;
        }
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.behind.drop_shown(host);
        self.leave_stream(host);
    }

    fn on_focus_lost(&mut self, host: &mut dyn Host) {
        let kept = (self.live_lens.clone(), self.default_lens.clone(), self.tele_lens.clone());
        self.behind.keep(host, self.session, kept);
        self.leave_stream(host);
    }

    fn on_phone_state(&mut self, host: &mut dyn Host, state: &Value) {
        let cameras = state.get("cameras").and_then(Value::as_array).cloned().unwrap_or_default();
        let active = state.get("active_lens");
        let live = cameras.iter().find(|c| c.get("id") == active);
        self.live_lens = live.map(lens_label).unwrap_or_default();
        // Learnt only once the ratio sent last had time to take: right after a sweep back under a lens, or on a reconnect,
        // the phone can still be on the telephoto, and taking that for the unzoomed lens made the dot red.
        let settled = now_s() - self.status.sent_ratio_at >= LENS_SETTLE_S;
        if let (false, Some(sent), true) = (self.live_lens.is_empty(), self.status.sent, settled) {
            if sent.ratio <= 1.0 {
                self.default_lens = self.live_lens.clone();
            } else if !self.default_lens.is_empty() && self.live_lens != self.default_lens {
                self.tele_lens = self.live_lens.clone();
            }
        }
        self.show_zoom_where(host);
    }

    fn settings(&self) -> Vec<SettingSpec> {
        let turns: Vec<(&str, &str)> = view::ROTATIONS.iter().map(|(name, _)| (*name, *name)).collect();
        vec![
            SettingSpec::switch("flip_h", "Mirrors the picture left to right", false),
            SettingSpec::switch("flip_v", "Turns the picture upside down by mirroring top to bottom", false),
            SettingSpec::choice("rotation", "Turns the picture a quarter or half turn", &turns, "None"),
            SettingSpec::number("zoom", "How far in the view is (the phone's own zoom may hold it lower)", 1.0, 30.0, 2, 1.0).unit("times"),
            SettingSpec::number("pan_x", "Moves the zoomed view left (-1) or right (1)", -1.0, 1.0, 3, 0.0),
            SettingSpec::number("pan_y", "Moves the zoomed view up (-1) or down (1)", -1.0, 1.0, 3, 0.0),
            SettingSpec::number("stretch_x", "Makes the picture wider or narrower", 0.5, 2.0, 2, 1.0).unit("1 is unchanged"),
            SettingSpec::number("stretch_y", "Makes the picture taller or shorter", 0.5, 2.0, 2, 1.0).unit("1 is unchanged"),
        ]
    }

    fn get_config(&self) -> Value {
        json!({
            "flip_h": self.flip_h,
            "flip_v": self.flip_v,
            "rotation": view::rotation_name(self.rotation),
            "zoom": self.zoom_steps as f64 / 100.0,
            // finer than the sliders: dragging the preview sets it directly
            "pan_x": self.pan_x,
            "pan_y": self.pan_y,
            "stretch_x": self.stretch_steps[0] as f64 / 100.0,
            "stretch_y": self.stretch_steps[1] as f64 / 100.0,
        })
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let truthy = |k: &str| match cfg.get(k) {
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64() != Some(0.0),
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Object(o)) => !o.is_empty(),
            _ => false,
        };
        self.flip_h = truthy("flip_h");
        self.flip_v = truthy("flip_v");
        if let Some(rot) = cfg.get("rotation").and_then(Value::as_str).filter(|r| !r.is_empty()) {
            // a name that isn't one of the choices leaves the rotation as it is
            if let Some(r) = view::rotation_named(rot) {
                self.rotation = r;
            }
        }
        // the slider holds it to Max zoom
        let zoom = number_of(cfg.get("zoom"), 1.0, 1.0, 30.0);
        self.zoom_steps = ((zoom * 100.0).round_ties_even() as i32).clamp(100, self.max_zoom_steps.max(100));
        self.zoom = self.zoom_steps as f64 / 100.0;
        let pan_active = zoom > 1.0;
        self.pan_x = if pan_active { number_of(cfg.get("pan_x"), 0.0, -1.0, 1.0) } else { 0.0 };
        self.pan_y = if pan_active { number_of(cfg.get("pan_y"), 0.0, -1.0, 1.0) } else { 0.0 };
        if self.zoom <= 1.0 {
            self.pan_x = 0.0;
            self.pan_y = 0.0;
        }
        self.raw_pan = None;
        self.follow_pan();
        let (lo, hi) = (STRETCH_RANGE.0 as f64 / 100.0, STRETCH_RANGE.1 as f64 / 100.0);
        for (i, key) in ["stretch_x", "stretch_y"].iter().enumerate() {
            self.stretch_steps[i] = (number_in(cfg.get(key), 1.0, lo, hi) * 100.0).round_ties_even() as i32;
        }
        self.stretch_x = self.stretch_steps[0] as f64 / 100.0;
        self.stretch_y = self.stretch_steps[1] as f64 / 100.0;
        self.pending_view = true;
        Ok(())
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.set_config(cfg)?;
        self.catch_up(host);
        Ok(())
    }
}

#[cfg(test)]
#[path = "transforms_tests.rs"]
mod tests;
