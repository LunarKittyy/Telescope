//! The Camera card (plugins/camera_control.py): lenses, exposure, focus and image settings sent to the phone. The card's
//! widgets live here as plain state, changed the way Qt changes them (a value set while signals flow runs its handler),
//! and camera.slint only draws them; its callbacks come back as UiEvents and the settings go out through Host::phone_control.

use serde_json::{json, Value};
use telescope_core::actions::{self, Choice, LogSliderSpec, SliderSpec};
use telescope_core::event::Event;
use telescope_core::i18n::{n_, number, tr, tr_ctx};
use telescope_core::lens::{lens_label, lens_name, shorten_lens_label};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo, StreamsBehind};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::{FrameStage, Host};

const FOCUS_STEPS: i64 = 1000;
/// LogSliderRow.STEPS
const LOG_STEPS: f64 = 2000.0;
/// (English label, the phone's value) of the noise reduction and sharpening modes.
const MODES: [(&str, i64); 3] = [(n_("Off"), 0), (n_("Fast"), 1), (n_("High Quality"), 2)];
const LOW_LIGHT_TIP: &str = n_("Brightens a dark room when exposure is automatic but can drop the frame rate as low as 10 fps.");
const CAPABILITY_LABELS: [(&str, &str); 5] = [
    ("supportsManualSensor", n_("Manual exposure")),
    ("supportsManualWB", n_("Manual WB")),
    ("supportsManualFocus", n_("Manual focus")),
    ("supportsFocusPoint", n_("Point focus")),
    ("hasOis", "OIS"),
];
/// Camera2's INFO_SUPPORTED_HARDWARE_LEVEL constants, in English.
const HW_LEVELS: [(&str, &str); 5] =
    [("LEGACY", n_("Legacy")), ("LIMITED", n_("Limited")), ("FULL", n_("Full")), ("LEVEL_3", n_("Level 3")), ("EXTERNAL", n_("External"))];
const WB_MIN_K: i64 = 2000;
const WB_MAX_K: i64 = 10000;
/// The temperature where the red and blue gains are equal.
const WB_NEUTRAL: i64 = 5500;
/// Values the segmented rows keep in saved shortcuts, whatever their text says in a language.
const EXPOSURE_VALUES: [&str; 2] = ["Auto", "Manual"];
const FOCUS_VALUES: [&str; 3] = ["Auto", "Point", "Manual"];

/// _kelvin_to_rggb: Camera2 channel gains (red, both greens, blue) for a temperature and a tint from -150 (green) to +150 (magenta).
pub fn kelvin_to_rggb(kelvin: f64, tint: f64) -> (f64, f64, f64, f64) {
    let t = ((kelvin - WB_NEUTRAL as f64) / WB_NEUTRAL as f64).clamp(-1.0, 1.0);
    let r = (2.0 * 2f64.powf(t)).max(0.3);
    let b = (2.0 * 2f64.powf(-t)).max(0.3);
    let g = (1.0 - tint / 500.0).clamp(0.5, 2.5);
    (r, g, g, b)
}

/// _diopters_to_label: a focus distance in metres, or infinity.
pub fn diopters_to_label(d: f64) -> String {
    if d <= 0.01 {
        return "∞".into(); // focused at infinity: the same in every language
    }
    tr("{n} m").replace("{n}", &number(1.0 / d, 2, false))
}

/// ns_to_display: an exposure time as the shutter row shows it.
pub fn ns_to_display(ns: i64) -> String {
    if ns <= 0 {
        return "?".into();
    }
    let s = ns as f64 / 1e9;
    if s >= 1.0 {
        return tr("{n} s").replace("{n}", &number(s, 1, false));
    }
    let denom = actions::round_half_even(1.0 / s) as i64;
    let digits = denom.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("1/{grouped}")
}

fn ev_text(ev: f64) -> String {
    tr("{n} EV").replace("{n}", &number(ev, 1, true))
}

fn signed(v: i64) -> String {
    format!("{v:+}")
}

/// Python's truthiness, for the flags a config or the phone's state holds.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn get_bool(map: &Value, key: &str, default: bool) -> bool {
    map.get(key).map_or(default, truthy)
}

fn get_f64(map: &Value, key: &str, default: f64) -> f64 {
    map.get(key).and_then(Value::as_f64).unwrap_or(default)
}

/// `cam.get(key) is True`, as the point checks read the phone's flags.
fn is_true(cam: &Value, key: &str) -> bool {
    cam.get(key) == Some(&Value::Bool(true))
}

fn mode_index(value: Option<f64>) -> usize {
    value.and_then(|v| MODES.iter().position(|(_, m)| *m as f64 == v)).unwrap_or(1)
}

/// lens_label(cam, compact=False): the name saved shortcuts pick a lens by.
fn lens_value(cam: &Value) -> String {
    match cam.get("facing").and_then(Value::as_str) {
        Some("back" | "front" | "external") => lens_label(cam),
        _ => shorten_lens_label(cam.get("label").and_then(Value::as_str).unwrap_or(""), false),
    }
}

/// _columns: up to five tiles in a row; more lenses split into rows as even as they can be.
pub fn lens_columns(count: usize) -> usize {
    if count == 0 {
        return 1;
    }
    let rows = count.div_ceil(5);
    count.div_ceil(rows).max(1)
}

/// split_lens_label: (number, unit, the rest) of a shortened label, "Auto ~24mm OIS" being ("24", "mm", "Auto · OIS").
pub fn split_lens_label(label: &str) -> (String, String, String) {
    let chars: Vec<char> = label.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let mut j = i;
        if chars[j] == '~' {
            j += 1;
        }
        let digits_from = j;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        if j > digits_from {
            let mut end = j;
            if j + 1 < chars.len() && chars[j] == '.' && chars[j + 1].is_ascii_digit() {
                end = j + 1;
                while end < chars.len() && chars[end].is_ascii_digit() {
                    end += 1;
                }
            }
            let mut k = end;
            while k < chars.len() && chars[k].is_whitespace() {
                k += 1;
            }
            let rest: String = chars[k..].iter().collect();
            let unit = if rest.starts_with("mm") { "mm" } else if rest.starts_with('x') { "x" } else { "" };
            let after = k + unit.len();
            // \b after the unit: not followed by a word character
            if !unit.is_empty() && chars.get(after).is_none_or(|c| !(c.is_alphanumeric() || *c == '_')) {
                let number: String = chars[digits_from..end].iter().collect();
                let before: String = chars[..start].iter().collect();
                let tail: String = chars[after..].iter().collect();
                let words = lens_name(&format!("{before} {tail}"));
                return (number, unit.into(), words.split_whitespace().collect::<Vec<_>>().join(" · "));
            }
        }
        i += 1;
    }
    (label.into(), String::new(), String::new())
}

/// Everything on_phone_state needs, worked out from the phone's state alone (CameraControlView).
#[derive(Clone, Debug, PartialEq)]
pub struct CameraControlView {
    pub lenses: Vec<Value>,
    pub current_camera: Option<Value>,
    pub manual_exposure: bool,
    pub iso: Option<f64>,
    pub shutter_ns: Option<f64>,
    pub manual_wb: bool,
    pub ois: bool,
    pub manual_focus: bool,
    pub focus_distance: f64,
    pub point_focus: bool,
    pub ae_comp: i64,
    pub ae_comp_step: f64,
    pub ae_comp_range: (i64, i64),
    pub nr_mode_index: usize,
    pub edge_mode_index: usize,
    pub black_level_lock: bool,
    pub torch: bool,
    pub low_light: bool,
    /// The phone takes low_light (older apps don't).
    pub low_light_available: bool,
}

