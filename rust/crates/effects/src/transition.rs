//! Getting from one One picture layout to the next: how long it takes, how it moves and what each moment looks like.
//! A transition is a kind (what happens), a length and an easing. The easings are the app's own springs (rust/crates/ui-kit/STYLE.md,
//! Motion): Springy is the Flick's hair of overshoot, Bouncy a visible one, Smooth none at all, Even a straight line. Tiles that are in
//! both layouts glide the way the Worm's outline does: the edge on the side a tile travels to leads on a stiff spring and the other
//! trails on the chosen one, so a tile stretches towards its place and settles in. Effects (a fade, a wipe) never overshoot.

use crate::composite::Tile;
use serde_json::{json, Value};

pub const DEFAULT_MS: u32 = 600;
pub const MIN_MS: u32 = 100;
pub const MAX_MS: u32 = 3000;
/// What a person who asked for less motion gets: a short fade, which never moves anything.
pub const REDUCED_MS: u32 = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Straight to the new layout
    Cut,
    Crossfade,
    /// The new picture slides in over the old
    Slide,
    /// The new picture pushes the old one out
    Push,
    /// The old picture zooms in and away while the new one zooms up to size
    Zoom,
    Wipe,
    /// Tiles in both layouts travel to their new place and size, the others fade
    Glide,
    /// Like Glide, but tiles set off one after another and fly in a small arc
    Shuffle,
    /// Like Glide, but each tile lands with a small bouncing hop
    Bounce,
    /// The new picture opens from the middle in a circle
    Iris,
    /// The new picture shows through eight slats that open one after another along the picture
    Blinds,
    /// The new picture opens from the middle outwards, side to side
    Curtain,
    /// Both pictures break up into big squares that shrink to nothing again, the new one taking over on the way
    Pixelate,
    /// The new picture appears in small squares in a scattered order
    Dissolve,
}

impl Kind {
    pub const ALL: [Kind; 14] = [
        Kind::Glide, Kind::Shuffle, Kind::Bounce, Kind::Crossfade, Kind::Slide, Kind::Push, Kind::Zoom, Kind::Wipe,
        Kind::Iris, Kind::Blinds, Kind::Curtain, Kind::Pixelate, Kind::Dissolve, Kind::Cut,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Kind::Cut => "cut",
            Kind::Crossfade => "crossfade",
            Kind::Slide => "slide",
            Kind::Push => "push",
            Kind::Zoom => "zoom",
            Kind::Wipe => "wipe",
            Kind::Glide => "glide",
            Kind::Shuffle => "shuffle",
            Kind::Bounce => "bounce",
            Kind::Iris => "iris",
            Kind::Blinds => "blinds",
            Kind::Curtain => "curtain",
            Kind::Pixelate => "pixelate",
            Kind::Dissolve => "dissolve",
        }
    }

    pub fn of(id: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.id() == id)
    }

    /// Whether it moves tiles from one layout's places to the other's (the rest mix two whole pictures).
    pub fn moves_tiles(self) -> bool {
        matches!(self, Kind::Glide | Kind::Shuffle | Kind::Bounce)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ease {
    Springy,
    Smooth,
    Bouncy,
    Even,
}

impl Ease {
    pub const ALL: [Ease; 4] = [Ease::Springy, Ease::Smooth, Ease::Bouncy, Ease::Even];

    pub fn id(self) -> &'static str {
        match self {
            Ease::Springy => "springy",
            Ease::Smooth => "smooth",
            Ease::Bouncy => "bouncy",
            Ease::Even => "even",
        }
    }

    pub fn of(id: &str) -> Option<Ease> {
        Ease::ALL.into_iter().find(|e| e.id() == id)
    }

    /// The damping ratio of its spring: 1 is no overshoot, and Slint's `spring(bounce)` is 1 - bounce.
    fn damping(self) -> f64 {
        match self {
            Ease::Springy => 0.78,
            Ease::Bouncy => 0.5,
            _ => 1.0,
        }
    }
}

