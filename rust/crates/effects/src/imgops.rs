//! The few OpenCV operations the CPU effects use, on 8-bit pictures: scaling (INTER_AREA, INTER_LINEAR, INTER_NEAREST), blurs,
//! the morphology a mask is tidied with and the compose that lays one picture over another by a mask. They follow OpenCV's
//! sampling (pixel centres, repeated or reflected edges) so a picture made here lines up with the one Python makes.

use crate::cpu::Rgb;

/// A single-channel 8-bit picture: a mask, or brightness.
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Gray {
    pub fn new(w: usize, h: usize) -> Gray {
        Gray { w, h, data: vec![0; w * h] }
    }

    pub fn filled(w: usize, h: usize, v: u8) -> Gray {
        Gray { w, h, data: vec![v; w * h] }
    }
}

fn round8(v: f32) -> u8 {
    v.round_ties_even().clamp(0.0, 255.0) as u8
}

/// The source cells each of `m` output cells covers along an axis of `n`, with their weights (OpenCV's area tables, in f64 and
/// stored as f32): used when both axes shrink by a ratio that isn't a whole number.
fn area_table(n: usize, m: usize, scale: f64) -> Vec<Vec<(usize, f32)>> {
    (0..m).map(|d| {
        let f1 = d as f64 * scale;
        let f2 = f1 + scale;
        let cell = scale.min(n as f64 - f1);
        let s2 = (f2.floor() as usize).min(n - 1);
        let s1 = (f1.ceil() as usize).min(s2);
        let mut row = Vec::new();
        if s1 as f64 - f1 > 1e-3 {
            row.push((s1 - 1, ((s1 as f64 - f1) / cell) as f32));
        }
        for s in s1..s2 {
            row.push((s, (1.0 / cell) as f32));
        }
        if f2 - s2 as f64 > 1e-3 {
            row.push((s2, ((f2 - s2 as f64).min(1.0).min(cell) / cell) as f32));
        }
        row
    }).collect()
}

/// The two taps of OpenCV's area-mode bilinear along an axis: (first source, second source, their weights out of 2048).
fn area_taps(n: usize, m: usize, inv: f64, scale: f64, horizontal: bool) -> Vec<(usize, usize, i64, i64)> {
    (0..m).map(|d| {
        let d = d as f64;
        let s = (d * scale).floor() as i64;
        let f = ((d + 1.0) - (s as f64 + 1.0) * inv) as f32;
        let mut f = if f <= 0.0 { 0.0 } else { f - f.floor() };
        if horizontal && s >= n as i64 - 1 {
            f = 0.0;
        }
        let (p, q) = ((1.0f32 - f) * 2048.0f32, f * 2048.0f32);
        ((s.clamp(0, n as i64 - 1)) as usize, ((s + 1).clamp(0, n as i64 - 1)) as usize, p.round_ties_even() as i64, q.round_ties_even() as i64)
    }).collect()
}

/// cv2.resize(INTER_AREA) of an 8-bit picture with `ch` channels, whatever the sizes (OpenCV 4.13): an exact copy at the same size,
/// the area average when both sides shrink, and for anything else (growing, or one side each way) a fixed-point two-tap blend.
pub fn resize_area(src: &[u8], w: usize, h: usize, ch: usize, w2: usize, h2: usize) -> Vec<u8> {
    if (w2, h2) == (w, h) {
        return src.to_vec();
    }
    let (ix, iy) = (w2 as f64 / w as f64, h2 as f64 / h as f64);
    let (sx, sy) = (1.0 / ix, 1.0 / iy);
    let mut out = vec![0u8; w2 * h2 * ch];
    if sx >= 1.0 && sy >= 1.0 {
        let (kx, ky) = (sx.round_ties_even() as usize, sy.round_ties_even() as usize);
        if (sx - kx as f64).abs() < f64::EPSILON && (sy - ky as f64).abs() < f64::EPSILON && w == w2 * kx && h == h2 * ky {
            // whole blocks: integer sums
            let recip = 1.0f32 / (kx * ky) as f32;
            for y in 0..h2 {
                for x in 0..w2 {
                    for c in 0..ch {
                        let mut sum = 0i32;
                        for j in 0..ky {
                            for i in 0..kx {
                                sum += src[((y * ky + j) * w + x * kx + i) * ch + c] as i32;
                            }
                        }
                        out[(y * w2 + x) * ch + c] = if kx == 2 && ky == 2 { ((sum + 2) >> 2) as u8 } else { (sum as f32 * recip).round_ties_even().clamp(0.0, 255.0) as u8 };
                    }
                }
            }
        } else {
            // every multiply and add rounded to f32 on its own, as OpenCV does them
            let (tx, ty) = (area_table(w, w2, sx), area_table(h, h2, sy));
            let mut across = vec![0f32; h * w2 * ch];
            for y in 0..h {
                for (dx, row) in tx.iter().enumerate() {
                    for c in 0..ch {
                        let mut acc = 0f32;
                        for &(x, alpha) in row {
                            acc += src[(y * w + x) * ch + c] as f32 * alpha;
                        }
                        across[(y * w2 + dx) * ch + c] = acc;
                    }
                }
            }
            for (dy, row) in ty.iter().enumerate() {
                for i in 0..w2 * ch {
                    let mut acc = 0f32;
                    for &(y, beta) in row {
                        acc += across[y * w2 * ch + i] * beta;
                    }
                    out[dy * w2 * ch + i] = acc.round_ties_even().clamp(0.0, 255.0) as u8;
                }
            }
        }
        return out;
    }
    let (xs, ys) = (area_taps(w, w2, ix, sx, true), area_taps(h, h2, iy, sy, false));
    let mut across = vec![0i64; h * w2 * ch];
    for y in 0..h {
        for (dx, &(x0, x1, p, q)) in xs.iter().enumerate() {
            for c in 0..ch {
                across[(y * w2 + dx) * ch + c] = src[(y * w + x0) * ch + c] as i64 * p + src[(y * w + x1) * ch + c] as i64 * q;
            }
        }
    }
    for (dy, &(y0, y1, p, q)) in ys.iter().enumerate() {
        for i in 0..w2 * ch {
            let v = (((across[y0 * w2 * ch + i] >> 4) * p) >> 16) + (((across[y1 * w2 * ch + i] >> 4) * q) >> 16);
            out[dy * w2 * ch + i] = ((v + 2) >> 2) as u8;
        }
    }
    out
}

