//! Desk mode's maths (plugins/desk.py): four corners marked on the preview, the sheet's real shape worked out from them, the
//! perspective warp that straightens it and the clean-up that makes the writing stand out. Pixels are RGB, 3 bytes each.

use crate::imgops::{gauss_exact, grey_of, resize_area, resize_linear_fixed, Remap};
use telescope_core::actions::round_half_even;
use telescope_core::avx2_too;
use telescope_core::image::{keep_bytes, over_rows, rint, spare_bytes};
use telescope_core::view::{view_to_shown, Geometry};

/// Width over height of a paper size, standing up.
pub fn paper(shape: &str) -> Option<f64> {
    match shape {
        "a4" => Some(210.0 / 297.0),
        "letter" => Some(8.5 / 11.0),
        "square" => Some(1.0),
        _ => None,
    }
}

pub const STRETCH_MIN: i32 = 50;
pub const STRETCH_MAX: i32 = 200;
/// A phone's main camera: focal length over the picture's long side (about 70 degrees across).
pub const FOCAL: f64 = 0.71;
/// BGR around a sheet that keeps its shape, as grey (all three are the same).
pub const BACKDROP: u8 = 28;
const PAPER: u8 = 255;
/// The paper's shading is worked out at an eighth of the size.
const SHADE_SMALL: usize = 8;
/// The sheet is cut this much inside the corners as marked.
const INSET: f64 = 0.985;
/// Contrast: the paper's brightness is divided out, then scaled by this so ink stays dark.
const INK: f64 = 0.92;
/// A sheet covering less than this share of the picture is corners bunched up, not a sheet.
const MIN_AREA: f64 = 0.01;
/// Degrees: a sharper corner, or one this close to flat, means corners in a line.
const MIN_ANGLE: f64 = 15.0;

pub type P = [f64; 2];

/// Four points as top left, top right, bottom right, bottom left, however they were placed.
pub fn order_corners(points: &[P; 4]) -> [P; 4] {
    let centre = [points.iter().map(|p| p[0]).sum::<f64>() / 4.0, points.iter().map(|p| p[1]).sum::<f64>() / 4.0];
    let mut pts = *points;
    // Clockwise on screen, since y points down
    pts.sort_by(|a, b| (a[1] - centre[1]).atan2(a[0] - centre[0]).total_cmp(&(b[1] - centre[1]).atan2(b[0] - centre[0])));
    let start = (0..4).min_by(|&i, &j| (pts[i][0] + pts[i][1]).total_cmp(&(pts[j][0] + pts[j][1]))).unwrap_or(0);
    [pts[start], pts[(start + 1) % 4], pts[(start + 2) % 4], pts[(start + 3) % 4]]
}

/// Whether four points (0..1 of the picture) outline a sheet that can be straightened: not bunched up, crossed or lined up,
/// which would give a warp of garbage.
pub fn usable_corners(points: &[P; 4]) -> bool {
    let pts = order_corners(points);
    let edge = |i: usize| [pts[(i + 1) % 4][0] - pts[i][0], pts[(i + 1) % 4][1] - pts[i][1]];
    let edges: Vec<P> = (0..4).map(edge).collect();
    let turns: Vec<f64> = (0..4).map(|i| edges[i][0] * edges[(i + 1) % 4][1] - edges[i][1] * edges[(i + 1) % 4][0]).collect();
    let area = (0..4).map(|i| pts[i][0] * pts[(i + 1) % 4][1] - pts[(i + 1) % 4][0] * pts[i][1]).sum::<f64>().abs() / 2.0;
    if area < MIN_AREA || !(turns.iter().all(|t| *t > 0.0) || turns.iter().all(|t| *t < 0.0)) {
        return false;
    }
    let len = |e: P| (e[0] * e[0] + e[1] * e[1]).sqrt();
    for i in 0..4 {
        let (before, after) = (edges[(i + 3) % 4], edges[i]);
        let before = [-before[0], -before[1]];
        let cos = (before[0] * after[0] + before[1] * after[1]) / (len(edges[(i + 3) % 4]) * len(edges[i])).max(1e-12);
        let angle = cos.clamp(-1.0, 1.0).acos().to_degrees();
        if !(MIN_ANGLE..=180.0 - MIN_ANGLE).contains(&angle) {
            return false;
        }
    }
    true
}

