//! The Background plugin (plugins/background.py): blurs what's behind you or swaps it for a picture. It's a CPU step on purpose: the person mask only reaches a frame step through FrameContext, and a shader step has no way to ask for it.

use crate::compositor::cover;
use crate::cpu::Rgb;
use crate::imgops::{blur_quick, compose, Gray};
use crate::peek;
use crate::shared::{no_pictures, thumb_of, Paired, PictureReader, Wanted};
use crate::vision_row;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use telescope_core::actions::{self, Choice, SliderSpec};
use telescope_core::config::clamped_int;
use telescope_core::event::Event;
use telescope_core::frame::{Alpha, CpuStep, Frame, FrameContext, FrameStage, FrameStep, PixelFormat, ShaderStep};
use telescope_core::host::{FileDialog, FileDialogKind, Host, RequestId};
use telescope_core::i18n::{n_, tr};
use telescope_core::image::{over_rows, planes_up_to_420, resize_linear_fixed, rgb_planes, rgb_to_420, small_rgb, spare_bytes, with_spares};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::settings::{off_default, reset_config};
use telescope_core::shader::{BLUR_CUTOUT, BLUR_WEIGHTS, ROOM_PICTURE};
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
const DEFAULT_CUT_IN: i64 = 0;
/// What the High quality cutout costs next to the normal one, measured on a 1080p stream: a bit under half a core.
const QUALITY_TIP: &str = n_("Cut out your hair and edges more cleanly, for about 40% of a CPU core.");

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
fn softened_peek(edges: i64, cut: i64) -> &'static Gray {
    static MASKS: [OnceLock<Gray>; 121] = [const { OnceLock::new() }; 121];
    let (i, c) = (edges.clamp(0, 10), cut.clamp(0, 10));
    MASKS[(i * 11 + c) as usize].get_or_init(|| {
        let (_, mask) = peek::blur_scene();
        if i == 0 && c == 0 {
            return mask.shrink(peek::WIDTH, peek::HEIGHT);
        }
        let mut small = mask.area((mask.w / 4).max(1), (mask.h / 4).max(1)).cut_in(c);
        if i > 0 {
            small = small.blur(i as f64 * 0.6);
        }
        small.linear_exact(peek::WIDTH, peek::HEIGHT)
    })
}

/// What a frame step needs, frozen: a stream behind the panels keeps the ones it was left with.
#[derive(Clone)]
struct Settings {
    mode: usize,
    blur: i64,
    edges: i64,
    cut_in: i64,
    picture: Option<Arc<Rgb>>,
    /// The slower model's mask (the normal one while it isn't there), see FrameContext::person_mask_hq
    high_quality: bool,
}

/// What replaces the room.
enum Backdrop {
    Blur,
    Picture(Arc<Rgb>),
}

struct Step {
    settings: Settings,
    /// Whose cut-out goes onto the frame as its alpha plane
    wanted: Wanted,
    /// The picture scaled to cover this frame's size
    covered: Option<Rgb>,
    spares: Vec<Vec<u8>>,
    /// The picture as I420 at this frame's size, for frames in YUV
    covered_yuv: Option<((usize, usize), Vec<u8>)>,
    /// What the last mask led to, so an unchanged mask isn't feathered and shrunk again
    derived: Option<Derived>,
    /// What the last quarter-size mask led to, for frames in YUV
    quarter: Option<Quarter>,
    /// The pictures the card's shader was last given, for as long as the mask they come from is the same
    card: CardPictures,
    /// The single mask snapshot shared by the card's textures and cutout for a frame.
    card_mask: Option<CardMask>,
    /// The picture covering a frame of this size, as the card takes it (the same data for as long as nothing changes, so it goes up once).
    card_room: Option<((usize, usize), Frame)>,
}

struct CardMask {
    source: String,
    number: u64,
    size: (usize, usize),
    mask: Option<Arc<Vec<u8>>>,
}

/// What a mask turned into for the card (Rgba, red the value): the feathered cutout at a quarter size and the share left for the blur at an eighth, each with what it was made from. None where there's nothing to say.
#[derive(Default)]
struct CardPictures {
    soft: Option<(CardKey, Option<Frame>)>,
    weights: Option<(CardKey, Option<Frame>)>,
}

