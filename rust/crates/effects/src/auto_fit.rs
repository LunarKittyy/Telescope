//! Auto colour grading's fit: what the picture should be like is decided from the measurement first, then the basic sliders are searched in stages (white balance, brightness, levels, pivot, highlights, saturation) a few at a time, running every candidate through `Grade::apply` on the measurement's sample pixels. Changing nothing is always a candidate and the search has no randomness, so the same frames give the same sliders.

use crate::auto_measure::*;
use crate::auto_tuning::*;
use crate::grade::{Grade, MASTER_RANGE, PIVOT};
use crate::scopes::SKIN_ANGLE;

/// The sliders Auto sets and replaces outright each time: Temperature, Tint, the master Lift, Gamma and Gain, Contrast, Pivot, Highlights and Saturation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Basic {
    pub temperature: f64,
    pub tint: f64,
    pub lift: f64,
    pub gamma: f64,
    pub gain: f64,
    pub contrast: f64,
    pub pivot: f64,
    pub highlights: f64,
    pub saturation: f64,
}

impl Default for Basic {
    fn default() -> Basic {
        Basic { temperature: 0.0, tint: 0.0, lift: 0.0, gamma: 0.0, gain: 1.0, contrast: 1.0, pivot: PIVOT, highlights: 0.0, saturation: 1.0 }
    }
}

impl Basic {
    pub fn of(g: &Grade) -> Basic {
        Basic {
            temperature: g.temperature, tint: g.tint, lift: g.masters[0], gamma: g.masters[1], gain: g.masters[2], contrast: g.contrast,
            pivot: g.pivot, highlights: g.highlights, saturation: g.saturation,
        }
    }

    pub fn put(&self, g: &mut Grade) {
        (g.temperature, g.tint, g.contrast, g.pivot, g.highlights, g.saturation) = (self.temperature, self.tint, self.contrast, self.pivot, self.highlights, self.saturation);
        (g.masters[0], g.masters[1], g.masters[2]) = (self.lift, self.gamma, self.gain);
    }

    /// A grade of these and nothing else.
    pub fn grade(&self) -> Grade {
        let mut g = Grade::default();
        self.put(&mut g);
        g
    }

    /// Whether each group of sliders is off neutral by enough to be worth saying.
    fn moved(&self) -> Moved {
        let n = Basic::default();
        Moved {
            white_balance: self.temperature.abs() >= WB_MIN || self.tint.abs() >= WB_MIN,
            exposure: (self.gamma - n.gamma).abs() >= 0.01 || (self.gain - n.gain).abs() >= 0.01,
            levels: (self.lift - n.lift).abs() >= 0.003 || (self.contrast - n.contrast).abs() >= 0.01,
            highlights: self.highlights.abs() >= 1.0,
            saturation: (self.saturation - n.saturation).abs() >= 0.02,
        }
    }

    /// Anything too small to see goes back to neutral, so a picture that's already right isn't nudged.
    fn snapped(mut self) -> Basic {
        let (n, m) = (Basic::default(), self.moved());
        if !m.white_balance {
            (self.temperature, self.tint) = (n.temperature, n.tint);
        }
        if !m.exposure {
            (self.gamma, self.gain) = (n.gamma, n.gain);
        }
        if !m.levels {
            (self.lift, self.contrast, self.pivot) = (n.lift, n.contrast, n.pivot);
        }
        if (self.contrast - 1.0).abs() < 0.01 {
            self.pivot = n.pivot;
        }
        if !m.highlights {
            self.highlights = n.highlights;
        }
        if !m.saturation {
            self.saturation = n.saturation;
        }
        self
    }
}

/// Which groups of sliders Auto moved.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Moved {
    pub white_balance: bool,
    pub exposure: bool,
    pub levels: bool,
    pub highlights: bool,
    pub saturation: bool,
}

impl Moved {
    pub fn any(&self) -> bool {
        self.white_balance || self.exposure || self.levels || self.highlights || self.saturation
    }
}

