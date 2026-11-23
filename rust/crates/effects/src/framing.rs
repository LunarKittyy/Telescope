//! The Auto framing plugin (plugins/framing.py): zooms and pans to keep the people in the picture framed, through the window's view.

use crate::peek;
use crate::shared::Paired;
use crate::vision_row;
use serde_json::{json, Value};
use std::collections::VecDeque;
use telescope_core::actions::{self, round_half_even, SliderSpec};
use telescope_core::event::Event;
use telescope_core::host::{Host, PickKind, View};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::settings::{off_default, reset_config};
use telescope_core::pose::{arm, body, hands_out, upright, Person, ARM_SEEN, SEEN, SIDES};
use telescope_core::time::monotonic_ns;
use telescope_core::ui::{ActionSpec, CardSpec, SlintSource, UiEvent, UiSpec};
use telescope_core::view::{pan_for, shown_to_view, view_to_shown, window_centre, Geometry};
use telescope_core::vision::{turned, Turn};

pub const NAME: &str = "framing";
const TITLE: &str = n_("Auto framing");
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/vision_status.slint"), "\n", include_str!("../ui/framing.slint"));

/// Framing runs through these, from 0 to len - 1, and anywhere in between.
// Translators: keep these short, they sit in the slider's narrow readout column
pub const FRAMINGS: [&str; 4] = [n_("Head"), n_("Chest"), n_("Waist"), n_("Body")];
pub const FRAMING_DEFAULT: f64 = 1.0;
/// Slider steps between two of them.
const FRAMING_STEPS: i64 = 10;
/// At each, half the box's width in shoulder widths (at Head about the head's own width, so it never pulls the shoulders in).
const HALF_WIDTH: [f64; 4] = [0.28, 0.75, 1.0, 1.2];
/// At each, how far the box's top is above the eyes, in shoulder widths (at Head just above the hair).
const HEADROOM: [f64; 4] = [0.4, 0.55, 0.55, 0.55];
/// Where a body sits under the shoulder line before it has been seen, in shoulder widths; a lost landmark is stood in for by what was learned, so the box doesn't jump.
const EYES_ABOVE: f64 = 0.6;
const NOSE_UNDER_EYES: f64 = 0.0;
const HIPS_BELOW: f64 = 1.4;
const ANKLES_BELOW: f64 = 3.1;
/// How much of a new measurement is taken into what was learned, per picture of the people (about a second to settle).
const LEARN: f64 = 0.1;
/// Nobody clear enough to frame for this long: the framing holds where it is before it widens, since a missed detection is not someone leaving.
const HOLD_S: f64 = 1.5;
/// A person the model hasn't reported for this long has left the group: the model reports only some of the people each time, and framing each report made it jump.
const GROUP_S: f64 = 3.0;
/// A report is the same person as one already followed when it is within this many shoulder widths of them.
const SAME_PERSON: f64 = 2.5;
/// How far under the eyes the chin is, in shoulder widths: Head ends just below it.
const CHIN: f64 = 0.43;
/// Chest ends this far under the shoulder line (about the armpits), in shoulder widths.
const CHEST: f64 = 0.4;
/// Waist ends this far above the hip points (they sit a little under the belt), in shoulder widths.
const BELT: f64 = 0.15;
// Translators: Speed 1 to 10, slowest first, as a playful word for each step; keep them short
pub const SPEEDS: [&str; 10] = [n_("Sloth"), n_("Lazy"), n_("Mellow"), n_("Calm"), n_("Easy"), n_("Steady"), n_("Brisk"), n_("Quick"), n_("Snappy"), n_("Zippy")];
pub const SPEED_DEFAULT: i64 = 5;
/// How fast the framing catches up at either end (spring stiffness, 1/s).
const OMEGA_SLOW: f64 = 0.4;
const OMEGA_FAST: f64 = 9.0;
const TICK_MS: u64 = 33;
/// The Speed bubble's pictures are moved this often (60 a second, as the window draws), only while it is up.
const CHASE_MS: u64 = 16;
/// How close it may zoom until Settings says (its Max zoom, which it then follows: how soft is too soft is the person's call).
pub const MAX_ZOOM: f64 = 10.0;
/// Nobody seen for this long: back to the whole picture.
const LOST_S: f64 = 2.0;
/// The person picked to follow, not seen for this long, is let go and everyone is framed again.
const FOLLOW_GIVE_UP_S: f64 = 30.0;
/// How far (in shoulder widths) someone can have moved between two reports back to back, and each second more they were out of sight.
const REACH_BASE: f64 = 0.8;
const REACH_PER_S: f64 = 1.5;
/// How far, in shoulder widths, someone could have got however long they were out of sight: two people this close to the same spot can't be told apart.
const FAR_REACH: f64 = 6.0;
/// A match is only clear when no other is this many times as near.
const AMBIGUOUS: f64 = 4.0;
/// A match looks for someone where they would be by now at the speed they were going, if they were last seen no longer ago than this (seconds). After a longer gap the speed is dropped and the old spot is all there is: a guess that far ahead could put two people the wrong way round.
const PREDICT_S: f64 = 0.5;
/// Reports this long without being clear about who is who (two people passing each other) and the pick is let go.
const COAST_MAX_S: f64 = 2.0;
/// A speed is only worked out from two sightings at least this far apart (s): closer ones are mostly noise.
const MIN_SPEED_DT: f64 = 0.03;
/// A sighting this long after the last one counts half as much as the speed from before, and more the longer it is.
const SPEED_HALF_S: f64 = 0.2;
/// A lost person is taken back as the lone unaccounted-for person only within this long of being seen.
const RESUME_WITHIN_S: f64 = 5.0;
/// The framing lets the target's centre wander this far (of the window's size) at Speed 5...
const DEAD_CENTRE: f64 = 0.06;
/// ...and its zoom this much, and only follows what goes past that.
const DEAD_ZOOM: f64 = 1.12;
/// Inside the dead zone it drifts back to the target this slowly (at Speed 5).
const SETTLE_S: f64 = 2.0;
// Translators: Room to move, what you can do before the framing follows: off, fidget, shift in your seat, lean over, take a step, move around. Keep them short
pub const ROOMS: [&str; 6] = [n_("Off"), n_("Fidget"), n_("Shift"), n_("Lean"), n_("Step"), n_("Roam")];
pub const ROOM_DEFAULT: i64 = 2;
/// Each step lets you move this much further (of the window's size) before the framing follows.
const ROOM_STEP: f64 = 0.06;
/// Outside the room for this long before it does (a quick lean and back doesn't count).
const LEAVE_S: f64 = 0.5;
/// Following stops once the framing has been this close to you for this long.
const SETTLED_S: f64 = 0.6;
/// Widening for a hand is at least this quick, whatever Speed says.
const WIDEN_OMEGA: f64 = 3.5;
const HAND_ON_S: f64 = 0.3;
const HAND_OFF_S: f64 = 0.8;
const HANDS: [&str; 5] = ["elbow", "wrist", "index", "pinky", "thumb"];

const T_TICK: &str = "tick";
/// The Speed bubble's timer: its dots run only while it is up.
const T_CHASE: &str = "chase";
const SWITCH_TIP: &str = n_("Zoom and pan to follow you.");
const FRAMING_TIP: &str = n_("Set how much of you is in the picture, from only your head to all of you.");
const SPEED_TIP: &str = n_("How quickly the framing follows when you move.");
const ROOM_TIP: &str = n_("How much you can move before the framing follows you, from a fidget to walking around.");
const HAND_NAME: &str = n_("Show hands");
const HAND_DETAIL: &str = n_("Widen the view so your hands are in it too.");
const VIEW_TAKEN: &str = n_("Something else is moving the zoom and pan.");
const HOW_MUCH: &str = n_("Auto framing: how much of you");
const SPEED_LABEL: &str = n_("Auto framing speed");

/// (x0, y0, x1, y1) in the picture as shown.
pub type Box4 = (f64, f64, f64, f64);

/// Where the framing wants to be: the centre of the window in the picture as shown (0..1), and the zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

impl Target {
    pub const fn new(x: f64, y: f64, zoom: f64) -> Target {
        Target { x, y, zoom }
    }
}