/// A transition as set up: what, for how long, and how it eases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    pub kind: Kind,
    pub ms: u32,
    pub ease: Ease,
}

impl Default for Spec {
    fn default() -> Spec {
        Spec { kind: Kind::Glide, ms: DEFAULT_MS, ease: Ease::Springy }
    }
}

impl Spec {
    /// From a saved config: anything missing or unknown is the default's.
    pub fn clean(raw: Option<&Value>) -> Spec {
        let d = Spec::default();
        let Some(o) = raw.and_then(Value::as_object) else { return d };
        Spec {
            kind: o.get("kind").and_then(Value::as_str).and_then(Kind::of).unwrap_or(d.kind),
            ms: o.get("ms").and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(d.ms, |v| (v.round() as i64).clamp(MIN_MS as i64, MAX_MS as i64) as u32),
            ease: o.get("ease").and_then(Value::as_str).and_then(Ease::of).unwrap_or(d.ease),
        }
    }

    pub fn dict(&self) -> Value {
        json!({"kind": self.kind.id(), "ms": self.ms, "ease": self.ease.id()})
    }

    /// What plays for the Motion appearance option: all of it for full, a short fade for reduced (nothing moves, as in the windows)
    /// and a cut for off.
    pub fn for_motion(self, reduced: bool, off: bool) -> Spec {
        if off || self.kind == Kind::Cut {
            Spec { kind: Kind::Cut, ..self }
        } else if reduced {
            Spec { kind: Kind::Crossfade, ms: self.ms.min(REDUCED_MS), ease: Ease::Smooth }
        } else {
            self
        }
    }

    pub fn length_ns(&self) -> u64 {
        u64::from(self.ms) * 1_000_000
    }
}

// ── Easing ────────────────────────────────────────────────────────────────

/// A spring's way from 0 to 1 in unit time, with damping ratio `zeta` (1 or more: critical). It has settled to under a percent at t = 1;
/// what is left is taken up by `finish`.
fn spring(t: f64, zeta: f64) -> f64 {
    let t = t.max(0.0);
    if zeta >= 1.0 {
        let w = 8.0;
        return 1.0 - (1.0 + w * t) * (-w * t).exp();
    }
    // the envelope decays as e^(-6 t)
    let (decay, root) = (6.0, (1.0 - zeta * zeta).sqrt());
    let wd = decay / zeta * root;
    1.0 - (-decay * t).exp() * ((wd * t).cos() + zeta / root * (wd * t).sin())
}

/// Takes the last tenth of the time to land exactly on 1.
fn finish(t: f64, v: f64) -> f64 {
    if t >= 1.0 {
        return 1.0;
    }
    let k = ((t - 0.9) / 0.1).clamp(0.0, 1.0);
    v + (1.0 - v) * k * k * (3.0 - 2.0 * k)
}

/// How far along a spatial move is `t` (0 to 1 of the length) into it: may pass 1 and come back for the springy easings.
pub fn spatial(ease: Ease, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match ease {
        Ease::Even => t,
        _ => finish(t, spring(t, ease.damping())),
    }
}

/// The same for an effect (a fade, a wipe): the easing without its overshoot, held between 0 and 1.
pub fn effect(ease: Ease, t: f64) -> f64 {
    match ease {
        Ease::Even => t.clamp(0.0, 1.0),
        _ => finish(t.clamp(0.0, 1.0), spring(t.clamp(0.0, 1.0), 1.0)).clamp(0.0, 1.0),
    }
}

/// The leading edge of a move: a critical spring that is there in under half the time, as the Worm's is.
fn lead(t: f64) -> f64 {
    finish((t / 0.45).clamp(0.0, 1.0), spring((t / 0.45).clamp(0.0, 1.0), 1.0))
}

// ── Tiles on their way ────────────────────────────────────────────────────

/// A tile at some moment of the transition, and how much of it shows (0 to 1).
#[derive(Clone, Debug, PartialEq)]
pub struct Posed {
    pub tile: Tile,
    pub alpha: f64,
}

