//! The window's view of the shown stream (view_controller.py): what's asked of it, split between the phone's own zoom
//! and the crop this computer does, the phone told only about changes, and what the Transforms card and the preview
//! follow (ViewStatus, LensBoxes). It hears the stream hooks as a follower named "view", like Python's.

use crate::handover::ViewGate;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use telescope_core::event::{Event, LensBox};
use telescope_core::host::{View, ViewChange};
use telescope_core::view::{lens_boxes, pan_for, rest_within, split_zoom, window_centre, Geometry, PhoneZoom, PhoneZoomCaps, ViewStatus, ZoomSplit};

/// While steered, the phone is sent a share this much wider than the framing
const HOLD_SLACK: f64 = 1.4;
/// and a new one once the share it would get is this far off its own
const HOLD_DRIFT: f64 = 1.25;
/// for this long (seconds).
const HOLD_S: f64 = 2.0;

/// What the controller wants done once it has moved.
#[derive(Clone, Debug, PartialEq)]
pub enum Said {
    /// The phone's `zoom` control for the shown stream.
    Zoom(PhoneZoom),
    Emit(Event),
    /// The view's frame step changed.
    Restep,
}

#[derive(Default)]
pub struct ViewController {
    /// What set_view asked for; differs from the view in zoom and pan while something has taken it.
    manual: View,
    owner: String,
    /// Something set a view: until then the phone is never sent a zoom.
    engaged: bool,
    /// The zoom and pan left for this computer to crop (split_zoom).
    desktop: (f64, f64, f64),
    /// The shown stream, while there's one to tell.
    source: Option<String>,
    /// StreamInfo::session of that stream.
    session: u64,
    caps: Option<PhoneZoomCaps>,
    sent: Option<PhoneZoom>,
    sent_ratio_at: f64,
    /// While steered: since when the phone's share has been clearly off.
    drift_since: Option<f64>,
    /// A share sent while steering, until its frames show up (the worker sees those).
    gate: Arc<ViewGate>,
    /// The phone side of each stream the panels moved away from (StreamsBehind).
    behind: HashMap<String, (u64, Option<PhoneZoomCaps>, Option<PhoneZoom>, f64)>,
}

fn with_crop(view: &View, (zoom, pan_x, pan_y): (f64, f64, f64)) -> View {
    View { zoom: zoom as f32, pan_x: pan_x as f32, pan_y: pan_y as f32, ..view.clone() }
}

/// What this computer crops from what `phone` sends, as near the view as that reaches (_rest).
fn rest(view: &View, phone: &PhoneZoom, otherwise: (f64, f64, f64)) -> (f64, f64, f64) {
    rest_within(view.zoom as f64, view.pan_x as f64, view.pan_y as f64, phone, true).unwrap_or(otherwise)
}

fn moves(c: &ViewChange) -> bool {
    c.zoom.is_some() || c.pan_x.is_some() || c.pan_y.is_some()
}

fn apply(view: &View, c: &ViewChange, with_moves: bool) -> View {
    let mut v = view.clone();
    if with_moves {
        if let Some(z) = c.zoom { v.zoom = z; }
        if let Some(x) = c.pan_x { v.pan_x = x; }
        if let Some(y) = c.pan_y { v.pan_y = y; }
    }
    if let Some(f) = c.flip_h { v.flip_h = f; }
    if let Some(f) = c.flip_v { v.flip_v = f; }
    if let Some(r) = c.rotation { v.rotation = r; }
    if let Some(s) = c.stretch_x { v.stretch_x = s; }
    if let Some(s) = c.stretch_y { v.stretch_y = s; }
    v
}

impl ViewController {
    pub fn new() -> ViewController {
        ViewController { manual: View::default(), desktop: (1.0, 0.0, 0.0), ..Default::default() }
    }

    /// The share of the zoom the phone was last sent, if any (what feed_raw's frames already have cropped).
    pub fn sent(&self) -> Option<telescope_core::view::PhoneZoom> {
        self.sent
    }

    pub fn gate(&self) -> Arc<ViewGate> {
        self.gate.clone()
    }

    /// The view as this computer's share of the zoom leaves it, the rest as asked.
    pub fn shown(&self, view: &View) -> View {
        with_crop(view, self.desktop)
    }