/// What came of the white balance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wb {
    /// Corrected part of the way, going by the face (and the grey background when it agreed).
    Corrected,
    /// Corrected part of the way going by grey background only, as there was no face.
    GreyOnly,
    /// Corrected a little going by the whole picture averaging out to grey, as there was no face or grey background.
    World,
    /// The face already sits where skin does, so it was left alone.
    InRange,
    /// The face and the background (or the background's own areas) disagree: the light is mixed or coloured, so it was left alone.
    Conflict,
    /// Nothing to go by (no face, no agreeing grey areas) or the evidence was unsure, so it was left alone.
    Unsure,
}

/// What the picture should be like, decided from the measurement before any search. None means nothing wants changing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Targets {
    pub face_luma: Option<f32>,
    /// With no face, the middle brightness a dark picture is brought up to.
    pub scene_luma: Option<f32>,
    /// The scene target is a ceiling to come down to rather than a floor to come up to.
    pub scene_bright: bool,
    /// Where the brightest 1% should reach, for a picture with nothing near white in it.
    pub top: Option<f32>,
    pub black: Option<f32>,
    /// The most Lift may add, less on a noisy picture.
    pub lift_up: f64,
    pub spread: Option<f32>,
    pub near_clip: Option<f32>,
    pub chroma: Option<f32>,
}

pub fn decide(m: &Measurement) -> Targets {
    let t = &m.tone;
    let face_luma = m.face.and_then(|f| {
        if f.luma < FACE_DARK_BELOW && t.p50 < PICTURE_DARK_BELOW {
            Some(FACE_DARK_TARGET)
        } else if f.luma > FACE_BRIGHT_ABOVE || f.clipped > FACE_CLIPPED_ABOVE {
            Some((f.luma - 0.05).min(FACE_BRIGHT_TARGET))
        } else {
            None
        }
    });
    let scene_luma = if m.face.is_some() {
        None
    } else if t.p50 >= SCENE_BLACK_BELOW && t.p50 < SCENE_DARK_BELOW {
        Some(SCENE_DARK_TARGET)
    } else if t.p50 > SCENE_BRIGHT_ABOVE || t.white_clipped > SCENE_BLOWN_ABOVE {
        Some(if t.white_clipped > 0.30 { SCENE_BLOWN_TARGET } else { SCENE_BRIGHT_TARGET })
    } else {
        None
    };
    let quiet = (1.0 - m.noise / NOISE_LIFT_MAX).clamp(0.0, 1.0) as f64;
    let lift_up = LIFT_RANGE.1 * quiet;
    // a picture the camera already clipped at an end reaches it, so its unclipped percentiles say nothing about milky blacks or a flat range
    let low = if t.black_clipped >= CLIPPED_ENDS { 0.0 } else { t.p01 };
    let high = if t.white_clipped >= CLIPPED_ENDS { 1.0 } else { t.p99 };
    // a picture with no range at all is a wall or a one-colour room, which stays as it is
    let ranged = high - low >= LEVELS_MIN_SPREAD;
    let black = if !ranged {
        None
    } else if low > BLACK_HIGH {
        Some(BLACK_TARGET)
    } else if low < BLACK_LOW && t.black_clipped <= BLACK_CLIP_MAX && lift_up > 0.0 {
        Some(BLACK_TARGET)
    } else {
        None
    };
    let scene_bright = scene_luma.is_some_and(|t| t > SCENE_DARK_TARGET);
    let top = (ranged && low < TOP_BLACK_BELOW && t.p99 < TOP_DARK_BELOW && t.white_clipped < CLIPPED_ENDS).then_some(TOP_TARGET);
    Targets {
        face_luma,
        scene_luma,
        scene_bright,
        top,
        black,
        lift_up,
        spread: (ranged && (high - low) < SPREAD_FLAT).then_some(SPREAD_TARGET),
        near_clip: (t.near_clip > NEAR_CLIP_ABOVE).then_some(NEAR_CLIP_TARGET),
        chroma: if t.chroma > 0.0 && t.chroma < CHROMA_DULL_BELOW {
            Some(CHROMA_DULL_TARGET)
        } else if t.chroma > CHROMA_LOUD_ABOVE {
            Some(CHROMA_LOUD_TARGET)
        } else {
            None
        },
    }
}

