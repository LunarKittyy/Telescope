//! Matching two different phones for the 3D plugin: the right phone's ISO nudged until its picture is as bright as the left one's, a tone curve per
//! colour channel that makes the rest of the right picture look like the left, and a starting Size from the two lenses. Every step is a plain
//! function or a small state machine over numbers, so none of it needs a phone, a plugin or a window to be tested.

use crate::cpu::Rgb;
use crate::stereo::Eye;
use serde_json::{json, Value};

/// How close the two pictures' mean brightness has to get, as a part of the left one's.
pub const LUMA_TOLERANCE: f64 = 0.03;
/// How many times the ISO is changed in all before Match settles for what it got.
pub const MAX_ISO_STEPS: u32 = 6;
/// The mean brightness (0 to 255) a phone is nudged to when it is left on its own, a bit under the middle.
pub const TARGET_LUMA: f64 = 118.0;
/// How long a change takes to show in the pictures (the phone applies it, encodes it, and the pairing hands it on), nanoseconds.
pub const SETTLE_NS: u64 = 700_000_000;
/// The ISO a phone on automatic starts from, since automatic doesn't say what it chose.
pub const START_ISO: i64 = 400;
/// The shutter a phone on automatic starts from, 1/60 s in nanoseconds.
pub const DEFAULT_SHUTTER_NS: i64 = 16_666_667;
/// A shutter stays under this part of a frame's time, so it never asks for more than the frame rate allows.
const SHUTTER_OF_FRAME: f64 = 0.9;
/// What the Size slider reaches, in percent.
pub const SIZE_MIN: f64 = 80.0;
pub const SIZE_MAX: f64 = 125.0;
/// The width of the 35 mm frame the phone's `focalMm` is equivalent to, millimetres.
const FULL_FRAME_WIDTH_MM: f64 = 36.0;
/// How far a step may change the exposure, as a factor either way, so the brightness settles instead of swinging.
const STEP_LIMIT: f64 = 2.0;
/// The exponent between a picture's brightness as stored and the light that made it.
const GAMMA: f64 = 2.2;

// ── The pictures ──────────────────────────────────────────────────────────

/// How many pixels of each value a picture has, per colour channel (red, green, blue).
pub type Hist = [[u32; 256]; 3];

/// What Match reads from the middle of one eye's picture.
#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    /// The mean brightness, 0 to 255.
    pub luma: f64,
    pub hist: Hist,
}

/// The mean brightness and the histograms of the middle half (in each direction) of the picture, which is what both lenses see the most alike.
/// Large pictures are sampled on a grid so it costs next to nothing. None for a picture too small to have a middle.
pub fn centre_stats(rgb: &Rgb) -> Option<Stats> {
    let (x0, x1, y0, y1) = (rgb.w / 4, rgb.w - rgb.w / 4, rgb.h / 4, rgb.h - rgb.h / 4);
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let step = (rgb.w.min(rgb.h) / 128).max(1);
    let mut hist: Hist = [[0; 256]; 3];
    let (mut sum, mut n) = (0.0, 0u64);
    for y in (y0..y1).step_by(step) {
        for x in (x0..x1).step_by(step) {
            let [r, g, b] = rgb.at(x, y);
            hist[0][r as usize] += 1;
            hist[1][g as usize] += 1;
            hist[2][b as usize] += 1;
            sum += 0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64;
            n += 1;
        }
    }
    Some(Stats { luma: sum / n as f64, hist })
}

/// A tone curve for each colour channel: the value that comes out for each value that goes in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lut(pub [[u8; 256]; 3]);

impl Lut {
    pub fn identity() -> Lut {
        let line: [u8; 256] = std::array::from_fn(|i| i as u8);
        Lut([line; 3])
    }

    pub fn is_identity(&self) -> bool {
        *self == Lut::identity()
    }

    /// The picture through the curves, in place.
    pub fn apply(&self, rgb: &mut Rgb) {
        for px in rgb.data.as_chunks_mut::<3>().0 {
            px[0] = self.0[0][px[0] as usize];
            px[1] = self.0[1][px[1] as usize];
            px[2] = self.0[2][px[2] as usize];
        }
    }
}

/// How far the curve is smoothed: each entry is the mean of the ones this many either side.
const SMOOTH_RADIUS: usize = 4;
/// The share of pixels at each end of a histogram that only get the curve's straight run to black and white.
const TAIL: f64 = 0.005;

