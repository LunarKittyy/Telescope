//! Merging a bracket into one picture: a focus bracket into a picture that is sharp all the way through, an exposure bracket into one that has
//! detail in both the dark and the bright parts (exposure fusion).
//!
//! The photos are lined up with the middle one, which also covers focus breathing (the picture grows a little as the focus moves nearer): the
//! shift between two photos is measured by phase correlation in five patches (core's `shift_between`, which Stargazer lines frames up with)
//! and a scale and a shift are fitted to them, then nudged until the coarse pictures differ least. Each photo is then weighed pixel by pixel on a
//! picture about 1000 pixels wide: for focus by how sharp it is there, for exposure by contrast, colour and how well exposed it is (Mertens,
//! Kautz and Van Reeth). Where a photo disagrees with the middle one (something moved) its weight goes to the middle one. The photos are blended
//! as Laplacian pyramids, so the join between the photos that win different parts is soft at every scale. The pictures are done in strips of rows,
//! and the photos are read twice (once to judge them, once to blend them), so one photo at a time is in memory besides the result.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use telescope_core::host::Poster;
use telescope_core::stack::{shift_between, Plane};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// What a focus bracket saves.
pub const FRAMES: &str = "frames";
pub const BOTH: &str = "both";
pub const MERGED: &str = "merged";

/// The picture the photos are judged and lined up on is at most this wide.
const WORK_WIDTH: usize = 1024;
/// A patch the shift is measured on, at most this big (a power of two) and at least this
const MAX_PATCH: usize = 256;
const MIN_PATCH: usize = 64;
/// A patch whose correlation peak is weaker than this is left out of the fit
const MIN_CONFIDENCE: f32 = 6.0;
/// Patches agree when the fit is within this many working pixels of each
const MAX_RESIDUAL: f64 = 2.0;
/// The picture may be scaled this much between two photos: more is no focus breathing
const MAX_BREATHING: f64 = 0.08;
/// How far (working pixels) the average and spread that brightness and contrast are evened out against reach
const LOCAL: usize = 12;
/// Two pictures lined up are only taken as matching when their correlation in every quarter that can be compared is at least this high
const MIN_AGREE: f64 = 0.45;
/// The mean square of a picture's (evened out) values must be at least this for it to have a say in whether the pictures match
const MIN_ENERGY: f64 = 0.02;
/// The fine detail is blurred away over this many pixels either way (twice) before photos are compared for how they sit
const ALIGN_BLUR: usize = 8;
/// Exposure fusion's well exposed target (the middle of the range) and how far the darker to lighter setting moves it, and how wide "well exposed" is
const EXPOSURE_TARGET: f32 = 0.5;
const EXPOSURE_REACH: f32 = 0.3;
const WELL_SIGMA: f32 = 0.2;
/// Luma this close to black or white is clipped: it says nothing about the scene
const CLIPPED: f32 = 0.04;
/// What moved is taken as this many working pixels bigger than where it was seen
const GHOST_GROW: usize = 4;
/// The sharpness is smoothed over this many working pixels either way: little, since the pyramid blend softens the join and a wide blur would let a sharp foreground edge win the background beside it
const SMOOTH: usize = 2;
/// How much sharper one photo has to be for it to win a pixel: the weight is the sharpness to this power
const FAVOUR: i32 = 4;
/// Photos that can't be merged: fewer than this many, or more than the phone's longest bracket
pub const MIN_FRAMES: usize = 2;

/// A picture to merge, R, G, B a pixel.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

/// Where a pixel of the result comes from in a photo: `from = centre + scale * (here - centre) + shift`, in pixels of the photo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Warp {
    pub scale: f64,
    pub shift: (f64, f64),
}

impl Warp {
    const NONE: Warp = Warp { scale: 1.0, shift: (0.0, 0.0) };

    fn at(&self, centre: (f64, f64), x: f64, y: f64) -> (f64, f64) {
        (centre.0 + self.scale * (x - centre.0) + self.shift.0, centre.1 + self.scale * (y - centre.1) + self.shift.1)
    }
}

/// The luma of an image, 0..1, shrunk by a box filter by an integer factor.
#[cfg(test)]
fn luma(image: &Image, factor: usize) -> Option<Plane> {
    let (w, h) = (image.width / factor, image.height / factor);
    if w == 0 || h == 0 || image.rgb.len() < image.width * image.height * 3 {
        return None;
    }
    let mut data = vec![0.0f32; w * h];
    let scale = 1.0 / (255.0 * (factor * factor) as f32);
    for (oy, row) in data.chunks_exact_mut(w).enumerate() {
        for (ox, out) in row.iter_mut().enumerate() {
            let mut sum = 0.0;
            for y in oy * factor..oy * factor + factor {
                for x in ox * factor..ox * factor + factor {
                    let i = (y * image.width + x) * 3;
                    sum += 0.2126 * image.rgb[i] as f32 + 0.7152 * image.rgb[i + 1] as f32 + 0.0722 * image.rgb[i + 2] as f32;
                }
            }
            *out = sum * scale;
        }
    }
    Some(Plane { width: w, height: h, data })
}

fn patch(plane: &Plane, size: usize, cx: usize, cy: usize) -> Plane {
    let (x0, y0) = (cx.saturating_sub(size / 2).min(plane.width - size), cy.saturating_sub(size / 2).min(plane.height - size));
    let mut data = Vec::with_capacity(size * size);
    for y in y0..y0 + size {
        data.extend_from_slice(&plane.data[y * plane.width + x0..][..size]);
    }
    Plane { width: size, height: size, data }
}

/// The plane with its fine detail blurred away, and its brightness and contrast evened out where it is (each pixel against the average and spread around it): photos
/// of one scene at different focus differ most in the fine detail, photos at different exposure in how bright and contrasty a part of the picture is, and it is
/// the coarse structure they share. Pixels that are clipped to black or white say nothing and are NaN.
fn coarse(plane: &Plane) -> Plane {
    let (w, h) = (plane.width, plane.height);
    let mut data = plane.data.clone();
    blur(&mut data, w, h, ALIGN_BLUR);
    blur(&mut data, w, h, ALIGN_BLUR);
    let mut mean = data.clone();
    blur(&mut mean, w, h, LOCAL);
    let mut spread: Vec<f32> = data.iter().zip(&mean).map(|(v, m)| (v - m) * (v - m)).collect();
    blur(&mut spread, w, h, LOCAL);
    for i in 0..w * h {
        data[i] = if (CLIPPED..=1.0 - CLIPPED).contains(&plane.data[i]) { (data[i] - mean[i]) / (spread[i].sqrt() + 0.02) } else { f32::NAN };
    }
    Plane { width: w, height: h, data }
}

/// `plane` with the NaNs (clipped pixels) as zeros, for the phase correlation.
fn filled(plane: &Plane) -> Plane {
    Plane { width: plane.width, height: plane.height, data: plane.data.iter().map(|v| if v.is_nan() { 0.0 } else { *v }).collect() }
}

