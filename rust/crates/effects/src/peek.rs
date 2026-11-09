//! The pictures the held bubbles show (the kit's `Peek`): a photo of someone standing for the Framing bubble, a drawn sample for the blur's, and where a framing window sits in a picture.
//! The bubble crops the picture itself, so what is made here is the photo decoded once and a live frame shrunk, only while a bubble is up.

use crate::cpu::Rgb;
use crate::imgops::Gray;
use std::sync::OnceLock;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::pose::Person;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// The bubble's picture is 176 x 99 logical pixels: this is twice that, so a sharp screen has something to show.
pub const WIDTH: usize = 352;
pub const HEIGHT: usize = 198;
/// A live picture in a bubble is made this often (8 a second), and only while the bubble is up.
pub const PICTURE_MS: u64 = 125;
/// The blur's sample is drawn at this size, close to what a stream is, since the blur works on an eighth of the picture and a person
/// has to be more than a few of those pixels wide to blur the way a real one does. It is made once, the first time a bubble needs it.
pub const BLUR_WIDTH: usize = 960;
pub const BLUR_HEIGHT: usize = 540;
/// The Framing bubble's photo is twice the bubble's size, so a crop of it is still sharp on a sharp screen.
pub const SCENE_WIDTH: usize = 2 * WIDTH;
pub const SCENE_HEIGHT: usize = 2 * HEIGHT;
/// The shape of the Framing photo and of the blur's drawn sample.
pub const SAMPLE_ASPECT: f64 = WIDTH as f64 / HEIGHT as f64;

fn mix(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn bytes(c: [f64; 3]) -> [u8; 3] {
    c.map(|v| v.clamp(0.0, 255.0) as u8)
}

/// Where a stand-in person is, as fractions of the picture: the head's middle and radius (in picture heights), the shoulders' line and
/// half their width, the hips and the ankles.
struct Figure {
    head: (f64, f64),
    head_r: f64,
    shoulders_y: f64,
    shoulders_half: f64,
    hips_y: f64,
    hips_half: f64,
    ankles_y: f64,
    ankles_half: f64,
}

/// Close to the camera as on a call, which is what the blur sample shows: a head and shoulders that fill a good part of the picture.
const PORTRAIT: Figure = Figure { head: (0.5, 0.34), head_r: 0.13, shoulders_y: 0.58, shoulders_half: 0.22, hips_y: 1.4, hips_half: 0.2, ankles_y: 2.0, ankles_half: 0.12 };

/// A scene with something behind a person to blur, and the person's mask: a wall with shelves and things standing on them, someone in front.
fn draw_scene(f: &Figure, w: usize, h: usize) -> (Rgb, Gray) {
    let mut rgb = Rgb::new(w, h);
    let mut mask = Gray::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = ((x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64);
            // The wall, a little darker toward the floor, with a shelf board every fifth of the height and things standing on them
            let mut c = mix([196.0, 178.0, 150.0], [128.0, 112.0, 96.0], v);
            let (row, shelf) = ((v * 5.0).floor(), (v * 5.0).fract());
            if shelf < 0.06 {
                c = [92.0, 62.0, 40.0];
            } else {
                let across = u * 14.0 + row * 3.7;
                let cell = across.floor();
                let tall = 0.25 + 0.5 * ((cell * 12.9898).sin() * 43758.5453).fract().abs();
                let inside = across.fract();
                if (1.0 - shelf) < tall && inside > 0.12 && inside < 0.88 {
                    let hue = ((cell * 78.233).sin() * 12345.678).fract().abs();
                    c = mix([60.0 + 150.0 * hue, 70.0 + 90.0 * (1.0 - hue), 90.0 + 120.0 * (hue * 3.0).fract()], [30.0, 30.0, 40.0], 0.25 * inside);
                }
            }
            // The person: a head, a neck, a shirt from the shoulders to the hips and two legs down to the feet
            let (dx, dy) = ((u - f.head.0).abs(), v - f.head.1);
            let along = |from: (f64, f64), to: (f64, f64)| from.1 + (to.1 - from.1) * ((v - from.0) / (to.0 - from.0)).clamp(0.0, 1.0);
            let in_head = (dx * SAMPLE_ASPECT).hypot(dy) < f.head_r;
            let in_neck = dx * SAMPLE_ASPECT <= f.head_r * 0.45 && v > f.head.1 && v < f.shoulders_y;
            // Rounded shoulders: narrow at the top of the shirt, full a little way down
            let round = 0.55 + 0.45 * ((v - (f.shoulders_y - 0.015)) / 0.1).clamp(0.0, 1.0).sqrt();
            let shirt_half = along((f.shoulders_y, f.shoulders_half), (f.hips_y, f.hips_half + 0.012)) * round;
            let in_shirt = v >= f.shoulders_y - 0.015 && v < f.hips_y && dx < shirt_half;
            let leg_centre = along((f.hips_y, f.hips_half * 0.55), (f.ankles_y, f.ankles_half));
            let in_legs = v >= f.hips_y && v < f.ankles_y + 0.05 && (dx - leg_centre).abs() < 0.017 + 0.004 * (1.0 - (v - f.hips_y) / (f.ankles_y - f.hips_y)).max(0.0);
            if in_head || in_neck || in_shirt || in_legs {
                c = if in_head || in_neck {
                    [224.0, 172.0, 140.0]
                } else if in_shirt {
                    [52.0, 78.0, 140.0]
                } else {
                    [38.0, 40.0, 52.0]
                };
                mask.data[y * w + x] = 255;
            }
            rgb.set(x, y, bytes(c));
        }
    }
    // A model's matte is soft at the edges, and the blur's own maths expects that
    (rgb, mask.blur(1.2))
}

