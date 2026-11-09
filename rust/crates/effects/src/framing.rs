//! The Auto framing plugin (plugins/framing.py): zooms and pans to keep the people in the picture framed, through the window's view.

use crate::peek;
use crate::shared::Paired;
use crate::vision_row;
use serde_json::{json, Value};
use std::collections::VecDeque;
use telescope_core::actions::{self, round_half_even, SliderSpec};
use telescope_core::event::Event;
use telescope_core::host::{Host, View};
use telescope_core::i18n::{n_, tr};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
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
const FRAMING_TIP: &str = n_("How much of you is in the picture, from just your head to all of you.");
const SPEED_TIP: &str = n_("How quickly the framing follows when you move.");
const ROOM_TIP: &str = n_("How much you can move before the framing follows you, from a fidget to walking around.");
const HAND_NAME: &str = n_("Show hands");
const HAND_DETAIL: &str = n_("Widens the view so your hands are in it too.");
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
pub fn target_box_with(people: &[Person], aspect: f64, framing: f64, hands: &[&str], props: &Proportions) -> Option<Box4> {
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
    /// The Framing bubble is up: its crop follows the slider.
    peeking: bool,
    /// The Speed bubble is up: its dots run.
    chasing: Option<Chase>,
    /// When the dots last moved, for the time they move by
    chased_at: Option<f64>,
    /// What the Framing bubble last showed: the part of the stand-in scene, so it is only redrawn when it moves
    peek_shown: Option<[f64; 4]>,
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
               "room-tip": tr(ROOM_TIP), "hand-name": tr(HAND_NAME), "hand-detail": tr(HAND_DETAIL), "fits": fits.join("\n")})
    }

    fn state_props(&self) -> Value {
        json!({"framing-peek": "picture", "on": self.switch, "hand": self.hand, "active": self.paired.on(), "framing": self.framing, "speed": self.speed, "room": self.room,
               "framing-text": self.framing_text(), "speed-text": self.speed_text(), "room-text": self.room_text()})
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
            (self.spring, self.goal, self.smooth) = (None, None, None);
            self.room_state.reset();
            self.ticked_at = None;
            host.schedule(TICK_MS, T_TICK);
        } else if !on && self.steering {
            self.steering = false;
            self.last_box = None;
            host.cancel_schedule(T_TICK);
            host.want_people(TITLE, false);
            host.release_view(TITLE);
            self.people.clear();
            self.seen_at = None;
        }
        self.refresh_status(host, self.switch, String::new());
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
            host.schedule_save();
        }
    }

    fn set_room(&mut self, host: &mut dyn Host, value: i64) {
        if value != self.room {
            self.room = value;
            host.set_ui_props("card", &json!({"room": value, "room-text": self.room_text()}));
            host.schedule_save();
        }
    }

    fn on_people(&mut self, host: &mut dyn Host, source_id: &str, people: &[Value]) {
        if self.steering && source_id == host.focused_source_id().unwrap_or_default() {
            self.people = people.iter().filter_map(Person::from_value).collect();
            let aspect = host.view_aspect() as f64;
            self.proportions.learn(&self.people, if aspect != 0.0 { aspect } else { 16.0 / 9.0 });
            self.seen_at = Some((self.clock)());
        }
    }

    fn on_view_changed(&mut self, view: &View) {
        let (old, new) = (self.orientation, turn_of(view));
        self.orientation = new;
        if old == new {
            return;
        }
        let remap = turned(old, new);
        self.people = self.people.iter().map(|person| person.mapped(&remap)).collect();
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
        let people: &[Person] = if fresh { &self.people } else { &[] };
        let out = if self.hand { hands_out(people, aspect) } else { Vec::new() };
        let mut widened = Vec::new();
        for (side, latch) in &mut self.hands {
            if latch.update(out.contains(side), now) {
                widened.push(*side);
            }
        }
        self.widened = widened;
        let found = target_box_with(people, aspect, self.framing_value(), &self.widened, &self.proportions);
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
            about: n_("Zooms and pans to keep you in the picture as you move.").into(),
            when_off: n_("Without it there's no Auto framing card and the zoom stays where you put it.").into(),
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
            Event::ViewChanged { view } => self.on_view_changed(view),
            Event::MaxZoomChanged { max_zoom } => self.max_zoom = (*max_zoom as f64).max(1.0),
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
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, _stream: &StreamInfo) {
        self.catch_up(host);
        self.people.clear();
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
                host.schedule_save();
            }
            "download-clicked" => {
                host.download_vision();
                self.show_status(host);
            }
            _ => {}
        }
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
        match id {
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
        }
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
        done
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("Auto framing".into(), if self.steering { "on" } else { "off" }.into())]
    }
}

#[cfg(test)]
#[path = "framing_tests.rs"]
mod tests;