fn mix(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// a to b at t (0 to 1), for what is not a place: the crop, the zoom and the look.
fn blended(a: &Tile, b: &Tile, t: f64) -> Tile {
    let n = |x: i64, y: i64| mix(x as f64, y as f64, t).round() as i64;
    Tile {
        zoom: mix(a.zoom, b.zoom, t),
        cx: mix(a.cx, b.cx, t),
        cy: mix(a.cy, b.cy, t),
        crop_l: mix(a.crop_l, b.crop_l, t),
        crop_t: mix(a.crop_t, b.crop_t, t),
        crop_r: mix(a.crop_r, b.crop_r, t),
        crop_b: mix(a.crop_b, b.crop_b, t),
        border: n(a.border, b.border),
        radius: n(a.radius, b.radius),
        ..b.clone()
    }
}

/// The box a tile has on its way from `a` to `b` (x, y, w, h as shares of the picture), a time fraction `t` into the move. The edge on
/// the side it travels to leads; the longer way across goes first and the other axis trails a beat later (the Worm's 0.9 and 1.3 of the time), so the path bows.
fn glide_box(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64), t: f64, ease: Ease, aspect: f64) -> (f64, f64, f64, f64) {
    let (ax1, ay1, bx1, by1) = (a.0 + a.2, a.1 + a.3, b.0 + b.2, b.1 + b.3);
    let (dx, dy) = ((b.0 + b.2 / 2.0 - a.0 - a.2 / 2.0) * aspect, b.1 + b.3 / 2.0 - a.1 - a.3 / 2.0);
    let (tx, ty) = if dx.abs() >= dy.abs() { (t / 0.69, t) } else { (t, t / 0.69) };
    let edge = |from: f64, to: f64, leads: bool, t: f64| mix(from, to, if leads { lead(t.clamp(0.0, 1.0)) } else { spatial(ease, t) });
    let (x0, x1) = (edge(a.0, b.0, dx < 0.0, tx), edge(ax1, bx1, dx >= 0.0, tx));
    let (y0, y1) = (edge(a.1, b.1, dy < 0.0, ty), edge(ay1, by1, dy >= 0.0, ty));
    let least = 0.01;
    (x0, y0, (x1 - x0).max(least), (y1 - y0).max(least))
}

/// How far into its own move the tile that sets off `order`th is, `t` (0 to 1) into a Glide or Shuffle that has `moving` tiles on the way.
fn own(kind: Kind, t: f64, order: usize, moving: usize) -> f64 {
    // Shuffle: each tile sets off a little after the one under it and the last has the rest of the time
    let stagger = if kind == Kind::Shuffle && moving > 1 { 0.5 / moving as f64 } else { 0.0 };
    ((t - stagger * order as f64) / (1.0 - stagger * (moving.max(1) - 1) as f64)).clamp(0.0, 1.0)
}

/// The box of a matched tile at `t`, with a Shuffle's flight in it.
fn box_at(kind: Kind, a: &Tile, b: &Tile, t: f64, order: usize, moving: usize, ease: Ease, aspect: f64) -> (f64, f64, f64, f64) {
    let own = own(kind, t, order, moving);
    let (x, y, w, h) = glide_box((a.x, a.y, a.w, a.h), (b.x, b.y, b.w, b.h), own, ease, aspect);
    // a Shuffle's flight: up and back down by a few percent of the picture's height. A Bounce lands with hops that die away
    let arc = match kind {
        Kind::Shuffle => -0.05 * (std::f64::consts::PI * own).sin(),
        Kind::Bounce => -0.05 * (1.0 - own) * (3.0 * std::f64::consts::PI * own).sin().abs(),
        _ => 0.0,
    };
    (x, y + arc, w, h)
}

