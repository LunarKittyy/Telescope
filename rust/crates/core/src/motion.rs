//! Motion in zones of the picture, for the plugins that ask (motion.py): a small grey copy of each frame compared with a background
//! that slowly takes in light changes. A shared host service, since any plugin can watch (Host::watch_motion); the shell feeds it
//! the shown stream's frames before the view.

use crate::actions::round_half_even;
use crate::frame::Frame;
use crate::image::{frame_rgb, grey_of, resize_area};
use crate::vision::{Turn, Upright};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

/// The grey copy's width
pub const WIDTH: usize = 160;
/// How often a stream's frames are looked at (seconds)
pub const EVERY_S: f64 = 0.1;
/// The background takes in changes with this time constant (light drifting, something left behind)
pub const ADAPT_S: f64 = 2.0;
/// A zone counts as moving for this long after it last moved, so a pause in a walk isn't two events
pub const HOLD_S: f64 = 1.0;
/// Frames this far apart (the panels came back to a stream, it stalled) start the background over
pub const GAP_S: f64 = 1.0;
/// Every part of the picture changing this much at once is a cut (a zoom, a lens picked on the phone)
pub const CUT: f64 = 0.6;
/// The parts: a grid this many across and down, so someone filling most of the picture still counts
pub const CUT_GRID: usize = 4;
/// After a lens switch the background follows the picture this long, till the new lens is in
pub const LENS_SETTLE_S: f64 = 1.5;
pub const SENSITIVITY_DEFAULT: u32 = 5;
/// At sensitivity 1 and 10: how many grey levels a pixel has to change by, and what share of a zone has to change
const LEVELS: (f64, f64) = (40.0, 12.0);
const SHARE: (f64, f64) = (0.05, 0.003);
const MIN_PIXELS: usize = 3;

/// (x0, y0, x1, y1), 0..1 of the picture.
pub type Zone = (f64, f64, f64, f64);

/// (grey levels, share of the zone) something has to change by to count, for a sensitivity of 1 to 10.
pub fn thresholds(sensitivity: i64) -> (f64, f64) {
    let t = (sensitivity.clamp(1, 10) - 1) as f64 / 9.0;
    (LEVELS.0 + (LEVELS.1 - LEVELS.0) * t, SHARE.0 * (SHARE.1 / SHARE.0).powf(t))
}

/// zones inside 0..1, each corner pair put in order; anything that isn't finite is left out, and so is a zone with no area.
pub fn clean_zones(zones: &[[f64; 4]]) -> Vec<Zone> {
    let mut out = Vec::new();
    for z in zones {
        if !z.iter().all(|v| v.is_finite()) {
            continue;
        }
        let c = z.map(|v| v.clamp(0.0, 1.0));
        let (x0, x1) = (c[0].min(c[2]), c[0].max(c[2]));
        let (y0, y1) = (c[1].min(c[3]), c[1].max(c[3]));
        if x1 > x0 && y1 > y0 {
            out.push((x0, y0, x1, y1));
        }
    }
    out
}

/// A zone given as f32 (the host's zones) as the number its decimal text says, so 0.6 is the 0.6 Python gets.
pub fn widen(zones: &[[f32; 4]]) -> Vec<[f64; 4]> {
    zones.iter().map(|z| z.map(|v| v.to_string().parse::<f64>().unwrap_or(f64::from(v)))).collect()
}

/// A small grey picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grey {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

/// cv2.GaussianBlur(grey, (5, 5), 0): the kernel is 1 4 6 4 1 over 16 each way, the edges mirrored without repeating the last pixel.
fn blur5(g: &Grey) -> Grey {
    let mirror = |i: i64, n: usize| -> usize {
        let n = n as i64;
        if n == 1 {
            return 0;
        }
        let mut i = i;
        while i < 0 || i >= n {
            i = if i < 0 { -i } else { 2 * (n - 1) - i };
        }
        i as usize
    };
    const K: [u32; 5] = [1, 4, 6, 4, 1];
    let mut across = vec![0u32; g.w * g.h];
    for y in 0..g.h {
        for x in 0..g.w {
            across[y * g.w + x] = (0..5).map(|k| K[k] * g.data[y * g.w + mirror(x as i64 + k as i64 - 2, g.w)] as u32).sum();
        }
    }
    let mut data = vec![0u8; g.w * g.h];
    for y in 0..g.h {
        for x in 0..g.w {
            let sum: u32 = (0..5).map(|k| K[k] * across[mirror(y as i64 + k as i64 - 2, g.h) * g.w + x]).sum();
            data[y * g.w + x] = ((sum + 128) >> 8) as u8;
        }
    }
    Grey { w: g.w, h: g.h, data }
}

