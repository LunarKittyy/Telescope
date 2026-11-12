//! The Filters plugin: the basic filters of a photo or video editor (denoise, dehaze, clarity, sharpen, skin smoothing, vignette, glow and
//! film grain), which look at the pixels around each one where Colour grading changes each pixel alone. One plugin puts steps in two places
//! of the picture's path (`staged_frame_steps`): the clean-up and the detail work in the effects stage, before the grade, and the finish
//! (glow, vignette, grain) in the colour stage after the grade's own step, so the grade doesn't change them. Every step is a shader with a
//! CPU twin (filters_cpu.rs) for when core::balance keeps the steps off the card, and a filter at its neutral value has no step at all.

use crate::cpu::Rgb;
use crate::filters_cpu as cpu;
use crate::imgops::Gray;
use crate::shared::{mask_of, Paired};
use serde_json::{json, Map, Value};
use telescope_core::actions;
use telescope_core::config::clamped_int;
use telescope_core::event::Event;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat, ShaderStep, Touch};
use telescope_core::host::Host;
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::settings::SettingSpec;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::vision::{self, status_text};

pub const NAME: &str = "filters";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/vision_status.slint"), "\n", include_str!("../ui/filters.slint"));

const SHARPEN: &str = include_str!("filter_sharpen.wgsl");
const CLARITY: &str = include_str!("filter_clarity.wgsl");
const DEHAZE: &str = include_str!("filter_dehaze.wgsl");
const DENOISE_SPACE: &str = include_str!("filter_denoise_space.wgsl");
const DENOISE_TIME: &str = include_str!("filter_denoise_time.wgsl");
const SKIN: &str = include_str!("filter_skin.wgsl");
const FINISH: &str = include_str!("filter_finish.wgsl");

/// A slider of the card: saved name, label, tooltip, lowest, highest, default, the heading above it (if it starts a section) and which
/// slider has to be above 0 for this one to show (a radius means nothing while the amount is 0).
pub struct SliderDef {
    pub key: &'static str,
    pub label: &'static str,
    pub tip: &'static str,
    /// What the setting is, for the tools that list it (not translated).
    pub about: &'static str,
    pub lo: i64,
    pub hi: i64,
    pub default: i64,
    pub heading: &'static str,
    pub after: Option<usize>,
}

pub const DENOISE: usize = 0;
pub const OVER_TIME: usize = 1;
pub const DEHAZE_AT: usize = 2;
pub const SKIN_AT: usize = 3;
pub const SHARPEN_AT: usize = 4;
pub const RADIUS: usize = 5;
pub const THRESHOLD: usize = 6;
pub const CLARITY_AT: usize = 7;
pub const VIGNETTE: usize = 8;
pub const VIGNETTE_SIZE: usize = 9;
pub const VIGNETTE_ROUND: usize = 10;
pub const GLOW: usize = 11;
pub const GLOW_SIZE: usize = 12;
pub const GRAIN: usize = 13;
pub const GRAIN_SIZE: usize = 14;

