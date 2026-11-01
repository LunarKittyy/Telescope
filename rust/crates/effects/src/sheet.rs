//! Desk mode's maths (plugins/desk.py): four corners marked on the preview, the sheet's real shape worked out from them, the
//! perspective warp that straightens it and the clean-up that makes the writing stand out. Pixels are RGB, 3 bytes each.

use crate::imgops::{grey_of, resize_area, Gray};
use telescope_core::actions::round_half_even;
use telescope_core::image::{over_rows, rint, spare_bytes};
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
const BACKDROP: u8 = 28;
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

/// Positions are looked up in 1/32 pixels (INTER_BITS 5) and blended with 15-bit weights (INTER_REMAP_COEF_BITS).
const TAB_BITS: u32 = 5;
const TAB_SIZE: i32 = 1 << TAB_BITS;
const COEF_BITS: u32 = 15;

/// cv2.warpPerspective(img, matrix, (rw, rh), INTER_LINEAR, BORDER_REPLICATE) on an 8-bit, 3-channel picture, the way OpenCV does it:
/// the inverse matrix is applied in blocks of at most 32 x 32 pixels (so the sums round as they do there), each position is taken to
/// the nearest 1/32 pixel, and the four pixels around it are blended with whole-number weights.
pub fn warp_perspective(src: &[u8], w: usize, h: usize, matrix: &M3, rw: usize, rh: usize) -> Vec<u8> {
    let inv = invert_like_opencv(matrix);
    let m = [inv[0][0], inv[0][1], inv[0][2], inv[1][0], inv[1][1], inv[1][2], inv[2][0], inv[2][1], inv[2][2]];
    let mut out = vec![0u8; rw * rh * 3];
    // OpenCV's block: 32 x 32 = 1024 pixels, 16 rows tall when the picture allows. Its width sets where the rounding starts over
    let bh0 = 16.min(rh);
    let bw0 = (1024 / bh0).min(rw);
    let (last_x, last_y) = (w as i64 - 1, h as i64 - 1);
    over_rows(&mut out, rw * 3, |y, line| {
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
                let sx = (fx >> TAB_BITS).clamp(i16::MIN as i32, i16::MAX as i32) as i64;
                let sy = (fy >> TAB_BITS).clamp(i16::MIN as i32, i16::MAX as i32) as i64;
                let (ax, ay) = (fx & (TAB_SIZE - 1), fy & (TAB_SIZE - 1));
                // The weights are exact: products of two 1/32 steps, in units of 1/32768
                let weights = [(TAB_SIZE - ay) * (TAB_SIZE - ax), (TAB_SIZE - ay) * ax, ay * (TAB_SIZE - ax), ay * ax].map(|v| v << (COEF_BITS - 2 * TAB_BITS));
                // BORDER_REPLICATE: a pixel outside the picture is the nearest one in it
                let (x_a, x_b) = (sx.clamp(0, last_x) as usize, (sx + 1).clamp(0, last_x) as usize);
                let (y_a, y_b) = (sy.clamp(0, last_y) as usize, (sy + 1).clamp(0, last_y) as usize);
                for c in 0..3 {
                    let at = |xx: usize, yy: usize| src[(yy * w + xx) * 3 + c] as i32;
                    let sum = at(x_a, y_a) * weights[0] + at(x_b, y_a) * weights[1] + at(x_a, y_b) * weights[2] + at(x_b, y_b) * weights[3];
                    line[(x + x1) * 3 + c] = ((sum + (1 << (COEF_BITS - 1))) >> COEF_BITS).clamp(0, 255) as u8;
                }
            }
            x += bw0;
        }
    });
    out
}

/// The 5 x 5 ellipse OpenCV makes for MORPH_ELLIPSE: three full middle rows and one pixel above and below.
const ELLIPSE: [(i32, i32); 17] = [
    (0, -2), (-2, -1), (-1, -1), (0, -1), (1, -1), (2, -1), (-2, 0), (-1, 0), (0, 0), (1, 0), (2, 0), (-2, 1), (-1, 1), (0, 1), (1, 1), (2, 1),
    (0, 2),
];