/// derive_camera_control_view: None for an empty state, so the card says "Unavailable" instead.
pub fn derive_camera_control_view(state: &Value) -> Option<CameraControlView> {
    if !truthy(state) {
        return None;
    }
    let cameras = state.get("cameras").and_then(Value::as_array).cloned().unwrap_or_default();
    let cur = cameras.iter().find(|c| c.get("current").is_some_and(truthy)).cloned();
    let range = |key: &str, default: i64| cur.as_ref().and_then(|c| c.get(key)).and_then(Value::as_f64).map_or(default, |v| v as i64);
    let focus_mode = state.get("focus_mode").and_then(Value::as_str);
    Some(CameraControlView {
        ae_comp_range: (range("aeCompMin", -8), range("aeCompMax", 8)),
        ae_comp_step: cur.as_ref().map_or(0.167, |c| get_f64(c, "aeCompStep", 0.167)),
        lenses: cameras,
        current_camera: cur,
        manual_exposure: !get_bool(state, "auto", true),
        iso: state.get("iso").and_then(Value::as_f64),
        shutter_ns: state.get("shutter_ns").and_then(Value::as_f64),
        manual_wb: get_bool(state, "wb_manual", false),
        ois: get_bool(state, "ois", true),
        manual_focus: focus_mode.unwrap_or("continuous") == "manual",
        focus_distance: get_f64(state, "focus_distance", 0.0),
        point_focus: focus_mode == Some("point"),
        ae_comp: get_f64(state, "ae_comp", 0.0) as i64,
        nr_mode_index: mode_index(Some(get_f64(state, "nr_mode", 1.0))),
        edge_mode_index: mode_index(Some(get_f64(state, "edge_mode", 1.0))),
        black_level_lock: get_bool(state, "black_level_lock", false),
        torch: get_bool(state, "torch", false),
        low_light: get_bool(state, "low_light", false),
        low_light_available: get_bool(state, "low_light_toggle", false),
    })
}

/// LogSliderRow: a log-scale slider with a box beside it that shows the value scaled (nanoseconds as milliseconds).
#[derive(Clone, Debug)]
struct LogRow {
    v_min: f64,
    v_max: f64,
    pos: f64,
    /// What the box holds, in its own units.
    spin: f64,
    /// What the readout says: the value last given (set_value), or where a hand put the slider.
    shown: f64,
    enabled: bool,
    spin_scale: f64,
    /// The box takes decimals (QDoubleSpinBox) rather than whole numbers.
    double: bool,
    shutter: bool,
}

impl LogRow {
    fn new(v_min: f64, v_max: f64, spin_scale: f64, double: bool, shutter: bool) -> LogRow {
        LogRow { v_min, v_max, pos: 0.0, spin: v_min * spin_scale, shown: v_min, enabled: false, spin_scale, double, shutter }
    }

    fn to_spin(&self, val: f64) -> f64 {
        let (lo, hi) = (self.v_min * self.spin_scale, self.v_max * self.spin_scale);
        let sv = val * self.spin_scale;
        let sv = if self.double { (sv * 100.0).round() / 100.0 } else { actions::round_half_even(sv) };
        sv.max(lo.min(hi)).min(hi.max(lo))
    }

    fn get_value(&self) -> f64 {
        actions::log_pos_to_val(self.pos, LOG_STEPS, self.v_min, self.v_max)
    }

    /// get_value as saved: an end of a whole-number range comes back as that number, which Python's max and min hand on as it is.
    fn saved_number(&self) -> Value {
        let value = self.get_value();
        if (value == self.v_min || value == self.v_max) && value.fract() == 0.0 { json!(value as i64) } else { json!(value) }
    }

    fn set_value(&mut self, val: f64) {
        self.pos = actions::val_to_log_pos(val, LOG_STEPS, self.v_min, self.v_max);
        self.spin = self.to_spin(val);
        self.shown = val;
    }

    /// Keeps the value, not the slider position, clamped to the new range.
    fn set_range(&mut self, v_min: f64, v_max: f64) {
        let cur = self.get_value();
        self.v_min = v_min;
        self.v_max = v_max;
        self.set_value(cur.max(v_min).min(v_min.max(v_max)));
    }

    fn text(&self) -> String {
        let v = self.shown;
        if self.shutter { ns_to_display(actions::round_half_even(v) as i64) } else { format!("ISO {}", actions::round_half_even(v) as i64) }
    }
}

/// A QSlider: a whole-number value inside its range.
#[derive(Clone, Debug)]
struct IntSlider {
    min: i64,
    max: i64,
    value: i64,
    enabled: bool,
}

impl IntSlider {
    /// setValue: true when the value changed (and valueChanged would fire).
    fn set(&mut self, v: i64) -> bool {
        let v = v.clamp(self.min, self.max);
        let changed = v != self.value;
        self.value = v;
        changed
    }

    fn set_range(&mut self, min: i64, max: i64) -> bool {
        self.min = min;
        self.max = max.max(min);
        let v = self.value;
        self.set(v)
    }
}

/// A switch tile: its check, whether it can be used, its tooltip and whether it shows.
#[derive(Clone, Debug)]
struct Tile {
    checked: bool,
    enabled: bool,
    tip: String,
    visible: bool,
}

impl Tile {
    fn new(checked: bool) -> Tile {
        Tile { checked, enabled: true, tip: String::new(), visible: true }
    }
}

/// A row of buttons only one of which is checked.
#[derive(Clone, Debug)]
struct Segments {
    index: usize,
    enabled: Vec<bool>,
    tips: Vec<String>,
}

impl Segments {
    fn new(n: usize) -> Segments {
        Segments { index: 0, enabled: vec![true; n], tips: vec![String::new(); n] }
    }
}

/// LensPanel: one tile per lens, or a line of text in their place.
#[derive(Clone, Debug)]
struct Lenses {
    cameras: Vec<Value>,
    checked: Option<usize>,
    placeholder: String,
}

impl Lenses {
    fn new() -> Lenses {
        Lenses { cameras: Vec::new(), checked: None, placeholder: tr("Start streaming to load lenses") }
    }

    fn load(&mut self, cameras: &[Value]) {
        if cameras.is_empty() {
            self.clear();
            self.placeholder = tr("No cameras available");
            return;
        }
        self.cameras = cameras.to_vec();
        self.checked = cameras.iter().position(|c| c.get("current").is_some_and(truthy));
    }

    /// select_id: checks the lens with this id without saying so, and gives its entry (None if this phone has none).
    fn select_id(&mut self, id: &str) -> Option<Value> {
        let i = self.cameras.iter().position(|c| c.get("id").and_then(Value::as_str) == Some(id))?;
        self.checked = Some(i);
        Some(self.cameras[i].clone())
    }

    /// Shows only while there are no tiles.
    fn set_placeholder(&mut self, text: String) {
        self.placeholder = text;
    }

    fn clear(&mut self) {
        self.cameras.clear();
        self.checked = None;
        self.placeholder = tr("Start streaming to load lenses");
    }
}