pub const SLIDERS: [SliderDef; 15] = [
    SliderDef { key: "denoise", label: n_("Denoise"), tip: n_("Smooth the noise in each frame while keeping the edges."), about: "Smooths the noise in each frame and keeps the edges", lo: 0, hi: 100, default: 0, heading: n_("Clean up"), after: None },
    SliderDef { key: "denoise_time", label: n_("Over time"), tip: n_("Average the parts that hold still with the frames before them to clean up a dim room, without ghosting on anything that moves."), about: "Averages the parts that hold still with the frames before them (cleans a dim room, moving parts are left out)", lo: 0, hi: 100, default: 0, heading: "", after: None },
    SliderDef { key: "dehaze", label: n_("Dehaze"), tip: n_("Lift the washed-out look of a dirty lens, a window behind you or a cheap camera."), about: "Takes the washed-out look off a dirty lens, a window behind the person or a cheap camera", lo: 0, hi: 100, default: 0, heading: "", after: None },
    SliderDef { key: "skin", label: n_("Skin smoothing"), tip: n_("Soften skin inside the person mask while eyes, brows, hair and edges stay sharp."), about: "Softens skin inside the person mask and keeps eyes, brows, hair and edges sharp (needs the vision pack)", lo: 0, hi: 100, default: 0, heading: "", after: None },
    SliderDef { key: "sharpen", label: n_("Sharpen"), tip: n_("Make edges crisper without a white outline."), about: "Makes edges crisper with no white outline", lo: 0, hi: 100, default: 0, heading: n_("Detail"), after: None },
    SliderDef { key: "sharpen_radius", label: n_("Radius"), tip: n_("Set how wide the detail Sharpen picks up is."), about: "How wide the detail Sharpen picks up is, in tenths of a pixel (only has an effect while sharpen is above 0)", lo: 5, hi: 30, default: 10, heading: "", after: Some(SHARPEN_AT) },
    SliderDef { key: "sharpen_threshold", label: n_("Threshold"), tip: n_("Leave detail smaller than this alone, which keeps skin and walls smooth."), about: "Detail smaller than this is left alone by Sharpen (only has an effect while sharpen is above 0)", lo: 0, hi: 100, default: 20, heading: "", after: Some(SHARPEN_AT) },
    SliderDef { key: "clarity", label: n_("Clarity"), tip: n_("Add contrast to the mid-sized detail so a flat picture looks less flat, or go below 0 to soften it."), about: "Contrast in the mid-sized detail; below 0 softens instead", lo: -100, hi: 100, default: 0, heading: "", after: None },
    SliderDef { key: "vignette", label: n_("Vignette"), tip: n_("Darken the corners, or lighten them below 0."), about: "Darkens the corners, or lightens them below 0", lo: -100, hi: 100, default: 0, heading: n_("Finish"), after: None },
    SliderDef { key: "vignette_size", label: n_("Size"), tip: n_("Set how much of the picture stays clear of the vignette."), about: "How much of the picture stays clear of the vignette (only has an effect while vignette is not 0)", lo: 0, hi: 100, default: 50, heading: "", after: Some(VIGNETTE) },
    SliderDef { key: "vignette_round", label: n_("Roundness"), tip: n_("Shape the vignette from the frame's own shape to a circle."), about: "Vignette shape, from the frame's own shape to a circle (only has an effect while vignette is not 0)", lo: 0, hi: 100, default: 50, heading: "", after: Some(VIGNETTE) },
    SliderDef { key: "glow", label: n_("Glow"), tip: n_("Add a soft bloom around the bright parts, like an old lens or a diffusion filter."), about: "A soft bloom around the bright parts", lo: 0, hi: 100, default: 0, heading: "", after: None },
    SliderDef { key: "glow_size", label: n_("Spread"), tip: n_("Set how far the glow reaches."), about: "How far the glow reaches (only has an effect while glow is above 0)", lo: 0, hi: 100, default: 50, heading: "", after: Some(GLOW) },
    SliderDef { key: "grain", label: n_("Film grain"), tip: n_("Add grain that moves every frame and is strongest in the mid-tones, like film."), about: "Grain that moves every frame, strongest in the mid-tones", lo: 0, hi: 100, default: 0, heading: "", after: None },
    SliderDef { key: "grain_size", label: n_("Grain size"), tip: n_("Set how coarse the film grain is."), about: "How coarse the film grain is (only has an effect while film grain is above 0)", lo: 0, hi: 100, default: 25, heading: "", after: Some(GRAIN) },
];

/// What the card says about the phone's own filters: sharpening twice looks worse than once.
const CAMERA_NOTE: &str = n_("Also on the phone");
const CAMERA_NOTE_DETAIL: &str = n_("The Camera card has the phone's own Sharpening and Noise reduction. Using both looks worse.");

/// What the sliders say, as the numbers the shaders and filters_cpu.rs take.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub values: [i64; 15],
}

impl Default for Settings {
    fn default() -> Self {
        Settings { values: std::array::from_fn(|i| SLIDERS[i].default) }
    }
}

impl Settings {
    fn v(&self, i: usize) -> f32 {
        self.values[i] as f32
    }

    pub fn denoise_space(&self) -> Option<cpu::DenoiseSpace> {
        (self.values[DENOISE] > 0).then(|| cpu::DenoiseSpace { range: 0.02 + 0.13 * self.v(DENOISE) / 100.0 })
    }