/// How the picture of `moved` sits against `reference` (planes of one size): a scale about their middle and a shift. Phase correlation in five patches
/// (what Stargazer lines frames up with) gives the first answer, to the pixel or so, when it can find one. Then the coarse pictures are laid over each
/// other and the answer is nudged until they differ least, which gets the last fraction of a pixel and also works where the patches saw too little.
pub fn estimate(reference: &Plane, moved: &Plane) -> Option<Warp> {
    let (reference, moved) = (coarse(reference), coarse(moved));
    let start = estimate_once(&reference, &moved).unwrap_or(Warp::NONE);
    let (half_ref, half_moved) = (halve(&reference), halve(&moved));
    // at half size a shift is half as many pixels
    let at_half = Warp { scale: start.scale, shift: (start.shift.0 / 2.0, start.shift.1 / 2.0) };
    let rough = nudge(&half_ref, &half_moved, at_half, 20)?;
    let rough = Warp { scale: rough.scale, shift: (rough.shift.0 * 2.0, rough.shift.1 * 2.0) };
    let fine = nudge(&reference, &moved, rough, 8)?;
    let far = reference.width.min(reference.height) as f64 / 4.0;
    ((fine.scale - 1.0).abs() <= MAX_BREATHING && fine.shift.0.hypot(fine.shift.1) <= far && agrees(&reference, &moved, &fine)).then_some(fine)
}

/// Whether the two pictures, laid over each other by the warp, match everywhere there is something to compare and not only in one part (a turn of the camera can
/// be put right in the middle and left wrong at the sides): the correlation of the two is checked in each quarter. A quarter that is mostly clipped in one of the
/// pictures, as the bright half of the middle photo of an exposure bracket can be, says nothing either way, but two quarters have to.
fn agrees(reference: &Plane, moved: &Plane, warp: &Warp) -> bool {
    let (w, h) = (reference.width, reference.height);
    let centre = (w as f64 / 2.0, h as f64 / 2.0);
    let (mut said, mut bad) = (0, 0);
    for q in 0..4 {
        let (x0, y0) = ((q % 2) * w / 2, (q / 2) * h / 2);
        let (mut ab, mut aa, mut bb, mut n) = (0.0f64, 0.0f64, 0.0f64, 0usize);
        for y in (y0..y0 + h / 2).step_by(2) {
            for x in (x0..x0 + w / 2).step_by(2) {
                let (sx, sy) = warp.at(centre, x as f64, y as f64);
                if sx < 0.0 || sy < 0.0 || sx > (w - 1) as f64 || sy > (h - 1) as f64 {
                    continue;
                }
                let (a, b) = (reference.data[y * w + x], bilinear(moved, sx, sy));
                if a.is_nan() || b.is_nan() {
                    continue;
                }
                (ab, aa, bb, n) = (ab + (a * b) as f64, aa + (a * a) as f64, bb + (b * b) as f64, n + 1);
            }
        }
        // a quarter of flat grey sky has nothing in it to line up on: no vote either way
        if n * 4 >= (w / 2) * (h / 2) / 4 && aa / n as f64 >= MIN_ENERGY && bb / n as f64 >= MIN_ENERGY {
            said += 1;
            if ab / (aa * bb).sqrt().max(1e-9) < MIN_AGREE {
                bad += 1;
            }
        }
    }
    said >= 2 && bad == 0
}

/// The plane at half the size, by averaging 2 x 2.
fn halve(plane: &Plane) -> Plane {
    let (w, h) = (plane.width / 2, plane.height / 2);
    let mut data = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let at = |dx: usize, dy: usize| plane.data[(y * 2 + dy) * plane.width + x * 2 + dx];
            data.push((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4.0);
        }
    }
    Plane { width: w, height: h, data }
}

/// `start` made better by Gauss-Newton steps on the squared difference between `reference` and `moved` seen through the warp: the scale and shift change
/// together until the two pictures differ least. None when there is no picture to go by (a flat plane has no gradient to follow).
fn nudge(reference: &Plane, moved: &Plane, start: Warp, steps: usize) -> Option<Warp> {
    let (w, h) = (reference.width, reference.height);
    let centre = (w as f64 / 2.0, h as f64 / 2.0);
    let margin = (w.min(h) / 8).max(4);
    let mut warp = start;
    for _ in 0..steps {
        // Jᵀ J (upper triangle of the 3 x 3: scale, x, y) and Jᵀ e
        let (mut a, mut b) = ([[0.0f64; 3]; 3], [0.0f64; 3]);
        for y in margin..h - margin {
            for x in margin..w - margin {
                let (u, v) = (x as f64 - centre.0, y as f64 - centre.1);
                let (sx, sy) = warp.at(centre, x as f64, y as f64);
                if sx < 2.0 || sy < 2.0 || sx > (w - 3) as f64 || sy > (h - 3) as f64 {
                    continue;
                }
                let raw = bilinear(moved, sx, sy);
                let at = reference.data[y * w + x];
                if raw.is_nan() || at.is_nan() {
                    continue;
                }
                let (gx, gy) = (((bilinear(moved, sx + 1.0, sy) - bilinear(moved, sx - 1.0, sy)) / 2.0) as f64, ((bilinear(moved, sx, sy + 1.0) - bilinear(moved, sx, sy - 1.0)) / 2.0) as f64);
                if !gx.is_finite() || !gy.is_finite() {
                    continue;
                }
                let jac = [gx * u + gy * v, gx, gy];
                let e = (raw - at) as f64;
                // a pixel that is far from agreeing (something moved) counts for less
                let weight = if e.abs() < 1.0 { 1.0 } else { 1.0 / e.abs() };
                for i in 0..3 {
                    b[i] += weight * jac[i] * e;
                    for j in i..3 {
                        a[i][j] += weight * jac[i] * jac[j];
                    }
                }
            }
        }
        let trace = a[0][0] + a[1][1] + a[2][2];
        if trace < 1e-9 {
            return None;
        }
        // a little toward no change, so a nearly flat picture doesn't throw the answer about
        for (i, row) in a.iter_mut().enumerate() {
            row[i] += trace * 1e-6;
        }
        let step = solve(&a, &b)?;
        warp = Warp { scale: warp.scale - step[0], shift: (warp.shift.0 - step[1], warp.shift.1 - step[2]) };
        if step[0].abs() < 1e-6 && step[1].abs() < 0.01 && step[2].abs() < 0.01 {
            break;
        }
    }
    warp.scale.is_finite().then_some(warp)
}

/// a x = b for a symmetric 3 x 3 given by its upper triangle, by elimination.
fn solve(a: &[[f64; 3]; 3], b: &[f64; 3]) -> Option<[f64; 3]> {
    let mut m = [[0.0f64; 4]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = if j >= i { a[i][j] } else { a[j][i] };
        }
        m[i][3] = b[i];
    }
    for col in 0..3 {
        let pivot = (col..3).max_by(|&x, &y| m[x][col].abs().total_cmp(&m[y][col].abs()))?;
        if m[pivot][col].abs() < 1e-18 {
            return None;
        }
        m.swap(col, pivot);
        for row in 0..3 {
            if row != col {
                let f = m[row][col] / m[col][col];
                let pivot_row = m[col];
                for (k, v) in m[row].iter_mut().enumerate().skip(col) {
                    *v -= f * pivot_row[k];
                }
            }
        }
    }
    Some([m[0][3] / m[0][0], m[1][3] / m[1][1], m[2][3] / m[2][2]])
}

