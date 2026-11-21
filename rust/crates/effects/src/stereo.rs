//! The pixel work and the pairing of the 3D plugin: two eyes packed side by side or as red/cyan, the depth, height and size of the right eye, the tilt of each eye,
//! the colour curves that match it to the left, which two frames go together and how well they line up. Nothing here knows about plugins, so each piece can be tested on its own.

use crate::cpu::Rgb;
use crate::stereo_match::{centre_stats, Lut, Stats, SIZE_MAX, SIZE_MIN};
use std::borrow::Cow;
use std::sync::Arc;
use telescope_core::frame::Frame;
use telescope_core::image::{over_rows, resample_linear, rint, Tap};

/// How many frames of each stream the host keeps for pairing (HostFrames::want_frame_history).
pub const HISTORY_FRAMES: usize = 6;
/// The most frames of each stream the pairing ever asks the host to keep.
const MAX_HISTORY: usize = 64;
/// What the Depth and Height sliders reach, in percent of the width and of the height.
pub const SHIFT_LIMIT: f64 = 15.0;
/// What each Tilt slider reaches, in degrees.
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

/// The eye scaled to w x h: averaged down where it gets smaller, interpolated where it gets bigger; the eye itself when it is that size already.
fn scale(rgb: &Rgb, w: usize, h: usize) -> Cow<'_, Rgb> {
    if (rgb.w, rgb.h) == (w, h) {
        Cow::Borrowed(rgb)
    } else if w <= rgb.w && h <= rgb.h {
        Cow::Owned(rgb.shrink(w, h))
    } else {
        Cow::Owned(rgb.linear(w, h))
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
    /// The right eye's size against the left's in percent, 100 for as they are: over 100 the right eye is zoomed in, under it the left one is.
    pub size: f64,
    /// The left and the right eye's roll in degrees, positive turns it clockwise.
    pub tilt_left: f64,
    pub tilt_right: f64,
    /// The colour curves that make the right eye look like the left (Match).
    pub lut: Option<Arc<Lut>>,
}

impl Default for Tune {
    fn default() -> Tune {
        Tune { depth: 0.0, height: 0.0, size: 100.0, tilt_left: 0.0, tilt_right: 0.0, lut: None }
    }
}

/// How much of an eye is zoomed into its box, shifted and turned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    /// 1 for the whole eye, more to cut the edges off and enlarge what is left.
    pub zoom: f64,
    /// How far the picture moves, in pixels of the box.
    pub dx: f64,
    pub dy: f64,
    /// The turn in degrees, clockwise.
    pub tilt: f64,
    /// How much narrower than its own shape the eye is drawn (1: not at all).
    pub squeeze: f64,
}

impl Place {
    /// Whether the eye goes into its box as it is.
    fn is_plain(&self) -> bool {
        (self.zoom - 1.0).abs() < 1e-6 && self.dx == 0.0 && self.dy == 0.0 && self.tilt.abs() < 0.005
    }

    /// The zoom that keeps the whole box inside the eye once it is shifted and turned: the box grown by the shifts, turned back, must fit in the
    /// eye (an eye of `w` x `h` in its own shape, `squeeze` narrower).
    fn cover(w: f64, h: f64, (dx, dy): (f64, f64), tilt: f64, squeeze: f64) -> f64 {
        let (sin, cos) = tilt.to_radians().sin_cos();
        let (own_w, own_h) = (w / squeeze, h);
        let (grown_w, grown_h) = (own_w + 2.0 * dx.abs() / squeeze, own_h + 2.0 * dy.abs());
        ((grown_w * cos.abs() + grown_h * sin.abs()) / own_w).max((grown_w * sin.abs() + grown_h * cos.abs()) / own_h).max(1.0)
    }
}

/// How both eyes sit in their boxes (`boxes`: each eye's width and height, `squeeze` as in `Place`), left and right. The eye of the wider view is
/// zoomed in to the narrower one's (Size, so neither is ever shrunk into borders), and both are zoomed by as much more as the shifts and the roll
/// need so that no black shows at an edge or corner, so the two stay the same size as each other.
fn places(tune: &Tune, boxes: [(usize, usize); 2], offsets: [(i64, i64); 2], squeeze: [f64; 2]) -> [Place; 2] {
    let size = tune.size.clamp(SIZE_MIN, SIZE_MAX) / 100.0;
    let tilts = [tune.tilt_left, tune.tilt_right].map(|t| t.clamp(-TILT_LIMIT, TILT_LIMIT));
    let need = (0..2).map(|i| Place::cover(boxes[i].0 as f64, boxes[i].1 as f64, (offsets[i].0 as f64, offsets[i].1 as f64), tilts[i], squeeze[i])).fold(1.0, f64::max);
    let sizes = [(1.0 / size).max(1.0), size.max(1.0)];
    std::array::from_fn(|i| Place { zoom: sizes[i] * need, dx: offsets[i].0 as f64, dy: offsets[i].1 as f64, tilt: tilts[i], squeeze: squeeze[i] })
}

