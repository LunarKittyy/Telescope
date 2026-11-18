//! The key's matte on the CPU: the alpha chroma_key.wgsl works out for each pixel, from the same tables, so a stream can hand it on as the cut-out of its picture (Frame::alpha). `matte` is the shader's own number, `cut_out` takes it in a pixel and softens it for a tile.

use crate::chroma_key::Tables;
use telescope_core::image::over_rows;

/// What the shader gets of a pixel: (Cr, Cb) as cv2.cvtColor's fixed-point BGR2YCrCb makes them.
fn cr_cb(r: i32, g: i32, b: i32) -> (u8, u8) {
    let y = (b * 1868 + g * 9617 + r * 4899 + 8192) >> 14;
    let cr = (((r - y) * 11682 + 2105344) >> 14).clamp(0, 255);
    let cb = (((b - y) * 9241 + 2105344) >> 14).clamp(0, 255);
    (cr as u8, cb as u8)
}

/// The Cr and Cb planes at half the size (floor, at least 1), averaged over what each of their pixels covers as INTER_AREA does: exactly four pixels for an even size.
fn half_size(rgb: &[u8], (w, h): (usize, usize), (sw, sh): (usize, usize)) -> (Vec<u8>, Vec<u8>) {
    let (mut cr, mut cb) = (vec![0u8; w * h], vec![0u8; w * h]);
    for ((c, b), p) in cr.iter_mut().zip(cb.iter_mut()).zip(rgb.as_chunks::<3>().0) {
        (*c, *b) = cr_cb(p[0] as i32, p[1] as i32, p[2] as i32);
    }
    let (mut out_cr, mut out_cb) = (vec![0u8; sw * sh], vec![0u8; sw * sh]);
    if w == sw * 2 && h == sh * 2 {
        for y in 0..sh {
            for x in 0..sw {
                let at = [(2 * y) * w + 2 * x, (2 * y) * w + 2 * x + 1, (2 * y + 1) * w + 2 * x, (2 * y + 1) * w + 2 * x + 1];
                out_cr[y * sw + x] = ((at.iter().map(|&i| cr[i] as u32).sum::<u32>() + 2) >> 2) as u8;
                out_cb[y * sw + x] = ((at.iter().map(|&i| cb[i] as u32).sum::<u32>() + 2) >> 2) as u8;
            }
        }
        return (out_cr, out_cb);
    }
    let (kx, ky) = (w as f32 / sw as f32, h as f32 / sh as f32);
    for y in 0..sh {
        let (lo_y, hi_y) = (y as f32 * ky, y as f32 * ky + ky);
        for x in 0..sw {
            let (lo_x, hi_x) = (x as f32 * kx, x as f32 * kx + kx);
            let (mut sum_cr, mut sum_cb) = (0.0f32, 0.0f32);
            for py in (lo_y.floor() as usize)..(hi_y.ceil() as usize).min(h) {
                let wy = ((py + 1) as f32).min(hi_y) - (py as f32).max(lo_y);
                for px in (lo_x.floor() as usize)..(hi_x.ceil() as usize).min(w) {
                    let weight = (((px + 1) as f32).min(hi_x) - (px as f32).max(lo_x)) * wy;
                    sum_cr += cr[py * w + px] as f32 * weight;
                    sum_cb += cb[py * w + px] as f32 * weight;
                }
            }
            out_cr[y * sw + x] = (sum_cr / (kx * ky) + 0.5).floor().clamp(0.0, 255.0) as u8;
            out_cb[y * sw + x] = (sum_cb / (kx * ky) + 0.5).floor().clamp(0.0, 255.0) as u8;
        }
    }
    (out_cr, out_cb)
}

/// The shader's matte for a picture of packed R, G, B bytes: the tables looked up at half the size, then scaled back up linearly (pixel centres line up, the edge repeats) and rounded.
pub fn matte(rgb: &[u8], (w, h): (usize, usize), tables: &Tables) -> Vec<u8> {
    let (sw, sh) = ((w / 2).max(1), (h / 2).max(1));
    if rgb.len() != w * h * 3 || w == 0 || h == 0 {
        return Vec::new();
    }
    let (cr, cb) = half_size(rgb, (w, h), (sw, sh));
    let small: Vec<f32> = cr.iter().zip(&cb).map(|(&r, &b)| tables.alpha[r as usize * 256 + b as usize] as f32).collect();
    // where each full-size column and row sits between two of the small ones
    let along = |n: usize, small_n: usize| -> Vec<(usize, usize, f32)> {
        (0..n).map(|i| {
            let pos = (i as f32 + 0.5) * small_n as f32 / n as f32 - 0.5;
            let base = pos.floor();
            let at = |k: f32| (k.max(0.0) as usize).min(small_n - 1);
            (at(base), at(base + 1.0), pos - base)
        }).collect()
    };
    let (cols, rows) = (along(w, sw), along(h, sh));
    let mut out = vec![0u8; w * h];
    over_rows(&mut out, w, |y, line| {
        let (y0, y1, fy) = rows[y];
        let (top, bottom) = (&small[y0 * sw..][..sw], &small[y1 * sw..][..sw]);
        for (o, &(x0, x1, fx)) in line.iter_mut().zip(&cols) {
            let upper = top[x0] * (1.0 - fx) + top[x1] * fx;
            let lower = bottom[x0] * (1.0 - fx) + bottom[x1] * fx;
            *o = (upper * (1.0 - fy) + lower * fy).round_ties_even().clamp(0.0, 255.0) as u8;
        }
    });
    out
}

/// A plane with each pixel replaced by what `pick` makes of it and its neighbours before and after along a row or a column (the edge repeats).
fn three<T: Copy, U: Copy + Default>(plane: &[T], (w, h): (usize, usize), rows: bool, pick: impl Fn(T, T, T) -> U) -> Vec<U> {
    let mut out = vec![U::default(); w * h];
    let (len, count, step) = if rows { (w, h, 1) } else { (h, w, w) };
    for line in 0..count {
        let start = if rows { line * w } else { line };
        for i in 0..len {
            let at = |k: usize| plane[start + k.min(len - 1) * step];
            out[start + i * step] = pick(at(i.saturating_sub(1)), at(i), at(i + 1));
        }
    }
    out
}

/// The matte for a tile: pulled in by 1 pixel (the smallest of each pixel and its neighbours, which takes the screen's fringe off) and then feathered by 1 pixel (the mean of each pixel and its neighbours, so the edge is a soft one).
pub fn cut_out(rgb: &[u8], size: (usize, usize), tables: &Tables) -> Vec<u8> {
    let raw = matte(rgb, size, tables);
    if raw.is_empty() {
        return raw;
    }
    let min = |a: u8, b: u8, c: u8| a.min(b).min(c);
    let eroded = three(&three(&raw, size, true, min), size, false, min);
    let sum = |a: u16, b: u16, c: u16| a + b + c;
    let widened: Vec<u16> = eroded.iter().map(|&v| v as u16).collect();
    let nine = three(&three(&widened, size, true, sum), size, false, sum);
    nine.iter().map(|&s| ((s as u32 + 4) / 9) as u8).collect()
}
