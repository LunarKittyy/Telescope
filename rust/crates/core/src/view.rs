//! Zoom, pan, flip, rotation and stretch of a stream's picture, and how the zoom is shared with the phone (view.py's
//! geometry; the pixel work is the frame pipeline's). Rotation is in degrees clockwise: 90, 180 or 270.
//! Moved here from the shell: Transforms, Framing and the window's view step all need the same geometry.

use crate::host::View;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Rotation's choices by the names saved settings and shortcuts use (view.ROTATIONS), with the degrees clockwise View holds.
pub const ROTATIONS: [(&str, Option<i32>); 4] = [("None", None), ("90 CW", Some(90)), ("180", Some(180)), ("90 CCW", Some(270))];

/// The degrees a rotation name stands for; None (the outer one) when the name isn't one of ROTATIONS'.
pub fn rotation_named(name: &str) -> Option<Option<i32>> {
    ROTATIONS.iter().find(|(n, _)| *n == name).map(|(_, r)| *r)
}

/// The name saved settings use for a rotation in degrees ("None" for anything that isn't a turn).
pub fn rotation_name(rotation: Option<i32>) -> &'static str {
    ROTATIONS.iter().find(|(_, r)| *r == rotation).map_or("None", |(n, _)| n)
}

/// How hard a full Lens correction slider bends the picture: the lens model is r_source = r * (1 - c * r^2) with r in half
/// diagonals from the centre and c = amount * this.
pub const LENS_STRENGTH: f64 = 0.3;

/// Lens correction's mapping, shared by the remap step (view_step) and everything that has to find a point through it: the picture the
/// correction makes (the output) against the picture as the lens drew it (the source), both 0..1 of the whole frame. Only the amount
/// and the source's width over height matter, not its size in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lens {
    c: f64,
    scale: f64,
    aspect: f64,
}

impl Lens {
    pub fn new(amount: f64, aspect: f64) -> Lens {
        let mut lens = Lens { c: amount * LENS_STRENGTH, scale: 1.0, aspect: if aspect > 0.0 { aspect } else { 1.0 } };
        lens.scale = lens.fitted_scale();
        lens
    }

    /// What the output is scaled by (1 = as it is) so that every edge is filled with picture: the largest scale at which the whole
    /// border of the output still reads from inside the source. Straightening a barrel loses the outer ring and a pincushion
    /// needs the output to pull in, since it would reach past the corners.
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// Half the frame's width and height, in units of its height.
    fn half(&self) -> (f64, f64) {
        (self.aspect / 2.0, 0.5)
    }

    fn fitted_scale(&self) -> f64 {
        let c = self.c;
        let (hw, hh) = self.half();
        let r = hw.hypot(hh);
        let border: Vec<(f64, f64)> = (0..=64).flat_map(|i| {
            let t = i as f64 / 64.0 * 2.0 - 1.0;
            [(t * hw, hh), (t * hw, -hh), (hw, t * hh), (-hw, t * hh)]
        }).collect();
        let fits = |s: f64| border.iter().all(|&(dx, dy)| {
            let k = s * (1.0 - c * (s * s) * (dx * dx + dy * dy) / (r * r));
            (dx * k).abs() <= hw + 1e-6 && (dy * k).abs() <= hh + 1e-6
        });
        // Past this a barrel's mapping folds back on itself
        let top = if c > 0.0 { (1.0 / (3.0 * c)).sqrt() * 0.999 } else { 1.5 }.min(1.5);
        let (mut good, mut bad) = if fits(1.0) { (1.0, top) } else { (0.2, 1.0) };
        if good == 1.0 && fits(top) {
            return top;
        }
        for _ in 0..30 {
            let mid = (good + bad) / 2.0;
            if fits(mid) { good = mid } else { bad = mid }
        }
        good
    }

    /// Where in the source an output point reads: its offset from the centre, scaled by one factor that depends on how far out it is.
    pub fn forward(&self, x: f64, y: f64) -> (f64, f64) {
        let (hw, hh) = self.half();
        let (dx, dy) = ((x - 0.5) * self.aspect, y - 0.5);
        let k = self.scale * (1.0 - self.c * self.scale * self.scale * (dx * dx + dy * dy) / (hw * hw + hh * hh));
        (0.5 + (x - 0.5) * k, 0.5 + (y - 0.5) * k)
    }

    /// The reverse of forward: where a point of the source (as the lens drew it) lands in the output. The radius is found by bisection
    /// on the rising part of the curve, so a point the output never reads (past a barrel's fold) lands on the fold's radius.
    pub fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let (hw, hh) = self.half();
        let (ex, ey) = ((x - 0.5) * self.aspect, y - 0.5);
        let want = ex.hypot(ey);
        if want < 1e-12 {
            return (x, y);
        }
        let big_r2 = hw * hw + hh * hh;
        // The source radius an output radius r reads from
        let at = |r: f64| self.scale * r * (1.0 - self.c * self.scale * self.scale * r * r / big_r2);
        let top = if self.c > 0.0 { (big_r2 / (3.0 * self.c)).sqrt() / self.scale } else { want / self.scale + 1e-9 };
        let (mut lo, mut hi) = (0.0, top);
        if at(top) <= want {
            lo = top;
        } else {
            for _ in 0..60 {
                let mid = (lo + hi) / 2.0;
                if at(mid) < want { lo = mid } else { hi = mid }
            }
        }
        let k = lo / want;
        (0.5 + (x - 0.5) * k, 0.5 + (y - 0.5) * k)
    }
}

/// A lens is taken only once the window sits this far inside its view (no flip-flopping).
const LENS_MARGIN: f64 = 1.05;

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi)
}

