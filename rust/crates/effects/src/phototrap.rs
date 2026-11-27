//! The Phototrap plugin: zones drawn on the preview are watched for movement while it's armed, and each time something moves in one
//! the phone takes a short burst of photos. Nothing is saved until something sets it off, and only photos, never video.

use crate::stills::{build_options, default_folder, defaults, focus_now, offered, add_shot, move_shot_focus, pick_shot, shot_focus_marks, shot_choice, shot_control_props, ShotChoice, StillCaps};
use crate::shared::Paired;
use serde_json::{json, Value};
use telescope_core::event::Event;
use telescope_core::host::{Host, PickKind, PreviewLayer, RequestId, Shape};
use telescope_core::i18n::{n_, tr, trn, Fields};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::settings::{off_default, reset_config};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use telescope_core::host::{Pick, View};
use telescope_core::system::{file_url, free_space};
use telescope_core::time::{local_clock, monotonic_ns};
use telescope_core::ui::{CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::view::{shown_to_view, view_to_shown, Geometry};
use telescope_core::vision::{turned, Turn};

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
/// The most the Phototrap folder may hold before it disarms itself.
pub const STORAGE_BUDGET: u64 = 2 << 30;
/// Less than this free on the volume the photos go to disarms it.
pub const MIN_FREE: u64 = 1 << 30;
/// Entries looked at when adding up what the folder holds, so a folder someone filled with other things can't hold the card up.
const SCAN_LIMIT: usize = 100_000;
/// A burst nobody has heard the end of this long after its stream stopped is given up on, in seconds.
const FORGET_AFTER_S: f64 = 60.0;

const SWITCH_TIP: &str = n_("Watch the zones and take photos when something moves in them.");
const DRAW_TIP: &str = n_("Drag across the preview to mark a place to watch. Up to four.");
const DRAW_OFF_TIP: &str = n_("Switch on the Preview plugin to draw zones on the preview.");
const DRAW_ELSEWHERE_TIP: &str = n_("Phototrap is armed on another phone. Switch to that one to draw zones.");
const CLEAR_TIP: &str = n_("Remove every zone. With none, the whole picture is watched.");
const NIGHT_TIP: &str = n_("Use the phone maker's Night mode for the photos.");
const NIGHT_OFF_TIP: &str = n_("This lens has no Night mode to offer");
const COUNT_TIP: &str = n_("Number of photos each time it goes off.");
const COUNT_OFF_TIP: &str = n_("This lens takes one photo at a time");

/// The view's flips and rotation, which turn the picture the detector sees.
fn turn_of(view: &View) -> Turn {
    Turn { flip_h: view.flip_h, flip_v: view.flip_v, rotation: view.rotation }
}

/// The picture as shown at zoom 1 with this turn, as a view with nothing else changed.
fn flat(turn: Turn) -> Geometry {
    Geometry { zoom: 1.0, pan_x: 0.0, pan_y: 0.0, flip_h: turn.flip_h, flip_v: turn.flip_v, rotation: turn.rotation, stretch_x: 1.0, stretch_y: 1.0, distortion: 0.0 }
}

/// A zone (x0, y0, x1, y1) with its two corners put through `map`, kept to the picture and with its corners in order.
fn map_zone(z: [f32; 4], map: impl Fn(f64, f64) -> (f64, f64)) -> [f32; 4] {
    let (a, b) = (map(f64::from(z[0]), f64::from(z[1])), map(f64::from(z[2]), f64::from(z[3])));
    [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)].map(|v| v.clamp(0.0, 1.0) as f32)
}

/// Bytes in a folder and the ones inside it, 0 for one that isn't there, and the files that made them when `keep` asks for them. A download
/// under way isn't a photo yet: its partial file and the empty one reserved for its name are left out.
fn folder_scan(folder: &Path, keep: bool) -> (u64, HashSet<PathBuf>) {
    let (mut total, mut seen, mut todo, mut files) = (0u64, 0usize, vec![folder.to_path_buf()], HashSet::new());
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > SCAN_LIMIT {
                return (total, files);
            }
            match entry.metadata() {
                Ok(m) if m.is_dir() => todo.push(entry.path()),
                Ok(m) if m.len() == 0 || entry.file_name().to_string_lossy().ends_with(".part") => {}
                Ok(m) => {
                    total += m.len();
                    if keep {
                        files.insert(entry.path());
                    }
                }
                Err(_) => {}
            }
        }
    }
    (total, files)
}

