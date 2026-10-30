//! What the Cinema tools judge and colour (plugins/cinema.py): false colour, focus peaking and zebras on the preview's copy of the
//! picture, and the maths of the level guide (how the phone's tilt looks in the shown picture, and the filter that keeps it calm).
//! The pixel work follows OpenCV and numpy to the last level, so the preview marks the same pixels the Python app does.

use crate::cpu::Rgb;
use crate::imgops::Gray;
use crate::scopes;
use std::sync::OnceLock;
use telescope_core::host::View;
use telescope_core::i18n::n_;

/// A scope is worked out this often.
pub const SCOPE_FPS: f32 = 12.0;
/// px, one black and one clear stripe.
pub const ZEBRA_PERIOD: usize = 8;
/// px, the widest the tools judge a frame at; a bigger preview gets their marks scaled up.
pub const WORK_WIDTH: usize = 1280;
/// Degrees: a tilt smaller than this shows as level.
pub const LEVEL_SNAP: f64 = 0.5;
/// Degrees: and it has to tilt more than this to stop showing as level, so it doesn't flicker.
pub const LEVEL_LEAVE: f64 = 1.0;
pub const PITCH_SNAP: f64 = 1.0;
pub const PITCH_LEAVE: f64 = 2.0;
/// How quickly the guide follows the phone; slower is calmer.
pub const LEVEL_SMOOTH_S: f64 = 0.3;
/// A reading older than this isn't drawn.
pub const LEVEL_STALE_S: f64 = 1.5;
/// The guide not painted for this long (preview hidden, window minimized): the sensors stop.
pub const LEVEL_UNSEEN_S: f64 = 2.0;

pub const SCOPES: [(&str, &str); 5] = [
    ("off", n_("Off")),
    ("histogram", n_("Histogram")),
    ("waveform", n_("Waveform")),
    ("parade", n_("RGB parade")),
    ("vectorscope", n_("Vectorscope")),
];

/// (key, name, R G B)
pub const PEAKING_COLOURS: [(&str, &str, [u8; 3]); 4] = [
    ("red", n_("Red"), [255, 40, 40]),
    ("green", n_("Green"), [60, 255, 60]),
    ("blue", n_("Blue"), [40, 120, 255]),
    ("white", n_("White"), [255, 255, 255]),
];

// Translators: framing guides drawn over the preview; "4 × 4" is a grid of four by four boxes
pub const GUIDES: [(&str, &str); 5] = [
    ("off", n_("Off")),
    ("thirds", n_("Thirds")),
    ("cross", n_("Centre cross")),
    ("4x4", n_("4 × 4")),
    ("golden", n_("Golden ratio")),
];

/// (key, name, share of the preview's width a scope takes)
pub const SIZES: [(&str, &str, f64); 3] = [("small", n_("Small"), 0.26), ("medium", n_("Medium"), 0.36), ("large", n_("Large"), 0.5)];

/// Where each guide's lines go across the width and down the height, as shares of the picture.
pub fn guide_lines(guide: &str) -> Option<Vec<f64>> {
    let phi = (1.0 + 5f64.sqrt()) / 2.0;
    Some(match guide {
        "thirds" => vec![1.0 / 3.0, 2.0 / 3.0],
        "cross" => vec![0.5],
        "4x4" => vec![0.25, 0.5, 0.75],
        "golden" => vec![1.0 - 1.0 / phi, 1.0 / phi],
        _ => return None,
    })
}

/// Why the level guide can't show, by the sensors' reasons.
pub const LEVEL_PROBLEMS: [(&str, &str); 5] = [
    ("not_phone", n_("The level needs a phone's motion sensors.")),
    ("old_phone", n_("Update Telescope on the phone to use the level.")),
    ("none", n_("This phone has no motion sensor for the level.")),
    ("camera", n_("The level works with the phone's own cameras, not an external one.")),
    ("failed", n_("Can't reach the phone's motion sensors.")),
];