/// Where zoom_origin puts the crop's centre along one axis, 0..1.
pub fn window_centre(zoom: f64, pan: f64) -> f64 {
    0.5 + pan * (1.0 - 1.0 / zoom) / 2.0
}

/// The output's window along one axis of the full view: (left edge, size), both 0..1.
pub fn window(zoom: f64, pan: f64) -> (f64, f64) {
    let zoom = zoom.max(1.0);
    (window_centre(zoom, pan) - 0.5 / zoom, 1.0 / zoom)
}

/// The crop apply_zoom takes: (x0, y0, crop_w, crop_h) in source pixels, fractional.
pub fn zoom_origin(w: f64, h: f64, zoom: f64, pan_x: f64, pan_y: f64) -> (f64, f64, f64, f64) {
    let zoom = zoom.max(1.0);
    let ((x0, sx), (y0, sy)) = (window(zoom, clamp(pan_x, -1.0, 1.0)), window(zoom, clamp(pan_y, -1.0, 1.0)));
    (x0 * w, y0 * h, sx * w, sy * h)
}

/// A point in the stretched picture (0..1) to where it was before the stretch moved it.
pub fn unstretch(u: f64, v: f64, sx: f64, sy: f64) -> (f64, f64) {
    (0.5 + (u - 0.5) / sx, 0.5 + (v - 0.5) / sy)
}

fn unturn(u: f64, v: f64, rotation: Option<i32>) -> (f64, f64) {
    match rotation {
        Some(90) => (v, 1.0 - u),
        Some(180) => (1.0 - u, 1.0 - v),
        Some(270) => (1.0 - v, u),
        _ => (u, v),
    }
}

fn turn(u: f64, v: f64, rotation: Option<i32>) -> (f64, f64) {
    match rotation {
        Some(90) => (1.0 - v, u),
        Some(180) => (1.0 - u, 1.0 - v),
        Some(270) => (v, 1.0 - u),
        _ => (u, v),
    }
}

/// The view's settings as plain numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub zoom: f64,
    pub pan_x: f64,
    pub pan_y: f64,
    pub flip_h: bool,
    pub flip_v: bool,
    pub rotation: Option<i32>,
    pub stretch_x: f64,
    pub stretch_y: f64,
    /// Lens correction's slider (View::distortion); inverse_map and raw_to_view map through it.
    pub distortion: f64,
}

impl Default for Geometry {
    fn default() -> Self {
        Geometry::from(&View::default())
    }
}

impl From<&View> for Geometry {
    fn from(v: &View) -> Self {
        Geometry {
            zoom: v.zoom as f64, pan_x: v.pan_x as f64, pan_y: v.pan_y as f64, flip_h: v.flip_h, flip_v: v.flip_v,
            rotation: v.rotation, stretch_x: v.stretch_x as f64, stretch_y: v.stretch_y as f64, distortion: v.distortion as f64,
        }
    }
}

/// A point in the frame as shown (after crop, flip, rotation and stretch) to the full view of the main lens.
/// Continuous and unclamped, and by the zoom asked for.
pub fn shown_to_view(u: f64, v: f64, g: &Geometry) -> (f64, f64) {
    let (u, v) = unstretch(u, v, g.stretch_x, g.stretch_y);
    let (mut u, mut v) = unturn(u, v, g.rotation);
    if g.flip_h {
        u = 1.0 - u;
    }
    if g.flip_v {
        v = 1.0 - v;
    }
    let ((x0, w), (y0, h)) = (window(g.zoom, g.pan_x), window(g.zoom, g.pan_y));
    (x0 + u * w, y0 + v * h)
}

/// The reverse of shown_to_view.
pub fn view_to_shown(x: f64, y: f64, g: &Geometry) -> (f64, f64) {
    let ((x0, w), (y0, h)) = (window(g.zoom, g.pan_x), window(g.zoom, g.pan_y));
    let (mut u, mut v) = ((x - x0) / w, (y - y0) / h);
    if g.flip_h {
        u = 1.0 - u;
    }
    if g.flip_v {
        v = 1.0 - v;
    }
    let (u, v) = turn(u, v, g.rotation);
    (0.5 + (u - 0.5) * g.stretch_x, 0.5 + (v - 0.5) * g.stretch_y)
}

/// A point in the transformed frame back to the phone's frame of w x h pixels, clamped (the pixel-exact one, for
/// focus).
pub fn inverse_map(u: f64, v: f64, w: f64, h: f64, g: &Geometry) -> (f64, f64) {
    let (u, v) = unstretch(u, v, g.stretch_x, g.stretch_y);
    let (mut u, mut v) = unturn(u, v, g.rotation);
    if g.flip_h {
        u = 1.0 - u;
    }
    if g.flip_v {
        v = 1.0 - v;
    }
    if g.zoom > 1.0 && w > 0.0 && h > 0.0 {
        let (x0, y0, cw, ch) = zoom_origin(w, h, g.zoom, g.pan_x, g.pan_y);
        u = (x0 + u * cw) / w;
        v = (y0 + v * ch) / h;
    }
    // The crop was of what Lens correction made, so the lens's own reading comes last of all
    if g.distortion.abs() >= 1e-6 && w > 0.0 && h > 0.0 {
        (u, v) = Lens::new(g.distortion, w / h).forward(u, v);
    }
    (clamp(u, 0.0, 1.0), clamp(v, 0.0, 1.0))
}

