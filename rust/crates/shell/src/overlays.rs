//! The points and areas plugins ask for on the preview, and the warp a plugin moving the picture tells it about
//! (overlays.py's picks and set_warp). The window hands it the mouse and keys on the preview card's stage and draws the
//! pick under way from pick_drawing(); results go out on the bus as PreviewPickChanged and PreviewPicked.

use telescope_core::bus::EventBus;
use telescope_core::event::Event;
use std::collections::BTreeMap;
use telescope_core::host::{Pick, PickKind, PreviewLayer, Shape, View};
use telescope_core::i18n::tr;
use telescope_core::view::{shown_to_view, view_to_shown, Geometry};

/// px, the radius of a corner handle
pub const HANDLE: f64 = 7.0;
/// px, how close to a corner a press grabs it
const GRAB: f64 = 16.0;
/// A rectangle narrower or shorter than this (of the frame) is a stray click, not an area
const MIN_RECT: f64 = 0.01;
/// Where a quad's corners start without a start, in from each edge of the frame as shown
const QUAD_INSET: f64 = 0.2;

type P = (f64, f64);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Corner(usize),
    All,
    Rect,
}

struct PickState {
    owner: String,
    kind: PickKind,
    hint: String,
    /// In the whole picture (to_full), so they stay put while the view moves.
    points: Vec<P>,
    drag: Option<Drag>,
    /// Where a drag of the whole quad is now, in the frame as shown.
    grab_at: P,
}

/// The mouse cursor over the stage while a pick runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    Default,
    Cross,
    Move,
    Grab,
}

/// What the window draws for the pick under way, in the frame as shown on the preview (0..1).
#[derive(Clone, Debug, PartialEq)]
pub struct PickDrawing {
    /// Four corners (a rect's or a quad's), or none while there's no area yet.
    pub corners: Vec<[f32; 2]>,
    pub handles: bool,
    pub hint: String,
}

#[derive(Default)]
pub struct Overlays {
    pick: Option<PickState>,
    /// (owner, matrix, inverse): a plugin moving the picture after the view.
    warp: Option<(String, [f64; 9], [f64; 9])>,
    /// The cursor over the stage while a pick runs, as the last move there left it.
    pub cursor: Option<Cursor>,
    /// The last frame the preview showed, while a pick runs.
    picture: Option<telescope_core::frame::Frame>,
}

fn to_full(view: &View, u: f64, v: f64) -> P {
    shown_to_view(u, v, &Geometry::from(view))
}

fn to_shown(view: &View, x: f64, y: f64) -> P {
    view_to_shown(x, y, &Geometry::from(view))
}

fn clamp(p: P) -> [f32; 2] {
    [p.0.clamp(0.0, 1.0) as f32, p.1.clamp(0.0, 1.0) as f32]
}

/// A move of d along one axis of shapes whose corners span lo..hi there, cut short only where it would push a corner
/// further off the frame. A shape already reaching past an edge doesn't jump back.
fn nudge(d: f64, lo: f64, hi: f64) -> f64 {
    if d < 0.0 {
        d.max((-lo).min(0.0))
    } else {
        d.min((1.0 - hi).max(0.0))
    }
}

/// Whether point is inside polygon (ray casting).
fn inside(point: P, polygon: &[P]) -> bool {
    let (x, y) = point;
    let mut is_in = false;
    for (i, &(x0, y0)) in polygon.iter().enumerate() {
        let (x1, y1) = polygon[(i + 1) % polygon.len()];
        if (y0 > y) != (y1 > y) && x < x0 + (y - y0) * (x1 - x0) / (y1 - y0) {
            is_in = !is_in;
        }
    }
    is_in
}

fn apply(m: &[f64; 9], u: f64, v: f64) -> P {
    let (x, y, z) = (m[0] * u + m[1] * v + m[2], m[3] * u + m[4] * v + m[5], m[6] * u + m[7] * v + m[8]);
    if z.abs() > 1e-9 { (x / z, y / z) } else { (u, v) }
}

fn invert(m: &[f64; 9]) -> Option<[f64; 9]> {
    let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6]) + m[2] * (m[3] * m[7] - m[4] * m[6]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv = [
        (m[4] * m[8] - m[5] * m[7]) / det, (m[2] * m[7] - m[1] * m[8]) / det, (m[1] * m[5] - m[2] * m[4]) / det,
        (m[5] * m[6] - m[3] * m[8]) / det, (m[0] * m[8] - m[2] * m[6]) / det, (m[2] * m[3] - m[0] * m[5]) / det,
        (m[3] * m[7] - m[4] * m[6]) / det, (m[1] * m[6] - m[0] * m[7]) / det, (m[0] * m[4] - m[1] * m[3]) / det,
    ];
    inv.iter().all(|c| c.is_finite()).then_some(inv)
}