    pub fn denoise_time(&self) -> Option<cpu::DenoiseTime> {
        (self.values[OVER_TIME] > 0).then(|| cpu::DenoiseTime { tolerance: 0.02 + 0.06 * self.v(OVER_TIME) / 100.0, keep: 0.5 + 0.4 * self.v(OVER_TIME) / 100.0 })
    }

    pub fn dehaze(&self) -> Option<cpu::Dehaze> {
        (self.values[DEHAZE_AT] > 0).then(|| cpu::Dehaze { strength: self.v(DEHAZE_AT) / 100.0 })
    }

    pub fn skin(&self) -> Option<cpu::Skin> {
        (self.values[SKIN_AT] > 0).then(|| cpu::Skin { strength: self.v(SKIN_AT) / 100.0 })
    }

    pub fn sharpen(&self) -> Option<cpu::Sharpen> {
        (self.values[SHARPEN_AT] > 0).then(|| cpu::Sharpen { amount: 2.0 * self.v(SHARPEN_AT) / 100.0, sigma: self.v(RADIUS) / 10.0, gate: 0.1 * self.v(THRESHOLD) / 100.0 })
    }

    pub fn clarity(&self) -> Option<cpu::Clarity> {
        // Up to 1.5 more of the detail; below 0 only as far as the blur itself, so softening never turns the detail upside down
        (self.values[CLARITY_AT] != 0).then(|| cpu::Clarity { amount: if self.values[CLARITY_AT] < 0 { self.v(CLARITY_AT) / 100.0 } else { 1.5 * self.v(CLARITY_AT) / 100.0 } })
    }

    pub fn finish(&self) -> Option<cpu::Finish> {
        let on = self.values[VIGNETTE] != 0 || self.values[GLOW] > 0 || self.values[GRAIN] > 0;
        on.then(|| cpu::Finish {
            vignette: self.v(VIGNETTE) / 100.0,
            vignette_inner: 0.15 + 0.7 * self.v(VIGNETTE_SIZE) / 100.0,
            vignette_round: self.v(VIGNETTE_ROUND) / 100.0,
            glow: self.v(GLOW) / 100.0,
            glow_radius: 20.0 + 80.0 * self.v(GLOW_SIZE) / 100.0,
            grain: self.v(GRAIN) / 100.0,
            grain_size: 1.0 + 3.0 * self.v(GRAIN_SIZE) / 100.0,
        })
    }

    /// The steps with the stage each one belongs in. Clean-up and detail come first, in the effects stage; the finish is in the colour stage,
    /// where the grade's own step is already ahead of it. Nothing at a neutral value has a step.
    pub fn steps(&self) -> Vec<(FrameStage, FrameStep)> {
        self.steps_with(&Memories::default(), true)
    }

