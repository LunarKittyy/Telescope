//! The Background plugin (plugins/background.py): blurs what's behind you or swaps it for a picture. It's a CPU step on purpose: the person mask only reaches a frame step through FrameContext, and a shader step has no way to ask for it.

use crate::compositor::cover;
use crate::cpu::Rgb;
use crate::shared::mask_shared;
use crate::imgops::{blur_quick, compose, Gray};
use crate::peek;
use crate::shared::{no_pictures, thumb_of, Paired, PictureReader};
use crate::vision_row;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use telescope_core::actions::{self, Choice, SliderSpec};
use telescope_core::config::clamped_int;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat};
use telescope_core::host::{FileDialog, FileDialogKind, Host, RequestId};
use telescope_core::i18n::{n_, tr};
use telescope_core::image::{over_rows, planes_up_to_420, resize_linear_fixed, rgb_planes, rgb_to_420, small_rgb, spare_bytes, with_spares};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "background";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/vision_status.slint"), "\n", include_str!("../ui/background.slint"));
pub const MODES: [(&str, &str); 3] = [("off", n_("Off")), ("blur", n_("Blur")), ("picture", n_("Picture"))];
/// The picture control shows its picture this many columns wide, twice what it takes on a sharp screen.
const THUMB_COLUMNS: usize = 96;
const PICTURES: &str = n_("Pictures (*.png *.jpg *.jpeg *.webp *.bmp)");
/// Edges are worked out at a quarter of the size, then scaled back up.
const SMALL: f64 = 4.0;
/// And the blurred background at an eighth, since it's that blurry anyway.
const BLUR_SMALL: usize = 8;
const DEFAULT_BLUR: i64 = 6;
const DEFAULT_EDGES: i64 = 3;

/// The frame blurred (strength 1..10), at an eighth of the size and scaled back up. With the person mask, the person is left out of the blur, so they don't smear into a dark halo around themselves.
pub fn blurred(frame: &Rgb, strength: i64, mask: Option<&Gray>) -> Rgb {
    blurred_to(frame, strength, mask, frame.w, frame.h)
}

/// `blurred`, scaled up to w x h instead of the frame's own size: the same blur, so a small copy needs no full-size picture made and shrunk.
fn blurred_to(frame: &Rgb, strength: i64, mask: Option<&Gray>, w: usize, h: usize) -> Rgb {
    let (sw, sh) = ((frame.w / BLUR_SMALL).max(1), (frame.h / BLUR_SMALL).max(1));
    blurred_by(frame, strength, mask.map(|m| m.shrink(sw, sh)).as_ref(), w, h)
}

/// `blurred_to` with the mask already shrunk to the blur's working size (an eighth of the frame).
fn blurred_by(frame: &Rgb, strength: i64, rest: Option<&Gray>, w: usize, h: usize) -> Rgb {
    let (sw, sh) = ((frame.w / BLUR_SMALL).max(1), (frame.h / BLUR_SMALL).max(1));
    blurred_small(&frame.shrink(sw, sh), strength, rest).linear_exact(w, h)
}

/// The blur at its working size: `small` (an eighth of the frame) blurred, with the people in `rest` (the mask shrunk to the same size) left out.
fn blurred_small(small: &Rgb, strength: i64, rest: Option<&Gray>) -> Rgb {
    let weights = rest.and_then(|r| Weights::of(r, strength));
    blurred_with(small, strength, weights.as_ref())
}

/// What the person-excluding blur takes from the mask alone: the background share of each pixel and that share blurred. Made once for a mask and a strength.
struct Weights {
    rest: Vec<f32>,
    spread: Vec<f32>,
}

impl Weights {
    /// None where nobody is in the mask, so the blur is the plain one.
    fn of(rest: &Gray, strength: i64) -> Option<Weights> {
        if rest.data.iter().all(|m| *m == 0) {
            return None;
        }
        let share: Vec<f32> = rest.data.iter().map(|m| 1.0 - *m as f32 / 255.0).collect();
        let spread = blur_quick(&share, rest.w, rest.h, 1, blur_sigma(strength));
        Some(Weights { rest: share, spread })
    }
}

