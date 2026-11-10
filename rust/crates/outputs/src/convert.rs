//! Pixel work the cameras need: fitting a frame to the size an app picked, and the formats each camera takes.

use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::image::resize_linear_into;

/// frame scaled to fit width x height, keeping its shape, with black bars as needed (the same frame if it fits).
pub fn fit_frame(frame: &Frame, width: u32, height: u32) -> Frame {
    if (frame.width, frame.height) == (width, height) || frame.format != PixelFormat::I420 {
        return frame.clone();
    }
    let scale = (width as f64 / frame.width as f64).min(height as f64 / frame.height as f64);
    let (nw, nh) = ((frame.width as f64 * scale) as u32, (frame.height as f64 * scale) as u32);
    let (x, y) = ((width - nw.min(width)) / 2, (height - nh.min(height)) / 2);
    let mut out = vec![0u8; PixelFormat::I420.size(width, height)];
    let (sy, su, sv) = frame.i420_planes().unwrap();
    let (cw, ch) = (width.div_ceil(2) as usize, height.div_ceil(2) as usize);
    let (oy, rest) = out.split_at_mut((width * height) as usize);
    let (ou, ov) = rest.split_at_mut(cw * ch);
    ou.fill(128);
    ov.fill(128);
    let (fw, fh) = (frame.width as usize, frame.height as usize);
    resize_linear_into(sy, fw, fh, 1, oy, width as usize, (x as usize, y as usize, nw as usize, nh as usize));
    let (fcw, fch) = (fw.div_ceil(2), fh.div_ceil(2));
    let chroma = ((x / 2) as usize, (y / 2) as usize, nw.div_ceil(2) as usize, nh.div_ceil(2) as usize);
    resize_linear_into(su, fcw, fch, 1, ou, cw, chroma);
    resize_linear_into(sv, fcw, fch, 1, ov, cw, chroma);
    let mut fitted = Frame::new(width, height, PixelFormat::I420, out);
    fitted.number = frame.number;
    fitted.time_ns = frame.time_ns;
    fitted
}

/// Writes an I420 frame as NV12 into out (Y plane, then U and V interleaved), which must hold w * h * 3 / 2 bytes.
pub fn write_nv12(frame: &Frame, out: &mut [u8]) {
    let Some((y, u, v)) = frame.i420_planes() else { return };
    out[..y.len()].copy_from_slice(y);
    for (i, (a, b)) in u.iter().zip(v).enumerate() {
        out[y.len() + 2 * i] = *a;
        out[y.len() + 2 * i + 1] = *b;
    }
}

/// BT.601 limited range, as OpenCV's YUV I420 conversions use.
pub(crate) fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = (y as f32 - 16.0) * 1.164;
    let (d, e) = (u as f32 - 128.0, v as f32 - 128.0);
    let clamp = |x: f32| x.round().clamp(0.0, 255.0) as u8;
    [clamp(c + 1.596 * e), clamp(c - 0.392 * d - 0.813 * e), clamp(c + 2.017 * d)]
}

/// x.round().clamp(0.0, 255.0) as u8 for a value that can't be NaN, without the library call: the add and subtract of 2^23 rounds
/// to even, and a tie that went down is moved up to round away from zero.
#[inline(always)]
fn round_byte(x: f32) -> u8 {
    let x = x.clamp(0.0, 255.0);
    let t = (x + 8_388_608.0) - 8_388_608.0;
    (if x - t == 0.5 { t + 1.0 } else { t }) as u8
}

/// Every product the conversions below multiply a byte by, made with the same f32 operations as the formulas they stand for.
struct Terms {
    /// (y - 16) * 1.164
    luma: [f32; 256],
    /// 1.596 * (v - 128), 0.813 * (v - 128)
    v_red: [f32; 256],
    v_green: [f32; 256],
    /// 0.392 * (u - 128), 2.017 * (u - 128)
    u_green: [f32; 256],
    u_blue: [f32; 256],
    /// 0.257, 0.504 and 0.098 times a channel, for luma
    y_of: [[f32; 256]; 3],
    /// 0.148, 0.291 and 0.439 times a channel, for U
    u_of: [[f32; 256]; 3],
    /// 0.439, 0.368 and 0.071 times a channel, for V
    v_of: [[f32; 256]; 3],
}

