//! The bottom bar's pins: which readouts are pinned, on which side and in what order, where each one sits and which fold into "+N".
//!
//! This holds no Slint. The bar's Slint (frame.slint) draws what `Bar::view` says, reports how wide each pin really is and where a drag
//! is, and the window turns that into calls here. Plugins reach it only through `Host::offer_bar_stats`, `pin_to_bar` and
//! `bar_stat`, so the Stats plugin and the bar never name each other.
//!
//! Each side stacks from its own edge. Overflow happens in the middle: while the pins don't fit, the fuller side folds its innermost
//! pin into a "+N" chip at its inner end, and no pin ever changes side by itself.

use std::collections::{BTreeMap, BTreeSet};
use telescope_core::bar::{self, BarStat, Side};

/// Nothing wider than this share of the room is given to one pin, so a long camera name can't push the numbers out.
const WIDEST_SHARE: f32 = 0.45;
/// What a pin is taken to be before the bar has measured it, in logical px.
const GUESS_WIDTH: f32 = 40.0;

/// How the bar's Slint measured itself, in logical px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub width: f32,
    /// The room at each edge.
    pub pad: f32,
    /// The room between two pins, and the least between the two sides.
    pub gap: f32,
    /// How wide the "+N" chip is.
    pub chip: f32,
}

impl Default for Metrics {
    fn default() -> Metrics {
        Metrics { width: 0.0, pad: 16.0, gap: 16.0, chip: 24.0 }
    }
}

/// What a pin is doing in the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// In its place.
    Shown,
    /// Folded into the "+N" chip of its side.
    Folded,
    /// Not drawn: unpinned, or nothing to read yet. It keeps the place it left, so it fades out where it was.
    Hidden,
}

/// One pin as the bar draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct PinRow {
    pub id: String,
    pub name: String,
    pub tip: String,
    pub icon: String,
    pub value: String,
    pub sub: String,
    pub tone: String,
    pub state: State,
    pub side: Side,
    /// Where its left edge goes, and how wide it is (logical px, from the bar's left edge).
    pub x: f32,
    pub w: f32,
    /// It can't be unpinned (the camera while two or more streams run).
    pub locked: bool,
}

/// A "+N" chip: how many pins it holds and where it sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chip {
    pub count: usize,
    pub x: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub rows: Vec<PinRow>,
    pub left: Option<Chip>,
    pub right: Option<Chip>,
    /// Pins that were hidden last time and are drawn now: the bar puts them in place first and lets them fade in a moment later, so a
    /// pin that comes back appears where it belongs instead of sliding there from where it left.
    pub returning: Vec<String>,
    /// How much of the bar's left the status word may use before the right side's first pin.
    pub status_room: f32,
}

/// A pin held by the pointer, and where it would land if let go now.
#[derive(Clone, Debug, PartialEq)]
struct Drag {
    id: String,
    side: Side,
    /// Among the pins of that side that are not this one, in drawn order, whether present or not.
    index: usize,
}

#[derive(Default)]
pub struct Bar {
    left: Vec<String>,
    right: Vec<String>,
    own: BTreeMap<String, BarStat>,
    offers: BTreeMap<String, Vec<BarStat>>,
    widths: BTreeMap<String, f32>,
    last_x: BTreeMap<String, f32>,
    /// Every id that has had a row this run, in the order it got one: the Slint list keeps this order, so a row is always the same pin.
    rows: Vec<String>,
    /// The rows drawn (not hidden) last time, to tell which pins have just come back.
    drawn: BTreeSet<String>,
    metrics: Metrics,
    streams: usize,
    drag: Option<Drag>,
    dirty: bool,
    /// The pinned ids the plugins were last told.
    told: Vec<String>,
}

/// The lists a settings file without pins starts with.
fn defaults() -> (Vec<String>, Vec<String>) {
    (bar::DEFAULT_LEFT.iter().map(|s| s.to_string()).collect(), bar::DEFAULT_RIGHT.iter().map(|s| s.to_string()).collect())
}