/// (width, height) of the sheet in pixels, from the average of its opposite edges.
pub fn sheet_size(c: &[P; 4]) -> (f64, f64) {
    let d = |a: P, b: P| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    (((d(c[1], c[0]) + d(c[2], c[3])) / 2.0).max(1.0), ((d(c[3], c[0]) + d(c[2], c[1])) / 2.0).max(1.0))
}

type V3 = [f64; 3];

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The real sheet's width over height from its ordered corners (px), seen by a camera with its middle at `centre` and about this
/// focal length (px). Undoes the perspective (Zhang and He's whiteboard method), where the average of opposite edges takes a
/// sheet tilted away for a much wider one.
pub fn sheet_aspect(corners: &[P; 4], centre: P, focal: f64) -> f64 {
    let [tl, tr, br, bl] = corners.map(|p| [p[0], p[1], 1.0]);
    let k2 = dot(cross(tl, br), bl) / dot(cross(tr, br), bl);
    let k3 = dot(cross(tl, br), tr) / dot(cross(bl, br), tr);
    if !(k2.is_finite() && k3.is_finite() && k2 > 0.0 && k3 > 0.0) {
        let (sw, sh) = sheet_size(corners);
        return sw / sh;
    }
    // The sheet's top and left edges in 3D, up to the camera's matrix
    let n2: V3 = [k2 * tr[0] - tl[0], k2 * tr[1] - tl[1], k2 * tr[2] - tl[2]];
    let n3: V3 = [k3 * bl[0] - tl[0], k3 * bl[1] - tl[1], k3 * bl[2] - tl[2]];
    let (u0, v0) = (centre[0], centre[1]);
    let mut focal = focal;
    // Converging edges give the focal length too (a zoomed lens), trusted only near the guess as it jumps around
    if (n2[2] * n3[2]).abs() > 1e-12 {
        let f2 = -((n2[0] * n3[0] - (n2[0] * n3[2] + n2[2] * n3[0]) * u0 + n2[2] * n3[2] * u0 * u0)
            + (n2[1] * n3[1] - (n2[1] * n3[2] + n2[2] * n3[1]) * v0 + n2[2] * n3[2] * v0 * v0)) / (n2[2] * n3[2]);
        if 0.25 * focal * focal < f2 && f2 < 16.0 * focal * focal {
            focal = f2.sqrt();
        }
    }
    let length2 = |n: V3| ((n[0] - u0 * n[2]) / focal).powi(2) + ((n[1] - v0 * n[2]) / focal).powi(2) + n[2] * n[2];
    (length2(n2) / length2(n3).max(1e-12)).sqrt().clamp(0.1, 10.0)
}

/// Width over height the sheet comes out at: the estimate, or a paper size turned the way the sheet lies, times the card's
/// stretch (percent). None fills the whole picture.
pub fn output_aspect(estimate: f64, shape: &str, stretch: [i32; 2]) -> Option<f64> {
    if shape == "fill" {
        return None;
    }
    let aspect = match paper(shape) {
        None => estimate,
        Some(size) => if estimate < 1.0 { size } else { 1.0 / size },
    };
    Some(aspect * stretch[0] as f64 / stretch[1] as f64)
}

/// (x, y, width, height) the sheet takes in a w x h frame: as big as fits at aspect and centred, or all of it.
pub fn target_rect(aspect: Option<f64>, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let Some(aspect) = aspect else { return (0, 0, w, h) };
    let (wf, hf) = (w as f64, h as f64);
    let (rw, rh) = if wf / aspect <= hf { (wf, wf / aspect) } else { (hf * aspect, hf) };
    let rw = (round_half_even(rw) as usize).clamp(2.min(w), w).max(2);
    let rh = (round_half_even(rh) as usize).clamp(2.min(h), h).max(2);
    ((w - rw) / 2, (h - rh) / 2, rw, rh)
}

