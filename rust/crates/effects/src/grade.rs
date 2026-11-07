//! The maths of the Colour grading card (telescope/grade.py): one Grade turns RGB values 0..1 into graded ones, in the order a
//! grading panel reads (white balance, wheels, contrast, tone, saturation, curves, then a LUT file).

use crate::cube::CubeLut;
use std::sync::Arc;

pub const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];
/// Where contrast turns around by default: about mid grey on a video signal.
pub const PIVOT: f64 = 0.435;
pub const WHEELS: [&str; 4] = ["lift", "gamma", "gain", "offset"];
pub const CHANNELS: [&str; 4] = ["all", "r", "g", "b"];
pub const LINEAR: [(f64, f64); 2] = [(0.0, 0.0), (1.0, 1.0)];
/// How far a wheel's puck at the rim moves each channel (wheel_shift gives up to about 0.9).
const WHEEL_REACH: [f32; 4] = [0.2, 0.5, 0.4, 0.2];
/// Each wheel's master slider: (lowest, highest, default).
pub const MASTER_RANGE: [(f64, f64); 4] = [(-0.5, 0.5), (-1.0, 1.0), (0.0, 2.0), (-0.5, 0.5)];
pub const MASTER_DEFAULT: [f64; 4] = [0.0, 0.0, 1.0, 0.0];
const TEMPERATURE_STOPS: f32 = 0.25;
const TINT_STOPS: f32 = 0.2;
/// Past the middle of the slider the reach grows, so the ends cover a real tungsten lamp (about 2700 K to 7500 K) and a green fluorescent.
const TEMPERATURE_EXTRA: f32 = 1.8;
const TINT_EXTRA: f32 = 1.0;

/// Stops a slider position (-1..1) stands for: linear within half the slider, then curving up.
fn reach(x: f32, per_unit: f32, extra: f32) -> f32 {
    let past = (x.abs() - 0.5).max(0.0);
    x.signum() * (per_unit * x.abs().min(0.5) + per_unit * past + extra * past * past)
}
/// Shadows or highlights at 100 add up to 4/27 of the range to the part they're about.
const TONE: f32 = 1.0;

/// A hue curve's points at red, yellow, green, cyan, blue and magenta.
pub fn neutral_hue() -> Vec<(f64, f64)> {
    (0..6).map(|i| (i as f64 / 6.0, 0.5)).collect()
}

pub fn wheel_index(name: &str) -> usize {
    WHEELS.iter().position(|w| *w == name).unwrap_or(0)
}

/// RGB change of a wheel's puck at (x, y) in the unit circle, x towards blue and y towards red as on a vectorscope (Rec. 709 Cb
/// and Cr at half scale). It adds up to no change in brightness.
pub fn wheel_shift(x: f64, y: f64) -> [f32; 3] {
    let (cb, cr) = (0.5 * x as f32, 0.5 * y as f32);
    [1.5748 * cr, -0.1873 * cb - 0.4681 * cr, 1.8556 * cb]
}