/// A frame as a small blurred grey copy, `width` wide.
pub fn grey_copy(rgb: &[u8], w: usize, h: usize, width: usize) -> Grey {
    let step = (w / (width * 2)).max(1);
    let (sw, sh) = (w.div_ceil(step), h.div_ceil(step));
    let mut small = Vec::with_capacity(sw * sh * 3);
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            small.extend_from_slice(&rgb[(y * w + x) * 3..(y * w + x) * 3 + 3]);
        }
    }
    let size = (width, (round_half_even(h as f64 * width as f64 / w as f64) as usize).max(1));
    let resized = resize_area(&small, sw, sh, 3, size.0, size.1);
    let data = resized.as_chunks::<3>().0.iter().map(|p| grey_of(p[0], p[1], p[2])).collect();
    blur5(&Grey { w: size.0, h: size.1, data })
}

/// The frame as a small blurred grey copy; None for a frame that can't be read as a picture.
pub fn grey(frame: &Frame) -> Option<Grey> {
    Some(grey_copy(&frame_rgb(frame)?, frame.width as usize, frame.height as usize, WIDTH))
}

/// cv2.flip and cv2.rotate by a view's flips and turn: the small picture turned the way the picture is shown.
fn turned(g: Grey, turn: Turn) -> Grey {
    let mut g = g;
    let (w, h) = (g.w, g.h);
    if turn.flip_h || turn.flip_v {
        let mut data = vec![0u8; g.data.len()];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (if turn.flip_h { w - 1 - x } else { x }, if turn.flip_v { h - 1 - y } else { y });
                data[y * w + x] = g.data[sy * w + sx];
            }
        }
        g.data = data;
    }
    match turn.rotation {
        Some(90) | Some(270) => {
            let mut data = vec![0u8; g.data.len()];
            for y in 0..h {
                for x in 0..w {
                    let (nx, ny) = if turn.rotation == Some(90) { (h - 1 - y, x) } else { (y, w - 1 - x) };
                    data[ny * h + nx] = g.data[y * w + x];
                }
            }
            Grey { w: h, h: w, data }
        }
        Some(180) => {
            g.data.reverse();
            g
        }
        _ => g,
    }
}

/// Whether every cell of a CUT_GRID grid has more than CUT of its pixels changed by a lot.
fn is_cut(diff: &[f32], w: usize, h: usize) -> bool {
    let changed = |v: &f32| *v as f64 > LEVELS.0;
    if h < CUT_GRID || w < CUT_GRID {
        return diff.iter().filter(|v| changed(v)).count() as f64 > CUT * diff.len() as f64;
    }
    for r in 0..CUT_GRID {
        for c in 0..CUT_GRID {
            let (y0, y1, x0, x1) = (r * h / CUT_GRID, (r + 1) * h / CUT_GRID, c * w / CUT_GRID, (c + 1) * w / CUT_GRID);
            let size = (y1 - y0) * (x1 - x0);
            let count = (y0..y1).flat_map(|y| diff[y * w + x0..y * w + x1].iter()).filter(|v| changed(v)).count();
            if count as f64 <= CUT * size as f64 {
                return false;
            }
        }
    }
    true
}

/// One stream's background, and how far each pixel of a frame is from it.
#[derive(Default)]
pub struct Detector {
    background: Option<(usize, usize, Vec<f32>)>,
    at: f64,
}

impl Detector {
    pub fn new() -> Detector {
        Detector::default()
    }

    /// How many grey levels each pixel of `small` differs from the background, or None for the first frame or a new size. The whole
    /// picture getting brighter or darker (the camera's exposure, a cloud) is taken out first, and the background then takes in a little
    /// of this frame. Everything is single precision, as numpy does it.
    pub fn change(&mut self, small: &Grey, now: f64) -> Option<Vec<u8>> {
        let frame: Vec<f32> = small.data.iter().map(|&v| v as f32).collect();
        let Some((w, h, background)) = self.background.as_ref().filter(|(w, h, _)| (*w, *h) == (small.w, small.h)) else {
            self.background = Some((small.w, small.h, frame));
            self.at = now;
            return None;
        };
        let (w, h) = (*w, *h);
        // The median ratio, so a dark coat filling part of the picture doesn't count as the room getting darker
        let mut ratios: Vec<f32> = (0..h).step_by(2).flat_map(|y| (0..w).step_by(2).map(move |x| (x, y)))
            .map(|(x, y)| frame[y * w + x] / background[y * w + x].max(1.0)).collect();
        ratios.sort_by(f32::total_cmp);
        let n = ratios.len();
        let lit = if n % 2 == 1 { ratios[n / 2] } else { (ratios[n / 2 - 1] + ratios[n / 2]) / 2.0 };
        let diff: Vec<f32> = frame.iter().zip(background).map(|(f, b)| (f - b * lit).abs()).collect();
        if is_cut(&diff, w, h) {
            self.background = Some((w, h, frame));
            self.at = now;
            return None;
        }
        let a = 1.0 - (-(now - self.at).max(0.0) / ADAPT_S).exp();
        let (keep, take) = ((1.0 - a) as f32, a as f32);
        let next: Vec<f32> = frame.iter().zip(background).map(|(f, b)| b * lit * keep + f * take).collect();
        self.background = Some((w, h, next));
        self.at = now;
        Some(diff.iter().map(|v| v.min(255.0) as u8).collect())
    }

