//! Pixel work the cameras need: fitting a frame to the size an app picked, and the formats each camera takes.

use telescope_core::frame::{Frame, PixelFormat};

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
    scale_plane(sy, fw, fh, oy, width as usize, (x as usize, y as usize, nw as usize, nh as usize));
    let (fcw, fch) = (fw.div_ceil(2), fh.div_ceil(2));
    let chroma = ((x / 2) as usize, (y / 2) as usize, nw.div_ceil(2) as usize, nh.div_ceil(2) as usize);
    scale_plane(su, fcw, fch, ou, cw, chroma);
    scale_plane(sv, fcw, fch, ov, cw, chroma);
    let mut fitted = Frame::new(width, height, PixelFormat::I420, out);
    fitted.number = frame.number;
    fitted.time_ns = frame.time_ns;
    fitted
}

/// Bilinear scaling of one plane into the (x, y, w, h) box of dst.
pub fn scale_plane(src: &[u8], sw: usize, sh: usize, dst: &mut [u8], dst_stride: usize, (x, y, w, h): (usize, usize, usize, usize)) {
    if w == 0 || h == 0 || sw == 0 || sh == 0 {
        return;
    }
    let dst_rows = dst.len() / dst_stride.max(1);
    for row in 0..h.min(dst_rows.saturating_sub(y)) {
        let fy = ((row as f32 + 0.5) * sh as f32 / h as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, wy) = (fy as usize, fy.fract());
        let y1 = (y0 + 1).min(sh - 1);
        for col in 0..w.min(dst_stride.saturating_sub(x)) {
            let fx = ((col as f32 + 0.5) * sw as f32 / w as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, wx) = (fx as usize, fx.fract());
            let x1 = (x0 + 1).min(sw - 1);
            let p = |yy: usize, xx: usize| src[yy * sw + xx] as f32;
            let top = p(y0, x0) * (1.0 - wx) + p(y0, x1) * wx;
            let bottom = p(y1, x0) * (1.0 - wx) + p(y1, x1) * wx;
            dst[(y + row) * dst_stride + x + col] = (top * (1.0 - wy) + bottom * wy + 0.5) as u8;
        }
    }
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

fn rgb_to_yuv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let y = 16.0 + 0.257 * r + 0.504 * g + 0.098 * b;
    let u = 128.0 - 0.148 * r - 0.291 * g + 0.439 * b;
    let v = 128.0 + 0.439 * r - 0.368 * g - 0.071 * b;
    (y, u, v)
}

/// An I420 frame as RGBA bytes, bottom row first (UnityCapture takes Unity's upside-down textures).
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

/// RGBA (or RGB with stride 3) pixels as an I420 frame; odd sizes round the chroma up. mirror flips left to right.
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
}
