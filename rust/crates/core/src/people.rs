//! Calm people tracking on top of the vision service's people (people.py): steady ids, smoothed boxes and faces, and a
//! people count that's quick to notice someone new and slow to let them go, since the pose model drops people and finds
//! them again all the time. The host runs one PeopleTracking for every stream and hands out its Tracks; Privacy blur and
//! Expose for my face read them back with Track::from_value and pick you with main().

use crate::pose::Person;
use serde_json::{json, Value};
use std::collections::HashMap;

/// A landmark counts at this visibility.
pub const SEEN: f64 = 0.5;
/// A new person counts once found in this many updates in a row.
pub const CONFIRM: u32 = 2;
/// Someone counted is only let go after being missing this long.
pub const DROP_S: f64 = 3.0;
/// Boxes and faces follow the model with this time constant.
pub const SMOOTH_S: f64 = 0.25;
/// A person found again within this many of their own sizes of where they were going is the same person.
pub const GATE: f64 = 1.2;
/// And the reach grows by this many sizes a second while they're missing.
pub const GATE_GROWTH: f64 = 1.5;
pub const GATE_MOST: f64 = 4.0;
/// A stream with no update for this long is forgotten.
pub const FORGET_S: f64 = 60.0;
/// A second skeleton overlapping someone found this much (intersection over union) is them again.
pub const DUPLICATE: f64 = 0.6;
const FACE: [&str; 5] = ["nose", "left_eye", "right_eye", "left_ear", "right_ear"];

/// (x0, y0, x1, y1), 0..1 of the picture as shown at zoom 1.
pub type Box4 = (f64, f64, f64, f64);

/// One person followed across updates, in the picture as shown at zoom 1 (as Person has it).
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: u64,
    /// Around the landmarks seen, smoothed.
    pub bbox: Box4,
    /// The middle of the face, smoothed; None until a face point has been seen.
    pub face: Option<(f64, f64)>,
    /// How long since they were first found.
    pub seen_s: f64,
    /// How long since they were last found; 0 while they're found.
    pub missing_s: f64,
}

impl Track {
    pub fn centre(&self) -> (f64, f64) {
        centre(self.bbox)
    }

    /// The record events and tracked_people carry: {"id", "box": [x0, y0, x1, y1], "face": [x, y] or null, "seen_s", "missing_s"}.
    pub fn to_value(&self) -> Value {
        let b = self.bbox;
        json!({"id": self.id, "box": [b.0, b.1, b.2, b.3], "face": self.face.map(|f| vec![f.0, f.1]),
               "seen_s": self.seen_s, "missing_s": self.missing_s})
    }

    /// A Track from its record; None when it isn't one.
    pub fn from_value(value: &Value) -> Option<Track> {
        let num = |v: &Value| v.as_f64();
        let b = value.get("box")?.as_array()?;
        if b.len() != 4 {
            return None;
        }
        let face = match value.get("face") {
            Some(Value::Array(f)) if f.len() == 2 => Some((num(&f[0])?, num(&f[1])?)),
            _ => None,
        };
        Some(Track {
            id: value.get("id")?.as_u64()?,
            bbox: (num(&b[0])?, num(&b[1])?, num(&b[2])?, num(&b[3])?),
            face,
            seen_s: value.get("seen_s").and_then(num).unwrap_or(0.0),
            missing_s: value.get("missing_s").and_then(num).unwrap_or(0.0),
        })
    }

    /// Every Track in a list of records, skipping anything that isn't one.
    pub fn all_from(values: &[Value]) -> Vec<Track> {
        values.iter().filter_map(Track::from_value).collect()
    }
}

/// Around the landmarks the model is sure of, or None with fewer than two.
pub fn box_of(person: &Person) -> Option<Box4> {
    let points: Vec<(f64, f64)> = person.points.keys().filter_map(|name| person.point(name, SEEN)).collect();
    if points.len() < 2 {
        return None;
    }
    let fold = |f: fn(f64, f64) -> f64, pick: fn(&(f64, f64)) -> f64| points.iter().map(pick).reduce(f).unwrap();
    Some((fold(f64::min, |p| p.0), fold(f64::min, |p| p.1), fold(f64::max, |p| p.0), fold(f64::max, |p| p.1)))
}

/// The middle of the face points the model is sure of, or None.
pub fn face_of(person: &Person) -> Option<(f64, f64)> {
    let points: Vec<(f64, f64)> = FACE.iter().filter_map(|name| person.point(name, SEEN)).collect();
    if points.is_empty() {
        return None;
    }
    let n = points.len() as f64;
    Some((points.iter().map(|p| p.0).sum::<f64>() / n, points.iter().map(|p| p.1).sum::<f64>() / n))
}

/// Whoever's been there longest (you, usually): the one a plugin follows, and the one it doesn't blur.
pub fn main(tracks: &[Track]) -> Option<&Track> {
    // max() keeps the first of equals like Python's max, so walk with a strict greater-than
    let mut best: Option<&Track> = None;
    for t in tracks {
        let better = match best {
            None => true,
            Some(b) => (t.seen_s, area(t.bbox)).partial_cmp(&(b.seen_s, area(b.bbox))) == Some(std::cmp::Ordering::Greater),
        };
        if better {
            best = Some(t);
        }
    }
    best
}

