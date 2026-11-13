//! The Colour grading plugin (plugins/colour_grading.py): wheels, curves and LUT files for the look of the picture. It hands the
//! colour pipeline its grade baked into a lattice (and into exact per-channel tables when each channel only depends on itself).

use telescope_core::i18n::Fields;
use crate::cube::{read_cube, write_cube, CubeLut, ReadError};
use crate::editors::{Change, Curve, Kind, Wheel};
use crate::grade::{self, Grade, CHANNELS, LINEAR, MASTER_DEFAULT, MASTER_RANGE, WHEELS};
use crate::shared::{number_in, Paired};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use telescope_core::actions::{self, SliderSpec};
use telescope_core::colour::{lattice, LEVELS};
use telescope_core::event::Event;
use crate::auto_fit::{self, Basic, Fit, Wb};
use crate::auto_grade::{AutoState, CameraLock, MeasureStep, POST_MEASURED, T_HOLD, T_LABEL, T_TIMEOUT};
use crate::auto_measure::Measurement;
use crate::auto_tuning::{HOLD_TIMEOUT_MS, LABEL_MS, MEASURE_TIMEOUT_MS};
use telescope_core::frame::{FrameStage, FrameStep};
use telescope_core::host::{ColourStage, FileDialog, FileDialogKind, Host, Poster, RequestId};
use telescope_core::people::{self, Track};
use telescope_core::pose::Person;
use telescope_core::view::Geometry;
use telescope_core::i18n::{n_, number, percent, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::settings::SettingSpec;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "colour_grading";
/// The pipeline's name for what it changes.
pub const OWNER: &str = NAME;
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/colour_grading.slint"));

/// Posted by the thread that baked a stage.
const POST_STAGE: &str = "stage_baked";
/// Posted by the thread that read a saved LUT.
const POST_LUT: &str = "lut_read";
const LUT_FILES: &str = n_("LUT files (*.cube)");
const EXPORT_SIZES: [(u32, &str); 2] = [(33, n_("33-point LUT (*.cube)")), (65, n_("65-point LUT (*.cube)"))];
const WHEEL_NAMES: [&str; 4] = [n_("Lift"), n_("Gamma"), n_("Gain"), n_("Offset")];
const WHEEL_TIPS: [&str; 4] = [
    n_("Change the colour and level of the shadows without changing white."),
    n_("Change the colour and brightness of the midtones without changing black or white."),
    n_("Change the colour and level of the highlights without changing black."),
    n_("Change the colour and level of the whole picture."),
];
const WHEEL_TIP: &str = n_("Drag towards a colour to add it.");
const WHEEL_HINT: &str = n_("Hold Shift to go slower. Double-click to reset.");
const CANT_OPEN: &str = n_("Telescope can't open that file.");
const CANT_SAVE: &str = n_("Telescope couldn't save the LUT there.");
const SAVED: &str = n_("Saved {file}.");
const SWITCH_TIP: &str = n_("Turn the grade off to see the camera's original colours.");
/// What vision is asked for under while Auto looks at the picture.
const AUTO_TITLE: &str = "Auto colour grading";
const AUTO_NO_STREAM: &str = n_("Start the stream first, so Auto has a picture to look at.");
const AUTO_LABEL_FINE: &str = n_("Looks fine");
const AUTO_LABEL_NO_STREAM: &str = n_("No stream");
const AUTO_LABEL_NO_PICTURE: &str = n_("No picture");
const AUTO_LOOKING: &str = n_("Analysing the picture…");
const AUTO_NO_PICTURE: &str = n_("No steady picture to analyse. Try again while nothing moves.");
const AUTO_NOTHING: &str = n_("Nothing to change, the picture already looks right.");
const AUTO_TAKEN_OFF: &str = n_("Nothing needs changing, so Auto's earlier changes were removed.");
const AUTO_CHANGED: &str = n_("Auto changed {parts}.");
const AUTO_NO_FACE: &str = n_("No face found, so Auto used the background and brightness.");
const AUTO_CONFLICT: &str = n_("The light is mixed or coloured, so white balance was not changed.");
const AUTO_BACKED_OFF: &str = n_("Your own wheels and curves would have clipped the picture, so Auto applied only part of the change.");
const AUTO_DRIFTS: &str = n_("The camera adjusts automatically, so the result may drift.");
const AUTO_PARTS: [&str; 5] = [n_("white balance"), n_("brightness"), n_("contrast and blacks"), n_("highlights"), n_("saturation")];
/// .cube files kept parsed, so switching phones doesn't read a 65-point LUT again.
const KEEP_PARSED: usize = 4;
const MASTER_STEPS: [f64; 4] = [1000.0, 100.0, 100.0, 1000.0];
/// What a wheel's master slider is called as a shortcut.
const MASTER_LABELS: [&str; 4] = [n_("Lift master"), n_("Gamma master"), n_("Gain master"), n_("Offset master")];
/// How far one press of a master shortcut moves it.
const MASTER_ACTION_STEPS: [f64; 4] = [0.05, 0.1, 0.1, 0.05];

/// A slider under the wheels: key, label, lowest, highest, steps per unit, decimals, signed, tooltip.
struct SliderDef {
    key: &'static str,
    label: &'static str,
    lo: f64,
    hi: f64,
    steps: f64,
    decimals: usize,
    signed: bool,
    tip: &'static str,
}

const SLIDERS: [SliderDef; 8] = [
    SliderDef { key: "temperature", label: n_("Temperature"), lo: -200.0, hi: 200.0, steps: 1.0, decimals: 0, signed: true, tip: n_("Make the picture warmer or cooler.") },
    SliderDef { key: "tint", label: n_("Tint"), lo: -200.0, hi: 200.0, steps: 1.0, decimals: 0, signed: true, tip: n_("Shift the picture towards magenta or green.") },
    SliderDef { key: "contrast", label: n_("Contrast"), lo: 0.0, hi: 2.0, steps: 100.0, decimals: 2, signed: false, tip: n_("Spread the tones apart around the pivot.") },
    SliderDef { key: "pivot", label: n_("Pivot"), lo: 0.0, hi: 1.0, steps: 1000.0, decimals: 3, signed: false, tip: n_("The brightness level that contrast leaves unchanged.") },
    SliderDef { key: "saturation", label: n_("Saturation"), lo: 0.0, hi: 2.0, steps: 100.0, decimals: 0, signed: false, tip: n_("Make the picture more or less colourful.") },
    SliderDef { key: "vibrance", label: n_("Vibrance"), lo: -100.0, hi: 100.0, steps: 1.0, decimals: 0, signed: true, tip: n_("Boost dull colours more than strong colours and skin tones.") },
    SliderDef { key: "shadows", label: n_("Shadows"), lo: -100.0, hi: 100.0, steps: 1.0, decimals: 0, signed: true, tip: n_("Brighten or darken the shadows.") },
    SliderDef { key: "highlights", label: n_("Highlights"), lo: -100.0, hi: 100.0, steps: 1.0, decimals: 0, signed: true, tip: n_("Brighten or darken the highlights.") },
];

fn slider_value(g: &Grade, key: &str) -> f64 {
    match key {
        "temperature" => g.temperature,
        "tint" => g.tint,
        "contrast" => g.contrast,
        "pivot" => g.pivot,
        "saturation" => g.saturation,
        "vibrance" => g.vibrance,
        "shadows" => g.shadows,
        _ => g.highlights,
    }
}

fn set_slider(g: &mut Grade, key: &str, v: f64) {
    match key {
        "temperature" => g.temperature = v,
        "tint" => g.tint = v,
        "contrast" => g.contrast = v,
        "pivot" => g.pivot = v,
        "saturation" => g.saturation = v,
        "vibrance" => g.vibrance = v,
        "shadows" => g.shadows = v,
        _ => g.highlights = v,
    }
}

/// Saved curve points, cleaned up: pairs of numbers 0..1, sorted, a tone curve's ends at 0 and 1, a hue curve's x below 1.
fn points(raw: Option<&Value>, default: &[(f64, f64)], tone: bool) -> Vec<(f64, f64)> {
    let Some(Value::Array(items)) = raw else { return default.to_vec() };
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for item in items.iter().take(64) {
        if let Value::Array(pair) = item {
            if pair.len() == 2 {
                pts.push((number_in(pair.first(), 0.0, 0.0, 1.0), number_in(pair.get(1), 0.0, 0.0, 1.0)));
            }
        }
    }
    if !tone {
        // a hue of 1 is the hue at 0 again, and one hue gets one point (the last said wins its place)
        let mut kept: Vec<(f64, f64)> = Vec::new();
        for (x, y) in pts {
            let x = x.rem_euclid(1.0);
            match kept.iter_mut().find(|p| p.0 == x) {
                Some(p) => p.1 = y,
                None => kept.push((x, y)),
            }
        }
        pts = kept;
    }
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if tone && (pts.len() < 2 || pts[0].0 != 0.0 || pts[pts.len() - 1].0 != 1.0) {
        return default.to_vec();
    }
    if pts.is_empty() { default.to_vec() } else { pts }
}

fn points_json(points: &[(f64, f64)]) -> Value {
    Value::Array(points.iter().map(|p| json!([p.0, p.1])).collect())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ask {
    Load,
    Export,
}

/// A saved .cube file's parse, for as long as the file is the same.
struct Parsed {
    path: String,
    stamp: (u128, u64),
    lut: Arc<CubeLut>,
}

/// A LUT file read on its thread: which read it was, the path, the file's stamp, and what came of it.
type LutRead = (u64, String, Option<(u128, u64)>, Result<CubeLut, ReadError>);

fn stamp(path: &Path) -> Option<(u128, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let ns = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    Some((ns, meta.len()))
}

/// The pipeline's stage for a grade that changes the picture: its lattice, and exact tables when each channel stands alone.
fn build_stage(g: &Grade) -> ColourStage {
    let size = g.lut_size();
    let mut rgb = lattice(size as usize);
    g.apply(&mut rgb);
    let table = g.per_channel().then(|| {
        let mut ramp: Vec<f32> = (0..LEVELS).flat_map(|i| [i as f32 / (LEVELS - 1) as f32; 3]).collect();
        g.apply(&mut ramp);
        Arc::new(ramp)
    });
    let grade = g.clone();
    ColourStage::new(size, Arc::new(rgb), table).with_eval(move |px: &mut [f32]| grade.apply(px))
}

pub struct ColourGradingPlugin {
    grade: Grade,
    on: bool,
    lut_path: String,
    lut_problem: String,
    /// The saved LUT file isn't read while the effect is off: it is when the effect goes on.
    lut_deferred: bool,
    /// Counts publishes; a bake is only handed over if nothing was published since it began.
    stage_gen: u64,
    stage_busy: bool,
    /// The saved LUT is being read on a thread (lut_gen says which read counts); nothing is published until it lands.
    lut_gen: u64,
    lut_loading: bool,
    lut_slot: Arc<Mutex<Vec<LutRead>>>,
    stage_slot: Arc<Mutex<Option<(u64, ColourStage)>>>,
    /// 0 tone curves, 1 hue vs sat, 2 hue vs hue.
    curve: usize,
    channel: usize,
    page: usize,
    compare: bool,
    split: i32,
    wheels: [Wheel; 4],
    editor: Curve,
    paired: Paired,
    /// The grade changed in set_config, which has no host: it is published with the next hook that has one.
    dirty: bool,
    parsed: Vec<Parsed>,
    asked: Option<(RequestId, Ask)>,
    /// A file the export would replace, waiting for a yes.
    replace: Option<PathBuf>,
    export_size: u32,
    export_note: (String, &'static str),
    started: Instant,
    /// Where the switch and the compare were when a key held for them went down.
    held: [Option<bool>; 2],
    auto: AutoState,
    #[cfg(test)]
    reads: usize,
}

impl Default for ColourGradingPlugin {
    fn default() -> Self {
        ColourGradingPlugin::new()
    }
}

impl ColourGradingPlugin {
    pub fn new() -> ColourGradingPlugin {
        let mut editor = Curve::default();
        editor.set_curve(Kind::Tone, &LINEAR, &LINEAR);
        ColourGradingPlugin {
            grade: Grade::default(),
            on: true,
            lut_path: String::new(),
            lut_problem: String::new(),
            lut_deferred: false,
            stage_gen: 0,
            stage_busy: false,
            lut_gen: 0,
            lut_loading: false,
            lut_slot: Arc::new(Mutex::new(Vec::new())),
            stage_slot: Arc::default(),
            curve: 0,
            channel: 0,
            page: 0,
            compare: false,
            split: 50,
            wheels: Default::default(),
            editor,
            paired: Paired::default(),
            dirty: false,
            parsed: Vec::new(),
            asked: None,
            replace: None,
            export_size: 33,
            export_note: (String::new(), "dim"),
            started: Instant::now(),
            held: [None; 2],
            auto: AutoState::default(),
            #[cfg(test)]
            reads: 0,
        }
    }

    pub fn grade(&self) -> &Grade {
        &self.grade
    }

    /// Where Auto reads the time people were seen at, for a test to run by hand.
    #[cfg(test)]
    pub(crate) fn set_auto_clock(&mut self, now: Arc<dyn Fn() -> u64 + Send + Sync>) {
        self.auto.now = now;
    }

    /// What the last press of Auto measured and fitted.
    pub fn last_auto(&self) -> Option<&(Measurement, Fit)> {
        self.auto.last.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn auto_shared(&self) -> Arc<crate::auto_grade::Shared> {
        self.auto.shared.clone()
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    // ── What goes to the colour pipeline ──────────────────────────────

    /// The grade as the pipeline takes it, or None for no change at all (off, or a grade that does nothing).
    pub fn stage(&self) -> Option<ColourStage> {
        if !self.on || self.grade.is_identity() {
            return None;
        }
        Some(build_stage(&self.grade))
    }

    /// Tells the pipeline. Baking the lattice takes milliseconds (tens with a detailed LUT), so a grade that changes the picture is baked
    /// on a thread, one at a time: edits that come while it runs are folded into the next bake, and only the newest is handed over.
    fn publish(&mut self, host: &mut dyn Host) {
        self.dirty = false;
        self.stage_gen += 1;
        // the grade isn't whole until the LUT is in: landing it publishes
        if self.lut_loading {
            return;
        }
        let wanted = self.on && !self.grade.is_identity();
        let poster = self.auto.poster.clone().filter(|_| wanted);
        let Some(poster) = poster else {
            host.set_colour_stage(OWNER, self.stage(), 0, None);
            return;
        };
        if !self.stage_busy && !self.bake_on_thread(poster) {
            host.set_colour_stage(OWNER, self.stage(), 0, None);
        }
    }

    /// Whether a stage is being baked on its thread (for a test to wait on).
    #[cfg(test)]
    pub(crate) fn baking(&self) -> bool {
        self.stage_busy || self.lut_loading
    }

    /// Starts baking the grade as it is now; false when there was no thread to do it on.
    fn bake_on_thread(&mut self, poster: Arc<dyn Poster>) -> bool {
        let (grade, generation, slot) = (self.grade.clone(), self.stage_gen, self.stage_slot.clone());
        let spawned = std::thread::Builder::new().name("colour-grading-bake".into()).spawn(move || {
            let stage = build_stage(&grade);
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, stage));
            poster.post(POST_STAGE, Value::Null);
        });
        self.stage_busy = spawned.is_ok();
        spawned.is_ok()
    }

    /// A bake finished: its stage goes to the pipeline unless the grade changed since, in which case the newest is baked next.
    fn stage_baked(&mut self, host: &mut dyn Host) {
        self.stage_busy = false;
        let Some((generation, stage)) = self.stage_slot.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        if generation == self.stage_gen {
            host.set_colour_stage(OWNER, Some(stage), 0, None);
        } else if self.on && !self.grade.is_identity() {
            if let Some(poster) = self.auto.poster.clone() {
                if self.bake_on_thread(poster) {
                    return;
                }
            }
            host.set_colour_stage(OWNER, self.stage(), 0, None);
        }
    }

    /// Settings were loaded without a host to tell: the card shows them and the pipeline gets the grade, once.
    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.dirty {
            self.show_all(host);
            self.publish(host);
        }
    }

    fn set_grade(&mut self, host: &mut dyn Host, edit: impl FnOnce(&mut Grade)) {
        edit(&mut self.grade);
        self.publish(host);
        host.schedule_save();
    }

    // ── .cube files ───────────────────────────────────────────────────

    /// (the LUT at path or None, the problem with it in words or ""), from the parsed ones while the file is unchanged.
    fn read_lut(&mut self, path: &str) -> (Option<Arc<CubeLut>>, String) {
        let key = stamp(Path::new(path));
        if let Some(key) = key {
            if let Some(i) = self.parsed.iter().position(|p| p.path == path && p.stamp == key) {
                let hit = self.parsed.remove(i);
                let lut = hit.lut.clone();
                self.parsed.push(hit);
                return (Some(lut), String::new());
            }
        }
        #[cfg(test)]
        {
            self.reads += 1;
        }
        self.kept_read(path, key, read_cube(Path::new(path)))
    }

    /// What a read of the file at path came to, kept for next time when it worked.
    fn kept_read(&mut self, path: &str, key: Option<(u128, u64)>, read: Result<CubeLut, ReadError>) -> (Option<Arc<CubeLut>>, String) {
        match read {
            Ok(lut) => {
                let lut = Arc::new(lut);
                if let Some(stamp) = key {
                    while self.parsed.len() >= KEEP_PARSED {
                        self.parsed.remove(0);
                    }
                    self.parsed.push(Parsed { path: path.to_string(), stamp, lut: lut.clone() });
                }
                (Some(lut), String::new())
            }
            Err(ReadError::Cube(e)) => (None, e.translated()),
            Err(ReadError::Io(_)) => (None, tr(CANT_OPEN)),
        }
    }

    /// The saved LUT the effect was switched on for: from the parsed ones if the file is unchanged, else read on a thread.
    fn read_deferred_lut(&mut self, host: &mut dyn Host) {
        let path = self.lut_path.clone();
        let cached = stamp(Path::new(&path)).is_some_and(|key| self.parsed.iter().any(|p| p.path == path && p.stamp == key));
        let Some(poster) = self.auto.poster.clone().filter(|_| !cached) else {
            let (lut, problem) = self.read_lut(&path);
            self.grade.lut = lut;
            self.lut_problem = problem;
            self.show(host, &self.lut_props());
            return;
        };
        self.lut_gen += 1;
        let (generation, slot) = (self.lut_gen, self.lut_slot.clone());
        let spawned = std::thread::Builder::new().name("colour-grading-lut".into()).spawn(move || {
            let key = stamp(Path::new(&path));
            let read = read_cube(Path::new(&path));
            slot.lock().unwrap_or_else(|e| e.into_inner()).push((generation, path, key, read));
            poster.post(POST_LUT, Value::Null);
        });
        self.lut_loading = spawned.is_ok();
        if !self.lut_loading {
            let path = self.lut_path.clone();
            let (lut, problem) = self.read_lut(&path);
            self.grade.lut = lut;
            self.lut_problem = problem;
            self.show(host, &self.lut_props());
        }
    }

    /// The LUT's read finished: it goes into the grade unless the file, the path or the switch changed since.
    fn lut_landed(&mut self, host: &mut dyn Host) {
        let done: Vec<LutRead> = std::mem::take(&mut *self.lut_slot.lock().unwrap_or_else(|e| e.into_inner()));
        let Some((_, path, key, read)) = done.into_iter().find(|(generation, path, ..)| *generation == self.lut_gen && *path == self.lut_path) else { return };
        if !std::mem::take(&mut self.lut_loading) || !self.on {
            return;
        }
        #[cfg(test)]
        {
            self.reads += 1;
        }
        let (lut, problem) = self.kept_read(&path, key, read);
        self.grade.lut = lut;
        self.lut_problem = problem;
        self.show(host, &self.lut_props());
        self.publish(host);
    }

    /// A read of the LUT still out is for a path or a time that's gone.
    fn drop_lut_read(&mut self) {
        self.lut_gen += 1;
        self.lut_loading = false;
    }

    fn load_lut(&mut self, host: &mut dyn Host, path: &str) {
        let (lut, problem) = if path.is_empty() { (None, String::new()) } else { self.read_lut(path) };
        self.lut_path = path.to_string();
        self.drop_lut_read();
        self.lut_deferred = false;
        self.lut_problem = problem;
        self.grade.lut = lut;
        self.show(host, &self.lut_props());
        self.publish(host);
    }

    fn start_folder(&self) -> String {
        let folder = Path::new(&self.lut_path).parent().filter(|p| !self.lut_path.is_empty() && !p.as_os_str().is_empty());
        folder.map_or_else(|| std::env::home_dir().unwrap_or_default().display().to_string(), |p| p.display().to_string())
    }

    fn choose_lut(&mut self, host: &mut dyn Host) {
        let dialog = FileDialog { kind: FileDialogKind::Open, title: tr("Load a LUT"), start: self.start_folder(), filters: vec![tr(LUT_FILES)] };
        self.asked = host.pick_file(&dialog).map(|id| (id, Ask::Load));
    }

    fn export(&mut self, host: &mut dyn Host) {
        let folder = if self.lut_path.is_empty() { std::env::home_dir().unwrap_or_default() } else { Path::new(&self.lut_path).parent().map(Path::to_path_buf).unwrap_or_default() };
        let filters: Vec<String> = EXPORT_SIZES.iter().map(|(_, t)| tr(t)).collect();
        let dialog = FileDialog { kind: FileDialogKind::Save, title: tr("Export the grade"), start: folder.join("Telescope grade.cube").display().to_string(), filters };
        self.asked = host.pick_file(&dialog).map(|id| (id, Ask::Export));
    }

    fn export_picked(&mut self, host: &mut dyn Host, path: &str, chosen: &str) {
        let mut path = PathBuf::from(path);
        let has_ext = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cube"));
        self.export_size = EXPORT_SIZES.iter().find(|(_, t)| tr(t) == chosen).map_or(33, |(s, _)| *s);
        if !has_ext {
            // the dialog only asked about the name without it
            let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            name.push(".cube");
            path.set_file_name(name);
            if path.exists() {
                self.replace = Some(path);
                self.show(host, &self.export_props());
                return;
            }
        }
        self.write_export(host, &path);
    }

    fn write_export(&mut self, host: &mut dyn Host, path: &Path) {
        let size = self.export_size as usize;
        let graded = if self.on { self.grade.clone() } else { Grade::default() };
        let mut cube = lattice(size);
        graded.apply(&mut cube);
        self.replace = None;
        self.export_note = match write_cube(path, &cube, size, "Telescope grade") {
            Ok(()) => (tr(SAVED).field("file", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), "dim"),
            Err(_) => (tr(CANT_SAVE), "warn"),
        };
        self.show(host, &self.export_props());
    }

    // ── What the card shows ───────────────────────────────────────────

    fn show(&self, host: &mut dyn Host, props: &Value) {
        host.set_ui_props("card", props);
    }

    fn master_text(name: usize, value: f64) -> String {
        number(value, 2, name != 2 && (value * 100.0).round_ties_even() / 100.0 != 0.0)
    }

    fn wheel_props(&self) -> Value {
        let g = &self.grade;
        let mut masters = Vec::new();
        let mut texts = Vec::new();
        for i in 0..4 {
            let v = g.masters[i];
            masters.push(((v * MASTER_STEPS[i]).round_ties_even() / MASTER_STEPS[i]).clamp(MASTER_RANGE[i].0, MASTER_RANGE[i].1));
            texts.push(Self::master_text(i, v));
        }
        json!({
            "wheel-x": self.wheels.iter().map(|w| w.x).collect::<Vec<_>>(),
            "wheel-y": self.wheels.iter().map(|w| w.y).collect::<Vec<_>>(),
            "wheel-colour": self.wheels.iter().map(|w| wheel_colour(w.x, w.y, 1.6)).collect::<Vec<_>>(),
            "master-value": masters,
            "master-text": texts,
            "grade-lit": self.off_default(),
        })
    }

    /// Anything Reset would change: every setting but the LUT, which has its own way off.
    fn off_default(&self) -> bool {
        let (g, d) = (&self.grade, Grade::default());
        g.temperature != d.temperature || g.tint != d.tint || g.wheels != d.wheels || g.masters != d.masters || g.contrast != d.contrast
            || g.pivot != d.pivot || g.shadows != d.shadows || g.highlights != d.highlights || g.saturation != d.saturation
            || g.vibrance != d.vibrance || g.curves != d.curves || g.hue_sat != d.hue_sat || g.hue_hue != d.hue_hue
    }

    fn slider_text(def: &SliderDef, value: f64) -> String {
        if def.key == "saturation" { percent(value * 100.0, 0, false) } else { number(value, def.decimals, def.signed && value != 0.0) }
    }

    fn slider_props(&self) -> Value {
        let values: Vec<f64> = SLIDERS.iter().map(|d| (slider_value(&self.grade, d.key) * d.steps).round_ties_even() / d.steps).collect();
        let texts: Vec<String> = SLIDERS.iter().map(|d| Self::slider_text(d, slider_value(&self.grade, d.key))).collect();
        json!({"slider-value": values, "slider-text": texts, "grade-lit": self.off_default()})
    }

    fn curve_props(&self) -> Value {
        let line: String = self.editor.line(200).iter().enumerate()
            .map(|(i, (x, y))| format!("{} {:.1} {:.1}", if i == 0 { "M" } else { "L" }, x * 1000.0, (1.0 - y) * 1000.0)).collect::<Vec<_>>().join(" ");
        json!({
            "curve": self.curve,
            "channel": self.channel,
            "curve-line": line,
            "curve-points": self.editor.points.iter().map(|p| json!({"x": p.0, "y": p.1})).collect::<Vec<_>>(),
            "curve-picked": self.editor.picked,
            "grade-lit": self.off_default(),
        })
    }

    fn lut_props(&self) -> Value {
        let name = Path::new(&self.lut_path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        json!({
            "lut-text": name,
            "lut-path": self.lut_path,
            "lut-loaded": !self.lut_path.is_empty(),
            "strength": (self.grade.lut_strength * 100.0).round_ties_even(),
            "strength-text": percent(self.grade.lut_strength * 100.0, 0, false),
            "lut-note": self.lut_problem,
        })
    }

    fn export_props(&self) -> Value {
        let (text, kind) = match &self.replace {
            Some(path) => (tr("{file} is already there. Replace it?").field("file", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), "none"),
            None => self.export_note.clone(),
        };
        json!({"export-note": text, "export-kind": kind, "replace-open": self.replace.is_some()})
    }

    fn split_props(&self) -> Value {
        json!({"compare": self.compare, "split": self.split, "split-text": percent(self.split as f64, 0, false)})
    }

    /// Every control from the grade, without each one handing it back (_show_controls).
    fn sync_wheels(&mut self) {
        for i in 0..4 {
            let (x, y) = self.grade.wheels[i];
            self.wheels[i].set_value(x, y);
        }
    }

    fn show_curve(&mut self) {
        let tone = self.curve == 0;
        if tone {
            let pts = self.grade.curves[self.channel].clone();
            self.editor.set_curve(Kind::Tone, &pts, &LINEAR);
        } else {
            let pts = if self.curve == 1 { self.grade.hue_sat.clone() } else { self.grade.hue_hue.clone() };
            let neutral = grade::neutral_hue();
            self.editor.set_curve(Kind::Hue, &pts, &neutral);
        }
    }

    fn all_props(&mut self) -> Value {
        self.sync_wheels();
        self.show_curve();
        let mut all = Map::new();
        for part in [self.wheel_props(), self.slider_props(), self.curve_props(), self.lut_props(), self.export_props(), self.split_props()] {
            if let Value::Object(m) = part {
                all.extend(m);
            }
        }
        all.insert("on".into(), json!(self.on));
        all.insert("page".into(), json!(self.page));
        all.insert("active".into(), json!(self.paired.on()));
        if let Value::Object(m) = self.auto.props() {
            all.extend(m);
        }
        Value::Object(all)
    }

    fn show_all(&mut self, host: &mut dyn Host) {
        let props = self.all_props();
        host.set_ui_props("card", &props);
    }

    /// The slider rows' fixed words, ranges and steps, which don't change.
    fn fixed_props() -> Value {
        json!({
            "switch-name": tr("Colour grading"),
            "switch-help": tr(SWITCH_TIP),
            "wheel-labels": WHEEL_NAMES.iter().map(|w| tr("{wheel} colour").field("wheel", tr(w))).collect::<Vec<_>>(),
            "master-tips": WHEEL_TIPS.iter().map(|t| tr(t)).collect::<Vec<_>>(),
            "master-labels": MASTER_LABELS.iter().map(|t| tr(t)).collect::<Vec<_>>(),
            "wheel-tip": tr(WHEEL_TIP),
            "wheel-hint": tr(WHEEL_HINT),
            "master-min": MASTER_RANGE.iter().map(|r| r.0).collect::<Vec<_>>(),
            "master-max": MASTER_RANGE.iter().map(|r| r.1).collect::<Vec<_>>(),
            "master-step": MASTER_STEPS.iter().map(|s| 1.0 / s).collect::<Vec<_>>(),
            "slider-tips": SLIDERS.iter().map(|d| tr(d.tip)).collect::<Vec<_>>(),
            "slider-min": SLIDERS.iter().map(|d| d.lo).collect::<Vec<_>>(),
            "slider-max": SLIDERS.iter().map(|d| d.hi).collect::<Vec<_>>(),
            "slider-step": SLIDERS.iter().map(|d| 1.0 / d.steps).collect::<Vec<_>>(),
            "slider-default": SLIDERS.iter().map(|d| slider_value(&Grade::default(), d.key)).collect::<Vec<_>>(),
            "master-default": MASTER_DEFAULT.to_vec(),
        })
    }

    // ── Auto ──────────────────────────────────────────────────────────

    fn show_auto(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.auto.props());
    }

    fn say(&mut self, host: &mut dyn Host, text: String, kind: &'static str) {
        self.auto.note = (text, kind);
        self.show_auto(host);
    }

    /// The button says a word or two about how the last press went, then goes back to "Auto".
    fn flash(&mut self, host: &mut dyn Host, label: &str) {
        self.auto.label = tr(label);
        host.cancel_schedule(T_LABEL);
        host.schedule(LABEL_MS, T_LABEL);
        self.show_auto(host);
    }

    /// What Auto last said in words, for tests and the log.
    #[cfg(test)]
    pub(crate) fn auto_note(&self) -> &str {
        &self.auto.note.0
    }

    /// Where the phone's auto exposure and white balance get locked before measuring, waiting until it confirms. Nothing locks yet.
    fn lock_camera(&mut self, _host: &mut dyn Host) -> CameraLock {
        CameraLock::default()
    }

    fn auto_pressed(&mut self, host: &mut dyn Host) {
        if self.auto.busy {
            return;
        }
        let source = host.focused_source_id();
        let Some(source) = source.filter(|_| host.is_streaming()) else {
            self.say(host, tr(AUTO_NO_STREAM), "warn");
            self.flash(host, AUTO_LABEL_NO_STREAM);
            return;
        };
        let lock = self.lock_camera(host);
        self.auto.lock = Some(lock);
        // vision stays off until someone asks, and may need to load its models: wait for its first answer unless it can't answer
        // no pack yet: fetch it for next time, and grade from the whole picture meanwhile (asking for people moves the state on)
        let pack_missing = host.vision_state() == "missing" && host.vision_pack().available;
        host.want_people(AUTO_TITLE, true);
        if pack_missing {
            host.download_vision();
        }
        self.auto.label.clear();
        host.cancel_schedule(T_LABEL);
        let hold = !matches!(host.vision_state().as_str(), "missing" | "failed");
        self.auto.shared.start(&source, Geometry::from(&host.view()), hold);
        host.schedule(if hold { HOLD_TIMEOUT_MS } else { MEASURE_TIMEOUT_MS }, if hold { T_HOLD } else { T_TIMEOUT });
        self.auto.busy = true;
        self.say(host, tr(AUTO_LOOKING), "dim");
    }

    /// Something outside the window (the MCP server) wants the picture as numbers: the same look Auto takes, with no slider moved.
    fn probe_requested(&mut self, host: &mut dyn Host, token: &str) {
        let refuse = |host: &mut dyn Host, why: &str| {
            host.emit(Event::PictureMeasured { token: token.into(), ok: false, message: why.into(), data: Value::Null });
        };
        if self.auto.busy {
            return refuse(host, "Something is already looking at the picture (Auto colour grading, or another measurement). Try again in a few seconds.");
        }
        let source = host.focused_source_id();
        let Some(source) = source.filter(|_| host.is_streaming()) else {
            return refuse(host, "No stream is running, so there is no picture to measure. Start the phone's stream first.");
        };
        self.auto.probe = Some(token.into());
        self.auto.lock = Some(CameraLock::default());
        self.auto.shared.start(&source, Geometry::from(&host.view()), false);
        if host.vision_state() == "ready" {
            host.want_people(AUTO_TITLE, true);
        }
        host.schedule(MEASURE_TIMEOUT_MS, T_TIMEOUT);
        self.auto.busy = true;
        self.say(host, tr(AUTO_LOOKING), "dim");
    }

    /// Stops looking without an answer (the stream ended or moved away).
    fn auto_abort(&mut self, host: &mut dyn Host) {
        if self.auto.busy {
            if let Some(token) = self.auto.probe.take() {
                host.emit(Event::PictureMeasured { token, ok: false, message: "The stream ended before the measurement was done.".into(), data: Value::Null });
            }
            self.auto.shared.stop();
            self.auto.busy = false;
            self.auto.lock = None;
            host.cancel_schedule(T_TIMEOUT);
            host.cancel_schedule(T_HOLD);
            host.want_people(AUTO_TITLE, false);
            self.say(host, String::new(), "dim");
        }
    }

    /// The measurement is in (or ran out of time): fit it and put the sliders in.
    fn auto_measured(&mut self, host: &mut dyn Host) {
        if !self.auto.busy {
            return;
        }
        host.cancel_schedule(T_TIMEOUT);
        host.cancel_schedule(T_HOLD);
        host.want_people(AUTO_TITLE, false);
        self.auto.busy = false;
        let lock = self.auto.lock.take().unwrap_or_default();
        let source = self.auto.shared.source();
        let measurement: Option<Measurement> = self.auto.shared.take();
        if let Some(token) = self.auto.probe.take() {
            let event = match measurement.filter(|m| !m.is_empty()) {
                Some(m) => {
                    let mut data = m.numbers();
                    data["source"] = json!(source);
                    data["picture"] = json!("the camera's own picture, before Telescope's view, effects and grade change it");
                    Event::PictureMeasured { token, ok: true, message: String::new(), data }
                }
                None => Event::PictureMeasured { token, ok: false, message: "No steady picture to measure. Try again while nothing moves.".into(), data: Value::Null },
            };
            host.emit(event);
            self.say(host, String::new(), "dim");
            return;
        }
        let Some(m) = measurement.filter(|m| !m.is_empty()) else {
            self.say(host, tr(AUTO_NO_PICTURE), "warn");
            self.flash(host, AUTO_LABEL_NO_PICTURE);
            return;
        };
        let fit = auto_fit::fit(&m, Some(&self.grade));
        self.auto.last = Some((m, fit));
        self.apply_fit(host, &fit, lock);
    }

    fn apply_fit(&mut self, host: &mut dyn Host, fit: &Fit, lock: CameraLock) {
        let before = Basic::of(&self.grade);
        let (a, b) = (before, fit.values);
        let same = [a.temperature - b.temperature, a.tint - b.tint, a.lift - b.lift, a.gamma - b.gamma, a.gain - b.gain, a.contrast - b.contrast,
                    a.pivot - b.pivot, a.highlights - b.highlights, a.saturation - b.saturation].iter().all(|d| d.abs() < 1e-6);
        let mut sentences = Vec::new();
        // Auto's values replace its earlier ones outright, so a picture that needs nothing now takes them off
        let take_off = !fit.moved.any() && self.auto.wrote && !same;
        let kind = if fit.moved.any() {
            let m = fit.moved;
            let parts: Vec<String> = [m.white_balance, m.exposure, m.levels, m.highlights, m.saturation].iter().zip(AUTO_PARTS)
                .filter(|(on, _)| **on).map(|(_, p)| tr(p)).collect();
            sentences.push(tr(AUTO_CHANGED).field("parts", parts.join(", ")));
            "ok"
        } else {
            sentences.push(tr(if take_off { AUTO_TAKEN_OFF } else { AUTO_NOTHING }));
            "dim"
        };
        if !fit.had_face {
            sentences.push(tr(AUTO_NO_FACE));
        }
        if fit.wb == Wb::Conflict {
            sentences.push(tr(AUTO_CONFLICT));
        }
        if fit.backed_off {
            sentences.push(tr(AUTO_BACKED_OFF));
        }
        if lock.drifts && fit.moved.any() {
            sentences.push(tr(AUTO_DRIFTS));
        }
        // Nothing to change leaves the sliders as the person has them, and a second press with the same answer changes nothing
        if (fit.moved.any() || take_off) && !same {
            self.auto.wrote = fit.moved.any();
            fit.values.put(&mut self.grade);
            self.show_all(host);
            self.publish(host);
            host.schedule_save();
        }
        self.say(host, sentences.join(" "), kind);
        if !(fit.moved.any() || take_off) {
            self.flash(host, AUTO_LABEL_FINE);
        }
    }

    /// Frames are being looked at from now on, and the measurement has its own time to finish in.
    fn auto_released(&mut self, host: &mut dyn Host) {
        host.cancel_schedule(T_HOLD);
        host.schedule(MEASURE_TIMEOUT_MS, T_TIMEOUT);
    }

    fn hear_people(&mut self, host: &mut dyn Host, source_id: &str, people: &[Value]) {
        if self.auto.busy && self.auto.shared.source().is_some_and(|s| s == source_id) && host.focused_source_id().is_some_and(|f| f == source_id) {
            if self.auto.shared.hear_people(people.iter().filter_map(Person::from_value).collect(), (self.auto.now)()) {
                self.auto_released(host);
            }
        }
    }

    fn hear_tracks(&mut self, source_id: &str, tracks: &[Value]) {
        if self.auto.busy && self.auto.shared.source().is_some_and(|s| s == source_id) {
            let tracks = Track::all_from(tracks);
            self.auto.shared.hear_main(people::main(&tracks).filter(|t| t.missing_s == 0.0).map(|t| t.bbox));
        }
    }

    // ── Doing what the card says ──────────────────────────────────────

    fn reset(&mut self, host: &mut dyn Host) {
        // the LUT has its own way off
        let (lut, strength) = (self.grade.lut.take(), self.grade.lut_strength);
        self.grade = Grade { lut, lut_strength: strength, ..Grade::default() };
        self.auto.wrote = false;
        self.show_all(host);
        self.publish(host);
        host.schedule_save();
    }

    fn curve_changed(&mut self, host: &mut dyn Host) {
        let pts = self.editor.points.clone();
        match self.curve {
            0 => {
                let ch = self.channel;
                self.set_grade(host, |g| g.curves[ch] = pts);
            }
            1 => self.set_grade(host, |g| g.hue_sat = pts),
            _ => self.set_grade(host, |g| g.hue_hue = pts),
        }
        self.show(host, &self.curve_props());
    }

    fn edited(&mut self, host: &mut dyn Host, change: Change) {
        match change {
            Change::Points => self.curve_changed(host),
            Change::Look => self.show(host, &self.curve_props()),
            Change::None => {}
        }
    }

    fn set_on(&mut self, host: &mut dyn Host, on: bool) {
        self.on = on;
        if !on && self.lut_loading {
            // not wanted after all: the file waits for the next time
            self.drop_lut_read();
            self.lut_deferred = true;
        }
        if on && std::mem::take(&mut self.lut_deferred) {
            self.read_deferred_lut(host);
        }
        self.show(host, &json!({"on": on}));
        self.publish(host);
        host.schedule_save();
    }

    fn set_compare(&mut self, host: &mut dyn Host, on: bool) {
        self.compare = on;
        self.show(host, &self.split_props());
        host.colour_split(OWNER, on.then_some(self.split as f32 / 100.0));
    }

    fn wheel_moved(&mut self, host: &mut dyn Host, i: usize) {
        let (x, y) = self.wheels[i].value();
        self.set_grade(host, |g| g.wheels[i] = (x, y));
        self.show(host, &self.wheel_props());
    }

    fn handle_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        self.catch_up(host);
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        let b = |i: usize| event.args.get(i).and_then(Value::as_bool).unwrap_or(false);
        let s = |i: usize| event.args.get(i).and_then(Value::as_str).unwrap_or("").to_string();
        let now = self.started.elapsed().as_millis() as u64;
        let size = (n(2), n(3));
        match event.callback.as_str() {
            "switch-toggled-by-hand" => self.set_on(host, b(0)),
            "compare-toggled" => self.set_compare(host, b(0)),
            "split-moved" => {
                self.split = n(0).round() as i32;
                self.show(host, &self.split_props());
                if self.compare {
                    host.colour_split(OWNER, Some(self.split as f32 / 100.0));
                }
            }
            "page-picked" => {
                self.page = n(0) as usize;
                self.show(host, &json!({"page": self.page}));
                self.show(host, &self.curve_props());
            }
            "reset-clicked" => self.reset(host),
            "auto-clicked" => self.auto_pressed(host),
            "wheel-pressed" => self.wheels[n(0) as usize % 4].press(),
            "wheel-dragged" => {
                let i = n(0) as usize % 4;
                if self.wheels[i].drag_by(n(1), n(2), b(3)) {
                    self.wheel_moved(host, i);
                }
            }
            "wheel-released" => self.wheels[n(0) as usize % 4].release(),
            "wheel-reset" => {
                let i = n(0) as usize % 4;
                if self.wheels[i].double_click() {
                    self.wheel_moved(host, i);
                }
            }
            "wheel-key" => {
                let i = n(0) as usize % 4;
                if self.wheels[i].key(&s(1), b(2)) == Some(true) {
                    self.wheel_moved(host, i);
                }
            }
            "master-moved" => {
                let i = n(0) as usize % 4;
                let value = (n(1) * MASTER_STEPS[i]).round_ties_even() / MASTER_STEPS[i];
                self.set_grade(host, |g| g.masters[i] = value);
                self.show(host, &self.wheel_props());
            }
            "slider-moved" => {
                let i = n(0) as usize % SLIDERS.len();
                let def = &SLIDERS[i];
                let value = (n(1) * def.steps).round_ties_even() / def.steps;
                self.set_grade(host, |g| set_slider(g, def.key, value));
                self.show(host, &self.slider_props());
            }
            "curve-picked-by-hand" => {
                self.curve = (n(0) as usize).min(2);
                self.show_curve();
                self.show(host, &self.curve_props());
            }
            "channel-picked" => {
                self.channel = (n(0) as usize).min(3);
                self.show_curve();
                self.show(host, &self.curve_props());
            }
            "curve-pressed" => {
                let change = self.editor.press(size, (n(0), n(1)), b(4), now);
                // (x, y, w, h, right): the area is reported after the position
                self.edited(host, change);
            }
            "curve-moved" => {
                let change = self.editor.move_to(size, (n(0), n(1)));
                self.edited(host, change);
            }
            "curve-released" => self.editor.release(),
            "curve-double-clicked" => {
                let change = self.editor.double_click(size, (n(0), n(1)));
                self.edited(host, change);
            }
            "curve-key" => {
                if let Some(change) = self.editor.key(&s(0), b(1)) {
                    self.edited(host, change);
                }
            }
            "load-clicked" => self.choose_lut(host),
            "clear-clicked" => {
                self.load_lut(host, "");
                host.schedule_save();
            }
            "strength-moved" => {
                let v = n(0).round();
                self.set_grade(host, |g| g.lut_strength = v / 100.0);
                self.show(host, &self.lut_props());
            }
            "export-clicked" => self.export(host),
            "replace-answered" => {
                if let Some(path) = self.replace.take() {
                    if b(0) {
                        self.write_export(host, &path);
                    } else {
                        self.show(host, &self.export_props());
                    }
                }
            }
            _ => {}
        }
    }

    fn asked_done(&mut self, host: &mut dyn Host, ask: Ask, ok: bool, data: &Value) {
        if !ok {
            return;
        }
        let path = data.get("path").and_then(Value::as_str).unwrap_or("");
        if path.is_empty() {
            return;
        }
        match ask {
            Ask::Load => {
                self.load_lut(host, path);
                host.schedule_save();
            }
            Ask::Export => self.export_picked(host, path, data.get("filter").and_then(Value::as_str).unwrap_or("")),
        }
    }
}

/// The colour a puck at (x, y) adds, shown on mid grey (what the ring and the puck are painted with), as "#rrggbb".
pub fn wheel_colour(x: f64, y: f64, strength: f32) -> String {
    let shift = grade::wheel_shift(x, y);
    let c = shift.map(|v| ((0.5 + v * 0.55 * strength).clamp(0.0, 1.0) * 255.0).round() as u8);
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

impl Plugin for ColourGradingPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Colour grading").into(),
            about: n_("Colour wheels, curves and LUT files for the look of the picture.").into(),
            when_off: n_("Without the Colour grading plugin there's no Colour grading card and the picture keeps the camera's colours.").into(),
            category: "look".into(),
            icon: "colour".into(),
            // Auto reads the picture clean, before the other steps change it
            frame_stage: FrameStage::Reading,
            optional: true,
            in_presets: true,
            // each phone has its own look to fix
            device_local: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.auto.poster = host.poster();
        self.auto.ready = host.is_streaming() && host.focused_source_id().is_some();
        self.show(host, &ColourGradingPlugin::fixed_props());
        self.show_all(host);
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.publish(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.auto_abort(host);
        self.auto.poster = None;
        host.set_colour_stage(OWNER, None, 0, None);
        host.colour_split(OWNER, None);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            self.show(host, &json!({"active": on}));
        }
        match event {
            Event::PeopleSeen { source_id, people } => self.hear_people(host, source_id, people),
            Event::PeopleTracked { source_id, tracks, .. } => self.hear_tracks(source_id, tracks),
            Event::ViewChanged { view } => self.auto.shared.hear_view(Geometry::from(view)),
            Event::PictureMeasureRequested { token } => self.probe_requested(host, token),
            Event::Posted { owner, token, .. } if owner == NAME && token == POST_MEASURED => self.auto_measured(host),
            Event::Posted { owner, token, .. } if owner == NAME && token == POST_STAGE => self.stage_baked(host),
            Event::Posted { owner, token, .. } if owner == NAME && token == POST_LUT => self.lut_landed(host),
            Event::TimerFired { owner, token } if owner == NAME && token == T_LABEL => {
                self.auto.label.clear();
                self.show_auto(host);
            }
            Event::TimerFired { owner, token } if owner == NAME && token == T_TIMEOUT => self.auto_measured(host),
            Event::TimerFired { owner, token } if owner == NAME && token == T_HOLD && self.auto.busy => {
                if self.auto.shared.release() {
                    self.auto_released(host);
                }
            }
            _ => {}
        }
        if let Event::RequestDone { request, ok, data, .. } = event {
            if let Some((id, ask)) = self.asked {
                if id == *request {
                    self.asked = None;
                    self.asked_done(host, ask, *ok, data);
                }
            }
        }
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, _stream: &StreamInfo) {
        self.auto.ready = true;
        self.show_auto(host);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.auto_abort(host);
        self.auto.ready = false;
        self.show_auto(host);
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        let poster = self.auto.poster.clone()?;
        Some(FrameStep::Cpu(Box::new(MeasureStep { shared: self.auto.shared.clone(), poster })))
    }

    fn card(&mut self) -> Option<CardSpec> {
        let mut props = ColourGradingPlugin::fixed_props();
        if let (Value::Object(p), Value::Object(all)) = (&mut props, self.all_props()) {
            p.extend(all);
        }
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "ColourGradingCard".into(), props }, region: "right".into() })
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Colour grading");
        let mut out = vec![
            actions::toggle_action("colour_grading.on", &tr("Colour grading"), &group, "", &[]),
            actions::toggle_action("colour_grading.compare", &tr("Compare before and after"), &group, "", &[]),
        ];
        for key in ["temperature", "tint", "saturation", "vibrance", "contrast", "pivot", "shadows", "highlights"] {
            let def = SLIDERS.iter().find(|d| d.key == key).expect("a slider by that key");
            let (scale, decimals, suffix) = if key == "saturation" { (100.0 / def.steps, 0, "%") } else { (1.0 / def.steps, def.decimals, "") };
            let step = match key {
                "saturation" => 10.0,
                "contrast" | "pivot" => 0.05,
                _ => 5.0,
            };
            let label = tr(def.label);
            let id = format!("colour_grading.{key}");
            let spec = SliderSpec { id: &id, label: &label, group: &group, keywords: "", minimum: def.lo * def.steps * scale, maximum: def.hi * def.steps * scale,
                                    step, decimals, suffix, has_default: true };
            out.push(actions::slider_action(&spec, slider_value(&self.grade, key) * def.steps * scale));
        }
        for (i, name) in WHEELS.iter().enumerate() {
            let label = tr(MASTER_LABELS[i]);
            let id = format!("colour_grading.{name}_master");
            let (lo, hi) = MASTER_RANGE[i];
            let spec = SliderSpec { id: &id, label: &label, group: &group, keywords: "", minimum: lo, maximum: hi, step: MASTER_ACTION_STEPS[i], decimals: 2, suffix: "",
                                    has_default: true };
            out.push(actions::slider_action(&spec, self.grade.masters[i]));
        }
        let label = tr("LUT strength");
        let spec = SliderSpec { id: "colour_grading.lut_strength", label: &label, group: &group, keywords: "", minimum: 0.0, maximum: 100.0, step: 10.0, decimals: 0,
                                suffix: "%", has_default: true };
        out.push(actions::slider_action(&spec, self.grade.lut_strength * 100.0));
        out
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        // A disabled card (no phone yet) and a hidden page leave their shortcuts with nothing to do
        if !self.paired.on() {
            return None;
        }
        match id {
            "colour_grading.on" => {
                let want = actions::toggle_wanted(params, self.on, &mut self.held[0]);
                self.set_on(host, want);
                Some(actions::toggle_text(&tr("Colour grading"), want))
            }
            "colour_grading.compare" => {
                let want = actions::toggle_wanted(params, self.compare, &mut self.held[1]);
                self.set_compare(host, want);
                Some(actions::toggle_text(&tr("Compare before and after"), want))
            }
            _ => {
                let key = id.strip_prefix("colour_grading.")?;
                if key == "lut_strength" {
                    // only where the Strength slider is: the LUT page, with a LUT loaded
                    if self.page != 2 || self.lut_path.is_empty() {
                        return None;
                    }
                    let current = (self.grade.lut_strength * 100.0).round_ties_even() as i64;
                    let target = actions::slider_target(params, current, 1.0, 0, 100, Some(100))?;
                    self.set_grade(host, |g| g.lut_strength = target as f64 / 100.0);
                    self.show(host, &self.lut_props());
                    return Some(actions::slider_text(&tr("LUT strength"), &percent(target as f64, 0, false)));
                }
                if let Some(i) = WHEELS.iter().position(|w| key.strip_suffix("_master") == Some(*w)) {
                    if self.page != 0 {
                        return None;
                    }
                    let steps = MASTER_STEPS[i];
                    let (lo, hi) = MASTER_RANGE[i];
                    let current = (self.grade.masters[i] * steps).round_ties_even() as i64;
                    let default = (MASTER_DEFAULT[i] * steps).round_ties_even() as i64;
                    let target = actions::slider_target(params, current, 1.0 / steps, (lo * steps) as i64, (hi * steps) as i64, Some(default))?;
                    let value = target as f64 / steps;
                    self.set_grade(host, |g| g.masters[i] = value);
                    self.show(host, &self.wheel_props());
                    return Some(actions::slider_text(&tr(MASTER_LABELS[i]), &Self::master_text(i, value)));
                }
                let def = SLIDERS.iter().find(|d| d.key == key)?;
                if self.page != 0 {
                    return None;
                }
                let scale = if key == "saturation" { 100.0 / def.steps } else { 1.0 / def.steps };
                let current = (slider_value(&self.grade, key) * def.steps).round_ties_even() as i64;
                let default = (slider_value(&Grade::default(), key) * def.steps).round_ties_even() as i64;
                let target = actions::slider_target(params, current, scale, (def.lo * def.steps) as i64, (def.hi * def.steps) as i64, Some(default))?;
                let value = target as f64 / def.steps;
                self.set_grade(host, |g| set_slider(g, key, value));
                self.show(host, &self.slider_props());
                Some(actions::slider_text(&tr(def.label), &Self::slider_text(def, value)))
            }
        }
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        match id {
            "colour_grading.on" => {
                if let Some(before) = actions::hold_released(params, &mut self.held[0]) {
                    if before != self.on && self.paired.on() {
                        self.set_on(host, before);
                    }
                }
            }
            "colour_grading.compare" => {
                if let Some(before) = actions::hold_released(params, &mut self.held[1]) {
                    if before != self.compare && self.paired.on() {
                        self.set_compare(host, before);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui == "card" {
            self.handle_ui(host, event);
        }
    }

    fn settings(&self) -> Vec<SettingSpec> {
        let mut out = vec![SettingSpec::switch("on", "Whether the grade is applied (off shows the camera's own colours)", true)];
        for def in &SLIDERS {
            let decimals = if def.key == "saturation" { 2 } else { def.decimals };
            let default = slider_value(&Grade::default(), def.key);
            let unit = match def.key {
                "contrast" | "saturation" => "1 is unchanged",
                "pivot" => "0 black to 1 white",
                _ => "0 is unchanged",
            };
            out.push(SettingSpec::number(def.key, def.tip, def.lo, def.hi, decimals, default).unit(unit));
        }
        let masters = ["Lift master: raises or lowers the shadows, with white staying put", "Gamma master: brightens or darkens the midtones, with black and white staying put",
                       "Gain master: raises or lowers the highlights, with black staying put", "Offset master: raises or lowers everything"];
        for (i, name) in WHEELS.iter().enumerate() {
            let (lo, hi) = MASTER_RANGE[i];
            out.push(SettingSpec::number(&format!("masters.{name}"), masters[i], lo, hi, 2, MASTER_DEFAULT[i]));
        }
        out.push(SettingSpec::number("lut_strength", "How much of the loaded LUT file shows (needs a LUT loaded)", 0.0, 1.0, 2, 1.0));
        out
    }

    fn get_config(&self) -> Value {
        let g = &self.grade;
        let mut cfg = Map::new();
        cfg.insert("on".into(), json!(self.on));
        cfg.insert("wheels".into(), Value::Object(WHEELS.iter().enumerate().map(|(i, w)| (w.to_string(), json!([g.wheels[i].0, g.wheels[i].1]))).collect()));
        cfg.insert("masters".into(), Value::Object(WHEELS.iter().enumerate().map(|(i, w)| (w.to_string(), json!(g.masters[i]))).collect()));
        cfg.insert("curves".into(), Value::Object(CHANNELS.iter().enumerate().map(|(i, c)| (c.to_string(), points_json(&g.curves[i]))).collect()));
        cfg.insert("hue_sat".into(), points_json(&g.hue_sat));
        cfg.insert("hue_hue".into(), points_json(&g.hue_hue));
        cfg.insert("lut".into(), json!(self.lut_path));
        cfg.insert("lut_strength".into(), json!(g.lut_strength));
        for def in &SLIDERS {
            // Vibrance came later: a grade without it saves exactly what older versions did
            if def.key == "vibrance" && g.vibrance == 0.0 {
                continue;
            }
            cfg.insert(def.key.into(), json!(slider_value(g, def.key)));
        }
        Value::Object(cfg)
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let empty = Value::Object(Map::new());
        let cfg = if cfg.is_object() { cfg } else { &empty };
        let dict = |key: &str| cfg.get(key).filter(|v| v.is_object()).unwrap_or(&empty).clone();
        let (wheels_raw, masters_raw, curves_raw) = (dict("wheels"), dict("masters"), dict("curves"));
        let mut g = Grade::default();
        for (i, name) in WHEELS.iter().enumerate() {
            let (x, y) = match wheels_raw.get(name) {
                Some(Value::Array(p)) if p.len() == 2 => (number_in(p.first(), 0.0, -1.0, 1.0), number_in(p.get(1), 0.0, -1.0, 1.0)),
                _ => (0.0, 0.0),
            };
            let r = x.hypot(y);
            // on the rim, as the wheel shows it
            g.wheels[i] = if r > 1.0 { (x / r, y / r) } else { (x, y) };
            g.masters[i] = number_in(masters_raw.get(name), MASTER_DEFAULT[i], MASTER_RANGE[i].0, MASTER_RANGE[i].1);
        }
        let defaults = Grade::default();
        for def in &SLIDERS {
            set_slider(&mut g, def.key, number_in(cfg.get(def.key), slider_value(&defaults, def.key), def.lo, def.hi));
        }
        for (i, c) in CHANNELS.iter().enumerate() {
            g.curves[i] = points(curves_raw.get(c), &LINEAR, true);
        }
        let neutral = grade::neutral_hue();
        g.hue_sat = points(cfg.get("hue_sat"), &neutral, false);
        g.hue_hue = points(cfg.get("hue_hue"), &neutral, false);
        g.lut_strength = number_in(cfg.get("lut_strength"), 1.0, 0.0, 1.0);
        let path = cfg.get("lut").and_then(Value::as_str).unwrap_or("").to_string();
        self.on = cfg.get("on") != Some(&Value::Bool(false));
        // Off: the file waits until the effect is switched on, so a LUT nobody uses costs no parse or memory
        self.drop_lut_read();
        self.lut_deferred = !self.on && !path.is_empty();
        let (lut, problem) = if path.is_empty() || self.lut_deferred { (None, String::new()) } else { self.read_lut(&path) };
        g.lut = lut;
        self.lut_problem = problem;
        self.lut_path = path;
        self.grade = g;
        self.auto.wrote = false;
        // once, with the whole grade, so the stream never sees half of it: the next hook with a host does it
        self.dirty = true;
        Ok(())
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.set_config(cfg)?;
        self.show_all(host);
        self.publish(host);
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let text = if !self.on {
            "off".to_string()
        } else if self.grade.is_identity() {
            "neutral".to_string()
        } else {
            format!("on{}", if self.grade.lut.is_some() { ", with a LUT" } else { "" })
        };
        vec![("Colour grading".into(), text)]
    }
}

#[cfg(test)]
#[path = "colour_grading_tests.rs"]
mod tests;
