//! The OpenCV operations more than one crate needs to match Python's pictures to the byte: INTER_AREA and INTER_LINEAR scaling,
//! grey, and frames as plain RGB. Moved here from effects, since the vision service shrinks frames the same way.

use crate::frame::{Frame, PixelFormat};
use std::cell::RefCell;

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

thread_local! {
    // The buffers a frame step lent this thread for one frame, and how many it has asked for. None outside that, so nothing is left
    // behind once the step is gone
    static SPARES: RefCell<Option<(Vec<Vec<u8>>, usize)>> = const { RefCell::new(None) };
}

/// How many buffers a step keeps, and the size below which one isn't worth keeping.
const SPARE_BUFFERS: usize = 12;
const SPARE_ABOVE: usize = 16 * 1024;

/// Runs a frame's work with held's buffers to draw on (spare_bytes) and whatever it finishes with added to them (keep_bytes).
/// Only as many as the frame asked for stay, the biggest ones, so buffers nothing draws on don't pile up.
pub fn with_spares<R>(held: &mut Vec<Vec<u8>>, work: impl FnOnce() -> R) -> R {
    struct Back<'a>(&'a mut Vec<Vec<u8>>);
    impl Drop for Back<'_> {
        fn drop(&mut self) {
            let (mut kept, asked) = SPARES.with_borrow_mut(Option::take).unwrap_or_default();
            kept.sort_by_key(|b| std::cmp::Reverse(b.capacity()));
            kept.truncate(asked);
            *self.0 = kept;
        }
    }
    SPARES.with_borrow_mut(|s| *s = Some((std::mem::take(held), 0)));
    let _back = Back(held);
    work()
}

/// A buffer of len bytes for work that writes every one of them: what's in it is whatever was left there.
pub fn spare_bytes(len: usize) -> Vec<u8> {
    let found = SPARES.with_borrow_mut(|s| {
        let (held, asked) = s.as_mut()?;
        *asked += 1;
        let (i, _) = held.iter().enumerate().filter(|(_, b)| b.capacity() >= len).min_by_key(|(_, b)| b.capacity())?;
        Some(held.swap_remove(i))
    });
    match found {
        Some(mut buffer) => {
            buffer.resize(len, 0);
            buffer
        }
        None => vec![0; len],
    }
}

/// Hands a finished buffer back for the frame's other work and the next frame, if a step is lending any.
pub fn keep_bytes(buffer: Vec<u8>) {
    if buffer.capacity() < SPARE_ABOVE {
        return;
    }
    // Nothing to do this late in a thread's life
    let _ = SPARES.try_with(|s| {
        if let Some((held, _)) = s.borrow_mut().as_mut() {
            if held.len() < SPARE_BUFFERS {
                held.push(buffer);
            } else if let Some(smallest) = held.iter_mut().min_by_key(|b| b.capacity()).filter(|b| b.capacity() < buffer.capacity()) {
                *smallest = buffer;
            }
        }
    });
}

/// Rows at or above this are worth spreading over threads.
const BANDED_ROWS: usize = 256;
const BANDS: usize = 4;

/// band(its first row's index, its rows) over all of data, in a few bands of rows on threads of their own when the picture is big.
/// For work that keeps what it found on one row for the next.
pub fn over_bands<T: Send>(data: &mut [T], row_len: usize, band: impl Fn(usize, &mut [T]) + Sync) {
    let rows = data.len() / row_len.max(1);
    if rows < BANDED_ROWS {
        band(0, data);
        return;
    }
    let per_band = rows.div_ceil(BANDS);
    std::thread::scope(|s| {
        for (b, part) in data.chunks_mut(per_band * row_len).enumerate() {
            let band = &band;
            s.spawn(move || band(b * per_band, part));
        }
    });
}

/// row(index, its elements) for every row of data, in a few bands of rows on threads of their own when the picture is big.
pub fn over_rows<T: Send>(data: &mut [T], row_len: usize, row: impl Fn(usize, &mut [T]) + Sync) {
    over_bands(data, row_len, |first, part| part.chunks_exact_mut(row_len.max(1)).enumerate().for_each(|(i, line)| row(first + i, line)));
}