/// Gigabytes the way a limit is told: "2", or "0.5".
fn gigabytes(bytes: u64) -> String {
    let gb = bytes as f64 / f64::from(1u32 << 30);
    if (gb - gb.round()).abs() < 0.05 { format!("{}", gb.round()) } else { format!("{gb:.1}") }
}

/// What an armed trap runs on: the settings of the phone it watches.
#[derive(Clone)]
struct Settings {
    zones: Vec<[f32; 4]>,
    sensitivity: u32,
    count: u32,
    pause_s: u32,
    night: bool,
    use_zoom: bool,
    shot: ShotChoice,
    orientation: Turn,
}

pub struct PhototrapPlugin {
    armed: bool,
    /// The settings the armed trap runs on, kept while the card shows another phone, whose own settings are loaded then.
    kept: Option<Settings>,
    /// In the picture as the detector sees it: 0..1 of the whole picture at zoom 1, turned as the view is.
    zones: Vec<[f32; 4]>,
    sensitivity: u32,
    count: u32,
    pause_s: u32,
    night: bool,
    use_zoom: bool,
    shot: ShotChoice,
    customize_open: bool,
    paired: Paired,
    /// The zones and sensitivity the watch on the host runs with, None while there's none.
    watch: Option<(Vec<[f32; 4]>, u32)>,
    /// The stream it watches and takes photos on: the one the panels showed when it was armed. None until there is one.
    source: Option<String>,
    /// How the view was turned (flips and rotation) when the zones were drawn, which is how the detector sees the picture.
    orientation: Turn,
    /// The zones something moves in now, as the host last said.
    moving: Vec<u32>,
    /// Bytes the folder holds: counted when armed, then grown with each burst saved.
    used: u64,
    /// The files the last rescan already counted in `used` while bursts were still pending, so their end doesn't count them again.
    counted: HashSet<PathBuf>,
    budget: u64,
    min_free: u64,
    free: Box<dyn Fn(&Path) -> Option<u64> + Send>,
    /// The stream ended and no start has been heard since.
    down: bool,
    window_shown: bool,
    caps: Option<StillCaps>,
    folder: String,
    triggers: u32,
    photos: u32,
    last: String,
    /// The bursts asked for that haven't finished, with when their stream stopped (on the clock) if it has.
    taking: Vec<(RequestId, Option<f64>)>,
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
            kept: None,
            zones: Vec::new(),
            sensitivity: DEFAULT_SENSITIVITY,
            count: DEFAULT_COUNT,
            pause_s: DEFAULT_PAUSE_S,
            night: false,
            use_zoom: false,
            shot: ShotChoice::default(),
            customize_open: false,
            paired: Paired::default(),
            watch: None,
            source: None,
            orientation: Turn::default(),
            moving: Vec::new(),
            used: 0,
            counted: HashSet::new(),
            budget: STORAGE_BUDGET,
            min_free: MIN_FREE,
            free: Box::new(free_space),
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

    fn settings(&self) -> Settings {
        Settings { zones: self.zones.clone(), sensitivity: self.sensitivity, count: self.count, pause_s: self.pause_s, night: self.night,
                   use_zoom: self.use_zoom, shot: self.shot.clone(), orientation: self.orientation }
    }

    /// Puts these settings in and gives back the ones that were there.
    fn swap(&mut self, with: Settings) -> Settings {
        let before = self.settings();
        let Settings { zones, sensitivity, count, pause_s, night, use_zoom, shot, orientation } = with;
        (self.zones, self.sensitivity, self.count, self.pause_s, self.night, self.use_zoom, self.shot, self.orientation) =
            (zones, sensitivity, count, pause_s, night, use_zoom, shot, orientation);
        before
    }

