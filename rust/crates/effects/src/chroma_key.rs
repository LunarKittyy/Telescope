//! The Chroma key plugin (plugins/chroma_key.py): a green or blue screen, or a colour picked on the preview, swapped for a colour or
//! a picture. The matte is a shader (chroma_key.wgsl) that looks colours up in the same three tables Python builds (`key_tables`,
//! handed over as a texture), so the picture is the one OpenCV makes. Picking the screen's colour needs the frame the key sees, which
//! no shader can hand back: while a pick runs the key steps aside for a CPU step that keeps the newest frame, as Python's process_frame
//! does (keying waits while its own pick runs, so the screen shows to click on).

use crate::cpu::rgb_at;
use crate::shared::{hex, hsv_to_rgb, no_pictures, parse_hex, rgb_to_hsv, Paired, PictureReader};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use telescope_core::actions::{self, Choice, SliderSpec};
use telescope_core::config::clamped_int;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat, ShaderStep, Touch};
use telescope_core::host::{FileDialog, FileDialogKind, Host, PickKind, RequestId};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "chroma_key";
/// The owner of the pipette's pick on the preview, apart from the screen's own so keying doesn't step aside for it.
const BACKDROP_OWNER: &str = "chroma_key.backdrop";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/chroma_key.slint"));
const WGSL: &str = include_str!("chroma_key.wgsl");

/// The screens to pick from, as (saved name, shown name, the colour as B, G, R). Picked has the colour picked on the preview.
pub const SCREENS: [(&str, &str, Option<[u8; 3]>); 3] = [("green", n_("Green"), Some([60, 180, 50])), ("blue", n_("Blue"), Some([190, 80, 20])), ("picked", n_("Picked"), None)];
pub const BACKGROUNDS: [(&str, &str); 2] = [("colour", n_("Colour")), ("picture", n_("Picture"))];
const PICTURES: &str = n_("Pictures (*.png *.jpg *.jpeg *.webp *.bmp)");
/// B, G, R: a dark neutral behind you
pub const DEFAULT_COLOUR: [u8; 3] = [40, 34, 30];
/// Spill comes off colours up to about 60 degrees of hue from the screen's, fading out towards there.
const SPILL_SPREAD: f32 = 0.5;
/// A picked colour is the median of a square this far around the click, at the stream's size.
const SAMPLE: i64 = 4;
/// A picked colour with less colour than this (out of about 128) can't be keyed: grey, white, black.
const MIN_CHROMA: f64 = 12.0;
/// tolerance, softness, spill: their saved names and defaults
const SLIDERS: [(&str, i64); 3] = [("tolerance", 40), ("softness", 30), ("spill", 60)];
const SLIDER_TIPS: [&str; 3] = [n_("Higher takes out more shades of the screen"), n_("How soft the edge around you is"), n_("Takes the screen's colour out of your edges and skin")];

/// (Cb, Cr) of a B, G, R colour, centred on 0, as cv2.cvtColor's fixed-point BGR2YCrCb makes them.
pub fn chroma_of(bgr: [u8; 3]) -> (f64, f64) {
    let (b, g, r) = (bgr[0] as i32, bgr[1] as i32, bgr[2] as i32);
    let y = (b * 1868 + g * 9617 + r * 4899 + (1 << 13)) >> 14;
    // saturated to a byte, as OpenCV does: a pure red would reach 256
    let cr = (((r - y) * 11682 + (128 << 14) + (1 << 13)) >> 14).clamp(0, 255);
    let cb = (((b - y) * 9241 + (128 << 14) + (1 << 13)) >> 14).clamp(0, 255);
    (cb as f64 - 128.0, cr as f64 - 128.0)
}

/// Whether a colour has enough colour in it to key on.
pub fn keyable(bgr: [u8; 3]) -> bool {
    let (cb, cr) = chroma_of(bgr);
    cb.hypot(cr) >= MIN_CHROMA
}