/// A point of a frame as the lens drew it (before Lens correction, turned and flipped the way g shows it) to the full view, with
/// the frame's own width over height (before any turn). Without lens correction in g this is shown_to_view.
pub fn raw_to_view(u: f64, v: f64, aspect: f64, g: &Geometry) -> (f64, f64) {
    let (u, v) = unstretch(u, v, g.stretch_x, g.stretch_y);
    let (mut u, mut v) = unturn(u, v, g.rotation);
    if g.flip_h {
        u = 1.0 - u;
    }
    if g.flip_v {
        v = 1.0 - v;
    }
    if g.distortion.abs() >= 1e-6 && aspect > 0.0 {
        (u, v) = Lens::new(g.distortion, aspect).inverse(u, v);
    }
    let ((x0, w), (y0, h)) = (window(g.zoom, g.pan_x), window(g.zoom, g.pan_y));
    (x0 + u * w, y0 + v * h)
}

/// A point in the picture as shown at zoom 1 and no stretch to where it lands in the frame as shown with g now.
pub fn upright_to_shown(g: &Geometry, x: f64, y: f64) -> (f64, f64) {
    let upright = Geometry { zoom: 1.0, pan_x: 0.0, pan_y: 0.0, stretch_x: 1.0, stretch_y: 1.0, ..*g };
    let (fx, fy) = shown_to_view(x, y, &upright);
    view_to_shown(fx, fy, g)
}

/// The pan that puts the window's centre at `centre` (0..1 of the full view), clamped to -1..1.
pub fn pan_for(centre: f64, zoom: f64) -> f64 {
    let slack = 1.0 - 1.0 / zoom.max(1.0);
    if slack < 1e-9 { 0.0 } else { clamp((centre - 0.5) * 2.0 / slack, -1.0, 1.0) }
}

/// A zoom step (slider values) that stops on the first lens mark it crosses; the next one goes past.
pub fn zoom_detent(old: i32, new: i32, marks: &[i32]) -> i32 {
    if new > old {
        marks.iter().copied().filter(|&m| old < m && m <= new).min().unwrap_or(new)
    } else if new < old {
        marks.iter().copied().filter(|&m| new <= m && m < old).max().unwrap_or(new)
    } else {
        new
    }
}

/// A lens's ratio as a Zoom slider value (x100), rounded up: a hair under the lens wouldn't get it.
pub fn lens_step(lens: f64) -> i32 {
    (lens * 100.0 - 1e-6).ceil() as i32
}

/// How far the current lens can zoom itself, from its /v1/state entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhoneZoomCaps {
    pub ratio_max: f64,
    pub crop_max: f64,
    pub freeform: bool,
    pub lens_zooms: Vec<f64>,
}

impl PhoneZoomCaps {
    /// None when the lens can't zoom itself (or the phone app predates phone-side zoom).
    pub fn from_camera(cam: Option<&Value>) -> Option<PhoneZoomCaps> {
        let cam = cam.filter(|c| c.as_object().is_some_and(|o| !o.is_empty()))?;
        let num = |k: &str| cam.get(k).and_then(Value::as_f64).filter(|v| *v != 0.0).unwrap_or(1.0);
        let ratio_max = num("zoomRatioMax").max(1.0);
        let mut lenses: Vec<f64> = cam.get("lensZooms").and_then(Value::as_array).into_iter().flatten()
            .filter_map(Value::as_f64).filter(|&z| 1.0 < z && z <= ratio_max).collect();
        lenses.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let caps = PhoneZoomCaps { ratio_max, crop_max: num("cropZoomMax").max(1.0), freeform: cam.get("freeformCrop") == Some(&Value::Bool(true)), lens_zooms: lenses };
        (caps.ratio_max > 1.0 || caps.crop_max > 1.0).then_some(caps)
    }
}

/// The phone's `zoom` control: a centred ratio, then a 1/crop box around (x, y) of that view (0..1).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhoneZoom {
    pub ratio: f64,
    pub crop: f64,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoomSplit {
    /// None: this lens can't zoom itself, so the phone is left alone.
    pub phone: Option<PhoneZoom>,
    /// (zoom, pan_x, pan_y) still to crop from the frame that arrives.
    pub desktop: (f64, f64, f64),
}

fn pan_within(centre: f64, view_centre: f64, view: f64, size: f64) -> f64 {
    let slack = (view - size) / 2.0;
    if slack < 1e-6 { 0.0 } else { clamp((centre - view_centre) / slack, -1.0, 1.0) }
}

