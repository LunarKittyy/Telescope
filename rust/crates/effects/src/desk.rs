//! The Desk mode plugin (plugins/desk.py): four corners of a sheet marked on the preview, straightened with a perspective warp
//! in the geometry stage. The warp for a frame size is worked out on the stream's thread and posted back, so the plugin can tell
//! the preview where each click on the straightened picture really lands (host.set_preview_warp).

use crate::imgops::Remap;
use crate::pixels::{put_rgb, to_rgb};
use crate::sheet::{self, card_rows, usable_corners, warp_for, warp_map, Sheet, Warp, BACKDROP, P, STRETCH_MAX, STRETCH_MIN};
use crate::shared::Paired;
use serde_json::{json, Value};
use std::sync::Arc;
use telescope_core::actions::{self, round_half_even, Choice};
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, ShaderStep};
use telescope_core::host::{Host, PickKind, Poster, View};
use telescope_core::i18n::{n_, tr};
use telescope_core::image::{keep_bytes, with_spares};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::view::{view_to_shown, Geometry};

pub const NAME: &str = "desk";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/desk.slint"));
const T_WARP: &str = "warp";
const DESK: &str = include_str!("desk.wgsl");

/// What "make the writing stand out" does to the sheet: nothing, more contrast, or black and white.
pub const CLEANUPS: [(&str, &str); 3] = [("off", n_("Off")), ("contrast", n_("Contrast")), ("bw", n_("Black and white"))];
/// The sheet's shape: worked out from its corners, a paper size, square, or stretched over the picture.
pub const SHAPES: [(&str, &str); 5] = [("auto", n_("Auto")), ("a4", n_("A4")), ("letter", n_("Letter")), ("square", n_("Square")), ("fill", n_("Fill the picture"))];
/// The English texts the shortcut editor names a choice by.
const CLEAN_VALUES: [&str; 3] = ["Off", "Contrast", "Black and white"];
const SHAPE_VALUES: [&str; 5] = ["Auto", "A4", "Letter", "Square", "Fill the picture"];

const SWITCH_TIP: &str = n_("Straighten the marked sheet.");
const MARK_TIP: &str = n_("Drag the four corners of the sheet into place on the preview.");
const RESET_TIP: &str = n_("Set the width and height back to 100%.");
const CLEAN_TIP: &str = n_("Make the writing on the sheet stand out.");
const SHAPE_TIP: &str = n_("Work out the shape from the corners.");
const SHAPE_FILL_TIP: &str = n_("Fill the whole picture with the sheet.");
const STRETCH_TIP: &str = n_("Stretch the sheet if it still looks squashed.");
const STRETCH_OFF_TIP: &str = n_("Fill already uses the whole picture.");
/// What the card's shape tiles are called: the same shapes, with Fill short and its full name in the tile's tooltip.
const SHAPE_NAMES: [&str; 5] = [n_("Auto"), n_("A4"), n_("Letter"), n_("Square"), n_("Fill")];

/// The frame step: the sheet, and the warp for the size of frame it saw last.
struct DeskStep {
    spares: Vec<Vec<u8>>,
    sheet: Sheet,
    made: Option<((usize, usize), Warp, Option<Remap>)>,
    poster: Option<Arc<dyn Poster>>,
}

impl CpuStep for DeskStep {
    fn process(&mut self, frame: &mut Frame, _ctx: &dyn FrameContext) {
        let mut spares = std::mem::take(&mut self.spares);
        with_spares(&mut spares, || self.run(frame));
        self.spares = spares;
    }

    fn card_uniforms(&mut self, width: u32, height: u32) -> Option<Value> {
        let size = (width as usize, height as usize);
        if size.0 < 2 || size.1 < 2 {
            // The CPU leaves such a frame alone: the identity warp over all of it does the same
            return Some(card_params([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]], (0, 0, size.0, size.1)));
        }
        self.warp_for_size(size);
        let (_, warp, _) = self.made.as_ref().expect("made above");
        Some(card_params(card_rows(warp), warp.rect))
    }
}

/// The card's uniforms (desk.wgsl's Params) for a warp.
fn card_params(rows: [[f64; 4]; 3], rect: (usize, usize, usize, usize)) -> Value {
    json!({"r0": rows[0], "r1": rows[1], "r2": rows[2], "rect": [rect.0, rect.1, rect.2, rect.3], "backdrop": BACKDROP as f64 / 255.0})
}