/// How a person's body sits under their shoulder line, in shoulder widths: what stands in for a landmark the model has lost.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Proportions {
    pub eyes: f64,
    pub nose: f64,
    pub hips: f64,
    pub ankles: f64,
}

impl Default for Proportions {
    fn default() -> Self {
        Proportions { eyes: EYES_ABOVE, nose: NOSE_UNDER_EYES, hips: HIPS_BELOW, ankles: ANKLES_BELOW }
    }
}

fn lowest(person: &Person, names: [&str; 2]) -> Option<f64> {
    names.iter().filter_map(|n| person.point(n, SEEN)).map(|p| p.1).reduce(f64::max)
}

fn nudge(slot: &mut f64, value: f64, range: (f64, f64)) {
    *slot += (value.clamp(range.0, range.1) - *slot) * LEARN;
}

impl Proportions {
    /// Height of the eyes in an upright person: the eyes when seen, else the nose less its usual drop, else the usual height above the shoulders.
    fn eyes_at(&self, person: &Person, sy: f64, width: f64) -> f64 {
        let eyes = ["left_eye", "right_eye"].iter().filter_map(|n| person.point(n, SEEN)).map(|p| p.1).reduce(f64::min);
        eyes.or_else(|| person.point("nose", SEEN).map(|p| p.1 - self.nose * width)).unwrap_or(sy - self.eyes * width)
    }

    /// Takes in what one person shows (a crowd isn't told apart from picture to picture, so nothing is learned from it).
    pub fn learn(&mut self, people: &[Person], aspect: f64) {
        let [one] = people else { return };
        let (person, _) = upright(one, aspect);
        let Some(((_, sy), width)) = body(&person, aspect) else { return };
        let eyes = ["left_eye", "right_eye"].iter().filter_map(|n| person.point(n, SEEN)).map(|p| p.1).reduce(f64::min);
        if let Some(eyes) = eyes {
            nudge(&mut self.eyes, (sy - eyes) / width, (0.4, 1.4));
            if let Some(nose) = person.point("nose", SEEN) {
                nudge(&mut self.nose, (nose.1 - eyes) / width, (0.0, 0.3));
            }
        }
        if let Some(hips) = lowest(&person, ["left_hip", "right_hip"]) {
            nudge(&mut self.hips, (hips - sy) / width, (0.8, 2.5));
        }
        if let Some(ankles) = lowest(&person, ["left_ankle", "right_ankle"]) {
            nudge(&mut self.ankles, (ankles - sy) / width, (2.5, 6.0));
        }
    }
}

/// Where the box ends at each of FRAMINGS, in picture heights: under the eyes (`eyes`) for Head, then down the body.
fn bottoms(person: &Person, eyes: f64, sy: f64, width: f64, props: &Proportions) -> [f64; 4] {
    let hips = lowest(person, ["left_hip", "right_hip"]).unwrap_or(sy + props.hips * width);
    let feet = lowest(person, ["left_ankle", "right_ankle"]).unwrap_or(sy + props.ankles * width) + 0.25 * width;
    let belt = (hips - BELT * width).max(sy + (CHEST + 0.25) * width);
    [eyes + CHIN * width, sy + CHEST * width, belt, feet.max(hips + width)]
}

/// (x0, y0, x1, y1) in the picture as shown around everyone, or None when nobody's clear enough to frame.
pub fn target_box(people: &[Person], aspect: f64, framing: f64, hands: &[&str]) -> Option<Box4> {
    target_box_with(people, aspect, framing, hands, &Proportions::default())
}

/// target_box with what a person's body is known to look like standing in for what the model has lost.
pub fn target_box_with<'a>(people: impl IntoIterator<Item = &'a Person>, aspect: f64, framing: f64, hands: &[&str], props: &Proportions) -> Option<Box4> {
    let mut boxes: Vec<Box4> = Vec::new();
    for person in people {
        let (person, to_shown) = upright(person, aspect);
        let Some(((cx, sy), width)) = body(&person, aspect) else { continue };
        let eyes = props.eyes_at(&person, sy, width);
        let at = framing.max(0.0).min(FRAMINGS.len() as f64 - 1.0);
        let i = (at as usize).min(FRAMINGS.len() - 2);
        let (f, bottoms) = (at - i as f64, bottoms(&person, eyes, sy, width, props));
        let bottom = bottoms[i] + (bottoms[i + 1] - bottoms[i]) * f;
        let top = eyes - (HEADROOM[i] + (HEADROOM[i + 1] - HEADROOM[i]) * f) * width;
        let half = (HALF_WIDTH[i] + (HALF_WIDTH[i + 1] - HALF_WIDTH[i]) * f) * width / aspect;
        let (mut x0, mut y0, mut x1, mut y1) = (cx - half, top, cx + half, bottom);
        // Room for what's in the hand
        let margin = 0.6 * width;
        for side in hands {
            let Some(found) = arm(&person, side, aspect) else { continue };
            for n in HANDS {
                let Some(p) = person.point(&format!("{side}_{n}"), ARM_SEEN) else { continue };
                // What's out to the side, not a hand hanging down
                if p.1 < found.below && found.out(p) > 0.25 * found.width {
                    (x0, y0) = (x0.min(p.0 - margin / aspect), y0.min(p.1 - margin));
                    (x1, y1) = (x1.max(p.0 + margin / aspect), y1.max(p.1 + margin));
                }
            }
        }
        let corners = [to_shown(x0, y0), to_shown(x0, y1), to_shown(x1, y0), to_shown(x1, y1)];
        let fold = |f: fn(f64, f64) -> f64, pick: fn(&(f64, f64)) -> f64| corners.iter().map(pick).reduce(f).unwrap_or(0.0);
        boxes.push((fold(f64::min, |p| p.0), fold(f64::min, |p| p.1), fold(f64::max, |p| p.0), fold(f64::max, |p| p.1)));
    }
    boxes.into_iter().reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
}

/// Shoulder width against torso length, None without both hips.
fn build(person: &Person, aspect: f64) -> Option<f64> {
    let (_, width) = body(person, aspect)?;
    let (l, r) = (person.point("left_hip", SEEN)?, person.point("right_hip", SEEN)?);
    let (ls, rs) = (person.point("left_shoulder", SEEN)?, person.point("right_shoulder", SEEN)?);
    let (hip, shoulder) = (((l.0 + r.0) / 2.0, (l.1 + r.1) / 2.0), ((ls.0 + rs.0) / 2.0, (ls.1 + rs.1) / 2.0));
    let torso = ((hip.0 - shoulder.0) * aspect).hypot(hip.1 - shoulder.1);
    (torso > 1e-3).then_some(width / torso)
}

/// Two poses could be one person: builds not more than a third apart, or one not seen well enough to say.
fn same_build(a: &Person, b: &Person, aspect: f64) -> bool {
    match (build(a, aspect), build(b, aspect)) {
        (Some(x), Some(y)) => (x - y).abs() / x.max(y) <= 0.35,
        _ => true,
    }
}

/// Index of the person nearest a spot of the picture as shown at zoom 1: someone the spot is on counts as distance 0, ties go to the nearest body centre.
pub fn nearest_person(people: &[Person], at: (f64, f64), aspect: f64) -> Option<usize> {
    let reach = |person: &Person| -> Option<(f64, f64)> {
        let seen: Vec<(f64, f64)> = person.points.keys().filter_map(|n| person.point(n, SEEN)).collect();
        if seen.is_empty() || body(person, aspect).is_none() {
            return None;
        }
        let (x0, x1) = seen.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
        let (y0, y1) = seen.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
        let outside = (((x0 - at.0).max(0.0).max(at.0 - x1)) * aspect).hypot((y0 - at.1).max(0.0).max(at.1 - y1));
        let centre = (((x0 + x1) / 2.0 - at.0) * aspect).hypot((y0 + y1) / 2.0 - at.1);
        Some((outside, centre))
    };
    people.iter().enumerate().filter_map(|(i, p)| reach(p).map(|r| (r, i))).min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)).map(|(_, i)| i)
}

