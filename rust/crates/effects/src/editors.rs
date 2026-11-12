//! What the Colour grading card's two hand-drawn controls do with the pointer and the keyboard (widgets/colour_wheel.py and
//! widgets/curve_editor.py). The card draws them and hands over the pointer in pixels and keys by name; the answers here are
//! the new puck or points.

use crate::grade::{monotone_curve, periodic_curve};

const WHEEL_STEP: f64 = 0.01;
const WHEEL_BIG_STEP: f64 = 0.05;
/// How far a drag moves the puck while Shift is held, against 1 for a plain drag.
const WHEEL_FINE: f64 = 0.1;

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

/// A puck on a ring of hues: dragged (it doesn't jump to the pointer, and Shift makes the moves a tenth the size, whenever it is
/// pressed), moved with the arrow keys, and put back in the middle by Home or a double-click.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wheel {
    pub x: f64,
    pub y: f64,
    /// While dragged: where the pointer was when it last reported (the card gives it from where it went down), and where the puck
    /// would be if the circle's edge didn't stop it, so pushing past the rim and coming back takes the same way back.
    drag: Option<Drag>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    pointer: (f64, f64),
    puck: (f64, f64),
}

impl Wheel {
    pub fn value(&self) -> (f64, f64) {
        (self.x, self.y)
    }

    /// Moves the puck (held inside the circle, rounded to four places); whether it moved.
    pub fn set_value(&mut self, x: f64, y: f64) -> bool {
        let r = x.hypot(y);
        let (x, y) = if r > 1.0 { (x / r, y / r) } else { (x, y) };
        let (x, y) = (round4(x), round4(y));
        if (x, y) == (self.x, self.y) {
            return false;
        }
        (self.x, self.y) = (x, y);
        true
    }

    pub fn press(&mut self) {
        self.drag = Some(Drag { pointer: (0.0, 0.0), puck: (self.x, self.y) });
    }

    /// The pointer is (dx, dy) from where it went down, in radii of the puck's area, y down; whether the puck moved. Each report moves
    /// the puck by what the pointer travelled since the last one, so letting Shift go or taking hold of it mid-drag changes the speed
    /// from there on and never makes the puck jump.
    pub fn drag_by(&mut self, dx: f64, dy: f64, shift: bool) -> bool {
        let Some(drag) = self.drag else { return false };
        let speed = if shift { WHEEL_FINE } else { 1.0 };
        let puck = (drag.puck.0 + (dx - drag.pointer.0) * speed, drag.puck.1 - (dy - drag.pointer.1) * speed);
        self.drag = Some(Drag { pointer: (dx, dy), puck });
        self.set_value(puck.0, puck.1)
    }

    pub fn release(&mut self) {
        self.drag = None;
    }

    pub fn double_click(&mut self) -> bool {
        self.drag = None;
        self.set_value(0.0, 0.0)
    }

    /// An arrow key ("left", "right", "up", "down") or Home, Delete and Backspace ("home"); whether the puck moved. None for a
    /// key the wheel doesn't use.
    pub fn key(&mut self, key: &str, shift: bool) -> Option<bool> {
        let step = if shift { WHEEL_BIG_STEP } else { WHEEL_STEP };
        let (dx, dy) = match key {
            "left" => (-step, 0.0),
            "right" => (step, 0.0),
            "up" => (0.0, step),
            "down" => (0.0, -step),
            "home" | "delete" | "backspace" => return Some(self.set_value(0.0, 0.0)),
            _ => return None,
        };
        Some(self.set_value(self.x + dx, self.y + dy))
    }
}

/// The least room between two points of a curve (round the hues for a hue curve).
const GAP: f64 = 0.01;
/// A hue point stays below 1, which is the hue at 0 again.
const HUE_TOP: f64 = 0.9999;
const STEP: f64 = 0.005;
const BIG_STEP: f64 = 0.05;
/// How close a press has to be to a point to pick it up, in pixels.
const GRAB_PX: f64 = 9.0;
/// Qt's default double-click time, for telling a double-click on a point the press just added.
const DOUBLE_CLICK_MS: u64 = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// From black to white with the end points stuck to the edges.
    Tone,
    /// Round the hues (a point near one edge also bends the other); 0.5 means no change.
    Hue,
}