fn terms() -> &'static Terms {
    static TERMS: std::sync::OnceLock<Terms> = std::sync::OnceLock::new();
    TERMS.get_or_init(|| {
        let by = |f: fn(f32) -> f32| std::array::from_fn(|i| f(i as f32));
        let weights = |a: f32, b: f32, c: f32| [by(|x| x).map(|x| x * a), by(|x| x).map(|x| x * b), by(|x| x).map(|x| x * c)];
        Terms {
            luma: by(|x| (x - 16.0) * 1.164),
            v_red: by(|x| 1.596 * (x - 128.0)),
            v_green: by(|x| 0.813 * (x - 128.0)),
            u_green: by(|x| 0.392 * (x - 128.0)),
            u_blue: by(|x| 2.017 * (x - 128.0)),
            y_of: weights(0.257, 0.504, 0.098),
            u_of: weights(0.148, 0.291, 0.439),
            v_of: weights(0.439, 0.368, 0.071),
        }
    })
}

/// The RGB yuv_to_rgb makes of a run of luma, each pair of pixels sharing its chroma sample; out is RGBA.
fn yuv_row_to_rgba(t: &Terms, y: &[u8], u: &[u8], v: &[u8], out: &mut [u8]) {
    for ((ys, os), (u, v)) in y.chunks(2).zip(out.chunks_mut(8)).zip(u.iter().zip(v)) {
        let (red, green_v, green_u, blue) = (t.v_red[*v as usize], t.v_green[*v as usize], t.u_green[*u as usize], t.u_blue[*u as usize]);
        for (y, o) in ys.iter().zip(os.chunks_exact_mut(4)) {
            let c = t.luma[*y as usize];
            o.copy_from_slice(&[round_byte(c + red), round_byte(c - green_u - green_v), round_byte(c + blue), 255]);
        }
    }
}

/// An I420 frame as RGBA bytes, bottom row first (UnityCapture takes Unity's upside-down textures).
pub fn i420_to_rgba_flipped(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    i420_to_rgba_flipped_into(frame, &mut out);
    out
}

/// i420_to_rgba_flipped into out, which keeps its allocation from one call to the next (black if the frame isn't I420).
pub fn i420_to_rgba_flipped_into(frame: &Frame, out: &mut Vec<u8>) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    out.clear();
    out.resize(w * h * 4, 0);
    let Some((py, pu, pv)) = frame.i420_planes() else { return };
    if w == 0 {
        return;
    }
    let (t, cw) = (terms(), w.div_ceil(2));
    telescope_core::image::over_rows(out, w * 4, |at, line| {
        let row = h - 1 - at;
        yuv_row_to_rgba(t, &py[row * w..][..w], &pu[(row / 2) * cw..][..cw], &pv[(row / 2) * cw..][..cw], line);
    });
}

/// An I420 frame as BGR bytes, top row first (an odd-sized v4l2 camera, which I420 can't be).
pub fn i420_to_bgr(frame: &Frame) -> Vec<u8> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut out = vec![0u8; w * h * 3];
    let Some((py, pu, pv)) = frame.i420_planes() else { return out };
    let cw = w.div_ceil(2);
    for row in 0..h {
        for col in 0..w {
            let ci = (row / 2) * cw + col / 2;
            let [r, g, b] = yuv_to_rgb(py[row * w + col], pu[ci], pv[ci]);
            out[(row * w + col) * 3..][..3].copy_from_slice(&[b, g, r]);
        }
    }
    out
}