    /// The view a new frame step draws: shown(), but for the old share's crop while the frames still come in that way.
    pub fn frame_view(&self, view: &View) -> View {
        self.gate.waiting_view().unwrap_or_else(|| self.shown(view))
    }

    pub fn set_view(&mut self, view: &mut View, change: &ViewChange, now: f64) -> Vec<Said> {
        self.manual = apply(&self.manual, change, true);
        let free = self.owner.is_empty();
        let mut out = Vec::new();
        if !self.engaged {
            self.engaged = true;
            self.sync(view, now, &mut out);
        }
        let new = apply(view, change, free);
        self.move_to(view, new, free && moves(change), now, &mut out);
        out
    }

    /// take_view: true if owner has it now.
    pub fn take(&mut self, owner: &str) -> (bool, Vec<Said>) {
        if !self.owner.is_empty() && self.owner != owner {
            return (false, Vec::new());
        }
        let mut out = Vec::new();
        if self.owner.is_empty() {
            self.owner = owner.into();
            self.drift_since = Some(f64::NEG_INFINITY); // the phone's share is picked afresh on the first steer
            out.push(Said::Emit(Event::ViewTaken { owner: owner.into() }));
        }
        (true, out)
    }

    pub fn steer(&mut self, view: &mut View, owner: &str, zoom: f32, pan_x: f32, pan_y: f32, now: f64) -> Vec<Said> {
        let mut out = Vec::new();
        if owner.is_empty() || owner != self.owner {
            return out;
        }
        let (engaging, before) = (!self.engaged, view.clone());
        self.engaged = true;
        let new = View { zoom, pan_x, pan_y, ..view.clone() };
        self.move_to(view, new, false, now, &mut out);
        if engaging && *view == before {
            self.sync(view, now, &mut out);
        }
        out
    }

    pub fn release(&mut self, view: &mut View, owner: &str, now: f64) -> Vec<Said> {
        let mut out = Vec::new();
        if owner.is_empty() || owner != self.owner {
            return out;
        }
        self.owner.clear();
        out.push(Said::Emit(Event::ViewTaken { owner: String::new() }));
        let new = View { zoom: self.manual.zoom, pan_x: self.manual.pan_x, pan_y: self.manual.pan_y, ..view.clone() };
        self.move_to(view, new, false, now, &mut out);
        out
    }

    fn move_to(&mut self, view: &mut View, new: View, moved: bool, now: f64, out: &mut Vec<Said>) {
        if new == *view {
            return;
        }
        *view = new;
        self.sync(view, now, out);
        out.push(Said::Emit(Event::ViewChanged { view: view.clone() }));
        out.push(Said::Emit(Event::ViewPannable { pannable: view.zoom > 1.0 }));
        out.push(self.lens_boxes(view, moved));
        out.push(Said::Restep);
    }

    // ── The stream it tells ──

    pub fn stream_start(&mut self, view: &View, source_id: &str, session: u64, now: f64) -> Vec<Said> {
        self.source = Some(source_id.into());
        self.session = session;
        self.sent = None; // a new connection (or a reconnect): tell the phone again
        let mut out = Vec::new();
        self.sync(view, now, &mut out);
        out
    }

    pub fn focus_gained(&mut self, view: &View, source_id: &str, session: u64, now: f64) -> Vec<Said> {
        // A stream that started again since has told the phone nothing yet
        let kept = self.behind.remove(source_id).filter(|k| k.0 == session);
        let Some((_, caps, sent, at)) = kept else { return self.stream_start(view, source_id, session, now) };
        // Back on a stream: the phone already crops what it was sent, so the desktop only does the rest
        self.source = Some(source_id.into());
        self.session = session;
        (self.caps, self.sent, self.sent_ratio_at) = (caps, sent, at);
        let mut out = Vec::new();
        self.sync(view, now, &mut out);
        out
    }

    pub fn stream_stop(&mut self, view: &View, now: f64) -> Vec<Said> {
        if let Some(id) = &self.source {
            self.behind.remove(id);
        }
        self.leave(view, now)
    }

    pub fn focus_lost(&mut self, view: &View, now: f64) -> Vec<Said> {
        if let Some(id) = self.source.clone() {
            self.behind.insert(id, (self.session, self.caps.clone(), self.sent, self.sent_ratio_at));
        }
        self.leave(view, now)
    }