pub fn area(b: Box4) -> f64 {
    (b.2 - b.0).max(0.0) * (b.3 - b.1).max(0.0)
}

/// Intersection over union of two boxes.
pub fn overlap(a: Box4, b: Box4) -> f64 {
    let inter = area((a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3)));
    let union = area(a) + area(b) - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

fn size(b: Box4) -> f64 {
    (b.2 - b.0).max(b.3 - b.1).max(0.02)
}

fn centre(b: Box4) -> (f64, f64) {
    ((b.0 + b.2) / 2.0, (b.1 + b.3) / 2.0)
}

fn ease4(o: Box4, n: Box4, a: f64) -> Box4 {
    (o.0 + (n.0 - o.0) * a, o.1 + (n.1 - o.1) * a, o.2 + (n.2 - o.2) * a, o.3 + (n.3 - o.3) * a)
}

fn ease2(o: (f64, f64), n: (f64, f64), a: f64) -> (f64, f64) {
    (o.0 + (n.0 - o.0) * a, o.1 + (n.1 - o.1) * a)
}

#[derive(Clone, Debug)]
struct Follow {
    id: u64,
    bbox: Box4,
    face: Option<(f64, f64)>,
    velocity: (f64, f64),
    first_at: f64,
    last_at: f64,
    hits: u32,
    counted: bool,
}

impl Follow {
    fn new(id: u64, bbox: Box4, face: Option<(f64, f64)>, now: f64) -> Follow {
        Follow { id, bbox, face, velocity: (0.0, 0.0), first_at: now, last_at: now, hits: 1, counted: false }
    }

    fn predicted(&self, now: f64) -> (f64, f64) {
        let (cx, cy) = centre(self.bbox);
        let ahead = (now - self.last_at).min(1.0);
        (cx + self.velocity.0 * ahead, cy + self.velocity.1 * ahead)
    }

    fn found(&mut self, bbox: Box4, face: Option<(f64, f64)>, now: f64, missed: bool) {
        let dt = (now - self.last_at).max(1e-3);
        let a = 1.0 - (-dt / SMOOTH_S).exp();
        let before = centre(self.bbox);
        self.bbox = ease4(self.bbox, bbox, a);
        if let Some(face) = face {
            self.face = Some(match self.face {
                None => face,
                Some(old) => ease2(old, face, a),
            });
        }
        let moved = centre(self.bbox);
        let step = ((moved.0 - before.0) / dt, (moved.1 - before.1) / dt);
        self.velocity = ease2(self.velocity, step, 0.5);
        self.last_at = now;
        self.hits = if missed { 1 } else { self.hits + 1 };
        if self.hits >= CONFIRM {
            self.counted = true;
        }
    }

    fn mapped(&mut self, to: &dyn Fn(f64, f64) -> (f64, f64)) {
        let b = self.bbox;
        let corners = [to(b.0, b.1), to(b.0, b.3), to(b.2, b.1), to(b.2, b.3)];
        let fold = |f: fn(f64, f64) -> f64, pick: fn(&(f64, f64)) -> f64| corners.iter().map(pick).reduce(f).unwrap();
        self.bbox = (fold(f64::min, |p| p.0), fold(f64::min, |p| p.1), fold(f64::max, |p| p.0), fold(f64::max, |p| p.1));
        self.face = self.face.map(|(x, y)| to(x, y));
        self.velocity = (0.0, 0.0);
    }

    fn track(&self, now: f64) -> Track {
        Track { id: self.id, bbox: self.bbox, face: self.face, seen_s: now - self.first_at, missing_s: now - self.last_at }
    }
}

/// One stream's people, from each update's Persons.
#[derive(Clone, Debug)]
pub struct PeopleTracker {
    follows: Vec<Follow>,
    next_id: u64,
    updated_at: Option<f64>,
}

impl Default for PeopleTracker {
    fn default() -> Self {
        PeopleTracker { follows: Vec::new(), next_id: 1, updated_at: None }
    }
}

impl PeopleTracker {
    pub fn new() -> PeopleTracker {
        PeopleTracker::default()
    }