/// The answer: the sliders, and the story of how it got there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub values: Basic,
    pub moved: Moved,
    pub wb: Wb,
    pub had_face: bool,
    pub targets: Targets,
    /// The person's own wheels and curves on top would have clipped more, so Auto went part of the way or not at all.
    pub backed_off: bool,
}

// ── Colour maths on the samples ──────────────────────────────────────

/// Where a colour lands on a vectorscope, in degrees from the Cb axis towards Cr, 0..360.
pub fn skin_angle(p: Px) -> f32 {
    let y = 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2];
    let (cb, cr) = (0.564 * (p[2] - y), 0.713 * (p[0] - y));
    cr.atan2(cb).to_degrees().rem_euclid(360.0)
}

/// How many degrees a colour's hue is past the band skin sits in.
pub fn angle_excess(p: Px) -> f32 {
    angle_beyond(p, SKIN_BAND_DEG)
}

/// The same for a band of another width, which the correction aims for once skin is far enough off to be corrected.
fn angle_beyond(p: Px, band: f32) -> f32 {
    let d = (skin_angle(p) - SKIN_ANGLE as f32 + 180.0).rem_euclid(360.0) - 180.0;
    (d.abs() - band).max(0.0)
}

fn hsv_saturation(p: Px) -> f32 {
    let top = p[0].max(p[1]).max(p[2]);
    if top <= 1e-4 { 0.0 } else { (top - p[0].min(p[1]).min(p[2])) / top }
}

fn flat(px: &[Px]) -> Vec<f32> {
    px.iter().flatten().copied().collect()
}

fn triplets(buf: &[f32]) -> impl Iterator<Item = Px> + '_ {
    buf.as_chunks::<3>().0.iter().copied()
}

fn middle_colour(buf: &[f32]) -> Px {
    std::array::from_fn(|c| median(&mut triplets(buf).map(|p| p[c]).collect::<Vec<_>>()))
}

fn mean_colour(buf: &[f32]) -> Px {
    let n = (buf.len() / 3).max(1) as f32;
    std::array::from_fn(|c| triplets(buf).map(|p| p[c]).sum::<f32>() / n)
}

/// The searcher's view of the picture: the samples flattened to run through `Grade::apply`.
#[derive(Clone)]
struct Rig {
    skin: Vec<f32>,
    patches: Vec<f32>,
    /// Every sample with the ones the camera clipped marked, and just the others' colours for percentiles.
    all: Vec<f32>,
    clipped: Vec<bool>,
    /// A thin set of the unclipped samples to guard against new clipping with.
    guard: Vec<f32>,
    /// Where the person isn't, with the ones the camera clipped marked (the whole picture when there's no mask).
    bg: Vec<(Px, bool)>,
}

impl Rig {
    /// The mean colour of the mid-bright unclipped samples, as a patch of its own.
    fn world(&self) -> Option<Rig> {
        let (mut sum, mut n) = ([0.0f32; 3], 0usize);
        for (p, c) in &self.bg {
            if !*c && (0.10..=0.90).contains(&luma(*p)) {
                (0..3).for_each(|i| sum[i] += p[i]);
                n += 1;
            }
        }
        // a picture with much of it clipped has lost its colour where it clipped, and what's left says nothing of the light
        let clipped = self.bg.iter().filter(|(_, c)| *c).count() as f32 / self.bg.len().max(1) as f32;
        (n >= 50 && clipped < 0.05).then(|| Rig { patches: sum.map(|v| v / n as f32).to_vec(), ..self.clone() })
    }
}

impl Rig {
    fn new(m: &Measurement) -> Rig {
        let unclipped: Vec<Px> = m.all.iter().filter(|p| !p.clipped).map(|p| p.rgb).collect();
        Rig {
            skin: flat(&m.skin),
            patches: flat(&m.patches),
            all: flat(&m.all.iter().map(|p| p.rgb).collect::<Vec<_>>()),
            clipped: m.all.iter().map(|p| p.clipped).collect(),
            guard: flat(&thin(&unclipped, 500)),
            bg: if m.background.is_empty() { m.all.iter().map(|p| (p.rgb, p.clipped)).collect() } else { m.background.iter().map(|p| (p.rgb, p.clipped)).collect() },
        }
    }
}