impl Bar {
    pub fn new() -> Bar {
        let (left, right) = defaults();
        Bar { left, right, metrics: Metrics::default(), dirty: true, ..Bar::default() }
    }

    /// The pins from the settings: None is the defaults.
    pub fn load(&mut self, pins: Option<(Vec<String>, Vec<String>)>) {
        let (left, right) = pins.unwrap_or_else(defaults);
        if (&left, &right) != (&self.left, &self.right) {
            self.left = left;
            self.right = right;
            self.dirty = true;
        }
    }

    /// The pins to save, as (left, right).
    pub fn persisted(&self) -> (Vec<String>, Vec<String>) {
        (self.left.clone(), self.right.clone())
    }

    /// Every pinned id, left side first. The camera counts while two or more streams run.
    pub fn pinned(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.left.iter().chain(&self.right).cloned().collect();
        if self.forced(bar::CAMERA) && !ids.iter().any(|p| p == bar::CAMERA) {
            ids.insert(0, bar::CAMERA.to_string());
        }
        ids
    }

    /// The pinned ids when they differ from what was last told (the first call tells too), for Event::BarPinsChanged.
    pub fn pins_changed(&mut self) -> Option<Vec<String>> {
        let now = self.pinned();
        (now != self.told).then(|| {
            self.told = now.clone();
            now
        })
    }

    pub fn is_pinned(&self, id: &str) -> bool {
        self.left.iter().chain(&self.right).any(|p| p == id) || self.forced(id)
    }

    fn side_of(&self, id: &str) -> Option<Side> {
        if self.left.iter().any(|p| p == id) {
            Some(Side::Left)
        } else if self.right.iter().any(|p| p == id) {
            Some(Side::Right)
        } else if self.forced(id) {
            Some(Side::Left)
        } else {
            None
        }
    }

    /// Whether the camera is pinned however the lists say: it is while two or more streams run, since it says which stream the numbers are of.
    fn forced(&self, id: &str) -> bool {
        id == bar::CAMERA && self.streams >= 2
    }

    /// How many streams run, which the camera pin depends on.
    pub fn set_streams(&mut self, n: usize) {
        if self.streams != n {
            self.streams = n;
            self.dirty = true;
        }
    }

    /// The window's own readouts (the camera, frame rate and throughput), as they read now.
    pub fn set_own(&mut self, stats: Vec<BarStat>) {
        let own: BTreeMap<String, BarStat> = stats.into_iter().map(|s| (s.id.clone(), s)).collect();
        if own != self.own {
            self.own = own;
            self.dirty = true;
        }
    }

    /// What `plugin` offers now, replacing what it offered before. An empty list is a plugin with nothing to show.
    pub fn offer(&mut self, plugin: &str, stats: &[BarStat]) {
        if self.offers.get(plugin).map(Vec::as_slice) != Some(stats) {
            if stats.is_empty() {
                self.offers.remove(plugin);
            } else {
                self.offers.insert(plugin.to_string(), stats.to_vec());
            }
            self.dirty = true;
        }
    }

    /// The reading for this id from the window or whichever plugin offers it.
    pub fn stat(&self, id: &str) -> Option<&BarStat> {
        self.own.get(id).or_else(|| self.offers.values().flatten().find(|s| s.id == id))
    }

    /// Pins a readout at the inner end of the right side, or takes it off. False when nothing changed (it was as asked, or it is the locked camera).
    pub fn pin(&mut self, id: &str, on: bool) -> bool {
        let listed = self.left.iter().chain(&self.right).any(|p| p == id);
        if listed == on || (!on && self.forced(id)) {
            return false;
        }
        if on {
            self.right.insert(0, id.to_string());
        } else {
            self.left.retain(|p| p != id);
            self.right.retain(|p| p != id);
        }
        self.dirty = true;
        true
    }