    /// Notes what the armed trap runs on while the card shows the phone it watches, and drops it once it's off.
    fn keep(&mut self, host: &dyn Host) {
        if !self.armed {
            self.kept = None;
        } else if self.on_armed_source(host) {
            self.kept = Some(self.settings());
        }
    }

    /// Runs `f` on what the armed trap runs on: the card's settings, or the ones kept for it while the card shows another phone's.
    fn running<R>(&mut self, host: &dyn Host, f: impl FnOnce(&mut Self) -> R) -> R {
        let elsewhere = self.armed && !self.on_armed_source(host);
        let Some(kept) = self.kept.clone().filter(|_| elsewhere) else { return f(self) };
        let card = self.swap(kept);
        let out = f(self);
        self.swap(card);
        out
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
        self.caps.as_ref().is_some_and(Self::night_in)
    }

    fn night_in(caps: &StillCaps) -> bool {
        let mut choices = defaults();
        choices.insert("mode".into(), json!("night"));
        offered(caps, &choices).modes.contains(&"night")
    }

    /// The stream it's about: the one it was armed on, else the one the panels show.
    fn target(&self, host: &dyn Host) -> Option<String> {
        self.source.clone().or_else(|| host.focused_source_id())
    }

    /// Whether the panels show the stream it's armed on, which is where zones can be drawn and seen. Always so while it isn't armed.
    fn on_armed_source(&self, host: &dyn Host) -> bool {
        match (&self.source, self.armed) {
            (Some(source), true) => host.focused_source_id().as_deref() == Some(source.as_str()),
            _ => true,
        }
    }

    /// What the phone on screen can do, which is what the card offers; the armed phone's own caps are read where it takes photos.
    fn read_caps(&self, host: &dyn Host) -> Option<StillCaps> {
        let id = host.focused_source_id()?;
        host.still_caps(Some(&id)).and_then(|v| serde_json::from_value(v).ok())
    }

    /// Whether its stream is running and can take photos.
    fn can_snap(&self, host: &dyn Host) -> bool {
        let Some(id) = self.target(host) else { return false };
        !self.down && host.is_streaming_from(Some(&id)) && host.still_caps(Some(&id)).is_some()
    }

    fn zones_to_watch(&self) -> Vec<[f32; 4]> {
        if self.zones.is_empty() { vec![WHOLE] } else { self.zones.clone() }
    }

    /// The photo options for the picks and what the lens offers.
    fn options(&self, caps: &StillCaps, host: &dyn Host) -> Value {
        let mut choices = defaults();
        choices.insert("use_zoom".into(), json!(self.use_zoom));
        let burst = caps.burst_max >= 2;
        choices.insert("shots".into(), json!(if burst { "burst" } else { "single" }));
        // the saved count, not count_now: that is capped by the camera on screen, and build_options caps it for this one
        choices.insert("burst".into(), json!(i64::from(self.count).max(2)));
        let night = self.night && Self::night_in(caps);
        if night {
            choices.insert("mode".into(), json!("night"));
        }
        let rotation = host.view().rotation.filter(|r| [90, 180, 270].contains(r)).unwrap_or(0);
        let mut options = build_options(caps, &choices, rotation);
        if caps.camera {
            if let Some(map) = options.as_object_mut() { map.remove("quality"); }
        }
        add_shot(&mut options, caps, &self.shot, true, night);
        options
    }

    // ── What the card shows ───────────────────────────────────────────

    fn status(&self, host: &dyn Host) -> (String, &'static str) {
        // The card tells about the phone on screen, and nothing is armed on that one
        if self.armed && !self.on_armed_source(host) {
            return (tr("Not armed"), "status_dim");
        }
        // A reason it disarmed itself stays up after it has
        if let Some(problem) = &self.problem {
            return (problem.clone(), "status_err");
        }
        if !self.armed {
            return (tr("Not armed"), "status_dim");
        }
        if !self.can_snap(host) {
            return (tr("Waiting for a phone that can take photos"), "status_warn");
        }
        if host.picture_hidden(self.target(host).as_deref()) {
            return (tr("Paused while the picture is hidden"), "status_warn");
        }
        let text = if self.zones.is_empty() {
            tr("Armed and watching the whole picture")
        } else {
            trn("Armed and watching {n} zone", "Armed and watching {n} zones", self.zones.len() as i64)
        };
        (text, "status_ok")
    }

