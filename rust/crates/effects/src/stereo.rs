//! The pixel work and the pairing of the 3D plugin: two eyes packed side by side or as red/cyan, the depth, height, size and tilt of the right eye,
//! the colour curves that match it to the left, which two frames go together and how well they line up. Nothing here knows about plugins, so each piece can be tested on its own.

use crate::cpu::Rgb;
use crate::stereo_match::{centre_stats, Lut, Stats, SIZE_MAX, SIZE_MIN};
use std::sync::Arc;
use telescope_core::frame::Frame;

/// How many frames of each stream the host keeps for pairing (HostFrames::want_frame_history).
pub const HISTORY_FRAMES: usize = 6;
/// The most frames of each stream the pairing ever asks the host to keep.
const MAX_HISTORY: usize = 64;
/// What the Depth and Height sliders reach, in percent of the width and of the height.
pub const SHIFT_LIMIT: f64 = 5.0;
/// What the Tilt slider reaches, in degrees.
pub const TILT_LIMIT: f64 = 5.0;
/// What the Timing slider reaches, in milliseconds either way.
pub const TIMING_LIMIT_MS: f64 = 200.0;
/// A frame time when nothing says otherwise (30 fps), in nanoseconds.
pub const DEFAULT_FRAME_NS: u64 = 33_333_333;
/// Past this much error plus capture gap two pictures are no longer called roughly in sync.
pub const ROUGH_LIMIT_NS: u64 = 100_000_000;

/// How the two eyes go out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    /// Each eye gets half the width of the picture.
    #[default]
    SideBySide,
    /// One picture for red/cyan glasses.
    RedCyan,
}

impl Look {
    pub const ALL: [Look; 2] = [Look::SideBySide, Look::RedCyan];

    /// The name it is saved under.
    pub fn name(self) -> &'static str {
        match self {
            Look::SideBySide => "side_by_side",
            Look::RedCyan => "red_cyan",
        }
    }

    pub fn from_name(name: &str) -> Option<Look> {
        Look::ALL.into_iter().find(|l| l.name() == name)
    }

    pub fn index(self) -> usize {
        Look::ALL.iter().position(|l| *l == self).unwrap_or(0)
    }
}

/// The left and the right eye.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eye {
    Left,
    Right,
}

// ── Pairing ───────────────────────────────────────────────────────────────

/// The index of the capture time nearest to `target`; of two as near the later one (the list is oldest first). None for no capture times.
pub fn nearest_capture(captures: &[Option<u64>], target: u64) -> Option<usize> {
    captures.iter().enumerate().filter_map(|(i, c)| c.map(|c| (c.abs_diff(target), std::cmp::Reverse(i), i))).min().map(|(_, _, i)| i)
}

/// Two frames that go together, and whether capture times chose them.
#[derive(Clone, Debug, PartialEq)]
pub struct Pair<T> {
    pub left: T,
    pub right: T,
    pub by_time: bool,
}

/// Picks the frames of the two eyes from the newest of each. With capture times on both, the eye that runs behind (the older newest frame) keeps its
/// frame and the other eye gives the frame of its recent history nearest to it (`near`), or its newest where it has none. Without them (an old phone
/// app, a webcam) the newest of each go together, and `by_time` says so. `offset_ns` is how much later the right eye's moment is than the left's
/// (Timing): the two line up when the right capture time is the left's plus the offset.
pub fn pair_frames<T: Clone>(left: &T, right: &T, capture: impl Fn(&T) -> Option<u64>, near: impl Fn(Eye, u64) -> Option<T>, offset_ns: i64) -> Pair<T> {
    match (capture(left), capture(right)) {
        // the right eye on the left's clock is its time less the offset
        (Some(l), Some(r)) if i128::from(l) <= i128::from(r) - i128::from(offset_ns) => {
            Pair { left: left.clone(), right: near(Eye::Right, l.saturating_add_signed(offset_ns)).unwrap_or_else(|| right.clone()), by_time: true }
        }
        (Some(_), Some(r)) => Pair { left: near(Eye::Left, r.saturating_add_signed(offset_ns.saturating_neg())).unwrap_or_else(|| left.clone()), right: right.clone(), by_time: true },
        _ => Pair { left: left.clone(), right: right.clone(), by_time: false },
    }
}

/// How many frames of history each eye needs for a Timing of `offset_ms`: the offset in frames and a few to spare, never fewer than the default.
pub fn history_frames(offset_ms: f64, frame_ns: u64) -> usize {
    let frames = (offset_ms.abs() * 1e6 / frame_ns.max(1) as f64).ceil() as usize;
    (frames + 3).clamp(HISTORY_FRAMES, MAX_HISTORY)
}

