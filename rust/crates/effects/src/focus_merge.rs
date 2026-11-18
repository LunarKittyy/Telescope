//! Focus merging: one picture that is sharp all the way through, made from the photos of a focus bracket.
//!
//! The photos are lined up with the middle one, which also covers focus breathing (the picture grows a little as the focus moves nearer): the
//! shift between two photos is measured by phase correlation in five patches (core's `shift_between`, which Stargazer lines frames up with)
//! and a scale and a shift are fitted to them. Each photo's sharpness is then judged on a picture about 1000 pixels wide, and every pixel of the
//! result is a blend of the photos, weighted heavily to the sharpest one and smoothly, so there are no seams where the sharpest photo changes.
//! The photos are read twice (once to judge them, once to blend them) so only one is in memory at a time besides the result.

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
/// The fine detail is blurred away over this many pixels either way (twice) before photos are compared for how they sit
const ALIGN_BLUR: usize = 8;
/// The sharpness is smoothed over this many working pixels either way (twice), so the weights change gradually
const SMOOTH: usize = 6;
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

/// The plane with its fine detail blurred away: photos of one scene at different focus differ most in the fine detail, and it is the coarse picture they share.
fn coarse(plane: &Plane) -> Plane {
    let mut data = plane.data.clone();
    blur(&mut data, plane.width, plane.height, ALIGN_BLUR);
    blur(&mut data, plane.width, plane.height, ALIGN_BLUR);
    Plane { width: plane.width, height: plane.height, data }
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
    ((fine.scale - 1.0).abs() <= MAX_BREATHING && fine.shift.0.hypot(fine.shift.1) <= far).then_some(fine)
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
    let mean = |p: &Plane| p.data.iter().sum::<f32>() / p.data.len() as f32;
    let (ref_mean, moved_mean) = (mean(reference), mean(moved));
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
                let value = bilinear(moved, sx, sy) - moved_mean;
                let (gx, gy) = (((bilinear(moved, sx + 1.0, sy) - bilinear(moved, sx - 1.0, sy)) / 2.0) as f64, ((bilinear(moved, sx, sy + 1.0) - bilinear(moved, sx, sy - 1.0)) / 2.0) as f64);
                let jac = [gx * u + gy * v, gx, gy];
                let e = (value - (reference.data[y * w + x] - ref_mean)) as f64;
                for i in 0..3 {
                    b[i] += jac[i] * e;
                    for j in i..3 {
                        a[i][j] += jac[i] * jac[j];
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
        let (a, b) = (patch(reference, size, cx, cy), patch(moved, size, cx, cy));
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

/// How sharp each pixel of the result's picture is in this photo, from its luma sampled through the warp: the size of the Laplacian, smoothed. Pixels the photo
/// doesn't reach are 0.
fn sharpness_map(plane: &Plane, warp: &Warp) -> Vec<f32> {
    let (w, h) = (plane.width, plane.height);
    let centre = (w as f64 / 2.0, h as f64 / 2.0);
    let mut warped = vec![0.0f32; w * h];
    let mut inside = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = warp.at(centre, x as f64, y as f64);
            inside[y * w + x] = sx >= 0.0 && sy >= 0.0 && sx <= (w - 1) as f64 && sy <= (h - 1) as f64;
            warped[y * w + x] = bilinear(plane, sx, sy);
        }
    }
    let mut map = vec![0.0f32; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            map[i] = (warped[i - 1] + warped[i + 1] + warped[i - w] + warped[i + w] - 4.0 * warped[i]).abs();
        }
    }
    // the Laplacian has nothing to look at on the edge rows and columns: they take what is next to them, or the edge would look flat
    if w > 2 && h > 2 {
        for x in 0..w {
            (map[x], map[(h - 1) * w + x]) = (map[w + x], map[(h - 2) * w + x]);
        }
        for y in 0..h {
            (map[y * w], map[y * w + w - 1]) = (map[y * w + 1], map[y * w + w - 2]);
        }
    }
    blur(&mut map, w, h, SMOOTH);
    blur(&mut map, w, h, SMOOTH);
    for (v, inside) in map.iter_mut().zip(&inside) {
        if !inside {
            *v = -1.0;
        }
    }
    map
}