/// `tracked` (each person with when they were last reported) after a report: each person goes to the nearest one within SAME_PERSON shoulder widths, taking their place, or is added.
pub fn track(mut tracked: Vec<(Person, f64)>, people: &[Person], now: f64, aspect: f64) -> Vec<(Person, f64)> {
    let place = |person: &Person| body(person, aspect);
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (i, person) in people.iter().enumerate() {
        let Some(((x, y), width)) = place(person) else { continue };
        for (j, (old, _)) in tracked.iter().enumerate() {
            let Some(((ox, oy), owidth)) = place(old) else { continue };
            let apart = ((x - ox) * aspect).hypot(y - oy);
            if apart <= SAME_PERSON * width.max(owidth) {
                pairs.push((apart, i, j));
            }
        }
    }
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut taken, mut used) = (vec![false; people.len()], vec![false; tracked.len()]);
    for (_, i, j) in pairs {
        if !taken[i] && !used[j] {
            (taken[i], used[j]) = (true, true);
            tracked[j] = (people[i].clone(), now);
        }
    }
    for (i, person) in people.iter().enumerate() {
        if !taken[i] && place(person).is_some() {
            tracked.push((person.clone(), now));
        }
    }
    tracked
}

/// The window (same shape as the picture) that holds box, as close as max_zoom allows and inside the picture.
pub fn fit(b: Box4, max_zoom: f64) -> Target {
    let (x0, y0, x1, y1) = b;
    let zoom = (1.0 / (x1 - x0).max(y1 - y0).max(1e-6)).max(1.0).min(max_zoom);
    let half = 0.5 / zoom;
    Target::new(((x0 + x1) / 2.0).max(half).min(1.0 - half), ((y0 + y1) / 2.0).max(half).min(1.0 - half), zoom)
}

/// (spring stiffness, target smoothing in s, dead zone scale) for a Speed of 1 to 10.
pub fn tuning(speed: i64) -> (f64, f64, f64) {
    let t = (speed.clamp(1, 10) - 1) as f64 / 9.0;
    let omega = OMEGA_SLOW * (OMEGA_FAST / OMEGA_SLOW).powf(t);
    (omega, (0.5 / omega).min(0.6), (1.6 / omega).sqrt())
}

/// goal moved only as far as it takes to keep target inside the dead zone around it, so small wobbles stay put and a steady move has no hops.
pub fn follow(goal: Target, target: Target, scale: f64) -> Target {
    let band = DEAD_CENTRE * scale / goal.zoom;
    let room = DEAD_ZOOM.ln() * scale;
    let zoom = goal.zoom.ln().max(target.zoom.ln() - room).min(target.zoom.ln() + room).exp();
    Target::new(goal.x.max(target.x - band).min(target.x + band), goal.y.max(target.y - band).min(target.y + band), zoom)
}

/// Where the goal is after another `dt`, with nothing holding it: it follows `smooth` as far as the dead zone asks and centres up once you stop.
pub fn settle(goal: Option<Target>, smooth: Target, dt: f64, scale: f64) -> Target {
    smoothed(Some(follow(goal.unwrap_or(smooth), smooth, scale)), smooth, dt, SETTLE_S * scale)
}

/// What the Speed bubble draws: a stand-in for you that hops about the box by itself, and the framing following it with the smoothing and
/// spring that `tick` uses at this Speed (the dead zone is left out, so it is only the pace you see), with two copies of where the framing
/// was a moment ago, so a quick one is drawn with gaps between them and a slow one huddled.
#[derive(Clone, Debug)]
pub struct Chase {
    at: f64,
    smooth: Option<Target>,
    spring: Spring,
    /// Where the framing was, and when: just enough to draw the copies
    history: VecDeque<(f64, [f64; 2])>,
}

impl Default for Chase {
    fn default() -> Self {
        Chase::new()
    }
}

impl Chase {
    /// How long it holds still at each spot, and how long a hop takes: a person stepping, so the bubble shows the follow within a second.
    const HOLD_S: f64 = 0.7;
    const MOVE_S: f64 = 0.12;
    /// Where it stands, as fractions of the room the box has (the frame's own size already kept out): left and high, right and low, across to
    /// the middle and low, then right and high, so every hop changes both ways.
    const SPOTS: [(f64, f64); 4] = [(0.1, 0.2), (0.9, 0.8), (0.5, 0.85), (0.9, 0.15)];
    /// How high a hop bows (of the room): you step round, not straight across.
    const BOW: f64 = 0.12;
    /// The demo follows this many times quicker than the real thing, so Speed 1 still gets a good way in a hop and Speed 10 lands in about a tenth of a second.
    const RATE: f64 = 5.0;
    /// How long ago the two copies were where the framing was.
    const TRAIL_S: [f64; 2] = [0.05, 0.1];

    /// Starts at the first hop, so the first move comes the moment the bubble is up.
    pub fn new() -> Chase {
        let start = Self::you(Self::HOLD_S);
        Chase { at: Self::HOLD_S, smooth: None, spring: Spring::new(start), history: VecDeque::new() }
    }

    /// Where you are `t` seconds in: each spot held for a while, then a hop to the next with a bow in it.
    pub fn you(t: f64) -> Target {
        let leg = Self::HOLD_S + Self::MOVE_S;
        let (lap, into) = ((t / leg).floor(), t.rem_euclid(leg));
        let from = lap.rem_euclid(Self::SPOTS.len() as f64) as usize;
        let (a, b) = (Self::SPOTS[from], Self::SPOTS[(from + 1) % Self::SPOTS.len()]);
        let m = ((into - Self::HOLD_S) / Self::MOVE_S).clamp(0.0, 1.0);
        let eased = m * m * (3.0 - 2.0 * m);
        let bow = Self::BOW * (std::f64::consts::PI * m).sin();
        Target::new(a.0 + (b.0 - a.0) * eased, (a.1 + (b.1 - a.1) * eased - bow).clamp(0.0, 1.0), 1.0)
    }

    /// Runs `dt` seconds on at this Speed: you (x, y), the framing, and the two copies of it, each from 0 to 1 across the room.
    pub fn step(&mut self, speed: i64, dt: f64) -> [f64; 8] {
        self.at += dt;
        let (omega, tau, _) = tuning(speed);
        let target = Self::you(self.at);
        let quick = dt * Self::RATE;
        let smooth = smoothed(self.smooth, target, quick, tau);
        self.smooth = Some(smooth);
        let framing = self.spring.step(smooth, quick, omega);
        self.history.push_back((self.at, [framing.x, framing.y]));
        let oldest = Self::TRAIL_S[1];
        while self.history.len() > 2 && self.at - self.history[1].0 >= oldest {
            self.history.pop_front();
        }
        let back = |lag: f64| self.history.iter().rev().find(|(t, _)| self.at - t >= lag).map_or([framing.x, framing.y], |(_, p)| *p);
        let (one, two) = (back(Self::TRAIL_S[0]), back(Self::TRAIL_S[1]));
        [target.x, target.y, framing.x, framing.y, one[0], one[1], two[0], two[1]]
    }
}

/// target is within room (of the window's size, and as a zoom ratio in log space) of anchor.
pub fn inside(anchor: Target, target: Target, room: f64) -> bool {
    let size = 1.0 / anchor.zoom;
    (target.x - anchor.x).abs() <= room * size && (target.y - anchor.y).abs() <= room * size && (target.zoom / anchor.zoom).ln().abs() <= room * 1.2
}

/// Room to move: the framing holds still near where it last settled, follows once you've been out of that for LEAVE_S, and holds where it settles.
#[derive(Clone, Debug, Default)]
pub struct Room {
    /// Where it holds; None while following.
    pub anchor: Option<Target>,
    out_since: Option<f64>,
    close_since: Option<f64>,
}

impl Room {
    /// Where to hold, or None to follow target.
    pub fn goal(&mut self, target: Target, at: Target, room: f64, now: f64) -> Option<Target> {
        if let Some(anchor) = self.anchor {
            if inside(anchor, target, room) {
                self.out_since = None;
                return Some(anchor);
            }
            let since = *self.out_since.get_or_insert(now);
            if now - since < LEAVE_S {
                return Some(anchor);
            }
            self.reset();
            return None;
        }
        // Caught up with you: hold again once it stays that way
        if inside(at, target, room * 0.25) {
            let since = *self.close_since.get_or_insert(now);
            if now - since >= SETTLED_S {
                // Where you are: the spring finishes getting there
                self.anchor = Some(target);
                self.close_since = None;
                return self.anchor;
            }
        } else {
            self.close_since = None;
        }
        None
    }