    fn tally(&self, host: &dyn Host) -> String {
        if self.armed && !self.on_armed_source(host) {
            String::new()
        } else if self.triggers > 0 {
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
        let preview = host.has_plugin("preview");
        let here = self.on_armed_source(host);
        let mut props = json!({
            "active": self.paired.on(), "armed": self.armed && here, "status": status, "status-kind": kind, "tally": self.tally(host),
            "zone-count": self.zones.len(), "draw-enabled": self.zones.len() < MAX_ZONES && preview && here, "clear-enabled": !self.zones.is_empty(),
            "draw-tip": tr(if !preview { DRAW_OFF_TIP } else if !here { DRAW_ELSEWHERE_TIP } else { DRAW_TIP }),
            "folder-text": self.shown_folder(), "folder-tip": self.folder,
            "sensitivity": self.sensitivity, "sensitivity-text": self.sensitivity.to_string(),
            "count": self.count_now().max(2), "count-max": max.max(3), "count-text": self.count_now().to_string(), "count-enabled": max >= 2,
            "count-tip": tr(if max >= 2 { COUNT_TIP } else { COUNT_OFF_TIP }),
            "pause": self.pause_s, "pause-text": tr("{seconds} s").field("seconds", self.pause_s),
            "night": self.night, "night-enabled": night_offered, "use-zoom": self.use_zoom,
            "night-tip": tr(if night_offered { NIGHT_TIP } else { NIGHT_OFF_TIP }),
            "reset-shown": self.reset_shown(), "customize-open": self.customize_open,
            "shot-focus-marks": shot_focus_marks(self.caps.as_ref(), focus_now(host)),
        });
        if let (Some(all), Value::Object(more)) = (props.as_object_mut(), shot_control_props(self.caps.as_ref(), &self.shot, true, self.night && night_offered)) {
            all.extend(more);
        }
        props
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

    /// The folder as the card says it, with the home folder as ~ (nothing personal on a screenshot).
    fn shown_folder(&self) -> String {
        let path = Path::new(&self.folder);
        match std::env::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
            Some(rest) => Path::new("~").join(rest).display().to_string(),
            None => self.folder.clone(),
        }
    }

    /// The zones drawn back onto the preview: from where the detector has them to the picture as the view shows it now.
    fn layer(&self, view: &View) -> PreviewLayer {
        let (flat, shown) = (flat(turn_of(view)), Geometry::from(view));
        let on_preview = |x: f64, y: f64| {
            let (fx, fy) = shown_to_view(x, y, &flat);
            view_to_shown(fx, fy, &shown)
        };
        let mut shapes = Vec::new();
        for (i, z) in self.zones.iter().enumerate() {
            let z = map_zone(*z, on_preview);
            // Off the preview while it's zoomed in somewhere else
            if z[2] - z[0] < 1e-4 || z[3] - z[1] < 1e-4 {
                continue;
            }
            shapes.push(Shape::Rect { x0: z[0], y0: z[1], x1: z[2], y1: z[3], stroke: "#f2b441".into(), width: 2.0, fill: Some("#f2b44126".into()) });
            shapes.push(Shape::Text { x: z[0] + 0.01, y: z[1] + 0.01, text: (i + 1).to_string(), color: "#f2b441".into(), size: 14.0 });
        }
        PreviewLayer { shapes }
    }

    /// The zones are on the preview while the window shows or it's armed, but never on a stream they aren't watching.
    fn show_layer(&self, host: &mut dyn Host) {
        if (self.armed || self.window_shown) && self.on_armed_source(host) {
            let layer = self.layer(&host.view());
            host.add_preview_layer(NAME, layer);
        } else {
            host.remove_preview_layer(NAME);
        }
        host.refresh_preview();
    }

    // ── Watching ──────────────────────────────────────────────────────

    /// Watches the zones while armed and the phone can take photos, and stops otherwise. The host starts the watch over each time it's told
    /// one, so it's told only when the zones or the sensitivity change.
    fn sync(&mut self, host: &mut dyn Host) {
        if self.armed && self.source.is_none() {
            self.source = host.focused_source_id();
        }
        self.caps = self.read_caps(host);
        let want = self.running(&*host, |p| (p.armed && p.can_snap(host)).then(|| (p.zones_to_watch(), p.sensitivity)));
        self.keep(&*host);
        if want != self.watch {
            match &want {
                Some((zones, sensitivity)) => host.watch_motion(NAME, zones, *sensitivity),
                None => host.watch_motion(NAME, &[], self.sensitivity),
            }
            self.watch = want;
            self.moving.clear();
        }
        self.show(host);
    }

    fn arm(&mut self, host: &mut dyn Host, on: bool) {
        self.armed = on;
        self.problem = None;
        self.source = None;
        if on {
            self.ready_at = 0.0;
            self.source = host.focused_source_id();
            // A burst still pending may already have put files there, which its end must not count a second time
            (self.used, self.counted) = folder_scan(Path::new(&self.folder), !self.taking.is_empty());
        }
        self.sync(host);
        self.show_layer(host);
        if on {
            if let Some(why) = self.over_limits() {
                self.disarm(host, why);
            }
        }
    }

    /// Why it can't take another burst: the folder has what it may hold, or the volume has too little free.
    fn over_limits(&self) -> Option<String> {
        if self.used >= self.budget {
            return Some(tr("Off since the {size} GB photo limit is reached").field("size", gigabytes(self.budget)));
        }
        if (self.free)(Path::new(&self.folder)).is_some_and(|free| free < self.min_free) {
            return Some(tr("Off since there's less than {size} GB free on the drive").field("size", gigabytes(self.min_free)));
        }
        None
    }

    /// Stops watching, with the reason on the card.
    fn disarm(&mut self, host: &mut dyn Host, why: String) {
        self.armed = false;
        self.source = None;
        self.sync(host);
        self.show_layer(host);
        self.problem = Some(why);
        self.show(host);
    }

    /// Something moved in the zones: a burst of photos when a zone has just been entered, unless it's paused, the phone is busy, the
    /// picture is hidden or the folder or the drive is full.
    fn motion(&mut self, host: &mut dyn Host, zones: &[u32]) {
        let entered = zones.iter().any(|z| !self.moving.contains(z));
        self.moving = zones.to_vec();
        let now = self.now();
        let Some(source) = self.source.clone() else { return };
        if !entered || !self.armed || now < self.ready_at || host.still_busy(Some(&source)) {
            return;
        }
        // A photo comes off the phone whole, never through the steps that hide the picture
        if host.picture_hidden(Some(&source)) {
            return self.show(host);
        }
        if let Some(why) = self.over_limits() {
            return self.disarm(host, why);
        }
        let Some(raw) = host.still_caps(Some(&source)) else { return };
        let caps: StillCaps = serde_json::from_value(raw).unwrap_or_default();
        let mut options = self.running(&*host, |p| p.options(&caps, host));
        // The host looks again when the request is about to go to the phone
        options["skip_if_hidden"] = json!(true);
        let Some(id) = host.take_still(&options, &self.folder, Some(&source)) else { return };
        self.taking.push((id, None));
        self.triggers += 1;
        self.last = local_clock();
        self.ready_at = now + f64::from(self.running(&*host, |p| p.pause_s));
        self.show(host);
    }

    fn done(&mut self, host: &mut dyn Host, ok: bool, data: &Value) {
        let files: Vec<&str> = data.get("files").and_then(Value::as_array).map_or_else(Vec::new, |a| a.iter().filter_map(Value::as_str).collect());
        let code = data.get("code").and_then(Value::as_str);
        // Asked not to take it while the picture was hidden, and it was before the first one: nothing went off
        if code == Some("hidden") && files.is_empty() {
            self.triggers = self.triggers.saturating_sub(1);
            self.ready_at = self.now();
            return self.show(host);
        }
        // What reached the folder counts even when the burst was cut short or failed on a later photo
        self.photos += files.len() as u32;
        self.used += files.iter().filter(|f| !self.counted.contains(Path::new(f))).filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum::<u64>();
        if ok && !files.is_empty() {
            self.problem = None;
        } else if code == Some("hidden") || code == Some("interrupted") {
            // Hiding began, or the stream stopped or changed lens, under it: the card already says it's waiting
        } else {
            let error = data.get("error").and_then(Value::as_str).filter(|e| !e.is_empty());
            self.problem = Some(error.map_or_else(|| tr("The phone couldn't take the photos"), String::from));
        }
        // The pause counts from when the burst ended
        self.ready_at = self.now() + f64::from(self.running(&*host, |p| p.pause_s));
        match self.over_limits().filter(|_| self.armed) {
            Some(why) => self.disarm(host, why),
            None => self.show(host),
        }
    }

    fn picked(&mut self, host: &mut dyn Host, pick: &Pick) {
        self.drawing = false;
        self.follow_view(host);
        // A zone is where the armed stream looks, so one drawn on another stream's picture isn't one
        if !self.on_armed_source(host) {
            return;
        }
        // Where the points are in the whole picture, turned the way the view turns it, as the detector has it
        let &[a, b] = pick.full.as_slice() else { return };
        let turn = turned(Turn::default(), self.orientation);
        let zone = map_zone([a[0], a[1], b[0], b[1]], turn);
        if zone[2] - zone[0] < MIN_ZONE || zone[3] - zone[1] < MIN_ZONE || self.zones.len() >= MAX_ZONES {
            return;
        }
        self.zones.push(zone);
        self.zones_changed(host);
    }

    /// The view turned or flipped: the zones go where the same part of the scene is now.
    fn follow_view(&mut self, host: &mut dyn Host) {
        let now = turn_of(&host.view());
        if now == self.orientation {
            return;
        }
        let remap = turned(self.orientation, now);
        self.zones = self.zones.iter().map(|z| map_zone(*z, &remap)).collect();
        self.orientation = now;
        self.sync(host);
        host.schedule_save();
    }

    fn zones_changed(&mut self, host: &mut dyn Host) {
        self.sync(host);
        self.show_layer(host);
        host.schedule_save();
    }

    fn draw(&mut self, host: &mut dyn Host) {
        if self.zones.len() >= MAX_ZONES || !host.has_plugin("preview") || !self.on_armed_source(host) {
            return;
        }
        self.drawing = host.pick_on_preview(NAME, PickKind::Rect, None, &tr("Drag across the picture to mark a zone. Esc cancels."));
    }

    /// The panels moved to another stream: zones can't be drawn or seen on one the trap isn't armed on.
    fn focus_moved(&mut self, host: &mut dyn Host) {
        if self.drawing && !self.on_armed_source(host) {
            self.drawing = false;
            host.cancel_pick(NAME);
        }
        self.sync(host);
        self.show_layer(host);
    }

    /// The zones are what people drew, with the way the view was turned when they did, so a Reset leaves both. Armed isn't a setting.
    const RESET_KEPT: &'static [&'static str] = &["zones", "turn"];