fn lens_ratio(fit: f64, caps: &PhoneZoomCaps, current: f64, zoom: f64) -> f64 {
    let mut ratio = 1.0;
    for &lens in &caps.lens_zooms {
        let on_it = (lens - current).abs() < 1e-3;
        let need = if on_it { lens } else { lens.max((lens * LENS_MARGIN).min(zoom)) };
        if fit * (1.0 + 1e-9) >= need {
            ratio = lens;
        }
    }
    ratio
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

/// The framing asked for, as the phone's share plus what's left for this computer (split_zoom).
pub fn split_zoom(zoom: f64, pan_x: f64, pan_y: f64, caps: Option<&PhoneZoomCaps>, current_ratio: f64) -> ZoomSplit {
    let Some(caps) = caps else { return ZoomSplit { phone: None, desktop: (zoom, pan_x, pan_y) } };
    let zoom = zoom.max(1.0);
    let size = 1.0 / zoom;
    let centre = (window_centre(zoom, pan_x), window_centre(zoom, pan_y));
    let fit = 1.0 / (2.0 * (centre.0 - 0.5).abs().max((centre.1 - 0.5).abs()) + size);
    let ratio = lens_ratio(fit, caps, current_ratio, zoom);
    let total = if caps.freeform { ratio.max(zoom.min(ratio * caps.crop_max)) } else { ratio };
    let view = 1.0 / total;
    let room = if caps.freeform { (1.0 / ratio - view) / 2.0 } else { 0.0 };
    let (vx, vy) = (clamp(centre.0, 0.5 - room, 0.5 + room), clamp(centre.1, 0.5 - room, 0.5 + room));
    let phone = PhoneZoom { ratio: round4(ratio), crop: round4(total / ratio), x: round4((vx - 0.5) * ratio + 0.5), y: round4((vy - 0.5) * ratio + 0.5) };
    let rest = zoom / total;
    if rest < 1.0 + 1e-3 {
        return ZoomSplit { phone: Some(phone), desktop: (1.0, 0.0, 0.0) };
    }
    ZoomSplit { phone: Some(phone), desktop: (rest, pan_within(centre.0, vx, view, size), pan_within(centre.1, vy, view, size)) }
}

/// The (zoom, pan_x, pan_y) of the full view that a phone share sends.
pub fn phone_window(phone: Option<&PhoneZoom>) -> (f64, f64, f64) {
    let Some(p) = phone else { return (1.0, 0.0, 0.0) };
    let total = p.ratio * p.crop;
    (total, pan_for(0.5 + (p.x - 0.5) / p.ratio, total), pan_for(0.5 + (p.y - 0.5) / p.ratio, total))
}

/// What this computer crops for the framing asked for when the phone stays on its share; None when it doesn't fit
/// inside what the phone sends. clamp: the nearest framing that does fit instead of None.
pub fn rest_within(zoom: f64, pan_x: f64, pan_y: f64, phone: &PhoneZoom, clamp_it: bool) -> Option<(f64, f64, f64)> {
    let total = phone.ratio * phone.crop;
    let zoom = if clamp_it { zoom.max(total) } else { zoom.max(1.0) };
    let (size, view) = (1.0 / zoom, 1.0 / total);
    let centre = (window_centre(zoom, pan_x), window_centre(zoom, pan_y));
    let vc = (0.5 + (phone.x - 0.5) / phone.ratio, 0.5 + (phone.y - 0.5) / phone.ratio);
    let fits = zoom >= total - 1e-6 && [(centre.0, vc.0), (centre.1, vc.1)].iter().all(|(c, v)| (c - v).abs() + size / 2.0 <= view / 2.0 + 1e-6);
    if !fits && !clamp_it {
        return None;
    }
    let rest = zoom / total;
    if rest < 1.0 + 1e-3 {
        return Some((1.0, 0.0, 0.0));
    }
    Some((rest, pan_within(centre.0, vc.0, view, size), pan_within(centre.1, vc.1, view, size)))
}

/// (scale, dx, dy): where a point at u of a frame cropped to old lands in one cropped to new, u * scale + d.
pub fn share_shift(old: &PhoneZoom, new: &PhoneZoom) -> (f64, f64, f64) {
    let ((oz, opx, opy), (nz, npx, npy)) = (phone_window(Some(old)), phone_window(Some(new)));
    let ((ox, osize), (oy, _)) = (window(oz, opx), window(oz, opy));
    let ((nx, nsize), (ny, _)) = (window(nz, npx), window(nz, npy));
    (osize / nsize, (ox - nx) / nsize, (oy - ny) / nsize)
}

/// How the shown stream's zoom is shared with the phone right now, for the Transforms card's lens dot (the payload of
/// Event::ViewStatus).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewStatus {
    /// A phone is there to tell (false: everything is cropped here).
    pub streaming: bool,
    /// What the current lens can zoom itself.
    pub caps: Option<PhoneZoomCaps>,
    /// What the phone was told last.
    pub sent: Option<PhoneZoom>,
    /// Monotonic seconds when it was last sent a new ratio (crate::time::monotonic_ns / 1e9); 0 before any.
    pub sent_ratio_at: f64,
    /// The zoom share this computer crops.
    pub desktop_zoom: f64,
}

impl Default for ViewStatus {
    fn default() -> Self {
        ViewStatus { streaming: false, caps: None, sent: None, sent_ratio_at: 0.0, desktop_zoom: 1.0 }
    }
}