/// The weights of every photo at every working pixel, each pixel's adding up to one.
fn weights(maps: &[Option<Vec<f32>>]) -> Vec<Option<Vec<f32>>> {
    let size = maps.iter().flatten().next().map_or(0, Vec::len);
    let mean = {
        let (sum, n) = maps.iter().flatten().flat_map(|m| m.iter()).filter(|v| **v >= 0.0).fold((0.0f64, 0usize), |a, v| (a.0 + *v as f64, a.1 + 1));
        if n == 0 { 0.0 } else { (sum / n as f64) as f32 }
    };
    // a little of the average everywhere, so a flat patch is the average of the photos rather than whichever has the least noise
    let floor = (mean * 0.05).max(1e-6);
    let mut out: Vec<Option<Vec<f32>>> = maps.iter().map(|m| m.as_ref().map(|_| vec![0.0f32; size])).collect();
    for i in 0..size {
        let mut total = 0.0f32;
        for (map, weight) in maps.iter().zip(out.iter_mut()) {
            if let (Some(map), Some(weight)) = (map, weight) {
                let s = map[i];
                let w = if s < 0.0 { 0.0 } else { (s + floor).powi(FAVOUR) };
                weight[i] = w;
                total += w;
            }
        }
        if total > 0.0 {
            for weight in out.iter_mut().flatten() {
                weight[i] /= total;
            }
        }
    }
    out
}