/// One measuring of the scale and shift, fitted to the shifts of five patches. None when too few patches show a clear match, or the scale is more than breathing explains.
fn estimate_once(reference: &Plane, moved: &Plane) -> Option<Warp> {
    let (w, h) = (reference.width, reference.height);
    if (w, h) != (moved.width, moved.height) {
        return None;
    }
    let small = w.min(h);
    if small < MIN_PATCH * 2 {
        return None;
    }
    let size = (1usize << (small / 2).ilog2()).clamp(MIN_PATCH, MAX_PATCH);
    let centre = (w as f64 / 2.0, h as f64 / 2.0);
    let spots = [(0.5, 0.5), (0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];
    let mut seen: Vec<((f64, f64), (f64, f64))> = Vec::new();
    for (fx, fy) in spots {
        let (cx, cy) = ((w as f64 * fx) as usize, (h as f64 * fy) as usize);
        let (a, b) = (patch(&filled(reference), size, cx, cy), patch(&filled(moved), size, cx, cy));
        let Some(shift) = shift_between(&a, &b).filter(|s| s.confidence >= MIN_CONFIDENCE) else { continue };
        // where the patch is, from the middle, and how far the photo is displaced there
        let (px, py) = (cx.saturating_sub(size / 2).min(w - size) as f64 + size as f64 / 2.0, cy.saturating_sub(size / 2).min(h - size) as f64 + size as f64 / 2.0);
        seen.push(((px - centre.0, py - centre.1), (shift.dx as f64, shift.dy as f64)));
    }
    if seen.len() < 3 {
        return None;
    }
    // some patches see little to go by and are wrong: the biggest group of them that agree is the answer
    let mut best: Option<(usize, f64, (f64, f64, f64))> = None;
    for subset in 1u32..(1 << seen.len()) {
        let group: Vec<_> = seen.iter().enumerate().filter(|(i, _)| subset >> i & 1 == 1).map(|(_, s)| *s).collect();
        if group.len() < 3 {
            continue;
        }
        let Some(fit) = fit_patches(&group) else { continue };
        let worst = group.iter().map(|(u, d)| (d.0 - (fit.0 * u.0 + fit.1)).hypot(d.1 - (fit.0 * u.1 + fit.2))).fold(0.0f64, f64::max);
        if worst <= MAX_RESIDUAL && best.is_none_or(|(n, w, _)| group.len() > n || (group.len() == n && worst < w)) {
            best = Some((group.len(), worst, fit));
        }
    }
    let (grow, tx, ty) = best?.2;
    if grow.abs() > MAX_BREATHING {
        return None;
    }
    Some(Warp { scale: 1.0 + grow, shift: (tx, ty) })
}

/// The growth and shift (at the middle) that fit the displacements best: d = grow * u + t, for patches at u from the middle.
fn fit_patches(seen: &[((f64, f64), (f64, f64))]) -> Option<(f64, f64, f64)> {
    let n = seen.len() as f64;
    let (mux, muy) = (seen.iter().map(|s| s.0 .0).sum::<f64>() / n, seen.iter().map(|s| s.0 .1).sum::<f64>() / n);
    let (mdx, mdy) = (seen.iter().map(|s| s.1 .0).sum::<f64>() / n, seen.iter().map(|s| s.1 .1).sum::<f64>() / n);
    let (num, den) = seen.iter().fold((0.0, 0.0), |(num, den), (u, d)| {
        let (ux, uy) = (u.0 - mux, u.1 - muy);
        (num + ux * (d.0 - mdx) + uy * (d.1 - mdy), den + ux * ux + uy * uy)
    });
    if den < 1.0 {
        return None;
    }
    let grow = num / den;
    Some((grow, mdx - grow * mux, mdy - grow * muy))
}

fn bilinear(plane: &Plane, x: f64, y: f64) -> f32 {
    let (w, h) = (plane.width, plane.height);
    let (x, y) = (x.clamp(0.0, (w - 1) as f64), y.clamp(0.0, (h - 1) as f64));
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let p = |xx: usize, yy: usize| plane.data[yy * w + xx];
    let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * fx;
    let bottom = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * fx;
    top + (bottom - top) * fy
}

/// A box blur with this radius, in both directions.
fn blur(data: &mut [f32], w: usize, h: usize, r: usize) {
    let mut line = vec![0.0f32; w.max(h)];
    for y in 0..h {
        let row = &mut data[y * w..(y + 1) * w];
        box_line(row, &mut line[..w], r);
    }
    let mut col = vec![0.0f32; h];
    for x in 0..w {
        for y in 0..h {
            col[y] = data[y * w + x];
        }
        box_line(&mut col, &mut line[..h], r);
        for y in 0..h {
            data[y * w + x] = col[y];
        }
    }
}

fn box_line(values: &mut [f32], scratch: &mut [f32], r: usize) {
    let n = values.len();
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for v in values.iter().take(r.min(n)) {
        sum += v;
        count += 1;
    }
    for i in 0..n {
        if i + r < n {
            sum += values[i + r];
            count += 1;
        }
        if i > r {
            sum -= values[i - r - 1];
            count -= 1;
        }
        scratch[i] = sum / count.max(1) as f32;
    }
    values.copy_from_slice(scratch);
}

/// What the photos are merged for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// A focus bracket: the sharpest photo wins at each point.
    Focus,
    /// An exposure bracket (exposure fusion): the best exposed, most contrasty and most colourful photo wins at each point. `brightness` is -100 (favour
    /// the darker photos) to 100 (the lighter ones), 0 for the middle.
    Exposure { brightness: i32 },
}

impl Kind {
    /// What the merged file's name carries.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Focus => "focus",
            Kind::Exposure { .. } => "exposure",
        }
    }
}

/// A photo as the working picture sees it: R, G, B in 0..1 and the luma, about 1000 pixels wide.
struct Small {
    width: usize,
    height: usize,
    rgb: Vec<f32>,
    luma: Plane,
}

fn small(image: &Image, factor: usize) -> Option<Small> {
    let (w, h) = (image.width / factor, image.height / factor);
    if w == 0 || h == 0 || image.rgb.len() < image.width * image.height * 3 {
        return None;
    }
    let mut rgb = vec![0.0f32; w * h * 3];
    let scale = 1.0 / (255.0 * (factor * factor) as f32);
    for (oy, row) in rgb.chunks_exact_mut(w * 3).enumerate() {
        for ox in 0..w {
            let mut sum = [0.0f32; 3];
            for y in oy * factor..oy * factor + factor {
                for x in ox * factor..ox * factor + factor {
                    let i = (y * image.width + x) * 3;
                    for (c, v) in sum.iter_mut().enumerate() {
                        *v += image.rgb[i + c] as f32;
                    }
                }
            }
            for c in 0..3 {
                row[ox * 3 + c] = sum[c] * scale;
            }
        }
    }
    let data = rgb.as_chunks::<3>().0.iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
    Some(Small { width: w, height: h, rgb, luma: Plane { width: w, height: h, data } })
}

/// The working picture as the result's picture sees it through the warp: its R, G, B and the pixels it reaches.
fn warped(small: &Small, warp: &Warp) -> (Vec<f32>, Vec<bool>) {
    let (w, h) = (small.width, small.height);
    let centre = (w as f64 / 2.0, h as f64 / 2.0);
    let mut rgb = vec![0.0f32; w * h * 3];
    let mut inside = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = warp.at(centre, x as f64, y as f64);
            inside[y * w + x] = sx >= 0.0 && sy >= 0.0 && sx <= (w - 1) as f64 && sy <= (h - 1) as f64;
            let (cx, cy) = (sx.clamp(0.0, (w - 1) as f64), sy.clamp(0.0, (h - 1) as f64));
            let (x0, y0) = (cx.floor() as usize, cy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (fx, fy) = ((cx - x0 as f64) as f32, (cy - y0 as f64) as f32);
            for c in 0..3 {
                let p = |xx: usize, yy: usize| small.rgb[(yy * w + xx) * 3 + c];
                let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * fx;
                let bottom = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * fx;
                rgb[(y * w + x) * 3 + c] = top + (bottom - top) * fy;
            }
        }
    }
    (rgb, inside)
}