/// The mask (by address), the edge settings and the frame size a card picture was made for.
type CardKey = (Arc<Vec<u8>>, (i64, i64), (usize, usize));

/// A quarter-size mask kept (so an unchanged one is known by its address) with its feathered copy for one edge setting and its eighth-size copy.
struct Quarter {
    src: Arc<Vec<u8>>,
    size: (usize, usize),
    /// The frame's size, which everything kept here is made for (two sizes can share a quarter-size mask)
    full: (usize, usize),
    soft: Option<((i64, i64), Gray)>,
    rest: Option<Gray>,
    /// The feathered mask scaled up to the luma and chroma planes' sizes, for the edge setting in `soft`
    wide: Option<Wide>,
    /// The blur's weights for one strength
    weights: Option<(i64, Option<Weights>)>,
    /// The feathered mask as the frame's alpha plane, made the first time a stream wants it and kept with `wide`
    alpha: Option<Arc<Alpha>>,
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
    soft: Option<((i64, i64), Gray)>,
    rest: Option<Gray>,
    /// `soft` as the frame's alpha plane, made the first time a stream wants it
    alpha: Option<Arc<Alpha>>,
}

impl Derived {
    /// The feathered mask as the frame's alpha plane.
    fn alpha(&mut self) -> Arc<Alpha> {
        let (w, h) = self.size;
        let edges = &self.soft.as_ref().unwrap().1;
        self.alpha.get_or_insert_with(|| background_alpha(w, h, edges.data.clone())).clone()
    }
}

impl Quarter {
    /// The feathered mask scaled up to a frame of this size, None while nobody is in it.
    fn wide(&mut self, (w, h): (usize, usize)) -> Option<&Wide> {
        let (qw, qh) = self.size;
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let soft = self.soft.as_ref().map(|(_, g)| g).filter(|g| g.data.iter().any(|&v| v != 0))?;
        Some(self.wide.get_or_insert_with(|| Wide { luma: resize_linear_fixed(&soft.data, qw, qh, 1, w, h), chroma: resize_linear_fixed(&soft.data, qw, qh, 1, cw, ch) }))
    }

    /// The feathered mask as the frame's alpha plane: nobody in it is all zero.
    fn alpha(&mut self, (w, h): (usize, usize)) -> Arc<Alpha> {
        if let Some(alpha) = &self.alpha {
            return alpha.clone();
        }
        let luma = self.wide((w, h)).map_or_else(|| vec![0; w * h], |wide| wide.luma.clone());
        self.alpha.get_or_insert_with(|| background_alpha(w, h, luma)).clone()
    }
}

fn background_alpha(w: usize, h: usize, data: Vec<u8>) -> Arc<Alpha> {
    Arc::new(Alpha::new(w as u32, h as u32, data, false).keep_opaque_when_empty())
}

impl CpuStep for Step {
    fn process(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        let mut spares = std::mem::take(&mut self.spares);
        with_spares(&mut spares, || self.run(frame, ctx));
        self.spares = spares;
    }

    fn card_uniforms(&mut self, _width: u32, _height: u32) -> Option<Value> {
        Some(card_uniforms(self.settings.blur))
    }

    fn card_block_chroma(&self) -> bool {
        true
    }

    fn card_textures_offered(&self) -> Option<bool> {
        Some(true)
    }

    fn card_textures_from_mask(&self) -> bool {
        true
    }

