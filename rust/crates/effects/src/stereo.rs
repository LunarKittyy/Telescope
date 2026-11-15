//! The pixel work and the pairing of the 3D plugin: two eyes packed side by side or as red/cyan, the depth and height shifts, which two
//! frames go together and how well they line up. Nothing here knows about plugins, so each piece can be tested on its own.

use crate::cpu::Rgb;
use serde_json::{json, Value};
use telescope_core::frame::Frame;

/// How many frames of each stream the host keeps for pairing (HostFrames::want_frame_history).
pub const HISTORY_FRAMES: usize = 6;
/// What the Depth and Height sliders reach, in percent of the width and of the height.
pub const SHIFT_LIMIT: f64 = 5.0;
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
/// app, a webcam) the newest of each go together, and `by_time` says so.
pub fn pair_frames<T: Clone>(left: &T, right: &T, capture: impl Fn(&T) -> Option<u64>, near: impl Fn(Eye, u64) -> Option<T>) -> Pair<T> {
    match (capture(left), capture(right)) {
        (Some(l), Some(r)) if l <= r => Pair { left: left.clone(), right: near(Eye::Right, l).unwrap_or_else(|| right.clone()), by_time: true },
        (Some(_), Some(r)) => Pair { left: near(Eye::Left, r).unwrap_or_else(|| left.clone()), right: right.clone(), by_time: true },
        _ => Pair { left: left.clone(), right: right.clone(), by_time: false },
    }
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

/// Side by side: each eye is stretched to its half of the picture, which is how a headset or a 3D screen takes half side by side (on a canvas twice
/// as wide as the eyes' own shape that's no stretch at all). What a shift uncovers stays black.
pub fn side_by_side(left: &Rgb, right: &Rgb, w: usize, h: usize, depth_percent: f64, height_percent: f64) -> Rgb {
    let (lw, rw) = (w / 2, w - w / 2);
    let (a, b) = eye_offsets(shift_px(depth_percent, lw), shift_px(height_percent, h));
    let mut out = Rgb::new(w, h);
    // a picture one pixel wide has no room for the left eye
    if lw > 0 && h > 0 {
        paste(&mut out, &scale(left, lw, h), a.0, a.1, (0, 0, lw, h));
    }
    if rw > 0 && h > 0 {
        paste(&mut out, &scale(right, rw, h), lw as i64 + b.0, b.1, (lw, 0, rw, h));
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
pub fn red_cyan(left: &Rgb, right: &Rgb, w: usize, h: usize, depth_percent: f64, height_percent: f64) -> Rgb {
    let (a, b) = eye_offsets(shift_px(depth_percent, w), shift_px(height_percent, h));
    let eye = |src: &Rgb, (dx, dy): (i64, i64)| {
        let (x, y, fw, fh) = contain(src.w, src.h, w, h);
        let mut canvas = Rgb::new(w, h);
        paste(&mut canvas, &scale(src, fw, fh), x as i64 + dx, y as i64 + dy, (0, 0, w, h));
        canvas
    };
    let (l, r) = (eye(left, a), eye(right, b));
    let mut out = Rgb::scratch(w, h);
    for ((o, l), r) in out.data.as_chunks_mut::<3>().0.iter_mut().zip(l.data.as_chunks::<3>().0).zip(r.data.as_chunks::<3>().0) {
        *o = dubois(*l, *r);
    }
    out
}

/// The picture for the two eyes' frames in the format of `like`, `w` x `h`; None for a frame whose format can't be read.
pub fn render(left: &Frame, right: &Frame, look: Look, (w, h): (u32, u32), depth_percent: f64, height_percent: f64, like: &Frame) -> Option<Frame> {
    if w == 0 || h == 0 {
        return None;
    }
    let (l, r) = (Rgb::from_frame(left)?, Rgb::from_frame(right)?);
    let (pw, ph) = (w as usize, h as usize);
    let picture = match look {
        Look::SideBySide => side_by_side(&l, &r, pw, ph, depth_percent, height_percent),
        Look::RedCyan => red_cyan(&l, &r, pw, ph, depth_percent, height_percent),
    };
    let mut out = Frame::new(w, h, like.format, vec![0; like.format.size(w, h)]);
    picture.write_to(&mut out);
    Some(out)
}

// ── Matching the phones ───────────────────────────────────────────────────

/// The /v1/control bodies that give any phone the shown phone's exposure, white balance and focus, from its /v1/state. Manual values are copied;
/// where the shown phone is automatic the others are put back to automatic too, so the two keep matching. A phone on point focus leaves focus alone.
pub fn link_bodies(state: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let whole = |key: &str| state.get(key).and_then(Value::as_f64).filter(|v| *v > 0.0).map(|v| v.round() as i64);
    match (whole("iso"), whole("shutter_ns")) {
        (Some(iso), Some(ns)) => out.extend([json!({"action": "iso", "value": iso}), json!({"action": "shutter", "value": ns})]),
        _ => out.push(json!({"action": "auto"})),
    }
    let gain = |key: &str| state.get(key).and_then(Value::as_f64).filter(|v| *v > 0.0);
    match (state.get("wb_manual").and_then(Value::as_bool), gain("wb_r"), gain("wb_ge"), gain("wb_go"), gain("wb_b")) {
        (Some(true), Some(r), Some(ge), Some(go), Some(b)) => out.push(json!({"action": "wb_gains", "r": r, "ge": ge, "go": go, "b": b})),
        _ => out.push(json!({"action": "wb_auto"})),
    }
    match state.get("focus_mode").and_then(Value::as_str) {
        Some("manual") => {
            out.push(json!({"action": "focus_mode", "value": "manual"}));
            if let Some(d) = state.get("focus_distance").and_then(Value::as_f64) {
                out.push(json!({"action": "focus_distance", "value": d}));
            }
        }
        Some("continuous") => out.push(json!({"action": "focus_mode", "value": "continuous"})),
        _ => {}
    }
    out
}

#[cfg(test)]
#[path = "stereo_tests.rs"]
mod tests;