pub type M3 = [[f64; 3]; 3];

/// cv2.getPerspectiveTransform: the matrix taking each of four points to the one in the same place of `to`.
pub fn perspective(from: &[P; 4], to: &[P; 4]) -> M3 {
    // Eight unknowns, h33 = 1: x' = (h11 x + h12 y + h13) / (h31 x + h32 y + 1), y' likewise
    let mut a = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let ([x, y], [u, v]) = (from[i], to[i]);
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -x * u, -y * u, u];
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -x * v, -y * v, v];
    }
    for col in 0..8 {
        let pivot = (col..8).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs())).unwrap_or(col);
        a.swap(col, pivot);
        let d = a[col][col];
        if d.abs() < 1e-12 {
            continue;
        }
        for v in &mut a[col][col..] {
            *v /= d;
        }
        for row in 0..8 {
            if row != col {
                let f = a[row][col];
                let pivot_row = a[col];
                for (v, p) in a[row][col..].iter_mut().zip(&pivot_row[col..]) {
                    *v -= f * p;
                }
            }
        }
    }
    let h = |i: usize| a[i][8];
    [[h(0), h(1), h(2)], [h(3), h(4), h(5)], [h(6), h(7), 1.0]]
}

pub fn mul(a: &M3, b: &M3) -> M3 {
    let mut out = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

pub fn invert(m: &M3) -> M3 {
    let c = |r: usize, c: usize| m[(r + 1) % 3][(c + 1) % 3] * m[(r + 2) % 3][(c + 2) % 3] - m[(r + 1) % 3][(c + 2) % 3] * m[(r + 2) % 3][(c + 1) % 3];
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = c(j, i) / det;
        }
    }
    out
}

/// cv2.invert(m) for a 3 x 3 of doubles, which OpenCV does by cofactors times the reciprocal of the determinant (zeros when singular).
fn invert_like_opencv(m: &M3) -> M3 {
    let d = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if d == 0.0 {
        return [[0.0; 3]; 3];
    }
    let d = 1.0 / d;
    [
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ]
}

/// Positions are looked up in 1/32 pixels (INTER_BITS 5).
const TAB_BITS: u32 = 5;
const TAB_SIZE: i32 = 1 << TAB_BITS;

/// Where cv2.warpPerspective(.., (rw, rh), INTER_LINEAR, BORDER_REPLICATE) looks for each pixel, for frames to share until the corners move.
pub fn warp_map(w: usize, h: usize, matrix: &M3, rw: usize, rh: usize) -> Remap {
    let inv = invert_like_opencv(matrix);
    let m = [inv[0][0], inv[0][1], inv[0][2], inv[1][0], inv[1][1], inv[1][2], inv[2][0], inv[2][1], inv[2][2]];
    let mut places = vec![(0, 0); rw * rh];
    // OpenCV's block: 32 x 32 = 1024 pixels, 16 rows tall when the picture allows. Its width sets where the rounding starts over
    let bh0 = 16.min(rh);
    let bw0 = (1024 / bh0).min(rw);
    over_rows(&mut places, rw, |y, line| {
        let row = y as f64;
        let mut x = 0;
        while x < rw {
            let bw = bw0.min(rw - x);
            let x0 = m[0] * x as f64 + m[1] * row + m[2];
            let y0 = m[3] * x as f64 + m[4] * row + m[5];
            let w0 = m[6] * x as f64 + m[7] * row + m[8];
            for x1 in 0..bw {
                let step = x1 as f64;
                let wd = w0 + m[6] * step;
                let wd = if wd != 0.0 { TAB_SIZE as f64 / wd } else { 0.0 };
                let fx = rint(((x0 + m[0] * step) * wd).clamp(i32::MIN as f64, i32::MAX as f64));
                let fy = rint(((y0 + m[3] * step) * wd).clamp(i32::MIN as f64, i32::MAX as f64));
                // The position as a short, and what is left of it in 1/32 pixels
                let limit = |v: i32| (v >> TAB_BITS).clamp(i16::MIN as i32, i16::MAX as i32) << TAB_BITS | (v & (TAB_SIZE - 1));
                line[x + x1] = (limit(fx), limit(fy));
            }
            x += bw0;
        }
    });
    Remap::new(rw, rh, w, h, &places)
}