    /// The cut-out for a stream that wants it, made on the CPU the way `run` makes it, since the card's blur never gives the frame one.
    fn card_cut_out(&mut self, frame: &Frame, ctx: &dyn FrameContext) -> Option<Arc<Alpha>> {
        let snapshot = self.card_mask.take();
        if self.settings.mode == 0 || !self.wants_cut_out(frame, ctx) {
            return None;
        }
        let (w, h) = (frame.width as usize, frame.height as usize);
        let by_planes = frame.format == PixelFormat::I420 && w >= 2 && h >= 2 && (frame.surface().is_some() || frame.i420_planes().is_some());
        let size = if by_planes { ((w / 4).max(1), (h / 4).max(1)) } else { (w, h) };
        let mask = match snapshot.filter(|s| s.source == ctx.source_id() && s.number == frame.number && s.size == size) {
            Some(snapshot) => snapshot.mask,
            None => self.mask_at(frame, ctx, size),
        };
        // No mask yet is nobody to cut out, the same as `run` gives
        let Some(mask) = mask else { return Some(background_alpha(w, h, vec![0; w * h])) };
        if by_planes {
            let mut q = self.derive(mask, (w, h), true);
            let alpha = q.alpha((w, h));
            self.quarter = Some(q);
            Some(alpha)
        } else {
            let mut derived = self.derived_for(mask, (w, h));
            let alpha = derived.alpha();
            self.derived = Some(derived);
            Some(alpha)
        }
    }

    /// The cutout and the share of the room the blur may take from, made from the mask the way the CPU way makes them for this frame's format (a quarter-size mask for planes, the full-size one otherwise); with no mask or nobody in it they're left out, which the shader reads as no cutout and a plain blur.
    fn card_textures(&mut self, frame: &Frame, ctx: &dyn FrameContext) -> Option<Vec<(String, Frame)>> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let by_planes = frame.format == PixelFormat::I420 && w >= 2 && h >= 2 && (frame.surface().is_some() || frame.i420_planes().is_some());
        let size = if by_planes { ((w / 4).max(1), (h / 4).max(1)) } else { (w, h) };
        let mask = self.mask_at(frame, ctx, size);
        self.card_mask = Some(CardMask { source: ctx.source_id().to_string(), number: frame.number, size, mask: mask.clone() });
        let quarter = by_planes.then(|| mask.clone()).flatten();
        let blur = matches!(self.backdrop(), Backdrop::Blur);
        let room = self.card_room_picture((w, h)).map(|f| (ROOM_PICTURE.to_string(), f));
        let Some(src) = mask else {
            self.quarter = None;
            self.card = CardPictures::default();
            return Some(room.into_iter().collect());
        };
        let key: CardKey = (src.clone(), (self.settings.edges, self.settings.cut_in), (w, h));
        if let Some(src) = quarter {
            let q = self.derive(src, (w, h), blur);
            if self.card.soft.as_ref().is_none_or(|(k, _)| !same_card_key(k, &key)) {
                let soft = q.soft.as_ref().map(|(_, g)| g).filter(|g| g.data.iter().any(|&v| v != 0));
                self.card.soft = Some((key.clone(), soft.map(|g| card_picture(g.w, g.h, g.data.iter().copied()))));
            }
            if self.card.weights.as_ref().is_none_or(|(k, _)| !same_card_key(k, &key)) {
                // 255 where the pixel is room, so what the shader weighs is the share Weights::of takes
                let weights = q.rest.as_ref().filter(|r| r.data.iter().any(|&m| m != 0));
                self.card.weights = Some((key.clone(), weights.map(|r| card_picture(r.w, r.h, r.data.iter().map(|m| 255 - m)))));
            }
            self.quarter = Some(q);
        } else {
            self.full_card_pictures(&src, key, (w, h));
        }
        let mut out = Vec::new();
        for (name, kept) in [(BLUR_CUTOUT, &self.card.soft), (BLUR_WEIGHTS, &self.card.weights)] {
            out.extend(kept.as_ref().and_then(|(_, f)| f.clone()).map(|f| (name.to_string(), f)));
        }
        out.extend(room);
        Some(out)
    }
}

impl Step {
    /// The picture of Picture mode scaled to cover a frame of this size, as the Rgba picture the card takes; None in blur mode.
    fn card_room_picture(&mut self, size: (usize, usize)) -> Option<Frame> {
        let Backdrop::Picture(picture) = self.backdrop() else { return None };
        if self.card_room.as_ref().is_none_or(|(s, _)| *s != size) {
            let covered = cover(&picture, size.0, size.1);
            let mut rgba = Frame::new(size.0 as u32, size.1 as u32, PixelFormat::Rgba, vec![255; size.0 * size.1 * 4]);
            for (o, p) in rgba.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(covered.data.as_chunks::<3>().0) {
                o[..3].copy_from_slice(p);
            }
            self.card_room = Some((size, rgba));
        }
        self.card_room.as_ref().map(|(_, f)| f.clone())
    }