/// Which of the frames that came in (by arrival time, oldest first) goes out when the eye is held back by `delay_ns`: the one nearest to the newest
/// time less the delay. None for no frames.
pub fn delayed_index(arrivals: &[u64], delay_ns: u64) -> Option<usize> {
    let want = arrivals.last()?.saturating_sub(delay_ns);
    arrivals.iter().enumerate().map(|(i, t)| (t.abs_diff(want), std::cmp::Reverse(i), i)).min().map(|(_, _, i)| i)
}

/// How well the two pictures of a pair line up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncStatus {
    /// Off by less than half a frame, counting what the clocks can be off by.
    InSync,
    /// Off by more than that, but not by much.
    Rough,
    /// No capture times to go by, or too far apart.
    NotSynced,
}

/// The sync of a pair from each phone's clock error bound, the gap between the two capture times and a frame's length (all nanoseconds). None
/// for a bound or a gap means the stream has no capture times (or no clock offset yet), so nothing can be said.
pub fn sync_status(errors: [Option<u64>; 2], gap_ns: Option<u64>, frame_ns: u64) -> SyncStatus {
    let (Some(a), Some(b), Some(gap)) = (errors[0], errors[1], gap_ns) else { return SyncStatus::NotSynced };
    let off = a.max(b).saturating_add(gap);
    if off < frame_ns / 2 {
        SyncStatus::InSync
    } else if off <= ROUGH_LIMIT_NS.max(frame_ns) {
        SyncStatus::Rough
    } else {
        SyncStatus::NotSynced
    }
}

/// The frame time after one more gap between frames: a running average that ignores gaps no stream makes (a stall, a burst).
pub fn smooth_frame_ns(before: u64, gap_ns: u64) -> u64 {
    if !(8_000_000..=250_000_000).contains(&gap_ns) {
        return before;
    }
    (before * 7 + gap_ns) / 8
}

// ── Pixels ────────────────────────────────────────────────────────────────

/// A shift in percent of `extent`, in pixels.
pub fn shift_px(percent: f64, extent: usize) -> i64 {
    (percent.clamp(-SHIFT_LIMIT, SHIFT_LIMIT) / 100.0 * extent as f64).round() as i64
}

/// The eye scaled to w x h: averaged down where it gets smaller, interpolated where it gets bigger.
fn scale(rgb: &Rgb, w: usize, h: usize) -> Rgb {
    if (rgb.w, rgb.h) == (w, h) {
        rgb.clone()
    } else if w <= rgb.w && h <= rgb.h {
        rgb.shrink(w, h)
    } else {
        rgb.linear(w, h)
    }
}

/// Copies `src` into `dst` with its top left corner at (x, y), keeping to the box (x0, y0, w, h) of `dst`.
fn paste(dst: &mut Rgb, src: &Rgb, x: i64, y: i64, (bx, by, bw, bh): (usize, usize, usize, usize)) {
    let (left, right) = ((bx as i64).max(x), ((bx + bw) as i64).min(x + src.w as i64));
    if left >= right {
        return;
    }
    for row in 0..src.h as i64 {
        let dy = y + row;
        if dy < by as i64 || dy >= (by + bh) as i64 {
            continue;
        }
        let from = ((row as usize) * src.w + (left - x) as usize) * 3;
        let to = (dy as usize * dst.w + left as usize) * 3;
        let n = (right - left) as usize * 3;
        dst.data[to..to + n].copy_from_slice(&src.data[from..from + n]);
    }
}

/// The largest box of the source's shape that fits w x h, centred: (x, y, width, height).
fn contain(src_w: usize, src_h: usize, w: usize, h: usize) -> (usize, usize, usize, usize) {
    if src_w == 0 || src_h == 0 {
        return (0, 0, w, h);
    }
    let s = (w as f64 / src_w as f64).min(h as f64 / src_h as f64);
    let (fw, fh) = (((src_w as f64 * s).round() as usize).clamp(1, w), ((src_h as f64 * s).round() as usize).clamp(1, h));
    ((w - fw) / 2, (h - fh) / 2, fw, fh)
}

/// Where the eyes go sideways and up: depth moves the left eye right and the right eye left (half each, the right one taking the odd pixel), and
/// height moves the right eye up (negative: down).
fn eye_offsets(depth_px: i64, height_px: i64) -> ((i64, i64), (i64, i64)) {
    let half = depth_px / 2;
    ((half, 0), (-(depth_px - half), -height_px))
}