fn overlap(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> f64 {
    let (w, h) = ((a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0), (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1));
    if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
}

/// The tiles to draw `t` (0 to 1 of the length) into a Glide or Shuffle, back to front: what is only in the old layout fading away under
/// the rest, then the new layout's tiles, matched ones on their way and new ones growing in as they fade. Tiles are the same stream by
/// source id. Two tiles that change places in the stack swap where their boxes overlap least on the way, so nothing pops. `aspect` is
/// the picture's width over its height.
pub fn poses(kind: Kind, from: &[Tile], to: &[Tile], t: f64, ease: Ease, aspect: f64) -> Vec<Posed> {
    let fade = effect(ease, t);
    let mut out: Vec<Posed> = from.iter().filter(|a| !to.iter().any(|b| b.source_id == a.source_id)).map(|a| {
        let s = mix(1.0, 0.9, fade);
        Posed { tile: shrunk(a, s), alpha: 1.0 - fade }
    }).collect();
    // (index in to, index in from, the order it sets off in) of the matched tiles
    let mut matched: Vec<(usize, usize, usize)> = Vec::new();
    for (n, b) in to.iter().enumerate() {
        if let Some(i) = from.iter().position(|a| a.source_id == b.source_id) {
            matched.push((n, i, matched.len()));
        }
    }
    let moving = matched.len();
    let at = |m: &(usize, usize, usize), t: f64| box_at(kind, &from[m.1], &to[m.0], t, m.2, moving, ease, aspect);
    // when a pair that changes stacking order swaps: where its boxes overlap least
    let swap_at = |x: &(usize, usize, usize), y: &(usize, usize, usize)| -> f64 {
        // of the moments they overlap least (never the first or the last), the one nearest the middle
        (1..20).map(|k| k as f64 / 20.0)
            .min_by(|p, q| overlap(at(x, *p), at(y, *p)).total_cmp(&overlap(at(x, *q), at(y, *q))).then((*p - 0.5).abs().total_cmp(&(*q - 0.5).abs()))).unwrap_or(0.5)
    };
    // above[k]: how many matched tiles the k-th is stacked over right now
    let above: Vec<usize> = matched.iter().map(|x| matched.iter().filter(|y| y.0 != x.0).filter(|y| {
        let (was, will) = (x.1 > y.1, x.0 > y.0);
        if was == will { will } else if t < swap_at(x, y) { was } else { will }
    }).count()).collect();
    let mut stack: Vec<(usize, usize, Posed)> = Vec::new();
    for (n, b) in to.iter().enumerate() {
        let Some(k) = matched.iter().position(|m| m.0 == n) else {
            let s = mix(0.9, 1.0, spatial(ease, t).min(1.0));
            // a new tile comes in on top
            stack.push((usize::MAX, n, Posed { tile: shrunk(b, s), alpha: fade }));
            continue;
        };
        let m = matched[k];
        let a = &from[m.1];
        let (x, y, w, h) = at(&m, t);
        let mut tile = blended(a, b, effect(ease, own(kind, t, m.2, moving)));
        (tile.x, tile.y, tile.w, tile.h) = (x, y, w, h);
        stack.push((above[k], n, Posed { tile, alpha: 1.0 }));
    }
    stack.sort_by_key(|(over, n, _)| (*over, *n));
    out.extend(stack.into_iter().map(|(_, _, p)| p));
    out
}

/// `tile` scaled about its middle.
fn shrunk(tile: &Tile, s: f64) -> Tile {
    let (cx, cy) = (tile.x + tile.w / 2.0, tile.y + tile.h / 2.0);
    Tile { x: cx - tile.w * s / 2.0, y: cy - tile.h * s / 2.0, w: tile.w * s, h: tile.h * s, ..tile.clone() }
}

// ── Whole pictures ────────────────────────────────────────────────────────

/// What one column (or row) of a mix of the old picture (A) and the new one (B) shows: where in A and in B it reads from (0 to 1 across
/// or down) and how much of B goes in. A pixel reads A and B at the spots its column and row give and takes the product of their mixes,
/// so a mix is worked out once per column and once per row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub a: f64,
    pub b: f64,
    pub mix: f64,
}

/// How many slats the Blinds have.
pub const BLINDS: f64 = 8.0;