/// cv2.warpPerspective(img, matrix, (rw, rh), INTER_LINEAR, BORDER_REPLICATE) on an 8-bit, 3-channel picture.
pub fn warp_perspective(src: &[u8], w: usize, h: usize, matrix: &M3, rw: usize, rh: usize) -> Vec<u8> {
    let mut out = spare_bytes(rw * rh * 3);
    warp_map(w, h, matrix, rw, rh).run(src, &mut out);
    out
}

avx2_too! {
    fn most_of_five(a: &[u8], b: &[u8], c: &[u8], d: &[u8], e: &[u8], out: &mut [u8]) {
        for (o, (((a, b), (c, d)), e)) in out.iter_mut().zip(a.iter().zip(b).zip(c.iter().zip(d)).zip(e)) {
            *o = (*a).max(*b).max(*c).max(*d).max(*e);
        }
    }
}

/// Each value's largest neighbour within two pixels along a row of `ch` to a pixel, as if the row went on in zeros.
fn most_nearby(row: &[u8], ch: usize, out: &mut [u8]) {
    let zero = vec![0u8; 2 * ch];
    let mut padded = zero.clone();
    padded.extend_from_slice(row);
    padded.extend_from_slice(&zero);
    let at = |n: usize| &padded[n * ch..][..row.len()];
    most_of_five(at(0), at(1), at(2), at(3), at(4), out);
}

/// cv2.dilate with OpenCV's 5 x 5 MORPH_ELLIPSE: three full middle rows and one pixel above and below.
fn dilate_ellipse(src: &[u8], w: usize, h: usize, ch: usize) -> Vec<u8> {
    let rl = w * ch;
    let mut wide = vec![0u8; rl * h];
    wide.chunks_exact_mut(rl).zip(src.chunks_exact(rl)).for_each(|(o, row)| most_nearby(row, ch, o));
    let none = vec![0u8; rl];
    let row = |of: usize, at: isize| -> &[u8] {
        match (at >= 0 && (at as usize) < h, of) {
            (false, _) => &none,
            (true, 0) => &src[at as usize * rl..][..rl],
            (true, _) => &wide[at as usize * rl..][..rl],
        }
    };
    let mut out = vec![0u8; rl * h];
    for (y, line) in out.chunks_exact_mut(rl).enumerate() {
        let y = y as isize;
        most_of_five(row(0, y - 2), row(1, y - 1), row(1, y), row(1, y + 1), row(0, y + 2), line);
    }
    out
}

avx2_too! {
    /// cv2.divide in floats, a zero below giving zero.
    fn over_paper(sheet: &[u8], paper: &[u8], scale: f32, out: &mut [u8]) {
        for (o, (s, p)) in out.iter_mut().zip(sheet.iter().zip(paper)) {
            let d = *p as f32;
            let v = (*s as f32 * scale / d).round_ties_even().clamp(0.0, 255.0);
            *o = if *p == 0 { 0 } else { v as i32 as u8 };
        }
    }
}

/// The paper's uneven light divided out, so it goes white and the ink stays dark.
pub fn whiten(sheet: &[u8], w: usize, h: usize) -> Vec<u8> {
    let (sw, sh) = ((w / SHADE_SMALL).max(1), (h / SHADE_SMALL).max(1));
    let small = resize_area(sheet, w, h, 3, sw, sh);
    // Ink is thin: the max is paper
    let dilated = dilate_ellipse(&small, sw, sh, 3);
    let paper = resize_linear_fixed(&gauss_exact(&dilated, sw, sh, 3, 2.0), sw, sh, 3, w, h);
    let scale = (255.0 * INK) as f32;
    let mut out = spare_bytes(sheet.len());
    let rl = w * 3;
    over_rows(&mut out, rl, |y, line| over_paper(&sheet[y * rl..][..rl], &paper[y * rl..][..rl], scale, line));
    keep_bytes(small);
    keep_bytes(paper);
    out
}