/// The three lookup tables, each 65536 long and indexed by Cr << 8 | Cb: how much of a pixel with that colour is kept (255
/// for all of it), and its Cr and Cb once the screen's spill is taken out.
#[derive(Clone, Debug, PartialEq)]
pub struct Tables {
    pub alpha: Vec<u8>,
    pub cr: Vec<u8>,
    pub cb: Vec<u8>,
}

fn table(v: f32) -> u8 {
    v.round_ties_even().clamp(0.0, 255.0) as u8
}

/// key_tables: tolerance, softness and spill are 0..100.
pub fn key_tables(key_bgr: [u8; 3], tolerance: i64, softness: i64, spill: i64) -> Tables {
    let (kcb, kcr) = chroma_of(key_bgr);
    let reach = kcb.hypot(kcr).max(MIN_CHROMA);
    // 0.1 + tolerance / 100 * 0.8, and a pixel this close to the screen's colour goes
    let inner = (reach * (0.1 + tolerance as f64 / 100.0 * 0.8)) as f32;
    // and from there on it fades back in
    let width = (reach * (0.02 + softness as f64 / 100.0 * 0.6)) as f32;
    let (ux, uy) = ((kcb / reach) as f32, (kcr / reach) as f32);
    let (kcb, kcr) = (kcb as f32, kcr as f32);
    let spill = spill as f32 / 100.0;
    let (mut alpha, mut out_cr, mut out_cb) = (vec![0u8; 65536], vec![0u8; 65536], vec![0u8; 65536]);
    for ri in 0..256usize {
        let cr = ri as f32 - 128.0;
        for bi in 0..256usize {
            let cb = bi as f32 - 128.0;
            let distance = (cb - kcb).hypot(cr - kcr);
            let a = ((distance - inner) / width).clamp(0.0, 1.0);
            let along = cb * ux + cr * uy;
            let length = cb.hypot(cr);
            let cos = if length > 1e-3 { along / length.max(1e-3) } else { 0.0 };
            let near = ((cos - SPILL_SPREAD) / (1.0 - SPILL_SPREAD)).clamp(0.0, 1.0);
            let take = along.max(0.0) * near * spill;
            let i = ri * 256 + bi;
            alpha[i] = table(a * 255.0);
            out_cr[i] = table(cr - take * uy + 128.0);
            out_cb[i] = table(cb - take * ux + 128.0);
        }
    }
    Tables { alpha, cr: out_cr, cb: out_cb }
}

impl Tables {
    /// The tables as the 256 x 256 picture the shader reads: red = alpha, green = Cr, blue = Cb.
    pub fn texture(&self) -> Frame {
        let mut data = Vec::with_capacity(65536 * 4);
        for i in 0..65536 {
            data.extend_from_slice(&[self.alpha[i], self.cr[i], self.cb[i], 255]);
        }
        Frame::new(256, 256, PixelFormat::Rgba, data)
    }
}

/// sample(): the colour around (u, v) of a frame (0..1) as B, G, R, as a median so a stray pixel doesn't decide it.
pub fn sample(frame: &Frame, u: f64, v: f64) -> [u8; 3] {
    let (w, h) = (frame.width as i64, frame.height as i64);
    let at = |t: f64, n: i64| ((t * (n - 1) as f64).round_ties_even() as i64).clamp(0, n - 1);
    let (x, y) = (at(u, w), at(v, h));
    let mut channels: [Vec<u8>; 3] = Default::default();
    for py in (y - SAMPLE).max(0)..(y + SAMPLE + 1).min(h) {
        for px in (x - SAMPLE).max(0)..(x + SAMPLE + 1).min(w) {
            if let Some(rgb) = rgb_at(frame, px as usize, py as usize) {
                for c in 0..3 {
                    channels[c].push(rgb[2 - c]);
                }
            }
        }
    }
    let median = |v: &mut Vec<u8>| {
        v.sort_unstable();
        match v.len() {
            0 => 0,
            n if n % 2 == 1 => v[n / 2],
            n => ((v[n / 2 - 1] as u16 + v[n / 2] as u16) / 2) as u8,
        }
    };
    [median(&mut channels[0]), median(&mut channels[1]), median(&mut channels[2])]
}