    /// The pictures for a frame the CPU way does with its full-size mask: the cutout is the mask itself when there's no feathering or cutting in, else what `soften_cut` makes before it's scaled up (the card scales it as it samples), and the share is the mask shrunk to the blur's size.
    fn full_card_pictures(&mut self, src: &Arc<Vec<u8>>, key: CardKey, (w, h): (usize, usize)) {
        let (edges, cut) = (self.settings.edges, self.settings.cut_in);
        let whole = || Gray { w, h, data: src.to_vec() };
        if self.card.soft.as_ref().is_none_or(|(k, _)| !same_card_key(k, &key)) {
            let soft = if edges <= 0 && cut <= 0 {
                whole()
            } else {
                let small = whole().area((w / 4).max(1), (h / 4).max(1)).cut_in(cut);
                if edges > 0 { small.blur(edges as f64 * 0.6) } else { small }
            };
            self.card.soft = Some((key.clone(), Some(soft).filter(|g| g.data.iter().any(|&v| v != 0)).map(|g| card_picture(g.w, g.h, g.data.iter().copied()))));
        }
        if self.card.weights.as_ref().is_none_or(|(k, _)| !same_card_key(k, &key)) {
            let rest = whole().shrink((w / BLUR_SMALL).max(1), (h / BLUR_SMALL).max(1));
            self.card.weights = Some((key, Some(rest).filter(|r| r.data.iter().any(|&m| m != 0)).map(|r| card_picture(r.w, r.h, r.data.iter().map(|m| 255 - m)))));
        }
    }
}

fn same_card_key(a: &CardKey, b: &CardKey) -> bool {
    Arc::ptr_eq(&a.0, &b.0) && a.1 == b.1 && a.2 == b.2
}

/// One value a pixel as the Rgba picture the card takes, in the red channel.
fn card_picture(w: usize, h: usize, values: impl Iterator<Item = u8>) -> Frame {
    Frame::new(w as u32, h as u32, PixelFormat::Rgba, values.flat_map(|v| [v, 0, 0, 255]).collect())
}

/// The blur's settings for the card's shader, which core::shader names: the blur is worked at an eighth of the picture with the CPU way's sigma, and an I420 picture is shrunk and laid over by planes as the CPU does it.
fn card_uniforms(strength: i64) -> Value {
    json!({"blur_shrink": BLUR_SMALL, "blur_sigma": blur_sigma(strength), "blur_planes": 1})
}

/// The same picture on the card: the room blurred (leaving out whoever the mask has in it) with the frame over it where the cutout says.
const CARD_BLUR: &str = include_str!("background.wgsl");

fn card_blur(strength: i64) -> ShaderStep {
    let nothing = || Frame::new(1, 1, PixelFormat::Rgba, vec![0, 0, 0, 255]);
    ShaderStep { keeps_geometry: false, wgsl: CARD_BLUR.into(), uniforms: card_uniforms(strength), textures: vec![(BLUR_CUTOUT.into(), nothing()), (BLUR_WEIGHTS.into(), nothing())] }
}

/// Picture mode on the card: the frame over the picture where the cutout says, laid over by planes for an I420 frame as the blur is.
const CARD_PICTURE: &str = include_str!("background_picture.wgsl");

fn card_picture_step() -> ShaderStep {
    let nothing = || Frame::new(1, 1, PixelFormat::Rgba, vec![0, 0, 0, 255]);
    ShaderStep { keeps_geometry: false, wgsl: CARD_PICTURE.into(), uniforms: json!({"blur_planes": 1}), textures: vec![(BLUR_CUTOUT.into(), nothing()), (ROOM_PICTURE.into(), nothing())] }
}