impl Overlays {
    /// Owner's plugin moves the whole picture after the view, by matrix (a 3 x 3 homography from the frame as shown
    /// with the view to what the preview shows, both 0..1), or stops with None. A matrix that can't be undone (it
    /// squashes the picture flat) counts as None.
    pub fn set_warp(&mut self, owner: &str, matrix: Option<[f32; 9]>) {
        let matrix = matrix.map(|m| m.map(|c| c as f64));
        match matrix.and_then(|m| invert(&m).map(|inv| (m, inv))) {
            Some((m, inv)) => self.warp = Some((owner.to_string(), m, inv)),
            None => {
                if matrix.is_some() {
                    log::warn!("{owner} gave the preview a warp that can't be undone; ignoring it");
                }
                if self.warp.as_ref().is_some_and(|w| w.0 == owner) {
                    self.warp = None;
                }
            }
        }
    }

    pub fn warping(&self) -> bool {
        self.warp.is_some()
    }

    /// A point on the preview back to the frame as shown with the view (where clicks there mean).
    pub fn unwarp(&self, u: f64, v: f64) -> P {
        self.warp.as_ref().map_or((u, v), |w| apply(&w.2, u, v))
    }

    /// The reverse of unwarp: where a point of the frame as shown lands on the preview.
    pub fn warp_point(&self, u: f64, v: f64) -> P {
        self.warp.as_ref().map_or((u, v), |w| apply(&w.1, u, v))
    }

    /// Starts owner's pick, cancelling any other; the host checks there's a preview to pick on first.
    pub fn start_pick(&mut self, view: &View, owner: &str, kind: PickKind, start: Option<[[f32; 2]; 4]>, hint: &str, bus: &EventBus) {
        self.cancel_pick(None, bus);
        let points = match (kind, start) {
            (PickKind::Quad, Some(start)) => start.iter().map(|p| (p[0] as f64, p[1] as f64)).collect(),
            (PickKind::Quad, None) => {
                let (lo, hi) = (QUAD_INSET, 1.0 - QUAD_INSET);
                [(lo, lo), (hi, lo), (hi, hi), (lo, hi)].iter().map(|&(u, v)| to_full(view, u, v)).collect()
            }
            _ => Vec::new(),
        };
        self.pick = Some(PickState { owner: owner.into(), kind, hint: hint.into(), points, drag: None, grab_at: (0.0, 0.0) });
        self.cursor = (kind != PickKind::Quad).then_some(Cursor::Cross);
        bus.emit(Event::PreviewPickChanged { owner: owner.into() });
    }

    /// The owner of the pick under way, or "".
    pub fn picking(&self) -> &str {
        self.pick.as_ref().map_or("", |p| p.owner.as_str())
    }

    /// Ends the pick with the points it has (a quad as it is now). False if it has none yet.
    pub fn finish_pick(&mut self, view: &View, owner: Option<&str>, bus: &EventBus) -> bool {
        let Some(pick) = &self.pick else { return false };
        if owner.is_some_and(|o| o != pick.owner) {
            return false;
        }
        if (pick.kind == PickKind::Rect && pick.points.len() < 2) || (pick.kind == PickKind::Point && pick.points.is_empty()) {
            return false;
        }
        let result = self.result(view);
        self.end(Some(result), bus);
        true
    }

    /// Ends the pick under way (only owner's, given one) without points: its owner hears None.
    pub fn cancel_pick(&mut self, owner: Option<&str>, bus: &EventBus) {
        if self.pick.as_ref().is_some_and(|p| owner.is_none_or(|o| o == p.owner)) {
            self.end(None, bus);
        }
    }

    /// The preview drew this frame, before any tint: what a pick under way hands over (tint()'s copy).
    pub fn shown_frame(&mut self, frame: &telescope_core::frame::Frame) {
        if self.pick.is_some() {
            self.picture = Some(frame.clone());
        }
    }