fn luma_of(rgb: &[f32]) -> Vec<f32> {
    rgb.as_chunks::<3>().0.iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect()
}

/// The size of the Laplacian of a luma picture, with its edge rows and columns taking what is next to them.
fn contrast(luma: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut map = vec![0.0f32; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            map[i] = (luma[i - 1] + luma[i + 1] + luma[i - w] + luma[i + w] - 4.0 * luma[i]).abs();
        }
    }
    if w > 2 && h > 2 {
        for x in 0..w {
            (map[x], map[(h - 1) * w + x]) = (map[w + x], map[(h - 2) * w + x]);
        }
        for y in 0..h {
            (map[y * w], map[y * w + w - 1]) = (map[y * w + 1], map[y * w + w - 2]);
        }
    }
    map
}

/// How much each pixel of this photo counts for the merge, before the photos are weighed against each other.
fn quality(kind: Kind, rgb: &[f32], luma: &[f32], inside: &[bool], w: usize, h: usize) -> Vec<f32> {
    let mut map = contrast(luma, w, h);
    match kind {
        Kind::Focus => {
            blur(&mut map, w, h, SMOOTH);
            // sharper wins by a lot: the weight is the sharpness to a power, in the weights() that follows
        }
        Kind::Exposure { brightness } => {
            // Mertens, Kautz and Van Reeth: contrast, saturation and how close to the middle of the range each channel is, multiplied
            let target = EXPOSURE_TARGET + EXPOSURE_REACH * brightness.clamp(-100, 100) as f32 / 100.0;
            blur(&mut map, w, h, 1);
            for (i, c) in map.iter_mut().enumerate() {
                let p = &rgb[i * 3..i * 3 + 3];
                let mean = (p[0] + p[1] + p[2]) / 3.0;
                let saturation = (((p[0] - mean).powi(2) + (p[1] - mean).powi(2) + (p[2] - mean).powi(2)) / 3.0).sqrt();
                // a channel at the very top or bottom of the range counts for little, whatever the setting looks for: clipped says nothing of the scene
                let well: f32 = p.iter().map(|v| (-(v - target).powi(2) / (2.0 * WELL_SIGMA * WELL_SIGMA)).exp() * (1.0 - 0.95 * smoothstep(*v, 0.9, 0.99)) * (0.05 + 0.95 * smoothstep(*v, 0.01, 0.05))).product();
                *c = (*c + 1e-3) * (saturation + 1e-2) * well;
            }
            blur(&mut map, w, h, 2);
        }
    }
    for (v, inside) in map.iter_mut().zip(inside) {
        if !inside {
            *v = -1.0;
        }
    }
    map
}

/// 0 below `lo`, 1 above `hi`, smoothly between.
fn smoothstep(v: f32, lo: f32, hi: f32) -> f32 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn median(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let mid = values.len() / 2;
    Some(*values.select_nth_unstable_by(mid, |a, b| a.total_cmp(b)).1)
}

/// Where this photo disagrees with the reference about the scene (something moved, or a photo is a different scene in places): 1 there, 0 where they
/// agree. Brightness is evened out first, since the photos of an exposure bracket are meant to differ in that, and where either is clipped there is nothing to compare.
fn ghosts(kind: Kind, rgb: &[f32], reference: &[f32], inside: &[bool], w: usize, h: usize) -> (Vec<f32>, f32) {
    let usable = |v: f32| (CLIPPED..=1.0 - CLIPPED).contains(&v);
    let (luma, ref_luma) = (luma_of(rgb), luma_of(reference));
    let gain = match kind {
        Kind::Focus => 1.0,
        Kind::Exposure { .. } => {
            let mut ratios: Vec<f32> = (0..w * h).filter(|&i| inside[i] && usable(luma[i]) && usable(ref_luma[i])).map(|i| ref_luma[i] / luma[i]).collect();
            median(&mut ratios).unwrap_or(1.0)
        }
    };
    let mut map = vec![0.0f32; w * h];
    let (lo, hi);
    match kind {
        Kind::Focus => {
            // the fine detail is what differs between two focus distances: only the coarse picture says whether something moved
            let (mut a, mut b) = (luma.clone(), ref_luma.clone());
            for plane in [&mut a, &mut b] {
                blur(plane, w, h, ALIGN_BLUR);
                blur(plane, w, h, ALIGN_BLUR);
            }
            (lo, hi) = (0.06, 0.16);
            for i in 0..w * h {
                if inside[i] && usable(luma[i]) && usable(ref_luma[i]) {
                    map[i] = smoothstep((a[i] - b[i]).abs(), lo, hi);
                }
            }
        }
        Kind::Exposure { .. } => {
            // colour counts: a different coloured thing can be as bright as what it hid
            (lo, hi) = (0.07, 0.18);
            for i in 0..w * h {
                if !inside[i] || !usable(luma[i]) {
                    continue;
                }
                if usable(ref_luma[i]) {
                    let diff = (0..3).map(|c| (rgb[i * 3 + c] * gain - reference[i * 3 + c]).abs()).fold(0.0f32, f32::max);
                    map[i] = smoothstep(diff, lo, hi);
                } else if ref_luma[i] > 0.5 {
                    // the reference is blown out, so the scene there is at least as bright as it shows: a photo that shows much less, once evened out, has something else in it
                    map[i] = smoothstep(0.85 - luma[i] * gain, 0.0, 0.25);
                } else {
                    // and the other way round where the reference is black
                    map[i] = smoothstep(luma[i] * gain - 0.15, 0.0, 0.25);
                }
            }
            // single pixels differing is noise, a patch of them is something that moved
            blur(&mut map, w, h, 2);
            for v in map.iter_mut() {
                *v = smoothstep(*v, 0.15, 0.5);
            }
        }
    }
    // what moved is a little bigger than where it was seen to differ
    blur(&mut map, w, h, GHOST_GROW);
    (map.iter().map(|v| (v * 3.0).min(1.0)).collect(), gain)
}

/// The weights of every photo at every working pixel, each pixel's adding up to one. Where a photo disagrees with the reference its weight goes to the reference.
fn weights(kind: Kind, qualities: &[Option<Vec<f32>>], ghost: &[Option<Vec<f32>>], reference: usize) -> Vec<Option<Vec<f32>>> {
    let size = qualities.iter().flatten().next().map_or(0, Vec::len);
    let power = match kind {
        Kind::Focus => FAVOUR,
        Kind::Exposure { .. } => 1,
    };
    let floor = match kind {
        Kind::Focus => {
            let (sum, n) = qualities.iter().flatten().flat_map(|m| m.iter()).filter(|v| **v >= 0.0).fold((0.0f64, 0usize), |a, v| (a.0 + *v as f64, a.1 + 1));
            // a little of the average everywhere, so a flat patch is the average of the photos rather than whichever has the least noise
            (if n == 0 { 0.0 } else { (sum / n as f64) as f32 * 0.05 }).max(1e-6)
        }
        Kind::Exposure { .. } => 1e-6,
    };
    let mut out: Vec<Option<Vec<f32>>> = qualities.iter().map(|m| m.as_ref().map(|_| vec![0.0f32; size])).collect();
    for i in 0..size {
        let mut moved = 0.0f32;
        let mut total = 0.0f32;
        for (k, (q, weight)) in qualities.iter().zip(out.iter_mut()).enumerate() {
            let (Some(q), Some(weight)) = (q, weight) else { continue };
            let s = q[i];
            let w = if s < 0.0 { 0.0 } else { (s + floor).powi(power) };
            let g = if k == reference { 0.0 } else { ghost[k].as_ref().map_or(0.0, |g| g[i]) };
            weight[i] = w * (1.0 - g);
            moved += w * g;
            total += w;
        }
        if let Some(Some(weight)) = out.get_mut(reference) {
            weight[i] += moved;
        }
        if total > 0.0 {
            for weight in out.iter_mut().flatten() {
                weight[i] /= total;
            }
        }
    }
    out
}