pub fn srgb_to_linear(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

pub fn linear_to_srgb(v: f32) -> f32 {
    let v = v.max(0.0);
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

fn luma(x: [f32; 3]) -> f32 {
    LUMA[0] * x[0] + LUMA[1] * x[1] + LUMA[2] * x[2]
}

/// Linear-light RGB gains for temperature and tint (-100..100; warmer and more magenta upwards), scaled so white keeps its
/// brightness.
pub fn white_balance_gains(temperature: f64, tint: f64) -> [f32; 3] {
    let (t, n) = (temperature as f32 / 100.0, tint as f32 / 100.0);
    let (warm, green) = (reach(t, TEMPERATURE_STOPS, TEMPERATURE_EXTRA), reach(n, TINT_STOPS, TINT_EXTRA));
    let gains = [2f32.powf(warm), 2f32.powf(-green), 2f32.powf(-warm)];
    let norm = luma(gains);
    gains.map(|g| g / norm)
}

/// A cubic Hermite spline through points with slopes at each.
#[derive(Clone, Debug)]
struct Hermite {
    xs: Vec<f64>,
    ys: Vec<f64>,
    m: Vec<f64>,
}

fn slopes(xs: &[f64], ys: &[f64]) -> Vec<f64> {
    let h: Vec<f64> = xs.windows(2).map(|w| if w[1] - w[0] <= 0.0 { 1e-9 } else { w[1] - w[0] }).collect();
    let d: Vec<f64> = ys.windows(2).zip(&h).map(|(w, h)| (w[1] - w[0]) / h).collect();
    let mut m = vec![0.0; ys.len()];
    m[0] = d[0];
    m[ys.len() - 1] = d[d.len() - 1];
    for i in 1..xs.len() - 1 {
        if d[i - 1] * d[i] <= 0.0 {
            m[i] = 0.0;
        } else {
            // weighted harmonic mean, which keeps each piece monotone
            let (w1, w2) = (2.0 * h[i] + h[i - 1], h[i] + 2.0 * h[i - 1]);
            m[i] = (w1 + w2) / (w1 / d[i - 1] + w2 / d[i]);
        }
    }
    m
}

impl Hermite {
    fn new(xs: Vec<f64>, ys: Vec<f64>) -> Hermite {
        let m = slopes(&xs, &ys);
        Hermite { xs, ys, m }
    }

    fn eval(&self, x: f64) -> f64 {
        let last = self.xs.len() - 1;
        if x <= self.xs[0] {
            return self.ys[0];
        }
        if x >= self.xs[last] {
            return self.ys[last];
        }
        let i = (self.xs.partition_point(|&v| v <= x).max(1) - 1).min(last - 1);
        let h = (self.xs[i + 1] - self.xs[i]).max(1e-9);
        let t = ((x - self.xs[i]) / h).clamp(0.0, 1.0);
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * self.ys[i] + (t3 - 2.0 * t2 + t) * h * self.m[i] + (-2.0 * t3 + 3.0 * t2) * self.ys[i + 1]
            + (t3 - t2) * h * self.m[i + 1]
    }
}

/// A curve as the grade evaluates it.
#[derive(Clone, Debug)]
enum Curve {
    Flat(f64),
    Spline(Hermite),
    /// A hue curve: x is a hue 0..1 and 1 is 0 again.
    Periodic(Hermite),
}

impl Curve {
    fn eval(&self, x: f64) -> f32 {
        (match self {
            Curve::Flat(v) => *v,
            Curve::Spline(h) => h.eval(x),
            Curve::Periodic(h) => h.eval(x.rem_euclid(1.0)),
        }) as f32
    }
}

/// The curve through points ((x, y), x rising, both 0..1): a monotone cubic (Fritsch and Carlson), so it never overshoots
/// between points. Flat past the first and last point.
fn monotone(points: &[(f64, f64)]) -> Curve {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if pts.len() < 2 {
        return Curve::Flat(pts.first().map_or(0.0, |p| p.1));
    }
    Curve::Spline(Hermite::new(pts.iter().map(|p| p.0).collect(), pts.iter().map(|p| p.1).collect()))
}

/// A hue curve: like monotone, but going round.
fn periodic(points: &[(f64, f64)]) -> Curve {
    let mut sorted: Vec<(f64, f64)> = points.iter().map(|&(x, y)| (x.rem_euclid(1.0), y)).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for (x, y) in sorted {
        match pts.last_mut() {
            // one hue given twice: the middle of the two
            Some(last) if x - last.0 < 1e-6 => last.1 = (last.1 + y) / 2.0,
            _ => pts.push((x, y)),
        }
    }
    // x near 1 is the hue at 0 again
    if pts.len() > 1 && pts[0].0 + 1.0 - pts[pts.len() - 1].0 < 1e-6 {
        let end = pts.pop().unwrap();
        pts[0].1 = (pts[0].1 + end.1) / 2.0;
    }
    match pts.len() {
        0 => return Curve::Flat(0.5),
        1 => return Curve::Flat(pts[0].1),
        _ => {}
    }
    let n = pts.len();
    let mut wrapped: Vec<(f64, f64)> = pts[n - 2..].iter().map(|&(x, y)| (x - 1.0, y)).collect();
    wrapped.extend(pts.iter().copied());
    wrapped.extend(pts[..2].iter().map(|&(x, y)| (x + 1.0, y)));
    Curve::Periodic(Hermite::new(wrapped.iter().map(|p| p.0).collect(), wrapped.iter().map(|p| p.1).collect()))
}

/// The tone curve through `points` at each x, as the card draws it.
pub fn monotone_curve(points: &[(f64, f64)], x: f64) -> f32 {
    monotone(points).eval(x)
}

/// A hue curve at hue x (0..1, 1 is 0 again).
pub fn periodic_curve(points: &[(f64, f64)], x: f64) -> f32 {
    periodic(points).eval(x)
}

/// HSV hue 0..1 (red 0, green 1/3, blue 2/3) of an RGB value; 0 for greys.
pub fn hue_of(rgb: [f32; 3]) -> f32 {
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
    let top = r.max(g).max(b);
    let low = r.min(g).min(b);
    if top - low <= 1e-6 {
        return 0.0;
    }
    let span = top - low;
    let h = if top == r { (g - b) / span } else if top == g { 2.0 + (b - r) / span } else { 4.0 + (r - g) / span };
    (h / 6.0).rem_euclid(1.0)
}

/// rgb turned around the grey axis by `turns` (1 = all the way round; positive goes red to green to blue).
pub fn rotate_hue(rgb: [f32; 3], turns: f32) -> [f32; 3] {
    let angle = 2.0 * std::f32::consts::PI * turns;
    let k = 1.0 / 3f32.sqrt();
    let cross = [k * (rgb[2] - rgb[1]), k * (rgb[0] - rgb[2]), k * (rgb[1] - rgb[0])];
    let along = k * k * (rgb[0] + rgb[1] + rgb[2]);
    let (cos, sin) = (angle.cos(), angle.sin());
    [0, 1, 2].map(|i| rgb[i] * cos + cross[i] * sin + along * (1.0 - cos))
}

fn neutral_points(points: &[(f64, f64)]) -> bool {
    points.iter().all(|(_, y)| (y - 0.5).abs() < 1e-6)
}

/// Where skin sits on the hue circle (in turns, orange) and how far either side of it Vibrance holds back.
const SKIN_HUE: f32 = 0.07;
const SKIN_REACH: f32 = 0.07;
/// How much of Vibrance skin tones still get at the middle of the skin hues.
const SKIN_SHARE: f32 = 0.25;

/// What Vibrance multiplies a colour's distance from grey by: the dull colours get most of a boost and the strong ones most of a cut, and skin hues get a quarter of either.
fn vibrance_gain(vibrance: f32, rgb: [f32; 3]) -> f32 {
    let top = rgb[0].max(rgb[1]).max(rgb[2]);
    let sat = if top > 1e-6 { (top - rgb[0].min(rgb[1]).min(rgb[2])) / top } else { 0.0 };
    let weight = if vibrance > 0.0 { (1.0 - sat) * (1.0 - sat) } else { sat };
    let t = (1.0 - (hue_of(rgb) - SKIN_HUE).abs() / SKIN_REACH).clamp(0.0, 1.0);
    let skin = 1.0 - (1.0 - SKIN_SHARE) * t * t * (3.0 - 2.0 * t);
    1.0 + vibrance / 100.0 * weight * skin
}

/// Every setting of the Colour grading card.
#[derive(Clone, Debug)]
pub struct Grade {
    pub temperature: f64,
    pub tint: f64,
    /// Each wheel's puck (x, y) in the unit circle, in WHEELS order.
    pub wheels: [(f64, f64); 4],
    pub masters: [f64; 4],
    pub contrast: f64,
    pub pivot: f64,
    /// -100..100
    pub shadows: f64,
    pub highlights: f64,
    pub saturation: f64,
    /// -100..100: saturation that favours dull colours and spares skin.
    pub vibrance: f64,
    /// The tone curves in CHANNELS order (all, r, g, b).
    pub curves: [Vec<(f64, f64)>; 4],
    pub hue_sat: Vec<(f64, f64)>,
    pub hue_hue: Vec<(f64, f64)>,
    pub lut: Option<Arc<CubeLut>>,
    pub lut_strength: f64,
}

impl Default for Grade {
    fn default() -> Grade {
        Grade {
            temperature: 0.0,
            tint: 0.0,
            wheels: [(0.0, 0.0); 4],
            masters: MASTER_DEFAULT,
            contrast: 1.0,
            pivot: PIVOT,
            shadows: 0.0,
            highlights: 0.0,
            saturation: 1.0,
            vibrance: 0.0,
            curves: std::array::from_fn(|_| LINEAR.to_vec()),
            hue_sat: neutral_hue(),
            hue_hue: neutral_hue(),
            lut: None,
            lut_strength: 1.0,
        }
    }
}

/// What a grade needs worked out once before it's run over many colours.
struct Prepared<'a> {
    grade: &'a Grade,
    gains: Option<[f32; 3]>,
    primaries: bool,
    offset: [f32; 3],
    lift: [f32; 3],
    gain: [f32; 3],
    power: [f32; 3],
    tone: bool,
    curves: Option<[Curve; 4]>,
    hue_hue: Option<Curve>,
    hue_sat: Option<Curve>,
    lut: Option<&'a CubeLut>,
}

impl Grade {
    fn wheel(&self, name: usize) -> [f32; 3] {
        let (x, y) = self.wheels[name];
        wheel_shift(x, y).map(|v| v * WHEEL_REACH[name])
    }

    fn primaries_neutral(&self) -> bool {
        (0..4).all(|w| self.wheels[w] == (0.0, 0.0) && self.masters[w] == MASTER_DEFAULT[w])
    }

    fn curves_linear(&self) -> bool {
        self.curves.iter().all(|c| c.as_slice() == LINEAR)
    }

    pub fn is_identity(&self) -> bool {
        self.temperature == 0.0 && self.tint == 0.0 && self.primaries_neutral() && self.contrast == 1.0 && self.shadows == 0.0
            && self.highlights == 0.0 && self.saturation == 1.0 && self.vibrance == 0.0 && self.curves_linear() && neutral_points(&self.hue_sat)
            && neutral_points(&self.hue_hue) && (self.lut.is_none() || self.lut_strength == 0.0)
    }

    /// Each channel only depends on itself: no saturation, tone, hue curves or 3D LUT in play.
    pub fn per_channel(&self) -> bool {
        self.shadows == 0.0 && self.highlights == 0.0 && self.saturation == 1.0 && self.vibrance == 0.0 && neutral_points(&self.hue_sat) && neutral_points(&self.hue_hue)
            && self.lut.as_ref().is_none_or(|l| self.lut_strength == 0.0 || l.per_channel())
    }

    /// Points the colour pipeline needs to carry this grade: 65 with a detailed 3D LUT file, else 33.
    pub fn lut_size(&self) -> u32 {
        match &self.lut {
            Some(l) if l.cube.is_some() && l.size() > 33 => 65,
            _ => 33,
        }
    }

    fn prepare(&self) -> Prepared<'_> {
        let primaries = !self.primaries_neutral();
        let (lift_i, gamma_i, gain_i, offset_i) = (0, 1, 2, 3);
        let (wl, wg, wn, wo) = (self.wheel(lift_i), self.wheel(gamma_i), self.wheel(gain_i), self.wheel(offset_i));
        let m = self.masters;
        Prepared {
            grade: self,
            gains: (self.temperature != 0.0 || self.tint != 0.0).then(|| white_balance_gains(self.temperature, self.tint)),
            primaries,
            offset: wo.map(|v| m[offset_i] as f32 + v),
            lift: wl.map(|v| m[lift_i] as f32 + v),
            gain: wn.map(|v| m[gain_i] as f32 * (1.0 + v)),
            power: wg.map(|v| 2f32.powf(-(m[gamma_i] as f32 + v))),
            tone: self.shadows != 0.0 || self.highlights != 0.0,
            curves: (!self.curves_linear()).then(|| std::array::from_fn(|i| monotone(&self.curves[i]))),
            hue_hue: (!neutral_points(&self.hue_hue)).then(|| periodic(&self.hue_hue)),
            hue_sat: (!neutral_points(&self.hue_sat)).then(|| periodic(&self.hue_sat)),
            lut: self.lut.as_deref().filter(|_| self.lut_strength > 0.0),
        }
    }

    /// RGB triples 0..1, graded in place.
    pub fn apply(&self, rgb: &mut [f32]) {
        let p = self.prepare();
        for px in rgb.as_chunks_mut::<3>().0 {
            let out = p.pixel([px[0], px[1], px[2]]);
            px.copy_from_slice(&out);
        }
    }

    /// One colour graded.
    pub fn eval(&self, rgb: [f32; 3]) -> [f32; 3] {
        self.prepare().pixel(rgb)
    }
}