    pub fn reset(&mut self) {
        *self = Room::default();
    }
}

/// target followed with a time constant of tau, zoom in log space; landmarks wobble a little every frame.
pub fn smoothed(last: Option<Target>, target: Target, dt: f64, tau: f64) -> Target {
    let Some(last) = last else { return target };
    let a = 1.0 - (-dt / tau.max(1e-3)).exp();
    Target::new(last.x + (target.x - last.x) * a, last.y + (target.y - last.y) * a, (last.zoom.ln() + (target.zoom.ln() - last.zoom.ln()) * a).exp())
}

/// Critically damped: heads for its target without overshooting; zoom moves in log space so it feels even.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub at: Target,
    v: [f64; 3],
}

impl Spring {
    pub fn new(at: Target) -> Spring {
        Spring { at, v: [0.0; 3] }
    }

    pub fn step(&mut self, goal: Target, dt: f64, omega: f64) -> Target {
        let now = [self.at.x, self.at.y, self.at.zoom.ln()];
        let want = [goal.x, goal.y, goal.zoom.ln()];
        let (f, k) = (1.0 + 2.0 * dt * omega, dt * omega * omega);
        let det = 1.0 / (f + dt * k);
        let moved = [0, 1, 2].map(|i| ((f * now[i] + dt * self.v[i] + dt * k * want[i]) * det, (self.v[i] + k * (want[i] - now[i])) * det));
        self.v = moved.map(|m| m.1);
        self.at = Target::new(moved[0].0, moved[1].0, moved[2].0.exp());
        self.at
    }
}

/// On after its input has been on for on_s, off after it's been off for off_s.
#[derive(Clone, Copy, Debug)]
pub struct Latch {
    on_s: f64,
    off_s: f64,
    pub on: bool,
    since: Option<f64>,
}

impl Latch {
    pub const fn new(on_s: f64, off_s: f64) -> Latch {
        Latch { on_s, off_s, on: false, since: None }
    }

    pub fn update(&mut self, value: bool, now: f64) -> bool {
        if value == self.on {
            self.since = None;
        } else if let Some(since) = self.since {
            if now - since >= if value { self.on_s } else { self.off_s } {
                self.on = value;
                self.since = None;
            }
        } else {
            self.since = Some(now);
        }
        self.on
    }
}

/// A view's flips and turn with nothing else: what's shown at zoom 1.
fn flat(view: &View) -> Geometry {
    Geometry { flip_h: view.flip_h, flip_v: view.flip_v, rotation: view.rotation, ..Geometry::default() }
}

fn turn_of(view: &View) -> Turn {
    Turn { flip_h: view.flip_h, flip_v: view.flip_v, rotation: view.rotation }
}

pub struct FramingPlugin {
    // The card's controls
    switch: bool,
    framing: i64,
    speed: i64,
    room: i64,
    hand: bool,
    held: Option<bool>,
    paired: Paired,
    // The status row's state between its refreshes
    status_active: bool,
    status_problem: String,
    // Following
    people: Vec<Person>,
    /// Each person followed with where they were last reported and when, dropped after GROUP_S unseen; only the current report's people unless two or more were seen together lately
    tracked: Vec<(Person, f64)>,
    /// When two or more people were last in one report
    together_at: Option<f64>,
    seen_at: Option<f64>,
    spring: Option<Spring>,
    goal: Option<Target>,
    smooth: Option<Target>,
    widened: Vec<&'static str>,
    room_state: Room,
    hands: [(&'static str, Latch); 2],
    ticked_at: Option<f64>,
    orientation: Turn,
    max_zoom: f64,
    steering: bool,
    /// What the person's body has been seen to look like, for the landmarks the model loses
    proportions: Proportions,
    /// The last box someone was framed by, and when: held for a moment when nobody is found
    last_box: Option<(Box4, f64)>,
    /// How long that is (HOLD_S)
    hold_s: f64,
    /// The one person picked on the preview to follow, as last matched in a report and when; None frames everyone
    followed: Option<(Person, f64)>,
    /// Everyone else in the last report while someone is followed, so a bystander is never mistaken for them
    others: Vec<(Person, f64)>,
    /// How fast the followed person and each of the others were moving across the picture (x, y per second) when last matched, so a match looks for them where they are going
    followed_vel: (f64, f64),
    others_vel: Vec<(f64, f64)>,
    /// Since when the reports have been too unclear to say who is who, and since when the followed person was last matched clearly: the tracks go on along their speeds meanwhile
    coasting_since: Option<(f64, f64)>,
    /// A pick on the preview is under way
    picking: bool,
    /// The Framing bubble is up: its crop follows the slider.
    peeking: bool,
    /// The Speed bubble is up: its dots run.
    chasing: Option<Chase>,
    /// When the dots last moved, for the time they move by
    chased_at: Option<f64>,
    /// What the Framing bubble last showed: the part of the stand-in scene, so it is only redrawn when it moves
    peek_shown: Option<[f64; 4]>,
    /// What the vision service was last told about whether faces are enough (None: nothing since people were last asked for).
    told_heads: Option<bool>,
    /// set_config has no host: taking the view and the card's update wait for the next hook that has one.
    pending: bool,
    /// Settings that switched it off came in before that hook, as a device change's defaults do: it goes through off first.
    cycle: bool,
    clock: Box<dyn Fn() -> f64 + Send>,
}

impl Default for FramingPlugin {
    fn default() -> Self {
        FramingPlugin::new()
    }
}

impl FramingPlugin {
    pub fn new() -> FramingPlugin {
        FramingPlugin::with_clock(|| monotonic_ns() as f64 / 1e9)
    }

    /// With its own clock in seconds, for a test to run time by hand.
    pub fn with_clock(clock: impl Fn() -> f64 + Send + 'static) -> FramingPlugin {
        FramingPlugin {
            switch: false,
            framing: Self::framing_steps(FRAMING_DEFAULT),
            speed: SPEED_DEFAULT,
            room: ROOM_DEFAULT,
            hand: false,
            held: None,
            paired: Paired::default(),
            status_active: false,
            status_problem: String::new(),
            people: Vec::new(),
            tracked: Vec::new(),
            followed: None,
            others: Vec::new(),
            followed_vel: (0.0, 0.0),
            others_vel: Vec::new(),
            coasting_since: None,
            picking: false,
            together_at: None,
            seen_at: None,
            spring: None,
            goal: None,
            smooth: None,
            widened: Vec::new(),
            room_state: Room::default(),
            hands: SIDES.map(|side| (side, Latch::new(HAND_ON_S, HAND_OFF_S))),
            ticked_at: None,
            orientation: Turn::default(),
            max_zoom: MAX_ZOOM,
            steering: false,
            proportions: Proportions::default(),
            last_box: None,
            hold_s: HOLD_S,
            peeking: false,
            chasing: None,
            chased_at: None,
            peek_shown: None,
            told_heads: None,
            pending: false,
            cycle: false,
            clock: Box::new(clock),
        }
    }

    fn framing_steps(framing: f64) -> i64 {
        round_half_even(framing * FRAMING_STEPS as f64) as i64
    }

    fn framing_value(&self) -> f64 {
        self.framing as f64 / FRAMING_STEPS as f64
    }

    fn framing_text(&self) -> String {
        tr(FRAMINGS[round_half_even(self.framing_value()) as usize])
    }

    fn speed_text(&self) -> String {
        tr(SPEEDS[(self.speed - 1) as usize])
    }

    fn room_text(&self) -> String {
        tr(ROOMS[self.room as usize])
    }

    fn fixed_props() -> Value {
        let fits: Vec<String> = FRAMINGS.iter().chain(&SPEEDS).chain(&ROOMS).map(|t| tr(t)).collect();
        json!({"switch-name": tr(TITLE), "switch-help": tr(SWITCH_TIP), "framing-tip": tr(FRAMING_TIP), "speed-tip": tr(SPEED_TIP),
               "room-tip": tr(ROOM_TIP), "hand-name": tr(HAND_NAME), "hand-detail": tr(HAND_DETAIL), "pick-name": tr("Follow one person"), "pick-text": tr("Pick someone"), "pick-again-text": tr("Pick someone else"),
               "pick-tip": tr("Click one person on the preview and Auto framing follows only them, not whoever else is in view."), "all-text": tr("Follow everyone"),
               "pick-blocked-tip": tr("People can't be picked while the preview shows a combined picture or another camera. Switch 3D or One picture off, or show a single camera."),
               "all-tip": tr("Frame everyone in view again."), "fits": fits.join("\n")})
    }

    fn state_props(&self) -> Value {
        json!({"framing-peek": "picture", "on": self.switch, "hand": self.hand, "following": self.followed.is_some(), "picking": self.picking, "active": self.paired.on(), "framing": self.framing, "speed": self.speed, "room": self.room,
               "framing-text": self.framing_text(), "speed-text": self.speed_text(), "room-text": self.room_text(), "reset-shown": self.reset_shown()})
    }

    /// The card's own switch is not part of its Reset.
    const RESET_KEPT: &'static [&'static str] = &["on"];

    fn reset_shown(&self) -> bool {
        off_default(&self.get_config(), &FramingPlugin::new().get_config(), Self::RESET_KEPT)
    }

    fn show_reset(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &json!({"reset-shown": self.reset_shown()}));
    }