/// What the key sees while its pick runs, kept for the click to read.
struct Tap(Arc<Mutex<Option<Frame>>>);

impl CpuStep for Tap {
    fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
        Touch::Reads
    }

    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        // A clone shares the pixels, and a later step that changes them gets a copy of its own
        *self.0.lock().unwrap() = Some(frame.clone());
    }
}

/// A saved colour: three whole numbers 0..255 in a list.
fn bgr(value: Option<&Value>) -> Option<[u8; 3]> {
    let list = value?.as_array().filter(|l| l.len() == 3)?;
    let mut out = [0u8; 3];
    for (o, c) in out.iter_mut().zip(list) {
        let n = c.as_i64().or_else(|| c.as_u64().and_then(|u| i64::try_from(u).ok()))?;
        *o = u8::try_from(n).ok()?;
    }
    Some(out)
}

/// What the baked tables were made from: the key colour and the sliders.
type TablesKey = (String, [i64; 3]);

pub struct ChromaKeyPlugin {
    on: bool,
    screen: &'static str,
    /// B, G, R picked on the preview
    picked: Option<[u8; 3]>,
    colour: [u8; 3],
    /// The colour as the pad and the strip left it (hue, saturation, brightness), so a grey keeps its hue
    hsv: (f32, f32, f32),
    sliders: [i64; 3],
    background: &'static str,
    picture_path: String,
    picture: Option<Frame>,
    picture_problem: String,
    status: Option<String>,
    paired: Paired,
    held: Option<bool>,
    /// Keying waits while the screen's colour is picked, so the screen shows to click on.
    picking: bool,
    last: Arc<Mutex<Option<Frame>>>,
    reader: PictureReader,
    asked: Option<RequestId>,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
    cached: Option<(TablesKey, Arc<Tables>)>,
}

impl Default for ChromaKeyPlugin {
    fn default() -> Self {
        ChromaKeyPlugin::new()
    }
}

impl ChromaKeyPlugin {
    pub fn new() -> ChromaKeyPlugin {
        ChromaKeyPlugin::with_reader(no_pictures())
    }

