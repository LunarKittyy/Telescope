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
            rotation: v.rotation, stretch_x: v.stretch_x as f64, stretch_y: v.stretch_y as f64,
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
    (clamp(u, 0.0, 1.0), clamp(v, 0.0, 1.0))
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
            let g = Geometry { zoom: 2.5, pan_x: 0.3, pan_y: -0.7, flip_h: true, flip_v: false, rotation, stretch_x: 1.2, stretch_y: 0.9 };
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

    #[test]
    fn pan_puts_the_window_where_asked() {
        assert_eq!(pan_for(0.5, 2.0), 0.0);
        assert!((window_centre(2.0, pan_for(0.6, 2.0)) - 0.6).abs() < 1e-9);
        assert_eq!(pan_for(0.9, 1.0), 0.0);
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
}