    /// The same, with the memories the CPU denoise over time keeps its last pictures in (the plugin's, so a new step for a changed setting
    /// goes on from them) and whether skin smoothing has any chance of a person mask (no vision pack means none, ever).
    pub fn steps_with(&self, memories: &Memories, skin_possible: bool) -> Vec<(FrameStage, FrameStep)> {
        let mut out = Vec::new();
        let mut early = |step: FrameStep| out.push((FrameStage::Effects, step));
        if let Some(p) = self.denoise_time() {
            let (run_memories, forget_memories, (tolerance, keep)) = (memories.clone(), memories.clone(), (p.tolerance, p.keep));
            let step = Pixels::new(move |rgb, meta, _| {
                // The registry is locked only to find the stream's own memory, so streams don't wait for each other's frames
                let memory = run_memories.lock().unwrap_or_else(|e| e.into_inner()).entry(meta.source.to_string()).or_default().clone();
                let mut memory = memory.lock().unwrap_or_else(|e| e.into_inner());
                cpu::denoise_time(rgb, &mut memory, &p, meta.source, meta.time_ns)
            }).forgetting(move |source| {
                let memory = forget_memories.lock().unwrap_or_else(|e| e.into_inner()).get(source).cloned();
                if let Some(memory) = memory {
                    memory.lock().unwrap_or_else(|e| e.into_inner()).reset();
                }
            });
            early(FrameStep::Either(shader(DENOISE_TIME, json!({"tolerance": tolerance, "keep": keep})), Box::new(step)));
        }
        if let Some(p) = self.denoise_space() {
            early(FrameStep::Either(shader(DENOISE_SPACE, json!({"range": p.range})), Box::new(Pixels::new(move |rgb, _, _| cpu::denoise_space(rgb, &p)))));
        }
        if let Some(p) = self.dehaze() {
            early(FrameStep::Either(shader(DEHAZE, json!({"strength": p.strength})), Box::new(Pixels::new(move |rgb, _, _| cpu::dehaze(rgb, &p)))));
        }
        if let Some(p) = self.clarity() {
            let (on_planes, p2) = (cpu::Clarity { amount: p.amount }, cpu::Clarity { amount: p.amount });
            early(FrameStep::Either(shader(CLARITY, json!({"amount": p.amount})), Box::new(Pixels::new(move |rgb, _, _| cpu::clarity(rgb, &p2)).on_luma(move |y, w, h| cpu::clarity_changes(y, w, h, &on_planes)))));
        }
        if let Some(p) = self.sharpen() {
            let (on_planes, p2) = (cpu::Sharpen { amount: p.amount, sigma: p.sigma, gate: p.gate }, cpu::Sharpen { amount: p.amount, sigma: p.sigma, gate: p.gate });
            early(FrameStep::Either(shader(SHARPEN, json!({"amount": p.amount, "sigma": p.sigma, "gate": p.gate})), Box::new(Pixels::new(move |rgb, _, _| cpu::sharpen(rgb, &p2)).on_luma(move |y, w, h| cpu::sharpen_changes(y, w, h, &on_planes)))));
        }
        if let Some(p) = self.skin().filter(|_| skin_possible) {
            early(FrameStep::Either(shader(SKIN, json!({"strength": p.strength})), Box::new(Pixels::masked(move |rgb, _, mask| {
                if let Some(mask) = mask {
                    cpu::skin(rgb, mask, &p)
                }
            }))));
        }
        if let Some(p) = self.finish() {
            let uniforms = json!({
                "vignette": p.vignette, "inner": p.vignette_inner, "round": p.vignette_round,
                "glow": p.glow, "glow_radius": p.glow_radius, "grain": p.grain, "grain_size": p.grain_size,
            });
            out.push((FrameStage::Colour, FrameStep::Either(shader(FINISH, uniforms), Box::new(Pixels::new(move |rgb, meta, _| cpu::finish(rgb, &p, meta.number))))));
        }
        out
    }
}

fn shader(wgsl: &str, uniforms: Value) -> ShaderStep {
    ShaderStep { wgsl: wgsl.into(), uniforms, textures: Vec::new() }
}

/// What the CPU denoise over time remembers, by the stream it saw, each in a lock of its own. The plugin keeps it, so moving a slider
/// (which makes every step new) doesn't make the average start again.
pub type Memories = Arc<Mutex<HashMap<String, Arc<Mutex<cpu::Memory>>>>>;

/// What a CPU filter is told about the frame it runs on.
pub struct Meta<'a> {
    pub number: u32,
    pub time_ns: u64,
    pub source: &'a str,
}

type Run = Box<dyn FnMut(&mut Rgb, &mut Meta, Option<&Gray>) + Send>;

/// What a CPU twin may take for one frame before it steps aside: a step that holds up the stream is worse than a picture without the filter.
pub const CPU_BUDGET_MS: u64 = 20;
/// How many frames it then lets pass untouched before it tries again, doubling each time the try is too slow again (up to CPU_REST_MOST).
pub const CPU_REST_FRAMES: u32 = 60;
pub const CPU_REST_MOST: u32 = 960;
/// How many frames a mask-needing step waits before it looks for a mask again once there was none.
const NO_MASK_REST: u32 = 30;