    /// With the reader that turns a picture file into a frame (shared::picture_reader of the app's Media).
    pub fn with_reader(reader: PictureReader) -> ChromaKeyPlugin {
        ChromaKeyPlugin {
            on: false,
            screen: "green",
            picked: None,
            colour: DEFAULT_COLOUR,
            hsv: rgb_to_hsv([DEFAULT_COLOUR[2], DEFAULT_COLOUR[1], DEFAULT_COLOUR[0]], (0.0, 0.0)),
            sliders: [SLIDERS[0].1, SLIDERS[1].1, SLIDERS[2].1],
            background: "colour",
            picture_path: String::new(),
            picture: None,
            picture_problem: String::new(),
            status: None,
            paired: Paired::default(),
            held: None,
            picking: false,
            last: Arc::new(Mutex::new(None)),
            reader,
            asked: None,
            pending: true,
            cached: None,
        }
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    /// The screen's colour as B, G, R; None while Picked has nothing picked.
    fn key_colour(&self) -> Option<[u8; 3]> {
        if self.screen == "picked" {
            return self.picked;
        }
        SCREENS.iter().find(|(k, ..)| *k == self.screen).and_then(|(.., bgr)| *bgr)
    }

    fn screen_index(&self) -> usize {
        SCREENS.iter().position(|(k, ..)| *k == self.screen).unwrap_or(0)
    }

    // ── What the card shows ───────────────────────────────────────────

    /// The text under the controls: why the picture behind you isn't there yet.
    fn note(&self) -> String {
        if self.on && self.background == "picture" && !self.picture_problem.is_empty() {
            self.picture_problem.clone()
        } else if self.on && self.background == "picture" && self.picture.is_none() {
            tr("Choose a picture to put behind you.")
        } else {
            String::new()
        }
    }

    fn fixed_props() -> Value {
        json!({
            "switch-name": tr("Chroma key"),
            "switch-help": tr("Swap the screen behind you"),
            "slider-tips": SLIDER_TIPS.iter().map(|t| tr(t)).collect::<Vec<_>>(),
            "reset-hint": tr("Double-click to reset"),
            "colour-tip": tr("Pick the colour that goes behind you"),
            "pipette-tip": tr("Pick a colour from the preview"),
            "picture-choose-tip": tr("Pick the picture that goes behind you"),
        })
    }

    fn all_props(&self) -> Value {
        let picked_tip = if self.picked.is_some() { tr("The colour picked from the preview. Click to pick again.") } else { tr("Pick the screen's colour from the preview") };
        let text = self.note();
        let (r, g, b) = (self.colour[2], self.colour[1], self.colour[0]);
        json!({
            "active": self.paired.on(),
            "on": self.on,
            "screen": self.screen_index(),
            "screen-tip": picked_tip,
            "has-picked": self.picked.is_some(),
            "picked-colour": hex(self.picked.map_or([128, 128, 128], |p| [p[2], p[1], p[0]])),
            "slider-value": self.sliders,
            "slider-text": self.sliders.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            "behind": BACKGROUNDS.iter().position(|(k, _)| *k == self.background).unwrap_or(0),
            "backdrop-colour": hex([r, g, b]),
            "screen-colour": hex(self.key_colour().map_or([128, 128, 128], |k| [k[2], k[1], k[0]])),
            "colour-h": self.hsv.0,
            "colour-s": self.hsv.1,
            "colour-v": self.hsv.2,
            "colour-hex": hex([r, g, b]),
            "channel-value": [r, g, b],
            "picture-text": if self.picture_path.is_empty() { tr("No picture yet") } else { file_name(&self.picture_path) },
            "picture-tip": self.picture_path,
            "status": text,
            "status-kind": if self.status_text().is_empty() { "status_dim" } else { "status_warn" },
        })
    }

    /// What the status line says: a message from the last pick, else the note about the picture.
    fn status_text(&self) -> String {
        self.status.clone().unwrap_or_else(|| self.note())
    }

    fn show(&self, host: &mut dyn Host) {
        let mut props = self.all_props();
        props["status"] = json!(self.status_text());
        host.set_ui_props("card", &props);
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            self.status = None;
            self.show(host);
        }
    }

    /// _changed: everything the settings decide has to be shown, kept and handed to the stream again.
    fn changed(&mut self, host: &mut dyn Host) {
        self.status = None;
        self.show(host);
        host.schedule_save();
        host.refresh_frame_step();
    }

    // ── Handlers ──────────────────────────────────────────────────────

    fn set_on(&mut self, host: &mut dyn Host, on: bool) {
        self.on = on;
        self.changed(host);
    }

    fn set_screen(&mut self, host: &mut dyn Host, screen: &'static str) {
        // Nothing picked yet: stay on what it was until one comes in
        if screen == "picked" && self.picked.is_none() {
            self.show(host);
            self.pick(host);
            return;
        }
        self.screen = screen;
        self.changed(host);
        if screen == "picked" {
            // after changed, whose status would hide why a pick couldn't start
            self.pick(host);
        }
    }

    fn pick(&mut self, host: &mut dyn Host) {
        if !host.pick_on_preview(NAME, PickKind::Point, None, &tr("Click the screen behind you on the preview. Esc cancels.")) {
            self.status = Some(tr("Start streaming to pick the screen's colour on the preview."));
            self.show(host);
        }
    }

