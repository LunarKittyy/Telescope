//! The wait screen (vcam.WaitScreen): what the virtual camera shows while nothing streams to it, so apps list the
//! camera and show a picture instead of the driver's own "no signal" screen, and a reader starting up can be noticed.
//!
//! Cheap on purpose: frames are prepared once, in the camera's own format, and sent once a second until someone reads
//! the camera.

use crate::convert::rgb_to_i420;
use crate::held::{Gate, Held, Job, Nudge, Stop};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::i18n::tr;
use telescope_core::output::VideoOutput;

/// Before any stream has said how big the camera is (most phones stream 1080p).
pub const DEFAULT_SIZE: (u32, u32) = (1920, 1080);
/// A still image while someone reads: enough to look alive, near-free to send.
pub const STILL_PERIOD: f64 = 0.2;
/// Fastest an animation plays.
pub const MIN_GIF_PERIOD: f64 = 1.0 / 15.0;
/// Bytes of prepared animation frames kept in memory.
pub const FRAME_BUDGET: usize = 64 * 1024 * 1024;

/// How often things happen; tests make them shorter.
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    /// Between wait-screen frames while nobody is reading.
    pub idle: Duration,
    /// Windows: how often to look for an app's filter while nothing receives the frames.
    pub filter_poll: Duration,
    /// Before trying a camera that wouldn't open again.
    pub retry_open: Duration,
}

impl Default for Pace {
    fn default() -> Pace {
        Pace { idle: Duration::from_secs(1), filter_poll: Duration::from_millis(50), retry_open: Duration::from_secs(3) }
    }
}

// The app's colours (theme.py)
const BG: [u8; 3] = [0x0f, 0x12, 0x16];
const SURFACE: [u8; 3] = [0x16, 0x1a, 0x20];
const SURFACE_SUNK: [u8; 3] = [0x0a, 0x0c, 0x0f];
const TEXT: [u8; 3] = [0xe7, 0xea, 0xef];
const TEXT_DIM: [u8; 3] = [0xa1, 0xab, 0xb8];
const ACCENT: [u8; 3] = [0xaa, 0x9c, 0xf5];
const ACCENT_SOFT: [u8; 3] = [0xcd, 0xc4, 0xfa];
const GLOW: [u8; 3] = [0x6a, 0x58, 0xcf];

/// An RGB picture, 3 bytes a pixel, rows packed.
#[derive(Clone, Debug, PartialEq)]
pub struct Rgb {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

// ── The default screen ──────────────────────────────────────────────────────

// The default screen in the project banner's units, which the Windows 11 camera's DLL lays out by too (wait_screen.cpp)
const SCREEN_WIDTH: f32 = 500.0;
const SCREEN_WIDTH_SHARE: f32 = 0.8;
const SCREEN_LENS: f32 = 500.0;
const SCREEN_HEIGHT_SHARE: f32 = 0.82;
const SCREEN_WITH_NAME: f32 = 654.0;
const SCREEN_CENTRE: f32 = 250.0;
const SCREEN_NAME_TOP: f32 = 290.0;
const SCREEN_NAME_PX: f32 = 56.0;
const SCREEN_NAME_MIN_PX: f32 = 14.0;

/// (scale in pixels per banner unit, the lens's centre, whether the name fits under it) for a width x height screen.
pub fn screen_layout(width: u32, height: u32) -> (f32, (f32, f32), bool) {
    let (w, h) = (width as f32, height as f32);
    let fit = |group_h: f32| (w * SCREEN_WIDTH_SHARE / SCREEN_WIDTH).min(h * SCREEN_HEIGHT_SHARE / group_h);
    let mut k = fit(SCREEN_WITH_NAME);
    let with_name = SCREEN_NAME_PX * k >= SCREEN_NAME_MIN_PX;
    if !with_name {
        k = fit(SCREEN_LENS);
    }
    let group_h = if with_name { SCREEN_WITH_NAME } else { SCREEN_LENS } * k;
    (k, (w / 2.0, (h - group_h) / 2.0 + SCREEN_CENTRE * k), with_name)
}

fn colour(rgb: [u8; 3], alpha: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(rgb[0], rgb[1], rgb[2], (alpha * 255.0).round() as u8)
}

fn radial(centre: (f32, f32), radius: f32, stops: &[(f32, tiny_skia::Color)]) -> Option<tiny_skia::Shader<'static>> {
    let p = tiny_skia::Point::from_xy(centre.0, centre.1);
    let stops = stops.iter().map(|(at, c)| tiny_skia::GradientStop::new(*at, *c)).collect();
    tiny_skia::RadialGradient::new(p, 0.0, p, radius, stops, tiny_skia::SpreadMode::Pad, tiny_skia::Transform::identity())
}

fn circle(c: (f32, f32), r: f32) -> Option<tiny_skia::Path> {
    tiny_skia::PathBuilder::from_circle(c.0, c.1, r.max(0.01))
}

/// The lens from the project banner, with its glow over the whole width x height (vcam.paint_lens).
fn paint_lens(pm: &mut tiny_skia::Pixmap, c: (f32, f32), k: f32) {
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let id = tiny_skia::Transform::identity();
    let mut paint = tiny_skia::Paint { anti_alias: true, ..Default::default() };
    if let Some(shader) = radial(c, 420.0 * k, &[(0.0, colour(GLOW, 0.34)), (0.55, colour(GLOW, 0.08)), (1.0, colour(GLOW, 0.0))]) {
        paint.shader = shader;
        if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, 0.0, w, h) {
            pm.fill_rect(rect, &paint, id, None);
        }
    }
    for (r, a) in [(300.0, 0.10), (372.0, 0.06), (452.0, 0.035)] {
        paint.set_color(colour(ACCENT, a));
        let stroke = tiny_skia::Stroke { width: (1.5 * k).max(1.0), ..Default::default() };
        if let Some(path) = circle(c, r * k) {
            pm.stroke_path(&path, &paint, &stroke, id, None);
        }
    }
    for i in 0..120 {
        let major = i % 10 == 0;
        let angle = (i as f32 * 3.0).to_radians();
        let (inner, outer) = (232.0 * k, if major { 250.0 } else { 241.0 } * k);
        paint.set_color(colour(ACCENT, if major { 0.55 } else { 0.22 }));
        let stroke = tiny_skia::Stroke {
            width: (if major { 2.0 } else { 1.4 } * k).max(1.0), line_cap: tiny_skia::LineCap::Round, ..Default::default()
        };
        let (dx, dy) = (angle.cos(), angle.sin());
        let mut pb = tiny_skia::PathBuilder::new();
        pb.move_to(c.0 + dx * inner, c.1 + dy * inner);
        pb.line_to(c.0 + dx * outer, c.1 + dy * outer);
        if let Some(path) = pb.finish() {
            pm.stroke_path(&path, &paint, &stroke, id, None);
        }
    }
    let fill = |pm: &mut tiny_skia::Pixmap, path: Option<tiny_skia::Path>, shader: Option<tiny_skia::Shader<'static>>, t: tiny_skia::Transform| {
        if let (Some(path), Some(shader)) = (path, shader) {
            let paint = tiny_skia::Paint { shader, anti_alias: true, ..Default::default() };
            pm.fill_path(&path, &paint, tiny_skia::FillRule::Winding, t, None);
        }
    };
    let soft = tiny_skia::Color::from_rgba8(ACCENT_SOFT[0], ACCENT_SOFT[1], ACCENT_SOFT[2], 255);
    let accent = tiny_skia::Color::from_rgba8(ACCENT[0], ACCENT[1], ACCENT[2], 255);
    fill(pm, circle(c, 190.0 * k), radial((c.0 - 60.0 * k, c.1 - 70.0 * k), 260.0 * k, &[(0.0, soft), (1.0, accent)]), id);
    let surface = tiny_skia::Color::from_rgba8(SURFACE[0], SURFACE[1], SURFACE[2], 255);
    let sunk = tiny_skia::Color::from_rgba8(SURFACE_SUNK[0], SURFACE_SUNK[1], SURFACE_SUNK[2], 255);
    fill(pm, circle(c, 86.0 * k), radial((c.0 + 18.0 * k, c.1 + 22.0 * k), 110.0 * k, &[(0.0, surface), (1.0, sunk)]), id);
    let glint = tiny_skia::Rect::from_xywh(-20.0 * k, -13.0 * k, 40.0 * k, 26.0 * k).and_then(tiny_skia::PathBuilder::from_oval);
    let t = tiny_skia::Transform::from_translate(c.0 - 28.0 * k, c.1 - 30.0 * k).pre_rotate(-35.0);
    fill(pm, glint, Some(tiny_skia::Shader::SolidColor(colour([255, 255, 255], 0.16))), t);
    fill(pm, circle((c.0 + 26.0 * k, c.1 + 30.0 * k), 5.0 * k), Some(tiny_skia::Shader::SolidColor(colour([255, 255, 255], 0.10))), id);
}