impl Step {
    fn run(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) {
        if self.settings.mode == 0 {
            return;
        }
        if self.run_planes(frame, ctx) {
            return;
        }
        let mask = self.mask_at(frame, ctx, (frame.width as usize, frame.height as usize));
        let Some(rgb) = Rgb::from_frame(frame) else { return };
        let want = self.wants_cut_out(frame, ctx);
        // Before the first mask, apply the chosen backdrop to the whole frame.
        let Some(mask) = mask else {
            self.hide_all(frame, &rgb);
            // Nobody to cut out: an all-clear plane, the same as the planes path gives
            if want {
                frame.set_alpha(background_alpha(rgb.w, rgb.h, vec![0; rgb.w * rgb.h]));
            }
            return;
        };
        let size = (rgb.w, rgb.h);
        let mut derived = self.derived_for(mask, size);
        match self.backdrop() {
            Backdrop::Blur => {
                if derived.rest.is_none() {
                    derived.rest = Some(Gray { w: rgb.w, h: rgb.h, data: derived.src.to_vec() }.shrink((rgb.w / BLUR_SMALL).max(1), (rgb.h / BLUR_SMALL).max(1)));
                }
                let edges = &derived.soft.as_ref().unwrap().1;
                compose(&rgb, edges, &blurred_by(&rgb, self.settings.blur, derived.rest.as_ref(), rgb.w, rgb.h)).write_to(frame);
            }
            Backdrop::Picture(picture) => {
                let edges = &derived.soft.as_ref().unwrap().1;
                compose(&rgb, edges, self.covering(&picture, &rgb)).write_to(frame);
            }
        }
        if want {
            frame.set_alpha(derived.alpha());
        }
        self.derived = Some(derived);
    }

    /// What the full-size mask leads to for these settings, the last frame's where the mask is the same one.
    fn derived_for(&mut self, mask: Arc<Vec<u8>>, size: (usize, usize)) -> Derived {
        let mut derived = match self.derived.take() {
            Some(d) if Arc::ptr_eq(&d.src, &mask) && d.size == size => d,
            _ => Derived { src: mask, size, soft: None, rest: None, alpha: None },
        };
        let shaping = (self.settings.edges, self.settings.cut_in);
        if derived.soft.as_ref().is_none_or(|(e, _)| *e != shaping) {
            let whole = Gray { w: size.0, h: size.1, data: derived.src.to_vec() };
            derived.soft = Some((shaping, whole.soften_cut(shaping.0, shaping.1)));
            derived.alpha = None;
        }
        derived
    }

    /// Whether this frame's stream asked for its cut-out and no step before this one gave it one.
    fn wants_cut_out(&self, frame: &Frame, ctx: &dyn FrameContext) -> bool {
        frame.alpha().is_none() && self.wanted.has(ctx.source_id())
    }

    /// What goes behind the person: the blur, or the picture; no readable picture falls back to blur.
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

    /// The last good mask at the requested size, mapped onto this frame while a fresh mask is computed.
    fn mask_at(&self, frame: &Frame, ctx: &dyn FrameContext, (qw, qh): (usize, usize)) -> Option<Arc<Vec<u8>>> {
        ctx.background_mask_at(frame, qw, qh, self.settings.high_quality).filter(|m| m.len() == qw * qh)
    }

    /// What the quarter-size mask leads to for these settings: the cutout feathered, and for the blur the mask at an eighth. What was kept from the last frame is used where the mask is the same one.
    fn derive(&mut self, src: Arc<Vec<u8>>, (w, h): (usize, usize), blur: bool) -> Quarter {
        let (qw, qh) = ((w / 4).max(1), (h / 4).max(1));
        let edges = (self.settings.edges, self.settings.cut_in);
        let mut q = match self.quarter.take() {
            Some(q) if Arc::ptr_eq(&q.src, &src) && q.size == (qw, qh) && q.full == (w, h) => q,
            _ => Quarter { src, size: (qw, qh), full: (w, h), soft: None, rest: None, wide: None, weights: None, alpha: None },
        };
        if q.soft.as_ref().is_none_or(|(e, _)| *e != edges) {
            let whole = Gray { w: qw, h: qh, data: q.src.to_vec() };
            let cut = whole.cut_in(edges.1);
            q.soft = Some((edges, if edges.0 <= 0 { cut } else { cut.blur(edges.0 as f64 * 0.6) }));
            q.wide = None;
            q.alpha = None;
        }
        if blur && q.rest.is_none() {
            let whole = Gray { w: qw, h: qh, data: q.src.to_vec() };
            q.rest = Some(whole.shrink((w / BLUR_SMALL).max(1), (h / BLUR_SMALL).max(1)));
        }
        q
    }