/// A curve's points dragged on a square. Click to add a point, drag to move it, double-click a point (or right-click it) to
/// remove it and double-click anywhere else to reset the curve. Arrows move the picked point, [ and ] pick another, Delete
/// removes it and Home resets.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    pub kind: Kind,
    pub points: Vec<(f64, f64)>,
    default: Vec<(f64, f64)>,
    pub picked: i32,
    dragging: bool,
    /// A point the last press added, and when: a double-click takes it off again.
    added: Option<((f64, f64), u64)>,
}

impl Default for Curve {
    fn default() -> Curve {
        Curve { kind: Kind::Tone, points: Vec::new(), default: Vec::new(), picked: -1, dragging: false, added: None }
    }
}

/// The square the curve is drawn in, inside the widget of size (w, h): (left, top, width, height).
pub fn area(kind: Kind, w: f64, h: f64) -> (f64, f64, f64, f64) {
    let pad = 8.0;
    let bottom = if kind == Kind::Hue { 14.0 } else { pad };
    (pad, pad, w - 2.0 * pad, h - pad - bottom)
}

fn sorted(mut points: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    points.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    points
}

/// What an edit changed, for the card to hand to the plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// The points changed (edited).
    Points,
    /// Only the picked point or the picture changed.
    Look,
    None,
}

impl Curve {
    /// Shows another curve: its kind, points and what Home or a double-click goes back to.
    pub fn set_curve(&mut self, kind: Kind, points: &[(f64, f64)], default: &[(f64, f64)]) {
        self.kind = kind;
        self.points = sorted(points.to_vec());
        self.default = default.to_vec();
        self.picked = self.picked.min(self.points.len() as i32 - 1);
        self.added = None;
    }

    pub fn value_at(&self, x: f64) -> f64 {
        f64::from(match self.kind {
            Kind::Hue => periodic_curve(&self.points, x),
            Kind::Tone => monotone_curve(&self.points, x),
        })
    }

    fn to_px(&self, size: (f64, f64), x: f64, y: f64) -> (f64, f64) {
        let (l, t, w, h) = area(self.kind, size.0, size.1);
        (l + x * w, t + h - y * h)
    }