pub struct CameraControl {
    phones: u32,
    /// A source that isn't a phone is picked: the card goes away then (dim_until_paired with phone_only).
    source: String,
    /// The stream the panels follow, by its url (Python's ctrl): None while idle.
    stream: Option<String>,
    /// StreamInfo::session of that stream.
    session: Option<u64>,
    lenses: Lenses,
    info_text: String,
    info_tip: String,
    exp: Segments,
    iso: LogRow,
    shutter: LogRow,
    ae: IntSlider,
    ae_text: String,
    ae_step: f64,
    wb: Segments,
    kelvin: IntSlider,
    kelvin_text: String,
    tint: IntSlider,
    tint_text: String,
    focus: Segments,
    focus_slider: IntSlider,
    focus_text: String,
    focus_max_diopters: f64,
    ois: Tile,
    low_light: Tile,
    bll: Tile,
    torch: Tile,
    nr: usize,
    edge: usize,
    manual_exp: bool,
    manual_wb: bool,
    manual_focus: bool,
    point_focus: bool,
    /// What focus_mode_changed last said.
    published_focus: Option<String>,
    lens_id: Option<String>,
    /// A preset's lens, applied while idle, for the next stream.
    pending_lens: Option<String>,
    /// The stream's latest state, for its lenses and ranges.
    last_state: Option<Value>,
    /// What set_config() was last given, before the sliders' ranges clamped it.
    config_in: Value,
    /// Each stream the panels moved away from: its last state and whether it focused on a point.
    behind: StreamsBehind<(Value, bool)>,
    /// The lens takes meter_point.
    meter_ok: bool,
    torch_on: bool,
    /// Where each switch was when a held key began (stabilization, black level lock, low light, torch).
    held: [Option<bool>; 4],
    /// What the handlers sent and said since the host last heard (refresh hands it over).
    out: Outbox,
}

/// Bodies for the phone, events for the bus and saves asked for, in order.
#[derive(Default)]
struct Outbox {
    bodies: Vec<Value>,
    events: Vec<Event>,
    saves: u32,
}

impl Default for CameraControl {
    fn default() -> Self {
        CameraControl::new()
    }
}

impl CameraControl {
    pub fn new() -> CameraControl {
        let mut focus = Segments::new(3);
        focus.enabled[1] = false;
        CameraControl {
            phones: 0,
            source: String::new(),
            stream: None,
            session: None,
            lenses: Lenses::new(),
            info_text: String::new(),
            info_tip: String::new(),
            exp: Segments::new(2),
            iso: LogRow::new(50.0, 6400.0, 1.0, false, false),
            shutter: LogRow::new(100_000.0, 1_000_000_000.0, 1e-6, true, true),
            ae: IntSlider { min: -8, max: 8, value: 0, enabled: true },
            ae_text: "0.0 EV".into(),
            ae_step: 0.167,
            wb: Segments::new(2),
            kelvin: IntSlider { min: WB_MIN_K, max: WB_MAX_K, value: WB_NEUTRAL, enabled: false },
            kelvin_text: format!("{WB_NEUTRAL} K"),
            tint: IntSlider { min: -150, max: 150, value: 0, enabled: false },
            tint_text: "+0".into(),
            focus,
            focus_slider: IntSlider { min: 0, max: FOCUS_STEPS, value: 0, enabled: false },
            focus_text: "∞".into(),
            focus_max_diopters: 10.0,
            ois: Tile { tip: tr("Optical image stabilization"), ..Tile::new(true) },
            low_light: Tile { tip: tr(LOW_LIGHT_TIP), visible: false, ..Tile::new(false) },
            bll: Tile::new(false),
            torch: Tile::new(false),
            nr: 1,
            edge: 1,
            manual_exp: false,
            manual_wb: false,
            manual_focus: false,
            point_focus: false,
            published_focus: None,
            lens_id: None,
            pending_lens: None,
            last_state: None,
            config_in: json!({}),
            behind: StreamsBehind::default(),
            meter_ok: false,
            torch_on: false,
            held: [None; 4],
            out: Outbox::default(),
        }
    }

    fn send(&mut self, body: Value) {
        if self.stream.is_some() {
            self.out.bodies.push(body);
        }
    }

    fn card_on(&self) -> bool {
        self.phones > 0 || !self.source.is_empty()
    }

    // ── Widgets changed as Qt changes them: a new value runs its handler unless signals are blocked ──

    fn set_exp(&mut self, manual: bool) {
        self.exp.index = usize::from(manual);
    }

    fn set_wb_mode(&mut self, manual: bool) {
        self.wb.index = usize::from(manual);
    }

    fn set_focus_index(&mut self, i: usize) {
        self.focus.index = i;
        self.publish_focus_mode(false); // buttonToggled
    }

    fn set_ae(&mut self, v: i64, blocked: bool) {
        if self.ae.set(v) && !blocked {
            let v = self.ae.value;
            self.on_ae_comp_changed(v);
        }
    }

    fn set_tint(&mut self, v: i64) {
        if self.tint.set(v) {
            let v = self.tint.value;
            self.on_tint_changed(v);
        }
    }

    fn set_kelvin(&mut self, v: i64) {
        if self.kelvin.set(v) {
            let v = self.kelvin.value;
            self.on_wb_changed(v);
        }
    }

    fn set_ois(&mut self, on: bool) {
        if self.ois.checked != on {
            self.ois.checked = on;
            self.on_ois(on);
        }
    }

    fn set_bll(&mut self, on: bool, blocked: bool) {
        if self.bll.checked != on {
            self.bll.checked = on;
            if !blocked {
                self.on_bll_changed(on);
            }
        }
    }

    fn set_low_light(&mut self, on: bool, blocked: bool) {
        if self.low_light.checked != on {
            self.low_light.checked = on;
            if !blocked {
                self.on_low_light(on);
            }
        }
    }

    fn set_torch(&mut self, on: bool, blocked: bool) {
        if self.torch.checked != on {
            self.torch.checked = on;
            if !blocked {
                self.on_torch_toggled(on);
            }
        }
    }

    fn set_nr(&mut self, i: usize, blocked: bool) {
        if self.nr != i {
            self.nr = i;
            if !blocked {
                self.on_nr_mode_changed(i);
            }
        }
    }

    fn set_edge(&mut self, i: usize, blocked: bool) {
        if self.edge != i {
            self.edge = i;
            if !blocked {
                self.on_edge_mode_changed(i);
            }
        }
    }

    fn set_manual_exp_widgets(&mut self, manual: bool) {
        self.manual_exp = manual;
        self.iso.enabled = manual;
        self.shutter.enabled = manual;
    }

    fn set_manual_wb_widgets(&mut self, manual: bool) {
        self.manual_wb = manual;
        self.kelvin.enabled = manual;
        self.tint.enabled = manual;
    }

    // ── Stream lifecycle ──

    fn start(&mut self, url: &str) {
        self.stream = Some(url.to_string());
        self.lenses.set_placeholder(tr("Loading lenses..."));
        self.push_settings_to_phone();
        self.publish_focus_mode(true);
    }

    /// Re-applies what the card holds to the phone (a new stream doesn't know the saved settings).
    fn push_settings_to_phone(&mut self) {
        if self.manual_exp {
            self.send(json!({"action": "iso", "value": self.iso.get_value() as i64}));
            self.send(json!({"action": "shutter", "value": self.shutter.get_value() as i64}));
        } else {
            self.send(json!({"action": "auto"}));
        }
        if self.manual_wb {
            self.send_wb_gains();
        } else {
            self.send(json!({"action": "wb_auto"}));
        }
        self.send(json!({"action": "ois", "value": if self.ois.checked { "1" } else { "0" }}));
        if self.manual_focus {
            self.send(json!({"action": "focus_mode", "value": "manual"}));
            self.send(json!({"action": "focus_distance", "value": self.slider_to_diopters(self.focus_slider.value)}));
        } else {
            self.send(json!({"action": "focus_mode", "value": "continuous"}));
        }
        self.send(json!({"action": "ae_comp", "value": self.ae.value}));
        self.send(json!({"action": "nr_mode", "value": MODES[self.nr].1}));
        self.send(json!({"action": "edge_mode", "value": MODES[self.edge].1}));
        self.send(json!({"action": "black_level_lock", "value": if self.bll.checked { "1" } else { "0" }}));
        if self.low_light.checked {
            // off is every stream's start, and an older phone doesn't know it
            self.send(json!({"action": "low_light", "value": "1"}));
        }
    }