    fn end(&mut self, result: Option<Pick>, bus: &EventBus) {
        let Some(pick) = self.pick.take() else { return };
        self.picture = None; // nothing kept once it's over
        self.cursor = None;
        bus.emit(Event::PreviewPickChanged { owner: String::new() });
        bus.emit(Event::PreviewPicked { owner: pick.owner, pick: result });
    }

    fn result(&self, view: &View) -> Pick {
        let pick = self.pick.as_ref().expect("a pick to end");
        let mut full = pick.points.clone();
        let shown: Vec<P> = if pick.kind == PickKind::Rect {
            let ((x0, y0), (x1, y1)) = (full[0], full[1]);
            full = vec![(x0.min(x1), y0.min(y1)), (x0.max(x1), y0.max(y1))];
            let (a, b) = (to_shown(view, full[0].0, full[0].1), to_shown(view, full[1].0, full[1].1));
            vec![(a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1))]
        } else {
            full.iter().map(|p| to_shown(view, p.0, p.1)).collect()
        };
        Pick {
            picture: self.picture.clone(),
            kind: pick.kind,
            in_picture: shown.iter().map(|p| clamp(self.warp_point(p.0, p.1))).collect(),
            shown: shown.into_iter().map(clamp).collect(),
            full: full.into_iter().map(clamp).collect(),
        }
    }

    fn shown_points(&self, view: &View) -> Vec<P> {
        self.pick.as_ref().map_or(Vec::new(), |p| p.points.iter().map(|q| to_shown(view, q.0, q.1)).collect())
    }

    /// The pick's points where they are on the preview.
    fn screen_points(&self, view: &View) -> Vec<P> {
        self.shown_points(view).into_iter().map(|p| self.warp_point(p.0, p.1)).collect()
    }

    fn corner_at(&self, view: &View, u: f64, v: f64, size: P) -> Option<usize> {
        let (w, h) = size;
        let mut best = None;
        let mut best_d = GRAB;
        for (i, (pu, pv)) in self.screen_points(view).into_iter().enumerate() {
            let d2 = ((pu - u) * w).powi(2) + ((pv - v) * h).powi(2);
            if d2 <= best_d * best_d {
                best = Some(i);
                best_d = d2.sqrt();
            }
        }
        best
    }

    /// The left button went down at (u, v) in the frame as shown (size: the frame's size on screen, px). True if the
    /// pick took it; false leaves it to the preview.
    pub fn press(&mut self, view: &View, u: f64, v: f64, size: P) -> bool {
        let Some(kind) = self.pick.as_ref().map(|p| p.kind) else { return false };
        let (su, sv) = self.unwarp(u, v);
        let full = to_full(view, su, sv);
        let corner = self.corner_at(view, u, v, size);
        let in_shape = inside((u, v), &self.screen_points(view));
        let pick = self.pick.as_mut().expect("checked above");
        match kind {
            PickKind::Point => pick.points = vec![full],
            PickKind::Rect => {
                pick.points = vec![full, full];
                pick.drag = Some(Drag::Rect);
            }
            PickKind::Quad => match corner {
                Some(i) => pick.drag = Some(Drag::Corner(i)),
                None if in_shape => {
                    pick.drag = Some(Drag::All);
                    pick.grab_at = (su, sv);
                }
                None => return false,
            },
        }
        true
    }

    /// The mouse moved with the button down after press() took it.
    pub fn move_to(&mut self, view: &View, u: f64, v: f64) -> bool {
        let Some(drag) = self.pick.as_ref().and_then(|p| p.drag) else { return false };
        let (u, v) = self.unwarp(u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
        let full = to_full(view, u, v);
        let shown = self.shown_points(view);
        let pick = self.pick.as_mut().expect("checked above");
        match drag {
            Drag::Rect => pick.points[1] = full,
            Drag::All => {
                // Moved as far as all four corners stay on the picture, so the shape never gets squashed at an edge
                let span = |f: fn(&P) -> f64| (shown.iter().map(f).fold(f64::MAX, f64::min), shown.iter().map(f).fold(f64::MIN, f64::max));
                let ((ulo, uhi), (vlo, vhi)) = (span(|p| p.0), span(|p| p.1));
                let du = nudge(u - pick.grab_at.0, ulo, uhi);
                let dv = nudge(v - pick.grab_at.1, vlo, vhi);
                pick.points = shown.iter().map(|p| to_full(view, p.0 + du, p.1 + dv)).collect();
                pick.grab_at = (pick.grab_at.0 + du, pick.grab_at.1 + dv);
            }
            Drag::Corner(i) => pick.points[i] = full,
        }
        true
    }

    /// The button came up. A point is picked here, and a rectangle once it's big enough; a quad waits for Enter or
    /// finish_pick().
    pub fn release(&mut self, view: &View, bus: &EventBus) -> bool {
        let Some(pick) = self.pick.as_mut() else { return false };
        let drag = pick.drag.take();
        match pick.kind {
            PickKind::Point if !pick.points.is_empty() => {
                let result = self.result(view);
                self.end(Some(result), bus);
            }
            PickKind::Rect if drag == Some(Drag::Rect) => {
                let s = self.shown_points(view);
                if (s[1].0 - s[0].0).abs() >= MIN_RECT && (s[1].1 - s[0].1).abs() >= MIN_RECT {
                    let result = self.result(view);
                    self.end(Some(result), bus);
                } else if let Some(pick) = self.pick.as_mut() {
                    pick.points.clear();
                }
            }
            _ => {}
        }
        true
    }

    /// Enter finishes the pick, Escape cancels it. True if it was for the pick.
    pub fn key(&mut self, view: &View, key: &str, bus: &EventBus) -> bool {
        if self.pick.is_none() {
            return false;
        }
        match key {
            "\n" | "\r" => {
                self.finish_pick(view, None, bus);
                true
            }
            "\u{1b}" => {
                self.cancel_pick(None, bus);
                true
            }
            _ => false,
        }
    }

    /// The mouse cursor over (u, v) while a pick runs, or None for the preview's usual one.
    pub fn cursor_at(&self, view: &View, u: f64, v: f64, size: P) -> Option<Cursor> {
        let pick = self.pick.as_ref()?;
        if pick.kind != PickKind::Quad {
            return Some(Cursor::Cross);
        }
        if pick.drag.is_some() || self.corner_at(view, u, v, size).is_some() {
            return Some(Cursor::Move);
        }
        inside((u, v), &self.screen_points(view)).then_some(Cursor::Grab)
    }

    fn hint(pick: &PickState) -> String {
        if !pick.hint.is_empty() {
            return pick.hint.clone();
        }
        match pick.kind {
            PickKind::Point => tr("Click a point on the picture. Esc cancels."),
            PickKind::Rect => tr("Drag across the picture to mark an area. Esc cancels."),
            PickKind::Quad => tr("Drag the corners into place, then press Enter. Esc cancels."),
        }
    }

    /// The pick under way as the window draws it, None while there's none.
    pub fn pick_drawing(&self, view: &View) -> Option<PickDrawing> {
        let pick = self.pick.as_ref()?;
        let mut points = self.shown_points(view);
        if pick.kind == PickKind::Rect && points.len() == 2 {
            let ((u0, v0), (u1, v1)) = (points[0], points[1]);
            points = vec![(u0, v0), (u1, v0), (u1, v1), (u0, v1)];
        }
        let corners = if points.len() == 4 {
            points.iter().map(|p| { let (u, v) = self.warp_point(p.0, p.1); [u as f32, v as f32] }).collect()
        } else {
            Vec::new()
        };
        Some(PickDrawing { corners, handles: pick.kind == PickKind::Quad, hint: Self::hint(pick) })
    }
}