    /// The bar's own size and spacing, from its Slint.
    pub fn set_metrics(&mut self, metrics: Metrics) {
        if metrics != self.metrics {
            self.metrics = metrics;
            self.dirty = true;
        }
    }

    /// A pin's real width in logical px. Kept as the widest seen while the bar has pins to show, so the digits changing don't move the rest.
    pub fn measured(&mut self, id: &str, width: f32) {
        let width = (width * 2.0).ceil() / 2.0;
        let held = self.widths.entry(id.to_string()).or_insert(0.0);
        if width > *held {
            *held = width;
            self.dirty = true;
        }
    }

    /// Whether something changed since the last look (and so the Slint list wants setting again).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    fn present(&self, id: &str) -> Option<&BarStat> {
        self.stat(id).filter(|s| !s.value.is_empty())
    }

    /// Whether any pinned readout has something to show.
    pub fn shows_any(&self) -> bool {
        self.pinned().iter().any(|id| self.present(id).is_some())
    }

    fn width_of(&self, id: &str, room: f32) -> f32 {
        self.widths.get(id).copied().unwrap_or(GUESS_WIDTH).min((room * WIDEST_SHARE).max(self.metrics.chip))
    }

    /// The two lists as they are drawn: the pins as pinned, the camera forced in while it has to be, and a held pin where it would land.
    fn lists(&self) -> (Vec<String>, Vec<String>) {
        let (mut left, mut right) = (self.left.clone(), self.right.clone());
        if self.forced(bar::CAMERA) && !left.iter().chain(&right).any(|p| p == bar::CAMERA) {
            left.insert(0, bar::CAMERA.to_string());
        }
        if let Some(d) = &self.drag {
            left.retain(|p| *p != d.id);
            right.retain(|p| *p != d.id);
            let list = if d.side == Side::Left { &mut left } else { &mut right };
            list.insert(d.index.min(list.len()), d.id.clone());
        }
        (left, right)
    }

    /// Where each pin goes and which fold, for the Slint list.
    pub fn view(&mut self) -> View {
        let m = self.metrics;
        let room = (m.width - 2.0 * m.pad).max(0.0);
        let (left_ids, right_ids) = self.lists();
        // Widths are forgotten while nothing shows, so the next stream's (a longer camera name) is measured again
        if !self.shows_any() && !self.widths.is_empty() {
            self.widths.clear();
        }

        // Each side from its own edge inwards: the left as drawn, the right reversed
        let outer_in = |ids: &[String], right: bool| -> Vec<(String, f32, bool)> {
            let mut list: Vec<(String, f32, bool)> = ids.iter().filter(|id| self.present(id).is_some()).map(|id| (id.clone(), self.width_of(id, room), id == bar::CAMERA)).collect();
            if right {
                list.reverse();
            }
            list
        };
        let (l, r) = (outer_in(&left_ids, false), outer_in(&right_ids, true));
        let sticky = |p: &Vec<(String, f32, bool)>| p.iter().map(|x| (x.1, x.2)).collect::<Vec<_>>();
        let fit = fold(&sticky(&l), &sticky(&r), room, m.gap, m.chip);

        let mut placed: BTreeMap<String, (State, Side, f32, f32)> = BTreeMap::new();
        let mut cursor = m.pad;
        for (i, (id, w, _)) in l.iter().enumerate() {
            if !fit.left[i] {
                placed.insert(id.clone(), (State::Shown, Side::Left, cursor, *w));
                cursor += w + m.gap;
            }
        }
        let held_left = fit.left.iter().filter(|f| **f).count();
        let left_chip = (held_left > 0).then(|| Chip { count: held_left, x: cursor });
        for (i, (id, w, _)) in l.iter().enumerate() {
            if fit.left[i] {
                placed.insert(id.clone(), (State::Folded, Side::Left, left_chip.map_or(m.pad, |c| c.x), *w));
            }
        }
        let mut cursor = m.width - m.pad;
        let mut any_right_shown = false;
        for (i, (id, w, _)) in r.iter().enumerate() {
            if !fit.right[i] {
                cursor -= w;
                placed.insert(id.clone(), (State::Shown, Side::Right, cursor, *w));
                cursor -= m.gap;
                any_right_shown = true;
            }
        }
        // The left edge of the pins that stay, or the right edge of the bar when none does
        let inner = if any_right_shown { cursor + m.gap } else { m.width - m.pad };
        let held_right = fit.right.iter().filter(|f| **f).count();
        let right_chip = (held_right > 0).then(|| Chip { count: held_right, x: inner - m.chip - if any_right_shown { m.gap } else { 0.0 } });
        for (i, (id, w, _)) in r.iter().enumerate() {
            if fit.right[i] {
                placed.insert(id.clone(), (State::Folded, Side::Right, right_chip.map_or(m.width - m.pad - w, |c| c.x), *w));
            }
        }

        // Every pin that has had a row keeps it, hidden when it is neither pinned nor readable
        let had: BTreeSet<String> = self.rows.iter().cloned().collect();
        for id in left_ids.iter().chain(&right_ids) {
            if !self.rows.contains(id) {
                self.rows.push(id.clone());
            }
        }
        let mut rows = Vec::new();
        let mut drawn = BTreeSet::new();
        let mut returning = Vec::new();
        for id in self.rows.clone() {
            let (state, side, x, w) = match placed.get(&id) {
                Some(&p) => p,
                None => (State::Hidden, self.side_of(&id).unwrap_or(Side::Right), self.last_x.get(&id).copied().unwrap_or(m.pad), self.widths.get(&id).copied().unwrap_or(GUESS_WIDTH)),
            };
            if state != State::Hidden {
                if had.contains(&id) && !self.drawn.contains(&id) {
                    returning.push(id.clone());
                }
                self.last_x.insert(id.clone(), x);
                drawn.insert(id.clone());
            }
            let stat = self.stat(&id).cloned().unwrap_or_default();
            rows.push(PinRow { name: stat.name, tip: stat.tip, icon: stat.icon, value: stat.value, sub: stat.sub, tone: stat.tone, state, side, x, w, locked: self.forced(&id), id });
        }
        self.drawn = drawn;
        let right_edge = rows.iter().filter(|r| r.side == Side::Right && r.state == State::Shown).map(|r| r.x).chain(right_chip.map(|c| c.x)).fold(m.width - m.pad, f32::min);
        let status_room = (right_edge - m.pad - if right_edge < m.width - m.pad { m.gap } else { 0.0 }).max(0.0);
        View { rows, left: left_chip, right: right_chip, returning, status_room }
    }

    /// A pin was taken hold of.
    pub fn drag_start(&mut self, id: &str) {
        let Some(side) = self.side_of(id) else { return };
        let list = if side == Side::Left { &self.left } else { &self.right };
        let index = list.iter().position(|p| p == id).unwrap_or(0);
        self.drag = Some(Drag { id: id.to_string(), side, index });
        self.dirty = true;
    }

    /// The held pin's centre is at `x` in the bar: where it would land. Pins that sit under it slide to make room.
    pub fn drag_to(&mut self, id: &str, x: f32) {
        if self.drag.as_ref().is_none_or(|d| d.id != id) {
            return;
        }
        let m = self.metrics;
        let room = (m.width - 2.0 * m.pad).max(0.0);
        let side = if x < m.width / 2.0 { Side::Left } else { Side::Right };
        // The other pins on that side, packed from their edge the way they would be without this one: where their centres are
        let (mut left, mut right) = (self.left.clone(), self.right.clone());
        if self.forced(bar::CAMERA) && !left.iter().chain(&right).any(|p| p == bar::CAMERA) {
            left.insert(0, bar::CAMERA.to_string());
        }
        left.retain(|p| p != id);
        right.retain(|p| p != id);
        let list = if side == Side::Left { &left } else { &right };
        let present: Vec<(usize, f32)> = list.iter().enumerate().filter(|(_, p)| self.present(p).is_some()).map(|(i, p)| (i, self.width_of(p, room))).collect();
        let mut centres = Vec::new();
        if side == Side::Left {
            let mut at = m.pad;
            for (i, w) in &present {
                centres.push((*i, at + w / 2.0));
                at += w + m.gap;
            }
        } else {
            let mut at = m.width - m.pad;
            for (i, w) in present.iter().rev() {
                centres.push((*i, at - w / 2.0));
                at -= w + m.gap;
            }
        }
        // After the last pin whose centre is left of the pointer
        let index = centres.iter().filter(|(_, c)| *c < x).map(|(i, _)| *i + 1).max().unwrap_or(0);
        let next = Drag { id: id.to_string(), side, index: index.min(list.len()) };
        if self.drag.as_ref() != Some(&next) {
            self.drag = Some(next);
            self.dirty = true;
        }
    }

    /// The pin was let go: it stays where it landed. True when that changed the pins.
    pub fn drag_end(&mut self, id: &str) -> bool {
        let Some(d) = self.drag.take().filter(|d| d.id == id) else { return false };
        self.dirty = true;
        let before = (self.left.clone(), self.right.clone());
        self.left.retain(|p| *p != d.id);
        self.right.retain(|p| *p != d.id);
        let list = if d.side == Side::Left { &mut self.left } else { &mut self.right };
        list.insert(d.index.min(list.len()), d.id.clone());
        before != (self.left.clone(), self.right.clone())
    }

    pub fn dragging(&self) -> Option<&str> {
        self.drag.as_ref().map(|d| d.id.as_str())
    }
}