    fn leave(&mut self, view: &View, now: f64) -> Vec<Said> {
        self.source = None;
        self.caps = None;
        let mut out = Vec::new();
        self.sync(view, now, &mut out);
        out.push(self.lens_boxes(view, false));
        out
    }

    pub fn phone_state(&mut self, view: &View, state: &Value, now: f64) -> Vec<Said> {
        let cam = state.get("cameras").and_then(Value::as_array).and_then(|c| c.iter().find(|c| c.get("current") == Some(&Value::Bool(true))));
        self.camera_caps(view, cam, now)
    }

    pub fn camera_caps(&mut self, view: &View, cam: Option<&Value>, now: f64) -> Vec<Said> {
        let caps = PhoneZoomCaps::from_camera(cam);
        let mut out = Vec::new();
        if caps != self.caps {
            self.caps = caps;
            self.sync(view, now, &mut out);
            out.push(self.lens_boxes(view, false));
        }
        out
    }

    // ── The phone's share of the zoom ──

    /// _sync_zoom: split the framing between phone and desktop; the phone only hears about changes.
    fn sync(&mut self, view: &View, now: f64, out: &mut Vec<Said>) {
        let current = self.sent.map_or(1.0, |s| s.ratio);
        let caps = self.caps.clone().filter(|_| self.source.is_some() && self.engaged);
        let (zoom, px, py) = (view.zoom as f64, view.pan_x as f64, view.pan_y as f64);
        let split = split_zoom(zoom, px, py, caps.as_ref(), current);
        let (mut phone, mut desktop) = (split.phone, split.desktop);
        if !self.owner.is_empty() && phone.is_some() {
            (phone, desktop) = self.held(view, caps.as_ref(), current, split, now);
        }
        if desktop != self.desktop {
            self.desktop = desktop;
            out.push(Said::Restep);
        }
        self.gate.follow(self.source.as_deref(), !self.owner.is_empty());
        if let Some(p) = phone.filter(|p| Some(*p) != self.sent) {
            if self.sent.is_none_or(|s| s.ratio != p.ratio) {
                self.sent_ratio_at = now;
            }
            // A new lens animates on the phone, so only a crop on the same lens gets a handover
            match self.sent.filter(|s| !self.owner.is_empty() && s.ratio == p.ratio) {
                Some(old) => self.gate.begin(old, p, (with_crop(view, rest(view, &old, desktop)), with_crop(view, desktop)), now),
                None => self.gate.end(),
            }
            out.push(Said::Restep);
            self.sent = Some(p);
            out.push(Said::Zoom(p));
        } else if self.gate.underway() {
            match self.gate.still_for(self.sent, !self.owner.is_empty(), now) {
                Some(old) => self.gate.retarget((with_crop(view, rest(view, &old, desktop)), with_crop(view, desktop))),
                None => self.gate.end(),
            }
            out.push(Said::Restep);
        }
        let streaming = self.source.is_some();
        let status = ViewStatus { streaming, caps: self.caps.clone().filter(|_| streaming), sent: self.sent, sent_ratio_at: self.sent_ratio_at, desktop_zoom: desktop.0 };
        out.push(Said::Emit(Event::ViewStatus { status: serde_json::to_value(status).unwrap_or(Value::Null) }));
    }

