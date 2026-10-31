//! The OpenCV operations more than one crate needs to match Python's pictures to the byte: INTER_AREA and INTER_LINEAR scaling,
//! grey, and frames as plain RGB. Moved here from effects, since the vision service shrinks frames the same way.

use crate::frame::{Frame, PixelFormat};

/// Rounds to a byte. The cast saturates, so this clamps too, and x86-64's baseline has no round instruction: f32::round is a call
/// to libm for every colour of every pixel, which alone used up a 1080p frame's time.
#[inline(always)]
pub fn byte(v: f32) -> u8 {
    (v + 0.5) as u8
}

/// v to the nearest whole number with ties to even, as OpenCV's cvRound. x86-64's baseline has an instruction for it, though
/// f64::round_ties_even still calls libm there.
#[inline(always)]
pub fn rint(v: f64) -> i32 {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::{_mm_cvtsd_si32, _mm_set_sd};
        // SAFETY: SSE2 is part of x86-64, and nothing here changes MXCSR from its round-to-nearest default
        unsafe { _mm_cvtsd_si32(_mm_set_sd(v)) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        v.round_ties_even() as i32
    }
}

/// Rows at or above this are worth spreading over threads.
const BANDED_ROWS: usize = 256;
const BANDS: usize = 4;

/// row(index, its elements) for every row of data, in a few bands of rows on threads of their own when the picture is big.
pub fn over_rows<T: Send>(data: &mut [T], row_len: usize, row: impl Fn(usize, &mut [T]) + Sync) {
    let rows = data.len() / row_len.max(1);
    if rows < BANDED_ROWS {
        data.chunks_exact_mut(row_len.max(1)).enumerate().for_each(|(i, line)| row(i, line));
        return;
    }
    let per_band = rows.div_ceil(BANDS);
    std::thread::scope(|s| {
        for (b, band) in data.chunks_mut(per_band * row_len).enumerate() {
            let row = &row;
            s.spawn(move || band.chunks_exact_mut(row_len).enumerate().for_each(|(i, line)| row(b * per_band + i, line)));
        }
    });
}

/// Reads a 4:2:0 frame into out, 3 bytes a pixel, through pixel(y, u, v). None when the frame is too short for its size.
pub fn read_420(frame: &Frame, out: &mut [u8], pixel: impl Fn(u8, u8, u8) -> [u8; 3] + Sync) -> Option<()> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    if frame.data.len() < w * h + 2 * cw * ch {
        return None;
    }
    let (y, chroma) = frame.data.split_at(w * h);
    let (u, v, step) = if frame.format == PixelFormat::Nv12 { (chroma, &chroma[1..], 2) } else { (chroma, &chroma[cw * ch..], 1) };
    over_rows(out, w * 3, |row, line| {
        let (ys, us, vs) = (&y[row * w..][..w], &u[(row / 2) * cw * step..], &v[(row / 2) * cw * step..]);
        for (col, (p, l)) in line.as_chunks_mut::<3>().0.iter_mut().zip(ys).enumerate() {
            *p = pixel(*l, us[(col / 2) * step], vs[(col / 2) * step]);
        }
    });
    Some(())
}

/// Writes a 4:2:0 picture of w x h into data: luma(x, y) for each pixel, chroma(cx, cy) as (u, v) for each 2 x 2.
pub fn write_420(data: &mut [u8], w: usize, h: usize, nv12: bool, luma: impl Fn(usize, usize) -> u8 + Sync, chroma: impl Fn(usize, usize) -> (u8, u8) + Sync) {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (y_plane, rest) = data.split_at_mut(w * h);
    over_rows(y_plane, w, |y, line| line.iter_mut().enumerate().for_each(|(x, l)| *l = luma(x, y)));
    if nv12 {
        over_rows(&mut rest[..2 * cw * ch], cw * 2, |cy, line| {
            for (cx, cell) in line.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let (u, v) = chroma(cx, cy);
                *cell = [u, v];
            }
        });
    } else {
        let (u_plane, v_plane) = rest.split_at_mut(cw * ch);
        over_rows(u_plane, cw, |cy, line| line.iter_mut().enumerate().for_each(|(cx, u)| *u = chroma(cx, cy).0));
        over_rows(&mut v_plane[..cw * ch], cw, |cy, line| line.iter_mut().enumerate().for_each(|(cx, v)| *v = chroma(cx, cy).1));
    }
}

