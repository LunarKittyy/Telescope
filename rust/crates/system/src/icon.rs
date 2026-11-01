//! The app's mark, drawn: widgets/common.py's create_app_icon (an accent disc with a dark centre) and _tray_pixmap (that disc with
//! what's streaming on it). Everything that shows the icon comes from here, in straight RGBA rows.

/// theme.ACCENT, theme.BG and theme.ERR.
const ACCENT: [f32; 3] = [0xaa as f32, 0x9c as f32, 0xf5 as f32];
const DARK: [f32; 3] = [0x0f as f32, 0x12 as f32, 0x16 as f32];
const RED: [f32; 3] = [0xef as f32, 0x6f as f32, 0x6b as f32];

/// Samples along a pixel's side, for smooth edges.
const SAMPLES: usize = 8;

/// widgets.common.TRAY_SIZES: the tray gets each of these drawn, not scaled, so the badges stay crisp.
pub const TRAY_SIZES: [u32; 7] = [16, 20, 22, 24, 32, 48, 64];

/// What the tray icon says is streaming.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrayShows {
    pub camera: bool,
    pub mic: Mic,
}

impl TrayShows {
    /// mic: whether it streams, as Some(muted).
    pub fn from_stream(camera: bool, mic: Option<bool>) -> TrayShows {
        TrayShows { camera, mic: mic.map_or(Mic::Off, |muted| if muted { Mic::Muted } else { Mic::On }) }
    }
}

/// The mic on the tray icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mic {
    #[default]
    Off,
    On,
    Muted,
}

/// A picture being painted: premultiplied red, green, blue and alpha per pixel, 0 to 1.
struct Canvas {
    size: usize,
    pixels: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(size: u32) -> Canvas {
        let size = size as usize;
        Canvas { size, pixels: vec![[0.0; 4]; size * size] }
    }

    /// How much of each pixel `inside` covers, with what `blend` does with it.
    fn each(&mut self, inside: &impl Fn(f32, f32) -> bool, blend: impl Fn(&mut [f32; 4], f32)) {
        for y in 0..self.size {
            for x in 0..self.size {
                let mut hits = 0;
                for sy in 0..SAMPLES {
                    for sx in 0..SAMPLES {
                        let at = |p: usize, s: usize| p as f32 + (s as f32 + 0.5) / SAMPLES as f32;
                        hits += usize::from(inside(at(x, sx), at(y, sy)));
                    }
                }
                if hits > 0 {
                    blend(&mut self.pixels[y * self.size + x], hits as f32 / (SAMPLES * SAMPLES) as f32);
                }
            }
        }
    }

    /// CompositionMode_SourceOver.
    fn paint(&mut self, inside: impl Fn(f32, f32) -> bool, colour: [f32; 3]) {
        self.each(&inside, |p, cover| {
            for c in 0..3 {
                p[c] = p[c] * (1.0 - cover) + colour[c] / 255.0 * cover;
            }
            p[3] = p[3] * (1.0 - cover) + cover;
        });
    }

    /// CompositionMode_Clear.
    fn clear(&mut self, inside: impl Fn(f32, f32) -> bool) {
        self.each(&inside, |p, cover| p.iter_mut().for_each(|v| *v *= 1.0 - cover));
    }

    fn rgba(&self) -> Vec<u8> {
        let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
        self.pixels.iter().flat_map(|&[r, g, b, a]| {
            let straight = |v: f32| if a > 0.0 { byte(v / a) } else { 0 };
            [straight(r), straight(g), straight(b), byte(a)]
        }).collect()
    }
}