/// Someone standing a few steps back (704 x 396, with some ground under the feet), so every Framing level from head and shoulders to the whole body has something different to show.
const SCENE_JPEG: &[u8] = include_bytes!("../assets/framing-scene.jpg");

fn scene() -> &'static Rgb {
    static SCENE: OnceLock<Rgb> = OnceLock::new();
    SCENE.get_or_init(|| {
        let mut decoder = JpegDecoder::new_with_options(ZCursor::new(SCENE_JPEG), DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB));
        let data = decoder.decode().expect("the bundled framing photo decodes");
        let (w, h) = decoder.dimensions().expect("a decoded photo has a size");
        Rgb { w, h, data }
    })
}

fn portrait() -> &'static (Rgb, Gray) {
    static SCENE: OnceLock<(Rgb, Gray)> = OnceLock::new();
    SCENE.get_or_init(|| draw_scene(&PORTRAIT, BLUR_WIDTH, BLUR_HEIGHT))
}

/// The person in the Framing photo as the app's own pose model finds them in it, as fractions of the picture: where the framing code looks for them.
const PHOTO_POINTS: [(&str, f64, f64); 17] = [
    ("nose", 0.5011, 0.0890), ("left_eye", 0.5080, 0.0744), ("right_eye", 0.4914, 0.0776), ("left_ear", 0.5143, 0.0902), ("right_ear", 0.4836, 0.0943),
    ("left_shoulder", 0.5490, 0.2045), ("right_shoulder", 0.4531, 0.2106), ("left_elbow", 0.5638, 0.3510), ("right_elbow", 0.4398, 0.3564),
    ("left_wrist", 0.5667, 0.4649), ("right_wrist", 0.4384, 0.4624), ("left_hip", 0.5298, 0.4703), ("right_hip", 0.4720, 0.4696),
    ("left_knee", 0.5265, 0.6787), ("right_knee", 0.4694, 0.6762), ("left_ankle", 0.5243, 0.8700), ("right_ankle", 0.4720, 0.8689),
];

/// The person the Framing photo shows, in the picture as shown.
pub fn sample_person() -> Person {
    Person::new(PHOTO_POINTS.iter().map(|&(name, x, y)| (name, x, y, 1.0)))
}