    fn reset_shown(&self) -> bool {
        off_default(&self.get_config(), &PhototrapPlugin::new().get_config(), Self::RESET_KEPT)
    }

    fn reset(&mut self, host: &mut dyn Host) {
        let cfg = reset_config(&self.get_config(), &PhototrapPlugin::new().get_config(), Self::RESET_KEPT);
        if self.set_config(&cfg).is_ok() {
            self.pending = false;
            self.sync(host);
            host.schedule_save();
        }
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

    /// A phone said how it is: what it can take may have changed, which is when arming waits or goes on.
    fn state_heard(&mut self, host: &mut dyn Host) {
        let streaming = self.can_snap(host);
        if self.read_caps(host) != self.caps || (self.armed && streaming != self.watch.is_some()) {
            self.sync(host);
        } else {
            // The phone's focus moved: only the dot on the Focus slider follows it
            host.set_ui_props("card", &json!({"shot-focus-marks": shot_focus_marks(self.caps.as_ref(), focus_now(host))}));
        }
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            self.follow_view(host);
            self.caps = self.read_caps(host);
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
            in_presets: true,
            device_local: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.follow_view(host);
        self.caps = self.read_caps(host);
        let props = self.all_props(host);
        host.set_ui_props("card", &props);
        self.show_layer(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        if self.watch.take().is_some() {
            host.watch_motion(NAME, &[], self.sensitivity);
        }
        if self.drawing {
            host.cancel_pick(NAME);
        }
        host.remove_preview_layer(NAME);
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, _stream: &StreamInfo) {
        self.focus_moved(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        let now = self.now();
        self.taking.retain(|(_, lost)| lost.is_none_or(|at| now - at < FORGET_AFTER_S));
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        match event {
            Event::RequestProgress { data, .. } if data.get("state").and_then(Value::as_str) == Some("late") && data.get("code").and_then(Value::as_str) == Some("restore") => {
                self.problem = Some(data.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()).map_or_else(|| tr("Couldn't restore the camera settings."), String::from));
                self.show(host);
            }
            // Only the stream it was armed on: another phone's movement is nothing to it
            Event::MotionSeen { source_id, owner, zones } if owner == NAME && self.source.as_deref() == Some(source_id.as_str()) => self.motion(host, zones),
            Event::PreviewPicked { owner, pick } if owner == NAME => match pick {
                Some(pick) => self.picked(host, pick),
                None => self.drawing = false,
            },
            Event::RequestDone { request, ok, data, .. } if self.taking.iter().any(|(r, _)| r == request) => {
                self.taking.retain(|(r, _)| r != request);
                self.done(host, *ok, data);
                if self.taking.is_empty() {
                    self.counted.clear();
                }
            }
            Event::WindowShown { shown } => {
                self.window_shown = *shown;
                self.show_layer(host);
            }
            Event::StreamStopped => {
                self.down = true;
                // The bursts still end on their own and say what reached the folder
                let now = self.now();
                for (_, lost) in &mut self.taking {
                    lost.get_or_insert(now);
                }
                self.sync(host);
            }
            Event::StreamStarted { .. } | Event::StreamConnected { .. } => {
                self.down = false;
                self.sync(host);
            }
            // The phone's lenses can arrive after its stream connects
            Event::PhoneStateUpdated { .. } if self.source.is_none() || self.source == host.focused_source_id() => self.state_heard(host),
            Event::StreamState { source_id, .. } if self.source.as_deref() == Some(source_id.as_str()) => self.state_heard(host),
            Event::ViewChanged { .. } => {
                self.follow_view(host);
                self.show_layer(host);
            }
            Event::SourceSelected { .. } => self.focus_moved(host),
            Event::StillCapsChanged { .. } | Event::StreamLost { .. } | Event::CameraSwitched { .. } => self.sync(host),
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        let props = json!({"switch-name": tr("Armed"), "switch-help": tr(SWITCH_TIP), "draw-text": tr("Draw zone"), "draw-tip": tr(DRAW_TIP),
                           "clear-text": tr("Clear zones"), "clear-tip": tr(CLEAR_TIP), "active": self.paired.on(), "armed": self.armed,
                           "status": tr("Not armed"), "status-kind": "status_dim", "count-max": 8, "count": 3, "count-enabled": false, "night-enabled": false,
                           "count-text": "1", "night-tip": tr(NIGHT_OFF_TIP), "pause-text": tr("{seconds} s").field("seconds", self.pause_s),
                           "sensitivity-text": self.sensitivity.to_string(), "sensitivity": self.sensitivity, "pause": self.pause_s,
                           "folder-text": self.shown_folder(), "folder-tip": self.folder});
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
            "customize-toggled" => {
                self.customize_open = flag;
                self.show(host);
            }
            "shot-shutter-picked" | "shot-iso-picked" => {
                let suppressed = self.night && self.night_offered();
                // Without the phone's capabilities (its stream stopped) Camera still clears an override
                let caps = self.caps.clone().unwrap_or_default();
                if pick_shot(&mut self.shot, &caps, event.callback == "shot-shutter-picked", number.max(0.0) as usize, suppressed) {
                    self.show(host);
                    host.schedule_save();
                }
            }
            "shot-focus-moved" | "shot-focus-reset" => {
                let suppressed = self.night && self.night_offered();
                let position = (event.callback == "shot-focus-moved").then_some(number);
                if move_shot_focus(&mut self.shot, self.caps.as_ref(), position, suppressed) {
                    self.show(host);
                    host.schedule_save();
                }
            }
            "switch-toggled-by-hand" => self.arm(host, flag),
            "draw-clicked" => self.draw(host),
            "folder-clicked" => {
                // A folder nothing has gone to yet isn't there to open
                let _ = std::fs::create_dir_all(&self.folder);
                host.open_url(&file_url(Path::new(&self.folder)));
            }
            "clear-clicked" => {
                self.zones.clear();
                self.zones_changed(host);
            }
            "sensitivity-moved" => self.set_number(host, "sensitivity", number),
            "count-moved" => self.set_number(host, "count", number),
            "pause-moved" => self.set_number(host, "pause", number),
            "reset-clicked" => self.reset(host),
            "use-zoom-toggled" => {
                self.use_zoom = flag;
                self.show(host);
                host.schedule_save();
            }
            "night-toggled" => {
                self.night = flag;
                self.show(host);
                host.schedule_save();
            }
            _ => {}
        }
        self.keep(&*host);
    }

    /// Armed isn't saved, so a restart never starts taking photos.
    fn get_config(&self) -> Value {
        let mut cfg = json!({"zones": self.zones, "sensitivity": self.sensitivity, "count": self.count, "pause": self.pause_s, "night": self.night, "use_zoom": self.use_zoom});
        if self.shot != ShotChoice::default() { cfg["shot"] = self.shot.to_options_value(); }
        // The zones' orientation, so they follow the view if it's turned another way by the time they're loaded
        if self.orientation != Turn::default() {
            cfg["turn"] = json!(self.orientation);
        }
        cfg
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
        self.use_zoom = cfg.get("use_zoom") == Some(&Value::Bool(true));
        self.shot = shot_choice(cfg.get("shot"));
        self.orientation = cfg.get("turn").and_then(|t| serde_json::from_value(t.clone()).ok()).unwrap_or_default();
        self.pending = true;
        Ok(())
    }

    /// No empty zones, so recalling the preset leaves the zones as they are.
    fn preset_config(&self) -> Value {
        let mut cfg = self.get_config();
        if self.zones.is_empty() {
            cfg.as_object_mut().map(|m| (m.remove("zones"), m.remove("turn")));
        }
        cfg
    }

    /// A key the preset lacks keeps its value. Armed stays as it is, so a recall never starts the trap or moves what it watches.
    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        let Value::Object(cfg) = cfg else { return Err(ConfigRejected("settings".into())) };
        let mut merged = self.get_config();
        // An armed trap keeps its zones, as they are what it watches
        let zones = cfg.get("zones").filter(|z| !self.armed && z.as_array().is_some_and(|a| !a.is_empty()));
        // The turn belongs to the zones, so a preset with zones and no turn means the default turn
        if zones.is_some() {
            merged.as_object_mut().map(|m| m.remove("turn"));
        }
        for (key, value) in cfg {
            if matches!(key.as_str(), "zones" | "turn") && zones.is_none() {
                continue;
            }
            merged[key] = value.clone();
        }
        self.set_config(&merged)?;
        self.catch_up(host);
        if self.armed {
            self.sync(host);
        }
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("Phototrap".into(), format!("{}, {} zones, {} triggers", if self.armed { "armed" } else { "off" }, self.zones.len(), self.triggers))]
    }
}

#[cfg(test)]
#[path = "phototrap_tests.rs"]
mod tests;
