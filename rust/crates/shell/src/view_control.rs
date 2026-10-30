//! The window's view of the shown stream (view_controller.py): what's asked of it, split between the phone's own zoom
//! and the crop this computer does, the phone told only about changes, and what the Transforms card and the preview
//! follow (ViewStatus, LensBoxes). It hears the stream hooks as a follower named "view", like Python's.
//!
//! Not ported: matching frames while a steered crop moves on the same lens (_Handover), so the desktop's crop switches
//! as the phone is told rather than once its frames show the new crop; and mapping picked points back onto the phone.

use serde_json::{json, Value};
use std::collections::HashMap;
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
    /// The phone side of each stream the panels moved away from (StreamsBehind).
    behind: HashMap<String, (u64, Option<PhoneZoomCaps>, Option<PhoneZoom>, f64)>,
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

    /// The view the frame step draws: this computer's share of the zoom, the rest as asked.
    pub fn shown(&self, view: &View) -> View {
        View { zoom: self.desktop.0 as f32, pan_x: self.desktop.1 as f32, pan_y: self.desktop.2 as f32, ..view.clone() }
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
        if let Some(p) = phone.filter(|p| Some(*p) != self.sent) {
            if self.sent.is_none_or(|s| s.ratio != p.ratio) {
                self.sent_ratio_at = now;
            }
            self.sent = Some(p);
            out.push(Said::Zoom(p));
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