    /// _held: while something steers the view the phone keeps what it was sent (a moving crop would make it hop lenses
    /// and lag behind) until the framing leaves it, or wants a clearly different one for HOLD_S.
    fn held(&mut self, view: &View, caps: Option<&PhoneZoomCaps>, current: f64, split: ZoomSplit, now: f64) -> (Option<PhoneZoom>, (f64, f64, f64)) {
        let (zoom, px, py) = (view.zoom as f64, view.pan_x as f64, view.pan_y as f64);
        let held = self.sent;
        if let Some(h) = held.filter(|_| self.gate.waiting()) {
            return (held, rest(view, &h, split.desktop)); // no new share until the last one has shown up
        }
        let rest = held.and_then(|h| rest_within(zoom, px, py, &h, false));
        let wide = (zoom / HOLD_SLACK).max(1.0); // room around the framing, so small moves stay on this computer
        let centre = (window_centre(zoom, px), window_centre(zoom, py));
        let Some(want) = split_zoom(wide, pan_for(centre.0, wide), pan_for(centre.1, wide), caps, current).phone else { return (split.phone, split.desktop) };
        let drifted = match held {
            None => true,
            Some(h) => {
                let off = (want.ratio * want.crop) / (h.ratio * h.crop);
                want.ratio != h.ratio || !(1.0 / HOLD_DRIFT..=HOLD_DRIFT).contains(&off)
            }
        };
        if let Some(rest) = rest {
            if !drifted {
                self.drift_since = None;
                return (held, rest);
            }
            let since = *self.drift_since.get_or_insert(now);
            if now - since < HOLD_S {
                return (held, rest);
            }
        }
        self.drift_since = None;
        match rest_within(zoom, px, py, &want, false) {
            Some(rest) => (Some(want), rest),
            None => (split.phone, split.desktop),
        }
    }

    fn lens_boxes(&self, view: &View, moved: bool) -> Said {
        let caps = self.caps.as_ref().filter(|_| self.source.is_some());
        let boxes = lens_boxes(caps, &Geometry::from(view)).into_iter()
            .map(|(label, x0, y0, x1, y1)| LensBox { label, x0: x0 as f32, y0: y0 as f32, x1: x1 as f32, y1: y1 as f32 }).collect();
        Said::Emit(Event::LensBoxes { boxes, moved })
    }
}