impl DeskStep {
    /// The warp for this size, worked out and posted to the preview the first time the size comes; its pixel map is made when the CPU needs it.
    fn warp_for_size(&mut self, size: (usize, usize)) {
        if self.made.as_ref().is_none_or(|(s, _, _)| *s != size) {
            let warp = warp_for(&self.sheet, size.0, size.1);
            if let Some(poster) = &self.poster {
                let m = &warp.on_preview;
                poster.post(T_WARP, json!({"matrix": [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]]}));
            }
            self.made = Some((size, warp, None));
        }
    }

    fn run(&mut self, frame: &mut Frame) {
        let size = (frame.width as usize, frame.height as usize);
        if size.0 < 2 || size.1 < 2 {
            return;
        }
        self.warp_for_size(size);
        let Some(rgb) = to_rgb(frame) else { return };
        let (_, warp, map) = self.made.as_mut().expect("made above");
        let map = map.get_or_insert_with(|| warp_map(size.0, size.1, &warp.matrix, warp.rect.2, warp.rect.3));
        let out = sheet::straighten_with(&rgb, size.0, size.1, &self.sheet, warp, map);
        put_rgb(frame, &out);
        keep_bytes(rgb);
        keep_bytes(out);
    }
}

pub struct DeskPlugin {
    on: bool,
    corners: Option<[P; 4]>,
    cleanup: String,
    shape: String,
    stretch: [i32; 2],
    view: View,
    /// The warp waits while the corners are being placed, so they go on the real picture.
    picking: bool,
    /// The last corners marked didn't make a sheet, so the card says so.
    refused: bool,
    poster: Option<Arc<dyn Poster>>,
    paired: Paired,
    held: Option<bool>,
    /// set_config has no host: the card and the frames hear the settings with the next hook that has one.
    pending: bool,
}

impl Default for DeskPlugin {
    fn default() -> Self {
        DeskPlugin::new()
    }
}

fn index_of(list: &[(&str, &str)], key: &str) -> usize {
    list.iter().position(|(k, _)| *k == key).unwrap_or(0)
}

/// Two numbers in a list that a settings file can hold (not booleans, which Python counts as numbers but this doesn't).
fn pair_of(v: Option<&Value>) -> Option<[f64; 2]> {
    let a = v?.as_array().filter(|a| a.len() == 2)?;
    let n = |x: &Value| x.as_f64().filter(|f| f.is_finite() && x.is_number());
    Some([n(&a[0])?, n(&a[1])?])
}

fn corners_of(v: Option<&Value>) -> Option<[P; 4]> {
    let a = v?.as_array().filter(|a| a.len() == 4)?;
    let mut out = [[0.0; 2]; 4];
    for (slot, p) in out.iter_mut().zip(a) {
        *slot = pair_of(Some(p))?;
    }
    Some(out)
}

impl DeskPlugin {
    pub fn new() -> DeskPlugin {
        DeskPlugin {
            on: false,
            corners: None,
            cleanup: "off".into(),
            shape: "auto".into(),
            stretch: [100, 100],
            view: View::default(),
            picking: false,
            refused: false,
            poster: None,
            paired: Paired::default(),
            held: None,
            pending: false,
        }
    }

    fn geometry(&self) -> Geometry {
        Geometry::from(&self.view)
    }

    fn sheet(&self) -> Option<Sheet> {
        Some(Sheet { corners: self.corners?, cleanup: self.cleanup.clone(), shape: self.shape.clone(), stretch: self.stretch, geometry: self.geometry() })
    }

    /// Whether frames are being straightened now.
    fn active(&self) -> bool {
        self.on && self.corners.is_some() && !self.picking
    }

    /// A corner sits outside the frame as shown (zoomed in past it, or Auto framing moved the view).
    fn off_picture(&self) -> bool {
        let g = self.geometry();
        self.corners.iter().flatten().any(|[x, y]| {
            let (u, v) = view_to_shown(*x, *y, &g);
            [u, v].iter().any(|c| !(-0.01..=1.01).contains(c))
        })
    }