fn run(g: &Grade, input: &[f32]) -> Vec<f32> {
    let mut out = input.to_vec();
    g.apply(&mut out);
    out
}

/// The share of the guard samples that come out clipped, past what's allowed, squared into a cost.
fn clip_cost(rig: &Rig, g: &Grade) -> f64 {
    if rig.guard.is_empty() {
        return 0.0;
    }
    let out = run(g, &rig.guard);
    let share = triplets(&out).filter(|p| p[0].max(p[1]).max(p[2]) >= CLIPPED).count() as f32 / (out.len() / 3) as f32;
    (((share - NEW_CLIP_OK).max(0.0) / 0.01) as f64).powi(2)
}

/// Brightness percentiles (darkest 1%, brightest 1%) of the samples the camera didn't clip, after the grade.
fn tone_after(rig: &Rig, g: &Grade) -> (f32, f32) {
    let out = run(g, &rig.all);
    let mut l: Vec<f32> = triplets(&out).zip(&rig.clipped).filter(|(_, c)| !**c).map(|(p, _)| luma(p)).collect();
    l.sort_by(f32::total_cmp);
    (percentile(&l, 0.01), percentile(&l, 0.99))
}

fn near_clip_after(rig: &Rig, g: &Grade) -> f32 {
    let out = run(g, &rig.all);
    triplets(&out).filter(|p| luma(*p) >= NEAR_CLIP_LUMA).count() as f32 / (out.len() / 3).max(1) as f32
}

fn chroma_after(rig: &Rig, g: &Grade) -> f32 {
    let out = run(g, &rig.all);
    let (mut sum, mut n) = (0.0, 0usize);
    for (p, c) in triplets(&out).zip(&rig.clipped) {
        let l = luma(p);
        if !*c && (CHROMA_LUMA.0..=CHROMA_LUMA.1).contains(&l) {
            sum += p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]);
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
}

fn scene_luma_after(rig: &Rig, g: &Grade) -> f32 {
    let out = run(g, &rig.all);
    // blown areas stay white whatever is done, so a picture with many of them is judged by the rest
    let blown = rig.clipped.iter().filter(|c| **c).count() as f32 / rig.clipped.len().max(1) as f32 > 0.05;
    let mut l: Vec<f32> = triplets(&out).zip(&rig.clipped).filter(|(_, c)| !blown || !**c).map(|(p, _)| luma(p)).collect();
    median(&mut l)
}

fn face_luma_after(rig: &Rig, g: &Grade) -> f32 {
    luma(middle_colour(&run(g, &rig.skin)))
}

// ── The search ───────────────────────────────────────────────────────

/// Compass search from `start` inside lo..hi: each axis tried up and down by its step, a step that finds nothing halves them all. No randomness.
fn minimise<const N: usize>(start: [f64; N], lo: [f64; N], hi: [f64; N], step: [f64; N], cost: &mut dyn FnMut(&[f64; N]) -> f64) -> ([f64; N], f64) {
    let (mut best, mut best_cost) = (start, cost(&start));
    let (mut step, mut evals) = (step, 1usize);
    let floor = step.map(|s| s / 40.0);
    while evals < STAGE_BUDGET && (0..N).any(|a| step[a] > floor[a]) {
        let mut improved = false;
        for axis in 0..N {
            for dir in [1.0, -1.0] {
                let mut p = best;
                p[axis] = (p[axis] + dir * step[axis]).clamp(lo[axis], hi[axis]);
                if p[axis] == best[axis] {
                    continue;
                }
                let c = cost(&p);
                evals += 1;
                if c < best_cost - 1e-12 {
                    (best, best_cost, improved) = (p, c, true);
                    break;
                }
            }
        }
        if !improved {
            step = step.map(|s| s / 2.0);
        }
    }
    (best, best_cost)
}