/// cv2.resize(INTER_LINEAR): pixel centres line up and the edges repeat.
fn linear(src: &[u8], w: usize, h: usize, ch: usize, w2: usize, h2: usize) -> Vec<u8> {
    let axis = |n: usize, n2: usize| -> Vec<(usize, usize, f32)> {
        (0..n2).map(|i| {
            let p = ((i as f32 + 0.5) * n as f32 / n2 as f32 - 0.5).max(-0.0);
            let p = if p < 0.0 { 0.0 } else { p };
            let i0 = (p.floor() as usize).min(n - 1);
            (i0, (i0 + 1).min(n - 1), p - i0 as f32)
        }).collect()
    };
    let (xs, ys) = (axis(w, w2), axis(h, h2));
    let mut out = vec![0u8; w2 * h2 * ch];
    for (y, &(y0, y1, fy)) in ys.iter().enumerate() {
        for (x, &(x0, x1, fx)) in xs.iter().enumerate() {
            for c in 0..ch {
                let at = |yy: usize, xx: usize| src[(yy * w + xx) * ch + c] as f32;
                let top = at(y0, x0) * (1.0 - fx) + at(y0, x1) * fx;
                let bottom = at(y1, x0) * (1.0 - fx) + at(y1, x1) * fx;
                out[(y * w2 + x) * ch + c] = round8(top * (1.0 - fy) + bottom * fy);
            }
        }
    }
    out
}

/// The two taps of OpenCV's INTER_LINEAR along an axis for 8-bit pictures: (first source, second source, their weights out of 2048).
fn linear_taps(n: usize, m: usize) -> Vec<(usize, usize, i64, i64)> {
    let scale = n as f64 / m as f64;
    (0..m).map(|d| {
        let mut f = ((d as f64 + 0.5) * scale - 0.5) as f32;
        let mut s = f.floor() as i64;
        f -= s as f32;
        if s < 0 {
            f = 0.0;
            s = 0;
        }
        if s >= n as i64 - 1 {
            f = 0.0;
            s = n as i64 - 1;
        }
        let (p, q) = ((1.0f32 - f) * 2048.0f32, f * 2048.0f32);
        (s as usize, (s as usize + 1).min(n - 1), p.round_ties_even() as i64, q.round_ties_even() as i64)
    }).collect()
}

/// cv2.resize(INTER_LINEAR) of an 8-bit picture with `ch` channels the way OpenCV does it: weights in 11 bits and whole-number sums,
/// so the result matches its bytes (the float `linear` above can be a level off).
pub fn resize_linear_fixed(src: &[u8], w: usize, h: usize, ch: usize, w2: usize, h2: usize) -> Vec<u8> {
    if (w2, h2) == (w, h) {
        return src.to_vec();
    }
    let (xs, ys) = (linear_taps(w, w2), linear_taps(h, h2));
    let mut across = vec![0i64; h * w2 * ch];
    for y in 0..h {
        for (dx, &(x0, x1, p, q)) in xs.iter().enumerate() {
            for c in 0..ch {
                across[(y * w2 + dx) * ch + c] = src[(y * w + x0) * ch + c] as i64 * p + src[(y * w + x1) * ch + c] as i64 * q;
            }
        }
    }
    let mut out = vec![0u8; w2 * h2 * ch];
    for (dy, &(y0, y1, p, q)) in ys.iter().enumerate() {
        for i in 0..w2 * ch {
            let v = (((across[y0 * w2 * ch + i] >> 4) * p) >> 16) + (((across[y1 * w2 * ch + i] >> 4) * q) >> 16);
            out[dy * w2 * ch + i] = ((v + 2) >> 2) as u8;
        }
    }
    out
}