    fn leave_stream(&mut self) {
        self.last_state = None;
        self.stream = None;
        self.session = None;
        self.set_point_available(false, Some(false));
        if self.point_focus {
            // the point was for that session; the next one starts continuous
            self.point_focus = false;
            self.set_focus_index(0);
        }
        self.lenses.clear();
        self.info_text.clear();
        self.info_tip.clear();
        self.place_switch_tiles(false); // the next phone says whether it takes it
    }

    fn place_switch_tiles(&mut self, low_light: bool) {
        self.low_light.visible = low_light;
    }

    fn phone_state(&mut self, state: &Value) {
        let Some(view) = derive_camera_control_view(state) else {
            self.lenses.set_placeholder(tr("Unavailable"));
            return;
        };
        self.last_state = Some(state.clone());
        self.lenses.load(&view.lenses);
        let cur = view.current_camera.clone();
        let lens = self.pending_lens.take();
        let cur_id = cur.as_ref().and_then(|c| c.get("id")).and_then(Value::as_str).map(str::to_string);
        let cam = match (&lens, &cur) {
            (Some(lens), Some(_)) if Some(lens) != cur_id.as_ref() => self.lenses.select_id(lens),
            _ => None,
        };
        if let (Some(cam), true) = (&cam, self.stream.is_some()) {
            self.on_lens_selected(cam);
            self.set_point_available(is_true(cam, "supportsFocusPoint"), Some(is_true(cam, "supportsMeterPoint")));
            self.push_settings_to_phone();
            return; // the widgets already hold the preset; the phone's next state comes from the new lens
        }
        if let Some(cur) = &cur {
            self.lens_id = cur.get("id").and_then(Value::as_str).map(str::to_string);
            self.set_lens_ranges(cur, view.ae_comp_step, view.ae_comp_range);
            self.set_point_available(is_true(cur, "supportsFocusPoint"), Some(is_true(cur, "supportsMeterPoint")));
        }

        self.set_exp(view.manual_exposure);
        self.set_manual_exp_widgets(view.manual_exposure);
        if let Some(iso) = view.iso.filter(|v| *v != 0.0) {
            self.iso.set_value(iso);
        }
        if let Some(ns) = view.shutter_ns.filter(|v| *v != 0.0) {
            self.shutter.set_value(ns);
        }

        self.set_wb_mode(view.manual_wb);
        self.set_manual_wb_widgets(view.manual_wb);

        self.set_ois(view.ois);

        let focus = if view.manual_focus { 2 } else if view.point_focus { 1 } else { 0 };
        self.set_focus_index(focus);
        self.manual_focus = view.manual_focus;
        self.point_focus = view.point_focus;
        self.focus_slider.enabled = view.manual_focus;
        self.set_focus_slider_value(view.focus_distance);

        self.set_ae(view.ae_comp, true);
        self.ae_text = ev_text(view.ae_comp as f64 * self.ae_step);
        self.set_nr(view.nr_mode_index, true);
        self.set_edge(view.edge_mode_index, true);
        self.set_bll(view.black_level_lock, true);
        self.set_torch(view.torch, true);
        self.torch_on = view.torch;

        self.place_switch_tiles(view.low_light_available);
        if view.low_light_available {
            self.set_low_light(view.low_light, true);
        }
    }

    /// The lens's ranges and what it can do, from its entry in the phone's state.
    fn set_lens_ranges(&mut self, cam: &Value, ae_step: f64, ae_range: (i64, i64)) {
        self.iso.set_range(get_f64(cam, "isoMin", 50.0), get_f64(cam, "isoMax", 6400.0));
        self.shutter.set_range(get_f64(cam, "shutterMinNs", 100_000.0), get_f64(cam, "shutterMaxNs", 1_000_000_000.0));
        self.ae_step = ae_step;
        self.ae.set_range(ae_range.0, ae_range.1); // signals blocked
        self.update_cam_info(cam);
        self.update_camera_caps(Caps::of(cam));
    }

    // ── What the lens can do ──

    /// "Full manual control" in the header when the lens has it; the tooltip lists what it has and lacks.
    fn update_cam_info(&mut self, cam: &Value) {
        let supported: Vec<(String, bool)> =
            CAPABILITY_LABELS.iter().map(|(key, label)| (tr(label), cam.get(*key).is_some_and(truthy))).collect();
        let raw = cam.get("hwLevel").and_then(Value::as_str).unwrap_or("");
        let hw_level = HW_LEVELS.iter().find(|(k, _)| *k == raw).map_or_else(|| tr(raw), |(_, l)| tr(l));
        let have: Vec<&str> = supported.iter().filter(|(_, ok)| *ok).map(|(l, _)| l.as_str()).collect();
        let missing: Vec<&str> = supported.iter().filter(|(_, ok)| !*ok).map(|(l, _)| l.as_str()).collect();
        let full = ["supportsManualSensor", "supportsManualWB", "supportsManualFocus"].iter().all(|k| cam.get(*k).is_some_and(truthy));
        self.info_text = if full { tr("Full manual control") } else { tr("Limited manual control") };
        let mut lines = Vec::new();
        if !hw_level.is_empty() {
            lines.push(tr("Camera level: {level}").replace("{level}", &hw_level));
        }
        if !have.is_empty() {
            lines.push(tr("Supports: {features}").replace("{features}", &have.join(", ")));
        }
        if !missing.is_empty() {
            lines.push(tr("Not supported: {features}").replace("{features}", &missing.join(", ")));
        }
        self.info_tip = lines.join("\n");
    }

    fn update_camera_caps(&mut self, caps: Caps) {
        self.exp.enabled[1] = caps.manual_sensor;
        if !caps.manual_sensor {
            self.set_exp(false);
            self.set_manual_exp_widgets(false);
            self.exp.tips[1] = tr("This lens has no manual exposure");
        } else {
            self.exp.tips[1].clear();
        }

        self.wb.enabled[1] = caps.manual_wb;
        if !caps.manual_wb {
            self.set_wb_mode(false);
            self.set_manual_wb_widgets(false);
            self.wb.tips[1] = tr("This lens has no manual white balance");
        } else {
            self.wb.tips[1].clear();
        }

        let focus = self.slider_to_diopters(self.focus_slider.value);
        self.focus_max_diopters = if caps.min_focus_distance > 0.0 { caps.min_focus_distance } else { 10.0 };
        self.set_focus_slider_value(focus.min(self.focus_max_diopters)); // same distance on the new scale
        self.focus.enabled[2] = caps.manual_focus;
        if !caps.manual_focus {
            if self.focus.index == 2 {
                self.set_focus_index(0);
            }
            self.manual_focus = false;
            self.focus_slider.enabled = false;
            self.focus.tips[2] = tr("This lens has no manual focus");
        } else {
            self.focus.tips[2].clear();
        }

        self.torch.enabled = caps.flash;
        self.torch.tip = if caps.flash { String::new() } else { tr("This lens has no flash to use as a torch") };
        // Greyed out but stays checked across lens switches
        self.ois.enabled = caps.ois;
        self.ois.tip = if caps.ois { tr("Optical image stabilization") } else { tr("This lens has no optical stabilization") };
    }

    // ── Handlers ──

    fn on_lens_selected(&mut self, cam: &Value) {
        if self.stream.is_none() {
            return;
        }
        let id = cam.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        self.lens_id = Some(id.clone());
        self.send(json!({"action": "camera", "id": id}));
        self.out.events.push(Event::CameraSwitched { camera: cam.clone() });
        let range = |k: &str, d: i64| cam.get(k).and_then(Value::as_f64).map_or(d, |v| v as i64);
        self.set_lens_ranges(cam, get_f64(cam, "aeCompStep", 0.167), (range("aeCompMin", -8), range("aeCompMax", 8)));
    }

