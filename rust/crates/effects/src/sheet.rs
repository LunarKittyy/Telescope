//! Desk mode's maths (plugins/desk.py): four corners marked on the preview, the sheet's real shape worked out from them, the
//! perspective warp that straightens it and the clean-up that makes the writing stand out. Pixels are RGB, 3 bytes each.

use crate::imgops::{grey_of, resize_area};
use crate::pixels::gaussian_blur;
use telescope_core::actions::round_half_even;
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
        for k in col..9 {
            a[col][k] /= d;
        }
        for row in 0..8 {
            if row != col {
                let f = a[row][col];
                for k in col..9 {
                    a[row][k] -= f * a[col][k];
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
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = c(j, i) / det;
        }
    }
    out
}

/// cv2.warpPerspective(img, matrix, (rw, rh), INTER_LINEAR, BORDER_REPLICATE): every pixel of the result samples the picture
/// where the inverse matrix says. OpenCV looks up in 1/32 pixels, which this doesn't.
pub fn warp_perspective(src: &[u8], w: usize, h: usize, matrix: &M3, rw: usize, rh: usize) -> Vec<u8> {
    let inv = invert(matrix);
    let mut out = vec![0u8; rw * rh * 3];
    let (mw, mh) = ((w - 1) as f64, (h - 1) as f64);
    for y in 0..rh {
        for x in 0..rw {
            let (fx, fy) = (x as f64, y as f64);
            let d = inv[2][0] * fx + inv[2][1] * fy + inv[2][2];
            let d = if d != 0.0 { 1.0 / d } else { 0.0 };
            let sx = ((inv[0][0] * fx + inv[0][1] * fy + inv[0][2]) * d).clamp(0.0, mw);
            let sy = ((inv[1][0] * fx + inv[1][1] * fy + inv[1][2]) * d).clamp(0.0, mh);
            let (x0, y0) = (sx.floor() as usize, sy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (ax, ay) = ((sx - x0 as f64) as f32, (sy - y0 as f64) as f32);
            for c in 0..3 {
                let at = |xx: usize, yy: usize| src[(yy * w + xx) * 3 + c] as f32;
                let top = at(x0, y0) * (1.0 - ax) + at(x1, y0) * ax;
                let bottom = at(x0, y1) * (1.0 - ax) + at(x1, y1) * ax;
                out[(y * rw + x) * 3 + c] = (top * (1.0 - ay) + bottom * ay).round() as u8;
            }
        }
    }
    out
}

/// cv2.resize(INTER_LINEAR) of one channel to a bigger size, pixel centres lined up.
fn linear_resize(src: &[u8], w: usize, h: usize, dw: usize, dh: usize) -> Vec<u8> {
    let (sx, sy) = (w as f64 / dw as f64, h as f64 / dh as f64);
    let mut out = vec![0u8; dw * dh];
    for y in 0..dh {
        let fy = ((y as f64 + 0.5) * sy - 0.5).clamp(0.0, (h - 1) as f64);
        let (y0, ay) = (fy.floor() as usize, (fy - fy.floor()) as f32);
        let y1 = (y0 + 1).min(h - 1);
        for x in 0..dw {
            let fx = ((x as f64 + 0.5) * sx - 0.5).clamp(0.0, (w - 1) as f64);
            let (x0, ax) = (fx.floor() as usize, (fx - fx.floor()) as f32);
            let x1 = (x0 + 1).min(w - 1);
            let at = |xx: usize, yy: usize| src[yy * w + xx] as f32;
            let v = (at(x0, y0) * (1.0 - ax) + at(x1, y0) * ax) * (1.0 - ay) + (at(x0, y1) * (1.0 - ax) + at(x1, y1) * ax) * ay;
            out[y * dw + x] = v.round() as u8;
        }
    }
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
        let plane: Vec<u8> = sheet.chunks_exact(3).map(|p| p[c]).collect();
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
        let blurred = gaussian_blur(&dilated, sw, sh, 2.0);
        paper_map.push(linear_resize(&blurred, sw, sh, w, h));
    }
    let scale = 255.0 * INK;
    let mut out = vec![0u8; sheet.len()];
    for i in 0..w * h {
        for c in 0..3 {
            let p = paper_map[c][i] as f64;
            out[i * 3 + c] = if p == 0.0 { 0 } else { (sheet[i * 3 + c] as f64 * scale / p).round().clamp(0.0, 255.0) as u8 };
        }
    }
    out
}

/// cv2.cvtColor(BGR2GRAY) of a sheet held as R, G, B: OpenCV's whole-number weights, not the float ones.
fn grey_plane(sheet: &[u8]) -> Vec<u8> {
    sheet.chunks_exact(3).map(|p| grey_of(p[0], p[1], p[2])).collect()
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
    let mut out = vec![if cleaned { PAPER } else { BACKDROP }; w * h * 3];
    for row in 0..rh {
        out[((y + row) * w + x) * 3..][..rw * 3].copy_from_slice(&flat[row * rw * 3..][..rw * 3]);
    }
    out
}

#[cfg(test)]
#[path = "sheet_tests.rs"]
pub(crate) mod tests;