    fn reset(&mut self, host: &mut dyn Host) {
        let cfg = reset_config(&self.get_config(), &FramingPlugin::new().get_config(), Self::RESET_KEPT);
        if self.apply_preset(host, &cfg).is_ok() {
            host.schedule_save();
        }
    }

    fn all_props(&self) -> Value {
        let mut all = Self::fixed_props();
        if let (Value::Object(a), Value::Object(s)) = (&mut all, self.state_props()) {
            a.extend(s);
        }
        all
    }

    fn catch_up(&mut self, host: &mut dyn Host) {
        if self.pending {
            self.pending = false;
            host.set_ui_props("card", &self.state_props());
            self.show_following(host);
            if std::mem::take(&mut self.cycle) {
                self.apply(host, false);
            }
            self.apply(host, self.switch);
        }
    }

    /// VisionStatus.refresh
    fn refresh_status(&mut self, host: &mut dyn Host, active: bool, problem: String) {
        self.status_active = active;
        self.status_problem = problem;
        self.show_status(host);
    }

    fn show_status(&self, host: &mut dyn Host) {
        let row = vision_row::props(&*host, self.status_active, &self.status_problem);
        host.set_ui_props("card", &row);
    }

    fn apply(&mut self, host: &mut dyn Host, on: bool) {
        if on && !self.steering {
            if !host.take_view(TITLE) {
                self.refresh_status(host, true, tr(VIEW_TAKEN));
                return;
            }
            self.steering = true;
            host.want_people(TITLE, true);
            self.told_heads = None;
            (self.spring, self.goal, self.smooth) = (None, None, None);
            self.room_state.reset();
            self.ticked_at = None;
            host.schedule(TICK_MS, T_TICK);
        } else if !on && self.steering {
            self.steering = false;
            self.last_box = None;
            host.cancel_schedule(T_TICK);
            host.want_people(TITLE, false);
            self.told_heads = None;
            host.release_view(TITLE);
            self.people.clear();
            self.forget_group();
            if self.picking {
                host.cancel_pick(NAME);
            }
            self.followed = None;
            self.others.clear();
            self.seen_at = None;
            self.show_following(host);
        }
        self.refresh_status(host, self.switch, String::new());
    }

    /// Faces with shoulders are enough at Head and Chest without Show hands or one person picked (no hips or hands in a face, and bodies tell people apart better).
    fn heads_enough(&self) -> bool {
        self.steering && self.framing <= FRAMING_STEPS && !self.hand && self.followed.is_none()
    }

    /// Tells the vision service when heads_enough changed.
    fn sync_heads(&mut self, host: &mut dyn Host) {
        let enough = self.heads_enough();
        if self.steering && self.told_heads != Some(enough) {
            self.told_heads = Some(enough);
            host.want_heads(TITLE, enough);
        }
    }

    /// The Framing bubble came up or went down: while it is up its crop follows the slider, and nothing runs while it is down.
    fn peek_open(&mut self, host: &mut dyn Host, open: bool) {
        if open == self.peeking {
            return;
        }
        self.peeking = open;
        self.peek_shown = None;
        if open {
            self.draw_peek(host);
        }
    }

    /// The stand-in scene cropped around the sample person at this Framing, never the live picture (too small to read in a bubble); the photo goes over once, then only the crop moves.
    fn draw_peek(&mut self, host: &mut dyn Host) {
        let model = target_box(&[peek::sample_person()], peek::SAMPLE_ASPECT, self.framing_value(), &[]).map_or(Target::new(0.5, 0.5, 1.0), |b| fit(b, self.max_zoom));
        let clip = peek::clip((model.x, model.y, model.zoom));
        if self.peek_shown.is_none() {
            host.set_ui_image("card", "framing-picture", peek::scene_picture());
        }
        if self.peek_shown != Some(clip) {
            host.set_ui_props("card", &json!({"framing-clip": clip}));
        }
        self.peek_shown = Some(clip);
    }

    /// The Speed bubble came up or went down: its dots start hopping at once and run only while it is up.
    fn chase_open(&mut self, host: &mut dyn Host, open: bool) {
        if open == self.chasing.is_some() {
            return;
        }
        if open {
            self.chasing = Some(Chase::new());
            self.chased_at = Some((self.clock)());
            // The first hop is under way before the first picture
            self.move_dots(host, 0.0);
            host.schedule(CHASE_MS, T_CHASE);
        } else {
            self.chasing = None;
            self.chased_at = None;
            host.cancel_schedule(T_CHASE);
        }
    }

    /// Moves the dots on by `dt` seconds at the Speed the slider has now, even while it is being dragged.
    fn move_dots(&mut self, host: &mut dyn Host, dt: f64) {
        let speed = self.speed;
        if let Some(chase) = self.chasing.as_mut() {
            host.set_ui_props("card", &json!({"speed-dots": chase.step(speed, dt)}));
        }
    }

    /// A tick of the Speed bubble: the time since the last one, not a fixed step, so a late timer moves the dots further instead of slowing them.
    fn chase_tick(&mut self, host: &mut dyn Host) {
        let now = (self.clock)();
        let dt = self.chased_at.map_or(CHASE_MS as f64 / 1000.0, |at| (now - at).clamp(0.0, 0.1));
        self.chased_at = Some(now);
        self.move_dots(host, dt);
    }

    fn switched_by_hand(&mut self, host: &mut dyn Host, on: bool) {
        self.switch = on;
        host.set_ui_props("card", &json!({"on": on}));
        host.schedule_save();
        self.apply(host, on);
    }

    fn set_framing(&mut self, host: &mut dyn Host, value: i64) {
        if value != self.framing {
            self.framing = value;
            host.set_ui_props("card", &json!({"framing": value, "framing-text": self.framing_text()}));
            self.show_reset(host);
            host.schedule_save();
            // The bubble crops what it has, so a drag shows its crop at once and doesn't wait for the next picture
            if self.peeking {
                self.draw_peek(host);
            }
        }
    }

    fn set_speed(&mut self, host: &mut dyn Host, value: i64) {
        if value != self.speed {
            self.speed = value;
            // The dots' hops keep their own clock whatever the slider does: a new Speed is seen in the next hop, never by skipping the pause
            host.set_ui_props("card", &json!({"speed": value, "speed-text": self.speed_text()}));
            self.show_reset(host);
            host.schedule_save();
        }
    }

    fn set_room(&mut self, host: &mut dyn Host, value: i64) {
        if value != self.room {
            self.room = value;
            host.set_ui_props("card", &json!({"room": value, "room-text": self.room_text()}));
            self.show_reset(host);
            host.schedule_save();
        }
    }

    fn on_people(&mut self, host: &mut dyn Host, source_id: &str, people: &[Value]) {
        if self.steering && source_id == host.focused_source_id().unwrap_or_default() {
            self.people = people.iter().filter_map(Person::from_value).collect();
            let aspect = host.view_aspect() as f64;
            let aspect = if aspect != 0.0 { aspect } else { 16.0 / 9.0 };
            self.proportions.learn(&self.people, aspect);
            let now = (self.clock)();
            self.seen_at = Some(now);
            self.follow_report(now, aspect);
            self.remember(now, aspect);
        }
    }