/// A font to draw the name and note with: Inter like the app, else whatever sans the system has.
fn find_font(display: bool) -> Option<telescope_core::text::FontArc> {
    let names: &[&str] = if display {
        &["InterDisplay-Bold.ttf", "Inter-Bold.ttf", "DejaVuSans-Bold.ttf", "segoeuib.ttf", "arialbd.ttf"]
    } else {
        &["Inter-Regular.ttf", "Inter.ttf", "DejaVuSans.ttf", "segoeui.ttf", "arial.ttf"]
    };
    telescope_core::text::font_named(names)
}

/// text centred across the pixmap with its top at top, px high, letters spaced apart by spacing px.
fn draw_text(pm: &mut tiny_skia::Pixmap, font: &telescope_core::text::FontArc, text: &str, top: f32, px: f32, spacing: f32, rgb: [u8; 3]) {
    let face = telescope_core::text::Face::em(font.clone(), px);
    let line = face.layout(text, spacing);
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    face.draw(&line, (w as f32 - line.width) / 2.0, top + face.ascent(), |x, y, cov| {
        if x < 0 || y < 0 || x >= w || y >= h {
            return;
        }
        let i = ((y * w + x) * 4) as usize;
        for c in 0..3 {
            let under = data[i + c] as f32;
            data[i + c] = (under + (rgb[c] as f32 - under) * cov.clamp(0.0, 1.0)).round() as u8;
        }
    });
}

/// The lens from the project banner, centred (calls crop tiles to squares and circles), with the name under it.
pub fn default_screen(width: u32, height: u32) -> Rgb {
    screen_with(width, height, &tr("Waiting for camera"))
}

/// The default screen with `waiting` under the name ("" for no such line).
fn screen_with(width: u32, height: u32, waiting: &str) -> Rgb {
    let Some(mut pm) = tiny_skia::Pixmap::new(width.max(1), height.max(1)) else {
        return Rgb { width, height, data: BG.repeat((width * height) as usize) };
    };
    pm.fill(tiny_skia::Color::from_rgba8(BG[0], BG[1], BG[2], 255));
    let (k, centre, with_name) = screen_layout(width, height);
    paint_lens(&mut pm, centre, k);
    if with_name {
        let top = centre.1 + SCREEN_NAME_TOP * k;
        if let Some(font) = find_font(true) {
            draw_text(&mut pm, &font, "Telescope", top, (SCREEN_NAME_PX * k).round(), -1.5 * k, TEXT);
        }
        if let Some(font) = find_font(false).filter(|_| !waiting.is_empty()) {
            draw_text(&mut pm, &font, waiting, top + 74.0 * k, (22.0 * k).round().max(9.0), 0.0, TEXT_DIM);
        }
    }
    Rgb { width, height, data: rgb_of(&pm) }
}

fn rgb_of(pm: &tiny_skia::Pixmap) -> Vec<u8> {
    pm.data().as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect()
}

// ── The Windows 11 camera's built-in screen ─────────────────────────────────

const CAMERA_MAGIC: u32 = 0x5357_5354; // "TSWS"
const CAMERA_VERSION: u32 = 1;
/// Nothing is drawn further than this from the lens's centre: the outer ring and the glow.
const SCREEN_REACH: f32 = 460.0;
const SCREEN_NAME_HEIGHT: f32 = 70.0;
/// Pixels per banner unit: the lens is sharp up to 1080p and resized up beyond.
const LENS_SCALE: f32 = 1.4;
/// The name's edges show more, so it's kept sharper.
const NAME_SCALE: f32 = 3.0;
/// Banner units across the canvas the name is drawn on, before its mask is cut to it.
const NAME_ROOM: f32 = 1000.0;

/// (Y, interleaved UV) as the camera's frames are converted.
fn nv12(rgb: &[u8], width: u32, height: u32) -> (Vec<u8>, Vec<u8>) {
    let frame = rgb_to_i420(rgb, width, height, 3, false);
    let mut out = vec![0; (width * height * 3 / 2) as usize];
    crate::convert::write_nv12(&frame, &mut out);
    let uv = out.split_off((width * height) as usize);
    (out, uv)
}

fn solid(rgb: [u8; 3]) -> [u32; 3] {
    let (y, uv) = nv12(&rgb.repeat(4), 2, 2);
    [y[0] as u32, uv[0] as u32, uv[1] as u32]
}

/// What the camera's DLL shows while Telescope sends nothing (write_camera_wait_screen.py): the lens as NV12 and the name as a mask, read by wait_screen.cpp.
pub fn camera_wait_screen() -> Result<Vec<u8>, String> {
    let blank = |w, h, fill: [u8; 3]| {
        let mut pm = tiny_skia::Pixmap::new(w, h).ok_or("a picture that size won't fit")?;
        pm.fill(tiny_skia::Color::from_rgba8(fill[0], fill[1], fill[2], 255));
        Ok::<_, String>(pm)
    };
    let size = 2 * (SCREEN_REACH * LENS_SCALE).round() as u32;
    let mut pm = blank(size, size, BG)?;
    paint_lens(&mut pm, (size as f32 / 2.0, size as f32 / 2.0), LENS_SCALE);
    let (lens_y, lens_uv) = nv12(&rgb_of(&pm), size, size);

    let font = find_font(true).ok_or("No font here can draw the name: it would show as boxes")?;
    let (room, name_h) = ((NAME_ROOM * NAME_SCALE).round() as u32, (SCREEN_NAME_HEIGHT * NAME_SCALE).round() as u32);
    let mut pm = blank(room, name_h, [0, 0, 0])?;
    draw_text(&mut pm, &font, "Telescope", 0.0, (SCREEN_NAME_PX * NAME_SCALE).round(), -1.5 * NAME_SCALE, [255, 255, 255]);
    let reds: Vec<u8> = pm.data().as_chunks::<4>().0.iter().map(|p| p[0]).collect();
    let rows: Vec<&[u8]> = reds.chunks(room as usize).collect();
    let used = |x: &usize| rows.iter().any(|row| row[*x] != 0);
    let room_px = room as usize;
    let inside = |x: &usize| *x != 0 && *x != room_px - 1;
    let (Some(first), Some(last)) = ((0..room_px).find(used).filter(inside), (0..room_px).rev().find(used).filter(inside)) else {
        return Err("The name doesn't fit its canvas: raise NAME_ROOM".into());
    };
    // Cut evenly, so the mask stays centred on the name
    let reach = (room_px / 2 - first).max(last + 1 - room_px / 2) + 2;
    let mask: Vec<u8> = rows.iter().flat_map(|row| row[room_px / 2 - reach..room_px / 2 + reach].iter().copied()).collect();

    let [bg_y, bg_u, bg_v] = solid(BG);
    let [text_y, text_u, text_v] = solid(TEXT);
    let header = [
        CAMERA_MAGIC, CAMERA_VERSION, bg_y, bg_u, bg_v, text_y, text_u, text_v,
        (SCREEN_WIDTH_SHARE * 1000.0).round() as u32, (SCREEN_HEIGHT_SHARE * 1000.0).round() as u32, SCREEN_WIDTH as u32,
        SCREEN_LENS as u32, SCREEN_WITH_NAME as u32, SCREEN_CENTRE as u32, SCREEN_NAME_TOP as u32, SCREEN_NAME_PX as u32,
        SCREEN_NAME_MIN_PX as u32, size, (LENS_SCALE * 1000.0).round() as u32, 2 * reach as u32, name_h, (NAME_SCALE * 1000.0).round() as u32,
    ];
    let mut out: Vec<u8> = header.iter().flat_map(|v| v.to_le_bytes()).collect();
    out.extend(lens_y);
    out.extend(lens_uv);
    out.extend(mask);
    Ok(out)
}

// ── Pictures people pick ────────────────────────────────────────────────────

/// img scaled to fit width x height, centred on the app background (transparency shows it too).
fn fit_image(img: &image::RgbaImage, width: u32, height: u32) -> Rgb {
    let mut data = SURFACE_SUNK.repeat((width * height) as usize);
    let scale = (width as f64 / img.width() as f64).min(height as f64 / img.height() as f64);
    let (sw, sh) = (((img.width() as f64 * scale).round() as u32).max(1), ((img.height() as f64 * scale).round() as u32).max(1));
    let scaled = image::imageops::resize(img, sw.min(width), sh.min(height), image::imageops::FilterType::Triangle);
    let (x0, y0) = ((width - scaled.width()) / 2, (height - scaled.height()) / 2);
    for (x, y, p) in scaled.enumerate_pixels() {
        let i = (((y0 + y) * width + x0 + x) * 3) as usize;
        let a = p[3] as f32 / 255.0;
        for c in 0..3 {
            data[i + c] = (data[i + c] as f32 * (1.0 - a) + p[c] as f32 * a).round() as u8;
        }
    }
    Rgb { width, height, data }
}