/// RGBA (or RGB with stride 3) pixels as an I420 frame; odd sizes round the chroma up. mirror flips left to right.
pub fn rgb_to_i420(pixels: &[u8], width: u32, height: u32, channels: usize, mirror: bool) -> Frame {
    let mut out = Vec::new();
    rgb_to_i420_into(pixels, width, height, channels, mirror, &mut out);
    Frame::new(width, height, PixelFormat::I420, out)
}

/// The I420 planes of rgb_to_i420 in out, which keeps its allocation from one call to the next.
pub fn rgb_to_i420_into(pixels: &[u8], width: u32, height: u32, channels: usize, mirror: bool, out: &mut Vec<u8>) {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    out.clear();
    out.resize(w * h + 2 * cw * ch, 0);
    if w == 0 || h == 0 {
        return;
    }
    let (luma, chroma) = out.split_at_mut(w * h);
    let (us, vs) = chroma.split_at_mut(cw * ch);
    // Bands of row pairs, each with its own part of the three planes
    let per_band = if w * h < 512 * 1024 { ch } else { ch.div_ceil(8) };
    let mut bands: Vec<_> = luma.chunks_mut(2 * per_band * w).zip(us.chunks_mut(per_band * cw)).zip(vs.chunks_mut(per_band * cw)).map(|((l, u), v)| (l, u, v)).collect();
    telescope_core::workers::each_part(&mut bands, |band, (luma, us, vs)| {
        let first = band * per_band;
        rgb_band_to_i420(pixels, (w, h), channels, mirror, first, luma, us, vs);
    });
}