    /// A frame in YUV done on its planes, with no RGB picture of it made at full size: the mask comes at a quarter of the size and is feathered there, the blur is made at an eighth, and both are scaled up as they go over the frame's own planes. Where there's no one in the mask the frame is the background alone. False when this isn't a frame it does that way (the caller does it in RGB).
    fn run_planes(&mut self, frame: &mut Frame, ctx: &dyn FrameContext) -> bool {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let want = self.wants_cut_out(frame, ctx);
        let (blur, picture) = match self.backdrop() {
            Backdrop::Blur => (true, None),
            Backdrop::Picture(picture) => (false, Some(picture)),
        };
        if frame.format != PixelFormat::I420 || w < 2 || h < 2 || frame.i420_planes().is_none() {
            return false;
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (qw, qh) = ((w / 4).max(1), (h / 4).max(1));
        // The high quality mask is the normal one while that model isn't downloaded, loaded or in time, so this never has less
        let mask = self.mask_at(frame, ctx, (qw, qh));
        let mut quarter = mask.map(|src| self.derive(src, (w, h), blur));
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
            if let (Some(wide), Some((sy, su, sv))) = (q.wide((w, h)), frame.i420_planes()) {
                let (oy, chroma) = out.split_at_mut(w * h);
                let (ou, ov) = chroma.split_at_mut(cw * ch);
                lay_over(sy, &wide.luma, oy, w);
                lay_over(su, &wide.chroma, ou, cw);
                lay_over(sv, &wide.chroma, ov, cw);
            }
        }
        frame.replace_data(out);
        // The alpha is the mask the frame was laid over the background with: nobody in it, or no mask yet, is all zero
        if want {
            let alpha = match quarter.as_mut() {
                Some(q) => q.alpha((w, h)),
                None => background_alpha(w, h, vec![0; w * h]),
            };
            frame.set_alpha(alpha);
        }
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
    cut_in: i64,
    high_quality: bool,
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
    /// What the host was last told with set_hiding: None until it has been.
    told_hiding: Option<bool>,
    /// A new camera's settings or the shown camera changed: report its capture status again.
    retell_hiding: bool,
    /// The streams whose cut-out is wanted (Event::CutOutWantedBy), which the steps read
    wanted: Wanted,
    /// Blur was switched on by a stream wanting its cut-out while the mode was Off, not picked on the card: it goes back to Off with the last stream's, and is saved as Off meanwhile.
    auto_on: bool,
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
            cut_in: DEFAULT_CUT_IN,
            high_quality: false,
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
            told_hiding: None,
            retell_hiding: false,
            wanted: Wanted::default(),
            auto_on: false,
        }
    }

    fn mode_name(&self) -> &'static str {
        MODES[self.mode].0
    }

    fn fixed_props() -> Value {
        json!({
            "edges-tip": tr("How soft the edge around you is."),
            "cut-in-tip": tr("Pull the cutout's border in towards you."),
            "picture-choose-tip": tr("Pick the picture that goes behind you."),
            "quality-tip": tr(QUALITY_TIP),
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
            "cut-in": self.cut_in,
            "cut-in-text": self.cut_in.to_string(),
            "picture-text": file_name(&self.picture_path),
            "has-picture": self.picture.is_some(),
            "picture-problem": self.picture_problem,
            "high-quality": self.high_quality,
            "picture-tip": self.picture_path,
            "reset-shown": off_default(&self.get_config(), &BackgroundPlugin::new().get_config(), &[]),
        })
    }

    /// Background is a comfort effect and does not block unattended phone photos.
    fn report_hiding(&mut self, host: &mut dyn Host) {
        let hiding = false;
        if self.retell_hiding || self.told_hiding != Some(hiding) {
            self.retell_hiding = false;
            self.told_hiding = Some(hiding);
            host.set_hiding(NAME, None, hiding);
        }
    }

    fn show(&mut self, host: &mut dyn Host) {
        self.report_hiding(host);
        let mut props = self.all_props();
        let row = vision_row::props(&*host, self.mode != 0, "");
        // The preview shows the blur only while it streams and the person can be told from the room; until then the bubble does
        let working = host.is_streaming() && row.get("vision-text").and_then(Value::as_str) == Some("");
        if let Value::Object(p) = &mut props {
            if let Value::Object(row) = row {
                p.extend(row);
            }
            if let Value::Object(note) = vision_row::cutout_props(&*host, self.mode != 0 && self.high_quality) {
                p.extend(note);
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
        let out = compose(scene, softened_peek(self.edges, self.cut_in), blurred_peek(self.blur));
        host.set_ui_image("card", "blur-picture", &peek::rgba(&out));
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            self.show(host);
            // New settings (a preset, another camera's) may say Off while the stream still wants its cut-out
            self.settle_auto(host);
        }
    }

    /// Everything the settings decide has to be shown, kept and handed to the stream again.
    fn changed(&mut self, host: &mut dyn Host) {
        self.show(host);
        host.schedule_save();
        host.refresh_frame_step();
    }

    /// A stream's picture is asked to carry its cut-out, or no longer.
    fn cut_out_wanted(&mut self, host: &mut dyn Host, owner: &str, source_id: &str, on: bool) {
        if self.wanted.set_by(owner, source_id, on).0 {
            host.refresh_frame_step();
        }
        self.settle_auto(host);
    }

    /// The shown camera with Background off gets Blur while its stream wants a cut-out and Chroma key isn't the one making it, and goes back to Off after.
    fn settle_auto(&mut self, host: &mut dyn Host) {
        // The mode here is the shown camera's, so only the shown stream counts
        let wanted = host.focused_source_id().map_or_else(|| self.wanted.any(), |shown| self.wanted.has(&shown));
        let keyed = host.shared("chroma_key.keying").and_then(|v| v.as_bool()).unwrap_or(false);
        let need = wanted && !keyed;
        let before = self.mode;
        if need && self.mode == 0 {
            self.mode = 1;
            self.auto_on = true;
        } else if !need && self.auto_on {
            self.mode = 0;
            self.auto_on = false;
        }
        if self.mode != before {
            self.show(host);
            host.refresh_frame_step();
        }
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
            about: n_("Blur the background behind you or swap it for a picture.").into(),
            when_off: n_("Without the Background plugin there's no Background card and the background is shown the way the camera sees it.").into(),
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

    fn shutdown(&mut self, host: &mut dyn Host) {
        if self.told_hiding.take() == Some(true) {
            host.set_hiding(NAME, None, false);
        }
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        // The panels moved to another camera: the host holds this plugin's report per camera, so it hears again
        if matches!(event, Event::SourceShown { .. }) {
            self.retell_hiding = true;
        }
        self.catch_up(host);
        if self.retell_hiding {
            self.report_hiding(host);
        }
        match event {
            Event::CutOutWanted { source_id, on } => self.cut_out_wanted(host, "", source_id, *on),
            Event::CutOutWantedBy { owner, source_id, on } => self.cut_out_wanted(host, owner, source_id, *on),
            Event::SharedChanged { key } if key == "chroma_key.keying" => self.settle_auto(host),
            Event::SourceShown { .. } => self.settle_auto(host),
            _ => {}
        }
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        if vision_row::changes_it(event) || vision_row::changes_cutout(event) || matches!(event, Event::StreamStarted { .. } | Event::StreamStopped) {
            self.show(host);
        }
        if let Event::RequestDone { request, ok, data, .. } = event {
            if self.asked == Some(*request) {
                self.asked = None;
                if let (true, Some(path)) = (*ok, data.get("path").and_then(Value::as_str)) {
                    self.load_picture(path);
                    if self.picture_problem.is_empty() {
                        self.mode = 2;
                        self.auto_on = false;
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
                    self.auto_on = false;
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
            "cut-in-moved" => {
                self.cut_in = (n(0).round() as i64).clamp(0, 10);
                self.changed(host);
            }
            "high-quality-toggled" => {
                self.high_quality = event.args.first().and_then(Value::as_bool).unwrap_or(false);
                self.changed(host);
            }
            "picture-clicked" => self.choose_picture(host),
            "reset-clicked" => {
                let cfg = reset_config(&self.get_config(), &BackgroundPlugin::new().get_config(), &[]);
                if self.apply_preset(host, &cfg).is_ok() {
                    host.schedule_save();
                }
            }
            "download-clicked" => {
                host.download_vision();
                self.show(host);
            }
            "cutout-download-clicked" => {
                host.download_matting();
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
                    self.auto_on = false;
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
        let settings = Settings { mode: self.mode, blur: self.blur, edges: self.edges, cut_in: self.cut_in, picture: self.picture.clone(), high_quality: self.high_quality };
        let blur = MODES[self.mode].0 == "blur";
        let step = Box::new(Step { settings, wanted: self.wanted.clone(), covered: None, spares: Vec::new(), covered_yuv: None, derived: None, quarter: None, card: CardPictures::default(), card_mask: None, card_room: None });
        // Both go to the graphics card while it has room; a picture that can't be read leaves Picture mode on the CPU, where it blurs
        Some(if blur {
            FrameStep::Either(card_blur(self.blur), step)
        } else if self.picture.is_some() {
            FrameStep::Either(card_picture_step(), step)
        } else {
            FrameStep::Cpu(step)
        })
    }

    fn settings(&self) -> Vec<telescope_core::settings::SettingSpec> {
        use telescope_core::settings::SettingSpec;
        vec![
            SettingSpec::choice("mode", "What goes behind you", &[("off", "Nothing, the picture as it is"), ("blur", "The room blurred"), ("picture", "The picture chosen on the card")], "off"),
            SettingSpec::number("blur", "How blurred the room is in blur mode", 1.0, 10.0, 0, DEFAULT_BLUR as f64),
            SettingSpec::number("edges", "How soft the edge around you is", 0.0, 10.0, 0, DEFAULT_EDGES as f64),
        ]
    }

    fn get_config(&self) -> Value {
        let mut cfg = Map::new();
        // Blur that a cut-out switched on is not what was chosen, so the settings keep Off
        cfg.insert("mode".into(), json!(if self.auto_on { MODES[0].0 } else { self.mode_name() }));
        cfg.insert("blur".into(), json!(self.blur));
        cfg.insert("edges".into(), json!(self.edges));
        cfg.insert("picture".into(), json!(self.picture_path));
        // Only when set, like high_quality, so a settings file stays as it was until someone pulls the border in
        if self.cut_in != DEFAULT_CUT_IN {
            cfg.insert("cut_in".into(), json!(self.cut_in));
        }
        // Only when it's on, so a settings file stays as the Python app wrote it until someone asks for the better cutout
        if self.high_quality {
            cfg.insert("high_quality".into(), json!(true));
        }
        Value::Object(cfg)
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let empty = Value::Object(Map::new());
        let cfg = if cfg.is_object() { cfg } else { &empty };
        self.auto_on = false;
        self.mode = cfg.get("mode").and_then(Value::as_str).and_then(|m| MODES.iter().position(|(k, _)| *k == m)).unwrap_or(0);
        self.blur = clamped_int(cfg.get("blur").unwrap_or(&Value::Null), DEFAULT_BLUR, 1, 10);
        self.edges = clamped_int(cfg.get("edges").unwrap_or(&Value::Null), DEFAULT_EDGES, 0, 10);
        self.cut_in = clamped_int(cfg.get("cut_in").unwrap_or(&Value::Null), DEFAULT_CUT_IN, 0, 10);
        self.high_quality = cfg.get("high_quality").and_then(Value::as_bool).unwrap_or(false);
        let picture = cfg.get("picture").and_then(Value::as_str).unwrap_or("").to_string();
        self.load_picture(&picture);
        // once, with the whole of it: the next hook with a host shows it
        self.pending = true;
        self.retell_hiding = true;
        Ok(())
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        self.set_config(cfg)?;
        self.catch_up(host);
        host.refresh_frame_step();
        Ok(())
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        let quality = if self.high_quality && self.mode != 0 { ", high quality cutout" } else { "" };
        vec![("Background".into(), format!("{}{quality}", self.mode_name()))]
    }
}

#[cfg(test)]
#[path = "background_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "background_card_tests.rs"]
mod card_tests;