/// The frame as 3 bytes a pixel (R, G, B). I420 is BT.601 video range with each chroma sample shared by its 2 x 2 pixels, as
/// OpenCV's YUV conversions read it. None for a format that isn't one of the picture formats.
pub fn frame_rgb(frame: &Frame) -> Option<Vec<u8>> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut out = vec![0; w * h * 3];
    match frame.format {
        PixelFormat::Rgba => out.as_chunks_mut::<3>().0.iter_mut().zip(frame.data.as_chunks::<4>().0).for_each(|(o, p)| *o = [p[0], p[1], p[2]]),
        PixelFormat::Bgra => out.as_chunks_mut::<3>().0.iter_mut().zip(frame.data.as_chunks::<4>().0).for_each(|(o, p)| *o = [p[2], p[1], p[0]]),
        PixelFormat::I420 | PixelFormat::Nv12 => read_420(frame, &mut out, |y, u, v| {
            let c = (y as f32 - 16.0).max(0.0) * 1.164_383;
            let (du, dv) = (u as f32 - 128.0, v as f32 - 128.0);
            [byte(c + 1.596_027 * dv), byte(c - 0.391_762 * du - 0.812_968 * dv), byte(c + 2.017_232 * du)]
        })?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_rounds_and_clamps_as_round_and_clamp_do() {
        for i in -4000..4000 {
            let v = i as f32 / 8.0 + 0.03;
            assert_eq!(byte(v), v.round().clamp(0.0, 255.0) as u8, "{v}");
        }
    }

    #[test]
    fn rint_rounds_ties_to_even() {
        for i in -4000..4000 {
            let v = i as f64 / 8.0;
            assert_eq!(rint(v), v.round_ties_even() as i32, "{v}");
        }
        assert_eq!((rint(0.5), rint(1.5), rint(2.5), rint(-0.5), rint(-1.5)), (0, 2, 2, 0, -2));
    }

    #[test]
    fn over_rows_visits_every_row_once_with_its_own_index() {
        for rows in [1, 255, 256, 257, 1081] {
            let mut data = vec![0usize; rows * 3];
            over_rows(&mut data, 3, |row, line| line.iter_mut().for_each(|v| *v += row + 1));
            assert_eq!(data, (0..rows).flat_map(|r| [r + 1; 3]).collect::<Vec<_>>(), "{rows}");
        }
    }

    // Tall enough for bands, odd sizes for the rounded up chroma
    fn picture(w: usize, h: usize, format: PixelFormat) -> Frame {
        let data = (0..format.size(w as u32, h as u32)).map(|i| (i * 7 % 251) as u8).collect();
        Frame::new(w as u32, h as u32, format, data)
    }

    #[test]
    fn a_tall_frame_reads_the_same_as_pixel_by_pixel() {
        let (w, h) = (33usize, 301usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        for format in [PixelFormat::I420, PixelFormat::Nv12] {
            let frame = picture(w, h, format);
            let mut out = vec![0; w * h * 3];
            read_420(&frame, &mut out, |y, u, v| [y, u, v]).unwrap();
            for (y, x) in [(0, 0), (150, 32), (149, 3), (300, 17), (300, 32)] {
                let cell = (y / 2) * cw + x / 2;
                let chroma = &frame.data[w * h..];
                let (u, v) = if format == PixelFormat::Nv12 { (chroma[cell * 2], chroma[cell * 2 + 1]) } else { (chroma[cell], chroma[cw * ch + cell]) };
                assert_eq!(out[(y * w + x) * 3..][..3], [frame.data[y * w + x], u, v], "{format:?} {x},{y}");
            }
        }
    }

    #[test]
    fn a_tall_picture_lands_in_the_right_places_of_both_formats() {
        let (w, h) = (33usize, 301usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        for format in [PixelFormat::I420, PixelFormat::Nv12] {
            let mut data = vec![0; format.size(w as u32, h as u32)];
            let luma = |x: usize, y: usize| ((x + y) % 200) as u8;
            let chroma = |cx: usize, cy: usize| ((cx + cy) as u8, (cx * 2 + cy) as u8);
            write_420(&mut data, w, h, format == PixelFormat::Nv12, luma, chroma);
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(data[y * w + x], luma(x, y));
                }
            }
            for cy in 0..ch {
                for cx in 0..cw {
                    let cell = cy * cw + cx;
                    let got = if format == PixelFormat::Nv12 { (data[w * h + cell * 2], data[w * h + cell * 2 + 1]) } else { (data[w * h + cell], data[w * h + cw * ch + cell]) };
                    assert_eq!(got, chroma(cx, cy), "{format:?}");
                }
            }
        }
    }
}