/// The row pairs of rgb_to_i420 from `first` on that fit in the planes' parts (each starting at that row pair).
#[allow(clippy::too_many_arguments)]
fn rgb_band_to_i420(pixels: &[u8], (w, h): (usize, usize), channels: usize, mirror: bool, first: usize, luma: &mut [u8], us: &mut [u8], vs: &mut [u8]) {
    let (t, cw) = (terms(), w.div_ceil(2));
    // Each pixel's U and V, and its luma written, from the pixel at output column x of row y (luma starting at row `first` * 2)
    let yuv = |x: usize, y: usize, luma: &mut [u8]| {
        let i = (y * w + if mirror { w - 1 - x } else { x }) * channels;
        let (r, g, b) = (pixels[i] as usize, pixels[i + 1] as usize, pixels[i + 2] as usize);
        luma[x] = round_byte(((16.0 + t.y_of[0][r]) + t.y_of[1][g]) + t.y_of[2][b]);
        let u = ((128.0 - t.u_of[0][r]) - t.u_of[1][g]) + t.u_of[2][b];
        let v = ((128.0 + t.v_of[0][r]) - t.v_of[1][g]) - t.v_of[2][b];
        (u, v)
    };
    for k in 0..us.len() / cw {
        let cy = first + k;
        let (top, bottom) = luma[2 * k * w..].split_at_mut(w);
        let bottom = if 2 * cy + 1 < h { Some(&mut bottom[..w]) } else { None };
        let (urow, vrow) = (&mut us[k * cw..][..cw], &mut vs[k * cw..][..cw]);
        match bottom {
            Some(bottom) => {
                for cx in 0..w / 2 {
                    let (x, y) = (2 * cx, 2 * cy);
                    let (u0, v0) = yuv(x, y, top);
                    let (u1, v1) = yuv(x + 1, y, top);
                    let (u2, v2) = yuv(x, y + 1, bottom);
                    let (u3, v3) = yuv(x + 1, y + 1, bottom);
                    urow[cx] = round_byte((((0.0 + u0) + u1) + u2 + u3) / 4.0);
                    vrow[cx] = round_byte((((0.0 + v0) + v1) + v2 + v3) / 4.0);
                }
                if w % 2 == 1 {
                    let (x, y) = (w - 1, 2 * cy);
                    let ((u0, v0), (u1, v1)) = (yuv(x, y, top), yuv(x, y + 1, bottom));
                    urow[cw - 1] = round_byte(((0.0 + u0) + u1) / 2.0);
                    vrow[cw - 1] = round_byte(((0.0 + v0) + v1) / 2.0);
                }
            }
            None => {
                for cx in 0..w / 2 {
                    let (u0, v0) = yuv(2 * cx, 2 * cy, top);
                    let (u1, v1) = yuv(2 * cx + 1, 2 * cy, top);
                    urow[cx] = round_byte(((0.0 + u0) + u1) / 2.0);
                    vrow[cx] = round_byte(((0.0 + v0) + v1) / 2.0);
                }
                if w % 2 == 1 {
                    let (u0, v0) = yuv(w - 1, 2 * cy, top);
                    urow[cw - 1] = round_byte((0.0 + u0) / 1.0);
                    vrow[cw - 1] = round_byte((0.0 + v0) / 1.0);
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn grey(w: u32, h: u32, y: u8) -> Frame {
    let mut data = vec![y; (w * h) as usize];
    data.extend(vec![128; 2 * (w.div_ceil(2) * h.div_ceil(2)) as usize]);
    Frame::new(w, h, PixelFormat::I420, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_that_fits_is_the_same_frame() {
        let f = grey(8, 4, 50);
        assert!(std::sync::Arc::ptr_eq(&fit_frame(&f, 8, 4).data, &f.data));
    }

    #[test]
    fn a_wider_frame_gets_bars_above_and_below() {
        let fitted = fit_frame(&grey(8, 2, 200), 8, 8);
        let (y, _, _) = fitted.i420_planes().unwrap();
        assert_eq!(y[0], 0);
        assert_eq!(y[4 * 8], 200);
    }

    #[test]
    fn frames_reach_an_i420_camera_converted() {
        let out = rgb_to_i420(&[255; 4 * 2 * 4], 4, 2, 4, false);
        assert_eq!(out.data.len(), 4 * 2 * 3 / 2);
        assert!(out.data[..8].iter().all(|y| *y == 235), "white's luma in the video range: {:?}", &out.data[..8]);
    }

    #[test]
    fn nv12_interleaves_u_and_v() {
        let mut f = grey(2, 2, 10);
        let d = f.data_mut();
        d[4] = 1;
        d[5] = 2;
        let mut out = vec![0; 6];
        write_nv12(&f, &mut out);
        assert_eq!(out, [10, 10, 10, 10, 1, 2]);
    }

    #[test]
    fn rgb_and_back_keeps_a_colour_close() {
        let px = [200u8, 40, 90, 255].repeat(4);
        let f = rgb_to_i420(&px, 2, 2, 4, false);
        let rgba = i420_to_rgba_flipped(&f);
        for (a, b) in rgba[..3].iter().zip([200u8, 40, 90]) {
            assert!((*a as i32 - b as i32).abs() <= 3, "{rgba:?}");
        }
    }

    #[test]
    fn mirror_flips_left_to_right() {
        let mut px = vec![0u8; 2 * 3];
        px[3..6].copy_from_slice(&[255, 255, 255]);
        let f = rgb_to_i420(&px, 2, 1, 3, true);
        let (y, _, _) = f.i420_planes().unwrap();
        assert!(y[0] > y[1]);
    }

    /// The conversions as they were before the table versions, to hold the new ones to.
    mod reference {
        use super::*;

        fn rgb_to_yuv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
            (16.0 + 0.257 * r + 0.504 * g + 0.098 * b, 128.0 - 0.148 * r - 0.291 * g + 0.439 * b, 128.0 + 0.439 * r - 0.368 * g - 0.071 * b)
        }

        pub fn i420_to_rgba_flipped(frame: &Frame) -> Vec<u8> {
            let (w, h) = (frame.width as usize, frame.height as usize);
            let mut out = vec![0u8; w * h * 4];
            let Some((py, pu, pv)) = frame.i420_planes() else { return out };
            let cw = w.div_ceil(2);
            for row in 0..h {
                let dst = &mut out[(h - 1 - row) * w * 4..][..w * 4];
                for col in 0..w {
                    let ci = (row / 2) * cw + col / 2;
                    let [r, g, b] = yuv_to_rgb(py[row * w + col], pu[ci], pv[ci]);
                    dst[col * 4..col * 4 + 4].copy_from_slice(&[r, g, b, 255]);
                }
            }
            out
        }

        pub fn rgb_to_i420(pixels: &[u8], width: u32, height: u32, channels: usize, mirror: bool) -> Frame {
            let (w, h) = (width as usize, height as usize);
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let mut out = vec![0u8; w * h + 2 * cw * ch];
            let px = |x: usize, y: usize| {
                let x = if mirror { w - 1 - x } else { x };
                let i = (y * w + x) * channels;
                (pixels[i] as f32, pixels[i + 1] as f32, pixels[i + 2] as f32)
            };
            for y in 0..h {
                for x in 0..w {
                    let (r, g, b) = px(x, y);
                    out[y * w + x] = rgb_to_yuv(r, g, b).0.round().clamp(0.0, 255.0) as u8;
                }
            }
            let (us, vs) = out[w * h..].split_at_mut(cw * ch);
            for cy in 0..ch {
                for cx in 0..cw {
                    let (mut su, mut sv, mut n) = (0.0, 0.0, 0.0);
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (x, y) = (cx * 2 + dx, cy * 2 + dy);
                        if x < w && y < h {
                            let (r, g, b) = px(x, y);
                            let (_, u, v) = rgb_to_yuv(r, g, b);
                            su += u;
                            sv += v;
                            n += 1.0;
                        }
                    }
                    us[cy * cw + cx] = (su / n).round().clamp(0.0, 255.0) as u8;
                    vs[cy * cw + cx] = (sv / n).round().clamp(0.0, 255.0) as u8;
                }
            }
            Frame::new(width, height, PixelFormat::I420, out)
        }
    }

    /// A deterministic spread of bytes
    fn noise(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
        (0..len).map(|_| { x ^= x << 13; x ^= x >> 17; x ^= x << 5; (x >> 11) as u8 }).collect()
    }

    fn noisy_i420(w: u32, h: u32, seed: u32) -> Frame {
        Frame::new(w, h, PixelFormat::I420, noise(PixelFormat::I420.size(w, h), seed))
    }

    #[test]
    fn the_rounding_matches_round_and_clamp_for_every_f32_near_the_range() {
        let check = |x: f32| assert_eq!(round_byte(x), x.round().clamp(0.0, 255.0) as u8, "{x}");
        // Every f32 from just under zero to just over 255 would be 1.4 billion; every tenth of a step in the ties' neighbourhoods and a stride over the rest
        let (lo, hi) = ((-1.0f32).to_bits(), 256.0f32.to_bits());
        for bits in (0..=hi).step_by(7) {
            check(f32::from_bits(bits));
        }
        for bits in (0x8000_0000u32..=lo).step_by(1 << 12) {
            check(f32::from_bits(bits));
        }
        for n in 0..=256 {
            let half = n as f32 + 0.5;
            for x in [half, f32::from_bits(half.to_bits() - 1), f32::from_bits(half.to_bits() + 1), n as f32, -(half)] {
                check(x);
            }
        }
    }

    #[test]
    fn every_yuv_value_converts_as_before() {
        let t = terms();
        for y in 0..=255u8 {
            for u in 0..=255u8 {
                for v in 0..=255u8 {
                    let c = t.luma[y as usize];
                    let got = [round_byte(c + t.v_red[v as usize]), round_byte(c - t.u_green[u as usize] - t.v_green[v as usize]), round_byte(c + t.u_blue[u as usize])];
                    assert_eq!(got, yuv_to_rgb(y, u, v), "y {y} u {u} v {v}");
                }
            }
        }
    }

    #[test]
    fn frames_convert_to_rgba_as_before_at_every_awkward_size() {
        for (w, h) in [(1, 1), (2, 2), (3, 3), (5, 2), (2, 7), (17, 9), (64, 36), (1921, 33), (300, 301)] {
            let f = noisy_i420(w, h, w * 31 + h);
            assert_eq!(i420_to_rgba_flipped(&f), reference::i420_to_rgba_flipped(&f), "{w}x{h}");
        }
        let mut reused = vec![9u8; 3];
        i420_to_rgba_flipped_into(&noisy_i420(6, 4, 1), &mut reused);
        let f = noisy_i420(10, 6, 2);
        i420_to_rgba_flipped_into(&f, &mut reused);
        assert_eq!(reused, reference::i420_to_rgba_flipped(&f));
    }

    #[test]
    fn rgb_converts_to_i420_as_before_at_every_awkward_size_and_stride() {
        for (w, h) in [(1, 1), (2, 2), (3, 3), (5, 2), (2, 7), (1, 6), (6, 1), (17, 9), (64, 36), (255, 129), (301, 300), (1201, 701), (1200, 700)] {
            for channels in [3, 4] {
                for mirror in [false, true] {
                    let px = noise(w * h * channels, (w * 7 + h) as u32);
                    let (a, b) = (rgb_to_i420(&px, w as u32, h as u32, channels, mirror), reference::rgb_to_i420(&px, w as u32, h as u32, channels, mirror));
                    assert_eq!(a.data, b.data, "{w}x{h} x{channels} mirror {mirror}");
                }
            }
        }
    }

    #[test]
    fn rgb_converts_to_i420_as_before_for_extreme_colours() {
        // Every corner of the colour cube and a grey ramp, which is where the sums sit nearest a rounding tie
        let mut px = Vec::new();
        for r in [0u8, 255] { for g in [0u8, 255] { for b in [0u8, 255] { px.extend([r, g, b, 255]); } } }
        for v in 0..=255u8 { px.extend([v, v, v, 255]); px.extend([v, 255 - v, v / 2, 255]); }
        let n = px.len() / 4;
        px.truncate(n * 4);
        let (w, h) = (n as u32, 1);
        assert_eq!(rgb_to_i420(&px, w, h, 4, false).data, reference::rgb_to_i420(&px, w, h, 4, false).data);
        let (w, h) = (n as u32 / 2, 2);
        assert_eq!(rgb_to_i420(&px[..(w * h * 4) as usize], w, h, 4, false).data, reference::rgb_to_i420(&px[..(w * h * 4) as usize], w, h, 4, false).data);
    }

    fn time(what: &str, runs: u32, mut f: impl FnMut()) -> f64 {
        f();
        let start = std::time::Instant::now();
        for _ in 0..runs {
            f();
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        eprintln!("{what}: {ms:.2} ms");
        ms
    }

    #[test]
    #[ignore = "timing: cargo test --release -p telescope-outputs convert_timing -- --ignored --nocapture"]
    fn convert_timing() {
        for (name, w, h) in [("1080p", 1920u32, 1080u32), ("4K", 3840, 2160)] {
            let i420 = noisy_i420(w, h, 5);
            let rgba = noise((w * h * 4) as usize, 6);
            time(&format!("{name} i420->rgba flipped, old"), 5, || { std::hint::black_box(reference::i420_to_rgba_flipped(&i420)); });
            let mut out = Vec::new();
            time(&format!("{name} i420->rgba flipped, new"), 20, || { i420_to_rgba_flipped_into(&i420, &mut out); std::hint::black_box(&out); });
            time(&format!("{name} rgba->i420, old"), 5, || { std::hint::black_box(reference::rgb_to_i420(&rgba, w, h, 4, false)); });
            for (to, tw, th) in [("1080p", 1920u32, 1080u32), ("720p", 1280, 720), ("4:3", 1600, 1200)] {
                if (tw, th) != (w, h) {
                    time(&format!("{name} fit to {to}"), 20, || { std::hint::black_box(fit_frame(&i420, tw, th)); });
                }
            }
            let mut out = Vec::new();
            time(&format!("{name} rgba->i420, new"), 20, || { rgb_to_i420_into(&rgba, w, h, 4, false, &mut out); std::hint::black_box(&out); });
        }
    }
}
