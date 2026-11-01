//! The few OpenCV operations the CPU effects use, on 8-bit pictures: scaling (INTER_AREA, INTER_LINEAR, INTER_NEAREST), blurs,
//! the morphology a mask is tidied with and the compose that lays one picture over another by a mask. They follow OpenCV's
//! sampling (pixel centres, repeated or reflected edges) so a picture made here lines up with the one Python makes.

use crate::cpu::Rgb;
use std::sync::OnceLock;
use telescope_core::image::{keep_bytes, over_bands, rint, spare_bytes};
pub use telescope_core::image::{grey_of, resize_area, resize_linear_fixed};

/// A single-channel 8-bit picture: a mask, or brightness.
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Drop for Gray {
    fn drop(&mut self) {
        keep_bytes(std::mem::take(&mut self.data));
    }
}

impl Gray {
    pub fn new(w: usize, h: usize) -> Gray {
        let mut out = Gray::scratch(w, h);
        out.data.fill(0);
        out
    }

    /// A picture to write every pixel of: until then what's in it is whatever a spare buffer had.
    pub fn scratch(w: usize, h: usize) -> Gray {
        Gray { w, h, data: spare_bytes(w * h) }
    }

    pub fn into_data(mut self) -> Vec<u8> {
        std::mem::take(&mut self.data)
    }

    pub fn filled(w: usize, h: usize, v: u8) -> Gray {
        Gray { w, h, data: vec![v; w * h] }
    }
}