/// over_rows on two planes of the same size at once.
fn over_row_pairs(a: &mut [u8], b: &mut [u8], row_len: usize, row: impl Fn(usize, &mut [u8], &mut [u8]) + Sync) {
    if row_len == 0 {
        return;
    }
    let rows = a.len() / row_len;
    let per_band = if rows < BANDED_ROWS { rows.max(1) } else { rows.div_ceil(BANDS) };
    let band = |n: usize, a: &mut [u8], b: &mut [u8]| {
        a.chunks_exact_mut(row_len).zip(b.chunks_exact_mut(row_len)).enumerate().for_each(|(i, (x, y))| row(n * per_band + i, x, y));
    };
    let parts = a.chunks_mut(per_band * row_len).zip(b.chunks_mut(per_band * row_len)).enumerate();
    if rows < BANDED_ROWS {
        parts.for_each(|(n, (a, b))| band(n, a, b));
        return;
    }
    std::thread::scope(|s| {
        for (n, (a, b)) in parts {
            let band = &band;
            s.spawn(move || band(n, a, b));
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

/// Reads the BGR the frame came with into out, as R, G, B. False when it has none.
pub fn read_bgr(frame: &Frame, out: &mut [u8]) -> bool {
    let Some(bgr) = frame.bgr() else { return false };
    let w = frame.width as usize;
    over_rows(out, w * 3, |y, line| {
        for (o, p) in line.as_chunks_mut::<3>().0.iter_mut().zip(bgr.row(y, w).as_chunks::<3>().0) {
            *o = [p[2], p[1], p[0]];
        }
    });
    true
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
        over_row_pairs(u_plane, &mut v_plane[..cw * ch], cw, |cy, us, vs| {
            for (cx, (u, v)) in us.iter_mut().zip(vs).enumerate() {
                (*u, *v) = chroma(cx, cy);
            }
        });
    }
}

/// The frame as 3 bytes a pixel (R, G, B). I420 is BT.601 video range with each chroma sample shared by its 2 x 2 pixels, as
/// OpenCV's YUV conversions read it. None for a format that isn't one of the picture formats.
pub fn frame_rgb(frame: &Frame) -> Option<Vec<u8>> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut out = spare_bytes(w * h * 3);
    if read_bgr(frame, &mut out) {
        return Some(out);
    }
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

/// One output cell of a two-tap blend: first source, second source, their weights out of 2048.
type Tap = (usize, usize, i32, i32);

/// Blends one row across into `into`, kept in the 15 bits OpenCV keeps it in. Inlined with a known ch so the channels don't loop.
#[inline(always)]
fn blend_across(row: &[u8], xs: &[Tap], ch: usize, into: &mut [i16]) {
    for (o, &(x0, x1, p, q)) in into.chunks_exact_mut(ch).zip(xs) {
        let (a, b) = (&row[x0 * ch..][..ch], &row[x1 * ch..][..ch]);
        for c in 0..ch {
            o[c] = ((a[c] as i32 * p + b[c] as i32 * q) >> 4) as i16;
        }
    }
}

/// OpenCV's fixed-point two-tap resize of a picture w wide with `ch` channels, row by row into dst, whose rows are `stride` long
/// and start `left` elements in. Rows stop at the end of ys or dst, whichever comes first; a band keeps the two rows it has blended.
fn blended(src: &[u8], (w, ch): (usize, usize), xs: &[Tap], ys: &[Tap], dst: &mut [u8], stride: usize, left: usize) {
    let rowlen = xs.len() * ch;
    let across = |y: usize, into: &mut [i16]| {
        let row = &src[y * w * ch..][..w * ch];
        match ch {
            1 => blend_across(row, xs, 1, into),
            3 => blend_across(row, xs, 3, into),
            _ => blend_across(row, xs, ch, into),
        }
    };
    over_bands(dst, stride, |first, part| {
        let (mut top, mut bottom) = (vec![0i16; rowlen], vec![0i16; rowlen]);
        let (mut have_top, mut have_bottom) = (usize::MAX, usize::MAX);
        for (line, &(y0, y1, p, q)) in part.chunks_exact_mut(stride).zip(&ys[first.min(ys.len())..]) {
            if have_bottom == y0 {
                std::mem::swap(&mut top, &mut bottom);
                std::mem::swap(&mut have_top, &mut have_bottom);
            }
            if have_top != y0 {
                across(y0, &mut top);
                have_top = y0;
            }
            if have_bottom != y1 {
                across(y1, &mut bottom);
                have_bottom = y1;
            }
            for ((o, &a), &b) in line[left..].iter_mut().zip(&top).zip(&bottom) {
                *o = ((((a as i32 * p) >> 16) + ((b as i32 * q) >> 16) + 2) >> 2) as u8;
            }
        }
    });
}

/// Adds each block of kx pixels of a source row into its output cell's sums. Inlined with a known ch so the channels don't loop.
#[inline(always)]
fn add_blocks(row: &[u8], kx: usize, ch: usize, sums: &mut [u32]) {
    for (s, block) in sums.chunks_exact_mut(ch).zip(row.chunks_exact(kx * ch)) {
        for px in block.chunks_exact(ch) {
            for c in 0..ch {
                s[c] += px[c] as u32;
            }
        }
    }
}

/// The two taps of OpenCV's area-mode bilinear along an axis: (first source, second source, their weights out of 2048).
fn area_taps(n: usize, m: usize, inv: f64, scale: f64, horizontal: bool) -> Vec<Tap> {
    (0..m).map(|d| {
        let d = d as f64;
        let s = (d * scale).floor() as i64;
        let f = ((d + 1.0) - (s as f64 + 1.0) * inv) as f32;
        let mut f = if f <= 0.0 { 0.0 } else { f - f.floor() };
        if horizontal && s >= n as i64 - 1 {
            f = 0.0;
        }
        let (p, q) = ((1.0f32 - f) * 2048.0f32, f * 2048.0f32);
        ((s.clamp(0, n as i64 - 1)) as usize, ((s + 1).clamp(0, n as i64 - 1)) as usize, rint(p as f64), rint(q as f64))
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
    let mut out = spare_bytes(w2 * h2 * ch);
    if sx >= 1.0 && sy >= 1.0 {
        let (kx, ky) = (sx.round_ties_even() as usize, sy.round_ties_even() as usize);
        if (sx - kx as f64).abs() < f64::EPSILON && (sy - ky as f64).abs() < f64::EPSILON && w == w2 * kx && h == h2 * ky {
            // whole blocks: integer sums, and a table so the rounding isn't a call per byte
            let recip = 1.0f32 / (kx * ky) as f32;
            let table: Vec<u8> = (0..=255 * (kx * ky) as u32).map(|sum| if kx == 2 && ky == 2 { ((sum + 2) >> 2) as u8 } else { rint((sum as f32 * recip) as f64).clamp(0, 255) as u8 }).collect();
            let rowlen = w2 * ch;
            let table = &table;
            over_bands(&mut out, rowlen, |first, part| {
                let mut sums = vec![0u32; rowlen];
                for (i, line) in part.chunks_exact_mut(rowlen).enumerate() {
                    sums.fill(0);
                    for j in 0..ky {
                        let row = &src[((first + i) * ky + j) * w * ch..][..w2 * kx * ch];
                        match ch {
                            1 => add_blocks(row, kx, 1, &mut sums),
                            3 => add_blocks(row, kx, 3, &mut sums),
                            _ => add_blocks(row, kx, ch, &mut sums),
                        }
                    }
                    line.iter_mut().zip(&sums).for_each(|(o, s)| *o = table[*s as usize]);
                }
            });
        } else {
            // every multiply and add rounded to f32 on its own, as OpenCV does them
            let (tx, ty) = (area_table(w, w2, sx), area_table(h, h2, sy));
            let mut across = vec![0f32; h * w2 * ch];
            over_rows(&mut across, w2 * ch, |y, line| {
                for (cell, row) in line.chunks_exact_mut(ch).zip(&tx) {
                    for (c, o) in cell.iter_mut().enumerate() {
                        let mut acc = 0f32;
                        for &(x, alpha) in row {
                            acc += src[(y * w + x) * ch + c] as f32 * alpha;
                        }
                        *o = acc;
                    }
                }
            });
            over_rows(&mut out, w2 * ch, |dy, line| {
                for (i, o) in line.iter_mut().enumerate() {
                    let mut acc = 0f32;
                    for &(y, beta) in &ty[dy] {
                        acc += across[y * w2 * ch + i] * beta;
                    }
                    *o = rint(acc as f64).clamp(0, 255) as u8;
                }
            });
        }
        return out;
    }
    let (xs, ys) = (area_taps(w, w2, ix, sx, true), area_taps(h, h2, iy, sy, false));
    blended(src, (w, ch), &xs, &ys, &mut out, w2 * ch, 0);
    out
}

/// The two taps of OpenCV's INTER_LINEAR along an axis for 8-bit pictures: (first source, second source, their weights out of 2048).
/// Across, a tap past either edge takes the edge pixel whole; down, the weights stay and only the rows are clamped, as OpenCV does.
fn linear_taps(n: usize, m: usize, vertical: bool) -> Vec<Tap> {
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
        (s.clamp(0, last) as usize, (s + 1).clamp(0, last) as usize, rint(p as f64), rint(q as f64))
    }).collect()
}

/// cv2.resize(INTER_LINEAR) of an 8-bit picture with `ch` channels the way OpenCV does it: weights in 11 bits and whole-number sums,
/// so the result matches its bytes (the float `linear` above can be a level off).
pub fn resize_linear_fixed(src: &[u8], w: usize, h: usize, ch: usize, w2: usize, h2: usize) -> Vec<u8> {
    if (w2, h2) == (w, h) {
        return src.to_vec();
    }
    let mut out = spare_bytes(w2 * h2 * ch);
    resize_linear_into(src, w, h, ch, &mut out, w2 * ch, (0, 0, w2, h2));
    out
}

/// resize_linear_fixed into the (x, y, w, h) box of dst, whose rows are `stride` bytes long; what falls outside dst is left off.
pub fn resize_linear_into(src: &[u8], w: usize, h: usize, ch: usize, dst: &mut [u8], stride: usize, (x, y, w2, h2): (usize, usize, usize, usize)) {
    if w == 0 || h == 0 || w2 == 0 || h2 == 0 || stride == 0 {
        return;
    }
    let (left, top) = ((x * ch).min(stride), (y * stride).min(dst.len()));
    let dst = &mut dst[top..];
    if (w2, h2) == (w, h) {
        for (line, row) in dst.chunks_exact_mut(stride).zip(src.chunks_exact(w * ch)) {
            let keep = row.len().min(stride - left);
            line[left..][..keep].copy_from_slice(&row[..keep]);
        }
    } else {
        blended(src, (w, ch), &linear_taps(w, w2, false), &linear_taps(h, h2, true), dst, stride, left);
    }
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

    #[test]
    fn over_bands_hands_out_every_row_once_with_its_place() {
        for rows in [1, 255, 256, 257, 1081] {
            let mut data = vec![0usize; rows * 3];
            over_bands(&mut data, 3, |first, part| part.chunks_exact_mut(3).enumerate().for_each(|(i, line)| line.iter_mut().for_each(|v| *v += first + i + 1)));
            assert_eq!(data, (0..rows).flat_map(|r| [r + 1; 3]).collect::<Vec<_>>(), "{rows}");
        }
    }

    #[test]
    fn frame_rgb_takes_the_decoders_bgr_when_the_frame_has_it() {
        let bgr = crate::frame::Bgr { data: vec![0, 1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0], start: 1, stride: 8 };
        let plain = picture(2, 2, PixelFormat::I420);
        let frame = plain.clone().with_bgr(std::sync::Arc::new(bgr));
        assert_eq!(frame_rgb(&frame).unwrap(), [3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10]);
        assert_ne!(frame_rgb(&plain), frame_rgb(&frame));
    }

    #[test]
    fn write_420_works_out_each_chroma_cell_once() {
        for (w, h, format) in [(33usize, 301usize, PixelFormat::I420), (33, 301, PixelFormat::Nv12), (6, 4, PixelFormat::I420)] {
            let calls = std::sync::atomic::AtomicUsize::new(0);
            let mut data = vec![0; format.size(w as u32, h as u32)];
            write_420(&mut data, w, h, format == PixelFormat::Nv12, |_, _| 0, |_, _| {
                calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                (1, 2)
            });
            assert_eq!(calls.into_inner(), w.div_ceil(2) * h.div_ceil(2), "{format:?}");
        }
    }

    #[test]
    fn a_step_gets_back_the_buffers_it_finished_with() {
        let mut held = Vec::new();
        let at = with_spares(&mut held, || {
            let buffer = spare_bytes(SPARE_ABOVE);
            let at = buffer.as_ptr();
            keep_bytes(buffer);
            let again = spare_bytes(SPARE_ABOVE / 2);
            assert_eq!(again.as_ptr(), at, "reused in the same frame");
            assert_eq!(again.len(), SPARE_ABOVE / 2);
            keep_bytes(again);
            at
        });
        assert_eq!(held.len(), 1);
        with_spares(&mut held, || {
            let buffer = spare_bytes(SPARE_ABOVE);
            assert_eq!(buffer.as_ptr(), at, "and in the next");
        });
        assert!(held.is_empty());
    }

    #[test]
    fn nothing_is_kept_outside_a_step_or_past_its_limit() {
        keep_bytes(vec![0; SPARE_ABOVE]);
        let mut held = Vec::new();
        with_spares(&mut held, || {});
        assert!(held.is_empty());

        with_spares(&mut held, || {
            (0..SPARE_BUFFERS + 3).for_each(|_| drop(spare_bytes(1)));
            keep_bytes(vec![0; SPARE_ABOVE - 1]);
            (0..SPARE_BUFFERS + 3).for_each(|_| keep_bytes(vec![0; SPARE_ABOVE]));
            keep_bytes(vec![0; SPARE_ABOVE * 2]);
        });
        assert_eq!(held.len(), SPARE_BUFFERS);
        assert_eq!(held.iter().filter(|b| b.capacity() >= SPARE_ABOVE * 2).count(), 1, "a bigger one replaces the smallest");
    }

    #[test]
    fn only_what_a_frame_asked_for_stays() {
        let mut held = vec![vec![0; SPARE_ABOVE * 2], vec![0; SPARE_ABOVE * 3]];
        with_spares(&mut held, || {
            let big = spare_bytes(SPARE_ABOVE * 3);
            (0..4).for_each(|_| keep_bytes(vec![0; SPARE_ABOVE]));
            keep_bytes(big);
        });
        assert_eq!(held.iter().map(Vec::capacity).collect::<Vec<_>>(), [SPARE_ABOVE * 3], "one asked for, so the biggest stays");
    }

    #[test]
    fn a_step_that_panics_lets_go_of_the_spares() {
        let mut held = Vec::new();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| with_spares(&mut held, || {
            keep_bytes(spare_bytes(SPARE_ABOVE));
            panic!("broken")
        })));
        assert_eq!(held.len(), 1);
        assert!(SPARES.with_borrow(Option::is_none), "the thread isn't lent anything any more");
    }

    // What cv2.resize(.., INTER_LINEAR) and INTER_AREA make of a 6 x 4 picture of random bytes
    const GREY_6X4: [u8; 24] = [139, 74, 229, 241, 169, 65, 6, 160, 149, 106, 38, 175, 188, 205, 175, 229, 98, 249, 10, 148, 95, 86, 147, 198];

    #[test]
    fn linear_matches_cv2_on_all_sizes() {
        assert_eq!(resize_linear_fixed(&GREY_6X4, 6, 4, 1, 4, 3), [110, 184, 201, 99, 118, 167, 143, 176, 69, 120, 117, 189]);
        assert_eq!(resize_linear_fixed(&GREY_6X4, 6, 4, 1, 9, 6), [139, 106, 100, 203, 235, 229, 181, 117, 65, 73, 95, 129, 177, 181, 162, 115, 112, 120, 36, 102, 165, 155, 140, 113, 61, 118, 187, 157, 178, 193, 175, 189, 188, 108, 162, 237, 99, 138, 170, 142, 146, 152, 128, 173, 224, 10, 79, 139, 104, 90, 96, 137, 172, 198]);
        assert_eq!(resize_linear_fixed(&GREY_6X4, 2, 12, 1, 3, 5), [202, 196, 191, 20, 87, 154, 113, 152, 190, 106, 176, 247, 111, 115, 119]);
    }

    #[test]
    fn half_size_linear_comes_out_as_the_area_average_as_in_cv2() {
        assert_eq!(resize_linear_fixed(&GREY_6X4, 6, 4, 1, 3, 2), [95, 181, 112, 138, 146, 173]);
        let noise: Vec<u8> = (0..64 * 64usize).map(|i| (i * i * 31 % 251) as u8).collect();
        assert_eq!(resize_linear_fixed(&noise, 64, 64, 1, 32, 32), resize_area(&noise, 64, 64, 1, 32, 32));
        assert_eq!(resize_linear_fixed(&GREY_6X4, 3, 8, 1, 3, 4)[..3], [190, 122, 147]);
    }

    #[test]
    fn area_matches_cv2_on_all_three_paths() {
        assert_eq!(resize_area(&GREY_6X4, 6, 4, 1, 2, 2), [126, 132, 137, 168]);
        assert_eq!(resize_area(&GREY_6X4, 6, 4, 1, 4, 3), [102, 171, 184, 107, 126, 169, 134, 164, 90, 131, 126, 185]);
        assert_eq!(resize_area(&GREY_6X4, 6, 4, 1, 8, 3), [106, 99, 133, 209, 207, 160, 121, 93, 97, 154, 175, 162, 168, 101, 116, 212, 55, 126, 146, 115, 122, 130, 160, 211]);
        assert_eq!(resize_area(&GREY_6X4, 2, 4, 3, 1, 2), [123, 110, 155, 128, 150, 179]);
    }

    #[test]
    fn three_channels_scale_like_each_channel_alone() {
        for (w2, h2) in [(4, 3), (9, 6), (3, 2), (2, 2)] {
            let (w, h) = (6, 4);
            let colour: Vec<u8> = GREY_6X4.iter().flat_map(|&v| [v, v.wrapping_mul(3), 255 - v]).collect();
            let one: Vec<Vec<u8>> = (0..3).map(|c| colour.iter().skip(c).step_by(3).copied().collect()).collect();
            let want: Vec<u8> = (0..w2 * h2).flat_map(|i| {
                let at = |c: usize| resize_linear_fixed(&one[c], w, h, 1, w2, h2)[i];
                [at(0), at(1), at(2)]
            }).collect();
            assert_eq!(resize_linear_fixed(&colour, w, h, 3, w2, h2), want, "{w2}x{h2}");
        }
    }

    #[test]
    fn a_tall_picture_scales_the_same_across_its_bands() {
        let (w, h, w2, h2) = (64usize, 300usize, 96usize, 601usize);
        let src: Vec<u8> = (0..w * h).map(|i| (i * i % 253) as u8).collect();
        let (xs, ys) = (linear_taps(w, w2, false), linear_taps(h, h2, true));
        let mut want = vec![0u8; w2 * h2];
        for (dy, &(y0, y1, p, q)) in ys.iter().enumerate() {
            for (dx, &(x0, x1, r, s)) in xs.iter().enumerate() {
                let across = |y: usize| (src[y * w + x0] as i32 * r + src[y * w + x1] as i32 * s) >> 4;
                want[dy * w2 + dx] = ((((across(y0) * p) >> 16) + ((across(y1) * q) >> 16) + 2) >> 2) as u8;
            }
        }
        assert_eq!(resize_linear_fixed(&src, w, h, 1, w2, h2), want);
    }

    #[test]
    fn a_box_is_filled_and_nothing_else_is_touched() {
        let want = resize_linear_fixed(&GREY_6X4, 6, 4, 1, 4, 3);
        let mut dst = vec![7u8; 8 * 5];
        resize_linear_into(&GREY_6X4, 6, 4, 1, &mut dst, 8, (2, 1, 4, 3));
        for y in 0..5 {
            for x in 0..8 {
                let inside = (2..6).contains(&x) && (1..4).contains(&y);
                assert_eq!(dst[y * 8 + x], if inside { want[(y - 1) * 4 + x - 2] } else { 7 }, "{x},{y}");
            }
        }
        // A box past the plane's edge loses what doesn't fit, whichever way it got its pixels
        for (w2, h2) in [(4, 3), (3, 2), (6, 4)] {
            let mut small = vec![7u8; 4 * 2];
            resize_linear_into(&GREY_6X4, 6, 4, 1, &mut small, 4, (2, 1, w2, h2));
            let want = resize_linear_fixed(&GREY_6X4, 6, 4, 1, w2, h2);
            assert_eq!(small[..6], [7; 6], "{w2}x{h2}");
            assert_eq!(small[6..], want[..2], "{w2}x{h2}");
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