/// How much of the new picture the pixel at (x, y) of a w x h plane shows, for the two that are not the same down every column
/// (an Iris, a Dissolve), `p` (0 to 1) of the way through.
pub fn mix_at(kind: Kind, x: f64, y: f64, (w, h): (f64, f64), p: f64) -> f64 {
    match kind {
        Kind::Iris => {
            // a circle from the middle that has covered the corners at the end, with a soft edge
            let reach = (w * w + h * h).sqrt() / 2.0;
            let band = 0.18 * reach;
            let radius = p * (reach + band);
            let d = ((x + 0.5 - w / 2.0).powi(2) + (y + 0.5 - h / 2.0).powi(2)).sqrt();
            ((radius - d) / band).clamp(0.0, 1.0)
        }
        Kind::Dissolve => {
            // squares of about a fortieth of the width each coming in at a time of its own
            let size = (w / 40.0).max(1.0);
            let at = scatter((x / size).floor() as u32, (y / size).floor() as u32);
            smooth((p * 1.4 - at * 0.8 - 0.1) / 0.3)
        }
        _ => p,
    }
}

/// A fixed scattered number from 0 to 1 for a square of the Dissolve.
fn scatter(x: u32, y: u32) -> f64 {
    let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B) ^ 0x27D4_EB2F;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    f64::from(h) / f64::from(u32::MAX)
}

fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// What the column (or row, with `vertical`) at s (0 to 1) shows `p` (0 to 1, eased) of the way through the whole-picture transition `kind`.
pub fn cell(kind: Kind, vertical: bool, s: f64, p: f64) -> Cell {
    let same = Cell { a: s, b: s, mix: 1.0 };
    match kind {
        Kind::Slide if !vertical => {
            let edge = 1.0 - p;
            if s >= edge { Cell { a: s, b: s - edge, mix: 1.0 } } else { Cell { a: s, b: s, mix: 0.0 } }
        }
        Kind::Push if !vertical => {
            let edge = 1.0 - p;
            if s < edge { Cell { a: s + p, b: s, mix: 0.0 } } else { Cell { a: s, b: s - edge, mix: 1.0 } }
        }
        Kind::Zoom => {
            let (sa, sb) = (1.0 + 0.6 * p, 0.7 + 0.3 * p);
            let about = |k: f64| 0.5 + (s - 0.5) / k;
            let b = about(sb);
            let inside = if (0.0..=1.0).contains(&b) { 1.0 } else { 0.0 };
            Cell { a: about(sa), b, mix: if vertical { inside } else { smooth(p) * inside } }
        }
        Kind::Wipe if !vertical => {
            let band = 0.15;
            Cell { a: s, b: s, mix: ((p * (1.0 + band) - s) / band).clamp(0.0, 1.0) }
        }
        // eight slats, each opening a little after the one before
        Kind::Blinds if !vertical => {
            let slat = (s * BLINDS).fract();
            let band = 0.3;
            Cell { a: s, b: s, mix: smooth((p * (1.0 + band) - slat) / band) }
        }
        Kind::Curtain if !vertical => {
            let band = 0.15;
            Cell { a: s, b: s, mix: ((p * (1.0 + band) - (2.0 * s - 1.0).abs()) / band).clamp(0.0, 1.0) }
        }
        // a mosaic of big squares that grows and shrinks again, the new picture taking over at the top of it
        Kind::Pixelate => {
            let size = 0.07 * (std::f64::consts::PI * p).sin();
            let q = if size < 0.003 { s } else { let n = 1.0 / size; ((s * n).floor() + 0.5) / n };
            Cell { a: q, b: q, mix: if vertical { 1.0 } else { ((p - 0.35) / 0.3).clamp(0.0, 1.0) } }
        }
        // a Crossfade, and what falls back to one
        Kind::Slide | Kind::Push | Kind::Wipe | Kind::Blinds | Kind::Curtain => same,
        _ => Cell { a: s, b: s, mix: if vertical { 1.0 } else { p } },
    }
}

#[cfg(test)]
#[path = "transition_tests.rs"]
mod tests;