/// The searched values if they beat changing nothing by `IMPROVE_MARGIN`, else the neutral ones.
fn pick<const N: usize>(neutral: [f64; N], found: ([f64; N], f64), neutral_cost: f64) -> [f64; N] {
    if found.1 < IMPROVE_MARGIN * neutral_cost { found.0 } else { neutral }
}

fn moves(p: &[f64], neutral: &[f64], weights: &[f64]) -> f64 {
    p.iter().zip(neutral).zip(weights).map(|((v, n), w)| w * (v - n).powi(2)).sum()
}

fn wb_grade(t: f64, n: f64) -> Grade {
    Grade { temperature: t, tint: n, ..Grade::default() }
}

fn wb_cost(rig: &Rig, p: &[f64; 2], face: bool, grey: bool) -> f64 {
    let g = wb_grade(p[0], p[1]);
    let mut cost = 0.0;
    if face {
        let excess = angle_beyond(mean_colour(&run(&g, &rig.skin)), SKIN_AIM_DEG) / SKIN_BAND_DEG;
        cost += WB_FACE_WEIGHT * (excess as f64).powi(2);
    }
    if grey {
        let out = run(&g, &rig.patches);
        let n = (out.len() / 3) as f32;
        let cast = triplets(&out).map(linear_ratios).fold([0.0f32; 2], |a, r| [a[0] + r[0] / n, a[1] + r[1] / n]);
        cost += WB_GREY_WEIGHT * ((cast[0].hypot(cast[1]) / GREY_AGREE) as f64).powi(2) / 4.0;
    }
    cost + moves(p, &[0.0, 0.0], &[WB_COST_TEMPERATURE / 100.0, WB_COST_TINT / 100.0]) + clip_cost(rig, &g)
}

/// How far the whole picture's average colour is from grey (log ratios, linear light), for tuning.
pub fn world_cast(m: &Measurement) -> Option<f32> {
    let w = Rig::new(m).world()?;
    let c = linear_ratios(mean_colour(&w.patches));
    Some(c[0].hypot(c[1]))
}

fn solve_wb(rig: &Rig, face: bool, grey: bool) -> [f64; 2] {
    let neutral = wb_cost(rig, &[0.0, 0.0], face, grey);
    let (rt, rn) = (WB_MAX_TEMPERATURE / WB_STRENGTH, WB_MAX_TINT / WB_STRENGTH);
    let found = minimise([0.0, 0.0], [-rt, -rn], [rt, rn], [8.0, 8.0], &mut |p| wb_cost(rig, p, face, grey));
    pick([0.0, 0.0], found, neutral)
}

/// Opposite ways, far enough apart to matter.
fn conflicting(a: [f64; 2], b: [f64; 2]) -> bool {
    (a[0] * b[0] < 0.0 && (a[0] - b[0]).abs() > WB_CONFLICT_TEMPERATURE) || (a[1] * b[1] < 0.0 && (a[1] - b[1]).abs() > WB_CONFLICT_TINT)
}

/// A gentle correction going by the whole picture averaging out to grey, only when it is far from it.
fn world_fix(rig: &Rig, face: bool) -> Option<[f64; 2]> {
    let world = rig.world()?;
    let cast = linear_ratios(mean_colour(&world.patches));
    let size = cast[0].hypot(cast[1]);
    // with a face to vouch that the light is what's off, a bigger share of the way is gone and no cast is too big; without one a far bigger tint than a lamp gives is coloured light or a coloured room, which is meant
    let (min, max_cast, base, ramp, top, caps) = if face {
        (WORLD_MIN_FACE, f32::MAX, WB_WORLD_STRENGTH_FACE, WB_WORLD_RAMP_FACE, WB_STRENGTH, (WB_MAX_TEMPERATURE, WB_MAX_TINT))
    } else {
        (WORLD_MIN, WORLD_MAX_CAST, WB_WORLD_STRENGTH, WB_WORLD_RAMP, 0.5, WB_WORLD_MAX)
    };
    if !(min..=max_cast).contains(&size) {
        return None;
    }
    let solved = solve_wb(&world, false, true);
    // the further past the threshold the cast is, the more sure it is a cast and not the room's colours
    let strength = (base + (size - min) as f64 * ramp).min(top);
    let capped = [(solved[0] * strength).clamp(-caps.0, caps.0), (solved[1] * strength).clamp(-caps.1, caps.1)];
    (capped[0].abs() >= WB_MIN || capped[1].abs() >= WB_MIN).then_some(capped)
}