/// False colour: (up to this luma %, R G B, name), the classic exposure scheme; None keeps the picture in grey there.
// Translators: the false colour legend on the preview, as short as you can
pub const FALSE_COLOURS: [(u32, Option<[u8; 3]>, &str); 9] = [
    (2, Some([120, 40, 160]), n_("Crushed")),
    (10, Some([20, 80, 200]), n_("Too dark")),
    (38, None, ""),
    (45, Some([60, 180, 60]), n_("Middle grey")),
    (52, None, ""),
    (58, Some([240, 130, 170]), n_("Skin")),
    (95, None, ""),
    (99, Some([240, 220, 40]), n_("Nearly clipped")),
    (100, Some([230, 40, 40]), n_("Clipped")),
];

fn false_colour_lut() -> [[u8; 3]; 256] {
    let mut lut = [[0u8; 3]; 256];
    let mut lo = 0usize;
    for (upper, colour, _) in FALSE_COLOURS {
        let hi = 255.min((upper as f64 / 100.0 * 255.0).round_ties_even() as usize);
        for level in lo..=hi {
            // the grey a little flatter
            lut[level] = colour.unwrap_or_else(|| [(level as f64 * 0.8 + 20.0) as u8; 3]);
        }
        lo = hi + 1;
    }
    lut
}

/// What each brightness is coloured in false colour, R G B.
pub fn false_lut() -> &'static [[u8; 3]; 256] {
    static LUT: OnceLock<[[u8; 3]; 256]> = OnceLock::new();
    LUT.get_or_init(false_colour_lut)
}

/// The frame with each pixel coloured by how bright it is (FALSE_COLOURS); luma when it's worked out already.
pub fn false_colour(frame: &Rgb, luma: Option<&Gray>) -> Rgb {
    let own;
    let y = match luma {
        Some(y) => y,
        None => {
            own = scopes::luma(frame);
            &own
        }
    };
    let lut = false_lut();
    let mut out = Rgb::new(frame.w, frame.h);
    for (o, &v) in out.data.chunks_exact_mut(3).zip(&y.data) {
        o.copy_from_slice(&lut[v as usize]);
    }
    out
}

/// How strong an edge has to be to light up, for a sensitivity of 1 to 10.
pub fn peaking_threshold(sensitivity: i64) -> i64 {
    200 - sensitivity.clamp(1, 10) * 15
}

/// cv2.Sobel(grey, CV_16S, dx, dy, ksize=3) turned to cv2.convertScaleAbs: |gradient| held to 255. The edges mirror (BORDER_REFLECT_101).
fn sobel_abs(grey: &Gray, dx: bool) -> Vec<u8> {
    let (w, h) = (grey.w as isize, grey.h as isize);
    let at = |x: isize, y: isize| -> i32 {
        let r = |i: isize, n: isize| if n == 1 { 0 } else if i < 0 { -i } else if i >= n { 2 * (n - 1) - i } else { i };
        grey.data[(r(y, h) * w + r(x, w)) as usize] as i32
    };
    let mut out = vec![0u8; grey.data.len()];
    for y in 0..h {
        for x in 0..w {
            let g = if dx {
                (at(x + 1, y - 1) - at(x - 1, y - 1)) + 2 * (at(x + 1, y) - at(x - 1, y)) + (at(x + 1, y + 1) - at(x - 1, y + 1))
            } else {
                (at(x - 1, y + 1) - at(x - 1, y - 1)) + 2 * (at(x, y + 1) - at(x, y - 1)) + (at(x + 1, y + 1) - at(x + 1, y - 1))
            };
            out[(y * w + x) as usize] = g.unsigned_abs().min(255) as u8;
        }
    }
    out
}

/// Where the frame has sharp edges (what's in focus).
pub fn peaking_mask(frame: &Rgb, sensitivity: i64) -> Vec<bool> {
    let grey = frame.gray();
    let (gx, gy) = (sobel_abs(&grey, true), sobel_abs(&grey, false));
    let threshold = peaking_threshold(sensitivity) / 2;
    gx.iter().zip(&gy).map(|(&x, &y)| ((x as f32 * 0.5 + y as f32 * 0.5).round_ties_even() as i64) > threshold).collect()
}

/// Whether the zebras' diagonal stripes cover a pixel; phase moves them along.
pub fn stripe(x: usize, y: usize, phase: usize) -> bool {
    ((x + phase % ZEBRA_PERIOD + y) / (ZEBRA_PERIOD / 2)) % 2 == 0
}

/// The brightness (0..255) that zebras start at, for a level in %.
pub fn zebra_from(level: i64) -> u8 {
    (level as f64 / 100.0 * 255.0).round_ties_even().clamp(0.0, 255.0) as u8
}