/// What a prepared picture costs to keep in memory, which is what FRAME_BUDGET counts.
pub trait KeptPicture {
    fn nbytes(&self) -> usize;
}

impl KeptPicture for Rgb {
    fn nbytes(&self) -> usize {
        self.data.len()
    }
}

impl KeptPicture for Frame {
    fn nbytes(&self) -> usize {
        self.data.len()
    }
}

/// The image or animation at path as [(picture, seconds)] fitted to width x height; empty if it won't load. An
/// animation keeps every nth frame if all of them wouldn't fit in FRAME_BUDGET (vcam.read_image_frames).
pub fn read_image_frames(path: &str, width: u32, height: u32) -> Vec<(Rgb, f64)> {
    read_image_frames_as(path, width, height, &|rgb| rgb)
}

/// read_image_frames, with convert turning each picture into what gets kept as it loads, so the budget counts that.
pub fn read_image_frames_as<T: KeptPicture>(path: &str, width: u32, height: u32, convert: &dyn Fn(Rgb) -> T) -> Vec<(T, f64)> {
    match read_frames(path, width, height, FRAME_BUDGET, convert) {
        Ok(frames) if !frames.is_empty() => frames,
        Ok(_) => {
            log::warn!("Wait screen image {path:?} didn't load: no frames");
            Vec::new()
        }
        Err(e) => {
            log::warn!("Wait screen image {path:?} didn't load: {e}");
            Vec::new()
        }
    }
}

/// The frames of the animation at path, or None for a picture that isn't one (a WebP can be either).
fn animation_frames(path: &str, format: Option<image::ImageFormat>) -> Result<Option<image::Frames<'static>>, String> {
    use image::AnimationDecoder;
    let file = || std::fs::File::open(path).map(std::io::BufReader::new).map_err(|e| e.to_string());
    Ok(match format {
        Some(image::ImageFormat::Gif) => Some(image::codecs::gif::GifDecoder::new(file()?).map_err(|e| e.to_string())?.into_frames()),
        Some(image::ImageFormat::WebP) => {
            let decoder = image::codecs::webp::WebPDecoder::new(file()?).map_err(|e| e.to_string())?;
            decoder.has_animation().then(|| decoder.into_frames())
        }
        _ => None,
    })
}

fn read_frames<T: KeptPicture>(path: &str, width: u32, height: u32, budget: usize, convert: &dyn Fn(Rgb) -> T) -> Result<Vec<(T, f64)>, String> {
    use image::ImageDecoder;
    let open = || -> Result<image::ImageReader<std::io::BufReader<std::fs::File>>, String> {
        let mut reader = image::ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
        // A huge photo is scaled down as it's read, but nothing is allowed to ask for more than this
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(DECODE_LIMIT);
        reader.limits(limits);
        Ok(reader)
    };
    let format = open()?.format();
    let count = animation_frames(path, format)?.map_or(0, |frames| frames.count());
    if count > 1 {
        let mut keep_every = 1;
        let mut frames: Vec<(T, f64)> = Vec::new();
        for (i, frame) in animation_frames(path, format)?.into_iter().flatten().enumerate() {
            let frame = frame.map_err(|e| e.to_string())?;
            let (num, den) = frame.delay().numer_denom_ms();
            let seconds = (num as f64 / den.max(1) as f64 / 1000.0).max(MIN_GIF_PERIOD);
            if i % keep_every == 0 && frames.first().is_none_or(|first| frames.len() * first.0.nbytes() < budget) {
                frames.push((convert(fit_image(frame.buffer(), width, height)), seconds));
                if i == 0 {
                    keep_every = (count * frames[0].0.nbytes()).div_ceil(budget).max(1);
                }
            } else if let Some(last) = frames.last_mut() {
                last.1 += seconds;
            }
        }
        return Ok(frames);
    }
    let mut decoder = open()?.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().ok();
    let mut img = decode_still(decoder, width.max(height), QT_ALLOC_LIMIT)?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(vec![(convert(fit_image(&img.to_rgba8(), width, height)), STILL_PERIOD)])
}

/// What Qt's allocation limit is by default, which Python's wait screen measures a photo against.
const QT_ALLOC_LIMIT: u64 = 256 * 1024 * 1024;
/// The most a picture's decoder may allocate: room for a 100 megapixel photo, and nothing a few header bytes could claim.
const DECODE_LIMIT: u64 = 512 * 1024 * 1024;

/// The size a picture too big for Qt's allocation limit is read at, if it is (vcam._limit_decode_size): fitted to
/// longest a side, keeping its shape. The longest side, not width x height, so a rotated photo still has pixels to spare.
fn reduced_decode_size(w: u32, h: u32, longest: u32, limit: u64) -> Option<(u32, u32)> {
    if w.max(h) <= longest || (w as u64 * h as u64 * 4) <= limit / 2 {
        return None;
    }
    // QSize::scaled(KeepAspectRatio) in whole numbers, which truncates
    let (w, h, longest) = (w as u64, h as u64, longest as u64);
    let wide = longest * w / h;
    Some(if wide <= longest { (wide as u32, longest as u32) } else { (longest as u32, (longest * h / w) as u32) })
}

/// A still picture, shrunk to the reduced size when it is big. Python asks Qt for that size, which a JPEG gives by
/// decoding fewer coefficients; the image crate reads it whole (within DECODE_LIMIT) and it is shrunk before it is kept.
fn decode_still(decoder: impl image::ImageDecoder, longest: u32, limit: u64) -> Result<image::DynamicImage, String> {
    let (w, h) = decoder.dimensions();
    // The JPEG decoder doesn't hold itself to the reader's limit, so the header's claim is checked here before anything is allocated
    if decoder.total_bytes() > DECODE_LIMIT {
        return Err(format!("a {w} x {h} picture is over the memory limit for reading one"));
    }
    let img = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    Ok(match reduced_decode_size(w, h, longest, limit) {
        Some((rw, rh)) => img.resize_exact(rw.max(1), rh.max(1), image::imageops::FilterType::Triangle),
        None => img,
    })
}

/// The wait screen as [(picture, seconds)] fitted to width x height: the image or animation at path, or the default
/// screen when there's no path or it won't load (vcam.load_frames).
pub fn load_frames(path: Option<&str>, width: u32, height: u32) -> Vec<(Rgb, f64)> {
    load_frames_as(path, width, height, &|rgb| rgb)
}

/// load_frames, with convert turning each picture into what gets kept as it loads.
pub fn load_frames_as<T: KeptPicture>(path: Option<&str>, width: u32, height: u32, convert: &dyn Fn(Rgb) -> T) -> Vec<(T, f64)> {
    let mut frames = path.map(|p| read_image_frames_as(p, width, height, convert)).unwrap_or_default();
    if frames.is_empty() {
        frames = vec![(convert(default_screen(width, height)), STILL_PERIOD)];
    }
    if frames.len() == 1 {
        frames[0].1 = STILL_PERIOD;
    }
    frames
}

// ── Holding the camera ──────────────────────────────────────────────────────

/// Opens the camera at width x height, fps, taking frames of the format given.
pub type OpenCamera = Arc<dyn Fn(u32, u32, u32, PixelFormat) -> Result<Box<dyn VideoOutput>, String> + Send + Sync>;
/// The wait screen's pictures for (path, width, height); Err when they couldn't be made at all.
pub type Loader = Arc<dyn Fn(Option<&str>, u32, u32) -> Result<Vec<(Rgb, f64)>, String> + Send + Sync>;
/// The wait screen's frames for (path, width, height), each turned into what's sent as it loads.
pub type FrameLoader = Arc<dyn Fn(Option<&str>, u32, u32, &dyn Fn(Rgb) -> Frame) -> Result<Vec<(Frame, f64)>, String> + Send + Sync>;
/// The size an app still reading the camera holds it at.
pub type Locked = Arc<dyn Fn() -> Option<(u32, u32)> + Send + Sync>;
/// Windows: a check made once per opening, telling whether an app has opened the camera yet.
pub type FilterProbe = Arc<dyn Fn() -> Option<Box<dyn Fn() -> bool + Send>> + Send + Sync>;

/// What the wait screen needs from the system, so tests can stand in for it.
#[derive(Clone)]
pub struct Deps {
    pub open_camera: OpenCamera,
    pub loader: FrameLoader,
    pub locked: Locked,
    pub filter_probe: Option<FilterProbe>,
    /// Prepare frames as I420 (the camera's own format) when the size allows; RGBA otherwise.
    pub i420: bool,
    pub pace: Pace,
}

/// Prepared frames for one size: the format they're in and each with how long it shows.
type Prepared = ((u32, u32), PixelFormat, Vec<(Frame, f64)>);