/// A filter's CPU twin as a frame step: the frame as plain RGB, the filter, and back into the frame's own format. It times itself, and a
/// frame that took longer than CPU_BUDGET_MS makes it skip the next CPU_REST_FRAMES (the picture comes out as the camera made it), and
/// twice as many each time it's still too slow when it tries again, so a heavy filter on a big picture settles into rare tries instead
/// of pulsing. That is what happens when a game keeps the card busy and the processor is the only place left.
struct Pixels {
    run: Run,
    wants_mask: bool,
    resting: AtomicU32,
    /// How many tries in a row were too slow.
    slow: u32,
    /// For a filter that only changes brightness: on a video frame it works on the Y plane as it is, which saves turning the whole picture
    /// into RGB and back (most of what a CPU step costs).
    on_luma: Option<LumaRun>,
    /// For a filter that keeps pictures of its own: told when the picture's geometry changed, to drop them.
    forget: Option<Box<dyn Fn(&str) + Send>>,
}

type LumaRun = Box<dyn Fn(&[f32], usize, usize) -> Vec<f32> + Send>;

impl Pixels {
    fn new(run: impl FnMut(&mut Rgb, &mut Meta, Option<&Gray>) + Send + 'static) -> Pixels {
        Pixels { run: Box::new(run), wants_mask: false, resting: AtomicU32::new(0), slow: 0, on_luma: None, forget: None }
    }

    /// One that needs the person mask and leaves the frame alone while there is none.
    fn masked(run: impl FnMut(&mut Rgb, &mut Meta, Option<&Gray>) + Send + 'static) -> Pixels {
        Pixels { run: Box::new(run), wants_mask: true, resting: AtomicU32::new(0), slow: 0, on_luma: None, forget: None }
    }

    fn forgetting(mut self, forget: impl Fn(&str) + Send + 'static) -> Pixels {
        self.forget = Some(Box::new(forget));
        self
    }

    fn on_luma(mut self, run: impl Fn(&[f32], usize, usize) -> Vec<f32> + Send + 'static) -> Pixels {
        self.on_luma = Some(Box::new(run));
        self
    }
}

impl CpuStep for Pixels {
    fn forget_history(&mut self, source: &str) {
        if let Some(forget) = &self.forget {
            forget(source);
        }
    }

    fn touch(&self, _ctx: &dyn FrameContext) -> Touch {
        if self.resting.load(Ordering::Relaxed) > 0 {
            self.resting.fetch_sub(1, Ordering::Relaxed);
            return Touch::Nothing;
        }
        Touch::Changes
    }

    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        let began = std::time::Instant::now();
        let mask = if self.wants_mask {
            match mask_of(ctx, frame) {
                Some(mask) => Some(mask),
                None => {
                    // nothing to do without one, and a step that says it changes the frame brings it down from the card to find out
                    self.resting.store(NO_MASK_REST, Ordering::Relaxed);
                    return;
                }
            }
        } else {
            None
        };
        if let (Some(run), PixelFormat::I420 | PixelFormat::Nv12) = (&self.on_luma, frame.format) {
            let (w, h) = (frame.width as usize, frame.height as usize);
            let changes = run(&cpu::plane_luma(&frame.data[..w * h], w, h), w, h);
            cpu::add_plane_luma(&mut frame.data_mut()[..w * h], w, &changes);
            self.timed(began, frame);
            return;
        }
        let Some(mut rgb) = Rgb::from_frame(frame) else { return };
        let source = ctx.source_id().to_string();
        let mut meta = Meta { number: frame.number as u32, time_ns: frame.time_ns, source: &source };
        (self.run)(&mut rgb, &mut meta, mask.as_ref());
        rgb.write_to(frame);
        self.timed(began, frame);
    }
}

impl Pixels {
    /// Notes how long a frame took, and steps aside for a while when it was too long.
    fn timed(&mut self, began: std::time::Instant, frame: &Frame) {
        if began.elapsed().as_millis() as u64 > CPU_BUDGET_MS {
            self.slow += 1;
            let rest = (CPU_REST_FRAMES << (self.slow - 1).min(8)).min(CPU_REST_MOST);
            if self.slow == 1 {
                log::info!("A Filters step took {} ms on the processor for a {}x{} frame; it rests for {rest} frames and tries again", began.elapsed().as_millis(), frame.width, frame.height);
            }
            self.resting.store(rest, Ordering::Relaxed);
        } else {
            self.slow = 0;
        }
    }
}

pub struct FiltersPlugin {
    on: bool,
    settings: Settings,
    paired: Paired,
    held: Option<bool>,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
    memories: Memories,
    /// There is, or could be, a vision pack to give skin smoothing its person mask.
    skin_possible: bool,
}