/// Diagonal stripes where luma is at level % or more; phase moves them along.
pub fn zebra_mask(frame: &Rgb, level: i64, phase: usize, luma: Option<&Gray>) -> Vec<bool> {
    let own;
    let y = match luma {
        Some(y) => y,
        None => {
            own = scopes::luma(frame);
            &own
        }
    };
    let from = zebra_from(level);
    (0..frame.h).flat_map(|row| (0..frame.w).map(move |col| (col, row))).zip(&y.data).map(|((x, row), &v)| v >= from && stripe(x, row, phase)).collect()
}

/// What the tools draw, swapped whole when anything changes (the stream and analysis threads read it).
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub peaking: bool,
    pub zebras: bool,
    pub false_colour: bool,
    pub scope: &'static str,
    pub peaking_colour: &'static str,
    pub peaking_level: i64,
    pub zebra_level: i64,
    pub scope_size: &'static str,
    pub level: bool,
    pub guides: &'static str,
}

impl Default for Look {
    fn default() -> Look {
        Look {
            peaking: false,
            zebras: false,
            false_colour: false,
            scope: "off",
            peaking_colour: "red",
            peaking_level: 5,
            zebra_level: 95,
            scope_size: "medium",
            level: false,
            guides: "off",
        }
    }
}

impl Look {
    pub fn any(&self) -> bool {
        self.peaking || self.zebras || self.false_colour || self.scope != "off" || self.level || self.guides != "off"
    }

    /// Whether any of the pixel tools is on.
    pub fn pixels(&self) -> bool {
        self.zebras || self.peaking || self.false_colour
    }
}

fn peaking_rgb(key: &str) -> [u8; 3] {
    PEAKING_COLOURS.iter().find(|(k, ..)| *k == key).map_or(PEAKING_COLOURS[0].2, |c| c.2)
}

/// A mask grown back to the frame's size when it was judged on a smaller copy: scaled smoothly and cut half way, so edges don't
/// come out as steps.
fn full(mask: &[bool], from: (usize, usize), to: (usize, usize)) -> Vec<bool> {
    if from == to {
        return mask.to_vec();
    }
    let grey = Gray { w: from.0, h: from.1, data: mask.iter().map(|&m| if m { 255 } else { 0 }).collect() };
    grey.linear_exact(to.0, to.1).data.iter().map(|&v| v > 127).collect()
}

/// The tools on a frame: judged on a copy at most WORK_WIDTH wide, so a full screen pop-out costs about what the card does. Every
/// tool judges the picture as it came in, then they all paint: one tool's paint isn't another's edge. `phase` is where the stripes
/// are, moved on by one each time the zebras are on.
pub fn tint(look: &Look, frame: &mut Rgb, phase: &mut usize) {
    if !look.pixels() {
        return;
    }
    let (w, h) = (frame.w, frame.h);
    let small = scopes::shrink(frame, WORK_WIDTH);
    let scaled = scopes::shrunk_size(w, h, WORK_WIDTH).is_some();
    let size = (small.w, small.h);
    let luma = (look.false_colour || look.zebras).then(|| scopes::luma(&small));
    let mut zebras = None;
    let mut peaks = None;
    if look.zebras {
        *phase = (*phase + 1) % ZEBRA_PERIOD;
        let from = zebra_from(look.zebra_level);
        let bright: Vec<bool> = luma.as_ref().unwrap().data.iter().map(|&v| v >= from).collect();
        let bright = full(&bright, size, (w, h));
        // The stripes are drawn at the frame's own size, so they stay crisp however it was judged
        zebras = Some(bright.iter().enumerate().map(|(i, &b)| b && stripe(i % w, i / w, *phase)).collect::<Vec<bool>>());
    }
    if look.peaking {
        peaks = Some(full(&peaking_mask(&small, look.peaking_level), size, (w, h)));
    }
    if look.false_colour {
        let coloured = false_colour(&small, luma.as_ref());
        *frame = if scaled { coloured.linear_exact(w, h) } else { coloured };
    }
    if let Some(mask) = zebras {
        paint(frame, &mask, [0, 0, 0]);
    }
    if let Some(mask) = peaks {
        paint(frame, &mask, peaking_rgb(look.peaking_colour));
    }
}