impl Prepared<'_> {
    fn pixel(&self, rgb: [f32; 3]) -> [f32; 3] {
        let g = self.grade;
        let mut x = rgb;
        if let Some(gains) = &self.gains {
            for c in 0..3 {
                x[c] = linear_to_srgb(srgb_to_linear(x[c]) * gains[c]);
            }
        }
        if self.primaries {
            #[allow(clippy::needless_range_loop)] // c walks five parallel arrays
            for c in 0..3 {
                let v = x[c] + self.offset[c];
                let v = self.gain[c] * v + self.lift[c] * (1.0 - v);
                x[c] = v.max(0.0).powf(self.power[c]);
            }
        }
        if g.contrast != 1.0 || g.pivot != PIVOT {
            for v in &mut x {
                *v = (*v - g.pivot as f32) * g.contrast as f32 + g.pivot as f32;
            }
        }
        x = x.map(|v| v.clamp(0.0, 1.0));
        if self.tone {
            let y = luma(x).clamp(0.0, 1.0);
            let shift = TONE * (g.shadows as f32 / 100.0 * y * (1.0 - y) * (1.0 - y) + g.highlights as f32 / 100.0 * y * y * (1.0 - y));
            x = x.map(|v| v + shift);
        }
        if g.saturation != 1.0 {
            let y = luma(x);
            x = x.map(|v| y + g.saturation as f32 * (v - y));
        }
        if g.vibrance != 0.0 {
            let k = vibrance_gain(g.vibrance as f32, x.map(|v| v.clamp(0.0, 1.0)));
            let y = luma(x);
            x = x.map(|v| y + k * (v - y));
        }
        if let Some(curves) = &self.curves {
            x = x.map(|v| v.clamp(0.0, 1.0));
            x = x.map(|v| curves[0].eval(v as f64));
            x = [0, 1, 2].map(|c| curves[c + 1].eval(x[c] as f64));
        }
        if self.hue_hue.is_some() || self.hue_sat.is_some() {
            x = x.map(|v| v.clamp(0.0, 1.0));
            let hue = hue_of(x);
            if let Some(curve) = &self.hue_hue {
                x = rotate_hue(x, curve.eval(hue as f64) - 0.5);
            }
            if let Some(curve) = &self.hue_sat {
                let y = luma(x);
                let k = 2.0 * curve.eval(hue as f64);
                x = x.map(|v| y + k * (v - y));
            }
        }
        if let Some(lut) = self.lut {
            x = x.map(|v| v.clamp(0.0, 1.0));
            let looked = lut.eval(x);
            let s = g.lut_strength as f32;
            x = [0, 1, 2].map(|c| x[c] + s * (looked[c] - x[c]));
        }
        x.map(|v| v.clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::colour::lattice;

    /// A lattice of 5 points a side and 200 pseudo-random colours.
    fn colours() -> Vec<[f32; 3]> {
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f32 / (1u64 << 24) as f32
        };
        let mut out: Vec<[f32; 3]> = lattice(5).as_chunks::<3>().0.iter().map(|p| [p[0], p[1], p[2]]).collect();
        out.extend((0..200).map(|_| [next(), next(), next()]));
        out
    }

    fn lum(x: [f32; 3]) -> f32 {
        0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2]
    }

    fn check(grade: &Grade, expected: impl Fn([f32; 3]) -> [f32; 3], tol: f32) {
        for x in colours() {
            let (got, want) = (grade.eval(x), expected(x));
            assert!((0..3).all(|c| (got[c] - want[c]).abs() <= tol), "{x:?}: {got:?} vs {want:?}");
        }
    }

    fn masters(lift: f64, gamma: f64, gain: f64, offset: f64) -> [f64; 4] {
        [lift, gamma, gain, offset]
    }

    #[test]
    fn the_default_grade_changes_nothing() {
        let g = Grade::default();
        assert!(g.is_identity() && g.per_channel());
        check(&g, |x| x, 0.0);
    }

    #[test]
    fn temperature_and_tint_are_gains_in_linear_light() {
        let (t, n) = (40.0f32, -25.0f32);
        let (gr, gg, gb) = (2f32.powf(0.25 * t / 100.0), 2f32.powf(-0.2 * n / 100.0), 2f32.powf(-0.25 * t / 100.0));
        let norm = 0.2126 * gr + 0.7152 * gg + 0.0722 * gb;
        let lin = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        let srgb = |v: f32| if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055 };
        let grade = Grade { temperature: t as f64, tint: n as f64, ..Default::default() };
        check(&grade, |x| [srgb(lin(x[0]) * gr / norm), srgb(lin(x[1]) * gg / norm), srgb(lin(x[2]) * gb / norm)].map(|v| v.clamp(0.0, 1.0)), 1e-5);
        let white = grade.eval([1.0; 3]);
        assert!(white[0] > white[2], "warmer: more red than blue");
        assert!(Grade { temperature: t as f64, ..Default::default() }.per_channel());
    }

    #[test]
    fn lift_gamma_gain_and_offset_masters() {
        for (lift, gamma, gain, offset) in [(0.1, 0.0, 1.0, 0.0), (0.0, 0.5, 1.0, 0.0), (0.0, 0.0, 1.4, 0.0), (0.0, 0.0, 1.0, -0.1), (-0.05, -0.3, 0.8, 0.05)] {
            let grade = Grade { masters: masters(lift, gamma, gain, offset), ..Default::default() };
            check(&grade, |x| x.map(|v| {
                let y = v + offset as f32;
                let y = gain as f32 * y + lift as f32 * (1.0 - y);
                y.max(0.0).powf(2f32.powf(-gamma as f32)).clamp(0.0, 1.0)
            }), 1e-5);
            assert!(grade.per_channel());
        }
    }

    #[test]
    fn lift_pins_white_gain_pins_black_and_gamma_pins_both() {
        let ends = [[0.0f32; 3], [1.0; 3]];
        let at = |m: [f64; 4]| { let g = Grade { masters: m, ..Default::default() }; [g.eval(ends[0]), g.eval(ends[1])] };
        let near = |a: [[f32; 3]; 2], b: [[f32; 3]; 2]| (0..2).all(|i| (0..3).all(|c| (a[i][c] - b[i][c]).abs() < 1e-5));
        assert!(near(at(masters(0.2, 0.0, 1.0, 0.0)), [[0.2; 3], [1.0; 3]]));
        assert!(near(at(masters(0.0, 0.0, 0.5, 0.0)), [[0.0; 3], [0.5; 3]]));
        assert!(near(at(masters(0.0, 0.7, 1.0, 0.0)), ends));
    }

    #[test]
    fn a_wheel_pushes_towards_the_colour_it_points_at_without_changing_brightness() {
        let up = wheel_shift(0.0, 1.0);
        assert!(up[0] > 0.0 && 0.0 > up[1] && up[2].abs() < 1e-6, "towards red on a vectorscope");
        let right = wheel_shift(1.0, 0.0);
        assert!(right[2] > 0.0 && 0.0 > right[1], "towards blue");
        for (x, y) in [(0.3, -0.8), (1.0, 0.0), (-0.6, 0.6)] {
            assert!(lum(wheel_shift(x, y)).abs() < 1e-4, "the Rec. 709 matrix to four places");
        }
        let mut wheels = [(0.0, 0.0); 4];
        wheels[2] = (0.0, 1.0);
        let lifted = Grade { wheels, ..Default::default() }.eval([0.5; 3]);
        assert!(lifted[0] > 0.5 && 0.5 > lifted[1]);
    }

    #[test]
    fn wheels_follow_the_same_formula_per_channel() {
        let wheels = [(0.2, 0.1), (-0.3, 0.4), (0.1, -0.5), (0.5, 0.0)];
        let shift = |i: usize, reach: f32| wheel_shift(wheels[i].0, wheels[i].1).map(|v| v * reach);
        let (lift, gamma, gain, offset) = (shift(0, 0.2), shift(1, 0.5), shift(2, 0.4), shift(3, 0.2));
        check(&Grade { wheels, ..Default::default() }, |x| [0, 1, 2].map(|c| {
            let y = x[c] + offset[c];
            let y = (1.0 + gain[c]) * y + lift[c] * (1.0 - y);
            y.max(0.0).powf(2f32.powf(-gamma[c])).clamp(0.0, 1.0)
        }), 1e-5);
    }

    #[test]
    fn contrast_turns_around_the_pivot() {
        let grade = Grade { contrast: 1.5, pivot: 0.4, ..Default::default() };
        check(&grade, |x| x.map(|v| ((v - 0.4) * 1.5 + 0.4).clamp(0.0, 1.0)), 1e-5);
        assert!((grade.eval([0.4; 3])[0] - 0.4).abs() < 1e-6);
        assert!(Grade { contrast: 0.5, ..Default::default() }.per_channel());
    }

    #[test]
    fn saturation_mixes_with_rec709_luma() {
        check(&Grade { saturation: 1.6, ..Default::default() }, |x| x.map(|v| (lum(x) + 1.6 * (v - lum(x))).clamp(0.0, 1.0)), 1e-5);
        check(&Grade { saturation: 0.0, ..Default::default() }, |x| [lum(x); 3], 1e-5);
        assert!(!Grade { saturation: 0.5, ..Default::default() }.per_channel());
    }

    #[test]
    fn vibrance_is_neutral_at_zero_and_leaves_greys_alone() {
        let neutral = Grade::default();
        let vibrant = Grade { vibrance: 0.0, ..Default::default() };
        assert!(vibrant.is_identity() && vibrant.per_channel());
        for x in colours() {
            assert_eq!(vibrant.eval(x), neutral.eval(x));
        }
        for v in [-100.0, -40.0, 60.0, 100.0] {
            let g = Grade { vibrance: v, ..Default::default() };
            assert!(!g.is_identity() && !g.per_channel());
            for l in [0.0, 0.2, 0.5, 0.9, 1.0] {
                assert!(g.eval([l; 3]).iter().all(|c| (c - l).abs() < 1e-6));
            }
        }
    }

    #[test]
    fn vibrance_boosts_dull_colours_more_than_strong_ones() {
        // blue-green hues, away from skin
        let (dull, strong) = ([0.45, 0.5, 0.55], [0.1, 0.4, 0.9]);
        let grade = Grade { vibrance: 80.0, ..Default::default() };
        let chroma = |x: [f32; 3]| x[0].max(x[1]).max(x[2]) - x[0].min(x[1]).min(x[2]);
        let (d, s) = (chroma(grade.eval(dull)) / chroma(dull), chroma(grade.eval(strong)) / chroma(strong));
        assert!(d > 1.5 && d > s && s > 1.0, "dull {d}, strong {s}");
        let cut = Grade { vibrance: -80.0, ..Default::default() };
        let (d, s) = (chroma(cut.eval(dull)) / chroma(dull), chroma(cut.eval(strong)) / chroma(strong));
        assert!(s < d && d < 1.0, "dull {d}, strong {s}");
    }

    #[test]
    fn vibrance_holds_back_on_skin_hues() {
        let grade = Grade { vibrance: 100.0, ..Default::default() };
        let chroma = |x: [f32; 3]| x[0].max(x[1]).max(x[2]) - x[0].min(x[1]).min(x[2]);
        // same saturation, one skin orange and one teal
        let (skin, teal) = ([0.6, 0.5, 0.4], [0.4, 0.6, 0.5]);
        assert!((hue_of(skin) - SKIN_HUE).abs() < 0.03);
        let (a, b) = (chroma(grade.eval(skin)) / chroma(skin), chroma(grade.eval(teal)) / chroma(teal));
        assert!(a < 1.0 + 0.5 * (b - 1.0), "skin {a}, teal {b}");
        assert!(a > 1.0);
    }

    #[test]
    fn shadows_and_highlights_move_the_tones_they_name() {
        let (sh, hi) = (60.0f32, -40.0f32);
        let grade = Grade { shadows: sh as f64, highlights: hi as f64, ..Default::default() };
        check(&grade, |x| { let y = lum(x); x.map(|v| (v + sh / 100.0 * y * (1.0 - y) * (1.0 - y) + hi / 100.0 * y * y * (1.0 - y)).clamp(0.0, 1.0)) }, 1e-5);
        for (s, h) in [(100.0, 100.0), (-100.0, -100.0), (100.0, -100.0), (-100.0, 100.0)] {
            let g = Grade { shadows: s, highlights: h, ..Default::default() };
            let ramp: Vec<f32> = (0..=100).map(|i| g.eval([i as f32 / 100.0; 3])[0]).collect();
            assert!(ramp.windows(2).all(|w| w[1] - w[0] >= -1e-6), "never turns tones around");
        }
        let g = Grade { shadows: 50.0, ..Default::default() };
        assert!(g.eval([0.2; 3])[0] > 0.2 + 0.05 && g.eval([0.9; 3])[0] < 0.9 + 0.02);
    }

    #[test]
    fn curves_go_through_their_points_without_overshooting() {
        let pts = [(0.0, 0.0), (0.25, 0.4), (0.5, 0.5), (0.75, 0.95), (1.0, 1.0)];
        for (px, py) in pts {
            assert!((monotone_curve(&pts, px) - py as f32).abs() < 1e-6);
        }
        let ys: Vec<f32> = (0..=400).map(|i| monotone_curve(&pts, i as f64 / 400.0)).collect();
        assert!(ys.windows(2).all(|w| w[1] - w[0] >= -1e-7) && ys.iter().all(|y| (0.0..=1.0).contains(y)));
        assert!((0..=400).all(|i| (monotone_curve(&LINEAR, i as f64 / 400.0) - i as f32 / 400.0).abs() < 1e-6));
    }

    #[test]
    fn the_master_curve_comes_before_each_channels_own() {
        let master = [(0.0, 0.0), (0.5, 0.7), (1.0, 1.0)];
        let red = [(0.0, 0.1), (1.0, 0.9)];
        let mut curves: [Vec<(f64, f64)>; 4] = std::array::from_fn(|_| LINEAR.to_vec());
        curves[0] = master.to_vec();
        curves[1] = red.to_vec();
        let grade = Grade { curves, ..Default::default() };
        check(&grade, |x| {
            let m = x.map(|v| monotone_curve(&master, v as f64));
            [monotone_curve(&red, m[0] as f64), m[1], m[2]]
        }, 1e-6);
        assert!(grade.per_channel());
    }

    #[test]
    fn hue_curves_go_round() {
        let pts = [(0.0, 0.8), (0.5, 0.5)];
        let (a, b, c, d) = (periodic_curve(&pts, 0.0), periodic_curve(&pts, 0.05), periodic_curve(&pts, 0.95), periodic_curve(&pts, 1.0));
        assert!((a - 0.8).abs() < 1e-6 && (d - 0.8).abs() < 1e-6 && (b - c).abs() < 1e-5);
        assert!([0.0, 0.05, 0.95, 1.0].iter().all(|&x| (periodic_curve(&neutral_hue(), x) - 0.5).abs() < 1e-6));
    }

    #[test]
    fn a_hue_curve_given_one_hue_twice_has_no_hard_edge() {
        let neutral = neutral_hue();
        let mut at_one = vec![neutral[0]];
        at_one.push((1.0, 0.7));
        at_one.extend(&neutral[1..]);
        let mut twice = neutral.clone();
        twice.push((1.0 / 6.0, 0.7));
        for pts in [at_one, twice] {
            let ys: Vec<f32> = (0..=2000).map(|i| periodic_curve(&pts, i as f64 / 2000.0)).collect();
            assert!(ys.iter().all(|y| y.is_finite()) && ys.windows(2).all(|w| (w[1] - w[0]).abs() < 0.01));
        }
    }

    #[test]
    fn hue_vs_sat_takes_the_colour_out_of_one_hue_only() {
        let pts: Vec<(f64, f64)> = neutral_hue().into_iter().map(|(h, _)| (h, if (h - 1.0 / 3.0).abs() < 1e-6 { 0.0 } else { 0.5 })).collect();
        let grade = Grade { hue_sat: pts, ..Default::default() };
        let green = grade.eval([0.2, 0.8, 0.2]);
        let want = lum([0.2, 0.8, 0.2]);
        assert!(green.iter().all(|v| (v - want).abs() < 1e-5));
        let red = grade.eval([0.8, 0.2, 0.2]);
        assert!((0..3).all(|c| (red[c] - [0.8, 0.2, 0.2][c]).abs() < 1e-5));
        assert!(!grade.per_channel());
    }

    #[test]
    fn hue_vs_hue_turns_one_hue_towards_the_next() {
        let turned = rotate_hue([1.0, 0.0, 0.0], 1.0 / 3.0);
        assert!(turned.iter().zip([0.0, 1.0, 0.0]).all(|(a, b)| (a - b).abs() < 1e-5));
        let pts: Vec<(f64, f64)> = neutral_hue().into_iter().map(|(h, y)| (h, if h == 0.0 { 0.5 + 1.0 / 12.0 } else { y })).collect();
        let grade = Grade { hue_hue: pts, ..Default::default() };
        let red = grade.eval([0.9, 0.1, 0.1]);
        let turns = hue_of(red) * 12.0;
        assert!(0.2 < turns && turns < 1.8, "towards orange: {turns}");
        let cyan = grade.eval([0.1, 0.9, 0.9]);
        assert!((0..3).all(|c| (cyan[c] - [0.1, 0.9, 0.9][c]).abs() < 1e-5));
    }

    fn invert() -> Arc<CubeLut> {
        Arc::new(CubeLut::from_cube(lattice(5).iter().map(|v| 1.0 - v).collect(), 5))
    }

    #[test]
    fn a_lut_file_goes_last_and_strength_mixes_it_in() {
        let lut = invert();
        check(&Grade { lut: Some(lut.clone()), ..Default::default() }, |x| x.map(|v| 1.0 - v), 1e-5);
        check(&Grade { lut: Some(lut.clone()), lut_strength: 0.25, ..Default::default() }, |x| x.map(|v| v + 0.25 * ((1.0 - v) - v)), 1e-5);
        check(&Grade { contrast: 2.0, lut: Some(lut.clone()), ..Default::default() }, |x| x.map(|v| 1.0 - ((v - PIVOT as f32) * 2.0 + PIVOT as f32).clamp(0.0, 1.0)), 1e-5);
        assert!(Grade { lut: Some(lut.clone()), lut_strength: 0.0, ..Default::default() }.is_identity());
        assert!(!Grade { lut: Some(lut), ..Default::default() }.per_channel());
        let shaper = Arc::new(CubeLut::from_shaper(vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0]));
        assert!(Grade { lut: Some(shaper), ..Default::default() }.per_channel());
    }

    #[test]
    fn a_detailed_lut_asks_for_a_bigger_bake() {
        assert_eq!(Grade::default().lut_size(), 33);
        let big = Arc::new(CubeLut::from_cube(lattice(65), 65));
        assert_eq!(Grade { lut: Some(big), ..Default::default() }.lut_size(), 65);
    }

    #[test]
    fn everything_together_is_each_step_in_turn() {
        let gain = masters(0.0, 0.0, 1.2, 0.0);
        let full = Grade { temperature: 20.0, masters: gain, contrast: 1.3, saturation: 1.2, ..Default::default() };
        let (a, b, c, d) = (
            Grade { temperature: 20.0, ..Default::default() },
            Grade { masters: gain, ..Default::default() },
            Grade { contrast: 1.3, ..Default::default() },
            Grade { saturation: 1.2, ..Default::default() },
        );
        check(&full, |x| d.eval(c.eval(b.eval(a.eval(x)))), 1e-5);
    }
}