    fn on_exp_mode(&mut self) {
        let manual = self.exp.index == 1;
        self.set_manual_exp_widgets(manual);
        if manual {
            self.send(json!({"action": "iso", "value": self.iso.get_value() as i64}));
            self.send(json!({"action": "shutter", "value": self.shutter.get_value() as i64}));
        } else {
            self.send(json!({"action": "auto"}));
        }
        self.out.saves += 1;
    }

    fn on_iso_changed(&mut self, val: f64) {
        if self.manual_exp {
            self.send(json!({"action": "iso", "value": val as i64}));
        }
        self.out.saves += 1;
    }

    fn on_shutter_changed(&mut self, val: f64) {
        if self.manual_exp {
            self.send(json!({"action": "shutter", "value": val as i64}));
        }
        self.out.saves += 1;
    }

    fn send_wb_gains(&mut self) {
        let (r, ge, go, b) = kelvin_to_rggb(self.kelvin.value as f64, self.tint.value as f64);
        self.send(wb_gains_body(r, ge, go, b));
    }

    fn on_wb_mode(&mut self) {
        let manual = self.wb.index == 1;
        self.set_manual_wb_widgets(manual);
        if manual {
            self.send_wb_gains();
        } else {
            self.send(json!({"action": "wb_auto"}));
        }
        self.out.saves += 1;
    }

    fn on_wb_changed(&mut self, k: i64) {
        self.kelvin_text = format!("{k} K");
        if self.manual_wb {
            self.send_wb_gains();
        }
        self.out.saves += 1;
    }

    fn on_tint_changed(&mut self, t: i64) {
        self.tint_text = signed(t);
        if self.manual_wb {
            self.send_wb_gains();
        }
        self.out.saves += 1;
    }

    fn on_ois(&mut self, on: bool) {
        self.send(json!({"action": "ois", "value": if on { "1" } else { "0" }}));
        self.out.saves += 1;
    }

    fn set_point_available(&mut self, available: bool, meter: Option<bool>) {
        if let Some(meter) = meter {
            self.meter_ok = meter;
        }
        self.focus.enabled[1] = available;
        self.focus.tips[1] = if available {
            tr("Click the preview to focus there")
        } else if self.stream.is_some() {
            tr("This lens can't focus on a point")
        } else {
            String::new()
        };
        if !available && self.point_focus {
            self.point_focus = false;
            self.set_focus_index(0);
        }
        let streaming = self.stream.is_some();
        self.out.events.push(Event::FocusPointAvailable { available: available && streaming });
        self.out.events.push(Event::MeterPointAvailable { available: self.meter_ok && streaming });
    }

    /// A point in the phone's frame, picked on the preview (or the centre, from the Point button).
    fn on_focus_point(&mut self, x: f64, y: f64) {
        if self.stream.is_none() || !self.focus.enabled[1] {
            return;
        }
        self.send(point_body("focus_point", x, y));
        self.show_point_focus();
    }

    /// A point to meter on, leaving focus and the Focus buttons as they are.
    fn on_meter_point(&mut self, x: f64, y: f64) {
        if self.meter_ok {
            self.send(point_body("meter_point", x, y));
        }
    }

    fn show_point_focus(&mut self) {
        self.point_focus = true;
        self.manual_focus = false;
        self.set_focus_index(1);
        self.focus_slider.enabled = false;
    }