/// cv2.resize(INTER_NEAREST): each output pixel takes the source pixel its left edge falls in.
fn nearest(src: &[u8], w: usize, h: usize, ch: usize, w2: usize, h2: usize) -> Vec<u8> {
    let mut out = vec![0u8; w2 * h2 * ch];
    for y in 0..h2 {
        let sy = (y * h / h2).min(h - 1);
        for x in 0..w2 {
            let sx = (x * w / w2).min(w - 1);
            out[(y * w2 + x) * ch..(y * w2 + x + 1) * ch].copy_from_slice(&src[(sy * w + sx) * ch..(sy * w + sx + 1) * ch]);
        }
    }
    out
}

/// One pixel's cv2.cvtColor(RGB2GRAY) / BGR2GRAY: OpenCV 4.13's 15-bit weights 9798, 19235 and 3735, rounded by the shift.
pub fn grey_of(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 9798 + g as u32 * 19235 + b as u32 * 3735 + (1 << 14)) >> 15) as u8
}

impl Rgb {
    pub fn area(&self, w: usize, h: usize) -> Rgb {
        Rgb { w, h, data: resize_area(&self.data, self.w, self.h, 3, w, h) }
    }

    pub fn linear(&self, w: usize, h: usize) -> Rgb {
        Rgb { w, h, data: linear(&self.data, self.w, self.h, 3, w, h) }
    }

    /// cv2.resize(INTER_LINEAR) to the byte.
    pub fn linear_exact(&self, w: usize, h: usize) -> Rgb {
        Rgb { w, h, data: resize_linear_fixed(&self.data, self.w, self.h, 3, w, h) }
    }

    pub fn nearest(&self, w: usize, h: usize) -> Rgb {
        Rgb { w, h, data: nearest(&self.data, self.w, self.h, 3, w, h) }
    }

    /// pictures.shrink: INTER_AREA, in exact halves first, which averages the same pixels and is quicker.
    pub fn shrink(&self, w: usize, h: usize) -> Rgb {
        let mut image = self.clone();
        while image.w >= 2 * w && image.h >= 2 * h {
            let (iw, ih) = (image.w - image.w % 2, image.h - image.h % 2);
            let mut half = Rgb::new(iw / 2, ih / 2);
            for y in 0..ih / 2 {
                for x in 0..iw / 2 {
                    for c in 0..3 {
                        let at = |dy: usize, dx: usize| image.data[((2 * y + dy) * image.w + 2 * x + dx) * 3 + c] as u32;
                        half.data[(y * (iw / 2) + x) * 3 + c] = ((at(0, 0) + at(0, 1) + at(1, 0) + at(1, 1) + 2) / 4) as u8;
                    }
                }
            }
            image = half;
        }
        if (image.w, image.h) == (w, h) { image } else { image.area(w, h) }
    }

    /// cv2.cvtColor(BGR2GRAY) of this picture (as R, G, B).
    pub fn gray(&self) -> Gray {
        Gray { w: self.w, h: self.h, data: self.data.chunks_exact(3).map(|p| grey_of(p[0], p[1], p[2])).collect() }
    }

    /// cv2.GaussianBlur with a 3 x 3 kernel and the sigma OpenCV picks for it.
    pub fn blur3(&self) -> Rgb {
        Rgb { w: self.w, h: self.h, data: gauss(&self.data, self.w, self.h, 3, &[0.25, 0.5, 0.25]) }
    }

    /// cv2.blur(k x k): the average of the square around each pixel, the edges mirrored.
    pub fn box_blur(&self, kw: usize, kh: usize) -> Rgb {
        Rgb { w: self.w, h: self.h, data: boxed(&self.data, self.w, self.h, 3, kw, kh) }
    }
}

impl Gray {
    pub fn area(&self, w: usize, h: usize) -> Gray {
        Gray { w, h, data: resize_area(&self.data, self.w, self.h, 1, w, h) }
    }

    pub fn linear(&self, w: usize, h: usize) -> Gray {
        Gray { w, h, data: linear(&self.data, self.w, self.h, 1, w, h) }
    }

    /// cv2.resize(INTER_LINEAR) to the byte.
    pub fn linear_exact(&self, w: usize, h: usize) -> Gray {
        Gray { w, h, data: resize_linear_fixed(&self.data, self.w, self.h, 1, w, h) }
    }