    /// (a short word, the sentence for its tooltip, its kind).
    fn status(&self) -> (String, String, &'static str) {
        // The preview says what to drag while the corners are placed, and the Mark button is the way in
        if self.picking {
            (String::new(), String::new(), "dim")
        } else if self.refused {
            (tr("Sheet unclear"), tr("Those corners were bunched up or in a line and weren't used. Mark the sheet again with one on each of its corners."), "status_warn")
        } else if self.on && self.off_picture() {
            (tr("Sheet cut off"), tr("Part of the sheet is outside the picture. Zoom out or mark it again."), "status_warn")
        } else {
            (String::new(), String::new(), "dim")
        }
    }

    fn fixed_props() -> Value {
        let list = |l: &[(&str, &str)]| l.iter().map(|(_, t)| tr(t)).collect::<Vec<_>>();
        // Fill is named in full in its tooltip, and Auto and Fill say what they do
        let shape_tips = [String::new(), String::new(), String::new(), String::new(), tr("Fill the picture")];
        let shape_details = [tr(SHAPE_TIP), String::new(), String::new(), String::new(), tr(SHAPE_FILL_TIP)];
        json!({
            "switch-name": tr("Desk mode"), "switch-help": tr(SWITCH_TIP), "mark-tip": tr(MARK_TIP), "done-text": tr("Done"),
            "cancel-text": tr("Cancel"), "reset-text": tr("Reset"), "reset-help": tr(RESET_TIP), "clean-items": list(&CLEANUPS),
            "clean-tip": tr(CLEAN_TIP), "shape-items": SHAPE_NAMES.iter().map(|t| tr(t)).collect::<Vec<_>>(), "shape-tips": shape_tips, "shape-details": shape_details,
            "stretch-tip": tr(STRETCH_TIP), "stretch-off-tip": tr(STRETCH_OFF_TIP),
        })
    }

    fn state_props(&self) -> Value {
        let fill = self.shape == "fill";
        let (status, detail, kind) = self.status();
        json!({
            "active": self.paired.on(), "on": self.on, "picking": self.picking,
            "mark-text": if self.corners.is_some() { tr("Mark the sheet again") } else { tr("Mark the sheet") },
            "reset-shown": !fill && self.stretch != [100, 100],
            "clean-current": index_of(&CLEANUPS, &self.cleanup), "shape-current": index_of(&SHAPES, &self.shape),
            "stretch-enabled": !fill, "stretch": self.stretch, "stretch-text": self.stretch.map(|v| format!("{v}%")),
            "status": status, "status-detail": detail, "status-kind": if kind == "dim" { "status_dim" } else { kind },
        })
    }

    fn all_props(&self) -> Value {
        let mut all = Self::fixed_props();
        if let (Value::Object(a), Value::Object(s)) = (&mut all, self.state_props()) {
            a.extend(s);
        }
        all
    }

    fn show(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.state_props());
    }

    /// The frames get a step for the settings as they are, and the preview forgets a warp that no longer applies.
    fn restep(&self, host: &mut dyn Host) {
        host.refresh_frame_step();
        if !self.active() {
            host.set_preview_warp(NAME, None);
        }
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            host.set_ui_props("card", &self.all_props());
            self.restep(host);
        }
    }

    /// Something about the sheet changed: the card, the frames and the file hear of it.
    fn changed(&self, host: &mut dyn Host, save: bool) {
        self.show(host);
        self.restep(host);
        if save {
            host.schedule_save();
        }
    }

    fn switch(&mut self, host: &mut dyn Host, on: bool) {
        if on && self.corners.is_none() {
            self.on = true;
            self.changed(host, false);
            self.mark(host);
        } else {
            self.on = on;
            self.changed(host, false);
        }
        host.schedule_save();
    }

    fn mark(&mut self, host: &mut dyn Host) {
        let start = self.corners.map(|c| c.map(|[x, y]| [x as f32, y as f32]));
        if !host.pick_on_preview(NAME, PickKind::Quad, start, &tr("Drag the corners onto the sheet's corners, then press Enter.")) {
            let mut props = self.state_props();
            props["status"] = json!(tr("Not streaming"));
            props["status-detail"] = json!(tr("Start streaming to mark the sheet on the preview."));
            props["status-kind"] = json!("status_warn");
            host.set_ui_props("card", &props);
        }
    }

    fn picked(&mut self, host: &mut dyn Host, full: Option<&[[f32; 2]]>) {
        let Some(full) = full else {
            self.show(host);
            return;
        };
        let points: Option<[P; 4]> = full.get(..4).map(|f| [0, 1, 2, 3].map(|i| [f[i][0] as f64, f[i][1] as f64]));
        let Some(points) = points.filter(usable_corners) else {
            self.refused = true;
            self.show(host);
            return;
        };
        self.refused = false;
        self.corners = Some(points);
        self.on = true;
        self.changed(host, true);
    }

    fn set_cleanup(&mut self, host: &mut dyn Host, index: usize) {
        self.cleanup = CLEANUPS.get(index).map_or("off", |(k, _)| k).into();
        self.changed(host, true);
    }

    fn set_shape(&mut self, host: &mut dyn Host, index: usize) {
        self.shape = SHAPES.get(index).map_or("auto", |(k, _)| k).into();
        self.changed(host, true);
    }

    fn set_stretch(&mut self, host: &mut dyn Host, which: usize, value: f64) {
        self.stretch[which.min(1)] = (round_half_even(value) as i32).clamp(STRETCH_MIN, STRETCH_MAX);
        self.changed(host, true);
    }
}