impl PickDrawing {
    /// SVG path commands in px of the picture's rect (w x h): the dim outside the area (even-odd), its outline, and the
    /// quad's corner handles.
    pub fn paths(&self, w: f32, h: f32) -> (String, String, String) {
        if self.corners.len() != 4 {
            return Default::default();
        }
        let at: Vec<(f32, f32)> = self.corners.iter().map(|p| (p[0] * w, p[1] * h)).collect();
        let shape = format!("M {} {} L {} {} L {} {} L {} {} Z", at[0].0, at[0].1, at[1].0, at[1].1, at[2].0, at[2].1, at[3].0, at[3].1);
        let dim = format!("M 0 0 L {w} 0 L {w} {h} L 0 {h} Z {shape}");
        let r = HANDLE as f32;
        let handles = if self.handles {
            at.iter().map(|(x, y)| format!("M {} {y} A {r} {r} 0 1 0 {} {y} A {r} {r} 0 1 0 {} {y} Z", x - r, x + r, x - r)).collect::<Vec<_>>().join(" ")
        } else {
            String::new()
        };
        (dim, shape, handles)
    }
}

/// A shape of a plugin's PreviewLayer as the preview card draws it: SVG path commands in px of the picture's rect, colours as
/// RGBA (None: not stroked or not filled).
#[derive(Clone, Debug, PartialEq)]
pub struct LayerPath {
    pub commands: String,
    pub stroke: Option<[u8; 4]>,
    pub width: f32,
    pub fill: Option<[u8; 4]>,
    pub evenodd: bool,
}