    /// cv2.GaussianBlur(mask, (0, 0), sigma).
    pub fn blur(&self, sigma: f32) -> Gray {
        // OpenCV makes a kernel of 3 sigma either way, rounded up to an odd width
        let size = ((sigma * 3.0 * 2.0 + 1.0).round() as usize) | 1;
        let half = size / 2;
        let mut k: Vec<f32> = (0..size).map(|i| (-((i as f32 - half as f32).powi(2)) / (2.0 * sigma * sigma)).exp()).collect();
        let sum: f32 = k.iter().sum();
        k.iter_mut().for_each(|v| *v /= sum);
        Gray { w: self.w, h: self.h, data: gauss(&self.data, self.w, self.h, 1, &k) }
    }

    /// pictures.soften: the mask's edge feathered (edges 0..10; 0 leaves it as it was drawn), blurred at a quarter of the size.
    pub fn soften(&self, edges: i64) -> Gray {
        if edges <= 0 {
            return self.clone();
        }
        let small = self.area((self.w / 4).max(1), (self.h / 4).max(1));
        small.blur(edges as f32 * 0.6).linear(self.w, self.h)
    }

    /// cv2.absdiff-and-threshold's result: 255 where this is above `level`, else 0.
    pub fn above(&self, level: u8) -> Gray {
        Gray { w: self.w, h: self.h, data: self.data.iter().map(|&v| if v > level { 255 } else { 0 }).collect() }
    }

    /// cv2.erode with a rectangle `k` wide and tall, outside the picture not counting.
    pub fn erode(&self, k: usize) -> Gray {
        Gray { w: self.w, h: self.h, data: morph(&self.data, self.w, self.h, k, true) }
    }

    pub fn dilate(&self, k: usize) -> Gray {
        Gray { w: self.w, h: self.h, data: morph(&self.data, self.w, self.h, k, false) }
    }

    pub fn open(&self, k: usize) -> Gray {
        self.erode(k).dilate(k)
    }

    pub fn close(&self, k: usize) -> Gray {
        self.dilate(k).erode(k)
    }

    /// cv2.blur(kw x kh) of a plane.
    pub fn box_blur(&self, kw: usize, kh: usize) -> Gray {
        Gray { w: self.w, h: self.h, data: boxed(&self.data, self.w, self.h, 1, kw, kh) }
    }

    /// cv2.max of two masks.
    pub fn max(&self, other: &Gray) -> Gray {
        Gray { w: self.w, h: self.h, data: self.data.iter().zip(&other.data).map(|(&a, &b)| a.max(b)).collect() }
    }
}

/// A position mirrored back into 0..n the way BORDER_REFLECT_101 does (the edge pixel isn't repeated).
fn reflect(i: isize, n: usize) -> usize {
    if n == 1 {
        return 0;
    }
    let n = n as isize;
    let mut i = i;
    while i < 0 || i >= n {
        i = if i < 0 { -i } else { 2 * (n - 1) - i };
    }
    i as usize
}

/// A separable kernel over every channel.
fn gauss(src: &[u8], w: usize, h: usize, ch: usize, k: &[f32]) -> Vec<u8> {
    let half = (k.len() / 2) as isize;
    let mut across = vec![0f32; w * h * ch];
    for y in 0..h {
        for x in 0..w {
            for c in 0..ch {
                across[(y * w + x) * ch + c] = k.iter().enumerate().map(|(j, wt)| src[(y * w + reflect(x as isize + j as isize - half, w)) * ch + c] as f32 * wt).sum();
            }
        }
    }
    let mut out = vec![0u8; w * h * ch];
    for y in 0..h {
        for x in 0..w {
            for c in 0..ch {
                out[(y * w + x) * ch + c] = round8(k.iter().enumerate().map(|(j, wt)| across[(reflect(y as isize + j as isize - half, h) * w + x) * ch + c] * wt).sum());
            }
        }
    }
    out
}

fn boxed(src: &[u8], w: usize, h: usize, ch: usize, kw: usize, kh: usize) -> Vec<u8> {
    let kx = vec![1.0 / kw as f32; kw];
    let ky = vec![1.0 / kh as f32; kh];
    let across = {
        let mut a = vec![0f32; w * h * ch];
        let half = (kw / 2) as isize;
        for y in 0..h {
            for x in 0..w {
                for c in 0..ch {
                    a[(y * w + x) * ch + c] = kx.iter().enumerate().map(|(j, wt)| src[(y * w + reflect(x as isize + j as isize - half, w)) * ch + c] as f32 * wt).sum();
                }
            }
        }
        a
    };
    let half = (kh / 2) as isize;
    let mut out = vec![0u8; w * h * ch];
    for y in 0..h {
        for x in 0..w {
            for c in 0..ch {
                out[(y * w + x) * ch + c] = round8(ky.iter().enumerate().map(|(j, wt)| across[(reflect(y as isize + j as isize - half, h) * w + x) * ch + c] * wt).sum());
            }
        }
    }
    out
}

