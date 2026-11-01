//! The Background plugin (plugins/background.py): blurs what's behind you or swaps it for a picture. It's a CPU step on purpose: the person mask only reaches a frame step through FrameContext, and a shader step has no way to ask for it.

use crate::compositor::cover;
use crate::cpu::Rgb;
use crate::shared::mask_of;
use crate::imgops::{blur_quick, compose, Gray};
use crate::shared::{no_pictures, Paired, PictureReader};
use crate::vision_row;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::Arc;
use telescope_core::actions::{self, Choice, SliderSpec};
use telescope_core::config::clamped_int;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep};
use telescope_core::host::{FileDialog, FileDialogKind, Host, RequestId};
use telescope_core::i18n::{n_, tr};
use telescope_core::image::with_spares;
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "background";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/vision_status.slint"), "\n", include_str!("../ui/background.slint"));
pub const MODES: [(&str, &str); 3] = [("off", n_("Off")), ("blur", n_("Blur")), ("picture", n_("Picture"))];
const PICTURES: &str = n_("Pictures (*.png *.jpg *.jpeg *.webp *.bmp)");
/// Edges are worked out at a quarter of the size, then scaled back up.
const SMALL: f64 = 4.0;
/// And the blurred background at an eighth, since it's that blurry anyway.
const BLUR_SMALL: usize = 8;
const DEFAULT_BLUR: i64 = 6;
const DEFAULT_EDGES: i64 = 3;

/// The frame blurred (strength 1..10), at an eighth of the size and scaled back up. With the person mask, the person is left out of the blur, so they don't smear into a dark halo around themselves.
pub fn blurred(frame: &Rgb, strength: i64, mask: Option<&Gray>) -> Rgb {
    let (w, h) = (frame.w, frame.h);
    let (sw, sh) = ((w / BLUR_SMALL).max(1), (h / BLUR_SMALL).max(1));
    let small = frame.shrink(sw, sh);
    let sigma = (1.0 + strength as f64 * 1.2) * SMALL / BLUR_SMALL as f64;
    let plain = small.blur(sigma);
    let Some(mask) = mask else { return plain.linear_exact(w, h) };
    let rest = mask.shrink(sw, sh);
    if rest.data.iter().all(|m| *m == 0) {
        // nobody to leave out
        return plain.linear_exact(w, h);
    }
    let rest: Vec<f32> = rest.data.iter().map(|m| 1.0 - *m as f32 / 255.0).collect();
    let spread = blur_quick(&rest, sw, sh, 1, sigma);
    let kept: Vec<f32> = small.data.as_chunks::<3>().0.iter().zip(&rest).flat_map(|(p, r)| p.map(|v| v as f32 * r)).collect();
    let total = blur_quick(&kept, sw, sh, 3, sigma);
    let mut out = Rgb::scratch(sw, sh);
    let colours = out.data.as_chunks_mut::<3>().0.iter_mut().zip(total.as_chunks::<3>().0).zip(plain.data.as_chunks::<3>().0);
    for ((o, (v, p)), s) in colours.map(|((o, v), p)| (o, (v, p))).zip(&spread) {
        // where there's next to no background, the plain blur
        let weight = (s * 4.0).clamp(0.0, 1.0);
        for c in 0..3 {
            o[c] = (v[c] / s.max(1e-3) * weight + p[c] as f32 * (1.0 - weight)).clamp(0.0, 255.0) as u8;
        }
    }
    out.linear_exact(w, h)
}

/// What a frame step needs, frozen: a stream behind the panels keeps the ones it was left with.
#[derive(Clone)]
struct Settings {
    mode: usize,
    blur: i64,
    edges: i64,
    picture: Option<Arc<Rgb>>,
}

struct Step {
    settings: Settings,
    /// The picture scaled to cover this frame's size
    covered: Option<Rgb>,
    spares: Vec<Vec<u8>>,
}

impl CpuStep for Step {
    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        let mut spares = std::mem::take(&mut self.spares);
        with_spares(&mut spares, || self.run(frame, ctx));
        self.spares = spares;
    }
}

impl Step {
    fn run(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        if self.settings.mode == 0 {
            return;
        }
        let Some(mask) = mask_of(ctx, frame) else { return };
        let Some(rgb) = Rgb::from_frame(frame) else { return };
        let edges = mask.soften(self.settings.edges);
        match (MODES[self.settings.mode].0, &self.settings.picture) {
            ("blur", _) => compose(&rgb, &edges, &blurred(&rgb, self.settings.blur, Some(&mask))).write_to(frame),
            ("picture", Some(picture)) => {
                if self.covered.as_ref().is_some_and(|c| (c.w, c.h) != (rgb.w, rgb.h)) {
                    self.covered = None;
                }
                let covered = self.covered.get_or_insert_with(|| cover(picture, rgb.w, rgb.h));
                compose(&rgb, &edges, covered).write_to(frame);
            }
            _ => {}
        }
    }
}