pub struct ScreenJob {
    deps: Deps,
    shown: Mutex<((u32, u32), Option<String>, bool)>,
    watched: AtomicBool,
    nudge: Nudge,
}

impl ScreenJob {
    fn prepare(&self, width: u32, height: u32) -> Result<(PixelFormat, Vec<(Frame, f64)>), String> {
        let (path, mirror) = {
            let s = self.shown.lock().unwrap();
            (s.1.clone(), s.2)
        };
        let i420 = self.deps.i420 && width.is_multiple_of(2) && height.is_multiple_of(2);
        let convert = |rgb: Rgb| {
            if i420 {
                rgb_to_i420(&rgb.data, rgb.width, rgb.height, 3, mirror)
            } else {
                Frame::new(rgb.width, rgb.height, PixelFormat::Rgba, rgba(&rgb, mirror))
            }
        };
        let frames = (self.deps.loader)(path.as_deref(), width, height, &convert)?;
        Ok((if i420 { PixelFormat::I420 } else { PixelFormat::Rgba }, frames))
    }

    fn feed(&self, cam: &mut dyn VideoOutput, frames: &[(Frame, f64)], stop: &Stop) -> Result<(), String> {
        let pace = self.deps.pace;
        let filter_open = self.deps.filter_probe.as_ref().and_then(|probe| probe());
        let mut i = 0;
        let mut checked = Instant::now();
        while !stop.is_set() {
            if checked.elapsed() >= pace.idle {
                checked = Instant::now();
                if cam.outdated() {
                    return Ok(());
                }
            }
            let (frame, seconds) = &frames[i];
            self.nudge.clear();
            cam.send(frame)?;
            if self.watched.load(Ordering::SeqCst) {
                i = (i + 1) % frames.len();
                self.nudge.wait(Duration::from_secs_f64(*seconds));
            } else if let Some(open) = filter_open.as_ref().filter(|open| !open()) {
                // Until an app opens the camera there's nowhere to send, and it shows its own screen until our first
                // frame lands: send the moment it's there (and at the idle pace regardless, should the probe be wrong).
                let until = Instant::now() + pace.idle;
                while !self.nudge.wait(pace.filter_poll) && !open() && Instant::now() < until {}
            } else {
                self.nudge.wait(pace.idle);
            }
        }
        Ok(())
    }
}

/// An RGB picture as RGBA bytes, flipped left to right if mirror.
pub(crate) fn rgba(rgb: &Rgb, mirror: bool) -> Vec<u8> {
    let (w, h) = (rgb.width as usize, rgb.height as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let x = if mirror { w - 1 - x } else { x };
            let i = (y * w + x) * 3;
            out.extend_from_slice(&[rgb.data[i], rgb.data[i + 1], rgb.data[i + 2], 255]);
        }
    }
    out
}

impl Job for ScreenJob {
    fn run(&self, stop: &Stop) {
        let mut prepared: Option<Prepared> = None;
        let mut complained = false;
        while !stop.is_set() {
            // An app still reading what the stream sent keeps the camera at that size, so show the screen at it.
            let size = (self.deps.locked)().unwrap_or_else(|| self.shown.lock().unwrap().0);
            if prepared.as_ref().is_none_or(|p| p.0 != size) {
                match self.prepare(size.0, size.1) {
                    Ok((fmt, frames)) => prepared = Some((size, fmt, frames)),
                    Err(e) => {
                        log::error!("Preparing the wait screen failed: {e}");
                        return;
                    }
                }
            }
            let (_, fmt, frames) = prepared.as_ref().unwrap();
            let mut cam = match (self.deps.open_camera)(size.0, size.1, 30, *fmt) {
                Ok(cam) => cam,
                Err(e) => {
                    if !complained {
                        log::info!("Wait screen can't open the virtual camera yet: {e}");
                        complained = true;
                    }
                    stop.wait(self.deps.pace.retry_open);
                    continue;
                }
            };
            complained = false;
            if let Err(e) = self.feed(cam.as_mut(), frames, stop) {
                log::error!("Wait screen lost the virtual camera: {e}");
                drop(cam);
                stop.wait(self.deps.pace.retry_open);
            }
        }
    }

    fn wake(&self) {
        self.nudge.set();
    }
}

/// Holds the virtual camera while nothing streams and shows the wait screen on it.
pub struct WaitScreen {
    held: Held<ScreenJob>,
}

impl WaitScreen {
    pub fn new(deps: Deps, gate: &Arc<Gate>) -> WaitScreen {
        let job = ScreenJob { deps, shown: Mutex::new((DEFAULT_SIZE, None, false)), watched: AtomicBool::new(false), nudge: Nudge::default() };
        WaitScreen { held: Held::new(job, gate) }
    }
}

impl telescope_core::output::Held for WaitScreen {
    fn start(&self) {
        self.held.start();
    }
    fn stop(&self) {
        self.held.stop();
    }
    fn running(&self) -> bool {
        self.held.running()
    }
}

impl telescope_core::output::WaitScreen for WaitScreen {
    fn show(&self, width: u32, height: u32, path: Option<&str>, mirror: bool) {
        let wanted = ((width, height), path.map(String::from), mirror);
        if self.held.alive_and_wanted() && *self.held.job().shown.lock().unwrap() == wanted {
            return;
        }
        let job = self.held.job().clone();
        self.held.restart_with(move || *job.shown.lock().unwrap() = wanted);
    }

    fn set_watched(&self, watched: bool) {
        let job = self.held.job();
        job.watched.store(watched, Ordering::SeqCst);
        job.nudge.set();
    }
}