/// Which pins of each side are folded, from the side's own edge inwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fit {
    pub left: Vec<bool>,
    pub right: Vec<bool>,
}

/// Folds pins into "+N" until both sides fit in `room`. Each list is the pins' (width, sticky) from its own edge inwards, so the last one is
/// the innermost. While they don't fit, the side that is wider at that moment (its chip counted) folds its innermost pin that is not sticky;
/// a sticky pin (the camera, which names the bar) is never folded, whatever it costs the other pins.
pub fn fold(left: &[(f32, bool)], right: &[(f32, bool)], room: f32, gap: f32, chip: f32) -> Fit {
    let width = |pins: &[(f32, bool)], folded: &[bool]| -> f32 {
        let shown: Vec<f32> = pins.iter().zip(folded).filter(|(_, f)| !**f).map(|(p, _)| p.0).collect();
        let any_folded = folded.iter().any(|f| *f);
        let pins_w: f32 = shown.iter().sum::<f32>() + gap * shown.len().saturating_sub(1) as f32;
        if any_folded {
            pins_w + if shown.is_empty() { 0.0 } else { gap } + chip
        } else {
            pins_w
        }
    };
    let innermost = |pins: &[(f32, bool)], folded: &[bool]| (0..pins.len()).rev().find(|i| !folded[*i] && !pins[*i].1);
    let (mut l, mut r) = (vec![false; left.len()], vec![false; right.len()]);
    loop {
        let (wl, wr) = (width(left, &l), width(right, &r));
        let between = if wl > 0.0 && wr > 0.0 { gap } else { 0.0 };
        if wl + wr + between <= room + 0.001 {
            break;
        }
        let (cl, cr) = (innermost(left, &l), innermost(right, &r));
        // The wider side folds, or the other when that one has nothing it may fold
        let pick_left = match (cl, cr) {
            (Some(_), Some(_)) => wl >= wr,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if pick_left {
            l[cl.unwrap()] = true;
        } else {
            r[cr.unwrap()] = true;
        }
    }
    Fit { left: l, right: r }
}

#[cfg(test)]
#[path = "bar_tests.rs"]
mod tests;