    /// Takes this report into the people followed. Once two or more have been seen together each person goes to the nearest one already followed, or is new; before that, and a while after, only the people in this report count, so one person who jumps is still one person.
    fn remember(&mut self, now: f64, aspect: f64) {
        let group = self.together_at.is_some_and(|at| now - at < GROUP_S);
        if self.people.iter().filter(|p| body(p, aspect).is_some()).count() >= 2 {
            self.together_at = Some(now);
        }
        if group {
            self.tracked.retain(|(_, at)| now - at < GROUP_S);
            self.tracked = track(std::mem::take(&mut self.tracked), &self.people, now, aspect);
            // Reports that alternate between them are still the group being there
            if self.tracked.iter().filter(|(_, at)| now - at < HOLD_S).count() >= 2 {
                self.together_at = Some(now);
            }
        } else {
            self.tracked = track(Vec::new(), &self.people, now, aspect);
        }
    }

    /// Starts a pick on the preview: whoever is clicked is the one Auto framing follows.
    fn pick(&mut self, host: &mut dyn Host) {
        // a preview that draws another stream's output (a 3D pair's combined picture) isn't the picture the people were found in
        if !self.steering || !host.preview_shows_own_picture() {
            return;
        }
        host.pick_on_preview(NAME, PickKind::Point, None, &tr("Click the person to follow on the preview. Esc cancels."));
    }