fn disc(cx: f32, cy: f32, r: f32) -> impl Fn(f32, f32) -> bool {
    move |x, y| (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

/// A stroke with round ends from one point to another, or anything with that shape.
fn line(from: (f32, f32), to: (f32, f32), r: f32) -> impl Fn(f32, f32) -> bool {
    move |x, y| {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let t = (((x - from.0) * dx + (y - from.1) * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
        (x - from.0 - t * dx).powi(2) + (y - from.1 - t * dy).powi(2) <= r * r
    }
}

fn mark(size: u32) -> Canvas {
    let s = size as f32;
    let mut canvas = Canvas::new(size);
    canvas.paint(disc(s / 2.0, s / 2.0, (s - 2.0 * s / 22.0) / 2.0), ACCENT);
    canvas.paint(disc(s / 2.0, s / 2.0, s * 8.0 / 22.0 / 2.0), DARK);
    canvas
}

/// create_app_icon: straight RGBA rows.
pub fn app_icon_rgba(size: u32) -> Vec<u8> {
    mark(size).rgba()
}

/// _tray_pixmap: the mark at this size with what shows on it, straight RGBA rows.
pub fn tray_rgba(size: u32, shows: TrayShows) -> Vec<u8> {
    let s = size as f32;
    let mut canvas = mark(size);
    let gap = (s / 16.0).max(1.0);
    if shows.camera {
        // The red dot of a recording light, top right, cut clear of the ring so it reads on any panel
        let d = s * 0.36;
        let (cx, cy) = (s - d / 2.0 - gap / 2.0, d / 2.0 + gap / 2.0);
        canvas.clear(disc(cx, cy, d / 2.0 + gap));
        canvas.paint(disc(cx, cy, d / 2.0), RED);
    }
    if shows.mic != Mic::Off {
        // A dark mic sitting wholly on the ring, bottom right, so the panel never shows through it
        let u = s * 0.44 / 12.0;
        let (cx, cy) = (s * 0.73, s * 0.72);
        canvas.paint(line((cx, cy - 2.5 * u), (cx, cy - 0.5 * u), 1.5 * u), DARK);
        let half = (1.1 * u).max(1.0) / 2.0;
        // The lower half of a circle under the capsule, with round ends
        let (ax, ay, ar) = (cx, cy - 0.5 * u, 3.0 * u);
        canvas.paint(move |x, y| {
            let round = |px: f32, py: f32| (x - px).powi(2) + (y - py).powi(2) <= half * half;
            let along = ((x - ax).powi(2) + (y - ay).powi(2)).sqrt();
            (y >= ay && (along - ar).abs() <= half) || round(ax - ar, ay) || round(ax + ar, ay)
        }, DARK);
        canvas.paint(line((cx, cy + 2.5 * u), (cx, cy + 4.0 * u), half), DARK);
        if shows.mic == Mic::Muted {
            canvas.paint(line((cx - 3.5 * u, cy - 3.5 * u), (cx + 3.5 * u, cy + 3.5 * u), half), RED);
        }
    }
    canvas.rgba()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(rgba: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        rgba[((y * size + x) * 4) as usize..][..4].try_into().unwrap()
    }

    #[test]
    fn the_mark_is_an_accent_ring_around_a_dark_centre() {
        let icon = app_icon_rgba(64);
        assert_eq!(pixel(&icon, 64, 32, 32), [0x0f, 0x12, 0x16, 255]);
        assert_eq!(pixel(&icon, 64, 32, 8), [0xaa, 0x9c, 0xf5, 255]);
        assert_eq!(pixel(&icon, 64, 0, 0)[3], 0);
    }

    #[test]
    fn the_camera_dot_is_red_and_cut_clear_of_the_ring() {
        let on = TrayShows { camera: true, mic: Mic::Off };
        let icon = tray_rgba(64, on);
        let d = 64.0 * 0.36;
        let (cx, cy) = (64.0 - d / 2.0 - 2.0, d / 2.0 + 2.0);
        assert_eq!(pixel(&icon, 64, cx as u32, cy as u32), [0xef, 0x6f, 0x6b, 255]);
        assert_eq!(pixel(&icon, 64, (cx - d / 2.0 - 2.0) as u32, cy as u32)[3], 0, "the gap around it shows the panel");
        assert_ne!(icon, app_icon_rgba(64));
    }

    #[test]
    fn a_muted_mic_differs_from_one_thats_on() {
        for size in TRAY_SIZES {
            let on = tray_rgba(size, TrayShows { camera: false, mic: Mic::On });
            let muted = tray_rgba(size, TrayShows { camera: false, mic: Mic::Muted });
            assert_ne!(on, muted, "{size}");
            assert_ne!(on, app_icon_rgba(size), "{size}");
        }
    }
}