/// Merges `count` photos, which `load` hands over one at a time (twice each). The middle one is what the others are lined up with, and what the result is the size of.
pub fn merge(count: usize, load: &dyn Fn(usize) -> Option<Image>) -> Result<Image, String> {
    if count < MIN_FRAMES {
        return Err("there are too few photos to merge".into());
    }
    let middle = count / 2;
    let reference = load(middle).ok_or("the middle photo can't be read")?;
    let (w, h) = (reference.width, reference.height);
    let factor = w.div_ceil(WORK_WIDTH).max(1);
    let ref_plane = luma(&reference, factor).ok_or("the middle photo is too small")?;
    // how each photo lies on the middle one, and how sharp it is there
    let mut warps: Vec<Option<Warp>> = vec![None; count];
    let mut maps: Vec<Option<Vec<f32>>> = vec![None; count];
    for i in 0..count {
        let plane = if i == middle { Some(ref_plane.clone()) } else { load(i).filter(|f| (f.width, f.height) == (w, h)).and_then(|f| luma(&f, factor)) };
        let Some(plane) = plane else { continue };
        let warp = if i == middle { Some(Warp::NONE) } else { estimate(&ref_plane, &plane) };
        if let Some(warp) = warp {
            maps[i] = Some(sharpness_map(&plane, &warp));
            warps[i] = Some(warp);
        }
    }
    let used = warps.iter().flatten().count();
    if used < MIN_FRAMES {
        return Err("the photos can't be lined up".into());
    }
    let weights = weights(&maps);
    drop(maps);
    let (ww, wh) = (ref_plane.width, ref_plane.height);
    let centre = (w as f64 / 2.0, h as f64 / 2.0);
    let mut acc = vec![0.0f32; w * h * 3];
    for i in 0..count {
        let (Some(warp), Some(weight)) = (warps[i], weights[i].as_ref()) else { continue };
        let frame = if i == middle { None } else { load(i) };
        let rgb: &[u8] = match (&frame, i == middle) {
            (_, true) => &reference.rgb,
            (Some(f), _) if (f.width, f.height) == (w, h) => &f.rgb,
            _ => continue,
        };
        telescope_core::image::over_rows(&mut acc, w * 3, |y, row| {
            for x in 0..w {
                // the weight where this pixel is on the working picture
                let (wx, wy) = (((x as f64 + 0.5) / factor as f64 - 0.5).clamp(0.0, (ww - 1) as f64), ((y as f64 + 0.5) / factor as f64 - 0.5).clamp(0.0, (wh - 1) as f64));
                let (x0, y0) = (wx.floor() as usize, wy.floor() as usize);
                let (x1, y1) = ((x0 + 1).min(ww - 1), (y0 + 1).min(wh - 1));
                let (fx, fy) = ((wx - x0 as f64) as f32, (wy - y0 as f64) as f32);
                let p = |xx: usize, yy: usize| weight[yy * ww + xx];
                let top = p(x0, y0) + (p(x1, y0) - p(x0, y0)) * fx;
                let bottom = p(x0, y1) + (p(x1, y1) - p(x0, y1)) * fx;
                let wgt = top + (bottom - top) * fy;
                if wgt <= 0.0 {
                    continue;
                }
                let (sx, sy) = warp.at(centre, x as f64, y as f64);
                let (sx, sy) = (sx.clamp(0.0, (w - 1) as f64), sy.clamp(0.0, (h - 1) as f64));
                let (ix, iy) = (sx.floor() as usize, sy.floor() as usize);
                let (jx, jy) = ((ix + 1).min(w - 1), (iy + 1).min(h - 1));
                let (gx, gy) = ((sx - ix as f64) as f32, (sy - iy as f64) as f32);
                for c in 0..3 {
                    let q = |xx: usize, yy: usize| rgb[(yy * w + xx) * 3 + c] as f32;
                    let top = q(ix, iy) + (q(jx, iy) - q(ix, iy)) * gx;
                    let bottom = q(ix, jy) + (q(jx, jy) - q(ix, jy)) * gx;
                    row[x * 3 + c] += wgt * (top + (bottom - top) * gy);
                }
            }
        });
    }
    let out = acc.iter().map(|v| (v + 0.5).clamp(0.0, 255.0) as u8).collect();
    Ok(Image { width: w, height: h, rgb: out })
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

/// A new file next to `first`, named for it with `_focus-merged`, taken by creating it empty.
fn reserve(first: &Path) -> std::io::Result<PathBuf> {
    let stem = first.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "photo".into());
    let folder = first.parent().unwrap_or_else(|| Path::new("."));
    for n in 1.. {
        let name = if n == 1 { format!("{stem}_focus-merged.jpg") } else { format!("{stem}_focus-merged ({n}).jpg") };
        let path = folder.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// What a finished merge tells the plugin: {"ok", "path", "frames", "removed"} or {"ok": false, "error"}.
pub fn merge_files(files: &[String], quality: u8, keep_frames: bool) -> Value {
    let jpegs: Vec<PathBuf> = files.iter().map(PathBuf::from).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"))).collect();
    let fail = |error: &str| json!({"ok": false, "error": error});
    if jpegs.len() < MIN_FRAMES {
        return fail("fewer than two JPEG photos");
    }
    let exif = read_jpeg(&jpegs[jpegs.len() / 2]).and_then(|(_, exif)| exif);
    let merged = match merge(jpegs.len(), &|i| read_jpeg(&jpegs[i]).map(|(image, _)| image)) {
        Ok(merged) => merged,
        Err(e) => return fail(&e),
    };
    let bytes = match encode_jpeg(&merged, quality, exif.as_deref()) {
        Ok(bytes) => bytes,
        Err(e) => return fail(&e),
    };
    let saved = reserve(&jpegs[0]).and_then(|path| match std::fs::write(&path, &bytes) {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(e)
        }
    });
    let path = match saved {
        Ok(path) => path,
        Err(e) => return fail(&e.to_string()),
    };
    let mut removed = 0;
    if !keep_frames {
        removed = files.iter().filter(|f| std::fs::remove_file(f).is_ok()).count();
    }
    json!({"ok": true, "path": path.display().to_string(), "frames": jpegs.len(), "removed": removed})
}

/// Merges on a thread of its own and posts "merged" with what merge_files said.
pub fn spawn(poster: std::sync::Arc<dyn Poster>, files: Vec<String>, quality: u8, keep_frames: bool) {
    let post = poster.clone();
    let started = std::thread::Builder::new().name("focus-merge".into()).spawn(move || {
        let result = std::panic::catch_unwind(|| merge_files(&files, quality, keep_frames)).unwrap_or_else(|_| json!({"ok": false, "error": "it stopped unexpectedly"}));
        post.post("merged", result);
    });
    if started.is_err() {
        poster.post("merged", json!({"ok": false, "error": "no thread to merge on"}));
    }
}

#[cfg(test)]
#[path = "focus_merge_tests.rs"]
mod tests;
