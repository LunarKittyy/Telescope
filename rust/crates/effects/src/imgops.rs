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

/// cv2.resize(INTER_AREA) for a smaller picture; a bigger one is scaled linearly, as OpenCV does.
fn area(src: &[u8], w: usize, h: usize, ch: usize, w2: usize, h2: usize) -> Vec<u8> {
    if w2 > w || h2 > h {
        return linear(src, w, h, ch, w2, h2);
    }
    // Along one axis: for each output cell, the source cells it covers and how much of each
    let weights = |n: usize, n2: usize| -> Vec<Vec<(usize, f32)>> {
        let scale = n as f32 / n2 as f32;
        (0..n2).map(|i| {
            let (lo, hi) = (i as f32 * scale, (i as f32 + 1.0) * scale);
            (lo.floor() as usize..(hi.ceil() as usize).min(n)).map(|k| (k, ((k as f32 + 1.0).min(hi) - (k as f32).max(lo)) / scale)).collect()
        }).collect()
    };
    let (wx, wy) = (weights(w, w2), weights(h, h2));
    // Across first, one row of the source at a time, then down
    let mut across = vec![0f32; h * w2 * ch];
    for y in 0..h {
        for (x, taps) in wx.iter().enumerate() {
            for c in 0..ch {
                across[(y * w2 + x) * ch + c] = taps.iter().map(|&(k, wt)| src[(y * w + k) * ch + c] as f32 * wt).sum();
            }
        }
    }
    let mut out = vec![0u8; w2 * h2 * ch];
    for (y, taps) in wy.iter().enumerate() {
        for x in 0..w2 {
            for c in 0..ch {
                out[(y * w2 + x) * ch + c] = round8(taps.iter().map(|&(k, wt)| across[(k * w2 + x) * ch + c] * wt).sum());
            }
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

impl Rgb {
    pub fn area(&self, w: usize, h: usize) -> Rgb {
        Rgb { w, h, data: area(&self.data, self.w, self.h, 3, w, h) }
    }

    pub fn linear(&self, w: usize, h: usize) -> Rgb {
        Rgb { w, h, data: linear(&self.data, self.w, self.h, 3, w, h) }
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
        let data = self.data.chunks_exact(3).map(|p| ((p[2] as u32 * 1868 + p[1] as u32 * 9617 + p[0] as u32 * 4899 + 8192) >> 14) as u8).collect();
        Gray { w: self.w, h: self.h, data }
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
        Gray { w, h, data: area(&self.data, self.w, self.h, 1, w, h) }
    }

    pub fn linear(&self, w: usize, h: usize) -> Gray {
        Gray { w, h, data: linear(&self.data, self.w, self.h, 1, w, h) }
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