/// cv2.cvtColor(BGR2GRAY) of a sheet held as R, G, B: OpenCV's whole-number weights, not the float ones.
fn grey_plane(sheet: &[u8]) -> Vec<u8> {
    sheet.as_chunks::<3>().0.iter().map(|p| grey_of(p[0], p[1], p[2])).collect()
}

avx2_too! {
    fn slide_columns(leaving: &[u8], coming: &[u8], sums: &mut [u32]) {
        for (s, (l, c)) in sums.iter_mut().zip(leaving.iter().zip(coming)) {
            *s = *s + *c as u32 - *l as u32;
        }
    }
}

/// Ink black, paper white: an adaptive threshold that follows the light across the sheet.
pub fn black_and_white(sheet: &[u8], w: usize, h: usize) -> Vec<u8> {
    let grey = grey_plane(sheet);
    let block = 15usize.max((w.min(h) / 24) | 1);
    let half = block / 2;
    let area = (block * block) as u32;
    let row_at = |y: isize| &grey[y.clamp(0, h as isize - 1) as usize * w..][..w];
    let grey = &grey;
    let mut out = spare_bytes(w * h * 3);
    // Column sums slide down and a window slides across them, the edges repeating the pixel they end on
    telescope_core::image::over_bands(&mut out, w * 3, |first, part| {
        let mut columns = vec![0u32; w];
        for j in 0..block as isize {
            let row = row_at(first as isize + j - half as isize);
            columns.iter_mut().zip(row).for_each(|(s, v)| *s += *v as u32);
        }
        let rows = part.len() / (w * 3);
        for (i, line) in part.chunks_exact_mut(w * 3).enumerate() {
            let y = first + i;
            let column = |x: isize| columns[x.clamp(0, w as isize - 1) as usize];
            let mut total: u32 = (0..block as isize).map(|j| column(j - half as isize)).sum();
            for (x, o) in line.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                // v > the rounded mean - 12, with no division
                let on = (grey[y * w + x] as u32 + 12) * area > total + area / 2;
                *o = [if on { 255 } else { 0 }; 3];
                total = total + column((x + half + 1) as isize) - column(x as isize - half as isize);
            }
            if i + 1 < rows {
                let top = y as isize - half as isize;
                slide_columns(row_at(top), row_at(top + block as isize), &mut columns);
            }
        }
    });
    out
}

/// Where the sheet goes and how, for one frame size.
#[derive(Clone, Debug, PartialEq)]
pub struct Warp {
    pub matrix: M3,
    /// (x, y, width, height) the sheet takes in the frame.
    pub rect: (usize, usize, usize, usize),
    /// The same warp from the frame as shown to what comes out, both 0..1, for the preview.
    pub on_preview: M3,
}

/// What a warp is worked out from.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    /// Four points in the whole picture.
    pub corners: [P; 4],
    pub cleanup: String,
    pub shape: String,
    pub stretch: [i32; 2],
    pub geometry: Geometry,
}