    pub fn reset(&mut self) {
        self.background = None;
    }
}

/// The indexes of the zones (0..1 of the change picture) where enough of it is over `level`.
pub fn moving_zones(change: &[u8], w: usize, h: usize, zones: &[Zone], level: f64, share: f64) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    for (i, &(x0, y0, x1, y1)) in zones.iter().enumerate() {
        let (c0, c1) = ((x0 * w as f64) as usize, ((x1 * w as f64).ceil() as usize).max((x0 * w as f64) as usize + 1));
        let (r0, r1) = ((y0 * h as f64) as usize, ((y1 * h as f64).ceil() as usize).max((y0 * h as f64) as usize + 1));
        let (c1, r1) = (c1.min(w), r1.min(h));
        let (cols, rows) = (c1.saturating_sub(c0), r1.saturating_sub(r0));
        let size = cols * rows;
        if size == 0 {
            continue;
        }
        let over = (r0..r1).flat_map(|y| change[y * w + c0..y * w + c1].iter()).filter(|&&v| v as f64 > level).count();
        if over as f64 >= (MIN_PIXELS as f64).max(share * size as f64) {
            out.insert(i);
        }
    }
    out
}

/// zones from the picture as shown at zoom 1 onto the upright frame `to_shown` maps from (the phone's share of the zoom moves and
/// scales it, without turning it).
pub fn to_frame(zones: &[Zone], to_shown: Option<&dyn Fn(f64, f64) -> (f64, f64)>) -> Vec<Zone> {
    let Some(to_shown) = to_shown else { return zones.to_vec() };
    let ((ax, ay), (bx, by)) = (to_shown(0.0, 0.0), to_shown(1.0, 1.0));
    let (sx, sy) = (bx - ax, by - ay);
    if sx.abs() < 1e-9 || sy.abs() < 1e-9 {
        return zones.to_vec();
    }
    zones.iter().map(|&(x0, y0, x1, y1)| {
        let mapped = [(x0 - ax) / sx, (y0 - ay) / sy, (x1 - ax) / sx, (y1 - ay) / sy];
        // Off the frame: a zone that never moves
        clean_zones(&[mapped]).first().copied().unwrap_or((0.0, 0.0, 0.0, 0.0))
    }).collect()
}

struct Watch {
    zones: Vec<Zone>,
    level: f64,
    share: f64,
}

#[derive(Default)]
struct Stream {
    detector: Detector,
    at: f64,
    /// owner: {zone index: when it last moved}
    last_moved: HashMap<String, BTreeMap<usize, f64>>,
    /// owner: [zone indexes], as last reported
    moving: HashMap<String, Vec<usize>>,
}

/// Called from a stream's worker thread when an owner's moving zones change: (stream key, owner, zone indexes).
pub type OnChange = Box<dyn Fn(&str, &str, &[u32]) + Send + Sync>;

/// Watches the shown stream's frames for the owners that ask; every call is safe from its own thread.
pub struct Motion {
    on_change: OnChange,
    /// owner: its zones and thresholds, swapped whole (read on worker threads)
    watches: Mutex<Arc<BTreeMap<String, Watch>>>,
    streams: Mutex<HashMap<String, Arc<Mutex<Stream>>>>,
    /// Until this time frames only take over the background
    quiet_until: Mutex<f64>,
    ms: Mutex<Vec<f64>>,
}

impl Motion {
    pub fn new(on_change: OnChange) -> Motion {
        Motion { on_change, watches: Mutex::new(Arc::new(BTreeMap::new())), streams: Mutex::default(), quiet_until: Mutex::new(f64::NEG_INFINITY), ms: Mutex::default() }
    }

    /// Looks for motion in zones for owner, or stops with no zones. Each call starts owner over (nothing moving).
    pub fn watch(&self, owner: &str, zones: &[[f64; 4]], sensitivity: i64) {
        let zones = clean_zones(zones);
        let mut watches = self.watches.lock().unwrap();
        let nobody_watched = watches.is_empty();
        let mut next: BTreeMap<String, Watch> = watches.iter().map(|(k, w)| (k.clone(), Watch { zones: w.zones.clone(), level: w.level, share: w.share })).collect();
        if zones.is_empty() {
            next.remove(owner);
        } else {
            let (level, share) = thresholds(sensitivity);
            next.insert(owner.to_string(), Watch { zones, level, share });
        }
        for stream in self.streams.lock().unwrap().values() {
            let mut sv = stream.lock().unwrap();
            sv.moving.remove(owner);
            sv.last_moved.remove(owner);
            if nobody_watched {
                // Nobody watched: the background is from long ago
                sv.detector.reset();
            }
        }
        *watches = Arc::new(next);
    }