/// What each longer lens sees, as (label, x0, y0, x1, y1) in the frame as shown (may run off it).
pub fn lens_boxes(caps: Option<&PhoneZoomCaps>, g: &Geometry) -> Vec<(String, f64, f64, f64, f64)> {
    caps.map(|c| c.lens_zooms.clone()).unwrap_or_default().into_iter().map(|lens| {
        let half = 0.5 / lens;
        let corners: Vec<(f64, f64)> = [0.5 - half, 0.5 + half].iter()
            .flat_map(|&x| [0.5 - half, 0.5 + half].map(move |y| (x, y)))
            .map(|(x, y)| view_to_shown(x, y, g)).collect();
        let xs = corners.iter().map(|c| c.0);
        let ys = corners.iter().map(|c| c.1);
        let label = format!("{}×", ((lens * 10.0).round() / 10.0).to_string().trim_end_matches(".0"));
        (label, xs.clone().fold(f64::MAX, f64::min), ys.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max), ys.fold(f64::MIN, f64::max))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn shown_and_view_are_each_others_reverse() {
        for rotation in [None, Some(90), Some(180), Some(270)] {
            let g = Geometry { zoom: 2.5, pan_x: 0.3, pan_y: -0.7, flip_h: true, flip_v: false, rotation, stretch_x: 1.2, stretch_y: 0.9, distortion: 0.0 };
            let p = shown_to_view(0.2, 0.8, &g);
            assert!(close(view_to_shown(p.0, p.1, &g), (0.2, 0.8)), "{rotation:?}");
        }
    }

    #[test]
    fn a_turned_picture_maps_its_corner_where_it_went() {
        let g = Geometry { rotation: Some(90), ..Default::default() };
        // Turning clockwise puts the top left corner at the top right
        assert!(close(view_to_shown(0.0, 0.0, &g), (1.0, 0.0)));
        assert!(close(inverse_map(1.0, 0.0, 100.0, 100.0, &g), (0.0, 0.0)));
    }

    fn grid() -> Vec<(f64, f64)> {
        (0..=10).flat_map(|i| (0..=10).map(move |j| (i as f64 / 10.0, j as f64 / 10.0))).collect()
    }

    #[test]
    fn the_lens_inverse_undoes_its_forward_reading_everywhere_including_the_corners() {
        for aspect in [16.0 / 9.0, 4.0 / 3.0, 1.0, 9.0 / 16.0] {
            for amount in [-1.0, -0.4, -0.05, 0.05, 0.4, 1.0] {
                let lens = Lens::new(amount, aspect);
                for (x, y) in grid() {
                    let (sx, sy) = lens.forward(x, y);
                    assert!(close(lens.inverse(sx, sy), (x, y)), "{aspect} {amount}: ({x}, {y}) read ({sx}, {sy}) came back {:?}", lens.inverse(sx, sy));
                }
            }
        }
    }

    #[test]
    fn a_lens_at_zero_changes_nothing() {
        let lens = Lens::new(0.0, 16.0 / 9.0);
        let near = |a: (f64, f64)| (a.0 - 0.9).abs() < 1e-6 && (a.1 - 0.2).abs() < 1e-6;
        assert!(near(lens.forward(0.9, 0.2)) && near(lens.inverse(0.9, 0.2)));
        assert!((lens.scale() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn a_point_picked_on_a_corrected_preview_is_found_where_the_lens_read_it() {
        let (w, h) = (1920.0, 1080.0);
        let g = Geometry { distortion: 1.0, ..Default::default() };
        let (x, y) = inverse_map(0.95, 0.95, w, h, &g);
        // The review's number: about 0.846, a 229 pixel difference from taking the preview as the phone's frame
        assert!((x - 0.846).abs() < 0.01 && (y - 0.846).abs() < 0.01, "({x}, {y})");
        assert!((0.95 - x) * w > 150.0);
        let lens = Lens::new(1.0, w / h);
        assert!(close(lens.inverse(x, y), (0.95, 0.95)), "and the lens undoes it");
        // No correction, no change
        assert!(close(inverse_map(0.95, 0.95, w, h, &Geometry::default()), (0.95, 0.95)));
    }

    #[test]
    fn picks_at_the_corners_survive_the_round_trip_through_every_turn_flip_and_crop() {
        let (w, h) = (1920.0, 1080.0);
        for rotation in [None, Some(90), Some(180), Some(270)] {
            for flip_h in [false, true] {
                for (zoom, pan) in [(1.0, 0.0), (2.0, 0.6)] {
                    for amount in [-1.0, -0.4, 0.4, 1.0] {
                        let g = Geometry { zoom, pan_x: pan, pan_y: -pan, flip_h, rotation, distortion: amount, ..Default::default() };
                        let flat = Geometry { distortion: 0.0, ..g };
                        for (u, v) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.5), (0.95, 0.95), (0.05, 0.9)] {
                            // The frame the phone sent, undistorted, is the lens's output; the pick reads the lens's source
                            let (sx, sy) = inverse_map(u, v, w, h, &g);
                            let (fx, fy) = Lens::new(amount, w / h).inverse(sx, sy);
                            let (ux, uy) = inverse_map(u, v, w, h, &flat);
                            assert!(close((fx, fy), (ux, uy)) || (ux <= 0.0 || ux >= 1.0 || uy <= 0.0 || uy >= 1.0 || sx <= 0.0 || sx >= 1.0 || sy <= 0.0 || sy >= 1.0), "{g:?} ({u}, {v}): {:?} against {:?}", (fx, fy), (ux, uy));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_point_the_model_finds_in_the_lens_drawn_frame_lands_in_the_corrected_picture() {
        let (w, h) = (1920.0, 1080.0);
        for rotation in [None, Some(90), Some(270)] {
            for flip_v in [false, true] {
                let g = Geometry { flip_v, rotation, distortion: 0.8, ..Default::default() };
                let flat = Geometry { distortion: 0.0, ..g };
                for (u, v) in [(0.1, 0.1), (0.9, 0.2), (0.95, 0.95), (0.5, 0.3)] {
                    // Where the shown point u, v is in the phone's frame, then turned the way the model gets it
                    let (rx, ry) = inverse_map(u, v, w, h, &g);
                    let (qx, qy) = view_to_shown(rx, ry, &flat);
                    let up = crate::vision::Upright { turn: crate::vision::Turn { flip_h: false, flip_v, rotation }, distortion: 0.8, ..Default::default() }.for_frame(1920, 1080);
                    assert!(close(up.to_shown(qx, qy), (u, v)), "{rotation:?} {flip_v} ({u}, {v}): {:?}", up.to_shown(qx, qy));
                    // Without the frame's size there is nothing to undo
                    let blind = crate::vision::Upright { aspect: 0.0, ..up };
                    assert!(!close(blind.to_shown(qx, qy), (u, v)) || (u - 0.5).abs() + (v - 0.5).abs() < 1e-9);
                }
            }
        }
    }

    #[test]
    fn a_phone_crop_is_undone_after_the_lens_in_the_frame_it_sent() {
        let sent = (2.0, 0.4, -0.3);
        let (x0, wx) = window(sent.0, sent.1);
        let (y0, wy) = window(sent.0, sent.2);
        let lens = Lens::new(0.6, 16.0 / 9.0);
        for (fx, fy) in [(0.1, 0.1), (0.9, 0.8), (0.5, 0.5)] {
            // A point of the corrected frame the phone sent, in the lens's source, found by the model there
            let (rx, ry) = lens.forward(fx, fy);
            let up = crate::vision::Upright { sent, distortion: 0.6, ..Default::default() }.for_frame(1920, 1080);
            let (px, py) = up.to_shown(rx, ry);
            assert!(close((px, py), (x0 + fx * wx, y0 + fy * wy)), "({fx}, {fy}) -> ({px}, {py})");
        }
    }

    #[test]
    fn pan_puts_the_window_where_asked() {
        assert_eq!(pan_for(0.5, 2.0), 0.0);
        assert_eq!(pan_for(0.75, 2.0), 1.0);
        assert_eq!(pan_for(0.9, 2.0), 1.0, "it clamps");
        assert!((window_centre(2.0, pan_for(0.6, 2.0)) - 0.6).abs() < 1e-9);
        assert_eq!(pan_for(0.9, 1.0), 0.0);
        assert_eq!(pan_for(0.6, 1.0), 0.0, "nothing to pan at 1x");
    }

    #[test]
    fn zoom_steps_stop_on_a_lens_once() {
        assert_eq!(zoom_detent(100, 400, &[200, 300]), 200);
        assert_eq!(zoom_detent(200, 400, &[200, 300]), 300);
        assert_eq!(zoom_detent(400, 100, &[200, 300]), 300);
        assert_eq!(lens_step(2.0), 200);
        assert_eq!(lens_step(2.501), 251);
    }

    #[test]
    fn the_phone_takes_its_lens_and_this_computer_the_rest() {
        let caps = PhoneZoomCaps::from_camera(Some(&json!({"zoomRatioMax": 10.0, "lensZooms": [3.0], "cropZoomMax": 1.0}))).unwrap();
        let split = split_zoom(4.0, 0.0, 0.0, Some(&caps), 1.0);
        assert_eq!(split.phone.unwrap().ratio, 3.0);
        assert!((split.desktop.0 - 4.0 / 3.0).abs() < 1e-9);
        assert_eq!(split_zoom(2.0, 0.0, 0.0, None, 1.0).desktop, (2.0, 0.0, 0.0));
        assert!(PhoneZoomCaps::from_camera(Some(&json!({"zoomRatioMax": 1.0}))).is_none());
    }

    #[test]
    fn a_framing_inside_the_phones_share_is_cropped_here() {
        let phone = PhoneZoom { ratio: 2.0, crop: 1.0, x: 0.5, y: 0.5 };
        assert_eq!(rest_within(2.0, 0.0, 0.0, &phone, false), Some((1.0, 0.0, 0.0)));
        assert!(rest_within(1.5, 0.0, 0.0, &phone, false).is_none());
        assert_eq!(share_shift(&phone, &phone), (1.0, 0.0, 0.0));
    }

    #[test]
    fn lens_boxes_show_what_a_longer_lens_sees() {
        let caps = PhoneZoomCaps { ratio_max: 5.0, crop_max: 1.0, freeform: false, lens_zooms: vec![2.0, 5.0] };
        let boxes = lens_boxes(Some(&caps), &Geometry::default());
        assert_eq!(boxes[0].0, "2×");
        assert!((boxes[0].1 - 0.25).abs() < 1e-9 && (boxes[0].4 - 0.75).abs() < 1e-9);
    }

    fn tele() -> PhoneZoomCaps {
        PhoneZoomCaps { ratio_max: 10.0, crop_max: 4.0, freeform: true, lens_zooms: vec![3.7] }
    }

    fn centre_only() -> PhoneZoomCaps {
        PhoneZoomCaps { freeform: false, ..tele() }
    }

    fn crop_only() -> PhoneZoomCaps {
        PhoneZoomCaps { ratio_max: 1.0, crop_max: 2.0, freeform: true, lens_zooms: Vec::new() }
    }

    fn one_lens() -> PhoneZoomCaps {
        PhoneZoomCaps { lens_zooms: Vec::new(), ..tele() }
    }

    /// (size, centre x, centre y) the phone's crop and the desktop's remainder end up showing, 0..1.
    fn framing(split: &ZoomSplit) -> (f64, f64, f64) {
        let phone = split.phone.unwrap_or(PhoneZoom { ratio: 1.0, crop: 1.0, x: 0.5, y: 0.5 });
        let view = 1.0 / (phone.ratio * phone.crop);
        let (vx, vy) = (0.5 + (phone.x - 0.5) / phone.ratio, 0.5 + (phone.y - 0.5) / phone.ratio);
        let (zoom, pan_x, pan_y) = split.desktop;
        let step = view * (1.0 - 1.0 / zoom) / 2.0;
        (view / zoom, vx + pan_x * step, vy + pan_y * step)
    }

    #[test]
    fn split_zoom_always_frames_what_was_asked() {
        let small = PhoneZoomCaps { ratio_max: 1.5, crop_max: 1.0, freeform: false, lens_zooms: Vec::new() };
        let all: [Option<PhoneZoomCaps>; 6] = [None, Some(tele()), Some(centre_only()), Some(crop_only()), Some(one_lens()), Some(small)];
        for caps in &all {
            for (zoom, (pan_x, pan_y)) in [(1.0, (0.0, 0.0)), (2.0, (0.0, 0.0)), (3.0, (1.0, 0.0)), (5.0, (-0.4, 0.8)), (2.5, (1.0, -1.0)), (4.5, (0.0, 0.0)), (5.0, (0.3, -0.2))] {
                for current in [1.0, 3.7] {
                    let (size, cx, cy) = framing(&split_zoom(zoom, pan_x, pan_y, caps.as_ref(), current));
                    let what = format!("{caps:?} {zoom} {pan_x} {pan_y} {current}");
                    assert!((size - 1.0 / zoom).abs() < 1e-3, "{what}: {size}");
                    assert!((cx - (0.5 + pan_x * (1.0 - 1.0 / zoom) / 2.0)).abs() < 1e-3, "{what}: {cx}");
                    assert!((cy - (0.5 + pan_y * (1.0 - 1.0 / zoom) / 2.0)).abs() < 1e-3, "{what}: {cy}");
                }
            }
        }
    }

    #[test]
    fn a_lens_that_cant_zoom_leaves_it_all_to_the_desktop() {
        let split = split_zoom(2.5, 0.3, -0.2, None, 1.0);
        assert!(split.phone.is_none());
        assert_eq!(split.desktop, (2.5, 0.3, -0.2));
    }

    #[test]
    fn below_the_telephoto_the_crop_zooms_and_the_ratio_stays_on_the_main_lens() {
        let split = split_zoom(3.0, 0.0, 0.0, Some(&tele()), 1.0);
        assert_eq!(split.phone, Some(PhoneZoom { ratio: 1.0, crop: 3.0, x: 0.5, y: 0.5 }));
        assert_eq!(split.desktop, (1.0, 0.0, 0.0));
    }

    #[test]
    fn past_the_telephoto_the_ratio_switches_lens_and_the_crop_does_the_rest() {
        let split = split_zoom(5.0, 0.0, 0.0, Some(&tele()), 1.0);
        let phone = split.phone.unwrap();
        assert_eq!(phone.ratio, 3.7);
        assert!((phone.crop - 5.0 / 3.7).abs() < 1e-3);
        assert_eq!(split.desktop, (1.0, 0.0, 0.0));
    }

    #[test]
    fn panning_out_of_the_telephotos_view_goes_back_to_the_main_lens() {
        let phone = split_zoom(5.0, 1.0, 0.0, Some(&tele()), 3.7).phone.unwrap();
        assert_eq!(phone.ratio, 1.0);
        assert!((phone.crop - 4.0).abs() < 1e-9, "as far as the crop goes; this computer does the rest");
        assert!(phone.x > 0.5);
    }

    #[test]
    fn a_pan_sweep_switches_the_ratio_at_most_at_the_lens_edges() {
        // The wobble: a ratio that follows the pan makes the phone animate zoom on every step
        for caps in [tele(), centre_only(), one_lens()] {
            for zoom in [2.0, 3.0, 4.5] {
                let (mut ratios, mut current) = (Vec::new(), 1.0);
                for step in -20..=20 {
                    let split = split_zoom(zoom, step as f64 / 20.0, step as f64 / 40.0, Some(&caps), current);
                    current = split.phone.unwrap().ratio;
                    ratios.push(current);
                }
                let changes = ratios.windows(2).filter(|w| w[0] != w[1]).count();
                assert!(ratios.iter().all(|r| *r == 1.0 || caps.lens_zooms.contains(r)), "{caps:?} {zoom}: {ratios:?}");
                assert!(changes <= 2, "into the telephoto's view and back out, nothing in between: {caps:?} {zoom}: {ratios:?}");
            }
        }
    }

    #[test]
    fn the_lens_doesnt_flip_back_and_forth_at_its_edge() {
        let pan = 0.0526; // at 4.5x the window then fits a 3.8x view: inside the telephoto's, short of the margin
        assert_eq!(split_zoom(4.5, pan, 0.0, Some(&tele()), 1.0).phone.unwrap().ratio, 1.0);
        assert_eq!(split_zoom(4.5, pan, 0.0, Some(&tele()), 3.7).phone.unwrap().ratio, 3.7);
    }

    #[test]
    fn a_centred_window_just_past_a_lens_gets_back_onto_it() {
        // 3.8x is inside the margin, so panning out and back used to leave it stuck on the main camera
        let out = split_zoom(3.8, 1.0, 0.0, Some(&tele()), 3.7).phone.unwrap().ratio;
        let back = split_zoom(3.8, 0.0, 0.0, Some(&tele()), out).phone.unwrap().ratio;
        assert_eq!((out, back), (1.0, 3.7));
    }

    #[test]
    fn a_centre_only_phones_share_doesnt_change_with_the_pan() {
        let shares: Vec<PhoneZoom> = (-10..=10).map(|step| split_zoom(3.0, step as f64 / 10.0, 0.0, Some(&centre_only()), 1.0).phone.unwrap()).collect();
        assert!(shares.iter().all(|s| *s == PhoneZoom { ratio: 1.0, crop: 1.0, x: 0.5, y: 0.5 }));
        assert_eq!(split_zoom(4.5, 0.0, 0.0, Some(&centre_only()), 1.0).phone, Some(PhoneZoom { ratio: 3.7, crop: 1.0, x: 0.5, y: 0.5 }));
    }

    #[test]
    fn zoom_past_what_the_phone_allows_is_finished_on_the_desktop() {
        let split = split_zoom(5.0, 0.0, 0.0, Some(&crop_only()), 1.0);
        assert_eq!(split.phone, Some(PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 }));
        assert!((split.desktop.0 - 2.5).abs() < 1e-9);
    }

    #[test]
    fn caps_come_from_the_current_camera_entry() {
        assert!(PhoneZoomCaps::from_camera(None).is_none());
        assert!(PhoneZoomCaps::from_camera(Some(&json!({"id": "0"}))).is_none(), "an older phone app");
        assert!(PhoneZoomCaps::from_camera(Some(&json!({"zoomRatioMax": null, "cropZoomMax": null}))).is_none());
        assert_eq!(PhoneZoomCaps::from_camera(Some(&json!({"zoomRatioMax": 8.0, "cropZoomMax": 1.0, "freeformCrop": true}))),
                   Some(PhoneZoomCaps { ratio_max: 8.0, crop_max: 1.0, freeform: true, lens_zooms: Vec::new() }));
        assert_eq!(PhoneZoomCaps::from_camera(Some(&json!({"zoomRatioMax": 5.0, "lensZooms": [9.0, 3.7, 0.6]}))).unwrap().lens_zooms, [3.7]);
    }

    #[test]
    fn a_zoom_right_on_a_lens_mark_gets_that_lens() {
        for lens in [1.51, 2.0, 3.7, 3.9, 5.0, 6.3] {
            let caps = PhoneZoomCaps { ratio_max: 10.0, crop_max: 4.0, freeform: true, lens_zooms: vec![lens] };
            // Where the slider's mark (and its snap) puts it
            let zoom = lens_step(lens) as f64 / 100.0;
            assert_eq!(split_zoom(zoom, 0.0, 0.0, Some(&caps), 1.0).phone.unwrap().ratio, lens);
        }
    }

    #[test]
    fn points_map_both_ways_and_agree_with_focus() {
        for rotation in [None, Some(90), Some(180), Some(270)] {
            for (flip_h, flip_v) in [(false, false), (true, false), (false, true), (true, true)] {
                let g = Geometry { zoom: 2.5, pan_x: 0.4, pan_y: -0.7, flip_h, flip_v, rotation, ..Default::default() };
                for (u, v) in [(0.1, 0.2), (0.5, 0.5), (0.9, 0.35), (-0.2, 1.3)] {
                    let (x, y) = shown_to_view(u, v, &g);
                    assert!(close(view_to_shown(x, y, &g), (u, v)), "{g:?}");
                }
                let g = Geometry { zoom: 2.0, pan_x: 0.5, pan_y: -0.5, ..g };
                let (x, y) = shown_to_view(0.3, 0.6, &g);
                let (fx, fy) = inverse_map(0.3, 0.6, 4000.0, 3000.0, &g);
                assert!((x - fx).abs() < 1e-3 && (y - fy).abs() < 1e-3, "{g:?}");
            }
        }
    }

    #[test]
    fn points_round_trip_through_a_stretch_and_the_centre_stays_put() {
        for (sx, sy) in [(1.5, 1.0), (0.5, 2.0)] {
            let g = Geometry { zoom: 2.0, pan_x: 0.3, pan_y: -0.5, flip_h: true, rotation: Some(270), stretch_x: sx, stretch_y: sy, ..Default::default() };
            for (u, v) in [(0.1, 0.2), (0.5, 0.5), (0.9, 0.7)] {
                let (x, y) = shown_to_view(u, v, &g);
                assert!(close(view_to_shown(x, y, &g), (u, v)));
            }
            assert!(close(shown_to_view(0.5, 0.5, &g), shown_to_view(0.5, 0.5, &Geometry { stretch_x: 1.0, stretch_y: 1.0, ..g })));
        }
    }

    #[test]
    fn inverse_map_clamps_and_survives_an_unknown_frame_size() {
        let g = Geometry { zoom: 2.0, ..Default::default() };
        assert_eq!(inverse_map(1.2, -0.1, 0.0, 0.0, &g), (1.0, 0.0));
    }

    #[test]
    fn a_lens_box_is_the_centred_part_of_the_view_that_lens_sees() {
        assert!(lens_boxes(None, &Geometry::default()).is_empty());
        let boxes = lens_boxes(Some(&tele()), &Geometry::default());
        assert_eq!(boxes.len(), 1);
        let (label, x0, y0, x1, y1) = boxes[0].clone();
        assert_eq!(label, "3.7×");
        let (lo, hi) = (0.5 - 0.5 / 3.7, 0.5 + 0.5 / 3.7);
        assert!((x0 - lo).abs() < 1e-9 && (y0 - lo).abs() < 1e-9 && (x1 - hi).abs() < 1e-9 && (y1 - hi).abs() < 1e-9);
        // Zoomed 2x and panned fully right: the window is the right half, so the box shifts left and doubles
        let (_, x0, _, x1, _) = lens_boxes(Some(&tele()), &Geometry { zoom: 2.0, pan_x: 1.0, ..Default::default() })[0].clone();
        assert!((x0 - (lo - 0.5) * 2.0).abs() < 1e-9 && (x1 - (hi - 0.5) * 2.0).abs() < 1e-9);
        // Rotated a quarter turn, the box's sides swap axes
        let (_, _, y0, _, y1) = lens_boxes(Some(&tele()), &Geometry { zoom: 2.0, pan_x: 1.0, rotation: Some(90), ..Default::default() })[0].clone();
        assert!((y0 - (lo - 0.5) * 2.0).abs() < 1e-9 && (y1 - (hi - 0.5) * 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_lens_step_never_lands_under_the_lens() {
        assert_eq!(lens_step(3.0), 300);
        assert_eq!(lens_step(3.004), 301);
        assert_eq!(lens_step(3.7), 370);
    }

    #[test]
    fn a_scroll_step_stops_on_the_first_lens_it_crosses() {
        assert_eq!(zoom_detent(290, 320, &[300, 700]), 300);
        assert_eq!(zoom_detent(300, 330, &[300, 700]), 330, "from the mark, it goes on");
        assert_eq!(zoom_detent(800, 650, &[300, 700]), 700);
        assert_eq!(zoom_detent(700, 640, &[300, 700]), 640);
        assert_eq!(zoom_detent(150, 170, &[300]), 170);
    }
}