/// A rectangle's minimum (erode) or maximum (dilate), one axis at a time.
fn morph(src: &[u8], w: usize, h: usize, k: usize, erode: bool) -> Vec<u8> {
    let half = (k / 2) as isize;
    let pick = |a: u8, b: u8| if erode { a.min(b) } else { a.max(b) };
    let start = if erode { 255 } else { 0 };
    let mut across = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut v = start;
            for j in -half..=half {
                let xx = x as isize + j;
                if xx >= 0 && (xx as usize) < w {
                    v = pick(v, src[y * w + xx as usize]);
                }
            }
            across[y * w + x] = v;
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut v = start;
            for j in -half..=half {
                let yy = y as isize + j;
                if yy >= 0 && (yy as usize) < h {
                    v = pick(v, across[yy as usize * w + x]);
                }
            }
            out[y * w + x] = v;
        }
    }
    out
}

/// pictures.compose: top where mask is 255 over under, each term rounded as cv2.multiply(scale=1/255) does.
pub fn compose(top: &Rgb, mask: &Gray, under: &Rgb) -> Rgb {
    let mut out = Rgb::new(top.w, top.h);
    for (i, m) in mask.data.iter().enumerate() {
        for c in 0..3 {
            let a = round8(top.data[i * 3 + c] as f32 * *m as f32 / 255.0) as u16;
            let b = round8(under.data[i * 3 + c] as f32 * (255 - *m) as f32 / 255.0) as u16;
            out.data[i * 3 + c] = (a + b).min(255) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from cv2.resize(INTER_AREA) in OpenCV 4.13.0: (width, height, channels, new width, new height, source, result)

    const AREA_CASES: &[(usize, usize, usize, usize, usize, &[u8], &[u8])] = &[
        (2, 1, 1, 4, 1, &[0, 100], &[0, 0, 100, 100]),
        (3, 1, 1, 2, 1, &[169, 65, 6], &[134, 26]),
        (5, 4, 3, 3, 3, &[149, 106, 38, 175, 188, 205, 175, 229, 98, 249, 10, 148, 95, 86, 147, 198, 66, 39, 106, 213, 171, 45, 167, 57, 69, 81, 55, 14, 153, 178, 215, 76, 52, 39, 250, 72, 215, 50, 161, 223, 112, 172, 161, 233, 38, 17, 89, 1, 221, 6, 242, 127, 143, 6, 60, 210, 94, 25, 166, 33], &[160, 135, 102, 158, 173, 117, 126, 73, 143, 153, 135, 76, 122, 131, 112, 111, 154, 110, 110, 78, 88, 145, 122, 89, 76, 184, 66]),
        (4, 4, 1, 1, 1, &[249, 189, 12, 204, 33, 57, 124, 30, 199, 149, 202, 119, 72, 220, 3, 209], &[129]),
        (6, 6, 3, 2, 2, &[122, 136, 147, 77, 133, 82, 115, 87, 162, 230, 70, 71, 240, 62, 47, 184, 31, 34, 63, 65, 146, 197, 142, 253, 81, 133, 240, 113, 30, 247, 103, 122, 31, 19, 42, 129, 141, 136, 33, 149, 161, 0, 178, 141, 107, 35, 107, 130, 72, 27, 217, 254, 99, 13, 195, 206, 59, 228, 235, 202, 183, 47, 69, 179, 82, 35, 71, 159, 110, 121, 77, 87, 3, 126, 44, 253, 233, 225, 90, 119, 137, 120, 30, 55, 33, 165, 87, 216, 229, 167, 3, 41, 109, 203, 129, 219, 63, 101, 207, 156, 27, 220, 93, 29, 143, 200, 63, 11], &[125, 126, 130, 139, 66, 102, 136, 137, 141, 132, 96, 102]),
        (4, 4, 3, 2, 2, &[11, 180, 219, 113, 190, 87, 34, 9, 133, 29, 61, 36, 244, 192, 207, 131, 254, 67, 99, 248, 47, 71, 89, 119, 137, 85, 246, 206, 214, 129, 203, 234, 113, 121, 206, 210, 200, 248, 20, 161, 52, 114, 255, 112, 206, 54, 157, 131], &[125, 204, 145, 58, 102, 84, 176, 150, 127, 158, 177, 165]),
        (6, 2, 3, 3, 5, &[133, 69, 42, 68, 243, 24, 51, 127, 8, 205, 30, 97, 79, 35, 93, 63, 131, 188, 69, 254, 233, 238, 4, 3, 167, 155, 225, 24, 84, 68, 65, 49, 124, 147, 17, 248], &[101, 156, 33, 128, 79, 53, 71, 83, 141, 101, 156, 33, 128, 79, 53, 71, 83, 141, 127, 143, 76, 112, 99, 100, 89, 58, 163, 154, 129, 118, 96, 120, 147, 106, 33, 186, 154, 129, 118, 96, 120, 147, 106, 33, 186]),
        (3, 7, 1, 8, 2, &[94, 4, 41, 177, 159, 126, 192, 225, 86, 246, 90, 51, 34, 231, 145, 184, 128, 238, 153, 94, 63], &[153, 153, 140, 115, 115, 106, 102, 102, 64, 64, 113, 211, 211, 158, 132, 132]),
        (3, 2, 3, 7, 5, &[47, 186, 244, 0, 105, 58, 23, 158, 13, 2, 126, 212, 229, 139, 231, 169, 235, 194], &[47, 186, 244, 47, 186, 244, 16, 132, 120, 0, 105, 58, 8, 123, 43, 23, 158, 13, 23, 158, 13, 47, 186, 244, 47, 186, 244, 16, 132, 120, 0, 105, 58, 8, 123, 43, 23, 158, 13, 23, 158, 13, 25, 156, 228, 25, 156, 228, 84, 133, 172, 115, 122, 145, 108, 147, 131, 96, 197, 104, 96, 197, 104, 2, 126, 212, 2, 126, 212, 153, 135, 225, 229, 139, 231, 209, 171, 219, 169, 235, 194, 169, 235, 194, 2, 126, 212, 2, 126, 212, 153, 135, 225, 229, 139, 231, 209, 171, 219, 169, 235, 194, 169, 235, 194]),
        (13, 9, 1, 12, 20, &[29, 67, 167, 136, 85, 99, 129, 68, 235, 102, 3, 247, 178, 114, 93, 225, 31, 230, 172, 47, 136, 166, 129, 130, 134, 151, 147, 240, 170, 214, 222, 216, 17, 110, 58, 181, 22, 129, 196, 163, 116, 254, 199, 10, 104, 179, 228, 189, 201, 43, 56, 121, 135, 65, 108, 23, 236, 144, 150, 62, 199, 102, 136, 138, 180, 184, 14, 186, 124, 92, 253, 129, 18, 27, 234, 156, 238, 24, 16, 223, 213, 22, 159, 175, 38, 205, 123, 92, 48, 200, 171, 164, 77, 151, 34, 153, 232, 199, 180, 27, 154, 29, 43, 15, 24, 6, 0, 169, 116, 214, 59, 99, 50, 32, 44, 153, 142], &[32, 82, 160, 120, 90, 113, 96, 171, 143, 26, 209, 183, 32, 82, 160, 120, 90, 113, 96, 171, 143, 26, 209, 183, 94, 106, 176, 98, 182, 114, 95, 158, 141, 106, 150, 157, 112, 113, 180, 92, 208, 114, 95, 154, 140, 130, 133, 150, 135, 178, 180, 161, 214, 120, 79, 112, 142, 90, 122, 173, 154, 229, 180, 216, 220, 124, 67, 78, 143, 59, 113, 191, 156, 199, 200, 191, 162, 129, 113, 120, 161, 66, 93, 166, 159, 137, 241, 141, 46, 139, 205, 204, 197, 79, 54, 116, 156, 130, 224, 135, 63, 139, 194, 198, 190, 85, 63, 123, 130, 72, 88, 89, 201, 147, 103, 146, 132, 128, 138, 177, 130, 72, 88, 89, 201, 147, 103, 146, 132, 128, 138, 177, 166, 44, 162, 111, 159, 190, 73, 37, 166, 169, 216, 55, 171, 40, 172, 114, 154, 196, 69, 24, 170, 174, 225, 41, 78, 161, 170, 81, 161, 140, 108, 111, 124, 97, 193, 129, 32, 221, 169, 64, 165, 112, 128, 155, 102, 58, 177, 173, 88, 162, 149, 67, 173, 158, 155, 124, 107, 58, 116, 104, 157, 88, 124, 71, 183, 217, 189, 86, 115, 58, 41, 17, 127, 70, 105, 89, 177, 200, 165, 82, 98, 54, 62, 45, 23, 5, 39, 153, 154, 142, 81, 69, 38, 41, 136, 143, 23, 5, 39, 153, 154, 142, 81, 69, 38, 41, 136, 143]),
        (17, 11, 3, 5, 4, &[44, 73, 196, 57, 76, 125, 115, 38, 91, 106, 175, 97, 178, 136, 251, 208, 174, 207, 167, 102, 97, 98, 35, 97, 30, 118, 76, 147, 170, 56, 143, 250, 189, 10, 106, 100, 18, 178, 9, 151, 30, 45, 130, 112, 118, 247, 228, 154, 184, 250, 25, 114, 113, 190, 83, 163, 68, 43, 18, 140, 218, 215, 43, 173, 15, 138, 238, 244, 42, 11, 154, 38, 215, 79, 199, 143, 37, 98, 184, 112, 16, 151, 165, 91, 85, 16, 84, 61, 39, 237, 96, 13, 227, 32, 10, 103, 73, 48, 127, 211, 120, 153, 193, 24, 129, 154, 246, 103, 67, 148, 195, 247, 170, 95, 147, 243, 44, 129, 10, 55, 202, 123, 241, 1, 60, 205, 248, 171, 138, 180, 166, 3, 153, 84, 232, 76, 190, 59, 63, 127, 17, 131, 195, 223, 131, 117, 197, 23, 180, 231, 134, 169, 170, 223, 116, 136, 248, 146, 177, 33, 145, 56, 122, 216, 106, 202, 86, 216, 83, 28, 146, 123, 147, 31, 232, 241, 110, 121, 91, 155, 51, 23, 103, 231, 18, 219, 232, 245, 51, 29, 217, 145, 127, 1, 230, 71, 16, 221, 60, 37, 244, 210, 22, 144, 160, 73, 69, 49, 53, 85, 14, 192, 25, 58, 139, 237, 23, 131, 213, 62, 201, 68, 101, 141, 29, 40, 150, 12, 68, 176, 56, 46, 147, 67, 142, 96, 120, 141, 81, 226, 121, 161, 49, 248, 19, 11, 62, 164, 189, 47, 93, 149, 229, 123, 215, 145, 67, 109, 132, 15, 78, 102, 84, 96, 129, 53, 187, 251, 136, 93, 52, 105, 89, 164, 155, 54, 64, 42, 79, 61, 204, 72, 159, 182, 81, 31, 190, 9, 229, 215, 43, 248, 52, 224, 79, 224, 40, 202, 73, 145, 224, 42, 189, 119, 39, 159, 237, 194, 6, 210, 49, 140, 168, 184, 40, 122, 111, 75, 121, 82, 9, 87, 93, 114, 119, 212, 86, 192, 246, 232, 26, 14, 88, 77, 115, 6, 96, 245, 107, 184, 181, 136, 71, 95, 112, 102, 193, 205, 113, 9, 197, 7, 154, 147, 62, 232, 87, 219, 117, 31, 242, 35, 114, 106, 217, 6, 151, 247, 73, 247, 198, 27, 216, 1, 99, 168, 201, 163, 86, 186, 145, 215, 159, 109, 36, 238, 58, 20, 235, 212, 19, 134, 216, 250, 239, 110, 158, 108, 112, 223, 65, 219, 169, 174, 181, 48, 30, 88, 36, 183, 219, 14, 80, 79, 29, 151, 6, 134, 147, 71, 104, 240, 5, 175, 13, 90, 35, 221, 180, 101, 252, 90, 72, 155, 67, 231, 110, 163, 227, 132, 150, 155, 228, 135, 164, 64, 231, 195, 234, 82, 4, 202, 187, 249, 191, 232, 230, 72, 202, 255, 119, 4, 172, 38, 210, 39, 160, 105, 195, 146, 244, 238, 179, 97, 194, 78, 243, 102, 83, 1, 120, 180, 121, 147, 56, 34, 195, 192, 207, 107, 60, 190, 180, 175, 126, 207, 242, 28, 211, 164, 8, 248, 2, 35, 227, 147, 221, 216, 8, 69, 61, 107, 173, 144, 239, 216, 171, 162, 180, 208, 150, 34, 109, 248, 212, 70, 167, 3, 252, 178, 189, 221, 216, 225, 226, 160, 154, 99, 63, 124, 52, 153, 3, 203, 70, 61, 196, 243, 234, 51, 84, 131, 229, 111, 100, 22, 131, 68, 209, 185, 107], &[104, 107, 130, 168, 144, 117, 138, 108, 102, 87, 115, 119, 132, 128, 139, 122, 116, 105, 113, 128, 123, 103, 135, 87, 150, 129, 121, 129, 77, 180, 165, 123, 70, 152, 133, 83, 106, 164, 104, 129, 174, 139, 123, 112, 157, 118, 139, 134, 153, 155, 116, 155, 150, 132, 164, 155, 116, 179, 118, 123]),
        (7, 5, 3, 7, 5, &[173, 227, 246, 57, 15, 74, 98, 49, 46, 72, 210, 50, 221, 195, 243, 232, 72, 227, 245, 92, 183, 226, 45, 27, 173, 141, 237, 45, 215, 251, 16, 101, 131, 123, 151, 88, 92, 174, 101, 79, 44, 66, 184, 242, 114, 101, 215, 221, 215, 238, 197, 146, 62, 180, 134, 252, 107, 179, 14, 87, 26, 150, 55, 35, 50, 163, 130, 69, 230, 167, 220, 22, 45, 220, 184, 243, 213, 74, 15, 56, 238, 82, 201, 113, 153, 82, 195, 51, 128, 38, 251, 250, 10, 223, 118, 40, 44, 75, 249, 131, 140, 37, 87, 243, 183], &[173, 227, 246, 57, 15, 74, 98, 49, 46, 72, 210, 50, 221, 195, 243, 232, 72, 227, 245, 92, 183, 226, 45, 27, 173, 141, 237, 45, 215, 251, 16, 101, 131, 123, 151, 88, 92, 174, 101, 79, 44, 66, 184, 242, 114, 101, 215, 221, 215, 238, 197, 146, 62, 180, 134, 252, 107, 179, 14, 87, 26, 150, 55, 35, 50, 163, 130, 69, 230, 167, 220, 22, 45, 220, 184, 243, 213, 74, 15, 56, 238, 82, 201, 113, 153, 82, 195, 51, 128, 38, 251, 250, 10, 223, 118, 40, 44, 75, 249, 131, 140, 37, 87, 243, 183]),
    ];


    #[test]
    fn grey_is_what_opencv_gives() {
        // recorded from cv2.cvtColor(RGB2GRAY): the older 14-bit weights and the float ones each get some of these a level off
        for (rgb, want) in [([126, 255, 203], 211), ([138, 167, 72], 148), ([48, 17, 212], 48), ([230, 156, 247], 188), ([125, 233, 161], 193)] {
            assert_eq!(grey_of(rgb[0], rgb[1], rgb[2]), want, "{rgb:?}");
        }
        assert_eq!(Rgb { w: 1, h: 1, data: vec![138, 167, 72] }.gray().data, vec![148]);
    }

    #[test]
    fn area_resizes_as_opencv_does_in_every_direction() {
        for (i, (w, h, ch, w2, h2, src, want)) in AREA_CASES.iter().enumerate() {
            assert_eq!(&resize_area(src, *w, *h, *ch, *w2, *h2), want, "case {i}: {w}x{h}x{ch} to {w2}x{h2}");
        }
    }

    #[test]
    fn area_averages_what_it_covers() {
        let mut a = Rgb::new(4, 2);
        a.set(0, 0, [100, 0, 0]);
        a.set(1, 0, [200, 0, 0]);
        let s = a.area(2, 1);
        assert_eq!(s.at(0, 0), [75, 0, 0]);
        assert_eq!(a.area(4, 2), a);
        // not an even fit: 3 wide into 2 gives each output 1.5 pixels
        let b = Gray { w: 3, h: 1, data: vec![0, 90, 90] }.area(2, 1);
        assert_eq!(b.data, vec![30, 90]);
    }

    #[test]
    fn linear_lines_up_pixel_centres() {
        let g = Gray { w: 2, h: 1, data: vec![0, 100] };
        assert_eq!(g.linear(4, 1).data, vec![0, 25, 75, 100]);
        assert_eq!(g.linear(2, 1), g);
    }

    #[test]
    fn nearest_takes_the_pixel_the_left_edge_is_in() {
        let a = Rgb { w: 2, h: 1, data: vec![1, 1, 1, 2, 2, 2] };
        assert_eq!(a.nearest(4, 1).data, vec![1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2]);
    }

    #[test]
    fn a_blur_keeps_flat_and_spreads_a_dot() {
        let flat = Gray::filled(9, 9, 80).blur(1.5);
        assert!(flat.data.iter().all(|&v| v == 80));
        let mut dot = Gray::new(9, 9);
        dot.data[4 * 9 + 4] = 255;
        let b = dot.blur(1.0);
        assert!(b.data[4 * 9 + 4] < 255 && b.data[4 * 9 + 5] > 0);
        assert_eq!(Rgb::filled(5, 5, [9, 9, 9]).blur3().at(2, 2), [9, 9, 9]);
        assert_eq!(Rgb::filled(5, 5, [9, 9, 9]).box_blur(3, 3).at(0, 0), [9, 9, 9]);
    }

    #[test]
    fn opening_removes_specks_and_closing_fills_gaps() {
        let mut m = Gray::new(12, 12);
        m.data[5 * 12 + 5] = 255;
        assert!(m.open(3).data.iter().all(|&v| v == 0));
        let mut m = Gray::new(12, 12);
        for y in 2..10 {
            for x in 2..10 {
                m.data[y * 12 + x] = 255;
            }
        }
        m.data[5 * 12 + 5] = 0;
        assert_eq!(m.close(3).data[5 * 12 + 5], 255);
    }

    #[test]
    fn compose_lays_by_the_mask() {
        let top = Rgb::filled(2, 1, [200, 100, 0]);
        let under = Rgb::filled(2, 1, [0, 0, 100]);
        let out = compose(&top, &Gray { w: 2, h: 1, data: vec![255, 0] }, &under);
        assert_eq!((out.at(0, 0), out.at(1, 0)), ([200, 100, 0], [0, 0, 100]));
    }

    #[test]
    fn soften_feathers_an_edge_but_zero_leaves_it() {
        let mut m = Gray::new(160, 40);
        for y in 0..40 {
            for x in 80..160 {
                m.data[y * 160 + x] = 255;
            }
        }
        assert_eq!(m.soften(0), m);
        let s = m.soften(3);
        let at = |x: usize| s.data[20 * 160 + x];
        assert!(at(79) > 0 && at(79) < 255 && at(5) == 0 && at(155) == 255);
    }
}