/// The taps of one axis of n pixels: each place samples the eye `zoom` times enlarged about its middle and moved `shift` pixels.
fn taps(n: usize, zoom: f64, shift: f64) -> Vec<Tap> {
    let (last, middle) = (n.saturating_sub(1), n as f64 / 2.0);
    (0..n)
        .map(|d| {
            let at = ((d as f64 + 0.5 - middle - shift) / zoom + middle - 0.5).clamp(0.0, last as f64);
            let first = at.floor();
            let second = rint(((at - first) * 2048.0).min(2048.0)).clamp(0, 2048);
            (first as usize, (first as usize + 1).min(last), 2048 - second, second)
        })
        .collect()
}

/// The fixed-point step of the turned sampler: sixteen bits after the point.
const FIX: i64 = 1 << 16;

/// The eye (already the size of its box) put in its place: zoomed, shifted and turned clockwise about its middle, in a picture of the same size.
/// Nothing is left uncovered as long as the zoom is at least what `Place::cover` gives. Without a turn the rows and the columns are blended
/// separately (fast); a turn samples each pixel from the four around where it came from.
pub fn place<'a>(src: &'a Rgb, p: &Place, lut: Option<&Lut>) -> Cow<'a, Rgb> {
    let curved = |eye: Cow<'a, Rgb>| match (eye, lut) {
        (eye, None) => eye,
        (Cow::Owned(mut eye), Some(lut)) => {
            lut.apply(&mut eye);
            Cow::Owned(eye)
        }
        (Cow::Borrowed(eye), Some(lut)) => Cow::Owned(lut.mapped(eye)),
    };
    if src.w == 0 || src.h == 0 || p.zoom <= 0.0 || p.squeeze <= 0.0 {
        return curved(Cow::Borrowed(src));
    }
    if p.is_plain() {
        return curved(Cow::Borrowed(src));
    }
    if p.tilt.abs() < 0.005 {
        let data = resample_linear(&src.data, src.w, src.h, 3, &taps(src.w, p.zoom, p.dx), &taps(src.h, p.zoom, p.dy));
        return curved(Cow::Owned(Rgb { w: src.w, h: src.h, data }));
    }
    let mut out = Rgb::scratch(src.w, src.h);
    let (sin, cos) = p.tilt.to_radians().sin_cos();
    let (cx, cy, inv) = (src.w as f64 / 2.0, src.h as f64 / 2.0, 1.0 / p.zoom);
    // each output pixel looks up where it came from: the way back is the opposite turn and a shrink
    let fixed = |v: f64| (v * FIX as f64).round() as i64;
    let (ax, ay) = (fixed(cos * inv), fixed(-sin / p.squeeze * inv));
    let (last_x, last_y) = ((src.w as i64 - 1) * FIX, (src.h as i64 - 1) * FIX);
    let (w, row_bytes) = (src.w, src.w * 3);
    let data = &src.data;
    let draw = |y: usize, line: &mut [u8], finish: &dyn Fn([u8; 3]) -> [u8; 3]| {
        let dy = y as f64 + 0.5 - cy - p.dy;
        let dx = 0.5 - cx - p.dx;
        let mut sx = fixed(cx - 0.5 + (cos * dx + p.squeeze * sin * dy) * inv);
        let mut sy = fixed(cy - 0.5 + (-sin * dx / p.squeeze + cos * dy) * inv);
        for px in line.as_chunks_mut::<3>().0 {
            let (x, y) = (sx.clamp(0, last_x), sy.clamp(0, last_y));
            let (x0, y0) = ((x >> 16) as usize, (y >> 16) as usize);
            let (fx, fy) = (((x >> 8) & 255) as u32, ((y >> 8) & 255) as u32);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(src.h - 1));
            let (top, bottom) = (&data[y0 * row_bytes..], &data[y1 * row_bytes..]);
            let mut v = [0u8; 3];
            for (c, o) in v.iter_mut().enumerate() {
                let above = top[x0 * 3 + c] as u32 * (256 - fx) + top[x1 * 3 + c] as u32 * fx;
                let below = bottom[x0 * 3 + c] as u32 * (256 - fx) + bottom[x1 * 3 + c] as u32 * fx;
                *o = ((above * (256 - fy) + below * fy + 32768) >> 16) as u8;
            }
            *px = finish(v);
            sx += ax;
            sy += ay;
        }
    };
    match lut {
        None => over_rows(&mut out.data, row_bytes, |y, line| draw(y, line, &|v| v)),
        Some(lut) => over_rows(&mut out.data, row_bytes, |y, line| draw(y, line, &|v| [lut.0[0][v[0] as usize], lut.0[1][v[1] as usize], lut.0[2][v[2] as usize]])),
    }
    Cow::Owned(out)
}