    fn focus_mode(&self) -> &'static str {
        ["auto", "point", "manual"][self.focus.index]
    }

    fn publish_focus_mode(&mut self, again: bool) {
        let mode = self.focus_mode();
        if again || self.published_focus.as_deref() != Some(mode) {
            self.published_focus = Some(mode.into());
            self.out.events.push(Event::FocusModeChanged { mode: mode.into() });
        }
    }

    fn on_focus_mode(&mut self) {
        self.out.events.push(Event::FocusModePicked { mode: self.focus_mode().into() });
        if self.focus.index == 1 {
            if !self.point_focus {
                self.out.events.push(Event::FocusPointPicked { x: 0.5, y: 0.5 }); // the middle of what the preview shows
            }
            return;
        }
        self.point_focus = false;
        let manual = self.focus.index == 2;
        self.manual_focus = manual;
        self.focus_slider.enabled = manual;
        if manual {
            self.send(json!({"action": "focus_mode", "value": "manual"}));
            self.send(json!({"action": "focus_distance", "value": self.slider_to_diopters(self.focus_slider.value)}));
        } else {
            self.send(json!({"action": "focus_mode", "value": "continuous"}));
        }
        self.out.saves += 1;
    }

    fn on_focus_slider(&mut self, pos: i64) {
        let d = self.slider_to_diopters(pos);
        self.focus_text = diopters_to_label(d);
        if self.manual_focus {
            self.send(json!({"action": "focus_distance", "value": d}));
        }
        self.out.saves += 1;
    }

    fn on_ae_comp_changed(&mut self, steps: i64) {
        self.ae_text = ev_text(steps as f64 * self.ae_step);
        self.send(json!({"action": "ae_comp", "value": steps}));
        self.out.saves += 1;
    }

    fn on_nr_mode_changed(&mut self, i: usize) {
        self.send(json!({"action": "nr_mode", "value": MODES[i].1}));
        self.out.saves += 1;
    }

    fn on_edge_mode_changed(&mut self, i: usize) {
        self.send(json!({"action": "edge_mode", "value": MODES[i].1}));
        self.out.saves += 1;
    }

    fn on_low_light(&mut self, on: bool) {
        self.send(low_light_body(on));
        self.out.saves += 1;
    }

    fn on_bll_changed(&mut self, on: bool) {
        self.send(json!({"action": "black_level_lock", "value": if on { "1" } else { "0" }}));
        self.out.saves += 1;
    }

    /// Live only: the torch isn't saved.
    fn on_torch_toggled(&mut self, on: bool) {
        self.torch_on = on;
        self.send(json!({"action": "torch", "value": if on { "1" } else { "0" }}));
    }

    fn slider_to_diopters(&self, pos: i64) -> f64 {
        pos as f64 / FOCUS_STEPS as f64 * self.focus_max_diopters
    }

    fn set_focus_slider_value(&mut self, diopters: f64) {
        let pos = if self.focus_max_diopters > 0.0 { (diopters / self.focus_max_diopters * FOCUS_STEPS as f64) as i64 } else { 0 };
        self.focus_slider.set(pos.clamp(0, FOCUS_STEPS)); // signals blocked
        self.focus_text = diopters_to_label(diopters);
    }

    // ── Config ──

    fn load_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let num = |key: &str| -> Result<Option<f64>, ConfigRejected> {
            match cfg.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Bool(b)) => Ok(Some(f64::from(u8::from(*b)))),
                Some(v) => v.as_f64().map(Some).ok_or_else(|| ConfigRejected(format!("{key} isn't a number"))),
            }
        };
        self.config_in = cfg.clone();
        let manual_exp = get_bool(cfg, "exp_manual", false);
        self.set_exp(manual_exp);
        self.set_manual_exp_widgets(manual_exp);
        if let Some(iso) = num("iso")?.filter(|v| *v != 0.0) {
            self.iso.set_value(iso);
        }
        if let Some(ns) = num("shutter_ns")?.filter(|v| *v != 0.0) {
            self.shutter.set_value(ns);
        }
        let ois = cfg.get("ois").is_none_or(truthy);
        self.set_ois(ois);
        let manual_focus = get_bool(cfg, "focus_manual", false);
        self.set_focus_index(if manual_focus { 2 } else { 0 });
        self.manual_focus = manual_focus;
        self.focus_slider.enabled = manual_focus;
        if let Some(d) = num("focus_diopters")? {
            self.set_focus_slider_value(d); // 0 is infinity
        }
        let manual_wb = get_bool(cfg, "wb_manual", false);
        self.set_wb_mode(manual_wb);
        self.set_manual_wb_widgets(manual_wb);
        if let Some(k) = num("wb_kelvin")?.filter(|v| *v != 0.0) {
            self.kelvin.set(k as i64); // signals blocked
            self.kelvin_text = format!("{} K", k as i64);
        }
        if let Some(t) = num("wb_tint")? {
            self.set_tint(t as i64);
            self.tint_text = signed(t as i64);
        }
        if let Some(ae) = num("ae_comp")? {
            self.set_ae(ae as i64, false);
            self.ae_text = ev_text(ae as i64 as f64 * self.ae_step);
        }
        if let Some(nr) = num("nr_mode")? {
            self.set_nr(mode_index(Some(nr)), false);
        }
        if let Some(em) = num("edge_mode")? {
            self.set_edge(mode_index(Some(em)), false);
        }
        self.set_bll(get_bool(cfg, "bll", false), false);
        self.set_low_light(get_bool(cfg, "low_light", false), false);
        self.lens_id = cfg.get("lens").and_then(Value::as_str).map(str::to_string);
        self.pending_lens = None;
        Ok(())
    }

    fn config(&self) -> Value {
        json!({
            "exp_manual": self.exp.index == 1,
            "iso": self.iso.saved_number(),
            "shutter_ns": self.shutter.get_value(),
            "ois": self.ois.checked,
            "focus_manual": self.focus.index == 2,
            "focus_diopters": self.slider_to_diopters(self.focus_slider.value),
            "wb_manual": self.wb.index == 1,
            "wb_kelvin": self.kelvin.value,
            "wb_tint": self.tint.value,
            "ae_comp": self.ae.value,
            "nr_mode": MODES[self.nr].1,
            "edge_mode": MODES[self.edge].1,
            "bll": self.bll.checked,
            "low_light": self.low_light.checked,
            "lens": self.lens_id,
        })
    }

    /// Load a preset's settings and, while streaming, send all of them, the lens first: it sets the slider ranges, so
    /// the preset's values aren't clamped to the old lens's.
    fn preset(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let live_lens = self.lens_id.clone();
        let lens = cfg.get("lens").and_then(Value::as_str).map(str::to_string);
        let mut cam = None;
        if self.stream.is_some() {
            if let Some(lens) = lens.as_ref().filter(|l| Some(*l) != live_lens.as_ref()) {
                cam = self.lenses.select_id(lens);
            }
            if let Some(cam) = &cam {
                self.on_lens_selected(cam);
            }
        }
        let loaded = self.load_config(cfg);
        self.point_focus = false;
        if self.stream.is_none() {
            self.pending_lens = lens;
            return loaded;
        }
        if cam.is_none() {
            self.lens_id = live_lens; // the preset's lens isn't on this phone (or is the live one)
        }
        self.push_settings_to_phone();
        loaded
    }

    // ── What the card shows ──

    fn props(&self) -> Value {
        let lenses: Vec<Value> = self.lenses.cameras.iter().map(|cam| {
            let label = lens_label(cam);
            let (number, unit, rest) = split_lens_label(&label);
            json!({"id": cam.get("id").and_then(Value::as_str).unwrap_or(""), "number": number, "unit": unit, "rest": rest,
                   "name": lens_name(&label)})
        }).collect();
        let tile = |t: &Tile| json!({"checked": t.checked, "enabled": t.enabled, "tip": t.tip});
        let mut props = json!({
            "active": self.card_on(),
            "phone-only-hidden": !self.source.is_empty(),
            "lenses": lenses,
            "current-lens": self.lenses.checked.map_or(-1, |i| i as i64),
            "lens-placeholder": self.lenses.placeholder,
            "info": self.info_text,
            "info-tip": self.info_tip,
            "exposure-mode": self.exp.index,
            "exposure-disabled": self.exp.enabled.iter().map(|e| !e).collect::<Vec<_>>(),
            "show-manual-exposure": self.manual_exp,
            "iso-pos": self.iso.pos,
            "iso-text": self.iso.text(),
            "iso-enabled": self.iso.enabled,
            "shutter-pos": self.shutter.pos,
            "shutter-text": self.shutter.text(),
            "shutter-enabled": self.shutter.enabled,
            "ae-comp": self.ae.value,
            "ae-min": self.ae.min,
            "ae-max": self.ae.max,
            "ae-text": self.ae_text,
            "wb-mode": self.wb.index,
            "wb-disabled": self.wb.enabled.iter().map(|e| !e).collect::<Vec<_>>(),
            "show-manual-wb": self.manual_wb,
            "kelvin": self.kelvin.value,
            "kelvin-text": self.kelvin_text,
            "tint": self.tint.value,
            "tint-text": self.tint_text,
            "wb-enabled": self.kelvin.enabled,
            "focus-mode": self.focus.index,
            "focus-disabled": self.focus.enabled.iter().map(|e| !e).collect::<Vec<_>>(),
            "show-manual-focus": self.manual_focus,
            "focus-pos": self.focus_slider.value,
            "focus-text": self.focus_text,
            "focus-enabled": self.focus_slider.enabled,
            "ois": tile(&self.ois),
            "low-light": tile(&self.low_light),
            "show-low-light": self.low_light.visible,
            "black-level-lock": tile(&self.bll),
            "torch": tile(&self.torch),
            "noise-reduction": self.nr,
            "sharpening": self.edge,
        });
        // Apart from the rest: json! gives up on one this long
        props["lens-columns"] = json!(lens_columns(self.lenses.cameras.len()));
        props["exposure-tips"] = json!(self.exp.tips);
        props["wb-tips"] = json!(self.wb.tips);
        props["focus-tips"] = json!(self.focus.tips);
        props
    }

    /// What the handlers queued goes out, then the card shows where things are.
    fn refresh(&mut self, host: &mut dyn Host) {
        let out = std::mem::take(&mut self.out);
        for body in &out.bodies {
            host.phone_control(None, body);
        }
        for event in out.events {
            host.emit(event);
        }
        for _ in 0..out.saves {
            host.schedule_save();
        }
        host.set_ui_props("card", &self.props());
    }

    // ── Shortcuts ──

    /// control_ready for a control the card shows only in some modes: the card on and the control there and enabled.
    fn ready(&self, shown: bool, enabled: bool) -> bool {
        self.card_on() && self.source.is_empty() && shown && enabled
    }

    fn choose(&mut self, id: &str, params: &Value) -> Option<String> {
        let (label, values, shown, current, enabled): (String, Vec<String>, Vec<String>, Option<usize>, Vec<bool>) = match id {
            "camera.lens" => (
                tr("Lens"),
                self.lenses.cameras.iter().map(lens_value).collect(),
                self.lenses.cameras.iter().map(|c| lens_name(&lens_label(c))).collect(),
                self.lenses.checked,
                vec![true; self.lenses.cameras.len()],
            ),
            "camera.exposure_mode" => (tr("Exposure mode"), EXPOSURE_VALUES.map(String::from).to_vec(),
                                       vec![tr("Auto"), tr("Manual")], Some(self.exp.index), self.exp.enabled.clone()),
            "camera.wb_mode" => (tr("White balance mode"), EXPOSURE_VALUES.map(String::from).to_vec(),
                                 vec![tr("Auto"), tr("Manual")], Some(self.wb.index), self.wb.enabled.clone()),
            "camera.focus_mode" => (tr("Focus mode"), FOCUS_VALUES.map(String::from).to_vec(),
                                    vec![tr("Auto"), tr("Tap a point"), tr_ctx("focus", "Manual")], Some(self.focus.index),
                                    self.focus.enabled.clone()),
            _ => {
                let (label, current) = if id == "camera.noise_reduction" { (tr("Noise reduction"), self.nr) } else { (tr("Sharpening"), self.edge) };
                (label, MODES.iter().map(|(l, _)| l.to_string()).collect(), MODES.iter().map(|(l, _)| tr(l)).collect(), Some(current),
                 vec![true; 3])
            }
        };
        let enabled: Vec<bool> = enabled.iter().map(|e| *e && self.card_on() && self.source.is_empty()).collect();
        if !enabled.iter().any(|e| *e) {
            return None;
        }
        let options: Vec<Choice> = values.iter().zip(&shown).zip(&enabled)
            .map(|((value, shown), enabled)| Choice { value, shown, enabled: *enabled }).collect();
        let (said, pick) = actions::choice_target(params, &options, current, true)?;
        if pick {
            self.pick(id, said);
        }
        Some(actions::slider_text(&label, &shown[said]))
    }

    /// A choice picked by a key, the way a click or a pick in the list would.
    fn pick(&mut self, id: &str, i: usize) {
        match id {
            "camera.lens" => self.on_ui_lens(i),
            "camera.exposure_mode" => {
                self.set_exp(i == 1);
                self.on_exp_mode();
            }
            "camera.wb_mode" => {
                self.set_wb_mode(i == 1);
                self.on_wb_mode();
            }
            "camera.focus_mode" => self.click_focus(i),
            "camera.noise_reduction" => self.set_nr(i, false),
            _ => self.set_edge(i, false),
        }
    }

    fn on_ui_lens(&mut self, i: usize) {
        let Some(cam) = self.lenses.cameras.get(i).cloned() else { return };
        self.lenses.checked = Some(i);
        self.on_lens_selected(&cam);
    }

    fn click_focus(&mut self, i: usize) {
        if self.focus.enabled.get(i) != Some(&true) {
            return;
        }
        self.set_focus_index(i);
        self.on_focus_mode();
    }

    fn toggle(&mut self, id: &str, params: &Value) -> Option<String> {
        let (which, label, tile) = match id {
            "camera.stabilization" => (0, tr("Stabilization"), &self.ois),
            "camera.black_level_lock" => (1, tr("Black level lock"), &self.bll),
            "camera.low_light" => (2, tr("Low light"), &self.low_light),
            _ => (3, tr("Torch"), &self.torch),
        };
        if !self.ready(tile.visible, tile.enabled) {
            return None;
        }
        let is_on = tile.checked;
        let want = actions::toggle_wanted(params, is_on, &mut self.held[which]);
        self.flip(which, want);
        Some(actions::toggle_text(&label, want))
    }

    fn flip(&mut self, which: usize, on: bool) {
        match which {
            0 => self.set_ois(on),
            1 => self.set_bll(on, false),
            2 => self.set_low_light(on, false),
            _ => self.set_torch(on, false),
        }
    }

    fn slide(&mut self, id: &str, params: &Value) -> Option<String> {
        match id {
            "camera.iso" | "camera.shutter" => {
                let shutter = id == "camera.shutter";
                let row = if shutter { &self.shutter } else { &self.iso };
                if !self.ready(self.manual_exp, row.enabled) {
                    return None;
                }
                let new = actions::log_slider_target(params, row.get_value(), row.v_min, row.v_max, row.spin_scale)?;
                let (label, text) = if shutter {
                    self.shutter.set_value(new);
                    self.on_shutter_changed(new);
                    (tr("Shutter"), self.shutter.text())
                } else {
                    self.iso.set_value(new);
                    self.on_iso_changed(new);
                    ("ISO".to_string(), self.iso.text())
                };
                Some(actions::slider_text(&label, &text))
            }
            "camera.compensation" => {
                if !self.ready(true, self.ae.enabled) {
                    return None;
                }
                let target = actions::slider_target(params, self.ae.value, self.ae_step, self.ae.min, self.ae.max, Some(0))?;
                self.set_ae(target, false);
                Some(actions::slider_text(&tr("Compensation"), &self.ae_text))
            }
            "camera.temperature" => {
                if !self.ready(self.manual_wb, self.kelvin.enabled) {
                    return None;
                }
                let target = actions::slider_target(params, self.kelvin.value, 1.0, WB_MIN_K, WB_MAX_K, Some(WB_NEUTRAL))?;
                self.set_kelvin(target);
                Some(actions::slider_text(&tr("Temperature"), &self.kelvin_text))
            }
            "camera.tint" => {
                if !self.ready(self.manual_wb, self.tint.enabled) {
                    return None;
                }
                let target = actions::slider_target(params, self.tint.value, 1.0, -150, 150, Some(0))?;
                self.set_tint(target);
                Some(actions::slider_text(&tr("Tint"), &self.tint_text))
            }
            _ => {
                if !self.ready(self.manual_focus, self.focus_slider.enabled) {
                    return None;
                }
                let target = actions::slider_target(params, self.focus_slider.value, 0.1, 0, FOCUS_STEPS, None)?;
                if self.focus_slider.set(target) {
                    self.on_focus_slider(target);
                }
                Some(actions::slider_text(&tr("Focus distance"), &self.focus_text))
            }
        }
    }
}