fn round8(v: f32) -> u8 {
    rint(v as f64).clamp(0, 255) as u8
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
    let mut out = spare_bytes(w2 * h2 * ch);
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
    let mut out = spare_bytes(w2 * h2 * ch);
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

    /// cv2.GaussianBlur(image, (0, 0), sigma) to the byte.
    pub fn blur(&self, sigma: f64) -> Rgb {
        Rgb { w: self.w, h: self.h, data: gauss_exact(&self.data, self.w, self.h, 3, sigma) }
    }

    /// cv2.cvtColor(BGR2GRAY) of this picture (as R, G, B).
    pub fn gray(&self) -> Gray {
        Gray { w: self.w, h: self.h, data: self.data.as_chunks::<3>().0.iter().map(|p| grey_of(p[0], p[1], p[2])).collect() }
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

    /// pictures.shrink of a mask.
    pub fn shrink(&self, w: usize, h: usize) -> Gray {
        let mut image = self.clone();
        while image.w >= 2 * w && image.h >= 2 * h {
            let (iw, ih) = (image.w / 2, image.h / 2);
            let mut half = Gray::new(iw, ih);
            for y in 0..ih {
                for x in 0..iw {
                    let at = |dy: usize, dx: usize| image.data[(2 * y + dy) * image.w + 2 * x + dx] as u32;
                    half.data[y * iw + x] = ((at(0, 0) + at(0, 1) + at(1, 0) + at(1, 1) + 2) / 4) as u8;
                }
            }
            image = half;
        }
        if (image.w, image.h) == (w, h) { image } else { image.area(w, h) }
    }

    /// cv2.GaussianBlur(mask, (0, 0), sigma) to the byte.
    pub fn blur(&self, sigma: f64) -> Gray {
        Gray { w: self.w, h: self.h, data: gauss_exact(&self.data, self.w, self.h, 1, sigma) }
    }

    /// pictures.soften: the mask's edge feathered (edges 0..10; 0 leaves it as it was drawn), blurred at a quarter of the size.
    pub fn soften(&self, edges: i64) -> Gray {
        if edges <= 0 {
            return self.clone();
        }
        let small = self.area((self.w / 4).max(1), (self.h / 4).max(1));
        small.blur(edges as f64 * 0.6).linear_exact(self.w, self.h)
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

/// getGaussianKernelBitExact: n weights around the middle for sigma, adding up to 1.
fn gaussian_kernel(n: usize, sigma: f64) -> Vec<f64> {
    let scale = -0.125 / (sigma * sigma);
    let half = n / 2;
    let side: Vec<f64> = (0..half).map(|i| {
        let x = (2 * i) as f64 + 1.0 - n as f64;
        (x * x * scale).exp()
    }).collect();
    let mul = 1.0 / (2.0 * side.iter().sum::<f64>() + 1.0);
    let mut kernel = vec![mul; n];
    for (i, v) in side.iter().enumerate() {
        kernel[i] = v * mul;
        kernel[n - 1 - i] = v * mul;
    }
    kernel
}

/// The kernel OpenCV uses on 8-bit pictures: 8 fractional bits, each weight taking the last one's rounding error, the middle one whatever makes them add up to 256.
fn fixed_kernel(n: usize, sigma: f64) -> Vec<u32> {
    let kernel = gaussian_kernel(n, sigma);
    let half = n / 2;
    let (mut err, mut sum) = (0.0f64, 0u32);
    let mut out = vec![0u32; n];
    for i in 0..half {
        let wanted = kernel[i] * 256.0 + err;
        let v = wanted.round_ties_even();
        err = wanted - v;
        out[i] = v as u32;
        out[n - 1 - i] = v as u32;
        sum += v as u32;
    }
    out[half] = 256 - 2 * sum;
    out
}

/// cv2.GaussianBlur(src, (0, 0), sigma) on an 8-bit picture: OpenCV's fixed-point path, the same on every computer.
fn gauss_exact(src: &[u8], w: usize, h: usize, ch: usize, sigma: f64) -> Vec<u8> {
    // 3 sigma either way, rounded to an odd width
    let n = ((sigma * 3.0 * 2.0 + 1.0).round_ties_even() as usize) | 1;
    let kernel = fixed_kernel(n, sigma);
    let half = (n / 2) as isize;
    let across_at: Vec<usize> = (-half..w as isize + half).map(|i| reflect(i, w)).collect();
    let width = w * ch;
    let mut across = vec![0u32; width * h];
    let mut line = vec![0u32; across_at.len() * ch];
    for (row, out) in src.chunks_exact(width).zip(across.chunks_exact_mut(width)) {
        for (i, &x) in across_at.iter().enumerate() {
            for c in 0..ch {
                line[i * ch + c] = row[x * ch + c] as u32;
            }
        }
        for (j, &k) in kernel.iter().enumerate() {
            for (o, v) in out.iter_mut().zip(&line[j * ch..]) {
                *o += k * v;
            }
        }
    }
    let mut out = spare_bytes(width * h);
    let mut sum = vec![0u32; width];
    for (y, row) in out.chunks_exact_mut(width).enumerate() {
        sum.fill(0);
        for (j, &k) in kernel.iter().enumerate() {
            let at = reflect(y as isize + j as isize - half, h);
            for (s, v) in sum.iter_mut().zip(&across[at * width..(at + 1) * width]) {
                *s += k * v;
            }
        }
        for (o, s) in row.iter_mut().zip(&sum) {
            *o = ((s + 0x8000) >> 16).min(255) as u8;
        }
    }
    out
}

/// a * b + c rounded once at the end, as the fused multiply-add OpenCV's float filters run on.
#[inline]
fn fused(a: f32, b: f64, c: f32) -> f32 {
    (a as f64 * b + c as f64) as f32
}

/// cv2.GaussianBlur(src, (0, 0), sigma) on float32 values (`ch` to a pixel). Floats go through sepFilter2D with fused multiply-adds on all but the last few values of a row, which is why those are added up plainly here too. Matches OpenCV's bits from a 7-tap kernel up (sigma from about 0.7); its 5-tap filter is a last bit off.
pub fn blur_f32(src: &[f32], w: usize, h: usize, ch: usize, sigma: f64) -> Vec<f32> {
    // 4 sigma either way for anything but 8 bits
    let n = ((sigma * 4.0 * 2.0 + 1.0).round_ties_even() as usize) | 1;
    let kernel: Vec<f32> = gaussian_kernel(n, sigma).iter().map(|k| *k as f32).collect();
    let wide: Vec<f64> = kernel.iter().map(|k| *k as f64).collect();
    let half = n / 2;
    let across_at: Vec<usize> = (-(half as isize)..(w + half) as isize).map(|i| reflect(i, w)).collect();
    let width = w * ch;
    let (row_lanes, column_lanes) = (width / 4 * 4, width / 8 * 8);
    let mut across = vec![0f32; width * h];
    let mut line = vec![0f32; across_at.len() * ch];
    for (row, out) in src.chunks_exact(width).zip(across.chunks_exact_mut(width)) {
        for (i, &x) in across_at.iter().enumerate() {
            for c in 0..ch {
                line[i * ch + c] = row[x * ch + c];
            }
        }
        for (o, v) in out.iter_mut().zip(&line) {
            *o = kernel[0] * v;
        }
        for j in 1..n {
            let (lanes, plain) = out.split_at_mut(row_lanes);
            for (o, v) in lanes.iter_mut().zip(&line[j * ch..]) {
                *o = fused(*v, wide[j], *o);
            }
            for (o, v) in plain.iter_mut().zip(&line[row_lanes + j * ch..]) {
                *o += kernel[j] * v;
            }
        }
    }
    let mut out = vec![0f32; width * h];
    for (y, row) in out.chunks_exact_mut(width).enumerate() {
        for (o, v) in row.iter_mut().zip(&across[y * width..(y + 1) * width]) {
            *o = kernel[half] * v;
        }
        for j in 1..=half as isize {
            let (a, b) = (reflect(y as isize + j, h) * width, reflect(y as isize - j, h) * width);
            let (near, wider) = (kernel[half + j as usize], wide[half + j as usize]);
            for (x, o) in row.iter_mut().enumerate() {
                let pair = across[a + x] + across[b + x];
                *o = if x < column_lanes { fused(pair, wider, *o) } else { *o + near * pair };
            }
        }
    }
    out
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
    let mut out = spare_bytes(w * h * ch);
    for y in 0..h {
        for x in 0..w {
            for c in 0..ch {
                out[(y * w + x) * ch + c] = round8(k.iter().enumerate().map(|(j, wt)| across[(reflect(y as isize + j as isize - half, h) * w + x) * ch + c] * wt).sum());
            }
        }
    }
    out
}

/// cv2.blur with running sums, so a big kernel costs the same as a small one. The rounding is OpenCV's for kernels of an odd area,
/// the only ones used.
fn boxed(src: &[u8], w: usize, h: usize, ch: usize, kw: usize, kh: usize) -> Vec<u8> {
    let n = (kw * kh) as u32;
    let table: Vec<u8> = (0..=255 * n).map(|s| ((2 * s + n) / (2 * n)) as u8).collect();
    let rl = w * ch;
    let (half_w, half_h) = ((kw / 2) as isize, (kh / 2) as isize);
    // Where the pixels each side of a row come from, once: reflect is the slow part
    let edges: Vec<usize> = (-half_w..0).chain(w as isize..(w + kw) as isize - half_w - 1).map(|x| reflect(x, w) * ch).collect();
    let across = |y: usize, padded: &mut [u8], line: &mut [u32]| {
        let row = &src[y * rl..][..rl];
        let (before, rest) = padded.split_at_mut(half_w as usize * ch);
        let (middle, after) = rest.split_at_mut(rl);
        middle.copy_from_slice(row);
        for (p, &at) in before.chunks_exact_mut(ch).chain(after.chunks_exact_mut(ch)).zip(&edges) {
            p.copy_from_slice(&row[at..at + ch]);
        }
        for c in 0..ch {
            let mut sum: u32 = (0..kw).map(|i| padded[i * ch + c] as u32).sum();
            line[c] = sum;
            for x in 1..w {
                sum = sum + padded[(x + kw - 1) * ch + c] as u32 - padded[(x - 1) * ch + c] as u32;
                line[x * ch + c] = sum;
            }
        }
    };
    let mut out = spare_bytes(h * rl);
    let table = &table;
    over_bands(&mut out, rl, |first, part| {
        let mut padded = vec![0u8; (w + kw - 1) * ch];
        // The kernel's rows, the one a step down replaces at index (row's place) % kh
        let mut window = vec![0u32; kh * rl];
        let mut sums = vec![0u32; rl];
        for (j, line) in window.chunks_exact_mut(rl).enumerate() {
            across(reflect(first as isize + j as isize - half_h, h), &mut padded, line);
            sums.iter_mut().zip(&*line).for_each(|(s, v)| *s += v);
        }
        let rows = part.len() / rl;
        for (i, line) in part.chunks_exact_mut(rl).enumerate() {
            line.iter_mut().zip(&sums).for_each(|(o, s)| *o = table[*s as usize]);
            if i + 1 < rows {
                let oldest = &mut window[(i % kh) * rl..][..rl];
                sums.iter_mut().zip(&*oldest).for_each(|(s, v)| *s -= v);
                across(reflect((first + i + kh) as isize - half_h, h), &mut padded, oldest);
                sums.iter_mut().zip(&*oldest).for_each(|(s, v)| *s += v);
            }
        }
    });
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

/// cv2.multiply(a, b, scale=1/255) for every a and b, looked up since a frame needs it twice a value.
pub(crate) fn times_table() -> &'static [u8] {
    static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    TABLE.get_or_init(|| (0..256u32).flat_map(|a| (0..256u32).map(move |b| round8(a as f32 * b as f32 / 255.0))).collect())
}

/// pictures.compose: top where mask is 255 over under, each term rounded as cv2.multiply(scale=1/255) does.
pub fn compose(top: &Rgb, mask: &Gray, under: &Rgb) -> Rgb {
    let times = times_table();
    let mut out = Rgb::new(top.w, top.h);
    for ((o, m), (t, u)) in out.data.as_chunks_mut::<3>().0.iter_mut().zip(&mask.data).zip(top.data.as_chunks::<3>().0.iter().zip(under.data.as_chunks::<3>().0)) {
        let m = *m as usize;
        for c in 0..3 {
            let a = times[t[c] as usize * 256 + m] as u16;
            let b = times[u[c] as usize * 256 + (255 - m)] as u16;
            o[c] = (a + b).min(255) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from cv2.resize(INTER_AREA) in OpenCV 4.13.0: (width, height, channels, new width, new height, source, result)

    type AreaCase = (usize, usize, usize, usize, usize, &'static [u8], &'static [u8]);

    const AREA_CASES: &[AreaCase] = &[
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

    // cv2.blur of small pictures, kernels wider than the picture included, so the edges reflect more than once
    #[test]
    fn box_blur_matches_cv2_to_the_byte() {
        let grey = Gray { w: 6, h: 5, data: (0..30usize).map(|i| (i * i * 7 % 251) as u8).collect() };
        assert_eq!(grey.box_blur(9, 9).data, [96, 93, 101, 94, 99, 108, 87, 86, 93, 88, 92, 100, 91, 88, 94, 89, 91, 101, 94, 91, 99, 92, 97, 106, 86, 85, 91, 87, 90, 98]);
        assert_eq!(grey.box_blur(9, 1).data, [47, 54, 59, 63, 65, 66, 123, 111, 126, 111, 123, 133, 89, 87, 83, 78, 71, 91, 58, 65, 71, 75, 77, 78, 140, 129, 143, 129, 141, 151]);
        assert_eq!(grey.box_blur(3, 1).data, [5, 12, 33, 68, 117, 133, 62, 97, 118, 153, 119, 163, 121, 100, 122, 74, 40, 28, 14, 22, 44, 80, 130, 146, 77, 113, 135, 171, 138, 183]);
        let colour = Rgb { w: 7, h: 4, data: (0..84usize).map(|i| (i * i * 11 % 253) as u8).collect() };
        assert_eq!(colour.box_blur(9, 9).data, [83, 82, 83, 78, 81, 83, 81, 81, 84, 84, 84, 81, 96, 90, 89, 89, 101, 89, 93, 103, 101, 83, 79, 86, 79, 80, 88, 81, 79, 87, 82, 84, 85, 93, 88, 93, 87, 101, 94, 93, 102, 105, 83, 82, 88, 81, 82, 91, 82, 83, 89, 83, 87, 87, 92, 90, 94, 89, 102, 97, 96, 101, 109, 82, 80, 90, 82, 81, 96, 82, 81, 92, 81, 86, 92, 89, 88, 97, 88, 103, 102, 96, 100, 113]);
        assert_eq!(colour.box_blur(3, 1).data, [66, 121, 29, 81, 73, 88, 125, 99, 95, 114, 70, 132, 132, 70, 114, 95, 99, 125, 139, 81, 128, 22, 33, 66, 77, 26, 81, 73, 88, 125, 99, 95, 114, 70, 132, 132, 70, 114, 95, 70, 136, 139, 66, 33, 22, 77, 66, 77, 26, 81, 73, 88, 125, 99, 95, 114, 70, 132, 132, 70, 88, 110, 70, 114, 37, 66, 81, 26, 77, 66, 77, 26, 81, 73, 88, 125, 99, 95, 114, 70, 132, 110, 88, 88]);
    }

    #[test]
    fn a_tall_picture_blurs_the_same_across_its_bands() {
        let (w, h, k) = (40usize, 300usize, 9usize);
        let data: Vec<u8> = (0..w * h).map(|i| (i * i % 253) as u8).collect();
        let half = (k / 2) as isize;
        let want: Vec<u8> = (0..w * h).map(|i| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            let sum: u32 = (-half..=half).flat_map(|a| (-half..=half).map(move |b| (a, b))).map(|(a, b)| data[reflect(y + a, h) * w + reflect(x + b, w)] as u32).sum();
            ((2 * sum + 81) / 162) as u8
        }).collect();
        assert_eq!(Gray { w, h, data }.box_blur(k, k).data, want);
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

    /// test_pictures.py's scene: gradients with a disc in the middle.
    fn scene(w: usize, h: usize) -> Rgb {
        let mut image = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let inside = (x as f64 - (w / 2) as f64).powi(2) + (y as f64 - (h / 2) as f64).powi(2) <= ((h / 4) as f64).powi(2);
                let px = if inside { [40, 200, 90] } else {
                    [(x as f32 / w as f32 * 255.0) as u8, (y as f32 / h as f32 * 255.0) as u8, ((x + y) as f32 / (w + h) as f32 * 255.0) as u8]
                };
                image.data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&px);
            }
        }
        image
    }

    fn mean_gap(a: &Rgb, b: &Rgb) -> f64 {
        a.data.iter().zip(&b.data).map(|(x, y)| (*x as i32 - *y as i32).abs() as f64).sum::<f64>() / a.data.len() as f64
    }

    #[test]
    fn shrink_gives_the_size_asked_for_and_looks_like_one_area_step() {
        let frame = scene(1920, 1080);
        for (w, h) in [(120, 67), (240, 135), (256, 144), (128, 72), (1500, 900), (1920, 1080)] {
            let out = frame.shrink(w, h);
            assert_eq!((out.w, out.h, out.data.len()), (w, h, w * h * 3));
            assert!(mean_gap(&out, &frame.area(w, h)) < 1.5, "{w} x {h}");
        }
    }

    #[test]
    fn shrink_by_powers_of_two_matches_one_area_step_to_a_level() {
        let frame = scene(1920, 1080);
        let (a, b) = (frame.shrink(240, 135), frame.area(240, 135));
        assert!(a.data.iter().zip(&b.data).all(|(x, y)| (*x as i32 - *y as i32).abs() <= 1));
    }

    #[test]
    fn shrink_takes_one_channel_and_odd_sizes() {
        let mut mask = Gray::new(1923, 1081);
        for y in 300..800 {
            mask.data[y * 1923 + 600..y * 1923 + 1300].fill(255);
        }
        let out = mask.shrink(120, 67);
        assert_eq!((out.w, out.h, out.data.len()), (120, 67, 120 * 67));
    }
}