    fn picked_point(&mut self, host: &mut dyn Host, pick: &telescope_core::host::Pick) {
        // The key's own input at the spot clicked: shown is already mapped back past later warps (Desk mode), and this frame comes
        // before anything later changes colours, unlike the preview's picture
        let kept = self.last.lock().unwrap().clone();
        let (frame, at) = if let (Some(frame), Some(at)) = (kept, pick.shown.first()) {
            (frame, *at)
        } else if let (Some(shown), Some(at)) = (&pick.picture, pick.in_picture.first()) {
            (shown.clone(), *at)
        } else {
            return;
        };
        let colour = sample(&frame, at[0] as f64, at[1] as f64);
        if !keyable(colour) {
            self.status = Some(tr("That spot has hardly any colour. Click the screen itself."));
            self.show(host);
            return;
        }
        self.picked = Some(colour);
        self.screen = "picked";
        self.changed(host);
    }

    /// The backdrop colour as a B, G, R: the pad's numbers, or a colour that came from somewhere else and keeps the hue a grey had.
    fn set_colour(&mut self, host: &mut dyn Host, bgr: [u8; 3]) {
        self.colour = bgr;
        self.hsv = rgb_to_hsv([bgr[2], bgr[1], bgr[0]], (self.hsv.0, self.hsv.1));
        self.changed(host);
    }

    fn pick_backdrop(&mut self, host: &mut dyn Host) {
        if !host.pick_on_preview(BACKDROP_OWNER, PickKind::Point, None, &tr("Click a colour on the preview. Esc cancels.")) {
            self.status = Some(tr("Start streaming to pick a colour on the preview."));
            self.show(host);
        }
    }

    fn picked_backdrop(&mut self, host: &mut dyn Host, pick: &telescope_core::host::Pick) {
        // What the preview showed, so the colour is the one that was clicked
        let (Some(shown), Some(at)) = (&pick.picture, pick.in_picture.first()) else { return };
        let bgr = sample(shown, at[0] as f64, at[1] as f64);
        self.set_colour(host, bgr);
    }

    fn set_background(&mut self, host: &mut dyn Host, background: &'static str) {
        self.background = background;
        self.changed(host);
    }

    fn choose_picture(&mut self, host: &mut dyn Host) {
        let start = Path::new(&self.picture_path).parent().filter(|p| !self.picture_path.is_empty() && !p.as_os_str().is_empty())
            .map_or_else(|| std::env::home_dir().unwrap_or_default().display().to_string(), |p| p.display().to_string());
        let dialog = FileDialog { kind: FileDialogKind::Open, title: tr("Choose a picture"), start, filters: vec![tr(PICTURES)] };
        self.asked = host.pick_file(&dialog);
    }

    fn load_picture(&mut self, path: &str) {
        self.picture_path = path.to_string();
        self.picture = if path.is_empty() { None } else { (self.reader)(path).ok().filter(|f| f.width > 0 && f.height > 0) };
        self.picture_problem = if !path.is_empty() && self.picture.is_none() { tr("Telescope can't open that picture.") } else { String::new() };
    }

    // ── The stream ────────────────────────────────────────────────────