fn white_balance(m: &Measurement, rig: &Rig) -> (Wb, [f64; 2]) {
    let capped = |p: [f64; 2], strength: f64| [(p[0] * strength).clamp(-WB_MAX_TEMPERATURE, WB_MAX_TEMPERATURE), (p[1] * strength).clamp(-WB_MAX_TINT, WB_MAX_TINT)];
    let face = m.face.is_some() && !rig.skin.is_empty();
    if face && angle_excess(mean_colour(&rig.skin)) == 0.0 {
        // skin that looks right still can't hide a picture that is plainly tinted all over
        // not when the walls disagree (mixed light), and not if it would push the skin off the line
        let keeps_skin = |t: &[f64; 2]| angle_excess(mean_colour(&run(&wb_grade(t[0], t[1]), &rig.skin))) == 0.0;
        return match world_fix(rig, true).filter(|t| !matches!(m.grey, Grey::Agree { .. } | Grey::Conflict) && keeps_skin(t)) {
            Some(solved) => (Wb::World, solved),
            None => (Wb::InRange, [0.0, 0.0]),
        };
    }
    if m.grey == Grey::Conflict {
        return (Wb::Conflict, [0.0, 0.0]);
    }
    let agree = matches!(m.grey, Grey::Agree { .. }) && !rig.patches.is_empty();
    let solved = match (face, agree) {
        (true, true) => {
            let (f, g) = (solve_wb(rig, true, false), solve_wb(rig, false, true));
            if conflicting(f, g) {
                return (Wb::Conflict, [0.0, 0.0]);
            }
            capped(solve_wb(rig, true, true), WB_STRENGTH)
        }
        (true, false) => capped(solve_wb(rig, true, false), WB_STRENGTH),
        (false, true) => capped(solve_wb(rig, false, true), WB_GREY_ONLY_STRENGTH),
        (false, false) => {
            return match world_fix(rig, false) {
                Some(solved) => (Wb::World, solved),
                None => (Wb::Unsure, [0.0, 0.0]),
            };
        }
    };
    if solved[0].abs() < WB_MIN && solved[1].abs() < WB_MIN {
        return (Wb::Unsure, [0.0, 0.0]);
    }
    (if face { Wb::Corrected } else { Wb::GreyOnly }, solved)
}

fn with_basic(base: &Basic, edit: impl FnOnce(&mut Basic)) -> Grade {
    let mut b = *base;
    edit(&mut b);
    b.grade()
}

fn exposure(rig: &Rig, targets: &Targets, base: &Basic) -> (f64, f64) {
    let face_target = targets.face_luma.filter(|_| !rig.skin.is_empty());
    let target = face_target.or(targets.scene_luma);
    if target.is_none() && targets.top.is_none() {
        return (0.0, 1.0);
    }
    let cost = |p: &[f64; 2]| {
        let g = with_basic(base, |b| (b.gamma, b.gain) = (p[0], p[1]));
        let mut face = 0.0;
        if let Some(target) = target {
            let seen = if face_target.is_some() { face_luma_after(rig, &g) } else { scene_luma_after(rig, &g) };
            let miss = if face_target.is_none() && targets.scene_bright { (seen - target).max(0.0) } else { seen - target };
            face += ((miss / 0.05) as f64).powi(2);
        }
        if let Some(top) = targets.top {
            face += (((top - tone_after(rig, &g).1).max(0.0) / 0.06) as f64).powi(2);
        }
        face + clip_cost(rig, &g) + moves(p, &[0.0, 1.0], &[MOVE_COST[1], MOVE_COST[2]])
    };
    let neutral = cost(&[0.0, 1.0]);
    let found = minimise([0.0, 1.0], [GAMMA_RANGE.0, GAIN_RANGE.0], [GAMMA_RANGE.1, GAIN_RANGE.1], [0.1, 0.05], &mut |p| cost(p));
    let p = pick([0.0, 1.0], found, neutral);
    (p[0], p[1])
}

