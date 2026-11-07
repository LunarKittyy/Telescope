//! A generated webcam scene for Auto colour grading's tests: a wall, a window, a person with a face, and landmarks and a mask to go with them. No picture files.

use crate::auto_measure::{Look, Measurement, Measurer, Px};
use crate::cpu::Rgb;
use crate::grade::Grade;
use crate::imgops::Gray;
use telescope_core::pose::Person;
use telescope_core::view::Geometry;

pub const W: usize = 320;
pub const H: usize = 180;

pub const PALE: Px = [0.88, 0.70, 0.60];
pub const MID: Px = [0.76, 0.57, 0.45];
pub const DARK: Px = [0.45, 0.30, 0.22];

#[derive(Clone, Debug)]
pub struct Scene {
    pub skin: Px,
    pub wall: Px,
    /// The right half of the wall, for light that isn't the same across the room.
    pub wall_right: Px,
    pub shirt: Px,
    pub window: bool,
    pub person: bool,
}

impl Default for Scene {
    fn default() -> Scene {
        Scene { skin: MID, wall: [0.64, 0.64, 0.62], wall_right: [0.64, 0.64, 0.62], shirt: [0.20, 0.30, 0.55], window: false, person: true }
    }
}

fn hash(x: usize, y: usize, seed: u32) -> f32 {
    let mut v = (x as u32).wrapping_mul(73856093) ^ (y as u32).wrapping_mul(19349663) ^ seed.wrapping_mul(83492791);
    v ^= v >> 13;
    v = v.wrapping_mul(0x5bd1e995);
    v ^= v >> 15;
    (v & 0xffff) as f32 / 65535.0 - 0.5
}

const HEAD: (f32, f32, f32, f32) = (160.0, 65.0, 27.0, 29.0);

fn in_head(x: f32, y: f32) -> bool {
    ((x - HEAD.0) / HEAD.2).powi(2) + ((y - HEAD.1) / HEAD.3).powi(2) <= 1.0
}

fn is_person(x: usize, y: usize) -> bool {
    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
    in_head(fx, fy) || (fx > 147.0 && fx < 173.0 && fy > 90.0 && fy < 112.0) || (fx > 96.0 && fx < 224.0 && fy > 111.6)
}

impl Scene {
    /// The scene as encoded RGB triples 0..1, with a little noise that changes with `seed`.
    pub fn render(&self, seed: u32) -> Vec<f32> {
        let mut out = Vec::with_capacity(W * H * 3);
        for y in 0..H {
            for x in 0..W {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let shade = 1.0 - 0.10 * x as f32 / W as f32;
                let mut c: Px = if x < W / 2 { self.wall } else { self.wall_right }.map(|v| v * shade);
                // a lamp-lit patch of wall and a dark desk, so the picture has a real range of tones
                if fx > 8.0 && fx < 70.0 && fy > 8.0 && fy < 60.0 {
                    c = c.map(|v| (v * 1.35).min(0.96));
                }
                if fy > 150.0 && (fx < 96.0 || fx > 224.0) {
                    c = [0.06, 0.05, 0.05];
                }
                if self.window && fx > 250.0 && fx < 304.0 && fy > 9.0 && fy < 63.0 {
                    c = [1.0; 3];
                }
                if self.person {
                    if fx > 96.0 && fx < 224.0 && fy > 111.6 {
                        c = self.shirt;
                    }
                    if fx > 147.0 && fx < 173.0 && fy > 90.0 && fy < 112.0 {
                        c = self.skin.map(|v| v * 0.85);
                    }
                    if in_head(fx, fy) {
                        let side = 1.0 - 0.10 * (fx - HEAD.0) / HEAD.2;
                        c = self.skin.map(|v| v * side);
                        if fy < HEAD.1 - 0.55 * HEAD.3 {
                            c = [0.15, 0.10, 0.08];
                        }
                        for ex in [150.4f32, 169.6] {
                            if (fx - ex).hypot(fy - 57.6) < 3.0 {
                                c = [0.10, 0.08, 0.07];
                            }
                        }
                    }
                }
                let n = hash(x, y, seed) * 0.012;
                out.extend(c.map(|v| (v + n).clamp(0.0, 1.0)));
            }
        }
        out
    }