    /// The people counted after this update. Someone new counts once found CONFIRM updates in a row; someone counted
    /// stays (missing_s growing) until missing for DROP_S.
    pub fn update(&mut self, people: &[Person], now: f64) -> Vec<Track> {
        let found: Vec<(Box4, Option<(f64, f64)>)> = people.iter().filter_map(|p| box_of(p).map(|b| (b, face_of(p)))).collect();
        let mut pairs: Vec<(f64, bool, usize, usize)> = Vec::new();
        for (i, f) in self.follows.iter().enumerate() {
            let (px, py) = f.predicted(now);
            let reach = (GATE + GATE_GROWTH * (now - f.last_at)).min(GATE_MOST) * size(f.bbox);
            for (j, (b, _)) in found.iter().enumerate() {
                let (cx, cy) = centre(*b);
                let distance = (cx - px).hypot(cy - py);
                if distance <= reach {
                    pairs.push((distance / size(f.bbox), !f.counted, i, j));
                }
            }
        }
        // nearest first, and someone already counted before someone new at the same distance
        pairs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let (mut taken_f, mut taken_p): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
        for &(_cost, _new, i, j) in &pairs {
            if taken_f.contains(&i) || taken_p.contains(&j) {
                continue;
            }
            taken_f.push(i);
            taken_p.push(j);
            let updated_at = self.updated_at;
            let f = &mut self.follows[i];
            let missed = updated_at.is_some_and(|u| f.last_at < u);
            f.found(found[j].0, found[j].1, now, missed);
        }
        let mut kept: Vec<Follow> = Vec::new();
        for (i, f) in self.follows.iter().enumerate() {
            if taken_f.contains(&i) || (f.counted && now - f.last_at < DROP_S) {
                kept.push(f.clone());
            }
        }
        // claimed in the order Python's set iteration gives small ints: ascending
        let mut sorted_p = taken_p.clone();
        sorted_p.sort_unstable();
        let mut claimed: Vec<Box4> = sorted_p.iter().map(|&j| found[j].0).collect();
        for (j, (b, face)) in found.iter().enumerate() {
            if !taken_p.contains(&j) && !claimed.iter().any(|other| overlap(*b, *other) > DUPLICATE) {
                claimed.push(*b);
                kept.push(Follow::new(self.next_id, *b, *face, now));
                self.next_id += 1;
            }
        }
        self.follows = kept;
        self.updated_at = Some(now);
        self.tracks(now)
    }

    pub fn tracks(&self, now: f64) -> Vec<Track> {
        self.follows.iter().filter(|f| f.counted).map(|f| f.track(now)).collect()
    }

    /// How many people there are, steadied.
    pub fn count(&self) -> u32 {
        self.follows.iter().filter(|f| f.counted).count() as u32
    }

    /// Everyone's position put through to(x, y): the picture turned or flipped, and they're the same people.
    pub fn remap(&mut self, to: &dyn Fn(f64, f64) -> (f64, f64)) {
        for f in &mut self.follows {
            f.mapped(to);
        }
    }

    pub fn reset(&mut self) {
        self.follows.clear();
        self.updated_at = None;
    }
}

type Latest = (Vec<Track>, u32, f64);

/// A PeopleTracker per stream, by source id.
#[derive(Clone, Debug, Default)]
pub struct PeopleTracking {
    trackers: HashMap<String, PeopleTracker>,
    /// source id: (tracks, count, when), in the order streams were first heard from
    last: Vec<(String, Latest)>,
}

impl PeopleTracking {
    pub fn new() -> PeopleTracking {
        PeopleTracking::default()
    }

    /// (tracks, count) for source_id after this update.
    pub fn update(&mut self, source_id: &str, people: &[Person], now: f64) -> (Vec<Track>, u32) {
        let stale: Vec<String> = self.last.iter().filter(|(_, (_, _, at))| now - at > FORGET_S).map(|(s, _)| s.clone()).collect();
        for sid in stale {
            self.drop_stream(&sid);
        }
        let tracker = self.trackers.entry(source_id.to_string()).or_default();
        let tracks = tracker.update(people, now);
        let count = tracker.count();
        let entry = (tracks.clone(), count, now);
        match self.last.iter_mut().find(|(s, _)| s == source_id) {
            Some(slot) => slot.1 = entry,
            None => self.last.push((source_id.to_string(), entry)),
        }
        (tracks, count)
    }

    /// What the last update for source_id gave, or ([], 0) without one in DROP_S (nothing asks for people, or the stream
    /// stopped).
    pub fn last(&self, source_id: &str, now: f64) -> (Vec<Track>, u32) {
        match self.last.iter().find(|(s, _)| s == source_id) {
            Some((_, (tracks, count, at))) if now - at <= DROP_S => (tracks.clone(), *count),
            _ => (Vec::new(), 0),
        }
    }

    /// source_id's stream ended.
    pub fn drop_stream(&mut self, source_id: &str) {
        self.trackers.remove(source_id);
        self.last.retain(|(s, _)| s != source_id);
    }

    /// Whether a stream's tracker is still kept.
    pub fn tracking(&self, source_id: &str) -> bool {
        self.trackers.contains_key(source_id)
    }

    /// Everyone on every stream put through to(x, y), keeping their ids (the picture turned or flipped).
    pub fn remap(&mut self, to: &dyn Fn(f64, f64) -> (f64, f64)) {
        for (sid, tracker) in self.trackers.iter_mut() {
            tracker.remap(to);
            if let Some((_, entry)) = self.last.iter_mut().find(|(s, _)| s == sid) {
                entry.0 = tracker.tracks(entry.2);
            }
        }
    }
}

#[cfg(test)]
#[path = "people_tests.rs"]
mod tests;