/// Lift and Contrast about a given pivot, against the black and spread targets, keeping the face's brightness where the exposure left it. Gives the values picked and what they cost.
fn levels(rig: &Rig, targets: &Targets, base: &Basic, pivot: f64, face_before: Option<f32>) -> ([f64; 2], f64) {
    let cost = |p: &[f64; 2]| {
        let g = with_basic(base, |b| (b.lift, b.contrast, b.pivot) = (p[0], p[1], pivot));
        let (p01, p99) = tone_after(rig, &g);
        let mut c = clip_cost(rig, &g) + moves(p, &[0.0, 1.0], &[MOVE_COST[0], MOVE_COST[3]]);
        if let Some(t) = targets.black {
            c += (((p01 - t) / 0.03) as f64).powi(2);
        }
        if let Some(t) = targets.spread {
            c += ((((p99 - p01) - t) / 0.06) as f64).powi(2);
        }
        if let Some(before) = face_before.filter(|_| !rig.skin.is_empty()) {
            c += (((face_luma_after(rig, &g) - before).abs() - FACE_STAY).max(0.0) / 0.05).powi(2) as f64;
        }
        c
    };
    let neutral = cost(&[0.0, 1.0]);
    let found = minimise([0.0, 1.0], [LIFT_RANGE.0, CONTRAST_RANGE.0], [targets.lift_up, CONTRAST_RANGE.1], [0.02, 0.1], &mut |p| cost(p));
    if found.1 < IMPROVE_MARGIN * neutral { (found.0, found.1) } else { ([0.0, 1.0], neutral) }
}

fn highlights(rig: &Rig, targets: &Targets, base: &Basic) -> f64 {
    let Some(target) = targets.near_clip else { return 0.0 };
    let cost = |p: &[f64; 1]| {
        let g = with_basic(base, |b| b.highlights = p[0]);
        let over = ((near_clip_after(rig, &g) - target).max(0.0) / 0.02) as f64;
        over.powi(2) + clip_cost(rig, &g) + moves(p, &[0.0], &[MOVE_COST[5]])
    };
    let neutral = cost(&[0.0]);
    let found = minimise([0.0], [HIGHLIGHTS_RANGE.0], [HIGHLIGHTS_RANGE.1], [10.0], &mut |p| cost(p));
    pick([0.0], found, neutral)[0]
}

fn saturation(rig: &Rig, targets: &Targets, base: &Basic) -> f64 {
    let Some(target) = targets.chroma else { return 1.0 };
    let skin = !rig.skin.is_empty();
    let before = with_basic(base, |_| {});
    let skin_before = mean_colour(&run(&before, &rig.skin));
    let (sat_cap, excess_cap) = (SKIN_SAT_MAX.min(hsv_saturation(skin_before) * SKIN_SAT_GROWTH + SKIN_SAT_SLACK), angle_excess(skin_before) + SKIN_HUE_SLACK_DEG);
    let cost = |p: &[f64; 1]| {
        let g = with_basic(base, |b| b.saturation = p[0]);
        let mut c = (((chroma_after(rig, &g) - target) / 0.02) as f64).powi(2) + moves(p, &[1.0], &[MOVE_COST[6]]);
        if skin {
            let out = mean_colour(&run(&g, &rig.skin));
            c += 4.0 * (((hsv_saturation(out) - sat_cap).max(0.0) / 0.03) as f64).powi(2);
            c += 4.0 * (((angle_excess(out) - excess_cap).max(0.0) / 3.0) as f64).powi(2);
        }
        c
    };
    let neutral = cost(&[1.0]);
    let found = minimise([1.0], [SATURATION_RANGE.0], [SATURATION_RANGE.1], [0.1], &mut |p| cost(p));
    pick([1.0], found, neutral)[0]
}

/// How badly a grade clips what it shouldn't, over the samples: new clipping of the picture and of the face.
fn damage(rig: &Rig, g: &Grade) -> (f32, f32) {
    let share = |buf: &[f32]| {
        let n = (buf.len() / 3).max(1) as f32;
        triplets(&run(g, buf)).filter(|p| p[0].max(p[1]).max(p[2]) >= CLIPPED).count() as f32 / n
    };
    (share(&rig.guard), share(&rig.skin))
}