    fn picked(&mut self, host: &mut dyn Host, pick: &telescope_core::host::Pick) {
        // Where the click is in the whole picture, before the view's zoom and pan, then turned the way the view turns it, as the people are
        if !self.steering || !host.preview_shows_own_picture() {
            return;
        }
        // a click on the bars beside the picture isn't on anyone
        let Some(at) = pick.full.first().filter(|p| (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1])) else { return };
        let (x, y) = turned(Turn::default(), turn_of(&host.view()))(at[0] as f64, at[1] as f64);
        let aspect = host.view_aspect() as f64;
        let aspect = if aspect != 0.0 { aspect } else { 16.0 / 9.0 };
        if let Some(i) = nearest_person(&self.people, (x, y), aspect) {
            self.followed = Some((self.people[i].clone(), (self.clock)()));
            self.followed_vel = (0.0, 0.0);
            self.others_vel.clear();
            // an unclear spell of the old pick doesn't count against the new one
            self.coasting_since = None;
            let now = (self.clock)();
            self.others = self.people.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, p)| (p.clone(), now)).collect();
            self.last_box = None;
            self.show_following(host);
        }
    }

    /// Takes a report into the one followed, and never guesses who is who. The followed person and everyone else known to be around
    /// each go to the nearest person in the report, one to one, within how far someone can have moved since they were last seen, and
    /// only if their build matches (shoulders against torso). The followed person's lock moves to a match only when it is clear:
    /// when a second person is nearly as near to them, or the match is nearly as near to someone else, the lock stays unresolved
    /// (nothing is refreshed, so Auto framing holds and then goes wide) until a report is clear again or the pick is made anew. A
    /// lost person comes back only as the one unaccounted-for person near where they were, soon enough, with a build that fits.
    /// Everyone met while they are missing is left out of the others, so a returning subject is never filed as a bystander, and the
    /// others are remembered for as long as the lock lasts. A report with nobody in it changes nothing.
    fn follow_report(&mut self, now: f64, aspect: f64) {
        let Some((old, at)) = self.followed.clone() else { return };
        let cands: Vec<usize> = (0..self.people.len()).filter(|&c| body(&self.people[c], aspect).is_some()).collect();
        if cands.is_empty() {
            return;
        }
        let before: Vec<(&Person, f64)> = std::iter::once((&old, at)).chain(self.others.iter().map(|(p, t)| (p, *t))).collect();
        // how long each has been out of reach of a clear match: while it's unclear who is who the clock for that keeps running, though everyone is carried along
        let lost_since: Vec<f64> = before.iter().map(|(_, seen)| self.coasting_since.map_or(*seen, |(_, lost)| lost.min(*seen))).collect();
        let vels: Vec<(f64, f64)> = std::iter::once(self.followed_vel).chain((0..self.others.len()).map(|i| self.others_vel.get(i).copied().unwrap_or((0.0, 0.0)))).collect();
        // (distance in shoulder widths, entity, candidate) for the pairs that could be the same person
        let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
        for (e, (who, seen)) in before.iter().enumerate() {
            let Some(((ox, oy), owidth)) = body(who, aspect) else { continue };
            // as far as they could have got, which is further than anyone is taken for them: whoever else could be them makes a match unclear
            let reach = (REACH_BASE + REACH_PER_S * (now - lost_since[e])).min(FAR_REACH);
            // where they should be by now, going the way they were going: two people crossing don't swap by passing each other's old spot
            let ahead = if now - seen <= PREDICT_S { now - seen } else { 0.0 };
            let (px, py) = (ox + vels[e].0 * ahead, oy + vels[e].1 * ahead);
            for &c in &cands {
                let Some(((x, y), width)) = body(&self.people[c], aspect) else { continue };
                let size = width.max(owidth);
                let apart = ((x - px) * aspect).hypot(y - py) / size;
                if apart <= reach && same_build(who, &self.people[c], aspect) {
                    pairs.push((apart, e, c));
                }
            }
        }
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut taken, mut used) = (vec![false; self.people.len()], vec![false; before.len()]);
        let mut matched: Vec<Option<(usize, f64)>> = vec![None; before.len()];
        for &(apart, e, c) in &pairs {
            // only someone within reach of where they were is taken for them
            if apart > (REACH_BASE + REACH_PER_S * (now - lost_since[e])).min(SAME_PERSON) {
                continue;
            }
            if !taken[c] && !used[e] {
                (taken[c], used[e]) = (true, true);
                matched[e] = Some((c, apart));
            }
        }
        let rival = |e: usize, c: usize, apart: f64, others_too: bool| {
            let close = |d: f64| d < AMBIGUOUS * apart.max(0.15);
            pairs.iter().any(|&(d, pe, pc)| (pe == e && pc != c && close(d)) || (others_too && pc == c && pe != e && close(d)))
        };
        let mut lock = matched[0].filter(|&(c, apart)| !rival(0, c, apart, true)).map(|(c, _)| c);
        if lock.is_none() && matched[0].is_none() && now - at <= RESUME_WITHIN_S {
            // not found: the one person nobody else accounts for, near where they were and built like them, is them
            let ((ox, oy), owidth) = body(&old, aspect).unwrap_or(((0.0, 0.0), 1.0));
            let near = |c: usize| {
                let Some(((x, y), width)) = body(&self.people[c], aspect) else { return false };
                ((x - ox) * aspect).hypot(y - oy) <= SAME_PERSON * width.max(owidth) && same_build(&old, &self.people[c], aspect)
            };
            let unaccounted: Vec<usize> = cands.iter().copied().filter(|&c| !taken[c]).collect();
            if let [only] = unaccounted[..] {
                // not if someone else known could be them as well
                if near(only) && !pairs.iter().any(|&(_, pe, pc)| pc == only && pe != 0) {
                    lock = Some(only);
                }
            }
        }
        if matched[0].is_some() && lock.is_none() {
            // unclear who is who, as when two people pass each other: nobody is matched, so the lock can't drift onto the wrong person, and everyone
            // goes on along their speed so that the reports after the overlap still find them. If it stays unclear that long the pick is let go.
            let (since, _) = *self.coasting_since.get_or_insert((now, at));
            if now - since > COAST_MAX_S {
                self.followed = None;
                self.others.clear();
                (self.followed_vel, self.others_vel, self.coasting_since) = ((0.0, 0.0), Vec::new(), None);
                return;
            }
            let advance = |person: &Person, seen: f64, v: (f64, f64)| {
                let dt = (now - seen).clamp(0.0, PREDICT_S);
                person.mapped(&|x, y| (x + v.0 * dt, y + v.1 * dt))
            };
            let moved: Vec<(Person, f64)> = self.others.iter().enumerate()
                .map(|(i, (p, t))| (advance(p, *t, self.others_vel.get(i).copied().unwrap_or((0.0, 0.0))), now)).collect();
            self.followed = Some((advance(&old, at, self.followed_vel), now));
            self.others = moved;
            return;
        }
        self.coasting_since = None;
        // bystanders as the report has them; anyone it didn't find stays as they were. While the lock is lost new faces aren't added: they might be the lost person
        let mut others: Vec<(Person, f64)> = Vec::new();
        let mut others_vel: Vec<(f64, f64)> = Vec::new();
        // the speed from where someone was last seen to where they are now, blended with the speed they had
        let speed = |e: usize, c: usize| -> (f64, f64) {
            let (who, seen) = before[e];
            let (Some(((ox, oy), _)), Some(((x, y), _))) = (body(who, aspect), body(&self.people[c], aspect)) else { return vels[e] };
            let dt = now - seen;
            if dt > PREDICT_S {
                // after a gap nobody knows how fast they went in between
                return (0.0, 0.0);
            }
            if dt < MIN_SPEED_DT {
                return vels[e];
            }
            // the newer the sighting the more it counts, so slow report rates (power saving) don't make the speed lag
            let w = 1.0 - 0.5f64.powf(dt / SPEED_HALF_S);
            (vels[e].0 + w * ((x - ox) / dt - vels[e].0), vels[e].1 + w * ((y - oy) / dt - vels[e].1))
        };
        for (e, (person, seen)) in self.others.iter().enumerate() {
            match matched[e + 1] {
                Some((c, _)) if Some(c) != lock => {
                    others.push((self.people[c].clone(), now));
                    others_vel.push(speed(e + 1, c));
                }
                _ => {
                    others.push((person.clone(), *seen));
                    others_vel.push(vels[e + 1]);
                }
            }
        }
        if lock.is_some() {
            for &c in &cands {
                if Some(c) != lock && !taken[c] {
                    others.push((self.people[c].clone(), now));
                    others_vel.push((0.0, 0.0));
                }
            }
        }
        if let Some(c) = lock {
            self.followed_vel = speed(0, c);
            self.followed = Some((self.people[c].clone(), now));
        }
        (self.others, self.others_vel) = (others, others_vel);
    }

    fn show_following(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &json!({"following": self.followed.is_some(), "picking": self.picking, "pick-blocked": !host.preview_shows_own_picture()}));
    }

    fn forget_group(&mut self) {
        self.tracked.clear();
        self.together_at = None;
    }

    fn on_view_changed(&mut self, view: &View) {
        let (old, new) = (self.orientation, turn_of(view));
        self.orientation = new;
        if old == new {
            return;
        }
        let remap = turned(old, new);
        self.people = self.people.iter().map(|person| person.mapped(&remap)).collect();
        if let Some((person, _)) = &mut self.followed {
            *person = person.mapped(&remap);
        }
        self.others = self.others.iter().map(|(person, at)| (person.mapped(&remap), *at)).collect();
        // the speeds were in the old turn of the picture
        (self.followed_vel, self.others_vel) = ((0.0, 0.0), Vec::new());
        // Who is who is found again from the next reports, in the new orientation
        self.forget_group();
        (self.spring, self.goal, self.smooth) = (None, None, None);
        self.last_box = None;
        self.room_state.reset();
        self.ticked_at = None;
    }

    fn start_from_view(view: &View) -> Spring {
        let (x, y) = (window_centre(view.zoom as f64, view.pan_x as f64), window_centre(view.zoom as f64, view.pan_y as f64));
        let (sx, sy) = view_to_shown(x, y, &flat(view));
        Spring::new(Target::new(sx, sy, (view.zoom as f64).max(1.0)))
    }

    fn target(&mut self, aspect: f64, now: f64) -> Target {
        let aspect = if aspect != 0.0 { aspect } else { 16.0 / 9.0 };
        let fresh = self.seen_at.is_some_and(|seen| now - seen < LOST_S);
        let solo: Vec<Person> = self.followed.iter().filter(|(_, at)| now - at < LOST_S).map(|(person, _)| person.clone()).collect();
        let people: &[Person] = match self.followed {
            Some(_) => &solo,
            None if fresh => &self.people,
            None => &[],
        };
        let out = if self.hand { hands_out(people, aspect) } else { Vec::new() };
        let mut widened = Vec::new();
        for (side, latch) in &mut self.hands {
            if latch.update(out.contains(side), now) {
                widened.push(*side);
            }
        }
        self.widened = widened;
        // With two or more followed, everyone followed counts, so a report that misses some of them doesn't move the framing; a report with nobody in it finds nobody
        let together = self.followed.is_none() && self.together_at.is_some_and(|at| now - at < GROUP_S);
        let group: Vec<&Person> = self.tracked.iter().filter(|(_, at)| now - at < GROUP_S).map(|(person, _)| person).collect();
        let reported = people.iter().any(|person| body(person, aspect).is_some());
        let found = if together && group.len() >= 2 && reported {
            target_box_with(group, aspect, self.framing_value(), &self.widened, &self.proportions)
        } else {
            target_box_with(people, aspect, self.framing_value(), &self.widened, &self.proportions)
        };
        // A person the model misses for a moment is not gone: keep framing where they were
        let held = self.last_box.filter(|(_, at)| now - at < self.hold_s).map(|(b, _)| b);
        if found.is_some() {
            self.last_box = found.map(|b| (b, now));
        } else if held.is_none() {
            self.last_box = None;
        }
        found.or(held).map_or(Target::new(0.5, 0.5, 1.0), |b| fit(b, self.max_zoom))
    }

    fn tick(&mut self, host: &mut dyn Host) {
        let now = (self.clock)();
        let dt = self.ticked_at.map_or(TICK_MS as f64 / 1000.0, |at| (now - at).min(0.2));
        self.ticked_at = Some(now);
        let mut spring = self.spring.unwrap_or_else(|| Self::start_from_view(&host.view()));
        let (mut omega, mut tau, mut scale) = tuning(self.speed);
        if self.followed.as_ref().is_some_and(|(_, at)| now - at > FOLLOW_GIVE_UP_S) {
            // gone for good as far as anyone can tell: back to everyone, and the card says so
            self.followed = None;
            self.others.clear();
            self.show_following(host);
        }
        let target = self.target(host.view_aspect() as f64, now);
        let widened = !self.widened.is_empty();
        // Someone's showing something: get it in the picture now, not at Speed's pace
        if widened {
            (omega, tau, scale) = (omega.max(WIDEN_OMEGA), tau.min(0.1), scale.min(0.5));
        }
        let smooth = smoothed(self.smooth, target, dt, tau);
        self.smooth = Some(smooth);
        let room = self.room as f64 * ROOM_STEP;
        let hold = if widened || room == 0.0 { None } else { self.room_state.goal(smooth, spring.at, room, now) };
        if widened || room == 0.0 {
            self.room_state.reset();
        }
        let goal = match hold {
            Some(hold) => hold,
            // And it centres up once you stop
            None => settle(self.goal, smooth, dt, scale),
        };
        self.goal = Some(goal);
        let before = spring.at;
        let at = spring.step(goal, dt, omega);
        self.spring = Some(spring);
        if (at.x - before.x).abs() + (at.y - before.y).abs() + (at.zoom - before.zoom).abs() > 1e-5 {
            self.steer(host, at);
        }
    }

    fn steer(&self, host: &mut dyn Host, at: Target) {
        let (x, y) = shown_to_view(at.x, at.y, &flat(&host.view()));
        host.steer_view(TITLE, at.zoom as f32, pan_for(x, at.zoom) as f32, pan_for(y, at.zoom) as f32);
    }
}