impl Default for FiltersPlugin {
    fn default() -> Self {
        FiltersPlugin::new()
    }
}

impl FiltersPlugin {
    pub fn new() -> FiltersPlugin {
        FiltersPlugin { on: true, settings: Settings::default(), paired: Paired::default(), held: None, pending: true, memories: Memories::default(), skin_possible: true }
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    fn fixed_props() -> Value {
        json!({
            "switch-name": tr("Filters"),
            "switch-help": tr("Turn every filter off to see the picture without them."),
            "camera-note": tr(CAMERA_NOTE),
            "camera-note-detail": tr(CAMERA_NOTE_DETAIL),
            "slider-label": SLIDERS.iter().map(|s| tr(s.label)).collect::<Vec<_>>(),
            "slider-setting": SLIDERS.iter().map(|s| format!("filters.{}", s.key)).collect::<Vec<_>>(),
            "slider-parent": SLIDERS.iter().map(|s| s.after.map_or(-1, |a| a as i64)).collect::<Vec<_>>(),
            "slider-mains": SLIDERS.iter().enumerate().filter(|(_, s)| s.after.is_none()).map(|(i, _)| i).collect::<Vec<_>>(),
            "slider-tip": SLIDERS.iter().map(|s| tr(s.tip)).collect::<Vec<_>>(),
            "slider-heading": SLIDERS.iter().map(|s| if s.heading.is_empty() { String::new() } else { tr(s.heading) }).collect::<Vec<_>>(),
            "slider-minimum": SLIDERS.iter().map(|s| s.lo).collect::<Vec<_>>(),
            "slider-maximum": SLIDERS.iter().map(|s| s.hi).collect::<Vec<_>>(),
            "slider-default": SLIDERS.iter().map(|s| s.default).collect::<Vec<_>>(),
            "skin-index": SKIN_AT,
        })
    }

    /// What a slider reads out.
    fn readout(i: usize, v: i64) -> String {
        match i {
            RADIUS => format!("{:.1} px", v as f64 / 10.0),
            VIGNETTE | CLARITY_AT if v > 0 => format!("+{v}"),
            _ => v.to_string(),
        }
    }

    fn all_props(&self, host: Option<&dyn Host>) -> Value {
        let values = &self.settings.values;
        let shown: Vec<bool> = SLIDERS.iter().map(|s| s.after.is_none_or(|a| values[a] != 0)).collect();
        let (mut text, mut download, mut usable) = (String::new(), false, true);
        if let Some(host) = host {
            let state = host.vision_state();
            let pack = host.vision_pack();
            usable = state != vision::MISSING && state != vision::FAILED && !pack.downloading;
            let wanted = values[SKIN_AT] > 0 || !usable;
            (text, download) = status_text(wanted, "", false, &state, &pack);
        }
        json!({
            "active": self.paired.on(),
            "on": self.on,
            "slider-value": values,
            "slider-text": values.iter().enumerate().map(|(i, v)| Self::readout(i, *v)).collect::<Vec<_>>(),
            "slider-shown": shown,
            "skin-available": usable,
            "vision-text": text,
            "vision-download": download,
        })
    }

    fn show(&mut self, host: &mut dyn Host) {
        let state = host.vision_state();
        let pack = host.vision_pack();
        // Without a pack (missing, failing or still on its way) skin smoothing has no mask to work with, so its step stays out until there is
        let possible = state != vision::MISSING && state != vision::FAILED && !pack.downloading;
        if possible != self.skin_possible {
            self.skin_possible = possible;
            if self.on && self.settings.values[SKIN_AT] > 0 {
                host.refresh_frame_step();
            }
        }
        let props = self.all_props(Some(&*host));
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

    fn set_on(&mut self, host: &mut dyn Host, on: bool) {
        self.on = on;
        self.changed(host);
    }

    fn set_value(&mut self, i: usize, v: f64) {
        let s = &SLIDERS[i];
        self.settings.values[i] = (v.round() as i64).clamp(s.lo, s.hi);
    }
}

impl Plugin for FiltersPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Filters").into(),
            about: n_("Sharpen, clarity, dehaze, denoise, skin smoothing, vignette, glow and film grain, like the filters of a photo editor.").into(),
            when_off: n_("Without it there's no Filters card and the picture isn't sharpened, cleaned up or finished.").into(),
            category: "look".into(),
            icon: "sliders".into(),
            optional: true,
            in_presets: true,
            // how noisy or soft a picture is depends on the camera
            device_local: true,
            // The clean-up goes before the grade; staged_frame_steps puts the finish after it
            frame_stage: FrameStage::Effects,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        host.set_ui_props("card", &FiltersPlugin::fixed_props());
        self.show(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        if crate::vision_row::changes_it(event) {
            self.show(host);
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        let mut props = FiltersPlugin::fixed_props();
        if let (Value::Object(p), Value::Object(all)) = (&mut props, self.all_props(None)) {
            p.extend(all);
        }
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "FiltersCard".into(), props }, region: "right".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let n = |i: usize| event.args.get(i).and_then(Value::as_f64).unwrap_or(0.0);
        match event.callback.as_str() {
            "switch-toggled-by-hand" => self.set_on(host, event.args.first().and_then(Value::as_bool).unwrap_or(false)),
            "slider-moved" => {
                let i = n(0) as usize;
                if i < SLIDERS.len() {
                    self.set_value(i, n(1));
                    self.changed(host);
                }
            }
            "download-clicked" => {
                host.download_vision();
                self.show(host);
            }
            _ => {}
        }
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        vec![actions::toggle_action("filters.on", &tr("Filters"), &tr("Filters"), "", &[])]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        // The card is dimmed until a phone is paired, and its shortcuts have nothing to do
        if !self.paired.on() || id != "filters.on" {
            return None;
        }
        let want = actions::toggle_wanted(params, self.on, &mut self.held);
        if want != self.on {
            self.set_on(host, want);
        }
        Some(actions::toggle_text(&tr("Filters"), want))
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        if id != "filters.on" {
            return;
        }
        if let Some(before) = actions::hold_released(params, &mut self.held) {
            if before != self.on && self.paired.on() {
                self.set_on(host, before);
            }
        }
    }

    fn on_stream_stop(&mut self, _host: &mut dyn Host) {
        // A stopped stream's pictures are 24 MiB each at 1080p; a running one starts its average again from its next frame
        self.memories.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    fn staged_frame_steps(&mut self) -> Vec<(FrameStage, FrameStep)> {
        if !self.on || self.settings.denoise_time().is_none() {
            // Nothing is averaging, so nothing needs the pictures it kept
            self.memories.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
        if self.on { self.settings.steps_with(&self.memories, self.skin_possible) } else { Vec::new() }
    }

    fn settings(&self) -> Vec<SettingSpec> {
        let mut out = vec![SettingSpec::switch("on", "Whether the filters are applied (off shows the picture without them)", true)];
        out.extend(SLIDERS.iter().map(|s| SettingSpec::number(s.key, s.about, s.lo as f64, s.hi as f64, 0, s.default as f64)));
        out
    }

    fn get_config(&self) -> Value {
        let mut cfg = Map::new();
        cfg.insert("on".into(), json!(self.on));
        for (i, s) in SLIDERS.iter().enumerate() {
            cfg.insert(s.key.into(), json!(self.settings.values[i]));
        }
        Value::Object(cfg)
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let empty = Value::Object(Map::new());
        let cfg = if cfg.is_object() { cfg } else { &empty };
        self.on = cfg.get("on") != Some(&Value::Bool(false));
        for (i, s) in SLIDERS.iter().enumerate() {
            self.settings.values[i] = clamped_int(cfg.get(s.key).unwrap_or(&Value::Null), s.default, s.lo, s.hi);
        }
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
        let used: Vec<&str> = SLIDERS.iter().enumerate()
            .filter(|(i, s)| self.settings.values[*i] != s.default && s.after.is_none())
            .map(|(_, s)| s.key)
            .collect();
        let state = if !self.on { "off".to_string() } else if used.is_empty() { "on, nothing set".to_string() } else { format!("on, {}", used.join(" ")) };
        vec![("Filters".into(), state)]
    }
}

#[cfg(test)]
#[path = "filters_tests.rs"]
mod tests;