    /// The landmarks the vision service would give, as the picture shown at zoom 1.
    pub fn people(&self) -> Vec<Person> {
        if !self.person {
            return Vec::new();
        }
        vec![Person::new([
            ("left_eye", 0.47, 0.32, 1.0), ("right_eye", 0.53, 0.32, 1.0), ("nose", 0.5, 0.385, 1.0), ("left_ear", 0.43, 0.34, 1.0),
            ("right_ear", 0.57, 0.34, 1.0), ("left_shoulder", 0.30, 0.62, 1.0), ("right_shoulder", 0.70, 0.62, 1.0),
        ])]
    }

    /// The person mask, 255 where the person is.
    pub fn mask(&self) -> Gray {
        let mut m = Gray::new(W, H);
        if self.person {
            for y in 0..H {
                for x in 0..W {
                    if is_person(x, y) {
                        m.data[y * W + x] = 255;
                    }
                }
            }
        }
        m
    }

    /// Where the face is, as a test of what a picture did to it: the middle of the head.
    pub fn in_face(x: usize, y: usize) -> bool {
        ((x as f32 + 0.5 - 160.0) / 15.0).powi(2) + ((y as f32 + 0.5 - 67.0) / 20.0).powi(2) <= 1.0
    }
}

/// What a camera with this grade on it would show: the grade applied and rounded to 8 bits.
pub fn observed(pixels: &[f32], grade: &Grade) -> Rgb {
    let mut v = pixels.to_vec();
    grade.apply(&mut v);
    Rgb { w: W, h: H, data: v.iter().map(|c| (c * 255.0).round().clamp(0.0, 255.0) as u8).collect() }
}

/// What a camera with a white balance off by these linear-light gains would show.
pub fn cast(gains: [f32; 3]) -> Box<dyn Fn(&[f32]) -> Rgb> {
    Box::new(move |pixels| {
        let v: Vec<f32> = pixels.chunks(3).flat_map(|p| (0..3).map(|c| crate::grade::linear_to_srgb(crate::grade::srgb_to_linear(p[c]) * gains[c]).clamp(0.0, 1.0)).collect::<Vec<_>>()).collect();
        observed(&v, &Grade::default())
    })
}

/// What a camera with this grade on it would show.
pub fn graded(grade: Grade) -> Box<dyn Fn(&[f32]) -> Rgb> {
    Box::new(move |pixels| observed(pixels, &grade))
}

/// A second and a bit of frames of the scene as `camera` shows it, measured as the plugin's step would.
pub fn measure(scene: &Scene, camera: &dyn Fn(&[f32]) -> Rgb) -> Measurement {
    measure_from(scene, camera, 1)
}

/// The same, with the noise starting from another seed: the same scene a few seconds later.
pub fn measure_from(scene: &Scene, camera: &dyn Fn(&[f32]) -> Rgb, seed: u32) -> Measurement {
    let mut measurer = Measurer::new();
    let (people, view) = (scene.people(), Geometry::default());
    let mask = scene.mask().shrink(160, 90);
    for k in 0..16u32 {
        let small = camera(&scene.render(seed + k)).shrink(160, 90);
        let done = measurer.look(&Look { small: &small, mask: Some(&mask), people: &people, main: None, view: &view, noise: Some(0.004), time_ns: k as u64 * 100_000_000 });
        if done {
            break;
        }
    }
    measurer.finish()
}

/// The mean absolute difference per channel between two pictures, over the pixels `keep` lets through.
pub fn difference(a: &Rgb, b: &Rgb, keep: impl Fn(usize, usize) -> bool) -> f32 {
    let (mut sum, mut n) = (0.0f32, 0usize);
    for y in 0..H {
        for x in 0..W {
            if keep(x, y) {
                for c in 0..3 {
                    sum += (a.data[(y * W + x) * 3 + c] as f32 - b.data[(y * W + x) * 3 + c] as f32).abs() / 255.0;
                }
                n += 3;
            }
        }
    }
    sum / n.max(1) as f32
}