pub struct BackgroundPlugin {
    mode: usize,
    blur: i64,
    edges: i64,
    picture_path: String,
    picture: Option<Arc<Rgb>>,
    picture_problem: String,
    paired: Paired,
    reader: PictureReader,
    asked: Option<RequestId>,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
}

impl Default for BackgroundPlugin {
    fn default() -> Self {
        BackgroundPlugin::new()
    }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

impl BackgroundPlugin {
    pub fn new() -> BackgroundPlugin {
        BackgroundPlugin::with_reader(no_pictures())
    }

    /// With the reader that turns a picture file into a frame (shared::picture_reader of the app's Media).
    pub fn with_reader(reader: PictureReader) -> BackgroundPlugin {
        BackgroundPlugin {
            mode: 0,
            blur: DEFAULT_BLUR,
            edges: DEFAULT_EDGES,
            picture_path: String::new(),
            picture: None,
            picture_problem: String::new(),
            paired: Paired::default(),
            reader,
            asked: None,
            pending: true,
        }
    }

    fn mode_name(&self) -> &'static str {
        MODES[self.mode].0
    }

    fn fixed_props() -> Value {
        json!({
            "reset-hint": tr("Double-click to reset"),
            "edges-tip": tr("How soft the edge around you is"),
            "picture-choose-tip": tr("Pick the picture that goes behind you"),
        })
    }

    fn all_props(&self) -> Value {
        json!({
            "active": self.paired.on(),
            "mode": self.mode,
            "blur": self.blur,
            "blur-text": self.blur.to_string(),
            "edges": self.edges,
            "edges-text": self.edges.to_string(),
            "picture-text": if self.picture_path.is_empty() { tr("No picture yet") } else { file_name(&self.picture_path) },
            "picture-tip": self.picture_path,
        })
    }

    /// _show_status: the card's own reason for the row, if there is one.
    fn problem(&self) -> String {
        if self.mode_name() != "picture" {
            String::new()
        } else if !self.picture_problem.is_empty() {
            self.picture_problem.clone()
        } else if self.picture.is_none() {
            tr("Choose a picture to put behind you.")
        } else {
            String::new()
        }
    }

    fn show(&self, host: &mut dyn Host) {
        let mut props = self.all_props();
        if let (Value::Object(p), Value::Object(row)) = (&mut props, vision_row::props(&*host, self.mode != 0, &self.problem())) {
            p.extend(row);
        }
        host.set_ui_props("card", &props);
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            self.show(host);
        }
    }

    /// Everything the settings decide has to be shown, kept and handed to the stream again.
    fn changed(&mut self, host: &mut dyn Host) {
        self.show(host);
        host.schedule_save();
        host.refresh_frame_step();
    }

    fn choose_picture(&mut self, host: &mut dyn Host) {
        let start = Path::new(&self.picture_path).parent().filter(|p| !self.picture_path.is_empty() && !p.as_os_str().is_empty())
            .map_or_else(|| std::env::home_dir().unwrap_or_default().display().to_string(), |p| p.display().to_string());
        let dialog = FileDialog { kind: FileDialogKind::Open, title: tr("Choose a picture"), start, filters: vec![tr(PICTURES)] };
        self.asked = host.pick_file(&dialog);
    }

    fn load_picture(&mut self, path: &str) {
        self.picture_path = path.to_string();
        let read = if path.is_empty() { None } else { (self.reader)(path).ok().filter(|f| f.width > 0 && f.height > 0) };
        self.picture = read.and_then(|f| Rgb::from_frame(&f)).map(Arc::new);
        self.picture_problem = if !path.is_empty() && self.picture.is_none() { tr("Telescope can't open that picture.") } else { String::new() };
    }
}