    /// Every stream's background starts over (the picture turned, so everything is somewhere else), and keeps following the picture for
    /// settle_s (another lens: its first frames are still on their way).
    /// When changes count as motion again.
    pub fn quiet_until(&self) -> f64 {
        *self.quiet_until.lock().unwrap()
    }

    pub fn reset(&self, settle_s: f64, now: f64) {
        if settle_s > 0.0 {
            let mut quiet = self.quiet_until.lock().unwrap();
            *quiet = quiet.max(now + settle_s);
        }
        for stream in self.streams.lock().unwrap().values() {
            stream.lock().unwrap().detector.reset();
        }
    }

    pub fn watching(&self) -> bool {
        !self.watches.lock().unwrap().is_empty()
    }

    /// The zones something moved in lately on a stream, for owner.
    pub fn moving(&self, key: &str, owner: &str) -> Vec<u32> {
        let stream = self.streams.lock().unwrap().get(key).cloned();
        stream.and_then(|s| s.lock().unwrap().moving.get(owner).map(|m| m.iter().map(|&i| i as u32).collect())).unwrap_or_default()
    }

    /// A stream's frame before the view (worker thread): one every EVERY_S is looked at while anyone watches.
    pub fn feed(&self, frame: &Frame, key: &str, upright: Option<&Upright>, now: f64) {
        let to_shown = upright.map(|u| move |x: f64, y: f64| u.to_shown(x, y));
        let turn = upright.map(|u| u.turn).unwrap_or_default();
        self.feed_with(frame, key, turn, to_shown.as_ref().map(|f| f as &dyn Fn(f64, f64) -> (f64, f64)), now);
    }

    /// feed with the turn and the mapping of points given directly.
    pub fn feed_with(&self, frame: &Frame, key: &str, turn: Turn, to_shown: Option<&dyn Fn(f64, f64) -> (f64, f64)>, now: f64) {
        let watches = self.watches.lock().unwrap().clone();
        if watches.is_empty() {
            return;
        }
        let stream = self.streams.lock().unwrap().entry(key.to_string()).or_default().clone();
        let mut sv = stream.lock().unwrap();
        if now - sv.at < EVERY_S {
            return;
        }
        if now - sv.at > GAP_S || now < *self.quiet_until.lock().unwrap() {
            sv.detector.reset();
        }
        sv.at = now;
        let started = std::time::Instant::now();
        let Some(small) = grey(frame) else { return };
        let small = turned(small, turn);
        let Some(change) = sv.detector.change(&small, now) else { return };
        let mut reports = Vec::new();
        for (owner, watch) in watches.iter() {
            let last = sv.last_moved.entry(owner.clone()).or_default();
            for i in moving_zones(&change, small.w, small.h, &to_frame(&watch.zones, to_shown), watch.level, watch.share) {
                last.insert(i, now);
            }
            let moving: Vec<usize> = last.iter().filter(|(i, at)| now - **at < HOLD_S && **i < watch.zones.len()).map(|(i, _)| *i).collect();
            if moving != sv.moving.get(owner).cloned().unwrap_or_default() {
                sv.moving.insert(owner.clone(), moving.clone());
                reports.push((owner.clone(), moving));
            }
        }
        let gone: Vec<String> = sv.moving.keys().filter(|o| !watches.contains_key(*o)).cloned().collect();
        for owner in gone {
            sv.moving.remove(&owner);
            sv.last_moved.remove(&owner);
        }
        drop(sv);
        let mut ms = self.ms.lock().unwrap();
        ms.push(started.elapsed().as_secs_f64() * 1000.0);
        let extra = ms.len().saturating_sub(60);
        ms.drain(..extra);
        drop(ms);
        for (owner, moving) in reports {
            let zones: Vec<u32> = moving.iter().map(|&i| i as u32).collect();
            (self.on_change)(key, &owner, &zones);
        }
    }

    pub fn drop_stream(&self, key: &str) {
        self.streams.lock().unwrap().remove(key);
    }

    /// What goes in Copy diagnostics: how many watch and how long a look takes, or nothing while nobody does.
    pub fn diagnostics(&self) -> Option<String> {
        let watching = self.watches.lock().unwrap().len();
        let mut ms = self.ms.lock().unwrap().clone();
        if watching == 0 || ms.is_empty() {
            return None;
        }
        ms.sort_by(f64::total_cmp);
        Some(format!("{watching} watching, {:.1} ms", ms[ms.len() / 2]))
    }
}