/// What the right eye is fixed with: the shifts, its size and roll, and the colour curves that match it to the left.
#[derive(Clone, Debug)]
pub struct Tune {
    /// Percent of the width, positive moves the eyes together.
    pub depth: f64,
    /// Percent of the height, positive lifts the right eye.
    pub height: f64,
    /// The right eye's size in percent, 100 for as it is.
    pub size: f64,
    /// The right eye's roll in degrees, positive turns it clockwise.
    pub tilt: f64,
    /// The colour curves that make the right eye look like the left (Match).
    pub lut: Option<Arc<Lut>>,
}

impl Default for Tune {
    fn default() -> Tune {
        Tune { depth: 0.0, height: 0.0, size: 100.0, tilt: 0.0, lut: None }
    }
}

impl Tune {
    /// The right eye needs more than moving: a size or a roll.
    fn warps(&self) -> bool {
        (self.size - 100.0).abs() > 0.05 || self.tilt.abs() > 0.05
    }
}

/// Bilinear value of `src` at the fractional pixel (x, y); None where it's more than half a pixel outside.
fn sample(src: &Rgb, x: f32, y: f32) -> Option<[u8; 3]> {
    let (w, h) = (src.w as f32, src.h as f32);
    if !(x >= -0.5 && x <= w - 0.5 && y >= -0.5 && y <= h - 0.5) {
        return None;
    }
    let (x, y) = (x.clamp(0.0, w - 1.0), y.clamp(0.0, h - 1.0));
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(src.w - 1), (y0 + 1).min(src.h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |x: usize, y: usize| {
        let i = (y * src.w + x) * 3;
        [src.data[i] as f32, src.data[i + 1] as f32, src.data[i + 2] as f32]
    };
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    Some(std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bottom = c[i] + (d[i] - c[i]) * fx;
        (top + (bottom - top) * fy + 0.5) as u8
    }))
}

/// The eye enlarged by `scale` (1.1: a tenth bigger) and turned clockwise by `tilt` degrees about its middle, in a picture of the same size; what it
/// leaves uncovered is black. `squeeze` is how much narrower than its own shape the eye is drawn (half side by side on 16:9 squeezes each eye to
/// half), so the turn is a turn of the eye's own shape and not a shear of the squeezed one: 1 for an eye drawn at its shape.
pub fn warp(src: &Rgb, scale: f64, tilt: f64, squeeze: f64) -> Rgb {
    let mut out = Rgb::new(src.w, src.h);
    if src.w == 0 || src.h == 0 || scale <= 0.0 || squeeze <= 0.0 {
        return out;
    }
    let (sin, cos) = tilt.to_radians().sin_cos();
    let (cx, cy, inv) = (src.w as f64 / 2.0, src.h as f64 / 2.0, 1.0 / scale);
    // each output pixel looks up where it came from: the way back is the opposite turn and a shrink
    let (dsx, dsy) = ((cos * inv) as f32, (-sin / squeeze * inv) as f32);
    for y in 0..src.h {
        let dy = y as f64 + 0.5 - cy;
        let dx = 0.5 - cx;
        let mut sx = (cx + (cos * dx + squeeze * sin * dy) * inv - 0.5) as f32;
        let mut sy = (cy + (-sin * dx / squeeze + cos * dy) * inv - 0.5) as f32;
        let row = &mut out.data[y * src.w * 3..(y + 1) * src.w * 3];
        for px in row.as_chunks_mut::<3>().0 {
            if let Some(v) = sample(src, sx, sy) {
                *px = v;
            }
            sx += dsx;
            sy += dsy;
        }
    }
    out
}

/// The right eye, already scaled into its place, through the colour curves and then the size and roll. `squeeze` as in `warp`.
fn fix_right(mut eye: Rgb, tune: &Tune, squeeze: f64) -> Rgb {
    if let Some(lut) = &tune.lut {
        lut.apply(&mut eye);
    }
    if tune.warps() { warp(&eye, tune.size.clamp(SIZE_MIN, SIZE_MAX) / 100.0, tune.tilt.clamp(-TILT_LIMIT, TILT_LIMIT), squeeze) } else { eye }
}

/// How much narrower than its own shape an eye of `src`'s shape is drawn at w x h.
fn squeeze_of(src: &Rgb, w: usize, h: usize) -> f64 {
    if src.w == 0 || src.h == 0 || h == 0 {
        return 1.0;
    }
    w as f64 / (h as f64 * src.w as f64 / src.h as f64)
}