impl Plugin for DeskPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Desk and document mode").into(),
            about: n_("Straightens a sheet of paper you mark on the preview and can make the writing on it stand out.").into(),
            when_off: n_("Without it there's no Desk mode card and the picture stays the way the camera sees it.").into(),
            category: "camera".into(),
            icon: "pencil".into(),
            frame_stage: FrameStage::Geometry,
            optional: true,
            in_presets: true,
            // Where the sheet sits depends on where that phone is
            device_local: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.view = host.view();
        self.poster = host.poster();
        host.set_ui_props("card", &self.all_props());
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        match event {
            Event::ViewChanged { view } => {
                self.view = view.clone();
                self.changed(host, false);
            }
            Event::PreviewPickChanged { owner } => {
                self.picking = owner == NAME;
                if self.picking {
                    self.refused = false;
                }
                self.changed(host, false);
            }
            Event::PreviewPicked { owner, pick } if owner == NAME => {
                self.picked(host, pick.as_ref().map(|p| p.full.as_slice()));
            }
            // A warp from a step that has been replaced since is of no use
            Event::Posted { owner, token, data } if owner == NAME && token == T_WARP && self.active() => {
                let m: Vec<f32> = data["matrix"].as_array().map(|a| a.iter().filter_map(|v| v.as_f64()).map(|v| v as f32).collect()).unwrap_or_default();
                if let Ok(matrix) = <[f32; 9]>::try_from(m) {
                    host.set_preview_warp(NAME, Some(matrix));
                }
            }
            _ => {}
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "DeskCard".into(), props: self.all_props() }, region: "left".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        match event.callback.as_str() {
            "switch-toggled-by-hand" => {
                let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.switch(host, on);
            }
            "mark-clicked" => self.mark(host),
            "done-clicked" => {
                host.finish_pick(NAME);
            }
            "cancel-clicked" => host.cancel_pick(NAME),
            "reset-clicked" => {
                self.stretch = [100, 100];
                self.changed(host, true);
            }
            "clean-picked" => self.set_cleanup(host, n(0).max(0.0) as usize),
            "shape-picked" => self.set_shape(host, n(0).max(0.0) as usize),
            "stretch-moved" => self.set_stretch(host, n(0) as usize, n(1)),
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr("Desk and document mode");
        let choices = |values: &[&str], list: &[(&str, &str)]| -> Vec<(String, String)> { values.iter().zip(list).map(|(v, (_, t))| (v.to_string(), tr(t))).collect() };
        fn as_pairs(c: &[(String, String)]) -> Vec<(&str, String)> {
            c.iter().map(|(v, t)| (v.as_str(), t.clone())).collect()
        }
        let (clean, shape) = (choices(&CLEAN_VALUES, &CLEANUPS), choices(&SHAPE_VALUES, &SHAPES));
        vec![
            actions::toggle_action("desk.on", &tr("Desk mode"), &group, "", &[]),
            ActionSpec { id: "desk.mark".into(), label: tr("Mark the sheet"), group: group.clone(), params: Vec::new(), repeats: false, on_release: false, current: None, keywords: String::new() },
            actions::choice_action("desk.writing", &tr("Make the writing stand out"), &group, "", &as_pairs(&clean)),
            actions::choice_action("desk.shape", &tr("Sheet shape"), &group, "", &as_pairs(&shape)),
        ]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        if !self.paired.on() {
            return None;
        }
        match id {
            "desk.on" => {
                let want = actions::toggle_wanted(params, self.on, &mut self.held);
                if want != self.on {
                    self.switch(host, want);
                }
                Some(actions::toggle_text(&tr("Desk mode"), want))
            }
            "desk.mark" if !self.picking => {
                self.mark(host);
                Some(tr("Mark the sheet"))
            }
            "desk.writing" | "desk.shape" => {
                let (values, list, current, label) = if id == "desk.writing" {
                    (&CLEAN_VALUES[..], &CLEANUPS[..], index_of(&CLEANUPS, &self.cleanup), tr("Make the writing stand out"))
                } else {
                    (&SHAPE_VALUES[..], &SHAPES[..], index_of(&SHAPES, &self.shape), tr("Sheet shape"))
                };
                let shown: Vec<String> = list.iter().map(|(_, t)| tr(t)).collect();
                let options: Vec<Choice> = values.iter().zip(&shown).map(|(v, s)| Choice { value: v, shown: s, enabled: true }).collect();
                let (said, pick) = actions::choice_target(params, &options, Some(current), true)?;
                if pick {
                    if id == "desk.writing" { self.set_cleanup(host, said) } else { self.set_shape(host, said) }
                }
                Some(actions::slider_text(&label, &shown[said]))
            }
            _ => None,
        }
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        if id != "desk.on" {
            return;
        }
        if let Some(before) = actions::hold_released(params, &mut self.held) {
            if before != self.on && self.paired.on() {
                self.switch(host, before);
            }
        }
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        host.cancel_pick(NAME);
        host.set_preview_warp(NAME, None);
    }

    fn frame_step(&mut self) -> Option<FrameStep> {
        // While the corners are being placed the frames pass untouched: they go on the real picture
        if !self.active() {
            return None;
        }
        let sheet = self.sheet()?;
        // Cleaning the sheet up works on its pixels, so only the plain straightening can run on the card
        let plain = !matches!(sheet.cleanup.as_str(), "contrast" | "bw");
        let step = Box::new(DeskStep { sheet, made: None, poster: self.poster.clone(), spares: Vec::new() });
        if plain {
            return Some(FrameStep::Either(ShaderStep { wgsl: DESK.into(), uniforms: card_params([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]], (0, 0, 0, 0)), textures: Vec::new() }, step));
        }
        Some(FrameStep::Cpu(step))
    }

    fn get_config(&self) -> Value {
        json!({
            "on": self.on,
            "corners": self.corners.map(|c| c.to_vec()),
            "cleanup": self.cleanup, "shape": self.shape, "stretch": self.stretch,
        })
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let corners = corners_of(cfg.get("corners")).filter(usable_corners);
        // Looked up in a dict, so a list or a dict there can't even be compared
        let pick = |key: &str, default: &str, list: &[(&str, &str)]| -> Result<String, ConfigRejected> {
            match cfg.get(key) {
                Some(Value::Array(_) | Value::Object(_)) => Err(ConfigRejected(key.into())),
                Some(Value::String(s)) if list.iter().any(|(k, _)| k == s) => Ok(s.clone()),
                _ => Ok(default.into()),
            }
        };
        let cleanup = pick("cleanup", "off", &CLEANUPS)?;
        // keep_shape: early builds
        let default_shape = if cfg.get("keep_shape") == Some(&Value::Bool(false)) { "fill" } else { "auto" };
        let shape = match cfg.get("shape") {
            None => default_shape.to_string(),
            Some(_) => pick("shape", "auto", &SHAPES)?,
        };
        let stretch = pair_of(cfg.get("stretch")).map_or([100, 100], |s| s.map(|v| (round_half_even(v) as i32).clamp(STRETCH_MIN, STRETCH_MAX)));
        self.on = cfg.get("on") == Some(&Value::Bool(true)) && corners.is_some();
        self.corners = corners;
        self.cleanup = cleanup;
        self.shape = shape;
        self.stretch = stretch;
        self.pending = true;
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let state = if self.on { "on" } else { "off" };
        let marked = if self.corners.is_some() { "marked" } else { "not marked" };
        vec![("Desk mode".into(), format!("{state}, corners {marked}, writing {}, shape {}, stretch {}% x {}%", self.cleanup, self.shape, self.stretch[0], self.stretch[1]))]
    }
}

#[cfg(test)]
#[path = "desk_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "desk_card_tests.rs"]
mod card_tests;