/// The sliders for this measurement. `creative` is the person's grade as it stands: its own wheels, curves and LUT are left alone and only checked at the end that they and Auto together don't clip the picture or the face more than they would alone.
pub fn fit(m: &Measurement, creative: Option<&Grade>) -> Fit {
    let targets = decide(m);
    let rig = Rig::new(m);
    let mut values = Basic::default();

    let (wb, [t, n]) = white_balance(m, &rig);
    (values.temperature, values.tint) = (t, n);

    let (gamma, gain) = exposure(&rig, &targets, &values);
    (values.gamma, values.gain) = (gamma, gain);
    let face_before = (!rig.skin.is_empty()).then(|| face_luma_after(&rig, &values.grade()));

    // pivot stays at its default until contrast has moved, then goes near the face if that does better
    let (flat, flat_cost) = levels(&rig, &targets, &values, PIVOT, face_before);
    (values.lift, values.contrast) = (flat[0], flat[1]);
    if values.contrast > 1.0 && face_before.is_some() {
        let entering = face_luma_after(&rig, &with_basic(&values, |b| (b.contrast, b.lift) = (1.0, 0.0)));
        let pivot = (entering as f64).clamp(PIVOT_RANGE.0, PIVOT_RANGE.1);
        let (near, near_cost) = levels(&rig, &targets, &values, pivot, face_before);
        if near_cost < flat_cost - 1e-9 {
            (values.lift, values.contrast, values.pivot) = (near[0], near[1], pivot);
        }
    }

    values.highlights = highlights(&rig, &targets, &values);
    values.saturation = saturation(&rig, &targets, &values);
    values.temperature = values.temperature.clamp(-WB_MAX_TEMPERATURE, WB_MAX_TEMPERATURE);
    values.gamma = values.gamma.clamp(MASTER_RANGE[1].0, MASTER_RANGE[1].1);
    let mut values = values.snapped();
    // white balance left alone means the person's own stays, not neutral
    let keeps_wb = matches!(wb, Wb::Conflict | Wb::Unsure);
    if let Some(own) = creative.filter(|_| keeps_wb) {
        (values.temperature, values.tint) = (own.temperature, own.tint);
    }

    let mut backed_off = false;
    if let Some(own) = creative.filter(|_| values.moved().any()) {
        let mut alone = own.clone();
        Basic::default().put(&mut alone);
        let base = damage(&rig, &alone);
        let bad = |b: &Basic| {
            let mut g = own.clone();
            b.put(&mut g);
            let d = damage(&rig, &g);
            d.0 > base.0 + NEW_CLIP_OK * 4.0 || d.1 > base.1 + FINAL_FACE_CLIP
        };
        let neutral = Basic::default();
        for scale in [1.0, 0.5, 0.0] {
            let mix = |a: f64, b: f64| b + (a - b) * scale;
            let tried = Basic {
                temperature: if keeps_wb { values.temperature } else { mix(values.temperature, neutral.temperature) },
                tint: if keeps_wb { values.tint } else { mix(values.tint, neutral.tint) }, lift: mix(values.lift, neutral.lift),
                gamma: mix(values.gamma, neutral.gamma), gain: mix(values.gain, neutral.gain), contrast: mix(values.contrast, neutral.contrast),
                pivot: values.pivot, highlights: mix(values.highlights, neutral.highlights), saturation: mix(values.saturation, neutral.saturation),
            };
            if scale == 0.0 || !bad(&tried) {
                backed_off = scale < 1.0;
                values = tried.snapped();
                if keeps_wb {
                    (values.temperature, values.tint) = (tried.temperature, tried.tint);
                }
                break;
            }
        }
    }
    // what moved is judged without the white balance that was only kept
    let moved = if keeps_wb { Basic { temperature: 0.0, tint: 0.0, ..values }.moved() } else { values.moved() };
    Fit { values, moved, wb, had_face: m.face.is_some(), targets, backed_off }
}