/// The phone's `zoom` control body.
pub fn zoom_body(p: &PhoneZoom) -> Value {
    json!({"action": "zoom", "ratio": p.ratio, "crop": p.crop, "x": p.x, "y": p.y})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zooms() -> Value {
        json!({"cameras": [{"id": "0", "current": true, "zoomRatioMax": 10.0, "cropZoomMax": 4.0, "freeformCrop": true}]})
    }

    fn sent(said: &[Said]) -> Vec<PhoneZoom> {
        said.iter().filter_map(|s| if let Said::Zoom(p) = s { Some(*p) } else { None }).collect()
    }

    fn change(zoom: Option<f32>, pan_x: Option<f32>, flip_h: Option<bool>) -> ViewChange {
        ViewChange { zoom, pan_x, flip_h, ..Default::default() }
    }

    #[test]
    fn without_anything_setting_a_view_the_phone_is_left_alone_and_frames_go_through() {
        let (mut vc, view) = (ViewController::new(), View::default());
        let mut said = vc.stream_start(&view, "p1", 1, 0.0);
        said.extend(vc.phone_state(&view, &zooms(), 0.0));
        assert!(sent(&said).is_empty());
        assert_eq!(vc.shown(&view), view);
    }

    #[test]
    fn taking_the_view_moves_zoom_and_pan_but_leaves_flip_and_rotation_manual() {
        let (mut vc, mut view) = (ViewController::new(), View::default());
        let mut said = vc.set_view(&mut view, &change(Some(2.0), Some(0.5), None), 0.0);
        let (took, s) = vc.take("Auto framing");
        assert!(took);
        said.extend(s);
        assert!(!vc.take("Something else").0);
        said.extend(vc.steer(&mut view, "Auto framing", 3.0, -0.2, 0.1, 0.0));
        said.extend(vc.steer(&mut view, "Something else", 1.0, 0.0, 0.0, 0.0)); // ignored: it hasn't got the view
        said.extend(vc.set_view(&mut view, &change(Some(1.5), None, Some(true)), 0.0)); // the flip goes through, the zoom waits
        assert_eq!(view, View { zoom: 3.0, pan_x: -0.2, pan_y: 0.1, flip_h: true, ..View::default() });
        said.extend(vc.release(&mut view, "Auto framing", 0.0));
        assert_eq!(view, View { zoom: 1.5, pan_x: 0.5, flip_h: true, ..View::default() });
        let taken: Vec<String> = said.iter().filter_map(|s| if let Said::Emit(Event::ViewTaken { owner }) = s { Some(owner.clone()) } else { None }).collect();
        assert_eq!(taken, ["Auto framing", ""]);
        let last = said.iter().rev().find_map(|s| if let Said::Emit(Event::ViewChanged { view }) = s { Some(view.clone()) } else { None });
        assert_eq!(last, Some(view));
    }

    /// A stream whose lens zooms itself, steered to 2x.
    fn steered() -> (ViewController, View, Vec<PhoneZoom>) {
        let (mut vc, mut view) = (ViewController::new(), View::default());
        let mut said = vc.stream_start(&view, "p1", 1, 100.0);
        said.extend(vc.phone_state(&view, &zooms(), 100.0));
        said.extend(vc.take("Auto framing").1);
        said.extend(vc.steer(&mut view, "Auto framing", 2.0, 0.0, 0.0, 100.0));
        (vc, view, sent(&said))
    }

    #[test]
    fn steering_alone_wakes_the_phone_up_to_its_share() {
        let (vc, _, sent) = steered();
        assert_eq!(sent, [PhoneZoom { ratio: 1.0, crop: 1.4286, x: 0.5, y: 0.5 }]); // with room to move
        assert!((vc.desktop.0 - 1.4).abs() < 1e-3);
    }

    #[test]
    fn small_steers_move_on_this_computer_and_leave_the_phone_alone() {
        let (mut vc, mut view, _) = steered();
        let mut more = Vec::new();
        for pan in [0.05, 0.1, -0.1, 0.0] {
            more.extend(sent(&vc.steer(&mut view, "Auto framing", 2.1, pan, pan, 100.0)));
        }
        assert!(more.is_empty());
    }

    #[test]
    fn a_steady_new_framing_gets_a_new_phone_share_after_a_while() {
        let (mut vc, mut view, _) = steered();
        assert!(sent(&vc.steer(&mut view, "Auto framing", 3.0, 0.0, 0.0, 100.0)).is_empty()); // still fits, just tighter
        let later = sent(&vc.steer(&mut view, "Auto framing", 3.0, 0.0, 0.0001, 102.5));
        assert_eq!(later.len(), 1);
        assert!((later[0].crop - 3.0 / 1.4).abs() < 1e-3);
    }

    #[test]
    fn a_framing_the_phone_share_cant_show_gets_one_at_once() {
        let (mut vc, mut view, _) = steered();
        let now = sent(&vc.steer(&mut view, "Auto framing", 1.0, 0.0, 0.0, 100.0));
        assert_eq!(now.last().map(|p| p.crop), Some(1.0));
        assert_eq!(vc.desktop, (1.0, 0.0, 0.0));
    }

    #[test]
    fn releasing_puts_back_the_manual_zoom() {
        let (mut vc, mut view, _) = steered();
        let said = sent(&vc.release(&mut view, "Auto framing", 100.0));
        assert_eq!(view.zoom, 1.0);
        assert_eq!(said.last().map(|p| p.crop), Some(1.0));
    }

    #[test]
    fn the_phone_gets_its_share_of_a_manual_zoom_and_the_transforms_card_hears_it() {
        let (mut vc, mut view) = (ViewController::new(), View::default());
        vc.stream_start(&view, "p1", 1, 0.0);
        vc.phone_state(&view, &zooms(), 0.0);
        let said = vc.set_view(&mut view, &change(Some(3.0), None, None), 5.0);
        // The first set_view engages the phone at the old zoom, then moves, as Python's does
        assert_eq!(sent(&said).last(), Some(&PhoneZoom { ratio: 1.0, crop: 3.0, x: 0.5, y: 0.5 }));
        assert_eq!(vc.shown(&view).zoom, 1.0, "the phone crops it all, so this computer doesn't");
        let status = said.iter().rev().find_map(|s| if let Said::Emit(Event::ViewStatus { status }) = s { Some(status.clone()) } else { None }).unwrap();
        assert_eq!(status["streaming"], true);
        assert_eq!(status["sent_ratio_at"], 5.0);
        // The stream ends: everything is cropped here again
        vc.stream_stop(&view, 6.0);
        assert_eq!(vc.shown(&view).zoom, 3.0);
    }

    #[test]
    fn coming_back_to_a_stream_keeps_what_its_phone_was_sent() {
        let (mut vc, mut view) = (ViewController::new(), View::default());
        vc.stream_start(&view, "p1", 1, 0.0);
        vc.phone_state(&view, &zooms(), 0.0);
        vc.set_view(&mut view, &change(Some(3.0), None, None), 0.0);
        vc.focus_lost(&view, 1.0);
        assert!(sent(&vc.focus_gained(&view, "p1", 1, 2.0)).is_empty(), "the phone still crops what it was sent");
        assert_eq!(vc.shown(&view).zoom, 1.0);
    }

    /// Something that shows a shift: blobs, the same every run.
    fn scene(u: f64, v: f64) -> u8 {
        let cell = |x: i64, y: i64| ((x.wrapping_mul(7_919) ^ y.wrapping_mul(104_729)).wrapping_mul(2_654_435_761) >> 13 & 255) as f64;
        let (x, y) = (u * 32.0, v * 18.0);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let top = cell(x0, y0) * (1.0 - fx) + cell(x0 + 1, y0) * fx;
        let bottom = cell(x0, y0 + 1) * (1.0 - fx) + cell(x0 + 1, y0 + 1) * fx;
        (top * (1.0 - fy) + bottom * fy) as u8
    }

    /// What the phone sends for a share: that part of the full view, at the stream's size.
    fn from_phone(share: &PhoneZoom) -> telescope_core::frame::Frame {
        use telescope_core::frame::{Frame, PixelFormat};
        use telescope_core::view::{phone_window, window};
        let (zoom, px, py) = phone_window(Some(share));
        let ((x0, w), (y0, h)) = (window(zoom, px), window(zoom, py));
        let (fw, fh) = (320u32, 180u32);
        let data = (0..fw * fh).flat_map(|i| {
            let (u, v) = ((i % fw) as f64 / fw as f64, (i / fw) as f64 / fh as f64);
            let g = scene(x0 + u * w, y0 + v * h);
            [g, g, g, 255]
        }).collect();
        Frame::new(fw, fh, PixelFormat::Rgba, data)
    }

    fn on_a_share() -> (ViewController, View, PhoneZoom) {
        let (mut vc, mut view) = (ViewController::new(), View::default());
        vc.stream_start(&view, "p1", 1, 0.0);
        vc.phone_state(&view, &zooms(), 0.0);
        vc.take("Auto framing");
        vc.steer(&mut view, "Auto framing", 3.0, 0.0, 0.0, 0.0);
        let old = vc.sent.unwrap();
        (vc, view, old)
    }

    #[test]
    fn a_new_phone_share_is_cropped_for_only_once_its_frames_arrive() {
        let (mut vc, mut view, old) = on_a_share();
        let gate = vc.gate();
        gate.for_frame("p1", &from_phone(&old), 0.0, false);
        vc.steer(&mut view, "Auto framing", 3.0, 0.22, 0.0, 0.01); // just past the side of what the phone sends: a new share at once
        let new = vc.sent.unwrap();
        assert!(new != old && gate.waiting());
        let late = rest_within(3.0, 0.22, 0.0, &old, true).unwrap();
        let fresh = vc.shown(&view);
        assert!((fresh.pan_x - late.1 as f32).abs() > 0.5, "the new share's crop is far from the old one's: {fresh:?} {late:?}");
        for i in 0..3 {
            gate.for_frame("p1", &from_phone(&old), 0.02 + i as f64 * 0.01, false); // the phone hasn't caught up yet
            assert!(gate.waiting());
            assert_eq!(vc.frame_view(&view).pan_x, late.1 as f32, "the old crop stays on the old frames");
        }
        let swapped = gate.for_frame("p1", &from_phone(&new), 0.06, false).expect("the crop changes on this very frame");
        assert!(!gate.waiting());
        assert_eq!((swapped.zoom, swapped.pan_x), (fresh.zoom, fresh.pan_x));
        assert_eq!(vc.frame_view(&view).pan_x, fresh.pan_x);
    }

    #[test]
    fn a_handover_gives_up_waiting_after_a_while() {
        let (mut vc, mut view, old) = on_a_share();
        let gate = vc.gate();
        vc.steer(&mut view, "Auto framing", 3.0, 0.6, 0.0, 100.0);
        assert!(gate.waiting());
        gate.for_frame("p1", &from_phone(&old), 100.1, false);
        assert!(gate.waiting());
        gate.for_frame("p1", &from_phone(&old), 101.6, false);
        assert!(!gate.waiting());
    }

    #[test]
    fn frames_of_another_stream_say_nothing_about_this_ones_share() {
        let (mut vc, mut view, old) = on_a_share();
        let gate = vc.gate();
        vc.steer(&mut view, "Auto framing", 3.0, 0.6, 0.0, 1.0);
        assert_eq!(gate.for_frame("p2", &from_phone(&old), 9.0, false), None);
        assert!(gate.waiting());
    }

    #[test]
    fn steps_put_in_while_it_waits_get_the_crop_of_the_moment_back() {
        let (mut vc, mut view, old) = on_a_share();
        let gate = vc.gate();
        vc.steer(&mut view, "Auto framing", 3.0, 0.22, 0.0, 1.0);
        assert!(gate.for_frame("p1", &from_phone(&old), 1.0, false).is_some());
        assert_eq!(gate.for_frame("p1", &from_phone(&old), 1.01, false), None, "already in place");
        assert_eq!(gate.for_frame("p1", &from_phone(&old), 1.02, true), Some(vc.frame_view(&view)));
    }

    #[test]
    fn a_frozen_step_keeps_the_view_it_was_taken_with() {
        let (mut vc, mut view) = (ViewController::new(), View::default());
        vc.set_view(&mut view, &ViewChange { flip_h: Some(true), ..Default::default() }, 0.0);
        let step = vc.frame_view(&view);
        vc.set_view(&mut view, &ViewChange { flip_h: Some(false), rotation: Some(Some(90)), ..Default::default() }, 0.0);
        assert!(step.flip_h && step.rotation.is_none());
    }

    #[test]
    fn rest_within_says_what_this_computer_crops_from_a_phone_share() {
        let centre = PhoneZoom { ratio: 1.0, crop: 2.0, x: 0.5, y: 0.5 };
        assert_eq!(rest_within(2.0, 0.0, 0.0, &centre, false), Some((1.0, 0.0, 0.0)));
        let rest = rest_within(4.0, 0.3, 0.0, &centre, false).unwrap();
        assert!((rest.0 - 2.0).abs() < 1e-9 && rest.1 > 0.0 && rest.2.abs() < 1e-9);
        assert_eq!(rest_within(1.5, 0.0, 0.0, &centre, false), None, "wider than what the phone sends");
        assert_eq!(rest_within(4.0, 1.0, 0.0, &centre, false), None, "off its edge");
    }

    #[test]
    fn people_are_found_through_the_phone_share() {
        let (mut vc, mut view) = (ViewController::new(), View::default());
        vc.stream_start(&view, "p1", 1, 0.0);
        vc.phone_state(&view, &zooms(), 0.0);
        vc.take("Auto framing");
        vc.steer(&mut view, "Auto framing", 2.0, 0.0, 0.0, 0.0); // the phone sends the middle 1/1.4286
        let turn = telescope_core::vision::Turn { rotation: Some(90), ..Default::default() };
        let upright = telescope_core::vision::Upright { turn, sent: telescope_core::view::phone_window(vc.sent.as_ref()) };
        let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
        let (x, y) = upright.to_shown(0.5, 0.5);
        assert!(near(x, 0.5) && near(y, 0.5));
        let (x, y) = upright.to_shown(1.0, 0.0); // the top right of the upright frame, inside the share
        assert!(near(x, 0.85) && near(y, 0.15), "{x} {y}");
    }

    #[test]
    fn a_stream_that_started_again_behind_the_panels_is_told_again() {
        let (mut vc, mut view) = (ViewController::new(), View::default());
        vc.stream_start(&view, "p1", 1, 0.0);
        vc.phone_state(&view, &zooms(), 0.0);
        vc.set_view(&mut view, &change(Some(3.0), None, None), 0.0);
        vc.focus_lost(&view, 1.0);
        vc.focus_gained(&view, "p1", 2, 2.0);
        assert_eq!(vc.shown(&view).zoom, 3.0, "this stream's phone hasn't been sent anything");
    }

    /// A stream whose current lens zooms itself and has a longer one at 3.7x.
    fn lens() -> Value {
        json!({"cameras": [{"id": "0", "current": true, "zoomRatioMax": 10.0, "cropZoomMax": 4.0, "freeformCrop": true, "lensZooms": [3.7]}]})
    }

    /// A controller that something has set a view on, as the Transforms card does when its settings load.
    fn engaged() -> (ViewController, View) {
        let (mut vc, mut view) = (ViewController::new(), View::default());
        vc.set_view(&mut view, &change(Some(1.0), None, None), 0.0);
        (vc, view)
    }

    #[test]
    fn the_phone_is_told_only_when_its_share_changes() {
        let (mut vc, mut view) = engaged();
        let mut said = vc.stream_start(&view, "p1", 1, 0.0);
        assert!(sent(&said).is_empty(), "no caps yet: nothing to tell it");
        said.extend(vc.phone_state(&view, &zooms(), 0.0));
        said.extend(vc.phone_state(&view, &zooms(), 0.0));
        said.extend(vc.set_view(&mut view, &change(Some(3.0), None, None), 0.0));
        assert_eq!(sent(&said), [PhoneZoom { ratio: 1.0, crop: 1.0, x: 0.5, y: 0.5 }, PhoneZoom { ratio: 1.0, crop: 3.0, x: 0.5, y: 0.5 }]);
        assert_eq!(vc.desktop, (1.0, 0.0, 0.0));
    }

    #[test]
    fn this_computer_crops_everything_without_a_phone_that_zooms() {
        let (mut vc, mut view) = engaged();
        let mut said = vc.stream_start(&view, "p1", 1, 0.0);
        said.extend(vc.phone_state(&view, &json!({"cameras": [{"id": "0", "current": true, "zoomRatioMax": 4.0}]}), 0.0));
        said.extend(vc.set_view(&mut view, &change(Some(2.0), None, None), 0.0));
        said.extend(vc.stream_stop(&view, 0.0));
        assert_eq!(vc.desktop, (2.0, 0.0, 0.0));
        said.extend(vc.stream_start(&view, "p1", 1, 0.0));
        said.extend(vc.camera_caps(&view, Some(&json!({"id": "2"})), 0.0)); // a lens that can't zoom itself
        assert_eq!(vc.desktop, (2.0, 0.0, 0.0));
        assert_eq!(sent(&said), [PhoneZoom { ratio: 1.0, crop: 1.0, x: 0.5, y: 0.5 }], "a ratio alone picks no lens");
    }

    #[test]
    fn back_on_a_tile_the_phone_isnt_told_its_zoom_again_but_after_a_real_stop_it_is() {
        let (mut vc, view) = engaged();
        vc.stream_start(&view, "p1", 1, 0.0);
        vc.phone_state(&view, &zooms(), 0.0);
        vc.focus_lost(&view, 1.0);
        let mut said = vc.focus_gained(&view, "p1", 1, 2.0);
        said.extend(vc.phone_state(&view, &zooms(), 2.0));
        assert!(sent(&said).is_empty());
        vc.stream_stop(&view, 3.0);
        let mut said = vc.focus_gained(&view, "p1", 1, 4.0);
        said.extend(vc.phone_state(&view, &zooms(), 4.0));
        assert_eq!(sent(&said), [PhoneZoom { ratio: 1.0, crop: 1.0, x: 0.5, y: 0.5 }]);
    }

    #[test]
    fn the_preview_hears_about_the_lens_boxes_and_the_pan() {
        let boxes_of = |said: &[Said]| said.iter().rev().find_map(|s| if let Said::Emit(Event::LensBoxes { boxes, moved }) = s { Some((boxes.clone(), *moved)) } else { None });
        let pannable = |said: &[Said]| said.iter().rev().find_map(|s| if let Said::Emit(Event::ViewPannable { pannable }) = s { Some(*pannable) } else { None });
        let (mut vc, mut view) = engaged();
        let mut said = vc.stream_start(&view, "p1", 1, 0.0);
        said.extend(vc.phone_state(&view, &lens(), 0.0));
        let (boxes, moved) = boxes_of(&said).unwrap();
        assert_eq!((boxes[0].label.as_str(), moved), ("3.7\u{d7}", false), "new lens data: no flash");
        let said = vc.set_view(&mut view, &change(Some(2.0), None, None), 0.0);
        assert_eq!((boxes_of(&said).unwrap().1, pannable(&said)), (true, Some(true)));
        let said = vc.stream_stop(&view, 0.0);
        assert_eq!(boxes_of(&said), Some((Vec::new(), false)));
    }
}