impl Plugin for FramingPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: TITLE.into(),
            about: n_("Zoom and pan to keep you in the picture as you move.").into(),
            when_off: n_("Without the Auto framing plugin there's no Auto framing card and the zoom only changes when you change it.").into(),
            category: "vision".into(),
            icon: "framing".into(),
            optional: true,
            needs_vision: true,
            // A preset can be a scene: framing on for talking, off with the zoom parked on something
            in_presets: true,
            // Framing the camera on you shouldn't zoom the desk camera out
            device_local: true,
            ..Default::default()
        }
    }

    fn setup(&mut self, host: &mut dyn Host) {
        self.orientation = turn_of(&host.view());
        host.set_ui_props("card", &self.all_props());
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.pending = false;
        self.peek_open(host, false);
        self.chase_open(host, false);
        self.apply(host, false);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        if let Some(on) = self.paired.hear(event) {
            host.set_ui_props("card", &json!({"active": on}));
        }
        match event {
            Event::PeopleSeen { source_id, people } => self.on_people(host, source_id, people),
            Event::PreviewSourceChanged { .. } => self.show_following(host),
            Event::PreviewPickChanged { owner } => {
                self.picking = owner == NAME;
                self.show_following(host);
            }
            Event::PreviewPicked { owner, pick: Some(pick) } if owner == NAME => self.picked(host, pick),
            Event::ViewChanged { view } => self.on_view_changed(view),
            Event::MaxZoomChanged { max_zoom } => {
                let max_zoom = (*max_zoom as f64).max(1.0);
                // Nothing stored may sit above a lowered ceiling (a held spot would stay, being near enough), so it lets go and the spring glides down
                if max_zoom < self.max_zoom {
                    self.room_state.reset();
                    let clamp = |t: Target| Target::new(t.x, t.y, t.zoom.min(max_zoom));
                    (self.smooth, self.goal) = (self.smooth.map(clamp), self.goal.map(clamp));
                }
                self.max_zoom = max_zoom;
            }
            e if vision_row::changes_it(e) => self.show_status(host),
            Event::TimerFired { owner, token } if owner == NAME && token == T_TICK && self.steering => {
                self.tick(host);
                host.schedule(TICK_MS, T_TICK);
            }
            Event::TimerFired { owner, token } if owner == NAME && token == T_CHASE && self.chasing.is_some() => {
                self.chase_tick(host);
                host.schedule(CHASE_MS, T_CHASE);
            }
            _ => {}
        }
        self.sync_heads(host);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        // a stream that starts again is a new session: nobody picked in the old one is followed in it
        if self.picking {
            host.cancel_pick(NAME);
        }
        self.followed = None;
        self.others.clear();
        self.people.clear();
        self.forget_group();
        // the next stream doesn't open on the last one's crop
        (self.seen_at, self.spring, self.goal, self.smooth) = (None, None, None, None);
        self.last_box = None;
        self.room_state.reset();
        self.ticked_at = None;
        if self.steering {
            // and the crop it left on the view is let go, so a quick restart doesn't open on it
            host.steer_view(TITLE, 1.0, 0.0, 0.0);
        }
        self.show_following(host);
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, _stream: &StreamInfo) {
        self.catch_up(host);
        self.people.clear();
        self.forget_group();
        self.followed = None;
        self.others.clear();
        self.show_following(host);
        (self.seen_at, self.spring, self.goal, self.smooth) = (None, None, None, None);
        self.last_box = None;
        self.room_state.reset();
    }

    fn card(&mut self) -> Option<CardSpec> {
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "FramingCard".into(), props: self.all_props() }, region: "left".into() })
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        self.catch_up(host);
        let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
        let at = |lo: i64, hi: i64| (event.args.first().and_then(Value::as_f64).unwrap_or(0.0).round() as i64).clamp(lo, hi);
        match event.callback.as_str() {
            "switch-toggled-by-hand" => self.switched_by_hand(host, on),
            "framing-moved" => self.set_framing(host, at(0, 30)),
            "framing-peek-open" => self.peek_open(host, on),
            "speed-peek-open" => self.chase_open(host, on),
            "speed-moved" => self.set_speed(host, at(1, 10)),
            "room-moved" => self.set_room(host, at(0, ROOMS.len() as i64 - 1)),
            "hand-toggled" => {
                self.hand = on;
                host.set_ui_props("card", &json!({"hand": on}));
                self.show_reset(host);
                host.schedule_save();
            }
            "pick-clicked" => self.pick(host),
            "follow-all-clicked" => {
                if self.picking {
                    host.cancel_pick(NAME);
                }
                self.followed = None;
                self.others.clear();
                self.last_box = None;
                self.show_following(host);
            }
            "reset-clicked" => self.reset(host),
            "download-clicked" => {
                host.download_vision();
                self.show_status(host);
            }
            _ => {}
        }
        self.sync_heads(host);
    }

    fn actions(&mut self) -> Vec<ActionSpec> {
        let group = tr(TITLE);
        let how = tr(HOW_MUCH);
        let quick = tr(SPEED_LABEL);
        let framing = SliderSpec { id: "framing.framing", label: &how, group: &group, keywords: "", minimum: 0.0, maximum: 30.0, step: 1.0, decimals: 0, suffix: "", has_default: true };
        let speed = SliderSpec { id: "framing.speed", label: &quick, group: &group, keywords: "", minimum: 1.0, maximum: 10.0, step: 1.0, decimals: 0, suffix: "", has_default: true };
        vec![
            actions::toggle_action("framing.on", &tr(TITLE), &group, "", &[]),
            actions::slider_action(&framing, self.framing as f64),
            actions::slider_action(&speed, self.speed as f64),
        ]
    }

    fn run_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) -> Option<String> {
        self.catch_up(host);
        // The card is dimmed until a phone is paired, and its shortcuts have nothing to do
        if !self.paired.on() {
            return None;
        }
        let said = match id {
            "framing.on" => {
                let want = actions::toggle_wanted(params, self.switch, &mut self.held);
                if want != self.switch {
                    self.switched_by_hand(host, want);
                }
                Some(actions::toggle_text(&tr(TITLE), want))
            }
            "framing.framing" => {
                let target = actions::slider_target(params, self.framing, 1.0, 0, 30, Some(Self::framing_steps(FRAMING_DEFAULT)))?;
                self.set_framing(host, target);
                Some(actions::slider_text(&tr(HOW_MUCH), &actions::shown(target as f64, 0, "", false)))
            }
            "framing.speed" => {
                let target = actions::slider_target(params, self.speed, 1.0, 1, 10, Some(SPEED_DEFAULT))?;
                self.set_speed(host, target);
                Some(actions::slider_text(&tr(SPEED_LABEL), &self.speed_text()))
            }
            _ => None,
        };
        self.sync_heads(host);
        said
    }

    fn release_action(&mut self, host: &mut dyn Host, id: &str, params: &Value) {
        if id != "framing.on" {
            return;
        }
        if let Some(before) = actions::hold_released(params, &mut self.held) {
            if before != self.switch && self.paired.on() {
                self.switched_by_hand(host, before);
            }
        }
        self.sync_heads(host);
    }

    fn settings(&self) -> Vec<telescope_core::settings::SettingSpec> {
        use telescope_core::settings::SettingSpec;
        vec![
            SettingSpec::switch("on", "Whether the picture follows you", false),
            SettingSpec::number("framing", "How much of you is in frame: 0 head, 1 chest, 2 waist, 3 body", 0.0, FRAMINGS.len() as f64 - 1.0, 1, FRAMING_DEFAULT),
            SettingSpec::switch("hand", "Whether the framing widens when you hold a hand out", false),
        ]
    }

    fn get_config(&self) -> Value {
        json!({"on": self.switch, "framing": self.framing_value(), "speed": self.speed, "room": self.room, "hand": self.hand})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let framing = cfg.get("framing").filter(|v| v.is_number()).and_then(Value::as_f64).filter(|f| (0.0..=FRAMINGS.len() as f64 - 1.0).contains(f));
        self.framing = Self::framing_steps(framing.unwrap_or(FRAMING_DEFAULT));
        self.speed = cfg.get("speed").and_then(Value::as_i64).filter(|s| (1..=10).contains(s)).unwrap_or(SPEED_DEFAULT);
        self.room = cfg.get("room").and_then(Value::as_i64).filter(|r| (0..ROOMS.len() as i64).contains(r)).unwrap_or(ROOM_DEFAULT);
        // Off unless turned on
        self.hand = cfg.get("hand") == Some(&Value::Bool(true));
        self.switch = cfg.get("on") == Some(&Value::Bool(true));
        self.cycle |= self.steering && !self.switch;
        self.pending = true;
        Ok(())
    }

    fn apply_preset(&mut self, host: &mut dyn Host, cfg: &Value) -> Result<(), ConfigRejected> {
        let done = self.set_config(cfg);
        self.catch_up(host);
        self.sync_heads(host);
        done
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("Auto framing".into(), if self.steering { "on" } else { "off" }.into())]
    }
}

#[cfg(test)]
#[path = "framing_tests.rs"]
mod tests;