/// The straightening for a w x h frame.
pub fn warp_for(sheet: &Sheet, w: usize, h: usize) -> Warp {
    let (wf, hf) = (w as f64, h as f64);
    let shown = sheet.corners.map(|[x, y]| view_to_shown(x, y, &sheet.geometry));
    let mut corners = order_corners(&shown.map(|(u, v)| [u * wf, v * hf]));
    // The lens's middle, wherever zoom and pan moved it
    let middle = view_to_shown(0.5, 0.5, &sheet.geometry);
    let focal = FOCAL * wf.max(hf) * sheet.geometry.zoom.max(1.0);
    let estimate = sheet_aspect(&corners, [middle.0 * wf, middle.1 * hf], focal);
    let (x, y, rw, rh) = target_rect(output_aspect(estimate, &sheet.shape, sheet.stretch), w, h);
    let centre = [corners.iter().map(|p| p[0]).sum::<f64>() / 4.0, corners.iter().map(|p| p[1]).sum::<f64>() / 4.0];
    // A corner a pixel off would bring the desk's edge along
    corners = corners.map(|p| [centre[0] + (p[0] - centre[0]) * INSET, centre[1] + (p[1] - centre[1]) * INSET]);
    let dest = [[0.0, 0.0], [rw as f64, 0.0], [rw as f64, rh as f64], [0.0, rh as f64]];
    let matrix = perspective(&corners, &dest);
    let moved = mul(&[[1.0, 0.0, x as f64], [0.0, 1.0, y as f64], [0.0, 0.0, 1.0]], &matrix);
    let on_preview = mul(&mul(&[[1.0 / wf, 0.0, 0.0], [0.0, 1.0 / hf, 0.0], [0.0, 0.0, 1.0]], &moved), &[[wf, 0.0, 0.0], [0.0, hf, 0.0], [0.0, 0.0, 1.0]]);
    Warp { matrix, rect: (x, y, rw, rh), on_preview }
}

/// The picture with the sheet straightened (and cleaned up), laid on the frame's own size.
pub fn straighten(rgb: &[u8], w: usize, h: usize, sheet: &Sheet, warp: &Warp) -> Vec<u8> {
    let (_, _, rw, rh) = warp.rect;
    straighten_with(rgb, w, h, sheet, warp, &warp_map(w, h, &warp.matrix, rw, rh))
}

/// The sheet straightened with no cleanup: each row is remapped straight into the frame-sized picture, with only the borders filled.
fn straightened_in_place(rgb: &[u8], w: usize, h: usize, warp: &Warp, map: &Remap) -> Vec<u8> {
    let (x, y, rw, rh) = warp.rect;
    let mut out = spare_bytes(w * h * 3);
    over_rows(&mut out, w * 3, |row, line| match row.checked_sub(y).filter(|r| *r < rh) {
        Some(r) => {
            let (left, rest) = line.split_at_mut(x * 3);
            let (middle, right) = rest.split_at_mut(rw * 3);
            left.fill(BACKDROP);
            right.fill(BACKDROP);
            map.run_row(rgb, r, middle);
        }
        None => line.fill(BACKDROP),
    });
    out
}

/// The warp as the card wants it: the inverse of the matrix (as OpenCV works it out) in three rows of four, so a pixel of the sheet's rectangle finds its place in the picture.
pub fn card_rows(warp: &Warp) -> [[f64; 4]; 3] {
    let inv = invert_like_opencv(&warp.matrix);
    inv.map(|r| [r[0], r[1], r[2], 0.0])
}

/// straighten with its warp_map already made.
pub fn straighten_with(rgb: &[u8], w: usize, h: usize, sheet: &Sheet, warp: &Warp, map: &Remap) -> Vec<u8> {
    let (x, y, rw, rh) = warp.rect;
    if !matches!(sheet.cleanup.as_str(), "contrast" | "bw") {
        return straightened_in_place(rgb, w, h, warp, map);
    }
    let mut flat = spare_bytes(rw * rh * 3);
    map.run(rgb, &mut flat);
    flat = if sheet.cleanup == "contrast" { whiten(&flat, rw, rh) } else { black_and_white(&flat, rw, rh) };
    if (rw, rh) == (w, h) {
        return flat;
    }
    let mut out = spare_bytes(w * h * 3);
    out.fill(PAPER);
    for row in 0..rh {
        out[((y + row) * w + x) * 3..][..rw * 3].copy_from_slice(&flat[row * rw * 3..][..rw * 3]);
    }
    keep_bytes(flat);
    out
}

#[cfg(test)]
#[path = "sheet_tests.rs"]
pub(crate) mod tests;