// ── Pyramids ──────────────────────────────────────────────────────────

/// A picture of `ch` values a pixel.
struct Layer {
    w: usize,
    h: usize,
    data: Vec<f32>,
}

/// Half the size, by blurring with the 1 4 6 4 1 kernel and keeping every other pixel.
fn reduce(src: &Layer, ch: usize) -> Layer {
    let (w, h) = (src.w, src.h);
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    let k = [1.0f32, 4.0, 6.0, 4.0, 1.0];
    let mut wide = vec![0.0f32; w2 * h * ch];
    for y in 0..h {
        for x2 in 0..w2 {
            for c in 0..ch {
                let mut v = 0.0;
                for (t, kv) in k.iter().enumerate() {
                    let x = (x2 * 2 + t).saturating_sub(2).min(w - 1);
                    v += kv * src.data[(y * w + x) * ch + c];
                }
                wide[(y * w2 + x2) * ch + c] = v / 16.0;
            }
        }
    }
    let mut data = vec![0.0f32; w2 * h2 * ch];
    for y2 in 0..h2 {
        for x in 0..w2 * ch {
            let mut v = 0.0;
            for (t, kv) in k.iter().enumerate() {
                let y = (y2 * 2 + t).saturating_sub(2).min(h - 1);
                v += kv * wide[y * w2 * ch + x];
            }
            data[y2 * w2 * ch + x] = v / 16.0;
        }
    }
    Layer { w: w2, h: h2, data }
}

/// `src` grown to w x h (twice its size, or one less) by the same kernel.
fn expand(src: &Layer, ch: usize, w: usize, h: usize) -> Layer {
    let at = |v: &[f32], x: usize, y: usize, stride: usize, c: usize| v[y * stride + x * ch + c];
    let mut wide = vec![0.0f32; w * src.h * ch];
    for y in 0..src.h {
        for x in 0..w {
            for c in 0..ch {
                let (i, last) = (x / 2, src.w - 1);
                let v = if x % 2 == 0 {
                    (6.0 * at(&src.data, i.min(last), y, src.w * ch, c) + at(&src.data, i.saturating_sub(1).min(last), y, src.w * ch, c) + at(&src.data, (i + 1).min(last), y, src.w * ch, c)) / 8.0
                } else {
                    (at(&src.data, i.min(last), y, src.w * ch, c) + at(&src.data, (i + 1).min(last), y, src.w * ch, c)) / 2.0
                };
                wide[(y * w + x) * ch + c] = v;
            }
        }
    }
    let mut data = vec![0.0f32; w * h * ch];
    for y in 0..h {
        let (i, last) = (y / 2, src.h - 1);
        for x in 0..w * ch {
            let v = if y % 2 == 0 {
                (6.0 * wide[i.min(last) * w * ch + x] + wide[i.saturating_sub(1).min(last) * w * ch + x] + wide[(i + 1).min(last) * w * ch + x]) / 8.0
            } else {
                (wide[i.min(last) * w * ch + x] + wide[(i + 1).min(last) * w * ch + x]) / 2.0
            };
            data[y * w * ch + x] = v;
        }
    }
    Layer { w, h, data }
}

/// The Gaussian pyramid of a picture: itself, then each level half the size of the one before.
fn gaussian(base: Layer, ch: usize, levels: usize) -> Vec<Layer> {
    let mut out = vec![base];
    for _ in 0..levels {
        let next = reduce(out.last().expect("a level"), ch);
        out.push(next);
    }
    out
}

/// The Laplacian pyramid: what each level has that the smaller one doesn't, with the smallest level whole.
fn laplacian(mut gauss: Vec<Layer>, ch: usize) -> Vec<Layer> {
    let mut out = Vec::with_capacity(gauss.len());
    for l in 0..gauss.len() - 1 {
        let up = expand(&gauss[l + 1], ch, gauss[l].w, gauss[l].h);
        out.push(Layer { w: gauss[l].w, h: gauss[l].h, data: gauss[l].data.iter().zip(&up.data).map(|(a, b)| a - b).collect() });
    }
    out.extend(gauss.pop());
    out
}

/// The pyramid back into a picture.
fn collapse(mut levels: Vec<Layer>, ch: usize) -> Layer {
    let mut out = levels.pop().expect("a level");
    while let Some(level) = levels.pop() {
        let up = expand(&out, ch, level.w, level.h);
        out = Layer { w: level.w, h: level.h, data: level.data.iter().zip(&up.data).map(|(a, b)| a + b).collect() };
    }
    out
}

/// The most memory the strips of the result may take while the photos are blended into them (bytes), and the biggest photo that is merged at all (pixels)
const MEMORY_BUDGET: usize = 1500 << 20;
/// The biggest photo that is looked at at all (pixels), checked from the file's header before anything is decoded
pub const MAX_PIXELS: usize = 100_000_000;
/// How many workers and how much for the collecting strips fit the memory the merge may use at w x h, None if not even one of each does. Everything alive at once
/// counts: the reference, the photo being added and the result (three whole pictures), what each worker needs for the strip it is on, and the strips collecting the blend.
fn memory_plan(w: usize, h: usize, cores: usize) -> Option<(usize, usize)> {
    let rows = strip_rows(h);
    let widest = rows.iter().map(|r| strip_bytes(w, h, *r)).max()?;
    let spare = MEMORY_BUDGET.checked_sub(3 * w * h * 3)?;
    let worker = widest * WORKER_STRIPS;
    if spare < widest + worker {
        return None;
    }
    let threads = cores.clamp(1, 4).min((spare - widest) / worker.max(1)).max(1);
    Some((threads, spare - threads * worker))
}

/// What a worker needs for a strip, as a multiple of what the strip's collecting pyramid takes (call that 16 bytes a pixel of the strip's rows, P of them): counted from add_to_strip
/// at its biggest, which is when laplacian() has made the first level, the weights 4P, the Gaussian pyramid 16P (its first level is the photo's rows, 12P), the grown
/// copy of the second level 12P and the first Laplacian level 12P: 44P, and the allocator rounds up a little. Four times 16P (64P) leaves room.
const WORKER_STRIPS: usize = 4;
/// The levels each strip is blended in
const LEVELS: usize = 6;
/// A strip is this many rows, with this many more above and below that the blend looks at but doesn't keep
const STRIP: usize = 768;
/// The rows a strip looks at above and below what it keeps: a pyramid reaches this far both in reducing and in expanding
const MARGIN: usize = 2 << LEVELS;

/// Where a photo disagrees with the reference, the reference's picture, and how much brighter than the reference this photo shows the same thing.
struct Ghost<'a> {
    map: &'a [f32],
    reference: &'a [u8],
    expected: f32,
}

/// One strip of rows of the result, whose pyramid collects what the photos give it.
struct Strip {
    /// The rows it keeps, and the rows (with the margin) it works on
    keep: (usize, usize),
    from: usize,
    levels: Vec<Layer>,
}

/// The rows of each strip of the result that are kept.
fn strip_rows(h: usize) -> Vec<(usize, usize)> {
    (0..h).step_by(STRIP).map(|y0| (y0, (y0 + STRIP).min(h))).collect()
}