/// Side by side: each eye is stretched to its half of the picture, which is how a headset or a 3D screen takes half side by side (on a canvas twice
/// as wide as the eyes' own shape that's no stretch at all). What a shift uncovers stays black.
pub fn side_by_side(left: &Rgb, right: &Rgb, w: usize, h: usize, tune: &Tune) -> Rgb {
    let (lw, rw) = (w / 2, w - w / 2);
    let (a, b) = eye_offsets(shift_px(tune.depth, lw), shift_px(tune.height, h));
    let mut out = Rgb::new(w, h);
    // a picture one pixel wide has no room for the left eye
    if lw > 0 && h > 0 {
        paste(&mut out, &scale(left, lw, h), a.0, a.1, (0, 0, lw, h));
    }
    if rw > 0 && h > 0 {
        let eye = fix_right(scale(right, rw, h), tune, squeeze_of(right, rw, h));
        paste(&mut out, &eye, lw as i64 + b.0, b.1, (lw, 0, rw, h));
    }
    out
}

/// Dubois least squares red/cyan matrices (rows: red, green, blue out; columns: red, green, blue in), for the left and the right eye.
const DUBOIS_LEFT: [[f32; 3]; 3] = [[0.4561, 0.500484, 0.176381], [-0.0400822, -0.0378246, -0.0157589], [-0.0152161, -0.0205971, -0.00546856]];
const DUBOIS_RIGHT: [[f32; 3]; 3] = [[-0.0434706, -0.0879388, -0.00155529], [0.378476, 0.73364, -0.0184503], [-0.0721527, -0.112961, 1.2264]];

/// One pixel of the red/cyan picture from the two eyes' pixels.
pub fn dubois(left: [u8; 3], right: [u8; 3]) -> [u8; 3] {
    let (l, r) = (left.map(f32::from), right.map(f32::from));
    let channel = |c: usize| {
        let v: f32 = (0..3).map(|i| DUBOIS_LEFT[c][i] * l[i] + DUBOIS_RIGHT[c][i] * r[i]).sum();
        v.round().clamp(0.0, 255.0) as u8
    };
    [channel(0), channel(1), channel(2)]
}

/// Red/cyan: each eye is fitted whole into the picture (bars where its shape differs), shifted, and the two are mixed through the Dubois matrices.
pub fn red_cyan(left: &Rgb, right: &Rgb, w: usize, h: usize, tune: &Tune) -> Rgb {
    let (a, b) = eye_offsets(shift_px(tune.depth, w), shift_px(tune.height, h));
    let eye = |src: &Rgb, (dx, dy): (i64, i64), right_eye: bool| {
        let (x, y, fw, fh) = contain(src.w, src.h, w, h);
        let mut canvas = Rgb::new(w, h);
        let scaled = scale(src, fw, fh);
        let scaled = if right_eye { fix_right(scaled, tune, 1.0) } else { scaled };
        paste(&mut canvas, &scaled, x as i64 + dx, y as i64 + dy, (0, 0, w, h));
        canvas
    };
    let (l, r) = (eye(left, a, false), eye(right, b, true));
    let mut out = Rgb::scratch(w, h);
    for ((o, l), r) in out.data.as_chunks_mut::<3>().0.iter_mut().zip(l.data.as_chunks::<3>().0).zip(r.data.as_chunks::<3>().0) {
        *o = dubois(*l, *r);
    }
    out
}

/// A finished picture and, when asked, what Match reads from the two eyes (left, right) before any of the fixing.
pub struct Rendered {
    pub frame: Frame,
    pub stats: Option<[Stats; 2]>,
}

/// The picture for the two eyes' frames in the format of `like`, `w` x `h`; None for a frame whose format can't be read. `measure` also reads the
/// middle of each eye (centre_stats).
pub fn render_measured(left: &Frame, right: &Frame, look: Look, (w, h): (u32, u32), tune: &Tune, like: &Frame, measure: bool) -> Option<Rendered> {
    if w == 0 || h == 0 {
        return None;
    }
    let (l, r) = (Rgb::from_frame(left)?, Rgb::from_frame(right)?);
    let stats = if measure { centre_stats(&l).zip(centre_stats(&r)).map(|(a, b)| [a, b]) } else { None };
    let (pw, ph) = (w as usize, h as usize);
    let picture = match look {
        Look::SideBySide => side_by_side(&l, &r, pw, ph, tune),
        Look::RedCyan => red_cyan(&l, &r, pw, ph, tune),
    };
    let mut out = Frame::new(w, h, like.format, vec![0; like.format.size(w, h)]);
    picture.write_to(&mut out);
    Some(Rendered { frame: out, stats })
}

/// The picture for the two eyes' frames in the format of `like`, `w` x `h`; None for a frame whose format can't be read.
pub fn render(left: &Frame, right: &Frame, look: Look, size: (u32, u32), tune: &Tune, like: &Frame) -> Option<Frame> {
    render_measured(left, right, look, size, tune, like, false).map(|r| r.frame)
}

#[cfg(test)]
#[path = "stereo_tests.rs"]
mod tests;