/// The blur's sample at its own size (BLUR_WIDTH x BLUR_HEIGHT): the picture and the person's mask, someone close to the camera.
pub fn blur_scene() -> &'static (Rgb, Gray) {
    portrait()
}

/// The blur's sample at the bubble's size (WIDTH x HEIGHT), shrunk once.
pub fn blur_scene_small() -> &'static (Rgb, Gray) {
    static SMALL: OnceLock<(Rgb, Gray)> = OnceLock::new();
    SMALL.get_or_init(|| {
        let (rgb, mask) = portrait();
        (rgb.shrink(WIDTH, HEIGHT), mask.shrink(WIDTH, HEIGHT))
    })
}

/// The same sample at w x h.
pub fn sample_scene(w: usize, h: usize) -> (Rgb, Gray) {
    let (rgb, mask) = portrait();
    if (w, h) == (BLUR_WIDTH, BLUR_HEIGHT) {
        (rgb.clone(), mask.clone())
    } else {
        (rgb.shrink(w, h), mask.shrink(w, h))
    }
}

/// A picture's pixels as a frame the card can show.
pub fn rgba(rgb: &Rgb) -> Frame {
    let mut data = Vec::with_capacity(rgb.w * rgb.h * 4);
    for p in rgb.data.as_chunks::<3>().0 {
        data.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    Frame::new(rgb.w as u32, rgb.h as u32, PixelFormat::Rgba, data)
}

/// Where a window the framing wants (centre and zoom as fractions of the picture as shown) sits in a picture that already shows `view`,
/// the window the view has now: the same shape, never wider than what the picture holds, slid in to stay inside it.
pub fn window_in(view: (f64, f64, f64), want: (f64, f64, f64)) -> (f64, f64, f64) {
    let (vx, vy, vz) = (view.0, view.1, view.2.max(1.0));
    let zoom = (want.2 / vz).max(1.0);
    let half = 0.5 / zoom;
    let u = ((want.0 - (vx - 0.5 / vz)) * vz).clamp(half, 1.0 - half);
    let v = ((want.1 - (vy - 0.5 / vz)) * vz).clamp(half, 1.0 - half);
    (u, v, zoom)
}

/// Where a window (centre and zoom as fractions of a picture) sits in the picture, as (x, y, width, height) fractions of it, slid in to stay
/// inside. The bubble hands this to the kit's `Image` as its source clip, so the crop is drawn by the renderer and nothing is built here.
pub fn clip(window: (f64, f64, f64)) -> [f64; 4] {
    let size = 1.0 / window.2.max(1.0);
    let half = size / 2.0;
    let (cx, cy) = (window.0.clamp(half, 1.0 - half), window.1.clamp(half, 1.0 - half));
    [cx - half, cy - half, size, size]
}

/// The Framing photo as a picture the card can show, decoded once: the bubble crops it with `clip` and never needs another.
pub fn scene_picture() -> &'static Frame {
    static PICTURE: OnceLock<Frame> = OnceLock::new();
    PICTURE.get_or_init(|| rgba(scene()))
}

/// The largest a live frame is handed to the bubble: twice the box, so a crop of it is still sharp on a sharp screen.
const LIVE_WIDTH: usize = 2 * WIDTH;
const LIVE_HEIGHT: usize = 2 * HEIGHT;

/// A stream's frame shrunk to what the bubble needs (never enlarged), or None when its pixels can't be read.
pub fn live_picture(frame: &Frame) -> Option<Frame> {
    let rgb = Rgb::from_frame(frame)?;
    let scale = (LIVE_WIDTH as f64 / rgb.w as f64).min(LIVE_HEIGHT as f64 / rgb.h as f64).min(1.0);
    if scale >= 1.0 {
        return Some(rgba(&rgb));
    }
    let (w, h) = (((rgb.w as f64 * scale).round() as usize).max(1), ((rgb.h as f64 * scale).round() as usize).max(1));
    Some(rgba(&rgb.shrink(w, h)))
}

#[cfg(test)]
#[path = "peek_tests.rs"]
mod tests;