#[cfg(test)]
mod camera_tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const HARNESS: &str = r#"
#include "wait_screen.h"
#include <cstdio>
#include <cstdlib>
int main(int, char** argv) {
    std::vector<uint8_t> data, out;
    FILE* f = fopen(argv[1], "rb");
    for (int c; (c = fgetc(f)) != EOF;) data.push_back(static_cast<uint8_t>(c));
    fclose(f);
    if (!telescope::ComposeWaitScreen(data.data(), data.size(), atoi(argv[3]), atoi(argv[4]), out)) return 3;
    f = fopen(argv[2], "wb");
    fwrite(out.data(), 1, out.size(), f);
    fclose(f);
}
"#;

    fn camera() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../desktop/virtualcamera")
    }

    fn read(name: &str) -> String {
        std::fs::read_to_string(camera().join(name)).unwrap()
    }

    fn compiler() -> Option<&'static str> {
        ["c++", "clang++", "g++"].into_iter().find(|c| Command::new(c).arg("--version").output().is_ok())
    }

    /// The header's fields as wait_screen.h names them, in order.
    fn header_fields() -> Vec<String> {
        let header = read("wait_screen.h");
        let body = &header[header.find("struct WaitScreenHeader {").unwrap()..];
        let body = &body[..body.find("};").unwrap()];
        body.lines().filter_map(|l| l.trim().strip_prefix("uint32_t ")).flat_map(|l| l[..l.find(';').unwrap()].split(','))
            .map(|n| n.trim().to_string()).collect()
    }

    fn harness(dir: &Path, flags: &[&str]) -> Option<PathBuf> {
        let exe = dir.join("harness");
        std::fs::write(dir.join("harness.cpp"), HARNESS).unwrap();
        let built = Command::new(compiler()?).args(flags).arg(format!("-I{}", camera().display())).arg(dir.join("harness.cpp"))
            .arg(camera().join("wait_screen.cpp")).arg("-o").arg(&exe).status().unwrap();
        assert!(built.success());
        Some(exe)
    }

    fn resource() -> Option<Vec<u8>> {
        find_font(true).map(|_| camera_wait_screen().unwrap())
    }

    // test_camera_wait_screen.py::test_the_header_is_the_one_the_dll_reads
    #[test]
    fn the_header_is_the_one_the_dll_reads() {
        let Some(resource) = resource() else { return };
        let fields = header_fields();
        let value = |name: &str| {
            let at = fields.iter().position(|f| f == name).unwrap_or_else(|| panic!("{name}"));
            u32::from_le_bytes(resource[at * 4..at * 4 + 4].try_into().unwrap())
        };
        assert_eq!((value("magic"), value("version")), (CAMERA_MAGIC, CAMERA_VERSION));
        assert!(read("wait_screen.h").contains(&format!("kWaitScreenVersion = {CAMERA_VERSION};")));
        assert_eq!((value("screen_width"), value("screen_with_name"), value("name_top")), (500, 654, 290));
        let (lens, mask) = (value("lens_size").pow(2) as usize, (value("name_width") * value("name_height")) as usize);
        assert_eq!(resource.len(), 4 * fields.len() + lens + lens / 2 + mask);
    }

    // test_camera_wait_screen.py::test_the_cmake_resource_id_is_the_one_the_dll_loads
    #[test]
    fn the_cmake_resource_id_is_the_one_the_dll_loads() {
        assert!(read("wait_screen.h").contains("kWaitScreenResource = 101;"));
        assert!(read("CMakeLists.txt").contains("\"101 RCDATA"));
    }

    fn rgb_of_nv12(data: &[u8], w: usize, h: usize) -> Vec<[u8; 3]> {
        let (y, uv) = data.split_at(w * h);
        (0..h).flat_map(|r| (0..w).map(move |c| (r, c))).map(|(r, c)| {
            let at = (r / 2) * w + (c / 2) * 2;
            crate::convert::yuv_to_rgb(y[r * w + c], uv[at], uv[at + 1])
        }).collect()
    }

    // test_camera_wait_screen.py::test_the_dll_lays_it_out_like_the_app, with this app's screen as the one to match
    #[test]
    fn the_dll_lays_it_out_like_the_app() {
        let (Some(resource), Some(_)) = (resource(), compiler()) else { return };
        let dir = telescope_core::testing::scratch_dir("camera-layout");
        let exe = harness(&dir, &["-std=c++17", "-O2"]).unwrap();
        std::fs::write(dir.join("screen.bin"), resource).unwrap();
        for (w, h) in [(1280u32, 720u32), (640, 480), (1080, 1920)] {
            let ran = Command::new(&exe).arg(dir.join("screen.bin")).arg(dir.join("out.nv12")).arg(w.to_string()).arg(h.to_string()).status().unwrap();
            assert!(ran.success());
            let (ws, hs) = (w as usize, h as usize);
            let dll = rgb_of_nv12(&std::fs::read(dir.join("out.nv12")).unwrap(), ws, hs);
            let screen = screen_with(w, h, ""); // the camera has no "Waiting for camera" line
            let (y, uv) = nv12(&screen.data, w, h);
            let app = rgb_of_nv12(&[y, uv].concat(), ws, hs);
            let (k, (_x, cy), _with_name) = screen_layout(w, h);
            let name_top = (cy + SCREEN_NAME_TOP * k) as usize;
            let mut above: Vec<i32> = Vec::new();
            let mut below: Vec<i32> = Vec::new();
            for (i, (a, b)) in dll.iter().zip(&app).enumerate() {
                let side = if i / ws < name_top { &mut above } else { &mut below };
                side.extend((0..3).map(|c| (a[c] as i32 - b[c] as i32).abs()));
            }
            let mean = |v: &[i32]| v.iter().map(|d| *d as f64).sum::<f64>() / v.len().max(1) as f64;
            above.sort_unstable();
            let p99 = above[above.len() * 99 / 100];
            assert!(mean(&above) < 1.0 && p99 < 8, "{w}x{h}: mean {}, 99% {p99}", mean(&above));
            assert!(mean(&below) < 5.0, "{w}x{h}: the name: {}", mean(&below));
        }
    }

    // test_camera_wait_screen.py::test_the_dll_turns_down_a_cut_short_resource
    #[test]
    fn the_dll_turns_down_a_cut_short_resource() {
        let (Some(mut resource), Some(_)) = (resource(), compiler()) else { return };
        let dir = telescope_core::testing::scratch_dir("camera-short");
        let exe = harness(&dir, &["-std=c++17"]).unwrap();
        resource.pop();
        std::fs::write(dir.join("screen.bin"), resource).unwrap();
        let ran = Command::new(&exe).arg(dir.join("screen.bin")).arg(dir.join("out.nv12")).args(["640", "480"]).status().unwrap();
        assert_eq!(ran.code(), Some(3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::output::{Held as _, WaitScreen as _};

    #[derive(Default)]
    struct Log {
        cams: Mutex<Vec<Arc<CamLog>>>,
        attempts: Mutex<Vec<(u32, u32)>>,
    }

    struct CamLog {
        size: (u32, u32),
        fmt: PixelFormat,
        sent: Mutex<Vec<Frame>>,
        closed: AtomicBool,
    }

    struct Cam(Arc<CamLog>, Arc<AtomicBool>);

    impl VideoOutput for Cam {
        fn send(&mut self, frame: &Frame) -> Result<(), String> {
            self.0.sent.lock().unwrap().push(frame.clone());
            Ok(())
        }
        fn size(&self) -> (u32, u32) {
            self.0.size
        }
        fn device(&self) -> String {
            "fake".into()
        }
        fn outdated(&self) -> bool {
            self.1.load(Ordering::SeqCst)
        }
    }

    impl Drop for Cam {
        fn drop(&mut self) {
            self.0.closed.store(true, Ordering::SeqCst);
        }
    }

    impl Log {
        fn cams(&self) -> Vec<Arc<CamLog>> {
            self.cams.lock().unwrap().clone()
        }
        fn sent(&self, i: usize) -> usize {
            self.cams().get(i).map_or(0, |c| c.sent.lock().unwrap().len())
        }
    }

    fn black(_path: Option<&str>, w: u32, h: u32) -> Result<Vec<(Rgb, f64)>, String> {
        Ok(vec![(Rgb { width: w, height: h, data: vec![0; (w * h * 3) as usize] }, STILL_PERIOD)])
    }

    struct Setup {
        fail: usize,
        loader: Loader,
        locked: Locked,
        filter: Option<FilterProbe>,
        pace: Pace,
        outdated: Arc<AtomicBool>,
    }

    impl Default for Setup {
        fn default() -> Setup {
            Setup { fail: 0, loader: Arc::new(black), locked: Arc::new(|| None), filter: None, pace: Pace::default(), outdated: Arc::default() }
        }
    }

    fn screen(setup: Setup) -> (WaitScreen, Arc<Log>, Arc<Gate>) {
        let log = Arc::new(Log::default());
        let l = log.clone();
        let (fail, outdated) = (setup.fail, setup.outdated.clone());
        let open: OpenCamera = Arc::new(move |w, h, _fps, fmt| {
            let mut attempts = l.attempts.lock().unwrap();
            attempts.push((w, h));
            if attempts.len() <= fail {
                return Err("no device".into());
            }
            let cam = Arc::new(CamLog { size: (w, h), fmt, sent: Mutex::default(), closed: AtomicBool::new(false) });
            l.cams.lock().unwrap().push(cam.clone());
            Ok(Box::new(Cam(cam, outdated.clone())) as Box<dyn VideoOutput>)
        });
        let gate = Arc::new(Gate::default());
        let pictures = setup.loader;
        let loader: FrameLoader = Arc::new(move |path, w, h, convert| {
            Ok(pictures(path, w, h)?.into_iter().map(|(rgb, seconds)| (convert(rgb), seconds)).collect())
        });
        let deps = Deps { open_camera: open, loader, locked: setup.locked, filter_probe: setup.filter, i420: true, pace: setup.pace };
        (WaitScreen::new(deps, &gate), log, gate)
    }

    fn wait_for(what: impl Fn() -> bool, secs: f64) {
        let until = Instant::now() + Duration::from_secs_f64(secs);
        while !what() {
            assert!(Instant::now() < until, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // test_vcam.py::test_holds_the_camera_until_stopped
    #[test]
    fn holds_the_camera_until_stopped() {
        let (s, log, _g) = screen(Setup::default());
        s.show(64, 36, None, false);
        wait_for(|| log.sent(0) > 0, 2.0);
        assert_eq!(log.cams()[0].size, (64, 36));
        assert!(s.running());
        s.stop();
        assert!(log.cams()[0].closed.load(Ordering::SeqCst) && !s.running());
    }

    // test_vcam.py::test_a_wait_screen_that_failed_to_prepare_starts_again_when_asked
    #[test]
    fn a_wait_screen_that_failed_to_prepare_starts_again_when_asked() {
        let calls = Arc::new(Mutex::new(0));
        let c = calls.clone();
        let loader: Loader = Arc::new(move |p, w, h| {
            *c.lock().unwrap() += 1;
            if *c.lock().unwrap() == 1 { Err("image unreadable for a moment".into()) } else { black(p, w, h) }
        });
        let (s, log, _g) = screen(Setup { loader, ..Setup::default() });
        s.show(64, 36, None, false);
        wait_for(|| !s.running(), 2.0);
        s.show(64, 36, None, false);
        wait_for(|| log.sent(0) > 0, 2.0);
        s.stop();
    }

    // test_vcam.py::test_mirror_flips_the_screen_left_to_right
    #[test]
    fn mirror_flips_the_screen_left_to_right() {
        let loader: Loader = Arc::new(|_p, w, h| {
            let mut data = vec![0u8; (w * h * 3) as usize];
            for y in 0..h {
                for x in 0..w / 2 {
                    let i = ((y * w + x) * 3) as usize;
                    data[i..i + 3].copy_from_slice(&[255, 255, 255]);
                }
            }
            Ok(vec![(Rgb { width: w, height: h, data }, STILL_PERIOD)])
        });
        let (s, log, _g) = screen(Setup { loader, ..Setup::default() });
        s.show(64, 36, None, true);
        wait_for(|| log.sent(0) > 0, 2.0);
        s.stop();
        let frame = log.cams()[0].sent.lock().unwrap()[0].clone();
        let (y, _, _) = frame.i420_planes().unwrap();
        let (left, right) = (y[0] as u32, y[63] as u32);
        assert!(left < right, "the white half is on the right now");
    }

    // test_vcam.py::test_opens_at_the_size_a_reader_holds_the_camera_at
    #[test]
    fn opens_at_the_size_a_reader_holds_the_camera_at() {
        let (s, log, _g) = screen(Setup { locked: Arc::new(|| Some((640, 480))), ..Setup::default() });
        s.show(64, 36, None, false);
        wait_for(|| log.sent(0) > 0, 2.0);
        s.stop();
        assert_eq!(log.cams()[0].size, (640, 480));
        assert_eq!(log.cams()[0].sent.lock().unwrap()[0].width, 640);
    }

    // test_vcam.py::test_linux_sends_the_cameras_own_format_prepared_once
    #[test]
    fn sends_the_cameras_own_format_prepared_once() {
        let (s, log, _g) = screen(Setup::default());
        s.show(64, 36, None, false);
        wait_for(|| log.sent(0) > 0, 2.0);
        s.stop();
        assert_eq!(log.cams()[0].fmt, PixelFormat::I420);
        assert_eq!(log.cams()[0].sent.lock().unwrap()[0].data.len(), 64 * 36 * 3 / 2);
    }

    // test_vcam.py::test_odd_sizes_and_windows_send_rgb (Windows' cameras here take I420 too)
    #[test]
    fn odd_sizes_send_rgba() {
        let (s, log, _g) = screen(Setup::default());
        s.show(63, 36, None, false);
        wait_for(|| log.sent(0) > 0, 2.0);
        s.stop();
        assert_eq!(log.cams()[0].fmt, PixelFormat::Rgba);
    }

    // test_vcam.py::test_sends_rarely_until_someone_watches
    #[test]
    fn sends_rarely_until_someone_watches() {
        let pace = Pace { idle: Duration::from_millis(300), ..Pace::default() };
        let (s, log, _g) = screen(Setup { pace, ..Setup::default() });
        s.show(8, 8, None, false);
        wait_for(|| log.sent(0) > 0, 2.0);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(log.sent(0), 1);
        s.set_watched(true);
        wait_for(|| log.sent(0) >= 3, 1.0);
        s.stop();
    }

    // test_vcam.py::test_windows_sends_the_moment_an_app_opens_the_camera
    #[test]
    fn sends_the_moment_an_app_opens_the_camera() {
        let polls = Arc::new(Mutex::new(0));
        let p = polls.clone();
        let filter: FilterProbe = Arc::new(move || {
            let p = p.clone();
            Some(Box::new(move || {
                let mut n = p.lock().unwrap();
                *n += 1;
                *n > 5
            }) as Box<dyn Fn() -> bool + Send>)
        });
        let pace = Pace { idle: Duration::from_secs(5), filter_poll: Duration::from_millis(10), ..Pace::default() };
        let (s, log, _g) = screen(Setup { filter: Some(filter), pace, ..Setup::default() });
        s.show(8, 8, None, false);
        wait_for(|| log.sent(0) >= 2, 1.0);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(log.sent(0), 2);
        assert_eq!(*polls.lock().unwrap(), 7, "then one look per idle send, not every 10 ms");
        s.stop();
    }

    // test_vcam.py::test_a_filter_never_found_still_gets_the_idle_pace
    #[test]
    fn a_filter_never_found_still_gets_the_idle_pace() {
        let filter: FilterProbe = Arc::new(|| Some(Box::new(|| false) as Box<dyn Fn() -> bool + Send>));
        let pace = Pace { idle: Duration::from_millis(100), filter_poll: Duration::from_millis(10), ..Pace::default() };
        let (s, log, _g) = screen(Setup { filter: Some(filter), pace, ..Setup::default() });
        s.show(8, 8, None, false);
        wait_for(|| log.sent(0) >= 3, 1.0);
        s.stop();
    }

    // test_vcam.py::test_a_camera_that_wont_open_is_tried_again
    #[test]
    fn a_camera_that_wont_open_is_tried_again() {
        let pace = Pace { retry_open: Duration::from_millis(10), ..Pace::default() };
        let (s, log, _g) = screen(Setup { fail: 2, pace, ..Setup::default() });
        s.show(8, 8, None, false);
        wait_for(|| !log.cams().is_empty(), 2.0);
        s.stop();
        assert_eq!(log.attempts.lock().unwrap().len(), 3);
    }

    // test_vcam.py::test_showing_the_same_thing_again_keeps_the_camera_open
    #[test]
    fn showing_the_same_thing_again_keeps_the_camera_open() {
        let (s, log, _g) = screen(Setup::default());
        s.show(8, 8, None, false);
        wait_for(|| !log.cams().is_empty(), 2.0);
        s.show(8, 8, None, false);
        assert_eq!(log.cams().len(), 1);
        s.show(16, 16, None, false);
        wait_for(|| log.cams().len() == 2, 2.0);
        s.stop();
        assert!(log.cams()[0].closed.load(Ordering::SeqCst));
        assert_eq!(log.cams()[1].size, (16, 16));
    }

    // test_vcam.py::test_lets_go_for_a_driver_reload_and_takes_it_back
    #[test]
    fn lets_go_for_a_driver_reload_and_takes_it_back() {
        let (s, log, gate) = screen(Setup::default());
        s.show(8, 8, None, false);
        wait_for(|| !log.cams().is_empty(), 2.0);
        {
            let _released = gate.release();
            assert!(log.cams()[0].closed.load(Ordering::SeqCst) && !s.running());
            s.show(8, 8, None, false);
            assert!(!s.running());
        }
        wait_for(|| log.cams().len() == 2, 2.0);
        s.stop();
    }

    // test_vcam.py::test_a_stopped_screen_stays_stopped_after_a_reload
    #[test]
    fn a_stopped_screen_stays_stopped_after_a_reload() {
        let (s, log, gate) = screen(Setup::default());
        s.show(8, 8, None, false);
        wait_for(|| !log.cams().is_empty(), 2.0);
        s.stop();
        drop(gate.release());
        assert!(!s.running());
        assert_eq!(log.cams().len(), 1);
    }

    // test_vcam.py::test_the_wait_screen_moves_to_a_camera_installed_while_it_runs
    #[test]
    fn the_wait_screen_moves_to_a_camera_installed_while_it_runs() {
        let outdated = Arc::new(AtomicBool::new(false));
        let pace = Pace { idle: Duration::from_millis(30), ..Pace::default() };
        let (s, log, _g) = screen(Setup { pace, outdated: outdated.clone(), ..Setup::default() });
        s.show(8, 8, None, false);
        wait_for(|| !log.cams().is_empty(), 2.0);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(log.attempts.lock().unwrap().len(), 1, "the same kind: kept");
        outdated.store(true, Ordering::SeqCst);
        wait_for(|| log.cams().len() >= 2, 2.0);
        assert!(log.cams()[0].closed.load(Ordering::SeqCst));
        s.stop();
    }

    // test_vcam.py::test_no_image_is_the_default_screen_at_the_camera_size
    #[test]
    fn no_image_is_the_default_screen_at_the_camera_size() {
        let frames = load_frames(None, 320, 180);
        assert_eq!(frames.len(), 1);
        assert_eq!((frames[0].0.width, frames[0].0.height), (320, 180));
        assert_eq!(frames[0].0.data.len(), 320 * 180 * 3);
        assert_eq!(frames[0].1, STILL_PERIOD);
        // The lens is lavender in the middle and the corners are the window's background
        let (k, (cx, cy), _) = screen_layout(320, 180);
        let mid = (cy as usize * 320 + (cx + 130.0 * k) as usize) * 3;
        assert!(frames[0].0.data[mid + 2] > 150, "{:?}", &frames[0].0.data[mid..mid + 3]);
        assert_eq!(&frames[0].0.data[..3], &BG);
    }

    // test_vcam.py::test_an_image_that_wont_load_falls_back_to_the_default
    #[test]
    fn an_image_that_wont_load_falls_back_to_the_default() {
        let dir = telescope_core::testing::scratch_dir("wait");
        let bad = dir.join("bad.png");
        std::fs::write(&bad, b"not a picture").unwrap();
        let frames = load_frames(Some(&bad.to_string_lossy()), 64, 36);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, default_screen(64, 36));
        let frames = load_frames(Some(&dir.join("missing.gif").to_string_lossy()), 64, 36);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, default_screen(64, 36));
    }

    // test_vcam.py::test_read_image_frames_is_empty_for_an_image_that_wont_load
    #[test]
    fn read_image_frames_is_empty_for_an_image_that_wont_load() {
        let dir = telescope_core::testing::scratch_dir("wait");
        let bad = dir.join("bad.png");
        std::fs::write(&bad, b"not an image").unwrap();
        assert!(read_image_frames(&bad.to_string_lossy(), 64, 36).is_empty());
        assert!(read_image_frames("/nonexistent/picture.png", 64, 36).is_empty());
    }

    // test_vcam.py::test_a_photo_over_qts_allocation_limit_still_loads
    #[test]
    fn a_big_photo_still_loads_fitted_to_the_camera() {
        let dir = telescope_core::testing::scratch_dir("wait");
        let path = dir.join("big.png");
        image::RgbImage::from_pixel(4000, 3000, image::Rgb([200, 10, 10])).save(&path).unwrap();
        let frames = read_image_frames(&path.to_string_lossy(), 64, 36);
        assert_eq!(frames.len(), 1);
        let mid = ((18 * 64 + 32) * 3) as usize;
        assert_eq!(&frames[0].0.data[mid..mid + 3], &[200, 10, 10]);
        // 4:3 in a 16:9 box leaves the background at the sides
        assert_eq!(&frames[0].0.data[..3], &SURFACE_SUNK);
    }

    /// A small JPEG whose header claims w x h.
    fn jpeg_claiming(w: u16, h: u16) -> Vec<u8> {
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut out).encode(&[90u8; 16 * 16 * 3], 16, 16, image::ExtendedColorType::Rgb8).unwrap();
        let at = out.windows(2).position(|p| p == [0xFF, 0xC0]).unwrap() + 5;
        out[at..at + 2].copy_from_slice(&h.to_be_bytes());
        out[at + 2..at + 4].copy_from_slice(&w.to_be_bytes());
        out
    }

    #[test]
    fn a_photo_claiming_a_size_nobody_could_hold_is_refused_before_decoding() {
        let dir = telescope_core::testing::scratch_dir("wait");
        let path = dir.join("claims.jpg");
        std::fs::write(&path, jpeg_claiming(30000, 30000)).unwrap();
        // Not unwrap_err: a picture that did load would be printed pixel by pixel
        let Err(why) = read_frames(&path.to_string_lossy(), 64, 36, FRAME_BUDGET, &|rgb| rgb) else { panic!("a 30000 x 30000 photo was decoded") };
        assert!(why.to_lowercase().contains("limit") || why.to_lowercase().contains("memory"), "{why}");
    }

    #[test]
    fn every_format_the_picker_offers_loads() {
        let dir = telescope_core::testing::scratch_dir("wait");
        // The picker lists the formats Qt reads in the Python app that this can read too
        for ext in ["bmp", "gif", "ico", "jpeg", "jpg", "pbm", "pgm", "png", "ppm", "tga", "webp"] {
            let path = dir.join(format!("pic.{ext}"));
            let img = image::RgbImage::from_pixel(32, 32, image::Rgb([200, 200, 200]));
            match ext {
                // These keep one channel
                "pbm" | "pgm" => image::DynamicImage::ImageRgb8(img).to_luma8().save(&path).unwrap(),
                // The icon decoder only takes the RGBA pictures real icon files hold
                "ico" => image::DynamicImage::ImageRgb8(img).to_rgba8().save(&path).unwrap(),
                _ => img.save(&path).unwrap(),
            }
            let frames = match read_frames(&path.to_string_lossy(), 64, 36, FRAME_BUDGET, &|rgb| rgb) {
                Ok(frames) => frames,
                Err(why) => panic!("{ext} didn't load: {why}"),
            };
            assert_eq!(frames.len(), 1, "{ext} gave {} pictures", frames.len());
            let mid = ((18 * 64 + 32) * 3) as usize;
            let got = frames[0].0.data[mid];
            assert!(got > 150, "{ext} came out as {got}");
            assert!(crate::wait_screen_plugin::PICKER_FORMATS.contains(&format!("*.{ext}")), "{ext} isn't in the picker");
        }
        assert!(crate::wait_screen_plugin::PICKER_FORMATS.contains("*.jfif"));
    }

    #[test]
    fn a_huge_photo_is_decoded_at_the_size_qt_would_be_asked_for() {
        // 12000 x 9000 on a 1920 x 1080 screen: 1920 x 1440, the screen's longest side on the longest side
        assert_eq!(reduced_decode_size(12000, 9000, 1920, QT_ALLOC_LIMIT), Some((1920, 1440)));
        assert_eq!(reduced_decode_size(9000, 12000, 1920, QT_ALLOC_LIMIT), Some((1440, 1920)));
        assert_eq!(reduced_decode_size(40000, 1000, 1920, QT_ALLOC_LIMIT), Some((1920, 48)));
        // 4000 x 3000 is 48 MB, under half the limit: decoded whole
        assert_eq!(reduced_decode_size(4000, 3000, 1920, QT_ALLOC_LIMIT), None);
        // Small enough for the screen already, however low the limit
        assert_eq!(reduced_decode_size(1920, 1080, 1920, 1000), None);
        assert_eq!(reduced_decode_size(5000, 4000, 1920, QT_ALLOC_LIMIT / 4), Some((1920, 1536)));
    }

    #[test]
    fn a_photo_over_the_limit_is_scaled_down_while_it_is_read() {
        let dir = telescope_core::testing::scratch_dir("wait");
        let path = dir.join("over.png");
        image::RgbImage::from_pixel(120, 60, image::Rgb([30, 60, 90])).save(&path).unwrap();
        let decoder = image::ImageReader::open(&path).unwrap().into_decoder().unwrap();
        // 120 x 60 x 4 is 28800 bytes, over half of a 20000 byte limit; the bound is 64 a side
        let img = decode_still(decoder, 64, 20_000).unwrap();
        assert_eq!((img.width(), img.height()), (64, 32));
        assert_eq!(img.to_rgb8().get_pixel(10, 10).0, [30, 60, 90]);
    }

    fn gif(path: &std::path::Path, frames: usize, delay_ms: u32) {
        let file = std::fs::File::create(path).unwrap();
        let mut enc = image::codecs::gif::GifEncoder::new(file);
        enc.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
        for i in 0..frames {
            let img = image::RgbaImage::from_pixel(16, 9, image::Rgba([(i * 40) as u8, 0, 0, 255]));
            let delay = image::Delay::from_numer_denom_ms(delay_ms, 1);
            enc.encode_frame(image::Frame::from_parts(img, 0, 0, delay)).unwrap();
        }
    }

    // 8x4, three frames: red 100 ms, blue 300 ms, green 50 ms (test_vcam.py's _GIF)
    const WAIT_GIF: &[u8] = b"\x47\x49\x46\x38\x39\x61\x08\x00\x04\x00\x81\x00\x00\xff\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x21\xff\x0b\x4e\x45\x54\x53\x43\x41\x50\x45\x32\x2e\x30\x03\x01\x00\x00\x00\x21\xf9\x04\x00\x0a\x00\x00\x00\x2c\x00\x00\x00\x00\x08\x00\x04\x00\x00\x08\x0c\x00\x01\x08\x1c\x48\xb0\xa0\xc1\x83\x04\x03\x02\x00\x21\xf9\x04\x01\x1e\x00\x01\x00\x2c\x00\x00\x00\x00\x08\x00\x04\x00\x81\x00\x00\xff\x00\x00\x00\x00\x00\x00\x00\x00\x00\x08\x0c\x00\x01\x08\x1c\x48\xb0\xa0\xc1\x83\x04\x03\x02\x00\x21\xf9\x04\x01\x05\x00\x01\x00\x2c\x00\x00\x00\x00\x08\x00\x04\x00\x81\x00\xff\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x08\x0c\x00\x01\x08\x1c\x48\xb0\xa0\xc1\x83\x04\x03\x02\x00\x3b";
    // 8x4, two frames: red 100 ms, blue 300 ms
    const WAIT_WEBP: &[u8] = b"\x52\x49\x46\x46\x84\x00\x00\x00\x57\x45\x42\x50\x56\x50\x38\x58\x0a\x00\x00\x00\x02\x00\x00\x00\x07\x00\x00\x03\x00\x00\x41\x4e\x49\x4d\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x41\x4e\x4d\x46\x28\x00\x00\x00\x00\x00\x00\x00\x00\x00\x07\x00\x00\x03\x00\x00\x64\x00\x00\x02\x56\x50\x38\x4c\x0f\x00\x00\x00\x2f\x07\xc0\x00\x00\x07\x10\xfd\x8f\xfe\x07\x22\xa2\xff\x01\x00\x41\x4e\x4d\x46\x28\x00\x00\x00\x00\x00\x00\x00\x00\x00\x07\x00\x00\x03\x00\x00\x2c\x01\x00\x00\x56\x50\x38\x4c\x0f\x00\x00\x00\x2f\x07\xc0\x00\x00\x07\x10\xd1\xff\xfe\x07\x22\xa2\xff\x01\x00";

    fn wait_gif() -> String {
        let path = telescope_core::testing::scratch_dir("wait").join("wait.gif");
        std::fs::write(&path, WAIT_GIF).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn pixel(picture: &Rgb, y: u32, x: u32) -> [u8; 3] {
        let at = ((y * picture.width + x) * 3) as usize;
        picture.data[at..at + 3].try_into().unwrap()
    }

    // test_vcam.py::test_an_animation_keeps_its_frames_and_timing_fitted_to_the_camera
    #[test]
    fn an_animation_keeps_its_frames_and_timing_fitted_to_the_camera() {
        let frames = load_frames(Some(&wait_gif()), 16, 16);
        let seconds: Vec<f64> = frames.iter().map(|f| (f.1 * 1000.0).round() / 1000.0).collect();
        assert_eq!(seconds, [0.1, 0.3, (MIN_GIF_PERIOD * 1000.0).round() / 1000.0]);
        let red = &frames[0].0;
        assert_eq!((red.width, red.height, red.data.len()), (16, 16, 16 * 16 * 3));
        assert_eq!(pixel(red, 8, 8), [255, 0, 0], "the 8x4 image scaled to fit, centred");
        assert_ne!(pixel(red, 0, 8), [255, 0, 0], "letterboxed above and below");
    }

    // Not in Python's tests, but Qt plays an animated WebP the same way
    #[test]
    fn an_animated_webp_keeps_its_frames_and_timing() {
        let path = telescope_core::testing::scratch_dir("wait").join("wait.webp");
        std::fs::write(&path, WAIT_WEBP).unwrap();
        let frames = load_frames(Some(&path.to_string_lossy()), 16, 16);
        assert_eq!(frames.iter().map(|f| (f.1 * 1000.0).round() / 1000.0).collect::<Vec<_>>(), [0.1, 0.3]);
        assert_eq!(pixel(&frames[0].0, 8, 8), [255, 0, 0]);
        assert!(pixel(&frames[1].0, 8, 8)[2] > 250);
    }

    // test_vcam.py::test_a_long_animation_is_thinned_to_the_memory_budget
    #[test]
    fn a_long_animation_is_thinned_to_the_memory_budget() {
        let path = wait_gif();
        // Room for two frames of three
        let frames = read_frames(&path, 16, 16, 16 * 16 * 3 * 2, &|rgb| rgb).unwrap();
        assert_eq!(frames.len(), 2);
        let total: f64 = frames.iter().map(|f| f.1).sum();
        assert_eq!((total * 1000.0).round(), ((0.1 + 0.3 + MIN_GIF_PERIOD) * 1000.0).round(), "same loop length");
    }

    // The budget counts what's kept, as vcam's len(frames) * frames[0].nbytes does: half the bytes fit twice the frames
    #[test]
    fn the_budget_counts_the_frames_as_they_are_kept() {
        let half = |rgb: Rgb| Rgb { data: rgb.data[..rgb.data.len() / 2].to_vec(), ..rgb };
        let frames = read_frames(&wait_gif(), 16, 16, 16 * 16 * 3 * 2, &half).unwrap();
        assert_eq!(frames.len(), 3);
    }

    // 41 frames at 1080p: Python keeps 11 as RGB and 21 as the I420 it sends on Linux, one over the budget by its own sums
    #[test]
    fn frames_for_the_camera_fit_the_budget_in_the_cameras_format() {
        let path = telescope_core::testing::scratch_dir("wait").join("long.gif");
        gif(&path, 41, 100);
        let path = path.to_string_lossy().into_owned();
        let loader: FrameLoader = Arc::new(|p, w, h, convert| Ok(load_frames_as(p, w, h, convert)));
        let job = ScreenJob {
            deps: Deps { open_camera: Arc::new(|_, _, _, _| Err("none".into())), loader, locked: Arc::new(|| None), filter_probe: None, i420: true, pace: Pace::default() },
            shown: Mutex::new(((1920, 1080), Some(path.clone()), false)), watched: AtomicBool::new(false), nudge: Nudge::default(),
        };
        let (fmt, frames) = job.prepare(1920, 1080).unwrap();
        assert_eq!((fmt, frames.len()), (PixelFormat::I420, 21));
        assert!((frames.iter().map(|f| f.1).sum::<f64>() - 4.1).abs() < 0.05, "the dropped frames' time goes to the ones kept");
        assert_eq!(read_image_frames(&path, 1920, 1080).len(), 11);
    }

    // test_vcam.py::test_convert_runs_on_each_frame_as_it_loads (frames are made in the camera's format as they're prepared)
    #[test]
    fn frames_are_prepared_in_the_cameras_format_with_their_timing() {
        let loader: Loader = Arc::new(|_p, w, h| {
            let pic = Rgb { width: w, height: h, data: vec![9; (w * h * 3) as usize] };
            Ok(vec![(pic.clone(), 0.1), (pic, 0.3)])
        });
        let (s, _log, _g) = screen(Setup { loader, ..Setup::default() });
        let (fmt, frames) = s.held.job().prepare(8, 4).unwrap();
        assert_eq!(fmt, PixelFormat::I420);
        assert_eq!(frames.iter().map(|f| f.1).collect::<Vec<_>>(), [0.1, 0.3]);
        assert!(frames.iter().all(|f| f.0.data.len() == 8 * 4 * 3 / 2));
    }

    #[test]
    fn the_name_goes_under_the_lens_only_where_it_fits() {
        assert!(screen_layout(1920, 1080).2);
        assert!(!screen_layout(80, 60).2);
        let (k, (cx, _), _) = screen_layout(1920, 1080);
        assert_eq!(cx, 960.0);
        assert!((k - (1080.0 * 0.82 / 654.0)).abs() < 1e-4);
    }

    /// Where the lettering is: the (first row, last row, first column, last column) of what differs from the lens alone, in the
    /// rows of the name and then in the rows of the note under it.
    fn lettering(width: u32, height: u32) -> [Option<(usize, usize, usize, usize)>; 2] {
        let full = default_screen(width, height);
        let (k, centre, _) = screen_layout(width, height);
        let mut pm = tiny_skia::Pixmap::new(width, height).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(BG[0], BG[1], BG[2], 255));
        paint_lens(&mut pm, centre, k);
        let top = centre.1 + SCREEN_NAME_TOP * k;
        let (name_from, name_to) = (top as i64 - 5, (top + 74.0 * k) as i64 - 5);
        let mut boxes: [Option<(usize, usize, usize, usize)>; 2] = [None, None];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let i = y * width as usize + x;
                let differs = (0..3).any(|c| (full.data[i * 3 + c] as i32 - pm.data()[i * 4 + c] as i32).abs() > 30);
                let band = if (name_from..name_to).contains(&(y as i64)) { 0 } else if y as f32 > top { 1 } else { continue };
                if differs {
                    let b = boxes[band].get_or_insert((y, y, x, x));
                    *b = (b.0.min(y), b.1.max(y), b.2.min(x), b.3.max(x));
                }
            }
        }
        boxes
    }

    // Measured by painting vcam._default_screen with Qt (Inter Display Bold for the name, Inter for the note): the name's size is
    // the em, so its capital T stands cap-height tall above a baseline `ascent` under the top of the text box
    #[test]
    fn the_lettering_sits_where_qt_puts_it() {
        if telescope_core::text::font_named(&["InterDisplay-Bold.ttf"]).is_none() || telescope_core::text::font_named(&["Inter-Regular.ttf"]).is_none() {
            eprintln!("Inter is not installed: the Qt comparison is skipped");
            return;
        }
        // (size, name rows, name columns, note rows, note columns)
        let qt = [
            ((1280, 720), (564, 610, 526, 753), (623, 641, 550, 728)),
            ((1920, 1080), (847, 917, 791, 1129), (934, 963, 828, 1089)),
        ];
        for ((w, h), name, note) in qt {
            let [got_name, got_note] = lettering(w, h);
            for (what, got, want) in [("name", got_name, name), ("note", got_note, note)] {
                let got = got.unwrap_or_else(|| panic!("no {what} at {w}x{h}"));
                // Qt rounds each glyph advance to whole pixels here (default hinting), which shifts the ends by a pixel or two
                let (close, near) = (|a: usize, b: usize| a.abs_diff(b) <= 1, |a: usize, b: usize| a.abs_diff(b) <= 3);
                assert!(
                    close(got.0, want.0) && close(got.1, want.1) && near(got.2, want.2) && near(got.3, want.3),
                    "{what} at {w}x{h}: rows {}..{} columns {}..{}, Qt has rows {}..{} columns {}..{}",
                    got.0, got.1, got.2, got.3, want.0, want.1, want.2, want.3
                );
            }
        }
    }
}