    fn unit_at(&self, size: (f64, f64), pos: (f64, f64)) -> (f64, f64) {
        let (l, t, w, h) = area(self.kind, size.0, size.1);
        let x = (pos.0 - l) / w.max(1.0);
        let y = (t + h - pos.1) / h.max(1.0);
        (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0))
    }

    /// The point within reach of the pointer (the nearest, the later one on a tie), or -1.
    fn near(&self, size: (f64, f64), pos: (f64, f64)) -> i32 {
        let (mut best, mut best_d) = (-1, GRAB_PX);
        for (i, &(x, y)) in self.points.iter().enumerate() {
            let p = self.to_px(size, x, y);
            let d = (p.0 - pos.0).abs() + (p.1 - pos.1).abs();
            if d <= best_d {
                (best, best_d) = (i as i32, d);
            }
        }
        best
    }

    fn fixed_x(&self, i: usize) -> bool {
        self.kind == Kind::Tone && (i == 0 || i + 1 == self.points.len())
    }

    fn move_point(&mut self, i: usize, x: f64, y: f64) {
        let pts = &mut self.points;
        let x = if self.kind == Kind::Tone && (i == 0 || i + 1 == pts.len()) {
            pts[i].0
        } else if self.kind == Kind::Tone {
            x.max(pts[i - 1].0 + GAP).min(pts[i + 1].0 - GAP)
        } else {
            // between its neighbours round the hues, and below 1
            let lo = if i > 0 { pts[i - 1].0 + GAP } else { (pts[pts.len() - 1].0 - 1.0 + GAP).max(0.0) };
            let hi = if i + 1 < pts.len() { pts[i + 1].0 - GAP } else { HUE_TOP.min(pts[0].0 + 1.0 - GAP) };
            x.max(lo).min(hi)
        };
        pts[i] = (round4(x), round4(y.clamp(0.0, 1.0)));
    }

    /// Adds a point; its index, or -1 when another is too close.
    fn add(&mut self, x: f64, y: f64) -> i32 {
        let x = if self.kind == Kind::Hue {
            let x = x.min(HUE_TOP);
            if self.points.iter().any(|&(px, _)| (px - x).abs().min(1.0 - (px - x).abs()) < GAP) {
                return -1;
            }
            x
        } else {
            if self.points.iter().any(|&(px, _)| (px - x).abs() < GAP) {
                return -1;
            }
            x
        };
        let point = (round4(x), round4(y));
        self.points.push(point);
        self.points = sorted(std::mem::take(&mut self.points));
        self.points.iter().position(|p| *p == point).map_or(-1, |i| i as i32)
    }

    fn removable(&self, i: i32) -> bool {
        i >= 0 && !self.fixed_x(i as usize) && !(self.kind == Kind::Tone && self.points.len() <= 2)
    }

    fn remove(&mut self, i: i32) -> Change {
        if !self.removable(i) {
            return Change::None;
        }
        self.points.remove(i as usize);
        self.picked = i.min(self.points.len() as i32 - 1);
        Change::Points
    }

    fn reset(&mut self) -> Change {
        self.points = sorted(self.default.clone());
        self.picked = -1;
        Change::Points
    }

    /// A button went down at pos (pixels in a widget of `size`) at `now` (ms); `right` for the right button.
    pub fn press(&mut self, size: (f64, f64), pos: (f64, f64), right: bool, now: u64) -> Change {
        if right {
            return self.remove(self.near(size, pos));
        }
        let mut i = self.near(size, pos);
        let added = self.added.take();
        let mut change = Change::Look;
        if i < 0 {
            let (x, _) = self.unit_at(size, pos);
            let y = self.value_at(x);
            i = self.add(x, y);
            if i >= 0 {
                self.added = Some((self.points[i as usize], now));
                change = Change::Points;
            }
        } else if let Some((point, at)) = added {
            if self.points[i as usize] == point && now.saturating_sub(at) <= DOUBLE_CLICK_MS {
                // the second press of a double-click on the point the first one added
                self.added = Some((point, at));
            }
        }
        self.picked = i;
        self.dragging = i >= 0;
        change
    }

    pub fn move_to(&mut self, size: (f64, f64), pos: (f64, f64)) -> Change {
        if self.dragging && self.picked >= 0 {
            let (x, y) = self.unit_at(size, pos);
            self.move_point(self.picked as usize, x, y);
            self.added = None;
            return Change::Points;
        }
        Change::None
    }

    /// Whether a point is under the pointer, for the cursor.
    pub fn over_point(&self, size: (f64, f64), pos: (f64, f64)) -> bool {
        self.near(size, pos) >= 0
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    pub fn double_click(&mut self, size: (f64, f64), pos: (f64, f64)) -> Change {
        let added = self.added.take();
        self.dragging = false;
        if let Some((point, _)) = added {
            if let Some(at) = self.points.iter().position(|p| *p == point) {
                // the first click of this double-click added it
                self.points.remove(at);
                return self.reset();
            }
        }
        let i = self.near(size, pos);
        if self.removable(i) { self.remove(i) } else { self.reset() }
    }

    /// A key by name: "left", "right", "up", "down", "[" , "]", "delete" (also Backspace) and "home". None for another key.
    pub fn key(&mut self, key: &str, shift: bool) -> Option<Change> {
        let n = self.points.len() as i32;
        let step = if shift { BIG_STEP } else { STEP };
        match key {
            "left" | "right" | "up" | "down" if n > 0 => {
                if self.picked < 0 {
                    self.picked = 0;
                }
                let (x, y) = self.points[self.picked as usize];
                let (dx, dy) = match key {
                    "left" => (-step, 0.0),
                    "right" => (step, 0.0),
                    "up" => (0.0, step),
                    _ => (0.0, -step),
                };
                self.move_point(self.picked as usize, x + dx, y + dy);
                Some(Change::Points)
            }
            "[" | "]" if n > 0 => {
                let forward = key == "]";
                self.picked = if self.picked >= 0 { (self.picked + if forward { 1 } else { -1 }).rem_euclid(n) } else { 0 };
                Some(Change::Look)
            }
            "delete" | "backspace" => Some(self.remove(self.picked)),
            "home" => Some(self.reset()),
            _ => None,
        }
    }

    /// The line as the card draws it: `samples` points along x, as (x, y) in 0..1.
    pub fn line(&self, samples: usize) -> Vec<(f64, f64)> {
        let n = samples.max(2);
        (0..n).map(|i| { let x = i as f64 / (n - 1) as f64; (x, self.value_at(x)) }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grade::{neutral_hue, LINEAR};

    const SIZE: (f64, f64) = (216.0, 216.0);

    fn wheel() -> Wheel {
        Wheel::default()
    }

    fn editor() -> Curve {
        let mut e = Curve::default();
        e.set_curve(Kind::Tone, &LINEAR, &LINEAR);
        e
    }

    fn at(e: &Curve, x: f64, y: f64) -> (f64, f64) {
        let p = e.to_px(SIZE, x, y);
        (p.0.round(), p.1.round())
    }

    fn click(e: &mut Curve, x: f64, y: f64) -> Change {
        let p = at(e, x, y);
        let c = e.press(SIZE, p, false, 0);
        e.release();
        c
    }

    #[test]
    fn arrow_keys_move_the_puck_and_home_puts_it_back() {
        let mut w = wheel();
        assert_eq!(w.key("right", false), Some(true));
        assert_eq!(w.key("up", true), Some(true));
        assert_eq!(w.value(), (0.01, 0.05));
        assert_eq!(w.key("home", false), Some(true));
        assert_eq!(w.value(), (0.0, 0.0));
        assert_eq!(w.key("x", false), None);
    }

    #[test]
    fn dragging_moves_the_puck_without_jumping_and_stays_in_the_circle() {
        let mut w = wheel();
        w.press();
        assert_eq!(w.value(), (0.0, 0.0), "pressing alone doesn't move it");
        w.drag_by(20.0 / 50.0, 0.0, false);
        assert!(w.x > 0.3 && w.y == 0.0);
        w.drag_by(400.0 / 50.0, -400.0 / 50.0, false);
        assert!((w.x.hypot(w.y) - 1.0).abs() < 1e-3 && w.y > 0.0);
        w.release();
        assert!(!w.drag_by(1.0, 1.0, false), "no drag without a press");
    }

    #[test]
    fn shift_moves_the_puck_a_tenth_as_far() {
        let mut w = wheel();
        w.press();
        w.drag_by(0.4, 0.0, true);
        assert!((w.x - 0.04).abs() < 1e-9);
        // and small values are there to be had: one pixel of a 50 pixel radius, with Shift
        let mut w = wheel();
        w.press();
        w.drag_by(1.0 / 50.0, 0.0, true);
        assert_eq!(w.value(), (0.002, 0.0));
    }

    #[test]
    fn shift_going_down_or_up_mid_drag_never_makes_the_puck_jump() {
        let mut w = wheel();
        w.press();
        w.drag_by(0.2, 0.0, false);
        let before = w.value();
        // the same pointer place, Shift now held: nothing moves
        assert!(!w.drag_by(0.2, 0.0, true));
        assert_eq!(w.value(), before);
        // further on at the fine speed
        w.drag_by(0.4, 0.0, true);
        assert!((w.x - (before.0 + 0.02)).abs() < 1e-9);
        // and Shift let go again: back to the full speed from here
        let fine = w.x;
        w.drag_by(0.5, 0.0, false);
        assert!((w.x - (fine + 0.1)).abs() < 1e-9);
    }

    #[test]
    fn pushing_past_the_rim_and_coming_back_retraces_the_way() {
        let mut w = wheel();
        w.press();
        w.drag_by(1.5, 0.0, false);
        assert!((w.x - 1.0).abs() < 1e-9, "stopped at the rim");
        w.drag_by(1.2, 0.0, false);
        assert!((w.x - 1.0).abs() < 1e-9, "still held by the rim while the pointer is past it");
        w.drag_by(0.5, 0.0, false);
        assert!((w.x - 0.5).abs() < 1e-9, "and follows once the pointer is back inside");
    }

    #[test]
    fn a_drag_never_snaps_to_the_middle_or_a_grid() {
        let mut w = wheel();
        w.press();
        let mut seen = Vec::new();
        for i in 1..=10 {
            w.drag_by(i as f64 * 0.001, 0.0, false);
            seen.push(w.x);
        }
        assert_eq!(seen, [0.001, 0.002, 0.003, 0.004, 0.005, 0.006, 0.007, 0.008, 0.009, 0.01]);
    }

    #[test]
    fn a_double_click_resets_the_wheel() {
        let mut w = wheel();
        w.set_value(0.4, 0.2);
        assert!(w.double_click());
        assert_eq!(w.value(), (0.0, 0.0));
    }

    #[test]
    fn a_click_adds_a_point_on_the_line_and_a_drag_moves_it() {
        let mut e = editor();
        let p = at(&e, 0.5, 0.5);
        e.press(SIZE, p, false, 0);
        assert!(e.points.len() == 3 && (e.points[1].1 - 0.5).abs() < 0.02);
        let to = at(&e, 0.5, 0.7);
        assert_eq!(e.move_to(SIZE, to), Change::Points);
        e.release();
        assert!((e.points[1].1 - 0.7).abs() < 0.02);
    }

    #[test]
    fn the_ends_of_a_tone_curve_stay_at_the_edges() {
        let mut e = editor();
        let p = at(&e, 0.0, 0.0);
        e.press(SIZE, p, false, 0);
        let to = at(&e, 0.3, 0.2);
        e.move_to(SIZE, to);
        e.release();
        assert!(e.points[0].0 == 0.0 && (e.points[0].1 - 0.2).abs() < 0.02);
        let on = at(&e, e.points[0].0, e.points[0].1);
        e.double_click(SIZE, on);
        assert_eq!(e.points, LINEAR, "an end point can't be removed, so the double-click resets the curve");
    }

    #[test]
    fn keys_pick_move_and_remove_points() {
        let mut e = editor();
        e.set_curve(Kind::Tone, &[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)], &LINEAR);
        e.key("]", false);
        e.key("]", false);
        assert_eq!(e.picked, 1);
        e.key("up", true);
        assert_eq!(e.points[1], (0.5, 0.55));
        e.key("delete", false);
        assert_eq!(e.points, LINEAR);
        e.set_curve(Kind::Tone, &[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)], &LINEAR);
        assert_eq!(e.key("home", false), Some(Change::Points));
        assert_eq!(e.points, LINEAR);
    }

    #[test]
    fn a_point_on_a_hue_curve_comes_off_with_a_double_click() {
        let mut e = editor();
        e.set_curve(Kind::Hue, &neutral_hue(), &neutral_hue());
        let p = at(&e, 1.0 / 3.0, 0.5);
        e.double_click(SIZE, p);
        assert_eq!(e.points.len(), 5);
        let p = at(&e, 0.6, 0.9);
        e.double_click(SIZE, p);
        assert_eq!(e.points, neutral_hue());
    }

    #[test]
    fn a_right_click_removes_a_point() {
        let mut e = editor();
        e.set_curve(Kind::Tone, &[(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)], &LINEAR);
        let p = at(&e, 0.5, 0.6);
        e.press(SIZE, p, true, 0);
        assert_eq!(e.points, LINEAR);
    }

    #[test]
    fn tone_points_keep_their_order_while_dragged() {
        let mut e = editor();
        e.set_curve(Kind::Tone, &[(0.0, 0.0), (0.4, 0.4), (0.6, 0.6), (1.0, 1.0)], &LINEAR);
        let p = at(&e, 0.4, 0.4);
        e.press(SIZE, p, false, 0);
        let to = at(&e, 0.9, 0.4);
        e.move_to(SIZE, to);
        e.release();
        let xs: Vec<f64> = e.points.iter().map(|p| p.0).collect();
        assert!(xs.windows(2).all(|w| w[0] <= w[1]) && xs[1] < 0.6);
    }

    #[test]
    fn the_curve_drawn_is_the_one_applied() {
        let mut e = editor();
        let pts = [(0.0, 0.1), (0.3, 0.2), (1.0, 0.9)];
        e.set_curve(Kind::Tone, &pts, &LINEAR);
        for (x, y) in e.line(11) {
            assert!((y - f64::from(monotone_curve(&pts, x))).abs() < 1e-9);
        }
    }

    #[test]
    fn a_real_double_click_on_the_curve_resets_it() {
        // Press (adds a point), release, maybe a second press, then the double-click event
        for second_press in [false, true] {
            let mut e = editor();
            e.set_curve(Kind::Tone, &[(0.0, 0.0), (0.3, 0.5), (1.0, 1.0)], &LINEAR);
            let y = e.value_at(0.7);
            let p = at(&e, 0.7, y);
            e.press(SIZE, p, false, 100);
            assert_eq!(e.points.len(), 4, "the first press adds a point");
            e.release();
            if second_press {
                e.press(SIZE, p, false, 200);
            }
            assert_eq!(e.double_click(SIZE, p), Change::Points, "the reset is told like any edit");
            e.release();
            assert_eq!(e.points, LINEAR);
        }
    }

    #[test]
    fn a_real_double_click_on_a_point_still_removes_it() {
        let mut e = editor();
        e.set_curve(Kind::Tone, &[(0.0, 0.0), (0.3, 0.5), (0.7, 0.6), (1.0, 1.0)], &LINEAR);
        let p = at(&e, 0.3, 0.5);
        e.press(SIZE, p, false, 100);
        e.release();
        e.double_click(SIZE, p);
        assert_eq!(e.points, vec![(0.0, 0.0), (0.7, 0.6), (1.0, 1.0)]);
    }

    #[test]
    fn hue_points_stay_below_1_and_apart_round_the_hues() {
        let mut e = editor();
        e.set_curve(Kind::Hue, &neutral_hue(), &neutral_hue());
        let p = at(&e, 5.0 / 6.0, 0.5);
        e.press(SIZE, p, false, 0);
        let to = at(&e, 1.0, 0.7);
        e.move_to(SIZE, to);
        e.release();
        let xs: Vec<f64> = e.points.iter().map(|p| p.0).collect();
        assert!(xs.len() == 6 && xs.iter().cloned().fold(0.0, f64::max) < 1.0 && 1.0 - xs.iter().cloned().fold(0.0, f64::max) >= 0.0099);
        let count = e.points.len();
        click(&mut e, 1.0, 0.2);
        assert_eq!(e.points.len(), count, "a click at 1 would land on the hue at 0");
    }

    #[test]
    fn arrow_keys_keep_a_hue_point_in_range() {
        let mut e = editor();
        e.set_curve(Kind::Hue, &neutral_hue(), &neutral_hue());
        e.key("]", false);
        assert_eq!(e.picked, 0);
        for _ in 0..5 {
            e.key("left", true);
        }
        assert_eq!(e.points[0].0, 0.0);
        e.key("[", false);
        for _ in 0..10 {
            e.key("right", true);
        }
        assert!(e.points.iter().all(|p| (0.0..1.0).contains(&p.0)));
        assert_eq!(e.points.last().unwrap().0, 0.99);
    }
}