/// One channel's curve that makes a picture with histogram `from` look like one with histogram `to` (histogram matching): each value goes to the
/// value of `to` that has the same share of pixels below it. The result is smoothed, so a noisy or gappy histogram doesn't make steps, and kept
/// from ever going down. An empty histogram on either side gives no change.
fn fit_channel(from: &[u32; 256], to: &[u32; 256]) -> [u8; 256] {
    let (total_from, total_to) = (from.iter().map(|c| *c as u64).sum::<u64>(), to.iter().map(|c| *c as u64).sum::<u64>());
    if total_from == 0 || total_to == 0 {
        return std::array::from_fn(|i| i as u8);
    }
    // the share of pixels below the middle of each value, so a value that most pixels share lands on the middle of what they map to
    let mut cdf_to = [0.0f64; 256];
    let mut acc = 0u64;
    for (i, c) in to.iter().enumerate() {
        cdf_to[i] = (acc + *c as u64) as f64 / total_to as f64;
        acc += *c as u64;
    }
    let mut raw = [0.0f32; 256];
    let (mut below, mut at) = (0u64, 0usize);
    // the values most of the pixels are between, since the middle of the picture may have no black or white at all
    let (mut lo, mut hi) = (0usize, 255usize);
    for v in 0..256 {
        if (below as f64) < TAIL * total_from as f64 {
            lo = v;
        }
        if (below as f64) < (1.0 - TAIL) * total_from as f64 {
            hi = v;
        }
        let mid = (below as f64 + from[v] as f64 / 2.0) / total_from as f64;
        below += from[v] as u64;
        while at < 255 && cdf_to[at] < mid {
            at += 1;
        }
        raw[v] = at as f32;
    }
    // outside them the curve runs on to the ends of the range instead of clipping: black stays black and white stays white
    for v in 0..lo {
        raw[v] = raw[lo] * v as f32 / lo as f32;
    }
    for v in hi + 1..256 {
        raw[v] = raw[hi] + (255.0 - raw[hi]) * (v - hi) as f32 / (255 - hi) as f32;
    }
    let mut out = [0u8; 256];
    let mut floor = 0.0f32;
    for v in 0..256usize {
        // narrower towards the ends, so black and white stay where they are
        let radius = SMOOTH_RADIUS.min(v).min(255 - v);
        let (lo, hi) = (v - radius, v + radius);
        let mean = raw[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32;
        floor = floor.max(mean);
        out[v] = floor.round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// The curves that make the picture `from` (the right eye's middle) look like `to` (the left eye's), one for each channel.
pub fn fit_lut(from: &Hist, to: &Hist) -> Lut {
    Lut(std::array::from_fn(|c| fit_channel(&from[c], &to[c])))
}

// ── The phones ────────────────────────────────────────────────────────────

/// What came of nudging the ISO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IsoStep {
    /// Close enough.
    Hold,
    /// Set the ISO to this.
    Set(i64),
    /// The ISO is at the end of what the lens takes, and it still isn't close.
    Limit,
}

/// The next ISO for a phone at `current` whose picture measures `measured` (mean brightness) when it should measure `target`, within the lens's
/// `limits`. Brightness is stored with a gamma, so the light is changed by the ratio to the power of 2.2, and by no more than double or half at a
/// time, which settles in a few steps without swinging.
pub fn iso_step(current: i64, measured: f64, target: f64, (min, max): (i64, i64)) -> IsoStep {
    if target > 0.0 && (measured - target).abs() <= LUMA_TOLERANCE * target {
        return IsoStep::Hold;
    }
    let factor = if measured < 1.0 { STEP_LIMIT } else { (target / measured).powf(GAMMA).clamp(1.0 / STEP_LIMIT, STEP_LIMIT) };
    let next = ((current as f64 * factor).round() as i64).clamp(min.min(max), max.max(min));
    if next == current { IsoStep::Limit } else { IsoStep::Set(next) }
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The two pictures are as bright as each other, within the tolerance.
    Matched,
    /// An ISO ran out of room or the steps ran out, so the brightness is as close as it got.
    Limited,
}

/// What a run wants done after a look at the pictures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Nothing yet: the last change hasn't shown in the pictures.
    Wait,
    /// Set this eye's phone to this ISO.
    SetIso { eye: Eye, iso: i64 },
    /// The brightness is done: fit the colour curves from the pictures just measured.
    Fit(Outcome),
}

/// The brightness part of Match, one look at a time. The left phone is only changed when it started on automatic (it gets a decent brightness of
/// its own first), and then the right phone is brought to the left one's. Each ISO change waits for the pictures to show it.
#[derive(Clone, Debug)]
pub struct Run {
    iso: [i64; 2],
    limits: [(i64, i64); 2],
    left_done: bool,
    brightness: bool,
    steps: u32,
    changed_at: u64,
    limited: bool,
}

impl Run {
    /// A run from the ISOs both phones were just set to. `tune_left`: the left phone's ISO is a guess and needs a brightness of its own.
    /// `brightness` false skips straight to the colour (phones that can't take a manual exposure).
    pub fn new(iso: [i64; 2], limits: [(i64, i64); 2], tune_left: bool, brightness: bool, now: u64) -> Run {
        Run { iso, limits, left_done: !tune_left, brightness, steps: 0, changed_at: now, limited: false }
    }

    pub fn iso(&self) -> [i64; 2] {
        self.iso
    }

    pub fn steps(&self) -> u32 {
        self.steps
    }

    fn outcome(&self) -> Outcome {
        if self.limited { Outcome::Limited } else { Outcome::Matched }
    }

    /// What to do after the look `(when, brightness of left and right)` at `now`; a look from before the last change has settled isn't used.
    pub fn tick(&mut self, now: u64, look: Option<(u64, [f64; 2])>) -> Act {
        let Some((at, luma)) = look else { return Act::Wait };
        if at < self.changed_at.saturating_add(SETTLE_NS) {
            return Act::Wait;
        }
        if !self.brightness {
            return Act::Fit(Outcome::Matched);
        }
        if !self.left_done {
            let step = if self.steps >= MAX_ISO_STEPS { IsoStep::Limit } else { iso_step(self.iso[0], luma[0], TARGET_LUMA, self.limits[0]) };
            match step {
                IsoStep::Set(iso) => return self.set(Eye::Left, iso, now),
                IsoStep::Limit => (self.left_done, self.limited) = (true, true),
                IsoStep::Hold => self.left_done = true,
            }
        }
        let step = if self.steps >= MAX_ISO_STEPS { IsoStep::Limit } else { iso_step(self.iso[1], luma[1], luma[0], self.limits[1]) };
        match step {
            IsoStep::Set(iso) => self.set(Eye::Right, iso, now),
            IsoStep::Limit => {
                self.limited = true;
                Act::Fit(self.outcome())
            }
            IsoStep::Hold => Act::Fit(self.outcome()),
        }
    }

    fn set(&mut self, eye: Eye, iso: i64, now: u64) -> Act {
        self.iso[if eye == Eye::Left { 0 } else { 1 }] = iso;
        self.changed_at = now;
        self.steps += 1;
        Act::SetIso { eye, iso }
    }
}

/// The lens a phone streams from in its /v1/state: the camera whose id is `active_lens` (a multi-lens camera says which lens it is on), else the
/// one marked `current`, else the first.
pub fn active_lens(state: &Value) -> Option<&Value> {
    let cameras = state.get("cameras")?.as_array()?;
    let active = state.get("active_lens").filter(|a| !a.is_null());
    active
        .and_then(|a| cameras.iter().find(|c| c.get("id") == Some(a)))
        .or_else(|| cameras.iter().find(|c| c.get("current") == Some(&Value::Bool(true))))
        .or_else(|| cameras.first())
}

fn lens_range(phone: &Value, min: &str, max: &str) -> Option<(i64, i64)> {
    let lens = active_lens(phone)?;
    let get = |key: &str| lens.get(key).and_then(Value::as_i64).filter(|v| *v > 0);
    Some((get(min)?, get(max)?)).filter(|(a, b)| a <= b)
}

/// The ISO range of the lens a phone is streaming from.
pub fn iso_range(phone: &Value) -> Option<(i64, i64)> {
    lens_range(phone, "isoMin", "isoMax")
}

/// The shutter range (nanoseconds) of the lens a phone is streaming from.
pub fn shutter_range(phone: &Value) -> Option<(i64, i64)> {
    lens_range(phone, "shutterMinNs", "shutterMaxNs")
}

/// Whether the lens a phone streams from takes a manual ISO and shutter.
pub fn takes_manual_exposure(state: &Value) -> bool {
    active_lens(state).and_then(|l| l.get("supportsManualSensor")).and_then(Value::as_bool) == Some(true) && iso_range(state).is_some() && shutter_range(state).is_some()
}

/// The ISO and shutter (nanoseconds) a phone is on when both are manual; None on automatic, which doesn't say what it chose.
pub fn manual_exposure(state: &Value) -> Option<(i64, i64)> {
    let whole = |key: &str| state.get(key).and_then(Value::as_f64).filter(|v| *v > 0.0).map(|v| v.round() as i64);
    if state.get("auto") == Some(&Value::Bool(true)) {
        return None;
    }
    Some((whole("iso")?, whole("shutter_ns")?))
}

/// The shutter both phones are set to (nanoseconds): the left phone's if it is manual, else the right phone's, else 1/60 s; held under the frame
/// time (from the phone's `phone_fps`, else `frame_ns`) and inside what both lenses take.
pub fn pick_shutter(left: &Value, right: &Value, frame_ns: u64) -> i64 {
    let wanted = manual_exposure(left).or_else(|| manual_exposure(right)).map_or(DEFAULT_SHUTTER_NS, |m| m.1);
    let fps = left.get("phone_fps").and_then(Value::as_f64).filter(|f| *f > 0.0);
    let frame = fps.map_or(frame_ns as f64, |f| 1e9 / f);
    let mut ns = wanted.min(((frame * SHUTTER_OF_FRAME).round() as i64).max(1));
    for state in [left, right] {
        if let Some((min, max)) = shutter_range(state) {
            ns = ns.clamp(min, max);
        }
    }
    ns
}

/// What Match starts a pair of phones with.
#[derive(Clone, Debug, PartialEq)]
pub struct Start {
    /// The shutter both phones get, nanoseconds.
    pub shutter: i64,
    /// The ISO each phone gets (left, right), held to its lens.
    pub iso: [i64; 2],
    pub limits: [(i64, i64); 2],
    /// The left phone was on automatic, so its ISO is a guess that has to be brought to a decent brightness first.
    pub tune_left: bool,
}

/// The start for two phones' states, or None when either can't take a manual exposure (Match then only fits the colour).
pub fn plan(left: &Value, right: &Value, frame_ns: u64) -> Option<Start> {
    if !takes_manual_exposure(left) || !takes_manual_exposure(right) {
        return None;
    }
    let limits = [iso_range(left)?, iso_range(right)?];
    let from_left = manual_exposure(left).map(|m| m.0);
    let first = from_left.unwrap_or(START_ISO);
    let iso = [first.clamp(limits[0].0, limits[0].1), first.clamp(limits[1].0, limits[1].1)];
    Some(Start { shutter: pick_shutter(left, right, frame_ns), iso, limits, tune_left: from_left.is_none() })
}

/// The /v1/control bodies that put a phone on a manual ISO and shutter (it goes manual once it has both).
pub fn exposure_bodies(iso: i64, shutter_ns: i64) -> [Value; 2] {
    [json!({"action": "iso", "value": iso}), json!({"action": "shutter", "value": shutter_ns})]
}

/// The one body that sets a phone's ISO.
pub fn iso_body(iso: i64) -> Value {
    json!({"action": "iso", "value": iso})
}

// ── Size from the lenses ──────────────────────────────────────────────────

/// The 35 mm equivalent focal length (millimetres) of the lens a phone streams from; None where the phone doesn't say.
pub fn focal_mm(state: &Value) -> Option<f64> {
    active_lens(state)?.get("focalMm").and_then(Value::as_f64).filter(|f| *f > 0.0)
}

/// The Size (percent) that makes the right picture show as much as the left from the two lenses' focal lengths: the ratio of the tangents of half
/// their fields of view. A right lens that sees wider (a shorter focal length) gets its picture enlarged. None when the ratio is off the slider,
/// like a wide lens against a telephoto, where Size can't help; held to a tenth of a percent.
pub fn size_from_focal(left_mm: f64, right_mm: f64) -> Option<f64> {
    if left_mm <= 0.0 || right_mm <= 0.0 {
        return None;
    }
    let half = |mm: f64| (FULL_FRAME_WIDTH_MM / (2.0 * mm)).atan().tan();
    let size = (half(right_mm) / half(left_mm) * 1000.0).round() / 10.0;
    (SIZE_MIN..=SIZE_MAX).contains(&size).then_some(size)
}

#[cfg(test)]
#[path = "stereo_match_tests.rs"]
mod tests;