impl Plugin for BackgroundPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Background").into(),
            about: n_("Blurs the background behind you or swaps it for a picture.").into(),
            when_off: n_("Without it there's no Background card and the background stays as the camera sees it.").into(),
            category: "look".into(),
            icon: "image".into(),
            optional: true,
            needs_vision: true,
            in_presets: true,
            // a blur or picture behind you on one camera, not on the desk camera too
            device_local: true,
            frame_stage: FrameStage::Background,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        host.set_ui_props("card", &BackgroundPlugin::fixed_props());
        self.show(host);
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        if vision_row::changes_it(event) {
            self.show(host);
        }
        if let Event::RequestDone { request, ok, data, .. } = event {
            if self.asked == Some(*request) {
                self.asked = None;
                if let (true, Some(path)) = (*ok, data.get("path").and_then(Value::as_str)) {
                    self.load_picture(path);
                    if self.picture_problem.is_empty() {
                        self.mode = 2;
                    }
                    self.changed(host);
                }
            }
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        let mut props = BackgroundPlugin::fixed_props();
        if let (Value::Object(p), Value::Object(all)) = (&mut props, self.all_props()) {
            p.extend(all);
        }
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "BackgroundCard".into(), props }, region: "right".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        match event.callback.as_str() {
            "mode-picked" => {
                if (n(0) as usize) < MODES.len() {
                    self.mode = n(0) as usize;
                    self.changed(host);
                }
            }
            "blur-moved" => {
                self.blur = (n(0).round() as i64).clamp(1, 10);
                self.changed(host);
            }
            "edges-moved" => {
                self.edges = (n(0).round() as i64).clamp(0, 10);
                self.changed(host);
            }
            "picture-clicked" => self.choose_picture(host),
            "download-clicked" => {
                host.download_vision();
                self.show(host);
            }
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Background");
        let modes: Vec<(&str, String)> = MODES.iter().map(|(k, t)| (*k, tr(t))).collect();
        let spec = SliderSpec { id: "background.blur", label: &tr("Background blur"), group: &group, keywords: "", minimum: 1.0, maximum: 10.0, step: 1.0, decimals: 0, suffix: "", has_default: true };
        vec![actions::choice_action("background.mode", &tr("Background"), &group, "", &modes), actions::slider_action(&spec, self.blur as f64)]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        // The card is dimmed until a phone is paired, and its shortcuts have nothing to do
        if !self.paired.on() {
            return None;
        }
        match id {
            "background.mode" => {
                let shown: Vec<String> = MODES.iter().map(|(_, t)| tr(t)).collect();
                let options: Vec<Choice> = MODES.iter().zip(&shown).map(|((k, _), s)| Choice { value: k, shown: s, enabled: true }).collect();
                let (said, pick) = actions::choice_target(params, &options, Some(self.mode), true)?;
                if pick {
                    self.mode = said;
                    self.changed(host);
                }
                Some(actions::slider_text(&tr("Background"), &shown[said]))
            }
            // The Blur row only shows with Blur picked
            "background.blur" if self.mode_name() == "blur" => {
                let target = actions::slider_target(params, self.blur, 1.0, 1, 10, Some(DEFAULT_BLUR))?;
                self.blur = target;
                self.changed(host);
                Some(actions::slider_text(&tr("Background blur"), &target.to_string()))
            }
            _ => None,
        }
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        if self.mode == 0 {
            return None;
        }
        let settings = Settings { mode: self.mode, blur: self.blur, edges: self.edges, picture: self.picture.clone() };
        Some(FrameStep::Cpu(Box::new(Step { settings, covered: None, spares: Vec::new() })))
    }

    fn get_config(&self) -> Value {
        let mut cfg = Map::new();
        cfg.insert("mode".into(), json!(self.mode_name()));
        cfg.insert("blur".into(), json!(self.blur));
        cfg.insert("edges".into(), json!(self.edges));
        cfg.insert("picture".into(), json!(self.picture_path));
        Value::Object(cfg)
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let empty = Value::Object(Map::new());
        let cfg = if cfg.is_object() { cfg } else { &empty };
        self.mode = cfg.get("mode").and_then(Value::as_str).and_then(|m| MODES.iter().position(|(k, _)| *k == m)).unwrap_or(0);
        self.blur = clamped_int(cfg.get("blur").unwrap_or(&Value::Null), DEFAULT_BLUR, 1, 10);
        self.edges = clamped_int(cfg.get("edges").unwrap_or(&Value::Null), DEFAULT_EDGES, 0, 10);
        let picture = cfg.get("picture").and_then(Value::as_str).unwrap_or("").to_string();
        self.load_picture(&picture);
        // once, with the whole of it: the next hook with a host shows it
        self.pending = true;
        Ok(())
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.set_config(cfg)?;
        self.catch_up(host);
        host.refresh_frame_step();
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("Background".into(), self.mode_name().into())]
    }
}

#[cfg(test)]
#[path = "background_tests.rs"]
pub(crate) mod tests;