/// How many levels every strip has: the same for all, so what two strips make of the rows they share is the same, and no more than the smallest strip allows.
fn pyramid_depth(w: usize, h: usize, rows: &[(usize, usize)]) -> usize {
    let smallest = rows.iter().map(|(y0, y1)| (y1 + MARGIN).min(h) - y0.saturating_sub(MARGIN)).min().unwrap_or(h);
    LEVELS.min(w.min(smallest).max(1).ilog2().saturating_sub(2) as usize)
}

/// An empty strip for these rows of a picture w pixels wide.
fn new_strip(w: usize, h: usize, (y0, y1): (usize, usize), depth: usize) -> Strip {
    let (r0, r1) = (y0.saturating_sub(MARGIN), (y1 + MARGIN).min(h));
    let (mut lw, mut lh) = (w, r1 - r0);
    let mut levels = Vec::with_capacity(depth + 1);
    for _ in 0..=depth {
        levels.push(Layer { w: lw, h: lh, data: vec![0.0; lw * lh * 3] });
        (lw, lh) = (lw.div_ceil(2), lh.div_ceil(2));
    }
    Strip { keep: (y0, y1), from: r0, levels }
}

/// The strips (by index, from..to) that go through the photos together: as many as fit the budget, and at least one.
fn groups(rows: &[(usize, usize)], w: usize, h: usize, budget: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < rows.len() {
        let mut end = at;
        let mut bytes = 0;
        while end < rows.len() && (end == at || bytes + strip_bytes(w, h, rows[end]) <= budget) {
            bytes += strip_bytes(w, h, rows[end]);
            end += 1;
        }
        out.push((at, end));
        at = end;
    }
    out
}

/// How many bytes a strip of these rows takes while it collects.
fn strip_bytes(w: usize, h: usize, rows: (usize, usize)) -> usize {
    let (r0, r1) = (rows.0.saturating_sub(MARGIN), (rows.1 + MARGIN).min(h));
    (w * (r1 - r0) * 3 * 4) * 4 / 3 + 1024
}

/// What photo `rgb` (w x h, through `warp`) gives a strip: its Laplacian pyramid weighed by the Gaussian pyramid of its weights, added to the strip's.
fn add_to_strip(strip: &mut Strip, rgb: &[u8], (w, h): (usize, usize), warp: &Warp, weight: &[f32], work: (usize, usize, usize), ghost: Option<&Ghost>) {
    let (ww, wh, factor) = work;
    let rows = strip.levels[0].h;
    // the middle of the working picture, where the warp turns about, is this pixel of the full one (a working pixel is the average of `factor` of them)
    let centre = ((ww as f64 / 2.0 + 0.5) * factor as f64 - 0.5, (wh as f64 / 2.0 + 0.5) * factor as f64 - 0.5);
    let mut image = vec![0.0f32; w * rows * 3];
    let mut wmap = vec![0.0f32; w * rows];
    for ry in 0..rows {
        let y = strip.from + ry;
        for x in 0..w {
            let (sx, sy) = warp.at(centre, x as f64, y as f64);
            let (sx, sy) = (sx.clamp(0.0, (w - 1) as f64), sy.clamp(0.0, (h - 1) as f64));
            let (ix, iy) = (sx.floor() as usize, sy.floor() as usize);
            let (jx, jy) = ((ix + 1).min(w - 1), (iy + 1).min(h - 1));
            let (gx, gy) = ((sx - ix as f64) as f32, (sy - iy as f64) as f32);
            for c in 0..3 {
                let q = |xx: usize, yy: usize| rgb[(yy * w + xx) * 3 + c] as f32;
                let top = q(ix, iy) + (q(jx, iy) - q(ix, iy)) * gx;
                let bottom = q(ix, jy) + (q(jx, jy) - q(ix, jy)) * gx;
                image[(ry * w + x) * 3 + c] = (top + (bottom - top) * gy) / 255.0;
            }
            // the weight where this pixel is on the working picture
            let (wx, wy) = (((x as f64 + 0.5) / factor as f64 - 0.5).clamp(0.0, (ww - 1) as f64), ((y as f64 + 0.5) / factor as f64 - 0.5).clamp(0.0, (wh - 1) as f64));
            let (x0, y0) = (wx.floor() as usize, wy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(ww - 1), (y0 + 1).min(wh - 1));
            let (fx, fy) = ((wx - x0 as f64) as f32, (wy - y0 as f64) as f32);
            let p = |xx: usize, yy: usize| weight[yy * ww + xx];
            let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * fx;
            let bottom = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * fx;
            wmap[ry * w + x] = top + (bottom - top) * fy;
            // where the photo disagrees with the reference the reference's picture stands in for it, as bright as this photo would have shown it: its weight is
            // moved to the reference too, but the coarse levels of a pyramid look past a small patch of zero weight, so what moved must not be in the photo at all
            if let Some(g) = ghost {
                let p = |xx: usize, yy: usize| g.map[yy * ww + xx];
                let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * fx;
                let bottom = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * fx;
                let moved = (top + (bottom - top) * fy).clamp(0.0, 1.0);
                if moved > 0.0 {
                    for c in 0..3 {
                        let stand_in = (g.reference[(y * w + x) * 3 + c] as f32 / 255.0 * g.expected).clamp(0.0, 1.0);
                        let here = &mut image[(ry * w + x) * 3 + c];
                        *here += (stand_in - *here) * moved;
                    }
                }
            }
        }
    }
    let depth = strip.levels.len() - 1;
    let lap = laplacian(gaussian(Layer { w, h: rows, data: image }, 3, depth), 3);
    let weights = gaussian(Layer { w, h: rows, data: wmap }, 1, depth);
    for ((acc, l), wl) in strip.levels.iter_mut().zip(&lap).zip(&weights) {
        for (i, wv) in wl.data.iter().enumerate() {
            for c in 0..3 {
                acc.data[i * 3 + c] += wv * l.data[i * 3 + c];
            }
        }
    }
}

/// The photo the others are lined up with and the result is the size of: the middle one of a focus bracket, and of an exposure bracket the one with the middle brightness,
/// whatever order the files came in (a photo that had to be fetched again can come last).
fn reference_of(count: usize, load: &dyn Fn(usize) -> Option<Image>, kind: Kind) -> usize {
    if !matches!(kind, Kind::Exposure { .. }) {
        return count / 2;
    }
    let mut means: Vec<(f64, usize)> = (0..count).filter_map(|i| {
        let image = load(i)?;
        let pixels = image.rgb.as_chunks::<3>().0.iter().step_by(37);
        let (sum, n) = pixels.fold((0.0f64, 0usize), |a, p| (a.0 + 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64, a.1 + 1));
        Some((sum / n.max(1) as f64, i))
    }).collect();
    means.sort_by(|a, b| a.0.total_cmp(&b.0));
    means.get(means.len() / 2).map_or(count / 2, |m| m.1)
}

/// Merges `count` photos, which `load` hands over one at a time (twice each). The middle one is what the others are lined up with, what the result is the
/// size of, and what stands in where a photo disagrees with it.
pub fn merge(count: usize, load: &dyn Fn(usize) -> Option<Image>, kind: Kind) -> Result<(Image, usize), String> {
    merge_frames(count, load, kind).map(|(image, used)| (image, used.iter().filter(|u| **u).count()))
}