/// What the lens can do, read from its entry with the card's defaults.
struct Caps {
    manual_sensor: bool,
    manual_wb: bool,
    manual_focus: bool,
    min_focus_distance: f64,
    flash: bool,
    ois: bool,
}

impl Caps {
    fn of(cam: &Value) -> Caps {
        Caps {
            manual_sensor: get_bool(cam, "supportsManualSensor", true),
            manual_wb: get_bool(cam, "supportsManualWB", true),
            manual_focus: get_bool(cam, "supportsManualFocus", false),
            min_focus_distance: get_f64(cam, "minFocusDistance", 10.0),
            flash: get_bool(cam, "supportsFlash", false),
            ois: get_bool(cam, "hasOis", true),
        }
    }
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// The bodies as link's controls.rs builds them, byte for byte (the shell doesn't depend on link).
fn point_body(action: &str, x: f64, y: f64) -> Value {
    json!({"action": action, "x": round4(x), "y": round4(y)})
}

fn low_light_body(on: bool) -> Value {
    json!({"action": "low_light", "value": if on { "1" } else { "0" }})
}

fn wb_gains_body(r: f64, ge: f64, go: f64, b: f64) -> Value {
    json!({"action": "wb_gains", "r": r, "ge": ge, "go": go, "b": b})
}

impl Plugin for CameraControl {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "camera_control".into(),
            title: "Camera".into(),
            about: "Exposure, white balance, focus, lenses and the phone camera's other settings.".into(),
            when_off: "Without it there's no Camera card and the phone uses its own automatic settings.".into(),
            category: "camera".into(),
            icon: "camera".into(),
            in_presets: true,
            device_local: true,
            frame_stage: FrameStage::Effects,
            ..Default::default()
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec {
            ui: UiSpec {
                source: SlintSource::Text(include_str!("../../ui/cards/camera.slint").into()),
                component: "CameraCard".into(),
                props: self.props(),
            },
            region: "right".into(),
        })
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.publish_focus_mode(true);
        self.refresh(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::PhonesChanged { count } => self.phones = *count,
            Event::SourceSelected { source_id } => self.source = source_id.clone(),
            Event::FocusPoint { x, y } => self.on_focus_point(f64::from(*x), f64::from(*y)),
            Event::MeterPoint { x, y } => self.on_meter_point(f64::from(*x), f64::from(*y)),
            _ => return,
        }
        self.refresh(host);
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.start(&stream.url);
        self.session = Some(stream.session);
        self.refresh(host);
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        let Some((state, point)) = self.behind.take(&*host, stream.session) else {
            self.on_stream_start(host, stream);
            return;
        };
        // Back on a stream that kept going: the phone has its settings, so the card only shows them, in its ranges
        self.phone_state(&state);
        let cfg = self.config_in.clone();
        let _ = self.load_config(&cfg);
        self.point_focus = false;
        if point {
            self.show_point_focus();
        }
        self.stream = Some(stream.url.clone());
        self.session = Some(stream.session);
        let point_ok = self.focus.enabled[1];
        self.set_point_available(point_ok, None);
        self.refresh(host);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        self.behind.drop_shown(&*host);
        self.leave_stream();
        self.refresh(host);
    }

    fn on_focus_lost(&mut self, host: &mut dyn Host) {
        if let Some(state) = self.last_state.clone() {
            self.behind.keep(&*host, self.session, (state, self.point_focus));
        }
        self.leave_stream();
        self.refresh(host);
    }

    fn on_phone_state(&mut self, host: &mut dyn Host, state: &Value) {
        self.phone_state(state);
        self.refresh(host);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        let b = |i: usize| event.args.get(i).and_then(Value::as_bool).unwrap_or(false);
        match event.callback.as_str() {
            "lens-picked" => self.on_ui_lens(n(0) as usize),
            "exposure-picked" => {
                if self.exp.enabled.get(n(0) as usize) == Some(&true) {
                    self.set_exp(n(0) as usize == 1);
                    self.on_exp_mode();
                }
            }
            "iso-moved" | "shutter-moved" => {
                let row = if event.callback == "iso-moved" { &mut self.iso } else { &mut self.shutter };
                if !row.enabled {
                    return;
                }
                row.pos = n(0).round().clamp(0.0, LOG_STEPS);
                let val = row.get_value();
                row.spin = row.to_spin(val);
                row.shown = val;
                if event.callback == "iso-moved" { self.on_iso_changed(val) } else { self.on_shutter_changed(val) }
            }
            "ae-moved" => self.set_ae(n(0).round() as i64, false),
            "wb-picked" => {
                if self.wb.enabled.get(n(0) as usize) == Some(&true) {
                    self.set_wb_mode(n(0) as usize == 1);
                    self.on_wb_mode();
                }
            }
            "kelvin-moved" => self.set_kelvin(n(0).round() as i64),
            "tint-moved" => self.set_tint(n(0).round() as i64),
            "focus-picked" => self.click_focus(n(0) as usize),
            "focus-moved" => {
                let pos = n(0).round() as i64;
                if self.focus_slider.set(pos) {
                    self.on_focus_slider(self.focus_slider.value);
                }
            }
            "ois-toggled" => self.set_ois(b(0)),
            "low-light-toggled" => self.set_low_light(b(0), false),
            "bll-toggled" => self.set_bll(b(0), false),
            "torch-toggled" => self.set_torch(b(0), false),
            "nr-picked" => self.set_nr((n(0) as usize).min(2), false),
            "edge-picked" => self.set_edge((n(0) as usize).min(2), false),
            _ => return,
        }
        self.refresh(host);
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let g = tr("Camera");
        let search = |words: &str| tr_ctx("search", words);
        let lens_options: Vec<(String, String)> =
            self.lenses.cameras.iter().map(|c| (lens_value(c), lens_name(&lens_label(c)))).collect();
        let lens_refs: Vec<(&str, String)> = lens_options.iter().map(|(v, s)| (v.as_str(), s.clone())).collect();
        let two = [("Auto", tr("Auto")), ("Manual", tr("Manual"))];
        let focus = [("Auto", tr("Auto")), ("Point", tr("Tap a point")), ("Manual", tr_ctx("focus", "Manual"))];
        let modes: Vec<(&str, String)> = MODES.iter().map(|(l, _)| (*l, tr(l))).collect();
        let shutter_label = tr("Shutter");
        let iso = LogSliderSpec { id: "camera.iso", label: "ISO", group: &g, keywords: &search("exposure manual"),
                                  minimum: self.iso.v_min, maximum: self.iso.v_max, decimals: 0, suffix: "" };
        let shutter = LogSliderSpec { id: "camera.shutter", label: &shutter_label, group: &g, keywords: &search("exposure manual speed"),
                                      minimum: self.shutter.v_min * 1e-6, maximum: self.shutter.v_max * 1e-6, decimals: 2, suffix: " ms" };
        let slider = |id: &str, label: &str, keywords: &str, min: f64, max: f64, step: f64, decimals: usize, suffix: &str, has_default: bool,
                      current: f64| {
            actions::slider_action(&SliderSpec { id, label, group: &g, keywords, minimum: min, maximum: max, step, decimals, suffix, has_default }, current)
        };
        let s = self.ae_step;
        vec![
            actions::choice_action("camera.lens", &tr("Lens"), &g, "", &lens_refs),
            actions::choice_action("camera.exposure_mode", &tr("Exposure mode"), &g, "", &two),
            actions::log_slider_action(&iso, self.iso.get_value()),
            actions::log_slider_action(&shutter, self.shutter.get_value() * 1e-6),
            slider("camera.compensation", &tr("Compensation"), &search("exposure ev brightness"), self.ae.min as f64 * s,
                   self.ae.max as f64 * s, 1.0, 1, " EV", true, self.ae.value as f64 * s),
            actions::choice_action("camera.wb_mode", &tr("White balance mode"), &g, "wb", &two),
            slider("camera.temperature", &tr("Temperature"), &search("white balance wb kelvin"), WB_MIN_K as f64, WB_MAX_K as f64, 100.0,
                   0, " K", true, self.kelvin.value as f64),
            slider("camera.tint", &tr("Tint"), &search("white balance wb green magenta"), -150.0, 150.0, 10.0, 0, "", true,
                   self.tint.value as f64),
            actions::choice_action("camera.focus_mode", &tr("Focus mode"), &g, "", &focus),
            slider("camera.focus", &tr("Focus distance"), &search("manual focus"), 0.0, 100.0, 5.0, 1, "%", false,
                   self.focus_slider.value as f64 * 0.1),
            actions::toggle_action("camera.stabilization", &tr("Stabilization"), &g, "ois", &[]),
            actions::choice_action("camera.noise_reduction", &tr("Noise reduction"), &g, "", &modes),
            actions::choice_action("camera.sharpening", &tr("Sharpening"), &g, &search("edge"), &modes),
            actions::toggle_action("camera.black_level_lock", &tr("Black level lock"), &g, "", &[]),
            actions::toggle_action("camera.low_light", &tr("Low light"), &g, &search("dark night brightness"), &[]),
            actions::toggle_action("camera.torch", &tr("Torch"), &g, &search("flash light"), &[]),
        ]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        let said = match id {
            "camera.lens" | "camera.exposure_mode" | "camera.wb_mode" | "camera.focus_mode" | "camera.noise_reduction" | "camera.sharpening" => {
                self.choose(id, params)
            }
            "camera.stabilization" | "camera.black_level_lock" | "camera.low_light" | "camera.torch" => self.toggle(id, params),
            "camera.iso" | "camera.shutter" | "camera.compensation" | "camera.temperature" | "camera.tint" | "camera.focus" => {
                self.slide(id, params)
            }
            _ => return None,
        };
        self.refresh(host);
        said
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        let which = match id {
            "camera.stabilization" => 0,
            "camera.black_level_lock" => 1,
            "camera.low_light" => 2,
            "camera.torch" => 3,
            _ => return,
        };
        if let Some(back) = actions::hold_released(params, &mut self.held[which]) {
            self.flip(which, back);
            self.refresh(host);
        }
    }

    fn get_config(&self) -> Value {
        self.config()
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        // What it sends and says waits for the next call with a host, as set_config has none
        self.load_config(cfg)
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        let done = self.preset(cfg);
        self.refresh(host);
        done
    }
}

#[cfg(test)]
#[path = "camera_control_tests.rs"]
mod tests;