fn blur_sigma(strength: i64) -> f64 {
    (1.0 + strength as f64 * 1.2) * SMALL / BLUR_SMALL as f64
}

/// `blurred_small` with the mask's weights already made (None for the plain blur).
fn blurred_with(small: &Rgb, strength: i64, weights: Option<&Weights>) -> Rgb {
    let (sw, sh) = (small.w, small.h);
    let sigma = blur_sigma(strength);
    let plain = small.blur(sigma);
    let Some(Weights { rest, spread }) = weights else { return plain };
    let kept: Vec<f32> = small.data.as_chunks::<3>().0.iter().zip(rest).flat_map(|(p, r)| p.map(|v| v as f32 * r)).collect();
    let total = blur_quick(&kept, sw, sh, 3, sigma);
    let mut out = Rgb::scratch(sw, sh);
    let colours = out.data.as_chunks_mut::<3>().0.iter_mut().zip(total.as_chunks::<3>().0).zip(plain.data.as_chunks::<3>().0);
    for ((o, (v, p)), s) in colours.map(|((o, v), p)| (o, (v, p))).zip(spread) {
        // where there's next to no background, the plain blur
        let weight = (s * 4.0).clamp(0.0, 1.0);
        for c in 0..3 {
            o[c] = (v[c] / s.max(1e-3) * weight + p[c] as f32 * (1.0 - weight)).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// `out` (the background) with `src` over it where `mask` says (0..255 a pixel): one plane, in place. Rows with no mask are left as the background and rows all mask are the source.
fn lay_over(src: &[u8], mask: &[u8], out: &mut [u8], w: usize) {
    over_rows(out, w, |y, line| {
        let (top, m) = (&src[y * w..][..w], &mask[y * w..][..w]);
        if m.iter().all(|&v| v == 0) {
            return;
        }
        if m.iter().all(|&v| v == 255) {
            line.copy_from_slice(top);
            return;
        }
        for ((o, &t), &m) in line.iter_mut().zip(top).zip(m) {
            let x = t as u32 * m as u32 + *o as u32 * (255 - m as u32) + 128;
            *o = ((x + (x >> 8)) >> 8) as u8;
        }
    });
}

/// The sample's background blurred at strength 1..10 at the bubble's size, made the first time and kept.
fn blurred_peek(strength: i64) -> &'static Rgb {
    static LEVELS: [OnceLock<Rgb>; 10] = [const { OnceLock::new() }; 10];
    let i = (strength.clamp(1, 10) - 1) as usize;
    LEVELS[i].get_or_init(|| {
        let (scene, mask) = peek::blur_scene();
        blurred_to(scene, i as i64 + 1, Some(mask), peek::WIDTH, peek::HEIGHT)
    })
}

/// The sample's person mask feathered by edges 0..10 at the bubble's size (`Gray::soften` with its work done at this size), made the first time and kept.
fn softened_peek(edges: i64) -> &'static Gray {
    static MASKS: [OnceLock<Gray>; 11] = [const { OnceLock::new() }; 11];
    let i = edges.clamp(0, 10) as usize;
    MASKS[i].get_or_init(|| {
        let (_, mask) = peek::blur_scene();
        if i == 0 {
            return mask.shrink(peek::WIDTH, peek::HEIGHT);
        }
        let small = mask.area((mask.w / 4).max(1), (mask.h / 4).max(1));
        small.blur(i as f64 * 0.6).linear_exact(peek::WIDTH, peek::HEIGHT)
    })
}

/// What a frame step needs, frozen: a stream behind the panels keeps the ones it was left with.
#[derive(Clone)]
struct Settings {
    mode: usize,
    blur: i64,
    edges: i64,
    picture: Option<Arc<Rgb>>,
}

/// What replaces the room.
enum Backdrop {
    Blur,
    Picture(Arc<Rgb>),
}

struct Step {
    settings: Settings,
    /// The picture scaled to cover this frame's size
    covered: Option<Rgb>,
    spares: Vec<Vec<u8>>,
    /// The picture as I420 at this frame's size, for frames in YUV
    covered_yuv: Option<((usize, usize), Vec<u8>)>,
    /// What the last mask led to, so an unchanged mask isn't feathered and shrunk again
    derived: Option<Derived>,
    /// What the last quarter-size mask led to, for frames in YUV
    quarter: Option<Quarter>,
}

/// A quarter-size mask kept (so an unchanged one is known by its address) with its feathered copy for one edge setting and its eighth-size copy.
struct Quarter {
    src: Arc<Vec<u8>>,
    size: (usize, usize),
    /// The frame's size, which everything kept here is made for (two sizes can share a quarter-size mask)
    full: (usize, usize),
    soft: Option<(i64, Gray)>,
    rest: Option<Gray>,
    /// The feathered mask scaled up to the luma and chroma planes' sizes, for the edge setting in `soft`
    wide: Option<Wide>,
    /// The blur's weights for one strength
    weights: Option<(i64, Option<Weights>)>,
}

/// A feathered mask at the size of the luma plane and of a chroma plane.
struct Wide {
    luma: Vec<u8>,
    chroma: Vec<u8>,
}

/// The mask kept (so an unchanged one is known by its address) with its feathered copy for one edge setting and its eighth-size copy.
struct Derived {
    src: Arc<Vec<u8>>,
    size: (usize, usize),
    soft: Option<(i64, Gray)>,
    rest: Option<Gray>,
}

impl CpuStep for Step {
    fn hides(&self) -> bool {
        true
    }

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
        if self.run_planes(frame, ctx) {
            return;
        }
        let mask = mask_shared(ctx, frame);
        let Some(rgb) = Rgb::from_frame(frame) else { return };
        // No mask yet (vision still loading, or it can't segment) is the same as having lost track: the room stays hidden
        let Some(mask) = mask else { return self.hide_all(frame, &rgb) };
        let size = (rgb.w, rgb.h);
        let mut derived = match self.derived.take() {
            Some(d) if Arc::ptr_eq(&d.src, &mask) && d.size == size => d,
            _ => Derived { src: mask, size, soft: None, rest: None },
        };
        let src = derived.src.clone();
        let whole = || Gray { w: size.0, h: size.1, data: src.to_vec() };
        if derived.soft.as_ref().is_none_or(|(e, _)| *e != self.settings.edges) {
            derived.soft = Some((self.settings.edges, whole().soften(self.settings.edges)));
        }
        match self.backdrop() {
            Backdrop::Blur => {
                if derived.rest.is_none() {
                    derived.rest = Some(whole().shrink((rgb.w / BLUR_SMALL).max(1), (rgb.h / BLUR_SMALL).max(1)));
                }
                let edges = &derived.soft.as_ref().unwrap().1;
                compose(&rgb, edges, &blurred_by(&rgb, self.settings.blur, derived.rest.as_ref(), rgb.w, rgb.h)).write_to(frame);
            }
            Backdrop::Picture(picture) => {
                let edges = &derived.soft.as_ref().unwrap().1;
                compose(&rgb, edges, self.covering(&picture, &rgb)).write_to(frame);
            }
        }
        self.derived = Some(derived);
    }

    /// What goes behind the person: the blur, or the picture. Picture mode with no picture to show (none chosen, or the file can't be read) blurs instead, so the room never shows.
    fn backdrop(&self) -> Backdrop {
        match (MODES[self.settings.mode].0, &self.settings.picture) {
            ("picture", Some(picture)) => Backdrop::Picture(picture.clone()),
            _ => Backdrop::Blur,
        }
    }

    fn covering(&mut self, picture: &Rgb, rgb: &Rgb) -> &Rgb {
        if self.covered.as_ref().is_some_and(|c| (c.w, c.h) != (rgb.w, rgb.h)) {
            self.covered = None;
        }
        self.covered.get_or_insert_with(|| cover(picture, rgb.w, rgb.h))
    }

    /// A frame in YUV done on its planes, with no RGB picture of it made at full size: the mask comes at a quarter of the size and is feathered there, the blur is made at an eighth, and both are scaled up as they go over the frame's own planes. Where there's no one in the mask the frame is the background alone. False when this isn't a frame it does that way (the caller does it in RGB).
    fn run_planes(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) -> bool {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let (blur, picture) = match self.backdrop() {
            Backdrop::Blur => (true, None),
            Backdrop::Picture(picture) => (false, Some(picture)),
        };
        if frame.format != PixelFormat::I420 || w < 2 || h < 2 || frame.i420_planes().is_none() {
            return false;
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (qw, qh) = ((w / 4).max(1), (h / 4).max(1));
        let mask = ctx.person_mask_at(frame, qw, qh).filter(|m| m.len() == qw * qh);
        let edges = self.settings.edges;
        let mut quarter = None;
        if let Some(src) = mask {
            let mut q = match self.quarter.take() {
                Some(q) if Arc::ptr_eq(&q.src, &src) && q.size == (qw, qh) && q.full == (w, h) => q,
                _ => Quarter { src, size: (qw, qh), full: (w, h), soft: None, rest: None, wide: None, weights: None },
            };
            if q.soft.as_ref().is_none_or(|(e, _)| *e != edges) {
                let whole = Gray { w: qw, h: qh, data: q.src.to_vec() };
                q.soft = Some((edges, if edges <= 0 { whole } else { whole.blur(edges as f64 * 0.6) }));
                q.wide = None;
            }
            if blur && q.rest.is_none() {
                let whole = Gray { w: qw, h: qh, data: q.src.to_vec() };
                q.rest = Some(whole.shrink((w / BLUR_SMALL).max(1), (h / BLUR_SMALL).max(1)));
            }
            quarter = Some(q);
        }
        let mut out = spare_bytes(frame.data.len());
        // the background goes down first, and the frame goes over it where the mask is
        if blur {
            let (sw, sh) = ((w / BLUR_SMALL).max(1), (h / BLUR_SMALL).max(1));
            let Some(small) = small_rgb(frame, sw, sh, false) else { return false };
            let strength = self.settings.blur;
            if let Some(q) = quarter.as_mut() {
                if q.weights.as_ref().is_none_or(|(s, _)| *s != strength) {
                    q.weights = Some((strength, q.rest.as_ref().and_then(|r| Weights::of(r, strength))));
                }
            }
            let weights = quarter.as_ref().and_then(|q| q.weights.as_ref()).and_then(|(_, w)| w.as_ref());
            let blurred = blurred_with(&Rgb { w: sw, h: sh, data: small }, strength, weights);
            planes_up_to_420(&rgb_planes(&blurred.data), (sw, sh), &mut out, (w, h));
        } else if let Some(picture) = picture {
            if self.covered_yuv.as_ref().is_none_or(|(s, _)| *s != (w, h)) {
                let covered = cover(&picture, w, h);
                let mut yuv = vec![0u8; frame.data.len()];
                rgb_to_420(&covered.data, w, h, &mut yuv, false);
                self.covered_yuv = Some(((w, h), yuv));
            }
            out.copy_from_slice(&self.covered_yuv.as_ref().unwrap().1);
        }
        if let Some(q) = quarter.as_mut() {
            let soft = q.soft.as_ref().map(|(_, g)| g).filter(|g| g.data.iter().any(|&v| v != 0));
            if let (Some(soft), Some((sy, su, sv))) = (soft, frame.i420_planes()) {
                let wide = q.wide.get_or_insert_with(|| Wide { luma: resize_linear_fixed(&soft.data, qw, qh, 1, w, h), chroma: resize_linear_fixed(&soft.data, qw, qh, 1, cw, ch) });
                let (oy, chroma) = out.split_at_mut(w * h);
                let (ou, ov) = chroma.split_at_mut(cw * ch);
                lay_over(sy, &wide.luma, oy, w);
                lay_over(su, &wide.chroma, ou, cw);
                lay_over(sv, &wide.chroma, ov, cw);
            }
        }
        frame.replace_data(out);
        self.quarter = quarter;
        true
    }

    /// No telling the person from the room right now: the whole picture gets what the room would.
    fn hide_all(&mut self, frame: &mut Frame, rgb: &Rgb) {
        match self.backdrop() {
            Backdrop::Blur => blurred(rgb, self.settings.blur, None).write_to(frame),
            Backdrop::Picture(picture) => self.covering(&picture, rgb).write_to(frame),
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
    /// The picture's small copy for the Choose control, and whether the card has it yet.
    thumb: Option<Frame>,
    thumb_sent: bool,
    paired: Paired,
    reader: PictureReader,
    asked: Option<RequestId>,
    /// set_config has no host: the card hears the settings with the next hook that has one.
    pending: bool,
    /// The Blur bubble is up: the sample is blurred again whenever the strength moves.
    peeking: bool,
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
            thumb: None,
            thumb_sent: true,
            paired: Paired::default(),
            reader,
            asked: None,
            pending: true,
            peeking: false,
        }
    }

    fn mode_name(&self) -> &'static str {
        MODES[self.mode].0
    }

    fn fixed_props() -> Value {
        json!({
            "edges-tip": tr("How soft the edge around you is."),
            "picture-choose-tip": tr("Pick the picture that goes behind you."),
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
            "picture-text": file_name(&self.picture_path),
            "has-picture": self.picture.is_some(),
            "picture-problem": self.picture_problem,
            "picture-tip": self.picture_path,
        })
    }

    fn show(&mut self, host: &mut dyn Host) {
        let mut props = self.all_props();
        let row = vision_row::props(&*host, self.mode != 0, "");
        // The preview shows the blur only while it streams and the person can be told from the room; until then the bubble does
        let working = host.is_streaming() && row.get("vision-text").and_then(Value::as_str) == Some("");
        if let Value::Object(p) = &mut props {
            if let Value::Object(row) = row {
                p.extend(row);
            }
            p.insert("blur-peek".into(), json!(if self.mode_name() == "blur" && working { "" } else { "picture" }));
        }
        host.set_ui_props("card", &props);
        if !self.thumb_sent {
            self.thumb_sent = true;
            let empty = Frame::new(0, 0, telescope_core::frame::PixelFormat::Rgba, Vec::new());
            host.set_ui_image("card", "picture-thumb", self.thumb.as_ref().unwrap_or(&empty));
        }
    }

    /// The stand-in scene blurred at this strength with the effect's own blur and edge, behind the person. Each strength's blur and each edge's feathered
    /// mask is made once, at the bubble's size, the first time it is asked for, so a drag over the slider only lays one over the other.
    fn draw_blur(&self, host: &mut dyn Host) {
        let (scene, _) = peek::blur_scene_small();
        let out = compose(scene, softened_peek(self.edges), blurred_peek(self.blur));
        host.set_ui_image("card", "blur-picture", &peek::rgba(&out));
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
        self.thumb = self.picture.as_ref().map(|p| thumb_of(p, THUMB_COLUMNS));
        self.thumb_sent = false;
        self.picture_problem = if !path.is_empty() && self.picture.is_none() { tr("Can't open that picture") } else { String::new() };
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
        if vision_row::changes_it(event) || matches!(event, Event::StreamStarted { .. } | Event::StreamStopped) {
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
                if self.peeking {
                    self.draw_blur(host);
                }
            }
            "blur-peek-open" => {
                self.peeking = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                if self.peeking {
                    self.draw_blur(host);
                }
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
        Some(FrameStep::Cpu(Box::new(Step { settings, covered: None, spares: Vec::new(), covered_yuv: None, derived: None, quarter: None })))
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