/// The paper's uneven light divided out, so it goes white and the ink stays dark.
pub fn whiten(sheet: &[u8], w: usize, h: usize) -> Vec<u8> {
    let (sw, sh) = ((w / SHADE_SMALL).max(1), (h / SHADE_SMALL).max(1));
    let mut paper_map: Vec<Vec<u8>> = Vec::new();
    for c in 0..3 {
        let plane: Vec<u8> = sheet.as_chunks::<3>().0.iter().map(|p| p[c]).collect();
        let small = resize_area(&plane, w, h, 1, sw, sh);
        // Ink is thin: the max is paper
        let mut dilated = vec![0u8; sw * sh];
        for y in 0..sh as i32 {
            for x in 0..sw as i32 {
                dilated[y as usize * sw + x as usize] = ELLIPSE.iter().filter_map(|(dx, dy)| {
                    let (xx, yy) = (x + dx, y + dy);
                    (xx >= 0 && yy >= 0 && xx < sw as i32 && yy < sh as i32).then(|| small[yy as usize * sw + xx as usize])
                }).max().unwrap_or(0);
            }
        }
        paper_map.push(Gray { w: sw, h: sh, data: dilated }.blur(2.0).linear_exact(w, h).into_data());
    }
    // cv2.divide in floats, a zero below giving zero
    let scale = (255.0 * INK) as f32;
    let mut out = vec![0u8; sheet.len()];
    for i in 0..w * h {
        for c in 0..3 {
            let p = paper_map[c][i] as f32;
            out[i * 3 + c] = if p == 0.0 { 0 } else { (sheet[i * 3 + c] as f32 * scale / p).round_ties_even().clamp(0.0, 255.0) as u8 };
        }
    }
    out
}

/// cv2.cvtColor(BGR2GRAY) of a sheet held as R, G, B: OpenCV's whole-number weights, not the float ones.
fn grey_plane(sheet: &[u8]) -> Vec<u8> {
    sheet.as_chunks::<3>().0.iter().map(|p| grey_of(p[0], p[1], p[2])).collect()
}

/// Ink black, paper white: an adaptive threshold that follows the light across the sheet.
pub fn black_and_white(sheet: &[u8], w: usize, h: usize) -> Vec<u8> {
    let grey = grey_plane(sheet);
    let block = 15usize.max((w.min(h) / 24) | 1);
    let half = (block / 2) as i64;
    // A summed-area table, so the mean of each block costs the same wherever it is; the edge repeats the pixel it ends on
    let mut sums = vec![0u64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0u64;
        for x in 0..w {
            row += grey[y * w + x] as u64;
            sums[(y + 1) * (w + 1) + x + 1] = sums[y * (w + 1) + x + 1] + row;
        }
    }
    let area = (block * block) as u64;
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let mut total = 0u64;
            // Along each axis: the part of the window inside the picture, and how many times the edge repeats on each side
            let axis = |i: i64, n: i64| -> (i64, i64, i64, i64) {
                let (lo, hi) = (i - half, i + half);
                (lo.max(0), hi.min(n - 1), (-lo).max(0), (hi - (n - 1)).max(0))
            };
            let (x0, x1, lx, rx) = axis(x, w as i64);
            let (y0, y1, ty, by) = axis(y, h as i64);
            let rect = |xa: i64, xb: i64, ya: i64, yb: i64| -> u64 {
                let at = |xx: i64, yy: i64| sums[yy as usize * (w + 1) + xx as usize];
                at(xb + 1, yb + 1) + at(xa, ya) - at(xb + 1, ya) - at(xa, yb + 1)
            };
            // Columns and rows repeated at the edges count once for every time they repeat
            let (mut cols, mut rows) = (Vec::new(), Vec::new());
            cols.push((x0, x1, 1u64));
            if lx > 0 {
                cols.push((0, 0, lx as u64));
            }
            if rx > 0 {
                cols.push((w as i64 - 1, w as i64 - 1, rx as u64));
            }
            rows.push((y0, y1, 1u64));
            if ty > 0 {
                rows.push((0, 0, ty as u64));
            }
            if by > 0 {
                rows.push((h as i64 - 1, h as i64 - 1, by as u64));
            }
            for (ya, yb, ry) in &rows {
                for (xa, xb, cx) in &cols {
                    total += rect(*xa, *xb, *ya, *yb) * ry * cx;
                }
            }
            let mean = (total + area / 2) / area;
            let v = grey[y as usize * w + x as usize] as i64;
            let on = if v > mean as i64 - 12 { 255 } else { 0 };
            out[(y as usize * w + x as usize) * 3..][..3].fill(on);
        }
    }
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
    let (x, y, rw, rh) = warp.rect;
    let mut flat = warp_perspective(rgb, w, h, &warp.matrix, rw, rh);
    let cleaned = match sheet.cleanup.as_str() {
        "contrast" => {
            flat = whiten(&flat, rw, rh);
            true
        }
        "bw" => {
            flat = black_and_white(&flat, rw, rh);
            true
        }
        _ => false,
    };
    if (rw, rh) == (w, h) {
        return flat;
    }
    let mut out = spare_bytes(w * h * 3);
    out.fill(if cleaned { PAPER } else { BACKDROP });
    for row in 0..rh {
        out[((y + row) * w + x) * 3..][..rw * 3].copy_from_slice(&flat[row * rw * 3..][..rw * 3]);
    }
    out
}

#[cfg(test)]
#[path = "sheet_tests.rs"]
pub(crate) mod tests;