/// merge, saying which of the photos went into the picture.
pub fn merge_frames(count: usize, load: &dyn Fn(usize) -> Option<Image>, kind: Kind) -> Result<(Image, Vec<bool>), String> {
    if count < MIN_FRAMES {
        return Err("there are too few photos to merge".into());
    }
    let middle = reference_of(count, load, kind);
    let reference = load(middle).ok_or("the middle photo can't be read")?;
    let (w, h) = (reference.width, reference.height);
    if w * h > MAX_PIXELS {
        return Err("the photos are too big to merge".into());
    }
    let factor = w.div_ceil(WORK_WIDTH).max(1);
    let ref_small = small(&reference, factor).ok_or("the middle photo is too small")?;
    let (ww, wh) = (ref_small.width, ref_small.height);
    // how each photo lies on the middle one, and how much each pixel of it counts
    let mut warps: Vec<Option<Warp>> = vec![None; count];
    let mut qualities: Vec<Option<Vec<f32>>> = vec![None; count];
    let mut ghost_maps: Vec<Option<Vec<f32>>> = vec![None; count];
    let mut gains = vec![1.0f32; count];
    for i in 0..count {
        let loaded = if i == middle { None } else { load(i).filter(|f| (f.width, f.height) == (w, h)).and_then(|f| small(&f, factor)) };
        let (sm, warp) = match (&loaded, i == middle) {
            (_, true) => (&ref_small, Some(Warp::NONE)),
            (Some(sm), _) => (sm, estimate(&ref_small.luma, &sm.luma)),
            _ => continue,
        };
        let Some(warp) = warp else { continue };
        let (rgb, inside) = warped(sm, &warp);
        let luma = luma_of(&rgb);
        qualities[i] = Some(quality(kind, &rgb, &luma, &inside, ww, wh));
        if i != middle {
            let (map, gain) = ghosts(kind, &rgb, &ref_small.rgb, &inside, ww, wh);
            ghost_maps[i] = Some(map);
            gains[i] = gain;
        }
        warps[i] = Some(warp);
    }
    if warps.iter().flatten().count() < MIN_FRAMES {
        return Err("the photos can't be lined up".into());
    }
    let weights = weights(kind, &qualities, &ghost_maps, middle);
    drop(qualities);
    let used: Vec<bool> = warps.iter().map(Option::is_some).collect();
    let rows = strip_rows(h);
    let depth = pyramid_depth(w, h, &rows);
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    let (threads, strip_budget) = memory_plan(w, h, cores).ok_or("the photos are too big to merge")?;
    let mut out = vec![0u8; w * h * 3];
    // the strips are done in groups that fit the memory left, each group reading the photos again
    for (at, end) in groups(&rows, w, h, strip_budget) {
        let mut parts: Vec<Strip> = rows[at..end].iter().map(|r| new_strip(w, h, *r, depth)).collect();
        for i in 0..count {
            let (Some(warp), Some(weight)) = (warps[i], weights[i].as_ref()) else { continue };
            // the warp was measured on the working picture: its shift is that many pixels there and more here
            let full = Warp { scale: warp.scale, shift: (warp.shift.0 * factor as f64, warp.shift.1 * factor as f64) };
            let loaded = if i == middle { None } else { load(i) };
            let ghost = ghost_maps[i].as_deref().map(|map| Ghost { map, reference: &reference.rgb, expected: 1.0 / gains[i] });
            let rgb: &[u8] = match (&loaded, i == middle) {
                (_, true) => &reference.rgb,
                (Some(f), _) if (f.width, f.height) == (w, h) => &f.rgb,
                // the weights were worked out with this photo in them: without it the picture would come out dark
                _ => return Err("a photo changed while the photos were merged".into()),
            };
            let per = parts.len().div_ceil(threads).max(1);
            std::thread::scope(|scope| {
                for chunk in parts.chunks_mut(per) {
                    scope.spawn(|| chunk.iter_mut().for_each(|strip| add_to_strip(strip, rgb, (w, h), &full, weight, (ww, wh, factor), ghost.as_ref())));
                }
            });
        }
        for strip in parts {
            let (y0, y1) = strip.keep;
            let from = strip.from;
            let done = collapse(strip.levels, 3);
            for y in y0..y1 {
                let src = &done.data[(y - from) * w * 3..(y - from + 1) * w * 3];
                for (o, v) in out[y * w * 3..(y + 1) * w * 3].iter_mut().zip(src) {
                    *o = (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    Ok((Image { width: w, height: h, rgb: out }, used))
}

// ── Files ─────────────────────────────────────────────────────────────

/// A JPEG's pixels, and its Exif block (the orientation and camera notes) as the file has it.
fn read_jpeg(path: &Path) -> Option<(Image, Option<Vec<u8>>)> {
    let bytes = std::fs::read(path).ok()?;
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(&bytes), DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB));
    let rgb = decoder.decode().ok()?;
    let info = decoder.info()?;
    let (width, height) = (info.width as usize, info.height as usize);
    (rgb.len() == width * height * 3).then(|| (Image { width, height, rgb }, exif_of(&bytes)))
}

/// A JPEG's width and height, from its header alone.
fn jpeg_size(path: &Path) -> Option<(usize, usize)> {
    let bytes = std::fs::read(path).ok()?;
    let mut decoder = JpegDecoder::new(ZCursor::new(&bytes));
    decoder.decode_headers().ok()?;
    decoder.dimensions()
}

/// The Exif segment of a JPEG, whole with its marker and length.
fn exif_of(jpeg: &[u8]) -> Option<Vec<u8>> {
    let mut at = 2;
    while at + 4 <= jpeg.len() && jpeg[at] == 0xFF {
        let marker = jpeg[at + 1];
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        let len = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        if at + 2 + len > jpeg.len() {
            return None;
        }
        if marker == 0xE1 && jpeg[at + 4..].starts_with(b"Exif\0\0") {
            return Some(jpeg[at..at + 2 + len].to_vec());
        }
        at += 2 + len;
    }
    None
}

/// A JPEG of the picture at this quality (1..100), with the Exif block put in after its start.
pub fn encode_jpeg(image: &Image, quality: u8, exif: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    image::ImageEncoder::write_image(encoder, &image.rgb, image.width as u32, image.height as u32, image::ExtendedColorType::Rgb8).map_err(|e| e.to_string())?;
    if let (Some(exif), true) = (exif, out.len() > 2) {
        let mut with = Vec::with_capacity(out.len() + exif.len());
        with.extend_from_slice(&out[..2]);
        with.extend_from_slice(exif);
        with.extend_from_slice(&out[2..]);
        out = with;
    }
    Ok(out)
}

/// The merged picture put in a new file next to `first`, named for it with `_focus-merged` (or `_exposure-merged`), whole or not at all: written to a file of its
/// own, synced, then linked to the name (which fails, and the next name is tried, if another file has it) and the folder synced.
fn publish(first: &Path, tag: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let stem = first.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "photo".into());
    let folder = first.parent().unwrap_or_else(|| Path::new("."));
    let part = folder.join(format!(".{stem}_{tag}-merged.{}.part", std::process::id()));
    let written = std::fs::File::create(&part).and_then(|mut f| std::io::Write::write_all(&mut f, bytes).and_then(|_| f.sync_all()));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    let mut published = Err(std::io::Error::other("no free name"));
    for n in 1..1000 {
        let name = if n == 1 { format!("{stem}_{tag}-merged.jpg") } else { format!("{stem}_{tag}-merged ({n}).jpg") };
        let path = folder.join(name);
        match std::fs::hard_link(&part, &path) {
            Ok(()) => {
                published = Ok(path);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            // a place that can't link (some network folders) gets the file moved there, which only fails over a name that is taken
            Err(_) if !path.exists() => {
                published = std::fs::rename(&part, &path).map(|_| path);
                break;
            }
            Err(e) => {
                published = Err(e);
                break;
            }
        }
    }
    let _ = std::fs::remove_file(&part);
    #[cfg(unix)]
    if published.is_ok() {
        let _ = std::fs::File::open(folder).and_then(|d| d.sync_all());
    }
    published
}

/// A small Exif block with only the orientation of the one `exif_of` found (the camera's notes and its thumbnail stay behind: the thumbnail would show a photo, not the merge).
fn orientation_exif(segment: &[u8]) -> Option<Vec<u8>> {
    let tiff = segment.get(4 + 6..)?;
    let big = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b = [*tiff.get(at)?, *tiff.get(at + 1)?];
        Some(if big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b = [*tiff.get(at)?, *tiff.get(at + 1)?, *tiff.get(at + 2)?, *tiff.get(at + 3)?];
        Some(if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    };
    let ifd = u32_at(4)? as usize;
    let entries = u16_at(ifd)? as usize;
    let orientation = (0..entries.min(64)).find_map(|n| {
        let at = ifd + 2 + n * 12;
        (u16_at(at)? == 0x0112).then(|| u16_at(at + 8)).flatten()
    })?;
    if !(1..=8).contains(&orientation) {
        return None;
    }
    let mut out = vec![0xFF, 0xE1, 0x00, 0x22];
    out.extend_from_slice(b"Exif\0\0MM\0\x2A\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01");
    out.extend_from_slice(&orientation.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    Some(out)
}

// Capacity includes captures still arriving, queued merges and the merge in progress.
const MERGE_CAPACITY: usize = 2;

#[derive(Clone)]
struct MergeQueue {
    tx: std::sync::mpsc::SyncSender<MergeJob>,
    used: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

pub struct Reservation {
    queue: MergeQueue,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.queue.used.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

struct MergeJob {
    _reservation: Reservation,
    poster: std::sync::Arc<dyn Poster>,
    files: Vec<String>,
    quality: u8,
    keep_frames: bool,
    kind: Kind,
    request: u64,
    expected: usize,
}

impl MergeJob {
    fn run(self) {
        let mut result = std::panic::catch_unwind(|| merge_files(&self.files, self.quality, self.keep_frames, self.kind, self.expected)).unwrap_or_else(|_| json!({"ok": false, "error": "it stopped unexpectedly"}));
        result["request"] = json!(self.request);
        self.poster.post("merged", result);
    }
}

impl MergeQueue {
    fn new(work: impl Fn(MergeJob) + Send + 'static) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel(MERGE_CAPACITY);
        std::thread::Builder::new().name("bracket-merge".into()).spawn(move || {
            while let Ok(job) = rx.recv() {
                work(job);
            }
        }).ok()?;
        Some(Self { tx, used: std::sync::Arc::default() })
    }

    fn reserve(&self) -> Option<Reservation> {
        self.used.try_update(std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst, |n| (n < MERGE_CAPACITY).then_some(n + 1)).ok()?;
        Some(Reservation { queue: self.clone() })
    }
}

/// Reserves merge capacity before asking the camera for a bracket.
pub fn reserve() -> Option<Reservation> {
    static QUEUE: std::sync::OnceLock<Option<MergeQueue>> = std::sync::OnceLock::new();
    QUEUE.get_or_init(|| MergeQueue::new(MergeJob::run)).as_ref()?.reserve()
}

/// What changed in a file if it did: its size and when it was written.
fn stamp(path: &Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// What a finished merge tells the plugin: {"ok", "path", "frames" (the shots there should be), "used", "removed", "left", "kept"} or {"ok": false, "error"}. The photos are
/// only deleted (`keep_frames` false) when there is a JPEG for every one of the `expected` shots, none of them changed meanwhile and the merged file is safely on
/// disk, the ones that could not be lined up too: a shot that never arrived has its only copy among the files being deleted (its RAW, in a RAW and JPEG bracket).
pub fn merge_files(files: &[String], quality: u8, keep_frames: bool, kind: Kind, expected: usize) -> Value {
    let jpegs: Vec<PathBuf> = files.iter().map(PathBuf::from).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))).collect();
    let fail = |error: &str| json!({"ok": false, "error": error});
    if jpegs.len() < MIN_FRAMES {
        return fail("fewer than two JPEG photos");
    }
    // how big the photos are, before any of them is decoded
    if jpegs.iter().any(|p| jpeg_size(p).is_none_or(|(w, h)| w * h > MAX_PIXELS)) {
        return fail("the photos are too big to merge");
    }
    let before: Vec<_> = jpegs.iter().map(|p| stamp(p)).collect();
    let exif = read_jpeg(&jpegs[jpegs.len() / 2]).and_then(|(_, exif)| exif).and_then(|e| orientation_exif(&e));
    // which photos decoded: a truncated one passes the header check but isn't a copy worth deleting its RAW for
    let decoded = std::sync::Mutex::new(vec![false; jpegs.len()]);
    let read = |i: usize| {
        let image = read_jpeg(&jpegs[i]).map(|(image, _)| image);
        decoded.lock().unwrap_or_else(|e| e.into_inner())[i] = image.is_some();
        image
    };
    let (merged, in_merge) = match merge_frames(jpegs.len(), &read, kind) {
        Ok(merged) => merged,
        Err(e) => return fail(&e),
    };
    // a photo that was edited while the merge worked has been read in two versions
    if jpegs.iter().map(|p| stamp(p)).collect::<Vec<_>>() != before || before.iter().any(Option::is_none) {
        return fail("a photo changed while the photos were merged");
    }
    let bytes = match encode_jpeg(&merged, quality, exif.as_deref()) {
        Ok(bytes) => bytes,
        Err(e) => return fail(&e),
    };
    drop(merged);
    let path = match publish(&jpegs[0], kind.tag(), &bytes) {
        Ok(path) => path,
        Err(e) => return fail(&e.to_string()),
    };
    let used = in_merge.iter().filter(|u| **u).count();
    // every shot has to be here: one that never arrived has its only copy among the files being deleted (its RAW, in a RAW and JPEG bracket)
    let complete = jpegs.len() >= expected && decoded.lock().unwrap_or_else(|e| e.into_inner()).iter().all(|d| *d);
    let (mut removed, mut left) = (0, 0);
    if !keep_frames && complete {
        for f in files {
            match std::fs::remove_file(f) {
                Ok(()) => removed += 1,
                Err(_) => left += 1,
            }
        }
    }
    json!({"ok": true, "path": path.display().to_string(), "frames": expected.max(jpegs.len()), "used": used, "removed": removed, "left": left, "kept": !keep_frames && !complete})
}

impl Reservation {
    /// Queues a reserved bracket without waiting for the worker.
    pub fn submit(self, poster: std::sync::Arc<dyn Poster>, files: Vec<String>, quality: u8, keep_frames: bool, kind: Kind, request: u64, expected: usize) {
        let tx = self.queue.tx.clone();
        let job = MergeJob { _reservation: self, poster, files, quality, keep_frames, kind, request, expected };
        if let Err(std::sync::mpsc::TrySendError::Full(job) | std::sync::mpsc::TrySendError::Disconnected(job)) = tx.try_send(job) {
            job.poster.post("merged", json!({"ok": false, "error": "no worker to merge on", "request": job.request}));
        }
    }
}

#[cfg(test)]
#[path = "bracket_merge_tests.rs"]
mod tests;