fn paint(frame: &mut Rgb, mask: &[bool], colour: [u8; 3]) {
    for (p, &m) in frame.data.chunks_exact_mut(3).zip(mask) {
        if m {
            p.copy_from_slice(&colour);
        }
    }
}

// ── Level guide ──────────────────────────────────────────────────────────────

/// angle as -180 up to 180.
pub fn wrap_degrees(angle: f64) -> f64 {
    (angle + 180.0).rem_euclid(360.0) - 180.0
}

/// The phone's roll (degrees the horizon is turned clockwise in its picture) as the preview shows it, after the view's flip and then
/// its rotation. A mirror turns a clockwise tilt anticlockwise.
pub fn shown_roll(roll: f64, view: &View) -> f64 {
    let mirrored = view.flip_h != view.flip_v;
    let turn = match view.rotation {
        Some(90) => 90.0,
        Some(180) => 180.0,
        Some(270) => -90.0,
        _ => 0.0,
    };
    wrap_degrees((if mirrored { -roll } else { roll }) + turn)
}

/// What the level guide draws: the horizon's tilt as shown and the camera's pitch, both smoothed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    /// None while the camera points nearly straight up or down.
    pub roll: Option<f64>,
    pub pitch: f64,
    pub roll_level: bool,
    pub pitch_level: bool,
}

/// What the guide shows of a level, to the tenth of a degree.
pub fn drawn(level: &Level) -> (Option<f64>, f64, bool, bool) {
    // Python's round(x, 1)
    let tenth = |v: f64| (v * 10.0).round_ties_even() / 10.0;
    (level.roll.map(tenth), tenth(level.pitch), level.roll_level, level.pitch_level)
}

/// Smooths the readings so the guide stays calm, and decides when they count as level with a margin either way, so it doesn't flicker
/// at the edge. Roll comes in as shown (shown_roll).
#[derive(Clone, Debug)]
pub struct LevelFilter {
    pub smooth_s: f64,
    roll: Option<f64>,
    pitch: Option<f64>,
    at: Option<f64>,
    roll_level: bool,
    pitch_level: bool,
}

impl LevelFilter {
    pub fn new(smooth_s: f64) -> LevelFilter {
        LevelFilter { smooth_s, roll: None, pitch: None, at: None, roll_level: false, pitch_level: false }
    }

    pub fn reset(&mut self) {
        *self = LevelFilter::new(self.smooth_s);
    }

    pub fn update(&mut self, roll: Option<f64>, pitch: Option<f64>, now: f64) -> Option<Level> {
        let Some(pitch) = pitch else {
            self.reset();
            return None;
        };
        let dt = self.at.map(|at| now - at);
        // Weighted by the time since the last reading, so a phone sending faster or slower moves it the same
        let a = match dt {
            Some(dt) if dt <= 1.0 => 1.0 - (-dt.max(0.0) / self.smooth_s).exp(),
            _ => 1.0,
        };
        self.roll = match (roll, self.roll, dt) {
            (None, ..) => None,
            (Some(r), None, _) | (Some(r), _, None) => Some(r),
            (Some(r), Some(old), Some(_)) => Some(wrap_degrees(old + a * wrap_degrees(r - old))),
        };
        self.pitch = Some(match (self.pitch, dt) {
            (Some(old), Some(_)) => old + a * (pitch - old),
            _ => pitch,
        });
        self.at = Some(now);
        self.roll_level = self.roll.is_some_and(|r| r.abs() < if self.roll_level { LEVEL_LEAVE } else { LEVEL_SNAP });
        let p = self.pitch.unwrap_or(0.0);
        self.pitch_level = p.abs() < if self.pitch_level { PITCH_LEAVE } else { PITCH_SNAP };
        Some(Level {
            roll: if self.roll_level { Some(0.0) } else { self.roll },
            pitch: if self.pitch_level { 0.0 } else { p },
            roll_level: self.roll_level,
            pitch_level: self.pitch_level,
        })
    }
}

impl Default for LevelFilter {
    fn default() -> LevelFilter {
        LevelFilter::new(LEVEL_SMOOTH_S)
    }
}

#[cfg(test)]
#[path = "cinema_tools_tests.rs"]
mod tests;