/// How much narrower than its own shape an eye of `src`'s shape is drawn at w x h.
fn squeeze_of(src: &Rgb, w: usize, h: usize) -> f64 {
    if src.w == 0 || src.h == 0 || h == 0 {
        return 1.0;
    }
    w as f64 / (h as f64 * src.w as f64 / src.h as f64)
}

/// Side by side: each eye is stretched to its half of the picture, which is how a headset or a 3D screen takes half side by side (on a canvas twice
/// as wide as the eyes' own shape that's no stretch at all). Shifts, size and roll cut into the eyes instead of leaving anything black.
pub fn side_by_side(left: &Rgb, right: &Rgb, w: usize, h: usize, tune: &Tune) -> Rgb {
    let (lw, rw) = (w / 2, w - w / 2);
    let mut out = Rgb::scratch(w, h);
    if w == 0 || h == 0 {
        return out;
    }
    let (a, b) = eye_offsets(shift_px(tune.depth, lw), shift_px(tune.height, h));
    let (lw_eye, rw_eye) = (lw.max(1), rw.max(1));
    let squeeze = [squeeze_of(left, lw_eye, h), squeeze_of(right, rw_eye, h)];
    let [pl, pr] = places(tune, [(lw_eye, h), (rw_eye, h)], [a, b], squeeze);
    // a picture one pixel wide has no room for the left eye
    let (scaled_left, scaled_right) = (scale(left, lw, h), scale(right, rw, h));
    let l = (lw > 0).then(|| place(&scaled_left, &pl, None));
    let r = place(&scaled_right, &pr, tune.lut.as_deref());
    over_rows(&mut out.data, w * 3, |y, line| {
        let (to_left, to_right) = line.split_at_mut(lw * 3);
        if let Some(l) = &l {
            to_left.copy_from_slice(&l.data[y * lw * 3..(y + 1) * lw * 3]);
        }
        to_right.copy_from_slice(&r.data[y * rw * 3..(y + 1) * rw * 3]);
    });
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

/// The eye on a canvas of the picture's size: as it is when it fills it, else in the middle of black.
fn on_canvas<'a>(eye: Cow<'a, Rgb>, (x, y, fw, fh): (usize, usize, usize, usize), (w, h): (usize, usize)) -> Cow<'a, Rgb> {
    if (fw, fh) == (w, h) {
        return eye;
    }
    let mut canvas = Rgb::new(w, h);
    paste(&mut canvas, &eye, x as i64, y as i64, (0, 0, w, h));
    Cow::Owned(canvas)
}

/// Red/cyan: each eye is fitted whole into the picture (bars where its shape differs), shifted, and the two are mixed through the Dubois matrices.
pub fn red_cyan(left: &Rgb, right: &Rgb, w: usize, h: usize, tune: &Tune) -> Rgb {
    let mut out = Rgb::scratch(w, h);
    if w == 0 || h == 0 {
        return out;
    }
    let (a, b) = eye_offsets(shift_px(tune.depth, w), shift_px(tune.height, h));
    let (lbox, rbox) = (contain(left.w, left.h, w, h), contain(right.w, right.h, w, h));
    let [pl, pr] = places(tune, [(lbox.2, lbox.3), (rbox.2, rbox.3)], [a, b], [1.0, 1.0]);
    let (scaled_left, scaled_right) = (scale(left, lbox.2, lbox.3), scale(right, rbox.2, rbox.3));
    let (l, r) = (on_canvas(place(&scaled_left, &pl, None), lbox, (w, h)), on_canvas(place(&scaled_right, &pr, tune.lut.as_deref()), rbox, (w, h)));
    over_rows(&mut out.data, w * 3, |y, line| {
        let (l, r) = (&l.data[y * w * 3..(y + 1) * w * 3], &r.data[y * w * 3..(y + 1) * w * 3]);
        for ((o, l), r) in line.as_chunks_mut::<3>().0.iter_mut().zip(l.as_chunks::<3>().0).zip(r.as_chunks::<3>().0) {
            *o = dubois(*l, *r);
        }
    });
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

#[cfg(test)]
#[path = "stereo_speed_tests.rs"]
mod speed_tests;