/// A Text shape, its top left in px of the picture's rect.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerText {
    pub x: f32,
    pub y: f32,
    pub text: String,
    pub color: [u8; 4],
    pub size: f32,
}

/// "#rrggbb" or "#rrggbbaa"; None for anything else.
pub fn rgba(colour: &str) -> Option<[u8; 4]> {
    let hex = colour.strip_prefix('#')?;
    let byte = |i: usize| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok());
    match hex.len() {
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

/// Every layer's shapes for a picture w x h px, by owner and then in the order each layer lists them.
pub fn layer_drawing(layers: &BTreeMap<String, PreviewLayer>, w: f32, h: f32) -> (Vec<LayerPath>, Vec<LayerText>) {
    let (mut paths, mut texts) = (Vec::new(), Vec::new());
    let line = |points: &[[f32; 2]], close: bool| -> String {
        let mut out: Vec<String> = points.iter().enumerate().map(|(i, p)| format!("{} {} {}", if i == 0 { "M" } else { "L" }, p[0] * w, p[1] * h)).collect();
        if close && !out.is_empty() {
            out.push("Z".into());
        }
        out.join(" ")
    };
    for shape in layers.values().flat_map(|l| &l.shapes) {
        match shape {
            Shape::Rect { x0, y0, x1, y1, stroke, width, fill } => {
                let corners = [[*x0, *y0], [*x1, *y0], [*x1, *y1], [*x0, *y1]];
                paths.push(LayerPath { commands: line(&corners, true), stroke: rgba(stroke), width: *width, fill: fill.as_deref().and_then(rgba), evenodd: false });
            }
            Shape::Polygon { points, stroke, width, fill } if points.len() > 1 => {
                paths.push(LayerPath { commands: line(points, true), stroke: rgba(stroke), width: *width, fill: fill.as_deref().and_then(rgba), evenodd: false });
            }
            Shape::Line { points, stroke, width } if points.len() > 1 => {
                paths.push(LayerPath { commands: line(points, false), stroke: rgba(stroke), width: *width, fill: None, evenodd: false });
            }
            Shape::Text { x, y, text, color, size } => {
                if let Some(color) = rgba(color) {
                    texts.push(LayerText { x: x * w, y: y * h, text: text.clone(), color, size: *size });
                }
            }
            Shape::Dim { points, color } if points.len() > 2 => {
                let commands = format!("M 0 0 L {w} 0 L {w} {h} L 0 {h} Z {}", line(points, true));
                paths.push(LayerPath { commands, stroke: None, width: 0.0, fill: rgba(color), evenodd: true });
            }
            _ => {}
        }
    }
    (paths, texts)
}

impl Cursor {
    /// The preview card's pick-cursor.
    pub fn name(self) -> &'static str {
        match self {
            Cursor::Default => "",
            Cursor::Cross => "cross",
            Cursor::Move => "move",
            Cursor::Grab => "grab",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: P = (640.0, 360.0);

    struct T {
        o: Overlays,
        view: View,
        bus: EventBus,
    }

    impl T {
        fn new() -> T {
            T { o: Overlays::default(), view: View::default(), bus: EventBus::new() }
        }
        fn start(&mut self, kind: PickKind, start: Option<[[f32; 2]; 4]>) {
            self.o.start_pick(&self.view, "Test", kind, start, "", &self.bus);
        }
        fn press(&mut self, u: f64, v: f64) -> bool {
            self.o.press(&self.view, u, v, SIZE)
        }
        fn drag(&mut self, u: f64, v: f64) {
            self.o.move_to(&self.view, u, v);
        }
        fn release(&mut self) {
            self.o.release(&self.view, &self.bus);
        }
        fn finish(&mut self) -> bool {
            self.o.finish_pick(&self.view, Some("Test"), &self.bus)
        }
        /// The picks that ended, in order.
        fn got(&self) -> Vec<(String, Option<Pick>)> {
            let mut out = Vec::new();
            while let Some(e) = self.bus.next() {
                if let Event::PreviewPicked { owner, pick } = e {
                    out.push((owner, pick));
                }
            }
            out
        }
        fn only(&self) -> Pick {
            let got = self.got();
            assert_eq!(got.len(), 1, "{got:?}");
            got.into_iter().next().unwrap().1.expect("a pick, not a cancel")
        }
    }

    fn close(a: &[[f32; 2]], b: &[[f32; 2]]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(p, q)| (p[0] - q[0]).abs() < 1e-5 && (p[1] - q[1]).abs() < 1e-5)
    }

    fn views() -> Vec<View> {
        let v = |f: &dyn Fn(&mut View)| { let mut view = View::default(); f(&mut view); view };
        vec![
            View::default(),
            v(&|w| { w.zoom = 2.0; w.pan_x = 0.5; w.pan_y = -0.3; }),
            v(&|w| w.flip_h = true),
            v(&|w| w.flip_v = true),
            v(&|w| w.rotation = Some(90)),
            v(&|w| w.rotation = Some(180)),
            v(&|w| { w.zoom = 1.6; w.pan_x = -1.0; w.flip_h = true; w.rotation = Some(270); }),
        ]
    }

    #[test]
    fn a_point_comes_back_where_it_was_clicked_and_in_the_whole_picture() {
        for view in views() {
            let mut t = T::new();
            t.view = view.clone();
            t.start(PickKind::Point, None);
            t.press(0.3, 0.7);
            t.release();
            let pick = t.only();
            assert_eq!(pick.kind, PickKind::Point);
            assert!(close(&pick.shown, &[[0.3, 0.7]]), "{view:?}");
            assert!(close(&pick.full, &[clamp(to_full(&view, 0.3, 0.7))]), "{view:?}");
        }
    }

    #[test]
    fn zoom_maps_onto_the_part_of_the_picture_shown() {
        let mut t = T::new();
        t.view.zoom = 2.0;
        t.start(PickKind::Point, None);
        t.press(0.0, 0.0);
        t.release();
        assert!(close(&t.only().full, &[[0.25, 0.25]]));
    }

    #[test]
    fn a_rectangle_is_dragged_out_and_comes_back_sorted() {
        let mut t = T::new();
        t.start(PickKind::Rect, None);
        assert!(t.press(0.8, 0.6));
        t.drag(0.2, 0.1);
        t.release();
        assert!(close(&t.only().shown, &[[0.2, 0.1], [0.8, 0.6]]));
    }

    #[test]
    fn a_rotated_rectangle_is_sorted_in_each_space_of_its_own() {
        let mut t = T::new();
        t.view.rotation = Some(90);
        t.view.flip_h = true;
        t.start(PickKind::Rect, None);
        t.press(0.1, 0.2);
        t.drag(0.5, 0.9);
        t.release();
        let pick = t.only();
        let ([x0, y0], [x1, y1]) = (pick.full[0], pick.full[1]);
        assert!(x0 < x1 && y0 < y1);
        let corners: Vec<P> = [(0.1, 0.2), (0.1, 0.9), (0.5, 0.2), (0.5, 0.9)].iter().map(|&(u, v)| to_full(&t.view, u, v)).collect();
        let lo = (corners.iter().map(|c| c.0).fold(9.0, f64::min), corners.iter().map(|c| c.1).fold(9.0, f64::min));
        assert!(close(&[[x0, y0]], &[clamp(lo)]));
    }

    #[test]
    fn a_click_is_not_an_area() {
        let mut t = T::new();
        t.start(PickKind::Rect, None);
        t.press(0.5, 0.5);
        t.release();
        assert!(t.got().is_empty());
        assert_eq!(t.o.picking(), "Test");
        assert!(!t.finish());
    }

    #[test]
    fn quad_corners_start_inset_move_and_finish_with_enter() {
        let mut t = T::new();
        t.start(PickKind::Quad, None);
        let shown: Vec<[f32; 2]> = t.o.shown_points(&t.view).into_iter().map(clamp).collect();
        assert!(close(&shown, &[[0.2, 0.2], [0.8, 0.2], [0.8, 0.8], [0.2, 0.8]]));
        assert!(t.press(0.205, 0.2));
        t.drag(0.05, 0.1);
        t.release();
        assert!(t.got().is_empty());
        assert!(t.o.key(&t.view, "\n", &t.bus));
        let pick = t.only();
        assert_eq!(pick.kind, PickKind::Quad);
        assert!(close(&pick.shown[..1], &[[0.05, 0.1]]));
        assert_eq!(t.o.picking(), "");
    }

    #[test]
    fn dragging_inside_a_quad_moves_all_of_it_and_outside_is_left_to_the_preview() {
        let mut t = T::new();
        t.start(PickKind::Quad, None);
        assert!(!t.press(0.05, 0.05));
        assert!(t.press(0.5, 0.5));
        t.drag(0.6, 0.55);
        t.release();
        t.finish();
        assert!(close(&t.only().shown, &[[0.3, 0.25], [0.9, 0.25], [0.9, 0.85], [0.3, 0.85]]));
    }

    #[test]
    fn dragging_a_quad_to_the_edge_stops_it_there_whole() {
        let mut t = T::new();
        t.start(PickKind::Quad, None);
        assert!(t.press(0.5, 0.5));
        t.drag(1.5, 0.9);
        t.drag(1.6, 0.95);
        t.release();
        t.finish();
        assert!(close(&t.only().shown, &[[0.4, 0.4], [1.0, 0.4], [1.0, 1.0], [0.4, 1.0]]));
    }

    #[test]
    fn a_quad_reaching_past_the_picture_does_not_jump_when_dragged() {
        let start = [[0.1, 0.1], [0.9, 0.1], [0.9, 0.9], [0.1, 0.9]];
        let mut t = T::new();
        t.view.zoom = 2.0;
        t.start(PickKind::Quad, Some(start));
        assert!(t.press(0.5, 0.5));
        t.drag(0.51, 0.5);
        t.release();
        t.finish();
        assert!(close(&t.only().full, &start));
    }

    #[test]
    fn a_quad_past_one_edge_still_moves_away_from_it() {
        let mut t = T::new();
        t.view.zoom = 2.0;
        t.start(PickKind::Quad, Some([[0.3, 0.3], [1.0, 0.3], [1.0, 0.7], [0.3, 0.7]]));
        assert!(t.press(0.5, 0.5));
        t.drag(0.4, 0.5);
        t.release();
        t.finish();
        assert!(close(&t.only().full[..1], &[[0.25, 0.3]]));
    }

    #[test]
    fn quad_corners_stay_on_the_picture_when_the_view_moves() {
        let start = [[0.1, 0.1], [0.9, 0.1], [0.9, 0.9], [0.1, 0.9]];
        let mut t = T::new();
        t.start(PickKind::Quad, Some(start));
        t.view.zoom = 2.0;
        t.view.flip_h = true;
        t.finish();
        let pick = t.only();
        assert!(close(&pick.full, &start));
        assert!(close(&pick.shown[..1], &[[1.0, 0.0]]));
    }

    #[test]
    fn escape_another_pick_or_the_owner_cancels() {
        let mut t = T::new();
        t.start(PickKind::Point, None);
        t.o.start_pick(&t.view, "Other", PickKind::Rect, None, "", &t.bus);
        assert_eq!(t.got(), vec![("Test".to_string(), None)]);
        assert_eq!(t.o.picking(), "Other");
        t.o.cancel_pick(Some("Test"), &t.bus);
        assert_eq!(t.o.picking(), "Other");
        assert!(t.o.key(&t.view, "\u{1b}", &t.bus));
        assert_eq!(t.got(), vec![("Other".to_string(), None)]);
        assert_eq!(t.o.picking(), "");
        assert!(!t.o.key(&t.view, "\u{1b}", &t.bus));
    }

    #[test]
    fn pick_changed_says_who_picks() {
        let mut t = T::new();
        t.start(PickKind::Point, None);
        t.o.cancel_pick(None, &t.bus);
        let mut heard = Vec::new();
        while let Some(e) = t.bus.next() {
            if let Event::PreviewPickChanged { owner } = e {
                heard.push(owner);
            }
        }
        assert_eq!(heard, ["Test", ""]);
    }

    #[test]
    fn a_pick_carries_the_picture_shown_before_any_tint() {
        use telescope_core::frame::{Frame, PixelFormat};
        let frame = |g: u8| Frame::new(4, 4, PixelFormat::Rgba, [g, g, g, 255].repeat(16));
        let mut t = T::new();
        t.o.shown_frame(&frame(1));
        assert_eq!(t.o.picture, None, "nothing is kept without a pick");
        t.start(PickKind::Point, None);
        t.o.shown_frame(&frame(3));
        let last = frame(7);
        t.o.shown_frame(&last);
        t.press(0.5, 0.5);
        t.release();
        assert_eq!(t.only().picture, Some(last));
        assert_eq!(t.o.picture, None, "nothing kept once it's over");
    }

    #[test]
    fn with_a_plugin_moving_the_picture_picks_map_back_through_it() {
        let mut t = T::new();
        let shift = [1.0, 0.0, 0.25, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        t.o.set_warp("Desk", Some(shift));
        let (u, v) = t.o.unwarp(0.75, 0.5);
        assert!((u - 0.5).abs() < 1e-9 && (v - 0.5).abs() < 1e-9);
        t.start(PickKind::Point, None);
        t.press(0.75, 0.5);
        t.release();
        let pick = t.only();
        assert!(close(&pick.shown, &[[0.5, 0.5]]) && close(&pick.in_picture, &[[0.75, 0.5]]));
        t.o.set_warp("Other", None);
        assert!(t.o.warping());
        t.o.set_warp("Desk", None);
        assert!(!t.o.warping());
        assert_eq!(t.o.unwarp(0.75, 0.5), (0.75, 0.5));
    }

    #[test]
    fn a_warp_that_squashes_the_picture_flat_counts_as_none() {
        let mut t = T::new();
        t.o.set_warp("Desk", Some([1.0, 0.0, 0.25, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]));
        t.o.set_warp("Desk", Some([1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]));
        assert!(!t.o.warping());
        assert_eq!(t.o.unwarp(0.3, 0.6), (0.3, 0.6));
    }

    #[test]
    fn the_drawing_has_the_quads_corners_where_the_warp_puts_them() {
        let mut t = T::new();
        t.o.set_warp("Desk", Some([1.0, 0.0, 0.1, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]));
        t.start(PickKind::Quad, None);
        let drawn = t.o.pick_drawing(&t.view).unwrap();
        assert!(drawn.handles);
        assert!(close(&drawn.corners, &[[0.3, 0.2], [0.9, 0.2], [0.9, 0.8], [0.3, 0.8]]));
        assert_eq!(drawn.hint, "Drag the corners into place, then press Enter. Esc cancels.");
    }

    #[test]
    fn a_layers_shapes_are_drawn_in_px_of_the_picture() {
        let shapes = vec![
            Shape::Polygon { points: vec![[0.0, 0.0], [0.5, 0.0], [0.5, 0.5]], stroke: "#5fc98c".into(), width: 3.0, fill: None },
            Shape::Line { points: vec![[0.0, 1.0], [1.0, 1.0]], stroke: "#0000008c".into(), width: 1.0 },
            Shape::Dim { points: vec![[0.25, 0.25], [0.75, 0.25], [0.75, 0.75]], color: "#00000080".into() },
            Shape::Text { x: 0.5, y: 0.5, text: "hi".into(), color: "#ffffff".into(), size: 12.0 },
            Shape::Polygon { points: vec![[0.1, 0.1]], stroke: "#ffffff".into(), width: 1.0, fill: None },
        ];
        let layers = BTreeMap::from([("QR code reader".to_string(), PreviewLayer { shapes })]);
        let (paths, texts) = layer_drawing(&layers, 200.0, 100.0);
        assert_eq!(paths.len(), 3, "a polygon of one point draws nothing");
        assert_eq!(paths[0].commands, "M 0 0 L 100 0 L 100 50 Z");
        assert_eq!(paths[0].stroke, Some([0x5f, 0xc9, 0x8c, 255]));
        assert_eq!(paths[1].commands, "M 0 100 L 200 100");
        assert_eq!(paths[1].stroke, Some([0, 0, 0, 0x8c]));
        assert!(paths[2].evenodd && paths[2].stroke.is_none());
        assert!(paths[2].commands.starts_with("M 0 0 L 200 0 L 200 100 L 0 100 Z M 50 25"));
        assert_eq!(texts, vec![LayerText { x: 100.0, y: 50.0, text: "hi".into(), color: [255; 4], size: 12.0 }]);
        assert_eq!(rgba("#12345"), None);
    }

    #[test]
    fn without_zoom_the_whole_picture_is_the_phone_frame_focus_uses() {
        for view in views().into_iter().filter(|v| v.zoom == 1.0) {
            let (x, y) = to_full(&view, 0.2, 0.9);
            let (fx, fy) = telescope_core::view::inverse_map(0.2, 0.9, 1920.0, 1080.0, &telescope_core::view::Geometry::from(&view));
            assert!((x - fx).abs() < 1e-9 && (y - fy).abs() < 1e-9, "{view:?}");
        }
    }
}