    fn tables(&mut self, key: [u8; 3]) -> Arc<Tables> {
        let id = (format!("{key:?}"), self.sliders);
        if let Some((cached, tables)) = &self.cached {
            if *cached == id {
                return tables.clone();
            }
        }
        let tables = Arc::new(key_tables(key, self.sliders[0], self.sliders[1], self.sliders[2]));
        self.cached = Some((id, tables.clone()));
        tables
    }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

impl Plugin for ChromaKeyPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Chroma key").into(),
            about: n_("Swaps a green or blue screen behind you for a colour or a picture, with no download.").into(),
            when_off: n_("Without it there's no Chroma key card and a green screen stays green.").into(),
            category: "look".into(),
            icon: "eyedropper".into(),
            optional: true,
            in_presets: true,
            // the screen behind you is only in that camera's picture
            device_local: true,
            frame_stage: FrameStage::Background,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        host.set_ui_props("card", &ChromaKeyPlugin::fixed_props());
        self.show(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        match event {
            Event::PreviewPickChanged { owner } => {
                let picking = owner == NAME;
                if picking != self.picking {
                    self.picking = picking;
                    if picking {
                        *self.last.lock().unwrap() = None;
                    }
                    host.refresh_frame_step();
                }
            }
            Event::PreviewPicked { owner, pick: Some(pick) } if owner == NAME => self.picked_point(host, pick),
            Event::PreviewPicked { owner, pick: Some(pick) } if owner == BACKDROP_OWNER => self.picked_backdrop(host, pick),
            Event::RequestDone { request, ok, data, .. } if self.asked == Some(*request) => {
                self.asked = None;
                if let (true, Some(path)) = (*ok, data.get("path").and_then(Value::as_str)) {
                    self.load_picture(path);
                    if self.picture_problem.is_empty() {
                        self.background = "picture";
                    }
                    self.changed(host);
                }
            }
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        let mut props = ChromaKeyPlugin::fixed_props();
        if let (Value::Object(p), Value::Object(all)) = (&mut props, self.all_props()) {
            p.extend(all);
        }
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "ChromaKeyCard".into(), props }, region: "right".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        match event.callback.as_str() {
            "switch-toggled-by-hand" => self.set_on(host, event.args.first().and_then(Value::as_bool).unwrap_or(false)),
            "screen-picked" => {
                if let Some((screen, ..)) = SCREENS.get(n(0) as usize) {
                    self.set_screen(host, screen);
                }
            }
            "slider-moved" => {
                let i = (n(0) as usize).min(2);
                self.sliders[i] = (n(1).round() as i64).clamp(0, 100);
                self.changed(host);
            }
            "background-picked" => {
                if let Some((background, _)) = BACKGROUNDS.get(n(0) as usize) {
                    self.set_background(host, background);
                }
            }
            "colour-moved" => {
                let unit = |i: usize| n(i).clamp(0.0, 1.0) as f32;
                self.hsv = (unit(0), unit(1), unit(2));
                let [r, g, b] = hsv_to_rgb(self.hsv.0, self.hsv.1, self.hsv.2);
                self.colour = [b, g, r];
                self.changed(host);
            }
            "hex-typed" => {
                let typed = event.args.first().and_then(Value::as_str).and_then(parse_hex);
                match typed {
                    Some([r, g, b]) => self.set_colour(host, [b, g, r]),
                    // the box goes back to the colour it had
                    None => self.show(host),
                }
            }
            "channel-typed" => {
                let typed = event.args.get(1).and_then(Value::as_str).and_then(|t| t.trim().parse::<i64>().ok());
                match typed {
                    Some(value) => {
                        let mut colour = self.colour;
                        colour[2 - (n(0) as usize).min(2)] = value.clamp(0, 255) as u8;
                        self.set_colour(host, colour);
                    }
                    None => self.show(host),
                }
            }
            "pipette-clicked" => self.pick_backdrop(host),
            "picture-clicked" => self.choose_picture(host),
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Chroma key");
        let screens: Vec<(&str, String)> = SCREENS.iter().map(|(k, t, _)| (*k, tr(t))).collect();
        let spec = SliderSpec { id: "chroma_key.tolerance", label: &tr("Chroma key tolerance"), group: &group, keywords: "", minimum: 0.0, maximum: 100.0, step: 5.0, decimals: 0, suffix: "", has_default: true };
        vec![
            actions::toggle_action("chroma_key.on", &tr("Chroma key"), &group, "", &[]),
            actions::choice_action("chroma_key.screen", &tr("Chroma key screen"), &group, "", &screens),
            actions::slider_action(&spec, self.sliders[0] as f64),
        ]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        // The card is dimmed until a phone is paired, and its shortcuts have nothing to do
        if !self.paired.on() {
            return None;
        }
        match id {
            "chroma_key.on" => {
                let want = actions::toggle_wanted(params, self.on, &mut self.held);
                if want != self.on {
                    self.set_on(host, want);
                }
                Some(actions::toggle_text(&tr("Chroma key"), want))
            }
            "chroma_key.screen" => {
                let shown: Vec<String> = SCREENS.iter().map(|(_, t, _)| tr(t)).collect();
                let options: Vec<Choice> = SCREENS.iter().zip(&shown).map(|((k, ..), s)| Choice { value: k, shown: s, enabled: true }).collect();
                let (said, pick) = actions::choice_target(params, &options, Some(self.screen_index()), true)?;
                if pick {
                    self.set_screen(host, SCREENS[said].0);
                }
                Some(actions::slider_text(&tr("Chroma key screen"), &shown[said]))
            }
            "chroma_key.tolerance" => {
                let target = actions::slider_target(params, self.sliders[0], 1.0, 0, 100, Some(SLIDERS[0].1))?;
                self.sliders[0] = target;
                self.changed(host);
                Some(actions::slider_text(&tr("Chroma key tolerance"), &target.to_string()))
            }
            _ => None,
        }
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        if id != "chroma_key.on" {
            return;
        }
        if let Some(before) = actions::hold_released(params, &mut self.held) {
            if before != self.on && self.paired.on() {
                self.set_on(host, before);
            }
        }
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        if self.picking {
            return Some(FrameStep::Cpu(Box::new(Tap(self.last.clone()))));
        }
        if !self.on {
            return None;
        }
        let key = self.key_colour()?;
        let keys = self.tables(key).texture();
        let (picture, size, use_picture) = match (&self.picture, self.background) {
            (Some(p), "picture") => (p.clone(), [p.width as f32, p.height as f32], 1.0),
            _ => (Frame::new(1, 1, PixelFormat::Rgba, vec![0, 0, 0, 255]), [1.0, 1.0], 0.0),
        };
        let [b, g, r] = self.colour;
        Some(FrameStep::Shader(ShaderStep {
            wgsl: WGSL.into(),
            uniforms: json!({"backdrop": [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, use_picture], "picture": size}),
            textures: vec![("keys".into(), keys), ("picture".into(), picture)],
        }))
    }

    fn get_config(&self) -> Value {
        let mut cfg = Map::new();
        cfg.insert("on".into(), json!(self.on));
        cfg.insert("screen".into(), json!(self.screen));
        cfg.insert("picked".into(), self.picked.map_or(Value::Null, |p| json!(p)));
        for (i, (name, _)) in SLIDERS.iter().enumerate() {
            cfg.insert((*name).into(), json!(self.sliders[i]));
        }
        cfg.insert("background".into(), json!(self.background));
        cfg.insert("colour".into(), json!(self.colour));
        cfg.insert("picture".into(), json!(self.picture_path));
        Value::Object(cfg)
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let empty = Value::Object(Map::new());
        let cfg = if cfg.is_object() { cfg } else { &empty };
        self.picked = bgr(cfg.get("picked")).filter(|p| keyable(*p));
        let screen = cfg.get("screen").and_then(Value::as_str).and_then(|s| SCREENS.iter().find(|(k, ..)| *k == s)).map_or("green", |(k, ..)| *k);
        self.screen = if screen == "picked" && self.picked.is_none() { "green" } else { screen };
        self.colour = bgr(cfg.get("colour")).unwrap_or(DEFAULT_COLOUR);
        self.hsv = rgb_to_hsv([self.colour[2], self.colour[1], self.colour[0]], (self.hsv.0, self.hsv.1));
        let picture = cfg.get("picture").and_then(Value::as_str).unwrap_or("").to_string();
        self.load_picture(&picture);
        self.background = cfg.get("background").and_then(Value::as_str).and_then(|b| BACKGROUNDS.iter().find(|(k, _)| *k == b)).map_or("colour", |(k, _)| *k);
        self.on = cfg.get("on") == Some(&Value::Bool(true));
        for (i, (name, default)) in SLIDERS.iter().enumerate() {
            self.sliders[i] = clamped_int(cfg.get(*name).unwrap_or(&Value::Null), *default, 0, 100);
        }
        self.status = None;
        // once, with the whole of it: the next hook with a host shows it
        self.pending = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("Chroma key".into(), format!("{}, {} screen, {} behind", if self.on { "on" } else { "off" }, self.screen, self.background))]
    }
}

#[cfg(test)]
#[path = "chroma_key_tests.rs"]
mod tests;
