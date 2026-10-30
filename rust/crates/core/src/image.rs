//! The OpenCV operations more than one crate needs to match Python's pictures to the byte: INTER_AREA and INTER_LINEAR scaling,
//! grey, and frames as plain RGB. Moved here from effects, since the vision service shrinks frames the same way.

use crate::frame::{Frame, PixelFormat};

/// The frame as 3 bytes a pixel (R, G, B). I420 is BT.601 video range with each chroma sample shared by its 2 x 2 pixels, as
/// OpenCV's YUV conversions read it. None for a format that isn't one of the picture formats.
pub fn frame_rgb(frame: &Frame) -> Option<Vec<u8>> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut out = Vec::with_capacity(w * h * 3);
    match frame.format {
        PixelFormat::Rgba => frame.data.as_chunks::<4>().0.iter().for_each(|p| out.extend_from_slice(&p[..3])),
        PixelFormat::Bgra => frame.data.as_chunks::<4>().0.iter().for_each(|p| out.extend([p[2], p[1], p[0]])),
        PixelFormat::I420 => {
            let (y, u, v) = frame.i420_planes()?;
            let cw = w.div_ceil(2);
            for row in 0..h {
                for col in 0..w {
                    let c = (y[row * w + col] as f32 - 16.0).max(0.0) * 1.164_383;
                    let (du, dv) = (u[(row / 2) * cw + col / 2] as f32 - 128.0, v[(row / 2) * cw + col / 2] as f32 - 128.0);
                    let q = |x: f32| x.round().clamp(0.0, 255.0) as u8;
                    out.extend([q(c + 1.596_027 * dv), q(c - 0.391_762 * du - 0.812_968 * dv), q(c + 2.017_232 * du)]);
                }
            }
        }
        _ => return None,
    }
    Some(out)
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

/// The two taps of OpenCV's INTER_LINEAR along an axis for 8-bit pictures: (first source, second source, their weights out of 2048).
/// Across, a tap past either edge takes the edge pixel whole; down, the weights stay and only the rows are clamped, as OpenCV does.
fn linear_taps(n: usize, m: usize, vertical: bool) -> Vec<(usize, usize, i64, i64)> {
    let scale = n as f64 / m as f64;
    let last = n as i64 - 1;
    (0..m).map(|d| {
        let mut f = ((d as f64 + 0.5) * scale - 0.5) as f32;
        let mut s = f.floor() as i64;
        f -= s as f32;
        if !vertical && s < 0 {
            f = 0.0;
            s = 0;
        }
        if !vertical && s >= last {
            f = 0.0;
            s = last;
        }
        let (p, q) = ((1.0f32 - f) * 2048.0f32, f * 2048.0f32);
        (s.clamp(0, last) as usize, (s + 1).clamp(0, last) as usize, p.round_ties_even() as i64, q.round_ties_even() as i64)
    }).collect()
}

/// cv2.resize(INTER_LINEAR) of an 8-bit picture with `ch` channels the way OpenCV does it: weights in 11 bits and whole-number sums,
/// so the result matches its bytes (the float `linear` above can be a level off).
pub fn resize_linear_fixed(src: &[u8], w: usize, h: usize, ch: usize, w2: usize, h2: usize) -> Vec<u8> {
    if (w2, h2) == (w, h) {
        return src.to_vec();
    }
    let (xs, ys) = (linear_taps(w, w2, false), linear_taps(h, h2, true));
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

/// One pixel's cv2.cvtColor(RGB2GRAY) / BGR2GRAY: OpenCV 4.13's 15-bit weights 9798, 19235 and 3735, rounded by the shift.
pub fn grey_of(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 9798 + g as u32 * 19235 + b as u32 * 3735 + (1 << 14)) >> 15) as u8
}
